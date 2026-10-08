//! A separate application for the local or TLS transactor. See docs/01_tutorials/01_application_workflow.md.
use edb_core::{
    Attribute, AttributeRef, Binding, Cardinality, Clause, Connection, DataPattern, Database,
    DatabaseValue, Entity, EntityMap, EntityRef, EntityValue, FindElement, FindSpec,
    FulltextOptions, FulltextReport, Function, IndexPrefix, InputSpec, Instruction, Keyword,
    MapValue, OperationContext, OperationKind, Program, ProgramControl, ProgramKind, ProgramOutput,
    ProgramRuntime, Query, QueryControl, QueryDataSource, QueryEngine, QueryInput, QueryResult,
    QueryTemplate, QueryTemplateSource, QueryValue, Schema, SemanticError, Term,
    TransactionRequest, TxForm, TxOp, TxValue, Unique, Value, ValueType, Variable, decode_program,
    encode_program, partition_eid, postgres_config_from_env, process_sql_stats, squuid,
    squuid_time_millis, uuid_v7, uuid_v7_time_millis,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Only compile-time fixture labels may bypass the normal error redaction.
#[derive(Debug)]
struct FixtureFailure(&'static str);

impl std::fmt::Display for FixtureFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for FixtureFailure {}

const PROJECT: u32 = 1_000;
const HOURS: u32 = 1_001;
const OWNER: u32 = 1_002;
const NAME: u32 = 1_003;
const TENANT_KEY: u32 = 1_004;
const TENANT_EVENTS: u32 = 1_005;
const TENANT_UUID: u32 = 1_006;
const TENANT_SQUUID: u32 = 1_007;
// The earlier named partition itself occupies a newly allocated db-partition
// entity after 1007. Do not reuse its identity for a later attribute.
const ARTICLE_TEXT: u32 = 1_010;
const ARTICLE_PUBLIC: u32 = 1_011;
const WAIT: Duration = Duration::from_secs(20);

fn source_ident() -> Keyword {
    Keyword::new("application.source", "demo")
}

fn application_source() -> EntityRef {
    EntityRef::Ident(source_ident())
}

fn source_description() -> TxOp {
    add(
        "application-source",
        edb_core::DB_IDENT as u32,
        Value::Keyword(source_ident()).into(),
    )
}

/// Attribution and the automatic timestamp apply to explicit and generated
/// changes alike. The source description is ordinary database information.
fn check_attribution(
    database: &DatabaseValue,
    datoms: &[edb_core::Datom],
) -> std::result::Result<(), SemanticError> {
    let source = database.entid(&source_ident()).ok_or_else(|| {
        SemanticError::incorrect(
            "application/source",
            "application source was not established",
        )
    })?;
    let tx = edb_core::t_to_tx(database.basis_t())?;
    let timestamps: Vec<_> = datoms
        .iter()
        .filter(|datom| datom.attribute == edb_core::DB_TX_INSTANT as u32)
        .collect();
    if datoms
        .iter()
        .any(|datom| datom.source != source || datom.tx != tx)
        || timestamps.len() != 1
        || !timestamps[0].added
        || timestamps[0].entity != tx
        || Some(&timestamps[0].value) != database.last_tx_instant().map(Value::Instant).as_ref()
    {
        return Err(SemanticError::incorrect(
            "application/attribution",
            "receipt changes or automatic timestamp have inconsistent attribution",
        ));
    }
    Ok(())
}

enum AppEndpoint {
    Local(PathBuf),
    Remote(edb_core::RemoteWriter),
}
trait AppSubmit {
    fn transact_application(
        &self,
        endpoint: &AppEndpoint,
        request: TransactionRequest,
        timeout: Duration,
    ) -> std::result::Result<edb_core::CommittedTransaction, SemanticError>;
}
impl AppSubmit for Connection {
    fn transact_application(
        &self,
        endpoint: &AppEndpoint,
        request: TransactionRequest,
        timeout: Duration,
    ) -> std::result::Result<edb_core::CommittedTransaction, SemanticError> {
        let mut committed = match endpoint {
            AppEndpoint::Local(path) => self.transact_socket(path, request, timeout),
            AppEndpoint::Remote(writer) => writer.transact(request, timeout),
        }?;
        // Keep a confirmed receipt confirmed even if opening or checking its
        // local report fails. Exact retries still use the complete request.
        if let Ok(report) = &committed.report
            && let Err(error) = check_attribution(&report.db_after, &report.tx_data)
        {
            committed.report = Err(error);
        }
        Ok(committed)
    }
}

#[derive(Debug, Eq, PartialEq)]
struct Summary {
    projects: BTreeMap<String, (i64, String)>,
    total_hours: i64,
}

/// Application logic depends on one immutable value, not a connection or storage backend.
fn calculation(database: &DatabaseValue) -> Result<Summary> {
    let entity = Variable::from("project");
    let query = Query::new(
        FindSpec::Collection(FindElement::Variable(entity.clone())),
        vec![Clause::Pattern(Box::new(DataPattern::new(
            edb_core::Term::Variable(entity),
            edb_core::Term::Constant(Value::Ref(u64::from(PROJECT))),
            edb_core::Term::Blank,
        )))],
    );
    let control = QueryControl {
        timeout: Some(WAIT),
        max_work: 10_000,
        max_result_rows: 100,
        max_intermediate_rows: 100,
        ..QueryControl::default()
    };
    let QueryResult::Collection(ids) = database.query(&query, &[], &control)?.result else {
        return Err("application query returned an unexpected shape".into());
    };
    let mut projects = BTreeMap::new();
    let mut total_hours = 0;
    for id in ids {
        let QueryValue::Scalar(Value::Ref(id)) = id else {
            return Err("application query returned a non-entity".into());
        };
        let project = database.entity(id)?.ok_or("project was not found")?;
        let key = string(&project, PROJECT)?;
        let Some(EntityValue::Scalar(Value::Long(hours))) = project.get(HOURS)? else {
            return Err("project has no hours".into());
        };
        let Some(EntityValue::Entity(owner)) = project.get(OWNER)? else {
            return Err("project has no owner".into());
        };
        projects.insert(key, (hours, string(&owner, NAME)?));
        total_hours += hours;
    }
    Ok(Summary {
        projects,
        total_hours,
    })
}

fn string(entity: &Entity, attribute: u32) -> Result<String> {
    match entity.get(attribute)? {
        Some(EntityValue::Scalar(Value::String(value))) => Ok(value),
        _ => Err("application entity has no expected string".into()),
    }
}

/// Run ordinary application reads on a single-thread async executor. Only pure
/// result rows cross into the task; native connection/value cleanup stays here.
fn async_read_workflow(connection: &Connection, captured: &DatabaseValue) -> Result<()> {
    use edb_core::{AsyncClient, AsyncConfig, AsyncExecutor, AsyncStreamOptions};
    let started = Instant::now();
    let workers = AsyncExecutor::new(AsyncConfig {
        workers: 2,
        max_operations: 4,
        max_resources: 4,
    })?;
    let client = AsyncClient::new(connection, &workers)?;
    let query = Query::new(
        FindSpec::Relation(vec![FindElement::Variable("hours".into())]),
        vec![Clause::Pattern(Box::new(DataPattern::new(
            Term::Blank,
            Term::Constant(Value::Ref(u64::from(HOURS))),
            Term::var("hours"),
        )))],
    );
    let current_expected = connection
        .db()
        .query(&query, &[], &QueryControl::default())?
        .result;
    let captured_expected = captured
        .query(&query, &[], &QueryControl::default())?
        .result;
    let sources = [QueryDataSource::database("$", captured.clone())];
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let current = client
            .query(query.clone(), vec![], QueryControl::default())?
            .await?;
        require(
            current.result == current_expected,
            "async captured current query changed",
        )?;
        let mut stream = workers.query_sequence_sources(
            query,
            &sources,
            vec![],
            QueryControl::default(),
            None,
            AsyncStreamOptions {
                chunk_rows: 1,
                timeout: Some(WAIT),
            },
        )?;
        let mut rows = Vec::new();
        while let Some(row) = stream.next().await {
            rows.push(row?);
        }
        require(
            QueryResult::Relation(rows) == captured_expected,
            "async old-value stream changed",
        )?;
        drop(stream);
        Ok::<(), Box<dyn std::error::Error>>(())
    })?;
    drop(client);
    let deadline = Instant::now() + WAIT;
    while workers.stats().operations != 0
        || workers.stats().resources != 0
        || workers.stats().running != 0
    {
        require(
            Instant::now() < deadline,
            "async worker cleanup did not finish",
        )?;
        std::thread::sleep(Duration::from_millis(1));
    }
    println!(
        "ASYNC_READ_OK single_thread=true old_basis=true chunk_rows=1 complete_us={} stats={:?}",
        started.elapsed().as_micros(),
        workers.stats()
    );
    Ok(())
}

