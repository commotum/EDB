//! Find/:with basis formation, grouping and final Pull projection preserve result shape.
use super::*;

pub(super) fn shape_results(
    query: &Query,
    rows: Vec<Row>,
    max: usize,
    sources: &BTreeMap<&str, SourceRef<'_>>,
    pull_budget: &mut QueryPullBudget,
    max_numeric_bytes: usize,
    aggregates: &custom_aggregate::Context<'_>,
) -> Result<QueryResult, SemanticError> {
    let elements = match &query.find {
        FindSpec::Relation(elements) | FindSpec::Tuple(elements) => elements.as_slice(),
        FindSpec::Collection(element) | FindSpec::Scalar(element) => std::slice::from_ref(element),
    };
    let basis_variables = find_variables(elements)
        .into_iter()
        .chain(query.with.iter().cloned())
        .collect::<Vec<_>>();
    let mut basis = Vec::new();
    for row in rows {
        pull_budget.check(1)?;
        let projected = basis_variables
            .iter()
            .map(|variable| {
                let value = row.get(variable).ok_or_else(|| {
                    SemanticError::incorrect(
                        "query/unbound-find-variable",
                        format!("find variable {} is unbound", variable.name()),
                    )
                })?;
                pull_budget.charge_value_bytes(join::bound_bytes(value))?;
                Ok(value.clone())
            })
            .collect::<Result<Vec<_>, _>>()?;
        basis.push(projected);
    }
    if basis
        .iter()
        .flatten()
        .any(|value| matches!(value, BoundValue::Query(_)))
    {
        checked_stable_dedupe_by(&mut basis, |left, right| {
            compare_bound_slices(left, right, pull_budget)
        })?;
    } else {
        stable_dedupe_by(&mut basis, Ord::cmp);
    }
    pull_budget.check(0)?;
    let mut output = if elements.iter().any(|element| {
        matches!(
            element,
            FindElement::Aggregate { .. } | FindElement::CustomAggregate(_)
        )
    }) {
        aggregate_rows(
            elements,
            &basis_variables,
            &basis,
            sources,
            pull_budget,
            max_numeric_bytes,
            aggregates,
        )?
    } else {
        basis
            .into_iter()
            .map(|row| {
                pull_budget.check(1)?;
                elements
                    .iter()
                    .map(|element| {
                        project_element(element, &basis_variables, &row, sources, pull_budget)
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    // Pull is a post-projection of distinct entity bindings, not a new set
    // operation on maps. Different entities may intentionally pull alike.
    // Recovered query.clj apply-pf uses mapv after relational set formation.
    let has_pull = elements
        .iter()
        .any(|element| matches!(element, FindElement::Pull { .. }));
    dedupe_query_rows(&mut output, query.with.is_empty() && !has_pull, pull_budget)?;
    pull_budget.check(0)?;
    if output.len() > max {
        return Err(resource(
            "query/result-limit",
            "query result exceeded its row limit",
        ));
    }
    Ok(match &query.find {
        FindSpec::Relation(_) => QueryResult::Relation(output),
        FindSpec::Collection(_) => {
            QueryResult::Collection(output.into_iter().map(|mut row| row.remove(0)).collect())
        }
        FindSpec::Tuple(_) => QueryResult::Tuple(output.into_iter().next()),
        FindSpec::Scalar(_) => {
            QueryResult::Scalar(output.into_iter().next().map(|mut row| row.remove(0)))
        }
    })
}

pub(super) fn aggregate_rows(
    elements: &[FindElement],
    basis_variables: &[Variable],
    basis: &[Vec<BoundValue>],
    sources: &BTreeMap<&str, SourceRef<'_>>,
    pull_budget: &mut QueryPullBudget,
    max_numeric_bytes: usize,
    aggregates: &custom_aggregate::Context<'_>,
) -> Result<Vec<Vec<QueryValue>>, SemanticError> {
    let group_variables: Vec<_> = elements
        .iter()
        .filter_map(|element| match element {
            FindElement::Variable(variable) | FindElement::Pull { variable, .. } => Some(variable),
            _ => None,
        })
        .collect();
    let positions = group_variables
        .iter()
        .map(|variable| {
            basis_variables
                .iter()
                .position(|candidate| candidate == *variable)
                .unwrap()
        })
        .collect::<Vec<_>>();
    let general = basis.iter().any(|row| {
        positions
            .iter()
            .any(|index| matches!(row[*index], BoundValue::Query(_)))
    });
    let mut groups: Vec<Vec<&Vec<BoundValue>>> = if general {
        // Sort borrowed row ordinals, not cloned arbitrary map/set keys.
        // Each group retains its first basis row; restore first-group order
        // after the sort so aggregate representation retention stays stable.
        pull_budget.check(basis.len())?;
        pull_budget.charge_value_bytes(basis.len().saturating_mul(
            8 * std::mem::size_of::<usize>() + 3 * std::mem::size_of::<Vec<&Vec<BoundValue>>>(),
        ))?;
        let mut order = (0..basis.len()).collect::<Vec<_>>();
        let mut compare = |left: &usize, right: &usize| {
            for position in &positions {
                let ordering = compare_bound_slices(
                    std::slice::from_ref(&basis[*left][*position]),
                    std::slice::from_ref(&basis[*right][*position]),
                    pull_budget,
                )?;
                if !ordering.is_eq() {
                    return Ok(ordering);
                }
            }
            Ok(std::cmp::Ordering::Equal)
        };
        checked_aggregate_sort(&mut order, &mut compare)?;
        let mut grouped: Vec<(usize, Vec<&Vec<BoundValue>>)> = Vec::new();
        for index in order {
            if let Some((first, rows)) = grouped.last_mut()
                && compare(first, &index)?.is_eq()
            {
                rows.push(&basis[index]);
                continue;
            }
            grouped.push((index, vec![&basis[index]]));
        }
        grouped.sort_by_key(|(index, _)| *index);
        grouped.into_iter().map(|(_, rows)| rows).collect()
    } else {
        let mut groups: Vec<Vec<&Vec<BoundValue>>> = Vec::new();
        let mut group_indices = BTreeMap::new();
        for row in basis {
            pull_budget.check(1)?;
            let key = positions
                .iter()
                .map(|index| row[*index].clone())
                .collect::<Vec<_>>();
            let index = *group_indices.entry(key).or_insert_with(|| {
                groups.push(Vec::new());
                groups.len() - 1
            });
            groups[index].push(row);
        }
        groups
    };
    if groups.is_empty() && group_variables.is_empty() {
        groups.push(Vec::new());
    }
    groups
        .into_iter()
        .map(|rows| {
            pull_budget.check(rows.len())?;
            elements
                .iter()
                .map(|element| match element {
                    FindElement::Variable(variable) => {
                        let value = rows
                            .first()
                            .and_then(|row| {
                                basis_variables
                                    .iter()
                                    .position(|candidate| candidate == variable)
                                    .map(|index| &row[index])
                            })
                            .ok_or_else(|| {
                                fault("query/missing-group-key", "aggregate group key is missing")
                            })?;
                        aggregate_output_value(value, pull_budget)
                    }
                    FindElement::Pull {
                        source,
                        variable,
                        pattern,
                    } => {
                        let row = rows.first().ok_or_else(|| {
                            fault("query/missing-group-key", "pull group key is missing")
                        })?;
                        let index = basis_variables
                            .iter()
                            .position(|candidate| candidate == variable)
                            .ok_or_else(|| {
                                fault("query/missing-group-key", "pull variable is missing")
                            })?;
                        let entity = row[index].stored().and_then(entity_id).ok_or_else(|| {
                            SemanticError::incorrect(
                                "query/pull-entity",
                                "pull expression variable must bind an entity id",
                            )
                        })?;
                        find_source(sources, source)?.pull_for_query(pattern, entity, pull_budget)
                    }
                    FindElement::Aggregate { function, variable } => {
                        let index = basis_variables
                            .iter()
                            .position(|candidate| candidate == variable)
                            .ok_or_else(|| {
                                SemanticError::incorrect(
                                    "query/unbound-aggregate-variable",
                                    format!("aggregate variable {} is unbound", variable.name()),
                                )
                            })?;
                        aggregate(
                            *function,
                            rows.iter().map(|row| &row[index]).collect(),
                            pull_budget,
                            max_numeric_bytes,
                        )
                    }
                    FindElement::CustomAggregate(call) => custom_aggregate::invoke(
                        call,
                        &rows,
                        basis_variables,
                        sources,
                        pull_budget,
                        aggregates,
                    ),
                })
                .collect()
        })
        .collect()
}

pub(super) fn aggregate(
    function: Aggregate,
    mut values: Vec<&BoundValue>,
    budget: &mut QueryPullBudget,
    max_numeric_bytes: usize,
) -> Result<QueryValue, SemanticError> {
    match function {
        Aggregate::Rand(count) => {
            if values.is_empty() {
                return Ok(QueryValue::Collection(Vec::new()));
            }
            // With-replacement output can exceed its input size. Admit before
            // allocation, and remain cooperatively interruptible while building.
            budget.check(count)?;
            let mut output = Vec::new();
            for _ in 0..count {
                budget.check(0)?;
                output.push(aggregate_output_value(
                    values[rand::random_range(0..values.len())],
                    budget,
                )?);
            }
            Ok(QueryValue::Collection(output))
        }
        Aggregate::Sample(count) => {
            sort_aggregate_values(&mut values, true, budget)?;
            let count = count.min(values.len());
            for index in 0..count {
                budget.check(0)?;
                let selected = rand::random_range(index..values.len());
                values.swap(index, selected);
            }
            Ok(QueryValue::Collection(
                values
                    .into_iter()
                    .take(count)
                    .map(|value| aggregate_output_value(value, budget))
                    .collect::<Result<_, _>>()?,
            ))
        }
        Aggregate::Count => Ok(QueryValue::Scalar(Value::Long(values.len() as i64))),
        Aggregate::CountDistinct => {
            sort_aggregate_values(&mut values, true, budget)?;
            Ok(QueryValue::Scalar(Value::Long(values.len() as i64)))
        }
        Aggregate::Min | Aggregate::Max => {
            let value = if values
                .iter()
                .any(|value| matches!(value, BoundValue::Query(_)))
            {
                let mut best = None;
                for value in values {
                    budget.check(1)?;
                    if let Some(previous) = best {
                        let order = compare_bound_slices(
                            std::slice::from_ref(value),
                            std::slice::from_ref(previous),
                            budget,
                        )?;
                        if (function == Aggregate::Min && order.is_lt())
                            || (function == Aggregate::Max && !order.is_lt())
                        {
                            best = Some(value);
                        }
                    } else {
                        best = Some(value);
                    }
                }
                best
            } else if function == Aggregate::Min {
                values.into_iter().min_by(|a, b| a.index_cmp(b))
            } else {
                values.into_iter().max_by(|a, b| a.index_cmp(b))
            };
            value.map_or(Ok(QueryValue::Nil), |value| {
                aggregate_output_value(value, budget)
            })
        }
        Aggregate::Sum
        | Aggregate::Average
        | Aggregate::Median
        | Aggregate::Variance
        | Aggregate::StandardDeviation => {
            let max_bytes = max_numeric_bytes.min(budget.remaining_value_bytes());
            let values = values
                .into_iter()
                .map(|value| require_stored(value, "query/numeric-type"))
                .collect::<Result<Vec<_>, _>>()?;
            numeric::aggregate(
                function,
                values,
                &mut numeric::Budget {
                    max_bytes,
                    charge: |work, bytes| {
                        budget.check(work)?;
                        budget.charge_value_bytes(bytes)
                    },
                },
            )
            .map(QueryValue::Scalar)
        }
        Aggregate::Distinct => {
            sort_aggregate_values(&mut values, true, budget)?;
            // ATOMIC-NOTE: aggregation/distinct is (set coll), including when
            // nested q returns it as a scalar. Keep the set's observable shape.
            Ok(QueryValue::Set(
                values
                    .into_iter()
                    .map(|value| aggregate_output_value(value, budget))
                    .collect::<Result<_, _>>()?,
            ))
        }
        Aggregate::MinN(limit) | Aggregate::MaxN(limit) => {
            sort_aggregate_values(&mut values, false, budget)?;
            if matches!(function, Aggregate::MaxN(_)) {
                values.reverse();
            }
            Ok(QueryValue::Collection(
                values
                    .into_iter()
                    .take(limit)
                    .map(|value| aggregate_output_value(value, budget))
                    .collect::<Result<_, _>>()?,
            ))
        }
    }
}

pub(super) fn aggregate_output_value(
    value: &BoundValue,
    budget: &mut QueryPullBudget,
) -> Result<QueryValue, SemanticError> {
    if let BoundValue::Query(value) = value {
        let size = value.measure_with(&mut |work| budget.check(work))?;
        budget.charge_value_bytes(size.retained_bytes)?;
    }
    Ok(value.query_value())
}

pub(super) fn sort_aggregate_values(
    values: &mut Vec<&BoundValue>,
    dedupe: bool,
    budget: &mut QueryPullBudget,
) -> Result<(), SemanticError> {
    if !values
        .iter()
        .any(|value| matches!(value, BoundValue::Query(_)))
    {
        values.sort_by(|left, right| left.index_cmp(right));
        if dedupe {
            values.dedup_by(|left, right| left.index_cmp(right).is_eq());
        }
        return Ok(());
    }
    budget.check(values.len())?;
    budget.charge_value_bytes(
        values
            .len()
            .saturating_mul(2 * std::mem::size_of::<&BoundValue>()),
    )?;
    let mut compare = |left: &&BoundValue, right: &&BoundValue| {
        compare_bound_slices(
            std::slice::from_ref(*left),
            std::slice::from_ref(*right),
            budget,
        )
    };
    checked_aggregate_sort(values, &mut compare)?;
    if dedupe {
        let mut kept = 0;
        for read in 0..values.len() {
            if kept == 0 || !compare(&values[kept - 1], &values[read])?.is_eq() {
                values[kept] = values[read];
                kept += 1;
            }
        }
        values.truncate(kept);
    }
    Ok(())
}

/// Borrowed aggregate inputs/ordinals are Copy. One admitted scratch vector
/// permits immediate failure on comparison errors and stable equal-key order.
pub(super) fn checked_aggregate_sort<T: Copy>(
    values: &mut [T],
    compare: &mut impl FnMut(&T, &T) -> Result<std::cmp::Ordering, SemanticError>,
) -> Result<(), SemanticError> {
    let len = values.len();
    let mut scratch = Vec::with_capacity(len);
    let mut width = 1usize;
    while width < len {
        scratch.clear();
        for start in (0..len).step_by(width.saturating_mul(2)) {
            let middle = start.saturating_add(width).min(len);
            let end = start.saturating_add(width.saturating_mul(2)).min(len);
            let (mut left, mut right) = (start, middle);
            while left < middle && right < end {
                if compare(&values[left], &values[right])?.is_gt() {
                    scratch.push(values[right]);
                    right += 1;
                } else {
                    scratch.push(values[left]);
                    left += 1;
                }
            }
            scratch.extend_from_slice(&values[left..middle]);
            scratch.extend_from_slice(&values[right..end]);
        }
        values.copy_from_slice(&scratch);
        width = width.saturating_mul(2);
    }
    Ok(())
}

pub(super) fn project_element(
    element: &FindElement,
    basis_variables: &[Variable],
    row: &[BoundValue],
    sources: &BTreeMap<&str, SourceRef<'_>>,
    pull_budget: &mut QueryPullBudget,
) -> Result<QueryValue, SemanticError> {
    let variable = match element {
        FindElement::Variable(variable) | FindElement::Aggregate { variable, .. } => variable,
        FindElement::Pull { variable, .. } => variable,
        FindElement::CustomAggregate(_) => {
            return Err(fault(
                "query/aggregate-projection",
                "custom aggregate requires grouped projection",
            ));
        }
    };
    let index = basis_variables
        .iter()
        .position(|candidate| candidate == variable)
        .ok_or_else(|| {
            fault(
                "query/missing-projection",
                "find variable is missing from projection",
            )
        })?;
    match element {
        FindElement::Pull {
            source, pattern, ..
        } => {
            let entity = row[index].stored().and_then(entity_id).ok_or_else(|| {
                SemanticError::incorrect(
                    "query/pull-entity",
                    "pull expression variable must bind an entity id",
                )
            })?;
            find_source(sources, source)?.pull_for_query(pattern, entity, pull_budget)
        }
        _ => {
            pull_budget.charge_value_bytes(join::bound_bytes(&row[index]))?;
            Ok(row[index].query_value())
        }
    }
}
pub(super) fn dedupe_query_rows(
    rows: &mut Vec<Vec<QueryValue>>,
    dedupe: bool,
    budget: &mut QueryPullBudget,
) -> Result<(), SemanticError> {
    if dedupe {
        if rows
            .iter()
            .flatten()
            .any(|v| !matches!(v, QueryValue::Nil | QueryValue::Scalar(_)))
        {
            budget
                .charge_value_bytes(rows.len().saturating_mul(3 * std::mem::size_of::<usize>()))?;
            checked_stable_dedupe_by(rows, |left, right| {
                for (left, right) in left.iter().zip(right) {
                    prepare_output_key(left, budget)?;
                    prepare_output_key(right, budget)?;
                    let order = left.compare_with(right, &mut |work| budget.check(work))?;
                    if !order.is_eq() {
                        return Ok(order);
                    }
                }
                Ok(left.len().cmp(&right.len()))
            })?;
        } else {
            stable_dedupe_by(rows, |left, right| {
                left.iter()
                    .zip(right)
                    .map(|(l, r)| l.canonical_cmp(r))
                    .find(|order| !order.is_eq())
                    .unwrap_or_else(|| left.len().cmp(&right.len()))
            });
        }
    }
    Ok(())
}

pub(super) fn prepare_output_key(
    value: &QueryValue,
    budget: &mut QueryPullBudget,
) -> Result<(), SemanticError> {
    let size = value.measure_with(&mut |work| budget.check(work))?;
    budget.charge_value_bytes(size.canonical_bytes)
}

pub(super) fn compare_bound_slices(
    left: &[BoundValue],
    right: &[BoundValue],
    budget: &mut QueryPullBudget,
) -> Result<std::cmp::Ordering, SemanticError> {
    for (left, right) in left.iter().zip(right) {
        if let BoundValue::Query(value) = left {
            prepare_output_key(value, budget)?;
        }
        if let BoundValue::Query(value) = right {
            prepare_output_key(value, budget)?;
        }
        let order = left
            .borrowed()
            .logical_cmp_with(right.borrowed(), &mut |work| budget.check(work))?;
        if !order.is_eq() {
            return Ok(order);
        }
    }
    Ok(left.len().cmp(&right.len()))
}

pub(super) fn result_len(result: &QueryResult) -> usize {
    match result {
        QueryResult::Relation(rows) => rows.len(),
        QueryResult::Collection(values) => values.len(),
        QueryResult::Tuple(value) => usize::from(value.is_some()),
        QueryResult::Scalar(value) => usize::from(value.is_some()),
    }
}
