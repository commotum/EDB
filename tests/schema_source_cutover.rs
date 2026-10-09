use edb_core::{
    Attribute, Cardinality, Database, DatabaseValue, EntityRef, IndexOrder, Keyword, Schema,
    TupleSpec, TxForm, TxOp, Unique, Value, ValueType, View,
};

const LEFT: u32 = 500;
const RIGHT: u32 = 501;
const PAIR: u32 = 502;
const ENTITY: u64 = 400;

fn add(entity: u64, attribute: u32, value: Value) -> TxOp {
    TxOp::Add {
        entity: EntityRef::Id(entity),
        attribute,
        value: value.into(),
    }
}

fn retract(entity: u64, attribute: u32) -> TxOp {
    TxOp::Retract {
        entity: EntityRef::Id(entity),
        attribute,
        value: None,
    }
}

fn scalar(id: u32, name: &str, cardinality: Cardinality) -> Attribute {
    Attribute::new(
        id,
        Keyword::new("schema-test", name),
        ValueType::String,
        cardinality,
    )
}

fn pair() -> Attribute {
    Attribute::new(
        PAIR,
        Keyword::new("schema-test", "pair"),
        ValueType::Tuple,
        Cardinality::One,
    )
    .tuple(TupleSpec::Composite(vec![LEFT, RIGHT]))
    .unique(Unique::Identity)
}

fn database(composite: bool) -> Database {
    let mut schema = Schema::new();
    schema
        .install(scalar(LEFT, "left", Cardinality::One))
        .unwrap();
    schema
        .install(scalar(RIGHT, "right", Cardinality::One))
        .unwrap();
    if composite {
        schema.install(pair()).unwrap();
    }
    Database::new(schema).unwrap()
}

fn forms(ops: Vec<TxOp>) -> Vec<TxForm> {
    ops.into_iter().map(TxForm::Op).collect()
}

fn live_sources(database: &DatabaseValue, entity: u64, attribute: u32) -> Vec<u64> {
    let mut sources = database
        .datoms(IndexOrder::Eavt)
        .unwrap()
        .into_iter()
        .filter(|d| d.entity == entity && d.attribute == attribute)
        .map(|d| d.source)
        .collect::<Vec<_>>();
    sources.sort_unstable();
    sources
}

#[test]
fn unchanged_composite_contribution_is_material_only_for_a_new_source() {
    let first = database(true)
        .with(
            &[
                add(ENTITY, LEFT, Value::String("left".into())),
                add(ENTITY, RIGHT, Value::String("right".into())),
            ],
            10,
            EntityRef::Id(99),
        )
        .unwrap();
    let second = first
        .db_after
        .with(
            &[add(ENTITY, LEFT, Value::String("left".into()))],
            11,
            EntityRef::Id(98),
        )
        .unwrap();
    assert!(
        second
            .tx_data
            .iter()
            .any(|d| d.entity == ENTITY && d.attribute == PAIR && d.added)
    );
    assert!(second.tx_data.iter().all(|d| d.source == 98));
    assert_eq!(
        live_sources(&second.db_after.database_value(), ENTITY, PAIR),
        vec![98, 99]
    );
    assert_eq!(second.db_after.values(ENTITY, PAIR).len(), 1);
    let repeated = second
        .db_after
        .with(
            &[add(ENTITY, LEFT, Value::String("left".into()))],
            12,
            EntityRef::Id(98),
        )
        .unwrap();
    assert_eq!(repeated.tx_data.len(), 1);
    let replaced = repeated
        .db_after
        .with(
            &[add(ENTITY, LEFT, Value::String("replacement".into()))],
            13,
            EntityRef::Id(97),
        )
        .unwrap();
    assert!(replaced.tx_data.iter().all(|d| d.source == 97));
    assert_eq!(
        replaced
            .tx_data
            .iter()
            .filter(|d| d.attribute == PAIR && !d.added)
            .count(),
        1
    );
    assert_eq!(
        live_sources(&replaced.db_after.database_value(), ENTITY, PAIR),
        vec![97]
    );
    let cleared = replaced
        .db_after
        .with(
            &[retract(ENTITY, LEFT), retract(ENTITY, RIGHT)],
            14,
            EntityRef::Id(96),
        )
        .unwrap();
    assert!(cleared.db_after.values(ENTITY, PAIR).is_empty());
    assert!(
        cleared
            .tx_data
            .iter()
            .any(|d| d.attribute == PAIR && !d.added && d.source == 96)
    );
    cleared
        .db_after
        .rebuild_derived_caches()
        .unwrap()
        .validate_invariants()
        .unwrap();
}

