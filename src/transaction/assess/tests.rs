use super::*;
use crate::{
    Attribute, AttributeRef, Database, EntityMap, Keyword, MapValue, Schema, TxForm, View,
};

fn add(entity: EntityRef, attribute: u32, value: Value) -> TxOp {
    TxOp::Add {
        entity,
        attribute,
        value: value.into(),
    }
}

fn doc(value: &str) -> TxOp {
    add(
        EntityRef::Id(400),
        crate::DB_DOC as u32,
        Value::String(value.into()),
    )
}

fn source(id: u64) -> EntityRef {
    EntityRef::Id(id)
}

#[test]
fn empty_transaction_has_one_attributed_automatic_instant() {
    let report = Database::bootstrap()
        .unwrap()
        .with(&[], 10, source(99))
        .unwrap();
    assert_eq!(report.tx_data.len(), 1);
    let datom = &report.tx_data[0];
    assert_eq!(datom.entity, t_to_tx(1).unwrap());
    assert_eq!(datom.attribute, DB_TX_INSTANT as u32);
    assert_eq!(datom.value, Value::Instant(10));
    assert_eq!(datom.source, 99);
    assert!(datom.added);
}

#[test]
fn source_uses_ordinary_tempid_ident_and_lookup_resolution() {
    let name = Keyword::new("contributor", "julia");
    let report = Database::bootstrap()
        .unwrap()
        .with(
            &[
                add(
                    EntityRef::Temp("julia".into()),
                    DB_IDENT as u32,
                    Value::Keyword(name.clone()),
                ),
                doc("attributed"),
            ],
            10,
            EntityRef::Temp("julia".into()),
        )
        .unwrap();
    let julia = report.tempids["julia"];
    assert!(report.tx_data.iter().all(|datom| datom.source == julia));
    let ident_report = report
        .db_after
        .with(&[], 11, EntityRef::Ident(name.clone()))
        .unwrap();
    assert_eq!(ident_report.tx_data[0].source, julia);
    let lookup_report = ident_report
        .db_after
        .with(
            &[],
            12,
            EntityRef::Lookup {
                attribute: DB_IDENT as u32,
                value: Value::Keyword(name),
            },
        )
        .unwrap();
    assert_eq!(lookup_report.tx_data[0].source, julia);
    let missing = Database::bootstrap()
        .unwrap()
        .with(&[], 10, EntityRef::Temp("unestablished".into()))
        .unwrap_err();
    assert_eq!(missing.code, "transaction/tempid-not-an-entity");
    let unissued = Database::bootstrap()
        .unwrap()
        .with(&[], 10, source(crate::INITIAL_EIDX_FRONTIER))
        .unwrap_err();
    assert_eq!(unissued.code, "transaction/invalid-entity-id");
}

#[test]
fn same_eavs_is_redundant_but_another_source_is_material() {
    let first = Database::bootstrap()
        .unwrap()
        .with(&[doc("shared")], 10, source(99))
        .unwrap();
    let repeated = first
        .db_after
        .with(&[doc("shared"), doc("shared")], 11, source(99))
        .unwrap();
    assert_eq!(repeated.tx_data.len(), 1);
    let second = repeated
        .db_after
        .with(&[doc("shared")], 12, source(98))
        .unwrap();
    let contribution = second
        .tx_data
        .iter()
        .find(|datom| datom.entity == 400)
        .unwrap();
    assert_eq!(contribution.source, 98);
    let current = second.db_after.datoms(View::Current, IndexOrder::Eavt);
    assert_eq!(
        current.iter().filter(|datom| datom.entity == 400).count(),
        2
    );
    assert_eq!(second.db_after.values(400, crate::DB_DOC as u32).len(), 1);
    let value = second.db_after.database_value();
    assert_eq!(value.values(400, crate::DB_DOC as u32).unwrap().len(), 1);
}

#[test]
fn cas_and_replacement_count_shared_values_and_withdraw_all_sources() {
    let first = Database::bootstrap()
        .unwrap()
        .with(&[doc("shared")], 10, source(99))
        .unwrap();
    let second = first
        .db_after
        .with(&[doc("shared")], 11, source(98))
        .unwrap();
    let replaced = second
        .db_after
        .with(
            &[TxOp::Cas {
                entity: EntityRef::Id(400),
                attribute: crate::DB_DOC as u32,
                old: Some(Value::String("shared".into()).into()),
                new: Value::String("replacement".into()).into(),
            }],
            12,
            source(97),
        )
        .unwrap();
    assert!(replaced.tx_data.iter().all(|datom| datom.source == 97));
    assert_eq!(
        replaced.tx_data.iter().filter(|datom| !datom.added).count(),
        1
    );
    assert_eq!(
        replaced.db_after.values(400, crate::DB_DOC as u32),
        vec![&Value::String("replacement".into())]
    );
    assert!(
        replaced
            .db_after
            .database_value()
            .values(400, crate::DB_DOC as u32)
            .unwrap()
            .iter()
            .all(|v| *v == Value::String("replacement".into()))
    );
    let history = replaced.db_after.datoms(View::History, IndexOrder::Eavt);
    assert!(
        history
            .iter()
            .any(|d| d.entity == 400 && d.source == 99 && d.added)
    );
    assert!(
        history
            .iter()
            .any(|d| d.entity == 400 && d.source == 98 && d.added)
    );
    assert!(
        history
            .iter()
            .any(|d| d.entity == 400 && d.source == 97 && !d.added)
    );
}

