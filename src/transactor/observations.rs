//! Local counters and immutable snapshots of service state; never storage authority.
use crate::{ErrorCategory, SemanticError};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackgroundIndexingFailure {
    pub category: ErrorCategory,
    pub code: &'static str,
    pub message: String,
}

/// Observations of the coherent background index/search projection job.
/// The field name is retained, but failures can arise in canonical preparation,
/// search preparation, or guarded adoption; they are not search-only failures.
/// A successful job can find no fulltext attributes. These service observations
/// do not imply that an independent peer has caught up.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BackgroundFulltextStats {
    /// Source basis of the last successfully adopted coherent projection job.
    /// Prepared but unadopted candidates never advance this observation.
    pub checked_basis_t: Option<u64>,
    /// Frozen source basis of the last preparation attempt. A dispatch failure
    /// before preparation records the requested basis instead.
    pub attempted_basis_t: Option<u64>,
    pub attempts: u64,
    /// Failed whole-job attempts, including preparation and adoption failures.
    pub failures: u64,
    /// Until the next real whole-job retry; None also covers a running attempt.
    pub retry_in: Option<Duration>,
    /// The whole-job retry budget was exhausted or ownership was lost, so this
    /// service stopped accepting work. Startup creates a new observation set.
    pub retry_exhausted: bool,
    pub last_failure: Option<BackgroundIndexingFailure>,
}

#[derive(Debug, Default)]
pub(super) struct ProjectionObservations {
    pub(super) stats: BackgroundFulltextStats,
    pub(super) retry_at: Option<Instant>,
}

impl ProjectionObservations {
    pub(super) fn started(&mut self, basis_t: u64) {
        self.stats.attempts = self.stats.attempts.saturating_add(1);
        self.stats.attempted_basis_t = Some(basis_t);
        self.retry_at = None;
        self.stats.retry_exhausted = false;
    }

    pub(super) fn failed(
        &mut self,
        error: &SemanticError,
        retry_at: Option<Instant>,
        exhausted: bool,
    ) {
        self.stats.failures = self.stats.failures.saturating_add(1);
        self.stats.last_failure = Some(BackgroundIndexingFailure {
            category: error.category,
            code: error.code,
            message: error.message.clone(),
        });
        self.retry_at = retry_at;
        self.stats.retry_exhausted = exhausted;
    }

    pub(super) fn adopted(&mut self, basis_t: u64) {
        self.stats.checked_basis_t = Some(basis_t);
        self.stats.last_failure = None;
        self.stats.retry_exhausted = false;
        self.retry_at = None;
    }

    pub(super) fn snapshot_with_messages(
        &self,
        now: Instant,
        messages: bool,
    ) -> BackgroundFulltextStats {
        let mut stats =
            BackgroundFulltextStats {
                checked_basis_t: self.stats.checked_basis_t,
                attempted_basis_t: self.stats.attempted_basis_t,
                attempts: self.stats.attempts,
                failures: self.stats.failures,
                retry_in: self.stats.retry_in,
                retry_exhausted: self.stats.retry_exhausted,
                last_failure: self.stats.last_failure.as_ref().map(|error| {
                    BackgroundIndexingFailure {
                        category: error.category,
                        code: error.code,
                        message: if messages {
                            error.message.clone()
                        } else {
                            String::new()
                        },
                    }
                }),
            };
        stats.retry_in = self.retry_at.map(|at| at.saturating_duration_since(now));
        stats
    }
}

