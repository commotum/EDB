//! Live observation over the same immutable block snapshots used by writers.
//! The updater serializes observations, never queries or lazy snapshot reads.
use super::{PeerLoadStats, PeerSnapshot, lock, transaction_hash};
use crate::index::cursor::MergeSource;
use crate::index::recent::{RecentLimits, RecentStats};
use crate::storage::{BlockDatabase, BlockReadConfig, BlockReader, BlockSnapshot};
use crate::{
    CacheStats, DatabaseIdentity, DatabaseValue, Digest, Entity, EntityIdentifier, ErrorCategory,
    PostgresConnectionConfig, PullPattern, Query, QueryControl, QueryExtensions, QueryInput,
    QueryOutcome, QueryValue, SemanticError, ServiceTransactionReport,
};
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct Peer {
    core: Arc<Core>,
}

struct Core {
    reader: BlockReader,
    reference: String,
    route: [u8; 16],
    identity: DatabaseIdentity,
    current: RwLock<State>,
    observed_basis: Mutex<u64>,
    state_advanced: Condvar,
    update: Mutex<()>,
    reports: Mutex<Option<VecDeque<ServiceTransactionReport>>>,
    report_ready: Condvar,
}

#[derive(Clone)]
struct State {
    snapshot: BlockSnapshot,
    publication_revision: u64,
    observation_generation: u64,
}

impl Peer {
    pub fn connect(
        connection: &str,
        database_id: impl Into<String>,
        cache_capacity: usize,
    ) -> Result<Self, SemanticError> {
        Self::connect_configured(
            &PostgresConnectionConfig::parse(connection)?,
            database_id,
            cache_capacity,
        )
    }

    pub fn connect_configured(
        connection: &PostgresConnectionConfig,
        database_id: impl Into<String>,
        cache_capacity: usize,
    ) -> Result<Self, SemanticError> {
        Self::connect_configured_with_cache_limits(
            connection,
            database_id,
            cache_capacity,
            cache_capacity.saturating_mul(512 * 1024),
        )
    }

    pub fn connect_with_cache_limits(
        connection: &str,
        database_id: impl Into<String>,
        cache_entries: usize,
        cache_bytes: usize,
    ) -> Result<Self, SemanticError> {
        Self::connect_configured_with_cache_limits(
            &PostgresConnectionConfig::parse(connection)?,
            database_id,
            cache_entries,
            cache_bytes,
        )
    }

    pub fn connect_configured_with_cache_limits(
        connection: &PostgresConnectionConfig,
        database_id: impl Into<String>,
        cache_entries: usize,
        cache_bytes: usize,
    ) -> Result<Self, SemanticError> {
        let database = BlockDatabase::resolve(connection, &database_id.into())?;
        Self::open(connection, database, cache_entries, cache_bytes, None)
    }

    pub fn connect_with_limits(
        connection: &str,
        database_id: impl Into<String>,
        cache_entries: usize,
        cache_bytes: usize,
        recent_limits: RecentLimits,
    ) -> Result<Self, SemanticError> {
        Self::connect_configured_with_limits(
            &PostgresConnectionConfig::parse(connection)?,
            database_id,
            cache_entries,
            cache_bytes,
            recent_limits,
        )
    }

    pub fn connect_configured_with_limits(
        connection: &PostgresConnectionConfig,
        database_id: impl Into<String>,
        cache_entries: usize,
        cache_bytes: usize,
        recent_limits: RecentLimits,
    ) -> Result<Self, SemanticError> {
        let database = BlockDatabase::resolve(connection, &database_id.into())?;
        Self::open(
            connection,
            database,
            cache_entries,
            cache_bytes,
            Some(recent_limits),
        )
    }

    pub(crate) fn connect_identity_configured(
        connection: &PostgresConnectionConfig,
        identity: &DatabaseIdentity,
        cache_entries: usize,
    ) -> Result<Self, SemanticError> {
        let identity_bytes = parse_identity(identity.lineage_id())?;
        let peer = Self::open(
            connection,
            BlockDatabase {
                identity: identity_bytes,
                route: parse_identity(identity.database_id())?,
                name: String::new(),
            },
            cache_entries,
            cache_entries.saturating_mul(512 * 1024),
            None,
        )?;
        if peer.identity() != *identity {
            return Err(identity_error());
        }
        Ok(peer)
    }

