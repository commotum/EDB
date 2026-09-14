use super::*;

#[derive(Default)]
pub(super) struct Inventory {
    pub(super) count: usize,
    pub(super) values: BTreeMap<ObjectId, DatabaseValueRoot>,
    pub(super) indexes: BTreeMap<u64, BTreeSet<ObjectId>>,
    pub(super) metadata: BTreeMap<u64, Vec<SnapshotMetadata>>,
    pub(super) receipts: BTreeSet<ObjectId>,
}

/// Authenticate the complete reachable graph, including program references
/// embedded in tree values, log bodies and native query/program literals.
/// Deep mode also validates tree routing/counts and typed descriptor codecs.
pub(crate) fn walk_repository(
    directory: &Path,
    roots: &[ObjectId],
    control: &MaintenanceControl,
    deep: bool,
) -> Result<usize, SemanticError> {
    Ok(inventory(directory, roots, control, deep)?.count)
}

pub(super) fn inventory(
    directory: &Path,
    roots: &[ObjectId],
    control: &MaintenanceControl,
    deep: bool,
) -> Result<Inventory, SemanticError> {
    let mut reader = Reader { directory, control };
    let mut seen = BTreeSet::new();
    let mut programs = BTreeSet::new();
    let mut pending = roots.to_vec();
    let mut result = Inventory::default();
    while let Some(id) = pending.pop() {
        control.check()?;
        if !seen.insert(id) {
            continue;
        }
        let bytes = reader.read_object(id)?;
        pending.extend(crate::storage::ownership::object_children(
            &mut reader,
            id,
            &bytes,
        )?);
        if bytes.starts_with(b"ATIX") {
            if let TreeNode::Leaf(leaf) = crate::index::tree::decode_tree_node(&id, &bytes)? {
                for value in &leaf.values {
                    crate::program_bindings::collect_program_hashes(value, &mut programs);
                }
            }
        } else if bytes.starts_with(b"ATMC") {
            let program = crate::decode_program(&bytes)?;
            let mut dependencies = Vec::new();
            crate::program_bindings::collect_fixed_program_dependencies(
                &program.instructions,
                &mut dependencies,
            );
            programs.extend(dependencies);
        } else {
            let block = Block::decode(&id, &bytes)?;
            match block.kind {
                DATABASE_VALUE_ROOT_KIND => {
                    result
                        .values
                        .insert(id, DatabaseValueRoot::decode(&id, &bytes)?);
                }
                INDEX_DESCRIPTOR_KIND => {
                    let descriptor = IndexDescriptor::decode(&id, &bytes)?;
                    if deep {
                        for tree in descriptor
                            .trees
                            .iter()
                            .chain(descriptor.avet_work.iter().map(|w| &w.source))
                        {
                            crate::index::tree::validate_tree_streaming(tree, |id| {
                                reader.read_object(*id)
                            })?;
                        }
                    }
                    result
                        .indexes
                        .entry(descriptor.basis)
                        .or_default()
                        .insert(id);
                }
                SNAPSHOT_METADATA_KIND => {
                    let metadata = SnapshotMetadata::decode(&id, &bytes)?;
                    result
                        .metadata
                        .entry(metadata.basis)
                        .or_default()
                        .push(metadata);
                }
                RECEIPT_KIND => {
                    result.receipts.insert(id);
                }
                crate::storage::log::LOG_ENTRY_KIND => {
                    programs.extend(crate::storage::log::entry_program_links(
                        &mut reader,
                        &block,
                    )?);
                }
                _ => {}
            }
        }
    }
    // A hash-valid tree or log object in a Function slot is not an executable
    // definition. Preserve that typed edge check as well as graph reachability.
    for id in programs {
        crate::decode_program(&reader.read_object(id)?)?;
    }
    result.count = seen.len();
    Ok(result)
}
