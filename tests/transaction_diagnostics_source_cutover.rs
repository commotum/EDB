use edb_core::{
    Attribute, AttributeRef, Cardinality, DB_DOC, DB_IDENT, DB_SOURCE_SYSTEM, DB_TX_INSTANT,
    Database, EntityMap, EntityRef, Keyword, MapValue, OperationContext, OperationKind, Schema,
    SpeculationLimits, TransactionWorkStats, TupleSpec, TxCall, TxForm, TxFunctions, TxOp, Value,
    ValueType,
};

fn add(entity: EntityRef, attribute: u32, value: Value) -> TxOp {
    TxOp::Add {
        entity,
        attribute,
        value: value.into(),
    }
}

fn doc(value: &str) -> TxOp {
    add(
        EntityRef::Id(400),
        DB_DOC as u32,
        Value::String(value.into()),
    )
}

fn measured<T>(operation: impl FnOnce() -> T) -> (T, TransactionWorkStats) {
    let context = OperationContext::diagnostic(OperationKind::Transaction);
    let (result, report) = context.measure(operation);
    (result, report.stats.transaction)
}

#[test]
fn opt_in_counters_explain_source_materiality_and_global_withdrawal() {
    let database = Database::new(Schema::default()).unwrap();
    let source = database
        .entid(&Keyword::new("db.source", "system"))
        .unwrap();
    let context = OperationContext::diagnostic(OperationKind::Transaction);
    let (empty, report) = context.measure(|| database.with(&[], 10, EntityRef::Id(source)));
    assert_eq!(report.stats.transaction.assessments, 1);
    assert_eq!(report.stats.transaction.input_operations, 0);
    assert_eq!(
        report.stats.transaction.produced_datoms,
        empty.unwrap().tx_data.len() as u64
    );

    let plain = OperationContext::new(OperationKind::Transaction);
    let (plain_result, report) =
        plain.measure(|| database.with(&[doc("shared")], 10, EntityRef::Id(99)));
    let first = plain_result.unwrap();
    assert_eq!(report.stats.transaction, TransactionWorkStats::default());
    let (repeated, counts) = measured(|| {
        first
            .db_after
            .with(&[doc("shared"), doc("shared")], 11, EntityRef::Id(99))
    });
    let repeated = repeated.unwrap();
    assert_eq!(counts.assessments, 1);
    assert_eq!(counts.input_operations, 2);
    assert_eq!(counts.duplicate_datoms, 1);
    assert_eq!(counts.redundancy_checks, 2);
    assert_eq!(counts.redundant_datoms, 1);
    assert_eq!(counts.produced_datoms, 1);
    assert_eq!(repeated.tx_data.len(), 1);
    let (second, counts) = measured(|| {
        repeated
            .db_after
            .with(&[doc("shared")], 12, EntityRef::Id(98))
    });
    let second = second.unwrap();
    assert_eq!(counts.redundant_datoms, 0);
    assert_eq!(counts.produced_datoms, 2);
    assert_eq!(second.db_after.values(400, DB_DOC as u32).len(), 1);
    let retract = TxOp::Retract {
        entity: EntityRef::Id(400),
        attribute: DB_DOC as u32,
        value: Some(Value::String("shared".into()).into()),
    };
    let (withdrawn, counts) = measured(|| {
        second
            .db_after
            .with(std::slice::from_ref(&retract), 13, EntityRef::Id(97))
    });
    let withdrawn = withdrawn.unwrap();
    assert_eq!(counts.redundancy_checks, 2);
    assert_eq!(counts.redundant_datoms, 0);
    assert_eq!(counts.produced_datoms, 2);
    assert!(withdrawn.tx_data.iter().all(|d| d.source == 97));
    assert_eq!(withdrawn.tx_data.iter().filter(|d| !d.added).count(), 1);
    assert!(withdrawn.db_after.values(400, DB_DOC as u32).is_empty());
    let (missing, counts) = measured(|| withdrawn.db_after.with(&[retract], 14, EntityRef::Id(97)));
    assert_eq!(missing.unwrap().tx_data.len(), 1);
    assert_eq!(counts.redundant_datoms, 1);
    assert_eq!(counts.produced_datoms, 1);
}

