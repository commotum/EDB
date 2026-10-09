use edb_core::edn_pull::parse_pull_edn;
use edb_core::{
    Attribute, Cardinality, Database, DatabaseValue, EntityRef, IndexBoundary, IndexComponents,
    IndexPullOptions, IndexTransaction, Keyword, PullControl, QueryValue, Schema, TxOp, TxValue,
    Value, ValueType,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn initial() -> Database {
    let mut schema = Schema::new();
    for (id, name, ty, cardinality) in [
        (100, "number", ValueType::Long, Cardinality::One),
        (101, "targets", ValueType::Ref, Cardinality::Many),
        (102, "name", ValueType::String, Cardinality::One),
    ] {
        let mut attr = Attribute::new(id, Keyword::new("item", name), ty, cardinality);
        attr.indexed = true;
        schema.install(attr).unwrap();
    }
    Database::new(schema).unwrap()
}

fn add(entity: u64, attribute: u32, value: Value) -> TxOp {
    TxOp::Add {
        entity: EntityRef::Id(entity),
        attribute,
        value: TxValue::Scalar(value),
    }
}

fn retract(entity: u64, attribute: u32, value: Value) -> TxOp {
    TxOp::Retract {
        entity: EntityRef::Id(entity),
        attribute,
        value: Some(TxValue::Scalar(value)),
    }
}

fn rows(
    db: &DatabaseValue,
    start: IndexBoundary,
    reverse: bool,
    selector: &str,
) -> Vec<QueryValue> {
    let mut options = IndexPullOptions::new(start, parse_pull_edn(selector).unwrap());
    options.reverse = reverse;
    db.index_pull(options)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn map(entity: u64, name: Option<&str>) -> QueryValue {
    let mut entries = vec![(
        QueryValue::Scalar(Value::Keyword(Keyword::new("db", "id"))),
        QueryValue::Scalar(Value::Ref(entity)),
    )];
    if let Some(name) = name {
        entries.push((
            QueryValue::Scalar(Value::Keyword(Keyword::new("item", "name"))),
            QueryValue::Scalar(Value::String(name.into())),
        ));
    }
    QueryValue::Map(entries)
}

#[test]
fn index_pull_source_and_temporal_views_resolve_withdrawals_before_full_boundaries() {
    let facts = vec![
        add(500, 100, Value::Long(1)),
        add(501, 100, Value::Long(2)),
        add(600, 101, Value::Ref(500)),
        add(601, 101, Value::Ref(501)),
        add(500, 102, Value::String("former".into())),
        add(501, 102, Value::String("next".into())),
    ];
    let first = initial().with(&facts, 1, EntityRef::Id(99)).unwrap();
    let first_t = first.db_after.basis_t();
    let first_tx = first.tx_data[0].tx;
    let eager_two = first
        .db_after
        .with(&facts, 2, EntityRef::Id(98))
        .unwrap()
        .db_after;
    let overlay_two = first
        .db_after
        .database_value()
        .with(&facts, 2, EntityRef::Id(98))
        .unwrap()
        .db_after;
    let changes = vec![
        retract(500, 100, Value::Long(1)),
        retract(600, 101, Value::Ref(500)),
        add(500, 102, Value::String("replacement".into())),
    ];
    let eager = eager_two
        .with(&changes, 3, EntityRef::Id(97))
        .unwrap()
        .db_after
        .database_value();
    let overlay = overlay_two
        .with(&changes, 3, EntityRef::Id(97))
        .unwrap()
        .db_after;
    for db in [eager, overlay] {
        for reverse in [false, true] {
            for start in [
                IndexBoundary::Avet(IndexComponents::Five(
                    100,
                    Value::Long(1),
                    500,
                    IndexTransaction::Tx(first_tx),
                    99,
                )),
                IndexBoundary::Aevt(IndexComponents::Five(
                    101,
                    600,
                    Value::Ref(500),
                    IndexTransaction::Tx(first_tx),
                    99,
                )),
            ] {
                let expected = if reverse {
                    vec![]
                } else {
                    vec![map(501, Some("next"))]
                };
                assert_eq!(
                    rows(
                        &db.clone().filter(|_, d| d.source == 99),
                        start.clone(),
                        reverse,
                        "[:db/id :item/name]"
                    ),
                    expected
                );
                let historical = db.clone().as_of(first_t).filter(|_, d| d.source == 99);
                let mut expected = vec![map(500, Some("former"))];
                if !reverse {
                    expected.push(map(501, Some("next")));
                }
                assert_eq!(
                    rows(&historical, start, reverse, "[:db/id :item/name]"),
                    expected
                );
            }
        }
        // The surviving source-98 roots remain selectable since T1; both
        // the index walk and projected fields use that same view.
        let recent_source = db.clone().since(first_t).filter(|_, d| d.source == 98);
        for start in [
            IndexBoundary::Avet(IndexComponents::One(100)),
            IndexBoundary::Aevt(IndexComponents::One(101)),
        ] {
            assert_eq!(
                rows(&recent_source, start, false, "[:db/id :item/name]"),
                vec![map(501, Some("next"))]
            );
        }
        assert!(
            db.history()
                .index_pull(IndexPullOptions::new(
                    IndexBoundary::Avet(IndexComponents::One(100)),
                    parse_pull_edn("[:db/id]").unwrap()
                ))
                .is_err()
        );
    }
}

#[test]
fn index_pull_projects_the_selected_captured_view_and_resets_entity_allowance() {
    let old = initial()
        .with(
            &[
                add(500, 100, Value::Long(1)),
                add(501, 100, Value::Long(2)),
                add(500, 102, Value::String("original".into())),
                add(501, 102, Value::String("other".into())),
            ],
            1,
            EntityRef::Id(99),
        )
        .unwrap()
        .db_after
        .database_value();
    let newer = old
        .with(
            &[add(500, 102, Value::String("changed".into()))],
            2,
            EntityRef::Id(98),
        )
        .unwrap()
        .db_after;
    let selector = parse_pull_edn("[:db/id :item/name]").unwrap();
    let control = PullControl {
        max_entities: 1,
        ..PullControl::default()
    };
    let start = IndexBoundary::Avet(IndexComponents::One(100));
    let mut cursor = old
        .index_pull_with_control(IndexPullOptions::new(start.clone(), selector), &control)
        .unwrap();
    assert_eq!(cursor.next().unwrap().unwrap(), map(500, Some("original")));
    assert_eq!(cursor.next().unwrap().unwrap(), map(501, Some("other")));
    assert!(cursor.next().is_none());
    assert_eq!(
        rows(&newer, start.clone(), false, "[:db/id :item/name]")[0],
        map(500, Some("changed"))
    );
    let source_view = newer.filter(|_, d| d.source == 99);
    assert_eq!(
        rows(&source_view, start, false, "[:db/id :item/name]"),
        vec![map(500, None), map(501, Some("other"))]
    );
}

#[test]
fn index_pull_zero_limit_validates_without_visits_and_offset_cancellation_is_fused() {
    let db = initial()
        .with(&[add(500, 100, Value::Long(1))], 1, EntityRef::Id(99))
        .unwrap()
        .db_after
        .database_value();
    for accepts_candidate in [false, true] {
        let visits = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&visits);
        let control = PullControl::default();
        let cancel = Arc::clone(&control.cancel);
        let filtered = db.clone().filter(move |_, _| {
            observed.fetch_add(1, Ordering::Relaxed);
            cancel.store(true, Ordering::Relaxed);
            accepts_candidate
        });
        let mut options = IndexPullOptions::new(
            IndexBoundary::Avet(IndexComponents::One(100)),
            parse_pull_edn("[:db/id]").unwrap(),
        );
        options.limit = Some(0);
        let mut zero = filtered
            .index_pull_with_control(options.clone(), &control)
            .unwrap();
        assert_eq!(visits.load(Ordering::Relaxed), 0);
        assert!(zero.next().is_none());
        assert_eq!(visits.load(Ordering::Relaxed), 0);
        options.start = IndexBoundary::Aevt(IndexComponents::One(100));
        assert_eq!(
            filtered.index_pull(options.clone()).unwrap_err().code,
            "index-pull/aevt-not-ref-many"
        );
        options.start = IndexBoundary::Avet(IndexComponents::One(100));
        options.limit = None;
        options.offset = 1;
        let mut cursor = filtered.index_pull_with_control(options, &control).unwrap();
        assert_eq!(cursor.next().unwrap().unwrap_err().code, "pull/canceled");
        assert_eq!(visits.load(Ordering::Relaxed), 1);
        assert!(cursor.next().is_none());
        assert!(cursor.next().is_none());
    }
}
