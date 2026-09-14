//! Historical ident assertions projected into the lookup map used by a value.

use crate::{Datom, ErrorCategory, Keyword, SemanticError, Value};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::mem::size_of;

/// Discardable bidirectional index derived from ordinary `:db/ident`
/// assertions.
///
/// The recovered peer deliberately rebuilds this index from every historical
/// assertion, in transaction order. Retractions do not erase a name, which is
/// what preserves old names as aliases after a rename. A later assertion of
/// the same keyword wins and therefore permits the documented repurposing of
/// an old alias.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct IdentIndex {
    by_ident: BTreeMap<Keyword, u64>,
    by_entity: BTreeMap<u64, Keyword>,
}

impl IdentIndex {
    pub(crate) fn derive<'a>(
        datoms: impl IntoIterator<Item = &'a Datom>,
        ident_attribute: u32,
    ) -> Result<Self, SemanticError> {
        let mut assertions: Vec<_> = datoms
            .into_iter()
            .filter(|datom| datom.added && datom.attribute == ident_attribute)
            .collect();
        assertions.sort_by(|left, right| {
            left.tx
                .cmp(&right.tx)
                .then(left.entity.cmp(&right.entity))
                .then_with(|| left.value.stored_cmp(&right.value))
                .then_with(|| {
                    // Keep the order total even if corrupt input repeats the
                    // same E/A/V with different physical flags in the future.
                    left.added.cmp(&right.added)
                })
        });

        let mut result = Self::default();
        for datom in assertions {
            result.apply_assertion(datom, ident_attribute)?;
        }
        Ok(result)
    }

    /// Apply one chronologically ordered `:db/ident` assertion to the
    /// discardable lookup maps. Retractions deliberately never reach this
    /// function: recovered `key-hook` rebuild also folds only assertions, so
    /// old names remain aliases and a later assertion can repurpose a name.
    pub(crate) fn apply_assertion(
        &mut self,
        datom: &Datom,
        ident_attribute: u32,
    ) -> Result<(), SemanticError> {
        if !datom.added || datom.attribute != ident_attribute {
            return Err(SemanticError::new(
                ErrorCategory::Fault,
                "schema/invalid-ident-assertion",
                "ident lookup reconstruction accepts only added :db/ident datoms",
            ));
        }
        let Value::Keyword(ident) = &datom.value else {
            return Err(SemanticError::new(
                ErrorCategory::Fault,
                "schema/invalid-ident-value",
                ":db/ident assertions must contain keyword values",
            ));
        };
        self.by_ident.insert(ident.clone(), datom.entity);
        self.by_entity.insert(datom.entity, ident.clone());
        Ok(())
    }

    pub(crate) fn resolve(&self, ident: &Keyword) -> Option<u64> {
        self.by_ident.get(ident).copied()
    }

    pub(crate) fn ident(&self, entity: u64) -> Option<&Keyword> {
        self.by_entity.get(&entity)
    }

    pub(crate) fn aliases(&self) -> impl Iterator<Item = (&Keyword, u64)> {
        self.by_ident.iter().map(|(ident, entity)| (ident, *entity))
    }

    pub(crate) fn name_count(&self) -> usize {
        self.by_ident.len()
    }

    pub(crate) fn entity_count(&self) -> usize {
        self.by_entity.len()
    }

    /// Allocator-independent account of retained map keys/values and owned
    /// keyword buffers. B-tree node headers and allocator metadata are
    /// intentionally excluded, so callers label this an estimate rather than
    /// process RSS.
    pub(crate) fn estimated_retained_bytes(&self) -> u64 {
        fn keyword_bytes(keyword: &Keyword) -> u64 {
            (size_of::<Keyword>() as u64)
                .saturating_add(
                    keyword
                        .namespace
                        .as_ref()
                        .map_or(0, |namespace| namespace.capacity() as u64),
                )
                .saturating_add(keyword.name.capacity() as u64)
        }

        let by_ident = self.by_ident.keys().fold(0_u64, |bytes, ident| {
            bytes
                .saturating_add(keyword_bytes(ident))
                .saturating_add(size_of::<u64>() as u64)
        });
        let by_entity = self.by_entity.values().fold(0_u64, |bytes, ident| {
            bytes
                .saturating_add(size_of::<u64>() as u64)
                .saturating_add(keyword_bytes(ident))
        });
        (size_of::<Self>() as u64)
            .saturating_add(by_ident)
            .saturating_add(by_entity)
    }

    pub(crate) fn cmp_observation(&self, other: &Self) -> Ordering {
        self.by_ident
            .cmp(&other.by_ident)
            .then_with(|| self.by_entity.cmp(&other.by_entity))
    }
}
