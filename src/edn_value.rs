//! Explicit conversion between EDN data and EDB's narrower value domains.
//!
//! Parsing EDN does not imply that a value can be stored as a datom. In
//! particular nil, characters, maps and sets are not stored scalar types.
//! Native-only value types use namespaced tags; these never execute code.

use crate::edn::EdnValue;
use crate::{
    Datom, Keyword, QueryResult, QueryValue, ReturnMapShape, ReturnMaps, SemanticError, Symbol,
    Value,
};
use std::collections::BTreeMap;

const MAX_DEPTH: usize = 64;
const MAX_NODES: usize = 1_000_000;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_WORK: usize = 64 * 1024 * 1024;

struct Budget {
    nodes: usize,
    bytes: usize,
    work: usize,
}
impl Budget {
    fn new() -> Self {
        Self {
            nodes: MAX_NODES,
            bytes: MAX_BYTES,
            work: MAX_WORK,
        }
    }
    fn node(&mut self, depth: usize) -> Result<(), SemanticError> {
        self.work(1)?;
        if depth > MAX_DEPTH || self.nodes == 0 {
            return Err(limit());
        }
        self.nodes -= 1;
        self.bytes(std::mem::size_of::<EdnValue>())
    }
    fn bytes(&mut self, bytes: usize) -> Result<(), SemanticError> {
        self.bytes = self.bytes.checked_sub(bytes).ok_or_else(limit)?;
        Ok(())
    }
    fn work(&mut self, amount: usize) -> Result<(), SemanticError> {
        self.work = self.work.checked_sub(amount).ok_or_else(limit)?;
        Ok(())
    }
}
fn limit() -> SemanticError {
    SemanticError::incorrect(
        "edn/conversion-limit",
        "value conversion exceeds its depth, node, work or accounted-byte limit",
    )
}
fn invalid(message: &str) -> SemanticError {
    SemanticError::incorrect("edn/value-type", message)
}
fn tag(name: &str, value: EdnValue) -> EdnValue {
    let symbol = match name.split_once('/') {
        Some((namespace, name)) => Symbol::new(namespace, name),
        None => Symbol::unqualified(name),
    };
    EdnValue::Tagged(symbol, Box::new(value))
}
fn tag_text(name: &str, value: String) -> EdnValue {
    tag(name, EdnValue::String(value))
}

/// Convert EDN to a stored value, without schema inference. Sequences denote
/// tuple values here; transaction adapters must distinguish many-values and
/// lookup references before calling this function.
pub fn edn_to_value(value: &EdnValue) -> Result<Value, SemanticError> {
    from_edn(value, &mut Budget::new(), 0)
}

