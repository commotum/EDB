# Log API

EDB's database log is a recording of transaction data in historic order,
organized for efficient access by transaction. The durable log stores immutable
transaction pages with a bounded tail and backward skip links; large transaction
payloads can span multiple chunks. Recent committed data is also maintained in
memory by the writer and peers. Log traversal reads and authenticates the
captured authoritative transaction content as needed.

## Log

[Connection API](types/02_connection.md#log) | [LogValue](types/08_log.md)

`Connection::log()` returns an immutable log value.

```rust
use edb_core::{Connection, LogValue};

fn capture_log(conn: &Connection) -> LogValue {
    conn.log()
}
```

The value captures the connection's currently observed transaction endpoint and
physical log generation without contacting the writer. Subsequent connection
advancement does not change it. `Peer::log()` and `BackupConnection::log()` also
return log values.

Given a log, you can retrieve an iterator over a range in the log with
[`tx_range`](#tx-range), or use the [log in query](#log-in-query-tx-ids-and-tx-data).

## tx-range

[LogValue API](types/08_log.md#tx_range) | [Async client API](01_async_client.md#exact-reads-and-streams)

You can access the log via the `tx_range` method.

```rust
use edb_core::{LogValue, SemanticError, TimePoint};

fn print_transactions(log: &LogValue) -> Result<(), SemanticError> {
    for transaction in log.tx_range(
        Some(TimePoint::T(1000)),
        Some(TimePoint::T(1020)),
    )? {
        let transaction = transaction?;
        println!("T={} datoms={}", transaction.t, transaction.data.len());
    }
    Ok(())
}
```

The asynchronous client provides the same range through a fallible stream:

```rust
use edb_core::{AsyncClient, AsyncStreamOptions, SemanticError, TimePoint};

async fn print_transactions_async(client: &AsyncClient) -> Result<(), SemanticError> {
    let mut transactions = client.tx_range(
        Some(TimePoint::T(1000)),
        Some(TimePoint::T(1020)),
        AsyncStreamOptions::default(),
    )?;
    while let Some(transaction) = transactions.next().await {
        let transaction = transaction?;
        println!("T={} datoms={}", transaction.t, transaction.data.len());
    }
    Ok(())
}
```

The arguments to `tx_range` are `start` (inclusive) and `end` (exclusive).
Legal values for these arguments are:

| Rust bound | Meaning |
| --- | --- |
| `Some(TimePoint::Tx(tx))` | A transaction entity ID. |
| `Some(TimePoint::T(t))` | A logical transaction coordinate. |
| `Some(TimePoint::Instant(milliseconds))` | A transaction instant in Unix epoch milliseconds. |
| `None` | The beginning or captured end of the log. |

An instant selects the first transaction at or after that instant. If several
transactions share an instant, the start includes them all and the end excludes
them all. An instant between transactions selects the following transaction.
Omitted bounds select `[1, basis_t() + 1)`; genesis at T=0 is not returned.
Valid bounds beyond the captured endpoint are clipped; reversed or disjoint
ranges are empty. Invalid transaction IDs and unrepresentable T coordinates
return errors.

`tx_range` returns a lazy `LogCursor`, an iterator of
`Result<LogTransaction, SemanticError>`. Each transaction contains these fields:

| Field | Value |
| --- | --- |
| `t` | The logical T coordinate of the transaction. |
| `data` | A `Vec<Datom>` containing the datoms asserted or retracted by the transaction. |

The [datoms](types/05_datom.md) have typed fields `entity`, `attribute`, `value`,
`tx`, `source`, and `added`. `source` identifies the entity to whom the assertion
or retraction is attributed. `added` is `true` for assertions and `false` for retractions.
Rust structs can be destructured accordingly:

```rust
use edb_core::{Datom, LogTransaction};

fn print_datoms(transaction: LogTransaction) {
    let LogTransaction { t, data } = transaction;
    for Datom { entity, attribute, value, tx, source, added } in data {
        println!("T={t} E={entity} A={attribute} V={value:?} TX={tx} S={source} added={added}");
    }
}
```

Both cursor construction and iteration can fail. Integer-bounded construction
performs no transaction payload reads; instant bounds can consult the captured
transaction-time index. Iteration authenticates transaction content and reports
storage failures, including missing transactions within the committed range.
After an error or exhaustion, the cursor yields no more items. It retains at
most one decoded transaction plus logarithmically many forward page anchors,
in addition to its captured snapshot; retaining results adds caller-owned memory.
The cursor keeps its navigation state between reads instead of seeking from the
captured root for each transaction. A large individual transaction still requires
correspondingly large memory.

Range traversal and direct transaction lookup use the same authenticated object
reader and generation-scoped [persistent log cache](../09_optional/02_log_cache.md).
Caching can reduce object reads; membership and contents still come from the
captured log. Measure complete traversal, including object reads and decoding,
when comparing costs; constructing a cursor does not consume its range.

## Log in Query: tx-ids and tx-data

The log API includes two convenience functions that are available within a
query. Supply the immutable log as a named source, such as `$log`, using
`QuerySourceValue::Log` for EDN queries or `QueryDataSource::log` for native
queries. A log source is consumed by `tx-ids` and `tx-data`.

The `tx-ids` function takes arguments similar to [tx-range](#tx-range), but
returns a collection of transaction entity references. You will typically use
the collection binding form `[?tx ...]` to capture the results:

```edn
[(tx-ids $log ?t1 ?t2) [?tx ...]]
```

Query bounds accept `nil` for an omitted bound, a nonnegative integer for T, an
`#edb/ref` transaction entity ID, or an `#inst` instant. An integer is a T
coordinate, not an implicitly recognized transaction entity ID.

The following example query returns the count of transactions within the range
`[t1, t2)`:

```rust
use edb_core::{LogValue, QueryControl, QueryOutcome, QuerySourceValue, SemanticError};
use edb_core::edn::read_edn;
use edb_core::edn_query::{parse_query_edn, EdnQueryArgument};

fn count_transactions(log: &LogValue) -> Result<QueryOutcome, SemanticError> {
    let query = parse_query_edn(
        r#"[:find (count ?tx)
            :in $log ?t1 ?t2
            :where [(tx-ids $log ?t1 ?t2) [?tx ...]]]"#,
    )?;
    let bound = query.bind(&[
        EdnQueryArgument::Source(QuerySourceValue::Log(log.clone())),
        EdnQueryArgument::Data(read_edn("1000")?),
        EdnQueryArgument::Data(read_edn("1020")?),
    ])?;
    bound.execute(&QueryControl::default(), None)
}
```

The `tx-data` function returns the datoms associated with a particular T or
transaction reference. You will typically use the relation binding form
`[[?e ?a ?v _ _ ?op]]` to capture the results:

```edn
[(tx-data $log ?tx) [[?e ?a ?v _ _ ?op]]]
```

The six positions are entity, attribute, value, transaction, source, and assertion flag.
Entity, attribute, transaction, and source positions are numeric references; `?op`
is a boolean. The first underscore skips the transaction, which is already bound
on input to the function; the second skips the assertion or retraction's source.
EDB also permits an already-bound transaction reference in that output position:
`[(tx-data $log ?tx) [[?e ?a ?v ?tx _ ?op]]]` unifies the returned reference with
`?tx`. When the input is a numeric T, use `_` or a different variable for the
transaction-reference output.
Trailing columns can also be omitted, as in `[[?e]]`.

`tx-data` is the direct way to get transaction data given T or a transaction
reference. A common scenario is to use `tx-data` in combination with `tx-ids`
to return the datoms associated with a range in time. The following example
query returns all the entities that were modified on August 1, 2013 (UTC):

```rust
use edb_core::{LogValue, QueryControl, QueryOutcome, QuerySourceValue, SemanticError};
use edb_core::edn::read_edn;
use edb_core::edn_query::{parse_query_edn, EdnQueryArgument};

fn modified_entities(log: &LogValue) -> Result<QueryOutcome, SemanticError> {
    let query = parse_query_edn(
        r#"[:find ?e
            :in $log ?t1 ?t2
            :where [(tx-ids $log ?t1 ?t2) [?tx ...]]
                   [(tx-data $log ?tx) [[?e]]]]"#,
    )?;
    let bound = query.bind(&[
        EdnQueryArgument::Source(QuerySourceValue::Log(log.clone())),
        EdnQueryArgument::Data(read_edn(r#"#inst "2013-08-01T00:00:00Z""#)?),
        EdnQueryArgument::Data(read_edn(r#"#inst "2013-08-02T00:00:00Z""#)?),
    ])?;
    bound.execute(&QueryControl::default(), None)
}
```

This includes entities appearing in assertions or retractions, including
transaction entities carrying transaction metadata. The relation deduplicates
entity results. Add a database source when joining these transaction references
to indexed provenance such as `:db/txInstant` or application-defined attributes;
see [query execution](../05_query_and_pull/07_executing_queries.md).

The corresponding native methods are `LogValue::tx_ids(start, end)` and
`LogValue::tx_data(IndexTransaction::T(t))` or
`LogValue::tx_data(IndexTransaction::Tx(tx))`. `tx_ids` still reads and
authenticates transaction payloads. `tx_data` returns `Ok(None)` for valid genesis
or future coordinates; missing data inside the captured committed range is an
error. In a query, valid genesis or future coordinates produce an empty
`tx-data` relation. Query functions accumulate their results under query resource limits;
use a native log cursor when you need incremental range consumption.

## Retained log data

The log includes assertions and retractions that `:db/noHistory` consolidation
may have removed from indexed history. Excision can publish a new physical log
generation; each captured value retains its original graph. Holding a log does
not create an indefinite storage retention pin: cold reads still require the
referenced objects to exist. See [log retention](types/08_log.md#retention) for
garbage-collection grace periods and offline backups, and
[change consumers](03_change_consumers.md) for acknowledged replay across captures.
