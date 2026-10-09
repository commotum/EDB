use edb_core::edn::read_edn;
use edb_core::edn_query::{EdnQueryArgument, parse_query_edn};
use edb_core::{
    Attribute, Cardinality, Clause, DataPattern, Database, DatabaseValue, EntityRef, FindElement,
    FindSpec, Keyword, PreparedQueryCache, Query, QueryControl, QueryDataSource, QueryEngine,
    QueryOutcome, QueryResult, QuerySourceValue, QueryValue, Schema, Term, TxOp, TxValue, Unique,
    Value, ValueType,
};

fn initial() -> Database {
    let mut schema = Schema::new();
    let mut tag = Attribute::new(
        100,
        Keyword::new("item", "tag"),
        ValueType::String,
        Cardinality::One,
    );
    tag.indexed = true;
    schema.install(tag).unwrap();
    schema
        .install(
            Attribute::new(
                101,
                Keyword::new("source", "name"),
                ValueType::String,
                Cardinality::One,
            )
            .unique(Unique::Identity),
        )
        .unwrap();
    schema
        .install(Attribute::new(
            102,
            Keyword::new("item", "plain"),
            ValueType::String,
            Cardinality::One,
        ))
        .unwrap();
    Database::new(schema).unwrap()
}
fn add(entity: u64, attribute: u32, text: &str) -> TxOp {
    TxOp::Add {
        entity: EntityRef::Id(entity),
        attribute,
        value: TxValue::Scalar(Value::String(text.into())),
    }
}
fn contributed() -> DatabaseValue {
    let first = initial()
        .with(
            &[
                add(99, 101, "Julia"),
                add(98, 101, "Jake"),
                add(42, 100, "same"),
                add(42, 102, "same"),
            ],
            1,
            EntityRef::Id(99),
        )
        .unwrap();
    first
        .db_after
        .with(&[add(42, 100, "same")], 2, EntityRef::Id(98))
        .unwrap()
        .db_after
        .database_value()
}
fn run(db: &DatabaseValue, text: &str, scan: bool) -> QueryOutcome {
    parse_query_edn(text)
        .unwrap()
        .bind(&[EdnQueryArgument::Source(QuerySourceValue::Database(
            db.clone(),
        ))])
        .unwrap()
        .execute(
            &QueryControl {
                force_scan: scan,
                ..QueryControl::default()
            },
            None,
        )
        .unwrap()
}
fn scalar(n: i64) -> QueryResult {
    QueryResult::Scalar(Some(QueryValue::Scalar(Value::Long(n))))
}
fn relation(values: Vec<Vec<Value>>) -> QueryResult {
    QueryResult::Relation(
        values
            .into_iter()
            .map(|row| row.into_iter().map(QueryValue::Scalar).collect())
            .collect(),
    )
}

#[test]
fn source_is_an_entity_coordinate_and_joins_ordinary_identity() {
    let db = contributed();
    for scan in [false, true] {
        let outcome = run(
            &db,
            "[:find ?e ?name :where [?e :item/tag \"same\" _ ?s] [?s :source/name ?name]]",
            scan,
        );
        assert_eq!(
            outcome.result,
            relation(vec![
                vec![Value::Ref(42), Value::String("Jake".into())],
                vec![Value::Ref(42), Value::String("Julia".into())]
            ])
        );
        for source in ["99", "[:source/name \"Julia\"]"] {
            assert_eq!(
                run(
                    &db,
                    &format!("[:find ?e :where [?e :item/tag \"same\" _ {source}]]"),
                    scan
                )
                .result,
                relation(vec![vec![Value::Ref(42)]])
            );
            let bound = parse_query_edn("[:find ?e :in $ ?s :where [?e :item/tag \"same\" _ ?s]]")
                .unwrap()
                .bind(&[
                    EdnQueryArgument::Source(QuerySourceValue::Database(db.clone())),
                    EdnQueryArgument::Data(read_edn(source).unwrap()),
                ])
                .unwrap();
            assert_eq!(
                bound
                    .execute(
                        &QueryControl {
                            force_scan: scan,
                            ..QueryControl::default()
                        },
                        None
                    )
                    .unwrap()
                    .result,
                relation(vec![vec![Value::Ref(42)]])
            );
        }
        assert_eq!(
            run(
                &db,
                "[:find ?e :where [?e :item/tag \"same\" _ [:source/name \"absent\"]]]",
                scan
            )
            .result,
            relation(vec![])
        );
    }
    let genesis = Database::bootstrap().unwrap().database_value();
    assert_eq!(
        run(
            &genesis,
            "[:find ?e :where [?e :db/ident :db/ident _ :db.source/system]]",
            false
        )
        .result,
        relation(vec![vec![Value::Ref(
            genesis.entid(&Keyword::new("db", "ident")).unwrap()
        )]])
    );
}

