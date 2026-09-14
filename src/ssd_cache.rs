//! Optional disposable immutable-block cache. A namespace separates access,
//! lineage, generation and format domains; it grants no database authorization.
//! Callers authenticate/open the database and establish retention independently.
use crate::storage::codec::{MAX_PHYSICAL_BLOCK_BYTES, decode_block, encode_block};
use crate::{Digest, ErrorCategory, SemanticError};
use std::fs::{File, FileTimes, Metadata, OpenOptions};
use std::io::{Read, Seek, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

const LOCK_NAME: &str = ".atomic-ssd-cache-v1.lock";
const STAGE_NAME: &str = ".atomic-ssd-cache-v1.stage";
const ENTRY_MAGIC: &[u8] = b"ATOMIC-SSD-BLOCK\x01";
const ENTRY_HEADER: usize = ENTRY_MAGIC.len() + 72;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SsdCacheLimits {
    pub max_entries: usize,
    /// Committed block-file lengths including entry framing; excludes the
    /// fixed ownership record and is not filesystem allocation. Admission
    /// reserves space for its one staging block before writing it.
    pub max_bytes: u64,
}

impl Default for SsdCacheLimits {
    fn default() -> Self {
        Self {
            max_entries: 4096,
            max_bytes: 1024 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SsdCacheConfig {
    pub directory: PathBuf,
    pub limits: SsdCacheLimits,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SsdCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub puts: u64,
    pub evictions: u64,
    pub purged_entries: u64,
    pub corruptions: u64,
    /// Framing failures preserved in place, including degraded open/put/purge.
    pub corrupt_entry_bypasses: u64,
    pub io_errors: u64,
    pub busy_bypasses: u64,
    pub oversized_bypasses: u64,
    pub disabled_bypasses: u64,
    pub physical_read_bytes: u64,
    pub physical_write_bytes: u64,
    pub canonical_read_bytes: u64,
    pub canonical_write_bytes: u64,
    /// Last observed shared-directory occupancy, including all namespaces.
    /// Recognized malformed files are included in these entry/byte counts.
    pub current_entries: usize,
    pub current_bytes: u64,
    /// False until an inventory succeeds without malformed recognized files.
    /// Incomplete inventory never authorizes cache writes or eviction/purge.
    pub inventory_complete: bool,
    pub peak_entries: usize,
    pub peak_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Identity {
    device: u64,
    inode: u64,
}
impl Identity {
    fn of(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
    fn matches(self, path: &Path) -> bool {
        std::fs::symlink_metadata(path).is_ok_and(|metadata| Self::of(&metadata) == self)
    }
}

struct Directory {
    path: PathBuf,
    identity: Identity,
    lock: File,
    lock_identity: Identity,
    limits: SsdCacheLimits,
    uid: u32,
}

struct State {
    directory: Option<Directory>,
    stats: SsdCacheStats,
}

/// Clones and namespace views share one directory/accounting handle. Separate
/// processes coordinate each disk operation with the permanent lock file.
#[derive(Clone)]
pub struct SsdCache {
    namespace: Digest,
    state: Arc<Mutex<State>>,
}

/// Open the immutable block cache using the connection's access namespace.
pub(crate) fn open_connection_cache(
    connection: &crate::PostgresConnectionConfig,
    identity: &str,
) -> Result<(SsdCache, Digest), crate::SemanticError> {
    let namespace = connection.ssd_access_namespace(identity);
    let cache = match connection.ssd_cache_config() {
        Some(config) => SsdCache::open(&config.directory, namespace, config.limits)?,
        None => SsdCache::disabled(),
    };
    Ok((cache, namespace))
}

impl SsdCache {
    pub fn disabled() -> Self {
        Self {
            namespace: [0; 32],
            state: Arc::new(Mutex::new(State {
                directory: None,
                stats: SsdCacheStats::default(),
            })),
        }
    }

    pub fn open(
        directory: impl AsRef<Path>,
        namespace: Digest,
        limits: SsdCacheLimits,
    ) -> Result<Self, SemanticError> {
        let directory = Directory::open(directory.as_ref(), limits)?;
        let mut stats = SsdCacheStats::default();
        match DiskLock::acquire(&directory.lock) {
            Ok(_guard) => {
                directory.recover_stage()?;
                let inventory = directory.inventory()?;
                inventory.observe(&mut stats);
                if !stats.inventory_complete {
                    record_disk_error(
                        &mut stats,
                        &corrupt_entry("cache inventory contains malformed entries"),
                    );
                } else if stats.current_bytes > limits.max_bytes {
                    return Err(unsafe_cache(
                        "existing cache exceeds its configured byte limit",
                    ));
                }
            }
            Err(error) if error.category == ErrorCategory::Busy => {
                // Another cooperative peer is active. Its immutable lock record
                // already validated the same limits; retry disk work on demand.
                stats.busy_bypasses += 1;
            }
            Err(error) => return Err(error),
        }
        Ok(Self {
            namespace,
            state: Arc::new(Mutex::new(State {
                directory: Some(directory),
                stats,
            })),
        })
    }

    pub fn for_namespace(&self, namespace: Digest) -> Self {
        Self {
            namespace,
            state: Arc::clone(&self.state),
        }
    }
    pub fn is_enabled(&self) -> bool {
        lock(&self.state).directory.is_some()
    }
    pub fn stats(&self) -> SsdCacheStats {
        lock(&self.state).stats
    }

    /// Return only bounded, hash-authenticated canonical bytes. The caller's
    /// native decoder remains responsible for canonical grammar validation.
    pub fn get(&self, hash: &Digest) -> Option<Vec<u8>> {
        let observed = crate::io_diagnostics::CacheObservation::start(crate::CacheTier::LocalDisk);
        let mut state = lock(&self.state);
        let State { directory, stats } = &mut *state;
        let Some(directory) = directory else {
            stats.disabled_bypasses += 1;
            return None;
        };
        let mut physical_bytes = 0;
        let mut corrupt = false;
        let _guard = match DiskLock::acquire(&directory.lock) {
            Ok(guard) => guard,
            Err(error) => {
                record_disk_error(stats, &error);
                stats.misses += 1;
                observed.finish(false, true, 0, 0);
                return None;
            }
        };
        let loaded = (|| {
            directory.ensure_paths()?;
            let path = directory.path.join(entry_name(&self.namespace, hash));
            let Some(entry) = directory.inspect(&path, Some((self.namespace, *hash)))? else {
                return Ok(None);
            };
            let mut file = open_read(&path)?;
            if Identity::of(&file.metadata().map_err(io_error)?) != entry.identity {
                return Err(unsafe_cache("cache entry changed during read"));
            }
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(entry.bytes as usize)
                .map_err(|_| unsafe_cache("bounded cache read allocation failed"))?;
            bytes.resize(entry.bytes as usize, 0);
            file.read_exact(&mut bytes).map_err(io_error)?;
            let mut extra = [0u8; 1];
            if file.read(&mut extra).map_err(io_error)? != 0 {
                return Err(unsafe_cache("cache entry grew during read"));
            }
            stats.physical_read_bytes =
                stats.physical_read_bytes.saturating_add(bytes.len() as u64);
            physical_bytes = bytes.len() as u64;
            let canonical = parse_header(bytes[..ENTRY_HEADER].try_into().unwrap()).and_then(
                |(namespace, actual_hash, physical_bytes)| {
                    if namespace != self.namespace
                        || &actual_hash != hash
                        || physical_bytes != (bytes.len() - ENTRY_HEADER) as u64
                    {
                        return Err(unsafe_cache(
                            "cache entry changed identity or length during read",
                        ));
                    }
                    decode_block(hash, &bytes[ENTRY_HEADER..])
                },
            );
            match canonical {
                Ok(canonical) => {
                    let _ = file.set_times(FileTimes::new().set_modified(SystemTime::now()));
                    stats.canonical_read_bytes = stats
                        .canonical_read_bytes
                        .saturating_add(canonical.len() as u64);
                    Ok(Some(canonical))
                }
                Err(_) => {
                    corrupt = true;
                    stats.corruptions += 1;
                    directory.remove(&entry)?;
                    stats.current_entries = stats.current_entries.saturating_sub(1);
                    stats.current_bytes = stats.current_bytes.saturating_sub(entry.bytes);
                    Ok(None)
                }
            }
        })();
        match loaded {
            Ok(Some(bytes)) => {
                stats.hits += 1;
                observed.finish(true, false, physical_bytes, bytes.len() as u64);
                Some(bytes)
            }
            Ok(None) => {
                stats.misses += 1;
                observed.finish(false, corrupt, physical_bytes, 0);
                None
            }
            Err(error) => {
                record_disk_error(stats, &error);
                stats.misses += 1;
                observed.finish(false, true, physical_bytes, 0);
                None
            }
        }
    }

    /// Admit only canonical bytes matching the supplied immutable identity.
    /// Runtime disk failures do not become database failures.
    pub fn put(&self, hash: &Digest, canonical: &[u8]) -> bool {
        if !self.is_enabled() {
            lock(&self.state).stats.disabled_bypasses += 1;
            return false;
        }
        let physical = match decode_block(hash, canonical).and_then(|decoded| {
            if decoded != canonical {
                return Err(unsafe_cache("put requires canonical, not enveloped, bytes"));
            }
            encode_block(canonical)
        }) {
            Ok(bytes) => bytes,
            Err(_) => {
                lock(&self.state).stats.corruptions += 1;
                return false;
            }
        };
        let mut state = lock(&self.state);
        let State { directory, stats } = &mut *state;
        let Some(directory) = directory else {
            stats.disabled_bypasses += 1;
            return false;
        };
        let bytes = (ENTRY_HEADER + physical.len()) as u64;
        if bytes > directory.limits.max_bytes {
            stats.oversized_bypasses += 1;
            return false;
        }
        let _guard = match DiskLock::acquire(&directory.lock) {
            Ok(guard) => guard,
            Err(error) => {
                record_disk_error(stats, &error);
                return false;
            }
        };
        let result = (|| {
            directory.ensure_paths()?;
            directory.recover_stage()?;
            let inventory = directory.inventory()?;
            inventory.observe(stats);
            let mut entries = inventory.intact_entries()?;
            let path = directory.path.join(entry_name(&self.namespace, hash));
            if let Some(index) = entries.iter().position(|entry| entry.path == path) {
                directory.remove(&entries.remove(index))?;
            }
            entries.sort_by_key(|entry| (entry.modified, entry.path.clone()));
            let mut occupied: u64 = entries.iter().map(|entry| entry.bytes).sum();
            while entries.len() >= directory.limits.max_entries
                || occupied.saturating_add(bytes) > directory.limits.max_bytes
            {
                let victim = entries.remove(0);
                directory.remove(&victim)?;
                occupied -= victim.bytes;
                stats.evictions += 1;
            }
            let stage_path = directory.path.join(STAGE_NAME);
            let mut stage = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&stage_path)
                .map_err(io_error)?;
            let stage_identity = Identity::of(&stage.metadata().map_err(io_error)?);
            let cleanup = StageCleanup {
                directory,
                path: stage_path.clone(),
                identity: stage_identity,
            };
            let mut header = Vec::with_capacity(ENTRY_HEADER);
            header.extend_from_slice(ENTRY_MAGIC);
            header.extend_from_slice(&self.namespace);
            header.extend_from_slice(hash);
            header.extend_from_slice(&(physical.len() as u64).to_be_bytes());
            stage.write_all(&header).map_err(io_error)?;
            stage.write_all(&physical).map_err(io_error)?;
            stage.sync_data().map_err(io_error)?;
            directory.ensure_paths()?;
            if !stage_identity.matches(&stage_path) {
                return Err(unsafe_cache("cache staging file was replaced"));
            }
            // No overwrite: a replacement/unexpected target remains untouched.
            std::fs::hard_link(&stage_path, &path).map_err(io_error)?;
            drop(cleanup);
            stats.physical_write_bytes = stats.physical_write_bytes.saturating_add(bytes);
            stats.canonical_write_bytes = stats
                .canonical_write_bytes
                .saturating_add(canonical.len() as u64);
            stats.puts += 1;
            entries.push(
                directory
                    .inspect(&path, Some((self.namespace, *hash)))?
                    .ok_or_else(|| unsafe_cache("published cache entry disappeared"))?,
            );
            observe(stats, &entries);
            Ok(())
        })();
        if let Err(error) = result {
            record_disk_error(stats, &error);
            false
        } else {
            true
        }
    }

    /// Best-effort deletion of this recognized namespace only. This is neither
    /// secure erasure nor a claim that other processes/media lost old copies.
    pub fn purge_namespace(&self, namespace: &Digest) -> bool {
        let mut state = lock(&self.state);
        let State { directory, stats } = &mut *state;
        let Some(directory) = directory else {
            return true;
        };
        let _guard = match DiskLock::acquire(&directory.lock) {
            Ok(guard) => guard,
            Err(error) => {
                record_disk_error(stats, &error);
                return false;
            }
        };
        let result = (|| {
            directory.ensure_paths()?;
            directory.recover_stage()?;
            let mut retained = Vec::new();
            let inventory = directory.inventory()?;
            inventory.observe(stats);
            for entry in inventory.intact_entries()? {
                if &entry.namespace == namespace {
                    directory.remove(&entry)?;
                    stats.purged_entries += 1;
                } else {
                    retained.push(entry);
                }
            }
            observe(stats, &retained);
            Ok(())
        })();
        if let Err(error) = result {
            record_disk_error(stats, &error);
            false
        } else {
            true
        }
    }
}

struct Entry {
    path: PathBuf,
    identity: Identity,
    namespace: Digest,
    bytes: u64,
    modified: SystemTime,
}

struct Inventory {
    entries: Vec<Entry>,
    corrupt_entries: usize,
    physical_bytes: u64,
}

impl Inventory {
    fn observe(&self, stats: &mut SsdCacheStats) {
        stats.current_entries = self.entries.len() + self.corrupt_entries;
        stats.current_bytes = self.physical_bytes;
        stats.peak_entries = stats.peak_entries.max(stats.current_entries);
        stats.peak_bytes = stats.peak_bytes.max(stats.current_bytes);
        stats.inventory_complete = self.corrupt_entries == 0;
    }

    fn intact_entries(self) -> Result<Vec<Entry>, SemanticError> {
        if self.corrupt_entries != 0 {
            return Err(corrupt_entry("cache inventory contains malformed entries"));
        }
        Ok(self.entries)
    }
}

impl Directory {
    fn open(path: &Path, limits: SsdCacheLimits) -> Result<Self, SemanticError> {
        if limits.max_entries == 0 || limits.max_bytes == 0 {
            return Err(unsafe_cache("enabled SSD cache limits must be positive"));
        }
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir().map_err(io_error)?.join(path)
        };
        // SAFETY: geteuid has no pointer arguments or memory-safety preconditions.
        let uid = unsafe { libc::geteuid() };
        let mut prefix = PathBuf::new();
        for component in path.components() {
            if matches!(component, Component::ParentDir) {
                return Err(unsafe_cache("cache path cannot contain parent traversal"));
            }
            prefix.push(component.as_os_str());
            let metadata = std::fs::symlink_metadata(&prefix).map_err(io_error)?;
            if !metadata.is_dir()
                || metadata.file_type().is_symlink()
                || (metadata.uid() != uid && metadata.uid() != 0)
                || (metadata.mode() & 0o022 != 0 && metadata.mode() & 0o1000 == 0)
            {
                return Err(unsafe_cache(
                    "cache path contains an unsafe directory or symlink",
                ));
            }
        }
        let metadata = std::fs::symlink_metadata(&path).map_err(io_error)?;
        if metadata.uid() != uid || metadata.mode() & 0o7777 != 0o700 {
            return Err(unsafe_cache(
                "cache requires an owned existing mode-0700 directory",
            ));
        }
        let identity = Identity::of(&metadata);
        // Serialize only opening/initializing this directory. No ownership
        // file is created until its complete record can be published. The
        // directory-inode lock also hides the brief temp/hard-link interval
        // from another opener; ordinary cache I/O keeps its permanent lock.
        let initialization = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
            .open(&path)
            .map_err(io_error)?;
        if Identity::of(&initialization.metadata().map_err(io_error)?) != identity {
            return Err(unsafe_cache(
                "cache directory changed before initialization",
            ));
        }
        let _initialization_guard = DiskLock::acquire_initialization(&initialization)?;
        let lock_path = path.join(LOCK_NAME);
        let record = format!(
            "ATOMIC-SSD-CACHE 1\nentries {}\nbytes {}\n",
            limits.max_entries, limits.max_bytes
        );
        let open_existing = || {
            OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&lock_path)
        };
        let mut lock = match open_existing() {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // An orphan initializer without a published ownership record
                // is not proof that unrelated files are disposable. Preserve
                // it and require inspection or another empty private root.
                if std::fs::read_dir(&path).map_err(io_error)?.next().is_some() {
                    return Err(unsafe_cache("unclaimed SSD cache directory must be empty"));
                }
                let mut candidate = tempfile::Builder::new()
                    .prefix(".atomic-ssd-init-")
                    .tempfile_in(&path)
                    .map_err(io_error)?;
                candidate.write_all(record.as_bytes()).map_err(io_error)?;
                candidate.as_file().sync_data().map_err(io_error)?;
                match candidate.persist_noclobber(&lock_path) {
                    Ok(file) => file,
                    Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                        drop(error);
                        open_existing().map_err(io_error)?
                    }
                    Err(error) => return Err(io_error(error.error)),
                }
            }
            Err(error) => return Err(io_error(error)),
        };
        let metadata = lock.metadata().map_err(io_error)?;
        validate_file(&metadata, uid)?;
        let lock_identity = Identity::of(&metadata);
        lock.rewind().map_err(io_error)?;
        let mut actual = String::new();
        (&lock)
            .take(record.len() as u64 + 1)
            .read_to_string(&mut actual)
            .map_err(io_error)?;
        if actual != record {
            return Err(unsafe_cache(
                "cache ownership format or shared-directory limits do not match",
            ));
        }
        let directory = Self {
            path,
            identity,
            lock,
            lock_identity,
            limits,
            uid,
        };
        directory.ensure_paths()?;
        Ok(directory)
    }

    fn ensure_paths(&self) -> Result<(), SemanticError> {
        let metadata = std::fs::symlink_metadata(&self.path).map_err(io_error)?;
        if !metadata.is_dir()
            || metadata.uid() != self.uid
            || metadata.mode() & 0o7777 != 0o700
            || Identity::of(&metadata) != self.identity
            || !self.lock_identity.matches(&self.path.join(LOCK_NAME))
        {
            return Err(unsafe_cache("cache ownership paths changed"));
        }
        Ok(())
    }

    fn inspect(
        &self,
        path: &Path,
        expected: Option<(Digest, Digest)>,
    ) -> Result<Option<Entry>, SemanticError> {
        let mut file = match open_read(path) {
            Ok(file) => file,
            Err(error) if error.code == "cache/not-found" => return Ok(None),
            Err(error) => return Err(error),
        };
        let metadata = file.metadata().map_err(io_error)?;
        validate_file(&metadata, self.uid)?;
        if metadata.len() < ENTRY_HEADER as u64
            || metadata.len() > (ENTRY_HEADER + MAX_PHYSICAL_BLOCK_BYTES) as u64
        {
            return Err(corrupt_entry("cache entry length is invalid"));
        }
        let mut header = [0u8; ENTRY_HEADER];
        file.read_exact(&mut header).map_err(io_error)?;
        let (namespace, hash, payload_bytes) =
            parse_header(&header).map_err(|_| corrupt_entry("cache entry header is invalid"))?;
        if payload_bytes > MAX_PHYSICAL_BLOCK_BYTES as u64
            || payload_bytes != metadata.len() - ENTRY_HEADER as u64
            || expected.is_some_and(|expected| expected != (namespace, hash))
        {
            return Err(corrupt_entry(
                "cache entry identity or length does not match",
            ));
        }
        Ok(Some(Entry {
            path: path.to_owned(),
            identity: Identity::of(&metadata),
            namespace,
            bytes: metadata.len(),
            modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        }))
    }

    fn inventory(&self) -> Result<Inventory, SemanticError> {
        self.ensure_paths()?;
        let mut entries = Vec::new();
        let mut corrupt_entries = 0;
        let mut physical_bytes = 0_u64;
        for (index, child) in std::fs::read_dir(&self.path).map_err(io_error)?.enumerate() {
            if index >= self.limits.max_entries.saturating_add(2) {
                return Err(unsafe_cache(
                    "cache directory exceeds its bounded scan limit",
                ));
            }
            let child = child.map_err(io_error)?;
            if child.file_name() == LOCK_NAME {
                continue;
            }
            let name = child.file_name();
            let (namespace, hash) = parse_name(
                name.to_str()
                    .ok_or_else(|| unsafe_cache("unexpected cache directory entry"))?,
            )?;
            match self.inspect(&child.path(), Some((namespace, hash))) {
                Ok(Some(entry)) => {
                    physical_bytes = physical_bytes.saturating_add(entry.bytes);
                    entries.push(entry);
                }
                Err(error) if error.code == "cache/corrupt-entry" => {
                    // A malformed owned regular file at a recognized name is
                    // unusable, not deletion authority. Count its physical
                    // occupancy and continue checking ALL remaining paths so
                    // corruption cannot conceal unknown files or unsafe links.
                    let metadata = std::fs::symlink_metadata(child.path()).map_err(io_error)?;
                    validate_file(&metadata, self.uid)?;
                    physical_bytes = physical_bytes.saturating_add(metadata.len());
                    corrupt_entries += 1;
                }
                Err(error) => return Err(error),
                Ok(None) => return Err(unsafe_cache("cache entry disappeared during accounting")),
            }
            if entries.len() + corrupt_entries > self.limits.max_entries {
                return Err(unsafe_cache("cache directory exceeds its entry limit"));
            }
        }
        Ok(Inventory {
            entries,
            corrupt_entries,
            physical_bytes,
        })
    }

    fn recover_stage(&self) -> Result<(), SemanticError> {
        self.ensure_paths()?;
        let path = self.path.join(STAGE_NAME);
        let mut file = match open_read(&path) {
            Ok(file) => file,
            Err(error) if error.code == "cache/not-found" => return Ok(()),
            Err(error) => return Err(error),
        };
        let metadata = file.metadata().map_err(io_error)?;
        if !metadata.is_file()
            || metadata.uid() != self.uid
            || metadata.mode() & 0o7777 != 0o600
            || !(1..=2).contains(&metadata.nlink())
        {
            return Err(unsafe_cache("unexpected cache staging target"));
        }
        let mut header = [0u8; ENTRY_HEADER];
        file.read_exact(&mut header).map_err(io_error)?;
        let (namespace, hash, payload_bytes) = parse_header(&header)?;
        if payload_bytes > MAX_PHYSICAL_BLOCK_BYTES as u64
            || metadata.len() > ENTRY_HEADER as u64 + payload_bytes
        {
            return Err(unsafe_cache("unexpected cache staging length"));
        }
        let identity = Identity::of(&metadata);
        if metadata.nlink() == 2
            && !identity.matches(&self.path.join(entry_name(&namespace, &hash)))
        {
            return Err(unsafe_cache(
                "cache staging file has an unexpected hard link",
            ));
        }
        if !identity.matches(&path) {
            return Err(unsafe_cache("cache staging file changed"));
        }
        std::fs::remove_file(path).map_err(io_error)
    }

    fn remove(&self, entry: &Entry) -> Result<(), SemanticError> {
        self.ensure_paths()?;
        let current = self
            .inspect(&entry.path, None)?
            .ok_or_else(|| unsafe_cache("cache eviction target disappeared"))?;
        if current.identity != entry.identity {
            return Err(unsafe_cache("cache eviction target was replaced"));
        }
        std::fs::remove_file(&entry.path).map_err(io_error)
    }
}

struct StageCleanup<'a> {
    directory: &'a Directory,
    path: PathBuf,
    identity: Identity,
}
impl Drop for StageCleanup<'_> {
    fn drop(&mut self) {
        if self.directory.ensure_paths().is_ok() && self.identity.matches(&self.path) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
struct DiskLock<'a>(&'a File);
impl<'a> DiskLock<'a> {
    fn acquire_initialization(file: &'a File) -> Result<Self, SemanticError> {
        let started = Instant::now();
        loop {
            match Self::acquire(file) {
                Err(error)
                    if error.category == ErrorCategory::Busy
                        && started.elapsed() < Duration::from_secs(1) =>
                {
                    std::thread::sleep(Duration::from_millis(1));
                }
                result => return result,
            }
        }
    }

