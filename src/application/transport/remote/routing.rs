//! Connection-owned routing, without implicit replay of ambiguous transactions.
use crate::{
    CommittedTransaction, Connection, DatabaseIdentity, ErrorCategory, PostgresConnectionConfig,
    RemoteClientConfig, RemoteWriterEndpoint, SemanticError, TransactionHints, TransactionRequest,
};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A cached, verified route bound to one captured database identity.
///
/// Construction does no I/O. Submission is synchronous; use the async facade
/// from executor threads. A stale route can be rediscovered once before the
/// request is written. An ambiguous outcome is returned unchanged, never
/// automatically resubmitted. Explicit retries must retain the same key/data.
#[derive(Clone)]
pub struct RemoteWriter(Arc<Inner>);

struct Inner {
    connection: Connection,
    postgres: PostgresConnectionConfig,
    tls: RemoteClientConfig,
    endpoint: Mutex<Option<RemoteWriterEndpoint>>,
}

impl std::fmt::Debug for RemoteWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteWriter")
            .field("identity", self.identity())
            .field("route_cached", &self.cached_endpoint().is_some())
            .finish_non_exhaustive()
    }
}

impl Connection {
    /// Capture this peer's stable identity and verified transport policies.
    /// Renaming or reusing a public name cannot redirect this writer.
    pub fn remote_writer(
        &self,
        postgres: PostgresConnectionConfig,
        tls: RemoteClientConfig,
    ) -> RemoteWriter {
        RemoteWriter(Arc::new(Inner {
            connection: self.clone(),
            postgres,
            tls,
            endpoint: Mutex::new(None),
        }))
    }
}

impl RemoteWriter {
    pub fn identity(&self) -> &DatabaseIdentity {
        self.0.connection.identity()
    }

    /// Local observation only; a cached endpoint is not proof of a live lease.
    pub fn cached_endpoint(&self) -> Option<RemoteWriterEndpoint> {
        self.0
            .endpoint
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Explicitly refresh from authenticated PostgreSQL metadata. The timeout
    /// includes discovery before submission; configured driver I/O limits still
    /// bound individual calls, not DNS/TLS startup or arbitrary blocked I/O.
    pub fn refresh(&self, timeout: Duration) -> Result<RemoteWriterEndpoint, SemanticError> {
        self.discover_until(deadline(timeout)?)
    }

    pub fn transact(
        &self,
        request: TransactionRequest,
        timeout: Duration,
    ) -> Result<CommittedTransaction, SemanticError> {
        self.submit(request, None, timeout)
    }

    pub fn transact_with_hints(
        &self,
        request: TransactionRequest,
        hints: TransactionHints,
        timeout: Duration,
    ) -> Result<CommittedTransaction, SemanticError> {
        self.submit(request, Some(hints), timeout)
    }

    fn submit(
        &self,
        request: TransactionRequest,
        hints: Option<TransactionHints>,
        timeout: Duration,
    ) -> Result<CommittedTransaction, SemanticError> {
        let deadline = deadline(timeout)?;
        let endpoint = match self.cached_endpoint() {
            Some(endpoint) => endpoint,
            None => self.discover_until(deadline)?,
        };
        let mut attempted = false;
        let first = self.0.connection.transact_remote_attempt(
            &endpoint,
            &self.0.tls,
            &request,
            hints.as_ref(),
            remaining(deadline)?,
            &mut attempted,
        );
        let Err(error) = first else {
            return first;
        };
        if error.category == ErrorCategory::Unavailable
            || error.category == ErrorCategory::UnknownOutcome
            || error.code == "remote/stale-endpoint"
        {
            self.invalidate(&endpoint);
        }
        // A server can return any rejection code. Only local write provenance
        // proves non-submission. Never retry a returned/ambiguous transaction.
        if attempted || !matches!(error.code, "remote/unavailable" | "remote/stale-endpoint") {
            return Err(error);
        }
        let replacement = self.discover_until(deadline)?;
        if replacement == endpoint {
            return Err(error);
        }
        let result = self.0.connection.transact_remote_attempt(
            &replacement,
            &self.0.tls,
            &request,
            hints.as_ref(),
            remaining(deadline)?,
            &mut attempted,
        );
        if result.is_err() {
            self.invalidate(&replacement);
        }
        result
    }

    fn invalidate(&self, failed: &RemoteWriterEndpoint) {
        let mut cached = self.0.endpoint.lock().unwrap_or_else(|p| p.into_inner());
        // Another caller may already have installed a newer route.
        if cached.as_ref() == Some(failed) {
            *cached = None;
        }
    }

    fn discover_until(&self, deadline: Instant) -> Result<RemoteWriterEndpoint, SemanticError> {
        let budget = remaining(deadline)?.min(Duration::from_millis(i32::MAX as u64));
        let mut policy = self.0.postgres.io_policy().clone();
        policy.connect_timeout = Some(policy.connect_timeout.map_or(budget, |v| v.min(budget)));
        policy.statement_timeout = Some(policy.statement_timeout.map_or(budget, |v| v.min(budget)));
        policy.lock_timeout = Some(policy.lock_timeout.map_or(budget, |v| v.min(budget)));
        let bounded = self.0.postgres.clone().with_io_policy(policy)?;
        let endpoint = self.0.connection.discover_remote_writer(&bounded)?;
        remaining(deadline)?;
        let mut cached = self.0.endpoint.lock().unwrap_or_else(|p| p.into_inner());
        // Concurrent discoveries can finish out of order. Within one lineage
        // a lower lease epoch must not replace an already discovered successor.
        if cached
            .as_ref()
            .is_none_or(|known| known.lease_epoch() <= endpoint.lease_epoch())
        {
            *cached = Some(endpoint.clone());
        }
        Ok(cached.as_ref().cloned().unwrap_or(endpoint))
    }
}

fn deadline(timeout: Duration) -> Result<Instant, SemanticError> {
    Instant::now()
        .checked_add(timeout)
        .filter(|_| !timeout.is_zero())
        .ok_or_else(|| {
            SemanticError::incorrect(
                "remote/timeout",
                "timeout must be positive and representable",
            )
        })
}

fn remaining(deadline: Instant) -> Result<Duration, SemanticError> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| {
            SemanticError::new(
                ErrorCategory::Unavailable,
                "remote/routing-timeout",
                "remote routing deadline elapsed before submission",
            )
        })
}
