//! Captured selective reads over opaque blocks, without relational peer state.
//!
//! Capturing and using a value only reads immutable storage. Storage collection
//! retains retired structures for the configured grace period; readers do not
//! register sessions, publish pins, or coordinate with the writer.
use super::descriptors::{IndexDescriptor, SnapshotMetadata, order_tag};
use super::root::{
    Block, DATABASE_ROOT_KIND, DATABASE_VALUE_ROOT_KIND, DatabaseRoot, DatabaseValueRoot,
};
use super::{ObjectId, PgBlockStore, RefCondition};
use crate::index::NormalizedIndexBoundary;
use crate::index::cursor::{DurableTreeCursor, DurableTreeSource, MergeCursor, MergeSource};
use crate::index::recent::{EndpointProjection, RecentLimits, RecentRange, RecentTier};
use crate::index::tree::cache::TreeNodeCache;
use crate::index::tree::{RootNode, TreeNode, TreeReadStats, decode_tree_node};
use crate::model::idents::IdentIndex;
use crate::runtime::executor::Owned;
use crate::{
    CacheStats, DatabaseValue, Datom, Digest, DurableTransaction, ErrorCategory, IndexBoundary,
    IndexOrder, IndexPrefix, Keyword, PeerCursorStats, PostgresConnectionConfig, Schema,
    SemanticError,
};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

/// The existing executor owns final driver cleanup; all captured snapshots
/// from one reader share this single configured resource reservation.
struct BlockStoreResource {
    store: Owned<Mutex<Option<PgBlockStore>>>,
}
impl BlockStoreResource {
    fn lock(&self) -> BlockStoreGuard<'_> {
        BlockStoreGuard {
            store: self.store.lock().unwrap_or_else(|e| e.into_inner()),
        }
    }
}
struct BlockStoreGuard<'a> {
    store: MutexGuard<'a, Option<PgBlockStore>>,
}
impl Deref for BlockStoreGuard<'_> {
    type Target = PgBlockStore;
    fn deref(&self) -> &Self::Target {
        self.store.as_ref().expect("live block store")
    }
}
impl DerefMut for BlockStoreGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.store.as_mut().expect("live block store")
    }
}
impl super::ObjectReader for BlockStoreGuard<'_> {
    fn read_object(&mut self, id: ObjectId) -> Result<Vec<u8>, SemanticError> {
        super::ObjectReader::read_object(&mut **self, id)
    }
}
#[derive(Clone, Debug)]
pub struct BlockReadConfig {
    pub cache_entries: usize,
    pub cache_bytes: usize,
    /// Optional local admission limits for already committed recent data.
    /// `usize::MAX` leaves a dimension unspecified. Ordinary readers accept
    /// the writer's authenticated window; fresh writer admission is separate.
    pub max_recent_transactions: usize,
    pub max_recent_bytes: usize,
    pub max_recent_datoms: usize,
}
impl Default for BlockReadConfig {
    fn default() -> Self {
        Self {
            cache_entries: 256,
            cache_bytes: 64 * 1024 * 1024,
            max_recent_transactions: usize::MAX,
            max_recent_bytes: usize::MAX,
            max_recent_datoms: usize::MAX,
        }
    }
}

/// Descriptor/tree/program payload reads, including selective metadata reads.
/// Log-tail I/O uses the log reader and is separately visible in OperationContext.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BlockReadStats {
    pub object_reads: u64,
    pub object_bytes: u64,
    pub tree: TreeReadStats,
    pub datoms_yielded: u64,
}

/// One exact immutable root over the block store. Methods that read cold
/// objects are synchronous. Values captured by one reader share its connection
/// and configured executor resource reservation. Dropping a value never
/// closes its PostgreSQL driver on the calling thread; final driver cleanup
/// is deferred to the existing executor.
#[derive(Clone)]
pub struct BlockSnapshot {
    inner: Arc<SnapshotInner>,
    source: BlockNodeSource,
}

/// Shareable connection/cache context for capturing immutable database values.
/// One reader reserves one resource on the supplied executor; capturing more
/// values creates no connections, workers, or additional resource reservations.
#[derive(Clone)]
pub struct BlockReader {
    source: BlockNodeSource,
    limits: BlockReadConfig,
}

/// An immutable publication observation. The revision is used only when a
/// writer later conditionally publishes a successor, not for reading this value.
#[derive(Clone, Debug)]
pub(crate) struct RootCapture {
    root_id: ObjectId,
    source_condition: RefCondition,
}
impl RootCapture {
    pub(crate) fn root_id(&self) -> ObjectId {
        self.root_id
    }
    pub(crate) fn source_condition(&self) -> RefCondition {
        self.source_condition.clone()
    }
    pub(crate) fn source_revision(&self) -> u64 {
        self.source_condition
            .expected
            .expect("captured publication revision")
    }
}

