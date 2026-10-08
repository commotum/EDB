use edb_core::{
    Attribute, Cardinality, Database, EntityRef, IndexPrefix, Keyword, Schema, TxOp, Value,
    ValueType, tx_to_t,
};

fn add(entity: u64, value: i64) -> TxOp {
    TxOp::Add {
        entity: EntityRef::Id(entity),
        attribute: 100,
        value: Value::Long(value).into(),
    }
}

#[test]
fn proposed_information_is_checked_as_one_set_and_captured_before_is_immutable() {
    let mut schema = Schema::new();
    schema
        .install(Attribute::new(
            100,
            Keyword::new("item", "count"),
            ValueType::Long,
            Cardinality::One,
        ))
        .unwrap();
    let before = Database::new(schema)
        .unwrap()
        .database_value()
        .with(&[add(42, 1), add(43, 10)], 1, EntityRef::Id(99))
        .unwrap()
        .db_after;
    let basis = before.basis_t();
    let forms = [add(42, 2), add(42, 3), add(43, 20)];
    for order in [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ] {
        let proposed = order.map(|index| forms[index].clone());
        let error = before.with(&proposed, 2, EntityRef::Id(98)).unwrap_err();
        assert_eq!(error.code, "transaction/cardinality-one-conflict");
        assert_eq!(before.basis_t(), basis);
        assert_eq!(before.values(42, 100).unwrap(), vec![Value::Long(1)]);
        assert_eq!(before.values(43, 100).unwrap(), vec![Value::Long(10)]);
    }

    let report = before
        .with(&[add(42, 2), add(43, 20)], 2, EntityRef::Id(98))
        .unwrap();
    assert_eq!(report.db_before.basis_t(), basis);
    assert_eq!(report.db_after.basis_t(), basis + 1);
    assert_eq!(before.values(42, 100).unwrap(), vec![Value::Long(1)]);
    assert_eq!(before.values(43, 100).unwrap(), vec![Value::Long(10)]);
    assert_eq!(
        report.db_before.values(42, 100).unwrap(),
        vec![Value::Long(1)]
    );
    assert_eq!(
        report.db_before.values(43, 100).unwrap(),
        vec![Value::Long(10)]
    );
    assert_eq!(
        report.db_after.values(42, 100).unwrap(),
        vec![Value::Long(2)]
    );
    assert_eq!(
        report.db_after.values(43, 100).unwrap(),
        vec![Value::Long(20)]
    );
    assert_eq!(report.tx_data.len(), 5);
    for datom in &report.tx_data {
        assert_eq!(datom.source, 98);
        assert_eq!(tx_to_t(datom.tx).unwrap(), report.db_after.basis_t());
    }

    let history = report
        .db_after
        .clone()
        .history()
        .datoms_with_prefix(&IndexPrefix::Eavt {
            entity: 42,
            attribute: Some(100),
            value: None,
        })
        .unwrap();
    assert_eq!(history.len(), 3);
    assert!(
        history
            .iter()
            .any(|d| d.added && d.source == 99 && d.value == Value::Long(1))
    );
    assert!(
        history
            .iter()
            .any(|d| !d.added && d.source == 98 && d.value == Value::Long(1))
    );
    assert!(
        history
            .iter()
            .any(|d| d.added && d.source == 98 && d.value == Value::Long(2))
    );
}
