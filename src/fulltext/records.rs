//! Logical payloads for the generic immutable fulltext key/value tree. Posting
//! construction streams tokens into the external sorter; duplicate occurrence
//! keys coalesce there. Text is stored once per historical assertion, not once
//! per posting. Candidate TF/phrase positions are derived from that document.
use super::analysis::next_token;
use super::store::FulltextRecord;
use crate::{Datom, ErrorCategory, FulltextReadStats, Schema, SemanticError, Value, sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub(super) type DocId = [u8; 48];
pub(super) fn doc_id(datom: &Datom) -> Result<DocId, SemanticError> {
    let Value::String(text) = &datom.value else {
        return Err(corrupt("document is not a string"));
    };
    let mut id = [0; 48];
    id[..8].copy_from_slice(&datom.entity.to_be_bytes());
    id[8..16].copy_from_slice(&datom.tx.to_be_bytes());
    id[16..].copy_from_slice(&sha256(text.as_bytes()));
    Ok(id)
}
pub(super) fn doc_key(attribute: u32, id: &DocId) -> Vec<u8> {
    let mut key = attribute.to_be_bytes().to_vec();
    key.push(0);
    key.extend_from_slice(id);
    key
}
pub(super) fn term_prefix(attribute: u32, term: &str, exact: bool) -> Vec<u8> {
    let mut key = attribute.to_be_bytes().to_vec();
    key.push(1);
    key.extend_from_slice(term.as_bytes());
    if exact {
        key.push(0);
    }
    key
}
pub(super) fn stats_key(attribute: u32) -> Vec<u8> {
    let mut key = attribute.to_be_bytes().to_vec();
    key.push(2);
    key
}
pub(super) fn posting(
    record: &FulltextRecord,
    attribute: u32,
) -> Result<(String, DocId, u32), SemanticError> {
    let key = &record.key;
    if key.len() < 55
        || key[..4] != attribute.to_be_bytes()
        || key[4] != 1
        || key[key.len() - 49] != 0
        || record.value.len() != 4
    {
        return Err(corrupt("invalid posting record"));
    }
    let term =
        std::str::from_utf8(&key[5..key.len() - 49]).map_err(|_| corrupt("invalid term UTF-8"))?;
    if term.is_empty() || term.contains('\0') {
        return Err(corrupt("invalid term key"));
    }
    Ok((
        term.to_owned(),
        key[key.len() - 48..].try_into().unwrap(),
        u32::from_be_bytes(record.value[..].try_into().unwrap()),
    ))
}
pub(super) fn document(
    record: FulltextRecord,
    attribute: u32,
    id: &DocId,
) -> Result<(Datom, u32), SemanticError> {
    if record.key != doc_key(attribute, id) || record.value.len() < 12 {
        return Err(corrupt("invalid document record"));
    }
    let length = u32::from_be_bytes(record.value[..4].try_into().unwrap());
    let source = u64::from_be_bytes(record.value[4..12].try_into().unwrap());
    let text =
        std::str::from_utf8(&record.value[12..]).map_err(|_| corrupt("invalid document UTF-8"))?;
    if sha256(text.as_bytes()) != id[16..] {
        return Err(corrupt("document text identity mismatch"));
    }
    let entity = u64::from_be_bytes(id[..8].try_into().unwrap());
    let tx = u64::from_be_bytes(id[8..16].try_into().unwrap());
    crate::eid_to_eidx(entity)?;
    crate::model::identity::validate_permanent_eid(source)?;
    crate::tx_to_t(tx)?;
    Ok((
        Datom {
            entity,
            attribute,
            tx,
            source,
            value: Value::String(text.into()),
            added: true,
        },
        length,
    ))
}
pub(super) fn statistics(
    record: &FulltextRecord,
    attribute: u32,
) -> Result<(u64, u64), SemanticError> {
    if record.key != stats_key(attribute) || record.value.len() != 16 {
        return Err(corrupt("invalid corpus statistics"));
    }
    Ok((
        u64::from_be_bytes(record.value[..8].try_into().unwrap()),
        u64::from_be_bytes(record.value[8..].try_into().unwrap()),
    ))
}
fn corrupt(message: &str) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, "fulltext/invalid-record", message)
}

struct PendingDocument {
    attribute: u32,
    id: DocId,
    text: String,
    length: u32,
    offset: usize,
    position: u32,
}
type RecordDatoms<'a> = Box<dyn Iterator<Item = Result<Datom, SemanticError>> + 'a>;
type ReadRecord<'a> = Box<
    dyn FnMut(&[u8]) -> Result<(Option<FulltextRecord>, FulltextReadStats), SemanticError> + 'a,
