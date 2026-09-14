use super::*;

pub struct RemoteTransactionEndpoint {
    pub(super) listener: TcpListener,
    pub(super) acceptor: TlsAcceptor,
}
impl RemoteTransactionEndpoint {
    pub fn bind(address: SocketAddr, identity: Identity) -> Result<Self, SemanticError> {
        let mut builder = TlsAcceptor::builder(identity);
        builder.min_protocol_version(Some(Protocol::Tlsv12));
        let acceptor = builder.build().map_err(tls_error)?;
        let listener = TcpListener::bind(address).map_err(unavailable)?;
        listener.set_nonblocking(true).map_err(unavailable)?;
        Ok(Self { listener, acceptor })
    }
    pub fn local_addr(&self) -> Result<SocketAddr, SemanticError> {
        self.listener.local_addr().map_err(unavailable)
    }
    pub fn start(
        self,
        client: TransactionClient,
        config: RemoteTransportConfig,
        token: RemoteAuthToken,
    ) -> Result<RemoteTransactionServer, SemanticError> {
        config.validate()?;
        if !client.is_available() {
            return Err(SemanticError::new(
                ErrorCategory::Unavailable,
                "remote/writer-unavailable",
                "writer is not accepting submissions",
            ));
        }
        let lease = client.transport_lease();
        let identity = client.identity();
        let mut instance_id = [0; 32];
        rand::rngs::SysRng
            .try_fill_bytes(&mut instance_id)
            .map_err(|_| {
                SemanticError::new(
                    ErrorCategory::Unavailable,
                    "remote/randomness",
                    "operating system randomness is unavailable",
                )
            })?;
        let hello = encode_hello(&identity, &lease, instance_id)?;
        let address = self.local_addr()?;
        let stats = Arc::new(Counters::default());
        let worker_stats = Arc::clone(&stats);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let server_client = client.clone();
        let worker = thread::Builder::new()
            .name("atomic-remote-listener".into())
            .spawn(move || {
                let mut workers: Vec<JoinHandle<()>> = Vec::new();
                while !stopped.load(Ordering::Acquire) {
                    workers.retain_mut(|worker| !worker.is_finished());
                    match self.listener.accept() {
                        Ok((socket, _)) => {
                            if workers.len() >= config.max_in_flight {
                                worker_stats.rejected_full.fetch_add(1, Ordering::Relaxed);
                                continue;
                            }
                            let client = server_client.clone();
                            let acceptor = self.acceptor.clone();
                            let hello = hello.clone();
                            let token = token.clone();
                            let stats = Arc::clone(&worker_stats);
                            if let Ok(worker) = thread::Builder::new()
                                .name("atomic-remote-request".into())
                                .spawn(move || {
                                    let active = stats.active.fetch_add(1, Ordering::Relaxed) + 1;
                                    stats.max_active.fetch_max(active, Ordering::Relaxed);
                                    let _active = Active(Arc::clone(&stats));
                                    let _ = serve(
                                        socket, &acceptor, client, config, &token, &hello, &stats,
                                    );
                                })
                            {
                                workers.push(worker);
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5))
                        }
                        Err(_) => break,
                    }
                }
                for worker in workers {
                    let _ = worker.join();
                }
            })
            .map_err(unavailable)?;
        Ok(RemoteTransactionServer {
            address,
            identity,
            lease,
            instance_id,
            client,
            stop,
            worker: Some(worker),
            stats,
        })
    }
}
#[derive(Default)]
pub(super) struct Counters {
    pub(super) active: AtomicUsize,
    pub(super) max_active: AtomicUsize,
    pub(super) rejected_full: AtomicU64,
    pub(super) auth_rejected: AtomicU64,
    pub(super) ignored_hints: AtomicU64,
    pub(super) lose_next_response: AtomicBool,
}
pub(super) struct Active(Arc<Counters>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::Relaxed);
    }
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RemoteTransportStats {
    pub active: usize,
    pub max_active: usize,
    pub rejected_full: u64,
    pub auth_rejected: u64,
    pub ignored_hints: u64,
}
pub struct RemoteTransactionServer {
    pub(super) address: SocketAddr,
    pub(super) identity: DatabaseIdentity,
    pub(super) lease: TransactorLease,
    pub(super) instance_id: Digest,
    pub(super) client: TransactionClient,
    pub(super) stop: Arc<AtomicBool>,
    pub(super) worker: Option<JoinHandle<()>>,
    pub(super) stats: Arc<Counters>,
}
impl RemoteTransactionServer {
    /// Deterministic fault-injection seam: next confirmed commit loses its
    /// response. The transaction stays durable and must use identical-key retry.
    #[doc(hidden)]
    pub fn inject_lost_next_committed_response(&self) {
        self.stats.lose_next_response.store(true, Ordering::Release);
    }
    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }
    pub fn is_available(&self) -> bool {
        self.client.is_available()
            && self
                .worker
                .as_ref()
                .is_some_and(|worker| !worker.is_finished())
    }
    pub fn stats(&self) -> RemoteTransportStats {
        RemoteTransportStats {
            active: self.stats.active.load(Ordering::Relaxed),
            max_active: self.stats.max_active.load(Ordering::Relaxed),
            rejected_full: self.stats.rejected_full.load(Ordering::Relaxed),
            auth_rejected: self.stats.auth_rejected.load(Ordering::Relaxed),
            ignored_hints: self.stats.ignored_hints.load(Ordering::Relaxed),
        }
    }
    pub fn publish(
        &self,
        connection: &PostgresConnectionConfig,
        address: SocketAddr,
        tls_server_name: &str,
    ) -> Result<RemoteWriterEndpoint, SemanticError> {
        validate_route(address, tls_server_name)?;
        if !self.is_available() {
            return Err(SemanticError::new(
                ErrorCategory::Unavailable,
                "remote/writer-unavailable",
                "writer endpoint is not available",
            ));
        }
        let endpoint = RemoteWriterEndpoint {
            identity: self.identity.clone(),
            address,
            tls_server_name: tls_server_name.into(),
            holder_id: self.lease.holder_id.clone(),
            lease_epoch: self.lease.epoch,
            instance_id: self.instance_id,
        };
        let encoded = encode_endpoint(&endpoint)?;
        let key = endpoint_key(&self.identity);
        let mut store = PgBlockStore::connect(connection)?;
        for _ in 0..3 {
            let mut guards = crate::transactor::authority::writer_endpoint_guards(
                &mut store,
                &self.identity,
                self.lease.epoch,
            )?;
            let previous = store.read_ref(&key)?;
            guards.push(RefCondition {
                key: key.clone(),
                expected: previous.as_ref().map(|reference| reference.revision),
            });
            match store.compare_exchange_many(
                &guards,
                &[RefChange {
                    key: key.clone(),
                    value: Some(encoded.clone()),
                }],
            )? {
                BatchOutcome::Applied(_) => return Ok(endpoint),
                BatchOutcome::Conflict(_) => std::thread::yield_now(),
            }
        }
        Err(SemanticError::conflict(
            "remote/registration-conflict",
            "Writer authority changed during endpoint registration",
        ))
    }
}
impl Drop for RemoteTransactionServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub(super) fn serve(
    socket: TcpStream,
    acceptor: &TlsAcceptor,
    client: TransactionClient,
    config: RemoteTransportConfig,
    token: &RemoteAuthToken,
    hello: &[u8],
    stats: &Counters,
) -> io::Result<()> {
    let deadline = Instant::now() + config.request_timeout;
    let mut tls = acceptor
        .accept(DeadlineStream { socket, deadline })
        .map_err(|_| io::Error::other("TLS handshake failed"))?;
    write_frame(&mut tls, hello)?;
    let mut received = [0; 32];
    tls.read_exact(&mut received)?;
    if !openssl::memcmp::eq(&received, token.0.as_ref()) {
        stats.auth_rejected.fetch_add(1, Ordering::Relaxed);
        tls.write_all(&[0])?;
        return Ok(());
    }
    tls.write_all(&[1])?;
    let request = read_frame(&mut tls, config.max_frame_bytes)?;
    // Wire framing has one fixed hard bound; the deployment's smaller hint
    // allowance controls advisory admission, not transaction rejection.
    let hint_bytes = read_frame(&mut tls, hints::MAX_WIRE_BYTES)?;
    let result = decode_submission(&request).and_then(|(identity, request)| {
        if identity != client.identity() {
            return Err(SemanticError::conflict(
                "remote/database-identity",
                "endpoint and peer address different database lineages",
            ));
        }
        let hints = if hint_bytes.len() > config.max_hint_bytes {
            stats.ignored_hints.fetch_add(1, Ordering::Relaxed);
            None
        } else if hint_bytes.is_empty() {
            None
        } else {
            match hints::decode(&hint_bytes) {
                Ok(hints) => Some(hints),
                Err(_) => {
                    stats.ignored_hints.fetch_add(1, Ordering::Relaxed);
                    None
                }
            }
        };
        let timeout = remaining(deadline).map_err(unavailable)?;
        if let Some(hints) = hints {
            client
                .submit_with_hints(request, hints, crate::HintPrefetchOptions::default())?
                .0
                .wait(timeout)
        } else {
            client.transact(request, timeout)
        }
    });
    let outcome = encode_submission_outcome(&result).map_err(io::Error::other)?;
    if result.is_ok() && stats.lose_next_response.swap(false, Ordering::AcqRel) {
        return Err(io::Error::other("injected lost committed response"));
    }
    if outcome.len() > config.max_frame_bytes {
        return Err(io::Error::other(
            "response exceeds configured remote frame policy",
        ));
    }
    write_frame(&mut tls, &outcome)?;
    Ok(())
}
