//! Catalog operations preserve canonical facts and permanently fence retired routes.
use edb_core::storage::{BlockDatabase, BlockReader, PgBlockStore};
use edb_core::{
    Attribute, BlockTransactor, BlockWriterOptions, Cardinality, DatabaseCatalog, DatabaseValue,
    EntityRef, ErrorCategory, IndexOrder, Keyword, Peer, PortableBackup, PostgresConnectionConfig,
    PostgresOperator, RestoreFault, Schema, TransactionRequest, TxOp, Value, ValueType,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn config() -> PostgresConnectionConfig {
    PostgresConnectionConfig::parse(
        std::env::var("EDB_TEST_POSTGRES_URL")
            .expect("EDB_TEST_POSTGRES_URL must select a disposable PostgreSQL catalog"),
    )
    .unwrap()
}

fn name(case: &str) -> String {
    format!(
        "lifecycle-{case}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

fn projections(value: &DatabaseValue) -> Vec<Vec<edb_core::Datom>> {
    [false, true]
        .into_iter()
        .flat_map(|history| {
            [
                IndexOrder::Eavt,
                IndexOrder::Aevt,
                IndexOrder::Avet,
                IndexOrder::Vaet,
                IndexOrder::Saevt,
                IndexOrder::Savet,
            ]
            .map(|order| {
                if history {
                    value.clone().history().collect_datoms(order).unwrap()
                } else {
                    value.collect_datoms(order).unwrap()
                }
            })
        })
        .collect()
}

fn fixture(config: &PostgresConnectionConfig, name: &str) -> (BlockDatabase, BlockTransactor) {
    PgBlockStore::install(config).unwrap();
    let database = BlockDatabase::create(config, name, Schema::new()).unwrap();
    let mut writer = BlockTransactor::claim(
        config,
        database.clone(),
        BlockWriterOptions {
            lease_duration: Duration::from_secs(60),
            ..Default::default()
        },
    )
    .unwrap();
    let mut attribute = Attribute::new(
        1000,
        Keyword::new("lifecycle", "number"),
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
    (database, writer)
}

fn add(key: &str, source: u64, value: i64) -> TransactionRequest {
    TransactionRequest::new(
        key,
        vec![TxOp::Add {
            entity: EntityRef::Id(42),
            attribute: 1000,
            value: Value::Long(value).into(),
        }],
        EntityRef::Id(source),
    )
}

#[test]
#[ignore = "requires EDB_TEST_POSTGRES_URL selecting a disposable PostgreSQL catalog"]
fn rename_retire_and_name_reuse_preserve_sources_and_captured_identity() {
    let config = config();
    let original = name("identity");
    let renamed = format!("{original}-renamed");
    let occupied = format!("{original}-occupied");
    let (database, mut writer) = fixture(&config, &original);
    let first_request = add("first", 99, 7);
    let first = writer.transact(&first_request).unwrap();
    writer.transact(&add("second", 98, 7)).unwrap();
    let reader = BlockReader::connect(&config, Default::default()).unwrap();
    let captured = reader
        .capture(&database.reference_key())
        .unwrap()
        .database_value();
    let expected = projections(&captured);
    assert_eq!(captured.values(42, 1000).unwrap(), vec![Value::Long(7)]);
    let mut contributors = captured
        .collect_datoms(IndexOrder::Eavt)
        .unwrap()
        .into_iter()
        .filter(|d| d.entity == 42 && d.attribute == 1000)
        .map(|d| d.source)
        .collect::<Vec<_>>();
    contributors.sort();
    assert_eq!(contributors, vec![98, 99]);
    let reference = captured.snapshot_reference().unwrap();
    let peer = Peer::connect_configured(&config, &original, 0).unwrap();
    let peer_identity = peer.identity();
    let directory = tempfile::tempdir().unwrap();
    let mut backup = PortableBackup::connect_configured(&config).unwrap();
    let point = backup
        .backup_database(&original, &directory.path().join("repository"))
        .unwrap();
    let mut catalog = DatabaseCatalog::connect_configured(&config).unwrap();
    let created = catalog.create_if_absent(&original, Schema::new()).unwrap();
    assert!(!created.created);
    let identity = created.database;
    let target = catalog.create_if_absent(&occupied, Schema::new()).unwrap();
    assert!(target.created);
    assert_eq!(
        catalog
            .rename_checked(&original, &occupied, &identity.lineage_id)
            .unwrap_err()
            .code,
        "catalog/name-exists"
    );
    let changed = catalog
        .rename_checked(&original, &renamed, &identity.lineage_id)
        .unwrap();
    assert_eq!(changed.database_id, identity.database_id);
    assert_eq!(changed.lineage_id, identity.lineage_id);
    assert!(catalog.resolve(&original).is_err());
    assert_eq!(projections(&peer.sync().unwrap()), expected);
    assert_eq!(peer.identity(), peer_identity);
    assert_eq!(
        projections(&reference.open(&config, 0, 0).unwrap()),
        expected
    );
    assert_eq!(
        backup
            .backup_database(&renamed, &directory.path().join("repository"))
            .unwrap()
            .manifest_hash,
        point.manifest_hash
    );
    let replay = writer.transact(&first_request).unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.tx_data, first.tx_data);
    assert_eq!(replay.tx_hash, first.tx_hash);
    let mut different_source = first_request.clone();
    different_source.source = EntityRef::Id(98);
    assert_eq!(
        writer.transact(&different_source).unwrap_err().category,
        ErrorCategory::Conflict
    );
    let replaced = writer.transact(&add("replace", 97, 8)).unwrap();
    assert!(replaced.tx_data.iter().all(|d| d.source == 97));
    let held = reader
        .capture(&database.reference_key())
        .unwrap()
        .database_value();
    let held_expected = projections(&held);
    let retired_reference = held.snapshot_reference().unwrap();
    let retired = catalog
        .retire_checked(&renamed, &identity.lineage_id)
        .unwrap()
        .unwrap();
    assert!(retired.retired);
    assert!(catalog.resolve(&renamed).is_err());
    assert!(Peer::connect_configured(&config, &renamed, 0).is_err());
    assert!(retired_reference.open(&config, 0, 0).is_err());
    assert!(reader.capture(&database.reference_key()).is_err());
    assert!(writer.transact(&add("fenced", 99, 9)).is_err());
    assert_eq!(projections(&held), held_expected);
    assert_eq!(projections(&captured), expected);
    assert_eq!(held.values(42, 1000).unwrap(), vec![Value::Long(8)]);
    let verified =
        PortableBackup::verify_backup(&directory.path().join("repository"), point.basis_t, true)
            .unwrap();
    assert_eq!(verified.point.manifest_hash, point.manifest_hash);
    let reused = catalog.create_if_absent(&renamed, Schema::new()).unwrap();
    assert!(reused.created);
    assert_ne!(reused.database.database_id, identity.database_id);
    assert_ne!(reused.database.lineage_id, identity.lineage_id);
    assert_eq!(
        catalog
            .retire_checked(&renamed, &identity.lineage_id)
            .unwrap_err()
            .code,
        "catalog/identity-mismatch"
    );
    assert_eq!(
        catalog
            .rename_checked(&renamed, &original, &identity.lineage_id)
            .unwrap_err()
            .code,
        "catalog/identity-mismatch"
    );
    assert!(
        Peer::connect_configured(&config, &renamed, 0)
            .unwrap()
            .db()
            .values(42, 1000)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        catalog
            .list_retired(Some(&identity.database_id), 1)
            .unwrap()
            .iter()
            .filter(|d| d.database_id == identity.database_id)
            .count(),
        0
    );
    assert_eq!(
        catalog
            .list(Some(&renamed), 1)
            .unwrap()
            .iter()
            .filter(|d| d.name.as_deref() == Some(&renamed))
            .count(),
        0
    );
    assert!(catalog.list(None, 0).is_err());
    assert!(catalog.list(None, 4097).is_err());
}

#[test]
#[ignore = "requires EDB_TEST_POSTGRES_URL selecting a disposable PostgreSQL catalog"]
fn retired_lineage_cannot_restore_before_reclamation() {
    let config = config();
    let original = name("reclamation-admission");
    let (_database, mut writer) = fixture(&config, &original);
    writer.transact(&add("fact", 99, 7)).unwrap();
    writer.release().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let mut backup = PortableBackup::connect_configured(&config).unwrap();
    let point = backup
        .backup_database(&original, &directory.path().join("repository"))
        .unwrap();
    let mut catalog = DatabaseCatalog::connect_configured(&config).unwrap();
    catalog
        .retire_checked(&original, &point.lineage_id)
        .unwrap()
        .unwrap();
    assert!(
        backup
            .restore_backup(
                &directory.path().join("repository"),
                point.basis_t,
                &original
            )
            .is_err(),
        "same-catalog restore accepted a retired lineage before any reclamation"
    );
    assert!(catalog.resolve(&original).is_err());
}

#[test]
#[ignore = "requires independent disposable EDB_TEST_POSTGRES_URL and EDB_TEST_RESTORE_POSTGRES_URL catalogs"]
fn retiring_an_incomplete_restore_reservation_fences_its_activation() {
    let config = config();
    let restore_config = PostgresConnectionConfig::parse(
        std::env::var("EDB_TEST_RESTORE_POSTGRES_URL")
            .expect("independent disposable restore catalog required"),
    )
    .unwrap();
    let original = name("restore-reservation");
    let target = format!("{original}-restore");
    let (_database, mut writer) = fixture(&config, &original);
    writer.transact(&add("fact", 99, 7)).unwrap();
    writer.release().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let point = PortableBackup::connect_configured(&config)
        .unwrap()
        .backup_database(&original, &directory.path().join("repository"))
        .unwrap();
    PgBlockStore::install(&restore_config).unwrap();
    let mut backup = PortableBackup::connect_configured(&restore_config).unwrap();
    assert!(
        backup
            .restore_backup_with_fault(
                &directory.path().join("repository"),
                point.basis_t,
                &target,
                RestoreFault::AfterFirstContentInserted
            )
            .is_err()
    );
    let mut catalog = DatabaseCatalog::connect_configured(&restore_config).unwrap();
    let reservation = catalog.resolve(&target).unwrap();
    assert_eq!(reservation.lineage_id, point.lineage_id);
    assert!(
        !catalog
            .create_if_absent(&target, Schema::new())
            .unwrap()
            .created
    );
    assert!(Peer::connect_configured(&restore_config, &target, 0).is_err());
    let mut reached_activation = false;
    assert!(
        backup
            .restore_backup_with_activation_probe(
                &directory.path().join("repository"),
                point.basis_t,
                &target,
                || {
                    reached_activation = true;
                    catalog
                        .retire_checked(&target, &reservation.lineage_id)
                        .unwrap()
                        .unwrap();
                }
            )
            .is_err()
    );
    assert!(
        reached_activation,
        "restore must reach the final publication boundary"
    );
    assert!(
        backup
            .restore_backup(&directory.path().join("repository"), point.basis_t, &target)
            .is_err()
    );
    assert!(catalog.resolve(&target).is_err());
}

#[test]
#[ignore = "runs catalog-wide GC; requires EDB_TEST_LIFECYCLE_POSTGRES_URL selecting an exclusive disposable catalog"]
fn reclaimed_lineage_gets_a_new_route_without_waiting_for_unrelated_publications() {
    let config = PostgresConnectionConfig::parse(
        std::env::var("EDB_TEST_LIFECYCLE_POSTGRES_URL")
            .expect("explicit exclusive disposable lifecycle catalog required for GC"),
    )
    .unwrap();
    let original = name("reclaimed-route");
    let (database, mut writer) = fixture(&config, &original);
    let request = add("fact", 99, 7);
    let accepted = writer.transact(&request).unwrap();
    writer.transact(&add("corroboration", 98, 7)).unwrap();
    writer.release().unwrap();
    let held = BlockReader::connect(&config, Default::default())
        .unwrap()
        .capture(&database.reference_key())
        .unwrap()
        .database_value();
    let reference = held.snapshot_reference().unwrap();
    let expected = projections(&held);
    let directory = tempfile::tempdir().unwrap();
    let repository = directory.path().join("repository");
    let mut backup = PortableBackup::connect_configured(&config).unwrap();
    let point = backup.backup_database(&original, &repository).unwrap();
    let mut catalog = DatabaseCatalog::connect_configured(&config).unwrap();
    let identity = catalog.resolve(&original).unwrap();
    let mut operator = PostgresOperator::connect_configured(&config).unwrap();
    assert_eq!(
        operator
            .reclaim_retired_database(&identity.database_id, &identity.lineage_id, Duration::ZERO)
            .unwrap_err()
            .code,
        "operations/database-not-retired"
    );
    catalog
        .retire_checked(&original, &identity.lineage_id)
        .unwrap()
        .unwrap();
    assert!(
        backup
            .restore_backup(&repository, point.basis_t, &original)
            .is_err()
    );
    let preview = operator
        .preview_retired_database_reclamation(
            &identity.database_id,
            &identity.lineage_id,
            Duration::ZERO,
        )
        .unwrap();
    assert!(!preview.applied);
    let collect = |operator: &mut PostgresOperator, age| {
        for _ in 0..128 {
            let report = operator
                .reclaim_retired_database(&identity.database_id, &identity.lineage_id, age)
                .unwrap();
            if report.cycle_complete {
                return;
            }
        }
        panic!("bounded fixture collection failed to finish its cycle");
    };
    collect(&mut operator, edb_core::RECOMMENDED_GARBAGE_COLLECTION_AGE);
    assert!(
        backup
            .restore_backup(&repository, point.basis_t, &original)
            .is_err(),
        "a completed retained-age cycle does not establish reclamation"
    );
    assert_eq!(projections(&held), expected);
    collect(&mut operator, Duration::ZERO);
    // Starting a later cycle proves the preceding cycle reached Complete.
    // Reclamation admission must remain possible during that later cycle.
    let later = operator.collect_garbage(Duration::ZERO).unwrap();
    assert_eq!(
        later.phase,
        Some(edb_core::storage::ownership::CollectionPhase::Adding)
    );
    // A new unrelated owner produces an unsettled ownership event after the
    // relevant roots were reclaimed. It must not impose global quiescence on
    // admission of this retired lineage.
    catalog
        .create_if_absent(&name("unrelated"), Schema::new())
        .unwrap();
    let restored = backup
        .restore_backup(&repository, point.basis_t, &original)
        .unwrap();
    assert_eq!(restored.point.lineage_id, identity.lineage_id);
    let active = catalog.resolve(&original).unwrap();
    assert_eq!(active.lineage_id, identity.lineage_id);
    assert_ne!(active.database_id, identity.database_id);
    assert!(reference.open(&config, 0, 0).is_err());
    assert!(
        BlockReader::connect(&config, Default::default())
            .unwrap()
            .capture(&database.reference_key())
            .is_err()
    );
    assert_eq!(
        projections(
            &Peer::connect_configured(&config, &original, 0)
                .unwrap()
                .db()
        ),
        expected
    );
    let route = BlockDatabase::resolve(&config, &original).unwrap();
    let mut writer = BlockTransactor::claim(&config, route, BlockWriterOptions::default()).unwrap();
    let replay = writer.transact(&request).unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.tx_data, accepted.tx_data);
    assert_eq!(replay.tx_hash, accepted.tx_hash);
    writer.release().unwrap();
    collect(&mut operator, Duration::ZERO);
    assert_eq!(
        projections(
            &Peer::connect_configured(&config, &original, 0)
                .unwrap()
                .db()
        ),
        expected
    );
    assert_eq!(
        PortableBackup::verify_backup(&repository, point.basis_t, true)
            .unwrap()
            .point
            .manifest_hash,
        point.manifest_hash
    );
}
