//! Immutable transaction pages with a bounded tail and backward skip links.
//!
//! Each append writes an entry and one replacement tail page, independent of
//! log length. Large entries additionally write payload chunks. Full pages are
//! sealed without rewriting them: the next page links back by powers of two.
//! Append accepts objects through ObjectWriter; the caller must flush before
//! independent reads/publication. Root publication and protection remain separate.

use super::root::Block;
use super::{ObjectId, ObjectReader, ObjectWriter};
use crate::encoding::{
    canonical_datom_bytes, decode_canonical_datoms, validate_transaction_content,
};
use crate::{Datom, ErrorCategory, INITIAL_EIDX_FRONTIER, IndexOrder, MAX_EIDX, SemanticError};

pub const LOG_PAGE_KIND: u16 = 10;
pub const LOG_ENTRY_KIND: u16 = 11;
pub const LOG_CHUNK_KIND: u16 = 12;
pub const LOG_PAGE_ENTRIES: usize = 64;
const INLINE_BYTES: usize = 64 * 1024;
const CHUNK_BYTES: usize = 4 * 1024 * 1024;
// Current transaction codecs admit a 64 MiB body plus envelope/lineage data.
// Preserve that admitted size even when an entry exceeds one physical object.
pub const MAX_LOG_TRANSACTION_BYTES: usize = 64 * 1024 * 1024 + 4096;
const ENTRY_HEADER: usize = 40;
const PAGE_HEADER: usize = 28;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogEntry {
    pub basis_t: u64,
    pub eidx_frontier: u64,
    pub reserved_frontier: u64,
    /// Current typed datoms; caller-chosen tempid names belong to receipts.
    pub tx_data: Vec<Datom>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogRecord {
    pub id: ObjectId,
    pub encoded_bytes: u64,
    pub entry: LogEntry,
}

impl LogEntry {
    fn encode_datoms(&self) -> Result<Vec<u8>, SemanticError> {
        validate_transaction_content(self.basis_t, self.eidx_frontier, &self.tx_data)?;
        validate_frontiers(self.eidx_frontier, self.reserved_frontier)?;
        let mut datoms = self.tx_data.iter().collect::<Vec<_>>();
        datoms.sort_by(|a, b| a.cmp_in(b, IndexOrder::Eavt));
        let mut bytes = Vec::new();
        for datom in datoms {
            let encoded = canonical_datom_bytes(datom)?;
            if encoded.len() > MAX_LOG_TRANSACTION_BYTES.saturating_sub(bytes.len()) {
                return Err(invalid(
                    "storage/log-entry-size",
                    "Transaction exceeds the log content bound",
                ));
            }
            bytes.extend_from_slice(&encoded);
        }
        Ok(bytes)
    }

    fn store_encoded(
        &self,
        store: &mut dyn ObjectWriter,
        bytes: &[u8],
    ) -> Result<ObjectId, SemanticError> {
        let mut payload = Vec::with_capacity(ENTRY_HEADER + bytes.len().min(INLINE_BYTES));
        for value in [
            self.basis_t,
            self.eidx_frontier,
            self.reserved_frontier,
            self.tx_data.len() as u64,
            bytes.len() as u64,
        ] {
            payload.extend_from_slice(&value.to_be_bytes());
        }
        let mut links = Vec::new();
        if bytes.len() <= INLINE_BYTES {
            payload.extend_from_slice(bytes);
        } else {
            for chunk in bytes.chunks(CHUNK_BYTES) {
                links.push(
                    store.put_object(
                        &Block {
                            kind: LOG_CHUNK_KIND,
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
                kind: LOG_ENTRY_KIND,
                links,
                payload,
            }
            .encode()?,
        )
    }
}

#[derive(Clone, Debug)]
struct Page {
    index: u64,
    eidx_frontier: u64,
    reserved_frontier: u64,
    entries: Vec<ObjectId>,
    skips: Vec<ObjectId>,
}

impl Page {
    fn basis_t(&self) -> u64 {
        self.index * LOG_PAGE_ENTRIES as u64 + self.entries.len() as u64
    }

    fn encode(&self) -> Result<Vec<u8>, SemanticError> {
        let mut payload = Vec::with_capacity(PAGE_HEADER);
        for value in [self.index, self.eidx_frontier, self.reserved_frontier] {
            payload.extend_from_slice(&value.to_be_bytes());
        }
        payload.extend_from_slice(&(self.entries.len() as u16).to_be_bytes());
        payload.push(self.skips.len() as u8);
        payload.push(0);
        Block {
            kind: LOG_PAGE_KIND,
            links: self.entries.iter().chain(&self.skips).copied().collect(),
            payload,
        }
        .encode()
    }

    fn load(store: &mut dyn ObjectReader, id: ObjectId) -> Result<Self, SemanticError> {
        let block = load_block(store, id)?;
        if block.kind != LOG_PAGE_KIND || block.payload.len() != PAGE_HEADER {
            return Err(invalid(
                "storage/log-page-format",
                "Expected a current log page",
            ));
        }
        let index = number(&block.payload, 0);
        let eidx_frontier = number(&block.payload, 8);
        let reserved_frontier = number(&block.payload, 16);
        let entries = u16::from_be_bytes(block.payload[24..26].try_into().unwrap()) as usize;
        let skips = block.payload[26] as usize;
        let expected_skips = (u64::BITS - index.leading_zeros()) as usize;
        if entries == 0
            || entries > LOG_PAGE_ENTRIES
            || skips != expected_skips
            || block.payload[27] != 0
            || block.links.len() != entries + skips
            || index > (MAX_EIDX - entries as u64) / LOG_PAGE_ENTRIES as u64
        {
            return Err(invalid(
                "storage/log-page-shape",
                "Log page counts or navigation coordinates are invalid",
            ));
        }
        validate_frontiers(eidx_frontier, reserved_frontier)?;
        Ok(Self {
            index,
            eidx_frontier,
            reserved_frontier,
            entries: block.links[..entries].to_vec(),
            skips: block.links[entries..].to_vec(),
        })
    }
}

/// A captured immutable log endpoint. Cloning retains only one bounded page;
/// no SQL reference is read or changed by append/read/range.
#[derive(Clone, Debug, Default)]
pub struct LogRoot {
    head: Option<ObjectId>,
    tail: Option<Page>,
}

impl LogRoot {
    pub fn empty() -> Self {
        Self::default()
    }
    pub fn head(&self) -> Option<ObjectId> {
        self.head
    }
    pub(crate) fn latest_entry_id(&self) -> Option<ObjectId> {
        self.tail
            .as_ref()
            .and_then(|page| page.entries.last().copied())
    }
    pub fn basis_t(&self) -> u64 {
        self.tail.as_ref().map_or(0, Page::basis_t)
    }
    pub fn eidx_frontier(&self) -> u64 {
        self.tail
            .as_ref()
            .map_or(INITIAL_EIDX_FRONTIER, |p| p.eidx_frontier)
    }
    pub fn reserved_frontier(&self) -> u64 {
        self.tail
            .as_ref()
            .map_or(INITIAL_EIDX_FRONTIER, |p| p.reserved_frontier)
    }

    pub fn open(store: &mut dyn ObjectReader, head: ObjectId) -> Result<Self, SemanticError> {
        let tail = Page::load(store, head)?;
        // Check the endpoint against its entry metadata without decoding its
        // potentially chunked transaction. Ordinary opens never walk history.
        let block = load_block(store, *tail.entries.last().unwrap())?;
        let metadata = entry_metadata(&block)?;
        if metadata.basis_t != tail.basis_t()
            || metadata.eidx_frontier != tail.eidx_frontier
            || metadata.reserved_frontier != tail.reserved_frontier
        {
            return Err(invalid(
                "storage/log-endpoint",
                "Log tail metadata differs from its final transaction",
            ));
        }
        Ok(Self {
            head: Some(head),
            tail: Some(tail),
        })
    }

    /// Stage one successor; the caller publishes its head by guarded root CAS.
    /// Branching from an older root does not change any captured endpoint.
    pub fn append(
        &self,
        store: &mut dyn ObjectWriter,
        entry: &LogEntry,
    ) -> Result<Self, SemanticError> {
        let bytes = entry.encode_datoms()?;
        self.append_encoded(store, entry, &bytes)
    }

    /// The writer's resident successor and immediate report must expose the
    /// exact datoms encoded in the durable log (including canonical float
    /// zeros/NaNs). Decode the already admitted body locally, without another
    /// SQL read or a second value-normalization algorithm.
    pub(crate) fn append_canonical(
        &self,
        store: &mut dyn ObjectWriter,
        entry: &LogEntry,
    ) -> Result<(Self, Vec<Datom>), SemanticError> {
        let bytes = entry.encode_datoms()?;
        let tx_data = decode_canonical_datoms(&bytes, entry.tx_data.len())?;
        self.append_encoded(store, entry, &bytes)
            .map(|log| (log, tx_data))
    }

    fn append_encoded(
        &self,
        store: &mut dyn ObjectWriter,
        entry: &LogEntry,
        bytes: &[u8],
    ) -> Result<Self, SemanticError> {
        if entry.basis_t != self.basis_t() + 1
            || entry.eidx_frontier < self.eidx_frontier()
            || entry.reserved_frontier < self.reserved_frontier()
        {
            return Err(invalid(
                "storage/log-append-coordinate",
                "Log append must advance exactly one basis without retreating allocation frontiers",
            ));
        }
        let mut page = match &self.tail {
            Some(tail) if tail.entries.len() < LOG_PAGE_ENTRIES => tail.clone(),
            Some(tail) => {
                let index = tail.index + 1;
                let mut skips = vec![self.head.unwrap()];
                let levels = (u64::BITS - index.leading_zeros()) as usize;
                for level in 1..levels {
                    let prior = Page::load(store, skips[level - 1])?;
                    let expected_index = index - (1_u64 << (level - 1));
                    if prior.index != expected_index || prior.entries.len() != LOG_PAGE_ENTRIES {
                        return Err(invalid(
                            "storage/log-skip-coordinate",
                            "Sealed log page does not match its navigation coordinate",
                        ));
                    }
                    skips.push(prior.skips[level - 1]);
                }
                Page {
                    index,
                    eidx_frontier: entry.eidx_frontier,
                    reserved_frontier: entry.reserved_frontier,
                    entries: Vec::new(),
                    skips,
                }
            }
            None => Page {
                index: 0,
                eidx_frontier: entry.eidx_frontier,
                reserved_frontier: entry.reserved_frontier,
                entries: Vec::new(),
                skips: Vec::new(),
            },
        };
        page.entries.push(entry.store_encoded(store, bytes)?);
        page.eidx_frontier = entry.eidx_frontier;
        page.reserved_frontier = entry.reserved_frontier;
        let head = store.put_object(&page.encode()?)?;
        Ok(Self {
            head: Some(head),
            tail: Some(page),
        })
    }

    pub fn read(
        &self,
        store: &mut dyn ObjectReader,
        basis_t: u64,
    ) -> Result<Option<LogEntry>, SemanticError> {
        if basis_t == 0 || basis_t > self.basis_t() {
            return Ok(None);
        }
        let page = self.page_for(store, basis_t)?;
        read_entry(store, &page, basis_t).map(|record| Some(record.entry))
    }

    pub fn read_record(
        &self,
        store: &mut dyn ObjectReader,
        basis_t: u64,
    ) -> Result<Option<LogRecord>, SemanticError> {
        if basis_t == 0 || basis_t > self.basis_t() {
            return Ok(None);
        }
        let page = self.page_for(store, basis_t)?;
        read_entry(store, &page, basis_t).map(Some)
    }

    /// Bound encoded transaction content before fetching chunks or decoding
    /// datoms. Callers additionally account their decoded event representation.
    pub(crate) fn read_record_bounded(
        &self,
        store: &mut dyn ObjectReader,
        basis_t: u64,
        max_bytes: usize,
    ) -> Result<Option<LogRecord>, SemanticError> {
        if basis_t == 0 || basis_t > self.basis_t() {
            return Ok(None);
        }
        let page = self.page_for(store, basis_t)?;
        read_entry_bounded(store, &page, basis_t, max_bytes).map(Some)
    }

    /// Authenticate one transaction identity through its bounded page path;
    /// no transaction payload is needed to compare a durable checkpoint.
    pub(crate) fn record_id(
        &self,
        store: &mut dyn ObjectReader,
        basis_t: u64,
    ) -> Result<Option<ObjectId>, SemanticError> {
        if basis_t == 0 || basis_t > self.basis_t() {
            return Ok(None);
        }
        let page = self.page_for(store, basis_t)?;
        Ok(Some(
            page.entries[(basis_t - 1) as usize % LOG_PAGE_ENTRIES],
        ))
    }

    /// Authenticate the entire previously captured prefix, not only its final
    /// transaction. Entry IDs do not themselves commit to predecessor entries.
    pub fn extends_prefix(
        &self,
        store: &mut dyn ObjectReader,
        previous: &LogRoot,
    ) -> Result<bool, SemanticError> {
        if self.basis_t() < previous.basis_t() {
            return Ok(false);
        }
        let Some(before) = &previous.tail else {
            return Ok(true);
        };
        let current = self.page_for(store, previous.basis_t())?;
        Ok(current.index == before.index
            && current.skips == before.skips
            && current.entries.starts_with(&before.entries)
            && (current.entries.len() != before.entries.len()
                || (current.eidx_frontier == before.eidx_frontier
                    && current.reserved_frontier == before.reserved_frontier)))
    }

    /// Explicit deep validation of the complete navigation structure. Every
    /// skip must name the same sealed page reached through predecessor links;
    /// matching only its page number would admit inconsistent history forks.
    /// This reads every page and entry header, retaining at most one expected
    /// object ID per page. Ordinary selective opens do not perform this walk.
    pub fn validate_structure(&self, store: &mut dyn ObjectReader) -> Result<(), SemanticError> {
        let Some(mut page) = self.tail.clone() else {
            return Ok(());
        };
        let mut id = self.head.expect("a captured log tail has a head");
        let mut expected = std::collections::BTreeMap::new();
        let mut newer_frontiers = None;
        loop {
            if expected
                .remove(&page.index)
                .is_some_and(|wanted| wanted != id)
            {
                return Err(invalid(
                    "storage/log-skip-lineage",
                    "Log navigation links disagree with the predecessor chain",
                ));
            }
            for (level, target) in page.skips.iter().copied().enumerate() {
                let index = page.index - (1_u64 << level);
                if expected
                    .insert(index, target)
                    .is_some_and(|prior| prior != target)
                {
                    return Err(invalid(
                        "storage/log-skip-lineage",
                        "Log navigation links disagree with the predecessor chain",
                    ));
                }
            }
            for (offset, entry) in page.entries.iter().copied().enumerate().rev() {
                let metadata = entry_metadata(&load_block(store, entry)?)?;
                let basis_t = page.index * LOG_PAGE_ENTRIES as u64 + offset as u64 + 1;
                if metadata.basis_t != basis_t
                    || (offset + 1 == page.entries.len()
                        && (metadata.eidx_frontier != page.eidx_frontier
                            || metadata.reserved_frontier != page.reserved_frontier))
                    || newer_frontiers.is_some_and(|(ordinary, reserved)| {
                        metadata.eidx_frontier > ordinary || metadata.reserved_frontier > reserved
                    })
                {
                    return Err(invalid(
                        "storage/log-entry-coordinate",
                        "Log entry coordinate or allocation frontier differs from its history",
                    ));
                }
                newer_frontiers = Some((metadata.eidx_frontier, metadata.reserved_frontier));
            }
            if page.index == 0 {
                return Ok(());
            }
            id = page.skips[0];
            let index = page.index - 1;
            page = Page::load(store, id)?;
            if page.index != index || page.entries.len() != LOG_PAGE_ENTRIES {
                return Err(invalid(
                    "storage/log-skip-coordinate",
                    "Log predecessor does not match its sealed page coordinate",
                ));
            }
        }
    }

    /// Canonical commitment to a prefix, derivable from the current authenticated
    /// page path without fetching an old root or decoding prior datoms. The
    /// bounded page is hashed locally; no object or retention pin is created.
    pub(crate) fn prefix_hash(
        &self,
        store: &mut dyn ObjectReader,
        basis_t: u64,
    ) -> Result<Option<ObjectId>, SemanticError> {
        if basis_t == 0 {
            return Ok(None);
        }
        if basis_t > self.basis_t() {
            return Err(invalid(
                "storage/log-prefix",
                "Prefix is beyond the captured log",
            ));
        }
        let mut page = self.page_for(store, basis_t)?;
        let count = ((basis_t - 1) as usize % LOG_PAGE_ENTRIES) + 1;
        let metadata = entry_metadata(&load_block(store, page.entries[count - 1])?)?;
        if metadata.basis_t != basis_t
            || (count == page.entries.len()
                && (metadata.eidx_frontier != page.eidx_frontier
                    || metadata.reserved_frontier != page.reserved_frontier))
        {
            return Err(invalid(
                "storage/log-entry-coordinate",
                "Prefix metadata differs from its authenticated page",
            ));
        }
        page.entries.truncate(count);
        page.eidx_frontier = metadata.eidx_frontier;
        page.reserved_frontier = metadata.reserved_frontier;
        Ok(Some(crate::sha256(&page.encode()?)))
    }

    /// Inclusive start, exclusive end. The iterator retains only the current
    /// page and logarithmically many forward anchors from the captured tail.
    /// Consumers enforce recent-tier budgets one bounded transaction at a time.
    pub fn range<'a>(
        &self,
        store: &'a mut dyn ObjectReader,
        start_t: u64,
        end_t: u64,
    ) -> Result<LogRange<'a>, SemanticError> {
        Ok(LogRange {
            store,
            traversal: self.traversal(start_t, end_t)?,
        })
    }

    /// Reader-independent state lets an owned snapshot supply authenticated I/O
    /// on each step without a self-referential cursor or a second traversal.
    pub(crate) fn traversal(
        &self,
        start_t: u64,
        end_t: u64,
    ) -> Result<LogTraversal, SemanticError> {
        if start_t == 0 || end_t < start_t {
            return Err(invalid(
                "storage/log-range",
                "Log range needs a positive start no later than its end",
            ));
        }
        Ok(LogTraversal {
            anchors: self.tail.iter().cloned().collect(),
            next: start_t,
            end: end_t.min(self.basis_t() + 1),
            page: None,
            failed: false,
        })
    }

    fn page_for(&self, store: &mut dyn ObjectReader, basis_t: u64) -> Result<Page, SemanticError> {
        let wanted = (basis_t - 1) / LOG_PAGE_ENTRIES as u64;
        let mut page = self
            .tail
            .clone()
            .ok_or_else(|| invalid("storage/log-empty", "An empty log has no pages"))?;
        while page.index > wanted {
            let distance = page.index - wanted;
            let level = (u64::BITS - 1 - distance.leading_zeros()) as usize;
            let expected = page.index - (1_u64 << level);
            page = Page::load(store, page.skips[level])?;
            if page.index != expected || page.entries.len() != LOG_PAGE_ENTRIES {
                return Err(invalid(
                    "storage/log-skip-coordinate",
                    "Log navigation did not reach the authenticated sealed page",
                ));
            }
        }
        Ok(page)
    }
}

pub struct LogRange<'a> {
    store: &'a mut dyn ObjectReader,
    traversal: LogTraversal,
}

pub(crate) struct LogTraversal {
    // Greater-index pages, descending toward the nearest unvisited ancestor.
    // Only the original captured tail may be partial; loaded pages are sealed.
    anchors: Vec<Page>,
    next: u64,
    end: u64,
    page: Option<Page>,
    failed: bool,
}

impl Iterator for LogRange<'_> {
    type Item = Result<LogEntry, SemanticError>;
    fn next(&mut self) -> Option<Self::Item> {
        self.next_record()
            .map(|result| result.map(|record| record.entry))
    }
}

