# Capacity Planning

This page covers capacity planning for EDB: transactor and peer memory, data
imports, transaction performance, indexing, PostgreSQL and storage reclamation.
Counter names refer to EDB's native statistics and optional
[operational events](03_observability.md). These counters describe work and
accounted bytes; measure process RSS, CPU and storage utilization separately.

## Transactor Memory

Transactor memory is used for three key things:

- The memory index holds transaction data that has been logged durably but not
  yet incorporated into the published durable indexes.
- The decoded tree-node cache caches recently used portions of database indexes.
- Transaction assessment and transaction functions need working memory.

Leave additional room for resident schema and tree roots, admission queues,
transaction reports, background index preparation, fulltext builds, maintenance,
transport buffers and the process itself. The frozen prefix being indexed and
newer live transactions coexist until publication.

| Memory use | What controls it | What the control does not bound |
| --- | --- | --- |
| Recent committed data, both live and frozen | Index threshold and maximum | Allocator RSS, old values retained by callers |
| Decoded tree-node cache | Entry and accounted-byte limits | Resident roots/schema, nodes still held by active readers |
| Transactions and functions | Transaction/read/program admission limits | All native allocations or arbitrary embedding callbacks |
| Indexing and maintenance | Preparation lanes and operation-specific limits | Whole-process peak memory or completion time |
| Requests and reports | Service admission queue; application consumption | Bytes in every transport, result or subscriber queue |