fn from_edn(value: &EdnValue, budget: &mut Budget, depth: usize) -> Result<Value, SemanticError> {
    budget.node(depth)?;
    Ok(match value {
        EdnValue::Bool(v) => Value::Bool(*v),
        EdnValue::Long(v) => Value::Long(*v),
        EdnValue::Double(v) => Value::Double(*v),
        EdnValue::BigInt(v) => {
            budget.bytes(v.bits().div_ceil(8) as usize)?;
            Value::BigInt(v.clone())
        }
        EdnValue::BigDec(v) => {
            budget.bytes(v.as_bigint_and_scale().0.bits().div_ceil(8) as usize)?;
            Value::BigDec(v.clone())
        }
        EdnValue::String(v) => {
            budget.bytes(v.len())?;
            Value::String(v.clone())
        }
        EdnValue::Keyword(v) => {
            budget.bytes(v.name.len() + v.namespace.as_ref().map_or(0, String::len))?;
            Value::Keyword(v.clone())
        }
        EdnValue::Symbol(v) => {
            budget.bytes(v.name.len() + v.namespace.as_ref().map_or(0, String::len))?;
            Value::Symbol(v.clone())
        }
        EdnValue::Vector(values) | EdnValue::List(values) => {
            if !(2..=8).contains(&values.len()) {
                return Err(invalid("stored tuples require 2–8 slots"));
            }
            let mut slots = Vec::with_capacity(values.len());
            for value in values {
                if matches!(value, EdnValue::Nil) {
                    budget.node(depth + 1)?;
                    slots.push(None);
                } else {
                    slots.push(Some(from_edn(value, budget, depth + 1)?));
                }
            }
            Value::Tuple(slots)
        }
        EdnValue::Tagged(name, payload) => {
            budget.node(depth + 1)?;
            let text = match payload.as_ref() {
                EdnValue::String(s) => Some(s.as_str()),
                _ => None,
            };
            if let Some(text) = text {
                budget.bytes(text.len())?;
            }
            match (name.namespace.as_deref(), name.name.as_str()) {
                (None, "uuid") => Value::Uuid(parse_uuid(
                    text.ok_or_else(|| invalid("#uuid requires a string"))?,
                )?),
                (None, "inst") => Value::Instant(instant_millis(
                    text.ok_or_else(|| invalid("#inst requires a string"))?,
                )?),
                (Some("edb"), "instant-millis") => match payload.as_ref() {
                    EdnValue::Long(v) => Value::Instant(*v),
                    _ => {
                        return Err(invalid(
                            "#edb/instant-millis requires a signed 64-bit integer",
                        ));
                    }
                },
                (Some("edb"), "ref") => {
                    let reference = match payload.as_ref() {
                        EdnValue::Long(v) => u64::try_from(*v).ok(),
                        EdnValue::BigInt(v) => num_traits::ToPrimitive::to_u64(v),
                        _ => None,
                    }
                    .ok_or_else(|| {
                        invalid("#edb/ref requires a nonnegative supported entity ID")
                    })?;
                    if reference > crate::MAX_EID {
                        return Err(invalid("reference exceeds the supported entity ID range"));
                    }
                    Value::Ref(reference)
                }
                (Some("edb"), "uri") => {
                    let uri = text.ok_or_else(|| invalid("#edb/uri requires a string"))?;
                    if !crate::model::uri::validate(uri) {
                        return Err(invalid("#edb/uri has invalid syntax or escaping"));
                    }
                    Value::Uri(uri.to_owned())
                }
                (Some("edb"), "bytes") => {
                    Value::Bytes(decode_hex(text.ok_or_else(|| {
                        invalid("#edb/bytes requires a hexadecimal string")
                    })?)?)
                }
                (Some("edb"), "function") => {
                    let bytes = decode_hex(text.ok_or_else(|| {
                        invalid("#edb/function requires a hexadecimal content hash")
                    })?)?;
                    Value::Function(
                        bytes
                            .try_into()
                            .map_err(|_| invalid("function content hash must contain 32 bytes"))?,
                    )
                }
                (Some("edb"), "float") => {
                    let (v, nonzero) = match payload.as_ref() {
                        EdnValue::String(v) => (
                            v.parse::<f32>()
                                .map_err(|_| invalid("invalid #edb/float value"))?,
                            v.split(['e', 'E']).next().is_some_and(|mantissa| {
                                mantissa.bytes().any(|b| matches!(b, b'1'..=b'9'))
                            }),
                        ),
                        EdnValue::Double(v) => (*v as f32, *v != 0.0),
                        _ => {
                            return Err(invalid(
                                "#edb/float requires a floating-point literal or string",
                            ));
                        }
                    };
                    if !v.is_finite() {
                        return Err(invalid("nonfinite float requires #edb/float-bits"));
                    }
                    if v == 0.0 && nonzero {
                        return Err(invalid("nonzero #edb/float underflows the stored range"));
                    }
                    Value::Float(v)
                }
                (Some("edb"), "float-bits") => {
                    let bytes: [u8; 4] = decode_hex(
                        text.ok_or_else(|| invalid("float bits require a hexadecimal string"))?,
                    )?
                    .try_into()
                    .map_err(|_| invalid("float bits require four bytes"))?;
                    Value::Float(f32::from_bits(u32::from_be_bytes(bytes)))
                }
                (Some("edb"), "double-bits") => {
                    let bytes: [u8; 8] = decode_hex(
                        text.ok_or_else(|| invalid("double bits require a hexadecimal string"))?,
                    )?
                    .try_into()
                    .map_err(|_| invalid("double bits require eight bytes"))?;
                    Value::Double(f64::from_bits(u64::from_be_bytes(bytes)))
                }
                _ => {
                    return Err(invalid(
                        "tag is not a registered EDB stored-value representation",
                    ));
                }
            }
        }
        EdnValue::Nil | EdnValue::Char(_) | EdnValue::Map(_) | EdnValue::Set(_) => {
            return Err(invalid(
                "nil, characters, maps and sets are not stored scalar values",
            ));
        }
    })
}

