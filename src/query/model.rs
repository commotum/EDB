//! Immutable query syntax and public result shapes; source values are invocation arguments.
use super::*;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct Variable(String);

impl Variable {
    pub fn new(name: impl Into<String>) -> Result<Self, SemanticError> {
        let name = name.into();
        if name.is_empty() {
            return Err(SemanticError::incorrect(
                "query/empty-variable",
                "query variable names cannot be empty",
            ));
        }
        Ok(Self(name))
    }

    pub fn name(&self) -> &str {
        &self.0
    }
}

impl From<&str> for Variable {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Term {
    Variable(Variable),
    Constant(Value),
    /// General immutable query data; never a new persisted datom value type.
    QueryConstant(QueryValue),
    /// Query-language nil. Nil is a legal literal and binding value, but is
    /// deliberately not part of the persisted `Value` domain.
    Nil,
    Blank,
}

impl Term {
    pub fn var(name: impl Into<Variable>) -> Self {
        Self::Variable(name.into())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DataPattern {
    pub source: String,
    pub entity: Term,
    pub attribute: Term,
    pub value: Term,
    pub transaction: Option<Term>,
    pub added: Option<Term>,
}

impl DataPattern {
    pub fn new(entity: Term, attribute: Term, value: Term) -> Self {
        Self {
            source: "$".into(),
            entity,
            attribute,
            value,
            transaction: None,
            added: None,
        }
    }
}

/// A pattern over ordinary relation data, without a datom's five-column limit.
/// Database sources still use their E/A/V/T/assertion semantics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelationPattern {
    pub source: String,
    pub terms: Vec<Term>,
}

impl RelationPattern {
    pub fn new(terms: Vec<Term>) -> Self {
        Self {
            source: "$".into(),
            terms,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Predicate {
    Eq,
    NotEq,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
    Missing,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Function {
    Ground,
    Count,
    Quot,
    Subs,
    Str,
    StartsWith,
    EndsWith,
    Includes,
    Add,
    Subtract,
    Multiply,
    Divide,
    Tuple,
    Untuple,
    GetElse,
    GetSome,
    /// Transaction entities in the named immutable log's [start, end) range.
    TxIds,
    /// E/A/V/T/assertion tuples for one transaction in the named immutable log.
    TxData,
    /// Ranked entity/value/transaction/score hits from an eventually consistent
    /// search index, checked against the clause's supplied database view.
    Fulltext,
    Extension(String),
    /// Execute a native subquery against this clause's exact source (as `$`)
    /// and the enclosing named sources, sharing work, deadline and cancellation.
    Query(Box<Query>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Binding {
    Scalar(Variable),
    Tuple(Vec<Option<Variable>>),
    Collection(Variable),
    Relation(Vec<Option<Variable>>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Clause {
    Pattern(Box<DataPattern>),
    RelationPattern(Box<RelationPattern>),
    Predicate {
        predicate: Predicate,
        source: String,
        args: Vec<Term>,
    },
    Function {
        function: Function,
        source: String,
        args: Vec<Term>,
        binding: Binding,
    },
    Not {
        join: Option<Vec<Variable>>,
        clauses: Vec<Clause>,
    },
    Or {
        join: Option<Vec<Variable>>,
        branches: Vec<Vec<Clause>>,
    },
    Rule {
        source: String,
        name: String,
        args: Vec<Term>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Rule {
    pub name: String,
    pub head: Vec<Variable>,
    pub required: BTreeSet<usize>,
    pub clauses: Vec<Clause>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputSpec {
    Scalar(Variable),
    Tuple(Vec<Option<Variable>>),
    Collection(Variable),
    Relation(Vec<Option<Variable>>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QueryInput {
    Scalar(Value),
    Tuple(Vec<Value>),
    Collection(Vec<Value>),
    Relation(Vec<Vec<Value>>),
    /// `InputSpec` controls scalar versus sequence/relation destructuring.
    /// A map, nil or nested collection may itself be a single scalar input.
    General(QueryValue),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Aggregate {
    Count,
    CountDistinct,
    Min,
    Max,
    Sum,
    Average,
    Distinct,
    Median,
    Variance,
    StandardDeviation,
    MinN(usize),
    MaxN(usize),
    /// Select exactly n items with replacement (empty input produces none).
    Rand(usize),
    /// Select up to n logically distinct items without replacement.
    Sample(usize),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FindElement {
    Variable(Variable),
    Pull {
        source: String,
        variable: Variable,
        pattern: Box<PullPattern>,
    },
    Aggregate {
        function: Aggregate,
        variable: Variable,
    },
    /// A trusted, process-local grouped aggregate registered by the caller.
    CustomAggregate(Box<AggregateCall>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FindSpec {
    Relation(Vec<FindElement>),
    Collection(FindElement),
    Tuple(Vec<FindElement>),
    Scalar(FindElement),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Query {
    pub find: FindSpec,
    pub with: Vec<Variable>,
    pub inputs: Vec<InputSpec>,
    pub clauses: Vec<Clause>,
    pub rules: Vec<Rule>,
}

impl Query {
    pub fn new(find: FindSpec, clauses: Vec<Clause>) -> Self {
        Self {
            find,
            with: Vec::new(),
            inputs: Vec::new(),
            clauses,
            rules: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QueryResult {
    Relation(Vec<Vec<QueryValue>>),
    Collection(Vec<QueryValue>),
    Tuple(Option<Vec<QueryValue>>),
    Scalar(Option<QueryValue>),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct QueryStats {
    /// Cooperative evaluator and projection work charged to QueryControl.
    pub work: u64,
    pub clauses_executed: u64,
    pub datoms_examined: u64,
    pub index_seeks: u64,
    pub rows_produced: u64,
    pub rule_iterations: u64,
    pub prepared_cache_hits: u64,
    pub hash_join_build_rows: u64,
    pub hash_join_probes: u64,
    pub join_candidates: u64,
    pub grouped_probes_saved: u64,
    /// Conservative auxiliary join-table retention estimate, not measured
    /// allocator usage or process RSS.
    pub peak_join_bytes: usize,
    /// Accounted row/value allocation bytes over execution, not process RSS.
    pub allocated_value_bytes: usize,
    pub fulltext_searches: u64,
    /// Search indexes can lag even when the supplied database value is fixed.
    pub fulltext_lagging_searches: u64,
    pub fulltext_truncated_searches: u64,
    pub fulltext_read_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlanStep {
    pub clause: String,
    pub access: String,
    pub rows_before: usize,
    pub rows_after: usize,
    /// Correlation with the opt-in start-ordered diagnostic timeline.
    pub diagnostic_step: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryOutcome {
    pub result: QueryResult,
    pub stats: QueryStats,
    pub plan: Vec<PlanStep>,
    pub diagnostics: Option<QueryDiagnostics>,
}

pub(super) fn variables_in_clauses(clauses: &[Clause]) -> Vec<Variable> {
    let mut variables = BTreeSet::new();
    for clause in clauses {
        collect_clause_variables(clause, &mut variables);
    }
    variables.into_iter().collect()
}

pub(super) fn collect_clause_variables(clause: &Clause, output: &mut BTreeSet<Variable>) {
    let mut terms = Vec::new();
    match clause {
        Clause::RelationPattern(pattern) => terms.extend(&pattern.terms),
        Clause::Pattern(pattern) => {
            terms.extend([&pattern.entity, &pattern.attribute, &pattern.value]);
            terms.extend(pattern.transaction.iter());
            terms.extend(pattern.added.iter());
        }
        Clause::Predicate { args, .. }
        | Clause::Function { args, .. }
        | Clause::Rule { args, .. } => terms.extend(args),
        Clause::Not {
            join: Some(join), ..
        }
        | Clause::Or {
            join: Some(join), ..
        } => {
            output.extend(join.iter().cloned());
        }
        Clause::Not {
            join: None,
            clauses,
        } => {
            output.extend(variables_in_clauses(clauses));
        }
        Clause::Or {
            join: None,
            branches,
        } => {
            for branch in branches {
                for variable in variables_in_clauses(branch) {
                    output.insert(variable);
                }
            }
        }
    }
    for term in terms {
        if let Term::Variable(variable) = term {
            output.insert(variable.clone());
        }
    }
    if let Clause::Function { binding, .. } = clause {
        for variable in binding_variables(binding) {
            output.insert(variable.clone());
        }
    }
}

pub(super) fn binding_variables(binding: &Binding) -> Vec<&Variable> {
    match binding {
        Binding::Scalar(variable) | Binding::Collection(variable) => vec![variable],
        Binding::Tuple(variables) | Binding::Relation(variables) => {
            variables.iter().flatten().collect()
        }
    }
}
pub(super) fn find_variables(elements: &[FindElement]) -> Vec<Variable> {
    let mut variables = Vec::new();
    for element in elements {
        match element {
            FindElement::Variable(variable)
            | FindElement::Pull { variable, .. }
            | FindElement::Aggregate { variable, .. } => variables.push(variable.clone()),
            FindElement::CustomAggregate(call) => {
                variables.extend(call.args.iter().filter_map(|arg| match arg {
                    AggregateArg::Variable(variable) => Some(variable.clone()),
                    _ => None,
                }));
            }
        }
    }
    variables
}
pub(super) fn find_elements(find: &FindSpec) -> &[FindElement] {
    match find {
        FindSpec::Relation(elements) | FindSpec::Tuple(elements) => elements,
        FindSpec::Collection(element) | FindSpec::Scalar(element) => std::slice::from_ref(element),
    }
}
