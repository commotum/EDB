//! Engine-owned immutable blocks and coherent database roots.
//!
//! PostgreSQL stores these bytes without interpreting them. An object's identity
//! is the SHA-256 of its entire encoding, including its kind and outgoing links.
//! Links preserve caller order (and duplicates); they are not a sorted set.

use super::ObjectId;
use crate::{SemanticError, sha256};

const MAGIC: &[u8; 4] = b"EDBO";
// Source-bearing datoms and twelve-tree descriptors form one storage cutover.
// Reject earlier blocks before interpreting their unversioned log payloads.
// The opaque PostgreSQL provider and physical compression format are unchanged.
const VERSION: u16 = 3;
const HEADER_BYTES: usize = 20;
const OBJECT_ID_BYTES: usize = 32;
const ROOT_PAYLOAD_BYTES: usize = 33;
const VALUE_ROOT_PAYLOAD_BYTES: usize = 25;

/// Inclusive encoded-object ceiling, not a claim about peak process memory.
pub const MAX_BLOCK_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_BLOCK_LINKS: usize = 65_536;
/// The kind reserved for [`DatabaseRoot`]. Other nonzero kinds belong to engine
/// codecs; an unknown kind remains a valid opaque block, not a valid root.
pub const DATABASE_ROOT_KIND: u16 = 1;
/// A captured database value, without publication authority or receipt roots.
pub const DATABASE_VALUE_ROOT_KIND: u16 = 2;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Block {
    pub kind: u16,
    pub links: Vec<ObjectId>,
    pub payload: Vec<u8>,
}

impl Block {
    pub fn encode(&self) -> Result<Vec<u8>, SemanticError> {
        validate_kind(self.kind)?;
        let length = encoded_length(self.links.len(), self.payload.len())?;
        let mut bytes = Vec::with_capacity(length);
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&VERSION.to_be_bytes());
        bytes.extend_from_slice(&self.kind.to_be_bytes());
        bytes.extend_from_slice(&(self.links.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&(self.payload.len() as u64).to_be_bytes());
        for link in &self.links {
            bytes.extend_from_slice(link);
        }
        bytes.extend_from_slice(&self.payload);
        Ok(bytes)
    }

    pub fn id(&self) -> Result<ObjectId, SemanticError> {
        Ok(sha256(&self.encode()?))
    }

    /// Authenticate before allocating either decoded collection. The caller must
    /// obtain `expected` from a trusted reference or an authenticated parent.
    pub fn decode(expected: &ObjectId, bytes: &[u8]) -> Result<Self, SemanticError> {
        if bytes.len() < HEADER_BYTES || bytes.len() > MAX_BLOCK_BYTES {
            return Err(invalid(
                "storage/block-size",
                "Block size is outside the format limits",
            ));
        }
        if sha256(bytes) != *expected {
            return Err(invalid(
                "storage/block-hash",
                "Block content does not match its object identity",
            ));
        }
        if &bytes[..4] != MAGIC || u16::from_be_bytes([bytes[4], bytes[5]]) != VERSION {
            return Err(invalid(
                "storage/block-format",
                "Unrecognized block format or version",
            ));
        }
        let kind = u16::from_be_bytes([bytes[6], bytes[7]]);
        validate_kind(kind)?;
        let count = u32::from_be_bytes(bytes[8..12].try_into().unwrap()) as usize;
        let payload_length = usize::try_from(u64::from_be_bytes(bytes[12..20].try_into().unwrap()))
            .map_err(|_| {
                invalid(
                    "storage/block-size",
                    "Block payload length is not addressable",
                )
            })?;
        if encoded_length(count, payload_length)? != bytes.len() {
            return Err(invalid(
                "storage/block-length",
                "Block lengths do not exactly match its encoding",
            ));
        }
        let payload_start = HEADER_BYTES + count * OBJECT_ID_BYTES;
        let links = bytes[HEADER_BYTES..payload_start]
            .as_chunks::<OBJECT_ID_BYTES>()
            .0
            .to_vec();
        Ok(Self {
            kind,
            links,
            payload: bytes[payload_start..].to_vec(),
        })
    }
}

/// A coherent engine state published through one conditional reference update.
///
/// `basis` orders committed transactions; it is NOT the storage CAS revision.
/// Index adoption and writer claims replace this descriptor without increasing
/// the basis. The root pointers are authenticated by this one object ID.
/// Each pointed-to engine codec must additionally validate its own identity,
/// basis and reachability invariants; this envelope cannot inspect absent data.
/// Receipt entries refer to [`DatabaseValueRoot`], not this descriptor: referring
/// back to a root containing the receipt map would create a content-address cycle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DatabaseRoot {
    pub identity: [u8; 16],
    pub basis: u64,
    pub writer_epoch: u64,
    pub log: Option<ObjectId>,
    pub indexes: Option<ObjectId>,
    pub receipts: Option<ObjectId>,
    pub metadata: Option<ObjectId>,
    /// Initial canonical value and the bounded-path registry of published
    /// covering indexes. This is publication authority, not value identity.
    pub read_authorization: Option<ObjectId>,
}

