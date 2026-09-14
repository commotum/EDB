//! Administrative inspection over one captured block publication. Explicit deep
//! inspection may replay history; ordinary operators and runtime reads do not.
use super::{GarbageInventory, IntegrityProblem, IntegrityReport, OperationalMetrics};
use crate::storage::descriptors::{IndexDescriptor, SnapshotMetadata};
use crate::storage::log::LogRoot;
use crate::storage::ownership::{BlockCollector, object_children};
use crate::storage::receipts::{ExactReceipt, RequestIndex, basis_receipt_key};
use crate::storage::root::{Block, DatabaseRoot, DatabaseValueRoot};
use crate::storage::{BlockReadConfig, BlockReader, ObjectId, PgBlockStore};
use crate::{
    Database, DatabaseCatalog, Datom, DurableTransaction, ErrorCategory, IndexOrder,
    MaintenanceControl, PostgresConnectionConfig, Schema, SemanticError, View,
};
use std::collections::BTreeSet;
use std::time::Duration;

pub(super) fn inspect(
    config: &PostgresConnectionConfig,
    database_id: &str,
    deep: bool,
    control: &MaintenanceControl,
) -> Result<IntegrityReport, SemanticError> {
    control.check()?;
    let entry = DatabaseCatalog::connect_configured(config)?.require_active_id(database_id)?;
    let reader = BlockReader::connect(
        config,
        BlockReadConfig {
            cache_entries: 0,
            cache_bytes: 0,
            max_recent_transactions: usize::MAX,
            max_recent_datoms: usize::MAX,
            max_recent_bytes: usize::MAX,
        },
    )?;
    let capture = reader.capture_reference(&format!("databases/{}", entry.database_id))?;
    let mut store = PgBlockStore::connect(config)?;
    let root_id = capture.root_id();
    let root = DatabaseRoot::decode(&root_id, &required(&mut store, root_id)?)?;
    if crate::storage::catalog::identity_string(root.identity) != entry.lineage_id {
        return Err(fault("Inspection root has another identity"));
    }
    let mut metrics = OperationalMetrics {
        basis_t: root.basis,
        publication_revision: capture.source_revision(),
        transactions: root.basis,
        ..Default::default()
    };
    let mut problems = Vec::new();
    let checked = (|| {
        let metadata_id = root
            .metadata
            .ok_or_else(|| fault("Publication lacks metadata"))?;
        let metadata = SnapshotMetadata::decode(&metadata_id, &required(&mut store, metadata_id)?)?;
        let indexes_id = root
            .indexes
            .ok_or_else(|| fault("Publication lacks indexes"))?;
        let indexes = IndexDescriptor::decode(&indexes_id, &required(&mut store, indexes_id)?)?;
        let log = match root.log {
            Some(id) => LogRoot::open(&mut store, id)?,
            None => LogRoot::empty(),
        };
        if metadata.identity != root.identity
            || metadata.basis != root.basis
            || indexes.identity != root.identity
            || indexes.basis > root.basis
            || indexes.generation != metadata.generation
            || log.basis_t() != root.basis
            || (root.basis != 0
                && (log.eidx_frontier() != metadata.eidx_frontier
                    || log.reserved_frontier() != metadata.reserved_frontier))
        {
            return Err(fault("Publication/log/index/metadata coordinates disagree"));
        }
        metrics.generation = metadata.generation;
        metrics.index_basis_t = indexes.basis;
        metrics.index_lag = root.basis - indexes.basis;
        metrics.pending_avet_projections = indexes.pending_avet.len() as u64;
        metrics.indexed_current_datoms = indexes
            .trees
            .iter()
            .find(|t| !t.history && t.order == IndexOrder::Eavt)
            .unwrap()
            .count;
        metrics.indexed_history_datoms = indexes
            .trees
            .iter()
            .find(|t| t.history && t.order == IndexOrder::Eavt)
            .unwrap()
            .count;
        if !deep {
            return Ok(());
        }
        log.validate_structure(&mut store)?;

        // One graph walk validates all retained roots/receipts/programs,
        // while keeping only identities rather than every payload.
        let mut seen = BTreeSet::new();
        let mut pending = vec![root_id];
        let mut bytes = 0u64;
        while let Some(id) = pending.pop() {
            control.check()?;
            if !seen.insert(id) {
                continue;
            }
            let payload = required(&mut store, id)?;
            bytes = bytes.saturating_add(payload.len() as u64);
            pending.extend(object_children(&mut store, id, &payload)?);
        }
        metrics.reachable_objects = Some(seen.len() as u64);
        metrics.reachable_bytes = Some(bytes);
        for descriptor in &indexes.trees {
            crate::index::tree::validate_tree_streaming(descriptor, |id| {
                control.check()?;
                required(&mut store, *id)
            })?;
        }

        let mut oracle = Database::new(Schema::new())?;
        let mut transaction_bytes = 0u64;
        let mut range = log.range(&mut store, 1, root.basis.saturating_add(1))?;
        while let Some(record) = range.next_record() {
            let record = record?;
            control.check()?;
            transaction_bytes = transaction_bytes.saturating_add(record.encoded_bytes);
            let before = oracle
                .reserved_allocation()
                .ok_or_else(|| fault("Replay lacks allocation checkpoint"))?;
            let after = crate::reserved_allocation::ReservedAllocation::from_frontier(
                record.entry.reserved_frontier,
                record.entry.eidx_frontier,
            )?;
            oracle = oracle.apply_committed_with_allocation_frontiers(
                &DurableTransaction {
                    database_id: entry.database_id.clone(),
                    basis_t: record.entry.basis_t,
                    previous_hash: [0; 32],
                    eidx_frontier: record.entry.eidx_frontier,
                    tempids: Default::default(),
                    tx_data: record.entry.tx_data,
                },
                before,
                after,
                metadata.generation != 0,
            )?;
        }
        drop(range);
        oracle.validate_invariants()?;
        if oracle.basis_t() != root.basis
            || oracle.eidx_frontier() != metadata.eidx_frontier
            || oracle.last_tx_instant() != metadata.last_tx_instant
        {
            return Err(fault("Replayed log differs from captured metadata"));
        }
        metrics.transaction_bytes = Some(transaction_bytes);
        let snapshot = reader.capture_root(&capture)?;
        let value = snapshot.database_value();
        if value.schema() != oracle.schema() {
            return Err(fault("Indexed schema differs from the authoritative log"));
        }
        let current = oracle.datoms(View::Current, IndexOrder::Eavt);
        let history = value.clone().history().datoms(IndexOrder::Eavt)?;
        super::projection::validate_physical_history_projection(
            &oracle.datoms(View::History, IndexOrder::Eavt),
            &history,
            root.basis,
        )?;
        for order in [
            IndexOrder::Eavt,
            IndexOrder::Aevt,
            IndexOrder::Avet,
            IndexOrder::Vaet,
        ] {
            control.check()?;
            compare(
                &value.datoms(order)?,
                &super::projection::derive_index_projection(&oracle, &current, order)?,
            )?;
            compare(
                &value.clone().history().datoms(order)?,
                &super::projection::derive_index_projection(&oracle, &history, order)?,
            )?;
        }
        metrics.current_datoms = Some(current.len() as u64);
        metrics.history_datoms = Some(history.len() as u64);
        let index = RequestIndex::from_root(root.receipts);
        let mut after = None;
        let mut request_keys = 0u64;
        loop {
            let page = index.scan(&mut store, after, 128)?;
            if page.is_empty() {
                break;
            }
            for (key, id) in &page {
                control.check()?;
                let bytes = required(&mut store, *id)?;
                let envelope = Block::decode(id, &bytes)?;
                if envelope.kind == crate::storage::excision::TOMBSTONE_KIND {
                    if !envelope.links.is_empty()
                        || envelope.payload.len() != 32
                        || envelope.payload[..16] != root.identity
                    {
                        return Err(fault("Receipt tombstone is malformed or foreign"));
                    }
                    request_keys += 1;
                    continue;
                }
                let receipt = ExactReceipt::load_from_bytes(&mut store, *id, &bytes)?;
                if receipt.identity != root.identity || receipt.basis > root.basis {
                    return Err(fault("Receipt coordinate differs from its publication"));
                }
                let record = log
                    .read_record(&mut store, receipt.basis)?
                    .ok_or_else(|| fault("Receipt transaction is absent"))?;
                if record.id != receipt.transaction {
                    return Err(fault("Receipt transaction differs from captured log"));
                }
                let before = DatabaseValueRoot::decode(
                    &receipt.before,
                    &required(&mut store, receipt.before)?,
                )?;
                let after = DatabaseValueRoot::decode(
                    &receipt.after,
                    &required(&mut store, receipt.after)?,
                )?;
                if before.identity != root.identity
                    || after.identity != root.identity
                    || before.basis + 1 != receipt.basis
                    || after.basis != receipt.basis
                {
                    return Err(fault("Receipt before/after coordinates disagree"));
                }
                for entity in receipt.tempids.values() {
                    if crate::eid_to_part(*entity)? != crate::TX_PARTITION
                        && crate::eid_to_eidx(*entity)? >= record.entry.eidx_frontier
                    {
                        return Err(fault(
                            "Receipt tempid exceeds its authenticated allocation frontier",
                        ));
                    }
                }
                if *key != basis_receipt_key(&root.identity, receipt.basis) {
                    request_keys += 1;
                }
            }
            after = page.last().map(|(key, _)| *key);
        }
        metrics.requests = Some(request_keys);
        Ok::<(), SemanticError>(())
    })();
    if let Err(error) = checked {
        control.check()?;
        problems.push(IntegrityProblem {
            code: error.code.into(),
            message: error.message,
        });
    }
    Ok(IntegrityReport {
        database_id: entry.database_id,
        metrics,
        problems,
    })
}

