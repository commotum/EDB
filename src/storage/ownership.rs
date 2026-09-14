//! Incremental engine-owned reachability, not PostgreSQL policy.
//!
//! Root transitions atomically record their before/after owners. A collector
//! seals an epoch, folds additions before removals, and retires only objects
//! with no owners. Newly protected puts cannot be swept by a sealed collector.
//! Immutable child metadata lets ownership reuse shared subgraphs and remove
//! old owners without re-reading live data. No SQL decodes any of these records.

use super::receipts::RequestIndex;
pub use super::report_handoff::ReportHandoffMaintenance;
use super::root::Block;
use super::{
    BatchOutcome, Guarded, ObjectId, PgBlockStore, RefChange, RefCondition, Reference,
    WriteProtection,
};
use crate::{ErrorCategory, SemanticError};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const EVENT_MAGIC: &[u8; 4] = b"ATOE";
const EVENT_PREFIX: &str = "ownership/events/";
pub(crate) const STATE_REF: &str = "ownership/state";
const CLOCK_REF: &str = "ownership/clock";
const PRUNE_REF: &str = "ownership/prune";
const STATE_KIND: u16 = 80;
const RECORD_KIND: u16 = 81;
const STACK_KIND: u16 = 82;
const STACK_ITEMS: usize = 128;
const WORK_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CollectionPhase {
    Adding,
    Removing,
    ProtectingMetadata,
    Sweeping,
    PruningRecords,
    ClearingEvents,
    Complete,
}
#[derive(Clone, Debug, Default)]
pub struct CollectionStats {
    pub ownership_steps: u64,
    pub newly_owned_objects: u64,
    pub ownership_payload_bytes: u64,
    pub metadata_objects_protected: u64,
    pub objects_examined: u64,
    pub objects_removed: u64,
    pub stored_bytes_removed: u64,
    pub events_removed: u64,
}
#[derive(Clone, Debug)]
pub struct CollectionProgress {
    pub phase: CollectionPhase,
    pub sealed_epoch: u64,
    pub stats: CollectionStats,
}
#[derive(Clone, Debug, Default)]
pub struct AuthorizationMaintenance {
    pub examined: usize,
    pub removed: usize,
    pub unsettled: bool,
    pub complete: bool,
}

/// An interrupted call resumes from its last atomic checkpoint. The work bound
/// counts graph/metadata/object steps, not merely DELETE statements. One legal
/// object may still be as large as the shared 64 MiB encoded-object ceiling.
pub struct BlockCollector {
    store: PgBlockStore,
}
impl BlockCollector {
    /// Retire one bounded page of report handoffs older than the read grace
    /// period. Its ownership deltas settle next cycle.
    pub fn prune_report_handoffs(
        &mut self,
        minimum_age: Duration,
        maximum: usize,
    ) -> Result<ReportHandoffMaintenance, SemanticError> {
        self.prune_report_handoffs_with_control(minimum_age, maximum, &mut || Ok(()))
    }

    pub fn prune_report_handoffs_with_control(
        &mut self,
        minimum_age: Duration,
        maximum: usize,
        control: &mut dyn FnMut() -> Result<(), SemanticError>,
    ) -> Result<ReportHandoffMaintenance, SemanticError> {
        super::report_handoff::prune(&mut self.store, minimum_age, maximum, control)
    }

