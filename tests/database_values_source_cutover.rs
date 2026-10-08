use edb_core::edn_pull::EdnPullTransforms;
use edb_core::edn_query::{EdnQueryArgument, parse_query_edn};
use edb_core::{
    Attribute, Cardinality, Database, DatabaseValue, EntityIdentifier, EntityRef, EntityValue,
    IndexOrder, IndexPrefix, Keyword, PullTransform, QueryControl, QueryDataSource,
    QuerySourceValue, QueryValue, Schema, TimePoint, TxOp, Value, ValueType,
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

#[test]
fn history_statistics_count_attributed_events_with_view_and_control_bounds() {
    let before = contributed();
    let t2 = before.basis_t();
    let after = before
        .with(
            &[TxOp::Retract {
                entity: EntityRef::Id(42),
                attribute: 100,
                value: Some(Value::String("Peaches".into()).into()),
            }],
            3,
            EntityRef::Id(97),
        )
        .unwrap()
        .db_after;
    let names = after.clone().filter(|_, d| d.attribute == 100);
    assert!(names.values(42, 100).unwrap().is_empty());
    let stats = names.db_stats().unwrap();
    assert_eq!(stats.datoms, 3);
    assert_eq!(stats.attrs.len(), 1);
    assert_eq!(stats.attrs[&100].count, 3);
    assert_eq!(names.clone().history().db_stats().unwrap(), stats);
    assert_eq!(names.clone().as_of(t2).db_stats().unwrap().datoms, 2);
    assert_eq!(names.clone().since(t2).db_stats().unwrap().datoms, 1);
    assert_eq!(
        names
            .clone()
            .filter(|_, d| d.source == 99)
            .db_stats()
            .unwrap()
            .datoms,
        1
    );
    let exact = QueryControl {
        max_work: 3,
        max_result_rows: 0,
        ..QueryControl::default()
    };
    assert_eq!(names.db_stats_with_control(&exact).unwrap(), stats);
    let too_small = QueryControl {
        max_work: 2,
        ..QueryControl::default()
    };
    assert_eq!(
        names.db_stats_with_control(&too_small).unwrap_err().code,
        "database-stats/work-limit"
    );
    let canceled = QueryControl::default();
    canceled.cancel.store(true, Ordering::Relaxed);
    let empty = names.clone().filter(|_, _| false);
    assert_eq!(
        empty.db_stats_with_control(&canceled).unwrap_err().code,
        "database-stats/canceled"
    );
    let timed_out = QueryControl {
        timeout: Some(std::time::Duration::ZERO),
        ..QueryControl::default()
    };
    assert_eq!(
        empty.db_stats_with_control(&timed_out).unwrap_err().code,
        "database-stats/timeout"
    );
    assert_eq!(names.db_stats().unwrap(), stats);
}

#[test]
fn instant_points_keep_captured_basis_and_select_earliest_equal_instant() {
    let first = initial()
        .with(&[add("Peaches")], 10, EntityRef::Id(99))
        .unwrap();
    let t1 = first.db_after.basis_t();
    let second = first
        .db_after
        .with(&[add("Peaches")], 10, EntityRef::Id(98))
        .unwrap();
    let t2 = second.db_after.basis_t();
    let third = second
        .db_after
        .with(&[add("Pears")], 20, EntityRef::Id(97))
        .unwrap();
    let eager = third.db_after.database_value();
    let overlay = eager.with(&[], 20, EntityRef::Id(96)).unwrap().db_after;
    let prefix = IndexPrefix::Eavt {
        entity: 42,
        attribute: Some(100),
        value: None,
    };
    let partition = EntityIdentifier::Id(edb_core::DB_PART_USER);
    for captured in [eager, overlay] {
        let past = captured
            .clone()
            .as_of_time_point(TimePoint::Instant(10))
            .unwrap();
        assert_eq!(past.as_of_t(), Some(t1));
        assert_eq!(past.basis_t(), captured.basis_t());
        assert_eq!(
            past.values(42, 100).unwrap(),
            vec![Value::String("Peaches".into())]
        );
        assert_eq!(past.datoms_with_prefix(&prefix).unwrap().len(), 1);
        let later_same_millisecond = captured
            .clone()
            .as_of(t2)
            .since_time_point(TimePoint::Instant(10))
            .unwrap();
        let datoms = later_same_millisecond.datoms_with_prefix(&prefix).unwrap();
        assert_eq!(datoms.len(), 1);
        assert_eq!(datoms[0].source, 98);
        assert_eq!(
            captured
                .entid_at(&partition, TimePoint::Instant(10))
                .unwrap(),
            edb_core::make_eid(edb_core::USER_PARTITION, t1).unwrap()
        );
        assert_eq!(
            captured
                .entid_at(&partition, TimePoint::Instant(15))
                .unwrap(),
            edb_core::make_eid(edb_core::USER_PARTITION, third.db_after.basis_t()).unwrap()
        );
        assert_eq!(
            captured
                .clone()
                .as_of_time_point(TimePoint::Tx(edb_core::t_to_tx(t2).unwrap()))
                .unwrap()
                .as_of_t(),
            Some(t2)
        );
        assert_eq!(
            captured
                .clone()
                .as_of_time_point(TimePoint::Instant(15))
                .unwrap()
                .as_of_t(),
            Some(t2)
        );
    }
}

#[test]
fn filtered_speculation_checks_full_basis_then_reapplies_read_view() {
    let first = initial()
        .with(&[add("Peaches")], 1, EntityRef::Id(99))
        .unwrap();
    let t1 = first.db_after.basis_t();
    let current = first
        .db_after
        .with(&[add("Pears")], 2, EntityRef::Id(98))
        .unwrap()
        .db_after
        .database_value();
    let view = current.clone().as_of(t1).filter(|_, d| d.attribute == 100);
    assert_eq!(
        view.values(42, 100).unwrap(),
        vec![Value::String("Peaches".into())]
    );
    let cas = |old: &str| TxOp::Cas {
        entity: EntityRef::Id(42),
        attribute: 100,
        old: Some(Value::String(old.into()).into()),
        new: Value::String("Plums".into()).into(),
    };
    assert!(view.with(&[cas("Peaches")], 3, EntityRef::Id(97)).is_err());
    let proposed = view.with(&[cas("Pears")], 3, EntityRef::Id(97)).unwrap();
    assert_eq!(proposed.db_after.basis_t(), current.basis_t() + 1);
    assert_eq!(proposed.db_before.as_of_t(), Some(t1));
    assert_eq!(proposed.db_after.as_of_t(), Some(t1));
    assert!(proposed.db_after.is_filtered());
    assert_eq!(
        proposed.db_after.values(42, 100).unwrap(),
        view.values(42, 100).unwrap()
    );
    let new_current = proposed.db_after.clone().as_of(proposed.db_after.basis_t());
    assert_eq!(
        new_current.values(42, 100).unwrap(),
        vec![Value::String("Plums".into())]
    );
    assert!(proposed.tx_data.iter().all(|d| d.source == 97));
    assert_eq!(
        proposed
            .tx_data
            .iter()
            .filter(|d| d.attribute == edb_core::DB_TX_INSTANT as u32)
            .count(),
        1
    );
    assert_eq!(
        current.values(42, 100).unwrap(),
        vec![Value::String("Pears".into())]
    );
    assert_eq!(
        view.history()
            .with(&[], 3, EntityRef::Id(97))
            .unwrap_err()
            .code,
        "transaction/history-with"
    );
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
