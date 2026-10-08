use edb_core::edn::{read_edn, write_edn};
use edb_core::edn_query::{EdnQueryArgument, parse_query_edn};
use edb_core::edn_value::{edn_to_value, transaction_report_to_edn, value_to_edn};
use edb_core::{Database, Keyword, QueryControl, QueryResult, QueryValue, Value, t_to_tx};

#[test]
fn edn_tags_preserve_stored_values() {
    for input in [
        r#"#edb/ref 42"#,
        r#"#edb/bytes "00ff""#,
        r#"#edb/uri "https://example.com/resource""#,
        r#"#edb/function "0000000000000000000000000000000000000000000000000000000000000000""#,
        r#"#edb/float "1.5""#,
        r#"#edb/float-bits "7fc00001""#,
        r#"#edb/double-bits "7ff8000000000001""#,
        r#"#edb/instant-millis 9223372036854775807"#,
    ] {
        let value = edn_to_value(&read_edn(input).unwrap()).unwrap();
        let encoded = write_edn(&value_to_edn(&value).unwrap()).unwrap();
        assert!(encoded.starts_with("#edb/"), "{encoded}");
        let decoded = edn_to_value(&read_edn(&encoded).unwrap()).unwrap();
        // Compare serialized forms so NaN payloads and numeric types are preserved.
        assert_eq!(
            write_edn(&value_to_edn(&decoded).unwrap()).unwrap(),
            encoded
        );
    }
}

#[test]
fn transaction_entity_and_report_use_edb_names() {
    let database = Database::bootstrap().unwrap();
    let report = database
        .with_edn(
            r#"[{:db/id "edb.tx" :db/doc "observed by an agent"}]"#,
            1000,
            edb_core::EntityRef::Id(edb_core::DB_SOURCE_SYSTEM),
        )
        .unwrap();
    let tx = t_to_tx(report.db_after.basis_t()).unwrap();
    let doc = report.db_after.entid(&Keyword::new("db", "doc")).unwrap() as u32;
    assert_eq!(
        report.db_after.values(tx, doc),
        vec![&Value::String("observed by an agent".into())]
    );
    assert!(report.tempids.is_empty());
    let output = transaction_report_to_edn(
        report.db_before.basis_t(),
        report.db_after.basis_t(),
        &report.tx_data,
        &report.tempids,
    )
    .unwrap();
    let text = write_edn(&output).unwrap();
    for key in ["db-before-t", "db-after-t", "tx-data", "tempids"] {
        assert!(text.contains(&format!(":edb/{key}")), "{text}");
    }
}

#[test]
fn qualified_subquery_executes_against_application_data() {
    let query = parse_query_edn(
        r#"[:find ?name
            :in $rows
            :where [(edb.api/q [:find ?n :in $r :where [$r ?n]] $rows) [[?name]]]]"#,
    )
    .unwrap();
    let bound = query
        .bind(&[EdnQueryArgument::Data(read_edn(r#"[["Ada"]]"#).unwrap())])
        .unwrap();
    let result = bound.execute(&QueryControl::default(), None).unwrap();
    assert_eq!(
        result.result,
        QueryResult::Relation(vec![vec![QueryValue::Scalar(Value::String("Ada".into()))]])
    );
}

#[test]
fn storage_headers_round_trip_and_authenticate_content() {
    use edb_core::storage::root::Block;
    use edb_core::{IndexOrder, View, decode_genesis, encode_genesis, sha256};

    let datoms = Database::bootstrap()
        .unwrap()
        .datoms(View::Current, IndexOrder::Eavt);
    let genesis = encode_genesis(&datoms).unwrap();
    assert!(genesis.starts_with(b"EDBC"));
    assert_eq!(decode_genesis(&genesis).unwrap(), datoms);

    let block = Block {
        kind: 3,
        links: vec![sha256(&genesis)],
        payload: genesis,
    };
    let mut encoded = block.encode().unwrap();
    assert!(encoded.starts_with(b"EDBO"));
    let hash = sha256(&encoded);
    assert_eq!(Block::decode(&hash, &encoded).unwrap(), block);
    *encoded.last_mut().unwrap() ^= 1;
    assert!(Block::decode(&hash, &encoded).is_err());
}

#[test]
fn cli_runs_queries_without_database_configuration() {
    use std::process::Command;

    let binary = env!("CARGO_BIN_EXE_edb");
    let version = Command::new(binary).arg("--version").output().unwrap();
    assert!(version.status.success());
    assert_eq!(
        String::from_utf8(version.stdout).unwrap().trim(),
        format!("edb {}", env!("CARGO_PKG_VERSION"))
    );
    let help = Command::new(binary).arg("--help").output().unwrap();
    assert!(help.status.success());
    assert!(
        String::from_utf8(help.stdout)
            .unwrap()
            .contains("EDB_POSTGRES_URL")
    );

    let directory = tempfile::tempdir().unwrap();
    let query = directory.path().join("query.edn");
    let sources = directory.path().join("sources.edn");
    std::fs::write(&query, "[:find ?name :in $rows :where [$rows ?name]]").unwrap();
    std::fs::write(&sources, r#"{$rows [["Ada"]]}"#).unwrap();
    let output = Command::new(binary)
        .arg("query")
        .arg("--file")
        .arg(&query)
        .arg("--sources")
        .arg(&sources)
        .env_remove("EDB_POSTGRES_URL")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        read_edn(text.trim()).unwrap(),
        read_edn(r#"#{["Ada"]}"#).unwrap()
    );
}

#[test]
fn cli_requires_a_structural_source_before_opening_storage() {
    use std::process::Command;
    let directory = tempfile::tempdir().unwrap();
    let transaction = directory.path().join("transaction.edn");
    std::fs::write(&transaction, "[]").unwrap();
    for command in ["transact", "with"] {
        for source in [None, Some("true"), Some("[]")] {
            let mut request = Command::new(env!("CARGO_BIN_EXE_edb"));
            request.args([
                command,
                "--database",
                "unopened-source-admission-test",
                "--file",
            ]);
            request.arg(&transaction);
            if command == "transact" {
                request.args(["--request-key", "test", "--endpoint"]);
                request.arg(directory.path().join("absent.sock"));
            }
            if let Some(source) = source {
                request.args(["--source", source]);
            }
            let output = request.env_remove("EDB_POSTGRES_URL").output().unwrap();
            assert!(!output.status.success());
            let error = String::from_utf8(output.stderr).unwrap();
            let expected = if source.is_none() {
                "cli/edn-usage"
            } else {
                "edn/entity"
            };
            assert!(error.contains(expected), "{command} {source:?}: {error}");
            assert!(!error.contains("config/environment"), "{error}");
        }
    }
}
