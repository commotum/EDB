//! Internal binding values, destructuring and logical unification preserve exact value equality.
use super::*;

#[derive(Clone, Debug)]
pub(super) enum BoundValue {
    Nil,
    Stored(Value),
    /// Query-only containers never enter persisted values or index keys.
    /// Construction normalizes nil/scalar leaves to the variants above.
    Query(QueryValue),
}

impl PartialEq for BoundValue {
    fn eq(&self, other: &Self) -> bool {
        self.index_cmp(other).is_eq()
    }
}
impl Eq for BoundValue {}

impl Ord for BoundValue {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.index_cmp(other)
    }
}

impl PartialOrd for BoundValue {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl BoundValue {
    pub(super) fn stored(&self) -> Option<&Value> {
        match self {
            Self::Nil | Self::Query(_) => None,
            Self::Stored(value) => Some(value),
        }
    }

    pub(super) fn index_cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.borrowed().logical_cmp(other.borrowed())
    }

    pub(super) fn borrowed(&self) -> QueryValueRef<'_> {
        match self {
            Self::Nil => QueryValueRef::Nil,
            Self::Stored(v) => QueryValueRef::Stored(v),
            Self::Query(v) => QueryValueRef::Query(v),
        }
    }

    pub(super) fn from_borrowed(value: QueryValueRef<'_>) -> Self {
        match value {
            QueryValueRef::Nil => Self::Nil,
            QueryValueRef::Stored(v) => Self::Stored(v.clone()),
            QueryValueRef::Query(v) => Self::from_query_value(v.clone()),
        }
    }

    pub(super) fn query_value(&self) -> QueryValue {
        match self {
            Self::Nil => QueryValue::Nil,
            Self::Stored(value) => QueryValue::Scalar(value.clone()),
            Self::Query(value) => value.clone(),
        }
    }

    pub(super) fn from_query_value(value: QueryValue) -> Self {
        match &value {
            QueryValue::Nil => Self::Nil,
            QueryValue::Scalar(_) => Self::Stored(value.into_scalar().expect("scalar variant")),
            QueryValue::Tuple(_) => {
                query_tuple_value(&value).map_or_else(|| Self::Query(value), Self::Stored)
            }
            _ => Self::Query(value),
        }
    }

    /// One-level sequence destructuring; nested containers remain intact.
    pub(super) fn sequence_values(&self) -> Option<Vec<Self>> {
        match self {
            Self::Stored(Value::Tuple(values)) => Some(
                values
                    .iter()
                    .cloned()
                    .map(|value| value.map_or(Self::Nil, Self::Stored))
                    .collect(),
            ),
            Self::Query(QueryValue::Tuple(values) | QueryValue::Collection(values)) => {
                Some(values.iter().cloned().map(Self::from_query_value).collect())
            }
            _ => None,
        }
    }

    pub(super) fn collection_values(&self) -> Option<Vec<Self>> {
        if let Self::Query(QueryValue::Set(values)) = self {
            Some(values.iter().cloned().map(Self::from_query_value).collect())
        } else {
            self.sequence_values()
        }
    }
}

/// A sequential tuple of stored values is the same legal tuple join key
/// whether constructed by `tuple` or returned by a tuple-shaped subquery.
/// Traverse iteratively; maps/collections remain query-only containers.
pub(super) fn query_tuple_value(value: &QueryValue) -> Option<Value> {
    enum Task<'a> {
        Value(&'a QueryValue, usize),
        Tuple(usize),
    }
    let mut pending = vec![Task::Value(value, 0)];
    let mut values = Vec::new();
    while let Some(task) = pending.pop() {
        match task {
            Task::Value(QueryValue::Nil, _) => values.push(None),
            Task::Value(QueryValue::Scalar(value), _) => values.push(Some(value.clone())),
            Task::Value(QueryValue::Tuple(children), depth) if depth < 32 => {
                pending.push(Task::Tuple(children.len()));
                pending.extend(
                    children
                        .iter()
                        .rev()
                        .map(|child| Task::Value(child, depth + 1)),
                );
            }
            Task::Value(..) => return None,
            Task::Tuple(count) => {
                let children = values.split_off(values.len() - count);
                values.push(Some(Value::Tuple(children)));
            }
        }
    }
    values.pop().flatten()
}

