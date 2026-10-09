//! Authenticated fulltext attachments over opaque immutable objects. The
//! shared analyzer, record delta, spill sorter and path-copy page editor own
//! search semantics; this module supplies object I/O and exact source binding.
use super::{BlockSnapshot, IndexDescriptor, ObjectId, ObjectWriter, root::Block};
use crate::fulltext::store::incremental::{self, Pages};
use crate::fulltext::store::{self, Child, FulltextMutation, Page, PageSummary};
use crate::index::cursor::{DurableTreeCursor, DurableTreeSource, LoadedDirectory, LoadedLeaf};
use crate::index::tree::{ChildRef, RootNode, TreeNode, TreeReadStats};
use crate::{
    ErrorCategory, FulltextBuildLimits, FulltextBuildStats, FulltextCursor, FulltextProjection,
    FulltextReadLimits, FulltextReadStats, IndexOrder, IndexPrefix, NativeFulltextReader, Schema,
    SemanticError, sha256,
};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

pub const FULLTEXT_ATTACHMENT_KIND: u16 = 30;
pub const FULLTEXT_PAGE_KIND: u16 = 31;
pub const FULLTEXT_CHUNKED_PAGE_KIND: u16 = 32;
pub const FULLTEXT_CHUNK_KIND: u16 = 33;
const CHUNK_BYTES: usize = 4 * 1024 * 1024;

/// Build or incrementally advance an attachment for an exact indexed source.
/// The caller retains both snapshots and protects the source and destination
/// until publication. Live stores use write protection; repository writers
/// write only to their separately published destination. A buffering ObjectWriter
/// must be flushed before independent reads/publication; returned block/byte
/// statistics count accepted page uploads, not that durability barrier or SQL I/O.
pub fn build(
    store: &mut dyn ObjectWriter,
    source: &BlockSnapshot,
    predecessor: Option<&BlockSnapshot>,
    limits: &FulltextBuildLimits,
) -> Result<Option<(ObjectId, FulltextBuildStats)>, SemanticError> {
    limits.validate()?;
    if source.index_descriptor().basis != source.basis_t() {
        return Err(fault(
            "fulltext/noncanonical-delta-source",
            "Search build requires an exact indexed source without a recent tail",
        ));
    }
    let predecessor =
        predecessor.filter(|before| before.index_descriptor().basis == before.basis_t());
    build_for_descriptor(
        store,
        source.index_descriptor(),
        &source.endpoint_metadata().schema,
        predecessor.map(|before| {
            (
                before.index_descriptor(),
                before.base_metadata().schema.as_ref(),
            )
        }),
        limits,
    )
}

/// Build against a prepared immutable descriptor before it is published. All
/// cold I/O uses the job's independent store; captured schemas are pure values.
/// The caller retains/protects the source objects through eventual adoption.
pub fn build_for_descriptor(
    store: &mut dyn ObjectWriter,
    descriptor: &IndexDescriptor,
    schema: &Schema,
    predecessor: Option<(&IndexDescriptor, &Schema)>,
    limits: &FulltextBuildLimits,
) -> Result<Option<(ObjectId, FulltextBuildStats)>, SemanticError> {
    build_for_descriptor_with_control(store, descriptor, schema, predecessor, limits, &mut || {
        Ok(())
    })
}

