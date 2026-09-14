//! Peer-local entry points and ready-clause execution over explicit immutable sources.
use super::*;

pub struct QueryEngine;

impl QueryEngine {
    pub fn execute(
        query: &Query,
        sources: &[QuerySource],
        inputs: &[QueryInput],
        control: &QueryControl,
    ) -> Result<QueryOutcome, SemanticError> {
        Self::execute_with_extensions(query, sources, inputs, control, None)
    }

    pub fn execute_with_extensions(
        query: &Query,
        sources: &[QuerySource],
        inputs: &[QueryInput],
        control: &QueryControl,
        extensions: Option<&QueryExtensions>,
    ) -> Result<QueryOutcome, SemanticError> {
        let sources: Vec<_> = sources
            .iter()
            .map(|source| QueryDataSource::database(&source.name, source.database.clone()))
            .collect();
        Self::execute_sources_with_extensions(query, &sources, inputs, control, extensions)
    }

    pub fn execute_sources(
        query: &Query,
        sources: &[QueryDataSource],
        inputs: &[QueryInput],
        control: &QueryControl,
    ) -> Result<QueryOutcome, SemanticError> {
        Self::execute_sources_with_extensions(query, sources, inputs, control, None)
    }

    pub fn execute_sources_with_extensions(
        query: &Query,
        sources: &[QueryDataSource],
        inputs: &[QueryInput],
        control: &QueryControl,
        extensions: Option<&QueryExtensions>,
    ) -> Result<QueryOutcome, SemanticError> {
        let (prepared, hit) = prepare::cached(query)?;
        let query = prepared.as_ref().map_or(query, PreparedQuery::query);
        let mut outcome = Self::execute_query(
            query,
            sources,
            inputs,
            control,
            extensions,
            prepared.as_ref(),
        )?;
        outcome.stats.prepared_cache_hits = u64::from(hit);
        Ok(outcome)
    }

    pub(super) fn execute_prepared(
        prepared: &PreparedQuery,
        sources: &[QueryDataSource],
        inputs: &[QueryInput],
        control: &QueryControl,
        extensions: Option<&QueryExtensions>,
    ) -> Result<QueryOutcome, SemanticError> {
        Self::execute_query(
            prepared.query(),
            sources,
            inputs,
            control,
            extensions,
            Some(prepared),
        )
    }

    fn execute_query(
        query: &Query,
        sources: &[QueryDataSource],
        inputs: &[QueryInput],
        control: &QueryControl,
        extensions: Option<&QueryExtensions>,
        prepared: Option<&PreparedQuery>,
    ) -> Result<QueryOutcome, SemanticError> {
        let mut source_map = BTreeMap::new();
        for source in sources {
            if source_map
                .insert(source.name.as_str(), source.borrowed())
                .is_some()
            {
                return Err(SemanticError::incorrect(
                    "query/duplicate-source",
                    format!("source {} is supplied more than once", source.name),
                ));
            }
        }
        let deadline = control
            .timeout
            .and_then(|timeout| Instant::now().checked_add(timeout));
        let mut state = State {
            sources: source_map,
            control,
            deadline,
            work: 0,
            stats: QueryStats::default(),
            plan: Vec::new(),
            extensions,
            rule_memo: BTreeMap::new(),
            negative_memo: BTreeMap::new(),
            solving_rules: false,
            rule_memo_complete: false,
            defer_rules: false,
            borrowed_cancel: None,
            max_value_bytes: control.max_value_bytes,
            diagnostics: control
                .diagnostics
                .map(|options| diagnostics::Trace::new(options, query, control, deadline)),
        };
        let result = run_query(query, inputs, &mut state, prepared)?;
        Ok(QueryOutcome {
            result,
            stats: state.stats,
            plan: state.plan,
            diagnostics: state.diagnostics.map(diagnostics::Trace::finish),
        })
    }

