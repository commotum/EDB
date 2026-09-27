//! Immutable byte-key search projection, derived from one captured
//! canonical index descriptor. Its Merkle pages authenticate range boundaries/absence.
//! It does not assign meaning to document/posting values or change datom roots.
use crate::collections::LruMap;
use crate::{Digest, ErrorCategory, OperationContext, SemanticError, sha256};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const PAGE_MAGIC: &[u8; 4] = b"EDBP";
const HEADER_MAGIC: &[u8; 4] = b"EDBF";
const FORMAT: u32 = 1;
// Unicode lowercase may expand a legal 16MiB string (for example U+0130).
// Routing-page capacity can still reject pathological combinations explicitly.
const MAX_KEY: usize = 32 * 1024 * 1024 + 64;
const MAX_RECORD: usize = 64 * 1024 * 1024;
pub(crate) const MAX_PAGE: usize = MAX_RECORD + 16384;
const FANOUT: usize = 128;
const MAX_DEPTH: usize = 32;

mod build;
mod cache;
mod cursor;
pub(crate) mod incremental;
mod model;
mod page;

pub(crate) use build::{PageSummary, build_pages, sorted_records};
use build::{Sorter, read_record, validate_record};
pub(crate) use cache::FulltextCache;
pub use cache::FulltextCacheStats;
pub use cursor::FulltextCursor;
pub(crate) use cursor::lookup_record;
pub(crate) use incremental::FulltextMutation;
pub use model::{
    FulltextBuildLimits, FulltextBuildStats, FulltextProjection, FulltextReadLimits,
    FulltextReadStats, FulltextRecord,
};
pub(crate) use page::{Child, Page};
use page::{Decoder, put_bytes};

fn incorrect(code: &'static str, message: impl Into<String>) -> SemanticError {
    SemanticError::incorrect(code, message)
}
fn fault(code: &'static str, message: impl Into<String>) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, code, message)
}
fn io_error(code: &'static str, error: std::io::Error) -> SemanticError {
    SemanticError::new(ErrorCategory::Unavailable, code, error.to_string())
}
