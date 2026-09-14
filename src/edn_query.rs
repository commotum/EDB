//! EDN query templates compile to the existing native query engine.
use crate::edn::{EdnValue, read_edn};
use crate::edn_pull::{
    EdnAdapterLimits, EdnPullTransforms, admission_size, admit, form_error,
    pull_pattern_from_edn_with_limits, sequential, symbol_name,
};
use crate::edn_value::{edn_to_query_value, edn_to_value, query_result_to_edn, return_maps_to_edn};
use crate::{
    Aggregate, AggregateArg, AggregateCall, Binding, Clause, DataPattern, FindElement, FindSpec,
    Function, InputSpec, Keyword, Predicate, PreparedQuery, Query, QueryControl, QueryDataSource,
    QueryExtensions, QueryInput, QueryOutcome, QueryResult, QuerySourceValue, QueryValue,
    RelationPattern, Rule, SemanticError, Term, Value, Variable,
};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// Ordered EDN `:in` parameters, including parameters absent from native ASTs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EdnQueryInput {
    Source(String),
    Binding(InputSpec),
    Rules,
    Pattern(String),
}

#[derive(Clone, Debug)]
pub enum EdnQueryArgument {
    Source(QuerySourceValue),
    Data(EdnValue),
}

/// A read-only template. Rule and pattern arguments are compiled during bind.
#[derive(Clone, Debug)]
pub struct EdnQuery {
    pub inputs: Vec<EdnQueryInput>,
    template: EdnValue,
}

#[derive(Clone, Debug)]
pub struct BoundEdnQuery {
    prepared: PreparedQuery,
    pub sources: Vec<QueryDataSource>,
    pub inputs: Vec<QueryInput>,
    pub return_keys: Option<Vec<Value>>,
}

pub fn parse_query_edn(text: &str) -> Result<EdnQuery, SemanticError> {
    query_from_edn(&read_edn(text)?)
}

pub fn query_from_edn(value: &EdnValue) -> Result<EdnQuery, SemanticError> {
    admit(value, &EdnAdapterLimits::default())?;
    let sections = sections(value)?;
    let inputs = input_specs(&sections)?;
    Ok(EdnQuery {
        inputs,
        template: value.clone(),
    })
}

impl EdnQuery {
    pub fn bind(&self, arguments: &[EdnQueryArgument]) -> Result<BoundEdnQuery, SemanticError> {
        self.bind_with_transforms(arguments, &EdnPullTransforms::default())
    }

    pub fn bind_with_transforms(
        &self,
        arguments: &[EdnQueryArgument],
        transforms: &EdnPullTransforms,
    ) -> Result<BoundEdnQuery, SemanticError> {
        self.bind_with_limits(arguments, transforms, &EdnAdapterLimits::default())
    }

    pub fn bind_with_limits(
        &self,
        arguments: &[EdnQueryArgument],
        transforms: &EdnPullTransforms,
        limits: &EdnAdapterLimits,
    ) -> Result<BoundEdnQuery, SemanticError> {
        let (input_nodes, input_bytes) = admission_size(
            std::iter::once(&self.template).chain(arguments.iter().filter_map(|argument| {
                match argument {
                    EdnQueryArgument::Data(value) => Some(value),
                    _ => None,
                }
            })),
            limits,
        )?;
        if arguments.len() != self.inputs.len() {
            return Err(form_error(
                "edn/query-input-arity",
                "argument count must match every :in parameter",
                "query/:in",
            ));
        }
        let mut sources = Vec::new();
        let mut inputs = Vec::new();
        let mut native_specs = Vec::new();
        let mut patterns = BTreeMap::new();
        let mut rules = None;
        for (index, (spec, argument)) in self.inputs.iter().zip(arguments).enumerate() {
            let path = format!("query/:in/{index}");
            match (spec, argument) {
                (EdnQueryInput::Source(name), EdnQueryArgument::Source(value)) => {
                    sources.push(QueryDataSource {
                        name: name.clone(),
                        value: value.clone(),
                    })
                }
                (EdnQueryInput::Source(name), EdnQueryArgument::Data(value)) => {
                    sources.push(QueryDataSource {
                        name: name.clone(),
                        value: relation_source(value, &path)?,
                    })
                }
                (EdnQueryInput::Binding(spec), EdnQueryArgument::Data(value)) => {
                    inputs.push(input_value(spec, value, &path)?);
                    native_specs.push(spec.clone());
                }
                (EdnQueryInput::Pattern(name), EdnQueryArgument::Data(value)) => {
                    patterns.insert(name.clone(), value);
                }
                (EdnQueryInput::Rules, EdnQueryArgument::Data(value)) => rules = Some(value),
                _ => {
                    return Err(form_error(
                        "edn/query-input-kind",
                        "this :in parameter requires EDN data, not a database/log source",
                        &path,
                    ));
                }
            }
        }
        let mut compiler = Compiler::new(transforms, limits, &self.template, arguments);
        compiler.remaining.set(EdnAdapterLimits {
            max_nodes: limits.max_nodes - input_nodes,
            max_bytes: limits.max_bytes - input_bytes,
            ..*limits
        });
        let (mut query, return_keys) =
            compiler.query(&self.template, &patterns, rules, &BTreeMap::new())?;
        query.inputs = native_specs;
        // Preparation and execution remain the existing native mechanisms.
        let prepared = PreparedQuery::new(&query)?;
        Ok(BoundEdnQuery {
            prepared,
            sources,
            inputs,
            return_keys,
        })
    }
}

