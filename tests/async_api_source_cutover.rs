use edb_core::edn_query::{EdnQueryArgument, parse_query_edn};
use edb_core::{
    AsyncConfig, AsyncExecutor, AsyncStreamOptions, Attribute, Cardinality, Database, EntityRef,
    Keyword, PreparedQuery, QueryControl, QueryDataSource, QueryResult, QuerySourceValue,
    QueryValue, Schema, TxOp, Value, ValueType,
};
use std::sync::mpsc;
use std::time::Duration;

#[test]
fn async_queries_capture_source_bearing_views_before_execution_or_first_poll() {
    let mut schema = Schema::new();
    schema
        .install(Attribute::new(
            1_000,
            Keyword::new("item", "tag"),
            ValueType::String,
            Cardinality::One,
        ))
        .unwrap();
    let operation = TxOp::Add {
        entity: EntityRef::Id(42),
        attribute: 1_000,
        value: Value::String("same".into()).into(),
    };
    let first = Database::new(schema)
        .unwrap()
        .database_value()
        .with(std::slice::from_ref(&operation), 1, EntityRef::Id(99))
        .unwrap();
    let captured = first
        .db_after
        .with(&[operation], 2, EntityRef::Id(98))
        .unwrap()
        .db_after;
    let bound = parse_query_edn(
        r#"[:find ?s ?added :in $snapshot
            :where [$snapshot 42 :item/tag "same" _ ?s ?added]]"#,
    )
    .unwrap()
    .bind(&[EdnQueryArgument::Source(QuerySourceValue::Database(
        captured.clone(),
    ))])
    .unwrap();
    let query = bound.query().clone();
    let mut sources = bound.sources;
    let prepared = PreparedQuery::new(&query).unwrap();
    let workers = AsyncExecutor::new(AsyncConfig {
        workers: 1,
        max_operations: 8,
        max_resources: 8,
    })
    .unwrap();
    let timeout = Duration::from_secs(10);
    let (entered, started) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    // The callback occupies the only worker until all capture points have run.
    workers
        .query_sources(
            query.clone(),
            &sources,
            vec![],
            QueryControl::default(),
            None,
        )
        .unwrap()
        .on_complete(move |result| {
            result.unwrap();
            entered.send(()).unwrap();
            gate.recv_timeout(timeout).unwrap();
        });
    started.recv_timeout(timeout).unwrap();
    let materialized = workers
        .query_sources(
            query.clone(),
            &sources,
            vec![],
            QueryControl::default(),
            None,
        )
        .unwrap();
    let prepared = workers
        .query_prepared(prepared, &sources, vec![], QueryControl::default(), None)
        .unwrap();
    let mut sequence = workers
        .query_sequence_sources(
            query.clone(),
            &sources,
            vec![],
            QueryControl::default(),
            None,
            AsyncStreamOptions {
                chunk_rows: 1,
                timeout: Some(timeout),
            },
        )
        .unwrap();
    let withdrawn = captured
        .with(
            &[TxOp::Retract {
                entity: EntityRef::Id(42),
                attribute: 1_000,
                value: Some(Value::String("same".into()).into()),
            }],
            3,
            EntityRef::Id(97),
        )
        .unwrap()
        .db_after;
    assert!(withdrawn.basis_t() > captured.basis_t());
    sources[0] = QueryDataSource::database("$snapshot", withdrawn.clone());
    drop(captured);
    // No future or stream has been polled; queued jobs now own the old sources.
    release.send(()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime.block_on(async {
        let expected = vec![
            vec![
                QueryValue::Scalar(Value::Ref(98)),
                QueryValue::Scalar(Value::Bool(true)),
            ],
            vec![
                QueryValue::Scalar(Value::Ref(99)),
                QueryValue::Scalar(Value::Bool(true)),
            ],
        ];
        assert_eq!(
            materialized.await.unwrap().result,
            QueryResult::Relation(expected.clone())
        );
        assert_eq!(
            prepared.await.unwrap().result,
            QueryResult::Relation(expected.clone())
        );
        let mut rows = vec![];
        while let Some(row) = sequence.next().await {
            rows.push(row.unwrap());
        }
        assert_eq!(rows, expected);
        assert!(sequence.is_terminated());
        assert!(sequence.next().await.is_none());
        assert_eq!(
            workers
                .query_sources(
                    query.clone(),
                    &sources,
                    vec![],
                    QueryControl::default(),
                    None
                )
                .unwrap()
                .await
                .unwrap()
                .result,
            QueryResult::Relation(vec![]),
        );
        // Hiding the retracting source must not resurrect either contribution.
        let filtered = withdrawn.clone().filter(|_, datom| datom.source != 97);
        assert_eq!(
            workers
                .query_sources(
                    query.clone(),
                    &[QueryDataSource::database("$snapshot", filtered)],
                    vec![],
                    QueryControl::default(),
                    None,
                )
                .unwrap()
                .await
                .unwrap()
                .result,
            QueryResult::Relation(vec![]),
        );
        let mut history = vec![vec![
            QueryValue::Scalar(Value::Ref(97)),
            QueryValue::Scalar(Value::Bool(false)),
        ]];
        history.extend(expected);
        assert_eq!(
            workers
                .query_sources(
                    query,
                    &[QueryDataSource::database("$snapshot", withdrawn.history())],
                    vec![],
                    QueryControl::default(),
                    None,
                )
                .unwrap()
                .await
                .unwrap()
                .result,
            QueryResult::Relation(history),
        );
    });
}
