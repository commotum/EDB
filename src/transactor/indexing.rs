//! Maintained live/frozen novelty, finite indexing demand and fresh-write pressure.
use super::config::BackgroundIndexingConfig;
use super::observations::{
    BackgroundIndexingFailure, BackgroundIndexingStats, ProjectionObservations,
};
use super::request::IndexRequest;
use crate::{ErrorCategory, SemanticError};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::Instant;

#[derive(Clone, Copy, Debug)]
pub(super) struct Novelty {
    pub(super) basis_t: u64,
    pub(super) datoms: u64,
    pub(super) bytes: u64,
}

#[derive(Debug)]
pub(super) struct IndexingSeed {
    /// Newest fully authenticated publication selected for replay.
    pub(super) published_revision: u64,
    pub(super) published_basis_t: u64,
    pub(super) pending_avet_projections: u64,
    /// Greatest physical publication coordinate, independent of validity.
    pub(super) newest_observed_revision: u64,
    pub(super) target_basis_t: u64,
    pub(super) pending: VecDeque<Novelty>,
    /// A published value whose live-set/AVET work must finish independently
    /// of the ordinary novelty threshold.
    pub(super) publication_work_through: Option<u64>,
    /// Latest schema-membership or explicit hard-cap demand that must reach
    /// a fully projected root before the special demand can be cleared.
    pub(super) required_publication_t: u64,
    /// No usable native publication covers the newest observed revision. A
    /// positive generation's canonical basis-zero value is publishable too.
    pub(super) needs_publication: bool,
}