impl From<Value> for BoundValue {
    fn from(value: Value) -> Self {
        Self::Stored(value)
    }
}

pub(super) type Row = BTreeMap<Variable, BoundValue>;

pub(super) fn bind_inputs(
    specs: &[InputSpec],
    values: &[QueryInput],
    state: &mut State<'_>,
) -> Result<Vec<Row>, SemanticError> {
    state.check(0)?;
    let mut rows = vec![Row::new()];
    for (spec, value) in specs.iter().zip(values) {
        if let QueryInput::General(value) = value {
            state.validate_general(value)?;
            let (terms, relation) = general_input_relation(spec, value)?;
            let terms = terms.iter().enumerate().collect();
            let (next, _) =
                join::evaluate_raw(terms, rows, join::RawRelation::Input(&relation), state)?;
            rows = dedupe_rows(next, state)?;
            state.check_row_count(rows.len())?;
            continue;
        }
        let relation = match (spec, value) {
            (InputSpec::Scalar(variable), QueryInput::Scalar(value)) => {
                vec![vec![(Some(variable), value)]]
            }
            (InputSpec::Tuple(vars), QueryInput::Tuple(values)) if vars.len() == values.len() => {
                vec![
                    vars.iter()
                        .zip(values)
                        .map(|(var, value)| (var.as_ref(), value))
                        .collect(),
                ]
            }
            (InputSpec::Collection(variable), QueryInput::Collection(values)) => values
                .iter()
                .map(|value| vec![(Some(variable), value)])
                .collect(),
            (InputSpec::Relation(vars), QueryInput::Relation(value_rows)) => {
                if value_rows.iter().any(|row| row.len() != vars.len()) {
                    return Err(SemanticError::incorrect(
                        "query/input-shape",
                        "relation input row has the wrong width",
                    ));
                }
                value_rows
                    .iter()
                    .map(|row| {
                        vars.iter()
                            .zip(row)
                            .map(|(var, value)| (var.as_ref(), value))
                            .collect()
                    })
                    .collect()
            }
            _ => {
                return Err(SemanticError::incorrect(
                    "query/input-shape",
                    "query input does not match its binding form",
                ));
            }
        };
        let next = join::input_join(&rows, &relation, state)?;
        rows = dedupe_rows(next, state)?;
        state.check(0)?;
        if rows.len() > state.control.max_intermediate_rows {
            return Err(resource(
                "query/intermediate-limit",
                "query input relation exceeded its row limit",
            ));
        }
    }
    Ok(rows)
}

pub(super) fn query_sequence_refs(
    value: &QueryValue,
    unordered: bool,
) -> Option<Vec<QueryValueRef<'_>>> {
    match value {
        QueryValue::Tuple(values) | QueryValue::Collection(values) => {
            Some(values.iter().map(QueryValueRef::Query).collect())
        }
        QueryValue::Set(values) if unordered => {
            Some(values.iter().map(QueryValueRef::Query).collect())
        }
        QueryValue::Scalar(Value::Tuple(values)) => Some(
            values
                .iter()
                .map(|value| {
                    value
                        .as_ref()
                        .map_or(QueryValueRef::Nil, QueryValueRef::Stored)
                })
                .collect(),
        ),
        _ => None,
    }
}

