use edb_core::{
    Attribute, AttributeName, Cardinality, Database, DatabaseValue, EntityIdentifier, EntityRef,
    IndexBoundary, IndexComponents, IndexOrder, IndexPrefix, IndexTransaction, Keyword,
    RawIndexValue, Schema, TxOp, TxValue, Unique, Value,
};

fn initial() -> Database {
    let mut schema = Schema::new();
    schema
        .install(
            Attribute::new(
                100,
                Keyword::new("item", "link"),
                edb_core::ValueType::Ref,
                Cardinality::One,
            )
            .unique(Unique::Identity),
        )
        .unwrap();
    Database::new(schema).unwrap()
}

fn add() -> TxOp {
    TxOp::Add {
        entity: EntityRef::Id(42),
        attribute: 100,
        value: TxValue::Scalar(Value::Ref(7)),
    }
}

fn retract() -> TxOp {
    TxOp::Retract {
        entity: EntityRef::Id(42),
        attribute: 100,
        value: Some(TxValue::Scalar(Value::Ref(7))),
    }
}

fn source_prefix(source: u64, order: IndexOrder) -> IndexPrefix {
    match order {
        IndexOrder::Saevt => IndexPrefix::Saevt {
            source,
            attribute: Some(100),
            entity: Some(42),
            value: Some(Value::Ref(7)),
        },
        IndexOrder::Savet => IndexPrefix::Savet {
            source,
            attribute: Some(100),
            value: Some(Value::Ref(7)),
            entity: Some(42),
        },
        _ => unreachable!(),
    }
}

fn item_count(db: &DatabaseValue, order: IndexOrder) -> usize {
    db.scan_cursor(order)
        .unwrap()
        .map(Result::unwrap)
        .filter(|d| d.entity == 42 && d.attribute == 100)
        .count()
}

#[test]
fn temporal_views_keep_source_contributions_and_project_one_value() {
    let first = initial().with(&[add()], 1, EntityRef::Id(99)).unwrap();
    let second = first.db_after.with(&[add()], 2, EntityRef::Id(98)).unwrap();
    let t1 = first.db_after.basis_t();
    let t2 = second.db_after.basis_t();
    let third = second
        .db_after
        .with(&[retract()], 3, EntityRef::Id(97))
        .unwrap();
    let db = third.db_after.database_value();
    let historical = db.clone().as_of(t2);
    assert_eq!(historical.values(42, 100).unwrap(), vec![Value::Ref(7)]);
    for order in [
        IndexOrder::Eavt,
        IndexOrder::Aevt,
        IndexOrder::Avet,
        IndexOrder::Vaet,
        IndexOrder::Saevt,
        IndexOrder::Savet,
    ] {
        assert_eq!(item_count(&db, order), 0);
        assert_eq!(item_count(&historical, order), 2);
        assert_eq!(item_count(&db.clone().as_of(t1), order), 1);
        assert_eq!(item_count(&historical.clone().since(t1), order), 1);
        assert_eq!(item_count(&db.clone().since(t1), order), 0);
        assert_eq!(item_count(&db.clone().history(), order), 3);
    }
    for order in [IndexOrder::Saevt, IndexOrder::Savet] {
        let prefix = source_prefix(99, order);
        assert!(db.prefix_cursor(&prefix).unwrap().next().is_none());
        assert_eq!(
            historical
                .collect_datoms_with_prefix(&prefix)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            db.clone()
                .history()
                .collect_datoms_with_prefix(&prefix)
                .unwrap()
                .len(),
            1
        );
    }
    assert_eq!(historical.lookup(100, &Value::Ref(7)).unwrap(), Some(42));
}

#[test]
fn predicates_cannot_restore_facts_withdrawn_by_another_source() {
    let first = initial().with(&[add()], 1, EntityRef::Id(99)).unwrap();
    let second = first.db_after.with(&[add()], 2, EntityRef::Id(98)).unwrap();
    let withdrawn = second
        .db_after
        .with(&[retract()], 3, EntityRef::Id(97))
        .unwrap();
    let db = withdrawn.db_after.database_value();
    for order in [IndexOrder::Eavt, IndexOrder::Saevt, IndexOrder::Savet] {
        assert_eq!(
            item_count(&db.clone().filter(|_, d| d.source == 99), order),
            0
        );
        assert_eq!(
            item_count(&db.clone().filter(|_, d| d.source != 97), order),
            0
        );
        assert_eq!(
            item_count(&db.clone().history().filter(|_, d| d.source != 97), order),
            2
        );
    }
    let earlier = db
        .clone()
        .as_of(second.db_after.basis_t())
        .filter(|_, d| d.source == 99);
    assert_eq!(earlier.values(42, 100).unwrap(), vec![Value::Ref(7)]);
}

