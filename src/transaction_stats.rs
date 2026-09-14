//! Opt-in semantic transaction work, independent of durability and SQL counts.

/// Native work actually attempted while assessing transaction data. Repeated
/// attempts and nested calls accumulate; these are not counts of unique user
/// forms or promises of equality between eager and indexed algorithms.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TransactionWorkStats {
    pub assessments: u64,
    /// Primitive operations entering assessment, after map/function expansion.
    pub input_operations: u64,
    /// Unique-identity assertions on temporary entities considered for upsert.
    pub identity_claims: u64,
    /// Exact unique-attribute lookups during transaction identity resolution.
    pub identity_lookups: u64,
    /// Existing-entity matches of identity assertions, including repeated claims.
    pub upsert_resolutions: u64,
    /// Values/groups checked for successor uniqueness, including schema changes.
    pub uniqueness_checks: u64,
    /// Final logical datoms compared with db-before for material change.
    pub redundancy_checks: u64,
    /// Logical assertions/retractions eliminated as redundant with db-before.
    pub redundant_datoms: u64,
    /// Identical logical datoms removed by transaction-local deduplication.
    pub duplicate_datoms: u64,
    /// Affected entity/composite pairs whose tuple value was computed.
    pub composite_candidates: u64,
    /// Composite assertions and retractions actually generated.
    pub composite_datoms: u64,
    /// Transaction-function expansion attempts, including nested or rejected
    /// calls; not attribute/entity predicates or durable dependency validation.
    pub function_calls: u64,
    /// Material datoms produced by assessment, including automatic metadata.
    pub produced_datoms: u64,
}

/// Ephemeral execution diagnostics attached to the receipt returned by this
/// process. They are not durable receipt content and cannot be reconstructed
/// by reopening a receipt later. No forms, values, tempids or request keys occur
/// here. Basis/replay status correlates the report with its committed outcome.
#[derive(Clone, Debug)]
pub struct TransactionDiagnostics {
    pub basis_t: u64,
    pub replayed: bool,
    pub report: crate::SqlIoReport,
}

#[inline]
pub(crate) fn count(field: fn(&mut TransactionWorkStats) -> &mut u64, amount: usize) {
    crate::sql_io::record_current_transaction_work(|work| {
        let counter = field(work);
        *counter = counter.saturating_add(amount as u64);
    });
}