    pub(crate) fn execute_program(
        query: &Query,
        sources: &[QueryDataSource],
        inputs: &[QueryInput],
        budget: &mut crate::ProgramBudget<'_>,
    ) -> Result<QueryOutcome, SemanticError> {
        let control = QueryControl {
            max_work: usize::try_from(budget.remaining_fuel()).unwrap_or(usize::MAX),
            max_intermediate_rows: budget.query_row_limit(),
            max_result_rows: budget.query_row_limit(),
            max_join_bytes: budget.query_remaining_value_bytes().min(4 * 1024 * 1024),
            max_numeric_bytes: budget.query_numeric_bytes(),
            ..QueryControl::default()
        };
        let mut source_map = BTreeMap::new();
        for source in sources {
            if source_map
                .insert(source.name.as_str(), source.borrowed())
                .is_some()
            {
                return Err(SemanticError::incorrect(
                    "query/duplicate-source",
                    "duplicate program query source",
                ));
            }
        }
        let (result, work, bytes) = {
            let mut state = State {
                sources: source_map,
                control: &control,
                deadline: budget.query_deadline(),
                work: 0,
                stats: QueryStats::default(),
                plan: Vec::new(),
                extensions: None,
                rule_memo: BTreeMap::new(),
                negative_memo: BTreeMap::new(),
                solving_rules: false,
                rule_memo_complete: false,
                defer_rules: false,
                borrowed_cancel: budget.query_cancelled(),
                max_value_bytes: budget
                    .query_remaining_value_bytes()
                    .min(control.max_value_bytes),
                diagnostics: None,
            };
            let result = (|| {
                state.check(1)?;
                let (prepared, hit) = prepare::cached(query)?;
                state.stats.prepared_cache_hits = u64::from(hit);
                let result = run_query(
                    prepared.as_ref().map_or(query, PreparedQuery::query),
                    inputs,
                    &mut state,
                    prepared.as_ref(),
                )?;
                Ok(QueryOutcome {
                    result,
                    stats: state.stats.clone(),
                    plan: std::mem::take(&mut state.plan),
                    diagnostics: state.diagnostics.take().map(diagnostics::Trace::finish),
                })
            })();
            (result, state.work as u64, state.stats.allocated_value_bytes)
        };
        // Debit attempted work even if cancellation, a source error, or a query
        // limit interrupted execution. Never grant a fresh interpreter budget.
        let work_charge = budget.charge_query_work(work);
        let bytes_charge = budget.charge_query_bytes(bytes);
        match result {
            Ok(outcome) => {
                work_charge?;
                bytes_charge?;
                Ok(outcome)
            }
            Err(error) => Err(error),
        }
    }
}

pub(super) fn run_query(
    query: &Query,
    inputs: &[QueryInput],
    state: &mut State<'_>,
    prepared: Option<&PreparedQuery>,
) -> Result<QueryResult, SemanticError> {
    state.check(0)?;
    validate_query_values(query, state)?;
    if query.inputs.len() != inputs.len() {
        return Err(SemanticError::incorrect(
            "query/input-arity",
            "query input count does not match its bindings",
        ));
    }
    // These checks depend on the invocation's sources, never cached preparation.
    validate_consumed_sources(query, &state.sources, state.extensions)?;
    match prepared {
        Some(prepared) => prepared.validate_negation(state)?,
        None => dependencies::validate_negation(query, state)?,
    }
    let initial = bind_inputs(&query.inputs, inputs, state)?;
    let rows = dependencies::evaluate_complete(query, initial, state)?;
    let mut pull_budget = QueryPullBudget::new(
        Arc::clone(&state.control.cancel),
        state.deadline,
        state.control.max_work,
        state.work,
    )
    .with_borrowed_cancel(state.borrowed_cancel)
    .with_value_budget(state.stats.allocated_value_bytes, state.max_value_bytes);
    let result = shape_results(
        query,
        rows,
        state.control.max_result_rows,
        &state.sources,
        &mut pull_budget,
        state.control.max_numeric_bytes,
        &custom_aggregate::Context {
            extensions: state.extensions,
            control: state.control,
            deadline: state.deadline,
        },
    );
    state.work = pull_budget.work();
    state.stats.allocated_value_bytes = pull_budget.value_bytes();
    state.stats.work = state.work as u64;
    let result = result?;
    state.stats.rows_produced = result_len(&result) as u64;
    Ok(result)
}

impl DatabaseValue {
    pub fn query(
        &self,
        query: &Query,
        inputs: &[QueryInput],
        control: &QueryControl,
    ) -> Result<QueryOutcome, SemanticError> {
        QueryEngine::execute(
            query,
            &[QuerySource {
                name: "$".into(),
                database: self.clone(),
            }],
            inputs,
            control,
        )
    }

