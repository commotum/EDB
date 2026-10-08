use edb_core::{
    Attribute, Cardinality, Database, EntityRef, Keyword, Schema, TupleSpec, TxOp, TxValue, Unique,
    Value, ValueType,
};

const NAME: u32 = 100;
const LINK: u32 = 101;
const PAIR: u32 = 102;

fn add(entity: EntityRef, attribute: u32, value: impl Into<TxValue>) -> TxOp {
    TxOp::Add {
        entity,
        attribute,
        value: value.into(),
    }
}

fn base() -> Database {
    let mut schema = Schema::new();
    let mut pair = Attribute::new(
        PAIR,
        Keyword::new("identity", "pair"),
        ValueType::Tuple,
        Cardinality::One,
    )
    .unique(Unique::Identity);
    pair.tuple = Some(TupleSpec::Heterogeneous(vec![
        ValueType::Ref,
        ValueType::String,
    ]));
    for attribute in [
        Attribute::new(
            NAME,
            Keyword::new("identity", "name"),
            ValueType::String,
            Cardinality::One,
        )
        .unique(Unique::Identity),
        Attribute::new(
            LINK,
            Keyword::new("identity", "link"),
            ValueType::Ref,
            Cardinality::One,
        )
        .unique(Unique::Identity),
        pair,
    ] {
        schema.install(attribute).unwrap();
    }
    Database::new(schema)
        .unwrap()
        .with(
            &[
                add(EntityRef::Id(500), NAME, Value::String("owner".into())),
                add(EntityRef::Id(501), LINK, Value::Ref(500)),
                add(
                    EntityRef::Id(502),
                    PAIR,
                    Value::Tuple(vec![
                        Some(Value::Ref(500)),
                        Some(Value::String("key".into())),
                    ]),
                ),
            ],
            1,
            EntityRef::Id(99),
        )
        .unwrap()
        .db_after
}

#[test]
fn reference_identity_uses_upserted_reference_tempid_and_resolves_source() {
    let base = base();
    let ops = [
        add(
            EntityRef::Temp("owner".into()),
            NAME,
            Value::String("owner".into()),
        ),
        add(
            EntityRef::Temp("record".into()),
            LINK,
            TxValue::Entity(EntityRef::Temp("owner".into())),
        ),
    ];
    let report = base
        .with(&ops, 2, EntityRef::Temp("record".into()))
        .unwrap();
    assert_eq!(report.tempids["owner"], 500);
    assert_eq!(report.tempids["record"], 501);
    assert!(report.tx_data.iter().all(|d| d.source == 501));
    assert_eq!(report.db_after.values(501, LINK), vec![&Value::Ref(500)]);
}

#[test]
fn tuple_identity_uses_upserted_reference_tempid_and_resolves_source() {
    let base = base();
    let ops = [
        add(
            EntityRef::Temp("owner".into()),
            NAME,
            Value::String("owner".into()),
        ),
        add(
            EntityRef::Temp("record".into()),
            PAIR,
            TxValue::Tuple(vec![
                Some(TxValue::Entity(EntityRef::Temp("owner".into()))),
                Some(Value::String("key".into()).into()),
            ]),
        ),
    ];
    let report = base
        .with(&ops, 2, EntityRef::Temp("record".into()))
        .unwrap();
    assert_eq!(report.tempids["owner"], 500);
    assert_eq!(report.tempids["record"], 502);
    assert!(report.tx_data.iter().all(|d| d.source == 502));
}

