//! Demand-driven positive rule fixed points; completed negatives are owned by dependencies.
use super::*;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct RuleInvocationKey {
    pub(super) source: String,
    pub(super) name: String,
    pub(super) bindings: Vec<Option<BoundValue>>,
}

pub(super) fn rule_signature<'a>(
    rules: &'a [Rule],
    name: &str,
) -> Result<(usize, &'a BTreeSet<usize>), SemanticError> {
    rules
        .iter()
        .find(|rule| rule.name == name)
        .map(|rule| (rule.head.len(), &rule.required))
        .ok_or_else(|| {
            SemanticError::incorrect("query/unknown-rule", format!("unknown rule {name}"))
        })
}

pub(super) fn solve_rule_invocations(
    rules: &[Rule],
    state: &mut State<'_>,
) -> EvaluationResult<()> {
    // ATOMIC-NOTE: Recovered `eval-query` keys its input work by source and adorned
    // predicate, accumulates answers in `ans`, and the outer `qsqr` loop
    // repeats until answer cardinalities stop changing. This memo uses the
    // concrete bound values as well as the adornment: it is more selective,
    // while retaining the same monotone fixed-point boundary.
    if !state.solving_rules && !state.rule_memo_complete {
        if state.defer_rules {
            // A sibling already requested negative work during this pass.
            // Register the new keys but let the driver solve them as one batch.
            return Err(dependencies::EvaluationError::AwaitRules);
        }
        state.solving_rules = true;
        let result = stabilize_rule_memo(rules, state);
        state.solving_rules = false;
        if matches!(
            &result,
            Err(dependencies::EvaluationError::AwaitNegative(_))
        ) {
            state.defer_rules = true;
        }
        result?;
        state.rule_memo_complete = true;
    }
    Ok(())
}

pub(super) fn stabilize_rule_memo(rules: &[Rule], state: &mut State<'_>) -> EvaluationResult<()> {
    loop {
        state.check(1)?;
        let before_entries = state.rule_memo.len();
        let before_rows = state.rule_memo.values().map(Vec::len).sum::<usize>();
        let keys = state.rule_memo.keys().cloned().collect::<Vec<_>>();
        let mut pending = BTreeSet::new();

        for key in keys {
            let produced = match evaluate_rule_key(&key, rules, state) {
                Ok(rows) => rows,
                Err(dependencies::EvaluationError::AwaitNegative(requests)) => {
                    pending.extend(requests);
                    continue;
                }
                Err(error) => return Err(error),
            };
            ensure_rule_memo_entry(state, key.clone())?;
            let mut rows = std::mem::take(
                state
                    .rule_memo
                    .get_mut(&key)
                    .expect("rule memo entry was ensured"),
            );
            let prior = rows.len();
            rows.extend(produced);
            dedupe_bound_tuples(&mut rows, state)?;
            let (length, added) = (rows.len(), rows.len() - prior);
            state.rule_memo.insert(key, rows);
            state.check(added)?;
            if length > state.control.max_intermediate_rows {
                return Err(resource(
                    "query/intermediate-limit",
                    "rule memo relation exceeded the intermediate row limit",
                )
                .into());
            }
        }

        if !pending.is_empty() {
            return Err(dependencies::EvaluationError::AwaitNegative(
                pending.into_iter().collect(),
            ));
        }

        state.stats.rule_iterations += 1;
        let after_rows = state.rule_memo.values().map(Vec::len).sum::<usize>();
        if state.rule_memo.len() == before_entries && after_rows == before_rows {
            return Ok(());
        }
    }
}

pub(super) fn evaluate_rule_key(
    key: &RuleInvocationKey,
    rules: &[Rule],
    state: &mut State<'_>,
) -> EvaluationResult<Vec<Vec<BoundValue>>> {
    let mut produced = Vec::new();
    let mut pending = BTreeSet::new();
    for rule in rules.iter().filter(|rule| rule.name == key.name) {
        let mut seed = Row::new();
        let mut compatible = true;
        for (variable, value) in rule.head.iter().zip(&key.bindings) {
            if let Some(value) = value
                && !unify_variable_checked(&mut seed, variable, value, state)?
            {
                compatible = false;
                break;
            }
        }
        if !compatible {
            continue;
        }
        let rows =
            match evaluate_clauses(&rule.clauses, vec![seed], rules, Some(&key.source), state) {
                Ok(rows) => rows,
                Err(dependencies::EvaluationError::AwaitNegative(requests)) => {
                    pending.extend(requests);
                    continue;
                }
                Err(error) => return Err(error),
            };
        for row in rows {
            let tuple = rule
                .head
                .iter()
                .map(|variable| {
                    row.get(variable).cloned().ok_or_else(|| {
                        SemanticError::incorrect(
                            "query/unbound-rule-head",
                            format!("rule {} did not bind {}", rule.name, variable.name()),
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            state.charge_value_bytes(tuple.iter().fold(0usize, |bytes, value| {
                bytes.saturating_add(join::bound_bytes(value))
            }))?;
            produced.push(tuple);
        }
    }
    if !pending.is_empty() {
        return Err(dependencies::EvaluationError::AwaitNegative(
            pending.into_iter().collect(),
        ));
    }
    dedupe_bound_tuples(&mut produced, state)?;
    Ok(produced)
}

pub(super) fn ensure_rule_memo_entry(
    state: &mut State<'_>,
    key: RuleInvocationKey,
) -> Result<(), SemanticError> {
    if !state.rule_memo.contains_key(&key) {
        state.rule_memo_complete = false;
        state.charge_value_bytes(key.bindings.iter().flatten().fold(
            std::mem::size_of::<RuleInvocationKey>() + key.name.len() + key.source.len(),
            |bytes, value| bytes.saturating_add(join::bound_bytes(value)),
        ))?;
    }
    state.rule_memo.entry(key).or_default();
    Ok(())
}

pub(super) fn rule_memo_rows(
    state: &mut State<'_>,
    key: &RuleInvocationKey,
) -> Result<Vec<Vec<BoundValue>>, SemanticError> {
    let bytes = state
        .rule_memo
        .get(key)
        .into_iter()
        .flatten()
        .flatten()
        .fold(0usize, |bytes, value| {
            bytes.saturating_add(join::bound_bytes(value))
        });
    state.charge_value_bytes(bytes)?;
    Ok(state.rule_memo.get(key).cloned().unwrap_or_default())
}
