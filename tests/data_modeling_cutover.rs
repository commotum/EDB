use edb_core::edn_pull::parse_pull_edn;
use edb_core::edn_query::{EdnQueryArgument, parse_query_edn};
use edb_core::{
    DB_SOURCE_SYSTEM, DB_TX_INSTANT, Database, DatabaseValue, EntityRef, EntityValue, IndexOrder,
    Keyword, QueryControl, QueryResult, QuerySourceValue, QueryValue, SpeculativeTransactionReport,
    Value,
};
use std::sync::Arc;

fn examples() -> Vec<&'static str> {
    include_str!("../docs/03_schema/04_data_modeling.md")
        .split("```edn\n")
        .skip(1)
        .map(|block| block.split("```").next().unwrap().trim())
        .collect()
}

fn attributed(report: &SpeculativeTransactionReport, source: u64) {
    assert!(report.tx_data.iter().all(|datom| datom.source == source));
    let instants: Vec<_> = report
        .tx_data
        .iter()
        .filter(|datom| u64::from(datom.attribute) == DB_TX_INSTANT)
        .collect();
    assert_eq!(instants.len(), 1);
    assert!(instants[0].added);
    assert_eq!(instants[0].entity, instants[0].tx);
}

fn initial() -> DatabaseValue {
    let source = Database::bootstrap()
        .unwrap()
        .database_value()
        .with_edn(
            "[{:db/id 99 :db/ident :source/catalog} {:db/id 98 :db/ident :source/curator}]",
            1,
            EntityRef::Id(DB_SOURCE_SYSTEM),
        )
        .unwrap()
        .db_after;
    let schema = source
        .with_edn(examples()[0], 2, EntityRef::Id(99))
        .unwrap();
    attributed(&schema, 99);
    schema.db_after
}

fn query(source: QuerySourceValue, text: &str, scan: bool) -> QueryResult {
    parse_query_edn(text)
        .unwrap()
        .bind(&[EdnQueryArgument::Source(source)])
        .unwrap()
        .execute(
            &QueryControl {
                force_scan: scan,
                ..QueryControl::default()
            },
            None,
        )
        .unwrap()
        .result
}

fn relation(rows: Vec<Vec<Value>>) -> QueryResult {
    QueryResult::Relation(
        rows.into_iter()
            .map(|row| row.into_iter().map(QueryValue::Scalar).collect())
            .collect(),
    )
}

fn keyword(namespace: &str, name: &str) -> Value {
    Value::Keyword(Keyword::new(namespace, name))
}

fn field<'a>(map: &'a QueryValue, namespace: &str, name: &str) -> &'a QueryValue {
    let QueryValue::Map(entries) = map else {
        panic!("expected Pull map")
    };
    let key = QueryValue::Scalar(keyword(namespace, name));
    &entries.iter().find(|(entry, _)| entry == &key).unwrap().1
}

fn artist(db: &DatabaseValue) -> u64 {
    let QueryResult::Relation(rows) = query(
        QuerySourceValue::Database(db.clone()),
        "[:find ?e :where [?e :artist/name \"Leonard Cohen\"]]",
        false,
    ) else {
        panic!("expected artist relation")
    };
    assert_eq!(rows.len(), 1);
    let QueryValue::Scalar(Value::Ref(entity)) = rows[0][0] else {
        panic!("expected artist reference")
    };
    entity
}

