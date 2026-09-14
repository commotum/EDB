//! The current bounded encoding of the portable native query AST.
use super::{
    Cursor, decode_keyword, decode_symbol, decode_value, encode_keyword, encode_symbol,
    encode_value, fault, invalid_tag, put_bool, put_len, put_string, put_u64,
};
use crate::program::native_query::NativeQueryTemplate;
use crate::{
    Aggregate, AggregateArg, AttributeName, Binding, Clause, DataPattern, FindElement, FindSpec,
    Function, InputSpec, Predicate, PullAttribute, PullDirection, PullLimit, PullNested,
    PullPattern, PullTransform, Query, QueryTemplate, QueryTemplateSource, QueryTemplateTime,
    QueryValue, RelationPattern, Rule, SemanticError, Term, TimePoint, Value, Variable,
};

const MAX_NODES: usize = 4096;
const MAX_DEPTH: usize = 32;
const MAX_NAME_BYTES: usize = 4096;

#[derive(Default)]
struct Budget {
    nodes: usize,
    comparisons: usize,
    // Cache keys use this same AST traversal but preserve in-memory literal
    // representations and identify registry callbacks by name. They are never
    // decoded or admitted as persisted code.
    cache_key_limit: Option<usize>,
}
impl Budget {
    fn check_bytes(&self, output: &[u8]) -> Result<(), SemanticError> {
        if output.len() > self.cache_key_limit.unwrap_or(super::MAX_PROGRAM_BYTES) {
            return Err(fault(
                "program/payload-limit",
                "query encoding exceeds its byte limit",
            ));
        }
        Ok(())
    }
    fn comparison(&mut self, work: usize) -> Result<(), SemanticError> {
        self.comparisons = self.comparisons.saturating_add(work);
        if self.comparisons > MAX_NODES * MAX_DEPTH {
            return Err(fault(
                "program/native-query-shape-limit",
                "query value comparison exceeds its work bound",
            ));
        }
        Ok(())
    }
    fn node(&mut self, depth: usize) -> Result<(), SemanticError> {
        if depth > MAX_DEPTH || self.nodes >= MAX_NODES {
            return Err(fault(
                "program/native-query-shape-limit",
                "native query AST exceeds its bounded node/depth limits",
            ));
        }
        self.nodes += 1;
        Ok(())
    }
}
fn count(cursor: &mut Cursor<'_>) -> Result<usize, SemanticError> {
    let count = cursor.collection_len()?;
    if count > MAX_NODES {
        return Err(fault(
            "program/native-query-shape-limit",
            "native query collection is too large",
        ));
    }
    Ok(count)
}
fn length(output: &mut Vec<u8>, count: usize) -> Result<(), SemanticError> {
    if count > MAX_NODES {
        return Err(fault(
            "program/native-query-shape-limit",
            "native query collection is too large",
        ));
    }
    put_len(output, count)
}
fn name(output: &mut Vec<u8>, value: &str) -> Result<(), SemanticError> {
    if value.is_empty() || value.len() > MAX_NAME_BYTES {
        return Err(fault(
            "program/native-query-name",
            "native query names must be nonempty and bounded",
        ));
    }
    put_string(output, value)?;
    payload_size(output)
}
fn payload_size(output: &[u8]) -> Result<(), SemanticError> {
    if output.len() > super::MAX_PROGRAM_BYTES {
        return Err(fault(
            "program/payload-limit",
            "native query exceeds the durable program byte limit",
        ));
    }
    Ok(())
}
fn read_name(cursor: &mut Cursor<'_>) -> Result<String, SemanticError> {
    let bytes = cursor.bytes()?;
    if bytes.is_empty() || bytes.len() > MAX_NAME_BYTES {
        return Err(fault(
            "program/native-query-name",
            "native query names must be nonempty and bounded",
        ));
    }
    String::from_utf8(bytes.to_vec())
        .map_err(|_| fault("encoding/invalid-utf8", "invalid native query name"))
}
fn number(cursor: &mut Cursor<'_>) -> Result<usize, SemanticError> {
    usize::try_from(cursor.u64()?).map_err(|_| {
        fault(
            "encoding/query-integer-range",
            "native query size does not fit this platform",
        )
    })
}
fn unsupported() -> SemanticError {
    SemanticError::new(
        crate::ErrorCategory::Unsupported,
        "program/nonportable-query",
        "persisted queries require portable native built-ins; local callbacks, extension registries and random/sample aggregates are not durable code",
    )
}

pub(crate) fn validate_native_query(query: &Query) -> Result<(), SemanticError> {
    let mut output = Vec::new();
    encode_query(&mut output, query, 0, &mut Budget::default())?;
    payload_size(&output)
}

pub(crate) fn query_cache_key(query: &Query, max_bytes: usize) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut budget = Budget {
        cache_key_limit: Some(max_bytes),
        ..Budget::default()
    };
    encode_query(&mut bytes, query, 0, &mut budget).ok()?;
    budget.check_bytes(&bytes).ok()?;
    Some(bytes)
}