/// Cancellation-aware variant used by the asynchronous index worker. Control
/// is checked at each source/page I/O and each streamed record, including sort.
pub fn build_for_descriptor_with_control(
    store: &mut dyn ObjectWriter,
    descriptor: &IndexDescriptor,
    schema: &Schema,
    predecessor: Option<(&IndexDescriptor, &Schema)>,
    limits: &FulltextBuildLimits,
    control: &mut dyn FnMut() -> Result<(), SemanticError>,
) -> Result<Option<(ObjectId, FulltextBuildStats)>, SemanticError> {
    control()?;
    limits.validate()?;
    if !schema.attributes().any(|a| a.fulltext) {
        return Ok(None);
    }
    let mut descriptor = descriptor.clone();
    descriptor.fulltext = None;
    let source_id = store.put_object(&descriptor.encode()?)?;
    let pages = HistoryPages::new(store, control);
    let mut stats = FulltextBuildStats::default();
    let summary = if let Some((before, before_schema)) = predecessor.filter(|(before, _)| {
        before.fulltext.is_some()
            && before.identity == descriptor.identity
            && before.generation == descriptor.generation
            && before.basis <= descriptor.basis
    }) {
        let prior = load_projection(before, &mut |id| pages.read(id))?;
        let before_root = pages.root(&before.trees[6])?;
        let after_root = pages.root(&descriptor.trees[6])?;
        let before_hash = before.trees[6].root_hash;
        let after_hash = descriptor.trees[6].root_hash;
        let mut difference = crate::fulltext::HistoryDifference::from_roots(
            pages.clone(),
            before_root,
            pages.clone(),
            after_root,
            before_hash == after_hash,
        );
        let new_attributes = schema
            .attributes()
            .filter(|a| a.fulltext && before_schema.attribute(a.id).is_err())
            .map(|a| a.id)
            .collect();
        let (empty, empty_work) =
            crate::fulltext::empty_corpus_from(before_schema, prior.record_count, |key| {
                pages.lookup(&prior, key)
            })?;
        let mut first_text = None;
        if empty.is_some() {
            for change in difference.by_ref() {
                let (datom, insert) = change?;
                if datom.added && schema.attribute(datom.attribute)?.fulltext {
                    if !insert {
                        return Err(fault(
                            "fulltext/empty-predecessor-document",
                            "Empty predecessor cannot remove an indexed document",
                        ));
                    }
                    first_text = Some((datom, insert));
                    break;
                }
            }
        }
        let bulk = first_text.is_some();
        let mut records = crate::fulltext::DeltaRecords::from_reader(
            first_text.into_iter().map(Ok).chain(&mut difference),
            schema,
            |key| pages.lookup(&prior, key),
            new_attributes,
        );
        let mutations = records.by_ref().map(|r| {
            let (record, insert) = r?;
            pages.poll()?;
            Ok(FulltextMutation {
                key: record.key,
                value: insert.then_some(record.value),
            })
        });
        let sorted = incremental::sort_mutations(mutations, limits, &mut stats)?;
        stats.documents_added = records.stats.documents_added;
        stats.documents_removed = records.stats.documents_removed;
        stats.tokenized_bytes = records.stats.tokenized_bytes;
        stats.statistics_reads = records.stats.statistics_reads + empty_work.statistics_reads;
        stats.statistics_read_bytes =
            records.stats.statistics_read_bytes + empty_work.statistics_read_bytes;
        drop(records);
        stats.source_references_examined = difference.references_examined;
        stats.source_datoms_examined = difference.datoms_examined;
        let summary = if bulk {
            stats.empty_corpus_bulk_builds = 1;
            let mut zero = empty.expect("bulk path proved empty corpus");
            let merged =
                incremental::EmptyCorpusMerge::new(&mut zero, sorted, prior.record_count, limits);
            store::build_pages(merged, limits, &mut |page| pages.save(page, &mut stats))?
        } else {
            incremental::edit_pages(
                ObjectPages {
                    pages: pages.clone(),
                    stats: &mut stats,
                },
                &prior,
                sorted,
                limits,
            )?
        };
        stats.reused_projections = u64::from(summary.root == prior.root_hash);
        summary
    } else {
        let root = pages.root(&descriptor.trees[7])?;
        let source = pages.clone();
        let mut records = crate::fulltext::records_from(schema, move |attribute| {
            let mut cursor = DurableTreeCursor::new_prefix(
                source.clone(),
                Arc::clone(&root),
                true,
                IndexPrefix::Aevt {
                    attribute,
                    entity: None,
                    value: None,
                },
            );
            Ok(Box::new(std::iter::from_fn(move || {
                cursor.next_datom().transpose()
            })))
        });
        let sorted = store::sorted_records(
            records.by_ref().map(|record| {
                let record = record?;
                pages.poll()?;
                Ok(record)
            }),
            limits,
            &mut stats,
        )?;
        stats.documents_added = records.stats.documents_added;
        stats.tokenized_bytes = records.stats.tokenized_bytes;
        stats.source_datoms_examined = records.datoms_examined;
        drop(records);
        store::build_pages(sorted, limits, &mut |page| pages.save(page, &mut stats))?
    };
    stats.records = summary.records;
    let reads = *pages.reads.borrow();
    stats.source_nodes_read = reads.root_reads + reads.directory_reads + reads.leaf_reads;
    stats.source_node_bytes = reads.decoded_bytes;
    let projection = projection(source_id, &descriptor, summary);
    pages.poll()?;
    let mut payload = descriptor.identity.to_vec();
    payload.extend_from_slice(&projection.encode());
    let attachment = pages.store.borrow_mut().put_object(
        &Block {
            kind: FULLTEXT_ATTACHMENT_KIND,
            links: vec![source_id, projection.root_hash],
            payload,
        }
        .encode()?,
    )?;
    Ok(Some((attachment, stats)))
}