impl LogRange<'_> {
    /// One authenticated transaction, retaining navigation between calls.
    pub fn next_record(&mut self) -> Option<Result<LogRecord, SemanticError>> {
        self.next_record_bounded(usize::MAX)
    }

    /// Reject oversized entries before fetching chunks or decoding datoms.
    pub(crate) fn next_record_bounded(
        &mut self,
        max_bytes: usize,
    ) -> Option<Result<LogRecord, SemanticError>> {
        self.traversal.next_record_bounded(self.store, max_bytes)
    }
}

impl LogTraversal {
    fn next_page(
        &mut self,
        store: &mut dyn ObjectReader,
        wanted: u64,
    ) -> Result<Page, SemanticError> {
        let mut page = self.anchors.pop().ok_or_else(|| {
            invalid(
                "storage/log-skip-coordinate",
                "Log range has no forward anchor for its next page",
            )
        })?;
        if page.index < wanted {
            return Err(invalid(
                "storage/log-skip-coordinate",
                "Log range anchor precedes its next page",
            ));
        }
        while page.index > wanted {
            let distance = page.index - wanted;
            let level = (u64::BITS - 1 - distance.leading_zeros()) as usize;
            let expected = page.index - (1_u64 << level);
            let id = page.skips[level];
            // Keep the parent for forward traversal rather than reseeking
            // from the tail at the next boundary. Each loaded page is either
            // yielded once or retained as one of O(log P) future anchors.
            self.anchors.push(page);
            page = Page::load(store, id)?;
            if page.index != expected || page.entries.len() != LOG_PAGE_ENTRIES {
                return Err(invalid(
                    "storage/log-skip-coordinate",
                    "Log navigation did not reach the authenticated sealed page",
                ));
            }
        }
        Ok(page)
    }

