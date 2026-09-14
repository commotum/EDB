use super::*;

type TransformFunction = dyn Fn(&QueryValue) -> Result<QueryValue, SemanticError> + Send + Sync;

/// Peer-local value transformation, applied after limits/nesting and before a
/// default. A missing attribute is passed as `QueryValue::Nil`. Native callbacks
/// are explicitly supplied Rust code, not dynamically resolved JVM functions.
/// Callbacks are cooperatively controlled: cancellation/deadlines are checked
/// before and after invocation, but cannot preempt arbitrary Rust code.
#[derive(Clone)]
pub enum PullTransform {
    /// Native readable text. Strings are unquoted at the top level; collections
    /// and maps include delimiters. This is not a JVM/wire serialization format.
    String,
    Keyword,
    Symbol,
    Name,
    Namespace,
    Function {
        name: Arc<str>,
        function: Arc<TransformFunction>,
    },
}

impl PullTransform {
    pub fn new(
        name: impl Into<Arc<str>>,
        function: impl Fn(&QueryValue) -> Result<QueryValue, SemanticError> + Send + Sync + 'static,
    ) -> Self {
        Self::Function {
            name: name.into(),
            function: Arc::new(function),
        }
    }

    pub(super) fn apply(
        &self,
        value: &QueryValue,
        state: &mut PullState<'_, '_>,
    ) -> Result<QueryValue, SemanticError> {
        let scalar = |value| Ok(QueryValue::Scalar(value));
        match (self, value) {
            (Self::Function { function, .. }, value) => function(value),
            (Self::String, value) => scalar(Value::String(pull_value_text(value, state)?)),
            (Self::Keyword, QueryValue::Nil) => Ok(QueryValue::Nil),
            (Self::Keyword, QueryValue::Scalar(Value::Keyword(value))) => {
                scalar(Value::Keyword(value.clone()))
            }
            (Self::Keyword, QueryValue::Scalar(Value::Symbol(value))) => {
                scalar(Value::Keyword(Keyword {
                    namespace: value.namespace.clone(),
                    name: value.name.clone(),
                }))
            }
            (Self::Keyword, QueryValue::Scalar(Value::String(value))) => {
                let (namespace, name) = pull_name_parts(value.strip_prefix(':').unwrap_or(value));
                scalar(Value::Keyword(Keyword { namespace, name }))
            }
            (Self::Symbol, QueryValue::Scalar(Value::Symbol(value))) => {
                scalar(Value::Symbol(value.clone()))
            }
            (Self::Symbol, QueryValue::Scalar(Value::String(value))) => {
                let (namespace, name) = pull_name_parts(value);
                scalar(Value::Symbol(Symbol { namespace, name }))
            }
            (Self::Name, QueryValue::Scalar(Value::String(value))) => {
                scalar(Value::String(value.clone()))
            }
            (Self::Name, QueryValue::Scalar(Value::Keyword(value))) => {
                scalar(Value::String(value.name.clone()))
            }
            (Self::Name, QueryValue::Scalar(Value::Symbol(value))) => {
                scalar(Value::String(value.name.clone()))
            }
            (Self::Namespace, QueryValue::Scalar(Value::Keyword(value))) => {
                Ok(value.namespace.as_ref().map_or(QueryValue::Nil, |value| {
                    QueryValue::Scalar(Value::String(value.clone()))
                }))
            }
            (Self::Namespace, QueryValue::Scalar(Value::Symbol(value))) => {
                Ok(value.namespace.as_ref().map_or(QueryValue::Nil, |value| {
                    QueryValue::Scalar(Value::String(value.clone()))
                }))
            }
            _ => Err(SemanticError::incorrect(
                "pull/transform-type",
                "pull transform does not accept this value type",
            )),
        }
    }
}

impl std::fmt::Debug for PullTransform {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::String => formatter.write_str("String"),
            Self::Keyword => formatter.write_str("Keyword"),
            Self::Symbol => formatter.write_str("Symbol"),
            Self::Name => formatter.write_str("Name"),
            Self::Namespace => formatter.write_str("Namespace"),
            Self::Function { name, .. } => formatter.debug_tuple("Function").field(name).finish(),
        }
    }
}

impl PartialEq for PullTransform {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Function { function: left, .. },
                Self::Function {
                    function: right, ..
                },
            ) => Arc::ptr_eq(left, right),
            _ => std::mem::discriminant(self) == std::mem::discriminant(other),
        }
    }
}

impl Eq for PullTransform {}

