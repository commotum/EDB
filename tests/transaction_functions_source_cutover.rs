use edb_core::{
    Attribute, CallableRef, Cardinality, DB_DOC, DB_TX_INSTANT, Database, DatabaseValue, EntityRef,
    Keyword, NativeRegistry, ProgramCall, RuntimeValue, Schema, SemanticError, SpeculationLimits,
    Symbol, TransactionExecutionOptions, TxForm, TxOp, Value, ValueType,
};

fn add(entity: EntityRef, attribute: u32, value: Value) -> TxForm {
    TxForm::Op(TxOp::Add {
        entity,
        attribute,
        value: value.into(),
    })
}

fn call(name: &str, arguments: Vec<RuntimeValue>) -> TxForm {
    TxForm::ProgramCall(ProgramCall {
        function: CallableRef::Local(Symbol::new("test.fns", name)),
        arguments,
    })
}

fn database() -> DatabaseValue {
    let mut schema = Schema::new();
    schema
        .install(Attribute::new(
            100,
            Keyword::new("account", "balance"),
            ValueType::Long,
            Cardinality::One,
        ))
        .unwrap();
    Database::new(schema)
        .unwrap()
        .database_value()
        .with_forms(
            &[add(EntityRef::Id(42), 100, Value::Long(100))],
            1,
            EntityRef::Id(99),
        )
        .unwrap()
        .db_after
}

fn native_options() -> TransactionExecutionOptions {
    let mut builder = NativeRegistry::builder();
    builder
        .transaction(Symbol::new("test.fns", "parent"), |db, _, control| {
            control.check(1)?;
            assert_eq!(db.values(42, 100)?, vec![Value::Long(100)]);
            Ok(vec![
                add(EntityRef::Id(42), 100, Value::Long(200)),
                add(
                    EntityRef::Temp("writer".into()),
                    DB_DOC as u32,
                    Value::String("Contributor".into()),
                ),
                call("child", vec![]),
            ])
        })
        .unwrap();
    for (name, target) in [("child", 43), ("sibling", 44)] {
        builder
            .transaction(Symbol::new("test.fns", name), move |db, _, control| {
                control.check(1)?;
                let values = db.values(42, 100)?;
                assert_eq!(values, vec![Value::Long(100)]);
                Ok(vec![add(EntityRef::Id(target), 100, values[0].clone())])
            })
            .unwrap();
    }
    TransactionExecutionOptions {
        native: builder.build(),
        ..Default::default()
    }
}

#[test]
fn nested_and_sibling_native_calls_share_db_before_source_and_call_budget() {
    let db = database();
    let options = native_options();
    let forms = [
        add(EntityRef::Id(42), 100, Value::Long(200)),
        call("parent", vec![]),
        call("sibling", vec![]),
    ];
    let mut expected = None;
    for order in [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ] {
        let permuted = order.map(|index| forms[index].clone());
        let report = db
            .with_forms_with_execution_options(
                &permuted,
                2,
                EntityRef::Temp("writer".into()),
                SpeculationLimits::default(),
                &options,
            )
            .unwrap();
        let source = report.tempids["writer"];
        assert!(report.tx_data.iter().all(|datom| datom.source == source));
        assert_eq!(
            report.db_after.values(42, 100).unwrap(),
            vec![Value::Long(200)]
        );
        for target in [43, 44] {
            assert_eq!(
                report.db_after.values(target, 100).unwrap(),
                vec![Value::Long(100)]
            );
        }
        assert_eq!(
            report
                .tx_data
                .iter()
                .filter(|d| d.attribute == DB_TX_INSTANT as u32)
                .count(),
            1
        );
        assert!(
            report
                .tx_data
                .iter()
                .any(|d| d.entity == 42 && !d.added && d.value == Value::Long(100))
        );
        match &expected {
            Some(data) => assert_eq!(&report.tx_data, data),
            None => expected = Some(report.tx_data),
        }
    }
    let mut limits = SpeculationLimits::default();
    limits.program.max_calls = 2;
    assert_eq!(
        db.with_forms_with_execution_options(
            &forms,
            2,
            EntityRef::Temp("writer".into()),
            limits,
            &options,
        )
        .unwrap_err()
        .code,
        "program/call-limit"
    );
    assert_eq!(db.values(42, 100).unwrap(), vec![Value::Long(100)]);
    assert!(db.values(43, 100).unwrap().is_empty());
}

#[test]
fn empty_native_output_keeps_one_timestamp_and_accepts_more_than_ten_arguments() {
    let db = database();
    let mut builder = NativeRegistry::builder();
    builder
        .transaction(Symbol::new("test.fns", "empty"), |_, args, control| {
            control.check(args.len() as u64)?;
            assert_eq!(args.len(), 11);
            Ok(vec![])
        })
        .unwrap();
    let options = TransactionExecutionOptions {
        native: builder.build(),
        ..Default::default()
    };
    let report = db
        .with_forms_with_execution_options(
            &[call("empty", vec![Value::Long(1).into(); 11])],
            2,
            EntityRef::Id(98),
            SpeculationLimits::default(),
            &options,
        )
        .unwrap();
    assert_eq!(report.tx_data.len(), 1);
    let timestamp = &report.tx_data[0];
    assert_eq!(timestamp.attribute, DB_TX_INSTANT as u32);
    assert_eq!(timestamp.value, Value::Instant(2));
    assert_eq!(timestamp.entity, timestamp.tx);
    assert_eq!(timestamp.source, 98);
    assert!(timestamp.added);
}

#[test]
fn native_rejections_panics_and_invalid_generated_forms_do_not_change_db_before() {
    let db = database();
    let original_basis = db.basis_t();
    let mut builder = NativeRegistry::builder();
    builder
        .transaction(Symbol::new("test.fns", "reject"), |_, _, _| {
            Err(SemanticError::incorrect("app/invalid-user", "invalid user"))
        })
        .unwrap();
    builder
        .transaction(Symbol::new("test.fns", "panic"), |_, _, _| {
            panic!("test callback panic")
        })
        .unwrap();
    builder
        .transaction(Symbol::new("test.fns", "invalid"), |_, _, _| {
            Ok(vec![add(
                EntityRef::Id(42),
                100,
                Value::String("wrong type".into()),
            )])
        })
        .unwrap();
    let options = TransactionExecutionOptions {
        native: builder.build(),
        ..Default::default()
    };
    for (name, code) in [
        ("reject", "app/invalid-user"),
        ("panic", "native/callback-panic"),
        ("invalid", "transaction/value-type"),
        ("missing", "native/deployment-not-found"),
    ] {
        let error = db
            .with_forms_with_execution_options(
                &[
                    call(name, vec![]),
                    add(EntityRef::Id(43), 100, Value::Long(200)),
                ],
                2,
                EntityRef::Id(98),
                SpeculationLimits::default(),
                &options,
            )
            .unwrap_err();
        assert_eq!(error.code, code);
        assert_eq!(db.basis_t(), original_basis);
        assert_eq!(db.values(42, 100).unwrap(), vec![Value::Long(100)]);
        assert!(db.values(43, 100).unwrap().is_empty());
    }
}