#[test]
fn unique_value_holder_survives_contributions_and_global_retraction_releases_it() {
    let name = Value::Keyword(Keyword::new("record", "shared"));
    let op = add(EntityRef::Id(400), DB_IDENT as u32, name.clone());
    let first = Database::bootstrap()
        .unwrap()
        .with(std::slice::from_ref(&op), 10, source(99))
        .unwrap();
    let second = first.db_after.with(&[op], 11, source(98)).unwrap();
    let moved = second
        .db_after
        .with(
            &[
                TxOp::Retract {
                    entity: EntityRef::Id(400),
                    attribute: DB_IDENT as u32,
                    value: Some(name.clone().into()),
                },
                add(EntityRef::Id(401), DB_IDENT as u32, name),
            ],
            12,
            source(97),
        )
        .unwrap();
    assert!(moved.db_after.values(400, DB_IDENT as u32).is_empty());
    assert_eq!(
        moved.db_after.entid(&Keyword::new("record", "shared")),
        Some(401)
    );
}

#[test]
fn generated_composite_changes_share_source_and_schema_counts_distinct_values() {
    let left = Attribute::new(
        500,
        Keyword::new("test", "left"),
        ValueType::String,
        Cardinality::One,
    );
    let right = Attribute::new(
        501,
        Keyword::new("test", "right"),
        ValueType::String,
        Cardinality::One,
    );
    let mut tuple = Attribute::new(
        502,
        Keyword::new("test", "pair"),
        ValueType::Tuple,
        Cardinality::One,
    );
    tuple.tuple = Some(TupleSpec::Composite(vec![500, 501]));
    let mut schema = Schema::new();
    for attribute in [left, right, tuple] {
        schema.install(attribute).unwrap();
    }
    let database = Database::new(schema).unwrap();
    let first = database
        .with(
            &[
                add(EntityRef::Id(400), 500, Value::String("left".into())),
                add(EntityRef::Id(400), 501, Value::String("right".into())),
            ],
            10,
            source(99),
        )
        .unwrap();
    assert!(first.tx_data.iter().all(|d| d.source == 99));
    assert!(first.tx_data.iter().any(|d| d.attribute == 502 && d.added));
    let changed = first
        .db_after
        .with(
            &[add(
                EntityRef::Id(400),
                500,
                Value::String("changed".into()),
            )],
            11,
            source(98),
        )
        .unwrap();
    assert!(changed.tx_data.iter().all(|d| d.source == 98));
    assert!(
        changed
            .tx_data
            .iter()
            .any(|d| d.attribute == 502 && !d.added)
    );

    let mut many = Attribute::new(
        503,
        Keyword::new("test", "many"),
        ValueType::String,
        Cardinality::Many,
    );
    let installed = changed
        .db_after
        .with(&[TxOp::InstallAttribute(many.clone())], 12, source(97))
        .unwrap();
    let op = add(EntityRef::Id(400), 503, Value::String("one".into()));
    let a = installed
        .db_after
        .with(std::slice::from_ref(&op), 13, source(99))
        .unwrap();
    let b = a.db_after.with(&[op], 14, source(98)).unwrap();
    many.cardinality = Cardinality::One;
    let altered = b
        .db_after
        .with(&[TxOp::AlterAttribute(many)], 15, source(97))
        .unwrap();
    assert_eq!(altered.db_after.values(400, 503).len(), 1);
    assert!(altered.tx_data.iter().all(|d| d.source == 97));
}

#[test]
fn anonymous_map_cannot_accidentally_establish_request_source_tempid() {
    let database = Database::bootstrap().unwrap().database_value();
    let forms = [TxForm::EntityMap(EntityMap {
        id: None,
        attributes: vec![(
            AttributeRef::Id(crate::DB_DOC as u32),
            MapValue::Value(Value::String("anonymous".into()).into()),
        )],
    })];
    let error = database
        .with_forms(
            &forms,
            10,
            EntityRef::Temp("__map/00000000000000000000".into()),
        )
        .err()
        .unwrap();
    assert_eq!(error.code, "transaction/tempid-not-an-entity");
}

