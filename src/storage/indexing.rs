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
    let mut changes = BTreeMap::<Vec<u8>, (Option<Datom>, Option<Datom>)>::new();
    for datom in entries
        .iter()
        .flat_map(|entry| entry.transaction.tx_data.iter())
    {
        (io.control)()?;
        io.stats.input_datoms = io.stats.input_datoms.saturating_add(1);
        let key = prepare::stored_eav_key(datom)?;
        if let std::collections::btree_map::Entry::Vacant(entry) = changes.entry(key) {
            let found = lookup.exact(io, datom, &mut cache)?;
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
        if datom.added {
            if change.1.is_none() || u64::from(datom.attribute) == crate::DB_ALTER_ATTRIBUTE {
                change.1 = Some(datom.clone());
            }
        } else {
            change.1 = None;
        }
    }
    let mut removals = Vec::new();
    let mut insertions = Vec::new();
    for (old, current) in changes.into_values() {
        if let (Some(old), Some(current)) = (&old, &current)
            && old.tx == current.tx
            && old.added == current.added
            && old.value.stored_eq(&current.value)
        {
            continue;
        }
        removals.extend(old);
        insertions.extend(current);
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
                if !(old.order == IndexOrder::Avet
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
    let index = tree_index(work.history, IndexOrder::Avet);
    let target = &descriptor.trees[index];
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
    prepare::canonicalize_merge_edits(&mut edits, IndexOrder::Avet);
    cache = TreeNodeSet::default();
    descriptor.trees[index] = io.merge(target, &edits, config, &mut cache)?;
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
    usize::from(history) * 4 + super::descriptors::order_tag(order) as usize
}
fn tree(descriptor: &IndexDescriptor, history: bool, order: IndexOrder) -> &TreeDescriptor {
    &descriptor.trees[tree_index(history, order)]
}
fn fault(message: &str) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, "storage/index-preparation", message)
}
