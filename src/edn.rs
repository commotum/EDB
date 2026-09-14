//! Native Extensible Data Notation. Reading is data-only: symbols and lists
//! never execute. Unknown tags are preserved unless explicitly rejected.
//!
//! The writer preserves collection order, not a persisted canonical encoding.
//! Long, BigInt, Double and BigDec are distinct EDN precision domains. Lists
//! and vectors compare as sequences; maps/sets compare independently of order.
//! NaN/infinities and Clojure-only reader macros are not standard EDN here.

pub(crate) mod instant;
mod reader;

use crate::{ErrorCategory, Keyword, SemanticError, Symbol};
use bigdecimal::BigDecimal;
use num_bigint::BigInt;
use std::collections::BTreeMap;
use std::sync::Arc;

pub use reader::{EdnReader, read_edn, read_edn_with_options};

#[derive(Clone, Debug)]
pub enum EdnValue {
    Nil,
    Bool(bool),
    Char(char),
    String(String),
    Keyword(Keyword),
    Symbol(Symbol),
    Long(i64),
    BigInt(BigInt),
    Double(f64),
    BigDec(BigDecimal),
    List(Vec<EdnValue>),
    Vector(Vec<EdnValue>),
    Map(Vec<(EdnValue, EdnValue)>),
    Set(Vec<EdnValue>),
    Tagged(Symbol, Box<EdnValue>),
}

impl PartialEq for EdnValue {
    fn eq(&self, other: &Self) -> bool {
        use EdnValue::*;
        match (self, other) {
            (Nil, Nil) => true,
            (Bool(a), Bool(b)) => a == b,
            (Char(a), Char(b)) => a == b,
            (String(a), String(b)) => a == b,
            (Keyword(a), Keyword(b)) => a == b,
            (Symbol(a), Symbol(b)) => a == b,
            (Long(a), Long(b)) => a == b,
            (BigInt(a), BigInt(b)) => a == b,
            (Double(a), Double(b)) => a == b,
            (BigDec(a), BigDec(b)) => decimal_identity(a) == decimal_identity(b),
            (List(a) | Vector(a), List(b) | Vector(b)) => a == b,
            (Set(a), Set(b)) => a.len() == b.len() && a.iter().all(|x| b.contains(x)),
            (Map(a), Map(b)) => {
                a.len() == b.len()
                    && a.iter().all(|(k, v)| {
                        b.iter()
                            .any(|(other_k, other_v)| k == other_k && v == other_v)
                    })
            }
            (Tagged(a, av), Tagged(b, bv)) if a == b => {
                if a.namespace.is_none() {
                    match (a.name.as_str(), av.as_ref(), bv.as_ref()) {
                        ("uuid", String(a), String(b)) => return a.eq_ignore_ascii_case(b),
                        ("inst", String(a), String(b)) => {
                            if let (Ok(a), Ok(b)) =
                                (instant::parse_instant(a), instant::parse_instant(b))
                            {
                                return a.seconds == b.seconds
                                    && a.leap_second == b.leap_second
                                    && a.fraction.trim_end_matches('0')
                                        == b.fraction.trim_end_matches('0');
                            }
                        }
                        _ => {}
                    }
                }
                av == bv
            }
            _ => false,
        }
    }
}

/// Admission controls shared by text reading and printing. Work includes
/// scanning, structural-key copies and numeric conversion, not user handler
/// execution. Native handlers are trusted callbacks; their returned data is
/// validated and admitted again. Limits are cumulative across a reader stream.
#[derive(Clone, Copy, Debug)]
pub struct EdnLimits {
    pub max_input_bytes: usize,
    pub max_output_bytes: usize,
    pub max_token_bytes: usize,
    pub max_numeric_bytes: usize,
    pub max_depth: usize,
    pub max_nodes: usize,
    pub max_work: usize,
}

impl Default for EdnLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 16 * 1024 * 1024,
            max_output_bytes: 16 * 1024 * 1024,
            max_token_bytes: 16 * 1024 * 1024,
            max_numeric_bytes: 64 * 1024,
            max_depth: 64,
            max_nodes: 1_000_000,
            max_work: 64 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EdnUnknownTagPolicy {
    #[default]
    Preserve,
    Reject,
}

pub type EdnTagHandler = Arc<dyn Fn(EdnValue) -> Result<EdnValue, SemanticError> + Send + Sync>;

#[derive(Clone, Default)]
pub struct EdnReadOptions {
    pub limits: EdnLimits,
    pub unknown_tags: EdnUnknownTagPolicy,
    pub tag_handlers: BTreeMap<Symbol, EdnTagHandler>,
}

