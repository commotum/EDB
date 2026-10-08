use edb_core::{
    Attribute, Cardinality, Database, EntityRef, HintLimits, HintPrefetchOptions, IndexPrefix,
    Keyword, ReadHint, Schema, SpeculationLimits, TransactionHints, TransactionRequest, TxOp,
    Unique, Value, ValueType,
};

const NAME: u32 = 1_000;
const LABEL: u32 = 1_001;

fn schema() -> Schema {
    let mut schema = Schema::new();
    schema
        .install(
            Attribute::new(
                NAME,
                Keyword::new("source", "name"),
                ValueType::String,
                Cardinality::One,
            )
            .unique(Unique::Identity),
        )
        .unwrap();
    schema
        .install(Attribute::new(
            LABEL,
            Keyword::new("item", "label"),
            ValueType::String,
            Cardinality::One,
        ))
        .unwrap();
    schema
}

fn add(entity: EntityRef, attribute: u32, text: &str) -> TxOp {
    TxOp::Add {
        entity,
        attribute,
        value: Value::String(text.into()).into(),
    }
}

#[test]
fn bounded_hinted_preview_keeps_ordinary_same_transaction_source_resolution() {
    let db = Database::new(schema()).unwrap().database_value();
    let request = TransactionRequest::new(
        "preview",
        vec![
            add(EntityRef::Temp("author".into()), NAME, "Julia"),
            add(EntityRef::Temp("item".into()), LABEL, "Peaches"),
        ],
        EntityRef::Temp("author".into()),
    );
    let ordinary = db
        .with_forms(&request.forms, 1, request.source.clone())
        .unwrap();
    let hinted = db
        .with_forms_with_hints(
            &request.forms,
            1,
            request.source,
            SpeculationLimits::default(),
            HintLimits {
                max_prefixes: 0,
                max_bytes: 0,
            },
        )
        .unwrap();
    assert_eq!(hinted.report.tx_data, ordinary.tx_data);
    assert_eq!(hinted.report.tempids, ordinary.tempids);
    assert!(hinted.hints.is_none()); // Eager values have no committed origin.
    assert_eq!(hinted.stats.admitted_prefixes, 0);
    assert_eq!(hinted.stats.retained_bytes, 0);
    assert!(hinted.stats.omitted_requests > 0);
    let author = hinted.report.tempids["author"];
    assert!(
        hinted
            .report
            .tx_data
            .iter()
            .all(|datom| datom.source == author)
    );
    assert_eq!(
        hinted
            .report
            .tx_data
            .iter()
            .filter(|datom| datom.attribute == edb_core::DB_TX_INSTANT as u32)
            .count(),
        1
    );
}

