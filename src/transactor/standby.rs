//! Interruptible one-shot leadership contention and explicit service handoff.
use super::config::{BackgroundIndexingConfig, ServiceOptions, TransactionServiceConfig};
use super::service::{TransactionService, resolve_service_database_name};
use super::writer::connection_unavailable;
use crate::{ErrorCategory, SemanticError};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// A local observation of a one-shot leadership contender. This is not a lease
/// check: after taking the service, use `TransactionClient::is_available` and
/// the application's listener state to decide whether it is write-ready.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StandbyStatus {
    Waiting,
    Activating,
    /// Activation completed and its service is waiting to be taken.
    Ready,
    Failed,
    /// Cancellation was requested; shutdown still owns joining and cleanup.
    Stopping,
    Stopped,
    /// Ownership was transferred to the caller; stopping this contender does
    /// not stop that independently owned service.
    Transferred,
}

pub struct TransactionStandby {
    receiver: mpsc::Receiver<Result<TransactionService, SemanticError>>,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<StandbyStatus>>,
    failure: Option<SemanticError>,
    worker: Option<JoinHandle<()>>,
}

impl TransactionStandby {
    pub fn start(
        config: TransactionServiceConfig,
        poll_interval: Duration,
    ) -> Result<Self, SemanticError> {
        Self::start_with_indexing_and_execution_options(
            config,
            BackgroundIndexingConfig::default(),
            crate::TransactionExecutionOptions::default(),
            poll_interval,
        )
    }

    /// Contend for leadership using the same deployment and resource settings
    /// on every attempt. The public name is resolved once before spawning;
    /// rename or name reuse never redirects an already waiting contender.
    ///
    /// Polling is interruptible. Synchronous PostgreSQL activation and final
    /// service cleanup are not preempted; their configured I/O limits still
    /// apply when `shutdown`, `await_active`, or Drop joins the worker.
    pub fn start_with_indexing_and_execution_options(
        config: TransactionServiceConfig,
        indexing_config: BackgroundIndexingConfig,
        options: crate::TransactionExecutionOptions,
        poll_interval: Duration,
    ) -> Result<Self, SemanticError> {
        Self::start_with_options(
            config,
            ServiceOptions {
                indexing: indexing_config,
                execution: options,
                ..Default::default()
            },
            poll_interval,
        )
    }

    pub fn start_with_options(
        config: TransactionServiceConfig,
        options: ServiceOptions,
        poll_interval: Duration,
    ) -> Result<Self, SemanticError> {
        TransactionService::validate_config(&config)?;
        options.indexing.validate()?;
        options.excision.validate()?;
        options.fulltext_build_limits.validate()?;
        options.hint_prefetch.validate()?;
        crate::index::prepare::validate_index_preparation_parallelism(
            options.index_preparation_parallelism,
        )?;
        if poll_interval.is_zero() {
            return Err(SemanticError::incorrect(
                "service/standby-poll",
                "standby poll interval must be positive",
            ));
        }
        // Resolve before spawning, not on each lease attempt. A rename or name
        // reuse while waiting must never redirect this standby to another DB.
        let database = resolve_service_database_name(&config.connection, &config.database_id)?;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let status = Arc::new(Mutex::new(StandbyStatus::Activating));
        let worker_status = Arc::clone(&status);
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name(format!("atomic-standby-{}", config.holder_id))
            .spawn(move || {
                loop {
                    {
                        let mut status = worker_status.lock().expect("standby status poisoned");
                        if worker_stop.load(Ordering::Acquire) {
                            *status = StandbyStatus::Stopped;
                            return;
                        }
                        *status = StandbyStatus::Activating;
                    }
                    let result = TransactionService::start_identity_with_options(
                        config.clone(),
                        options.clone(),
                        database.clone(),
                    );
                    match result {
                        Err(error)
                            if error.code == "storage/writer-active"
                                || error.code == "storage/writer-claim-conflict"
                                || connection_unavailable(&error) =>
                        {
                            {
                                let mut status =
                                    worker_status.lock().expect("standby status poisoned");
                                if worker_stop.load(Ordering::Acquire) {
                                    *status = StandbyStatus::Stopped;
                                    return;
                                }
                                *status = StandbyStatus::Waiting;
                            }
                            // unpark carries a token, so cancellation between
                            // the stop check and parking cannot be lost.
                            thread::park_timeout(poll_interval);
                        }
                        result => {
                            let mut status = worker_status.lock().expect("standby status poisoned");
                            if worker_stop.load(Ordering::Acquire) {
                                drop(status);
                                // A successful activation raced cancellation.
                                // Release its lease/workers before reporting
                                // stopped, never leaving an unclaimed writer.
                                drop(result);
                                *worker_status.lock().expect("standby status poisoned") =
                                    StandbyStatus::Stopped;
                                return;
                            }
                            *status = if result.is_ok() {
                                StandbyStatus::Ready
                            } else {
                                StandbyStatus::Failed
                            };
                            // This one-shot, capacity-one channel cannot fill.
                            // Hold only the status lock across publication so
                            // try_active cannot race a later Ready update.
                            let sent = sender.send(result);
                            drop(status);
                            drop(sent);
                            return;
                        }
                    }
                }
            })
            .map_err(|error| {
                SemanticError::new(
                    ErrorCategory::Unavailable,
                    "service/standby-spawn",
                    error.to_string(),
                )
            })?;
        Ok(Self {
            receiver,
            stop,
            status,
            failure: None,
            worker: Some(worker),
        })
    }

