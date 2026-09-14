//! Attribution at the PostgreSQL driver-call boundary.
//!
//! Counts are driver API calls, not SQL statements, packets, or protocol round
//! trips. Streaming consumption is measured separately. No SQL text, parameter
//! values, connection strings, or PostgreSQL error messages enter these reports.
use postgres::fallible_iterator::FallibleIterator;
use postgres::types::{BorrowToSql, FromSql, ToSql, Type};
use postgres::{Error, IsolationLevel, Row, SimpleQueryMessage, Statement, ToStatement};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum OperationKind {
    Unscoped,
    Application,
    Query,
    Transaction,
    TransactionExpansion,
    TransactionAssessment,
    TransactionEncoding,
    TransactionCommit,
    TransactionReport,
    Indexing,
    NodeCompression,
    HintGeneration,
    HintPrefetch,
    Recovery,
    PinMaintenance,
    PeerObservation,
    WriterMaintenance,
    Administration,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SqlCallKind {
    Connect,
    Query,
    QueryOne,
    QueryOpt,
    QueryRaw,
    Execute,
    Prepare,
    SimpleQuery,
    BatchExecute,
    Begin,
    Commit,
    Rollback,
    Close,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SqlCallStats {
    pub calls: u64,
    pub completed_calls: u64,
    pub errors: u64,
    pub elapsed_nanos: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OperationPhaseStats {
    pub invocations: u64,
    /// Sum of phase wall durations, including CPU and waits; overlapping or
    /// nested phases are not a disjoint decomposition of elapsed wall time.
    pub elapsed_nanos: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SqlIoStats {
    /// Opt-in operation-local cache/index observations; empty when disabled.
    pub reads: crate::ReadIoStats,
    /// Actual semantic work, not replayed receipt contents or durable I/O.
    pub transaction: crate::TransactionWorkStats,
    /// All started driver calls, including connection and control calls.
    pub calls: u64,
    pub completed_calls: u64,
    /// Calls other than Connect and Close, including transaction control.
    pub sql_calls: u64,
    pub connect_calls: u64,
    /// Begin, commit, rollback and explicit close; a subset of `calls`.
    pub control_calls: u64,
    /// Failed calls and stream-consumption errors (not just decoded rows).
    pub errors: u64,
    /// Sum of call durations; concurrent time is additive, not wall time.
    pub elapsed_nanos: u64,
    pub rows: u64,
    /// Binary field bytes returned in ordinary/streamed Rows. This excludes
    /// protocol framing and is not a disk, network or heap allocation measure.
    pub result_cell_bytes: u64,
    /// Explicitly known application payload sizes, separate from SQL cells.
    pub known_payload_read_bytes: u64,
    pub known_payload_write_bytes: u64,
    /// Actual `RowIter::next` calls, not new SQL requests.
    pub stream_polls: u64,
    pub stream_errors: u64,
    pub stream_elapsed_nanos: u64,
    pub abandoned_streams: u64,
    pub by_kind: BTreeMap<SqlCallKind, SqlCallStats>,
    pub by_operation: BTreeMap<OperationKind, SqlCallStats>,
    pub phases: BTreeMap<OperationKind, OperationPhaseStats>,
}

#[derive(Clone, Debug)]
pub struct SqlIoReport {
    pub kind: OperationKind,
    pub context: Option<crate::Keyword>,
    /// Inclusive statistics: each physical call contributes once to its
    /// operation and once to each ancestor, never to a sibling.
    pub stats: SqlIoStats,
    pub operation_elapsed_nanos: u64,
    /// Inclusive contributions grouped by each distinct named descendant.
    /// Groups may nest/overlap; do not sum them to reconstruct the total.
    pub nested: BTreeMap<crate::Keyword, SqlIoStats>,
    /// At most128 distinct nested business labels are retained per context.
    /// Overflow drops detail only; total I/O and results are unaffected.
    pub nested_truncated: bool,
}

pub type SqlMetricCallback = Arc<dyn Fn(SqlIoReport) + Send + Sync>;

struct OperationInner {
    kind: OperationKind,
    name: Option<crate::Keyword>,
    diagnostics: bool,
    started: Instant,
    parent: Option<OperationContext>,
    stats: Mutex<SqlIoStats>,
    nested: Mutex<(BTreeMap<crate::Keyword, SqlIoStats>, bool)>,
    callback: Option<SqlMetricCallback>,
}

/// Clonable attribution shared explicitly across tasks and lazy cursors.
/// It never grants storage authority or changes database-value identity.
#[derive(Clone)]
pub struct OperationContext(Arc<OperationInner>);

impl fmt::Debug for OperationContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OperationContext")
            .field("kind", &self.0.kind)
            .finish_non_exhaustive()
    }
}

struct ActiveScopes {
    next_id: u64,
    entries: Vec<(u64, OperationContext)>,
}

thread_local! {
    static ACTIVE: RefCell<ActiveScopes> = const { RefCell::new(ActiveScopes { next_id: 0, entries: Vec::new() }) };
}

fn process_context() -> OperationContext {
    static PROCESS: OnceLock<OperationContext> = OnceLock::new();
    PROCESS
        .get_or_init(|| OperationContext::new(OperationKind::Unscoped))
        .clone()
}

/// Process totals include unscoped/background calls through observed clients.
/// Concurrent snapshots are cumulative observations, not operation deltas.
pub fn process_sql_stats() -> SqlIoStats {
    process_context().snapshot()
}

fn io_context() -> OperationContext {
    OperationContext::current().unwrap_or_else(process_context)
}

/// A scope is thread-bound. Its drop restores attribution but never invokes
/// arbitrary callbacks while a database/cache lock might still be held.
pub struct OperationScope {
    identity: u64,
    _not_send: PhantomData<Rc<()>>,
}

/// Scoped phase timing is separate from SQL time and never calls user code.
pub struct PhaseScope {
    context: OperationContext,
    scope: Option<OperationScope>,
    started: Instant,
}

impl PhaseScope {
    pub fn context(&self) -> OperationContext {
        self.context.clone()
    }
}

impl Drop for PhaseScope {
    fn drop(&mut self) {
        let elapsed = nanos(self.started.elapsed());
        self.scope.take();
        self.context.update(|stats| {
            let phase = stats.phases.entry(self.context.0.kind).or_default();
            phase.invocations = phase.invocations.saturating_add(1);
            phase.elapsed_nanos = phase.elapsed_nanos.saturating_add(elapsed);
        });
    }
}

impl OperationContext {
    pub fn new(kind: OperationKind) -> Self {
        Self::create(kind, None, None)
    }

    pub fn with_callback(kind: OperationKind, callback: SqlMetricCallback) -> Self {
        Self::create(kind, None, Some(callback))
    }

    fn create(
        kind: OperationKind,
        parent: Option<Self>,
        callback: Option<SqlMetricCallback>,
    ) -> Self {
        let diagnostics = parent.as_ref().is_some_and(Self::diagnostics_enabled);
        Self::create_observed(kind, parent, callback, None, diagnostics)
    }

    fn create_observed(
        kind: OperationKind,
        parent: Option<Self>,
        callback: Option<SqlMetricCallback>,
        name: Option<crate::Keyword>,
        diagnostics: bool,
    ) -> Self {
        Self(Arc::new(OperationInner {
            kind,
            name,
            diagnostics,
            started: Instant::now(),
            parent,
            stats: Mutex::new(SqlIoStats::default()),
            nested: Mutex::new((BTreeMap::new(), false)),
            callback,
        }))
    }

    pub fn child(&self, kind: OperationKind) -> Self {
        Self::create(kind, Some(self.clone()), self.0.callback.clone())
    }

    pub fn child_diagnostic(&self, kind: OperationKind) -> Self {
        Self::create_observed(
            kind,
            Some(self.clone()),
            self.0.callback.clone(),
            None,
            true,
        )
    }

    /// Enable detailed native read/semantic counters for this operation.
    /// Existing `new` contexts retain their inexpensive SQL-only behavior.
    pub fn diagnostic(kind: OperationKind) -> Self {
        Self::create_observed(kind, None, None, None, true)
    }

    /// Business names are intentional diagnostic labels, not subject data.
    pub fn named(kind: OperationKind, name: crate::Keyword) -> Result<Self, crate::SemanticError> {
        validate_context_name(&name)?;
        Ok(Self::create_observed(kind, None, None, Some(name), true))
    }

    pub fn child_named(
        &self,
        kind: OperationKind,
        name: crate::Keyword,
    ) -> Result<Self, crate::SemanticError> {
        validate_context_name(&name)?;
        Ok(Self::create_observed(
            kind,
            Some(self.clone()),
            self.0.callback.clone(),
            Some(name),
            true,
        ))
    }

    pub fn diagnostics_enabled(&self) -> bool {
        self.0.diagnostics
    }

    pub fn report(&self) -> SqlIoReport {
        // Phases and queued submissions are unnamed children of their caller.
        // Preserve the effective business label without changing aggregation.
        let mut effective = Some(self);
        let mut name = None;
        while let Some(context) = effective {
            if context.0.name.is_some() {
                name = context.0.name.clone();
                break;
            }
            effective = context.0.parent.as_ref();
        }
        let (nested, truncated) = &*self
            .0
            .nested
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        SqlIoReport {
            kind: self.0.kind,
            context: name,
            stats: self.snapshot(),
            operation_elapsed_nanos: nanos(self.0.started.elapsed()),
            nested: nested.clone(),
            nested_truncated: *truncated,
        }
    }

    /// Run any native API against one explicitly scoped context. The result
    /// (including errors) is unchanged; take `report()` after lazy consumption.
    pub fn measure<T>(&self, operation: impl FnOnce() -> T) -> (T, SqlIoReport) {
        let result = {
            let _scope = self.enter();
            operation()
        };
        (result, self.report())
    }

    pub(crate) fn record_transaction_work(
        &self,
        mut record: impl FnMut(&mut crate::TransactionWorkStats),
    ) {
        if self.diagnostics_enabled() {
            self.update(|stats| record(&mut stats.transaction));
        }
    }

    pub fn current() -> Option<Self> {
        ACTIVE.with(|active| {
            active
                .borrow()
                .entries
                .last()
                .map(|(_, context)| context.clone())
        })
    }

    pub fn current_or_process() -> Self {
        io_context()
    }

    pub fn phase(&self, kind: OperationKind) -> PhaseScope {
        let context = self.child(kind);
        let started = Instant::now();
        let scope = Some(context.enter());
        PhaseScope {
            context,
            scope,
            started,
        }
    }

    pub fn enter(&self) -> OperationScope {
        let identity = ACTIVE.with(|active| {
            let mut active = active.borrow_mut();
            let identity = loop {
                active.next_id = active.next_id.wrapping_add(1);
                if !active.entries.iter().any(|(id, _)| *id == active.next_id) {
                    break active.next_id;
                }
            };
            active.entries.push((identity, self.clone()));
            identity
        });
        OperationScope {
            identity,
            _not_send: PhantomData,
        }
    }

    pub fn snapshot(&self) -> SqlIoStats {
        self.0
            .stats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Explicitly deliver a snapshot after releasing application/storage locks.
    /// No wrapper or scope-drop calls this implicitly. Callback panic is
    /// contained; the boolean reports delivery success without changing I/O.
    pub fn publish(&self) -> bool {
        let Some(callback) = &self.0.callback else {
            return true;
        };
        let report = self.report();
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(report))).is_ok()
    }

    pub fn record_payload_read(&self, bytes: u64) {
        self.update(|stats| {
            stats.known_payload_read_bytes = stats.known_payload_read_bytes.saturating_add(bytes)
        });
    }

    pub fn record_payload_write(&self, bytes: u64) {
        self.update(|stats| {
            stats.known_payload_write_bytes = stats.known_payload_write_bytes.saturating_add(bytes)
        });
    }

    pub(crate) fn update(&self, mut update: impl FnMut(&mut SqlIoStats)) {
        let process = process_context();
        let mut counted_process = false;
        let mut current = Some(self.clone());
        let mut labels: Vec<crate::Keyword> = Vec::new();
        while let Some(context) = current {
            counted_process |= Arc::ptr_eq(&context.0, &process.0);
            {
                let mut stats = context
                    .0
                    .stats
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                update(&mut stats);
            }
            if !labels.is_empty() {
                let mut grouped = context
                    .0
                    .nested
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                for name in &labels {
                    if grouped.0.len() < 128 || grouped.0.contains_key(name) {
                        update(grouped.0.entry(name.clone()).or_default());
                    } else {
                        grouped.1 = true;
                    }
                }
            }
            if let Some(name) = &context.0.name
                && !labels.contains(name)
            {
                labels.push(name.clone());
            }
            current = context.0.parent.clone();
        }
        if !counted_process {
            update(
                &mut process
                    .0
                    .stats
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
        }
    }

    fn begin_call(&self, kind: SqlCallKind) {
        self.update(|stats| {
            stats.calls = stats.calls.saturating_add(1);
            stats.sql_calls = stats.sql_calls.saturating_add(u64::from(!matches!(
                kind,
                SqlCallKind::Connect | SqlCallKind::Close
            )));
            stats.connect_calls = stats
                .connect_calls
                .saturating_add(u64::from(kind == SqlCallKind::Connect));
            stats.control_calls = stats.control_calls.saturating_add(u64::from(matches!(
                kind,
                SqlCallKind::Begin
                    | SqlCallKind::Commit
                    | SqlCallKind::Rollback
                    | SqlCallKind::Close
            )));
            let entry = stats.by_kind.entry(kind).or_default();
            entry.calls = entry.calls.saturating_add(1);
            let entry = stats.by_operation.entry(self.0.kind).or_default();
            entry.calls = entry.calls.saturating_add(1);
        });
    }

    fn finish_call(&self, kind: SqlCallKind, elapsed: u64, error: bool) {
        self.update(|stats| {
            stats.completed_calls = stats.completed_calls.saturating_add(1);
            stats.errors = stats.errors.saturating_add(u64::from(error));
            stats.elapsed_nanos = stats.elapsed_nanos.saturating_add(elapsed);
            let entry = stats.by_kind.entry(kind).or_default();
            entry.completed_calls = entry.completed_calls.saturating_add(1);
            entry.errors = entry.errors.saturating_add(u64::from(error));
            entry.elapsed_nanos = entry.elapsed_nanos.saturating_add(elapsed);
            let entry = stats.by_operation.entry(self.0.kind).or_default();
            entry.completed_calls = entry.completed_calls.saturating_add(1);
            entry.errors = entry.errors.saturating_add(u64::from(error));
            entry.elapsed_nanos = entry.elapsed_nanos.saturating_add(elapsed);
        });
    }

    fn record_rows<'a>(&self, rows: impl IntoIterator<Item = &'a Row>) {
        let mut count = 0_u64;
        let mut bytes = 0_u64;
        for row in rows {
            count = count.saturating_add(1);
            for index in 0..row.len() {
                // Length-only FromSql accepts every type and never copies or
                // formats data. SQL NULL contributes zero payload bytes.
                if let Ok(Some(length)) = row.try_get::<_, Option<FieldLength>>(index) {
                    bytes = bytes.saturating_add(length.0);
                }
            }
        }
        self.update(|stats| {
            stats.rows = stats.rows.saturating_add(count);
            stats.result_cell_bytes = stats.result_cell_bytes.saturating_add(bytes);
        });
    }
}

fn validate_context_name(name: &crate::Keyword) -> Result<(), crate::SemanticError> {
    if name.namespace.as_ref().is_none_or(String::is_empty)
        || name.name.is_empty()
        || name
            .namespace
            .as_ref()
            .map_or(0, String::len)
            .saturating_add(name.name.len())
            > 256
    {
        return Err(crate::SemanticError::incorrect(
            "diagnostics/context-name",
            "I/O context requires a qualified nonempty keyword of at most 256 bytes",
        ));
    }
    Ok(())
}

pub(crate) fn record_current_transaction_work(
    record: impl FnMut(&mut crate::TransactionWorkStats),
) {
    if let Some(context) = OperationContext::current() {
        context.record_transaction_work(record);
    }
}

impl Drop for OperationScope {
    fn drop(&mut self) {
        ACTIVE.with(|active| {
            let mut active = active.borrow_mut();
            // Removing the matching scope also tolerates explicit out-of-order
            // drops without attributing later operations to a departed scope.
            if let Some(index) = active
                .entries
                .iter()
                .rposition(|(identity, _)| *identity == self.identity)
            {
                active.entries.remove(index);
            }
        });
    }
}

struct FieldLength(u64);
impl<'a> FromSql<'a> for FieldLength {
    fn from_sql(_: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        Ok(Self(raw.len() as u64))
    }
    fn accepts(_: &Type) -> bool {
        true
    }
}

fn nanos(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn observed<T, E>(
    context: Option<&OperationContext>,
    kind: SqlCallKind,
    call: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    if let Some(context) = context {
        context.begin_call(kind);
    }
    let started = Instant::now();
    let result = call();
    if let Some(context) = context {
        context.finish_call(kind, nanos(started.elapsed()), result.is_err());
    }
    result
}

/// Owned PostgreSQL connection used internally. There is deliberately no
/// Deref or raw-client accessor that could bypass instrumentation.
pub(crate) fn observe_driver_call<T, E>(
    kind: SqlCallKind,
    call: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    observed(Some(&io_context()), kind, call)
}

/// Instrumented synchronous client; asynchronous notice setup uses the same
/// narrow accounting boundary above, but notice waiting itself sends no SQL.
pub struct SqlClient {
    inner: postgres::Client,
}

impl SqlClient {
    /// Adapt an existing public raw client. Its earlier connection handshake
    /// cannot be measured retroactively; use `connect_with` for production.
    pub fn from_raw(inner: postgres::Client) -> Self {
        Self { inner }
    }

    pub fn connect_with<E>(
        connect: impl FnOnce() -> Result<postgres::Client, E>,
    ) -> Result<Self, E> {
        observed(Some(&io_context()), SqlCallKind::Connect, connect).map(Self::from_raw)
    }

    pub fn connect<T>(parameters: &str, tls: T) -> Result<Self, Error>
    where
        T: postgres::tls::MakeTlsConnect<postgres::Socket> + 'static + Send,
        T::TlsConnect: Send,
        T::Stream: Send,
        <T::TlsConnect as postgres::tls::TlsConnect<postgres::Socket>>::Future: Send,
    {
        Self::connect_with(|| postgres::Client::connect(parameters, tls))
    }

    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }

    pub fn close(self) -> Result<(), Error> {
        observed(Some(&io_context()), SqlCallKind::Close, || {
            self.inner.close()
        })
    }

    pub fn build_transaction(&mut self) -> SqlTransactionBuilder<'_> {
        SqlTransactionBuilder {
            inner: self.inner.build_transaction(),
        }
    }
}

