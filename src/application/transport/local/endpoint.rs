use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FileIdentity {
    pub(super) device: u64,
    pub(super) inode: u64,
}

impl FileIdentity {
    pub(super) fn of(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }

    pub(super) fn matches(self, path: &Path) -> bool {
        std::fs::symlink_metadata(path).is_ok_and(|metadata| Self::of(&metadata) == self)
    }
}

/// The lock pathname is permanent: unlinking a locked file would let another
/// process lock a different inode at the same name. The record identifies the
/// only stale socket this adapter is allowed to reclaim after a process crash.
pub(super) struct EndpointOwner {
    pub(super) endpoint: PathBuf,
    pub(super) directory: PathBuf,
    pub(super) directory_identity: FileIdentity,
    pub(super) lock_path: PathBuf,
    pub(super) lock: File,
    pub(super) lock_identity: FileIdentity,
    pub(super) socket_identity: Option<FileIdentity>,
    pub(super) _temporary: Option<tempfile::TempDir>,
}

impl EndpointOwner {
    pub(super) fn bind(endpoint: &Path) -> Result<(UnixListener, Self), SemanticError> {
        let endpoint = if endpoint.is_absolute() {
            endpoint.to_owned()
        } else {
            std::env::current_dir().map_err(unavailable)?.join(endpoint)
        };
        if endpoint
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
        {
            return Err(unsafe_endpoint(
                "endpoint must not contain . or .. components",
            ));
        }
        let name = endpoint
            .file_name()
            .ok_or_else(|| unsafe_endpoint("endpoint must name a socket in a private directory"))?;
        let directory = endpoint
            .parent()
            .ok_or_else(|| unsafe_endpoint("endpoint requires a private parent directory"))?
            .to_owned();
        // SAFETY: geteuid takes no arguments and has no memory-safety preconditions.
        let uid = unsafe { libc::geteuid() };
        let mut prefix = PathBuf::new();
        for component in directory.components() {
            prefix.push(component.as_os_str());
            let metadata = std::fs::symlink_metadata(&prefix).map_err(unavailable)?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(unsafe_endpoint(
                    "endpoint directory components must not be symlinks",
                ));
            }
            // A root-owned sticky /tmp is safe for an owned private child.
            // Untrusted owners or non-sticky writable ancestors can replace it.
            if (metadata.uid() != uid && metadata.uid() != 0)
                || (metadata.mode() & 0o022 != 0 && metadata.mode() & 0o1000 == 0)
            {
                return Err(unsafe_endpoint(
                    "endpoint has an unsafe writable directory ancestor",
                ));
            }
        }
        let directory_file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(&directory)
            .map_err(unavailable)?;
        let metadata = directory_file.metadata().map_err(unavailable)?;
        if metadata.uid() != uid || metadata.mode() & 0o7777 != 0o700 {
            return Err(unsafe_endpoint(
                "endpoint parent must be owned by this user with mode 0700",
            ));
        }
        let directory_identity = FileIdentity::of(&metadata);
        if !directory_identity.matches(&directory) {
            return Err(unsafe_endpoint("endpoint directory changed during startup"));
        }
        let mut lock_name = name.to_owned();
        lock_name.push(".lock");
        let lock_path = directory.join(lock_name);
        // Publish an initialized, already-locked file atomically. Publishing an
        // empty file before taking its lock lets a simultaneous first start
        // mistake the initialization window for a broken ownership record.
        let mut candidate = tempfile::Builder::new()
            .prefix(".atomic-endpoint-lock-")
            .tempfile_in(&directory)
            .map_err(unavailable)?;
        candidate
            .write_all(ENDPOINT_LOCK_HEADER.as_bytes())
            .map_err(unavailable)?;
        candidate.as_file().sync_data().map_err(unavailable)?;
        candidate
            .as_file()
            .try_lock()
            .map_err(|error| unavailable(error.into()))?;
        let (mut lock, already_locked) = match candidate.persist_noclobber(&lock_path) {
            Ok(file) => (file, true),
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                drop(error);
                let metadata = std::fs::symlink_metadata(&lock_path).map_err(unavailable)?;
                if !metadata.is_file() || metadata.file_type().is_symlink() {
                    return Err(unsafe_endpoint(
                        "endpoint lock must be a regular, non-symlink file",
                    ));
                }
                let lock = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                    .open(&lock_path)
                    .map_err(unavailable)?;
                (lock, false)
            }
            Err(error) => return Err(unavailable(error.error)),
        };
        let metadata = lock.metadata().map_err(unavailable)?;
        if !metadata.is_file()
            || metadata.uid() != uid
            || metadata.mode() & 0o7777 != 0o600
            || metadata.nlink() != 1
        {
            return Err(unsafe_endpoint(
                "endpoint lock must be an owned, unlinked-elsewhere mode-0600 file",
            ));
        }
        let lock_identity = FileIdentity::of(&metadata);
        if !lock_identity.matches(&lock_path) || !directory_identity.matches(&directory) {
            return Err(unsafe_endpoint(
                "endpoint ownership paths changed during startup",
            ));
        }
        if !already_locked {
            match lock.try_lock() {
                Ok(()) => {}
                Err(TryLockError::WouldBlock) => return Err(endpoint_busy()),
                Err(TryLockError::Error(error)) => return Err(unavailable(error)),
            }
        }
        let mut record = String::new();
        lock.rewind().map_err(unavailable)?;
        (&mut lock)
            .take(129)
            .read_to_string(&mut record)
            .map_err(unavailable)?;
        let record = record
            .strip_prefix(ENDPOINT_LOCK_HEADER)
            .filter(|record| record.len() <= 100)
            .ok_or_else(|| {
                unsafe_endpoint("endpoint lock is not a recognized Atomic ownership record")
            })?;
        let previous = if record.is_empty() {
            None
        } else {
            let mut fields = record.split_whitespace();
            let device = fields.next().and_then(|value| value.parse().ok());
            let inode = fields.next().and_then(|value| value.parse().ok());
            match (device, inode, fields.next()) {
                (Some(device), Some(inode), None) => Some(FileIdentity { device, inode }),
                _ => {
                    return Err(unsafe_endpoint(
                        "endpoint lock has an invalid socket identity",
                    ));
                }
            }
        };
        let mut owner = Self {
            endpoint,
            directory,
            directory_identity,
            lock_path,
            lock,
            lock_identity,
            socket_identity: None,
            _temporary: None,
        };
        match std::fs::symlink_metadata(&owner.endpoint) {
            Ok(metadata) => {
                let identity = FileIdentity::of(&metadata);
                if !metadata.file_type().is_socket()
                    || metadata.uid() != uid
                    || metadata.mode() & 0o7777 != 0o600
                    || Some(identity) != previous
                {
                    return Err(unsafe_endpoint(
                        "existing endpoint is not the recorded Atomic socket",
                    ));
                }
                let socket =
                    socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)
                        .map_err(unavailable)?;
                let address = socket2::SockAddr::unix(&owner.endpoint).map_err(unavailable)?;
                match socket.connect_timeout(&address, Duration::from_millis(100)) {
                    Ok(()) => return Err(endpoint_busy()),
                    Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {}
                    Err(error) => return Err(unavailable(error)),
                }
                owner.ensure_paths()?;
                if !identity.matches(&owner.endpoint) {
                    return Err(unsafe_endpoint("stale endpoint changed during startup"));
                }
                std::fs::remove_file(&owner.endpoint).map_err(unavailable)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(unavailable(error)),
        }
        owner.ensure_paths()?;
        let listener = UnixListener::bind(&owner.endpoint).map_err(unavailable)?;
        let metadata = std::fs::symlink_metadata(&owner.endpoint).map_err(unavailable)?;
        if !metadata.file_type().is_socket() || metadata.uid() != uid {
            return Err(unsafe_endpoint("bound endpoint changed during startup"));
        }
        owner.socket_identity = Some(FileIdentity::of(&metadata));
        owner.ensure_paths()?;
        std::fs::set_permissions(&owner.endpoint, std::fs::Permissions::from_mode(0o600))
            .map_err(unavailable)?;
        listener.set_nonblocking(true).map_err(unavailable)?;
        let identity = owner.socket_identity.expect("bound socket identity");
        let record = format!(
            "{ENDPOINT_LOCK_HEADER}{} {}\n",
            identity.device, identity.inode
        );
        owner.lock.rewind().map_err(unavailable)?;
        owner
            .lock
            .write_all(record.as_bytes())
            .map_err(unavailable)?;
        owner
            .lock
            .set_len(record.len() as u64)
            .map_err(unavailable)?;
        owner.lock.sync_data().map_err(unavailable)?;
        Ok((listener, owner))
    }

    pub(super) fn ensure_paths(&self) -> Result<(), SemanticError> {
        if self.directory_identity.matches(&self.directory)
            && self.lock_identity.matches(&self.lock_path)
        {
            Ok(())
        } else {
            Err(unsafe_endpoint("endpoint ownership paths changed"))
        }
    }
}

