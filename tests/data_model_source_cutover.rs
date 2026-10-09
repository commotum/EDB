use edb_core::{
    Attribute, Cardinality, DB_SOURCE_SYSTEM, Database, Datom, DurableTransaction, EntityRef,
    IndexOrder, Keyword, Schema, TxOp, TxValue, Unique, Value, ValueType, View, decode_genesis,
    decode_transaction, encode_genesis, encode_transaction, request_digest, sha256,
    submission_request_digest, t_to_tx,
};
use std::collections::BTreeMap;

fn schema_database() -> Database {
    let mut schema = Schema::new();
    schema
        .install(
            Attribute::new(
                100,
                Keyword::new("item", "name"),
                ValueType::String,
                Cardinality::One,
            )
            .unique(Unique::Identity),
        )
        .unwrap();
    Database::new(schema).unwrap()
}

fn add(value: &str) -> TxOp {
    TxOp::Add {
        entity: EntityRef::Id(42),
        attribute: 100,
        value: TxValue::Scalar(Value::String(value.into())),
    }
}

#[test]
fn sourced_contributions_share_cardinality_and_global_withdrawal() {
    let initial = schema_database();
    let first = initial
        .with(&[add("Peaches")], 1, EntityRef::Id(99))
        .unwrap();
    let second = first
        .db_after
        .with(&[add("Peaches")], 2, EntityRef::Id(98))
        .unwrap();
    assert_eq!(
        second
            .tx_data
            .iter()
            .filter(|d| d.entity == 42 && d.attribute == 100)
            .count(),
        1
    );
    assert_eq!(
        second.db_after.values(42, 100),
        vec![&Value::String("Peaches".into())]
    );
    let live: Vec<_> = second
        .db_after
        .datoms(View::Current, IndexOrder::Eavt)
        .into_iter()
        .filter(|d| d.entity == 42 && d.attribute == 100)
        .collect();
    assert_eq!(live.len(), 2);
    assert_eq!(
        live.iter()
            .map(|d| d.source)
            .collect::<std::collections::BTreeSet<_>>(),
        [98, 99].into_iter().collect()
    );
    let redundant = second
        .db_after
        .with(&[add("Peaches")], 3, EntityRef::Id(98))
        .unwrap();
    assert!(
        !redundant
            .tx_data
            .iter()
            .any(|d| d.entity == 42 && d.attribute == 100)
    );
    let retract = TxOp::Retract {
        entity: EntityRef::Id(42),
        attribute: 100,
        value: Some(TxValue::Scalar(Value::String("Peaches".into()))),
    };
    let withdrawn = redundant
        .db_after
        .with(&[retract], 4, EntityRef::Id(97))
        .unwrap();
    assert!(withdrawn.db_after.values(42, 100).is_empty());
    assert_eq!(
        withdrawn
            .tx_data
            .iter()
            .find(|d| d.entity == 42 && d.attribute == 100)
            .unwrap()
            .source,
        97
    );
    assert_eq!(
        withdrawn
            .db_after
            .datoms(View::AsOf(second.db_after.basis_t()), IndexOrder::Saevt)
            .iter()
            .filter(|d| d.entity == 42 && d.attribute == 100)
            .count(),
        2
    );
    withdrawn
        .db_after
        .rebuild_derived_caches()
        .unwrap()
        .validate_invariants()
        .unwrap();
    assert_eq!(
        withdrawn
            .db_after
            .datoms(View::History, IndexOrder::Eavt)
            .iter()
            .filter(|d| d.entity == 42 && d.attribute == 100)
            .count(),
        3
    );
}

#[test]
fn cross_source_replacement_retracts_every_contribution() {
    let first = schema_database()
        .with(&[add("Peaches")], 1, EntityRef::Id(99))
        .unwrap();
    let second = first
        .db_after
        .with(&[add("Peaches")], 2, EntityRef::Id(98))
        .unwrap();
    let replacement = second
        .db_after
        .with(&[add("Pears")], 3, EntityRef::Id(97))
        .unwrap();
    assert_eq!(
        replacement.db_after.values(42, 100),
        vec![&Value::String("Pears".into())]
    );
    assert!(replacement.tx_data.iter().all(|d| d.source == 97));
    for order in [
        IndexOrder::Eavt,
        IndexOrder::Aevt,
        IndexOrder::Avet,
        IndexOrder::Saevt,
        IndexOrder::Savet,
    ] {
        let facts = replacement.db_after.datoms(View::Current, order);
        assert_eq!(
            facts
                .iter()
                .filter(|d| d.entity == 42 && d.attribute == 100)
                .count(),
            1
        );
    }
    replacement.db_after.validate_invariants().unwrap();
}

#[test]
fn canonical_genesis_is_system_attributed_and_old_envelopes_are_rejected() {
    let db = Database::bootstrap().unwrap();
    assert_eq!(
        db.entid(&Keyword::new("db.source", "system")),
        Some(DB_SOURCE_SYSTEM)
    );
    let datoms = db.datoms(View::History, IndexOrder::Eavt);
    assert!(datoms.iter().all(|d| d.source == DB_SOURCE_SYSTEM));
    let encoded = encode_genesis(&datoms).unwrap();
    assert_eq!(decode_genesis(&encoded).unwrap(), datoms);
    let mut old = encoded;
    old[5..7].copy_from_slice(&3u16.to_be_bytes());
    let payload_end = old.len() - 32;
    let checksum = sha256(&old[..payload_end]);
    old[payload_end..].copy_from_slice(&checksum);
    assert!(decode_genesis(&old).is_err());
}

#[test]
fn durable_codec_and_request_identity_preserve_source() {
    let report = Database::bootstrap()
        .unwrap()
        .with(&[], 1, EntityRef::Id(99))
        .unwrap();
    let transaction = DurableTransaction {
        database_id: "cutover-unit".into(),
        basis_t: report.db_after.basis_t(),
        previous_hash: [0; 32],
        eidx_frontier: report.db_after.eidx_frontier(),
        tempids: BTreeMap::new(),
        tx_data: report.tx_data,
    };
    assert_eq!(
        decode_transaction(&encode_transaction(&transaction).unwrap()).unwrap(),
        transaction
    );
    let mut mixed = transaction.clone();
    mixed.tx_data.push(Datom {
        entity: 42,
        attribute: 62,
        value: Value::String("note".into()),
        tx: t_to_tx(mixed.basis_t).unwrap(),
        source: 98,
        added: true,
    });
    assert!(encode_transaction(&mixed).is_err());
    assert_ne!(
        request_digest(&[], 1, 0, &EntityRef::Id(99)).unwrap(),
        request_digest(&[], 1, 0, &EntityRef::Id(98)).unwrap()
    );
    assert_ne!(
        submission_request_digest(&[], &EntityRef::Id(99), None, None).unwrap(),
        submission_request_digest(&[], &EntityRef::Id(98), None, None).unwrap()
    );
}
