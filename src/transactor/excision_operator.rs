//! Administrative excision owns a temporary writer; shared jobs own only resumable candidates.
//! Keep claim/renew/admit/adopt/release here, not in the shared rewrite/read machinery.
use crate::storage::excision::{ExcisionJob, operator_database, source_generation, work_key};
use crate::storage::{BlockReader, PgBlockStore};
use crate::{ErrorCategory, ExcisionConfig, SemanticError};

/// Administrative driver over exactly the service's checkpoint protocol. A
/// running service keeps its writer lease; callers must stop it or let its
/// automatic worker handle the request instead of bypassing write authority.
pub(crate) fn process_requests(
    config: &crate::PostgresConnectionConfig,
    database_id: &str,
    fault_point: crate::ExcisionFault,
    maintenance: &crate::MaintenanceControl,
    fulltext_limits: &crate::FulltextBuildLimits,
) -> Result<crate::ExcisionReceipt, SemanticError> {
    use crate::{BlockTransactor, BlockWriterOptions};
    maintenance.check()?;
    fulltext_limits.validate()?;
    let database = operator_database(config, database_id)?;
    let reader = BlockReader::connect(config, Default::default())?;
    let mut store = PgBlockStore::connect(config)?;
    let mut writer =
        BlockTransactor::claim(config, database.clone(), BlockWriterOptions::default())?;
    let result = (|| {
        let resumed = store
            .read_ref(&work_key(&database.route))?
            .is_some_and(|r| r.value.is_some());
        let mut control = || maintenance.check();
        let Some(mut job) = ExcisionJob::open_with_fulltext_limits(
            reader.clone(),
            &mut store,
            database.clone(),
            ExcisionConfig::default(),
            fulltext_limits.clone(),
            &mut control,
        )?
        else {
            let snapshot = reader.capture(&database.reference_key())?;
            let root = snapshot.captured_root();
            let hash = root.log.unwrap_or([0; 32]);
            return Ok(crate::ExcisionReceipt {
                database_id: database_id.to_owned(),
                source_generation: snapshot.generation(),
                generation: snapshot.generation(),
                basis_t: root.basis,
                request_count: 0,
                removed_datoms: 0,
                old_head_hash: hash,
                new_head_hash: hash,
                // Completion is already authoritative; this call performed no rewrite.
                resumed: true,
            });
        };
        let source = job.source(&mut store)?;
        let source_generation = source_generation(&mut store, &source)?;
        writer.admit_excision(&source)?;
        let inject = |at| -> Result<(), SemanticError> {
            if fault_point == at {
                Err(fault(
                    "excision/injected-fault",
                    "Injected interruption; durable checkpoints remain resumable",
                ))
            } else {
                Ok(())
            }
        };
        inject(crate::ExcisionFault::AfterCapture)?;
        loop {
            writer.renew()?;
            let finished = job.step(&mut store, &mut control)?;
            maintenance.after_batch()?;
            if finished {
                break;
            }
        }
        let prepared = job.prepared(&mut store)?;
        let receipt = crate::ExcisionReceipt {
            database_id: database_id.to_owned(),
            source_generation,
            generation: job.generation(),
            basis_t: prepared.candidate.basis,
            request_count: job.request_count(),
            removed_datoms: prepared.progress.removed_datoms,
            old_head_hash: source.log.unwrap_or([0; 32]),
            new_head_hash: prepared.candidate.log.unwrap_or([0; 32]),
            resumed,
        };
        inject(crate::ExcisionFault::AfterCandidateStaged)?;
        writer.adopt_excision(prepared)?;
        inject(crate::ExcisionFault::AfterActivation)?;
        Ok(receipt)
    })();
    let release = writer.release();
    match (result, release) {
        (Err(error), _) | (_, Err(error)) => Err(error),
        (Ok(receipt), Ok(())) => Ok(receipt),
    }
}

fn fault(code: &'static str, message: &str) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, code, message)
}