#[test]
fn speculative_overlays_keep_contributions_and_global_withdrawal() {
    let eager = initial().with(&[add()], 1, EntityRef::Id(99)).unwrap();
    let one = eager.db_after.database_value();
    let two = one.with(&[add()], 2, EntityRef::Id(98)).unwrap();
    let three = two
        .db_after
        .with(&[retract()], 3, EntityRef::Id(97))
        .unwrap();
    assert_eq!(two.db_after.values(42, 100).unwrap(), vec![Value::Ref(7)]);
    for order in [
        IndexOrder::Eavt,
        IndexOrder::Aevt,
        IndexOrder::Avet,
        IndexOrder::Vaet,
        IndexOrder::Saevt,
        IndexOrder::Savet,
    ] {
        assert_eq!(item_count(&two.db_after, order), 2);
        assert_eq!(item_count(&three.db_after, order), 0);
        assert_eq!(
            item_count(&three.db_after.clone().as_of(two.db_after.basis_t()), order),
            2
        );
        assert_eq!(item_count(&three.db_after.clone().history(), order), 3);
    }
    assert_eq!(item_count(&one, IndexOrder::Eavt), 1);
    let four = three.db_after.with(&[add()], 4, EntityRef::Id(99)).unwrap();
    assert_eq!(item_count(&four.db_after, IndexOrder::Saevt), 1);
}

#[test]
fn six_boundaries_normalize_source_and_resolve_visibility_before_seeking() {
    let first = initial().with(&[add()], 1, EntityRef::Id(99)).unwrap();
    let second = first.db_after.with(&[add()], 2, EntityRef::Id(98)).unwrap();
    let third = second
        .db_after
        .with(&[retract()], 3, EntityRef::Id(97))
        .unwrap();
    let db = third.db_after.database_value();
    let e = EntityIdentifier::Id(42);
    let a = AttributeName::Ident(Keyword::new("item", "link"));
    let v = RawIndexValue::Entity(EntityIdentifier::Id(7));
    let t = IndexTransaction::T(first.db_after.basis_t());
    let s = EntityIdentifier::Id(99);
    let boundaries = [
        db.eavt_boundary(IndexComponents::Five(
            e.clone(),
            a.clone(),
            v.clone(),
            t,
            s.clone(),
        ))
        .unwrap(),
        db.aevt_boundary(IndexComponents::Five(
            a.clone(),
            e.clone(),
            v.clone(),
            t,
            s.clone(),
        ))
        .unwrap(),
        db.avet_boundary(IndexComponents::Five(
            a.clone(),
            v.clone(),
            e.clone(),
            t,
            s.clone(),
        ))
        .unwrap(),
        db.vaet_boundary(IndexComponents::Five(
            v.clone(),
            a.clone(),
            e.clone(),
            t,
            s.clone(),
        ))
        .unwrap(),
        db.saevt_boundary(IndexComponents::Five(
            s.clone(),
            a.clone(),
            e.clone(),
            v.clone(),
            t,
        ))
        .unwrap(),
        db.savet_boundary(IndexComponents::Five(
            s.clone(),
            a.clone(),
            v.clone(),
            e.clone(),
            t,
        ))
        .unwrap(),
    ];
    for boundary in boundaries {
        assert_eq!(boundary.len(), 5);
        let window = db.clone().filter(|_, _| true);
        assert!(
            !window
                .collect_seek_datoms(&boundary)
                .unwrap()
                .iter()
                .any(|d| d.entity == 42 && d.attribute == 100)
        );
        assert!(
            !window
                .collect_reverse_seek_datoms(&boundary)
                .unwrap()
                .iter()
                .any(|d| d.entity == 42 && d.attribute == 100)
        );
        let past = window.as_of(second.db_after.basis_t());
        assert!(
            past.collect_reverse_seek_datoms(&boundary)
                .unwrap()
                .iter()
                .any(|d| d.entity == 42 && d.attribute == 100 && d.source == 99)
        );
    }
    assert_eq!(
        db.saevt_boundary(IndexComponents::Empty).unwrap(),
        IndexBoundary::Saevt(IndexComponents::Empty)
    );
    assert_eq!(
        db.savet_boundary(IndexComponents::Empty).unwrap(),
        IndexBoundary::Savet(IndexComponents::Empty)
    );
}