struct SnapshotInner {
    captured: DatabaseValueRoot,
    storage_root: ObjectId,
    publication_revision: Option<u64>,
    route: Option<Arc<str>>,
    metadata: SnapshotMetadata,
    indexes: Arc<IndexDescriptor>,
    base_metadata: Arc<crate::index::metadata::MetadataProjection>,
    endpoint_metadata: Arc<crate::index::metadata::MetadataProjection>,
    lineage: String,
    roots: BTreeMap<(bool, u8), Arc<RootNode>>,
    schema: Arc<Schema>,
    idents: Arc<IdentIndex>,
    recent: RecentTier,
    log: Option<super::log::LogRoot>,
    avet_unready: Arc<BTreeSet<u32>>,
    metadata_residency: crate::index::metadata::ResidentMetadataStats,
    root_residency: crate::index::metadata::ResidentTreeRootStats,
    limits: BlockReadConfig,
}
impl std::fmt::Debug for BlockSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlockSnapshot")
            .field("identity", &self.inner.lineage)
            .field("basis", &self.basis_t())
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
struct BlockNodeSource {
    backend: SourceBackend,
    programs: crate::program_cache::SharedProgramCache,
    cache: TreeNodeCache,
    fulltext_cache: crate::fulltext::store::FulltextCache,
    ssd_cache: crate::SsdCache,
    ssd_namespace: Digest,
    ssd_scope: Option<([u8; 16], u64)>,
    stats: Arc<Mutex<BlockReadStats>>,
    node_stats: Arc<Mutex<crate::NodeBlockReadStats>>,
    cursor_stats: Arc<Mutex<crate::PeerLoadStats>>,
}
#[derive(Clone)]
enum SourceBackend {
    Live(Arc<LiveSource>),
    // EDB-NOTE: the backup owner authenticates repository files; this owner
    // still interprets the same immutable database structures. The dependency
    // is offline-provider composition, not a second query engine or a route to
    // live publication authority (see `live` below).
    Repository(Arc<PathBuf>),
}
#[derive(Clone)]
struct LiveSource {
    store: Arc<BlockStoreResource>,
    connection: PostgresConnectionConfig,
}
impl BlockNodeSource {
    fn live(&self) -> Result<&LiveSource, SemanticError> {
        match &self.backend {
            SourceBackend::Live(live) => Ok(live),
            SourceBackend::Repository(_) => Err(repository_authority()),
        }
    }
    fn lock(&self) -> Result<BlockStoreGuard<'_>, SemanticError> {
        Ok(self.live()?.store.lock())
    }
    fn scoped(&self, identity: [u8; 16], generation: u64) -> Self {
        let mut source = self.clone();
        source.ssd_scope = Some((identity, generation));
        source
    }
    fn cache_namespace(&self, identity: [u8; 16], generation: u64) -> Digest {
        let mut key = b"edb/opaque-cache/1\0".to_vec();
        key.extend_from_slice(&self.ssd_namespace);
        key.extend_from_slice(&identity);
        key.extend_from_slice(&generation.to_be_bytes());
        crate::sha256(&key)
    }
    fn read(&self, id: Digest) -> Result<Vec<u8>, SemanticError> {
        self.read_with_stats(id).map(|(bytes, _)| bytes)
    }
    fn read_with_stats(
        &self,
        id: Digest,
    ) -> Result<(Vec<u8>, Option<super::ObjectReadStats>), SemanticError> {
        if let SourceBackend::Repository(directory) = &self.backend {
            let observed =
                crate::io_diagnostics::CacheObservation::start(crate::CacheTier::LocalDisk);
            let result = crate::backup::read_object(directory, id);
            let bytes = result.as_ref().map_or(0, |bytes| bytes.len() as u64);
            observed.finish(result.is_ok(), result.is_err(), bytes, bytes);
            let mut stats = self.stats.lock().unwrap_or_else(|e| e.into_inner());
            stats.object_reads = stats.object_reads.saturating_add(1);
            stats.object_bytes = stats.object_bytes.saturating_add(bytes);
            return result.map(|bytes| (bytes, None));
        }
        let ssd = self.ssd_scope.map(|(identity, generation)| {
            self.ssd_cache
                .for_namespace(self.cache_namespace(identity, generation))
        });
        if let Some(bytes) = ssd.as_ref().and_then(|cache| cache.get(&id)) {
            return Ok((bytes, None));
        }
        let observed =
            crate::io_diagnostics::CacheObservation::start(crate::CacheTier::PostgresBlock);
        let (result, read) = {
            let mut store = match self.lock() {
                Ok(store) => store,
                Err(error) => {
                    observed.finish(false, true, 0, 0);
                    return Err(error);
                }
            };
            let before = store.object_read_stats();
            let result = store.get(id);
            let after = store.object_read_stats();
            (
                result,
                super::ObjectReadStats {
                    physical_bytes: after.physical_bytes.saturating_sub(before.physical_bytes),
                    canonical_bytes: after.canonical_bytes.saturating_sub(before.canonical_bytes),
                    compressed_hits: after.compressed_hits.saturating_sub(before.compressed_hits),
                    raw_reads: after.raw_reads.saturating_sub(before.raw_reads),
                    decode_nanos: after.decode_nanos.saturating_sub(before.decode_nanos),
                },
            )
        };
        observed.finish(
            matches!(&result, Ok(Some(_))),
            result.is_err(),
            read.physical_bytes,
            read.canonical_bytes,
        );
        let mut stats = self.stats.lock().unwrap_or_else(|e| e.into_inner());
        stats.object_reads = stats.object_reads.saturating_add(1);
        if let Ok(Some(bytes)) = &result {
            stats.object_bytes = stats.object_bytes.saturating_add(bytes.len() as u64);
        }
        let bytes = result?.ok_or_else(|| {
            fault(
                "storage/missing-object",
                "Captured root references a missing immutable object",
            )
        })?;
        drop(stats);
        if let Some(cache) = ssd {
            cache.put(&id, &bytes);
        }
        Ok((bytes, Some(read)))
    }
}
impl DurableTreeSource for BlockNodeSource {
    fn load_node(
        &self,
        hash: Digest,
        stats: &mut TreeReadStats,
    ) -> Result<Arc<TreeNode>, SemanticError> {
        let observed =
            crate::io_diagnostics::CacheObservation::start(crate::CacheTier::DecodedNode);
        if let Some(node) = self.cache.get(&hash) {
            stats.cache_hits = stats.cache_hits.saturating_add(1);
            let mut total = self.stats.lock().unwrap_or_else(|e| e.into_inner());
            total.tree.cache_hits = total.tree.cache_hits.saturating_add(1);
            observed.finish(true, false, 0, 0);
            return Ok(node);
        }
        observed.finish(false, false, 0, 0);
        stats.cache_misses = stats.cache_misses.saturating_add(1);
        {
            let mut total = self.stats.lock().unwrap_or_else(|e| e.into_inner());
            total.tree.cache_misses = total.tree.cache_misses.saturating_add(1);
        }
        let (bytes, read) = self.read_with_stats(hash)?;
        let node = Arc::new(decode_tree_node(&hash, &bytes)?);
        let mut total = self.stats.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(read) = read {
            let mut nodes = self.node_stats.lock().unwrap_or_else(|e| e.into_inner());
            nodes.compressed_hits = nodes.compressed_hits.saturating_add(read.compressed_hits);
            nodes.canonical_reads = nodes.canonical_reads.saturating_add(read.raw_reads);
            nodes.canonical_bytes = nodes.canonical_bytes.saturating_add(read.canonical_bytes);
            nodes.physical_read_bytes = nodes
                .physical_read_bytes
                .saturating_add(read.physical_bytes);
            nodes.decode_elapsed_nanos =
                nodes.decode_elapsed_nanos.saturating_add(read.decode_nanos);
        }
        if read.is_some() || matches!(self.backend, SourceBackend::Repository(_)) {
            match node.as_ref() {
                TreeNode::Root(_) => {
                    stats.root_reads += 1;
                    total.tree.root_reads += 1;
                }
                TreeNode::Directory(_) => {
                    stats.directory_reads += 1;
                    total.tree.directory_reads += 1;
                }
                TreeNode::Leaf(_) => {
                    stats.leaf_reads += 1;
                    total.tree.leaf_reads += 1;
                }
            }
            stats.decoded_bytes = stats.decoded_bytes.saturating_add(bytes.len() as u64);
            total.tree.decoded_bytes = total.tree.decoded_bytes.saturating_add(bytes.len() as u64);
        }
        drop(total);
        let retained = node.retained_bytes().saturating_add(bytes.len() as u64);
        self.cache.insert(
            hash,
            Arc::clone(&node),
            usize::try_from(retained).unwrap_or(usize::MAX),
        );
        Ok(node)
    }
}

impl BlockReader {
    /// A reader-local single cleanup worker. Reuse this reader for additional
    /// captures, or supply an application executor with `connect_with_executor`.
    pub fn connect(
        config: &PostgresConnectionConfig,
        limits: BlockReadConfig,
    ) -> Result<Self, SemanticError> {
        let executor = crate::AsyncExecutor::new(crate::AsyncConfig {
            workers: 1,
            max_operations: 1,
            max_resources: 1,
        })?;
        Self::connect_with_executor(config, limits, &executor)
    }
    /// Connect on a blocking thread during application setup. The supplied
    /// executor's existing bounded cleanup path owns the driver's final Drop.
    pub fn connect_with_executor(
        config: &PostgresConnectionConfig,
        limits: BlockReadConfig,
        executor: &crate::AsyncExecutor,
    ) -> Result<Self, SemanticError> {
        let retained = executor.retain(|| Mutex::new(None))?;
        let store = PgBlockStore::connect(config)?;
        let (ssd_cache, ssd_namespace) =
            crate::ssd_cache::open_connection_cache(config, "opaque-blocks")?;
        *retained.lock().unwrap_or_else(|e| e.into_inner()) = Some(store);
        Ok(Self {
            source: BlockNodeSource {
                programs: Default::default(),
                backend: SourceBackend::Live(Arc::new(LiveSource {
                    store: Arc::new(BlockStoreResource { store: retained }),
                    connection: config.clone(),
                })),
                cache: TreeNodeCache::new(limits.cache_entries, limits.cache_bytes),
                fulltext_cache: crate::fulltext::store::FulltextCache::new(
                    limits.cache_entries,
                    limits.cache_bytes,
                ),
                ssd_cache,
                ssd_namespace,
                ssd_scope: None,
                stats: Arc::new(Mutex::new(BlockReadStats::default())),
                node_stats: Arc::new(Mutex::new(crate::NodeBlockReadStats::default())),
                cursor_stats: Arc::new(Mutex::new(crate::PeerLoadStats::default())),
            },
            limits,
        })
    }

    /// Resolve a publication/value reference once and follow that immutable
    /// capture. Concurrent publication does not retarget it or require a retry.
    /// Durable ownership and retirement grace, not a reader registration, keep
    /// the referenced objects available; missing required objects fail closed.
    pub fn capture(&self, reference_key: &str) -> Result<BlockSnapshot, SemanticError> {
        let capture = self.capture_reference(reference_key)?;
        self.capture_root(&capture)
    }
    fn live(&self) -> &LiveSource {
        // BlockReader has only PostgreSQL constructors. Repository values are
        // fixed BlockSnapshots and cannot acquire publication authority.
        self.source.live().expect("BlockReader is a live source")
    }
    pub(crate) fn connection(&self) -> &PostgresConnectionConfig {
        &self.live().connection
    }
    /// Shared decoded-program work and bounded accounted cache footprint.
    pub fn program_cache_stats(&self) -> crate::ProgramCacheStats {
        crate::program_cache::stats(&self.source.programs)
    }
    /// Change discardable cache admission for this reader and its values.
    /// Zero disables retention, not program execution.
    pub fn set_program_cache_limits(&self, entries: usize, bytes: usize) {
        crate::program_cache::lock(&self.source.programs).set_limits(entries, bytes);
    }
    pub(crate) fn program_cache(&self) -> crate::program_cache::SharedProgramCache {
        Arc::clone(&self.source.programs)
    }
    pub(crate) fn with_program_cache(
        mut self,
        cache: crate::program_cache::SharedProgramCache,
    ) -> Self {
        self.source.programs = cache;
        self
    }
    pub(crate) fn reconnect(&self) -> Result<(), SemanticError> {
        let next = PgBlockStore::connect(self.connection())?;
        *self
            .live()
            .store
            .store
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(next);
        Ok(())
    }
    pub(crate) fn ssd_cache_stats(&self) -> crate::SsdCacheStats {
        self.source.ssd_cache.stats()
    }
    pub(crate) fn node_block_read_stats(&self) -> crate::NodeBlockReadStats {
        *self
            .source
            .node_stats
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }
    pub(crate) fn load_stats(&self) -> crate::PeerLoadStats {
        let tree = self.read_stats().tree;
        let mut result = *self
            .source
            .cursor_stats
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        result.root_reads = tree.root_reads;
        result.directory_reads = tree.directory_reads;
        result.leaf_reads = tree.leaf_reads;
        result
    }
    pub(crate) fn purge_ssd_generation(&self, identity: [u8; 16], generation: u64) -> bool {
        self.source
            .ssd_cache
            .purge_namespace(&self.source.cache_namespace(identity, generation))
    }