fn projection(
    source: ObjectId,
    descriptor: &IndexDescriptor,
    pages: PageSummary,
) -> FulltextProjection {
    FulltextProjection {
        source_manifest: source,
        source_basis_t: descriptor.basis,
        source_generation: descriptor.generation,
        analyzer_version: crate::fulltext::analysis::ANALYZER_VERSION,
        root_hash: pages.root,
        record_count: pages.records,
        encoded_bytes: pages.bytes,
        block_count: pages.blocks,
    }
}

pub(crate) fn reader(snapshot: &BlockSnapshot) -> Result<NativeFulltextReader, SemanticError> {
    let descriptor = snapshot.index_descriptor();
    let attachment = attachment_id(descriptor)?;
    let cache = snapshot.fulltext_cache();
    let projection = if let Some(projection) = cache.header(attachment) {
        projection
    } else {
        let projection = load_projection(descriptor, &mut |id| snapshot.read_object(id))?;
        cache.insert_header_at(attachment, projection.clone());
        projection
    };
    let mut source = descriptor.clone();
    source.fulltext = None;
    validate_projection(&projection, sha256(&source.encode()?), descriptor)?;
    Ok(NativeFulltextReader::from_block(
        snapshot.clone(),
        projection,
    ))
}

fn attachment_id(descriptor: &IndexDescriptor) -> Result<ObjectId, SemanticError> {
    descriptor.fulltext.ok_or_else(|| {
        SemanticError::new(
            ErrorCategory::Unavailable,
            "fulltext/index-unavailable",
            "Captured index has no fulltext attachment",
        )
    })
}

pub(crate) fn load_projection(
    descriptor: &IndexDescriptor,
    read: &mut impl FnMut(ObjectId) -> Result<Vec<u8>, SemanticError>,
) -> Result<FulltextProjection, SemanticError> {
    let attachment = attachment_id(descriptor)?;
    let mut source = descriptor.clone();
    source.fulltext = None;
    let expected_source = sha256(&source.encode()?);
    let bytes = read(attachment)?;
    if bytes.len() != 20 + 2 * 32 + 16 + 116 {
        return Err(fault(
            "fulltext/attachment-format",
            "Invalid search attachment size",
        ));
    }
    let block = Block::decode(&attachment, &bytes)?;
    if block.kind != FULLTEXT_ATTACHMENT_KIND
        || block.links.len() != 2
        || block.payload.len() != 132
        || block.payload[..16] != descriptor.identity
    {
        return Err(fault(
            "fulltext/attachment-format",
            "Search attachment identity/kind/links differ",
        ));
    }
    let projection = FulltextProjection::decode(&block.payload[16..])?;
    if block.links != [projection.source_manifest, projection.root_hash] {
        return Err(fault(
            "fulltext/source-binding",
            "Search attachment links differ from its header",
        ));
    }
    validate_projection(&projection, expected_source, descriptor)?;
    Ok(projection)
}

fn validate_projection(
    p: &FulltextProjection,
    source: ObjectId,
    descriptor: &IndexDescriptor,
) -> Result<(), SemanticError> {
    if p.source_manifest != source
        || p.source_basis_t != descriptor.basis
        || p.source_generation != descriptor.generation
    {
        return Err(fault(
            "fulltext/source-binding",
            "Search attachment does not describe its exact index source",
        ));
    }
    if p.analyzer_version != crate::fulltext::analysis::ANALYZER_VERSION {
        return Err(SemanticError::new(
            ErrorCategory::Unavailable,
            "fulltext/analyzer-version",
            "Search attachment uses a different analyzer",
        ));
    }
    Ok(())
}

pub(crate) fn prefix(
    snapshot: BlockSnapshot,
    projection: &FulltextProjection,
    prefix: &[u8],
    limits: FulltextReadLimits,
) -> Result<FulltextCursor, SemanticError> {
    let source = projection.source_manifest;
    FulltextCursor::new(
        projection,
        prefix,
        limits,
        Box::new(move |id, stats| {
            let cache = snapshot.fulltext_cache();
            if let Some(page) = cache.get(source, id) {
                stats.cache_hits += 1;
                return Ok(page);
            }
            let (page, blocks, bytes) = read_page(id, &mut |id| snapshot.read_object(id))?;
            stats.blocks_read += blocks;
            stats.block_bytes += bytes;
            let page = Arc::new(page);
            cache.insert(source, id, Arc::clone(&page), bytes as usize);
            Ok(page)
        }),
    )
}

