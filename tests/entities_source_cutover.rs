use edb_core::{
    Attribute, Cardinality, Database, DatabaseValue, EntityRef, EntityValue, Keyword, Schema, TxOp,
    Value, ValueType,
};

fn decimal(text: &str) -> Value {
    Value::BigDec(text.parse().unwrap())
}

fn add(text: &str) -> TxOp {
    TxOp::Add {
        entity: EntityRef::Id(500),
        attribute: 100,
        value: decimal(text).into(),
    }
}

fn entity_scales(database: &DatabaseValue) -> Vec<i64> {
    let entity = database.entity(500).unwrap().unwrap().touch().unwrap();
    let Some(EntityValue::Collection(values)) = entity.get(100).unwrap() else {
        panic!("cardinality-many decimals retain collection shape")
    };
    values
        .into_iter()
        .map(|value| match value {
            EntityValue::Scalar(Value::BigDec(value)) => value.fractional_digit_count(),
            _ => panic!("decimal expected"),
        })
        .collect()
}

#[test]
fn entity_projection_preserves_stored_decimal_values_across_sources_and_views() {
    let mut schema = Schema::new();
    schema
        .install(Attribute::new(
            100,
            Keyword::new("item", "amount"),
            ValueType::BigDec,
            Cardinality::Many,
        ))
        .unwrap();
    let mut eager = Database::new(schema).unwrap();
    for (instant, source, text) in [(1, 99, "1.0"), (2, 98, "1.00"), (3, 97, "1.0")] {
        eager = eager
            .with(&[add(text)], instant, EntityRef::Id(source))
            .unwrap()
            .db_after;
    }
    let captured = eager.database_value();
    let overlay = captured
        .with(&[add("1.00")], 4, EntityRef::Id(96))
        .unwrap()
        .db_after;
    for database in [&captured, &overlay] {
        let mut scales = entity_scales(database);
        scales.sort();
        assert_eq!(scales, vec![1, 2]);
        assert_eq!(
            entity_scales(&database.clone().filter(|_, d| d.source == 99)),
            vec![1]
        );
    }
    let withdrawn = overlay
        .with(
            &[TxOp::Retract {
                entity: EntityRef::Id(500),
                attribute: 100,
                value: Some(decimal("1.0").into()),
            }],
            5,
            EntityRef::Id(95),
        )
        .unwrap()
        .db_after;
    assert_eq!(entity_scales(&withdrawn), vec![2]);
    assert!(
        withdrawn
            .clone()
            .filter(|_, d| d.source == 99)
            .entity(500)
            .unwrap()
            .unwrap()
            .get(100)
            .unwrap()
            .is_none()
    );
    let mut before_scales = entity_scales(&withdrawn.clone().as_of(overlay.basis_t()));
    before_scales.sort();
    assert_eq!(before_scales, vec![1, 2]);
    assert_eq!(
        entity_scales(&overlay.clone().since(captured.basis_t())),
        vec![2]
    );
    assert_eq!(
        withdrawn.history().entity(500).unwrap_err().code,
        "database/history-not-point-in-time"
    );
}

fn reference(entity: u64, attribute: u32, target: u64) -> TxOp {
    TxOp::Add {
        entity: EntityRef::Id(entity),
        attribute,
        value: Value::Ref(target).into(),
    }
}

fn entity_ids(database: &DatabaseValue, entity: u64, attribute: u32) -> Vec<u64> {
    let handle = database.entity(entity).unwrap().unwrap().touch().unwrap();
    match handle.get(attribute).unwrap() {
        None => Vec::new(),
        Some(EntityValue::Collection(values)) => values
            .into_iter()
            .map(|value| match value {
                EntityValue::Entity(entity) => entity.id(),
                _ => panic!("entity expected"),
            })
            .collect(),
        _ => panic!("cardinality-many refs retain collection shape"),
    }
}

#[test]
fn entity_references_and_components_resolve_global_withdrawals_before_source_filters() {
    let mut schema = Schema::new();
    for (id, name, component) in [(100, "children", true), (101, "links", false)] {
        let mut attribute = Attribute::new(
            id,
            Keyword::new("item", name),
            ValueType::Ref,
            Cardinality::Many,
        );
        if component {
            attribute = attribute.component();
        }
        schema.install(attribute).unwrap();
    }
    let facts = [
        reference(500, 100, 600),
        reference(500, 100, 601),
        reference(500, 101, 600),
        reference(501, 101, 600),
    ];
    let first = Database::new(schema)
        .unwrap()
        .with(&facts, 1, EntityRef::Id(99))
        .unwrap();
    let one = first.db_after.database_value();
    let eager = first
        .db_after
        .with(&facts, 2, EntityRef::Id(98))
        .unwrap()
        .db_after
        .database_value();
    let overlay = one.with(&facts, 2, EntityRef::Id(98)).unwrap().db_after;
    for two in [eager, overlay] {
        assert_eq!(entity_ids(&two, 500, 100), vec![600, 601]);
        assert_eq!(
            two.entity(600)
                .unwrap()
                .unwrap()
                .reverse(101)
                .unwrap()
                .iter()
                .map(|entity| entity.id())
                .collect::<Vec<_>>(),
            vec![500, 501]
        );
        assert_eq!(
            two.entity(600)
                .unwrap()
                .unwrap()
                .reverse(100)
                .unwrap()
                .iter()
                .map(|entity| entity.id())
                .collect::<Vec<_>>(),
            vec![500]
        );
        let retractions: Vec<_> = [100, 101]
            .into_iter()
            .map(|attribute| TxOp::Retract {
                entity: EntityRef::Id(500),
                attribute,
                value: Some(Value::Ref(600).into()),
            })
            .collect();
        let three = two
            .with(&retractions, 3, EntityRef::Id(97))
            .unwrap()
            .db_after;
        for current in [
            three.clone(),
            three.clone().filter(|_, d| d.source == 99),
            three.clone().filter(|_, d| d.source != 97),
        ] {
            assert_eq!(entity_ids(&current, 500, 100), vec![601]);
            assert_eq!(
                current
                    .entity(600)
                    .unwrap()
                    .unwrap()
                    .reverse(101)
                    .unwrap()
                    .iter()
                    .map(|entity| entity.id())
                    .collect::<Vec<_>>(),
                vec![501]
            );
            assert!(
                current
                    .entity(600)
                    .unwrap()
                    .unwrap()
                    .reverse(100)
                    .unwrap()
                    .is_empty()
            );
        }
        for previous in [
            three
                .clone()
                .as_of(two.basis_t())
                .filter(|_, d| d.source == 99),
            three.as_of(two.basis_t()).since(one.basis_t()),
        ] {
            assert_eq!(entity_ids(&previous, 500, 100), vec![600, 601]);
            assert_eq!(
                previous
                    .entity(600)
                    .unwrap()
                    .unwrap()
                    .reverse(101)
                    .unwrap()
                    .iter()
                    .map(|entity| entity.id())
                    .collect::<Vec<_>>(),
                vec![500, 501]
            );
        }
    }
}
