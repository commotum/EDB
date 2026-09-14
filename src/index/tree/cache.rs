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
