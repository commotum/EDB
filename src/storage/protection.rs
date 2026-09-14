//! Shared collection-epoch guards for immutable preparation.
//! Catalog, maintenance, backup and writer callers use this barrier without
//! importing writer scheduling or lease ownership. SQL sees opaque references.
use super::{PgBlockStore, RefCondition, Reference, WriteProtection};
use crate::{ErrorCategory, SemanticError};

pub(crate) const GC_REFERENCE: &str = "system/gc";

/// Epoch/revision belongs to the Rust collector protocol. No SQL decodes it.
pub(crate) fn protection(
    store: &mut PgBlockStore,
    guards: &[RefCondition],
) -> Result<WriteProtection, SemanticError> {
    let gc = store.read_ref(GC_REFERENCE)?;
    let epoch = match gc.as_ref().and_then(|r| r.value.as_deref()) {
        None => 0,
        Some(bytes) if bytes.len() == 8 => u64::from_be_bytes(bytes.try_into().unwrap()),
        Some(_) => {
            return Err(fault(
                "storage/gc-epoch",
                "Malformed current collection epoch",
            ));
        }
    };
    let mut conditions = guards.to_vec();
    let current = condition(GC_REFERENCE, gc.as_ref());
    if let Some(existing) = conditions.iter().find(|g| g.key == GC_REFERENCE) {
        if existing != &current {
            return Err(conflict(
                "storage/write-protection-conflict",
                "Collection epoch changed before immutable preparation",
            ));
        }
    } else {
        conditions.push(current);
    }
    Ok(WriteProtection { epoch, conditions })
}
fn condition(key: &str, reference: Option<&Reference>) -> RefCondition {
    RefCondition {
        key: key.to_owned(),
        expected: reference.map(|r| r.revision),
    }
}

fn fault(code: &'static str, message: &str) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, code, message)
}
fn conflict(code: &'static str, message: &str) -> SemanticError {
    SemanticError::conflict(code, message)
}
