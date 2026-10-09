//! Current membership is an EAV event concern, independent of index order.
use crate::{Datom, IndexOrder, Value};
use std::cmp::Ordering;
use std::collections::BTreeMap;

struct StoredValue(Value);
impl PartialEq for StoredValue {
    fn eq(&self, other: &Self) -> bool {
        self.0.stored_eq(&other.0)
    }
}
impl Eq for StoredValue {}
impl Ord for StoredValue {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.stored_cmp(&other.0)
    }
}
impl PartialOrd for StoredValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Resolve one complete logical E/A/V event group before source filtering.
/// Keep the newest assertion per source after the latest global retraction.
/// Assertions at the retraction's T survive, preserving atomic add/retract
/// behavior. Strict stored value representations retain their existing identity.
pub(crate) fn current_eav_group(datoms: impl IntoIterator<Item = Datom>) -> Vec<Datom> {
    let mut values = BTreeMap::<StoredValue, Vec<Datom>>::new();
    for datom in datoms {
        values
            .entry(StoredValue(datom.value.clone()))
            .or_default()
            .push(datom);
    }
    let mut result = Vec::new();
    for mut events in values.into_values() {
        events.sort_by(|a, b| a.cmp_in(b, IndexOrder::Eavt));
        let barrier = events.iter().filter(|d| !d.added).map(|d| d.tx).max();
        let mut sources = BTreeMap::new();
        for datom in events {
            if datom.added && barrier.is_none_or(|t| datom.tx >= t) {
                sources.entry(datom.source).or_insert(datom);
            }
        }
        result.extend(sources.into_values());
    }
    result.sort_by(|a, b| a.cmp_in(b, IndexOrder::Eavt));
    result
}

/// Omission witnesses for one complete logical EAV group. Every retraction
/// witness is paired with every older assertion it withdraws; consumers
/// deduplicate the shared barrier only after collecting all witnesses.
pub(crate) fn no_history_eav_group(
    datoms: impl IntoIterator<Item = Datom>,
) -> Vec<crate::index::recent::NoHistoryPair> {
    let mut events: Vec<_> = datoms.into_iter().collect();
    events.sort_by(|a, b| a.cmp_in(b, IndexOrder::Eavt));
    let mut barriers = BTreeMap::<StoredValue, Datom>::new();
    let mut pairs = Vec::new();
    for datom in events {
        let key = StoredValue(datom.value.clone());
        if !datom.added {
            barriers.insert(key, datom);
        } else if let Some(retraction) = barriers.get(&key)
            && retraction.tx > datom.tx
        {
            pairs.push(crate::index::recent::NoHistoryPair {
                retraction: retraction.clone(),
                assertion: datom,
            });
        }
    }
    pairs
}
