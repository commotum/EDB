//! Canonical writer authority, fresh transaction publication and guarded adoption.
//! Candidate preparation and opaque storage remain shared mechanisms; only this
//! owner validates the current writer lease and publishes its successor.
use crate::index::cursor::MergeSource;
use crate::storage::catalog::identity_string;
use crate::storage::descriptors::SnapshotMetadata;
use crate::storage::log::{LogEntry, LogRoot, MAX_LOG_TRANSACTION_BYTES};
use crate::storage::protection::{GC_REFERENCE, protection};
use crate::storage::receipts::{ExactReceipt, RequestIndex, scoped_request_key};
use crate::storage::root::{DatabaseRoot, DatabaseValueRoot};
use crate::storage::{
    BatchOutcome, BlockDatabase, BlockReadConfig, BlockReader, BlockSnapshot, ObjectId,
    PgBlockStore, RefChange, RefCondition, Reference,
};
use crate::{
    CapacityLimits, DatabaseValue, ErrorCategory, PostgresConnectionConfig, SemanticError,
    ServiceTransactionReport, SpeculationLimits, TransactionExecutionOptions, TransactionRequest,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// The route is shared catalog identity; its writer-reference convention is
// authority policy and does not require starting a writer to inspect a lease.
impl BlockDatabase {
    pub(crate) fn lease_key(&self) -> String {
        format!("writers/{}", identity_string(self.route))
    }
}

#[derive(Clone, Debug)]
pub struct BlockWriterOptions {
    pub lease_duration: Duration,
    pub capacity: CapacityLimits,
    pub reads: BlockReadConfig,
    pub execution: TransactionExecutionOptions,
}
impl Default for BlockWriterOptions {
    fn default() -> Self {
        Self {
            lease_duration: Duration::from_secs(30),
            capacity: CapacityLimits::default(),
            reads: BlockReadConfig::default(),
            execution: TransactionExecutionOptions::default(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Lease {
    epoch: u64,
    token: [u8; 16],
    expires_ms: i64,
}
impl Lease {
    fn encode(&self) -> Vec<u8> {
        let mut bytes = b"EDBW\x01".to_vec();
        bytes.extend_from_slice(&self.epoch.to_be_bytes());
        bytes.extend_from_slice(&self.token);
        bytes.extend_from_slice(&self.expires_ms.to_be_bytes());
        bytes
    }
    fn decode(bytes: &[u8]) -> Result<Self, SemanticError> {
        if bytes.len() != 37 || &bytes[..5] != b"EDBW\x01" {
            return Err(fault(
                "storage/writer-lease",
                "Malformed current writer lease",
            ));
        }
        let value = Self {
            epoch: u64::from_be_bytes(bytes[5..13].try_into().unwrap()),
            token: bytes[13..29].try_into().unwrap(),
            expires_ms: i64::from_be_bytes(bytes[29..37].try_into().unwrap()),
        };
        if value.epoch == 0 || value.token == [0; 16] {
            return Err(fault("storage/writer-lease", "Invalid writer epoch/token"));
        }
        Ok(value)
    }
}

/// One serialized writer. Concurrent callers serialize outside this owner;
/// any replacement writer must claim a new epoch by the same guarded root CAS.
pub struct BlockTransactor {
    store: PgBlockStore,
    reader: BlockReader,
    database: BlockDatabase,
    options: BlockWriterOptions,
    lease: Lease,
    lease_revision: u64,
    current: Option<BlockSnapshot>,
    publication_revision: u64,
    last_read_work: crate::database_value::read_context::TransactionReadWork,
    last_assessment_work: crate::transaction::assess::AssessmentReadWork,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct IndexBacklogEntry {
    pub basis_t: u64,
    pub datoms: u64,
    pub accounted_bytes: u64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct IndexAdoption {
    pub indexed_basis: u64,
    pub publication_revision: u64,
    pub pending_avet: u64,
}
impl BlockTransactor {
    pub fn claim(
        config: &PostgresConnectionConfig,
        database: BlockDatabase,
        options: BlockWriterOptions,
    ) -> Result<Self, SemanticError> {
        let duration = lease_millis(options.lease_duration)?;
        let mut store = PgBlockStore::connect(config)?;
        let reader = BlockReader::connect(config, options.reads.clone())?;
        let capture = reader.capture_reference(&database.reference_key())?;
        let result = (|| {
            let root = DatabaseRoot::decode_for_identity(
                &capture.root_id(),
                &required(&mut store, capture.root_id())?,
                &database.identity,
            )?;
            let lease_key = database.lease_key();
            let old_lease = store.read_ref(&lease_key)?;
            let now = now_ms()?;
            if let Some(bytes) = old_lease.as_ref().and_then(|r| r.value.as_deref()) {
                let lease = Lease::decode(bytes)?;
                if lease.expires_ms > now {
                    return Err(SemanticError::new(
                        ErrorCategory::Busy,
                        "storage/writer-active",
                        "Another writer lease is active",
                    ));
                }
            }
            let root = root.claim_writer()?;
            let lease = Lease {
                epoch: root.writer_epoch,
                token: crate::uuid_v7()?.to_be_bytes(),
                expires_ms: now
                    .checked_add(duration)
                    .ok_or_else(|| fault("storage/lease-time", "Lease expiry overflows"))?,
            };
            let guards = vec![
                capture.source_condition(),
                condition(&lease_key, old_lease.as_ref()),
            ];
            let protection = protection(&mut store, &guards)?;
            store.set_write_protection(Some(protection.clone()))?;
            let id = store.put(&root.encode()?)?;
            let changes = [
                RefChange {
                    key: database.reference_key(),
                    value: Some(id.to_vec()),
                },
                RefChange {
                    key: lease_key.clone(),
                    value: Some(lease.encode()),
                },
            ];
            let outcome = match crate::storage::ownership::publish_refs(
                &mut store,
                &protection.conditions,
                &changes,
            ) {
                Ok(outcome) => outcome,
                Err(error) if error.category == ErrorCategory::UnknownOutcome => {
                    // Acknowledgement loss must not strand a successfully
                    // acquired owner until timeout. Reconnect and recognize
                    // only this exact token; do not issue another claim.
                    let mut connected = PgBlockStore::connect(config).map_err(|_| error.clone())?;
                    let owned = connected
                        .read_ref(&lease_key)
                        .map_err(|_| error.clone())?
                        .filter(|r| r.value.as_deref() == Some(lease.encode().as_slice()));
                    let Some(owned) = owned else {
                        return Err(error);
                    };
                    store = connected;
                    BatchOutcome::Applied(vec![(lease_key.clone(), owned)])
                }
                Err(error) => return Err(error),
            };
            let applied = match outcome {
                BatchOutcome::Applied(applied) => applied,
                BatchOutcome::Conflict(_) => {
                    return Err(conflict(
                        "storage/writer-claim-conflict",
                        "Writer authority changed during claim",
                    ));
                }
            };
            let revision = applied
                .into_iter()
                .find(|(key, _)| key == &lease_key)
                .unwrap()
                .1
                .revision;
            store.set_write_protection(None)?;
            Ok((lease, revision))
        })();
        let (lease, lease_revision) = result?;
        Ok(Self {
            store,
            reader,
            database,
            options,
            lease,
            lease_revision,
            current: None,
            publication_revision: 0,
            last_read_work: Default::default(),
            last_assessment_work: Default::default(),
        })
    }
    pub fn database(&self) -> &BlockDatabase {
        &self.database
    }
    pub fn writer_epoch(&self) -> u64 {
        self.lease.epoch
    }
    pub fn program_cache_stats(&self) -> crate::ProgramCacheStats {
        self.reader.program_cache_stats()
    }
    pub fn set_program_cache_limits(&self, entries: usize, bytes: usize) {
        self.reader.set_program_cache_limits(entries, bytes);
    }
    pub(crate) fn program_cache(&self) -> crate::program_cache::SharedProgramCache {
        self.reader.program_cache()
    }
    pub fn db(&self) -> Result<DatabaseValue, SemanticError> {
        Ok(self
            .reader
            .capture(&self.database.reference_key())?
            .database_value())
    }
    /// Reconnect transport without acquiring new write authority. Receipt
    /// resolution remains possible even if a replacement writer has taken over.
    /// Fresh writes/renewals reconcile the owner token before using a revision.
    pub fn reconnect(&mut self, config: &PostgresConnectionConfig) -> Result<(), SemanticError> {
        let store = PgBlockStore::connect(config)?;
        let reader = BlockReader::connect(config, self.options.reads.clone())?
            .with_program_cache(self.reader.program_cache());
        self.store = store;
        self.reader = reader;
        self.current = None;
        Ok(())
    }
    pub fn resolve_request_outcome(
        &mut self,
        request_key: &str,
        digest: ObjectId,
    ) -> Result<Option<ServiceTransactionReport>, SemanticError> {
        self.reader
            .resolve_request_outcome(&self.database, request_key, digest)
    }
    pub(crate) fn activate(
        &mut self,
    ) -> Result<(crate::RecoveryStats, crate::WriterResidencyStats), SemanticError> {
        let capture = self
            .reader
            .capture_reference(&self.database.reference_key())?;
        let root = self.load_root(capture.root_id())?;
        self.check_epoch(&root)?;
        self.refresh_owned_lease()?;
        let snapshot = self.reader.capture_root(&capture)?;
        let base_t = snapshot.indexed_basis_t();
        let target_t = snapshot.basis_t();
        self.publication_revision = capture.source_revision();
        self.current = Some(snapshot);
        Ok((
            crate::RecoveryStats {
                base_t,
                target_t,
                tail_transactions: target_t - base_t,
                tail_range_reads: u64::from(target_t > base_t),
            },
            self.writer_residency_stats(),
        ))
    }
    pub(crate) fn hint_database_value(&self) -> Option<DatabaseValue> {
        self.current.as_ref().map(BlockSnapshot::database_value)
    }
    pub(crate) fn current_index_id(&self) -> Option<ObjectId> {
        self.current
            .as_ref()
            .and_then(|snapshot| snapshot.captured_root().indexes)
    }
    pub(crate) fn index_backlog(&self) -> Vec<IndexBacklogEntry> {
        self.current.as_ref().map_or_else(Vec::new, |snapshot| {
            snapshot
                .recent_tier()
                .entries()
                .iter()
                .map(|entry| IndexBacklogEntry {
                    basis_t: entry.transaction.basis_t,
                    datoms: entry.transaction.tx_data.len() as u64,
                    accounted_bytes: entry.accounted_bytes,
                })
                .collect()
        })
    }
    /// A pure scheduling signal, consumed only after the exact-receipt lookup.
    /// Leave headroom for unknown expansion size without evaluating callbacks.
    /// Empty tails admit one attempt; an individually oversized transaction is
    /// rejected by actual assessment/accounting, never parked forever.
    pub(crate) fn fresh_tail_admission(&self) -> Result<(), SemanticError> {
        let Some(snapshot) = &self.current else {
            return Ok(());
        };
        let tail = snapshot.recent_tier().stats();
        let limits = &self.options.reads;
        if tail.transactions != 0
            && (tail.transactions >= (limits.max_recent_transactions as u64).max(1)
                || tail.datoms >= (limits.max_recent_datoms as u64 / 2).max(1)
                || tail.accounted_bytes >= (limits.max_recent_bytes as u64 / 2).max(1))
        {
            return Err(SemanticError::new(
                ErrorCategory::Busy,
                "service/index-backpressure",
                "Fresh work waits for durable index consolidation",
            ));
        }
        Ok(())
    }
    pub(crate) fn indexed_basis_t(&self) -> u64 {
        self.current
            .as_ref()
            .map_or(0, BlockSnapshot::indexed_basis_t)
    }
    pub(crate) fn pending_avet_projections(&self) -> u64 {
        self.current.as_ref().map_or(0, |s| {
            let descriptor = s.index_descriptor();
            (descriptor.avet_work.len().max(s.avet_unready().len())) as u64
        })
    }
    pub(crate) fn index_input(
        &mut self,
    ) -> Result<crate::storage::indexing::IndexInput, SemanticError> {
        if self.current.is_none() {
            self.activate()?;
        } else if let Some(after) = self.reader.capture_changed(
            &self.database.reference_key(),
            self.current.as_ref().unwrap(),
        )? {
            self.publication_revision = after.publication_revision().ok_or_else(|| {
                fault(
                    "storage/index-publication",
                    "Captured index source has no revision",
                )
            })?;
            self.current = Some(after);
        }
        crate::storage::indexing::IndexInput::new(self.current.as_ref().unwrap().clone())
    }

    /// The serialized publication lane adopts a frozen job on the latest
    /// database root. It never publishes the job's stale log/receipt/metadata.
    pub(crate) fn adopt_index(
        &mut self,
        prepared: crate::storage::indexing::PreparedIndex,
    ) -> Result<IndexAdoption, SemanticError> {
        let capture = self
            .reader
            .capture_reference(&self.database.reference_key())?;
        let result = (|| {
            let root = self.load_root(capture.root_id())?;
            self.check_epoch(&root)?;
            self.refresh_owned_lease()?;
            if prepared.through_basis != prepared.descriptor.basis
                || prepared.generation != prepared.descriptor.generation
            {
                return Err(conflict(
                    "storage/index-source-changed",
                    "Prepared index summary differs from its descriptor",
                ));
            }
            let lease = self.lease_condition();
            let publication = crate::storage::index_publication::publish_index(
                &mut self.store,
                &self.reader,
                &capture,
                lease,
                self.lease.epoch,
                crate::storage::index_publication::IndexCandidate {
                    source_indexes: Some(prepared.source_indexes),
                    source_index_basis: prepared.snapshot.indexed_basis_t(),
                    descriptor: &prepared.descriptor,
                    descriptor_id: prepared.descriptor_id,
                    endpoint_entry: prepared.endpoint_entry,
                    protection: &prepared.protection,
                },
            );
            if publication
                .as_ref()
                .is_err_and(|error| error.category == ErrorCategory::Conflict)
            {
                // Preserve receipt-first authority semantics: only a replaced
                // lease token is fencing, not an operator/GC publication race.
                self.refresh_owned_lease()?;
            }
            let publication = publication?;
            self.publication_revision = publication.revision;
            self.current = Some(publication.snapshot);
            Ok(IndexAdoption {
                indexed_basis: prepared.through_basis,
                publication_revision: self.publication_revision,
                pending_avet: self.pending_avet_projections(),
            })
        })();
        let _ = self.store.set_write_protection(None);
        result
    }
    pub(crate) fn writer_residency_stats(&self) -> crate::WriterResidencyStats {
        let resident = self
            .current
            .as_ref()
            .map_or_else(Default::default, BlockSnapshot::residency_stats);
        crate::WriterResidencyStats {
            publication_revision: self.publication_revision,
            last_transaction_read_datoms: self.last_read_work.logical_datoms,
            last_transaction_read_bytes: self.last_read_work.logical_retained_bytes,
            last_schema_transition_attributes: self
                .last_assessment_work
                .schema_transition_attributes,
            last_schema_projection_attributes: self
                .last_assessment_work
                .schema_projection_attributes,
            last_schema_validation_attributes: self
                .last_assessment_work
                .schema_validation
                .attributes,
            last_schema_validation_predicates: self
                .last_assessment_work
                .schema_validation
                .predicates,
            last_schema_validation_tuple_types: self
                .last_assessment_work
                .schema_validation
                .tuple_types,
            last_schema_validation_tuple_constituents: self
                .last_assessment_work
                .schema_validation
                .tuple_constituents,
            last_schema_validation_installations: self
                .last_assessment_work
                .schema_validation
                .installations,
            last_schema_reuses: self.last_assessment_work.schema_reuses,
            last_schema_dependency_lookups: self.last_assessment_work.dependency_lookups,
            last_schema_dependency_edges: self.last_assessment_work.dependency_edges,
            last_schema_composite_candidates: self.last_assessment_work.composite_candidates,
            last_transaction_source_read_datoms: self.last_read_work.source_datoms,
            last_transaction_source_read_bytes: self.last_read_work.source_retained_bytes,
            last_transaction_prefix_memo_hits: self.last_read_work.prefix_hits,
            last_transaction_prefix_memo_misses: self.last_read_work.prefix_misses,
            last_transaction_prefix_memo_admissions: self.last_read_work.memo_admissions,
            last_transaction_prefix_memo_rejections: self.last_read_work.memo_rejections,
            last_transaction_prefix_memo_peak_entries: self.last_read_work.memo_peak_entries,
            last_transaction_prefix_memo_peak_bytes: self.last_read_work.memo_peak_retained_bytes,
            last_native_cursor_ranges: self.last_read_work.native_cursor_ranges,
            last_native_cache_hits: self.last_read_work.native_cache_hits,
            last_native_cache_misses: self.last_read_work.native_cache_misses,
            last_native_sql_root_reads: self.last_read_work.native_sql_root_reads,
            last_native_sql_directory_reads: self.last_read_work.native_sql_directory_reads,
            last_native_sql_leaf_reads: self.last_read_work.native_sql_leaf_reads,
            last_native_sql_reads: self.last_read_work.native_sql_reads,
            last_native_sql_read_bytes: self.last_read_work.native_sql_read_bytes,
            last_native_recent_datoms_examined: self.last_read_work.native_recent_datoms_examined,
            last_native_recent_datoms_yielded: self.last_read_work.native_recent_datoms_yielded,
            ..resident
        }
    }
    /// The timestamp is takeover eligibility, not an unfenced second authority.
    /// Root and lease revisions fence every publication even under clock skew.
    pub fn renew(&mut self) -> Result<(), SemanticError> {
        for _ in 0..4 {
            let capture = self
                .reader
                .capture_reference(&self.database.reference_key())?;
            let result = (|| {
                let root = self.load_root(capture.root_id())?;
                self.check_epoch(&root)?;
                self.refresh_owned_lease()?;
                let mut lease = self.lease.clone();
                lease.expires_ms = now_ms()?
                    .checked_add(lease_millis(self.options.lease_duration)?)
                    .ok_or_else(|| fault("storage/lease-time", "Lease expiry overflows"))?;
                let guards = [capture.source_condition(), self.lease_condition()];
                match self.store.compare_exchange_many(
                    &guards,
                    &[RefChange {
                        key: self.database.lease_key(),
                        value: Some(lease.encode()),
                    }],
                )? {
                    BatchOutcome::Applied(rows) => {
                        self.lease_revision = rows[0].1.revision;
                        self.lease = lease;
                        Ok(true)
                    }
                    BatchOutcome::Conflict(_) => {
                        // Maintenance can replace only read authorization while
                        // this same writer still owns the lease. Recapture that
                        // root; only a changed lease token/epoch fences the writer.
                        self.refresh_owned_lease()?;
                        Ok(false)
                    }
                }
            })();
            if result? {
                return Ok(());
            }
        }
        Err(SemanticError::new(
            ErrorCategory::Busy,
            "storage/renewal-contention",
            "Concurrent root maintenance prevented lease renewal within this attempt",
        ))
    }
    pub fn transact(
        &mut self,
        request: &TransactionRequest,
    ) -> Result<ServiceTransactionReport, SemanticError> {
        self.transact_with_fresh_gate(request, || Ok(()))
    }

    pub(crate) fn transact_with_fresh_gate(
        &mut self,
        request: &TransactionRequest,
        gate: impl FnOnce() -> Result<(), SemanticError>,
    ) -> Result<ServiceTransactionReport, SemanticError> {
        let operation = crate::OperationContext::current_or_process();
        let _transaction_phase = operation.phase(crate::OperationKind::Transaction);
        // Canonical format admission is fixed. Mutable configured capacity and
        // preconditions are intentionally checked only after receipt lookup.
        let (digest, bytes) = crate::encoding::canonical_submission_request(
            &request.forms,
            &request.source,
            request.compare_basis_t,
            request.tx_instant_override,
            MAX_LOG_TRANSACTION_BYTES,
        )?;
        let key = scoped_request_key(&self.database.identity, &request.request_key)?;
        let capture = self
            .reader
            .capture_reference(&self.database.reference_key())?;
        let result = self.transact_captured(request, digest, bytes, key, &capture, gate);
        let _ = self.store.set_write_protection(None);
        result
    }
    fn transact_captured(
        &mut self,
        request: &TransactionRequest,
        digest: ObjectId,
        request_bytes: usize,
        key: ObjectId,
        capture: &crate::storage::snapshot::RootCapture,
        gate: impl FnOnce() -> Result<(), SemanticError>,
    ) -> Result<ServiceTransactionReport, SemanticError> {
        let root = self.load_root(capture.root_id())?;
        let requests = RequestIndex::from_root(root.receipts);
        if let Some(receipt) = requests.lookup(&mut self.store, key)? {
            let report = self.replay(receipt, digest, capture)?;
            return Ok(report);
        }
        gate()?;
        self.check_epoch(&root)?;
        self.refresh_owned_lease()?;
        if request
            .compare_basis_t
            .is_some_and(|basis| basis != root.basis)
        {
            return Err(conflict(
                "postgres/stale-basis",
                "Expected basis differs from the serialized database value",
            ));
        }
        if request_bytes > self.options.capacity.max_transaction_bytes
            || root.basis >= self.options.capacity.max_history_transactions
        {
            return Err(SemanticError::new(
                ErrorCategory::Busy,
                "storage/transaction-capacity",
                "Fresh transaction exceeds configured capacity",
            ));
        }
        let before = if let Some(snapshot) = &self.current
            && snapshot.captured_root() == &DatabaseValueRoot::from(&root)
        {
            snapshot.clone()
        } else {
            self.reader.capture_root(capture)?
        };
        let limits = SpeculationLimits {
            max_operations: self.options.capacity.max_transaction_ops,
            max_read_datoms: self.options.capacity.max_transaction_read_datoms,
            max_read_bytes: self.options.capacity.max_transaction_read_bytes,
            program: self.options.capacity.program,
            ..Default::default()
        };
        let mut assessed = crate::transaction::pipeline::assess_durable_forms(
            &before.database_value(),
            &request.forms,
            now_ms()?,
            request.tx_instant_override,
            &request.source,
            limits,
            &self.options.execution,
        )?;
        let instant = assessed.db_after.last_tx_instant().ok_or_else(|| {
            fault(
                "storage/transaction-instant",
                "Assessed transaction has no instant",
            )
        })?;
        let operation = crate::OperationContext::current_or_process();
        let encoding_phase = operation.phase(crate::OperationKind::TransactionEncoding);
        let guards = [capture.source_condition(), self.lease_condition()];
        let protection = protection(&mut self.store, &guards)?;
        self.store.set_write_protection(Some(protection.clone()))?;
        let writer_epoch = self.lease.epoch;
        let (log, transaction, metadata, after_root, after_id, next_id) =
            crate::storage::object_io::with_staged_transaction(
                &mut self.store,
                &protection,
                |objects| {
                    // Newly referenced code, including dormant fixed dependencies, is
                    // protected before it can be published. Prior code is protected by the
                    // captured db-before; deduplicated orphan code still needs this touch.
                    for (hash, program) in &assessed.retained_programs {
                        if objects.put_object(&crate::encode_program(program.program())?)? != *hash
                        {
                            return Err(fault(
                                "storage/program-identity",
                                "Validated program identity changed",
                            ));
                        }
                    }
                    let reserved = assessed
                        .db_after
                        .reserved_allocation()?
                        .ok_or_else(|| {
                            fault(
                                "storage/allocation",
                                "Proposed value has no reserved checkpoint",
                            )
                        })?
                        .frontier();
                    let prior_log = before
                        .captured_log()
                        .cloned()
                        .unwrap_or_else(LogRoot::empty);
                    let (log, canonical_data) = prior_log.append_canonical(
                        objects,
                        &LogEntry {
                            basis_t: assessed.basis_t,
                            eidx_frontier: assessed.eidx_frontier,
                            reserved_frontier: reserved,
                            tx_data: std::mem::take(&mut assessed.tx_data),
                        },
                    )?;
                    assessed.tx_data = canonical_data;
                    let transaction = log.latest_entry_id().ok_or_else(|| {
                        fault("storage/log-entry", "Appended log has no transaction")
                    })?;
                    let metadata = SnapshotMetadata {
                        identity: root.identity,
                        basis: assessed.basis_t,
                        generation: before.captured_metadata().generation,
                        eidx_frontier: assessed.eidx_frontier,
                        reserved_frontier: reserved,
                        last_tx_instant: Some(instant),
                        excision: before.captured_metadata().excision,
                    };
                    let metadata_id = objects.put_object(&metadata.encode()?)?;
                    let before_id =
                        objects.put_object(&DatabaseValueRoot::from(&root).encode()?)?;
                    let after_root = DatabaseValueRoot {
                        identity: root.identity,
                        basis: assessed.basis_t,
                        log: log.head(),
                        indexes: root.indexes,
                        metadata: Some(metadata_id),
                    };
                    let after_id = objects.put_object(&after_root.encode()?)?;
                    let receipt_id = ExactReceipt {
                        identity: root.identity,
                        request_digest: digest,
                        basis: assessed.basis_t,
                        transaction,
                        before: before_id,
                        after: after_id,
                        tempids: assessed.tempids.clone(),
                    }
                    .put(objects)?;
                    let receipts = requests.insert(objects, key, receipt_id)?;
                    let receipts = receipts.insert(
                        objects,
                        crate::storage::receipts::basis_receipt_key(
                            &root.identity,
                            assessed.basis_t,
                        ),
                        receipt_id,
                    )?;
                    let next = DatabaseRoot {
                        identity: root.identity,
                        basis: assessed.basis_t,
                        writer_epoch,
                        log: log.head(),
                        indexes: root.indexes,
                        receipts: receipts.root(),
                        metadata: Some(metadata_id),
                        read_authorization: root.read_authorization,
                    };
                    let next_id = objects.put_object(&next.encode()?)?;
                    Ok((log, transaction, metadata, after_root, after_id, next_id))
                },
            )?;
        // The closure's barrier durably authenticates every object, including
        // the receipt and publication root, before these fields can escape.
        // Validate the captured read tail before publication/acknowledgment,
        // retaining the existing protection against a post-commit capture race.
        let after = self.reader.capture_successor(
            &before,
            after_id,
            after_root,
            metadata,
            log,
            transaction,
            assessed.tx_data.clone(),
            &[
                capture.source_condition(),
                protection
                    .conditions
                    .iter()
                    .find(|c| c.key == GC_REFERENCE)
                    .unwrap()
                    .clone(),
            ],
        )?;
        let mut renewed = self.lease.clone();
        renewed.expires_ms = now_ms()?
            .checked_add(lease_millis(self.options.lease_duration)?)
            .ok_or_else(|| fault("storage/lease-time", "Lease expiry overflows"))?;
        drop(encoding_phase);
        let commit_phase = operation.phase(crate::OperationKind::TransactionCommit);
        let applied = crate::storage::ownership::publish_refs(
            &mut self.store,
            &protection.conditions,
            &[
                RefChange {
                    key: self.database.reference_key(),
                    value: Some(next_id.to_vec()),
                },
                RefChange {
                    key: self.database.lease_key(),
                    value: Some(renewed.encode()),
                },
            ],
        )
        .map_err(publication_error)?;
        drop(commit_phase);
        let rows = match applied {
            BatchOutcome::Applied(rows) => rows,
            BatchOutcome::Conflict(_) => {
                // The engine barrier may reject a GC guard before issuing the
                // provider batch. Its observation then need not contain lease
                // state: absence is not evidence that another writer owns it.
                self.refresh_owned_lease()?;
                return Err(conflict(
                    "storage/publication-conflict",
                    "A root or maintenance revision changed; no transaction was published",
                ));
            }
        };
        self.publication_revision = rows
            .iter()
            .find(|(key, _)| key == &self.database.reference_key())
            .unwrap()
            .1
            .revision;
        self.lease_revision = rows
            .iter()
            .find(|(key, _)| key == &self.database.lease_key())
            .unwrap()
            .1
            .revision;
        self.lease = renewed;
        self.current = Some(after.clone());
        self.last_read_work = assessed.read_work;
        self.last_assessment_work = assessed.assessment_work;
        let _report_phase = operation.phase(crate::OperationKind::TransactionReport);
        Ok(ServiceTransactionReport {
            db_before: assessed.db_before,
            db_after: after.database_value(),
            basis_t: assessed.basis_t,
            tx_hash: transaction,
            tx_data: assessed.tx_data,
            tempids: assessed.tempids,
            replayed: false,
            diagnostics: None,
        })
    }
    fn replay(
        &mut self,
        id: ObjectId,
        digest: ObjectId,
        capture: &crate::storage::snapshot::RootCapture,
    ) -> Result<ServiceTransactionReport, SemanticError> {
        self.reader
            .replay_receipt(capture, id, digest, self.database.identity)
    }

    /// Revalidate an admitted maintenance source on the serialized writer.
    /// Until this succeeds the service must not park ordinary fresh work.
    pub(crate) fn admit_excision(&mut self, source: &DatabaseRoot) -> Result<(), SemanticError> {
        let capture = self
            .reader
            .capture_reference(&self.database.reference_key())?;
        let current = self.load_root(capture.root_id())?;
        self.check_epoch(&current)?;
        self.refresh_owned_lease()?;
        if !crate::storage::excision::same_source(&current, source) {
            return Err(conflict(
                "excision/source-changed",
                "Canonical source changed during excision admission",
            ));
        }
        Ok(())
    }

    pub(crate) fn adopt_excision(
        &mut self,
        mut prepared: crate::storage::excision::PreparedExcision,
    ) -> Result<IndexAdoption, SemanticError> {
        let capture = self
            .reader
            .capture_reference(&self.database.reference_key())?;
        let result = (|| {
            let current = self.load_root(capture.root_id())?;
            self.check_epoch(&current)?;
            self.refresh_owned_lease()?;
            if !crate::storage::excision::same_source(&current, &prepared.source)
                || prepared.candidate.basis != current.basis
            {
                return Err(conflict(
                    "excision/source-changed",
                    "Canonical source changed before excision activation",
                ));
            }
            let old_metadata = SnapshotMetadata::decode(
                &current.metadata.unwrap(),
                &required(&mut self.store, current.metadata.unwrap())?,
            )?;
            let metadata_id = prepared
                .candidate
                .metadata
                .ok_or_else(|| fault("excision/metadata", "Candidate metadata is missing"))?;
            let metadata =
                SnapshotMetadata::decode(&metadata_id, &required(&mut self.store, metadata_id)?)?;
            if metadata.generation
                != old_metadata
                    .generation
                    .checked_add(1)
                    .ok_or_else(|| fault("excision/generation", "Generation overflows"))?
                || metadata.excision.is_none()
                || metadata.identity != current.identity
                || metadata.eidx_frontier != old_metadata.eidx_frontier
                || metadata.reserved_frontier != old_metadata.reserved_frontier
                || metadata.last_tx_instant != old_metadata.last_tx_instant
            {
                return Err(fault(
                    "excision/candidate",
                    "Candidate generation or allocation coordinate differs",
                ));
            }
            let lease_condition = self.lease_condition();
            // Restore checkpoints and exact activation receipts own the
            // pre-excision graph. Revoke them with the generation transition:
            // a stale restore must neither retain nor return erased history.
            let restore_keys = [
                format!("restores/{}", identity_string(self.database.route)),
                format!(
                    "restores/completed/{}",
                    identity_string(self.database.route)
                ),
            ];
            let mut conditions = vec![
                capture.source_condition(),
                lease_condition,
                prepared.work_condition.clone(),
            ];
            for key in &restore_keys {
                conditions.push(RefCondition {
                    key: key.clone(),
                    expected: self
                        .store
                        .read_ref(key)?
                        .map(|reference| reference.revision),
                });
            }
            let protected = protection(&mut self.store, &conditions)?;
            self.store.set_write_protection(Some(protected.clone()))?;
            prepared.candidate.writer_epoch = self.lease.epoch;
            let id = self.store.put(&prepared.candidate.encode()?)?;
            let snapshot = self.reader.capture_immutable(id, &protected.conditions)?;
            // Keep the final original receipts for the collection grace period
            // that have not crossed this generation yet. The handoff is an
            // independent reclaimable owner, never a prior-publication link
            // reachable from ordinary database values.
            let (handoff_guards, handoff_changes) =
                crate::storage::report_handoff::prepare_handoff(
                    &mut self.store,
                    self.database.route,
                    current.identity,
                    old_metadata.generation,
                    current.basis,
                    capture.root_id(),
                )?;
            let mut publication_guards = protected.conditions.clone();
            publication_guards.extend(handoff_guards);
            let mut changes = vec![
                RefChange {
                    key: self.database.reference_key(),
                    value: Some(id.to_vec()),
                },
                RefChange {
                    key: prepared.work_condition.key.clone(),
                    value: None,
                },
                RefChange {
                    key: restore_keys[0].clone(),
                    value: None,
                },
                RefChange {
                    key: restore_keys[1].clone(),
                    value: None,
                },
            ];
            changes.extend(handoff_changes);
            let rows = match crate::storage::ownership::publish_refs(
                &mut self.store,
                &publication_guards,
                &changes,
            )? {
                BatchOutcome::Applied(rows) => rows,
                BatchOutcome::Conflict(_) => {
                    return Err(conflict(
                        "excision/activation-conflict",
                        "Excision activation lost its source or authority guard",
                    ));
                }
            };
            self.publication_revision = rows
                .iter()
                .find(|(key, _)| *key == self.database.reference_key())
                .unwrap()
                .1
                .revision;
            self.current = Some(snapshot);
            Ok(IndexAdoption {
                indexed_basis: current.basis,
                publication_revision: self.publication_revision,
                pending_avet: self.pending_avet_projections(),
            })
        })();
        let _ = self.store.set_write_protection(None);
        result
    }
    fn load_root(&mut self, id: ObjectId) -> Result<DatabaseRoot, SemanticError> {
        DatabaseRoot::decode_for_identity(
            &id,
            &required(&mut self.store, id)?,
            &self.database.identity,
        )
    }
    fn check_epoch(&self, root: &DatabaseRoot) -> Result<(), SemanticError> {
        if root.writer_epoch != self.lease.epoch {
            Err(conflict(
                "storage/writer-fenced",
                "A replacement writer has claimed this database",
            ))
        } else {
            Ok(())
        }
    }
    fn refresh_owned_lease(&mut self) -> Result<(), SemanticError> {
        let reference = self.store.read_ref(&self.database.lease_key())?;
        let Some(reference) = reference.filter(|r| r.value.is_some()) else {
            return Err(conflict(
                "storage/writer-fenced",
                "Writer lease has been released",
            ));
        };
        let lease = Lease::decode(reference.value.as_deref().unwrap())?;
        if lease.epoch != self.lease.epoch || lease.token != self.lease.token {
            return Err(conflict(
                "storage/writer-fenced",
                "A replacement writer owns the lease",
            ));
        }
        self.lease_revision = reference.revision;
        self.lease = lease;
        Ok(())
    }
    fn lease_condition(&self) -> RefCondition {
        RefCondition {
            key: self.database.lease_key(),
            expected: Some(self.lease_revision),
        }
    }
    pub fn release(mut self) -> Result<(), SemanticError> {
        self.refresh_owned_lease()?;
        match self.store.compare_exchange(
            &self.database.lease_key(),
            Some(self.lease_revision),
            None,
        )? {
            crate::storage::CasOutcome::Applied(_) => Ok(()),
            crate::storage::CasOutcome::Conflict(_) => Err(conflict(
                "storage/writer-fenced",
                "A replacement writer owns the lease",
            )),
        }
    }
}

fn condition(key: &str, reference: Option<&Reference>) -> RefCondition {
    RefCondition {
        key: key.to_owned(),
        expected: reference.map(|r| r.revision),
    }
}

/// Engine-owned authority for a remote route. The provider only sees revisions;
/// endpoint publication must compare these guards in its own indivisible ref update.
pub(crate) fn writer_endpoint_guards(
    store: &mut PgBlockStore,
    identity: &crate::DatabaseIdentity,
    epoch: u64,
) -> Result<Vec<RefCondition>, SemanticError> {
    let unavailable = || {
        SemanticError::new(
            ErrorCategory::Unavailable,
            "remote/no-writer",
            "No active writer for this database identity",
        )
    };
    if epoch == 0 {
        return Err(unavailable());
    }
    let root_key = format!("databases/{}", identity.database_id());
    let root_ref = store.read_ref(&root_key)?.ok_or_else(unavailable)?;
    let root_id = object_id(root_ref.value.as_deref().ok_or_else(unavailable)?)?;
    let root = DatabaseRoot::decode(&root_id, &required(store, root_id)?)?;
    if identity_string(root.identity) != identity.lineage_id() || root.writer_epoch != epoch {
        return Err(unavailable());
    }
    let lease_key = format!("writers/{}", identity.database_id());
    let lease_ref = store.read_ref(&lease_key)?.ok_or_else(unavailable)?;
    let lease = Lease::decode(lease_ref.value.as_deref().ok_or_else(unavailable)?)?;
    if lease.epoch != epoch || lease.expires_ms <= now_ms()? {
        return Err(unavailable());
    }
    Ok(vec![
        condition(&root_key, Some(&root_ref)),
        condition(&lease_key, Some(&lease_ref)),
    ])
}

/// Restoration fences the exact observed lease revision even when expired.
/// It never imports the source backup's writer authority into this route.
pub(crate) fn restore_lease_guard(
    store: &mut PgBlockStore,
    database: &BlockDatabase,
) -> Result<RefCondition, SemanticError> {
    let key = database.lease_key();
    let previous = store.read_ref(&key)?;
    if let Some(bytes) = previous.as_ref().and_then(|r| r.value.as_deref())
        && Lease::decode(bytes)?.expires_ms > now_ms()?
    {
        return Err(SemanticError::new(
            ErrorCategory::Busy,
            "backup/restore-writer-active",
            "Restore target currently has an active writer",
        ));
    }
    Ok(condition(&key, previous.as_ref()))
}

fn required(store: &mut PgBlockStore, id: ObjectId) -> Result<Vec<u8>, SemanticError> {
    store.get(id)?.ok_or_else(|| {
        fault(
            "storage/missing-object",
            "Referenced immutable object is absent",
        )
    })
}
fn object_id(bytes: &[u8]) -> Result<ObjectId, SemanticError> {
    bytes.try_into().map_err(|_| {
        fault(
            "storage/root-reference",
            "Reference must contain one object identity",
        )
    })
}
fn now_ms() -> Result<i64, SemanticError> {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| fault("storage/clock", "Clock is before Unix epoch"))?
            .as_millis(),
    )
    .map_err(|_| fault("storage/clock", "Clock exceeds supported instant range"))
}
fn lease_millis(duration: Duration) -> Result<i64, SemanticError> {
    i64::try_from(duration.as_millis())
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| {
            SemanticError::incorrect(
                "storage/lease-duration",
                "Writer lease must be a positive representable millisecond duration",
            )
        })
}
fn fault(code: &'static str, message: &str) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, code, message)
}
fn conflict(code: &'static str, message: &str) -> SemanticError {
    SemanticError::conflict(code, message)
}

fn publication_error(error: SemanticError) -> SemanticError {
    if error.category == ErrorCategory::UnknownOutcome {
        error.detail("ambiguity_kind", "publication")
    } else {
        error
    }
}