#[test]
fn exact_enumeration_examples_compose_through_transactions_queries_and_navigation() {
    let blocks = examples();
    assert_eq!(blocks.len(), 5);
    let schema = initial();
    let canada = schema.entid(&Keyword::new("country", "CA")).unwrap();
    let country =
        edb_core::schema_eid_to_attr_id(schema.entid(&Keyword::new("artist", "country")).unwrap())
            .unwrap();
    assert!(edb_core::schema_eid_to_attr_id(canada).is_err());
    assert!(!schema.schema().attribute(country).unwrap().component);
    let first = schema.with_edn(blocks[1], 3, EntityRef::Id(99)).unwrap();
    attributed(&first, 99);
    let entity = artist(&first.db_after);
    for scan in [false, true] {
        assert_eq!(
            query(
                QuerySourceValue::Database(first.db_after.clone()),
                blocks[2],
                scan
            ),
            relation(vec![vec![Value::String("Leonard Cohen".into())]])
        );
        assert_eq!(
            query(
                QuerySourceValue::Database(first.db_after.clone()),
                blocks[3],
                scan
            ),
            relation(vec![vec![
                Value::String("Leonard Cohen".into()),
                keyword("country", "CA")
            ]])
        );
    }
    let nested = first
        .db_after
        .pull(&parse_pull_edn(blocks[4]).unwrap(), entity)
        .unwrap();
    assert_eq!(
        field(field(&nested, "artist", "country"), "db", "ident"),
        &QueryValue::Scalar(keyword("country", "CA"))
    );
    let plain = first
        .db_after
        .pull(&parse_pull_edn("[:artist/country]").unwrap(), entity)
        .unwrap();
    assert_eq!(
        field(field(&plain, "artist", "country"), "db", "id"),
        &QueryValue::Scalar(Value::Ref(canada))
    );
    let Some(EntityValue::Scalar(country_keyword)) = first
        .db_after
        .entity(entity)
        .unwrap()
        .unwrap()
        .get(country)
        .unwrap()
    else {
        panic!("identified forward ref presents a keyword")
    };
    assert_eq!(country_keyword, keyword("country", "CA"));
    let second = first
        .db_after
        .with_edn(
            &format!("[{{:db/id {entity} :artist/country :country/CA}}]"),
            4,
            EntityRef::Id(98),
        )
        .unwrap();
    attributed(&second, 98);
    let contributions: Vec<_> = second
        .db_after
        .collect_datoms(IndexOrder::Eavt)
        .unwrap()
        .into_iter()
        .filter(|d| d.entity == entity && d.attribute == country)
        .collect();
    assert_eq!(contributions.len(), 2);
    assert_eq!(
        second
            .db_after
            .pull(&parse_pull_edn(blocks[4]).unwrap(), entity)
            .unwrap(),
        nested
    );
    let replacement = second
        .db_after
        .with_edn(
            &format!("[{{:db/id {entity} :artist/country :country/JP}}]"),
            5,
            EntityRef::Id(99),
        )
        .unwrap();
    attributed(&replacement, 99);
    assert_eq!(
        replacement
            .tx_data
            .iter()
            .filter(|d| d.attribute == country && !d.added)
            .count(),
        1
    );
    assert!(
        query(
            QuerySourceValue::Database(replacement.db_after.clone().filter(|_, d| d.source == 98)),
            blocks[2],
            false
        ) == relation(vec![])
    );
    assert_eq!(
        query(
            QuerySourceValue::Database(replacement.db_after.as_of(second.db_after.basis_t())),
            blocks[2],
            false
        ),
        relation(vec![vec![Value::String("Leonard Cohen".into())]])
    );

    // Raw tuples preserve literal keywords; only a database source has ref schema.
    let raw = QuerySourceValue::Tuples(Arc::new(vec![vec![
        Value::Ref(entity),
        keyword("artist", "country"),
        Value::Ref(canada),
    ]]));
    assert_eq!(
        query(
            raw,
            "[:find ?e :where [?e :artist/country :country/CA]]",
            false
        ),
        relation(vec![])
    );
    let raw = QuerySourceValue::Tuples(Arc::new(vec![vec![
        Value::Ref(entity),
        keyword("artist", "country"),
        keyword("country", "CA"),
    ]]));
    assert_eq!(
        query(
            raw,
            "[:find ?e :where [?e :artist/country :country/CA]]",
            false
        ),
        relation(vec![vec![Value::Ref(entity)]])
    );
}

