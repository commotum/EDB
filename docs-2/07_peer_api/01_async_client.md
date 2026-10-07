# Client library reference

EDB provides synchronous native APIs and asynchronous adapters in the
`edb-core` Rust library. This page covers using those APIs after you have
[added the library to your project](../00_start_here/02_peer_library.md).
For a complete application walkthrough, see
[Client Getting Started](../01_tutorials/10_client_workflow.md).
For signatures and operation-specific options, see the
[synchronous API](09_client_api_sync.md) and
[asynchronous API](10_client_api_async.md) references.

`AsyncClient` and `AsyncExecutor` are additive adapters over the same native
Rust/PostgreSQL query, Pull, log, synchronization and transaction APIs. They
do not start a second database engine or turn the peer into a thin network
client. Existing blocking APIs and durable request formats are unchanged.

## Synchronous API

Synchronous API functions have the following common semantics:

- They block the calling thread when they perform storage or transaction I/O.
- They return a value, commonly `Result<T, SemanticError>` for fallible work.
- They indicate ordinary errors with `Err`, rather than throwing exceptions.

The following example shows a simple transaction using the synchronous API.
It assumes `connection` has an attached writer and the
[movie schema](../01_tutorials/06_transact_schema.md) is already installed:

```rust,ignore
use edb_core::TransactionRequest;
use std::time::Duration;

let request = TransactionRequest::from_edn(
    "add-goonies",
    r#"[{:db/id "goonies"
         :movie/title "The Goonies"
         :movie/genre "action/adventure"
         :movie/release-year 1985}]"#,
)?;
let result = connection.transact(request, Duration::from_secs(30))?;

// What id was assigned to The Goonies?
let goonies_id = result.tempids["goonies"];
```

