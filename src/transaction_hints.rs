//! Advisory semantic read hints. Hints never supply trusted blocks or facts:
//! prefetch uses the writer's ordinary authenticated current-value read path.
use crate::{
    DatabaseValue, EntityRef, IndexPrefix, OperationContext, OperationKind, SemanticError,
    SnapshotReference, SpeculationLimits, SpeculativeTransactionReport, TxForm,
};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

const MAX_HINT_PREFIXES: usize = 4096;
const MAX_HINT_BYTES: u64 = 4 * 1024 * 1024;
const MAX_HINT_ORIGIN_BYTES: u64 = 256 * 1024;
const MAX_PREFETCH_DATOMS: u64 = 16_384;
const MAX_PREFETCH_READ_BYTES: u64 = 16 * 1024 * 1024;
const MAX_PREFETCH_TIME: Duration = Duration::from_secs(1);
const MAX_ACTIVE_PREFETCH_WORKERS: usize = 8;
static ACTIVE_PREFETCH_WORKERS: AtomicUsize = AtomicUsize::new(0);

/// Per-service advisory read concurrency. Every lane has independent SQL and
/// retention ownership; all lanes share the attempt's read budgets. The fixed
/// process ceiling of eight workers also includes canceled/stalled old services.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HintPrefetchConcurrency {
    /// Zero disables advisory workers. Valid values are 0..=8; default is one.
    pub max_workers: usize,
}
impl Default for HintPrefetchConcurrency {
    fn default() -> Self {
        Self { max_workers: 1 }
    }
}
impl HintPrefetchConcurrency {
    pub(crate) fn validate(self) -> Result<Self, SemanticError> {
        if self.max_workers > MAX_ACTIVE_PREFETCH_WORKERS {
            return Err(SemanticError::incorrect(
                "hints/invalid-concurrency",
                "hint prefetch concurrency must be between zero and eight",
            ));
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct HintLimits {
    pub max_prefixes: usize,
    /// Allocator-independent retained prefix weight, including owned values.
    /// The shared origin has a separate fixed 256 KiB admission bound.
    pub max_bytes: u64,
}
impl Default for HintLimits {
    fn default() -> Self {
        Self {
            max_prefixes: 128,
            max_bytes: 64 * 1024,
        }
    }
}
impl HintLimits {
    fn bounded(self) -> Self {
        Self {
            max_prefixes: self.max_prefixes.min(MAX_HINT_PREFIXES),
            max_bytes: self.max_bytes.min(MAX_HINT_BYTES),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct ReadHint {
    pub history: bool,
    pub prefix: IndexPrefix,
}
impl ReadHint {
    fn weight(&self) -> u64 {
        prefix_weight(&self.prefix)
    }
}
fn prefix_weight(prefix: &IndexPrefix) -> u64 {
    fn heap(value: &crate::Value, remaining: u64, depth: usize) -> Option<u64> {
        if depth > 16 {
            return None;
        }
        let bytes = if let crate::Value::Tuple(slots) = value {
            let mut bytes = (slots.capacity() as u64)
                .checked_mul(std::mem::size_of::<Option<crate::Value>>() as u64)?;
            if bytes > remaining {
                return None;
            }
            for value in slots.iter().flatten() {
                bytes = bytes.checked_add(heap(value, remaining - bytes, depth + 1)?)?;
            }
            bytes
        } else {
            value.retained_heap_bytes()
        };
        (bytes <= remaining).then_some(bytes)
    }
    let value = match prefix {
        IndexPrefix::Eavt { value, .. }
        | IndexPrefix::Aevt { value, .. }
        | IndexPrefix::Avet { value, .. }
        | IndexPrefix::Saevt { value, .. }
        | IndexPrefix::Savet { value, .. } => value.as_ref(),
        IndexPrefix::Vaet { value, .. } => Some(value),
    };
    let bytes = value
        .map_or(Some(0), |value| heap(value, MAX_HINT_BYTES, 0))
        .unwrap_or(u64::MAX);
    (std::mem::size_of::<ReadHint>() as u64).saturating_add(bytes)
}
fn origin_fits(origin: &SnapshotReference) -> bool {
    (std::mem::size_of::<SnapshotReference>() as u64)
        .saturating_add(origin.database_id().len() as u64)
        .saturating_add(origin.key().lineage_id().len() as u64)
        <= MAX_HINT_ORIGIN_BYTES
}

/// Immutable, bounded advisory prefix requests. Origin is a routing/lineage
/// check, not permission to use its old value. A stale basis may still be useful;
/// a different lineage/generation is ignored. Contents may be untrusted.
#[derive(Clone, Debug)]
pub struct TransactionHints {
    origin: SnapshotReference,
    reads: Arc<[ReadHint]>,
    retained_bytes: u64,
}
impl TransactionHints {
    pub fn origin(&self) -> &SnapshotReference {
        &self.origin
    }
    pub fn reads(&self) -> &[ReadHint] {
        &self.reads
    }
    pub fn retained_bytes(&self) -> u64 {
        self.retained_bytes
    }

    /// Explicit authoring also serves the future versioned transport decoder.
    /// Admission limits truncate advisory data; invalid prefixes reject the hint
    /// object, never alter a transaction. Server prefetch has independent limits.
    pub fn from_reads(
        origin: SnapshotReference,
        reads: impl IntoIterator<Item = ReadHint>,
        limits: HintLimits,
    ) -> Result<Self, SemanticError> {
        let limits = limits.bounded();
        if !origin_fits(&origin) {
            return Err(SemanticError::incorrect(
                "hints/origin-capacity",
                "hint origin exceeds its independent retention limit",
            ));
        }
        let mut retained = BTreeSet::new();
        let mut bytes = 0u64;
        for read in reads.into_iter().take(limits.max_prefixes) {
            read.prefix.validate()?;
            let next = bytes.saturating_add(read.weight());
            if next > limits.max_bytes {
                break;
            }
            if retained.contains(&read) {
                continue;
            }
            bytes = next;
            retained.insert(read);
        }
        Ok(Self {
            origin,
            reads: retained.into_iter().collect::<Vec<_>>().into(),
            retained_bytes: bytes,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HintTraceStats {
    pub prefix_requests: u64,
    pub admitted_prefixes: usize,
    pub retained_bytes: u64,
    pub omitted_requests: u64,
    /// Broad scans/boundary seeks are intentionally not expanded into unbounded
    /// work. Hints can be incomplete even when all recorded prefixes fit.
    pub broad_reads: u64,
    pub elapsed_nanos: u64,
}
pub struct HintedSpeculation {
    pub report: SpeculativeTransactionReport,
    /// Eager/speculative/opaque-filter origins have no portable committed origin.
    /// Their with result is still returned; hints are simply unavailable.
    pub hints: Option<TransactionHints>,
    pub stats: HintTraceStats,
}
struct TraceState {
    limits: HintLimits,
    reads: BTreeSet<ReadHint>,
    stats: HintTraceStats,
}
thread_local! { static TRACE: RefCell<Option<Rc<RefCell<TraceState>>>> = const { RefCell::new(None) }; }
struct TraceGuard(Option<Rc<RefCell<TraceState>>>);
impl Drop for TraceGuard {
    fn drop(&mut self) {
        TRACE.with(|trace| {
            *trace.borrow_mut() = self.0.take();
        });
    }
}
pub(crate) fn record_prefix(history: bool, prefix: &IndexPrefix) {
    let trace = TRACE.with(|trace| trace.borrow().clone());
    if let Some(trace) = trace {
        let mut trace = trace.borrow_mut();
        trace.stats.prefix_requests += 1;
        // Bound before cloning potentially large values.
        let bytes = prefix_weight(prefix);
        if bytes > trace.limits.max_bytes || trace.reads.len() >= trace.limits.max_prefixes {
            trace.stats.omitted_requests += 1;
            return;
        }
        if trace.stats.retained_bytes.saturating_add(bytes) > trace.limits.max_bytes {
            trace.stats.omitted_requests += 1;
            return;
        }
        let read = ReadHint {
            history,
            prefix: prefix.clone(),
        };
        if trace.reads.contains(&read) {
            return;
        }
        trace.stats.retained_bytes += bytes;
        trace.reads.insert(read);
        trace.stats.admitted_prefixes = trace.reads.len();
    }
}
pub(crate) fn record_broad_read() {
    TRACE.with(|trace| {
        if let Some(trace) = trace.borrow().as_ref() {
            trace.borrow_mut().stats.broad_reads += 1;
        }
    });
}
impl DatabaseValue {
    /// Run ordinary pure with while recording bounded advisory reads. No data
    /// is written and all ordinary speculation limits/program controls apply.
    pub fn with_forms_with_hints(
        &self,
        forms: &[TxForm],
        tx_instant: i64,
        source: EntityRef,
        speculation: SpeculationLimits,
        hint_limits: HintLimits,
    ) -> Result<HintedSpeculation, SemanticError> {
        let _phase = OperationContext::current_or_process().phase(OperationKind::HintGeneration);
        let started = Instant::now();
        let origin = self.snapshot_reference().ok().filter(origin_fits);
        let trace = Rc::new(RefCell::new(TraceState {
            limits: hint_limits.bounded(),
            reads: BTreeSet::new(),
            stats: HintTraceStats::default(),
        }));
        let _guard = TraceGuard(TRACE.with(|active| active.replace(Some(Rc::clone(&trace)))));
        let report = self.with_forms_with_limits(forms, tx_instant, source, speculation)?;
        let mut trace = trace.borrow_mut();
        trace.stats.elapsed_nanos = nanos(started.elapsed());
        let hints = origin.map(|origin| TransactionHints {
            origin,
            reads: std::mem::take(&mut trace.reads)
                .into_iter()
                .collect::<Vec<_>>()
                .into(),
            retained_bytes: trace.stats.retained_bytes,
        });
        Ok(HintedSpeculation {
            report,
            hints,
            stats: trace.stats,
        })
    }
}

#[derive(Clone, Debug)]
pub struct HintPrefetchOptions {
    pub limits: HintLimits,
    /// At most 16,384 delivered datoms per attempt, even for larger options.
    pub max_datoms: u64,
    /// Stop between datoms after this cumulative logical read weight (clamped
    /// to 16 MiB). Each concurrent lane may deliver one in-flight datom past
    /// the remaining allowance; this is a shared budget, not multiplied by lanes.
    /// native block decoding/cache admission retain their separate limits.
    pub max_read_bytes: u64,
    /// Cooperative between-read deadline, clamped to one second. This is not
    /// an interruption or wall-clock bound for a synchronous PostgreSQL read.
    pub timeout: Duration,
    /// Cancels prefetch only, never the submitted transaction.
    pub cancel: Arc<AtomicBool>,
}
impl HintPrefetchOptions {
    fn bounded(mut self) -> Self {
        self.limits = self.limits.bounded();
        self.max_datoms = self.max_datoms.min(MAX_PREFETCH_DATOMS);
        self.max_read_bytes = self.max_read_bytes.min(MAX_PREFETCH_READ_BYTES);
        self.timeout = self.timeout.min(MAX_PREFETCH_TIME);
        self
    }
}
impl Default for HintPrefetchOptions {
    fn default() -> Self {
        Self {
            limits: HintLimits::default(),
            max_datoms: 1024,
            max_read_bytes: 4 * 1024 * 1024,
            timeout: Duration::from_millis(20),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HintPrefetchStats {
    pub attempts: u64,
    pub prefixes: u64,
    pub datoms: u64,
    pub retained_read_bytes: u64,
    /// Largest delivered datom, including its recursively owned value bytes.
    /// This exposes the one-item overshoot of the cooperative byte budget.
    pub max_inflight_datom_bytes: u64,
    pub errors: u64,
    pub ignored_origin: bool,
    pub canceled_or_limited: bool,
    pub spawn_failed: bool,
    /// Active asynchronous workers. An acknowledgement does not imply zero.
    pub active_workers: u64,
    pub peak_active_workers: u64,
    pub completed_workers: u64,
    pub skipped_busy: u64,
    pub queue_nanos: u64,
    pub prefetch_nanos: u64,
    pub processing_nanos: u64,
    pub overlap_nanos: u64,
    /// Always zero: authority completion never joins the optional worker.
    pub join_nanos: u64,
}
#[derive(Clone, Default)]
pub struct HintExecution(Arc<Mutex<HintPrefetchStats>>);
impl HintExecution {
    /// Cumulative observation, which can still change after acknowledgement
    /// while `active_workers != 0`. Waiting is a measurement/operator choice,
    /// never part of transaction completion.
    pub fn snapshot(&self) -> HintPrefetchStats {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn update(&self, action: impl FnOnce(&mut HintPrefetchStats)) {
        action(
            &mut self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
    }
}

#[derive(Clone)]
pub(crate) struct HintWorkerSlot {
    active: Arc<AtomicUsize>,
    maximum: usize,
}
impl Default for HintWorkerSlot {
    fn default() -> Self {
        Self::new(HintPrefetchConcurrency::default()).expect("valid default")
    }
}
impl HintWorkerSlot {
    pub(crate) fn new(config: HintPrefetchConcurrency) -> Result<Self, SemanticError> {
        let config = config.validate()?;
        Ok(Self {
            active: Arc::new(AtomicUsize::new(0)),
            maximum: config.max_workers,
        })
    }
}

struct WorkerPermit(HintWorkerSlot);
impl WorkerPermit {
    fn acquire(slot: &HintWorkerSlot) -> Option<Self> {
        #[allow(deprecated, reason = "Atomic::try_update is unavailable on Rust 1.90")]
        slot.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < slot.maximum).then_some(active + 1)
            })
            .ok()?;
        #[allow(deprecated, reason = "Atomic::try_update is unavailable on Rust 1.90")]
        if ACTIVE_PREFETCH_WORKERS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAX_ACTIVE_PREFETCH_WORKERS).then_some(active + 1)
            })
            .is_err()
        {
            slot.active.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(Self(slot.clone()))
    }
}
impl Drop for WorkerPermit {
    fn drop(&mut self) {
        ACTIVE_PREFETCH_WORKERS.fetch_sub(1, Ordering::AcqRel);
        self.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}

struct HintJob {
    execution: HintExecution,
    stop: AtomicBool,
    started: Instant,
    processing_end: Mutex<Option<Instant>>,
    next_prefix: AtomicUsize,
    reserved_datoms: AtomicU64,
    read_bytes: AtomicU64,
}
struct FinishAuthority(Arc<HintJob>);
impl Drop for FinishAuthority {
    fn drop(&mut self) {
        let end = Instant::now();
        *self
            .0
            .processing_end
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(end);
        self.0.stop.store(true, Ordering::Release);
        self.0
            .execution
            .update(|stats| stats.processing_nanos += nanos(end.duration_since(self.0.started)));
    }
}
pub(crate) struct PendingHints {
    pub(crate) hints: TransactionHints,
    pub(crate) options: HintPrefetchOptions,
    pub(crate) execution: HintExecution,
    pub(crate) submitted: Instant,
}
impl PendingHints {
    pub(crate) fn new(
        hints: TransactionHints,
        options: HintPrefetchOptions,
        execution: HintExecution,
    ) -> Self {
        let options = options.bounded();
        // Admission owns only its allowed prefix of the advisory object. The
        // canonical request and its admission limits are entirely unchanged.
        let mut reads = Vec::new();
        let mut retained_bytes = 0u64;
        for read in hints.reads.iter().take(options.limits.max_prefixes) {
            let next = retained_bytes.saturating_add(read.weight());
            if next > options.limits.max_bytes {
                break;
            }
            retained_bytes = next;
            reads.push(read.clone());
        }
        let hints = TransactionHints {
            origin: hints.origin,
            reads: reads.into(),
            retained_bytes,
        };
        Self {
            hints,
            options,
            execution,
            submitted: Instant::now(),
        }
    }
}

/// Configured workers per writer and at most eight per process. Synchronous driver
/// calls cannot be interrupted safely: stuck workers retain their permits, so
/// dropping/restarting services cannot accumulate unbounded threads or connections.
/// Authority completion cancels further work but never waits for it. The worker
/// creates independent SQL/cache-miss lanes; only authenticated node cache is shared.
pub(crate) fn overlap<T>(
    database: Option<DatabaseValue>,
    hints: Option<&PendingHints>,
    slot: &HintWorkerSlot,
    action: impl FnOnce() -> T,
) -> T {
    let Some(hints) = hints else {
        drop(database);
        return action();
    };
    let started = Instant::now();
    hints.execution.update(|stats| {
        stats.attempts += 1;
        stats.queue_nanos = nanos(hints.submitted.elapsed());
    });
    let job = Arc::new(HintJob {
        execution: hints.execution.clone(),
        stop: AtomicBool::new(false),
        started,
        processing_end: Mutex::new(None),
        next_prefix: AtomicUsize::new(0),
        reserved_datoms: AtomicU64::new(0),
        read_bytes: AtomicU64::new(0),
    });
    let finish = FinishAuthority(job.clone());
    let plan = database
        .as_ref()
        .map(DatabaseValue::hint_prefetch_plan)
        .transpose();
    // No original reader connection is moved into the detached worker. Releasing
    // this local source happens while authority still owns its current value.
    drop(database);
    if let Ok(Some(plan)) = plan {
        if hints.options.cancel.load(Ordering::Acquire)
            || hints.options.timeout.is_zero()
            || hints.options.max_datoms == 0
            || hints.options.max_read_bytes == 0
            || hints.hints.reads.is_empty()
            || slot.maximum == 0
        {
            job.execution
                .update(|stats| stats.canceled_or_limited = true);
        } else {
            for _ in 0..slot.maximum.min(hints.hints.reads.len()) {
                let Some(permit) = WorkerPermit::acquire(slot) else {
                    job.execution.update(|stats| stats.skipped_busy += 1);
                    break;
                };
                let operation = OperationContext::current_or_process();
                let reads = hints.hints.clone();
                let options = hints.options.clone();
                let plan = plan.clone();
                let worker_job = job.clone();
                job.execution.update(|stats| {
                    stats.active_workers += 1;
                    stats.peak_active_workers = stats.peak_active_workers.max(stats.active_workers);
                });
                let spawned = std::thread::Builder::new()
                    .name("edb-hint-prefetch".into())
                    .spawn(move || {
                        let phase = operation.phase(OperationKind::HintPrefetch);
                        let begin = Instant::now();
                        let measured =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                if worker_job.stop.load(Ordering::Acquire)
                                    || options.cancel.load(Ordering::Acquire)
                                {
                                    return HintPrefetchStats {
                                        canceled_or_limited: true,
                                        ..Default::default()
                                    };
                                }
                                let independent = plan.open(options.timeout);
                                match independent {
                                    Ok(independent) => {
                                        prefetch(&independent, &reads, &options, &worker_job)
                                    }
                                    Err(_) => HintPrefetchStats {
                                        errors: 1,
                                        ..Default::default()
                                    },
                                }
                            }))
                            .unwrap_or_else(|_| HintPrefetchStats {
                                errors: 1,
                                ..Default::default()
                            });
                        let end = Instant::now();
                        let processing_end = worker_job
                            .processing_end
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .unwrap_or(end);
                        drop(phase);
                        drop(permit);
                        worker_job.execution.update(|stats| {
                            stats.active_workers -= 1;
                            stats.completed_workers += 1;
                            stats.prefixes += measured.prefixes;
                            stats.datoms += measured.datoms;
                            stats.retained_read_bytes += measured.retained_read_bytes;
                            stats.max_inflight_datom_bytes = stats
                                .max_inflight_datom_bytes
                                .max(measured.max_inflight_datom_bytes);
                            stats.errors += measured.errors;
                            stats.ignored_origin |= measured.ignored_origin;
                            stats.canceled_or_limited |= measured.canceled_or_limited;
                            stats.prefetch_nanos += nanos(end.duration_since(begin));
                            stats.overlap_nanos +=
                                nanos(processing_end.min(end).saturating_duration_since(begin));
                        });
                    });
                if spawned.is_err() {
                    job.execution.update(|stats| {
                        stats.active_workers -= 1;
                        stats.spawn_failed = true;
                    });
                }
            }
        }
    } else {
        job.execution.update(|stats| stats.ignored_origin = true);
    }
    let result = action();
    drop(finish);
    result
}

fn prefetch(
    database: &DatabaseValue,
    hints: &TransactionHints,
    options: &HintPrefetchOptions,
    job: &HintJob,
) -> HintPrefetchStats {
    let mut stats = HintPrefetchStats::default();
    let Ok(key) = database.snapshot_key() else {
        stats.ignored_origin = true;
        return stats;
    };
    if key.lineage_id() != hints.origin.key().lineage_id()
        || key.generation() != hints.origin.key().generation()
    {
        stats.ignored_origin = true;
        return stats;
    }
    loop {
        let index = job.next_prefix.fetch_add(1, Ordering::Relaxed);
        let Some(read) = hints.reads.get(index) else {
            break;
        };
        let stopped = || {
            job.stop.load(Ordering::Acquire)
                || options.cancel.load(Ordering::Acquire)
                || job.started.elapsed() >= options.timeout
        };
        if stopped()
            || job.reserved_datoms.load(Ordering::Acquire) >= options.max_datoms
            || job.read_bytes.load(Ordering::Acquire) >= options.max_read_bytes
        {
            stats.canceled_or_limited = true;
            break;
        }
        let view = if read.history {
            database.clone().history()
        } else {
            database.clone()
        };
        let Ok(mut cursor) = view.prefix_cursor(&read.prefix) else {
            stats.errors += 1;
            continue;
        };
        stats.prefixes += 1;
        loop {
            if stopped() || job.read_bytes.load(Ordering::Acquire) >= options.max_read_bytes {
                stats.canceled_or_limited = true;
                return stats;
            }
            // Reserve before asking the cursor for another datom, so even
            // concurrent reads never multiply the operation-wide count cap.
            #[allow(deprecated, reason = "Atomic::try_update is unavailable on Rust 1.90")]
            if job
                .reserved_datoms
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                    (count < options.max_datoms).then_some(count + 1)
                })
                .is_err()
            {
                stats.canceled_or_limited = true;
                return stats;
            }
            let Some(datum) = cursor.next() else {
                job.reserved_datoms.fetch_sub(1, Ordering::AcqRel);
                break;
            };
            match datum {
                Ok(datom) => {
                    stats.datoms += 1;
                    let bytes = datom.retained_bytes();
                    stats.max_inflight_datom_bytes = stats.max_inflight_datom_bytes.max(bytes);
                    stats.retained_read_bytes = stats.retained_read_bytes.saturating_add(bytes);
                    #[allow(deprecated, reason = "Atomic::try_update is unavailable on Rust 1.90")]
                    let _ = job.read_bytes.fetch_update(
                        Ordering::AcqRel,
                        Ordering::Acquire,
                        |current| Some(current.saturating_add(bytes)),
                    );
                }
                Err(_) => {
                    job.reserved_datoms.fetch_sub(1, Ordering::AcqRel);
                    stats.errors += 1;
                    break;
                }
            }
        }
    }
    stats
}
fn nanos(duration: Duration) -> u64 {
    duration.as_nanos().min(u64::MAX as u128) as u64
}
