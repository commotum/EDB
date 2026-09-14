//! Resumable, Rust-owned excision over immutable objects. The work reference
//! owns the frozen source and every checkpoint; only the writer can activate
//! the completed successor. Ordinary SQL never interprets these objects.
use super::catalog::{BlockDatabase, identity_string};
use super::descriptors::{IndexDescriptor, SnapshotMetadata};
use super::log::LogRoot;
use super::protection::protection;
use super::receipts::{ExactReceipt, RequestIndex, basis_receipt_key};
use super::root::{Block, DatabaseRoot, DatabaseValueRoot};
use super::{
    BatchOutcome, BlockReader, BlockSnapshot, ObjectId, PgBlockStore, RefChange, RefCondition,
};
use crate::database::{ExcisionCutoff, FrozenExcisionRequest};
use crate::excision::{ExcisionPlan, ExcisionTargetKind, PlannedExcisionPredicate};
use crate::index::tree::{TreeConfig, build_tree};
use crate::{
    Database, DatabaseValue, Digest, ErrorCategory, ExcisionConfig, ExcisionProgress, IndexOrder,
    IndexPrefix, SemanticError, Value, View, sha256, tx_to_t,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub const CHECKPOINT_KIND: u16 = 40;
pub const PLAN_KIND: u16 = 41;
pub const TOMBSTONE_KIND: u16 = 42;
pub const COMPLETION_KIND: u16 = 43;
const ORDERS: [IndexOrder; 4] = [
    IndexOrder::Eavt,
    IndexOrder::Aevt,
    IndexOrder::Avet,
    IndexOrder::Vaet,
];

pub(crate) fn work_key(identity: &[u8; 16]) -> String {
    format!("excision/{}", identity_string(*identity))
}

/// The old key remains occupied, but no original request digest, tempid name,
/// value root, or transaction content is reachable through this decision.
fn tombstone(
    store: &mut PgBlockStore,
    identity: &[u8; 16],
    generation: u64,
    basis: u64,
) -> Result<ObjectId, SemanticError> {
    let mut payload = identity.to_vec();
    u64_into(&mut payload, generation);
    u64_into(&mut payload, basis);
    store.put(
        &Block {
            kind: TOMBSTONE_KIND,
            links: vec![],
            payload,
        }
        .encode()?,
    )
}

pub(crate) fn reject_tombstone_bytes(
    id: ObjectId,
    bytes: &[u8],
    identity: &[u8; 16],
) -> Result<(), SemanticError> {
    // The generic provider already authenticated the bytes. Avoid decoding an
    // ordinary large receipt twice just to classify its fixed kind header.
    if bytes.get(6..8) != Some(&TOMBSTONE_KIND.to_be_bytes()) {
        return Ok(());
    }
    let block = Block::decode(&id, bytes)?;
    if !block.links.is_empty() || block.payload.len() != 32 || &block.payload[..16] != identity {
        return Err(fault(
            "excision/tombstone",
            "Malformed or foreign excision tombstone",
        ));
    }
    Err(SemanticError::conflict(
        "postgres/idempotency-predates-excision",
        "Request key was committed before excision; its original request and receipt were deliberately erased",
    ))
}

#[derive(Clone)]
struct Checkpoint {
    identity: [u8; 16],
    generation: u64,
    phase: u8,
    source: ObjectId,
    plan: ObjectId,
    candidate: ObjectId,
    initial: ObjectId,
    authorization: ObjectId,
    receipts: Option<ObjectId>,
    after: Option<Digest>,
    progress: ExcisionProgress,
}

/// One bounded step at a time. Drop does not abandon or erase the checkpoint;
/// another authorized owner can reopen the same work after interruption.
pub struct ExcisionJob {
    reader: BlockReader,
    database: BlockDatabase,
    config: ExcisionConfig,
    fulltext_limits: crate::FulltextBuildLimits,
    source_condition: RefCondition,
    work_condition: RefCondition,
    state: Checkpoint,
    plan: ExcisionPlan,
}

pub(crate) struct PreparedExcision {
    pub source: DatabaseRoot,
    pub candidate: DatabaseRoot,
    pub work_condition: RefCondition,
    pub progress: ExcisionProgress,
}

pub(crate) fn sync_requests(
    config: &crate::PostgresConnectionConfig,
    database_id: &str,
    through_t: u64,
    control: &crate::MaintenanceControl,
) -> Result<bool, SemanticError> {
    control.check()?;
    let database = operator_database(config, database_id)?;
    control.check()?;
    let reader = BlockReader::connect(config, Default::default())?;
    let snapshot = reader.capture(&database.reference_key())?;
    control.check()?;
    Ok(snapshot.basis_t() >= through_t
        && sync_complete_with_control(&snapshot, through_t, &mut || control.check())?)
}

pub(crate) fn operator_database(
    config: &crate::PostgresConnectionConfig,
    database_id: &str,
) -> Result<BlockDatabase, SemanticError> {
    let entry =
        crate::DatabaseCatalog::connect_configured(config)?.require_active_id(database_id)?;
    // require_active_id already validated the canonical hyphenated UUID.
    let identity = u128::from_str_radix(&entry.lineage_id.replace('-', ""), 16)
        .map_err(|_| {
            fault(
                "excision/database-identity",
                "Catalog returned a malformed stable identity",
            )
        })?
        .to_be_bytes();
    Ok(BlockDatabase {
        identity,
        route: super::catalog::parse_identity(&entry.database_id)?,
        name: entry.name.ok_or_else(|| {
            fault(
                "excision/database-identity",
                "Active catalog identity has no public name",
            )
        })?,
    })
}

impl ExcisionJob {
    pub fn open(
        reader: BlockReader,
        store: &mut PgBlockStore,
        database: BlockDatabase,
        config: ExcisionConfig,
        control: &mut dyn FnMut() -> Result<(), SemanticError>,
    ) -> Result<Option<Self>, SemanticError> {
        Self::open_with_fulltext_limits(
            reader,
            store,
            database,
            config,
            crate::FulltextBuildLimits::default(),
            control,
        )
    }

    /// Admission applies to new and resumed work. It is not persisted in the
    /// checkpoint, so an operator can raise the policy and resume safely.
    pub fn open_with_fulltext_limits(
        reader: BlockReader,
        store: &mut PgBlockStore,
        database: BlockDatabase,
        config: ExcisionConfig,
        fulltext_limits: crate::FulltextBuildLimits,
        control: &mut dyn FnMut() -> Result<(), SemanticError>,
    ) -> Result<Option<Self>, SemanticError> {
        config.validate()?;
        fulltext_limits.validate()?;
        control()?;
        if !config.enabled {
            return Ok(None);
        }
        let capture = reader.capture_reference(&database.reference_key())?;
        let source_condition = capture.source_condition();
        let source = DatabaseRoot::decode_for_identity(
            &capture.root_id(),
            &required(store, capture.root_id())?,
            &database.identity,
        )?;
        let key = work_key(&database.route);
        let old = store.read_ref(&key)?;
        let work_condition = RefCondition {
            key,
            expected: old.as_ref().map(|r| r.revision),
        };
        if let Some(bytes) = old.as_ref().and_then(|r| r.value.as_deref()) {
            let id: Digest = bytes
                .try_into()
                .map_err(|_| fault("excision/work-ref", "Malformed excision work reference"))?;
            let mut state = Checkpoint::load(store, id)?;
            let original = DatabaseRoot::decode_for_identity(
                &state.source,
                &required(store, state.source)?,
                &database.identity,
            )?;
            if state.identity != database.identity {
                return Err(fault(
                    "excision/source-changed",
                    "Pending excision belongs to another canonical source",
                ));
            }
            let plan = decode_plan(store, state.plan, config.max_admitted_bytes)?;
            let mut work_condition = work_condition;
            if !same_source(&source, &original) {
                let original_metadata = metadata(store, &DatabaseValueRoot::from(&original))?;
                let current_metadata = metadata(store, &DatabaseValueRoot::from(&source))?;
                if source.basis < original.basis
                    || current_metadata.generation != original_metadata.generation
                {
                    return Err(fault(
                        "excision/source-changed",
                        "Excision source generation was replaced",
                    ));
                }
                if original.basis > 0 {
                    let old = LogRoot::open(
                        store,
                        original
                            .log
                            .ok_or_else(|| fault("excision/log", "Original log is absent"))?,
                    )?;
                    let current = LogRoot::open(
                        store,
                        source
                            .log
                            .ok_or_else(|| fault("excision/log", "Current log is absent"))?,
                    )?;
                    if current.read_record(store, original.basis)?.map(|r| r.id)
                        != old.latest_entry_id()
                    {
                        return Err(fault(
                            "excision/source-prefix",
                            "Current log does not extend the frozen canonical source",
                        ));
                    }
                }
                state.source = capture.root_id();
                state.after = None;
                if state.phase >= 2 {
                    state.phase = 0;
                }
                let protected =
                    protection(store, &[work_condition.clone(), source_condition.clone()])?;
                store.set_write_protection(Some(protected.clone()))?;
                let saved = (|| {
                    let id = store.put(&state.encode()?)?;
                    let rows = publish(
                        store,
                        &protected.conditions,
                        &[RefChange {
                            key: work_condition.key.clone(),
                            value: Some(id.to_vec()),
                        }],
                    )?;
                    Ok::<_, SemanticError>(rows[0].1.revision)
                })();
                let clear = store.set_write_protection(None);
                let revision = saved?;
                clear?;
                work_condition.expected = Some(revision);
            }
            return Ok(Some(Self {
                reader,
                database,
                config,
                fulltext_limits,
                source_condition,
                work_condition,
                state,
                plan,
            }));
        }
        let snapshot =
            reader.capture_immutable(capture.root_id(), &[capture.source_condition()])?;
        let complete = completed(
            store,
            snapshot.captured_metadata().excision,
            &database.identity,
            snapshot.generation(),
        )?;
        let plan = plan_value(
            &snapshot.database_value(),
            &complete,
            config.max_admitted_bytes,
            control,
        )?;
        if plan.is_empty() {
            return Ok(None);
        }
        let generation = snapshot
            .generation()
            .checked_add(1)
            .ok_or_else(|| fault("excision/generation", "Generation overflows"))?;
        let guards = [source_condition.clone(), work_condition.clone()];
        let protected = protection(store, &guards)?;
        store.set_write_protection(Some(protected.clone()))?;
        let result = (|| {
            let plan_id = store.put(&encode_plan(&plan)?)?;
            let mut all = complete;
            all.extend(
                plan.frozen_predicates()
                    .iter()
                    .map(|p| (p.request.request_t, p.request.request_entity)),
            );
            let completion = put_completion(store, database.identity, generation, &all)?;
            let genesis = Database::bootstrap()?;
            let mut trees = Vec::with_capacity(8);
            for history in [false, true] {
                for order in ORDERS {
                    control()?;
                    let built = build_tree(
                        order,
                        history,
                        genesis.datoms(
                            if history {
                                View::History
                            } else {
                                View::Current
                            },
                            order,
                        ),
                        &TreeConfig::default(),
                    )?;
                    for (_, bytes) in built.nodes.iter() {
                        store.put(bytes)?;
                    }
                    trees.push(built.descriptor);
                }
            }
            let indexes = store.put(
                &IndexDescriptor {
                    identity: database.identity,
                    basis: 0,
                    generation,
                    trees,
                    pending_avet: vec![],
                    avet_work: vec![],
                    fulltext: None,
                }
                .encode()?,
            )?;
            let metadata = store.put(
                &SnapshotMetadata {
                    identity: database.identity,
                    basis: 0,
                    generation,
                    eidx_frontier: genesis.eidx_frontier(),
                    reserved_frontier: genesis.reserved_allocation().unwrap().frontier(),
                    last_tx_instant: None,
                    excision: Some(completion),
                }
                .encode()?,
            )?;
            let initial = DatabaseValueRoot {
                identity: database.identity,
                basis: 0,
                log: None,
                indexes: Some(indexes),
                metadata: Some(metadata),
            };
            let initial_id = store.put(&initial.encode()?)?;
            let authorization =
                super::read_authorization::ReadAuthorization::create(store, &initial, initial_id)?;
            let state = Checkpoint {
                identity: database.identity,
                generation,
                phase: 0,
                source: capture.root_id(),
                plan: plan_id,
                candidate: initial_id,
                initial: initial_id,
                authorization,
                receipts: None,
                after: None,
                progress: ExcisionProgress {
                    phase: "admitted",
                    ..Default::default()
                },
            };
            let id = store.put(&state.encode()?)?;
            let rows = publish(
                store,
                &protected.conditions,
                &[RefChange {
                    key: work_condition.key.clone(),
                    value: Some(id.to_vec()),
                }],
            )?;
            let work_condition = RefCondition {
                key: work_condition.key.clone(),
                expected: Some(rows[0].1.revision),
            };
            Ok(Some(Self {
                reader: reader.clone(),
                database: database.clone(),
                config,
                fulltext_limits: fulltext_limits.clone(),
                source_condition: source_condition.clone(),
                work_condition,
                state,
                plan: plan.clone(),
            }))
        })();
        let clear = store.set_write_protection(None);
        match (result, clear) {
            (Err(e), _) | (_, Err(e)) => Err(e),
            (Ok(v), Ok(())) => Ok(v),
        }
    }

    pub fn progress(&self) -> ExcisionProgress {
        self.state.progress.clone()
    }
    pub(crate) fn generation(&self) -> u64 {
        self.state.generation
    }
    pub(crate) fn request_count(&self) -> u64 {
        self.plan.predicates_len() as u64
    }
    pub(crate) fn source(&self, store: &mut PgBlockStore) -> Result<DatabaseRoot, SemanticError> {
        DatabaseRoot::decode_for_identity(
            &self.state.source,
            &required(store, self.state.source)?,
            &self.database.identity,
        )
    }

    pub fn step(
        &mut self,
        store: &mut PgBlockStore,
        control: &mut dyn FnMut() -> Result<(), SemanticError>,
    ) -> Result<bool, SemanticError> {
        control()?;
        if self.state.phase == 3 {
            return Ok(true);
        }
        if self.state.phase == 1 {
            return self.index_step(store, control);
        }
        let protected = protection(
            store,
            &[self.source_condition.clone(), self.work_condition.clone()],
        )?;
        store.set_write_protection(Some(protected.clone()))?;
        let old = self.state.clone();
        let result = (|| {
            match self.state.phase {
                0 => self.rewrite_step(store, control)?,
                2 => self.receipt_step(store, control)?,
                _ => return Err(fault("excision/phase", "Invalid checkpoint phase")),
            }
            self.save(store, &protected.conditions)?;
            Ok(self.state.phase == 3)
        })();
        let clear = store.set_write_protection(None);
        match (result, clear) {
            (Err(e), _) | (_, Err(e)) => {
                self.state = old;
                Err(e)
            }
            (Ok(v), Ok(())) => Ok(v),
        }
    }

    fn rewrite_step(
        &mut self,
        store: &mut PgBlockStore,
        control: &mut dyn FnMut() -> Result<(), SemanticError>,
    ) -> Result<(), SemanticError> {
        let source = self.source(store)?;
        let mut candidate = value(store, self.state.candidate)?;
        if candidate.basis == source.basis {
            self.state.phase = 2;
            return Ok(());
        }
        let source_log = LogRoot::open(
            store,
            source
                .log
                .ok_or_else(|| fault("excision/log", "Source has no transaction log"))?,
        )?;
        let mut log = match candidate.log {
            Some(id) => LogRoot::open(store, id)?,
            None => LogRoot::empty(),
        };
        let mut metadata = metadata(store, &candidate)?;
        let mut receipts = RequestIndex::from_root(self.state.receipts);
        let mut admitted = 0u64;
        let end = source.basis.min(
            candidate
                .basis
                .saturating_add(self.config.log_batch_transactions as u64),
        );
        // Source reads use the job's existing authenticated reader while writes
        // keep their guarded provider. Frozen source IDs, not a fresh database
        // capture, determine this range. This also retains normal source-read
        // telemetry and avoids holding the write connection inside the cursor.
        let source_reader = self.reader.clone();
        let mut read_source = |id| source_reader.read_object(id);
        let mut records = source_log.range(&mut read_source, candidate.basis + 1, end + 1)?;
        for t in candidate.basis + 1..=end {
            control()?;
            let record = records
                .next_record_bounded(
                    usize::try_from(
                        self.config.max_admitted_bytes.saturating_sub(1024 * 1024) / 64,
                    )
                    .unwrap_or(usize::MAX),
                )
                .ok_or_else(|| fault("excision/log-gap", "Source transaction is missing"))??;
            let required = record
                .encoded_bytes
                .saturating_mul(64)
                .saturating_add(1024 * 1024);
            if required > self.config.max_admitted_bytes {
                return Err(capacity(required, self.config.max_admitted_bytes));
            }
            if admitted != 0 && admitted.saturating_add(required) > self.config.max_admitted_bytes {
                break;
            }
            admitted = admitted.saturating_add(required);
            self.state.progress.source_transactions += 1;
            self.state.progress.source_payload_bytes = self
                .state
                .progress
                .source_payload_bytes
                .saturating_add(record.encoded_bytes);
            let mut entry = record.entry;
            let before = entry.tx_data.len();
            entry.tx_data.retain(|d| !self.plan.removes(d));
            self.state.progress.removed_datoms = self
                .state
                .progress
                .removed_datoms
                .saturating_add((before - entry.tx_data.len()) as u64);
            let instant = entry
                .tx_data
                .iter()
                .find_map(|d| {
                    (d.added && d.attribute == crate::DB_TX_INSTANT as u32)
                        .then_some(&d.value)
                        .and_then(|v| {
                            if let Value::Instant(i) = v {
                                Some(*i)
                            } else {
                                None
                            }
                        })
                })
                .ok_or_else(|| {
                    fault(
                        "excision/instant",
                        "Rewritten transaction has no protected instant",
                    )
                })?;
            let before = self.state.candidate;
            log = log.append(store, &entry)?;
            metadata.basis = t;
            metadata.eidx_frontier = entry.eidx_frontier;
            metadata.reserved_frontier = entry.reserved_frontier;
            metadata.last_tx_instant = Some(instant);
            candidate.basis = t;
            candidate.log = log.head();
            candidate.metadata = Some(store.put(&metadata.encode()?)?);
            let after = store.put(&candidate.encode()?)?;
            let receipt = ExactReceipt {
                identity: self.state.identity,
                request_digest: sanitized_digest(self.state.generation, t),
                basis: t,
                transaction: log.latest_entry_id().unwrap(),
                before,
                after,
                tempids: BTreeMap::new(),
            }
            .put(store)?;
            receipts =
                receipts.insert(store, basis_receipt_key(&self.state.identity, t), receipt)?;
            self.state.candidate = after;
            self.state.progress.rewritten_transactions += 1;
        }
        self.state.receipts = receipts.root();
        self.state.phase = 1;
        self.state.progress.phase = "rewrite";
        self.state.progress.peak_admitted_bytes =
            self.state.progress.peak_admitted_bytes.max(admitted);
        Ok(())
    }

    fn index_step(
        &mut self,
        store: &mut PgBlockStore,
        control: &mut dyn FnMut() -> Result<(), SemanticError>,
    ) -> Result<bool, SemanticError> {
        let snapshot = self.reader.capture_immutable(
            self.state.candidate,
            &[self.work_condition.clone(), self.source_condition.clone()],
        )?;
        let prepared = super::indexing::IndexInput::new(snapshot)?
            .with_fulltext_build_limits(self.fulltext_limits.clone())?
            .prepare_with_control(store, &TreeConfig::default(), control)?;
        let mut guards = prepared.protection.conditions.clone();
        guards.push(self.work_condition.clone());
        guards.push(self.source_condition.clone());
        let mut protected = prepared.protection.clone();
        protected.conditions = guards.clone();
        store.set_write_protection(Some(protected))?;
        let old = self.state.clone();
        let result = (|| {
            let mut candidate = value(store, self.state.candidate)?;
            candidate.indexes = Some(prepared.descriptor_id);
            self.state.candidate = store.put(&candidate.encode()?)?;
            self.state.authorization = super::read_authorization::ReadAuthorization::retain_index(
                store,
                self.state.authorization,
                &self.state.identity,
                prepared.descriptor_id,
            )?;
            if prepared.descriptor.basis == candidate.basis
                && prepared.descriptor.pending_avet.is_empty()
                && prepared.descriptor.avet_work.is_empty()
            {
                self.state.phase = 0;
            }
            self.state.progress.phase = "index";
            self.save(store, &guards)?;
            Ok(false)
        })();
        let clear = store.set_write_protection(None);
        match (result, clear) {
            (Err(e), _) | (_, Err(e)) => {
                self.state = old;
                Err(e)
            }
            (Ok(v), Ok(())) => Ok(v),
        }
    }

    fn receipt_step(
        &mut self,
        store: &mut PgBlockStore,
        control: &mut dyn FnMut() -> Result<(), SemanticError>,
    ) -> Result<(), SemanticError> {
        let source = self.source(store)?;
        let page = RequestIndex::from_root(source.receipts).scan(
            store,
            self.state.after,
            self.config.log_batch_transactions,
        )?;
        let mut receipts = RequestIndex::from_root(self.state.receipts);
        for (key, id) in &page {
            control()?;
            let bytes = required(store, *id)?;
            let block = Block::decode(id, &bytes)?;
            let basis = if block.kind == TOMBSTONE_KIND {
                if block.payload.len() != 32 || block.payload[..16] != self.state.identity {
                    return Err(fault("excision/tombstone", "Foreign prior tombstone"));
                }
                u64::from_be_bytes(block.payload[24..32].try_into().unwrap())
            } else {
                let receipt = ExactReceipt::load_from_bytes_bounded(
                    store,
                    *id,
                    &bytes,
                    usize::try_from(
                        self.config.max_admitted_bytes.saturating_sub(1024 * 1024) / 64,
                    )
                    .unwrap_or(usize::MAX),
                )?;
                if receipt.identity != self.state.identity {
                    return Err(fault("excision/receipt-identity", "Foreign source receipt"));
                }
                receipt.basis
            };
            if *key != basis_receipt_key(&self.state.identity, basis) {
                let id = tombstone(store, &self.state.identity, self.state.generation, basis)?;
                receipts = receipts.insert(store, *key, id)?;
            }
            self.state.after = Some(*key);
            self.state.progress.rows_staged += 1;
        }
        self.state.receipts = receipts.root();
        self.state.progress.phase = "receipts";
        if page.len() < self.config.log_batch_transactions {
            self.state.phase = 3;
            self.state.progress.phase = "prepared";
        }
        Ok(())
    }

    fn save(
        &mut self,
        store: &mut PgBlockStore,
        guards: &[RefCondition],
    ) -> Result<(), SemanticError> {
        self.state.progress.steps += 1;
        let id = store.put(&self.state.encode()?)?;
        let rows = publish(
            store,
            guards,
            &[RefChange {
                key: self.work_condition.key.clone(),
                value: Some(id.to_vec()),
            }],
        )?;
        self.work_condition.expected = Some(rows[0].1.revision);
        Ok(())
    }
    pub(crate) fn prepared(
        &self,
        store: &mut PgBlockStore,
    ) -> Result<PreparedExcision, SemanticError> {
        if self.state.phase != 3 {
            return Err(fault(
                "excision/incomplete",
                "Excision candidate is not complete",
            ));
        }
        let source = self.source(store)?;
        let value = value(store, self.state.candidate)?;
        Ok(PreparedExcision {
            source,
            candidate: DatabaseRoot {
                identity: value.identity,
                basis: value.basis,
                writer_epoch: 0,
                log: value.log,
                indexes: value.indexes,
                metadata: value.metadata,
                receipts: self.state.receipts,
                read_authorization: Some(self.state.authorization),
            },
            work_condition: self.work_condition.clone(),
            progress: self.progress(),
        })
    }
}

pub(crate) fn same_source(left: &DatabaseRoot, right: &DatabaseRoot) -> bool {
    left.identity == right.identity
        && left.basis == right.basis
        && left.log == right.log
        && left.metadata == right.metadata
        && left.receipts == right.receipts
}
fn sanitized_digest(generation: u64, basis: u64) -> Digest {
    let mut b = b"atomic/excised-outcome/1\0".to_vec();
    u64_into(&mut b, generation);
    u64_into(&mut b, basis);
    sha256(&b)
}
fn value(store: &mut PgBlockStore, id: ObjectId) -> Result<DatabaseValueRoot, SemanticError> {
    DatabaseValueRoot::decode(&id, &required(store, id)?)
}
/// Read the already captured source's generation without reopening its root.
pub(crate) fn source_generation(
    store: &mut PgBlockStore,
    source: &DatabaseRoot,
) -> Result<u64, SemanticError> {
    Ok(metadata(store, &DatabaseValueRoot::from(source))?.generation)
}

fn metadata(
    store: &mut PgBlockStore,
    value: &DatabaseValueRoot,
) -> Result<SnapshotMetadata, SemanticError> {
    let id = value
        .metadata
        .ok_or_else(|| fault("excision/metadata", "Value has no metadata"))?;
    SnapshotMetadata::decode(&id, &required(store, id)?)
}
fn required(store: &mut PgBlockStore, id: ObjectId) -> Result<Vec<u8>, SemanticError> {
    store.get(id)?.ok_or_else(|| {
        fault(
            "excision/missing-object",
            "Protected excision object is missing",
        )
    })
}
fn publish(
    store: &mut PgBlockStore,
    guards: &[RefCondition],
    changes: &[RefChange],
) -> Result<Vec<(String, super::Reference)>, SemanticError> {
    match super::ownership::publish_refs(store, guards, changes)? {
        BatchOutcome::Applied(rows) => Ok(rows),
        BatchOutcome::Conflict(_) => Err(SemanticError::conflict(
            "excision/checkpoint-conflict",
            "Excision source, worker or GC guard changed",
        )),
    }
}
fn capacity(required: u64, maximum: u64) -> SemanticError {
    SemanticError::new(
        ErrorCategory::Busy,
        "excision/admission-capacity",
        "Excision step exceeds its accounted-byte allowance",
    )
    .detail("required_bytes", required.to_string())
    .detail("maximum_bytes", maximum.to_string())
}
fn fault(code: &'static str, message: &str) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, code, message)
}
fn u64_into(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_be_bytes());
}

