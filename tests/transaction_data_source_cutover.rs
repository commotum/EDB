use edb_core::{
    DB_IDENT, DB_SOURCE_SYSTEM, DB_TX_INSTANT, Database, Datom, EntityRef, Keyword, TxOp, Value,
    t_to_tx,
};

fn timestamp(entity: EntityRef, instant: i64) -> TxOp {
    TxOp::Add {
        entity,
        attribute: DB_TX_INSTANT as u32,
        value: Value::Instant(instant).into(),
    }
}

fn assert_one_timestamp(data: &[Datom], tx: u64, instant: i64, source: u64) {
    let timestamps = data
        .iter()
        .filter(|d| d.attribute == DB_TX_INSTANT as u32)
        .collect::<Vec<_>>();
    assert_eq!(timestamps.len(), 1);
    assert_eq!(timestamps[0].entity, tx);
    assert_eq!(timestamps[0].tx, tx);
    assert_eq!(timestamps[0].value, Value::Instant(instant));
    assert_eq!(timestamps[0].source, source);
    assert!(timestamps[0].added);
    assert!(data.iter().all(|d| d.source == source));
}

#[test]
fn equal_explicit_timestamp_forms_share_one_selected_instant_and_source() {
    let database = Database::bootstrap().unwrap();
    let forms = r#"[{:db/id "edb.tx" :db/txInstant #inst "1970-01-01T00:00:00.010Z"}
        [:db/add "edb.tx" :db/txInstant #inst "1970-01-01T00:00:00.010Z"]
        {:db/id "contributor" :db/ident :source/contributor}]"#;
    {
        let report = database
            .database_value()
            .with_edn(forms, 10, EntityRef::Temp("contributor".into()))
            .unwrap();
        assert_one_timestamp(
            &report.tx_data,
            t_to_tx(report.db_after.basis_t()).unwrap(),
            10,
            report.tempids["contributor"],
        );
    }
    let report = database
        .with_edn(forms, 10, EntityRef::Temp("contributor".into()))
        .unwrap();
    assert_one_timestamp(
        &report.tx_data,
        t_to_tx(report.db_after.basis_t()).unwrap(),
        10,
        report.tempids["contributor"],
    );
    let ops = vec![
        timestamp(EntityRef::from_edn(r#""edb.tx""#).unwrap(), 10),
        timestamp(EntityRef::Tx, 10),
    ];
    let report = database
        .database_value()
        .with(&ops, 10, EntityRef::Id(DB_SOURCE_SYSTEM))
        .unwrap();
    assert_one_timestamp(
        &report.tx_data,
        t_to_tx(report.db_after.basis_t()).unwrap(),
        10,
        DB_SOURCE_SYSTEM,
    );
}

#[test]
fn timestamp_dedup_keeps_conflicts_fixed_time_and_immutable_transaction_checks() {
    let database = Database::bootstrap().unwrap().database_value();
    let source = EntityRef::Id(DB_SOURCE_SYSTEM);
    for (first, second) in [(10, 11), (11, 10)] {
        assert_eq!(
            database
                .with(
                    &[
                        timestamp(EntityRef::Tx, first),
                        timestamp(EntityRef::Tx, second)
                    ],
                    10,
                    source.clone(),
                )
                .unwrap_err()
                .code,
            "transaction/multiple-tx-instants"
        );
    }
    assert_eq!(
        database
            .with(
                &[timestamp(EntityRef::Tx, 11), timestamp(EntityRef::Tx, 11)],
                10,
                source.clone()
            )
            .unwrap_err()
            .code,
        "transaction/tx-instant-mismatch"
    );
    let committed = database.with(&[], 10, source.clone()).unwrap();
    let tx = t_to_tx(committed.db_after.basis_t()).unwrap();
    for op in [
        timestamp(EntityRef::Id(tx), 10),
        TxOp::Retract {
            entity: EntityRef::Id(tx),
            attribute: DB_TX_INSTANT as u32,
            value: Some(Value::Instant(10).into()),
        },
        TxOp::Cas {
            entity: EntityRef::Id(tx),
            attribute: DB_TX_INSTANT as u32,
            old: Some(Value::Instant(10).into()),
            new: Value::Instant(11).into(),
        },
    ] {
        assert_eq!(
            committed
                .db_after
                .with(&[op], 11, source.clone())
                .unwrap_err()
                .code,
            "transaction/reset-tx-instant"
        );
    }
    assert_eq!(
        committed
            .db_after
            .with(
                &[timestamp(EntityRef::Tx, 9), timestamp(EntityRef::Tx, 9)],
                9,
                source
            )
            .unwrap_err()
            .code,
        "transaction/non-monotonic-instant"
    );
}

#[test]
#[ignore = "requires EDB_TEST_POSTGRES_URL for a disposable PostgreSQL database"]
fn durable_equal_explicit_timestamp_forms_keep_override_bounds_and_exact_retry() {
    use edb_core::{
        CapacityLimits, Connection, PostgresConnectionConfig, Schema, TransactionRequest,
        TransactionService, TransactionServiceConfig,
    };
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    let config = PostgresConnectionConfig::parse(
        std::env::var("EDB_TEST_POSTGRES_URL")
            .expect("EDB_TEST_POSTGRES_URL must name a disposable PostgreSQL database"),
    )
    .unwrap();
    let name = format!(
        "transaction-data-{}-{}",
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
    let connection = Connection::attach_configured(config, writer.client(), 64).unwrap();
    let timeout = Duration::from_secs(5);
    let instant = connection.db().last_tx_instant().unwrap_or(10);
    let ops = vec![
        timestamp(EntityRef::from_edn(r#""edb.tx""#).unwrap(), instant),
        timestamp(EntityRef::Tx, instant),
        TxOp::Add {
            entity: EntityRef::Temp("contributor".into()),
            attribute: DB_IDENT as u32,
            value: Value::Keyword(Keyword::new("source", "contributor")).into(),
        },
    ];
    let request = TransactionRequest::new("equal", ops, EntityRef::Temp("contributor".into()))
        .with_tx_instant(instant);
    let report = connection.transact(request.clone(), timeout).unwrap();
    let source = report.tempids["contributor"];
    assert_one_timestamp(
        &report.tx_data,
        t_to_tx(report.basis_t).unwrap(),
        instant,
        source,
    );
    let replay = connection.transact(request, timeout).unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.tx_data, report.tx_data);
    assert_eq!(replay.basis_t, report.basis_t);
    let literal = edb_core::edn::write_edn(
        &edb_core::edn_value::value_to_edn(&Value::Instant(instant)).unwrap(),
    )
    .unwrap();
    let forms = format!(
        r#"[{{:db/id "edb.tx" :db/txInstant {literal}}}
        [:db/add "edb.tx" :db/txInstant {literal}]]"#
    );
    let edn_report = connection
        .transact(
            TransactionRequest::from_edn("edn-equal", &forms, EntityRef::Id(source)).unwrap(),
            timeout,
        )
        .unwrap();
    assert_one_timestamp(
        &edn_report.tx_data,
        t_to_tx(edn_report.basis_t).unwrap(),
        instant,
        source,
    );
    for (key, values, selected, code) in [
        (
            "conflict",
            [instant, instant + 1],
            Some(instant),
            "transaction/multiple-tx-instants",
        ),
        (
            "override-mismatch",
            [instant, instant],
            Some(instant + 1),
            "transaction/tx-instant-mismatch",
        ),
        (
            "past",
            [instant - 1, instant - 1],
            None,
            "transaction/past-tx-instant",
        ),
        (
            "future",
            [i64::MAX, i64::MAX],
            None,
            "transaction/future-tx-instant",
        ),
    ] {
        let mut request = TransactionRequest::new(
            key,
            values
                .into_iter()
                .map(|instant| timestamp(EntityRef::Tx, instant))
                .collect(),
            EntityRef::Id(source),
        );
        request.tx_instant_override = selected;
        assert_eq!(
            connection.transact(request, timeout).unwrap_err().code,
            code
        );
    }
    let immutable = TransactionRequest::new(
        "immutable",
        vec![timestamp(
            EntityRef::Id(t_to_tx(report.basis_t).unwrap()),
            instant,
        )],
        EntityRef::Id(source),
    );
    assert_eq!(
        connection.transact(immutable, timeout).unwrap_err().code,
        "transaction/reset-tx-instant"
    );
    let last_basis = connection.db().basis_t();
    let report = connection
        .transact(
            TransactionRequest::new("after-errors", vec![], EntityRef::Id(source)),
            timeout,
        )
        .unwrap();
    assert_eq!(report.basis_t, last_basis + 1);
    drop(connection);
    writer.shutdown();
}

#[test]
fn ordinary_nested_component_maps_support_documented_entity_depth() {
    use edb_core::{
        Attribute, AttributeRef, Cardinality, EntityMap, MapValue, Schema, TxForm, ValueType,
    };
    let mut schema = Schema::new();
    schema
        .install(
            Attribute::new(
                100,
                Keyword::new("node", "children"),
                ValueType::Ref,
                Cardinality::Many,
            )
            .component(),
        )
        .unwrap();
    let database = Database::new(schema).unwrap().database_value();
    let nested = |depth, many| {
        let mut child = EntityMap {
            id: None,
            attributes: vec![(
                AttributeRef::Id(DB_IDENT as u32),
                MapValue::Value(Value::Keyword(Keyword::new("node", "leaf")).into()),
            )],
        };
        for _ in 0..depth {
            child = EntityMap {
                id: None,
                attributes: vec![(
                    AttributeRef::Id(100),
                    if many {
                        MapValue::Many(vec![MapValue::Nested(Box::new(child))])
                    } else {
                        MapValue::Nested(Box::new(child))
                    },
                )],
            };
        }
        TxForm::EntityMap(child)
    };
    for many in [false, true] {
        let forms = [nested(32, many)];
        let report = database
            .with_forms(&forms, 1, EntityRef::Id(DB_SOURCE_SYSTEM))
            .unwrap();
        assert_eq!(
            report.tx_data.iter().filter(|d| d.attribute == 100).count(),
            32
        );
        assert!(report.tx_data.iter().all(|d| d.source == DB_SOURCE_SYSTEM));
        edb_core::submission_request_digest(&forms, &EntityRef::Id(DB_SOURCE_SYSTEM), None, None)
            .unwrap();
        assert_eq!(
            database
                .with_forms(&[nested(33, many)], 1, EntityRef::Id(DB_SOURCE_SYSTEM))
                .unwrap_err()
                .code,
            "transaction/map-depth"
        );
    }
}
