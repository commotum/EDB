//! Path-copy maintenance of the existing authenticated byte-key tree. The
//! mutation spool, one routing path, and one bounded page are resident; old
//! subtrees are referenced directly, never copied into a successor namespace.
use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FulltextMutation {
    pub key: Vec<u8>,
    pub value: Option<Vec<u8>>,
}

pub(crate) fn sort_mutations(
    mutations: impl Iterator<Item = Result<FulltextMutation, SemanticError>>,
    limits: &FulltextBuildLimits,
    stats: &mut FulltextBuildStats,
) -> Result<File, SemanticError> {
    let mut sorter = Sorter::new(limits);
    for mutation in mutations {
        let mutation = mutation?;
        let tag = u8::from(mutation.value.is_some());
        let mut record = FulltextRecord {
            key: mutation.key,
            value: mutation.value.unwrap_or_default(),
        };
        // The ephemeral spool has one operation byte; persisted record limits
        // and page encodings are unchanged.
        validate_record(&record, limits.max_record_bytes)?;
        if record.key.len() > limits.max_key_bytes {
            return Err(incorrect(
                "fulltext/key-size",
                "search key exceeds configured capacity",
            ));
        }
        stats.input_records += 1;
        if stats.input_records > limits.max_records {
            return Err(incorrect(
                "fulltext/build-limit",
                "search mutation input limit exhausted",
            ));
        }
        record.value.insert(0, tag);
        sorter.push(record, stats)?;
    }
    sorter.finish(stats)
}

fn decode_mutation(mut record: FulltextRecord) -> Result<FulltextMutation, SemanticError> {
    let Some(tag) = record.value.first().copied() else {
        return Err(fault("fulltext/spill-frame", "missing mutation tag"));
    };
    if tag > 1 || (tag == 0 && record.value.len() != 1) {
        return Err(fault("fulltext/spill-frame", "invalid mutation tag"));
    }
    record.value.remove(0);
    Ok(FulltextMutation {
        key: record.key,
        value: (tag == 1).then_some(record.value),
    })
}

