//! Binding availability and complete negative subqueries over immutable sources.
//! A positive fixed point may expose provisional answers to another positive
//! rule, but never to set difference. Recovered `eval-not-join` invokes `q` to
//! completion with a separate answer table before removing matching tuples.
use super::*;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(super) struct NegativeInvocation {
    // Addresses identify clauses only during this immutable query invocation.
    // They never escape to a prepared cache, serialized query or durable key.
    clause: usize,
    source: Option<String>,
    seed: Row,
}

pub(super) enum EvaluationError {
    Semantic(SemanticError),
    AwaitNegative(Vec<NegativeInvocation>),
    /// A sibling has already requested negatives. Keep collecting invocation
    /// keys without repeatedly re-solving the suspended positive table.
    AwaitRules,
}

pub(super) enum NegativeStatus {
    Complete(bool),
    Pending(NegativeInvocation),
}

impl From<SemanticError> for EvaluationError {
    fn from(error: SemanticError) -> Self {
        Self::Semantic(error)
    }
}

pub(super) type EvaluationResult<T> = Result<T, EvaluationError>;

/// Drive demand for complete negative relations on an explicit stack. Each task
/// has its own positive fixed point. Pausing it cannot publish a provisional
/// complement: the typed suspension unwinds to this driver before set difference.
/// Only existence is retained, keyed by lexical clause, inherited source and the
/// exact projected input row. All tasks share the original resource accounting.
pub(super) fn evaluate_complete(
    query: &Query,
    initial: Vec<Row>,
    state: &mut State<'_>,
) -> Result<Vec<Row>, SemanticError> {
    struct Task<'a> {
        clauses: &'a [Clause],
        initial: Vec<Row>,
        key: Option<NegativeInvocation>,
        memo: BTreeMap<RuleInvocationKey, Vec<Vec<BoundValue>>>,
        memo_complete: bool,
    }

    let mut negatives = BTreeMap::new();
    let mut pending: Vec<_> = query.clauses.iter().collect();
    pending.extend(query.rules.iter().flat_map(|rule| &rule.clauses));
    while let Some(clause) = pending.pop() {
        state.check(1)?;
        match clause {
            Clause::Not { clauses, .. } => {
                negatives.insert(clause as *const Clause as usize, clauses.as_slice());
                pending.extend(clauses);
            }
            Clause::Or { branches, .. } => pending.extend(branches.iter().flatten()),
            _ => {}
        }
    }
    if negatives.is_empty() {
        return match evaluate_clauses(&query.clauses, initial, &query.rules, None, state) {
            Ok(rows) => Ok(rows),
            Err(EvaluationError::Semantic(error)) => Err(error),
            Err(EvaluationError::AwaitNegative(_) | EvaluationError::AwaitRules) => Err(fault(
                "query/negative-clause",
                "negative dependency is outside its query scope",
            )),
        };
    }
    let mut tasks = vec![Task {
        clauses: &query.clauses,
        initial,
        key: None,
        memo: BTreeMap::new(),
        memo_complete: false,
    }];
    while let Some(task) = tasks.last_mut() {
        state.check(1)?;
        state.rule_memo = std::mem::take(&mut task.memo);
        state.rule_memo_complete = task.memo_complete;
        state.defer_rules = false;
        // A task restarts its clause sequence after a negative dependency has
        // completed. Retained positive answers remain valid; charge every repeat
        // and every cloned input rather than hiding the cost of this restart.
        state.charge_value_bytes(task.initial.iter().fold(0usize, |bytes, row| {
            bytes.saturating_add(join::row_bytes(row))
        }))?;
        let source = task.key.as_ref().and_then(|key| key.source.as_deref());
        let result = evaluate_clauses(
            task.clauses,
            task.initial.clone(),
            &query.rules,
            source,
            state,
        );
        task.memo = std::mem::take(&mut state.rule_memo);
        task.memo_complete = state.rule_memo_complete;
        match result {
            Ok(rows) => {
                let completed = tasks.pop().expect("active task");
                if let Some(key) = completed.key {
                    state.negative_memo.insert(key, rows.is_empty());
                } else {
                    return Ok(rows);
                }
            }
            Err(EvaluationError::Semantic(error)) => return Err(error),
            Err(EvaluationError::AwaitRules) => {
                return Err(fault(
                    "query/negative-dependency",
                    "deferred rule work has no negative dependency",
                ));
            }
            Err(EvaluationError::AwaitNegative(requests)) => {
                // A clause submits its whole set of missing seeds at once. Do
                // not restart its earlier scans once per candidate row.
                for key in requests.into_iter().rev() {
                    let clauses = negatives.get(&key.clause).copied().ok_or_else(|| {
                        fault(
                            "query/negative-clause",
                            "negative dependency is outside its query scope",
                        )
                    })?;
                    state.charge_value_bytes(
                        std::mem::size_of::<Task<'_>>() + join::row_bytes(&key.seed),
                    )?;
                    tasks.push(Task {
                        clauses,
                        initial: vec![key.seed.clone()],
                        key: Some(key),
                        memo: BTreeMap::new(),
                        memo_complete: false,
                    });
                }
            }
        }
    }
    unreachable!("the root task returns its relation")
}

