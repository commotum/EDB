use edb_core::{
    Attribute, Cardinality, Database, EntityRef, Instruction, Keyword, Program, ProgramControl,
    ProgramKind, ProgramOutput, ProgramRuntime, RuntimeValue, Schema, TxOp, Unique, Value,
    ValueType, decode_program, encode_program, program_hash, sha256,
};

fn database() -> Database {
    let mut schema = Schema::new();
    schema
        .install(
            Attribute::new(
                100,
                Keyword::new("item", "name"),
                ValueType::String,
                Cardinality::One,
            )
            .unique(Unique::Identity),
        )
        .unwrap();
    schema
        .install(Attribute::new(
            101,
            Keyword::new("item", "value"),
            ValueType::Long,
            Cardinality::Many,
        ))
        .unwrap();
    Database::new(schema).unwrap()
}

fn add(entity: u64, attribute: u32, value: Value) -> TxOp {
    TxOp::Add {
        entity: EntityRef::Id(entity),
        attribute,
        value: value.into(),
    }
}

fn reader(reference: EntityRef, instruction: Instruction) -> Program {
    let emit = if matches!(instruction, Instruction::LoadMany(_)) {
        Instruction::ForEach {
            body: vec![Instruction::EmitRow(1)],
        }
    } else {
        Instruction::EmitRow(1)
    };
    Program {
        kind: ProgramKind::Query,
        arity: 0,
        instructions: vec![
            Instruction::PushEntity(reference),
            instruction,
            emit,
            Instruction::Return,
        ],
    }
}

fn rows(output: ProgramOutput) -> Vec<Vec<Value>> {
    let ProgramOutput::Query(rows) = output else {
        panic!("expected scalar query output");
    };
    rows
}

#[test]
fn vm_reads_count_distinct_values_and_keep_exact_views() {
    let first = database()
        .with(
            &[
                add(42, 100, Value::String("Peaches".into())),
                add(42, 101, Value::Long(10)),
                add(42, 101, Value::Long(20)),
            ],
            1,
            EntityRef::Id(99),
        )
        .unwrap();
    let second = first
        .db_after
        .with(
            &[
                add(42, 100, Value::String("Peaches".into())),
                add(42, 101, Value::Long(10)),
                add(42, 101, Value::Long(20)),
            ],
            2,
            EntityRef::Id(98),
        )
        .unwrap();
    let one = reader(EntityRef::Id(42), Instruction::LoadOne(100));
    let many = reader(EntityRef::Id(42), Instruction::LoadMany(101));
    assert_eq!(
        rows(
            ProgramRuntime
                .execute(&one, &second.db_after, &[], ProgramControl::default())
                .unwrap()
        ),
        vec![vec![Value::String("Peaches".into())]]
    );
    let exact = second.db_after.database_value();
    assert_eq!(
        rows(
            ProgramRuntime
                .execute_query(&one, &exact, &[], ProgramControl::default())
                .unwrap()
        ),
        vec![vec![Value::String("Peaches".into())]]
    );
    {
        let db = &second.db_after;
        assert_eq!(
            rows(
                ProgramRuntime
                    .execute(
                        &many,
                        db,
                        &[],
                        ProgramControl {
                            max_collection_items: 2,
                            ..ProgramControl::default()
                        }
                    )
                    .unwrap()
            ),
            vec![vec![Value::Long(10)], vec![Value::Long(20)]]
        );
    }
    assert_eq!(
        rows(
            ProgramRuntime
                .execute_query(
                    &many,
                    &exact,
                    &[],
                    ProgramControl {
                        max_collection_items: 2,
                        ..ProgramControl::default()
                    }
                )
                .unwrap()
        ),
        vec![vec![Value::Long(10)], vec![Value::Long(20)]]
    );
    let multiple = reader(EntityRef::Id(42), Instruction::LoadOne(101));
    assert_eq!(
        ProgramRuntime
            .execute_query(&multiple, &exact, &[], ProgramControl::default())
            .unwrap_err()
            .code,
        "program/not-cardinality-one"
    );
    let replaced = second
        .db_after
        .with(
            &[add(42, 100, Value::String("Pears".into()))],
            3,
            EntityRef::Id(97),
        )
        .unwrap();
    assert_eq!(
        rows(
            ProgramRuntime
                .execute_query(
                    &one,
                    &replaced.db_after.database_value(),
                    &[],
                    ProgramControl::default()
                )
                .unwrap()
        ),
        vec![vec![Value::String("Pears".into())]]
    );
    assert_eq!(
        rows(
            ProgramRuntime
                .execute_query(
                    &one,
                    &replaced.db_after.database_value().as_of(2),
                    &[],
                    ProgramControl::default()
                )
                .unwrap()
        ),
        vec![vec![Value::String("Peaches".into())]]
    );
}

