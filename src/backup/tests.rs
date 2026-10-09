use super::*;
use crate::index::tree::TreeConfig;
use crate::storage::root::Block;
use crate::{
    Attribute, BlockTransactor, BlockWriterOptions, Cardinality, Datom, EntityRef, IndexOrder,
    IndexPrefix, Keyword, PostgresOperator, Schema, TransactionRequest, TxOp, Value, ValueType,
};
use std::time::{SystemTime, UNIX_EPOCH};

const SCORE: u32 = 1000;
const ORDERS: [IndexOrder; 6] = [
    IndexOrder::Eavt,
    IndexOrder::Aevt,
    IndexOrder::Avet,
    IndexOrder::Vaet,
    IndexOrder::Saevt,
    IndexOrder::Savet,
];

fn add(entity: EntityRef, value: i64) -> TxOp {
    TxOp::Add {
        entity,
        attribute: SCORE,
        value: Value::Long(value).into(),
    }
}
fn request(key: &str, source: u64, operations: Vec<TxOp>) -> TransactionRequest {
    TransactionRequest::new(key, operations, EntityRef::Id(source))
}
fn read(
    config: &PostgresConnectionConfig,
    database: &BlockDatabase,
) -> crate::storage::BlockSnapshot {
    BlockReader::connect(config, Default::default())
        .unwrap()
        .capture(&database.reference_key())
        .unwrap()
}
fn assert_reads_equal(left: &crate::DatabaseValue, right: &crate::DatabaseValue) {
    assert_eq!(left.basis_t(), right.basis_t());
    assert_eq!(left.eidx_frontier(), right.eidx_frontier());
    assert_eq!(left.schema(), right.schema());
    for history in [false, true] {
        let left = if history {
            left.clone().history()
        } else {
            left.clone()
        };
        let right = if history {
            right.clone().history()
        } else {
            right.clone()
        };
        for order in ORDERS {
            assert_eq!(
                left.collect_datoms(order).unwrap(),
                right.collect_datoms(order).unwrap(),
                "order={order:?} history={history}"
            );
        }
    }
}
fn assert_pending(value: &crate::DatabaseValue) {
    assert!(!value.has_avet(&crate::AttributeName::Id(SCORE)).unwrap());
    for prefix in [
        IndexPrefix::Avet {
            attribute: SCORE,
            value: None,
            entity: None,
        },
        IndexPrefix::Savet {
            source: 98,
            attribute: Some(SCORE),
            value: None,
            entity: None,
        },
    ] {
        for history in [false, true] {
            let value = if history {
                value.clone().history()
            } else {
                value.clone()
            };
            assert!(
                value.prefix_cursor(&prefix).is_err(),
                "qualified pending access must reject {prefix:?}"
            );
        }
    }
    for order in [IndexOrder::Avet, IndexOrder::Savet] {
        assert!(
            value
                .collect_datoms(order)
                .unwrap()
                .iter()
                .all(|d| d.attribute != SCORE)
        );
    }
    assert!(
        value
            .prefix_cursor(&IndexPrefix::Savet {
                source: 98,
                attribute: None,
                value: None,
                entity: None
            })
            .unwrap()
            .all(|row| row.unwrap().attribute != SCORE)
    );
}
fn log_data(snapshot: &crate::storage::BlockSnapshot) -> Vec<Datom> {
    snapshot
        .log()
        .tx_range(None, None)
        .unwrap()
        .flat_map(|entry| entry.unwrap().data)
        .collect()
}
fn step(config: &PostgresConnectionConfig, writer: &mut BlockTransactor) -> IndexDescriptor {
    let mut store = PgBlockStore::connect(config).unwrap();
    let prepared = writer
        .index_input()
        .unwrap()
        .prepare(&mut store, &TreeConfig::default())
        .unwrap();
    let descriptor = prepared.descriptor().clone();
    writer.adopt_index(prepared).unwrap();
    descriptor
}