#[test]
fn speculative_savet_backfill_keeps_same_value_from_both_sources() {
    let system = EntityRef::Ident(Keyword::new("db.source", "system"));
    let schema = Database::bootstrap()
        .unwrap()
        .with_edn(
            r#"[{:db/ident :item/tag :db/valueType :db.type/string
              :db/cardinality :db.cardinality/one}]"#,
            0,
            system.clone(),
        )
        .unwrap()
        .db_after;
    let first = schema
        .with_edn(
            r#"[{:db/id "item" :item/tag "same"}]"#,
            1,
            EntityRef::Id(99),
        )
        .unwrap();
    let entity = first.tempids["item"];
    let second = first
        .db_after
        .with_edn(
            &format!(r#"[[:db/add {entity} :item/tag "same"]]"#),
            2,
            EntityRef::Id(98),
        )
        .unwrap();
    let enabled = second
        .db_after
        .database_value()
        .with_edn(r#"[{:db/id :item/tag :db/index true}]"#, 3, system)
        .unwrap()
        .db_after;
    let attribute = enabled.entid(&Keyword::new("item", "tag")).unwrap() as u32;
    for source in [98, 99] {
        for attribute_prefix in [None, Some(attribute)] {
            let prefix = IndexPrefix::Savet {
                source,
                attribute: attribute_prefix,
                value: attribute_prefix.map(|_| Value::String("same".into())),
                entity: None,
            };
            let datoms = enabled.collect_datoms_with_prefix(&prefix).unwrap();
            assert_eq!(datoms.iter().filter(|d| d.entity == entity).count(), 1);
        }
    }
    assert_eq!(
        enabled
            .collect_datoms(IndexOrder::Savet)
            .unwrap()
            .iter()
            .filter(|d| d.entity == entity)
            .count(),
        2
    );
}

#[test]
fn callback_context_tracks_later_views_and_removes_every_chained_predicate() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let first = initial().with(&[add()], 1, EntityRef::Id(99)).unwrap();
    let second = first.db_after.with(&[add()], 2, EntityRef::Id(98)).unwrap();
    let t1 = first.db_after.basis_t();
    let t2 = second.db_after.basis_t();
    let third = second
        .db_after
        .with(&[retract()], 3, EntityRef::Id(97))
        .unwrap();
    let t3 = third.db_after.basis_t();
    let db = third.db_after.database_value();
    let prefix = IndexPrefix::Eavt {
        entity: 42,
        attribute: Some(100),
        value: None,
    };
    let saw_current = Arc::new(AtomicBool::new(false));
    let saw_retraction = Arc::new(AtomicBool::new(false));
    let observed_current = Arc::clone(&saw_current);
    let observed_retraction = Arc::clone(&saw_retraction);
    let nested_prefix = prefix.clone();
    let filtered = db
        .clone()
        .filter(move |context, datom| {
            assert!(!context.is_filtered());
            assert_eq!(context.basis_t(), t3);
            assert_eq!(context.since_t(), Some(t1));
            if datom.entity != 42 || datom.attribute != 100 {
                return false;
            }
            // The nested read succeeds without re-entering this callback or
            // the later predicate, and exposes the complete temporal view.
            let nested = context.datoms_with_prefix(&nested_prefix).unwrap();
            if context.is_history() {
                assert_eq!(context.as_of_t(), Some(t3));
                assert_eq!(nested.len(), 2);
                assert!(nested.iter().any(|d| d.source == 98 && d.added));
                assert!(nested.iter().any(|d| d.source == 97 && !d.added));
                assert_eq!(
                    context.entity(42).unwrap_err().code,
                    "database/history-not-point-in-time"
                );
                if !datom.added {
                    observed_retraction.store(true, Ordering::Relaxed);
                }
            } else {
                assert_eq!(context.as_of_t(), Some(t2));
                assert!(datom.added);
                assert_eq!(nested.len(), 1);
                assert_eq!(nested[0].source, 98);
                assert!(
                    context
                        .entity(42)
                        .unwrap()
                        .unwrap()
                        .get(100)
                        .unwrap()
                        .is_some()
                );
                observed_current.store(true, Ordering::Relaxed);
            }
            datom.source == 98
        })
        .filter(|context, datom| {
            // Conjunction short-circuits for every datom rejected above.
            assert!(!context.is_filtered());
            assert_eq!(datom.entity, 42);
            assert_eq!(datom.attribute, 100);
            assert_eq!(datom.source, 98);
            true
        });

    // These operations occur after both predicates are installed.
    let current = filtered.clone().as_of(t2).since(t1);
    let datoms = current.datoms_with_prefix(&prefix).unwrap();
    assert_eq!(datoms.len(), 1);
    assert!(saw_current.load(Ordering::Relaxed));
    assert!(!saw_retraction.load(Ordering::Relaxed));

    let history = filtered.history().as_of(t3).since(t1);
    let events = history.datoms_with_prefix(&prefix).unwrap();
    assert_eq!(events.len(), 1);
    assert!(events[0].added);
    assert!(saw_retraction.load(Ordering::Relaxed));
    assert!(!db.is_filtered());
    assert!(!db.is_history());
    assert_eq!(db.as_of_t(), None);
    assert_eq!(db.since_t(), None);
}