#[test]
fn changed_schema_property_preserves_overlapping_source_contributions() {
    let attribute = scalar(LEFT, "left", Cardinality::One).predicate("schema-test/first?");
    let installed = Database::bootstrap()
        .unwrap()
        .with(
            &[TxOp::InstallAttribute(attribute.clone())],
            10,
            EntityRef::Id(99),
        )
        .unwrap();
    assert!(installed.tx_data.iter().all(|d| d.source == 99));
    let changed = installed
        .db_after
        .with(
            &[TxOp::AlterAttribute(
                attribute.predicate("schema-test/second?"),
            )],
            11,
            EntityRef::Id(98),
        )
        .unwrap();
    let predicate_datoms = changed
        .tx_data
        .iter()
        .filter(|d| d.entity == u64::from(LEFT) && d.attribute == edb_core::DB_ATTR_PREDS as u32)
        .collect::<Vec<_>>();
    assert_eq!(predicate_datoms.len(), 2);
    assert!(predicate_datoms.iter().all(|d| d.added && d.source == 98));
    assert_eq!(
        live_sources(
            &changed.db_after.database_value(),
            LEFT.into(),
            edb_core::DB_ATTR_PREDS as u32
        ),
        vec![98, 98, 99]
    );
    changed
        .db_after
        .rebuild_derived_caches()
        .unwrap()
        .validate_invariants()
        .unwrap();
    // Source is a coordinate. Native vocabulary has no implicit source property.
    assert!(
        changed
            .db_after
            .entid(&Keyword::new("db", "source"))
            .is_none()
    );
    assert!(
        changed
            .db_after
            .datoms(View::Current, IndexOrder::Eavt)
            .iter()
            .any(|d| d.source == 99)
    );
}

#[test]
fn composite_backfill_is_paced_and_discontinuation_is_permanent() {
    let old = database(false)
        .with(
            &[add(ENTITY, LEFT, Value::String("old".into()))],
            10,
            EntityRef::Id(99),
        )
        .unwrap();
    let installed = old
        .db_after
        .with(&[TxOp::InstallAttribute(pair())], 11, EntityRef::Id(98))
        .unwrap();
    assert!(installed.db_after.values(ENTITY, PAIR).is_empty());
    let backfill = installed
        .db_after
        .with(
            &[add(ENTITY, LEFT, Value::String("old".into()))],
            12,
            EntityRef::Id(99),
        )
        .unwrap();
    assert!(
        !backfill
            .tx_data
            .iter()
            .any(|d| d.entity == ENTITY && d.attribute == LEFT)
    );
    assert!(
        backfill
            .tx_data
            .iter()
            .any(|d| d.attribute == PAIR && d.source == 99 && d.added)
    );
    let expected = Value::Tuple(vec![Some(Value::String("old".into())), None]);
    assert_eq!(backfill.db_after.values(ENTITY, PAIR), vec![&expected]);
    let mut discontinued = pair();
    discontinued.tuple_discontinued = true;
    let stopped = backfill
        .db_after
        .with(
            &[TxOp::AlterAttribute(discontinued.clone())],
            13,
            EntityRef::Id(98),
        )
        .unwrap();
    let later = stopped
        .db_after
        .with(
            &[add(ENTITY, LEFT, Value::String("later".into()))],
            14,
            EntityRef::Id(97),
        )
        .unwrap();
    assert_eq!(later.db_after.values(ENTITY, PAIR), vec![&expected]);
    assert!(!later.tx_data.iter().any(|d| d.attribute == PAIR));
    let direct = later
        .db_after
        .with(
            &[add(
                ENTITY,
                PAIR,
                Value::Tuple(vec![Some(Value::String("manual".into())), None]),
            )],
            15,
            EntityRef::Id(96),
        )
        .unwrap();
    assert_eq!(direct.db_after.values(ENTITY, PAIR), vec![&expected]);
    assert!(!direct.tx_data.iter().any(|d| d.attribute == PAIR));
    let direct_retraction = direct
        .db_after
        .with(&[retract(ENTITY, PAIR)], 16, EntityRef::Id(95))
        .unwrap();
    assert_eq!(
        direct_retraction.db_after.values(ENTITY, PAIR),
        vec![&expected]
    );
    assert!(
        !direct_retraction
            .tx_data
            .iter()
            .any(|d| d.attribute == PAIR)
    );
    assert_eq!(
        later
            .db_after
            .with(&[TxOp::AlterAttribute(pair())], 15, EntityRef::Id(96))
            .unwrap_err()
            .code,
        "schema/tuple-discontinuation-irreversible"
    );
    assert_eq!(
        database(false)
            .with(
                &[TxOp::InstallAttribute(discontinued)],
                10,
                EntityRef::Id(99)
            )
            .unwrap_err()
            .code,
        "schema/cannot-discontinue-at-install"
    );
    later
        .db_after
        .rebuild_derived_caches()
        .unwrap()
        .validate_invariants()
        .unwrap();
}