fn page_links(page: &Page) -> Vec<ObjectId> {
    match page {
        Page::Leaf(_) => vec![],
        Page::Branch(children) => children.iter().map(|c| c.hash).collect(),
    }
}
fn save_page(
    store: &mut dyn ObjectWriter,
    page: &Page,
    stats: &mut FulltextBuildStats,
) -> Result<Child, SemanticError> {
    let bytes = page.encode()?;
    let mut links = page_links(page);
    let inline = bytes.len() + 20 + links.len() * 32 <= super::root::MAX_BLOCK_BYTES;
    let block = if inline {
        Block {
            kind: FULLTEXT_PAGE_KIND,
            links,
            payload: bytes.clone(),
        }
    } else {
        let mut payload = (bytes.len() as u64).to_be_bytes().to_vec();
        payload.extend_from_slice(&(links.len() as u32).to_be_bytes());
        for chunk in bytes.chunks(CHUNK_BYTES) {
            links.push(
                store.put_object(
                    &Block {
                        kind: FULLTEXT_CHUNK_KIND,
                        links: vec![],
                        payload: chunk.to_vec(),
                    }
                    .encode()?,
                )?,
            );
        }
        Block {
            kind: FULLTEXT_CHUNKED_PAGE_KIND,
            links,
            payload,
        }
    };
    let id = store.put_object(&block.encode()?)?;
    stats.blocks += 1;
    stats.encoded_bytes += bytes.len() as u64;
    stats.peak_buffer_bytes = stats.peak_buffer_bytes.max(
        page.retained_bytes()
            .saturating_add(bytes.len().saturating_mul(2)),
    );
    Ok(page.descriptor(id))
}

pub(crate) fn read_page(
    id: ObjectId,
    read: &mut impl FnMut(ObjectId) -> Result<Vec<u8>, SemanticError>,
) -> Result<(Page, u64, u64), SemanticError> {
    let bytes = read(id)?;
    let mut transferred = bytes.len() as u64;
    let mut blocks = 1;
    let block = Block::decode(&id, &bytes)?;
    let (content, links) = match block.kind {
        FULLTEXT_PAGE_KIND => (block.payload, block.links),
        FULLTEXT_CHUNKED_PAGE_KIND if block.payload.len() == 12 => {
            let length =
                usize::try_from(u64::from_be_bytes(block.payload[..8].try_into().unwrap()))
                    .map_err(|_| fault("fulltext/page-size", "Page length is not addressable"))?;
            let children = u32::from_be_bytes(block.payload[8..].try_into().unwrap()) as usize;
            if length > store::MAX_PAGE
                || children > 128
                || block.links.len() != children + length.div_ceil(CHUNK_BYTES)
                || length + 20 + children * 32 <= super::root::MAX_BLOCK_BYTES
            {
                return Err(fault(
                    "fulltext/page-size",
                    "Invalid chunked page length/links",
                ));
            }
            let mut content = Vec::with_capacity(length);
            for chunk in &block.links[children..] {
                let bytes = read(*chunk)?;
                transferred += bytes.len() as u64;
                blocks += 1;
                let chunk = Block::decode(chunk, &bytes)?;
                if chunk.kind != FULLTEXT_CHUNK_KIND
                    || !chunk.links.is_empty()
                    || chunk.payload.len() != (length - content.len()).min(CHUNK_BYTES)
                {
                    return Err(fault("fulltext/page-chunk", "Invalid fulltext page chunk"));
                }
                content.extend_from_slice(&chunk.payload);
            }
            (content, block.links[..children].to_vec())
        }
        _ => {
            return Err(fault(
                "fulltext/page-format",
                "Expected an opaque fulltext page",
            ));
        }
    };
    // The envelope/chunks authenticated these exact bytes. The current raw
    // page codec additionally validates ordering, range/counts and sizes.
    let page = Page::decode(sha256(&content), &content)?;
    if links != page_links(&page) {
        return Err(fault(
            "fulltext/page-links",
            "Fulltext page reachability links disagree",
        ));
    }
    Ok((page, blocks, transferred))
}

