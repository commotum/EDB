use edb_core::edn::write_edn;
use edb_core::storage::{BlockDatabase, PgBlockStore};
use edb_core::{
    Attribute, BlockTransactor, BlockWriterOptions, CacheTier, Cardinality, EntityRef, IndexOrder,
    IndexPrefix, Keyword, OperationContext, OperationKind, Peer, PostgresConnectionConfig,
    PostgresOperator, Schema, TransactionRequest, TxOp, Value, ValueType, io_report_to_edn,
};
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
#[ignore = "requires EDB_TEST_POSTGRES_URL for a disposable PostgreSQL database"]
fn source_index_io_follows_lazy_cursor_context_and_cache_state() {
    let config = PostgresConnectionConfig::parse(
        std::env::var("EDB_TEST_POSTGRES_URL")
            .expect("EDB_TEST_POSTGRES_URL must name disposable PostgreSQL"),
    )
    .unwrap();
    let name = format!(
        "io-stats-source-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    PgBlockStore::install(&config).unwrap();
    let database = BlockDatabase::create(&config, &name, Schema::new()).unwrap();
    let database_id = database
        .reference_key()
        .strip_prefix("databases/")
        .unwrap()
        .to_owned();
    let mut writer =
        BlockTransactor::claim(&config, database, BlockWriterOptions::default()).unwrap();
    let mut attribute = Attribute::new(
        1000,
        Keyword::new("item", "score"),
        ValueType::Long,
        Cardinality::One,
    );
    attribute.indexed = true;
    writer
        .transact(&TransactionRequest::new(
            "schema",
            vec![TxOp::InstallAttribute(attribute)],
            EntityRef::Id(99),
        ))
        .unwrap();
    let first = writer
        .transact(&TransactionRequest::new(
            "item",
            vec![TxOp::Add {
                entity: EntityRef::Temp("item".into()),
                attribute: 1000,
                value: Value::Long(7).into(),
            }],
            EntityRef::Id(99),
        ))
        .unwrap();
    let item = first.tempids["item"];
    writer
        .transact(&TransactionRequest::new(
            "second-contribution",
            vec![TxOp::Add {
                entity: EntityRef::Id(item),
                attribute: 1000,
                value: Value::Long(7).into(),
            }],
            EntityRef::Id(98),
        ))
        .unwrap();
    PostgresOperator::connect_configured(&config)
        .unwrap()
        .consolidate_database(&database_id)
        .unwrap();
    let peer = Peer::connect_configured(&config, &name, 64).unwrap();
    let db = peer.db();

    for (order, prefix) in [
        (
            IndexOrder::Saevt,
            IndexPrefix::Saevt {
                source: 99,
                attribute: Some(1000),
                entity: None,
                value: None,
            },
        ),
        (
            IndexOrder::Savet,
            IndexPrefix::Savet {
                source: 99,
                attribute: Some(1000),
                value: None,
                entity: None,
            },
        ),
    ] {
        for history in [false, true] {
            let view = if history {
                db.clone().history()
            } else {
                db.clone()
            };
            for warm in [false, true] {
                let parent = OperationContext::named(
                    OperationKind::Query,
                    Keyword::new("catalog", "inspect"),
                )
                .unwrap();
                let child_name = Keyword::new("catalog", "source-facts");
                let child = parent
                    .child_named(OperationKind::Query, child_name.clone())
                    .unwrap();
                let sibling = parent.child(OperationKind::Query);
                let cursor = {
                    let _scope = child.enter();
                    view.prefix_cursor(&prefix).unwrap()
                };
                // Only cursor construction has happened. Reads during later
                // consumption must retain this child even inside another scope.
                assert_eq!(child.report().stats.reads.indexes[&order].cursors, 1);
                assert_eq!(child.report().stats.reads.indexes[&order].node_accesses, 0);
                let datoms = {
                    let _scope = sibling.enter();
                    cursor.collect::<Result<Vec<_>, _>>().unwrap()
                };
                assert_eq!(datoms.len(), 1);
                assert_eq!(datoms[0].entity, item);
                assert_eq!(datoms[0].source, 99);
                assert_eq!(datoms[0].value, Value::Long(7));
                let report = child.report();
                let index = report.stats.reads.indexes[&order];
                assert_eq!(index.cursors, 1);
                assert!(index.node_accesses > 0);
                assert!(report.stats.reads.cache[&CacheTier::DecodedNode].accesses > 0);
                assert_eq!(sibling.report().stats.reads, Default::default());
                assert_eq!(parent.report().stats.reads, report.stats.reads);
                assert_eq!(
                    parent.report().nested[&child_name].reads,
                    report.stats.reads
                );
                if warm {
                    assert_eq!(index.decoded_misses, 0);
                    assert!(
                        !report
                            .stats
                            .reads
                            .cache
                            .contains_key(&CacheTier::PostgresBlock)
                    );
                    assert!(report.stats.reads.cache[&CacheTier::DecodedNode].hits > 0);
                } else {
                    assert!(index.decoded_misses > 0);
                    assert!(report.stats.reads.cache[&CacheTier::PostgresBlock].physical_bytes > 0);
                }
                let edn = write_edn(&io_report_to_edn(&parent.report())).unwrap();
                assert!(edn.contains(&format!(":index.{order:?}")));
                assert!(edn.contains(":catalog/source-facts"));
            }
        }
    }
    writer.release().unwrap();
}
