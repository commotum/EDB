//! Per-execution, bounded relational join helpers.
use super::*;
use std::collections::HashMap;
use std::hash::{BuildHasher, Hash, Hasher, RandomState};

pub(super) fn bound_bytes(value: &BoundValue) -> usize {
    let inline = std::mem::size_of::<BoundValue>();
    match value {
        BoundValue::Nil => inline,
        BoundValue::Stored(value) => inline
            .saturating_add(usize::try_from(value.retained_heap_bytes()).unwrap_or(usize::MAX)),
        BoundValue::Query(value) => inline.saturating_add(query_value_bytes(value)),
    }
}

pub(super) fn query_value_bytes(value: &QueryValue) -> usize {
    value.retained_bytes()
}

pub(super) fn row_bytes(row: &Row) -> usize {
    row.iter()
        .fold(std::mem::size_of::<Row>(), |bytes, (variable, value)| {
            bytes
                .saturating_add(
                    std::mem::size_of::<Variable>()
                        + variable.name().len()
                        + 4 * std::mem::size_of::<usize>(),
                )
                .saturating_add(bound_bytes(value))
        })
}

fn hash_values<'a>(values: impl IntoIterator<Item = &'a Value>, random: &RandomState) -> u64 {
    let mut hash = random.build_hasher();
    for value in values {
        1u8.hash(&mut hash);
        value.logical_hash(&mut hash);
    }
    hash.finish()
}

// Conservative per-row table allowance: hash bucket, collision-vector capacity,
// row ordinal and allocator overhead. Source values remain borrowed, not copied.
const TABLE_ROW_BYTES: usize = 128;
fn chunk_rows(state: &State<'_>) -> usize {
    (state.control.max_join_bytes / TABLE_ROW_BYTES).max(1)
}

pub(super) fn input_join(
    rows: &[Row],
    relation: &[Vec<(Option<&Variable>, &Value)>],
    state: &mut State<'_>,
) -> Result<Vec<Row>, SemanticError> {
    let mut next = Vec::new();
    let shared: Vec<(usize, &Variable)> = relation
        .first()
        .into_iter()
        .flat_map(|row| row.iter().enumerate())
        .filter_map(|(column, (variable, _))| {
            variable
                .filter(|var| !rows.is_empty() && rows.iter().all(|row| row.contains_key(*var)))
                .map(|var| (column, var))
        })
        .collect();
    let use_hash = !shared.is_empty()
        && relation.len() > 10
        && !state.control.force_scan
        && state.control.max_join_bytes >= TABLE_ROW_BYTES;
    let random = RandomState::new();
    let chunk_size = if use_hash {
        chunk_rows(state)
    } else {
        relation.len().max(1)
    };
    for chunk in relation.chunks(chunk_size) {
        let mut table: HashMap<u64, Vec<usize>> = HashMap::new();
        if use_hash {
            for (index, bindings) in chunk.iter().enumerate() {
                state.check(1)?;
                let hash = hash_values(
                    shared.iter().map(|(column, _)| bindings[*column].1),
                    &random,
                );
                table.entry(hash).or_default().push(index);
                state.stats.hash_join_build_rows += 1;
            }
            state.stats.peak_join_bytes = state
                .stats
                .peak_join_bytes
                .max(chunk.len() * TABLE_ROW_BYTES);
        }
        for row in rows {
            state.check(1)?;
            let candidates: Box<dyn Iterator<Item = usize> + '_> = if use_hash {
                state.stats.hash_join_probes += 1;
                let Some(values) = shared
                    .iter()
                    .map(|(_, variable)| row.get(*variable).and_then(BoundValue::stored))
                    .collect::<Option<Vec<_>>>()
                else {
                    continue;
                };
                let hash = hash_values(values, &random);
                Box::new(table.get(&hash).into_iter().flatten().copied())
            } else {
                Box::new(0..chunk.len())
            };
            for index in candidates {
                state.check(1)?;
                state.stats.join_candidates += 1;
                let mut candidate = row.clone();
                if chunk[index]
                    .iter()
                    .try_fold(true, |matches, (variable, value)| {
                        if !matches {
                            return Ok(false);
                        }
                        variable.map_or(Ok(true), |variable| {
                            unify_variable_checked(
                                &mut candidate,
                                variable,
                                &BoundValue::Stored((*value).clone()),
                                state,
                            )
                        })
                    })?
                {
                    state.push_row(&mut next, candidate)?;
                }
            }
        }
    }
    Ok(next)
}

/// Borrowed cell access keeps corpus payloads out of the auxiliary hash table.
#[derive(Clone, Copy)]
pub(super) enum RawRelation<'a> {
    Stored(&'a [Vec<Value>]),
    General(&'a [Vec<QueryValue>]),
    Input(&'a [Vec<QueryValueRef<'a>>]),
}
impl<'a> RawRelation<'a> {
    fn len(self) -> usize {
        match self {
            Self::Stored(v) => v.len(),
            Self::General(v) => v.len(),
            Self::Input(v) => v.len(),
        }
    }
    fn width(self, row: usize) -> usize {
        match self {
            Self::Stored(v) => v[row].len(),
            Self::General(v) => v[row].len(),
            Self::Input(v) => v[row].len(),
        }
    }
    fn cell(self, row: usize, col: usize) -> QueryValueRef<'a> {
        match self {
            Self::Stored(v) => QueryValueRef::Stored(&v[row][col]),
            Self::General(v) => QueryValueRef::Query(&v[row][col]),
            Self::Input(v) => v[row][col],
        }
    }
}

