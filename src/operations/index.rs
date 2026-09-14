//! Explicit derived-index maintenance using the ordinary preparation and
//! publication protocol. No writer lease is acquired or stolen by operators.
use crate::storage::index_publication::{IndexCandidate, publish_index_as_operator};
use crate::storage::index_recovery::{RecoveredIndex, prepare_recovery};
use crate::storage::root::DatabaseRoot;
use crate::storage::{
    BlockReadConfig, BlockReader, BlockSnapshot, IndexInput, ObjectId, PgBlockStore, PreparedIndex,
};
use crate::{
    DatabaseCatalog, ErrorCategory, FulltextBuildLimits, FulltextBuildStats, MaintenanceControl,
    PostgresConnectionConfig, SemanticError,
};

#[derive(Clone, Debug)]
pub struct IndexMaintenanceReceipt {
    pub basis_t: u64,
    pub generation: u64,
    pub publication_revision: u64,
    pub descriptor_id: ObjectId,
    pub input_datoms: u64,
    pub steps: u64,
    pub fulltext: Option<FulltextBuildStats>,
    /// Recovery replaces the current derived index, never old receipt or
    /// authorization roots. Those require separate deep integrity inspection.
    pub recovered: bool,
}

// One candidate lives on the operator stack; boxing solely for variant size adds an allocation.
#[allow(clippy::large_enum_variant)]
enum Candidate {
    Ordinary(PreparedIndex),
    Recovered(RecoveredIndex),
}
impl Candidate {
    fn publication(&self) -> IndexCandidate<'_> {
        match self {
            Self::Ordinary(p) => IndexCandidate {
                source_indexes: Some(p.source_indexes),
                source_index_basis: p.snapshot.indexed_basis_t(),
                descriptor: &p.descriptor,
                descriptor_id: p.descriptor_id,
                endpoint_entry: p.endpoint_entry,
                protection: &p.protection,
            },
            Self::Recovered(p) => p.candidate(),
        }
    }
    fn input_datoms(&self) -> u64 {
        match self {
            Self::Ordinary(p) => p.stats.input_datoms,
            Self::Recovered(p) => p.stats.input_datoms,
        }
    }
    fn fulltext(&self) -> Option<FulltextBuildStats> {
        match self {
            Self::Ordinary(p) => p.fulltext_stats,
            Self::Recovered(p) => p.fulltext_stats,
        }
    }
}

