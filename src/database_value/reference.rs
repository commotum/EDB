//! Logical committed identity and bounded, non-capability snapshot references.
use crate::storage::{
    BlockReadConfig, BlockReader, SnapshotMetadata,
    root::{DatabaseRoot, DatabaseValueRoot},
};
use crate::{DatabaseValue, Digest, ErrorCategory, PostgresConnectionConfig, SemanticError};
use std::sync::Arc;

const MAGIC: &[u8; 6] = b"EDBS\0\x03";
const MAX_REFERENCE_BYTES: usize = 256 * 1024;
const MAX_NAME_BYTES: usize = 64 * 1024;
const MAX_VALUE_ROOT_BYTES: usize = 20 + 4 * 32 + 25;

/// Inexpensive logical identity of a committed native value and supported view.
/// Physical publications, cache allocations and the catalog address are absent.
/// This identifies a database input, not a query/result (which also depends on
/// the query, inputs, rules and extension code, and noHistory retention). It is not content equality
/// between independently created databases or a security capability.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct SnapshotKey {
    lineage: Arc<str>,
    generation: u64,
    basis_t: u64,
    transaction_hash: Digest,
    state_hash: Digest,
    eidx_frontier: u64,
    as_of_t: Option<u64>,
    since_t: Option<u64>,
    history: bool,
}

impl SnapshotKey {
    pub fn lineage_id(&self) -> &str {
        &self.lineage
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn basis_t(&self) -> u64 {
        self.basis_t
    }
    pub fn transaction_hash(&self) -> Digest {
        self.transaction_hash
    }
    pub fn state_hash(&self) -> Digest {
        self.state_hash
    }
    pub fn eidx_frontier(&self) -> u64 {
        self.eidx_frontier
    }
    pub fn as_of_t(&self) -> Option<u64> {
        self.as_of_t
    }
    pub fn since_t(&self) -> Option<u64> {
        self.since_t
    }
    pub fn is_history(&self) -> bool {
        self.history
    }

    fn view(&self, mut value: DatabaseValue) -> DatabaseValue {
        if let Some(t) = self.as_of_t {
            value = value.as_of(t);
        }
        if let Some(t) = self.since_t {
            value = value.since(t);
        }
        if self.history {
            value = value.history();
        }
        value
    }
}

/// Versioned route and exact logical coordinate. Holding or serializing this
/// object does not retain database data or grant access. Reopening checks the
/// supplied PostgreSQL authority, lineage, active excision generation and exact
/// authenticated endpoint. Pre-excision references are rejected even if old
/// artifacts remain; already-held values retain their existing retention policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotReference {
    database_id: Arc<str>,
    key: SnapshotKey,
    // Bounded claimed value, never logical key identity or read authority. In particular,
    // noHistory consolidation may omit old pairs at the same committed endpoint.
    value_root: DatabaseValueRoot,
}

impl DatabaseValue {
    /// Capture the authenticated transaction log through this committed basis,
    /// with the same immutable value and read authority and no new connection. Temporal
    /// modifiers select database datoms, not log endpoints: use tx_range bounds
    /// on the returned log. Eager/speculative/opaque-filter values are unsupported.
    pub fn log_value(&self) -> Result<crate::LogValue, SemanticError> {
        let snapshot = self.block_log_snapshot().ok_or_else(|| {
            SemanticError::new(
                ErrorCategory::Unsupported,
                "database/uncommitted-log",
                "Only exact live or repository block values have captured logs",
            )
        })?;
        Ok(snapshot.log())
    }

    /// No SQL or fact scan. Eager fixtures, speculative values and opaque
    /// predicate filters have no portable committed key and return Unsupported.
    /// An as-of view keeps its original basis/schema, so it intentionally differs
    /// from reopening the older committed value itself.
    pub fn snapshot_key(&self) -> Result<SnapshotKey, SemanticError> {
        let (snapshot, as_of_t, since_t, history) = self.committed_block_parts()?;
        let root = snapshot.captured_root();
        Ok(SnapshotKey {
            lineage: Arc::from(snapshot.lineage_id()),
            generation: snapshot.generation(),
            basis_t: snapshot.basis_t(),
            transaction_hash: snapshot
                .captured_log()
                .and_then(|log| log.latest_entry_id())
                .unwrap_or([0; 32]),
            // The log root changes with committed novelty, not with physical
            // indexing. Genesis has its authenticated metadata instead.
            state_hash: root.log.or(root.metadata).ok_or_else(invalid_reference)?,
            eidx_frontier: snapshot.eidx_frontier(),
            as_of_t,
            since_t,
            history,
        })
    }

