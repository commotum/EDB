//! Storage-neutral index edit preparation, selected-path loading and merging.
use super::metadata::{same_eav, schema_index_member, sort_dedup_datoms};
use crate::index::recent::RecentTier;
use crate::index::tree::navigation::{
    floor_tree_child, leaf_lower_bound, validate_loaded_child_datom, validate_loaded_child_key,
};
use crate::index::tree::{
    ChildRef, DirectoryNode, LeafSegment, RootNode, TreeConfig, TreeMergeEdits, TreeNode,
    TreeNodeSet, decode_tree_node, merge_tree_with_boundary_loader,
};
use crate::{Datom, Digest, ErrorCategory, IndexOrder, SemanticError};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::Ordering;
pub(crate) fn validate_index_preparation_parallelism(workers: usize) -> Result<(), SemanticError> {
    if !(1..=8).contains(&workers) {
        return Err(SemanticError::incorrect(
            "index/invalid-preparation-parallelism",
            "index edit preparation parallelism must be between one and eight",
        ));
    }
    Ok(())
}

fn same_logical_eav(left: &Datom, right: &Datom) -> bool {
    left.entity == right.entity
        && left.attribute == right.attribute
        && left.value.index_cmp(&right.value) == std::cmp::Ordering::Equal
}

pub(crate) fn prepare_index_group<T: Send>(
    group: &[(bool, IndexOrder)],
    prepare: &(impl Fn((bool, IndexOrder)) -> Result<T, SemanticError> + Sync),
) -> Result<(Vec<T>, u64, u64), SemanticError> {
    if group.len() == 1 {
        return Ok((vec![prepare(group[0])?], 0, 0));
    }
    let active = std::sync::atomic::AtomicUsize::new(0);
    let peak = std::sync::atomic::AtomicUsize::new(0);
    let mut workers = 0;
    let prepared = std::thread::scope(|scope| {
        let mut pending = Vec::with_capacity(group.len());
        for &projection in group {
            let active = &active;
            let peak = &peak;
            let context = crate::OperationContext::current_or_process();
            let worker = std::thread::Builder::new()
                .name("edb-index-prepare".into())
                .spawn_scoped(scope, move || {
                    let _scope = context.enter();
                    let count = active.fetch_add(1, Ordering::AcqRel) + 1;
                    peak.fetch_max(count, Ordering::Relaxed);
                    let result = prepare(projection);
                    active.fetch_sub(1, Ordering::AcqRel);
                    result
                });
            pending.push(match worker {
                Ok(worker) => {
                    workers += 1;
                    Ok(worker)
                }
                // A denied optional worker does not invalidate a legal build.
                Err(_) => Err(prepare(projection)),
            });
        }
        // Join every task, including after an error, before borrowed inputs or
        // build ownership can be released. No worker survives publication.
        let mut output = Vec::with_capacity(group.len());
        let mut failure = None;
        for task in pending {
            let result = match task {
                Ok(worker) => worker.join().unwrap_or_else(|_| {
                    Err(fault(
                        "index/preparation-panic",
                        "index edit preparation worker panicked",
                    ))
                }),
                Err(result) => result,
            };
            match result {
                Ok(value) => output.push(value),
                Err(error) => {
                    failure.get_or_insert(error);
                }
            }
        }
        failure.map_or(Ok(output), Err)
    })?;
    Ok((prepared, workers, peak.load(Ordering::Relaxed) as u64))
}

// Borrow the paired schema/datom changes and projection work without an owned staging wrapper.
#[allow(clippy::too_many_arguments)]
pub(crate) fn current_edits(
    order: IndexOrder,
    removals: &[Datom],
    insertions: &[Datom],
    base_schema: &crate::Schema,
    endpoint_schema: &crate::Schema,
    changed_avet: &[(u32, bool, bool)],
    avet_backfills: &BTreeMap<u32, (Vec<Datom>, Vec<Datom>)>,
    avet_drops: &BTreeMap<u32, (Vec<Datom>, Vec<Datom>)>,
) -> Result<TreeMergeEdits, SemanticError> {
    let changed = |attribute| {
        matches!(order, IndexOrder::Avet | IndexOrder::Savet)
            && changed_avet
                .iter()
                .any(|(candidate, _, _)| *candidate == attribute)
    };
    let mut edits = TreeMergeEdits::default();
    for datom in removals {
        if !changed(datom.attribute) && schema_index_member(base_schema, datom, order)? {
            edits.removals.push(datom.clone());
        }
    }
    for datom in insertions {
        if !changed(datom.attribute) && schema_index_member(endpoint_schema, datom, order)? {
            edits.insertions.push(datom.clone());
        }
    }
    if matches!(order, IndexOrder::Avet | IndexOrder::Savet) {
        for (current, _) in avet_backfills.values() {
            edits.insertions.extend(current.iter().cloned());
        }
        for (current, _) in avet_drops.values() {
            edits.projection_removals.extend(current.iter().cloned());
        }
    }
    Ok(edits)
}