#[test]
fn contribution_distinctness_uses_the_existing_find_with_basis() {
    let db = contributed();
    for scan in [false, true] {
        assert_eq!(
            run(&db, "[:find ?v :where [42 :item/tag ?v]]", scan).result,
            relation(vec![vec![Value::String("same".into())]])
        );
        assert_eq!(
            run(
                &db,
                "[:find (count ?v) . :where [42 :item/tag ?v _ ?s]]",
                scan
            )
            .result,
            scalar(1)
        );
        assert_eq!(
            run(
                &db,
                "[:find (count ?v) . :with ?s :where [42 :item/tag ?v _ ?s]]",
                scan
            )
            .result,
            scalar(2)
        );
        assert_eq!(
            run(
                &db,
                "[:find (count ?v) . :with ?t :where [42 :item/tag ?v ?t]]",
                scan
            )
            .result,
            scalar(2)
        );
        assert_eq!(
            run(
                &db,
                "[:find ?s :where [42 :item/tag \"same\" _ ?s true]]",
                scan
            )
            .result,
            relation(vec![vec![Value::Ref(98)], vec![Value::Ref(99)]])
        );
    }
}

#[test]
fn source_patterns_never_resurrect_globally_withdrawn_values() {
    let live = contributed();
    let withdrawn = live
        .with(
            &[TxOp::Retract {
                entity: EntityRef::Id(42),
                attribute: 100,
                value: Some(TxValue::Scalar(Value::String("same".into()))),
            }],
            3,
            EntityRef::Id(97),
        )
        .unwrap()
        .db_after;
    for scan in [false, true] {
        for view in [
            withdrawn.clone(),
            withdrawn.clone().filter(|_, d| d.source != 97),
            withdrawn.clone().since(0),
        ] {
            assert_eq!(
                run(
                    &view,
                    "[:find (count ?e) . :where [?e :item/tag \"same\" _ 99]]",
                    scan
                )
                .result,
                scalar(0)
            );
            for pattern in [
                "[?e :item/tag ?v _ 99]",
                "[42 :item/tag ?v _ 99]",
                "[?e :item/tag ?v _ 99] [(>= ?v \"same\")]",
                "[?e :item/tag ?v _ 99] [(> ?v \"r\")] [(< ?v \"t\")]",
            ] {
                assert_eq!(
                    run(
                        &view,
                        &format!("[:find (count ?v) . :where {pattern}]"),
                        scan
                    )
                    .result,
                    scalar(0)
                );
            }
        }
        assert_eq!(
            run(
                &withdrawn.clone().history(),
                "[:find ?s ?added :where [42 :item/tag \"same\" _ ?s ?added]]",
                scan
            )
            .result,
            relation(vec![
                vec![Value::Ref(97), Value::Bool(false)],
                vec![Value::Ref(98), Value::Bool(true)],
                vec![Value::Ref(99), Value::Bool(true)]
            ])
        );
        assert_eq!(
            run(
                &withdrawn.clone().as_of(live.basis_t()),
                "[:find ?e :where [?e :item/tag \"same\" _ 99]]",
                scan
            )
            .result,
            relation(vec![vec![Value::Ref(42)]])
        );
        assert_eq!(
            run(
                &withdrawn.clone().history().since(live.basis_t()),
                "[:find ?s ?added :where [42 :item/tag \"same\" _ ?s ?added]]",
                scan
            )
            .result,
            relation(vec![vec![Value::Ref(97), Value::Bool(false)]])
        );
    }
}

