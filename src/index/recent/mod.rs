//! Immutable, authenticated recent-transaction tier.
//!
//! Datomic peers combine durable shallow trees with an in-memory index rebuilt
//! from the authoritative log. This module owns that bounded semantic value;
//! it neither persists data nor evicts an uncovered transaction. A soft limit
//! requests consolidation, while a separate hard limit applies write
//! backpressure if consolidation has not caught up.
pub(crate) mod btset;

use crate::encoding::validate_persistent_index_datoms;
use crate::index::recent::btset::{BtCursor, BtWork, RecentBtSet, RecentDatomRef};
use crate::index::{IndexComponents, NormalizedIndexBoundary};
use crate::{
    Datom, Digest, DurableTransaction, ErrorCategory, IndexOrder, IndexPrefix, Schema,
    SemanticError, ValueType, encode_transaction, t_to_tx, transaction_hash, tx_to_t,
};
use std::cmp::Ordering;
use std::collections::VecDeque;
use std::mem::size_of;
use std::sync::Arc;

const MAX_BTSET_REFERENCES_PER_DATOM: u64 = 5;
const LOG_CHUNK_SIZE: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecentLimits {
    /// Optional deterministic test/deployment guard. Production scheduling is
    /// byte-authoritative; defaults leave this dimension unconstrained.
    pub soft_datoms: u64,
    pub soft_bytes: u64,
    /// Optional deterministic test/deployment guard. Production scheduling is
    /// byte-authoritative; defaults leave this dimension unconstrained.
    pub hard_datoms: u64,
    pub hard_bytes: u64,
}

impl Default for RecentLimits {
    fn default() -> Self {
        Self {
            // Datomic sizes the memory index in bytes at the transactor. A
            // peer must accept that authoritative window; an unrelated local
            // datom-count default would reject valid tails based on value
            // shape rather than configured novelty capacity.
            soft_datoms: u64::MAX,
            soft_bytes: 32 * 1024 * 1024,
            hard_datoms: u64::MAX,
            hard_bytes: 512 * 1024 * 1024,
        }
    }
}

impl RecentLimits {
    fn validate(self) -> Result<(), SemanticError> {
        if self.soft_datoms == 0
            || self.soft_bytes == 0
            || self.hard_datoms == 0
            || self.hard_bytes == 0
            || self.soft_datoms > self.hard_datoms
            || self.soft_bytes > self.hard_bytes
        {
            return Err(SemanticError::incorrect(
                "recent/invalid-limits",
                "recent soft limits must be positive and cannot exceed hard limits",
            ));
        }
        Ok(())
    }
}

/// Endpoint schema used for AVET/VAET membership and, only during an explicit
/// consolidation, the current `:db/noHistory` flag.
///
/// The recent/log tier never projects history away. Datomic documents
/// noHistory as permission for future indexing jobs to forget matching pairs,
/// with no precise removal guarantee and no immediate effect on historical
/// values. Keeping that policy at consolidation avoids inventing stronger
/// database-value semantics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EndpointProjection {
    schema: Arc<Schema>,
}

impl EndpointProjection {
    pub fn new(schema: Schema) -> Self {
        Self {
            schema: Arc::new(schema),
        }
    }

    pub fn from_schema(schema: Arc<Schema>) -> Self {
        Self { schema }
    }

    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    pub(crate) fn shares_schema(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.schema, &other.schema)
    }
}

/// One immutable authenticated log entry retained by the recent value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecentEntry {
    pub transaction: Arc<DurableTransaction>,
    pub hash: Digest,
    pub encoded_bytes: u64,
    pub accounted_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecentStats {
    pub base_t: u64,
    pub end_t: u64,
    pub transactions: u64,
    pub datoms: u64,
    pub encoded_bytes: u64,
    /// Conservative allocator-independent account of retained transaction
    /// values plus persistent-tree datom references.
    pub accounted_bytes: u64,
    pub base_hash: Digest,
    pub end_hash: Digest,
    pub needs_consolidation: bool,
    /// Four raw event-tree entries are retained at most. AVET and VAET are
    /// conditional; no endpoint or touched-key duplicate trees exist.
    pub raw_index_entries: u64,
    pub index_nodes: u64,
    pub max_index_height: u32,
    pub log_chunks: u64,
}

/// Exact deterministic account for the representation retained by one
/// authenticated recent-log entry.
///
/// The transaction service uses this same authority for scheduling and
/// backpressure. Keeping it here prevents its backlog counter from drifting
/// below the hard limit enforced by `RecentTier` when a transaction has a
/// large envelope, tempid map, spare datom capacity, or compactly encoded
/// resident value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RecentEntryStats {
    pub datoms: u64,
    pub encoded_bytes: u64,
    pub accounted_bytes: u64,
}

/// Deterministic work evidence for construction of an immutable recent value.
/// These counters make algorithmic regressions testable without wall clocks.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RecentWork {
    pub authenticated_datoms: u64,
    pub index_insert_attempts: u64,
    pub comparisons: u64,
    pub node_visits: u64,
    pub nodes_copied: u64,
    pub node_splits: u64,
    /// Immediate-child aggregate reads made while copying touched branches.
    /// Includes bookkeeping beyond the root-to-leaf insertion comparisons.
    pub aggregate_child_reads: u64,
    /// Root summaries read to publish final statistics (four indexes and log).
    /// No descendant tree or predecessor-log traversal is needed for stats.
    pub statistics_root_reads: u64,
    pub bulk_rebuild_datoms: u64,
    /// Attributes inspected for an actual endpoint schema transition. An
    /// unchanged shared schema takes the pointer-identity fast path and keeps
    /// this at zero for ordinary transactions.
    pub schema_attributes_examined: u64,
    pub schema_backfill_datoms: u64,
    pub log_entries_copied: u64,
    /// New immutable log chunks allocated, including replacement partial tails.
    pub log_chunks_created: u64,
}

impl RecentWork {
    fn absorb_bt(&mut self, work: BtWork) {
        self.comparisons = self.comparisons.saturating_add(work.comparisons);
        self.node_visits = self.node_visits.saturating_add(work.node_visits);
        self.nodes_copied = self.nodes_copied.saturating_add(work.nodes_copied);
        self.node_splits = self.node_splits.saturating_add(work.node_splits);
        self.aggregate_child_reads = self
            .aggregate_child_reads
            .saturating_add(work.aggregate_child_reads);
    }

    fn saturating_add(self, other: Self) -> Self {
        Self {
            authenticated_datoms: self
                .authenticated_datoms
                .saturating_add(other.authenticated_datoms),
            index_insert_attempts: self
                .index_insert_attempts
                .saturating_add(other.index_insert_attempts),
            comparisons: self.comparisons.saturating_add(other.comparisons),
            node_visits: self.node_visits.saturating_add(other.node_visits),
            nodes_copied: self.nodes_copied.saturating_add(other.nodes_copied),
            node_splits: self.node_splits.saturating_add(other.node_splits),
            aggregate_child_reads: self
                .aggregate_child_reads
                .saturating_add(other.aggregate_child_reads),
            statistics_root_reads: self
                .statistics_root_reads
                .saturating_add(other.statistics_root_reads),
            bulk_rebuild_datoms: self
                .bulk_rebuild_datoms
                .saturating_add(other.bulk_rebuild_datoms),
            schema_attributes_examined: self
                .schema_attributes_examined
                .saturating_add(other.schema_attributes_examined),
            schema_backfill_datoms: self
                .schema_backfill_datoms
                .saturating_add(other.schema_backfill_datoms),
            log_entries_copied: self
                .log_entries_copied
                .saturating_add(other.log_entries_copied),
            log_chunks_created: self
                .log_chunks_created
                .saturating_add(other.log_chunks_created),
        }
    }
}

