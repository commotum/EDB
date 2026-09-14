//! Serialized outcome/renewal/maintenance/admission order. Preparation grants no publication right.
use super::client::{Shared, Work};
use super::excision_lane::ExcisionLane;
use super::index_lane::IndexLane;
use super::indexing::Novelty;
use super::request::ServiceTransactionReport;
use crate::model::identity::DatabaseIdentity;
use crate::postgres_connection::is_postgres_connection_error;
use crate::{
    Digest, DurableTransaction, ErrorCategory, OperationContext, OperationKind,
    PostgresConnectionConfig, SemanticError,
};
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use crate::BlockTransactor;

pub(super) const MAINTENANCE_POLL: Duration = Duration::from_millis(25);

pub(super) fn connection_unavailable(error: &SemanticError) -> bool {
    (error.category == ErrorCategory::Unavailable && error.code == "storage/postgres")
        || is_postgres_connection_error(error)
}

pub(super) fn spawn_error(error: std::io::Error) -> SemanticError {
    SemanticError::new(
        ErrorCategory::Unavailable,
        "service/spawn",
        error.to_string(),
    )
}

pub(super) fn refresh_progress(writer: &BlockTransactor, shared: &Shared) {
    *shared
        .writer_residency
        .lock()
        .expect("writer residency mutex poisoned") = writer.writer_residency_stats();
}

struct Ambiguous {
    request_key: String,
    request_hash: Digest,
    operation: OperationContext,
    notify: bool,
}

// This worker boundary transfers its transport, queues, maintenance lanes and shared observation state.
#[allow(clippy::too_many_arguments)]
pub(super) fn run(
    mut writer: BlockTransactor,
    connection: PostgresConnectionConfig,
    renew_interval: Duration,
    receiver: mpsc::Receiver<Work>,
    mut index: IndexLane,
    excision_config: crate::ExcisionConfig,
    fulltext_limits: crate::FulltextBuildLimits,
    queue_capacity: usize,
    shared: &Shared,
) {
    let maintenance = OperationContext::new(OperationKind::WriterMaintenance);
    let _maintenance_scope = maintenance.enter();
    let mut renewed_at = Instant::now();
    let mut ambiguous: Option<Ambiguous> = None;
    let mut parked: Option<Work> = None;
    let mut excision = ExcisionLane::new(excision_config, fulltext_limits);
    let mut excision_parked = VecDeque::<Work>::new();
    loop {
        if !shared.accepting.load(Ordering::Acquire) {
            break;
        }
        // Publication uncertainty outranks every maintenance/result/user item.
        if let Some(pending) = ambiguous.take() {
            let _operation = pending.operation.enter();
            let result = writer.reconnect(&connection).and_then(|()| {
                writer.resolve_request_outcome(&pending.request_key, pending.request_hash)
            });
            match result {
                Ok(report) => {
                    if let Some(mut report) = report
                        && pending.notify
                    {
                        report.replayed = false;
                        record_transaction_diagnostics(shared, &mut report, &pending.operation);
                        if let Err(error) = note_report_commit(shared, &shared.database_id, &report)
                        {
                            shared.indexing.fail_job(error);
                            shared.accepting.store(false, Ordering::Release);
                        }
                        shared.publish(&report);
                        crate::observation::notices::publish(&connection, &shared.database_id);
                    }
                    if writer.activate().and_then(|_| writer.renew()).is_err() {
                        shared.accepting.store(false, Ordering::Release);
                    } else {
                        refresh_progress(&writer, shared);
                    }
                    renewed_at = Instant::now();
                }
                Err(error)
                    if connection_unavailable(&error)
                        || error.category == ErrorCategory::UnknownOutcome =>
                {
                    ambiguous = Some(pending);
                    thread::park_timeout(renew_interval.min(MAINTENANCE_POLL));
                }
                Err(_) => shared.accepting.store(false, Ordering::Release),
            }
            continue;
        }
        if renewed_at.elapsed() >= renew_interval {
            if writer.renew().is_err() {
                shared.accepting.store(false, Ordering::Release);
                continue;
            }
            renewed_at = Instant::now();
        }
        // Dedicated result slot is never behind a full queue or parked request.
        if !excision.busy() {
            index.poll(&mut writer, &connection, shared);
        }
        excision.poll(&mut writer, &connection, shared);
        if !shared.accepting.load(Ordering::Acquire) {
            continue;
        }
        if !excision.active && parked.is_some() && shared.indexing.should_continue() {
            thread::park_timeout(MAINTENANCE_POLL);
            continue;
        }
        let ready = if excision.active {
            None
        } else {
            excision_parked.pop_front().or_else(|| parked.take())
        };
        let work = match ready {
            Some(work) => work,
            None => match receiver.recv_timeout(
                renew_interval
                    .saturating_sub(renewed_at.elapsed())
                    .min(MAINTENANCE_POLL),
            ) {
                Ok(work) => work,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            },
        };
        let _operation = work.operation.enter();
        // This pure retained-state check is only acted on by the engine's
        // post-receipt gate. A matching retry never inherits fresh-work limits.
        let tail_admission = writer.fresh_tail_admission();
        let mut dequeued = false;
        let mut result = crate::transaction_hints::overlap(
            writer.hint_database_value(),
            work.hints.as_ref(),
            &shared.hint_worker,
            || {
                writer.transact_with_fresh_gate(&work.request, || {
                    if excision.active {
                        return Err(SemanticError::new(
                            ErrorCategory::Busy,
                            "service/excision-in-progress",
                            "Fresh writes wait for the admitted excision generation",
                        ));
                    }
                    tail_admission?;
                    if let Some(error) = shared.indexing.limiting_error() {
                        return Err(error);
                    }
                    decrement_queued(shared);
                    dequeued = true;
                    Ok(())
                })
            },
        );
        if result
            .as_ref()
            .is_err_and(|e| e.code == "service/excision-in-progress")
            && excision_parked.len() < queue_capacity
        {
            // At most one configured queue of parked work. Receipt lookups
            // continue ahead of this fresh-only gate; overflow returns Busy.
            excision_parked.push_back(work);
            continue;
        }
        if result
            .as_ref()
            .is_err_and(|error| error.code == "service/index-backpressure")
            && shared.indexing.force_publication()
        {
            shared.indexing.record_backpressure_stall();
            parked = Some(work);
            continue;
        }
        if !dequeued {
            decrement_queued(shared);
        }
        if let Ok(report) = &mut result {
            record_transaction_diagnostics(shared, report, &work.operation);
            shared.processed.fetch_add(1, Ordering::Relaxed);
            if !report.replayed
                && let Err(error) = note_report_commit(shared, &shared.database_id, report)
            {
                shared.indexing.fail_job(error);
                shared.accepting.store(false, Ordering::Release);
            }
        }
        let publish = result
            .as_ref()
            .ok()
            .filter(|report| !report.replayed)
            .cloned();
        if result.as_ref().is_err_and(|error| {
            error.code == "storage/writer-fenced" || connection_unavailable(error)
        }) {
            shared.accepting.store(false, Ordering::Release);
        }
        if let Err(error) = &result
            && error.category == ErrorCategory::UnknownOutcome
        {
            ambiguous = Some(Ambiguous {
                request_key: work.request.request_key.clone(),
                request_hash: work.request_hash,
                operation: work.operation.clone(),
                notify: error
                    .details
                    .get("ambiguity_kind")
                    .is_some_and(|kind| kind == "publication"),
            });
        }
        refresh_progress(&writer, shared);
        let _ = work.response.send(result);
        if let Some(report) = publish {
            shared.publish(&report);
            crate::observation::notices::publish(&connection, &shared.database_id);
        }
    }
    shared.accepting.store(false, Ordering::Release);
    if let Some(work) = parked {
        decrement_queued(shared);
        let _ = work.response.send(Err(shared.unavailable()));
    }
    drain_unavailable(&receiver, shared);
    for work in excision_parked {
        decrement_queued(shared);
        let _ = work.response.send(Err(shared.unavailable()));
    }
    drop(excision);
    // Dropping the job sender wakes the idle worker; active preparation polls
    // accepting and cannot publish. Its result owns only protected candidates.
    drop(index);
    let _ = writer.release();
}