impl BoundEdnQuery {
    pub fn query(&self) -> &Query {
        self.prepared.query()
    }

    pub fn execute(
        &self,
        control: &QueryControl,
        extensions: Option<&QueryExtensions>,
    ) -> Result<QueryOutcome, SemanticError> {
        match extensions {
            Some(extensions) => self.prepared.execute_with_extensions(
                &self.sources,
                &self.inputs,
                control,
                extensions,
            ),
            None => self.prepared.execute(&self.sources, &self.inputs, control),
        }
    }

    /// Prepare the same bound query as a fallible sequence of result tuples.
    /// Relational work is eager; Pull and its registered transforms are lazy.
    /// Return-map keys remain available on this binding for per-row rendering.
    pub fn sequence(
        &self,
        control: &QueryControl,
        extensions: Option<&QueryExtensions>,
    ) -> Result<crate::QuerySequence, SemanticError> {
        crate::QueryEngine::sequence_sources_with_extensions(
            self.query(),
            &self.sources,
            &self.inputs,
            control,
            extensions,
        )
    }

    pub fn result_to_edn(&self, result: &QueryResult) -> Result<EdnValue, SemanticError> {
        match &self.return_keys {
            Some(keys) => {
                let arity = match &self.query().find {
                    FindSpec::Relation(v) | FindSpec::Tuple(v) => v.len(),
                    _ => {
                        return Err(form_error(
                            "edn/query-return-map-shape",
                            "return maps require relation or tuple find",
                            "query/:find",
                        ));
                    }
                };
                return_maps_to_edn(
                    &result
                        .clone()
                        .into_return_maps_with_arity(keys.clone(), arity)?,
                )
            }
            None => query_result_to_edn(result),
        }
    }
}

type Sections<'a> = BTreeMap<&'a str, &'a [EdnValue]>;

fn sections<'a>(value: &'a EdnValue) -> Result<Sections<'a>, SemanticError> {
    let mut result = BTreeMap::new();
    let mut insert = |key: &'a EdnValue, values: &'a [EdnValue]| {
        let EdnValue::Keyword(key) = key else {
            return Err(form_error(
                "edn/query-section",
                "query section names must be keywords",
                "query",
            ));
        };
        if key.namespace.is_some()
            || !matches!(
                key.name.as_str(),
                "find" | "with" | "in" | "where" | "keys" | "strs" | "syms" | "rules"
            )
        {
            return Err(form_error(
                "edn/query-section",
                "unknown query section",
                "query",
            ));
        }
        if result.insert(key.name.as_str(), values).is_some() {
            return Err(form_error(
                "edn/query-section",
                "duplicate query section",
                "query",
            ));
        }
        Ok(())
    };
    if let EdnValue::Map(entries) = value {
        for (key, value) in entries {
            insert(
                key,
                sequential(value).ok_or_else(|| {
                    form_error(
                        "edn/query-section",
                        "map query sections must contain lists or vectors",
                        "query",
                    )
                })?,
            )?;
        }
    } else if let Some(values) = sequential(value) {
        let mut at = 0;
        while at < values.len() {
            let start = at;
            at += 1;
            while at < values.len() && !matches!(&values[at], EdnValue::Keyword(_)) {
                at += 1;
            }
            insert(&values[start], &values[start + 1..at])?;
        }
    } else {
        return Err(form_error(
            "edn/query-form",
            "query must be a list, vector or map",
            "query",
        ));
    }
    if result.get("find").is_none_or(|values| values.is_empty()) {
        return Err(form_error(
            "edn/query-find",
            "query requires a nonempty :find clause",
            "query/:find",
        ));
    }
    Ok(result)
}

fn input_specs(sections: &Sections<'_>) -> Result<Vec<EdnQueryInput>, SemanticError> {
    let Some(values) = sections.get("in") else {
        return Ok(vec![EdnQueryInput::Source("$".into())]);
    };
    let mut result = Vec::new();
    let mut special = BTreeSet::new();
    for value in *values {
        let name = symbol_name(value);
        let input = match name.as_deref() {
            Some(name) if name.starts_with('$') => EdnQueryInput::Source(name.into()),
            Some("%") => EdnQueryInput::Rules,
            Some(name) if plain(name) => EdnQueryInput::Pattern(name.into()),
            _ => EdnQueryInput::Binding(input_spec(value)?),
        };
        if !matches!(&input, EdnQueryInput::Binding(_)) && !special.insert(name.unwrap()) {
            return Err(form_error(
                "edn/query-input-duplicate",
                "duplicate source, rules or pattern parameter",
                "query/:in",
            ));
        }
        result.push(input);
    }
    Ok(result)
}

fn input_spec(value: &EdnValue) -> Result<InputSpec, SemanticError> {
    Ok(match binding(value)? {
        Binding::Scalar(v) => InputSpec::Scalar(v),
        Binding::Tuple(v) => InputSpec::Tuple(v),
        Binding::Collection(v) => InputSpec::Collection(v),
        Binding::Relation(v) => InputSpec::Relation(v),
    })
}