    pub fn snapshot_reference(&self) -> Result<SnapshotReference, SemanticError> {
        let (snapshot, ..) = self.committed_block_parts()?;
        Ok(SnapshotReference {
            database_id: Arc::from(snapshot.route_id().ok_or_else(invalid_reference)?),
            key: self.snapshot_key()?,
            value_root: snapshot.captured_root().clone(),
        })
    }
}

impl SnapshotReference {
    pub fn database_id(&self) -> &str {
        &self.database_id
    }
    pub fn key(&self) -> &SnapshotKey {
        &self.key
    }

    /// Bounded current-format representation. Its checksum detects corruption, not
    /// forgery; decoded coordinates are always authenticated again when opened.
    /// Reference framing limits do not change supported database identifiers.
    pub fn encode(&self) -> Result<Vec<u8>, SemanticError> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        put_name(&mut bytes, &self.database_id)?;
        put_name(&mut bytes, &self.key.lineage)?;
        for n in [
            self.key.generation,
            self.key.basis_t,
            self.key.eidx_frontier,
        ] {
            bytes.extend_from_slice(&n.to_be_bytes());
        }
        bytes.extend_from_slice(&self.key.transaction_hash);
        bytes.extend_from_slice(&self.key.state_hash);
        let value = self.value_root.encode()?;
        if value.len() > MAX_VALUE_ROOT_BYTES {
            return Err(invalid_reference());
        }
        bytes.extend_from_slice(&(value.len() as u16).to_be_bytes());
        bytes.extend_from_slice(&value);
        for bound in [self.key.as_of_t, self.key.since_t] {
            bytes.push(u8::from(bound.is_some()));
            bytes.extend_from_slice(&bound.unwrap_or(0).to_be_bytes());
        }
        bytes.push(u8::from(self.key.history));
        bytes.extend_from_slice(&crate::sha256(&bytes));
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, SemanticError> {
        if bytes.len() > MAX_REFERENCE_BYTES || bytes.len() < MAGIC.len() + 32 {
            return Err(invalid_reference());
        }
        let (body, hash) = bytes.split_at(bytes.len() - 32);
        if crate::sha256(body) != hash {
            return Err(invalid_reference());
        }
        let mut input = ReferenceInput { bytes: body };
        if input.take(MAGIC.len())? != MAGIC {
            return Err(invalid_reference());
        }
        let database_id = input.name()?;
        let lineage = input.name()?;
        let generation = input.u64()?;
        let basis_t = input.u64()?;
        let eidx_frontier = input.u64()?;
        let transaction_hash = input.take(32)?.try_into().unwrap();
        let state_hash = input.take(32)?.try_into().unwrap();
        let value_length = u16::from_be_bytes(input.take(2)?.try_into().unwrap()) as usize;
        if value_length > MAX_VALUE_ROOT_BYTES {
            return Err(invalid_reference());
        }
        let value_bytes = input.take(value_length)?;
        let value_root = DatabaseValueRoot::decode(&crate::sha256(value_bytes), value_bytes)?;
        let as_of_t = input.bound()?;
        let since_t = input.bound()?;
        let history = input.boolean()?;
        if !input.bytes.is_empty() {
            return Err(invalid_reference());
        }
        let reference = Self {
            database_id,
            value_root,
            key: SnapshotKey {
                lineage,
                generation,
                basis_t,
                transaction_hash,
                state_hash,
                eidx_frontier,
                as_of_t,
                since_t,
                history,
            },
        };
        crate::t_to_tx(reference.key.basis_t)?;
        crate::model::identity::validate_frontier(reference.key.eidx_frontier)?;
        if reference.key.state_hash == [0; 32] {
            return Err(invalid_reference());
        }
        Ok(reference)
    }