#[test]
fn many_to_one_and_uniqueness_count_values_and_holders_across_sources() {
    let mut attribute = scalar(LEFT, "many", Cardinality::Many);
    attribute.indexed = true;
    let mut schema = Schema::new();
    schema.install(attribute.clone()).unwrap();
    let first = Database::new(schema)
        .unwrap()
        .with(
            &[add(ENTITY, LEFT, Value::String("one".into()))],
            10,
            EntityRef::Id(99),
        )
        .unwrap();
    let shared = first
        .db_after
        .with(
            &[add(ENTITY, LEFT, Value::String("one".into()))],
            11,
            EntityRef::Id(98),
        )
        .unwrap();
    attribute.cardinality = Cardinality::One;
    attribute.unique = Some(Unique::Identity);
    let constrained = shared
        .db_after
        .with(
            &[TxOp::AlterAttribute(attribute.clone())],
            12,
            EntityRef::Id(97),
        )
        .unwrap();
    assert!(constrained.tx_data.iter().all(|d| d.source == 97));
    assert_eq!(
        live_sources(&constrained.db_after.database_value(), ENTITY, LEFT),
        vec![98, 99]
    );
    assert_eq!(
        constrained
            .db_after
            .with(
                &[add(401, LEFT, Value::String("one".into())),],
                13,
                EntityRef::Id(96)
            )
            .unwrap_err()
            .code,
        "transaction/unique-conflict"
    );
    let upsert = constrained
        .db_after
        .with(
            &[TxOp::Add {
                entity: EntityRef::Temp("upsert".into()),
                attribute: LEFT,
                value: Value::String("one".into()).into(),
            }],
            13,
            EntityRef::Id(96),
        )
        .unwrap();
    assert_eq!(upsert.tempids["upsert"], ENTITY);
    assert_eq!(
        live_sources(&upsert.db_after.database_value(), ENTITY, LEFT),
        vec![96, 98, 99]
    );
    let moved = upsert
        .db_after
        .with(
            &[
                retract(ENTITY, LEFT),
                add(401, LEFT, Value::String("one".into())),
            ],
            14,
            EntityRef::Id(95),
        )
        .unwrap();
    assert!(moved.db_after.values(ENTITY, LEFT).is_empty());
    assert_eq!(moved.db_after.values(401, LEFT).len(), 1);
    moved
        .db_after
        .rebuild_derived_caches()
        .unwrap()
        .validate_invariants()
        .unwrap();
}

#[test]
fn schema_transitions_can_retract_conflicts_but_cannot_rewrite_history() {
    let mut attribute = scalar(LEFT, "many", Cardinality::Many);
    attribute.indexed = true;
    let mut schema = Schema::new();
    schema.install(attribute.clone()).unwrap();
    let old = Database::new(schema)
        .unwrap()
        .with(
            &[
                add(ENTITY, LEFT, Value::String("one".into())),
                add(ENTITY, LEFT, Value::String("two".into())),
                add(401, LEFT, Value::String("one".into())),
            ],
            10,
            EntityRef::Id(99),
        )
        .unwrap();
    attribute.cardinality = Cardinality::One;
    assert_eq!(
        old.db_after
            .with(
                &[TxOp::AlterAttribute(attribute.clone())],
                11,
                EntityRef::Id(98)
            )
            .unwrap_err()
            .code,
        "schema/cardinality-change-conflict"
    );
    let one = old
        .db_after
        .with(
            &[
                TxOp::AlterAttribute(attribute.clone()),
                TxOp::Retract {
                    entity: EntityRef::Id(ENTITY),
                    attribute: LEFT,
                    value: Some(Value::String("two".into()).into()),
                },
            ],
            11,
            EntityRef::Id(98),
        )
        .unwrap();
    attribute.unique = Some(Unique::Value);
    assert_eq!(
        one.db_after
            .with(
                &[TxOp::AlterAttribute(attribute.clone())],
                12,
                EntityRef::Id(97)
            )
            .unwrap_err()
            .code,
        "schema/unique-change-conflict"
    );
    let unique = one
        .db_after
        .with(
            &[TxOp::AlterAttribute(attribute), retract(401, LEFT)],
            12,
            EntityRef::Id(97),
        )
        .unwrap();
    assert!(unique.tx_data.iter().all(|d| d.source == 97));
    assert_eq!(
        unique
            .db_after
            .datoms(View::AsOf(old.db_after.basis_t()), IndexOrder::Aevt)
            .iter()
            .filter(|d| d.attribute == LEFT)
            .count(),
        3
    );
    unique
        .db_after
        .rebuild_derived_caches()
        .unwrap()
        .validate_invariants()
        .unwrap();
}

