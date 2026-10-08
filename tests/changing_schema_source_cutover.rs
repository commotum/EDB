use edb_core::{
    Cardinality, Database, DatabaseValue, EntityRef, EntityValue, IndexOrder, Keyword, Value,
};

fn keyword(name: &str) -> Keyword {
    let (namespace, name) = name.split_once('/').unwrap();
    Keyword::new(namespace, name)
}

fn attribute(db: &DatabaseValue, name: &str) -> u32 {
    db.entid(&keyword(name)).unwrap() as u32
}

fn sources(db: &DatabaseValue, entity: u64, attr: u32) -> Vec<u64> {
    let mut result = db
        .datoms(IndexOrder::Eavt)
        .unwrap()
        .into_iter()
        .filter(|d| d.entity == entity && d.attribute == attr)
        .map(|d| d.source)
        .collect::<Vec<_>>();
    result.sort_unstable();
    result
}

fn initial() -> DatabaseValue {
    Database::bootstrap().unwrap().with_edn(
        r#"[{:db/ident :person/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
            {:db/ident :person/food :db/valueType :db.type/string :db/cardinality :db.cardinality/many}
            {:db/ident :person/email :db/valueType :db.type/string :db/cardinality :db.cardinality/one :db/unique :db.unique/identity}
            {:db/ident :order/children :db/valueType :db.type/ref :db/cardinality :db.cardinality/many}]"#,
        1, EntityRef::Id(99),
    ).unwrap().db_after.database_value()
}

#[test]
fn rename_and_alias_repurpose_preserve_captured_names_and_application_datoms() {
    let installed = initial();
    let name = attribute(&installed, "person/name");
    let first = installed
        .with_edn(
            r#"[{:db/id "person" :person/name "Ada"}]"#,
            2,
            EntityRef::Id(99),
        )
        .unwrap();
    let entity = first.tempids["person"];
    let shared = first
        .db_after
        .with_edn(
            &format!("[{{:db/id {entity} :person/name \"Ada\"}}]"),
            3,
            EntityRef::Id(98),
        )
        .unwrap();
    let before = shared.db_after;
    let renamed = before
        .with_edn(
            "[{:db/id :person/name :db/ident :person/full-name}]",
            4,
            EntityRef::Id(97),
        )
        .unwrap();
    assert!(renamed.tx_data.iter().all(|d| d.source == 97));
    assert!(!renamed.tx_data.iter().any(|d| d.attribute == name));
    for name_alias in ["person/name", "person/full-name"] {
        assert_eq!(
            renamed.db_after.entid(&keyword(name_alias)),
            Some(u64::from(name))
        );
    }
    assert_eq!(
        renamed.db_after.ident(name.into()),
        Some(&keyword("person/full-name"))
    );
    assert_eq!(
        renamed.db_after.values(entity, name).unwrap(),
        vec![Value::String("Ada".into())]
    );
    assert_eq!(sources(&renamed.db_after, entity, name), vec![98, 99]);
    assert_eq!(before.ident(name.into()), Some(&keyword("person/name")));
    assert_eq!(before.entid(&keyword("person/full-name")), None);

    let repurposed = renamed
        .db_after
        .with_edn(
            "[{:db/id :person/food :db/ident :person/name}]",
            5,
            EntityRef::Id(96),
        )
        .unwrap();
    let food = attribute(&installed, "person/food");
    assert_eq!(
        repurposed.db_after.entid(&keyword("person/name")),
        Some(food.into())
    );
    assert_eq!(
        repurposed.db_after.entid(&keyword("person/full-name")),
        Some(name.into())
    );
    assert_eq!(
        renamed.db_after.entid(&keyword("person/name")),
        Some(name.into())
    );
    assert_eq!(sources(&repurposed.db_after, entity, name), vec![98, 99]);
}

#[test]
fn cardinality_cleanup_is_atomic_and_temporal_entity_shapes_use_captured_schema() {
    let installed = initial();
    let food = attribute(&installed, "person/food");
    let first = installed
        .with_edn(
            r#"[{:db/id "person" :person/food ["peach" "pear"]}]"#,
            2,
            EntityRef::Id(99),
        )
        .unwrap();
    let entity = first.tempids["person"];
    let shared = first
        .db_after
        .with_edn(
            &format!("[{{:db/id {entity} :person/food [\"peach\" \"pear\"]}}]"),
            3,
            EntityRef::Id(98),
        )
        .unwrap();
    let before = shared.db_after;
    let invalid = format!(
        "[{{:db/id :person/food :db/cardinality :db.cardinality/one}} [:db/retract {entity} :person/food \"absent\"]]"
    );
    assert_eq!(
        before
            .with_edn(&invalid, 4, EntityRef::Id(97))
            .unwrap_err()
            .code,
        "schema/cardinality-change-conflict"
    );
    assert_eq!(
        before.schema().attribute(food).unwrap().cardinality,
        Cardinality::Many
    );
    assert_eq!(before.values(entity, food).unwrap().len(), 2);
    assert_eq!(sources(&before, entity, food), vec![98, 98, 99, 99]);
    let cleaned = before.with_edn(
        &format!("[{{:db/id :person/food :db/cardinality :db.cardinality/one}} [:db/retract {entity} :person/food \"pear\"]]"),
        4, EntityRef::Id(97),
    ).unwrap();
    assert!(cleaned.tx_data.iter().all(|d| d.source == 97));
    assert_eq!(sources(&cleaned.db_after, entity, food), vec![98, 99]);
    let as_of = cleaned.db_after.clone().as_of(before.basis_t());
    let since = cleaned
        .db_after
        .clone()
        .since(installed.basis_t())
        .as_of(before.basis_t());
    for temporal in [&as_of, &since] {
        assert_eq!(
            temporal.schema().attribute(food).unwrap().cardinality,
            Cardinality::One
        );
        assert_eq!(temporal.values(entity, food).unwrap().len(), 2);
        assert!(matches!(
            temporal.entity(entity).unwrap().unwrap().get(food).unwrap(),
            Some(EntityValue::Scalar(Value::String(_)))
        ));
    }
    assert_eq!(
        cleaned
            .db_after
            .clone()
            .history()
            .schema()
            .attribute(food)
            .unwrap()
            .cardinality,
        Cardinality::One
    );
    assert!(
        matches!(before.entity(entity).unwrap().unwrap().get(food).unwrap(),
        Some(EntityValue::Collection(values)) if values.len() == 2)
    );
}