fn binding(value: &EdnValue) -> Result<Binding, SemanticError> {
    if symbol_name(value).is_some_and(|s| s.starts_with('?')) {
        return Ok(Binding::Scalar(variable(value)?));
    }
    let parts = sequential(value).ok_or_else(|| {
        form_error(
            "edn/query-binding",
            "binding must be a variable or sequential destructuring form",
            "query/binding",
        )
    })?;
    if let [value, ellipsis] = parts
        && symbol_name(ellipsis).as_deref() == Some("...")
    {
        return Ok(Binding::Collection(variable(value)?));
    }
    if let [tuple] = parts
        && let Some(tuple) = sequential(tuple)
    {
        return Ok(Binding::Relation(binding_variables(tuple)?));
    }
    Ok(Binding::Tuple(binding_variables(parts)?))
}

fn binding_variables(values: &[EdnValue]) -> Result<Vec<Option<Variable>>, SemanticError> {
    if values.is_empty() {
        return Err(form_error(
            "edn/query-binding",
            "destructuring binding cannot be empty",
            "query/binding",
        ));
    }
    values
        .iter()
        .map(|value| {
            if symbol_name(value).as_deref() == Some("_") {
                Ok(None)
            } else {
                variable(value).map(Some)
            }
        })
        .collect()
}

fn variable(value: &EdnValue) -> Result<Variable, SemanticError> {
    let name = symbol_name(value)
        .filter(|s| s.starts_with('?') && s.len() > 1)
        .ok_or_else(|| form_error("edn/query-variable", "expected a ?variable symbol", "query"))?;
    Variable::new(name)
}

fn plain(name: &str) -> bool {
    !name.is_empty() && !name.starts_with(['?', '$', '%']) && !matches!(name, "_" | "." | "...")
}

fn collection<'a>(value: &'a EdnValue, path: &str) -> Result<&'a [EdnValue], SemanticError> {
    Ok(match value {
        EdnValue::List(v) | EdnValue::Vector(v) | EdnValue::Set(v) => v,
        _ => {
            return Err(form_error(
                "edn/query-input-shape",
                "expected a list, vector or set",
                path,
            ));
        }
    })
}

fn positional<'a>(value: &'a EdnValue, path: &str) -> Result<&'a [EdnValue], SemanticError> {
    sequential(value).ok_or_else(|| {
        form_error(
            "edn/query-input-shape",
            "positional tuple rows require a list or vector, not an unordered set",
            path,
        )
    })
}

fn relation_source(value: &EdnValue, path: &str) -> Result<QuerySourceValue, SemanticError> {
    let rows = collection(value, path)?
        .iter()
        .map(|row| {
            positional(row, path)?
                .iter()
                .map(edn_to_query_value)
                .collect()
        })
        .collect::<Result<Vec<Vec<QueryValue>>, SemanticError>>()?;
    if rows
        .iter()
        .flatten()
        .all(|value| matches!(value, QueryValue::Scalar(_)))
    {
        // Retain the existing borrowed scalar-tuple fast path for ordinary data.
        Ok(QuerySourceValue::Tuples(Arc::new(
            rows.into_iter()
                .map(|row| {
                    row.into_iter()
                        .map(|value| value.into_scalar().expect("checked scalar"))
                        .collect()
                })
                .collect(),
        )))
    } else {
        Ok(QuerySourceValue::Relation(Arc::new(rows)))
    }
}

fn input_value(
    spec: &InputSpec,
    value: &EdnValue,
    path: &str,
) -> Result<QueryInput, SemanticError> {
    match spec {
        InputSpec::Scalar(_) => {}
        InputSpec::Tuple(_) => {
            positional(value, path)?;
        }
        InputSpec::Collection(_) => {
            collection(value, path)?;
        }
        InputSpec::Relation(_) => {
            for row in collection(value, path)? {
                positional(row, path)?;
            }
        }
    }
    // Nil and containers remain parameters, not embedded Ground clauses. A
    // prepared query can therefore be reused with different general values.
    Ok(QueryInput::General(edn_to_query_value(value)?))
}

struct Compiler<'a> {
    transforms: &'a EdnPullTransforms,
    limits: &'a EdnAdapterLimits,
    used_variables: BTreeSet<String>,
    fresh: usize,
    remaining: Cell<EdnAdapterLimits>,
    rule_names: BTreeSet<String>,
    rule_data: Option<Arc<EdnValue>>,
}

impl<'a> Compiler<'a> {
    fn new(
        transforms: &'a EdnPullTransforms,
        limits: &'a EdnAdapterLimits,
        template: &EdnValue,
        arguments: &[EdnQueryArgument],
    ) -> Self {
        let mut used_variables = BTreeSet::new();
        let mut pending = vec![template];
        pending.extend(arguments.iter().filter_map(|arg| {
            if let EdnQueryArgument::Data(v) = arg {
                Some(v)
            } else {
                None
            }
        }));
        while let Some(value) = pending.pop() {
            match value {
                EdnValue::Symbol(_) => {
                    if let Some(name) = symbol_name(value) {
                        used_variables.insert(name);
                    }
                }
                EdnValue::List(v) | EdnValue::Vector(v) | EdnValue::Set(v) => pending.extend(v),
                EdnValue::Map(v) => {
                    for (key, value) in v {
                        pending.push(key);
                        pending.push(value);
                    }
                }
                EdnValue::Tagged(_, value) => pending.push(value),
                _ => {}
            }
        }
        Self {
            transforms,
            limits,
            used_variables,
            fresh: 0,
            remaining: Cell::new(*limits),
            rule_names: BTreeSet::new(),
            rule_data: None,
        }
    }

