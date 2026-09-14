//! Reclaimable report continuity across excision generations.
//!
//! A generation handoff retains its final publication for the configured grace
//! period. Readers never register interest or mutate these references.
use super::root::{Block, DatabaseRoot};
use super::{BatchOutcome, ObjectId, PgBlockStore, RefChange, RefCondition};
use crate::{ErrorCategory, SemanticError};
use std::collections::BTreeMap;

const HANDOFF_KIND: u16 = 91;
const PREFIX: &str = "handoffs/";
const CURSOR: &str = "ownership/handoff-prune";

#[derive(Clone, Debug, Default)]
pub struct ReportHandoffMaintenance {
    pub examined: usize,
    pub removed: usize,
    pub complete: bool,
}

pub(crate) fn handoff_key(route: [u8; 16], generation: u64) -> String {
    format!(
        "{PREFIX}{}/{generation:016x}",
        crate::storage::catalog::identity_string(route)
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReportHandoff {
    pub route: [u8; 16],
    pub identity: [u8; 16],
    pub generation: u64,
    pub basis: u64,
    pub publication: ObjectId,
    pub created_ms: u64,
}

impl ReportHandoff {
    fn encode(&self) -> Result<Vec<u8>, SemanticError> {
        self.validate()?;
        let mut payload = Vec::with_capacity(56);
        payload.extend_from_slice(&self.route);
        payload.extend_from_slice(&self.identity);
        payload.extend_from_slice(&self.generation.to_be_bytes());
        payload.extend_from_slice(&self.basis.to_be_bytes());
        payload.extend_from_slice(&self.created_ms.to_be_bytes());
        Block {
            kind: HANDOFF_KIND,
            links: vec![self.publication],
            payload,
        }
        .encode()
    }

    pub(crate) fn decode(id: &ObjectId, bytes: &[u8]) -> Result<Self, SemanticError> {
        // Fixed-size codec admission precedes Block's payload/link allocations.
        if bytes.len() != 20 + 32 + 56 {
            return Err(invalid("Invalid report handoff length"));
        }
        let block = Block::decode(id, bytes)?;
        if block.kind != HANDOFF_KIND || block.links.len() != 1 || block.payload.len() != 56 {
            return Err(invalid("Invalid report handoff shape"));
        }
        let result = Self {
            route: block.payload[..16].try_into().unwrap(),
            identity: block.payload[16..32].try_into().unwrap(),
            generation: u64::from_be_bytes(block.payload[32..40].try_into().unwrap()),
            basis: u64::from_be_bytes(block.payload[40..48].try_into().unwrap()),
            publication: block.links[0],
            created_ms: u64::from_be_bytes(block.payload[48..56].try_into().unwrap()),
        };
        result.validate()?;
        Ok(result)
    }

    fn validate(&self) -> Result<(), SemanticError> {
        if self.route == [0; 16] || self.identity == [0; 16] || self.generation == u64::MAX {
            return Err(invalid(
                "Report handoff requires a valid route, lineage and generation",
            ));
        }
        Ok(())
    }
}

/// Stage under the caller's root/lease/GC write protection, then join the
/// returned reference transition to that exact excision activation batch.
pub(crate) fn prepare_handoff(
    store: &mut PgBlockStore,
    route: [u8; 16],
    identity: [u8; 16],
    generation: u64,
    basis: u64,
    publication: ObjectId,
) -> Result<(Vec<RefCondition>, Vec<RefChange>), SemanticError> {
    if store.write_protection().is_none() {
        return Err(invalid("Report handoff staging requires write protection"));
    }
    let key = handoff_key(route, generation);
    // Neither a previous activation nor its pruned tombstone can be recreated.
    if store.read_ref(&key)?.is_some() {
        return Err(SemanticError::conflict(
            "storage/report-handoff-conflict",
            "Report generation was already handed off",
        ));
    }
    let root = DatabaseRoot::decode(&publication, &required(store, publication)?)?;
    let metadata_id = root
        .metadata
        .ok_or_else(|| invalid("Report publication lacks generation metadata"))?;
    let metadata = super::SnapshotMetadata::decode(&metadata_id, &required(store, metadata_id)?)?;
    if (root.identity, root.basis) != (metadata.identity, metadata.basis)
        || (root.identity, metadata.generation, root.basis) != (identity, generation, basis)
    {
        return Err(invalid("Report handoff differs from its final publication"));
    }
    let id = store.put(
        &ReportHandoff {
            route,
            identity,
            generation,
            basis,
            publication,
            created_ms: now_ms()?,
        }
        .encode()?,
    )?;
    Ok((
        vec![RefCondition {
            key: key.clone(),
            expected: None,
        }],
        vec![RefChange {
            key,
            value: Some(id.to_vec()),
        }],
    ))
}

pub(crate) fn prune(
    store: &mut PgBlockStore,
    minimum_age: std::time::Duration,
    maximum: usize,
    control: &mut dyn FnMut() -> Result<(), SemanticError>,
) -> Result<ReportHandoffMaintenance, SemanticError> {
    if !(1..=32).contains(&maximum) {
        return Err(SemanticError::incorrect(
            "storage/report-prune-limit",
            "Report handoff pruning pages must contain 1–32 entries",
        ));
    }
    control()?;
    let cutoff =
        now_ms()?.saturating_sub(u64::try_from(minimum_age.as_millis()).unwrap_or(u64::MAX));
    let cursor = store.read_ref(CURSOR)?;
    let after = cursor
        .as_ref()
        .and_then(|r| r.value.as_deref())
        .unwrap_or(&[]);
    let after = std::str::from_utf8(after).map_err(|_| invalid("Invalid report handoff cursor"))?;
    if !after.is_empty() && (!after.starts_with(PREFIX) || after.len() > 1024) {
        return Err(invalid("Invalid report handoff cursor namespace"));
    }
    let page = store.list_live_refs(PREFIX, (!after.is_empty()).then_some(after), maximum)?;
    let mut report = ReportHandoffMaintenance {
        complete: page.len() < maximum,
        ..Default::default()
    };
    let mut guards = BTreeMap::from([(CURSOR.to_owned(), cursor.as_ref().map(|r| r.revision))]);
    let mut changes = Vec::with_capacity(maximum + 1);
    let mut last = String::new();
    for (key, reference) in page {
        control()?;
        let id: ObjectId = reference
            .value
            .as_deref()
            .unwrap()
            .try_into()
            .map_err(|_| invalid("Invalid report handoff reference"))?;
        let handoff = ReportHandoff::decode(&id, &required(store, id)?)?;
        if key != handoff_key(handoff.route, handoff.generation) {
            return Err(invalid("Report handoff differs from its reference key"));
        }
        report.examined += 1;
        if handoff.created_ms < cutoff {
            guards.insert(key.clone(), Some(reference.revision));
            changes.push(RefChange {
                key: key.clone(),
                value: None,
            });
            report.removed += 1;
        }
        last = key;
    }
    changes.push(RefChange {
        key: CURSOR.into(),
        value: Some(if report.complete {
            Vec::new()
        } else {
            last.into_bytes()
        }),
    });
    control()?;
    let guards = guards
        .into_iter()
        .map(|(key, expected)| RefCondition { key, expected })
        .collect::<Vec<_>>();
    match super::ownership::publish_refs(store, &guards, &changes)? {
        BatchOutcome::Applied(_) => Ok(report),
        BatchOutcome::Conflict(_) => Err(stale()),
    }
}

fn required(store: &mut PgBlockStore, id: ObjectId) -> Result<Vec<u8>, SemanticError> {
    store
        .get(id)?
        .ok_or_else(|| invalid("A report handoff object is absent"))
}
fn invalid(message: &str) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, "storage/report-handoff", message)
}
fn stale() -> SemanticError {
    SemanticError::conflict(
        "storage/collector-stale",
        "Report handoff ownership proof changed",
    )
}

fn now_ms() -> Result<u64, SemanticError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| invalid("Clock precedes Unix epoch"))?
        .as_millis();
    u64::try_from(now).map_err(|_| invalid("Clock exceeds storage time range"))
}
