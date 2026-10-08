use edb_core::{
    AsyncClient, AsyncConfig, AsyncExecutor, Attribute, CapacityLimits, Cardinality, Connection,
    DB_SOURCE_SYSTEM, EntityRef, Keyword, OperationContext, OperationKind,
    PostgresConnectionConfig, Schema, TransactionRequest, TransactionService,
    TransactionServiceConfig, TxOp, Value, ValueType,
};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const NAME: u32 = 1_000;

#[test]
#[ignore = "requires EDB_TEST_POSTGRES_URL for a disposable PostgreSQL database"]
fn causal_values_local_waits_and_queued_async_deadlines_remain_independent() {
    let config = PostgresConnectionConfig::parse(
        std::env::var("EDB_TEST_POSTGRES_URL")
            .expect("EDB_TEST_POSTGRES_URL must name a disposable PostgreSQL database"),
    )
    .unwrap();
    let name = format!(
        "synchronization-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    edb_core::storage::PgBlockStore::install(&config).unwrap();
    edb_core::storage::BlockDatabase::create(&config, &name, Schema::new()).unwrap();
    let writer = TransactionService::start(TransactionServiceConfig {
        connection: config.clone(),
        database_id: name.clone(),
        holder_id: name.clone(),
        lease_duration: Duration::from_secs(10),
        renew_interval: Duration::from_millis(100),
        queue_capacity: 16,
        capacity_limits: CapacityLimits::default(),
    })
    .unwrap();
    let client = writer.client();
    let timeout = Duration::from_secs(5);
    let schema = client
        .transact(
            TransactionRequest::new(
                "schema",
                vec![TxOp::InstallAttribute(Attribute::new(
                    NAME,
                    Keyword::new("item", "name"),
                    ValueType::String,
                    Cardinality::One,
                ))],
                EntityRef::Id(DB_SOURCE_SYSTEM),
            ),
            timeout,
        )
        .unwrap();
    let reader = Connection::connect_configured(config, &name, 64).unwrap();
    let old = reader.sync_to(schema.basis_t, timeout).unwrap();
    let first = client
        .transact(
            TransactionRequest::new(
                "first",
                vec![TxOp::Add {
                    entity: EntityRef::Temp("item".into()),
                    attribute: NAME,
                    value: Value::String("Peaches".into()).into(),
                }],
                EntityRef::Id(99),
            ),
            timeout,
        )
        .unwrap();
    let entity = first.tempids["item"];
    let second = client
        .transact(
            TransactionRequest::new(
                "second",
                vec![TxOp::Add {
                    entity: EntityRef::Id(entity),
                    attribute: NAME,
                    value: Value::String("Pears".into()).into(),
                }],
                EntityRef::Id(98),
            ),
            timeout,
        )
        .unwrap();
    assert!(reader.sync_to(first.basis_t, timeout).unwrap().basis_t() >= first.basis_t);
    let current = reader.sync_to(second.basis_t, timeout).unwrap();
    assert_eq!(
        current.values(entity, NAME).unwrap(),
        vec![Value::String("Pears".into())]
    );
    assert!(old.values(entity, NAME).unwrap().is_empty());
    assert_eq!(old.basis_t(), schema.basis_t);
    let boundary = current.clone().as_of(first.basis_t);
    assert_eq!(boundary.basis_t(), current.basis_t());
    assert_eq!(boundary.as_of_t(), Some(first.basis_t));
    assert_eq!(
        boundary.values(entity, NAME).unwrap(),
        vec![Value::String("Peaches".into())]
    );
    assert!(first.tx_data.iter().all(|datom| datom.source == 99));
    assert!(second.tx_data.iter().all(|datom| datom.source == 98));

    let local = OperationContext::new(OperationKind::Application);
    {
        let _scope = local.enter();
        assert!(reader.db().basis_t() >= second.basis_t);
        assert!(
            reader
                .sync_to(second.basis_t, Duration::ZERO)
                .unwrap()
                .basis_t()
                >= second.basis_t
        );
        let error = reader
            .sync_to(second.basis_t + 1, Duration::from_millis(20))
            .unwrap_err();
        assert_eq!(error.code, "connection/observation-timeout");
    }
    assert_eq!(
        local.snapshot().calls,
        0,
        "local target waits must not issue storage I/O"
    );

    let executor = AsyncExecutor::new(AsyncConfig {
        workers: 1,
        max_operations: 8,
        max_resources: 8,
    })
    .unwrap();
    let asynchronous = AsyncClient::new(&reader, &executor).unwrap();
    let (entered, started) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    asynchronous.db().unwrap().on_complete(move |result| {
        result.unwrap();
        entered.send(()).unwrap();
        gate.recv_timeout(Duration::from_secs(5)).unwrap();
    });
    started.recv_timeout(timeout).unwrap();
    let queued = OperationContext::new(OperationKind::Application);
    let (fresh, target) = {
        let _scope = queued.enter();
        (
            asynchronous.sync(Some(Duration::from_millis(20))).unwrap(),
            asynchronous
                .sync_to(second.basis_t, Duration::from_millis(20))
                .unwrap(),
        )
    };
    // The channel gate keeps both operations queued through their deadline.
    std::thread::sleep(Duration::from_millis(30));
    release.send(()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        for result in [fresh.await, target.await] {
            assert_eq!(result.unwrap_err().code, "async/deadline");
        }
        assert!(asynchronous.sync(None).unwrap().await.unwrap().basis_t() >= second.basis_t);
        assert!(
            asynchronous
                .sync_to(second.basis_t, timeout)
                .unwrap()
                .await
                .unwrap()
                .basis_t()
                >= second.basis_t
        );
    });
    assert_eq!(
        queued.snapshot().calls,
        0,
        "expired queued sync must not read storage"
    );
    drop(asynchronous);
    drop(executor);
    drop(reader);
    writer.shutdown();
}