/// Exact history pair an indexing job may omit for a currently noHistory
/// attribute. Keeping both datoms explicit prevents a caller from turning the
/// storage hint into a broad attribute/time purge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NoHistoryPair {
    pub retraction: Datom,
    pub assertion: Datom,
}

#[derive(Clone, Debug)]
struct RecentIndexes {
    eavt: RecentBtSet,
    aevt: RecentBtSet,
    avet: RecentBtSet,
    vaet: RecentBtSet,
}

impl RecentIndexes {
    fn empty() -> Self {
        Self {
            eavt: RecentBtSet::empty(IndexOrder::Eavt),
            aevt: RecentBtSet::empty(IndexOrder::Aevt),
            avet: RecentBtSet::empty(IndexOrder::Avet),
            vaet: RecentBtSet::empty(IndexOrder::Vaet),
        }
    }

    fn get(&self, order: IndexOrder) -> &RecentBtSet {
        match order {
            IndexOrder::Eavt => &self.eavt,
            IndexOrder::Aevt => &self.aevt,
            IndexOrder::Avet => &self.avet,
            IndexOrder::Vaet => &self.vaet,
        }
    }

    fn replace(&mut self, order: IndexOrder, tree: RecentBtSet) {
        match order {
            IndexOrder::Eavt => self.eavt = tree,
            IndexOrder::Aevt => self.aevt = tree,
            IndexOrder::Avet => self.avet = tree,
            IndexOrder::Vaet => self.vaet = tree,
        }
    }

    fn summary(&self, work: &mut RecentWork) -> (u64, u64, u32) {
        let (mut entries, mut nodes, mut height) = (0_u64, 0_u64, 0_u32);
        for tree in [&self.eavt, &self.aevt, &self.avet, &self.vaet] {
            work.statistics_root_reads = work.statistics_root_reads.saturating_add(1);
            let (tree_entries, tree_nodes, tree_height) = tree.summary();
            entries = entries.saturating_add(tree_entries);
            nodes = nodes.saturating_add(tree_nodes);
            height = height.max(tree_height);
        }
        (entries, nodes, height)
    }
}

#[derive(Debug)]
struct LogChunk {
    previous: Option<Arc<LogChunk>>,
    entries: Arc<[RecentEntry]>,
    total_len: u64,
}

impl Drop for LogChunk {
    fn drop(&mut self) {
        let mut previous = self.previous.take();
        while let Some(chunk) = previous {
            // Consume our ownership atomically. If another snapshot owns this
            // predecessor, it is responsible for its eventual release. Unlike
            // try_unwrap(...).ok(), racing last owners cannot both fail and then
            // recursively drop the chain through an unclaimed Err(Arc).
            let Some(mut chunk) = Arc::into_inner(chunk) else {
                break;
            };
            previous = chunk.previous.take();
            // Dropping this detached chunk releases its entries, but its own
            // Drop sees no predecessor. Native stack depth is therefore fixed.
        }
    }
}

/// Reverse-linked immutable chunks make successor extension bounded while
/// retaining chronological replay. A partial tail copies at most 31 entry
/// records; all older chunks and all transaction payloads remain shared.
#[derive(Clone, Debug, Default)]
struct PersistentLog {
    head: Option<Arc<LogChunk>>,
}

impl PersistentLog {
    fn len(&self) -> u64 {
        self.head.as_ref().map_or(0, |head| head.total_len)
    }

    fn summary(&self, work: &mut RecentWork) -> (u64, u64) {
        work.statistics_root_reads = work.statistics_root_reads.saturating_add(1);
        let entries = self.len();
        // Push seals every predecessor at LOG_CHUNK_SIZE. Only the current
        // tail can be partial, so total_len also determines the chunk count.
        (entries, entries.div_ceil(LOG_CHUNK_SIZE as u64))
    }

    fn push(&self, entry: RecentEntry, work: &mut RecentWork) -> Self {
        work.log_chunks_created = work.log_chunks_created.saturating_add(1);
        let head = match &self.head {
            Some(head) if head.entries.len() < LOG_CHUNK_SIZE => {
                let mut entries = Vec::with_capacity(head.entries.len() + 1);
                entries.extend(head.entries.iter().cloned());
                work.log_entries_copied = work
                    .log_entries_copied
                    .saturating_add(head.entries.len() as u64);
                entries.push(entry);
                Arc::new(LogChunk {
                    previous: head.previous.as_ref().map(Arc::clone),
                    total_len: head
                        .previous
                        .as_ref()
                        .map_or(0, |previous| previous.total_len)
                        .saturating_add(entries.len() as u64),
                    entries: entries.into(),
                })
            }
            Some(head) => Arc::new(LogChunk {
                previous: Some(Arc::clone(head)),
                entries: vec![entry].into(),
                total_len: head.total_len.saturating_add(1),
            }),
            None => Arc::new(LogChunk {
                previous: None,
                entries: vec![entry].into(),
                total_len: 1,
            }),
        };
        Self { head: Some(head) }
    }

    fn entries(&self) -> Vec<RecentEntry> {
        let mut chunks = Vec::new();
        let mut chunk = self.head.as_ref().map(Arc::clone);
        while let Some(current) = chunk {
            chunk = current.previous.as_ref().map(Arc::clone);
            chunks.push(current);
        }
        let mut entries = Vec::with_capacity(self.len() as usize);
        for chunk in chunks.iter().rev() {
            entries.extend(chunk.entries.iter().cloned());
        }
        entries
    }
}

/// Immutable recent tier. Clones share transaction payloads, completed log
/// chunks, projection metadata, and untouched B-tree nodes; failed appends
/// leave the prior value usable.
#[derive(Clone, Debug)]
pub struct RecentTier {
    database_id: Arc<str>,
    base_t: u64,
    base_hash: Digest,
    log: PersistentLog,
    projection: Arc<EndpointProjection>,
    /// The four raw event sets from recovered `IndexSet`. Current values are
    /// streamed by collapsing the newest operation in each stored E/A/V group.
    indexes: RecentIndexes,
    limits: RecentLimits,
    stats: RecentStats,
    index_builds: u64,
    work: RecentWork,
    last_work: RecentWork,
}

impl RecentTier {
    pub fn new(
        database_id: impl Into<String>,
        base_t: u64,
        base_hash: Digest,
        transactions: impl IntoIterator<Item = DurableTransaction>,
        endpoint_projection: EndpointProjection,
        limits: RecentLimits,
    ) -> Result<Self, SemanticError> {
        Self::new_entries(
            database_id,
            base_t,
            base_hash,
            transactions
                .into_iter()
                .map(|transaction| (None, transaction)),
            endpoint_projection,
            limits,
        )
    }

