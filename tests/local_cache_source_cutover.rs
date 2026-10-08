use edb_core::{SsdCache, SsdCacheLimits, sha256};
use std::fs::{FileTimes, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

fn private_root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    root
}

fn block_files(directory: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "block")
        })
        .collect()
}

#[test]
fn shared_disk_capacity_and_purge_keep_namespaces_independent() {
    let root = private_root();
    let limits = SsdCacheLimits {
        max_entries: 2,
        max_bytes: 180,
    };
    let namespace = sha256(b"access/lineage/generation-one");
    let other_namespace = sha256(b"access/lineage/generation-two");
    let cache = SsdCache::open(root.path(), namespace, limits).unwrap();
    let other = cache.for_namespace(other_namespace);
    let first = sha256(b"one");
    let second = sha256(b"two");
    assert!(cache.put(&first, b"one"));
    assert_eq!(other.get(&first), None);
    assert!(other.put(&second, b"two"));
    assert_eq!(cache.stats().current_entries, 2);
    assert!(cache.stats().current_bytes <= limits.max_bytes);
    // Make recency deterministic without tying correctness to wall-clock speed.
    let oldest = block_files(root.path())
        .into_iter()
        .find(|path| {
            let bytes = std::fs::read(path).unwrap();
            bytes.ends_with(b"one")
        })
        .unwrap();
    OpenOptions::new()
        .write(true)
        .open(oldest)
        .unwrap()
        .set_times(FileTimes::new().set_modified(SystemTime::UNIX_EPOCH))
        .unwrap();
    let third = sha256(b"three");
    assert!(other.put(&third, b"three"));
    assert_eq!(cache.get(&first), None);
    assert_eq!(other.get(&second), Some(b"two".to_vec()));
    assert_eq!(cache.stats().evictions, 1);
    assert_eq!(cache.stats(), other.stats());
    assert!(cache.purge_namespace(&namespace));
    assert_eq!(cache.stats().current_entries, 2);
    assert!(cache.purge_namespace(&other_namespace));
    assert_eq!(cache.stats().current_entries, 0);
    assert_eq!(cache.stats().purged_entries, 2);
    assert!(block_files(root.path()).is_empty());
}

#[test]
fn reopened_disk_authenticates_bytes_and_preserves_malformed_owned_files() {
    let root = private_root();
    let namespace = sha256(b"canonical-source-facts");
    let limits = SsdCacheLimits::default();
    let bytes = b"[42 :painting/year 1916 12 99 true]";
    let hash = sha256(bytes);
    let cache = SsdCache::open(root.path(), namespace, limits).unwrap();
    assert!(!cache.put(&hash, b"different bytes"));
    assert!(cache.put(&hash, bytes));
    drop(cache);
    let cache = SsdCache::open(root.path(), namespace, limits).unwrap();
    assert_eq!(cache.get(&hash), Some(bytes.to_vec()));
    assert_eq!(cache.stats().hits, 1);
    assert!(cache.stats().physical_read_bytes > cache.stats().canonical_read_bytes);
    let path = block_files(root.path()).pop().unwrap();
    let mut file = OpenOptions::new().write(true).open(&path).unwrap();
    file.seek(SeekFrom::End(-1)).unwrap();
    file.write_all(b"!").unwrap();
    drop(file);
    assert_eq!(cache.get(&hash), None);
    assert!(!path.exists());
    assert_eq!(cache.stats().corruptions, 1);
    assert!(cache.put(&hash, bytes));
    let path = block_files(root.path()).pop().unwrap();
    std::fs::write(&path, b"incomplete framing").unwrap();
    drop(cache);
    let cache = SsdCache::open(root.path(), namespace, limits).unwrap();
    let stats = cache.stats();
    assert!(!stats.inventory_complete);
    assert_eq!(stats.current_entries, 1);
    assert_eq!(stats.current_bytes, b"incomplete framing".len() as u64);
    assert_eq!(cache.get(&hash), None);
    assert!(!cache.put(&sha256(b"new"), b"new"));
    assert!(!cache.purge_namespace(&namespace));
    assert_eq!(std::fs::read(path).unwrap(), b"incomplete framing");
}

#[test]
fn disk_open_rejects_unclaimed_files_unsafe_roots_and_conflicting_limits() {
    let root = private_root();
    let namespace = sha256(b"private-root");
    let limits = SsdCacheLimits::default();
    std::fs::write(root.path().join("application.txt"), b"keep me").unwrap();
    assert!(SsdCache::open(root.path(), namespace, limits).is_err());
    assert_eq!(
        std::fs::read(root.path().join("application.txt")).unwrap(),
        b"keep me"
    );
    let private = root.path().join("private");
    std::fs::create_dir(&private).unwrap();
    std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
    SsdCache::open(&private, namespace, limits).unwrap();
    assert!(
        SsdCache::open(
            &private,
            namespace,
            SsdCacheLimits {
                max_entries: limits.max_entries + 1,
                ..limits
            },
        )
        .is_err()
    );
    std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(SsdCache::open(&private, namespace, limits).is_err());
    std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink(&private, &alias).unwrap();
    assert!(SsdCache::open(alias, namespace, limits).is_err());
}

