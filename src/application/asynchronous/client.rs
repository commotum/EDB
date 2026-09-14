//! Nonblocking operation/stream facade over the native Rust engine.
//! Native results returned to callers retain their synchronous methods and
//! final-drop behavior. Abandoned facade-owned resources are cleaned on workers.
use super::{AsyncStream, AsyncStreamOptions};
use crate::runtime::executor::{AsyncExecutor, AsyncOperation};

use crate::runtime::executor::{Owned, deadline, remaining};
use crate::{
    Connection, DatabaseIdentity, DatabaseValue, Datom, EntityIdentifier, IndexOrder, IndexPrefix,
    LogTransaction, LogValue, PreparedQuery, PullControl, PullPattern, Query, QueryControl,
    QueryDataSource, QueryEngine, QueryExtensions, QueryInput, QueryOutcome, QueryValue,
    SemanticError, ServiceTransactionReport, SnapshotReference, TimePoint, TransactionRequest,
};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

/// Nonblocking operations against one already-captured native connection.
/// Construction performs no I/O and never resolves a mutable name again.
/// The executor retains this connection until worker-side cleanup, so dropping
/// the last facade does not join an embedded transactor on an executor thread.
#[derive(Clone)]
pub struct AsyncClient {
    connection: Owned<Connection>,
    executor: AsyncExecutor,
}
impl std::fmt::Debug for AsyncClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncClient")
            .field("identity", self.identity())
            .field("executor", &self.executor)
            .finish()
    }
}
impl AsyncClient {
    pub fn new(connection: &Connection, executor: &AsyncExecutor) -> Result<Self, SemanticError> {
        Ok(Self {
            connection: executor.retain(|| connection.clone())?,
            executor: executor.clone(),
        })
    }
    pub fn identity(&self) -> &DatabaseIdentity {
        self.connection.identity()
    }
    pub fn executor(&self) -> &AsyncExecutor {
        &self.executor
    }