fn schema() -> Result<Schema> {
    let mut schema = Schema::new();
    for attribute in [
        Attribute::new(
            PROJECT,
            Keyword::new("project", "key"),
            ValueType::String,
            Cardinality::One,
        )
        .unique(Unique::Identity),
        Attribute::new(
            HOURS,
            Keyword::new("project", "hours"),
            ValueType::Long,
            Cardinality::One,
        ),
        Attribute::new(
            OWNER,
            Keyword::new("project", "owner"),
            ValueType::Ref,
            Cardinality::One,
        ),
        Attribute::new(
            NAME,
            Keyword::new("person", "name"),
            ValueType::String,
            Cardinality::One,
        ),
    ] {
        schema.install(attribute)?;
    }
    Ok(schema)
}

fn add(entity: &str, attribute: u32, value: TxValue) -> TxOp {
    TxOp::Add {
        entity: EntityRef::Temp(entity.into()),
        attribute,
        value,
    }
}

fn initial_data() -> Vec<TxOp> {
    let mut operations = vec![
        source_description(),
        add("owner", NAME, Value::String("Ada".into()).into()),
    ];
    for (key, hours) in [("alpha", 3), ("beta", 5)] {
        operations.extend([
            add(key, PROJECT, Value::String(key.into()).into()),
            add(key, HOURS, Value::Long(hours).into()),
            add(key, OWNER, TxValue::Entity(EntityRef::Temp("owner".into()))),
        ]);
    }
    operations
}

fn update() -> Vec<TxOp> {
    vec![TxOp::Add {
        entity: EntityRef::Lookup {
            attribute: PROJECT,
            value: Value::String("alpha".into()),
        },
        attribute: HOURS,
        value: Value::Long(7).into(),
    }]
}

fn require(condition: bool, message: &'static str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Box::new(FixtureFailure(message)))
    }
}

/// Join immutable database facts to application-owned planning data. Neither
/// lazy projection nor entity comparison needs a fresh connection value.
fn read_values_workflow(before: &DatabaseValue, after: &DatabaseValue) -> Result<()> {
    let measured = OperationContext::new(OperationKind::Application);
    let _scope = measured.enter();
    let started = Instant::now();
    let indexed = before.has_avet(&edb_core::AttributeName::Id(PROJECT))?;
    let mut target = edb_core::RelationPattern::new(vec![Term::var("key"), Term::var("target")]);
    target.source = "$targets".into();
    let query = Query::new(
        FindSpec::Relation(vec![
            FindElement::Variable("key".into()),
            FindElement::Variable("target".into()),
        ]),
        vec![
            Clause::Pattern(Box::new(DataPattern::new(
                Term::var("project"),
                Term::Constant(Value::Ref(PROJECT.into())),
                Term::var("key"),
            ))),
            Clause::RelationPattern(Box::new(target)),
        ],
    );
    {
        let sources = [
            QueryDataSource::database("$", before.clone()),
            QueryDataSource::tuples(
                "$targets",
                vec![
                    vec![Value::String("alpha".into()), Value::Long(10)],
                    vec![Value::String("beta".into()), Value::Long(20)],
                ],
            ),
        ];
        let control = QueryControl {
            timeout: Some(WAIT),
            max_work: 10_000,
            ..Default::default()
        };
        let sequence = QueryEngine::sequence_sources(&query, &sources, &[], &control)?;
        require(
            sequence.remaining_rows() == 2,
            "mixed-source query lost a project",
        )?;
        let rows = sequence.collect::<std::result::Result<Vec<_>, _>>()?;
        require(rows.len() == 2, "mixed-source sequence did not finish")?;
    }
    let eid = before
        .lookup(PROJECT, &Value::String("alpha".into()))?
        .ok_or("alpha missing")?;
    let old = before.entity(eid)?.ok_or("old alpha missing")?;
    let new = after.entity(eid)?.ok_or("new alpha missing")?;
    require(
        old == new && old.identity() == new.identity(),
        "entity identity changed across time",
    )?;
    println!(
        "READ_VALUES_OK mixed_sources=true entity_identity=true avet_ready={indexed} elapsed_ms={} sql_calls={}",
        started.elapsed().as_millis(),
        measured.snapshot().sql_calls
    );
    Ok(())
}