    /// Observe the contender without consuming its result or performing I/O.
    pub fn status(&self) -> StandbyStatus {
        let mut status = self.status.lock().expect("standby status poisoned");
        if matches!(*status, StandbyStatus::Waiting | StandbyStatus::Activating)
            && self
                .worker
                .as_ref()
                .is_some_and(|worker| worker.is_finished())
        {
            // A panicking worker has disconnected its result channel. Do not
            // advertise an indefinitely healthy contender in that case.
            *status = StandbyStatus::Failed;
        }
        *status
    }

    /// Take a completed activation, or return None immediately while waiting.
    /// Does not wait for PostgreSQL, sleep, or join a worker. A terminal error
    /// remains observable on subsequent polls; a successful service transfers
    /// exactly once. Stopping this object never stops a transferred service.
    pub fn try_active(&mut self) -> Result<Option<TransactionService>, SemanticError> {
        let mut status = self.status.lock().expect("standby status poisoned");
        if *status == StandbyStatus::Transferred {
            return Err(SemanticError::incorrect(
                "service/standby-consumed",
                "standby activation was already transferred",
            ));
        }
        if self.stop.load(Ordering::Acquire) {
            return Err(standby_stopped());
        }
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        match self.receiver.try_recv() {
            Ok(Ok(service)) => {
                *status = StandbyStatus::Transferred;
                Ok(Some(service))
            }
            Ok(Err(error)) => {
                *status = StandbyStatus::Failed;
                self.failure = Some(error.clone());
                Err(error)
            }
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => {
                let error = standby_stopped();
                *status = StandbyStatus::Failed;
                self.failure = Some(error.clone());
                Err(error)
            }
        }
    }

    /// Request cancellation and wake idle polling. This does not wait for an
    /// in-flight activation or release a service already buffered for handoff;
    /// call `shutdown` (or drop this object) to join and complete cleanup.
    pub fn request_stop(&self) {
        let mut status = self.status.lock().expect("standby status poisoned");
        self.stop.store(true, Ordering::Release);
        if !matches!(*status, StandbyStatus::Transferred | StandbyStatus::Stopped) {
            *status = StandbyStatus::Stopping;
        }
        if let Some(worker) = &self.worker {
            worker.thread().unpark();
        }
    }

    pub fn shutdown(mut self) {
        self.stop_and_join();
    }

    pub fn await_active(mut self, timeout: Duration) -> Result<TransactionService, SemanticError> {
        if *self.status.lock().expect("standby status poisoned") == StandbyStatus::Transferred {
            return Err(SemanticError::incorrect(
                "service/standby-consumed",
                "standby activation was already transferred",
            ));
        }
        if self.stop.load(Ordering::Acquire) {
            return Err(standby_stopped());
        }
        if let Some(error) = self.failure.take() {
            return Err(error);
        }
        let result = match self.receiver.recv_timeout(timeout) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(SemanticError::new(
                ErrorCategory::Unavailable,
                "service/standby-timeout",
                "standby did not acquire leadership before the deadline",
            )),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(standby_stopped()),
        };
        if result.is_ok() {
            *self.status.lock().expect("standby status poisoned") = StandbyStatus::Transferred;
        }
        self.stop_and_join();
        result
    }

    fn stop_and_join(&mut self) {
        self.request_stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        // Dropping an unclaimed activation runs ordinary service shutdown,
        // including lease release. There is at most one buffered result.
        drop(self.receiver.try_recv());
        let mut status = self.status.lock().expect("standby status poisoned");
        if *status != StandbyStatus::Transferred {
            *status = StandbyStatus::Stopped;
        }
    }
}

fn standby_stopped() -> SemanticError {
    SemanticError::new(
        ErrorCategory::Unavailable,
        "service/standby-stopped",
        "standby stopped before acquiring leadership",
    )
}

impl Drop for TransactionStandby {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}