    pub fn connect(config: &crate::PostgresConnectionConfig) -> Result<Self, SemanticError> {
        Ok(Self {
            store: PgBlockStore::connect(config)?,
        })
    }
    /// Read-only checkpoint inspection; does not seal or advance collection.
    pub fn status(&mut self) -> Result<Option<CollectionProgress>, SemanticError> {
        let Some(reference) = self.store.read_ref(STATE_REF)? else {
            return Ok(None);
        };
        let Some(id) = reference.value.as_deref() else {
            return Ok(None);
        };
        let state = State::load(&mut self.store, root_id(id)?)?;
        Ok(Some(CollectionProgress {
            phase: state.phase,
            sealed_epoch: state.epoch,
            stats: CollectionStats::default(),
        }))
    }
    /// One bounded page of old, otherwise-unowned read-index authorizations.
    /// Its durable cursor makes progress across invocations and databases. A
    /// publication creates normal ownership events for the next collection;
    /// it never changes user facts, receipt bases or the current index.
    pub fn prune_read_authorizations(
        &mut self,
        minimum_age: Duration,
        maximum: usize,
    ) -> Result<AuthorizationMaintenance, SemanticError> {
        if !(1..=32).contains(&maximum) {
            return Err(SemanticError::incorrect(
                "storage/authorization-prune-limit",
                "Pruning pages must contain 1–32 entries",
            ));
        }
        let checkpoint = self.store.read_ref(STATE_REF)?;
        let Some(id) = checkpoint.as_ref().and_then(|r| r.value.as_deref()) else {
            return Ok(AuthorizationMaintenance {
                unsettled: true,
                ..Default::default()
            });
        };
        if State::load(&mut self.store, root_id(id)?)?.phase != CollectionPhase::Complete
            || !self.store.list_live_refs(EVENT_PREFIX, None, 1)?.is_empty()
        {
            return Ok(AuthorizationMaintenance {
                unsettled: true,
                ..Default::default()
            });
        }
        let cursor_ref = self.store.read_ref(PRUNE_REF)?;
        let (key, after) = decode_prune_cursor(
            cursor_ref
                .as_ref()
                .and_then(|r| r.value.as_deref())
                .unwrap_or(&[]),
        )?;
        let publication = if after.is_some() {
            self.store
                .read_ref(&key)?
                .filter(|r| r.value.is_some())
                .map(|r| (key.clone(), r))
        } else {
            self.store
                .list_live_refs("databases/", (!key.is_empty()).then_some(key.as_str()), 1)?
                .into_iter()
                .next()
        };
        let mut guards = vec![
            condition(PRUNE_REF, cursor_ref.as_ref()),
            condition(STATE_REF, checkpoint.as_ref()),
        ];
        let Some((key, reference)) = publication else {
            applied(self.store.compare_exchange_many(
                &guards,
                &[RefChange {
                    key: PRUNE_REF.into(),
                    value: Some(Vec::new()),
                }],
            )?)?;
            return Ok(AuthorizationMaintenance {
                complete: true,
                ..Default::default()
            });
        };
        let id = root_id(reference.value.as_deref().unwrap())?;
        let mut root = super::root::DatabaseRoot::decode(&id, &required(&mut self.store, id)?)?;
        let cutoff =
            now_ms()?.saturating_sub(u64::try_from(minimum_age.as_millis()).unwrap_or(u64::MAX));
        let report = super::read_authorization::ReadAuthorization::prune_indexes(
            &mut self.store,
            &root,
            condition(&key, Some(&reference)),
            cutoff,
            after,
            maximum,
        )?;
        if report.unsettled {
            return Ok(AuthorizationMaintenance {
                examined: report.examined,
                unsettled: true,
                ..Default::default()
            });
        }
        for guard in report.guards {
            if let Some(existing) = guards.iter().find(|g| g.key == guard.key) {
                if existing != &guard {
                    return Err(SemanticError::conflict(
                        "storage/collector-stale",
                        "Authorization proof changed",
                    ));
                }
            } else {
                guards.push(guard);
            }
        }
        let mut changes = vec![RefChange {
            key: PRUNE_REF.into(),
            value: Some(encode_prune_cursor(
                &key,
                if report.complete { None } else { report.after },
            )),
        }];
        let protection = crate::storage::protection::protection(&mut self.store, &guards)?;
        guards = protection.conditions.clone();
        self.store.set_write_protection(Some(protection))?;
        let result = (|| {
            if report.removed != 0 {
                root.read_authorization = Some(report.authorization);
                let next = self.store.put(&root.encode()?)?;
                changes.push(RefChange {
                    key,
                    value: Some(next.to_vec()),
                });
            }
            applied(publish_refs(&mut self.store, &guards, &changes)?)?;
            Ok(AuthorizationMaintenance {
                examined: report.examined,
                removed: report.removed,
                ..Default::default()
            })
        })();
        let clear = self.store.set_write_protection(None);
        match (result, clear) {
            (Err(e), _) | (Ok(_), Err(e)) => Err(e),
            (Ok(r), Ok(())) => Ok(r),
        }
    }
    pub fn advance(
        &mut self,
        minimum_age: Duration,
        maximum_steps: usize,
    ) -> Result<CollectionProgress, SemanticError> {
        self.advance_with_control(minimum_age, maximum_steps, &mut || Ok(()))
    }
    pub fn advance_with_control(
        &mut self,
        minimum_age: Duration,
        maximum_steps: usize,
        control: &mut dyn FnMut() -> Result<(), SemanticError>,
    ) -> Result<CollectionProgress, SemanticError> {
        if !(1..=4096).contains(&maximum_steps) {
            return Err(SemanticError::incorrect(
                "storage/collection-budget",
                "Collection steps must be 1..=4096",
            ));
        }
        control()?;
        self.expire_staging(minimum_age, maximum_steps.min(32))?;
        // Bound repeated trie/stack metadata reads within this advance. A new
        // call starts cold; cross-checkpoint authentication is not hidden by a
        // forever-resident collector cache.
        self.store.reset_object_cache(WORK_BYTES);
        let previous = self.store.read_ref(STATE_REF)?;
        let mut state = match previous.as_ref().and_then(|r| r.value.as_deref()) {
            Some(id) => State::load(&mut self.store, root_id(id)?)?,
            None => State::empty(),
        };
        let gc = self
            .store
            .read_ref(crate::storage::protection::GC_REFERENCE)?;
        if state.phase == CollectionPhase::Complete {
            let current = epoch(&gc)?;
            let next = current
                .checked_add(1)
                .filter(|n| *n <= i64::MAX as u64)
                .ok_or_else(|| fault("Collection epochs exhausted"))?;
            state.epoch = current;
            state.phase = CollectionPhase::Adding;
            state.event_after = None;
            state.scan_after = None;
            state.frontier = None;
            state.frontier_offset = 0;
            state.cutoff_ms = now_ms()?
                .saturating_sub(u64::try_from(minimum_age.as_millis()).unwrap_or(u64::MAX));
            let guards = [
                condition(STATE_REF, previous.as_ref()),
                condition(crate::storage::protection::GC_REFERENCE, gc.as_ref()),
            ];
            self.store.set_write_protection(Some(WriteProtection {
                epoch: next,
                conditions: guards.to_vec(),
            }))?;
            let outcome = (|| {
                let id = state.put(&mut self.store)?;
                self.store.compare_exchange_many(
                    &guards,
                    &[
                        RefChange {
                            key: STATE_REF.into(),
                            value: Some(id.to_vec()),
                        },
                        RefChange {
                            key: crate::storage::protection::GC_REFERENCE.into(),
                            value: Some(next.to_be_bytes().to_vec()),
                        },
                    ],
                )
            })();
            self.store.set_write_protection(None)?;
            applied(outcome?)?;
            return Ok(CollectionProgress {
                phase: state.phase,
                sealed_epoch: current,
                stats: CollectionStats::default(),
            });
        }
        if epoch(&gc)? != state.epoch + 1 {
            return Err(fault(
                "Collection checkpoint and current protection epoch differ",
            ));
        }
        let guards = vec![
            condition(STATE_REF, previous.as_ref()),
            condition(crate::storage::protection::GC_REFERENCE, gc.as_ref()),
        ];
        let protection = WriteProtection {
            epoch: state.epoch + 1,
            conditions: guards.clone(),
        };
        self.store.set_write_protection(Some(protection))?;
        let mut stats = CollectionStats::default();
        let mut records = CountChanges::default();
        let mut metadata_to_protect = Vec::with_capacity(super::MAX_BATCH);
        let result = (|| {
            for _ in 0..maximum_steps {
                control()?;
                if records
                    .bytes
                    .saturating_add(state.pending.len().saturating_mul(32))
                    >= WORK_BYTES
                {
                    break;
                }
                match state.phase {
                    CollectionPhase::Adding | CollectionPhase::Removing => {
                        if let Some(id) = state.pop(&mut self.store)? {
                            let mut record =
                                match records.load(&mut self.store, state.counts, id)? {
                                    Some(record) => record,
                                    None if state.phase == CollectionPhase::Adding => {
                                        let bytes = required(&mut self.store, id)?;
                                        stats.newly_owned_objects += 1;
                                        stats.ownership_payload_bytes += bytes.len() as u64;
                                        Record {
                                            id,
                                            count: 0,
                                            retired_ms: 0,
                                            children: object_children(&mut self.store, id, &bytes)?,
                                        }
                                    }
                                    None => {
                                        return Err(fault(
                                            "Removing an object with no registered owner",
                                        ));
                                    }
                                };
                            if state.phase == CollectionPhase::Adding {
                                if record.count == 0 {
                                    state.push(&mut self.store, &record.children)?;
                                    record.retired_ms = 0;
                                }
                                record.count = record
                                    .count
                                    .checked_add(1)
                                    .ok_or_else(|| fault("Ownership count overflow"))?;
                            } else {
                                record.count = record
                                    .count
                                    .checked_sub(1)
                                    .ok_or_else(|| fault("Ownership count underflow"))?;
                                record.retired_ms = record.retired_ms.max(state.retire_ms);
                                if record.count == 0 {
                                    state.push(&mut self.store, &record.children)?;
                                }
                            }
                            records.change(id, Some(record));
                            stats.ownership_steps += 1;
                        } else if let Some((key, reference)) = sealed_event(
                            &mut self.store,
                            state.epoch,
                            state.event_after.as_deref(),
                        )? {
                            let event = Event::decode(reference.value.as_deref().unwrap())?;
                            state.retire_ms = event.at_ms;
                            state.push(
                                &mut self.store,
                                if state.phase == CollectionPhase::Adding {
                                    &event.added
                                } else {
                                    &event.removed
                                },
                            )?;
                            state.event_after = Some(key);
                        } else {
                            state.event_after = None;
                            state.phase = if state.phase == CollectionPhase::Adding {
                                CollectionPhase::Removing
                            } else {
                                state.counts = records.flush(&mut self.store, state.counts)?;
                                if let Some(root) = state.counts {
                                    state.push(&mut self.store, &[root])?;
                                }
                                CollectionPhase::ProtectingMetadata
                            };
                        }
                    }
                    CollectionPhase::ProtectingMetadata => {
                        if let Some(id) = state.pop(&mut self.store)? {
                            let block = Block::decode(&id, &required(&mut self.store, id)?)?;
                            if !matches!(
                                block.kind,
                                RECORD_KIND
                                    | super::receipts::REQUEST_LEAF_KIND
                                    | super::receipts::REQUEST_BRANCH_KIND
                            ) {
                                return Err(fault(
                                    "Ownership index contains a non-metadata object",
                                ));
                            }
                            state.push(&mut self.store, &block.links)?;
                            metadata_to_protect.push(id);
                            if metadata_to_protect.len() == super::MAX_BATCH {
                                guarded(self.store.protect_existing(
                                    &metadata_to_protect,
                                    state.epoch + 1,
                                    &guards,
                                )?)?;
                                metadata_to_protect.clear();
                            }
                            stats.metadata_objects_protected += 1;
                        } else {
                            if !metadata_to_protect.is_empty() {
                                guarded(self.store.protect_existing(
                                    &metadata_to_protect,
                                    state.epoch + 1,
                                    &guards,
                                )?)?;
                                metadata_to_protect.clear();
                            }
                            state.phase = CollectionPhase::Sweeping;
                            // Publish the folded counts before deleting any data.
                            // Otherwise a crash could restart Adding after the
                            // data needed to reconstruct its children was swept.
                            break;
                        }
                    }
                    CollectionPhase::Sweeping => {
                        let rows = self.store.list_object_info(state.scan_after, 1)?;
                        if let Some(info) = rows.first() {
                            state.scan_after = Some(info.id);
                            stats.objects_examined += 1;
                            if info.protected_epoch > state.epoch {
                                continue;
                            }
                            let record = records.load(&mut self.store, state.counts, info.id)?;
                            // Unknown objects never acquired a published owner:
                            // these are fenced abandoned writes or obsolete GC
                            // metadata, not age-retained database values.
                            if record
                                .as_ref()
                                .is_some_and(|r| r.count != 0 || r.retired_ms > state.cutoff_ms)
                            {
                                continue;
                            }
                            let removed = guarded(self.store.remove_unprotected(
                                &[info.id],
                                state.epoch + 1,
                                &guards,
                            )?)?;
                            if removed != 0 {
                                stats.objects_removed += removed as u64;
                                stats.stored_bytes_removed += info.stored_bytes;
                            }
                            if record.is_some() && !self.store.object_exists(info.id)? {
                                records.change(info.id, None);
                            }
                        } else {
                            state.counts = records.flush(&mut self.store, state.counts)?;
                            state.scan_after = None;
                            state.phase = CollectionPhase::PruningRecords;
                        }
                    }
                    CollectionPhase::PruningRecords => {
                        let index = RequestIndex::from_root(state.counts);
                        if let Some((id, pointer)) = index
                            .scan(&mut self.store, state.scan_after, 1)?
                            .into_iter()
                            .next()
                        {
                            let record = Record::load(&mut self.store, pointer, id)?;
                            if record.count == 0 && !self.store.object_exists(id)? {
                                records.change(id, None);
                            }
                            state.scan_after = Some(id);
                        } else {
                            state.scan_after = None;
                            state.phase = CollectionPhase::ClearingEvents;
                            // Deleting events is safe only after the checkpoint
                            // durably records that both ownership passes ended.
                            break;
                        }
                    }
                    CollectionPhase::ClearingEvents => {
                        if let Some((key, reference)) =
                            sealed_event(&mut self.store, state.epoch, None)?
                        {
                            let mut remove_guards = guards.clone();
                            remove_guards.push(condition(&key, Some(&reference)));
                            stats.events_removed +=
                                guarded(self.store.forget_ephemeral_refs(&[key], &remove_guards)?)?
                                    as u64;
                        } else {
                            state.event_after = None;
                            state.phase = CollectionPhase::Complete;
                            break;
                        }
                    }
                    CollectionPhase::Complete => break,
                }
            }
            if !metadata_to_protect.is_empty() {
                guarded(self.store.protect_existing(
                    &metadata_to_protect,
                    state.epoch + 1,
                    &guards,
                )?)?;
            }
            state.counts = records.flush(&mut self.store, state.counts)?;
            state.flush_pending(&mut self.store)?;
            let id = state.put(&mut self.store)?;
            applied(self.store.compare_exchange_many(
                &guards,
                &[RefChange {
                    key: STATE_REF.into(),
                    value: Some(id.to_vec()),
                }],
            )?)?;
            Ok(CollectionProgress {
                phase: state.phase,
                sealed_epoch: state.epoch,
                stats,
            })
        })();
        let clear = self.store.set_write_protection(None);
        match (result, clear) {
            (Err(e), _) | (Ok(_), Err(e)) => Err(e),
            (Ok(result), Ok(())) => Ok(result),
        }
    }

