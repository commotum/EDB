//! EDN is data: selector compilation never resolves or executes host code.
use crate::edn::{EdnValue, read_edn};
use crate::edn_value::edn_to_query_value;
use crate::{
    AttributeName, ErrorCategory, PullAttribute, PullLimit, PullNested, PullPattern, PullTransform,
    QueryValue, SemanticError, Value,
};
use std::collections::{BTreeMap, BTreeSet};

/// Explicit admission for semantic conversion, including programmatic EDN.
#[derive(Clone, Copy, Debug)]
pub struct EdnAdapterLimits {
    pub max_depth: usize,
    pub max_nodes: usize,
    pub max_bytes: usize,
}

impl Default for EdnAdapterLimits {
    fn default() -> Self {
        Self {
            max_depth: 128,
            max_nodes: 1_000_000,
            max_bytes: 32 * 1024 * 1024,
        }
    }
}

/// Explicit native Pull callbacks. No namespace lookup or dynamic evaluation.
#[derive(Clone, Default)]
pub struct EdnPullTransforms(BTreeMap<String, PullTransform>);

impl EdnPullTransforms {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn register(&mut self, name: impl Into<String>, transform: PullTransform) {
        self.0.insert(name.into(), transform);
    }

    fn resolve(&self, name: &str) -> Result<PullTransform, SemanticError> {
        if let Some(transform) = self.0.get(name) {
            return Ok(transform.clone());
        }
        Ok(match name.strip_prefix("clojure.core/").unwrap_or(name) {
            "str" => PullTransform::String,
            "keyword" => PullTransform::Keyword,
            "symbol" => PullTransform::Symbol,
            "name" => PullTransform::Name,
            "namespace" => PullTransform::Namespace,
            "clojure.edn/read-string" => PullTransform::new("clojure.edn/read-string", |value| {
                let QueryValue::Scalar(Value::String(text)) = value else {
                    return Err(form_error(
                        "edn/pull-transform-input",
                        "read-string requires a string",
                        "pull/:xform",
                    ));
                };
                edn_to_query_value(&read_edn(text)?)
            }),
            _ => {
                return Err(form_error(
                    "edn/pull-transform",
                    "Pull transform is not registered",
                    "pull/:xform",
                )
                .detail("transform", name));
            }
        })
    }
}

/// Compile selector data without choosing a database. Underscore-leading names
/// retain schema-resolved direction: an exact installed attribute wins over
/// interpreting the spelling as reverse navigation.
pub fn parse_pull_edn(text: &str) -> Result<PullPattern, SemanticError> {
    parse_pull_edn_with_transforms(text, &EdnPullTransforms::default())
}

/// Preserve a read-side ID/ident/lookup reference until the supplied immutable
/// database resolves it. In particular, a keyword is not resolved while reading.
pub fn entity_identifier_from_edn(
    value: &EdnValue,
) -> Result<crate::EntityIdentifier, SemanticError> {
    if let Some([attribute, value]) = sequential(value) {
        let attribute = match attribute {
            EdnValue::Keyword(keyword) => AttributeName::Ident(keyword.clone()),
            EdnValue::Long(id) if *id >= 0 => {
                AttributeName::Id(u32::try_from(*id).map_err(|_| {
                    form_error(
                        "edn/entity-identifier",
                        "lookup attribute exceeds native range",
                        "entity",
                    )
                })?)
            }
            _ => {
                return Err(form_error(
                    "edn/entity-identifier",
                    "lookup attribute must be an ID or keyword",
                    "entity",
                ));
            }
        };
        return Ok(crate::EntityIdentifier::Lookup {
            attribute,
            value: crate::edn_value::edn_to_value(value)?,
        });
    }
    match value {
        EdnValue::Keyword(keyword) => Ok(crate::EntityIdentifier::Ident(keyword.clone())),
        _ => match crate::edn_value::edn_to_value(value)? {
            Value::Ref(id) => Ok(crate::EntityIdentifier::Id(id)),
            Value::Long(id) if id >= 0 => Ok(crate::EntityIdentifier::Id(id as u64)),
            _ => Err(form_error(
                "edn/entity-identifier",
                "entity must be an ID, ident or lookup reference",
                "entity",
            )),
        },
    }
}

pub fn parse_pull_edn_with_transforms(
    text: &str,
    transforms: &EdnPullTransforms,
) -> Result<PullPattern, SemanticError> {
    pull_pattern_from_edn(&read_edn(text)?, transforms)
}

pub fn pull_pattern_from_edn(
    value: &EdnValue,
    transforms: &EdnPullTransforms,
) -> Result<PullPattern, SemanticError> {
    pull_pattern_from_edn_with_limits(value, transforms, &EdnAdapterLimits::default())
}

pub fn pull_pattern_from_edn_with_limits(
    value: &EdnValue,
    transforms: &EdnPullTransforms,
    limits: &EdnAdapterLimits,
) -> Result<PullPattern, SemanticError> {
    admit(value, limits)?;
    if let EdnValue::String(text) = value {
        let parsed = read_edn(text)?;
        admit(&parsed, limits)?;
        return pattern(&parsed, transforms, "pull");
    }
    pattern(value, transforms, "pull")
}

