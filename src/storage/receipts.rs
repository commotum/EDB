//! Exact immutable outcomes and a path-copy request index.
//!
//! The index is a compressed binary radix trie of scoped request-key digests.
//! Lookup and insertion visit at most 256 branches, regardless of history size;
//! ordinary hashed keys have logarithmic paths. Neither operation reads the
//! complete mapping. Edits accept objects through ObjectWriter; the caller must
//! flush before independent reads/publication. Protection remains caller-owned.

use super::object_io::{ObjectReader, ObjectWriter};
use super::{ObjectId, root::Block};
use crate::{Digest, ErrorCategory, SemanticError};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;

pub const RECEIPT_KIND: u16 = 20;
pub const RECEIPT_CHUNK_KIND: u16 = 21;
pub const REQUEST_LEAF_KIND: u16 = 22;
pub const REQUEST_BRANCH_KIND: u16 = 23;
/// Preserves the current canonical transaction body's admitted tempid payload.
pub const MAX_RECEIPT_TEMPID_BYTES: usize = 64 * 1024 * 1024;
const MAX_TEMPIDS: usize = 1_000_000;
const MAX_TEMPID_NAME_BYTES: usize = 16 * 1024 * 1024;
const INLINE_BYTES: usize = 64 * 1024;
const CHUNK_BYTES: usize = 4 * 1024 * 1024;
const RECEIPT_HEADER: usize = 64;

/// Secondary address in the same immutable outcome trie. Durable observers
/// can find exact outcomes by log position without scanning caller request keys.
pub(crate) fn basis_receipt_key(identity: &[u8; 16], basis: u64) -> Digest {
    let mut hash = Sha256::new();
    hash.update(b"atomic/receipt-by-basis/1\0");
    hash.update(identity);
    hash.update(basis.to_be_bytes());
    hash.finalize().into()
}

/// Scope a caller's request key to one immutable database identity. Hash the
/// original key, not an EDN rendering or a mutable database alias.
pub fn scoped_request_key(identity: &[u8; 16], key: &str) -> Result<Digest, SemanticError> {
    if *identity == [0; 16] || key.is_empty() {
        return Err(invalid(
            "storage/request-key",
            "Request identity/key is empty",
        ));
    }
    let mut hash = Sha256::new();
    hash.update(b"atomic/request-key/1\0");
    hash.update(identity);
    hash.update((key.len() as u64).to_be_bytes());
    hash.update(key.as_bytes());
    Ok(hash.finalize().into())
}

/// Caller-visible outcome, independent of current writer/index authority.
/// Before/after are DatabaseValueRoot objects, not publication roots containing
/// this receipt index (which would introduce a content-addressed cycle).
/// The consumer also validates their identity/basis and the log entry when
/// constructing database values; decoding this descriptor does not read them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExactReceipt {
    pub identity: [u8; 16],
    pub request_digest: Digest,
    pub basis: u64,
    pub transaction: ObjectId,
    pub before: ObjectId,
    pub after: ObjectId,
    pub tempids: BTreeMap<String, u64>,
}

impl ExactReceipt {
    pub fn put(&self, store: &mut dyn ObjectWriter) -> Result<ObjectId, SemanticError> {
        validate_coordinate(self.identity, self.basis)?;
        let tempids = encode_tempids(&self.tempids, self.basis)?;
        let mut payload = Vec::with_capacity(RECEIPT_HEADER + tempids.len().min(INLINE_BYTES));
        payload.extend_from_slice(&self.identity);
        payload.extend_from_slice(&self.request_digest);
        payload.extend_from_slice(&self.basis.to_be_bytes());
        payload.extend_from_slice(&(tempids.len() as u64).to_be_bytes());
        let mut links = vec![self.before, self.after, self.transaction];
        if tempids.len() <= INLINE_BYTES {
            payload.extend_from_slice(&tempids);
        } else {
            for chunk in tempids.chunks(CHUNK_BYTES) {
                links.push(
                    store.put_object(
                        &Block {
                            kind: RECEIPT_CHUNK_KIND,
                            links: vec![],
                            payload: chunk.to_vec(),
                        }
                        .encode()?,
                    )?,
                );
            }
        }
        store.put_object(
            &Block {
                kind: RECEIPT_KIND,
                links,
                payload,
            }
            .encode()?,
        )
    }

