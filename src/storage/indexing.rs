//! Bounded Rust-owned index preparation. A frozen snapshot protects the input;
//! preparation writes immutable objects, but never changes a database head.
//! The serialized writer validates the source and adopts the descriptor later.
use super::descriptors::{BlockAvetWork, IndexDescriptor};
use super::{BlockSnapshot, ObjectId, ObjectWriter, PgBlockStore, WriteProtection};
use crate::index::cursor::MergeSource;
use crate::index::metadata;
use crate::index::prepare::{self, IndexEavLookup, IndexNodeReader};
use crate::index::tree::{TreeConfig, TreeDescriptor, TreeMergeEdits, TreeNodeSet};
use crate::{Datom, ErrorCategory, IndexOrder, SemanticError};
use std::collections::{BTreeMap, BTreeSet};

const PROJECTION_DATOMS: usize = 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BlockIndexStats {
    pub input_datoms: u64,
    pub nodes_read: u64,
    pub bytes_read: u64,
    /// Objects accepted by the writer, including deduplicated content. The live
    /// preparation path uses immediate guarded puts; these are not SQL counts.
    pub nodes_written: u64,
    /// Canonical accepted bytes, not physical traffic or unique retained bytes.
    pub bytes_written: u64,
    pub reused_subtrees: u64,
    pub no_history_pairs: u64,
    pub projection_datoms: u64,
    pub projection_steps: u64,
    pub peak_preloaded_bytes: u64,
    pub preparation_workers: u64,
    pub peak_preparation_workers: u64,
}

/// Source capture is immutable and retained by the storage grace period. Ordinary
/// writes may continue while a worker prepares this immutable prefix.
pub struct IndexInput {
    pub(crate) snapshot: BlockSnapshot,
    fulltext_limits: crate::FulltextBuildLimits,
}

/// Successful preparation retains its immutable source and exact protection epoch.
/// A stale source or GC guard must cause adoption to discard/retry, not publish
/// a potentially reclaimed candidate. Dropping this value never publishes it.
pub struct PreparedIndex {
    pub(crate) snapshot: BlockSnapshot,
    pub(crate) source_indexes: ObjectId,
    pub(crate) through_basis: u64,
    pub(crate) generation: u64,
    pub(crate) endpoint_entry: Option<ObjectId>,
    pub(crate) descriptor: IndexDescriptor,
    pub(crate) descriptor_id: ObjectId,
    pub(crate) protection: WriteProtection,
    pub(crate) stats: BlockIndexStats,
    pub(crate) fulltext_stats: Option<crate::FulltextBuildStats>,
}

