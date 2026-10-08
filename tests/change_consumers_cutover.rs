use edb_core::storage::{BlockDatabase, PgBlockStore};
use edb_core::{
    Attribute, BlockTransactor, BlockWriterOptions, Cardinality, ChangeConsumer,
    ChangeConsumerConfig, DB_EXCISE, DB_EXCISE_BEFORE_T, DB_TX_INSTANT, EntityRef, Keyword,
    PostgresConnectionConfig, PostgresOperator, Schema, ServiceTransactionReport,
    TransactionRequest, TxOp, Value, ValueType,
};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const NOTE: u32 = 1000;

fn add(entity: EntityRef, text: &str) -> TxOp {
    TxOp::Add {
        entity,
        attribute: NOTE,
        value: Value::String(text.into()).into(),
    }
}

fn check_event(event: &edb_core::ChangeEvent, report: &ServiceTransactionReport, source: u64) {
    assert_eq!(event.transaction.t, report.basis_t);
    assert_eq!(event.transaction.data, report.tx_data);
    assert_eq!(event.checkpoint.last_t(), report.basis_t);
    assert_eq!(event.checkpoint.commit_hash(), report.tx_hash);
    assert!(event.transaction.data.iter().all(|d| d.source == source));
    let timestamp = event
        .transaction
        .data
        .iter()
        .filter(|d| d.attribute == DB_TX_INSTANT as u32)
        .collect::<Vec<_>>();
    assert_eq!(timestamp.len(), 1);
    assert!(timestamp[0].added);
    assert_eq!(timestamp[0].entity, timestamp[0].tx);
    assert!(event.accounted_bytes > 0);
}

