use edb_core::{
    AttributeName, Database, DatabaseValue, EntityIdentifier, EntityRef, IndexBoundary,
    IndexComponents, IndexPrefix, IndexTransaction, Keyword, RawIndexValue, Value,
};

fn ident(name: &str) -> Keyword {
    Keyword::new("index-test", name)
}

fn fixture() -> Database {
    let system = EntityRef::Ident(Keyword::new("db.source", "system"));
    let installed = Database::bootstrap()
        .unwrap()
        .with_edn(
            r#"[
          {:db/ident :index-test/key :db/valueType :db.type/string
           :db/cardinality :db.cardinality/one :db/unique :db.unique/identity}
          {:db/ident :index-test/link :db/valueType :db.type/ref
           :db/cardinality :db.cardinality/one :db/index true}
          {:db/ident :index-test/pair :db/valueType :db.type/tuple
           :db/cardinality :db.cardinality/many :db/index true
           :db/tupleTypes [:db.type/string :db.type/ref]}
          {:db/ident :index-test/plain :db/valueType :db.type/string
           :db/cardinality :db.cardinality/one}
        ]"#,
            1,
            system.clone(),
        )
        .unwrap()
        .db_after;
    let described = installed
        .with_edn(
            r#"[
          {:db/id "subject" :db/ident :index-test/subject :index-test/key "subject"}
          {:db/id "target" :db/ident :index-test/target :index-test/key "target"}
          {:db/id "source" :db/ident :index-test/source :index-test/key "source"}
        ]"#,
            2,
            system,
        )
        .unwrap()
        .db_after;
    described
        .with_edn(
            r#"[{:db/id :index-test/subject :index-test/link :index-test/target
             :index-test/pair [["a" nil] ["a" :index-test/target] ["b" nil]]}]"#,
            3,
            EntityRef::Ident(ident("source")),
        )
        .unwrap()
        .db_after
}

fn attribute(db: &DatabaseValue, name: &str) -> u32 {
    db.schema().resolve_ident(&ident(name)).unwrap()
}

fn lookup(name: &str) -> EntityIdentifier {
    EntityIdentifier::Lookup {
        attribute: AttributeName::Ident(ident("key")),
        value: Value::String(name.into()),
    }
}

