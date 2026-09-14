//! Bounded spill sorting and bulk page packing; no document-value semantics.
use super::*;

pub(crate) struct PageSummary {
    pub(crate) root: Digest,
    pub(crate) records: u64,
    pub(crate) bytes: u64,
    pub(crate) blocks: u64,
}

/// Shared bounded bulk packer. The sink chooses physical storage; sorted
/// record semantics, fanout, depth and page admission stay here.
pub(crate) fn build_pages(
    records: impl Iterator<Item = Result<FulltextRecord, SemanticError>>,
    limits: &FulltextBuildLimits,
    save: &mut impl FnMut(&Page) -> Result<Child, SemanticError>,
) -> Result<PageSummary, SemanticError> {
    fn push(
        page: Page,
        level: usize,
        levels: &mut Vec<Vec<Child>>,
        limits: &FulltextBuildLimits,
        summary: &mut PageSummary,
        save: &mut impl FnMut(&Page) -> Result<Child, SemanticError>,
    ) -> Result<(), SemanticError> {
        if level > MAX_DEPTH {
            return Err(incorrect(
                "fulltext/tree-depth",
                "search build exceeds maximum depth",
            ));
        }
        summary.bytes += page.encode()?.len() as u64;
        summary.blocks += 1;
        let child = save(&page)?;
        while levels.len() <= level {
            levels.push(Vec::new());
        }
        levels[level].push(child);
        let estimated: usize = levels[level]
            .iter()
            .map(|c| c.first.len() + c.last.len() + 48)
            .sum();
        if levels[level].len() >= FANOUT
            || (levels[level].len() > 1 && estimated >= limits.page_bytes)
        {
            let children = std::mem::take(&mut levels[level]);
            push(
                Page::Branch(children),
                level + 1,
                levels,
                limits,
                summary,
                save,
            )?;
        }
        Ok(())
    }
    let mut summary = PageSummary {
        root: [0; 32],
        records: 0,
        bytes: 0,
        blocks: 0,
    };
    let mut levels = Vec::new();
    let mut leaf = Vec::new();
    let mut bytes = 13;
    for record in records {
        let record = record?;
        let size = record.key.len() + record.value.len() + 8;
        if !leaf.is_empty() && (leaf.len() >= FANOUT || bytes + size > limits.page_bytes) {
            push(
                Page::Leaf(std::mem::take(&mut leaf)),
                0,
                &mut levels,
                limits,
                &mut summary,
                save,
            )?;
            bytes = 13;
        }
        bytes += size;
        leaf.push(record);
        summary.records += 1;
    }
    if !leaf.is_empty() || levels.is_empty() {
        push(Page::Leaf(leaf), 0, &mut levels, limits, &mut summary, save)?;
    }
    loop {
        let nonempty = levels
            .iter()
            .enumerate()
            .filter(|(_, c)| !c.is_empty())
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        if nonempty.len() == 1 && levels[nonempty[0]].len() == 1 {
            summary.root = levels[nonempty[0]][0].hash;
            return Ok(summary);
        }
        let level = nonempty[0];
        let children = std::mem::take(&mut levels[level]);
        push(
            Page::Branch(children),
            level + 1,
            &mut levels,
            limits,
            &mut summary,
            save,
        )?;
    }
}

pub(crate) struct SortedRecords(File);

impl Iterator for SortedRecords {
    type Item = Result<FulltextRecord, SemanticError>;
    fn next(&mut self) -> Option<Self::Item> {
        read_record(&mut self.0).transpose()
    }
}

pub(crate) fn sorted_records(
    records: impl Iterator<Item = Result<FulltextRecord, SemanticError>>,
    limits: &FulltextBuildLimits,
    stats: &mut FulltextBuildStats,
) -> Result<SortedRecords, SemanticError> {
    limits.validate()?;
    let mut sorter = Sorter::new(limits);
    for record in records {
        let record = record?;
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
                "search record build limit exhausted",
            ));
        }
        sorter.push(record, stats)?;
    }
    Ok(SortedRecords(sorter.finish(stats)?))
}

