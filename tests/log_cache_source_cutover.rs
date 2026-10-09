use edb_core::storage::{BlockDatabase, PgBlockStore};
use edb_core::{
    Attribute, BlockTransactor, BlockWriterOptions, Cardinality, EntityRef, IndexTransaction,
    Keyword, LogCursorStats, LogTransaction, LogValue, OperationContext, OperationKind, Peer,
    PostgresConnectionConfig, PostgresOperator, Schema, SsdCacheConfig, SsdCacheLimits, TimePoint,
    TransactionRequest, TxOp, Value, ValueType, canonical_datom_bytes,
};
use std::os::unix::fs::PermissionsExt;
use std::time::{SystemTime, UNIX_EPOCH};

fn private_cache() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    directory
}

fn setup() -> (PostgresConnectionConfig, String, String, BlockTransactor) {
    let config = PostgresConnectionConfig::parse(
        std::env::var("EDB_TEST_POSTGRES_URL")
            .expect("EDB_TEST_POSTGRES_URL must name disposable PostgreSQL"),
    )
    .unwrap();
    let name = format!(
        "log-cache-source-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    );
    PgBlockStore::install(&config).unwrap();
    let database = BlockDatabase::create(&config, &name, Schema::new()).unwrap();
    let database_id = database
        .reference_key()
        .strip_prefix("databases/")
        .unwrap()
        .to_owned();
    let writer = BlockTransactor::claim(&config, database, BlockWriterOptions::default()).unwrap();
    (config, name, database_id, writer)
}

fn read(log: &LogValue, start: u64) -> (Vec<LogTransaction>, LogCursorStats, u64) {
    let work = OperationContext::new(OperationKind::Query);
    let mut cursor = {
        let _scope = work.enter();
        log.tx_range(Some(TimePoint::T(start)), None).unwrap()
    };
    assert_eq!(cursor.stats(), LogCursorStats::default());
    assert_eq!(work.snapshot().sql_calls, 0, "integer bounds must be lazy");
    // Cursor consumption retains the context established during construction.
    let data = cursor.by_ref().collect::<Result<Vec<_>, _>>().unwrap();
    assert!(cursor.next().is_none());
    assert!(cursor.next().is_none());
    (data, cursor.stats(), work.snapshot().sql_calls)
}

fn logical_bytes(transactions: &[LogTransaction]) -> u64 {
    transactions
        .iter()
        .map(|transaction| {
            40 + transaction
                .data
                .iter()
                .map(|datom| canonical_datom_bytes(datom).unwrap().len() as u64)
                .sum::<u64>()
        })
        .sum()
}

fn cached_config(
    config: &PostgresConnectionConfig,
    root: &tempfile::TempDir,
    bytes: u64,
) -> PostgresConnectionConfig {
    config.clone().with_ssd_cache(SsdCacheConfig {
        directory: root.path().to_owned(),
        limits: SsdCacheLimits {
            max_entries: 4096,
            max_bytes: bytes,
        },
    })
}

#[test]
#[ignore = "requires EDB_TEST_POSTGRES_URL for a disposable PostgreSQL database"]
fn uniform_log_objects_are_lazy_then_warm_across_pages_and_peer_reopen() {
    let (config, name, database_id, mut writer) = setup();
    let mut expected = Vec::new();
    // Three pages exercise retained forward navigation, not just the tail.
    for number in 0..130 {
        let source = if number % 2 == 0 { 99 } else { 98 };
        let report = writer
            .transact(&TransactionRequest::new(
                format!("empty-{number}"),
                vec![],
                EntityRef::Id(source),
            ))
            .unwrap();
        assert_eq!(report.tx_data.len(), 1);
        assert_eq!(report.tx_data[0].source, source);
        expected.push(LogTransaction {
            t: report.basis_t,
            data: report.tx_data,
        });
    }
    writer.release().unwrap();
    PostgresOperator::connect_configured(&config)
        .unwrap()
        .consolidate_database(&database_id)
        .unwrap();
    let directory = private_cache();
    let cached = cached_config(&config, &directory, 16 * 1024 * 1024);
    let peer = Peer::connect_configured_with_cache_limits(&cached, &name, 0, 0).unwrap();
    let log = peer.log();
    assert_eq!(log.basis_t(), 130);
    assert!(peer.purge_ssd_generation(log.generation()));
    let (cold, cold_stats, cold_sql) = read(&log, 1);
    assert_eq!(cold, expected);
    assert_eq!(cold_stats.range_reads, 130);
    assert_eq!(cold_stats.transactions_read, 130);
    assert_eq!(cold_stats.datoms_read, 130);
    assert_eq!(cold_stats.peak_buffered_transactions, 1);
    assert_eq!(cold_stats.cache_hits, 0);
    assert_eq!(cold_stats.payload_bytes_read, logical_bytes(&cold));
    assert!(cold_stats.postgres_payload_bytes_read > cold_stats.payload_bytes_read);
    assert!(cold_sql > 0);
    let (warm, warm_stats, warm_sql) = read(&log, 1);
    assert_eq!(warm, expected);
    assert_eq!(warm_stats.cache_hits, 130);
    assert_eq!(warm_stats.postgres_payload_bytes_read, 0);
    assert_eq!(warm_stats.payload_bytes_read, cold_stats.payload_bytes_read);
    assert_eq!(warm_sql, 0);
    drop(peer);
    let peer = Peer::connect_configured_with_cache_limits(&cached, &name, 0, 0).unwrap();
    let (reopened, reopened_stats, reopened_sql) = read(&peer.log(), 1);
    assert_eq!(reopened, expected);
    assert_eq!(reopened_stats, warm_stats);
    assert_eq!(reopened_sql, 0);
    let data_work = OperationContext::new(OperationKind::Query);
    {
        let _scope = data_work.enter();
        assert_eq!(
            log.tx_data(IndexTransaction::T(1)).unwrap().unwrap(),
            expected[0].data
        );
    }
    assert_eq!(data_work.snapshot().sql_calls, 0);

    let database = BlockDatabase::resolve(&config, &name).unwrap();
    let mut successor =
        BlockTransactor::claim(&config, database, BlockWriterOptions::default()).unwrap();
    let later = successor
        .transact(&TransactionRequest::new("later", vec![], EntityRef::Id(98)))
        .unwrap();
    successor.release().unwrap();
    assert_eq!(later.basis_t, 131);
    assert_eq!(log.basis_t(), 130);
    assert_eq!(log.tx_data(IndexTransaction::T(131)).unwrap(), None);
    let (retained, retained_stats, retained_sql) = read(&log, 1);
    assert_eq!(retained, expected);
    assert_eq!(retained_stats, warm_stats);
    assert_eq!(retained_sql, 0);
    println!(
        "log cache cold_sql={cold_sql} cold_pg_bytes={} logical_bytes={} warm_sql={warm_sql} reopened_sql={reopened_sql} transactions=130",
        cold_stats.postgres_payload_bytes_read, cold_stats.payload_bytes_read
    );
}

#[test]
#[ignore = "requires EDB_TEST_POSTGRES_URL for a disposable PostgreSQL database"]
fn rejected_body_chunks_preserve_metadata_reuse_and_report_partial_lookup_bytes() {
    let (config, name, database_id, mut writer) = setup();
    writer
        .transact(&TransactionRequest::new(
            "schema",
            vec![TxOp::InstallAttribute(Attribute::new(
                1000,
                Keyword::new("item", "body"),
                ValueType::String,
                Cardinality::One,
            ))],
            EntityRef::Id(99),
        ))
        .unwrap();
    // A varied body stays larger than the physical cache limit after compression.
    let mut state = 0x352a_81fc_916b_778du64;
    let body: String = (0..128 * 1024)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            char::from(b'!' + (state % 90) as u8)
        })
        .collect();
    let report = writer
        .transact(&TransactionRequest::new(
            "body",
            vec![TxOp::Add {
                entity: EntityRef::Temp("item".into()),
                attribute: 1000,
                value: Value::String(body).into(),
            }],
            EntityRef::Id(98),
        ))
        .unwrap();
    writer.release().unwrap();
    PostgresOperator::connect_configured(&config)
        .unwrap()
        .consolidate_database(&database_id)
        .unwrap();
    let directory = private_cache();
    let cached = cached_config(&config, &directory, 32 * 1024);
    let peer = Peer::connect_configured_with_cache_limits(&cached, &name, 0, 0).unwrap();
    let log = peer.log();
    assert!(peer.purge_ssd_generation(log.generation()));
    let (cold, cold_stats, cold_sql) = read(&log, report.basis_t);
    assert_eq!(
        cold,
        [LogTransaction {
            t: report.basis_t,
            data: report.tx_data
        }]
    );
    let (mixed, mixed_stats, mixed_sql) = read(&log, report.basis_t);
    assert_eq!(mixed, cold);
    assert!(
        mixed
            .iter()
            .flat_map(|tx| &tx.data)
            .all(|datom| datom.source == 98)
    );
    assert_eq!(mixed_stats.transactions_read, 1);
    assert_eq!(mixed_stats.payload_bytes_read, logical_bytes(&mixed));
    assert_eq!(
        mixed_stats.payload_bytes_read,
        cold_stats.payload_bytes_read
    );
    assert_eq!(
        mixed_stats.cache_hits, 0,
        "metadata hit alone is not a transaction hit"
    );
    assert!(mixed_stats.postgres_payload_bytes_read > 0);
    assert!(mixed_stats.postgres_payload_bytes_read < cold_stats.postgres_payload_bytes_read);
    assert!(mixed_sql > 0 && mixed_sql < cold_sql);
    assert!(peer.ssd_cache_stats().hits > 0);
    assert!(peer.ssd_cache_stats().oversized_bypasses >= 2);
    assert!(peer.ssd_cache_stats().current_bytes <= 32 * 1024);
    println!(
        "log mixed cold_sql={cold_sql} mixed_sql={mixed_sql} cold_pg_bytes={} mixed_pg_bytes={} logical_bytes={} oversized_bypasses={}",
        cold_stats.postgres_payload_bytes_read,
        mixed_stats.postgres_payload_bytes_read,
        mixed_stats.payload_bytes_read,
        peer.ssd_cache_stats().oversized_bypasses
    );
}
