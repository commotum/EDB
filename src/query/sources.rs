//! Explicit database, raw tuple, and immutable log arguments to one evaluator.
use super::*;
use crate::{Datom, IndexTransaction, LogValue, TimePoint};

#[derive(Clone, Debug)]
pub enum QuerySourceValue {
    Database(DatabaseValue),
    /// E/A/V(/T/assertion) rows. Values are literal: raw tuples have no schema
    /// and do not perform keyword/entity/lookup-ref resolution.
    Tuples(Arc<Vec<Vec<Value>>>),
    /// Arbitrary-width relations whose cells may be general query values.
    Relation(Arc<Vec<Vec<QueryValue>>>),
    Log(LogValue),
}

#[derive(Clone, Debug)]
pub struct QueryDataSource {
    pub name: String,
    pub value: QuerySourceValue,
}

impl QueryDataSource {
    pub fn relation(name: impl Into<String>, rows: Vec<Vec<QueryValue>>) -> Self {
        Self {
            name: name.into(),
            value: QuerySourceValue::Relation(Arc::new(rows)),
        }
    }
    pub fn database(name: impl Into<String>, database: DatabaseValue) -> Self {
        Self {
            name: name.into(),
            value: QuerySourceValue::Database(database),
        }
    }
    pub fn tuples(name: impl Into<String>, rows: Vec<Vec<Value>>) -> Self {
        Self {
            name: name.into(),
            value: QuerySourceValue::Tuples(Arc::new(rows)),
        }
    }
    pub fn datoms(name: impl Into<String>, datoms: impl IntoIterator<Item = Datom>) -> Self {
        Self::tuples(name, datoms.into_iter().map(datom_tuple).collect())
    }
    pub fn log(name: impl Into<String>, log: LogValue) -> Self {
        Self {
            name: name.into(),
            value: QuerySourceValue::Log(log),
        }
    }
    pub(super) fn borrowed(&self) -> SourceRef<'_> {
        match &self.value {
            QuerySourceValue::Database(database) => SourceRef::Database(database),
            QuerySourceValue::Tuples(tuples) => SourceRef::Tuples(tuples),
            QuerySourceValue::Relation(rows) => SourceRef::Relation(rows),
            QuerySourceValue::Log(log) => SourceRef::Log(log),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum SourceRef<'a> {
    Database(&'a DatabaseValue),
    Tuples(&'a [Vec<Value>]),
    Relation(&'a [Vec<QueryValue>]),
    Log(&'a LogValue),
}

pub(super) fn datom_tuple(datom: Datom) -> Vec<Value> {
    vec![
        Value::Ref(datom.entity),
        Value::Ref(u64::from(datom.attribute)),
        datom.value,
        Value::Ref(datom.tx),
        Value::Bool(datom.added),
    ]
}

pub(super) fn log_function(
    function: Function,
    source: &str,
    args: &[BoundValue],
    binding: &Binding,
    state: &mut State<'_>,
) -> Result<Vec<Vec<BoundValue>>, SemanticError> {
    let log = match state.sources.get(source) {
        Some(SourceRef::Log(log)) => *log,
        Some(_) => {
            return Err(SemanticError::incorrect(
                "query/source-kind",
                "transaction functions require an immutable log source",
            ));
        }
        None => {
            return Err(SemanticError::incorrect(
                "query/unknown-source",
                format!("unknown source {source}"),
            ));
        }
    };
    let point = |value: &BoundValue| -> Result<Option<TimePoint>, SemanticError> {
        match value {
            BoundValue::Nil => Ok(None),
            BoundValue::Stored(Value::Long(t)) if *t >= 0 => Ok(Some(TimePoint::T(*t as u64))),
            BoundValue::Stored(Value::Ref(tx)) => Ok(Some(TimePoint::Tx(*tx))),
            BoundValue::Stored(Value::Instant(instant)) => Ok(Some(TimePoint::Instant(*instant))),
            _ => Err(SemanticError::incorrect(
                "query/log-time",
                "log bound must be nil, nonnegative T, transaction ref, or instant",
            )),
        }
    };
    let mut rows = Vec::new();
    match function {
        Function::TxIds => {
            let [start, end] = args else {
                return Err(SemanticError::incorrect(
                    "query/function-arity",
                    "tx-ids requires start and end",
                ));
            };
            let mut values = Vec::new();
            state.check(1)?;
            for tx in log.tx_ids(point(start)?, point(end)?)? {
                state.check(1)?;
                let value = Value::Ref(tx?);
                state.charge_value_bytes(std::mem::size_of::<Value>())?;
                values.push(QueryValue::Scalar(value));
                state.check_row_count(values.len())?;
            }
            return ground_output(
                &[BoundValue::Query(QueryValue::Collection(values))],
                binding,
            );
        }
        Function::TxData => {
            let [tx] = args else {
                return Err(SemanticError::incorrect(
                    "query/function-arity",
                    "tx-data requires one transaction",
                ));
            };
            let tx = match point(tx)? {
                Some(TimePoint::T(t)) => IndexTransaction::T(t),
                Some(TimePoint::Tx(tx)) => IndexTransaction::Tx(tx),
                _ => {
                    return Err(SemanticError::incorrect(
                        "query/log-transaction",
                        "tx-data requires a T or transaction ref",
                    ));
                }
            };
            state.check(1)?;
            for datom in log.tx_data(tx)?.unwrap_or_default() {
                state.check(1)?;
                state.stats.datoms_examined += 1;
                let tuple = datom_tuple(datom);
                state.charge_value_bytes(tuple.iter().fold(0usize, |bytes, value| {
                    bytes
                        .saturating_add(std::mem::size_of::<Value>())
                        .saturating_add(
                            usize::try_from(value.retained_heap_bytes()).unwrap_or(usize::MAX),
                        )
                }))?;
                rows.push(tuple.into_iter().map(BoundValue::Stored).collect());
                state.check_row_count(rows.len())?;
            }
        }
        _ => unreachable!("log function dispatch"),
    }
    let value = QueryValue::Collection(
        rows.into_iter()
            .map(|row: Vec<BoundValue>| {
                QueryValue::Tuple(row.into_iter().map(|value| value.query_value()).collect())
            })
            .collect(),
    );
    ground_output(&[BoundValue::Query(value)], binding)
}

#[derive(Clone, Debug)]
pub struct QuerySource {
    pub name: String,
    pub database: DatabaseValue,
}

pub(super) fn validate_consumed_sources(
    query: &Query,
    sources: &BTreeMap<&str, SourceRef<'_>>,
    extensions: Option<&QueryExtensions>,
) -> Result<(), SemanticError> {
    let mut consumed = BTreeSet::new();
    let mut visited_rules = BTreeSet::new();
    collect_consumed_sources(
        &query.clauses,
        &query.rules,
        None,
        &mut consumed,
        &mut visited_rules,
        extensions,
    );
    for element in find_elements(&query.find) {
        if let FindElement::Pull { source, .. } = element {
            consumed.insert(source.clone());
        }
        if let FindElement::CustomAggregate(call) = element {
            custom_aggregate::validate(call, extensions)?;
            for arg in &call.args {
                if let AggregateArg::Source(source) = arg {
                    consumed.insert(source.clone());
                }
            }
        }
    }
    if let Some(source) = consumed
        .into_iter()
        .find(|source| !sources.contains_key(source.as_str()))
    {
        return Err(SemanticError::incorrect(
            "query/unknown-source",
            format!("unknown source {source}"),
        ));
    }
    for element in find_elements(&query.find) {
        if let FindElement::Pull { source, .. } = element {
            find_source(sources, source)?.require_point_in_time("pull")?;
        }
    }
    validate_nested_sources(
        &query.clauses,
        &query.rules,
        None,
        sources,
        &mut BTreeSet::new(),
        extensions,
    )?;
    Ok(())
}

// Validate semantic source consumers even when an outer relation is empty.
// Subquery rule scopes are independent; inherited `$` must follow the same
// mapping as runtime invocation, including queries nested in rule bodies.
pub(super) fn validate_nested_sources(
    clauses: &[Clause],
    rules: &[Rule],
    inherited: Option<&str>,
    sources: &BTreeMap<&str, SourceRef<'_>>,
    visited: &mut BTreeSet<(String, String)>,
    extensions: Option<&QueryExtensions>,
) -> Result<(), SemanticError> {
    let mut pending: Vec<_> = clauses
        .iter()
        .rev()
        .map(|clause| (clause, inherited.map(str::to_owned)))
        .collect();
    while let Some((clause, inherited_name)) = pending.pop() {
        let inherited = inherited_name.as_deref();
        match clause {
            Clause::RelationPattern(pattern) => {
                let name = effective_source(&pattern.source, inherited);
                match sources.get(name) {
                    Some(SourceRef::Tuples(_) | SourceRef::Relation(_)) => {}
                    Some(SourceRef::Database(database)) => {
                        let pattern = relation_data_pattern(pattern)?;
                        if let Term::Constant(attribute) = &pattern.attribute {
                            attribute_value(database, attribute)?;
                        }
                    }
                    Some(SourceRef::Log(_)) => {
                        return Err(SemanticError::incorrect(
                            "query/source-kind",
                            "log sources require tx-ids/tx-data",
                        ));
                    }
                    None => {
                        return Err(SemanticError::incorrect(
                            "query/unknown-source",
                            format!("unknown source {name}"),
                        ));
                    }
                }
            }
            Clause::Pattern(pattern) => {
                let name = effective_source(&pattern.source, inherited);
                match sources.get(name) {
                    Some(SourceRef::Database(database)) => {
                        if let Term::Constant(attribute) = &pattern.attribute {
                            attribute_value(database, attribute)?;
                        }
                    }
                    Some(SourceRef::Tuples(_) | SourceRef::Relation(_)) => {}
                    Some(SourceRef::Log(_)) => {
                        return Err(SemanticError::incorrect(
                            "query/source-kind",
                            "log sources require tx-ids/tx-data",
                        ));
                    }
                    None => {
                        return Err(SemanticError::incorrect(
                            "query/unknown-source",
                            format!("unknown source {name}"),
                        ));
                    }
                }
            }
            Clause::Function {
                function: Function::TxIds | Function::TxData,
                source,
                ..
            } => {
                let name = effective_source(source, inherited);
                if !matches!(sources.get(name), Some(SourceRef::Log(_))) {
                    return Err(SemanticError::incorrect(
                        "query/source-kind",
                        "transaction functions require an immutable log source",
                    ));
                }
            }
            Clause::Function {
                function: Function::Fulltext,
                source,
                args,
                ..
            } => {
                let database = find_source(sources, effective_source(source, inherited))?;
                if args.len() != 2 {
                    return Err(SemanticError::incorrect(
                        "query/function-arity",
                        "fulltext requires attribute and search string",
                    ));
                }
                // Per-run validation is retained even when an earlier clause
                // produces no rows or the structural prepared query is reused.
                if let Term::Constant(attribute) = &args[0] {
                    let attribute = attribute_value(database, attribute)?;
                    if !database.schema().attribute(attribute)?.fulltext {
                        return Err(SemanticError::incorrect(
                            "query/not-fulltext-attribute",
                            "fulltext requires an attribute installed with fulltext enabled",
                        ));
                    }
                }
                if let Term::Constant(search) = &args[1]
                    && !matches!(search, Value::String(_))
                {
                    return Err(SemanticError::incorrect(
                        "query/fulltext-search",
                        "fulltext search must be a string",
                    ));
                }
            }
            Clause::Predicate {
                predicate: Predicate::Missing,
                source,
                ..
            }
            | Clause::Function {
                function: Function::GetElse | Function::GetSome,
                source,
                ..
            } => {
                find_source(sources, effective_source(source, inherited))?;
            }
            Clause::Function {
                function: Function::Extension(name),
                source,
                ..
            } => {
                let extension = extensions
                    .and_then(|extensions| extensions.functions.get(name))
                    .ok_or_else(|| {
                        SemanticError::incorrect(
                            "query/unknown-extension",
                            format!("unknown query extension {name}"),
                        )
                    })?;
                if !matches!(
                    extension.implementation,
                    QueryExtensionImplementation::Pure(_)
                ) {
                    find_source(sources, effective_source(source, inherited))?;
                }
            }
            Clause::Function {
                function: Function::Query(query),
                source,
                ..
            } => {
                let selected = effective_source(source, inherited);
                let mut nested = sources.clone();
                if let Some(database) = sources.get(selected) {
                    nested.insert("$", *database);
                } else {
                    nested.remove("$");
                }
                validate_consumed_sources(query, &nested, extensions)?;
            }
            Clause::Not { clauses, .. } => {
                pending.extend(
                    clauses
                        .iter()
                        .rev()
                        .map(|clause| (clause, inherited_name.clone())),
                );
            }
            Clause::Or { branches, .. } => {
                pending.extend(
                    branches
                        .iter()
                        .rev()
                        .flat_map(|branch| branch.iter().rev())
                        .map(|clause| (clause, inherited_name.clone())),
                );
            }
            Clause::Rule { source, name, .. } => {
                let source = effective_source(source, inherited);
                if visited.insert((name.clone(), source.into())) {
                    for rule in rules.iter().rev().filter(|rule| rule.name == *name) {
                        pending.extend(
                            rule.clauses
                                .iter()
                                .rev()
                                .map(|clause| (clause, Some(source.to_owned()))),
                        );
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

pub(super) fn collect_consumed_sources(
    clauses: &[Clause],
    rules: &[Rule],
    inherited_source: Option<&str>,
    consumed: &mut BTreeSet<String>,
    visited_rules: &mut BTreeSet<(String, String)>,
    extensions: Option<&QueryExtensions>,
) {
    let mut pending: Vec<_> = clauses
        .iter()
        .rev()
        .map(|clause| (clause, inherited_source.map(str::to_owned)))
        .collect();
    while let Some((clause, inherited_name)) = pending.pop() {
        let inherited_source = inherited_name.as_deref();
        match clause {
            Clause::RelationPattern(pattern) => {
                consumed.insert(effective_source(&pattern.source, inherited_source).to_owned());
            }
            Clause::Pattern(pattern) => {
                consumed.insert(effective_source(&pattern.source, inherited_source).to_owned());
            }
            Clause::Predicate {
                predicate: Predicate::Missing,
                source,
                ..
            }
            | Clause::Function {
                function: Function::GetElse | Function::GetSome | Function::TxIds | Function::TxData,
                source,
                ..
            }
            | Clause::Function {
                function: Function::Fulltext,
                source,
                ..
            } => {
                consumed.insert(effective_source(source, inherited_source).to_owned());
            }
            Clause::Function {
                function: Function::Extension(name),
                source,
                ..
            } if !extensions
                .and_then(|registry| registry.functions.get(name))
                .is_some_and(|extension| {
                    matches!(
                        extension.implementation,
                        QueryExtensionImplementation::Pure(_)
                    )
                }) =>
            {
                consumed.insert(effective_source(source, inherited_source).to_owned());
            }
            Clause::Not { clauses, .. } => {
                pending.extend(
                    clauses
                        .iter()
                        .rev()
                        .map(|clause| (clause, inherited_name.clone())),
                );
            }
            Clause::Or { branches, .. } => {
                pending.extend(
                    branches
                        .iter()
                        .rev()
                        .flat_map(|branch| branch.iter().rev())
                        .map(|clause| (clause, inherited_name.clone())),
                );
            }
            Clause::Rule { source, name, .. } => {
                let source = effective_source(source, inherited_source).to_owned();
                if visited_rules.insert((name.clone(), source.clone())) {
                    for rule in rules.iter().rev().filter(|rule| rule.name == *name) {
                        pending.extend(
                            rule.clauses
                                .iter()
                                .rev()
                                .map(|clause| (clause, Some(source.clone()))),
                        );
                    }
                }
            }
            Clause::Predicate { .. } | Clause::Function { .. } => {}
        }
    }
}

pub(super) fn source_database<'a>(
    state: &'a State<'_>,
    source: &str,
) -> Result<&'a DatabaseValue, SemanticError> {
    find_source(&state.sources, source)
}

pub(super) fn find_source<'a>(
    sources: &'a BTreeMap<&str, SourceRef<'_>>,
    source: &str,
) -> Result<&'a DatabaseValue, SemanticError> {
    match sources.get(source) {
        Some(SourceRef::Database(database)) => Ok(database),
        Some(_) => Err(SemanticError::incorrect(
            "query/source-kind",
            format!("source {source} must be a database"),
        )),
        None => Err(SemanticError::incorrect(
            "query/unknown-source",
            format!("unknown source {source}"),
        )),
    }
}
pub(super) fn effective_source<'a>(source: &'a str, inherited_source: Option<&'a str>) -> &'a str {
    if source == "$" {
        inherited_source.unwrap_or(source)
    } else {
        source
    }
}
