//! Streaming Merkle difference of two authenticated canonical history trees.
//! Equal directories/leaves are skipped before any contained datom is decoded.
//! Unequal leaf streams are merged globally, including across changed physical
//! boundaries; a split/repack alone therefore creates no document mutations.
use crate::index::cursor::{LoadedDirectory, LoadedLeaf};
use crate::index::tree::{ChildRef, RootNode, TreeReadStats};
use crate::{Datom, Digest, ErrorCategory, IndexOrder, SemanticError};
use std::cmp::Ordering as Cmp;
use std::sync::Arc;

fn fault(code: &'static str, message: &'static str) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, code, message)
}

pub(crate) struct HistoryDifference<S> {
    before: Side<S>,
    after: Side<S>,
    failed: bool,
    pub(crate) references_examined: u64,
    pub(crate) datoms_examined: u64,
}

struct Side<S> {
    source: S,
    root: Arc<RootNode>,
    directory_index: usize,
    directory: Option<LoadedDirectory>,
    leaf_index: usize,
    leaf: Option<LoadedLeaf>,
    datom_index: usize,
    head: Option<Datom>,
    reads: TreeReadStats,
}

pub(crate) trait HistorySource {
    fn directory(
        &self,
        reference: &ChildRef,
        reads: &mut TreeReadStats,
    ) -> Result<LoadedDirectory, SemanticError>;
    fn leaf(
        &self,
        reference: &ChildRef,
        reads: &mut TreeReadStats,
    ) -> Result<LoadedLeaf, SemanticError>;
}

impl<S: HistorySource> Side<S> {
    fn from_root(source: S, root: Arc<RootNode>) -> Self {
        Self {
            source,
            root,
            directory_index: 0,
            directory: None,
            leaf_index: 0,
            leaf: None,
            datom_index: 0,
            head: None,
            reads: TreeReadStats::default(),
        }
    }

    fn exhausted(&self) -> bool {
        self.directory_index == self.root.directories.len()
    }

    fn directory_hash(&self) -> Option<Digest> {
        self.directory
            .is_none()
            .then(|| {
                self.root
                    .directories
                    .get(self.directory_index)
                    .map(|r| r.hash)
            })
            .flatten()
    }

    fn load_directory(&mut self) -> Result<(), SemanticError> {
        if self.directory.is_none() && !self.exhausted() {
            self.directory = Some(self.source.directory(
                &self.root.directories[self.directory_index],
                &mut self.reads,
            )?);
            self.leaf_index = 0;
        }
        Ok(())
    }

    fn leaf_hash(&self) -> Option<Digest> {
        self.leaf
            .is_none()
            .then(|| {
                self.directory
                    .as_ref()
                    .and_then(|d| d.leaves.get(self.leaf_index))
                    .map(|r| r.hash)
            })
            .flatten()
    }

    fn advance_leaf(&mut self) {
        self.leaf = None;
        self.head = None;
        self.datom_index = 0;
        self.leaf_index += 1;
        if self
            .directory
            .as_ref()
            .is_some_and(|d| self.leaf_index == d.leaves.len())
        {
            self.directory = None;
            self.directory_index += 1;
        }
    }

    fn fill_head(&mut self) -> Result<bool, SemanticError> {
        if self.head.is_some() || self.exhausted() {
            return Ok(false);
        }
        if self.leaf.is_none() {
            let directory = self
                .directory
                .as_ref()
                .expect("directory loaded before leaf");
            self.leaf = Some(
                self.source
                    .leaf(&directory.leaves[self.leaf_index], &mut self.reads)?,
            );
        }
        self.head = self
            .leaf
            .as_ref()
            .and_then(|leaf| leaf.datom(self.datom_index));
        if self.head.is_none() {
            return Err(fault(
                "fulltext/empty-history-leaf",
                "canonical history leaf is empty",
            ));
        }
        Ok(true)
    }

    fn take(&mut self) -> Datom {
        let datom = self.head.take().expect("loaded history head");
        self.datom_index += 1;
        if self
            .leaf
            .as_ref()
            .is_some_and(|leaf| self.datom_index == leaf.len())
        {
            self.advance_leaf();
        }
        datom
    }
}

impl<S: HistorySource> HistoryDifference<S> {
    pub(crate) fn from_roots(
        before: S,
        before_root: Arc<RootNode>,
        after: S,
        after_root: Arc<RootNode>,
        equal: bool,
    ) -> Self {
        let mut result = Self {
            before: Side::from_root(before, before_root),
            after: Side::from_root(after, after_root),
            failed: false,
            references_examined: 0,
            datoms_examined: 0,
        };
        if equal {
            result.before.directory_index = result.before.root.directories.len();
            result.after.directory_index = result.after.root.directories.len();
        }
        result
    }
    fn next_difference(&mut self) -> Result<Option<(Datom, bool)>, SemanticError> {
        loop {
            if self.before.exhausted() && self.after.exhausted() {
                return Ok(None);
            }
            let directory_hashes = (self.before.directory_hash(), self.after.directory_hash());
            self.references_examined +=
                u64::from(directory_hashes.0.is_some()) + u64::from(directory_hashes.1.is_some());
            if let (Some(before), Some(after)) = directory_hashes
                && before == after
            {
                self.before.directory_index += 1;
                self.after.directory_index += 1;
                continue;
            }
            self.before.load_directory()?;
            self.after.load_directory()?;
            let leaf_hashes = (self.before.leaf_hash(), self.after.leaf_hash());
            self.references_examined +=
                u64::from(leaf_hashes.0.is_some()) + u64::from(leaf_hashes.1.is_some());
            if let (Some(before), Some(after)) = leaf_hashes
                && before == after
            {
                self.before.advance_leaf();
                self.after.advance_leaf();
                continue;
            }
            self.datoms_examined += u64::from(self.before.fill_head()?);
            self.datoms_examined += u64::from(self.after.fill_head()?);
            match (&self.before.head, &self.after.head) {
                (Some(before), Some(after)) => match before.cmp_in(after, IndexOrder::Eavt) {
                    Cmp::Less => return Ok(Some((self.before.take(), false))),
                    Cmp::Greater => return Ok(Some((self.after.take(), true))),
                    Cmp::Equal => {
                        self.before.take();
                        self.after.take();
                    }
                },
                (Some(_), None) => return Ok(Some((self.before.take(), false))),
                (None, Some(_)) => return Ok(Some((self.after.take(), true))),
                (None, None) => return Ok(None),
            }
        }
    }
}

impl<S: HistorySource> Iterator for HistoryDifference<S> {
    type Item = Result<(Datom, bool), SemanticError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        match self.next_difference() {
            Ok(Some(change)) => Some(Ok(change)),
            Ok(None) => None,
            Err(error) => {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}