impl IndexInput {
    pub fn new(snapshot: BlockSnapshot) -> Result<Self, SemanticError> {
        if snapshot.captured_root().indexes.is_none() {
            return Err(fault("Frozen index input has no indexed base"));
        }
        Ok(Self {
            snapshot,
            fulltext_limits: crate::FulltextBuildLimits::default(),
        })
    }
    /// Resource policy for this coherent canonical/search preparation job.
    /// This changes admission, not the identity of committed transactions.
    pub fn with_fulltext_build_limits(
        mut self,
        limits: crate::FulltextBuildLimits,
    ) -> Result<Self, SemanticError> {
        limits.validate()?;
        self.fulltext_limits = limits;
        Ok(self)
    }
    pub fn through_basis(&self) -> u64 {
        if has_projection(self.snapshot.index_descriptor()) {
            self.snapshot.index_descriptor().basis
        } else {
            self.snapshot.basis_t()
        }
    }
    pub fn prepare(
        self,
        store: &mut PgBlockStore,
        config: &TreeConfig,
    ) -> Result<PreparedIndex, SemanticError> {
        self.prepare_with_control(store, config, &mut || Ok(()))
    }
    pub fn prepare_with_control(
        self,
        store: &mut PgBlockStore,
        config: &TreeConfig,
        control: &mut dyn FnMut() -> Result<(), SemanticError>,
    ) -> Result<PreparedIndex, SemanticError> {
        self.prepare_with_parallelism(store, config, 1, control)
    }
    /// Only pure edit planning runs in parallel. The independent worker owns
    /// all node I/O, uploads, cancellation and publication protection.
    pub fn prepare_with_parallelism(
        self,
        store: &mut PgBlockStore,
        config: &TreeConfig,
        parallelism: usize,
        control: &mut dyn FnMut() -> Result<(), SemanticError>,
    ) -> Result<PreparedIndex, SemanticError> {
        if !(1..=8).contains(&parallelism) {
            return Err(SemanticError::incorrect(
                "storage/index-parallelism",
                "Index preparation parallelism must be 1..=8",
            ));
        }
        control()?;
        config.validate()?;
        if store.write_protection().is_some() {
            return Err(SemanticError::incorrect(
                "storage/index-protection",
                "Index worker requires an unconfigured independent store",
            ));
        }
        let protection = crate::storage::protection::protection(store, &[])?;
        store.set_write_protection(Some(protection.clone()))?;
        let result = (|| {
            let through_basis = self.through_basis();
            let endpoint_entry = if through_basis == 0 {
                None
            } else {
                Some(
                    self.snapshot
                        .captured_log()
                        .ok_or_else(|| fault("Nonempty input has no captured log"))?
                        .read_record(store, through_basis)?
                        .ok_or_else(|| fault("Prepared endpoint is absent from captured log"))?
                        .id,
                )
            };
            let mut io = PreparationIo {
                store,
                control,
                stats: BlockIndexStats::default(),
            };
            let mut descriptor = if has_projection(self.snapshot.index_descriptor()) {
                advance_projection(
                    self.snapshot.index_descriptor(),
                    &self.snapshot.base_metadata().schema,
                    &mut io,
                    config,
                )?
            } else {
                prepare_tail(
                    &self.snapshot,
                    self.snapshot.index_descriptor(),
                    self.snapshot.avet_unready(),
                    &mut io,
                    config,
                    parallelism,
                )?
            };
            (io.control)()?;
            let schema = if descriptor.basis == self.snapshot.basis_t() {
                &self.snapshot.endpoint_metadata().schema
            } else {
                &self.snapshot.base_metadata().schema
            };
            // Canonical and search roots are one prepared publication. Failure
            // cannot expose a covering index with missing fulltext documents.
            let fulltext = super::fulltext::build_for_descriptor_with_control(
                io.store,
                &descriptor,
                schema,
                Some((
                    self.snapshot.index_descriptor(),
                    &self.snapshot.base_metadata().schema,
                )),
                &self.fulltext_limits,
                io.control,
            )?;
            let fulltext_stats = fulltext.map(|(attachment, stats)| {
                descriptor.fulltext = Some(attachment);
                stats
            });
            (io.control)()?;
            let descriptor_id = io.write(&descriptor.encode()?)?;
            Ok(PreparedIndex {
                source_indexes: self.snapshot.captured_root().indexes.unwrap(),
                through_basis,
                generation: descriptor.generation,
                endpoint_entry,
                descriptor,
                descriptor_id,
                protection,
                stats: io.stats,
                fulltext_stats,
                snapshot: self.snapshot,
            })
        })();
        let clear = store.set_write_protection(None);
        match (result, clear) {
            (Err(e), _) | (Ok(_), Err(e)) => Err(e),
            (Ok(value), Ok(())) => Ok(value),
        }
    }
}

impl PreparedIndex {
    pub fn through_basis(&self) -> u64 {
        self.through_basis
    }
    pub fn descriptor(&self) -> &IndexDescriptor {
        &self.descriptor
    }
    pub fn descriptor_id(&self) -> ObjectId {
        self.descriptor_id
    }
    pub fn stats(&self) -> BlockIndexStats {
        self.stats
    }
    pub fn fulltext_stats(&self) -> Option<crate::FulltextBuildStats> {
        self.fulltext_stats
    }
}

fn has_projection(descriptor: &IndexDescriptor) -> bool {
    !descriptor.avet_work.is_empty() || !descriptor.pending_avet.is_empty()
}