pub(crate) fn history_edits(
    order: IndexOrder,
    recent: &RecentTier,
    changed_avet: &[(u32, bool, bool)],
    avet_backfills: &BTreeMap<u32, (Vec<Datom>, Vec<Datom>)>,
    avet_drops: &BTreeMap<u32, (Vec<Datom>, Vec<Datom>)>,
) -> Result<TreeMergeEdits, SemanticError> {
    let changed = |attribute| {
        matches!(order, IndexOrder::Avet | IndexOrder::Savet)
            && changed_avet
                .iter()
                .any(|(candidate, _, _)| *candidate == attribute)
    };
    let projection = recent.projection();
    let mut edits = TreeMergeEdits {
        insertions: recent
            .datoms(order)
            .iter()
            .filter(|datom| !changed(datom.attribute))
            .cloned()
            .collect(),
        // Recovered `filter-nohist-pairs` runs after old and new segment data
        // are merged. Let `merge_tree` inspect every complete selected leaf
        // stream so incidental older durable pairs in rewritten segments are
        // eligible too; untouched leaves remain opaque and unchanged.
        no_history_attributes: projection
            .schema()
            .attributes()
            .filter(|attribute| attribute.no_history && !changed(attribute.id))
            .map(|attribute| attribute.id)
            .collect(),
        ..TreeMergeEdits::default()
    };
    if matches!(order, IndexOrder::Avet | IndexOrder::Savet) {
        for (_, history) in avet_backfills.values() {
            edits.insertions.extend(history.iter().cloned());
        }
        for (_, history) in avet_drops.values() {
            edits.projection_removals.extend(history.iter().cloned());
        }
    }
    Ok(edits)
}

pub(crate) fn canonicalize_merge_edits(edits: &mut TreeMergeEdits, order: IndexOrder) {
    sort_dedup_datoms(&mut edits.removals, order);
    sort_dedup_datoms(&mut edits.projection_removals, order);
    sort_dedup_datoms(&mut edits.insertions, order);
    edits.no_history_pairs.sort_by(|left, right| {
        left.retraction
            .cmp_in(&right.retraction, order)
            .then_with(|| left.assertion.cmp_in(&right.assertion, order))
    });
}

pub(crate) fn stored_eav_key(datom: &Datom) -> Result<Vec<u8>, SemanticError> {
    let value = crate::encoding::encode_canonical_value(&datom.value)?;
    let mut key = Vec::with_capacity(12 + value.len());
    key.extend_from_slice(&datom.entity.to_be_bytes());
    key.extend_from_slice(&datom.attribute.to_be_bytes());
    key.extend_from_slice(&value);
    Ok(key)
}

fn eav_bound(exemplar: &Datom, lower: bool) -> Datom {
    Datom {
        entity: exemplar.entity,
        attribute: exemplar.attribute,
        value: exemplar.value.clone(),
        tx: if lower { u64::MAX } else { 0 },
        source: if lower { 0 } else { u64::MAX },
        added: lower,
    }
}

fn attribute_bounds(attribute: u32) -> Result<(Datom, Datom), SemanticError> {
    let end = attribute.checked_add(1).ok_or_else(|| {
        SemanticError::new(
            ErrorCategory::Unsupported,
            "index/attribute-range-overflow",
            "AVET attribute range cannot represent an exclusive upper bound",
        )
    })?;
    let bound = |attribute| Datom {
        entity: 0,
        attribute,
        value: crate::Value::Double(f64::NEG_INFINITY),
        tx: u64::MAX,
        source: 0,
        added: true,
    };
    Ok((bound(attribute), bound(end)))
}