pub(super) fn maintain(
    config: &PostgresConnectionConfig,
    database_id: &str,
    rebuild_search: bool,
    expected_index: Option<ObjectId>,
    control: &MaintenanceControl,
    fulltext_limits: &FulltextBuildLimits,
    tree: &crate::index::tree::TreeConfig,
) -> Result<IndexMaintenanceReceipt, SemanticError> {
    fulltext_limits.validate()?;
    tree.validate()?;
    let entry = DatabaseCatalog::connect_configured(config)?.require_active_id(database_id)?;
    let key = format!("databases/{}", entry.database_id);
    let lease_key = format!("writers/{}", entry.database_id);
    let reader = BlockReader::connect(config, BlockReadConfig::default())?;
    let mut store = PgBlockStore::connect(config)?;
    let mut target = None;
    let mut input_datoms = 0;
    let mut steps = 0;
    let mut conflicts = 0;
    let mut recovered = false;
    loop {
        control.check()?;
        let capture = reader.capture_reference(&key)?;
        let root = load_root(&mut store, capture.root_id())?;
        if crate::storage::catalog::identity_string(root.identity) != entry.lineage_id {
            return Err(fault(
                "Catalog lineage differs from the captured publication",
            ));
        }
        if expected_index.is_some_and(|expected| root.indexes != Some(expected)) {
            return Err(target_changed());
        }
        let target = *target.get_or_insert(root.basis);
        let captured = reader.capture_root(&capture);
        let prepared = match captured {
            Ok(snapshot) if rebuild_search => Candidate::Ordinary(prepare_search(
                snapshot,
                &mut store,
                control,
                fulltext_limits,
            )?),
            Ok(snapshot) => Candidate::Ordinary(
                IndexInput::new(snapshot)?
                    .with_fulltext_build_limits(fulltext_limits.clone())?
                    .prepare_with_control(&mut store, tree, &mut || control.check())?,
            ),
            // This is an explicitly requested operator repair, never the
            // ordinary reader/startup path. Replay still authenticates the
            // canonical log and metadata and propagates unrecoverable damage.
            Err(error) if !rebuild_search && error.category == ErrorCategory::Fault => {
                recovered = true;
                Candidate::Recovered(prepare_recovery(
                    &mut store,
                    &capture,
                    tree,
                    fulltext_limits,
                    &mut || control.check(),
                )?)
            }
            Err(error) => return Err(error),
        };
        input_datoms += prepared.input_datoms();
        let candidate = prepared.publication();
        let basis_t = candidate.descriptor.basis;
        let generation = candidate.descriptor.generation;
        let descriptor_id = candidate.descriptor_id;
        let complete = rebuild_search
            || (basis_t >= target
                && candidate.descriptor.pending_avet.is_empty()
                && candidate.descriptor.avet_work.is_empty());
        control.check()?;
        // Catch up only the publication coordinate, not the prepared input.
        // Ordinary writes may have appended a newer tail during preparation.
        let latest = reader.capture_reference(&key)?;
        let latest_root = load_root(&mut store, latest.root_id())?;
        if expected_index.is_some_and(|expected| latest_root.indexes != Some(expected)) {
            return Err(target_changed());
        }
        let publication = publish_index_as_operator(
            &mut store,
            &reader,
            &latest,
            &lease_key,
            latest_root.writer_epoch,
            candidate,
        );
        match publication {
            Ok(publication) => {
                steps += 1;
                conflicts = 0;
                crate::observation::notices::publish(config, &entry.database_id);
                let receipt = IndexMaintenanceReceipt {
                    basis_t,
                    generation,
                    publication_revision: publication.revision,
                    descriptor_id,
                    input_datoms,
                    steps,
                    fulltext: prepared.fulltext(),
                    recovered,
                };
                drop((publication, prepared, latest, capture));
                control.after_batch()?;
                if complete {
                    return Ok(receipt);
                }
            }
            Err(error) if error.category == ErrorCategory::Conflict && conflicts < 3 => {
                conflicts += 1;
                drop((prepared, latest, capture));
                control.after_batch()?;
            }
            Err(error) => return Err(error),
        }
    }
}

fn prepare_search(
    snapshot: BlockSnapshot,
    store: &mut PgBlockStore,
    control: &MaintenanceControl,
    fulltext_limits: &FulltextBuildLimits,
) -> Result<PreparedIndex, SemanticError> {
    let protection = crate::storage::protection::protection(store, &[])?;
    store.set_write_protection(Some(protection.clone()))?;
    let result = (|| {
        let mut descriptor = snapshot.index_descriptor().clone();
        descriptor.fulltext = None;
        let fulltext = crate::storage::fulltext::build_for_descriptor_with_control(
            store,
            &descriptor,
            &snapshot.base_metadata().schema,
            None,
            fulltext_limits,
            &mut || control.check(),
        )?;
        let fulltext_stats = fulltext.map(|(attachment, stats)| {
            descriptor.fulltext = Some(attachment);
            stats
        });
        let descriptor_id = store.put(&descriptor.encode()?)?;
        let endpoint_entry = match snapshot.captured_log() {
            Some(log) => log.record_id(store, descriptor.basis)?,
            None => None,
        };
        Ok(PreparedIndex {
            source_indexes: snapshot.captured_root().indexes.unwrap(),
            through_basis: descriptor.basis,
            generation: descriptor.generation,
            endpoint_entry,
            descriptor,
            descriptor_id,
            protection,
            stats: Default::default(),
            fulltext_stats,
            snapshot,
        })
    })();
    let clear = store.set_write_protection(None);
    match (result, clear) {
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        (Ok(prepared), Ok(())) => Ok(prepared),
    }
}

fn load_root(store: &mut PgBlockStore, id: ObjectId) -> Result<DatabaseRoot, SemanticError> {
    DatabaseRoot::decode(
        &id,
        &store
            .get(id)?
            .ok_or_else(|| fault("Publication is missing"))?,
    )
}
fn fault(message: &'static str) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, "index/maintenance", message)
}
fn target_changed() -> SemanticError {
    SemanticError::conflict(
        "cli/repair-target-mismatch",
        "Selected digest is not this database's current index descriptor",
    )
}