    /// The same streaming read with its actual authenticated object identity
    /// and canonical content byte count for recent-tier admission/accounting.
    pub(crate) fn next_record(
        &mut self,
        store: &mut dyn ObjectReader,
    ) -> Option<Result<LogRecord, SemanticError>> {
        self.next_record_bounded(store, usize::MAX)
    }

    /// The same cursor with per-entry encoded-byte admission before chunk fetch
    /// or datom decoding. Exceeding the bound fuses the cursor like other errors;
    /// checkpoint owners resume with a new range from their last durable basis.
    pub(crate) fn next_record_bounded(
        &mut self,
        store: &mut dyn ObjectReader,
        max_bytes: usize,
    ) -> Option<Result<LogRecord, SemanticError>> {
        if self.failed || self.next >= self.end {
            return None;
        }
        let result = (|| {
            let index = (self.next - 1) / LOG_PAGE_ENTRIES as u64;
            if self.page.as_ref().is_none_or(|p| p.index != index) {
                self.page = Some(self.next_page(store, index)?);
            }
            read_entry_bounded(store, self.page.as_ref().unwrap(), self.next, max_bytes)
        })();
        self.failed = result.is_err();
        self.next += 1;
        Some(result)
    }
}

struct EntryMetadata {
    basis_t: u64,
    eidx_frontier: u64,
    reserved_frontier: u64,
    count: usize,
    bytes: usize,
}