    /// Construct a recent value from log entries whose chain hashes were
    /// already authenticated by the durable log reader. Generation logs hash
    /// their immutable membership envelope, not merely the portable
    /// transaction payload, so that hash cannot be recomputed from
    /// `DurableTransaction` alone.
    pub(crate) fn new_authenticated(
        database_id: impl Into<String>,
        base_t: u64,
        base_hash: Digest,
        transactions: impl IntoIterator<Item = (Digest, DurableTransaction)>,
        endpoint_projection: EndpointProjection,
        limits: RecentLimits,
    ) -> Result<Self, SemanticError> {
        Self::new_entries(
            database_id,
            base_t,
            base_hash,
            transactions
                .into_iter()
                .map(|(hash, transaction)| (Some(hash), transaction)),
            endpoint_projection,
            limits,
        )
    }

    fn new_entries(
        database_id: impl Into<String>,
        base_t: u64,
        base_hash: Digest,
        transactions: impl IntoIterator<Item = (Option<Digest>, DurableTransaction)>,
        endpoint_projection: EndpointProjection,
        limits: RecentLimits,
    ) -> Result<Self, SemanticError> {
        limits.validate()?;
        t_to_tx(base_t)?;
        let database_id = database_id.into();
        if database_id.is_empty() {
            return Err(SemanticError::incorrect(
                "recent/empty-database-id",
                "recent tier database id cannot be empty",
            ));
        }

        let database_id: Arc<str> = database_id.into();
        let mut expected_t = base_t;
        let mut expected_hash = base_hash;
        let mut log = PersistentLog::default();
        let mut indexes = RecentIndexes::empty();
        let mut work = RecentWork::default();
        let mut datom_count = 0_u64;
        let mut byte_count = 0_u64;
        let mut accounted_count = 0_u64;
        for (authenticated_hash, transaction) in transactions {
            expected_t = expected_t.checked_add(1).ok_or_else(|| {
                fault("recent/basis-overflow", "recent transaction basis overflow")
            })?;
            let entry = authenticate_entry(
                &database_id,
                expected_t,
                expected_hash,
                transaction,
                authenticated_hash,
            )?;
            let entry_datoms = entry.transaction.tx_data.len() as u64;
            work.authenticated_datoms = work.authenticated_datoms.saturating_add(entry_datoms);
            work.bulk_rebuild_datoms = work.bulk_rebuild_datoms.saturating_add(entry_datoms);
            datom_count = checked_add(datom_count, entry_datoms, "recent datom count")?;
            byte_count = checked_add(byte_count, entry.encoded_bytes, "recent byte count")?;
            accounted_count = checked_add(
                accounted_count,
                entry.accounted_bytes,
                "recent accounted byte count",
            )?;
            enforce_hard_limit(datom_count, accounted_count, limits)?;
            insert_entry(
                &mut indexes,
                &entry,
                endpoint_projection.schema(),
                &mut work,
            )?;
            expected_hash = entry.hash;
            log = log.push(entry, &mut work);
        }
        let stats = recent_stats(
            base_t,
            base_hash,
            &log,
            &indexes,
            datom_count,
            byte_count,
            accounted_count,
            expected_hash,
            limits,
            &mut work,
        )?;
        Ok(Self {
            database_id,
            base_t,
            base_hash,
            log,
            projection: Arc::new(endpoint_projection),
            indexes,
            limits,
            stats,
            index_builds: 1,
            work,
            last_work: work,
        })
    }

    /// Return a successor recent value or hard-capacity backpressure. No
    /// covered/uncovered entry is removed from `self` in either case.
    pub fn append(
        &self,
        transaction: DurableTransaction,
        endpoint_projection: EndpointProjection,
    ) -> Result<Self, SemanticError> {
        self.extend(std::iter::once(transaction), endpoint_projection)
    }

    /// Authenticate and add a contiguous batch by path-copying only the four
    /// affected search paths per datom. The prior value's roots and completed
    /// log chunks remain shared.
    pub fn extend(
        &self,
        transactions: impl IntoIterator<Item = DurableTransaction>,
        endpoint_projection: EndpointProjection,
    ) -> Result<Self, SemanticError> {
        self.extend_entries(
            transactions
                .into_iter()
                .map(|transaction| (None, transaction)),
            endpoint_projection,
        )
    }

    pub(crate) fn extend_authenticated(
        &self,
        transactions: impl IntoIterator<Item = (Digest, DurableTransaction)>,
        endpoint_projection: EndpointProjection,
    ) -> Result<Self, SemanticError> {
        self.extend_entries(
            transactions
                .into_iter()
                .map(|(hash, transaction)| (Some(hash), transaction)),
            endpoint_projection,
        )
    }

    fn extend_entries(
        &self,
        transactions: impl IntoIterator<Item = (Option<Digest>, DurableTransaction)>,
        endpoint_projection: EndpointProjection,
    ) -> Result<Self, SemanticError> {
        let mut expected_t = self.stats.end_t;
        let mut end_hash = self.stats.end_hash;
        let mut datom_count = self.stats.datoms;
        let mut byte_count = self.stats.encoded_bytes;
        let mut accounted_count = self.stats.accounted_bytes;
        let mut log = self.log.clone();
        let mut indexes = self.indexes.clone();
        let mut last_work = RecentWork::default();
        if !self.projection.shares_schema(&endpoint_projection) {
            backfill_schema_transitions(
                self.projection.schema(),
                endpoint_projection.schema(),
                &mut indexes,
                &mut last_work,
            )?;
        }
        for (authenticated_hash, transaction) in transactions {
            expected_t = expected_t.checked_add(1).ok_or_else(|| {
                fault("recent/basis-overflow", "recent transaction basis overflow")
            })?;
            let entry = authenticate_entry(
                &self.database_id,
                expected_t,
                end_hash,
                transaction,
                authenticated_hash,
            )?;
            let entry_datoms = entry.transaction.tx_data.len() as u64;
            last_work.authenticated_datoms =
                last_work.authenticated_datoms.saturating_add(entry_datoms);
            datom_count = checked_add(datom_count, entry_datoms, "recent datom count")?;
            byte_count = checked_add(byte_count, entry.encoded_bytes, "recent byte count")?;
            accounted_count = checked_add(
                accounted_count,
                entry.accounted_bytes,
                "recent accounted byte count",
            )?;
            enforce_hard_limit(datom_count, accounted_count, self.limits)?;
            insert_entry(
                &mut indexes,
                &entry,
                endpoint_projection.schema(),
                &mut last_work,
            )?;
            end_hash = entry.hash;
            log = log.push(entry, &mut last_work);
        }
        let stats = recent_stats(
            self.base_t,
            self.base_hash,
            &log,
            &indexes,
            datom_count,
            byte_count,
            accounted_count,
            end_hash,
            self.limits,
            &mut last_work,
        )?;
        Ok(Self {
            database_id: Arc::clone(&self.database_id),
            base_t: self.base_t,
            base_hash: self.base_hash,
            log,
            projection: Arc::new(endpoint_projection),
            indexes,
            limits: self.limits,
            stats,
            index_builds: self.index_builds.saturating_add(1),
            work: self.work.saturating_add(last_work),
            last_work,
        })
    }

