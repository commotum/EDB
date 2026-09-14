//! Current-format postorder restore. Pending object IDs are weak payload data;
//! only fully copied child-closed objects enter the checkpoint's ownership map.
use super::{PortableBackup, ReadPoint, RestoreFault, RestoreResult, load_selected, read_object};
use crate::storage::catalog::{self, identity_key, lineage_key};
use crate::storage::catalog::{identity_string, name_key};
use crate::storage::log::LogRoot;
use crate::storage::ownership::{object_children, publish_refs};
use crate::storage::protection::protection;
use crate::storage::receipts::{ExactReceipt, RequestIndex, basis_receipt_key};
use crate::storage::root::{Block, DatabaseRoot};
use crate::storage::{
    BatchOutcome, BlockDatabase, ObjectId, ObjectReader, PgBlockStore, RefChange, RefCondition,
    Reference,
};
use crate::transactor::authority::restore_lease_guard;
use crate::{ErrorCategory, SemanticError};
use std::collections::BTreeSet;
use std::path::Path;

const CHECKPOINT_KIND: u16 = 61;
const FRAME_KIND: u16 = 62;
const COMPLETION_KIND: u16 = 63;
const COPY_STEPS: usize = 64;
// Only immutable bytes are cached. The independent per-attempt budget is
// bounded even when a repository or its persistent copied-object map is huge.
const RESTORE_CACHE_BYTES: usize = 1024 * 1024;

fn work_key(database: &BlockDatabase) -> String {
    format!("restores/{}", identity_string(database.route))
}
fn completions_key(database: &BlockDatabase) -> String {
    format!("restores/completed/{}", identity_string(database.route))
}
fn guard(key: String, reference: Option<&Reference>) -> RefCondition {
    RefCondition {
        key,
        expected: reference.map(|r| r.revision),
    }
}
fn changed(key: String, id: Option<ObjectId>) -> RefChange {
    RefChange {
        key,
        value: id.map(|id| id.to_vec()),
    }
}
fn object_id(bytes: &[u8]) -> Result<ObjectId, SemanticError> {
    bytes
        .try_into()
        .map_err(|_| fault("Malformed restore object reference"))
}
fn required(
    store: &mut (impl ObjectReader + ?Sized),
    id: ObjectId,
) -> Result<Vec<u8>, SemanticError> {
    store.read_object(id)
}
fn fault(message: &str) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, "backup/restore-state", message)
}
fn stale() -> SemanticError {
    SemanticError::conflict(
        "backup/restore-target-changed",
        "Restore target, reservation, lease or checkpoint changed",
    )
}
fn injected(code: &'static str) -> SemanticError {
    SemanticError::new(
        ErrorCategory::Unavailable,
        code,
        "Injected restore interruption",
    )
}
fn applied(outcome: BatchOutcome) -> Result<Vec<(String, Reference)>, SemanticError> {
    match outcome {
        BatchOutcome::Applied(rows) => Ok(rows),
        BatchOutcome::Conflict(_) => Err(stale()),
    }
}

