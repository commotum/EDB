//! Source-exact semantic faults with valid hashes, routing, and ownership publication.
use crate::index::tree::{TreeConfig, build_tree_with_sink};
use crate::storage::ownership::publish_refs;
use crate::storage::read_authorization::ReadAuthorization;
use crate::storage::root::{Block, DatabaseRoot};
use crate::storage::{
    BatchOutcome, BlockDatabase, BlockReader, ObjectId, PgBlockStore, RefChange, RefCondition,
};
use crate::{
    Attribute, BlockTransactor, BlockWriterOptions, Cardinality, EntityRef, FulltextBuildLimits,
    FulltextProjection, IndexOrder, Keyword, PortableBackup, PostgresConnectionConfig,
    PostgresOperator, Schema, TransactionRequest, TxOp, Value, ValueType,
};
use std::time::{SystemTime, UNIX_EPOCH};

const TEXT: u32 = 1000;

struct RootRestore {
    config: PostgresConnectionConfig,
    key: String,
    original: ObjectId,
}
impl RootRestore {
    fn publish(&self, store: &mut PgBlockStore, root: ObjectId) {
        let reference = store.read_ref(&self.key).unwrap().unwrap();
        assert!(matches!(
            publish_refs(
                store,
                &[RefCondition {
                    key: self.key.clone(),
                    expected: Some(reference.revision)
                }],
                &[RefChange {
                    key: self.key.clone(),
                    value: Some(root.to_vec())
                }],
            )
            .unwrap(),
            BatchOutcome::Applied(_)
        ));
    }
    fn restore(&self) {
        self.publish(
            &mut PgBlockStore::connect(&self.config).unwrap(),
            self.original,
        );
    }
}
impl Drop for RootRestore {
    fn drop(&mut self) {
        // Use the ordinary ownership barrier on every exit, including panics.
        // No deleted object or unbalanced raw head-CAS fault survives this test.
        if !std::thread::panicking() {
            self.restore();
        } else if let Ok(mut store) = PgBlockStore::connect(&self.config) {
            let result = (|| {
                let reference = store.read_ref(&self.key)?.unwrap();
                publish_refs(
                    &mut store,
                    &[RefCondition {
                        key: self.key.clone(),
                        expected: Some(reference.revision),
                    }],
                    &[RefChange {
                        key: self.key.clone(),
                        value: Some(self.original.to_vec()),
                    }],
                )
            })();
            if !matches!(result, Ok(BatchOutcome::Applied(_))) {
                eprintln!("disposable integrity fixture restoration failed: {result:?}");
            }
        }
    }
}

fn changed_source_tree(
    store: &mut PgBlockStore,
    value: &crate::DatabaseValue,
    order: IndexOrder,
) -> crate::index::tree::TreeDescriptor {
    let mut datoms = value.clone().history().collect_datoms(order).unwrap();
    let target = datoms
        .iter_mut()
        .find(|d| d.attribute == TEXT && d.added)
        .unwrap();
    assert_ne!(target.source, 93);
    target.source = 93;
    datoms.sort_by(|a, b| a.cmp_in(b, order));
    let built = build_tree_with_sink(
        order,
        true,
        datoms.into_iter().map(Ok),
        &TreeConfig::default(),
        &mut |id, bytes| {
            assert_eq!(store.put(bytes)?, id);
            Ok(())
        },
    )
    .unwrap();
    built.descriptor
}
fn assert_rejected(config: &PostgresConnectionConfig, name: &str, id: &str, code: &str) {
    let report = PostgresOperator::connect_configured(config)
        .unwrap()
        .inspect_database(id, true)
        .unwrap();
    assert!(!report.healthy());
    assert!(
        report.problems.iter().any(|p| p.code == code),
        "{:?}",
        report.problems
    );
    // Capture copies immutable content; semantic verification is a separate step.
    let directory = tempfile::tempdir().unwrap();
    let repository = directory.path().join("backup");
    let point = PortableBackup::connect_configured(config)
        .unwrap()
        .backup_database(name, &repository)
        .unwrap();
    PortableBackup::verify_backup_point_presence(&repository, point.basis_t, point.log_generation)
        .unwrap();
    let error =
        PortableBackup::verify_backup_point(&repository, point.basis_t, point.log_generation, true)
            .unwrap_err();
    assert_eq!(error.code, code);
}