fn pattern(
    value: &EdnValue,
    transforms: &EdnPullTransforms,
    path: &str,
) -> Result<PullPattern, SemanticError> {
    let values = sequential(value).ok_or_else(|| {
        form_error(
            "edn/pull-pattern",
            "Pull pattern must be a list or vector",
            path,
        )
    })?;
    let mut result = PullPattern::default();
    let mut directions = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        let path = format!("{path}/{index}");
        if text_name(value).as_deref() == Some("*") {
            result.wildcard = true;
            continue;
        }
        if let EdnValue::Map(entries) = value {
            for (key, nested) in entries {
                let mut attribute = attribute(key, transforms, &path)?;
                attribute.nested = Some(match nested {
                    EdnValue::Long(depth) if *depth >= 0 => {
                        PullNested::Recursion(Some(usize::try_from(*depth).map_err(|_| {
                            form_error("edn/pull-recursion", "recursion limit is too large", &path)
                        })?))
                    }
                    value if symbol_name(value).as_deref() == Some("...") => {
                        PullNested::Recursion(None)
                    }
                    EdnValue::Vector(_) | EdnValue::List(_) => {
                        PullNested::Pattern(Box::new(pattern(nested, transforms, &path)?))
                    }
                    _ => {
                        return Err(form_error(
                            "edn/pull-recursion",
                            "nested selector must be a pattern, nonnegative recursion depth, or ...",
                            &path,
                        ));
                    }
                });
                push_attribute(&mut result, &mut directions, attribute, &path)?;
            }
        } else {
            push_attribute(
                &mut result,
                &mut directions,
                attribute(value, transforms, &path)?,
                &path,
            )?;
        }
    }
    Ok(result)
}

fn push_attribute(
    pattern: &mut PullPattern,
    directions: &mut BTreeSet<crate::PullDirection>,
    attribute: PullAttribute,
    path: &str,
) -> Result<(), SemanticError> {
    if !directions.insert(attribute.direction.clone()) {
        return Err(form_error(
            "edn/pull-duplicate-attribute",
            "multiple explicit specifications for one attribute direction",
            path,
        ));
    }
    pattern.attributes.push(attribute);
    Ok(())
}

fn attribute(
    value: &EdnValue,
    transforms: &EdnPullTransforms,
    path: &str,
) -> Result<PullAttribute, SemanticError> {
    if let Some(parts) = sequential(value) {
        if parts.len() == 3 && matches!(text_name(&parts[0]).as_deref(), Some("limit" | "default"))
        {
            let mut attr = attribute_name(&parts[1], path)?;
            if text_name(&parts[0]).as_deref() == Some("limit") {
                attr.limit = limit(&parts[2], path)?;
            } else {
                attr.default = Some(edn_to_query_value(&parts[2])?);
            }
            return Ok(attr);
        }
        let Some((name, options)) = parts.split_first() else {
            return Err(form_error(
                "edn/pull-attribute",
                "empty attribute expression",
                path,
            ));
        };
        if options.is_empty() || options.len() % 2 != 0 {
            return Err(form_error(
                "edn/pull-option",
                "attribute options must be keyword/value pairs",
                path,
            ));
        }
        let mut attr = attribute_name(name, path)?;
        let mut seen = BTreeSet::new();
        for option in options.chunks_exact(2) {
            let EdnValue::Keyword(key) = &option[0] else {
                return Err(form_error(
                    "edn/pull-option",
                    "attribute option must be a keyword",
                    path,
                ));
            };
            if key.namespace.is_some() || !seen.insert(key.name.as_str()) {
                return Err(form_error(
                    "edn/pull-option",
                    "unknown or duplicate attribute option",
                    path,
                ));
            }
            match key.name.as_str() {
                "as" => attr.alias = Some(edn_to_query_value(&option[1])?),
                "default" => attr.default = Some(edn_to_query_value(&option[1])?),
                "limit" => attr.limit = limit(&option[1], path)?,
                "xform" => {
                    let name = symbol_name(&option[1]).ok_or_else(|| {
                        form_error(
                            "edn/pull-transform",
                            "xform requires a symbol naming a native transform",
                            path,
                        )
                    })?;
                    attr.transform = Some(transforms.resolve(&name)?);
                }
                _ => {
                    return Err(form_error(
                        "edn/pull-option",
                        "unknown attribute option",
                        path,
                    ));
                }
            }
        }
        return Ok(attr);
    }
    attribute_name(value, path)
}

