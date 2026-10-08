use edb_core::edn_query::{EdnQueryArgument, parse_query_edn};
use edb_core::{
    Attribute, Cardinality, Database, EntityRef, Keyword, QueryControl, QueryDataSource,
    QueryResult, QuerySourceValue, QueryValue, Schema, TxOp, Value, ValueType,
};

const NAME: u32 = 1_000;

fn add(entity: EntityRef, text: &str) -> TxOp {
    TxOp::Add {
        entity,
        attribute: NAME,
        value: Value::String(text.into()).into(),
    }
}

#[test]
fn documented_report_query_joins_names_and_preserves_retractions() {
    let mut schema = Schema::new();
    schema
        .install(Attribute::new(
            NAME,
            Keyword::new("item", "name"),
            ValueType::String,
            Cardinality::One,
        ))
        .unwrap();
    let first = Database::new(schema)
        .unwrap()
        .with(&[add(EntityRef::Id(42), "Peaches")], 1, EntityRef::Id(99))
        .unwrap();
    let replacement = first
        .db_after
        .database_value()
        .with(&[add(EntityRef::Id(42), "Pears")], 2, EntityRef::Id(98))
        .unwrap();
    let page = include_str!("../docs-2/04_transactions/06_processing_transactions.md");
    let query = page
        .split("```edn\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    let raw = QueryDataSource::datoms("$tx", replacement.tx_data.clone()).value;
    let QuerySourceValue::Tuples(tuples) = &raw else {
        panic!("datoms must expose raw tuples");
    };
    for (tuple, datom) in tuples.iter().zip(&replacement.tx_data) {
        assert_eq!(
            tuple,
            &vec![
                Value::Ref(datom.entity),
                Value::Ref(u64::from(datom.attribute)),
                datom.value.clone(),
                Value::Ref(datom.tx),
                Value::Ref(98),
                Value::Bool(datom.added),
            ]
        );
    }
    let result = parse_query_edn(query)
        .unwrap()
        .bind(&[
            EdnQueryArgument::Source(QuerySourceValue::Database(replacement.db_after.clone())),
            EdnQueryArgument::Source(raw),
        ])
        .unwrap()
        .execute(&QueryControl::default(), None)
        .unwrap()
        .result;
    let QueryResult::Relation(rows) = result else {
        panic!("expected relation");
    };
    let expected = replacement
        .tx_data
        .iter()
        .map(|datom| {
            vec![
                QueryValue::Scalar(Value::Ref(datom.entity)),
                QueryValue::Scalar(Value::Keyword(if datom.attribute == NAME {
                    Keyword::new("item", "name")
                } else {
                    Keyword::new("db", "txInstant")
                })),
                QueryValue::Scalar(datom.value.clone()),
                QueryValue::Scalar(Value::Bool(datom.added)),
            ]
        })
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 3);
    assert!(expected.iter().all(|row| rows.contains(row)));
    assert_eq!(
        replacement.db_after.values(42, NAME).unwrap(),
        vec![Value::String("Pears".into())]
    );
    assert!(rows.iter().any(
        |row| row[2] == QueryValue::Scalar(Value::String("Peaches".into()))
            && row[3] == QueryValue::Scalar(Value::Bool(false))
    ));
}

#[test]
#[ignore = "requires EDB_TEST_POSTGRES_URL for a disposable PostgreSQL database"]
fn independent_connection_report_queue_preserves_exact_source_reports_and_removal() {
    use edb_core::{
        CapacityLimits, Connection, DB_SOURCE_SYSTEM, PostgresConnectionConfig, TransactionRequest,
        TransactionService, TransactionServiceConfig,
    };
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    let config = PostgresConnectionConfig::parse(
        std::env::var("EDB_TEST_POSTGRES_URL")
            .expect("EDB_TEST_POSTGRES_URL must name a disposable PostgreSQL database"),
    )
    .unwrap();
    let name = format!(
        "processing-transactions-{}-{}",
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
    let observer = Connection::connect_configured(config, &name, 64).unwrap();
    observer.sync_to(schema.basis_t, timeout).unwrap();
    assert!(observer.enable_transaction_reports());
    assert!(!observer.enable_transaction_reports());
    assert!(observer.try_next_transaction_report().is_none());

    let first_request = TransactionRequest::new(
        "first",
        vec![add(EntityRef::Temp("item".into()), "Peaches")],
        EntityRef::Id(99),
    );
    let first = client.transact(first_request.clone(), timeout).unwrap();
    let entity = first.tempids["item"];
    let second = client
        .transact(
            TransactionRequest::new(
                "second",
                vec![add(EntityRef::Id(entity), "Pears")],
                EntityRef::Id(98),
            ),
            timeout,
        )
        .unwrap();
    observer.sync_to(second.basis_t, timeout).unwrap();
    for (expected, source) in [(&first, 99), (&second, 98)] {
        let monitored = observer.next_transaction_report(timeout).unwrap().unwrap();
        assert_eq!(monitored.basis_t, expected.basis_t);
        assert_eq!(monitored.tx_hash, expected.tx_hash);
        assert_eq!(monitored.tx_data, expected.tx_data);
        assert_eq!(monitored.tempids, expected.tempids);
        assert!(!monitored.replayed);
        assert!(monitored.diagnostics.is_none());
        assert_eq!(monitored.db_before.basis_t(), expected.basis_t - 1);
        assert_eq!(monitored.db_after.basis_t(), expected.basis_t);
        assert_eq!(
            monitored.db_before.values(entity, NAME).unwrap(),
            expected.db_before.values(entity, NAME).unwrap()
        );
        assert_eq!(
            monitored.db_after.values(entity, NAME).unwrap(),
            expected.db_after.values(entity, NAME).unwrap()
        );
        assert!(monitored.tx_data.iter().all(|datom| datom.source == source));
    }
    assert!(observer.try_next_transaction_report().is_none());
    assert!(client.transact(first_request, timeout).unwrap().replayed);
    observer.sync().unwrap();
    assert!(observer.try_next_transaction_report().is_none());

    let pending = client
        .transact(
            TransactionRequest::new("pending", vec![], EntityRef::Id(97)),
            timeout,
        )
        .unwrap();
    observer.sync_to(pending.basis_t, timeout).unwrap();
    assert!(observer.remove_transaction_report_queue());
    assert!(!observer.remove_transaction_report_queue());
    assert!(
        observer
            .next_transaction_report(Duration::ZERO)
            .unwrap()
            .is_none()
    );
    assert!(observer.enable_transaction_reports());
    assert!(observer.try_next_transaction_report().is_none());
    let next = client
        .transact(
            TransactionRequest::new("next", vec![], EntityRef::Id(96)),
            timeout,
        )
        .unwrap();
    observer.sync_to(next.basis_t, timeout).unwrap();
    let monitored = observer.next_transaction_report(timeout).unwrap().unwrap();
    assert_eq!(monitored.basis_t, next.basis_t);
    assert_eq!(monitored.tx_data, next.tx_data);
    assert!(monitored.tx_data.iter().all(|datom| datom.source == 96));
    assert!(observer.try_next_transaction_report().is_none());
    observer.remove_transaction_report_queue();
    drop(observer);
    writer.shutdown();
}
