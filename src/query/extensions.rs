//! Caller-owned cooperative callbacks and controlled native query programs.
use super::*;

type NativeQueryFunction = dyn Fn(&DatabaseValue, &[Value], &QueryControl) -> Result<Vec<Vec<Value>>, SemanticError>
    + Send
    + Sync;
type PureQueryFunction =
    dyn Fn(&[QueryValue], &QueryControl) -> Result<QueryValue, SemanticError> + Send + Sync;

#[derive(Clone)]
pub(super) struct QueryExtension {
    pub(super) implementation: QueryExtensionImplementation,
    pub(super) exact_hash: Option<ProgramHash>,
}

#[derive(Clone)]
pub(super) enum QueryExtensionImplementation {
    Local(Arc<NativeQueryFunction>),
    Pure(Arc<PureQueryFunction>),
    Program(Arc<Program>),
}

#[derive(Clone, Default)]
pub struct QueryExtensions {
    pub(super) functions: BTreeMap<String, QueryExtension>,
    pub(super) aggregates: BTreeMap<String, Arc<custom_aggregate::NativeAggregate>>,
}

impl fmt::Debug for QueryExtensions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_map()
            .entries(
                self.functions
                    .iter()
                    .map(|(name, function)| (name, function.exact_hash)),
            )
            .entries(
                self.aggregates
                    .keys()
                    .map(|name| (name, None::<ProgramHash>)),
            )
            .finish()
    }
}