pub struct SqlTransaction<'a> {
    inner: Option<postgres::Transaction<'a>>,
    context: Option<OperationContext>,
}

impl<'a> SqlTransaction<'a> {
    fn from_raw(inner: postgres::Transaction<'a>, context: Option<OperationContext>) -> Self {
        Self {
            inner: Some(inner),
            context,
        }
    }

    fn context(&self) -> Option<OperationContext> {
        OperationContext::current()
            .or_else(|| self.context.clone())
            .or_else(|| Some(process_context()))
    }

    pub fn commit(mut self) -> Result<(), Error> {
        let context = self.context();
        observed(context.as_ref(), SqlCallKind::Commit, || {
            self.inner.take().expect("live transaction").commit()
        })
    }

    pub fn rollback(mut self) -> Result<(), Error> {
        let context = self.context();
        observed(context.as_ref(), SqlCallKind::Rollback, || {
            self.inner.take().expect("live transaction").rollback()
        })
    }
}

impl Drop for SqlTransaction<'_> {
    fn drop(&mut self) {
        if let Some(transaction) = self.inner.take() {
            // Match postgres::Transaction's blocking drop rollback, but retain
            // the attempt/error evidence. Taking it prevents double rollback.
            let _ = observed(self.context().as_ref(), SqlCallKind::Rollback, || {
                transaction.rollback()
            });
        }
    }
}