    pub(crate) fn capture_reference(
        &self,
        reference_key: &str,
    ) -> Result<RootCapture, SemanticError> {
        let mut store = self.source.lock()?;
        let publication = store.read_ref(reference_key)?.ok_or_else(|| {
            SemanticError::new(
                ErrorCategory::NotFound,
                "storage/reference-not-found",
                "Database reference does not exist",
            )
        })?;
        let root_id: ObjectId = publication
            .value
            .as_deref()
            .ok_or_else(|| {
                SemanticError::new(
                    ErrorCategory::NotFound,
                    if reference_key.starts_with("databases/") {
                        "catalog/database-retired"
                    } else {
                        "storage/reference-retired"
                    },
                    "Database reference is retired",
                )
            })?
            .try_into()
            .map_err(|_| {
                fault(
                    "storage/root-reference",
                    "Database reference must contain one object identity",
                )
            })?;
        drop(store);
        Ok(RootCapture {
            root_id,
            source_condition: RefCondition {
                key: reference_key.into(),
                expected: Some(publication.revision),
            },
        })
    }

    /// Reuse one observed immutable publication for a complete report
    /// batch. No per-report read may switch to a changing current head.
    pub(crate) fn report_publication(
        &self,
        reference_key: &str,
        snapshot: &BlockSnapshot,
    ) -> Result<RootCapture, SemanticError> {
        Ok(RootCapture {
            root_id: snapshot.storage_root_id(),
            source_condition: RefCondition {
                key: reference_key.into(),
                expected: Some(snapshot.publication_revision().ok_or_else(|| {
                    fault(
                        "storage/report-source",
                        "Report source has no publication revision",
                    )
                })?),
            },
        })
    }

    /// Recover a generation's exact reports within the collection grace period.
    /// Ordinary snapshot reads never consult report handoffs.
    pub(crate) fn report_handoff(
        &self,
        source: &RootCapture,
        route: [u8; 16],
        identity: [u8; 16],
        source_generation: u64,
        generation: u64,
    ) -> Result<(RootCapture, u64), SemanticError> {
        if generation < source_generation {
            return Err(fault(
                "storage/report-generation",
                "Report catchup cannot move backwards",
            ));
        }
        let key = super::report_handoff::handoff_key(route, generation);
        let handoff = self.capture_reference(&key).map_err(|error| {
            if error.category == ErrorCategory::NotFound {
                SemanticError::new(
                    ErrorCategory::NotFound,
                    "peer/report-history-unavailable",
                    "Original transaction reports are unavailable across this generation replacement; disconnect and reopen the observer",
                )
                .detail("generation", generation.to_string())
            } else {
                error
            }
        })?;
        let descriptor = super::report_handoff::ReportHandoff::decode(
            &handoff.root_id(),
            &self.read_object(handoff.root_id())?,
        )?;
        if descriptor.route != route
            || descriptor.identity != identity
            || descriptor.generation != generation
        {
            return Err(fault(
                "storage/report-handoff-coordinate",
                "Report handoff belongs to a different route or generation",
            ));
        }
        let root = DatabaseRoot::decode(
            &descriptor.publication,
            &self.read_object(descriptor.publication)?,
        )?;
        let metadata_id = root.metadata.ok_or_else(|| {
            fault(
                "storage/report-handoff-coordinate",
                "Handoff has no metadata",
            )
        })?;
        let metadata = SnapshotMetadata::decode(&metadata_id, &self.read_object(metadata_id)?)?;
        if root.identity != identity
            || root.basis != descriptor.basis
            || metadata.identity != identity
            || metadata.basis != root.basis
            || metadata.generation != generation
        {
            return Err(fault(
                "storage/report-handoff-coordinate",
                "Handoff publication differs from its authenticated coordinates",
            ));
        }
        Ok((
            RootCapture {
                root_id: descriptor.publication,
                // Retain the route provenance, not the handoff namespace,
                // on returned exact before/after values.
                source_condition: source.source_condition(),
            },
            root.basis,
        ))
    }

    pub(crate) fn capture_root(
        &self,
        capture: &RootCapture,
    ) -> Result<BlockSnapshot, SemanticError> {
        let mut snapshot =
            self.capture_immutable(capture.root_id(), &[capture.source_condition()])?;
        let inner = Arc::get_mut(&mut snapshot.inner).unwrap();
        inner.publication_revision = Some(capture.source_revision());
        inner.route = route_from_conditions(&[capture.source_condition()]);
        Ok(snapshot)
    }

    /// Observe a new publication without rebuilding an unchanged reader or
    /// replaying its already-resident log prefix. Index/generation changes
    /// reconstruct only the bounded tail against their new durable base.
    /// Reuse authenticated immutable state without borrowing another owner's
    /// driver. The caller validates identity and the observed successor first.
    pub(crate) fn adopt_snapshot(&self, snapshot: &BlockSnapshot) -> BlockSnapshot {
        BlockSnapshot {
            inner: Arc::clone(&snapshot.inner),
            source: self
                .source
                .scoped(snapshot.inner.captured.identity, snapshot.generation()),
        }
    }

    pub(crate) fn capture_changed(
        &self,
        key: &str,
        previous: &BlockSnapshot,
    ) -> Result<Option<BlockSnapshot>, SemanticError> {
        let capture = self.capture_reference(key)?;
        if capture.root_id() == previous.storage_root_id() {
            return Ok(None);
        }
        let root = DatabaseRoot::decode(&capture.root_id(), &self.read_object(capture.root_id())?)?;
        let captured = DatabaseValueRoot::from(&root);
        if captured == previous.inner.captured {
            return Ok(None);
        }
        let mut snapshot = self.open_captured_value(
            capture.root_id(),
            captured,
            &[capture.source_condition()],
            previous,
        )?;
        Arc::get_mut(&mut snapshot.inner)
            .unwrap()
            .publication_revision = Some(capture.source_revision());
        Ok(Some(snapshot))
    }

    /// Open a root already authenticated through a publication or exact receipt.
    /// Matching index/generation captures share their prior immutable prefix;
    /// metadata is derived only from authenticated canonical log records, never
    /// from a report caller's schema or transaction data. A physical boundary
    /// opens the target's complete current-format indexed base and recent tail.
    fn open_captured_value(
        &self,
        root_id: ObjectId,
        captured: DatabaseValueRoot,
        conditions: &[RefCondition],
        previous: &BlockSnapshot,
    ) -> Result<BlockSnapshot, SemanticError> {
        if captured.identity != previous.inner.captured.identity
            || captured.basis < previous.basis_t()
        {
            return Err(fault(
                "storage/observation-lineage",
                "Live observation cannot change identity or move backwards",
            ));
        }
        let metadata_id = captured
            .metadata
            .ok_or_else(|| fault("storage/read-metadata", "Observed root has no metadata"))?;
        let metadata = SnapshotMetadata::decode(&metadata_id, &self.read_object(metadata_id)?)?;
        if captured.indexes != previous.inner.captured.indexes
            || metadata.generation != previous.generation()
            || captured.log.is_none()
        {
            // A genesis endpoint without a log also uses its authenticated
            // indexed base; there is no suffix representation to extend.
            return BlockSnapshot::open_captured(
                self.source.clone(),
                root_id,
                captured,
                route_from_conditions(conditions),
                self.limits.clone(),
            );
        }
        if metadata.identity != captured.identity || metadata.basis != captured.basis {
            return Err(fault(
                "storage/observation-metadata",
                "Observed metadata disagrees with its root",
            ));
        }
        if captured.basis - previous.indexed_basis_t() > self.limits.max_recent_transactions as u64
        {
            return Err(limit(
                "Observed recent transaction window exceeds reader admission",
            ));
        }
        let (log, entries) = {
            let mut store = self.source.lock()?;
            let log = super::log::LogRoot::open(
                &mut store,
                captured
                    .log
                    .ok_or_else(|| fault("storage/read-log", "New observations require a log"))?,
            )?;
            if log.basis_t() != captured.basis
                || log.eidx_frontier() != metadata.eidx_frontier
                || log.reserved_frontier() != metadata.reserved_frontier
            {
                return Err(fault(
                    "storage/observation-log",
                    "Observed log endpoint disagrees with metadata",
                ));
            }
            // The new suffix must extend this exact captured prefix. Do
            // not synthesize continuity merely by assigning previous_hash
            // while constructing the in-memory transaction wrappers.
            if let Some(previous_log) = &previous.inner.log
                && !log.extends_prefix(&mut store, previous_log)?
            {
                return Err(fault(
                    "storage/observation-prefix",
                    "Observed log does not extend the captured transaction prefix",
                ));
            }
            let mut range = log.range(&mut store, previous.basis_t() + 1, captured.basis + 1)?;
            let mut entries = Vec::new();
            let mut hash = previous.inner.recent.stats().end_hash;
            let mut retained = previous.inner.recent.stats().accounted_bytes;
            let mut datoms = previous.inner.recent.stats().datoms;
            while let Some(record) = range.next_record() {
                let record = record?;
                let transaction = DurableTransaction {
                    database_id: previous.inner.lineage.clone(),
                    basis_t: record.entry.basis_t,
                    previous_hash: hash,
                    eidx_frontier: record.entry.eidx_frontier,
                    tempids: BTreeMap::new(),
                    tx_data: record.entry.tx_data,
                };
                retained = retained.saturating_add(
                    crate::index::recent::retained_entry_stats(&transaction)?.accounted_bytes,
                );
                datoms = datoms.saturating_add(transaction.tx_data.len() as u64);
                if retained > self.limits.max_recent_bytes as u64
                    || datoms > self.limits.max_recent_datoms as u64
                {
                    return Err(limit("Observed recent payload exceeds reader admission"));
                }
                hash = record.id;
                entries.push((record.id, transaction));
            }
            drop(range);
            (log, entries)
        };
        if entries.len() as u64 != captured.basis - previous.basis_t() {
            return Err(fault(
                "storage/observation-gap",
                "Observed log delta is incomplete",
            ));
        }
        let (hashes, transactions): (Vec<_>, Vec<_>) = entries.into_iter().unzip();
        let (endpoint, unready) = crate::index::metadata::apply_metadata_and_avet_readiness(
            previous.endpoint_metadata(),
            &previous.inner.avet_unready,
            &transactions,
            |attribute| {
                previous
                    .prefix_cursor(
                        true,
                        &IndexPrefix::Aevt {
                            attribute,
                            entity: None,
                            value: None,
                        },
                    )?
                    .next()
                    .transpose()
                    .map(|datom| datom.is_some())
            },
        )?;
        let recent = previous.inner.recent.extend_authenticated(
            hashes.into_iter().zip(transactions),
            EndpointProjection::from_schema(Arc::clone(&endpoint.schema)),
        )?;
        let metadata_residency = endpoint.resident_stats();
        Ok(BlockSnapshot {
            source: self.source.scoped(captured.identity, metadata.generation),
            inner: Arc::new(SnapshotInner {
                captured,
                metadata,
                storage_root: root_id,
                publication_revision: None,
                route: route_from_conditions(conditions),
                indexes: Arc::clone(&previous.inner.indexes),
                base_metadata: Arc::clone(&previous.inner.base_metadata),
                schema: Arc::clone(&endpoint.schema),
                idents: Arc::clone(&endpoint.idents),
                endpoint_metadata: Arc::new(endpoint),
                lineage: previous.inner.lineage.clone(),
                roots: previous.inner.roots.clone(),
                recent,
                log: Some(log),
                avet_unready: unready,
                metadata_residency,
                root_residency: previous.inner.root_residency,
                limits: self.limits.clone(),
            }),
        })
    }