    fn acquire(file: &'a File) -> Result<Self, SemanticError> {
        file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => {
                SemanticError::new(ErrorCategory::Busy, "cache/busy", "SSD cache is busy")
            }
            std::fs::TryLockError::Error(error) => io_error(error),
        })?;
        Ok(Self(file))
    }
}
impl Drop for DiskLock<'_> {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn validate_file(metadata: &Metadata, uid: u32) -> Result<(), SemanticError> {
    if !metadata.is_file()
        || metadata.uid() != uid
        || metadata.mode() & 0o7777 != 0o600
        || metadata.nlink() != 1
    {
        return Err(unsafe_cache(
            "cache file must be owned, mode-0600, regular, and singly linked",
        ));
    }
    Ok(())
}
fn open_read(path: &Path) -> Result<File, SemanticError> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(io_error)
}
fn parse_header(header: &[u8; ENTRY_HEADER]) -> Result<(Digest, Digest, u64), SemanticError> {
    if !header.starts_with(ENTRY_MAGIC) {
        return Err(unsafe_cache("unrecognized cache entry header"));
    }
    let at = ENTRY_MAGIC.len();
    Ok((
        header[at..at + 32].try_into().unwrap(),
        header[at + 32..at + 64].try_into().unwrap(),
        u64::from_be_bytes(header[at + 64..].try_into().unwrap()),
    ))
}
fn hex(hash: &Digest) -> String {
    hash.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn entry_name(namespace: &Digest, hash: &Digest) -> String {
    format!("v1-{}-{}.block", hex(namespace), hex(hash))
}
fn parse_name(name: &str) -> Result<(Digest, Digest), SemanticError> {
    let bytes = name.as_bytes();
    if bytes.len() != 138
        || !bytes.starts_with(b"v1-")
        || bytes[67] != b'-'
        || !bytes.ends_with(b".block")
    {
        return Err(unsafe_cache("unexpected cache directory entry"));
    }
    fn digest(bytes: &[u8]) -> Result<Digest, SemanticError> {
        let mut out = [0; 32];
        for (out, pair) in out.iter_mut().zip(bytes.chunks_exact(2)) {
            let digit = |byte: u8| match byte {
                b'0'..=b'9' => Some(byte - b'0'),
                b'a'..=b'f' => Some(byte - b'a' + 10),
                _ => None,
            };
            *out = digit(pair[0])
                .zip(digit(pair[1]))
                .map(|(a, b)| a * 16 + b)
                .ok_or_else(|| unsafe_cache("unexpected cache digest filename"))?;
        }
        Ok(out)
    }
    Ok((digest(&bytes[3..67])?, digest(&bytes[68..132])?))
}
fn observe(stats: &mut SsdCacheStats, entries: &[Entry]) {
    stats.current_entries = entries.len();
    stats.current_bytes = entries.iter().map(|entry| entry.bytes).sum();
    stats.peak_entries = stats.peak_entries.max(stats.current_entries);
    stats.peak_bytes = stats.peak_bytes.max(stats.current_bytes);
    stats.inventory_complete = true;
}
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
fn unsafe_cache(message: &'static str) -> SemanticError {
    SemanticError::incorrect("cache/unsafe-config", message)
}
fn corrupt_entry(message: &'static str) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, "cache/corrupt-entry", message)
}
fn io_error(error: std::io::Error) -> SemanticError {
    let code = if error.kind() == std::io::ErrorKind::NotFound {
        "cache/not-found"
    } else {
        "cache/io"
    };
    SemanticError::new(
        ErrorCategory::Unavailable,
        code,
        "SSD cache filesystem operation failed",
    )
}
fn record_disk_error(stats: &mut SsdCacheStats, error: &SemanticError) {
    if error.code == "cache/corrupt-entry" {
        stats.corruptions += 1;
        stats.corrupt_entry_bypasses += 1;
        stats.inventory_complete = false;
    } else if error.category == ErrorCategory::Busy {
        stats.busy_bypasses += 1;
    } else {
        stats.io_errors += 1;
    }
}