    pub fn query_with_extensions(
        &self,
        query: &Query,
        inputs: &[QueryInput],
        control: &QueryControl,
        extensions: &QueryExtensions,
    ) -> Result<QueryOutcome, SemanticError> {
        QueryEngine::execute_with_extensions(
            query,
            &[QuerySource {
                name: "$".into(),
                database: self.clone(),
            }],
            inputs,
            control,
            Some(extensions),
        )
    }
}

impl Database {
    pub fn query(
        &self,
        query: &Query,
        inputs: &[QueryInput],
        control: &QueryControl,
    ) -> Result<QueryOutcome, SemanticError> {
        self.database_value().query(query, inputs, control)
    }

    pub fn query_with_extensions(
        &self,
        query: &Query,
        inputs: &[QueryInput],
        control: &QueryControl,
        extensions: &QueryExtensions,
    ) -> Result<QueryOutcome, SemanticError> {
        self.database_value()
            .query_with_extensions(query, inputs, control, extensions)
    }
}

pub(super) fn evaluate_clauses(
    clauses: &[Clause],
    rows: Vec<Row>,
    rules: &[Rule],
    inherited_source: Option<&str>,
    state: &mut State<'_>,
) -> EvaluationResult<Vec<Row>> {
    let phase = state
        .diagnostics
        .as_mut()
        .and_then(|trace| trace.enter_phase(clauses));
    let result = evaluate_clauses_in_phase(clauses, rows, rules, inherited_source, state);
    if let Some(trace) = &mut state.diagnostics {
        trace.leave_phase(phase);
    }
    result
}

pub(super) fn evaluate_clauses_in_phase(
    clauses: &[Clause],
    mut rows: Vec<Row>,
    rules: &[Rule],
    inherited_source: Option<&str>,
    state: &mut State<'_>,
) -> EvaluationResult<Vec<Row>> {
    // ATOMIC-NOTE: recovered sched-in-order defers unavailable clauses in
    // source order. Native execution retains its established bound-score choice
    // (later clauses win ties); diagnostics expose the actual schedule.
    let mut remaining: Vec<_> = clauses.iter().collect();
    while !remaining.is_empty() {
        state.check(1)?;
        let sample = rows.first().cloned().unwrap_or_default();
        let mut selected = None;
        for (index, clause) in remaining.iter().enumerate() {
            if clause_ready(clause, &sample, rules, state)? {
                let score = clause_score(clause, &sample);
                if selected.is_none_or(|(_, best)| score >= best) {
                    selected = Some((index, score));
                }
            }
        }
        let selected = selected.map(|(index, _)| index).ok_or_else(|| {
            SemanticError::incorrect(
                "query/insufficient-binding",
                "no remaining clause has its required variables bound",
            )
        })?;
        let clause = remaining.remove(selected);
        let before = rows.len();
        let diagnostic = state.diagnostics.as_mut().map(|trace| {
            trace.begin_step(clause, &rows, inherited_source, state.work, &state.stats)
        });
        let diagnostic_step = diagnostic.as_ref().and_then(|token| token.id);
        let evaluated = evaluate_clause(clause, rows, rules, inherited_source, clauses, state);
        let (next, access) = match evaluated {
            Ok(value) => value,
            Err(error) => {
                if let (Some(trace), Some(token)) = (&mut state.diagnostics, diagnostic) {
                    let status = match &error {
                        dependencies::EvaluationError::AwaitNegative(_) => {
                            QueryStepStatus::AwaitNegative
                        }
                        dependencies::EvaluationError::AwaitRules => QueryStepStatus::AwaitRules,
                        dependencies::EvaluationError::Semantic(_) => QueryStepStatus::Failed,
                    };
                    trace.end_step(token, None, "", status, state.work, &state.stats);
                }
                return Err(error);
            }
        };
        rows = dedupe_rows(next, state)?;
        if rows.len() > state.control.max_intermediate_rows {
            return Err(resource(
                "query/intermediate-limit",
                "query intermediate relation exceeded its row limit",
            )
            .into());
        }
        state.stats.clauses_executed += 1;
        if let (Some(trace), Some(token)) = (&mut state.diagnostics, diagnostic) {
            trace.end_step(
                token,
                Some(&rows),
                &access,
                QueryStepStatus::Complete,
                state.work,
                &state.stats,
            );
        }
        state.plan.push(PlanStep {
            clause: clause_name(clause).into(),
            access,
            rows_before: before,
            rows_after: rows.len(),
            diagnostic_step,
        });
        if rows.is_empty() {
            break;
        }
    }
    Ok(rows)
}

