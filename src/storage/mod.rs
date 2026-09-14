//! Opaque immutable storage and conditional references.
//!
//! The PostgreSQL provider never decodes a database, log, index or receipt. Rust
//! owns those structures and the publication/protection protocol using them.
pub mod catalog;
pub(crate) mod codec;
pub mod descriptors;
pub mod excision;
pub mod fulltext;
pub(crate) mod index_publication;
pub(crate) mod index_recovery;
pub mod indexing;
pub mod log;
pub(crate) mod object_io;
pub mod ownership;
mod postgres;
pub(crate) mod protection;
mod protocol;
pub mod read_authorization;
pub mod receipts;
pub(crate) mod report_handoff;
pub mod root;
pub mod snapshot;

pub use catalog::BlockDatabase;
pub use descriptors::{BlockAvetWork, IndexDescriptor, SnapshotMetadata};
pub use indexing::{BlockIndexStats, IndexInput, PreparedIndex};
pub use object_io::{ObjectReader, ObjectWriter};
pub use postgres::{ObjectInfo, ObjectReadStats, PgBlockStore, WriteProtection};
pub use protocol::{BatchOutcome, RefChange, RefCondition};
pub(crate) use snapshot::BlockIndexCursor;
pub use snapshot::{BlockReadConfig, BlockReadStats, BlockReader, BlockSnapshot};
pub type ObjectId = crate::Digest;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reference {
    pub revision: u64,
    /// A tombstone retains its revision, preventing delete/recreate ABA.
    pub value: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CasOutcome {
    Applied(Reference),
    Conflict(Option<Reference>),
}

/// Generic guarded storage operation. Conflict includes the observed guards so
/// the engine can re-read/rebase; it never silently retries a changed authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Guarded<T> {
    Applied(T),
    Conflict(Vec<(String, Option<Reference>)>),
}

pub const MAX_BATCH: usize = 128;
pub const MAX_REF_BYTES: usize = 1024 * 1024;

fn validate_key(key: &str) -> Result<(), crate::SemanticError> {
    if key.is_empty() || key.len() > 1024 || key.contains('\0') {
        return Err(crate::SemanticError::incorrect(
            "storage/invalid-key",
            "Reference keys must contain 1–1024 UTF-8 bytes without NUL",
        ));
    }
    Ok(())
}

fn validate_limit(limit: usize) -> Result<(), crate::SemanticError> {
    if limit == 0 || limit > MAX_BATCH {
        return Err(crate::SemanticError::incorrect(
            "storage/invalid-limit",
            "Storage batches must contain 1–128 entries",
        ));
    }
    Ok(())
}
