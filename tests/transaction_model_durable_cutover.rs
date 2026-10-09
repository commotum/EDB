use edb_core::{
    CapacityLimits, Connection, DB_SOURCE_SYSTEM, EntityRef, PostgresConnectionConfig, Schema,
    TransactionRequest, TransactionService, TransactionServiceConfig,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn test_config() -> PostgresConnectionConfig {
    PostgresConnectionConfig::parse(
        std::env::var("EDB_TEST_POSTGRES_URL")
            .expect("EDB_TEST_POSTGRES_URL must name a disposable PostgreSQL database"),
    )
    .unwrap()
}

#[test]
#[ignore = "requires EDB_TEST_POSTGRES_URL for a disposable PostgreSQL database"]
fn durable_transaction_model_observes_empty_source_transactions() {
    let config = test_config();
    let name = format!(
        "transaction-model-{}-{}",
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
        queue_capacity: 32,
        capacity_limits: CapacityLimits::default(),
    })
    .unwrap();
    let connection = Connection::attach_configured(config.clone(), writer.client(), 64).unwrap();
    let reader = Connection::connect_configured(config, &name, 64).unwrap();
    for n in 0..5 {
        let report = connection
            .transact(
                TransactionRequest::new(
                    format!("empty-{n}"),
                    vec![],
                    EntityRef::Id(DB_SOURCE_SYSTEM),
                ),
                Duration::from_secs(3),
            )
            .unwrap();
        assert!(report.tx_data.iter().all(|d| d.source == DB_SOURCE_SYSTEM));
        assert_eq!(report.tx_data.len(), 1);
        assert_eq!(report.tx_data[0].attribute, edb_core::DB_TX_INSTANT as u32);
        assert_eq!(
            report.tx_data[0].entity,
            edb_core::t_to_tx(report.basis_t).unwrap()
        );
        assert!(report.tx_data[0].added);
        let observed = reader.sync_to(report.basis_t, Duration::from_secs(3));
        assert!(
            observed.is_ok(),
            "observed={observed:?}; background={:?}; writer-observation={:?}",
            reader.observation_error(),
            connection.observation_error()
        );
        assert_eq!(
            connection.db().basis_t(),
            report.basis_t,
            "{:?}",
            connection.observation_error()
        );
    }
    drop(connection);
    writer.shutdown();
}