#[test]
fn chained_reference_and_tuple_identities_are_order_independent_and_lookup_once() {
    use edb_core::{OperationContext, OperationKind};
    const DEPTH: usize = 64;
    let mut existing = Vec::new();
    let mut claims = vec![add(
        EntityRef::Temp("z-owner".into()),
        NAME,
        Value::String("owner".into()),
    )];
    claims.push(add(
        EntityRef::Temp("z-record".into()),
        LINK,
        TxValue::Entity(EntityRef::Temp("z-owner".into())),
    ));
    let mut prior = 501;
    let mut prior_name = "z-record".to_string();
    for index in 0..DEPTH {
        let entity = 600 + index as u64;
        // The dependent name precedes its dependency, forcing delayed wakeups.
        let name = format!("node-{:03}", DEPTH - index);
        let (attribute, stored, input) = if index % 2 == 0 {
            (
                LINK,
                Value::Ref(prior),
                TxValue::Entity(EntityRef::Temp(prior_name)),
            )
        } else {
            (
                PAIR,
                Value::Tuple(vec![
                    Some(Value::Ref(prior)),
                    Some(Value::String("chain".into())),
                ]),
                TxValue::Tuple(vec![
                    Some(TxValue::Entity(EntityRef::Temp(prior_name))),
                    Some(Value::String("chain".into()).into()),
                ]),
            )
        };
        existing.push(add(EntityRef::Id(entity), attribute, stored));
        claims.push(add(EntityRef::Temp(name.clone()), attribute, input));
        prior = entity;
        prior_name = name;
    }
    let base = base()
        .with(&existing, 2, EntityRef::Id(99))
        .unwrap()
        .db_after;
    for reverse in [false, true] {
        let mut ops = claims.clone();
        if reverse {
            ops.reverse();
        }
        for eager in [false, true] {
            let context = OperationContext::diagnostic(OperationKind::Transaction);
            let scope = context.enter();
            let (tempids, datoms) = if eager {
                let report = base
                    .with(&ops, 3, EntityRef::Temp(prior_name.clone()))
                    .unwrap();
                (report.tempids, report.tx_data)
            } else {
                let report = base
                    .database_value()
                    .with(&ops, 3, EntityRef::Temp(prior_name.clone()))
                    .unwrap();
                (report.tempids, report.tx_data)
            };
            drop(scope);
            assert_eq!(tempids["z-owner"], 500);
            assert_eq!(tempids["z-record"], 501);
            for index in 0..DEPTH {
                assert_eq!(
                    tempids[&format!("node-{:03}", DEPTH - index)],
                    600 + index as u64
                );
            }
            assert!(datoms.iter().all(|d| d.source == prior));
            let stats = context.report().stats.transaction;
            assert_eq!(stats.identity_lookups, (DEPTH + 2) as u64);
            assert_eq!(stats.upsert_resolutions, (DEPTH + 2) as u64);
            // One initial consideration plus at most one binding wakeup per claim.
            assert!(stats.identity_claims <= (2 * DEPTH + 3) as u64);
        }
    }
}

#[test]
fn new_only_reference_and_tuple_aliases_unify_before_allocation() {
    for attribute in [LINK, PAIR] {
        let owner = |name: &str| {
            add(
                EntityRef::Temp(name.into()),
                NAME,
                Value::String("new-owner".into()),
            )
        };
        let record = |name: &str, target: &str| {
            let reference = TxValue::Entity(EntityRef::Temp(target.into()));
            let value = if attribute == LINK {
                reference
            } else {
                TxValue::Tuple(vec![
                    Some(reference),
                    Some(Value::String("new-key".into()).into()),
                ])
            };
            add(EntityRef::Temp(name.into()), attribute, value)
        };
        let ops = [
            owner("left-owner"),
            owner("right-owner"),
            record("left-record", "left-owner"),
            record("right-record", "right-owner"),
        ];
        let base = base();
        for reverse in [false, true] {
            let mut ops = ops.to_vec();
            if reverse {
                ops.reverse();
            }
            let report = base
                .database_value()
                .with(&ops, 2, EntityRef::Temp("right-record".into()))
                .unwrap();
            assert_eq!(report.tempids["left-owner"], report.tempids["right-owner"]);
            assert_eq!(
                report.tempids["left-record"],
                report.tempids["right-record"]
            );
            let entity = report.tempids["right-record"];
            assert_ne!(entity, report.tempids["left-owner"]);
            assert!(report.tx_data.iter().all(|d| d.source == entity));
            assert_eq!(report.db_after.values(entity, attribute).unwrap().len(), 1);
            let eager = base
                .with(&ops, 2, EntityRef::Temp("right-record".into()))
                .unwrap();
            assert_eq!(eager.tempids, report.tempids);
        }
    }
}