impl Drop for EndpointOwner {
    fn drop(&mut self) {
        // Cooperative instances cannot replace these paths while the file lock
        // is held. If a caller nevertheless replaces a path, leave it alone.
        if self.ensure_paths().is_ok()
            && self.socket_identity.is_some_and(|identity| {
                std::fs::symlink_metadata(&self.endpoint).is_ok_and(|metadata| {
                    metadata.file_type().is_socket() && FileIdentity::of(&metadata) == identity
                })
            })
        {
            let _ = std::fs::remove_file(&self.endpoint);
        }
        // The lock closes after cleanup; its permanent pathname is never removed.
    }
}

pub(super) fn unsafe_endpoint(message: &'static str) -> SemanticError {
    SemanticError::incorrect("transport/unsafe-endpoint", message)
}

pub(super) fn endpoint_busy() -> SemanticError {
    SemanticError::new(
        ErrorCategory::Busy,
        "transport/endpoint-in-use",
        "local endpoint is already in use",
    )
}

/// A stable endpoint reserved before starting its writer service. Binding
/// performs the same ownership checks as [`LocalTransactionServer::start_at`]
/// and holds the listener and exclusive lock until it is started or dropped.
/// No requests are processed before `start`; dropping an unused endpoint
/// removes only its own socket, retaining the caller's directory and lock file.
pub struct LocalTransactionEndpoint {
    pub(super) listener: UnixListener,
    pub(super) owner: EndpointOwner,
}

impl LocalTransactionEndpoint {
    pub fn bind_at(endpoint: impl AsRef<Path>) -> Result<Self, SemanticError> {
        let (listener, owner) = EndpointOwner::bind(endpoint.as_ref())?;
        Ok(Self { listener, owner })
    }

    pub fn endpoint(&self) -> &Path {
        &self.owner.endpoint
    }

    pub fn start(
        self,
        client: TransactionClient,
        config: LocalTransportConfig,
    ) -> Result<LocalTransactionServer, SemanticError> {
        config.validate()?;
        self.owner.ensure_paths()?;
        if !self.owner.socket_identity.is_some_and(|identity| {
            std::fs::symlink_metadata(&self.owner.endpoint).is_ok_and(|metadata| {
                metadata.file_type().is_socket() && FileIdentity::of(&metadata) == identity
            })
        }) {
            return Err(unsafe_endpoint("prepared endpoint changed before startup"));
        }
        LocalTransactionServer::start_bound(client, config, self.listener, self.owner)
    }
}