    fn temporary(&mut self) -> Variable {
        loop {
            let name = format!("?__atomic_edn_{}", self.fresh);
            self.fresh += 1;
            if self.used_variables.insert(name.clone()) {
                return Variable::new(name).unwrap();
            }
        }
    }

    fn admit_expansion(&self, value: &EdnValue) -> Result<(), SemanticError> {
        let remaining = self.remaining.get();
        let (nodes, bytes) = admission_size(std::iter::once(value), &remaining)?;
        self.remaining.set(EdnAdapterLimits {
            max_nodes: remaining.max_nodes - nodes,
            max_bytes: remaining.max_bytes - bytes,
            ..remaining
        });
        Ok(())
    }

    fn query(
        &mut self,
        value: &EdnValue,
        patterns: &BTreeMap<String, &EdnValue>,
        rule_input: Option<&EdnValue>,
        source_names: &BTreeMap<String, String>,
    ) -> Result<(Query, Option<Vec<Value>>), SemanticError> {
        let sections = sections(value)?;
        // `%` is a lexical input, not an expression to evaluate. Keep a scoped,
        // bounded copy so nested static q can forward the same rules argument.
        if let Some(value) = rule_input {
            self.admit_expansion(value)?;
        }
        let previous_data =
            std::mem::replace(&mut self.rule_data, rule_input.map(|v| Arc::new(v.clone())));
        let rule_definitions = rule_input
            .and_then(sequential)
            .or_else(|| sections.get("rules").copied())
            .unwrap_or_default();
        let names = rule_definitions
            .iter()
            .filter_map(|definition| {
                sequential(definition)
                    .and_then(|d| d.first())
                    .and_then(sequential)
                    .and_then(|h| h.first())
                    .and_then(symbol_name)
            })
            .collect();
        let previous_names = std::mem::replace(&mut self.rule_names, names);
        let result = (|| {
            let mut query = Query::new(
                self.find(sections["find"], patterns, source_names)?,
                Vec::new(),
            );
            query.with = sections
                .get("with")
                .copied()
                .unwrap_or_default()
                .iter()
                .map(variable)
                .collect::<Result<_, _>>()?;
            query.inputs = input_specs(&sections)?
                .into_iter()
                .filter_map(|spec| {
                    if let EdnQueryInput::Binding(spec) = spec {
                        Some(spec)
                    } else {
                        None
                    }
                })
                .collect();
            query.clauses = self.clauses(
                sections.get("where").copied().unwrap_or_default(),
                "$",
                patterns,
                source_names,
            )?;
            if let Some(rules) = rule_input {
                query.rules = self.rules(rules, patterns, source_names)?;
            }
            if let Some(rules) = sections.get("rules") {
                // Native map/list query extension: static rule definitions can also
                // be carried in :rules instead of supplied through %.
                if rule_input.is_some() {
                    return Err(form_error(
                        "edn/query-rules",
                        "supply rules via :rules or %, not both",
                        "query/:rules",
                    ));
                }
                query.rules = self.rules_slice(rules, patterns, source_names)?;
            }
            let return_keys = self.return_keys(&sections, &query.find)?;
            Ok((query, return_keys))
        })();
        self.rule_names = previous_names;
        self.rule_data = previous_data;
        result
    }

    fn find(
        &self,
        values: &[EdnValue],
        patterns: &BTreeMap<String, &EdnValue>,
        sources: &BTreeMap<String, String>,
    ) -> Result<FindSpec, SemanticError> {
        if let [value, marker] = values
            && symbol_name(marker).as_deref() == Some(".")
        {
            return Ok(FindSpec::Scalar(
                self.find_element(value, patterns, sources)?,
            ));
        }
        if let [value] = values
            && let Some(tuple) = sequential(value)
            && (matches!(value, EdnValue::Vector(_))
                || tuple.first().is_some_and(|first| {
                    sequential(first).is_some()
                        || symbol_name(first).is_some_and(|v| v.starts_with('?'))
                }))
        {
            if let [value, marker] = tuple
                && symbol_name(marker).as_deref() == Some("...")
            {
                return Ok(FindSpec::Collection(
                    self.find_element(value, patterns, sources)?,
                ));
            }
            return Ok(FindSpec::Tuple(
                tuple
                    .iter()
                    .map(|v| self.find_element(v, patterns, sources))
                    .collect::<Result<_, _>>()?,
            ));
        }
        Ok(FindSpec::Relation(
            values
                .iter()
                .map(|v| self.find_element(v, patterns, sources))
                .collect::<Result<_, _>>()?,
        ))
    }

