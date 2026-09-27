//! Rust-owned provenance for reopening an untrusted serialized value reference.
//!
//! A publication links one compact descriptor: its initial canonical value and
//! a persistent registry of covering index descriptors that were published.
//! Registry updates use the existing immutable radix-trie kernel and become
//! authority only in the same CAS that adopts their index. No previous
//! publication chain is retained. A collector can rebuild/prune the registry
//! according to current publications and receipts without changing this codec.
//!
//! Membership alone is insufficient: the candidate's entire log and metadata
//! pointers must match a canonical committed value at the requested basis.
//! Callers follow a captured trusted publication while checking, then open the
//! authorized wrapper in memory without writing it. Durable ownership and grace
//! protect the underlying objects, not the lifetime of the caller's handle.
//! Authorization proves child reachability; it does not make an arbitrary
//! caller-supplied wrapper a retained object.

use super::descriptors::{IndexDescriptor, SnapshotMetadata};
use super::receipts::{ExactReceipt, RequestIndex, basis_receipt_key};
use super::root::{Block, DatabaseRoot, DatabaseValueRoot};
use super::{ObjectId, PgBlockStore, RefCondition};
use crate::{ErrorCategory, SemanticError};
use sha2::{Digest as _, Sha256};

pub const READ_AUTHORIZATION_KIND: u16 = 24;
pub const INDEX_AUTHORIZATION_KIND: u16 = 25;
const PAYLOAD_BYTES: usize = 24;
const ENCODED_BYTES: usize = 20 + 2 * 32 + PAYLOAD_BYTES;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadAuthorization {
    identity: [u8; 16],
    initial_basis: u64,
    initial_value: ObjectId,
    indexes: ObjectId,
}