/// Two streaming lookaheads, independent of both corpus and delta width. The
/// base was authenticated by the source producer; validate its complete count
/// and strict ordering before allowing the bulk builder to publish.
pub(crate) struct EmptyCorpusMerge<'a> {
    base: &'a mut dyn Iterator<Item = Result<FulltextRecord, SemanticError>>,
    mutations: File,
    limits: &'a FulltextBuildLimits,
    expected: u64,
    seen: u64,
    prior: Option<Vec<u8>>,
    base_next: Option<FulltextRecord>,
    mutation_next: Option<FulltextMutation>,
    base_done: bool,
    mutation_done: bool,
    failed: bool,
}
impl<'a> EmptyCorpusMerge<'a> {
    pub(crate) fn new(
        base: &'a mut dyn Iterator<Item = Result<FulltextRecord, SemanticError>>,
        mutations: File,
        expected: u64,
        limits: &'a FulltextBuildLimits,
    ) -> Self {
        Self {
            base,
            mutations,
            limits,
            expected,
            seen: 0,
            prior: None,
            base_next: None,
            mutation_next: None,
            base_done: false,
            mutation_done: false,
            failed: false,
        }
    }
    fn advance(&mut self) -> Result<Option<FulltextRecord>, SemanticError> {
        loop {
            if self.base_next.is_none() && !self.base_done {
                match self.base.next().transpose()? {
                    Some(record) => {
                        validate_record(&record, self.limits.max_record_bytes)?;
                        if record.key.len() > self.limits.max_key_bytes {
                            return Err(incorrect(
                                "fulltext/key-size",
                                "search base key exceeds configured capacity",
                            ));
                        }
                        if self.seen >= self.expected
                            || self.prior.as_ref().is_some_and(|key| key >= &record.key)
                        {
                            return Err(fault(
                                "fulltext/empty-base",
                                "authenticated empty-corpus base is duplicated, unordered, or exceeds its root count",
                            ));
                        }
                        self.prior = Some(record.key.clone());
                        self.seen += 1;
                        self.base_next = Some(record);
                    }
                    None => {
                        self.base_done = true;
                        if self.seen != self.expected {
                            return Err(fault(
                                "fulltext/empty-base",
                                "authenticated empty-corpus base is incomplete",
                            ));
                        }
                    }
                }
            }
            if self.mutation_next.is_none() && !self.mutation_done {
                self.mutation_next = read_record(&mut self.mutations)?
                    .map(decode_mutation)
                    .transpose()?;
                self.mutation_done = self.mutation_next.is_none();
            }
            match (&self.base_next, &self.mutation_next) {
                (None, None) => return Ok(None),
                (Some(_), None) => return Ok(self.base_next.take()),
                (Some(base), Some(mutation)) if base.key < mutation.key => {
                    return Ok(self.base_next.take());
                }
                _ => {
                    let mutation = self.mutation_next.take().unwrap();
                    if self
                        .base_next
                        .as_ref()
                        .is_some_and(|base| base.key == mutation.key)
                    {
                        self.base_next = None;
                    }
                    if let Some(value) = mutation.value {
                        return Ok(Some(FulltextRecord {
                            key: mutation.key,
                            value,
                        }));
                    }
                }
            }
        }
    }
}
impl Iterator for EmptyCorpusMerge<'_> {
    type Item = Result<FulltextRecord, SemanticError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        match self.advance() {
            Ok(record) => record.map(Ok),
            Err(error) => {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}

fn checked_total(value: i128) -> Result<u64, SemanticError> {
    u64::try_from(value).map_err(|_| {
        fault(
            "fulltext/header-counts",
            "incremental search totals overflowed",
        )
    })
}

pub(crate) trait Pages {
    fn load(&mut self, hash: Digest) -> Result<Page, SemanticError>;
    fn save(&mut self, page: &Page) -> Result<Child, SemanticError>;
    fn peak(&mut self, bytes: usize);
}

/// Path-copy the shared byte-key tree using an already admitted sorted spool.
pub(crate) fn edit_pages(
    io: impl Pages,
    predecessor: &FulltextProjection,
    mut sorted: File,
    limits: &FulltextBuildLimits,
) -> Result<PageSummary, SemanticError> {
    let mut editor = Editor {
        io,
        limits,
        blocks: i128::from(predecessor.block_count),
        bytes: i128::from(predecessor.encoded_bytes),
        resident: 0,
    };
    let mut root = predecessor.root_hash;
    let mut count = predecessor.record_count;
    while let Some(record) = read_record(&mut sorted)? {
        (root, count) = editor.apply(root, count, &decode_mutation(record)?)?;
    }
    Ok(PageSummary {
        root,
        records: count,
        bytes: checked_total(editor.bytes)?,
        blocks: checked_total(editor.blocks)?,
    })
}
struct Editor<'a, P> {
    io: P,
    limits: &'a FulltextBuildLimits,
    blocks: i128,
    bytes: i128,
    resident: usize,
}
impl<P: Pages> Editor<'_, P> {
    fn save(&mut self, page: Page) -> Result<Child, SemanticError> {
        if let Page::Branch(children) = &page {
            children
                .iter()
                .try_fold(0u64, |total, child| total.checked_add(child.count))
                .ok_or_else(|| {
                    fault(
                        "fulltext/page-count",
                        "incremental branch record count overflowed",
                    )
                })?;
        }
        let bytes = page.encode()?.len();
        self.io.peak(
            self.resident
                .saturating_add(page.retained_bytes())
                .saturating_add(bytes.saturating_mul(2)),
        );
        let child = self.io.save(&page)?;
        self.blocks += 1;
        self.bytes += bytes as i128;
        Ok(child)
    }
    fn leaves(&mut self, records: Vec<FulltextRecord>) -> Result<Vec<Child>, SemanticError> {
        let mut out = Vec::new();
        let mut group = Vec::new();
        let mut bytes = 13;
        for record in records {
            let size = 8 + record.key.len() + record.value.len();
            if !group.is_empty() && (group.len() == FANOUT || bytes + size > self.limits.page_bytes)
            {
                out.push(self.save(Page::Leaf(std::mem::take(&mut group)))?);
                bytes = 13;
            }
            bytes += size;
            group.push(record);
        }
        if !group.is_empty() {
            out.push(self.save(Page::Leaf(group))?);
        }
        Ok(out)
    }
    fn branches(&mut self, children: Vec<Child>) -> Result<Vec<Child>, SemanticError> {
        let mut out = Vec::new();
        let mut group = Vec::new();
        let mut bytes = 13;
        for child in children {
            let size = 48 + child.first.len() + child.last.len();
            if !group.is_empty() && (group.len() == FANOUT || bytes + size > self.limits.page_bytes)
            {
                out.push(self.save(Page::Branch(std::mem::take(&mut group)))?);
                bytes = 13;
            }
            bytes += size;
            group.push(child);
        }
        if !group.is_empty() {
            out.push(self.save(Page::Branch(group))?);
        }
        Ok(out)
    }
    fn apply(
        &mut self,
        root: Digest,
        count: u64,
        mutation: &FulltextMutation,
    ) -> Result<(Digest, u64), SemanticError> {
        let mut deepest = 0;
        let (mut children, changed) =
            self.edit(root, None, Some(count), mutation, 0, &mut deepest)?;
        if !changed {
            return Ok((root, count));
        }
        if children.is_empty() {
            children.push(self.save(Page::Leaf(Vec::new()))?);
        }
        while children.len() > 1 {
            deepest += 1;
            if deepest > MAX_DEPTH {
                return Err(incorrect(
                    "fulltext/tree-depth",
                    "search update exceeds maximum depth",
                ));
            }
            let old_len = children.len();
            children = self.branches(children)?;
            if children.len() >= old_len {
                return Err(incorrect(
                    "fulltext/encoding-size",
                    "routing keys cannot fit a bounded branch",
                ));
            }
        }
        let root = children.pop().unwrap();
        Ok((root.hash, root.count))
    }
    fn edit(
        &mut self,
        hash: Digest,
        expected: Option<&Child>,
        root_count: Option<u64>,
        mutation: &FulltextMutation,
        depth: usize,
        deepest: &mut usize,
    ) -> Result<(Vec<Child>, bool), SemanticError> {
        if depth > MAX_DEPTH {
            return Err(fault(
                "fulltext/tree-depth",
                "search update path exceeds depth bound",
            ));
        }
        *deepest = (*deepest).max(depth);
        let page = self.io.load(hash)?;
        if let Some(expected) = expected {
            page.validate_child(expected)?;
        }
        if root_count.is_some_and(|count| count != page.count()) {
            return Err(fault(
                "fulltext/root-count",
                "search root differs from header count",
            ));
        }
        let original = page.descriptor(hash);
        let old_bytes = page.encode()?.len();
        let retained = page.retained_bytes();
        self.resident = self.resident.saturating_add(retained);
        self.io.peak(
            self.resident
                .saturating_add(mutation.key.len())
                .saturating_add(mutation.value.as_ref().map_or(0, Vec::len)),
        );
        let result = match page {
            Page::Leaf(mut records) => {
                let position = records.binary_search_by(|r| r.key.cmp(&mutation.key));
                let changed = match (position, &mutation.value) {
                    (Ok(i), Some(value)) if records[i].value == *value => false,
                    (Ok(i), Some(value)) => {
                        records[i].value = value.clone();
                        true
                    }
                    (Ok(i), None) => {
                        records.remove(i);
                        true
                    }
                    (Err(i), Some(value)) => {
                        records.insert(
                            i,
                            FulltextRecord {
                                key: mutation.key.clone(),
                                value: value.clone(),
                            },
                        );
                        true
                    }
                    (Err(_), None) => false,
                };
                if changed {
                    self.blocks -= 1;
                    self.bytes -= old_bytes as i128;
                    (self.leaves(records)?, true)
                } else {
                    (vec![original], false)
                }
            }
            Page::Branch(mut children) => {
                let i = children
                    .partition_point(|c| c.first <= mutation.key)
                    .saturating_sub(1);
                let (replacement, changed) = self.edit(
                    children[i].hash,
                    Some(&children[i]),
                    None,
                    mutation,
                    depth + 1,
                    deepest,
                )?;
                if changed {
                    children.splice(i..=i, replacement);
                    self.blocks -= 1;
                    self.bytes -= old_bytes as i128;
                    (self.branches(children)?, true)
                } else {
                    (vec![original], false)
                }
            }
        };
        self.resident = self.resident.saturating_sub(retained);
        Ok(result)
    }
}
