//! Resident speculative indexes. Every index entry shares one immutable datom;
//! branches copy tree paths, never an accumulated transaction vector.
use crate::collections::persistent_map::{MapCursor, SharedMap};
use crate::{Datom, IndexOrder, IndexPrefix, Keyword};
use std::cmp::Ordering;
use std::sync::Arc;

#[derive(Clone, Debug)]
struct IndexKey {
    datom: Arc<Datom>,
    order: IndexOrder,
}
impl Ord for IndexKey {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.order as u8)
            .cmp(&(other.order as u8))
            .then_with(|| self.datom.cmp_in(&other.datom, self.order))
    }
}
impl PartialOrd for IndexKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for IndexKey {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}
impl Eq for IndexKey {}

#[derive(Clone, Debug)]
struct StoredEav(Arc<Datom>);
fn compare_eav(left: &Datom, right: &Datom) -> Ordering {
    left.entity
        .cmp(&right.entity)
        .then(left.attribute.cmp(&right.attribute))
        .then_with(|| left.value.stored_cmp(&right.value))
}
impl Ord for StoredEav {
    fn cmp(&self, other: &Self) -> Ordering {
        compare_eav(&self.0, &other.0)
    }
}
impl PartialOrd for StoredEav {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for StoredEav {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}
impl Eq for StoredEav {}

#[derive(Clone, Debug, Default)]
pub(crate) struct EavSet(SharedMap<StoredEav, ()>);
impl EavSet {
    pub(crate) fn contains(&self, datom: &Datom) -> bool {
        self.0.get_by(|key| compare_eav(&key.0, datom)).is_some()
    }
    pub(crate) fn insert(&mut self, datom: Arc<Datom>) {
        self.0.insert(StoredEav(datom), ());
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct OverlayIndexes {
    history: [SharedMap<IndexKey, ()>; 4],
    current: [SharedMap<IndexKey, ()>; 4],
    current_eav: EavSet,
    pub(crate) removals: EavSet,
    pub(crate) entids: SharedMap<Keyword, u64>,
    pub(crate) idents: SharedMap<u64, Keyword>,
}

const ORDERS: [IndexOrder; 4] = [
    IndexOrder::Eavt,
    IndexOrder::Aevt,
    IndexOrder::Avet,
    IndexOrder::Vaet,
];

impl OverlayIndexes {
    pub(crate) fn extend(&mut self, datoms: &[Datom], idents: Vec<(Keyword, u64)>) {
        let datoms: Vec<_> = datoms.iter().cloned().map(Arc::new).collect();
        for datom in &datoms {
            for (index, order) in ORDERS.into_iter().enumerate() {
                self.history[index].insert(
                    IndexKey {
                        datom: datom.clone(),
                        order,
                    },
                    (),
                );
            }
        }
        // A transaction's assertions win over its retractions for the same
        // physical E/A/V, independent of canonical assertion-first ordering.
        for datom in datoms.iter().filter(|datom| !datom.added) {
            self.remove_current(datom);
            self.removals.insert(datom.clone());
        }
        for datom in datoms.iter().filter(|datom| datom.added) {
            self.remove_current(datom);
            self.current_eav.insert(datom.clone());
            for (index, order) in ORDERS.into_iter().enumerate() {
                self.current[index].insert(
                    IndexKey {
                        datom: datom.clone(),
                        order,
                    },
                    (),
                );
            }
        }
        // Ident retractions do not erase aliases. Later assertions replace
        // only their own ident->entity and entity->canonical-name entries.
        for (ident, entity) in idents {
            self.entids.insert(ident.clone(), entity);
            self.idents.insert(entity, ident);
        }
    }

    fn remove_current(&mut self, datom: &Datom) {
        let old = self
            .current_eav
            .0
            .get_by(|key| compare_eav(&key.0, datom))
            .map(|(key, _)| key.clone());
        if let Some(old) = old {
            for (index, order) in ORDERS.into_iter().enumerate() {
                self.current[index].remove(&IndexKey {
                    datom: old.0.clone(),
                    order,
                });
            }
            self.current_eav.0.remove(&old);
        }
    }

    pub(crate) fn cursor(
        &self,
        history: bool,
        order: IndexOrder,
        compare: impl Fn(&Datom) -> Ordering,
        reverse: bool,
        prefix: Option<IndexPrefix>,
    ) -> OverlayIndexCursor {
        let index = if history {
            &self.history
        } else {
            &self.current
        };
        OverlayIndexCursor {
            cursor: index[order as usize].seek(|key| compare(&key.datom), reverse),
            prefix,
            finished: false,
        }
    }
}

pub(crate) struct OverlayIndexCursor {
    cursor: MapCursor<IndexKey, ()>,
    prefix: Option<IndexPrefix>,
    finished: bool,
}
impl Iterator for OverlayIndexCursor {
    type Item = Arc<Datom>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        let (key, ()) = self.cursor.next()?;
        if self
            .prefix
            .as_ref()
            .is_some_and(|prefix| !crate::index::compare_prefix(&key.datom, prefix).is_eq())
        {
            self.finished = true;
            return None;
        }
        Some(key.datom)
    }
}
