//! EDB facts and covering-index orders shared by memory and durable trees.

use crate::Value;
use std::cmp::Ordering;
use std::mem::size_of;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Datom {
    pub entity: u64,
    pub attribute: u32,
    pub value: Value,
    pub tx: u64,
    pub source: u64,
    pub added: bool,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum IndexOrder {
    Eavt,
    Aevt,
    Avet,
    Vaet,
    Saevt,
    Savet,
}

impl Datom {
    /// Deterministic account of the inline datom plus recursively owned value
    /// storage. It intentionally excludes allocator headers and shared index
    /// locators, which callers add according to their representation.
    pub(crate) fn retained_bytes(&self) -> u64 {
        (size_of::<Self>() as u64).saturating_add(self.value.retained_heap_bytes())
    }

    pub fn cmp_in(&self, other: &Self, index: IndexOrder) -> Ordering {
        let ordering = match index {
            IndexOrder::Eavt => self
                .entity
                .cmp(&other.entity)
                .then(self.attribute.cmp(&other.attribute))
                .then_with(|| self.value.index_cmp(&other.value)),
            IndexOrder::Aevt => self
                .attribute
                .cmp(&other.attribute)
                .then(self.entity.cmp(&other.entity))
                .then_with(|| self.value.index_cmp(&other.value)),
            IndexOrder::Avet => self
                .attribute
                .cmp(&other.attribute)
                .then_with(|| self.value.index_cmp(&other.value))
                .then(self.entity.cmp(&other.entity)),
            IndexOrder::Vaet => self
                .value
                .index_cmp(&other.value)
                .then(self.attribute.cmp(&other.attribute))
                .then(self.entity.cmp(&other.entity)),
            IndexOrder::Saevt => self
                .source
                .cmp(&other.source)
                .then(self.attribute.cmp(&other.attribute))
                .then(self.entity.cmp(&other.entity))
                .then_with(|| self.value.index_cmp(&other.value)),
            IndexOrder::Savet => self
                .source
                .cmp(&other.source)
                .then(self.attribute.cmp(&other.attribute))
                .then_with(|| self.value.index_cmp(&other.value))
                .then(self.entity.cmp(&other.entity)),
        };
        ordering
            .then_with(|| other.tx.cmp(&self.tx))
            .then(self.source.cmp(&other.source))
            .then_with(|| other.added.cmp(&self.added))
            // The recovered comparator orders a logical E/A/V group by
            // descending transaction and operation. Retain a final physical
            // tie-break only for two otherwise identical stored coordinates,
            // e.g. BigDecimal scale variants in one transaction.
            .then_with(|| self.value.stored_cmp(&other.value))
    }
}
