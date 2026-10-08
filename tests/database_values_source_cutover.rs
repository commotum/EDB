use edb_core::edn_pull::EdnPullTransforms;
use edb_core::edn_query::{EdnQueryArgument, parse_query_edn};
use edb_core::{
    Attribute, Cardinality, Database, DatabaseValue, EntityRef, EntityValue, IndexOrder, Keyword,
    PullTransform, QueryControl, QueryDataSource, QuerySourceValue, QueryValue, Schema, TxOp,
    Value, ValueType,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn initial() -> Database {
    let mut schema = Schema::new();
    schema
        .install(Attribute::new(
            100,
            Keyword::new("item", "name"),
            ValueType::String,
            Cardinality::One,
        ))
        .unwrap();
    Database::new(schema).unwrap()
}

fn add(name: &str) -> TxOp {
    TxOp::Add {
        entity: EntityRef::Id(42),
        attribute: 100,
        value: Value::String(name.into()).into(),
    }
}

fn contributed() -> DatabaseValue {
    initial()
        .with(&[add("Peaches")], 1, EntityRef::Id(99))
        .unwrap()
        .db_after
        .with(&[add("Peaches")], 2, EntityRef::Id(98))
        .unwrap()
        .db_after
        .database_value()
}

#[test]
fn mixed_sequence_retains_captured_contributors_and_projects_only_consumed_rows() {
    let captured = contributed();
    let raw = captured
        .collect_datoms(IndexOrder::Eavt)
        .unwrap()
        .into_iter()
        .filter(|d| d.entity == 42 && d.attribute == 100)
        .collect::<Vec<_>>();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut transforms = EdnPullTransforms::new();
    let observed = Arc::clone(&calls);
    transforms.register(
        "test/observe",
        PullTransform::new("test/observe", move |value| {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(value.clone())
        }),
    );
    let template = parse_query_edn(
        "[:find ?s (pull $ ?e [[:item/name :xform test/observe]]) \
         :in $ $raw \
         :where [$raw ?e _ ?v ?t ?s true] [$ ?e :item/name ?v ?t ?s true]]",
    )
    .unwrap();
    let bind = |db: &DatabaseValue| {
        template
            .bind_with_transforms(
                &[
                    EdnQueryArgument::Source(QuerySourceValue::Database(db.clone())),
                    EdnQueryArgument::Source(QueryDataSource::datoms("$raw", raw.clone()).value),
                ],
                &transforms,
            )
            .unwrap()
    };
    let bound = bind(&captured);
    let mut sequence = bound.sequence(&QueryControl::default(), None).unwrap();
    assert_eq!(sequence.remaining_rows(), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let replaced = captured
        .with(&[add("Pears")], 3, EntityRef::Id(97))
        .unwrap()
        .db_after;
    let row = sequence.next().unwrap().unwrap();
    assert_eq!(row[0], QueryValue::Scalar(Value::Ref(98)));
    let QueryValue::Map(entries) = &row[1] else {
        panic!("expected Pull map")
    };
    assert_eq!(
        entries,
        &vec![(
            QueryValue::Scalar(Value::Keyword(Keyword::new("item", "name"))),
            QueryValue::Scalar(Value::String("Peaches".into())),
        )]
    );
    assert_eq!(sequence.remaining_rows(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        sequence.next().unwrap().unwrap()[0],
        QueryValue::Scalar(Value::Ref(99))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(sequence.next().is_none());
    assert_eq!(
        bind(&replaced)
            .sequence(&QueryControl::default(), None)
            .unwrap()
            .remaining_rows(),
        0
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn entity_identity_survives_source_changes_without_equating_states_or_origins() {
    let captured = contributed();
    let before = captured.entity(42).unwrap().unwrap();
    let old = before.get(100).unwrap();
    let replaced = captured
        .with(&[add("Pears")], 3, EntityRef::Id(97))
        .unwrap()
        .db_after;
    let after = replaced.entity(42).unwrap().unwrap();
    assert_eq!(before, after);
    assert_eq!(before.identity(), after.identity());
    assert!(matches!(old, Some(EntityValue::Scalar(Value::String(name))) if name == "Peaches"));
    assert!(
        matches!(before.get(100).unwrap(), Some(EntityValue::Scalar(Value::String(name))) if name == "Peaches")
    );
    assert!(
        matches!(after.get(100).unwrap(), Some(EntityValue::Scalar(Value::String(name))) if name == "Pears")
    );
    let independent = contributed().entity(42).unwrap().unwrap();
    assert_ne!(before, independent);
    assert_ne!(before.identity(), independent.identity());
}