>;

pub(crate) struct Records<'a> {
    cursor: Box<dyn FnMut(u32) -> Result<RecordDatoms<'a>, SemanticError> + 'a>,
    attributes: VecDeque<u32>,
    active: Option<(u32, RecordDatoms<'a>)>,
    document: Option<PendingDocument>,
    documents: u64,
    total_length: u64,
    failed: bool,
    pub stats: DeltaRecordStats,
    pub datoms_examined: u64,
}

impl Records<'_> {
    fn next_record(&mut self) -> Result<Option<FulltextRecord>, SemanticError> {
        loop {
            if let Some(doc) = &mut self.document {
                if let Some(token) = next_token(&doc.text, &mut doc.offset, &mut doc.position) {
                    let mut key = term_prefix(doc.attribute, &token.term, true);
                    key.extend_from_slice(&doc.id);
                    return Ok(Some(FulltextRecord {
                        key,
                        value: doc.length.to_be_bytes().to_vec(),
                    }));
                }
                self.document = None;
            }
            if let Some((attribute, cursor)) = &mut self.active {
                if let Some(datom) = cursor.next() {
                    let datom = datom?;
                    self.datoms_examined += 1;
                    if !datom.added {
                        continue;
                    }
                    let id = doc_id(&datom)?;
                    let Value::String(text) = datom.value else {
                        unreachable!()
                    };
                    let (mut offset, mut position) = (0, 0);
                    let length =
                        std::iter::from_fn(|| next_token(&text, &mut offset, &mut position))
                            .count();
                    let length = u32::try_from(length)
                        .map_err(|_| corrupt("document token count overflow"))?;
                    self.stats.documents_added += 1;
                    self.stats.tokenized_bytes = self
                        .stats
                        .tokenized_bytes
                        .saturating_add((text.len() as u64).saturating_mul(2));
                    self.documents = self
                        .documents
                        .checked_add(1)
                        .ok_or_else(|| corrupt("corpus document count overflow"))?;
                    self.total_length = self
                        .total_length
                        .checked_add(u64::from(length))
                        .ok_or_else(|| corrupt("corpus token count overflow"))?;
                    let mut value = length.to_be_bytes().to_vec();
                    value.extend_from_slice(&datom.source.to_be_bytes());
                    value.extend_from_slice(text.as_bytes());
                    let key = doc_key(*attribute, &id);
                    self.document = Some(PendingDocument {
                        attribute: *attribute,
                        id,
                        text,
                        length,
                        offset: 0,
                        position: 0,
                    });
                    return Ok(Some(FulltextRecord { key, value }));
                }
                let key = stats_key(*attribute);
                let mut value = self.documents.to_be_bytes().to_vec();
                value.extend_from_slice(&self.total_length.to_be_bytes());
                self.active = None;
                self.documents = 0;
                self.total_length = 0;
                return Ok(Some(FulltextRecord { key, value }));
            }
            let Some(attribute) = self.attributes.pop_front() else {
                return Ok(None);
            };
            self.active = Some((attribute, (self.cursor)(attribute)?));
        }
    }
}
impl Iterator for Records<'_> {
    type Item = Result<FulltextRecord, SemanticError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        match self.next_record() {
            Ok(Some(record)) => Some(Ok(record)),
            Ok(None) => None,
            Err(error) => {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}

pub(crate) fn records_from<'a>(
    schema: &Schema,
    cursor: impl FnMut(u32) -> Result<RecordDatoms<'a>, SemanticError> + 'a,
) -> Records<'a> {
    Records {
        cursor: Box::new(cursor),
        attributes: schema
            .attributes()
            .filter(|a| a.fulltext)
            .map(|a| a.id)
            .collect(),
        active: None,
        document: None,
        documents: 0,
        total_length: 0,
        failed: false,
        stats: DeltaRecordStats::default(),
        datoms_examined: 0,
    }
}

/// Only logical assertion changes reach the analyzer. Retractions are history
/// facts, not document deletion requests; physical noHistory omissions arrive
/// as removal of the corresponding assertion from the canonical tree diff.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct DeltaRecordStats {
    pub documents_added: u64,
    pub documents_removed: u64,
    /// Both streaming analyzer passes (length, then postings) are counted.
    pub tokenized_bytes: u64,
    pub statistics_reads: u64,
    pub statistics_read_bytes: u64,
}

