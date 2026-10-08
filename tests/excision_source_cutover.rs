use edb_core::storage::{
    BlockDatabase, BlockReader, IndexDescriptor, PgBlockStore, SnapshotMetadata,
};
use edb_core::{
    Attribute, BlockTransactor, BlockWriterOptions, Cardinality, DB_EXCISE, DB_EXCISE_BEFORE_T,
    Datom, EntityRef, ExcisionFault, IndexOrder, IndexPrefix, Keyword, PostgresConnectionConfig,
    PostgresOperator, Schema, TransactionRequest, TxOp, Value, ValueType,
};
use std::time::{SystemTime, UNIX_EPOCH};

fn add(entity: EntityRef, attribute: u32, value: Value) -> TxOp {
    TxOp::Add {
        entity,
        attribute,
        value: value.into(),
    }
}
fn retract(entity: u64, attribute: u32, value: Value) -> TxOp {
    TxOp::Retract {
        entity: EntityRef::Id(entity),
        attribute,
        value: Some(value.into()),
    }
}
fn submit(
    writer: &mut BlockTransactor,
    key: &str,
    source: u64,
    forms: Vec<TxOp>,
) -> edb_core::ServiceTransactionReport {
    writer
        .transact(&TransactionRequest::new(key, forms, EntityRef::Id(source)))
        .unwrap()
}
fn log_datoms(snapshot: &edb_core::storage::BlockSnapshot) -> Vec<Datom> {
    snapshot
        .log()
        .tx_range(None, None)
        .unwrap()
        .flat_map(|entry| entry.unwrap().data)
        .collect()
}
fn metadata(
    store: &mut PgBlockStore,
    snapshot: &edb_core::storage::BlockSnapshot,
) -> SnapshotMetadata {
    let id = snapshot.captured_root().metadata.unwrap();
    SnapshotMetadata::decode(&id, &store.get(id).unwrap().unwrap()).unwrap()
}