#[test]
fn every_boundary_arity_resolves_ordinary_entity_source_and_reference_forms() {
    let memory = fixture();
    let db = memory.database_value();
    let subject_id = db
        .resolve_entity_identifier(&lookup("subject"))
        .unwrap()
        .unwrap();
    let target_id = db
        .resolve_entity_identifier(&lookup("target"))
        .unwrap()
        .unwrap();
    let source_id = db
        .resolve_entity_identifier(&lookup("source"))
        .unwrap()
        .unwrap();
    let entity = lookup("subject");
    let source = EntityIdentifier::Ident(ident("source"));
    let value = RawIndexValue::Entity(lookup("target"));
    let attr = AttributeName::Ident(ident("link"));
    let attr_id = attribute(&db, "link");
    let tx = IndexTransaction::T(memory.basis_t());
    macro_rules! check {
        ($helper:ident, $variant:ident;
         ($a:expr, $b:expr, $c:expr, $d:expr, $e:expr);
         ($ra:expr, $rb:expr, $rc:expr, $rd:expr, $re:expr)) => {{
            let actual = [
                db.$helper(IndexComponents::Empty).unwrap(),
                db.$helper(IndexComponents::One($a)).unwrap(),
                db.$helper(IndexComponents::Two($a, $b)).unwrap(),
                db.$helper(IndexComponents::Three($a, $b, $c)).unwrap(),
                db.$helper(IndexComponents::Four($a, $b, $c, $d)).unwrap(),
                db.$helper(IndexComponents::Five($a, $b, $c, $d, $e))
                    .unwrap(),
            ];
            let expected = [
                IndexBoundary::$variant(IndexComponents::Empty),
                IndexBoundary::$variant(IndexComponents::One($ra)),
                IndexBoundary::$variant(IndexComponents::Two($ra, $rb)),
                IndexBoundary::$variant(IndexComponents::Three($ra, $rb, $rc)),
                IndexBoundary::$variant(IndexComponents::Four($ra, $rb, $rc, $rd)),
                IndexBoundary::$variant(IndexComponents::Five($ra, $rb, $rc, $rd, $re)),
            ];
            assert_eq!(actual, expected);
            for (arity, boundary) in actual.iter().enumerate() {
                assert_eq!(boundary.len(), arity);
                let forward = db.collect_seek_datoms(boundary).unwrap();
                let reverse = db.collect_reverse_seek_datoms(boundary).unwrap();
                // The target is tied with every supplied leading sequence,
                // so both inclusive seeks must retain its attributed fact.
                for datoms in [&forward, &reverse] {
                    assert!(datoms.iter().any(|d| d.entity == subject_id
                        && d.attribute == attr_id
                        && d.value == Value::Ref(target_id)
                        && d.source == source_id));
                }
            }
        }};
    }
    check!(eavt_boundary, Eavt;
        (entity.clone(), attr.clone(), value.clone(), tx, source.clone());
        (subject_id, attr_id, Value::Ref(target_id), tx, source_id));
    check!(aevt_boundary, Aevt;
        (attr.clone(), entity.clone(), value.clone(), tx, source.clone());
        (attr_id, subject_id, Value::Ref(target_id), tx, source_id));
    check!(avet_boundary, Avet;
        (attr.clone(), value.clone(), entity.clone(), tx, source.clone());
        (attr_id, Value::Ref(target_id), subject_id, tx, source_id));
    check!(vaet_boundary, Vaet;
        (value.clone(), attr.clone(), entity.clone(), tx, source.clone());
        (Value::Ref(target_id), attr_id, subject_id, tx, source_id));
    check!(saevt_boundary, Saevt;
        (source.clone(), attr.clone(), entity.clone(), value.clone(), tx);
        (source_id, attr_id, subject_id, Value::Ref(target_id), tx));
    check!(savet_boundary, Savet;
        (source.clone(), attr.clone(), value.clone(), entity.clone(), tx);
        (source_id, attr_id, Value::Ref(target_id), subject_id, tx));

    // Source lookup refs use the same entity resolver as idents, and T/Tx
    // denote the same stored transaction position.
    let by_lookup = db
        .savet_boundary(IndexComponents::Five(
            lookup("source"),
            attr,
            value,
            entity,
            IndexTransaction::Tx(edb_core::t_to_tx(memory.basis_t()).unwrap()),
        ))
        .unwrap();
    let by_ident = db
        .savet_boundary(IndexComponents::Five(
            source,
            AttributeName::Id(attr_id),
            Value::Ref(target_id).into(),
            EntityIdentifier::Id(subject_id),
            tx,
        ))
        .unwrap();
    assert_eq!(
        db.collect_seek_datoms(&by_lookup).unwrap(),
        db.collect_seek_datoms(&by_ident).unwrap()
    );
}

