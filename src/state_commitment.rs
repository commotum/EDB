//! Packing-independent commitment to one immutable database value.
//!
//! Transaction hashes authenticate the chronological log, but a materialized
//! index can copy the right terminal transaction hash while containing
//! different facts.  The v2 commitment below authenticates the semantic
//! current logical set without depending on physical index segments, fanout,
//! insertion order, or the timing of noHistory consolidation. The separately
//! bound transaction hash authenticates chronological log identity.
//!
//! The accumulator is a deterministic persistent Merkle treap. Keys are
//! `(attribute, canonical-datom-digest)`, which gives exact datom identity
//! under SHA-256 collision resistance. Priorities are separately
//! domain-separated hashes of keys, so the Cartesian tree is canonical for a
//! set rather than for its insertion history. Every node hash authenticates
//! its leaf, child roots and subtree count. This is not an algebraic/XOR
//! accumulator: removal rebuilds and re-authenticates the affected path.
//!
//! A semantic root copied into a physical-tree manifest remains only a claim.
//! Ordinary recovery authenticates and trusts the conditionally published
//! native indexer root, as it trusts the transactor/indexer boundary itself.
//! Explicit deep integrity inspection reconstructs the authoritative log value
//! and compares its exact current and retained-history information with the
//! physical tree. Manifest metadata alone cannot prove that cross-structure
//! equivalence against a privileged publisher capable of forging both values.

use crate::encoding::canonical_datom_hash;
use crate::{Database, Datom, Digest, ErrorCategory, IndexOrder, IndexPrefix, SemanticError, View};
use sha2::{Digest as _, Sha256};
use std::cmp::Ordering;
use std::sync::Arc;

const EMPTY_DOMAIN: &[u8] = b"edb/semantic-set/v2/empty\0";
const LEAF_DOMAIN: &[u8] = b"edb/semantic-set/v2/leaf\0";
const PRIORITY_DOMAIN: &[u8] = b"edb/semantic-set/v2/priority\0";
const NODE_DOMAIN: &[u8] = b"edb/semantic-set/v2/node\0";

pub(crate) const SEMANTIC_STATE_VERSION: u32 = 2;

/// Canonical identity of one committed datom. The attribute prefix provides a
/// stable range key for future authenticated physical-tree proofs; the digest
/// commits to every stored field, including transaction, assertion bit and
/// typed value bytes.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct SemanticDatomKey {
    pub(crate) attribute: u32,
    pub(crate) digest: Digest,
}

pub(crate) fn semantic_datom_key(datom: &Datom) -> Result<SemanticDatomKey, SemanticError> {
    Ok(SemanticDatomKey {
        attribute: datom.attribute,
        digest: canonical_datom_hash(datom)?,
    })
}

pub(crate) fn semantic_leaf_hash(key: SemanticDatomKey) -> Digest {
    hash_parts(LEAF_DOMAIN, &[&key.attribute.to_be_bytes(), &key.digest])
}

/// Packing-independent roots suitable for binding a physical index to an
/// already-authoritative database semantic state. They are not, by themselves,
/// evidence that an untrusted physical tree actually contains those sets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SemanticRootMetadata {
    pub(crate) version: u32,
    pub(crate) current_root: Digest,
    pub(crate) current_count: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct CommitmentWork {
    /// Search-tree nodes inspected by the incremental update.
    pub(crate) node_visits: u64,
    /// Merkle nodes rehashed (including new leaves).
    pub(crate) node_hashes: u64,
    /// Set members actually inserted or removed.
    pub(crate) leaf_changes: u64,
}

impl CommitmentWork {
    pub(crate) fn visit(&mut self) {
        self.node_visits = self.node_visits.saturating_add(1);
    }

    pub(crate) fn rehash(&mut self) {
        self.node_hashes = self.node_hashes.saturating_add(1);
    }

    pub(crate) fn change_leaf(&mut self) {
        self.leaf_changes = self.leaf_changes.saturating_add(1);
    }
}

type Link = Option<Arc<Node>>;

#[derive(Clone, Debug)]
struct Node {
    key: SemanticDatomKey,
    priority: Digest,
    left: Link,
    right: Link,
    count: u64,
    hash: Digest,
}

impl Node {
    fn build(
        key: SemanticDatomKey,
        left: Link,
        right: Link,
        work: &mut CommitmentWork,
    ) -> Result<Arc<Self>, SemanticError> {
        let count = link_count(&left)
            .checked_add(1)
            .and_then(|count| count.checked_add(link_count(&right)))
            .ok_or_else(|| {
                SemanticError::new(
                    ErrorCategory::Fault,
                    "commitment/member-count-overflow",
                    "semantic commitment cannot contain more than u64 datoms",
                )
            })?;
        let hash = semantic_node_hash(key, count, link_hash(&left), link_hash(&right));
        work.rehash();
        Ok(Arc::new(Self {
            key,
            priority: semantic_key_priority(key),
            left,
            right,
            count,
            hash,
        }))
    }
}

