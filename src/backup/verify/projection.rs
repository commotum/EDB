use super::*;

pub(super) fn check_basis(
    reader: &mut Reader<'_>,
    database: &Database,
    endpoint: &SnapshotMetadata,
    inventory: &mut Inventory,
    values: &mut BTreeMap<u64, Vec<DatabaseValueRoot>>,
    deep: bool,
) -> Result<(), SemanticError> {
    let basis = database.basis_t();
    for metadata in inventory.metadata.remove(&basis).unwrap_or_default() {
        if metadata.identity != endpoint.identity
            || metadata.generation != endpoint.generation
            || metadata.eidx_frontier != database.eidx_frontier()
            || Some(metadata.reserved_frontier)
                != database.reserved_allocation().map(|a| a.frontier())
            || metadata.last_tx_instant != database.last_tx_instant()
        {
            return Err(fault(
                "backup/metadata-semantic-mismatch",
                "Read metadata differs from its canonical replay checkpoint",
            ));
        }
    }
    values.remove(&basis);
    for id in inventory.indexes.remove(&basis).unwrap_or_default() {
        let index = IndexDescriptor::decode(&id, &reader.read_object(id)?)?;
        if index.identity != endpoint.identity || index.generation != endpoint.generation {
            return Err(fault(
                "backup/index-generation",
                "Retained index belongs to another generation",
            ));
        }
        if deep {
            verify_index(reader, database, &index)?;
        }
    }
    Ok(())
}

pub(super) fn read_tree(
    reader: &mut Reader<'_>,
    descriptor: &TreeDescriptor,
) -> Result<Vec<Datom>, SemanticError> {
    let mut datoms = Vec::new();
    crate::index::tree::validate_tree_streaming(descriptor, |id| {
        let bytes = reader.read_object(*id)?;
        if let TreeNode::Leaf(leaf) = crate::index::tree::decode_tree_node(id, &bytes)? {
            for row in 0..leaf.len() {
                datoms.push(leaf.datom(row).expect("decoded leaf columns"));
            }
        }
        Ok(bytes)
    })?;
    datoms.sort_by(|a, b| a.cmp_in(b, descriptor.order));
    Ok(datoms)
}

pub(super) fn verify_index(
    reader: &mut Reader<'_>,
    database: &Database,
    index: &IndexDescriptor,
) -> Result<(), SemanticError> {
    let current = database.datoms(View::Current, IndexOrder::Eavt);
    let replayed = database.datoms(View::History, IndexOrder::Eavt);
    let physical = read_tree(reader, &index.trees[4])?;
    validate_physical_history_projection(&replayed, &physical, database.basis_t())
        .map_err(|e| fault("backup/tree-semantic-mismatch", e.message))?;
    for tree in &index.trees {
        reader.control.check()?;
        if tree.history && tree.order == IndexOrder::Eavt {
            continue;
        }
        let source = if tree.history { &physical } else { &current };
        let mut expected = derive_index_projection(database, source, tree.order)?;
        let mut actual = read_tree(reader, tree)?;
        if tree.order == IndexOrder::Avet {
            let pending =
                verify_pending(reader, index, database, &actual, &replayed, tree.history)?;
            expected.retain(|d| !pending.contains(&d.attribute));
            actual.retain(|d| !pending.contains(&d.attribute));
        }
        if !same_stored_datoms(&actual, &expected) {
            return Err(fault(
                "backup/tree-semantic-mismatch",
                "Covering tree disagrees with canonical replay",
            )
            .detail("basis_t", database.basis_t().to_string())
            .detail("order", format!("{:?}", tree.order))
            .detail("history", tree.history.to_string()));
        }
    }
    if index.fulltext.is_some() {
        verify_fulltext(reader, database, index, &physical)?;
    }
    Ok(())
}

pub(super) fn verify_fulltext(
    reader: &mut Reader<'_>,
    database: &Database,
    index: &IndexDescriptor,
    history: &[Datom],
) -> Result<(), SemanticError> {
    let projection =
        crate::storage::fulltext::load_projection(index, &mut |id| reader.read_object(id))?;
    let mut source = history.to_vec();
    source.sort_by(|a, b| a.cmp_in(b, IndexOrder::Aevt));
    let records = crate::fulltext::records_from(database.schema(), |attribute| {
        let first = source.partition_point(|d| d.attribute < attribute);
        let end = source.partition_point(|d| d.attribute <= attribute);
        Ok(Box::new(source[first..end].iter().cloned().map(Ok)))
    });
    let limits = crate::FulltextBuildLimits {
        max_records: u64::MAX,
        max_spill_bytes: u64::MAX,
        ..Default::default()
    };
    let mut expected = crate::fulltext::store::sorted_records(
        records.map(|r| {
            reader.control.check()?;
            r
        }),
        &limits,
        &mut Default::default(),
    )?;
    let directory = reader.directory.to_path_buf();
    let control = reader.control.clone();
    let mut observed = crate::FulltextCursor::new(
        &projection,
        &[],
        crate::FulltextReadLimits {
            max_records: u64::MAX,
            max_block_bytes: u64::MAX,
        },
        Box::new(move |id, stats| {
            control.check()?;
            let (page, blocks, bytes) = crate::storage::fulltext::read_page(id, &mut |id| {
                control.check()?;
                read_object(&directory, id)
            })?;
            stats.blocks_read = stats.blocks_read.saturating_add(blocks);
            stats.block_bytes = stats.block_bytes.saturating_add(bytes);
            Ok(std::sync::Arc::new(page))
        }),
    )?;
    loop {
        reader.control.check()?;
        let expected = expected.next().transpose()?;
        let actual = observed.next().transpose()?;
        if expected != actual {
            return Err(fault(
                "backup/fulltext-semantic-mismatch",
                "Search records disagree with their canonical history source",
            ));
        }
        if expected.is_none() {
            return Ok(());
        }
    }
}