fn encode_scalar(
    output: &mut Vec<u8>,
    value: &Value,
    depth: usize,
    budget: &mut Budget,
) -> Result<(), SemanticError> {
    let Some(limit) = budget.cache_key_limit else {
        return encode_value(output, value);
    };
    budget.node(depth)?;
    match value {
        Value::Float(value) => {
            output.push(5);
            output.extend_from_slice(&value.to_bits().to_be_bytes());
        }
        Value::Double(value) => {
            output.push(4);
            output.extend_from_slice(&value.to_bits().to_be_bytes());
        }
        Value::Tuple(values) => {
            output.push(12);
            length(output, values.len())?;
            for value in values {
                put_bool(output, value.is_some());
                if let Some(value) = value {
                    encode_scalar(output, value, depth + 1, budget)?;
                }
            }
        }
        _ => {
            if value.retained_heap_bytes() > limit.saturating_sub(output.len()) as u64 {
                return Err(fault(
                    "program/payload-limit",
                    "query literal exceeds cache admission",
                ));
            }
            encode_value(output, value)?;
        }
    }
    budget.check_bytes(output)
}

pub(super) fn encode_template(
    output: &mut Vec<u8>,
    template: &NativeQueryTemplate,
) -> Result<(), SemanticError> {
    length(output, template.input_arguments.len())?;
    output.extend_from_slice(&template.input_arguments);
    length(output, template.sources.len())?;
    for source in &template.sources {
        name(output, &source.name)?;
        put_bool(output, source.history);
        put_bool(output, source.log);
        encode_time(output, &source.as_of);
        encode_time(output, &source.since);
        put_bool(output, source.relation_argument.is_some());
        if let Some(argument) = source.relation_argument {
            output.push(argument);
        }
    }
    encode_query(output, &template.query, 0, &mut Budget::default())
}
pub(super) fn decode_template(cursor: &mut Cursor<'_>) -> Result<QueryTemplate, SemanticError> {
    let length = count(cursor)?;
    let arguments = cursor.take(length)?.to_vec();
    let mut sources = Vec::new();
    for _ in 0..count(cursor)? {
        sources.push(QueryTemplateSource {
            name: read_name(cursor)?,
            history: cursor.boolean()?,
            log: cursor.boolean()?,
            as_of: decode_time(cursor)?,
            since: decode_time(cursor)?,
            relation_argument: if cursor.boolean()? {
                Some(cursor.u8()?)
            } else {
                None
            },
        });
    }
    QueryTemplate::native(
        decode_query(cursor, 0, &mut Budget::default())?,
        arguments,
        sources,
    )
}
fn encode_time(output: &mut Vec<u8>, value: &Option<QueryTemplateTime>) {
    match value {
        None => output.push(0),
        Some(QueryTemplateTime::Argument(index)) => {
            output.push(1);
            output.push(*index);
        }
        Some(QueryTemplateTime::Literal(point)) => match point {
            TimePoint::T(t) => {
                output.push(2);
                put_u64(output, *t);
            }
            TimePoint::Tx(tx) => {
                output.push(3);
                put_u64(output, *tx);
            }
            TimePoint::Instant(instant) => {
                output.push(4);
                output.extend_from_slice(&instant.to_be_bytes());
            }
        },
    }
}
fn decode_time(cursor: &mut Cursor<'_>) -> Result<Option<QueryTemplateTime>, SemanticError> {
    Ok(match cursor.u8()? {
        0 => None,
        1 => Some(QueryTemplateTime::Argument(cursor.u8()?)),
        2 => Some(QueryTemplateTime::Literal(TimePoint::T(cursor.u64()?))),
        3 => Some(QueryTemplateTime::Literal(TimePoint::Tx(cursor.u64()?))),
        4 => Some(QueryTemplateTime::Literal(TimePoint::Instant(
            cursor.i64()?,
        ))),
        tag => return Err(invalid_tag("query time", tag)),
    })
}

