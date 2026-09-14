use super::*;

pub(super) fn metadata_for(
    reader: &mut Reader<'_>,
    value: &DatabaseValueRoot,
) -> Result<SnapshotMetadata, SemanticError> {
    let id = value
        .metadata
        .ok_or_else(|| fault("backup/value-metadata", "Retained value has no metadata"))?;
    let metadata = SnapshotMetadata::decode(&id, &reader.read_object(id)?)?;
    if metadata.identity != value.identity || metadata.basis != value.basis {
        return Err(fault(
            "backup/value-metadata",
            "Retained value and metadata disagree",
        ));
    }
    Ok(metadata)
}

pub(super) fn validate_authorization(
    reader: &mut Reader<'_>,
    publication: &DatabaseRoot,
    inventory: &Inventory,
) -> Result<(), SemanticError> {
    use crate::storage::read_authorization::{IndexAuthorization, ReadAuthorization, index_key};
    let id = publication.read_authorization.ok_or_else(|| {
        fault(
            "backup/read-authorization",
            "Publication has no read authorization",
        )
    })?;
    let authorization = ReadAuthorization::decode(&id, &reader.read_object(id)?)?;
    let initial = inventory
        .values
        .get(&authorization.initial_value())
        .ok_or_else(|| {
            fault(
                "backup/read-authorization",
                "Initial read anchor is absent from the repository graph",
            )
        })?;
    if authorization.identity() != publication.identity
        || initial.identity != publication.identity
        || initial.basis != authorization.initial_basis()
    {
        return Err(fault(
            "backup/read-authorization",
            "Initial read authorization belongs to another committed value",
        ));
    }
    let registry = RequestIndex::from_root(Some(authorization.indexes_root()));
    let mut after = None;
    loop {
        let page = registry.scan(reader, after, 256)?;
        if page.is_empty() {
            break;
        }
        for (key, id) in &page {
            let witness = IndexAuthorization::decode(id, &reader.read_object(*id)?)?;
            if witness.identity != publication.identity
                || *key != index_key(&publication.identity, witness.index)
            {
                return Err(fault(
                    "backup/read-authorization",
                    "Index publication witness differs from its scoped registry key",
                ));
            }
            let index =
                IndexDescriptor::decode(&witness.index, &reader.read_object(witness.index)?)?;
            if index.identity != publication.identity || index.basis > publication.basis {
                return Err(fault(
                    "backup/read-authorization",
                    "Authorized index differs from its canonical publication",
                ));
            }
        }
        after = page.last().map(|entry| entry.0);
    }
    Ok(())
}

pub(super) fn validate_receipts(
    reader: &mut Reader<'_>,
    publication: &DatabaseRoot,
    log: &LogRoot,
    inventory: &Inventory,
) -> Result<(), SemanticError> {
    let requests = RequestIndex::from_root(publication.receipts);
    let mut mapped = BTreeSet::new();
    let mut after = None;
    loop {
        let page = requests.scan(reader, after, 256)?;
        if page.is_empty() {
            break;
        }
        for (_, id) in &page {
            let bytes = reader.read_object(*id)?;
            let block = Block::decode(id, &bytes)?;
            if block.kind == crate::storage::excision::TOMBSTONE_KIND {
                let error = crate::storage::excision::reject_tombstone_bytes(
                    *id,
                    &bytes,
                    &publication.identity,
                )
                .unwrap_err();
                if error.code != "postgres/idempotency-predates-excision" {
                    return Err(error);
                }
                continue;
            }
            if block.kind != RECEIPT_KIND {
                return Err(fault(
                    "backup/request-outcome",
                    "Request index points to an invalid outcome",
                ));
            }
            if !mapped.insert(*id) {
                continue;
            }
            let receipt = ExactReceipt::load_from_bytes(reader, *id, &bytes)?;
            let before = inventory
                .values
                .get(&receipt.before)
                .ok_or_else(|| fault("backup/receipt-value", "Receipt before value is absent"))?;
            let after_value = inventory
                .values
                .get(&receipt.after)
                .ok_or_else(|| fault("backup/receipt-value", "Receipt after value is absent"))?;
            if receipt.identity != publication.identity
                || before.identity != receipt.identity
                || after_value.identity != receipt.identity
                || before.basis.checked_add(1) != Some(receipt.basis)
                || after_value.basis != receipt.basis
            {
                return Err(fault(
                    "backup/receipt-coordinate",
                    "Receipt values do not describe one canonical transaction",
                ));
            }
            let record = log
                .read_record(reader, receipt.basis)?
                .ok_or_else(|| fault("backup/receipt-log", "Receipt transaction is missing"))?;
            if record.id != receipt.transaction {
                return Err(fault(
                    "backup/receipt-log",
                    "Receipt transaction differs from canonical log",
                ));
            }
            crate::storage::receipts::validate_receipt_frontiers(
                &receipt.tempids,
                receipt.basis,
                record.entry.eidx_frontier,
                record.entry.reserved_frontier,
            )?;
            if requests.lookup(reader, basis_receipt_key(&receipt.identity, receipt.basis))?
                != Some(*id)
            {
                return Err(fault(
                    "backup/receipt-address",
                    "Receipt has no matching canonical basis address",
                ));
            }
        }
        after = page.last().map(|entry| entry.0);
    }
    if mapped != inventory.receipts {
        return Err(fault(
            "backup/receipt-map",
            "Reachable receipt is not a current canonical request outcome",
        ));
    }
    Ok(())
}