#[test]
fn partial_tuple_boundaries_keep_lexical_positions_and_resolve_reference_slots() {
    let memory = fixture();
    let db = memory.database_value();
    let target_id = db
        .resolve_entity_identifier(&lookup("target"))
        .unwrap()
        .unwrap();
    let source_id = db
        .resolve_entity_identifier(&lookup("source"))
        .unwrap()
        .unwrap();
    let attr = attribute(&db, "pair");
    let boundary = db
        .savet_boundary(IndexComponents::Three(
            lookup("source"),
            AttributeName::Id(attr),
            RawIndexValue::Tuple(vec![Some(Value::String("a".into()).into())]),
        ))
        .unwrap();
    let selected: Vec<_> = db
        .seek_cursor(&boundary)
        .unwrap()
        .map(Result::unwrap)
        .take_while(|d| d.source == source_id && d.attribute == attr)
        .map(|d| d.value)
        .collect();
    assert_eq!(
        selected,
        vec![
            Value::Tuple(vec![Some(Value::String("a".into())), None]),
            Value::Tuple(vec![
                Some(Value::String("a".into())),
                Some(Value::Ref(target_id))
            ]),
            Value::Tuple(vec![Some(Value::String("b".into())), None]),
        ]
    );
    assert!(
        !db.reverse_seek_cursor(&boundary)
            .unwrap()
            .map(Result::unwrap)
            .any(|d| d.source == source_id && d.attribute == attr)
    );
    let full = db
        .avet_boundary(IndexComponents::Two(
            AttributeName::Id(attr),
            RawIndexValue::Tuple(vec![
                Some(Value::String("a".into()).into()),
                Some(RawIndexValue::Entity(lookup("target"))),
            ]),
        ))
        .unwrap();
    assert_eq!(
        db.seek_cursor(&full)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .value,
        Value::Tuple(vec![
            Some(Value::String("a".into())),
            Some(Value::Ref(target_id))
        ])
    );
    let empty = db
        .avet_boundary(IndexComponents::Two(
            AttributeName::Id(attr),
            RawIndexValue::Tuple(vec![]),
        ))
        .unwrap();
    assert_eq!(
        db.seek_cursor(&empty)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .value,
        Value::Tuple(vec![Some(Value::String("a".into())), None])
    );
    for slots in [vec![Some(Value::Long(1).into())], vec![None, None, None]] {
        assert!(
            db.avet_boundary(IndexComponents::Two(
                AttributeName::Id(attr),
                RawIndexValue::Tuple(slots)
            ))
            .is_err()
        );
    }
}

#[test]
fn avet_and_source_value_ranges_share_eligibility_and_exclusive_upper_bounds() {
    let memory = fixture();
    let db = memory.database_value();
    let source_id = db
        .resolve_entity_identifier(&lookup("source"))
        .unwrap()
        .unwrap();
    let attr = attribute(&db, "key");
    let start = Value::String("source".into());
    let end = Value::String("target".into());
    let values: Vec<_> = memory
        .avet_range(attr, Some(&start), Some(&end))
        .unwrap()
        .iter()
        .map(|d| d.value.clone())
        .collect();
    assert_eq!(
        values,
        vec![
            Value::String("source".into()),
            Value::String("subject".into())
        ]
    );
    assert!(
        memory
            .avet_range(attr, Some(&start), Some(&start))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        memory
            .avet_range(attr, Some(&end), Some(&start))
            .unwrap_err()
            .code,
        "index/invalid-range"
    );
    let system = db
        .resolve_entity_identifier(&EntityIdentifier::Ident(Keyword::new(
            "db.source",
            "system",
        )))
        .unwrap()
        .unwrap();
    let boundary = db
        .savet_boundary(IndexComponents::Three(
            EntityIdentifier::Id(system),
            AttributeName::Id(attr),
            start.into(),
        ))
        .unwrap();
    let source_values: Vec<_> = db
        .seek_cursor(&boundary)
        .unwrap()
        .map(Result::unwrap)
        .take_while(|d| {
            d.source == system && d.attribute == attr && d.value.index_cmp(&end).is_lt()
        })
        .map(|d| d.value)
        .collect();
    assert_eq!(source_values, values);
    let plain = attribute(&db, "plain");
    assert!(!db.has_avet(&AttributeName::Id(plain)).unwrap());
    assert_eq!(
        db.avet_boundary(IndexComponents::One(AttributeName::Id(plain)))
            .unwrap_err()
            .code,
        "index/attribute-not-in-avet"
    );
    assert_eq!(
        db.savet_boundary(IndexComponents::Two(
            lookup("source"),
            AttributeName::Id(plain)
        ))
        .unwrap_err()
        .code,
        "index/attribute-not-in-avet"
    );
    assert!(
        db.datoms_with_prefix(&IndexPrefix::Savet {
            source: source_id,
            attribute: Some(plain),
            value: None,
            entity: None
        })
        .unwrap()
        .is_empty()
    );
}
