//! One preparation slot and guarded index adoption on the serialized writer.
use super::client::Shared;
use super::indexing::IndexCommand;
use super::writer::{MAINTENANCE_POLL, refresh_progress};
use crate::{
    ErrorCategory, OperationContext, OperationKind, PostgresConnectionConfig, SemanticError,
};
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::BlockTransactor;
use crate::index::tree::TreeConfig;
use crate::storage::PgBlockStore;
use crate::storage::indexing::{IndexInput, PreparedIndex};

// Recovered `process-request-index` permits the initial publication attempt
// plus two retries before failing the transactor process.
const MAX_INDEX_JOB_RETRIES: usize = 2;

pub(super) struct IndexLane {
    pub(super) jobs: mpsc::SyncSender<IndexInput>,
    pub(super) completions: mpsc::Receiver<Result<PreparedIndex, SemanticError>>,
    pub(super) wakes: mpsc::Receiver<IndexCommand>,
    pub(super) failures: usize,
    pub(super) retry_at: Option<Instant>,
}

pub(super) fn prepare_worker(
    connection: PostgresConnectionConfig,
    jobs: mpsc::Receiver<IndexInput>,
    completed: mpsc::SyncSender<Result<PreparedIndex, SemanticError>>,
    parallelism: usize,
    fulltext_limits: crate::FulltextBuildLimits,
    shared: &Shared,
) {
    let operation = OperationContext::new(OperationKind::Indexing);
    let _scope = operation.enter();
    while shared.accepting.load(Ordering::Acquire) {
        let input = match jobs.recv_timeout(MAINTENANCE_POLL) {
            Ok(input) => input,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        shared.indexing.projection_started(input.through_basis());
        let result = (|| {
            // Never borrow the writer's reader/SQL mutex while doing preparation.
            let mut store = PgBlockStore::connect(&connection)?;
            input
                .with_fulltext_build_limits(fulltext_limits.clone())?
                .prepare_with_parallelism(
                    &mut store,
                    &TreeConfig::default(),
                    parallelism,
                    &mut || {
                        if shared.accepting.load(Ordering::Acquire) {
                            Ok(())
                        } else {
                            Err(SemanticError::new(
                                ErrorCategory::Interrupted,
                                "service/index-cancelled",
                                "Index preparation stopped before publication",
                            ))
                        }
                    },
                )
        })();
        // Exactly one job is in flight. Nonblocking delivery also permits
        // shutdown while the writer no longer services this mailbox.
        if completed.try_send(result).is_err() {
            break;
        }
    }
}

impl IndexLane {
    pub(super) fn failed(&mut self, error: SemanticError, shared: &Shared) {
        if !shared.accepting.load(Ordering::Acquire) {
            return;
        }
        self.failures += 1;
        if self.failures > MAX_INDEX_JOB_RETRIES || error.code == "storage/writer-fenced" {
            self.retry_at = None;
            shared.indexing.projection_failed(&error, None, true);
            shared.indexing.fail_job(error);
            shared.accepting.store(false, Ordering::Release);
        } else {
            self.retry_at = Some(Instant::now() + Duration::from_millis(50 * self.failures as u64));
            shared
                .indexing
                .projection_failed(&error, self.retry_at, false);
            shared.indexing.retry_job();
        }
    }

    pub(super) fn poll(
        &mut self,
        writer: &mut BlockTransactor,
        connection: &PostgresConnectionConfig,
        shared: &Shared,
    ) {
        while let Ok(command) = self.wakes.try_recv() {
            if command == IndexCommand::Shutdown {
                return;
            }
        }
        match self.completions.try_recv() {
            Ok(Ok(prepared)) => {
                let descriptor_id = prepared.descriptor_id;
                let mut adopted = writer.adopt_index(prepared);
                if adopted
                    .as_ref()
                    .is_err_and(|error| error.category == ErrorCategory::UnknownOutcome)
                {
                    adopted = (|| {
                        writer.reconnect(connection)?;
                        writer.activate()?;
                        if writer.current_index_id() != Some(descriptor_id) {
                            return Err(SemanticError::conflict(
                                "service/index-not-observed",
                                "Uncertain index adoption is absent from the current root",
                            ));
                        }
                        Ok(crate::transactor::authority::IndexAdoption {
                            indexed_basis: writer.indexed_basis_t(),
                            publication_revision: writer
                                .writer_residency_stats()
                                .publication_revision,
                            pending_avet: writer.pending_avet_projections(),
                        })
                    })();
                }
                match adopted {
                    Ok(receipt) => {
                        shared.indexing.projection_adopted(receipt.indexed_basis);
                        shared.indexing.complete_job(
                            receipt.publication_revision,
                            receipt.indexed_basis,
                            receipt.pending_avet as usize,
                            receipt.pending_avet != 0,
                        );
                        refresh_progress(writer, shared);
                        crate::observation::notices::publish(connection, &shared.database_id);
                        self.failures = 0;
                        self.retry_at = None;
                    }
                    Err(error) if error.code == "storage/index-source-changed" => {
                        // An explicit operator may publish a newer derived
                        // index without replacing this writer. Discard stale
                        // work and recapture; it is not a failed writer/job.
                        self.failures = 0;
                        self.retry_at = None;
                        shared.indexing.retry_job();
                    }
                    Err(error) => self.failed(error, shared),
                }
            }
            Ok(Err(error)) => self.failed(error, shared),
            Err(mpsc::TryRecvError::Disconnected) => {
                if shared.accepting.load(Ordering::Acquire) {
                    let error = SemanticError::new(
                        ErrorCategory::Unavailable,
                        "service/index-worker-stopped",
                        "Index preparation worker disconnected",
                    );
                    shared.indexing.projection_failed(&error, None, true);
                    shared.indexing.fail_job(error);
                    shared.accepting.store(false, Ordering::Release);
                }
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        if !shared.accepting.load(Ordering::Acquire)
            || self.retry_at.is_some_and(|at| Instant::now() < at)
        {
            return;
        }
        if let Some(through) = shared.indexing.begin_job() {
            let sent = writer.index_input().and_then(|input| {
                self.jobs.try_send(input).map_err(|_| {
                    SemanticError::new(
                        ErrorCategory::Fault,
                        "service/index-mailbox",
                        "The single preparation slot was unexpectedly unavailable",
                    )
                })
            });
            if let Err(error) = sent {
                // No preparer received this attempt, so record its requested
                // basis here rather than silently reporting an idle failure.
                shared.indexing.projection_started(through);
                self.failed(error, shared);
            }
        }
    }
}
