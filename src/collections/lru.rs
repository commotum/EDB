//! Keyed, mutable least-recently-used ordering; callers own cache policy.
//!
//! The bundled `clojure.core.cache/LRUCache` composes value lookup with the
//! persistent `clojure.data.priority-map` to update an item's recency and remove
//! the least recent item. EDB's caches already mutate under their owners'
//! locks; database values retain immutable payloads, not old cache versions.
//! A hash index plus linked slots preserves keyed recency without copying trees,
//! scanning resident keys, accumulating stale queue entries, or advancing a clock.
//!
//! Lookup, touch and eviction have expected O(1) work; inserting a new key is
//! amortized expected O(1). Each live key appears in the index and its slot.
//! Eviction makes the slot reusable, so slot count follows peak simultaneous
//! occupancy. Buffer capacity is retained after eviction; this is not an RSS or
//! byte-limit claim. Capacity, weights, counters and synchronization belong to
//! callers. Values need not be cloneable; callers may retain shared `Arc`s.

use std::collections::HashMap;
use std::hash::Hash;

#[derive(Debug)]
struct Entry<K, V> {
    key: K,
    value: V,
    previous: Option<usize>,
    next: Option<usize>,
}

#[derive(Debug)]
pub(crate) struct LruMap<K, V> {
    positions: HashMap<K, usize>,
    slots: Vec<Option<Entry<K, V>>>,
    free: Vec<usize>,
    least_recent: Option<usize>,
    most_recent: Option<usize>,
}

impl<K, V> Default for LruMap<K, V> {
    fn default() -> Self {
        Self {
            positions: HashMap::new(),
            slots: Vec::new(),
            free: Vec::new(),
            least_recent: None,
            most_recent: None,
        }
    }
}

impl<K: Eq + Hash + Clone, V> LruMap<K, V> {
    pub(crate) fn len(&self) -> usize {
        self.positions.len()
    }

    /// A successful lookup moves the existing entry to most recent.
    pub(crate) fn get(&mut self, key: &K) -> Option<&V> {
        let slot = *self.positions.get(key)?;
        self.touch(slot);
        Some(&self.entry(slot).value)
    }

    /// Insert or replace one value, making its key most recent in either case.
    pub(crate) fn insert(&mut self, key: K, value: V) -> Option<V> {
        if let Some(&slot) = self.positions.get(&key) {
            let previous = std::mem::replace(&mut self.entry_mut(slot).value, value);
            self.touch(slot);
            return Some(previous);
        }
        let entry = Entry {
            key: key.clone(),
            value,
            previous: None,
            next: None,
        };
        let slot = if let Some(slot) = self.free.pop() {
            self.slots[slot] = Some(entry);
            slot
        } else {
            self.slots.push(Some(entry));
            self.slots.len() - 1
        };
        self.positions.insert(key, slot);
        self.link_most_recent(slot);
        None
    }

    /// Remove and return the least recent entry, preserving value ownership.
    pub(crate) fn pop_lru(&mut self) -> Option<(K, V)> {
        let slot = self.least_recent?;
        self.unlink(slot);
        let entry = self.slots[slot].take().expect("occupied LRU slot");
        let removed = self.positions.remove(&entry.key);
        debug_assert_eq!(removed, Some(slot));
        self.free.push(slot);
        Some((entry.key, entry.value))
    }

    fn entry(&self, slot: usize) -> &Entry<K, V> {
        self.slots[slot].as_ref().expect("occupied LRU slot")
    }

    fn entry_mut(&mut self, slot: usize) -> &mut Entry<K, V> {
        self.slots[slot].as_mut().expect("occupied LRU slot")
    }

    fn touch(&mut self, slot: usize) {
        if self.most_recent != Some(slot) {
            self.unlink(slot);
            self.link_most_recent(slot);
        }
    }

    fn unlink(&mut self, slot: usize) {
        let entry = self.entry(slot);
        let (previous, next) = (entry.previous, entry.next);
        match previous {
            Some(previous) => self.entry_mut(previous).next = next,
            None => self.least_recent = next,
        }
        match next {
            Some(next) => self.entry_mut(next).previous = previous,
            None => self.most_recent = previous,
        }
        let entry = self.entry_mut(slot);
        entry.previous = None;
        entry.next = None;
    }

    fn link_most_recent(&mut self, slot: usize) {
        let previous = self.most_recent;
        self.entry_mut(slot).previous = previous;
        match previous {
            Some(previous) => self.entry_mut(previous).next = Some(slot),
            None => self.least_recent = Some(slot),
        }
        self.most_recent = Some(slot);
    }
}
