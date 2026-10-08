use edb_core::storage::{BlockDatabase, PgBlockStore};
use edb_core::{
    Attribute, AttributeName, BlockTransactor, BlockWriterOptions, Cardinality, EntityRef,
    IndexOrder, Keyword, Peer, PeerSnapshot, PostgresConnectionConfig, PostgresOperator, Schema,
    TransactionRequest, TxOp, Value, ValueType,
};
use std::time::{SystemTime, UNIX_EPOCH};

const SCORE: u32 = 1000;

fn assert_pending(snapshot: &PeerSnapshot) {
    let value = snapshot.database_value();
    assert!(!value.has_avet(&AttributeName::Id(SCORE)).unwrap());
    let key = value
        .datoms(IndexOrder::Eavt)
        .unwrap()
        .into_iter()
        .find(|d| d.attribute == SCORE)
        .unwrap();
    for history in [false, true] {
        for order in [IndexOrder::Avet, IndexOrder::Savet] {
            let rejected = match snapshot.range_cursor(history, order, None, None) {
                Err(error) => error,
                Ok(_) => panic!("pending {order:?} raw range was accepted, history={history}"),
            };
            assert_eq!(rejected.code, "peer/avet-not-ready");
            assert_eq!(
                snapshot.seek(history, order, &key).unwrap_err().code,
                rejected.code
            );
            assert_eq!(
                snapshot.range(history, order, None, None).unwrap_err().code,
                rejected.code
            );
            assert_eq!(
                snapshot.datoms(history, order).unwrap_err().code,
                rejected.code
            );
            let view = if history {
                value.clone().history()
            } else {
                value.clone()
            };
            assert!(
                view.datoms(order)
                    .unwrap()
                    .iter()
                    .all(|d| d.attribute != SCORE)
            );
        }
    }
}

#[test]
#[ignore = "requires EDB_TEST_POSTGRES_URL for a disposable PostgreSQL database"]
fn raw_peer_snapshot_shares_avet_savet_readiness_across_publication() {
    let config = PostgresConnectionConfig::parse(
        std::env::var("EDB_TEST_POSTGRES_URL")
            .expect("explicit disposable PostgreSQL database required"),
    )
    .unwrap();
    let name = format!(
        "peer-index-readiness-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
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
        SCORE,
        Keyword::new("readiness", "score"),
        ValueType::Long,
        Cardinality::One,
    );
    writer
        .transact(&TransactionRequest::new(
            "install",
            vec![TxOp::InstallAttribute(attribute.clone())],
            EntityRef::Id(99),
        ))
        .unwrap();
    let first = writer
        .transact(&TransactionRequest::new(
            "first",
            vec![TxOp::Add {
                entity: EntityRef::Temp("item".into()),
                attribute: SCORE,
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
                attribute: SCORE,
                value: Value::Long(7).into(),
            }],
            EntityRef::Id(98),
        ))
        .unwrap();
    let mut operator = PostgresOperator::connect_configured(&config).unwrap();
    operator.consolidate_database(&database_id).unwrap();
    attribute.indexed = true;
    writer
        .transact(&TransactionRequest::new(
            "enable",
            vec![TxOp::AlterAttribute(attribute)],
            EntityRef::Id(97),
        ))
        .unwrap();
    let peer = Peer::connect_configured(&config, &name, 64).unwrap();
    let pending = peer.snapshot();
    assert_pending(&pending);
    operator.consolidate_database(&database_id).unwrap();
    let ready = Peer::connect_configured(&config, &name, 64)
        .unwrap()
        .snapshot();
    assert_eq!(ready.basis_t(), pending.basis_t());
    assert!(
        ready
            .database_value()
            .has_avet(&AttributeName::Id(SCORE))
            .unwrap()
    );
    for history in [false, true] {
        let mut projections = Vec::new();
        for order in [IndexOrder::Avet, IndexOrder::Savet] {
            let mut data = ready.datoms(history, order).unwrap().datoms;
            data.retain(|d| d.attribute == SCORE);
            data.sort_by(|a, b| a.cmp_in(b, IndexOrder::Eavt));
            assert_eq!(data.len(), 2);
            assert!(
                data.iter()
                    .all(|d| d.entity == item && d.value == Value::Long(7))
            );
            assert_eq!(data.iter().map(|d| d.source).collect::<Vec<_>>(), [98, 99]);
            projections.push(data);
        }
        assert_eq!(projections[0], projections[1]);
    }
    assert_pending(&pending);
    writer.release().unwrap();
}
