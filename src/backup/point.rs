use super::*;

pub(crate) fn read_object(directory: &Path, id: Digest) -> Result<Vec<u8>, SemanticError> {
    repository::read_object(directory, id)
}
pub(crate) fn load_publication(
    directory: &Path,
    point: &ReadPoint,
) -> Result<DatabaseRoot, SemanticError> {
    DatabaseRoot::decode(
        &point.publication,
        &read_object(directory, point.publication)?,
    )
}
pub(crate) fn open_read_point(
    directory: &Path,
    selection: Option<&BackupPoint>,
) -> Result<ReadPoint, SemanticError> {
    let expected;
    let point = match selection {
        Some(point) => point,
        None => {
            expected = PortableBackup::list_backup_points(directory)?
                .pop()
                .ok_or_else(|| fault("backup/no-points", "No completed backup point"))?;
            &expected
        }
    };
    let actual = load_selected(directory, Some(point.log_generation), point.basis_t)?;
    if actual.point.manifest_hash != point.manifest_hash
        || actual.point.lineage_id != point.lineage_id
    {
        return Err(fault(
            "backup/selected-point",
            "Selected point differs from the repository",
        ));
    }
    Ok(actual)
}
pub(crate) fn load_selected(
    directory: &Path,
    generation: Option<u64>,
    basis: u64,
) -> Result<ReadPoint, SemanticError> {
    repository::admit(directory, false)?;
    let generation = match generation {
        Some(generation) => generation,
        None => PortableBackup::list_backup_points(directory)?
            .into_iter()
            .filter(|point| point.basis_t == basis)
            .map(|point| point.log_generation)
            .max()
            .ok_or_else(|| fault("backup/no-point", "Requested backup basis is absent"))?,
    };
    let (point, links, identity) = point_header(directory, generation, basis)?;
    let mut open_object_reads = 0u64;
    let mut open_object_bytes = 0u64;
    let mut read = |id| {
        let bytes = read_object(directory, id)?;
        open_object_reads += 1;
        open_object_bytes += bytes.len() as u64;
        Ok::<_, SemanticError>(bytes)
    };
    let publication = DatabaseRoot::decode(&links[0], &read(links[0])?)?;
    let value = DatabaseValueRoot::from(&publication);
    if publication.identity != identity || publication.basis != basis {
        return Err(fault(
            "backup/manifest-coordinate",
            "Backup roots disagree on committed coordinates",
        ));
    }
    let metadata_id = value
        .metadata
        .ok_or_else(|| fault("backup/metadata", "Value lacks metadata"))?;
    let metadata = SnapshotMetadata::decode(&metadata_id, &read(metadata_id)?)?;
    let index_id = value
        .indexes
        .ok_or_else(|| fault("backup/read-index", "Value lacks indexes"))?;
    let indexes = IndexDescriptor::decode(&index_id, &read(index_id)?)?;
    if metadata.identity != identity
        || metadata.basis != basis
        || metadata.generation != generation
        || indexes.identity != identity
        || indexes.basis > basis
        || indexes.generation != generation
    {
        return Err(fault(
            "backup/read-coordinate",
            "Backup index and metadata disagree with the captured publication",
        ));
    }
    Ok(ReadPoint {
        point,
        publication: links[0],
        value,
        open_object_reads,
        open_object_bytes,
    })
}

pub(super) fn point_header(
    directory: &Path,
    generation: u64,
    basis: u64,
) -> Result<(BackupPoint, [ObjectId; 1], [u8; 16]), SemanticError> {
    let bytes =
        repository::read_bounded(&repository::point_path(directory, generation, basis), 4096)?;
    let manifest_hash = crate::sha256(&bytes);
    let block = Block::decode(&manifest_hash, &bytes)?;
    if block.kind != POINT_KIND || block.links.len() != 1 || block.payload.len() != 32 {
        return Err(fault(
            "backup/manifest-format",
            "Expected a current single-root backup point",
        ));
    }
    let identity: [u8; 16] = block.payload[..16].try_into().unwrap();
    if u64::from_be_bytes(block.payload[16..24].try_into().unwrap()) != generation
        || u64::from_be_bytes(block.payload[24..32].try_into().unwrap()) != basis
    {
        return Err(fault(
            "backup/manifest-coordinate",
            "Point filename and content disagree",
        ));
    }
    repository::verify_claim(directory, identity)?;
    if identity == [0; 16] {
        return Err(fault("backup/lineage", "Backup lineage is empty"));
    }
    Ok((
        BackupPoint {
            lineage_id: crate::storage::catalog::identity_string(identity),
            log_generation: generation,
            basis_t: basis,
            manifest_hash,
            objects_written: 0,
            objects_reused: 0,
        },
        [block.links[0]],
        identity,
    ))
}
pub(super) fn same_information(
    left: &DatabaseRoot,
    right: &DatabaseRoot,
) -> Result<(), SemanticError> {
    if left.identity != right.identity
        || left.basis != right.basis
        || left.log != right.log
        || left.metadata != right.metadata
        || left.receipts != right.receipts
    {
        Err(fault(
            "backup/point-conflict",
            "Existing point has different committed information",
        ))
    } else {
        Ok(())
    }
}
pub(super) fn parse_point_name(name: &str) -> Result<(u64, u64), SemanticError> {
    let bad = || fault("backup/point-name", "Invalid current-format point filename");
    let bytes = name.as_bytes();
    if bytes.len() != 38
        || bytes[16] != b'-'
        || !bytes.ends_with(b".root")
        || !bytes[..16]
            .iter()
            .chain(&bytes[17..33])
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
    {
        return Err(bad());
    }
    Ok((
        u64::from_str_radix(&name[..16], 16).map_err(|_| bad())?,
        u64::from_str_radix(&name[17..33], 16).map_err(|_| bad())?,
    ))
}