#[test]
fn nested_expansion_and_generated_changes_share_source_and_counter_scope() {
    const LEFT: u32 = 500;
    const RIGHT: u32 = 501;
    const PAIR: u32 = 502;
    let mut schema = Schema::new();
    schema
        .install(Attribute::new(
            LEFT,
            Keyword::new("item", "left"),
            ValueType::String,
            Cardinality::One,
        ))
        .unwrap();
    schema
        .install(Attribute::new(
            RIGHT,
            Keyword::new("item", "right"),
            ValueType::String,
            Cardinality::One,
        ))
        .unwrap();
    let mut pair = Attribute::new(
        PAIR,
        Keyword::new("item", "pair"),
        ValueType::Tuple,
        Cardinality::One,
    );
    pair.tuple = Some(TupleSpec::Composite(vec![LEFT, RIGHT]));
    schema.install(pair).unwrap();
    let database = Database::new(schema).unwrap();
    let mut functions = TxFunctions::new();
    functions.register("outer", |_, _| {
        Ok(vec![TxForm::Call(TxCall {
            function: "inner".into(),
            arguments: vec![],
        })])
    });
    functions.register("inner", |_, _| {
        Ok(vec![
            TxForm::EntityMap(EntityMap {
                id: Some(EntityRef::Temp("contributor".into())),
                attributes: vec![(
                    AttributeRef::Id(DB_IDENT as u32),
                    MapValue::Value(Value::Keyword(Keyword::new("source", "counter-proof")).into()),
                )],
            }),
            TxForm::EntityMap(EntityMap {
                id: Some(EntityRef::Id(400)),
                attributes: vec![
                    (
                        AttributeRef::Id(LEFT),
                        MapValue::Value(Value::String("left".into()).into()),
                    ),
                    (
                        AttributeRef::Id(RIGHT),
                        MapValue::Value(Value::String("right".into()).into()),
                    ),
                ],
            }),
            TxForm::Op(add(EntityRef::Tx, DB_TX_INSTANT as u32, Value::Instant(10))),
        ])
    });
    let forms = [TxForm::Call(TxCall {
        function: "outer".into(),
        arguments: vec![],
    })];
    let (result, counts) = measured(|| {
        database.with_forms(
            &forms,
            &functions,
            10,
            EntityRef::Temp("contributor".into()),
        )
    });
    let first = result.unwrap();
    assert_eq!(counts.function_calls, 2);
    assert_eq!(counts.assessments, 1);
    assert_eq!(counts.input_operations, 4);
    assert_eq!(counts.identity_claims, 1);
    assert_eq!(counts.composite_candidates, 1);
    assert_eq!(counts.composite_datoms, 1);
    assert_eq!(counts.duplicate_datoms, 1); // explicit and automatic timestamp
    assert_eq!(counts.produced_datoms, 5);
    assert_eq!(first.tx_data.len(), 5);
    assert!(
        first
            .tx_data
            .iter()
            .all(|d| d.source == first.tempids["contributor"])
    );
    let (replacement, counts) = measured(|| {
        first.db_after.with(
            &[add(
                EntityRef::Id(400),
                LEFT,
                Value::String("changed".into()),
            )],
            11,
            EntityRef::Id(99),
        )
    });
    let replacement = replacement.unwrap();
    assert_eq!(counts.composite_candidates, 1);
    assert_eq!(counts.composite_datoms, 2);
    assert_eq!(counts.produced_datoms, 5);
    assert!(replacement.tx_data.iter().all(|d| d.source == 99));
    assert_eq!(replacement.tx_data.iter().filter(|d| !d.added).count(), 2);

    let context = OperationContext::diagnostic(OperationKind::Transaction);
    let (error, report) = context.measure(|| {
        database.with_forms(
            &[TxForm::Call(TxCall {
                function: "missing".into(),
                arguments: vec![],
            })],
            &functions,
            10,
            EntityRef::Id(99),
        )
    });
    assert_eq!(error.unwrap_err().code, "transaction/unknown-function");
    assert_eq!(report.stats.transaction.function_calls, 1);
    assert_eq!(report.stats.transaction.assessments, 0);
    context
        .measure(|| database.with(&[], 10, EntityRef::Id(99)))
        .0
        .unwrap();
    assert_eq!(context.report().stats.transaction.function_calls, 1);
    assert_eq!(context.report().stats.transaction.assessments, 1);
}

