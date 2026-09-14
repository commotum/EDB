//! Immutable canonical object I/O shared by live storage and portable repositories.
use super::{ObjectId, PgBlockStore};
use crate::{ErrorCategory, SemanticError};

mod staging;
pub(crate) use staging::with_staged_transaction;

/// Read one required immutable object. Implementations authenticate its identity
/// and apply input admission before returning canonical bytes; missing objects
/// are errors, never an empty payload. No reference or publication authority is
/// implied by implementing this interface.
pub trait ObjectReader {
    fn read_object(&mut self, id: ObjectId) -> Result<Vec<u8>, SemanticError>;
}

/// Accept canonical immutable bytes and return their content identity. Accepted
/// bytes must be available through this writer's ObjectReader (read-your-writes).
/// Acceptance may be buffered: only successful `flush_objects` establishes that
/// all accepted bytes are persisted and existing content has been authenticated.
/// Mutable references, protection policy and publication remain separate. Callers
/// must flush before publishing IDs or handing them to an independent reader.
pub trait ObjectWriter: ObjectReader {
    fn put_object(&mut self, bytes: &[u8]) -> Result<ObjectId, SemanticError>;

    /// A bounded ordered batch, including duplicate inputs. Providers may
    /// coalesce I/O. Success accepts every input; persistence/authentication is
    /// guaranteed only after `flush_objects`. Failure may leave unreferenced
    /// objects; publication remains separate.
    fn put_objects(&mut self, objects: &[&[u8]]) -> Result<Vec<ObjectId>, SemanticError> {
        validate_write_batch(objects)?;
        objects.iter().map(|bytes| self.put_object(bytes)).collect()
    }

    /// Persist/authenticate every accepted object under the writer's unchanged
    /// protection context, if any. Immediate writers explicitly return success
    /// with no pending work. A buffering writer must retain failure: catching a
    /// failed flush cannot turn this attempt into a later successful barrier.
    fn flush_objects(&mut self) -> Result<(), SemanticError>;
}

pub(crate) const MAX_WRITE_BATCH_OBJECTS: usize = 128;
pub(crate) fn validate_write_batch(objects: &[&[u8]]) -> Result<(), SemanticError> {
    let bytes = objects
        .iter()
        .try_fold(0usize, |total, bytes| total.checked_add(bytes.len()));
    if objects.len() > MAX_WRITE_BATCH_OBJECTS
        || bytes.is_none_or(|bytes| bytes > super::root::MAX_BLOCK_BYTES)
    {
        return Err(SemanticError::incorrect(
            "storage/write-batch-limit",
            "Object batch exceeds 128 objects or 64 MiB of canonical bytes",
        ));
    }
    Ok(())
}

impl ObjectReader for PgBlockStore {
    fn read_object(&mut self, id: ObjectId) -> Result<Vec<u8>, SemanticError> {
        self.get(id)?.ok_or_else(|| {
            SemanticError::new(
                ErrorCategory::Fault,
                "storage/missing-object",
                "Required immutable object is absent",
            )
        })
    }
}
impl ObjectWriter for PgBlockStore {
    fn put_object(&mut self, bytes: &[u8]) -> Result<ObjectId, SemanticError> {
        self.put(bytes)
    }
    fn put_objects(&mut self, objects: &[&[u8]]) -> Result<Vec<ObjectId>, SemanticError> {
        self.put_many(objects)
    }
    fn flush_objects(&mut self) -> Result<(), SemanticError> {
        // put/put_many commit and authenticate before returning.
        Ok(())
    }
}

impl<F> ObjectReader for F
where
    F: FnMut(ObjectId) -> Result<Vec<u8>, SemanticError>,
{
    fn read_object(&mut self, id: ObjectId) -> Result<Vec<u8>, SemanticError> {
        self(id)
    }
}
