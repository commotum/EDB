//! Attempt-local logical accounting and bounded complete-prefix memoization.
//! This discardable context must be stripped from committed/report values.
use super::cursor::{DatabaseValuePrefixCursor, DatabaseValuePrefixCursorInner};
use super::value::{DatabaseValue, ReadBasis};
use crate::{Datom, ErrorCategory, IndexPrefix, PeerCursorStats, SemanticError, Value};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

const MAX_TRANSACTION_PREFIX_MEMO_ENTRIES: usize = 4_096;
const MAX_TRANSACTION_PREFIX_MEMO_BYTES: u64 = 64 * 1024 * 1024;

/// Exact logical database work delivered through transaction read APIs.
///
/// This deliberately does not infer work from persistent-tree node loads:
/// cache hits still deliver datoms, while one loaded leaf can contain many
/// datoms outside the requested range. The writer carries one observer across
/// generation, assessment, validation, and successor construction; the
/// attempt-local prefix memo prevents repeated source deliveries while replay
/// remains logically charged.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct LogicalReadWork {
    pub(crate) datoms: u64,
    pub(crate) retained_bytes: u64,
}

/// Transaction-attempt-local read evidence.
///
/// `logical_*` counts every datom delivered to a transaction phase, including
/// memo replays. `source_*` counts successful datoms delivered by an
/// underlying exact-prefix cursor on cache misses. The latter is deliberately
/// a semantic source-delivery counter rather than a PostgreSQL page-I/O
/// counter: a tree leaf may contain candidates outside the requested prefix.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct TransactionReadWork {
    pub(crate) logical_datoms: u64,
    pub(crate) logical_retained_bytes: u64,
    pub(crate) source_datoms: u64,
    pub(crate) source_retained_bytes: u64,
    pub(crate) prefix_hits: u64,
    pub(crate) prefix_misses: u64,
    pub(crate) memo_admissions: u64,
    pub(crate) memo_rejections: u64,
    pub(crate) memo_peak_entries: usize,
    pub(crate) memo_peak_retained_bytes: u64,
    pub(crate) native_cursor_ranges: u64,
    pub(crate) native_cache_hits: u64,
    pub(crate) native_cache_misses: u64,
    pub(crate) native_sql_root_reads: u64,
    pub(crate) native_sql_directory_reads: u64,
    pub(crate) native_sql_leaf_reads: u64,
    pub(crate) native_sql_reads: u64,
    pub(crate) native_sql_read_bytes: u64,
    pub(crate) native_recent_datoms_examined: u64,
    pub(crate) native_recent_datoms_yielded: u64,
}

/// Stable identity of one immutable database value/view while it participates
/// in a transaction attempt. Cache keys retain this allocation, so pointer
/// identity cannot be recycled while an entry remains live.
#[derive(Debug)]
pub(super) struct ReadValueIdentity;

#[derive(Clone, Debug)]
pub(super) struct PrefixMemoKey {
    pub(super) value: Arc<ReadValueIdentity>,
    pub(super) history: bool,
    pub(super) prefix: IndexPrefix,
}

impl PrefixMemoKey {
    /// Stable retained-memory admission weight for the owned map key.
    ///
    /// The enum and its inline fields are covered by `size_of`; owned value
    /// payloads are charged recursively. B-tree node and allocator overhead
    /// remain intentionally outside this allocator-independent estimate.
    pub(super) fn retained_bytes(&self) -> u64 {
        let value_heap = match &self.prefix {
            IndexPrefix::Eavt { value, .. }
            | IndexPrefix::Aevt { value, .. }
            | IndexPrefix::Avet { value, .. }
            | IndexPrefix::Saevt { value, .. }
            | IndexPrefix::Savet { value, .. } => {
                value.as_ref().map_or(0, Value::retained_heap_bytes)
            }
            IndexPrefix::Vaet { value, .. } => value.retained_heap_bytes(),
        };
        (std::mem::size_of::<Self>() as u64).saturating_add(value_heap)
    }
}

impl PartialEq for PrefixMemoKey {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.value, &other.value)
            && self.history == other.history
            && self.prefix == other.prefix
    }
}

impl Eq for PrefixMemoKey {}

impl PartialOrd for PrefixMemoKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PrefixMemoKey {
    fn cmp(&self, other: &Self) -> Ordering {
        let left = Arc::as_ptr(&self.value) as usize;
        let right = Arc::as_ptr(&other.value) as usize;
        left.cmp(&right)
            .then_with(|| self.history.cmp(&other.history))
            .then_with(|| self.prefix.cmp(&other.prefix))
    }
}