    pub fn database_id(&self) -> &str {
        &self.database_id
    }

    pub fn projection(&self) -> Arc<EndpointProjection> {
        Arc::clone(&self.projection)
    }

    pub fn includes(&self, order: IndexOrder, datom: &Datom) -> Result<bool, SemanticError> {
        index_member(self.projection.schema(), datom, order)
    }

    pub fn entries(&self) -> Arc<[RecentEntry]> {
        self.log.entries().into()
    }

    /// Sorted authenticated tail datoms with endpoint membership for `order`.
    /// Both assertions and retractions are retained; collapse happens only at
    /// the requested current/history read boundary.
    pub fn datoms(&self, order: IndexOrder) -> Arc<[Datom]> {
        self.cursor(true, order, &RecentRange::unbounded())
            .expect("an authenticated recent tree has a valid unbounded cursor")
            .collect::<Vec<_>>()
            .into()
    }

    /// Sorted current assertions contributed by this tail at its endpoint.
    /// This collects the lazy raw-event cursor;
    /// no endpoint copy is retained in the database value.
    pub fn current_datoms(&self, order: IndexOrder) -> Arc<[Datom]> {
        self.cursor(false, order, &RecentRange::unbounded())
            .expect("an authenticated recent tree has a valid unbounded cursor")
            .collect::<Vec<_>>()
            .into()
    }

    pub(crate) fn touches_current(&self, datom: &Datom) -> bool {
        let mut cursor = self
            .indexes
            .eavt
            .seek_by(|candidate| logical_eav_cmp(candidate, datom));
        while let Some(candidate) = cursor.next() {
            if !same_logical_eav(candidate.datom(), datom) {
                break;
            }
            if same_eav(candidate.datom(), datom) {
                return true;
            }
        }
        false
    }

    pub fn history_seek(&self, order: IndexOrder, key: &Datom) -> Option<Datom> {
        self.cursor(true, order, &RecentRange::new(Some(key.clone()), None))
            .ok()?
            .next()
    }

    pub fn current_seek(&self, order: IndexOrder, key: &Datom) -> Option<Datom> {
        self.cursor(false, order, &RecentRange::new(Some(key.clone()), None))
            .ok()?
            .next()
    }

    pub fn cursor(
        &self,
        history: bool,
        order: IndexOrder,
        range: &RecentRange,
    ) -> Result<RecentCursor, SemanticError> {
        range.validate(order)?;
        Ok(RecentCursor::new(
            self.indexes.get(order),
            Arc::clone(&self.projection),
            history,
            order,
            range.clone(),
        ))
    }

    /// Open a lower-bound cursor over one left-contiguous index prefix.
    ///
    /// Unlike `datoms_with_prefix` collecting helpers, this does not first
    /// collect the recent tier. The persistent memory index seeks directly to
    /// the virtual prefix and stops as soon as the ordered stream leaves it.
    pub(crate) fn prefix_cursor(
        &self,
        history: bool,
        prefix: &IndexPrefix,
    ) -> Result<RecentCursor, SemanticError> {
        prefix.validate()?;
        Ok(RecentCursor::new_prefix(
            self.indexes.get(prefix.order()),
            Arc::clone(&self.projection),
            history,
            prefix.clone(),
        ))
    }

    /// Open a forward cursor at the lowest datom admitted by a typed virtual
    /// boundary. Current traversal begins at the complete logical group when
    /// T is present, then applies the exact boundary to the selected winner.
    pub(crate) fn boundary_cursor(
        &self,
        history: bool,
        boundary: &NormalizedIndexBoundary,
    ) -> RecentCursor {
        RecentCursor::new_boundary(
            self.indexes.get(boundary.order()),
            Arc::clone(&self.projection),
            history,
            boundary.clone(),
        )
    }

    /// Open a reverse cursor immediately after the highest datom matching the
    /// typed virtual boundary. The memory tree descends by upper bound; no
    /// forward collection or reversal is involved.
    pub(crate) fn reverse_boundary_cursor(
        &self,
        history: bool,
        boundary: &NormalizedIndexBoundary,
    ) -> RecentCursor {
        RecentCursor::new_reverse(
            self.indexes.get(boundary.order()),
            Arc::clone(&self.projection),
            history,
            boundary.clone(),
        )
    }

    pub fn stats(&self) -> RecentStats {
        self.stats
    }

    pub fn work(&self) -> RecentWork {
        self.work
    }

    pub fn last_work(&self) -> RecentWork {
        self.last_work
    }

    /// Number of immutable construction generations in this value's lineage.
    /// Use [`RecentTier::last_work`] for actual incremental work; successors
    /// path-copy index nodes and do not rebuild complete indexes.
    pub fn index_builds(&self) -> u64 {
        self.index_builds
    }

    pub fn needs_consolidation(&self) -> bool {
        self.stats.needs_consolidation
    }

    /// Overlay the authenticated tail on a durable current range.
    ///
    /// The durable input may be a logical subrange; every tail delta is
    /// applied before the final bounds filter so a retraction cannot become
    /// invisible merely because its transaction component differs from the
    /// assertion it retracts.
    pub fn merge_current_range(
        &self,
        order: IndexOrder,
        durable_current: &[Datom],
        range: &RecentRange,
    ) -> Result<Vec<Datom>, SemanticError> {
        validate_durable_range(order, durable_current, false, self.base_t)?;
        range.validate(order)?;
        let mut current = Vec::with_capacity(durable_current.len());
        for datom in durable_current {
            if index_member(&self.projection.schema, datom, order)?
                && !self.touches_current(datom)
                && range.contains(datom, order)
            {
                current.push(datom.clone());
            }
        }
        current.extend(self.cursor(false, order, range)?);
        current.sort_by(|left, right| left.cmp_in(right, order));
        Ok(current)
    }

    /// Merge durable retained history with every authenticated recent event.
    ///
    /// noHistory never changes this read: the log/memory tier remains exact
    /// until an indexing job explicitly consolidates it. Results are owned, so
    /// callers do not retain cache/segment borrows across eviction or snapshot
    /// changes.
    pub fn merge_history_range(
        &self,
        order: IndexOrder,
        durable_history: &[Datom],
        range: &RecentRange,
    ) -> Result<Vec<Datom>, SemanticError> {
        validate_durable_range(order, durable_history, true, self.base_t)?;
        range.validate(order)?;
        let mut retained = Vec::with_capacity(durable_history.len());
        for datom in durable_history {
            if index_member(&self.projection.schema, datom, order)? && range.contains(datom, order)
            {
                retained.push(datom.clone());
            }
        }
        retained.extend(self.cursor(true, order, range)?);
        retained.sort_by(|left, right| left.cmp_in(right, order));
        retained.dedup_by(|left, right| same_stored_datom(left, right));
        Ok(retained)
    }