#[test]
fn attribute_predicates_use_db_before_and_validate_new_source_assertions() {
    use edb_core::TxFunctions;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let old = database(false)
        .with(
            &[add(ENTITY, LEFT, Value::String("invalid".into()))],
            10,
            EntityRef::Id(99),
        )
        .unwrap();
    let guarded = old
        .db_after
        .with(
            &[
                add(
                    LEFT.into(),
                    edb_core::DB_ATTR_PREDS as u32,
                    Value::Symbol(edb_core::Symbol::new("schema-test", "valid?")),
                ),
                add(401, LEFT, Value::String("invalid".into())),
            ],
            11,
            EntityRef::Id(98),
        )
        .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    let mut functions = TxFunctions::new();
    functions.register_attribute_predicate("schema-test/valid?", move |value| {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(value == &Value::String("valid".into()))
    });
    let same = guarded
        .db_after
        .database_value()
        .with_functions(
            &forms(vec![add(ENTITY, LEFT, Value::String("invalid".into()))]),
            &functions,
            12,
            EntityRef::Id(99),
        )
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(same.tx_data.len(), 1);
    let error = same
        .db_after
        .with_functions(
            &forms(vec![add(ENTITY, LEFT, Value::String("invalid".into()))]),
            &functions,
            13,
            EntityRef::Id(97),
        )
        .unwrap_err();
    assert_eq!(error.code, "transaction/attribute-predicate");
    for detail in ["entity", "attribute", "value", "predicate", "pred_return"] {
        assert!(error.details.contains_key(detail));
    }
    let removed_and_added = same
        .db_after
        .with_functions(
            &forms(vec![
                retract(LEFT.into(), edb_core::DB_ATTR_PREDS as u32),
                add(402, LEFT, Value::String("invalid".into())),
            ]),
            &functions,
            13,
            EntityRef::Id(97),
        )
        .unwrap_err();
    assert_eq!(removed_and_added.code, "transaction/attribute-predicate");
    let withdrawn = same
        .db_after
        .with(&[retract(ENTITY, LEFT)], 13, EntityRef::Id(97))
        .unwrap();
    assert!(withdrawn.db_after.values(ENTITY, LEFT).unwrap().is_empty());
    assert_eq!(guarded.db_after.values(ENTITY, LEFT).len(), 1);
}

