use edb_core::{
    Attribute, Cardinality, Database, EntityRef, IndexOrder, Keyword, Schema, TxOp, Value,
    ValueType, View, t_to_tx,
};

fn initial() -> Database {
    let mut schema = Schema::new();
    schema
        .install(
            Attribute::new(
                100,
                Keyword::new("item", "child"),
                ValueType::Ref,
                Cardinality::One,
            )
            .component(),
        )
        .unwrap();
    schema
        .install(Attribute::new(
            101,
            Keyword::new("item", "name"),
            ValueType::String,
            Cardinality::One,
        ))
        .unwrap();
    schema
        .install(Attribute::new(
            102,
            Keyword::new("item", "link"),
            ValueType::Ref,
            Cardinality::One,
        ))
        .unwrap();
    Database::new(schema).unwrap()
}

fn add(entity: EntityRef, attribute: u32, value: impl Into<edb_core::TxValue>) -> TxOp {
    TxOp::Add {
        entity,
        attribute,
        value: value.into(),
    }
}

#[test]
fn component_retraction_cannot_withdraw_transaction_activity() {
    let first = initial().with(&[], 1, EntityRef::Id(99)).unwrap();
    let transaction = t_to_tx(first.db_after.basis_t()).unwrap();
    let linked = first
        .db_after
        .with(
            &[
                add(
                    EntityRef::Temp("parent".into()),
                    101,
                    Value::String("parent".into()),
                ),
                add(
                    EntityRef::Temp("parent".into()),
                    100,
                    edb_core::TxValue::Entity(EntityRef::Temp("child".into())),
                ),
                add(
                    EntityRef::Temp("child".into()),
                    101,
                    Value::String("child".into()),
                ),
                add(
                    EntityRef::Temp("child".into()),
                    100,
                    Value::Ref(transaction),
                ),
            ],
            2,
            EntityRef::Id(98),
        )
        .unwrap();
    let parent = linked.tempids["parent"];
    let child = linked.tempids["child"];
    let captured = linked.db_after.datoms(View::Current, IndexOrder::Saevt);
    let history = linked.db_after.datoms(View::History, IndexOrder::Eavt);
    let basis = linked.db_after.basis_t();
    for entity in [transaction, parent] {
        let ops = [TxOp::RetractEntity(EntityRef::Id(entity))];
        let error = linked
            .db_after
            .with(&ops, 3, EntityRef::Id(97))
            .err()
            .expect("transaction entity cascade must fail atomically");
        assert_eq!(error.code, "transaction/reset-tx-instant");
        let overlay = linked.db_after.database_value();
        let error = overlay
            .with(&ops, 3, EntityRef::Id(97))
            .err()
            .expect("overlay transaction entity cascade must fail atomically");
        assert_eq!(error.code, "transaction/reset-tx-instant");
        assert_eq!(overlay.basis_t(), basis);
        assert_eq!(overlay.collect_datoms(IndexOrder::Saevt).unwrap(), captured);
        assert_eq!(
            overlay.history().collect_datoms(IndexOrder::Eavt).unwrap(),
            history
        );
    }
    assert_eq!(linked.db_after.basis_t(), basis);
    assert_eq!(
        linked.db_after.datoms(View::Current, IndexOrder::Saevt),
        captured
    );
    for (entity, name) in [(parent, "parent"), (child, "child")] {
        assert_eq!(
            linked.db_after.values(entity, 101),
            vec![&Value::String(name.into())]
        );
    }
    assert_eq!(
        linked.db_after.values(child, 100),
        vec![&Value::Ref(transaction)]
    );
    assert!(captured.iter().any(|d| {
        d.entity == transaction
            && d.attribute == edb_core::DB_TX_INSTANT as u32
            && d.source == 99
            && d.added
    }));
}

#[test]
fn ordinary_component_cascade_withdraws_all_sources_and_preserves_history() {
    let first = initial()
        .with(
            &[
                add(
                    EntityRef::Temp("parent".into()),
                    101,
                    Value::String("parent".into()),
                ),
                add(
                    EntityRef::Temp("child".into()),
                    101,
                    Value::String("child".into()),
                ),
                add(
                    EntityRef::Temp("leaf".into()),
                    101,
                    Value::String("leaf".into()),
                ),
                add(
                    EntityRef::Temp("parent".into()),
                    100,
                    edb_core::TxValue::Entity(EntityRef::Temp("child".into())),
                ),
                add(
                    EntityRef::Temp("child".into()),
                    100,
                    edb_core::TxValue::Entity(EntityRef::Temp("leaf".into())),
                ),
                add(
                    EntityRef::Temp("outside".into()),
                    102,
                    edb_core::TxValue::Entity(EntityRef::Temp("child".into())),
                ),
            ],
            1,
            EntityRef::Id(99),
        )
        .unwrap();
    let mut corroboration = Vec::new();
    for name in ["parent", "child", "leaf"] {
        corroboration.push(add(
            EntityRef::Id(first.tempids[name]),
            101,
            Value::String(name.into()),
        ));
    }
    let second = first
        .db_after
        .with(&corroboration, 2, EntityRef::Id(98))
        .unwrap();
    let captured = second.db_after.database_value();
    let ops = [TxOp::RetractEntity(EntityRef::Id(first.tempids["parent"]))];
    let eager = second.db_after.with(&ops, 3, EntityRef::Id(97)).unwrap();
    let overlay = captured.with(&ops, 3, EntityRef::Id(97)).unwrap();
    assert_eq!(eager.tx_data, overlay.tx_data);
    assert_eq!(eager.tx_data.iter().filter(|d| !d.added).count(), 6);
    assert!(eager.tx_data.iter().all(|d| d.source == 97));
    for name in ["parent", "child", "leaf"] {
        let entity = first.tempids[name];
        assert!(eager.db_after.values(entity, 101).is_empty());
        assert!(overlay.db_after.values(entity, 101).unwrap().is_empty());
        assert_eq!(
            captured.values(entity, 101).unwrap(),
            vec![Value::String(name.into())]
        );
        let events = eager
            .db_after
            .datoms(View::History, IndexOrder::Eavt)
            .into_iter()
            .filter(|d| d.entity == entity && d.attribute == 101)
            .collect::<Vec<_>>();
        assert_eq!(events.len(), 3);
        assert!(events.iter().any(|d| d.source == 99 && d.added));
        assert!(events.iter().any(|d| d.source == 98 && d.added));
        assert!(events.iter().any(|d| d.source == 97 && !d.added));
    }
    assert!(
        eager
            .db_after
            .values(first.tempids["outside"], 102)
            .is_empty()
    );
    for order in [
        IndexOrder::Eavt,
        IndexOrder::Aevt,
        IndexOrder::Avet,
        IndexOrder::Vaet,
        IndexOrder::Saevt,
        IndexOrder::Savet,
    ] {
        assert_eq!(
            eager.db_after.datoms(View::Current, order),
            overlay.db_after.collect_datoms(order).unwrap()
        );
    }
}