fn entry_metadata(block: &Block) -> Result<EntryMetadata, SemanticError> {
    if block.kind != LOG_ENTRY_KIND || block.payload.len() < ENTRY_HEADER {
        return Err(invalid(
            "storage/log-entry-format",
            "Expected a current log transaction entry",
        ));
    }
    let count = usize::try_from(number(&block.payload, 24))
        .map_err(|_| invalid("storage/log-entry-size", "Datom count is not addressable"))?;
    let bytes = usize::try_from(number(&block.payload, 32)).map_err(|_| {
        invalid(
            "storage/log-entry-size",
            "Transaction size is not addressable",
        )
    })?;
    if bytes > MAX_LOG_TRANSACTION_BYTES
        || count > bytes / 23
        || (bytes <= INLINE_BYTES
            && (!block.links.is_empty() || block.payload.len() != ENTRY_HEADER + bytes))
        || (bytes > INLINE_BYTES
            && (block.payload.len() != ENTRY_HEADER
                || block.links.len() != bytes.div_ceil(CHUNK_BYTES)))
    {
        return Err(invalid(
            "storage/log-entry-shape",
            "Transaction bytes, count, and chunk links disagree",
        ));
    }
    let metadata = EntryMetadata {
        basis_t: number(&block.payload, 0),
        eidx_frontier: number(&block.payload, 8),
        reserved_frontier: number(&block.payload, 16),
        count,
        bytes,
    };
    if metadata.basis_t == 0 || metadata.basis_t > MAX_EIDX {
        return Err(invalid(
            "storage/log-entry-basis",
            "Log transaction basis is outside the current identity range",
        ));
    }
    validate_frontiers(metadata.eidx_frontier, metadata.reserved_frontier)?;
    Ok(metadata)
}

