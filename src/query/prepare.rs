//! Bounded reuse of schema-independent query preparation. Bindings, databases,
//! callback registries and results never enter this cache.
use super::*;
use crate::collections::LruMap;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Debug)]
pub struct PreparedQuery {
    query: Arc<Query>,
    // Source-independent rule analysis is shared with cloned prepared values.
    // Populate only after successful validation under an invocation's controls.
    negation_validated: Arc<OnceLock<()>>,
}

impl PreparedQuery {
    pub fn new(query: &Query) -> Result<Self, SemanticError> {
        validate_query(query, query.inputs.len())?;
        Ok(Self {
            query: Arc::new(query.clone()),
            negation_validated: Arc::new(OnceLock::new()),
        })
    }

    pub fn query(&self) -> &Query {
        &self.query
    }

    pub(super) fn validate_negation(&self, state: &mut State<'_>) -> Result<(), SemanticError> {
        state.check(0)?;
        if self.negation_validated.get().is_none() {
            dependencies::validate_negation(self.query(), state)?;
            let _ = self.negation_validated.set(());
        }
        Ok(())
    }

    pub fn execute(
        &self,
        sources: &[QueryDataSource],
        inputs: &[QueryInput],
        control: &QueryControl,
    ) -> Result<QueryOutcome, SemanticError> {
        QueryEngine::execute_prepared(self, sources, inputs, control, None)
    }

    pub fn execute_with_extensions(
        &self,
        sources: &[QueryDataSource],
        inputs: &[QueryInput],
        control: &QueryControl,
        extensions: &QueryExtensions,
    ) -> Result<QueryOutcome, SemanticError> {
        QueryEngine::execute_prepared(self, sources, inputs, control, Some(extensions))
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PreparedQueryCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub entries: usize,
    /// Conservative retention weight, including AST and structural-key bytes;
    /// this is a cache policy unit, not measured allocator usage or RSS.
    pub retained_weight: usize,
}

struct Entry {
    query: PreparedQuery,
    weight: usize,
}

pub struct PreparedQueryCache {
    entries: LruMap<Vec<u8>, Entry>,
    max_entries: usize,
    max_weight: usize,
    stats: PreparedQueryCacheStats,
}

impl PreparedQueryCache {
    pub fn new(max_entries: usize, max_weight: usize) -> Self {
        Self {
            entries: LruMap::default(),
            max_entries,
            max_weight,
            stats: PreparedQueryCacheStats::default(),
        }
    }
    pub fn stats(&self) -> PreparedQueryCacheStats {
        self.stats
    }

    pub fn prepare(&mut self, query: &Query) -> Result<PreparedQuery, SemanticError> {
        let key = structural_key(query, self.max_weight / 8);
        self.prepare_key(query, key)
    }

    fn prepare_key(
        &mut self,
        query: &Query,
        key: Option<Vec<u8>>,
    ) -> Result<PreparedQuery, SemanticError> {
        if let Some(key) = &key
            && let Some(entry) = self.entries.get(key)
        {
            let prepared = entry.query.clone();
            self.stats.hits = self.stats.hits.saturating_add(1);
            return Ok(prepared);
        }
        self.stats.misses = self.stats.misses.saturating_add(1);
        let prepared = PreparedQuery::new(query)?;
        if let Some(key) = key {
            let weight = key
                .len()
                .saturating_mul(8)
                // ATOMIC-NOTE: Preserve the established policy weight when
                // moving the structural key from Entry to the shared LRU owner.
                // It is an admission unit, not a measurement of the map's RSS.
                .saturating_add(std::mem::size_of::<(Vec<u8>, Entry)>());
            if self.max_entries > 0 && weight <= self.max_weight {
                while self.entries.len() >= self.max_entries
                    || self.stats.retained_weight.saturating_add(weight) > self.max_weight
                {
                    let (_, removed) = self.entries.pop_lru().expect("nonempty bounded cache");
                    self.stats.retained_weight -= removed.weight;
                    self.stats.evictions += 1;
                }
                self.entries.insert(
                    key,
                    Entry {
                        query: prepared.clone(),
                        weight,
                    },
                );
                self.stats.retained_weight += weight;
                self.stats.entries = self.entries.len();
            }
        }
        Ok(prepared)
    }
}

// Reuse the bounded AST encoder without storing callback objects or sources.
fn structural_key(query: &Query, max: usize) -> Option<Vec<u8>> {
    crate::encoding::query_cache_key(query, max)
}

pub(super) fn cached(query: &Query) -> Result<(Option<PreparedQuery>, bool), SemanticError> {
    // Bypass borrowed execution before cloning an uncacheable query.
    let Some(key) = structural_key(query, (4 * 1024 * 1024) / 8) else {
        validate_query(query, query.inputs.len())?;
        return Ok((None, false));
    };
    static CACHE: OnceLock<Mutex<PreparedQueryCache>> = OnceLock::new();
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(PreparedQueryCache::new(64, 4 * 1024 * 1024)))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let before = cache.stats().hits;
    let prepared = cache.prepare_key(query, Some(key))?;
    Ok((Some(prepared), cache.stats().hits > before))
}

pub(crate) fn validate_program_query(
    query: &Query,
    input_count: usize,
) -> Result<(), SemanticError> {
    validate_query(query, input_count)
}

