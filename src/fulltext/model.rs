//! Public search policy and observations, independent of physical placement.
use std::sync::{Arc, atomic::AtomicBool};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct FulltextOptions {
    pub limit: usize,
    pub max_work: usize,
    /// Cumulative admitted logical allocation/read bytes, not process RSS.
    pub max_bytes: usize,
    pub timeout: Option<Duration>,
    pub cancel: Arc<AtomicBool>,
    pub max_query_bytes: usize,
    pub max_query_tokens: usize,
    pub max_query_depth: usize,
}
impl Default for FulltextOptions {
    fn default() -> Self {
        Self {
            limit: 10_000,
            max_work: 10_000_000,
            max_bytes: 64 * 1024 * 1024,
            timeout: None,
            cancel: Arc::new(AtomicBool::new(false)),
            max_query_bytes: 64 * 1024,
            max_query_tokens: 4096,
            max_query_depth: 64,
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct FulltextHit {
    pub entity: u64,
    pub value: String,
    pub tx: u64,
    pub score: f64,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FulltextStats {
    pub work: u64,
    /// Authenticated search-page bytes fetched from the object source; eager
    /// values have no such I/O. Distinct from allocation admission below.
    pub read_bytes: u64,
    pub admitted_bytes: u64,
    /// Persisted search-index basis. Newer committed and speculative assertions
    /// are searched from their bounded in-memory tiers in the same operation.
    pub index_basis_t: u64,
    pub truncated: bool,
}
#[derive(Clone, Debug)]
pub struct FulltextReport {
    pub hits: Vec<FulltextHit>,
    pub stats: FulltextStats,
}