    /// Capture one immutable value now, not on first Future poll. Consuming
    /// the result transfers its ordinary native destructor contract to you.
    pub fn db(&self) -> Result<AsyncOperation<DatabaseValue>, SemanticError> {
        let reservation = self.executor.reserve()?;
        let database = self.connection.db();
        Ok(reservation.run(None, false, move || Ok(database)))
    }
    pub fn log(&self) -> Result<AsyncOperation<LogValue>, SemanticError> {
        let reservation = self.executor.reserve()?;
        let log = self.connection.log();
        Ok(reservation.run(None, false, move || Ok(log)))
    }
    pub fn query(
        &self,
        query: Query,
        inputs: Vec<QueryInput>,
        control: QueryControl,
    ) -> Result<AsyncOperation<QueryOutcome>, SemanticError> {
        self.query_with_extensions(query, inputs, control, None)
    }
    pub fn query_with_extensions(
        &self,
        query: Query,
        inputs: Vec<QueryInput>,
        mut control: QueryControl,
        extensions: Option<QueryExtensions>,
    ) -> Result<AsyncOperation<QueryOutcome>, SemanticError> {
        let deadline = deadline(control.timeout)?;
        let reservation = self.executor.reserve()?;
        let database = self.connection.db();
        Ok(reservation.run(deadline, false, move || {
            control.timeout = remaining(deadline);
            match extensions {
                Some(extensions) => {
                    database.query_with_extensions(&query, &inputs, &control, &extensions)
                }
                None => database.query(&query, &inputs, &control),
            }
        }))
    }
    pub fn pull(
        &self,
        pattern: PullPattern,
        entity: impl Into<EntityIdentifier>,
        control: PullControl,
        timeout: Option<Duration>,
    ) -> Result<AsyncOperation<QueryValue>, SemanticError> {
        let deadline = deadline(timeout)?;
        let reservation = self.executor.reserve()?;
        let database = self.connection.db();
        let entity = entity.into();
        Ok(reservation.run(deadline, false, move || {
            database.pull_with_control(&pattern, entity, &control)
        }))
    }
    pub fn reopen_snapshot(
        &self,
        reference: SnapshotReference,
        timeout: Option<Duration>,
    ) -> Result<AsyncOperation<DatabaseValue>, SemanticError> {
        self.coordinate(timeout, move |connection, _| {
            connection.reopen_snapshot(&reference)
        })
    }
    /// Head capture occurs on a worker and includes commits already complete
    /// at admission. Blocking SQL remains subject to configured native I/O
    /// policies; this optional deadline also rejects expired queued work.
    pub fn sync(
        &self,
        timeout: Option<Duration>,
    ) -> Result<AsyncOperation<DatabaseValue>, SemanticError> {
        self.coordinate(timeout, |connection, _| connection.sync())
    }
    pub fn sync_to(
        &self,
        target: u64,
        timeout: Duration,
    ) -> Result<AsyncOperation<DatabaseValue>, SemanticError> {
        self.coordinate(Some(timeout), move |connection, remaining| {
            connection.sync_to(target, remaining.unwrap())
        })
    }
    pub fn sync_index(
        &self,
        target: u64,
        timeout: Duration,
    ) -> Result<AsyncOperation<DatabaseValue>, SemanticError> {
        self.coordinate(Some(timeout), move |connection, remaining| {
            connection.sync_index(target, remaining.unwrap())
        })
    }
    pub fn sync_schema(
        &self,
        target: u64,
        timeout: Duration,
    ) -> Result<AsyncOperation<DatabaseValue>, SemanticError> {
        self.coordinate(Some(timeout), move |connection, remaining| {
            connection.sync_schema(target, remaining.unwrap())
        })
    }
    pub fn sync_excise(
        &self,
        target: u64,
        timeout: Duration,
    ) -> Result<AsyncOperation<DatabaseValue>, SemanticError> {
        self.coordinate(Some(timeout), move |connection, remaining| {
            connection.sync_excise(target, remaining.unwrap())
        })
    }
    pub fn request_index(&self) -> Result<AsyncOperation<crate::IndexRequest>, SemanticError> {
        self.coordinate(None, |connection, _| connection.request_index())
    }
    fn coordinate<T: Send + 'static>(
        &self,
        timeout: Option<Duration>,
        run: impl FnOnce(&Connection, Option<Duration>) -> Result<T, SemanticError> + Send + 'static,
    ) -> Result<AsyncOperation<T>, SemanticError> {
        let deadline = deadline(timeout)?;
        let reservation = self.executor.reserve()?;
        let connection = self.connection.clone();
        Ok(reservation.run(deadline, false, move || {
            run(&connection, remaining(deadline))
        }))
    }

    /// Queue transaction execution without running canonical request encoding
    /// on the caller. Busy admission means it was not submitted. After success
    /// here, dropping the waiter does not cancel this queued transaction.
    pub fn transact(
        &self,
        request: TransactionRequest,
        timeout: Duration,
    ) -> Result<AsyncTransaction, SemanticError> {
        let deadline = deadline(Some(timeout))?;
        let reservation = self.executor.reserve()?;
        let connection = self.connection.clone();
        let request_key = request.request_key.clone();
        let operation = reservation.run(deadline, true, move || {
            connection.transact(request, remaining(deadline).unwrap())
        });
        Ok(AsyncTransaction {
            operation,
            request_key,
        })
    }
    pub fn transact_with_hints(
        &self,
        request: TransactionRequest,
        hints: crate::TransactionHints,
        options: crate::HintPrefetchOptions,
        timeout: Duration,
    ) -> Result<AsyncOperation<(ServiceTransactionReport, crate::HintExecution)>, SemanticError>
    {
        let deadline = deadline(Some(timeout))?;
        let reservation = self.executor.reserve()?;
        let connection = self.connection.clone();
        Ok(reservation.run(deadline, true, move || {
            let (ticket, execution) = connection.submit_with_hints(request, hints, options)?;
            Ok((ticket.wait(remaining(deadline).unwrap())?, execution))
        }))
    }
    #[cfg(unix)]
    pub fn transact_socket(
        &self,
        endpoint: impl Into<std::path::PathBuf>,
        request: TransactionRequest,
        timeout: Duration,
    ) -> Result<AsyncOperation<crate::CommittedTransaction>, SemanticError> {
        let deadline = deadline(Some(timeout))?;
        let reservation = self.executor.reserve()?;
        let connection = self.connection.clone();
        let endpoint = endpoint.into();
        Ok(reservation.run(deadline, true, move || {
            connection.transact_socket(endpoint, request, remaining(deadline).unwrap())
        }))
    }
    #[cfg(unix)]
    pub fn transact_remote(
        &self,
        endpoint: &crate::RemoteWriterEndpoint,
        config: &crate::RemoteClientConfig,
        request: TransactionRequest,
        timeout: Duration,
    ) -> Result<AsyncOperation<crate::CommittedTransaction>, SemanticError> {
        let deadline = deadline(Some(timeout))?;
        let reservation = self.executor.reserve()?;
        let connection = self.connection.clone();
        let endpoint = endpoint.clone();
        let config = config.clone();
        Ok(reservation.run(deadline, true, move || {
            connection.transact_remote(&endpoint, &config, request, remaining(deadline).unwrap())
        }))
    }
    #[cfg(unix)]
    pub fn transact_routed(
        &self,
        route: &crate::RemoteWriter,
        request: TransactionRequest,
        timeout: Duration,
    ) -> Result<AsyncOperation<crate::CommittedTransaction>, SemanticError> {
        if route.identity() != self.identity() {
            return Err(SemanticError::conflict(
                "async/route-identity",
                "async client and remote route identify different databases",
            ));
        }
        let deadline = deadline(Some(timeout))?;
        let reservation = self.executor.reserve()?;
        let route = route.clone();
        Ok(reservation.run(deadline, true, move || {
            route.transact(request, remaining(deadline).unwrap())
        }))
    }
    #[cfg(unix)]
    pub fn transact_routed_with_hints(
        &self,
        route: &crate::RemoteWriter,
        request: TransactionRequest,
        hints: crate::TransactionHints,
        timeout: Duration,
    ) -> Result<AsyncOperation<crate::CommittedTransaction>, SemanticError> {
        if route.identity() != self.identity() {
            return Err(SemanticError::conflict(
                "async/route-identity",
                "async client and remote route identify different databases",
            ));
        }
        let deadline = deadline(Some(timeout))?;
        let reservation = self.executor.reserve()?;
        let route = route.clone();
        Ok(reservation.run(deadline, true, move || {
            route.transact_with_hints(request, hints, remaining(deadline).unwrap())
        }))
    }
    #[cfg(unix)]
    pub fn refresh_route(
        &self,
        route: &crate::RemoteWriter,
        timeout: Duration,
    ) -> Result<AsyncOperation<crate::RemoteWriterEndpoint>, SemanticError> {
        if route.identity() != self.identity() {
            return Err(SemanticError::conflict(
                "async/route-identity",
                "async client and remote route identify different databases",
            ));
        }
        let deadline = deadline(Some(timeout))?;
        let reservation = self.executor.reserve()?;
        let route = route.clone();
        Ok(reservation.run(deadline, false, move || {
            route.refresh(remaining(deadline).unwrap())
        }))
    }
    pub fn query_sequence(
        &self,
        query: Query,
        inputs: Vec<QueryInput>,
        mut control: QueryControl,
        extensions: Option<QueryExtensions>,
        options: AsyncStreamOptions,
    ) -> Result<AsyncStream<Vec<QueryValue>>, SemanticError> {
        let deadline = combined_deadline(control.timeout, options.timeout)?;
        AsyncStream::new(&self.executor, options, deadline, || {
            let database = self.connection.db();
            Box::new(move || {
                control.timeout = remaining(deadline);
                Ok(Box::new(QueryEngine::sequence_sources_with_extensions(
                    &query,
                    &[QueryDataSource::database("$", database)],
                    &inputs,
                    &control,
                    extensions.as_ref(),
                )?))
            })
        })
    }
    pub fn tx_range(
        &self,
        start: Option<TimePoint>,
        end: Option<TimePoint>,
        options: AsyncStreamOptions,
    ) -> Result<AsyncStream<LogTransaction>, SemanticError> {
        let deadline = deadline(options.timeout)?;
        AsyncStream::new(&self.executor, options, deadline, || {
            let log = self.connection.log();
            Box::new(move || Ok(Box::new(log.tx_range(start, end)?)))
        })
    }
    /// Explicitly materialize the native index result on a worker. Query and
    /// log sequences have separate demand-driven stream methods.
    pub fn collect_datoms(
        &self,
        order: IndexOrder,
        timeout: Option<Duration>,
    ) -> Result<AsyncOperation<Vec<Datom>>, SemanticError> {
        let deadline = deadline(timeout)?;
        let reservation = self.executor.reserve()?;
        let database = self.connection.db();
        Ok(reservation.run(deadline, false, move || database.collect_datoms(order)))
    }
    pub fn collect_datoms_with_prefix(
        &self,
        prefix: IndexPrefix,
        timeout: Option<Duration>,
    ) -> Result<AsyncOperation<Vec<Datom>>, SemanticError> {
        let deadline = deadline(timeout)?;
        let reservation = self.executor.reserve()?;
        let database = self.connection.db();
        Ok(reservation.run(deadline, false, move || {
            database.collect_datoms_with_prefix(&prefix)
        }))
    }
}

