//! Pure, source-faithful excision predicate planning.
//!
//! An excision request is ordinary immutable database information. This module
//! only decides which historical datoms a background copy-on-write rewrite
//! removes; it owns no SQL, publication, or synchronization state.

use crate::database::FrozenExcisionRequest;
use crate::{
    DB_PARTITION, Datom, Digest, ErrorCategory, SemanticError, Value, eid_to_part, sha256, tx_to_t,
};
use std::collections::BTreeSet;

/// Exact values of recovered 1.0.7705 `datomic.db/BOOT-IDS`.
///
/// `datomic.excise/keeper?` preserves a datom when its entity is in partition
/// zero or its attribute is one of these ids (`excise.clj:139-147`). This is
/// intentionally not a numeric range: native ids 54--72 are useful design
/// evidence but were not members of the recovered keeper set.
pub(crate) const RECOVERED_BOOT_IDS: &[u64] = &[
    0, 1, 2, 3, 4, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27,
    35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53,
];

pub(crate) fn is_excision_keeper(datom: &Datom) -> bool {
    eid_to_part(datom.entity).is_ok_and(|partition| partition == DB_PARTITION)
        || RECOVERED_BOOT_IDS
            .binary_search(&u64::from(datom.attribute))
            .is_ok()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExcisionTargetKind {
    Entity,
    Attribute,
}

#[derive(Clone, Debug)]
struct ExcisionPredicate {
    request: FrozenExcisionRequest,
    kind: ExcisionTargetKind,
    before_t: u64,
    /// Target plus recursively owned component entities, as-of request_t.
    extent: BTreeSet<u64>,
    reference_attributes: BTreeSet<u32>,
    protected_entity_target: bool,
}

/// Canonical OR-union of every pending predicate in one background batch.
/// Overlapping requests remove a datom once, matching recovered
/// `create-xpreds`/`create-e->xpreds`/`create-a->xpreds`
/// (`excise.clj:331-428`).
#[derive(Clone, Debug, Default)]
pub(crate) struct ExcisionPlan {
    predicates: Vec<ExcisionPredicate>,
}

/// Fully frozen material predicate persisted beside a COW generation build.
///
/// Raw A=15 facts remain the semantic/audit authority. This projection also
/// records every current-schema choice made while planning (target kind,
/// effective cutoff, component extent, and reference classification), so a
/// crash cannot resume an old build under a newly interpreted predicate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlannedExcisionPredicate {
    pub(crate) request: FrozenExcisionRequest,
    pub(crate) kind: ExcisionTargetKind,
    pub(crate) before_t: u64,
    pub(crate) extent: BTreeSet<u64>,
    pub(crate) reference_attributes: BTreeSet<u32>,
    pub(crate) protected_entity_target: bool,
    pub(crate) hash: Digest,
}

impl ExcisionPlan {
    /// Construct/check the persisted material predicate without replaying an
    /// eager database. A zero hash is used only by the initial Rust planner;
    /// persisted callers require and supply the previously sealed hash.
    pub(crate) fn from_materialized(
        items: Vec<PlannedExcisionPredicate>,
    ) -> Result<Self, SemanticError> {
        let mut predicates = Vec::with_capacity(items.len());
        let mut previous = None;
        for item in items {
            let identity = (item.request.request_t, item.request.request_entity);
            if previous.is_some_and(|p| p >= identity) || item.before_t > item.request.request_t {
                return Err(SemanticError::new(
                    ErrorCategory::Fault,
                    "excision/plan-order",
                    "Frozen predicates are not canonical",
                ));
            }
            previous = Some(identity);
            let predicate = ExcisionPredicate {
                request: item.request,
                kind: item.kind,
                before_t: item.before_t,
                extent: item.extent,
                reference_attributes: item.reference_attributes,
                protected_entity_target: item.protected_entity_target,
            };
            if item.hash != [0; 32] && predicate.canonical_hash() != item.hash {
                return Err(SemanticError::new(
                    ErrorCategory::Fault,
                    "excision/plan-hash",
                    "Frozen predicate differs from its commitment",
                ));
            }
            predicates.push(predicate);
        }
        Ok(Self { predicates })
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.predicates.is_empty()
    }

