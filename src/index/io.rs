//! Shared diagnostics for canonical and projected index-node reads.

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NodeBlockReadStats {
    pub compressed_hits: u64,
    pub canonical_reads: u64,
    pub corrupt_projections: u64,
    pub canonical_bytes: u64,
    /// Returned physical payload bytes, excluding metadata and wire framing.
    pub physical_read_bytes: u64,
    pub decode_elapsed_nanos: u64,
}