/// Convert a stored value into readable EDN, preserving its native type. Ref,
/// Float, Bytes, URI, Function and out-of-RFC3339-range instants use edb/*
/// tags. Nonfinite binary floats retain their exact bits, including NaN payloads.
pub fn value_to_edn(value: &Value) -> Result<EdnValue, SemanticError> {
    to_edn(value, &mut Budget::new(), 0)
}

/// Encode transaction report data with one cumulative conversion budget for
/// all datoms, containers and temporary IDs. This describes a report, not its
/// commit status: callers must retain confirmed receipt metadata independently
/// if the full report exceeds admission or cannot be observed.
pub fn transaction_report_to_edn(
    before: u64,
    after: u64,
    datoms: &[Datom],
    tempids: &BTreeMap<String, u64>,
) -> Result<EdnValue, SemanticError> {
    report_to_edn(before, after, datoms, tempids, &mut Budget::new())
}

fn report_to_edn(
    before: u64,
    after: u64,
    datoms: &[Datom],
    tempids: &BTreeMap<String, u64>,
    budget: &mut Budget,
) -> Result<EdnValue, SemanticError> {
    if datoms.len().saturating_add(tempids.len()) > 100_000 {
        return Err(limit());
    }
    let integer = |value: u64| {
        i64::try_from(value)
            .map(EdnValue::Long)
            .unwrap_or_else(|_| EdnValue::BigInt(value.into()))
    };
    // Root, four key/value pairs and the short, fixed metadata payloads.
    for _ in 0..9 {
        budget.node(0)?;
    }
    budget.bytes(128)?;
    let mut data = Vec::new();
    for datom in datoms {
        // Row container and five scalar fields; value conversion shares budget.
        for _ in 0..6 {
            budget.node(2)?;
        }
        let value = to_edn(&datom.value, budget, 3)?;
        data.push(EdnValue::Vector(vec![
            integer(datom.entity),
            integer(u64::from(datom.attribute)),
            value,
            integer(datom.tx),
            integer(datom.source),
            EdnValue::Bool(datom.added),
        ]));
    }
    let mut ids = Vec::new();
    for (id, entity) in tempids {
        budget.node(2)?;
        budget.node(2)?;
        budget.bytes(id.len())?;
        ids.push((EdnValue::String(id.clone()), integer(*entity)));
    }
    Ok(EdnValue::Map(vec![
        (edn_keyword("edb", "db-before-t"), integer(before)),
        (edn_keyword("edb", "db-after-t"), integer(after)),
        (edn_keyword("edb", "tx-data"), EdnValue::Vector(data)),
        (edn_keyword("edb", "tempids"), EdnValue::Map(ids)),
    ]))
}

fn to_edn(value: &Value, budget: &mut Budget, depth: usize) -> Result<EdnValue, SemanticError> {
    budget.node(depth)?;
    Ok(match value {
        Value::Bool(v) => EdnValue::Bool(*v),
        Value::Long(v) => EdnValue::Long(*v),
        Value::BigInt(v) => {
            budget.bytes(v.bits().div_ceil(8) as usize)?;
            EdnValue::BigInt(v.clone())
        }
        Value::BigDec(v) => {
            budget.bytes(v.as_bigint_and_scale().0.bits().div_ceil(8) as usize)?;
            EdnValue::BigDec(v.clone())
        }
        Value::Double(v) if v.is_finite() => EdnValue::Double(*v),
        Value::Double(v) => tag_text("edb/double-bits", hex(&v.to_bits().to_be_bytes())),
        Value::Float(v) if v.is_finite() => tag_text("edb/float", v.to_string()),
        Value::Float(v) => tag_text("edb/float-bits", hex(&v.to_bits().to_be_bytes())),
        Value::String(v) => {
            budget.bytes(v.len())?;
            EdnValue::String(v.clone())
        }
        Value::Keyword(v) => {
            budget.bytes(v.name.len() + v.namespace.as_ref().map_or(0, String::len))?;
            EdnValue::Keyword(v.clone())
        }
        Value::Symbol(v) => {
            budget.bytes(v.name.len() + v.namespace.as_ref().map_or(0, String::len))?;
            EdnValue::Symbol(v.clone())
        }
        Value::Ref(v) => tag(
            "edb/ref",
            if let Ok(v) = i64::try_from(*v) {
                EdnValue::Long(v)
            } else {
                EdnValue::BigInt((*v).into())
            },
        ),
        Value::Instant(v) => match crate::edn::instant::format_millis(*v) {
            Some(v) => tag_text("inst", v),
            None => tag("edb/instant-millis", EdnValue::Long(*v)),
        },
        Value::Uuid(v) => {
            let bytes = hex(&v.to_be_bytes());
            tag_text(
                "uuid",
                format!(
                    "{}-{}-{}-{}-{}",
                    &bytes[..8],
                    &bytes[8..12],
                    &bytes[12..16],
                    &bytes[16..20],
                    &bytes[20..]
                ),
            )
        }
        Value::Uri(v) => {
            budget.bytes(v.len())?;
            tag_text("edb/uri", v.clone())
        }
        Value::Bytes(v) => {
            budget.bytes(v.len().checked_mul(2).ok_or_else(limit)?)?;
            tag_text("edb/bytes", hex(v))
        }
        Value::Function(v) => tag_text("edb/function", hex(v)),
        Value::Tuple(v) => {
            let mut values = Vec::new();
            for value in v {
                values.push(match value {
                    None => {
                        budget.node(depth + 1)?;
                        EdnValue::Nil
                    }
                    Some(value) => to_edn(value, budget, depth + 1)?,
                });
            }
            EdnValue::Vector(values)
        }
    })
}