    pub(crate) fn predicates_len(&self) -> usize {
        self.predicates.len()
    }

    pub(crate) fn frozen_predicates(&self) -> Vec<PlannedExcisionPredicate> {
        self.predicates
            .iter()
            .map(ExcisionPredicate::frozen)
            .collect()
    }

    pub(crate) fn removes(&self, datom: &Datom) -> bool {
        self.predicates
            .iter()
            .any(|predicate| predicate.removes(datom))
    }
}

impl ExcisionPredicate {
    fn frozen(&self) -> PlannedExcisionPredicate {
        PlannedExcisionPredicate {
            request: self.request.clone(),
            kind: self.kind,
            before_t: self.before_t,
            extent: self.extent.clone(),
            reference_attributes: self.reference_attributes.clone(),
            protected_entity_target: self.protected_entity_target,
            hash: self.canonical_hash(),
        }
    }

    fn canonical_hash(&self) -> Digest {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"atomic/excision-predicate/v1\0");
        bytes.extend_from_slice(&self.request.request_entity.to_be_bytes());
        bytes.extend_from_slice(&self.request.request_t.to_be_bytes());
        bytes.extend_from_slice(&self.request.target.to_be_bytes());
        match self.request.cutoff {
            None => bytes.push(0),
            Some(crate::database::ExcisionCutoff::BeforeT(t)) => {
                bytes.push(1);
                bytes.extend_from_slice(&t.to_be_bytes());
            }
            Some(crate::database::ExcisionCutoff::BeforeInstant(instant)) => {
                bytes.push(2);
                bytes.extend_from_slice(&instant.to_be_bytes());
            }
        }
        bytes.push(match self.kind {
            ExcisionTargetKind::Entity => 0,
            ExcisionTargetKind::Attribute => 1,
        });
        bytes.extend_from_slice(&self.before_t.to_be_bytes());
        bytes.push(u8::from(self.protected_entity_target));
        bytes.extend_from_slice(&(self.request.attributes.len() as u64).to_be_bytes());
        for attribute in &self.request.attributes {
            bytes.extend_from_slice(&attribute.to_be_bytes());
        }
        bytes.extend_from_slice(&(self.extent.len() as u64).to_be_bytes());
        for entity in &self.extent {
            bytes.extend_from_slice(&entity.to_be_bytes());
        }
        bytes.extend_from_slice(&(self.reference_attributes.len() as u64).to_be_bytes());
        for attribute in &self.reference_attributes {
            bytes.extend_from_slice(&attribute.to_be_bytes());
        }
        sha256(&bytes)
    }

    fn removes(&self, datom: &Datom) -> bool {
        if is_excision_keeper(datom) || tx_to_t(datom.tx).map_or(true, |t| t >= self.before_t) {
            return false;
        }
        match self.kind {
            ExcisionTargetKind::Attribute => u64::from(datom.attribute) == self.request.target,
            ExcisionTargetKind::Entity => {
                let reference = self.reference_attributes.contains(&datom.attribute)
                    && matches!(datom.value, Value::Ref(_));
                let references = |entity| {
                    reference && matches!(datom.value, Value::Ref(value) if value == entity)
                };
                let selected_target_attribute = self.request.attributes.is_empty()
                    || self
                        .request
                        .attributes
                        .contains(&u64::from(datom.attribute));
                let target_fact = selected_target_attribute
                    && (datom.entity == self.request.target || references(self.request.target));
                let component_fact =
                    self.extent.contains(&datom.entity) && datom.entity != self.request.target;
                let component_reference = reference
                    && matches!(datom.value, Value::Ref(value) if value != self.request.target && self.extent.contains(&value));
                target_fact || component_fact || component_reference
            }
        }
    }
}