pub(crate) trait IndexNodeReader {
    fn load_node(&mut self, hash: Digest) -> Result<Option<Vec<u8>>, SemanticError>;
}

pub(crate) fn load_old_tree_node(
    store: &mut impl IndexNodeReader,
    cache: &mut TreeNodeSet,
    hash: Digest,
) -> Result<TreeNode, SemanticError> {
    if let Some(bytes) = cache.get(&hash) {
        return decode_tree_node(&hash, bytes);
    }
    let bytes = store.load_node(hash)?.ok_or_else(|| {
        fault(
            "index/missing-tree-node",
            "published tree path references missing immutable content",
        )
    })?;
    let node = decode_tree_node(&hash, &bytes)?;
    cache.insert_known(hash, bytes)?;
    Ok(node)
}

pub(crate) fn load_old_root(
    store: &mut impl IndexNodeReader,
    descriptor: &crate::index::tree::TreeDescriptor,
    cache: &mut TreeNodeSet,
) -> Result<RootNode, SemanticError> {
    let TreeNode::Root(root) = load_old_tree_node(store, cache, descriptor.root_hash)? else {
        return Err(fault(
            "index/tree-root-kind",
            "tree descriptor resolves to a non-root node",
        ));
    };
    if root.order != descriptor.order
        || root.history != descriptor.history
        || root.count != descriptor.count
        || (root.count == 0) != root.directories.is_empty()
    {
        return Err(fault(
            "index/tree-root-content",
            "tree root disagrees with its manifest descriptor",
        ));
    }
    Ok(root)
}

pub(crate) fn load_old_directory(
    store: &mut impl IndexNodeReader,
    cache: &mut TreeNodeSet,
    reference: &ChildRef,
    order: IndexOrder,
    history: bool,
) -> Result<DirectoryNode, SemanticError> {
    let TreeNode::Directory(directory) = load_old_tree_node(store, cache, reference.hash)? else {
        return Err(fault(
            "index/tree-directory-kind",
            "root child resolves to a non-directory node",
        ));
    };
    validate_loaded_child_key(
        reference,
        directory.order,
        directory.history,
        directory.count,
        directory.leaves.first().map(|leaf| &leaf.key),
        order,
        history,
    )?;
    Ok(directory)
}

pub(crate) fn load_old_leaf(
    store: &mut impl IndexNodeReader,
    cache: &mut TreeNodeSet,
    reference: &ChildRef,
    order: IndexOrder,
    history: bool,
) -> Result<LeafSegment, SemanticError> {
    let TreeNode::Leaf(leaf) = load_old_tree_node(store, cache, reference.hash)? else {
        return Err(fault(
            "index/tree-leaf-kind",
            "directory child resolves to a non-leaf node",
        ));
    };
    let first = leaf.datom(0);
    validate_loaded_child_datom(
        reference,
        leaf.order,
        leaf.history,
        leaf.len() as u64,
        first.as_ref(),
        order,
        history,
    )?;
    Ok(leaf)
}

pub(crate) struct IndexEavLookup {
    root: RootNode,
    directory: Option<(usize, DirectoryNode)>,
    leaf: Option<(usize, usize, LeafSegment)>,
}

impl IndexEavLookup {
    pub(crate) fn new(
        store: &mut impl IndexNodeReader,
        descriptor: &crate::index::tree::TreeDescriptor,
        cache: &mut TreeNodeSet,
    ) -> Result<Self, SemanticError> {
        Ok(Self {
            root: load_old_root(store, descriptor, cache)?,
            directory: None,
            leaf: None,
        })
    }

    /// All current contributions for one strict stored E/A/V, across sources.
    pub(crate) fn all(
        &mut self,
        store: &mut impl IndexNodeReader,
        exemplar: &Datom,
        cache: &mut TreeNodeSet,
    ) -> Result<Vec<Datom>, SemanticError> {
        let lower = eav_bound(exemplar, true);
        let mut candidate = self.seek(store, &lower, false, cache)?;
        let mut contributions = Vec::new();
        while let Some(datom) = candidate {
            if !same_logical_eav(&datom, exemplar) {
                break;
            }
            if same_eav(&datom, exemplar) {
                contributions.push(datom.clone());
            }
            candidate = self.seek(store, &datom, true, cache)?;
        }
        Ok(contributions)
    }