    fn open(
        connection: &PostgresConnectionConfig,
        database: BlockDatabase,
        cache_entries: usize,
        cache_bytes: usize,
        recent_limits: Option<RecentLimits>,
    ) -> Result<Self, SemanticError> {
        let mut reads = BlockReadConfig {
            cache_entries,
            cache_bytes,
            ..BlockReadConfig::default()
        };
        if let Some(recent_limits) = recent_limits {
            if recent_limits.soft_bytes == 0
                || recent_limits.soft_datoms == 0
                || recent_limits.soft_bytes > recent_limits.hard_bytes
                || recent_limits.soft_datoms > recent_limits.hard_datoms
            {
                return Err(SemanticError::incorrect(
                    "recent/invalid-limits",
                    "Recent soft limits must be positive and not exceed hard limits",
                ));
            }
            reads.max_recent_bytes =
                usize::try_from(recent_limits.hard_bytes).unwrap_or(usize::MAX);
            reads.max_recent_datoms =
                usize::try_from(recent_limits.hard_datoms).unwrap_or(usize::MAX);
        }
        let reader = BlockReader::connect(connection, reads)?;
        let reference = database.reference_key();
        let snapshot = reader.capture(&reference)?;
        if snapshot.captured_root().identity != database.identity {
            return Err(identity_error());
        }
        let id = crate::storage::catalog::identity_string(database.identity);
        let revision = snapshot.publication_revision().ok_or_else(|| {
            fault(
                "peer/publication-coordinate",
                "Publication capture has no reference revision",
            )
        })?;
        let initial_basis = snapshot.basis_t();
        Ok(Self {
            core: Arc::new(Core {
                reader,
                reference,
                route: database.route,
                identity: DatabaseIdentity::new(
                    crate::storage::catalog::identity_string(database.route),
                    id,
                ),
                current: RwLock::new(State {
                    snapshot,
                    publication_revision: revision,
                    observation_generation: 0,
                }),
                observed_basis: Mutex::new(initial_basis),
                state_advanced: Condvar::new(),
                update: Mutex::new(()),
                reports: Mutex::new(None),
                report_ready: Condvar::new(),
            }),
        })
    }