pub(super) fn evaluate_clause(
    clause: &Clause,
    rows: Vec<Row>,
    rules: &[Rule],
    inherited_source: Option<&str>,
    conjunction: &[Clause],
    state: &mut State<'_>,
) -> EvaluationResult<(Vec<Row>, String)> {
    match clause {
        Clause::Pattern(pattern) => Ok(evaluate_pattern(
            pattern,
            rows,
            inherited_source,
            conjunction,
            state,
        )?),
        Clause::RelationPattern(pattern) => Ok(evaluate_relation_pattern(
            pattern,
            rows,
            inherited_source,
            conjunction,
            state,
        )?),
        Clause::Predicate {
            predicate,
            source,
            args,
        } => {
            let mut next = Vec::new();
            for row in rows {
                state.check(1)?;
                let values = resolve_args(args, &row, state)?;
                if evaluate_predicate(
                    *predicate,
                    effective_source(source, inherited_source),
                    &values,
                    state,
                )? {
                    state.push_row(&mut next, row)?;
                }
            }
            Ok((next, "predicate".into()))
        }
        Clause::Function {
            function,
            source,
            args,
            binding,
        } => {
            let mut next = Vec::new();
            for row in rows {
                state.check(1)?;
                let values = resolve_args(args, &row, state)?;
                for produced in evaluate_function(
                    function.clone(),
                    effective_source(source, inherited_source),
                    &values,
                    binding,
                    state,
                )? {
                    for bound in bind_output(&row, binding, &produced, state)? {
                        state.push_row(&mut next, bound)?;
                    }
                }
            }
            let access = match function {
                Function::Extension(name) => state.extensions.map_or_else(
                    || format!("extension {name}"),
                    |registry| registry.description(name),
                ),
                _ => "function".into(),
            };
            Ok((next, access))
        }
        Clause::Not { join, clauses } => {
            let required = join
                .clone()
                .unwrap_or_else(|| variables_in_clauses(clauses));
            let mut next = Vec::new();
            let mut pending = BTreeSet::new();
            for row in rows {
                if required.iter().any(|variable| !row.contains_key(variable)) {
                    return Err(SemanticError::incorrect(
                        "query/insufficient-binding",
                        "not clause has unbound join variables",
                    )
                    .into());
                }
                let seed = if join.is_some() {
                    project_row(&row, &required)
                } else {
                    row.clone()
                };
                match dependencies::evaluate_negative(clause, seed, inherited_source, state)? {
                    dependencies::NegativeStatus::Complete(true) => {
                        state.push_row(&mut next, row)?
                    }
                    dependencies::NegativeStatus::Complete(false) => {}
                    dependencies::NegativeStatus::Pending(key) => {
                        pending.insert(key);
                    }
                }
            }
            if !pending.is_empty() {
                return Err(dependencies::EvaluationError::AwaitNegative(
                    pending.into_iter().collect(),
                ));
            }
            Ok((next, "set-difference".into()))
        }
        Clause::Or { join, branches } => {
            validate_or(branches, join.as_deref())?;
            let mut next = Vec::new();
            let mut pending = BTreeSet::new();
            let mut deferred = false;
            for row in rows {
                for branch in branches {
                    let seed = join
                        .as_ref()
                        .map_or_else(|| row.clone(), |variables| project_row(&row, variables));
                    let produced = match evaluate_clauses(
                        branch,
                        vec![seed],
                        rules,
                        inherited_source,
                        state,
                    ) {
                        Ok(rows) => rows,
                        Err(dependencies::EvaluationError::AwaitNegative(requests)) => {
                            pending.extend(requests);
                            continue;
                        }
                        Err(dependencies::EvaluationError::AwaitRules) => {
                            deferred = true;
                            continue;
                        }
                        Err(error) => return Err(error),
                    };
                    for produced in produced {
                        if let Some(join) = join {
                            let mut merged = row.clone();
                            if join.iter().try_fold(true, |matches, variable| {
                                if !matches {
                                    return Ok(false);
                                }
                                produced.get(variable).map_or(Ok(true), |value| {
                                    unify_variable_checked(&mut merged, variable, value, state)
                                })
                            })? {
                                state.push_row(&mut next, merged)?;
                            }
                        } else {
                            state.push_row(&mut next, produced)?;
                        }
                    }
                }
            }
            if !pending.is_empty() {
                return Err(dependencies::EvaluationError::AwaitNegative(
                    pending.into_iter().collect(),
                ));
            }
            if deferred {
                return Err(dependencies::EvaluationError::AwaitRules);
            }
            Ok((next, "set-union".into()))
        }
        Clause::Rule { source, name, args } => {
            let source = effective_source(source, inherited_source);
            let (arity, required) = rule_signature(rules, name)?;
            if args.len() != arity {
                return Err(SemanticError::incorrect(
                    "query/rule-arity",
                    format!("rule {name} expects {arity} arguments, got {}", args.len()),
                )
                .into());
            }
            let mut invocations = Vec::new();
            for row in rows {
                state.check(1)?;
                if required
                    .iter()
                    .any(|index| !term_is_bound(&args[*index], &row))
                {
                    return Err(SemanticError::incorrect(
                        "query/insufficient-binding",
                        format!("rule {name} requires its declared input positions to be bound"),
                    )
                    .into());
                }
                let key = RuleInvocationKey {
                    source: source.to_owned(),
                    name: name.clone(),
                    bindings: args
                        .iter()
                        .map(|term| term_bound_value(term, &row))
                        .collect(),
                };
                ensure_rule_memo_entry(state, key.clone())?;
                invocations.push((row, key));
            }
            // Demand is a relation, not one invocation at a time. In particular
            // collect every row's negative dependencies before restarting a scan.
            solve_rule_invocations(rules, state)?;
            let mut next = Vec::new();
            for (row, key) in invocations {
                let relation = rule_memo_rows(state, &key)?;
                for tuple in &relation {
                    if tuple.len() != args.len() {
                        return Err(fault(
                            "query/rule-relation-width",
                            "compiled rule relation has the wrong width",
                        )
                        .into());
                    }
                    let mut candidate = row.clone();
                    if args
                        .iter()
                        .zip(tuple)
                        .try_fold(true, |matches, (term, value)| {
                            if matches {
                                unify_term_checked(&mut candidate, term, value, state)
                            } else {
                                Ok(false)
                            }
                        })?
                    {
                        state.push_row(&mut next, candidate)?;
                    }
                }
            }
            Ok((next, "rule-relation".into()))
        }
    }
}

