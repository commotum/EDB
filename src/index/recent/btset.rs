//! Private persistent memory-index tree used by `recent`.
//!
//! The tree uses small
//! immutable leaves and branches, a fanout of sixteen, path-copy insertion,
//! and a lower-bound cursor.  It is specialized to recent datom references;
//! this is not a storage abstraction.

use crate::{Datom, DurableTransaction, IndexOrder};
use std::cmp::Ordering;
use std::sync::Arc;

const NODE_WIDTH: usize = 16;

#[derive(Clone, Debug)]
pub(crate) struct RecentDatomRef {
    transaction: Arc<DurableTransaction>,
    datom: u32,
}

impl RecentDatomRef {
    pub(crate) fn new(transaction: Arc<DurableTransaction>, datom: usize) -> Result<Self, ()> {
        Ok(Self {
            transaction,
            datom: u32::try_from(datom).map_err(|_| ())?,
        })
    }

    pub(crate) fn datom(&self) -> &Datom {
        &self.transaction.tx_data[self.datom as usize]
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct BtWork {
    pub(crate) comparisons: u64,
    pub(crate) node_visits: u64,
    pub(crate) nodes_copied: u64,
    pub(crate) node_splits: u64,
    /// Immediate-child aggregate reads while constructing copied branches.
    pub(crate) aggregate_child_reads: u64,
}

impl BtWork {
    fn compared(&mut self, ordering: Ordering) -> Ordering {
        self.comparisons = self.comparisons.saturating_add(1);
        ordering
    }
}

#[derive(Debug)]
enum Node {
    Leaf {
        items: Vec<RecentDatomRef>,
    },
    Branch {
        first: RecentDatomRef,
        separators: Vec<RecentDatomRef>,
        children: Vec<Arc<Node>>,
        node_count: u64,
        height: u32,
    },
}

impl Node {
    fn aggregate(&self) -> (u64, u32) {
        match self {
            Self::Leaf { .. } => (1, 1),
            Self::Branch {
                node_count, height, ..
            } => (*node_count, *height),
        }
    }

    fn first(&self) -> &RecentDatomRef {
        match self {
            Self::Leaf { items } => &items[0],
            Self::Branch { first, .. } => first,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RecentBtSet {
    order: IndexOrder,
    len: u64,
    root: Option<Arc<Node>>,
}

enum Inserted {
    Duplicate,
    One(Arc<Node>),
    Split(Arc<Node>, Arc<Node>),
}

impl RecentBtSet {
    pub(crate) fn empty(order: IndexOrder) -> Self {
        Self {
            order,
            len: 0,
            root: None,
        }
    }

    /// Read the immutable root summary, without traversing retained children.
    pub(crate) fn summary(&self) -> (u64, u64, u32) {
        let (nodes, height) = self.root.as_deref().map_or((0, 0), Node::aggregate);
        (self.len, nodes, height)
    }

    pub(crate) fn insert(&self, item: RecentDatomRef, work: &mut BtWork) -> (Self, bool) {
        let Some(root) = &self.root else {
            work.nodes_copied = work.nodes_copied.saturating_add(1);
            return (
                Self {
                    order: self.order,
                    len: 1,
                    root: Some(Arc::new(Node::Leaf { items: vec![item] })),
                },
                true,
            );
        };

        match insert_node(root, item, self.order, work) {
            Inserted::Duplicate => (self.clone(), false),
            Inserted::One(root) => (
                Self {
                    order: self.order,
                    len: self.len + 1,
                    root: Some(root),
                },
                true,
            ),
            Inserted::Split(left, right) => {
                work.nodes_copied = work.nodes_copied.saturating_add(1);
                let root = branch(vec![left, right], work);
                (
                    Self {
                        order: self.order,
                        len: self.len + 1,
                        root: Some(root),
                    },
                    true,
                )
            }
        }
    }

    pub(crate) fn cursor(&self) -> BtCursor {
        BtCursor::first(self.root.clone())
    }

    pub(crate) fn seek(&self, key: &Datom) -> BtCursor {
        BtCursor::seek(self.root.clone(), |candidate| {
            candidate.cmp_in(key, self.order)
        })
    }

    /// Lower-bound seek using a monotonic comparison against a virtual key.
    /// This supports primary-key and AEVT-attribute seeks without manufacturing
    /// sentinel `Value`s that are not part of the data model.
    pub(crate) fn seek_by(&self, compare: impl Fn(&Datom) -> Ordering) -> BtCursor {
        BtCursor::seek(self.root.clone(), compare)
    }

    /// Reverse cursor beginning at the greatest item in this immutable tree.
    pub(crate) fn reverse_cursor(&self) -> BtCursor {
        BtCursor::last(self.root.clone())
    }

    /// Reverse upper-bound seek against a virtual key.
    ///
    /// The first `prev` returns the greatest item whose comparison is less
    /// than or equal to the key. Projected raw-index boundaries can therefore
    /// position after their highest match without manufacturing a concrete
    /// sentinel datom.
    pub(crate) fn reverse_seek_by(&self, compare: impl Fn(&Datom) -> Ordering) -> BtCursor {
        BtCursor::seek_upper(self.root.clone(), compare)
    }
}

fn compare(
    left: &RecentDatomRef,
    right: &RecentDatomRef,
    order: IndexOrder,
    work: &mut BtWork,
) -> Ordering {
    work.compared(left.datom().cmp_in(right.datom(), order))
}

fn branch(children: Vec<Arc<Node>>, work: &mut BtWork) -> Arc<Node> {
    debug_assert!(children.len() >= 2 && children.len() <= NODE_WIDTH);
    let first = children[0].first().clone();
    let mut separators = Vec::with_capacity(children.len() - 1);
    let mut node_count = 1_u64;
    let mut height = 0_u32;
    // These children are already visited to build the copied branch. Read only
    // their cached summaries; untouched descendant subtrees remain shared.
    for (index, child) in children.iter().enumerate() {
        work.aggregate_child_reads = work.aggregate_child_reads.saturating_add(1);
        let (child_nodes, child_height) = child.aggregate();
        node_count = node_count.saturating_add(child_nodes);
        height = height.max(child_height);
        if index > 0 {
            separators.push(child.first().clone());
        }
    }
    Arc::new(Node::Branch {
        first,
        separators,
        children,
        node_count,
        height: height.saturating_add(1),
    })
}

fn insert_node(
    node: &Arc<Node>,
    item: RecentDatomRef,
    order: IndexOrder,
    work: &mut BtWork,
) -> Inserted {
    work.node_visits = work.node_visits.saturating_add(1);
    match node.as_ref() {
        Node::Leaf { items } => {
            let position =
                items.binary_search_by(|candidate| compare(candidate, &item, order, work));
            let Err(position) = position else {
                return Inserted::Duplicate;
            };
            let mut next = Vec::with_capacity(items.len() + 1);
            next.extend_from_slice(&items[..position]);
            next.push(item);
            next.extend_from_slice(&items[position..]);
            if next.len() <= NODE_WIDTH {
                work.nodes_copied = work.nodes_copied.saturating_add(1);
                Inserted::One(Arc::new(Node::Leaf { items: next }))
            } else {
                let right_items = next.split_off(next.len() / 2);
                work.nodes_copied = work.nodes_copied.saturating_add(2);
                work.node_splits = work.node_splits.saturating_add(1);
                Inserted::Split(
                    Arc::new(Node::Leaf { items: next }),
                    Arc::new(Node::Leaf { items: right_items }),
                )
            }
        }
        Node::Branch {
            separators,
            children,
            ..
        } => {
            let child_index = separators.partition_point(|separator| {
                compare(separator, &item, order, work) != Ordering::Greater
            });
            match insert_node(&children[child_index], item, order, work) {
                Inserted::Duplicate => Inserted::Duplicate,
                Inserted::One(child) => {
                    let mut next = children.clone();
                    next[child_index] = child;
                    work.nodes_copied = work.nodes_copied.saturating_add(1);
                    Inserted::One(branch(next, work))
                }
                Inserted::Split(left, right) => {
                    let mut next = Vec::with_capacity(children.len() + 1);
                    next.extend_from_slice(&children[..child_index]);
                    next.push(left);
                    next.push(right);
                    next.extend_from_slice(&children[child_index + 1..]);
                    if next.len() <= NODE_WIDTH {
                        work.nodes_copied = work.nodes_copied.saturating_add(1);
                        Inserted::One(branch(next, work))
                    } else {
                        let right_children = next.split_off(next.len() / 2);
                        work.nodes_copied = work.nodes_copied.saturating_add(2);
                        work.node_splits = work.node_splits.saturating_add(1);
                        Inserted::Split(branch(next, work), branch(right_children, work))
                    }
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
struct PathFrame {
    branch: Arc<Node>,
    child: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct BtCursor {
    path: Vec<PathFrame>,
    leaf: Option<Arc<Node>>,
    position: usize,
    work: BtWork,
}

impl BtCursor {
    fn first(root: Option<Arc<Node>>) -> Self {
        let mut cursor = Self {
            path: Vec::new(),
            leaf: None,
            position: 0,
            work: BtWork::default(),
        };
        if let Some(root) = root {
            cursor.descend_left(root);
        }
        cursor
    }

    fn seek(root: Option<Arc<Node>>, compare: impl Fn(&Datom) -> Ordering) -> Self {
        let mut cursor = Self {
            path: Vec::new(),
            leaf: None,
            position: 0,
            work: BtWork::default(),
        };
        let Some(mut node) = root else {
            return cursor;
        };
        loop {
            cursor.work.node_visits = cursor.work.node_visits.saturating_add(1);
            match node.as_ref() {
                Node::Leaf { items } => {
                    let position = items.partition_point(|item| {
                        cursor.work.comparisons = cursor.work.comparisons.saturating_add(1);
                        compare(item.datom()) == Ordering::Less
                    });
                    cursor.leaf = Some(Arc::clone(&node));
                    cursor.position = position;
                    if position == items.len() {
                        cursor.advance_leaf();
                    }
                    return cursor;
                }
                Node::Branch {
                    separators,
                    children,
                    ..
                } => {
                    let child = separators.partition_point(|separator| {
                        cursor.work.comparisons = cursor.work.comparisons.saturating_add(1);
                        compare(separator.datom()) == Ordering::Less
                    });
                    cursor.path.push(PathFrame {
                        branch: Arc::clone(&node),
                        child,
                    });
                    node = Arc::clone(&children[child]);
                }
            }
        }
    }

    fn last(root: Option<Arc<Node>>) -> Self {
        let mut cursor = Self {
            path: Vec::new(),
            leaf: None,
            position: 0,
            work: BtWork::default(),
        };
        if let Some(root) = root {
            cursor.descend_right(root);
        }
        cursor
    }

    /// Position immediately after the greatest item `<=` a virtual key.
    fn seek_upper(root: Option<Arc<Node>>, compare: impl Fn(&Datom) -> Ordering) -> Self {
        let mut cursor = Self {
            path: Vec::new(),
            leaf: None,
            position: 0,
            work: BtWork::default(),
        };
        let Some(mut node) = root else {
            return cursor;
        };
        loop {
            cursor.work.node_visits = cursor.work.node_visits.saturating_add(1);
            match node.as_ref() {
                Node::Leaf { items } => {
                    let position = items.partition_point(|item| {
                        cursor.work.comparisons = cursor.work.comparisons.saturating_add(1);
                        !compare(item.datom()).is_gt()
                    });
                    cursor.leaf = Some(Arc::clone(&node));
                    cursor.position = position;
                    if position == 0 {
                        cursor.retreat_leaf();
                    }
                    return cursor;
                }
                Node::Branch {
                    separators,
                    children,
                    ..
                } => {
                    // A separator is the first item of its right child. For
                    // an upper bound, equality belongs on the right so a
                    // projected prefix reaches its rightmost matching child.
                    let child = separators.partition_point(|separator| {
                        cursor.work.comparisons = cursor.work.comparisons.saturating_add(1);
                        !compare(separator.datom()).is_gt()
                    });
                    cursor.path.push(PathFrame {
                        branch: Arc::clone(&node),
                        child,
                    });
                    node = Arc::clone(&children[child]);
                }
            }
        }
    }

    fn descend_left(&mut self, mut node: Arc<Node>) {
        loop {
            self.work.node_visits = self.work.node_visits.saturating_add(1);
            match node.as_ref() {
                Node::Leaf { .. } => {
                    self.leaf = Some(node);
                    self.position = 0;
                    return;
                }
                Node::Branch { children, .. } => {
                    self.path.push(PathFrame {
                        branch: Arc::clone(&node),
                        child: 0,
                    });
                    node = Arc::clone(&children[0]);
                }
            }
        }
    }

    fn descend_right(&mut self, mut node: Arc<Node>) {
        loop {
            self.work.node_visits = self.work.node_visits.saturating_add(1);
            match node.as_ref() {
                Node::Leaf { items } => {
                    self.position = items.len();
                    self.leaf = Some(node);
                    return;
                }
                Node::Branch { children, .. } => {
                    let child = children.len() - 1;
                    self.path.push(PathFrame {
                        branch: Arc::clone(&node),
                        child,
                    });
                    node = Arc::clone(&children[child]);
                }
            }
        }
    }

    fn advance_leaf(&mut self) {
        self.leaf = None;
        while let Some(frame) = self.path.last_mut() {
            let Node::Branch { children, .. } = frame.branch.as_ref() else {
                unreachable!("cursor path contains only branches")
            };
            if frame.child + 1 < children.len() {
                frame.child += 1;
                let child = Arc::clone(&children[frame.child]);
                self.descend_left(child);
                return;
            }
            self.path.pop();
        }
    }

    fn retreat_leaf(&mut self) {
        self.leaf = None;
        while let Some(frame) = self.path.last_mut() {
            let Node::Branch { children, .. } = frame.branch.as_ref() else {
                unreachable!("cursor path contains only branches")
            };
            if frame.child > 0 {
                frame.child -= 1;
                let child = Arc::clone(&children[frame.child]);
                self.descend_right(child);
                return;
            }
            self.path.pop();
        }
    }

    pub(crate) fn next(&mut self) -> Option<RecentDatomRef> {
        loop {
            let leaf = self.leaf.as_ref()?;
            let Node::Leaf { items } = leaf.as_ref() else {
                unreachable!("cursor leaf points to a leaf")
            };
            if let Some(item) = items.get(self.position) {
                self.position += 1;
                return Some(item.clone());
            }
            self.advance_leaf();
        }
    }

    pub(crate) fn prev(&mut self) -> Option<RecentDatomRef> {
        loop {
            let leaf = self.leaf.as_ref()?;
            let Node::Leaf { items } = leaf.as_ref() else {
                unreachable!("cursor leaf points to a leaf")
            };
            if self.position > 0 {
                self.position -= 1;
                return Some(items[self.position].clone());
            }
            self.retreat_leaf();
        }
    }

    pub(crate) fn work(&self) -> BtWork {
        self.work
    }
}
