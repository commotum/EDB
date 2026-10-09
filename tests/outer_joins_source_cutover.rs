use edb_core::edn::{EdnValue, read_edn};
use edb_core::edn_query::{BoundEdnQuery, EdnQueryArgument, parse_query_edn};
use edb_core::{
    Attribute, Cardinality, Database, DatabaseValue, EntityRef, Keyword, QueryControl,
    QuerySourceValue, Schema, SemanticError, TxOp, TxValue, Value, ValueType,
};

const PULL: &str = r#"[:find (pull ?e details-expr)
        :in $ details-expr
        :where [?e :artist/name]]"#;
const GET_ELSE: &str = r#"[:find ?e ?name ?year
 :where
 [?e :artist/name ?name]
 [(get-else $ ?e :artist/startYear "Unknown") ?year]]"#;
const MISSING: &str = r#"[:find ?name
 :where
 [?e :artist/name ?name]
 [(missing? $ ?e :artist/startYear)]]"#;
const GET_SOME: &str = r#"[:find ?name ?attribute ?year
 :where
 [?e :artist/name ?name]
 [(get-some $ ?e :artist/startYear) [?attribute ?year]]]"#;

fn add(entity: u64, attribute: u32, value: Value) -> TxOp {
    TxOp::Add {
        entity: EntityRef::Id(entity),
        attribute,
        value: TxValue::Scalar(value),
    }
}

fn artists() -> DatabaseValue {
    let mut schema = Schema::new();
    for (id, name, value_type, cardinality) in [
        (100, "name", ValueType::String, Cardinality::One),
        (101, "startYear", ValueType::Long, Cardinality::One),
        (102, "tags", ValueType::String, Cardinality::Many),
    ] {
        schema
            .install(Attribute::new(
                id,
                Keyword::new("artist", name),
                value_type,
                cardinality,
            ))
            .unwrap();
    }
    let facts = [
        add(42, 100, Value::String("Known Artist".into())),
        add(42, 101, Value::Long(1968)),
        add(43, 100, Value::String("Unknown Artist".into())),
    ];
    Database::new(schema)
        .unwrap()
        .with(&facts, 1, EntityRef::Id(99))
        .unwrap()
        .db_after
        .database_value()
        .with(&facts, 2, EntityRef::Id(98))
        .unwrap()
        .db_after
}

fn bind(db: &DatabaseValue, text: &str) -> BoundEdnQuery {
    let mut arguments = vec![EdnQueryArgument::Source(QuerySourceValue::Database(
        db.clone(),
    ))];
    if text == PULL {
        arguments.push(EdnQueryArgument::Data(
            read_edn("[:artist/name :artist/startYear]").unwrap(),
        ));
    }
    parse_query_edn(text).unwrap().bind(&arguments).unwrap()
}

fn run(db: &DatabaseValue, text: &str, force_scan: bool) -> Result<EdnValue, SemanticError> {
    let bound = bind(db, text);
    let outcome = bound.execute(
        &QueryControl {
            force_scan,
            ..QueryControl::default()
        },
        None,
    )?;
    bound.result_to_edn(&outcome.result)
}

fn expect(db: &DatabaseValue, text: &str, expected: &str) {
    for force_scan in [false, true] {
        assert_eq!(
            run(db, text, force_scan).unwrap(),
            read_edn(expected).unwrap(),
            "query: {text}, force_scan: {force_scan}"
        );
    }
}

#[test]
fn exact_artist_examples_preserve_missing_rows_and_distinct_values() {
    let db = artists();
    expect(
        &db,
        PULL,
        r#"#{[{:artist/name "Known Artist" :artist/startYear 1968}]
             [{:artist/name "Unknown Artist"}]}"#,
    );
    expect(
        &db,
        GET_ELSE,
        r#"#{[#edb/ref 42 "Known Artist" 1968] [#edb/ref 43 "Unknown Artist" "Unknown"]}"#,
    );
    expect(&db, MISSING, r#"#{["Unknown Artist"]}"#);
    expect(&db, GET_SOME, r#"#{["Known Artist" #edb/ref 101 1968]}"#);
    expect(
        &db,
        r#"[:find ?name ?attribute ?value :where
            [?e :artist/name ?name]
            [(get-some $ ?e :artist/startYear :artist/name) [?attribute ?value]]]"#,
        r#"#{["Known Artist" #edb/ref 101 1968] ["Unknown Artist" #edb/ref 100 "Unknown Artist"]}"#,
    );
    // The named query source chooses a database. The fifth datom position
    // independently selects attribution and may be included in the find basis.
    expect(
        &db,
        r#"[:find ?name ?s ?year :in $artists :where
            [$artists ?e :artist/name ?name _ ?s]
            [(get-else $artists ?e :artist/startYear "Unknown") ?year]]"#,
        r#"#{["Known Artist" #edb/ref 98 1968] ["Known Artist" #edb/ref 99 1968]
             ["Unknown Artist" #edb/ref 98 "Unknown"] ["Unknown Artist" #edb/ref 99 "Unknown"]}"#,
    );
}