fn encode_variables(output: &mut Vec<u8>, variables: &[Variable]) -> Result<(), SemanticError> {
    length(output, variables.len())?;
    for variable in variables {
        name(output, variable.name())?;
    }
    Ok(())
}
fn decode_variables(cursor: &mut Cursor<'_>) -> Result<Vec<Variable>, SemanticError> {
    (0..count(cursor)?)
        .map(|_| Variable::new(read_name(cursor)?))
        .collect()
}
fn encode_optional_variables(
    output: &mut Vec<u8>,
    variables: &Option<Vec<Variable>>,
) -> Result<(), SemanticError> {
    put_bool(output, variables.is_some());
    if let Some(variables) = variables {
        encode_variables(output, variables)?;
    }
    Ok(())
}
fn decode_optional_variables(
    cursor: &mut Cursor<'_>,
) -> Result<Option<Vec<Variable>>, SemanticError> {
    if cursor.boolean()? {
        Ok(Some(decode_variables(cursor)?))
    } else {
        Ok(None)
    }
}
fn encode_slots(output: &mut Vec<u8>, slots: &[Option<Variable>]) -> Result<(), SemanticError> {
    length(output, slots.len())?;
    for slot in slots {
        put_bool(output, slot.is_some());
        if let Some(variable) = slot {
            name(output, variable.name())?;
        }
    }
    Ok(())
}
fn decode_slots(cursor: &mut Cursor<'_>) -> Result<Vec<Option<Variable>>, SemanticError> {
    (0..count(cursor)?)
        .map(|_| {
            if cursor.boolean()? {
                Ok(Some(Variable::new(read_name(cursor)?)?))
            } else {
                Ok(None)
            }
        })
        .collect()
}
fn encode_term(
    output: &mut Vec<u8>,
    term: &Term,
    depth: usize,
    budget: &mut Budget,
) -> Result<(), SemanticError> {
    budget.node(depth)?;
    match term {
        Term::Variable(variable) => {
            output.push(0);
            name(output, variable.name())?;
        }
        Term::Constant(value) => {
            output.push(1);
            encode_scalar(output, value, depth + 1, budget)?;
        }
        Term::QueryConstant(value) => {
            output.push(4);
            encode_query_value(output, value, depth + 1, budget)?;
        }
        Term::Nil => output.push(2),
        Term::Blank => output.push(3),
    }
    payload_size(output)
}
fn decode_term(
    cursor: &mut Cursor<'_>,
    depth: usize,
    budget: &mut Budget,
) -> Result<Term, SemanticError> {
    budget.node(depth)?;
    Ok(match cursor.u8()? {
        0 => Term::Variable(Variable::new(read_name(cursor)?)?),
        1 => Term::Constant(decode_value(cursor, 0)?),
        2 => Term::Nil,
        3 => Term::Blank,
        4 => Term::QueryConstant(decode_query_value(cursor, depth + 1, budget)?),
        tag => return Err(invalid_tag("native query term", tag)),
    })
}
fn encode_terms(
    output: &mut Vec<u8>,
    terms: &[Term],
    depth: usize,
    budget: &mut Budget,
) -> Result<(), SemanticError> {
    length(output, terms.len())?;
    for term in terms {
        encode_term(output, term, depth, budget)?;
    }
    Ok(())
}
fn decode_terms(
    cursor: &mut Cursor<'_>,
    depth: usize,
    budget: &mut Budget,
) -> Result<Vec<Term>, SemanticError> {
    (0..count(cursor)?)
        .map(|_| decode_term(cursor, depth, budget))
        .collect()
}
fn encode_binding(output: &mut Vec<u8>, binding: &Binding) -> Result<(), SemanticError> {
    match binding {
        Binding::Scalar(variable) => {
            output.push(0);
            name(output, variable.name())?;
        }
        Binding::Tuple(slots) => {
            output.push(1);
            encode_slots(output, slots)?;
        }
        Binding::Collection(variable) => {
            output.push(2);
            name(output, variable.name())?;
        }
        Binding::Relation(slots) => {
            output.push(3);
            encode_slots(output, slots)?;
        }
    }
    Ok(())
}
fn decode_binding(cursor: &mut Cursor<'_>) -> Result<Binding, SemanticError> {
    Ok(match cursor.u8()? {
        0 => Binding::Scalar(Variable::new(read_name(cursor)?)?),
        1 => Binding::Tuple(decode_slots(cursor)?),
        2 => Binding::Collection(Variable::new(read_name(cursor)?)?),
        3 => Binding::Relation(decode_slots(cursor)?),
        tag => return Err(invalid_tag("query binding", tag)),
    })
}

