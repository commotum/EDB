//! Administrative inspection over one captured block publication. Explicit deep
//! inspection may replay history; ordinary operators and runtime reads do not.
use super::{GarbageInventory, IntegrityProblem, IntegrityReport, OperationalMetrics};
use crate::storage::descriptors::{IndexDescriptor, SnapshotMetadata};
use crate::storage::log::LogRoot;
use crate::storage::ownership::BlockCollector;
use crate::storage::root::DatabaseRoot;
use crate::storage::{BlockReadConfig, BlockReader, ObjectId, PgBlockStore};
use crate::{
    DatabaseCatalog, Datom, ErrorCategory, IndexOrder, MaintenanceControl,
    PostgresConnectionConfig, SemanticError, View,
};
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
        let selected_config = config.clone();
        let mut factory = || {
            Ok(Box::new(PgBlockStore::connect(&selected_config)?)
                as Box<dyn crate::storage::ObjectReader + Send>)
        };
        let verified = super::integrity::verify_publication(
            root_id,
            &[],
            &mut store,
            &mut factory,
            true,
            control,
        )?;
        metrics.reachable_objects = Some(verified.objects_read as u64);
        metrics.reachable_bytes = Some(verified.bytes_read);
        metrics.transaction_bytes = Some(verified.transaction_bytes);
        metrics.requests = Some(verified.requests);
        let oracle = verified.database;
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
            IndexOrder::Saevt,
            IndexOrder::Savet,
        ] {
            control.check()?;
            // Unrestricted AVET/SAVET reads omit projections whose shared
            // backfill is pending at this captured value. Compare the visible
            // projection, rather than treating incomplete maintenance as loss.
            let visible_projection = |source: &[Datom]| {
                let mut expected =
                    super::projection::derive_index_projection(&oracle, source, order)?;
                if matches!(order, IndexOrder::Avet | IndexOrder::Savet) {
                    expected.retain(|datom| value.physical_avet_ready(datom.attribute));
                }
                Ok::<_, SemanticError>(expected)
            };
            compare(&value.datoms(order)?, &visible_projection(&current)?)?;
            compare(
                &value.clone().history().datoms(order)?,
                &visible_projection(&history)?,
            )?;
        }
        metrics.current_datoms = Some(current.len() as u64);
        metrics.history_datoms = Some(history.len() as u64);
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
