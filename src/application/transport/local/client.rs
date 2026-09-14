use super::*;

impl Connection {
    /// Submit to a separately running same-host writer. Before an authenticated
    /// response, a lost connection/timeout is UnknownOutcome. After success,
    /// local value-opening failures cannot turn that commit into a rejection.
    /// Remote semantic errors preserve category, details, and structured
    /// anomaly; their original code is in `details["remote_code"]`.
    pub fn transact_socket(
        &self,
        endpoint: impl AsRef<Path>,
        request: TransactionRequest,
        timeout: Duration,
    ) -> Result<CommittedTransaction, SemanticError> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .filter(|_| !timeout.is_zero())
            .ok_or_else(|| {
                SemanticError::incorrect(
                    "transport/timeout",
                    "timeout must be positive and representable",
                )
            })?;
        let bytes = encode_submission(self.identity(), &request)?;
        if bytes.len() > MAX_FRAME {
            return Err(SemanticError::incorrect(
                "transport/frame-limit",
                "request exceeds transport frame policy",
            ));
        }
        let socket = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)
            .map_err(unavailable)?;
        let address = socket2::SockAddr::unix(endpoint).map_err(unavailable)?;
        socket
            .connect_timeout(&address, remaining(deadline).map_err(unavailable)?)
            .map_err(unavailable)?;
        let mut stream: UnixStream = socket.into();
        // From the first attempted write onward, delivery/admission may have
        // happened. Never classify an ambiguous transport error as a rollback.
        write_frame(&mut stream, &bytes, deadline).map_err(unknown)?;
        let bytes = read_frame(&mut stream, MAX_FRAME, deadline).map_err(unknown)?;
        let outcome = decode_submission_outcome(&bytes).map_err(|error| {
            SemanticError::new(
                ErrorCategory::UnknownOutcome,
                "transport/invalid-outcome",
                "received no usable authenticated transaction outcome",
            )
            .detail("cause", error.to_string())
        })?;
        match outcome {
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
