//! Candidate discovery, native ranking and exact supplied-view validation.
use super::analysis::{CompiledSearch, SearchTerm, Token, bm25};
use super::control::{Budget, resource};
use super::{FulltextHit, FulltextOptions, FulltextReport, FulltextStats, records};
use crate::{DatabaseValue, Datom, ErrorCategory, IndexPrefix, SemanticError, Value, ValueType};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

struct Document {
    datom: Datom,
    tokens: Vec<Token>,
}

impl DatabaseValue {
    pub fn fulltext(
        &self,
        attribute: u32,
        search: &str,
        options: &FulltextOptions,
    ) -> Result<FulltextReport, SemanticError> {
        self.fulltext_with_budget(attribute, search, options, &mut |_, _| Ok(()))
    }

    pub(crate) fn fulltext_with_budget(
        &self,
        attribute: u32,
        search: &str,
        options: &FulltextOptions,
        charge: &mut dyn FnMut(usize, usize) -> Result<(), SemanticError>,
    ) -> Result<FulltextReport, SemanticError> {
        let mut budget = Budget {
            options,
            deadline: options
                .timeout
                .and_then(|timeout| Instant::now().checked_add(timeout)),
            work: 0,
            bytes: 0,
            read_bytes: 0,
            external: charge,
        };
        budget.charge(1, 0)?;
        let descriptor = self.schema().attribute(attribute)?;
        if !descriptor.fulltext || descriptor.value_type != ValueType::String {
            return Err(SemanticError::incorrect(
                "fulltext/not-indexed",
                "search requires a fulltext string attribute",
            ));
        }
        if search.len() > options.max_query_bytes {
            return Err(resource(
                "fulltext/query-capacity",
                "search expression exceeds its configured byte allowance",
            ));
        }
        budget.charge(search.len(), search.len().saturating_mul(128))?;
        let query = CompiledSearch::parse(
            search,
            options.max_query_tokens,
            options.max_query_depth.min(128),
        )?;
        if query.terms.is_empty() {
            return Ok(FulltextReport {
                hits: Vec::new(),
                stats: FulltextStats {
                    work: budget.work as u64,
                    admitted_bytes: budget.bytes as u64,
                    index_basis_t: self.basis_t(),
                    ..Default::default()
                },
            });
        }
        // A native value must never silently materialize its entire database
        // when the durable sidecar is not available.
        if let Some(snapshot) = self.block_snapshot() {
            let reader = if snapshot
                .base_metadata()
                .schema
                .attribute(attribute)
                .is_ok_and(|a| a.fulltext)
            {
                self.native_fulltext_reader()?
            } else {
                None
            };
            let mut report = self.search_native(attribute, &query, reader.as_ref(), &mut budget)?;
            if reader.is_none() {
                // A recently installed attribute has no indexed corpus yet.
                // Report physical indexing progress even though the recent
                // and speculative tiers cover its complete assertion corpus.
                report.stats.index_basis_t = snapshot.index_descriptor().basis;
            }
            return Ok(report);
        }
        let mut docs = Vec::new();
        for datom in self.fulltext_history_cursor(attribute)? {
            let datom = datom?;
            budget.charge(1, std::mem::size_of::<Document>())?;
            if !datom.added {
                continue;
            }
            let Value::String(text) = &datom.value else {
                return Err(SemanticError::new(
                    ErrorCategory::Fault,
                    "fulltext/non-string-index",
                    "search source contains a non-string",
                ));
            };
            let tokens = budget.tokens(text)?;
            docs.push(Document { datom, tokens });
        }
        let documents = docs.len() as u64;
        let total_length = docs.iter().map(|doc| doc.tokens.len() as u64).sum();
        let mut frequencies = BTreeMap::<String, u64>::new();
        for doc in &docs {
            budget.charge(doc.tokens.len(), doc.tokens.len().saturating_mul(128))?;
            let terms: BTreeSet<_> = doc.tokens.iter().map(|token| &token.term).collect();
            for term in terms {
                *frequencies.entry(term.clone()).or_default() += 1;
            }
        }
        let mut hits = BTreeMap::<(u64, String, u64), f64>::new();
        for doc in docs {
            budget.charge(1, 0)?;
            if !budget.matches(&query, &doc.tokens)? {
                continue;
            }
            let score = score(&query, &doc.tokens, documents, total_length, &frequencies);
            self.accept_fulltext_candidate(&doc.datom, score, &mut hits, &mut budget)?;
        }
        finish(hits, self.basis_t(), &mut budget)
    }

