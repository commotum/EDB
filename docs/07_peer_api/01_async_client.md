# Async native clients

`AsyncClient` and `AsyncExecutor` are additive adapters over the same native
Rust/PostgreSQL query, Pull, log, synchronization and transaction APIs. They
do not start a second database engine or turn the peer into a thin network
client. Existing blocking APIs and durable request formats are unchanged.

Create the native connection and fixed worker pool during application setup,
outside an executor task:

```rust,ignore
use atomic_core::{AsyncClient, AsyncExecutor, QueryControl};

let workers = AsyncExecutor::new(Default::default())?;
let client = AsyncClient::new(&connection, &workers)?;

// Inside an async task, on Tokio or another Rust Future executor:
let outcome = client.query(query, inputs, QueryControl::default())?.await?;
let transaction = client.transact(request, std::time::Duration::from_secs(30))?;
transaction.on_complete(|result| {
    // This callback and the report's eventual drop run on a worker.
    match result {
        Ok(report) => println!("committed at {}", report.basis_t),
        Err(error) => eprintln!("transaction outcome: {error}"),
    }
});
```

The facade performs blocking driver calls, canonical request encoding, query
evaluation and cursor advancement on its own fixed worker threads. Polling a
future only registers a waker or transfers a completed result. It does not
require a Tokio runtime. Each executor creates the configured number of
workers, not a new thread for every operation.

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
executor's own timeout combinator to stop waiting promptly if needed.

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