struct PreparationIo<'a> {
    store: &'a mut dyn ObjectWriter,
    control: &'a mut dyn FnMut() -> Result<(), SemanticError>,
    stats: BlockIndexStats,
}
impl IndexNodeReader for PreparationIo<'_> {
    fn load_node(&mut self, hash: ObjectId) -> Result<Option<Vec<u8>>, SemanticError> {
        (self.control)()?;
        let bytes = self.store.read_object(hash)?;
        self.stats.nodes_read = self.stats.nodes_read.saturating_add(1);
        self.stats.bytes_read = self.stats.bytes_read.saturating_add(bytes.len() as u64);
        Ok(Some(bytes))
    }
}
impl PreparationIo<'_> {
    fn write(&mut self, bytes: &[u8]) -> Result<ObjectId, SemanticError> {
        (self.control)()?;
        let hash = self.store.put_object(bytes)?;
        if hash != crate::sha256(bytes) {
            return Err(fault(
                "Object writer returned a different canonical identity",
            ));
        }
        self.stats.nodes_written = self.stats.nodes_written.saturating_add(1);
        self.stats.bytes_written = self.stats.bytes_written.saturating_add(bytes.len() as u64);
        Ok(hash)
    }
    fn merge(
        &mut self,
        descriptor: &TreeDescriptor,
        edits: &TreeMergeEdits,
        config: &TreeConfig,
        cache: &mut TreeNodeSet,
    ) -> Result<TreeDescriptor, SemanticError> {
        (self.control)()?;
        prepare::preload_merge_paths(self, descriptor, edits, cache)?;
        self.stats.peak_preloaded_bytes = self
            .stats
            .peak_preloaded_bytes
            .max(cache.iter().map(|(_, b)| b.len() as u64).sum());
        let merged = prepare::merge_index_tree(self, descriptor, cache, edits, config)?;
        self.stats.reused_subtrees = self
            .stats
            .reused_subtrees
            .saturating_add(merged.stats.reused_directory_refs)
            .saturating_add(merged.stats.reused_leaf_refs)
            .saturating_add(merged.stats.reused_hashes);
        self.write_nodes(&merged.new_nodes)?;
        Ok(merged.descriptor)
    }

    fn write_nodes(&mut self, nodes: &TreeNodeSet) -> Result<(), SemanticError> {
        let mut pending = Vec::with_capacity(super::object_io::MAX_WRITE_BATCH_OBJECTS);
        let mut bytes = 0usize;
        for (id, payload) in nodes.iter() {
            if !pending.is_empty()
                && (pending.len() == super::object_io::MAX_WRITE_BATCH_OBJECTS
                    || bytes.saturating_add(payload.len()) > 4 * 1024 * 1024)
            {
                self.write_batch(&pending)?;
                pending.clear();
                bytes = 0;
            }
            pending.push((*id, payload));
            bytes += payload.len();
        }
        if !pending.is_empty() {
            self.write_batch(&pending)?;
        }
        Ok(())
    }
    fn write_batch(&mut self, pending: &[(ObjectId, &[u8])]) -> Result<(), SemanticError> {
        (self.control)()?;
        let payloads = pending.iter().map(|(_, bytes)| *bytes).collect::<Vec<_>>();
        let ids = self.store.put_objects(&payloads)?;
        if ids.len() != pending.len()
            || ids
                .iter()
                .zip(pending)
                .any(|(actual, (expected, _))| actual != expected)
        {
            return Err(fault("Merged node identity changed during batch upload"));
        }
        self.stats.nodes_written = self
            .stats
            .nodes_written
            .saturating_add(pending.len() as u64);
        self.stats.bytes_written = self
            .stats
            .bytes_written
            .saturating_add(payloads.iter().map(|bytes| bytes.len() as u64).sum());
        (self.control)()?;
        Ok(())
    }
}