    pub fn load(
        store: &mut (impl ObjectReader + ?Sized),
        id: ObjectId,
    ) -> Result<Self, SemanticError> {
        let bytes = required(store, id)?;
        Self::load_from_bytes(store, id, &bytes)
    }

    /// Reuse an already fetched request outcome descriptor after inspecting
    /// its kind (for example an excision occupancy decision). Authentication
    /// and bounded chunk decoding are identical to `load`.
    pub(crate) fn load_from_bytes(
        store: &mut (impl ObjectReader + ?Sized),
        id: ObjectId,
        bytes: &[u8],
    ) -> Result<Self, SemanticError> {
        Self::load_from_bytes_bounded(store, id, bytes, MAX_RECEIPT_TEMPID_BYTES)
    }

    pub(crate) fn load_from_bytes_bounded(
        store: &mut (impl ObjectReader + ?Sized),
        id: ObjectId,
        bytes: &[u8],
        max_tempid_bytes: usize,
    ) -> Result<Self, SemanticError> {
        // Check before the generic decoder clones an arbitrary payload.
        if bytes.len() > 20 + 3 * 32 + RECEIPT_HEADER + INLINE_BYTES {
            return Err(invalid(
                "storage/receipt-size",
                "Receipt descriptor is oversized",
            ));
        }
        let block = Block::decode(&id, bytes)?;
        if block.kind != RECEIPT_KIND
            || block.links.len() < 3
            || block.payload.len() < RECEIPT_HEADER
        {
            return Err(invalid(
                "storage/receipt-format",
                "Expected an exact receipt descriptor",
            ));
        }
        let p = &block.payload;
        let identity = p[..16].try_into().unwrap();
        let basis = u64::from_be_bytes(p[48..56].try_into().unwrap());
        validate_coordinate(identity, basis)?;
        let length = usize::try_from(u64::from_be_bytes(p[56..64].try_into().unwrap()))
            .map_err(|_| invalid("storage/receipt-size", "Tempid payload is not addressable"))?;
        if !(4..=MAX_RECEIPT_TEMPID_BYTES).contains(&length) {
            return Err(invalid(
                "storage/receipt-size",
                "Tempid payload exceeds its format bound",
            ));
        }
        if length > max_tempid_bytes {
            return Err(SemanticError::new(
                ErrorCategory::Busy,
                "storage/receipt-read-limit",
                "Receipt tempids exceed the admitted encoded byte budget",
            ));
        }
        let tempids = if length <= INLINE_BYTES {
            if block.links.len() != 3 || p.len() != RECEIPT_HEADER + length {
                return Err(invalid(
                    "storage/receipt-format",
                    "Inline tempid lengths/links disagree",
                ));
            }
            decode_tempids(&p[RECEIPT_HEADER..], basis)?
        } else {
            if p.len() != RECEIPT_HEADER || block.links.len() != 3 + length.div_ceil(CHUNK_BYTES) {
                return Err(invalid(
                    "storage/receipt-format",
                    "Chunked tempid lengths/links disagree",
                ));
            }
            let mut content = Vec::with_capacity(length);
            for id in &block.links[3..] {
                let bytes = required(store, *id)?;
                if bytes.len() > 20 + CHUNK_BYTES {
                    return Err(invalid(
                        "storage/receipt-chunk",
                        "Receipt chunk is oversized",
                    ));
                }
                let chunk = Block::decode(id, &bytes)?;
                if chunk.kind != RECEIPT_CHUNK_KIND
                    || !chunk.links.is_empty()
                    || chunk.payload.len() != (length - content.len()).min(CHUNK_BYTES)
                {
                    return Err(invalid(
                        "storage/receipt-chunk",
                        "Receipt chunk kind/length/links disagree",
                    ));
                }
                content.extend_from_slice(&chunk.payload);
            }
            decode_tempids(&content, basis)?
        };
        Ok(Self {
            identity,
            request_digest: p[16..48].try_into().unwrap(),
            basis,
            before: block.links[0],
            after: block.links[1],
            transaction: block.links[2],
            tempids,
        })
    }
}

