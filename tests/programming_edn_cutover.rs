use edb_core::edn::{
    EdnLimits, EdnReadOptions, EdnReader, EdnUnknownTagPolicy, EdnValue, read_edn,
    read_edn_with_options, write_edn, write_edn_with_limits,
};
use edb_core::edn_query::{EdnQueryArgument, parse_query_edn, query_from_edn};
use edb_core::edn_value::{edn_to_query_value, edn_to_value, value_to_edn};
use edb_core::{Database, EntityRef, Keyword, QueryControl, QuerySourceValue, Symbol, Value};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[test]
fn overview_data_construction_transaction_and_query_run_through_public_apis() {
    let text = r#"[{:person/name "Anna" :person/email "anna@example.com"}]"#;
    let parsed = read_edn(text).unwrap();
    let constructed = EdnValue::Vector(vec![EdnValue::Map(vec![
        (
            EdnValue::Keyword(Keyword::new("person", "name")),
            EdnValue::String("Anna".into()),
        ),
        (
            EdnValue::Keyword(Keyword::new("person", "email")),
            EdnValue::String("anna@example.com".into()),
        ),
    ])]);
    assert_eq!(parsed, constructed);
    assert_eq!(
        read_edn(&write_edn(&constructed).unwrap()).unwrap(),
        constructed
    );

    let installed = Database::bootstrap().unwrap().with_edn(
        r#"[{:db/ident :person/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
            {:db/ident :person/email :db/valueType :db.type/string :db/cardinality :db.cardinality/one :db/unique :db.unique/identity}
            {:db/ident :community/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}]"#,
        1, EntityRef::Ident(Keyword::new("db.source", "system")),
    ).unwrap();
    let report = installed
        .db_after
        .database_value()
        .with_edn(text, 2, EntityRef::Id(99))
        .unwrap();
    assert!(report.tx_data.iter().all(|d| d.source == 99));
    assert_eq!(report.tx_data.len(), 3);
    let timestamp = report
        .db_after
        .entid(&Keyword::new("db", "txInstant"))
        .unwrap() as u32;
    assert_eq!(
        report
            .tx_data
            .iter()
            .filter(|d| d.attribute == timestamp)
            .count(),
        1
    );

    let community = report
        .db_after
        .with_edn(
            r#"[{:db/id "community" :community/name "belltown"}]"#,
            3,
            EntityRef::from_edn("99").unwrap(),
        )
        .unwrap();
    let query_text = r#"[:find ?c :where [?c :community/name "belltown"]]"#;
    for query in [
        parse_query_edn(query_text).unwrap(),
        query_from_edn(&read_edn(query_text).unwrap()).unwrap(),
    ] {
        let bound = query
            .bind(&[EdnQueryArgument::Source(QuerySourceValue::Database(
                community.db_after.clone(),
            ))])
            .unwrap();
        let result = bound.execute(&QueryControl::default(), None).unwrap();
        let output = bound.result_to_edn(&result.result).unwrap();
        assert_eq!(
            output,
            read_edn(&format!(
                "#{{[#edb/ref {}]}}",
                community.tempids["community"]
            ))
            .unwrap()
        );
    }
    assert_eq!(installed.db_after.basis_t(), 1);
}