/// Convert general EDN data into the query-only value domain. Characters,
/// sets and inert custom tags remain query data, never stored datom types.
pub fn edn_to_query_value(value: &EdnValue) -> Result<QueryValue, SemanticError> {
    to_query(value, &mut Budget::new(), 0)
}
fn to_query(
    value: &EdnValue,
    budget: &mut Budget,
    depth: usize,
) -> Result<QueryValue, SemanticError> {
    budget.node(depth)?;
    Ok(match value {
        EdnValue::Nil => QueryValue::Nil,
        EdnValue::Char(value) => QueryValue::Char(*value),
        EdnValue::List(v) | EdnValue::Vector(v) => QueryValue::Tuple(
            v.iter()
                .map(|v| to_query(v, budget, depth + 1))
                .collect::<Result<_, _>>()?,
        ),
        EdnValue::Set(v) => QueryValue::Set(
            v.iter()
                .map(|v| to_query(v, budget, depth + 1))
                .collect::<Result<_, _>>()?,
        ),
        EdnValue::Map(v) => {
            let mut entries = v
                .iter()
                .map(|(k, v)| {
                    Ok((
                        to_query(k, budget, depth + 1)?,
                        to_query(v, budget, depth + 1)?,
                    ))
                })
                .collect::<Result<Vec<_>, SemanticError>>()?;
            // Conversion has admitted the owned keys, but ordering arbitrary
            // map/set keys also traverses them and creates canonical scratch.
            // Admit that scratch before each checked comparison, including the
            // final duplicate-key pass. A failed comparison is latched just as
            // in set conversion; no further key work is done after failure.
            budget.bytes(
                entries
                    .len()
                    .saturating_mul(std::mem::size_of::<(QueryValue, QueryValue)>()),
            )?;
            let mut compare = |left: &QueryValue, right: &QueryValue| {
                let left_size = left.measure_with(&mut |work| budget.work(work))?;
                let right_size = right.measure_with(&mut |work| budget.work(work))?;
                budget.bytes(
                    left_size
                        .canonical_bytes
                        .saturating_add(right_size.canonical_bytes),
                )?;
                left.compare_with(right, &mut |work| budget.work(work))
            };
            let mut failure = None;
            entries.sort_by(|(left, _), (right, _)| {
                if failure.is_some() {
                    return std::cmp::Ordering::Equal;
                }
                compare(left, right).unwrap_or_else(|error| {
                    failure = Some(error);
                    std::cmp::Ordering::Equal
                })
            });
            if let Some(error) = failure {
                return Err(error);
            }
            for pair in entries.windows(2) {
                if compare(&pair[0].0, &pair[1].0)?.is_eq() {
                    return Err(SemanticError::incorrect(
                        "edn/query-key-collision",
                        "distinct EDN keys collapse under native query-value equality",
                    ));
                }
            }
            QueryValue::Map(entries)
        }
        EdnValue::Tagged(name, payload) if !stored_tag(name) => {
            budget.bytes(name.name.len() + name.namespace.as_ref().map_or(0, String::len))?;
            QueryValue::Tagged(
                name.clone(),
                Box::new(to_query(payload, budget, depth + 1)?),
            )
        }
        _ => QueryValue::Scalar(from_edn(value, budget, depth)?),
    })
}

