# LogValue

```rust,ignore
pub struct LogValue { /* private fields */ }
```

Implements the [transaction log API](../../06_indexes/00_indexes_and_log.md#the-log-is-not-an-indexed-history-view).
Obtain a cloneable, immutable `edb_core::LogValue` with
[`Connection::log()`](02_connection.md#log) or `Peer::log()`.
It captures the reader's currently observed snapshot, including its transaction
endpoint and physical log generation. It does not contact the writer to discover
newer transactions. Subsequent peer advancement does not change this value.

## Field summary and details

`LogValue` has no public fields. Each transaction returned by the log is a
`LogTransaction` with these public fields:

| Field | Type | Description |
| --- | --- | --- |
| `t` | `u64` | The logical T point of the transaction. |
| `data` | `Vec<Datom>` | The datoms asserted or retracted by the transaction. |

```rust
use edb_core::{Datom, LogTransaction};

fn transaction_parts(transaction: LogTransaction) -> (u64, Vec<Datom>) {
    (transaction.t, transaction.data)
}
```

These are typed fields rather than keys in a transaction map. A
[datom's `added` field](05_datom.md) distinguishes assertions from retractions.
`t` is a logical coordinate; use `t_to_tx(t)` when a transaction entity ID is
required.

## Method summary

All methods below take `&self`. Fallible calls return `Result<_, SemanticError>`.

| Method | Successful result | Description |
| --- | --- | --- |
| `basis_t()` | `u64` | Last logical transaction coordinate captured by this log. |
| `generation()` | `u64` | Physical log generation captured with this value. |
| [`tx_range(start, end)`](#tx_range) | `LogCursor` | Transactions between inclusive start and exclusive end. |
| [`tx_ids(start, end)`](#tx_ids) | Fallible iterator of `u64` | Transaction entity IDs in the same range. |
| [`tx_data(transaction)`](#tx_data) | `Option<Vec<Datom>>` | Data for one transaction. |

## Method details

### tx_range

```rust,ignore
fn tx_range(
    &self,
    start: Option<TimePoint>,
    end: Option<TimePoint>,
) -> Result<LogCursor, SemanticError>;
```

Returns a range of transactions in the log, starting at `start`, or from the
beginning if `start` is `None`, and ending before `end`, or through the captured
end of the log if `end` is `None`. Each transaction contains the `t` and `data`
fields described above.

Parameters:

| Parameter | Description |
| --- | --- |
| `start` | A `TimePoint` or `None`; inclusive. |
| `end` | A `TimePoint` or `None`; exclusive. |

`TimePoint` has three explicit forms:

| Form | Meaning |
| --- | --- |
| `TimePoint::T(u64)` | Exact logical transaction coordinate. |
| `TimePoint::Tx(u64)` | Exact transaction entity ID, converted to its T coordinate. |
| `TimePoint::Instant(i64)` | Unix epoch milliseconds; selects the first transaction at or after that instant. |

An instant shared by several transactions resolves to the earliest of them.
As a start, it includes those transactions; as an end, it excludes them.
An instant between transactions selects the following transaction. An instant
after all captured transactions resolves to the boundary after `basis_t()`.

The first submitted transaction is T=1; genesis at T=0 is not returned.
Omitted bounds select `[1, basis_t() + 1)`. Valid bounds beyond the endpoint
are clipped to that endpoint, and reversed or disjoint ranges are empty.
Invalid transaction entity IDs or unrepresentable T coordinates return errors,
even if the range would otherwise be empty.

Returns a lazy `LogCursor` implementing
`Iterator<Item = Result<LogTransaction, SemanticError>>`, in increasing T order.
Handle both construction and iteration errors:

```rust
use edb_core::{LogValue, SemanticError, TimePoint};

fn print_transactions(log: &LogValue) -> Result<(), SemanticError> {
    // T=100 is included; T=200 is excluded, subject to the captured endpoint.
    let mut cursor = log.tx_range(Some(TimePoint::T(100)), Some(TimePoint::T(200)))?;
    for transaction in cursor.by_ref() {
        let transaction = transaction?;
        println!("T={} datoms={}", transaction.t, transaction.data.len());
    }
    println!("read {} transactions", cursor.stats().transactions_read);
    Ok(())
}
```

Integer-bounded cursor construction performs no transaction payload reads.
Instant bounds can read the captured transaction-time index. Construction can
fail during bound resolution or log navigation validation. Inconsistent instant
index contents produce `log/invalid-tx-instant`; an overflowing captured endpoint
produces `log/time-overflow`.

Iteration authenticates transaction content and can return storage, decoding,
or authentication errors. Unexpected exhaustion before the requested boundary
produces `storage/read-log-gap`, rather than silently shortening the range. After an error
or range exhaustion, the cursor is fused: every subsequent `next()` returns
`None`. Creating the cursor successfully does not guarantee successful traversal.

The cursor holds at most one complete decoded transaction plus logarithmic
forward navigation state, in addition to the captured snapshot. Large individual
transactions and caller-retained results still require memory.
`LogCursor::stats()` returns `LogCursorStats`; see the
[log cache counters](../../09_optional/02_log_cache.md#counters) for read, byte,
cache-hit, and buffering measurements.

### tx_ids

```rust,ignore
fn tx_ids(
    &self,
    start: Option<TimePoint>,
    end: Option<TimePoint>,
) -> Result<impl Iterator<Item = Result<u64, SemanticError>> + use<>, SemanticError>;
```

Returns transaction entity IDs for the same captured range and boundary rules
as `tx_range`. Transaction payloads are still read and authenticated; each
result converts the transaction's `t` with `t_to_tx`.

### tx_data

```rust,ignore
fn tx_data(
    &self,
    transaction: IndexTransaction,
) -> Result<Option<Vec<Datom>>, SemanticError>;
```

Returns full datoms for `IndexTransaction::T(t)` or `IndexTransaction::Tx(tx)`.
Valid genesis or future coordinates return `Ok(None)`. Invalid coordinates
return errors. A missing transaction within the captured committed range is a
storage error, not `None`.

For a source's activity, traverse the SAEVT prefix `[source :db/txInstant]`
on a database value at the same captured basis. Each transaction has one
immutable timestamp assertion with its source. Transaction entity IDs increase
with logical T, so reverse traversal gives newest transactions first. Retrieve
each timestamp datom's complete transaction with
`tx_data(IndexTransaction::Tx(datom.entity))`. The timeline retains transactions
whose facts were later replaced or retracted.

## Retention

Log data comes from authoritative transaction content, including assertions and
retractions that `:db/noHistory` consolidation may have removed from indexes.
The captured excision generation determines its retained data. Excision can
publish a new generation; an already-held value retains its original graph.

Holding or cloning a log does not register an indefinite storage retention pin.
Cold reads still require its referenced objects to exist. Configure the
[garbage-collection grace period](../../08_operations/00_deployment.md#inspection-retention-and-reclamation)
to cover active reads. An [offline backup](../../08_operations/05_backup_reads.md)
owns its immutable point and can also supply a log through `BackupConnection::log()`.
For acknowledged replay across captures, see
[change consumers](../03_change_consumers.md).