fn validate_coordinate(identity: [u8; 16], basis: u64) -> Result<(), SemanticError> {
    if identity == [0; 16] || basis == 0 || basis > crate::MAX_EIDX {
        return Err(invalid(
            "storage/receipt-coordinate",
            "Receipt identity/basis is invalid",
        ));
    }
    Ok(())
}

/// Caller aliases can refer to existing IDs as well as freshly allocated IDs.
/// Their upper bound is partition-specific: transaction time is not an entity
/// allocation frontier, and reserved IDs do not inherit the ordinary frontier.
pub(crate) fn validate_receipt_frontiers(
    tempids: &BTreeMap<String, u64>,
    basis: u64,
    ordinary: u64,
    reserved: u64,
) -> Result<(), SemanticError> {
    crate::reserved_allocation::ReservedAllocation::from_frontier(reserved, ordinary)?;
    for entity in tempids.values() {
        validate_tempid_entity(*entity, basis)?;
        let index = crate::eid_to_eidx(*entity)?;
        let outside = match crate::eid_to_part(*entity)? {
            crate::TX_PARTITION => false, // validated against transaction basis above
            crate::DB_PARTITION => index >= reserved,
            _ => index >= ordinary,
        };
        if outside {
            return Err(invalid(
                "storage/receipt-tempids",
                "Receipt alias exceeds its partition's authenticated allocation frontier",
            ));
        }
    }
    Ok(())
}

fn validate_tempid_entity(entity: u64, basis: u64) -> Result<(), SemanticError> {
    let partition = crate::eid_to_part(entity).map_err(|_| {
        invalid(
            "storage/receipt-tempids",
            "Tempid has an invalid permanent entity",
        )
    })?;
    if partition == crate::TX_PARTITION {
        let t = crate::tx_to_t(entity)?;
        if t == 0 || t > basis {
            return Err(invalid(
                "storage/receipt-tempids",
                "Tempid names a future transaction",
            ));
        }
    }
    Ok(())
}

fn encode_tempids(tempids: &BTreeMap<String, u64>, basis: u64) -> Result<Vec<u8>, SemanticError> {
    if tempids.len() > MAX_TEMPIDS {
        return Err(invalid(
            "storage/receipt-tempids",
            "Too many tempid mappings",
        ));
    }
    let mut size = 4usize;
    for (name, entity) in tempids {
        if name.len() > MAX_TEMPID_NAME_BYTES
            || name.len() + 12 > MAX_RECEIPT_TEMPID_BYTES.saturating_sub(size)
        {
            return Err(invalid(
                "storage/receipt-size",
                "Tempid payload exceeds its format bound",
            ));
        }
        validate_tempid_entity(*entity, basis)?;
        size += name.len() + 12;
    }
    let mut bytes = Vec::with_capacity(size);
    bytes.extend_from_slice(&(tempids.len() as u32).to_be_bytes());
    for (name, entity) in tempids {
        bytes.extend_from_slice(&(name.len() as u32).to_be_bytes());
        bytes.extend_from_slice(name.as_bytes());
        bytes.extend_from_slice(&entity.to_be_bytes());
    }
    Ok(bytes)
}

