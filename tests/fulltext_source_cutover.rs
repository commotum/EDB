use edb_core::{
    Attribute, Cardinality, Database, DatabaseValue, EntityRef, FulltextOptions, Keyword, Schema,
    TxOp, TxValue, Value, ValueType,
};

const TEXT: u32 = 100;
const PRIVATE: u32 = 101;
const PHRASE: &str = "Blue River";

fn schema() -> Schema {
    let mut schema = Schema::new();
    schema
        .install(
            Attribute::new(
                TEXT,
                Keyword::new("article", "text"),
                ValueType::String,
                Cardinality::Many,
            )
            .fulltext(),
        )
        .unwrap();
    let mut private = Attribute::new(
        PRIVATE,
        Keyword::new("article", "private"),
        ValueType::String,
        Cardinality::Many,
    )
    .fulltext();
    private.no_history = true;
    schema.install(private).unwrap();
    schema
}
fn add(attribute: u32, text: &str) -> TxOp {
    TxOp::Add {
        entity: EntityRef::Id(42),
        attribute,
        value: TxValue::Scalar(Value::String(text.into())),
    }
}
fn retract(attribute: u32) -> TxOp {
    TxOp::Retract {
        entity: EntityRef::Id(42),
        attribute,
        value: Some(TxValue::Scalar(Value::String(PHRASE.into()))),
    }
}
fn hits(db: &DatabaseValue, attribute: u32) -> Vec<(u64, String, u64)> {
    let report = db
        .fulltext(attribute, "\"blue river\"", &FulltextOptions::default())
        .unwrap();
    assert!(
        report
            .hits
            .iter()
            .all(|hit| hit.score.is_finite() && hit.score > 0.0)
    );
    report
        .hits
        .into_iter()
        .map(|h| (h.entity, h.value, h.tx))
        .collect()
}
fn assertion_tx(db: &DatabaseValue, attribute: u32, source: u64) -> u64 {
    db.clone()
        .history()
        .collect_datoms(edb_core::IndexOrder::Eavt)
        .unwrap()
        .into_iter()
        .find(|d| d.entity == 42 && d.attribute == attribute && d.added && d.source == source)
        .unwrap()
        .tx
}

#[test]
fn fulltext_current_history_time_and_filters_keep_distinct_contributions() {
    let first = Database::new(schema())
        .unwrap()
        .with(&[add(TEXT, PHRASE)], 1, EntityRef::Id(99))
        .unwrap();
    let t1 = first.db_after.basis_t();
    let second = first
        .db_after
        .with(&[add(TEXT, PHRASE)], 2, EntityRef::Id(98))
        .unwrap();
    let t2 = second.db_after.basis_t();
    let live = second.db_after.database_value();
    let tx1 = assertion_tx(&live, TEXT, 99);
    let tx2 = assertion_tx(&live, TEXT, 98);
    assert_eq!(
        hits(&live, TEXT),
        vec![(42, PHRASE.into(), tx1), (42, PHRASE.into(), tx2)]
    );
    assert_eq!(
        hits(&live.clone().filter(|_, d| d.source == 99), TEXT),
        vec![(42, PHRASE.into(), tx1)]
    );
    assert_eq!(
        hits(&live.clone().since(t1), TEXT),
        vec![(42, PHRASE.into(), tx2)]
    );
    let repeated = live
        .with(&[add(TEXT, PHRASE)], 3, EntityRef::Id(99))
        .unwrap()
        .db_after;
    assert_eq!(hits(&repeated, TEXT), hits(&live, TEXT));
    let withdrawn = repeated
        .with(&[retract(TEXT)], 4, EntityRef::Id(97))
        .unwrap()
        .db_after;
    for view in [
        withdrawn.clone(),
        withdrawn.clone().since(t1),
        withdrawn.clone().filter(|_, d| d.source == 99),
        withdrawn.clone().filter(|_, d| d.source != 97),
    ] {
        assert!(hits(&view, TEXT).is_empty());
    }
    assert_eq!(hits(&withdrawn.clone().as_of(t2), TEXT), hits(&live, TEXT));
    assert_eq!(hits(&withdrawn.clone().history(), TEXT), hits(&live, TEXT));
    assert_eq!(
        hits(
            &withdrawn.clone().history().filter(|_, d| d.source == 99),
            TEXT
        ),
        vec![(42, PHRASE.into(), tx1)]
    );
    assert!(hits(&withdrawn.clone().history().since(t2), TEXT).is_empty());
    let revived = withdrawn
        .with(&[add(TEXT, PHRASE)], 5, EntityRef::Id(98))
        .unwrap()
        .db_after;
    assert_eq!(hits(&revived, TEXT).len(), 1);
    assert_eq!(hits(&revived.clone().history(), TEXT).len(), 3);
    assert!(hits(&revived.filter(|_, d| d.source == 99), TEXT).is_empty());
}

