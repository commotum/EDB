use edb_core::{
    CapacityLimits, Connection, DB_DOC, DB_SOURCE_SYSTEM, DB_TX_INSTANT, EntityRef, ErrorCategory,
    NativeRegistry, PostgresConnectionConfig, RuntimeValue, Schema, Symbol,
    TransactionExecutionOptions, TransactionRequest, TransactionService, TransactionServiceConfig,
    TxForm, TxOp, Value,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[test]
#[ignore = "requires EDB_TEST_POSTGRES_URL for a disposable PostgreSQL database"]
fn accepted_native_requests_replay_after_deployment_removal_but_fresh_calls_fail() {
    let config = PostgresConnectionConfig::parse(
        std::env::var("EDB_TEST_POSTGRES_URL")
            .expect("EDB_TEST_POSTGRES_URL must name a disposable PostgreSQL database"),
    )
    .unwrap();
    let name = format!(
        "native-deployment-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    edb_core::storage::PgBlockStore::install(&config).unwrap();
    edb_core::storage::BlockDatabase::create(&config, &name, Schema::new()).unwrap();
    let service_config = TransactionServiceConfig {
        connection: config.clone(),
        database_id: name.clone(),
        holder_id: name.clone(),
        lease_duration: Duration::from_secs(10),
        renew_interval: Duration::from_millis(100),
        queue_capacity: 32,
        capacity_limits: CapacityLimits::default(),
    };
    let mut registry = NativeRegistry::builder();
    registry
        .transaction(
            Symbol::new("test.native", "describe"),
            |_, args, context| {
                context.check(1)?;
                let [
                    RuntimeValue::Scalar(Value::String(tempid)),
                    RuntimeValue::Scalar(value),
                ] = args
                else {
                    return Err(edb_core::SemanticError::incorrect(
                        "test/arguments",
                        "expected a tempid and a description",
                    ));
                };
                Ok(vec![TxForm::Op(TxOp::Add {
                    entity: EntityRef::Temp(tempid.clone()),
                    attribute: DB_DOC as u32,
                    value: value.clone().into(),
                })])
            },
        )
        .unwrap();
    let writer = TransactionService::start_with_execution_options(
        service_config.clone(),
        TransactionExecutionOptions {
            native: registry.build(),
            ..Default::default()
        },
    )
    .unwrap();
    let connection = Connection::attach_configured(config.clone(), writer.client(), 64).unwrap();
    let captured_before = connection.db();
    let request = TransactionRequest::from_edn(
        "native-description",
        r#"[[test.native/describe "contributor" "Original description"]]"#,
        EntityRef::from_edn(r#""contributor""#).unwrap(),
    )
    .unwrap();
    let timeout = Duration::from_secs(10);
    let accepted = connection.transact(request.clone(), timeout).unwrap();
    assert!(!accepted.replayed);
    let source = accepted.tempids["contributor"];
    assert_eq!(accepted.tx_data.len(), 2);
    assert!(accepted.tx_data.iter().all(|datom| datom.source == source));
    assert_eq!(
        accepted
            .tx_data
            .iter()
            .filter(|datom| datom.attribute == DB_TX_INSTANT as u32)
            .count(),
        1
    );
    assert_eq!(
        accepted.db_after.values(source, DB_DOC as u32).unwrap(),
        vec![Value::String("Original description".into())]
    );
    assert!(
        captured_before
            .values(source, DB_DOC as u32)
            .unwrap()
            .is_empty()
    );
    let original_snapshot = accepted.db_after.snapshot_key().unwrap();
    drop(connection);
    writer.shutdown();

    // Stock execution has no native deployments. Receipts precede fresh assessment.
    let restarted = TransactionService::start(service_config).unwrap();
    let connection = Connection::attach_configured(config, restarted.client(), 64).unwrap();
    let basis = connection.db().basis_t();
    assert_eq!(basis, accepted.basis_t);
    let replay = connection.transact(request.clone(), timeout).unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.basis_t, accepted.basis_t);
    assert_eq!(replay.tx_hash, accepted.tx_hash);
    assert_eq!(replay.tx_data, accepted.tx_data);
    assert_eq!(replay.tempids, accepted.tempids);
    assert_eq!(replay.db_after.snapshot_key().unwrap(), original_snapshot);
    let mut fresh = request.clone();
    fresh.request_key = "native-description-fresh".into();
    assert_eq!(
        connection.transact(fresh, timeout).unwrap_err().code,
        "native/deployment-not-found"
    );
    let mut changed = request;
    changed.source = EntityRef::Id(DB_SOURCE_SYSTEM);
    assert_eq!(
        connection.transact(changed, timeout).unwrap_err().category,
        ErrorCategory::Conflict
    );
    assert_eq!(connection.db().basis_t(), basis);
    assert_eq!(connection.db().snapshot_key().unwrap(), original_snapshot);
    assert!(
        captured_before
            .values(source, DB_DOC as u32)
            .unwrap()
            .is_empty()
    );
    drop(connection);
    restarted.shutdown();
}