/// Application-owned planning values need neither a stored schema nor a dummy
/// database. The same query also joins them to a captured durable database value.
fn general_query_workflow(before: &DatabaseValue, after: &DatabaseValue) -> Result<()> {
    use edb_core::edn::read_edn;
    use edb_core::edn_query::{EdnQueryArgument, parse_query_edn};
    use edb_core::{QueryExtensions, QuerySourceValue};
    let measured = OperationContext::new(OperationKind::Application);
    let _scope = measured.enter();
    let started = Instant::now();
    let rows = EdnQueryArgument::Data(read_edn(
        r#"[["alpha" nil \A #app/status :planned {:note "review"} #{:local :draft} 7]]"#,
    )?);
    let query = parse_query_edn(
        r#"[:find ?hours ?summary :in $ $planning
            :where [?project :project/key ?key] [?project :project/hours ?hours]
                   [$planning ?key ?optional ?letter ?tag ?details ?labels ?priority]
                   [(app/summary ?details ?labels ?tag ?optional) ?summary]]"#,
    )?;
    let mut extensions = QueryExtensions::new();
    extensions.register_pure("app/summary", |args, _control| {
        let [details, labels, tag, optional] = args else {
            return Err(SemanticError::incorrect(
                "app/arity",
                "summary requires four values",
            ));
        };
        Ok(QueryValue::Map(vec![
            (
                QueryValue::Scalar(Value::Keyword(Keyword::unqualified("details"))),
                details.clone(),
            ),
            (
                QueryValue::Scalar(Value::Keyword(Keyword::unqualified("labels"))),
                labels.clone(),
            ),
            (
                QueryValue::Scalar(Value::Keyword(Keyword::unqualified("status"))),
                tag.clone(),
            ),
            (
                QueryValue::Scalar(Value::Keyword(Keyword::unqualified("optional"))),
                optional.clone(),
            ),
        ]))
    });
    let control = QueryControl {
        timeout: Some(WAIT),
        max_work: 100_000,
        ..Default::default()
    };
    let run = |database: &DatabaseValue| -> Result<QueryResult> {
        let bound = query.bind(&[
            EdnQueryArgument::Source(QuerySourceValue::Database(database.clone())),
            rows.clone(),
        ])?;
        let result = bound.execute(&control, Some(&extensions))?.result;
        let QueryResult::Relation(actual) = &result else {
            return Err("general query shape".into());
        };
        require(actual.len() == 1, "general data join lost alpha")?;
        require(
            actual[0][0]
                == QueryValue::Scalar(Value::Long(calculation(database)?.projects["alpha"].0)),
            "general query changed captured hours",
        )?;
        require(
            matches!(&actual[0][1], QueryValue::Map(fields) if fields.len() == 4),
            "callback lost nested data",
        )?;
        Ok(result)
    };
    let old = run(before)?;
    drop(run(after)?);
    require(
        run(before)? == old,
        "general query changed an old database value",
    )?;
    drop(old);
    drop(extensions);
    drop(query);
    drop(rows);
    println!(
        "QUERY_DATA_OK width=7 nested_values=true pure_callbacks=true old_basis=true elapsed_ms={} sql_calls={}",
        started.elapsed().as_millis(),
        measured.snapshot().sql_calls
    );
    Ok(())
}

