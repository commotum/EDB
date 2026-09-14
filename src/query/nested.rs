//! Subqueries reuse the enclosing budget and immutable named database values.
use super::*;

pub(super) fn execute(
    query: &Query,
    source: &str,
    args: &[BoundValue],
    binding: &Binding,
    parent: &mut State<'_>,
) -> Result<Vec<Vec<BoundValue>>, SemanticError> {
    let (prepared, _) = prepare::cached(query)?;
    let query = prepared.as_ref().map_or(query, PreparedQuery::query);
    if query.inputs.len() != args.len() {
        return Err(SemanticError::incorrect(
            "query/input-arity",
            "query input count does not match its bindings",
        ));
    }
    let mut sources = parent.sources.clone();
    if let Some(default) = parent.sources.get(source).copied() {
        sources.insert("$", default);
    } else {
        sources.remove("$");
    }
    validate_consumed_sources(query, &sources, parent.extensions)?;
    if let Some(trace) = &mut parent.diagnostics {
        trace.nested(query);
    }
    let mut child = State {
        sources,
        control: parent.control,
        deadline: parent.deadline,
        work: parent.work,
        stats: QueryStats::default(),
        plan: Vec::new(),
        extensions: parent.extensions,
        rule_memo: BTreeMap::new(),
        negative_memo: BTreeMap::new(),
        solving_rules: false,
        rule_memo_complete: false,
        defer_rules: false,
        borrowed_cancel: parent.borrowed_cancel,
        max_value_bytes: parent
            .max_value_bytes
            .saturating_sub(parent.stats.allocated_value_bytes),
        diagnostics: parent.diagnostics.take(),
    };
    let result = (|| {
        match &prepared {
            Some(prepared) => prepared.validate_negation(&mut child)?,
            None => dependencies::validate_negation(query, &mut child)?,
        }
        let initial = bind_arguments(&query.inputs, args, &mut child)?;
        let rows = dependencies::evaluate_complete(query, initial, &mut child)?;
        let mut budget = QueryPullBudget::new(
            Arc::clone(&parent.control.cancel),
            parent.deadline,
            parent.control.max_work,
            child.work,
        )
        .with_borrowed_cancel(parent.borrowed_cancel)
        .with_value_budget(child.stats.allocated_value_bytes, child.max_value_bytes);
        let result = shape_results(
            query,
            rows,
            parent.control.max_result_rows,
            &child.sources,
            &mut budget,
            parent.control.max_numeric_bytes,
            &custom_aggregate::Context {
                extensions: parent.extensions,
                control: parent.control,
                deadline: parent.deadline,
            },
        );
        child.work = budget.work();
        child.stats.allocated_value_bytes = budget.value_bytes();
        result
    })();
    parent.work = child.work;
    parent.diagnostics = child.diagnostics.take();
    // q returns one value in its find-selected shape. The enclosing function
    // binding decides whether to keep that value or destructure it once.
    let bytes_charge = parent.charge_value_bytes(child.stats.allocated_value_bytes);
    parent.stats.clauses_executed += child.stats.clauses_executed;
    parent.stats.datoms_examined += child.stats.datoms_examined;
    parent.stats.index_seeks += child.stats.index_seeks;
    parent.stats.rule_iterations += child.stats.rule_iterations;
    parent.stats.hash_join_build_rows += child.stats.hash_join_build_rows;
    parent.stats.hash_join_probes += child.stats.hash_join_probes;
    parent.stats.join_candidates += child.stats.join_candidates;
    parent.stats.grouped_probes_saved += child.stats.grouped_probes_saved;
    parent.stats.fulltext_searches += child.stats.fulltext_searches;
    parent.stats.fulltext_lagging_searches += child.stats.fulltext_lagging_searches;
    parent.stats.fulltext_truncated_searches += child.stats.fulltext_truncated_searches;
    parent.stats.fulltext_read_bytes += child.stats.fulltext_read_bytes;
    parent.stats.peak_join_bytes = parent
        .stats
        .peak_join_bytes
        .max(child.stats.peak_join_bytes);
    let result = result?;
    bytes_charge?;
    let value = match result {
        QueryResult::Relation(rows) => {
            QueryValue::Collection(rows.into_iter().map(QueryValue::Tuple).collect())
        }
        QueryResult::Tuple(row) => row.map_or(QueryValue::Nil, QueryValue::Tuple),
        QueryResult::Collection(values) => QueryValue::Collection(values),
        QueryResult::Scalar(value) => value.unwrap_or(QueryValue::Nil),
    };
    parent.plan.extend(child.plan.into_iter().map(|mut step| {
        step.clause.insert_str(0, "nested/");
        step
    }));
    ground_output(&[BoundValue::from_query_value(value)], binding)
}

fn bind_arguments(
    specs: &[InputSpec],
    args: &[BoundValue],
    state: &mut State<'_>,
) -> Result<Vec<Row>, SemanticError> {
    let mut rows = vec![Row::new()];
    for (spec, arg) in specs.iter().zip(args) {
        let binding = match spec {
            InputSpec::Scalar(variable) => Binding::Scalar(variable.clone()),
            InputSpec::Tuple(variables) => Binding::Tuple(variables.clone()),
            InputSpec::Collection(variable) => Binding::Collection(variable.clone()),
            InputSpec::Relation(variables) => Binding::Relation(variables.clone()),
        };
        let relation = ground_output(std::slice::from_ref(arg), &binding)?;
        let mut next = Vec::new();
        for row in &rows {
            for tuple in &relation {
                state.check(1)?;
                let bound = bind_output(row, &binding, tuple, state)?;
                state.check(bound.len())?;
                for row in bound {
                    state.push_row(&mut next, row)?;
                }
            }
        }
        rows = dedupe_rows(next, state)?;
        if rows.len() > state.control.max_intermediate_rows {
            return Err(resource(
                "query/intermediate-limit",
                "nested input exceeded its intermediate row limit",
            ));
        }
    }
    state.check(0)?;
    Ok(rows)
}
