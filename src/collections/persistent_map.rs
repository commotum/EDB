//! Small private path-copied AVL map for speculative value ownership.
//!
//! Updates and cursor stacks are bounded by balanced tree height, not by the
//! number of speculative ancestors. Keys/values should be cheap shared handles.
//!
//! Retains Datomic's persistent-collection principle: updates copy a search path,
//! not all entries. Unlike the insertion-only recent BTSet, speculative current
//! indexes also remove entries; a balanced ordered map supports both operations.
//! This is not a port of the bundled priority-map, whose second ordering serves
//! a different consumer.
use std::cmp::Ordering;
use std::sync::Arc;

type Link<K, V> = Option<Arc<Node<K, V>>>;

#[derive(Debug)]
struct Node<K, V> {
    key: K,
    value: V,
    left: Link<K, V>,
    right: Link<K, V>,
    height: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct MapWork {
    pub(crate) comparisons: u64,
    pub(crate) nodes_created: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct SharedMap<K, V> {
    root: Link<K, V>,
    len: usize,
    work: MapWork,
}

impl<K, V> Default for SharedMap<K, V> {
    fn default() -> Self {
        Self {
            root: None,
            len: 0,
            work: MapWork::default(),
        }
    }
}

fn height<K, V>(link: &Link<K, V>) -> u32 {
    link.as_ref().map_or(0, |node| node.height)
}

fn node<K, V>(
    key: K,
    value: V,
    left: Link<K, V>,
    right: Link<K, V>,
    work: &mut MapWork,
) -> Arc<Node<K, V>> {
    work.nodes_created += 1;
    Arc::new(Node {
        key,
        value,
        height: 1 + height(&left).max(height(&right)),
        left,
        right,
    })
}

fn balance<K: Clone, V: Clone>(
    key: K,
    value: V,
    left: Link<K, V>,
    right: Link<K, V>,
    work: &mut MapWork,
) -> Arc<Node<K, V>> {
    if height(&left) > height(&right) + 1 {
        let l = left.as_ref().unwrap();
        if height(&l.left) >= height(&l.right) {
            let r = node(key, value, l.right.clone(), right, work);
            node(
                l.key.clone(),
                l.value.clone(),
                l.left.clone(),
                Some(r),
                work,
            )
        } else {
            let pivot = l.right.as_ref().unwrap();
            let l = node(
                l.key.clone(),
                l.value.clone(),
                l.left.clone(),
                pivot.left.clone(),
                work,
            );
            let r = node(key, value, pivot.right.clone(), right, work);
            node(
                pivot.key.clone(),
                pivot.value.clone(),
                Some(l),
                Some(r),
                work,
            )
        }
    } else if height(&right) > height(&left) + 1 {
        let r = right.as_ref().unwrap();
        if height(&r.right) >= height(&r.left) {
            let l = node(key, value, left, r.left.clone(), work);
            node(
                r.key.clone(),
                r.value.clone(),
                Some(l),
                r.right.clone(),
                work,
            )
        } else {
            let pivot = r.left.as_ref().unwrap();
            let l = node(key, value, left, pivot.left.clone(), work);
            let r = node(
                r.key.clone(),
                r.value.clone(),
                pivot.right.clone(),
                r.right.clone(),
                work,
            );
            node(
                pivot.key.clone(),
                pivot.value.clone(),
                Some(l),
                Some(r),
                work,
            )
        }
    } else {
        node(key, value, left, right, work)
    }
}

impl<K: Ord + Clone, V: Clone> SharedMap<K, V> {
    pub(crate) fn get(&self, key: &K) -> Option<&V> {
        self.get_by(|candidate| candidate.cmp(key))
            .map(|(_, value)| value)
    }

    pub(crate) fn get_by(&self, compare: impl Fn(&K) -> Ordering) -> Option<(&K, &V)> {
        let mut link = self.root.as_deref();
        while let Some(node) = link {
            link = match compare(&node.key) {
                Ordering::Less => node.right.as_deref(),
                Ordering::Greater => node.left.as_deref(),
                Ordering::Equal => return Some((&node.key, &node.value)),
            };
        }
        None
    }

    pub(crate) fn insert(&mut self, key: K, value: V) {
        fn insert<K: Ord + Clone, V: Clone>(
            link: &Link<K, V>,
            key: K,
            value: V,
            work: &mut MapWork,
        ) -> (Arc<Node<K, V>>, bool) {
            let Some(old) = link else {
                return (node(key, value, None, None, work), true);
            };
            work.comparisons += 1;
            let (left, right, added) = match key.cmp(&old.key) {
                Ordering::Equal => {
                    return (
                        node(key, value, old.left.clone(), old.right.clone(), work),
                        false,
                    );
                }
                Ordering::Less => {
                    let (left, added) = insert(&old.left, key, value, work);
                    (Some(left), old.right.clone(), added)
                }
                Ordering::Greater => {
                    let (right, added) = insert(&old.right, key, value, work);
                    (old.left.clone(), Some(right), added)
                }
            };
            (
                balance(old.key.clone(), old.value.clone(), left, right, work),
                added,
            )
        }
        let (root, added) = insert(&self.root, key, value, &mut self.work);
        self.root = Some(root);
        self.len += usize::from(added);
    }

    pub(crate) fn remove(&mut self, key: &K) {
        fn remove<K: Ord + Clone, V: Clone>(
            link: &Link<K, V>,
            key: &K,
            work: &mut MapWork,
        ) -> (Link<K, V>, bool) {
            let Some(old) = link else {
                return (None, false);
            };
            work.comparisons += 1;
            let (left, right, removed) = match key.cmp(&old.key) {
                Ordering::Less => {
                    let (left, removed) = remove(&old.left, key, work);
                    (left, old.right.clone(), removed)
                }
                Ordering::Greater => {
                    let (right, removed) = remove(&old.right, key, work);
                    (old.left.clone(), right, removed)
                }
                Ordering::Equal => {
                    if old.left.is_none() {
                        return (old.right.clone(), true);
                    }
                    let Some(mut next) = old.right.as_deref() else {
                        return (old.left.clone(), true);
                    };
                    while let Some(left) = next.left.as_deref() {
                        next = left;
                    }
                    let (right, _) = remove(&old.right, &next.key, work);
                    return (
                        Some(balance(
                            next.key.clone(),
                            next.value.clone(),
                            old.left.clone(),
                            right,
                            work,
                        )),
                        true,
                    );
                }
            };
            if !removed {
                return (link.clone(), false);
            }
            (
                Some(balance(
                    old.key.clone(),
                    old.value.clone(),
                    left,
                    right,
                    work,
                )),
                true,
            )
        }
        let (root, removed) = remove(&self.root, key, &mut self.work);
        self.root = root;
        self.len -= usize::from(removed);
    }

    /// Inclusive lower bound (or inclusive upper bound when reversed).
    /// The comparison must be monotonic in this map's order.
    pub(crate) fn seek(&self, compare: impl Fn(&K) -> Ordering, reverse: bool) -> MapCursor<K, V> {
        let mut cursor = MapCursor {
            stack: Vec::new(),
            reverse,
            visited: 0,
        };
        let mut link = self.root.clone();
        while let Some(node) = link {
            cursor.visited += 1;
            let compared = compare(&node.key);
            if if reverse {
                !compared.is_gt()
            } else {
                !compared.is_lt()
            } {
                link = if reverse {
                    node.right.clone()
                } else {
                    node.left.clone()
                };
                cursor.stack.push(node);
            } else {
                link = if reverse {
                    node.left.clone()
                } else {
                    node.right.clone()
                };
            }
        }
        cursor
    }
}

pub(crate) struct MapCursor<K, V> {
    stack: Vec<Arc<Node<K, V>>>,
    reverse: bool,
    pub(crate) visited: u64,
}

impl<K: Clone, V: Clone> Iterator for MapCursor<K, V> {
    type Item = (K, V);
    fn next(&mut self) -> Option<Self::Item> {
        let node = self.stack.pop()?;
        let mut link = if self.reverse {
            node.left.clone()
        } else {
            node.right.clone()
        };
        while let Some(child) = link {
            self.visited += 1;
            link = if self.reverse {
                child.right.clone()
            } else {
                child.left.clone()
            };
            self.stack.push(child);
        }
        Some((node.key.clone(), node.value.clone()))
    }
}