#[derive(Default)]
pub(super) struct PrefixMemoState {
    pub(super) entries: BTreeMap<PrefixMemoKey, Arc<[Datom]>>,
    pub(super) retained_bytes: u64,
    pub(super) source_datoms: u64,
    pub(super) source_retained_bytes: u64,
    pub(super) prefix_hits: u64,
    pub(super) prefix_misses: u64,
    pub(super) admissions: u64,
    pub(super) rejections: u64,
    pub(super) peak_entries: usize,
    pub(super) peak_retained_bytes: u64,
    pub(super) native_cursor_ranges: u64,
    pub(super) native_cache_hits: u64,
    pub(super) native_cache_misses: u64,
    pub(super) native_sql_root_reads: u64,
    pub(super) native_sql_directory_reads: u64,
    pub(super) native_sql_leaf_reads: u64,
    pub(super) native_sql_reads: u64,
    pub(super) native_sql_read_bytes: u64,
    pub(super) native_recent_datoms_examined: u64,
    pub(super) native_recent_datoms_yielded: u64,
}

/// One bounded exact-prefix memo shared by all phases of a transaction
/// attempt. It is discardable acceleration only; immutable database values
/// and their observer remain the semantic boundary.
pub(crate) struct TransactionReadContext {
    pub(super) observer: Arc<LogicalReadObserver>,
    pub(super) memo: Mutex<PrefixMemoState>,
    pub(super) max_entries: usize,
    pub(super) max_retained_bytes: u64,
    pub(super) max_entry_datoms: u64,
}

impl TransactionReadContext {
    pub(crate) fn new(max_datoms: u64, max_retained_bytes: u64) -> Self {
        Self {
            observer: Arc::new(LogicalReadObserver::new(max_datoms, max_retained_bytes)),
            memo: Mutex::new(PrefixMemoState::default()),
            max_entries: usize::try_from(max_datoms)
                .unwrap_or(usize::MAX)
                .min(MAX_TRANSACTION_PREFIX_MEMO_ENTRIES),
            max_retained_bytes: max_retained_bytes.min(MAX_TRANSACTION_PREFIX_MEMO_BYTES),
            max_entry_datoms: max_datoms,
        }
    }

    pub(crate) fn remaining(&self) -> Result<LogicalReadWork, SemanticError> {
        self.observer.remaining()
    }

    pub(crate) fn snapshot(&self) -> Result<TransactionReadWork, SemanticError> {
        let logical = self.observer.snapshot()?;
        let memo = self.memo.lock().map_err(|_| read_context_poisoned())?;
        Ok(TransactionReadWork {
            logical_datoms: logical.datoms,
            logical_retained_bytes: logical.retained_bytes,
            source_datoms: memo.source_datoms,
            source_retained_bytes: memo.source_retained_bytes,
            prefix_hits: memo.prefix_hits,
            prefix_misses: memo.prefix_misses,
            memo_admissions: memo.admissions,
            memo_rejections: memo.rejections,
            memo_peak_entries: memo.peak_entries,
            memo_peak_retained_bytes: memo.peak_retained_bytes,
            native_cursor_ranges: memo.native_cursor_ranges,
            native_cache_hits: memo.native_cache_hits,
            native_cache_misses: memo.native_cache_misses,
            native_sql_root_reads: memo.native_sql_root_reads,
            native_sql_directory_reads: memo.native_sql_directory_reads,
            native_sql_leaf_reads: memo.native_sql_leaf_reads,
            native_sql_reads: memo.native_sql_reads,
            native_sql_read_bytes: memo.native_sql_read_bytes,
            native_recent_datoms_examined: memo.native_recent_datoms_examined,
            native_recent_datoms_yielded: memo.native_recent_datoms_yielded,
        })
    }

    pub(crate) fn observer(&self) -> Arc<LogicalReadObserver> {
        Arc::clone(&self.observer)
    }

    pub(super) fn lookup(
        &self,
        key: &PrefixMemoKey,
    ) -> Result<Option<Arc<[Datom]>>, SemanticError> {
        let mut memo = self.memo.lock().map_err(|_| read_context_poisoned())?;
        let found = memo.entries.get(key).cloned();
        if found.is_some() {
            memo.prefix_hits = memo.prefix_hits.saturating_add(1);
        } else {
            memo.prefix_misses = memo.prefix_misses.saturating_add(1);
        }
        Ok(found)
    }

    pub(super) fn record_source_datom(&self, datom: &Datom) -> Result<(), SemanticError> {
        let mut memo = self.memo.lock().map_err(|_| read_context_poisoned())?;
        memo.source_datoms = memo.source_datoms.saturating_add(1);
        memo.source_retained_bytes = memo
            .source_retained_bytes
            .saturating_add(datom.retained_bytes());
        Ok(())
    }