// These tests intentionally exercise one ordinary publication at a time. That
// gives reproducible partial checkpoints without sleeps or scheduler races.
#[test]
#[ignore = "requires EDB_TEST_POSTGRES_URL and EDB_TEST_RESTORE_POSTGRES_URL selecting independent disposable catalogs"]
fn source_bearing_backup_restore_preserves_pending_and_partial_six_index_publications() {
    let source = PostgresConnectionConfig::parse(
        std::env::var("EDB_TEST_POSTGRES_URL")
            .expect("explicit disposable source catalog required"),
    )
    .unwrap();
    let destination = PostgresConnectionConfig::parse(
        std::env::var("EDB_TEST_RESTORE_POSTGRES_URL")
            .expect("explicit independent disposable restore catalog required"),
    )
    .unwrap();
    PgBlockStore::install(&source).unwrap();
    PgBlockStore::install(&destination).unwrap();
    let unique = format!(
        "backup-source-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let target_name = format!("{unique}-restored");
    let database = BlockDatabase::create(&source, &unique, Schema::new()).unwrap();
    let mut writer =
        BlockTransactor::claim(&source, database.clone(), BlockWriterOptions::default()).unwrap();
    let mut attribute = Attribute::new(
        SCORE,
        Keyword::new("item", "score"),
        ValueType::Long,
        Cardinality::One,
    );
    writer
        .transact(&request(
            "schema",
            99,
            vec![TxOp::InstallAttribute(attribute.clone())],
        ))
        .unwrap();
    let mut entities = Vec::new();
    // More than one 1024-datom projection batch; values oppose entity order,
    // and source order opposes insertion order, exercising both sort orders.
    for batch in 0..5 {
        let report = writer
            .transact(&request(
                &format!("seed-{batch}"),
                99,
                (0..128)
                    .map(|offset| {
                        let n = batch * 128 + offset;
                        add(EntityRef::Temp(format!("row-{n}")), (640 - n) as i64)
                    })
                    .collect(),
            ))
            .unwrap();
        for offset in 0..128 {
            entities.push(report.tempids[&format!("row-{}", batch * 128 + offset)]);
        }
    }
    for batch in 0..5 {
        writer
            .transact(&request(
                &format!("second-{batch}"),
                98,
                (0..128)
                    .map(|offset| {
                        let n = batch * 128 + offset;
                        add(EntityRef::Id(entities[n]), (640 - n) as i64)
                    })
                    .collect(),
            ))
            .unwrap();
    }
    let retract = request(
        "withdraw-and-replace",
        97,
        vec![
            TxOp::Retract {
                entity: EntityRef::Id(entities[0]),
                attribute: SCORE,
                value: Some(Value::Long(640).into()),
            },
            add(EntityRef::Id(entities[1]), 9000),
        ],
    );
    let accepted = writer.transact(&retract).unwrap();
    assert!(accepted.tx_data.iter().all(|d| d.source == 97));
    step(&source, &mut writer);
    attribute.indexed = true;
    writer
        .transact(&request(
            "enable-index",
            96,
            vec![TxOp::AlterAttribute(attribute)],
        ))
        .unwrap();
    let before = read(&source, &database);
    assert_pending(&before.database_value());
    let current = before
        .database_value()
        .collect_datoms(IndexOrder::Saevt)
        .unwrap();
    assert!(
        !current
            .iter()
            .any(|d| d.entity == entities[0] && d.attribute == SCORE)
    );
    assert_eq!(
        current
            .iter()
            .filter(|d| d.entity == entities[2] && d.attribute == SCORE)
            .map(|d| d.source)
            .collect::<BTreeSet<_>>(),
        [98, 99].into()
    );
    assert_eq!(
        before.database_value().values(entities[2], SCORE).unwrap(),
        vec![Value::Long(638)]
    );
    let original_log = log_data(&before);
    for entry in before.log().tx_range(None, None).unwrap() {
        let data = entry.unwrap().data;
        assert!(data.iter().all(|d| d.source == data[0].source));
        assert_eq!(
            data.iter()
                .filter(|d| d.attribute == crate::DB_TX_INSTANT as u32)
                .count(),
            1
        );
    }
    let mut capture = PortableBackup::connect_configured(&source).unwrap();
    let mut restore = PortableBackup::connect_configured(&destination).unwrap();
    let parent = tempfile::tempdir().unwrap();
    let mut checked_current_copy = false;
    let mut checked_history_copy = false;
    let mut ready = false;
    for phase in 0..20 {
        let snapshot = read(&source, &database);
        let descriptor = snapshot.index_descriptor();
        let partial = descriptor.avet_work.iter().any(|work| {
            !work.clearing && (work.directory != 0 || work.leaf != 0 || work.slot != 0)
        });
        let copy_history = descriptor
            .avet_work
            .first()
            .is_some_and(|work| work.history);
        let complete = descriptor.pending_avet.is_empty()
            && descriptor.avet_work.is_empty()
            && snapshot.indexed_basis_t() == snapshot.basis_t();
        let capture_this = phase == 0
            || (partial
                && ((!copy_history && !checked_current_copy)
                    || (copy_history && !checked_history_copy)))
            || complete;
        if capture_this {
            // Capture/restore can take longer than a writer lease. Stop the
            // source writer while checking this immutable publication.
            writer.release().unwrap();
            eprintln!(
                "backup acceptance phase={phase} partial={partial} history={copy_history} complete={complete}"
            );
            let repository = parent.path().join(format!("phase-{phase}"));
            let point = capture.backup_database(&unique, &repository).unwrap();
            let repeated = capture.backup_database(&unique, &repository).unwrap();
            assert_eq!(repeated.objects_written, 0);
            assert_eq!(point.manifest_hash, repeated.manifest_hash);
            assert!(repeated.objects_reused > 0);
            assert_eq!(
                PortableBackup::list_backup_points(&repository)
                    .unwrap()
                    .len(),
                1
            );
            PortableBackup::verify_backup_point_presence(
                &repository,
                point.basis_t,
                point.log_generation,
            )
            .unwrap();
            let verification = PortableBackup::verify_backup_point(
                &repository,
                point.basis_t,
                point.log_generation,
                true,
            )
            .unwrap();
            assert_eq!(
                verification
                    .database
                    .datoms(crate::View::Current, IndexOrder::Eavt),
                snapshot
                    .database_value()
                    .collect_datoms(IndexOrder::Eavt)
                    .unwrap()
            );
            let offline = crate::BackupConnection::open_point(&repository, &point).unwrap();
            assert_reads_equal(&snapshot.database_value(), &offline.db());
            assert_eq!(
                offline
                    .log()
                    .tx_range(None, None)
                    .unwrap()
                    .flat_map(|entry| entry.unwrap().data)
                    .collect::<Vec<_>>(),
                original_log
            );
            if phase == 0 {
                assert_eq!(
                    restore
                        .restore_backup_with_fault(
                            &repository,
                            point.basis_t,
                            &target_name,
                            RestoreFault::AfterFirstContentInserted
                        )
                        .unwrap_err()
                        .code,
                    "backup/restore-after-content"
                );
            }
            let result = restore
                .restore_backup_point(
                    &repository,
                    point.basis_t,
                    point.log_generation,
                    &target_name,
                )
                .unwrap();
            assert_eq!(
                restore
                    .restore_backup_point(
                        &repository,
                        point.basis_t,
                        point.log_generation,
                        &target_name
                    )
                    .unwrap()
                    .activated_root,
                result.activated_root
            );
            let target = BlockDatabase::resolve(&destination, &target_name).unwrap();
            let restored = read(&destination, &target);
            assert_eq!(restored.index_descriptor(), descriptor);
            assert_reads_equal(&snapshot.database_value(), &restored.database_value());
            assert_eq!(log_data(&restored), original_log);
            if !complete {
                assert_pending(&restored.database_value());
            }
            let id = crate::DatabaseCatalog::connect_configured(&destination)
                .unwrap()
                .resolve(&target_name)
                .unwrap()
                .database_id;
            let inspection = PostgresOperator::connect_configured(&destination)
                .unwrap()
                .inspect_database(&id, true)
                .unwrap();
            assert!(
                inspection.healthy(),
                "pending={:?}, problems={:?}",
                descriptor.avet_work,
                inspection.problems
            );
            if partial {
                if copy_history {
                    checked_history_copy = true;
                } else {
                    checked_current_copy = true;
                }
            }
            if complete {
                ready = true;
                // Exact retry preserves the original report/source and basis.
                let mut target_writer =
                    BlockTransactor::claim(&destination, target, BlockWriterOptions::default())
                        .unwrap();
                let retry = target_writer.transact(&retract).unwrap();
                assert_eq!(retry.basis_t, accepted.basis_t);
                assert_eq!(retry.tx_data, accepted.tx_data);
                assert_eq!(
                    target_writer
                        .transact(&request("withdraw-and-replace", 95, vec![]))
                        .unwrap_err()
                        .code,
                    "postgres/idempotency-key-reused"
                );
                target_writer.release().unwrap();
                // Corrupt authenticated content must be rejected in both modes.
                let object = std::fs::read_dir(repository.join("objects"))
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap()
                    .path();
                let bytes = std::fs::read(&object).unwrap();
                let mut corrupt = bytes.clone();
                let last = corrupt.len() - 1;
                corrupt[last] ^= 1;
                std::fs::write(&object, &corrupt).unwrap();
                assert!(
                    PortableBackup::verify_backup_point_presence(
                        &repository,
                        point.basis_t,
                        point.log_generation
                    )
                    .is_err()
                );
                assert!(
                    PortableBackup::verify_backup_point(
                        &repository,
                        point.basis_t,
                        point.log_generation,
                        true
                    )
                    .is_err()
                );
                std::fs::write(object, bytes).unwrap();
                break;
            }
            writer =
                BlockTransactor::claim(&source, database.clone(), BlockWriterOptions::default())
                    .unwrap();
        }
        step(&source, &mut writer);
    }
    assert!(
        checked_current_copy && checked_history_copy && ready,
        "must observe both partial copies and completion"
    );
    assert_eq!(log_data(&read(&source, &database)), original_log);
}

#[test]
fn backup_listing_rejects_old_format_points_without_inventing_source() {
    let parent = tempfile::tempdir().unwrap();
    let directory = parent.path().join("repository");
    repository::admit(&directory, true).unwrap();
    let identity = [1; 16];
    repository::claim(&directory, identity).unwrap();
    let mut payload = identity.to_vec();
    payload.extend_from_slice(&0u64.to_be_bytes());
    payload.extend_from_slice(&0u64.to_be_bytes());
    let mut bytes = Block {
        kind: POINT_KIND,
        links: vec![[2; 32]],
        payload,
    }
    .encode()
    .unwrap();
    bytes[4..6].copy_from_slice(&2u16.to_be_bytes());
    std::fs::write(repository::point_path(&directory, 0, 0), bytes).unwrap();
    assert_eq!(
        PortableBackup::list_backup_points(&directory)
            .unwrap_err()
            .code,
        "storage/block-format"
    );
}
