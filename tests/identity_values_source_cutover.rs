use edb_core::{
    Attribute, Cardinality, Database, DatabaseValue, EntityRef, IndexPrefix, Keyword, Schema,
    TxForm, TxOp, Unique, Value, ValueType, submission_request_digest,
};

fn add(entity: EntityRef, attribute: u32, value: Value) -> TxOp {
    TxOp::Add {
        entity,
        attribute,
        value: value.into(),
    }
}

fn contributions(db: &DatabaseValue, attribute: u32) -> Vec<edb_core::Datom> {
    db.datoms_with_prefix(&IndexPrefix::Eavt {
        entity: 500,
        attribute: Some(attribute),
        value: None,
    })
    .unwrap()
}

#[test]
fn uri_aliases_share_identity_but_preserve_source_and_exact_input() {
    let upper = Value::Uri("HTTP://EXAMPLE.COM:00080/%4a".into());
    let lower = Value::Uri("http://example.com:80/%4A".into());
    assert_eq!(upper, lower);
    let mut schema = Schema::new();
    schema
        .install(
            Attribute::new(
                100,
                Keyword::new("identity-test", "uri"),
                ValueType::Uri,
                Cardinality::One,
            )
            .unique(Unique::Identity),
        )
        .unwrap();
    let first = Database::new(schema)
        .unwrap()
        .with(
            &[add(EntityRef::Id(500), 100, upper.clone())],
            1,
            EntityRef::Id(99),
        )
        .unwrap();
    let captured = first.db_after.database_value();
    let redundant = captured
        .with(
            &[add(EntityRef::Id(500), 100, lower.clone())],
            2,
            EntityRef::Id(99),
        )
        .unwrap();
    assert_eq!(redundant.tx_data.len(), 1); // Automatic timestamp only.
    let upsert = add(EntityRef::Temp("alias".into()), 100, lower.clone());
    let eager = first
        .db_after
        .with(std::slice::from_ref(&upsert), 2, EntityRef::Id(98))
        .unwrap();
    let overlay = captured.with(&[upsert], 2, EntityRef::Id(98)).unwrap();
    assert_eq!(eager.tempids["alias"], 500);
    assert_eq!(overlay.tempids["alias"], 500);
    for db in [eager.db_after.database_value(), overlay.db_after] {
        assert_eq!(db.lookup(100, &lower).unwrap(), Some(500));
        assert_eq!(db.values(500, 100).unwrap().len(), 1);
        let facts = contributions(&db, 100);
        assert_eq!(facts.len(), 2);
        assert!(facts.iter().any(|d| d.source == 99
            && matches!(&d.value, Value::Uri(s) if s == "HTTP://EXAMPLE.COM:00080/%4a")));
        assert!(facts.iter().any(|d| d.source == 98
            && matches!(&d.value, Value::Uri(s) if s == "http://example.com:80/%4A")));
        let withdrawn = db
            .with(
                &[TxOp::Retract {
                    entity: EntityRef::Id(500),
                    attribute: 100,
                    value: Some(lower.clone().into()),
                }],
                3,
                EntityRef::Id(97),
            )
            .unwrap();
        let retract = withdrawn.tx_data.iter().find(|d| !d.added).unwrap();
        assert_eq!(retract.source, 97);
        assert!(matches!(&retract.value, Value::Uri(s) if s == "http://example.com:80/%4A"));
        assert!(
            contributions(
                &withdrawn.db_after.clone().filter(|_, d| d.source == 99),
                100
            )
            .is_empty()
        );
        assert_eq!(
            contributions(&withdrawn.db_after.as_of(db.basis_t()), 100).len(),
            2
        );
    }
    assert!(
        matches!(&captured.values(500, 100).unwrap()[0], Value::Uri(s) if s == "HTTP://EXAMPLE.COM:00080/%4a")
    );
    let forms = |value| vec![TxForm::Op(add(EntityRef::Id(500), 100, value))];
    let digest =
        |forms: &[TxForm], source| submission_request_digest(forms, &source, None, None).unwrap();
    assert_ne!(
        digest(&forms(upper), EntityRef::Id(99)),
        digest(&forms(lower.clone()), EntityRef::Id(99))
    );
    assert_ne!(
        digest(&forms(lower.clone()), EntityRef::Id(99)),
        digest(&forms(lower), EntityRef::Id(98))
    );
}