    /// Open directly at this endpoint, without recovering/capturing the latest
    /// value first. Discovery/authenticated tail replay may perform I/O; key
    /// construction and comparison do not. Cache limits have normal peer meaning.
    pub fn open(
        &self,
        connection: &PostgresConnectionConfig,
        cache_entries: usize,
        cache_bytes: usize,
    ) -> Result<DatabaseValue, SemanticError> {
        let reader = BlockReader::connect(
            connection,
            BlockReadConfig {
                cache_entries,
                cache_bytes,
                ..BlockReadConfig::default()
            },
        )?;
        self.open_with_reader(&reader)
    }

    /// Reuse an application's reader and caches without advancing its live
    /// value. Only root/log coordinates are read from the current publication;
    /// an unrelated large current tail is never replayed to reopen an old value.
    pub(crate) fn open_with_reader(
        &self,
        reader: &BlockReader,
    ) -> Result<DatabaseValue, SemanticError> {
        let capture = reader.capture_reference(&format!("databases/{}", self.database_id))?;
        let root_id = capture.root_id();
        let root = DatabaseRoot::decode(&root_id, &reader.read_object(root_id)?)?;
        if crate::storage::catalog::identity_string(root.identity) != self.key.lineage.as_ref()
            || root.basis < self.key.basis_t
        {
            return Err(wrong_identity());
        }
        let metadata_id = root.metadata.ok_or_else(invalid_reference)?;
        let metadata = SnapshotMetadata::decode(&metadata_id, &reader.read_object(metadata_id)?)?;
        if metadata.identity != root.identity || metadata.basis != root.basis {
            return Err(wrong_identity());
        }
        if metadata.generation != self.key.generation {
            return Err(SemanticError::new(
                ErrorCategory::Unavailable,
                "snapshot/generation-not-current",
                "Snapshot is outside the current excision generation",
            ));
        }
        if self.key.basis_t != 0 {
            let entry = reader
                .log_record(root.log.ok_or_else(invalid_reference)?, self.key.basis_t)?
                .ok_or_else(wrong_identity)?;
            if entry.id != self.key.transaction_hash
                || entry.entry.eidx_frontier != self.key.eidx_frontier
            {
                return Err(wrong_identity());
            }
        } else if self.key.transaction_hash != [0; 32] {
            return Err(wrong_identity());
        }
        let snapshot = reader.capture_authorized(&root, &capture, &self.value_root)?;
        let value = self.key.view(snapshot.database_value());
        if value.snapshot_key()? != self.key {
            return Err(wrong_identity());
        }
        Ok(value)
    }
}

fn invalid_reference() -> SemanticError {
    SemanticError::incorrect(
        "snapshot/invalid-reference",
        "unsupported or malformed snapshot reference",
    )
}
fn wrong_identity() -> SemanticError {
    SemanticError::new(
        ErrorCategory::Forbidden,
        "snapshot/identity-mismatch",
        "snapshot reference does not match the authorized database lineage",
    )
}
fn put_name(bytes: &mut Vec<u8>, name: &str) -> Result<(), SemanticError> {
    if name.is_empty() || name.len() > MAX_NAME_BYTES {
        return Err(invalid_reference());
    }
    bytes.extend_from_slice(&(name.len() as u32).to_be_bytes());
    bytes.extend_from_slice(name.as_bytes());
    Ok(())
}
struct ReferenceInput<'a> {
    bytes: &'a [u8],
}
impl<'a> ReferenceInput<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], SemanticError> {
        let (value, rest) = self
            .bytes
            .split_at_checked(count)
            .ok_or_else(invalid_reference)?;
        self.bytes = rest;
        Ok(value)
    }
    fn u64(&mut self) -> Result<u64, SemanticError> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn boolean(&mut self) -> Result<bool, SemanticError> {
        match self.take(1)?[0] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(invalid_reference()),
        }
    }
    fn bound(&mut self) -> Result<Option<u64>, SemanticError> {
        let present = self.boolean()?;
        let t = self.u64()?;
        if !present && t != 0 {
            return Err(invalid_reference());
        }
        Ok(present.then_some(t))
    }
    fn name(&mut self) -> Result<Arc<str>, SemanticError> {
        let length = u32::from_be_bytes(self.take(4)?.try_into().unwrap()) as usize;
        if length == 0 || length > MAX_NAME_BYTES {
            return Err(invalid_reference());
        }
        Ok(Arc::from(
            std::str::from_utf8(self.take(length)?).map_err(|_| invalid_reference())?,
        ))
    }
}
