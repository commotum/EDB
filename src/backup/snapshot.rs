//! Fixed portable values using the same block snapshot, log and index readers
//! as live peers. The repository contains current canonical objects, not an
//! alternate query engine or a restored PostgreSQL database.
use crate::backup::BackupPoint;
use crate::storage::{BlockReadConfig, BlockSnapshot};
use crate::{CacheStats, DatabaseValue, ErrorCategory, SemanticError};
use std::path::Path;

#[derive(Clone, Debug)]
pub struct BackupReadConfig {
    pub cache_entries: usize,
    pub cache_bytes: usize,
}
impl Default for BackupReadConfig {
    fn default() -> Self {
        let config = BlockReadConfig::default();
        Self {
            cache_entries: config.cache_entries,
            cache_bytes: config.cache_bytes,
        }
    }
}

/// Actual canonical filesystem payload work, including open-time metadata and
/// subsequent shared-cursor reads. It is not SQL or filesystem block traffic.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BackupReadStats {
    pub object_reads: u64,
    pub object_bytes: u64,
    pub root_reads: u64,
    pub directory_reads: u64,
    pub leaf_reads: u64,
    pub datoms_yielded: u64,
}

#[derive(Clone)]
pub struct BackupConnection {
    snapshot: BlockSnapshot,
    point: BackupPoint,
}
impl BackupConnection {
    pub fn open(directory: impl AsRef<Path>) -> Result<Self, SemanticError> {
        Self::open_configured(directory, None, BackupReadConfig::default())
    }
    pub fn open_point(
        directory: impl AsRef<Path>,
        point: &BackupPoint,
    ) -> Result<Self, SemanticError> {
        Self::open_configured(directory, Some(point), BackupReadConfig::default())
    }
    pub fn open_configured(
        directory: impl AsRef<Path>,
        point: Option<&BackupPoint>,
        config: BackupReadConfig,
    ) -> Result<Self, SemanticError> {
        // Anchor once without canonicalizing away symlinks that repository
        // admission must reject. Cold reads retain this meaning after cwd moves.
        let directory = directory.as_ref();
        let directory = if directory.is_absolute() {
            directory.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|_| {
                    SemanticError::new(
                        ErrorCategory::Unavailable,
                        "backup/working-directory",
                        "Could not anchor the relative backup repository",
                    )
                })?
                .join(directory)
        };
        let read = crate::backup::open_read_point(&directory, point)?;
        let snapshot = BlockSnapshot::open_repository(
            &directory,
            &read,
            BlockReadConfig {
                cache_entries: config.cache_entries,
                cache_bytes: config.cache_bytes,
                ..BlockReadConfig::default()
            },
        )?;
        Ok(Self {
            snapshot,
            point: read.point,
        })
    }
    pub fn db(&self) -> DatabaseValue {
        self.snapshot.database_value()
    }
    pub fn point(&self) -> &BackupPoint {
        &self.point
    }
    pub fn log(&self) -> crate::LogValue {
        self.snapshot.log()
    }
    pub fn read_stats(&self) -> BackupReadStats {
        let stats = self.snapshot.read_stats();
        BackupReadStats {
            object_reads: stats.object_reads,
            object_bytes: stats.object_bytes,
            root_reads: stats.tree.root_reads,
            directory_reads: stats.tree.directory_reads,
            leaf_reads: stats.tree.leaf_reads,
            datoms_yielded: stats.datoms_yielded,
        }
    }
    pub fn cache_stats(&self) -> CacheStats {
        self.snapshot.cache_stats()
    }
}