impl QueryExtensions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a trusted, cooperative grouped Rust aggregate. Input rows are
    /// borrowed and aligned; constants and sources remain explicit arguments.
    /// Call `group.check(work)` inside substantial callback loops to share the
    /// enclosing query's work, cancellation and deadline checks. This callback
    /// is process-local code, not a content-addressed portable program.
    pub fn register_aggregate<F>(&mut self, name: impl Into<String>, function: F)
    where
        F: Fn(&AggregateGroup<'_>, &QueryControl) -> Result<QueryValue, SemanticError>
            + Send
            + Sync
            + 'static,
    {
        self.aggregates.insert(name.into(), Arc::new(function));
    }

    /// Register a trusted, cooperative pure Rust function. Its general query
    /// value result is interpreted using the caller's binding form; no database
    /// argument or database source is required. Panics become fault anomalies.
    pub fn register_pure<F>(&mut self, name: impl Into<String>, function: F)
    where
        F: Fn(&[QueryValue], &QueryControl) -> Result<QueryValue, SemanticError>
            + Send
            + Sync
            + 'static,
    {
        self.functions.insert(
            name.into(),
            QueryExtension {
                implementation: QueryExtensionImplementation::Pure(Arc::new(function)),
                exact_hash: None,
            },
        );
    }

    /// Register trusted peer-local Rust code. The callback receives remaining
    /// work/time allowances; cancellation and deadline checks surround it.
    /// Arbitrary Rust code is cooperative, not forcibly preempted or metered.
    pub fn register_local<F>(&mut self, name: impl Into<String>, function: F)
    where
        F: Fn(&DatabaseValue, &[Value], &QueryControl) -> Result<Vec<Vec<Value>>, SemanticError>
            + Send
            + Sync
            + 'static,
    {
        self.functions.insert(
            name.into(),
            QueryExtension {
                implementation: QueryExtensionImplementation::Local(Arc::new(function)),
                exact_hash: None,
            },
        );
    }

    /// Register controlled native bytecode. Interpreter fuel is charged to
    /// the enclosing query, and siblings share its original absolute deadline.
    pub fn register_program(
        &mut self,
        name: impl Into<String>,
        hash: ProgramHash,
        program: Program,
    ) -> Result<(), SemanticError> {
        if program.kind != ProgramKind::Query {
            return Err(SemanticError::incorrect(
                "query/not-query-program",
                "persisted query extension must have query program kind",
            ));
        }
        program.validate()?;
        self.functions.insert(
            name.into(),
            QueryExtension {
                implementation: QueryExtensionImplementation::Program(Arc::new(program)),
                exact_hash: Some(hash),
            },
        );
        Ok(())
    }

    pub(super) fn invoke(
        &self,
        name: &str,
        database: &DatabaseValue,
        arguments: &[QueryValue],
        control: &QueryControl,
        deadline: Option<Instant>,
    ) -> (Result<Vec<Vec<QueryValue>>, SemanticError>, usize) {
        let mut work = 0;
        let result = catch_unwind(AssertUnwindSafe(|| {
            let extension = self.functions.get(name).ok_or_else(|| {
                SemanticError::incorrect(
                    "query/unknown-extension",
                    format!("unknown query extension {name}"),
                )
            })?;
            match &extension.implementation {
                QueryExtensionImplementation::Local(callback) => {
                    let arguments = arguments
                        .iter()
                        .map(|value| match value {
                            QueryValue::Scalar(value) => Ok(value.clone()),
                            _ => Err(SemanticError::incorrect(
                                "query/nil-extension-arg",
                                "local database callbacks require stored scalar arguments",
                            )),
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    callback(database, &arguments, control).map(|rows| {
                        rows.into_iter()
                            .map(|row| row.into_iter().map(QueryValue::Scalar).collect())
                            .collect()
                    })
                }
                QueryExtensionImplementation::Pure(_) => {
                    unreachable!("pure callback does not consume a database")
                }
                QueryExtensionImplementation::Program(program) => {
                    if control.max_work == 0 {
                        return Err(resource(
                            "query/work-limit",
                            "query has no remaining program fuel",
                        ));
                    }
                    let fuel = u64::try_from(control.max_work).unwrap_or(u64::MAX);
                    let program_control = ProgramControl {
                        fuel,
                        max_stack: control.max_intermediate_rows,
                        max_value_bytes: control.max_work.min(control.max_value_bytes),
                        max_collection_items: control.max_intermediate_rows,
                        max_forms: control.max_result_rows,
                        max_output: control.max_work,
                        max_calls: 1,
                        cancelled: Some(control.cancel.as_ref()),
                    };
                    let mut budget = crate::ProgramBudget::new(program_control)?
                        .with_deadline(deadline)
                        .with_query_numeric_bytes(control.max_numeric_bytes);
                    // Keep the established scalar ABI/fuel path unchanged.
                    // General admission is necessary only for newly admitted
                    // query-only arguments, not a tax on all old invocations.
                    let result = if arguments
                        .iter()
                        .all(|value| matches!(value, QueryValue::Scalar(_)))
                    {
                        let arguments = arguments
                            .iter()
                            .map(|value| match value {
                                QueryValue::Scalar(value) => value.clone(),
                                _ => unreachable!(),
                            })
                            .collect::<Vec<_>>();
                        ProgramRuntime.execute_query_with_budget(
                            program,
                            database,
                            &arguments,
                            &mut budget,
                        )
                    } else {
                        ProgramRuntime.execute_query_general_with_budget(
                            program,
                            database,
                            arguments,
                            &mut budget,
                        )
                    };
                    work = usize::try_from(fuel - budget.remaining_fuel()).unwrap_or(usize::MAX);
                    match result {
                        Ok(ProgramOutput::Query(rows)) => Ok(rows
                            .into_iter()
                            .map(|row| row.into_iter().map(QueryValue::Scalar).collect())
                            .collect()),
                        Ok(ProgramOutput::GeneralQuery(rows)) => Ok(rows),
                        Ok(_) => unreachable!("program kind was checked"),
                        Err(error) if error.code == "program/fuel-exhausted" => Err(resource(
                            "query/work-limit",
                            "query program exhausted the remaining shared work",
                        )),
                        Err(error) => Err(error),
                    }
                }
            }
        }))
        .map_err(|_| {
            SemanticError::new(
                ErrorCategory::Fault,
                "query/local-extension-panicked",
                format!("query extension {name} panicked"),
            )
        })
        .and_then(|result| result);
        (result, work)
    }

    pub(super) fn description(&self, name: &str) -> String {
        match self
            .functions
            .get(name)
            .and_then(|function| function.exact_hash)
        {
            Some(hash) => format!(
                "persisted extension {}",
                hash.iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            ),
            None => format!("local extension {name}"),
        }
    }
}