// Known native/scalar tags retain their precise existing interpretation and
// validation. An invalid known tag must not become an inert custom tag instead.
fn stored_tag(name: &Symbol) -> bool {
    matches!(
        (name.namespace.as_deref(), name.name.as_str()),
        (None, "uuid" | "inst")
            | (
                Some("edb"),
                "instant-millis"
                    | "ref"
                    | "uri"
                    | "bytes"
                    | "function"
                    | "float"
                    | "float-bits"
                    | "double-bits"
            )
    )
}

/// Encode a query/pull value without flattening nested maps or nil values.
pub fn query_value_to_edn(value: &QueryValue) -> Result<EdnValue, SemanticError> {
    from_query(value, &mut Budget::new(), 0)
}
fn from_query(
    value: &QueryValue,
    budget: &mut Budget,
    depth: usize,
) -> Result<EdnValue, SemanticError> {
    budget.node(depth)?;
    Ok(match value {
        QueryValue::Nil => EdnValue::Nil,
        QueryValue::Char(value) => EdnValue::Char(*value),
        QueryValue::Scalar(v) => to_edn(v, budget, depth)?,
        QueryValue::Collection(v) | QueryValue::Tuple(v) => EdnValue::Vector(
            v.iter()
                .map(|v| from_query(v, budget, depth + 1))
                .collect::<Result<_, _>>()?,
        ),
        QueryValue::Map(v) => EdnValue::Map(
            v.iter()
                .map(|(k, v)| {
                    Ok((
                        from_query(k, budget, depth + 1)?,
                        from_query(v, budget, depth + 1)?,
                    ))
                })
                .collect::<Result<_, SemanticError>>()?,
        ),
        QueryValue::Set(values) => {
            let members = canonical_query_set_members(values, budget)?;
            EdnValue::Set(
                members
                    .into_iter()
                    .map(|value| from_query(value, budget, depth + 1))
                    .collect::<Result<_, _>>()?,
            )
        }
        QueryValue::Tagged(name, payload) => {
            budget.bytes(name.name.len() + name.namespace.as_ref().map_or(0, String::len))?;
            EdnValue::Tagged(
                name.clone(),
                Box::new(from_query(payload, budget, depth + 1)?),
            )
        }
    })
}

// Borrowed members are admitted before sorting: even members discarded by
// deduplication consume input traversal, comparison and scratch allowances.
// Canonical comparison uses heap frames, not host-stack recursion.
fn canonical_query_set_members<'a>(
    values: &'a [QueryValue],
    budget: &mut Budget,
) -> Result<Vec<&'a QueryValue>, SemanticError> {
    budget.bytes(
        values
            .len()
            .saturating_mul(2 * std::mem::size_of::<(&QueryValue, usize)>()),
    )?;
    let mut members = Vec::with_capacity(values.len());
    for value in values {
        let size = value.measure_with(&mut |work| {
            budget.work(work)?;
            budget.nodes = budget.nodes.checked_sub(work).ok_or_else(limit)?;
            Ok(())
        })?;
        budget.bytes(size.retained_bytes.saturating_add(size.canonical_bytes))?;
        value.validate_with(&mut |work| budget.work(work))?;
        members.push((value, size.canonical_bytes));
    }
    let mut compare = |left: &(&QueryValue, usize), right: &(&QueryValue, usize)| {
        budget.bytes(left.1.saturating_add(right.1))?;
        left.0.compare_with(right.0, &mut |work| budget.work(work))
    };
    let mut failure = None;
    members.sort_by(|left, right| {
        if failure.is_some() {
            return std::cmp::Ordering::Equal;
        }
        compare(left, right).unwrap_or_else(|error| {
            failure = Some(error);
            std::cmp::Ordering::Equal
        })
    });
    if let Some(error) = failure.take() {
        return Err(error);
    }
    members.dedup_by(|left, right| {
        if failure.is_some() {
            return false;
        }
        compare(left, right)
            .map(|order| order.is_eq())
            .unwrap_or_else(|error| {
                failure = Some(error);
                false
            })
    });
    if let Some(error) = failure {
        return Err(error);
    }
    budget.bytes(
        members
            .len()
            .saturating_mul(std::mem::size_of::<&QueryValue>()),
    )?;
    Ok(members.into_iter().map(|(value, _)| value).collect())
}