    /// Unbound program uploads are ordinary temporary roots. They expire by
    /// age, without reader sessions or application-owned release handles.
    fn expire_staging(&mut self, age: Duration, maximum: usize) -> Result<(), SemanticError> {
        let cutoff = now_ms()?.saturating_sub(u64::try_from(age.as_millis()).unwrap_or(u64::MAX));
        for (key, reference) in self.store.list_refs(Some("staging/"), maximum)? {
            if !key.starts_with("staging/") {
                break;
            }
            let time = key
                .split('/')
                .nth(1)
                .and_then(|s| u64::from_str_radix(s, 16).ok())
                .ok_or_else(|| fault("Invalid staged root timestamp"))?;
            if time >= cutoff {
                break;
            }
            let tombstone = if reference.value.is_some() {
                let guard = condition(&key, Some(&reference));
                match publish_refs(
                    &mut self.store,
                    &[guard],
                    &[RefChange {
                        key: key.clone(),
                        value: None,
                    }],
                )? {
                    BatchOutcome::Applied(mut changed) => changed.remove(0).1,
                    BatchOutcome::Conflict(_) => continue,
                }
            } else {
                reference
            };
            // Staging keys contain a fresh UUID and are never reused. The
            // durable ownership event precedes removal, including after a crash.
            self.store.forget_ephemeral_refs(
                std::slice::from_ref(&key),
                &[condition(&key, Some(&tombstone))],
            )?;
        }
        Ok(())
    }
}

