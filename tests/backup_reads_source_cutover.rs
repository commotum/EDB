use edb_core::storage::{BlockDatabase, BlockReader, PgBlockStore};
use edb_core::{
    Attribute, BackupConnection, BackupReadConfig, BlockTransactor, BlockWriterOptions,
    Cardinality, DB_FN, DB_IDENT, DB_TX_INSTANT, EntityRef, FulltextOptions, IndexOrder,
    IndexPrefix, IndexTransaction, Instruction, InvokeControl, Keyword, PortableBackup,
    PostgresConnectionConfig, PostgresOperator, Program, ProgramKind, ProgramOutput, Schema,
    TransactionRequest, TxOp, Value, ValueType,
};
use std::time::{SystemTime, UNIX_EPOCH};

const TEXT: u32 = 1000;
const ITEM: u64 = 42;
fn add(entity: u64, attribute: u32, value: Value) -> TxOp {
    TxOp::Add {
        entity: EntityRef::Id(entity),
        attribute,
        value: value.into(),
    }
}
fn submit(
    writer: &mut BlockTransactor,
    key: &str,
    source: u64,
    ops: Vec<TxOp>,
) -> edb_core::ServiceTransactionReport {
    writer
        .transact(&TransactionRequest::new(key, ops, EntityRef::Id(source)).with_tx_instant(1000))
        .unwrap()
}
fn hits(db: &edb_core::DatabaseValue) -> usize {
    db.fulltext(TEXT, "river", &FulltextOptions::default())
        .unwrap()
        .hits
        .len()
}
fn cli(repository: &std::path::Path, args: &[&str]) -> std::process::Output {
    let environment: Vec<_> = std::env::vars_os()
        .filter(|(key, _)| !key.to_string_lossy().starts_with("EDB_"))
        .collect();
    std::process::Command::new(env!("CARGO_BIN_EXE_edb"))
        .env_clear()
        .envs(environment)
        .args(args)
        .arg("--repository")
        .arg(repository)
        .output()
        .unwrap()
}