    fn find_element(
        &self,
        value: &EdnValue,
        patterns: &BTreeMap<String, &EdnValue>,
        sources: &BTreeMap<String, String>,
    ) -> Result<FindElement, SemanticError> {
        if symbol_name(value).is_some_and(|s| s.starts_with('?')) {
            return Ok(FindElement::Variable(variable(value)?));
        }
        let parts = sequential(value).ok_or_else(|| {
            form_error(
                "edn/query-find",
                "find element must be a variable, aggregate, or pull expression",
                "query/:find",
            )
        })?;
        let Some((name, args)) = parts.split_first() else {
            return Err(form_error(
                "edn/query-find",
                "empty find expression",
                "query/:find",
            ));
        };
        let name = symbol_name(name).ok_or_else(|| {
            form_error(
                "edn/query-find",
                "find function name must be a symbol",
                "query/:find",
            )
        })?;
        if name == "pull" {
            let (source, args) = source_prefix(args, "$", sources);
            let [entity, pattern] = args else {
                return Err(form_error(
                    "edn/query-pull",
                    "pull requires an entity variable and pattern",
                    "query/:find/pull",
                ));
            };
            let pattern = if let Some(name) = symbol_name(pattern) {
                *patterns.get(&name).ok_or_else(|| {
                    form_error(
                        "edn/query-pattern-input",
                        "Pull pattern parameter was not supplied",
                        "query/:find/pull",
                    )
                })?
            } else {
                pattern
            };
            let decoded;
            let pattern = if let EdnValue::String(text) = pattern {
                decoded = read_edn(text)?;
                &decoded
            } else {
                pattern
            };
            // Pattern parameters can be referenced many times; charge every
            // compiled copy before cloning it, not merely the input once.
            self.admit_expansion(pattern)?;
            return Ok(FindElement::Pull {
                source,
                variable: variable(entity)?,
                pattern: Box::new(pull_pattern_from_edn_with_limits(
                    pattern,
                    self.transforms,
                    self.limits,
                )?),
            });
        }
        let builtin_name = name.strip_prefix("clojure.core/").unwrap_or(&name);
        if !matches!(
            builtin_name,
            "count"
                | "count-distinct"
                | "min"
                | "max"
                | "sum"
                | "avg"
                | "distinct"
                | "median"
                | "variance"
                | "stddev"
                | "rand"
                | "sample"
        ) {
            if args.is_empty() {
                return Err(form_error(
                    "edn/query-aggregate",
                    "custom aggregate requires arguments",
                    "query/:find",
                ));
            }
            let args = args
                .iter()
                .map(|arg| match symbol_name(arg) {
                    Some(name) if name.starts_with('?') => {
                        variable(arg).map(AggregateArg::Variable)
                    }
                    Some(name) if name.starts_with('$') => Ok(AggregateArg::Source(
                        sources.get(&name).cloned().unwrap_or(name),
                    )),
                    _ => edn_to_query_value(arg).map(AggregateArg::Constant),
                })
                .collect::<Result<_, SemanticError>>()?;
            return Ok(FindElement::CustomAggregate(Box::new(AggregateCall::new(
                name, args,
            ))));
        }
        let (function, value) = aggregate(builtin_name, args)?;
        Ok(FindElement::Aggregate {
            function,
            variable: variable(value)?,
        })
    }

    fn return_keys(
        &self,
        sections: &Sections<'_>,
        find: &FindSpec,
    ) -> Result<Option<Vec<Value>>, SemanticError> {
        let present: Vec<_> = ["keys", "strs", "syms"]
            .into_iter()
            .filter_map(|key| sections.get(key).map(|values| (key, *values)))
            .collect();
        if present.len() > 1 {
            return Err(form_error(
                "edn/query-return-maps",
                "only one return-map clause is permitted",
                "query",
            ));
        }
        let Some((kind, values)) = present.first() else {
            return Ok(None);
        };
        let width = match find {
            FindSpec::Relation(values) | FindSpec::Tuple(values) => values.len(),
            _ => {
                return Err(form_error(
                    "edn/query-return-map-shape",
                    "return maps require relation or tuple find",
                    "query/:find",
                ));
            }
        };
        if width != values.len() {
            return Err(form_error(
                "edn/query-return-map-arity",
                "return-map key count must equal find width",
                "query",
            ));
        }
        let mut keys = Vec::new();
        for value in *values {
            let EdnValue::Symbol(symbol) = value else {
                return Err(form_error(
                    "edn/query-return-map-key",
                    "return-map keys must be plain symbols",
                    "query",
                ));
            };
            if !plain(&symbol.qualified_name()) {
                return Err(form_error(
                    "edn/query-return-map-key",
                    "return-map keys must be plain symbols",
                    "query",
                ));
            }
            let key = match *kind {
                "keys" => Value::Keyword(Keyword {
                    namespace: symbol.namespace.clone(),
                    name: symbol.name.clone(),
                }),
                "strs" => Value::String(symbol.qualified_name()),
                _ => Value::Symbol(symbol.clone()),
            };
            if keys.contains(&key) {
                return Err(form_error(
                    "edn/query-return-map-key",
                    "duplicate return-map key",
                    "query",
                ));
            }
            keys.push(key);
        }
        Ok(Some(keys))
    }