pub struct SqlTransactionBuilder<'a> {
    inner: postgres::TransactionBuilder<'a>,
}

impl<'a> SqlTransactionBuilder<'a> {
    pub fn isolation_level(mut self, level: IsolationLevel) -> Self {
        self.inner = self.inner.isolation_level(level);
        self
    }
    pub fn read_only(mut self, read_only: bool) -> Self {
        self.inner = self.inner.read_only(read_only);
        self
    }
    pub fn deferrable(mut self, deferrable: bool) -> Self {
        self.inner = self.inner.deferrable(deferrable);
        self
    }
    pub fn start(self) -> Result<SqlTransaction<'a>, Error> {
        let context = Some(io_context());
        observed(context.as_ref(), SqlCallKind::Begin, || self.inner.start())
            .map(|transaction| SqlTransaction::from_raw(transaction, context))
    }
}

pub struct SqlRowIter<'a> {
    inner: postgres::RowIter<'a>,
    context: Option<OperationContext>,
    completed: bool,
}

impl SqlRowIter<'_> {
    pub fn rows_affected(&self) -> Option<u64> {
        self.inner.rows_affected()
    }
}

impl FallibleIterator for SqlRowIter<'_> {
    type Item = Row;
    type Error = Error;
    fn next(&mut self) -> Result<Option<Row>, Error> {
        let started = Instant::now();
        let result = self.inner.next();
        let elapsed = nanos(started.elapsed());
        self.completed |= matches!(&result, Ok(None) | Err(_));
        if let Some(context) = &self.context {
            context.update(|stats| {
                stats.stream_polls = stats.stream_polls.saturating_add(1);
                stats.stream_elapsed_nanos = stats.stream_elapsed_nanos.saturating_add(elapsed);
                stats.errors = stats.errors.saturating_add(u64::from(result.is_err()));
                if result.is_err() {
                    stats.stream_errors = stats.stream_errors.saturating_add(1);
                    let entry = stats.by_kind.entry(SqlCallKind::QueryRaw).or_default();
                    entry.errors = entry.errors.saturating_add(1);
                    let entry = stats.by_operation.entry(context.0.kind).or_default();
                    entry.errors = entry.errors.saturating_add(1);
                }
            });
            if let Ok(Some(row)) = &result {
                context.record_rows([row]);
            }
        }
        result
    }
}

