//! Peer-local search over immutable source-bound candidates.
//!
//! ATOMIC-NOTE: native analysis and authenticated page maintenance fulfill the
//! search/publication roles without importing Lucene's directory or score ABI.
//! Object I/O and coherent publication remain with storage and the transactor.
pub(crate) mod analysis;
mod control;
mod difference;
mod model;
mod reader;
mod records;
mod search;
pub(crate) mod store;

pub(crate) use difference::{HistoryDifference, HistorySource};
pub use model::{FulltextHit, FulltextOptions, FulltextReport, FulltextStats};
pub use reader::NativeFulltextReader;
pub(crate) use records::{DeltaRecords, empty_corpus_from, records_from};
pub use store::{
    FulltextBuildLimits, FulltextBuildStats, FulltextCacheStats, FulltextCursor,
    FulltextProjection, FulltextReadLimits, FulltextReadStats, FulltextRecord,
};
