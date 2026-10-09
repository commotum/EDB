//! Explicit current-index recovery is opt-in; ordinary opens never replay the log.
use edb_core::storage::root::DatabaseRoot;
use edb_core::storage::{BlockDatabase, BlockReader, IndexDescriptor, PgBlockStore};
use edb_core::{
    Attribute, BlockTransactor, BlockWriterOptions, Cardinality, DatabaseCatalog, DatabaseValue,
    EntityRef, ErrorCategory, IndexOrder, IndexPrefix, Keyword, PostgresConnectionConfig,
    PostgresOperator, Schema, TransactionRequest, TxOp, Value, ValueType,
};
use std::time::{SystemTime, UNIX_EPOCH};

const ORDERS: [IndexOrder; 6] = [
    IndexOrder::Eavt,
    IndexOrder::Aevt,
    IndexOrder::Avet,
    IndexOrder::Vaet,
    IndexOrder::Saevt,
    IndexOrder::Savet,
];

struct DescriptorRestore {
    config: PostgresConnectionConfig,
    id: edb_core::Digest,
    bytes: Vec<u8>,
    restored: bool,
}

impl DescriptorRestore {
    fn restore(&mut self, store: &mut PgBlockStore) {
        assert_eq!(store.put(&self.bytes).unwrap(), self.id);
        self.restored = true;
    }
}

impl Drop for DescriptorRestore {
    fn drop(&mut self) {
        if !self.restored {
            let restored =
                PgBlockStore::connect(&self.config).and_then(|mut store| store.put(&self.bytes));
            if !matches!(restored, Ok(id) if id == self.id) {
                eprintln!("failed to restore disposable descriptor fault fixture: {restored:?}");
            }
        }
    }
}

fn projections(value: &DatabaseValue) -> Vec<Vec<edb_core::Datom>> {
    [false, true]
        .into_iter()
        .flat_map(|history| {
            ORDERS.map(|order| {
                if history {
                    value.clone().history().collect_datoms(order).unwrap()
                } else {
                    value.collect_datoms(order).unwrap()
                }
            })
        })
        .collect()
}

fn root(store: &mut PgBlockStore, key: &str) -> DatabaseRoot {
    let reference = store.read_ref(key).unwrap().unwrap();
    let id = reference.value.unwrap().try_into().unwrap();
    DatabaseRoot::decode(&id, &store.get(id).unwrap().unwrap()).unwrap()
}

fn add(entity: EntityRef, attribute: u32, value: Value) -> TxOp {
    TxOp::Add {
        entity,
        attribute,
        value: value.into(),
    }
}