    fn search_native(
        &self,
        attribute: u32,
        query: &CompiledSearch,
        reader: Option<&crate::NativeFulltextReader>,
        budget: &mut Budget<'_>,
    ) -> Result<FulltextReport, SemanticError> {
        let mut documents = 0u64;
        let mut total_length = 0u64;
        let mut candidates = BTreeSet::<records::DocId>::new();
        let mut frequencies = BTreeMap::<String, u64>::new();
        let mut index_basis_t = self.basis_t();
        if let Some(reader) = reader {
            index_basis_t = reader.source_basis_t();
            let corpus =
                read_one(reader, &records::stats_key(attribute), budget)?.ok_or_else(|| {
                    SemanticError::new(
                        ErrorCategory::Unavailable,
                        "fulltext/index-unavailable",
                        "attribute is not represented by this search publication",
                    )
                })?;
            (documents, total_length) = records::statistics(&corpus, attribute)?;
            for term in effective_terms(query) {
                let (text, exact) = match term {
                    SearchTerm::Exact(text) => (text, true),
                    SearchTerm::Prefix(text) => (text, false),
                };
                budget.charge(1, text.len() + 5)?;
                let mut cursor = reader.prefix(
                    &records::term_prefix(attribute, text, exact),
                    read_limits(budget),
                )?;
                let mut prior = crate::FulltextReadStats::default();
                loop {
                    budget.charge(1, 0)?;
                    let result = cursor.next().transpose();
                    let now = cursor.stats();
                    let charged = charge_cursor(budget, prior, now);
                    prior = now;
                    charged?;
                    let record = result?;
                    let Some(record) = record else {
                        break;
                    };
                    budget.charge(1, record.key.len() + record.value.len() + 96)?;
                    let (term, id, _length) = records::posting(&record, attribute)?;
                    *frequencies.entry(term).or_default() += 1;
                    candidates.insert(id);
                }
            }
        }
        // Complete the persisted corpus with the bounded authenticated recent
        // tail and speculative assertions for this attribute. Keep historical
        // assertions as candidates too: exact-view validation below handles
        // current retractions, temporal windows, noHistory and opaque filters.
        let mut local = Vec::new();
        for datom in self.fulltext_unindexed_cursor(attribute)? {
            budget.charge(1, std::mem::size_of::<Document>())?;
            if !datom.added {
                continue;
            }
            let Value::String(text) = &datom.value else {
                return Err(records_error(
                    "non-string recent or speculative search datum",
                ));
            };
            let tokens = budget.tokens(text)?;
            budget.charge(tokens.len(), tokens.len().saturating_mul(128))?;
            documents = documents.saturating_add(1);
            total_length = total_length.saturating_add(tokens.len() as u64);
            let unique: BTreeSet<_> = tokens.iter().map(|t| &t.term).collect();
            for term in unique {
                *frequencies.entry(term.clone()).or_default() += 1;
            }
            local.push(Document { datom, tokens });
        }
        if frequencies.values().any(|frequency| *frequency > documents) {
            return Err(records_error(
                "posting frequency exceeds authenticated corpus count",
            ));
        }
        let mut hits = BTreeMap::new();
        if let Some(reader) = reader {
            for id in candidates {
                let record = read_one(reader, &records::doc_key(attribute, &id), budget)?
                    .ok_or_else(|| records_error("posting references a missing document"))?;
                let (datom, length) = records::document(record, attribute, &id)?;
                let Value::String(text) = &datom.value else {
                    unreachable!()
                };
                let tokens = budget.tokens(text)?;
                if tokens.len() as u64 != u64::from(length) {
                    return Err(records_error("analyzed document length mismatch"));
                }
                budget.charge(tokens.len(), 0)?;
                if budget.matches(query, &tokens)? {
                    self.accept_fulltext_candidate(
                        &datom,
                        score(query, &tokens, documents, total_length, &frequencies),
                        &mut hits,
                        budget,
                    )?;
                }
            }
        }
        for doc in local {
            budget.charge(doc.tokens.len(), 0)?;
            if budget.matches(query, &doc.tokens)? {
                self.accept_fulltext_candidate(
                    &doc.datom,
                    score(query, &doc.tokens, documents, total_length, &frequencies),
                    &mut hits,
                    budget,
                )?;
            }
        }
        finish(hits, index_basis_t, budget)
    }

    fn accept_fulltext_candidate(
        &self,
        candidate: &Datom,
        score: f64,
        hits: &mut BTreeMap<(u64, String, u64), f64>,
        budget: &mut Budget<'_>,
    ) -> Result<(), SemanticError> {
        let prefix = IndexPrefix::Eavt {
            entity: candidate.entity,
            attribute: Some(candidate.attribute),
            value: Some(candidate.value.clone()),
        };
        budget.charge(1, candidate.value.retained_heap_bytes() as usize)?;
        let mut cursor = self.prefix_cursor(&prefix)?;
        // ATOMIC-NOTE: exact-view validation corresponds to source windowed
        // lookup, but native shared budgets also cover candidates rejected by
        // time/history filters inside a single cursor advancement.
        while let Some(fact) = cursor.next_with_control(&mut |candidate| {
            budget.charge(usize::from(candidate.is_some()), 0)?;
            Ok(true)
        }) {
            let fact = fact?;
            budget.charge(0, 0)?;
            if !fact.added || !fact.value.stored_eq(&candidate.value) {
                continue;
            }
            let Value::String(text) = fact.value else {
                unreachable!()
            };
            budget.charge(1, text.len() + std::mem::size_of::<FulltextHit>())?;
            hits.entry((fact.entity, text, fact.tx))
                .and_modify(|old| *old = old.max(score))
                .or_insert(score);
        }
        Ok(())
    }
}