#[test]
fn source_bound_patterns_choose_source_indexes_and_schedule_first() {
    let db = contributed();
    assert_eq!(
        run(
            &db,
            "[:find ?e :where [?e :item/plain \"same\" _ 99]]",
            false
        )
        .plan[0]
            .access,
        "SAEVT seek"
    );
    assert_eq!(
        run(&db, "[:find ?e :where [?e :item/tag \"same\" _ 99]]", false).plan[0].access,
        "SAVET seek"
    );
    let outcome = run(
        &db,
        "[:find ?e :where [?e :source/name _] [?e :item/tag _ _ 99]]",
        false,
    );
    assert_eq!(outcome.plan[0].access, "SAEVT seek");
    assert_eq!(outcome.plan[0].rows_after, 1);
}

#[test]
fn source_ranges_share_strict_bounds_exclusions_and_filtered_visibility() {
    let db = contributed()
        .with(
            &[
                add(43, 100, "a"),
                add(44, 100, "b"),
                add(45, 100, "c"),
                add(46, 100, "d"),
            ],
            3,
            EntityRef::Id(99),
        )
        .unwrap()
        .db_after
        .with(&[add(47, 100, "b")], 4, EntityRef::Id(98))
        .unwrap()
        .db_after;
    let query =
        "[:find ?e :where [?e :item/tag ?v _ 99] [(> ?v \"a\")] [(<= ?v \"d\")] [(not= ?v \"c\")]]";
    for view in [
        db.clone(),
        db.clone().filter(|_, _| true),
        db.clone().history(),
        db.clone().as_of(db.basis_t()),
    ] {
        let indexed = run(&view, query, false);
        assert_eq!(
            indexed.result,
            relation(vec![vec![Value::Ref(44)], vec![Value::Ref(46)]])
        );
        assert_eq!(indexed.result, run(&view, query, true).result);
        assert_eq!(indexed.plan[0].access, "SAVET range");
    }
}

#[test]
fn raw_datoms_preserve_six_positions_and_literal_source_values() {
    let db = contributed();
    let datoms = db
        .collect_datoms(edb_core::IndexOrder::Eavt)
        .unwrap()
        .into_iter()
        .filter(|d| d.entity == 42 && d.attribute == 100)
        .collect::<Vec<_>>();
    let source = QueryDataSource::datoms("$", datoms);
    let mut pattern = DataPattern::new(Term::var("e"), Term::Blank, Term::Blank);
    pattern.transaction = Some(Term::var("t"));
    pattern.fact_source = Some(Term::var("s"));
    pattern.added = Some(Term::var("added"));
    let query = Query::new(
        FindSpec::Relation(vec![
            FindElement::Variable("s".into()),
            FindElement::Variable("added".into()),
        ]),
        vec![Clause::Pattern(Box::new(pattern))],
    );
    assert_eq!(
        QueryEngine::execute_sources(&query, &[source], &[], &QueryControl::default())
            .unwrap()
            .result,
        relation(vec![
            vec![Value::Ref(98), Value::Bool(true)],
            vec![Value::Ref(99), Value::Bool(true)]
        ])
    );
    let source = QuerySourceValue::Tuples(std::sync::Arc::new(vec![vec![
        Value::Ref(42),
        Value::Ref(100),
        Value::String("same".into()),
        Value::Ref(1),
        Value::Keyword(Keyword::new("source", "literal")),
        Value::Bool(true),
    ]]));
    let bound = parse_query_edn("[:find ?e ?added :where [?e _ _ _ :source/literal ?added]]")
        .unwrap()
        .bind(&[EdnQueryArgument::Source(source)])
        .unwrap();
    assert_eq!(
        bound
            .execute(&QueryControl::default(), None)
            .unwrap()
            .result,
        relation(vec![vec![Value::Ref(42), Value::Bool(true)]])
    );
}