fn decode_tempids(bytes: &[u8], basis: u64) -> Result<BTreeMap<String, u64>, SemanticError> {
    let bad = || invalid("storage/receipt-tempids", "Tempid payload is not canonical");
    if bytes.len() < 4 {
        return Err(bad());
    }
    let count = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
    if count > MAX_TEMPIDS || count > (bytes.len() - 4) / 12 {
        return Err(bad());
    }
    let mut offset = 4usize;
    let mut result = BTreeMap::<String, u64>::new();
    for _ in 0..count {
        let end = offset
            .checked_add(4)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(bad)?;
        let length = u32::from_be_bytes(bytes[offset..end].try_into().unwrap()) as usize;
        offset = end;
        let end = offset
            .checked_add(length)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(bad)?;
        if length > MAX_TEMPID_NAME_BYTES {
            return Err(bad());
        }
        let name = std::str::from_utf8(&bytes[offset..end]).map_err(|_| bad())?;
        if result
            .last_key_value()
            .is_some_and(|(previous, _)| previous.as_str() >= name)
        {
            return Err(bad());
        }
        offset = end;
        let end = offset
            .checked_add(8)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(bad)?;
        let entity = u64::from_be_bytes(bytes[offset..end].try_into().unwrap());
        validate_tempid_entity(entity, basis)?;
        result.insert(name.to_owned(), entity);
        offset = end;
    }
    if offset != bytes.len() {
        return Err(bad());
    }
    Ok(result)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RequestIndex {
    root: Option<ObjectId>,
}

impl RequestIndex {
    pub fn empty() -> Self {
        Self::default()
    }
    /// The pointer must come from an authenticated publication root. Each
    /// selected trie path is validated lazily; no root-wide prefetch occurs.
    pub fn from_root(root: Option<ObjectId>) -> Self {
        Self { root }
    }
    pub fn root(&self) -> Option<ObjectId> {
        self.root
    }

    pub fn lookup(
        &self,
        store: &mut (impl ObjectReader + ?Sized),
        key: Digest,
    ) -> Result<Option<ObjectId>, SemanticError> {
        let Some(mut id) = self.root else {
            return Ok(None);
        };
        let mut parent = None;
        loop {
            let node = Node::load(store, id, parent)?;
            match node {
                Node::Leaf {
                    key: found,
                    receipt,
                } => return Ok((key == found).then_some(receipt)),
                Node::Branch(branch) => {
                    if common_bits(&key, &branch.prefix) < branch.bit {
                        return Ok(None);
                    }
                    let slot = key_bit(&key, branch.bit);
                    id = branch.children[slot];
                    parent = Some((branch.prefix, branch.bit, slot));
                }
            }
        }
    }

    /// Insert only. A repeated identical mapping is a no-op; another outcome
    /// for an occupied key is a conflict and performs no object writes.
    pub fn insert(
        &self,
        store: &mut dyn ObjectWriter,
        key: Digest,
        receipt: ObjectId,
    ) -> Result<Self, SemanticError> {
        self.insert_inner(store, key, receipt, false)
    }

    /// Replace one mapping by path copying. The old root remains immutable.
    /// Administrative callers can retain occupancy with a privacy-safe
    /// tombstone object; ordinary receipt insertion remains insert-only.
    pub fn upsert(
        &self,
        store: &mut dyn ObjectWriter,
        key: Digest,
        value: ObjectId,
    ) -> Result<Self, SemanticError> {
        self.insert_inner(store, key, value, true)
    }

    /// Apply a sorted, unique page of replacements/deletions. Only the final
    /// affected Patricia nodes are submitted, once each: intermediate roots
    /// exist solely in memory and untouched subtrees keep their object IDs.
    pub fn apply_batch(
        &self,
        store: &mut dyn ObjectWriter,
        changes: &[(Digest, Option<ObjectId>)],
    ) -> Result<Self, SemanticError> {
        if changes.len() > 4096 || changes.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
            return Err(SemanticError::incorrect(
                "storage/request-index-batch",
                "Request-index batches require at most 4096 sorted unique keys",
            ));
        }
        let mut tree = self.root.map(|id| BatchNode::stored(id, None));
        for &(key, value) in changes {
            tree = BatchNode::edit(tree, store, key, value)?.0;
        }
        Ok(Self {
            root: tree.map(|tree| tree.persist(store)).transpose()?,
        })
    }

    /// Remove one mapping by path copying, collapsing its binary parent. No
    /// value or unrelated subtree is read or changed; the previous root keeps
    /// its original meaning for any retained owner.
    pub fn remove(&self, store: &mut dyn ObjectWriter, key: Digest) -> Result<Self, SemanticError> {
        let Some(mut id) = self.root else {
            return Ok(*self);
        };
        let mut parent = None;
        let mut path = Vec::new();
        loop {
            match Node::load(store, id, parent)? {
                Node::Leaf { key: found, .. } if found != key => return Ok(*self),
                Node::Leaf { .. } => break,
                Node::Branch(branch) => {
                    if common_bits(&key, &branch.prefix) < branch.bit {
                        return Ok(*self);
                    }
                    let slot = key_bit(&key, branch.bit);
                    id = branch.children[slot];
                    parent = Some((branch.prefix, branch.bit, slot));
                    path.push((branch, slot));
                }
            }
        }
        let Some((parent, removed_slot)) = path.pop() else {
            return Ok(Self::empty());
        };
        id = parent.children[1 - removed_slot];
        for (mut branch, slot) in path.into_iter().rev() {
            branch.children[slot] = id;
            id = Node::Branch(branch).put(store)?;
        }
        Ok(Self { root: Some(id) })
    }

    /// A bounded lexicographic page with an exclusive digest continuation.
    /// Skips subtrees ending at/before `after` without fetching their objects;
    /// work is bounded by trie depth plus the returned page, not total entries.
    pub fn scan(
        &self,
        store: &mut (impl ObjectReader + ?Sized),
        after: Option<Digest>,
        limit: usize,
    ) -> Result<Vec<(Digest, ObjectId)>, SemanticError> {
        if !(1..=4096).contains(&limit) {
            return Err(SemanticError::incorrect(
                "storage/request-index-limit",
                "Request index page limit must be in 1..=4096",
            ));
        }
        let mut pending = Vec::with_capacity(257);
        if let Some(root) = self.root {
            pending.push((root, None));
        }
        let mut result = Vec::with_capacity(limit);
        while let Some((id, parent)) = pending.pop() {
            match Node::load(store, id, parent)? {
                Node::Leaf { key, receipt } => {
                    if after.is_none_or(|after| key > after) {
                        result.push((key, receipt));
                        if result.len() == limit {
                            break;
                        }
                    }
                }
                Node::Branch(branch) => {
                    for slot in [1, 0] {
                        if after.is_some_and(|after| {
                            subtree_last(branch.prefix, branch.bit, slot) <= after
                        }) {
                            continue;
                        }
                        pending.push((
                            branch.children[slot],
                            Some((branch.prefix, branch.bit, slot)),
                        ));
                    }
                }
            }
        }
        Ok(result)
    }

    fn insert_inner(
        &self,
        store: &mut dyn ObjectWriter,
        key: Digest,
        receipt: ObjectId,
        replace: bool,
    ) -> Result<Self, SemanticError> {
        let Some(mut id) = self.root else {
            return Ok(Self {
                root: Some(Node::Leaf { key, receipt }.put(store)?),
            });
        };
        let mut parent = None;
        let mut path = Vec::new();
        loop {
            let node = Node::load(store, id, parent)?;
            let split = common_bits(&key, &node.anchor());
            match node {
                Node::Leaf {
                    receipt: existing, ..
                } if split == 256 => {
                    if existing == receipt {
                        return Ok(*self);
                    }
                    if !replace {
                        return Err(SemanticError::conflict(
                            "storage/request-key-exists",
                            "Request key already has an immutable outcome",
                        ));
                    }
                    id = Node::Leaf { key, receipt }.put(store)?;
                    break;
                }
                Node::Branch(branch) if split >= branch.bit => {
                    let slot = key_bit(&key, branch.bit);
                    id = branch.children[slot];
                    parent = Some((branch.prefix, branch.bit, slot));
                    path.push((branch, slot));
                    continue;
                }
                _ => {}
            }
            let new_leaf = Node::Leaf { key, receipt }.put(store)?;
            let slot = key_bit(&key, split);
            let mut children = [id; 2];
            children[slot] = new_leaf;
            id = Node::Branch(Branch {
                prefix: prefix(key, split),
                bit: split,
                children,
            })
            .put(store)?;
            break;
        }
        for (mut branch, slot) in path.into_iter().rev() {
            branch.children[slot] = id;
            id = Node::Branch(branch).put(store)?;
        }
        Ok(Self { root: Some(id) })
    }
}

