//! An immutable log endpoint and a lazy, authenticated transaction cursor.
//!
//! A retained snapshot captures one immutable publication, protected by the GC
//! grace period; an offline backup owns its immutable point. Log data comes
//! from authoritative transaction content, never an index whose noHistory
//! consolidation may already have discarded earlier assertions.
use crate::{
    Datom, IndexBoundary, IndexComponents, IndexTransaction, SemanticError, TimePoint, Value,
};
use std::fmt;
use std::iter::FusedIterator;

/// One immutable transaction in historical order. `data` includes both
/// additions and retractions, including information omitted from noHistory
/// indexes. The owning log's excision generation determines its retained data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogTransaction {
    pub t: u64,
    pub data: Vec<Datom>,
}

/// Captured transaction-log value, independent of subsequent peer advancement.
#[derive(Clone)]
pub struct LogValue {
    source: std::sync::Arc<crate::storage::BlockSnapshot>,
}

impl fmt::Debug for LogValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LogValue")
            .field("offline", &self.source.is_repository())
            .field("basis_t", &self.basis_t())
            .field("generation", &self.generation())
            .finish()
    }
}

/// Successful payload-read work performed by one cursor. Creating an integer-
/// bounded cursor performs no payload reads. Instant bounds may consult the
/// captured database's txInstant index separately from this accounting.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LogCursorStats {
    /// Logical page requests, including a failing request. An internal
    /// transport retry does not constitute a second logical page.
    pub range_reads: u64,
    pub transactions_read: u64,
    pub datoms_read: u64,
    pub payload_bytes_read: u64,
    /// Actual canonical object bytes transferred from PostgreSQL for this
    /// traversal, including authenticated navigation and chunk envelopes.
    pub postgres_payload_bytes_read: u64,
    /// Transactions decoded from authenticated persistent local payloads.
    pub cache_hits: u64,
    /// The implementation holds at most one complete transaction at a time.
    /// A caller retaining yielded values owns that additional memory.
    pub peak_buffered_transactions: usize,
}

/// Fallible ordered transaction traversal. It reads one authenticated
/// transaction per payload page and fuses after an error or range exhaustion.
/// Additional traversal memory is one codec-bounded transaction plus the
/// shared log traversal's logarithmic forward anchors, not the range length. A large
/// individual transaction still requires correspondingly large memory.
pub struct LogCursor {
    log: LogValue,
    traversal: crate::storage::log::LogTraversal,
    operation: Option<crate::OperationContext>,
    next_t: u64,
    end_t: u64,
    stats: LogCursorStats,
}

impl fmt::Debug for LogCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LogCursor")
            .field("log", &self.log)
            .field("next_t", &self.next_t)
            .field("end_t", &self.end_t)
            .field("stats", &self.stats)
            .finish()
    }
}

impl LogValue {
    pub(crate) fn from_block(snapshot: crate::storage::BlockSnapshot) -> Self {
        Self {
            source: std::sync::Arc::new(snapshot),
        }
    }

    pub fn basis_t(&self) -> u64 {
        self.source.basis_t()
    }

    /// The physical log generation captured with this immutable value.
    pub fn generation(&self) -> u64 {
        self.source.generation()
    }

    /// Start is inclusive and end exclusive. `None` means the beginning or
    /// captured end of this log. T/Tx bounds are exact; an Instant selects the
    /// first transaction at or after it, including the earliest duplicate.
    /// A reversed or disjoint range is empty.
    pub fn tx_range(
        &self,
        start: Option<TimePoint>,
        end: Option<TimePoint>,
    ) -> Result<LogCursor, SemanticError> {
        let next_t = self.basis_t().checked_add(1).ok_or_else(|| {
            fault(
                "log/time-overflow",
                "captured log has no next transaction boundary",
            )
        })?;
        let start = start
            .map(|point| self.resolve_bound(point))
            .transpose()?
            .unwrap_or(1)
            .max(1)
            .min(next_t);
        let end = end
            .map(|point| self.resolve_bound(point))
            .transpose()?
            .unwrap_or(next_t)
            .min(next_t);
        Ok(LogCursor {
            log: self.clone(),
            traversal: self
                .source
                .captured_log()
                .cloned()
                .unwrap_or_else(crate::storage::log::LogRoot::empty)
                .traversal(start, end.max(start))?,
            operation: crate::OperationContext::current(),
            next_t: start,
            end_t: end,
            stats: LogCursorStats::default(),
        })
    }

    /// Transaction entity ids in the same captured range. Payloads are still
    /// authenticated; this does not substitute unauthenticated SQL metadata.
    pub fn tx_ids(
        &self,
        start: Option<TimePoint>,
        end: Option<TimePoint>,
    ) -> Result<impl Iterator<Item = Result<u64, SemanticError>> + use<>, SemanticError> {
        Ok(self
            .tx_range(start, end)?
            .map(|transaction| crate::t_to_tx(transaction?.t)))
    }