#[test]
#[ignore = "requires EDB_TEST_POSTGRES_URL for a disposable PostgreSQL database"]
fn durable_transaction_model_preserves_attribution_and_receipt_first_retry() {
    use edb_core::{
        Attribute, Cardinality, DB_IDENT, DB_TX_INSTANT, IndexOrder, IndexPrefix, Keyword, TxOp,
        Unique, Value, ValueType,
    };
    const NAME: u32 = 1_000;
    const SOURCE_KEY: u32 = 1_001;
    const CHILD: u32 = 1_002;
    let config = test_config();
    let name = format!(
        "transaction-model-parity-{}-{}",
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
        queue_capacity: 32,
        capacity_limits: CapacityLimits::default(),
    })
    .unwrap();
    let connection = Connection::attach_configured(config.clone(), writer.client(), 64).unwrap();
    let timeout = Duration::from_secs(5);
    let submit = |key: &str, ops: Vec<TxOp>, source: EntityRef| {
        connection
            .transact(TransactionRequest::new(key, ops, source), timeout)
            .unwrap()
    };
    let add = |entity: EntityRef, attribute, value: Value| TxOp::Add {
        entity,
        attribute,
        value: value.into(),
    };
    submit(
        "schema",
        vec![
            TxOp::InstallAttribute(Attribute::new(
                NAME,
                Keyword::new("item", "name"),
                ValueType::String,
                Cardinality::One,
            )),
            TxOp::InstallAttribute(
                Attribute::new(
                    SOURCE_KEY,
                    Keyword::new("source", "key"),
                    ValueType::String,
                    Cardinality::One,
                )
                .unique(Unique::Identity),
            ),
            TxOp::InstallAttribute(
                Attribute::new(
                    CHILD,
                    Keyword::new("item", "child"),
                    ValueType::Ref,
                    Cardinality::One,
                )
                .component(),
            ),
        ],
        EntityRef::Id(DB_SOURCE_SYSTEM),
    );
    let described = submit(
        "sources",
        vec![
            add(
                EntityRef::Temp("first".into()),
                DB_IDENT as u32,
                Value::Keyword(Keyword::new("source", "first")),
            ),
            add(
                EntityRef::Temp("first".into()),
                SOURCE_KEY,
                Value::String("first".into()),
            ),
            add(
                EntityRef::Temp("second".into()),
                DB_IDENT as u32,
                Value::Keyword(Keyword::new("source", "second")),
            ),
            add(
                EntityRef::Temp("second".into()),
                SOURCE_KEY,
                Value::String("second".into()),
            ),
        ],
        EntityRef::Temp("first".into()),
    );
    let first_source = described.tempids["first"];
    let second_source = described.tempids["second"];
    assert!(described.tx_data.iter().all(|d| d.source == first_source));
    let nested = connection.transact(
        TransactionRequest::from_edn(
            "nested",
            r#"[{:db/id "parent" :item/name "Parent" :item/child {:db/id "child" :item/name "Child"}}]"#,
            EntityRef::Id(first_source),
        ).unwrap(),
        timeout,
    ).unwrap();
    assert!(nested.tx_data.iter().all(|d| d.source == first_source));
    let cascade = submit(
        "cascade",
        vec![TxOp::RetractEntity(EntityRef::Id(nested.tempids["parent"]))],
        EntityRef::Id(second_source),
    );
    assert!(cascade.tx_data.iter().all(|d| d.source == second_source));
    assert_eq!(cascade.tx_data.iter().filter(|d| !d.added).count(), 3);
    let first_request = TransactionRequest::new(
        "first-contribution",
        vec![add(
            EntityRef::Temp("item".into()),
            NAME,
            Value::String("Peaches".into()),
        )],
        EntityRef::Ident(Keyword::new("source", "first")),
    );
    let first = connection.transact(first_request.clone(), timeout).unwrap();
    let entity = first.tempids["item"];
    assert!(first.tx_data.iter().all(|d| d.source == first_source));
    let lookup_request = TransactionRequest::new(
        "second-contribution",
        vec![add(
            EntityRef::Id(entity),
            NAME,
            Value::String("Peaches".into()),
        )],
        EntityRef::Lookup {
            attribute: SOURCE_KEY,
            value: Value::String("second".into()),
        },
    );
    let second = connection
        .transact(lookup_request.clone(), timeout)
        .unwrap();
    assert!(second.tx_data.iter().all(|d| d.source == second_source));
    let live = second
        .db_after
        .collect_datoms(IndexOrder::Eavt)
        .unwrap()
        .into_iter()
        .filter(|d| d.entity == entity && d.attribute == NAME)
        .collect::<Vec<_>>();
    assert_eq!(live.len(), 2);
    assert_eq!(
        second.db_after.values(entity, NAME).unwrap(),
        vec![Value::String("Peaches".into())]
    );
    let indexed = connection.request_index().unwrap();
    connection.sync_index(indexed.target_t, timeout).unwrap();
    assert_eq!(
        connection
            .db()
            .collect_datoms(IndexOrder::Eavt)
            .unwrap()
            .into_iter()
            .filter(|d| d.entity == entity && d.attribute == NAME)
            .count(),
        2
    );
    let redundant = submit(
        "redundant",
        vec![add(
            EntityRef::Id(entity),
            NAME,
            Value::String("Peaches".into()),
        )],
        EntityRef::Id(second_source),
    );
    assert!(!redundant.tx_data.iter().any(|d| d.entity == entity));
    assert_eq!(redundant.tx_data.len(), 1);
    assert_eq!(redundant.tx_data[0].attribute, DB_TX_INSTANT as u32);
    let cas = submit(
        "cas",
        vec![TxOp::Cas {
            entity: EntityRef::Id(entity),
            attribute: NAME,
            old: Some(Value::String("Peaches".into()).into()),
            new: Value::String("Pears".into()).into(),
        }],
        EntityRef::Id(DB_SOURCE_SYSTEM),
    );
    assert!(cas.tx_data.iter().all(|d| d.source == DB_SOURCE_SYSTEM));
    assert_eq!(
        cas.tx_data
            .iter()
            .filter(|d| d.entity == entity && !d.added)
            .count(),
        1
    );
    assert_eq!(
        cas.db_after.values(entity, NAME).unwrap(),
        vec![Value::String("Pears".into())]
    );
    let prefix = IndexPrefix::Saevt {
        source: first_source,
        attribute: Some(NAME),
        entity: None,
        value: None,
    };
    assert!(
        cas.db_after
            .collect_datoms_with_prefix(&prefix)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        cas.db_after
            .as_of(second.basis_t)
            .collect_datoms_with_prefix(&prefix)
            .unwrap()
            .len(),
        1
    );
    submit(
        "retract-ident",
        vec![TxOp::Retract {
            entity: EntityRef::Id(first_source),
            attribute: DB_IDENT as u32,
            value: Some(Value::Keyword(Keyword::new("source", "first")).into()),
        }],
        EntityRef::Id(DB_SOURCE_SYSTEM),
    );
    submit(
        "retract-lookup",
        vec![TxOp::Retract {
            entity: EntityRef::Id(second_source),
            attribute: SOURCE_KEY,
            value: Some(Value::String("second".into()).into()),
        }],
        EntityRef::Id(DB_SOURCE_SYSTEM),
    );
    let basis = connection.db().basis_t();
    for (request, original) in [
        (first_request.clone(), &first),
        (lookup_request.clone(), &second),
    ] {
        let replay = connection.transact(request, timeout).unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.basis_t, original.basis_t);
        assert_eq!(replay.tx_hash, original.tx_hash);
        assert_eq!(replay.tx_data, original.tx_data);
        assert_eq!(replay.tempids, original.tempids);
    }
    assert_eq!(connection.db().basis_t(), basis);
    let mut changed_intent = first_request;
    changed_intent.source = EntityRef::Id(DB_SOURCE_SYSTEM);
    assert_eq!(
        connection
            .transact(changed_intent, timeout)
            .unwrap_err()
            .category,
        edb_core::ErrorCategory::Conflict
    );
    submit(
        "corroborate-replacement",
        vec![add(
            EntityRef::Id(entity),
            NAME,
            Value::String("Pears".into()),
        )],
        EntityRef::Id(first_source),
    );
    assert_eq!(
        connection
            .db()
            .collect_datoms(IndexOrder::Eavt)
            .unwrap()
            .into_iter()
            .filter(|d| d.entity == entity && d.attribute == NAME)
            .count(),
        2
    );
    let withdrawn = submit(
        "withdraw",
        vec![TxOp::Retract {
            entity: EntityRef::Id(entity),
            attribute: NAME,
            value: None,
        }],
        EntityRef::Id(second_source),
    );
    assert!(withdrawn.tx_data.iter().all(|d| d.source == second_source));
    assert!(withdrawn.db_after.values(entity, NAME).unwrap().is_empty());
    assert!(
        withdrawn
            .db_after
            .collect_datoms_with_prefix(&prefix)
            .unwrap()
            .is_empty()
    );
    let indexed = connection.request_index().unwrap();
    connection.sync_index(indexed.target_t, timeout).unwrap();
    let final_basis = withdrawn.basis_t;
    let expected_history = withdrawn
        .db_after
        .history()
        .collect_datoms(IndexOrder::Eavt)
        .unwrap();
    drop(connection);
    writer.shutdown();
    let reopened = Connection::connect_configured(config, &name, 64).unwrap();
    assert_eq!(reopened.db().basis_t(), final_basis);
    assert_eq!(
        reopened
            .db()
            .history()
            .collect_datoms(IndexOrder::Eavt)
            .unwrap(),
        expected_history
    );
    assert!(
        reopened
            .db()
            .collect_datoms_with_prefix(&prefix)
            .unwrap()
            .is_empty()
    );
}
