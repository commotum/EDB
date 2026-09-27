//! Explicit integrity proofs shared by live and repository inspection.
use crate::{Datom, IndexOrder, SemanticError};
use std::collections::BTreeMap;

pub(crate) fn derive_index_projection(
    database: &crate::Database,
    source: &[Datom],
    order: IndexOrder,
) -> Result<Vec<Datom>, SemanticError> {
    let mut datoms = source
        .iter()
        .filter_map(|datom| {
            let included = match order {
                IndexOrder::Eavt | IndexOrder::Aevt => Ok(true),
                IndexOrder::Avet => database
                    .schema()
                    .attribute(datom.attribute)
                    .map(|attribute| attribute.indexed || attribute.unique.is_some()),
                IndexOrder::Vaet => database
                    .schema()
                    .attribute(datom.attribute)
                    .map(|attribute| attribute.value_type == crate::ValueType::Ref),
            };
            match included {
                Ok(true) => Some(Ok(datom.clone())),
                Ok(false) => None,
                Err(error) => Some(Err(error)),
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    datoms.sort_by(|left, right| left.cmp_in(right, order));
    Ok(datoms)
}

pub(crate) fn same_stored_datoms(left: &[Datom], right: &[Datom]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| same_stored_datom(left, right))
}

fn same_stored_datom(left: &Datom, right: &Datom) -> bool {
    left.entity == right.entity
        && left.attribute == right.attribute
        && left.value.stored_eq(&right.value)
        && left.tx == right.tx
        && left.added == right.added
}

/// Prove that physical EAVT history is an authenticated-log projection, not
/// merely a self-consistent alternative tree. Within one logical E/A/V group,
/// each contiguous omitted run must reduce to empty by repeatedly deleting a
/// retraction followed by an assertion. This admits the nested omissions that
/// successive consolidation jobs can expose (`R R A A`), while rejecting a
/// fabricated fact, a one-sided omission, or an omission across a retained
/// fact. Every matched retraction must also have a noHistory=true opportunity
/// at a transaction boundary on or after it. This proves permission, not the
/// exact scheduler instant: there is no precise time at which an
/// eligible pair is physically forgotten.
pub(crate) fn validate_physical_history_projection(
    replayed: &[Datom],
    physical: &[Datom],
    through_t: u64,
) -> Result<(), SemanticError> {
    let opportunities = no_history_opportunities(replayed, through_t)?;
    let mut replay_offset = 0_usize;
    let mut physical_offset = 0_usize;

    while replay_offset < replayed.len() {
        let group_start = replay_offset;
        replay_offset += 1;
        while replay_offset < replayed.len()
            && same_logical_eav(&replayed[group_start], &replayed[replay_offset])
        {
            replay_offset += 1;
        }
        let group = &replayed[group_start..replay_offset];

        if let Some(actual) = physical.get(physical_offset)
            && logical_eav_cmp(actual, &group[0]).is_lt()
        {
            return Err(integrity_fault(
                "physical EAVT history contains a fact absent from authoritative replay",
            ));
        }

        let mut expected_in_group = 0_usize;
        let mut missing_start = 0_usize;
        while let Some(actual) = physical.get(physical_offset) {
            match logical_eav_cmp(actual, &group[0]) {
                std::cmp::Ordering::Less => {
                    return Err(integrity_fault(
                        "physical EAVT history contains a fact absent from authoritative replay",
                    ));
                }
                std::cmp::Ordering::Greater => break,
                std::cmp::Ordering::Equal => {}
            }

            while expected_in_group < group.len()
                && !same_stored_datom(&group[expected_in_group], actual)
            {
                expected_in_group += 1;
            }
            if expected_in_group == group.len() {
                return Err(integrity_fault(
                    "physical EAVT history contains a fact absent from authoritative replay",
                ));
            }
            validate_history_omission(
                &group[missing_start..expected_in_group],
                &opportunities,
                through_t,
            )?;
            expected_in_group += 1;
            missing_start = expected_in_group;
            physical_offset += 1;
        }
        validate_history_omission(&group[missing_start..], &opportunities, through_t)?;
    }

    if physical_offset != physical.len() {
        return Err(integrity_fault(
            "physical EAVT history has trailing facts absent from authoritative replay",
        ));
    }
    Ok(())
}

fn same_logical_eav(left: &Datom, right: &Datom) -> bool {
    logical_eav_cmp(left, right).is_eq()
}

fn logical_eav_cmp(left: &Datom, right: &Datom) -> std::cmp::Ordering {
    left.entity
        .cmp(&right.entity)
        .then(left.attribute.cmp(&right.attribute))
        .then_with(|| left.value.index_cmp(&right.value))
}

#[derive(Clone, Copy, Debug, Default)]
struct NoHistoryTransactionFold {
    asserted: Option<bool>,
    saw_retraction: bool,
}

/// Precompute inclusive transaction intervals in which each attribute had
/// `:db/noHistory` enabled. Schema facts in one transaction are folded as one
/// declarative change: an asserted boolean is the resulting value, while a
/// retraction-only transaction falls back to false. In particular, retracting
/// `false` never means `true`.
fn no_history_opportunities(
    replayed: &[Datom],
    through_t: u64,
) -> Result<BTreeMap<u32, Vec<(u64, u64)>>, SemanticError> {
    let mut transaction_folds = BTreeMap::<(u32, u64), NoHistoryTransactionFold>::new();
    for datom in replayed {
        if datom.attribute != crate::DB_NO_HISTORY as u32 {
            continue;
        }
        let attribute = crate::schema_eid_to_attr_id(datom.entity).map_err(|_| {
            integrity_fault("authoritative noHistory schema fact has an invalid schema entity")
        })?;
        let value = match datom.value {
            crate::Value::Bool(value) => value,
            _ => {
                return Err(integrity_fault(
                    "authoritative noHistory schema history has a non-boolean value",
                ));
            }
        };
        let at = crate::tx_to_t(datom.tx)?;
        let fold = transaction_folds.entry((attribute, at)).or_default();
        if datom.added {
            if fold.asserted.is_some_and(|asserted| asserted != value) {
                return Err(integrity_fault(
                    "authoritative noHistory schema transaction asserts conflicting values",
                ));
            }
            fold.asserted = Some(value);
        } else {
            fold.saw_retraction = true;
        }
    }

    let mut changes = BTreeMap::<u32, Vec<(u64, bool)>>::new();
    for ((attribute, at), fold) in transaction_folds {
        let enabled = fold.asserted.unwrap_or(false);
        debug_assert!(fold.asserted.is_some() || fold.saw_retraction);
        changes.entry(attribute).or_default().push((at, enabled));
    }

    let mut intervals = BTreeMap::<u32, Vec<(u64, u64)>>::new();
    for (attribute, changes) in changes {
        let mut enabled_from = None::<u64>;
        let mut attribute_intervals = Vec::new();
        for (at, enabled) in changes {
            if at > through_t {
                break;
            }
            if !enabled {
                if let Some(start) = enabled_from.take() {
                    attribute_intervals.push((start, at.saturating_sub(1)));
                }
            } else if enabled_from.is_none() {
                enabled_from = Some(at);
            }
        }
        if let Some(start) = enabled_from {
            attribute_intervals.push((start, through_t));
        }
        if !attribute_intervals.is_empty() {
            intervals.insert(attribute, attribute_intervals);
        }
    }
    Ok(intervals)
}

fn validate_history_omission(
    omitted: &[Datom],
    opportunities: &BTreeMap<u32, Vec<(u64, u64)>>,
    through_t: u64,
) -> Result<(), SemanticError> {
    let mut retractions = Vec::<&Datom>::new();
    for datom in omitted {
        if !datom.added {
            retractions.push(datom);
            continue;
        }
        let Some(retraction) = retractions.pop() else {
            return Err(integrity_fault(
                "physical EAVT history omission is not reducible noHistory history",
            ));
        };
        let retraction_t = crate::tx_to_t(retraction.tx)?;
        let permitted = opportunities
            .get(&retraction.attribute)
            .is_some_and(|intervals| {
                let candidate = intervals.partition_point(|(_, end)| *end < retraction_t);
                intervals
                    .get(candidate)
                    .is_some_and(|(start, _)| *start <= through_t)
            });
        if !permitted {
            return Err(integrity_fault(
                "physical EAVT history omission had no noHistory=true opportunity",
            ));
        }
    }
    if !retractions.is_empty() {
        return Err(integrity_fault(
            "physical EAVT history omits an unpaired authoritative retraction",
        ));
    }
    Ok(())
}

fn integrity_fault(message: impl Into<String>) -> SemanticError {
    SemanticError::new(
        crate::ErrorCategory::Fault,
        "integrity/tree-history-projection-mismatch",
        message,
    )
}
