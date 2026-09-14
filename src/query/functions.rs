//! Portable, source-free data functions. String positions count Unicode scalar
//! values, not UTF-8 bytes or the older VM Length instruction's UTF-16 units.
use super::*;
use crate::{SemanticError, Value};
use num_bigint::BigInt;
use num_traits::ToPrimitive;
use std::mem::size_of;

fn arity(message: &str) -> SemanticError {
    SemanticError::incorrect("query/function-arity", message)
}
fn value_type(message: &str) -> SemanticError {
    SemanticError::incorrect("query/function-type", message)
}
fn string(value: &BoundValue) -> Result<&str, SemanticError> {
    match value.stored() {
        Some(Value::String(value)) => Ok(value),
        _ => Err(value_type("string function requires a string")),
    }
}

pub(super) fn execute(
    function: Function,
    args: &[BoundValue],
    state: &mut State<'_>,
) -> Result<Value, SemanticError> {
    state.check(1)?;
    match function {
        Function::Count => {
            let [value] = args else {
                return Err(arity("count requires one argument"));
            };
            let count = match value.borrowed() {
                QueryValueRef::Nil | QueryValueRef::Query(QueryValue::Nil) => 0,
                QueryValueRef::Query(QueryValue::Tuple(v) | QueryValue::Collection(v)) => v.len(),
                QueryValueRef::Query(QueryValue::Map(v)) => v.len(),
                QueryValueRef::Query(QueryValue::Set(v)) => ordered_members(v, state)?.len(),
                QueryValueRef::Stored(Value::Tuple(v)) => v.len(),
                QueryValueRef::Stored(Value::Bytes(v)) => v.len(),
                QueryValueRef::Stored(Value::String(v)) => scalar_count(v, state)?,
                _ => {
                    return Err(value_type(
                        "count requires nil, a string, bytes or a collection",
                    ));
                }
            };
            Ok(Value::Long(
                i64::try_from(count).map_err(|_| value_type("count exceeds Long"))?,
            ))
        }
        Function::Quot => {
            let [left, right] = args else {
                return Err(arity("quot requires two arguments"));
            };
            let left = super::require_stored(left, "query/numeric-type")?;
            let right = super::require_stored(right, "query/numeric-type")?;
            let max_bytes = state.control.max_numeric_bytes.min(
                state
                    .max_value_bytes
                    .saturating_sub(state.stats.allocated_value_bytes),
            );
            numeric::quotient(
                left,
                right,
                &mut numeric::Budget {
                    max_bytes,
                    charge: |work, bytes| {
                        state.check(work)?;
                        state.charge_value_bytes(bytes)
                    },
                },
            )
        }
        Function::Subs => substring(args, state),
        Function::Str => {
            let mut output = String::new();
            for value in args {
                render(value.borrowed(), &mut output, state)?;
            }
            Ok(Value::String(output))
        }
        Function::StartsWith | Function::EndsWith | Function::Includes => {
            let [haystack, needle] = args else {
                return Err(arity("string predicates require two arguments"));
            };
            let haystack = string(haystack)?.as_bytes();
            let needle = string(needle)?.as_bytes();
            let found = match function {
                Function::StartsWith => byte_equal_prefix(haystack, needle, state)?,
                Function::EndsWith => {
                    if haystack.len() < needle.len() {
                        false
                    } else {
                        byte_equal_prefix(
                            &haystack[haystack.len() - needle.len()..],
                            needle,
                            state,
                        )?
                    }
                }
                Function::Includes => includes(haystack, needle, state)?,
                _ => unreachable!(),
            };
            Ok(Value::Bool(found))
        }
        _ => unreachable!("portable dispatch is closed"),
    }
}

fn scalar_count(value: &str, state: &mut State<'_>) -> Result<usize, SemanticError> {
    let mut count = 0;
    for character in value.chars() {
        state.check(character.len_utf8())?;
        count += 1;
    }
    Ok(count)
}