#[test]
fn entity_specs_check_distinct_values_and_managed_tuples_in_complete_db_after() {
    use edb_core::{Symbol, TxFunctions};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let installed = database(true)
        .with(
            &[
                add(
                    450,
                    edb_core::DB_ENTITY_ATTRS as u32,
                    Value::Keyword(Keyword::new("schema-test", "pair")),
                ),
                add(
                    450,
                    edb_core::DB_ENTITY_PREDS as u32,
                    Value::Symbol(Symbol::new("schema-test", "shape?")),
                ),
                add(ENTITY, LEFT, Value::String("left".into())),
            ],
            10,
            EntityRef::Id(99),
        )
        .unwrap();
    let shared_spec = installed
        .db_after
        .with(
            &[
                add(
                    450,
                    edb_core::DB_ENTITY_ATTRS as u32,
                    Value::Keyword(Keyword::new("schema-test", "pair")),
                ),
                add(
                    450,
                    edb_core::DB_ENTITY_PREDS as u32,
                    Value::Symbol(Symbol::new("schema-test", "shape?")),
                ),
                add(ENTITY, LEFT, Value::String("left".into())),
            ],
            11,
            EntityRef::Id(98),
        )
        .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    let mut functions = TxFunctions::new();
    functions.register_entity_predicate("schema-test/shape?", move |after, entity| {
        observed.fetch_add(1, Ordering::SeqCst);
        assert_eq!(after.values(entity, LEFT)?.len(), 1);
        assert_eq!(
            after.values(entity, PAIR)?,
            vec![Value::Tuple(vec![
                Some(Value::String("left".into())),
                Some(Value::String("right".into()))
            ])]
        );
        assert_eq!(live_sources(after, entity, PAIR), vec![97]);
        assert!(after.values(entity, edb_core::DB_ENSURE as u32)?.is_empty());
        Ok(true)
    });
    let ensured = shared_spec
        .db_after
        .database_value()
        .with_functions(
            &forms(vec![
                add(ENTITY, RIGHT, Value::String("right".into())),
                TxOp::Ensure {
                    entity: EntityRef::Id(ENTITY),
                    spec: EntityRef::Id(450),
                },
            ]),
            &functions,
            12,
            EntityRef::Id(97),
        )
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(ensured.tx_data.iter().all(|d| d.source == 97));
    assert!(
        !ensured
            .tx_data
            .iter()
            .any(|d| d.attribute == edb_core::DB_ENSURE as u32)
    );
    let error = ensured
        .db_after
        .with_functions(
            &forms(vec![
                retract(ENTITY, LEFT),
                retract(ENTITY, RIGHT),
                TxOp::Ensure {
                    entity: EntityRef::Id(ENTITY),
                    spec: EntityRef::Id(450),
                },
            ]),
            &functions,
            13,
            EntityRef::Id(96),
        )
        .unwrap_err();
    assert_eq!(error.code, "transaction/entity-spec");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let unchecked = ensured
        .db_after
        .with(
            &[retract(ENTITY, LEFT), retract(ENTITY, RIGHT)],
            13,
            EntityRef::Id(96),
        )
        .unwrap();
    assert!(unchecked.db_after.values(ENTITY, PAIR).unwrap().is_empty());
}

#[test]
fn entity_specs_use_db_before_and_reject_nonattribute_required_names() {
    let installed = database(false)
        .with(
            &[
                add(
                    450,
                    edb_core::DB_ENTITY_ATTRS as u32,
                    Value::Keyword(Keyword::new("schema-test", "left")),
                ),
                add(
                    451,
                    edb_core::DB_IDENT as u32,
                    Value::Keyword(Keyword::new("schema-test", "ordinary-entity")),
                ),
                add(ENTITY, LEFT, Value::String("left".into())),
            ],
            10,
            EntityRef::Id(99),
        )
        .unwrap();
    let changed = installed
        .db_after
        .with(
            &[
                add(
                    450,
                    edb_core::DB_ENTITY_ATTRS as u32,
                    Value::Keyword(Keyword::new("schema-test", "right")),
                ),
                TxOp::Ensure {
                    entity: EntityRef::Id(ENTITY),
                    spec: EntityRef::Id(450),
                },
            ],
            11,
            EntityRef::Id(98),
        )
        .unwrap();
    assert_eq!(
        changed
            .db_after
            .with(
                &[TxOp::Ensure {
                    entity: EntityRef::Id(ENTITY),
                    spec: EntityRef::Id(450)
                },],
                12,
                EntityRef::Id(97)
            )
            .unwrap_err()
            .code,
        "transaction/entity-spec"
    );
    let invalid_spec = changed
        .db_after
        .with(
            &[add(
                452,
                edb_core::DB_ENTITY_ATTRS as u32,
                Value::Keyword(Keyword::new("schema-test", "ordinary-entity")),
            )],
            12,
            EntityRef::Id(97),
        )
        .unwrap();
    assert_eq!(
        invalid_spec
            .db_after
            .with(
                &[TxOp::Ensure {
                    entity: EntityRef::Id(ENTITY),
                    spec: EntityRef::Id(452)
                },],
                13,
                EntityRef::Id(96)
            )
            .unwrap_err()
            .code,
        "transaction/unknown-spec-attribute"
    );
}