    fn seek(
        &mut self,
        store: &mut impl IndexNodeReader,
        key: &Datom,
        strict: bool,
        cache: &mut TreeNodeSet,
    ) -> Result<Option<Datom>, SemanticError> {
        if self.root.directories.is_empty() {
            return Ok(None);
        }
        let order = self.root.order;
        let history = self.root.history;
        let first_directory = floor_tree_child(&self.root.directories, key, order);
        for directory_index in first_directory..self.root.directories.len() {
            if self
                .directory
                .as_ref()
                .is_none_or(|(index, _)| *index != directory_index)
            {
                let directory = load_old_directory(
                    store,
                    cache,
                    &self.root.directories[directory_index],
                    order,
                    history,
                )?;
                self.directory = Some((directory_index, directory));
            }
            let directory = &self.directory.as_ref().expect("loaded directory").1;
            let first_leaf = if directory_index == first_directory {
                floor_tree_child(&directory.leaves, key, order)
            } else {
                0
            };
            for leaf_index in first_leaf..directory.leaves.len() {
                if self.leaf.as_ref().is_none_or(|(directory, leaf, _)| {
                    (*directory, *leaf) != (directory_index, leaf_index)
                }) {
                    let leaf =
                        load_old_leaf(store, cache, &directory.leaves[leaf_index], order, history)?;
                    self.leaf = Some((directory_index, leaf_index, leaf));
                }
                let leaf = &self.leaf.as_ref().expect("loaded leaf").2;
                let mut index = if directory_index == first_directory && leaf_index == first_leaf {
                    leaf_lower_bound(leaf, key, order)
                } else {
                    0
                };
                while let Some(datom) = leaf.datom(index) {
                    if !strict || datom.cmp_in(key, order).is_gt() {
                        return Ok(Some(datom));
                    }
                    index += 1;
                }
            }
        }
        Ok(None)
    }
}

pub(crate) struct TreeStructuralChunk {
    pub(crate) datoms: Vec<Datom>,
    pub(crate) directory: u32,
    pub(crate) leaf: u32,
    pub(crate) slot: u32,
    pub(crate) complete: bool,
}

