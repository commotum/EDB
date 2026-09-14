//! Public projection format, build/read policy and observations.
use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FulltextRecord {
    pub key: Vec<u8>,
    pub value: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FulltextBuildLimits {
    pub sort_memory_bytes: usize,
    pub page_bytes: usize,
    pub max_record_bytes: usize,
    pub max_key_bytes: usize,
    pub max_records: u64,
    /// Cumulative spill writes, including merge passes (not a disk/RSS claim).
    pub max_spill_bytes: u64,
    pub work_directory: PathBuf,
}
impl Default for FulltextBuildLimits {
    fn default() -> Self {
        Self {
            sort_memory_bytes: 8 * 1024 * 1024,
            page_bytes: 64 * 1024,
            max_record_bytes: MAX_RECORD,
            max_key_bytes: MAX_KEY,
            max_records: 10_000_000,
            max_spill_bytes: 4 * 1024 * 1024 * 1024,
            work_directory: std::env::temp_dir(),
        }
    }
}
impl FulltextBuildLimits {
    pub(crate) fn validate(&self) -> Result<(), SemanticError> {
        if self.sort_memory_bytes == 0
            || !(1024..=1024 * 1024).contains(&self.page_bytes)
            || self.max_record_bytes == 0
            || self.max_record_bytes > MAX_RECORD
            || self.max_key_bytes == 0
            || self.max_key_bytes > MAX_KEY
            || self.max_records == 0
            || self.max_spill_bytes == 0
        {
            return Err(incorrect(
                "fulltext/build-limits",
                "invalid fulltext build limits",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FulltextBuildStats {
    /// Input records or mutations admitted by this operation, before dedup.
    pub input_records: u64,
    /// Records reachable from the resulting projection root.
    pub records: u64,
    /// Complete page uploads, including deduplication and intermediate paths.
    pub blocks: u64,
    /// Encoded page bytes uploaded, including deduplication/intermediate paths.
    pub encoded_bytes: u64,
    pub spill_bytes: u64,
    pub peak_buffer_bytes: usize,
    /// Page payload reads, including upload verification reads.
    pub blocks_read: u64,
    pub block_bytes_read: u64,
    pub source_nodes_read: u64,
    pub source_node_bytes: u64,
    pub source_datoms_examined: u64,
    pub source_references_examined: u64,
    pub documents_added: u64,
    pub documents_removed: u64,
    pub tokenized_bytes: u64,
    pub statistics_reads: u64,
    pub statistics_read_bytes: u64,
    pub reused_projections: u64,
    /// Initial bulk construction after proving the predecessor contains only
    /// zero-valued attribute statistics and finding new canonical text.
    pub empty_corpus_bulk_builds: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FulltextProjection {
    pub source_manifest: Digest,
    pub source_basis_t: u64,
    pub source_generation: u64,
    pub analyzer_version: u32,
    pub root_hash: Digest,
    pub record_count: u64,
    pub encoded_bytes: u64,
    pub block_count: u64,
}
impl FulltextProjection {
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(HEADER_MAGIC);
        bytes.extend_from_slice(&FORMAT.to_be_bytes());
        bytes.extend_from_slice(&self.analyzer_version.to_be_bytes());
        bytes.extend_from_slice(&self.source_manifest);
        for n in [self.source_basis_t, self.source_generation] {
            bytes.extend_from_slice(&n.to_be_bytes());
        }
        bytes.extend_from_slice(&self.root_hash);
        for n in [self.record_count, self.encoded_bytes, self.block_count] {
            bytes.extend_from_slice(&n.to_be_bytes());
        }
        bytes
    }
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, SemanticError> {
        let mut d = Decoder::new(bytes);
        if d.take(4)? != HEADER_MAGIC || d.u32()? != FORMAT {
            return Err(fault("fulltext/header-format", "unsupported search header"));
        }
        let analyzer_version = d.u32()?;
        let source_manifest = d.digest()?;
        let source_basis_t = d.u64()?;
        let source_generation = d.u64()?;
        let root_hash = d.digest()?;
        let record_count = d.u64()?;
        let encoded_bytes = d.u64()?;
        let block_count = d.u64()?;
        d.finish()?;
        if analyzer_version == 0 || block_count == 0 || encoded_bytes == 0 {
            return Err(fault(
                "fulltext/header-counts",
                "invalid search header counts",
            ));
        }
        Ok(Self {
            source_manifest,
            source_basis_t,
            source_generation,
            analyzer_version,
            root_hash,
            record_count,
            encoded_bytes,
            block_count,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FulltextReadLimits {
    pub max_records: u64,
    pub max_block_bytes: u64,
}
impl Default for FulltextReadLimits {
    fn default() -> Self {
        Self {
            max_records: 100_000,
            max_block_bytes: 64 * 1024 * 1024,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FulltextReadStats {
    pub cache_hits: u64,
    pub blocks_read: u64,
    pub block_bytes: u64,
    /// Decoded page allocation bytes visited, including cache hits. One bounded
    /// in-flight page may exceed the remaining allowance before rejection.
    pub visited_bytes: u64,
    pub records_examined: u64,
    pub records_yielded: u64,
}