#[test]
fn db_before_binding_and_representation_choice_remain_ordinary() {
    let schema = initial();
    for text in [
        "[{:artist/name \"Unknown country\" :artist/country :country/ZZ}]",
        "[{:db/ident :country/ZZ} {:artist/name \"New country\" :artist/country :country/ZZ}]",
    ] {
        assert_eq!(
            schema
                .with_edn(text, 3, EntityRef::Id(99))
                .unwrap_err()
                .code,
            "transaction/unknown-ident"
        );
    }
    assert_eq!(schema.with_edn(
        "[{:db/ident :artist/new-name :db/valueType :db.type/string :db/cardinality :db.cardinality/one} {:artist/new-name \"New name\"}]",
        3,
        EntityRef::Id(99),
    ).unwrap_err().code, "schema/unknown-attribute");
    let added = schema.with_edn(
        "[{:db/ident :genre/folk} {:db/ident :artist/choice :db/valueType :db.type/keyword :db/cardinality :db.cardinality/one} {:db/ident :artist/countries :db/valueType :db.type/ref :db/cardinality :db.cardinality/many}]",
        3,
        EntityRef::Id(99),
    ).unwrap().db_after;
    let flexible = added.with_edn(
        "[{:db/id 500 :artist/name \"Choice\" :artist/country :genre/folk :artist/choice :country/unregistered :artist/countries [:country/CA :country/JP]} {:artist/name \"Choice\"}]",
        4,
        EntityRef::Id(98),
    ).unwrap();
    attributed(&flexible, 98);
    assert!(
        flexible
            .db_after
            .entid(&Keyword::new("country", "unregistered"))
            .is_none()
    );
    assert_eq!(
        query(
            QuerySourceValue::Database(flexible.db_after.clone()),
            "[:find ?v :where [500 :artist/choice ?v]]",
            false
        ),
        relation(vec![vec![keyword("country", "unregistered")]])
    );
    assert_eq!(
        query(
            QuerySourceValue::Database(flexible.db_after.clone()),
            "[:find ?v :where [500 :artist/countries ?c] [?c :db/ident ?v]]",
            false
        ),
        relation(vec![
            vec![keyword("country", "CA")],
            vec![keyword("country", "JP")]
        ])
    );
    let QueryResult::Relation(names) = query(
        QuerySourceValue::Database(flexible.db_after),
        "[:find ?e :where [?e :artist/name \"Choice\"]]",
        false,
    ) else {
        panic!("expected relation")
    };
    assert_eq!(names.len(), 2);
}

#[test]
fn enumeration_rename_keeps_reference_identity_and_noncomponent_country_survives() {
    let first = initial()
        .with_edn(examples()[1], 3, EntityRef::Id(99))
        .unwrap()
        .db_after;
    let entity = artist(&first);
    let canada = first.entid(&Keyword::new("country", "CA")).unwrap();
    let renamed = first
        .with_edn(
            "[{:db/id :country/CA :db/ident :country/Canada}]",
            4,
            EntityRef::Id(98),
        )
        .unwrap();
    attributed(&renamed, 98);
    assert_eq!(
        renamed.db_after.entid(&Keyword::new("country", "Canada")),
        Some(canada)
    );
    assert_eq!(
        renamed.db_after.ident(canada),
        Some(&Keyword::new("country", "Canada"))
    );
    assert_eq!(first.ident(canada), Some(&Keyword::new("country", "CA")));
    let plain = renamed
        .db_after
        .pull(&parse_pull_edn("[:artist/country]").unwrap(), entity)
        .unwrap();
    assert_eq!(
        field(field(&plain, "artist", "country"), "db", "id"),
        &QueryValue::Scalar(Value::Ref(canada))
    );
    let nested = renamed
        .db_after
        .pull(&parse_pull_edn(examples()[4]).unwrap(), entity)
        .unwrap();
    assert_eq!(
        field(field(&nested, "artist", "country"), "db", "ident"),
        &QueryValue::Scalar(keyword("country", "Canada"))
    );
    let withdrawn = renamed
        .db_after
        .with_edn(
            &format!("[[:db/retractEntity {entity}]]"),
            5,
            EntityRef::Id(99),
        )
        .unwrap();
    attributed(&withdrawn, 99);
    let current = withdrawn.db_after.collect_datoms(IndexOrder::Eavt).unwrap();
    assert!(!current.iter().any(|d| d.entity == entity));
    assert!(
        current
            .iter()
            .any(|d| d.entity == canada && d.value == keyword("country", "Canada"))
    );
    assert!(withdrawn.tx_data.iter().all(|d| d.entity != canada));
}
