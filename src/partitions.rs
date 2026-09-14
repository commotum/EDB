//! Shared transaction-local partition policy. No entity IDs are allocated here.
use crate::{EntityRef, Keyword, Schema, SemanticError, TxOp, TxValue, eid_to_eidx, eid_to_part};
use std::collections::{BTreeMap, BTreeSet};

/// Explicit execution defaults, separate from transaction data and resource
/// limits. Existing APIs use the user partition. This policy affects only new
/// domain entities without a stronger placement directive or existing identity.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TransactionDefaults {
    default_partition: Option<Keyword>,
}

impl TransactionDefaults {
    pub fn with_default_partition(mut self, partition: Keyword) -> Self {
        self.default_partition = Some(partition);
        self
    }

    pub fn default_partition(&self) -> Option<&Keyword> {
        self.default_partition.as_ref()
    }

    pub(crate) fn resolve(
        &self,
        schema: &Schema,
        resolve: impl FnOnce(&Keyword) -> Option<u64>,
    ) -> Result<u32, SemanticError> {
        let Some(name) = &self.default_partition else {
            return Ok(crate::USER_PARTITION);
        };
        let entity = resolve(name).ok_or_else(|| {
            SemanticError::incorrect(
                "transaction/default-partition-not-found",
                format!(
                    "configured default partition {} is not installed",
                    name.qualified_name()
                ),
            )
        })?;
        let bits = partition_bits(entity)
            .and_then(|bits| {
                schema.validate_partition_bits(bits)?;
                Ok(bits)
            })
            .map_err(|_| {
                SemanticError::incorrect(
                    "transaction/invalid-default-partition",
                    format!(
                        "configured default {} is not a partition",
                        name.qualified_name()
                    ),
                )
            })?;
        if bits < crate::USER_PARTITION {
            return Err(SemanticError::incorrect(
                "transaction/invalid-default-partition",
                "the default for application entities cannot be a reserved system or transaction partition",
            ));
        }
        Ok(bits)
    }
}

enum Affinity {
    Temp(String),
    Bits(u32),
}

pub(crate) struct PartitionPolicy {
    forced: BTreeMap<String, u32>,
    affinity: BTreeMap<String, Affinity>,
    system: BTreeSet<String>,
    resolved: BTreeMap<String, u32>,
    default_partition: u32,
}

impl PartitionPolicy {
    pub(crate) fn with_default(
        ops: &[TxOp],
        names: &BTreeSet<String>,
        schema: &Schema,
        default_partition: u32,
        mut resolve: impl FnMut(&EntityRef) -> Result<u64, SemanticError>,
    ) -> Result<Self, SemanticError> {
        let mut result = Self {
            forced: BTreeMap::new(),
            affinity: BTreeMap::new(),
            system: BTreeSet::new(),
            resolved: BTreeMap::new(),
            default_partition,
        };
        for op in ops {
            match op {
                TxOp::Add {
                    entity: EntityRef::Temp(name),
                    attribute,
                    ..
                } if matches!(
                    u64::from(*attribute),
                    crate::DB_VALUE_TYPE
                        | crate::DB_CARDINALITY
                        | crate::DB_UNIQUE
                        | crate::DB_IS_COMPONENT
                        | crate::DB_INDEX
                        | crate::DB_NO_HISTORY
                        | crate::DB_FULLTEXT
                        | crate::DB_TUPLE_TYPE
                        | crate::DB_TUPLE_TYPES
                        | crate::DB_TUPLE_ATTRS
                        | crate::DB_ATTR_PREDS
                        | crate::DB_TUPLE_DISCONTINUED
                ) =>
                {
                    result.system.insert(name.clone());
                }
                TxOp::Add {
                    attribute,
                    value: TxValue::Entity(EntityRef::Temp(name)),
                    ..
                } if matches!(
                    u64::from(*attribute),
                    crate::DB_INSTALL_PARTITION
                        | crate::DB_INSTALL_ATTRIBUTE
                        | crate::DB_ALTER_ATTRIBUTE
                ) =>
                {
                    result.system.insert(name.clone());
                }
                _ => {}
            }
        }
        for op in ops {
            match op {
                TxOp::ForcePartition { tempid, partition } => {
                    require_tempid(names, tempid)?;
                    let bits = partition_bits(resolve(partition)?)?;
                    schema.validate_partition_bits(bits)?;
                    if result.system.contains(tempid) {
                        continue;
                    }
                    if result
                        .forced
                        .insert(tempid.clone(), bits)
                        .is_some_and(|prior| prior != bits)
                    {
                        return Err(conflict("one tempid requests distinct forced partitions"));
                    }
                }
                TxOp::MatchPartition { tempid, entity } => {
                    require_tempid(names, tempid)?;
                    let affinity = if let EntityRef::Temp(name) = entity {
                        require_tempid(names, name)?;
                        Affinity::Temp(name.clone())
                    } else {
                        Affinity::Bits(eid_to_part(resolve(entity)?)?)
                    };
                    if result.system.contains(tempid) {
                        continue;
                    }
                    if let Some(prior) = result.affinity.get(tempid) {
                        let equal = match (prior, &affinity) {
                            (Affinity::Temp(a), Affinity::Temp(b)) => a == b,
                            (Affinity::Bits(a), Affinity::Bits(b)) => a == b,
                            _ => false,
                        };
                        if !equal {
                            return Err(conflict(
                                "one tempid requests distinct partition affinities",
                            ));
                        }
                    } else {
                        result.affinity.insert(tempid.clone(), affinity);
                    }
                }
                _ => {}
            }
        }
        // Check policies even when upsert later makes allocation unnecessary.
        for name in names {
            if result.system.contains(name)
                || result.forced.contains_key(name)
                || result.affinity.contains_key(name)
            {
                result.resolve_partition(name)?;
            }
        }
        Ok(result)
    }

