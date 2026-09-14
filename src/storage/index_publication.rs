//! Shared adoption of derived indexes. Preparing data never owns the mutable
//! writer lease; both the writer and explicit operator use this same guarded
//! root replacement without changing canonical log, receipts or metadata.
use super::descriptors::SnapshotMetadata;
use super::log::LogRoot;
use super::read_authorization::ReadAuthorization;
use super::root::DatabaseRoot;
use super::snapshot::RootCapture;
use super::{
    BatchOutcome, BlockReader, BlockSnapshot, Guarded, IndexDescriptor, ObjectId, PgBlockStore,
    RefChange, RefCondition, WriteProtection,
};
use crate::SemanticError;

pub(crate) struct IndexCandidate<'a> {
    pub source_indexes: Option<ObjectId>,
    pub source_index_basis: u64,
    pub descriptor: &'a IndexDescriptor,
    pub descriptor_id: ObjectId,
    pub endpoint_entry: Option<ObjectId>,
    pub protection: &'a WriteProtection,
}

pub(crate) struct IndexPublication {
    pub snapshot: BlockSnapshot,
    pub revision: u64,
}

pub(crate) fn publish_index(
    store: &mut PgBlockStore,
    reader: &BlockReader,
    capture: &RootCapture,
    lease: RefCondition,
    epoch: u64,
    candidate: IndexCandidate<'_>,
) -> Result<IndexPublication, SemanticError> {
    publish(
        store,
        reader,
        capture,
        PublicationLease::Owned(lease),
        epoch,
        candidate,
    )
}

/// Operators do not own a writer lease. Stage immutable derived state under
/// the exact source/root/GC guards, then observe the lease immediately before
/// publication. A renewal need not invalidate expensive immutable staging.
pub(crate) fn publish_index_as_operator(
    store: &mut PgBlockStore,
    reader: &BlockReader,
    capture: &RootCapture,
    lease_key: &str,
    epoch: u64,
    candidate: IndexCandidate<'_>,
) -> Result<IndexPublication, SemanticError> {
    publish(
        store,
        reader,
        capture,
        PublicationLease::Observed(lease_key),
        epoch,
        candidate,
    )
}

enum PublicationLease<'a> {
    Owned(RefCondition),
    Observed(&'a str),
}

fn publish(
    store: &mut PgBlockStore,
    reader: &BlockReader,
    capture: &RootCapture,
    lease: PublicationLease<'_>,
    epoch: u64,
    candidate: IndexCandidate<'_>,
) -> Result<IndexPublication, SemanticError> {
    let result = (|| {
        let root_id = capture.root_id();
        let root = DatabaseRoot::decode(&root_id, &required(store, root_id)?)?;
        let metadata_id = root
            .metadata
            .ok_or_else(|| fault("Current root has no metadata"))?;
        let metadata = SnapshotMetadata::decode(&metadata_id, &required(store, metadata_id)?)?;
        let descriptor = candidate.descriptor;
        if root.writer_epoch != epoch
            || descriptor.identity != root.identity
            || descriptor.generation != metadata.generation
            || descriptor.basis > root.basis
            || descriptor.basis < candidate.source_index_basis
            || root.indexes != candidate.source_indexes
        {
            return Err(SemanticError::conflict(
                "storage/index-source-changed",
                "Prepared index no longer extends the current index lineage",
            ));
        }
        let endpoint = match root.log {
            Some(id) if descriptor.basis > 0 => LogRoot::open(store, id)?
                .record_id(store, descriptor.basis)?
                .ok_or_else(|| fault("Prepared endpoint is missing from the current log"))?
                .into(),
            _ => None,
        };
        if endpoint != candidate.endpoint_entry {
            return Err(SemanticError::conflict(
                "storage/index-log-changed",
                "Prepared index does not cover the authenticated current log prefix",
            ));
        }
        let mut protection = candidate.protection.clone();
        protection.conditions.push(capture.source_condition());
        // Writer-owned adoption retains its original lease guard throughout.
        // Only the explicit operator path observes a non-owned lease late.
        if let PublicationLease::Owned(condition) = &lease {
            protection.conditions.push(condition.clone());
        }
        store.set_write_protection(Some(protection.clone()))?;
        // Validate the original preparation guard before reading candidate
        // provenance: a collector may already have reclaimed stale output.
        if matches!(
            store.protect_existing(
                &[candidate.descriptor_id],
                protection.epoch,
                &protection.conditions,
            )?,
            Guarded::Conflict(_)
        ) {
            return Err(publication_conflict());
        }
        let mut next = root.adopt_indexes(epoch, candidate.descriptor_id)?;
        next.read_authorization = Some(ReadAuthorization::retain_index(
            store,
            root.read_authorization
                .ok_or_else(|| fault("Publication has no read authorization"))?,
            &root.identity,
            candidate.descriptor_id,
        )?);
        let next_id = store.put(&next.encode()?)?;
        let after = reader.capture_immutable(next_id, &protection.conditions)?;
        let changes = [RefChange {
            key: capture.source_condition().key,
            value: Some(next_id.to_vec()),
        }];
        for attempt in 0..4 {
            let mut guards = protection.conditions.clone();
            let observed = match &lease {
                PublicationLease::Owned(_) => None,
                PublicationLease::Observed(key) => {
                    let condition = RefCondition {
                        key: (*key).to_owned(),
                        expected: store.read_ref(key)?.map(|r| r.revision),
                    };
                    guards.push(condition.clone());
                    Some(condition)
                }
            };
            match super::ownership::publish_refs(store, &guards, &changes)? {
                BatchOutcome::Applied(rows) => {
                    return Ok(IndexPublication {
                        snapshot: after,
                        revision: rows[0].1.revision,
                    });
                }
                BatchOutcome::Conflict(current)
                    if attempt < 3
                        && observed.as_ref().is_some_and(|lease| {
                            current
                                .iter()
                                .find(|(key, _)| key == &lease.key)
                                .is_some_and(|(_, r)| {
                                    r.as_ref().map(|r| r.revision) != lease.expected
                                })
                        })
                        && protection.conditions.iter().all(|guard| {
                            current
                                .iter()
                                .find(|(key, _)| key == &guard.key)
                                .is_some_and(|(_, r)| {
                                    r.as_ref().map(|r| r.revision) == guard.expected
                                })
                        }) => {}
                BatchOutcome::Conflict(_) => return Err(publication_conflict()),
            }
        }
        Err(publication_conflict())
    })();
    let clear = store.set_write_protection(None);
    match (result, clear) {
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        (Ok(publication), Ok(())) => Ok(publication),
    }
}

fn required(store: &mut PgBlockStore, id: ObjectId) -> Result<Vec<u8>, SemanticError> {
    store
        .get(id)?
        .ok_or_else(|| fault("Index publication object is missing"))
}
fn fault(message: &'static str) -> SemanticError {
    SemanticError::new(
        crate::ErrorCategory::Fault,
        "storage/index-publication",
        message,
    )
}
fn publication_conflict() -> SemanticError {
    SemanticError::conflict(
        "storage/index-publication-conflict",
        "Index publication lost its root, writer or maintenance guard",
    )
}
