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
                IndexOrder::Eavt | IndexOrder::Aevt | IndexOrder::Saevt => Ok(true),
                IndexOrder::Avet | IndexOrder::Savet => database
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
        && left.source == right.source
        && left.added == right.added
}

/// Prove that physical EAVT history is an authenticated-log projection.
/// A noHistory omission removes a retraction together with every assertion of
/// the same stored EAV that it withdrew. One retraction can withdraw several
/// source contributions. Keeping only part of that group would lose attribution
/// or allow a removed retraction to expose an older assertion.
/// Permission is established at any noHistory=true transaction boundary on or
/// after the retraction; physical maintenance has no prescribed instant.
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
        let mut retained = vec![false; group.len()];
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
            retained[expected_in_group] = true;
            expected_in_group += 1;
            physical_offset += 1;
        }
        validate_history_omission(group, &retained, &opportunities, through_t)?;
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
    group: &[Datom],
    retained: &[bool],
    opportunities: &BTreeMap<u32, Vec<(u64, u64)>>,
    through_t: u64,
) -> Result<(), SemanticError> {
    // Logical numeric ordering can group different stored representations.
    // Retractions withdraw an exact stored EAV, so validate each separately.
    let mut positions: Vec<_> = (0..group.len()).collect();
    positions.sort_by(|a, b| {
        group[*a]
            .value
            .stored_cmp(&group[*b].value)
            .then_with(|| group[*a].cmp_in(&group[*b], IndexOrder::Eavt))
    });
    let mut start = 0;
    while start < positions.len() {
        let mut end = start + 1;
        while end < positions.len()
            && group[positions[start]]
                .value
                .stored_eq(&group[positions[end]].value)
        {
            end += 1;
        }
        let stored = &positions[start..end];
        let mut at = 0;
        // Assertions preceding the newest retraction are still current.
        while at < stored.len() && group[stored[at]].added {
            if !retained[stored[at]] {
                return Err(integrity_fault(
                    "physical history omits an unwithdrawn assertion",
                ));
            }
            at += 1;
        }
        while at < stored.len() {
            let retraction_index = stored[at];
            let retraction = &group[retraction_index];
            let mut next = at + 1;
            while next < stored.len() && group[stored[next]].added {
                next += 1;
            }
            let assertions = &stored[at + 1..next];
            let kept = retained[retraction_index];
            if assertions
                .iter()
                .any(|i| retained[*i] != kept || group[*i].tx >= retraction.tx)
            {
                return Err(integrity_fault(
                    "physical history must retain or omit a retraction with all withdrawn source contributions",
                ));
            }
            if !kept {
                if assertions.is_empty() {
                    return Err(integrity_fault(
                        "physical history omits an unpaired retraction",
                    ));
                }
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
            at = next;
        }
        start = end;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn event(t: u64, source: u64, added: bool) -> Datom {
        Datom {
            entity: 100,
            attribute: 99,
            value: crate::Value::Long(1916),
            tx: crate::t_to_tx(t).unwrap(),
            source,
            added,
        }
    }

    #[test]
    fn history_omission_preserves_complete_global_withdrawals() {
        let group = vec![
            event(3, 102, false),
            event(2, 101, true),
            event(1, 100, true),
        ];
        let opportunities = BTreeMap::from([(99, vec![(4, 5)])]);
        for kept in [[true, true, true], [false, false, false]] {
            validate_history_omission(&group, &kept, &opportunities, 5).unwrap();
        }
        for kept in [
            [false, true, false],
            [false, false, true],
            [true, false, false],
            [true, true, false],
        ] {
            assert!(validate_history_omission(&group, &kept, &opportunities, 5).is_err());
        }
        assert!(validate_history_omission(&group, &[false; 3], &BTreeMap::new(), 5).is_err());
    }

    #[test]
    fn history_omission_keeps_new_assertions_and_separates_withdrawals() {
        let group = vec![
            event(6, 100, true),
            event(5, 102, false),
            event(4, 101, true),
            event(3, 102, false),
            event(2, 101, true),
            event(1, 100, true),
        ];
        let opportunities = BTreeMap::from([(99, vec![(0, 6)])]);
        for kept in [
            [true, false, false, true, true, true],
            [true, true, true, false, false, false],
            [true, false, false, false, false, false],
        ] {
            validate_history_omission(&group, &kept, &opportunities, 6).unwrap();
        }
        assert!(validate_history_omission(&group, &[false; 6], &opportunities, 6).is_err());
    }
}
