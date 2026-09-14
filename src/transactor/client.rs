//! Bounded admission, report retention and nonblocking native notification hints.
use super::indexing::BackgroundIndexing;
use super::observations::{
    BackgroundIndexingStats, OperationalServiceStats, ServiceStats, WriterResidencyStats,
};
use super::request::{
    IndexRequest, ServiceTransactionReport, TransactionRequest, TransactionTicket,
};
use crate::model::identity::DatabaseIdentity;
use crate::{Digest, ErrorCategory, OperationContext, OperationKind, SemanticError};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::time::Duration;

// Diagnostic routing coordinate only; durable writer authority lives in guarded references.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TransactorLease {
    pub database_id: String,
    pub holder_id: String,
    pub epoch: u64,
}

pub(super) struct Work {
    pub(super) request: TransactionRequest,
    pub(super) request_hash: Digest,
    pub(super) response: mpsc::SyncSender<Result<ServiceTransactionReport, SemanticError>>,
    pub(super) operation: OperationContext,
    pub(super) hints: Option<crate::transaction_hints::PendingHints>,
}

#[derive(Debug)]
pub(super) struct ReportSubscriber {
    sender: mpsc::Sender<ServiceTransactionReport>,
    pending: Arc<AtomicUsize>,
    retention: Arc<Mutex<VecDeque<ReportRetention>>>,
}

#[derive(Clone, Debug)]
struct ReportRetention {
    basis_t: u64,
    payload_bytes: u64,
    roots: [Option<Digest>; 2],
    generations: [Option<u64>; 2],
}

impl ReportRetention {
    fn from_report(report: &ServiceTransactionReport) -> Self {
        let before = report.db_before.native_retention_coordinate();
        let after = report.db_after.native_retention_coordinate();
        let tx_data_bytes = report.tx_data.iter().fold(0_u64, |bytes, datom| {
            bytes.saturating_add(datom.retained_bytes())
        });
        let tempid_bytes = report.tempids.keys().fold(0_u64, |bytes, tempid| {
            bytes
                .saturating_add(std::mem::size_of::<String>() as u64)
                .saturating_add(tempid.capacity() as u64)
                .saturating_add(std::mem::size_of::<u64>() as u64)
        });
        Self {
            basis_t: report.basis_t,
            // Stable owned-payload account. Shared immutable database trees
            // are represented separately by distinct root/generation counts;
            // allocator and mpsc-node overhead are intentionally excluded.
            payload_bytes: (std::mem::size_of::<ServiceTransactionReport>() as u64)
                .saturating_add(tx_data_bytes)
                .saturating_add(tempid_bytes),
            roots: [
                before.and_then(|value| value.1),
                after.and_then(|value| value.1),
            ],
            generations: [before.map(|value| value.0), after.map(|value| value.0)],
        }
    }
}

/// Registration does not own the writer. Removing it releases the peer and
/// does not affect admitted transactions or other connections.
pub(crate) struct NativeObserver {
    id: u64,
    shared: Weak<Shared>,
}

impl Drop for NativeObserver {
    fn drop(&mut self) {
        if let Some(shared) = self.shared.upgrade() {
            shared
                .observers
                .lock()
                .expect("observer mutex poisoned")
                .remove(&self.id);
        }
    }
}

pub(super) struct Shared {
    pub(super) accepting: AtomicBool,
    pub(super) admission: Mutex<()>,
    pub(super) queued: AtomicUsize,
    pub(super) max_queued: AtomicUsize,
    pub(super) processed: AtomicU64,
    pub(super) rejected_full: AtomicU64,
    pub(super) next_subscriber: AtomicU64,
    pub(super) subscribers: Mutex<BTreeMap<u64, ReportSubscriber>>,
    observers: Mutex<BTreeMap<u64, mpsc::SyncSender<Arc<ServiceTransactionReport>>>>,
    pub(super) queued_reports: AtomicUsize,
    pub(super) max_queued_reports: AtomicUsize,
    pub(super) queued_report_payload_bytes: AtomicU64,
    pub(super) max_queued_report_payload_bytes: AtomicU64,
    pub(super) writer_residency: Mutex<WriterResidencyStats>,
    pub(super) program_cache: crate::program_cache::SharedProgramCache,
    pub(super) max_request_bytes: usize,
    pub(super) hint_worker: crate::transaction_hints::HintWorkerSlot,
    pub(super) indexing: Arc<BackgroundIndexing>,
    pub(super) database_id: String,
    pub(super) lineage_id: String,
    pub(super) transport_lease: TransactorLease,
    pub(super) request_identity: [u8; 16],
    pub(super) telemetry: Option<crate::TelemetryEmitter>,
}