pub(super) fn general_input_relation<'a>(
    spec: &InputSpec,
    value: &'a QueryValue,
) -> Result<(Vec<Term>, Vec<Vec<QueryValueRef<'a>>>), SemanticError> {
    let shape = || {
        SemanticError::incorrect(
            "query/input-shape",
            "query input does not match its binding form",
        )
    };
    let terms = |variables: &[Option<Variable>]| {
        variables
            .iter()
            .map(|variable| variable.clone().map_or(Term::Blank, Term::Variable))
            .collect()
    };
    match spec {
        InputSpec::Scalar(variable) => Ok((
            vec![Term::Variable(variable.clone())],
            vec![vec![QueryValueRef::Query(value)]],
        )),
        InputSpec::Tuple(variables) => {
            let row = query_sequence_refs(value, false).ok_or_else(shape)?;
            if row.len() != variables.len() {
                return Err(shape());
            }
            Ok((terms(variables), vec![row]))
        }
        InputSpec::Collection(variable) => Ok((
            vec![Term::Variable(variable.clone())],
            query_sequence_refs(value, true)
                .ok_or_else(shape)?
                .into_iter()
                .map(|value| vec![value])
                .collect(),
        )),
        InputSpec::Relation(variables) => {
            let outer = query_sequence_refs(value, true).ok_or_else(shape)?;
            let mut rows = Vec::with_capacity(outer.len());
            for value in outer {
                let row = match value {
                    QueryValueRef::Query(value) => query_sequence_refs(value, false),
                    QueryValueRef::Stored(Value::Tuple(values)) => Some(
                        values
                            .iter()
                            .map(|v| v.as_ref().map_or(QueryValueRef::Nil, QueryValueRef::Stored))
                            .collect(),
                    ),
                    _ => None,
                }
                .ok_or_else(shape)?;
                if row.len() != variables.len() {
                    return Err(shape());
                }
                rows.push(row);
            }
            Ok((terms(variables), rows))
        }
    }
}

pub(super) fn ground_output(
    args: &[BoundValue],
    binding: &Binding,
) -> Result<Vec<Vec<BoundValue>>, SemanticError> {
    let [constant] = args else {
        return Err(SemanticError::incorrect(
            "query/function-arity",
            "ground requires exactly one constant",
        ));
    };
    match binding {
        Binding::Scalar(_) | Binding::Collection(_) => Ok(vec![vec![constant.clone()]]),
        Binding::Tuple(_) => constant
            .sequence_values()
            .map(|values| vec![values])
            .ok_or_else(|| {
                SemanticError::incorrect(
                    "query/function-binding",
                    "tuple binding requires one sequential value",
                )
            }),
        Binding::Relation(_) => constant
            .collection_values()
            .ok_or_else(|| {
                SemanticError::incorrect(
                    "query/function-binding",
                    "relation binding requires a collection of tuple values",
                )
            })?
            .iter()
            .map(|row| {
                row.sequence_values().ok_or_else(|| {
                    SemanticError::incorrect(
                        "query/function-binding",
                        "relation binding requires sequential rows",
                    )
                })
            })
            .collect(),
    }
}

pub(super) fn require_stored<'a>(
    value: &'a BoundValue,
    code: &'static str,
) -> Result<&'a Value, SemanticError> {
    value.stored().ok_or_else(|| {
        SemanticError::incorrect(code, "this query function argument must be a stored scalar value, not nil or a query container")
    })
}

pub(super) fn bind_output(
    row: &Row,
    binding: &Binding,
    output: &[BoundValue],
    state: &mut State<'_>,
) -> Result<Vec<Row>, SemanticError> {
    if let Binding::Collection(variable) = binding {
        let [value] = output else {
            return Err(SemanticError::incorrect(
                "query/function-binding",
                "collection binding requires one sequential value",
            ));
        };
        let values = value.collection_values().ok_or_else(|| {
            SemanticError::incorrect(
                "query/function-binding",
                "collection binding requires one sequential value",
            )
        })?;
        let mut rows = Vec::new();
        for value in values {
            let mut candidate = row.clone();
            if unify_variable_checked(&mut candidate, variable, &value, state)? {
                rows.push(candidate);
            }
        }
        return dedupe_rows(rows, state);
    }
    let bindings: Vec<(Option<&Variable>, &BoundValue)> = match binding {
        Binding::Scalar(variable) if output.len() == 1 => vec![(Some(variable), &output[0])],
        Binding::Tuple(variables) | Binding::Relation(variables)
            if variables.len() == output.len() =>
        {
            variables
                .iter()
                .zip(output)
                .map(|(variable, value)| (variable.as_ref(), value))
                .collect()
        }
        Binding::Collection(_) => unreachable!(),
        _ => {
            return Err(SemanticError::incorrect(
                "query/function-binding",
                "function output does not match its binding form",
            ));
        }
    };
    let mut candidate = row.clone();
    for (variable, value) in bindings {
        if let Some(variable) = variable
            && !unify_variable_checked(&mut candidate, variable, value, state)?
        {
            return Ok(Vec::new());
        }
    }
    Ok(vec![candidate])
}

