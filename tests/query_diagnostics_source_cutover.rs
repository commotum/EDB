use edb_core::edn::{read_edn, write_edn};
use edb_core::edn_query::{BoundEdnQuery, EdnQueryArgument, parse_query_edn};
use edb_core::{
    Attribute, Cardinality, Database, DatabaseValue, EntityRef, Keyword, QueryControl,
    QueryDiagnosticOptions, QueryOutcome, QuerySourceValue, QueryStepStatus, QueryWarning, Schema,
    TxOp, TxValue, Value, ValueType, query_diagnostics_to_edn,
};

fn database() -> DatabaseValue {
    let mut schema = Schema::new();
    let mut indexed = Attribute::new(
        100,
        Keyword::new("item", "tag"),
        ValueType::String,
        Cardinality::One,
    );
    indexed.indexed = true;
    schema.install(indexed).unwrap();
    schema
        .install(Attribute::new(
            101,
            Keyword::new("item", "plain"),
            ValueType::String,
            Cardinality::One,
        ))
        .unwrap();
    let add = |entity, attribute, text: &str| TxOp::Add {
        entity: EntityRef::Id(entity),
        attribute,
        value: TxValue::Scalar(Value::String(text.into())),
    };
    let first = Database::new(schema)
        .unwrap()
        .with(
            &[
                add(42, 100, "payload-secret"),
                add(42, 101, "payload-secret"),
                add(43, 100, "z"),
            ],
            1,
            EntityRef::Id(99),
        )
        .unwrap();
    first
        .db_after
        .with(&[add(42, 100, "payload-secret")], 2, EntityRef::Id(98))
        .unwrap()
        .db_after
        .database_value()
}

fn bound(db: &DatabaseValue, pattern: &str) -> BoundEdnQuery {
    parse_query_edn(&format!("[:find ?e :in $catalog ?s :where {pattern}]"))
        .unwrap()
        .bind(&[
            EdnQueryArgument::Source(QuerySourceValue::Database(db.clone())),
            EdnQueryArgument::Data(read_edn("99").unwrap()),
        ])
        .unwrap()
}

fn captured(bound: &BoundEdnQuery, force_scan: bool) -> QueryOutcome {
    bound
        .execute(
            &QueryControl {
                diagnostics: Some(QueryDiagnosticOptions::default()),
                force_scan,
                ..Default::default()
            },
            None,
        )
        .unwrap()
}

#[test]
fn source_index_reports_use_existing_counters_and_named_source_labels() {
    let db = database();
    for (pattern, access) in [
        (
            "[$catalog ?e :item/plain \"payload-secret\" _ ?s]",
            "SAEVT seek",
        ),
        (
            "[$catalog ?e :item/tag \"payload-secret\" _ ?s]",
            "SAVET seek",
        ),
        (
            "[$catalog ?e :item/tag ?v _ ?s] [(> ?v \"a\")] [(< ?v \"r\")]",
            "SAVET range",
        ),
    ] {
        let query = bound(&db, pattern);
        let plain = query.execute(&QueryControl::default(), None).unwrap();
        let outcome = captured(&query, false);
        assert_eq!(outcome.result, plain.result);
        assert_eq!(outcome.stats, plain.stats);
        let report = outcome.diagnostics.as_ref().unwrap();
        assert!(!report.truncated);
        let step = &report.steps[0];
        assert_eq!(step.source.as_deref(), Some("$catalog"));
        assert_eq!(step.access, access);
        assert_eq!(step.clause_path, "where/0");
        assert_eq!(step.rows_in, 1);
        assert_eq!(step.rows_out, Some(1));
        assert_eq!(step.status, QueryStepStatus::Complete);
        assert!(step.bindings_complete);
        assert!(step.binds_in.contains(&"?s".to_owned()));
        assert!(step.binds_out.contains(&"?e".to_owned()));
        assert!(!step.warnings.contains(&QueryWarning::UnboundPattern));
        assert!(!step.warnings.contains(&QueryWarning::FullScan));
        assert!(step.work.index_seeks > 0);
        assert!(step.work.datoms_examined > 0);
        assert_eq!(step.work.join_candidates, 1);
        // These queries have no nested phases. Input binding and result
        // shaping also charge query-wide work outside scheduled clauses.
        assert_eq!(
            report.steps.iter().map(|s| s.work.index_seeks).sum::<u64>(),
            outcome.stats.index_seeks
        );
        assert!(
            report
                .steps
                .iter()
                .map(|s| s.work.datoms_examined)
                .sum::<u64>()
                <= outcome.stats.datoms_examined
        );
        assert!(
            report
                .steps
                .iter()
                .map(|s| s.work.join_candidates)
                .sum::<u64>()
                <= outcome.stats.join_candidates
        );
        assert!(
            report
                .steps
                .iter()
                .map(|s| s.work.allocated_value_bytes)
                .sum::<usize>()
                <= outcome.stats.allocated_value_bytes
        );
        for plan in &outcome.plan {
            let step = &report.steps[plan.diagnostic_step.unwrap()];
            assert_eq!(step.access, plan.access);
            assert_eq!(step.rows_out, Some(plan.rows_after));
        }
        let metadata = write_edn(&query_diagnostics_to_edn(report)).unwrap();
        assert!(metadata.contains(":query/source \"$catalog\""));
        assert!(!metadata.contains("payload-secret"));
        let scan = captured(&query, true);
        assert_eq!(scan.result, outcome.result);
        let scan_step = &scan.diagnostics.as_ref().unwrap().steps[0];
        assert_eq!(scan_step.access, "EAVT scan");
        assert!(scan_step.warnings.contains(&QueryWarning::FullScan));
        assert!(scan_step.work.datoms_examined >= step.work.datoms_examined);
    }
}

