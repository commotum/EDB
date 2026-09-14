//! Immutable program staging. A temporary root, rather than an application
//! handle, retains an unbound closure until the configured collection cutoff.
use crate::storage::{BatchOutcome, PgBlockStore, RefChange, RefCondition};
use crate::{
    ErrorCategory, MaintenanceControl, PostgresConnectionConfig, Program, ProgramHash,
    SemanticError,
};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn deploy(
    config: &PostgresConnectionConfig,
    program: &Program,
    control: &MaintenanceControl,
) -> Result<ProgramHash, SemanticError> {
    deploy_graph(config, program, &[], control)
}

fn deploy_graph(
    config: &PostgresConnectionConfig,
    program: &Program,
    dependencies: &[Program],
    control: &MaintenanceControl,
) -> Result<ProgramHash, SemanticError> {
    let bytes = crate::encode_program(program)?;
    let root = crate::sha256(&bytes);
    let mut supplied = BTreeMap::from([(root, bytes)]);
    for dependency in dependencies {
        let bytes = crate::encode_program(dependency)?;
        supplied.insert(crate::sha256(&bytes), bytes);
    }
    let mut store = PgBlockStore::connect(config)?;
    for _ in 0..8 {
        control.check()?;
        let protection = crate::storage::protection::protection(&mut store, &[])?;
        let mut guards = protection.conditions.clone();
        store.set_write_protection(Some(protection))?;
        let result = (|| {
            let mut seen = BTreeSet::new();
            let mut pending = vec![root];
            while let Some(id) = pending.pop() {
                control.check()?;
                if !seen.insert(id) {
                    continue;
                }
                let payload = if let Some(bytes) = supplied.get(&id) {
                    bytes.clone()
                } else {
                    store.get(id)?.ok_or_else(|| {
                        SemanticError::new(
                            ErrorCategory::NotFound,
                            "program/dependency-not-found",
                            "A fixed program dependency is absent",
                        )
                    })?
                };
                let dependency = crate::decode_program(&payload)?;
                crate::program_bindings::collect_fixed_program_dependencies(
                    &dependency.instructions,
                    &mut pending,
                );
                store.put(&payload)?;
            }
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| {
                    SemanticError::incorrect("program/staging-clock", "Clock precedes Unix epoch")
                })?
                .as_millis();
            let key = format!("staging/{now:016x}/{:032x}", crate::uuid_v7()?);
            guards.push(RefCondition {
                key: key.clone(),
                expected: None,
            });
            crate::storage::ownership::publish_refs(
                &mut store,
                &guards,
                &[RefChange {
                    key,
                    value: Some(root.to_vec()),
                }],
            )
        })();
        store.set_write_protection(None)?;
        match result {
            Ok(BatchOutcome::Applied(_)) => return Ok(root),
            Ok(BatchOutcome::Conflict(_)) => continue,
            Err(error) if error.category == ErrorCategory::Conflict => continue,
            Err(error) => return Err(error),
        }
    }
    Err(SemanticError::new(
        ErrorCategory::Busy,
        "program/staging-contention",
        "Concurrent maintenance prevented program staging; retry this operation",
    ))
}

/// Resolve an acknowledged request before inspecting deployment dependencies.
/// Staging and its bounded collector retry are internal to installation.
pub(crate) fn install(
    operator: &super::PostgresOperator,
    client: &crate::TransactionClient,
    request_key: &str,
    ident: crate::Keyword,
    program: &Program,
    dependencies: &[Program],
    timeout: std::time::Duration,
) -> Result<crate::ServiceTransactionReport, SemanticError> {
    let config = &operator.connection;
    let control = &operator.maintenance;
    let hash = crate::program_hash(program)?;
    let identity = client.identity();
    let entry = crate::DatabaseCatalog::connect_configured(config)?
        .require_active_id(identity.database_id())?;
    if entry.lineage_id != identity.lineage_id() {
        return Err(SemanticError::incorrect(
            "program/install-target",
            "Client and storage identify different databases",
        ));
    }
    let parse = |text: &str| {
        u128::from_str_radix(&text.replace('-', ""), 16)
            .map(u128::to_be_bytes)
            .map_err(|_| {
                SemanticError::incorrect("program/install-target", "Invalid catalog identity")
            })
    };
    let database = crate::storage::BlockDatabase {
        identity: parse(&entry.lineage_id)?,
        route: parse(&entry.database_id)?,
        name: entry.name.unwrap_or_default(),
    };
    let request = crate::TransactionRequest::new(
        request_key,
        vec![
            crate::TxOp::Add {
                entity: crate::EntityRef::Temp("function".into()),
                attribute: crate::DB_IDENT as u32,
                value: crate::Value::Keyword(ident).into(),
            },
            crate::TxOp::Add {
                entity: crate::EntityRef::Temp("function".into()),
                attribute: crate::DB_FN as u32,
                value: crate::Value::Function(hash).into(),
            },
        ],
    );
    let (digest, _) = crate::encoding::canonical_submission_request(
        &request.forms,
        request.compare_basis_t,
        request.tx_instant_override,
        crate::storage::log::MAX_LOG_TRANSACTION_BYTES,
    )?;
    let reader = crate::storage::BlockReader::connect(config, Default::default())?;
    if let Some(outcome) = reader.resolve_request_outcome(&database, request_key, digest)? {
        return Ok(outcome);
    }
    deploy_graph(config, program, dependencies, control)?;
    client.transact(request, timeout)
}