fn encode_clauses(
    output: &mut Vec<u8>,
    clauses: &[Clause],
    depth: usize,
    budget: &mut Budget,
) -> Result<(), SemanticError> {
    length(output, clauses.len())?;
    for clause in clauses {
        budget.node(depth)?;
        match clause {
            Clause::Pattern(pattern) => {
                output.push(0);
                name(output, &pattern.source)?;
                for term in [&pattern.entity, &pattern.attribute, &pattern.value] {
                    encode_term(output, term, depth + 1, budget)?;
                }
                for term in [&pattern.transaction, &pattern.added] {
                    put_bool(output, term.is_some());
                    if let Some(term) = term {
                        encode_term(output, term, depth + 1, budget)?;
                    }
                }
            }
            Clause::RelationPattern(pattern) => {
                output.push(6);
                name(output, &pattern.source)?;
                encode_terms(output, &pattern.terms, depth + 1, budget)?;
            }
            Clause::Predicate {
                predicate,
                source,
                args,
            } => {
                output.push(1);
                output.push(match predicate {
                    Predicate::Eq => 0,
                    Predicate::NotEq => 1,
                    Predicate::Less => 2,
                    Predicate::LessOrEqual => 3,
                    Predicate::Greater => 4,
                    Predicate::GreaterOrEqual => 5,
                    Predicate::Missing => 6,
                });
                name(output, source)?;
                encode_terms(output, args, depth + 1, budget)?;
            }
            Clause::Function {
                function,
                source,
                args,
                binding,
            } => {
                output.push(2);
                match function {
                    Function::Ground => output.push(0),
                    Function::Add => output.push(1),
                    Function::Subtract => output.push(2),
                    Function::Multiply => output.push(3),
                    Function::Divide => output.push(4),
                    Function::Tuple => output.push(5),
                    Function::Untuple => output.push(6),
                    Function::GetElse => output.push(7),
                    Function::GetSome => output.push(8),
                    Function::Query(query) => {
                        output.push(9);
                        encode_query(output, query, depth + 1, budget)?;
                    }
                    Function::TxIds => output.push(10),
                    Function::TxData => output.push(11),
                    Function::Fulltext => output.push(12),
                    Function::Count
                    | Function::Quot
                    | Function::Subs
                    | Function::Str
                    | Function::StartsWith
                    | Function::EndsWith
                    | Function::Includes => {
                        output.push(match function {
                            Function::Count => 13,
                            Function::Quot => 14,
                            Function::Subs => 15,
                            Function::Str => 16,
                            Function::StartsWith => 17,
                            Function::EndsWith => 18,
                            Function::Includes => 19,
                            _ => unreachable!("portable data function arm"),
                        });
                    }
                    Function::Extension(extension) if budget.cache_key_limit.is_some() => {
                        output.push(20);
                        name(output, extension)?;
                    }
                    Function::Extension(_) => return Err(unsupported()),
                }
                name(output, source)?;
                encode_terms(output, args, depth + 1, budget)?;
                encode_binding(output, binding)?;
            }
            Clause::Not { join, clauses } => {
                output.push(3);
                encode_optional_variables(output, join)?;
                encode_clauses(output, clauses, depth + 1, budget)?;
            }
            Clause::Or { join, branches } => {
                output.push(4);
                encode_optional_variables(output, join)?;
                length(output, branches.len())?;
                for branch in branches {
                    encode_clauses(output, branch, depth + 1, budget)?;
                }
            }
            Clause::Rule {
                source,
                name: rule,
                args,
            } => {
                output.push(5);
                name(output, source)?;
                name(output, rule)?;
                encode_terms(output, args, depth + 1, budget)?;
            }
        }
    }
    Ok(())
}
fn decode_clauses(
    cursor: &mut Cursor<'_>,
    depth: usize,
    budget: &mut Budget,
) -> Result<Vec<Clause>, SemanticError> {
    let mut clauses = Vec::new();
    for _ in 0..count(cursor)? {
        budget.node(depth)?;
        clauses.push(match cursor.u8()? {
            0 => {
                let source = read_name(cursor)?;
                let entity = decode_term(cursor, depth + 1, budget)?;
                let attribute = decode_term(cursor, depth + 1, budget)?;
                let value = decode_term(cursor, depth + 1, budget)?;
                let transaction = if cursor.boolean()? {
                    Some(decode_term(cursor, depth + 1, budget)?)
                } else {
                    None
                };
                let added = if cursor.boolean()? {
                    Some(decode_term(cursor, depth + 1, budget)?)
                } else {
                    None
                };
                Clause::Pattern(Box::new(DataPattern {
                    source,
                    entity,
                    attribute,
                    value,
                    transaction,
                    added,
                }))
            }
            1 => {
                let predicate = match cursor.u8()? {
                    0 => Predicate::Eq,
                    1 => Predicate::NotEq,
                    2 => Predicate::Less,
                    3 => Predicate::LessOrEqual,
                    4 => Predicate::Greater,
                    5 => Predicate::GreaterOrEqual,
                    6 => Predicate::Missing,
                    tag => return Err(invalid_tag("query predicate", tag)),
                };
                Clause::Predicate {
                    predicate,
                    source: read_name(cursor)?,
                    args: decode_terms(cursor, depth + 1, budget)?,
                }
            }
            2 => {
                let function = match cursor.u8()? {
                    0 => Function::Ground,
                    1 => Function::Add,
                    2 => Function::Subtract,
                    3 => Function::Multiply,
                    4 => Function::Divide,
                    5 => Function::Tuple,
                    6 => Function::Untuple,
                    7 => Function::GetElse,
                    8 => Function::GetSome,
                    9 => Function::Query(Box::new(decode_query(cursor, depth + 1, budget)?)),
                    10 => Function::TxIds,
                    11 => Function::TxData,
                    12 => Function::Fulltext,
                    13 => Function::Count,
                    14 => Function::Quot,
                    15 => Function::Subs,
                    16 => Function::Str,
                    17 => Function::StartsWith,
                    18 => Function::EndsWith,
                    19 => Function::Includes,
                    tag => return Err(invalid_tag("query function", tag)),
                };
                Clause::Function {
                    function,
                    source: read_name(cursor)?,
                    args: decode_terms(cursor, depth + 1, budget)?,
                    binding: decode_binding(cursor)?,
                }
            }
            3 => Clause::Not {
                join: decode_optional_variables(cursor)?,
                clauses: decode_clauses(cursor, depth + 1, budget)?,
            },
            4 => {
                let join = decode_optional_variables(cursor)?;
                let branches = (0..count(cursor)?)
                    .map(|_| decode_clauses(cursor, depth + 1, budget))
                    .collect::<Result<_, _>>()?;
                Clause::Or { join, branches }
            }
            5 => Clause::Rule {
                source: read_name(cursor)?,
                name: read_name(cursor)?,
                args: decode_terms(cursor, depth + 1, budget)?,
            },
            6 => Clause::RelationPattern(Box::new(RelationPattern {
                source: read_name(cursor)?,
                terms: decode_terms(cursor, depth + 1, budget)?,
            })),
            tag => return Err(invalid_tag("native query clause", tag)),
        });
    }
    Ok(clauses)
}