    fn clauses(
        &mut self,
        values: &[EdnValue],
        source: &str,
        patterns: &BTreeMap<String, &EdnValue>,
        sources: &BTreeMap<String, String>,
    ) -> Result<Vec<Clause>, SemanticError> {
        let mut clauses = Vec::new();
        for value in values {
            clauses.extend(self.clause(value, source, patterns, sources)?);
        }
        Ok(clauses)
    }

    fn clause(
        &mut self,
        value: &EdnValue,
        inherited: &str,
        patterns: &BTreeMap<String, &EdnValue>,
        sources: &BTreeMap<String, String>,
    ) -> Result<Vec<Clause>, SemanticError> {
        let parts = sequential(value).ok_or_else(|| {
            form_error(
                "edn/query-clause",
                "where clause must be a list or vector",
                "query/:where",
            )
        })?;
        let (source, parts) = source_prefix(parts, inherited, sources);
        let Some((first, rest)) = parts.split_first() else {
            return Err(form_error(
                "edn/query-clause",
                "empty where clause",
                "query/:where",
            ));
        };
        if let Some(name) = symbol_name(first) {
            match name.as_str() {
                "not" | "not-join" | "or" | "or-join" => {
                    let (join, body) = if name.ends_with("-join") {
                        let Some((join, body)) = rest.split_first() else {
                            return Err(form_error(
                                "edn/query-join",
                                "join clause requires variables and body",
                                "query/:where",
                            ));
                        };
                        let vars = sequential(join)
                            .ok_or_else(|| {
                                form_error(
                                    "edn/query-join",
                                    "join variables must be sequential",
                                    "query/:where",
                                )
                            })?
                            .iter()
                            .map(variable)
                            .collect::<Result<Vec<_>, _>>()?;
                        (Some(vars), body)
                    } else {
                        (None, rest)
                    };
                    if body.is_empty() {
                        return Err(form_error(
                            "edn/query-join",
                            "logical clause requires a body",
                            "query/:where",
                        ));
                    }
                    return Ok(vec![if name.starts_with("not") {
                        Clause::Not {
                            join,
                            clauses: self.clauses(body, &source, patterns, sources)?,
                        }
                    } else {
                        Clause::Or {
                            join,
                            branches: body
                                .iter()
                                .map(|v| self.clause(v, &source, patterns, sources))
                                .collect::<Result<_, _>>()?,
                        }
                    }]);
                }
                "and" => return self.clauses(rest, &source, patterns, sources),
                name if plain(name)
                    && (matches!(value, EdnValue::List(_)) || self.rule_names.contains(name)) =>
                {
                    return Ok(vec![Clause::Rule {
                        source,
                        name: name.into(),
                        args: rest.iter().map(term).collect::<Result<_, _>>()?,
                    }]);
                }
                _ => {}
            }
        }
        if let Some(call) = sequential(first)
            && call
                .first()
                .and_then(symbol_name)
                .is_some_and(|name| plain(&name))
        {
            return self.expression(call, rest, &source, patterns, sources);
        }
        if parts.len() > 5 {
            return Ok(vec![Clause::RelationPattern(Box::new(RelationPattern {
                source,
                terms: parts.iter().map(term).collect::<Result<_, _>>()?,
            }))]);
        }
        let component = |index| parts.get(index).map_or(Ok(Term::Blank), term);
        Ok(vec![Clause::Pattern(Box::new(DataPattern {
            source,
            entity: component(0)?,
            attribute: component(1)?,
            value: component(2)?,
            transaction: (parts.len() > 3).then(|| component(3)).transpose()?,
            added: (parts.len() > 4).then(|| component(4)).transpose()?,
        }))])
    }

    fn expression(
        &mut self,
        call: &[EdnValue],
        tail: &[EdnValue],
        inherited: &str,
        patterns: &BTreeMap<String, &EdnValue>,
        sources: &BTreeMap<String, String>,
    ) -> Result<Vec<Clause>, SemanticError> {
        let Some((name, args)) = call.split_first() else {
            return Err(form_error(
                "edn/query-expression",
                "empty function expression",
                "query/:where",
            ));
        };
        let name = symbol_name(name).ok_or_else(|| {
            form_error(
                "edn/query-expression",
                "function name must be an explicit native symbol",
                "query/:where",
            )
        })?;
        let (mut source, args) = source_prefix(args, inherited, sources);
        if tail.is_empty()
            && let Some(predicate) = predicate(&name)
        {
            return Ok(vec![Clause::Predicate {
                predicate,
                source,
                args: args.iter().map(term).collect::<Result<_, _>>()?,
            }]);
        }
        if tail.len() > 1 {
            return Err(form_error(
                "edn/query-binding",
                "function clause requires one output binding",
                "query/:where",
            ));
        }
        let (function, args) = if matches!(name.as_str(), "q" | "datomic.api/q") {
            let (function, args, nested_source) = self.subquery(args, patterns, sources)?;
            source = nested_source;
            (function, args)
        } else {
            (
                function(&name),
                args.iter().map(term).collect::<Result<_, _>>()?,
            )
        };
        if let Some(output) = tail.first() {
            return Ok(vec![Clause::Function {
                function,
                source,
                args,
                binding: binding(output)?,
            }]);
        }
        // Native extensions return relation rows; predicate position takes
        // their one scalar result and applies Clojure/Datalog truthiness.
        let result = self.temporary();
        Ok(vec![
            Clause::Function {
                function,
                source: source.clone(),
                args,
                binding: Binding::Scalar(result.clone()),
            },
            Clause::Predicate {
                predicate: Predicate::NotEq,
                source: source.clone(),
                args: vec![
                    Term::Variable(result.clone()),
                    Term::Constant(Value::Bool(false)),
                ],
            },
            Clause::Predicate {
                predicate: Predicate::NotEq,
                source,
                args: vec![Term::Variable(result), Term::Nil],
            },
        ])
    }