/// A bounded edit page materializes only selected paths. A loaded but unchanged
/// node retains `original`, allowing final persistence to skip it altogether.
struct BatchNode {
    original: Option<ObjectId>,
    body: BatchBody,
}
enum BatchBody {
    Stored(Option<(Digest, u16, usize)>),
    Leaf {
        key: Digest,
        value: ObjectId,
    },
    Branch {
        prefix: Digest,
        bit: u16,
        children: [Box<BatchNode>; 2],
    },
}
impl BatchNode {
    fn stored(id: ObjectId, parent: Option<(Digest, u16, usize)>) -> Box<Self> {
        Box::new(Self {
            original: Some(id),
            body: BatchBody::Stored(parent),
        })
    }
    fn leaf(key: Digest, value: ObjectId) -> Box<Self> {
        Box::new(Self {
            original: None,
            body: BatchBody::Leaf { key, value },
        })
    }
    fn load(&mut self, store: &mut dyn ObjectReader) -> Result<(), SemanticError> {
        if let BatchBody::Stored(parent) = self.body {
            self.body =
                match Node::load(store, self.original.expect("stored node identity"), parent)? {
                    Node::Leaf { key, receipt } => BatchBody::Leaf {
                        key,
                        value: receipt,
                    },
                    Node::Branch(branch) => BatchBody::Branch {
                        prefix: branch.prefix,
                        bit: branch.bit,
                        children: [0, 1].map(|slot| {
                            Self::stored(
                                branch.children[slot],
                                Some((branch.prefix, branch.bit, slot)),
                            )
                        }),
                    },
                };
        }
        Ok(())
    }
    /// Recursion is bounded by the 256-bit key width, not the entry count.
    fn edit(
        tree: Option<Box<Self>>,
        store: &mut dyn ObjectReader,
        key: Digest,
        value: Option<ObjectId>,
    ) -> Result<(Option<Box<Self>>, bool), SemanticError> {
        let Some(mut tree) = tree else {
            return Ok((value.map(|value| Self::leaf(key, value)), value.is_some()));
        };
        tree.load(store)?;
        let (anchor, branch_bit) = match &tree.body {
            BatchBody::Leaf { key, .. } => (*key, None),
            BatchBody::Branch { prefix, bit, .. } => (*prefix, Some(*bit)),
            BatchBody::Stored(_) => unreachable!("node loaded"),
        };
        let split = common_bits(&key, &anchor);
        if branch_bit.is_some_and(|bit| split >= bit) {
            let BatchBody::Branch {
                prefix,
                bit,
                children,
            } = tree.body
            else {
                unreachable!()
            };
            let slot = key_bit(&key, bit);
            let [left, right] = children;
            let (selected, sibling) = if slot == 0 {
                (left, right)
            } else {
                (right, left)
            };
            let (child, changed) = Self::edit(Some(selected), store, key, value)?;
            let Some(child) = child else {
                return Ok((Some(sibling), true));
            };
            tree.body = BatchBody::Branch {
                prefix,
                bit,
                children: if slot == 0 {
                    [child, sibling]
                } else {
                    [sibling, child]
                },
            };
            if changed {
                tree.original = None;
            }
            return Ok((Some(tree), changed));
        }
        if branch_bit.is_none() && split == 256 {
            let Some(value) = value else {
                return Ok((None, true));
            };
            let BatchBody::Leaf { value: current, .. } = &mut tree.body else {
                unreachable!()
            };
            let changed = *current != value;
            if changed {
                *current = value;
                tree.original = None;
            }
            return Ok((Some(tree), changed));
        }
        let Some(value) = value else {
            return Ok((Some(tree), false));
        };
        let new = Self::leaf(key, value);
        let children = if key_bit(&key, split) == 0 {
            [new, tree]
        } else {
            [tree, new]
        };
        Ok((
            Some(Box::new(Self {
                original: None,
                body: BatchBody::Branch {
                    prefix: prefix(key, split),
                    bit: split,
                    children,
                },
            })),
            true,
        ))
    }
    fn persist(self, store: &mut dyn ObjectWriter) -> Result<ObjectId, SemanticError> {
        if let Some(id) = self.original {
            return Ok(id);
        }
        match self.body {
            BatchBody::Leaf { key, value } => Node::Leaf {
                key,
                receipt: value,
            }
            .put(store),
            BatchBody::Branch {
                prefix,
                bit,
                children: [left, right],
            } => {
                let children = [left.persist(store)?, right.persist(store)?];
                Node::Branch(Branch {
                    prefix,
                    bit,
                    children,
                })
                .put(store)
            }
            BatchBody::Stored(_) => unreachable!("stored node has original identity"),
        }
    }
}