fn encode_query(
    output: &mut Vec<u8>,
    query: &Query,
    depth: usize,
    budget: &mut Budget,
) -> Result<(), SemanticError> {
    budget.node(depth)?;
    let elements: &[FindElement] = match &query.find {
        FindSpec::Relation(elements) => {
            output.push(0);
            elements
        }
        FindSpec::Collection(element) => {
            output.push(1);
            std::slice::from_ref(element)
        }
        FindSpec::Tuple(elements) => {
            output.push(2);
            elements
        }
        FindSpec::Scalar(element) => {
            output.push(3);
            std::slice::from_ref(element)
        }
    };
    length(output, elements.len())?;
    for element in elements {
        encode_find(output, element, depth + 1, budget)?;
    }
    encode_variables(output, &query.with)?;
    length(output, query.inputs.len())?;
    for input in &query.inputs {
        match input {
            InputSpec::Scalar(variable) => {
                output.push(0);
                name(output, variable.name())?;
            }
            InputSpec::Tuple(slots) => {
                output.push(1);
                encode_slots(output, slots)?;
            }
            InputSpec::Collection(variable) => {
                output.push(2);
                name(output, variable.name())?;
            }
            InputSpec::Relation(slots) => {
                output.push(3);
                encode_slots(output, slots)?;
            }
        }
    }
    encode_clauses(output, &query.clauses, depth + 1, budget)?;
    length(output, query.rules.len())?;
    for rule in &query.rules {
        budget.node(depth + 1)?;
        name(output, &rule.name)?;
        encode_variables(output, &rule.head)?;
        length(output, rule.required.len())?;
        for index in &rule.required {
            put_u64(output, *index as u64);
        }
        encode_clauses(output, &rule.clauses, depth + 1, budget)?;
    }
    Ok(())
}
fn decode_query(
    cursor: &mut Cursor<'_>,
    depth: usize,
    budget: &mut Budget,
) -> Result<Query, SemanticError> {
    budget.node(depth)?;
    let tag = cursor.u8()?;
    let mut elements = (0..count(cursor)?)
        .map(|_| decode_find(cursor, depth + 1, budget))
        .collect::<Result<Vec<_>, _>>()?;
    let find = match tag {
        0 => FindSpec::Relation(elements),
        2 => FindSpec::Tuple(elements),
        1 if elements.len() == 1 => FindSpec::Collection(elements.pop().unwrap()),
        3 if elements.len() == 1 => FindSpec::Scalar(elements.pop().unwrap()),
        _ => return Err(invalid_tag("native find shape", tag)),
    };
    let with = decode_variables(cursor)?;
    let inputs = (0..count(cursor)?)
        .map(|_| {
            Ok(match decode_binding(cursor)? {
                Binding::Scalar(variable) => InputSpec::Scalar(variable),
                Binding::Tuple(slots) => InputSpec::Tuple(slots),
                Binding::Collection(variable) => InputSpec::Collection(variable),
                Binding::Relation(slots) => InputSpec::Relation(slots),
            })
        })
        .collect::<Result<_, SemanticError>>()?;
    let clauses = decode_clauses(cursor, depth + 1, budget)?;
    let mut rules = Vec::new();
    for _ in 0..count(cursor)? {
        budget.node(depth + 1)?;
        let name = read_name(cursor)?;
        let head = decode_variables(cursor)?;
        let required = (0..count(cursor)?)
            .map(|_| number(cursor))
            .collect::<Result<_, _>>()?;
        rules.push(Rule {
            name,
            head,
            required,
            clauses: decode_clauses(cursor, depth + 1, budget)?,
        });
    }
    Ok(Query {
        find,
        with,
        inputs,
        clauses,
        rules,
    })
}
fn encode_find(
    output: &mut Vec<u8>,
    element: &FindElement,
    depth: usize,
    budget: &mut Budget,
) -> Result<(), SemanticError> {
    budget.node(depth)?;
    match element {
        FindElement::Variable(variable) => {
            output.push(0);
            name(output, variable.name())?;
        }
        FindElement::CustomAggregate(call) if budget.cache_key_limit.is_some() => {
            output.push(3);
            name(output, &call.name)?;
            length(output, call.args.len())?;
            for argument in &call.args {
                budget.node(depth + 1)?;
                match argument {
                    AggregateArg::Variable(variable) => {
                        output.push(0);
                        name(output, variable.name())?;
                    }
                    AggregateArg::Constant(value) => {
                        output.push(1);
                        encode_query_value(output, value, depth + 1, budget)?;
                    }
                    AggregateArg::Source(source) => {
                        output.push(2);
                        name(output, source)?;
                    }
                }
            }
        }
        FindElement::CustomAggregate(_) => return Err(unsupported()),
        FindElement::Aggregate { function, variable } => {
            output.push(1);
            output.push(match function {
                Aggregate::Count => 0,
                Aggregate::CountDistinct => 1,
                Aggregate::Min => 2,
                Aggregate::Max => 3,
                Aggregate::Sum => 4,
                Aggregate::Average => 5,
                Aggregate::Distinct => 6,
                Aggregate::Median => 7,
                Aggregate::Variance => 8,
                Aggregate::StandardDeviation => 9,
                Aggregate::MinN(_) => 10,
                Aggregate::MaxN(_) => 11,
                Aggregate::Rand(_) if budget.cache_key_limit.is_some() => 12,
                Aggregate::Sample(_) if budget.cache_key_limit.is_some() => 13,
                Aggregate::Rand(_) | Aggregate::Sample(_) => return Err(unsupported()),
            });
            if let Aggregate::MinN(n)
            | Aggregate::MaxN(n)
            | Aggregate::Rand(n)
            | Aggregate::Sample(n) = function
            {
                put_u64(output, *n as u64);
            }
            name(output, variable.name())?;
        }
        FindElement::Pull {
            source,
            variable,
            pattern,
        } => {
            output.push(2);
            name(output, source)?;
            name(output, variable.name())?;
            encode_pull(output, pattern, depth + 1, budget)?;
        }
    }
    Ok(())
}
fn decode_find(
    cursor: &mut Cursor<'_>,
    depth: usize,
    budget: &mut Budget,
) -> Result<FindElement, SemanticError> {
    budget.node(depth)?;
    Ok(match cursor.u8()? {
        0 => FindElement::Variable(Variable::new(read_name(cursor)?)?),
        1 => {
            let function = match cursor.u8()? {
                0 => Aggregate::Count,
                1 => Aggregate::CountDistinct,
                2 => Aggregate::Min,
                3 => Aggregate::Max,
                4 => Aggregate::Sum,
                5 => Aggregate::Average,
                6 => Aggregate::Distinct,
                7 => Aggregate::Median,
                8 => Aggregate::Variance,
                9 => Aggregate::StandardDeviation,
                10 => Aggregate::MinN(number(cursor)?),
                11 => Aggregate::MaxN(number(cursor)?),
                tag => return Err(invalid_tag("query aggregate", tag)),
            };
            FindElement::Aggregate {
                function,
                variable: Variable::new(read_name(cursor)?)?,
            }
        }
        2 => FindElement::Pull {
            source: read_name(cursor)?,
            variable: Variable::new(read_name(cursor)?)?,
            pattern: Box::new(decode_pull(cursor, depth + 1, budget)?),
        },
        tag => return Err(invalid_tag("query find element", tag)),
    })
}