#[test]
#[ignore = "requires explicit EDB_TEST_POSTGRES_URL selecting a disposable PostgreSQL catalog"]
fn deep_live_and_offline_inspection_reject_hash_valid_retained_sources_and_search() {
    let config = PostgresConnectionConfig::parse(
        std::env::var("EDB_TEST_POSTGRES_URL").expect("explicit disposable catalog required"),
    )
    .unwrap();
    PgBlockStore::install(&config).unwrap();
    let name = format!(
        "administration-integrity-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let database = BlockDatabase::create(&config, &name, Schema::new()).unwrap();
    let mut writer =
        BlockTransactor::claim(&config, database.clone(), BlockWriterOptions::default()).unwrap();
    writer
        .transact(&TransactionRequest::new(
            "schema",
            vec![TxOp::InstallAttribute(
                Attribute::new(
                    TEXT,
                    Keyword::new("test", "text"),
                    ValueType::String,
                    Cardinality::Many,
                )
                .fulltext(),
            )],
            EntityRef::Id(99),
        ))
        .unwrap();
    let first = writer
        .transact(&TransactionRequest::new(
            "first",
            vec![TxOp::Add {
                entity: EntityRef::Temp("item".into()),
                attribute: TEXT,
                value: Value::String(name.clone()).into(),
            }],
            EntityRef::Id(99),
        ))
        .unwrap();
    writer.release().unwrap();
    let mut operator = PostgresOperator::connect_configured(&config).unwrap();
    operator
        .consolidate_database(&crate::storage::catalog::identity_string(database.route))
        .unwrap();
    let reader = BlockReader::connect(&config, Default::default()).unwrap();
    let old = reader.capture(&database.reference_key()).unwrap();
    let mut writer =
        BlockTransactor::claim(&config, database.clone(), BlockWriterOptions::default()).unwrap();
    writer
        .transact(&TransactionRequest::new(
            "second",
            vec![TxOp::Add {
                entity: EntityRef::Id(first.tempids["item"]),
                attribute: TEXT,
                value: Value::String("Blue River".into()).into(),
            }],
            EntityRef::Id(98),
        ))
        .unwrap();
    writer.release().unwrap();
    let id = crate::DatabaseCatalog::connect_configured(&config)
        .unwrap()
        .resolve(&name)
        .unwrap()
        .database_id;
    operator.consolidate_database(&id).unwrap();
    let latest = reader.capture(&database.reference_key()).unwrap();
    let mut store = PgBlockStore::connect(&config).unwrap();
    let reference = store.read_ref(&database.reference_key()).unwrap().unwrap();
    let original = reference.value.unwrap().try_into().unwrap();
    let root = DatabaseRoot::decode(&original, &store.get(original).unwrap().unwrap()).unwrap();
    let restore = RootRestore {
        config: config.clone(),
        key: database.reference_key(),
        original,
    };
    assert!(operator.inspect_database(&id, true).unwrap().healthy());

    // The latest covering indexes are correct. Only an authorized older
    // source-leading history descriptor lies about its source coordinate.
    let mut corrupted = old.index_descriptor().clone();
    let replacement = changed_source_tree(&mut store, &old.database_value(), IndexOrder::Saevt);
    *corrupted
        .trees
        .iter_mut()
        .find(|t| t.order == IndexOrder::Saevt && t.history)
        .unwrap() = replacement;
    corrupted.fulltext = None;
    let bad_index = store.put(&corrupted.encode().unwrap()).unwrap();
    let mut candidate = root.clone();
    candidate.read_authorization = Some(
        ReadAuthorization::retain_index(
            &mut store,
            root.read_authorization.unwrap(),
            &root.identity,
            bad_index,
        )
        .unwrap(),
    );
    let bad_root = store.put(&candidate.encode().unwrap()).unwrap();
    restore.publish(&mut store, bad_root);
    assert_eq!(
        reader
            .capture(&database.reference_key())
            .unwrap()
            .index_descriptor(),
        latest.index_descriptor()
    );
    assert_rejected(&config, &name, &id, "backup/tree-semantic-mismatch");
    restore.restore();
    assert!(operator.inspect_database(&id, true).unwrap().healthy());

    // Rebind hash-valid search pages with false source attribution to the
    // correct latest descriptor. Header binding and canonical trees agree;
    // only canonical record comparison can detect this fault.
    let mut wrong_source = latest.index_descriptor().clone();
    *wrong_source
        .trees
        .iter_mut()
        .find(|t| t.order == IndexOrder::Aevt && t.history)
        .unwrap() = changed_source_tree(&mut store, &latest.database_value(), IndexOrder::Aevt);
    wrong_source.fulltext = None;
    let (attachment, _) = crate::storage::fulltext::build_for_descriptor(
        &mut store,
        &wrong_source,
        latest.database_value().schema(),
        None,
        &FulltextBuildLimits::default(),
    )
    .unwrap()
    .unwrap();
    let mut block = Block::decode(&attachment, &store.get(attachment).unwrap().unwrap()).unwrap();
    let mut descriptor = latest.index_descriptor().clone();
    descriptor.fulltext = None;
    let source = store.put(&descriptor.encode().unwrap()).unwrap();
    let mut projection = FulltextProjection::decode(&block.payload[16..]).unwrap();
    projection.source_manifest = source;
    block.links[0] = source;
    block.payload.truncate(16);
    block.payload.extend_from_slice(&projection.encode());
    descriptor.fulltext = Some(store.put(&block.encode().unwrap()).unwrap());
    let bad_index = store.put(&descriptor.encode().unwrap()).unwrap();
    let mut candidate = root.clone();
    candidate.indexes = Some(bad_index);
    candidate.read_authorization = Some(
        ReadAuthorization::retain_index(
            &mut store,
            root.read_authorization.unwrap(),
            &root.identity,
            bad_index,
        )
        .unwrap(),
    );
    let bad_root = store.put(&candidate.encode().unwrap()).unwrap();
    restore.publish(&mut store, bad_root);
    assert_rejected(&config, &name, &id, "backup/fulltext-semantic-mismatch");
    restore.restore();
    let healthy = operator.inspect_database(&id, true).unwrap();
    assert!(healthy.healthy(), "{:?}", healthy.problems);
    assert_eq!(healthy.metrics.basis_t, 3);
    assert_eq!(healthy.metrics.requests, Some(3));
    // Common verification remains cancellable for all graph/replay work.
    let control = crate::MaintenanceControl::default();
    control.cancel();
    assert_eq!(
        operator
            .with_maintenance_control(control)
            .inspect_database(&id, true)
            .unwrap_err()
            .code,
        "maintenance/canceled"
    );
}