/// Direct observations from the one background-indexing worker.
///
/// `indexing_*` is the frozen prefix being merged and `memory_index_*` is
/// novelty committed after that job was requested. Their sum is the pressure
/// used by admission, mirroring recovered `datomic.indexer/IndexerImpl`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackgroundIndexingStats {
    /// Newest fully authenticated physical publication adopted by the
    /// service's backlog accounting.
    pub published_revision: u64,
    pub published_basis_t: u64,
    /// Authenticated broad AVET projection jobs still represented by the
    /// adopted manifest. This is physical cleanup/backfill observability, not
    /// Datomic's positive-add `t-needing-avet` / `sync-schema` coordinate:
    /// drops are included here too.
    pub pending_avet_projections: u64,
    /// Greatest append-only physical revision observed, usable or corrupt.
    pub newest_observed_revision: u64,
    pub target_basis_t: u64,
    pub memory_index_transactions: u64,
    pub memory_index_datoms: u64,
    pub memory_index_bytes: u64,
    pub indexing_transactions: u64,
    pub indexing_datoms: u64,
    pub indexing_bytes: u64,
    pub total_transactions: u64,
    pub total_datoms: u64,
    pub total_bytes: u64,
    pub jobs_started: u64,
    pub jobs_completed: u64,
    pub jobs_failed: u64,
    /// Times the worker deliberately stopped dequeuing ordinary work while
    /// the frozen/recent tiers were at the configured hard bound.
    pub backpressure_stalls: u64,
    /// Requests rejected only because the bounded admission queue filled
    /// while indexing pressure had the writer parked.
    pub backpressure_rejections: u64,
    pub job_in_flight: bool,
    pub last_failure: Option<BackgroundIndexingFailure>,
    pub fulltext: BackgroundFulltextStats,
    /// Accounted excision work is maintenance, not recent-tier residency.
    pub excision: crate::ExcisionProgress,
    pub excision_failure: Option<BackgroundIndexingFailure>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OperationalServiceStats {
    pub accepting: bool,
    pub queued: usize,
    pub max_queued: usize,
    pub processed: u64,
    pub rejected_full: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServiceStats {
    pub queued: usize,
    pub max_queued: usize,
    pub processed: u64,
    pub rejected_full: u64,
    pub subscribers: usize,
    /// Lossless reports currently retaining two immutable database values
    /// across all subscribers.
    pub queued_reports: usize,
    /// High-water mark for `queued_reports` during this service lifetime.
    pub max_queued_reports: usize,
    /// Stable owned report payload currently retained across subscriber
    /// queues. Shared immutable tree/cache memory is not multiplied here.
    pub queued_report_payload_bytes: u64,
    /// High-water mark for `queued_report_payload_bytes`.
    pub max_queued_report_payload_bytes: u64,
    /// Oldest committed basis still awaiting delivery to any subscriber.
    pub oldest_queued_report_basis_t: Option<u64>,
    /// Distinct durable roots referenced by queued db-before/db-after values,
    /// rather than the number of report clones. This does not register readers
    /// or control storage retention.
    pub distinct_report_roots: usize,
    /// Distinct authoritative log generations referenced by queued reports.
    pub distinct_report_generations: usize,
}

/// Representation-level state retained by one block writer.
///
/// Counts describe semantic objects, not allocator RSS. Eager-value counters
/// remain zero on the production path; the explicit pure oracle is separate.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WriterResidencyStats {
    pub eager_database_values: usize,
    pub eager_current_facts: usize,
    pub eager_history_datoms: usize,
    pub recent_datoms: u64,
    pub recent_accounted_bytes: u64,
    pub tree_cache_entries: usize,
    pub tree_cache_bytes: usize,
    /// Child references retained by the eight decoded native roots, outside
    /// the discardable node cache.
    pub resident_tree_root_children: usize,
    /// Estimated decoded root allocations, including child vectors and
    /// recursively owned routing-key data. Canonical payload bytes are not
    /// retained here and are not included in this figure.
    pub resident_tree_root_estimated_bytes: u64,
    /// Resident authenticated schema/ident projections. These are expected to
    /// scale with metadata cardinality, so their size is explicit rather than
    /// hidden inside the database-size-independent writer claim.
    pub resident_schema_attributes: usize,
    pub resident_schema_information_datoms: usize,
    pub resident_schema_estimated_bytes: u64,
    pub resident_ident_names: usize,
    pub resident_ident_entities: usize,
    pub resident_ident_estimated_bytes: u64,
    pub native_root_reads: u64,
    pub native_directory_reads: u64,
    pub native_leaf_reads: u64,
    pub publication_revision: u64,
    /// Logical datoms delivered across the complete last committed
    /// transaction, from persisted-function expansion through assessment
    /// and successor validation/predicates.
    /// This is not a physical PostgreSQL/tree-node I/O counter.
    pub last_transaction_read_datoms: u64,
    /// Deterministic retained width of those delivered logical datoms.
    pub last_transaction_read_bytes: u64,
    /// Actual descriptor/dependency visits in the last fresh transaction's
    /// assessment, including unchanged-schema validation and tuple helpers.
    /// These are process-local work counts, not persisted bytes or RSS.
    pub last_schema_transition_attributes: u64,
    pub last_schema_projection_attributes: u64,
    pub last_schema_validation_attributes: u64,
    pub last_schema_validation_predicates: u64,
    pub last_schema_validation_tuple_types: u64,
    pub last_schema_validation_tuple_constituents: u64,
    pub last_schema_validation_installations: u64,
    pub last_schema_reuses: u64,
    pub last_schema_dependency_lookups: u64,
    pub last_schema_dependency_edges: u64,
    pub last_schema_composite_candidates: u64,
    /// Successful datoms delivered by underlying exact-prefix cursors. Memo
    /// replays increase logical reads but leave this source count unchanged.
    pub last_transaction_source_read_datoms: u64,
    pub last_transaction_source_read_bytes: u64,
    pub last_transaction_prefix_memo_hits: u64,
    pub last_transaction_prefix_memo_misses: u64,
    pub last_transaction_prefix_memo_admissions: u64,
    pub last_transaction_prefix_memo_rejections: u64,
    pub last_transaction_prefix_memo_peak_entries: usize,
    pub last_transaction_prefix_memo_peak_bytes: u64,
    /// Exact native cursor source work during the last committed operation.
    /// SQL counts are successful immutable-node rows, and byte counts are
    /// canonical node payloads rather than PostgreSQL/wire overhead.
    pub last_native_cursor_ranges: u64,
    pub last_native_cache_hits: u64,
    pub last_native_cache_misses: u64,
    pub last_native_sql_root_reads: u64,
    pub last_native_sql_directory_reads: u64,
    pub last_native_sql_leaf_reads: u64,
    pub last_native_sql_reads: u64,
    pub last_native_sql_read_bytes: u64,
    pub last_native_recent_datoms_examined: u64,
    pub last_native_recent_datoms_yielded: u64,
}
