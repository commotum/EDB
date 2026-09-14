//! One transaction's bounded uploads. Only the closure's successful final
//! barrier may return prepared fields; this module never publishes references.
use super::{MAX_WRITE_BATCH_OBJECTS, ObjectReader, ObjectWriter, validate_write_batch};
use crate::storage::{ObjectId, PgBlockStore, WriteProtection};
use crate::{ErrorCategory, SemanticError, sha256};
use std::collections::BTreeMap;

// A target, not a new object-size admission limit. A larger admitted object is
// buffered alone, up to MAX_BLOCK_BYTES. This accounts canonical payloads, not
// process RSS, map overhead, encoder temporaries or provider/driver copies.
const TARGET_PENDING_BYTES: usize = 4 * 1024 * 1024;

pub(crate) fn with_staged_transaction<T>(
    store: &mut PgBlockStore,
    protection: &WriteProtection,
    build: impl FnOnce(&mut dyn ObjectWriter) -> Result<T, SemanticError>,
) -> Result<T, SemanticError> {
    if store.write_protection() != Some(protection) {
        return Err(fault(
            "storage/staging-protection",
            "Transaction staging requires its captured write protection",
        ));
    }
    // The exclusive object-only borrow prevents the builder from changing the
    // installed context. Every provider batch uses these same guards and epoch.
    with_buffered_objects(store, MAX_WRITE_BATCH_OBJECTS, TARGET_PENDING_BYTES, build)
}

fn with_buffered_objects<T>(
    store: &mut dyn ObjectWriter,
    max_objects: usize,
    target_bytes: usize,
    build: impl FnOnce(&mut dyn ObjectWriter) -> Result<T, SemanticError>,
) -> Result<T, SemanticError> {
    let mut writer = BufferedObjects {
        store,
        pending: BTreeMap::new(),
        pending_bytes: 0,
        max_objects,
        target_bytes,
        failed: None,
    };
    let prepared = build(&mut writer)?;
    writer.flush_objects()?;
    Ok(prepared)
}

// No Drop implementation: failure/unwind discards the pending group without I/O.
// Previous successful groups may be durable orphans, never a publication.
struct BufferedObjects<'a> {
    store: &'a mut dyn ObjectWriter,
    pending: BTreeMap<ObjectId, Vec<u8>>,
    pending_bytes: usize,
    max_objects: usize,
    target_bytes: usize,
    failed: Option<SemanticError>,
}

impl BufferedObjects<'_> {
    fn active(&self) -> Result<(), SemanticError> {
        self.failed.clone().map_or(Ok(()), Err)
    }

    fn flush_pending(&mut self) -> Result<(), SemanticError> {
        if !self.pending.is_empty() {
            let payloads = self.pending.values().map(Vec::as_slice).collect::<Vec<_>>();
            let actual = self.store.put_objects(&payloads)?;
            if actual.len() != self.pending.len() || !actual.iter().eq(self.pending.keys()) {
                return Err(fault(
                    "storage/staging-identity",
                    "Object batch returned different canonical identities",
                ));
            }
        }
        self.store.flush_objects()?;
        // Existing objects have been authenticated and protected too. Never
        // replace this barrier with cached reads or object-existence checks.
        self.pending.clear();
        self.pending_bytes = 0;
        Ok(())
    }
}

impl ObjectReader for BufferedObjects<'_> {
    fn read_object(&mut self, id: ObjectId) -> Result<Vec<u8>, SemanticError> {
        self.active()?;
        match self.pending.get(&id) {
            Some(bytes) => Ok(bytes.clone()),
            None => self.store.read_object(id),
        }
    }
}

impl ObjectWriter for BufferedObjects<'_> {
    fn put_object(&mut self, bytes: &[u8]) -> Result<ObjectId, SemanticError> {
        self.active()?;
        validate_write_batch(&[bytes])?;
        let id = sha256(bytes);
        if let Some(existing) = self.pending.get(&id) {
            if existing != bytes {
                return Err(fault(
                    "storage/staging-identity",
                    "Different canonical bytes have the same object identity",
                ));
            }
            return Ok(id);
        }
        if !self.pending.is_empty()
            && (self.pending.len() >= self.max_objects
                || bytes.len() > self.target_bytes.saturating_sub(self.pending_bytes))
        {
            self.flush_objects()?;
        }
        self.pending.insert(id, bytes.to_vec());
        self.pending_bytes += bytes.len();
        Ok(id)
    }

    fn flush_objects(&mut self) -> Result<(), SemanticError> {
        self.active()?;
        if let Err(error) = self.flush_pending() {
            self.failed = Some(error.clone());
            return Err(error);
        }
        Ok(())
    }
}

fn fault(code: &'static str, message: &str) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, code, message)
}