#[test]
fn prepared_cache_distinguishes_fact_sources() {
    let mut cache = PreparedQueryCache::new(4, 100_000);
    let make = |source| {
        let mut pattern =
            DataPattern::new(Term::var("e"), Term::Constant(Value::Ref(100)), Term::Blank);
        pattern.fact_source = Some(Term::Constant(Value::Ref(source)));
        Query::new(
            FindSpec::Collection(FindElement::Variable("e".into())),
            vec![Clause::Pattern(Box::new(pattern))],
        )
    };
    let a = cache.prepare(&make(99)).unwrap();
    let b = cache.prepare(&make(98)).unwrap();
    assert_ne!(a.query(), b.query());
    assert_eq!(cache.stats().entries, 2);
    cache.prepare(&make(99)).unwrap();
    assert_eq!(cache.stats().hits, 1);
}

#[test]
fn portable_query_codec_roundtrips_every_fact_coordinate() {
    let mut pattern = DataPattern::new(
        Term::var("e"),
        Term::Constant(Value::Ref(100)),
        Term::var("v"),
    );
    pattern.transaction = Some(Term::var("t"));
    pattern.fact_source = Some(Term::Constant(Value::Ref(99)));
    pattern.added = Some(Term::Constant(Value::Bool(true)));
    let query = Query::new(
        FindSpec::Relation(vec![
            FindElement::Variable("e".into()),
            FindElement::Variable("t".into()),
        ]),
        vec![Clause::Pattern(Box::new(pattern))],
    );
    let program = edb_core::Program {
        kind: edb_core::ProgramKind::Query,
        arity: 0,
        instructions: vec![
            edb_core::Instruction::Query(
                edb_core::QueryTemplate::native(query, vec![], vec![]).unwrap(),
            ),
            edb_core::Instruction::Return,
        ],
    };
    let bytes = edb_core::encode_program(&program).unwrap();
    assert_eq!(edb_core::decode_program(&bytes).unwrap(), program);
}

#[test]
fn raw_pattern_arity_includes_blanks_without_padding_short_patterns() {
    let db = contributed();
    let rows = vec![
        vec![Value::Long(1)],
        vec![Value::Long(2), Value::Long(20)],
        vec![Value::Long(3), Value::Long(30), Value::Long(300)],
    ];
    for scan in [false, true] {
        assert_eq!(
            run(&db, "[:find ?e :where [?e]]", scan).result,
            run(&db, "[:find ?e :where [?e _ _]]", scan).result,
        );
        assert_eq!(
            run(&db, "[:find ?e :where [?e :item/tag]]", scan).result,
            relation(vec![vec![Value::Ref(42)]]),
        );
        for (pattern, expected) in [
            ("[?n]", vec![1, 2, 3]),
            ("[?n _]", vec![2, 3]),
            ("[?n _ _]", vec![3]),
            ("[?n _ _ _]", vec![]),
        ] {
            let bound = parse_query_edn(&format!(
                "[:find ?n :in $rows :where [$rows {}]]",
                &pattern[1..pattern.len() - 1]
            ))
            .unwrap()
            .bind(&[EdnQueryArgument::Source(QuerySourceValue::Tuples(
                std::sync::Arc::new(rows.clone()),
            ))])
            .unwrap();
            assert_eq!(
                bound
                    .execute(
                        &QueryControl {
                            force_scan: scan,
                            ..QueryControl::default()
                        },
                        None
                    )
                    .unwrap()
                    .result,
                relation(expected.into_iter().map(|n| vec![Value::Long(n)]).collect())
            );
        }
        let source = QuerySourceValue::Tuples(std::sync::Arc::new(vec![
            vec![Value::Long(1); 5],
            vec![Value::Long(2); 6],
            vec![Value::Long(3); 7],
        ]));
        let bound = parse_query_edn("[:find ?n :where [?n _ _ _ _ _]]")
            .unwrap()
            .bind(&[EdnQueryArgument::Source(source)])
            .unwrap();
        assert_eq!(
            bound
                .execute(
                    &QueryControl {
                        force_scan: scan,
                        ..QueryControl::default()
                    },
                    None
                )
                .unwrap()
                .result,
            relation(vec![vec![Value::Long(2)], vec![Value::Long(3)]])
        );
    }
}