    fn state(&self) -> State {
        self.core
            .current
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn publish(&self, state: State) {
        let mut observed = lock(&self.core.observed_basis);
        let basis = state.snapshot.basis_t();
        *self.core.current.write().unwrap_or_else(|e| e.into_inner()) = state;
        *observed = basis;
        self.core.state_advanced.notify_all();
    }

    /// Wait only on local adoption, independently of update/storage locks.
    pub(crate) fn wait_for_local_basis(
        &self,
        target: u64,
        timeout: Duration,
    ) -> Result<(), SemanticError> {
        let start = Instant::now();
        let mut observed = lock(&self.core.observed_basis);
        while *observed < target {
            let remaining = timeout.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return Err(SemanticError::new(
                    ErrorCategory::Unavailable,
                    "connection/observation-timeout",
                    "Transaction committed but local observation has not caught up",
                )
                .detail("basis_t", target.to_string()));
            }
            observed = self
                .core
                .state_advanced
                .wait_timeout(observed, remaining)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        Ok(())
    }

    pub fn db(&self) -> DatabaseValue {
        self.database_value()
    }
    pub fn database_value(&self) -> DatabaseValue {
        self.state().snapshot.database_value()
    }
    pub fn snapshot(&self) -> PeerSnapshot {
        let state = self.state();
        PeerSnapshot {
            native: state.snapshot,
            publication_revision: state.publication_revision,
        }
    }
    pub fn identity(&self) -> DatabaseIdentity {
        self.core.identity.clone()
    }
    pub fn basis_t(&self) -> u64 {
        self.state().snapshot.basis_t()
    }
    pub fn durable_base_t(&self) -> u64 {
        self.state().snapshot.indexed_basis_t()
    }
    pub fn durable_base_revision(&self) -> u64 {
        self.state().publication_revision
    }
    pub fn generation(&self) -> u64 {
        self.state().observation_generation
    }
    pub fn excision_generation(&self) -> u64 {
        self.state().snapshot.generation()
    }
    pub fn recent_stats(&self) -> RecentStats {
        self.state().snapshot.recent_tier().stats()
    }
    pub fn log(&self) -> crate::LogValue {
        self.state().snapshot.log()
    }
    pub fn cache_stats(&self) -> CacheStats {
        self.core.reader.cache_stats()
    }
    pub fn ssd_cache_stats(&self) -> crate::SsdCacheStats {
        self.core.reader.ssd_cache_stats()
    }
    pub fn node_block_read_stats(&self) -> crate::NodeBlockReadStats {
        self.core.reader.node_block_read_stats()
    }
    pub fn load_stats(&self) -> PeerLoadStats {
        self.core.reader.load_stats()
    }
    pub fn purge_ssd_generation(&self, generation: u64) -> bool {
        self.core
            .reader
            .purge_ssd_generation(self.state().snapshot.captured_root().identity, generation)
    }
    pub fn reopen_snapshot(
        &self,
        reference: &crate::SnapshotReference,
    ) -> Result<DatabaseValue, SemanticError> {
        if reference.database_id() != self.core.identity.database_id() {
            return Err(identity_error());
        }
        reference.open_with_reader(&self.core.reader)
    }
    pub fn entity(
        &self,
        identifier: impl Into<EntityIdentifier>,
    ) -> Result<Option<Entity>, SemanticError> {
        self.database_value().entity(identifier)
    }
    pub fn query(
        &self,
        query: &Query,
        inputs: &[QueryInput],
        control: &QueryControl,
    ) -> Result<QueryOutcome, SemanticError> {
        self.database_value().query(query, inputs, control)
    }
    pub fn query_with_extensions(
        &self,
        query: &Query,
        inputs: &[QueryInput],
        control: &QueryControl,
        extensions: &QueryExtensions,
    ) -> Result<QueryOutcome, SemanticError> {
        self.database_value()
            .query_with_extensions(query, inputs, control, extensions)
    }
    pub fn pull(
        &self,
        pattern: &PullPattern,
        entity: impl Into<EntityIdentifier>,
    ) -> Result<QueryValue, SemanticError> {
        self.database_value().pull(pattern, entity)
    }

    fn sync_once(&self) -> Result<PeerSnapshot, SemanticError> {
        let _update = lock(&self.core.update);
        let previous = self.state();
        let Some(next) = self
            .core
            .reader
            .capture_changed(&self.core.reference, &previous.snapshot)?
        else {
            return Ok(PeerSnapshot {
                native: previous.snapshot,
                publication_revision: previous.publication_revision,
            });
        };
        if next.captured_root().identity != previous.snapshot.captured_root().identity {
            return Err(identity_error());
        }
        if next.basis_t() < previous.snapshot.basis_t() {
            return Err(fault(
                "peer/basis-regressed",
                "Publication regressed the observed basis",
            ));
        }
        if next.generation() < previous.snapshot.generation() {
            return Err(fault(
                "peer/generation-regressed",
                "Publication regressed the observed generation",
            ));
        }
        let revision = next.publication_revision().ok_or_else(|| {
            fault(
                "peer/publication-coordinate",
                "Publication capture has no reference revision",
            )
        })?;
        let mut reports = Vec::new();
        if self.tx_reports_enabled() {
            let mut through = previous.snapshot.basis_t();
            let source = self
                .core
                .reader
                .report_publication(&self.core.reference, &previous.snapshot)?;
            for generation in previous.snapshot.generation()..next.generation() {
                let (source, end) = self.core.reader.report_handoff(
                    &source,
                    self.core.route,
                    next.captured_root().identity,
                    previous.snapshot.generation(),
                    generation,
                )?;
                if end < through || end > next.basis_t() {
                    return Err(fault(
                        "peer/report-handoff-order",
                        "Excision handoff does not extend the observed transaction prefix",
                    ));
                }
                self.collect_reports(&source, generation, through, end, &mut reports)?;
                through = end;
            }
            let source = self
                .core
                .reader
                .report_publication(&self.core.reference, &next)?;
            self.collect_reports(
                &source,
                next.generation(),
                through,
                next.basis_t(),
                &mut reports,
            )?;
        }
        self.publish(State {
            snapshot: next.clone(),
            publication_revision: revision,
            observation_generation: previous.observation_generation.saturating_add(1),
        });
        if !reports.is_empty()
            && let Some(queue) = lock(&self.core.reports).as_mut()
        {
            for mut report in reports {
                report.replayed = false;
                queue.push_back(report);
            }
            self.core.report_ready.notify_all();
        }
        Ok(PeerSnapshot {
            native: next,
            publication_revision: revision,
        })
    }

    fn collect_reports(
        &self,
        source: &crate::storage::snapshot::RootCapture,
        generation: u64,
        after_basis: u64,
        through_basis: u64,
        reports: &mut Vec<ServiceTransactionReport>,
    ) -> Result<(), SemanticError> {
        // sync_once holds the update lock until the entire observation is
        // validated. Carry the exact previous endpoint across batches, including
        // generation handoffs; the reader authenticates any changed root before
        // reusing its recent prefix or falling back to a current-format open.
        let previous = match reports.last() {
            Some(report) => direct_report_value(&report.db_after)?.clone(),
            None => self.state().snapshot,
        };
        let batch = self.core.reader.exact_reports_from_capture(
            source,
            after_basis,
            through_basis,
            Some(&previous),
        )?;
        for report in batch {
            let before = direct_report_value(&report.db_before)?;
            let after = direct_report_value(&report.db_after)?;
            if before.generation() != generation
                || after.generation() != generation
                || after.lineage_id() != self.core.identity.lineage_id()
            {
                return Err(fault(
                    "peer/report-generation-changed",
                    "Exact report differs from its retained publication generation",
                ));
            }
            reports.push(report);
        }
        Ok(())
    }

    pub fn sync_snapshot(&self) -> Result<PeerSnapshot, SemanticError> {
        match self.sync_once() {
            Err(error) if error.category == ErrorCategory::Unavailable => {
                self.reconnect()?;
                self.sync_once()
            }
            result => result,
        }
    }
    pub fn sync(&self) -> Result<DatabaseValue, SemanticError> {
        self.sync_database_value()
    }
    pub fn sync_database_value(&self) -> Result<DatabaseValue, SemanticError> {
        Ok(self.sync_snapshot()?.database_value())
    }
    pub fn reconnect(&self) -> Result<(), SemanticError> {
        self.core.reader.reconnect()
    }
    pub fn refresh_index(&self) -> Result<bool, SemanticError> {
        let _update = lock(&self.core.update);
        let previous = self.state();
        let Some(next) = self
            .core
            .reader
            .capture_changed(&self.core.reference, &previous.snapshot)?
        else {
            return Ok(false);
        };
        // Physical refresh never advances logical time. A caller that wants
        // newer transactions uses sync; a newer-than-value base is not usable.
        if next.basis_t() != previous.snapshot.basis_t()
            || (next.captured_root().indexes == previous.snapshot.captured_root().indexes
                && next.generation() == previous.snapshot.generation())
        {
            return Ok(false);
        }
        let revision = next.publication_revision().ok_or_else(|| {
            fault(
                "peer/publication-coordinate",
                "Publication capture has no reference revision",
            )
        })?;
        self.publish(State {
            snapshot: next,
            publication_revision: revision,
            observation_generation: previous.observation_generation.saturating_add(1),
        });
        Ok(true)
    }
    pub fn sync_to(&self, target: u64, timeout: Duration) -> Result<DatabaseValue, SemanticError> {
        self.sync_to_database_value(target, timeout)
    }
    pub fn sync_to_snapshot(
        &self,
        target: u64,
        timeout: Duration,
    ) -> Result<PeerSnapshot, SemanticError> {
        self.wait_for(target, timeout, "peer/sync-timeout", |_| Ok(true))
    }
    pub fn sync_to_database_value(
        &self,
        target: u64,
        timeout: Duration,
    ) -> Result<DatabaseValue, SemanticError> {
        Ok(self.sync_to_snapshot(target, timeout)?.database_value())
    }
    pub fn sync_index(
        &self,
        target: u64,
        timeout: Duration,
    ) -> Result<DatabaseValue, SemanticError> {
        Ok(self
            .wait_for(target, timeout, "peer/sync-index-timeout", |snapshot| {
                if snapshot.indexed_basis_t() < target {
                    return Ok(false);
                }
                avet_ready_through(snapshot, target, true)
            })?
            .database_value())
    }
    pub fn sync_schema(
        &self,
        target: u64,
        timeout: Duration,
    ) -> Result<DatabaseValue, SemanticError> {
        Ok(self
            .wait_for(target, timeout, "peer/sync-schema-timeout", |snapshot| {
                avet_ready_through(snapshot, target, false)
            })?
            .database_value())
    }
    pub fn sync_excise(
        &self,
        target: u64,
        timeout: Duration,
    ) -> Result<DatabaseValue, SemanticError> {
        let started = Instant::now();
        loop {
            let snapshot = self.sync_snapshot()?;
            if snapshot.basis_t() >= target
                && crate::storage::excision::sync_complete(&snapshot.native, target)?
            {
                return Ok(snapshot.database_value());
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(SemanticError::new(
                    ErrorCategory::Unavailable,
                    "peer/excision-not-ready",
                    "Timed out waiting for committed excision completion",
                ));
            }
            std::thread::sleep(remaining.min(Duration::from_millis(10)));
        }
    }
    fn wait_for(
        &self,
        target: u64,
        timeout: Duration,
        code: &'static str,
        ready: impl Fn(&BlockSnapshot) -> Result<bool, SemanticError>,
    ) -> Result<PeerSnapshot, SemanticError> {
        let started = Instant::now();
        loop {
            match self.sync_snapshot() {
                Ok(snapshot) => {
                    if snapshot.basis_t() >= target && ready(&snapshot.native)? {
                        return Ok(snapshot);
                    }
                }
                Err(error)
                    if error.category == ErrorCategory::Unavailable
                        || error.category == ErrorCategory::Conflict => {}
                Err(error) => return Err(error),
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(SemanticError::new(
                    ErrorCategory::Unavailable,
                    code,
                    "Timed out waiting for the requested database frontier",
                )
                .detail("target_t", target.to_string()));
            }
            std::thread::sleep(remaining.min(Duration::from_millis(10)));
        }
    }

    pub fn enable_tx_reports(&self) -> bool {
        let _update = lock(&self.core.update);
        let mut reports = lock(&self.core.reports);
        if reports.is_some() {
            return false;
        }
        *reports = Some(VecDeque::new());
        true
    }
    pub fn remove_tx_reports(&self) -> bool {
        let _update = lock(&self.core.update);
        let removed = lock(&self.core.reports).take().is_some();
        self.core.report_ready.notify_all();
        removed
    }
    pub fn tx_reports_enabled(&self) -> bool {
        lock(&self.core.reports).is_some()
    }
    pub fn take_tx_reports(&self) -> Vec<ServiceTransactionReport> {
        let _update = lock(&self.core.update);
        lock(&self.core.reports)
            .as_mut()
            .map_or_else(Vec::new, |queue| queue.drain(..).collect())
    }
    pub fn try_next_tx_report(&self) -> Option<ServiceTransactionReport> {
        lock(&self.core.reports)
            .as_mut()
            .and_then(VecDeque::pop_front)
    }
    pub fn next_tx_report(
        &self,
        timeout: Duration,
    ) -> Result<Option<ServiceTransactionReport>, SemanticError> {
        let start = Instant::now();
        let mut slot = lock(&self.core.reports);
        loop {
            let Some(queue) = slot.as_mut() else {
                return Ok(None);
            };
            if let Some(report) = queue.pop_front() {
                return Ok(Some(report));
            }
            let remaining = timeout.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return Ok(None);
            }
            let (next, timed) = self
                .core
                .report_ready
                .wait_timeout(slot, remaining)
                .unwrap_or_else(|e| e.into_inner());
            slot = next;
            if timed.timed_out() {
                return Ok(None);
            }
        }
    }