#[test]
fn native_predicates_require_literal_true_and_contain_missing_code_and_panics() {
    use edb_core::{
        NativeRegistry, RuntimeValue, SpeculationLimits, Symbol, TransactionExecutionOptions,
    };
    let mut attribute = scalar(LEFT, "left", Cardinality::One);
    attribute.predicates.push("schema-test/check?".into());
    let mut schema = Schema::new();
    schema.install(attribute).unwrap();
    let base = Database::new(schema).unwrap().database_value();
    let assertion = forms(vec![add(ENTITY, LEFT, Value::String("name".into()))]);
    let execute = |native| {
        base.with_forms_with_execution_options(
            &assertion,
            10,
            EntityRef::Id(99),
            SpeculationLimits::default(),
            &TransactionExecutionOptions {
                native,
                ..Default::default()
            },
        )
    };
    assert_eq!(
        execute(NativeRegistry::default()).unwrap_err().code,
        "native/deployment-not-found"
    );
    let mut diagnostic = NativeRegistry::builder();
    diagnostic
        .attribute_predicate(Symbol::new("schema-test", "check?"), |_, context| {
            context.check(1)?;
            Ok(RuntimeValue::Scalar(Value::String("reason".into())))
        })
        .unwrap();
    let error = execute(diagnostic.build()).unwrap_err();
    assert_eq!(error.code, "transaction/attribute-predicate");
    assert!(error.details["pred_return"].contains("reason"));
    let mut panic = NativeRegistry::builder();
    panic
        .attribute_predicate(Symbol::new("schema-test", "check?"), |_, _| {
            panic!("contained predicate panic")
        })
        .unwrap();
    assert_eq!(
        execute(panic.build()).unwrap_err().code,
        "native/callback-panic"
    );
    let mut accepted = NativeRegistry::builder();
    accepted
        .attribute_predicate(Symbol::new("schema-test", "check?"), |_, _| {
            Ok(RuntimeValue::Scalar(Value::Bool(true)))
        })
        .unwrap();
    let report = execute(accepted.build()).unwrap();
    assert!(report.tx_data.iter().all(|d| d.source == 99));
    assert!(base.values(ENTITY, LEFT).unwrap().is_empty());
}

#[test]
fn schema_definition_and_evolution_restrictions_remain_authoritative() {
    let base = database(false);
    let current = base.schema().attribute(LEFT).unwrap().clone();
    let mut changed_type = current.clone();
    changed_type.value_type = ValueType::Long;
    let mut changed_fulltext = current.clone();
    changed_fulltext.fulltext = true;
    for (attribute, code) in [
        (changed_type, "schema/value-type-immutable"),
        (changed_fulltext, "schema/fulltext-immutable"),
    ] {
        assert_eq!(
            base.with(&[TxOp::AlterAttribute(attribute)], 10, EntityRef::Id(99))
                .unwrap_err()
                .code,
            code
        );
    }
    assert!(
        base.with(
            &[retract(LEFT.into(), edb_core::DB_VALUE_TYPE as u32)],
            10,
            EntityRef::Id(99)
        )
        .is_err()
    );
    assert!(
        base.with(
            &[retract(edb_core::DB_IDENT, edb_core::DB_CARDINALITY as u32)],
            10,
            EntityRef::Id(99)
        )
        .is_err()
    );
    let mut component = current.clone();
    component.id = 510;
    component.ident = Keyword::new("schema-test", "bad-component");
    component.component = true;
    assert_eq!(
        base.with(&[TxOp::InstallAttribute(component)], 10, EntityRef::Id(99))
            .unwrap_err()
            .code,
        "schema/component-must-be-ref"
    );
    let mut bytes = Attribute::new(
        510,
        Keyword::new("schema-test", "bytes"),
        ValueType::Bytes,
        Cardinality::One,
    )
    .unique(Unique::Value);
    assert_eq!(
        base.with(
            &[TxOp::InstallAttribute(bytes.clone())],
            10,
            EntityRef::Id(99)
        )
        .unwrap_err()
        .code,
        "schema/bytes-cannot-be-unique"
    );
    bytes.value_type = ValueType::Long;
    bytes.cardinality = Cardinality::Many;
    assert_eq!(
        base.with(&[TxOp::InstallAttribute(bytes)], 10, EntityRef::Id(99))
            .unwrap_err()
            .code,
        "schema/unique-must-be-cardinality-one"
    );
    let mut bad_tuple = pair();
    bad_tuple.tuple = Some(TupleSpec::Homogeneous(ValueType::Float));
    assert_eq!(
        database(false)
            .with(&[TxOp::InstallAttribute(bad_tuple)], 10, EntityRef::Id(99))
            .unwrap_err()
            .code,
        "schema/invalid-tuple-element-type"
    );
    let composite = database(true);
    let mut many_constituent = current;
    many_constituent.cardinality = Cardinality::Many;
    assert_eq!(
        composite
            .with(
                &[TxOp::AlterAttribute(many_constituent)],
                10,
                EntityRef::Id(99)
            )
            .unwrap_err()
            .code,
        "schema/invalid-tuple-attributes"
    );
    let mut changed_tuple = pair();
    changed_tuple.tuple = Some(TupleSpec::Composite(vec![RIGHT, LEFT]));
    assert_eq!(
        composite
            .with(
                &[TxOp::AlterAttribute(changed_tuple)],
                10,
                EntityRef::Id(99)
            )
            .unwrap_err()
            .code,
        "schema/tuple-definition-immutable"
    );
}

