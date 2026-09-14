//! Persisted query authoring over the shared native query engine.
//! No evaluator or source authority lives in this module.
use super::{ProgramBudget, ProgramRead, RuntimeValue, incorrect};
use crate::{
    DatabaseValue, Query, QueryDataSource, QueryEngine, QueryInput, QueryResult, QueryValue,
    SemanticError, TimePoint, Value,
};

/// A literal documented time point, or an argument containing Long (T), Ref
/// (transaction entity), or Instant (Unix milliseconds).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QueryTemplateTime {
    Literal(TimePoint),
    Argument(u8),
}

/// One named view of the exact database supplied to the program invocation.
/// Bounds use ordinary native as-of/since semantics; history retains both
/// assertions and retractions. Existing filters remain attached to the value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryTemplateSource {
    pub name: String,
    pub history: bool,
    /// Full captured committed log, not a database history projection.
    pub log: bool,
    pub as_of: Option<QueryTemplateTime>,
    pub since: Option<QueryTemplateTime>,
    /// A finite general relation supplied by this program argument. This
    /// source is data, not a view of the invocation's database.
    pub relation_argument: Option<u8>,
}

impl QueryTemplateSource {
    pub fn current(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            history: false,
            log: false,
            as_of: None,
            since: None,
            relation_argument: None,
        }
    }
    pub fn history(name: impl Into<String>) -> Self {
        Self {
            history: true,
            ..Self::current(name)
        }
    }
    pub fn log(name: impl Into<String>) -> Self {
        Self {
            log: true,
            ..Self::current(name)
        }
    }
    pub fn relation(name: impl Into<String>, argument: u8) -> Self {
        Self {
            relation_argument: Some(argument),
            ..Self::current(name)
        }
    }
    pub fn as_of(mut self, point: QueryTemplateTime) -> Self {
        self.as_of = Some(point);
        self
    }
    pub fn since(mut self, point: QueryTemplateTime) -> Self {
        self.since = Some(point);
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NativeQueryTemplate {
    pub(crate) query: Query,
    pub(crate) input_arguments: Vec<u8>,
    pub(crate) sources: Vec<QueryTemplateSource>,
}

impl NativeQueryTemplate {
    pub(crate) fn new(
        query: Query,
        input_arguments: Vec<u8>,
        mut sources: Vec<QueryTemplateSource>,
    ) -> Result<Self, SemanticError> {
        if sources.is_empty() {
            sources.push(QueryTemplateSource::current("$"));
        }
        sources.sort_by(|left, right| left.name.cmp(&right.name));
        let template = Self {
            query,
            input_arguments,
            sources,
        };
        template.validate()?;
        Ok(template)
    }

    pub(crate) fn validate(&self) -> Result<(), SemanticError> {
        if self.query.inputs.len() != self.input_arguments.len() {
            return Err(incorrect(
                "program/query-input-arity",
                "each native query input needs a program argument binding",
            ));
        }
        if self.sources.is_empty()
            || self.sources.len() > super::MAX_QUERY_PATTERNS
            || self
                .sources
                .iter()
                .any(|source| source.name.is_empty() || source.name.len() > 256)
            || self
                .sources
                .windows(2)
                .any(|sources| sources[0].name >= sources[1].name)
        {
            return Err(incorrect(
                "program/query-sources",
                "native query sources must have unique bounded names in canonical order",
            ));
        }
        if self.sources.iter().any(|source| {
            (source.log && (source.history || source.as_of.is_some() || source.since.is_some()))
                || (source.relation_argument.is_some()
                    && (source.log
                        || source.history
                        || source.as_of.is_some()
                        || source.since.is_some()))
        }) {
            return Err(incorrect(
                "program/query-log-view",
                "log sources use their captured committed basis; supply ranges to tx-ids rather than database view modifiers",
            ));
        }
        // This bounded codec validation rejects local callbacks and unstable
        // built-ins before such values enter an immutable durable artifact.
        crate::encoding::validate_native_query(&self.query)?;
        crate::query::validate_program_query(&self.query, self.input_arguments.len())?;
        self.validate_arity(super::MAX_ARITY)
    }

    pub(crate) fn validate_arity(&self, arity: u8) -> Result<(), SemanticError> {
        let bounds = self
            .sources
            .iter()
            .flat_map(|source| [&source.as_of, &source.since])
            .filter_map(|bound| match bound {
                Some(QueryTemplateTime::Argument(index)) => Some(*index),
                _ => None,
            });
        if self
            .input_arguments
            .iter()
            .copied()
            .chain(bounds)
            .chain(
                self.sources
                    .iter()
                    .filter_map(|source| source.relation_argument),
            )
            .any(|index| index >= arity)
        {
            return Err(incorrect(
                "program/query-input-index",
                "native query binding is outside the program arity",
            ));
        }
        Ok(())
    }
}

fn time_point(
    bound: &QueryTemplateTime,
    arguments: &[RuntimeValue],
) -> Result<TimePoint, SemanticError> {
    Ok(match bound {
        QueryTemplateTime::Literal(point) => *point,
        QueryTemplateTime::Argument(index) => match &arguments[usize::from(*index)] {
            RuntimeValue::Scalar(Value::Long(t)) if *t >= 0 => TimePoint::T(*t as u64),
            RuntimeValue::Scalar(Value::Ref(tx))
            | RuntimeValue::Entity(crate::EntityRef::Id(tx)) => TimePoint::Tx(*tx),
            RuntimeValue::Scalar(Value::Instant(instant)) => TimePoint::Instant(*instant),
            _ => {
                return Err(incorrect(
                    "program/query-time-point",
                    "query time argument must be a nonnegative T, transaction entity, or instant",
                ));
            }
        },
    })
}

pub(crate) fn runtime_query_value(value: &RuntimeValue) -> Result<QueryValue, SemanticError> {
    map_runtime_query_value(value, &mut |entity| match entity {
        crate::EntityRef::Id(entity) => Ok(*entity),
        _ => Err(incorrect(
            "program/query-value",
            "query data requires resolved entity references",
        )),
    })
}

fn input_value(
    value: &RuntimeValue,
    database: &DatabaseValue,
    budget: &mut ProgramBudget<'_>,
) -> Result<QueryValue, SemanticError> {
    map_runtime_query_value(value, &mut |entity| {
        super::database_entity_id(
            ProgramRead::Exact(database),
            RuntimeValue::Entity(entity.clone()),
            budget,
        )
    })
}

fn map_runtime_query_value(
    value: &RuntimeValue,
    entity_id: &mut impl FnMut(&crate::EntityRef) -> Result<u64, SemanticError>,
) -> Result<QueryValue, SemanticError> {
    Ok(match value {
        RuntimeValue::Null => QueryValue::Nil,
        RuntimeValue::Scalar(value) => QueryValue::Scalar(value.clone()),
        RuntimeValue::Entity(entity) => QueryValue::Scalar(Value::Ref(entity_id(entity)?)),
        RuntimeValue::Query(value) => value.clone(),
        RuntimeValue::Vector(values) => QueryValue::Tuple(
            values
                .iter()
                .map(|value| map_runtime_query_value(value, entity_id))
                .collect::<Result<_, _>>()?,
        ),
        RuntimeValue::Map(entries) => QueryValue::Map(
            entries
                .iter()
                .map(|(key, value)| {
                    Ok((
                        QueryValue::Scalar(key.clone()),
                        map_runtime_query_value(value, entity_id)?,
                    ))
                })
                .collect::<Result<_, SemanticError>>()?,
        ),
    })
}

fn runtime_value(value: QueryValue, depth: usize) -> Result<RuntimeValue, SemanticError> {
    if depth > 16 {
        return Err(incorrect(
            "program/value-depth",
            "query result exceeds the runtime value nesting limit",
        ));
    }
    match &value {
        QueryValue::Tuple(_) => Ok(RuntimeValue::Vector(
            value
                .into_tuple()
                .expect("tuple")
                .into_iter()
                .map(|value| runtime_value(value, depth + 1))
                .collect::<Result<_, _>>()?,
        )),
        QueryValue::Map(entries)
            if entries
                .iter()
                .all(|(key, _)| matches!(key, QueryValue::Scalar(_))) =>
        {
            RuntimeValue::map(
                value
                    .into_map()
                    .expect("map")
                    .into_iter()
                    .map(|(key, value)| {
                        Ok((
                            key.into_scalar().expect("scalar key"),
                            runtime_value(value, depth + 1)?,
                        ))
                    })
                    .collect::<Result<_, SemanticError>>()?,
            )
        }
        _ => Ok(RuntimeValue::from_query_value(value)),
    }
}

pub(super) fn execute(
    database: ProgramRead<'_>,
    arguments: &[RuntimeValue],
    budget: &mut ProgramBudget<'_>,
    template: &NativeQueryTemplate,
) -> Result<RuntimeValue, SemanticError> {
    budget.check_cancel()?;
    let database: DatabaseValue = match database {
        ProgramRead::Exact(database) => database.clone(),
        ProgramRead::Eager(database) => database.database_value(),
        ProgramRead::AttributePredicate => {
            unreachable!("attribute predicates cannot read databases")
        }
    };
    let mut sources = Vec::with_capacity(template.sources.len());
    for source in &template.sources {
        budget.charge(1)?;
        if let Some(argument) = source.relation_argument {
            let value = input_value(&arguments[usize::from(argument)], &database, budget)?;
            let rows = match &value {
                QueryValue::Tuple(rows) | QueryValue::Collection(rows) | QueryValue::Set(rows) => {
                    rows
                }
                _ => {
                    return Err(incorrect(
                        "program/query-relation-shape",
                        "relation source needs a finite collection of row sequences",
                    ));
                }
            };
            let rows = rows
                .iter()
                .map(|row| match row {
                    QueryValue::Tuple(values) | QueryValue::Collection(values) => {
                        Ok(values.clone())
                    }
                    _ => Err(incorrect(
                        "program/query-relation-shape",
                        "relation source rows must be sequences",
                    )),
                })
                .collect::<Result<Vec<_>, _>>()?;
            sources.push(QueryDataSource::relation(source.name.clone(), rows));
            continue;
        }
        if source.log {
            sources.push(QueryDataSource::log(
                source.name.clone(),
                database.log_value()?,
            ));
            continue;
        }
        let mut view = database.clone();
        if let Some(bound) = &source.as_of {
            view = view.as_of_time_point(time_point(bound, arguments)?)?;
        }
        if let Some(bound) = &source.since {
            view = view.since_time_point(time_point(bound, arguments)?)?;
        }
        if source.history {
            view = view.history();
        }
        sources.push(QueryDataSource::database(source.name.clone(), view));
    }
    let inputs = template
        .input_arguments
        .iter()
        .map(|index| {
            input_value(&arguments[usize::from(*index)], &database, budget).map(QueryInput::General)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let outcome = QueryEngine::execute_program(&template.query, &sources, &inputs, budget)?;
    budget.check_cancel()?;
    match outcome.result {
        QueryResult::Relation(rows) => rows
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(|value| runtime_value(value, 1))
                    .collect::<Result<Vec<_>, _>>()
                    .map(RuntimeValue::Vector)
            })
            .collect::<Result<Vec<_>, _>>()
            .map(RuntimeValue::Vector),
        QueryResult::Collection(values) => values
            .into_iter()
            .map(|value| runtime_value(value, 1))
            .collect::<Result<Vec<_>, _>>()
            .map(RuntimeValue::Vector),
        QueryResult::Tuple(Some(values)) => values
            .into_iter()
            .map(|value| runtime_value(value, 1))
            .collect::<Result<Vec<_>, _>>()
            .map(RuntimeValue::Vector),
        QueryResult::Scalar(Some(value)) => runtime_value(value, 0),
        QueryResult::Tuple(None) | QueryResult::Scalar(None) => Ok(RuntimeValue::Null),
    }
}