#[test]
fn transaction_read_quota_charges_contributions_before_distinct_value_projection() {
    let first = Database::bootstrap()
        .unwrap()
        .with(&[doc("shared")], 10, EntityRef::Id(99))
        .unwrap();
    let second = first
        .db_after
        .with(&[doc("shared")], 11, EntityRef::Id(98))
        .unwrap();
    let database = second.db_after.database_value();
    assert_eq!(database.values(400, DB_DOC as u32).unwrap().len(), 1);
    let forms = [TxForm::Op(doc("shared"))];
    let limits = SpeculationLimits {
        max_read_datoms: 1,
        ..Default::default()
    };
    let (result, counts) =
        measured(|| database.with_forms_with_limits(&forms, 12, EntityRef::Id(99), limits));
    assert_eq!(result.err().unwrap().code, "transaction/read-capacity");
    assert_eq!(counts.assessments, 1);
    assert_eq!(counts.input_operations, 1);
    assert_eq!(counts.produced_datoms, 0);
    assert_eq!(
        database
            .with_forms_with_limits(&forms, 12, EntityRef::Id(99), limits)
            .err()
            .unwrap()
            .code,
        "transaction/read-capacity"
    );
    let (result, counts) = measured(|| database.with_forms(&forms, 12, EntityRef::Id(99)));
    assert_eq!(result.unwrap().tx_data.len(), 1);
    assert_eq!(counts.redundant_datoms, 1);
    assert_eq!(counts.produced_datoms, 1);
}