fn position(value: &BoundValue) -> Result<usize, SemanticError> {
    let position = match value.stored() {
        Some(Value::Long(value)) => usize::try_from(*value).ok(),
        Some(Value::BigInt(value)) => value.to_usize(),
        _ => None,
    };
    position.ok_or_else(|| value_type("subs indices must be nonnegative Long or BigInt integers"))
}

fn substring(args: &[BoundValue], state: &mut State<'_>) -> Result<Value, SemanticError> {
    let (text, start, end) = match args {
        [text, start] => (string(text)?, position(start)?, None),
        [text, start, end] => (string(text)?, position(start)?, Some(position(end)?)),
        _ => return Err(arity("subs requires a string, start, and optional end")),
    };
    if end.is_some_and(|end| end < start) {
        return Err(value_type("subs end must not precede start"));
    }
    let mut from = None;
    let mut to = None;
    for (index, (offset, character)) in text
        .char_indices()
        .chain(std::iter::once((text.len(), '\0')))
        .enumerate()
    {
        state.check(character.len_utf8())?;
        if index == start {
            from = Some(offset);
        }
        if end == Some(index) {
            to = Some(offset);
            break;
        }
    }
    let from = from.ok_or_else(|| value_type("subs start exceeds string length"))?;
    let to = match end {
        Some(_) => to.ok_or_else(|| value_type("subs end exceeds string length"))?,
        None => text.len(),
    };
    let result = &text[from..to];
    state.check(result.len())?;
    state.charge_value_bytes(result.len())?;
    Ok(Value::String(result.to_owned()))
}

fn byte_equal_prefix(
    haystack: &[u8],
    needle: &[u8],
    state: &mut State<'_>,
) -> Result<bool, SemanticError> {
    if haystack.len() < needle.len() {
        return Ok(false);
    }
    for (a, b) in haystack.iter().zip(needle) {
        state.check(1)?;
        if a != b {
            return Ok(false);
        }
    }
    Ok(true)
}

// KMP avoids quadratic comparisons for long, nearly equal prefixes. The table
// and every compared byte share the enclosing query's allocation/work budget.
fn includes(haystack: &[u8], needle: &[u8], state: &mut State<'_>) -> Result<bool, SemanticError> {
    if needle.is_empty() {
        return Ok(true);
    }
    if needle.len() > haystack.len() {
        return Ok(false);
    }
    // Initializing the table is real linear work, admitted before allocation.
    state.check(needle.len())?;
    state.charge_value_bytes(needle.len().saturating_mul(size_of::<usize>()))?;
    let mut prefix = vec![0; needle.len()];
    let mut matched = 0;
    for index in 1..needle.len() {
        loop {
            state.check(1)?;
            if needle[index] == needle[matched] {
                matched += 1;
                break;
            }
            if matched == 0 {
                break;
            }
            matched = prefix[matched - 1];
        }
        prefix[index] = matched;
    }
    matched = 0;
    for byte in haystack {
        loop {
            state.check(1)?;
            if *byte == needle[matched] {
                matched += 1;
                break;
            }
            if matched == 0 {
                break;
            }
            matched = prefix[matched - 1];
        }
        if matched == needle.len() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn compare(
    left: &QueryValue,
    right: &QueryValue,
    state: &mut State<'_>,
) -> Result<std::cmp::Ordering, SemanticError> {
    state.prepare_key(QueryValueRef::Query(left))?;
    state.prepare_key(QueryValueRef::Query(right))?;
    left.compare_with(right, &mut |work| state.check(work))
}

fn ordered_members<'a>(
    values: &'a [QueryValue],
    state: &mut State<'_>,
) -> Result<Vec<&'a QueryValue>, SemanticError> {
    state.check(values.len())?;
    state.charge_value_bytes(values.len().saturating_mul(2 * size_of::<&QueryValue>()))?;
    let mut members: Vec<_> = values.iter().collect();
    checked_aggregate_sort(&mut members, &mut |a, b| compare(a, b, state))?;
    let mut length = 0;
    for index in 0..members.len() {
        if length == 0 || !compare(members[length - 1], members[index], state)?.is_eq() {
            members[length] = members[index];
            length += 1;
        }
    }
    members.truncate(length);
    Ok(members)
}