    /// Local receipt observation never performs storage I/O after acknowledgement.
    pub(crate) fn adopt_committed_report(
        &self,
        report: &ServiceTransactionReport,
    ) -> Result<DatabaseValue, SemanticError> {
        let before = direct_report_value(&report.db_before)?;
        let after = direct_report_value(&report.db_after)?;
        if before.lineage_id() != self.core.identity.lineage_id()
            || after.lineage_id() != before.lineage_id()
        {
            return Err(identity_error());
        }
        if before.basis_t().checked_add(1) != Some(report.basis_t)
            || after.basis_t() != report.basis_t
            || transaction_hash(&after) != report.tx_hash
            || before.generation() != after.generation()
        {
            return Err(fault(
                "peer/report-coordinate",
                "Receipt values do not form their claimed successor",
            ));
        }
        let _update = lock(&self.core.update);
        let previous = self.state();
        if previous.snapshot.basis_t() > report.basis_t {
            return Ok(previous.snapshot.database_value());
        }
        if previous.snapshot.basis_t() == report.basis_t {
            if transaction_hash(&previous.snapshot) != report.tx_hash {
                return Err(fault(
                    "peer/report-fork",
                    "Receipt conflicts with the observed basis",
                ));
            }
            return Ok(previous.snapshot.database_value());
        }
        if previous.snapshot.basis_t() != before.basis_t()
            || transaction_hash(&previous.snapshot) != transaction_hash(&before)
            || previous.snapshot.generation() != before.generation()
        {
            return Err(fault(
                "peer/report-gap",
                "Receipt is not a contiguous observed successor",
            ));
        }
        let after = self.core.reader.adopt_snapshot(&after);
        self.publish(State {
            snapshot: after.clone(),
            publication_revision: previous.publication_revision,
            observation_generation: previous.observation_generation.saturating_add(1),
        });
        if !report.replayed
            && let Some(queue) = lock(&self.core.reports).as_mut()
        {
            queue.push_back(report.clone());
            self.core.report_ready.notify_one();
        }
        Ok(after.database_value())
    }