    pub(crate) fn exact_report(
        &self,
        reference_key: &str,
        basis: u64,
    ) -> Result<crate::ServiceTransactionReport, SemanticError> {
        let capture = self.capture_reference(reference_key)?;
        self.exact_report_from_capture(&capture, basis)
    }

    pub(crate) fn exact_report_from_capture(
        &self,
        capture: &RootCapture,
        basis: u64,
    ) -> Result<crate::ServiceTransactionReport, SemanticError> {
        let operation = crate::OperationContext::current_or_process();
        let _report_phase = operation.phase(crate::OperationKind::TransactionReport);
        let root_id = capture.root_id();
        let root = DatabaseRoot::decode(&root_id, &self.read_object(root_id)?)?;
        let receipt = self.exact_receipt_from_root(&root, basis)?;
        self.report_from_receipt(capture, receipt, false)
    }

    /// Reconstruct an ordered report batch from one immutable publication.
    /// Each exact after-value supplies the next receipt's before-value only
    /// through its authenticated root. Sharing the reader's incremental capture
    /// avoids replaying every earlier recent tail for every report. No partial
    /// batch escapes if any receipt, endpoint, or log-prefix check fails.
    pub(crate) fn exact_reports_from_capture(
        &self,
        capture: &RootCapture,
        after_basis: u64,
        through_basis: u64,
        previous: Option<&BlockSnapshot>,
    ) -> Result<Vec<crate::ServiceTransactionReport>, SemanticError> {
        if after_basis > through_basis {
            return Err(fault(
                "storage/report-basis",
                "Report batch has inverted transaction boundaries",
            ));
        }
        if after_basis == through_basis {
            return Ok(Vec::new());
        }
        let operation = crate::OperationContext::current_or_process();
        let _report_phase = operation.phase(crate::OperationKind::TransactionReport);
        let root_id = capture.root_id();
        let root = DatabaseRoot::decode(&root_id, &self.read_object(root_id)?)?;
        if through_basis > root.basis {
            return Err(fault(
                "storage/report-basis",
                "Report batch is outside the captured log",
            ));
        }
        let conditions = [capture.source_condition()];
        let mut previous = previous.cloned();
        let mut reports = Vec::new();
        let mut basis = after_basis;
        while basis < through_basis {
            basis += 1;
            let receipt = self.exact_receipt_from_root(&root, basis)?;
            let before = match previous.as_ref() {
                Some(previous) => {
                    self.capture_immutable_after(receipt.before, &conditions, previous)?
                }
                None => self.capture_immutable(receipt.before, &conditions)?,
            };
            let after = self.capture_immutable_after(receipt.after, &conditions, &before)?;
            let report =
                self.report_from_captured_receipt(receipt, false, before, after.clone())?;
            previous = Some(after);
            reports.push(report);
        }
        Ok(reports)
    }

    /// The immutable publication, not a supplied endpoint or basis alone,
    /// authenticates receipt membership and its canonical log transaction.
    fn exact_receipt_from_root(
        &self,
        root: &DatabaseRoot,
        basis: u64,
    ) -> Result<super::receipts::ExactReceipt, SemanticError> {
        if basis == 0 || basis > root.basis {
            return Err(fault(
                "storage/report-basis",
                "Report is outside the captured log",
            ));
        }
        let receipt = {
            let mut store = self.source.lock()?;
            let id = super::receipts::RequestIndex::from_root(root.receipts)
                .lookup(
                    &mut store,
                    super::receipts::basis_receipt_key(&root.identity, basis),
                )?
                .ok_or_else(|| {
                    SemanticError::new(
                        ErrorCategory::NotFound,
                        "storage/report-unavailable",
                        "Exact outcome is not retained for this log position",
                    )
                })?;
            super::receipts::ExactReceipt::load(&mut store, id)?
        };
        if receipt.identity != root.identity || receipt.basis != basis {
            return Err(fault(
                "storage/report-identity",
                "Report index points to a different outcome",
            ));
        }
        let head = root.log.ok_or_else(|| {
            fault(
                "storage/report-source",
                "Report publication has no transaction log",
            )
        })?;
        if self
            .log_record(head, basis)?
            .is_none_or(|record| record.id != receipt.transaction)
        {
            return Err(fault(
                "storage/report-source",
                "Exact report transaction differs from the captured publication log",
            ));
        }
        Ok(receipt)
    }

    pub(crate) fn report_from_receipt(
        &self,
        capture: &RootCapture,
        receipt: super::receipts::ExactReceipt,
        replayed: bool,
    ) -> Result<crate::ServiceTransactionReport, SemanticError> {
        let mut before = self.capture_immutable(receipt.before, &[capture.source_condition()])?;
        let mut after = self.capture_immutable(receipt.after, &[capture.source_condition()])?;
        let route = route_from_conditions(&[capture.source_condition()]);
        Arc::get_mut(&mut before.inner).unwrap().route = route.clone();
        Arc::get_mut(&mut after.inner).unwrap().route = route;
        self.report_from_captured_receipt(receipt, replayed, before, after)
    }

    fn report_from_captured_receipt(
        &self,
        receipt: super::receipts::ExactReceipt,
        replayed: bool,
        before: BlockSnapshot,
        after: BlockSnapshot,
    ) -> Result<crate::ServiceTransactionReport, SemanticError> {
        if before.captured_root().identity != receipt.identity
            || after.captured_root().identity != receipt.identity
            || before.basis_t().checked_add(1) != Some(receipt.basis)
            || after.basis_t() != receipt.basis
        {
            return Err(fault(
                "storage/receipt-coordinate",
                "Receipt bases do not describe one transaction",
            ));
        }
        let record = after.read_log_record(receipt.basis)?;
        if record.id != receipt.transaction {
            return Err(fault(
                "storage/receipt-transaction",
                "Receipt transaction differs from its exact after log",
            ));
        }
        super::receipts::validate_receipt_frontiers(
            &receipt.tempids,
            receipt.basis,
            record.entry.eidx_frontier,
            record.entry.reserved_frontier,
        )?;
        Ok(crate::ServiceTransactionReport {
            db_before: before.database_value(),
            db_after: after.database_value(),
            basis_t: receipt.basis,
            tx_hash: receipt.transaction,
            tx_data: record.entry.tx_data,
            tempids: receipt.tempids,
            replayed,
            diagnostics: None,
        })
    }