fn score(
    query: &CompiledSearch,
    tokens: &[Token],
    documents: u64,
    total: u64,
    frequencies: &BTreeMap<String, u64>,
) -> f64 {
    let mut tf = BTreeMap::<&str, u32>::new();
    for token in tokens {
        *tf.entry(&token.term).or_default() += 1;
    }
    tf.into_iter()
        .filter(|(word, _)| {
            query.terms.iter().any(|term| match term {
                SearchTerm::Exact(term) => term == word,
                SearchTerm::Prefix(prefix) => word.starts_with(prefix),
            })
        })
        .map(|(word, count)| {
            bm25(
                count,
                tokens.len() as u32,
                documents,
                total,
                frequencies.get(word).copied().unwrap_or(0),
            )
        })
        .sum()
}

fn finish(
    hits: BTreeMap<(u64, String, u64), f64>,
    index_basis_t: u64,
    budget: &mut Budget<'_>,
) -> Result<FulltextReport, SemanticError> {
    // Admit an n*ceil(log2(n)) sorting allowance before doing the work. String
    // comparison costs are additionally bounded by the already admitted text.
    let levels = if hits.len() < 2 {
        1
    } else {
        usize::BITS as usize - (hits.len() - 1).leading_zeros() as usize
    };
    budget.charge(
        hits.len().saturating_mul(levels),
        hits.len()
            .saturating_mul(std::mem::size_of::<FulltextHit>()),
    )?;
    let mut hits: Vec<_> = hits
        .into_iter()
        .map(|((entity, value, tx), score)| FulltextHit {
            entity,
            value,
            tx,
            score,
        })
        .collect();
    hits.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.entity.cmp(&b.entity))
            .then(a.value.cmp(&b.value))
            .then(a.tx.cmp(&b.tx))
    });
    budget.charge(0, 0)?;
    let truncated = hits.len() > budget.options.limit;
    hits.truncate(budget.options.limit);
    Ok(FulltextReport {
        hits,
        stats: FulltextStats {
            work: budget.work as u64,
            read_bytes: budget.read_bytes,
            admitted_bytes: budget.bytes as u64,
            index_basis_t,
            truncated,
        },
    })
}

fn effective_terms(query: &CompiledSearch) -> Vec<&SearchTerm> {
    // Disjoint prefix ranges avoid double-counting document frequencies. Keep
    // the broadest prefixes in sorted order, then check exact terms by seek;
    // do not compare every query term to every other term.
    let mut prefixes = BTreeSet::<&str>::new();
    for term in &query.terms {
        if let SearchTerm::Prefix(prefix) = term
            && !prefixes
                .last()
                .is_some_and(|previous| prefix.starts_with(previous))
        {
            prefixes.insert(prefix);
        }
    }
    query
        .terms
        .iter()
        .filter(|term| match term {
            SearchTerm::Exact(text) => !prefixes
                .range(..=text.as_str())
                .next_back()
                .is_some_and(|prefix| text.starts_with(prefix)),
            SearchTerm::Prefix(text) => prefixes.contains(text.as_str()),
        })
        .collect()
}
fn read_limits(budget: &Budget<'_>) -> crate::FulltextReadLimits {
    crate::FulltextReadLimits {
        max_records: budget.options.max_work.saturating_sub(budget.work).max(1) as u64,
        max_block_bytes: budget.options.max_bytes.saturating_sub(budget.bytes).max(1) as u64,
    }
}
fn charge_cursor(
    budget: &mut Budget<'_>,
    before: crate::FulltextReadStats,
    after: crate::FulltextReadStats,
) -> Result<(), SemanticError> {
    let read = after.block_bytes.saturating_sub(before.block_bytes);
    budget.read_bytes = budget.read_bytes.saturating_add(read);
    budget.charge(
        after
            .records_examined
            .saturating_sub(before.records_examined) as usize,
        after.visited_bytes.saturating_sub(before.visited_bytes) as usize,
    )
}
fn read_one(
    reader: &crate::NativeFulltextReader,
    key: &[u8],
    budget: &mut Budget<'_>,
) -> Result<Option<crate::FulltextRecord>, SemanticError> {
    budget.charge(1, key.len())?;
    let mut cursor = reader.prefix(key, read_limits(budget))?;
    let result = cursor.next().transpose();
    let charged = charge_cursor(budget, crate::FulltextReadStats::default(), cursor.stats());
    charged?;
    let record = result?;
    let record = record.filter(|record| record.key == key);
    if let Some(record) = &record {
        budget.charge(1, record.key.len() + record.value.len())?;
    }
    Ok(record)
}
fn records_error(message: &str) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, "fulltext/invalid-record", message)
}
