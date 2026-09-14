//! Explicit repository integrity work. Ordinary repository opens do not call
//! this replay. Deep verification retains one eager canonical database and
//! small object/basis schedules, never one database clone per receipt prefix.
use super::{BackupVerification, ReadPoint, load_publication, read_object};
use crate::index::tree::{TreeDescriptor, TreeNode};
use crate::operations::projection::{
    derive_index_projection, same_stored_datoms, validate_physical_history_projection,
};
use crate::storage::descriptors::{INDEX_DESCRIPTOR_KIND, SNAPSHOT_METADATA_KIND};
use crate::storage::log::LogRoot;
use crate::storage::receipts::{ExactReceipt, RECEIPT_KIND, RequestIndex, basis_receipt_key};
use crate::storage::root::{Block, DATABASE_VALUE_ROOT_KIND, DatabaseRoot, DatabaseValueRoot};
use crate::storage::{IndexDescriptor, ObjectId, ObjectReader, SnapshotMetadata};
use crate::{
    Database, Datom, DurableTransaction, ErrorCategory, IndexOrder, MaintenanceControl,
    SemanticError, View,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

struct Reader<'a> {
    directory: &'a Path,
    control: &'a MaintenanceControl,
}
impl ObjectReader for Reader<'_> {
    fn read_object(&mut self, id: ObjectId) -> Result<Vec<u8>, SemanticError> {
        self.control.check()?;
        read_object(self.directory, id)
    }
}

mod authority;
mod inventory;
mod projection;
mod replay;
use authority::{metadata_for, validate_authorization, validate_receipts};
pub(crate) use inventory::walk_repository;
use inventory::{Inventory, inventory};
use projection::*;
pub(crate) fn verify_read_point(
    directory: &Path,
    point: ReadPoint,
    deep: bool,
    control: &MaintenanceControl,
) -> Result<BackupVerification, SemanticError> {
    control.check()?;
    let publication = load_publication(directory, &point)?;
    let mut inventory = inventory(directory, &[point.point.manifest_hash], control, deep)?;
    let mut reader = Reader { directory, control };
    let endpoint = metadata_for(&mut reader, &point.value)?;
    if point.value.identity != publication.identity
        || point.value.basis != publication.basis
        || point.value.log != publication.log
        || point.value.metadata != publication.metadata
        || endpoint.generation != point.point.log_generation
    {
        return Err(fault(
            "backup/read-coordinate",
            "Read value differs from its committed publication",
        ));
    }
    let log = match publication.log {
        Some(id) => LogRoot::open(&mut reader, id)?,
        None => LogRoot::empty(),
    };
    if log.basis_t() != publication.basis {
        return Err(fault(
            "backup/log-basis",
            "Captured log does not reach publication basis",
        ));
    }
    if deep {
        // Selective prefix lookup assumes one coherent skip graph. An explicit
        // deep pass proves that alternative skip levels cannot select forks.
        log.validate_structure(&mut reader)?;
    }
    inventory
        .values
        .insert(point.value.id()?, point.value.clone());
    validate_authorization(&mut reader, &publication, &inventory)?;
    let mut values_by_basis = BTreeMap::<u64, Vec<DatabaseValueRoot>>::new();
    for value in inventory.values.values() {
        let metadata = metadata_for(&mut reader, value)?;
        if value.identity != publication.identity
            || value.basis > publication.basis
            || metadata.generation != endpoint.generation
            || value.log != log.prefix_hash(&mut reader, value.basis)?
        {
            return Err(fault(
                "backup/value-authority",
                "Retained value differs from the canonical generation or log prefix",
            ));
        }
        let id = value
            .indexes
            .ok_or_else(|| fault("backup/value-index", "Retained value has no indexes"))?;
        let index = IndexDescriptor::decode(&id, &reader.read_object(id)?)?;
        if index.identity != value.identity
            || index.generation != metadata.generation
            || index.basis > value.basis
        {
            return Err(fault(
                "backup/value-index",
                "Retained index does not belong to its value",
            ));
        }
        values_by_basis
            .entry(value.basis)
            .or_default()
            .push(value.clone());
    }
    validate_receipts(&mut reader, &publication, &log, &inventory)?;
    let mut database = Database::bootstrap()?;
    database.entity_origin =
        crate::entity_identity::DatabaseOrigin::durable(&point.point.lineage_id);
    check_basis(
        &mut reader,
        &database,
        &endpoint,
        &mut inventory,
        &mut values_by_basis,
        deep,
    )?;
    replay::canonical_log(
        &log,
        &mut reader,
        &point.point.lineage_id,
        endpoint.generation,
        control,
        &mut database,
        |reader, database| {
            check_basis(
                reader,
                database,
                &endpoint,
                &mut inventory,
                &mut values_by_basis,
                deep,
            )
        },
    )?;
    if !inventory.indexes.is_empty()
        || !inventory.metadata.is_empty()
        || !values_by_basis.is_empty()
    {
        return Err(fault(
            "backup/future-value",
            "Repository graph retains a value beyond its captured basis",
        ));
    }
    database.validate_invariants()?;
    Ok(BackupVerification {
        point: point.point,
        database,
        objects_read: inventory.count,
    })
}

fn fault(code: &'static str, message: impl Into<String>) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, code, message)
}