impl DatabaseRoot {
    pub fn encode(&self) -> Result<Vec<u8>, SemanticError> {
        self.validate()?;
        let mut links = Vec::with_capacity(5);
        let mut flags = 0u8;
        for (slot, link) in [
            self.log,
            self.indexes,
            self.receipts,
            self.metadata,
            self.read_authorization,
        ]
        .into_iter()
        .enumerate()
        {
            if let Some(link) = link {
                flags |= 1 << slot;
                links.push(link);
            }
        }
        let mut payload = Vec::with_capacity(ROOT_PAYLOAD_BYTES);
        payload.extend_from_slice(&self.identity);
        payload.extend_from_slice(&self.basis.to_be_bytes());
        payload.extend_from_slice(&self.writer_epoch.to_be_bytes());
        payload.push(flags);
        Block {
            kind: DATABASE_ROOT_KIND,
            links,
            payload,
        }
        .encode()
    }

    pub fn id(&self) -> Result<ObjectId, SemanticError> {
        Ok(sha256(&self.encode()?))
    }

    pub fn decode(expected: &ObjectId, bytes: &[u8]) -> Result<Self, SemanticError> {
        // A root has at most five outgoing pointers. Reject an oversized generic
        // block here, before its payload could be cloned by the generic decoder.
        if bytes.len() > HEADER_BYTES + 5 * OBJECT_ID_BYTES + ROOT_PAYLOAD_BYTES {
            return Err(invalid(
                "storage/root-size",
                "Database root exceeds its fixed format size",
            ));
        }
        let block = Block::decode(expected, bytes)?;
        if block.kind != DATABASE_ROOT_KIND || block.payload.len() != ROOT_PAYLOAD_BYTES {
            return Err(invalid(
                "storage/root-format",
                "Object is not a database root descriptor",
            ));
        }
        let flags = block.payload[32];
        if flags & !0x1f != 0 || flags.count_ones() as usize != block.links.len() {
            return Err(invalid(
                "storage/root-links",
                "Database root pointer flags do not match its links",
            ));
        }
        let mut links = block.links.into_iter();
        let mut pointer = |slot: u32| {
            if flags & (1u8 << slot) != 0 {
                links.next()
            } else {
                None
            }
        };
        let root = Self {
            identity: block.payload[..16].try_into().unwrap(),
            basis: u64::from_be_bytes(block.payload[16..24].try_into().unwrap()),
            writer_epoch: u64::from_be_bytes(block.payload[24..32].try_into().unwrap()),
            log: pointer(0),
            indexes: pointer(1),
            receipts: pointer(2),
            metadata: pointer(3),
            read_authorization: pointer(4),
        };
        root.validate()?;
        Ok(root)
    }

    pub fn decode_for_identity(
        expected: &ObjectId,
        bytes: &[u8],
        identity: &[u8; 16],
    ) -> Result<Self, SemanticError> {
        let root = Self::decode(expected, bytes)?;
        if root.identity != *identity {
            return Err(invalid(
                "storage/root-identity",
                "Database root belongs to another identity",
            ));
        }
        Ok(root)
    }

    fn validate(&self) -> Result<(), SemanticError> {
        // The schema convenience constructor can publish its initial schema
        // transaction without a caller request key. Receipt retention can also
        // leave an empty map. Per-request receipt publication is a writer
        // invariant, not a requirement that every nonzero root has a receipt.
        if self.basis > 0 && self.log.is_none() {
            return Err(invalid(
                "storage/root-incomplete",
                "A committed basis requires a log root",
            ));
        }
        Ok(())
    }

    /// Construct a writer claim; this does not itself acquire authority. The
    /// caller must durably publish it by CAS against this root's exact reference
    /// revision. Every subsequent publication uses that claimed revision (or a
    /// successor it has authenticated), so an intervening claim fences it out.
    pub fn claim_writer(&self) -> Result<Self, SemanticError> {
        self.validate()?;
        let writer_epoch = self
            .writer_epoch
            .checked_add(1)
            .ok_or_else(|| invalid("storage/writer-epoch-overflow", "Writer epoch is exhausted"))?;
        Ok(Self {
            writer_epoch,
            ..self.clone()
        })
    }

    /// Adopt an index on the CURRENT authenticated root, preserving its current
    /// log, receipts, metadata and basis. Validate the index's captured source
    /// against that root before calling. CAS conflicts require reloading and
    /// repeating that validation; blindly retrying an old descriptor can lose
    /// newer transactions. The storage primitive remains the authority fence.
    pub fn adopt_indexes(
        &self,
        expected_epoch: u64,
        indexes: ObjectId,
    ) -> Result<Self, SemanticError> {
        self.validate()?;
        // Derived-index maintenance can run before the first writer claim.
        // Epoch zero is valid only when the captured root is still unclaimed;
        // publication must guard that exact root and lease revision as usual.
        if self.writer_epoch != expected_epoch {
            return Err(SemanticError::conflict(
                "storage/writer-fenced",
                "Writer epoch does not match the current database root",
            ));
        }
        Ok(Self {
            indexes: Some(indexes),
            ..self.clone()
        })
    }
}