fn prepare_tail(
    snapshot: &BlockSnapshot,
    previous: &IndexDescriptor,
    unready: &BTreeSet<u32>,
    io: &mut PreparationIo<'_>,
    config: &TreeConfig,
    parallelism: usize,
) -> Result<IndexDescriptor, SemanticError> {
    let recent = snapshot.recent_tier();
    let base = snapshot.base_metadata();
    let endpoint = snapshot.endpoint_metadata();
    if previous.basis == snapshot.basis_t() {
        return Ok(previous.clone());
    }
    let entries = recent.entries();
    let mut cache = TreeNodeSet::default();
    let mut lookup = IndexEavLookup::new(io, tree(previous, false, IndexOrder::Eavt), &mut cache)?;
    let mut changes = BTreeMap::<Vec<u8>, (Vec<Datom>, Vec<Datom>)>::new();
    for datom in entries
        .iter()
        .flat_map(|entry| entry.transaction.tx_data.iter())
    {
        (io.control)()?;
        io.stats.input_datoms = io.stats.input_datoms.saturating_add(1);
        let key = prepare::stored_eav_key(datom)?;
        if let std::collections::btree_map::Entry::Vacant(entry) = changes.entry(key) {
            let found = lookup.all(io, datom, &mut cache)?;
            entry.insert((found.clone(), found));
        }
    }
    for datom in entries
        .iter()
        .flat_map(|entry| entry.transaction.tx_data.iter())
    {
        (io.control)()?;
        let change = changes
            .get_mut(&prepare::stored_eav_key(datom)?)
            .expect("collected change");
        change.1.push(datom.clone());
    }
    let mut removals = Vec::new();
    let mut insertions = Vec::new();
    for (old, events) in changes.into_values() {
        let current = crate::index::current_eav_group(events);
        // A retained contribution preserves its original transaction/source.
        // Compare complete datoms so another source's identical value remains
        // material, and a global retraction removes every earlier source.
        removals.extend(
            old.iter()
                .filter(|d| {
                    current
                        .binary_search_by(|c| c.cmp_in(d, IndexOrder::Eavt))
                        .is_err()
                })
                .cloned(),
        );
        insertions.extend(
            current
                .iter()
                .filter(|d| {
                    old.binary_search_by(|c| c.cmp_in(d, IndexOrder::Eavt))
                        .is_err()
                })
                .cloned(),
        );
    }
    // Snapshot construction already folded every transition, including a
    // disable/re-enable cycle hidden by equal base/endpoint schemas.
    let transitions = if std::sync::Arc::ptr_eq(&base.schema, &endpoint.schema) {
        Vec::new()
    } else {
        metadata::changed_avet_attributes(&base.schema, &endpoint.schema)
    };
    let mut changed: BTreeMap<u32, (u32, bool, bool)> = transitions
        .into_iter()
        .filter(|(a, _, after)| !after || unready.contains(a))
        .map(|c| (c.0, c))
        .collect();
    for attribute in unready.iter().copied() {
        changed.entry(attribute).or_insert((
            attribute,
            metadata::effective_avet(&base.schema, attribute),
            true,
        ));
    }
    let changed = changed.into_values().collect::<Vec<_>>();
    let empty = BTreeMap::new();
    let mut history_edits =
        prepare::history_edits(IndexOrder::Eavt, recent, &changed, &empty, &empty)?;
    prepare::canonicalize_merge_edits(&mut history_edits, IndexOrder::Eavt);
    cache = TreeNodeSet::default();
    let history = tree(previous, true, IndexOrder::Eavt);
    prepare::preload_merge_paths(io, history, &history_edits, &mut cache)?;
    let pairs =
        crate::index::tree::discover_merge_no_history_pairs(history, &cache, &history_edits)?;
    io.stats.no_history_pairs = pairs.len() as u64;
    let mut descriptor = previous.clone();
    descriptor.basis = snapshot.basis_t();
    descriptor.fulltext = None;
    let prepare = |(history, order)| -> Result<_, SemanticError> {
        let index = tree_index(history, order);
        let old = &previous.trees[index];
        let mut edits = if old.history {
            let mut edits = prepare::history_edits(old.order, recent, &changed, &empty, &empty)?;
            for pair in &pairs {
                if !(matches!(old.order, IndexOrder::Avet | IndexOrder::Savet)
                    && changed.iter().any(|c| c.0 == pair.retraction.attribute))
                    && metadata::schema_index_member(&endpoint.schema, &pair.retraction, old.order)?
                {
                    edits.no_history_pairs.push(pair.clone());
                }
            }
            edits.no_history_attributes.clear();
            edits
        } else {
            prepare::current_edits(
                old.order,
                &removals,
                &insertions,
                &base.schema,
                &endpoint.schema,
                &changed,
                &empty,
                &empty,
            )?
        };
        prepare::canonicalize_merge_edits(&mut edits, old.order);
        Ok((index, edits))
    };
    let projections: Vec<_> = previous
        .trees
        .iter()
        .map(|t| (t.history, t.order))
        .collect();
    for group in projections.chunks(parallelism) {
        (io.control)()?;
        let (prepared, workers, peak) = prepare::prepare_index_group(group, &prepare)?;
        io.stats.preparation_workers = io.stats.preparation_workers.saturating_add(workers);
        io.stats.peak_preparation_workers = io.stats.peak_preparation_workers.max(peak);
        for (index, edits) in prepared {
            cache = TreeNodeSet::default();
            descriptor.trees[index] =
                io.merge(&previous.trees[index], &edits, config, &mut cache)?;
        }
    }
    descriptor.pending_avet = changed.iter().filter(|c| c.2).map(|c| c.0).collect();
    descriptor.avet_work = changed
        .iter()
        .map(|&(attribute, _, adding)| new_work(&descriptor, attribute, adding, false))
        .collect();
    Ok(descriptor)
}

fn new_work(
    descriptor: &IndexDescriptor,
    attribute: u32,
    adding: bool,
    history: bool,
) -> BlockAvetWork {
    BlockAvetWork {
        attribute,
        adding,
        history,
        clearing: true,
        directory: 0,
        leaf: 0,
        slot: 0,
        source: tree(descriptor, history, IndexOrder::Aevt).clone(),
    }
}