impl AsyncExecutor {
    /// Borrowed sources are cloned only after a slot is reserved. Their native
    /// values/logs remain fixed even if a connection advances before execution.
    pub fn query_sources(
        &self,
        query: Query,
        sources: &[QueryDataSource],
        inputs: Vec<QueryInput>,
        mut control: QueryControl,
        extensions: Option<QueryExtensions>,
    ) -> Result<AsyncOperation<QueryOutcome>, SemanticError> {
        let deadline = deadline(control.timeout)?;
        let reservation = self.reserve()?;
        let sources = sources.to_vec();
        Ok(reservation.run(deadline, false, move || {
            control.timeout = remaining(deadline);
            QueryEngine::execute_sources_with_extensions(
                &query,
                &sources,
                &inputs,
                &control,
                extensions.as_ref(),
            )
        }))
    }
    pub fn query_prepared(
        &self,
        query: PreparedQuery,
        sources: &[QueryDataSource],
        inputs: Vec<QueryInput>,
        mut control: QueryControl,
        extensions: Option<QueryExtensions>,
    ) -> Result<AsyncOperation<QueryOutcome>, SemanticError> {
        let deadline = deadline(control.timeout)?;
        let reservation = self.reserve()?;
        let sources = sources.to_vec();
        Ok(reservation.run(deadline, false, move || {
            control.timeout = remaining(deadline);
            match extensions {
                Some(extensions) => {
                    query.execute_with_extensions(&sources, &inputs, &control, &extensions)
                }
                None => query.execute(&sources, &inputs, &control),
            }
        }))
    }
    pub fn query_sequence_sources(
        &self,
        query: Query,
        sources: &[QueryDataSource],
        inputs: Vec<QueryInput>,
        mut control: QueryControl,
        extensions: Option<QueryExtensions>,
        options: AsyncStreamOptions,
    ) -> Result<AsyncStream<Vec<QueryValue>>, SemanticError> {
        let deadline = combined_deadline(control.timeout, options.timeout)?;
        AsyncStream::new(self, options, deadline, || {
            let sources = sources.to_vec();
            Box::new(move || {
                control.timeout = remaining(deadline);
                Ok(Box::new(QueryEngine::sequence_sources_with_extensions(
                    &query,
                    &sources,
                    &inputs,
                    &control,
                    extensions.as_ref(),
                )?))
            })
        })
    }
}

fn combined_deadline(
    left: Option<Duration>,
    right: Option<Duration>,
) -> Result<Option<Instant>, SemanticError> {
    deadline(match (left, right) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (left, right) => left.or(right),
    })
}

/// The request key is retained for explicit exact retry. This is an async
/// worker admission receipt, not a claim of durability or cancellation support.
#[derive(Debug)]
pub struct AsyncTransaction {
    operation: AsyncOperation<ServiceTransactionReport>,
    request_key: String,
}
impl AsyncTransaction {
    pub fn request_key(&self) -> &str {
        &self.request_key
    }
    pub fn on_complete(
        self,
        callback: impl FnOnce(Result<ServiceTransactionReport, SemanticError>) + Send + 'static,
    ) {
        self.operation.on_complete(callback);
    }
}
impl Future for AsyncTransaction {
    type Output = Result<ServiceTransactionReport, SemanticError>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.operation).poll(cx)
    }
}
