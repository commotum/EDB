//! Repository admission and point coordinates are separate from common integrity policy.
use super::{BackupVerification, ReadPoint};
use crate::storage::{ObjectId, ObjectReader};
use crate::{MaintenanceControl, SemanticError};
use std::path::Path;

pub(crate) fn walk_repository(
    directory: &Path,
    roots: &[ObjectId],
    control: &MaintenanceControl,
    deep: bool,
) -> Result<usize, SemanticError> {
    let mut source = |id| super::read_object(directory, id);
    crate::operations::integrity::walk_objects(&mut source, roots, control, deep)
}

pub(crate) fn verify_read_point(
    directory: &Path,
    point: ReadPoint,
    deep: bool,
    control: &MaintenanceControl,
) -> Result<BackupVerification, SemanticError> {
    let mut source = |id| super::read_object(directory, id);
    let path = directory.to_path_buf();
    let mut factory = || {
        let path = path.clone();
        Ok(Box::new(move |id| super::read_object(&path, id)) as Box<dyn ObjectReader + Send>)
    };
    let verified = crate::operations::integrity::verify_publication(
        point.publication,
        &[point.point.manifest_hash],
        &mut source,
        &mut factory,
        deep,
        control,
    )?;
    let publication = &verified.publication;
    if point.value.identity != publication.identity
        || point.value.basis != publication.basis
        || point.value.log != publication.log
        || point.value.metadata != publication.metadata
        || verified.generation != point.point.log_generation
    {
        return Err(super::repository::fault(
            "backup/read-coordinate",
            "Read value differs from its committed publication",
        ));
    }
    Ok(BackupVerification {
        point: point.point,
        database: verified.database,
        objects_read: verified.objects_read,
    })
}
