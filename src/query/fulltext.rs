//! Fulltext combines persisted candidates with a bounded committed-tail search,
//! then checks matches against the supplied immutable database value.
use super::*;

pub(super) fn execute(
    source: &str,
    args: &[BoundValue],
    binding: &Binding,
    state: &mut State<'_>,
) -> Result<Vec<Vec<BoundValue>>, SemanticError> {
    let [attribute, search] = args else {
        return Err(SemanticError::incorrect(
            "query/function-arity",
            "fulltext requires attribute and search string",
        ));
    };
    let database = source_database(state, source)?.clone();
    let attribute = attribute_value(
        &database,
        require_stored(attribute, "query/attribute-value")?,
    )?;
    let BoundValue::Stored(Value::String(search)) = search else {
        return Err(SemanticError::incorrect(
            "query/fulltext-search",
            "fulltext search must be a string",
        ));
    };
    let mut options = crate::FulltextOptions::default();
    options.max_work = options
        .max_work
        .min(state.control.max_work.saturating_sub(state.work));
    options.max_bytes = options.max_bytes.min(
        state
            .max_value_bytes
            .saturating_sub(state.stats.allocated_value_bytes),
    );
    if let Some(deadline) = state.deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        options.timeout = Some(
            options
                .timeout
                .map_or(remaining, |timeout| timeout.min(remaining)),
        );
    }
    state.check(0)?;
    state.stats.fulltext_searches = state.stats.fulltext_searches.saturating_add(1);
    let report =
        database.fulltext_with_budget(attribute, search, &options, &mut |work, bytes| {
            // Debit attempted work on failures too; a caught failed stored query
            // cannot reset its enclosing transaction's shared fuel or byte budget.
            let work_result = state.check(work);
            let bytes_result = state.charge_value_bytes(bytes);
            work_result?;
            bytes_result
        })?;
    state.stats.fulltext_read_bytes = state
        .stats
        .fulltext_read_bytes
        .saturating_add(report.stats.read_bytes);
    state.stats.fulltext_lagging_searches = state
        .stats
        .fulltext_lagging_searches
        .saturating_add(u64::from(report.stats.index_basis_t < database.basis_t()));
    state.stats.fulltext_truncated_searches = state
        .stats
        .fulltext_truncated_searches
        .saturating_add(u64::from(report.stats.truncated));
    let mut rows = Vec::new();
    for hit in report.hits {
        state.check(1)?;
        state.check_row_count(rows.len().saturating_add(1))?;
        state.charge_value_bytes(4 * std::mem::size_of::<BoundValue>())?;
        rows.push(vec![
            BoundValue::Stored(Value::Ref(hit.entity)),
            BoundValue::Stored(Value::String(hit.value)),
            BoundValue::Stored(Value::Ref(hit.tx)),
            BoundValue::Stored(Value::Double(hit.score)),
        ]);
    }
    if matches!(binding, Binding::Relation(_)) {
        return Ok(rows);
    }
    // Like tx-data and nested queries, a caller may bind the complete
    // relation as a scalar or destructure its row collection explicitly.
    state.charge_value_bytes(rows.iter().flatten().fold(0usize, |bytes, value| {
        bytes.saturating_add(join::bound_bytes(value))
    }))?;
    let relation = QueryValue::Collection(
        rows.into_iter()
            .map(|row| {
                QueryValue::Tuple(row.into_iter().map(|value| value.query_value()).collect())
            })
            .collect(),
    );
    ground_output(&[BoundValue::Query(relation)], binding)
}
