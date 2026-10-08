use edb_core::edn_query::{EdnQueryArgument, parse_query_edn};
use edb_core::storage::{BlockDatabase, PgBlockStore};
use edb_core::{
    Attribute, BlockTransactor, BlockWriterOptions, Cardinality, DB_SOURCE_SYSTEM, DB_TX_INSTANT,
    DatabaseValue, Datom, EntityRef, IndexBoundary, IndexComponents, IndexPrefix, IndexTransaction,
    Keyword, LogValue, PostgresConnectionConfig, QueryControl, QueryResult, QuerySourceValue,
    QueryValue, Schema, SemanticError, ServiceTransactionReport, TimePoint, TransactionRequest,
    TxOp, Value, ValueType,
};
use std::time::{SystemTime, UNIX_EPOCH};

const TAG: u32 = 1000;

fn writer() -> BlockTransactor {
    let config = PostgresConnectionConfig::parse(
        std::env::var("EDB_TEST_POSTGRES_URL")
            .expect("EDB_TEST_POSTGRES_URL must name a disposable PostgreSQL database"),
    )
    .unwrap();
    let name = format!(
        "log-source-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    PgBlockStore::install(&config).unwrap();
    let database = BlockDatabase::create(&config, &name, Schema::new()).unwrap();
    BlockTransactor::claim(&config, database, BlockWriterOptions::default()).unwrap()
}

fn add(entity: EntityRef, text: &str) -> TxOp {
    TxOp::Add {
        entity,
        attribute: TAG,
        value: Value::String(text.into()).into(),
    }
}

fn submit(
    writer: &mut BlockTransactor,
    key: &str,
    source: u64,
    ops: Vec<TxOp>,
    instant: i64,
) -> ServiceTransactionReport {
    writer
        .transact(
            &TransactionRequest::new(key, ops, EntityRef::Id(source)).with_tx_instant(instant),
        )
        .unwrap()
}

fn setup(writer: &mut BlockTransactor) -> (u64, u64) {
    submit(
        writer,
        "schema",
        DB_SOURCE_SYSTEM,
        vec![TxOp::InstallAttribute(Attribute::new(
            TAG,
            Keyword::new("item", "tag"),
            ValueType::String,
            Cardinality::One,
        ))],
        1000,
    );
    let sources = submit(
        writer,
        "sources",
        DB_SOURCE_SYSTEM,
        vec![
            add(EntityRef::Temp("alice".into()), "Alice"),
            add(EntityRef::Temp("bob".into()), "Bob"),
        ],
        1000,
    );
    (sources.tempids["alice"], sources.tempids["bob"])
}

fn activity_page(db: &DatabaseValue, source: u64, before: Option<u64>, limit: usize) -> Vec<Datom> {
    let components = match before {
        Some(tx) => IndexComponents::Three(source, DB_TX_INSTANT as u32, tx),
        None => IndexComponents::Two(source, DB_TX_INSTANT as u32),
    };
    db.reverse_seek_cursor(&IndexBoundary::Saevt(components))
        .unwrap()
        .map(Result::unwrap)
        .take_while(|d| d.source == source && d.attribute == DB_TX_INSTANT as u32)
        .filter(|d| before.is_none_or(|tx| d.entity < tx))
        .take(limit)
        .collect()
}

#[test]
#[ignore = "requires EDB_TEST_POSTGRES_URL for a disposable PostgreSQL database"]
fn source_activity_composes_same_basis_log_with_stable_reverse_pages() {
    let mut writer = writer();
    let (alice, bob) = setup(&mut writer);
    let original = submit(
        &mut writer,
        "original",
        alice,
        vec![add(EntityRef::Temp("item".into()), "old")],
        1000,
    );
    let item = original.tempids["item"];
    let redundant = submit(
        &mut writer,
        "redundant",
        alice,
        vec![add(EntityRef::Id(item), "old")],
        1000,
    );
    let corroborated = submit(
        &mut writer,
        "corroborated",
        bob,
        vec![add(EntityRef::Id(item), "old")],
        1000,
    );
    let withdrawn = submit(
        &mut writer,
        "withdrawn",
        bob,
        vec![TxOp::Retract {
            entity: EntityRef::Id(item),
            attribute: TAG,
            value: Some(Value::String("old".into()).into()),
        }],
        1000,
    );
    let empty = submit(&mut writer, "empty", alice, vec![], 1000);
    let captured = empty.db_after.clone();
    let log = captured.log_value().unwrap();
    assert_eq!(log.basis_t(), captured.basis_t());
    assert_eq!(redundant.tx_data.len(), 1);
    assert_eq!(empty.tx_data.len(), 1);
    let first_page = activity_page(&captured, alice, None, 2);
    assert_eq!(
        first_page.iter().map(|d| d.entity).collect::<Vec<_>>(),
        [
            edb_core::t_to_tx(empty.basis_t).unwrap(),
            edb_core::t_to_tx(redundant.basis_t).unwrap()
        ]
    );
    let later = submit(
        &mut writer,
        "later",
        alice,
        vec![add(EntityRef::Id(item), "new")],
        2000,
    );
    let second_page = activity_page(&captured, alice, Some(first_page[1].entity), 2);
    assert_eq!(
        second_page.iter().map(|d| d.entity).collect::<Vec<_>>(),
        [edb_core::t_to_tx(original.basis_t).unwrap()]
    );
    let pages = first_page
        .into_iter()
        .chain(second_page)
        .collect::<Vec<_>>();
    for (timestamp, report) in pages.iter().zip([&empty, &redundant, &original]) {
        assert_eq!(timestamp.value, Value::Instant(1000));
        assert_eq!(timestamp.entity, timestamp.tx);
        assert!(timestamp.added);
        assert_eq!(
            log.tx_data(IndexTransaction::Tx(timestamp.entity))
                .unwrap()
                .unwrap(),
            report.tx_data
        );
    }
    assert_eq!(
        log.tx_data(IndexTransaction::T(later.basis_t)).unwrap(),
        None
    );
    assert_eq!(
        activity_page(&later.db_after, alice, None, 1)[0].entity,
        edb_core::t_to_tx(later.basis_t).unwrap()
    );
    let prefix = IndexPrefix::Saevt {
        source: alice,
        attribute: Some(TAG),
        entity: Some(item),
        value: None,
    };
    assert!(
        captured.datoms_with_prefix(&prefix).unwrap().is_empty(),
        "Bob's retraction must not resurrect Alice's contribution"
    );
    assert!(
        captured
            .history()
            .datoms_with_prefix(&prefix)
            .unwrap()
            .iter()
            .any(|d| d.added && d.value == Value::String("old".into()))
    );
    for report in [&corroborated, &withdrawn] {
        let data = log
            .tx_data(IndexTransaction::T(report.basis_t))
            .unwrap()
            .unwrap();
        assert_eq!(data, report.tx_data);
        assert!(data.iter().all(|d| d.source == bob));
    }
    assert!(
        withdrawn
            .tx_data
            .iter()
            .any(|d| d.entity == item && !d.added)
    );
    writer.release().unwrap();
}

fn query(log: &LogValue, text: &str, control: &QueryControl) -> Result<QueryResult, SemanticError> {
    parse_query_edn(text)?
        .bind(&[EdnQueryArgument::Source(QuerySourceValue::Log(log.clone()))])?
        .execute(control, None)
        .map(|outcome| outcome.result)
}
fn refs(values: impl IntoIterator<Item = u64>) -> QueryResult {
    QueryResult::Collection(
        values
            .into_iter()
            .map(|v| QueryValue::Scalar(Value::Ref(v)))
            .collect(),
    )
}
fn rows(datoms: &[Datom]) -> QueryResult {
    QueryResult::Relation(
        datoms
            .iter()
            .map(|d| {
                [
                    Value::Ref(d.entity),
                    Value::Ref(d.attribute as u64),
                    d.value.clone(),
                    Value::Ref(d.tx),
                    Value::Ref(d.source),
                    Value::Bool(d.added),
                ]
                .into_iter()
                .map(QueryValue::Scalar)
                .collect()
            })
            .collect(),
    )
}
fn range(log: &LogValue, start: Option<TimePoint>, end: Option<TimePoint>) -> Vec<u64> {
    log.tx_range(start, end)
        .unwrap()
        .map(|entry| entry.unwrap().t)
        .collect()
}

#[test]
#[ignore = "requires EDB_TEST_POSTGRES_URL for a disposable PostgreSQL database"]
fn captured_log_bounds_lazy_cursor_and_query_shapes_obey_contract() {
    let mut writer = writer();
    let (alice, bob) = setup(&mut writer);
    let first = submit(
        &mut writer,
        "first",
        alice,
        vec![add(EntityRef::Temp("item".into()), "old")],
        2000,
    );
    let item = first.tempids["item"];
    let tied = submit(
        &mut writer,
        "tied",
        bob,
        vec![add(EntityRef::Id(item), "new")],
        2000,
    );
    let last = submit(&mut writer, "last", alice, vec![], 4000);
    let log = last.db_after.log_value().unwrap();
    let all = (1..=last.basis_t).collect::<Vec<_>>();
    assert_eq!(range(&log, None, None), all);
    assert_eq!(
        range(
            &log,
            Some(TimePoint::Instant(2000)),
            Some(TimePoint::Instant(4000))
        ),
        [first.basis_t, tied.basis_t]
    );
    assert_eq!(range(&log, None, Some(TimePoint::Instant(2000))), [1, 2]);
    assert_eq!(
        range(&log, Some(TimePoint::Instant(3000)), None),
        [last.basis_t]
    );
    assert_eq!(
        range(
            &log,
            Some(TimePoint::Tx(edb_core::t_to_tx(first.basis_t).unwrap())),
            Some(TimePoint::T(last.basis_t))
        ),
        [first.basis_t, tied.basis_t]
    );
    assert_eq!(
        range(&log, Some(TimePoint::T(0)), Some(TimePoint::T(999))),
        all
    );
    for (start, end) in [(999, 1000), (last.basis_t, 1), (0, 0)] {
        assert!(range(&log, Some(TimePoint::T(start)), Some(TimePoint::T(end))).is_empty());
    }
    assert!(range(&log, Some(TimePoint::Instant(5000)), None).is_empty());
    assert!(log.tx_range(Some(TimePoint::Tx(item)), None).is_err());
    assert!(log.tx_range(None, Some(TimePoint::T(u64::MAX))).is_err());
    assert!(log.tx_data(IndexTransaction::Tx(item)).is_err());
    assert!(log.tx_data(IndexTransaction::T(u64::MAX)).is_err());
    assert_eq!(log.tx_data(IndexTransaction::T(0)).unwrap(), None);
    assert_eq!(log.tx_data(IndexTransaction::T(999)).unwrap(), None);
    let mut cursor = log
        .tx_range(
            Some(TimePoint::T(first.basis_t)),
            Some(TimePoint::T(last.basis_t)),
        )
        .unwrap();
    assert_eq!(cursor.stats(), Default::default());
    assert_eq!(cursor.next().unwrap().unwrap().data, first.tx_data);
    assert_eq!(cursor.stats().transactions_read, 1);
    assert_eq!(cursor.stats().peak_buffered_transactions, 1);
    assert_eq!(cursor.next().unwrap().unwrap().data, tied.tx_data);
    assert!(cursor.next().is_none());
    assert!(cursor.next().is_none());
    assert_eq!(
        log.tx_ids(None, None)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap(),
        all.iter()
            .map(|t| edb_core::t_to_tx(*t).unwrap())
            .collect::<Vec<_>>()
    );
    let control = QueryControl::default();
    assert_eq!(
        query(
            &log,
            "[:find [?tx ...] :in $log :where [(tx-ids $log nil nil) [?tx ...]]]",
            &control
        )
        .unwrap(),
        refs(all.iter().map(|t| edb_core::t_to_tx(*t).unwrap()))
    );
    let text = format!(
        "[:find ?e ?a ?v ?tx ?s ?op :in $log :where [(tx-data $log {}) [[?e ?a ?v ?tx ?s ?op]]]]",
        tied.basis_t
    );
    let mut expected = match rows(&tied.tx_data) {
        QueryResult::Relation(rows) => rows,
        _ => unreachable!(),
    };
    expected.sort_by(|a, b| {
        a.iter()
            .zip(b)
            .map(|(a, b)| a.canonical_cmp(b))
            .find(|c| !c.is_eq())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    assert_eq!(
        query(&log, &text, &control).unwrap(),
        QueryResult::Relation(expected)
    );
    let bound_tx = edb_core::t_to_tx(tied.basis_t).unwrap();
    let unified = format!(
        "[:find ?e ?a ?v ?tx ?s ?op :in $log :where [(ground #edb/ref {bound_tx}) ?tx] [(tx-data $log ?tx) [[?e ?a ?v ?tx ?s ?op]]]]"
    );
    assert_eq!(
        query(&log, &unified, &control).unwrap(),
        query(&log, &text, &control).unwrap()
    );
    let truncated = format!(
        "[:find ?e :in $log :where [(tx-data $log {}) [[?e]]]]",
        tied.basis_t
    );
    assert_eq!(
        query(&log, &truncated, &control).unwrap(),
        QueryResult::Relation({
            let mut entities = [item, bound_tx];
            entities.sort_unstable();
            entities
                .into_iter()
                .map(|entity| vec![QueryValue::Scalar(Value::Ref(entity))])
                .collect()
        })
    );
    let ids_by_instant = "[:find [?tx ...] :in $log :where [(tx-ids $log #inst \"1970-01-01T00:00:02Z\" #inst \"1970-01-01T00:00:04Z\") [?tx ...]]]";
    assert_eq!(
        query(&log, ids_by_instant, &control).unwrap(),
        refs([edb_core::t_to_tx(first.basis_t).unwrap(), bound_tx])
    );
    let ids_by_ref = format!(
        "[:find [?tx ...] :in $log :where [(tx-ids $log #edb/ref {} #edb/ref {}) [?tx ...]]]",
        edb_core::t_to_tx(first.basis_t).unwrap(),
        edb_core::t_to_tx(last.basis_t).unwrap()
    );
    assert_eq!(
        query(&log, &ids_by_ref, &control).unwrap(),
        query(&log, ids_by_instant, &control).unwrap()
    );
    assert!(
        query(
            &log,
            &format!("[:find ?e :in $log :where [(tx-data $log {bound_tx}) [[?e]]]]"),
            &control
        )
        .is_err(),
        "a numeric transaction entity is interpreted as T, not automatically as a reference"
    );
    let scalar_relation = query(
        &log,
        &format!(
            "[:find ?data . :in $log :where [(tx-data $log {}) ?data]]",
            tied.basis_t
        ),
        &control,
    )
    .unwrap();
    let QueryResult::Scalar(Some(QueryValue::Collection(tuples))) = &scalar_relation else {
        panic!("tx-data scalar relation")
    };
    assert_eq!(tuples.len(), tied.tx_data.len());
    for tuple in tuples {
        let QueryValue::Tuple(columns) = tuple else {
            panic!("tx-data tuple")
        };
        assert_eq!(columns.len(), 6);
        assert_eq!(columns[4], QueryValue::Scalar(Value::Ref(bob)));
        assert!(matches!(columns[5], QueryValue::Scalar(Value::Bool(_))));
    }
    let collection_relation = query(
        &log,
        &format!(
            "[:find [?row ...] :in $log :where [(tx-data $log {}) [?row ...]]]",
            tied.basis_t
        ),
        &control,
    )
    .unwrap();
    let QueryResult::Collection(collection) = collection_relation else {
        panic!("tx-data collection relation")
    };
    assert_eq!(collection.len(), tuples.len());
    for tuple in &collection {
        match tuple {
            QueryValue::Tuple(columns) => {
                assert_eq!(columns.len(), 6);
                assert_eq!(columns[4], QueryValue::Scalar(Value::Ref(bob)));
            }
            QueryValue::Scalar(Value::Tuple(columns)) => {
                assert_eq!(columns.len(), 6);
                assert_eq!(columns[4], Some(Value::Ref(bob)));
            }
            other => panic!("tx-data collection row: {other:?}"),
        }
    }
    for t in [tied.basis_t, 0] {
        let too_wide =
            format!("[:find ?e :in $log :where [(tx-data $log {t}) [[?e _ _ _ _ _ _]]]]");
        assert_eq!(
            query(&log, &too_wide, &control).unwrap_err().code,
            "query/function-binding"
        );
    }
    let wrong_source = parse_query_edn("[:find ?e :in $log :where [(tx-data $log 1) [[?e]]]]")
        .unwrap()
        .bind(&[EdnQueryArgument::Source(QuerySourceValue::Database(
            last.db_after.clone(),
        ))])
        .unwrap();
    assert_eq!(
        wrong_source.execute(&control, None).unwrap_err().code,
        "query/source-kind"
    );
    for t in [0, 999] {
        assert_eq!(
            query(
                &log,
                &format!("[:find ?e :in $log :where [(tx-data $log {t}) [[?e]]]]"),
                &control
            )
            .unwrap(),
            QueryResult::Relation(vec![])
        );
    }
    for (form, code) in [
        ("(tx-ids $log -1 nil)", "query/log-time"),
        ("(tx-ids $log \"bad\" nil)", "query/log-time"),
        ("(tx-data $log nil)", "query/log-transaction"),
        (
            "(tx-data $log #inst \"1970-01-01T00:00:02Z\")",
            "query/log-transaction",
        ),
    ] {
        let text = format!("[:find ?x :in $log :where [{form} [[?x]]]]");
        assert_eq!(query(&log, &text, &control).unwrap_err().code, code);
    }
    assert!(query(&log, "[:find ?e :in $log :where [$log ?e]]", &control).is_err());
    assert!(
        query(
            &log,
            &text,
            &QueryControl {
                max_intermediate_rows: 1,
                ..QueryControl::default()
            }
        )
        .is_err()
    );
    writer.release().unwrap();
}
