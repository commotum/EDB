//! Generic authority guards for Rust-owned publication and protection.
//!
//! Publishing a successor compares the descriptor carrying the writer epoch.
//! Readers capture immutable values without mutating references. Persistent
//! reference tombstones prevent delete/recreate ABA; absence is not a tombstone.
//!
//! The Rust collector announces an epoch through an opaque reference. Root
//! mutations atomically append before/after ownership changes under that guard.
//! Sealing fixes the change set for incremental reachability accounting.
//! Protected puts refresh even reused objects; guarded deletion checks
//! the collector's authority and an older protection epoch. Durable roots,
//! retention grace and those barriers protect objects without a global lock during graph
//! traversal. This module supplies validation, not GC orchestration or policy:
//! the engine classifies roots and accounts for reachability and retention.

use super::{MAX_BATCH, MAX_REF_BYTES, Reference, validate_key, validate_limit};
use crate::SemanticError;
use std::collections::BTreeSet;

/// Expected state of one opaque reference, checked atomically with all changes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefCondition {
    pub key: String,
    /// `None` requires that the key has never been created. A tombstone has a
    /// revision and must instead be matched using `Some(revision)`.
    pub expected: Option<u64>,
}

/// Replacement bytes for a guarded reference. Every changed key needs its own
/// condition; additional conditions may guard authority without changing it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefChange {
    pub key: String,
    /// `None` writes a revisioned tombstone; it does not remove the reference.
    pub value: Option<Vec<u8>>,
}

/// An atomic batch either applies every change or changes nothing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BatchOutcome {
    /// Changed references, with their resulting revisions, in change order.
    Applied(Vec<(String, Reference)>),
    /// Observed states for all guarded keys, including absent keys. Consumers
    /// should match by key rather than depend on an ordering of conflicts.
    Conflict(Vec<(String, Option<Reference>)>),
}

const MAX_BATCH_REF_BYTES: usize = 8 * 1024 * 1024;

/// Validate caller-owned data before I/O or payload cloning. Empty changes are
/// valid for guard-only object operations; an empty condition set is not.
pub(super) fn validate_batch(
    conditions: &[RefCondition],
    changes: &[RefChange],
) -> Result<(), SemanticError> {
    validate_limit(conditions.len())?;
    if changes.len() > MAX_BATCH {
        validate_limit(changes.len())?;
    }
    let mut guards = BTreeSet::new();
    for condition in conditions {
        validate_key(&condition.key)?;
        if !guards.insert(condition.key.as_str()) {
            return Err(SemanticError::incorrect(
                "storage/duplicate-guard",
                "Each guarded reference key must occur exactly once",
            ));
        }
        if condition
            .expected
            .is_some_and(|revision| revision == 0 || revision > i64::MAX as u64)
        {
            return Err(SemanticError::incorrect(
                "storage/invalid-revision",
                "Expected reference revisions must be between 1 and PostgreSQL bigint maximum",
            ));
        }
    }
    let mut changed = BTreeSet::new();
    let mut bytes = 0_usize;
    for change in changes {
        validate_key(&change.key)?;
        if !changed.insert(change.key.as_str()) {
            return Err(SemanticError::incorrect(
                "storage/duplicate-change",
                "Each changed reference key must occur exactly once",
            ));
        }
        if !guards.contains(change.key.as_str()) {
            return Err(SemanticError::incorrect(
                "storage/unguarded-change",
                "Every changed reference key must have a matching condition",
            ));
        }
        let value_bytes = change.value.as_ref().map_or(0, Vec::len);
        if value_bytes > MAX_REF_BYTES {
            return Err(SemanticError::incorrect(
                "storage/reference-size",
                "Reference payload exceeds the 1 MiB bound",
            ));
        }
        // Counts and individual payloads are already bounded, so addition
        // cannot overflow even on a 32-bit target.
        bytes += value_bytes;
        if bytes > MAX_BATCH_REF_BYTES {
            return Err(SemanticError::incorrect(
                "storage/reference-batch-size",
                "Combined reference payloads exceed the 8 MiB batch bound",
            ));
        }
    }
    Ok(())
}