pub(super) fn inventory(
    config: &PostgresConnectionConfig,
    age: Duration,
    control: &MaintenanceControl,
) -> Result<GarbageInventory, SemanticError> {
    let mut store = PgBlockStore::connect(config)?;
    let status = BlockCollector::connect(config)?.status()?;
    let mut after = None;
    let mut count = 0u64;
    let mut bytes = 0u64;
    loop {
        control.check()?;
        let page = store.list_object_info(after, 128)?;
        if page.is_empty() {
            break;
        }
        for object in &page {
            count += 1;
            bytes = bytes.saturating_add(object.stored_bytes);
        }
        after = page.last().map(|object| object.id);
    }
    let mut after = None;
    let mut events = 0u64;
    loop {
        control.check()?;
        let page = store.list_live_refs("ownership/events/", after.as_deref(), 128)?;
        if page.is_empty() {
            break;
        }
        events += page.len() as u64;
        after = page.last().map(|(key, _)| key.clone());
    }
    Ok(GarbageInventory {
        phase: status.as_ref().map(|s| s.phase),
        sealed_epoch: status.map(|s| s.sealed_epoch),
        minimum_age: age,
        stored_objects: Some(count),
        stored_bytes: Some(bytes),
        pending_events: Some(events),
        collection: Default::default(),
        authorization: None,
        report_handoffs: None,
        applied: false,
    })
}