#[test]
fn nan_payload_aliases_share_values_and_replacement_withdraws_every_source() {
    for (value_type, nan, alias, finite) in [
        (
            ValueType::Float,
            Value::Float(f32::NAN),
            Value::Float(f32::from_bits(0xffc0_0042)),
            Value::Float(3.5),
        ),
        (
            ValueType::Double,
            Value::Double(f64::NAN),
            Value::Double(f64::from_bits(0xfff8_0000_0000_0042)),
            Value::Double(3.5),
        ),
    ] {
        let mut schema = Schema::new();
        schema
            .install(Attribute::new(
                100,
                Keyword::new("identity-test", "number"),
                value_type,
                Cardinality::One,
            ))
            .unwrap();
        let first = Database::new(schema)
            .unwrap()
            .with(
                &[add(EntityRef::Id(500), 100, nan.clone())],
                1,
                EntityRef::Id(99),
            )
            .unwrap();
        let captured = first.db_after.database_value();
        assert_eq!(
            captured
                .with(
                    &[add(EntityRef::Id(500), 100, alias.clone())],
                    2,
                    EntityRef::Id(99)
                )
                .unwrap()
                .tx_data
                .len(),
            1
        );
        let eager = first
            .db_after
            .with(
                &[add(EntityRef::Id(500), 100, alias.clone())],
                2,
                EntityRef::Id(98),
            )
            .unwrap();
        let overlay = captured
            .with(
                &[add(EntityRef::Id(500), 100, alias.clone())],
                2,
                EntityRef::Id(98),
            )
            .unwrap();
        for db in [eager.db_after.database_value(), overlay.db_after] {
            assert_eq!(contributions(&db, 100).len(), 2);
            assert_eq!(db.values(500, 100).unwrap().len(), 1);
            let replaced = db
                .with(
                    &[add(EntityRef::Id(500), 100, finite.clone())],
                    3,
                    EntityRef::Id(97),
                )
                .unwrap();
            let changes: Vec<_> = replaced
                .tx_data
                .iter()
                .filter(|d| d.attribute == 100)
                .collect();
            assert_eq!(changes.len(), 2);
            assert!(changes.iter().all(|d| d.source == 97));
            assert!(changes.iter().any(|d| !d.added && d.value.is_nan()));
            assert!(changes.iter().any(|d| d.added && d.value == finite));
            assert!(
                contributions(
                    &replaced.db_after.clone().filter(|_, d| d.source == 99),
                    100
                )
                .is_empty()
            );
            assert!(
                replaced
                    .db_after
                    .clone()
                    .as_of(db.basis_t())
                    .values(500, 100)
                    .unwrap()[0]
                    .is_nan()
            );
            let restored = replaced
                .db_after
                .with(
                    &[add(EntityRef::Id(500), 100, nan.clone())],
                    4,
                    EntityRef::Id(96),
                )
                .unwrap();
            assert!(restored.db_after.values(500, 100).unwrap()[0].is_nan());
            assert!(
                restored
                    .tx_data
                    .iter()
                    .filter(|d| d.attribute == 100)
                    .all(|d| d.source == 96)
            );
            let removed = db
                .with(
                    &[TxOp::Retract {
                        entity: EntityRef::Id(500),
                        attribute: 100,
                        value: Some(alias.clone().into()),
                    }],
                    3,
                    EntityRef::Id(97),
                )
                .unwrap();
            assert!(removed.db_after.values(500, 100).unwrap().is_empty());
        }
        assert!(captured.values(500, 100).unwrap()[0].is_nan());
    }
}

#[test]
fn nan_cannot_become_a_unique_key_even_when_replacing_an_existing_value() {
    for (value_type, nan, finite) in [
        (ValueType::Float, Value::Float(f32::NAN), Value::Float(3.5)),
        (
            ValueType::Double,
            Value::Double(f64::NAN),
            Value::Double(3.5),
        ),
    ] {
        for unique in [Unique::Identity, Unique::Value] {
            let mut schema = Schema::new();
            schema
                .install(
                    Attribute::new(
                        100,
                        Keyword::new("identity-test", "number"),
                        value_type,
                        Cardinality::One,
                    )
                    .unique(unique),
                )
                .unwrap();
            let base = Database::new(schema).unwrap();
            let held = base
                .with(
                    &[add(EntityRef::Id(500), 100, finite.clone())],
                    1,
                    EntityRef::Id(99),
                )
                .unwrap()
                .db_after
                .database_value();
            for entity in [EntityRef::Id(500), EntityRef::Temp("nan".into())] {
                assert_eq!(
                    held.with(&[add(entity, 100, nan.clone())], 2, EntityRef::Id(98))
                        .unwrap_err()
                        .code,
                    "transaction/nan-cannot-identify"
                );
            }
            assert_eq!(held.values(500, 100).unwrap(), vec![finite.clone()]);
        }
    }
}