    /// Record the physical work of one native cursor in this transaction
    /// attempt. Unlike shared peer load counters, this attribution cannot be
    /// contaminated by concurrent readers using the same immutable core.
    pub(crate) fn record_native_cursor(&self, stats: PeerCursorStats) -> Result<(), SemanticError> {
        let mut memo = self.memo.lock().map_err(|_| read_context_poisoned())?;
        memo.native_cursor_ranges = memo.native_cursor_ranges.saturating_add(1);
        memo.native_cache_hits = memo.native_cache_hits.saturating_add(stats.tree.cache_hits);
        memo.native_cache_misses = memo
            .native_cache_misses
            .saturating_add(stats.tree.cache_misses);
        memo.native_sql_root_reads = memo
            .native_sql_root_reads
            .saturating_add(stats.tree.root_reads);
        memo.native_sql_directory_reads = memo
            .native_sql_directory_reads
            .saturating_add(stats.tree.directory_reads);
        memo.native_sql_leaf_reads = memo
            .native_sql_leaf_reads
            .saturating_add(stats.tree.leaf_reads);
        let sql_reads = stats
            .tree
            .root_reads
            .saturating_add(stats.tree.directory_reads)
            .saturating_add(stats.tree.leaf_reads);
        memo.native_sql_reads = memo.native_sql_reads.saturating_add(sql_reads);
        memo.native_sql_read_bytes = memo
            .native_sql_read_bytes
            .saturating_add(stats.tree.decoded_bytes);
        memo.native_recent_datoms_examined = memo
            .native_recent_datoms_examined
            .saturating_add(stats.recent.datoms_examined);
        memo.native_recent_datoms_yielded = memo
            .native_recent_datoms_yielded
            .saturating_add(stats.recent.datoms_yielded);
        Ok(())
    }

    pub(super) fn admit(
        &self,
        key: PrefixMemoKey,
        datoms: Vec<Datom>,
        retained_bytes: u64,
    ) -> Result<(), SemanticError> {
        let mut memo = self.memo.lock().map_err(|_| read_context_poisoned())?;
        if memo.entries.contains_key(&key) {
            return Ok(());
        }
        let next_bytes = memo.retained_bytes.checked_add(retained_bytes);
        if memo.entries.len() >= self.max_entries
            || next_bytes.is_none_or(|bytes| bytes > self.max_retained_bytes)
        {
            memo.rejections = memo.rejections.saturating_add(1);
            return Ok(());
        }
        memo.retained_bytes = next_bytes.expect("checked above");
        memo.entries.insert(key, datoms.into());
        memo.admissions = memo.admissions.saturating_add(1);
        memo.peak_entries = memo.peak_entries.max(memo.entries.len());
        memo.peak_retained_bytes = memo.peak_retained_bytes.max(memo.retained_bytes);
        Ok(())
    }

    pub(super) fn reject(&self) -> Result<(), SemanticError> {
        let mut memo = self.memo.lock().map_err(|_| read_context_poisoned())?;
        memo.rejections = memo.rejections.saturating_add(1);
        Ok(())
    }
}

pub(crate) struct LogicalReadObserver {
    pub(super) max_datoms: u64,
    pub(super) max_retained_bytes: u64,
    pub(super) work: Mutex<LogicalReadWork>,
}

impl LogicalReadObserver {
    pub(crate) fn new(max_datoms: u64, max_retained_bytes: u64) -> Self {
        Self {
            max_datoms,
            max_retained_bytes,
            work: Mutex::new(LogicalReadWork::default()),
        }
    }

    pub(crate) fn snapshot(&self) -> Result<LogicalReadWork, SemanticError> {
        self.work
            .lock()
            .map(|work| *work)
            .map_err(|_| read_observer_poisoned())
    }

    pub(crate) fn remaining(&self) -> Result<LogicalReadWork, SemanticError> {
        let work = self.snapshot()?;
        Ok(LogicalReadWork {
            datoms: self.max_datoms.saturating_sub(work.datoms),
            retained_bytes: self.max_retained_bytes.saturating_sub(work.retained_bytes),
        })
    }

    pub(crate) fn charge_datom(&self, datom: &Datom) -> Result<(), SemanticError> {
        self.charge(LogicalReadWork {
            datoms: 1,
            retained_bytes: datom.retained_bytes(),
        })
    }

