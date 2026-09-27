//! Bounded, at-least-once application consumption of the authenticated log.
//! Checkpoints neither reserve history nor acknowledge external side effects.
use crate::observation::notices::NoticeListener;
use crate::{
    Digest, ErrorCategory, LogTransaction, ObservationConfig, PostgresConnectionConfig,
    SemanticError,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChangeConsumerConfig {
    pub observation: ObservationConfig,
    /// Admission bound for one encoded payload and its decoded datoms.
    /// Oversize errors leave the checkpoint untouched; reopen with a larger
    /// bound to process that transaction. This is not a transaction-size rule.
    pub max_event_bytes: usize,
}
impl Default for ChangeConsumerConfig {
    fn default() -> Self {
        Self {
            observation: ObservationConfig::default(),
            max_event_bytes: 128 * 1024 * 1024,
        }
    }
}

/// An immutable progress token; only a token delivered by this consumer's
/// current next() call can be acknowledged. Opaque references are reauthenticated on
/// reopen, never treated as an alternate source of transaction authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChangeCheckpoint {
    lineage_id: String,
    generation: u64,
    last_t: u64,
    commit_hash: Digest,
    prefix_hash: Digest,
    revision: u64,
}
impl ChangeCheckpoint {
    pub fn lineage_id(&self) -> &str {
        &self.lineage_id
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn last_t(&self) -> u64 {
        self.last_t
    }
    /// Authenticated transaction object identity; zero only at the t=0 origin.
    pub fn commit_hash(&self) -> Digest {
        self.commit_hash
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChangeEvent {
    pub transaction: LogTransaction,
    pub checkpoint: ChangeCheckpoint,
    pub accounted_bytes: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ChangeConsumerStats {
    pub delivered: u64,
    pub acknowledged: u64,
    pub notices: u64,
    pub reconnects: u64,
    pub gap_checks: u64,
    pub payload_bytes: u64,
    /// Only a progress token is retained internally after delivery. The
    /// returned event is caller-owned; no database snapshots are held.
    pub unacknowledged: bool,
}

pub struct ChangeConsumer {
    connection: PostgresConnectionConfig,
    database: crate::storage::BlockDatabase,
    principal: String,
    checkpoint_key: String,
    config: ChangeConsumerConfig,
    store: crate::storage::PgBlockStore,
    reader: crate::storage::BlockReader,
    listener: Option<NoticeListener>,
    checkpoint: ChangeCheckpoint,
    pending: Option<ChangeCheckpoint>,
    stats: ChangeConsumerStats,
}

const CHECKPOINT_MAGIC: &[u8; 8] = b"EDBC\0\0\0\x01";
const CHECKPOINT_BYTES: usize = 8 + 16 + 8 + 8 + 32 + 32;
const MAX_PUBLICATION_RETRIES: usize = 8;

struct Head {
    identity: [u8; 16],
    generation: u64,
    log: crate::storage::log::LogRoot,
    publication: crate::storage::RefCondition,
}

fn fault(code: &'static str, message: &str) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, code, message)
}
fn unavailable(code: &'static str, message: &str) -> SemanticError {
    SemanticError::new(ErrorCategory::Unavailable, code, message)
}
fn history_error(error: SemanticError) -> SemanticError {
    if error.code == "storage/missing-object" {
        unavailable(
            "consumer/history-unavailable",
            "Required authenticated log history is not retained",
        )
    } else if error.code == "storage/log-read-limit" {
        oversized()
    } else {
        error
    }
}
fn oversized() -> SemanticError {
    SemanticError::new(
        ErrorCategory::Busy,
        "consumer/event-too-large",
        "Transaction exceeds the configured encoded/decoded event byte limit",
    )
}
fn transport(error: &SemanticError) -> bool {
    error.category == ErrorCategory::Unavailable
        && (error.code == "storage/postgres"
            || error
                .details
                .get("postgres_transport")
                .is_some_and(|v| v == "true"))
}
fn checkpoint_key(identity: [u8; 16], principal: &str, name: &str) -> String {
    let mut scope = b"edb/change-consumer/v1".to_vec();
    for value in [principal, name] {
        scope.extend_from_slice(&(value.len() as u64).to_be_bytes());
        scope.extend_from_slice(value.as_bytes());
    }
    let digest = crate::sha256(&scope)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    format!(
        "consumers/{}/{digest}",
        crate::storage::catalog::identity_string(identity)
    )
}
fn encode_checkpoint(checkpoint: &ChangeCheckpoint, identity: [u8; 16]) -> Vec<u8> {
    let mut bytes = CHECKPOINT_MAGIC.to_vec();
    bytes.extend_from_slice(&identity);
    bytes.extend_from_slice(&checkpoint.generation.to_be_bytes());
    bytes.extend_from_slice(&checkpoint.last_t.to_be_bytes());
    bytes.extend_from_slice(&checkpoint.commit_hash);
    bytes.extend_from_slice(&checkpoint.prefix_hash);
    bytes
}
fn decode_checkpoint(
    reference: &crate::storage::Reference,
) -> Result<ChangeCheckpoint, SemanticError> {
    let bytes = reference.value.as_deref().ok_or_else(|| {
        unavailable(
            "consumer/checkpoint-retired",
            "Checkpoint is retired; choose a new consumer name",
        )
    })?;
    if bytes.len() != CHECKPOINT_BYTES || &bytes[..8] != CHECKPOINT_MAGIC {
        return Err(fault(
            "consumer/checkpoint-format",
            "Checkpoint has an invalid current encoding",
        ));
    }
    let identity: [u8; 16] = bytes[8..24].try_into().unwrap();
    let last_t = u64::from_be_bytes(bytes[32..40].try_into().unwrap());
    let commit_hash: Digest = bytes[40..72].try_into().unwrap();
    let prefix_hash: Digest = bytes[72..104].try_into().unwrap();
    if identity == [0; 16]
        || crate::t_to_tx(last_t).is_err()
        || (last_t == 0) != (commit_hash == [0; 32])
        || (last_t == 0) != (prefix_hash == [0; 32])
    {
        return Err(fault(
            "consumer/checkpoint-format",
            "Checkpoint identity or transaction coordinate is invalid",
        ));
    }
    Ok(ChangeCheckpoint {
        lineage_id: crate::storage::catalog::identity_string(identity),
        generation: u64::from_be_bytes(bytes[24..32].try_into().unwrap()),
        last_t,
        commit_hash,
        prefix_hash,
        // The public token starts at revision zero; the generic reference
        // starts at one. The provider revision is the only CAS authority.
        revision: reference.revision.checked_sub(1).ok_or_else(|| {
            fault(
                "consumer/invalid-coordinate",
                "Reference revision must be positive",
            )
        })?,
    })
}
fn verify_head(head: &Head, checkpoint: &ChangeCheckpoint) -> Result<(), SemanticError> {
    if crate::storage::catalog::identity_string(head.identity) != checkpoint.lineage_id {
        return Err(unavailable(
            "consumer/lineage-changed",
            "Database publication belongs to another lineage",
        ));
    }
    if head.generation != checkpoint.generation {
        return Err(unavailable(
            "consumer/generation-changed",
            "Checkpoint predates excision; choose a new consumer in the current generation",
        ));
    }
    if checkpoint.last_t > head.log.basis_t() {
        return Err(fault(
            "consumer/checkpoint-future",
            "Checkpoint is beyond the authenticated log head",
        ));
    }
    Ok(())
}
fn required(
    store: &mut crate::storage::PgBlockStore,
    id: Digest,
) -> Result<Vec<u8>, SemanticError> {
    store.get(id)?.ok_or_else(|| {
        unavailable(
            "consumer/history-unavailable",
            "Publication object is not retained",
        )
    })
}

/// Read one immutable publication. No snapshot or log page is retained by the
/// consumer between calls; storage retention governs its availability.
fn with_head<T>(
    store: &mut crate::storage::PgBlockStore,
    reader: &crate::storage::BlockReader,
    database: &crate::storage::BlockDatabase,
    f: impl FnOnce(&mut crate::storage::PgBlockStore, &Head) -> Result<T, SemanticError>,
) -> Result<T, SemanticError> {
    let capture = reader
        .capture_reference(&database.reference_key())
        .map_err(|error| {
            if matches!(
                error.code,
                "storage/reference-not-found"
                    | "storage/reference-retired"
                    | "catalog/database-retired"
            ) {
                unavailable(
                    "consumer/database-unavailable",
                    "Consumer database identity is no longer active",
                )
            } else {
                error
            }
        })?;
    let id = capture.root_id();
    let root = crate::storage::root::DatabaseRoot::decode(&id, &required(store, id)?)?;
    if root.identity != database.identity {
        return Err(unavailable(
            "consumer/lineage-changed",
            "Database publication belongs to another lineage",
        ));
    }
    let metadata = root
        .metadata
        .ok_or_else(|| fault("consumer/head-format", "Publication lacks metadata"))?;
    let metadata =
        crate::storage::SnapshotMetadata::decode(&metadata, &required(store, metadata)?)?;
    let log = match root.log {
        Some(id) => crate::storage::log::LogRoot::open(store, id).map_err(history_error)?,
        None => crate::storage::log::LogRoot::empty(),
    };
    if metadata.identity != root.identity
        || metadata.basis != root.basis
        || log.basis_t() != root.basis
        || (root.basis != 0
            && (log.eidx_frontier() != metadata.eidx_frontier
                || log.reserved_frontier() != metadata.reserved_frontier))
    {
        return Err(fault(
            "consumer/head-format",
            "Publication log and metadata coordinates disagree",
        ));
    }
    f(
        store,
        &Head {
            identity: root.identity,
            generation: metadata.generation,
            log,
            publication: capture.source_condition(),
        },
    )
}
fn authenticate_checkpoint(
    store: &mut crate::storage::PgBlockStore,
    head: &Head,
    checkpoint: &ChangeCheckpoint,
    max_bytes: usize,
) -> Result<(), SemanticError> {
    verify_head(head, checkpoint)?;
    if checkpoint.last_t == 0 {
        return Ok(());
    }
    let record = head
        .log
        .read_record_bounded(store, checkpoint.last_t, max_bytes)
        .map_err(history_error)?
        .ok_or_else(|| {
            unavailable(
                "consumer/history-unavailable",
                "Checkpoint transaction is not retained",
            )
        })?;
    if record.id != checkpoint.commit_hash {
        return Err(fault(
            "consumer/checkpoint-hash",
            "Checkpoint differs from the authenticated transaction",
        ));
    }
    verify_prefix(store, head, checkpoint)?;
    admit_decoded(&record, max_bytes)?;
    Ok(())
}

fn verify_prefix(
    store: &mut crate::storage::PgBlockStore,
    head: &Head,
    checkpoint: &ChangeCheckpoint,
) -> Result<(), SemanticError> {
    if head
        .log
        .prefix_hash(store, checkpoint.last_t)
        .map_err(history_error)?
        .unwrap_or([0; 32])
        != checkpoint.prefix_hash
    {
        return Err(fault(
            "consumer/checkpoint-prefix",
            "Previously consumed log prefix changed without a generation transition",
        ));
    }
    Ok(())
}
fn admit_decoded(
    record: &crate::storage::log::LogRecord,
    limit: usize,
) -> Result<usize, SemanticError> {
    let bytes = record
        .entry
        .tx_data
        .iter()
        .fold(record.encoded_bytes as usize, |n, d| {
            n.saturating_add(d.retained_bytes() as usize)
        });
    if bytes > limit {
        return Err(oversized());
    }
    Ok(bytes)
}
fn busy_head() -> SemanticError {
    SemanticError::new(
        ErrorCategory::Busy,
        "consumer/publication-busy",
        "Publication changed repeatedly; retry without advancing this checkpoint",
    )
}

impl ChangeConsumer {
    pub fn connect(
        connection: &str,
        database: impl Into<String>,
        name: impl Into<String>,
        config: ChangeConsumerConfig,
    ) -> Result<Self, SemanticError> {
        Self::connect_configured(
            PostgresConnectionConfig::parse(connection)?,
            database,
            name,
            config,
        )
    }
    pub fn connect_configured(
        connection: PostgresConnectionConfig,
        database: impl Into<String>,
        name: impl Into<String>,
        config: ChangeConsumerConfig,
    ) -> Result<Self, SemanticError> {
        use crate::storage::{
            BatchOutcome, BlockDatabase, BlockReader, PgBlockStore, RefChange, RefCondition,
        };
        config.observation.validate()?;
        let name = name.into();
        if name.is_empty() || name.len() > 512 || name.contains('\0') || config.max_event_bytes == 0
        {
            return Err(SemanticError::incorrect(
                "consumer/invalid-config",
                "Consumer name must contain 1..512 non-NUL bytes and event byte limit must be positive",
            ));
        }
        let mut store = PgBlockStore::connect(&connection)?;
        let database =
            BlockDatabase::resolve_in(&mut store, &database.into())?.ok_or_else(|| {
                unavailable(
                    "consumer/database-unavailable",
                    "Consumer database name is not active",
                )
            })?;
        let principal = store.principal()?;
        let key = checkpoint_key(database.route, &principal, &name);
        let reader = BlockReader::connect(&connection, Default::default())?;
        // Listen before reading a durable endpoint; hints never supply data.
        let listener = Some(NoticeListener::connect(
            &connection,
            &crate::storage::catalog::identity_string(database.route),
        )?);
        for _ in 0..MAX_PUBLICATION_RETRIES {
            let checkpoint = with_head(&mut store, &reader, &database, |store, head| {
                if let Some(reference) = store.read_ref(&key)? {
                    let checkpoint = decode_checkpoint(&reference)?;
                    authenticate_checkpoint(store, head, &checkpoint, config.max_event_bytes)?;
                    return Ok(Some(checkpoint));
                }
                let checkpoint = ChangeCheckpoint {
                    lineage_id: crate::storage::catalog::identity_string(database.identity),
                    generation: head.generation,
                    last_t: 0,
                    commit_hash: [0; 32],
                    prefix_hash: [0; 32],
                    revision: 0,
                };
                match store.compare_exchange_many(
                    &[
                        RefCondition {
                            key: key.clone(),
                            expected: None,
                        },
                        head.publication.clone(),
                    ],
                    &[RefChange {
                        key: key.clone(),
                        value: Some(encode_checkpoint(&checkpoint, database.identity)),
                    }],
                )? {
                    BatchOutcome::Applied(_) => Ok(Some(checkpoint)),
                    BatchOutcome::Conflict(_) => Ok(None),
                }
            })?;
            if let Some(checkpoint) = checkpoint {
                return Ok(Self {
                    connection,
                    database,
                    principal,
                    checkpoint_key: key,
                    config,
                    store,
                    reader,
                    listener,
                    checkpoint,
                    pending: None,
                    stats: Default::default(),
                });
            }
        }
        Err(busy_head())
    }
    pub fn checkpoint(&self) -> &ChangeCheckpoint {
        &self.checkpoint
    }
    pub fn stats(&self) -> ChangeConsumerStats {
        ChangeConsumerStats {
            unacknowledged: self.pending.is_some(),
            ..self.stats
        }
    }
    /// Reconnect transport, never silently change identity or durable progress.
    pub fn reconnect(&mut self) -> Result<(), SemanticError> {
        self.store = crate::storage::PgBlockStore::connect(&self.connection)?;
        if self.store.principal()? != self.principal {
            return Err(unavailable(
                "consumer/principal-changed",
                "Reconnected PostgreSQL principal differs",
            ));
        }
        self.reader.reconnect()?;
        self.listener = Some(NoticeListener::connect(
            &self.connection,
            &crate::storage::catalog::identity_string(self.database.route),
        )?);
        with_head(
            &mut self.store,
            &self.reader,
            &self.database,
            |store, head| {
                authenticate_checkpoint(store, head, &self.checkpoint, self.config.max_event_bytes)
            },
        )?;
        self.stats.reconnects = self.stats.reconnects.saturating_add(1);
        Ok(())
    }
    pub fn next(&mut self, timeout: Duration) -> Result<Option<ChangeEvent>, SemanticError> {
        static NEVER: AtomicBool = AtomicBool::new(false);
        self.next_with_cancel(timeout, &NEVER)
    }
    /// Timeout/cancellation bound idle waiting, not already-issued provider I/O.
    pub fn next_with_cancel(
        &mut self,
        timeout: Duration,
        cancelled: &AtomicBool,
    ) -> Result<Option<ChangeEvent>, SemanticError> {
        if self.pending.is_some() {
            return Err(SemanticError::new(
                ErrorCategory::Busy,
                "consumer/unacknowledged",
                "Acknowledge the delivered event before requesting another",
            ));
        }
        let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
            SemanticError::incorrect("consumer/timeout-overflow", "Consumer timeout is too large")
        })?;
        let mut check = true;
        let mut repair_at = Instant::now();
        loop {
            if cancelled.load(Ordering::Relaxed) {
                return Err(SemanticError::new(
                    ErrorCategory::Interrupted,
                    "consumer/cancelled",
                    "Consumer wait was cancelled",
                ));
            }
            if check || Instant::now() >= repair_at {
                match self.read_next() {
                    Ok(Some(event)) => return Ok(Some(event)),
                    Ok(None) => {}
                    Err(error) if transport(&error) => {
                        self.reconnect()?;
                        if let Some(event) = self.read_next()? {
                            return Ok(Some(event));
                        }
                    }
                    Err(error) => return Err(error),
                }
                check = false;
                repair_at = Instant::now() + self.config.observation.anti_entropy_interval;
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            let wait = deadline
                .saturating_duration_since(now)
                .min(repair_at.saturating_duration_since(now))
                .min(Duration::from_millis(25));
            match self
                .listener
                .as_mut()
                .expect("consumer listener exists")
                .wait(wait)
            {
                Ok(true) => {
                    self.stats.notices = self.stats.notices.saturating_add(1);
                    check = true;
                }
                Ok(false) => {}
                Err(_) => {
                    self.reconnect()?;
                    check = true;
                }
            }
        }
    }
    fn read_next(&mut self) -> Result<Option<ChangeEvent>, SemanticError> {
        self.stats.gap_checks = self.stats.gap_checks.saturating_add(1);
        let result = with_head(
            &mut self.store,
            &self.reader,
            &self.database,
            |store, head| {
                verify_head(head, &self.checkpoint)?;
                verify_prefix(store, head, &self.checkpoint)?;
                if self.checkpoint.last_t != 0
                    && head
                        .log
                        .record_id(store, self.checkpoint.last_t)
                        .map_err(history_error)?
                        != Some(self.checkpoint.commit_hash)
                {
                    return Err(fault(
                        "consumer/checkpoint-hash",
                        "Checkpoint is not in the captured authenticated log",
                    ));
                }
                if head.log.basis_t() == self.checkpoint.last_t {
                    return Ok(None);
                }
                let next = self.checkpoint.last_t + 1;
                let record = head
                    .log
                    .read_record_bounded(store, next, self.config.max_event_bytes)
                    .map_err(history_error)?
                    .ok_or_else(|| {
                        unavailable(
                            "consumer/history-unavailable",
                            "Next transaction is not retained",
                        )
                    })?;
                let accounted = admit_decoded(&record, self.config.max_event_bytes)?;
                let prefix = head
                    .log
                    .prefix_hash(store, next)
                    .map_err(history_error)?
                    .expect("positive retained transaction");
                Ok(Some((record, accounted, prefix)))
            },
        )?;
        let Some((record, accounted_bytes, prefix_hash)) = result else {
            return Ok(None);
        };
        let checkpoint = ChangeCheckpoint {
            lineage_id: self.checkpoint.lineage_id.clone(),
            generation: self.checkpoint.generation,
            last_t: record.entry.basis_t,
            commit_hash: record.id,
            prefix_hash,
            revision: self.checkpoint.revision.checked_add(1).ok_or_else(|| {
                fault(
                    "consumer/revision-overflow",
                    "Checkpoint revision exhausted",
                )
            })?,
        };
        self.pending = Some(checkpoint.clone());
        self.stats.delivered = self.stats.delivered.saturating_add(1);
        self.stats.payload_bytes = self
            .stats
            .payload_bytes
            .saturating_add(record.encoded_bytes);
        Ok(Some(ChangeEvent {
            transaction: LogTransaction {
                t: record.entry.basis_t,
                data: record.entry.tx_data,
            },
            checkpoint,
            accounted_bytes,
        }))
    }
    /// Acknowledgement and external effects are not edb. On ambiguous
    /// transport failure reopen to discover the persisted checkpoint.
    pub fn acknowledge(&mut self, checkpoint: &ChangeCheckpoint) -> Result<(), SemanticError> {
        use crate::storage::{BatchOutcome, RefChange, RefCondition};
        if self.pending.as_ref() != Some(checkpoint) {
            return Err(SemanticError::incorrect(
                "consumer/not-delivered",
                "Checkpoint is not this consumer's pending contiguous event",
            ));
        }
        for _ in 0..MAX_PUBLICATION_RETRIES {
            let applied = with_head(
                &mut self.store,
                &self.reader,
                &self.database,
                |store, head| {
                    verify_head(head, &self.checkpoint)?;
                    verify_prefix(store, head, checkpoint)?;
                    if head
                        .log
                        .record_id(store, checkpoint.last_t)
                        .map_err(history_error)?
                        != Some(checkpoint.commit_hash)
                    {
                        return Err(fault(
                            "consumer/checkpoint-hash",
                            "Delivered transaction is not in the current authenticated log",
                        ));
                    }
                    let previous = store.read_ref(&self.checkpoint_key)?;
                    if previous
                        .as_ref()
                        .map(decode_checkpoint)
                        .transpose()?
                        .as_ref()
                        != Some(&self.checkpoint)
                    {
                        return Err(SemanticError::conflict(
                            "consumer/checkpoint-conflict",
                            "Another consumer advanced this checkpoint; reopen before continuing",
                        ));
                    }
                    let conditions = [
                        RefCondition {
                            key: self.checkpoint_key.clone(),
                            expected: Some(self.checkpoint.revision + 1),
                        },
                        head.publication.clone(),
                    ];
                    let changes = [RefChange {
                        key: self.checkpoint_key.clone(),
                        value: Some(encode_checkpoint(checkpoint, self.database.identity)),
                    }];
                    let outcome = store.compare_exchange_many(&conditions, &changes).map_err(|error| {
                        if transport(&error) || error.category == ErrorCategory::UnknownOutcome {
                            SemanticError::new(
                                ErrorCategory::UnknownOutcome,
                                "consumer/checkpoint-unknown-outcome",
                                "Acknowledgement may have committed; reopen to resolve durable progress",
                            )
                        } else { error }
                    })?;
                    Ok(matches!(outcome, BatchOutcome::Applied(_)))
                },
            )?;
            if applied {
                self.checkpoint = checkpoint.clone();
                self.pending = None;
                self.stats.acknowledged = self.stats.acknowledged.saturating_add(1);
                return Ok(());
            }
        }
        Err(busy_head())
    }
}
