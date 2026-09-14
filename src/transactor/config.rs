//! Admission/resource policy, shared by direct activation and standby.
use crate::{PostgresConnectionConfig, ProgramLimits, SemanticError};
use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapacityLimits {
    pub max_transaction_ops: usize,
    pub max_transaction_bytes: usize,
    pub max_history_transactions: u64,
    pub max_transaction_read_datoms: u64,
    pub max_transaction_read_bytes: u64,
    pub writer_tree_cache_entries: usize,
    pub writer_tree_cache_bytes: usize,
    pub program: ProgramLimits,
}

impl Default for CapacityLimits {
    fn default() -> Self {
        Self {
            max_transaction_ops: 100_000,
            max_transaction_bytes: 64 * 1024 * 1024,
            max_history_transactions: i64::MAX as u64,
            max_transaction_read_datoms: 1_000_000,
            max_transaction_read_bytes: 64 * 1024 * 1024,
            writer_tree_cache_entries: 4_096,
            writer_tree_cache_bytes: 64 * 1024 * 1024,
            program: ProgramLimits::default(),
        }
    }
}

const DEFAULT_MEMORY_INDEX_THRESHOLD_BYTES: u64 = 32 * 1024 * 1024;
const DEFAULT_MEMORY_INDEX_MAX_BYTES: u64 = 512 * 1024 * 1024;

/// Bounded novelty settings for the transactor-owned background indexer.
///
/// The byte measure is a deterministic native approximation: the retained
/// Rust `Datom`, canonical value bytes, and up to five compact references for
/// the four raw recent BTSet indexes plus persistent chunk-log/tree overhead.
/// It deliberately does not pretend to be allocator RSS. The defaults retain
/// Datomic Pro's documented 32 MiB scheduling point and 512 MiB back-pressure
/// point while tests and small deployments can use explicit lower values
/// without changing `TransactionServiceConfig` literals.
/// At the maximum, the writer parks ordinary dequeue while continuing lease
/// renewal and prioritizing indexing. The bounded admission queue may then
/// return `Busy` when it is genuinely full; committed idempotency retries that
/// have already reached the worker remain reconcilable without extending the
/// log. This preserves Datomic's index-first hard-limit behavior without an
/// unbounded native submission queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BackgroundIndexingConfig {
    pub memory_index_threshold_bytes: u64,
    pub memory_index_max_bytes: u64,
}

impl Default for BackgroundIndexingConfig {
    fn default() -> Self {
        Self {
            memory_index_threshold_bytes: DEFAULT_MEMORY_INDEX_THRESHOLD_BYTES,
            memory_index_max_bytes: DEFAULT_MEMORY_INDEX_MAX_BYTES,
        }
    }
}

impl BackgroundIndexingConfig {
    pub(super) fn validate(self) -> Result<Self, SemanticError> {
        if self.memory_index_threshold_bytes == 0
            || self.memory_index_max_bytes < self.memory_index_threshold_bytes
        {
            return Err(SemanticError::incorrect(
                "service/invalid-indexing-config",
                "memory index threshold must be positive and no greater than its maximum",
            ));
        }
        Ok(self)
    }
}

#[derive(Clone, Debug)]
pub struct TransactionServiceConfig {
    /// One transport and I/O policy shared by every service-owned role.
    pub connection: PostgresConnectionConfig,
    /// Public catalog name at startup. A service resolves it once and retains
    /// the resulting immutable storage ID for its lease, reader and workers.
    pub database_id: String,
    pub holder_id: String,
    pub lease_duration: Duration,
    pub renew_interval: Duration,
    pub queue_capacity: usize,
    pub capacity_limits: CapacityLimits,
}

/// Deployment/resource settings shared by direct activation and standby
/// takeover. None of these settings alter the identity of an admitted request.
#[derive(Clone, Debug)]
pub struct ServiceOptions {
    /// CPU edit-preparation lanes for incremental indexing; SQL stays serial.
    pub index_preparation_parallelism: usize,
    /// Search build admission shared by background indexing and excision.
    pub fulltext_build_limits: crate::FulltextBuildLimits,
    /// Advisory prefix-read lanes, independent of transaction authority.
    pub hint_prefetch: crate::HintPrefetchConcurrency,
    pub indexing: BackgroundIndexingConfig,
    pub execution: crate::TransactionExecutionOptions,
    pub excision: crate::ExcisionConfig,
    /// Optional bounded nonblocking diagnostics publication. The emitter owns
    /// its sink worker; transaction execution never invokes sink callbacks.
    pub telemetry: Option<crate::TelemetryEmitter>,
}

impl Default for ServiceOptions {
    fn default() -> Self {
        Self {
            index_preparation_parallelism: 1,
            fulltext_build_limits: crate::FulltextBuildLimits::default(),
            hint_prefetch: crate::HintPrefetchConcurrency::default(),
            indexing: BackgroundIndexingConfig::default(),
            execution: crate::TransactionExecutionOptions::default(),
            excision: crate::ExcisionConfig::default(),
            telemetry: None,
        }
    }
}
