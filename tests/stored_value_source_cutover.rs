use edb_core::{
    Attribute, Cardinality, Database, EntityRef, Keyword, QueryValue, Schema, TxOp, Value,
    ValueType,
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

fn scales(values: impl IntoIterator<Item = Value>) -> Vec<i64> {
    values
        .into_iter()
        .map(|value| match value {
            Value::BigDec(value) => value.fractional_digit_count(),
            _ => panic!("decimal expected"),
        })
        .collect()
}

#[test]
fn contribution_deduplication_preserves_decimal_scale_and_pull_limits() {
    let mut schema = Schema::new();
    schema
        .install(Attribute::new(
            100,
            Keyword::new("item", "amount"),
            ValueType::BigDec,
            Cardinality::Many,
        ))
        .unwrap();
    let mut db = Database::new(schema).unwrap();
    // Equal numeric values have different stored scales; transaction order
    // interleaves the two representations in their common index-value group.
    for (time, source, text) in [(1, 99, "1.0"), (2, 98, "1.00"), (3, 97, "1.0")] {
        db = db
            .with(&[add(text)], time, EntityRef::Id(source))
            .unwrap()
            .db_after;
    }
    assert_eq!(scales(db.values(500, 100).into_iter().cloned()), vec![1, 2]);
    let base = db.database_value();
    let overlay = base
        .with(&[add("1.00")], 4, EntityRef::Id(96))
        .unwrap()
        .db_after;
    for (value, expected) in [(&base, vec![1, 2]), (&overlay, vec![2, 1])] {
        assert_eq!(scales(value.values(500, 100).unwrap()), expected);
        for (limit, count) in [(1, 1), (2, 2)] {
            let selector = format!("[[:item/amount :limit {limit}]]");
            let pattern = edb_core::edn_pull::parse_pull_edn(&selector).unwrap();
            let QueryValue::Map(ref entries) = value.pull(&pattern, 500).unwrap() else {
                panic!()
            };
            let QueryValue::Collection(items) = &entries[0].1 else {
                panic!()
            };
            let returned = items.iter().map(|item| match item {
                QueryValue::Scalar(value) => value.clone(),
                _ => panic!(),
            });
            assert_eq!(scales(returned), expected[..count]);
        }
    }
    let program = edb_core::Program {
        kind: edb_core::ProgramKind::Query,
        arity: 0,
        instructions: vec![
            edb_core::Instruction::PushEntity(EntityRef::Id(500)),
            edb_core::Instruction::LoadMany(100),
            edb_core::Instruction::Length,
            edb_core::Instruction::EmitRow(1),
            edb_core::Instruction::Return,
        ],
    };
    for result in [
        edb_core::ProgramRuntime.execute(&program, &db, &[], Default::default()),
        edb_core::ProgramRuntime.execute_query(&program, &overlay, &[], Default::default()),
    ] {
        let edb_core::ProgramOutput::Query(rows) = result.unwrap() else {
            panic!()
        };
        assert_eq!(rows, vec![vec![Value::Long(2)]]);
    }
    let mut one_program = program;
    one_program.instructions[1] = edb_core::Instruction::LoadOne(100);
    one_program.instructions.remove(2);
    for result in [
        edb_core::ProgramRuntime.execute(&one_program, &db, &[], Default::default()),
        edb_core::ProgramRuntime.execute_query(&one_program, &overlay, &[], Default::default()),
    ] {
        assert_eq!(result.unwrap_err().code, "program/not-cardinality-one");
    }
    // Collapsing attribution must not turn two stored values into one value.
    let mut one = db.schema().attribute(100).unwrap().clone();
    one.cardinality = Cardinality::One;
    assert_eq!(
        db.with(&[TxOp::AlterAttribute(one)], 5, EntityRef::Id(95))
            .unwrap_err()
            .code,
        "schema/cardinality-change-conflict"
    );
    let retracted = overlay
        .with(
            &[TxOp::Retract {
                entity: EntityRef::Id(500),
                attribute: 100,
                value: Some(decimal("1.0").into()),
            }],
            5,
            EntityRef::Id(95),
        )
        .unwrap();
    assert_eq!(
        scales(retracted.db_after.values(500, 100).unwrap()),
        vec![2]
    );
    db.validate_invariants().unwrap();
}