fn encode_attribute(output: &mut Vec<u8>, attribute: &AttributeName) -> Result<(), SemanticError> {
    match attribute {
        AttributeName::Id(id) => {
            output.push(0);
            output.extend_from_slice(&id.to_be_bytes());
        }
        AttributeName::Ident(ident) => {
            output.push(1);
            encode_keyword(output, ident)?;
        }
    }
    payload_size(output)
}
fn decode_attribute(cursor: &mut Cursor<'_>) -> Result<AttributeName, SemanticError> {
    Ok(match cursor.u8()? {
        0 => AttributeName::Id(cursor.u32()?),
        1 => AttributeName::Ident(decode_keyword(cursor)?),
        tag => return Err(invalid_tag("pull attribute", tag)),
    })
}
fn encode_pull(
    output: &mut Vec<u8>,
    pattern: &PullPattern,
    depth: usize,
    budget: &mut Budget,
) -> Result<(), SemanticError> {
    budget.node(depth)?;
    put_bool(output, pattern.wildcard);
    length(output, pattern.attributes.len())?;
    for attribute in &pattern.attributes {
        budget.node(depth + 1)?;
        match &attribute.direction {
            PullDirection::Forward(attribute) => {
                output.push(0);
                encode_attribute(output, attribute)?;
            }
            PullDirection::Reverse(attribute) => {
                output.push(1);
                encode_attribute(output, attribute)?;
            }
            PullDirection::SchemaResolved(keyword) => {
                output.push(2);
                encode_keyword(output, keyword)?;
            }
        }
        for value in [&attribute.alias, &attribute.default] {
            put_bool(output, value.is_some());
            if let Some(value) = value {
                encode_query_value(output, value, depth + 1, budget)?;
            }
        }
        match attribute.limit {
            PullLimit::Default => output.push(0),
            PullLimit::Limit(n) => {
                output.push(1);
                put_u64(output, n as u64);
            }
            PullLimit::Unlimited => output.push(2),
        }
        match &attribute.nested {
            None => output.push(0),
            Some(PullNested::Pattern(pattern)) => {
                output.push(1);
                encode_pull(output, pattern, depth + 1, budget)?;
            }
            Some(PullNested::Recursion(None)) => output.push(2),
            Some(PullNested::Recursion(Some(n))) => {
                output.push(3);
                put_u64(output, *n as u64);
            }
        }
        output.push(match &attribute.transform {
            None => 0,
            Some(PullTransform::String) => 1,
            Some(PullTransform::Keyword) => 2,
            Some(PullTransform::Symbol) => 3,
            Some(PullTransform::Name) => 4,
            Some(PullTransform::Namespace) => 5,
            Some(PullTransform::Function { .. }) => return Err(unsupported()),
        });
    }
    Ok(())
}
fn decode_pull(
    cursor: &mut Cursor<'_>,
    depth: usize,
    budget: &mut Budget,
) -> Result<PullPattern, SemanticError> {
    budget.node(depth)?;
    let wildcard = cursor.boolean()?;
    let mut attributes = Vec::new();
    for _ in 0..count(cursor)? {
        budget.node(depth + 1)?;
        let direction = match cursor.u8()? {
            0 => PullDirection::Forward(decode_attribute(cursor)?),
            1 => PullDirection::Reverse(decode_attribute(cursor)?),
            2 => PullDirection::SchemaResolved(decode_keyword(cursor)?),
            tag => return Err(invalid_tag("pull direction", tag)),
        };
        let alias = if cursor.boolean()? {
            Some(decode_query_value(cursor, depth + 1, budget)?)
        } else {
            None
        };
        let default = if cursor.boolean()? {
            Some(decode_query_value(cursor, depth + 1, budget)?)
        } else {
            None
        };
        let limit = match cursor.u8()? {
            0 => PullLimit::Default,
            1 => PullLimit::Limit(number(cursor)?),
            2 => PullLimit::Unlimited,
            tag => return Err(invalid_tag("pull limit", tag)),
        };
        let nested = match cursor.u8()? {
            0 => None,
            1 => Some(PullNested::Pattern(Box::new(decode_pull(
                cursor,
                depth + 1,
                budget,
            )?))),
            2 => Some(PullNested::Recursion(None)),
            3 => Some(PullNested::Recursion(Some(number(cursor)?))),
            tag => return Err(invalid_tag("pull nesting", tag)),
        };
        let transform = match cursor.u8()? {
            0 => None,
            1 => Some(PullTransform::String),
            2 => Some(PullTransform::Keyword),
            3 => Some(PullTransform::Symbol),
            4 => Some(PullTransform::Name),
            5 => Some(PullTransform::Namespace),
            tag => return Err(invalid_tag("pull transform", tag)),
        };
        attributes.push(PullAttribute {
            direction,
            alias,
            default,
            limit,
            nested,
            transform,
        });
    }
    Ok(PullPattern {
        wildcard,
        attributes,
    })
}
fn encode_query_value(
    output: &mut Vec<u8>,
    value: &QueryValue,
    depth: usize,
    budget: &mut Budget,
) -> Result<(), SemanticError> {
    let mut work = 0usize;
    let mut check = |amount: usize| {
        work = work.saturating_add(amount);
        if work > MAX_NODES * MAX_DEPTH {
            return Err(fault(
                "program/native-query-shape-limit",
                "query value validation exceeds its work bound",
            ));
        }
        Ok(())
    };
    let size = value.measure_with(&mut check)?;
    if size.nodes > MAX_NODES
        || size.depth.saturating_add(depth) > MAX_DEPTH + 1
        || size.retained_bytes > budget.cache_key_limit.unwrap_or(super::MAX_PROGRAM_BYTES)
    {
        return Err(fault(
            "program/native-query-shape-limit",
            "query value exceeds durable shape or retention bounds",
        ));
    }
    value.validate_with(&mut check)?;
    encode_query_value_node(output, value, depth, budget)
}