/// Re-emit only the exact zero-stat bytes authenticated by the constructor.
/// The schema is immutable, so no old records need to be collected or reread.
pub(crate) struct EmptyFulltextCorpus<'a> {
    attributes: Box<dyn Iterator<Item = u32> + 'a>,
}

impl Iterator for EmptyFulltextCorpus<'_> {
    type Item = Result<FulltextRecord, SemanticError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.attributes.next().map(|attribute| {
            Ok(FulltextRecord {
                key: stats_key(attribute),
                value: vec![0; 16],
            })
        })
    }
}

pub(crate) fn empty_corpus_from<'a>(
    schema: &'a Schema,
    record_count: u64,
    read: impl FnMut(&[u8]) -> Result<(Option<FulltextRecord>, FulltextReadStats), SemanticError>,
) -> Result<(Option<EmptyFulltextCorpus<'a>>, DeltaRecordStats), SemanticError> {
    let (empty, work) = verified_empty_corpus(schema, record_count, read)?;
    Ok((
        empty.then(|| EmptyFulltextCorpus {
            attributes: Box::new(schema.attributes().filter(|a| a.fulltext).map(|a| a.id)),
        }),
        work,
    ))
}

fn verified_empty_corpus(
    schema: &Schema,
    record_count: u64,
    mut read: impl FnMut(&[u8]) -> Result<(Option<FulltextRecord>, FulltextReadStats), SemanticError>,
) -> Result<(bool, DeltaRecordStats), SemanticError> {
    let attribute_count = schema.attributes().filter(|a| a.fulltext).count() as u64;
    let mut work = DeltaRecordStats::default();
    // A document always has its own record, including empty/stop-word-only
    // strings that have no postings. Counts alone are a rejection fast path,
    // never proof of emptiness: every expected statistic must be authenticated.
    if record_count > attribute_count {
        return Ok((false, work));
    }
    if record_count < attribute_count {
        return Err(corrupt(
            "predecessor has fewer records than fulltext attributes",
        ));
    }
    for attribute in schema.attributes().filter(|a| a.fulltext).map(|a| a.id) {
        let (record, reads) = read(&stats_key(attribute))?;
        work.statistics_reads += 1;
        work.statistics_read_bytes += reads.block_bytes;
        let record = record.ok_or_else(|| corrupt("empty predecessor lacks corpus statistics"))?;
        if statistics(&record, attribute)? != (0, 0) {
            return Err(corrupt(
                "statistics-only predecessor reports a nonempty corpus",
            ));
        }
    }
    Ok((true, work))
}

pub(crate) struct DeltaRecords<'a> {
    changes: Box<dyn Iterator<Item = Result<(Datom, bool), SemanticError>> + 'a>,
    schema: &'a Schema,
    read: ReadRecord<'a>,
    new_attributes: BTreeSet<u32>,
    counts: BTreeMap<u32, (u64, u64)>,
    document: Option<(PendingDocument, bool)>,
    finished_changes: bool,
    failed: bool,
    pub stats: DeltaRecordStats,
}

impl<'a> DeltaRecords<'a> {
    pub(crate) fn from_reader(
        changes: impl Iterator<Item = Result<(Datom, bool), SemanticError>> + 'a,
        schema: &'a Schema,
        read: impl FnMut(&[u8]) -> Result<(Option<FulltextRecord>, FulltextReadStats), SemanticError>
        + 'a,
        new_attributes: BTreeSet<u32>,
    ) -> Self {
        Self {
            changes: Box::new(changes),
            schema,
            read: Box::new(read),
            counts: new_attributes.iter().map(|a| (*a, (0, 0))).collect(),
            new_attributes,
            document: None,
            finished_changes: false,
            failed: false,
            stats: DeltaRecordStats::default(),
        }
    }

    fn change_count(
        &mut self,
        attribute: u32,
        length: u32,
        insert: bool,
    ) -> Result<(), SemanticError> {
        if !self.counts.contains_key(&attribute) {
            let (record, reads) = (self.read)(&stats_key(attribute))?;
            self.stats.statistics_reads += 1;
            self.stats.statistics_read_bytes += reads.block_bytes;
            let record = record.ok_or_else(|| corrupt("predecessor lacks corpus statistics"))?;
            self.counts
                .insert(attribute, statistics(&record, attribute)?);
        }
        if !insert && self.new_attributes.contains(&attribute) {
            return Err(corrupt("new fulltext attribute has a predecessor document"));
        }
        let (documents, tokens) = self
            .counts
            .get_mut(&attribute)
            .expect("corpus count initialized");
        *documents = if insert {
            documents.checked_add(1)
        } else {
            documents.checked_sub(1)
        }
        .ok_or_else(|| corrupt("corpus document count overflow or underflow"))?;
        *tokens = if insert {
            tokens.checked_add(u64::from(length))
        } else {
            tokens.checked_sub(u64::from(length))
        }
        .ok_or_else(|| corrupt("corpus token count overflow or underflow"))?;
        Ok(())
    }

