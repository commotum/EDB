//! Explicit eager-oracle indexes; native values use durable and recent tiers.
use super::{IndexPrefix, compare_prefix};
use crate::{Datom, IndexOrder, Schema, SemanticError, Value, ValueType};
use std::sync::Arc;

/// Immutable roots for a single database value.
///
/// `Arc` gives eager values cheap clones. Durable database values use the
/// block-backed persistent trees and bounded recent tier instead.
#[derive(Clone, Debug, Default)]
pub(crate) struct IndexRoots {
    eavt: Arc<[Datom]>,
    aevt: Arc<[Datom]>,
    avet: Arc<[Datom]>,
    vaet: Arc<[Datom]>,
}

impl IndexRoots {
    pub(crate) fn build(schema: &Schema, datoms: impl IntoIterator<Item = Datom>) -> Self {
        let all: Vec<_> = datoms.into_iter().collect();
        let eavt = sorted(all.clone(), IndexOrder::Eavt);
        let aevt = sorted(all.clone(), IndexOrder::Aevt);
        let avet = sorted(
            all.iter()
                .filter(|datom| {
                    schema
                        .attribute(datom.attribute)
                        .is_ok_and(|attribute| attribute.indexed || attribute.unique.is_some())
                })
                .cloned()
                .collect(),
            IndexOrder::Avet,
        );
        let vaet = sorted(
            all.into_iter()
                .filter(|datom| {
                    schema
                        .attribute(datom.attribute)
                        .is_ok_and(|attribute| attribute.value_type == ValueType::Ref)
                })
                .collect(),
            IndexOrder::Vaet,
        );
        Self {
            eavt: eavt.into(),
            aevt: aevt.into(),
            avet: avet.into(),
            vaet: vaet.into(),
        }
    }

    pub(crate) fn get(&self, order: IndexOrder) -> &[Datom] {
        match order {
            IndexOrder::Eavt => &self.eavt,
            IndexOrder::Aevt => &self.aevt,
            IndexOrder::Avet => &self.avet,
            IndexOrder::Vaet => &self.vaet,
        }
    }

    pub(crate) fn matching(&self, prefix: &IndexPrefix) -> Result<&[Datom], SemanticError> {
        prefix.validate()?;
        let datoms = self.get(prefix.order());
        let start = datoms.partition_point(|datom| compare_prefix(datom, prefix).is_lt());
        let len = datoms[start..].partition_point(|datom| compare_prefix(datom, prefix).is_eq());
        Ok(&datoms[start..start + len])
    }

    pub(crate) fn seek(&self, prefix: &IndexPrefix) -> Result<&[Datom], SemanticError> {
        prefix.validate()?;
        let datoms = self.get(prefix.order());
        let start = datoms.partition_point(|datom| compare_prefix(datom, prefix).is_lt());
        Ok(&datoms[start..])
    }

    pub(crate) fn reverse_seek(&self, prefix: &IndexPrefix) -> Result<Vec<Datom>, SemanticError> {
        prefix.validate()?;
        let datoms = self.get(prefix.order());
        let end = datoms.partition_point(|datom| !compare_prefix(datom, prefix).is_gt());
        Ok(datoms[..end].iter().rev().cloned().collect())
    }

    pub(crate) fn avet_range(
        &self,
        attribute: u32,
        start: Option<&Value>,
        end: Option<&Value>,
    ) -> &[Datom] {
        let datoms = self.get(IndexOrder::Avet);
        let lower = datoms.partition_point(|datom| {
            datom.attribute < attribute
                || (datom.attribute == attribute
                    && start.is_some_and(|start| datom.value.index_cmp(start).is_lt()))
        });
        let upper = datoms.partition_point(|datom| {
            datom.attribute < attribute
                || (datom.attribute == attribute
                    && end.is_none_or(|end| datom.value.index_cmp(end).is_lt()))
        });
        &datoms[lower..upper]
    }
}

fn sorted(mut datoms: Vec<Datom>, order: IndexOrder) -> Vec<Datom> {
    datoms.sort_by(|left, right| left.cmp_in(right, order));
    datoms
}