fn capacity() -> SemanticError {
    SemanticError::new(
        ErrorCategory::Busy,
        "edn/capacity",
        "EDN resource limit exceeded",
    )
}

struct Budget {
    limits: EdnLimits,
    nodes: usize,
    work: usize,
}

impl Budget {
    fn new(limits: EdnLimits) -> Self {
        Self {
            limits,
            nodes: 0,
            work: 0,
        }
    }
    fn charge(&mut self, amount: usize) -> Result<(), SemanticError> {
        self.work = self.work.checked_add(amount).ok_or_else(capacity)?;
        if self.work > self.limits.max_work {
            return Err(capacity());
        }
        Ok(())
    }
    fn depth(&self, depth: usize) -> Result<(), SemanticError> {
        // The public value model is recursive, so bounded parsing also bounds
        // destruction of partially read data on every error path.
        if depth > self.limits.max_depth.min(128) {
            Err(capacity())
        } else {
            Ok(())
        }
    }
    fn node(&mut self, depth: usize) -> Result<(), SemanticError> {
        self.depth(depth)?;
        self.nodes = self.nodes.checked_add(1).ok_or_else(capacity)?;
        if self.nodes > self.limits.max_nodes {
            return Err(capacity());
        }
        self.charge(1)
    }
    fn token(&mut self, bytes: usize) -> Result<(), SemanticError> {
        if bytes > self.limits.max_token_bytes {
            return Err(capacity());
        }
        self.charge(bytes)
    }
    fn numeric(&mut self, bytes: usize) -> Result<(), SemanticError> {
        if bytes > self.limits.max_numeric_bytes {
            return Err(capacity());
        }
        self.token(bytes)?;
        self.charge(bytes.saturating_mul(bytes / 32 + 1))
    }
}

fn valid_part(part: &str) -> bool {
    let mut chars = part.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let constituent = |c: char| c.is_alphanumeric() || ".*+!-_?$%&=<>".contains(c);
    !first.is_numeric()
        && constituent(first)
        && !(matches!(first, '-' | '+' | '.') && chars.clone().next().is_some_and(char::is_numeric))
        && chars.all(|c| constituent(c) || matches!(c, ':' | '#'))
}

fn valid_name(namespace: Option<&str>, name: &str, keyword: bool) -> bool {
    if namespace.is_none() && name == "/" {
        return !keyword;
    }
    namespace.is_none_or(valid_part) && valid_part(name)
}

fn validate_symbol(symbol: &Symbol, tag: bool) -> Result<(), SemanticError> {
    let reserved =
        symbol.namespace.is_none() && matches!(symbol.name.as_str(), "nil" | "true" | "false");
    let first = symbol
        .namespace
        .as_deref()
        .unwrap_or(&symbol.name)
        .chars()
        .next();
    if !valid_name(symbol.namespace.as_deref(), &symbol.name, false)
        || (!tag && reserved)
        || (tag && !first.is_some_and(char::is_alphabetic))
    {
        return Err(SemanticError::incorrect(
            "edn/symbol",
            "invalid EDN symbol or tag",
        ));
    }
    Ok(())
}

