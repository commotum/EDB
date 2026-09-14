//! Owned lazy range traversal and exact-key lookup with authenticated bounds.
use super::*;

type Loader =
    Box<dyn FnMut(Digest, &mut FulltextReadStats) -> Result<Arc<Page>, SemanticError> + Send>;
/// Owned, lazy Merkle range traversal over a captured immutable source.
/// Each decoded child is checked against its parent's range/count proof.
pub struct FulltextCursor {
    loader: Loader,
    pending: Vec<(Child, usize)>,
    leaf: Option<Arc<Page>>,
    offset: usize,
    lower: Vec<u8>,
    upper: Option<Vec<u8>>,
    limits: FulltextReadLimits,
    stats: FulltextReadStats,
    failed: bool,
    operation: Option<OperationContext>,
}
impl FulltextCursor {
    pub(crate) fn new(
        projection: &FulltextProjection,
        prefix: &[u8],
        limits: FulltextReadLimits,
        loader: Loader,
    ) -> Result<Self, SemanticError> {
        if prefix.len() > MAX_KEY || limits.max_records == 0 || limits.max_block_bytes == 0 {
            return Err(incorrect(
                "fulltext/read-limits",
                "invalid search range/read limits",
            ));
        }
        Ok(Self {
            loader,
            pending: vec![(
                Child {
                    first: Vec::new(),
                    last: Vec::new(),
                    hash: projection.root_hash,
                    count: projection.record_count,
                },
                0,
            )],
            leaf: None,
            offset: 0,
            lower: prefix.to_vec(),
            upper: prefix_upper(prefix),
            limits,
            stats: FulltextReadStats::default(),
            failed: false,
            operation: OperationContext::current(),
        })
    }
    pub fn stats(&self) -> FulltextReadStats {
        self.stats
    }
    fn advance(&mut self) -> Result<Option<FulltextRecord>, SemanticError> {
        loop {
            if let Some(page) = &self.leaf {
                let Page::Leaf(records) = page.as_ref() else {
                    unreachable!()
                };
                while let Some(record) = records.get(self.offset) {
                    self.offset += 1;
                    self.stats.records_examined += 1;
                    if record.key < self.lower {
                        continue;
                    }
                    if self
                        .upper
                        .as_ref()
                        .is_some_and(|upper| record.key >= *upper)
                    {
                        self.pending.clear();
                        self.leaf = None;
                        return Ok(None);
                    }
                    if self.stats.records_yielded >= self.limits.max_records {
                        return Err(SemanticError::new(
                            ErrorCategory::Busy,
                            "fulltext/read-limit",
                            "search record budget exhausted",
                        ));
                    }
                    self.stats.records_yielded += 1;
                    return Ok(Some(record.clone()));
                }
                self.leaf = None;
            }
            let Some((child, depth)) = self.pending.pop() else {
                return Ok(None);
            };
            if depth > MAX_DEPTH {
                return Err(fault(
                    "fulltext/tree-depth",
                    "search tree depth exceeds bound",
                ));
            }
            let page = (self.loader)(child.hash, &mut self.stats)?;
            self.stats.visited_bytes = self
                .stats
                .visited_bytes
                .saturating_add(page.retained_bytes() as u64);
            if self.stats.block_bytes.max(self.stats.visited_bytes) > self.limits.max_block_bytes {
                return Err(SemanticError::new(
                    ErrorCategory::Busy,
                    "fulltext/read-limit",
                    "search block-byte budget exhausted (including current bounded page)",
                ));
            }
            if depth > 0 {
                page.validate_child(&child)?;
            } else if page.count() != child.count {
                return Err(fault(
                    "fulltext/root-count",
                    "search root differs from header count",
                ));
            }
            match page.as_ref() {
                Page::Leaf(_) => {
                    self.leaf = Some(page);
                    self.offset = 0;
                }
                Page::Branch(children) => {
                    for child in children.iter().rev() {
                        if child.last >= self.lower
                            && self.upper.as_ref().is_none_or(|u| child.first < *u)
                        {
                            self.pending.push((child.clone(), depth + 1));
                        }
                    }
                }
            }
        }
    }
}
/// Synchronous selective lookup for a build worker's borrowed page source.
/// It applies the same authentication and byte/depth limits as public cursors.
pub(crate) fn lookup_record(
    projection: &FulltextProjection,
    key: &[u8],
    mut load: impl FnMut(Digest, &mut FulltextReadStats) -> Result<Page, SemanticError>,
) -> Result<(Option<FulltextRecord>, FulltextReadStats), SemanticError> {
    let mut stats = FulltextReadStats::default();
    let mut expected: Option<Child> = None;
    let mut id = projection.root_hash;
    for depth in 0..=MAX_DEPTH {
        let page = load(id, &mut stats)?;
        stats.visited_bytes = stats
            .visited_bytes
            .saturating_add(page.retained_bytes() as u64);
        if stats.block_bytes.max(stats.visited_bytes) > 128 * 1024 * 1024 {
            return Err(SemanticError::new(
                ErrorCategory::Busy,
                "fulltext/read-limit",
                "Search lookup block-byte budget exhausted",
            ));
        }
        if let Some(child) = &expected {
            page.validate_child(child)?;
        } else if page.count() != projection.record_count {
            return Err(fault(
                "fulltext/root-count",
                "Search root differs from header count",
            ));
        }
        match page {
            Page::Leaf(records) => {
                let record = records
                    .binary_search_by(|record| record.key.as_slice().cmp(key))
                    .ok()
                    .map(|i| records[i].clone());
                stats.records_examined += 1;
                stats.records_yielded += u64::from(record.is_some());
                return Ok((record, stats));
            }
            Page::Branch(children) => {
                let Some(child) = children
                    .into_iter()
                    .find(|child| child.first.as_slice() <= key && child.last.as_slice() >= key)
                else {
                    return Ok((None, stats));
                };
                id = child.hash;
                expected = Some(child);
                if depth == MAX_DEPTH {
                    break;
                }
            }
        }
    }
    Err(fault(
        "fulltext/tree-depth",
        "Search tree depth exceeds bound",
    ))
}

impl Iterator for FulltextCursor {
    type Item = Result<FulltextRecord, SemanticError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        let operation = self.operation.clone();
        let _guard = operation.as_ref().map(OperationContext::enter);
        match self.advance() {
            Ok(Some(r)) => Some(Ok(r)),
            Ok(None) => None,
            Err(e) => {
                self.failed = true;
                Some(Err(e))
            }
        }
    }
}

fn prefix_upper(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut upper = prefix.to_vec();
    while let Some(last) = upper.pop() {
        if last != 255 {
            upper.push(last + 1);
            return Some(upper);
        }
    }
    None
}
