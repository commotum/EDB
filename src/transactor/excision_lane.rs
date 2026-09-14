//! Independent excision preparation; the writer acknowledges admission and adoption.
use super::client::Shared;
use super::writer::{MAINTENANCE_POLL, refresh_progress, spawn_error};
use crate::{ErrorCategory, OperationContext, PostgresConnectionConfig, SemanticError};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::BlockTransactor;
use crate::storage::{BlockReadConfig, PgBlockStore};

// The single-slot channel transfers one owned candidate; keep it inline without another allocation.
#[allow(clippy::large_enum_variant)]
enum ExcisionEvent {
    Admitted(crate::storage::root::DatabaseRoot, mpsc::SyncSender<bool>),
    Progress(crate::ExcisionProgress),
    Finished(Result<Option<crate::storage::excision::PreparedExcision>, SemanticError>),
}

/// One independent maintenance worker, one result slot and a fresh-admission
/// gate acknowledged only after the serialized writer validates its source.
pub(super) struct ExcisionLane {
    config: crate::ExcisionConfig,
    fulltext_limits: crate::FulltextBuildLimits,
    events: Option<mpsc::Receiver<ExcisionEvent>>,
    worker: Option<JoinHandle<()>>,
    cancel: Arc<AtomicBool>,
    pub(super) active: bool,
    attempted_basis: Option<u64>,
    retry_at: Option<Instant>,
    pause_started: Option<Instant>,
}
impl ExcisionLane {
    pub(super) fn new(
        config: crate::ExcisionConfig,
        fulltext_limits: crate::FulltextBuildLimits,
    ) -> Self {
        Self {
            config,
            fulltext_limits,
            events: None,
            worker: None,
            cancel: Arc::new(AtomicBool::new(false)),
            active: false,
            attempted_basis: None,
            retry_at: None,
            pause_started: None,
        }
    }
    pub(super) fn busy(&self) -> bool {
        self.events.is_some()
    }
    pub(super) fn failed(&mut self, error: SemanticError, shared: &Shared) {
        if error.category == ErrorCategory::Conflict || error.category == ErrorCategory::Unavailable
        {
            self.retry_at = Some(Instant::now() + Duration::from_millis(100));
            self.attempted_basis = None;
        }
        shared
            .indexing
            .excision
            .lock()
            .expect("excision status mutex poisoned")
            .1 = Some(error);
        self.active = false;
    }
    pub(super) fn poll(
        &mut self,
        writer: &mut BlockTransactor,
        connection: &PostgresConnectionConfig,
        shared: &Shared,
    ) {
        let event = match self.events.as_ref().map(|events| events.try_recv()) {
            Some(Ok(event)) => Some(event),
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.events = None;
                self.failed(
                    SemanticError::new(
                        ErrorCategory::Unavailable,
                        "excision/worker-stopped",
                        "Excision worker stopped before completion",
                    ),
                    shared,
                );
                None
            }
            _ => None,
        };
        if let Some(event) = event {
            match event {
                ExcisionEvent::Admitted(source, permit) => match writer.admit_excision(&source) {
                    Ok(()) => {
                        self.active = true;
                        self.pause_started = Some(Instant::now());
                        let _ = permit.try_send(true);
                    }
                    Err(error) => {
                        let _ = permit.try_send(false);
                        self.failed(error, shared);
                    }
                },
                ExcisionEvent::Progress(progress) => {
                    let mut state = shared
                        .indexing
                        .excision
                        .lock()
                        .expect("excision status mutex poisoned");
                    state.0 = progress;
                    state.1 = None;
                }
                ExcisionEvent::Finished(result) => {
                    self.events = None;
                    match result {
                        Ok(Some(prepared)) => {
                            let expected_metadata = prepared.candidate.metadata;
                            let mut progress = prepared.progress.clone();
                            let result = writer.adopt_excision(prepared).or_else(|error| {
                                if error.category != ErrorCategory::UnknownOutcome {
                                    return Err(error);
                                }
                                writer.reconnect(connection)?;
                                writer.activate()?;
                                let snapshot = writer
                                    .hint_database_value()
                                    .and_then(|v| v.block_snapshot());
                                if snapshot
                                    .as_ref()
                                    .is_none_or(|s| s.captured_root().metadata != expected_metadata)
                                {
                                    return Err(error);
                                }
                                Ok(crate::transactor::authority::IndexAdoption {
                                    indexed_basis: writer.indexed_basis_t(),
                                    publication_revision: writer
                                        .writer_residency_stats()
                                        .publication_revision,
                                    pending_avet: writer.pending_avet_projections(),
                                })
                            });
                            match result {
                                Ok(receipt) => {
                                    progress.phase = "complete";
                                    progress.complete = true;
                                    progress.fresh_write_pause_nanos =
                                        self.pause_started.map_or(0, |s| {
                                            s.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64
                                        });
                                    *shared
                                        .indexing
                                        .excision
                                        .lock()
                                        .expect("excision status mutex poisoned") =
                                        (progress, None);
                                    shared.indexing.publish_completed(
                                        receipt.publication_revision,
                                        receipt.indexed_basis,
                                        receipt.pending_avet as usize,
                                        false,
                                    );
                                    refresh_progress(writer, shared);
                                    crate::observation::notices::publish(
                                        connection,
                                        &shared.database_id,
                                    );
                                    self.attempted_basis = None;
                                }
                                Err(error) => self.failed(error, shared),
                            }
                        }
                        Ok(None) => {}
                        Err(error) => self.failed(error, shared),
                    }
                    self.active = false;
                    self.pause_started = None;
                }
            }
        }
        if self.worker.as_ref().is_some_and(|w| w.is_finished()) {
            let _ = self.worker.take().unwrap().join();
            // A terminal event can still occupy the bounded result slot.
        }
        if !self.config.enabled
            || self.worker.is_some()
            || self.events.is_some()
            || shared.indexing.stats().job_in_flight
            || self.retry_at.is_some_and(|at| Instant::now() < at)
        {
            return;
        }
        let basis = writer.hint_database_value().map(|v| v.basis_t());
        if self.attempted_basis.is_some()
            && !shared.indexing.excision_requested.load(Ordering::Acquire)
        {
            return;
        }
        shared
            .indexing
            .excision_requested
            .store(false, Ordering::Release);
        self.attempted_basis = basis;
        self.retry_at = None;
        let config = self.config;
        let fulltext_limits = self.fulltext_limits.clone();
        let database = writer.database().clone();
        let connection = connection.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = cancel.clone();
        let (send, events) = mpsc::sync_channel(1);
        let operation = OperationContext::current_or_process();
        match thread::Builder::new()
            .name("atomic-block-excision".into())
            .spawn(move || {
                let _scope = operation.enter();
                let mut control = || {
                    if cancel.load(Ordering::Acquire) {
                        Err(SemanticError::new(
                            ErrorCategory::Interrupted,
                            "excision/stopped",
                            "Excision stopped at a durable checkpoint",
                        ))
                    } else {
                        Ok(())
                    }
                };
                let result = (|| {
                    let mut store = PgBlockStore::connect(&connection)?;
                    let reader = crate::storage::BlockReader::connect(
                        &connection,
                        BlockReadConfig {
                            cache_bytes: (config.max_admitted_bytes / 16).min(64 * 1024 * 1024)
                                as usize,
                            max_recent_bytes: usize::try_from(config.max_admitted_bytes / 4)
                                .unwrap_or(usize::MAX),
                            ..Default::default()
                        },
                    )?;
                    let Some(mut job) =
                        crate::storage::excision::ExcisionJob::open_with_fulltext_limits(
                            reader,
                            &mut store,
                            database,
                            config,
                            fulltext_limits,
                            &mut control,
                        )?
                    else {
                        return Ok(None);
                    };
                    let (permit, allowed) = mpsc::sync_channel(1);
                    send.send(ExcisionEvent::Admitted(job.source(&mut store)?, permit))
                        .map_err(|_| {
                            SemanticError::new(
                                ErrorCategory::Interrupted,
                                "excision/stopped",
                                "Excision owner stopped",
                            )
                        })?;
                    loop {
                        control()?;
                        match allowed.recv_timeout(MAINTENANCE_POLL) {
                            Ok(true) => break,
                            Err(mpsc::RecvTimeoutError::Timeout) => {}
                            _ => {
                                return Err(SemanticError::conflict(
                                    "excision/source-changed",
                                    "Source changed during admission",
                                ));
                            }
                        }
                    }
                    let _ = send.try_send(ExcisionEvent::Progress(job.progress()));
                    while !job.step(&mut store, &mut control)? {
                        let _ = send.try_send(ExcisionEvent::Progress(job.progress()));
                    }
                    job.prepared(&mut store).map(Some)
                })();
                let _ = send.send(ExcisionEvent::Finished(result));
            }) {
            Ok(worker) => {
                self.worker = Some(worker);
                self.events = Some(events);
            }
            Err(error) => self.failed(spawn_error(error), shared),
        }
    }
}
impl Drop for ExcisionLane {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        self.events = None;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