pub(super) fn evaluate_negative(
    clause: &Clause,
    seed: Row,
    inherited_source: Option<&str>,
    state: &mut State<'_>,
) -> Result<NegativeStatus, SemanticError> {
    state.check(1)?;
    state.charge_value_bytes(
        std::mem::size_of::<NegativeInvocation>()
            + inherited_source.map_or(0, str::len)
            + join::row_bytes(&seed),
    )?;
    let key = NegativeInvocation {
        clause: clause as *const Clause as usize,
        source: inherited_source.map(str::to_owned),
        seed,
    };
    match state.negative_memo.get(&key) {
        Some(empty) => Ok(NegativeStatus::Complete(*empty)),
        None => Ok(NegativeStatus::Pending(key)),
    }
}

pub(super) fn ready(
    clause: &Clause,
    row: &Row,
    rules: &[Rule],
    state: &mut State<'_>,
) -> Result<bool, SemanticError> {
    let bound = row.keys().cloned().collect();
    Ok(clause_bindings(clause, &bound, rules, state)?.is_some())
}

type Bindings = BTreeSet<Variable>;

fn term_ready(term: &Term, bound: &Bindings) -> bool {
    match term {
        Term::Variable(variable) => bound.contains(variable),
        Term::Blank => false,
        Term::Constant(_) | Term::QueryConstant(_) | Term::Nil => true,
    }
}

fn sequence_bindings(
    clauses: &[Clause],
    mut bound: Bindings,
    rules: &[Rule],
    state: &mut State<'_>,
) -> Result<Option<Bindings>, SemanticError> {
    let mut remaining: Vec<_> = clauses.iter().collect();
    while !remaining.is_empty() {
        let mut selected = None;
        for (index, clause) in remaining.iter().enumerate() {
            if let Some(produced) = clause_bindings(clause, &bound, rules, state)? {
                selected = Some((index, produced));
                break;
            }
        }
        let Some((index, produced)) = selected else {
            return Ok(None);
        };
        remaining.remove(index);
        bound.extend(produced);
    }
    Ok(Some(bound))
}

fn clause_bindings(
    clause: &Clause,
    bound: &Bindings,
    rules: &[Rule],
    state: &mut State<'_>,
) -> Result<Option<Bindings>, SemanticError> {
    state.check(1)?;
    match clause {
        Clause::Or { join, branches } => {
            validate_or(branches, join.as_deref())?;
            let outward: Bindings = join.as_ref().map_or_else(
                || variables_in_clauses(&branches[0]).into_iter().collect(),
                |variables| variables.iter().cloned().collect(),
            );
            let seed: Bindings = join.as_ref().map_or_else(
                || bound.clone(),
                |variables| {
                    variables
                        .iter()
                        .filter(|v| bound.contains(*v))
                        .cloned()
                        .collect()
                },
            );
            let mut produced: Option<Bindings> = None;
            for branch in branches {
                let Some(branch) = sequence_bindings(branch, seed.clone(), rules, state)? else {
                    return Ok(None);
                };
                // Every successful branch must supply the same outward bindings.
                // Join-local variables do not leak or acquire outer values merely
                // because the caller happens to use the same variable name.
                if !outward.is_subset(&branch) {
                    return Ok(None);
                }
                produced = Some(match produced {
                    Some(previous) => previous.intersection(&branch).cloned().collect(),
                    None => branch,
                });
            }
            let mut produced = produced.unwrap_or_default();
            if let Some(join) = join {
                produced.retain(|variable| join.contains(variable));
            }
            Ok(Some(produced))
        }
        Clause::Not { join, clauses } => {
            let required: Bindings = join
                .clone()
                .unwrap_or_else(|| variables_in_clauses(clauses))
                .into_iter()
                .collect();
            if !required.is_subset(bound) {
                return Ok(None);
            }
            let seed = if join.is_some() {
                required
            } else {
                bound.clone()
            };
            Ok(sequence_bindings(clauses, seed, rules, state)?.map(|_| Bindings::new()))
        }
        Clause::Function { args, .. } | Clause::Predicate { args, .. }
            if args
                .iter()
                .any(|term| matches!(term, Term::Variable(v) if !bound.contains(v))) =>
        {
            Ok(None)
        }
        Clause::Rule { name, args, .. } => {
            // Preserve the evaluator's precise unknown-rule/arity diagnostics.
            if let Ok((arity, required)) = rule_signature(rules, name)
                && arity == args.len()
                && required
                    .iter()
                    .any(|index| !term_ready(&args[*index], bound))
            {
                return Ok(None);
            }
            Ok(Some(
                variables_in_clauses(std::slice::from_ref(clause))
                    .into_iter()
                    .collect(),
            ))
        }
        Clause::Predicate { .. } => Ok(Some(Bindings::new())),
        Clause::Function { binding, .. } => Ok(Some(
            binding_variables(binding).into_iter().cloned().collect(),
        )),
        Clause::Pattern(_) | Clause::RelationPattern(_) => Ok(Some(
            variables_in_clauses(std::slice::from_ref(clause))
                .into_iter()
                .collect(),
        )),
    }
}