pub(super) fn collect(
    config: &PostgresConnectionConfig,
    age: Duration,
    control: &MaintenanceControl,
) -> Result<GarbageInventory, SemanticError> {
    let mut collector = BlockCollector::connect(config)?;
    let progress = collector
        .advance_with_control(age, super::MAX_COLLECTION_STEPS, &mut || control.check())?;
    let (authorization, report_handoffs) =
        if progress.phase == crate::storage::ownership::CollectionPhase::Complete {
            control.check()?;
            let handoffs =
                collector.prune_report_handoffs_with_control(age, 32, &mut || control.check())?;
            (
                Some(collector.prune_read_authorizations(age, 32)?),
                Some(handoffs),
            )
        } else {
            (None, None)
        };
    control.after_batch()?;
    Ok(GarbageInventory {
        phase: Some(progress.phase),
        sealed_epoch: Some(progress.sealed_epoch),
        minimum_age: age,
        stored_objects: None,
        stored_bytes: None,
        pending_events: None,
        collection: progress.stats,
        authorization,
        report_handoffs,
        applied: true,
    })
}
fn compare(actual: &[Datom], expected: &[Datom]) -> Result<(), SemanticError> {
    if super::projection::same_stored_datoms(actual, expected) {
        Ok(())
    } else {
        Err(fault(
            "Index projection differs from authoritative information",
        ))
    }
}
fn required(store: &mut PgBlockStore, id: ObjectId) -> Result<Vec<u8>, SemanticError> {
    store
        .get(id)?
        .ok_or_else(|| fault("Referenced immutable object is absent"))
}
fn fault(message: &str) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, "integrity/block-content", message)
}