#[test]
fn late_identity_unification_propagates_existing_bindings_to_dependents() {
    let ops = [
        add(
            EntityRef::Temp("owner-a".into()),
            NAME,
            Value::String("owner".into()),
        ),
        add(
            EntityRef::Temp("owner-b".into()),
            NAME,
            Value::String("owner".into()),
        ),
        add(
            EntityRef::Temp("left".into()),
            PAIR,
            TxValue::Tuple(vec![
                Some(TxValue::Entity(EntityRef::Temp("owner-a".into()))),
                Some(Value::String("new-key".into()).into()),
            ]),
        ),
        add(
            EntityRef::Temp("right".into()),
            PAIR,
            TxValue::Tuple(vec![
                Some(TxValue::Entity(EntityRef::Temp("owner-b".into()))),
                Some(Value::String("new-key".into()).into()),
            ]),
        ),
        add(
            EntityRef::Temp("right".into()),
            NAME,
            Value::String("owner".into()),
        ),
        add(
            EntityRef::Temp("child".into()),
            LINK,
            TxValue::Entity(EntityRef::Temp("left".into())),
        ),
    ];
    let report = base()
        .with(&ops, 2, EntityRef::Temp("child".into()))
        .unwrap();
    assert_eq!(report.tempids["left"], 500);
    assert_eq!(report.tempids["right"], 500);
    assert_eq!(report.tempids["child"], 501);
    assert!(report.tx_data.iter().all(|d| d.source == 501));
}

#[test]
fn new_only_cycles_allocate_deterministically_without_identity_lookups() {
    use edb_core::{OperationContext, OperationKind};
    let ops = [
        add(
            EntityRef::Temp("a".into()),
            LINK,
            TxValue::Entity(EntityRef::Temp("b".into())),
        ),
        add(
            EntityRef::Temp("b".into()),
            LINK,
            TxValue::Entity(EntityRef::Temp("a".into())),
        ),
    ];
    let base = base();
    let context = OperationContext::diagnostic(OperationKind::Transaction);
    let scope = context.enter();
    let report = base.with(&ops, 2, EntityRef::Temp("b".into())).unwrap();
    drop(scope);
    let a = report.tempids["a"];
    let b = report.tempids["b"];
    assert_ne!(a, b);
    assert!(report.tx_data.iter().all(|d| d.source == b));
    assert_eq!(report.db_after.values(a, LINK), vec![&Value::Ref(b)]);
    assert_eq!(report.db_after.values(b, LINK), vec![&Value::Ref(a)]);
    assert_eq!(context.report().stats.transaction.identity_lookups, 0);
    let reverse = base
        .database_value()
        .with(
            &[ops[1].clone(), ops[0].clone()],
            2,
            EntityRef::Temp("b".into()),
        )
        .unwrap();
    assert_eq!(report.tempids, reverse.tempids);
}

#[test]
fn conflicting_dependent_identities_and_unique_values_fail_without_changing_basis() {
    let base = base().database_value();
    let ops = [
        add(
            EntityRef::Temp("owner".into()),
            NAME,
            Value::String("owner".into()),
        ),
        add(
            EntityRef::Temp("record".into()),
            LINK,
            TxValue::Entity(EntityRef::Temp("owner".into())),
        ),
        add(
            EntityRef::Temp("record".into()),
            PAIR,
            TxValue::Tuple(vec![
                Some(TxValue::Entity(EntityRef::Temp("owner".into()))),
                Some(Value::String("key".into()).into()),
            ]),
        ),
    ];
    assert_eq!(
        base.with(&ops, 2, EntityRef::Temp("record".into()))
            .unwrap_err()
            .code,
        "transaction/upsert-conflict"
    );
    assert_eq!(base.lookup(LINK, &Value::Ref(500)).unwrap(), Some(501));
    assert_eq!(base.values(502, PAIR).unwrap().len(), 1);
    let attribute = Attribute::new(
        103,
        Keyword::new("identity", "exclusive"),
        ValueType::Ref,
        Cardinality::One,
    )
    .unique(Unique::Value);
    let installed = base
        .with(&[TxOp::InstallAttribute(attribute)], 2, EntityRef::Id(99))
        .unwrap()
        .db_after;
    let held = installed
        .with(
            &[add(EntityRef::Id(550), 103, Value::Ref(500))],
            3,
            EntityRef::Id(99),
        )
        .unwrap()
        .db_after;
    let claim = [
        ops[0].clone(),
        add(
            EntityRef::Temp("record".into()),
            103,
            TxValue::Entity(EntityRef::Temp("owner".into())),
        ),
    ];
    assert_eq!(
        held.with(&claim, 4, EntityRef::Temp("record".into()))
            .unwrap_err()
            .code,
        "transaction/unique-value-conflict"
    );
    assert_eq!(held.lookup(103, &Value::Ref(500)).unwrap(), Some(550));
}