    pub(crate) fn resolve_request_outcome(
        &self,
        database: &super::BlockDatabase,
        request_key: &str,
        digest: Digest,
    ) -> Result<Option<crate::ServiceTransactionReport>, SemanticError> {
        let key = super::receipts::scoped_request_key(&database.identity, request_key)?;
        let capture = self.capture_reference(&database.reference_key())?;
        let root = DatabaseRoot::decode_for_identity(
            &capture.root_id(),
            &self.read_object(capture.root_id())?,
            &database.identity,
        )?;
        let id = super::receipts::RequestIndex::from_root(root.receipts)
            .lookup(&mut self.source.lock()?, key)?;
        id.map(|id| self.replay_receipt(&capture, id, digest, database.identity))
            .transpose()
    }

    pub(crate) fn replay_receipt(
        &self,
        capture: &RootCapture,
        id: ObjectId,
        digest: Digest,
        identity: [u8; 16],
    ) -> Result<crate::ServiceTransactionReport, SemanticError> {
        // Every caller, including post-disconnect outcome reconciliation, uses
        // this shared reconstruction path. Attribute its actual reads and wall
        // time here rather than only in the writer's submission adapter.
        let operation = crate::OperationContext::current_or_process();
        let _report_phase = operation.phase(crate::OperationKind::TransactionReport);
        let bytes = self.read_object(id)?;
        super::excision::reject_tombstone_bytes(id, &bytes, &identity)?;
        let receipt =
            super::receipts::ExactReceipt::load_from_bytes(&mut self.source.lock()?, id, &bytes)?;
        if receipt.identity != identity {
            return Err(fault(
                "storage/receipt-identity",
                "Receipt belongs to a different database",
            ));
        }
        if receipt.request_digest != digest {
            return Err(SemanticError::conflict(
                "postgres/idempotency-key-reused",
                "Request key is already bound to different transaction data",
            ));
        }
        self.report_from_receipt(capture, receipt, true)
    }

    /// Read an exact immutable value reached through a trusted publication or
    /// receipt. Mutation callers separately guard their final publication.
    pub(crate) fn capture_immutable(
        &self,
        root_id: ObjectId,
        conditions: &[RefCondition],
    ) -> Result<BlockSnapshot, SemanticError> {
        let bytes = self.read_object(root_id)?;
        BlockSnapshot::open_captured(
            self.source.clone(),
            root_id,
            decode_captured_root(root_id, &bytes)?,
            route_from_conditions(conditions),
            self.limits.clone(),
        )
    }

    /// The receipt's exact root link, not logical SnapshotKey equality, permits
    /// reuse. A different root is decoded and opened against its own physical
    /// index/generation coordinates, sharing only an authenticated log prefix.
    fn capture_immutable_after(
        &self,
        root_id: ObjectId,
        conditions: &[RefCondition],
        previous: &BlockSnapshot,
    ) -> Result<BlockSnapshot, SemanticError> {
        if root_id == previous.storage_root_id()
            && previous.publication_revision().is_none()
            && previous.inner.route == route_from_conditions(conditions)
        {
            let recent = previous.inner.recent.stats();
            if recent.transactions > self.limits.max_recent_transactions as u64
                || recent.datoms > self.limits.max_recent_datoms as u64
                || recent.accounted_bytes > self.limits.max_recent_bytes as u64
            {
                return Err(limit("Captured recent data exceeds reader admission"));
            }
            return Ok(self.adopt_snapshot(previous));
        }
        let bytes = self.read_object(root_id)?;
        self.open_captured_value(
            root_id,
            decode_captured_root(root_id, &bytes)?,
            conditions,
            previous,
        )
    }

    /// Reopen an untrusted serialized reference only through committed engine
    /// authority. The trusted publication identifies its canonical values and
    /// authorized indexes. A reader needs no immutable-object write privilege
    /// to hold their decoded value wrapper in memory.
    pub(crate) fn capture_authorized(
        &self,
        publication: &DatabaseRoot,
        capture: &RootCapture,
        candidate: &DatabaseValueRoot,
    ) -> Result<BlockSnapshot, SemanticError> {
        {
            let mut store = self.source.lock()?;
            super::read_authorization::authorize_value(&mut store, publication, candidate)?;
        }
        BlockSnapshot::open_captured(
            self.source.clone(),
            candidate.id()?,
            candidate.clone(),
            route_from_conditions(&[capture.source_condition()]),
            self.limits.clone(),
        )
    }

    /// Advance a validated writer value by path-copying recent indexes. The
    /// caller has already durably stored/authenticated this exact log entry and
    /// root; no current publication or prior transaction tail is reread here.
    #[allow(clippy::too_many_arguments)] // The arguments bind one authenticated commit and its capture guards.
    pub(crate) fn capture_successor(
        &self,
        before: &BlockSnapshot,
        root_id: ObjectId,
        captured: DatabaseValueRoot,
        metadata: SnapshotMetadata,
        log: super::log::LogRoot,
        entry_id: ObjectId,
        tx_data: Vec<Datom>,
        _conditions: &[RefCondition],
    ) -> Result<BlockSnapshot, SemanticError> {
        if captured.identity != before.inner.captured.identity
            || captured.basis != before.basis_t().saturating_add(1)
            || captured.indexes != before.inner.captured.indexes
            || captured.log != log.head()
            || captured.basis != log.basis_t()
            || metadata.identity != captured.identity
            || metadata.basis != captured.basis
            || metadata.generation != before.generation()
            || metadata.eidx_frontier != log.eidx_frontier()
            || metadata.reserved_frontier != log.reserved_frontier()
            || log.latest_entry_id() != Some(entry_id)
        {
            return Err(fault(
                "storage/successor-coordinates",
                "Successor does not extend its captured value",
            ));
        }
        if captured.basis - before.indexed_basis_t() > self.limits.max_recent_transactions as u64 {
            return Err(limit(
                "Proposed recent transaction window exceeds reader admission",
            ));
        }
        let transaction = DurableTransaction {
            database_id: before.inner.lineage.clone(),
            basis_t: captured.basis,
            previous_hash: before.inner.recent.stats().end_hash,
            eidx_frontier: metadata.eidx_frontier,
            tempids: BTreeMap::new(),
            tx_data,
        };
        let (endpoint, unready) = crate::index::metadata::apply_metadata_and_avet_readiness(
            &before.inner.endpoint_metadata,
            &before.inner.avet_unready,
            std::slice::from_ref(&transaction),
            |attribute| {
                // A schema change can require history from either durable or
                // recent tiers, not merely from the old durable base.
                let mut cursor = before.prefix_cursor(
                    true,
                    &IndexPrefix::Aevt {
                        attribute,
                        entity: None,
                        value: None,
                    },
                )?;
                Ok(cursor.next().transpose()?.is_some())
            },
        )?;
        let recent = before.inner.recent.extend_authenticated(
            [(entry_id, transaction)],
            EndpointProjection::from_schema(Arc::clone(&endpoint.schema)),
        )?;
        if recent.stats().datoms > self.limits.max_recent_datoms as u64
            || recent.stats().accounted_bytes > self.limits.max_recent_bytes as u64
        {
            return Err(limit("Proposed recent data exceeds reader admission"));
        }
        let metadata_residency = endpoint.resident_stats();
        Ok(BlockSnapshot {
            source: before.source.clone(),
            inner: Arc::new(SnapshotInner {
                captured,
                storage_root: root_id,
                publication_revision: None,
                route: before.inner.route.clone(),
                metadata,
                indexes: Arc::clone(&before.inner.indexes),
                base_metadata: Arc::clone(&before.inner.base_metadata),
                schema: Arc::clone(&endpoint.schema),
                idents: Arc::clone(&endpoint.idents),
                endpoint_metadata: Arc::new(endpoint),
                lineage: before.inner.lineage.clone(),
                roots: before.inner.roots.clone(),
                recent,
                log: Some(log),
                avet_unready: unready,
                metadata_residency,
                root_residency: before.inner.root_residency,
                limits: self.limits.clone(),
            }),
        })
    }