/// Reduce related planning columns together, using one captured value and
/// ordinary native string helpers for presentation.
fn application_computation_workflow(before: &DatabaseValue, after: &DatabaseValue) -> Result<()> {
    use edb_core::edn::read_edn;
    use edb_core::edn_query::{EdnQueryArgument, parse_query_edn};
    use edb_core::{AggregateValue, QueryExtensions, QuerySourceValue};
    let measured = OperationContext::new(OperationKind::Application);
    let _scope = measured.enter();
    let started = Instant::now();
    let mut extensions = QueryExtensions::new();
    extensions.register_aggregate("app/weighted-hours", |group, _| {
        let Some(QueryValue::Scalar(Value::Long(scale))) = group.constant(2) else {
            return Err(SemanticError::incorrect(
                "app/scale",
                "integer scale required",
            ));
        };
        let mut total = 0;
        for row in 0..group.len() {
            group.check(1)?;
            let integer = |arg| match group.value(row, arg) {
                Some(AggregateValue::Stored(Value::Long(value)))
                | Some(AggregateValue::Query(QueryValue::Scalar(Value::Long(value)))) => Ok(*value),
                _ => Err(SemanticError::incorrect(
                    "app/weight",
                    "integer hours and weight required",
                )),
            };
            total += integer(0)? * integer(1)? * scale;
        }
        Ok(QueryValue::Scalar(Value::Long(total)))
    });
    let query = parse_query_edn(
        r#"[:find ?label (app/weighted-hours ?hours ?weight 10)
        :in $ $planning
        :where [?project :project/key ?key] [?project :project/hours ?hours]
               [$planning ?key ?weight] [(starts-with? ?key "a")]
               [(subs ?key 0 1) ?prefix] [(str ?prefix ":" ?key) ?label]]"#,
    )?;
    let plans = EdnQueryArgument::Data(read_edn(r#"[["alpha" 2] ["beta" 3]]"#)?);
    let control = QueryControl {
        timeout: Some(WAIT),
        max_work: 100_000,
        ..Default::default()
    };
    let evaluate = |database: &DatabaseValue| -> Result<QueryResult> {
        let bound = query.bind(&[
            EdnQueryArgument::Source(QuerySourceValue::Database(database.clone())),
            plans.clone(),
        ])?;
        let actual = bound.execute(&control, Some(&extensions))?.result;
        let expected = QueryResult::Relation(vec![vec![
            QueryValue::Scalar(Value::String("a:alpha".into())),
            QueryValue::Scalar(Value::Long(calculation(database)?.projects["alpha"].0 * 20)),
        ]]);
        require(
            actual == expected,
            "weighted application computation changed its basis or arguments",
        )?;
        Ok(actual)
    };
    let old = evaluate(before)?;
    drop(evaluate(after)?);
    require(
        evaluate(before)? == old,
        "aggregate changed old captured value",
    )?;
    drop(old);
    drop(query);
    drop(plans);
    drop(extensions);
    println!(
        "COMPUTATION_OK weighted=true native_helpers=true old_basis=true elapsed_us={} sql_calls={}",
        started.elapsed().as_micros(),
        measured.snapshot().sql_calls
    );
    Ok(())
}

/// Plans retain replayable intent and a protected basis, not speculative IDs
/// or a datom diff. Fixed request keys keep the entire example restartable.
fn planning_workflow(
    connection: &Connection,
    endpoint: &AppEndpoint,
    base: &DatabaseValue,
) -> Result<()> {
    let measured = OperationContext::new(OperationKind::Application);
    let _scope = measured.enter();
    let started = Instant::now();
    let reference = base.snapshot_reference()?;
    let original = calculation(base)?;
    let time = base
        .last_tx_instant()
        .ok_or("planning base has no transaction instant")?
        + 1;
    let intent = |hours| {
        vec![
            TxOp::Add {
                entity: EntityRef::Lookup {
                    attribute: PROJECT,
                    value: Value::String("alpha".into()),
                },
                attribute: HOURS,
                value: Value::Long(hours).into(),
            },
            add(
                "planned-person",
                NAME,
                Value::String("Selected plan".into()).into(),
            ),
        ]
    };
    let low = base.with(&intent(9), time, application_source())?;
    check_attribution(&low.db_after, &low.tx_data)?;
    let selected_intent = intent(11);
    let selected = base.with_forms_with_hints(
        &selected_intent
            .iter()
            .cloned()
            .map(edb_core::TxForm::Op)
            .collect::<Vec<_>>(),
        time,
        application_source(),
        edb_core::SpeculationLimits::default(),
        edb_core::HintLimits::default(),
    )?;
    check_attribution(&selected.report.db_after, &selected.report.tx_data)?;
    require(
        calculation(&low.db_after)?.total_hours == 14,
        "low alternative mismatch",
    )?;
    require(
        calculation(&selected.report.db_after)?.total_hours == 16,
        "selected alternative mismatch",
    )?;
    let speculative_id = selected.report.tempids["planned-person"];
    let generated_hint_count = selected
        .hints
        .as_ref()
        .map_or(0, |hints| hints.reads().len());
    drop(low);
    drop(selected.report);
    require(
        calculation(base)? == original,
        "discarded alternatives changed their base",
    )?;

    // A real intervening transaction invalidates the protected basis and
    // consumes an ID the speculative branch had only imagined allocating.
    let intervened = connection
        .transact_application(
            endpoint,
            TransactionRequest::new(
                "application-workflow/planning-intervene/v1",
                vec![
                    TxOp::Add {
                        entity: EntityRef::Lookup {
                            attribute: PROJECT,
                            value: Value::String("alpha".into()),
                        },
                        attribute: HOURS,
                        value: Value::Long(9).into(),
                    },
                    add(
                        "intervening-person",
                        NAME,
                        Value::String("Intervening person".into()).into(),
                    ),
                ],
                application_source(),
            )
            .comparing_basis(base.basis_t()),
            WAIT,
        )?
        .report?;
    let stale = connection.transact_application(
        endpoint,
        TransactionRequest::new(
            "application-workflow/planning-stale/v1",
            selected_intent.clone(),
            application_source(),
        )
        .comparing_basis(base.basis_t()),
        WAIT,
    );
    require(
        stale
            .as_ref()
            .is_err_and(|error| error.category == edb_core::ErrorCategory::Conflict),
        "stale plan was not rejected",
    )?;
    let replanned = intervened.db_after.with(
        &selected_intent,
        intervened
            .db_after
            .last_tx_instant()
            .ok_or("replan base has no time")?
            + 1,
        application_source(),
    )?;
    check_attribution(&replanned.db_after, &replanned.tx_data)?;
    let selected = connection
        .transact_application(
            endpoint,
            TransactionRequest::new(
                "application-workflow/planning-select/v1",
                selected_intent,
                application_source(),
            )
            .comparing_basis(intervened.basis_t),
            WAIT,
        )?
        .report?;
    require(
        calculation(&selected.db_after)? == calculation(&replanned.db_after)?,
        "revalidated plan changed meaning",
    )?;
    require(
        selected.tempids["planned-person"] != speculative_id,
        "plan reused its imaginary allocation",
    )?;
    require(
        selected.tempids["planned-person"] == replanned.tempids["planned-person"],
        "revalidated logical ID resolution disagrees",
    )?;

    // Normal new facts restore the demonstration's initial current state;
    // this is not undo/backtracking and history continues to record all commits.
    let restored = connection
        .transact_application(
            endpoint,
            TransactionRequest::new(
                "application-workflow/planning-restore/v1",
                vec![
                    TxOp::Add {
                        entity: EntityRef::Lookup {
                            attribute: PROJECT,
                            value: Value::String("alpha".into()),
                        },
                        attribute: HOURS,
                        value: Value::Long(7).into(),
                    },
                    TxOp::RetractEntity(EntityRef::Id(selected.tempids["planned-person"])),
                    TxOp::RetractEntity(EntityRef::Id(intervened.tempids["intervening-person"])),
                ],
                application_source(),
            )
            .comparing_basis(selected.basis_t),
            WAIT,
        )?
        .report?;
    require(
        calculation(&restored.db_after)? == original,
        "planning cleanup current state mismatch",
    )?;
    let reopened =
        connection.reopen_snapshot(&edb_core::SnapshotReference::decode(&reference.encode()?)?)?;
    require(
        reopened.snapshot_key()? == base.snapshot_key()? && calculation(&reopened)? == original,
        "exact plan base reopen mismatch",
    )?;
    println!(
        "PLANNING_OK discarded=true stale_rejected=true logical_ids_remapped=true exact_report=true exact_reference=true basis_t={} elapsed_ms={} sql_calls={} generated_hints={generated_hint_count}",
        restored.basis_t,
        started.elapsed().as_millis(),
        measured.snapshot().sql_calls
    );
    Ok(())
}

/// A fixed tenant identity lets this owned fixture recover already-committed
/// UUID intent on process restart. General applications should persist an
/// outbox/request before submission, not regenerate random retry payloads.
fn partition_workflow(connection: &Connection, endpoint: &AppEndpoint) -> Result<()> {
    let attributes = vec![
        Attribute::new(
            TENANT_KEY,
            Keyword::new("tenant", "key"),
            ValueType::String,
            Cardinality::One,
        )
        .unique(Unique::Identity),
        Attribute::new(
            TENANT_EVENTS,
            Keyword::new("tenant", "events"),
            ValueType::Ref,
            Cardinality::Many,
        )
        .component(),
        Attribute::new(
            TENANT_UUID,
            Keyword::new("tenant", "uuid"),
            ValueType::Uuid,
            Cardinality::One,
        )
        .unique(Unique::Value),
        Attribute::new(
            TENANT_SQUUID,
            Keyword::new("tenant", "squuid"),
            ValueType::Uuid,
            Cardinality::One,
        ),
    ];
    connection
        .transact_application(
            endpoint,
            TransactionRequest::new(
                "application-workflow/partition-schema/v1",
                attributes.into_iter().map(TxOp::InstallAttribute).collect(),
                application_source(),
            ),
            WAIT,
        )?
        .report?;
    let partition_ident = Keyword::new("application.partition", "acme");
    let partition = connection
        .transact_application(
            endpoint,
            TransactionRequest::new(
                "application-workflow/partition-install/v1",
                vec![],
                application_source(),
            )
            .with_forms([TxForm::EntityMap(EntityMap {
                id: Some(EntityRef::Temp("partition".into())),
                attributes: vec![
                    (
                        AttributeRef::Ident(Keyword::new("db", "ident")),
                        MapValue::Value(Value::Keyword(partition_ident.clone()).into()),
                    ),
                    (
                        AttributeRef::ReverseIdent(Keyword::new("db.install", "partition")),
                        MapValue::Value(Value::Keyword(Keyword::new("db.part", "db")).into()),
                    ),
                ],
            })]),
            WAIT,
        )?
        .report?;
    let partition_id = partition
        .db_after
        .entid(&partition_ident)
        .ok_or("partition was not installed")?;

    // On a later run the install receipt still describes basis 8; only an
    // explicit current sync can recover UUID intent from the later tenant tx.
    let current = connection.sync()?;
    let prior = calculation(&current)?;
    let key = Value::String("acme".into());
    let (uuid, sequential) = if let Some(tenant) = current.lookup(TENANT_KEY, &key)? {
        let one = |attribute| -> Result<u128> {
            match current.values(tenant, attribute)?.as_slice() {
                [Value::Uuid(value)] => Ok(*value),
                _ => Err("committed UUID intent is missing; do not regenerate a retry".into()),
            }
        };
        (one(TENANT_UUID)?, one(TENANT_SQUUID)?)
    } else {
        (uuid_v7()?, squuid()?)
    };
    let uuid_time = uuid_v7_time_millis(uuid)?;
    let squuid_time = squuid_time_millis(sequential);
    let request = TransactionRequest::new(
        "application-workflow/tenant/v1",
        vec![TxOp::ForcePartition {
            tempid: "tenant".into(),
            partition: EntityRef::Ident(partition_ident),
        }],
        application_source(),
    )
    .with_forms([TxForm::EntityMap(EntityMap {
        id: Some(EntityRef::Temp("tenant".into())),
        attributes: vec![
            (AttributeRef::Id(TENANT_KEY), MapValue::Value(key.into())),
            (
                AttributeRef::Id(TENANT_UUID),
                MapValue::Value(Value::Uuid(uuid).into()),
            ),
            (
                AttributeRef::Id(TENANT_SQUUID),
                MapValue::Value(Value::Uuid(sequential).into()),
            ),
            (
                AttributeRef::Id(TENANT_EVENTS),
                MapValue::Nested(Box::new(EntityMap {
                    id: Some(EntityRef::Temp("event".into())),
                    attributes: vec![(
                        AttributeRef::Id(NAME),
                        MapValue::Value(Value::String("Tenant event".into()).into()),
                    )],
                })),
            ),
        ],
    })]);
    let outcome = connection.transact_application(endpoint, request.clone(), WAIT)?;
    let replayed = outcome.replayed;
    let committed = outcome.report?;
    let tenant = committed.tempids["tenant"];
    let event = committed.tempids["event"];
    require(
        partition_eid(tenant)? == partition_id && partition_eid(event)? == partition_id,
        "tenant component partition affinity failed",
    )?;
    require(
        committed.db_after.values(tenant, TENANT_UUID)? == vec![Value::Uuid(uuid)]
            && committed.db_after.values(tenant, TENANT_SQUUID)? == vec![Value::Uuid(sequential)],
        "UUID storage roundtrip failed",
    )?;
    require(
        calculation(&committed.db_after)? == prior,
        "tenant data changed the project calculation",
    )?;
    let retried = connection.transact_application(endpoint, request, WAIT)?;
    require(
        retried.replayed && retried.tx_hash == committed.tx_hash,
        "tenant retry changed receipt",
    )?;
    let retried = retried.report?;
    require(
        retried.tempids == committed.tempids
            && retried.db_after.snapshot_key()? == committed.db_after.snapshot_key()?,
        "tenant retry changed identities or exact report",
    )?;
    let reopened = connection.reopen_snapshot(&edb_core::SnapshotReference::decode(
        &committed.db_after.snapshot_reference()?.encode()?,
    )?)?;
    require(
        reopened.values(tenant, TENANT_EVENTS)? == vec![Value::Ref(event)]
            && reopened.values(tenant, TENANT_UUID)? == vec![Value::Uuid(uuid)],
        "tenant exact reference changed facts",
    )?;
    println!(
        "PARTITIONS_OK named=true component_affinity=true uuid_roundtrip=true retry_exact=true reference_exact=true replayed={replayed} basis_t={} uuid_v7_millis={uuid_time} squuid_millis={squuid_time}",
        committed.basis_t
    );
    Ok(())
}

/// Search the captured value once. Recent/speculative text completes physical
/// index lag; an absent attachment cannot appear on this immutable value.
fn search_fulltext(database: &DatabaseValue) -> Result<FulltextReport> {
    let options = FulltextOptions {
        timeout: Some(WAIT),
        ..FulltextOptions::default()
    };
    Ok(database.fulltext(ARTICLE_TEXT, "immutable", &options)?)
}

fn fulltext_workflow(connection: &Connection, endpoint: &AppEndpoint) -> Result<()> {
    connection
        .transact_application(
            endpoint,
            TransactionRequest::new(
                "application-workflow/fulltext-schema/v1",
                vec![
                    TxOp::InstallAttribute(
                        Attribute::new(
                            ARTICLE_TEXT,
                            Keyword::new("article", "text"),
                            ValueType::String,
                            Cardinality::One,
                        )
                        .fulltext(),
                    ),
                    TxOp::InstallAttribute(Attribute::new(
                        ARTICLE_PUBLIC,
                        Keyword::new("article", "public"),
                        ValueType::Boolean,
                        Cardinality::One,
                    )),
                ],
                application_source(),
            ),
            WAIT,
        )?
        .report?;
    let request = TransactionRequest::new(
        "application-workflow/fulltext-data/v1",
        vec![
            add(
                "public-article",
                ARTICLE_TEXT,
                Value::String("Immutable facts in Rust".into()).into(),
            ),
            add("public-article", ARTICLE_PUBLIC, Value::Bool(true).into()),
            add(
                "archived-article",
                ARTICLE_TEXT,
                Value::String("Immutable archival notes".into()).into(),
            ),
            add(
                "archived-article",
                ARTICLE_PUBLIC,
                Value::Bool(false).into(),
            ),
        ],
        application_source(),
    );
    let submitted = connection.transact_application(endpoint, request.clone(), WAIT)?;
    let replayed = submitted.replayed;
    let committed = submitted.report?;
    let retry = connection.transact_application(endpoint, request, WAIT)?;
    require(
        retry.replayed && retry.tx_hash == committed.tx_hash,
        "fulltext data retry changed receipt",
    )?;
    require(
        retry.report?.db_after.snapshot_key()? == committed.db_after.snapshot_key()?,
        "fulltext data retry changed exact report",
    )?;

    // This separate app has no maintenance credentials. The small fixture
    // threshold schedules the coherent index/search job; query the captured
    // value returned after its canonical basis has caught up.
    let database = connection.sync_index(committed.basis_t, WAIT)?;
    let report = search_fulltext(&database)?;
    require(
        report.stats.index_basis_t >= committed.basis_t && report.hits.len() == 2,
        "fulltext projection did not cover the article transaction",
    )?;
    let mut query = Query::new(
        FindSpec::Relation(
            ["entity", "text", "tx", "score"]
                .into_iter()
                .map(|v| FindElement::Variable(v.into()))
                .collect(),
        ),
        vec![
            Clause::Function {
                function: Function::Fulltext,
                source: "$".into(),
                args: vec![
                    Term::Constant(Value::Keyword(Keyword::new("article", "text"))),
                    Term::var("search"),
                ],
                binding: Binding::Relation(
                    ["entity", "text", "tx", "score"]
                        .into_iter()
                        .map(|v| Some(v.into()))
                        .collect(),
                ),
            },
            Clause::Pattern(Box::new(DataPattern::new(
                Term::var("entity"),
                Term::Constant(Value::Ref(u64::from(ARTICLE_PUBLIC))),
                Term::Constant(Value::Bool(true)),
            ))),
        ],
    );
    query.inputs = vec![InputSpec::Scalar("search".into())];
    let QueryResult::Relation(rows) = database
        .query(
            &query,
            &[QueryInput::Scalar(Value::String("immutable".into()))],
            &QueryControl::default(),
        )?
        .result
    else {
        return Err("fulltext query returned an unexpected shape".into());
    };
    require(
        rows.len() == 1
            && rows[0][0] == QueryValue::Scalar(Value::Ref(committed.tempids["public-article"]))
            && rows[0][1] == QueryValue::Scalar(Value::String("Immutable facts in Rust".into())),
        "structured fulltext join selected the wrong article",
    )?;

    // This read-only app authorizes no program deployment. It demonstrates the
    // portable codec locally; the stored-program PostgreSQL test covers operator
    // deployment and re-opening under its explicit fixture authority.
    let program = Program {
        kind: ProgramKind::Query,
        arity: 1,
        instructions: vec![
            Instruction::Query(QueryTemplate::native(
                query,
                vec![0],
                vec![QueryTemplateSource::current("$")],
            )?),
            Instruction::ForEach {
                body: vec![Instruction::Unpack(4), Instruction::EmitRow(4)],
            },
            Instruction::Return,
        ],
    };
    let encoded = encode_program(&program)?;
    require(
        encoded[16..18] == edb_core::PROGRAM_ABI_VERSION.to_be_bytes(),
        "fulltext program did not use the current program format",
    )?;
    let ProgramOutput::Query(program_rows) = ProgramRuntime.execute_query(
        &decode_program(&encoded)?,
        &database,
        &[Value::String("immutable".into())],
        ProgramControl::default(),
    )?
    else {
        return Err("fulltext program returned an unexpected shape".into());
    };
    require(
        program_rows.len() == 1
            && program_rows[0]
                .iter()
                .cloned()
                .map(QueryValue::Scalar)
                .collect::<Vec<_>>()
                == rows[0],
        "portable fulltext program changed query meaning",
    )?;
    require(
        calculation(&database)? == calculation(&committed.db_before)?,
        "article data changed the project calculation",
    )?;
    println!(
        "FULLTEXT_OK native=true structured_join=true portable_program=true retry_exact=true replayed={replayed} basis_t={} index_basis_t={} hits=2 public_hits=1",
        committed.basis_t, report.stats.index_basis_t
    );
    Ok(())
}

fn run() -> Result<()> {
    let application_context = OperationContext::new(OperationKind::Application);
    let _application_scope = application_context.enter();
    let mut database_id = None;
    let mut endpoint = None;
    let mut reference_in: Option<PathBuf> = None;
    let mut reference_out: Option<PathBuf> = None;
    let mut remote = false;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--database" if database_id.is_none() => {
                database_id = Some(arguments.next().ok_or("missing database")?)
            }
            "--endpoint" if endpoint.is_none() => {
                endpoint = Some(PathBuf::from(arguments.next().ok_or("missing endpoint")?))
            }
            "--remote" if !remote => remote = true,
            "--reference-in" if reference_in.is_none() => {
                reference_in = Some(arguments.next().ok_or("missing reference input")?.into())
            }
            "--reference-out" if reference_out.is_none() => {
                reference_out = Some(arguments.next().ok_or("missing reference output")?.into())
            }
            _ => {
                return Err(
                    "usage: application_workflow --database ID (--endpoint PATH | --remote)".into(),
                );
            }
        }
    }
    let database_id = database_id.ok_or("--database is required")?;
    let started = Instant::now();
    let config = postgres_config_from_env()?;
    if let Some(path) = reference_in {
        use std::io::Read;
        use std::os::unix::fs::OpenOptionsExt;
        if endpoint.is_some() || remote || reference_out.is_some() {
            return Err(
                "reference-in is a read-only invocation; no endpoint/output options".into(),
            );
        }
        let mut bytes = Vec::new();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        require(
            file.metadata()?.is_file(),
            "snapshot reference must be a regular file",
        )?;
        file.take(256 * 1024 + 1).read_to_end(&mut bytes)?;
        let reference = edb_core::SnapshotReference::decode(&bytes)?;
        let selected =
            edb_core::DatabaseCatalog::connect_configured(&config)?.resolve(&database_id)?;
        require(
            reference.database_id() == selected.database_id
                && reference.key().lineage_id() == selected.lineage_id,
            "reference identity differs from selected database",
        )?;
        let value = reference.open(&config, 128, 16 * 1024 * 1024)?;
        require(
            &value.snapshot_key()? == reference.key(),
            "reopened reference has different logical identity",
        )?;
        let summary = calculation(&value)?;
        require(
            summary.total_hours == 8 && summary.projects.len() == 2,
            "handoff did not reproduce the captured application result",
        )?;
        println!(
            "REFERENCE_OK basis_t={} projects=2 hours=8 exact_key=true read_only=true",
            value.basis_t()
        );
        return Ok(());
    }
    if endpoint.is_some() == remote {
        return Err("choose exactly one of --endpoint PATH or --remote".into());
    }
    let remote_client = remote
        .then(edb_core::remote_client_config_from_env)
        .transpose()?;
    let connection = Connection::connect_configured(config.clone(), &database_id, 8)?;
    let endpoint = match (endpoint, remote_client) {
        (Some(path), None) => AppEndpoint::Local(path),
        (None, Some(client)) => {
            AppEndpoint::Remote(connection.remote_writer(config.clone(), client))
        }
        _ => return Err("choose exactly one of --endpoint PATH or --remote".into()),
    };

    // Database provisioning belongs to the operator. These are ordinary application facts.
    let schema = schema()?;
    let schema_request = TransactionRequest::new(
        "application-workflow/schema/v1",
        schema
            .attributes()
            .cloned()
            .map(TxOp::InstallAttribute)
            .chain([source_description()])
            .collect(),
        EntityRef::Temp("application-source".into()),
    );
    connection
        .transact_application(&endpoint, schema_request, WAIT)?
        .report?;
    let request = TransactionRequest::new(
        "application-workflow/seed/v1",
        initial_data(),
        application_source(),
    );
    let seeded = connection.transact_application(&endpoint, request.clone(), WAIT)?;
    let seed_replayed = seeded.replayed;
    let seeded = seeded.report?;
    let replay = connection.transact_application(&endpoint, request, WAIT)?;
    require(
        replay.replayed && replay.tx_hash == seeded.tx_hash,
        "retry changed the receipt",
    )?;
    require(
        replay.report?.tempids == seeded.tempids,
        "retry changed allocated identities",
    )?;

    // Capture the exact seed value from its receipt, including on a later replay after restart.
    let captured = seeded.db_after;
    let old = calculation(&captured)?;
    let fixture_seed = Database::new(schema)?.with(
        &initial_data(),
        1_000,
        EntityRef::Temp("application-source".into()),
    )?;
    check_attribution(
        &fixture_seed.db_after.database_value(),
        &fixture_seed.tx_data,
    )?;
    let fixture = fixture_seed.db_after;
    require(
        calculation(&fixture.database_value())? == old,
        "native/fixture calculation mismatch",
    )?;
    require(
        old.total_hours == 8 && old.projects.len() == 2,
        "unexpected initial calculation",
    )?;
    if let Some(path) = reference_out {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let bytes = captured.snapshot_reference()?.encode()?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        println!(
            "REFERENCE_WRITTEN basis_t={} bytes={} retains_storage=false",
            captured.basis_t(),
            bytes.len()
        );
    }

    let updated = connection.transact_application(
        &endpoint,
        TransactionRequest::new(
            "application-workflow/update/v1",
            update(),
            application_source(),
        ),
        WAIT,
    )?;
    let update_replayed = updated.replayed;
    let updated = updated.report?;
    connection.sync_to(updated.basis_t, WAIT)?;
    // Later runs may already include the planning workflow's later commits.
    // Use the exact receipt value, never substitute a newer connection capture.
    let current = updated.db_after;
    read_values_workflow(&captured, &current)?;
    general_query_workflow(&captured, &current)?;
    application_computation_workflow(&captured, &current)?;
    let fixture_update = fixture.with(&update(), 2_000, application_source())?;
    check_attribution(
        &fixture_update.db_after.database_value(),
        &fixture_update.tx_data,
    )?;
    let expected = fixture_update.db_after.database_value();
    require(
        calculation(&current)? == calculation(&expected)?,
        "updated calculation mismatch",
    )?;
    require(
        calculation(&captured)? == old,
        "retained value changed after commit",
    )?;
    require(
        calculation(&current.clone().as_of(captured.basis_t()))? == old,
        "as-of calculation mismatch",
    )?;

    let events = current
        .clone()
        .history()
        .collect_datoms_with_prefix(&IndexPrefix::Eavt {
            entity: seeded.tempids["alpha"],
            attribute: Some(HOURS),
            value: None,
        })?;
    require(
        events.len() == 3 && events.iter().filter(|event| event.added).count() == 2,
        "history did not retain the assertion and retraction",
    )?;
    let source = current
        .entid(&source_ident())
        .ok_or("application source missing")?;
    require(
        source != edb_core::DB_SOURCE_SYSTEM && events.iter().all(|event| event.source == source),
        "history changed application attribution",
    )?;
    let current_hours = current.collect_datoms_with_prefix(&IndexPrefix::Saevt {
        source,
        attribute: Some(HOURS),
        entity: None,
        value: None,
    })?;
    require(
        current_hours.len() == 2
            && current_hours.iter().all(|datom| datom.added)
            && current_hours
                .iter()
                .map(|datom| &datom.value)
                .collect::<Vec<_>>()
                .iter()
                .all(|value| matches!(value, Value::Long(5) | Value::Long(7))),
        "current source facts did not honor the replacement",
    )?;
    println!(
        "SOURCE_OK ordinary_entity=true receipts=true automatic_timestamp=true speculation=true current=true history=true"
    );
    let reopened = Connection::connect_configured(config, &database_id, 8)?;
    require(
        reopened.identity() == connection.identity(),
        "reopen changed database lineage",
    )?;
    require(
        calculation(&reopened.db())? == calculation(&current)?,
        "reopen changed application facts",
    )?;
    async_read_workflow(&connection, &captured)?;

    // Other workflow reads can evict this old value's selected nodes. Record
    // one explicit rewarm separately; neither enlarge the cache nor hide its I/O.
    let cache_before_warmup = connection.cache_stats();
    let warmup_context = application_context.child(OperationKind::Query);
    let warmup_started = Instant::now();
    {
        let _warmup_scope = warmup_context.enter();
        require(calculation(&captured)? == old, "warmup calculation changed")?;
    }
    let warmup_us = warmup_started.elapsed().as_micros();
    let warmup_sql = warmup_context.snapshot();
    let cache_after_warmup = connection.cache_stats();
    let loop_started = Instant::now();
    let query_context = application_context.child_named(
        OperationKind::Query,
        Keyword::new("app", "captured-calculation"),
    )?;
    {
        let _query_scope = query_context.enter();
        for _ in 0..20 {
            require(
                calculation(&captured)? == old,
                "repeated calculation changed",
            )?;
        }
    }
    let calculation_loop_us = loop_started.elapsed().as_micros();
    let query_sql = query_context.snapshot();
    let cache_after_loop = connection.cache_stats();
    require(
        !query_sql.reads.indexes.is_empty(),
        "named diagnostics omitted index work",
    )?;
    println!("DIAGNOSTICS_OK named=true reads=true immutable_basis=true");
    println!(
        "QUERY_WARMUP calculations=1 elapsed_us={} sql_calls={} errors={} result_cell_bytes={}",
        warmup_us, warmup_sql.sql_calls, warmup_sql.errors, warmup_sql.result_cell_bytes
    );
    println!(
        "QUERY_SQL sql_calls={} errors={} result_cell_bytes={}",
        query_sql.sql_calls, query_sql.errors, query_sql.result_cell_bytes
    );
    println!(
        "QUERY_CACHE before_warmup={cache_before_warmup:?} after_warmup={cache_after_warmup:?} after_loop={cache_after_loop:?}"
    );
    require(
        query_sql.sql_calls == 0 && query_sql.errors == 0 && query_sql.result_cell_bytes == 0,
        "warmed fixture calculation performed PostgreSQL I/O",
    )?;
    let stats_started = Instant::now();
    let stats = current.db_stats()?;
    let stats_scan_us = stats_started.elapsed().as_micros();
    require(
        stats == expected.db_stats()? && stats.attrs[&HOURS].count == 4,
        "native/fixture history statistics mismatch",
    )?;
    let load = connection.load_stats();
    println!(
        "APPLICATION_OK basis_t={} old_basis_t={} seed_replayed={} update_replayed={} projects=2 old_hours=8 current_hours=12 history_events=3",
        current.basis_t(),
        captured.basis_t(),
        seed_replayed,
        update_replayed
    );
    println!(
        "BASELINE elapsed_ms={} repeated_calculations=20 calculation_loop_us={} stats_scan_us={} history_datoms={} rss_kib={:?} connection_cursor_sql_reads={} connection_cursor_sql_bytes={} cache={:?}",
        started.elapsed().as_millis(),
        calculation_loop_us,
        stats_scan_us,
        stats.datoms,
        rss_kib(),
        load.cursor_sql_reads,
        load.cursor_sql_read_bytes,
        connection.cache_stats()
    );
    let application_sql = application_context.snapshot();
    let process_sql = process_sql_stats();
    println!(
        "BLOCK_IO {:?} SSD_CACHE {:?}",
        connection.node_block_read_stats(),
        connection.ssd_cache_stats()
    );
    println!(
        "APPLICATION_SQL sql_calls={} errors={} result_cell_bytes={}",
        application_sql.sql_calls, application_sql.errors, application_sql.result_cell_bytes
    );
    println!(
        "PROCESS_SQL sql_calls={} errors={} result_cell_bytes={} by_operation={:?}",
        process_sql.sql_calls,
        process_sql.errors,
        process_sql.result_cell_bytes,
        process_sql.by_operation
    );
    planning_workflow(&connection, &endpoint, &current)?;
    partition_workflow(&connection, &endpoint)?;
    fulltext_workflow(&connection, &endpoint)?;
    Ok(())
}