#[test]
fn overview_numeric_and_precision_boundaries_retain_their_domains() {
    for text in [
        "42",
        "+42",
        "-42",
        "42N",
        "+42N",
        "-42N",
        "9223372036854775808N",
        "3.14",
        "-3.14",
        "1e3",
        "3.14M",
        "-3.14M",
        "42M",
        "1e100M",
    ] {
        let parsed = read_edn(text).unwrap();
        assert_eq!(read_edn(&write_edn(&parsed).unwrap()).unwrap(), parsed);
        let stored = edn_to_value(&parsed).unwrap();
        assert_eq!(
            edn_to_value(&value_to_edn(&stored).unwrap()).unwrap(),
            stored
        );
        if let EdnValue::BigDec(original) = parsed {
            let EdnValue::BigDec(printed) =
                read_edn(&write_edn(&EdnValue::BigDec(original.clone())).unwrap()).unwrap()
            else {
                unreachable!()
            };
            assert_eq!(
                printed.as_bigint_and_scale(),
                original.as_bigint_and_scale()
            );
        }
    }
    assert!(matches!(
        edn_to_value(&read_edn("3.14").unwrap()).unwrap(),
        Value::Double(_)
    ));
    assert!(matches!(
        edn_to_value(&read_edn(r#"#edb/float "3.13""#).unwrap()).unwrap(),
        Value::Float(_)
    ));
    for text in ["9223372036854775808", "1e999", "##Inf", "##NaN"] {
        assert!(read_edn(text).is_err(), "{text}");
    }
    for text in [
        r#"#edb/float "1e999""#,
        r#"#edb/float "1e-999""#,
        r#"#edb/float 1e-100"#,
    ] {
        assert!(edn_to_value(&read_edn(text).unwrap()).is_err(), "{text}");
    }
    for text in [
        r#"#inst "1985-04-12T23:20:50.5201Z""#,
        r#"#inst "2016-12-31T23:59:60Z""#,
    ] {
        let parsed = read_edn(text).unwrap();
        assert_eq!(read_edn(&write_edn(&parsed).unwrap()).unwrap(), parsed);
        assert!(edn_to_value(&parsed).is_err(), "{text}");
    }
    assert!(edn_to_value(&read_edn(r#"#inst "1985-04-12T23:20:50.52Z""#).unwrap()).is_ok());
    let map = read_edn(r#"{42 "long" 42N "bigint"}"#).unwrap();
    assert_eq!(
        edn_to_query_value(&map).unwrap_err().code,
        "edn/query-key-collision"
    );
}

#[test]
fn overview_collections_and_nonstored_scalars_remain_ordinary_query_data() {
    for text in [
        r#""this is a string""#,
        r"\f",
        r"\tab",
        r"\return",
        r"\u001B",
        "true",
        "false",
        r#"#uuid "f81d4fae-7dec-11d0-a765-00a0c91e6bf6""#,
        "?c",
        "my-query-rules",
        ":namespace/foo",
        "nil",
        "(1 2 3)",
        "[a b 42]",
        "{:a 100 :b 90}",
        r#"{:a 1, "foo" :bar, [1 2 3] four}"#,
        "#{:red :green :blue}",
    ] {
        let parsed = read_edn(text).unwrap();
        assert_eq!(read_edn(&write_edn(&parsed).unwrap()).unwrap(), parsed);
        assert!(edn_to_query_value(&parsed).is_ok(), "{text}");
    }
    for text in ["nil", r"\f", "{}", "#{1 2}", "[]", "#custom/data [1 2]"] {
        let parsed = read_edn(text).unwrap();
        assert!(edn_to_query_value(&parsed).is_ok());
        assert!(edn_to_value(&parsed).is_err(), "{text}");
    }
    assert_eq!(
        edn_to_value(&read_edn("[1 nil]").unwrap()).unwrap(),
        Value::Tuple(vec![Some(Value::Long(1)), None])
    );
    assert_eq!(read_edn("[1 2]").unwrap(), read_edn("(1,2)").unwrap());
    assert_eq!(
        read_edn("{:a 1 :b 2}").unwrap(),
        read_edn("{:b 2 :a 1}").unwrap()
    );
    assert_eq!(read_edn("#{1 2}").unwrap(), read_edn("#{2 1}").unwrap());
    for text in ["{:a 1 :a 2}", "#{1 1}", "#{[1 2] (1 2)}", "'x", "::foo"] {
        assert!(read_edn(text).is_err(), "{text}");
    }
}

#[test]
fn overview_reader_stream_tags_and_limits_have_explicit_boundaries() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut options = EdnReadOptions {
        unknown_tags: EdnUnknownTagPolicy::Reject,
        ..Default::default()
    };
    options.tag_handlers.insert(Symbol::new("custom", "data"), {
        let calls = calls.clone();
        Arc::new(move |value| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(value)
        })
    });
    let inert = read_edn("#custom/data (dangerous-call)").unwrap();
    assert!(matches!(inert, EdnValue::Tagged(_, _)));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(read_edn_with_options("#unknown/data 1", &options).is_err());
    assert_eq!(
        read_edn_with_options("#custom/data [1 2]", &options).unwrap(),
        read_edn("[1 2]").unwrap()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(read_edn_with_options("1 #custom/data 2", &options).is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        EdnReader::new("1, ; first\n 2")
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap(),
        vec![EdnValue::Long(1), EdnValue::Long(2)]
    );
    assert!(read_edn("1 2").is_err());
    let mut stream = EdnReader::new("1 ] 2").unwrap();
    assert!(stream.next().unwrap().is_ok());
    assert!(stream.next().unwrap().is_err());
    assert!(stream.next().is_none());
    for limits in [
        EdnLimits {
            max_input_bytes: 2,
            ..Default::default()
        },
        EdnLimits {
            max_depth: 0,
            ..Default::default()
        },
        EdnLimits {
            max_nodes: 1,
            ..Default::default()
        },
        EdnLimits {
            max_work: 1,
            ..Default::default()
        },
    ] {
        let options = EdnReadOptions {
            limits,
            ..Default::default()
        };
        assert!(read_edn_with_options("[1 2]", &options).is_err());
    }
    assert!(
        write_edn_with_limits(
            &EdnValue::Long(42),
            EdnLimits {
                max_output_bytes: 1,
                ..Default::default()
            }
        )
        .is_err()
    );
}
