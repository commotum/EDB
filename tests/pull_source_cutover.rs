use edb_core::edn_pull::{EdnPullTransforms, parse_pull_edn, parse_pull_edn_with_transforms};
use edb_core::edn_query::{EdnQueryArgument, parse_query_edn};
use edb_core::{
    Attribute, AttributeName, Cardinality, Database, DatabaseValue, EntityRef, EntityValue,
    IndexBoundary, IndexComponents, IndexOrder, IndexPullOptions, IndexTransaction, Keyword,
    PullControl, PullDirection, PullTransform, QueryControl, QueryResult, QuerySourceValue,
    QueryValue, Schema, TxOp, TxValue, Value, ValueType,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn initial() -> Database {
    let mut schema = Schema::new();
    for (id, name, ty, cardinality, component) in [
        (100, "name", ValueType::String, Cardinality::One, false),
        (101, "tag", ValueType::String, Cardinality::Many, false),
        (102, "friends", ValueType::Ref, Cardinality::Many, false),
        (103, "children", ValueType::Ref, Cardinality::Many, true),
        (104, "link", ValueType::Ref, Cardinality::One, false),
        (105, "number", ValueType::Long, Cardinality::Many, false),
    ] {
        let mut attribute = Attribute::new(id, Keyword::new("item", name), ty, cardinality);
        if component {
            attribute = attribute.component();
        }
        attribute.indexed = true;
        schema.install(attribute).unwrap();
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

fn facts() -> Vec<TxOp> {
    let mut facts = vec![
        add(500, 100, Value::String("root".into())),
        add(500, 104, Value::Ref(600)),
        add(500, 103, Value::Ref(700)),
        add(500, 103, Value::Ref(701)),
        add(700, 103, Value::Ref(710)),
        add(601, 101, Value::String("nested-a".into())),
        add(602, 101, Value::String("nested-b".into())),
    ];
    for tag in ["a", "b", "c"] {
        facts.push(add(500, 101, Value::String(tag.into())));
    }
    for entity in [600, 601, 602] {
        facts.push(add(500, 102, Value::Ref(entity)));
        facts.push(add(entity, 100, Value::String(format!("friend-{entity}"))));
    }
    for holder in [501, 502] {
        facts.push(add(holder, 100, Value::String(format!("holder-{holder}"))));
        facts.push(add(holder, 102, Value::Ref(600)));
        facts.push(add(holder, 104, Value::Ref(600)));
    }
    for entity in [700, 701, 710] {
        facts.push(add(entity, 100, Value::String(format!("child-{entity}"))));
    }
    facts
}

fn pull(db: &DatabaseValue, selector: &str, entity: u64) -> QueryValue {
    db.pull(&parse_pull_edn(selector).unwrap(), entity).unwrap()
}

fn scalar(value: Value) -> QueryValue {
    QueryValue::Scalar(value)
}

fn id_map(entity: u64) -> QueryValue {
    QueryValue::Map(vec![(
        scalar(Value::Keyword(Keyword::new("db", "id"))),
        scalar(Value::Ref(entity)),
    )])
}

fn attribute<'a>(result: &'a QueryValue, name: &str) -> &'a QueryValue {
    let QueryValue::Map(entries) = result else {
        panic!("expected map")
    };
    let key = scalar(Value::Keyword(Keyword::new("item", name)));
    &entries.iter().find(|(k, _)| k == &key).unwrap().1
}

fn collection(value: &QueryValue) -> &[QueryValue] {
    let QueryValue::Collection(values) = value else {
        panic!("expected collection")
    };
    values
}

#[test]
fn every_pull_shape_projects_values_once_across_eager_and_overlay_sources() {
    let first = initial().with(&facts(), 1, EntityRef::Id(99)).unwrap();
    let single = first.db_after.database_value();
    let eager = first
        .db_after
        .with(&facts(), 2, EntityRef::Id(98))
        .unwrap()
        .db_after
        .database_value();
    let overlay = single
        .with(&facts(), 2, EntityRef::Id(98))
        .unwrap()
        .db_after;
    for (entity, selector) in [
        (500, "[:item/name :item/link :item/tag :item/friends]"),
        (500, "[*]"),
        (500, "[[:item/tag :limit 2]]"),
        (500, "[[:item/tag :limit nil]]"),
        (500, "[{:item/friends [:db/id :item/name]}]"),
        (500, "[{[:item/friends :limit 2] [:item/tag]}]"),
        (
            500,
            "[[:item/name :as \"name\" :xform str] [:item/missing :default 0]]",
        ),
        (500, "[{:item/link [:item/name]}]"),
        (500, "[:item/children]"),
        (
            500,
            "[{:item/children [:item/name {:item/children [:item/name]}]}]",
        ),
        (500, "[:item/name {:item/children ...}]"),
        (500, "[:item/name {:item/friends 2}]"),
        (600, "[:item/_friends :item/_link]"),
        (600, "[[:item/_friends :limit 2]]"),
        (600, "[{[:item/_friends :limit 2] [:item/name]}]"),
        (700, "[:item/_children]"),
        (700, "[{:item/_children [:item/name]}]"),
    ] {
        let expected = pull(&single, selector, entity);
        for db in [&eager, &overlay] {
            assert_eq!(pull(db, selector, entity), expected, "{selector}");
        }
    }
    for db in [&eager, &overlay] {
        let result = pull(db, "[:item/name :item/tag :item/friends]", 500);
        assert_eq!(
            attribute(&result, "name"),
            &scalar(Value::String("root".into()))
        );
        assert_eq!(
            attribute(&result, "tag"),
            &QueryValue::Collection(
                ["a", "b", "c"]
                    .map(|v| scalar(Value::String(v.into())))
                    .to_vec()
            )
        );
        assert_eq!(
            attribute(&result, "friends"),
            &QueryValue::Collection([600, 601, 602].map(id_map).to_vec())
        );
        let reverse = pull(db, "[[:item/_friends :limit 2]]", 600);
        assert_eq!(
            attribute(&reverse, "_friends"),
            &QueryValue::Collection([500, 501].map(id_map).to_vec())
        );
        let reverse_component = pull(db, "[:item/_children]", 700);
        assert_eq!(attribute(&reverse_component, "_children"), &id_map(500));
        // Empty nested maps are omitted after selecting the logical prefix:
        // 600 is selected but absent; 602 must not replace it under limit 2.
        let nested = pull(db, "[{[:item/friends :limit 2] [:item/tag]}]", 500);
        assert_eq!(collection(attribute(&nested, "friends")).len(), 1);
        assert_eq!(
            attribute(&collection(attribute(&nested, "friends"))[0], "tag"),
            &QueryValue::Collection(vec![scalar(Value::String("nested-a".into()))])
        );
        let controlled = PullControl {
            max_entities: 4,
            ..PullControl::default()
        };
        assert_eq!(
            db.pull_with_control(
                &parse_pull_edn("[:item/children]").unwrap(),
                500,
                &controlled
            )
            .unwrap(),
            pull(&single, "[:item/children]", 500)
        );
        assert_eq!(
            db.scan_cursor(IndexOrder::Eavt)
                .unwrap()
                .map(Result::unwrap)
                .filter(|d| d.entity == 500 && d.attribute == 101)
                .count(),
            6
        );
        assert_eq!(
            db.pull_many(&parse_pull_edn("[:item/tag]").unwrap(), [500, 500])
                .unwrap(),
            vec![pull(&single, "[:item/tag]", 500); 2]
        );
    }
}

#[test]
fn default_and_explicit_limits_count_values_before_transforming() {
    let facts: Vec<_> = (0..1005).map(|n| add(500, 105, Value::Long(n))).collect();
    let one = initial()
        .with(&facts, 1, EntityRef::Id(99))
        .unwrap()
        .db_after
        .database_value();
    let two = one.with(&facts, 2, EntityRef::Id(98)).unwrap().db_after;
    for db in [&two, &two.clone().filter(|_, d| d.source == 99)] {
        let result = pull(db, "[:item/number]", 500);
        let values = collection(attribute(&result, "number"));
        assert_eq!(values.len(), 1000);
        assert_eq!(values.last(), Some(&scalar(Value::Long(999))));
        let unlimited = pull(db, "[[:item/number :limit nil]]", 500);
        assert_eq!(collection(attribute(&unlimited, "number")).len(), 1005);
        let limited = pull(db, "[[:item/number :limit 2]]", 500);
        assert_eq!(
            attribute(&limited, "number"),
            &QueryValue::Collection(vec![scalar(Value::Long(0)), scalar(Value::Long(1))])
        );
        let mut transforms = EdnPullTransforms::new();
        transforms.register(
            "test/count",
            PullTransform::new("test/count", |value| {
                Ok(scalar(Value::Long(collection(value).len() as i64)))
            }),
        );
        let pattern = parse_pull_edn_with_transforms(
            "[[:item/number :limit 2 :xform test/count]]",
            &transforms,
        )
        .unwrap();
        assert_eq!(
            attribute(&db.pull(&pattern, 500).unwrap(), "number"),
            &scalar(Value::Long(2))
        );
    }
}

#[test]
fn entity_navigation_is_set_valued_for_forward_reverse_and_components() {
    let one = initial()
        .with(&facts(), 1, EntityRef::Id(99))
        .unwrap()
        .db_after
        .database_value();
    let two = one.with(&facts(), 2, EntityRef::Id(98)).unwrap().db_after;
    let entity = two.entity(500).unwrap().unwrap().touch().unwrap();
    assert!(
        matches!(entity.get(100).unwrap(), Some(EntityValue::Scalar(Value::String(value))) if value == "root")
    );
    for (attribute, len) in [(101, 3), (102, 3), (103, 2)] {
        assert!(
            matches!(entity.get(attribute).unwrap(), Some(EntityValue::Collection(values)) if values.len() == len)
        );
    }
    let friend = two.entity(600).unwrap().unwrap();
    assert_eq!(
        friend
            .reverse(102)
            .unwrap()
            .iter()
            .map(|e| e.id())
            .collect::<Vec<_>>(),
        vec![500, 501, 502]
    );
    assert_eq!(
        friend
            .reverse(104)
            .unwrap()
            .iter()
            .map(|e| e.id())
            .collect::<Vec<_>>(),
        vec![500, 501, 502]
    );
    let component = two.entity(700).unwrap().unwrap();
    assert!(
        matches!(component.get_direction(&PullDirection::Reverse(AttributeName::Id(103))).unwrap(), Some(EntityValue::Entity(parent)) if parent.id() == 500)
    );
}

#[test]
fn filtered_temporal_pull_honors_global_withdrawals_and_preserves_history() {
    let first = initial().with(&facts(), 1, EntityRef::Id(99)).unwrap();
    let one = first.db_after.database_value();
    let second = one.with(&facts(), 2, EntityRef::Id(98)).unwrap();
    let two = second.db_after;
    let retractions: Vec<_> = [
        (500, 101, Value::String("a".into())),
        (500, 102, Value::Ref(600)),
        (500, 103, Value::Ref(700)),
        (500, 104, Value::Ref(600)),
    ]
    .into_iter()
    .map(|(entity, attribute, value)| TxOp::Retract {
        entity: EntityRef::Id(entity),
        attribute,
        value: Some(TxValue::Scalar(value)),
    })
    .collect();
    let three = two
        .with(&retractions, 3, EntityRef::Id(97))
        .unwrap()
        .db_after;
    for db in [
        three.clone(),
        three.clone().filter(|_, d| d.source == 99),
        three.clone().filter(|_, d| d.source != 97),
    ] {
        let result = pull(
            &db,
            "[:item/tag :item/friends :item/link :item/children]",
            500,
        );
        assert_eq!(
            attribute(&result, "tag"),
            &QueryValue::Collection(["b", "c"].map(|v| scalar(Value::String(v.into()))).to_vec())
        );
        assert_eq!(
            attribute(&result, "friends"),
            &QueryValue::Collection([601, 602].map(id_map).to_vec())
        );
        assert_eq!(collection(attribute(&result, "children")).len(), 1);
        assert_eq!(pull(&db, "[:item/link]", 500), QueryValue::Map(vec![]));
        let reverse = pull(&db, "[:item/_friends]", 600);
        assert_eq!(
            attribute(&reverse, "_friends"),
            &QueryValue::Collection([501, 502].map(id_map).to_vec())
        );
        assert_eq!(pull(&db, "[:item/_children]", 700), QueryValue::Map(vec![]));
    }
    let historical = three.clone().as_of(two.basis_t());
    for db in [
        historical.clone(),
        historical.clone().filter(|_, d| d.source == 99),
        historical.clone().since(one.basis_t()),
    ] {
        assert_eq!(pull(&db, "[*]", 500), pull(&one, "[*]", 500));
        assert_eq!(
            pull(&db, "[:item/_friends]", 600),
            pull(&one, "[:item/_friends]", 600)
        );
    }
    // A new assertion starts a new contribution after the global withdrawal;
    // older source 99 must remain absent in current source-filtered reads.
    let four = three
        .with(
            &[add(500, 101, Value::String("a".into()))],
            4,
            EntityRef::Id(98),
        )
        .unwrap()
        .db_after;
    assert_eq!(
        pull(&four, "[:item/tag]", 500),
        pull(&one, "[:item/tag]", 500)
    );
    assert_eq!(
        pull(&four.filter(|_, d| d.source == 99), "[:item/tag]", 500),
        pull(&three, "[:item/tag]", 500)
    );
    let history = three.history().filter(|_, d| {
        d.entity == 500 && d.attribute == 101 && d.value == Value::String("a".into())
    });
    let datoms = history
        .scan_cursor(IndexOrder::Eavt)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(datoms.len(), 3);
    assert!(datoms.iter().any(|d| d.source == 99 && d.added));
    assert!(datoms.iter().any(|d| d.source == 98 && d.added));
    assert!(datoms.iter().any(|d| d.source == 97 && !d.added));
    assert!(history.pull(&parse_pull_edn("[*]").unwrap(), 500).is_err());
    assert!(history.entity(500).is_err());
    assert!(
        history
            .pull_many(&parse_pull_edn("[*]").unwrap(), Vec::<u64>::new())
            .is_err()
    );
}

#[test]
fn index_pull_retains_datoms_but_each_projection_has_distinct_values() {
    let first = initial()
        .with(&facts(), 1, EntityRef::Id(99))
        .unwrap()
        .db_after;
    let db = first
        .with(&facts(), 2, EntityRef::Id(98))
        .unwrap()
        .db_after
        .database_value();
    for reverse in [false, true] {
        for (attribute, value, projected_entity) in [
            (101, Value::String("a".into()), 500),
            (102, Value::Ref(600), 600),
        ] {
            let fact = db
                .scan_cursor(IndexOrder::Eavt)
                .unwrap()
                .map(Result::unwrap)
                .find(|d| {
                    d.entity == 500
                        && d.attribute == attribute
                        && d.value == value
                        && d.source == 98
                })
                .unwrap();
            let start = if attribute == 101 {
                IndexBoundary::Avet(IndexComponents::Five(
                    attribute,
                    value,
                    500,
                    IndexTransaction::Tx(fact.tx),
                    fact.source,
                ))
            } else {
                IndexBoundary::Aevt(IndexComponents::Five(
                    attribute,
                    500,
                    value,
                    IndexTransaction::Tx(fact.tx),
                    fact.source,
                ))
            };
            let mut options =
                IndexPullOptions::new(start, parse_pull_edn("[:item/name :item/tag]").unwrap());
            options.reverse = reverse;
            options.limit = Some(1);
            assert_eq!(
                db.index_pull(options)
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap(),
                vec![pull(&db, "[:item/name :item/tag]", projected_entity)]
            );
        }
        // Six visible source contributions to three logical tag values project
        // the root six times, as raw index traversal specifies.
        let start = IndexBoundary::Avet(IndexComponents::Two(
            101,
            Value::String(if reverse { "c" } else { "a" }.into()),
        ));
        let mut options = IndexPullOptions::new(start, parse_pull_edn("[:item/tag]").unwrap());
        options.reverse = reverse;
        let rows = db
            .index_pull(options)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let mut expected = vec![pull(&db, "[:item/tag]", 500); 6];
        if !reverse {
            // The seek is not an equality constraint: later tag values on
            // other entities stay in the same attribute walk.
            expected.extend(vec![pull(&db, "[:item/tag]", 601); 2]);
            expected.extend(vec![pull(&db, "[:item/tag]", 602); 2]);
        }
        assert_eq!(rows, expected);
        // Offsets continue to count datoms, including another contribution to
        // the same EAV; limit still caps projected maps.
        let start = IndexBoundary::Aevt(IndexComponents::Two(102, if reverse { 502 } else { 500 }));
        let mut options =
            IndexPullOptions::new(start, parse_pull_edn("[:item/name :item/tag]").unwrap());
        options.reverse = reverse;
        options.offset = 1;
        options.limit = Some(2);
        let rows = db
            .index_pull(options)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], pull(&db, "[:item/name :item/tag]", 600));
        if !reverse {
            assert_eq!(rows[1], pull(&db, "[:item/name :item/tag]", 601));
        } else {
            assert_eq!(rows[1], pull(&db, "[:item/name :item/tag]", 600));
        }
    }
}

