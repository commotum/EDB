//! Storage-neutral immutable tree traversal and recent-tier merging.
//! Sources authenticate/cache nodes; cursor positioning and replacement semantics
//! are shared by live block snapshots, existing peers, and offline backups.

use crate::index::NormalizedIndexBoundary;
use crate::index::recent::{RecentCursor, RecentCursorStats, RecentTier};
use crate::index::tree::navigation::{
    TreeBoundary, boundary_floor_child, boundary_start_child, floor_tree_child,
    leaf_boundary_lower_bound, leaf_lower_bound, leaf_prefix_lower_bound, leaf_reverse_upper_bound,
    prefix_start_child, validate_loaded_child_datom, validate_loaded_child_key,
};
use crate::index::tree::{ChildRef, DirectoryNode, LeafSegment, RootNode, TreeNode, TreeReadStats};
use crate::{Datom, Digest, ErrorCategory, IndexOrder, IndexPrefix, SemanticError};
use std::collections::BTreeSet;
use std::ops::Deref;
use std::sync::Arc;

/// Typed views keep the cache-owned `Arc<TreeNode>` alive without cloning a
/// directory's routing table or a leaf's value columns. Cache eviction only
/// drops the cache's reference; an active seek/cursor remains valid.
#[derive(Clone, Debug)]
pub(crate) struct LoadedDirectory(pub(crate) Arc<TreeNode>);

impl Deref for LoadedDirectory {
    type Target = DirectoryNode;