    #[cfg(unix)]
    pub(crate) fn open_socket_report(
        &self,
        wire: crate::encoding::WireReport,
    ) -> Result<ServiceTransactionReport, SemanticError> {
        let mut report = self
            .core
            .reader
            .exact_report(&self.core.reference, wire.after.basis_t)?;
        let matches = |value: &DatabaseValue,
                       endpoint: &crate::encoding::ExactEndpoint,
                       root: Digest|
         -> Result<bool, SemanticError> {
            let (snapshot, ..) = value.committed_block_parts()?;
            let key = value.snapshot_key()?;
            let same_root = if snapshot.storage_root_id() == root {
                true
            } else {
                let bytes = self.core.reader.read_object(root)?;
                let block = crate::storage::root::Block::decode(&root, &bytes)?;
                let claimed = match block.kind {
                    crate::storage::root::DATABASE_ROOT_KIND => {
                        crate::storage::root::DatabaseValueRoot::from(
                            &crate::storage::root::DatabaseRoot::decode(&root, &bytes)?,
                        )
                    }
                    crate::storage::root::DATABASE_VALUE_ROOT_KIND => {
                        crate::storage::root::DatabaseValueRoot::decode(&root, &bytes)?
                    }
                    _ => return Ok(false),
                };
                claimed == *snapshot.captured_root()
            };
            Ok(same_root
                && key.basis_t() == endpoint.basis_t
                && key.generation() == endpoint.generation
                && key.transaction_hash() == endpoint.tx_hash
                && key.state_hash() == endpoint.state_hash
                && key.eidx_frontier() == endpoint.eidx_frontier)
        };
        if !matches(&report.db_before, &wire.before, wire.before_manifest)?
            || !matches(&report.db_after, &wire.after, wire.after_manifest)?
            || report.tx_data != wire.tx_data
            || report.tempids != wire.tempids
        {
            return Err(fault(
                "transport/report-coordinate",
                "Transport receipt differs from its authenticated outcome",
            ));
        }
        report.replayed = wire.replayed;
        Ok(report)
    }
}