pub(super) fn finish_attribute(
    key: QueryValue,
    value: Option<QueryValue>,
    default: Option<&QueryValue>,
    transform: Option<&PullTransform>,
    state: &mut PullState<'_, '_>,
) -> Result<Option<(QueryValue, QueryValue)>, SemanticError> {
    let value = if let Some(transform) = transform {
        state.check(1)?;
        let input = value.unwrap_or(QueryValue::Nil);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            transform.apply(&input, state)
        }))
        .map_err(|_| fault("pull/transform-panicked", "pull transform panicked"))?;
        state.check(0)?;
        let result = result?;
        state.charge_value_bytes(crate::query::query_value_allocation_bytes(&result))?;
        match result {
            QueryValue::Nil => None,
            value => Some(value),
        }
    } else {
        value
    };
    if value.is_none()
        && let Some(default) = default
    {
        state.charge_value_bytes(crate::query::query_value_allocation_bytes(default))?;
    }
    Ok(value.or_else(|| default.cloned()).map(|value| (key, value)))
}

fn pull_name_parts(value: &str) -> (Option<String>, String) {
    match value.split_once('/') {
        Some((namespace, name)) if !namespace.is_empty() && !name.is_empty() => {
            (Some(namespace.to_owned()), name.to_owned())
        }
        _ => (None, value.to_owned()),
    }
}

fn pull_value_text(
    value: &QueryValue,
    state: &mut PullState<'_, '_>,
) -> Result<String, SemanticError> {
    enum Task<'a> {
        Query(&'a QueryValue, bool),
        Stored(&'a Value, bool),
        Text(&'static str),
    }
    let mut pending = vec![Task::Query(value, false)];
    let mut result = String::new();
    while let Some(task) = pending.pop() {
        state.check(1)?;
        match task {
            Task::Text(text) => result.push_str(text),
            Task::Query(QueryValue::Nil, quoted) => {
                if quoted {
                    result.push_str("nil");
                }
            }
            Task::Query(QueryValue::Scalar(value), quoted) => {
                pending.push(Task::Stored(value, quoted));
            }
            Task::Query(QueryValue::Char(value), quoted) => {
                if quoted {
                    result.push('\\');
                }
                result.push(*value);
            }
            Task::Query(QueryValue::Tagged(tag, value), _) => {
                result.push('#');
                result.push_str(&tag.qualified_name());
                result.push(' ');
                pending.push(Task::Query(value, true));
            }
            Task::Query(QueryValue::Set(values), _) => {
                result.push_str("#{");
                pending.push(Task::Text("}"));
                for (index, value) in values.iter().enumerate().rev() {
                    pending.push(Task::Query(value, true));
                    if index > 0 {
                        pending.push(Task::Text(" "));
                    }
                }
            }
            Task::Query(QueryValue::Collection(values) | QueryValue::Tuple(values), _) => {
                result.push('[');
                pending.push(Task::Text("]"));
                for (index, value) in values.iter().enumerate().rev() {
                    pending.push(Task::Query(value, true));
                    if index > 0 {
                        pending.push(Task::Text(" "));
                    }
                }
            }
            Task::Query(QueryValue::Map(entries), _) => {
                result.push('{');
                pending.push(Task::Text("}"));
                for (index, (key, value)) in entries.iter().enumerate().rev() {
                    pending.push(Task::Query(value, true));
                    pending.push(Task::Text(" "));
                    pending.push(Task::Query(key, true));
                    if index > 0 {
                        pending.push(Task::Text(", "));
                    }
                }
            }
            Task::Stored(Value::Tuple(values), _) => {
                result.push('[');
                pending.push(Task::Text("]"));
                for (index, value) in values.iter().enumerate().rev() {
                    pending.push(match value {
                        Some(value) => Task::Stored(value, true),
                        None => Task::Text("nil"),
                    });
                    if index > 0 {
                        pending.push(Task::Text(" "));
                    }
                }
            }
            Task::Stored(value, quoted) => {
                let text = match value {
                    Value::String(value) if quoted => format!("{value:?}"),
                    Value::String(value) | Value::Uri(value) => value.clone(),
                    Value::Keyword(value) => format!(":{}", value.qualified_name()),
                    Value::Symbol(value) => value.qualified_name(),
                    Value::Long(value) | Value::Instant(value) => value.to_string(),
                    Value::Ref(value) => value.to_string(),
                    Value::Bool(value) => value.to_string(),
                    Value::Double(value) => value.to_string(),
                    Value::Float(value) => value.to_string(),
                    Value::BigDec(value) => value.to_string(),
                    Value::BigInt(value) => value.to_string(),
                    Value::Uuid(value) => format!("{value:032x}"),
                    Value::Bytes(value) => format!("#bytes{value:?}"),
                    Value::Function(value) => format!("#function{value:02x?}"),
                    Value::Tuple(_) => unreachable!("tuples use iterative tasks"),
                };
                result.push_str(&text);
            }
        }
    }
    Ok(result)
}