#[derive(Clone, Copy)]
struct Branch {
    prefix: Digest,
    bit: u16,
    children: [ObjectId; 2],
}
enum Node {
    Leaf { key: Digest, receipt: ObjectId },
    Branch(Branch),
}
impl Node {
    fn anchor(&self) -> Digest {
        match self {
            Self::Leaf { key, .. } => *key,
            Self::Branch(b) => b.prefix,
        }
    }
    fn put(self, store: &mut dyn ObjectWriter) -> Result<ObjectId, SemanticError> {
        let block = match self {
            Self::Leaf { key, receipt } => Block {
                kind: REQUEST_LEAF_KIND,
                links: vec![receipt],
                payload: key.to_vec(),
            },
            Self::Branch(b) => {
                let mut payload = b.prefix.to_vec();
                payload.extend_from_slice(&b.bit.to_be_bytes());
                Block {
                    kind: REQUEST_BRANCH_KIND,
                    links: b.children.to_vec(),
                    payload,
                }
            }
        };
        store.put_object(&block.encode()?)
    }
    fn load(
        store: &mut (impl ObjectReader + ?Sized),
        id: ObjectId,
        parent: Option<(Digest, u16, usize)>,
    ) -> Result<Self, SemanticError> {
        let bad = || {
            invalid(
                "storage/request-index",
                "Request trie node/coordinate is invalid",
            )
        };
        let bytes = required(store, id)?;
        if bytes.len() > 118 {
            return Err(bad());
        }
        let block = Block::decode(&id, &bytes)?;
        let node = match (block.kind, block.payload.len(), block.links.len()) {
            (REQUEST_LEAF_KIND, 32, 1) => Self::Leaf {
                key: block.payload[..].try_into().unwrap(),
                receipt: block.links[0],
            },
            (REQUEST_BRANCH_KIND, 34, 2) => {
                let anchor = block.payload[..32].try_into().unwrap();
                let bit = u16::from_be_bytes(block.payload[32..34].try_into().unwrap());
                if bit >= 256 || prefix(anchor, bit) != anchor || block.links[0] == block.links[1] {
                    return Err(bad());
                }
                Self::Branch(Branch {
                    prefix: anchor,
                    bit,
                    children: [block.links[0], block.links[1]],
                })
            }
            _ => return Err(bad()),
        };
        if let Some((anchor, bit, slot)) = parent
            && (common_bits(&node.anchor(), &anchor) < bit
                || key_bit(&node.anchor(), bit) != slot
                || matches!(&node, Self::Branch(child) if child.bit <= bit))
        {
            return Err(bad());
        }
        Ok(node)
    }
}

