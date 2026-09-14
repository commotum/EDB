//! Shared admission and progress for the block-native excision worker.
//! The service and administrative driver use `storage::excision`; there is
//! no separate SQL generation-rewrite worker.
use crate::SemanticError;

/// Admission for rare, potentially whole-database maintenance. Each bounded
/// rewrite batch accounts 1 MiB plus 64 times its canonical source bytes.
/// This is a policy envelope, not measured allocator RSS. Predicate discovery
/// and each index preparation have their own bounded admission checks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExcisionConfig {
    pub enabled: bool,
    pub max_admitted_bytes: u64,
    pub log_batch_transactions: usize,
}
impl Default for ExcisionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_admitted_bytes: 512 * 1024 * 1024,
            log_batch_transactions: 256,
        }
    }
}
impl ExcisionConfig {
    pub(crate) fn validate(self) -> Result<Self, SemanticError> {
        if self.max_admitted_bytes == 0
            || self.log_batch_transactions == 0
            || self.log_batch_transactions > 4096
        {
            return Err(SemanticError::incorrect(
                "excision/invalid-config",
                "excision bytes must be positive and each log batch must contain 1..=4096 transactions",
            ));
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExcisionProgress {
    pub phase: &'static str,
    pub steps: u64,
    /// Loaded source transactions in completed rewrite checkpoints.
    pub source_transactions: u64,
    pub rewritten_transactions: u64,
    pub source_payload_bytes: u64,
    pub peak_admitted_bytes: u64,
    /// Source receipt entries advanced; not a count of SQL writes.
    pub rows_staged: u64,
    /// Canonical datoms removed from the rewritten log so far.
    pub removed_datoms: u64,
    pub complete: bool,
    /// Actual fresh-write pause of the latest automatic activation attempt.
    /// Excludes off-thread discovery/admission; not persisted across restarts.
    pub fresh_write_pause_nanos: u64,
}