pub(super) fn pattern_terms(pattern: &DataPattern) -> Vec<(usize, &Term)> {
    let mut terms = vec![
        (0, &pattern.entity),
        (1, &pattern.attribute),
        (2, &pattern.value),
    ];
    terms.extend(pattern.transaction.iter().map(|term| (3, term)));
    terms.extend(pattern.added.iter().map(|term| (4, term)));
    terms
}

pub(super) fn evaluate_tuples(
    pattern: &DataPattern,
    rows: Vec<Row>,
    tuples: &[Vec<Value>],
    state: &mut State<'_>,
) -> Result<(Vec<Row>, String), SemanticError> {
    evaluate_raw(
        pattern_terms(pattern),
        rows,
        RawRelation::Stored(tuples),
        state,
    )
}

fn hash_refs<'a>(
    values: impl IntoIterator<Item = QueryValueRef<'a>>,
    random: &RandomState,
    state: &mut State<'_>,
) -> Result<u64, SemanticError> {
    let mut hash = random.build_hasher();
    for value in values {
        1_u8.hash(&mut hash);
        state.prepare_key(value)?;
        value.logical_hash_with(&mut hash, &mut |work| state.check(work))?;
    }
    Ok(hash.finish())
}

fn unify_ref(
    row: &mut Row,
    term: &Term,
    value: QueryValueRef<'_>,
    state: &mut State<'_>,
) -> Result<bool, SemanticError> {
    if matches!(term, Term::Blank) {
        return Ok(true);
    }
    if let Some(expected) = term_borrowed(term, row) {
        state.prepare_key(expected)?;
        state.prepare_key(value)?;
        return Ok(expected
            .logical_cmp_with(value, &mut |work| state.check(work))?
            .is_eq());
    }
    let Term::Variable(variable) = term else {
        return Ok(false);
    };
    state.admit_ref(value)?;
    row.insert(variable.clone(), BoundValue::from_borrowed(value));
    Ok(true)
}

pub(super) fn evaluate_raw(
    mut terms: Vec<(usize, &Term)>,
    rows: Vec<Row>,
    relation: RawRelation<'_>,
    state: &mut State<'_>,
) -> Result<(Vec<Row>, String), SemanticError> {
    terms.retain(|(_, term)| !matches!(term, Term::Blank));
    let width = terms.last().map_or(0, |(column, _)| column + 1);
    let shared: Vec<_> = terms
        .iter()
        .copied()
        .filter(|(_, term)| !rows.is_empty() && rows.iter().all(|row| term_is_bound(term, row)))
        .collect();
    let use_hash = !shared.is_empty()
        && relation.len() > 10
        && !state.control.force_scan
        && state.control.max_join_bytes >= TABLE_ROW_BYTES;
    let random = RandomState::new();
    let mut next = Vec::new();
    let chunk_size = if use_hash {
        chunk_rows(state)
    } else {
        relation.len().max(1)
    };
    for start in (0..relation.len()).step_by(chunk_size) {
        let end = start.saturating_add(chunk_size).min(relation.len());
        let mut table: HashMap<u64, Vec<usize>> = HashMap::new();
        if use_hash {
            for index in start..end {
                state.check(1)?;
                state.stats.datoms_examined += 1;
                if relation.width(index) < width {
                    continue;
                }
                let hash = hash_refs(
                    shared
                        .iter()
                        .map(|(column, _)| relation.cell(index, *column)),
                    &random,
                    state,
                )?;
                table.entry(hash).or_default().push(index);
                state.stats.hash_join_build_rows += 1;
            }
            state.stats.peak_join_bytes = state
                .stats
                .peak_join_bytes
                .max((end - start) * TABLE_ROW_BYTES);
        }
        for row in &rows {
            state.check(1)?;
            let candidates: Box<dyn Iterator<Item = usize> + '_> = if use_hash {
                state.stats.hash_join_probes += 1;
                let hash = hash_refs(
                    shared
                        .iter()
                        .map(|(_, term)| term_borrowed(term, row).expect("shared bound term")),
                    &random,
                    state,
                )?;
                Box::new(table.get(&hash).into_iter().flatten().copied())
            } else {
                Box::new(start..end)
            };
            for index in candidates {
                state.check(1)?;
                state.stats.join_candidates += 1;
                if !use_hash {
                    state.stats.datoms_examined += 1;
                }
                if relation.width(index) < width {
                    continue;
                }
                let mut candidate = row.clone();
                let mut matches = true;
                for (column, term) in &terms {
                    if !unify_ref(&mut candidate, term, relation.cell(index, *column), state)? {
                        matches = false;
                        break;
                    }
                }
                if matches {
                    state.push_row(&mut next, candidate)?;
                }
            }
        }
    }
    Ok((
        next,
        if use_hash {
            "tuple hash join"
        } else {
            "tuple scan"
        }
        .into(),
    ))
}