    fn next_record(&mut self) -> Result<Option<(FulltextRecord, bool)>, SemanticError> {
        loop {
            if let Some((doc, insert)) = &mut self.document {
                if let Some(token) = next_token(&doc.text, &mut doc.offset, &mut doc.position) {
                    let mut key = term_prefix(doc.attribute, &token.term, true);
                    key.extend_from_slice(&doc.id);
                    return Ok(Some((
                        FulltextRecord {
                            key,
                            value: doc.length.to_be_bytes().to_vec(),
                        },
                        *insert,
                    )));
                }
                self.document = None;
            }
            if self.finished_changes {
                return Ok(self
                    .counts
                    .pop_first()
                    .map(|(attribute, (documents, tokens))| {
                        let mut value = documents.to_be_bytes().to_vec();
                        value.extend_from_slice(&tokens.to_be_bytes());
                        (
                            FulltextRecord {
                                key: stats_key(attribute),
                                value,
                            },
                            true,
                        )
                    }));
            }
            let Some(change) = self.changes.next() else {
                self.finished_changes = true;
                continue;
            };
            let (datom, insert) = change?;
            if !datom.added || !self.schema.attribute(datom.attribute)?.fulltext {
                continue;
            }
            let id = doc_id(&datom)?;
            let Value::String(text) = datom.value else {
                unreachable!()
            };
            let (mut offset, mut position) = (0, 0);
            let length =
                std::iter::from_fn(|| next_token(&text, &mut offset, &mut position)).count();
            let length =
                u32::try_from(length).map_err(|_| corrupt("document token count overflow"))?;
            self.change_count(datom.attribute, length, insert)?;
            if insert {
                self.stats.documents_added += 1;
            } else {
                self.stats.documents_removed += 1;
            }
            self.stats.tokenized_bytes = self
                .stats
                .tokenized_bytes
                .saturating_add((text.len() as u64).saturating_mul(2));
            let mut value = length.to_be_bytes().to_vec();
            value.extend_from_slice(&datom.source.to_be_bytes());
            value.extend_from_slice(text.as_bytes());
            let key = doc_key(datom.attribute, &id);
            self.document = Some((
                PendingDocument {
                    attribute: datom.attribute,
                    id,
                    text,
                    length,
                    offset: 0,
                    position: 0,
                },
                insert,
            ));
            return Ok(Some((FulltextRecord { key, value }, insert)));
        }
    }
}

impl Iterator for DeltaRecords<'_> {
    type Item = Result<(FulltextRecord, bool), SemanticError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        match self.next_record() {
            Ok(Some(record)) => Some(Ok(record)),
            Ok(None) => None,
            Err(error) => {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assertion_records_roundtrip_source_and_distinguish_transaction_contributions() {
        let mut schema = Schema::new();
        schema
            .install(
                crate::Attribute::new(
                    100,
                    crate::Keyword::new("article", "text"),
                    crate::ValueType::String,
                    crate::Cardinality::Many,
                )
                .fulltext(),
            )
            .unwrap();
        let first = Datom {
            entity: 42,
            attribute: 100,
            value: Value::String("Blue River".into()),
            tx: crate::t_to_tx(1).unwrap(),
            source: 99,
            added: true,
        };
        let second = Datom {
            tx: crate::t_to_tx(2).unwrap(),
            source: 98,
            ..first.clone()
        };
        let inputs = [first.clone(), second.clone()];
        let records = records_from(&schema, |_| Ok(Box::new(inputs.iter().cloned().map(Ok))))
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_ne!(doc_id(&first).unwrap(), doc_id(&second).unwrap());
        for datom in inputs {
            let id = doc_id(&datom).unwrap();
            let record = records
                .iter()
                .find(|r| r.key == doc_key(100, &id))
                .unwrap()
                .clone();
            let (decoded, length) = document(record.clone(), 100, &id).unwrap();
            assert_eq!(decoded, datom);
            assert_eq!(length, 2);
            let mut bad_source = record;
            bad_source.value[4..12].fill(0xff);
            assert!(document(bad_source, 100, &id).is_err());
        }
        let stats = records.iter().find(|r| r.key == stats_key(100)).unwrap();
        assert_eq!(statistics(stats, 100).unwrap(), (2, 4));
    }
}
