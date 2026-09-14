//! Opt-in, payload-free observations of the native query scheduler.
use super::*;
use crate::Keyword;
use std::sync::atomic::Ordering;

/// Limits diagnostic capture independently of semantic query admission.
/// Exhaustion truncates the report, never changes the result or query budget.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QueryDiagnosticOptions {
    pub max_steps: usize,
    /// Cumulative conservative annotation bytes. Also bounds annotation-only
    /// inspections; this is a capture policy, not allocator/RSS measurement.
    pub max_bytes: usize,
}
impl Default for QueryDiagnosticOptions {
    fn default() -> Self {
        Self {
            max_steps: 4096,
            max_bytes: 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueryPhaseKind {
    Query,
    OrBranch,
    Negation,
    Rule,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryPhase {
    pub id: usize,
    pub parent: Option<usize>,
    pub parent_step: Option<usize>,
    /// Lexical scope in the caller's query, not a serialized clause/value.
    pub path: String,
    pub kind: QueryPhaseKind,
    pub scheduled_steps: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueryStepStatus {
    Running,
    Complete,
    AwaitNegative,
    AwaitRules,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueryWarning {
    /// No variable in this pattern was already bound; constants/index ranges
    /// can still make it selective. This is a hint, not a correctness failure.
    UnboundPattern,
    FullScan,
    /// Output cardinality exceeds input cardinality. Expansion may be intended.
    Expansion,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct QueryStepWork {
    /// Inclusive of child clauses. Do not sum parent and child counters.
    pub work: u64,
    pub datoms_examined: u64,
    pub index_seeks: u64,
    pub join_candidates: u64,
    pub allocated_value_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryClauseStep {
    /// Clause start order.
    pub id: usize,
    pub phase_id: usize,
    pub schedule_position: usize,
    pub clause_path: String,
    pub original_index: usize,
    pub operator: &'static str,
    pub source: Option<String>,
    pub access: String,
    pub binds_in: Vec<String>,
    pub binds_out: Vec<String>,
    /// False if capture stopped while inspecting output bindings.
    pub bindings_complete: bool,
    pub rows_in: usize,
    pub rows_out: Option<usize>,
    pub status: QueryStepStatus,
    pub work: QueryStepWork,
    pub warnings: Vec<QueryWarning>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct QueryDiagnostics {
    pub phases: Vec<QueryPhase>,
    pub steps: Vec<QueryClauseStep>,
    pub truncated: bool,
    pub capture_bytes: usize,
    pub inspection_work: usize,
}

#[derive(Clone)]
struct Location {
    path: String,
    scope: String,
    kind: QueryPhaseKind,
    index: usize,
}
pub(super) struct Trace {
    options: QueryDiagnosticOptions,
    report: QueryDiagnostics,
    locations: BTreeMap<usize, Location>,
    phase: Option<usize>,
    step: Option<usize>,
    cancel: Arc<AtomicBool>,
    deadline: Option<Instant>,
}
pub(super) struct StepToken {
    pub id: Option<usize>,
    previous: Option<usize>,
    before: QueryStepWork,
}

impl Trace {
    pub(super) fn new(
        options: QueryDiagnosticOptions,
        query: &Query,
        control: &QueryControl,
        deadline: Option<Instant>,
    ) -> Self {
        let mut trace = Self {
            options,
            report: QueryDiagnostics::default(),
            locations: BTreeMap::new(),
            phase: None,
            step: None,
            cancel: control.cancel.clone(),
            deadline,
        };
        if options.max_steps == 0 {
            trace.report.truncated = true;
        }
        trace.register(query, "");
        trace
    }
    pub(super) fn finish(self) -> QueryDiagnostics {
        self.report
    }
    fn bytes(&mut self, bytes: usize) -> bool {
        if !self.active() {
            return false;
        }
        if bytes
            > self
                .options
                .max_bytes
                .saturating_sub(self.report.capture_bytes)
        {
            self.report.truncated = true;
            return false;
        }
        self.report.capture_bytes += bytes;
        true
    }
    fn inspect(&mut self, work: usize) -> bool {
        if !self.active() {
            return false;
        }
        if work
            > self
                .options
                .max_bytes
                .saturating_sub(self.report.inspection_work)
        {
            self.report.truncated = true;
            return false;
        }
        self.report.inspection_work += work;
        true
    }
    fn active(&mut self) -> bool {
        if self.cancel.load(Ordering::Relaxed)
            || self
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.report.truncated = true;
        }
        !self.report.truncated
    }
    fn path(&mut self, prefix: &str, suffix: &str) -> Option<String> {
        let bytes = prefix.len().saturating_add(suffix.len()).saturating_add(1);
        if !self.bytes(bytes.saturating_add(std::mem::size_of::<String>())) {
            return None;
        }
        Some(if prefix.is_empty() {
            suffix.to_owned()
        } else {
            format!("{prefix}/{suffix}")
        })
    }
    fn register(&mut self, query: &Query, prefix: &str) {
        let Some(root) = self.path(prefix, "where") else {
            return;
        };
        if !self.bytes(std::mem::size_of::<(&[Clause], String, QueryPhaseKind)>() * 2) {
            return;
        }
        let mut pending = vec![(query.clauses.as_slice(), root, QueryPhaseKind::Query)];
        for (index, rule) in query.rules.iter().enumerate() {
            if !self.inspect(1)
                || !self.bytes(std::mem::size_of::<(&[Clause], String, QueryPhaseKind)>())
            {
                return;
            }
            let Some(path) = self.path(prefix, &format!("rules/{index}")) else {
                return;
            };
            pending.push((&rule.clauses, path, QueryPhaseKind::Rule));
        }
        while let Some((clauses, scope, kind)) = pending.pop() {
            for (index, clause) in clauses.iter().enumerate() {
                if !self.inspect(1) {
                    return;
                }
                let Some(path) = self.path(&scope, &index.to_string()) else {
                    return;
                };
                if !self.bytes(std::mem::size_of::<Location>() + 64 + scope.len() + path.len()) {
                    return;
                }
                self.locations.insert(
                    clause as *const Clause as usize,
                    Location {
                        path: path.clone(),
                        scope: scope.clone(),
                        kind,
                        index,
                    },
                );
                match clause {
                    Clause::Not { clauses, .. } => {
                        let Some(nested) = self.path(&path, "not") else {
                            return;
                        };
                        if !self.bytes(std::mem::size_of::<(&[Clause], String, QueryPhaseKind)>()) {
                            return;
                        }
                        pending.push((clauses, nested, QueryPhaseKind::Negation));
                    }
                    Clause::Or { branches, .. } => {
                        for (branch, clauses) in branches.iter().enumerate() {
                            if !self.inspect(1) {
                                return;
                            }
                            let Some(nested) = self.path(&path, &format!("or/{branch}")) else {
                                return;
                            };
                            if !self
                                .bytes(std::mem::size_of::<(&[Clause], String, QueryPhaseKind)>())
                            {
                                return;
                            }
                            pending.push((clauses, nested, QueryPhaseKind::OrBranch));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    pub(super) fn nested(&mut self, query: &Query) {
        let Some(step) = self.step else {
            return;
        };
        let bytes = self.report.steps[step].clause_path.len();
        if !self.bytes(bytes) {
            return;
        }
        let path = self.report.steps[step].clause_path.clone();
        let Some(prefix) = self.path(&path, "query") else {
            return;
        };
        self.register(query, &prefix);
    }
    pub(super) fn enter_phase(&mut self, clauses: &[Clause]) -> Option<usize> {
        let previous = self.phase;
        if self.report.truncated {
            return previous;
        }
        let location = clauses
            .first()
            .and_then(|clause| self.locations.get(&(clause as *const Clause as usize)));
        let Some(location) = location else {
            return previous;
        };
        let bytes = location.scope.len() + std::mem::size_of::<QueryPhase>() * 2;
        let pointer = clauses.first().unwrap() as *const Clause as usize;
        if !self.bytes(bytes) {
            return previous;
        }
        let location = &self.locations[&pointer];
        let location = (location.scope.clone(), location.kind);
        let id = self.report.phases.len();
        self.report.phases.push(QueryPhase {
            id,
            parent: previous,
            parent_step: self.step,
            path: location.0,
            kind: location.1,
            scheduled_steps: 0,
        });
        self.phase = Some(id);
        previous
    }
    pub(super) fn leave_phase(&mut self, previous: Option<usize>) {
        self.phase = previous;
    }
    fn bindings(&mut self, rows: &[Row]) -> Option<Vec<String>> {
        let Some(first) = rows.first() else {
            return Some(Vec::new());
        };
        let mut bindings = Vec::new();
        for variable in first.keys() {
            if !self.inspect(variable.name().len().saturating_add(1)) {
                return None;
            }
            let mut bound = true;
            for row in &rows[1..] {
                // BTree lookup compares at most logarithmically many names.
                let cost =
                    variable.name().len().saturating_add(1).saturating_mul(
                        usize::BITS as usize - row.len().leading_zeros() as usize + 1,
                    );
                if !self.inspect(cost) {
                    return None;
                }
                if !row.contains_key(variable) {
                    bound = false;
                    break;
                }
            }
            if bound {
                if !self.bytes(variable.name().len() + std::mem::size_of::<String>() * 2) {
                    return None;
                }
                bindings.push(variable.name().to_owned());
            }
        }
        Some(bindings)
    }
    fn unbound_pattern(&mut self, clause: &Clause, bindings: &[String]) -> bool {
        match clause {
            Clause::Pattern(pattern) => self.unbound_terms(
                [&pattern.entity, &pattern.attribute, &pattern.value]
                    .into_iter()
                    .chain(pattern.transaction.iter())
                    .chain(pattern.added.iter()),
                bindings,
            ),
            Clause::RelationPattern(pattern) => self.unbound_terms(pattern.terms.iter(), bindings),
            _ => false,
        }
    }
    fn unbound_terms<'a>(
        &mut self,
        terms: impl Iterator<Item = &'a Term>,
        bindings: &[String],
    ) -> bool {
        let mut has_variables = false;
        for term in terms {
            if !self.inspect(1) {
                return false;
            }
            if let Term::Variable(variable) = term {
                has_variables = true;
                let cost = variable.name().len().saturating_add(1).saturating_mul(
                    usize::BITS as usize - bindings.len().leading_zeros() as usize + 1,
                );
                if !self.inspect(cost) {
                    return false;
                }
                if bindings
                    .binary_search_by(|bound| bound.as_str().cmp(variable.name()))
                    .is_ok()
                {
                    return false;
                }
            }
        }
        has_variables
    }
    pub(super) fn begin_step(
        &mut self,
        clause: &Clause,
        rows: &[Row],
        inherited_source: Option<&str>,
        work: usize,
        stats: &QueryStats,
    ) -> StepToken {
        let mut token = StepToken {
            id: None,
            previous: self.step,
            before: snapshot(work, stats),
        };
        if self.report.truncated {
            return token;
        }
        if self.report.steps.len() >= self.options.max_steps {
            self.report.truncated = true;
            return token;
        }
        let Some(phase) = self.phase else {
            return token;
        };
        let Some(location) = self.locations.get(&(clause as *const Clause as usize)) else {
            return token;
        };
        let bytes = location.path.len()
            + std::mem::size_of::<QueryClauseStep>() * 2
            + 3 * std::mem::size_of::<QueryWarning>();
        if !self.bytes(bytes) {
            return token;
        }
        let location = &self.locations[&(clause as *const Clause as usize)];
        let (path, index) = (location.path.clone(), location.index);
        let Some(binds_in) = self.bindings(rows) else {
            return token;
        };
        let source = match clause {
            Clause::Pattern(p) => Some(p.source.as_str()),
            Clause::RelationPattern(p) => Some(p.source.as_str()),
            Clause::Predicate { source, .. }
            | Clause::Function { source, .. }
            | Clause::Rule { source, .. } => Some(source.as_str()),
            _ => inherited_source,
        }
        .map(|source| effective_source(source, inherited_source));
        let source = match source {
            Some(source) => {
                if !self.bytes(source.len()) {
                    return token;
                }
                Some(source.to_owned())
            }
            None => None,
        };
        let id = self.report.steps.len();
        let schedule_position = self.report.phases[phase].scheduled_steps;
        self.report.phases[phase].scheduled_steps += 1;
        let mut warnings = Vec::with_capacity(3);
        if self.unbound_pattern(clause, &binds_in) {
            warnings.push(QueryWarning::UnboundPattern);
        }
        self.report.steps.push(QueryClauseStep {
            id,
            phase_id: phase,
            schedule_position,
            clause_path: path,
            original_index: index,
            operator: operator(clause),
            source,
            access: String::new(),
            binds_in,
            binds_out: Vec::new(),
            bindings_complete: false,
            rows_in: rows.len(),
            rows_out: None,
            status: QueryStepStatus::Running,
            work: QueryStepWork::default(),
            warnings,
        });
        self.step = Some(id);
        token.id = Some(id);
        token
    }
    pub(super) fn end_step(
        &mut self,
        token: StepToken,
        rows: Option<&[Row]>,
        access: &str,
        status: QueryStepStatus,
        work: usize,
        stats: &QueryStats,
    ) {
        self.step = token.previous;
        let Some(id) = token.id else {
            return;
        };
        let binds_out = rows.and_then(|rows| self.bindings(rows));
        let access = if self.bytes(access.len()) {
            access.to_owned()
        } else {
            String::new()
        };
        let after = snapshot(work, stats);
        let step = &mut self.report.steps[id];
        step.bindings_complete = binds_out.is_some();
        step.binds_out = binds_out.unwrap_or_default();
        step.rows_out = rows.map(<[Row]>::len);
        step.status = status;
        step.work = QueryStepWork {
            work: after.work.saturating_sub(token.before.work),
            datoms_examined: after
                .datoms_examined
                .saturating_sub(token.before.datoms_examined),
            index_seeks: after.index_seeks.saturating_sub(token.before.index_seeks),
            join_candidates: after
                .join_candidates
                .saturating_sub(token.before.join_candidates),
            allocated_value_bytes: after
                .allocated_value_bytes
                .saturating_sub(token.before.allocated_value_bytes),
        };
        if access.contains("scan") {
            step.warnings.push(QueryWarning::FullScan);
        }
        if step.rows_out.is_some_and(|rows| rows > step.rows_in) {
            step.warnings.push(QueryWarning::Expansion);
        }
        step.access = access;
    }
}
fn snapshot(work: usize, stats: &QueryStats) -> QueryStepWork {
    QueryStepWork {
        work: work as u64,
        datoms_examined: stats.datoms_examined,
        index_seeks: stats.index_seeks,
        join_candidates: stats.join_candidates,
        allocated_value_bytes: stats.allocated_value_bytes,
    }
}
fn operator(clause: &Clause) -> &'static str {
    match clause {
        Clause::Predicate { predicate, .. } => match predicate {
            Predicate::Eq => "=",
            Predicate::NotEq => "!=",
            Predicate::Less => "<",
            Predicate::LessOrEqual => "<=",
            Predicate::Greater => ">",
            Predicate::GreaterOrEqual => ">=",
            Predicate::Missing => "missing?",
        },
        Clause::Function { function, .. } => match function {
            Function::Query(_) => "q",
            Function::Fulltext => "fulltext",
            Function::Extension(_) => "extension",
            Function::Ground => "ground",
            Function::Count => "count",
            Function::Quot => "quot",
            Function::Subs => "subs",
            Function::Str => "str",
            Function::StartsWith => "starts-with?",
            Function::EndsWith => "ends-with?",
            Function::Includes => "includes?",
            Function::Add => "+",
            Function::Subtract => "-",
            Function::Multiply => "*",
            Function::Divide => "/",
            Function::Tuple => "tuple",
            Function::Untuple => "untuple",
            Function::GetElse => "get-else",
            Function::GetSome => "get-some",
            Function::TxIds => "tx-ids",
            Function::TxData => "tx-data",
        },
        _ => clause_name(clause),
    }
}

/// EDN diagnostic data contains identifiers/counts only, never query constants,
/// argument values, complete clauses, result rows or callback payloads.
pub fn query_diagnostics_to_edn(report: &QueryDiagnostics) -> crate::edn::EdnValue {
    use crate::edn::EdnValue as E;
    fn number(n: usize) -> E {
        i64::try_from(n)
            .map(E::Long)
            .unwrap_or_else(|_| E::BigInt(n.into()))
    }
    fn count(n: u64) -> E {
        i64::try_from(n)
            .map(E::Long)
            .unwrap_or_else(|_| E::BigInt(n.into()))
    }
    fn map(fields: impl IntoIterator<Item = (&'static str, E)>) -> E {
        E::Map(
            fields
                .into_iter()
                .map(|(name, value)| (E::Keyword(Keyword::new("query", name)), value))
                .collect(),
        )
    }
    fn names(names: &[String]) -> E {
        E::Vector(names.iter().cloned().map(E::String).collect())
    }
    map([
        ("truncated", E::Bool(report.truncated)),
        ("capture-bytes", number(report.capture_bytes)),
        ("inspection-work", number(report.inspection_work)),
        (
            "phases",
            E::Vector(
                report
                    .phases
                    .iter()
                    .map(|phase| {
                        map([
                            ("id", number(phase.id)),
                            ("parent", phase.parent.map_or(E::Nil, number)),
                            ("parent-step", phase.parent_step.map_or(E::Nil, number)),
                            ("path", E::String(phase.path.clone())),
                            ("kind", E::String(format!("{:?}", phase.kind))),
                            ("scheduled-steps", number(phase.scheduled_steps)),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "steps",
            E::Vector(
                report
                    .steps
                    .iter()
                    .map(|step| {
                        map([
                            ("id", number(step.id)),
                            ("phase", number(step.phase_id)),
                            ("schedule-position", number(step.schedule_position)),
                            ("clause-path", E::String(step.clause_path.clone())),
                            ("original-index", number(step.original_index)),
                            ("operator", E::String(step.operator.into())),
                            ("source", step.source.clone().map_or(E::Nil, E::String)),
                            ("access", E::String(step.access.clone())),
                            ("binds-in", names(&step.binds_in)),
                            ("binds-out", names(&step.binds_out)),
                            ("bindings-complete", E::Bool(step.bindings_complete)),
                            ("rows-in", number(step.rows_in)),
                            ("rows-out", step.rows_out.map_or(E::Nil, number)),
                            ("status", E::String(format!("{:?}", step.status))),
                            ("work", count(step.work.work)),
                            ("datoms-examined", count(step.work.datoms_examined)),
                            ("index-seeks", count(step.work.index_seeks)),
                            ("join-candidates", count(step.work.join_candidates)),
                            ("value-bytes", number(step.work.allocated_value_bytes)),
                            (
                                "warnings",
                                E::Vector(
                                    step.warnings
                                        .iter()
                                        .map(|warning| E::String(format!("{warning:?}")))
                                        .collect(),
                                ),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}