#[test]
fn fulltext_relation_preserves_four_columns_and_joins_attributed_facts() {
    use edb_core::{
        Binding, Clause, DataPattern, FindElement, FindSpec, Function, Query, QueryControl,
        QueryDataSource, QueryEngine, QueryResult, QueryValue, Term,
    };
    let live = Database::new(schema())
        .unwrap()
        .with(&[add(TEXT, PHRASE)], 1, EntityRef::Id(99))
        .unwrap()
        .db_after
        .with(&[add(TEXT, PHRASE)], 2, EntityRef::Id(98))
        .unwrap()
        .db_after
        .database_value();
    let mut pattern = DataPattern::new(
        Term::var("e"),
        Term::Constant(Value::Ref(u64::from(TEXT))),
        Term::var("v"),
    );
    pattern.source = "$articles".into();
    pattern.transaction = Some(Term::var("tx"));
    pattern.fact_source = Some(Term::var("s"));
    let query = Query::new(
        FindSpec::Relation(vec![FindElement::Variable("s".into())]),
        vec![
            Clause::Function {
                function: Function::Fulltext,
                source: "$articles".into(),
                args: vec![
                    Term::Constant(Value::Ref(u64::from(TEXT))),
                    Term::Constant(Value::String("blue".into())),
                ],
                binding: Binding::Relation(vec![
                    Some("e".into()),
                    Some("v".into()),
                    Some("tx".into()),
                    Some("score".into()),
                ]),
            },
            Clause::Pattern(Box::new(pattern)),
        ],
    );
    let execute = |db: DatabaseValue| {
        QueryEngine::execute_sources(
            &query,
            &[QueryDataSource::database("$articles", db)],
            &[],
            &QueryControl::default(),
        )
        .unwrap()
        .result
    };
    assert_eq!(
        execute(live.clone()),
        QueryResult::Relation(vec![
            vec![QueryValue::Scalar(Value::Ref(98))],
            vec![QueryValue::Scalar(Value::Ref(99))]
        ])
    );
    assert_eq!(
        execute(live.clone().filter(|_, d| d.source == 99)),
        QueryResult::Relation(vec![vec![QueryValue::Scalar(Value::Ref(99))]])
    );
    let withdrawn = live
        .with(&[retract(TEXT)], 3, EntityRef::Id(97))
        .unwrap()
        .db_after;
    assert_eq!(
        execute(withdrawn.clone().filter(|_, d| d.source != 97)),
        QueryResult::Relation(vec![])
    );
    assert_eq!(
        execute(withdrawn.history()),
        QueryResult::Relation(vec![
            vec![QueryValue::Scalar(Value::Ref(98))],
            vec![QueryValue::Scalar(Value::Ref(99))]
        ])
    );
}