#[derive(Clone)]
struct Checkpoint {
    identity: [u8; 16],
    route: [u8; 16],
    manifest: ObjectId,
    prior: Option<ObjectId>,
    stack: Option<ObjectId>,
    copied: RequestIndex,
    guards: Vec<RefCondition>,
}
impl Checkpoint {
    fn put(&self, store: &mut PgBlockStore) -> Result<ObjectId, SemanticError> {
        let mut payload = self.identity.to_vec();
        payload.extend_from_slice(&self.route);
        payload.extend_from_slice(&self.manifest);
        let mut links = Vec::new();
        let mut flags = 0u8;
        for (bit, id) in [self.prior, self.stack, self.copied.root()]
            .into_iter()
            .enumerate()
        {
            if let Some(id) = id {
                flags |= 1 << bit;
                links.push(id);
            }
        }
        payload.push(flags);
        payload.extend_from_slice(&(self.guards.len() as u16).to_be_bytes());
        for condition in &self.guards {
            if condition.key.len() > 1024 || condition.expected == Some(0) {
                return Err(fault("Invalid restore guard"));
            }
            payload.extend_from_slice(&(condition.key.len() as u16).to_be_bytes());
            payload.extend_from_slice(condition.key.as_bytes());
            payload.extend_from_slice(&condition.expected.unwrap_or(0).to_be_bytes());
        }
        store.put(
            &Block {
                kind: CHECKPOINT_KIND,
                links,
                payload,
            }
            .encode()?,
        )
    }
    fn load(store: &mut PgBlockStore, id: ObjectId) -> Result<Self, SemanticError> {
        let block = Block::decode(&id, &required(store, id)?)?;
        if block.kind != CHECKPOINT_KIND || block.payload.len() < 67 || block.links.len() > 3 {
            return Err(fault("Malformed restore checkpoint"));
        }
        let identity = block.payload[..16].try_into().unwrap();
        let route = block.payload[16..32].try_into().unwrap();
        let manifest = block.payload[32..64].try_into().unwrap();
        let flags = block.payload[64];
        if flags > 7
            || flags.count_ones() as usize != block.links.len()
            || identity == [0; 16]
            || route == [0; 16]
        {
            return Err(fault("Malformed restore checkpoint coordinates"));
        }
        let mut links = block.links.into_iter();
        let prior = (flags & 1 != 0).then(|| links.next().unwrap());
        let stack = (flags & 2 != 0).then(|| links.next().unwrap());
        let copied = RequestIndex::from_root((flags & 4 != 0).then(|| links.next().unwrap()));
        let count = u16::from_be_bytes(block.payload[65..67].try_into().unwrap()) as usize;
        if !(5..=12).contains(&count) {
            return Err(fault("Invalid restore guard count"));
        }
        let mut at = 67;
        let mut guards = Vec::with_capacity(count);
        for _ in 0..count {
            let length = block
                .payload
                .get(at..at + 2)
                .ok_or_else(|| fault("Truncated restore guard"))?;
            let length = u16::from_be_bytes(length.try_into().unwrap()) as usize;
            at += 2;
            if length == 0 || length > 1024 {
                return Err(fault("Invalid restore guard key"));
            }
            let key = std::str::from_utf8(
                block
                    .payload
                    .get(at..at + length)
                    .ok_or_else(|| fault("Truncated restore guard"))?,
            )
            .map_err(|_| fault("Restore guard is not UTF8"))?
            .to_owned();
            at += length;
            let revision = u64::from_be_bytes(
                block
                    .payload
                    .get(at..at + 8)
                    .ok_or_else(|| fault("Truncated restore revision"))?
                    .try_into()
                    .unwrap(),
            );
            at += 8;
            if key.contains('\0') || guards.iter().any(|g: &RefCondition| g.key == key) {
                return Err(fault("Duplicate or invalid restore guard"));
            }
            guards.push(RefCondition {
                key,
                expected: (revision != 0).then_some(revision),
            });
        }
        if at != block.payload.len() {
            return Err(fault("Trailing restore checkpoint bytes"));
        }
        Ok(Self {
            identity,
            route,
            manifest,
            prior,
            stack,
            copied,
            guards,
        })
    }
}

struct Frame {
    object: ObjectId,
    child: u64,
    next: Option<ObjectId>,
}
impl Frame {
    fn put(&self, store: &mut PgBlockStore) -> Result<ObjectId, SemanticError> {
        // The object ID is deliberately not a link: the source may not exist
        // in the destination yet. Only the internal stack successor is strong.
        let mut payload = self.object.to_vec();
        payload.extend_from_slice(&self.child.to_be_bytes());
        store.put(
            &Block {
                kind: FRAME_KIND,
                links: self.next.into_iter().collect(),
                payload,
            }
            .encode()?,
        )
    }
    fn load(store: &mut PgBlockStore, id: ObjectId) -> Result<Self, SemanticError> {
        let block = Block::decode(&id, &required(store, id)?)?;
        if block.kind != FRAME_KIND || block.links.len() > 1 || block.payload.len() != 40 {
            return Err(fault("Malformed restore stack frame"));
        }
        Ok(Self {
            object: block.payload[..32].try_into().unwrap(),
            child: u64::from_be_bytes(block.payload[32..].try_into().unwrap()),
            next: block.links.first().copied(),
        })
    }
}