    pub fn cache_stats(&self) -> CacheStats {
        self.source.cache.stats()
    }
    pub(crate) fn read_object(&self, id: ObjectId) -> Result<Vec<u8>, SemanticError> {
        self.source.read(id)
    }
    pub(crate) fn log_record(
        &self,
        head: ObjectId,
        t: u64,
    ) -> Result<Option<super::log::LogRecord>, SemanticError> {
        let mut store = self.source.lock()?;
        super::log::LogRoot::open(&mut store, head)?.read_record(&mut store, t)
    }
    pub fn read_stats(&self) -> BlockReadStats {
        *self.source.stats.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl BlockSnapshot {
    /// Open the repository's independently prepared, same-format covering
    /// value. The manifest binds it to the captured publication; no live root,
    /// writer authority, session or PostgreSQL connection is manufactured.
    pub(crate) fn open_repository(
        directory: &Path,
        point: &crate::backup::ReadPoint,
        limits: BlockReadConfig,
    ) -> Result<Self, SemanticError> {
        let source = BlockNodeSource {
            programs: Default::default(),
            backend: SourceBackend::Repository(Arc::new(directory.to_path_buf())),
            cache: TreeNodeCache::new(limits.cache_entries, limits.cache_bytes),
            fulltext_cache: crate::fulltext::store::FulltextCache::new(
                limits.cache_entries,
                limits.cache_bytes,
            ),
            ssd_cache: crate::SsdCache::disabled(),
            ssd_namespace: [0; 32],
            ssd_scope: None,
            stats: Arc::new(Mutex::new(BlockReadStats {
                object_reads: point.open_object_reads,
                object_bytes: point.open_object_bytes,
                ..BlockReadStats::default()
            })),
            node_stats: Arc::new(Mutex::new(crate::NodeBlockReadStats::default())),
            cursor_stats: Arc::new(Mutex::new(crate::PeerLoadStats::default())),
        };
        let publication =
            DatabaseRoot::decode(&point.publication, &source.read(point.publication)?)?;
        let value = &point.value;
        if value.identity != publication.identity
            || value.basis != publication.basis
            || value.log != publication.log
            || value.metadata != publication.metadata
        {
            return Err(fault(
                "backup/read-value",
                "Repository read value differs from its captured publication",
            ));
        }
        Self::open_captured(source, value.id()?, value.clone(), None, limits)
    }
    fn open_captured(
        source: BlockNodeSource,
        root_id: ObjectId,
        captured: DatabaseValueRoot,
        route: Option<Arc<str>>,
        limits: BlockReadConfig,
    ) -> Result<Self, SemanticError> {
        let metadata_id = captured.metadata.ok_or_else(|| {
            fault(
                "storage/read-metadata",
                "Database root has no read metadata",
            )
        })?;
        let metadata = SnapshotMetadata::decode(&metadata_id, &source.read(metadata_id)?)?;
        let indexes_id = captured.indexes.ok_or_else(|| {
            fault(
                "storage/read-indexes",
                "Database root has no indexed read base",
            )
        })?;
        let indexes = IndexDescriptor::decode(&indexes_id, &source.read(indexes_id)?)?;
        if metadata.identity != captured.identity
            || metadata.basis != captured.basis
            || indexes.identity != captured.identity
            || indexes.generation != metadata.generation
            || indexes.basis > captured.basis
        {
            return Err(fault(
                "storage/read-coordinates",
                "Read descriptors disagree with their captured root",
            ));
        }
        let source = source.scoped(captured.identity, metadata.generation);
        if captured.basis - indexes.basis > limits.max_recent_transactions as u64 {
            return Err(limit(
                "Captured recent transaction window exceeds reader admission",
            ));
        }
        let mut roots = BTreeMap::new();
        for descriptor in &indexes.trees {
            let node = source.load_node(descriptor.root_hash, &mut TreeReadStats::default())?;
            let TreeNode::Root(root) = node.as_ref() else {
                return Err(fault(
                    "storage/read-tree-kind",
                    "Index descriptor does not name a tree root",
                ));
            };
            if root.order != descriptor.order
                || root.history != descriptor.history
                || root.count != descriptor.count
                || (root.count == 0) != root.directories.is_empty()
                || root
                    .directories
                    .first()
                    .map(|child| crate::index::tree::routing_key_hash(&child.key))
                    .transpose()?
                    != descriptor.first_hash
            {
                return Err(fault(
                    "storage/read-tree-content",
                    "Index descriptor differs from its immutable root",
                ));
            }
            roots.insert(
                (root.history, order_tag(root.order)),
                Arc::new(root.clone()),
            );
        }
        let base = crate::index::metadata::derive_metadata_from_roots(&roots, |hash| {
            source.load_node(hash, &mut TreeReadStats::default())
        })?;
        let lineage = format_identity(captured.identity);
        let mut transactions = Vec::new();
        let mut hashes = Vec::new();
        let mut tail_bytes = 0u64;
        let mut tail_datoms = 0usize;
        let mut previous = [0u8; 32];
        let mut captured_log = None;
        if let Some(log_id) = captured.log {
            let mut store = |id| source.read(id);
            let log = super::log::LogRoot::open(&mut store, log_id)?;
            if log.basis_t() != captured.basis
                || log.eidx_frontier() != metadata.eidx_frontier
                || log.reserved_frontier() != metadata.reserved_frontier
            {
                return Err(fault(
                    "storage/read-log-basis",
                    "Log endpoint differs from captured basis/frontiers",
                ));
            }
            let end = captured
                .basis
                .checked_add(1)
                .ok_or_else(|| fault("storage/read-log-basis", "Log endpoint overflow"))?;
            let mut range = log.range(&mut store, indexes.basis + 1, end)?;
            while let Some(record) = range.next_record() {
                let record = record?;
                let entry = record.entry;
                tail_datoms = tail_datoms.saturating_add(entry.tx_data.len());
                if tail_datoms > limits.max_recent_datoms {
                    return Err(limit(
                        "Captured recent datom window exceeds reader admission",
                    ));
                }
                let transaction = DurableTransaction {
                    database_id: lineage.clone(),
                    basis_t: entry.basis_t,
                    previous_hash: previous,
                    eidx_frontier: entry.eidx_frontier,
                    tempids: BTreeMap::new(),
                    tx_data: entry.tx_data,
                };
                let input_bytes = transaction
                    .tx_data
                    .iter()
                    .fold(record.encoded_bytes, |n, d| {
                        n.saturating_add(d.retained_bytes())
                    });
                if input_bytes > (limits.max_recent_bytes as u64).saturating_sub(tail_bytes) {
                    return Err(limit(
                        "Captured recent byte window exceeds reader admission",
                    ));
                }
                let retained = crate::index::recent::retained_entry_stats(&transaction)?;
                tail_bytes = tail_bytes.saturating_add(retained.accounted_bytes);
                if tail_bytes > limits.max_recent_bytes as u64 {
                    return Err(limit(
                        "Captured recent byte window exceeds reader admission",
                    ));
                }
                // Preserve actual authenticated object identities while the
                // adapter supplies the old recent-tier predecessor field.
                previous = record.id;
                hashes.push(record.id);
                transactions.push(transaction);
            }
            drop(range);
            captured_log = Some(log);
        }
        if transactions.len() as u64 != captured.basis - indexes.basis {
            return Err(fault(
                "storage/read-log-gap",
                "Captured read tail is incomplete",
            ));
        }
        if let Some(last) = transactions.last()
            && last.eidx_frontier != metadata.eidx_frontier
        {
            return Err(fault(
                "storage/read-log-frontier",
                "Log frontier differs from captured metadata",
            ));
        }
        let initial = Arc::new(indexes.pending_avet.iter().copied().collect());
        let (endpoint, unready) = crate::index::metadata::apply_metadata_and_avet_readiness(
            &base,
            &initial,
            &transactions,
            |attribute| {
                let prefix = IndexPrefix::Aevt {
                    attribute,
                    entity: None,
                    value: None,
                };
                let mut cursor = DurableTreeCursor::new_prefix(
                    source.clone(),
                    Arc::clone(&roots[&(true, order_tag(IndexOrder::Aevt))]),
                    true,
                    prefix,
                );
                Ok(cursor.next_datom()?.is_some())
            },
        )?;
        let recent = RecentTier::new_authenticated(
            lineage.clone(),
            indexes.basis,
            [0; 32],
            hashes.into_iter().zip(transactions),
            EndpointProjection::from_schema(Arc::clone(&endpoint.schema)),
            RecentLimits {
                soft_datoms: limits.max_recent_datoms.max(1) as u64,
                hard_datoms: limits.max_recent_datoms.max(1) as u64,
                soft_bytes: limits.max_recent_bytes.max(1) as u64,
                hard_bytes: limits.max_recent_bytes.max(1) as u64,
            },
        )?;
        // Compute the metadata/root estimates during construction, never by
        // walking a wide schema or routing table on a service statistics read.
        let metadata_residency = endpoint.resident_stats();
        let root_residency = roots.values().fold(
            crate::index::metadata::ResidentTreeRootStats::default(),
            |mut total, root| {
                total.children = total.children.saturating_add(root.directories.len());
                total.estimated_bytes = total
                    .estimated_bytes
                    .saturating_add(root.estimated_retained_bytes());
                total
            },
        );
        Ok(Self {
            source,
            inner: Arc::new(SnapshotInner {
                captured,
                storage_root: root_id,
                publication_revision: None,
                route,
                metadata,
                indexes: Arc::new(indexes),
                base_metadata: Arc::new(base),
                schema: Arc::clone(&endpoint.schema),
                idents: Arc::clone(&endpoint.idents),
                endpoint_metadata: Arc::new(endpoint),
                lineage,
                roots,
                recent,
                log: captured_log,
                avet_unready: unready,
                metadata_residency,
                root_residency,
                limits,
            }),
        })
    }

    /// Address used by the live catalog, distinct from the immutable lineage.
    /// A portable repository or unpublished build has no live catalog route.
    pub fn route_id(&self) -> Option<&str> {
        self.inner.route.as_deref()
    }
    pub(crate) fn is_repository(&self) -> bool {
        matches!(self.source.backend, SourceBackend::Repository(_))
    }
    pub fn captured_root(&self) -> &DatabaseValueRoot {
        &self.inner.captured
    }
    /// Descriptor identity, not proof that its wrapper is currently stored.
    /// Authorized serialized values can retain this fixed wrapper in memory.
    pub(crate) fn storage_root_id(&self) -> ObjectId {
        self.inner.storage_root
    }
    pub(crate) fn publication_revision(&self) -> Option<u64> {
        self.inner.publication_revision
    }
    pub(crate) fn index_descriptor(&self) -> &IndexDescriptor {
        &self.inner.indexes
    }
    pub(crate) fn base_metadata(&self) -> &crate::index::metadata::MetadataProjection {
        &self.inner.base_metadata
    }
    pub(crate) fn endpoint_metadata(&self) -> &crate::index::metadata::MetadataProjection {
        &self.inner.endpoint_metadata
    }
    pub(crate) fn recent_tier(&self) -> &RecentTier {
        &self.inner.recent
    }
    pub(crate) fn read_object(&self, hash: ObjectId) -> Result<Vec<u8>, SemanticError> {
        self.source.read(hash)
    }
    pub(crate) fn fulltext_cache(&self) -> crate::fulltext::store::FulltextCache {
        self.source.fulltext_cache.clone()
    }
    /// Advisory work owns a separate driver, while immutable metadata, recent
    /// indexes and bounded caches remain shared. No writer I/O mutex can
    /// be occupied by this returned reader's cold requests.
    pub(crate) fn fork_for_hints(
        &self,
        timeout: std::time::Duration,
    ) -> Result<Self, SemanticError> {
        if timeout.is_zero() {
            return Err(SemanticError::incorrect(
                "peer/hint-timeout",
                "Hint timeout must be positive",
            ));
        }
        let live = self.source.live()?;
        let mut policy = live.connection.io_policy().clone();
        policy.connect_timeout = Some(
            policy
                .connect_timeout
                .map_or(timeout, |old| old.min(timeout)),
        );
        policy.statement_timeout = Some(
            policy
                .statement_timeout
                .map_or(timeout, |old| old.min(timeout)),
        );
        policy.lock_timeout = Some(policy.lock_timeout.map_or(timeout, |old| old.min(timeout)));
        // Advisory deadlines cannot bound local filesystem calls. Preserve the
        // existing hint policy: only RAM caches and independent bounded SQL.
        let connection = live
            .connection
            .clone()
            .without_ssd_cache()
            .with_io_policy(policy)?;
        let reader = BlockReader::connect(&connection, self.inner.limits.clone())?;
        let mut source = reader
            .source
            .scoped(self.inner.captured.identity, self.generation());
        source.cache = self.source.cache.clone();
        source.fulltext_cache = self.source.fulltext_cache.clone();
        source.ssd_cache = crate::SsdCache::disabled();
        source.ssd_namespace = self.source.ssd_namespace;
        source.stats = self.source.stats.clone();
        source.node_stats = self.source.node_stats.clone();
        source.cursor_stats = self.source.cursor_stats.clone();
        Ok(Self {
            inner: Arc::clone(&self.inner),
            source,
        })
    }
    pub(crate) fn captured_metadata(&self) -> &SnapshotMetadata {
        &self.inner.metadata
    }
    pub(crate) fn captured_log(&self) -> Option<&super::log::LogRoot> {
        self.inner.log.as_ref()
    }
    pub(crate) fn indexed_basis_t(&self) -> u64 {
        self.inner.recent.stats().base_t
    }
    /// Current block-reader residency mapped onto the service's existing
    /// diagnostics type. No old commitment/manifest search is performed. The
    /// publication revision and last-transaction work belong to the writer and
    /// remain zero here; an exact immutable ValueRoot has no CAS revision.
    pub(crate) fn residency_stats(&self) -> crate::WriterResidencyStats {
        let recent = self.inner.recent.stats();
        let cache = self.cache_stats();
        let reads = self.read_stats();
        let metadata = self.inner.metadata_residency;
        let roots = self.inner.root_residency;
        crate::WriterResidencyStats {
            recent_datoms: recent.datoms,
            recent_accounted_bytes: recent.accounted_bytes,
            tree_cache_entries: cache.current_entries,
            tree_cache_bytes: cache.current_bytes,
            resident_tree_root_children: roots.children,
            resident_tree_root_estimated_bytes: roots.estimated_bytes,
            resident_schema_attributes: metadata.schema_attributes,
            resident_schema_information_datoms: metadata.schema_information_datoms,
            resident_schema_estimated_bytes: metadata.schema_estimated_bytes,
            resident_ident_names: metadata.ident_names,
            resident_ident_entities: metadata.ident_entities,
            resident_ident_estimated_bytes: metadata.ident_estimated_bytes,
            native_root_reads: reads.tree.root_reads,
            native_directory_reads: reads.tree.directory_reads,
            native_leaf_reads: reads.tree.leaf_reads,
            ..crate::WriterResidencyStats::default()
        }
    }
    pub fn basis_t(&self) -> u64 {
        self.inner.captured.basis
    }
    pub fn generation(&self) -> u64 {
        self.inner.metadata.generation
    }
    pub fn database_value(&self) -> DatabaseValue {
        DatabaseValue::block(self.clone())
    }
    /// Capture a lazy transaction log from the same immutable value root.
    pub fn log(&self) -> crate::LogValue {
        crate::LogValue::from_block(self.clone())
    }
    pub(crate) fn read_log_record(&self, t: u64) -> Result<super::log::LogRecord, SemanticError> {
        self.read_log_record_measured(t)
            .map(|(record, _, _)| record)
    }
    pub(crate) fn read_log_record_measured(
        &self,
        t: u64,
    ) -> Result<(super::log::LogRecord, u64, bool), SemanticError> {
        if t == 0 || t > self.basis_t() {
            return Err(SemanticError::incorrect(
                "storage/read-log-coordinate",
                "Transaction is outside the captured log",
            ));
        }
        let log = self.inner.log.as_ref().ok_or_else(|| {
            fault(
                "storage/read-log-missing",
                "Captured nonempty value has no log",
            )
        })?;
        self.measure_log_read(|read| {
            log.read_record(read, t)?.ok_or_else(|| {
                fault(
                    "storage/read-log-gap",
                    "Captured log is missing a transaction",
                )
            })
        })
    }

    pub(crate) fn next_log_record_measured(
        &self,
        traversal: &mut super::log::LogTraversal,
    ) -> Option<Result<(super::log::LogRecord, u64, bool), SemanticError>> {
        // The caller owns forward navigation, while this captured source owns
        // generation-scoped cache and authenticated object I/O. No mutable head
        // lookup, writer lock, or per-transaction restart of the seek path.
        self.measure_log_read(|read| traversal.next_record(read).transpose())
            .map(|(record, bytes, cached)| record.map(|record| (record, bytes, cached)))
            .transpose()
    }

    fn measure_log_read<T>(
        &self,
        read_record: impl FnOnce(&mut dyn super::ObjectReader) -> Result<T, SemanticError>,
    ) -> Result<(T, u64, bool), SemanticError> {
        let mut postgres_bytes = 0u64;
        let mut all_cached = !self.is_repository();
        let mut read = |id| {
            let (bytes, stats) = self.source.read_with_stats(id)?;
            if let Some(stats) = stats {
                all_cached = false;
                postgres_bytes = postgres_bytes.saturating_add(stats.canonical_bytes);
            }
            Ok(bytes)
        };
        let record = read_record(&mut read)?;
        Ok((record, postgres_bytes, all_cached))
    }
    pub fn cache_stats(&self) -> CacheStats {
        self.source.cache.stats()
    }
    pub fn read_stats(&self) -> BlockReadStats {
        *self.source.stats.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub(crate) fn lineage_id(&self) -> &str {
        &self.inner.lineage
    }
    pub(crate) fn eidx_frontier(&self) -> u64 {
        self.inner.metadata.eidx_frontier
    }
    pub(crate) fn reserved_allocation(
        &self,
    ) -> Option<crate::reserved_allocation::ReservedAllocation> {
        Some(
            crate::reserved_allocation::ReservedAllocation::from_frontier(
                self.inner.metadata.reserved_frontier,
                self.inner.metadata.eidx_frontier,
            )
            .expect("validated metadata"),
        )
    }
    pub(crate) fn last_tx_instant(&self) -> Option<i64> {
        self.inner.metadata.last_tx_instant
    }
    pub(crate) fn schema(&self) -> &Schema {
        &self.inner.schema
    }
    pub(crate) fn schema_arc(&self) -> Arc<Schema> {
        Arc::clone(&self.inner.schema)
    }
    pub(crate) fn entid(&self, ident: &Keyword) -> Option<u64> {
        self.inner.idents.resolve(ident)
    }
    pub(crate) fn ident(&self, entity: u64) -> Option<&Keyword> {
        self.inner.idents.ident(entity)
    }
    pub(crate) fn avet_ready(&self, attribute: u32) -> bool {
        !self.inner.avet_unready.contains(&attribute)
    }
    fn root(&self, history: bool, order: IndexOrder) -> Arc<RootNode> {
        Arc::clone(&self.inner.roots[&(history, order_tag(order))])
    }
    pub(crate) fn cursor(&self, history: bool, order: IndexOrder) -> BlockIndexCursor {
        self.range_cursor(history, order, None, None)
            .expect("unbounded range")
    }

    pub(crate) fn range_cursor(
        &self,
        history: bool,
        order: IndexOrder,
        start: Option<&Datom>,
        end: Option<&Datom>,
    ) -> Result<BlockIndexCursor, SemanticError> {
        crate::io_diagnostics::record_index_cursor(order);
        let range = RecentRange::new(start.cloned(), end.cloned());
        let recent = self.inner.recent.cursor(history, order, &range)?;
        let durable = DurableTreeCursor::new(
            self.clone(),
            self.root(history, order),
            history,
            order,
            start.cloned(),
            end.cloned(),
        );
        Ok(BlockIndexCursor {
            reads: Arc::clone(&self.source.stats),
            cursor: MergeCursor::new(durable, recent, history, order, false),
        })
    }
    pub(crate) fn prefix_cursor(
        &self,
        history: bool,
        prefix: &IndexPrefix,
    ) -> Result<BlockIndexCursor, SemanticError> {
        prefix.validate()?;
        if let IndexPrefix::Avet { attribute, .. } = prefix {
            self.ensure_avet_ready(*attribute)?;
        }
        crate::io_diagnostics::record_index_cursor(prefix.order());
        let durable = DurableTreeCursor::new_prefix(
            self.clone(),
            self.root(history, prefix.order()),
            history,
            prefix.clone(),
        );
        Ok(BlockIndexCursor {
            reads: Arc::clone(&self.source.stats),
            cursor: MergeCursor::new(
                durable,
                self.inner.recent.prefix_cursor(history, prefix)?,
                history,
                prefix.order(),
                false,
            ),
        })
    }
    pub(crate) fn boundary_cursor(
        &self,
        history: bool,
        boundary: &IndexBoundary,
        reverse: bool,
    ) -> Result<BlockIndexCursor, SemanticError> {
        if let Some(attribute) = boundary.avet_attribute() {
            self.ensure_avet_ready(attribute)?;
        }
        Ok(self.normalized_cursor(history, boundary.normalized()?, reverse))
    }
    pub(crate) fn normalized_cursor(
        &self,
        history: bool,
        boundary: NormalizedIndexBoundary,
        reverse: bool,
    ) -> BlockIndexCursor {
        let order = boundary.order();
        crate::io_diagnostics::record_index_cursor(order);
        let recent = if reverse {
            self.inner
                .recent
                .reverse_boundary_cursor(history, &boundary)
        } else {
            self.inner.recent.boundary_cursor(history, &boundary)
        };
        let durable = if reverse {
            DurableTreeCursor::new_reverse(
                self.clone(),
                self.root(history, order),
                history,
                boundary,
            )
        } else {
            DurableTreeCursor::new_forward_boundary(
                self.clone(),
                self.root(history, order),
                history,
                boundary,
            )
        };
        BlockIndexCursor {
            reads: Arc::clone(&self.source.stats),
            cursor: MergeCursor::new(durable, recent, history, order, reverse),
        }
    }
    fn ensure_avet_ready(&self, attribute: u32) -> Result<(), SemanticError> {
        if !self
            .schema()
            .attribute(attribute)
            .is_ok_and(|a| a.indexed || a.unique.is_some())
        {
            return Err(SemanticError::incorrect(
                "peer/avet-attribute-not-indexed",
                "Attribute has no AVET storage",
            ));
        }
        if !self.avet_ready(attribute) {
            return Err(SemanticError::new(
                ErrorCategory::Unavailable,
                "peer/avet-not-ready",
                "Captured AVET projection is incomplete",
            ));
        }
        Ok(())
    }
    pub(crate) fn resolve_program(
        &self,
        hash: crate::ProgramHash,
    ) -> Result<Arc<crate::program::ValidatedProgram>, SemanticError> {
        let key = (self.inner.captured.identity, self.generation(), hash);
        if let Some(program) = crate::program_cache::lock(&self.source.programs).get(key) {
            return Ok(program);
        }
        // Never retain a cache mutex across source I/O. Cold reads retain the
        // existing provider or repository authentication boundary.
        let payload = self.source.read(hash)?;
        if crate::sha256(&payload) != hash {
            return Err(fault(
                "storage/program-hash",
                "Program differs from its captured identity",
            ));
        }
        // decode_program already checks canonical re-encoding and validates
        // the IR; do not re-encode and validate it a third time for its hash.
        let program = crate::decode_program(&payload)?;
        let program = Arc::new(crate::program::ValidatedProgram::from_canonical(program));
        crate::program_cache::lock(&self.source.programs).insert_validated(
            key,
            Arc::clone(&program),
            payload.len(),
        );
        Ok(program)
    }
}

impl DurableTreeSource for BlockSnapshot {
    fn load_node(
        &self,
        hash: Digest,
        stats: &mut TreeReadStats,
    ) -> Result<Arc<TreeNode>, SemanticError> {
        self.source.load_node(hash, stats)
    }
}
impl MergeSource for BlockSnapshot {
    fn recent(&self) -> &RecentTier {
        &self.inner.recent
    }
    fn avet_unready(&self) -> &BTreeSet<u32> {
        &self.inner.avet_unready
    }
    fn record_cursor(&self, stats: PeerCursorStats) {
        let mut total = self
            .source
            .cursor_stats
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        total.cursor_ranges = total.cursor_ranges.saturating_add(1);
        total.cursor_cache_hits = total
            .cursor_cache_hits
            .saturating_add(stats.tree.cache_hits);
        total.cursor_cache_misses = total
            .cursor_cache_misses
            .saturating_add(stats.tree.cache_misses);
        total.cursor_root_reads = total
            .cursor_root_reads
            .saturating_add(stats.tree.root_reads);
        total.cursor_directory_reads = total
            .cursor_directory_reads
            .saturating_add(stats.tree.directory_reads);
        total.cursor_leaf_reads = total
            .cursor_leaf_reads
            .saturating_add(stats.tree.leaf_reads);
        if !self.is_repository() {
            total.cursor_sql_reads = total
                .cursor_sql_reads
                .saturating_add(stats.tree.root_reads)
                .saturating_add(stats.tree.directory_reads)
                .saturating_add(stats.tree.leaf_reads);
            total.cursor_sql_read_bytes = total
                .cursor_sql_read_bytes
                .saturating_add(stats.tree.decoded_bytes);
        }
        total.cursor_recent_datoms_examined = total
            .cursor_recent_datoms_examined
            .saturating_add(stats.recent.datoms_examined);
        total.cursor_recent_datoms_yielded = total
            .cursor_recent_datoms_yielded
            .saturating_add(stats.recent.datoms_yielded);
    }
}
pub(crate) struct BlockIndexCursor {
    cursor: MergeCursor<BlockSnapshot>,
    reads: Arc<Mutex<BlockReadStats>>,
}
impl BlockIndexCursor {
    pub(crate) fn stats(&self) -> PeerCursorStats {
        self.cursor.stats()
    }
    pub(crate) fn next_with_poll(
        &mut self,
        poll: &mut dyn FnMut() -> Result<bool, SemanticError>,
    ) -> Option<Result<Datom, SemanticError>> {
        let result = self.cursor.next_with_poll(poll);
        if matches!(&result, Some(Ok(_))) {
            let mut reads = self.reads.lock().unwrap_or_else(|e| e.into_inner());
            reads.datoms_yielded = reads.datoms_yielded.saturating_add(1);
        }
        result
    }
}
impl Iterator for BlockIndexCursor {
    type Item = Result<Datom, SemanticError>;
    fn next(&mut self) -> Option<Self::Item> {
        self.next_with_poll(&mut || Ok(true))
    }
}
fn format_identity(identity: [u8; 16]) -> String {
    let v = u128::from_be_bytes(identity);
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        v >> 96,
        (v >> 80) & 65535,
        (v >> 64) & 65535,
        (v >> 48) & 65535,
        v & 0xffffffffffff
    )
}
fn route_from_conditions(conditions: &[RefCondition]) -> Option<Arc<str>> {
    conditions
        .iter()
        .find_map(|condition| condition.key.strip_prefix("databases/").map(Arc::from))
}
fn repository_authority() -> SemanticError {
    SemanticError::new(
        ErrorCategory::Unsupported,
        "storage/repository-authority",
        "A repository value has no live publication or writer authority",
    )
}
fn fault(code: &'static str, message: &str) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, code, message)
}
fn limit(message: &str) -> SemanticError {
    SemanticError::new(ErrorCategory::Busy, "storage/recent-window-limit", message)
}

fn decode_captured_root(
    root_id: ObjectId,
    bytes: &[u8],
) -> Result<DatabaseValueRoot, SemanticError> {
    if bytes.len() > 20 + 5 * 32 + 33 {
        return Err(fault(
            "storage/read-root-size",
            "Database root exceeds its fixed descriptor size",
        ));
    }
    let block = Block::decode(&root_id, bytes)?;
    match block.kind {
        DATABASE_ROOT_KIND => Ok(DatabaseValueRoot::from(&DatabaseRoot::decode(
            &root_id, bytes,
        )?)),
        DATABASE_VALUE_ROOT_KIND => DatabaseValueRoot::decode(&root_id, bytes),
        _ => Err(fault(
            "storage/read-root-kind",
            "Reference does not name a database root",
        )),
    }
}