pub(super) fn resolve_args(
    args: &[Term],
    row: &Row,
    state: &mut State<'_>,
) -> Result<Vec<BoundValue>, SemanticError> {
    args.iter()
        .map(|term| {
            if let Some(value) = term_borrowed(term, row) {
                state.admit_ref(value)?;
            }
            match term {
                Term::Constant(value) => Ok(BoundValue::Stored(value.clone())),
                Term::QueryConstant(value) => Ok(BoundValue::from_query_value(value.clone())),
                Term::Nil => Ok(BoundValue::Nil),
                Term::Variable(variable) => row.get(variable).cloned().ok_or_else(|| {
                    SemanticError::incorrect(
                        "query/insufficient-binding",
                        format!("{} is not bound", variable.name()),
                    )
                }),
                Term::Blank => Err(SemanticError::incorrect(
                    "query/blank-expression-arg",
                    "blank cannot be an expression argument",
                )),
            }
        })
        .collect()
}
pub(super) fn term_bound_value(term: &Term, row: &Row) -> Option<BoundValue> {
    match term {
        Term::Constant(value) => Some(BoundValue::Stored(value.clone())),
        Term::QueryConstant(value) => Some(BoundValue::from_query_value(value.clone())),
        Term::Nil => Some(BoundValue::Nil),
        Term::Variable(variable) => row.get(variable).cloned(),
        Term::Blank => None,
    }
}
pub(super) fn term_borrowed<'a>(term: &'a Term, row: &'a Row) -> Option<QueryValueRef<'a>> {
    match term {
        Term::Constant(v) => Some(QueryValueRef::Stored(v)),
        Term::QueryConstant(v) => Some(QueryValueRef::Query(v)),
        Term::Nil => Some(QueryValueRef::Nil),
        Term::Variable(v) => row.get(v).map(BoundValue::borrowed),
        Term::Blank => None,
    }
}
pub(super) fn term_is_bound(term: &Term, row: &Row) -> bool {
    !matches!(term, Term::Blank)
        && !matches!(term, Term::Variable(variable) if !row.contains_key(variable))
}
pub(super) fn unify_term_checked(
    row: &mut Row,
    term: &Term,
    value: &BoundValue,
    state: &mut State<'_>,
) -> Result<bool, SemanticError> {
    if matches!(term, Term::Blank) {
        return Ok(true);
    }
    if let Some(expected) = term_borrowed(term, row) {
        return Ok(state.compare_keys(expected, value.borrowed())?.is_eq());
    }
    let Term::Variable(variable) = term else {
        return Ok(false);
    };
    unify_variable_checked(row, variable, value, state)
}

pub(super) fn unify_variable_checked(
    row: &mut Row,
    variable: &Variable,
    value: &BoundValue,
    state: &mut State<'_>,
) -> Result<bool, SemanticError> {
    if let Some(expected) = row.get(variable) {
        return Ok(state
            .compare_keys(expected.borrowed(), value.borrowed())?
            .is_eq());
    }
    if matches!(value, BoundValue::Query(_)) {
        state.admit_ref(value.borrowed())?;
    }
    row.insert(variable.clone(), value.clone());
    Ok(true)
}
pub(super) fn project_row(row: &Row, variables: &[Variable]) -> Row {
    variables
        .iter()
        .filter_map(|variable| {
            row.get(variable)
                .cloned()
                .map(|value| (variable.clone(), value))
        })
        .collect()
}
pub(super) fn dedupe_bound_tuples(
    rows: &mut Vec<Vec<BoundValue>>,
    state: &mut State<'_>,
) -> Result<(), SemanticError> {
    if rows
        .iter()
        .flatten()
        .any(|value| matches!(value, BoundValue::Query(_)))
    {
        state.charge_value_bytes(rows.len().saturating_mul(3 * std::mem::size_of::<usize>()))?;
        checked_stable_dedupe_by(rows, |left, right| {
            for (left, right) in left.iter().zip(right) {
                let order = state.compare_keys(left.borrowed(), right.borrowed())?;
                if !order.is_eq() {
                    return Ok(order);
                }
            }
            Ok(left.len().cmp(&right.len()))
        })
    } else {
        stable_dedupe_by(rows, Ord::cmp);
        Ok(())
    }
}