    fn resolve_partition(&mut self, name: &str) -> Result<u32, SemanticError> {
        let mut seen = BTreeSet::new();
        let mut path = Vec::new();
        let mut current = name;
        // Cache the whole traversed suffix, so a long chain is visited once
        // rather than once per tempid. Iteration also bounds stack usage.
        let bits = loop {
            if let Some(bits) = self.resolved.get(current) {
                break *bits;
            }
            if self.system.contains(current) {
                break crate::DB_PARTITION;
            }
            if let Some(bits) = self.forced.get(current) {
                break *bits;
            }
            path.push(current);
            if !seen.insert(current) {
                return Err(SemanticError::incorrect(
                    "transaction/partition-affinity-cycle",
                    "partition affinity contains an unanchored cycle",
                ));
            }
            match self.affinity.get(current) {
                Some(Affinity::Temp(next)) => {
                    current = next;
                }
                // Recovered matching excludes reserved system/tx targets.
                Some(Affinity::Bits(bits)) if *bits >= crate::USER_PARTITION => break *bits,
                _ => break self.default_partition,
            }
        };
        for name in path {
            self.resolved.insert(name.to_owned(), bits);
        }
        self.resolved.insert(name.to_owned(), bits);
        Ok(bits)
    }

    pub(crate) fn explicit_partition(&self, name: &str) -> Result<Option<u32>, SemanticError> {
        if self.system.contains(name)
            || self.forced.contains_key(name)
            || self.affinity.contains_key(name)
        {
            Ok(Some(
                *self
                    .resolved
                    .get(name)
                    .expect("every explicit partition policy was resolved"),
            ))
        } else {
            Ok(None)
        }
    }
}

fn require_tempid(names: &BTreeSet<String>, name: &str) -> Result<(), SemanticError> {
    if names.contains(name) {
        Ok(())
    } else {
        Err(SemanticError::incorrect(
            "transaction/partition-tempid-not-an-entity",
            format!("partition policy tempid {name} has no entity position"),
        ))
    }
}

pub(crate) fn partition_bits(entity: u64) -> Result<u32, SemanticError> {
    let bits = eid_to_part(entity)?;
    let index = eid_to_eidx(entity)?;
    if bits == crate::DB_PARTITION {
        u32::try_from(index)
            .ok()
            .filter(|bits| *bits <= crate::MAX_PARTITION)
            .ok_or_else(|| {
                SemanticError::incorrect(
                    "transaction/not-a-partition",
                    "partition entity is outside partition bit range",
                )
            })
    } else if index == 0 {
        Ok(bits)
    } else {
        Err(SemanticError::incorrect(
            "transaction/not-a-partition",
            "partition must name a system entity or an implicit partition base",
        ))
    }
}

fn conflict(message: &str) -> SemanticError {
    SemanticError::conflict("transaction/partition-conflict", message)
}