impl PortableBackup {
    pub fn restore_backup(
        &mut self,
        directory: &Path,
        basis: u64,
        target: &str,
    ) -> Result<RestoreResult, SemanticError> {
        self.restore_selected(
            directory,
            basis,
            None,
            target,
            RestoreFault::None,
            None,
            None,
        )
    }
    pub fn restore_backup_point(
        &mut self,
        directory: &Path,
        basis: u64,
        generation: u64,
        target: &str,
    ) -> Result<RestoreResult, SemanticError> {
        self.restore_selected(
            directory,
            basis,
            Some(generation),
            target,
            RestoreFault::None,
            None,
            None,
        )
    }
    #[doc(hidden)]
    pub fn restore_backup_with_fault(
        &mut self,
        directory: &Path,
        basis: u64,
        target: &str,
        fault: RestoreFault,
    ) -> Result<RestoreResult, SemanticError> {
        self.restore_selected(directory, basis, None, target, fault, None, None)
    }
    #[doc(hidden)]
    pub fn restore_backup_with_activation_probe<F: FnMut()>(
        &mut self,
        directory: &Path,
        basis: u64,
        target: &str,
        mut probe: F,
    ) -> Result<RestoreResult, SemanticError> {
        self.restore_selected(
            directory,
            basis,
            None,
            target,
            RestoreFault::None,
            Some(&mut probe),
            None,
        )
    }
    #[doc(hidden)]
    pub fn restore_backup_with_completion_probe<F: FnMut()>(
        &mut self,
        directory: &Path,
        basis: u64,
        target: &str,
        mut probe: F,
    ) -> Result<RestoreResult, SemanticError> {
        self.restore_selected(
            directory,
            basis,
            None,
            target,
            RestoreFault::None,
            None,
            Some(&mut probe),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn restore_selected(
        &mut self,
        directory: &Path,
        basis: u64,
        generation: Option<u64>,
        target: &str,
        fault_at: RestoreFault,
        mut activation_probe: Option<&mut dyn FnMut()>,
        mut completion_probe: Option<&mut dyn FnMut()>,
    ) -> Result<RestoreResult, SemanticError> {
        self.maintenance.check()?;
        catalog::validate_name(target)?;
        let point = load_selected(directory, generation, basis)?;
        let identity = catalog::parse_identity(&point.point.lineage_id)?;
        let mut store = PgBlockStore::connect(&self.connection)?;
        store.reset_object_cache(RESTORE_CACHE_BYTES);
        let existing = catalog::restore_binding(&mut store, target, identity)?;
        let database = existing.clone().unwrap_or(BlockDatabase {
            identity,
            route: crate::uuid_v7()?.to_be_bytes(),
            name: target.to_owned(),
        });
        let completion_key = completions_key(&database);
        let completed_ref = store.read_ref(&completion_key)?;
        let completed = RequestIndex::from_root(
            completed_ref
                .as_ref()
                .and_then(|r| r.value.as_deref())
                .map(object_id)
                .transpose()?,
        );
        if let Some(id) = completed.lookup(&mut store, point.point.manifest_hash)? {
            let activated_root = validate_completion(&mut store, id, &database, &point)?;
            // Exact completion is checked before writer/precondition admission
            // and all callback hooks. It does not roll back a newer live head.
            return Ok(RestoreResult {
                point: point.point,
                target: database.name,
                activated_root,
            });
        }
        let work_key = work_key(&database);
        let old_work = store.read_ref(&work_key)?;
        let mut work_guard = guard(work_key.clone(), old_work.as_ref());
        let mut checkpoint = if let Some(bytes) = old_work.as_ref().and_then(|r| r.value.as_deref())
        {
            let checkpoint = Checkpoint::load(&mut store, object_id(bytes)?)?;
            if checkpoint.identity != identity
                || checkpoint.route != database.route
                || checkpoint.manifest != point.point.manifest_hash
            {
                return Err(SemanticError::conflict(
                    "backup/restore-build-exists",
                    "This route has an incomplete restore of another point",
                ));
            }
            validate_guards(&checkpoint, &database)?;
            checkpoint
        } else {
            let root_ref = store.read_ref(&database.reference_key())?;
            if root_ref.as_ref().is_some_and(|r| r.value.is_none()) {
                return Err(stale());
            }
            let prior = root_ref
                .as_ref()
                .and_then(|r| r.value.as_deref())
                .map(object_id)
                .transpose()?;
            let lease_guard = restore_lease_guard(&mut store, &database)?;
            // Refuse incompatible live branches before creating durable work.
            // Final activation repeats the proof using only copied PG objects.
            let candidate = DatabaseRoot::decode(
                &point.publication,
                &read_object(directory, point.publication)?,
            )?;
            let mut union = |id| match store.get(id)? {
                Some(bytes) => Ok(bytes),
                None => read_object(directory, id),
            };
            validate_successor(&mut union, prior, &candidate, &self.maintenance)?;
            let mut guards = vec![
                guard(
                    name_key(target),
                    store.read_ref(&name_key(target))?.as_ref(),
                ),
                guard(database.reference_key(), root_ref.as_ref()),
                lease_guard,
                guard(completion_key.clone(), completed_ref.as_ref()),
            ];
            if existing.is_none() {
                guards.extend(catalog::creation_conditions(&mut store, &database)?);
            } else {
                for key in [identity_key(database.route), lineage_key(identity)] {
                    guards.push(guard(key.clone(), store.read_ref(&key)?.as_ref()));
                }
            }
            guards.push(work_guard.clone());
            let protected = protection(&mut store, &guards)?;
            store.set_write_protection(Some(protected.clone()))?;
            let stack = Frame {
                object: point.point.manifest_hash,
                child: 0,
                next: None,
            }
            .put(&mut store)?;
            let mut changes = Vec::new();
            if existing.is_none() {
                let mapping = catalog::put_name(&mut store, &database)?;
                changes.push(changed(name_key(target), Some(mapping)));
                changes.extend(catalog::creation_changes(&mut store, &database, mapping)?);
            }
            let mut retained = guards
                .iter()
                .filter(|g| {
                    g.key != work_key
                        && !g.key.starts_with("catalog/names/")
                        && !g.key.starts_with("catalog/overflow/")
                })
                .cloned()
                .collect::<Vec<_>>();
            for condition in &mut retained {
                if changes.iter().any(|change| change.key == condition.key) {
                    condition.expected = Some(
                        condition
                            .expected
                            .unwrap_or(0)
                            .checked_add(1)
                            .ok_or_else(|| fault("Restore revision overflow"))?,
                    );
                }
            }
            let checkpoint = Checkpoint {
                identity,
                route: database.route,
                manifest: point.point.manifest_hash,
                prior,
                stack: Some(stack),
                copied: RequestIndex::empty(),
                guards: retained,
            };
            let id = checkpoint.put(&mut store)?;
            changes.push(changed(work_key.clone(), Some(id)));
            let rows = applied(publish_refs(&mut store, &protected.conditions, &changes)?)?;
            work_guard.expected = Some(
                rows.iter()
                    .find(|(key, _)| key == &work_key)
                    .ok_or_else(|| fault("Restore publication lacks checkpoint"))?
                    .1
                    .revision,
            );
            store.set_write_protection(None)?;
            checkpoint
        };
        while checkpoint.stack.is_some() {
            self.maintenance.check()?;
            let mut guards = checkpoint.guards.clone();
            guards.push(work_guard.clone());
            let protected = protection(&mut store, &guards)?;
            store.set_write_protection(Some(protected.clone()))?;
            let mut uploaded = false;
            // A batch changes only a bounded stack prefix. Keep its transient
            // pushes/pops in memory; the untouched suffix remains one link to
            // the previous durable frontier. At most COPY_STEPS + 1 frames and
            // COPY_STEPS child-closed IDs can be retained here.
            let mut pending = Vec::with_capacity(COPY_STEPS + 1);
            let mut suffix = checkpoint.stack;
            let mut copied = BTreeSet::new();
            for _ in 0..COPY_STEPS {
                self.maintenance.check()?;
                let frame = if let Some(frame) = pending.pop() {
                    frame
                } else {
                    let Some(head) = suffix else { break };
                    let frame = Frame::load(&mut store, head)?;
                    suffix = frame.next;
                    frame
                };
                if copied.contains(&frame.object) {
                    continue;
                }
                if let Some(existing) = checkpoint.copied.lookup(&mut store, frame.object)? {
                    if existing != frame.object {
                        return Err(fault("Copied-object proof differs from its key"));
                    }
                    continue;
                }
                let bytes = read_object(directory, frame.object)?;
                let mut source = |id| read_object(directory, id);
                let children = object_children(&mut source, frame.object, &bytes)?;
                if frame.child > children.len() as u64 {
                    return Err(fault("Restore child cursor exceeds authenticated children"));
                }
                if let Some(child) = children.get(frame.child as usize) {
                    pending.push(Frame {
                        object: frame.object,
                        child: frame.child + 1,
                        next: None,
                    });
                    pending.push(Frame {
                        object: *child,
                        child: 0,
                        next: None,
                    });
                } else {
                    // All children passed through the copied map before this
                    // parent. Reused objects are re-protected with the same GC
                    // and target guards as newly uploaded objects.
                    if store.put(&bytes)? != frame.object {
                        return Err(fault("Restored object hash changed"));
                    }
                    copied.insert(frame.object);
                    uploaded = true;
                    if fault_at == RestoreFault::AfterFirstContentInserted {
                        break;
                    }
                }
            }
            // Persist only the final stack frontier and final trie paths.
            // Unpublished objects are still epoch protected under the captured
            // work/target guards; only this child-closed map becomes a strong
            // owner in the atomic checkpoint publication below.
            checkpoint.stack = suffix;
            for mut frame in pending {
                frame.next = checkpoint.stack;
                checkpoint.stack = Some(frame.put(&mut store)?);
            }
            checkpoint.copied = checkpoint.copied.apply_batch(
                &mut store,
                &copied
                    .into_iter()
                    .map(|id| (id, Some(id)))
                    .collect::<Vec<_>>(),
            )?;
            let id = checkpoint.put(&mut store)?;
            let rows = applied(publish_refs(
                &mut store,
                &protected.conditions,
                &[changed(work_key.clone(), Some(id))],
            )?)?;
            work_guard.expected = Some(
                rows.iter()
                    .find(|(key, _)| key == &work_key)
                    .ok_or_else(|| fault("Restore publication lacks checkpoint"))?
                    .1
                    .revision,
            );
            store.set_write_protection(None)?;
            self.maintenance.after_batch()?;
            if uploaded && fault_at == RestoreFault::AfterFirstContentInserted {
                return Err(injected("backup/restore-after-content"));
            }
        }
        if checkpoint.copied.lookup(&mut store, checkpoint.manifest)? != Some(checkpoint.manifest) {
            return Err(fault(
                "Completed restore has no child-closed manifest proof",
            ));
        }
        if fault_at == RestoreFault::BeforeCommit {
            return Err(injected("backup/restore-before-activation"));
        }
        if let Some(probe) = activation_probe.as_mut() {
            probe();
        }
        self.maintenance.check()?;
        let mut guards = checkpoint.guards.clone();
        guards.push(work_guard);
        let protected = protection(&mut store, &guards)?;
        store.set_write_protection(Some(protected.clone()))?;
        let mut root = DatabaseRoot::decode(
            &point.publication,
            &required(&mut store, point.publication)?,
        )?;
        validate_successor(&mut store, checkpoint.prior, &root, &self.maintenance)?;
        root.writer_epoch = match checkpoint.prior {
            Some(id) => DatabaseRoot::decode(&id, &required(&mut store, id)?)?
                .writer_epoch
                .checked_add(1)
                .ok_or_else(|| fault("Writer epoch overflows"))?,
            None => 0,
        };
        let activated = store.put(&root.encode()?)?;
        let mut payload = identity.to_vec();
        payload.extend_from_slice(&database.route);
        payload.extend_from_slice(&point.point.manifest_hash);
        let receipt = store.put(
            &Block {
                kind: COMPLETION_KIND,
                links: vec![activated, point.point.manifest_hash],
                payload,
            }
            .encode()?,
        )?;
        let completions = completed.insert(&mut store, point.point.manifest_hash, receipt)?;
        // The completion hook can race an operator/writer before publication;
        // all captured guards are still compared by the final atomic batch.
        if let Some(probe) = completion_probe.as_mut() {
            probe();
        }
        self.maintenance.check()?;
        applied(publish_refs(
            &mut store,
            &protected.conditions,
            &[
                changed(database.reference_key(), Some(activated)),
                changed(database.lease_key(), None),
                changed(completion_key, completions.root()),
                changed(work_key, None),
            ],
        )?)?;
        store.set_write_protection(None)?;
        crate::observation::notices::publish(&self.connection, &identity_string(database.route));
        if fault_at == RestoreFault::AfterCommitBeforeResponse {
            return Err(injected("backup/restore-after-activation"));
        }
        if fault_at == RestoreFault::AfterTreePublication {
            // Publication is complete; its ordinary ownership event remains
            // for the incremental collector, never a SQL membership workflow.
            return Err(injected("backup/restore-after-tree-publication"));
        }
        Ok(RestoreResult {
            point: point.point,
            target: database.name,
            activated_root: activated,
        })
    }
}

fn validate_completion(
    store: &mut PgBlockStore,
    id: ObjectId,
    database: &BlockDatabase,
    point: &ReadPoint,
) -> Result<ObjectId, SemanticError> {
    let block = Block::decode(&id, &required(store, id)?)?;
    if block.kind != COMPLETION_KIND
        || block.links.len() != 2
        || block.payload.len() != 64
        || block.payload[..16] != database.identity
        || block.payload[16..32] != database.route
        || block.payload[32..] != point.point.manifest_hash
        || block.links[1] != point.point.manifest_hash
    {
        return Err(fault(
            "Restore completion differs from its request or route",
        ));
    }
    let current = catalog::require_active(store, database.route)?;
    if current.lineage_id != point.point.lineage_id {
        return Err(stale());
    }
    Ok(block.links[0])
}

fn validate_guards(checkpoint: &Checkpoint, database: &BlockDatabase) -> Result<(), SemanticError> {
    let mut expected = vec![
        name_key(&database.name),
        database.reference_key(),
        database.lease_key(),
        identity_key(database.route),
        lineage_key(database.identity),
        completions_key(database),
    ];
    expected.sort();
    let mut actual = checkpoint
        .guards
        .iter()
        .map(|guard| guard.key.clone())
        .collect::<Vec<_>>();
    actual.sort();
    if actual != expected {
        return Err(fault("Restore checkpoint does not fence its exact route"));
    }
    for key in [
        name_key(&database.name),
        identity_key(database.route),
        lineage_key(database.identity),
    ] {
        if checkpoint
            .guards
            .iter()
            .find(|guard| guard.key == key)
            .and_then(|guard| guard.expected)
            .is_none()
        {
            return Err(fault(
                "Restore checkpoint lacks its name/lineage reservation",
            ));
        }
    }
    let root_revision = checkpoint
        .guards
        .iter()
        .find(|guard| guard.key == database.reference_key())
        .unwrap()
        .expected;
    if root_revision.is_some() != checkpoint.prior.is_some() {
        return Err(fault("Restore prior root and fence disagree"));
    }
    Ok(())
}

fn validate_successor(
    store: &mut dyn ObjectReader,
    previous: Option<ObjectId>,
    next: &DatabaseRoot,
    control: &crate::MaintenanceControl,
) -> Result<(), SemanticError> {
    let Some(id) = previous else { return Ok(()) };
    let previous = DatabaseRoot::decode(&id, &required(store, id)?)?;
    let generation =
        |store: &mut dyn ObjectReader, root: &DatabaseRoot| -> Result<u64, SemanticError> {
            let id = root
                .metadata
                .ok_or_else(|| fault("Publication lacks metadata"))?;
            Ok(
                crate::storage::descriptors::SnapshotMetadata::decode(&id, &required(store, id)?)?
                    .generation,
            )
        };
    let before_generation = generation(store, &previous)?;
    let after_generation = generation(store, next)?;
    if previous.identity != next.identity
        || previous.basis > next.basis
        || before_generation > after_generation
    {
        return Err(stale());
    }
    if before_generation == after_generation {
        let before = match previous.log {
            Some(id) => LogRoot::open(store, id)?,
            None => LogRoot::empty(),
        };
        let after = match next.log {
            Some(id) => LogRoot::open(store, id)?,
            None => LogRoot::empty(),
        };
        if !after.extends_prefix(store, &before)? {
            return Err(stale());
        }
    }
    // The log intentionally excludes caller keys and receipt-only outcomes.
    // Equal fact history alone cannot authorize forgetting an acknowledged
    // request on another same-lineage branch. Linear backups retain exact IDs;
    // a newer excision may replace occupancy only with its sanitized decision.
    let previous_requests = RequestIndex::from_root(previous.receipts);
    let next_requests = RequestIndex::from_root(next.receipts);
    let mut after = None;
    loop {
        control.check()?;
        let page = previous_requests.scan(store, after, 128)?;
        if page.is_empty() {
            break;
        }
        for (key, old_id) in &page {
            control.check()?;
            let new_id = next_requests.lookup(store, *key)?.ok_or_else(stale)?;
            if new_id == *old_id {
                continue;
            }
            if before_generation == after_generation {
                return Err(stale());
            }
            let old = Block::decode(old_id, &required(store, *old_id)?)?;
            let basis = if old.kind == crate::storage::excision::TOMBSTONE_KIND {
                if old.payload.len() != 32
                    || !old.links.is_empty()
                    || old.payload[..16] != next.identity
                {
                    return Err(fault("Prior receipt tombstone is malformed"));
                }
                u64::from_be_bytes(old.payload[24..32].try_into().unwrap())
            } else {
                let receipt = ExactReceipt::load(store, *old_id)?;
                if receipt.identity != next.identity {
                    return Err(fault("Prior receipt is foreign"));
                }
                receipt.basis
            };
            if *key == basis_receipt_key(&next.identity, basis) {
                let receipt = ExactReceipt::load(store, new_id)?;
                if receipt.identity != next.identity || receipt.basis != basis {
                    return Err(stale());
                }
            } else {
                let tombstone = Block::decode(&new_id, &required(store, new_id)?)?;
                if tombstone.kind != crate::storage::excision::TOMBSTONE_KIND
                    || !tombstone.links.is_empty()
                    || tombstone.payload.len() != 32
                    || tombstone.payload[..16] != next.identity
                    || u64::from_be_bytes(tombstone.payload[24..32].try_into().unwrap()) != basis
                {
                    return Err(stale());
                }
                let generation = u64::from_be_bytes(tombstone.payload[16..24].try_into().unwrap());
                if generation <= before_generation || generation > after_generation {
                    return Err(stale());
                }
            }
        }
        after = page.last().map(|(key, _)| *key);
    }
    Ok(())
}
