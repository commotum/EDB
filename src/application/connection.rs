//! Native application composition: one advancing reader and an optional submission/service owner.
use crate::peer::Peer;
use crate::{
    DatabaseIdentity, DatabaseValue, Datom, Entity, EntityIdentifier, IndexBoundary, IndexOrder,
    IndexPrefix, PostgresConnectionConfig, PullPattern, Query, QueryControl, QueryExtensions,
    QueryInput, QueryOutcome, QueryValue, SemanticError, ServiceTransactionReport,
    TransactionClient, TransactionRequest, TransactionService, TransactionServiceConfig,
    TransactionTicket,
};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

type NoticeChannel = (
    mpsc::SyncSender<Arc<ServiceTransactionReport>>,
    mpsc::Receiver<Arc<ServiceTransactionReport>>,
);

struct ConnectionCore {
    identity: DatabaseIdentity,
    peer: Peer,
    client: Mutex<Option<TransactionClient>>,
    observer: Mutex<Option<crate::transactor::client::NativeObserver>>,
    notices: mpsc::SyncSender<Arc<ServiceTransactionReport>>,
    observation_error: Arc<Mutex<Option<SemanticError>>>,
    observation_work: crate::OperationContext,
    _advancement: Advancement,
    // The embedded service owns the single-writer lease and both worker
    // lifetimes. It is intentionally retained by the last Connection clone.
    _service: Option<TransactionService>,
}

struct Advancement {
    stop: mpsc::Sender<()>,
    worker: Option<JoinHandle<()>>,
}

impl Drop for Advancement {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take()
            && worker.is_finished()
        {
            let _ = worker.join();
        }
        // A pending peer read may be waiting on SQL or another snapshot's I/O.
        // Request stop without making dropping a read handle wait for it. The
        // worker owns only one peer, releases it when that operation returns,
        // and never starts another iteration after observing stop.
    }
}

/// Cloneable, long-lived native connection to one PostgreSQL database.
///
/// The mutable connection is only
/// an observation/co-ordination point. Every `db` call returns an immutable
/// value, and advancing this shared connection never changes an older value.
/// Reads follow the authoritative PostgreSQL log independently of the writer.
/// An attached TransactionClient is an in-process submission endpoint; opening
/// a read connection does not start or retain a transaction service.
#[derive(Clone)]
pub struct Connection {
    core: Arc<ConnectionCore>,
}

impl Connection {
    /// Open an independently advancing native read connection, without a writer.
    pub fn connect(
        connection: &str,
        database_id: impl Into<String>,
        cache_capacity: usize,
    ) -> Result<Self, SemanticError> {
        Self::connect_configured(
            PostgresConnectionConfig::parse(connection)?,
            database_id,
            cache_capacity,
        )
    }