fn uuid_valid(text: &str) -> bool {
    text.len() == 36
        && text.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

fn validate_builtin(tag: &Symbol, value: &EdnValue) -> Result<(), SemanticError> {
    if tag.namespace.is_some() {
        return Ok(());
    }
    match (tag.name.as_str(), value) {
        ("uuid", EdnValue::String(text)) if uuid_valid(text) => Ok(()),
        ("uuid", _) => Err(SemanticError::incorrect(
            "edn/uuid",
            "expected a canonical UUID string",
        )),
        ("inst", EdnValue::String(text)) => instant::parse_instant(text).map(|_| ()),
        ("inst", _) => Err(SemanticError::incorrect(
            "edn/instant",
            "expected an RFC 3339 string",
        )),
        _ => Ok(()),
    }
}

fn decimal_identity(value: &BigDecimal) -> (String, i128) {
    let (coefficient, scale) = value.as_bigint_and_scale();
    let digits = coefficient.to_str_radix(10);
    normalize_decimal(digits, scale)
}

fn normalize_decimal(digits: String, scale: i64) -> (String, i128) {
    if digits == "0" {
        return ("0".into(), 0);
    }
    let trimmed = digits.trim_end_matches('0');
    let exponent = -i128::from(scale) + (digits.len() - trimmed.len()) as i128;
    (trimmed.to_owned(), exponent)
}

fn integer_text(value: &BigInt, budget: &mut Budget) -> Result<String, SemanticError> {
    // Reject oversized coefficients before conversion. The conservative bit
    // precheck allows at most a constant-factor overshoot of the token bound;
    // exact decimal-token admission follows immediately after conversion.
    let token_limit = budget
        .limits
        .max_token_bytes
        .min(budget.limits.max_numeric_bytes);
    if u128::from(value.bits()) > (token_limit as u128).saturating_mul(4) {
        return Err(capacity());
    }
    let bound = usize::try_from(value.bits() / 3 + 2).map_err(|_| capacity())?;
    budget.charge(bound.saturating_mul(bound / 32 + 1))?;
    let text = value.to_str_radix(10);
    if text.len() > budget.limits.max_numeric_bytes {
        return Err(capacity());
    }
    budget.token(text.len())?;
    Ok(text)
}

fn append_key(key: &mut Vec<u8>, bytes: &[u8], budget: &mut Budget) -> Result<(), SemanticError> {
    budget.charge(bytes.len().saturating_add(8))?;
    key.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    key.extend_from_slice(bytes);
    Ok(())
}

fn leaf_key(kind: u8, bytes: &[u8], budget: &mut Budget) -> Result<Vec<u8>, SemanticError> {
    let mut key = vec![kind];
    append_key(&mut key, bytes, budget)?;
    Ok(key)
}

fn compare_keys(
    a: &[u8],
    b: &[u8],
    budget: &mut Budget,
) -> Result<std::cmp::Ordering, SemanticError> {
    for (a, b) in a.iter().zip(b) {
        budget.charge(1)?;
        let order = a.cmp(b);
        if order != std::cmp::Ordering::Equal {
            return Ok(order);
        }
    }
    budget.charge(1)?;
    Ok(a.len().cmp(&b.len()))
}

// Fallible heapsort admits comparisons before examining key bytes and has no
// unmetered comparison callback or auxiliary collection-sized allocation.
fn sort_keys<T>(values: &mut [(Vec<u8>, T)], budget: &mut Budget) -> Result<(), SemanticError> {
    fn sift<T>(
        values: &mut [(Vec<u8>, T)],
        mut root: usize,
        end: usize,
        budget: &mut Budget,
    ) -> Result<(), SemanticError> {
        while root < end / 2 {
            let mut child = root * 2 + 1;
            if child + 1 < end
                && compare_keys(&values[child].0, &values[child + 1].0, budget)?.is_lt()
            {
                child += 1;
            }
            if !compare_keys(&values[root].0, &values[child].0, budget)?.is_lt() {
                break;
            }
            values.swap(root, child);
            root = child;
        }
        Ok(())
    }
    for root in (0..values.len() / 2).rev() {
        sift(values, root, values.len(), budget)?;
    }
    for end in (1..values.len()).rev() {
        values.swap(0, end);
        sift(values, 0, end, budget)?;
    }
    Ok(())
}

/// Collision-free, length-delimited structural keys. This is an ephemeral
/// duplicate-check representation, NOT a public or durable encoding.
fn identity(
    value: &EdnValue,
    budget: &mut Budget,
    depth: usize,
    count_nodes: bool,
    discarded: bool,
) -> Result<Vec<u8>, SemanticError> {
    if count_nodes {
        budget.node(depth)?;
    } else {
        budget.depth(depth)?;
        budget.charge(1)?;
    }
    let mut key = Vec::new();
    match value {
        EdnValue::Nil => {
            budget.token(3)?;
            key = leaf_key(0, &[], budget)?;
        }
        EdnValue::Bool(v) => {
            budget.token(if *v { 4 } else { 5 })?;
            key = leaf_key(1, &[u8::from(*v)], budget)?;
        }
        EdnValue::Char(v) => {
            budget.token(v.len_utf8())?;
            key = leaf_key(2, &u32::from(*v).to_be_bytes(), budget)?;
        }
        EdnValue::String(v) => {
            budget.token(v.len())?;
            key = leaf_key(3, v.as_bytes(), budget)?;
        }
        EdnValue::Keyword(v) => {
            budget.token(
                v.namespace
                    .as_ref()
                    .map_or(0, |ns| ns.len() + 1)
                    .saturating_add(v.name.len()),
            )?;
            if !valid_name(v.namespace.as_deref(), &v.name, true) {
                return Err(SemanticError::incorrect(
                    "edn/keyword",
                    "invalid EDN keyword",
                ));
            }
            key.push(4);
            append_key(&mut key, v.qualified_name().as_bytes(), budget)?;
        }
        EdnValue::Symbol(v) => {
            budget.token(
                v.namespace
                    .as_ref()
                    .map_or(0, |ns| ns.len() + 1)
                    .saturating_add(v.name.len()),
            )?;
            validate_symbol(v, false)?;
            key.push(5);
            append_key(&mut key, v.qualified_name().as_bytes(), budget)?;
        }
        EdnValue::Long(v) => {
            budget.numeric(v.to_string().len())?;
            key = leaf_key(6, &v.to_be_bytes(), budget)?;
        }
        EdnValue::BigInt(v) => {
            let digits = integer_text(v, budget)?;
            key.push(7);
            append_key(&mut key, digits.as_bytes(), budget)?;
        }
        EdnValue::Double(v) => {
            if !v.is_finite() {
                return Err(SemanticError::incorrect(
                    "edn/non-finite",
                    "non-finite doubles have no standard EDN representation",
                ));
            }
            budget.numeric(v.to_string().len())?;
            key = leaf_key(
                8,
                &if *v == 0.0 { 0_u64 } else { v.to_bits() }.to_be_bytes(),
                budget,
            )?;
        }
        EdnValue::BigDec(v) => {
            let (coefficient, scale) = v.as_bigint_and_scale();
            let digits = integer_text(&coefficient, budget)?;
            let (digits, exponent) = normalize_decimal(digits, scale);
            key.push(9);
            append_key(&mut key, digits.as_bytes(), budget)?;
            append_key(&mut key, &exponent.to_be_bytes(), budget)?;
        }
        EdnValue::List(values) | EdnValue::Vector(values) | EdnValue::Set(values) => {
            let is_set = matches!(value, EdnValue::Set(_));
            key.push(if is_set { 11 } else { 10 });
            let mut children = Vec::new();
            for value in values {
                children.push((
                    identity(value, budget, depth + 1, count_nodes, discarded)?,
                    (),
                ));
            }
            if is_set {
                sort_keys(&mut children, budget)?;
                for pair in children.windows(2) {
                    if compare_keys(&pair[0].0, &pair[1].0, budget)?.is_eq() {
                        return Err(SemanticError::incorrect(
                            "edn/duplicate-element",
                            "EDN set contains an equal element twice",
                        ));
                    }
                }
            }
            for (child, ()) in children {
                append_key(&mut key, &child, budget)?;
            }
        }
        EdnValue::Map(entries) => {
            key.push(12);
            let mut children = Vec::new();
            for (k, v) in entries {
                let k = identity(k, budget, depth + 1, count_nodes, discarded)?;
                let v = identity(v, budget, depth + 1, count_nodes, discarded)?;
                children.push((k, v));
            }
            sort_keys(&mut children, budget)?;
            for pair in children.windows(2) {
                if compare_keys(&pair[0].0, &pair[1].0, budget)?.is_eq() {
                    return Err(SemanticError::incorrect(
                        "edn/duplicate-key",
                        "EDN map contains an equal key twice",
                    ));
                }
            }
            for (k, v) in children {
                append_key(&mut key, &k, budget)?;
                append_key(&mut key, &v, budget)?;
            }
        }
        EdnValue::Tagged(tag, value) => {
            budget.token(
                tag.namespace
                    .as_ref()
                    .map_or(0, |ns| ns.len() + 1)
                    .saturating_add(tag.name.len()),
            )?;
            validate_symbol(tag, true)?;
            let child = identity(value, budget, depth + 1, count_nodes, discarded)?;
            if !discarded {
                validate_builtin(tag, value)?;
            }
            key.push(13);
            append_key(&mut key, tag.qualified_name().as_bytes(), budget)?;
            match (tag.namespace.as_deref(), tag.name.as_str(), value.as_ref()) {
                (None, "uuid", EdnValue::String(text)) if uuid_valid(text) => {
                    budget.token(text.len())?;
                    append_key(&mut key, text.to_ascii_lowercase().as_bytes(), budget)?;
                }
                (None, "inst", EdnValue::String(text)) if !discarded => {
                    budget.token(text.len())?;
                    let parsed = instant::parse_instant(text)?;
                    append_key(&mut key, &parsed.seconds.to_be_bytes(), budget)?;
                    append_key(&mut key, &[u8::from(parsed.leap_second)], budget)?;
                    append_key(
                        &mut key,
                        parsed.fraction.trim_end_matches('0').as_bytes(),
                        budget,
                    )?;
                }
                _ => {
                    append_key(&mut key, &child, budget)?;
                }
            }
        }
    }
    Ok(key)
}

pub fn write_edn(value: &EdnValue) -> Result<String, SemanticError> {
    write_edn_with_limits(value, EdnLimits::default())
}

pub fn write_edn_with_limits(value: &EdnValue, limits: EdnLimits) -> Result<String, SemanticError> {
    let mut budget = Budget::new(limits);
    // Validate before rendering: hand-built maps/sets and tag-handler output
    // receive the same semantic and depth checks as parsed data.
    identity(value, &mut budget, 0, true, false)?;
    let mut output = String::new();
    render(value, &mut output, &mut budget)?;
    Ok(output)
}

fn emit(output: &mut String, text: &str, budget: &mut Budget) -> Result<(), SemanticError> {
    if output
        .len()
        .checked_add(text.len())
        .is_none_or(|bytes| bytes > budget.limits.max_output_bytes)
    {
        return Err(capacity());
    }
    budget.charge(text.len())?;
    output.push_str(text);
    Ok(())
}

fn render(value: &EdnValue, output: &mut String, budget: &mut Budget) -> Result<(), SemanticError> {
    match value {
        EdnValue::Nil => emit(output, "nil", budget),
        EdnValue::Bool(v) => emit(output, if *v { "true" } else { "false" }, budget),
        EdnValue::Char(v) => {
            emit(output, "\\", budget)?;
            let text = match v {
                '\n' => "newline".into(),
                '\r' => "return".into(),
                ' ' => "space".into(),
                '\t' => "tab".into(),
                c if c.is_control() || c.is_whitespace() => format!("u{:04X}", u32::from(*c)),
                c => c.to_string(),
            };
            emit(output, &text, budget)
        }
        EdnValue::String(text) => {
            emit(output, "\"", budget)?;
            for c in text.chars() {
                match c {
                    '\n' => emit(output, "\\n", budget)?,
                    '\r' => emit(output, "\\r", budget)?,
                    '\t' => emit(output, "\\t", budget)?,
                    '"' => emit(output, "\\\"", budget)?,
                    '\\' => emit(output, "\\\\", budget)?,
                    c if c.is_control() => {
                        emit(output, &format!("\\u{:04X}", u32::from(c)), budget)?
                    }
                    c => emit(output, c.encode_utf8(&mut [0; 4]), budget)?,
                }
            }
            emit(output, "\"", budget)
        }
        EdnValue::Keyword(v) => {
            emit(output, ":", budget)?;
            emit(output, &v.qualified_name(), budget)
        }
        EdnValue::Symbol(v) => emit(output, &v.qualified_name(), budget),
        EdnValue::Long(v) => emit(output, &v.to_string(), budget),
        EdnValue::BigInt(v) => {
            let text = integer_text(v, budget)?;
            emit(output, &text, budget)?;
            emit(output, "N", budget)
        }
        EdnValue::Double(v) => {
            let text = v.to_string();
            emit(output, &text, budget)?;
            if !text.contains(['.', 'e', 'E']) {
                emit(output, ".0", budget)?;
            }
            Ok(())
        }
        EdnValue::BigDec(v) => {
            let (coefficient, scale) = v.as_bigint_and_scale();
            let text = integer_text(&coefficient, budget)?;
            emit(output, &text, budget)?;
            emit(output, "e", budget)?;
            emit(output, &(-i128::from(scale)).to_string(), budget)?;
            emit(output, "M", budget)
        }
        EdnValue::List(values) | EdnValue::Vector(values) | EdnValue::Set(values) => {
            let (start, end) = match value {
                EdnValue::List(_) => ("(", ")"),
                EdnValue::Vector(_) => ("[", "]"),
                _ => ("#{", "}"),
            };
            emit(output, start, budget)?;
            for (i, value) in values.iter().enumerate() {
                if i > 0 {
                    emit(output, " ", budget)?;
                }
                render(value, output, budget)?;
            }
            emit(output, end, budget)
        }
        EdnValue::Map(entries) => {
            emit(output, "{", budget)?;
            for (i, (key, value)) in entries.iter().enumerate() {
                if i > 0 {
                    emit(output, " ", budget)?;
                }
                render(key, output, budget)?;
                emit(output, " ", budget)?;
                render(value, output, budget)?;
            }
            emit(output, "}", budget)
        }
        EdnValue::Tagged(tag, value) => {
            emit(output, "#", budget)?;
            emit(output, &tag.qualified_name(), budget)?;
            emit(output, " ", budget)?;
            render(value, output, budget)
        }
    }
}

fn drop_iterative(value: EdnValue) {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            EdnValue::List(values) | EdnValue::Vector(values) | EdnValue::Set(values) => {
                pending.extend(values)
            }
            EdnValue::Map(entries) => {
                for (key, value) in entries {
                    pending.push(key);
                    pending.push(value);
                }
            }
            EdnValue::Tagged(_, value) => pending.push(*value),
            _ => {}
        }
    }
}