fn emit(output: &mut String, text: &str, state: &mut State<'_>) -> Result<(), SemanticError> {
    state.check(text.len())?;
    let required = output
        .len()
        .checked_add(text.len())
        .ok_or_else(|| super::resource("query/value-byte-limit", "string result size overflow"))?;
    if required > output.capacity() {
        let capacity = required.max(output.capacity().saturating_mul(2)).max(16);
        state.charge_value_bytes(capacity.saturating_sub(output.capacity()))?;
        output.reserve_exact(capacity - output.len());
    }
    output.push_str(text);
    Ok(())
}

fn character(output: &mut String, value: char, state: &mut State<'_>) -> Result<(), SemanticError> {
    emit(output, value.encode_utf8(&mut [0; 4]), state)
}

fn quoted(output: &mut String, text: &str, state: &mut State<'_>) -> Result<(), SemanticError> {
    emit(output, "\"", state)?;
    for value in text.chars() {
        match value {
            '"' => emit(output, "\\\"", state)?,
            '\\' => emit(output, "\\\\", state)?,
            '\n' => emit(output, "\\n", state)?,
            '\r' => emit(output, "\\r", state)?,
            '\t' => emit(output, "\\t", state)?,
            value if value.is_control() => {
                state.charge_value_bytes(6)?;
                emit(output, &format!("\\u{:04x}", u32::from(value)), state)?;
            }
            value => character(output, value, state)?,
        }
    }
    emit(output, "\"", state)
}

fn integer_text(value: &BigInt, state: &mut State<'_>) -> Result<String, SemanticError> {
    let bound = usize::try_from(value.bits() / 3 + 2)
        .map_err(|_| super::resource("query/numeric-capacity", "numeric text exceeds capacity"))?;
    if bound > state.control.max_numeric_bytes {
        return Err(super::resource(
            "query/numeric-capacity",
            "numeric text exceeds capacity",
        ));
    }
    state.check(bound.saturating_mul(bound / 32 + 1))?;
    state.charge_value_bytes(bound)?;
    Ok(value.to_str_radix(10))
}