#[test]
fn tuple_slots_keep_exact_types_nil_arity_and_utf16_limits() {
    use num_bigint::BigInt;
    let homogeneous = Attribute::new(
        510,
        Keyword::new("schema-test", "strings"),
        ValueType::Tuple,
        Cardinality::One,
    )
    .tuple(TupleSpec::Homogeneous(ValueType::String));
    let mixed = Attribute::new(
        511,
        Keyword::new("schema-test", "mixed"),
        ValueType::Tuple,
        Cardinality::One,
    )
    .tuple(TupleSpec::Heterogeneous(vec![
        ValueType::Long,
        ValueType::BigInt,
    ]));
    let mut schema = Schema::new();
    schema.install(homogeneous.clone()).unwrap();
    schema.install(mixed.clone()).unwrap();
    let value = |first| Value::Tuple(vec![Some(Value::String(first)), None]);
    assert!(
        schema
            .validate_value(&homogeneous, &value("😀".repeat(128)))
            .is_ok()
    );
    assert_eq!(
        schema
            .validate_value(&homogeneous, &value("😀".repeat(129)))
            .unwrap_err()
            .code,
        "transaction/tuple-string-too-large"
    );
    assert_eq!(
        schema
            .validate_value(&homogeneous, &Value::Tuple(vec![None]))
            .unwrap_err()
            .code,
        "transaction/invalid-tuple-length"
    );
    assert_eq!(
        schema
            .validate_value(&homogeneous, &Value::Tuple(vec![None; 9]))
            .unwrap_err()
            .code,
        "transaction/invalid-tuple-length"
    );
    assert_eq!(
        schema
            .validate_value(&mixed, &Value::Tuple(vec![Some(Value::Double(1.0)), None]))
            .unwrap_err()
            .code,
        "transaction/invalid-tuple-element"
    );
    let bound = BigInt::from(1) << 256;
    assert!(
        schema
            .validate_value(
                &mixed,
                &Value::Tuple(vec![None, Some(Value::BigInt(-&bound))])
            )
            .is_ok()
    );
    assert_eq!(
        schema
            .validate_value(
                &mixed,
                &Value::Tuple(vec![None, Some(Value::BigInt(bound))])
            )
            .unwrap_err()
            .code,
        "transaction/tuple-bigint-too-large"
    );
    assert_eq!(
        schema
            .validate_value(&mixed, &Value::Tuple(vec![None; 3]))
            .unwrap_err()
            .code,
        "transaction/invalid-tuple-length"
    );
}

#[test]
fn adding_uniqueness_requires_the_prior_projection_when_history_exists() {
    let attribute = scalar(LEFT, "left", Cardinality::One);
    let mut schema = Schema::new();
    schema.install(attribute.clone()).unwrap();
    let first = Database::new(schema)
        .unwrap()
        .with(
            &[add(ENTITY, LEFT, Value::String("key".into()))],
            10,
            EntityRef::Id(99),
        )
        .unwrap();
    let shared = first
        .db_after
        .with(
            &[add(ENTITY, LEFT, Value::String("key".into()))],
            11,
            EntityRef::Id(98),
        )
        .unwrap();
    let mut indexed = attribute;
    indexed.indexed = true;
    let unique = indexed.clone().unique(Unique::Value);
    assert_eq!(
        shared
            .db_after
            .with(
                &[TxOp::AlterAttribute(unique.clone())],
                12,
                EntityRef::Id(97)
            )
            .unwrap_err()
            .code,
        "schema/unique-requires-avet"
    );
    let ready = shared
        .db_after
        .with(&[TxOp::AlterAttribute(indexed)], 12, EntityRef::Id(97))
        .unwrap();
    let constrained = ready
        .db_after
        .with(&[TxOp::AlterAttribute(unique)], 13, EntityRef::Id(96))
        .unwrap();
    assert_eq!(
        live_sources(&constrained.db_after.database_value(), ENTITY, LEFT),
        vec![98, 99]
    );
    constrained.db_after.validate_invariants().unwrap();
}