    /// Model the recovered indexing-time `filter-nohist-pairs` operation.
    ///
    /// Datomic applies `filter-nohist-pairs` to future indexing jobs only.
    ///
    /// Any adjacent retraction/assertion pair in the affected merged segment
    /// is eligible while the endpoint attribute is currently noHistory. This
    /// deliberately has no transaction-boundary test: Datomic applies
    /// `filter-nohist-pairs` after merging old and new data for each segment,
    /// so a pair may straddle the persistent/recent boundary and an older pair
    /// may be forgotten incidentally when its segment is rebuilt. This remains
    /// range-local rather than a global retroactive sweep. If noHistory is
    /// false at this indexing boundary, all pairs are retained. The supplied
    /// durable range must include both sides of any E/A/V history group it
    /// expects the consolidator to consider.
    pub fn consolidate_history_range(
        &self,
        order: IndexOrder,
        durable_history: &[Datom],
        range: &RecentRange,
    ) -> Result<Vec<Datom>, SemanticError> {
        range.validate(order)?;
        let merged = self.merge_history_range(order, durable_history, &RecentRange::unbounded())?;
        let mut retained = Vec::with_capacity(merged.len());
        let mut index = 0;
        while index < merged.len() {
            let datom = &merged[index];
            let matching_assertion = merged
                .get(index + 1)
                .is_some_and(|next| self.eligible_no_history_pair(datom, next));
            if matching_assertion {
                index += 2;
            } else {
                retained.push(datom.clone());
                index += 1;
            }
        }
        Ok(retained
            .into_iter()
            .filter(|datom| range.contains(datom, order))
            .collect())
    }

    /// Enumerate exact pair removals for a copy-on-write consolidator.
    pub fn no_history_pairs(
        &self,
        order: IndexOrder,
        durable_history: &[Datom],
    ) -> Result<Vec<NoHistoryPair>, SemanticError> {
        let merged = self.merge_history_range(order, durable_history, &RecentRange::unbounded())?;
        Ok(merged
            .windows(2)
            .filter(|pair| self.eligible_no_history_pair(&pair[0], &pair[1]))
            .map(|pair| NoHistoryPair {
                retraction: pair[0].clone(),
                assertion: pair[1].clone(),
            })
            .collect())
    }