pub(super) fn validate_query_values(
    query: &Query,
    state: &mut State<'_>,
) -> Result<(), SemanticError> {
    let mut queries = vec![query];
    while let Some(query) = queries.pop() {
        for element in find_elements(&query.find) {
            if let FindElement::CustomAggregate(call) = element {
                state.check(call.args.len())?;
                for arg in &call.args {
                    if let AggregateArg::Constant(value) = arg {
                        state.validate_general(value)?;
                    }
                }
            }
        }
        let mut clauses: Vec<_> = query
            .clauses
            .iter()
            .chain(query.rules.iter().flat_map(|rule| &rule.clauses))
            .collect();
        while let Some(clause) = clauses.pop() {
            state.check(1)?;
            let terms: Vec<&Term> = match clause {
                Clause::Pattern(pattern) => join::pattern_terms(pattern)
                    .into_iter()
                    .map(|(_, term)| term)
                    .collect(),
                Clause::RelationPattern(pattern) => pattern.terms.iter().collect(),
                Clause::Predicate { args, .. } | Clause::Rule { args, .. } => args.iter().collect(),
                Clause::Function { function, args, .. } => {
                    if let Function::Query(query) = function {
                        queries.push(query);
                    }
                    args.iter().collect()
                }
                Clause::Not {
                    clauses: nested, ..
                } => {
                    clauses.extend(nested);
                    Vec::new()
                }
                Clause::Or { branches, .. } => {
                    clauses.extend(branches.iter().flatten());
                    Vec::new()
                }
            };
            for term in terms {
                if let Term::QueryConstant(value) = term {
                    state.validate_general(value)?;
                }
            }
        }
    }
    Ok(())
}

pub(super) fn validate_query(query: &Query, input_count: usize) -> Result<(), SemanticError> {
    if query.clauses.is_empty() && query.inputs.is_empty() {
        return Err(SemanticError::incorrect(
            "query/no-input-or-where",
            "query requires input bindings or where clauses",
        ));
    }
    if query.inputs.len() != input_count {
        return Err(SemanticError::incorrect(
            "query/input-arity",
            format!(
                "expected {} inputs, got {}",
                query.inputs.len(),
                input_count
            ),
        ));
    }
    validate_ground_clauses(&query.clauses)?;
    for rule in &query.rules {
        if rule.name.is_empty()
            || rule.head.is_empty()
            || rule.required.iter().any(|index| *index >= rule.head.len())
        {
            return Err(SemanticError::incorrect(
                "query/invalid-rule-head",
                "rule heads need a name, arguments, and valid required indexes",
            ));
        }
        validate_ground_clauses(&rule.clauses)?;
    }
    let mut pulled = BTreeSet::new();
    for element in find_elements(&query.find) {
        if let FindElement::Pull { variable, .. } = element
            && !pulled.insert(variable)
        {
            return Err(SemanticError::incorrect(
                "query/duplicate-pull-variable",
                format!(
                    "{} appears in more than one pull expression",
                    variable.name()
                ),
            ));
        }
    }
    let mut signatures = BTreeMap::new();
    for rule in &query.rules {
        if let Some((arity, required)) = signatures.get(rule.name.as_str()) {
            if *arity != rule.head.len() {
                return Err(SemanticError::incorrect(
                    "query/rule-arity",
                    format!("rule {} has inconsistent arity", rule.name),
                ));
            }
            if *required != &rule.required {
                return Err(SemanticError::incorrect(
                    "query/rule-required-mismatch",
                    format!("rule {} has inconsistent required bindings", rule.name),
                ));
            }
        } else {
            signatures.insert(rule.name.as_str(), (rule.head.len(), &rule.required));
        }
    }
    Ok(())
}

pub(super) fn validate_ground_clauses(clauses: &[Clause]) -> Result<(), SemanticError> {
    for clause in clauses {
        match clause {
            Clause::RelationPattern(pattern) if pattern.terms.is_empty() => {
                return Err(SemanticError::incorrect(
                    "query/empty-pattern",
                    "relation patterns require at least one term",
                ));
            }
            Clause::Function {
                function: Function::Fulltext,
                args,
                ..
            } if args.len() != 2 => {
                return Err(SemanticError::incorrect(
                    "query/function-arity",
                    "fulltext requires attribute and search string",
                ));
            }
            Clause::Function {
                function: Function::Tuple,
                args,
                ..
            } if args.is_empty() => {
                return Err(SemanticError::incorrect(
                    "query/function-arity",
                    "tuple requires one or more arguments",
                ));
            }
            Clause::Function {
                function: Function::Ground,
                args,
                ..
            } => {
                if args.len() != 1 {
                    return Err(SemanticError::incorrect(
                        "query/function-arity",
                        "ground requires exactly one constant",
                    ));
                }
                if !matches!(
                    args[0],
                    Term::Constant(_) | Term::QueryConstant(_) | Term::Nil
                ) {
                    return Err(SemanticError::incorrect(
                        "query/ground-not-constant",
                        "ground requires a constant argument",
                    ));
                }
            }
            Clause::Not { clauses, .. } => validate_ground_clauses(clauses)?,
            Clause::Or { branches, .. } => {
                for branch in branches {
                    validate_ground_clauses(branch)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

pub(super) fn validate_or(
    branches: &[Vec<Clause>],
    join: Option<&[Variable]>,
) -> Result<(), SemanticError> {
    if branches.is_empty() {
        return Err(SemanticError::incorrect(
            "query/empty-or",
            "or requires at least one branch",
        ));
    }
    if join.is_none() {
        let expected: BTreeSet<_> = variables_in_clauses(&branches[0]).into_iter().collect();
        if branches[1..].iter().any(|branch| {
            variables_in_clauses(branch)
                .into_iter()
                .collect::<BTreeSet<_>>()
                != expected
        }) {
            return Err(SemanticError::incorrect(
                "query/or-variable-mismatch",
                "or branches must use the same variables",
            ));
        }
    }
    Ok(())
}