#[test]
fn canonical_genesis_and_open_schema_metadata_keep_ordinary_attribution() {
    let base = Database::bootstrap().unwrap();
    let genesis = base.datoms(View::History, IndexOrder::Eavt);
    assert!(
        genesis
            .iter()
            .all(|d| d.source == edb_core::DB_SOURCE_SYSTEM)
    );
    assert!(
        !genesis
            .iter()
            .any(|d| d.attribute == edb_core::DB_TX_INSTANT as u32)
    );
    let metadata = Attribute::new(
        510,
        Keyword::new("schema-test", "owner"),
        ValueType::Ref,
        Cardinality::One,
    );
    let installed = base
        .with(
            &[
                TxOp::InstallAttribute(scalar(LEFT, "left", Cardinality::One)),
                TxOp::InstallAttribute(metadata),
            ],
            10,
            EntityRef::Id(99),
        )
        .unwrap();
    assert!(installed.tx_data.iter().all(|d| d.source == 99));
    let annotated = installed
        .db_after
        .with(
            &[
                add(LEFT.into(), 510, Value::Ref(400)),
                add(
                    LEFT.into(),
                    edb_core::DB_DOC as u32,
                    Value::String("ordinary documentation".into()),
                ),
                add(
                    LEFT.into(),
                    edb_core::DB_VALUE_TYPE as u32,
                    Value::Ref(edb_core::DB_TYPE_STRING),
                ),
                add(
                    LEFT.into(),
                    edb_core::DB_CARDINALITY as u32,
                    Value::Ref(edb_core::DB_CARDINALITY_ONE),
                ),
            ],
            11,
            EntityRef::Id(98),
        )
        .unwrap();
    assert!(annotated.tx_data.iter().all(|d| d.source == 98));
    assert_eq!(
        live_sources(
            &annotated.db_after.database_value(),
            LEFT.into(),
            edb_core::DB_VALUE_TYPE as u32
        ),
        vec![98, 99]
    );
    assert_eq!(
        annotated.db_after.values(LEFT.into(), 510),
        vec![&Value::Ref(400)]
    );
    annotated
        .db_after
        .rebuild_derived_caches()
        .unwrap()
        .validate_invariants()
        .unwrap();
}

#[test]
fn decimal_stored_scales_remain_distinct_when_sources_share_each_representation() {
    use bigdecimal::BigDecimal;
    let attribute = Attribute::new(
        LEFT,
        Keyword::new("schema-test", "decimal"),
        ValueType::BigDec,
        Cardinality::Many,
    );
    let mut schema = Schema::new();
    schema.install(attribute.clone()).unwrap();
    let decimal = |text: &str| Value::BigDec(text.parse::<BigDecimal>().unwrap());
    let first = Database::new(schema)
        .unwrap()
        .with(
            &[
                add(ENTITY, LEFT, decimal("1.0")),
                add(ENTITY, LEFT, decimal("1.00")),
            ],
            10,
            EntityRef::Id(99),
        )
        .unwrap();
    let shared = first
        .db_after
        .with(
            &[
                add(ENTITY, LEFT, decimal("1.0")),
                add(ENTITY, LEFT, decimal("1.00")),
            ],
            11,
            EntityRef::Id(98),
        )
        .unwrap();
    assert_eq!(shared.db_after.values(ENTITY, LEFT).len(), 2);
    assert_eq!(
        shared
            .db_after
            .database_value()
            .values(ENTITY, LEFT)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        live_sources(&shared.db_after.database_value(), ENTITY, LEFT),
        vec![98, 98, 99, 99]
    );
    let mut one = attribute;
    one.cardinality = Cardinality::One;
    assert_eq!(
        shared
            .db_after
            .with(&[TxOp::AlterAttribute(one)], 12, EntityRef::Id(97))
            .unwrap_err()
            .code,
        "schema/cardinality-change-conflict"
    );
    let withdrawn = shared
        .db_after
        .with(
            &[TxOp::Retract {
                entity: EntityRef::Id(ENTITY),
                attribute: LEFT,
                value: Some(decimal("1.0").into()),
            }],
            12,
            EntityRef::Id(97),
        )
        .unwrap();
    assert_eq!(
        live_sources(&withdrawn.db_after.database_value(), ENTITY, LEFT),
        vec![98, 99]
    );
    assert_eq!(withdrawn.db_after.values(ENTITY, LEFT).len(), 1);
    withdrawn
        .db_after
        .rebuild_derived_caches()
        .unwrap()
        .validate_invariants()
        .unwrap();
}