fn attribute_name(value: &EdnValue, path: &str) -> Result<PullAttribute, SemanticError> {
    match value {
        EdnValue::Keyword(keyword) => {
            let mut attribute = PullAttribute::forward(AttributeName::Ident(keyword.clone()));
            if keyword.name.starts_with('_') {
                // Schema precedence is decided on the supplied immutable value,
                // not while parsing database-independent selector data.
                attribute.direction = crate::PullDirection::SchemaResolved(keyword.clone());
            }
            Ok(attribute)
        }
        EdnValue::Long(id) if *id >= 0 => Ok(PullAttribute::forward(AttributeName::Id(
            u32::try_from(*id).map_err(|_| {
                form_error(
                    "edn/pull-attribute",
                    "numeric attribute ID exceeds native range",
                    path,
                )
            })?,
        ))),
        _ => Err(form_error(
            "edn/pull-attribute",
            "attribute must be a keyword or native numeric ID",
            path,
        )),
    }
}

fn limit(value: &EdnValue, path: &str) -> Result<PullLimit, SemanticError> {
    match value {
        EdnValue::Nil => Ok(PullLimit::Unlimited),
        EdnValue::Long(n) if *n >= 0 => {
            Ok(PullLimit::Limit(usize::try_from(*n).map_err(|_| {
                form_error("edn/pull-limit", "limit exceeds native range", path)
            })?))
        }
        _ => Err(form_error(
            "edn/pull-limit",
            "limit must be nil or a nonnegative native integer",
            path,
        )),
    }
}

pub(crate) fn sequential(value: &EdnValue) -> Option<&[EdnValue]> {
    match value {
        EdnValue::Vector(items) | EdnValue::List(items) => Some(items),
        _ => None,
    }
}

pub(crate) fn symbol_name(value: &EdnValue) -> Option<String> {
    if let EdnValue::Symbol(symbol) = value {
        Some(symbol.qualified_name())
    } else {
        None
    }
}

fn text_name(value: &EdnValue) -> Option<String> {
    if let EdnValue::String(text) = value {
        Some(text.clone())
    } else {
        symbol_name(value)
    }
}

pub(crate) fn form_error(code: &'static str, message: &'static str, path: &str) -> SemanticError {
    SemanticError::incorrect(code, message).detail("path", path)
}

pub(crate) fn admit(value: &EdnValue, limits: &EdnAdapterLimits) -> Result<(), SemanticError> {
    admit_many(std::iter::once(value), limits)
}

pub(crate) fn admit_many<'a>(
    values: impl Iterator<Item = &'a EdnValue>,
    limits: &EdnAdapterLimits,
) -> Result<(), SemanticError> {
    admission_size(values, limits).map(|_| ())
}

pub(crate) fn admission_size<'a>(
    values: impl Iterator<Item = &'a EdnValue>,
    limits: &EdnAdapterLimits,
) -> Result<(usize, usize), SemanticError> {
    let mut nodes = 0usize;
    let mut bytes = 0usize;
    for value in values {
        let mut stack = vec![(value, 0usize)];
        while let Some((value, depth)) = stack.pop() {
            nodes = nodes.saturating_add(1);
            let payload = match value {
                EdnValue::String(v) => v.len(),
                EdnValue::Keyword(v) => v
                    .namespace
                    .as_ref()
                    .map_or(0, String::len)
                    .saturating_add(v.name.len()),
                EdnValue::Symbol(v) | EdnValue::Tagged(v, _) => v
                    .namespace
                    .as_ref()
                    .map_or(0, String::len)
                    .saturating_add(v.name.len()),
                EdnValue::BigInt(v) => usize::try_from(v.bits().div_ceil(8)).unwrap_or(usize::MAX),
                EdnValue::BigDec(v) => {
                    usize::try_from(v.as_bigint_and_scale().0.bits().div_ceil(8))
                        .unwrap_or(usize::MAX)
                }
                _ => 0,
            };
            bytes = bytes
                .saturating_add(std::mem::size_of::<EdnValue>())
                .saturating_add(payload);
            // Match the shared reader/writer's native recursion safety ceiling.
            if depth > limits.max_depth.min(128)
                || nodes > limits.max_nodes
                || bytes > limits.max_bytes
            {
                return Err(SemanticError::new(
                    ErrorCategory::Busy,
                    "edn/adapter-limit",
                    "EDN semantic conversion exceeds its depth, node or byte admission limit",
                ));
            }
            let children = match value {
                EdnValue::Vector(v) | EdnValue::List(v) | EdnValue::Set(v) => v.len(),
                EdnValue::Map(v) => v.len().saturating_mul(2),
                EdnValue::Tagged(_, _) => 1,
                _ => 0,
            };
            if children
                > limits
                    .max_nodes
                    .saturating_sub(nodes)
                    .saturating_sub(stack.len())
            {
                return Err(SemanticError::new(
                    ErrorCategory::Busy,
                    "edn/adapter-limit",
                    "EDN semantic conversion exceeds its node admission limit",
                ));
            }
            match value {
                EdnValue::Vector(values) | EdnValue::List(values) | EdnValue::Set(values) => {
                    stack.extend(values.iter().map(|value| (value, depth + 1)))
                }
                EdnValue::Map(values) => {
                    for (key, value) in values {
                        stack.push((key, depth + 1));
                        stack.push((value, depth + 1));
                    }
                }
                EdnValue::Tagged(_, value) => stack.push((value, depth + 1)),
                _ => {}
            }
        }
    }
    Ok((nodes, bytes))
}