impl Shared {
    pub(super) fn new(
        max_request_bytes: usize,
        writer_residency: WriterResidencyStats,
        indexing: Arc<BackgroundIndexing>,
        database_id: String,
        lineage_id: String,
        transport_lease: TransactorLease,
        request_identity: [u8; 16],
    ) -> Self {
        Self {
            accepting: AtomicBool::new(true),
            admission: Mutex::new(()),
            queued: AtomicUsize::new(0),
            max_queued: AtomicUsize::new(0),
            processed: AtomicU64::new(0),
            rejected_full: AtomicU64::new(0),
            next_subscriber: AtomicU64::new(1),
            subscribers: Mutex::new(BTreeMap::new()),
            observers: Mutex::new(BTreeMap::new()),
            queued_reports: AtomicUsize::new(0),
            max_queued_reports: AtomicUsize::new(0),
            queued_report_payload_bytes: AtomicU64::new(0),
            max_queued_report_payload_bytes: AtomicU64::new(0),
            writer_residency: Mutex::new(writer_residency),
            program_cache: Default::default(),
            max_request_bytes,
            hint_worker: crate::transaction_hints::HintWorkerSlot::default(),
            indexing,
            database_id,
            lineage_id,
            transport_lease,
            request_identity,
            telemetry: None,
        }
    }

    pub(super) fn unavailable(&self) -> SemanticError {
        SemanticError::new(
            ErrorCategory::Unavailable,
            "service/unavailable",
            "transaction service is not accepting requests",
        )
    }

    pub(super) fn indexing_stats(&self, messages: bool) -> BackgroundIndexingStats {
        self.indexing.stats_with_messages(messages)
    }

    pub(super) fn observe(&self, report: &ServiceTransactionReport) {
        let observers = self.observers.lock().expect("observer mutex poisoned");
        if !observers.is_empty() {
            let report = Arc::new(report.clone());
            for observer in observers.values() {
                // Hints carry data, not code to invoke on the authoritative
                // writer. A full slot is safe: peer log catch-up repairs gaps.
                let _ = observer.try_send(Arc::clone(&report));
            }
        }
    }

    pub(super) fn publish(&self, report: &ServiceTransactionReport) {
        // Also covers a durable commit discovered after an unknown outcome.
        // Native adoption is idempotent; normal outcomes were observed before
        // their originating result was delivered.
        self.observe(report);
        self.publish_reports(report);
    }

    pub(super) fn publish_reports(&self, report: &ServiceTransactionReport) {
        let retention = ReportRetention::from_report(report);
        let mut subscribers = self.subscribers.lock().expect("subscriber mutex poisoned");
        subscribers.retain(|_, subscriber| {
            subscriber.pending.fetch_add(1, Ordering::AcqRel);
            let queued = self.queued_reports.fetch_add(1, Ordering::AcqRel) + 1;
            self.max_queued_reports.fetch_max(queued, Ordering::Relaxed);
            let payload_bytes = self
                .queued_report_payload_bytes
                .fetch_add(retention.payload_bytes, Ordering::AcqRel)
                .saturating_add(retention.payload_bytes);
            self.max_queued_report_payload_bytes
                .fetch_max(payload_bytes, Ordering::Relaxed);
            subscriber
                .retention
                .lock()
                .expect("report retention mutex poisoned")
                .push_back(retention.clone());
            if subscriber.sender.send(report.clone()).is_ok() {
                true
            } else {
                let removed = subscriber
                    .retention
                    .lock()
                    .expect("report retention mutex poisoned")
                    .pop_back()
                    .expect("failed report send has queued retention");
                subscriber.pending.fetch_sub(1, Ordering::AcqRel);
                self.queued_reports.fetch_sub(1, Ordering::AcqRel);
                self.queued_report_payload_bytes
                    .fetch_sub(removed.payload_bytes, Ordering::AcqRel);
                false
            }
        });
    }
}

#[derive(Clone)]
pub struct TransactionClient {
    pub(super) sender: mpsc::SyncSender<Work>,
    pub(super) shared: Arc<Shared>,
}

