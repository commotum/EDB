//! Generic bounded immutable upload, sharing single-put protection and codec.
use super::*;

impl PgBlockStore {
    /// Persist at most 128 objects / 64 MiB of canonical bytes in one database
    /// transaction. Input order and duplicate IDs are preserved in the result.
    /// Authentication failure rolls back every insert and protection touch.
    pub fn put_many(&mut self, payloads: &[&[u8]]) -> Result<Vec<ObjectId>, SemanticError> {
        let protection = self.write_protection.clone();
        let (epoch, conditions) = protection
            .as_ref()
            .map_or((0, &[][..]), |p| (p.epoch, p.conditions.as_slice()));
        match self.put_many_protected(payloads, epoch, conditions)? {
            Guarded::Applied(ids) => Ok(ids),
            Guarded::Conflict(_) => Err(SemanticError::conflict(
                "storage/write-protection-conflict",
                "Immutable write authority changed during the publication attempt",
            )),
        }
    }

    /// Guarded bulk counterpart of `put_protected`. Even existing objects are
    /// touched at the supplied epoch before a successful batch can return.
    pub fn put_many_protected(
        &mut self,
        payloads: &[&[u8]],
        epoch: u64,
        conditions: &[RefCondition],
    ) -> Result<Guarded<Vec<ObjectId>>, SemanticError> {
        super::super::object_io::validate_write_batch(payloads)?;
        // An unguarded immutable upload is valid, just like `put`. The
        // reference-batch validator intentionally rejects a wholly empty CAS;
        // there is no CAS operation to validate in this case.
        if !conditions.is_empty() {
            super::super::protocol::validate_batch(conditions, &[])?;
        }
        let epoch = sql_epoch(epoch)?;
        if payloads.is_empty() && conditions.is_empty() {
            return Ok(Guarded::Applied(Vec::new()));
        }
        let ids = payloads
            .iter()
            .map(|bytes| sha256(bytes))
            .collect::<Vec<_>>();
        // Stable lock order also prevents two overlapping upload batches from
        // deadlocking on the same immutable object rows in different orders.
        let unique = ids
            .iter()
            .copied()
            .zip(payloads.iter().copied())
            .collect::<BTreeMap<_, _>>();
        let unique_ids = unique.keys().copied().collect::<Vec<_>>();
        let encoded = unique
            .values()
            .map(|bytes| crate::storage::codec::encode_block(bytes))
            .collect::<Result<Vec<_>, _>>()?;
        let keys = unique_ids
            .iter()
            .map(|id| id.as_slice())
            .collect::<Vec<_>>();
        let physical = encoded.iter().map(Vec::as_slice).collect::<Vec<_>>();
        let mut tx = self
            .client
            .build_transaction()
            .isolation_level(postgres::IsolationLevel::ReadCommitted)
            .start()
            .map_err(read_error)?;
        let current = guarded_refs(&mut tx, &self.namespace, &self.refs, conditions)?;
        if conditions
            .iter()
            .any(|guard| current[&guard.key].as_ref().map(|r| r.revision) != guard.expected)
        {
            tx.rollback().map_err(read_error)?;
            return Ok(Guarded::Conflict(current.into_iter().collect()));
        }
        if payloads.is_empty() {
            tx.commit().map_err(commit_error)?;
            return Ok(Guarded::Applied(Vec::new()));
        }
        // Existing representation remains immutable. The server checks the
        // aggregate physical length before returning any payloads to the driver.
        let rows = tx.query(&format!(
            "WITH inserted AS (INSERT INTO {} AS objects(id,payload,protected_epoch) \
             SELECT id,payload,$3::bigint FROM unnest($1::bytea[],$2::bytea[]) AS input(id,payload) ORDER BY id \
             ON CONFLICT(id) DO UPDATE SET protected_epoch=GREATEST(objects.protected_epoch,EXCLUDED.protected_epoch) \
             RETURNING id,payload) SELECT id,CASE WHEN sum(octet_length(payload)::bigint) OVER () <= $4::bigint \
             THEN payload ELSE NULL END FROM inserted ORDER BY id", self.objects),
            &[&keys, &physical, &epoch, &(MAX_BLOCK_BYTES as i64)]).map_err(read_error)?;
        if rows.len() != unique.len() {
            return Err(corrupt());
        }
        let mut returned = Vec::with_capacity(rows.len());
        for (row, id) in rows.into_iter().zip(&unique_ids) {
            if row.get::<_, Vec<u8>>(0).as_slice() != id {
                return Err(corrupt());
            }
            let Some(bytes) = row.get::<_, Option<Vec<u8>>>(1) else {
                return Err(corrupt());
            };
            returned.push(Some(bytes));
        }
        let decoded = decode_objects(&unique_ids, returned, &mut self.object_read_stats)?;
        for ((_, expected), actual) in unique.iter().zip(decoded) {
            if actual.as_deref() != Some(*expected) {
                return Err(corrupt());
            }
        }
        tx.commit().map_err(commit_error)?;
        for bytes in encoded {
            record_write(bytes.len());
        }
        Ok(Guarded::Applied(ids))
    }
}
