//! Live peer observation over the single immutable-object storage engine.
use crate::storage::BlockSnapshot;
use crate::{Datom, Digest, SemanticError};
use std::sync::{Mutex, MutexGuard};
mod live;
mod snapshot;
pub use crate::index::cursor::MergeCursorStats as PeerCursorStats;
pub use crate::index::tree::cache::CacheStats;
pub use live::Peer;
pub use snapshot::PeerSnapshot;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecoveryStats {
    pub base_t: u64,
    pub target_t: u64,
    pub tail_transactions: u64,
    /// PostgreSQL transaction-range streams used to authenticate the writer
    /// tail. This is zero for a covered endpoint and one for any non-empty
    /// tail, independent of transaction count.
    pub tail_range_reads: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PeerLoadStats {
    pub root_reads: u64,
    pub directory_reads: u64,
    pub leaf_reads: u64,
    /// Completed or dropped native cursors. This is the exact source-range
    /// count; memo replays do not open another cursor.
    pub cursor_ranges: u64,
    pub cursor_cache_hits: u64,
    pub cursor_cache_misses: u64,
    pub cursor_root_reads: u64,
    pub cursor_directory_reads: u64,
    pub cursor_leaf_reads: u64,
    pub cursor_sql_reads: u64,
    /// Canonical durable-node payload bytes fetched from PostgreSQL by those
    /// cursors. Cache hits correctly contribute zero bytes.
    pub cursor_sql_read_bytes: u64,
    pub cursor_recent_datoms_examined: u64,
    pub cursor_recent_datoms_yielded: u64,
}

/// Lazy ordered traversal retaining one immutable block snapshot.
pub struct PeerIndexCursor {
    cursor: crate::storage::snapshot::BlockIndexCursor,
}
impl PeerIndexCursor {
    pub(crate) fn block(cursor: crate::storage::snapshot::BlockIndexCursor) -> Self {
        Self { cursor }
    }
    pub fn stats(&self) -> PeerCursorStats {
        self.cursor.stats()
    }
    pub(crate) fn next_with_poll(
        &mut self,
        poll: &mut dyn FnMut() -> Result<bool, SemanticError>,
    ) -> Option<Result<Datom, SemanticError>> {
        self.cursor.next_with_poll(poll)
    }
}
impl Iterator for PeerIndexCursor {
    type Item = Result<Datom, SemanticError>;
    fn next(&mut self) -> Option<Self::Item> {
        self.next_with_poll(&mut || Ok(true))
    }
}
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn transaction_hash(snapshot: &BlockSnapshot) -> Digest {
    snapshot
        .captured_log()
        .and_then(|log| log.latest_entry_id())
        .unwrap_or([0; 32])
}