fn advance_projection(
    previous: &IndexDescriptor,
    schema: &crate::Schema,
    io: &mut PreparationIo<'_>,
    config: &TreeConfig,
) -> Result<IndexDescriptor, SemanticError> {
    let mut descriptor = previous.clone();
    // A readiness-only descriptor always starts by clearing the target, never
    // guesses that an arbitrary existing partial range is already complete.
    for attribute in descriptor.pending_avet.clone() {
        if !descriptor
            .avet_work
            .iter()
            .any(|w| w.attribute == attribute)
        {
            descriptor
                .avet_work
                .push(new_work(&descriptor, attribute, true, false));
        }
    }
    descriptor.avet_work.sort_by_key(|w| w.attribute);
    let work = descriptor
        .avet_work
        .first()
        .ok_or_else(|| fault("Missing AVET work"))?
        .clone();
    if metadata::effective_avet(schema, work.attribute) != work.adding
        || work.source != *tree(&descriptor, work.history, IndexOrder::Aevt)
    {
        return Err(fault("AVET work differs from frozen schema or source"));
    }
    let target = tree(&descriptor, work.history, IndexOrder::Avet);
    let source = if work.clearing { target } else { &work.source };
    let mut cache = TreeNodeSet::default();
    let chunk = prepare::tree_attribute_chunk(
        io,
        source,
        work.attribute,
        work.directory,
        work.leaf,
        work.slot,
        PROJECTION_DATOMS,
        config.max_leaf_bytes as u64,
        &mut cache,
    )?;
    if chunk.datoms.iter().any(|d| d.attribute != work.attribute) {
        return Err(fault("AVET chunk escaped its attribute"));
    }
    let mut edits = TreeMergeEdits::default();
    if work.clearing {
        edits.projection_removals = chunk.datoms;
    } else {
        edits.insertions = chunk.datoms;
    }
    let datoms = (edits.projection_removals.len() + edits.insertions.len()) as u64;
    io.stats.projection_datoms = io.stats.projection_datoms.saturating_add(datoms);
    io.stats.input_datoms = io.stats.input_datoms.saturating_add(datoms);
    io.stats.projection_steps = io.stats.projection_steps.saturating_add(1);
    // Attribute is contiguous only in AVET/AEVT. Use one bounded chunk as
    // the exact datom witness for both projections, including clearing: a
    // SAVET attribute may be scattered across every source in the tree.
    for order in [IndexOrder::Avet, IndexOrder::Savet] {
        prepare::canonicalize_merge_edits(&mut edits, order);
        cache = TreeNodeSet::default();
        let index = tree_index(work.history, order);
        descriptor.trees[index] = io.merge(&descriptor.trees[index], &edits, config, &mut cache)?;
    }
    descriptor.fulltext = None;
    if chunk.complete {
        if work.clearing && work.adding {
            descriptor.avet_work[0].clearing = false;
        } else if !work.history {
            descriptor.avet_work[0] = new_work(&descriptor, work.attribute, work.adding, true);
        } else {
            descriptor.avet_work.remove(0);
            descriptor.pending_avet.retain(|a| *a != work.attribute);
        }
    } else if !work.clearing {
        let next = &mut descriptor.avet_work[0];
        next.directory = chunk.directory;
        next.leaf = chunk.leaf;
        next.slot = chunk.slot;
    }
    Ok(descriptor)
}

fn tree_index(history: bool, order: IndexOrder) -> usize {
    usize::from(history) * 6 + super::descriptors::order_tag(order) as usize
}
fn tree(descriptor: &IndexDescriptor, history: bool, order: IndexOrder) -> &TreeDescriptor {
    &descriptor.trees[tree_index(history, order)]
}
fn fault(message: &str) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, "storage/index-preparation", message)
}

#[cfg(test)]
mod tests {
    use super::super::{ObjectReader, SnapshotMetadata};
    use super::*;
    use crate::{
        Attribute, Cardinality, Database, EntityRef, Keyword, Schema, TxOp, TxValue, Value,
        ValueType, View,
    };

    const ORDERS: [IndexOrder; 6] = [
        IndexOrder::Eavt,
        IndexOrder::Aevt,
        IndexOrder::Avet,
        IndexOrder::Vaet,
        IndexOrder::Saevt,
        IndexOrder::Savet,
    ];

    #[derive(Default)]
    struct Memory(BTreeMap<ObjectId, Vec<u8>>);
    impl ObjectReader for Memory {
        fn read_object(&mut self, id: ObjectId) -> Result<Vec<u8>, SemanticError> {
            self.0
                .get(&id)
                .cloned()
                .ok_or_else(|| fault("Missing test object"))
        }
    }
    impl ObjectWriter for Memory {
        fn put_object(&mut self, bytes: &[u8]) -> Result<ObjectId, SemanticError> {
            let id = crate::sha256(bytes);
            self.0.insert(id, bytes.to_vec());
            Ok(id)
        }
        fn flush_objects(&mut self) -> Result<(), SemanticError> {
            Ok(())
        }
    }