impl TransactionClient {
    /// Register a nonblocking notification hint. Setup must not hold the writer
    /// notification lock across peer I/O. Durable-log catch-up fills any gap
    /// between initial opening and registration, or a full local hint channel.
    pub(crate) fn observe_commits(
        &self,
        observer: mpsc::SyncSender<Arc<ServiceTransactionReport>>,
    ) -> NativeObserver {
        let mut observers = self
            .shared
            .observers
            .lock()
            .expect("observer mutex poisoned");
        let id = self.shared.next_subscriber.fetch_add(1, Ordering::Relaxed);
        observers.insert(id, observer);
        NativeObserver {
            id,
            shared: Arc::downgrade(&self.shared),
        }
    }

    /// Stable identity of the one database this client can transact against.
    ///
    /// Request-key hashing is lineage scoped, so exposing the same pair to
    /// the native connection facade lets it reject a peer/client mismatch
    /// before either side can publish observable state.
    pub fn identity(&self) -> DatabaseIdentity {
        DatabaseIdentity::new(
            self.shared.database_id.clone(),
            self.shared.lineage_id.clone(),
        )
    }

    pub(crate) fn transport_lease(&self) -> TransactorLease {
        self.shared.transport_lease.clone()
    }

    pub fn submit(&self, request: TransactionRequest) -> Result<TransactionTicket, SemanticError> {
        self.submit_advisory(request, None)
    }

    /// Admit hints separately from canonical transaction data and request
    /// identity. The execution handle observes best-effort bounded prefetch;
    /// canceling it cannot cancel or change the submitted transaction.
    pub fn submit_with_hints(
        &self,
        request: TransactionRequest,
        hints: crate::TransactionHints,
        options: crate::HintPrefetchOptions,
    ) -> Result<(TransactionTicket, crate::HintExecution), SemanticError> {
        let execution = crate::HintExecution::default();
        let hints = crate::transaction_hints::PendingHints::new(hints, options, execution.clone());
        let ticket = self.submit_advisory(request, Some(hints))?;
        Ok((ticket, execution))
    }

    fn submit_advisory(
        &self,
        request: TransactionRequest,
        hints: Option<crate::transaction_hints::PendingHints>,
    ) -> Result<TransactionTicket, SemanticError> {
        // Attribution crosses the queue explicitly. Contexts contain counters,
        // not storage authority; no worker automatically invokes their callback.
        let detailed = self
            .shared
            .telemetry
            .as_ref()
            .is_some_and(crate::TelemetryEmitter::is_enabled);
        let operation = match (OperationContext::current(), detailed) {
            (Some(parent), true) => parent.child_diagnostic(OperationKind::Transaction),
            (Some(parent), false) => parent.child(OperationKind::Transaction),
            (None, true) => OperationContext::diagnostic(OperationKind::Transaction),
            (None, false) => OperationContext::new(OperationKind::Transaction),
        };
        let _operation_scope = operation.enter();
        if !self.shared.accepting.load(Ordering::Acquire) {
            return Err(self.shared.unavailable());
        }
        if request.request_key.is_empty() {
            return Err(SemanticError::incorrect(
                "postgres/empty-request-key",
                "idempotency request key cannot be empty",
            ));
        }
        if request.request_key.len() > 1_024 {
            return Err(SemanticError::new(
                ErrorCategory::Busy,
                "service/request-key-capacity",
                "idempotency request key exceeds the 1024-byte admission limit",
            ));
        }
        let (request_hash, _) = crate::encoding::canonical_submission_request(
            &request.forms,
            request.compare_basis_t,
            request.tx_instant_override,
            self.shared.max_request_bytes,
        )?;
        let request_key = request.request_key.clone();
        let request_key_hash = crate::storage::receipts::scoped_request_key(
            &self.shared.request_identity,
            &request_key,
        )?;
        let (sender, receiver) = mpsc::sync_channel(1);
        let _admission = self
            .shared
            .admission
            .lock()
            .expect("admission mutex poisoned");
        match self.sender.try_send(Work {
            request,
            request_hash,
            response: sender,
            operation,
            hints,
        }) {
            Ok(()) => {
                let queued = self.shared.queued.fetch_add(1, Ordering::AcqRel) + 1;
                self.shared.max_queued.fetch_max(queued, Ordering::Relaxed);
                Ok(TransactionTicket {
                    request_key,
                    request_key_hash,
                    receiver,
                })
            }
            Err(mpsc::TrySendError::Full(_)) => {
                self.shared.rejected_full.fetch_add(1, Ordering::Relaxed);
                if self.shared.indexing.at_hard_limit() {
                    self.shared.indexing.record_backpressure_rejection();
                }
                Err(SemanticError::new(
                    ErrorCategory::Busy,
                    "service/queue-full",
                    "transaction service queue is full",
                ))
            }
            Err(mpsc::TrySendError::Disconnected(_)) => Err(self.shared.unavailable()),
        }
    }