// Keep the durable resume coordinates and independent byte/datom limits explicit at this boundary.
#[allow(clippy::too_many_arguments)]
pub(crate) fn tree_attribute_chunk(
    store: &mut impl IndexNodeReader,
    descriptor: &crate::index::tree::TreeDescriptor,
    attribute: u32,
    resume_directory: u32,
    resume_leaf: u32,
    resume_slot: u32,
    maximum_datoms: usize,
    maximum_bytes: u64,
    cache: &mut TreeNodeSet,
) -> Result<TreeStructuralChunk, SemanticError> {
    if !matches!(descriptor.order, IndexOrder::Aevt | IndexOrder::Avet) {
        return Err(SemanticError::incorrect(
            "index/invalid-avet-projection-source",
            "AVET projection chunks require an AEVT or AVET source",
        ));
    }
    if maximum_datoms == 0 || maximum_bytes == 0 {
        return Err(SemanticError::incorrect(
            "index/invalid-avet-projection-limit",
            "AVET projection chunk limits must be positive",
        ));
    }
    let (start, end) = attribute_bounds(attribute)?;
    let root = load_old_root(store, descriptor, cache)?;
    if root.directories.is_empty() {
        return Ok(TreeStructuralChunk {
            datoms: Vec::new(),
            directory: 0,
            leaf: 0,
            slot: 0,
            complete: true,
        });
    }

    let fresh = resume_directory == 0 && resume_leaf == 0 && resume_slot == 0;
    let mut directory_index = if fresh {
        floor_tree_child(&root.directories, &start, descriptor.order)
    } else {
        usize::try_from(resume_directory).map_err(|_| {
            fault(
                "index/avet-projection-cursor",
                "AVET projection directory cursor exceeds usize",
            )
        })?
    };
    if directory_index >= root.directories.len() {
        return Err(fault(
            "index/avet-projection-cursor",
            "AVET projection directory cursor is outside its immutable source root",
        ));
    }
    let mut output = Vec::new();
    let mut retained_bytes = 0_u64;
    while directory_index < root.directories.len() {
        let reference = &root.directories[directory_index];
        if !reference.key.cmp_datom(&end, descriptor.order).is_lt() {
            return Ok(TreeStructuralChunk {
                datoms: output,
                directory: 0,
                leaf: 0,
                slot: 0,
                complete: true,
            });
        }
        let directory = load_old_directory(
            store,
            cache,
            reference,
            descriptor.order,
            descriptor.history,
        )?;
        let mut leaf_index = if fresh
            && directory_index == floor_tree_child(&root.directories, &start, descriptor.order)
        {
            floor_tree_child(&directory.leaves, &start, descriptor.order)
        } else {
            usize::try_from(resume_leaf).map_err(|_| {
                fault(
                    "index/avet-projection-cursor",
                    "AVET projection leaf cursor exceeds usize",
                )
            })?
        };
        if !fresh && directory_index != usize::try_from(resume_directory).unwrap_or(usize::MAX) {
            leaf_index = 0;
        }
        while leaf_index < directory.leaves.len() {
            let leaf_ref = &directory.leaves[leaf_index];
            if !leaf_ref.key.cmp_datom(&end, descriptor.order).is_lt() {
                return Ok(TreeStructuralChunk {
                    datoms: output,
                    directory: 0,
                    leaf: 0,
                    slot: 0,
                    complete: true,
                });
            }
            let leaf = load_old_leaf(store, cache, leaf_ref, descriptor.order, descriptor.history)?;
            let mut slot = if fresh
                && directory_index == floor_tree_child(&root.directories, &start, descriptor.order)
                && leaf_index == floor_tree_child(&directory.leaves, &start, descriptor.order)
            {
                leaf_lower_bound(&leaf, &start, descriptor.order)
            } else if !fresh
                && directory_index == usize::try_from(resume_directory).unwrap_or(usize::MAX)
                && leaf_index == usize::try_from(resume_leaf).unwrap_or(usize::MAX)
            {
                usize::try_from(resume_slot).map_err(|_| {
                    fault(
                        "index/avet-projection-cursor",
                        "AVET projection slot cursor exceeds usize",
                    )
                })?
            } else {
                0
            };
            if slot > leaf.len() {
                return Err(fault(
                    "index/avet-projection-cursor",
                    "AVET projection slot cursor is outside its immutable source leaf",
                ));
            }
            while slot < leaf.len() {
                let datom = leaf.datom(slot).expect("validated leaf columns");
                if datom.cmp_in(&start, descriptor.order).is_lt() {
                    slot += 1;
                    continue;
                }
                if !datom.cmp_in(&end, descriptor.order).is_lt() {
                    return Ok(TreeStructuralChunk {
                        datoms: output,
                        directory: 0,
                        leaf: 0,
                        slot: 0,
                        complete: true,
                    });
                }
                let datom_bytes = datom.retained_bytes();
                if !output.is_empty()
                    && (output.len() >= maximum_datoms
                        || retained_bytes.saturating_add(datom_bytes) > maximum_bytes)
                {
                    return Ok(TreeStructuralChunk {
                        datoms: output,
                        directory: u32::try_from(directory_index).map_err(|_| {
                            fault(
                                "index/avet-projection-cursor",
                                "AVET projection directory cursor exceeds u32",
                            )
                        })?,
                        leaf: u32::try_from(leaf_index).map_err(|_| {
                            fault(
                                "index/avet-projection-cursor",
                                "AVET projection leaf cursor exceeds u32",
                            )
                        })?,
                        slot: u32::try_from(slot).map_err(|_| {
                            fault(
                                "index/avet-projection-cursor",
                                "AVET projection slot cursor exceeds u32",
                            )
                        })?,
                        complete: false,
                    });
                }
                retained_bytes = retained_bytes.saturating_add(datom_bytes);
                output.push(datom);
                slot += 1;
            }
            leaf_index += 1;
        }
        directory_index += 1;
    }
    Ok(TreeStructuralChunk {
        datoms: output,
        directory: 0,
        leaf: 0,
        slot: 0,
        complete: true,
    })
}