#[test]
#[ignore = "requires EDB_TEST_POSTGRES_URL for a disposable PostgreSQL database"]
fn source_bytes_reuse_disk_after_peer_restart_without_acquiring_authority() {
    use edb_core::storage::{BlockDatabase, PgBlockStore};
    use edb_core::{
        Attribute, BlockTransactor, BlockWriterOptions, CacheTier, Cardinality, EntityRef,
        IndexPrefix, Keyword, OperationContext, OperationKind, Peer, PostgresConnectionConfig,
        PostgresOperator, Schema, SsdCacheConfig, TransactionRequest, TxOp, Value, ValueType,
    };
    use std::time::UNIX_EPOCH;

    let parameters = std::env::var("EDB_TEST_POSTGRES_URL")
        .expect("EDB_TEST_POSTGRES_URL must name disposable PostgreSQL");
    let config = PostgresConnectionConfig::parse(&parameters).unwrap();
    let name = format!(
        "local-cache-source-{}-{}",
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
            "first",
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
            "second",
            vec![TxOp::Add {
                entity: EntityRef::Id(item),
                attribute: 1000,
                value: Value::Long(7).into(),
            }],
            EntityRef::Id(98),
        ))
        .unwrap();
    writer.release().unwrap();
    PostgresOperator::connect_configured(&config)
        .unwrap()
        .consolidate_database(&database_id)
        .unwrap();

    let root = private_root();
    let cached = config.with_ssd_cache(SsdCacheConfig {
        directory: root.path().to_owned(),
        limits: SsdCacheLimits::default(),
    });
    // No decoded retention: the second peer must obtain its bytes from disk.
    let read_sources = |peer: &Peer| {
        for history in [false, true] {
            let db = if history {
                peer.db().history()
            } else {
                peer.db()
            };
            for (source, prefix) in [
                (
                    99,
                    IndexPrefix::Saevt {
                        source: 99,
                        attribute: Some(1000),
                        entity: None,
                        value: None,
                    },
                ),
                (
                    98,
                    IndexPrefix::Savet {
                        source: 98,
                        attribute: Some(1000),
                        value: None,
                        entity: None,
                    },
                ),
            ] {
                let datoms = db
                    .prefix_cursor(&prefix)
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
                assert_eq!(datoms.len(), 1);
                assert_eq!(datoms[0].entity, item);
                assert_eq!(datoms[0].source, source);
                assert_eq!(datoms[0].value, Value::Long(7));
                assert!(datoms[0].added);
            }
        }
    };
    let peer = Peer::connect_configured_with_cache_limits(&cached, &name, 0, 0).unwrap();
    read_sources(&peer);
    assert!(peer.ssd_cache_stats().puts > 0);
    drop(peer);
    let peer = Peer::connect_configured_with_cache_limits(&cached, &name, 0, 0).unwrap();
    let context = OperationContext::diagnostic(OperationKind::Query);
    {
        let _scope = context.enter();
        read_sources(&peer);
    }
    let reads = context.report().stats.reads;
    assert!(reads.cache[&CacheTier::LocalDisk].hits > 0);
    assert!(!reads.cache.contains_key(&CacheTier::PostgresBlock));
    assert_eq!(peer.load_stats().cursor_sql_reads, 0);
    assert!(peer.ssd_cache_stats().hits > 0);

    // Different raw connection parameters intentionally partition the same
    // server's cache. Check source content again across this access boundary.
    let changed_parameters =
        if parameters.starts_with("postgres://") || parameters.starts_with("postgresql://") {
            format!(
                "{parameters}{}application_name=edb-local-cache-other-access",
                if parameters.contains('?') { "&" } else { "?" }
            )
        } else {
            format!("{parameters} application_name=edb-local-cache-other-access")
        };
    let changed = PostgresConnectionConfig::parse(changed_parameters)
        .unwrap()
        .with_ssd_cache(SsdCacheConfig {
            directory: root.path().to_owned(),
            limits: SsdCacheLimits::default(),
        });
    let original_files = block_files(root.path());
    let other = Peer::connect_configured_with_cache_limits(&changed, &name, 0, 0).unwrap();
    // Opening can hit entries it admitted earlier in that same open. Cold
    // misses and new filenames show this access config cannot reuse old entries.
    assert!(other.ssd_cache_stats().misses > 0);
    assert!(
        block_files(root.path())
            .iter()
            .any(|path| !original_files.contains(path))
    );
    read_sources(&other);
    assert!(other.ssd_cache_stats().puts > 0);
    let before_purge = block_files(root.path()).len();
    assert!(other.purge_ssd_generation(other.excision_generation()));
    assert!(block_files(root.path()).len() < before_purge);
    assert!(original_files.iter().all(|path| path.exists()));
    // Purging this access namespace cannot discard the first reader's entries.
    let context = OperationContext::diagnostic(OperationKind::Query);
    {
        let _scope = context.enter();
        read_sources(&peer);
    }
    assert!(
        !context
            .report()
            .stats
            .reads
            .cache
            .contains_key(&CacheTier::PostgresBlock)
    );
    let unavailable = PostgresConnectionConfig::parse(format!(
        "host={} port=1 user=edb_cache_test dbname=edb_cache_test sslmode=disable connect_timeout=1",
        root.path().join("missing-socket").display(),
    )).unwrap().with_ssd_cache(SsdCacheConfig { directory: root.path().to_owned(), limits: SsdCacheLimits::default() });
    assert!(Peer::connect_configured_with_cache_limits(&unavailable, &name, 0, 0).is_err());
    // Cache remains usable under the already authenticated captured reader.
    read_sources(&peer);
    assert!(peer.purge_ssd_generation(peer.excision_generation()));
    assert!(block_files(root.path()).is_empty());
}
