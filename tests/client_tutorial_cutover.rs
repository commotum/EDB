use edb_core::{EntityRef, TransactionRequest, submission_request_digest};

fn between<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    text.split_once(start)
        .expect("documented example start")
        .1
        .split_once(end)
        .expect("documented example end")
        .0
}

fn schema_data() -> &'static str {
    between(
        include_str!("../docs-2/01_tutorials/06_transact_schema.md"),
        "<<'EDN'\n",
        "\nEDN",
    )
}

fn requests() -> (TransactionRequest, TransactionRequest) {
    let page = include_str!("../docs-2/01_tutorials/10_client_workflow.md");
    let rust_key = between(page, "TransactionRequest::from_edn(\"", "\"");
    let rust_source = between(page, "EntityRef::from_edn(\"", "\")?");
    let cli_key = between(page, "--request-key ", " ");
    let cli_source = between(page, "--source '", "'");
    (
        TransactionRequest::from_edn(
            rust_key,
            schema_data(),
            EntityRef::from_edn(rust_source).unwrap(),
        )
        .unwrap(),
        TransactionRequest::from_edn(
            cli_key,
            schema_data(),
            EntityRef::from_edn(cli_source).unwrap(),
        )
        .unwrap(),
    )
}

fn digest(request: &TransactionRequest) -> edb_core::Digest {
    submission_request_digest(
        &request.forms,
        &request.source,
        request.compare_basis_t,
        request.tx_instant_override,
    )
    .unwrap()
}

#[test]
fn client_tutorial_rust_and_cli_share_exact_request_identity() {
    let (rust, mut cli) = requests();
    assert_eq!(rust.request_key, cli.request_key);
    assert_eq!(digest(&rust), digest(&cli));
    cli.source = EntityRef::from_edn("98").unwrap();
    assert_ne!(digest(&rust), digest(&cli));
}

#[cfg(unix)]
#[test]
#[ignore = "requires EDB_TEST_POSTGRES_URL for a disposable PostgreSQL database"]
fn client_tutorial_sync_and_async_socket_preserve_source_and_exact_retry() {
    use edb_core::{
        AsyncClient, AsyncExecutor, CapacityLimits, Connection, DB_SOURCE_SYSTEM, DB_TX_INSTANT,
        ErrorCategory, LocalTransactionServer, PostgresConnectionConfig, Schema,
        TransactionService, TransactionServiceConfig,
    };
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    let config = PostgresConnectionConfig::parse(
        std::env::var("EDB_TEST_POSTGRES_URL")
            .expect("EDB_TEST_POSTGRES_URL must name a disposable PostgreSQL database"),
    )
    .unwrap();
    let name = format!(
        "client-tutorial-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    edb_core::storage::PgBlockStore::install(&config).unwrap();
    edb_core::storage::BlockDatabase::create(&config, &name, Schema::new()).unwrap();
    let writer = TransactionService::start(TransactionServiceConfig {
        connection: config.clone(),
        database_id: name.clone(),
        holder_id: name.clone(),
        lease_duration: Duration::from_secs(10),
        renew_interval: Duration::from_millis(100),
        queue_capacity: 32,
        capacity_limits: CapacityLimits::default(),
    })
    .unwrap();
    let server = LocalTransactionServer::start(writer.client(), Default::default()).unwrap();
    let conn = Connection::connect_configured(config, &name, 64).unwrap();
    assert_eq!(conn.db().basis_t(), 0);
    let timeout = Duration::from_secs(10);
    let (request, retry) = requests();
    let committed = conn
        .transact_socket(server.endpoint(), request, timeout)
        .unwrap();
    assert!(!committed.replayed);
    let report = committed.report.unwrap();
    assert!(report.tx_data.iter().all(|d| d.source == DB_SOURCE_SYSTEM));
    assert_eq!(
        report
            .tx_data
            .iter()
            .filter(|d| d.attribute == DB_TX_INSTANT as u32 && d.added)
            .count(),
        1
    );
    let edn = edb_core::edn_value::transaction_report_to_edn(
        report.db_before.basis_t(),
        report.db_after.basis_t(),
        &report.tx_data,
        &report.tempids,
    )
    .unwrap();
    let edb_core::edn::EdnValue::Map(fields) = &edn else {
        panic!("report must render as an EDN map");
    };
    let (_, edb_core::edn::EdnValue::Vector(rows)) = fields
        .iter()
        .find(|(key, _)| {
            matches!(key, edb_core::edn::EdnValue::Keyword(k) if k.qualified_name() == "edb/tx-data")
        })
        .expect("report datoms")
    else {
        panic!("report datoms must render as a vector");
    };
    assert!(
        rows.iter().all(|row| {
            matches!(row, edb_core::edn::EdnValue::Vector(slots) if slots.len() == 6)
        })
    );
    edb_core::edn::write_edn(&edn).unwrap();

    let workers = AsyncExecutor::new(Default::default()).unwrap();
    let client = AsyncClient::new(&conn, &workers).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let replay = runtime
        .block_on(async {
            client
                .transact_socket(server.endpoint(), retry.clone(), timeout)?
                .await
        })
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.basis_t, committed.basis_t);
    assert_eq!(replay.tx_hash, committed.tx_hash);
    let replay_report = replay.report.unwrap();
    assert_eq!(replay_report.tx_data, report.tx_data);
    assert_eq!(replay_report.tempids, report.tempids);
    let mut changed_source = retry;
    changed_source.source = EntityRef::from_edn("98").unwrap();
    assert_eq!(
        conn.transact_socket(server.endpoint(), changed_source, timeout)
            .unwrap_err()
            .category,
        ErrorCategory::Conflict
    );
    let fresh = TransactionRequest::from_edn(
        "tutorial-first-client-movie",
        r#"[{:db/id "movie" :movie/title "The Goonies" :movie/release-year 1985}]"#,
        EntityRef::from_edn(":db.source/system").unwrap(),
    )
    .unwrap();
    let movie = runtime
        .block_on(async {
            client
                .transact_socket(server.endpoint(), fresh, timeout)?
                .await
        })
        .unwrap();
    assert!(!movie.replayed);
    assert_eq!(movie.basis_t, committed.basis_t + 1);
    let movie_report = movie.report.unwrap();
    assert!(
        movie_report
            .tx_data
            .iter()
            .all(|d| d.source == DB_SOURCE_SYSTEM)
    );
    assert_eq!(movie_report.tx_data.len(), 3);
    drop(client);
    drop(workers);
    drop(conn);
    drop(server);
    writer.shutdown();
}