#[test]
fn lookup_refs_still_cannot_resolve_keys_created_in_the_same_transaction() {
    let base = base().database_value();
    let ops = [add(
        EntityRef::Temp("fresh".into()),
        NAME,
        Value::String("fresh".into()),
    )];
    let source = EntityRef::Lookup {
        attribute: NAME,
        value: Value::String("fresh".into()),
    };
    assert_eq!(
        base.with(&ops, 2, source).unwrap_err().code,
        "transaction/lookup-not-found"
    );
    assert_eq!(
        base.lookup(NAME, &Value::String("fresh".into())).unwrap(),
        None
    );
    let report = base.with(&ops, 2, EntityRef::Temp("fresh".into())).unwrap();
    assert!(
        report
            .tx_data
            .iter()
            .all(|d| d.source == report.tempids["fresh"])
    );
    let withdrawn = base
        .with(
            &[TxOp::Retract {
                entity: EntityRef::Id(500),
                attribute: NAME,
                value: Some(Value::String("owner".into()).into()),
            }],
            2,
            EntityRef::Id(99),
        )
        .unwrap()
        .db_after;
    // Ordinary domain keys, unlike resident ident aliases, disappear on withdrawal.
    assert_eq!(
        withdrawn
            .lookup(NAME, &Value::String("owner".into()))
            .unwrap(),
        None
    );
    assert_eq!(
        withdrawn
            .with(
                &[],
                3,
                EntityRef::Lookup {
                    attribute: NAME,
                    value: Value::String("owner".into()),
                }
            )
            .unwrap_err()
            .code,
        "transaction/lookup-not-found"
    );
}

#[test]
fn ident_lookup_sources_follow_the_captured_alias_dictionary() {
    use edb_core::{DB_IDENT, OperationContext, OperationKind, SpeculationLimits};
    let first = Database::bootstrap()
        .unwrap()
        .with_edn(
            "[{:db/id \"contributor\" :db/ident :contributor/original}]",
            1,
            EntityRef::Id(99),
        )
        .unwrap();
    let entity = first.tempids["contributor"];
    let renamed = first
        .db_after
        .with_edn(
            &format!("[{{:db/id {entity} :db/ident :contributor/renamed}}]"),
            2,
            EntityRef::Id(98),
        )
        .unwrap()
        .db_after
        .database_value();
    let old = Keyword::new("contributor", "original");
    assert_eq!(renamed.entid(&old), Some(entity));
    assert_eq!(
        renamed
            .lookup(DB_IDENT as u32, &Value::Keyword(old.clone()))
            .unwrap(),
        Some(entity)
    );
    let sources = [
        EntityRef::Ident(old.clone()),
        EntityRef::Lookup {
            attribute: DB_IDENT as u32,
            value: Value::Keyword(old.clone()),
        },
        EntityRef::from_edn("[:db/ident :contributor/original]").unwrap(),
    ];
    let withdrawn = renamed
        .with_edn(
            &format!("[[:db/retract {entity} :db/ident :contributor/renamed]]"),
            3,
            EntityRef::Id(98),
        )
        .unwrap()
        .db_after;
    assert!(
        withdrawn
            .values(entity, DB_IDENT as u32)
            .unwrap()
            .is_empty()
    );
    // The existing dictionary intentionally retains every historical alias;
    // explicit withdrawal removes the fact, not the resident lookup name.
    for db in [renamed, withdrawn] {
        assert_eq!(db.entid(&old), Some(entity));
        for source in sources.clone() {
            let expected_lookups = u64::from(!matches!(source, EntityRef::Ident(_)));
            let context = OperationContext::diagnostic(OperationKind::Transaction);
            let (result, work) = context.measure(|| {
                db.with_forms_with_limits(
                    &[],
                    4,
                    source,
                    SpeculationLimits {
                        max_read_datoms: 1,
                        max_read_bytes: 1,
                        ..Default::default()
                    },
                )
            });
            let report = result.unwrap();
            assert_eq!(report.tx_data.len(), 1);
            assert_eq!(report.tx_data[0].source, entity);
            assert_eq!(work.stats.transaction.identity_lookups, expected_lookups);
        }
        assert_eq!(
            db.with(
                &[],
                4,
                EntityRef::Lookup {
                    attribute: DB_IDENT as u32,
                    value: Value::Keyword(Keyword::new("contributor", "unknown")),
                }
            )
            .unwrap_err()
            .code,
            "transaction/lookup-not-found"
        );
    }
}