#[test]
fn callback_generated_entities_and_explicit_timestamp_share_resolved_source() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    let mut functions = TxFunctions::new();
    functions.register("generated", move |_, _| {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(vec![
            TxForm::EntityMap(EntityMap {
                id: Some(EntityRef::Temp("julia".into())),
                attributes: vec![(
                    AttributeRef::Id(DB_IDENT as u32),
                    MapValue::Value(Value::Keyword(Keyword::new("contributor", "julia")).into()),
                )],
            }),
            TxForm::Op(doc("generated")),
            TxForm::Op(add(EntityRef::Tx, DB_TX_INSTANT as u32, Value::Instant(10))),
        ])
    });
    let forms = [TxForm::Call(crate::TxCall {
        function: "generated".into(),
        arguments: vec![],
    })];
    let report = Database::bootstrap()
        .unwrap()
        .with_forms(&forms, &functions, 10, EntityRef::Temp("julia".into()))
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        report
            .tx_data
            .iter()
            .all(|d| d.source == report.tempids["julia"])
    );
    assert_eq!(
        report
            .tx_data
            .iter()
            .filter(|d| d.attribute == DB_TX_INSTANT as u32)
            .count(),
        1
    );
}

#[test]
fn ordinary_explicit_instant_retains_clock_validation() {
    let database = Database::bootstrap().unwrap();
    let explicit = add(EntityRef::Tx, DB_TX_INSTANT as u32, Value::Instant(10));
    let report = database
        .with(std::slice::from_ref(&explicit), 10, source(99))
        .unwrap();
    assert_eq!(report.tx_data.len(), 1);
    assert_eq!(report.tx_data[0].source, 99);
    assert_eq!(
        database.with(&[explicit], 11, source(99)).unwrap_err().code,
        "transaction/tx-instant-mismatch"
    );
    let retract = TxOp::Retract {
        entity: EntityRef::Tx,
        attribute: DB_TX_INSTANT as u32,
        value: None,
    };
    assert_eq!(
        report
            .db_after
            .with(&[retract], 11, source(99))
            .unwrap_err()
            .code,
        "transaction/reset-tx-instant"
    );
}

#[test]
fn deferred_edn_source_lookup_uses_authoritative_attribute_and_value_types() {
    let mut name = Attribute::new(
        500,
        Keyword::new("source", "name"),
        ValueType::String,
        Cardinality::One,
    );
    name.unique = Some(Unique::Identity);
    let mut friend = Attribute::new(
        501,
        Keyword::new("source", "friend"),
        ValueType::Ref,
        Cardinality::One,
    );
    friend.unique = Some(Unique::Identity);
    let mut schema = Schema::new();
    schema.install(name).unwrap();
    schema.install(friend).unwrap();
    let database = Database::new(schema).unwrap();
    let report = database
        .with(
            &[
                add(
                    EntityRef::Temp("julia".into()),
                    500,
                    Value::String("julia".into()),
                ),
                TxOp::Add {
                    entity: EntityRef::Temp("agent".into()),
                    attribute: 501,
                    value: TxValue::Entity(EntityRef::Temp("julia".into())),
                },
            ],
            10,
            source(99),
        )
        .unwrap();
    let julia = report.tempids["julia"];
    let by_name = report
        .db_after
        .with(&[], 11, EntityRef::Edn(r#"[:source/name "julia"]"#.into()))
        .unwrap();
    assert_eq!(by_name.tx_data[0].source, julia);
    let agent = by_name
        .db_after
        .with(
            &[],
            12,
            EntityRef::Edn(r#"[:source/friend [:source/name "julia"]]"#.into()),
        )
        .unwrap();
    assert_eq!(agent.tx_data[0].source, report.tempids["agent"]);
}

#[test]
fn deferred_edn_tempid_source_retains_ordinary_entity_position_rule() {
    let database = Database::bootstrap().unwrap();
    let report = database
        .with(
            &[add(
                EntityRef::Edn(r#""julia""#.into()),
                DB_IDENT as u32,
                Value::Keyword(Keyword::new("source", "julia")),
            )],
            10,
            EntityRef::Edn(r#""julia""#.into()),
        )
        .unwrap();
    assert!(
        report
            .tx_data
            .iter()
            .all(|datom| datom.source == report.tempids["julia"])
    );
    let forms = [TxForm::EntityMap(EntityMap {
        id: None,
        attributes: vec![(
            AttributeRef::Id(crate::DB_DOC as u32),
            MapValue::Value(Value::String("anonymous".into()).into()),
        )],
    })];
    let error = database
        .with_forms(
            &forms,
            &TxFunctions::new(),
            10,
            EntityRef::Edn(r#""__map/00000000000000000000""#.into()),
        )
        .unwrap_err();
    assert_eq!(error.code, "transaction/tempid-not-an-entity");
}
