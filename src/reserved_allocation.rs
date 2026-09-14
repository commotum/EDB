//! Retained high-water state for partition-zero entity issuance.
//!
//! This is allocation history, not derived schema state: an acknowledged
//! tempid or a typed reference can reserve an identity without leaving any
//! current entity facts. Callers retain this state through immutable values,
//! canonical replay, and physical excision. The ordinary frontier remains an
//! upper bound, but is never the starting point for reserved allocation.

use crate::{
    DB_PARTITION, Datom, DurableTransaction, INITIAL_EIDX_FRONTIER, MAX_EIDX, SemanticError, Value,
    eid_to_eidx, eid_to_part, make_eid,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ReservedAllocation {
    frontier: u64,
}

impl ReservedAllocation {
    pub(crate) const fn initial() -> Self {
        Self {
            frontier: INITIAL_EIDX_FRONTIER,
        }
    }

    /// Reconstitute an authenticated current checkpoint.
    pub(crate) fn from_frontier(
        frontier: u64,
        ordinary_frontier: u64,
    ) -> Result<Self, SemanticError> {
        crate::model::identity::validate_frontier(frontier)?;
        crate::model::identity::validate_frontier(ordinary_frontier)?;
        if frontier > ordinary_frontier {
            return Err(SemanticError::incorrect(
                "allocation/reserved-frontier-out-of-range",
                "reserved allocation frontier exceeds the ordinary issued frontier",
            ));
        }
        Ok(Self { frontier })
    }

    pub(crate) const fn frontier(self) -> u64 {
        self.frontier
    }

    /// Reserve every previously observed partition-zero ID, including ones
    /// above schema/partition installation capacity. Clamping would permit
    /// reuse or hide genuine exhaustion.
    pub(crate) fn observe_entity(&mut self, entity: u64) -> Result<(), SemanticError> {
        if eid_to_part(entity)? == DB_PARTITION {
            self.frontier = self.frontier.max(eid_to_eidx(entity)? + 1);
        }
        Ok(())
    }

    pub(crate) fn observe_attribute(&mut self, attribute: u32) -> Result<(), SemanticError> {
        self.observe_entity(u64::from(attribute))
    }

    /// Only typed references are identities. Numeric values, including tuple
    /// numeric slots, must not consume reserved IDs. The explicit stack keeps
    /// programmatically constructed nested values off the Rust call stack.
    pub(crate) fn observe_value(&mut self, value: &Value) -> Result<(), SemanticError> {
        let mut next = *self;
        let mut pending = Vec::new();
        let mut current = Some(value);
        loop {
            if let Some(value) = current.take() {
                match value {
                    Value::Ref(entity) => next.observe_entity(*entity)?,
                    Value::Tuple(slots) => pending.push(slots.iter().flatten()),
                    _ => {}
                }
            }
            loop {
                let Some(values) = pending.last_mut() else {
                    *self = next;
                    return Ok(());
                };
                if let Some(value) = values.next() {
                    current = Some(value);
                    break;
                }
                pending.pop();
            }
        }
    }

    pub(crate) fn observe_datoms<'a>(
        &mut self,
        datoms: impl IntoIterator<Item = &'a Datom>,
    ) -> Result<(), SemanticError> {
        let mut next = *self;
        for datom in datoms {
            next.observe_entity(datom.entity)?;
            next.observe_attribute(datom.attribute)?;
            next.observe_value(&datom.value)?;
        }
        *self = next;
        Ok(())
    }

    /// Replay includes receipt-only allocation witnesses, not just datoms.
    pub(crate) fn observe_transaction(
        &mut self,
        transaction: &DurableTransaction,
    ) -> Result<(), SemanticError> {
        let mut next = *self;
        next.observe_datoms(&transaction.tx_data)?;
        for entity in transaction.tempids.values() {
            next.observe_entity(*entity)?;
        }
        *self = next;
        Ok(())
    }

    /// Issue from the retained reserved cursor. Schema/named-partition range
    /// checks remain with their owning validators. Before publication callers
    /// raise the ordinary frontier to at least `self.frontier()`.
    pub(crate) fn allocate(&mut self) -> Result<u64, SemanticError> {
        if self.frontier > MAX_EIDX {
            return Err(SemanticError::incorrect(
                "allocation/reserved-exhausted",
                "reserved entity-index space is exhausted",
            ));
        }
        let entity = make_eid(DB_PARTITION, self.frontier)?;
        self.frontier += 1;
        Ok(entity)
    }
}