For a separately running writer, use `transact_socket`, `transact_remote` or
`RemoteWriter`; see [transaction transports](#transaction-transports).
Keep the request key and exact data for retries of this logical request. A new
transaction needs a new key.

## Asynchronous API

The asynchronous API functions differ from the synchronous API in that:

- They admit work to a fixed worker pool and return Rust futures or streams.
- Database work runs on workers; polling does not perform that blocking work.
- Successful operations complete with a value; failed operations complete with
  `SemanticError`.
- Admission can itself return an error before a future is created.

Create the native connection and fixed worker pool during application setup,
outside an executor task:

```rust,ignore
use edb_core::{AsyncClient, AsyncExecutor};

let workers = AsyncExecutor::new(Default::default())?;
let client = AsyncClient::new(&connection, &workers)?;
```

You must await the future using a Rust future executor, such as Tokio, and
check both admission and execution errors. Using the same movie request from
above, inside an async function returning `Result<_, SemanticError>`:

```rust,ignore
let transaction = client.transact(request, Duration::from_secs(30))?;
let result = transaction.await?;
let goonies_id = result.tempids["goonies"];
// The result is a native report; keep its synchronous operations and
// any potentially blocking final destruction off the executor thread.
```

The facade performs blocking driver calls, canonical request encoding, query
evaluation and cursor advancement on its own fixed worker threads. Polling a
future only registers a waker or transfers a completed result. It does not
require a Tokio runtime. Each executor creates the configured number of
workers, not a new thread for every operation. See the
[async operation type](types/07_async_operation.md) for completion callbacks.

## Client Object

Database access begins by opening a native `Connection`; the async client
retains that connection and a worker pool. Configuration is supplied as Rust
arguments and types:

| Setting | Meaning | Default or requirement |
| --- | --- | --- |
| `PostgresConnectionConfig` | Storage connection, credentials, TLS and I/O policy | Explicit configuration; `postgres_config_from_env()` is a convenience loader |
| Database name | Logical database to open with `Connection::connect_configured` | Required; resolves to stable identity once |
| `cache_entries` | Native immutable-node cache entry capacity | Explicit argument to `connect_configured` |
| `AsyncConfig::workers` | Fixed blocking worker threads | `4` |
| `AsyncConfig::max_operations` | Queued, running and unconsumed operations | `64` |
| `AsyncConfig::max_resources` | Retained async clients and streams | `64` |

All three `AsyncConfig` counts must be positive. `AsyncClient::new` performs
no I/O, but can fail to reserve a resource slot. Worker construction starts
threads. Opening a native connection accesses storage. These belong in
application setup, with the PostgreSQL configuration described in
[configuration reference](../08_operations/14_configuration_reference.md).
Remote writes additionally take a verified writer endpoint and transport
configuration; those credentials do not authorize PostgreSQL reads.

### Catalog Operations

Use `DatabaseCatalog` to list, create, rename and retire logical databases.
Catalog operations use administrative storage access separately from an
`AsyncClient`:

```rust,ignore
use edb_core::{DatabaseCatalog, Schema};

let mut catalog = DatabaseCatalog::connect_configured(&admin_config)?;
let created = catalog.create_if_absent("movies", Schema::default())?;
let databases = catalog.list(None, 100)?;
```

Creation returns whether the database was created or already existed. Listing
uses an exclusive name cursor and an explicit limit of `1..=4096`. Retirement
removes the active name and fences its writer; physical reclamation is a
separate operation with a retention policy. There is no automatic client-side
background deletion promise. See [database lifecycle](../08_operations/02_database_lifecycle.md)
for checked rename/retirement and reclamation.

### Connection

`Connection::connect_configured` returns a connection passed to transaction
and synchronization APIs. `Connection::db()` captures its current immutable
database value without I/O. APIs that work against a single database, such as
query, Pull and datom access, use that value without a redundant connection
argument. Cold reads can still require PostgreSQL.

Connections are cloneable shared handles. Reuse and clone an existing handle
for concurrent callers; opening the same public name again is a new native
connection, not an automatically cached lookup. Handles do not need a
checkout/return pool API. Rust ownership controls their lifetime, including
background observation and any owned embedded writer. In particular, a
native connection's final drop may join its service. The async facade keeps
its retained connection's cleanup on a worker, as described below.

Existing handles keep their stable identity through rename and name reuse.
An ordinary operation error does not require reconstructing the facade, but
retryability depends on the error and whether a transaction was submitted.
See the [Connection type](types/02_connection.md) and
[database value type](types/03_database.md).

### Handling Errors

Errors are represented as `SemanticError`, with `category`, `code`, `message`,
`details` and an optional structured program-cancellation `anomaly`. The sync
API returns the error directly. The async API returns an admission error or
completes its future with an error; a stream yields an error item and ends.
See [error handling](07_error_handling.md) for categories, wrapped transport
errors and exact retries after an unknown transaction outcome.

### Busy

A `Busy` response means the relevant resource cannot currently admit the work.
For example, `async/operation-capacity` means the worker pool's operation slots
are occupied; that transaction was not submitted. `async/resource-capacity`
means retained clients or streams fill the resource limit. The facade does not
perform exponential backoff and retry before reporting these errors.

At this point you can consume or release retained work, reduce concurrency and
retry with application-level backoff, or give up on completing the request.
Repeated query pressure calls for measuring the bottleneck before changing
worker counts or capacity. Use [I/O statistics](08_io_stats.md), query
diagnostics and `AsyncExecutor::stats()` to distinguish work from retention.
For transactions, inspect the submission boundary before retrying: `Busy`
admission and an unknown outcome after submission have different contracts.

### Sync API Error Example

This native query is incorrect because its find variable `?nomen` is unbound.
It needs no database to demonstrate the error:

```rust
use edb_core::{ErrorCategory, FindElement, FindSpec, InputSpec, Query,
               QueryControl, QueryEngine, QueryInput, Value};

let mut query = Query::new(
    FindSpec::Relation(vec![FindElement::Variable("?nomen".into())]),
    vec![],
);
query.inputs.push(InputSpec::Scalar("?name".into()));
let inputs = vec![QueryInput::Scalar(Value::String("Rubble".into()))];
let error = QueryEngine::execute_sources(
    &query, &[], &inputs, &QueryControl::default(),
).unwrap_err();
assert_eq!(error.category, ErrorCategory::Incorrect);
assert_eq!(error.code, "query/unbound-find-variable");
println!("{}", error);
```

Output:

```text
query/unbound-find-variable: find variable ?nomen is unbound
```

### Async API Error Example

With the async API, the same native query's error is the future's result.
Using `query` above and a previously created `workers`, inside an async
function:

```rust,ignore
let operation = workers.query_sources(
    query, &[], inputs, QueryControl::default(), None,
)?;
let error = operation.await.unwrap_err();
assert_eq!(error.category, ErrorCategory::Incorrect);
assert_eq!(error.code, "query/unbound-find-variable");
```

EDN parsing and binding can reject invalid syntax or bindings before any
execution is admitted. Those remain synchronous errors from the parser or
binder; submitting a native `Query` here demonstrates an execution error.

### Timeouts

There is no single client-wide timeout or shared 60-second default. Specify
operation deadlines using the corresponding Rust argument:

| API | Timeout setting | Default |
| --- | --- | --- |
| Queries | `QueryControl::timeout` | `None` |
| Query/log streams | `AsyncStreamOptions::timeout` | `None` |
| Pull, snapshot reopening, `sync`, datom collection | `Option<Duration>` argument | Explicit `None` means no facade deadline |
| Transactions, route refresh, finite-target synchronization | `Duration` argument | Required |
| `db`, `log`, `request_index` | No timeout argument | No facade deadline |

For query sequences, the smaller of the query and stream timeouts applies.
Deadlines start at API admission and include queue delay. Expiry before a
worker step returns `Interrupted` with code `async/deadline`; cooperative
query expiry can return `query/timeout`. For example, this admitted source-free
query expires before execution:

```rust,ignore
let operation = workers.query_sources(
    Query::new(FindSpec::Relation(vec![]), vec![]),
    &[], vec![],
    QueryControl {
        timeout: Some(std::time::Duration::ZERO),
        ..Default::default()
    },
    None,
)?;
let error = operation.await.unwrap_err();
assert_eq!(error.category, ErrorCategory::Interrupted);
assert_eq!(error.code, "async/deadline");
```

A deadline is not a hard interrupt for a blocked driver or native callback.
Configure native PostgreSQL I/O policies separately. An admitted transaction
wait can instead report `UnknownOutcome`: giving up waiting does not imply
rollback. The following ownership and cancellation rules apply.

## Admission, ownership and cancellation

`AsyncConfig` defaults to four workers, 64 operation slots and 64 retained
resource slots. `max_operations` includes queued, running **and completed but
unconsumed** results. `max_resources` includes retained async clients and
streams. Release or consume unused operations; retaining every completed
future can intentionally prevent more work from being admitted. These are
operational concurrency/retention policies, not new data or query limits.
Dropping the executor does not synchronously join workers. Admitted work and
cleanup retain the pool until they finish; an application that needs a known
transaction outcome before process exit must await it or its completion signal.

Fresh operation admission returns `async/operation-capacity` (`Busy`) if all
slots are occupied. Such a transaction was not submitted. Once `transact`
returns an `AsyncTransaction`, dropping it abandons the result, not the queued
transaction. The worker still attempts it unless its deadline expired before
submission. Retain the exact request key and data when explicitly retrying an
unknown outcome. No facade method invents a new retry key.

Dropping an ordinary read future can skip its not-yet-started read. A running
query remains cooperative: its existing `QueryControl.cancel`, work/value
budgets and timeout still apply. Dropping a stream stops future advancement,
not an already-running PostgreSQL call. Deadlines start at API admission and
include queue delay; expiry is checked before worker execution and between
stream steps. They are not a hard interrupt for arbitrary native callbacks or
blocked driver calls. Configure native PostgreSQL I/O policies and use the
future runtime's timeout combinator to stop waiting promptly if needed.

Abandoned results, streams and the facade's retained connection are disposed
on workers. Their reserved slots stay live until that cleanup finishes.
Neither future/stream cancellation nor dropping the last facade joins a
blocked worker or performs storage writes on the executor thread.

**Consumed native results keep their native contract.** A returned
`DatabaseValue`, `LogValue`, `ServiceTransactionReport` or
`CommittedTransaction` is not an async wrapper. Its synchronous reads may
perform PostgreSQL I/O; an embedded connection may also join its service on
last drop. Keep those operations off your executor thread. Dropping immutable
read values issues no SQL; their driver uses the bounded disposal path. This boundary
does not change the existing native types or pretend that Rust `Drop` is async.

`operation.on_complete(callback)` consumes the future instead of awaiting it.
The callback runs once on a blocking worker, even if completion preceded
registration. It never runs on the writer or invoking executor thread. Its
panics are contained, but arbitrary callback work is cooperative and occupies
a worker while it runs.

For example, leave an admitted transaction's report and its eventual destruction
on a worker by consuming the operation with a completion callback:

```rust,ignore
let transaction = client.transact(request, std::time::Duration::from_secs(30))?;
transaction.on_complete(|result| {
    match result {
        Ok(report) => println!("committed at {}", report.basis_t),
        Err(error) => eprintln!("transaction outcome: {error}"),
    }
});
```

## Exact reads and streams

Client query, Pull, `db`, log and index-collection methods capture the
connection's immutable value when called, not on the future's first poll.
`query_sequence` and `tx_range` similarly retain one captured endpoint through
all chunks. Subsequent commits, connection advancement or mutable-name reuse
cannot redirect these reads.

`AsyncExecutor::query_sources`, `query_prepared` and
`query_sequence_sources` accept the existing explicit database, ordinary
relation and immutable-log sources. All existing query inputs, rules, native
callbacks and custom aggregate registries remain engine inputs; no fake
database is required for source-free computation. Prepared execution reuses
the native prepared query.

To query an `as_of`, `since` or `history` view, derive it from a captured
`DatabaseValue` and pass it as `QueryDataSource::database("$", view)` to an
executor query method. A speculative transaction's `db_after` is also a native
database query source. The view and speculation rules are those of
[`DatabaseValue`](types/03_database.md); no separate `with-db` preparation is
needed. Speculation, Pull on an explicit value, and native index cursors retain
their synchronous contracts, so keep their potentially blocking work off the
future executor thread. The facade's `collect_datoms` methods capture the
connection's current value; they do not accept a derived view.

`AsyncStream<T>` implements `futures_core::Stream<Item = Result<T,
SemanticError>>` and also provides `next().await`. `AsyncStreamOptions` defaults
to chunks of 64 rows, with at most one chunk job outstanding per stream. A
slow consumer never leaves a worker blocked on a full output channel. A
stream waiting for worker admission is woken when an operation slot becomes
available. One error ends the stream; any preceding successful rows already
computed in that chunk are delivered before the error, then it stays ended.

Query sequences retain the existing contract: relational evaluation and
aggregate preparation are eager, while Pull projection is performed only for
demanded chunks. Chunking does not make joins memory-streaming. Log traversal
uses the existing authenticated transaction cursor and captured end bound.
A chunk limit counts rows, not bytes; a single transaction/value can be large
within its existing codec/query admission policy. `collect_datoms` and
`collect_datoms_with_prefix` explicitly return materialized vectors on a
worker; they do not claim lazy index traversal.

`sync_to`, `sync_index`, `sync_schema` and `sync_excise` retain their distinct
logical/index/schema/excision completion conditions and finite target. Their
native waits receive the remaining deadline after queueing. `sync` captures a
fresh durable head on its worker and includes transactions completed before
admission; later transactions may also be included.

## Transaction transports

`transact` uses the attached in-process writer. On Unix,
`transact_socket` uses the existing same-host protocol; `transact_remote`
uses an explicit verified TLS endpoint. `transact_routed` and `refresh_route`
delegate to a connection-bound `RemoteWriter`, never re-resolving a public
database name. The route may rediscover a stale endpoint only under its
pre-submission retry policy; ambiguous submissions are not replayed
automatically. Hint variants retain the native advisory-hint contract.

Remote/socket results preserve `CommittedTransaction`: a known committed
basis/hash can accompany `report: Err` if local snapshot opening failed.
That is not a rejected transaction. Native observation failure likewise
cannot change an attached writer's known committed report into a rollback.

## Chunked Results

APIs returning sequences can compute and deliver results in chunks. Control
async worker batching with `AsyncStreamOptions`:

| Field | Meaning | Default | Accepted values |
| --- | --- | --- | --- |
| `chunk_rows` | Maximum rows advanced by one worker job | `64` | Positive `usize` |
| `timeout` | Deadline for the whole stream, including queue delay | `None` | Optional `Duration` |

Change chunk size to address a measurable performance need. Smaller chunks
require more worker scheduling and can increase overhead, but retain fewer
projected rows at once. Larger chunks reverse this tradeoff and keep a worker
advancing that stream for longer. This is a worker scheduling boundary, not a
network message or SQL roundtrip boundary. A chunk limit counts rows, not bytes.

Chunk size is orthogonal to the actual relational work:

- Ordinary query execution realizes its result before completing the future.
- Query sequences prepare joins and aggregates eagerly, then perform Pull
  projection for demanded chunks. They do not make joins memory-streaming.
- Log ranges advance their native authenticated cursor in demand-driven chunks.
- `collect_datoms` and `collect_datoms_with_prefix` return a materialized vector
  and have no chunk option.

The synchronous `QuerySequence` and log cursor expose iterators. The async
`AsyncStream<T>` buffers chunks internally and exposes individual
`Result<T, SemanticError>` items via `next().await`; it does not return arrays
of chunks to the caller. There is at most one chunk job outstanding per stream.
A slow consumer does not leave a worker blocked on a full output channel.

### Chunked Results Example

If you wanted to know the average length of an artist's name, you could reduce
query-sequence results in worker batches of 10,000 rows. The small ordinary-data
fixture below keeps the example runnable without a database. An artist ID in
`:with` preserves separate artists that happen to have the same name.

```rust
use edb_core::{AsyncExecutor, AsyncStreamOptions, QueryControl, QueryValue,
               SemanticError, Value};
use edb_core::edn::read_edn;
use edb_core::edn_query::{EdnQueryArgument, parse_query_edn};

#[derive(Default)]
struct Averager {
    count: usize,
    total: usize,
}
impl Averager {
    fn add(&mut self, length: usize) {
        self.count += 1;
        self.total += length;
    }
    fn finish(self) -> Option<f64> {
        (self.count != 0).then(|| self.total as f64 / self.count as f64)
    }
}

fn name_length(row: &[QueryValue]) -> Result<usize, SemanticError> {
    match row.first() {
        Some(QueryValue::Scalar(Value::String(name))) => Ok(name.chars().count()),
        _ => Err(SemanticError::incorrect(
            "app/name-shape", "expected a string in the first result column",
        )),
    }
}

async fn average_names(workers: &AsyncExecutor) -> Result<Option<f64>, SemanticError> {
    let bound = parse_query_edn(
        "[:find ?name :with ?artist :in [[?artist ?name]]]",
    )?.bind(&[EdnQueryArgument::Data(read_edn(
        r#"[[1 "Flintstone"] [2 "Rubble"]]"#,
    )?)])?;
    let mut names = workers.query_sequence_sources(
        bound.query().clone(), &bound.sources, bound.inputs.clone(),
        QueryControl::default(), None,
        AsyncStreamOptions { chunk_rows: 10_000, ..Default::default() },
    )?;
    let mut average = Averager::default();
    while let Some(row) = names.next().await {
        average.add(name_length(&row?)?);
    }
    Ok(average.finish())
}
```

Awaiting `average_names(&workers)` returns `Ok(Some(8.0))`. Errors stop the
reduction immediately; the stream is dropped and its retained cursor is cleaned
on a worker. An empty input returns `Ok(None)`, avoiding division by zero.
Lengths count Unicode scalar values, matching query `count` on strings.

The decomposition into named reducing functions and transforms makes it easy
to test the parts of an async program synchronously, without mocking or
stubbing. Using the definitions above:

```rust
let mut average = Averager::default();
for length in [5, 4] {
    average.add(length);
}
assert_eq!(average.finish(), Some(4.5));

let rows = ["Flintstone", "Rubble"].map(|name| {
    vec![QueryValue::Scalar(Value::String(name.into()))]
});
let lengths = rows.iter().map(|row| name_length(row)).collect::<Result<Vec<_>, _>>()?;
assert_eq!(lengths, vec![10, 6]);
let mut average = Averager::default();
for length in lengths {
    average.add(length);
}
assert_eq!(average.finish(), Some(8.0));
```

Add such reductions when your application has a measured need. For this
simple calculation, the same computation can be expressed as an aggregate
query, using the ordinary-data fixture as its input:

```edn
[:find (avg ?ct)
 :with ?artist
 :in [[?artist ?name]]
 :where [(count ?name) ?ct]]
```

For database artists, replace `:in [[?artist ?name]]` with `:in $` and add
`[?artist :artist/name ?name]` to `:where`. Results from a music corpus depend
on the data; the two-name fixture above has mean length `8.0`.

## Offset and Limit

There is no common `offset`/`limit` argument map for native query, datom and
log APIs, and no automatic truncation to 1,000 rows. `QueryControl` resource
budgets fail execution when exceeded; they do not select a result page.
Consume the desired part of a sequence explicitly:

| Choice | Meaning |
| --- | --- |
| Offset | Number of successful rows to consume and omit |
| Limit | Maximum successful rows to retain after the offset |
| All results | Consume the sequence until it ends; there is no `-1` sentinel |

For ordinary materialized results, slicing a collection changes only what the
application retains; the query has already computed that result. Consuming only
part of an async query sequence still prepares its relational work eagerly and
may project additional rows in the last worker chunk. Dropping the stream stops
future advancement, subject to already-running work. Catalog listing has its
own [keyset pagination](#catalog-operations) contract.

### Offset and Limit Example

The following helper consumes ten rows, then retains at most two. Offset ten
starts with the **eleventh** result. It checks errors even in skipped rows:

```rust
use edb_core::{AsyncStream, QueryValue, SemanticError};

async fn page(
    mut rows: AsyncStream<Vec<QueryValue>>,
    offset: usize,
    limit: usize,
) -> Result<Vec<Vec<QueryValue>>, SemanticError> {
    let mut result = Vec::new();
    if limit == 0 {
        return Ok(result);
    }
    for _ in 0..offset {
        match rows.next().await {
            Some(row) => { row?; }
            None => return Ok(result),
        }
    }
    for _ in 0..limit {
        match rows.next().await {
            Some(row) => result.push(row?),
            None => break,
        }
    }
    Ok(result)
}
```

Call `page(names, 10, 2).await?` with a stream from `query_sequence` or
`query_sequence_sources`. A query relation is a set of tuples, not an
application-defined sort order; do not interpret these as the eleventh and
twelfth names alphabetically. For a stable user-facing page order, define the
order in the application and keep the database value fixed across pages.