#[test]
#[ignore = "requires explicit isolated cutover PostgreSQL configuration"]
fn durable_fulltext_source_roundtrip_recent_tail_and_nohistory_removal() {
    use edb_core::{
        CapacityLimits, Connection, PostgresConnectionConfig, TransactionRequest,
        TransactionService, TransactionServiceConfig,
    };
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    let url = std::env::var("EDB_TEST_POSTGRES_URL")
        .expect("set explicit disposable EDB_TEST_POSTGRES_URL");
    let config = PostgresConnectionConfig::plaintext(url);
    let name = format!(
        "fulltext-source-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    edb_core::storage::PgBlockStore::install(&config).unwrap();
    edb_core::storage::BlockDatabase::create(&config, &name, schema()).unwrap();
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
    let timeout = Duration::from_secs(10);
    let submit = |key: &str, operations, source| {
        connection
            .transact(
                TransactionRequest::new(key, operations, EntityRef::Id(source)),
                timeout,
            )
            .unwrap()
    };
    let index = || {
        let target = connection.request_index().unwrap();
        connection.sync_index(target.target_t, timeout).unwrap();
        connection.db()
    };
    let first = submit("first", vec![add(TEXT, PHRASE), add(PRIVATE, PHRASE)], 99);
    let first_indexed = index();
    let reader = first_indexed.native_fulltext_reader().unwrap().unwrap();
    let second = submit("second", vec![add(TEXT, PHRASE), add(PRIVATE, PHRASE)], 98);
    let live = connection.db();
    assert_eq!(hits(&live, TEXT).len(), 2);
    assert_eq!(
        hits(&live.clone().filter(|_, d| d.source == 99), TEXT).len(),
        1
    );
    assert_eq!(
        hits(&live.clone().filter(|_, d| d.source == 98), TEXT).len(),
        1
    );
    assert_eq!(reader.source_basis_t(), first_indexed.basis_t());
    let speculative_instant = live.last_tx_instant().unwrap();
    let speculative = live
        .with(&[retract(TEXT)], speculative_instant, EntityRef::Id(97))
        .unwrap()
        .db_after;
    assert!(hits(&speculative.clone().filter(|_, d| d.source == 99), TEXT).is_empty());
    assert_eq!(hits(&speculative.clone().history(), TEXT).len(), 2);
    assert_eq!(hits(&live, TEXT).len(), 2);
    let persisted = index();
    let projection = persisted.native_fulltext_reader().unwrap().unwrap();
    // Decode source attribution directly from persisted document payloads.
    let mut prefix = TEXT.to_be_bytes().to_vec();
    prefix.push(0);
    let records = projection
        .prefix(&prefix, edb_core::FulltextReadLimits::default())
        .unwrap()
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    let sources = records
        .iter()
        .map(|r| u64::from_be_bytes(r.value[4..12].try_into().unwrap()))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(sources, [98, 99].into_iter().collect());
    assert_eq!(hits(&persisted, TEXT), hits(&live, TEXT));
    assert_eq!(hits(&persisted.clone().since(first.basis_t), TEXT).len(), 1);
    submit("withdraw", vec![retract(TEXT), retract(PRIVATE)], 97);
    let withdrawn = connection.db();
    for view in [
        withdrawn.clone(),
        withdrawn.clone().filter(|_, d| d.source == 99),
        withdrawn.clone().filter(|_, d| d.source != 97),
    ] {
        assert!(hits(&view, TEXT).is_empty());
        assert!(hits(&view, PRIVATE).is_empty());
    }
    assert_eq!(
        hits(&withdrawn.clone().as_of(second.basis_t), TEXT).len(),
        2
    );
    assert_eq!(
        hits(
            &withdrawn.clone().history().filter(|_, d| d.source == 99),
            TEXT
        )
        .len(),
        1
    );
    let consolidated = index();
    assert_eq!(hits(&consolidated.clone().history(), TEXT).len(), 2);
    assert!(hits(&consolidated.clone().history(), PRIVATE).is_empty());
    let projection = consolidated.native_fulltext_reader().unwrap().unwrap();
    let mut private_docs = PRIVATE.to_be_bytes().to_vec();
    private_docs.push(0);
    assert!(
        projection
            .prefix(&private_docs, edb_core::FulltextReadLimits::default())
            .unwrap()
            .next()
            .is_none()
    );
    assert_eq!(hits(&first_indexed, TEXT).len(), 1);
    let reopened = Connection::connect_configured(config, &name, 64)
        .unwrap()
        .db();
    assert!(hits(&reopened.clone().filter(|_, d| d.source == 99), TEXT).is_empty());
    assert_eq!(hits(&reopened.history(), TEXT).len(), 2);
    drop(connection);
    writer.shutdown();
}