fn record_transaction_diagnostics(
    shared: &Shared,
    report: &mut ServiceTransactionReport,
    context: &OperationContext,
) {
    if !context.diagnostics_enabled() {
        return;
    }
    let diagnostics = Arc::new(crate::TransactionDiagnostics {
        basis_t: report.basis_t,
        replayed: report.replayed,
        report: context.report(),
    });
    if let Some(emitter) = &shared.telemetry {
        let identity = DatabaseIdentity::new(shared.database_id.clone(), shared.lineage_id.clone());
        emitter.publish_transaction(&identity, &diagnostics);
    }
    report.diagnostics = Some(diagnostics);
}

fn drain_unavailable(receiver: &mpsc::Receiver<Work>, shared: &Shared) {
    while let Ok(work) = receiver.try_recv() {
        decrement_queued(shared);
        let _ = work.response.send(Err(shared.unavailable()));
    }
}

fn decrement_queued(shared: &Shared) {
    let _admission = shared.admission.lock().expect("admission mutex poisoned");
    shared.queued.fetch_sub(1, Ordering::AcqRel);
}

fn note_report_commit(
    shared: &Shared,
    database_id: &str,
    report: &ServiceTransactionReport,
) -> Result<(), SemanticError> {
    if report
        .tx_data
        .iter()
        .any(|d| d.attribute == crate::DB_EXCISE as u32)
    {
        shared
            .indexing
            .excision_requested
            .store(true, Ordering::Release);
    }
    let transaction = DurableTransaction {
        database_id: database_id.to_owned(),
        basis_t: report.basis_t,
        // The hash bytes have fixed width in the canonical envelope and do
        // not affect retained size. The exact predecessor is authenticated
        // by the writer before this accounting-only reconstruction.
        previous_hash: [0; 32],
        eidx_frontier: report.db_after.eidx_frontier(),
        // Opaque log entries contain datoms/allocation checkpoints, while
        // caller tempid names live in the separate receipt tree.
        tempids: BTreeMap::new(),
        tx_data: report.tx_data.clone(),
    };
    let retained = crate::index::recent::retained_entry_stats(&transaction)?;
    let changes_avet_membership = report.tx_data.iter().any(|datom| {
        matches!(
            u64::from(datom.attribute),
            crate::DB_INDEX | crate::DB_UNIQUE
        )
    });
    shared.indexing.note_commit(
        Novelty {
            basis_t: report.basis_t,
            datoms: retained.datoms,
            bytes: retained.accounted_bytes,
        },
        changes_avet_membership,
    );
    Ok(())
}