#[test]
fn cardinality_and_unique_changes_use_old_replacement_and_upsert_rules() {
    let installed = initial();
    let name = attribute(&installed, "person/name");
    let email = attribute(&installed, "person/email");
    let first = installed
        .with_edn(
            r#"[{:db/id "person" :person/name "Ada" :person/email "ada@example.com"}]"#,
            2,
            EntityRef::Id(99),
        )
        .unwrap();
    let entity = first.tempids["person"];
    let shared = first
        .db_after
        .with_edn(
            &format!("[{{:db/id {entity} :person/name \"Ada\"}}]"),
            3,
            EntityRef::Id(98),
        )
        .unwrap();
    let changed = shared.db_after.with_edn(
        &format!("[{{:db/id :person/name :db/cardinality :db.cardinality/many}} {{:db/id {entity} :person/name \"Grace\"}} [:db/retract :person/email :db/unique :db.unique/identity] {{:db/id \"upsert\" :person/email \"ada@example.com\"}}]"),
        4, EntityRef::Id(97),
    ).unwrap();
    assert_eq!(changed.tempids["upsert"], entity);
    assert!(changed.tx_data.iter().all(|d| d.source == 97));
    assert_eq!(
        changed
            .tx_data
            .iter()
            .filter(|d| d.attribute == name && !d.added)
            .count(),
        1
    );
    assert_eq!(
        changed.db_after.values(entity, name).unwrap(),
        vec![Value::String("Grace".into())]
    );
    assert_eq!(sources(&changed.db_after, entity, name), vec![97]);
    assert!(
        changed
            .db_after
            .schema()
            .attribute(email)
            .unwrap()
            .unique
            .is_none()
    );
    let later = changed.db_after.with_edn(
        &format!("[{{:db/id {entity} :person/name \"Ada\"}} {{:db/id \"separate\" :person/email \"ada@example.com\"}}]"),
        5, EntityRef::Id(96),
    ).unwrap();
    assert_ne!(later.tempids["separate"], entity);
    assert_eq!(later.db_after.values(entity, name).unwrap().len(), 2);
    assert_eq!(
        shared.db_after.values(entity, name).unwrap(),
        vec![Value::String("Ada".into())]
    );
}

#[test]
fn component_changes_expand_retract_entity_under_db_before_and_share_source() {
    let installed = initial();
    let name = attribute(&installed, "person/name");
    let children = attribute(&installed, "order/children");
    let first = installed
        .with_edn(
            r#"[{:db/id "parent" :person/name "Parent" :order/children ["child"]}
            {:db/id "child" :person/name "Child"}]"#,
            2,
            EntityRef::Id(99),
        )
        .unwrap();
    let parent = first.tempids["parent"];
    let child = first.tempids["child"];
    let shared = first.db_after.with_edn(
        &format!("[{{:db/id {parent} :order/children [{child}]}} {{:db/id {child} :person/name \"Child\"}}]"),
        3, EntityRef::Id(98),
    ).unwrap();
    let enabled = shared
        .db_after
        .with_edn(
            &format!(
                "[{{:db/id :order/children :db/isComponent true}} [:db/retractEntity {parent}]]"
            ),
            4,
            EntityRef::Id(97),
        )
        .unwrap();
    assert!(enabled.tx_data.iter().all(|d| d.source == 97));
    assert!(
        enabled
            .db_after
            .values(parent, children)
            .unwrap()
            .is_empty()
    );
    assert_eq!(sources(&enabled.db_after, child, name), vec![98, 99]);
    let relinked = enabled
        .db_after
        .with_edn(
            &format!("[{{:db/id {parent} :order/children [{child}]}}]"),
            5,
            EntityRef::Id(96),
        )
        .unwrap();
    let disabled = relinked
        .db_after
        .with_edn(
            &format!(
                "[{{:db/id :order/children :db/isComponent false}} [:db.fn/retractEntity {parent}]]"
            ),
            6,
            EntityRef::Id(95),
        )
        .unwrap();
    assert!(disabled.tx_data.iter().all(|d| d.source == 95));
    assert!(disabled.db_after.values(child, name).unwrap().is_empty());
    assert_eq!(
        disabled
            .tx_data
            .iter()
            .filter(|d| d.entity == child && d.attribute == name && !d.added)
            .count(),
        1
    );
    assert_eq!(sources(&shared.db_after, child, name), vec![98, 99]);
}
