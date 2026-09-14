//! Captured block-backed search reads share their immutable value's retention
//! and bounded positive cache, including filesystem repository values.
use crate::storage::BlockSnapshot;
use crate::{
    DatabaseValue, FulltextCursor, FulltextProjection, FulltextReadLimits, FulltextReadStats,
    FulltextRecord, SemanticError,
};

#[derive(Clone)]
pub struct NativeFulltextReader {
    snapshot: BlockSnapshot,
    projection: FulltextProjection,
}

impl NativeFulltextReader {
    pub(crate) fn from_block(snapshot: BlockSnapshot, projection: FulltextProjection) -> Self {
        Self {
            snapshot,
            projection,
        }
    }
    pub fn cache_stats(&self) -> crate::FulltextCacheStats {
        self.snapshot.fulltext_cache().stats()
    }
    pub fn source_basis_t(&self) -> u64 {
        self.projection.source_basis_t
    }
    pub fn source_generation(&self) -> u64 {
        self.projection.source_generation
    }
    pub fn record_count(&self) -> u64 {
        self.projection.record_count
    }
    pub fn projection(&self) -> &FulltextProjection {
        &self.projection
    }
    pub fn prefix(
        &self,
        prefix: &[u8],
        limits: FulltextReadLimits,
    ) -> Result<FulltextCursor, SemanticError> {
        crate::storage::fulltext::prefix(self.snapshot.clone(), &self.projection, prefix, limits)
    }
    pub fn get(&self, key: &[u8]) -> Result<Option<FulltextRecord>, SemanticError> {
        self.get_with_stats(key).map(|(record, _)| record)
    }
    pub fn get_with_stats(
        &self,
        key: &[u8],
    ) -> Result<(Option<FulltextRecord>, FulltextReadStats), SemanticError> {
        let mut cursor = self.prefix(
            key,
            FulltextReadLimits {
                max_records: 1,
                max_block_bytes: 128 * 1024 * 1024,
            },
        )?;
        let record = cursor
            .next()
            .transpose()?
            .filter(|record| record.key == key);
        Ok((record, cursor.stats()))
    }
}

impl DatabaseValue {
    /// Eager values return None; persisted values with a missing projection
    /// return an explicit error, never an implicit whole-database scan.
    pub fn native_fulltext_reader(&self) -> Result<Option<NativeFulltextReader>, SemanticError> {
        self.block_snapshot()
            .map(|snapshot| crate::storage::fulltext::reader(&snapshot))
            .transpose()
    }
}
