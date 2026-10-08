use edb_core::{
    DB_DOC, DB_IDENT, DB_PARTITION, DB_TX_INSTANT, Database, EntityRef, Keyword,
    TransactionDefaults, TxOp, Value, eid_to_part, implicit_part, partition_eid, t_to_tx,
};

fn named_database() -> (Database, TransactionDefaults, u64) {
    let installed = Database::bootstrap()
        .unwrap()
        .with_edn(
            r#"[{:db/id "partition" :db/ident :part/orders
                 :db.install/_partition :db.part/db}]"#,
            1,
            EntityRef::Temp("partition".into()),
        )
        .unwrap();
    let partition = installed.tempids["partition"];
    assert_eq!(eid_to_part(partition).unwrap(), DB_PARTITION);
    assert!(installed.tx_data.iter().all(|d| d.source == partition));
    let defaults =
        TransactionDefaults::default().with_default_partition(Keyword::new("part", "orders"));
    (installed.db_after, defaults, partition)
}

fn add(entity: EntityRef, attribute: u64, value: Value) -> TxOp {
    TxOp::Add {
        entity,
        attribute: attribute as u32,
        value: value.into(),
    }
}

#[test]
fn source_tempid_uses_ordinary_default_and_explicit_partition_policy() {
    let (db, defaults, named) = named_database();
    let implicit = implicit_part(343).unwrap();
    let text = format!(
        r#"[{{:db/id "source" :db/doc "contributor"}}
            {{:db/id "related" :db/doc "related"}}
            {{:db/id "unassigned" :db/doc "ordinary"}}
            {{:db/force-partition {{"source" {implicit}}}
              :db/match-partition {{"related" "source"}}}}]"#
    );
    let source = EntityRef::from_edn(r#""source""#).unwrap();
    let memory = db
        .with_edn_with_defaults(&text, 2, source.clone(), &defaults)
        .unwrap();
    let exact = db
        .database_value()
        .with_edn_with_defaults(&text, 2, source, &defaults)
        .unwrap();
    assert_eq!(memory.tempids, exact.tempids);
    assert_eq!(memory.tx_data, exact.tx_data);
    for name in ["source", "related"] {
        assert_eq!(partition_eid(memory.tempids[name]).unwrap(), implicit);
    }
    assert_eq!(partition_eid(memory.tempids["unassigned"]).unwrap(), named);
    assert!(
        memory
            .tx_data
            .iter()
            .all(|d| d.source == memory.tempids["source"])
    );
    assert_eq!(db.basis_t(), 1);
    assert_eq!(db.eidx_frontier(), memory.db_before.eidx_frontier());

    let unforced = db
        .with_edn_with_defaults(
            r#"[{:db/id "source" :db/doc "contributor"}]"#,
            2,
            EntityRef::Temp("source".into()),
            &defaults,
        )
        .unwrap();
    assert_eq!(partition_eid(unforced.tempids["source"]).unwrap(), named);
}

#[test]
fn source_upsert_keeps_identity_and_matching_tempid_keeps_original_policy() {
    let (db, defaults, named) = named_database();
    let first = db
        .with_edn_with_defaults(
            r#"[{:db/id "source" :db/ident :contributor/julia}]"#,
            2,
            EntityRef::Temp("source".into()),
            &defaults,
        )
        .unwrap();
    let existing = first.tempids["source"];
    let implicit = implicit_part(500).unwrap();
    let next = first
        .db_after
        .with_defaults(
            &[
                add(
                    EntityRef::Temp("source".into()),
                    DB_IDENT,
                    Value::Keyword(Keyword::new("contributor", "julia")),
                ),
                add(
                    EntityRef::Temp("via-tempid".into()),
                    DB_DOC,
                    Value::String("tempid".into()),
                ),
                add(
                    EntityRef::Temp("via-id".into()),
                    DB_DOC,
                    Value::String("id".into()),
                ),
                TxOp::ForcePartition {
                    tempid: "source".into(),
                    partition: EntityRef::Id(implicit),
                },
                TxOp::MatchPartition {
                    tempid: "via-tempid".into(),
                    entity: EntityRef::Temp("source".into()),
                },
                TxOp::MatchPartition {
                    tempid: "via-id".into(),
                    entity: EntityRef::Id(existing),
                },
            ],
            3,
            EntityRef::Temp("source".into()),
            &defaults,
        )
        .unwrap();
    assert_eq!(next.tempids["source"], existing);
    assert_eq!(partition_eid(existing).unwrap(), named);
    assert_eq!(partition_eid(next.tempids["via-tempid"]).unwrap(), implicit);
    assert_eq!(partition_eid(next.tempids["via-id"]).unwrap(), named);
    assert!(next.tx_data.iter().all(|d| d.source == existing));
}

