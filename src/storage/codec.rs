//! Physical block representation, separate from canonical content identity.
//! PostgreSQL publication formats are not changed by this codec. A canonical
//! hash always authenticates the uncompressed bytes, never this envelope.
use crate::{Digest, ErrorCategory, SemanticError, sha256};
use flate2::{Compression, bufread::GzDecoder, write::GzEncoder};
use std::io::{Read, Write};

pub const MAX_CANONICAL_BLOCK_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_PHYSICAL_BLOCK_BYTES: usize = MAX_CANONICAL_BLOCK_BYTES;
pub const BLOCK_CODEC_VERSION: u8 = 1;
const MAGIC: &[u8; 8] = b"ATOMICBL";
const GZIP: u8 = 1;
const HEADER_BYTES: usize = 26;

/// Detect the physical-envelope marker, including unsupported versions that
/// the decoder must reject. This never recursively interprets decoded bytes.
pub fn is_encoded(bytes: &[u8]) -> bool {
    bytes.starts_with(MAGIC)
}

/// Retain raw bytes unless gzip, including its physical envelope, is
/// strictly smaller. This changes no canonical bytes, hashes, or tree grammar.
pub fn encode_block(canonical: &[u8]) -> Result<Vec<u8>, SemanticError> {
    if canonical.len() > MAX_CANONICAL_BLOCK_BYTES {
        return Err(invalid(
            "block/size-limit",
            "canonical block exceeds its size limit",
        ));
    }
    if canonical.len() <= HEADER_BYTES {
        return Ok(canonical.to_vec());
    }
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder
        .write_all(canonical)
        .map_err(|_| invalid("block/compression", "block compression failed"))?;
    let compressed = encoder
        .finish()
        .map_err(|_| invalid("block/compression", "block compression failed"))?;
    if compressed.len().saturating_add(HEADER_BYTES) >= canonical.len() {
        return Ok(canonical.to_vec());
    }
    let mut physical = Vec::with_capacity(HEADER_BYTES + compressed.len());
    physical.extend_from_slice(MAGIC);
    physical.extend_from_slice(&[BLOCK_CODEC_VERSION, GZIP]);
    physical.extend_from_slice(&(canonical.len() as u64).to_be_bytes());
    physical.extend_from_slice(&(compressed.len() as u64).to_be_bytes());
    physical.extend_from_slice(&compressed);
    Ok(physical)
}

/// Authenticate raw bytes or one versioned gzip member. The advertised
/// lengths bound allocation and output independently; gzip CRC, exact output,
/// complete input consumption, and canonical SHA-256 must all agree. Native
/// callers must still apply their canonical tree/log decoder to the result.
pub fn decode_block(expected_hash: &Digest, physical: &[u8]) -> Result<Vec<u8>, SemanticError> {
    inspect_block(expected_hash, physical)?.decode()
}

/// Borrowed preflight proof. Multi-object readers sum `canonical_len` before
/// allocating any decoded output. The borrow binds admission to the exact
/// physical bytes inspected, including legal raw content starting with MAGIC.
pub(crate) struct BlockDecoder<'a> {
    expected_hash: &'a Digest,
    physical: &'a [u8],
    canonical_len: usize,
    compressed: bool,
}
impl BlockDecoder<'_> {
    pub(crate) fn canonical_len(&self) -> usize {
        self.canonical_len
    }
    pub(crate) fn is_compressed(&self) -> bool {
        self.compressed
    }
    pub(crate) fn decode(self) -> Result<Vec<u8>, SemanticError> {
        if !self.compressed {
            return Ok(self.physical.to_vec());
        }
        decode_gzip(self.expected_hash, self.physical, self.canonical_len)
    }
}

pub(crate) fn inspect_block<'a>(
    expected_hash: &'a Digest,
    physical: &'a [u8],
) -> Result<BlockDecoder<'a>, SemanticError> {
    if physical.len() > MAX_PHYSICAL_BLOCK_BYTES {
        return Err(invalid(
            "block/size-limit",
            "physical block exceeds its size limit",
        ));
    }
    // Authenticate raw input first, preserving every canonical byte string
    // even if its prefix happens to resemble a physical-envelope marker.
    if &sha256(physical) == expected_hash {
        return Ok(BlockDecoder {
            expected_hash,
            physical,
            canonical_len: physical.len(),
            compressed: false,
        });
    }
    if !is_encoded(physical) {
        return Err(invalid(
            "block/hash-mismatch",
            "canonical block hash does not match",
        ));
    }
    if physical.len() < HEADER_BYTES {
        return Err(invalid(
            "block/truncated-header",
            "physical block header is incomplete",
        ));
    }
    if physical[8] != BLOCK_CODEC_VERSION || physical[9] != GZIP {
        return Err(invalid(
            "block/unsupported-codec",
            "physical block codec or version is unsupported",
        ));
    }
    let canonical_len = u64::from_be_bytes(physical[10..18].try_into().unwrap());
    let encoded_len = u64::from_be_bytes(physical[18..26].try_into().unwrap());
    if canonical_len > MAX_CANONICAL_BLOCK_BYTES as u64 {
        return Err(invalid(
            "block/size-limit",
            "declared canonical block exceeds its size limit",
        ));
    }
    if encoded_len != (physical.len() - HEADER_BYTES) as u64 {
        return Err(invalid(
            "block/physical-length",
            "physical block length does not match its envelope",
        ));
    }
    Ok(BlockDecoder {
        expected_hash,
        physical,
        canonical_len: canonical_len as usize,
        compressed: true,
    })
}

fn decode_gzip(
    expected_hash: &Digest,
    physical: &[u8],
    canonical_len: usize,
) -> Result<Vec<u8>, SemanticError> {
    let mut canonical = Vec::new();
    canonical.try_reserve_exact(canonical_len).map_err(|_| {
        invalid(
            "block/allocation",
            "bounded canonical block allocation failed",
        )
    })?;
    canonical.resize(canonical_len, 0);
    let mut decoder = GzDecoder::new(&physical[HEADER_BYTES..]);
    decoder.read_exact(&mut canonical).map_err(|_| {
        invalid(
            "block/decompression",
            "compressed block is corrupt or shorter than declared",
        )
    })?;
    let mut extra = [0u8; 1];
    if decoder.read(&mut extra).map_err(|_| {
        invalid(
            "block/decompression",
            "compressed block integrity check failed",
        )
    })? != 0
    {
        return Err(invalid(
            "block/canonical-length",
            "compressed block expands beyond its declared length",
        ));
    }
    if !decoder.into_inner().is_empty() {
        return Err(invalid(
            "block/trailing-data",
            "compressed block contains trailing data or another member",
        ));
    }
    if &sha256(&canonical) != expected_hash {
        return Err(invalid(
            "block/hash-mismatch",
            "decoded canonical block hash does not match",
        ));
    }
    Ok(canonical)
}

fn invalid(code: &'static str, message: &'static str) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, code, message)
}