#[test]
fn raw_lookup_programs_have_canonical_identity_and_resolve_at_invocation() {
    let canonical = reader(
        EntityRef::Edn(r#"[:item/name "Peaches"]"#.into()),
        Instruction::LoadMany(101),
    );
    let spelled = reader(
        EntityRef::Edn("[ ; retained author spelling\n :item/name   \"Peaches\" ]".into()),
        Instruction::LoadMany(101),
    );
    assert_eq!(
        encode_program(&canonical).unwrap(),
        encode_program(&spelled).unwrap()
    );
    assert_eq!(
        program_hash(&canonical).unwrap(),
        program_hash(&spelled).unwrap()
    );
    let decoded = decode_program(&encode_program(&spelled).unwrap()).unwrap();
    assert_eq!(decoded, canonical);
    for (entity, number) in [(42, 10), (43, 20)] {
        let db = database()
            .with(
                &[
                    add(entity, 100, Value::String("Peaches".into())),
                    add(entity, 101, Value::Long(number)),
                ],
                1,
                EntityRef::Id(99),
            )
            .unwrap()
            .db_after;
        assert_eq!(
            rows(
                ProgramRuntime
                    .execute(&decoded, &db, &[], ProgramControl::default())
                    .unwrap()
            ),
            vec![vec![Value::Long(number)]]
        );
        assert_eq!(
            rows(
                ProgramRuntime
                    .execute_query(
                        &decoded,
                        &db.database_value(),
                        &[],
                        ProgramControl::default()
                    )
                    .unwrap()
            ),
            vec![vec![Value::Long(number)]]
        );
    }
    // Structural admission is independent of the later database's schema.
    assert_eq!(
        ProgramRuntime
            .execute(&decoded, &database(), &[], ProgramControl::default())
            .unwrap_err()
            .code,
        "program/entity-not-found"
    );
}

#[test]
fn abi_twelve_is_rejected_even_with_an_authenticated_envelope() {
    let program = Program {
        kind: ProgramKind::Transaction,
        arity: 0,
        instructions: vec![Instruction::Return],
    };
    let mut bytes = encode_program(&program).unwrap();
    bytes[16..18].copy_from_slice(&12u16.to_be_bytes());
    let payload_len = bytes.len() - 32;
    let checksum = sha256(&bytes[..payload_len]);
    bytes[payload_len..].copy_from_slice(&checksum);
    assert_eq!(
        decode_program(&bytes).unwrap_err().code,
        "encoding/unsupported-program-abi"
    );
}

#[test]
fn raw_reference_retention_cannot_hide_behind_small_canonical_bytes() {
    let program = Program {
        kind: ProgramKind::Transaction,
        arity: 1,
        instructions: vec![
            Instruction::PushArgument(0),
            Instruction::EmitRetractEntity,
            Instruction::Return,
        ],
    };
    let raw = EntityRef::Edn(format!("42 ;{}", "retained comment ".repeat(4096)));
    assert_eq!(
        ProgramRuntime
            .execute_runtime(
                &program,
                &database(),
                &[RuntimeValue::Entity(raw)],
                ProgramControl {
                    max_value_bytes: 1024,
                    ..ProgramControl::default()
                }
            )
            .unwrap_err()
            .code,
        "program/value-byte-limit"
    );
}

#[test]
fn hash_equivalent_raw_literals_and_nested_arguments_have_the_same_vm_equality() {
    let literal = |raw: &str| Program {
        kind: ProgramKind::Query,
        arity: 0,
        instructions: vec![
            Instruction::PushEntity(EntityRef::Edn(raw.into())),
            Instruction::PushEntity(EntityRef::Edn(r#"[:item/name "Peaches"]"#.into())),
            Instruction::Equal,
            Instruction::Return,
        ],
    };
    let canonical = literal(r#"[:item/name "Peaches"]"#);
    let spelled = literal("[ ; comments are spelling\n :item/name  \"Peaches\" ]");
    assert_eq!(
        program_hash(&canonical).unwrap(),
        program_hash(&spelled).unwrap()
    );
    for program in [
        &canonical,
        &spelled,
        &decode_program(&encode_program(&spelled).unwrap()).unwrap(),
    ] {
        assert_eq!(
            rows(
                ProgramRuntime
                    .execute(program, &database(), &[], ProgramControl::default())
                    .unwrap()
            ),
            vec![vec![Value::Bool(true)]]
        );
    }
    let program = Program {
        kind: ProgramKind::Query,
        arity: 2,
        instructions: vec![
            Instruction::PushArgument(0),
            Instruction::PushArgument(1),
            Instruction::Equal,
            Instruction::Return,
        ],
    };
    let nested = |raw: &str| {
        RuntimeValue::map(vec![(
            Value::Long(1),
            RuntimeValue::Vector(vec![RuntimeValue::Entity(EntityRef::LookupInput {
                attribute: 100,
                value: Box::new(edb_core::TxValue::Entity(EntityRef::Edn(raw.into()))),
            })]),
        )])
        .unwrap()
    };
    assert_eq!(
        rows(
            ProgramRuntime
                .execute_runtime(
                    &program,
                    &database(),
                    &[nested(":item/name"), nested(" ; comment\n :item/name")],
                    ProgramControl::default()
                )
                .unwrap()
        ),
        vec![vec![Value::Bool(true)]]
    );
}

#[test]
fn inert_emitted_callable_literals_use_their_canonical_program_identity() {
    let program = Program {
        kind: ProgramKind::Transaction,
        arity: 0,
        instructions: vec![
            Instruction::EmitCall {
                function: edb_core::CallableRef::Database(EntityRef::Edn(
                    " ; call\n :test/function ".into(),
                )),
                argument_count: 0,
            },
            Instruction::Return,
        ],
    };
    let ProgramOutput::Transaction(forms) = ProgramRuntime
        .execute(&program, &database(), &[], ProgramControl::default())
        .unwrap()
    else {
        panic!("expected transaction forms");
    };
    let [edb_core::TxForm::ProgramCall(call)] = forms.as_slice() else {
        panic!("expected one call");
    };
    assert_eq!(
        call.function,
        edb_core::CallableRef::Database(EntityRef::Edn(":test/function".into()))
    );
}