/// Relation results are EDN sets of tuples; collection/tuple find results are
/// vectors, and missing scalar/tuple results are nil.
pub fn query_result_to_edn(result: &QueryResult) -> Result<EdnValue, SemanticError> {
    let budget = &mut Budget::new();
    Ok(match result {
        QueryResult::Relation(rows) => EdnValue::Set(
            rows.iter()
                .map(|row| {
                    budget.node(0)?;
                    Ok(EdnValue::Vector(
                        row.iter()
                            .map(|v| from_query(v, budget, 1))
                            .collect::<Result<_, _>>()?,
                    ))
                })
                .collect::<Result<_, SemanticError>>()?,
        ),
        QueryResult::Collection(v) | QueryResult::Tuple(Some(v)) => EdnValue::Vector(
            v.iter()
                .map(|v| from_query(v, budget, 1))
                .collect::<Result<_, _>>()?,
        ),
        QueryResult::Scalar(Some(v)) => from_query(v, budget, 0)?,
        QueryResult::Scalar(None) | QueryResult::Tuple(None) => EdnValue::Nil,
    })
}

/// Preserve keyword/string/symbol return-map keys and the original find shape.
pub fn return_maps_to_edn(result: &ReturnMaps) -> Result<EdnValue, SemanticError> {
    let budget = &mut Budget::new();
    let rows = result
        .iter()
        .map(|row| {
            budget.node(0)?;
            Ok(EdnValue::Map(
                row.iter()
                    .map(|(key, value)| {
                        Ok((to_edn(key, budget, 1)?, from_query(value, budget, 1)?))
                    })
                    .collect::<Result<_, SemanticError>>()?,
            ))
        })
        .collect::<Result<Vec<_>, SemanticError>>()?;
    Ok(match result.shape() {
        ReturnMapShape::Relation => EdnValue::Set(rows),
        ReturnMapShape::Tuple => rows.into_iter().next().unwrap_or(EdnValue::Nil),
    })
}

fn parse_uuid(text: &str) -> Result<u128, SemanticError> {
    if text.len() != 36 || [8, 13, 18, 23].iter().any(|&i| text.as_bytes()[i] != b'-') {
        return Err(invalid(
            "UUID requires canonical 8-4-4-4-12 hexadecimal groups",
        ));
    }
    let compact: String = text.chars().filter(|&c| c != '-').collect();
    let bytes: [u8; 16] = decode_hex(&compact)?
        .try_into()
        .map_err(|_| invalid("invalid UUID"))?;
    Ok(u128::from_be_bytes(bytes))
}
fn instant_millis(text: &str) -> Result<i64, SemanticError> {
    let parts = crate::edn::instant::parse_instant(text)?;
    if parts.leap_second || parts.fraction.bytes().skip(3).any(|b| b != b'0') {
        return Err(SemanticError::incorrect(
            "edn/instant-precision",
            "stored instants have millisecond precision and cannot represent leap seconds",
        ));
    }
    let mut millis = 0i64;
    let mut digits = parts.fraction.bytes();
    for _ in 0..3 {
        millis = millis * 10 + i64::from(digits.next().unwrap_or(b'0') - b'0');
    }
    parts
        .seconds
        .checked_mul(1000)
        .and_then(|v| v.checked_add(millis))
        .ok_or_else(|| invalid("instant exceeds stored range"))
}
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        output.push(char::from(DIGITS[(byte >> 4) as usize]));
        output.push(char::from(DIGITS[(byte & 15) as usize]));
    }
    output
}
fn decode_hex(text: &str) -> Result<Vec<u8>, SemanticError> {
    if !text.len().is_multiple_of(2) {
        return Err(invalid("hexadecimal data must contain complete bytes"));
    }
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let high = char::from(pair[0])
                .to_digit(16)
                .ok_or_else(|| invalid("invalid hexadecimal data"))?;
            let low = char::from(pair[1])
                .to_digit(16)
                .ok_or_else(|| invalid("invalid hexadecimal data"))?;
            Ok((high * 16 + low) as u8)
        })
        .collect()
}

/// Convenience constructor for metadata/result maps used by text applications.
pub fn edn_keyword(namespace: &str, name: &str) -> EdnValue {
    EdnValue::Keyword(Keyword::new(namespace, name))
}