#[test]
#[ignore = "requires EDB_TEST_POSTGRES_URL for a disposable PostgreSQL database"]
fn explicit_recovery_preserves_sources_receipts_and_retained_values() {
    const NUMBER: u32 = 1000;
    const LINK: u32 = 1001;
    let config = PostgresConnectionConfig::parse(
        std::env::var("EDB_TEST_POSTGRES_URL")
            .expect("EDB_TEST_POSTGRES_URL must name a disposable PostgreSQL database"),
    )
    .unwrap();
    let name = format!(
        "deployment-recovery-{}-{}",
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
    let mut number = Attribute::new(
        NUMBER,
        Keyword::new("recovery", "number"),
        ValueType::Long,
        Cardinality::One,
    );
    number.indexed = true;
    writer
        .transact(&TransactionRequest::new(
            "schema",
            vec![
                TxOp::InstallAttribute(number),
                TxOp::InstallAttribute(Attribute::new(
                    LINK,
                    Keyword::new("recovery", "link"),
                    ValueType::Ref,
                    Cardinality::Many,
                )),
            ],
            EntityRef::Id(99),
        ))
        .unwrap();
    let first_request = TransactionRequest::new(
        "first-contribution",
        vec![
            add(EntityRef::Temp("shared".into()), NUMBER, Value::Long(7)),
            add(
                EntityRef::Temp("shared".into()),
                edb_core::DB_DOC as u32,
                Value::String(name.clone()),
            ),
            add(EntityRef::Temp("withdrawn".into()), NUMBER, Value::Long(8)),
            TxOp::Add {
                entity: EntityRef::Temp("shared".into()),
                attribute: LINK,
                value: edb_core::TxValue::Entity(EntityRef::Temp("withdrawn".into())),
            },
        ],
        EntityRef::Id(99),
    );
    let first = writer.transact(&first_request).unwrap();
    let shared = first.tempids["shared"];
    let withdrawn = first.tempids["withdrawn"];
    writer
        .transact(&TransactionRequest::new(
            "second-contribution",
            vec![
                add(EntityRef::Id(shared), NUMBER, Value::Long(7)),
                add(EntityRef::Id(withdrawn), NUMBER, Value::Long(8)),
            ],
            EntityRef::Id(98),
        ))
        .unwrap();
    writer
        .transact(&TransactionRequest::new(
            "withdraw-value",
            vec![TxOp::Retract {
                entity: EntityRef::Id(withdrawn),
                attribute: NUMBER,
                value: Some(Value::Long(8).into()),
            }],
            EntityRef::Id(97),
        ))
        .unwrap();
    writer.release().unwrap();

    let id = DatabaseCatalog::connect_configured(&config)
        .unwrap()
        .resolve(&name)
        .unwrap()
        .database_id;
    // Construct a workload-specific descriptor before injecting loss. The
    // smaller tree limit also makes the retained descriptor distinct from the
    // default-layout recovery result, so historical damage remains detectable.
    let consolidated = PostgresOperator::connect_configured(&config)
        .unwrap()
        .with_tree_config(edb_core::index::tree::TreeConfig {
            max_leaf_datoms: 8,
            target_leaf_bytes: 2048,
            ..Default::default()
        })
        .unwrap()
        .consolidate_database(&id)
        .unwrap();
    assert!(!consolidated.recovered);

    let reader = BlockReader::connect(&config, Default::default()).unwrap();
    let captured = reader.capture(&database.reference_key()).unwrap();
    let value = captured.database_value();
    let expected = projections(&value);
    let old_expected = projections(&first.db_after);
    let expected_log: Vec<_> = captured
        .log()
        .tx_range(None, None)
        .unwrap()
        .map(|entry| entry.unwrap().data)
        .collect();
    for data in &expected_log {
        let source = data.first().unwrap().source;
        assert!(data.iter().all(|datom| datom.source == source));
        if source != edb_core::DB_SOURCE_SYSTEM {
            let timestamps: Vec<_> = data
                .iter()
                .filter(|datom| datom.attribute == edb_core::DB_TX_INSTANT as u32)
                .collect();
            assert_eq!(timestamps.len(), 1);
            assert_eq!(timestamps[0].entity, timestamps[0].tx);
            assert!(timestamps[0].added);
        }
    }
    assert_eq!(value.values(shared, NUMBER).unwrap(), vec![Value::Long(7)]);
    assert!(value.values(withdrawn, NUMBER).unwrap().is_empty());
    assert_eq!(
        value
            .collect_datoms_with_prefix(&IndexPrefix::Saevt {
                source: 99,
                attribute: Some(NUMBER),
                entity: None,
                value: None,
            })
            .unwrap()
            .iter()
            .filter(|d| d.entity == shared)
            .count(),
        1
    );
    assert!(
        value
            .collect_datoms_with_prefix(&IndexPrefix::Saevt {
                source: 98,
                attribute: Some(NUMBER),
                entity: None,
                value: None,
            })
            .unwrap()
            .iter()
            .all(|d| d.entity != withdrawn)
    );

    let mut store = PgBlockStore::connect(&config).unwrap();
    let key = database.reference_key();
    let original = root(&mut store, &key);
    let original_index = original.indexes.unwrap();
    let original_index_bytes = store.get(original_index).unwrap().unwrap();
    let original_descriptor =
        IndexDescriptor::decode(&original_index, &original_index_bytes).unwrap();
    assert_eq!(original_descriptor.identity, database.identity);
    assert_eq!(original_descriptor.basis, original.basis);
    assert!(original_descriptor.basis > first.basis_t);
    let mut restore = DescriptorRestore {
        config: config.clone(),
        id: original_index,
        bytes: original_index_bytes,
        restored: false,
    };
    // Isolated administrative fault fixture. Delete the uniquely identified
    // current descriptor, keeping its exact bytes for restoration after proving
    // that current recovery cannot certify retained historical indexes.
    assert_eq!(store.remove_objects(&[original_index]).unwrap(), 1);
    let cold = BlockReader::connect(&config, Default::default()).unwrap();
    let error = match cold.capture(&key) {
        Ok(_) => panic!("ordinary open must fail"),
        Err(error) => error,
    };
    assert_eq!(error.category, ErrorCategory::Fault);
    assert_eq!(root(&mut store, &key), original);

    let mut operator = PostgresOperator::connect_configured(&config).unwrap();
    let repaired = operator.consolidate_database(&id).unwrap();
    assert!(repaired.recovered);
    assert_eq!(repaired.basis_t, original.basis);
    let after = root(&mut store, &key);
    assert_eq!(after.basis, original.basis);
    assert_eq!(after.writer_epoch, original.writer_epoch);
    assert_eq!(after.log, original.log);
    assert_eq!(after.receipts, original.receipts);
    assert_eq!(after.metadata, original.metadata);
    let descriptor_id = after.indexes.unwrap();
    assert_ne!(descriptor_id, original_index);
    let descriptor =
        IndexDescriptor::decode(&descriptor_id, &store.get(descriptor_id).unwrap().unwrap())
            .unwrap();
    assert_eq!(descriptor.trees.len(), 12);
    assert!(descriptor.pending_avet.is_empty());
    assert!(descriptor.avet_work.is_empty());
    for history in [false, true] {
        for order in ORDERS {
            assert_eq!(
                descriptor
                    .trees
                    .iter()
                    .filter(|tree| tree.history == history && tree.order == order)
                    .count(),
                1
            );
        }
    }
    let recovered = cold.capture(&key).unwrap();
    assert_eq!(projections(&recovered.database_value()), expected);
    let historical_damage = operator.inspect_database(&id, true).unwrap();
    assert!(!historical_damage.healthy());
    // Recovery replaces only today's descriptor. Restore the fault fixture's
    // original object so receipts, retained indexes and ownership metadata all
    // remain intact for subsequent tests in this disposable namespace.
    restore.restore(&mut store);
    let timestamps = recovered
        .database_value()
        .collect_datoms_with_prefix(&IndexPrefix::Saevt {
            source: 99,
            attribute: Some(edb_core::DB_TX_INSTANT as u32),
            entity: None,
            value: None,
        })
        .unwrap();
    assert_eq!(timestamps.len(), 2);
    for timestamp in timestamps {
        let transaction = expected_log
            .iter()
            .find(|data| data.iter().any(|datom| datom == &timestamp))
            .unwrap();
        assert!(transaction.iter().all(|datom| datom.source == 99));
    }
    assert_eq!(projections(&first.db_after), old_expected);
    assert_eq!(
        projections(&recovered.database_value().as_of(first.basis_t)),
        old_expected
    );
    assert_eq!(
        recovered
            .log()
            .tx_range(None, None)
            .unwrap()
            .map(|entry| entry.unwrap().data)
            .collect::<Vec<_>>(),
        expected_log
    );
    let inspection = operator.inspect_database(&id, true).unwrap();
    assert!(inspection.healthy(), "{:?}", inspection.problems);

    let mut writer =
        BlockTransactor::claim(&config, database, BlockWriterOptions::default()).unwrap();
    let replayed = writer.transact(&first_request).unwrap();
    assert!(replayed.replayed);
    assert_eq!(replayed.basis_t, first.basis_t);
    assert_eq!(replayed.tx_hash, first.tx_hash);
    assert_eq!(replayed.tempids, first.tempids);
    assert_eq!(replayed.tx_data, first.tx_data);
    assert_eq!(projections(&replayed.db_after), old_expected);
    assert_eq!(
        projections(&replayed.db_before),
        projections(&first.db_before)
    );
    let mut changed_source = first_request.clone();
    changed_source.source = EntityRef::Id(98);
    assert_eq!(
        writer.transact(&changed_source).err().unwrap().category,
        ErrorCategory::Conflict
    );
    writer.release().unwrap();
}