    fn subquery(
        &mut self,
        args: &[EdnValue],
        outer_patterns: &BTreeMap<String, &EdnValue>,
        outer_sources: &BTreeMap<String, String>,
    ) -> Result<(Function, Vec<Term>, String), SemanticError> {
        let Some((template, args)) = args.split_first() else {
            return Err(form_error(
                "edn/query-subquery",
                "q requires a literal query template",
                "query/:where/q",
            ));
        };
        let sections = sections(template)?;
        let specs = input_specs(&sections)?;
        if specs.len() != args.len() {
            return Err(form_error(
                "edn/query-subquery-arity",
                "q arguments must match nested :in",
                "query/:where/q",
            ));
        }
        let mut source_names = outer_sources.clone();
        let mut patterns = outer_patterns.clone();
        let mut rules = None;
        let enclosing_rules = self.rule_data.clone();
        let mut values = Vec::new();
        let mut invocation_source = None;
        for (spec, value) in specs.iter().zip(args) {
            match spec {
                EdnQueryInput::Source(name) => {
                    let supplied = symbol_name(value)
                        .filter(|v| v.starts_with('$'))
                        .ok_or_else(|| {
                            form_error(
                                "edn/query-subquery-source",
                                "nested source must name an enclosing immutable source",
                                "query/:where/q",
                            )
                        })?;
                    let supplied = outer_sources.get(&supplied).cloned().unwrap_or(supplied);
                    invocation_source.get_or_insert_with(|| supplied.clone());
                    source_names.insert(name.clone(), supplied);
                }
                EdnQueryInput::Binding(_) => values.push(term(value)?),
                EdnQueryInput::Rules => {
                    rules = Some(if symbol_name(value).as_deref() == Some("%") {
                        enclosing_rules.as_deref().ok_or_else(|| {
                            form_error(
                                "edn/query-subquery-rules",
                                "nested % requires an enclosing rules input",
                                "query/:where/q",
                            )
                        })?
                    } else {
                        value
                    });
                }
                EdnQueryInput::Pattern(name) => {
                    let value = symbol_name(value)
                        .and_then(|name| outer_patterns.get(&name).copied())
                        .unwrap_or(value);
                    patterns.insert(name.clone(), value);
                }
            }
        }
        let (query, return_keys) = self.query(template, &patterns, rules, &source_names)?;
        if return_keys.is_some() {
            return Err(form_error(
                "edn/query-subquery-return-maps",
                "native q returns its find shape; return-map adapters apply to top-level results",
                "query/:where/q",
            ));
        }
        // The engine validates sources actually consumed by the nested query.
        // Source-free computations need neither a dummy database nor a relation.
        let source = invocation_source.unwrap_or_else(|| "$".into());
        Ok((Function::Query(Box::new(query)), values, source))
    }

    fn rules(
        &mut self,
        value: &EdnValue,
        patterns: &BTreeMap<String, &EdnValue>,
        sources: &BTreeMap<String, String>,
    ) -> Result<Vec<Rule>, SemanticError> {
        self.rules_slice(
            sequential(value).ok_or_else(|| {
                form_error(
                    "edn/query-rules",
                    "rules must be a list or vector of definitions",
                    "query/%",
                )
            })?,
            patterns,
            sources,
        )
    }

    fn rules_slice(
        &mut self,
        values: &[EdnValue],
        patterns: &BTreeMap<String, &EdnValue>,
        sources: &BTreeMap<String, String>,
    ) -> Result<Vec<Rule>, SemanticError> {
        let mut result = Vec::new();
        for value in values {
            let definition = sequential(value).ok_or_else(|| {
                form_error(
                    "edn/query-rule",
                    "rule definition must be sequential",
                    "query/%",
                )
            })?;
            let Some((head, body)) = definition.split_first() else {
                return Err(form_error(
                    "edn/query-rule",
                    "rule definition requires head and body",
                    "query/%",
                ));
            };
            let head = sequential(head).ok_or_else(|| {
                form_error(
                    "edn/query-rule-head",
                    "rule head must be sequential",
                    "query/%",
                )
            })?;
            let Some((name, vars)) = head.split_first() else {
                return Err(form_error(
                    "edn/query-rule-head",
                    "empty rule head",
                    "query/%",
                ));
            };
            let name = symbol_name(name).filter(|s| plain(s)).ok_or_else(|| {
                form_error(
                    "edn/query-rule-name",
                    "rule name must be a plain symbol",
                    "query/%",
                )
            })?;
            let mut head = Vec::new();
            let mut required = BTreeSet::new();
            for (index, value) in vars.iter().enumerate() {
                if let Some(vars) = sequential(value) {
                    if index != 0 || vars.is_empty() {
                        return Err(form_error(
                            "edn/query-rule-required",
                            "required rule variables must be a nonempty first group",
                            "query/%",
                        ));
                    }
                    for value in vars {
                        required.insert(head.len());
                        head.push(variable(value)?);
                    }
                } else {
                    head.push(variable(value)?);
                }
            }
            if body.is_empty() {
                return Err(form_error(
                    "edn/query-rule",
                    "rule definition requires a body",
                    "query/%",
                ));
            }
            result.push(Rule {
                name,
                head,
                required,
                clauses: self.clauses(body, "$", patterns, sources)?,
            });
        }
        Ok(result)
    }
}