pub(super) fn dedupe_rows(
    mut rows: Vec<Row>,
    state: &mut State<'_>,
) -> Result<Vec<Row>, SemanticError> {
    if rows.iter().any(|row| {
        row.values()
            .any(|value| matches!(value, BoundValue::Query(_)))
    }) {
        state.charge_value_bytes(rows.len().saturating_mul(3 * std::mem::size_of::<usize>()))?;
        checked_stable_dedupe_by(&mut rows, |left, right| {
            for ((lv, left), (rv, right)) in left.iter().zip(right) {
                let order = lv.cmp(rv);
                if !order.is_eq() {
                    return Ok(order);
                }
                let order = state.compare_keys(left.borrowed(), right.borrowed())?;
                if !order.is_eq() {
                    return Ok(order);
                }
            }
            Ok(left.len().cmp(&right.len()))
        })?;
    } else {
        stable_dedupe_by(&mut rows, Ord::cmp);
    }
    Ok(rows)
}
/// Stable index merge-sort permits a comparison to stop immediately on a
/// budget/cancellation error, without handing an inconsistent comparator to
/// the standard library sort or cloning any payloads.
pub(super) fn checked_stable_dedupe_by<T>(
    values: &mut Vec<T>,
    mut compare: impl FnMut(&T, &T) -> Result<std::cmp::Ordering, SemanticError>,
) -> Result<(), SemanticError> {
    let len = values.len();
    if len < 2 {
        return Ok(());
    }
    let mut order: Vec<_> = (0..len).collect();
    let mut next = Vec::with_capacity(len);
    let mut width = 1_usize;
    while width < len {
        next.clear();
        for start in (0..len).step_by(width.saturating_mul(2)) {
            let middle = start.saturating_add(width).min(len);
            let end = middle.saturating_add(width).min(len);
            let (mut left, mut right) = (start, middle);
            while left < middle && right < end {
                if compare(&values[order[left]], &values[order[right]])?.is_gt() {
                    next.push(order[right]);
                    right += 1;
                } else {
                    next.push(order[left]);
                    left += 1;
                }
            }
            next.extend_from_slice(&order[left..middle]);
            next.extend_from_slice(&order[right..end]);
        }
        std::mem::swap(&mut order, &mut next);
        width = width.saturating_mul(2);
    }
    let mut keep = vec![false; len];
    let mut previous = None;
    for index in order {
        if let Some(prior) = previous
            && compare(&values[prior], &values[index])?.is_eq()
        {
            continue;
        }
        keep[index] = true;
        previous = Some(index);
    }
    let mut index = 0;
    values.retain(|_| {
        let retain = keep[index];
        index += 1;
        retain
    });
    Ok(())
}

/// Set semantics with stable first-representation retention. Sorting offsets
/// avoids payload clones and the former quadratic repeated `Vec::contains`.
pub(super) fn stable_dedupe_by<T>(
    values: &mut Vec<T>,
    compare: impl Fn(&T, &T) -> std::cmp::Ordering,
) {
    if values.len() < 2 {
        return;
    }
    let mut order: Vec<_> = (0..values.len()).collect();
    order.sort_unstable_by(|&left, &right| {
        compare(&values[left], &values[right]).then_with(|| left.cmp(&right))
    });
    let mut keep = vec![false; values.len()];
    let mut previous = None;
    for index in order {
        if previous.is_none_or(|previous| !compare(&values[previous], &values[index]).is_eq()) {
            keep[index] = true;
            previous = Some(index);
        }
    }
    let mut index = 0;
    values.retain(|_| {
        let retained = keep[index];
        index += 1;
        retained
    });
}