/// The immutable database value captured by an exact before/after receipt.
///
/// This descriptor deliberately excludes receipts and writer authority. Its log,
/// indexes and metadata describe the captured value, while the publication root
/// independently names the receipt map. Consequently a receipt can link to this
/// object without its own identity becoming an input to the object's hash.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DatabaseValueRoot {
    pub identity: [u8; 16],
    pub basis: u64,
    pub log: Option<ObjectId>,
    pub indexes: Option<ObjectId>,
    pub metadata: Option<ObjectId>,
}

impl From<&DatabaseRoot> for DatabaseValueRoot {
    fn from(root: &DatabaseRoot) -> Self {
        Self {
            identity: root.identity,
            basis: root.basis,
            log: root.log,
            indexes: root.indexes,
            metadata: root.metadata,
        }
    }
}

impl DatabaseValueRoot {
    pub fn encode(&self) -> Result<Vec<u8>, SemanticError> {
        self.validate()?;
        let mut links = Vec::with_capacity(3);
        let mut flags = 0u8;
        for (slot, link) in [self.log, self.indexes, self.metadata]
            .into_iter()
            .enumerate()
        {
            if let Some(link) = link {
                flags |= 1 << slot;
                links.push(link);
            }
        }
        let mut payload = Vec::with_capacity(VALUE_ROOT_PAYLOAD_BYTES);
        payload.extend_from_slice(&self.identity);
        payload.extend_from_slice(&self.basis.to_be_bytes());
        payload.push(flags);
        Block {
            kind: DATABASE_VALUE_ROOT_KIND,
            links,
            payload,
        }
        .encode()
    }

    pub fn id(&self) -> Result<ObjectId, SemanticError> {
        Ok(sha256(&self.encode()?))
    }

    pub fn decode(expected: &ObjectId, bytes: &[u8]) -> Result<Self, SemanticError> {
        if bytes.len() > HEADER_BYTES + 3 * OBJECT_ID_BYTES + VALUE_ROOT_PAYLOAD_BYTES {
            return Err(invalid(
                "storage/value-root-size",
                "Database value root exceeds its fixed format size",
            ));
        }
        let block = Block::decode(expected, bytes)?;
        if block.kind != DATABASE_VALUE_ROOT_KIND || block.payload.len() != VALUE_ROOT_PAYLOAD_BYTES
        {
            return Err(invalid(
                "storage/value-root-format",
                "Object is not a database value root descriptor",
            ));
        }
        let flags = block.payload[24];
        if flags & !0x07 != 0 || flags.count_ones() as usize != block.links.len() {
            return Err(invalid(
                "storage/value-root-links",
                "Database value pointer flags do not match its links",
            ));
        }
        let mut links = block.links.into_iter();
        let mut pointer = |slot: u32| {
            if flags & (1u8 << slot) != 0 {
                links.next()
            } else {
                None
            }
        };
        let root = Self {
            identity: block.payload[..16].try_into().unwrap(),
            basis: u64::from_be_bytes(block.payload[16..24].try_into().unwrap()),
            log: pointer(0),
            indexes: pointer(1),
            metadata: pointer(2),
        };
        root.validate()?;
        Ok(root)
    }

    pub fn decode_for_identity(
        expected: &ObjectId,
        bytes: &[u8],
        identity: &[u8; 16],
    ) -> Result<Self, SemanticError> {
        let root = Self::decode(expected, bytes)?;
        if root.identity != *identity {
            return Err(invalid(
                "storage/value-root-identity",
                "Database value root belongs to another identity",
            ));
        }
        Ok(root)
    }

    fn validate(&self) -> Result<(), SemanticError> {
        if self.basis > 0 && self.log.is_none() {
            return Err(invalid(
                "storage/value-root-incomplete",
                "A committed database value requires a log root",
            ));
        }
        Ok(())
    }
}

fn validate_kind(kind: u16) -> Result<(), SemanticError> {
    if kind == 0 {
        Err(invalid("storage/block-kind", "Block kind zero is reserved"))
    } else {
        Ok(())
    }
}

fn encoded_length(links: usize, payload: usize) -> Result<usize, SemanticError> {
    if links > MAX_BLOCK_LINKS {
        return Err(invalid(
            "storage/block-links",
            "Block has too many outgoing links",
        ));
    }
    let length = links
        .checked_mul(OBJECT_ID_BYTES)
        .and_then(|n| n.checked_add(HEADER_BYTES))
        .and_then(|n| n.checked_add(payload))
        .filter(|n| *n <= MAX_BLOCK_BYTES)
        .ok_or_else(|| invalid("storage/block-size", "Block size exceeds the format limit"))?;
    Ok(length)
}

fn invalid(code: &'static str, message: &'static str) -> SemanticError {
    SemanticError::incorrect(code, message)
}
