# Background Indexing

## Rationale

EDB maintains multiple sorted indexes of datoms. For the most part, users do
not have to request, specify, or otherwise think about these indexes. Rather,
the indexes are used implicitly to enforce transactional invariants and to
perform Datalog queries and pulls. The job of background indexing in EDB is to
build and maintain their durable representation. AVET and SAVET share coverage
that depends on schema and completed projection work; see [ordered indexes](00_indexes_and_log.md).

## The Job

Naively, one could sort the entire database on every transaction. To avoid this,
EDB maintains durable covering trees and a recent tier and merges them to
produce the six sorted index orders for the database. The recent tier is the memory
index. The memory index must be very fast for two reasons: First, it incorporates
every new datom on every transaction. Second, opening a database reconstructs
the unindexed tail from the log against the durable indexed base. Subsequent
refreshes can extend an existing recent tier without replaying its resident
prefix.

The memory index cannot grow forever. Every so often, EDB will perform a
background indexing job. This job freezes a committed prefix, merging those
datoms into durable index trees. Newly committed transactions remain in the live
memory index while that prefix is prepared. Publication retires the covered
prefix; it does not discard newer transactions. Already captured immutable
database values retain their view.

The trees have a wide branching factor: the default configuration allows 1,024
leaves per directory and 1,024 directories per root. Leaf segments have a target
size of 256 KiB and a maximum of 4,096 datoms; a larger legal value can occupy a
larger leaf subject to the separate 20 MiB leaf limit. Incremental merging can
reuse untouched subtrees and rewrite affected regions. This reduces work when
changes are localized, but is not a guarantee of sublinear job time for every
workload.

Background indexing jobs use both CPU and I/O. CPU is used to prepare and merge
datoms and to serialize and deserialize segments. I/O is needed to both read
the current index segments and to write the new ones. EDB runs one background
preparation job at a time in the transaction service, with its own storage
connection. Selected CPU edit-preparation work can run in parallel; object
loading, tree merging and publication remain ordered.

Preparation writes protected immutable objects. The serialized writer then
checks the candidate's source, log prefix and publication guards before
adopting it. Writes can advance during preparation, and adoption preserves
their log and receipts. A candidate whose index source has changed is discarded
and recaptured. Other job failures have bounded retries; an exhausted failure
closes write admission and is exposed in indexing statistics. Required fulltext
data is prepared as part of the same publication, so a failed search build
does not publish an otherwise successful index with missing search data.

## Implications

When deploying EDB, you normally want to allocate sufficient CPU and I/O
resources so that indexing can keep up with transaction load. Storage must
sustain both transaction traffic and the reads and writes needed to maintain
the indexes. A prototype's successful import does not establish that a different
storage configuration will keep up with the same load.

`BackgroundIndexingConfig`, supplied through `ServiceOptions.indexing` or
`TransactionService::start_with_indexing`, controls the scheduling and pressure
points:

| Setting | Default | Meaning |
| --- | --- | --- |
| `memory_index_threshold_bytes` | 32 MiB | Schedule indexing when retained novelty exceeds this amount. |
| `memory_index_max_bytes` | 512 MiB | Apply write backpressure when retained novelty exceeds this amount. |

The threshold must be positive and no greater than the maximum. These byte
counts are deterministic estimates of the retained datoms, values and recent
index structures, not allocator RSS or a whole-process memory limit. The live
and frozen portions both count toward pressure until publication.

If background indexing cannot keep up with the growth of the memory index,
EDB parks fresh transaction work while continuing lease renewal and prioritizing
indexing. The bounded submission queue can then fill and reject new submissions
as busy. Committed idempotency retries that reach the writer can still reconcile
their receipts. Raising the maximum allows more retained work; it does not make
indexing catch up.

`TransactionService::background_indexing_stats()` and the corresponding
`TransactionClient` method report the committed target, published indexed basis,
live and frozen byte/datom counts, pending AVET/SAVET projections, job counts,
backpressure and failures. Observe these together with foreground latency and
storage activity. For CPU preparation parallelism and search-build limits, see
[maintenance controls](../08_operations/04_maintenance_controls.md).

### Requesting indexing and waiting for publication

Ordinary indexing is automatic. Schema changes affecting AVET/SAVET membership
also request publication even below the ordinary byte threshold. To explicitly
request indexing through the currently observed committed frontier:

```rust
use edb_core::{Connection, DatabaseValue, SemanticError};
use std::time::Duration;

fn indexed_value(connection: &Connection) -> Result<DatabaseValue, SemanticError> {
    let request = connection.request_index()?;
    connection.sync_index(request.target_t, Duration::from_secs(30))
}
```

`request_index` performs bounded in-memory coordination with the attached
transaction service and returns an `IndexRequest { target_t, scheduled }`.
Queued, uncommitted transactions are excluded from the target. Requests
coalesce: `scheduled == false` means existing demand or a publication already
covers the target, not necessarily that indexing has completed.

`sync_index` waits until the observed indexed basis reaches the target and
AVET/SAVET additions and removals relevant through that target are ready. Later commits do
not extend the requested target. Waiting does not itself request indexing;
dropping the request receipt or timing out does not cancel shared work. Service
shutdown or failure can prevent completion.

A connection needs an attached local `TransactionClient` to request indexing.
A writer-independent read connection, including one using only the local
transaction socket, returns `connection/no-writer` for this operation. It can
still observe index publication. See [connections](../07_peer_api/00_connections.md)
for connection construction and synchronization.

## When Incremental Work Becomes Broad

Some operations require work beyond merging a small recent prefix:

1. Excision targets datoms matching a predicate and must remove them regardless
   of which tier or segment they live in. EDB rewrites retained history into a
   new generation and rebuilds its indexes. This is broad maintenance work;
   see [administration](../08_operations/01_administration.md).
2. Adding AVET/SAVET coverage requires processing the extant values for that
   attribute. EDB persists bounded backfill progress against a frozen source;
   committing `:db/index` or `:db/unique` does not by itself mean that physical
   AVET and SAVET coverage is ready. Removing coverage also requires projection
   work.
3. Fulltext indexing adds tokenization, sorting and search-page writes. A
   compatible previous attachment permits incremental updates; building an
   attachment without one scans the applicable fulltext history. Build limits
   govern admission and spill work, not a sublinear runtime promise. See
   [fulltext search](../05_query_and_pull/03_fulltext.md).