impl Drop for SqlRowIter<'_> {
    fn drop(&mut self) {
        if !self.completed
            && let Some(context) = &self.context
        {
            context.update(|stats| {
                stats.abandoned_streams = stats.abandoned_streams.saturating_add(1)
            });
        }
    }
}

/// Local counterpart of the sealed driver trait. All repository-used methods
/// are observed, including when a generic helper receives a raw test client.
pub trait GenericClient {
    fn execute<T: ?Sized + ToStatement>(
        &mut self,
        query: &T,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<u64, Error>;
    fn query<T: ?Sized + ToStatement>(
        &mut self,
        query: &T,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Vec<Row>, Error>;
    fn query_one<T: ?Sized + ToStatement>(
        &mut self,
        query: &T,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Row, Error>;
    fn query_opt<T: ?Sized + ToStatement>(
        &mut self,
        query: &T,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Option<Row>, Error>;
    fn query_raw<T, P, I>(&mut self, query: &T, params: I) -> Result<SqlRowIter<'_>, Error>
    where
        T: ?Sized + ToStatement,
        P: BorrowToSql,
        I: IntoIterator<Item = P>,
        I::IntoIter: ExactSizeIterator;
    fn prepare(&mut self, query: &str) -> Result<Statement, Error>;
    fn simple_query(&mut self, query: &str) -> Result<Vec<SimpleQueryMessage>, Error>;
    fn batch_execute(&mut self, query: &str) -> Result<(), Error>;
    fn transaction(&mut self) -> Result<SqlTransaction<'_>, Error>;
}

// A single implementation body serves wrappers and raw fixture clients.
// Inherent methods and generic helpers share this accounting boundary;
// the owned trait records the same calls without duplicating driver behavior.
macro_rules! sql_methods {
    ($visibility:vis, $this:ident, $raw:expr, $context:expr) => {
        $visibility fn execute<T: ?Sized + ToStatement>(&mut self, query: &T, params: &[&(dyn ToSql + Sync)]) -> Result<u64, Error> {
            let $this = self; let context = $context;
            observed(context.as_ref(), SqlCallKind::Execute, || ($raw).execute(query, params))
        }
        $visibility fn query<T: ?Sized + ToStatement>(&mut self, query: &T, params: &[&(dyn ToSql + Sync)]) -> Result<Vec<Row>, Error> {
            let $this = self; let context = $context;
            let result = observed(context.as_ref(), SqlCallKind::Query, || ($raw).query(query, params));
            if let (Some(context), Ok(rows)) = (&context, &result) { context.record_rows(rows); }
            result
        }
        $visibility fn query_one<T: ?Sized + ToStatement>(&mut self, query: &T, params: &[&(dyn ToSql + Sync)]) -> Result<Row, Error> {
            let $this = self; let context = $context;
            let result = observed(context.as_ref(), SqlCallKind::QueryOne, || ($raw).query_one(query, params));
            if let (Some(context), Ok(row)) = (&context, &result) { context.record_rows([row]); }
            result
        }
        $visibility fn query_opt<T: ?Sized + ToStatement>(&mut self, query: &T, params: &[&(dyn ToSql + Sync)]) -> Result<Option<Row>, Error> {
            let $this = self; let context = $context;
            let result = observed(context.as_ref(), SqlCallKind::QueryOpt, || ($raw).query_opt(query, params));
            if let (Some(context), Ok(Some(row))) = (&context, &result) { context.record_rows([row]); }
            result
        }
        $visibility fn query_raw<T, P, I>(&mut self, query: &T, params: I) -> Result<SqlRowIter<'_>, Error>
        where T: ?Sized + ToStatement, P: BorrowToSql, I: IntoIterator<Item = P>, I::IntoIter: ExactSizeIterator {
            let $this = self; let context = $context;
            observed(context.as_ref(), SqlCallKind::QueryRaw, || ($raw).query_raw(query, params))
                .map(|inner| SqlRowIter { inner, context, completed: false })
        }
        $visibility fn prepare(&mut self, query: &str) -> Result<Statement, Error> {
            let $this = self; let context = $context;
            observed(context.as_ref(), SqlCallKind::Prepare, || ($raw).prepare(query))
        }
        $visibility fn simple_query(&mut self, query: &str) -> Result<Vec<SimpleQueryMessage>, Error> {
            let $this = self; let context = $context;
            observed(context.as_ref(), SqlCallKind::SimpleQuery, || ($raw).simple_query(query))
        }
        $visibility fn batch_execute(&mut self, query: &str) -> Result<(), Error> {
            let $this = self; let context = $context;
            observed(context.as_ref(), SqlCallKind::BatchExecute, || ($raw).batch_execute(query))
        }
        $visibility fn transaction(&mut self) -> Result<SqlTransaction<'_>, Error> {
            let $this = self; let context = $context;
            observed(context.as_ref(), SqlCallKind::Begin, || ($raw).transaction())
                .map(|inner| SqlTransaction::from_raw(inner, context))
        }
    };
}

impl SqlClient {
    sql_methods!(pub, this, &mut this.inner, Some(io_context()));
}
impl GenericClient for SqlClient {
    sql_methods!(, this, &mut this.inner, Some(io_context()));
}
impl SqlTransaction<'_> {
    sql_methods!(
        pub,
        this,
        this.inner.as_mut().expect("live transaction"),
        this.context()
    );
}
impl GenericClient for SqlTransaction<'_> {
    sql_methods!(, this, this.inner.as_mut().expect("live transaction"), this.context());
}
impl GenericClient for postgres::Client {
    sql_methods!(, this, this, Some(io_context()));
}
impl GenericClient for postgres::Transaction<'_> {
    sql_methods!(, this, this, Some(io_context()));
}