    pub fn connect_configured(
        connection: PostgresConnectionConfig,
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

    /// Open a writer-independent read connection with separate immutable-node
    /// cache entry and accounted-byte limits. Zero disables cache retention.
    /// These limits do not cap recent information, resident roots/schema,
    /// caller-retained values, or total process memory.
    pub fn connect_with_cache_limits(
        connection: &str,
        database_id: impl Into<String>,
        cache_entries: usize,
        cache_bytes: usize,
    ) -> Result<Self, SemanticError> {
        Self::connect_configured_with_cache_limits(
            PostgresConnectionConfig::parse(connection)?,
            database_id,
            cache_entries,
            cache_bytes,
        )
    }

    /// Open with explicit transport/I/O policy and independent cache limits.
    /// As with `connect_configured`, the configuration is owned by the reader;
    /// opening it does not acquire or retain a writer's lease.
    pub fn connect_configured_with_cache_limits(
        connection: PostgresConnectionConfig,
        database_id: impl Into<String>,
        cache_entries: usize,
        cache_bytes: usize,
    ) -> Result<Self, SemanticError> {
        Self::connect_configured_with_observation(
            connection,
            database_id,
            cache_entries,
            cache_bytes,
            crate::ObservationConfig::default(),
        )
    }

    /// Configure durable gap-repair cadence independently of hint delivery.
    pub fn connect_configured_with_observation(
        connection: PostgresConnectionConfig,
        database_id: impl Into<String>,
        cache_entries: usize,
        cache_bytes: usize,
        observation: crate::ObservationConfig,
    ) -> Result<Self, SemanticError> {
        observation.validate()?;
        let peer = Peer::connect_configured_with_cache_limits(
            &connection,
            database_id,
            cache_entries,
            cache_bytes,
        )?;
        Self::from_peer(
            peer,
            connection,
            observation,
            None,
            None,
            None,
            Arc::new(Mutex::new(None)),
            mpsc::sync_channel(1),
        )
    }

    /// Attach to an existing writer without owning its lifetime or lease.
    pub fn attach(
        connection: &str,
        client: TransactionClient,
        cache_capacity: usize,
    ) -> Result<Self, SemanticError> {
        Self::attach_configured(
            PostgresConnectionConfig::parse(connection)?,
            client,
            cache_capacity,
        )
    }

    pub fn attach_configured(
        connection: PostgresConnectionConfig,
        client: TransactionClient,
        cache_capacity: usize,
    ) -> Result<Self, SemanticError> {
        Self::open_attached(connection, client, cache_capacity, None)
    }

    fn open_attached(
        connection: PostgresConnectionConfig,
        client: TransactionClient,
        cache_capacity: usize,
        service: Option<TransactionService>,
    ) -> Result<Self, SemanticError> {
        let identity = client.identity();
        let observation_error = Arc::new(Mutex::new(None));
        let notices = mpsc::sync_channel(1);
        let peer = Peer::connect_identity_configured(&connection, &identity, cache_capacity)?;
        if peer.identity() != identity {
            return Err(identity_mismatch(&identity, &peer.identity()));
        }
        let observer = client.observe_commits(notices.0.clone());
        Self::from_peer(
            peer,
            connection,
            crate::ObservationConfig::default(),
            Some(client),
            Some(observer),
            service,
            observation_error,
            notices,
        )
    }

    // Assemble the captured peer and its optional service/observation ownership in one place.
    #[allow(clippy::too_many_arguments)]
    fn from_peer(
        peer: Peer,
        connection: PostgresConnectionConfig,
        observation: crate::ObservationConfig,
        client: Option<TransactionClient>,
        observer: Option<crate::transactor::client::NativeObserver>,
        service: Option<TransactionService>,
        observation_error: Arc<Mutex<Option<SemanticError>>>,
        notices: NoticeChannel,
    ) -> Result<Self, SemanticError> {
        let (stop, stopped) = mpsc::channel();
        let follower = peer.clone();
        let error_slot = Arc::clone(&observation_error);
        let (notice_sender, notice_receiver) = notices;
        let observation_work = crate::OperationContext::new(crate::OperationKind::PeerObservation);
        let operation = observation_work.clone();
        let database_id = peer.identity().database_id().to_owned();
        let worker = std::thread::Builder::new()
            .name("edb-peer-observer".into())
            .spawn(move || {
                let _scope = operation.enter();
                let mut listener = None;
                let mut reconnect_at = Instant::now();
                let mut refresh_at = Instant::now();
                while matches!(stopped.try_recv(), Err(mpsc::TryRecvError::Empty)) {
                    let mut needs_catchup = false;
                    match notice_receiver.try_recv() {
                        Ok(report) => {
                            if let Err(error) = follower.adopt_committed_report(&report) {
                                *error_slot.lock().expect("observation mutex poisoned") =
                                    Some(error);
                                needs_catchup = true;
                            }
                        }
                        Err(mpsc::TryRecvError::Empty) => {}
                        Err(mpsc::TryRecvError::Disconnected) => break,
                    }
                    if listener.is_none() && Instant::now() >= reconnect_at {
                        match crate::observation::notices::NoticeListener::connect(
                            &connection,
                            &database_id,
                        ) {
                            Ok(connected) => {
                                listener = Some(connected);
                                needs_catchup = true;
                            }
                            Err(error) => {
                                *error_slot.lock().expect("observation mutex poisoned") =
                                    Some(error);
                                reconnect_at = Instant::now() + Duration::from_secs(1);
                            }
                        }
                    }
                    if let Some(active) = listener.as_mut() {
                        match active.wait(Duration::from_millis(25)) {
                            Ok(notice) => needs_catchup |= notice,
                            Err(error) => {
                                *error_slot.lock().expect("observation mutex poisoned") =
                                    Some(error);
                                listener = None;
                                reconnect_at = Instant::now() + Duration::from_secs(1);
                                needs_catchup = true;
                            }
                        }
                    } else if stopped.recv_timeout(Duration::from_millis(25)).is_ok() {
                        break;
                    }
                    if needs_catchup || Instant::now() >= refresh_at {
                        let result = follower
                            .sync_database_value()
                            .and_then(|_| follower.refresh_index());
                        *error_slot.lock().expect("observation mutex poisoned") = result.err();
                        refresh_at = Instant::now() + observation.anti_entropy_interval;
                    }
                }
            })
            .map_err(|error| {
                SemanticError::new(
                    crate::ErrorCategory::Unavailable,
                    "connection/observer-start",
                    error.to_string(),
                )
            })?;
        Ok(Self {
            core: Arc::new(ConnectionCore {
                identity: peer.identity(),
                peer,
                client: Mutex::new(client),
                observer: Mutex::new(observer),
                notices: notice_sender,
                observation_error,
                observation_work,
                _advancement: Advancement {
                    stop,
                    worker: Some(worker),
                },
                _service: service,
            }),
        })
    }

    /// Replace or attach the submission endpoint after writer failover. Peer
    /// state and previously captured values remain independent of that writer.
    pub fn attach_writer(&self, client: TransactionClient) -> Result<(), SemanticError> {
        if client.identity() != self.core.identity {
            return Err(identity_mismatch(&client.identity(), &self.core.identity));
        }
        let mut endpoint = self.core.client.lock().expect("client mutex poisoned");
        self.core.peer.sync_database_value()?;
        let observer = client.observe_commits(self.core.notices.clone());
        *self.core.observer.lock().expect("observer mutex poisoned") = Some(observer);
        *endpoint = Some(client);
        Ok(())
    }

    /// Most recent background/adoption failure, separate from transaction
    /// outcomes. A subsequent successful catch-up clears it. Captured values
    /// remain valid even while this connection cannot advance.
    pub fn observation_error(&self) -> Option<SemanticError> {
        self.core
            .observation_error
            .lock()
            .expect("observation mutex poisoned")
            .clone()
    }

    /// All listener, reconnect and authenticated catch-up driver work.
    /// Waiting for a network hint sends no SQL and is not counted as a call.
    pub fn observation_sql_stats(&self) -> crate::SqlIoStats {
        self.core.observation_work.snapshot()
    }

    pub fn load_stats(&self) -> crate::PeerLoadStats {
        self.core.peer.load_stats()
    }

    /// Shared immutable-node cache counters, including byte/entry residency
    /// and eviction. This is representation accounting, not process RSS.
    pub fn cache_stats(&self) -> crate::CacheStats {
        self.core.peer.cache_stats()
    }

    pub fn ssd_cache_stats(&self) -> crate::SsdCacheStats {
        self.core.peer.ssd_cache_stats()
    }

    pub fn node_block_read_stats(&self) -> crate::NodeBlockReadStats {
        self.core.peer.node_block_read_stats()
    }

    pub fn purge_ssd_generation(&self, generation: u64) -> bool {
        self.core.peer.purge_ssd_generation(generation)
    }

    /// Recent-log residency of the latest adopted value, independent of the
    /// immutable-node cache and of older values retained by callers.
    pub fn recent_stats(&self) -> crate::index::recent::RecentStats {
        self.core.peer.recent_stats()
    }

    #[cfg(unix)]
    pub(crate) fn open_socket_report(
        &self,
        wire: crate::encoding::WireReport,
    ) -> Result<ServiceTransactionReport, SemanticError> {
        let report = self.core.peer.open_socket_report(wire)?;
        if let Err(error) = self.core.peer.adopt_committed_report(&report) {
            *self
                .core
                .observation_error
                .lock()
                .expect("observation mutex poisoned") = Some(error);
        }
        Ok(report)
    }
    /// Start the embedded transactor and native peer with the same typed connection.
    pub fn start(
        config: TransactionServiceConfig,
        cache_capacity: usize,
    ) -> Result<Self, SemanticError> {
        let connection = config.connection.clone();
        let service = TransactionService::start(config)?;
        let client = service.client();
        Self::open_attached(connection, client, cache_capacity, Some(service))
    }

    pub fn identity(&self) -> &DatabaseIdentity {
        &self.core.identity
    }

    /// Return the connection's most recently adopted immutable value without
    /// PostgreSQL I/O or transactor co-ordination.
    pub fn db(&self) -> DatabaseValue {
        self.core.peer.database_value()
    }

    /// Reopen an exact supported retained value, without advancing this connection.
    pub fn reopen_snapshot(
        &self,
        reference: &crate::SnapshotReference,
    ) -> Result<DatabaseValue, SemanticError> {
        self.core.peer.reopen_snapshot(reference)
    }

    /// Capture an immutable transaction log without contacting the writer.
    pub fn log(&self) -> crate::LogValue {
        self.core.peer.log()
    }

    /// Capture and adopt the newest durable head visible at the time of this
    /// call. This is the native synchronous counterpart of recovered
    /// zero-argument `sync`.
    pub fn sync(&self) -> Result<DatabaseValue, SemanticError> {
        self.core.peer.sync_database_value()
    }

    /// Wait until this connection has adopted at least `target_t` without
    /// communicating with a separate transactor endpoint. The background
    /// observer performs storage I/O; a stalled read cannot extend this wait.
    pub fn sync_to(
        &self,
        target_t: u64,
        timeout: Duration,
    ) -> Result<DatabaseValue, SemanticError> {
        self.core.peer.wait_for_local_basis(target_t, timeout)?;
        Ok(self.db())
    }

    pub fn sync_index(
        &self,
        target_t: u64,
        timeout: Duration,
    ) -> Result<DatabaseValue, SemanticError> {
        self.core.peer.sync_index(target_t, timeout)
    }

    /// Schedule indexing at a finite committed target on an attached writer.
    ///
    /// This is asynchronous; use the returned `target_t` with `sync_index` to
    /// observe completion. Later transactions do not extend that wait target.
    /// A writer-independent read connection has no control endpoint and returns
    /// `connection/no-writer`; the local transaction socket does not currently
    /// carry maintenance requests. Dropping this receipt or timing out a wait
    /// does not cancel shared background work.
    pub fn request_index(&self) -> Result<crate::IndexRequest, SemanticError> {
        let client = self.core.client.lock().expect("client mutex poisoned");
        let client = client.as_ref().ok_or_else(|| {
            SemanticError::new(
                crate::ErrorCategory::Unavailable,
                "connection/no-writer",
                "read-only connection has no attached transaction endpoint",
            )
        })?;
        client.request_index()
    }

    pub fn sync_schema(
        &self,
        target_t: u64,
        timeout: Duration,
    ) -> Result<DatabaseValue, SemanticError> {
        self.core.peer.sync_schema(target_t, timeout)
    }

    pub fn sync_excise(
        &self,
        target_t: u64,
        timeout: Duration,
    ) -> Result<DatabaseValue, SemanticError> {
        self.core.peer.sync_excise(target_t, timeout)
    }

    /// Admit a transaction without blocking. The returned ticket does not
    /// expose a successful report until ordered local observation has been
    /// attempted. Observation faults are separate from a known durable success.
    pub fn submit(
        &self,
        request: TransactionRequest,
    ) -> Result<ConnectionTransactionTicket, SemanticError> {
        let client = self.core.client.lock().expect("client mutex poisoned");
        let client = client.as_ref().ok_or_else(|| {
            SemanticError::new(
                crate::ErrorCategory::Unavailable,
                "connection/no-writer",
                "read-only connection has no attached transaction endpoint",
            )
        })?;
        Ok(ConnectionTransactionTicket {
            ticket: client.submit(request)?,
            connection: self.clone(),
        })
    }

    /// Same admission and report adoption as submit, with separate advisory
    /// prefetch data. The hint handle remains usable after waiting on the ticket.
    pub fn submit_with_hints(
        &self,
        request: TransactionRequest,
        hints: crate::TransactionHints,
        options: crate::HintPrefetchOptions,
    ) -> Result<(ConnectionTransactionTicket, crate::HintExecution), SemanticError> {
        let client = self.core.client.lock().expect("client mutex poisoned");
        let client = client.as_ref().ok_or_else(|| {
            SemanticError::new(
                crate::ErrorCategory::Unavailable,
                "connection/no-writer",
                "read-only connection has no attached transaction endpoint",
            )
        })?;
        let (ticket, execution) = client.submit_with_hints(request, hints, options)?;
        Ok((
            ConnectionTransactionTicket {
                ticket,
                connection: self.clone(),
            },
            execution,
        ))
    }

    pub fn transact(
        &self,
        request: TransactionRequest,
        timeout: Duration,
    ) -> Result<ServiceTransactionReport, SemanticError> {
        self.submit(request)?.wait(timeout)
    }

    /// Enable the single unbounded report queue associated with this
    /// connection. Reports begin with the next transaction adopted after the
    /// queue is enabled; no already-observed prefix is manufactured.
    pub fn enable_transaction_reports(&self) -> bool {
        self.core.peer.enable_tx_reports()
    }

    pub fn try_next_transaction_report(&self) -> Option<ServiceTransactionReport> {
        self.core.peer.try_next_tx_report()
    }

    pub fn next_transaction_report(
        &self,
        timeout: Duration,
    ) -> Result<Option<ServiceTransactionReport>, SemanticError> {
        self.core.peer.next_tx_report(timeout)
    }

    /// Remove this connection's report queue and release all unconsumed
    /// immutable db-before/db-after values.
    pub fn remove_transaction_report_queue(&self) -> bool {
        self.core.peer.remove_tx_reports()
    }

    /// Query convenience that captures exactly one immutable value.
    pub fn query(
        &self,
        query: &Query,
        inputs: &[QueryInput],
        control: &QueryControl,
    ) -> Result<QueryOutcome, SemanticError> {
        let database = self.db();
        database.query(query, inputs, control)
    }

    pub fn query_with_extensions(
        &self,
        query: &Query,
        inputs: &[QueryInput],
        control: &QueryControl,
        extensions: &QueryExtensions,
    ) -> Result<QueryOutcome, SemanticError> {
        let database = self.db();
        database.query_with_extensions(query, inputs, control, extensions)
    }

    /// Pull convenience that pins all recursive navigation to one immutable
    /// value even if this connection advances concurrently.
    pub fn pull(
        &self,
        pattern: &PullPattern,
        entity: impl Into<EntityIdentifier>,
    ) -> Result<QueryValue, SemanticError> {
        let database = self.db();
        database.pull(pattern, entity)
    }

    /// Entity convenience that pins subsequent lazy navigation to the one
    /// immutable value captured here.
    pub fn entity(
        &self,
        identifier: impl Into<EntityIdentifier>,
    ) -> Result<Option<Entity>, SemanticError> {
        let database = self.db();
        database.entity(identifier)
    }

    pub fn collect_datoms(&self, order: IndexOrder) -> Result<Vec<Datom>, SemanticError> {
        let database = self.db();
        database.collect_datoms(order)
    }

    pub fn collect_seek_datoms(
        &self,
        boundary: &IndexBoundary,
    ) -> Result<Vec<Datom>, SemanticError> {
        let database = self.db();
        database.collect_seek_datoms(boundary)
    }

    pub fn collect_reverse_seek_datoms(
        &self,
        boundary: &IndexBoundary,
    ) -> Result<Vec<Datom>, SemanticError> {
        let database = self.db();
        database.collect_reverse_seek_datoms(boundary)
    }

    pub fn collect_datoms_with_prefix(
        &self,
        prefix: &IndexPrefix,
    ) -> Result<Vec<Datom>, SemanticError> {
        let database = self.db();
        database.collect_datoms_with_prefix(prefix)
    }
}

/// An admitted transaction. A separate peer worker adopts ordered commits even
/// without ticket waits. Waiters coordinate with local adoption within their
/// remaining deadline; a known commit is never turned into a rejection if that
/// observation times out (see `Connection::observation_error`).
pub struct ConnectionTransactionTicket {
    ticket: TransactionTicket,
    connection: Connection,
}

impl ConnectionTransactionTicket {
    pub fn wait(self, timeout: Duration) -> Result<ServiceTransactionReport, SemanticError> {
        let started = Instant::now();
        let report = self.ticket.wait(timeout)?;
        if let Err(error) = self
            .connection
            .core
            .peer
            .wait_for_local_basis(report.basis_t, timeout.saturating_sub(started.elapsed()))
        {
            *self
                .connection
                .core
                .observation_error
                .lock()
                .expect("observation mutex poisoned") = Some(error);
        }
        Ok(report)
    }

    pub fn request_key(&self) -> &str {
        self.ticket.request_key()
    }
}

fn identity_mismatch(client: &DatabaseIdentity, peer: &DatabaseIdentity) -> SemanticError {
    SemanticError::new(
        crate::ErrorCategory::Fault,
        "connection/database-identity-mismatch",
        "the transaction client and native peer do not address the same database lineage",
    )
    .detail("client_database_id", client.database_id())
    .detail("client_lineage_id", client.lineage_id())
    .detail("peer_database_id", peer.database_id())
    .detail("peer_lineage_id", peer.lineage_id())
}