#[test]
fn outer_join_functions_and_pull_honor_global_withdrawal_before_source_filtering() {
    let before = artists();
    let after = before
        .with(
            &[TxOp::Retract {
                entity: EntityRef::Id(42),
                attribute: 101,
                value: Some(TxValue::Scalar(Value::Long(1968))),
            }],
            3,
            EntityRef::Id(97),
        )
        .unwrap()
        .db_after;
    for view in [after.clone(), after.clone().filter(|_, d| d.source == 99)] {
        expect(
            &view,
            PULL,
            r#"#{[{:artist/name "Known Artist"}] [{:artist/name "Unknown Artist"}]}"#,
        );
        expect(
            &view,
            GET_ELSE,
            r#"#{[#edb/ref 42 "Known Artist" "Unknown"] [#edb/ref 43 "Unknown Artist" "Unknown"]}"#,
        );
        expect(&view, MISSING, r#"#{["Known Artist"] ["Unknown Artist"]}"#);
        expect(&view, GET_SOME, "#{}");
    }
    let captured = after.as_of(before.basis_t()).filter(|_, d| d.source == 99);
    expect(
        &captured,
        GET_ELSE,
        r#"#{[#edb/ref 42 "Known Artist" 1968] [#edb/ref 43 "Unknown Artist" "Unknown"]}"#,
    );
    expect(&captured, MISSING, r#"#{["Unknown Artist"]}"#);
    expect(
        &captured,
        GET_SOME,
        r#"#{["Known Artist" #edb/ref 101 1968]}"#,
    );
    expect(
        &captured,
        PULL,
        r#"#{[{:artist/name "Known Artist" :artist/startYear 1968}]
             [{:artist/name "Unknown Artist"}]}"#,
    );
}

#[test]
fn optional_attribute_functions_keep_their_documented_validation() {
    let db = artists();
    let unknown_selector = parse_query_edn(PULL)
        .unwrap()
        .bind(&[
            EdnQueryArgument::Source(QuerySourceValue::Database(db.clone())),
            EdnQueryArgument::Data(read_edn("[:artist/name :artist/unknown]").unwrap()),
        ])
        .unwrap();
    for force_scan in [false, true] {
        let outcome = unknown_selector
            .execute(
                &QueryControl {
                    force_scan,
                    ..QueryControl::default()
                },
                None,
            )
            .unwrap();
        assert_eq!(
            unknown_selector.result_to_edn(&outcome.result).unwrap(),
            read_edn(r#"#{[{:artist/name "Known Artist"}] [{:artist/name "Unknown Artist"}]}"#)
                .unwrap()
        );
    }
    for (expression, code) in [
        (
            "[(get-else $ ?e :artist/startYear nil) ?value]",
            "query/get-else-nil-default",
        ),
        (
            "[(get-else $ ?e :artist/tags \"Unknown\") ?value]",
            "query/get-else-cardinality",
        ),
        (
            "[(get-some $ ?e :artist/tags) [?attribute ?value]]",
            "query/get-some-cardinality",
        ),
        (
            "[(get-else $ ?e :artist/unknown \"Unknown\") ?value]",
            "query/unknown-attribute",
        ),
        (
            "[(get-some $ ?e :artist/unknown) [?attribute ?value]]",
            "query/unknown-attribute",
        ),
        (
            "[(missing? $ ?e :artist/unknown)] [(ground true) ?value]",
            "query/unknown-attribute",
        ),
    ] {
        let query = format!("[:find ?value :where [?e :artist/name] {expression}]");
        for force_scan in [false, true] {
            assert_eq!(run(&db, &query, force_scan).unwrap_err().code, code);
        }
    }
}