// Compact codecs and cursor-based predicate planning follow below.
impl Checkpoint {
    fn encode(&self) -> Result<Vec<u8>, SemanticError> {
        let mut p = self.identity.to_vec();
        u64_into(&mut p, self.generation);
        p.push(self.phase);
        p.push(u8::from(self.after.is_some()));
        p.extend_from_slice(&self.after.unwrap_or([0; 32]));
        p.push(u8::from(self.receipts.is_some()));
        for n in [
            self.progress.steps,
            self.progress.source_transactions,
            self.progress.rewritten_transactions,
            self.progress.source_payload_bytes,
            self.progress.peak_admitted_bytes,
            self.progress.rows_staged,
            self.progress.removed_datoms,
        ] {
            u64_into(&mut p, n);
        }
        let mut links = vec![
            self.source,
            self.plan,
            self.candidate,
            self.initial,
            self.authorization,
        ];
        links.extend(self.receipts);
        Block {
            kind: CHECKPOINT_KIND,
            links,
            payload: p,
        }
        .encode()
    }
    fn load(store: &mut PgBlockStore, id: ObjectId) -> Result<Self, SemanticError> {
        let bytes = required(store, id)?;
        if bytes.len() > 20 + 6 * 32 + 115 {
            return Err(fault("excision/checkpoint", "Oversized checkpoint"));
        }
        let b = Block::decode(&id, &bytes)?;
        let mut p = Input::new(&b.payload);
        let identity = p.take(16)?.try_into().unwrap();
        let generation = p.u64()?;
        let phase = p.byte()?;
        let after_flag = p.byte()?;
        let after_hash = p.take(32)?.try_into().unwrap();
        let receipts_flag = p.byte()?;
        if b.kind != CHECKPOINT_KIND
            || b.links.len() != 5 + usize::from(receipts_flag != 0)
            || phase > 3
            || after_flag > 1
            || receipts_flag > 1
            || generation == 0
            || identity == [0; 16]
            || (after_flag == 0 && after_hash != [0; 32])
        {
            return Err(fault(
                "excision/checkpoint",
                "Invalid checkpoint coordinates",
            ));
        }
        let progress = ExcisionProgress {
            phase: "resumed",
            steps: p.u64()?,
            source_transactions: p.u64()?,
            rewritten_transactions: p.u64()?,
            source_payload_bytes: p.u64()?,
            peak_admitted_bytes: p.u64()?,
            rows_staged: p.u64()?,
            removed_datoms: p.u64()?,
            complete: false,
            fresh_write_pause_nanos: 0,
        };
        p.finish()?;
        Ok(Self {
            identity,
            generation,
            phase,
            source: b.links[0],
            plan: b.links[1],
            candidate: b.links[2],
            initial: b.links[3],
            authorization: b.links[4],
            receipts: (receipts_flag != 0).then(|| b.links[5]),
            after: (after_flag != 0).then_some(after_hash),
            progress,
        })
    }
}
struct Input<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Input<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], SemanticError> {
        let end = self
            .at
            .checked_add(n)
            .filter(|n| *n <= self.bytes.len())
            .ok_or_else(|| fault("excision/encoding", "Truncated excision object"))?;
        let out = &self.bytes[self.at..end];
        self.at = end;
        Ok(out)
    }
    fn byte(&mut self) -> Result<u8, SemanticError> {
        Ok(self.take(1)?[0])
    }
    fn u64(&mut self) -> Result<u64, SemanticError> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn set(&mut self) -> Result<BTreeSet<u64>, SemanticError> {
        let count = self.u64()?;
        if count > (self.bytes.len() - self.at) as u64 / 8 {
            return Err(fault("excision/encoding", "Set length exceeds payload"));
        }
        let mut out = BTreeSet::new();
        let mut last = None;
        for _ in 0..count {
            let n = self.u64()?;
            if last.is_some_and(|v| v >= n) {
                return Err(fault("excision/encoding", "Set is not canonical"));
            }
            last = Some(n);
            out.insert(n);
        }
        Ok(out)
    }
    fn finish(&self) -> Result<(), SemanticError> {
        if self.at == self.bytes.len() {
            Ok(())
        } else {
            Err(fault("excision/encoding", "Trailing excision bytes"))
        }
    }
}
fn put_set(out: &mut Vec<u8>, set: impl ExactSizeIterator<Item = u64>) {
    u64_into(out, set.len() as u64);
    for n in set {
        u64_into(out, n);
    }
}
fn encode_plan(plan: &ExcisionPlan) -> Result<Vec<u8>, SemanticError> {
    let items = plan.frozen_predicates();
    let mut p = Vec::new();
    u64_into(&mut p, items.len() as u64);
    for item in items {
        for n in [
            item.request.request_t,
            item.request.request_entity,
            item.request.target,
        ] {
            u64_into(&mut p, n);
        }
        match item.request.cutoff {
            None => {
                p.push(0);
                u64_into(&mut p, 0)
            }
            Some(ExcisionCutoff::BeforeT(t)) => {
                p.push(1);
                u64_into(&mut p, t)
            }
            Some(ExcisionCutoff::BeforeInstant(i)) => {
                p.push(2);
                p.extend_from_slice(&i.to_be_bytes())
            }
        }
        put_set(&mut p, item.request.attributes.iter().copied());
        p.push(u8::from(item.kind == ExcisionTargetKind::Attribute));
        u64_into(&mut p, item.before_t);
        put_set(&mut p, item.extent.iter().copied());
        put_set(
            &mut p,
            item.reference_attributes.iter().map(|a| u64::from(*a)),
        );
        p.push(u8::from(item.protected_entity_target));
        p.extend_from_slice(&item.hash);
    }
    Block {
        kind: PLAN_KIND,
        links: vec![],
        payload: p,
    }
    .encode()
}
fn decode_plan(
    store: &mut PgBlockStore,
    id: ObjectId,
    maximum: u64,
) -> Result<ExcisionPlan, SemanticError> {
    let bytes = required(store, id)?;
    let account = (bytes.len() as u64)
        .saturating_mul(64)
        .saturating_add(1024 * 1024);
    if account > maximum {
        return Err(capacity(account, maximum));
    }
    let b = Block::decode(&id, &bytes)?;
    if b.kind != PLAN_KIND || !b.links.is_empty() {
        return Err(fault("excision/plan", "Invalid plan object"));
    }
    let mut p = Input::new(&b.payload);
    let count = p.u64()?;
    if count > b.payload.len() as u64 / 100 {
        return Err(fault("excision/plan", "Invalid predicate count"));
    }
    let mut items = Vec::new();
    for _ in 0..count {
        let request_t = p.u64()?;
        let request_entity = p.u64()?;
        let target = p.u64()?;
        let cutoff_kind = p.byte()?;
        let cutoff_value = p.u64()?;
        let cutoff = match cutoff_kind {
            0 if cutoff_value == 0 => None,
            1 => Some(ExcisionCutoff::BeforeT(cutoff_value)),
            2 => Some(ExcisionCutoff::BeforeInstant(cutoff_value as i64)),
            _ => return Err(fault("excision/plan", "Invalid cutoff")),
        };
        let attributes = p.set()?;
        let kind = match p.byte()? {
            0 => ExcisionTargetKind::Entity,
            1 => ExcisionTargetKind::Attribute,
            _ => return Err(fault("excision/plan", "Invalid predicate kind")),
        };
        let before_t = p.u64()?;
        let extent = p.set()?;
        let reference_attributes = p
            .set()?
            .into_iter()
            .map(|a| {
                u32::try_from(a).map_err(|_| fault("excision/plan", "Invalid reference attribute"))
            })
            .collect::<Result<_, _>>()?;
        let protected_entity_target = match p.byte()? {
            0 => false,
            1 => true,
            _ => return Err(fault("excision/plan", "Invalid keeper marker")),
        };
        let hash = p.take(32)?.try_into().unwrap();
        if hash == [0; 32] {
            return Err(fault("excision/plan", "Missing predicate commitment"));
        }
        items.push(PlannedExcisionPredicate {
            request: FrozenExcisionRequest {
                request_entity,
                request_t,
                target,
                attributes,
                cutoff,
            },
            kind,
            before_t,
            extent,
            reference_attributes,
            protected_entity_target,
            hash,
        });
    }
    p.finish()?;
    ExcisionPlan::from_materialized(items)
}
fn put_completion(
    store: &mut PgBlockStore,
    identity: [u8; 16],
    generation: u64,
    requests: &BTreeSet<(u64, u64)>,
) -> Result<ObjectId, SemanticError> {
    let mut p = identity.to_vec();
    u64_into(&mut p, generation);
    u64_into(&mut p, requests.len() as u64);
    for (t, e) in requests {
        u64_into(&mut p, *t);
        u64_into(&mut p, *e);
    }
    store.put(
        &Block {
            kind: COMPLETION_KIND,
            links: vec![],
            payload: p,
        }
        .encode()?,
    )
}
fn completed(
    store: &mut PgBlockStore,
    id: Option<ObjectId>,
    identity: &[u8; 16],
    generation: u64,
) -> Result<BTreeSet<(u64, u64)>, SemanticError> {
    let Some(id) = id else {
        return Ok(BTreeSet::new());
    };
    let bytes = required(store, id)?;
    completion_bytes(id, &bytes, identity, generation)
}
fn completion_bytes(
    id: ObjectId,
    bytes: &[u8],
    identity: &[u8; 16],
    generation: u64,
) -> Result<BTreeSet<(u64, u64)>, SemanticError> {
    let b = Block::decode(&id, bytes)?;
    let mut p = Input::new(&b.payload);
    if b.kind != COMPLETION_KIND
        || !b.links.is_empty()
        || p.take(16)? != identity
        || p.u64()? != generation
    {
        return Err(fault(
            "excision/completion",
            "Completion identity or generation differs",
        ));
    }
    let count = p.u64()?;
    if count > (b.payload.len().saturating_sub(32) / 16) as u64 {
        return Err(fault("excision/completion", "Invalid completion count"));
    }
    let mut set = BTreeSet::new();
    let mut last = None;
    for _ in 0..count {
        let next = (p.u64()?, p.u64()?);
        if last.is_some_and(|v| v >= next) {
            return Err(fault(
                "excision/completion",
                "Completion identities are not sorted",
            ));
        }
        last = Some(next);
        set.insert(next);
    }
    p.finish()?;
    Ok(set)
}