pub(crate) fn merge_index_tree(
    store: &mut impl IndexNodeReader,
    descriptor: &crate::index::tree::TreeDescriptor,
    preloaded: &TreeNodeSet,
    edits: &TreeMergeEdits,
    config: &TreeConfig,
) -> Result<crate::index::tree::TreeMerge, SemanticError> {
    merge_tree_with_boundary_loader(descriptor, preloaded, edits, config, &mut |hash| {
        store.load_node(*hash)?.ok_or_else(|| {
            fault(
                "index/missing-tree-node",
                "published tree boundary references missing immutable content",
            )
        })
    })
}

pub(crate) fn preload_merge_paths(
    store: &mut impl IndexNodeReader,
    descriptor: &crate::index::tree::TreeDescriptor,
    edits: &TreeMergeEdits,
    cache: &mut TreeNodeSet,
) -> Result<(), SemanticError> {
    let root = load_old_root(store, descriptor, cache)?;
    if root.directories.is_empty() {
        return Ok(());
    }
    let mut points = edits
        .removals
        .iter()
        .chain(&edits.projection_removals)
        .chain(&edits.insertions)
        .cloned()
        .collect::<Vec<_>>();
    for pair in &edits.no_history_pairs {
        points.push(pair.retraction.clone());
        points.push(pair.assertion.clone());
    }
    sort_dedup_datoms(&mut points, descriptor.order);
    let selected = select_merge_leaves(&root, &points, |reference| {
        load_old_directory(
            store,
            cache,
            reference,
            descriptor.order,
            descriptor.history,
        )
    })?;
    for (directory_index, leaf_index) in selected.leaves {
        let leaf_ref = &selected.directories[&directory_index].leaves[leaf_index];
        load_old_leaf(store, cache, leaf_ref, descriptor.order, descriptor.history)?;
    }
    Ok(())
}

struct SelectedMergeLeaves {
    directories: BTreeMap<usize, DirectoryNode>,
    leaves: BTreeSet<(usize, usize)>,
}

fn select_merge_leaves(
    root: &RootNode,
    points: &[Datom],
    mut load_directory: impl FnMut(&ChildRef) -> Result<DirectoryNode, SemanticError>,
) -> Result<SelectedMergeLeaves, SemanticError> {
    let mut directories = BTreeMap::new();
    let mut leaves = BTreeSet::new();
    for point in points {
        let directory_index = floor_tree_child(&root.directories, point, root.order);
        let directory = match directories.entry(directory_index) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(load_directory(&root.directories[directory_index])?)
            }
        };
        // The affected directory itself is re-encoded even when only one
        // interior leaf changes. Root separator repair authenticates its new
        // first and last datoms, which may live in otherwise untouched leaves.
        leaves.insert((directory_index, 0));
        leaves.insert((directory_index, directory.leaves.len() - 1));
        let leaf_index = floor_tree_child(&directory.leaves, point, root.order);
        let first_leaf = leaf_index.saturating_sub(1);
        let last_leaf = leaf_index
            .saturating_add(1)
            .min(directory.leaves.len().saturating_sub(1));
        for leaf_index in first_leaf..=last_leaf {
            leaves.insert((directory_index, leaf_index));
        }

        // Rewriting a leaf or directory can change the sparse separator on
        // either side even though those neighboring children are themselves
        // reused. `merge_tree` authenticates those boundary datoms when it
        // repairs routing keys, so preload exactly those adjacent paths too.
        if let Some(previous_index) = directory_index.checked_sub(1) {
            let previous = match directories.entry(previous_index) {
                std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(load_directory(&root.directories[previous_index])?)
                }
            };
            leaves.insert((previous_index, previous.leaves.len() - 1));
        }
        let next_index = directory_index + 1;
        if let Some(next_ref) = root.directories.get(next_index) {
            if let std::collections::btree_map::Entry::Vacant(entry) = directories.entry(next_index)
            {
                entry.insert(load_directory(next_ref)?);
            }
            leaves.insert((next_index, 0));
        }
    }
    Ok(SelectedMergeLeaves {
        directories,
        leaves,
    })
}

fn fault(code: &'static str, message: impl Into<String>) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, code, message)
}