#[derive(Clone, Debug, Default)]
struct AuthenticatedSet {
    root: Link,
}

impl AuthenticatedSet {
    fn from_datoms<'a>(datoms: impl IntoIterator<Item = &'a Datom>) -> Result<Self, SemanticError> {
        let mut set = Self::default();
        let mut ignored = CommitmentWork::default();
        for datom in datoms {
            set.insert(datom, &mut ignored)?;
        }
        Ok(set)
    }

    fn insert(&mut self, datom: &Datom, work: &mut CommitmentWork) -> Result<(), SemanticError> {
        let key = semantic_datom_key(datom)?;
        let (root, changed) = insert(self.root.clone(), key, work)?;
        if changed {
            work.change_leaf();
            self.root = root;
        }
        Ok(())
    }

    fn remove(&mut self, datom: &Datom, work: &mut CommitmentWork) -> Result<(), SemanticError> {
        let key = semantic_datom_key(datom)?;
        let (root, changed) = remove(self.root.clone(), key, work)?;
        if changed {
            work.change_leaf();
            self.root = root;
        }
        Ok(())
    }

    fn count(&self) -> u64 {
        link_count(&self.root)
    }

    fn root_hash(&self) -> Digest {
        link_hash(&self.root)
    }
}

/// Incrementally maintained roots carried by every immutable Database value.
#[derive(Clone, Debug, Default)]
pub(crate) struct SemanticStateCommitment {
    current: AuthenticatedSet,
}

impl SemanticStateCommitment {
    pub(crate) fn recompute(database: &Database) -> Result<Self, SemanticError> {
        Self::from_current(&database.datoms(View::Current, IndexOrder::Eavt))
    }

    pub(crate) fn from_current(current: &[Datom]) -> Result<Self, SemanticError> {
        Ok(Self {
            current: AuthenticatedSet::from_datoms(current)?,
        })
    }

    pub(crate) fn advance(
        &self,
        before: &Database,
        tx_data: &[Datom],
    ) -> Result<(Self, CommitmentWork), SemanticError> {
        let mut next = self.clone();
        let mut work = CommitmentWork::default();

        for datom in tx_data {
            apply_current_change(&mut next.current, before, datom, &mut work)?;
        }
        Ok((next, work))
    }

    pub(crate) fn metadata(&self) -> SemanticRootMetadata {
        SemanticRootMetadata {
            version: SEMANTIC_STATE_VERSION,
            current_root: self.current.root_hash(),
            current_count: self.current.count(),
        }
    }
}

fn apply_current_change(
    set: &mut AuthenticatedSet,
    before: &Database,
    datom: &Datom,
    work: &mut CommitmentWork,
) -> Result<(), SemanticError> {
    let prior = exact_current_datom(before, datom)?;
    if datom.added {
        match prior {
            Some(prior) if u64::from(datom.attribute) == crate::DB_ALTER_ATTRIBUTE => {
                set.remove(&prior, work)?;
                set.insert(datom, work)?;
            }
            None => {
                set.insert(datom, work)?;
            }
            Some(_) => {}
        }
    } else if let Some(prior) = prior {
        set.remove(&prior, work)?;
    }
    Ok(())
}

fn exact_current_datom(
    database: &Database,
    candidate: &Datom,
) -> Result<Option<Datom>, SemanticError> {
    Ok(database
        .datoms_with_prefix(&IndexPrefix::Eavt {
            entity: candidate.entity,
            attribute: Some(candidate.attribute),
            value: Some(candidate.value.clone()),
        })?
        .iter()
        .find(|datom| datom.value.stored_eq(&candidate.value))
        .cloned())
}