pub(crate) fn sync_complete(snapshot: &BlockSnapshot, target: u64) -> Result<bool, SemanticError> {
    sync_complete_with_control(snapshot, target, &mut || Ok(()))
}

fn sync_complete_with_control(
    snapshot: &BlockSnapshot,
    target: u64,
    control: &mut dyn FnMut() -> Result<(), SemanticError>,
) -> Result<bool, SemanticError> {
    control()?;
    let set = match snapshot.captured_metadata().excision {
        Some(id) => completion_bytes(
            id,
            &snapshot.read_object(id)?,
            &snapshot.captured_root().identity,
            snapshot.generation(),
        )?,
        None => BTreeSet::new(),
    };
    for d in snapshot
        .database_value()
        .history()
        .prefix_cursor(&IndexPrefix::Aevt {
            attribute: crate::DB_EXCISE as u32,
            entity: None,
            value: None,
        })?
    {
        control()?;
        let d = d?;
        let t = tx_to_t(d.tx)?;
        if d.added && t <= target && !set.contains(&(t, d.entity)) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn plan_value(
    value: &DatabaseValue,
    complete: &BTreeSet<(u64, u64)>,
    maximum: u64,
    control: &mut dyn FnMut() -> Result<(), SemanticError>,
) -> Result<ExcisionPlan, SemanticError> {
    let mut requests = BTreeMap::new();
    let mut account = 1024 * 1024u64;
    for d in value.prefix_cursor(&IndexPrefix::Aevt {
        attribute: crate::DB_EXCISE as u32,
        entity: None,
        value: None,
    })? {
        control()?;
        let d = d?;
        let t = tx_to_t(d.tx)?;
        if complete.contains(&(t, d.entity)) {
            continue;
        }
        account = account.saturating_add(512);
        if account > maximum {
            return Err(capacity(account, maximum));
        }
        requests.insert((t, d.entity), d);
    }
    let mut predicates = Vec::new();
    for ((request_t, request_entity), _) in requests {
        control()?;
        let as_of = value.clone().as_of(request_t);
        let datoms = as_of.prefix_cursor(&IndexPrefix::Eavt {
            entity: request_entity,
            attribute: None,
            value: None,
        })?;
        let mut target = None;
        let mut attributes = BTreeSet::new();
        let mut cutoff = None;
        for d in datoms {
            control()?;
            let d = d?;
            if matches!(
                u64::from(d.attribute),
                crate::DB_EXCISE
                    | crate::DB_EXCISE_ATTRS
                    | crate::DB_EXCISE_BEFORE
                    | crate::DB_EXCISE_BEFORE_T
            ) {
                account = account.saturating_add(256);
                if account > maximum {
                    return Err(capacity(account, maximum));
                }
            }
            match (d.attribute, &d.value) {
                (a, Value::Ref(e)) if a == crate::DB_EXCISE as u32 => {
                    if target.replace(*e).is_some() {
                        return Err(fault("excision/request", "Multiple frozen targets"));
                    }
                }
                (a, Value::Ref(e)) if a == crate::DB_EXCISE_ATTRS as u32 => {
                    attributes.insert(*e);
                }
                (a, Value::Long(t)) if a == crate::DB_EXCISE_BEFORE_T as u32 => {
                    let raw = u64::try_from(*t)
                        .map_err(|_| fault("excision/request", "Negative cutoff"))?;
                    let t = if crate::eid_to_part(raw).is_ok_and(|p| p == crate::TX_PARTITION) {
                        tx_to_t(raw)?
                    } else {
                        crate::t_to_tx(raw)?;
                        raw
                    };
                    if cutoff.replace(ExcisionCutoff::BeforeT(t)).is_some() {
                        return Err(fault("excision/request", "Conflicting cutoffs"));
                    }
                }
                (a, Value::Instant(i)) if a == crate::DB_EXCISE_BEFORE as u32 => {
                    if cutoff.replace(ExcisionCutoff::BeforeInstant(*i)).is_some() {
                        return Err(fault("excision/request", "Conflicting cutoffs"));
                    }
                }
                _ => {}
            }
        }
        let target = target.ok_or_else(|| fault("excision/request", "Frozen target is missing"))?;
        let request = FrozenExcisionRequest {
            request_entity,
            request_t,
            target,
            attributes,
            cutoff,
        };
        let kind = if request.attributes.is_empty()
            && crate::eid_to_part(target).is_ok_and(|p| p == crate::DB_PARTITION)
            && u32::try_from(target)
                .ok()
                .is_some_and(|a| value.schema().attribute(a).is_ok())
        {
            ExcisionTargetKind::Attribute
        } else {
            ExcisionTargetKind::Entity
        };
        let protected_entity_target = kind == ExcisionTargetKind::Entity
            && crate::eid_to_part(target).is_ok_and(|p| p == crate::DB_PARTITION);
        let before_t = match cutoff {
            None => request_t,
            Some(ExcisionCutoff::BeforeT(t)) => t.min(request_t),
            Some(ExcisionCutoff::BeforeInstant(i)) => {
                let mut boundary = request_t;
                for d in value.prefix_cursor(&IndexPrefix::Avet {
                    attribute: crate::DB_TX_INSTANT as u32,
                    value: None,
                    entity: None,
                })? {
                    control()?;
                    let d = d?;
                    if matches!(d.value,Value::Instant(n) if n>=i) {
                        boundary = tx_to_t(d.tx)?.min(request_t);
                        break;
                    }
                }
                boundary
            }
        };
        let reference_attributes = value
            .schema()
            .attributes()
            .filter(|a| a.value_type == crate::ValueType::Ref)
            .map(|a| a.id)
            .collect::<BTreeSet<_>>();
        let mut extent = BTreeSet::from([target]);
        let mut queue = VecDeque::from([(target, true)]);
        if kind == ExcisionTargetKind::Entity && !protected_entity_target {
            while let Some((entity, first)) = queue.pop_front() {
                for d in
                    value
                        .clone()
                        .history()
                        .as_of(request_t)
                        .prefix_cursor(&IndexPrefix::Eavt {
                            entity,
                            attribute: None,
                            value: None,
                        })?
                {
                    control()?;
                    let d = d?;
                    if !reference_attributes.contains(&d.attribute)
                        || !value.schema().attribute(d.attribute)?.component
                        || (first
                            && !request.attributes.is_empty()
                            && !request.attributes.contains(&u64::from(d.attribute)))
                    {
                        continue;
                    }
                    if let Value::Ref(child) = d.value
                        && extent.insert(child)
                    {
                        account = account.saturating_add(256);
                        if account > maximum {
                            return Err(capacity(account, maximum));
                        }
                        queue.push_back((child, false));
                    }
                }
            }
        }
        account = account
            .saturating_add((reference_attributes.len() + request.attributes.len()) as u64 * 128);
        if account > maximum {
            return Err(capacity(account, maximum));
        }
        predicates.push(PlannedExcisionPredicate {
            request,
            kind,
            before_t,
            extent,
            reference_attributes,
            protected_entity_target,
            hash: [0; 32],
        });
    }
    let plan = ExcisionPlan::from_materialized(predicates)?;
    let account = (encode_plan(&plan)?.len() as u64)
        .saturating_mul(64)
        .saturating_add(1024 * 1024);
    if account > maximum {
        return Err(capacity(account, maximum));
    }
    Ok(plan)
}
