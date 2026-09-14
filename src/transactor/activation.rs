//! Claim and activate one writer; unwind each partial worker-start failure.
use super::client::{Shared, TransactionClient, TransactorLease};
use super::config::{ServiceOptions, TransactionServiceConfig};
use super::index_lane;
use super::index_lane::IndexLane;
use super::indexing::{BackgroundIndexing, IndexingSeed, Novelty};
use super::service::TransactionService;
use super::writer;
use super::writer::spawn_error;
use crate::{ErrorCategory, SemanticError};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use crate::storage::{BlockDatabase, BlockReadConfig};
use crate::{BlockTransactor, BlockWriterOptions};

pub(super) fn start(
    config: TransactionServiceConfig,
    options: ServiceOptions,
    database: BlockDatabase,
) -> Result<TransactionService, SemanticError> {
    let connection = config.connection.clone();
    TransactionService::validate_config(&config)?;
    let indexing_config = options.indexing.validate()?;
    options.excision.validate()?;
    options.fulltext_build_limits.validate()?;
    let hint_worker = crate::transaction_hints::HintWorkerSlot::new(options.hint_prefetch)?;
    crate::index::prepare::validate_index_preparation_parallelism(
        options.index_preparation_parallelism,
    )?;
    let reads = BlockReadConfig {
        cache_entries: config.capacity_limits.writer_tree_cache_entries,
        cache_bytes: config.capacity_limits.writer_tree_cache_bytes,
        // Committed data remains readable after a restart with smaller local
        // thresholds. Receipt-first fresh admission remains byte-bounded.
        ..BlockReadConfig::default()
    };
    let mut writer = BlockTransactor::claim(
        &connection,
        database.clone(),
        BlockWriterOptions {
            lease_duration: config.lease_duration,
            capacity: config.capacity_limits,
            reads,
            execution: options.execution,
        },
    )?;
    let (recovery_stats, residency) = match writer.activate() {
        Ok(stats) => stats,
        Err(mut error) => {
            let _ = writer.release();
            if error.category == ErrorCategory::Fault {
                error.message.push_str(
                    "; inspect the database before retrying. If only its derived current index is damaged, use atomic consolidate to rebuild it from the authenticated log; canonical or retained-receipt damage requires separate recovery",
                );
            }
            return Err(error);
        }
    };
    let identity = crate::storage::catalog::identity_string(database.identity);
    let route = crate::storage::catalog::identity_string(database.route);
    // Diagnostic carrier only; BlockTransactor owns the opaque lease token.
    let transport_lease = TransactorLease {
        database_id: route.clone(),
        holder_id: config.holder_id.clone(),
        epoch: writer.writer_epoch(),
    };
    let (index_sender, index_wakes) = mpsc::sync_channel(1);
    let pending = writer
        .index_backlog()
        .into_iter()
        .map(|entry| Novelty {
            basis_t: entry.basis_t,
            datoms: entry.datoms,
            bytes: entry.accounted_bytes,
        })
        .collect();
    let pending_avet = writer.pending_avet_projections();
    let indexing = Arc::new(BackgroundIndexing::new(
        indexing_config,
        IndexingSeed {
            published_revision: residency.publication_revision,
            published_basis_t: recovery_stats.base_t,
            pending_avet_projections: pending_avet,
            newest_observed_revision: residency.publication_revision,
            target_basis_t: recovery_stats.target_t,
            pending,
            publication_work_through: (pending_avet != 0).then_some(recovery_stats.base_t),
            required_publication_t: if pending_avet != 0 {
                recovery_stats.target_t
            } else {
                0
            },
            needs_publication: pending_avet != 0,
        },
        index_sender,
    ));
    let mut shared = Shared::new(
        crate::storage::log::MAX_LOG_TRANSACTION_BYTES,
        residency,
        indexing,
        route,
        identity,
        transport_lease,
        database.identity,
    );
    shared.hint_worker = hint_worker;
    shared.program_cache = writer.program_cache();
    shared.telemetry = options.telemetry;
    let shared = Arc::new(shared);
    let (jobs, job_receiver) = mpsc::sync_channel(1);
    let (completed, completions) = mpsc::sync_channel(1);
    let index_shared = Arc::clone(&shared);
    let index_connection = connection.clone();
    let preparation_parallelism = options.index_preparation_parallelism;
    let excision_config = options.excision;
    let fulltext_limits = options.fulltext_build_limits;
    let index_fulltext_limits = fulltext_limits.clone();
    let index_worker = match thread::Builder::new()
        .name(format!("atomic-index-{}", config.holder_id))
        .spawn(move || {
            index_lane::prepare_worker(
                index_connection,
                job_receiver,
                completed,
                preparation_parallelism,
                index_fulltext_limits,
                &index_shared,
            )
        }) {
        Ok(worker) => worker,
        Err(error) => {
            let _ = writer.release();
            return Err(spawn_error(error));
        }
    };
    let (sender, receiver) = mpsc::sync_channel(config.queue_capacity);
    // A failed spawn still leaves the real lease available for cleanup.
    let owner = Arc::new(Mutex::new(Some(writer)));
    let worker_owner = Arc::clone(&owner);
    let worker_shared = Arc::clone(&shared);
    let worker = match thread::Builder::new()
        .name(format!("atomic-transactor-{}", config.holder_id))
        .spawn(move || {
            let writer = worker_owner
                .lock()
                .expect("writer startup mutex poisoned")
                .take()
                .expect("one writer thread");
            writer::run(
                writer,
                connection,
                config.renew_interval,
                receiver,
                IndexLane {
                    jobs,
                    completions,
                    wakes: index_wakes,
                    failures: 0,
                    retry_at: None,
                },
                excision_config,
                fulltext_limits,
                config.queue_capacity,
                &worker_shared,
            );
        }) {
        Ok(worker) => worker,
        Err(error) => {
            shared.accepting.store(false, Ordering::Release);
            if let Some(writer) = owner.lock().expect("writer startup mutex poisoned").take() {
                let _ = writer.release();
            }
            let _ = index_worker.join();
            return Err(spawn_error(error));
        }
    };
    Ok(TransactionService {
        client: TransactionClient { sender, shared },
        recovery_stats,
        worker: Some(worker),
        index_worker: Some(index_worker),
    })
}