pub(super) fn verify_pending(
    reader: &mut Reader<'_>,
    index: &IndexDescriptor,
    database: &Database,
    actual: &[Datom],
    replayed: &[Datom],
    history: bool,
) -> Result<BTreeSet<u32>, SemanticError> {
    let mut pending = BTreeSet::new();
    for attribute in &index.pending_avet {
        if !crate::index::metadata::effective_avet(database.schema(), *attribute) {
            return Err(fault(
                "backup/avet-direction",
                "Pending AVET attribute is not indexed in its frozen schema",
            ));
        }
        if !index.avet_work.iter().any(|w| w.attribute == *attribute) {
            pending.insert(*attribute);
        }
    }
    for work in &index.avet_work {
        if work.adding != crate::index::metadata::effective_avet(database.schema(), work.attribute)
            || work.source != index.trees[usize::from(work.history) * 4 + 1]
        {
            return Err(fault(
                "backup/avet-source",
                "AVET work disagrees with frozen schema or AEVT source",
            ));
        }
        if work.history && !history {
            continue;
        }
        pending.insert(work.attribute);
        if work.history == history && !work.clearing {
            let expected = projection_prefix(reader, work)?;
            let observed = actual
                .iter()
                .filter(|d| d.attribute == work.attribute)
                .cloned()
                .collect::<Vec<_>>();
            if !same_stored_datoms(&observed, &expected) {
                return Err(fault(
                    "backup/avet-prefix",
                    "Pending AVET copy is not its exact authenticated source prefix",
                ));
            }
        }
    }
    // Clearing/readiness-only ranges may retain old facts, but never fabricated
    // facts. Copy prefixes above receive the stronger exact-position proof.
    for datom in actual.iter().filter(|d| pending.contains(&d.attribute)) {
        let at =
            replayed.partition_point(|candidate| candidate.cmp_in(datom, IndexOrder::Eavt).is_lt());
        if (!history && !datom.added)
            || !replayed[at..]
                .iter()
                .take_while(|row| row.cmp_in(datom, IndexOrder::Eavt).is_eq())
                .any(|row| {
                    same_stored_datoms(std::slice::from_ref(row), std::slice::from_ref(datom))
                })
        {
            return Err(fault(
                "backup/avet-fabricated",
                "Pending AVET contains a fact absent from canonical history",
            ));
        }
    }
    Ok(pending)
}

pub(super) fn projection_prefix(
    reader: &mut Reader<'_>,
    work: &crate::storage::BlockAvetWork,
) -> Result<Vec<Datom>, SemanticError> {
    let bytes = reader.read_object(work.source.root_hash)?;
    let TreeNode::Root(root) =
        crate::index::tree::decode_tree_node(&work.source.root_hash, &bytes)?
    else {
        return Err(fault("backup/avet-root", "AVET source is not a root"));
    };
    let mut output = Vec::new();
    let mut coordinate_found = false;
    for (directory_index, child) in root.directories.iter().enumerate() {
        if directory_index > work.directory as usize {
            break;
        }
        let bytes = reader.read_object(child.hash)?;
        let TreeNode::Directory(directory) =
            crate::index::tree::decode_tree_node(&child.hash, &bytes)?
        else {
            return Err(fault(
                "backup/avet-directory",
                "AVET source directory is invalid",
            ));
        };
        for (leaf_index, child) in directory.leaves.iter().enumerate() {
            if directory_index == work.directory as usize && leaf_index > work.leaf as usize {
                break;
            }
            let bytes = reader.read_object(child.hash)?;
            let TreeNode::Leaf(leaf) = crate::index::tree::decode_tree_node(&child.hash, &bytes)?
            else {
                return Err(fault("backup/avet-leaf", "AVET source leaf is invalid"));
            };
            let boundary =
                directory_index == work.directory as usize && leaf_index == work.leaf as usize;
            let count = if boundary {
                coordinate_found = (work.slot as usize) < leaf.len();
                (work.slot as usize).min(leaf.len())
            } else {
                leaf.len()
            };
            for slot in 0..count {
                let datom = leaf.datom(slot).expect("decoded leaf columns");
                if datom.attribute == work.attribute {
                    output.push(datom);
                }
            }
        }
    }
    // A freshly started population phase has no copied facts and position 0.
    if !(coordinate_found
        || work.directory == 0 && work.leaf == 0 && work.slot == 0 && root.directories.is_empty())
    {
        return Err(fault(
            "backup/avet-position",
            "AVET continuation lies outside its authenticated source",
        ));
    }
    output.sort_by(|a, b| a.cmp_in(b, IndexOrder::Avet));
    Ok(output)
}
