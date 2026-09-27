//! Private filesystem repository for current canonical objects. Files become
//! visible only after fsync; points are published after their object closure.
//! This is not a live storage provider: it has no database mutation or CAS API.
use crate::{Digest, ErrorCategory, SemanticError};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const MAX_FILE: u64 = crate::storage::codec::MAX_PHYSICAL_BLOCK_BYTES as u64;
const CLAIM_MAGIC: &[u8; 6] = b"EDBB\0\x01";

pub(super) fn fault(code: &'static str, message: impl Into<String>) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, code, message)
}
pub(super) fn io(error: std::io::Error) -> SemanticError {
    SemanticError::new(ErrorCategory::Unavailable, "backup/io", error.to_string())
}
pub(super) fn anchor(path: &Path) -> Result<PathBuf, SemanticError> {
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        Ok(std::env::current_dir().map_err(io)?.join(path))
    }
}

fn regular(path: &Path) -> Result<(), SemanticError> {
    let meta = fs::symlink_metadata(path).map_err(io)?;
    if !meta.file_type().is_file() {
        return Err(fault(
            "backup/file-type",
            "Repository entries must be regular files",
        ));
    }
    Ok(())
}
fn private_directory(path: &Path) -> Result<(), SemanticError> {
    let meta = fs::symlink_metadata(path).map_err(io)?;
    if !meta.file_type().is_dir() {
        return Err(fault(
            "backup/directory-type",
            "Repository must be a real directory, not a symlink",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o022 != 0 {
            return Err(fault(
                "backup/directory-permissions",
                "Repository directories must not be group/world writable",
            ));
        }
    }
    Ok(())
}
fn create_private(path: &Path) -> Result<(), SemanticError> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(path) {
        Ok(()) => {
            if let Some(parent) = path.parent() {
                File::open(parent).and_then(|f| f.sync_all()).map_err(io)?;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(io(error)),
    }
    private_directory(path)
}
pub(super) fn admit(directory: &Path, create: bool) -> Result<(), SemanticError> {
    if create {
        // Deliberately do not create arbitrary ancestor directories. The caller
        // chooses an existing parent and one private repository destination.
        create_private(directory)?;
        create_private(&directory.join("objects"))?;
        create_private(&directory.join("points"))?;
    } else {
        private_directory(directory)?;
        private_directory(&directory.join("objects"))?;
        private_directory(&directory.join("points"))?;
    }
    Ok(())
}

pub(super) fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, SemanticError> {
    regular(path)?;
    let file = File::open(path).map_err(io)?;
    let length = file.metadata().map_err(io)?.len();
    if length > limit || length > usize::MAX as u64 {
        return Err(fault(
            "backup/file-size",
            "Repository file exceeds its current format limit",
        ));
    }
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(length as usize).map_err(|_| {
        fault(
            "backup/file-allocation",
            "Cannot allocate admitted repository object",
        )
    })?;
    // Bound reads even if a file grows after metadata admission.
    file.take(length + 1).read_to_end(&mut bytes).map_err(io)?;
    if bytes.len() as u64 != length {
        return Err(fault(
            "backup/file-size",
            "Repository file changed length while being read",
        ));
    }
    Ok(bytes)
}

/// Return true only for a newly published immutable file. Existing content is
/// authenticated by its caller, never overwritten. Tempfile cleanup is scoped
/// to the one private file this operation created.
fn publish_file(path: &Path, bytes: &[u8]) -> Result<bool, SemanticError> {
    let parent = path
        .parent()
        .ok_or_else(|| fault("backup/file-path", "Missing repository parent"))?;
    private_directory(parent)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".edb-stage-")
        .tempfile_in(parent)
        .map_err(io)?;
    temporary.write_all(bytes).map_err(io)?;
    temporary.as_file().sync_all().map_err(io)?;
    match temporary.persist_noclobber(path) {
        Ok(file) => {
            file.sync_all().map_err(io)?;
            File::open(parent).and_then(|f| f.sync_all()).map_err(io)?;
            Ok(true)
        }
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            regular(path)?;
            Ok(false)
        }
        Err(error) => Err(io(error.error)),
    }
}

pub(super) fn hex(id: Digest) -> String {
    let mut result = String::with_capacity(64);
    use std::fmt::Write as _;
    for byte in id {
        write!(&mut result, "{byte:02x}").expect("String writes cannot fail");
    }
    result
}
pub(super) fn object_path(directory: &Path, id: Digest) -> PathBuf {
    directory.join("objects").join(hex(id))
}
pub(super) fn contains(directory: &Path, id: Digest) -> Result<bool, SemanticError> {
    match fs::symlink_metadata(object_path(directory, id)) {
        Ok(meta) if meta.file_type().is_file() => Ok(true),
        Ok(_) => Err(fault(
            "backup/file-type",
            "Object entry is not a regular file",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(io(error)),
    }
}
pub(super) fn read_object(directory: &Path, id: Digest) -> Result<Vec<u8>, SemanticError> {
    private_directory(directory)?;
    private_directory(&directory.join("objects"))?;
    let bytes = read_bounded(&object_path(directory, id), MAX_FILE)?;
    crate::storage::codec::decode_block(&id, &bytes)
        .map_err(|error| fault("backup/object-corrupt", error.message).detail("object_id", hex(id)))
}
pub(super) fn put_object(directory: &Path, bytes: &[u8]) -> Result<(Digest, bool), SemanticError> {
    if bytes.len() > crate::storage::root::MAX_BLOCK_BYTES {
        return Err(fault(
            "backup/object-size",
            "Canonical object exceeds the current storage ceiling",
        ));
    }
    let id = crate::sha256(bytes);
    if contains(directory, id)? {
        if read_object(directory, id)? != bytes {
            return Err(fault(
                "backup/object-conflict",
                "Existing object differs from its canonical bytes",
            ));
        }
        return Ok((id, false));
    }
    let encoded = crate::storage::codec::encode_block(bytes)?;
    let written = publish_file(&object_path(directory, id), &encoded)?;
    if !written && read_object(directory, id)? != bytes {
        return Err(fault(
            "backup/object-conflict",
            "Concurrent immutable object publication differs",
        ));
    }
    Ok((id, written))
}

fn claim_bytes(lineage: [u8; 16]) -> Vec<u8> {
    let mut bytes = CLAIM_MAGIC.to_vec();
    bytes.extend_from_slice(&lineage);
    bytes.extend_from_slice(&crate::sha256(&bytes));
    bytes
}
pub(super) fn claim(directory: &Path, lineage: [u8; 16]) -> Result<(), SemanticError> {
    let bytes = claim_bytes(lineage);
    if !publish_file(&directory.join("claim"), &bytes)? {
        verify_claim(directory, lineage)?;
    }
    Ok(())
}
pub(super) fn verify_claim(directory: &Path, lineage: [u8; 16]) -> Result<(), SemanticError> {
    if read_bounded(&directory.join("claim"), 54)? != claim_bytes(lineage) {
        return Err(SemanticError::conflict(
            "backup/claim-failed",
            "Repository belongs to another lineage or its identity claim is corrupt",
        ));
    }
    Ok(())
}
pub(super) fn point_path(directory: &Path, generation: u64, basis: u64) -> PathBuf {
    directory
        .join("points")
        .join(format!("{generation:016x}-{basis:016x}.root"))
}
pub(super) fn publish_point(
    directory: &Path,
    generation: u64,
    basis: u64,
    bytes: &[u8],
) -> Result<bool, SemanticError> {
    publish_file(&point_path(directory, generation, basis), bytes)
}