#[derive(Debug)]
pub(super) struct IndexingBacklog {
    pub(super) published_revision: u64,
    pub(super) published_basis_t: u64,
    pub(super) pending_avet_projections: u64,
    pub(super) newest_observed_revision: u64,
    pub(super) target_basis_t: u64,
    pub(super) pending: VecDeque<Novelty>,
    // Keep exact private totals so subtraction after publication still reports
    // the correct suffix even if a public u64 counter has saturated. A queue
    // of at most usize::MAX entries, each carrying u64 counts, fits in u128.
    pub(super) total_datoms: u128,
    pub(super) total_bytes: u128,
    pub(super) indexing_through: Option<u64>,
    pub(super) indexing_totals: IndexingTotals,
    pub(super) publication_work_through: Option<u64>,
    pub(super) required_publication_t: u64,
    pub(super) needs_publication: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct IndexingTotals {
    pub(super) transactions: u64,
    pub(super) datoms: u128,
    pub(super) bytes: u128,
}

fn public_index_count(count: u128) -> u64 {
    u64::try_from(count).unwrap_or(u64::MAX)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum IndexCommand {
    Wake,
    Shutdown,
}

#[derive(Debug)]
pub(super) struct BackgroundIndexing {
    pub(super) config: BackgroundIndexingConfig,
    pub(super) backlog: Mutex<IndexingBacklog>,
    pub(super) sender: mpsc::SyncSender<IndexCommand>,
    pub(super) jobs_started: AtomicU64,
    pub(super) jobs_completed: AtomicU64,
    pub(super) jobs_failed: AtomicU64,
    pub(super) backpressure_stalls: AtomicU64,
    pub(super) backpressure_rejections: AtomicU64,
    pub(super) last_failure: Mutex<Option<SemanticError>>,
    pub(super) fulltext: Mutex<ProjectionObservations>,
    pub(super) excision: Mutex<(crate::ExcisionProgress, Option<SemanticError>)>,
    pub(super) excision_requested: AtomicBool,
}

impl BackgroundIndexing {
    pub(super) fn new(
        config: BackgroundIndexingConfig,
        seed: IndexingSeed,
        sender: mpsc::SyncSender<IndexCommand>,
    ) -> Self {
        let (total_datoms, total_bytes) =
            seed.pending
                .iter()
                .fold((0_u128, 0_u128), |(datoms, bytes), item| {
                    (
                        datoms + u128::from(item.datoms),
                        bytes + u128::from(item.bytes),
                    )
                });
        Self {
            config,
            backlog: Mutex::new(IndexingBacklog {
                published_revision: seed.published_revision,
                published_basis_t: seed.published_basis_t,
                pending_avet_projections: seed.pending_avet_projections,
                newest_observed_revision: seed.newest_observed_revision,
                target_basis_t: seed.target_basis_t,
                pending: seed.pending,
                total_datoms,
                total_bytes,
                indexing_through: None,
                indexing_totals: IndexingTotals::default(),
                publication_work_through: seed.publication_work_through,
                required_publication_t: seed.required_publication_t,
                needs_publication: seed.needs_publication,
            }),
            sender,
            jobs_started: AtomicU64::new(0),
            jobs_completed: AtomicU64::new(0),
            jobs_failed: AtomicU64::new(0),
            backpressure_stalls: AtomicU64::new(0),
            backpressure_rejections: AtomicU64::new(0),
            last_failure: Mutex::new(None),
            fulltext: Mutex::new(ProjectionObservations::default()),
            excision: Mutex::new(Default::default()),
            excision_requested: AtomicBool::new(true),
        }
    }

    pub(super) fn note_commit(&self, novelty: Novelty, changes_avet_membership: bool) {
        let should_wake = {
            let mut backlog = self.backlog.lock().expect("index backlog mutex poisoned");
            if novelty.basis_t <= backlog.published_basis_t {
                return;
            }
            if backlog
                .pending
                .back()
                .is_some_and(|previous| previous.basis_t >= novelty.basis_t)
            {
                // Fresh authoritative commits are ordered by the one writer.
                // A duplicate basis can only be a replay/race already covered
                // by a publication and must not be charged twice.
                return;
            }
            backlog.target_basis_t = backlog.target_basis_t.max(novelty.basis_t);
            backlog.pending.push_back(novelty);
            backlog.total_datoms += u128::from(novelty.datoms);
            backlog.total_bytes += u128::from(novelty.bytes);
            // AVET membership is information-derived from :db/index and
            // :db/unique. Enabling either requires a broad durable rebuild of
            // prior values even when ordinary novelty is below the byte
            // threshold; disabling it should publish the matching physical
            // projection as well.
            backlog.needs_publication |= changes_avet_membership;
            if changes_avet_membership {
                backlog.required_publication_t =
                    backlog.required_publication_t.max(novelty.basis_t);
            }
            should_index(&backlog, self.config)
        };
        if should_wake {
            self.wake();
        }
    }

    /// Demand a publication independently of the ordinary novelty threshold.
    /// This is the liveness escape hatch when `RecentTier` refuses a
    /// not-yet-assessed append at its own exact hard-cap boundary. The request
    /// remains parked and is retried only after the covering publication.
    pub(super) fn force_publication(&self) -> bool {
        let can_advance = {
            let mut backlog = self.backlog.lock().expect("index backlog mutex poisoned");
            if backlog.target_basis_t <= backlog.published_basis_t {
                false
            } else {
                backlog.needs_publication = true;
                // A force racing an older publication's maintenance must
                // survive that publication's final completion signal.
                backlog.required_publication_t =
                    backlog.required_publication_t.max(backlog.target_basis_t);
                true
            }
        };
        if can_advance {
            self.wake();
        }
        can_advance
    }

    pub(super) fn request_index(&self) -> IndexRequest {
        let request = {
            let mut backlog = self.backlog.lock().expect("index backlog mutex poisoned");
            // Only note_commit (after durable success) and verified publications
            // advance this frontier. Queued/unassessed transactions are excluded.
            let target_t = backlog.target_basis_t;
            let covered = backlog
                .published_basis_t
                .max(backlog.required_publication_t)
                .max(backlog.indexing_through.unwrap_or(0));
            let scheduled = target_t > covered;
            if scheduled {
                backlog.required_publication_t = target_t;
                backlog.needs_publication = true;
            }
            IndexRequest {
                target_t,
                scheduled,
            }
        };
        if request.scheduled {
            self.wake();
        }
        request
    }

    pub(super) fn wake(&self) {
        match self.sender.try_send(IndexCommand::Wake) {
            Ok(()) | Err(mpsc::TrySendError::Full(_)) => {}
            Err(mpsc::TrySendError::Disconnected(_)) => {}
        }
    }

    pub(super) fn shutdown(&self) {
        let _ = self.sender.send(IndexCommand::Shutdown);
    }

    pub(super) fn begin_job(&self) -> Option<u64> {
        let mut backlog = self.backlog.lock().expect("index backlog mutex poisoned");
        if backlog.indexing_through.is_some() {
            return None;
        }
        if !should_index(&backlog, self.config) {
            return None;
        }
        let through = backlog
            .publication_work_through
            .unwrap_or(backlog.target_basis_t);
        // An ordinary job freezes the entire currently committed tail. A
        // maintenance job works only on an already-published value: pending
        // novelty is strictly newer and stays in the live memory index.
        backlog.indexing_totals = if backlog.publication_work_through.is_some() {
            debug_assert!(through <= backlog.published_basis_t);
            IndexingTotals::default()
        } else {
            IndexingTotals {
                transactions: backlog.pending.len() as u64,
                datoms: backlog.total_datoms,
                bytes: backlog.total_bytes,
            }
        };
        backlog.indexing_through = Some(through);
        self.jobs_started.fetch_add(1, Ordering::Relaxed);
        Some(through)
    }

    pub(super) fn complete_job(
        &self,
        published_revision: u64,
        published_basis_t: u64,
        pending_avet_projections: usize,
        index_work_remaining: bool,
    ) {
        self.publish_completed(
            published_revision,
            published_basis_t,
            pending_avet_projections,
            index_work_remaining,
        );
        self.jobs_completed.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn publish_completed(
        &self,
        published_revision: u64,
        published_basis_t: u64,
        pending_avet_projections: usize,
        index_work_remaining: bool,
    ) {
        let mut backlog = self.backlog.lock().expect("index backlog mutex poisoned");
        backlog.published_revision = backlog.published_revision.max(published_revision);
        backlog.newest_observed_revision = backlog.newest_observed_revision.max(published_revision);
        backlog.published_basis_t = backlog.published_basis_t.max(published_basis_t);
        backlog.target_basis_t = backlog.target_basis_t.max(published_basis_t);
        backlog.pending_avet_projections = pending_avet_projections as u64;
        while backlog
            .pending
            .front()
            .is_some_and(|novelty| novelty.basis_t <= published_basis_t)
        {
            if let Some(novelty) = backlog.pending.pop_front() {
                backlog.total_datoms -= u128::from(novelty.datoms);
                backlog.total_bytes -= u128::from(novelty.bytes);
            }
        }
        backlog.indexing_through = None;
        backlog.indexing_totals = IndexingTotals::default();
        backlog.publication_work_through = index_work_remaining.then_some(published_basis_t);
        backlog.needs_publication = index_work_remaining
            || pending_avet_projections != 0
            || backlog.published_revision < backlog.newest_observed_revision
            || backlog.published_basis_t < backlog.required_publication_t;
    }

    pub(super) fn fail_job(&self, error: SemanticError) {
        {
            let mut backlog = self.backlog.lock().expect("index backlog mutex poisoned");
            backlog.indexing_through = None;
            backlog.indexing_totals = IndexingTotals::default();
        }
        *self
            .last_failure
            .lock()
            .expect("index failure mutex poisoned") = Some(error);
        self.jobs_failed.fetch_add(1, Ordering::Relaxed);
    }

    /// A failed preparation/adoption has published no observed completion.
    /// Keep all demand/novelty while releasing its frozen in-flight accounting.
    pub(super) fn retry_job(&self) {
        let mut backlog = self.backlog.lock().expect("index backlog mutex poisoned");
        backlog.indexing_through = None;
        backlog.indexing_totals = IndexingTotals::default();
    }

    pub(super) fn should_continue(&self) -> bool {
        let backlog = self.backlog.lock().expect("index backlog mutex poisoned");
        should_index(&backlog, self.config)
    }

    pub(super) fn limiting_error(&self) -> Option<SemanticError> {
        if let Some(error) = self
            .last_failure
            .lock()
            .expect("index failure mutex poisoned")
            .as_ref()
        {
            return Some(
                SemanticError::new(
                    ErrorCategory::Unavailable,
                    "service/indexing-failed",
                    "background indexing failed; writes are closed",
                )
                .detail("cause_code", error.code)
                .detail("cause", error.message.clone()),
            );
        }
        let total_bytes = self
            .backlog
            .lock()
            .expect("index backlog mutex poisoned")
            .total_bytes;
        if total_bytes > u128::from(self.config.memory_index_max_bytes) {
            return Some(
                SemanticError::new(
                    ErrorCategory::Busy,
                    "service/index-backpressure",
                    "recent index novelty reached the configured hard limit",
                )
                .detail("backlog_bytes", public_index_count(total_bytes).to_string())
                .detail(
                    "memory_index_max_bytes",
                    self.config.memory_index_max_bytes.to_string(),
                ),
            );
        }
        None
    }

    pub(super) fn has_failure(&self) -> bool {
        self.last_failure
            .lock()
            .expect("index failure mutex poisoned")
            .is_some()
    }

    pub(super) fn at_hard_limit(&self) -> bool {
        self.backlog
            .lock()
            .expect("index backlog mutex poisoned")
            .total_bytes
            > u128::from(self.config.memory_index_max_bytes)
    }

    pub(super) fn record_backpressure_stall(&self) {
        self.backpressure_stalls.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record_backpressure_rejection(&self) {
        self.backpressure_rejections.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn projection_started(&self, basis_t: u64) {
        self.fulltext
            .lock()
            .expect("projection status mutex poisoned")
            .started(basis_t);
    }

    pub(super) fn projection_failed(
        &self,
        error: &SemanticError,
        retry_at: Option<Instant>,
        exhausted: bool,
    ) {
        self.fulltext
            .lock()
            .expect("projection status mutex poisoned")
            .failed(error, retry_at, exhausted);
    }

    pub(super) fn projection_adopted(&self, basis_t: u64) {
        self.fulltext
            .lock()
            .expect("projection status mutex poisoned")
            .adopted(basis_t);
    }

    pub(super) fn stats(&self) -> BackgroundIndexingStats {
        self.stats_with_messages(true)
    }

    pub(super) fn stats_with_messages(&self, messages: bool) -> BackgroundIndexingStats {
        let backlog = self.backlog.lock().expect("index backlog mutex poisoned");
        // Status sampling must not make fixed-size commits proportional to the
        // accumulated tail. Only startup and retiring a published prefix walk
        // pending entries; frozen/live counts are maintained by their writers.
        let indexing = backlog.indexing_totals;
        let total_transactions = backlog.pending.len() as u64;
        let last_failure = self
            .last_failure
            .lock()
            .expect("index failure mutex poisoned")
            .as_ref()
            .map(|error| BackgroundIndexingFailure {
                category: error.category,
                code: error.code,
                message: if messages {
                    error.message.clone()
                } else {
                    String::new()
                },
            });
        let excision = self
            .excision
            .lock()
            .expect("excision status mutex poisoned");
        BackgroundIndexingStats {
            excision: excision.0.clone(),
            excision_failure: excision.1.as_ref().map(|e| BackgroundIndexingFailure {
                category: e.category,
                code: e.code,
                message: if messages {
                    e.message.clone()
                } else {
                    String::new()
                },
            }),
            published_revision: backlog.published_revision,
            published_basis_t: backlog.published_basis_t,
            pending_avet_projections: backlog.pending_avet_projections,
            newest_observed_revision: backlog.newest_observed_revision,
            target_basis_t: backlog.target_basis_t,
            memory_index_transactions: total_transactions - indexing.transactions,
            memory_index_datoms: public_index_count(backlog.total_datoms - indexing.datoms),
            memory_index_bytes: public_index_count(backlog.total_bytes - indexing.bytes),
            indexing_transactions: indexing.transactions,
            indexing_datoms: public_index_count(indexing.datoms),
            indexing_bytes: public_index_count(indexing.bytes),
            total_transactions,
            total_datoms: public_index_count(backlog.total_datoms),
            total_bytes: public_index_count(backlog.total_bytes),
            jobs_started: self.jobs_started.load(Ordering::Relaxed),
            jobs_completed: self.jobs_completed.load(Ordering::Relaxed),
            jobs_failed: self.jobs_failed.load(Ordering::Relaxed),
            backpressure_stalls: self.backpressure_stalls.load(Ordering::Relaxed),
            backpressure_rejections: self.backpressure_rejections.load(Ordering::Relaxed),
            job_in_flight: backlog.indexing_through.is_some(),
            last_failure,
            fulltext: self
                .fulltext
                .lock()
                .expect("projection status mutex poisoned")
                .snapshot_with_messages(Instant::now(), messages),
        }
    }
}

pub(super) fn should_index(backlog: &IndexingBacklog, config: BackgroundIndexingConfig) -> bool {
    // Positive log generations have a canonical native basis-zero value.
    // `needs_publication` therefore drives fresh-database bootstrap and
    // same-basis repair even before the first ordinary transaction.
    backlog.needs_publication
        || (backlog.target_basis_t > backlog.published_basis_t
            && backlog.total_bytes > u128::from(config.memory_index_threshold_bytes))
}
