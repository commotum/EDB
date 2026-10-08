//! Explicit administrative repair of the selected current read projection.
//! Ordinary reads never call this eager canonical replay. Receipt and read
//! authorization roots are preserved: repairing the current index does not
//! claim that damaged historical retained indexes have also been repaired.
use super::index_publication::IndexCandidate;
use super::root::DatabaseRoot;
use super::snapshot::RootCapture;
use super::{
    BlockIndexStats, IndexDescriptor, ObjectId, ObjectReader, PgBlockStore, SnapshotMetadata,
    WriteProtection,
};
use crate::index::tree::TreeConfig;
use crate::{Database, DurableTransaction, ErrorCategory, IndexOrder, SemanticError, View};
use std::collections::BTreeMap;

pub(crate) struct RecoveredIndex {
    source_indexes: Option<ObjectId>,
    pub(crate) descriptor: IndexDescriptor,
    pub(crate) descriptor_id: ObjectId,
    endpoint_entry: Option<ObjectId>,
    protection: WriteProtection,
    pub(crate) stats: BlockIndexStats,
    pub(crate) fulltext_stats: Option<crate::FulltextBuildStats>,
}
impl RecoveredIndex {
    pub(crate) fn candidate(&self) -> IndexCandidate<'_> {
        IndexCandidate {
            source_indexes: self.source_indexes,
            source_index_basis: 0,
            descriptor: &self.descriptor,
            descriptor_id: self.descriptor_id,
            endpoint_entry: self.endpoint_entry,
            protection: &self.protection,
        }
    }
}