    pub fn transact(
        &self,
        request: TransactionRequest,
        timeout: Duration,
    ) -> Result<ServiceTransactionReport, SemanticError> {
        self.submit(request)?.wait(timeout)
    }

    pub fn subscribe_reports(&self) -> ReportSubscription {
        let id = self.shared.next_subscriber.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::channel();
        let pending = Arc::new(AtomicUsize::new(0));
        let retention = Arc::new(Mutex::new(VecDeque::new()));
        self.shared
            .subscribers
            .lock()
            .expect("subscriber mutex poisoned")
            .insert(
                id,
                ReportSubscriber {
                    sender,
                    pending: Arc::clone(&pending),
                    retention: Arc::clone(&retention),
                },
            );
        ReportSubscription {
            id,
            receiver: Some(receiver),
            pending,
            retention,
            shared: Arc::downgrade(&self.shared),
        }
    }

    /// Fixed-size local counters; unlike `stats`, this never walks retained
    /// subscriber reports or takes their queue locks.
    pub fn operational_stats(&self) -> OperationalServiceStats {
        OperationalServiceStats {
            accepting: self.shared.accepting.load(Ordering::Acquire),
            queued: self.shared.queued.load(Ordering::Relaxed),
            max_queued: self.shared.max_queued.load(Ordering::Relaxed),
            processed: self.shared.processed.load(Ordering::Relaxed),
            rejected_full: self.shared.rejected_full.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn operational_indexing_stats(&self) -> BackgroundIndexingStats {
        self.shared.indexing_stats(false)
    }

    pub fn stats(&self) -> ServiceStats {
        let subscribers = self
            .shared
            .subscribers
            .lock()
            .expect("subscriber mutex poisoned");
        let mut oldest_queued_report_basis_t = None;
        let mut report_roots = BTreeSet::new();
        let mut report_generations = BTreeSet::new();
        let mut queued_reports = 0_usize;
        let mut queued_report_payload_bytes = 0_u64;
        for subscriber in subscribers.values() {
            let retention = subscriber
                .retention
                .lock()
                .expect("report retention mutex poisoned");
            for retained in retention.iter() {
                queued_reports = queued_reports.saturating_add(1);
                queued_report_payload_bytes =
                    queued_report_payload_bytes.saturating_add(retained.payload_bytes);
                oldest_queued_report_basis_t = Some(
                    oldest_queued_report_basis_t
                        .map_or(retained.basis_t, |oldest: u64| oldest.min(retained.basis_t)),
                );
                report_roots.extend(retained.roots.into_iter().flatten());
                report_generations.extend(retained.generations.into_iter().flatten());
            }
        }
        ServiceStats {
            queued: self.shared.queued.load(Ordering::Relaxed),
            max_queued: self.shared.max_queued.load(Ordering::Relaxed),
            processed: self.shared.processed.load(Ordering::Relaxed),
            rejected_full: self.shared.rejected_full.load(Ordering::Relaxed),
            subscribers: subscribers.len(),
            queued_reports,
            max_queued_reports: self.shared.max_queued_reports.load(Ordering::Relaxed),
            queued_report_payload_bytes,
            max_queued_report_payload_bytes: self
                .shared
                .max_queued_report_payload_bytes
                .load(Ordering::Relaxed),
            oldest_queued_report_basis_t,
            distinct_report_roots: report_roots.len(),
            distinct_report_generations: report_generations.len(),
        }
    }

    /// Snapshot maintained live/frozen aggregates without scanning the backlog.
    pub fn background_indexing_stats(&self) -> BackgroundIndexingStats {
        self.shared.indexing_stats(true)
    }

    /// Request background indexing through the currently observed committed
    /// frontier, independent of the configured novelty threshold.
    ///
    /// Returns after bounded in-memory coordination with the existing index
    /// worker; it performs no indexing or SQL on the caller. Requests coalesce,
    /// and later commits cannot change the returned target. Service shutdown or
    /// failure may prevent completion; a timed-out waiter does not cancel the
    /// shared job. No separate worker or writer authority is created.
    /// A previous optional fulltext failure is also retried, even if the
    /// returned canonical target is already indexed. Search status is separate
    /// and does not change this request's canonical completion semantics.
    pub fn request_index(&self) -> Result<IndexRequest, SemanticError> {
        if !self.shared.accepting.load(Ordering::Acquire) || self.shared.indexing.has_failure() {
            return Err(self.shared.unavailable());
        }
        Ok(self.shared.indexing.request_index())
    }

    /// Deterministic representation-level residency of the live writer.
    /// Unlike allocator RSS, these counters distinguish the recent tier and
    /// immutable cache from a forbidden eager database value.
    pub fn writer_residency_stats(&self) -> WriterResidencyStats {
        *self
            .shared
            .writer_residency
            .lock()
            .expect("writer residency mutex poisoned")
    }

    pub fn is_available(&self) -> bool {
        self.shared.accepting.load(Ordering::Acquire) && !self.shared.indexing.has_failure()
    }
}

pub struct ReportSubscription {
    id: u64,
    receiver: Option<mpsc::Receiver<ServiceTransactionReport>>,
    pending: Arc<AtomicUsize>,
    retention: Arc<Mutex<VecDeque<ReportRetention>>>,
    shared: Weak<Shared>,
}

impl ReportSubscription {
    pub fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> Result<ServiceTransactionReport, mpsc::RecvTimeoutError> {
        let result = self
            .receiver
            .as_ref()
            .expect("live report subscription retains its receiver")
            .recv_timeout(timeout);
        if result.is_ok() {
            self.note_received();
        }
        result
    }

    pub fn try_recv(&self) -> Result<ServiceTransactionReport, mpsc::TryRecvError> {
        let result = self
            .receiver
            .as_ref()
            .expect("live report subscription retains its receiver")
            .try_recv();
        if result.is_ok() {
            self.note_received();
        }
        result
    }

    /// Reports currently retained for this lossless subscriber. Every queued
    /// report owns its immutable db-before/db-after snapshots and their
    /// in-memory read resources until received or dropped; GC retention is age-based.
    pub fn pending_reports(&self) -> usize {
        self.pending.load(Ordering::Acquire)
    }

    fn note_received(&self) {
        let retained = self
            .retention
            .lock()
            .expect("report retention mutex poisoned")
            .pop_front()
            .expect("received report has queued retention");
        let pending = self.pending.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(pending > 0, "received report was not accounted as pending");
        if let Some(shared) = self.shared.upgrade() {
            let queued = shared.queued_reports.fetch_sub(1, Ordering::AcqRel);
            debug_assert!(queued > 0, "received report was not globally accounted");
            let payload = shared
                .queued_report_payload_bytes
                .fetch_sub(retained.payload_bytes, Ordering::AcqRel);
            debug_assert!(
                payload >= retained.payload_bytes,
                "report payload accounting underflow"
            );
        }
    }
}

impl Drop for ReportSubscription {
    fn drop(&mut self) {
        if let Some(shared) = self.shared.upgrade() {
            // Match publish/stats lock order. Removing the subscriber while
            // holding this mutex waits for any in-flight publication and
            // prevents a later one from enqueueing after we drain its exact
            // retention ledger.
            let mut subscribers = shared
                .subscribers
                .lock()
                .expect("subscriber mutex poisoned");
            subscribers.remove(&self.id);
            // Drop the channel's queued report objects before removing their
            // mirrored retention ledger. Concurrent stats may briefly
            // over-report queued coordinates, but can never claim zero while reports still
            // retain immutable database roots.
            drop(self.receiver.take());
            let abandoned_payload_bytes = {
                let mut retention = self
                    .retention
                    .lock()
                    .expect("report retention mutex poisoned");
                let bytes = retention.iter().fold(0_u64, |bytes, retained| {
                    bytes.saturating_add(retained.payload_bytes)
                });
                retention.clear();
                bytes
            };
            let abandoned = self.pending.swap(0, Ordering::AcqRel);
            if abandoned > 0 {
                let queued = shared.queued_reports.fetch_sub(abandoned, Ordering::AcqRel);
                debug_assert!(queued >= abandoned, "report queue accounting underflow");
                let payload = shared
                    .queued_report_payload_bytes
                    .fetch_sub(abandoned_payload_bytes, Ordering::AcqRel);
                debug_assert!(
                    payload >= abandoned_payload_bytes,
                    "report payload accounting underflow"
                );
            }
        } else {
            drop(self.receiver.take());
            self.pending.store(0, Ordering::Release);
            self.retention
                .lock()
                .expect("report retention mutex poisoned")
                .clear();
        }
    }
}
