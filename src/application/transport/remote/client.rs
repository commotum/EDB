use super::*;

impl Connection {
    pub fn discover_remote_writer(
        &self,
        connection: &PostgresConnectionConfig,
    ) -> Result<RemoteWriterEndpoint, SemanticError> {
        // Retirement is a permanent route decision, not a temporarily absent
        // transactor. Check it before the endpoint tombstone and never follow
        // a reused human name to a different database.
        let entry = crate::DatabaseCatalog::connect_configured(connection)?
            .require_active_id(self.identity().database_id())?;
        if entry.lineage_id != self.identity().lineage_id() {
            return Err(SemanticError::conflict(
                "remote/database-identity",
                "Captured route no longer names the expected lineage",
            ));
        }
        let mut store = PgBlockStore::connect(connection)?;
        let value = store
            .read_ref(&endpoint_key(self.identity()))?
            .and_then(|reference| reference.value)
            .ok_or_else(no_writer)?;
        let endpoint = decode_endpoint(&value)?;
        if endpoint.identity != *self.identity() {
            return Err(invalid(
                "remote/discovery",
                "Endpoint identity differs from its captured route",
            ));
        }
        crate::transactor::authority::writer_endpoint_guards(
            &mut store,
            self.identity(),
            endpoint.lease_epoch,
        )
        .map_err(|error| {
            if matches!(
                error.category,
                ErrorCategory::Conflict | ErrorCategory::NotFound | ErrorCategory::Unavailable
            ) {
                no_writer()
            } else {
                error
            }
        })?;
        Ok(endpoint)
    }
    pub fn transact_remote(
        &self,
        endpoint: &RemoteWriterEndpoint,
        config: &RemoteClientConfig,
        request: TransactionRequest,
        timeout: Duration,
    ) -> Result<CommittedTransaction, SemanticError> {
        self.transact_remote_inner(endpoint, config, request, None, timeout)
    }
    pub fn transact_remote_with_hints(
        &self,
        endpoint: &RemoteWriterEndpoint,
        config: &RemoteClientConfig,
        request: TransactionRequest,
        hints: TransactionHints,
        timeout: Duration,
    ) -> Result<CommittedTransaction, SemanticError> {
        self.transact_remote_inner(endpoint, config, request, Some(hints), timeout)
    }
    fn transact_remote_inner(
        &self,
        endpoint: &RemoteWriterEndpoint,
        config: &RemoteClientConfig,
        request: TransactionRequest,
        hints: Option<TransactionHints>,
        timeout: Duration,
    ) -> Result<CommittedTransaction, SemanticError> {
        self.transact_remote_attempt(
            endpoint,
            config,
            &request,
            hints.as_ref(),
            timeout,
            &mut false,
        )
    }

    /// The routing facade may refresh/retry only before any request write.
    /// Keep that provenance out of server-controlled error codes.
    pub(crate) fn transact_remote_attempt(
        &self,
        endpoint: &RemoteWriterEndpoint,
        config: &RemoteClientConfig,
        request: &TransactionRequest,
        hints: Option<&TransactionHints>,
        timeout: Duration,
        attempted: &mut bool,
    ) -> Result<CommittedTransaction, SemanticError> {
        *attempted = false;
        let deadline = Instant::now()
            .checked_add(timeout)
            .filter(|_| !timeout.is_zero())
            .ok_or_else(|| {
                invalid(
                    "remote/timeout",
                    "timeout must be positive and representable",
                )
            })?;
        if endpoint.identity != *self.identity() {
            return Err(SemanticError::conflict(
                "remote/database-identity",
                "endpoint and peer address different database lineages",
            ));
        }
        let request = encode_submission(self.identity(), request)?;
        if request.len() > MAX_FRAME {
            return Err(invalid(
                "remote/frame-limit",
                "request exceeds remote frame policy",
            ));
        }
        let hints = hints
            .and_then(|hints| hints::encode(hints).ok())
            .unwrap_or_default();
        let socket = TcpStream::connect_timeout(
            &endpoint.address,
            remaining(deadline).map_err(unavailable)?,
        )
        .map_err(unavailable)?;
        socket.set_nodelay(true).map_err(unavailable)?;
        let mut tls = config
            .connector
            .connect(
                &endpoint.tls_server_name,
                DeadlineStream { socket, deadline },
            )
            .map_err(|_| {
                SemanticError::new(
                    ErrorCategory::Unavailable,
                    "remote/tls",
                    "TLS server authentication or handshake failed",
                )
            })?;
        let hello = read_frame(&mut tls, MAX_HELLO).map_err(unavailable)?;
        validate_hello(&hello, endpoint)?;
        // No request has been attempted until the server's lineage/fence and
        // instance have been checked through verified TLS.
        tls.write_all(config.token.0.as_ref())
            .map_err(unavailable)?;
        let mut authorized = [0];
        tls.read_exact(&mut authorized).map_err(unavailable)?;
        if authorized != [1] {
            return Err(SemanticError::new(
                ErrorCategory::Forbidden,
                "remote/authentication",
                "remote database authentication failed",
            ));
        }
        *attempted = true;
        write_frame(&mut tls, &request).map_err(unknown)?;
        write_frame(&mut tls, &hints).map_err(unknown)?;
        let outcome = read_frame(&mut tls, MAX_FRAME).map_err(unknown)?;
        match decode_submission_outcome(&outcome).map_err(|_| {
            SemanticError::new(
                ErrorCategory::UnknownOutcome,
                "remote/invalid-outcome",
                "received no usable authenticated transaction outcome",
            )
        })? {
            WireOutcome::Rejected(error) => Err(error),
            WireOutcome::Committed(wire) => {
                let basis_t = wire.after.basis_t;
                let tx_hash = wire.after.tx_hash;
                let replayed = wire.replayed;
                let report = self.open_socket_report(*wire);
                Ok(CommittedTransaction {
                    basis_t,
                    tx_hash,
                    replayed,
                    report,
                })
            }
        }
    }
}
