//! Bounded, optional operational publication. No sink executes on a writer.
use crate::{DatabaseIdentity, SemanticError, SqlIoReport, SqlIoStats, TransactionClient};
use std::fmt::{self, Write as _};
use std::io::{self, Write as _};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
pub struct TelemetryConfig {
    pub enabled: bool,
    pub interval: Duration,
    /// Maximum waiting encoded events, excluding one active sink call.
    pub queue_capacity: usize,
    /// Maximum UTF-8 bytes per JSON line, including its newline.
    pub max_event_bytes: usize,
    pub warnings: TelemetryWarnings,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval: Duration::from_secs(60),
            queue_capacity: 64,
            max_event_bytes: 16 * 1024,
            warnings: TelemetryWarnings::default(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct TelemetryWarnings {
    pub writer_unavailable: bool,
    pub readiness: bool,
    pub backpressure: bool,
    /// Additionally warn while the observed queued request count meets this limit.
    pub queued_requests: Option<usize>,
    pub indexing_failure: bool,
    pub excision_paused: bool,
}

impl Default for TelemetryWarnings {
    fn default() -> Self {
        Self {
            writer_unavailable: true,
            readiness: true,
            backpressure: true,
            queued_requests: None,
            indexing_failure: true,
            excision_paused: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TelemetryPhase {
    Waiting,
    Activating,
    Active,
    Stopping,
    Failed,
}

impl TelemetryPhase {
    fn name(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Activating => "activating",
            Self::Active => "active",
            Self::Stopping => "stopping",
            Self::Failed => "failed",
        }
    }
}

/// Local observations, not an atomic snapshot or a new lease/readiness check.
/// Capturing this value makes no SQL calls and never walks queued reports.
#[derive(Clone, Debug)]
pub struct TelemetrySnapshot {
    pub identity: Option<DatabaseIdentity>,
    pub phase: TelemetryPhase,
    pub listener_ready: bool,
    pub writer_available: bool,
    pub lease_epoch: Option<u64>,
    pub service: Option<crate::OperationalServiceStats>,
    pub indexing: Option<crate::BackgroundIndexingStats>,
    /// Process-wide driver totals, not attributed to this database alone.
    pub process_sql: SqlIoStats,
}

impl TelemetrySnapshot {
    pub fn capture(
        client: Option<&TransactionClient>,
        phase: TelemetryPhase,
        listener_ready: bool,
    ) -> Self {
        Self {
            identity: client.map(TransactionClient::identity),
            phase,
            listener_ready,
            writer_available: client.is_some_and(TransactionClient::is_available),
            lease_epoch: client.map(|client| client.transport_lease().epoch),
            service: client.map(TransactionClient::operational_stats),
            indexing: client.map(TransactionClient::operational_indexing_stats),
            process_sql: crate::process_sql_stats(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TelemetryStats {
    pub accepted: u64,
    pub delivered: u64,
    pub dropped_full: u64,
    pub dropped_oversize: u64,
    pub dropped_stopped: u64,
    pub dropped_shutdown: u64,
    pub sink_errors: u64,
    pub sink_panics: u64,
    pub bytes_delivered: u64,
}

#[derive(Default)]
struct Counters {
    accepted: AtomicU64,
    delivered: AtomicU64,
    dropped_full: AtomicU64,
    dropped_oversize: AtomicU64,
    dropped_stopped: AtomicU64,
    dropped_shutdown: AtomicU64,
    sink_errors: AtomicU64,
    sink_panics: AtomicU64,
    bytes_delivered: AtomicU64,
}

impl Counters {
    fn snapshot(&self) -> TelemetryStats {
        TelemetryStats {
            accepted: self.accepted.load(Ordering::Relaxed),
            delivered: self.delivered.load(Ordering::Relaxed),
            dropped_full: self.dropped_full.load(Ordering::Relaxed),
            dropped_oversize: self.dropped_oversize.load(Ordering::Relaxed),
            dropped_stopped: self.dropped_stopped.load(Ordering::Relaxed),
            dropped_shutdown: self.dropped_shutdown.load(Ordering::Relaxed),
            sink_errors: self.sink_errors.load(Ordering::Relaxed),
            sink_panics: self.sink_panics.load(Ordering::Relaxed),
            bytes_delivered: self.bytes_delivered.load(Ordering::Relaxed),
        }
    }
}

struct Inner {
    config: TelemetryConfig,
    sender: Option<mpsc::SyncSender<String>>,
    stop: AtomicBool,
    admission: Mutex<()>,
    sequence: AtomicU64,
    next_sample: Mutex<Option<Instant>>,
    last_pressure: Mutex<Option<(Option<DatabaseIdentity>, u64)>>,
    counters: Counters,
}

/// Clones share one bounded output queue. Encoding is performed by the caller;
/// arbitrary sink code is only invoked on the publisher's dedicated worker.
#[derive(Clone)]
pub struct TelemetryEmitter(Arc<Inner>);

impl fmt::Debug for TelemetryEmitter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TelemetryEmitter")
            .field("enabled", &self.is_enabled())
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}

/// Owns one optional sink worker. Drop requests stop without joining a callback
/// or stdout write that may block indefinitely. Use `shutdown` to wait explicitly.
pub struct TelemetryPublisher {
    emitter: TelemetryEmitter,
    finished: mpsc::Receiver<()>,
    worker: Option<JoinHandle<()>>,
}

impl TelemetryPublisher {
    pub fn start(
        config: TelemetryConfig,
        mut sink: impl FnMut(&str) -> io::Result<()> + Send + 'static,
    ) -> Result<Self, SemanticError> {
        if config.interval.is_zero()
            || Instant::now().checked_add(config.interval).is_none()
            || config.queue_capacity == 0
            || config.max_event_bytes == 0
            || config
                .queue_capacity
                .checked_mul(config.max_event_bytes)
                .is_none()
        {
            return Err(SemanticError::incorrect(
                "telemetry/invalid-config",
                "interval, queue capacity and event bytes must be positive and representable",
            ));
        }
        let (finished_tx, finished) = mpsc::sync_channel(1);
        let (sender, receiver) = if config.enabled {
            let (sender, receiver) = mpsc::sync_channel(config.queue_capacity);
            (Some(sender), Some(receiver))
        } else {
            (None, None)
        };
        let emitter = TelemetryEmitter(Arc::new(Inner {
            config,
            sender,
            stop: AtomicBool::new(false),
            admission: Mutex::new(()),
            sequence: AtomicU64::new(0),
            next_sample: Mutex::new(None),
            last_pressure: Mutex::new(None),
            counters: Counters::default(),
        }));
        let worker = if let Some(receiver) = receiver {
            let inner = emitter.0.clone();
            Some(
                thread::Builder::new()
                    .name("atomic-telemetry".into())
                    .spawn(move || {
                        while !inner.stop.load(Ordering::Acquire) {
                            let line = match receiver.recv_timeout(Duration::from_millis(25)) {
                                Ok(line) => line,
                                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                            };
                            if inner.stop.load(Ordering::Acquire) {
                                inner
                                    .counters
                                    .dropped_shutdown
                                    .fetch_add(1, Ordering::Relaxed);
                                break;
                            }
                            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                sink(&line)
                            })) {
                                Ok(Ok(())) => {
                                    inner.counters.delivered.fetch_add(1, Ordering::Relaxed);
                                    inner
                                        .counters
                                        .bytes_delivered
                                        .fetch_add(line.len() as u64, Ordering::Relaxed);
                                }
                                Ok(Err(_)) => {
                                    inner.counters.sink_errors.fetch_add(1, Ordering::Relaxed);
                                }
                                Err(_) => {
                                    inner.counters.sink_panics.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        }
                        let _admission = inner.admission.lock().unwrap_or_else(|p| p.into_inner());
                        while receiver.try_recv().is_ok() {
                            inner
                                .counters
                                .dropped_shutdown
                                .fetch_add(1, Ordering::Relaxed);
                        }
                        drop(_admission);
                        // A user-supplied sink may itself own blocking destructors.
                        // Completion must not be announced before those run.
                        drop(sink);
                        let _ = finished_tx.send(());
                    })
                    .map_err(|_| {
                        SemanticError::new(
                            crate::ErrorCategory::Unavailable,
                            "telemetry/spawn",
                            "could not start the telemetry sink worker",
                        )
                    })?,
            )
        } else {
            None
        };
        Ok(Self {
            emitter,
            finished,
            worker,
        })
    }

    /// JSON lines on stdout. The worker may block in an OS write; transaction
    /// producers never wait for that write. Use a supervised pipe/file sink.
    pub fn stdout(config: TelemetryConfig) -> Result<Self, SemanticError> {
        Self::start(config, |line| {
            let mut out = io::stdout().lock();
            out.write_all(line.as_bytes())?;
            out.flush()
        })
    }

    pub fn emitter(&self) -> TelemetryEmitter {
        self.emitter.clone()
    }
    pub fn stats(&self) -> TelemetryStats {
        self.emitter.stats()
    }
    pub fn request_stop(&self) {
        let _admission = self
            .emitter
            .0
            .admission
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        self.emitter.0.stop.store(true, Ordering::Release);
    }

    /// Stops admission and discards pending events. Returns false if a sink has
    /// not returned within the wait; the worker is detached, never forcibly killed.
    /// An in-flight callback remains owned by that worker until it returns.
    pub fn shutdown(mut self, timeout: Duration) -> bool {
        self.request_stop();
        if self.worker.is_none() {
            return true;
        }
        if self.finished.recv_timeout(timeout).is_err() {
            return false;
        }
        self.worker
            .take()
            .is_none_or(|worker| worker.join().is_ok())
    }
}

impl Drop for TelemetryPublisher {
    fn drop(&mut self) {
        self.request_stop();
    }
}

impl TelemetryEmitter {
    pub fn is_enabled(&self) -> bool {
        self.0.config.enabled && !self.0.stop.load(Ordering::Acquire)
    }
    pub fn stats(&self) -> TelemetryStats {
        self.0.counters.snapshot()
    }

    /// Samples at most once per configured interval. Disabled and not-due calls
    /// do not invoke `capture`; call this from an existing lifecycle/event loop.
    /// There is no hidden sampler, timer task or PostgreSQL connection.
    pub fn publish_if_due(&self, capture: impl FnOnce() -> TelemetrySnapshot) -> bool {
        if !self.is_enabled() {
            return false;
        }
        let now = Instant::now();
        {
            let mut next = self.0.next_sample.lock().unwrap_or_else(|p| p.into_inner());
            if next.is_some_and(|next| next > now) {
                return false;
            }
            *next = now.checked_add(self.0.config.interval);
        }
        self.try_publish(capture());
        true
    }

    /// Submit one snapshot and applicable warnings, without waiting for the sink.
    /// Returns the number of accepted events, not delivery acknowledgements.
    pub fn try_publish(&self, snapshot: TelemetrySnapshot) -> usize {
        if !self.is_enabled() {
            return 0;
        }
        let mut accepted = usize::from(self.encode("atomic.metrics", "info", |json| {
            snapshot_fields(json, &snapshot)?;
            json.raw(",\"metrics\":{")?;
            snapshot_metrics(json, &snapshot)?;
            telemetry_metrics(json, self.stats())?;
            json.raw("}")
        }));
        let config = &self.0.config.warnings;
        let mut warn = |name: &'static str, code: Option<&'static str>| {
            accepted += usize::from(self.encode(name, "warning", |json| {
                snapshot_fields(json, &snapshot)?;
                if let Some(code) = code {
                    json.field_string("code", code)?;
                }
                Ok(())
            }));
        };
        if config.writer_unavailable
            && (snapshot.phase == TelemetryPhase::Failed
                || (snapshot.phase == TelemetryPhase::Active && !snapshot.writer_available))
        {
            warn("atomic.warning.writer_unavailable", None);
        }
        if config.readiness
            && snapshot.phase == TelemetryPhase::Active
            && !(snapshot.listener_ready && snapshot.writer_available)
        {
            warn("atomic.warning.not_ready", None);
        }
        if let Some(service) = &snapshot.service {
            let mut previous = self
                .0
                .last_pressure
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let prior = previous
                .as_ref()
                .filter(|(id, _)| id == &snapshot.identity)
                .map_or(0, |(_, value)| *value);
            let grew = service.rejected_full > prior;
            *previous = Some((snapshot.identity.clone(), service.rejected_full));
            drop(previous);
            if (config.backpressure && grew)
                || config
                    .queued_requests
                    .is_some_and(|limit| service.queued >= limit)
            {
                warn("atomic.warning.backpressure", None);
            }
        }
        if let Some(index) = &snapshot.indexing {
            if config.indexing_failure {
                if let Some(failure) = &index.last_failure {
                    warn("atomic.warning.indexing_failed", Some(failure.code));
                }
                if let Some(failure) = &index.fulltext.last_failure {
                    warn("atomic.warning.fulltext_failed", Some(failure.code));
                }
            }
            if config.excision_paused
                && let Some(failure) = &index.excision_failure
            {
                warn("atomic.warning.excision_paused", Some(failure.code));
            }
        }
        accepted
    }

    pub fn publish_operation(&self, report: &SqlIoReport) -> bool {
        self.encode("atomic.operation", "info", |json| {
            operation_fields(json, report)
        })
    }

    /// A callback bridge that only attempts bounded encoding/enqueueing.
    pub fn operation_callback(&self) -> crate::SqlMetricCallback {
        let emitter = self.clone();
        Arc::new(move |report| {
            emitter.publish_operation(&report);
        })
    }

    pub fn publish_transaction(
        &self,
        identity: &DatabaseIdentity,
        diagnostics: &crate::TransactionDiagnostics,
    ) -> bool {
        self.encode("atomic.transaction", "info", |json| {
            identity_fields(json, Some(identity))?;
            json.field_number("basis_t", diagnostics.basis_t)?;
            json.field_bool("replayed", diagnostics.replayed)?;
            operation_fields(json, &diagnostics.report)
        })
    }

    fn encode(
        &self,
        event: &'static str,
        level: &'static str,
        body: impl FnOnce(&mut Json) -> fmt::Result,
    ) -> bool {
        if !self.0.config.enabled {
            return false;
        }
        if self.0.stop.load(Ordering::Acquire) {
            self.0
                .counters
                .dropped_stopped
                .fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let mut json = Json {
            text: String::with_capacity(self.0.config.max_event_bytes),
            limit: self.0.config.max_event_bytes,
        };
        let result = (|| {
            json.raw("{\"version\":1")?;
            json.field_string("event", event)?;
            json.field_string("level", level)?;
            json.field_number("sequence", self.0.sequence.fetch_add(1, Ordering::Relaxed))?;
            json.field_number(
                "unix_millis",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .min(u64::MAX as u128) as u64,
            )?;
            body(&mut json)?;
            json.raw("}\n")
        })();
        if result.is_err() {
            self.0
                .counters
                .dropped_oversize
                .fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let _admission = self.0.admission.lock().unwrap_or_else(|p| p.into_inner());
        if self.0.stop.load(Ordering::Acquire) {
            self.0
                .counters
                .dropped_stopped
                .fetch_add(1, Ordering::Relaxed);
            return false;
        }
        match self
            .0
            .sender
            .as_ref()
            .expect("enabled sender")
            .try_send(json.text)
        {
            Ok(()) => {
                self.0.counters.accepted.fetch_add(1, Ordering::Relaxed);
                true
            }
            Err(mpsc::TrySendError::Full(_)) => {
                self.0.counters.dropped_full.fetch_add(1, Ordering::Relaxed);
                false
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                self.0
                    .counters
                    .dropped_stopped
                    .fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }
}

struct Json {
    text: String,
    limit: usize,
}
impl fmt::Write for Json {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        if text.len() > self.limit.saturating_sub(self.text.len()) {
            return Err(fmt::Error);
        }
        self.text.push_str(text);
        Ok(())
    }
}
impl Json {
    fn raw(&mut self, text: &str) -> fmt::Result {
        self.write_str(text)
    }
    fn string(&mut self, text: &str) -> fmt::Result {
        self.raw("\"")?;
        self.string_body(text)?;
        self.raw("\"")
    }
    fn string_body(&mut self, text: &str) -> fmt::Result {
        for ch in text.chars() {
            match ch {
                '"' => self.raw("\\\"")?,
                '\\' => self.raw("\\\\")?,
                '\n' => self.raw("\\n")?,
                '\r' => self.raw("\\r")?,
                '\t' => self.raw("\\t")?,
                '\u{0000}'..='\u{001f}' => write!(self, "\\u{:04x}", ch as u32)?,
                ch => self.write_char(ch)?,
            }
        }
        Ok(())
    }
    fn keyword(&mut self, keyword: &crate::Keyword) -> fmt::Result {
        self.raw("\":")?;
        if let Some(namespace) = &keyword.namespace {
            self.string_body(namespace)?;
            self.raw("/")?;
        }
        self.string_body(&keyword.name)?;
        self.raw("\"")
    }
    fn key(&mut self, key: &str) -> fmt::Result {
        self.raw(",")?;
        self.string(key)?;
        self.raw(":")
    }
    fn field_string(&mut self, key: &str, value: &str) -> fmt::Result {
        self.key(key)?;
        self.string(value)
    }
    fn field_number(&mut self, key: &str, value: u64) -> fmt::Result {
        self.key(key)?;
        write!(self, "{value}")
    }
    fn field_bool(&mut self, key: &str, value: bool) -> fmt::Result {
        self.key(key)?;
        write!(self, "{value}")
    }
}

fn identity_fields(json: &mut Json, identity: Option<&DatabaseIdentity>) -> fmt::Result {
    if let Some(identity) = identity {
        json.field_string("database_id", identity.database_id())?;
        json.field_string("lineage_id", identity.lineage_id())?;
    }
    Ok(())
}
fn snapshot_fields(json: &mut Json, snapshot: &TelemetrySnapshot) -> fmt::Result {
    identity_fields(json, snapshot.identity.as_ref())?;
    json.field_string("phase", snapshot.phase.name())?;
    json.field_bool("writer_available", snapshot.writer_available)?;
    json.field_bool("listener_ready", snapshot.listener_ready)?;
    if let Some(epoch) = snapshot.lease_epoch {
        json.field_number("lease_epoch", epoch)?;
    }
    Ok(())
}
fn snapshot_metrics(json: &mut Json, snapshot: &TelemetrySnapshot) -> fmt::Result {
    // The first metric has no comma. All later numeric fields use the same helper.
    write!(json, "\"process.sql.calls\":{}", snapshot.process_sql.calls)?;
    json.field_number("process.sql.errors", snapshot.process_sql.errors)?;
    json.field_number(
        "process.sql.result_cell_bytes",
        snapshot.process_sql.result_cell_bytes,
    )?;
    json.field_number(
        "process.sql.elapsed_nanos",
        snapshot.process_sql.elapsed_nanos,
    )?;
    if let Some(service) = &snapshot.service {
        json.field_number("service.queued", service.queued as u64)?;
        json.field_number("service.max_queued", service.max_queued as u64)?;
        json.field_number("service.processed", service.processed)?;
        json.field_number("service.rejected_full", service.rejected_full)?;
    }
    if let Some(index) = &snapshot.indexing {
        for (key, value) in [
            ("index.published_basis_t", index.published_basis_t),
            ("index.target_basis_t", index.target_basis_t),
            (
                "index.pending_avet_projections",
                index.pending_avet_projections,
            ),
            ("index.memory_bytes", index.memory_index_bytes),
            ("index.frozen_bytes", index.indexing_bytes),
            ("index.jobs_started", index.jobs_started),
            ("index.jobs_completed", index.jobs_completed),
            ("index.jobs_failed", index.jobs_failed),
            ("index.backpressure_stalls", index.backpressure_stalls),
            (
                "index.backpressure_rejections",
                index.backpressure_rejections,
            ),
            ("fulltext.attempts", index.fulltext.attempts),
            ("fulltext.failures", index.fulltext.failures),
            ("excision.steps", index.excision.steps),
            (
                "excision.source_transactions",
                index.excision.source_transactions,
            ),
            (
                "excision.source_payload_bytes",
                index.excision.source_payload_bytes,
            ),
            (
                "excision.peak_admitted_bytes",
                index.excision.peak_admitted_bytes,
            ),
            ("excision.rows_staged", index.excision.rows_staged),
        ] {
            json.field_number(key, value)?;
        }
        json.field_bool("excision.complete", index.excision.complete)?;
        json.field_string("excision.phase", index.excision.phase)?;
    }
    Ok(())
}
fn telemetry_metrics(json: &mut Json, stats: TelemetryStats) -> fmt::Result {
    for (key, value) in [
        ("telemetry.accepted", stats.accepted),
        ("telemetry.delivered", stats.delivered),
        ("telemetry.dropped_full", stats.dropped_full),
        ("telemetry.dropped_oversize", stats.dropped_oversize),
        ("telemetry.dropped_stopped", stats.dropped_stopped),
        ("telemetry.dropped_shutdown", stats.dropped_shutdown),
        ("telemetry.sink_errors", stats.sink_errors),
        ("telemetry.sink_panics", stats.sink_panics),
        ("telemetry.bytes_delivered", stats.bytes_delivered),
    ] {
        json.field_number(key, value)?;
    }
    Ok(())
}
fn operation_fields(json: &mut Json, report: &SqlIoReport) -> fmt::Result {
    json.raw(",\"operation\":\"")?;
    write!(json, "{:?}", report.kind)?;
    json.raw("\"")?;
    json.field_number("operation_elapsed_nanos", report.operation_elapsed_nanos)?;
    if let Some(context) = &report.context {
        json.key("context")?;
        json.keyword(context)?;
    }
    json.field_bool("nested_truncated", report.nested_truncated)?;
    json.raw(",\"metrics\":{")?;
    operation_metrics(json, &report.stats)?;
    json.raw("},\"nested\":[")?;
    for (index, (name, stats)) in report.nested.iter().enumerate() {
        if index != 0 {
            json.raw(",")?;
        }
        json.raw("{\"context\":")?;
        json.keyword(name)?;
        json.raw(",\"metrics\":{")?;
        operation_metrics(json, stats)?;
        json.raw("}}")?;
    }
    json.raw("]")
}

fn scalar_metrics(stats: &SqlIoStats) -> impl Iterator<Item = (&'static str, u64)> {
    [
        ("sql.calls", stats.calls),
        ("sql.errors", stats.errors),
        ("sql.rows", stats.rows),
        ("sql.result_cell_bytes", stats.result_cell_bytes),
        ("sql.elapsed_nanos", stats.elapsed_nanos),
        ("sql.payload_read_bytes", stats.known_payload_read_bytes),
        ("sql.payload_write_bytes", stats.known_payload_write_bytes),
        ("transaction.assessments", stats.transaction.assessments),
        (
            "transaction.input_operations",
            stats.transaction.input_operations,
        ),
        (
            "transaction.identity_claims",
            stats.transaction.identity_claims,
        ),
        (
            "transaction.identity_lookups",
            stats.transaction.identity_lookups,
        ),
        (
            "transaction.upsert_resolutions",
            stats.transaction.upsert_resolutions,
        ),
        (
            "transaction.uniqueness_checks",
            stats.transaction.uniqueness_checks,
        ),
        (
            "transaction.redundancy_checks",
            stats.transaction.redundancy_checks,
        ),
        (
            "transaction.redundant_datoms",
            stats.transaction.redundant_datoms,
        ),
        (
            "transaction.duplicate_datoms",
            stats.transaction.duplicate_datoms,
        ),
        (
            "transaction.composite_candidates",
            stats.transaction.composite_candidates,
        ),
        (
            "transaction.composite_datoms",
            stats.transaction.composite_datoms,
        ),
        (
            "transaction.function_calls",
            stats.transaction.function_calls,
        ),
        (
            "transaction.produced_datoms",
            stats.transaction.produced_datoms,
        ),
    ]
    .into_iter()
}

fn cache_metrics(cache: &crate::CacheIoStats) -> impl Iterator<Item = (&'static str, u64)> {
    [
        ("accesses", cache.accesses),
        ("hits", cache.hits),
        ("misses", cache.misses),
        ("errors", cache.errors),
        ("physical_bytes", cache.physical_bytes),
        ("canonical_bytes", cache.canonical_bytes),
        ("elapsed_nanos", cache.elapsed_nanos),
        ("min_nanos", cache.min_nanos),
        ("max_nanos", cache.max_nanos),
    ]
    .into_iter()
}

fn index_metrics(index: &crate::IndexIoStats) -> impl Iterator<Item = (&'static str, u64)> {
    [
        ("cursors", index.cursors),
        ("node_accesses", index.node_accesses),
        ("decoded_misses", index.decoded_misses),
    ]
    .into_iter()
}

fn phase_metrics(
    phase: &crate::sql_io::OperationPhaseStats,
) -> impl Iterator<Item = (&'static str, u64)> {
    [
        ("invocations", phase.invocations),
        ("elapsed_nanos", phase.elapsed_nanos),
    ]
    .into_iter()
}

fn numeric_json(json: &mut Json, fields: impl Iterator<Item = (&'static str, u64)>) -> fmt::Result {
    for (index, (key, value)) in fields.enumerate() {
        if index != 0 {
            json.raw(",")?;
        }
        json.string(key)?;
        write!(json, ":{value}")?;
    }
    Ok(())
}

fn operation_metrics(json: &mut Json, stats: &SqlIoStats) -> fmt::Result {
    numeric_json(json, scalar_metrics(stats))?;
    for (tier, cache) in &stats.reads.cache {
        json.raw(",\"cache.")?;
        write!(json, "{tier:?}")?;
        json.raw("\":{")?;
        numeric_json(json, cache_metrics(cache))?;
        json.raw("}")?;
    }
    for (order, index) in &stats.reads.indexes {
        json.raw(",\"index.")?;
        write!(json, "{order:?}")?;
        json.raw("\":{")?;
        numeric_json(json, index_metrics(index))?;
        json.raw("}")?;
    }
    for (operation, phase) in &stats.phases {
        json.raw(",\"phase.")?;
        write!(json, "{operation:?}")?;
        json.raw("\":{")?;
        numeric_json(json, phase_metrics(phase))?;
        json.raw("}")?;
    }
    Ok(())
}

/// Convert a diagnostic report to plain EDN using the same named numeric fields
/// as the JSON event publisher. This explicit conversion preserves u64 exactly;
/// it is not subject to the asynchronous publisher's queue/event-byte admission.
pub fn io_report_to_edn(report: &SqlIoReport) -> crate::edn::EdnValue {
    use crate::edn::EdnValue as E;
    let keyword = |name| E::Keyword(crate::Keyword::unqualified(name));
    E::Map(vec![
        (
            keyword("operation"),
            E::String(format!("{:?}", report.kind)),
        ),
        (
            keyword("context"),
            report.context.clone().map(E::Keyword).unwrap_or(E::Nil),
        ),
        (
            keyword("operation-elapsed-nanos"),
            edn_number(report.operation_elapsed_nanos),
        ),
        (keyword("metrics"), stats_to_edn(&report.stats)),
        (
            keyword("nested"),
            E::Map(
                report
                    .nested
                    .iter()
                    .map(|(name, stats)| (E::Keyword(name.clone()), stats_to_edn(stats)))
                    .collect(),
            ),
        ),
        (
            keyword("nested-truncated"),
            E::Bool(report.nested_truncated),
        ),
    ])
}

fn edn_number(value: u64) -> crate::edn::EdnValue {
    match i64::try_from(value) {
        Ok(value) => crate::edn::EdnValue::Long(value),
        Err(_) => crate::edn::EdnValue::BigInt(value.into()),
    }
}

fn numeric_edn(
    fields: impl Iterator<Item = (&'static str, u64)>,
) -> Vec<(crate::edn::EdnValue, crate::edn::EdnValue)> {
    fields
        .map(|(name, value)| {
            (
                crate::edn::EdnValue::Keyword(crate::Keyword::unqualified(name)),
                edn_number(value),
            )
        })
        .collect()
}

fn stats_to_edn(stats: &SqlIoStats) -> crate::edn::EdnValue {
    use crate::edn::EdnValue as E;
    let mut fields = numeric_edn(scalar_metrics(stats));
    for (tier, cache) in &stats.reads.cache {
        fields.push((
            E::Keyword(crate::Keyword::unqualified(format!("cache.{tier:?}"))),
            E::Map(numeric_edn(cache_metrics(cache))),
        ));
    }
    for (order, index) in &stats.reads.indexes {
        fields.push((
            E::Keyword(crate::Keyword::unqualified(format!("index.{order:?}"))),
            E::Map(numeric_edn(index_metrics(index))),
        ));
    }
    for (operation, phase) in &stats.phases {
        fields.push((
            E::Keyword(crate::Keyword::unqualified(format!("phase.{operation:?}"))),
            E::Map(numeric_edn(phase_metrics(phase))),
        ));
    }
    E::Map(fields)
}