    fn config() -> TreeConfig {
        TreeConfig {
            max_leaf_datoms: 7,
            ..TreeConfig::default()
        }
    }
    fn schema(indexed: bool, no_history: bool) -> Schema {
        let mut schema = Schema::new();
        for (id, name, kind) in [
            (100, "name", ValueType::String),
            (101, "link", ValueType::Ref),
        ] {
            let mut attribute =
                Attribute::new(id, Keyword::new("test", name), kind, Cardinality::Many);
            attribute.indexed = indexed;
            attribute.no_history = no_history;
            schema.install(attribute).unwrap();
        }
        schema
    }
    fn add(attribute: u32, value: Value) -> TxOp {
        TxOp::Add {
            entity: EntityRef::Id(500),
            attribute,
            value: TxValue::Scalar(value),
        }
    }
    fn withdraw(attribute: u32, value: Value) -> TxOp {
        TxOp::Retract {
            entity: EntityRef::Id(500),
            attribute,
            value: Some(TxValue::Scalar(value)),
        }
    }
    fn descriptor(memory: &mut Memory, db: &Database) -> IndexDescriptor {
        let mut trees = Vec::new();
        for history in [false, true] {
            for order in ORDERS {
                let built = crate::index::tree::build_tree(
                    order,
                    history,
                    db.datoms(
                        if history {
                            View::History
                        } else {
                            View::Current
                        },
                        order,
                    ),
                    &config(),
                )
                .unwrap();
                for (_, bytes) in built.nodes.iter() {
                    memory.put_object(bytes).unwrap();
                }
                trees.push(built.descriptor);
            }
        }
        IndexDescriptor {
            identity: [7; 16],
            basis: db.basis_t(),
            generation: 0,
            trees,
            pending_avet: Vec::new(),
            avet_work: Vec::new(),
            fulltext: None,
        }
    }
    fn read(
        memory: &Memory,
        descriptor: &IndexDescriptor,
        history: bool,
        order: IndexOrder,
    ) -> Vec<Datom> {
        let nodes = TreeNodeSet::from_nodes(memory.0.clone());
        crate::index::tree::range_tree(tree(descriptor, history, order), &nodes, None, None)
            .unwrap()
            .datoms
    }
    fn snapshot(
        memory: &mut Memory,
        indexed: &IndexDescriptor,
        states: &[Database],
    ) -> (tempfile::TempDir, BlockSnapshot) {
        let endpoint = states.last().unwrap();
        let mut log = super::super::log::LogRoot::empty();
        for db in states {
            let entry = super::super::log::LogEntry {
                basis_t: db.basis_t(),
                eidx_frontier: db.eidx_frontier(),
                reserved_frontier: db.reserved_allocation().unwrap().frontier(),
                tx_data: db
                    .datoms(View::History, IndexOrder::Eavt)
                    .into_iter()
                    .filter(|d| crate::tx_to_t(d.tx).ok() == Some(db.basis_t()))
                    .collect(),
            };
            log = log.append(memory, &entry).unwrap();
        }
        let metadata = SnapshotMetadata {
            identity: indexed.identity,
            basis: endpoint.basis_t(),
            generation: 0,
            eidx_frontier: endpoint.eidx_frontier(),
            reserved_frontier: endpoint.reserved_allocation().unwrap().frontier(),
            last_tx_instant: endpoint.last_tx_instant(),
            excision: None,
        };
        let metadata = memory.put_object(&metadata.encode().unwrap()).unwrap();
        let indexes = memory.put_object(&indexed.encode().unwrap()).unwrap();
        let publication = super::super::root::DatabaseRoot {
            identity: indexed.identity,
            basis: endpoint.basis_t(),
            writer_epoch: 0,
            log: log.head(),
            indexes: Some(indexes),
            metadata: Some(metadata),
            receipts: None,
            read_authorization: None,
        };
        let value = super::super::root::DatabaseValueRoot::from(&publication);
        let publication = memory.put_object(&publication.encode().unwrap()).unwrap();
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("objects")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [
                directory.path().to_path_buf(),
                directory.path().join("objects"),
            ] {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
        for (id, bytes) in &memory.0 {
            let name: String = id.iter().map(|b| format!("{b:02x}")).collect();
            std::fs::write(directory.path().join("objects").join(name), bytes).unwrap();
        }
        let point = crate::backup::ReadPoint {
            point: crate::BackupPoint {
                lineage_id: "test".into(),
                log_generation: 0,
                basis_t: value.basis,
                manifest_hash: [0; 32],
                objects_written: 0,
                objects_reused: 0,
            },
            publication,
            value,
            open_object_reads: 0,
            open_object_bytes: 0,
        };
        let snapshot = BlockSnapshot::open_repository(
            directory.path(),
            &point,
            super::super::BlockReadConfig::default(),
        )
        .unwrap();
        (directory, snapshot)
    }
    fn prepared(
        memory: &mut Memory,
        indexed: &IndexDescriptor,
        states: &[Database],
    ) -> (IndexDescriptor, BlockIndexStats) {
        let (_directory, snapshot) = snapshot(memory, indexed, states);
        let mut control = || Ok(());
        let mut io = PreparationIo {
            store: memory,
            control: &mut control,
            stats: BlockIndexStats::default(),
        };
        let descriptor = prepare_tail(
            &snapshot,
            indexed,
            snapshot.avet_unready(),
            &mut io,
            &config(),
            2,
        )
        .unwrap();
        (descriptor, io.stats)
    }

    #[test]
    fn durable_tail_keeps_sources_and_global_retraction_in_all_orders() {
        let initial = Database::new(schema(true, false)).unwrap();
        let operations = [
            add(100, Value::String("peaches".into())),
            add(101, Value::Ref(43)),
        ];
        let first = initial
            .with(&operations, 1, EntityRef::Id(99))
            .unwrap()
            .db_after;
        let second = first
            .with(&operations, 2, EntityRef::Id(98))
            .unwrap()
            .db_after;
        let mut memory = Memory::default();
        let old = descriptor(&mut memory, &first);
        let states = [initial.clone(), first.clone(), second.clone()];
        let (merged, _) = prepared(&mut memory, &old, &states);
        assert_eq!(merged.trees.len(), 12);
        for history in [false, true] {
            for order in ORDERS {
                assert_eq!(
                    read(&memory, &merged, history, order),
                    second.datoms(
                        if history {
                            View::History
                        } else {
                            View::Current
                        },
                        order
                    )
                );
            }
        }
        let removed = second
            .with(
                &[
                    withdraw(100, Value::String("peaches".into())),
                    withdraw(101, Value::Ref(43)),
                ],
                3,
                EntityRef::Id(97),
            )
            .unwrap()
            .db_after;
        let readded = removed
            .with(&operations, 4, EntityRef::Id(96))
            .unwrap()
            .db_after;
        let states = [initial, first.clone(), second, removed, readded.clone()];
        let (replacement, _) = prepared(&mut memory, &merged, &states);
        for history in [false, true] {
            for order in ORDERS {
                assert_eq!(
                    read(&memory, &replacement, history, order),
                    readded.datoms(
                        if history {
                            View::History
                        } else {
                            View::Current
                        },
                        order
                    )
                );
            }
        }
        assert_eq!(
            read(&memory, &old, false, IndexOrder::Saevt),
            first.datoms(View::Current, IndexOrder::Saevt)
        );
    }

    #[test]
    fn no_history_withdrawal_uses_shared_cross_source_witnesses() {
        let initial = Database::new(schema(true, true)).unwrap();
        let operations = [
            add(100, Value::String("peaches".into())),
            add(101, Value::Ref(43)),
        ];
        let first = initial
            .with(&operations, 1, EntityRef::Id(99))
            .unwrap()
            .db_after;
        let second = first
            .with(&operations, 2, EntityRef::Id(98))
            .unwrap()
            .db_after;
        let mut memory = Memory::default();
        let old = descriptor(&mut memory, &second);
        let removed = second
            .with(
                &[
                    withdraw(100, Value::String("peaches".into())),
                    withdraw(101, Value::Ref(43)),
                ],
                3,
                EntityRef::Id(97),
            )
            .unwrap()
            .db_after;
        let (merged, stats) = prepared(&mut memory, &old, &[initial, first, second, removed]);
        assert_eq!(stats.no_history_pairs, 4);
        for history in [false, true] {
            for order in ORDERS {
                let facts = read(&memory, &merged, history, order);
                assert!(
                    facts.iter().all(|d| d.entity != 500),
                    "history={history}, order={order:?}, facts={:?}",
                    facts
                        .into_iter()
                        .filter(|d| d.entity == 500)
                        .collect::<Vec<_>>()
                );
            }
        }
        assert_eq!(
            read(&memory, &old, true, IndexOrder::Saevt)
                .iter()
                .filter(|d| d.entity == 500)
                .count(),
            4
        );
    }

    #[test]
    fn bounded_backfill_and_clear_publish_avet_savet_together() {
        let mut memory = Memory::default();
        let genesis = Database::new(schema(false, false)).unwrap();
        let mut descriptor = descriptor(&mut memory, &genesis);
        // Scatter the attribute among source-first ranges and force multiple
        // bounded chunks in both current and history trees.
        for history in [false, true] {
            let datoms: Vec<_> = (0..1100)
                .map(|i| Datom {
                    entity: 1000 + i,
                    attribute: 100,
                    value: Value::String(format!("item-{i}")),
                    tx: crate::t_to_tx(1).unwrap(),
                    source: 90 + i % 5,
                    added: true,
                })
                .collect();
            let mut aevt = read(&memory, &descriptor, history, IndexOrder::Aevt);
            aevt.extend(datoms);
            let built =
                crate::index::tree::build_tree(IndexOrder::Aevt, history, aevt, &config()).unwrap();
            for (_, bytes) in built.nodes.iter() {
                memory.put_object(bytes).unwrap();
            }
            descriptor.trees[tree_index(history, IndexOrder::Aevt)] = built.descriptor;
        }
        descriptor.pending_avet.push(100);
        descriptor
            .avet_work
            .push(new_work(&descriptor, 100, true, false));
        let original = descriptor.clone();
        let enabled = schema(true, false);
        let disabled = schema(false, false);
        let mut control = || Ok(());
        let mut steps = 0;
        while has_projection(&descriptor) {
            let mut io = PreparationIo {
                store: &mut memory,
                control: &mut control,
                stats: BlockIndexStats::default(),
            };
            descriptor = advance_projection(&descriptor, &enabled, &mut io, &config()).unwrap();
            assert!(io.stats.projection_datoms <= PROJECTION_DATOMS as u64);
            steps += 1;
            for history in [false, true] {
                let mut avet = read(&memory, &descriptor, history, IndexOrder::Avet);
                let mut savet = read(&memory, &descriptor, history, IndexOrder::Savet);
                avet.sort_by(|a, b| a.cmp_in(b, IndexOrder::Eavt));
                savet.sort_by(|a, b| a.cmp_in(b, IndexOrder::Eavt));
                assert_eq!(avet, savet);
            }
            assert!(steps < 20);
        }
        assert!(steps >= 6);
        for history in [false, true] {
            assert_eq!(
                read(&memory, &descriptor, history, IndexOrder::Savet)
                    .iter()
                    .filter(|d| d.attribute == 100)
                    .count(),
                1100
            );
            assert!(
                read(&memory, &original, history, IndexOrder::Savet)
                    .iter()
                    .all(|d| d.attribute != 100)
            );
        }
        descriptor
            .avet_work
            .push(new_work(&descriptor, 100, false, false));
        while has_projection(&descriptor) {
            let mut io = PreparationIo {
                store: &mut memory,
                control: &mut control,
                stats: BlockIndexStats::default(),
            };
            descriptor = advance_projection(&descriptor, &disabled, &mut io, &config()).unwrap();
            assert!(io.stats.projection_datoms <= PROJECTION_DATOMS as u64);
            for history in [false, true] {
                let mut avet = read(&memory, &descriptor, history, IndexOrder::Avet);
                let mut savet = read(&memory, &descriptor, history, IndexOrder::Savet);
                avet.sort_by(|a, b| a.cmp_in(b, IndexOrder::Eavt));
                savet.sort_by(|a, b| a.cmp_in(b, IndexOrder::Eavt));
                assert_eq!(avet, savet);
            }
        }
        for history in [false, true] {
            for order in [IndexOrder::Avet, IndexOrder::Savet] {
                assert!(
                    read(&memory, &descriptor, history, order)
                        .iter()
                        .all(|d| d.attribute != 100)
                );
            }
        }
    }

    #[test]
    fn fulltext_uses_the_six_order_history_offset_for_incremental_omissions() {
        let mut schema = Schema::new();
        let mut attribute = Attribute::new(
            100,
            Keyword::new("test", "text"),
            ValueType::String,
            Cardinality::Many,
        )
        .fulltext();
        attribute.no_history = true;
        schema.install(attribute).unwrap();
        let initial = Database::new(schema).unwrap();
        let operation = add(100, Value::String("fresh peaches".into()));
        let first = initial
            .with(std::slice::from_ref(&operation), 1, EntityRef::Id(99))
            .unwrap()
            .db_after;
        let second = first
            .with(&[operation], 2, EntityRef::Id(98))
            .unwrap()
            .db_after;
        let mut memory = Memory::default();
        let mut before = descriptor(&mut memory, &second);
        let (attachment, stats) = super::super::fulltext::build_for_descriptor(
            &mut memory,
            &before,
            second.schema(),
            None,
            &crate::FulltextBuildLimits::default(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(stats.documents_added, 2);
        before.fulltext = Some(attachment);
        let removed = second
            .with(
                &[withdraw(100, Value::String("fresh peaches".into()))],
                3,
                EntityRef::Id(97),
            )
            .unwrap()
            .db_after;
        let (after, _) = prepared(
            &mut memory,
            &before,
            &[initial, first, second.clone(), removed.clone()],
        );
        let (_, stats) = super::super::fulltext::build_for_descriptor(
            &mut memory,
            &after,
            removed.schema(),
            Some((&before, second.schema())),
            &crate::FulltextBuildLimits::default(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(stats.documents_removed, 2);
        assert_eq!(stats.documents_added, 0);
    }

    #[test]
    fn old_storage_blocks_are_rejected_before_log_payload_interpretation() {
        let block = super::super::root::Block {
            kind: super::super::log::LOG_ENTRY_KIND,
            links: Vec::new(),
            payload: vec![0; 40],
        };
        let bytes = block.encode().unwrap();
        assert_eq!(&bytes[4..6], &3u16.to_be_bytes());
        assert_eq!(
            super::super::root::Block::decode(&crate::sha256(&bytes), &bytes).unwrap(),
            block
        );
        let mut old = bytes;
        old[4..6].copy_from_slice(&2u16.to_be_bytes());
        assert!(super::super::root::Block::decode(&crate::sha256(&old), &old).is_err());
    }
}