    pub(super) fn charge(&self, additional: LogicalReadWork) -> Result<(), SemanticError> {
        let mut work = self.work.lock().map_err(|_| read_observer_poisoned())?;
        let next_datoms = work
            .datoms
            .checked_add(additional.datoms)
            .ok_or_else(read_work_overflow)?;
        let next_bytes = work
            .retained_bytes
            .checked_add(additional.retained_bytes)
            .ok_or_else(read_work_overflow)?;
        if next_datoms > self.max_datoms || next_bytes > self.max_retained_bytes {
            return Err(SemanticError::new(
                ErrorCategory::Busy,
                "transaction/read-capacity",
                format!(
                    "transaction reads exceed {} logical datoms or {} retained bytes",
                    self.max_datoms, self.max_retained_bytes
                ),
            ));
        }
        work.datoms = next_datoms;
        work.retained_bytes = next_bytes;
        Ok(())
    }
}

pub(super) fn read_work_overflow() -> SemanticError {
    SemanticError::new(
        ErrorCategory::Busy,
        "transaction/read-capacity",
        "transaction logical read accounting overflowed",
    )
}

pub(super) fn read_observer_poisoned() -> SemanticError {
    SemanticError::new(
        ErrorCategory::Fault,
        "transaction/read-observer-poisoned",
        "transaction logical read observer mutex was poisoned",
    )
}

pub(super) fn read_context_poisoned() -> SemanticError {
    SemanticError::new(
        ErrorCategory::Fault,
        "transaction/read-context-poisoned",
        "transaction prefix memo mutex was poisoned",
    )
}

pub(super) struct PrefixMemoSource {
    pub(super) context: Arc<TransactionReadContext>,
    pub(super) key: Option<PrefixMemoKey>,
    pub(super) datoms: Vec<Datom>,
    pub(super) retained_bytes: u64,
    pub(super) cacheable: bool,
    pub(super) completed: bool,
}

impl DatabaseValue {
    /// Attach the shared, bounded prefix memo and logical observer for one
    /// transaction attempt. This context must never escape into a committed
    /// writer snapshot, receipt, or report queue; the publication handoff
    /// strips it from both db-before and db-after values.
    pub(crate) fn with_transaction_read_context(
        mut self,
        context: Arc<TransactionReadContext>,
    ) -> Self {
        if let ReadBasis::TransactionOverlay(overlay) = &self.basis {
            let mut overlay = (**overlay).clone();
            overlay.base.read_observer = None;
            overlay.base.read_context = None;
            self.basis = ReadBasis::TransactionOverlay(Arc::new(overlay));
        }
        self.read_observer = Some(context.observer());
        self.read_context = Some(context);
        self
    }

    pub(crate) fn transaction_read_context(&self) -> Option<Arc<TransactionReadContext>> {
        self.read_context.clone()
    }

    pub(crate) fn without_transaction_read_context(mut self) -> Self {
        self.read_context = None;
        self.read_observer = None;
        if let ReadBasis::TransactionOverlay(overlay) = &self.basis {
            let mut overlay = (**overlay).clone();
            overlay.base.read_context = None;
            overlay.base.read_observer = None;
            self.basis = ReadBasis::TransactionOverlay(Arc::new(overlay));
        }
        self
    }

    pub(super) fn memoized_prefix_cursor(
        &self,
        history: bool,
        prefix: &IndexPrefix,
    ) -> Result<DatabaseValuePrefixCursor<'_>, SemanticError> {
        crate::transaction_hints::record_prefix(history, prefix);
        let Some(context) = &self.read_context else {
            return self.basis_prefix_cursor(history, prefix, self.read_observer.clone(), None);
        };
        let key = PrefixMemoKey {
            value: Arc::clone(&self.read_identity),
            history,
            prefix: prefix.clone(),
        };
        if let Some(datoms) = context.lookup(&key)? {
            return Ok(DatabaseValuePrefixCursor {
                inner: DatabaseValuePrefixCursorInner::Memoized { datoms, next: 0 },
                observer: Some(context.observer()),
                physical_context: None,
                physical_recorded: false,
                operation: crate::sql_io::OperationContext::current(),
                memo_source: None,
                memo_hit: true,
                failed: false,
            });
        }
        let mut cursor = match self.basis_prefix_cursor(
            history,
            prefix,
            Some(context.observer()),
            Some(Arc::clone(context)),
        ) {
            Ok(cursor) => cursor,
            Err(error) => {
                context.reject()?;
                return Err(error);
            }
        };
        let retained_bytes = key.retained_bytes();
        cursor.memo_source = Some(PrefixMemoSource {
            context: Arc::clone(context),
            key: Some(key),
            datoms: Vec::new(),
            retained_bytes,
            cacheable: retained_bytes <= context.max_retained_bytes,
            completed: false,
        });
        Ok(cursor)
    }
}