#[test]
fn query_pull_uses_the_same_logical_selection() {
    let one = initial()
        .with(&facts(), 1, EntityRef::Id(99))
        .unwrap()
        .db_after
        .database_value();
    let db = one.with(&facts(), 2, EntityRef::Id(98)).unwrap().db_after;
    let selector = "[:item/tag {[:item/friends :limit 2] [:item/name]}]";
    let query = parse_query_edn(&format!(
        "[:find (pull ?e {selector}) . :where [?e :item/name \"root\"]]"
    ))
    .unwrap()
    .bind(&[EdnQueryArgument::Source(QuerySourceValue::Database(
        db.clone(),
    ))])
    .unwrap();
    for force_scan in [false, true] {
        assert_eq!(
            query
                .execute(
                    &QueryControl {
                        force_scan,
                        ..QueryControl::default()
                    },
                    None
                )
                .unwrap()
                .result,
            QueryResult::Scalar(Some(pull(&one, selector, 500)))
        );
    }
}

#[test]
fn cancellation_still_checks_duplicate_source_candidates() {
    let one = initial()
        .with(&facts(), 1, EntityRef::Id(99))
        .unwrap()
        .db_after
        .database_value();
    let two = one.with(&facts(), 2, EntityRef::Id(98)).unwrap().db_after;
    let control = PullControl::default();
    let cancel = Arc::clone(&control.cancel);
    let reads = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&reads);
    let filtered = two.filter(move |_, datom| {
        if datom.entity == 500
            && datom.attribute == 101
            && observed.fetch_add(1, Ordering::Relaxed) == 1
        {
            cancel.store(true, Ordering::Relaxed);
        }
        true
    });
    let error = filtered
        .pull_with_control(
            &parse_pull_edn("[[:item/tag :limit 2]]").unwrap(),
            500,
            &control,
        )
        .unwrap_err();
    assert_eq!(error.code, "pull/canceled");
    assert_eq!(reads.load(Ordering::Relaxed), 2);
}