fn rss_kib() -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find(|line| line.starts_with("VmRSS:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

fn main() {
    if let Err(error) = run() {
        // A semantic error may contain subject data or credentials in its details.
        if let Some(error) = error.downcast_ref::<SemanticError>() {
            eprintln!(
                "application failed: category={:?} code={}",
                error.category, error.code
            );
            // Only fixed known semantic labels may cross this diagnostics
            // boundary; arbitrary remote strings/details remain redacted.
            if let Some(code) = error.details.get("remote_code") {
                match code.as_str() {
                    "transport/version"
                    | "schema/ident-immutable"
                    | "schema/reserved-system-entity"
                    | "schema/fulltext-upgrade-required"
                    | "schema/fulltext-immutable"
                    | "schema/ident-conflict"
                    | "schema/attribute-exists"
                    | "transaction/unique-conflict" => {
                        eprintln!("remote semantic code={code}");
                    }
                    _ => {}
                }
            }
        } else if let Some(error) = error.downcast_ref::<FixtureFailure>() {
            // This type contains a static assertion label only, never request
            // values, paths, credentials, or arbitrary provider messages.
            eprintln!("application fixture failed: {error}");
        } else {
            eprintln!(
                "application failed; check arguments, endpoint and documented fixture contract"
            );
        }
        std::process::exit(1);
    }
}