fn encode_prune_cursor(key: &str, after: Option<ObjectId>) -> Vec<u8> {
    let mut bytes = (key.len() as u16).to_be_bytes().to_vec();
    bytes.extend_from_slice(key.as_bytes());
    if let Some(id) = after {
        bytes.extend_from_slice(&id);
    }
    bytes
}
fn decode_prune_cursor(bytes: &[u8]) -> Result<(String, Option<ObjectId>), SemanticError> {
    if bytes.is_empty() {
        return Ok((String::new(), None));
    }
    if bytes.len() < 2 {
        return Err(fault("Invalid authorization cursor"));
    }
    let len = u16::from_be_bytes(bytes[..2].try_into().unwrap()) as usize;
    if len > 1024 || (bytes.len() != len + 2 && bytes.len() != len + 34) {
        return Err(fault("Invalid authorization cursor size"));
    }
    let key = std::str::from_utf8(&bytes[2..2 + len])
        .map_err(|_| fault("Invalid authorization cursor key"))?;
    if !key.starts_with("databases/") {
        return Err(fault("Invalid authorization cursor namespace"));
    }
    Ok((
        key.into(),
        (bytes.len() == len + 34).then(|| bytes[2 + len..].try_into().unwrap()),
    ))
}

// Include every unprocessed older epoch as well as the just-sealed epoch. A
// protection-only epoch advance must never strand an acknowledged root event.
fn sealed_event(
    store: &mut PgBlockStore,
    sealed: u64,
    after: Option<&str>,
) -> Result<Option<(String, Reference)>, SemanticError> {
    let Some((key, reference)) = store
        .list_live_refs(EVENT_PREFIX, after, 1)?
        .into_iter()
        .next()
    else {
        return Ok(None);
    };
    let suffix = key.strip_prefix(EVENT_PREFIX).unwrap();
    let (encoded_epoch, uuid) = suffix
        .split_once('/')
        .ok_or_else(|| fault("Invalid ownership event key"))?;
    if encoded_epoch.len() != 16
        || uuid.len() != 32
        || !suffix
            .bytes()
            .filter(|b| *b != b'/')
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(fault("Invalid ownership event key"));
    }
    let epoch = u64::from_str_radix(encoded_epoch, 16)
        .map_err(|_| fault("Invalid ownership event epoch"))?;
    Ok((epoch <= sealed).then_some((key, reference)))
}

