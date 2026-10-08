//! Canonical replay retains authenticated forward navigation between transactions.
use super::*;

pub(super) fn canonical_log<R: ObjectReader>(
    log: &LogRoot,
    reader: &mut R,
    lineage: &str,
    generation: u64,
    control: &MaintenanceControl,
    database: &mut Database,
    mut check_basis: impl FnMut(&mut R, &Database) -> Result<(), SemanticError>,
) -> Result<u64, SemanticError> {
    // EDB-NOTE: keep the reader-independent traversal while checking retained
    // values between transactions. Reseeking the tail for every basis discards
    // the log owner's authenticated forward anchors. Sparse receipt/prefix
    // authority checks remain point reads; this is not a whole-verifier bound.
    let mut traversal = log.traversal(1, log.basis_t() + 1)?;
    let mut previous = [0; 32];
    let mut transaction_bytes = 0u64;
    for basis in 1..=log.basis_t() {
        control.check()?;
        let record = traversal
            .next_record(reader)
            .transpose()?
            .ok_or_else(|| fault("backup/log-gap", "Canonical log has a missing transaction"))?;
        transaction_bytes = transaction_bytes.saturating_add(record.encoded_bytes);
        let after = crate::reserved_allocation::ReservedAllocation::from_frontier(
            record.entry.reserved_frontier,
            record.entry.eidx_frontier,
        )?;
        let prior = database.reserved_allocation().ok_or_else(|| {
            fault(
                "backup/allocation",
                "Replay lost its authenticated allocation state",
            )
        })?;
        // Caller tempid names cannot justify allocation gaps: authenticated
        // log frontiers, not captured receipt names, are the replay proof.
        let transaction = DurableTransaction {
            database_id: lineage.to_owned(),
            basis_t: basis,
            previous_hash: previous,
            eidx_frontier: record.entry.eidx_frontier,
            tempids: BTreeMap::new(),
            tx_data: record.entry.tx_data,
        };
        *database = database.apply_committed_with_allocation_frontiers(
            &transaction,
            prior,
            after,
            generation != 0,
        )?;
        previous = record.id;
        check_basis(reader, database)?;
    }
    Ok(transaction_bytes)
}