fn read_entry(
    store: &mut dyn ObjectReader,
    page: &Page,
    basis_t: u64,
) -> Result<LogRecord, SemanticError> {
    read_entry_bounded(store, page, basis_t, usize::MAX)
}

fn read_entry_bounded(
    store: &mut dyn ObjectReader,
    page: &Page,
    basis_t: u64,
    max_bytes: usize,
) -> Result<LogRecord, SemanticError> {
    let offset = (basis_t - 1) as usize % LOG_PAGE_ENTRIES;
    let block = load_block(store, page.entries[offset])?;
    let metadata = entry_metadata(&block)?;
    if metadata.basis_t != basis_t
        || (basis_t == page.basis_t()
            && (metadata.eidx_frontier != page.eidx_frontier
                || metadata.reserved_frontier != page.reserved_frontier))
    {
        return Err(invalid(
            "storage/log-entry-coordinate",
            "Log entry differs from its page coordinate",
        ));
    }
    if metadata.bytes.saturating_add(ENTRY_HEADER) > max_bytes {
        return Err(SemanticError::new(
            ErrorCategory::Busy,
            "storage/log-read-limit",
            "Transaction exceeds the requested encoded read limit",
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.bytes);
    if block.links.is_empty() {
        bytes.extend_from_slice(&block.payload[ENTRY_HEADER..]);
    } else {
        for id in block.links {
            let chunk = load_block(store, id)?;
            let expected = CHUNK_BYTES.min(metadata.bytes - bytes.len());
            if chunk.kind != LOG_CHUNK_KIND
                || !chunk.links.is_empty()
                || chunk.payload.len() != expected
            {
                return Err(invalid(
                    "storage/log-chunk-shape",
                    "Transaction chunk differs from its authenticated position",
                ));
            }
            bytes.extend_from_slice(&chunk.payload);
        }
    }
    let tx_data = decode_canonical_datoms(&bytes, metadata.count)?;
    let entry = LogEntry {
        basis_t,
        eidx_frontier: metadata.eidx_frontier,
        reserved_frontier: metadata.reserved_frontier,
        tx_data,
    };
    if entry.encode_datoms()? != bytes {
        return Err(invalid(
            "storage/log-noncanonical",
            "Log transaction datoms are not canonically encoded",
        ));
    }
    Ok(LogRecord {
        id: page.entries[offset],
        encoded_bytes: (ENTRY_HEADER + metadata.bytes) as u64,
        entry,
    })
}

fn load_block(store: &mut dyn ObjectReader, id: ObjectId) -> Result<Block, SemanticError> {
    let bytes = store.read_object(id)?;
    Block::decode(&id, &bytes)
}

/// Discover native program links once when a newly owned log entry enters the
/// incremental ownership graph. Chunks themselves retain no independent value
/// interpretation; their concatenation is the authenticated canonical datom body.
pub(crate) fn entry_program_links(
    store: &mut dyn ObjectReader,
    block: &Block,
) -> Result<Vec<ObjectId>, SemanticError> {
    let metadata = entry_metadata(block)?;
    let bytes = if block.links.is_empty() {
        block.payload[ENTRY_HEADER..].to_vec()
    } else {
        let mut content = Vec::with_capacity(metadata.bytes);
        for id in &block.links {
            let chunk = load_block(store, *id)?;
            let expected = CHUNK_BYTES.min(metadata.bytes.saturating_sub(content.len()));
            if chunk.kind != LOG_CHUNK_KIND
                || !chunk.links.is_empty()
                || chunk.payload.len() != expected
            {
                return Err(invalid(
                    "storage/log-chunk-shape",
                    "Invalid owned transaction chunk",
                ));
            }
            content.extend_from_slice(&chunk.payload);
        }
        content
    };
    let mut programs = std::collections::BTreeSet::new();
    for datom in decode_canonical_datoms(&bytes, metadata.count)? {
        crate::program_bindings::collect_program_hashes(&datom.value, &mut programs);
    }
    Ok(programs.into_iter().collect())
}

fn number(bytes: &[u8], offset: usize) -> u64 {
    u64::from_be_bytes(bytes[offset..offset + 8].try_into().unwrap())
}
fn validate_frontiers(ordinary: u64, reserved: u64) -> Result<(), SemanticError> {
    crate::reserved_allocation::ReservedAllocation::from_frontier(reserved, ordinary).map(|_| ())
}
fn invalid(code: &'static str, message: &'static str) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, code, message)
}