The following [transactor options](07_transactor_reference.md#configuring-options)
determine the principal memory-index and cache settings:

| Setting | Default | Description |
| --- | --- | --- |
| `--index-threshold-bytes` | 33,554,432 (32 MiB) | Schedule ordinary indexing when accounted recent bytes exceed this value |
| `--index-max-bytes` | 536,870,912 (512 MiB) | Apply back pressure when accounted recent bytes exceed this value |
| `--tree-cache-entries` | 4,096 | Maximum retained writer cache entries |
| `--tree-cache-bytes` | 67,108,864 (64 MiB) | Maximum accounted bytes retained by the writer cache |

Both index comparisons are strict `>` comparisons. Equality alone does not
trigger them. The threshold must be positive and no greater than the maximum.
Recent-memory accounting includes retained datoms, canonical value bytes and
recent-index structures. It is not an allocator measurement or a hard cap on
all memory allocated by a transaction. Cache accounting is also a representation
measure, with both entry and byte limits enforced.

EDB is a native Rust process. Set process/container memory using your supervisor,
and allow room beyond the index maximum and cache budget. There is no automatic
1 GiB heap allocation or JVM `Xmx`/`Xms` option. After
[preparing storage and a private endpoint directory](07_transactor_reference.md#preparing-storage),
the normal command uses the defaults:

```sh
target/debug/edb transactor --database application \
  --endpoint "$endpoint_dir/transactor.sock"
```

The actual amount of memory used by the memory index depends on recent
transaction history. High write volume accumulates more novelty while indexing
runs. Under low write volume, a tail can remain below the scheduling threshold;
crossing the threshold schedules work but does not instantly release memory.
An admitted commit can carry the accounted total past a pressure point, so the
maximum must not be treated as an exact peak allocation. Captured old values
and unconsumed transaction reports can retain data after the writer publishes
new indexes.

### Prefer Memory Index Defaults

Background indexing automatically manages the memory index. Start with the
default settings and measure your load before changing them:

```sh
target/debug/edb transactor --database application \
  --endpoint "$endpoint_dir/transactor.sock" \
  --index-threshold-bytes 33554432 --index-max-bytes 536870912
```

Rust applications configure `ServiceOptions.indexing` with
`BackgroundIndexingConfig`. Raising the maximum permits a larger recent tail;
it does not make indexing catch up. Lower settings can be useful in a constrained
development process, but must leave indexing enough opportunity to keep pace.

### Tuning the Tree Cache

The following scenarios may warrant adjusting the writer cache:

- **Small processes.** If the transactor has a small memory allocation, you may
  want to turn down `--tree-cache-bytes` and `--tree-cache-entries`. The most
  common scenario is development and testing. Leave room for recent data and
  transaction/indexing work as well as the cache.
- **Uniqueness checks.** If a database attribute is unique, transactions must
  check claims against existing values. Keeping the hot index ranges of unique
  attributes in memory can reduce cold reads. Measure cache hits, misses,
  evictions and transaction I/O before increasing the budget. Both cache limits
  apply; increasing bytes alone will not help an entry-limited cache.
- **Transaction functions.** If transaction functions require large working
  sets, you may need to reduce the cache budget to leave space for this activity.
  Reduce the amount of memory your function needs, or eliminate the need for
  the function by adjusting your data model. Persisted programs have their own
  execution limits; those limits are not a process RSS guarantee.

`CapacityLimits` defaults include 100,000 transaction operations, 64 MiB of
transaction bytes, 1,000,000 transaction-read datoms and 64 MiB of transaction-read
bytes, plus program limits. These are separate admission dimensions. The CLI
exposes the writer cache controls, while Rust embedders can configure the full
structure. See [transaction processing](../04_transactions/06_processing_transactions.md).

## Data Imports

There are several things to consider when performing data imports.

### Plan for Back Pressure

The transactor automatically applies back pressure during sustained heavy write
loads such as imports. When accumulated novelty exceeds `--index-max-bytes`
before indexing can finish, the writer parks ordinary fresh work, continues
lease renewal and prioritizes indexing. The admission queue remains bounded;
a genuinely full queue returns Busy with `service/queue-full`. A committed
retry that reaches the worker can still resolve its receipt without extending
the log.

Import jobs should submit asynchronously with bounded concurrency and allow
for indexing pauses. An embedded `TransactionClient::submit` returns a
`TransactionTicket`; wait for its result with a deliberately chosen timeout.
The stock transport's default request deadline is 30 seconds, independently of
how long indexing takes. Tune client and server deadlines together if an import
needs longer waits. A timeout after admission or attempted transmission can
leave an unknown outcome. Reconcile with the **same request key and content**;
do not assume a timeout rolled back the transaction.

Back-pressure events can be expected during a controlled import, but distinguish
them from indexing failures, unavailable writers and storage errors. Follow
published index progress and eventual completion instead of ignoring every
warning. Successful indexing reduces the covered backlog; it does not establish
a fixed transaction latency afterward.

### Organizing Import Transactions

Two practices help make imports efficient: pipelining and batching.

- To pipeline transactions, send more than one transaction at a time. It is
  still important to wait on every result. Bound the number outstanding, for
  example with a semaphore or a fixed queue of transaction tickets, and retain
  each request's key and content until its outcome is resolved.
- If the groupings of datoms into transactions during an import do not have
  semantic significance for your data, batch data into transaction sizes that
  fit your workload. An atomic unit must still fit transaction, read, program
  and transport limits. Large values and function expansion make a datom count
  alone an inadequate size budget.

The stock service defaults to 64 queued requests and four in-flight transport
workers. `--max-in-flight` accepts 1–256; `--queue-capacity` must be positive.
These limits are different: queue capacity is a request count, not a memory-byte
budget. The default maximum transport frame is 67,108,912 bytes, including
framing; it is separate from transaction admission. Start with a small bounded
pipeline and representative batches, then compare throughput, foreground latency,
queue rejections and index backlog. There is no universal optimal transaction
size or pipeline depth.

Drain or remove transaction-report subscriptions when you no longer need them.
The service's report subscription channel is not bounded by the admission queue;
unconsumed reports retain payloads and immutable database values. Application
result retention can consume memory even when indexing is keeping up.

### Storage Size and Write Throughput

Consider temporarily provisioning more CPU, memory or PostgreSQL I/O capacity
for a large import than you will need for its subsequent read workload. Storage
must sustain transaction object writes and guarded publication, index reads and
writes, and PostgreSQL's own durable storage work. Measure the limiting resource
before increasing capacity; additional import concurrency cannot create parallel
transaction assessment or publication for one logical database.

### Clean up After Import

After a large import, publish the remaining index tail with
[an explicit indexing request](#indexing) or administrative consolidation. Then
use [garbage collection](#garbage-collection-for-live-databases) to reclaim eligible
unowned storage. Index completion is not GC completion, and neither operation
removes canonical history simply because the import has finished. Retention
must still cover any readers and backup copies using older values.

## Peer Memory

Peers need memory for three things:

- Their recent in-memory index for the captured database value.
- Their own decoded tree-node cache, resident tree roots and schema metadata.
- Application use, including query results, cursors, reports and retained values.

Budget recent data for every connected database. The writer's 512 MiB default
is a pressure point in its accounting, not a peer process memory reservation or
an upper bound on all snapshots held by an application. Ordinary readers accept
the writer's authenticated recent window; explicit lower reader admission limits
can reject a capture and do not shrink already committed data.

For example, a process allocated 4 GiB might choose a 256 MiB decoded-cache
budget for one connection. It must still leave enough memory for the recent
tail, roots/schema, active readers, other connections and the application.
That cache setting alone does not establish how much of the 4 GiB remains free.

Configure the cache when opening the connection, rather than setting a global
heap percentage:

```rust
use edb_core::{Connection, PostgresConnectionConfig, SemanticError};

fn open_peer(config: PostgresConnectionConfig) -> Result<Connection, SemanticError> {
    Connection::connect_configured_with_cache_limits(
        config,
        "application",
        4096,
        256 * 1024 * 1024,
    )
}
```

The constructor's entry and byte limits are independent; zero disables cache
retention. The simpler `connect_configured(config, name, cache_capacity)` derives
its byte budget as `cache_capacity * 512 KiB` with saturating arithmetic. Values
captured by one reader share its cache and storage resource, while independently
opened connections have their own budgets. Holding a decoded node outside the
cache can keep it alive after cache eviction. See
[connections](../07_peer_api/00_connections.md).

### Small Databases

If your database is very small, a cache large enough to retain its working set
can avoid repeated cold tree reads. Both the entry count and byte budget must
fit the nodes you use. Fully resident reads can avoid foreground SQL within
the retention policy; opening or refreshing a live value still authenticates
authority through PostgreSQL. Do not infer disconnected startup or a fixed read
performance guarantee from the database fitting in RAM.

## Transaction Performance

EDB transactions enforce the invariants of an EDB database. To improve
transaction performance, you can:

- Give EDB more resources, such as [CPU for index preparation](#index-parallelism),
  [cache capacity](#cache-for-transactions), or [memory](#transactor-memory).
- Do less work, for example [prefer time-local UUIDs](#prefer-squuids-to-random-uuids),
  [queue transactions](#queue-transactions), or [store what you need](#store-what-you-need).

### Cache for Transactions

Transactions must consider the existing state of the database, for example to
enforce uniqueness constraints and cardinality rules. When the needed index
nodes are not resident, EDB reads them on demand. In a basic configuration, cold
reads fetch immutable objects from PostgreSQL. The optional
[local SSD cache](../09_optional/00_local_cache.md) can reuse authenticated block
bytes before falling back to PostgreSQL. Measure whether that reduces your
workload's storage reads and transaction latency.

Set `EDB_SSD_CACHE_DIR` to a dedicated private directory to opt in. Its default
limits are 4,096 entries and 1 GiB of physical file lengths including framing;
`EDB_SSD_CACHE_ENTRIES` and `EDB_SSD_CACHE_BYTES` override them. Processes sharing
a directory must use the same limits. This is a disk budget, separate from the
decoded RAM cache. EDB has no Memcached or Valcache service setting.

### Prefer Squuids to Random UUIDs

Prefer `edb_core::squuid()` or `edb_core::uuid_v7()` when time-local UUIDs suit
your data model.

When a transaction submits a datom whose attribute is unique, EDB must test it
against existing datoms with that attribute. Since many UUID generators are
deliberately random, this test has poor locality, potentially consuming or
thrashing many segments in the tree cache. `squuid()` generates UUIDs with Unix
seconds in the leading bits; `uuid_v7()` uses Unix milliseconds. This gives
temporally nearby identifiers locality in value-ordered indexes without changing
uniqueness semantics.

These functions return a `u128` suitable for `Value::Uuid`. They use random bits
within the time prefix, reveal coarse generation time and promise neither strict
ordering within a clock tick nor monotonicity after clock regression. Retain
the generated identifier when retrying a transaction; generating another UUID
changes the submitted content. A timestamp in a UUID is not authenticated
provenance or a transaction clock. See [data utilities](../07_peer_api/types/11_data_utilities.md).

### Queue Transactions

Many database systems support a variety of workloads, with varying degrees of
priority. For example, a common pattern is a system that has an ongoing load of
user-facing transactions, plus one or more batch jobs that import data from
other systems.

Submit transactions directly where you need a low-latency, synchronous response.
Put other transactions on an application queue and limit their submission rate
so that high-priority transactions have capacity available. EDB's bounded service
queue does not provide a separate priority scheduler. Increasing its size can
increase waiting time and retained request memory without increasing throughput.

### Store What You Need

Because EDB stores time information, a common modeling mistake is to assume
that **everything** temporal belongs in EDB—particularly high-volume,
non-transactional operational logs.

Use EDB as a system-of-record database. Put your records in EDB, and put your
operational logs somewhere else.

If your system stores information whose past values are unimportant, declare
those attributes as [:db/noHistory](../03_schema/02_schema_reference.md#dbnohistory).
This permits older facts to be omitted from retained indexes; it does not
synchronously erase transaction logs, exact receipts, captured values or backups.

## Indexing

On an ongoing basis, EDB performs
[background indexing jobs](../06_indexes/02_background_indexing.md) to incorporate
recent changes into persistent indexes. An ordinary job is triggered when
accounted recent bytes **exceed** `--index-threshold-bytes`. Schema projection
changes and explicit demand can request publication below this threshold.

You can explicitly request indexing with `TransactionClient::request_index()`
or an attached `Connection::request_index()`. The returned `target_t` fixes the
committed frontier; use `Connection::sync_index(target_t, timeout)` to wait for
publication. A read-only/socket-only connection has no maintenance control
endpoint and returns `connection/no-writer` for `request_index`. The separate
administrative command is:

```sh
target/debug/edb consolidate --database application
```

Once indexing has begun, the transactor can continue processing transactions.
The frozen prefix and newly committed suffix both count toward the pressure
maximum until adoption. Once their total **exceeds** `--index-max-bytes`, fresh
work waits for indexing. The limit protects against continuing to accumulate an
unbounded recent tail, but does not bound all peer and transactor memory.

Starting a job does not release its frozen memory charge. Publication removes
only the covered prefix; transactions committed after the job's captured basis
remain charged. Preparation runs in one job slot per service, with selected work
parallelized inside that job. The serialized writer adopts its result only after
checking the current index lineage and publication guards. Ordinary index
adoption preserves newer log data and exact receipts.

Separate recent-tail admission limits can also pause fresh work below the
ordinary indexing threshold. For a nonempty tail, the writer reserves headroom
when retained transactions reach their configured limit, or datoms or accounted
bytes reach half their respective limits. This check follows exact-receipt
lookup, so a matching committed retry can still resolve. When publication can
advance, the writer parks fresh work and requests publication through the current
committed frontier. That demand survives completion of an older indexing job;
the older job cannot clear a request for a newer basis. An empty tail permits
one assessment attempt, whose actual transaction and recent-data limits still
apply; an individually oversized transaction is not parked indefinitely.

To check how well EDB is keeping up with your load, follow
`background_indexing_stats()`:

| Field | Meaning |
| --- | --- |
| `memory_index_bytes` | Live novelty outside the frozen indexing prefix |
| `indexing_bytes` | Frozen prefix currently being indexed |
| `total_bytes` | Both portions, used for threshold and pressure decisions |
| `target_basis_t`, `published_basis_t` | Committed frontier and indexed publication progress |
| `pending_avet_projections` | Remaining AVET projection work |
| `jobs_started`, `jobs_completed`, `jobs_failed` | Preparation/publication progress and failure counts |
| `backpressure_stalls`, `backpressure_rejections` | Writer stalls and full-queue rejections while over the index maximum |

If the total repeatedly rises toward the maximum during normal operation, plan
carefully to avoid back pressure. The arithmetic midpoint of the defaults is
272 MiB; it is merely a possible planning marker, not an automatic alarm or a
validated capacity boundary. A small live count can conceal a large frozen
prefix, so do not monitor `memory_index_bytes` alone. Exhausted indexing failures
close write admission and must be investigated.

Enable `--telemetry-ms 10000` to publish optional operational events. There is
no fixed once-per-minute index alarm: warnings are sampled under the configured
telemetry policy. Correlate index progress with foreground latency, queue
occupancy, cache behavior and PostgreSQL work.

### Index Parallelism

By default, background index preparation uses one lane. If you have high write
volumes and CPUs to spare, you can request up to eight edit-preparation lanes:

```sh
target/debug/edb transactor --database application \
  --endpoint "$endpoint_dir/transactor.sock" --index-workers 8
```

The library setting is `ServiceOptions.index_preparation_parallelism`, range
1–8. Only part of the indexing job is parallelized. Sorting and canonical edit
preparation can overlap; object loading, tree merging and publication remain
ordered. This does not create eight SQL publication connections or promise a
linear speedup. More lanes can increase temporary memory. Compare complete
preparation/adoption time, I/O and foreground latency before raising the setting.

Fulltext and AVET backfills add work beyond the ordinary recent-data merge.
Automatic excision is broader still: its default admission allowance is 512 MiB
of accounted bytes, with replay batches of 256 transactions. Admitted excision
parks fresh writes while maintenance progresses. These are separate controls,
not part of the memory-index maximum; see [maintenance controls](04_maintenance_controls.md).

## Multiple Databases

When you serve multiple databases with EDB, the stock executable runs one
logical database per transactor process. You can colocate those processes on a
host, or embed multiple `TransactionService`s in one Rust process. Similarly,
an application can open connections to one or many databases.

When you serve multiple databases from the same peer or transactor process,
those databases share **and compete for** that process's resources. In particular:

- A process must accommodate the recent data for every database, as well as
  any older captured values it retains. Several databases with high write
  volume can strain or exhaust process memory.
- Independently opened readers and services have their own cache budgets.
  Those budgets add to total memory demand; there is no automatic global
  half-of-RAM cache allocation.
- Transactions, queries and background maintenance compete for CPUs and I/O.
- Allocator pressure and process resource limits affect all colocated databases.
- If the process has an operational problem, all databases in it are impacted.

Index thresholds, preparation workers and admission policies belong to individual
services. EDB does not provide a process-wide scheduler that selects the database
with the largest backlog or guarantees fairness across databases. Account for
the sum of live and frozen work in every service when sizing a shared process.

For these reasons, give large or mission-critical databases dedicated transactor
and application processes when you need independent resource and failure
boundaries. Conversely, sharing resources can be reasonable for low-volume,
non-critical databases. Separate processes still compete for host resources and
shared PostgreSQL storage; process isolation alone does not isolate storage
capacity or failure.

## PostgreSQL

PostgreSQL is EDB's durable storage provider. Plan its CPU, memory, connections,
storage space and I/O capacity together with the transactor and peers. EDB
stores immutable objects and guarded references, rather than relational rows
for each application datom. PostgreSQL executes storage operations; Rust owns
transaction assessment and index construction. See [storage services](06_storage_services.md).

| Workload | Storage demand to budget |
| --- | --- |
| Fresh transactions | Immutable log/receipt objects and guarded reference publication |
| Indexing | Existing-node reads, new tree/search objects and publication |
| Cold peer reads | Authenticated immutable-object reads plus root/authority capture |
| Backup, restore and inspection | Potentially broad traversal, copying or replay |
| Garbage collection | Catalog ownership metadata, namespace enumeration and guarded deletion |

There is no provider-independent conversion from instance size to transactions
per second, or a required ratio between storage spending and transactor spending.
Use representative data and the expected combination of reads, writes and
maintenance to size your deployment.

### Tuning PostgreSQL Writes

Every storage wait contributes to operation latency. However, I/O may not be
your bottleneck. Correlate EDB operation durations and SQL counters with
PostgreSQL activity, lock waits and host storage measurements before increasing
provisioning. EDB's SQL call counters count driver calls, not network round trips
or physical disk writes; payload-byte counters do not measure WAL or filesystem
traffic.

Statement and lock timeouts bound individual SQL work. Configure them together
with connection policy, writer lease renewal and request deadlines. A timeout
does not prove that an attempted publication failed to commit. See
[transport and failure policy](00_deployment.md#transport-and-failure-policy).

### Tuning Write Concurrency

EDB has no `write-concurrency` setting tied to provisioned storage throughput.
Transaction assessment and publication for a database are serialized. The
background index worker has a separate connection for preparation, while peer
reads and administrative operations contribute their own concurrency.

Increasing `--max-in-flight` admits more transport work; it does not create more
transaction writers. Increasing `--index-workers` parallelizes selected CPU work;
it does not parallelize SQL publication. Budget PostgreSQL connections for the
actual services, readers, standby contenders and maintenance tools you run, and
measure contention before adding concurrency.

### Minimum Provisioning

If storage cannot keep up, transaction waits and index backlog grow. A full
submission queue then rejects work, and storage/lease failures can make the
writer unavailable. There is no universal one-second SQL latency boundary or
fixed minimum storage throughput that establishes sufficient capacity. Exercise
the expected foreground workload alongside indexing and maintenance, and leave
capacity for the deployment's recovery and replication work.

### PostgreSQL Reads

If cold reads dominate, inspect decoded-cache misses and physical block reads.
Larger RAM caches or the optional SSD cache can reduce repeated immutable-object
fetches when the workload has reuse. Broad scans can consume I/O even with a
large cache, and a cache does not eliminate live root/authority reads. Compare
the actual working set and measured hit rate before changing cache sizes.

### PostgreSQL Metrics

Combine [EDB events and I/O diagnostics](03_observability.md) with your PostgreSQL
and host monitoring. Watch transaction latency, queue occupancy/rejections,
index publication progress, storage errors, connection usage and storage space.
PostgreSQL replication, WAL retention, physical backups and server failover are
deployment responsibilities. EDB's writer lease and readiness endpoint do not
establish that those systems have spare capacity or meet your recovery target.

## Storage Size

EDB uses PostgreSQL to store log and index objects. The database indexes are
trees of immutable segments. You can model leaf segments as arrays of datoms;
the native representation keeps their components in parallel columns. How many
datoms fit in a segment depends on value types, value sizes and tree construction
limits.

Larger value types generally take up more space than smaller ones. The default
tree targets 256 KiB leaf segments, limits leaves to 4,096 datoms and permits a
larger legal scalar to occupy a larger leaf up to the separate 20 MiB limit.
Directories and roots have their own bounds. These are construction limits,
not observed average segment sizes or guaranteed compression ratios.

EDB uses canonical native encodings. The storage codec retains raw bytes or a
versioned gzip representation when compression, including its envelope, is
smaller. Content identity always authenticates the uncompressed canonical bytes.
The provider's per-object canonical ceiling is 64 MiB even when a compressed
payload is small.

Plan for datoms to be represented in several structures:

- The canonical transaction log records assertions and retractions.
- EAVT and AEVT contain all datoms in their selected views.
- VAET additionally contains datoms of reference type.
- AVET additionally contains attributes with index or unique properties once
  their projection is ready.

Current and history trees are distinct projections of these four orders. A
current assertion can therefore participate in both views; a superseded fact
need not remain in the current view. `noHistory` changes retained-history
coverage. Fulltext attachments, metadata, receipts and ownership information
also use storage. Unchanged subtrees and identical content may be shared, and
each ordering can have different redundancy and compression. A literal fixed
number of physical copies per datom is not a reliable sizing formula.

Load a nontrivial amount of representative data, publish its index tail and
take a backup to establish an initial size. Include realistic value lengths,
references, uniqueness, update/retraction rates, fulltext and retained history.
Measure provider object bytes separately from PostgreSQL's physical tables,
indexes, free space, WAL, replicas and backups. Repeated index publications and
retention grace can make the live store larger than one captured backup.
More similar data generally requires more storage, but extrapolate from measured
growth rather than a guaranteed bytes-per-datom constant.

## Separate Garbage Collection Tool

The `edb gc` command runs catalog-wide collection as a separate administrative
process. Select the physical PostgreSQL database and catalog schema explicitly,
with administrative credentials in `EDB_POSTGRES_URL`:

```sh
target/debug/edb gc --postgres-database edb_storage --catalog-schema public \
  --older-than-seconds 2592000
target/debug/edb gc --postgres-database edb_storage --catalog-schema public \
  --older-than-seconds 2592000 --apply --batches 3
```

The first command previews metadata and collector state without advancing
collection. Apply validates its own current state; the preview is not a frozen
deletion list. The CLI defaults to one bounded apply call. Each call advances
at most 4,096 graph/metadata/object work steps and persists a resumable checkpoint.
One legal object can still be as large as 64 MiB, so step count is not a byte,
SQL-row, wall-time or RSS bound.

Rust operators can attach `MaintenanceControl` through
`PostgresOperator::with_maintenance_control` to pace completed safe batches and
check cancellation. This is cooperative pacing, not a global bandwidth limiter.
There are no JVM read/delete concurrency properties. See
[maintenance pacing](04_maintenance_controls.md#cooperative-maintenance-pacing).

## Garbage Collection for Live Databases

During normal operation, no-longer-owned storage objects accumulate, and you
should periodically run collection to clean up. The appropriate schedule depends
on write volume, retention and maintenance cost; completion is explicit:

- GC works across the entire shared catalog, not just one logical database.
- It seals an epoch, folds ownership additions before removals, protects
  metadata and sweeps eligible objects under guarded authority.
- It checkpoints progress. Repeat the command when
  `GC_PROGRESS cycle_complete=false`; a successful exit or zero deletions does
  not prove cleanup finished.
- A completed sealed cycle does not establish global quiescence. Later
  publications, staging expiry and authorization pruning can create more work.

Collection competes with foreground work for CPU and PostgreSQL resources.
Unchanged shared subgraphs do not need a fresh live-data traversal every cycle,
but a complete cycle walks ownership-index metadata and enumerates namespace
objects, including live objects. It is not a promise of lock-free, read-free or
constant-cost cleanup. Stopping the command stops its work; a later operator
resumes the persisted checkpoint.

The reason obsolete objects are not deleted immediately after a new tree is
published is that not all consumers immediately adopt the latest tree. Held
database values create no storage pins or reader sessions. Normal collection
uses a **30-day age** as its recommended margin, and retention must cover active
read and backup-copy durations. Explicitly shorter ages, including zero for
controlled imports or isolated tests, can invalidate cold reads from older
values. The age applies after a previously owned object loses its last owner,
not from its original upload time. Never-published abandoned uploads rely on
epoch protection rather than that retirement-age promise.

Current roots, exact receipts, protected work and shared owners can retain
objects. GC is not excision of selected facts and does not erase external
backups, replicas, WAL or already copied process memory. Its cutoff is fixed
when a cycle starts; changing the age on a later invocation does not change
the running cycle retroactively.

PostgreSQL also has its own vacuuming and space-reclamation facilities. Deleting
EDB objects and reclaiming reusable SQL space are distinct from returning space
to the operating system. Plan PostgreSQL maintenance alongside EDB collection.
See [retention and reader protection](00_deployment.md#retirement-age-and-reader-protection).

## Garbage Collection for Deleted Databases

When you delete an EDB database with `edb delete`, it loses its active catalog
name and its writer is fenced. New opens are rejected; already captured immutable
values remain readable only within retention. As a separate step, you may later
reclaim eligible storage associated with retired databases.

### GC for Deleted Databases in Development and Testing

For a wholly disposable, isolated fixture, removing and recreating its dedicated
PostgreSQL database or schema is an alternative to walking its EDB objects.
Perform that teardown through PostgreSQL administration, then reinstall EDB
storage. This destroys the entire selected namespace, including every logical
database in it; `edb delete` itself does not drop PostgreSQL tables or schemas.
Use isolation when you want this clean-reset workflow for development or tests.

### GC for Deleted Databases in Production

It may be impossible to delete the underlying storage in production because
other databases still use it. Use the retired database's exact storage ID and
lineage as additional target checks:

```sh
target/debug/edb gc-deleted --storage-id ID --lineage UUID \
  --postgres-database edb_storage --catalog-schema public \
  --older-than-seconds 2592000
target/debug/edb gc-deleted --storage-id ID --lineage UUID \
  --postgres-database edb_storage --catalog-schema public \
  --older-than-seconds 2592000 --apply --batches 10
```

Obtain the retired identity through `edb list-databases --retired` and follow
[database lifecycle](02_database_lifecycle.md#retire-then-reclaim). `gc-deleted`
advances the same catalog-wide collector as `gc`; the checked identity does not
limit its sweep to that database. It preserves live owners and shared content.

Such garbage collection competes for resources with ongoing use. Bound the
number of apply calls per invocation, schedule subsequent calls deliberately,
or use library maintenance pacing. A stopped process can leave committed work;
restart to continue from the checkpoint. Missing or corrupt ownership evidence
fails closed rather than authorizing speculative deletion.

Compared with removing an isolated storage namespace, incremental reclamation
must account for the catalog's ownership structures and object enumeration, and
shared owners and retention can keep objects present after a cycle completes.
`cycle_complete=true` is not a claim that every byte associated with the retired
database has disappeared. Neither command reclaims external PostgreSQL backups,
replicas, application exports or local cache copies.