#[test]
#[ignore = "requires EDB_TEST_POSTGRES_URL for a disposable PostgreSQL database"]
fn bounded_consumer_preserves_source_log_and_contiguous_durable_progress() {
    let config = PostgresConnectionConfig::parse(
        std::env::var("EDB_TEST_POSTGRES_URL")
            .expect("EDB_TEST_POSTGRES_URL must name a disposable PostgreSQL database"),
    )
    .unwrap();
    let database_name = format!(
        "change-consumers-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    PgBlockStore::install(&config).unwrap();
    let database = BlockDatabase::create(&config, &database_name, Schema::new()).unwrap();
    let mut writer =
        BlockTransactor::claim(&config, database, BlockWriterOptions::default()).unwrap();
    let mut attribute = Attribute::new(
        NOTE,
        Keyword::new("item", "note"),
        ValueType::String,
        Cardinality::One,
    );
    attribute.no_history = true;
    let schema = writer
        .transact(&TransactionRequest::new(
            "schema",
            vec![TxOp::InstallAttribute(attribute)],
            EntityRef::Id(99),
        ))
        .unwrap();

    let open = |name: &str, max_event_bytes| {
        ChangeConsumer::connect_configured(
            config.clone(),
            &database_name,
            name,
            ChangeConsumerConfig {
                max_event_bytes,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let capacity = ChangeConsumerConfig::default().max_event_bytes;
    let mut undersized = open("admission", 1);
    let origin = undersized.checkpoint().clone();
    assert_eq!(
        undersized.next(Duration::ZERO).unwrap_err().code,
        "consumer/event-too-large"
    );
    assert_eq!(undersized.checkpoint(), &origin);
    assert!(!undersized.stats().unacknowledged);
    drop(undersized);
    let mut consumer = open("admission", capacity);
    assert_eq!(consumer.checkpoint(), &origin);
    let event = consumer.next(Duration::ZERO).unwrap().unwrap();
    check_event(&event, &schema, 99);
    assert_eq!(consumer.checkpoint(), &origin);
    assert_eq!(
        consumer.acknowledge(&origin).unwrap_err().code,
        "consumer/not-delivered"
    );
    assert_eq!(
        consumer.next(Duration::ZERO).unwrap_err().code,
        "consumer/unacknowledged"
    );
    // Drop before acknowledgment: durable progress still starts at genesis.
    drop(consumer);
    let mut consumer = open("admission", capacity);
    let replay = consumer.next(Duration::ZERO).unwrap().unwrap();
    assert_eq!(replay, event);
    consumer.acknowledge(&replay.checkpoint).unwrap();
    assert_eq!(consumer.checkpoint(), &replay.checkpoint);
    assert_eq!(consumer.stats().acknowledged, 1);
    assert!(!consumer.stats().unacknowledged);

    let request = TransactionRequest::new(
        "original",
        vec![add(EntityRef::Temp("item".into()), "original")],
        EntityRef::Id(98),
    );
    let original = writer.transact(&request).unwrap();
    let item = original.tempids["item"];
    let mut competing = open("admission", capacity);
    let first = consumer.next(Duration::ZERO).unwrap().unwrap();
    let duplicate = competing.next(Duration::ZERO).unwrap().unwrap();
    check_event(&first, &original, 98);
    assert_eq!(first, duplicate);
    consumer.acknowledge(&first.checkpoint).unwrap();
    assert_eq!(
        competing
            .acknowledge(&duplicate.checkpoint)
            .unwrap_err()
            .code,
        "consumer/checkpoint-conflict"
    );
    assert_eq!(competing.checkpoint(), &replay.checkpoint);
    drop(competing);
    drop(consumer);
    let mut consumer = open("admission", capacity);
    assert_eq!(consumer.checkpoint(), &first.checkpoint);

    let replacement = writer
        .transact(&TransactionRequest::new(
            "replacement",
            vec![add(EntityRef::Id(item), "replacement")],
            EntityRef::Id(97),
        ))
        .unwrap();
    let replaced = consumer.next(Duration::ZERO).unwrap().unwrap();
    check_event(&replaced, &replacement, 97);
    assert!(replaced.transaction.data.iter().any(|d| {
        d.attribute == NOTE && !d.added && d.value == Value::String("original".into())
    }));
    assert_eq!(event.transaction.data, schema.tx_data);
    assert_eq!(first.transaction.data, original.tx_data);
    assert_eq!(
        replacement.db_after.values(item, NOTE).unwrap(),
        [Value::String("replacement".into())]
    );
    consumer.acknowledge(&replaced.checkpoint).unwrap();
    let retry = writer.transact(&request).unwrap();
    assert!(retry.replayed);
    assert_eq!(retry.tx_hash, original.tx_hash);
    assert_eq!(retry.tx_data, original.tx_data);
    assert!(consumer.next(Duration::ZERO).unwrap().is_none());
    consumer.reconnect().unwrap();
    assert_eq!(consumer.checkpoint(), &replaced.checkpoint);
    assert_eq!(consumer.stats().reconnects, 1);
    assert!(consumer.next(Duration::from_millis(1)).unwrap().is_none());
    assert_eq!(
        consumer
            .next_with_cancel(Duration::from_secs(5), &AtomicBool::new(true))
            .unwrap_err()
            .code,
        "consumer/cancelled"
    );
    assert_eq!(consumer.checkpoint(), &replaced.checkpoint);
    let mut idle = open("admission", capacity);
    let excision = writer
        .transact(&TransactionRequest::new(
            "excise",
            vec![
                TxOp::Add {
                    entity: EntityRef::Temp("excision".into()),
                    attribute: DB_EXCISE as u32,
                    value: Value::Ref(item).into(),
                },
                TxOp::Add {
                    entity: EntityRef::Temp("excision".into()),
                    attribute: DB_EXCISE_BEFORE_T as u32,
                    value: Value::Long(100_000).into(),
                },
            ],
            EntityRef::Id(96),
        ))
        .unwrap();
    let pending = consumer.next(Duration::ZERO).unwrap().unwrap();
    check_event(&pending, &excision, 96);
    writer.release().unwrap();
    let database_id = edb_core::DatabaseCatalog::connect_configured(&config)
        .unwrap()
        .resolve(&database_name)
        .unwrap()
        .database_id;
    PostgresOperator::connect_configured(&config)
        .unwrap()
        .process_excision_requests(&database_id)
        .unwrap();
    assert_eq!(
        consumer.acknowledge(&pending.checkpoint).unwrap_err().code,
        "consumer/generation-changed"
    );
    assert_eq!(
        idle.next(Duration::ZERO).unwrap_err().code,
        "consumer/generation-changed"
    );
    assert_eq!(consumer.checkpoint(), &replaced.checkpoint);
    assert_eq!(first.transaction.data, original.tx_data);
    assert_eq!(replaced.transaction.data, replacement.tx_data);
    drop(consumer);
    drop(idle);
    let error = ChangeConsumer::connect_configured(
        config.clone(),
        &database_name,
        "admission",
        ChangeConsumerConfig::default(),
    )
    .err()
    .expect("old consumer must not reopen across excision");
    assert_eq!(error.code, "consumer/generation-changed");
    let mut current = open("after-excision", capacity);
    assert_eq!(
        current.checkpoint().generation(),
        replaced.checkpoint.generation() + 1
    );
    assert_eq!(current.checkpoint().last_t(), 0);
    for t in 1..=excision.basis_t {
        let retained = current.next(Duration::ZERO).unwrap().unwrap();
        assert_eq!(retained.transaction.t, t);
        assert!(!retained.transaction.data.iter().any(|d| d.entity == item));
        current.acknowledge(&retained.checkpoint).unwrap();
    }
    assert!(current.next(Duration::ZERO).unwrap().is_none());
}

#[test]
#[ignore = "requires EDB_TEST_POSTGRES_URL for a disposable PostgreSQL database"]
fn consumer_rejects_formerly_inline_bodies_before_postgres_transfer() {
    let config = PostgresConnectionConfig::parse(
        std::env::var("EDB_TEST_POSTGRES_URL")
            .expect("EDB_TEST_POSTGRES_URL must name a disposable PostgreSQL database"),
    )
    .unwrap();
    let mut state = 0x1234_5678_9abc_def0_u64;
    let random = (0..48 * 1024)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            char::from(b'!' + (state % 90) as u8)
        })
        .collect::<String>();
    for (label, body) in [("compressible", "x".repeat(48 * 1024)), ("varied", random)] {
        let name = format!(
            "consumer-admission-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        PgBlockStore::install(&config).unwrap();
        let mut schema = Schema::new();
        schema
            .install(Attribute::new(
                NOTE,
                Keyword::new("item", "note"),
                ValueType::String,
                Cardinality::One,
            ))
            .unwrap();
        let database = BlockDatabase::create(&config, &name, schema).unwrap();
        let mut writer =
            BlockTransactor::claim(&config, database, BlockWriterOptions::default()).unwrap();
        let mut consumer = ChangeConsumer::connect_configured(
            config.clone(),
            &name,
            "bounded",
            ChangeConsumerConfig {
                max_event_bytes: 8192,
                ..Default::default()
            },
        )
        .unwrap();
        let schema = consumer.next(Duration::ZERO).unwrap().unwrap();
        consumer.acknowledge(&schema.checkpoint).unwrap();
        let accepted = writer
            .transact(&TransactionRequest::new(
                "body",
                vec![add(EntityRef::Temp("item".into()), &body)],
                EntityRef::Id(98),
            ))
            .unwrap();
        let work = edb_core::OperationContext::new(edb_core::OperationKind::PeerObservation);
        {
            let _scope = work.enter();
            assert_eq!(
                consumer.next(Duration::ZERO).unwrap_err().code,
                "consumer/event-too-large"
            );
        }
        let bytes = work.snapshot().known_payload_read_bytes;
        println!(
            "admission {label}: rejected48KiB body; authenticated metadata physical_bytes={bytes}"
        );
        assert!(bytes < 4096, "oversized body transferred: {bytes} bytes");
        assert_eq!(consumer.checkpoint(), &schema.checkpoint);
        assert!(!consumer.stats().unacknowledged);
        drop(consumer);
        let mut reopened = ChangeConsumer::connect_configured(
            config.clone(),
            &name,
            "bounded",
            ChangeConsumerConfig::default(),
        )
        .unwrap();
        let event = reopened.next(Duration::ZERO).unwrap().unwrap();
        check_event(&event, &accepted, 98);
        reopened.acknowledge(&event.checkpoint).unwrap();
        drop(reopened);
        writer.release().unwrap();
    }
}