    fn deref(&self) -> &Self::Target {
        match self.0.as_ref() {
            TreeNode::Directory(directory) => directory,
            _ => unreachable!("LoadedDirectory is constructed only from a directory node"),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct LoadedLeaf(pub(crate) Arc<TreeNode>);

impl Deref for LoadedLeaf {
    type Target = LeafSegment;

    fn deref(&self) -> &Self::Target {
        match self.0.as_ref() {
            TreeNode::Leaf(leaf) => leaf,
            _ => unreachable!("LoadedLeaf is constructed only from a leaf node"),
        }
    }
}

/// The immutable tree traversal is independent of the location of its
/// authenticated nodes. Live peers and read-only backups use the same
/// positioning, child verification, and bounded leaf retention.
pub(crate) trait DurableTreeSource {
    fn load_node(
        &self,
        hash: Digest,
        stats: &mut TreeReadStats,
    ) -> Result<Arc<TreeNode>, SemanticError>;

    fn load_directory(
        &self,
        reference: &ChildRef,
        order: IndexOrder,
        history: bool,
        stats: &mut TreeReadStats,
    ) -> Result<LoadedDirectory, SemanticError> {
        let misses = stats.cache_misses;
        let node = self.load_node(reference.hash, stats);
        crate::io_diagnostics::record_index_node(order, stats.cache_misses != misses);
        let node = node?;
        let TreeNode::Directory(directory) = node.as_ref() else {
            return Err(fault(
                "peer/tree-child-kind",
                "root child is not a directory node",
            ));
        };
        validate_loaded_child_key(
            reference,
            directory.order,
            directory.history,
            directory.count,
            directory.leaves.first().map(|child| &child.key),
            order,
            history,
        )?;
        Ok(LoadedDirectory(node))
    }

    fn load_leaf(
        &self,
        reference: &ChildRef,
        order: IndexOrder,
        history: bool,
        stats: &mut TreeReadStats,
    ) -> Result<LoadedLeaf, SemanticError> {
        let misses = stats.cache_misses;
        let node = self.load_node(reference.hash, stats);
        crate::io_diagnostics::record_index_node(order, stats.cache_misses != misses);
        let node = node?;
        let TreeNode::Leaf(leaf) = node.as_ref() else {
            return Err(fault(
                "peer/tree-child-kind",
                "directory child is not a leaf node",
            ));
        };
        let first = leaf.datom(0);
        validate_loaded_child_datom(
            reference,
            leaf.order,
            leaf.history,
            leaf.len() as u64,
            first.as_ref(),
            order,
            history,
        )?;
        Ok(LoadedLeaf(node))
    }
}

pub(crate) struct DurableTreeCursor<S> {
    snapshot: S,
    root: Arc<RootNode>,
    history: bool,
    order: IndexOrder,
    start: Option<Datom>,
    end: Option<Datom>,
    prefix: Option<IndexPrefix>,
    forward_boundary: Option<TreeBoundary>,
    reverse_boundary: Option<TreeBoundary>,
    directory_index: usize,
    leaf_index: usize,
    directory: Option<LoadedDirectory>,
    leaf: Option<LoadedLeaf>,
    datom_index: usize,
    stats: TreeReadStats,
    exhausted: bool,
}

impl<S: DurableTreeSource> DurableTreeCursor<S> {
    pub(crate) fn stats(&self) -> TreeReadStats {
        self.stats
    }

    pub(crate) fn new(
        snapshot: S,
        root: Arc<RootNode>,
        history: bool,
        order: IndexOrder,
        start: Option<Datom>,
        end: Option<Datom>,
    ) -> Self {
        let exhausted = root.directories.is_empty();
        let directory_index = start
            .as_ref()
            .map_or(0, |key| floor_tree_child(&root.directories, key, order));
        Self {
            snapshot,
            root,
            history,
            order,
            start,
            end,
            prefix: None,
            forward_boundary: None,
            reverse_boundary: None,
            directory_index,
            leaf_index: 0,
            directory: None,
            leaf: None,
            datom_index: 0,
            stats: TreeReadStats::default(),
            exhausted,
        }
    }

    pub(crate) fn new_prefix(
        snapshot: S,
        root: Arc<RootNode>,
        history: bool,
        prefix: IndexPrefix,
    ) -> Self {
        let order = prefix.order();
        let exhausted = root.directories.is_empty();
        let directory_index = prefix_start_child(&root.directories, &prefix);
        Self {
            snapshot,
            root,
            history,
            order,
            start: None,
            end: None,
            prefix: Some(prefix),
            forward_boundary: None,
            reverse_boundary: None,
            directory_index,
            leaf_index: 0,
            directory: None,
            leaf: None,
            datom_index: 0,
            stats: TreeReadStats::default(),
            exhausted,
        }
    }

    pub(crate) fn new_forward_boundary(
        snapshot: S,
        root: Arc<RootNode>,
        history: bool,
        boundary: NormalizedIndexBoundary,
    ) -> Self {
        let order = boundary.order();
        let boundary = TreeBoundary::new(boundary);
        let exhausted = root.directories.is_empty();
        let directory_index = boundary_start_child(&root.directories, &boundary);
        Self {
            snapshot,
            root,
            history,
            order,
            start: None,
            end: None,
            prefix: None,
            forward_boundary: Some(boundary),
            reverse_boundary: None,
            directory_index,
            leaf_index: 0,
            directory: None,
            leaf: None,
            datom_index: 0,
            stats: TreeReadStats::default(),
            exhausted,
        }
    }

    pub(crate) fn new_reverse(
        snapshot: S,
        root: Arc<RootNode>,
        history: bool,
        boundary: NormalizedIndexBoundary,
    ) -> Self {
        let order = boundary.order();
        let boundary = TreeBoundary::new(boundary);
        let directory_index = boundary_floor_child(&root.directories, &boundary);
        Self {
            snapshot,
            root,
            history,
            order,
            start: None,
            end: None,
            prefix: None,
            forward_boundary: None,
            reverse_boundary: Some(boundary),
            directory_index: directory_index.unwrap_or(0),
            leaf_index: 0,
            directory: None,
            leaf: None,
            datom_index: 0,
            stats: TreeReadStats::default(),
            exhausted: directory_index.is_none(),
        }
    }

    pub(crate) fn next_datom(&mut self) -> Result<Option<Datom>, SemanticError> {
        if self.reverse_boundary.is_some() {
            return self.next_reverse_datom();
        }
        if self.exhausted {
            return Ok(None);
        }
        loop {
            if let Some(leaf) = &self.leaf {
                while let Some(datom) = leaf.datom(self.datom_index) {
                    self.datom_index += 1;
                    if let Some(prefix) = &self.prefix {
                        match crate::index::compare_prefix(&datom, prefix) {
                            std::cmp::Ordering::Less => continue,
                            std::cmp::Ordering::Equal => {}
                            std::cmp::Ordering::Greater => {
                                self.exhausted = true;
                                return Ok(None);
                            }
                        }
                    }
                    if self
                        .forward_boundary
                        .as_ref()
                        .is_some_and(|boundary| boundary.normalized.compare_datom(&datom).is_lt())
                    {
                        continue;
                    }
                    if self
                        .start
                        .as_ref()
                        .is_some_and(|start| datom.cmp_in(start, self.order).is_lt())
                    {
                        continue;
                    }
                    if self
                        .end
                        .as_ref()
                        .is_some_and(|end| !datom.cmp_in(end, self.order).is_lt())
                    {
                        self.exhausted = true;
                        return Ok(None);
                    }
                    return Ok(Some(datom));
                }
                self.leaf = None;
                self.leaf_index = self.leaf_index.saturating_add(1);
            }

            if let Some(directory) = &self.directory {
                if let Some(reference) = directory.leaves.get(self.leaf_index).cloned() {
                    if self
                        .prefix
                        .as_ref()
                        .is_some_and(|prefix| reference.key.cmp_prefix(prefix).is_gt())
                    {
                        self.exhausted = true;
                        return Ok(None);
                    }
                    if self
                        .end
                        .as_ref()
                        .is_some_and(|end| !reference.key.cmp_datom(end, self.order).is_lt())
                    {
                        self.exhausted = true;
                        return Ok(None);
                    }
                    let leaf = self.snapshot.load_leaf(
                        &reference,
                        self.order,
                        self.history,
                        &mut self.stats,
                    )?;
                    self.datom_index = if let Some(start) = &self.start {
                        leaf_lower_bound(&leaf, start, self.order)
                    } else if let Some(prefix) = &self.prefix {
                        leaf_prefix_lower_bound(&leaf, prefix)
                    } else if let Some(boundary) = &self.forward_boundary {
                        leaf_boundary_lower_bound(&leaf, boundary)
                    } else {
                        0
                    };
                    self.leaf = Some(leaf);
                    continue;
                }
                self.directory = None;
                self.directory_index = self.directory_index.saturating_add(1);
            }

            let Some(reference) = self.root.directories.get(self.directory_index).cloned() else {
                self.exhausted = true;
                return Ok(None);
            };
            if self
                .prefix
                .as_ref()
                .is_some_and(|prefix| reference.key.cmp_prefix(prefix).is_gt())
            {
                self.exhausted = true;
                return Ok(None);
            }
            if self
                .end
                .as_ref()
                .is_some_and(|end| !reference.key.cmp_datom(end, self.order).is_lt())
            {
                self.exhausted = true;
                return Ok(None);
            }
            let directory = self.snapshot.load_directory(
                &reference,
                self.order,
                self.history,
                &mut self.stats,
            )?;
            self.leaf_index = if let Some(key) = &self.start {
                floor_tree_child(&directory.leaves, key, self.order)
            } else if let Some(prefix) = &self.prefix {
                prefix_start_child(&directory.leaves, prefix)
            } else if let Some(boundary) = &self.forward_boundary {
                boundary_start_child(&directory.leaves, boundary)
            } else {
                0
            };
            self.directory = Some(directory);
        }
    }

    fn next_reverse_datom(&mut self) -> Result<Option<Datom>, SemanticError> {
        if self.exhausted {
            return Ok(None);
        }
        loop {
            if let Some(leaf) = &self.leaf {
                while self.datom_index > 0 {
                    self.datom_index -= 1;
                    let datom = leaf
                        .datom(self.datom_index)
                        .expect("validated parallel leaf columns");
                    if self
                        .reverse_boundary
                        .as_ref()
                        .expect("reverse cursor retains its boundary")
                        .normalized
                        .compare_datom(&datom)
                        .is_gt()
                    {
                        continue;
                    }
                    return Ok(Some(datom));
                }
                self.leaf = None;
                if self.leaf_index > 0 {
                    self.leaf_index -= 1;
                } else {
                    self.directory = None;
                    if self.directory_index > 0 {
                        self.directory_index -= 1;
                    } else {
                        self.exhausted = true;
                        return Ok(None);
                    }
                }
            }

            if let Some(directory) = &self.directory {
                let reference = directory
                    .leaves
                    .get(self.leaf_index)
                    .cloned()
                    .expect("reverse leaf index was selected from this directory");
                let leaf = self.snapshot.load_leaf(
                    &reference,
                    self.order,
                    self.history,
                    &mut self.stats,
                )?;
                self.datom_index = leaf_reverse_upper_bound(
                    &leaf,
                    self.reverse_boundary
                        .as_ref()
                        .expect("reverse cursor retains its boundary"),
                );
                self.leaf = Some(leaf);
                continue;
            }

            let reference = self
                .root
                .directories
                .get(self.directory_index)
                .cloned()
                .expect("reverse directory index was selected from the root");
            let directory = self.snapshot.load_directory(
                &reference,
                self.order,
                self.history,
                &mut self.stats,
            )?;
            let Some(leaf_index) = boundary_floor_child(
                &directory.leaves,
                self.reverse_boundary
                    .as_ref()
                    .expect("reverse cursor retains its boundary"),
            ) else {
                if self.directory_index > 0 {
                    self.directory_index -= 1;
                    continue;
                }
                self.exhausted = true;
                return Ok(None);
            };
            self.leaf_index = leaf_index;
            self.directory = Some(directory);
        }
    }
}

/// Snapshot-local classification and accounting for the shared ordered merge.
pub(crate) trait MergeSource: DurableTreeSource {
    fn recent(&self) -> &RecentTier;
    fn avet_unready(&self) -> &BTreeSet<u32>;
    fn record_cursor(&self, _stats: MergeCursorStats) {}
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MergeCursorStats {
    pub tree: TreeReadStats,
    pub recent: RecentCursorStats,
}

/// Lazy ordered merge of one immutable durable tree and its authenticated
/// recent tier. The cursor owns the snapshot and every currently visited
/// node, so connection advancement and cache eviction cannot change it.
pub(crate) struct MergeCursor<S: MergeSource> {
    operation: Option<crate::sql_io::OperationContext>,
    durable: DurableTreeCursor<S>,
    recent: RecentCursor,
    history: bool,
    order: IndexOrder,
    reverse: bool,
    durable_next: Option<Datom>,
    recent_next: Option<Datom>,
    failed: bool,
    work_recorded: bool,
}
impl<S: MergeSource> MergeCursor<S> {
    pub(crate) fn new(
        durable: DurableTreeCursor<S>,
        recent: RecentCursor,
        history: bool,
        order: IndexOrder,
        reverse: bool,
    ) -> Self {
        Self {
            operation: crate::sql_io::OperationContext::current(),
            durable,
            recent,
            history,
            order,
            reverse,
            durable_next: None,
            recent_next: None,
            failed: false,
            work_recorded: false,
        }
    }

    pub fn stats(&self) -> MergeCursorStats {
        MergeCursorStats {
            tree: self.durable.stats(),
            recent: self.recent.stats(),
        }
    }

    fn fill_durable(
        &mut self,
        poll: &mut dyn FnMut() -> Result<bool, SemanticError>,
    ) -> Result<bool, SemanticError> {
        while self.durable_next.is_none() {
            if !poll()? {
                return Ok(false);
            }
            let Some(datom) = self.durable.next_datom()? else {
                break;
            };
            let recent = self.durable.snapshot.recent();
            if !recent.includes(self.order, &datom)?
                || (self.order == IndexOrder::Avet
                    && self
                        .durable
                        .snapshot
                        .avet_unready()
                        .contains(&datom.attribute))
                || (!self.history && recent.touches_current(&datom))
            {
                continue;
            }
            self.durable_next = Some(datom);
        }
        Ok(true)
    }

    fn next_result(
        &mut self,
        poll: &mut dyn FnMut() -> Result<bool, SemanticError>,
    ) -> Result<Option<Datom>, SemanticError> {
        if !self.fill_durable(poll)? {
            self.failed = true;
            return Ok(None);
        }
        while self.recent_next.is_none() {
            if !poll()? {
                self.failed = true;
                return Ok(None);
            }
            let Some(datom) = self.recent.next_with_poll(poll)? else {
                break;
            };
            if self.order == IndexOrder::Avet
                && self
                    .durable
                    .snapshot
                    .avet_unready()
                    .contains(&datom.attribute)
            {
                continue;
            }
            self.recent_next = Some(datom);
        }
        let take_durable = match (&self.durable_next, &self.recent_next) {
            (Some(durable), Some(recent)) if self.reverse => {
                !durable.cmp_in(recent, self.order).is_lt()
            }
            (Some(durable), Some(recent)) => !recent.cmp_in(durable, self.order).is_lt(),
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => return Ok(None),
        };
        if take_durable {
            let datom = self.durable_next.take();
            if datom.as_ref().is_some_and(|durable| {
                self.recent_next
                    .as_ref()
                    .is_some_and(|recent| recent.cmp_in(durable, self.order).is_eq())
            }) {
                self.recent_next = None;
            }
            Ok(datom)
        } else {
            Ok(self.recent_next.take())
        }
    }

    /// Poll every merge-source advancement, including facts hidden by recent
    /// replacements or index readiness. Datom fences must stay above the
    /// ordered merge: either source may be looking beyond an earlier value
    /// buffered by the other source.
    pub(crate) fn next_with_poll(
        &mut self,
        poll: &mut dyn FnMut() -> Result<bool, SemanticError>,
    ) -> Option<Result<Datom, SemanticError>> {
        let _operation = self.operation.as_ref().map(|operation| operation.enter());
        if self.failed {
            return None;
        }
        let mut stopped = false;
        let result = self.next_result(&mut || {
            if stopped {
                return Ok(false);
            }
            let proceed = poll()?;
            stopped = !proceed;
            Ok(proceed)
        });
        if stopped {
            // A recent source may have stopped while a durable lookahead was
            // already buffered. Do not leak that lookahead as a final result.
            self.failed = true;
            return None;
        }
        match result {
            Ok(Some(datom)) => Some(Ok(datom)),
            Ok(None) => None,
            Err(error) => {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}

impl<S: MergeSource> Iterator for MergeCursor<S> {
    type Item = Result<Datom, SemanticError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_with_poll(&mut || Ok(true))
    }
}

impl<S: MergeSource> Drop for MergeCursor<S> {
    fn drop(&mut self) {
        if self.work_recorded {
            return;
        }
        self.work_recorded = true;
        let stats = self.stats();
        self.durable.snapshot.record_cursor(stats);
    }
}

fn fault(code: &'static str, message: &'static str) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, code, message)
}