// The query-only renderer follows the existing native Pull text shapes but
// supplies cooperative traversal, scratch and output admission. It deliberately
// does not route through a database or expand compact decimal exponents.
fn render(
    value: QueryValueRef<'_>,
    output: &mut String,
    state: &mut State<'_>,
) -> Result<(), SemanticError> {
    enum Task<'a> {
        Value(QueryValueRef<'a>, bool),
        Text(&'static str),
    }
    state.charge_value_bytes(size_of::<Task<'_>>())?;
    let mut pending = vec![Task::Value(value, false)];
    while let Some(task) = pending.pop() {
        state.check(1)?;
        match task {
            Task::Text(text) => emit(output, text, state)?,
            Task::Value(QueryValueRef::Nil | QueryValueRef::Query(QueryValue::Nil), nested) => {
                if nested {
                    emit(output, "nil", state)?;
                }
            }
            Task::Value(QueryValueRef::Query(QueryValue::Scalar(value)), nested) => {
                pending.push(Task::Value(QueryValueRef::Stored(value), nested))
            }
            Task::Value(QueryValueRef::Query(QueryValue::Char(value)), nested) => {
                if nested {
                    emit(output, "\\", state)?;
                    let named = match value {
                        ' ' => Some("space"),
                        '\n' => Some("newline"),
                        '\r' => Some("return"),
                        '\t' => Some("tab"),
                        '\u{0008}' => Some("backspace"),
                        '\u{000c}' => Some("formfeed"),
                        _ => None,
                    };
                    if let Some(named) = named {
                        emit(output, named, state)?;
                        continue;
                    }
                }
                character(output, *value, state)?;
            }
            Task::Value(QueryValueRef::Query(QueryValue::Tagged(tag, value)), _) => {
                emit(output, "#", state)?;
                if let Some(namespace) = &tag.namespace {
                    emit(output, namespace, state)?;
                    emit(output, "/", state)?;
                }
                emit(output, &tag.name, state)?;
                emit(output, " ", state)?;
                pending.push(Task::Value(QueryValueRef::Query(value), true));
            }
            Task::Value(QueryValueRef::Query(QueryValue::Set(values)), _) => {
                let values = ordered_members(values, state)?;
                state.charge_value_bytes(
                    (values.len().saturating_mul(2) + 1).saturating_mul(size_of::<Task<'_>>()),
                )?;
                emit(output, "#{", state)?;
                pending.push(Task::Text("}"));
                for (index, value) in values.into_iter().enumerate().rev() {
                    pending.push(Task::Value(QueryValueRef::Query(value), true));
                    if index > 0 {
                        pending.push(Task::Text(" "));
                    }
                }
            }
            Task::Value(
                QueryValueRef::Query(QueryValue::Tuple(values) | QueryValue::Collection(values)),
                _,
            ) => {
                state.check(values.len())?;
                state.charge_value_bytes(
                    (values.len().saturating_mul(2) + 1).saturating_mul(size_of::<Task<'_>>()),
                )?;
                emit(output, "[", state)?;
                pending.push(Task::Text("]"));
                for (index, value) in values.iter().enumerate().rev() {
                    pending.push(Task::Value(QueryValueRef::Query(value), true));
                    if index > 0 {
                        pending.push(Task::Text(" "));
                    }
                }
            }
            Task::Value(QueryValueRef::Query(QueryValue::Map(entries)), _) => {
                state.check(entries.len())?;
                state.charge_value_bytes(
                    entries
                        .len()
                        .saturating_mul(2 * size_of::<&(QueryValue, QueryValue)>()),
                )?;
                let mut ordered: Vec<_> = entries.iter().collect();
                checked_aggregate_sort(&mut ordered, &mut |a, b| compare(&a.0, &b.0, state))?;
                state.charge_value_bytes(
                    (ordered.len().saturating_mul(4) + 1).saturating_mul(size_of::<Task<'_>>()),
                )?;
                emit(output, "{", state)?;
                pending.push(Task::Text("}"));
                for (index, (key, value)) in ordered.into_iter().enumerate().rev() {
                    pending.push(Task::Value(QueryValueRef::Query(value), true));
                    pending.push(Task::Text(" "));
                    pending.push(Task::Value(QueryValueRef::Query(key), true));
                    if index > 0 {
                        pending.push(Task::Text(", "));
                    }
                }
            }
            Task::Value(QueryValueRef::Stored(Value::Tuple(values)), _) => {
                state.check(values.len())?;
                state.charge_value_bytes(
                    (values.len().saturating_mul(2) + 1).saturating_mul(size_of::<Task<'_>>()),
                )?;
                emit(output, "[", state)?;
                pending.push(Task::Text("]"));
                for (index, value) in values.iter().enumerate().rev() {
                    pending.push(Task::Value(
                        value
                            .as_ref()
                            .map_or(QueryValueRef::Nil, QueryValueRef::Stored),
                        true,
                    ));
                    if index > 0 {
                        pending.push(Task::Text(" "));
                    }
                }
            }
            Task::Value(QueryValueRef::Stored(value), nested) => {
                scalar_text(value, nested, output, state)?
            }
        }
    }
    Ok(())
}

fn scalar_text(
    value: &Value,
    nested: bool,
    output: &mut String,
    state: &mut State<'_>,
) -> Result<(), SemanticError> {
    match value {
        Value::String(value) if nested => quoted(output, value, state),
        Value::String(value) | Value::Uri(value) => emit(output, value, state),
        Value::Keyword(value) => {
            emit(output, ":", state)?;
            if let Some(namespace) = &value.namespace {
                emit(output, namespace, state)?;
                emit(output, "/", state)?;
            }
            emit(output, &value.name, state)
        }
        Value::Symbol(value) => {
            if let Some(namespace) = &value.namespace {
                emit(output, namespace, state)?;
                emit(output, "/", state)?;
            }
            emit(output, &value.name, state)
        }
        Value::BigInt(value) => {
            let text = integer_text(value, state)?;
            emit(output, &text, state)
        }
        Value::BigDec(value) => {
            let (coefficient, scale) = value.as_bigint_and_scale();
            let digits = integer_text(&coefficient, state)?;
            if u128::from(scale.unsigned_abs()) + digits.len() as u128 <= 128 {
                state.charge_value_bytes(132)?;
                emit(output, &value.to_string(), state)
            } else {
                emit(output, &digits, state)?;
                if scale != 0 {
                    state.charge_value_bytes(42)?;
                    emit(output, &format!("E{}", -i128::from(scale)), state)?;
                }
                Ok(())
            }
        }
        Value::Bytes(_) | Value::Function(_) => {
            let values: &[u8] = match value {
                Value::Bytes(values) => values,
                Value::Function(values) => values,
                _ => unreachable!(),
            };
            emit(
                output,
                if matches!(value, Value::Bytes(_)) {
                    "#bytes["
                } else {
                    "#function["
                },
                state,
            )?;
            for (index, byte) in values.iter().enumerate() {
                if index > 0 {
                    emit(output, " ", state)?;
                }
                state.charge_value_bytes(3)?;
                emit(output, &byte.to_string(), state)?;
            }
            emit(output, "]", state)
        }
        Value::Tuple(_) => unreachable!("tuples use iterative tasks"),
        _ => {
            state.check(1)?;
            state.charge_value_bytes(64)?;
            let text = match value {
                Value::Long(value) | Value::Instant(value) => value.to_string(),
                Value::Ref(value) => value.to_string(),
                Value::Bool(value) => value.to_string(),
                Value::Double(value) => value.to_string(),
                Value::Float(value) => value.to_string(),
                Value::Uuid(value) => format!("{value:032x}"),
                _ => unreachable!(),
            };
            emit(output, &text, state)
        }
    }
}

pub(super) fn evaluate_predicate(
    predicate: Predicate,
    source: &str,
    args: &[BoundValue],
    state: &mut State<'_>,
) -> Result<bool, SemanticError> {
    match predicate {
        Predicate::Eq
        | Predicate::NotEq
        | Predicate::Less
        | Predicate::LessOrEqual
        | Predicate::Greater
        | Predicate::GreaterOrEqual => {
            if args.len() != 2 {
                return Err(SemanticError::incorrect(
                    "query/predicate-arity",
                    "comparison predicates require two arguments",
                ));
            }
            let ordering = state.compare_keys(args[0].borrowed(), args[1].borrowed())?;
            Ok(match predicate {
                Predicate::Eq => ordering.is_eq(),
                Predicate::NotEq => !ordering.is_eq(),
                Predicate::Less => ordering.is_lt(),
                Predicate::LessOrEqual => !ordering.is_gt(),
                Predicate::Greater => ordering.is_gt(),
                Predicate::GreaterOrEqual => !ordering.is_lt(),
                Predicate::Missing => unreachable!(),
            })
        }
        Predicate::Missing => {
            if args.len() != 2 {
                return Err(SemanticError::incorrect(
                    "query/predicate-arity",
                    "missing requires entity and attribute",
                ));
            }
            let database = source_database(state, source)?;
            let entity = entity_value(database, require_stored(&args[0], "query/entity-value")?)?
                .ok_or_else(|| {
                SemanticError::incorrect("query/entity-value", "missing entity must be a ref")
            })?;
            let attribute =
                attribute_value(database, require_stored(&args[1], "query/attribute-value")?)?;
            Ok(database.values(entity, attribute)?.is_empty())
        }
    }
}

pub(super) fn evaluate_function(
    function: Function,
    source: &str,
    args: &[BoundValue],
    binding: &Binding,
    state: &mut State<'_>,
) -> Result<Vec<Vec<BoundValue>>, SemanticError> {
    match function {
        Function::Count
        | Function::Quot
        | Function::Subs
        | Function::Str
        | Function::StartsWith
        | Function::EndsWith
        | Function::Includes => functions::execute(function, args, state)
            .map(|value| vec![vec![BoundValue::Stored(value)]]),
        Function::TxIds | Function::TxData => {
            sources::log_function(function, source, args, binding, state)
        }
        Function::Fulltext => fulltext::execute(source, args, binding, state),
        Function::Query(query) => nested::execute(&query, source, args, binding, state),
        Function::Ground => ground_output(args, binding),
        Function::Tuple => Ok(vec![vec![if args
            .iter()
            .any(|value| matches!(value, BoundValue::Query(_)))
        {
            BoundValue::Query(QueryValue::Tuple(
                args.iter().map(BoundValue::query_value).collect(),
            ))
        } else {
            BoundValue::Stored(Value::Tuple(
                args.iter().map(|value| value.stored().cloned()).collect(),
            ))
        }]]),
        Function::Untuple => match args {
            [BoundValue::Stored(Value::Tuple(values))] => Ok(vec![
                values
                    .iter()
                    .cloned()
                    .map(|value| value.map_or(BoundValue::Nil, BoundValue::Stored))
                    .collect(),
            ]),
            [value @ BoundValue::Query(QueryValue::Tuple(_))] => {
                Ok(vec![value.sequence_values().expect("tuple variant")])
            }
            _ => Err(SemanticError::incorrect(
                "query/function-type",
                "untuple requires one tuple",
            )),
        },
        Function::Add | Function::Subtract | Function::Multiply | Function::Divide => {
            numeric_function(function, args, state)
                .map(|value| vec![vec![BoundValue::Stored(value)]])
        }
        Function::GetElse => {
            if args.len() != 3 {
                return Err(SemanticError::incorrect(
                    "query/function-arity",
                    "get-else requires entity, attribute, and default",
                ));
            }
            let database = source_database(state, source)?;
            let entity = entity_value(database, require_stored(&args[0], "query/entity-value")?)?
                .ok_or_else(|| {
                SemanticError::incorrect("query/entity-value", "get-else entity must be a ref")
            })?;
            let attribute =
                attribute_value(database, require_stored(&args[1], "query/attribute-value")?)?;
            if database.schema().attribute(attribute)?.cardinality != crate::Cardinality::One {
                return Err(SemanticError::incorrect(
                    "query/get-else-cardinality",
                    "get-else requires cardinality one",
                ));
            }
            if matches!(args[2], BoundValue::Nil) {
                return Err(SemanticError::incorrect(
                    "query/get-else-nil-default",
                    "get-else default cannot be nil",
                ));
            }
            let values = database.values(entity, attribute)?;
            Ok(vec![vec![values.first().map_or_else(
                || args[2].clone(),
                |value| BoundValue::Stored(value.clone()),
            )]])
        }
        Function::GetSome => {
            if args.len() < 2 {
                return Err(SemanticError::incorrect(
                    "query/function-arity",
                    "get-some requires entity and attributes",
                ));
            }
            let database = source_database(state, source)?;
            let entity = entity_value(database, require_stored(&args[0], "query/entity-value")?)?
                .ok_or_else(|| {
                SemanticError::incorrect("query/entity-value", "get-some entity must be a ref")
            })?;
            let attributes = args[1..]
                .iter()
                .map(|value| {
                    let attribute =
                        attribute_value(database, require_stored(value, "query/attribute-value")?)?;
                    if database.schema().attribute(attribute)?.cardinality
                        != crate::Cardinality::One
                    {
                        return Err(SemanticError::incorrect(
                            "query/get-some-cardinality",
                            "get-some requires cardinality-one attributes",
                        ));
                    }
                    Ok(attribute)
                })
                .collect::<Result<Vec<_>, _>>()?;
            for attribute in attributes {
                if let Some(found) = database.values(entity, attribute)?.first() {
                    return Ok(vec![vec![
                        BoundValue::Stored(Value::Ref(u64::from(attribute))),
                        BoundValue::Stored(found.clone()),
                    ]]);
                }
            }
            Ok(Vec::new())
        }
        Function::Extension(name) => {
            state.check(1)?;
            let extensions = state.extensions.ok_or_else(|| {
                SemanticError::incorrect(
                    "query/unknown-extension",
                    format!("no query extension registry supplies {name}"),
                )
            })?;
            for value in args {
                state.admit_ref(value.borrowed())?;
            }
            let extension_args: Vec<_> = args.iter().map(BoundValue::query_value).collect();
            // Native callbacks are trusted cooperative code, but receive only
            // the parent's remaining allowance. Persisted programs report
            // interpreter fuel back into that same allowance on every call.
            let mut control = state.control.clone();
            control.max_work = control.max_work.saturating_sub(state.work);
            control.max_value_bytes = state
                .max_value_bytes
                .saturating_sub(state.stats.allocated_value_bytes);
            control.timeout = state
                .deadline
                .map(|deadline| deadline.saturating_duration_since(Instant::now()));
            if let Some(QueryExtension {
                implementation: QueryExtensionImplementation::Pure(callback),
                ..
            }) = extensions.functions.get(&name)
            {
                let result = catch_unwind(AssertUnwindSafe(|| callback(&extension_args, &control)))
                    .map_err(|_| {
                        SemanticError::new(
                            ErrorCategory::Fault,
                            "query/local-extension-panicked",
                            format!("query extension {name} panicked"),
                        )
                    })?;
                state.check(0)?;
                let value = result?;
                state.validate_general(&value)?;
                state.charge_value_bytes(value.retained_bytes())?;
                return ground_output(&[BoundValue::from_query_value(value)], binding);
            }
            let database = source_database(state, source)?;
            let (rows, work) =
                extensions.invoke(&name, database, &extension_args, &control, state.deadline);
            state.check(work)?;
            let rows = rows?;
            for row in &rows {
                for value in row {
                    state.validate_general(value)?;
                    state.charge_value_bytes(value.retained_bytes())?;
                }
            }
            if rows.len() > state.control.max_result_rows
                || rows
                    .iter()
                    .fold(0_usize, |total, row| total.saturating_add(row.len()))
                    > state.control.max_intermediate_rows
            {
                return Err(resource(
                    "query/extension-output-limit",
                    "query extension exceeded its output limits",
                ));
            }
            Ok(rows
                .into_iter()
                .map(|row| row.into_iter().map(BoundValue::from_query_value).collect())
                .collect())
        }
    }
}

pub(super) fn numeric_function(
    function: Function,
    args: &[BoundValue],
    state: &mut State<'_>,
) -> Result<Value, SemanticError> {
    if args.len() != 2 {
        return Err(SemanticError::incorrect(
            "query/function-arity",
            "arithmetic functions require two arguments",
        ));
    }
    let left = require_stored(&args[0], "query/numeric-type")?;
    let right = require_stored(&args[1], "query/numeric-type")?;
    let max_bytes = state.control.max_numeric_bytes.min(
        state
            .max_value_bytes
            .saturating_sub(state.stats.allocated_value_bytes),
    );
    numeric::binary(
        &function,
        left,
        right,
        &mut numeric::Budget {
            max_bytes,
            charge: |work, bytes| {
                state.check(work)?;
                state.charge_value_bytes(bytes)
            },
        },
    )
}
