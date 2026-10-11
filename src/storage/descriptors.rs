//! Compact Rust-owned read descriptors. Tree objects retain the current raw
//! canonical tree encoding; their hashes are direct outgoing object links.
use super::{ObjectId, root::Block};
use crate::index::tree::TreeDescriptor;
use crate::{ErrorCategory, IndexOrder, SemanticError};

pub const INDEX_DESCRIPTOR_KIND: u16 = 13;
pub const SNAPSHOT_METADATA_KIND: u16 = 4;
const INDEX_FIXED_BYTES: usize = 32 + 12 * 72 + 4;
const AVET_WORK_BYTES: usize = 89;
// No independent schema-width ceiling: the shared encoded-object bound is
// the admission limit for this compact list of attribute IDs.
const MAX_PENDING: usize = (super::root::MAX_BLOCK_BYTES - 20 - 12 * 32 - INDEX_FIXED_BYTES) / 4;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexDescriptor {
    pub identity: [u8; 16],
    pub basis: u64,
    pub generation: u64,
    /// Current EAVT, AEVT, AVET, VAET, SAEVT, SAVET, then six history trees.
    pub trees: Vec<TreeDescriptor>,
    /// Strictly increasing attributes whose shared AVET/SAVET backfill is pending.
    pub pending_avet: Vec<u32>,
    /// Bounded structural checkpoints over immutable AEVT sources. Source
    /// roots are outgoing object links, not merely hashes hidden in payloads.
    pub avet_work: Vec<BlockAvetWork>,
    /// Optional search attachment, bound to this descriptor with this field
    /// cleared. The attachment links that source plus its immutable pages.
    pub fulltext: Option<ObjectId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockAvetWork {
    pub attribute: u32,
    pub adding: bool,
    pub history: bool,
    pub clearing: bool,
    pub directory: u32,
    pub leaf: u32,
    pub slot: u32,
    pub source: TreeDescriptor,
}

impl IndexDescriptor {
    pub fn encode(&self) -> Result<Vec<u8>, SemanticError> {
        self.validate()?;
        let total = 20usize
            .saturating_add((12 + self.avet_work.len() + usize::from(self.fulltext.is_some())) * 32)
            .saturating_add(INDEX_FIXED_BYTES)
            .saturating_add(self.pending_avet.len() * 4)
            .saturating_add(5)
            .saturating_add(self.avet_work.len() * AVET_WORK_BYTES);
        if total > super::root::MAX_BLOCK_BYTES {
            return Err(invalid("Index descriptor exceeds its bounded format"));
        }
        let mut payload = Vec::with_capacity(INDEX_FIXED_BYTES + self.pending_avet.len() * 4);
        payload.extend_from_slice(&self.identity);
        payload.extend_from_slice(&self.basis.to_be_bytes());
        payload.extend_from_slice(&self.generation.to_be_bytes());
        for tree in &self.trees {
            payload.extend_from_slice(&tree.count.to_be_bytes());
            payload.extend_from_slice(&tree.first_hash.unwrap_or([0; 32]));
            payload.extend_from_slice(&tree.last_hash.unwrap_or([0; 32]));
        }
        payload.extend_from_slice(&(self.pending_avet.len() as u32).to_be_bytes());
        for attribute in &self.pending_avet {
            payload.extend_from_slice(&attribute.to_be_bytes());
        }
        payload.extend_from_slice(&(self.avet_work.len() as u32).to_be_bytes());
        let mut links = self.trees.iter().map(|t| t.root_hash).collect::<Vec<_>>();
        for work in &self.avet_work {
            payload.extend_from_slice(&work.attribute.to_be_bytes());
            payload.push(
                u8::from(work.adding)
                    | (u8::from(work.history) << 1)
                    | (u8::from(work.clearing) << 2),
            );
            for coordinate in [work.directory, work.leaf, work.slot] {
                payload.extend_from_slice(&coordinate.to_be_bytes());
            }
            payload.extend_from_slice(&work.source.count.to_be_bytes());
            payload.extend_from_slice(&work.source.first_hash.unwrap_or([0; 32]));
            payload.extend_from_slice(&work.source.last_hash.unwrap_or([0; 32]));
            links.push(work.source.root_hash);
        }
        payload.push(u8::from(self.fulltext.is_some()));
        links.extend(self.fulltext);
        Block {
            kind: INDEX_DESCRIPTOR_KIND,
            links,
            payload,
        }
        .encode()
    }

    pub fn decode(expected: &ObjectId, bytes: &[u8]) -> Result<Self, SemanticError> {
        // Bound before Block clones the payload.
        if bytes.len() > super::root::MAX_BLOCK_BYTES {
            return Err(invalid("Index descriptor exceeds its bounded format"));
        }
        let block = Block::decode(expected, bytes)?;
        if block.kind != INDEX_DESCRIPTOR_KIND
            || block.links.len() < 12
            || block.payload.len() < INDEX_FIXED_BYTES + 5
        {
            return Err(invalid("Expected a twelve-tree index descriptor"));
        }
        let p = &block.payload;
        let pending = u32::from_be_bytes(p[896..900].try_into().unwrap()) as usize;
        let work_start = INDEX_FIXED_BYTES
            .checked_add(
                pending
                    .checked_mul(4)
                    .ok_or_else(|| invalid("Pending length overflow"))?,
            )
            .ok_or_else(|| invalid("Pending length overflow"))?;
        if pending > MAX_PENDING || p.len() < work_start + 5 {
            return Err(invalid(
                "Index descriptor pending-attribute length is invalid",
            ));
        }
        let work_count =
            u32::from_be_bytes(p[work_start..work_start + 4].try_into().unwrap()) as usize;
        let final_offset = work_start
            .checked_add(4)
            .and_then(|n| {
                work_count
                    .checked_mul(AVET_WORK_BYTES)
                    .and_then(|m| n.checked_add(m))
            })
            .ok_or_else(|| invalid("AVET work length overflow"))?;
        if work_count > MAX_PENDING
            || p.len() != final_offset + 1
            || p[final_offset] > 1
            || block.links.len() != 12 + work_count + usize::from(p[final_offset])
        {
            return Err(invalid("AVET work or attachment framing is invalid"));
        }
        let mut trees = Vec::with_capacity(12);
        for (i, root_hash) in block.links.iter().copied().take(12).enumerate() {
            let offset = 32 + i * 72;
            let count = u64::from_be_bytes(p[offset..offset + 8].try_into().unwrap());
            let first: ObjectId = p[offset + 8..offset + 40].try_into().unwrap();
            let last: ObjectId = p[offset + 40..offset + 72].try_into().unwrap();
            if count == 0 && (first != [0; 32] || last != [0; 32]) {
                return Err(invalid("Empty tree has endpoint commitments"));
            }
            trees.push(TreeDescriptor {
                root_hash,
                order: order(i % 6),
                history: i >= 6,
                count,
                first_hash: (count != 0).then_some(first),
                last_hash: (count != 0).then_some(last),
            });
        }
        let mut avet_work = Vec::with_capacity(work_count);
        for i in 0..work_count {
            let at = work_start + 4 + i * AVET_WORK_BYTES;
            let flags = p[at + 4];
            if flags > 7 {
                return Err(invalid("Invalid AVET work flags"));
            }
            let history = flags & 2 != 0;
            let count = u64::from_be_bytes(p[at + 17..at + 25].try_into().unwrap());
            let first: ObjectId = p[at + 25..at + 57].try_into().unwrap();
            let last: ObjectId = p[at + 57..at + 89].try_into().unwrap();
            if count == 0 && (first != [0; 32] || last != [0; 32]) {
                return Err(invalid("Empty AVET source has endpoints"));
            }
            avet_work.push(BlockAvetWork {
                attribute: u32::from_be_bytes(p[at..at + 4].try_into().unwrap()),
                adding: flags & 1 != 0,
                history,
                clearing: flags & 4 != 0,
                directory: u32::from_be_bytes(p[at + 5..at + 9].try_into().unwrap()),
                leaf: u32::from_be_bytes(p[at + 9..at + 13].try_into().unwrap()),
                slot: u32::from_be_bytes(p[at + 13..at + 17].try_into().unwrap()),
                source: TreeDescriptor {
                    root_hash: block.links[12 + i],
                    order: IndexOrder::Aevt,
                    history,
                    count,
                    first_hash: (count != 0).then_some(first),
                    last_hash: (count != 0).then_some(last),
                },
            });
        }
        let result = Self {
            identity: p[..16].try_into().unwrap(),
            basis: u64::from_be_bytes(p[16..24].try_into().unwrap()),
            generation: u64::from_be_bytes(p[24..32].try_into().unwrap()),
            trees,
            pending_avet: p[900..work_start]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|v| u32::from_be_bytes(*v))
                .collect(),
            avet_work,
            fulltext: (p[final_offset] != 0).then(|| *block.links.last().unwrap()),
        };
        result.validate()?;
        Ok(result)
    }

    fn validate(&self) -> Result<(), SemanticError> {
        crate::t_to_tx(self.basis)?;
        if self.identity == [0; 16]
            || self.trees.len() != 12
            || self.pending_avet.len() > MAX_PENDING
            || self.pending_avet.windows(2).any(|p| p[0] >= p[1])
            || self.avet_work.len() > MAX_PENDING
            || self
                .avet_work
                .windows(2)
                .any(|p| p[0].attribute >= p[1].attribute)
            || self.avet_work.iter().any(|w| {
                self.pending_avet.binary_search(&w.attribute).is_ok() != w.adding
                    || w.source.order != IndexOrder::Aevt
                    || w.source.history != w.history
                    || (w.source.count == 0) != w.source.first_hash.is_none()
                    || (w.source.count == 0) != w.source.last_hash.is_none()
                    || (!w.adding && !w.clearing)
                    || (w.clearing && (w.directory != 0 || w.leaf != 0 || w.slot != 0))
            })
            || self.trees.iter().enumerate().any(|(i, t)| {
                t.order != order(i % 6)
                    || t.history != (i >= 6)
                    || (t.count == 0) != t.first_hash.is_none()
                    || (t.count == 0) != t.last_hash.is_none()
            })
        {
            return Err(invalid(
                "Index identity, tree order, endpoints, or pending attributes are invalid",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotMetadata {
    pub identity: [u8; 16],
    pub basis: u64,
    pub generation: u64,
    pub eidx_frontier: u64,
    pub reserved_frontier: u64,
    pub last_tx_instant: Option<i64>,
    /// Immutable completion evidence for this physical excision generation.
    pub excision: Option<ObjectId>,
}
impl SnapshotMetadata {
    pub fn encode(&self) -> Result<Vec<u8>, SemanticError> {
        self.validate()?;
        let mut payload = Vec::with_capacity(57);
        payload.extend_from_slice(&self.identity);
        for value in [
            self.basis,
            self.generation,
            self.eidx_frontier,
            self.reserved_frontier,
        ] {
            payload.extend_from_slice(&value.to_be_bytes());
        }
        payload.push(u8::from(self.last_tx_instant.is_some()));
        payload.extend_from_slice(&self.last_tx_instant.unwrap_or(0).to_be_bytes());
        Block {
            kind: SNAPSHOT_METADATA_KIND,
            links: self.excision.into_iter().collect(),
            payload,
        }
        .encode()
    }
    pub fn decode(expected: &ObjectId, bytes: &[u8]) -> Result<Self, SemanticError> {
        if bytes.len() != 20 + 57 && bytes.len() != 20 + 57 + 32 {
            return Err(invalid("Snapshot metadata has an invalid size"));
        }
        let block = Block::decode(expected, bytes)?;
        if block.kind != SNAPSHOT_METADATA_KIND
            || block.links.len() > 1
            || block.payload.len() != 57
        {
            return Err(invalid("Expected snapshot metadata"));
        }
        let p = &block.payload;
        let instant = i64::from_be_bytes(p[49..57].try_into().unwrap());
        if p[48] > 1 || (p[48] == 0 && instant != 0) {
            return Err(invalid("Invalid snapshot instant marker"));
        }
        let result = Self {
            identity: p[..16].try_into().unwrap(),
            basis: u64::from_be_bytes(p[16..24].try_into().unwrap()),
            generation: u64::from_be_bytes(p[24..32].try_into().unwrap()),
            eidx_frontier: u64::from_be_bytes(p[32..40].try_into().unwrap()),
            reserved_frontier: u64::from_be_bytes(p[40..48].try_into().unwrap()),
            last_tx_instant: (p[48] != 0).then_some(instant),
            excision: block.links.first().copied(),
        };
        result.validate()?;
        Ok(result)
    }
    fn validate(&self) -> Result<(), SemanticError> {
        crate::t_to_tx(self.basis)?;
        crate::reserved_allocation::ReservedAllocation::from_frontier(
            self.reserved_frontier,
            self.eidx_frontier,
        )?;
        if self.identity == [0; 16] || (self.basis == 0) != self.last_tx_instant.is_none() {
            return Err(invalid("Invalid snapshot identity or genesis instant"));
        }
        Ok(())
    }
}

pub(crate) fn order_tag(order: IndexOrder) -> u8 {
    match order {
        IndexOrder::Eavt => 0,
        IndexOrder::Aevt => 1,
        IndexOrder::Avet => 2,
        IndexOrder::Vaet => 3,
        IndexOrder::Saevt => 4,
        IndexOrder::Savet => 5,
    }
}
fn order(i: usize) -> IndexOrder {
    [
        IndexOrder::Eavt,
        IndexOrder::Aevt,
        IndexOrder::Avet,
        IndexOrder::Vaet,
        IndexOrder::Saevt,
        IndexOrder::Savet,
    ][i]
}
fn invalid(message: &str) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, "storage/read-descriptor", message)
}