fn insert(
    root: Link,
    key: SemanticDatomKey,
    work: &mut CommitmentWork,
) -> Result<(Link, bool), SemanticError> {
    work.visit();
    let Some(node) = root else {
        return Ok((Some(Node::build(key, None, None, work)?), true));
    };
    match key.cmp(&node.key) {
        Ordering::Equal => Ok((Some(node), false)),
        Ordering::Less => {
            let (left, changed) = insert(node.left.clone(), key, work)?;
            if !changed {
                return Ok((Some(node), false));
            }
            let rebuilt = Node::build(node.key, left, node.right.clone(), work)?;
            if heap_precedes(
                rebuilt.left.as_ref().expect("inserted left child"),
                &rebuilt,
            ) {
                Ok((Some(rotate_right(rebuilt, work)?), true))
            } else {
                Ok((Some(rebuilt), true))
            }
        }
        Ordering::Greater => {
            let (right, changed) = insert(node.right.clone(), key, work)?;
            if !changed {
                return Ok((Some(node), false));
            }
            let rebuilt = Node::build(node.key, node.left.clone(), right, work)?;
            if heap_precedes(
                rebuilt.right.as_ref().expect("inserted right child"),
                &rebuilt,
            ) {
                Ok((Some(rotate_left(rebuilt, work)?), true))
            } else {
                Ok((Some(rebuilt), true))
            }
        }
    }
}

fn remove(
    root: Link,
    key: SemanticDatomKey,
    work: &mut CommitmentWork,
) -> Result<(Link, bool), SemanticError> {
    work.visit();
    let Some(node) = root else {
        return Ok((None, false));
    };
    match key.cmp(&node.key) {
        Ordering::Equal => Ok((merge(node.left.clone(), node.right.clone(), work)?, true)),
        Ordering::Less => {
            let (left, changed) = remove(node.left.clone(), key, work)?;
            if !changed {
                return Ok((Some(node), false));
            }
            Ok((
                Some(Node::build(node.key, left, node.right.clone(), work)?),
                true,
            ))
        }
        Ordering::Greater => {
            let (right, changed) = remove(node.right.clone(), key, work)?;
            if !changed {
                return Ok((Some(node), false));
            }
            Ok((
                Some(Node::build(node.key, node.left.clone(), right, work)?),
                true,
            ))
        }
    }
}

fn merge(left: Link, right: Link, work: &mut CommitmentWork) -> Result<Link, SemanticError> {
    match (left, right) {
        (None, right) => Ok(right),
        (left, None) => Ok(left),
        (Some(left), Some(right)) if heap_precedes(&left, &right) => {
            let merged = merge(left.right.clone(), Some(right), work)?;
            Ok(Some(Node::build(
                left.key,
                left.left.clone(),
                merged,
                work,
            )?))
        }
        (Some(left), Some(right)) => {
            let merged = merge(Some(left), right.left.clone(), work)?;
            Ok(Some(Node::build(
                right.key,
                merged,
                right.right.clone(),
                work,
            )?))
        }
    }
}

fn rotate_right(root: Arc<Node>, work: &mut CommitmentWork) -> Result<Arc<Node>, SemanticError> {
    let pivot = root.left.as_ref().expect("rotation requires left child");
    let right = Node::build(root.key, pivot.right.clone(), root.right.clone(), work)?;
    Node::build(pivot.key, pivot.left.clone(), Some(right), work)
}

fn rotate_left(root: Arc<Node>, work: &mut CommitmentWork) -> Result<Arc<Node>, SemanticError> {
    let pivot = root.right.as_ref().expect("rotation requires right child");
    let left = Node::build(root.key, root.left.clone(), pivot.left.clone(), work)?;
    Node::build(pivot.key, Some(left), pivot.right.clone(), work)
}

fn heap_precedes(left: &Node, right: &Node) -> bool {
    left.priority
        .cmp(&right.priority)
        .then_with(|| left.key.cmp(&right.key))
        .is_lt()
}

fn link_count(link: &Link) -> u64 {
    link.as_ref().map_or(0, |node| node.count)
}

fn link_hash(link: &Link) -> Digest {
    link.as_ref()
        .map_or_else(semantic_empty_hash, |node| node.hash)
}

pub(crate) fn semantic_empty_hash() -> Digest {
    hash_parts(EMPTY_DOMAIN, &[])
}

pub(crate) fn semantic_key_priority(key: SemanticDatomKey) -> Digest {
    hash_parts(
        PRIORITY_DOMAIN,
        &[&key.attribute.to_be_bytes(), &key.digest],
    )
}

/// Reproduce the exact v2 node digest from a canonical logical node. Durable
/// implementations use this boundary instead of copying the domain separator
/// or field ordering and silently creating a second commitment format.
pub(crate) fn semantic_node_hash(
    key: SemanticDatomKey,
    count: u64,
    left: Digest,
    right: Digest,
) -> Digest {
    let leaf = semantic_leaf_hash(key);
    hash_parts(NODE_DOMAIN, &[&count.to_be_bytes(), &left, &leaf, &right])
}

fn hash_parts(domain: &[u8], parts: &[&[u8]]) -> Digest {
    let mut hash = Sha256::new();
    hash.update(domain);
    for part in parts {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part);
    }
    hash.finalize().into()
}