struct ObjectPages<'a, 'b> {
    pages: HistoryPages<'a>,
    stats: &'b mut FulltextBuildStats,
}
impl Pages for ObjectPages<'_, '_> {
    fn load(&mut self, id: ObjectId) -> Result<Page, SemanticError> {
        let (page, blocks, bytes) = read_page(id, &mut |id| self.pages.read(id))?;
        self.stats.blocks_read += blocks;
        self.stats.block_bytes_read += bytes;
        Ok(page)
    }
    fn save(&mut self, page: &Page) -> Result<Child, SemanticError> {
        self.pages.save(page, self.stats)
    }
    fn peak(&mut self, bytes: usize) {
        self.stats.peak_buffer_bytes = self.stats.peak_buffer_bytes.max(bytes);
    }
}

#[derive(Clone)]
struct HistoryPages<'a> {
    store: Rc<RefCell<&'a mut dyn ObjectWriter>>,
    control: Rc<RefCell<&'a mut dyn FnMut() -> Result<(), SemanticError>>>,
    reads: Rc<RefCell<TreeReadStats>>,
}
impl<'a> HistoryPages<'a> {
    fn new(
        store: &'a mut dyn ObjectWriter,
        control: &'a mut dyn FnMut() -> Result<(), SemanticError>,
    ) -> Self {
        Self {
            store: Rc::new(RefCell::new(store)),
            control: Rc::new(RefCell::new(control)),
            reads: Rc::new(RefCell::new(TreeReadStats::default())),
        }
    }
    fn read(&self, id: ObjectId) -> Result<Vec<u8>, SemanticError> {
        self.poll()?;
        self.store.borrow_mut().read_object(id)
    }
    fn poll(&self) -> Result<(), SemanticError> {
        (self.control.borrow_mut())()
    }
    fn save(&self, page: &Page, stats: &mut FulltextBuildStats) -> Result<Child, SemanticError> {
        self.poll()?;
        save_page(&mut **self.store.borrow_mut(), page, stats)
    }
    fn lookup(
        &self,
        projection: &FulltextProjection,
        key: &[u8],
    ) -> Result<(Option<store::FulltextRecord>, FulltextReadStats), SemanticError> {
        store::lookup_record(projection, key, |id, stats| {
            let (page, blocks, bytes) = read_page(id, &mut |id| self.read(id))?;
            stats.blocks_read += blocks;
            stats.block_bytes += bytes;
            Ok(page)
        })
    }
    fn root(
        &self,
        descriptor: &crate::index::tree::TreeDescriptor,
    ) -> Result<Arc<RootNode>, SemanticError> {
        let node = self.load_node(descriptor.root_hash, &mut TreeReadStats::default())?;
        let TreeNode::Root(root) = node.as_ref() else {
            return Err(fault(
                "fulltext/source-root",
                "History source is not a tree root",
            ));
        };
        if root.order != descriptor.order
            || root.history != descriptor.history
            || root.count != descriptor.count
        {
            return Err(fault(
                "fulltext/source-root",
                "History source differs from its descriptor",
            ));
        }
        Ok(Arc::new(root.clone()))
    }
}
impl DurableTreeSource for HistoryPages<'_> {
    fn load_node(
        &self,
        hash: ObjectId,
        stats: &mut TreeReadStats,
    ) -> Result<Arc<TreeNode>, SemanticError> {
        let bytes = self.read(hash)?;
        let node = crate::index::tree::decode_tree_node(&hash, &bytes)?;
        let mut total = self.reads.borrow_mut();
        for reads in [stats, &mut *total] {
            reads.cache_misses += 1;
            reads.decoded_bytes += bytes.len() as u64;
            match &node {
                TreeNode::Root(_) => reads.root_reads += 1,
                TreeNode::Directory(_) => reads.directory_reads += 1,
                TreeNode::Leaf(_) => reads.leaf_reads += 1,
            }
        }
        Ok(Arc::new(node))
    }
}
impl crate::fulltext::HistorySource for HistoryPages<'_> {
    fn directory(
        &self,
        reference: &ChildRef,
        stats: &mut TreeReadStats,
    ) -> Result<LoadedDirectory, SemanticError> {
        DurableTreeSource::load_directory(self, reference, IndexOrder::Eavt, true, stats)
    }
    fn leaf(
        &self,
        reference: &ChildRef,
        stats: &mut TreeReadStats,
    ) -> Result<LoadedLeaf, SemanticError> {
        DurableTreeSource::load_leaf(self, reference, IndexOrder::Eavt, true, stats)
    }
}
fn fault(code: &'static str, message: &str) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, code, message)
}