pub(super) struct Sorter<'a> {
    limits: &'a FulltextBuildLimits,
    buffer: Vec<FulltextRecord>,
    bytes: usize,
    levels: Vec<Option<File>>,
}
impl<'a> Sorter<'a> {
    pub(super) fn new(limits: &'a FulltextBuildLimits) -> Self {
        Self {
            limits,
            buffer: Vec::new(),
            bytes: 0,
            levels: Vec::new(),
        }
    }
    pub(super) fn push(
        &mut self,
        record: FulltextRecord,
        stats: &mut FulltextBuildStats,
    ) -> Result<(), SemanticError> {
        self.bytes = self.bytes.saturating_add(
            record.key.len() + record.value.len() + std::mem::size_of::<FulltextRecord>(),
        );
        self.buffer.push(record);
        stats.peak_buffer_bytes = stats.peak_buffer_bytes.max(self.bytes);
        if self.bytes >= self.limits.sort_memory_bytes {
            self.flush(stats)?;
        }
        Ok(())
    }
    fn file(&self) -> Result<File, SemanticError> {
        tempfile::tempfile_in(&self.limits.work_directory)
            .map_err(|e| io_error("fulltext/spill-open", e))
    }
    fn flush(&mut self, stats: &mut FulltextBuildStats) -> Result<(), SemanticError> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        self.buffer.sort_by(|a, b| a.key.cmp(&b.key));
        let mut run = self.file()?;
        let mut prior: Option<FulltextRecord> = None;
        for record in self.buffer.drain(..) {
            if let Some(p) = prior.as_ref()
                && p.key == record.key
            {
                if p.value != record.value {
                    return Err(incorrect(
                        "fulltext/duplicate-key",
                        "search records disagree on one key",
                    ));
                }
                continue;
            }
            if let Some(p) = prior.replace(record) {
                write_record(&mut run, &p, self.limits, stats)?;
            }
        }
        if let Some(p) = prior {
            write_record(&mut run, &p, self.limits, stats)?;
        }
        self.bytes = 0;
        rewind(&mut run)?;
        let mut level = 0;
        loop {
            if self.levels.len() <= level {
                self.levels.push(None);
            }
            if let Some(existing) = self.levels[level].take() {
                run = self.merge(existing, run, stats)?;
                level += 1;
            } else {
                self.levels[level] = Some(run);
                break;
            }
        }
        Ok(())
    }
    fn merge(
        &self,
        mut a: File,
        mut b: File,
        stats: &mut FulltextBuildStats,
    ) -> Result<File, SemanticError> {
        let mut output = self.file()?;
        let mut left = read_record(&mut a)?;
        let mut right = read_record(&mut b)?;
        while left.is_some() || right.is_some() {
            let take_left = match (&left, &right) {
                (Some(l), Some(r)) => l.key <= r.key,
                (Some(_), None) => true,
                _ => false,
            };
            let record = if take_left {
                let value = left.take().unwrap();
                left = read_record(&mut a)?;
                if right.as_ref().is_some_and(|r| r.key == value.key) {
                    let r = right.take().unwrap();
                    if r.value != value.value {
                        return Err(incorrect(
                            "fulltext/duplicate-key",
                            "search records disagree on one key",
                        ));
                    }
                    right = read_record(&mut b)?;
                }
                value
            } else {
                let value = right.take().unwrap();
                right = read_record(&mut b)?;
                value
            };
            write_record(&mut output, &record, self.limits, stats)?;
        }
        rewind(&mut output)?;
        Ok(output)
    }
    pub(super) fn finish(mut self, stats: &mut FulltextBuildStats) -> Result<File, SemanticError> {
        self.flush(stats)?;
        let mut result = None;
        for level in 0..self.levels.len() {
            if let Some(run) = self.levels[level].take() {
                result = Some(if let Some(previous) = result {
                    self.merge(previous, run, stats)?
                } else {
                    run
                });
            }
        }
        match result {
            Some(run) => Ok(run),
            None => self.file(),
        }
    }
}
pub(super) fn validate_record(r: &FulltextRecord, max: usize) -> Result<(), SemanticError> {
    if r.key.is_empty() || r.key.len() > MAX_KEY || r.key.len().saturating_add(r.value.len()) > max
    {
        return Err(incorrect(
            "fulltext/record-size",
            "search record exceeds configured key/value bound",
        ));
    }
    Ok(())
}
fn write_record(
    file: &mut File,
    r: &FulltextRecord,
    limits: &FulltextBuildLimits,
    stats: &mut FulltextBuildStats,
) -> Result<(), SemanticError> {
    let mut bytes = Vec::new();
    put_bytes(&mut bytes, &r.key)?;
    put_bytes(&mut bytes, &r.value)?;
    let hash = sha256(&bytes);
    stats.spill_bytes = stats.spill_bytes.saturating_add(bytes.len() as u64 + 32);
    if stats.spill_bytes > limits.max_spill_bytes {
        return Err(incorrect(
            "fulltext/spill-limit",
            "search cumulative spill-write budget exhausted",
        ));
    }
    file.write_all(&bytes)
        .and_then(|_| file.write_all(&hash))
        .map_err(|e| io_error("fulltext/spill-write", e))
}
pub(super) fn read_record(file: &mut File) -> Result<Option<FulltextRecord>, SemanticError> {
    let mut length = [0; 4];
    let read = file
        .read(&mut length)
        .map_err(|e| io_error("fulltext/spill-read", e))?;
    if read == 0 {
        return Ok(None);
    }
    file.read_exact(&mut length[read..])
        .map_err(|e| io_error("fulltext/spill-frame", e))?;
    let key_length = u32::from_be_bytes(length) as usize;
    if key_length == 0 || key_length > MAX_KEY {
        return Err(fault("fulltext/spill-frame", "invalid spill key length"));
    }
    let mut key = vec![0; key_length];
    file.read_exact(&mut key)
        .map_err(|e| io_error("fulltext/spill-frame", e))?;
    let mut value_length = [0; 4];
    file.read_exact(&mut value_length)
        .map_err(|e| io_error("fulltext/spill-frame", e))?;
    let n = u32::from_be_bytes(value_length) as usize;
    // Incremental mutation spools carry one private operation byte. Persisted
    // pages still enforce MAX_RECORD, independently of this temporary format.
    if n > (MAX_RECORD + 1).saturating_sub(key_length) {
        return Err(fault("fulltext/spill-frame", "invalid spill value length"));
    }
    let mut value = vec![0; n];
    file.read_exact(&mut value)
        .map_err(|e| io_error("fulltext/spill-frame", e))?;
    let mut hash = [0; 32];
    file.read_exact(&mut hash)
        .map_err(|e| io_error("fulltext/spill-frame", e))?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&length);
    bytes.extend_from_slice(&key);
    bytes.extend_from_slice(&value_length);
    bytes.extend_from_slice(&value);
    if sha256(&bytes) != hash {
        return Err(fault(
            "fulltext/spill-hash",
            "search spill checksum mismatch",
        ));
    }
    Ok(Some(FulltextRecord { key, value }))
}
fn rewind(file: &mut File) -> Result<(), SemanticError> {
    file.seek(SeekFrom::Start(0))
        .map(|_| ())
        .map_err(|e| io_error("fulltext/spill-seek", e))
}