/// A pruning proof is useful only at a settled boundary and must join the
/// caller's publication CAS. A concurrent ownership change invalidates its clock guard.
pub(crate) fn settled_count(
    store: &mut PgBlockStore,
    id: ObjectId,
) -> Result<Option<(u64, Vec<RefCondition>)>, SemanticError> {
    let clock = store.read_ref(CLOCK_REF)?;
    let gc = store.read_ref(crate::storage::protection::GC_REFERENCE)?;
    let reference = store.read_ref(STATE_REF)?;
    let Some(bytes) = reference.as_ref().and_then(|r| r.value.as_deref()) else {
        return Ok(None);
    };
    let state = State::load(store, root_id(bytes)?)?;
    if state.phase != CollectionPhase::Complete
        || !store.list_live_refs(EVENT_PREFIX, None, 1)?.is_empty()
    {
        return Ok(None);
    }
    let count = match RequestIndex::from_root(state.counts).lookup(store, id)? {
        Some(record) => Record::load(store, record, id)?.count,
        None => 0,
    };
    Ok(Some((
        count,
        vec![
            condition(CLOCK_REF, clock.as_ref()),
            condition(STATE_REF, reference.as_ref()),
            condition(crate::storage::protection::GC_REFERENCE, gc.as_ref()),
        ],
    )))
}

#[derive(Clone)]
struct State {
    epoch: u64,
    phase: CollectionPhase,
    counts: Option<ObjectId>,
    frontier: Option<ObjectId>,
    frontier_offset: usize,
    event_after: Option<String>,
    scan_after: Option<ObjectId>,
    cutoff_ms: u64,
    retire_ms: u64,
    // Transient bounded work. Only the final frontier is persisted per advance,
    // not a new immutable stack for every push/pop within the same checkpoint.
    pending: Vec<ObjectId>,
}
impl State {
    fn empty() -> Self {
        Self {
            epoch: 0,
            phase: CollectionPhase::Complete,
            counts: None,
            frontier: None,
            frontier_offset: 0,
            event_after: None,
            scan_after: None,
            cutoff_ms: 0,
            retire_ms: 0,
            pending: Vec::new(),
        }
    }
    fn put(&self, store: &mut PgBlockStore) -> Result<ObjectId, SemanticError> {
        let phase = match self.phase {
            CollectionPhase::Adding => 0,
            CollectionPhase::Removing => 1,
            CollectionPhase::ProtectingMetadata => 2,
            CollectionPhase::Sweeping => 3,
            CollectionPhase::ClearingEvents => 4,
            CollectionPhase::Complete => 5,
            CollectionPhase::PruningRecords => 6,
        };
        let mut payload = vec![phase];
        payload.extend_from_slice(&self.epoch.to_be_bytes());
        payload.extend_from_slice(&self.cutoff_ms.to_be_bytes());
        payload.extend_from_slice(&(self.frontier_offset as u32).to_be_bytes());
        let name = self.event_after.as_deref().unwrap_or("").as_bytes();
        payload.extend_from_slice(&(name.len() as u16).to_be_bytes());
        payload.extend_from_slice(name);
        for pointer in [self.counts, self.frontier, self.scan_after] {
            payload.push(u8::from(pointer.is_some()));
            payload.extend_from_slice(&pointer.unwrap_or([0; 32]));
        }
        payload.extend_from_slice(&self.retire_ms.to_be_bytes());
        store.put(
            &Block {
                kind: STATE_KIND,
                links: self.counts.into_iter().chain(self.frontier).collect(),
                payload,
            }
            .encode()?,
        )
    }
    fn load(store: &mut PgBlockStore, id: ObjectId) -> Result<Self, SemanticError> {
        let block = Block::decode(&id, &required(store, id)?)?;
        if block.kind != STATE_KIND || block.payload.len() < 23 + 99 + 8 {
            return Err(fault("Invalid ownership checkpoint"));
        }
        let p = &block.payload;
        let length = u16::from_be_bytes(p[21..23].try_into().unwrap()) as usize;
        if length > 1024 || p.len() != 23 + length + 99 + 8 {
            return Err(fault("Invalid ownership checkpoint size"));
        }
        let mut pointers = Vec::new();
        for part in p[23 + length..p.len() - 8].chunks_exact(33) {
            if part[0] > 1 || (part[0] == 0 && part[1..].iter().any(|b| *b != 0)) {
                return Err(fault("Invalid ownership checkpoint pointer"));
            }
            pointers.push((part[0] == 1).then(|| part[1..].try_into().unwrap()));
        }
        let result = Self {
            epoch: u64::from_be_bytes(p[1..9].try_into().unwrap()),
            phase: match p[0] {
                0 => CollectionPhase::Adding,
                1 => CollectionPhase::Removing,
                2 => CollectionPhase::ProtectingMetadata,
                3 => CollectionPhase::Sweeping,
                4 => CollectionPhase::ClearingEvents,
                5 => CollectionPhase::Complete,
                6 => CollectionPhase::PruningRecords,
                _ => return Err(fault("Invalid collection phase")),
            },
            cutoff_ms: u64::from_be_bytes(p[9..17].try_into().unwrap()),
            retire_ms: u64::from_be_bytes(p[p.len() - 8..].try_into().unwrap()),
            frontier_offset: u32::from_be_bytes(p[17..21].try_into().unwrap()) as usize,
            event_after: if length == 0 {
                None
            } else {
                Some(
                    std::str::from_utf8(&p[23..23 + length])
                        .map_err(|_| fault("Invalid event cursor"))?
                        .into(),
                )
            },
            counts: pointers[0],
            frontier: pointers[1],
            scan_after: pointers[2],
            pending: Vec::new(),
        };
        if result.frontier_offset > STACK_ITEMS
            || block.links
                != result
                    .counts
                    .into_iter()
                    .chain(result.frontier)
                    .collect::<Vec<_>>()
        {
            return Err(fault("Invalid ownership checkpoint links"));
        }
        Ok(result)
    }
    fn pop(&mut self, store: &mut PgBlockStore) -> Result<Option<ObjectId>, SemanticError> {
        if let Some(id) = self.pending.pop() {
            return Ok(Some(id));
        }
        loop {
            let Some(id) = self.frontier else {
                return Ok(None);
            };
            let block = Block::decode(&id, &required(store, id)?)?;
            if block.kind != STACK_KIND
                || block.links.len() > 1
                || block.payload.len() % 32 != 0
                || block.payload.len() / 32 > STACK_ITEMS
            {
                return Err(fault("Invalid ownership work stack"));
            }
            let count = block.payload.len() / 32;
            if self.frontier_offset > count {
                return Err(fault("Invalid ownership work cursor"));
            }
            if self.frontier_offset == count {
                self.frontier = block.links.first().copied();
                self.frontier_offset = 0;
                continue;
            }
            let start = self.frontier_offset * 32;
            self.frontier_offset += 1;
            return Ok(Some(block.payload[start..start + 32].try_into().unwrap()));
        }
    }
    fn push(&mut self, store: &mut PgBlockStore, ids: &[ObjectId]) -> Result<(), SemanticError> {
        self.pending.extend(ids.iter().rev().copied());
        if self.pending.len().saturating_mul(32) >= WORK_BYTES {
            self.flush_pending(store)?;
        }
        Ok(())
    }
    fn flush_pending(&mut self, store: &mut PgBlockStore) -> Result<(), SemanticError> {
        let mut pending = std::mem::take(&mut self.pending);
        pending.reverse();
        self.push_persisted(store, &pending)
    }
    fn push_persisted(
        &mut self,
        store: &mut PgBlockStore,
        ids: &[ObjectId],
    ) -> Result<(), SemanticError> {
        if ids.is_empty() {
            return Ok(());
        }
        // Persist the unconsumed suffix once before adding a child frame.
        if self.frontier_offset != 0 {
            let old = self.frontier.ok_or_else(|| fault("Missing work stack"))?;
            let mut block = Block::decode(&old, &required(store, old)?)?;
            if block.kind != STACK_KIND || self.frontier_offset * 32 > block.payload.len() {
                return Err(fault("Invalid ownership work suffix"));
            }
            block.payload = block.payload[self.frontier_offset * 32..].to_vec();
            self.frontier = if block.payload.is_empty() {
                block.links.first().copied()
            } else {
                Some(store.put(&block.encode()?)?)
            };
            self.frontier_offset = 0;
        }
        for chunk in ids.chunks(STACK_ITEMS).rev() {
            self.frontier = Some(
                store.put(
                    &Block {
                        kind: STACK_KIND,
                        links: self.frontier.into_iter().collect(),
                        payload: chunk.iter().flat_map(|id| id.iter().copied()).collect(),
                    }
                    .encode()?,
                )?,
            );
        }
        Ok(())
    }
}

