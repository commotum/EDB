use super::*;

pub struct LocalTransactionServer {
    pub(super) endpoint: PathBuf,
    pub(super) stop: Arc<AtomicBool>,
    pub(super) worker: Option<JoinHandle<()>>,
    // Drop after the listener/workers; retain the exclusive lock through cleanup.
    pub(super) _owner: EndpointOwner,
}

impl LocalTransactionServer {
    /// Serve an existing fenced writer without owning its lease or lifetime.
    /// Share the returned endpoint only with trusted processes running as the
    /// same OS user. Restarting this adapter creates a new endpoint; reconnect
    /// clients explicitly rather than silently retargeting an old pathname.
    pub fn start(
        client: TransactionClient,
        config: LocalTransportConfig,
    ) -> Result<Self, SemanticError> {
        config.validate()?;
        let directory = tempfile::Builder::new()
            .prefix("atomic-writer-")
            .tempdir()
            .map_err(unavailable)?;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
            .map_err(unavailable)?;
        let endpoint = directory.path().join("writer.sock");
        let (listener, mut owner) = EndpointOwner::bind(&endpoint)?;
        owner._temporary = Some(directory);
        Self::start_bound(client, config, listener, owner)
    }

    /// Serve at a stable same-user endpoint in an existing owned mode-0700
    /// directory. Directory components cannot be symlinks or replaceable by
    /// other users. The socket is mode 0600; `<endpoint>.lock` is a permanent
    /// mode-0600 ownership record and must not be removed or replaced while the
    /// adapter is running. Startup fails if another adapter holds its lock.
    /// After a crash, only its recorded, no-longer-listening socket is reclaimed.
    /// Unexpected files/sockets are rejected without replacement. Dropping the
    /// adapter removes its own socket, leaving the caller's directory and lock.
    /// The writer lease/lifetime remains owned by the caller, as with `start`.
    pub fn start_at(
        client: TransactionClient,
        config: LocalTransportConfig,
        endpoint: impl AsRef<Path>,
    ) -> Result<Self, SemanticError> {
        config.validate()?;
        LocalTransactionEndpoint::bind_at(endpoint)?.start(client, config)
    }

    pub(super) fn start_bound(
        client: TransactionClient,
        config: LocalTransportConfig,
        listener: UnixListener,
        owner: EndpointOwner,
    ) -> Result<Self, SemanticError> {
        let endpoint = owner.endpoint.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let worker = thread::Builder::new()
            .name("atomic-local-listener".into())
            .spawn(move || {
                let mut workers: Vec<JoinHandle<()>> = Vec::new();
                while !stopped.load(Ordering::Acquire) {
                    let mut index = 0;
                    while index < workers.len() {
                        if workers[index].is_finished() {
                            let _ = workers.swap_remove(index).join();
                        } else {
                            index += 1;
                        }
                    }
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            if workers.len() >= config.max_in_flight {
                                let rejection = Err(SemanticError::new(
                                    ErrorCategory::Busy,
                                    "transport/full",
                                    "local transaction endpoint is at its in-flight limit",
                                ));
                                if let Ok(bytes) = encode_submission_outcome(&rejection) {
                                    let _ = write_frame(
                                        &mut stream,
                                        &bytes,
                                        Instant::now() + Duration::from_millis(10),
                                    );
                                }
                                continue;
                            }
                            let client = client.clone();
                            if let Ok(worker) = thread::Builder::new()
                                .name("atomic-local-request".into())
                                .spawn(move || {
                                    let _ = serve(stream, client, config);
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
        Ok(Self {
            endpoint,
            stop,
            worker: Some(worker),
            _owner: owner,
        })
    }

    pub fn endpoint(&self) -> &Path {
        &self.endpoint
    }

    /// Whether the listener worker is still running. This does not check the
    /// writer lease; a daemon must also supervise its transaction service.
    pub fn is_available(&self) -> bool {
        self.worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
    }
}

impl Drop for LocalTransactionServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
pub(super) fn serve(
    mut stream: UnixStream,
    client: TransactionClient,
    config: LocalTransportConfig,
) -> io::Result<()> {
    let deadline = Instant::now() + config.request_timeout;
    let bytes = read_frame(&mut stream, config.max_frame_bytes, deadline)?;
    let result = decode_submission(&bytes).and_then(|(identity, request)| {
        if identity != client.identity() {
            return Err(SemanticError::new(
                ErrorCategory::Conflict,
                "transport/database-identity",
                "endpoint and peer do not address the same database lineage",
            ));
        }
        client.transact(request, remaining(deadline).map_err(unavailable)?)
    });
    let bytes = encode_submission_outcome(&result).map_err(io::Error::other)?;
    if bytes.len() > config.max_frame_bytes {
        return Err(io::Error::other("response exceeds configured frame policy"));
    }
    write_frame(&mut stream, &bytes, deadline)?;
    Ok(())
}
