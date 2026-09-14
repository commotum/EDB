//! Bounded, discardable canonical-program decoding cache. Scope is captured
//! database lineage plus excision generation; a cache entry never grants a
//! snapshot, writer lease, or authorization to open a retired value.
use crate::ProgramHash;
use crate::collections::LruMap;
use crate::program::ValidatedProgram;
use std::sync::{Arc, Mutex, MutexGuard};

/// Cumulative canonical-program decoding work and bounded accounted cache
/// footprint. Hits cross no new decode or validation boundary.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProgramCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub decodes: u64,
    pub validations: u64,
    pub evictions: u64,
    pub current_entries: usize,
    pub current_bytes: usize,
}

pub(crate) type ProgramCacheKey = ([u8; 16], u64, ProgramHash);
pub(crate) type SharedProgramCache = Arc<Mutex<ProgramCache>>;

struct CachedProgram {
    program: Arc<ValidatedProgram>,
    weight: usize,
}

pub(crate) struct ProgramCache {
    entries: LruMap<ProgramCacheKey, CachedProgram>,
    max_entries: usize,
    max_bytes: usize,
    stats: ProgramCacheStats,
}
impl Default for ProgramCache {
    fn default() -> Self {
        Self {
            entries: LruMap::default(),
            max_entries: 64,
            max_bytes: 64 * 1024 * 1024,
            stats: ProgramCacheStats::default(),
        }
    }
}
impl ProgramCache {
    pub(crate) fn get(&mut self, key: ProgramCacheKey) -> Option<Arc<ValidatedProgram>> {
        let value = self
            .entries
            .get(&key)
            .map(|entry| Arc::clone(&entry.program));
        if value.is_some() {
            self.stats.hits = self.stats.hits.saturating_add(1);
        } else {
            self.stats.misses = self.stats.misses.saturating_add(1);
        }
        value
    }
    pub(crate) fn insert_validated(
        &mut self,
        key: ProgramCacheKey,
        program: Arc<ValidatedProgram>,
        payload_bytes: usize,
    ) {
        self.stats.decodes = self.stats.decodes.saturating_add(1);
        self.stats.validations = self.stats.validations.saturating_add(1);
        // Canonical payload plus fixed function/key allowance is accounted
        // cache weight, not a claim about allocator RSS.
        let weight = payload_bytes.saturating_add(1024 + std::mem::size_of::<ProgramCacheKey>());
        if self.max_entries == 0 || weight > self.max_bytes {
            return;
        }
        if let Some(previous) = self.entries.insert(key, CachedProgram { program, weight }) {
            self.stats.current_bytes = self.stats.current_bytes.saturating_sub(previous.weight);
        }
        self.stats.current_bytes = self.stats.current_bytes.saturating_add(weight);
        self.enforce_limits();
    }
    pub(crate) fn set_limits(&mut self, entries: usize, bytes: usize) {
        self.max_entries = entries;
        self.max_bytes = bytes;
        self.enforce_limits();
    }
    fn enforce_limits(&mut self) {
        while self.entries.len() > self.max_entries || self.stats.current_bytes > self.max_bytes {
            let Some((_, entry)) = self.entries.pop_lru() else {
                break;
            };
            self.stats.current_bytes = self.stats.current_bytes.saturating_sub(entry.weight);
            self.stats.evictions = self.stats.evictions.saturating_add(1);
        }
        self.stats.current_entries = self.entries.len();
    }
    pub(crate) fn stats(&self) -> ProgramCacheStats {
        self.stats
    }
}

pub(crate) fn lock(cache: &SharedProgramCache) -> MutexGuard<'_, ProgramCache> {
    cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
pub(crate) fn stats(cache: &SharedProgramCache) -> ProgramCacheStats {
    lock(cache).stats()
}