    /// Full datoms for one T or transaction entity id. Genesis (T=0) is not a
    /// submitted transaction; absent or future transactions return `None`.
    pub fn tx_data(
        &self,
        transaction: IndexTransaction,
    ) -> Result<Option<Vec<Datom>>, SemanticError> {
        let t = match transaction {
            IndexTransaction::T(t) => {
                crate::t_to_tx(t)?;
                t
            }
            IndexTransaction::Tx(tx) => crate::tx_to_t(tx)?,
        };
        if t == 0 || t > self.basis_t() {
            return Ok(None);
        }
        self.read_transaction(t)
            .map(|(transaction, _, _, _)| Some(transaction.data))
    }

    fn resolve_bound(&self, point: TimePoint) -> Result<u64, SemanticError> {
        match point {
            TimePoint::T(t) => {
                crate::t_to_tx(t)?;
                Ok(t)
            }
            TimePoint::Tx(tx) => crate::tx_to_t(tx),
            TimePoint::Instant(instant) => {
                // Recovered log.clj tx-range uses db/t-at-or-since, not
                // as-of-t's predecessor rule for an instant between txes.
                let database = self.source.database_value();
                let mut datoms = database.seek_cursor(&IndexBoundary::Avet(
                    IndexComponents::Two(crate::DB_TX_INSTANT as u32, Value::Instant(instant)),
                ))?;
                let candidate = datoms.next().transpose()?;
                if let Some(datom) =
                    candidate.filter(|datom| datom.attribute == crate::DB_TX_INSTANT as u32)
                {
                    let Value::Instant(found) = datom.value else {
                        return Err(fault(
                            "log/invalid-tx-instant",
                            "transaction instant index contains a non-instant value",
                        ));
                    };
                    let t = crate::tx_to_t(datom.tx).map_err(|_| {
                        fault(
                            "log/invalid-tx-instant",
                            "transaction instant index contains an invalid transaction entity id",
                        )
                    })?;
                    if !datom.added
                        || found < instant
                        || datom.entity != datom.tx
                        || t == 0
                        || t > self.basis_t()
                    {
                        return Err(fault(
                            "log/invalid-tx-instant",
                            "transaction instant index contains an invalid transaction coordinate",
                        ));
                    }
                    Ok(t)
                } else {
                    self.basis_t().checked_add(1).ok_or_else(|| {
                        fault(
                            "log/time-overflow",
                            "captured log has no next transaction boundary",
                        )
                    })
                }
            }
        }
    }

    fn read_transaction(&self, t: u64) -> Result<(LogTransaction, u64, u64, bool), SemanticError> {
        // Membership is authenticated through the captured immutable log root.
        let (record, postgres_bytes, cached) = self.source.read_log_record_measured(t)?;
        Ok((
            LogTransaction {
                t: record.entry.basis_t,
                data: record.entry.tx_data,
            },
            record.encoded_bytes,
            postgres_bytes,
            cached,
        ))
    }
}

fn fault(code: &'static str, message: impl Into<String>) -> SemanticError {
    SemanticError::new(crate::ErrorCategory::Fault, code, message)
}

impl LogCursor {
    pub fn stats(&self) -> LogCursorStats {
        self.stats
    }
}

impl Iterator for LogCursor {
    type Item = Result<LogTransaction, SemanticError>;

    fn next(&mut self) -> Option<Self::Item> {
        let operation = self.operation.clone();
        let _scope = operation.as_ref().map(crate::OperationContext::enter);
        if self.next_t >= self.end_t {
            return None;
        }
        self.stats.range_reads = self.stats.range_reads.saturating_add(1);
        match self
            .log
            .source
            .next_log_record_measured(&mut self.traversal)
        {
            Some(Ok((record, postgres_bytes, cache_hit))) => {
                let transaction = LogTransaction {
                    t: record.entry.basis_t,
                    data: record.entry.tx_data,
                };
                let bytes = record.encoded_bytes;
                self.next_t += 1;
                self.stats.transactions_read = self.stats.transactions_read.saturating_add(1);
                self.stats.datoms_read = self
                    .stats
                    .datoms_read
                    .saturating_add(transaction.data.len() as u64);
                self.stats.payload_bytes_read = self.stats.payload_bytes_read.saturating_add(bytes);
                if cache_hit {
                    self.stats.cache_hits = self.stats.cache_hits.saturating_add(1);
                }
                self.stats.postgres_payload_bytes_read = self
                    .stats
                    .postgres_payload_bytes_read
                    .saturating_add(postgres_bytes);
                self.stats.peak_buffered_transactions = 1;
                Some(Ok(transaction))
            }
            Some(Err(error)) => {
                self.next_t = self.end_t;
                Some(Err(error))
            }
            None => {
                self.next_t = self.end_t;
                Some(Err(fault(
                    "storage/read-log-gap",
                    "Captured log range ended before its requested boundary",
                )))
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (
            0,
            usize::try_from(self.end_t.saturating_sub(self.next_t)).ok(),
        )
    }
}

impl FusedIterator for LogCursor {}
