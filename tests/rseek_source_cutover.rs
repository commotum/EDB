use edb_core::{
    Attribute, AttributeName, Cardinality, Database, EntityIdentifier, IndexBoundary,
    IndexComponents, IndexTransaction, Keyword, RawIndexValue, Schema, Value, ValueType,
};

#[test]
fn seek_boundaries_reject_invalid_native_transaction_domains_in_every_order() {
    let mut schema = Schema::new();
    let mut link = Attribute::new(
        100,
        Keyword::new("seek-test", "link"),
        ValueType::Ref,
        Cardinality::One,
    );
    link.indexed = true;
    schema.install(link).unwrap();
    let memory = Database::new(schema).unwrap();
    let db = memory.database_value();

    for (transaction, code) in [
        // A native Tx denotes a transaction-partition entity, not a logical T.
        (IndexTransaction::Tx(42), "identity/not-a-transaction-id"),
        (IndexTransaction::T(u64::MAX), "identity/eidx-out-of-range"),
    ] {
        let raw = [
            IndexBoundary::Eavt(IndexComponents::Five(
                42,
                100,
                Value::Ref(77),
                transaction,
                99,
            )),
            IndexBoundary::Aevt(IndexComponents::Five(
                100,
                42,
                Value::Ref(77),
                transaction,
                99,
            )),
            IndexBoundary::Avet(IndexComponents::Five(
                100,
                Value::Ref(77),
                42,
                transaction,
                99,
            )),
            IndexBoundary::Vaet(IndexComponents::Five(
                Value::Ref(77),
                100,
                42,
                transaction,
                99,
            )),
            IndexBoundary::Saevt(IndexComponents::Five(
                99,
                100,
                42,
                Value::Ref(77),
                transaction,
            )),
            IndexBoundary::Savet(IndexComponents::Five(
                99,
                100,
                Value::Ref(77),
                42,
                transaction,
            )),
        ];
        for boundary in &raw {
            assert_eq!(boundary.validate().unwrap_err().code, code);
            assert_eq!(
                db.seek_cursor(boundary)
                    .err()
                    .expect("invalid transaction must fail before forward traversal")
                    .code,
                code,
                "forward {:?}",
                boundary.order(),
            );
            assert_eq!(
                db.reverse_seek_cursor(boundary)
                    .err()
                    .expect("invalid transaction must fail before reverse traversal")
                    .code,
                code,
                "reverse {:?}",
                boundary.order(),
            );
        }

        let entity = EntityIdentifier::Id(42);
        let attribute = AttributeName::Id(100);
        let value = RawIndexValue::Stored(Value::Ref(77));
        let source = EntityIdentifier::Id(99);
        let resolved = [
            db.eavt_boundary(IndexComponents::Five(
                entity.clone(),
                attribute.clone(),
                value.clone(),
                transaction,
                source.clone(),
            )),
            db.aevt_boundary(IndexComponents::Five(
                attribute.clone(),
                entity.clone(),
                value.clone(),
                transaction,
                source.clone(),
            )),
            db.avet_boundary(IndexComponents::Five(
                attribute.clone(),
                value.clone(),
                entity.clone(),
                transaction,
                source.clone(),
            )),
            db.vaet_boundary(IndexComponents::Five(
                value.clone(),
                attribute.clone(),
                entity.clone(),
                transaction,
                source.clone(),
            )),
            db.saevt_boundary(IndexComponents::Five(
                source.clone(),
                attribute.clone(),
                entity.clone(),
                value.clone(),
                transaction,
            )),
            db.savet_boundary(IndexComponents::Five(
                source,
                attribute,
                value,
                entity,
                transaction,
            )),
        ];
        for boundary in resolved {
            assert_eq!(boundary.unwrap_err().code, code);
        }
    }
}