// TWatcherImpl.sync-background-t / db.ts-needing-index inspect requests only
// through the requested T, not every job visible at a newer observed head.
// Native descriptors can publish a new basis with partial AVET checkpoints,
// so index basis alone is not completion. Inspect their existing schema
// history at the frozen descriptor basis before considering the newer tail:
// a later disable/re-enable must not conceal unfinished earlier work.
fn avet_ready_through(
    snapshot: &BlockSnapshot,
    target: u64,
    require_removals: bool,
) -> Result<bool, SemanticError> {
    let descriptor = snapshot.index_descriptor();
    let value = snapshot.database_value();
    for &attribute in &descriptor.pending_avet {
        if !avet_transition_after(&value, attribute, descriptor.basis, true, target)? {
            return Ok(false);
        }
    }
    if require_removals {
        for work in descriptor.avet_work.iter().filter(|work| !work.adding) {
            if !avet_transition_after(&value, work.attribute, descriptor.basis, false, target)? {
                return Ok(false);
            }
        }
    } else {
        for &attribute in snapshot.avet_unready() {
            if !avet_transition_after(&value, attribute, snapshot.basis_t(), true, target)? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// Whether the effective membership transition belongs strictly after target.
/// Both index and unique flags confer AVET. Fold each transaction atomically:
/// replacing one unique mode or moving between flags is not a disable/enable.
/// Only pending attributes are read, using selective schema-entity prefixes.
fn avet_transition_after(
    value: &DatabaseValue,
    attribute: u32,
    through: u64,
    adding: bool,
    target: u64,
) -> Result<bool, SemanticError> {
    #[derive(Default)]
    struct Change {
        retract_index: bool,
        add_index: Option<bool>,
        retract_unique: bool,
        add_unique: bool,
    }
    let mut changes = std::collections::BTreeMap::<u64, Change>::new();
    let history = value.clone().history();
    for schema_attribute in [crate::DB_INDEX, crate::DB_UNIQUE] {
        for datom in history.prefix_cursor(&crate::IndexPrefix::Eavt {
            entity: u64::from(attribute),
            attribute: Some(schema_attribute as u32),
            value: None,
        })? {
            let datom = datom?;
            let t = crate::tx_to_t(datom.tx)?;
            if t > through {
                continue;
            }
            let change = changes.entry(t).or_default();
            if schema_attribute == crate::DB_INDEX {
                let crate::Value::Bool(indexed) = datom.value else {
                    return Err(fault(
                        "peer/schema-index-value",
                        "Index schema fact is not boolean",
                    ));
                };
                if datom.added {
                    change.add_index = Some(indexed);
                } else {
                    change.retract_index = true;
                }
            } else if datom.added {
                change.add_unique = true;
            } else {
                change.retract_unique = true;
            }
        }
    }
    let (mut indexed, mut unique, mut latest_transition) = (false, false, None);
    for (t, change) in changes {
        let before = indexed || unique;
        if change.retract_index {
            indexed = false;
        }
        if let Some(next) = change.add_index {
            indexed = next;
        }
        if change.retract_unique {
            unique = false;
        }
        unique |= change.add_unique;
        if before != (indexed || unique) {
            latest_transition = Some(t);
        }
    }
    // Absent/inconsistent provenance cannot establish that pending work is
    // later than the requested coordinate; conservatively keep waiting.
    Ok((indexed || unique) == adding && latest_transition.is_some_and(|t| t > target))
}

fn direct_report_value(value: &DatabaseValue) -> Result<BlockSnapshot, SemanticError> {
    let (snapshot, as_of, since, history) = value.committed_block_parts()?;
    if as_of.is_some() || since.is_some() || history {
        return Err(fault(
            "peer/report-value",
            "Receipt requires direct committed values",
        ));
    }
    Ok(snapshot)
}
fn fault(code: &'static str, message: &'static str) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, code, message)
}
fn identity_error() -> SemanticError {
    SemanticError::new(
        ErrorCategory::Forbidden,
        "peer/identity-mismatch",
        "Captured database identity does not match the publication",
    )
}

fn parse_identity(text: &str) -> Result<[u8; 16], SemanticError> {
    if text.len() != 36 {
        return Err(identity_error());
    }
    let compact = text.replace('-', "");
    let bytes = u128::from_str_radix(&compact, 16)
        .map_err(|_| identity_error())?
        .to_be_bytes();
    if bytes == [0; 16] || crate::storage::catalog::identity_string(bytes) != text {
        return Err(identity_error());
    }
    Ok(bytes)
}