#[test]
#[ignore = "requires EDB_TEST_POSTGRES_URL for a disposable PostgreSQL database"]
fn durable_diagnostics_follow_queue_scope_material_work_and_receipt_first_replay() {
    use edb_core::{
        CapacityLimits, PostgresConnectionConfig, ServiceOptions, TelemetryConfig,
        TelemetryPublisher, TransactionRequest, TransactionService, TransactionServiceConfig,
        io_report_to_edn,
    };
    use std::sync::{Arc, mpsc};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    let config = PostgresConnectionConfig::parse(
        std::env::var("EDB_TEST_POSTGRES_URL")
            .expect("EDB_TEST_POSTGRES_URL must name disposable PostgreSQL"),
    )
    .unwrap();
    let name = format!(
        "transaction-diagnostics-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    edb_core::storage::PgBlockStore::install(&config).unwrap();
    edb_core::storage::BlockDatabase::create(&config, &name, Schema::new()).unwrap();
    let (events, received) = mpsc::channel();
    let publisher = TelemetryPublisher::start(
        TelemetryConfig {
            enabled: true,
            ..Default::default()
        },
        move |line| {
            events.send(line.to_owned()).unwrap();
            Ok(())
        },
    )
    .unwrap();
    let service_config = TransactionServiceConfig {
        connection: config,
        database_id: name.clone(),
        holder_id: name,
        lease_duration: Duration::from_secs(10),
        renew_interval: Duration::from_millis(100),
        queue_capacity: 8,
        capacity_limits: CapacityLimits::default(),
    };
    let writer = TransactionService::start_with_options(
        service_config,
        ServiceOptions {
            telemetry: Some(publisher.emitter()),
            ..Default::default()
        },
    )
    .unwrap();
    let client = writer.client();
    let timeout = Duration::from_secs(5);
    let described = client
        .transact(
            TransactionRequest::new(
                "source-description",
                vec![add(
                    EntityRef::Temp("source".into()),
                    DB_IDENT as u32,
                    Value::Keyword(Keyword::new("source", "diagnostics-proof")),
                )],
                EntityRef::Id(DB_SOURCE_SYSTEM),
            ),
            timeout,
        )
        .unwrap();
    received.recv_timeout(timeout).unwrap();
    let source = described.tempids["source"];
    let request = TransactionRequest::new(
        "private-request-key",
        vec![add(
            EntityRef::Temp("item".into()),
            DB_DOC as u32,
            Value::String("private-domain-value".into()),
        )],
        EntityRef::Ident(Keyword::new("source", "diagnostics-proof")),
    );
    let context = OperationContext::named(
        OperationKind::Transaction,
        Keyword::new("billing", "update-account"),
    )
    .unwrap();
    let ticket = {
        let _scope = context.enter();
        client.submit(request.clone()).unwrap()
    };
    let first = ticket.wait(timeout).unwrap();
    let diagnostic = first.diagnostics.as_ref().unwrap();
    assert_eq!(diagnostic.basis_t, first.basis_t);
    assert!(!diagnostic.replayed);
    assert_eq!(
        diagnostic.report.context,
        Some(Keyword::new("billing", "update-account"))
    );
    assert_eq!(diagnostic.report.stats.transaction.assessments, 1);
    assert_eq!(
        diagnostic.report.stats.transaction.produced_datoms,
        first.tx_data.len() as u64
    );
    assert!(Arc::ptr_eq(
        first.clone().diagnostics.as_ref().unwrap(),
        diagnostic
    ));
    for phase in [
        OperationKind::TransactionExpansion,
        OperationKind::TransactionAssessment,
        OperationKind::TransactionEncoding,
        OperationKind::TransactionCommit,
        OperationKind::TransactionReport,
    ] {
        assert_eq!(diagnostic.report.stats.phases[&phase].invocations, 1);
    }
    assert!(diagnostic.report.stats.sql_calls > 0);
    let event = received.recv_timeout(timeout).unwrap();
    assert!(event.contains("\"transaction.produced_datoms\":2"));
    assert!(event.contains("\"context\":\":billing/update-account\""));
    assert!(!event.contains("private-request-key"));
    assert!(!event.contains("private-domain-value"));
    let edn = io_report_to_edn(&diagnostic.report);
    let field = |fields: &[(edb_core::edn::EdnValue, edb_core::edn::EdnValue)], name: &str| {
        fields.iter().find(|(key, _)| {
            matches!(key, edb_core::edn::EdnValue::Keyword(key) if key.qualified_name() == name)
        }).unwrap().1.clone()
    };
    let edb_core::edn::EdnValue::Map(fields) = &edn else {
        panic!("diagnostic report must be a map");
    };
    let edb_core::edn::EdnValue::Map(metrics) = field(fields, "metrics") else {
        panic!("diagnostic metrics must be a map");
    };
    assert_eq!(
        field(&metrics, "transaction.produced_datoms"),
        edb_core::edn::EdnValue::Long(2)
    );
    assert!(!format!("{edn:?}").contains("private-domain-value"));
    let entity = first.tempids["item"];
    let contribution = |key, contributor| {
        TransactionRequest::new(
            key,
            vec![add(
                EntityRef::Id(entity),
                DB_DOC as u32,
                Value::String("private-domain-value".into()),
            )],
            EntityRef::Id(contributor),
        )
    };
    let second = client
        .transact(
            contribution("second-contribution", DB_SOURCE_SYSTEM),
            timeout,
        )
        .unwrap();
    assert_eq!(
        second
            .diagnostics
            .as_ref()
            .unwrap()
            .report
            .stats
            .transaction
            .produced_datoms,
        2
    );
    received.recv_timeout(timeout).unwrap();
    let repeated = client
        .transact(contribution("redundant", source), timeout)
        .unwrap();
    let stats = &repeated
        .diagnostics
        .as_ref()
        .unwrap()
        .report
        .stats
        .transaction;
    assert_eq!(stats.redundant_datoms, 1);
    assert_eq!(stats.produced_datoms, 1);
    let read = client.writer_residency_stats();
    assert!(read.last_transaction_prefix_memo_hits > 0);
    assert!(read.last_transaction_read_datoms > read.last_transaction_source_read_datoms);
    assert!(read.last_transaction_source_read_datoms >= 2);
    received.recv_timeout(timeout).unwrap();
    client
        .transact(
            TransactionRequest::new(
                "withdraw-source-ident",
                vec![TxOp::Retract {
                    entity: EntityRef::Id(source),
                    attribute: DB_IDENT as u32,
                    value: None,
                }],
                EntityRef::Id(DB_SOURCE_SYSTEM),
            ),
            timeout,
        )
        .unwrap();
    received.recv_timeout(timeout).unwrap();
    let replay = client.transact(request, timeout).unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.tx_data, first.tx_data);
    assert_eq!(replay.tx_hash, first.tx_hash);
    let replay_diagnostic = replay.diagnostics.as_ref().unwrap();
    assert!(replay_diagnostic.replayed);
    assert_eq!(replay_diagnostic.basis_t, first.basis_t);
    assert_eq!(
        replay_diagnostic.report.stats.transaction,
        TransactionWorkStats::default()
    );
    for phase in [
        OperationKind::TransactionExpansion,
        OperationKind::TransactionAssessment,
        OperationKind::TransactionEncoding,
        OperationKind::TransactionCommit,
    ] {
        assert!(!replay_diagnostic.report.stats.phases.contains_key(&phase));
    }
    assert_eq!(
        replay_diagnostic.report.stats.phases[&OperationKind::TransactionReport].invocations,
        1
    );
    let event = received.recv_timeout(timeout).unwrap();
    assert!(event.contains("\"replayed\":true"));
    assert!(event.contains("\"transaction.assessments\":0"));
    writer.shutdown();
    assert!(publisher.shutdown(timeout));
}