/// Publication provenance has its own age: old indexes that are still used by
/// captured values or receipts cannot be pruned merely because they are old.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct IndexAuthorization {
    pub(crate) identity: [u8; 16],
    pub(crate) index: ObjectId,
    published_ms: u64,
}
impl IndexAuthorization {
    fn put(
        store: &mut PgBlockStore,
        identity: [u8; 16],
        index: ObjectId,
    ) -> Result<ObjectId, SemanticError> {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| denied("Publication clock precedes Unix epoch"))?
            .as_millis();
        let published_ms = u64::try_from(millis)
            .map_err(|_| denied("Publication clock exceeds timestamp range"))?;
        store.put(
            &Self {
                identity,
                index,
                published_ms,
            }
            .encode()?,
        )
    }
    fn encode(&self) -> Result<Vec<u8>, SemanticError> {
        if self.identity == [0; 16] {
            return Err(denied("Index authorization has no database identity"));
        }
        let mut payload = self.identity.to_vec();
        payload.extend_from_slice(&self.published_ms.to_be_bytes());
        Block {
            kind: INDEX_AUTHORIZATION_KIND,
            links: vec![self.index],
            payload,
        }
        .encode()
    }
    fn load(store: &mut PgBlockStore, id: ObjectId) -> Result<Self, SemanticError> {
        Self::decode(&id, &required(store, id)?)
    }
    pub(crate) fn decode(id: &ObjectId, bytes: &[u8]) -> Result<Self, SemanticError> {
        if bytes.len() != 20 + 32 + 24 {
            return Err(denied("Index authorization has an invalid fixed size"));
        }
        let block = Block::decode(id, bytes)?;
        if block.kind != INDEX_AUTHORIZATION_KIND
            || block.links.len() != 1
            || block.payload.len() != 24
        {
            return Err(denied("Expected an index authorization witness"));
        }
        let value = Self {
            identity: block.payload[..16].try_into().unwrap(),
            index: block.links[0],
            published_ms: u64::from_be_bytes(block.payload[16..24].try_into().unwrap()),
        };
        if value.identity == [0; 16] {
            return Err(denied("Index authorization has no database identity"));
        }
        Ok(value)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct AuthorizationPrune {
    pub authorization: ObjectId,
    /// Must join the database-root CAS publishing `authorization`.
    pub guards: Vec<RefCondition>,
    pub examined: usize,
    pub removed: usize,
    pub after: Option<ObjectId>,
    pub complete: bool,
    pub unsettled: bool,
}

impl ReadAuthorization {
    pub(crate) fn identity(&self) -> [u8; 16] {
        self.identity
    }
    pub(crate) fn initial_value(&self) -> ObjectId {
        self.initial_value
    }
    pub(crate) fn initial_basis(&self) -> u64 {
        self.initial_basis
    }
    pub(crate) fn indexes_root(&self) -> ObjectId {
        self.indexes
    }
    /// Stage the initial value and registry. The caller must publish the
    /// returned descriptor under the same protection as the database root.
    pub fn create(
        store: &mut PgBlockStore,
        initial: &DatabaseValueRoot,
        initial_value: ObjectId,
    ) -> Result<ObjectId, SemanticError> {
        if initial.identity == [0; 16] || initial.id()? != initial_value {
            return Err(denied(
                "Initial read anchor differs from its canonical value",
            ));
        }
        let index = initial
            .indexes
            .ok_or_else(|| denied("Initial value has no covering indexes"))?;
        let descriptor = IndexDescriptor::decode(&index, &required(store, index)?)?;
        if descriptor.identity != initial.identity || descriptor.basis > initial.basis {
            return Err(denied("Initial covering index differs from its value"));
        }
        if store.put(&initial.encode()?)? != initial_value {
            return Err(denied("Initial value object identity changed"));
        }
        let witness = IndexAuthorization::put(store, initial.identity, index)?;
        let indexes = RequestIndex::empty()
            .insert(store, index_key(&initial.identity, index), witness)?
            .root()
            .expect("one retained index");
        let descriptor = Self {
            identity: initial.identity,
            initial_basis: initial.basis,
            initial_value,
            indexes,
        };
        store.put(&descriptor.encode()?)
    }

    /// Return a path-copied registry naming the newly prepared descriptor.
    /// This does not authorize it until the database publication CAS succeeds.
    pub fn retain_index(
        store: &mut PgBlockStore,
        authorization: ObjectId,
        identity: &[u8; 16],
        index: ObjectId,
    ) -> Result<ObjectId, SemanticError> {
        let mut descriptor = Self::load(store, authorization, identity)?;
        let index_descriptor = IndexDescriptor::decode(&index, &required(store, index)?)?;
        if index_descriptor.identity != *identity {
            return Err(denied("Retained index belongs to a different database"));
        }
        let registry = RequestIndex::from_root(Some(descriptor.indexes));
        let key = index_key(identity, index);
        if let Some(id) = registry.lookup(store, key)? {
            let witness = IndexAuthorization::load(store, id)?;
            if witness.identity != *identity || witness.index != index {
                return Err(denied("Index authorization differs from its registry key"));
            }
            // A repeated adoption does not restart the retention clock.
            return Ok(authorization);
        }
        let witness = IndexAuthorization::put(store, *identity, index)?;
        descriptor.indexes = RequestIndex::from_root(Some(descriptor.indexes))
            .insert(store, key, witness)?
            .root()
            .expect("retained registry remains nonempty");
        store.put(&descriptor.encode()?)
    }

    /// Examine one bounded registry page. Retire only old indexes with no
    /// owner except their provenance witness. The settled ownership proof is
    /// conditional on its mutation clock, so a concurrent publication
    /// invalidates the caller's eventual database-root CAS.
    pub(crate) fn prune_indexes(
        store: &mut PgBlockStore,
        publication: &DatabaseRoot,
        publication_condition: RefCondition,
        older_than_ms: u64,
        after: Option<ObjectId>,
        maximum: usize,
    ) -> Result<AuthorizationPrune, SemanticError> {
        if !(1..=32).contains(&maximum) || publication_condition.expected.is_none() {
            return Err(SemanticError::incorrect(
                "storage/authorization-prune-limit",
                "Pruning needs a captured publication and a 1–32 entry page",
            ));
        }
        let authorization = publication
            .read_authorization
            .ok_or_else(|| denied("Publication has no read authorization"))?;
        let mut descriptor = Self::load(store, authorization, &publication.identity)?;
        let mut registry = RequestIndex::from_root(Some(descriptor.indexes));
        let page = registry.scan(store, after, maximum)?;
        let original = store.write_protection().cloned();
        let mut proof = original
            .as_ref()
            .map_or_else(Vec::new, |p| p.conditions.clone());
        proof.push(publication_condition);
        let mut report = AuthorizationPrune {
            authorization,
            guards: Vec::new(),
            examined: 0,
            removed: 0,
            after,
            complete: page.len() < maximum,
            unsettled: false,
        };
        let mut removals = Vec::new();
        for (key, witness_id) in page {
            let witness = IndexAuthorization::load(store, witness_id)?;
            if witness.identity != publication.identity
                || key != index_key(&publication.identity, witness.index)
            {
                return Err(denied("Index authorization differs from its registry key"));
            }
            report.examined += 1;
            if witness.published_ms < older_than_ms && Some(witness.index) != publication.indexes {
                let Some((owners, guards)) = super::ownership::settled_count(store, witness.index)?
                else {
                    report.complete = false;
                    report.unsettled = true;
                    break;
                };
                proof.extend(guards);
                if owners == 1 {
                    removals.push(key);
                }
            }
            report.after = Some(key);
        }
        report.guards = merge_guards(proof)?;
        if removals.is_empty() {
            return Ok(report);
        }
        let mut protection = crate::storage::protection::protection(store, &report.guards)?;
        protection.conditions = merge_guards(protection.conditions)?;
        report.guards = protection.conditions.clone();
        store.set_write_protection(Some(protection))?;
        let result = (|| {
            for key in removals {
                registry = registry.remove(store, key)?;
                report.removed += 1;
            }
            descriptor.indexes = registry
                .root()
                .ok_or_else(|| denied("Current covering index must remain authorized"))?;
            report.authorization = store.put(&descriptor.encode()?)?;
            Ok(report)
        })();
        let restored = store.set_write_protection(original);
        match result {
            Err(error) => Err(error),
            Ok(report) => restored.map(|()| report),
        }
    }

    pub fn load(
        store: &mut PgBlockStore,
        id: ObjectId,
        identity: &[u8; 16],
    ) -> Result<Self, SemanticError> {
        let value = Self::decode(&id, &required(store, id)?)?;
        if value.identity != *identity {
            return Err(denied("Read authorization belongs to a different database"));
        }
        Ok(value)
    }

    fn encode(&self) -> Result<Vec<u8>, SemanticError> {
        self.validate()?;
        let mut payload = Vec::with_capacity(PAYLOAD_BYTES);
        payload.extend_from_slice(&self.identity);
        payload.extend_from_slice(&self.initial_basis.to_be_bytes());
        Block {
            kind: READ_AUTHORIZATION_KIND,
            links: vec![self.initial_value, self.indexes],
            payload,
        }
        .encode()
    }

    pub(crate) fn decode(id: &ObjectId, bytes: &[u8]) -> Result<Self, SemanticError> {
        if bytes.len() != ENCODED_BYTES {
            return Err(denied("Read authorization has an invalid fixed size"));
        }
        let block = Block::decode(id, bytes)?;
        if block.kind != READ_AUTHORIZATION_KIND
            || block.links.len() != 2
            || block.payload.len() != PAYLOAD_BYTES
        {
            return Err(denied("Expected a read authorization descriptor"));
        }
        let value = Self {
            identity: block.payload[..16].try_into().unwrap(),
            initial_basis: u64::from_be_bytes(block.payload[16..24].try_into().unwrap()),
            initial_value: block.links[0],
            indexes: block.links[1],
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), SemanticError> {
        if self.identity == [0; 16] || self.initial_basis > 1 {
            return Err(denied(
                "Initial read anchor must describe database creation",
            ));
        }
        Ok(())
    }
}

/// Authenticate a candidate's semantic pointers and covering-index provenance.
/// `publication` must come from a captured, trusted database reference.
/// This helper performs no publication or unbounded history walk.
pub fn authorize_value(
    store: &mut PgBlockStore,
    publication: &DatabaseRoot,
    candidate: &DatabaseValueRoot,
) -> Result<(), SemanticError> {
    if candidate.identity != publication.identity || candidate.basis > publication.basis {
        return Err(denied(
            "Requested value is outside the committed database lineage",
        ));
    }
    let authorization_id = publication
        .read_authorization
        .ok_or_else(|| denied("Publication has no retained read authorization"))?;
    let authorization = ReadAuthorization::load(store, authorization_id, &publication.identity)?;
    let canonical = if candidate.basis == publication.basis {
        DatabaseValueRoot::from(publication)
    } else if candidate.basis == authorization.initial_basis {
        let id = authorization.initial_value;
        DatabaseValueRoot::decode_for_identity(&id, &required(store, id)?, &publication.identity)?
    } else {
        let id = RequestIndex::from_root(publication.receipts)
            .lookup(
                store,
                basis_receipt_key(&publication.identity, candidate.basis),
            )?
            .ok_or_else(|| denied("Requested canonical value is not retained"))?;
        let receipt = ExactReceipt::load(store, id)?;
        if receipt.identity != publication.identity || receipt.basis != candidate.basis {
            return Err(denied(
                "Canonical receipt belongs to a different transaction",
            ));
        }
        DatabaseValueRoot::decode_for_identity(
            &receipt.after,
            &required(store, receipt.after)?,
            &publication.identity,
        )?
    };
    if canonical.basis != candidate.basis
        || canonical.log != candidate.log
        || canonical.metadata != candidate.metadata
    {
        return Err(denied(
            "Requested log or metadata is not the canonical committed value",
        ));
    }
    let index = candidate
        .indexes
        .ok_or_else(|| denied("Requested value has no covering index"))?;
    let witness = RequestIndex::from_root(Some(authorization.indexes))
        .lookup(store, index_key(&publication.identity, index))?
        .ok_or_else(|| denied("Requested covering index was not retained by a publication"))?;
    let witness = IndexAuthorization::load(store, witness)?;
    if witness.identity != publication.identity || witness.index != index {
        return Err(denied(
            "Requested covering index differs from its publication witness",
        ));
    }
    let descriptor = IndexDescriptor::decode(&index, &required(store, index)?)?;
    let metadata_id = candidate
        .metadata
        .ok_or_else(|| denied("Requested value has no metadata"))?;
    let metadata = SnapshotMetadata::decode(&metadata_id, &required(store, metadata_id)?)?;
    let current_metadata_id = publication
        .metadata
        .ok_or_else(|| denied("Publication has no metadata"))?;
    let current_generation = if current_metadata_id == metadata_id {
        metadata.generation
    } else {
        SnapshotMetadata::decode(&current_metadata_id, &required(store, current_metadata_id)?)?
            .generation
    };
    if descriptor.identity != candidate.identity
        || descriptor.basis > candidate.basis
        || metadata.identity != candidate.identity
        || metadata.basis != candidate.basis
        || descriptor.generation != metadata.generation
        || metadata.generation != current_generation
    {
        return Err(denied(
            "Authorized covering index does not describe the requested value",
        ));
    }
    Ok(())
}

fn merge_guards(guards: Vec<RefCondition>) -> Result<Vec<RefCondition>, SemanticError> {
    let mut result = std::collections::BTreeMap::new();
    for guard in guards {
        if result
            .insert(guard.key, guard.expected)
            .is_some_and(|old| old != guard.expected)
        {
            return Err(SemanticError::conflict(
                "storage/authorization-prune-conflict",
                "Ownership changed while proving index retirement",
            ));
        }
    }
    Ok(result
        .into_iter()
        .map(|(key, expected)| RefCondition { key, expected })
        .collect())
}

pub(crate) fn index_key(identity: &[u8; 16], index: ObjectId) -> ObjectId {
    let mut hash = Sha256::new();
    hash.update(b"edb/published-index/1\0");
    hash.update(identity);
    hash.update(index);
    hash.finalize().into()
}

fn required(store: &mut PgBlockStore, id: ObjectId) -> Result<Vec<u8>, SemanticError> {
    store.get(id)?.ok_or_else(|| {
        SemanticError::new(
            ErrorCategory::Fault,
            "storage/missing-object",
            "Read authorization references a missing immutable object",
        )
    })
}

fn denied(message: &str) -> SemanticError {
    SemanticError::new(
        ErrorCategory::Forbidden,
        "snapshot/unpublished-value",
        message,
    )
}