#[derive(Default)]
struct CountChanges {
    records: BTreeMap<ObjectId, Option<Record>>,
    dirty: BTreeSet<ObjectId>,
    bytes: usize,
}
impl CountChanges {
    fn load(
        &mut self,
        store: &mut PgBlockStore,
        root: Option<ObjectId>,
        id: ObjectId,
    ) -> Result<Option<Record>, SemanticError> {
        if let Some(record) = self.records.get(&id) {
            return Ok(record.clone());
        }
        let record = RequestIndex::from_root(root)
            .lookup(store, id)?
            .map(|pointer| Record::load(store, pointer, id))
            .transpose()?;
        self.bytes = self
            .bytes
            .saturating_add(record.as_ref().map_or(64, |r| 112 + r.children.len() * 32));
        self.records.insert(id, record.clone());
        Ok(record)
    }
    fn change(&mut self, id: ObjectId, record: Option<Record>) {
        let old = self.records.insert(id, record.clone());
        self.bytes = self
            .bytes
            .saturating_sub(old.as_ref().map_or(0, |r| {
                r.as_ref().map_or(64, |r| 112 + r.children.len() * 32)
            }))
            .saturating_add(record.as_ref().map_or(64, |r| 112 + r.children.len() * 32));
        self.dirty.insert(id);
    }
    fn flush(
        &mut self,
        store: &mut PgBlockStore,
        root: Option<ObjectId>,
    ) -> Result<Option<ObjectId>, SemanticError> {
        let mut changes = Vec::with_capacity(self.dirty.len());
        for id in &self.dirty {
            let pointer = self.records[id]
                .as_ref()
                .map(|r| r.put(store))
                .transpose()?;
            changes.push((*id, pointer));
        }
        let result = RequestIndex::from_root(root)
            .apply_batch(store, &changes)?
            .root();
        *self = Self::default();
        Ok(result)
    }
}

