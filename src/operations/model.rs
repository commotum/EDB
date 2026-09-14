use crate::Digest;
use std::time::Duration;

/// Routine retention for retired immutable structures. Shorter ages are an
/// explicit operator choice and can disrupt long-running readers.
pub const RECOMMENDED_GARBAGE_COLLECTION_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// Maximum graph, metadata and object steps advanced by one operator call.
pub const MAX_COLLECTION_STEPS: usize = 4096;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IntegrityProblem {
    pub code: String,
    pub message: String,
}

/// Coordinates refer to one captured publication. Optional counts are only
/// populated by explicit deep inspection; None never means zero.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OperationalMetrics {
    pub basis_t: u64,
    pub index_basis_t: u64,
    pub index_lag: u64,
    pub generation: u64,
    pub publication_revision: u64,
    pub transactions: u64,
    pub indexed_current_datoms: u64,
    pub indexed_history_datoms: u64,
    pub pending_avet_projections: u64,
    pub transaction_bytes: Option<u64>,
    pub requests: Option<u64>,
    pub current_datoms: Option<u64>,
    pub history_datoms: Option<u64>,
    pub reachable_objects: Option<u64>,
    pub reachable_bytes: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IntegrityReport {
    pub database_id: String,
    pub metrics: OperationalMetrics,
    pub problems: Vec<IntegrityProblem>,
}

/// Read-only preview describes provider metadata and the durable collector
/// checkpoint, not a speculative deletion list. Applied results describe one
/// bounded advance; totals are None rather than rescanning the whole provider.
#[derive(Clone, Debug)]
pub struct GarbageInventory {
    pub phase: Option<crate::storage::ownership::CollectionPhase>,
    pub sealed_epoch: Option<u64>,
    pub minimum_age: Duration,
    pub stored_objects: Option<u64>,
    pub stored_bytes: Option<u64>,
    pub pending_events: Option<u64>,
    pub collection: crate::storage::ownership::CollectionStats,
    /// One bounded authorization-pruning page after a settled cycle. Its
    /// ownership deltas, if any, are folded by the next collection cycle.
    pub authorization: Option<crate::storage::ownership::AuthorizationMaintenance>,
    /// Report bridges retired after a settled cycle; their objects become
    /// eligible for age-respecting collection in a subsequent cycle.
    pub report_handoffs: Option<crate::storage::ownership::ReportHandoffMaintenance>,
    pub applied: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExcisionFault {
    None,
    AfterCapture,
    AfterCandidateStaged,
    AfterActivation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExcisionReceipt {
    pub database_id: String,
    pub source_generation: u64,
    pub generation: u64,
    pub basis_t: u64,
    pub request_count: u64,
    pub removed_datoms: u64,
    pub old_head_hash: Digest,
    pub new_head_hash: Digest,
    pub resumed: bool,
}

impl IntegrityReport {
    pub fn healthy(&self) -> bool {
        self.problems.is_empty()
    }
}