fn source_prefix<'a>(
    args: &'a [EdnValue],
    inherited: &str,
    sources: &BTreeMap<String, String>,
) -> (String, &'a [EdnValue]) {
    if let Some(name) = args
        .first()
        .and_then(symbol_name)
        .filter(|s| s.starts_with('$'))
    {
        return (sources.get(&name).cloned().unwrap_or(name), &args[1..]);
    }
    (
        sources
            .get(inherited)
            .cloned()
            .unwrap_or_else(|| inherited.into()),
        args,
    )
}

fn term(value: &EdnValue) -> Result<Term, SemanticError> {
    match symbol_name(value).as_deref() {
        Some("_") => Ok(Term::Blank),
        Some(name) if name.starts_with('?') => variable(value).map(Term::Variable),
        Some(name) if name.starts_with('$') => Err(form_error(
            "edn/query-source-position",
            "source symbols are allowed only in source argument positions",
            "query",
        )),
        _ if matches!(value, EdnValue::Nil) => Ok(Term::Nil),
        _ => match edn_to_value(value) {
            Ok(value) => Ok(Term::Constant(value)),
            Err(_) => edn_to_query_value(value).map(Term::QueryConstant),
        },
    }
}

fn predicate(name: &str) -> Option<Predicate> {
    Some(match name.strip_prefix("clojure.core/").unwrap_or(name) {
        "=" => Predicate::Eq,
        "not=" | "!=" => Predicate::NotEq,
        "<" => Predicate::Less,
        "<=" => Predicate::LessOrEqual,
        ">" => Predicate::Greater,
        ">=" => Predicate::GreaterOrEqual,
        "missing?" => Predicate::Missing,
        _ => return None,
    })
}

fn function(name: &str) -> Function {
    match name {
        "clojure.string/starts-with?" => return Function::StartsWith,
        "clojure.string/ends-with?" => return Function::EndsWith,
        "clojure.string/includes?" => return Function::Includes,
        "clojure.core/starts-with?" | "clojure.core/ends-with?" | "clojure.core/includes?" => {
            return Function::Extension(name.into());
        }
        _ => {}
    }
    let native_name = name.strip_prefix("clojure.core/").unwrap_or(name);
    match native_name {
        "ground" => Function::Ground,
        "+" => Function::Add,
        "-" => Function::Subtract,
        "*" => Function::Multiply,
        "/" => Function::Divide,
        "count" => Function::Count,
        "quot" => Function::Quot,
        "subs" => Function::Subs,
        "str" => Function::Str,
        "starts-with?" => Function::StartsWith,
        "ends-with?" => Function::EndsWith,
        "includes?" => Function::Includes,
        "tuple" => Function::Tuple,
        "untuple" => Function::Untuple,
        "get-else" => Function::GetElse,
        "get-some" => Function::GetSome,
        "fulltext" => Function::Fulltext,
        "tx-ids" => Function::TxIds,
        "tx-data" => Function::TxData,
        _ => Function::Extension(name.into()),
    }
}

fn aggregate<'a>(
    name: &str,
    args: &'a [EdnValue],
) -> Result<(Aggregate, &'a EdnValue), SemanticError> {
    if let [EdnValue::Long(size), value] = args {
        let size = usize::try_from(*size).map_err(|_| {
            form_error(
                "edn/query-aggregate",
                "aggregate count must be nonnegative",
                "query/:find",
            )
        })?;
        let function = match name {
            "min" => Aggregate::MinN(size),
            "max" => Aggregate::MaxN(size),
            "rand" => Aggregate::Rand(size),
            "sample" => Aggregate::Sample(size),
            _ => {
                return Err(form_error(
                    "edn/query-aggregate",
                    "unknown counted aggregate",
                    "query/:find",
                ));
            }
        };
        return Ok((function, value));
    }
    let [value] = args else {
        return Err(form_error(
            "edn/query-aggregate",
            "aggregate requires one variable, optionally preceded by a count",
            "query/:find",
        ));
    };
    let function = match name {
        "count" => Aggregate::Count,
        "count-distinct" => Aggregate::CountDistinct,
        "min" => Aggregate::Min,
        "max" => Aggregate::Max,
        "sum" => Aggregate::Sum,
        "avg" => Aggregate::Average,
        "distinct" => Aggregate::Distinct,
        "median" => Aggregate::Median,
        "variance" => Aggregate::Variance,
        "stddev" => Aggregate::StandardDeviation,
        _ => {
            return Err(form_error(
                "edn/query-aggregate",
                "unknown native aggregate",
                "query/:find",
            ));
        }
    };
    Ok((function, value))
}