#[test]
#[ignore = "requires EDB_TEST_POSTGRES_URL for a disposable PostgreSQL database"]
fn backup_values_keep_offline_source_reads_programs_and_fixed_authority() {
    let config = PostgresConnectionConfig::parse(
        std::env::var("EDB_TEST_POSTGRES_URL")
            .expect("explicit disposable PostgreSQL configuration required"),
    )
    .unwrap();
    PgBlockStore::install(&config).unwrap();
    let name = format!(
        "backup-reads-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let database = BlockDatabase::create(&config, &name, Schema::new()).unwrap();
    let mut writer =
        BlockTransactor::claim(&config, database.clone(), BlockWriterOptions::default()).unwrap();
    submit(
        &mut writer,
        "schema",
        99,
        vec![TxOp::InstallAttribute(
            Attribute::new(
                TEXT,
                Keyword::new("article", "text"),
                ValueType::String,
                Cardinality::One,
            )
            .fulltext(),
        )],
    );
    let text = Value::String("Blue River".into());
    let first = submit(
        &mut writer,
        "first",
        99,
        vec![add(ITEM, TEXT, text.clone())],
    );
    let second = submit(
        &mut writer,
        "second",
        98,
        vec![add(ITEM, TEXT, text.clone())],
    );
    let mut operator = PostgresOperator::connect_configured(&config).unwrap();
    let hash = operator
        .deploy_program(&Program {
            kind: ProgramKind::Query,
            arity: 0,
            instructions: vec![
                Instruction::PushEntity(EntityRef::Id(ITEM)),
                Instruction::LoadOne(TEXT),
                Instruction::EmitRow(1),
                Instruction::Return,
            ],
        })
        .unwrap();
    submit(
        &mut writer,
        "program",
        99,
        vec![
            TxOp::Add {
                entity: EntityRef::Temp("program".into()),
                attribute: DB_IDENT as u32,
                value: Value::Keyword(Keyword::new("test", "read")).into(),
            },
            TxOp::Add {
                entity: EntityRef::Temp("program".into()),
                attribute: DB_FN as u32,
                value: Value::Function(hash).into(),
            },
        ],
    );
    writer.release().unwrap();
    operator
        .consolidate_database(database.reference_key().strip_prefix("databases/").unwrap())
        .unwrap();
    let live = BlockReader::connect(&config, Default::default())
        .unwrap()
        .capture(&database.reference_key())
        .unwrap()
        .database_value();
    assert_eq!(hits(&live), 2);
    let parent = tempfile::tempdir().unwrap();
    let repository = parent.path().join("repository");
    let mut capture = PortableBackup::connect_configured(&config).unwrap();
    let point = capture.backup_database(&name, &repository).unwrap();
    let offline = BackupConnection::open_configured(
        &repository,
        Some(&point),
        BackupReadConfig {
            cache_entries: 0,
            cache_bytes: 0,
        },
    )
    .unwrap();
    let db = offline.db();
    assert_eq!(db.basis_t(), live.basis_t());
    assert_eq!(db.schema(), live.schema());
    assert!(db.snapshot_reference().is_err());
    assert_eq!(db.values(ITEM, TEXT).unwrap(), std::slice::from_ref(&text));
    let Some(edb_core::EntityValue::Scalar(entity_value)) =
        db.entity(ITEM).unwrap().unwrap().get(TEXT).unwrap()
    else {
        panic!("cardinality-one entity field must be scalar")
    };
    assert_eq!(entity_value, text);
    let pattern = edb_core::edn_pull::parse_pull_edn("[:article/text]").unwrap();
    assert_eq!(
        db.pull(&pattern, ITEM).unwrap(),
        live.pull(&pattern, ITEM).unwrap()
    );

    assert_eq!(hits(&db), 2);
    assert_eq!(hits(&db.clone().filter(|_, d| d.source == 99)), 1);
    assert_eq!(hits(&db.clone().since(first.basis_t)), 1);
    assert_eq!(hits(&db.clone().as_of(first.basis_t)), 1);
    let ProgramOutput::Query(rows) = db
        .invoke(Keyword::new("test", "read"), &[], InvokeControl::default())
        .unwrap()
    else {
        panic!("query program must return rows")
    };
    assert_eq!(rows, vec![vec![text.clone()]]);
    let activity = db
        .datoms_with_prefix(&IndexPrefix::Saevt {
            source: 99,
            attribute: Some(DB_TX_INSTANT as u32),
            entity: None,
            value: None,
        })
        .unwrap();
    assert_eq!(offline.log().basis_t(), db.basis_t());
    for timestamp in activity {
        let data = offline
            .log()
            .tx_data(IndexTransaction::Tx(timestamp.entity))
            .unwrap()
            .unwrap();
        assert!(data.iter().all(|d| d.source == 99));
        assert_eq!(
            data.iter()
                .filter(|d| d.attribute == DB_TX_INSTANT as u32)
                .count(),
            1
        );
    }
    let speculative = db
        .with(
            &[TxOp::Retract {
                entity: EntityRef::Id(ITEM),
                attribute: TEXT,
                value: Some(text.clone().into()),
            }],
            1000,
            EntityRef::Id(97),
        )
        .unwrap();
    assert!(speculative.tx_data.iter().all(|d| d.source == 97));
    assert_eq!(
        hits(&speculative.db_after.clone().filter(|_, d| d.source == 99)),
        0
    );
    assert_eq!(hits(&speculative.db_after.clone().history()), 2);
    assert_eq!(hits(&db), 2);
    assert_eq!(offline.cache_stats().current_bytes, 0);
    assert_eq!(offline.cache_stats().current_entries, 0);
    assert!(offline.read_stats().object_reads > 0);
    assert!(offline.read_stats().leaf_reads > 0);

    // Later publication advances the default selection while this handle and
    // its captured log remain fixed, including a withdrawal by another source.
    writer = BlockTransactor::claim(&config, database, BlockWriterOptions::default()).unwrap();
    let withdrawn = submit(
        &mut writer,
        "withdraw",
        97,
        vec![TxOp::Retract {
            entity: EntityRef::Id(ITEM),
            attribute: TEXT,
            value: Some(text.clone().into()),
        }],
    );
    writer.release().unwrap();
    let latest = capture.backup_database(&name, &repository).unwrap();
    assert!(latest.basis_t > point.basis_t);
    assert_eq!(offline.point().manifest_hash, point.manifest_hash);
    assert_eq!(offline.point().basis_t, point.basis_t);
    assert_eq!(hits(&offline.db()), 2);
    assert_eq!(
        offline
            .log()
            .tx_data(IndexTransaction::T(withdrawn.basis_t))
            .unwrap(),
        None
    );
    let new = BackupConnection::open(&repository).unwrap();
    assert_eq!(hits(&new.db().filter(|_, d| d.source == 99)), 0);
    assert_eq!(hits(&new.db().history()), 2);
    assert_eq!(
        new.log()
            .tx_data(IndexTransaction::T(second.basis_t))
            .unwrap()
            .unwrap(),
        second.tx_data
    );

    let query = parent.path().join("query.edn");
    std::fs::write(&query, "[:find ?s :where [42 :article/text _ _ ?s]]").unwrap();
    let coordinates = [point.basis_t.to_string(), point.log_generation.to_string()];
    let output = cli(
        &repository,
        &[
            "query",
            "--file",
            query.to_str().unwrap(),
            "--backup-basis",
            &coordinates[0],
            "--backup-generation",
            &coordinates[1],
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result = String::from_utf8(output.stdout).unwrap();
    assert!(result.contains("98") && result.contains("99"), "{result}");
    let preview = parent.path().join("preview.edn");
    std::fs::write(&preview, "[[:db/add 42 :article/text \"Green River\"]]").unwrap();
    let output = cli(
        &repository,
        &[
            "with",
            "--file",
            preview.to_str().unwrap(),
            "--source",
            "96",
            "--tx-instant",
            "1000",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        BackupConnection::open(&repository)
            .unwrap()
            .point()
            .manifest_hash,
        latest.manifest_hash
    );

    let cached = BackupConnection::open_point(&repository, &point).unwrap();
    assert_eq!(
        cached.db().values(ITEM, TEXT).unwrap(),
        std::slice::from_ref(&text)
    );

    // A cold, uncached leaf is authenticated at consumption. The cursor must
    // return one error and fuse; restoring the file makes a new cursor usable.
    let leaf_path = std::fs::read_dir(repository.join("objects"))
        .unwrap()
        .map(Result::unwrap)
        .find_map(|entry| {
            let file = std::fs::read(entry.path()).unwrap();
            let name = entry.file_name();
            let name = name.to_str().unwrap();
            let mut id = [0u8; 32];
            for (i, byte) in id.iter_mut().enumerate() {
                *byte = u8::from_str_radix(&name[i * 2..i * 2 + 2], 16).unwrap();
            }
            let bytes = if file.starts_with(b"EDBBLOCK") {
                use std::io::Read;
                let mut bytes = Vec::new();
                flate2::read::GzDecoder::new(&file[26..])
                    .read_to_end(&mut bytes)
                    .unwrap();
                bytes
            } else {
                file
            };
            assert_eq!(edb_core::sha256(&bytes), id);
            match edb_core::index::tree::decode_tree_node(&id, &bytes) {
                Ok(edb_core::index::tree::TreeNode::Leaf(leaf))
                    if leaf.order == IndexOrder::Eavt
                        && !leaf.history
                        && (0..leaf.len()).any(|i| leaf.datom(i).unwrap().attribute == TEXT) =>
                {
                    Some(entry.path())
                }
                _ => None,
            }
        })
        .unwrap();
    let saved = std::fs::read(&leaf_path).unwrap();
    std::fs::write(&leaf_path, b"corrupt").unwrap();
    assert_eq!(
        cached.db().values(ITEM, TEXT).unwrap(),
        std::slice::from_ref(&text)
    );
    let mut cursor = db
        .prefix_cursor(&IndexPrefix::Eavt {
            entity: ITEM,
            attribute: Some(TEXT),
            value: None,
        })
        .unwrap();
    assert!(cursor.next().unwrap().is_err());
    assert!(cursor.next().is_none());
    assert!(cursor.next().is_none());
    std::fs::write(&leaf_path, saved).unwrap();
    assert_eq!(db.values(ITEM, TEXT).unwrap(), [text]);
}