pub(super) fn clause_ready(
    clause: &Clause,
    row: &Row,
    rules: &[Rule],
    state: &mut State<'_>,
) -> Result<bool, SemanticError> {
    Ok(match clause {
        Clause::Pattern(_) | Clause::RelationPattern(_) => true,
        Clause::Or { .. } => dependencies::ready(clause, row, rules, state)?,
        Clause::Predicate { args, .. } | Clause::Function { args, .. } => args
            .iter()
            .all(|term| !matches!(term, Term::Variable(variable) if !row.contains_key(variable))),
        Clause::Not { join, clauses } => join
            .clone()
            .unwrap_or_else(|| variables_in_clauses(clauses))
            .iter()
            .all(|variable| row.contains_key(variable)),
        Clause::Rule { name, args, .. } => {
            match rule_signature(rules, name) {
                Ok((arity, required)) => {
                    args.len() != arity
                        || required
                            .iter()
                            .all(|index| term_is_bound(&args[*index], row))
                }
                // Let evaluation produce the precise unknown-rule anomaly.
                Err(_) => true,
            }
        }
    })
}

pub(super) fn clause_score(clause: &Clause, row: &Row) -> usize {
    match clause {
        Clause::RelationPattern(pattern) => pattern
            .terms
            .iter()
            .filter(|term| term_is_bound(term, row))
            .count()
            .saturating_mul(10),
        Clause::Pattern(pattern) => {
            [&pattern.entity, &pattern.attribute, &pattern.value]
                .into_iter()
                .filter(|term| term_is_bound(term, row))
                .count()
                * 10
        }
        Clause::Predicate { .. } | Clause::Not { .. } => 40,
        Clause::Function { .. } => 30,
        Clause::Rule { .. } => 20,
        Clause::Or { .. } => 10,
    }
}

pub(super) fn clause_name(clause: &Clause) -> &'static str {
    match clause {
        Clause::Pattern(_) => "pattern",
        Clause::RelationPattern(_) => "relation pattern",
        Clause::Predicate { .. } => "predicate",
        Clause::Function { .. } => "function",
        Clause::Not { .. } => "not",
        Clause::Or { .. } => "or",
        Clause::Rule { .. } => "rule",
    }
}