#[test]
#[ignore = "requires EDB_TEST_POSTGRES_URL for a disposable PostgreSQL database"]
fn durable_excision_preserves_sources_global_visibility_and_frozen_requests() {
    const NAME: u32 = 1000;
    const NO_HISTORY: u32 = 1001;
    const CHILD: u32 = 1002;
    const LINK: u32 = 1003;
    let config = PostgresConnectionConfig::parse(
        std::env::var("EDB_TEST_POSTGRES_URL")
            .expect("EDB_TEST_POSTGRES_URL must name a disposable PostgreSQL database"),
    )
    .unwrap();
    let name = format!(
        "excision-source-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    PgBlockStore::install(&config).unwrap();
    let database = BlockDatabase::create(&config, &name, Schema::new()).unwrap();
    let mut writer =
        BlockTransactor::claim(&config, database.clone(), BlockWriterOptions::default()).unwrap();
    let mut no_history = Attribute::new(
        NO_HISTORY,
        Keyword::new("item", "ephemeral"),
        ValueType::Long,
        Cardinality::One,
    );
    no_history.no_history = true;
    no_history.indexed = true;
    let mut name_attribute = Attribute::new(
        NAME,
        Keyword::new("item", "name"),
        ValueType::String,
        Cardinality::One,
    )
    .fulltext();
    name_attribute.indexed = true;
    submit(
        &mut writer,
        "schema",
        99,
        vec![
            TxOp::InstallAttribute(name_attribute),
            TxOp::InstallAttribute(no_history),
            TxOp::InstallAttribute(
                Attribute::new(
                    CHILD,
                    Keyword::new("item", "child"),
                    ValueType::Ref,
                    Cardinality::One,
                )
                .component(),
            ),
            TxOp::InstallAttribute(Attribute::new(
                LINK,
                Keyword::new("item", "link"),
                ValueType::Ref,
                Cardinality::Many,
            )),
        ],
    );
    let created_request = TransactionRequest::new(
        "created",
        vec![
            add(
                EntityRef::Temp("private".into()),
                NAME,
                Value::String("private text".into()),
            ),
            add(
                EntityRef::Temp("private".into()),
                NO_HISTORY,
                Value::Long(1),
            ),
            add(
                EntityRef::Temp("child".into()),
                NAME,
                Value::String("private child".into()),
            ),
            TxOp::Add {
                entity: EntityRef::Temp("private".into()),
                attribute: CHILD,
                value: edb_core::TxValue::Entity(EntityRef::Temp("child".into())),
            },
            add(
                EntityRef::Temp("withdrawn".into()),
                NAME,
                Value::String("withdrawn text".into()),
            ),
            add(
                EntityRef::Temp("retained".into()),
                NAME,
                Value::String("retained text".into()),
            ),
        ],
        EntityRef::Id(99),
    );
    let created = writer.transact(&created_request).unwrap();
    let private = created.tempids["private"];
    let child = created.tempids["child"];
    let withdrawn = created.tempids["withdrawn"];
    let retained = created.tempids["retained"];
    submit(
        &mut writer,
        "second-source",
        98,
        vec![
            add(
                EntityRef::Id(private),
                NAME,
                Value::String("private text".into()),
            ),
            add(
                EntityRef::Id(withdrawn),
                NAME,
                Value::String("withdrawn text".into()),
            ),
            add(
                EntityRef::Id(retained),
                NAME,
                Value::String("retained text".into()),
            ),
            add(EntityRef::Id(retained), LINK, Value::Ref(private)),
            add(EntityRef::Id(retained), LINK, Value::Ref(child)),
        ],
    );
    submit(
        &mut writer,
        "withdraw-and-replace",
        97,
        vec![
            retract(withdrawn, NAME, Value::String("withdrawn text".into())),
            add(EntityRef::Id(private), NO_HISTORY, Value::Long(2)),
        ],
    );
    let excise = submit(
        &mut writer,
        "excision-request",
        99,
        vec![
            add(
                EntityRef::Temp("request".into()),
                DB_EXCISE as u32,
                Value::Ref(private),
            ),
            add(
                EntityRef::Temp("request".into()),
                DB_EXCISE_BEFORE_T as u32,
                Value::Long(100000),
            ),
        ],
    );
    let request = excise.tempids["request"];
    let second_request = submit(
        &mut writer,
        "same-predicate-second-source",
        98,
        vec![
            add(
                EntityRef::Id(request),
                DB_EXCISE as u32,
                Value::Ref(private),
            ),
            add(
                EntityRef::Id(request),
                DB_EXCISE_BEFORE_T as u32,
                Value::Long(100000),
            ),
        ],
    );
    submit(
        &mut writer,
        "later-edits",
        97,
        vec![
            retract(request, DB_EXCISE as u32, Value::Ref(private)),
            add(
                EntityRef::Id(request),
                DB_EXCISE_BEFORE_T as u32,
                Value::Long(0),
            ),
            add(
                EntityRef::Id(private),
                NAME,
                Value::String("later public text".into()),
            ),
            add(EntityRef::Id(private), NO_HISTORY, Value::Long(3)),
        ],
    );
    writer.release().unwrap();
    let reader = BlockReader::connect(&config, Default::default()).unwrap();
    let before = reader.capture(&database.reference_key()).unwrap();
    let original = log_datoms(&before);
    let expected = original.iter().filter(|d| {
        let selected = d.entity == private || d.entity == child
            || matches!(d.value, Value::Ref(e) if (d.attribute == CHILD || d.attribute == LINK) && (e == private || e == child));
        !(selected && edb_core::tx_to_t(d.tx).unwrap() < second_request.basis_t)
    }).cloned().collect::<Vec<_>>();
    assert!(
        original
            .iter()
            .any(|d| d.entity == private && d.attribute == NO_HISTORY && d.value == Value::Long(1))
    );
    let mut store = PgBlockStore::connect(&config).unwrap();
    let original_metadata = metadata(&mut store, &before);
    let mut operator = PostgresOperator::connect_configured(&config).unwrap();
    let id = edb_core::DatabaseCatalog::connect_configured(&config)
        .unwrap()
        .resolve(&name)
        .unwrap()
        .database_id;
    assert!(!operator.sync_excise(&id, second_request.basis_t).unwrap());
    let fault = operator
        .process_excision_requests_with_fault(&id, ExcisionFault::AfterCandidateStaged)
        .unwrap_err();
    assert_eq!(fault.code, "excision/injected-fault");
    assert_eq!(
        log_datoms(&reader.capture(&database.reference_key()).unwrap()),
        original
    );
    let receipt = operator.process_excision_requests(&id).unwrap();
    assert!(receipt.resumed);
    assert_eq!(receipt.request_count, 2);
    assert_eq!(
        receipt.removed_datoms as usize,
        original.len() - expected.len()
    );
    assert!(operator.sync_excise(&id, second_request.basis_t).unwrap());
    let after = reader.capture(&database.reference_key()).unwrap();
    assert_eq!(log_datoms(&after), expected);
    let successor_metadata = metadata(&mut store, &after);
    assert_eq!(
        successor_metadata.generation,
        original_metadata.generation + 1
    );
    assert_eq!(successor_metadata.basis, original_metadata.basis);
    assert_eq!(
        successor_metadata.eidx_frontier,
        original_metadata.eidx_frontier
    );
    assert_eq!(
        successor_metadata.reserved_frontier,
        original_metadata.reserved_frontier
    );
    assert_eq!(
        successor_metadata.last_tx_instant,
        original_metadata.last_tx_instant
    );
    let index_id = after.captured_root().indexes.unwrap();
    let descriptor =
        IndexDescriptor::decode(&index_id, &store.get(index_id).unwrap().unwrap()).unwrap();
    assert_eq!(descriptor.trees.len(), 12);
    assert!(descriptor.fulltext.is_some());
    let value = after.database_value();
    for order in [
        IndexOrder::Eavt,
        IndexOrder::Aevt,
        IndexOrder::Avet,
        IndexOrder::Saevt,
        IndexOrder::Savet,
    ] {
        let facts = value.collect_datoms(order).unwrap();
        assert!(
            !facts
                .iter()
                .any(|d| d.entity == withdrawn && d.attribute == NAME)
        );
        let contributions = facts
            .iter()
            .filter(|d| d.entity == retained && d.attribute == NAME)
            .collect::<Vec<_>>();
        assert_eq!(contributions.len(), 2);
        assert_eq!(
            contributions
                .iter()
                .map(|d| d.source)
                .collect::<std::collections::BTreeSet<_>>(),
            [98, 99].into()
        );
        let later = facts
            .iter()
            .find(|d| d.entity == private && d.attribute == NAME)
            .unwrap();
        assert_eq!(later.source, 97);
        assert_eq!(later.value, Value::String("later public text".into()));
        let history = value.clone().history().collect_datoms(order).unwrap();
        assert!(!history.iter().any(|d| d.added
            && d.entity == private
            && d.value == Value::String("private text".into())));
        assert!(
            history
                .iter()
                .any(|d| d.entity == withdrawn && !d.added && d.source == 97)
        );
    }
    assert!(!value.collect_datoms(IndexOrder::Vaet).unwrap().iter().any(
        |d| matches!(d.value, Value::Ref(e) if e == private || e == child) && d.attribute == LINK
    ));
    for history in [false, true] {
        let search_value = if history {
            value.clone().history()
        } else {
            value.clone()
        };
        assert!(
            search_value
                .fulltext(
                    NAME,
                    "\"private text\"",
                    &edb_core::FulltextOptions::default()
                )
                .unwrap()
                .hits
                .is_empty()
        );
        assert!(
            !search_value
                .fulltext(
                    NAME,
                    "\"retained text\"",
                    &edb_core::FulltextOptions::default()
                )
                .unwrap()
                .hits
                .is_empty()
        );
    }
    let requests = value
        .clone()
        .history()
        .prefix_cursor(&IndexPrefix::Aevt {
            attribute: DB_EXCISE as u32,
            entity: Some(request),
            value: None,
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests
            .iter()
            .filter(|d| d.added)
            .map(|d| d.source)
            .collect::<std::collections::BTreeSet<_>>(),
        [98, 99].into()
    );
    for source in [98, 99] {
        let facts = value
            .prefix_cursor(&IndexPrefix::Saevt {
                source,
                attribute: Some(NAME),
                entity: None,
                value: None,
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(!facts.iter().any(|d| d.entity == withdrawn));
        assert!(facts.iter().any(|d| d.entity == retained));
    }
    assert!(
        !value
            .clone()
            .history()
            .collect_datoms(IndexOrder::Vaet)
            .unwrap()
            .iter()
            .any(|d| (d.attribute == LINK || d.attribute == CHILD)
                && matches!(d.value, Value::Ref(e) if e == private || e == child))
    );
    let remaining_no_history = value
        .collect_datoms(IndexOrder::Eavt)
        .unwrap()
        .into_iter()
        .filter(|d| d.entity == private && d.attribute == NO_HISTORY)
        .collect::<Vec<_>>();
    assert_eq!(remaining_no_history.len(), 1);
    assert_eq!(remaining_no_history[0].value, Value::Long(3));
    assert_eq!(remaining_no_history[0].source, 97);
    let completed_again = operator.process_excision_requests(&id).unwrap();
    assert_eq!(completed_again.request_count, 0);
    assert_eq!(completed_again.generation, receipt.generation);
    let reopened = BlockReader::connect(&config, Default::default())
        .unwrap()
        .capture(&database.reference_key())
        .unwrap();
    assert_eq!(log_datoms(&reopened), expected);
    let mut writer =
        BlockTransactor::claim(&config, database, BlockWriterOptions::default()).unwrap();
    assert_eq!(
        writer.transact(&created_request).unwrap_err().code,
        "postgres/idempotency-predates-excision"
    );
    for key in [
        "schema",
        "second-source",
        "withdraw-and-replace",
        "excision-request",
        "same-predicate-second-source",
        "later-edits",
    ] {
        assert_eq!(
            writer
                .transact(&TransactionRequest::new(key, vec![], EntityRef::Id(99)))
                .unwrap_err()
                .code,
            "postgres/idempotency-predates-excision"
        );
    }
    let fresh = TransactionRequest::new(
        "after-excision",
        vec![add(
            EntityRef::Id(retained),
            NAME,
            Value::String("fresh text".into()),
        )],
        EntityRef::Id(96),
    );
    let report = writer.transact(&fresh).unwrap();
    assert!(report.tx_data.iter().all(|d| d.source == 96));
    assert_eq!(writer.transact(&fresh).unwrap().basis_t, report.basis_t);
    writer.release().unwrap();
}