    fn eligible_no_history_pair(&self, retraction: &Datom, assertion: &Datom) -> bool {
        !retraction.added
            && assertion.added
            && same_logical_eav(retraction, assertion)
            && self
                .projection
                .schema
                .attribute(retraction.attribute)
                .is_ok_and(|attribute| attribute.no_history)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecentRange {
    pub start: Option<Datom>,
    pub end: Option<Datom>,
}

impl RecentRange {
    pub fn unbounded() -> Self {
        Self::default()
    }

    pub fn new(start: Option<Datom>, end: Option<Datom>) -> Self {
        Self { start, end }
    }

    fn validate(&self, order: IndexOrder) -> Result<(), SemanticError> {
        if let (Some(start), Some(end)) = (&self.start, &self.end)
            && !start.cmp_in(end, order).is_lt()
        {
            return Err(SemanticError::incorrect(
                "recent/invalid-range",
                "recent range start must be strictly below its exclusive end",
            ));
        }
        Ok(())
    }

    fn contains(&self, datom: &Datom, order: IndexOrder) -> bool {
        self.start
            .as_ref()
            .is_none_or(|start| !datom.cmp_in(start, order).is_lt())
            && self
                .end
                .as_ref()
                .is_none_or(|end| datom.cmp_in(end, order).is_lt())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RecentCursorStats {
    pub comparisons: u64,
    pub node_visits: u64,
    pub datoms_examined: u64,
    pub datoms_yielded: u64,
    /// Largest logical E/A/V history group buffered to collapse a current
    /// value. History cursors leave this at zero.
    pub max_group_datoms: u64,
}

/// Lazy ordered view over one raw memory-index tree. Current cursors collapse
/// one contiguous logical E/A/V group while tracking its distinct stored
/// representations, then expose each representation's newest assertion.
#[derive(Clone, Debug)]
pub struct RecentCursor {
    inner: BtCursor,
    projection: Arc<EndpointProjection>,
    history: bool,
    order: IndexOrder,
    range: RecentRange,
    prefix: Option<IndexPrefix>,
    forward_boundary: Option<NormalizedIndexBoundary>,
    reverse_boundary: Option<NormalizedIndexBoundary>,
    pending: Option<RecentDatomRef>,
    current_output: VecDeque<RecentDatomRef>,
    exhausted: bool,
    examined: u64,
    yielded: u64,
    max_group_datoms: u64,
}

impl RecentCursor {
    fn new(
        tree: &RecentBtSet,
        projection: Arc<EndpointProjection>,
        history: bool,
        order: IndexOrder,
        range: RecentRange,
    ) -> Self {
        let inner = match (&range.start, history) {
            (Some(start), true) => tree.seek(start),
            (Some(start), false) => {
                tree.seek_by(|candidate| primary_cmp_in(candidate, start, order))
            }
            (None, _) => tree.cursor(),
        };
        Self {
            inner,
            projection,
            history,
            order,
            range,
            prefix: None,
            forward_boundary: None,
            reverse_boundary: None,
            pending: None,
            current_output: VecDeque::new(),
            exhausted: false,
            examined: 0,
            yielded: 0,
            max_group_datoms: 0,
        }
    }

    fn new_prefix(
        tree: &RecentBtSet,
        projection: Arc<EndpointProjection>,
        history: bool,
        prefix: IndexPrefix,
    ) -> Self {
        let order = prefix.order();
        let seek_prefix = prefix.clone();
        let inner =
            tree.seek_by(move |candidate| crate::index::compare_prefix(candidate, &seek_prefix));
        Self {
            inner,
            projection,
            history,
            order,
            range: RecentRange::unbounded(),
            prefix: Some(prefix),
            forward_boundary: None,
            reverse_boundary: None,
            pending: None,
            current_output: VecDeque::new(),
            exhausted: false,
            examined: 0,
            yielded: 0,
            max_group_datoms: 0,
        }
    }

    fn new_boundary(
        tree: &RecentBtSet,
        projection: Arc<EndpointProjection>,
        history: bool,
        boundary: NormalizedIndexBoundary,
    ) -> Self {
        let order = boundary.order();
        let seek_boundary = boundary.clone();
        let inner = if history {
            tree.seek_by(move |candidate| seek_boundary.compare_datom(candidate))
        } else {
            tree.seek_by(move |candidate| seek_boundary.compare_current_group_start(candidate))
        };
        Self {
            inner,
            projection,
            history,
            order,
            range: RecentRange::unbounded(),
            prefix: None,
            forward_boundary: Some(boundary),
            reverse_boundary: None,
            pending: None,
            current_output: VecDeque::new(),
            exhausted: false,
            examined: 0,
            yielded: 0,
            max_group_datoms: 0,
        }
    }

    fn new_reverse(
        tree: &RecentBtSet,
        projection: Arc<EndpointProjection>,
        history: bool,
        boundary: NormalizedIndexBoundary,
    ) -> Self {
        let order = boundary.order();
        let inner = match &boundary {
            NormalizedIndexBoundary::Eavt(IndexComponents::Empty)
            | NormalizedIndexBoundary::Aevt(IndexComponents::Empty)
            | NormalizedIndexBoundary::Avet(IndexComponents::Empty)
            | NormalizedIndexBoundary::Vaet(IndexComponents::Empty) => tree.reverse_cursor(),
            _ => tree.reverse_seek_by(|candidate| boundary.compare_datom(candidate)),
        };
        Self {
            inner,
            projection,
            history,
            order,
            range: RecentRange::unbounded(),
            prefix: None,
            forward_boundary: None,
            reverse_boundary: Some(boundary),
            pending: None,
            current_output: VecDeque::new(),
            exhausted: false,
            examined: 0,
            yielded: 0,
            max_group_datoms: 0,
        }
    }

    fn prefix_member(&mut self, datom: &Datom) -> bool {
        match self
            .prefix
            .as_ref()
            .map(|prefix| crate::index::compare_prefix(datom, prefix))
        {
            Some(Ordering::Greater) => {
                self.exhausted = true;
                false
            }
            Some(Ordering::Less) => false,
            Some(Ordering::Equal) | None => true,
        }
    }

    fn poll(
        &mut self,
        poll: &mut dyn FnMut() -> Result<bool, SemanticError>,
    ) -> Result<bool, SemanticError> {
        if self.exhausted {
            return Ok(false);
        }
        if !poll()? {
            self.stop();
            return Ok(false);
        }
        Ok(true)
    }

    fn stop(&mut self) {
        self.exhausted = true;
        self.pending = None;
        self.current_output.clear();
    }

    fn next_member(
        &mut self,
        poll: &mut dyn FnMut() -> Result<bool, SemanticError>,
    ) -> Result<Option<RecentDatomRef>, SemanticError> {
        if let Some(candidate) = self.pending.take() {
            return Ok(Some(candidate));
        }
        loop {
            if !self.poll(poll)? {
                return Ok(None);
            }
            let candidate = if self.reverse_boundary.is_some() {
                self.inner.prev()
            } else {
                self.inner.next()
            };
            let Some(candidate) = candidate else {
                return Ok(None);
            };
            self.examined = self.examined.saturating_add(1);
            if index_member(self.projection.schema(), candidate.datom(), self.order)
                .expect("authenticated recent datom has an endpoint schema attribute")
            {
                return Ok(Some(candidate));
            }
        }
    }

    pub fn stats(&self) -> RecentCursorStats {
        let work = self.inner.work();
        RecentCursorStats {
            comparisons: work.comparisons,
            node_visits: work.node_visits,
            datoms_examined: self.examined,
            datoms_yielded: self.yielded,
            max_group_datoms: self.max_group_datoms,
        }
    }

    fn next_reverse(
        &mut self,
        poll: &mut dyn FnMut() -> Result<bool, SemanticError>,
    ) -> Result<Option<Datom>, SemanticError> {
        if self.history {
            loop {
                let Some(candidate) = self.next_member(poll)? else {
                    return Ok(None);
                };
                let datom = candidate.datom();
                if self
                    .reverse_boundary
                    .as_ref()
                    .is_some_and(|boundary| boundary.compare_datom(datom).is_gt())
                {
                    continue;
                }
                self.yielded = self.yielded.saturating_add(1);
                return Ok(Some(datom.clone()));
            }
        }

        loop {
            if self.current_output.is_empty() {
                let Some(first) = self.next_member(poll)? else {
                    return Ok(None);
                };
                let mut group = vec![first];
                self.max_group_datoms = self.max_group_datoms.max(1);
                while let Some(candidate) = self.next_member(poll)? {
                    if same_logical_eav(group[0].datom(), candidate.datom()) {
                        group.push(candidate);
                        self.max_group_datoms = self.max_group_datoms.max(group.len() as u64);
                    } else {
                        self.pending = Some(candidate);
                        break;
                    }
                }
                if self.exhausted {
                    return Ok(None);
                }
                self.max_group_datoms = self.max_group_datoms.max(group.len() as u64);

                // Reversed history visits one logical group oldest-first.
                // Recovered `distinct-last-by [e a v]` therefore retains its
                // newest event. Atomic deliberately retains one newest event
                // per strict stored V representation: documented rseek is the
                // reverse complement of forward access, and the native kernel
                // treats scale-distinct BigDecimals as distinct stored facts.
                // This buffers only the current logical group, never an index.
                let mut winners = Vec::<RecentDatomRef>::new();
                for candidate in group {
                    if !self.poll(poll)? {
                        return Ok(None);
                    }
                    let value = &candidate.datom().value;
                    match winners.binary_search_by(|prior| prior.datom().value.stored_cmp(value)) {
                        Ok(position) => winners[position] = candidate,
                        Err(position) => winners.insert(position, candidate),
                    }
                }
                winners.retain(|candidate| candidate.datom().added);
                winners.sort_by(|left, right| right.datom().cmp_in(left.datom(), self.order));
                self.current_output.extend(winners);
                if self.current_output.is_empty() {
                    continue;
                }
            }
            let winner = self
                .current_output
                .pop_front()
                .expect("a non-empty reverse current group retained an assertion");
            self.yielded = self.yielded.saturating_add(1);
            return Ok(Some(winner.datom().clone()));
        }
    }

    /// Cooperative admission inside raw membership and current-group loops.
    /// A false poll or error permanently ends this cursor: an incompletely
    /// observed group must never be resumed and mistaken for current truth.
    pub(crate) fn next_with_poll(
        &mut self,
        poll: &mut dyn FnMut() -> Result<bool, SemanticError>,
    ) -> Result<Option<Datom>, SemanticError> {
        let result = (|| {
            if !self.poll(poll)? {
                return Ok(None);
            }
            self.next_ordered(poll)
        })();
        if !matches!(result, Ok(Some(_))) {
            self.stop();
        }
        result
    }

    fn next_ordered(
        &mut self,
        poll: &mut dyn FnMut() -> Result<bool, SemanticError>,
    ) -> Result<Option<Datom>, SemanticError> {
        if self.reverse_boundary.is_some() {
            return self.next_reverse(poll);
        }
        if self.history {
            loop {
                let Some(candidate) = self.next_member(poll)? else {
                    return Ok(None);
                };
                let datom = candidate.datom();
                if !self.prefix_member(datom) {
                    if self.exhausted {
                        return Ok(None);
                    }
                    continue;
                }
                if self
                    .range
                    .end
                    .as_ref()
                    .is_some_and(|end| !datom.cmp_in(end, self.order).is_lt())
                {
                    self.exhausted = true;
                    return Ok(None);
                }
                if self.range.contains(datom, self.order) {
                    if self
                        .forward_boundary
                        .as_ref()
                        .is_some_and(|boundary| boundary.compare_datom(datom).is_lt())
                    {
                        continue;
                    }
                    self.yielded = self.yielded.saturating_add(1);
                    return Ok(Some(datom.clone()));
                }
            }
        }

        loop {
            if self.current_output.is_empty() {
                let Some(first) = self.next_member(poll)? else {
                    return Ok(None);
                };
                let mut group = vec![first];
                self.max_group_datoms = self.max_group_datoms.max(1);
                while let Some(candidate) = self.next_member(poll)? {
                    if same_logical_eav(group[0].datom(), candidate.datom()) {
                        group.push(candidate);
                        self.max_group_datoms = self.max_group_datoms.max(group.len() as u64);
                    } else {
                        self.pending = Some(candidate);
                        break;
                    }
                }
                if self.exhausted {
                    return Ok(None);
                }
                self.max_group_datoms = self.max_group_datoms.max(group.len() as u64);
                // The recovered comparator makes this group newest-first.
                // Select only the first operation for each exact stored value;
                // a newer retraction shadows that representation without
                // hiding a scale-distinct assertion in the same logical group.
                let mut seen = Vec::<RecentDatomRef>::new();
                for candidate in group {
                    if !self.poll(poll)? {
                        return Ok(None);
                    }
                    let value = &candidate.datom().value;
                    match seen.binary_search_by(|prior| prior.datom().value.stored_cmp(value)) {
                        Ok(_) => {}
                        Err(position) => {
                            seen.insert(position, candidate.clone());
                            if candidate.datom().added {
                                self.current_output.push_back(candidate);
                            }
                        }
                    }
                }
                if self.current_output.is_empty() {
                    continue;
                }
            }
            let winner = self
                .current_output
                .pop_front()
                .expect("a non-empty current group retained an assertion");
            let datom = winner.datom();
            if !self.prefix_member(datom) {
                if self.exhausted {
                    return Ok(None);
                }
                continue;
            }
            if self
                .range
                .end
                .as_ref()
                .is_some_and(|end| !datom.cmp_in(end, self.order).is_lt())
            {
                self.exhausted = true;
                return Ok(None);
            }
            if datom.added && self.range.contains(datom, self.order) {
                if self
                    .forward_boundary
                    .as_ref()
                    .is_some_and(|boundary| boundary.compare_datom(datom).is_lt())
                {
                    continue;
                }
                self.yielded = self.yielded.saturating_add(1);
                return Ok(Some(datom.clone()));
            }
        }
    }
}

impl Iterator for RecentCursor {
    type Item = Datom;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_with_poll(&mut || Ok(true))
            .expect("uncontrolled recent cursor polling is infallible")
    }
}

fn authenticate_entry(
    database_id: &str,
    expected_t: u64,
    expected_hash: Digest,
    transaction: DurableTransaction,
    authenticated_hash: Option<Digest>,
) -> Result<RecentEntry, SemanticError> {
    if transaction.database_id != database_id {
        return Err(fault(
            "recent/database-mismatch",
            "recent transaction belongs to a different database",
        ));
    }
    if transaction.basis_t != expected_t {
        return Err(fault(
            "recent/noncontiguous-basis",
            format!(
                "expected recent transaction basis {expected_t}, got {}",
                transaction.basis_t
            ),
        ));
    }
    if transaction.previous_hash != expected_hash {
        return Err(fault(
            "recent/hash-chain-mismatch",
            "recent transaction predecessor does not match the authenticated chain",
        ));
    }
    let (encoded, retained) = encode_and_account_transaction(&transaction)?;
    let hash = authenticated_hash.unwrap_or_else(|| transaction_hash(&encoded));

    Ok(RecentEntry {
        transaction: Arc::new(transaction),
        hash,
        encoded_bytes: retained.encoded_bytes,
        accounted_bytes: retained.accounted_bytes,
    })
}

/// Measure one canonical durable transaction exactly as `RecentTier` will
/// retain it. This is deliberately crate-private: it is an accounting seam,
/// not a second public storage representation.
pub(crate) fn retained_entry_stats(
    transaction: &DurableTransaction,
) -> Result<RecentEntryStats, SemanticError> {
    encode_and_account_transaction(transaction).map(|(_, stats)| stats)
}

fn encode_and_account_transaction(
    transaction: &DurableTransaction,
) -> Result<(Vec<u8>, RecentEntryStats), SemanticError> {
    let encoded = encode_transaction(transaction)?;
    let encoded_bytes = encoded.len() as u64;
    let locator_bytes = (transaction.tx_data.len() as u64)
        .saturating_mul(size_of::<RecentDatomRef>() as u64)
        .saturating_mul(MAX_BTSET_REFERENCES_PER_DATOM);
    let datom_bytes = transaction.tx_data.iter().fold(0_u64, |total, datom| {
        total.saturating_add(datom.retained_bytes())
    });
    let spare_datom_capacity = (transaction
        .tx_data
        .capacity()
        .saturating_sub(transaction.tx_data.len()) as u64)
        .saturating_mul(size_of::<Datom>() as u64);
    let tempid_bytes = transaction.tempids.iter().fold(0_u64, |total, (key, _)| {
        // Conservatively account the key buffer and one independently owned
        // ordered-map entry. Exact BTree node packing is allocator-specific.
        total
            .saturating_add(key.capacity() as u64)
            .saturating_add(size_of::<(String, u64)>() as u64)
            .saturating_add(4 * size_of::<usize>() as u64)
    });
    // Canonical bytes conservatively cover transaction-owned strings,
    // tempids, map nodes, and scalar magnitudes not already represented by
    // the inline datom account. Double-counting payload is preferable to an
    // input shape that slips past the configured resident-memory ceiling.
    let accounted_bytes = (encoded.len() as u64)
        .saturating_add(size_of::<DurableTransaction>() as u64)
        .saturating_add(datom_bytes)
        .saturating_add(spare_datom_capacity)
        .saturating_add(transaction.database_id.capacity() as u64)
        .saturating_add(tempid_bytes)
        .saturating_add(locator_bytes);
    Ok((
        encoded,
        RecentEntryStats {
            datoms: transaction.tx_data.len() as u64,
            encoded_bytes,
            accounted_bytes,
        },
    ))
}

fn insert_reference(
    indexes: &mut RecentIndexes,
    order: IndexOrder,
    reference: RecentDatomRef,
    work: &mut RecentWork,
    duplicate_is_error: bool,
) -> Result<(), SemanticError> {
    work.index_insert_attempts = work.index_insert_attempts.saturating_add(1);
    let mut bt_work = BtWork::default();
    let (tree, inserted) = indexes.get(order).insert(reference, &mut bt_work);
    work.absorb_bt(bt_work);
    if !inserted && duplicate_is_error {
        return Err(fault(
            "recent/duplicate-index-datom",
            "authenticated recent transactions do not form a strict index order",
        ));
    }
    indexes.replace(order, tree);
    Ok(())
}

fn insert_entry(
    indexes: &mut RecentIndexes,
    entry: &RecentEntry,
    schema: &Schema,
    work: &mut RecentWork,
) -> Result<(), SemanticError> {
    for (datom_index, datom) in entry.transaction.tx_data.iter().enumerate() {
        for order in [
            IndexOrder::Eavt,
            IndexOrder::Aevt,
            IndexOrder::Avet,
            IndexOrder::Vaet,
        ] {
            validate_persistent_index_datoms(order, std::slice::from_ref(datom))?;
            if index_member(schema, datom, order)? {
                let reference = RecentDatomRef::new(Arc::clone(&entry.transaction), datom_index)
                    .map_err(|_| {
                        fault(
                            "recent/too-many-transaction-datoms",
                            "one recent transaction cannot address more than u32::MAX datoms",
                        )
                    })?;
                insert_reference(indexes, order, reference, work, true)?;
            }
        }
    }
    Ok(())
}

fn backfill_schema_transitions(
    previous: &Schema,
    endpoint: &Schema,
    indexes: &mut RecentIndexes,
    work: &mut RecentWork,
) -> Result<(), SemanticError> {
    for attribute in endpoint.attributes() {
        work.schema_attributes_examined = work.schema_attributes_examined.saturating_add(1);
        let previous_attribute = previous.attribute(attribute.id).ok();
        let previous_avet = previous_attribute
            .is_some_and(|attribute| attribute.indexed || attribute.unique.is_some());
        let endpoint_avet = attribute.indexed || attribute.unique.is_some();
        let previous_vaet =
            previous_attribute.is_some_and(|attribute| attribute.value_type == ValueType::Ref);
        let endpoint_vaet = attribute.value_type == ValueType::Ref;

        for order in [IndexOrder::Avet, IndexOrder::Vaet] {
            let enabled = match order {
                IndexOrder::Avet => endpoint_avet && !previous_avet,
                IndexOrder::Vaet => endpoint_vaet && !previous_vaet,
                _ => false,
            };
            if !enabled {
                continue;
            }
            let mut source = indexes
                .aevt
                .seek_by(|datom| datom.attribute.cmp(&attribute.id));
            while let Some(reference) = source.next() {
                let datom = reference.datom();
                if datom.attribute != attribute.id {
                    break;
                }
                work.schema_backfill_datoms = work.schema_backfill_datoms.saturating_add(1);
                insert_reference(indexes, order, reference, work, false)?;
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn recent_stats(
    base_t: u64,
    base_hash: Digest,
    log: &PersistentLog,
    indexes: &RecentIndexes,
    datoms: u64,
    encoded_bytes: u64,
    accounted_bytes: u64,
    end_hash: Digest,
    limits: RecentLimits,
    work: &mut RecentWork,
) -> Result<RecentStats, SemanticError> {
    let (transactions, log_chunks) = log.summary(work);
    let (raw_index_entries, index_nodes, max_index_height) = indexes.summary(work);
    let end_t = base_t
        .checked_add(transactions)
        .ok_or_else(|| fault("recent/basis-overflow", "recent transaction basis overflow"))?;
    Ok(RecentStats {
        base_t,
        end_t,
        transactions,
        datoms,
        encoded_bytes,
        accounted_bytes,
        base_hash,
        end_hash,
        needs_consolidation: datoms >= limits.soft_datoms || accounted_bytes >= limits.soft_bytes,
        raw_index_entries,
        index_nodes,
        max_index_height,
        log_chunks,
    })
}

fn index_member(schema: &Schema, datom: &Datom, order: IndexOrder) -> Result<bool, SemanticError> {
    match order {
        IndexOrder::Eavt | IndexOrder::Aevt => Ok(true),
        IndexOrder::Avet => {
            let attribute = schema.attribute(datom.attribute)?;
            Ok(attribute.indexed || attribute.unique.is_some())
        }
        IndexOrder::Vaet => Ok(schema.attribute(datom.attribute)?.value_type == ValueType::Ref),
    }
}

fn validate_durable_range(
    order: IndexOrder,
    datoms: &[Datom],
    history: bool,
    base_t: u64,
) -> Result<(), SemanticError> {
    if datoms
        .windows(2)
        .any(|pair| !pair[0].cmp_in(&pair[1], order).is_lt())
    {
        return Err(SemanticError::incorrect(
            "recent/noncanonical-durable-range",
            "durable range must be strictly ordered in the requested index",
        ));
    }
    if !history && datoms.iter().any(|datom| !datom.added) {
        return Err(SemanticError::incorrect(
            "recent/retraction-in-current-base",
            "durable current range cannot contain retractions",
        ));
    }
    if datoms
        .iter()
        .any(|datom| !tx_to_t(datom.tx).is_ok_and(|t| t <= base_t))
    {
        return Err(SemanticError::incorrect(
            "recent/durable-beyond-base",
            "durable range contains a datom newer than the recent tier base",
        ));
    }
    validate_persistent_index_datoms(order, datoms)
}

fn enforce_hard_limit(
    datoms: u64,
    accounted_bytes: u64,
    limits: RecentLimits,
) -> Result<(), SemanticError> {
    if datoms > limits.hard_datoms || accounted_bytes > limits.hard_bytes {
        return Err(SemanticError::new(
            ErrorCategory::Busy,
            "recent/hard-capacity",
            format!(
                "uncovered recent tail requires {datoms} datoms/{accounted_bytes} accounted resident bytes, above hard limits {}/{}; consolidate before accepting more",
                limits.hard_datoms, limits.hard_bytes
            ),
        ));
    }
    Ok(())
}

fn checked_add(left: u64, right: u64, name: &str) -> Result<u64, SemanticError> {
    left.checked_add(right)
        .ok_or_else(|| fault("recent/count-overflow", format!("{name} overflow")))
}

fn same_eav(left: &Datom, right: &Datom) -> bool {
    left.entity == right.entity
        && left.attribute == right.attribute
        && left.value.stored_eq(&right.value)
}

fn logical_eav_cmp(left: &Datom, right: &Datom) -> Ordering {
    left.entity
        .cmp(&right.entity)
        .then(left.attribute.cmp(&right.attribute))
        .then_with(|| left.value.index_cmp(&right.value))
}

fn primary_cmp_in(left: &Datom, right: &Datom, order: IndexOrder) -> Ordering {
    match order {
        IndexOrder::Eavt => left
            .entity
            .cmp(&right.entity)
            .then(left.attribute.cmp(&right.attribute))
            .then_with(|| left.value.index_cmp(&right.value)),
        IndexOrder::Aevt => left
            .attribute
            .cmp(&right.attribute)
            .then(left.entity.cmp(&right.entity))
            .then_with(|| left.value.index_cmp(&right.value)),
        IndexOrder::Avet => left
            .attribute
            .cmp(&right.attribute)
            .then_with(|| left.value.index_cmp(&right.value))
            .then(left.entity.cmp(&right.entity)),
        IndexOrder::Vaet => left
            .value
            .index_cmp(&right.value)
            .then(left.attribute.cmp(&right.attribute))
            .then(left.entity.cmp(&right.entity)),
    }
}

fn same_logical_eav(left: &Datom, right: &Datom) -> bool {
    left.entity == right.entity
        && left.attribute == right.attribute
        && left.value.index_cmp(&right.value) == Ordering::Equal
}

fn same_stored_datom(left: &Datom, right: &Datom) -> bool {
    same_eav(left, right) && left.tx == right.tx && left.added == right.added
}

fn fault(code: &'static str, message: impl Into<String>) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, code, message)
}