fn key_bit(key: &Digest, bit: u16) -> usize {
    ((key[bit as usize / 8] >> (7 - bit % 8)) & 1) as usize
}
fn subtree_last(mut anchor: Digest, bit: u16, slot: usize) -> Digest {
    let byte = bit as usize / 8;
    let shift = 7 - bit % 8;
    anchor[byte] |= (slot as u8) << shift;
    anchor[byte] |= (1u8 << shift) - 1;
    anchor[byte + 1..].fill(0xff);
    anchor
}
fn common_bits(a: &Digest, b: &Digest) -> u16 {
    for (index, (a, b)) in a.iter().zip(b).enumerate() {
        let different = a ^ b;
        if different != 0 {
            return index as u16 * 8 + different.leading_zeros() as u16;
        }
    }
    256
}
fn prefix(mut key: Digest, bits: u16) -> Digest {
    let whole = bits as usize / 8;
    let partial = bits % 8;
    if partial != 0 {
        key[whole] &= 0xff << (8 - partial);
        key[whole + 1..].fill(0);
    } else {
        key[whole..].fill(0);
    }
    key
}
fn required(
    store: &mut (impl ObjectReader + ?Sized),
    id: ObjectId,
) -> Result<Vec<u8>, SemanticError> {
    store.read_object(id)
}
fn invalid(code: &'static str, message: impl Into<String>) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, code, message)
}
