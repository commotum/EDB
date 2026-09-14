//! Opt-in, operation-local cache and index attribution. These are actual native
//! accesses, not estimates obtained by subtracting process-wide counters.
use crate::{IndexOrder, OperationContext};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CacheTier {
    DecodedNode,
    LocalDisk,
    PostgresBlock,
    Inflight,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CacheIoStats {
    pub accesses: u64,
    pub hits: u64,
    pub misses: u64,
    pub errors: u64,
    /// Returned payload bytes, not protocol traffic or filesystem block I/O.
    pub physical_bytes: u64,
    pub canonical_bytes: u64,
    /// Sum of wall durations at this tier, including waits. Tiers overlap.
    pub elapsed_nanos: u64,
    pub min_nanos: u64,
    pub max_nanos: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IndexIoStats {
    pub cursors: u64,
    pub node_accesses: u64,
    /// Decoded-cache misses; these can be satisfied by another loader or disk.
    /// They are not necessarily PostgreSQL reads.
    pub decoded_misses: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReadIoStats {
    pub cache: BTreeMap<CacheTier, CacheIoStats>,
    pub indexes: BTreeMap<IndexOrder, IndexIoStats>,
}

/// Captures the caller once so delayed completion is not attributed to a later
/// sibling operation. Disabled instrumentation takes no timestamp or stats lock.
pub(crate) struct CacheObservation {
    context: Option<OperationContext>,
    tier: CacheTier,
    started: Option<Instant>,
}
impl CacheObservation {
    pub fn start(tier: CacheTier) -> Self {
        let context = OperationContext::current().filter(|c| c.diagnostics_enabled());
        let started = context.as_ref().map(|_| Instant::now());
        Self {
            context,
            tier,
            started,
        }
    }
    pub fn finish(self, hit: bool, error: bool, physical_bytes: u64, canonical_bytes: u64) {
        if let (Some(context), Some(started)) = (self.context, self.started) {
            context.record_cache_read(
                self.tier,
                hit,
                error,
                physical_bytes,
                canonical_bytes,
                started.elapsed(),
            );
        }
    }
}

impl OperationContext {
    pub(crate) fn record_cache_read(
        &self,
        tier: CacheTier,
        hit: bool,
        error: bool,
        physical_bytes: u64,
        canonical_bytes: u64,
        elapsed: Duration,
    ) {
        if !self.diagnostics_enabled() {
            return;
        }
        let elapsed = elapsed.as_nanos().min(u64::MAX as u128) as u64;
        self.update(|stats| {
            let tier = stats.reads.cache.entry(tier).or_default();
            tier.min_nanos = if tier.accesses == 0 {
                elapsed
            } else {
                tier.min_nanos.min(elapsed)
            };
            tier.max_nanos = tier.max_nanos.max(elapsed);
            tier.accesses = tier.accesses.saturating_add(1);
            tier.hits = tier.hits.saturating_add(u64::from(hit));
            tier.misses = tier.misses.saturating_add(u64::from(!hit));
            tier.errors = tier.errors.saturating_add(u64::from(error));
            tier.physical_bytes = tier.physical_bytes.saturating_add(physical_bytes);
            tier.canonical_bytes = tier.canonical_bytes.saturating_add(canonical_bytes);
            tier.elapsed_nanos = tier.elapsed_nanos.saturating_add(elapsed);
        });
    }
}

pub(crate) fn record_index_cursor(order: IndexOrder) {
    if let Some(context) = OperationContext::current().filter(|c| c.diagnostics_enabled()) {
        context.update(|stats| {
            let index = stats.reads.indexes.entry(order).or_default();
            index.cursors = index.cursors.saturating_add(1);
        });
    }
}
pub(crate) fn record_index_node(order: IndexOrder, missed: bool) {
    if let Some(context) = OperationContext::current().filter(|c| c.diagnostics_enabled()) {
        context.update(|stats| {
            let index = stats.reads.indexes.entry(order).or_default();
            index.node_accesses = index.node_accesses.saturating_add(1);
            index.decoded_misses = index.decoded_misses.saturating_add(u64::from(missed));
        });
    }
}