#[derive(Clone)]
struct Record {
    id: ObjectId,
    count: u64,
    retired_ms: u64,
    children: Vec<ObjectId>,
}
impl Record {
    fn put(&self, store: &mut PgBlockStore) -> Result<ObjectId, SemanticError> {
        let mut payload = self.id.to_vec();
        payload.extend_from_slice(&self.count.to_be_bytes());
        payload.extend_from_slice(&self.retired_ms.to_be_bytes());
        for child in &self.children {
            payload.extend_from_slice(child);
        }
        // Child IDs are ownership metadata, deliberately NOT strong links out
        // of the collector's own metadata graph.
        store.put(
            &Block {
                kind: RECORD_KIND,
                links: vec![],
                payload,
            }
            .encode()?,
        )
    }
    fn load(
        store: &mut PgBlockStore,
        pointer: ObjectId,
        id: ObjectId,
    ) -> Result<Self, SemanticError> {
        let block = Block::decode(&pointer, &required(store, pointer)?)?;
        let p = &block.payload;
        if block.kind != RECORD_KIND
            || !block.links.is_empty()
            || p.len() < 48
            || (p.len() - 48) % 32 != 0
            || p[..32] != id
        {
            return Err(fault("Invalid ownership record"));
        }
        let result = Self {
            id,
            count: u64::from_be_bytes(p[32..40].try_into().unwrap()),
            retired_ms: u64::from_be_bytes(p[40..48].try_into().unwrap()),
            children: p[48..]
                .chunks_exact(32)
                .map(|b| b.try_into().unwrap())
                .collect(),
        };
        if result.children.windows(2).any(|w| w[0] >= w[1]) {
            return Err(fault("Noncanonical ownership record"));
        }
        Ok(result)
    }
}

pub(crate) fn object_children(
    store: &mut dyn super::ObjectReader,
    id: ObjectId,
    bytes: &[u8],
) -> Result<Vec<ObjectId>, SemanticError> {
    if crate::sha256(bytes) != id {
        return Err(fault("Owned object failed authentication"));
    }
    let mut children = BTreeSet::new();
    if bytes.starts_with(b"ATOB") {
        let block = Block::decode(&id, bytes)?;
        children.extend(block.links.iter().copied());
        if block.kind == super::log::LOG_ENTRY_KIND {
            children.extend(super::log::entry_program_links(store, &block)?);
        }
    } else if bytes.starts_with(b"ATIX") {
        use crate::index::tree::{TreeNode, decode_tree_node};
        match decode_tree_node(&id, bytes)? {
            TreeNode::Root(root) => children.extend(root.directories.iter().map(|c| c.hash)),
            TreeNode::Directory(dir) => children.extend(dir.leaves.iter().map(|c| c.hash)),
            TreeNode::Leaf(leaf) => {
                for value in &leaf.values {
                    crate::program_bindings::collect_program_hashes(value, &mut children);
                }
            }
        }
    } else if bytes.starts_with(b"ATMC") {
        let program = crate::decode_program(bytes)?;
        let mut dependencies = Vec::new();
        crate::program_bindings::collect_fixed_program_dependencies(
            &program.instructions,
            &mut dependencies,
        );
        children.extend(dependencies);
    } else {
        return Err(fault("Unknown owned object format"));
    }
    Ok(children.into_iter().collect())
}
fn required(store: &mut PgBlockStore, id: ObjectId) -> Result<Vec<u8>, SemanticError> {
    store.get(id)?.ok_or_else(|| {
        fault("An owned immutable object is missing").detail(
            "object_id",
            id.iter().map(|b| format!("{b:02x}")).collect::<String>(),
        )
    })
}
fn now_ms() -> Result<u64, SemanticError> {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| fault("Clock precedes Unix epoch"))?
            .as_millis(),
    )
    .map_err(|_| fault("Clock exceeds timestamp range"))
}
fn applied(value: BatchOutcome) -> Result<(), SemanticError> {
    match value {
        BatchOutcome::Applied(_) => Ok(()),
        BatchOutcome::Conflict(_) => Err(SemanticError::conflict(
            "storage/collector-stale",
            "Collector checkpoint or epoch changed",
        )),
    }
}
fn guarded<T>(value: Guarded<T>) -> Result<T, SemanticError> {
    match value {
        Guarded::Applied(value) => Ok(value),
        Guarded::Conflict(_) => Err(SemanticError::conflict(
            "storage/collector-stale",
            "Collector checkpoint or epoch changed",
        )),
    }
}

/// All engine object-root namespaces use one 32-byte immutable object identity.
/// Session/lease/consumer/network values are coordination, not object roots.
fn is_root_key(key: &str) -> bool {
    [
        "databases/",
        "names/",
        "catalog/",
        "staging/",
        "excision/",
        "restores/",
        "handoffs/",
    ]
    .iter()
    .any(|prefix| key.starts_with(prefix))
}