fn encode_query_value_node(
    output: &mut Vec<u8>,
    value: &QueryValue,
    depth: usize,
    budget: &mut Budget,
) -> Result<(), SemanticError> {
    budget.node(depth)?;
    match value {
        QueryValue::Nil => output.push(0),
        QueryValue::Scalar(value) => {
            output.push(1);
            encode_scalar(output, value, depth + 1, budget)?;
        }
        QueryValue::Collection(values) | QueryValue::Tuple(values) => {
            output.push(if matches!(value, QueryValue::Collection(_)) {
                2
            } else {
                3
            });
            length(output, values.len())?;
            for value in values {
                encode_query_value_node(output, value, depth + 1, budget)?;
            }
        }
        QueryValue::Map(entries) => {
            output.push(4);
            length(output, entries.len())?;
            for (key, value) in entries {
                encode_query_value_node(output, key, depth + 1, budget)?;
                encode_query_value_node(output, value, depth + 1, budget)?;
            }
        }
        QueryValue::Set(values) if budget.cache_key_limit.is_some() => {
            output.push(5);
            length(output, values.len())?;
            for value in values {
                encode_query_value_node(output, value, depth + 1, budget)?;
            }
        }
        QueryValue::Set(values) => {
            output.push(5);
            // Sets have no presentation order. Logical ties choose the least
            // representation bytes, so mixed numeric duplicates do not let
            // input order choose a durable payload's representative.
            let mut entries = Vec::new();
            let mut retained = 0usize;
            for value in values {
                let mut bytes = Vec::new();
                encode_query_value_node(&mut bytes, value, depth + 1, budget)?;
                retained = retained.saturating_add(bytes.len());
                if retained > super::MAX_PROGRAM_BYTES {
                    return Err(fault(
                        "program/payload-limit",
                        "set encoding exceeds the durable byte limit",
                    ));
                }
                entries.push((value, bytes));
            }
            let mut failure = None;
            entries.sort_by(|left, right| {
                if failure.is_some() {
                    return std::cmp::Ordering::Equal;
                }
                match left
                    .0
                    .compare_with(right.0, &mut |work| budget.comparison(work))
                {
                    Ok(order) => order.then_with(|| left.1.cmp(&right.1)),
                    Err(error) => {
                        failure = Some(error);
                        std::cmp::Ordering::Equal
                    }
                }
            });
            if let Some(error) = failure.take() {
                return Err(error);
            }
            entries.dedup_by(|right, left| {
                match left
                    .0
                    .compare_with(right.0, &mut |work| budget.comparison(work))
                {
                    Ok(order) => order.is_eq(),
                    Err(error) => {
                        failure = Some(error);
                        false
                    }
                }
            });
            if let Some(error) = failure {
                return Err(error);
            }
            length(output, entries.len())?;
            for (_, bytes) in entries {
                output.extend_from_slice(&bytes);
            }
        }
        QueryValue::Char(value) => {
            output.push(6);
            output.extend_from_slice(&u32::from(*value).to_be_bytes());
        }
        QueryValue::Tagged(tag, value) => {
            output.push(7);
            encode_symbol(output, tag)?;
            encode_query_value_node(output, value, depth + 1, budget)?;
        }
    }
    payload_size(output)
}
fn decode_query_value(
    cursor: &mut Cursor<'_>,
    depth: usize,
    budget: &mut Budget,
) -> Result<QueryValue, SemanticError> {
    budget.node(depth)?;
    Ok(match cursor.u8()? {
        0 => QueryValue::Nil,
        1 => QueryValue::Scalar(decode_value(cursor, 0)?),
        tag @ (2 | 3) => {
            let values = (0..count(cursor)?)
                .map(|_| decode_query_value(cursor, depth + 1, budget))
                .collect::<Result<_, _>>()?;
            if tag == 2 {
                QueryValue::Collection(values)
            } else {
                QueryValue::Tuple(values)
            }
        }
        4 => QueryValue::Map(
            (0..count(cursor)?)
                .map(|_| {
                    Ok((
                        decode_query_value(cursor, depth + 1, budget)?,
                        decode_query_value(cursor, depth + 1, budget)?,
                    ))
                })
                .collect::<Result<_, SemanticError>>()?,
        ),
        5 => QueryValue::Set(
            (0..count(cursor)?)
                .map(|_| decode_query_value(cursor, depth + 1, budget))
                .collect::<Result<_, _>>()?,
        ),
        6 => QueryValue::Char(char::from_u32(cursor.u32()?).ok_or_else(|| {
            fault(
                "encoding/query-character",
                "query character is not a Unicode scalar",
            )
        })?),
        7 => QueryValue::Tagged(
            decode_symbol(cursor)?,
            Box::new(decode_query_value(cursor, depth + 1, budget)?),
        ),
        tag => return Err(invalid_tag("query value", tag)),
    })
}

/// A separately versioned encoding used only by additive general query output.
pub(super) fn encode_general_value(
    output: &mut Vec<u8>,
    value: &QueryValue,
) -> Result<(), SemanticError> {
    output.extend_from_slice(&1u16.to_be_bytes());
    encode_query_value(output, value, 0, &mut Budget::default())
}