#[test]
fn capture_limits_preserve_source_query_results_stats_and_resource_failures() {
    let query = bound(
        &database(),
        "[$catalog ?e :item/tag ?v _ ?s] [(< ?v \"r\")]",
    );
    let baseline = query.execute(&QueryControl::default(), None).unwrap();
    for diagnostics in [
        None,
        Some(QueryDiagnosticOptions::default()),
        Some(QueryDiagnosticOptions {
            max_steps: 0,
            max_bytes: usize::MAX,
        }),
        Some(QueryDiagnosticOptions {
            max_steps: usize::MAX,
            max_bytes: 0,
        }),
        Some(QueryDiagnosticOptions {
            max_steps: 1,
            max_bytes: 1024 * 1024,
        }),
    ] {
        let control = QueryControl {
            diagnostics,
            max_work: baseline.stats.work as usize,
            max_value_bytes: baseline.stats.allocated_value_bytes,
            ..Default::default()
        };
        let outcome = query.execute(&control, None).unwrap();
        assert_eq!(outcome.result, baseline.result);
        assert_eq!(outcome.stats, baseline.stats);
        if let Some(options) = diagnostics {
            let report = outcome.diagnostics.unwrap();
            assert_eq!(
                report.truncated,
                options != QueryDiagnosticOptions::default()
            );
            assert!(report.steps.len() <= options.max_steps);
            assert!(report.capture_bytes <= options.max_bytes);
            assert!(report.inspection_work <= options.max_bytes);
        } else {
            assert!(outcome.diagnostics.is_none());
        }
        assert_eq!(
            query
                .execute(
                    &QueryControl {
                        max_work: control.max_work - 1,
                        ..control.clone()
                    },
                    None
                )
                .unwrap_err()
                .code,
            "query/work-limit"
        );
        assert_eq!(
            query
                .execute(
                    &QueryControl {
                        max_value_bytes: control.max_value_bytes - 1,
                        ..control
                    },
                    None
                )
                .unwrap_err()
                .code,
            "query/value-byte-limit"
        );
    }
}

#[test]
fn stock_cli_query_stats_preserve_predicate_schedule_and_normal_return() {
    use std::process::Command;
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("query.edn");
    std::fs::write(
        &file,
        "[:find ?n :in $rows :where [$rows ?n] [(< ?n 3)] [(> ?n 1)]]",
    )
    .unwrap();
    let sources = directory.path().join("sources.edn");
    std::fs::write(&sources, "{$rows [[1] [2] [3]]}").unwrap();
    let execute = |diagnostics: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_edb"));
        command
            .arg("query")
            .arg("--file")
            .arg(&file)
            .arg("--sources")
            .arg(&sources)
            .env_remove("EDB_POSTGRES_URL");
        if diagnostics {
            command.arg("--query-stats");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        read_edn(String::from_utf8(output.stdout).unwrap().trim()).unwrap()
    };
    let normal = execute(false);
    let edb_core::edn::EdnValue::Map(fields) = execute(true) else {
        panic!("query-stats response must be a map")
    };
    let lookup = |name| {
        fields
            .iter()
            .find(|(key, _)| key == &edb_core::edn::EdnValue::Keyword(Keyword::new("edb", name)))
            .unwrap()
            .1
            .clone()
    };
    assert_eq!(lookup("ret"), normal);
    assert_eq!(normal, read_edn("#{[2]}").unwrap());
    let stats = lookup("query-stats");
    let field = |map: &edb_core::edn::EdnValue, name| {
        let edb_core::edn::EdnValue::Map(fields) = map else {
            panic!("diagnostic record must be a map")
        };
        fields
            .iter()
            .find(|(key, _)| key == &edb_core::edn::EdnValue::Keyword(Keyword::new("query", name)))
            .unwrap()
            .1
            .clone()
    };
    let edb_core::edn::EdnValue::Vector(steps) = field(&stats, "steps") else {
        panic!("diagnostic steps must be a vector")
    };
    assert_eq!(
        steps
            .iter()
            .map(|step| field(step, "clause-path"))
            .collect::<Vec<_>>(),
        ["where/0", "where/2", "where/1"].map(|path| edb_core::edn::EdnValue::String(path.into()))
    );
    assert_eq!(
        field(&steps[1], "operator"),
        edb_core::edn::EdnValue::String(">".into())
    );
    assert_eq!(
        field(&steps[1], "rows-in"),
        edb_core::edn::EdnValue::Long(3)
    );
    assert_eq!(
        field(&steps[1], "rows-out"),
        edb_core::edn::EdnValue::Long(2)
    );
    assert_eq!(
        field(&steps[2], "rows-out"),
        edb_core::edn::EdnValue::Long(1)
    );
    assert!(parse_query_edn("[:find ?n :query-stats true :where [(ground 1) ?n]]").is_err());
}