/// Engine barrier for a root publication, capture or release. Opaque provider
/// operations remain available for non-root coordination and collector state.
/// Conflicts are returned to the caller, never retried as a different decision.
pub(crate) fn publish_refs(
    store: &mut PgBlockStore,
    conditions: &[RefCondition],
    changes: &[RefChange],
) -> Result<BatchOutcome, SemanticError> {
    super::protocol::validate_batch(conditions, changes)?;
    if !changes.iter().any(|change| is_root_key(&change.key)) {
        return store.compare_exchange_many(conditions, changes);
    }
    let mut guards = conditions.to_vec();
    let gc = store.read_ref(crate::storage::protection::GC_REFERENCE)?;
    let epoch = epoch(&gc)?;
    let gc_condition = condition(crate::storage::protection::GC_REFERENCE, gc.as_ref());
    if let Some(existing) = guards.iter().find(|g| g.key == gc_condition.key) {
        if *existing != gc_condition {
            return Ok(BatchOutcome::Conflict(vec![(gc_condition.key, gc)]));
        }
    } else {
        guards.push(gc_condition);
    }
    let mut removed = Vec::new();
    let mut added = Vec::new();
    for change in changes.iter().filter(|change| is_root_key(&change.key)) {
        let previous = store.read_ref(&change.key)?;
        let expected = conditions.iter().find(|g| g.key == change.key).unwrap();
        if previous.as_ref().map(|r| r.revision) != expected.expected {
            return Ok(BatchOutcome::Conflict(vec![(change.key.clone(), previous)]));
        }
        // Multiplicity matters: two different root keys can own one object.
        let old = previous
            .as_ref()
            .and_then(|r| r.value.as_deref())
            .map(root_id)
            .transpose()?;
        let new = change.value.as_deref().map(root_id).transpose()?;
        if old != new {
            removed.extend(old);
            added.extend(new);
        }
    }
    if removed.is_empty() && added.is_empty() {
        return store.compare_exchange_many(&guards, changes);
    }
    let event_key = format!("{EVENT_PREFIX}{epoch:016x}/{:032x}", crate::uuid_v7()?);
    guards.push(RefCondition {
        key: event_key.clone(),
        expected: None,
    });
    let mut changes = changes.to_vec();
    changes.push(RefChange {
        key: event_key,
        value: Some(
            Event {
                removed,
                added,
                at_ms: now_ms()?,
            }
            .encode()?,
        ),
    });
    // A compact mutation clock makes a settled ownership proof conditional on
    // there being no intervening captures/publications. Only clock contention
    // is retried, never a changed caller authority or GC epoch.
    changes.push(RefChange {
        key: CLOCK_REF.into(),
        value: Some(Vec::new()),
    });
    for _ in 0..32 {
        let clock = store.read_ref(CLOCK_REF)?;
        let expected = condition(CLOCK_REF, clock.as_ref());
        let caller_guarded_clock = guards.iter().any(|g| g.key == CLOCK_REF);
        if let Some(caller) = guards.iter().find(|g| g.key == CLOCK_REF) {
            if *caller != expected {
                return Ok(BatchOutcome::Conflict(vec![(CLOCK_REF.into(), clock)]));
            }
        } else {
            guards.push(expected);
        }
        match store.compare_exchange_many(&guards, &changes)? {
            BatchOutcome::Conflict(current)
                if !caller_guarded_clock
                    && guards.iter().filter(|g| g.key != CLOCK_REF).all(|g| {
                        current
                            .iter()
                            .find(|(key, _)| key == &g.key)
                            .is_some_and(|(_, value)| {
                                value.as_ref().map(|r| r.revision) == g.expected
                            })
                    }) =>
            {
                guards.retain(|g| g.key != CLOCK_REF);
            }
            outcome => return Ok(outcome),
        }
    }
    Err(SemanticError::new(
        ErrorCategory::Busy,
        "storage/ownership-contention",
        "Concurrent root transitions exhausted the publication retry bound",
    ))
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Event {
    removed: Vec<ObjectId>,
    added: Vec<ObjectId>,
    at_ms: u64,
}
impl Event {
    fn encode(&self) -> Result<Vec<u8>, SemanticError> {
        if self.removed.len() > super::MAX_BATCH || self.added.len() > super::MAX_BATCH {
            return Err(fault("Ownership event exceeds one atomic root batch"));
        }
        let mut bytes = EVENT_MAGIC.to_vec();
        bytes.extend_from_slice(&(self.removed.len() as u16).to_be_bytes());
        bytes.extend_from_slice(&(self.added.len() as u16).to_be_bytes());
        bytes.extend_from_slice(&self.at_ms.to_be_bytes());
        for id in self.removed.iter().chain(&self.added) {
            bytes.extend_from_slice(id);
        }
        Ok(bytes)
    }
    fn decode(bytes: &[u8]) -> Result<Self, SemanticError> {
        if bytes.len() < 16 || &bytes[..4] != EVENT_MAGIC {
            return Err(fault("Invalid ownership event"));
        }
        let removed = u16::from_be_bytes(bytes[4..6].try_into().unwrap()) as usize;
        let added = u16::from_be_bytes(bytes[6..8].try_into().unwrap()) as usize;
        if removed > super::MAX_BATCH
            || added > super::MAX_BATCH
            || bytes.len() != 16 + (removed + added) * 32
        {
            return Err(fault("Invalid ownership event lengths"));
        }
        let ids: Vec<ObjectId> = bytes[16..]
            .chunks_exact(32)
            .map(|v| v.try_into().unwrap())
            .collect();
        Ok(Self {
            at_ms: u64::from_be_bytes(bytes[8..16].try_into().unwrap()),
            removed: ids[..removed].to_vec(),
            added: ids[removed..].to_vec(),
        })
    }
}

fn epoch(value: &Option<Reference>) -> Result<u64, SemanticError> {
    match value.as_ref().and_then(|r| r.value.as_deref()) {
        None => Ok(0),
        Some(bytes) if bytes.len() == 8 => {
            let epoch = u64::from_be_bytes(bytes.try_into().unwrap());
            if epoch > i64::MAX as u64 {
                return Err(fault("Collection epoch exceeds storage bounds"));
            }
            Ok(epoch)
        }
        Some(_) => Err(fault("Malformed collection epoch")),
    }
}
fn root_id(bytes: &[u8]) -> Result<ObjectId, SemanticError> {
    bytes
        .try_into()
        .map_err(|_| fault("Engine root reference must contain one object identity"))
}
fn condition(key: &str, value: Option<&Reference>) -> RefCondition {
    RefCondition {
        key: key.into(),
        expected: value.map(|r| r.revision),
    }
}
fn fault(message: &str) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, "storage/ownership", message)
}