/// Rebuild from canonical log/metadata, never from the possibly missing index.
/// The caller keeps `capture` alive through guarded adoption. This is a broad,
/// controlled maintenance operation: one eager database plus one index's datom
/// vector are resident; completed immutable tree nodes are streamed to storage.
pub(crate) fn prepare_recovery(
    store: &mut PgBlockStore,
    capture: &RootCapture,
    config: &TreeConfig,
    fulltext_limits: &crate::FulltextBuildLimits,
    control: &mut dyn FnMut() -> Result<(), SemanticError>,
) -> Result<RecoveredIndex, SemanticError> {
    control()?;
    config.validate()?;
    fulltext_limits.validate()?;
    if store.write_protection().is_some() {
        return Err(SemanticError::incorrect(
            "storage/recovery-protection",
            "Index recovery requires an independent unconfigured store",
        ));
    }
    let root = DatabaseRoot::decode(&capture.root_id(), &store.read_object(capture.root_id())?)?;
    let metadata_id = root
        .metadata
        .ok_or_else(|| fault("Canonical publication has no metadata"))?;
    let metadata = SnapshotMetadata::decode(&metadata_id, &store.read_object(metadata_id)?)?;
    if metadata.identity != root.identity || metadata.basis != root.basis {
        return Err(fault("Canonical root and allocation metadata disagree"));
    }
    let mut reader = ControlledReader {
        store: &mut *store,
        control: &mut *control,
    };
    let log = match root.log {
        Some(id) => super::log::LogRoot::open(&mut reader, id)?,
        None => super::log::LogRoot::empty(),
    };
    if log.basis_t() != root.basis {
        return Err(fault(
            "Canonical log does not reach the selected publication",
        ));
    }
    log.validate_structure(&mut reader)?;
    let mut database = Database::bootstrap()?;
    let lineage = crate::storage::catalog::identity_string(root.identity);
    database.entity_origin = crate::entity_identity::DatabaseOrigin::durable(&lineage);
    let mut endpoint_entry = None;
    let mut stats = BlockIndexStats::default();
    // Recovery is ordered replay: retain page navigation instead of seeking
    // from the captured endpoint independently for every transaction.
    let mut records = log.range(&mut reader, 1, root.basis + 1)?;
    for basis in 1..=root.basis {
        let record = records
            .next_record()
            .ok_or_else(|| fault("Canonical log has a gap"))??;
        let before = database
            .reserved_allocation()
            .ok_or_else(|| fault("Canonical replay lost allocation proof"))?;
        let after = crate::reserved_allocation::ReservedAllocation::from_frontier(
            record.entry.reserved_frontier,
            record.entry.eidx_frontier,
        )?;
        stats.input_datoms = stats
            .input_datoms
            .saturating_add(record.entry.tx_data.len() as u64);
        database = database.apply_committed_with_allocation_frontiers(
            &DurableTransaction {
                database_id: lineage.clone(),
                basis_t: basis,
                previous_hash: endpoint_entry.unwrap_or([0; 32]),
                eidx_frontier: record.entry.eidx_frontier,
                tempids: BTreeMap::new(),
                tx_data: record.entry.tx_data,
            },
            before,
            after,
            metadata.generation != 0,
        )?;
        endpoint_entry = Some(record.id);
    }
    if database.eidx_frontier() != metadata.eidx_frontier
        || database.reserved_allocation().map(|a| a.frontier()) != Some(metadata.reserved_frontier)
        || database.last_tx_instant() != metadata.last_tx_instant
    {
        return Err(fault(
            "Canonical replay disagrees with the selected allocation/time checkpoint",
        ));
    }
    database.validate_invariants()?;
    let protection = crate::storage::protection::protection(store, &[])?;
    store.set_write_protection(Some(protection.clone()))?;
    let result = (|| {
        let mut trees = Vec::with_capacity(12);
        for history in [false, true] {
            for order in [
                IndexOrder::Eavt,
                IndexOrder::Aevt,
                IndexOrder::Avet,
                IndexOrder::Vaet,
                IndexOrder::Saevt,
                IndexOrder::Savet,
            ] {
                control()?;
                let datoms = database.datoms(
                    if history {
                        View::History
                    } else {
                        View::Current
                    },
                    order,
                );
                let built = crate::index::tree::build_tree_with_sink(
                    order,
                    history,
                    datoms.into_iter().map(Ok),
                    config,
                    &mut |id, bytes| {
                        control()?;
                        if store.put(bytes)? != id {
                            return Err(fault("Recovered tree identity changed during upload"));
                        }
                        stats.nodes_written = stats.nodes_written.saturating_add(1);
                        stats.bytes_written =
                            stats.bytes_written.saturating_add(bytes.len() as u64);
                        Ok(())
                    },
                )?;
                trees.push(built.descriptor);
            }
        }
        let mut descriptor = IndexDescriptor {
            identity: root.identity,
            basis: root.basis,
            generation: metadata.generation,
            trees,
            pending_avet: vec![],
            avet_work: vec![],
            fulltext: None,
        };
        let fulltext = super::fulltext::build_for_descriptor_with_control(
            store,
            &descriptor,
            database.schema(),
            None,
            fulltext_limits,
            control,
        )?;
        let fulltext_stats = fulltext.map(|(attachment, stats)| {
            descriptor.fulltext = Some(attachment);
            stats
        });
        control()?;
        let bytes = descriptor.encode()?;
        let descriptor_id = store.put(&bytes)?;
        stats.nodes_written = stats.nodes_written.saturating_add(1);
        stats.bytes_written = stats.bytes_written.saturating_add(bytes.len() as u64);
        Ok(RecoveredIndex {
            source_indexes: root.indexes,
            descriptor,
            descriptor_id,
            endpoint_entry,
            protection,
            stats,
            fulltext_stats,
        })
    })();
    let clear = store.set_write_protection(None);
    match (result, clear) {
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        (Ok(prepared), Ok(())) => Ok(prepared),
    }
}

struct ControlledReader<'a> {
    store: &'a mut PgBlockStore,
    control: &'a mut dyn FnMut() -> Result<(), SemanticError>,
}
impl ObjectReader for ControlledReader<'_> {
    fn read_object(&mut self, id: ObjectId) -> Result<Vec<u8>, SemanticError> {
        (self.control)()?;
        let bytes = self.store.read_object(id)?;
        Ok(bytes)
    }
}
fn fault(message: &str) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, "storage/index-recovery", message)
}
