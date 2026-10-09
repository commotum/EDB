//! Mutable decoded-node retention; independent of cursor position and immutable values.
use crate::Digest;
use crate::collections::LruMap;
use crate::index::tree::TreeNode;
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub current_entries: usize,
    pub current_bytes: usize,
    pub peak_entries: usize,
    pub peak_bytes: usize,
    pub oversized_bypasses: u64,
}

#[derive(Debug)]
struct CachedTreeNode {
    node: Arc<TreeNode>,
    bytes: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct TreeNodeCache {
    pub(crate) max_entries: usize,
    pub(crate) max_bytes: usize,
    state: Arc<Mutex<TreeNodeCacheState>>,
}

#[derive(Debug, Default)]
struct TreeNodeCacheState {
    entries: LruMap<Digest, CachedTreeNode>,
    stats: CacheStats,
}

impl TreeNodeCache {
    pub(crate) fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            max_entries,
            max_bytes,
            state: Arc::new(Mutex::new(TreeNodeCacheState::default())),
        }
    }

    pub(crate) fn get(&self, hash: &Digest) -> Option<Arc<TreeNode>> {
        lock(&self.state).get(hash)
    }

    pub(crate) fn insert(&self, hash: Digest, node: Arc<TreeNode>, bytes: usize) {
        lock(&self.state).insert(hash, node, bytes, self.max_entries, self.max_bytes);
    }

    pub(crate) fn stats(&self) -> CacheStats {
        lock(&self.state).stats
    }
}

impl TreeNodeCacheState {
    fn get(&mut self, hash: &Digest) -> Option<Arc<TreeNode>> {
        let node = self.entries.get(hash).map(|entry| Arc::clone(&entry.node));
        if node.is_some() {
            self.stats.hits = self.stats.hits.saturating_add(1);
        } else {
            self.stats.misses = self.stats.misses.saturating_add(1);
        }
        node
    }

    fn insert(
        &mut self,
        hash: Digest,
        node: Arc<TreeNode>,
        bytes: usize,
        max_entries: usize,
        max_bytes: usize,
    ) {
        if max_entries == 0 || max_bytes == 0 || bytes > max_bytes {
            self.stats.oversized_bypasses = self.stats.oversized_bypasses.saturating_add(1);
            return;
        }
        if let Some(old) = self.entries.insert(hash, CachedTreeNode { node, bytes }) {
            self.stats.current_bytes = self.stats.current_bytes.saturating_sub(old.bytes);
        }
        self.stats.current_bytes = self.stats.current_bytes.saturating_add(bytes);
        while self.entries.len() > max_entries || self.stats.current_bytes > max_bytes {
            let Some((_, old)) = self.entries.pop_lru() else {
                break;
            };
            self.stats.current_bytes = self.stats.current_bytes.saturating_sub(old.bytes);
            self.stats.evictions = self.stats.evictions.saturating_add(1);
        }
        self.stats.current_entries = self.entries.len();
        self.stats.peak_entries = self.stats.peak_entries.max(self.entries.len());
        self.stats.peak_bytes = self.stats.peak_bytes.max(self.stats.current_bytes);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::tree::{LeafSegment, encode_tree_node};
    use crate::{Datom, IndexOrder, Value, t_to_tx};

    fn leaf(source: u64) -> (Digest, Arc<TreeNode>, usize) {
        let node = Arc::new(TreeNode::Leaf(LeafSegment {
            order: IndexOrder::Saevt,
            history: true,
            entities: vec![42],
            attributes: vec![1000],
            values: vec![Value::Long(7)],
            transactions: vec![t_to_tx(1).unwrap()],
            sources: vec![source],
            assertions: vec![true],
        }));
        let bytes = encode_tree_node(&node).unwrap();
        let weight = node.retained_bytes() as usize + bytes.len();
        (crate::sha256(&bytes), node, weight)
    }

    #[test]
    fn eviction_releases_only_cache_ownership_of_source_bearing_nodes() {
        let (first_hash, first, weight) = leaf(99);
        let (second_hash, second, _) = leaf(98);
        let (third_hash, third, _) = leaf(97);
        let cache = TreeNodeCache::new(2, weight * 3);
        cache.insert(first_hash, Arc::clone(&first), weight);
        cache.insert(second_hash, second, weight);
        let active = cache.get(&first_hash).unwrap();
        cache.insert(third_hash, third, weight);
        assert!(cache.get(&second_hash).is_none());
        assert_eq!(cache.stats().evictions, 1);

        // Replacement is also a recency event, independently of reads.
        let (_, second, _) = leaf(98);
        cache.insert(first_hash, Arc::clone(&first), weight);
        cache.insert(second_hash, second, weight);
        assert!(cache.get(&third_hash).is_none());
        let (_, third, _) = leaf(97);
        cache.insert(third_hash, third, weight);
        assert!(cache.get(&first_hash).is_none());
        assert_eq!(cache.stats().evictions, 3);
        let active_weak = Arc::downgrade(&active);
        drop(first);
        drop(cache);
        assert_eq!(Arc::strong_count(&active), 1);
        let TreeNode::Leaf(leaf) = active.as_ref() else {
            panic!("retained leaf");
        };
        assert_eq!(
            leaf.datom(0),
            Some(Datom {
                entity: 42,
                attribute: 1000,
                value: Value::Long(7),
                tx: t_to_tx(1).unwrap(),
                source: 99,
                added: true,
            })
        );
        drop(active);
        assert!(active_weak.upgrade().is_none());
    }

    #[test]
    fn byte_budget_and_bypass_policy_are_independent_of_entry_budget() {
        let (first_hash, first, weight) = leaf(99);
        let (second_hash, second, _) = leaf(98);
        let cache = TreeNodeCache::new(8, weight);
        cache.insert(first_hash, Arc::clone(&first), weight);
        assert_eq!(cache.stats().current_bytes, weight);
        cache.insert(second_hash, second, weight);
        assert!(cache.get(&first_hash).is_none());
        assert!(cache.get(&second_hash).is_some());
        assert_eq!(cache.stats().current_entries, 1);
        assert_eq!(cache.stats().current_bytes, weight);
        assert_eq!(cache.stats().peak_bytes, weight);
        assert_eq!(cache.stats().evictions, 1);

        for (entries, bytes) in [(0, weight), (8, 0), (8, weight - 1)] {
            let cache = TreeNodeCache::new(entries, bytes);
            cache.insert(first_hash, Arc::clone(&first), weight);
            assert!(cache.get(&first_hash).is_none());
            assert_eq!(cache.stats().current_entries, 0);
            assert_eq!(cache.stats().current_bytes, 0);
            assert_eq!(cache.stats().oversized_bypasses, 1);
            assert_eq!(cache.stats().evictions, 0);
        }
    }
}