#[test]
fn dependent_source_upsert_preserves_existing_partition_and_original_match_policy() {
    let (db, defaults, named) = named_database();
    let installed = db
        .with_edn(
            r#"[{:db/ident :contributor/account :db/valueType :db.type/ref
                 :db/cardinality :db.cardinality/one :db/unique :db.unique/identity}]"#,
            2,
            EntityRef::Id(99),
        )
        .unwrap();
    let seed = r#"[{:db/id "account" :db/ident :account/julia}
                   {:db/id "source" :contributor/account "account"}]"#;
    let source = EntityRef::from_edn(r#""source""#).unwrap();
    let memory_seed = installed
        .db_after
        .with_edn_with_defaults(seed, 3, source.clone(), &defaults)
        .unwrap();
    let overlay_seed = installed
        .db_after
        .database_value()
        .with_edn_with_defaults(seed, 3, source.clone(), &defaults)
        .unwrap();
    assert_eq!(memory_seed.tempids, overlay_seed.tempids);
    let existing_account = memory_seed.tempids["account"];
    let existing_source = memory_seed.tempids["source"];
    assert_eq!(partition_eid(existing_source).unwrap(), named);

    let implicit = implicit_part(500).unwrap();
    let text = format!(
        r#"[{{:db/id "account" :db/ident :account/julia}}
            {{:db/id "source" :contributor/account "account"}}
            {{:db/id "related" :db/doc "new related entity"}}
            {{:db/force-partition {{"source" {implicit}}}
              :db/match-partition {{"related" "source"}}}}]"#
    );
    let memory = memory_seed
        .db_after
        .with_edn_with_defaults(&text, 4, source.clone(), &defaults)
        .unwrap();
    let overlay = overlay_seed
        .db_after
        .with_edn_with_defaults(&text, 4, source, &defaults)
        .unwrap();
    assert_eq!(memory.tempids, overlay.tempids);
    assert_eq!(memory.tx_data, overlay.tx_data);
    assert_eq!(memory.tempids["account"], existing_account);
    assert_eq!(memory.tempids["source"], existing_source);
    assert_eq!(partition_eid(memory.tempids["source"]).unwrap(), named);
    assert_eq!(partition_eid(memory.tempids["related"]).unwrap(), implicit);
    assert!(memory.tx_data.iter().all(|d| d.source == existing_source));
    assert!(!memory.tx_data.iter().any(|d| d.entity == existing_source));
    assert_eq!(
        memory.db_after.eidx_frontier(),
        memory_seed.db_after.eidx_frontier() + 1,
    );
    assert_eq!(
        overlay.db_after.eidx_frontier(),
        memory.db_after.eidx_frontier(),
    );
}

#[test]
fn source_schema_affinity_and_transaction_identity_remain_ordinary() {
    let (db, defaults, named) = named_database();
    let schema = db
        .with_edn_with_defaults(
            r#"[{:db/id "schema" :db/ident :item/title
                  :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
                {:db/force-partition {"schema" :part/orders}}]"#,
            2,
            EntityRef::Temp("schema".into()),
            &defaults,
        )
        .unwrap();
    assert_eq!(eid_to_part(schema.tempids["schema"]).unwrap(), DB_PARTITION);
    assert!(
        schema
            .tx_data
            .iter()
            .all(|d| d.source == schema.tempids["schema"])
    );

    let tx = schema
        .db_after
        .with_edn_with_defaults(
            r#"[{:db/id "tx-source" :db/doc "transaction contributor"}
                {:db/id "ordinary" :db/doc "ordinary"}
                {:db/force-partition {"tx-source" :db.part/tx}
                 :db/match-partition {"ordinary" "edb.tx"}}]"#,
            3,
            EntityRef::Temp("tx-source".into()),
            &defaults,
        )
        .unwrap();
    let current_tx = t_to_tx(tx.db_after.basis_t()).unwrap();
    assert_eq!(tx.tempids["tx-source"], current_tx);
    assert!(tx.tx_data.iter().all(|d| d.source == current_tx));
    assert_eq!(partition_eid(tx.tempids["ordinary"]).unwrap(), named);
    let current = tx
        .db_after
        .with(&[], 4, EntityRef::from_edn(r#""edb.tx""#).unwrap())
        .unwrap();
    assert_eq!(current.tx_data[0].source, t_to_tx(4).unwrap());
    let previous = current
        .db_after
        .with(&[], 5, EntityRef::Id(current_tx))
        .unwrap();
    assert_eq!(previous.tx_data[0].source, current_tx);
    assert_eq!(previous.tx_data[0].attribute, DB_TX_INSTANT as u32);
}

#[test]
fn policy_does_not_establish_source_and_invalid_source_ids_are_rejected() {
    let (db, defaults, _) = named_database();
    let error = db
        .with_edn_with_defaults(
            r#"[{:db/force-partition {"source" :part/orders}}]"#,
            2,
            EntityRef::Temp("source".into()),
            &defaults,
        )
        .unwrap_err();
    assert_eq!(error.code, "transaction/tempid-not-an-entity");
    let error = db
        .with(&[], 2, EntityRef::Id(edb_core::make_eid(500, 10).unwrap()))
        .unwrap_err();
    assert_eq!(error.code, "transaction/not-a-partition");
    let error = db
        .with(
            &[],
            2,
            EntityRef::Id(
                edb_core::make_eid(edb_core::USER_PARTITION, db.eidx_frontier()).unwrap(),
            ),
        )
        .unwrap_err();
    assert_eq!(error.code, "transaction/invalid-entity-id");
    let error = db
        .with_edn_with_defaults(
            r#"[{:db/id "source" :db/doc "contributor"}
                {:db/force-partition {"source" :part/orders}}
                {:db/force-partition {"source" :db.part/user}}]"#,
            2,
            EntityRef::Temp("source".into()),
            &defaults,
        )
        .unwrap_err();
    assert_eq!(error.code, "transaction/partition-conflict");
}
