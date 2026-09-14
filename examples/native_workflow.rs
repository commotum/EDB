//! A repeatable public-API workflow. Run against a disposable PostgreSQL database:
//! ATOMIC_POSTGRES_URL='host=... user=... dbname=...' cargo run --example native_workflow
use atomic_core::{
    Attribute, AttributeName, CallableRef, CapacityLimits, Cardinality, Clause, Connection,
    DataPattern, EntityRef, FindElement, FindSpec, IndexComponents, IndexOrder, IndexPullOptions,
    IndexTransaction, Instruction, Keyword, PostgresConnectionConfig, Program, ProgramCall,
    ProgramKind, PullAttribute, PullPattern, PullTransform, Query, QueryControl, QueryResult,
    QueryValue, ReturnMapShape, Schema, Term, TimePoint, TransactionRequest, TransactionService,
    TransactionServiceConfig, TupleSpec, TxForm, TxOp, TxValue, Unique, Value, ValueType, Variable,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const NAME: u32 = 1_000;
const REGISTRATION: u32 = 1_001;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let postgres = std::env::var("ATOMIC_POSTGRES_URL")?;
    let database_id = format!(
        "workflow-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );
    // Provisioning is explicit. All subsequent application work uses Connection.
    let storage = PostgresConnectionConfig::plaintext(&postgres);
    atomic_core::storage::PgBlockStore::install(&storage)?;
    atomic_core::storage::BlockDatabase::create(&storage, &database_id, Schema::new())?;
    let rename_program = Program {
        kind: ProgramKind::Transaction,
        arity: 2,
        instructions: vec![
            Instruction::PushArgument(0),
            Instruction::PushArgument(1),
            Instruction::EmitAdd(NAME),
            Instruction::Return,
        ],
    };
    let mut operator = atomic_core::PostgresOperator::connect_configured(&storage)?;
    let config = TransactionServiceConfig {
        connection: storage.clone(),
        database_id: database_id.clone(),
        holder_id: format!("workflow-{}", std::process::id()),
        lease_duration: Duration::from_secs(5),
        renew_interval: Duration::from_millis(100),
        queue_capacity: 32,
        capacity_limits: CapacityLimits::default(),
    };
    let writer = TransactionService::start(config.clone())?;
    let connection = Connection::attach_configured(config.connection.clone(), writer.client(), 64)?;
    let reader = Connection::connect_configured(config.connection.clone(), &database_id, 64)?;
    let timeout = Duration::from_secs(10);
    let mut name_attribute = Attribute::new(
        NAME,
        Keyword::new("person", "name"),
        ValueType::String,
        Cardinality::One,
    );
    name_attribute.indexed = true;
    connection.transact(
        TransactionRequest::new(
            "schema",
            vec![
                TxOp::InstallAttribute(name_attribute),
                TxOp::InstallAttribute(
                    Attribute::new(
                        REGISTRATION,
                        Keyword::new("registration", "key"),
                        ValueType::Tuple,
                        Cardinality::One,
                    )
                    .tuple(TupleSpec::Heterogeneous(vec![
                        ValueType::Ref,
                        ValueType::Long,
                    ]))
                    .unique(Unique::Identity),
                ),
            ],
        ),
        timeout,
    )?;
    operator.install_program(
        &writer.client(),
        "install-rename",
        Keyword::new("person", "rename"),
        &rename_program,
        &[],
        timeout,
    )?;
    let inserted = connection.transact(
        TransactionRequest::new(
            "insert",
            vec![
                TxOp::Add {
                    entity: EntityRef::Temp("person".into()),
                    attribute: NAME,
                    value: Value::String("Ada".into()).into(),
                },
                TxOp::Add {
                    entity: EntityRef::Temp("person".into()),
                    attribute: atomic_core::DB_IDENT as u32,
                    value: Value::Keyword(Keyword::new("person", "ada")).into(),
                },
            ],
        ),
        timeout,
    )?;
    let person = inserted.tempids["person"];
    let original = connection.db();
    let variable = Variable::new("name")?;
    let query = Query::new(
        FindSpec::Collection(FindElement::Variable(variable.clone())),
        vec![Clause::Pattern(Box::new(DataPattern::new(
            Term::Constant(Value::Ref(person)),
            Term::Constant(Value::Keyword(Keyword::new("person", "name"))),
            Term::Variable(variable),
        )))],
    );
    let names = |database: &atomic_core::DatabaseValue| {
        database
            .query(&query, &[], &QueryControl::default())
            .map(|outcome| outcome.result)
    };
    assert_eq!(
        names(&original)?,
        QueryResult::Collection(vec![QueryValue::Scalar(Value::String("Ada".into()))])
    );
    let pattern = PullPattern {
        wildcard: false,
        attributes: vec![PullAttribute::forward(AttributeName::Ident(Keyword::new(
            "person", "name",
        )))],
    };
    let original_pull = original.pull(&pattern, person)?;
    // Log capture and index-pull retain this exact endpoint, not the mutable
    // connection. Neither cursor is consumed until after a later commit and
    // writer shutdown below.
    let original_log = connection.log();
    assert_eq!(original_log.basis_t(), original.basis_t());
    let mut original_transactions =
        original_log.tx_range(Some(TimePoint::T(inserted.basis_t)), None)?;
    assert_eq!(original_transactions.stats().range_reads, 0);
    let start = original.avet_boundary(IndexComponents::One(AttributeName::Ident(
        Keyword::new("person", "name"),
    )))?;
    let mut original_index_pull =
        original.index_pull(IndexPullOptions::new(start, pattern.clone()))?;

    let person_variable = Variable::new("person")?;
    let name_variable = Variable::new("name")?;
    let relation_clause = Clause::Pattern(Box::new(DataPattern::new(
        Term::Variable(person_variable.clone()),
        Term::Constant(Value::Keyword(Keyword::new("person", "name"))),
        Term::Variable(name_variable.clone()),
    )));
    let relation = Query::new(
        FindSpec::Relation(vec![
            FindElement::Variable(person_variable.clone()),
            FindElement::Variable(name_variable),
        ]),
        vec![relation_clause.clone()],
    );
    let return_keys = || {
        vec![
            Value::Keyword(Keyword::new("person", "id")),
            Value::String("name".into()),
        ]
    };
    let original_rows = original
        .query(&relation, &[], &QueryControl::default())?
        .result
        .into_return_maps_with_arity(return_keys(), 2)?;
    assert_eq!(original_rows.shape(), ReturnMapShape::Relation);
    assert_eq!(original_rows.len(), 1);
    assert_eq!(original_rows[0][0], QueryValue::Scalar(Value::Ref(person)));
    assert_eq!(
        original_rows[0].get(&Value::String("name".into())),
        Some(&QueryValue::Scalar(Value::String("Ada".into())))
    );

    // Joins are prepared now; the pull transform is deferred until the one
    // result row is consumed. The counter makes that distinction observable.
    let transforms = Arc::new(AtomicUsize::new(0));
    let observed_transforms = Arc::clone(&transforms);
    let mut transformed_name = PullAttribute::forward(AttributeName::Id(NAME));
    transformed_name.transform = Some(PullTransform::new("workflow/captured-name", move |value| {
        observed_transforms.fetch_add(1, Ordering::Relaxed);
        let QueryValue::Scalar(Value::String(name)) = value else {
            return Err(atomic_core::SemanticError::incorrect(
                "workflow/name-type",
                "expected a person name",
            ));
        };
        Ok(QueryValue::Scalar(Value::String(format!(
            "captured:{name}"
        ))))
    }));
    let sequence_query = Query::new(
        FindSpec::Relation(vec![
            FindElement::Variable(person_variable.clone()),
            FindElement::Pull {
                source: "$".into(),
                variable: person_variable,
                pattern: Box::new(PullPattern::attributes(vec![transformed_name])),
            },
        ]),
        vec![relation_clause],
    );
    let mut deferred = original.query_sequence(&sequence_query, &[], &QueryControl::default())?;
    assert_eq!(deferred.remaining_rows(), 1);
    assert_eq!(transforms.load(Ordering::Relaxed), 0);
    // Pure native branches can be queried, navigated, and extended without
    // advancing either peer or the durable writer.
    let speculative_instant = original.last_tx_instant().unwrap_or(0) + 1;
    let rename_form = |name: &str| {
        TxForm::ProgramCall(ProgramCall {
            function: CallableRef::Database(EntityRef::Ident(Keyword::new("person", "rename"))),
            arguments: vec![
                EntityRef::Id(person).into(),
                Value::String(name.into()).into(),
            ],
        })
    };
    let registration_key = || {
        TxValue::Tuple(vec![
            Some(TxValue::Entity(EntityRef::Ident(Keyword::new(
                "person", "ada",
            )))),
            Some(Value::Long(2026).into()),
        ])
    };
    let registration = TxForm::Op(TxOp::Add {
        entity: EntityRef::Temp("registration".into()),
        attribute: REGISTRATION,
        value: registration_key(),
    });
    let speculative = original.with_forms(
        &[rename_form("Hypothetical Ada"), registration.clone()],
        speculative_instant,
    )?;
    let chained = speculative.db_after.with(
        &[TxOp::Add {
            entity: EntityRef::LookupInput {
                attribute: REGISTRATION,
                value: Box::new(registration_key()),
            },
            attribute: NAME,
            value: Value::String("Hypothetical registration".into()).into(),
        }],
        speculative_instant + 1,
    )?;
    assert_ne!(names(&chained.db_after)?, names(&original)?);
    assert_ne!(chained.db_after.pull(&pattern, person)?, original_pull);
    assert_eq!(connection.db().basis_t(), original.basis_t());
    assert_eq!(original.pull(&pattern, person)?, original_pull);
    let changed = connection.transact(
        TransactionRequest::from_forms("rename", vec![rename_form("Ada Lovelace"), registration]),
        timeout,
    )?;
    assert_eq!(original.pull(&pattern, person)?, original_pull);
    assert_eq!(
        connection
            .db()
            .as_of(original.basis_t())
            .pull(&pattern, person)?,
        original_pull
    );
    let history: Vec<_> = connection
        .db()
        .history()
        .scan_cursor(IndexOrder::Eavt)?
        .filter_map(|row| match row {
            Ok(datom) if datom.entity == person && datom.attribute == NAME => Some(Ok(datom)),
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .collect::<Result<_, _>>()?;
    assert_eq!(history.len(), 3);
    assert_eq!(history.iter().filter(|datom| !datom.added).count(), 1);
    let expected = names(&connection.db())?;
    assert_eq!(
        expected,
        QueryResult::Collection(vec![QueryValue::Scalar(Value::String(
            "Ada Lovelace".into()
        ))])
    );
    let identity = connection.identity().clone();
    assert_eq!(names(&reader.sync_to(changed.basis_t, timeout)?)?, expected);
    drop(connection);
    writer.shutdown();
    // The producer is gone, but captured log/index values and prepared
    // sequences still read their original immutable database, including pull.
    assert_eq!(transforms.load(Ordering::Relaxed), 0);
    let deferred_row = deferred.next().transpose()?.expect("one prepared person");
    assert_eq!(deferred_row[0], QueryValue::Scalar(Value::Ref(person)));
    assert_eq!(
        deferred_row[1],
        QueryValue::Map(vec![(
            QueryValue::Scalar(Value::Keyword(Keyword::new("person", "name"))),
            QueryValue::Scalar(Value::String("captured:Ada".into())),
        )])
    );
    assert_eq!(transforms.load(Ordering::Relaxed), 1);
    assert!(deferred.next().is_none());
    assert_eq!(
        original_index_pull.next().transpose()?,
        Some(original_pull.clone())
    );
    assert!(original_index_pull.next().is_none());
    let original_transaction = original_transactions
        .next()
        .transpose()?
        .expect("insert is in captured log");
    assert_eq!(original_transaction.t, inserted.basis_t);
    assert_eq!(original_transaction.data, inserted.tx_data);
    assert!(original_transactions.next().is_none());
    assert_eq!(original_log.basis_t(), original.basis_t());
    assert!(
        original_log
            .tx_data(IndexTransaction::T(changed.basis_t))?
            .is_none()
    );
    let latest_log = reader.log();
    assert_eq!(latest_log.basis_t(), changed.basis_t);
    let latest_transactions = latest_log
        .tx_range(Some(TimePoint::T(inserted.basis_t)), None)?
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(
        latest_transactions
            .iter()
            .map(|transaction| transaction.t)
            .collect::<Vec<_>>(),
        [inserted.basis_t, changed.basis_t]
    );
    assert_eq!(latest_transactions[1].data, changed.tx_data);
    // Reads and reopening do not acquire or retain a writer lease.
    assert_eq!(names(&reader.db())?, expected);
    let reopened = Connection::connect_configured(config.connection.clone(), &database_id, 64)?;
    assert_eq!(reopened.identity(), &identity);
    assert_eq!(reopened.db().basis_t(), changed.basis_t);
    assert_eq!(names(&reopened.db())?, expected);
    assert_eq!(original.pull(&pattern, person)?, original_pull);
    let reopened_database = reopened.db();
    let start = reopened_database.avet_boundary(IndexComponents::One(AttributeName::Id(NAME)))?;
    assert_eq!(
        reopened_database
            .index_pull(IndexPullOptions::new(start, pattern.clone()))?
            .collect::<Result<Vec<_>, _>>()?,
        vec![reopened_database.pull(&pattern, person)?],
    );
    let current_rows = reopened_database
        .query(&relation, &[], &QueryControl::default())?
        .result
        .into_return_maps_with_arity(return_keys(), 2)?;
    assert_eq!(
        current_rows[0].get(&Value::String("name".into())),
        Some(&QueryValue::Scalar(Value::String("Ada Lovelace".into())))
    );
    assert_eq!(
        original_rows[0].get(&Value::String("name".into())),
        Some(&QueryValue::Scalar(Value::String("Ada".into())))
    );
    println!(
        "PASS {database_id}: schema, controlled transact, tuple references, lookup keys, query, pull, immutable log, index-pull, deferred query transforms, return maps, history, chained native speculation, immutable values, reopen at t={}",
        changed.basis_t
    );
    Ok(())
}