#[test]
#[ignore = "requires EDB_TEST_POSTGRES_URL for a disposable PostgreSQL database and loopback TLS"]
fn source_prefix_hints_remain_advisory_across_in_process_and_remote_exact_retries() {
    use edb_core::{
        CapacityLimits, Connection, PostgresConnectionConfig, RemoteAuthToken, RemoteClientConfig,
        RemoteTransactionEndpoint, RemoteTransportConfig, TransactionService,
        TransactionServiceConfig,
    };
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    let config = PostgresConnectionConfig::parse(
        std::env::var("EDB_TEST_POSTGRES_URL")
            .expect("EDB_TEST_POSTGRES_URL must name a disposable PostgreSQL database"),
    )
    .unwrap();
    let name = format!(
        "transaction-hints-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    edb_core::storage::PgBlockStore::install(&config).unwrap();
    edb_core::storage::BlockDatabase::create(&config, &name, schema()).unwrap();
    let writer = TransactionService::start(TransactionServiceConfig {
        connection: config.clone(),
        database_id: name.clone(),
        holder_id: name,
        lease_duration: Duration::from_secs(10),
        renew_interval: Duration::from_millis(100),
        queue_capacity: 16,
        capacity_limits: CapacityLimits::default(),
    })
    .unwrap();
    let conn = Connection::attach_configured(config.clone(), writer.client(), 64).unwrap();
    let timeout = Duration::from_secs(10);
    let source_report = conn
        .transact(
            TransactionRequest::new(
                "author",
                vec![add(EntityRef::Temp("author".into()), NAME, "Julia")],
                EntityRef::Id(edb_core::DB_SOURCE_SYSTEM),
            ),
            timeout,
        )
        .unwrap();
    let author = source_report.tempids["author"];
    let base = conn.db();
    let request = TransactionRequest::new(
        "original",
        vec![add(EntityRef::Temp("item".into()), LABEL, "Peaches")],
        EntityRef::Edn(r#"[:source/name "Julia"]"#.into()),
    )
    .comparing_basis(base.basis_t());
    let preview = base
        .with_forms_with_hints(
            &request.forms,
            base.last_tx_instant().unwrap(),
            request.source.clone(),
            SpeculationLimits::default(),
            HintLimits::default(),
        )
        .unwrap();
    assert!(preview.stats.admitted_prefixes > 0);
    assert!(
        preview
            .report
            .tx_data
            .iter()
            .all(|datom| datom.source == author)
    );
    let generated = preview.hints.unwrap();
    assert_eq!(generated.origin(), &base.snapshot_reference().unwrap());
    let source_hints = TransactionHints::from_reads(
        generated.origin().clone(),
        [
            ReadHint {
                history: false,
                prefix: IndexPrefix::Saevt {
                    source: author,
                    attribute: Some(NAME),
                    entity: None,
                    value: None,
                },
            },
            ReadHint {
                history: true,
                prefix: IndexPrefix::Savet {
                    source: author,
                    attribute: Some(NAME),
                    value: Some(Value::String("Julia".into())),
                    entity: None,
                },
            },
        ],
        HintLimits::default(),
    )
    .unwrap();
    let (ticket, execution) = conn
        .submit_with_hints(request.clone(), generated, HintPrefetchOptions::default())
        .unwrap();
    let accepted = ticket.wait(timeout).unwrap();
    assert!(!accepted.replayed);
    assert!(accepted.tx_data.iter().all(|datom| datom.source == author));
    assert_eq!(execution.snapshot().attempts, 1);
    assert_eq!(execution.snapshot().join_nanos, 0);

    // The source lookup and basis guard are now invalid for fresh assessment.
    conn.transact(
        TransactionRequest::new(
            "withdraw-author-name",
            vec![TxOp::Retract {
                entity: EntityRef::Id(author),
                attribute: NAME,
                value: Some(Value::String("Julia".into()).into()),
            }],
            EntityRef::Id(edb_core::DB_SOURCE_SYSTEM),
        ),
        timeout,
    )
    .unwrap();
    assert_eq!(
        conn.db()
            .lookup(NAME, &Value::String("Julia".into()))
            .unwrap(),
        None
    );
    let options = HintPrefetchOptions {
        max_datoms: 0,
        ..Default::default()
    };
    let replayed = conn
        .submit_with_hints(request.clone(), source_hints.clone(), options)
        .unwrap()
        .0
        .wait(timeout)
        .unwrap();
    assert!(replayed.replayed);
    assert_eq!(replayed.basis_t, accepted.basis_t);
    assert_eq!(replayed.tx_hash, accepted.tx_hash);
    assert_eq!(replayed.tx_data, accepted.tx_data);
    assert_eq!(replayed.tempids, accepted.tempids);
    assert!(conn.transact(request.clone(), timeout).unwrap().replayed);
    let mut changed_source = request.clone();
    changed_source.source = EntityRef::Id(edb_core::DB_SOURCE_SYSTEM);
    assert_eq!(
        conn.transact(changed_source, timeout).unwrap_err().code,
        "postgres/idempotency-key-reused"
    );
    let mut fresh = request.clone();
    fresh.request_key = "fresh-unresolved".into();
    fresh.compare_basis_t = None;
    assert_eq!(
        conn.transact(fresh, timeout).unwrap_err().code,
        "transaction/lookup-not-found"
    );

    let (identity, certificate) = tls_identity();
    let token = RemoteAuthToken::from_bytes([37; 32]);
    let server = RemoteTransactionEndpoint::bind("127.0.0.1:0".parse().unwrap(), identity)
        .unwrap()
        .start(
            writer.client(),
            RemoteTransportConfig {
                max_hint_bytes: 0,
                ..Default::default()
            },
            token.clone(),
        )
        .unwrap();
    let endpoint = server
        .publish(&config, server.local_addr(), "localhost")
        .unwrap();
    let remote_config = RemoteClientConfig::new(token)
        .unwrap()
        .with_root_certificate_pem(&certificate)
        .unwrap();
    // A valid current-format hint exceeds only the server's advisory allowance.
    let remote = conn
        .transact_remote_with_hints(
            &endpoint,
            &remote_config,
            request,
            source_hints.clone(),
            timeout,
        )
        .unwrap();
    assert!(remote.replayed);
    assert_eq!(remote.basis_t, accepted.basis_t);
    assert_eq!(remote.tx_hash, accepted.tx_hash);
    assert_eq!(remote.report.unwrap().tx_data, accepted.tx_data);
    let remote_fresh = conn
        .transact_remote_with_hints(
            &endpoint,
            &remote_config,
            TransactionRequest::new("remote-fresh", vec![], EntityRef::Id(author)),
            source_hints,
            timeout,
        )
        .unwrap();
    assert!(!remote_fresh.replayed);
    let remote_fresh = remote_fresh.report.unwrap();
    assert_eq!(remote_fresh.tx_data.len(), 1);
    assert_eq!(remote_fresh.tx_data[0].source, author);
    assert_eq!(server.stats().ignored_hints, 2);
    drop(server);
    drop(conn);
    writer.shutdown();
}

fn tls_identity() -> (native_tls::Identity, Vec<u8>) {
    use openssl::{
        asn1::Asn1Time,
        hash::MessageDigest,
        pkey::PKey,
        rsa::Rsa,
        x509::{
            X509, X509NameBuilder,
            extension::{BasicConstraints, SubjectAlternativeName},
        },
    };
    let key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
    let mut name = X509NameBuilder::new().unwrap();
    name.append_entry_by_text("CN", "localhost").unwrap();
    let name = name.build();
    let mut cert = X509::builder().unwrap();
    cert.set_version(2).unwrap();
    cert.set_subject_name(&name).unwrap();
    cert.set_issuer_name(&name).unwrap();
    cert.set_pubkey(&key).unwrap();
    cert.set_not_before(&Asn1Time::days_from_now(0).unwrap())
        .unwrap();
    cert.set_not_after(&Asn1Time::days_from_now(1).unwrap())
        .unwrap();
    cert.append_extension(BasicConstraints::new().critical().ca().build().unwrap())
        .unwrap();
    let san = SubjectAlternativeName::new()
        .dns("localhost")
        .build(&cert.x509v3_context(None, None))
        .unwrap();
    cert.append_extension(san).unwrap();
    cert.sign(&key, MessageDigest::sha256()).unwrap();
    let pem = cert.build().to_pem().unwrap();
    let identity =
        native_tls::Identity::from_pkcs8(&pem, &key.private_key_to_pem_pkcs8().unwrap()).unwrap();
    (identity, pem)
}