/// A cycle that crosses a complement cannot use the monotone positive-rule
/// evaluator. Reject it before exposing any provisional result, while permitting
/// positive/mutual recursion and arbitrarily many acyclic negative strata. Only
/// rules reachable from this query participate; nested `q` has its own rule scope.
pub(super) fn validate_negation(query: &Query, state: &mut State<'_>) -> Result<(), SemanticError> {
    if query.rules.is_empty() {
        return Ok(());
    }
    let names: BTreeSet<_> = query.rules.iter().map(|rule| rule.name.as_str()).collect();
    let indices: BTreeMap<_, _> = names
        .iter()
        .copied()
        .enumerate()
        .map(|(i, name)| (name, i))
        .collect();
    let names: Vec<_> = names.into_iter().collect();
    let mut edges = vec![Vec::new(); names.len()];
    for rule in &query.rules {
        edges[indices[rule.name.as_str()]].extend(rule_edges(&rule.clauses, &indices, state)?);
    }
    let mut reachable = vec![false; names.len()];
    let mut pending: Vec<_> = rule_edges(&query.clauses, &indices, state)?
        .into_iter()
        .map(|(i, _)| i)
        .collect();
    while let Some(node) = pending.pop() {
        state.check(1)?;
        if std::mem::replace(&mut reachable[node], true) {
            continue;
        }
        pending.extend(edges[node].iter().map(|(next, _)| *next));
    }
    // Iterative Kosaraju traversal: graph depth never becomes Rust call depth.
    let mut seen = vec![false; names.len()];
    let mut finished = Vec::new();
    for start in 0..names.len() {
        if !reachable[start] || seen[start] {
            continue;
        }
        seen[start] = true;
        let mut stack = vec![(start, 0)];
        while let Some((node, next)) = stack.last_mut() {
            state.check(1)?;
            if let Some((child, _)) = edges[*node].get(*next) {
                *next += 1;
                if !seen[*child] {
                    seen[*child] = true;
                    stack.push((*child, 0));
                }
            } else {
                finished.push(*node);
                stack.pop();
            }
        }
    }
    let mut reverse = vec![Vec::new(); names.len()];
    for (owner, dependencies) in edges.iter().enumerate() {
        for (dependency, _) in dependencies {
            state.check(1)?;
            reverse[*dependency].push(owner);
        }
    }
    let mut components = vec![usize::MAX; names.len()];
    for root in finished.into_iter().rev() {
        if components[root] != usize::MAX {
            continue;
        }
        let mut pending = vec![root];
        while let Some(node) = pending.pop() {
            state.check(1)?;
            if !reachable[node] || components[node] != usize::MAX {
                continue;
            }
            components[node] = root;
            pending.extend(reverse[node].iter().copied());
        }
    }
    for (owner, dependencies) in edges.iter().enumerate() {
        if !reachable[owner] {
            continue;
        }
        for (dependency, negative) in dependencies {
            state.check(1)?;
            if *negative && components[owner] == components[*dependency] {
                return Err(SemanticError::incorrect(
                    "query/unstratified-negation",
                    "a recursive rule dependency crosses negation; its complement has no completed positive fixed point",
                ).detail("rule", names[owner]).detail("dependency", names[*dependency]));
            }
        }
    }
    Ok(())
}

fn rule_edges(
    clauses: &[Clause],
    indices: &BTreeMap<&str, usize>,
    state: &mut State<'_>,
) -> Result<Vec<(usize, bool)>, SemanticError> {
    let mut pending: Vec<_> = clauses.iter().map(|clause| (clause, false)).collect();
    let mut edges = Vec::new();
    while let Some((clause, negative)) = pending.pop() {
        state.check(1)?;
        match clause {
            Clause::Rule { name, .. } => {
                if let Some(index) = indices.get(name.as_str()) {
                    state.charge_value_bytes(std::mem::size_of::<(usize, bool)>())?;
                    edges.push((*index, negative));
                }
            }
            Clause::Not { clauses, .. } => {
                pending.extend(clauses.iter().map(|clause| (clause, true)))
            }
            Clause::Or { branches, .. } => {
                pending.extend(branches.iter().flatten().map(|clause| (clause, negative)))
            }
            _ => {}
        }
    }
    Ok(edges)
}
