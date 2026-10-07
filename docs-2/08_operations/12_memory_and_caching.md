# Memory and Caching

EDB provides a number of caches, all of which are transparent to application
reads. Cached database data is immutable, so a cached value does not become
stale when a new transaction commits. Cache contents and eviction policy are
mutable; retaining cached data does not advance a database value, grant access
to a database, or extend the storage retention period for historical reads.

## Memory Index

Recent datoms are held in the memory index on the transactor and on peers.
This data is required to represent the database's captured basis until it is
incorporated into durable indexes. It cannot be evicted like an optional cached
copy of a durable index node.
The memory index size is [controlled](09_capacity_planning.md#prefer-memory-index-defaults)
by `BackgroundIndexingConfig.memory_index_threshold_bytes` and
`memory_index_max_bytes` on the transactor. The corresponding
[transactor options](07_transactor_reference.md#configuring-options) are
`--index-threshold-bytes` and `--index-max-bytes`, with defaults of 32 MiB and
512 MiB, respectively.

Ordinary indexing is scheduled when accounted recent bytes **exceed** the
threshold. Sustained writes, such as data imports, can accumulate additional
recent data while indexing runs. When the total **exceeds** the maximum, the
writer applies back pressure to fresh work while continuing lease renewal and
prioritizing indexing. Equality alone triggers neither comparison. The maximum
is a pressure point, not an exact peak allocation: an admitted commit can carry
the total past it. See [data imports](09_capacity_planning.md#data-imports).

The byte count includes retained datoms, canonical value bytes and recent-index
structures; it is not allocator RSS. Both the frozen prefix being indexed and
the newer live suffix count toward the threshold and maximum. Starting an
indexing job does not release the frozen prefix's charge.

Setting a smaller indexing threshold on a peer cannot make the writer publish
indexes sooner. Peers must be able to handle the recent window given to them
by the writer. Ordinary readers accept that authenticated window. Explicit
reader admission limits can reject a capture that is too large; they cannot
shrink already committed data.

The memory index is managed as follows:

### On the Transactor

- The transactor reconstructs its recent index from the authenticated log and
  durable indexed base before it begins serving the database.
- The transactor incorporates committed transactions as it publishes their
  durable log state. Uncommitted work does not become visible to readers.
- An ordinary indexing job freezes the currently committed prefix for merging
  into durable indexes. Transactions committed after its captured basis remain
  in the live suffix.
- When an indexing job is published, the transactor drops the covered prefix
  from its current recent backlog, as it is now in the durable index. Newer
  transactions remain charged. Old values held by callers can still retain
  their own recent data.

### On the Peer

- A peer builds its recent index from the durable base and authenticated log
  before the call to connect returns.
- A live `Connection` observes publication changes in the background, using
  notifications as hints and durable observation to repair gaps. A lower-level
  `Peer` advances through synchronization. When the indexed base and generation
  match, the reader shares its previous immutable prefix and reads only the
  new log suffix.
- When it observes a changed indexed base or generation, the reader rebuilds
  the bounded recent tail against that base. `Peer::refresh_index()` can adopt
  an index change at the same logical basis; advancing to newer transactions
  uses synchronization.
- Previously returned database values remain immutable. Refreshing a connection
  does not remove the recent data retained by those values, entities, cursors
  or transaction reports.

See [connections](../07_peer_api/00_connections.md) and
[background indexing](../06_indexes/02_background_indexing.md).

## Entity Caching

Data retrieved via `get`, `get_named`, `get_direction` and `touch` on the
[Entity API](../07_peer_api/types/06_entity.md) is cached for the lifetime of
the entity's shared cache. This includes missing values. A clone shares the
cache; constructing another entity for the same ID creates a separate cache.
Numeric and keyword selectors are separate cache keys.

`touch` realizes direct attributes and recursively realizes component entities;
noncomponent references remain lazy. Entities retain their captured database
value, so their cached attributes do not change when the connection advances.
Dropping the final owner releases the cache. Entity caches have no configurable
entry or byte limit: retain entities according to your application's memory
budget.

## Object Cache

EDB readers maintain an LRU cache of decoded immutable tree nodes from database
[indexes](../06_indexes/00_indexes_and_log.md). When a reader needs a node, it looks in
the decoded cache first. If the node is unavailable there, the reader obtains
its immutable bytes from the optional local SSD cache or PostgreSQL, decodes
the node, and offers it to the decoded cache.

Successful lookups make an entry most recently used; inserting or replacing an
entry does the same. The cache has independent entry and accounted-byte limits.
It evicts the least recently used entries until both limits are satisfied.
A node larger than the
byte budget bypasses retention. Zero for either limit disables retention.
Accounting includes the node's retained representation and canonical byte size;
it does not measure total native allocation. Resident roots and schema, recent
data, and nodes still held by active readers are outside this retention budget.

Eviction releases the cache's ownership of a node. Active seeks and cursors
retain shared ownership of the immutable node, so eviction does not invalidate
the values they are reading. Those values may outlive their cache entries.

The writer's cache size is limited by
`CapacityLimits.writer_tree_cache_entries` and `writer_tree_cache_bytes`, exposed
as `--tree-cache-entries` and `--tree-cache-bytes` on the transactor. Their
defaults are 4,096 entries and 64 MiB. Peers can configure both limits when
opening a connection:

```rust
use edb_core::{Connection, PostgresConnectionConfig, SemanticError};

fn open_peer(config: PostgresConnectionConfig) -> Result<Connection, SemanticError> {
    Connection::connect_configured_with_cache_limits(
        config,
        "application",
        4096,
        64 * 1024 * 1024,
    )
}
```

The simpler `Connection::connect_configured(config, name, cache_capacity)`
derives its byte limit as `cache_capacity * 512 KiB`, using saturating arithmetic.
The lower-level `BlockReadConfig` defaults to 256 entries and 64 MiB; these are
not the transactor service's defaults.

Unlike the recent window imposed by committed transactions, the decoded cache
can vary in both configuration and contents across different readers. Captured
values from one reader share its cache and storage resource. Independently
opened readers have independent caches, which automatically adjust over time
to their workloads. Concurrent decoded-cache misses are not coalesced into a
single per-node load; sharing a cache is not a guarantee that simultaneous cold
reads perform only one fetch or decode.

This tree cache is not a general cache of every log or application result.
Entity, prepared-query, program and fulltext caches have their own owners and
lifetimes. See [observability](03_observability.md) for the available counters.

The decoded tree-node cache, decoded-program cache and fulltext page cache
maintain keyed LRU recency without scanning all resident entries on each hit.
Their hash tables and reusable slot buffers can retain capacity after eviction;
slot storage follows peak concurrent occupancy. Their entry and byte counters
describe retained cache contents, not all memory allocated by the cache or
process. Allocator overhead, code, stacks, other caches and application memory
also contribute to process memory. A bounded cache counter does not establish
a bound on process RSS or end-to-end read latency.

## Local SSD Cache

The [local SSD cache](../09_optional/00_local_cache.md) is an optional addition
to an EDB system, providing another level of cache between in-process decoded
nodes and storage. Adding it can reduce the load on PostgreSQL and the latency
associated with populating the decoded cache. Measure the effect for your
workload; disk hits still require authentication and decoding.

Set `EDB_SSD_CACHE_DIR` to a dedicated private directory with mode `0700`.
`EDB_SSD_CACHE_ENTRIES` and `EDB_SSD_CACHE_BYTES` control its limits. Defaults
are 4,096 entries and 1 GiB of physical block-file lengths including framing,
not filesystem allocation. Without the directory setting, SSD caching is
disabled. The Rust equivalent is:

```rust
use edb_core::{PostgresConnectionConfig, SsdCacheConfig, SsdCacheLimits};
use std::path::PathBuf;

fn with_local_cache(
    config: PostgresConnectionConfig,
    private_directory: PathBuf,
) -> PostgresConnectionConfig {
    config.with_ssd_cache(SsdCacheConfig {
        directory: private_directory,
        limits: SsdCacheLimits::default(),
    })
}
```

Processes may choose separate cache directories or share one directory with
the same limits. Separate processes coordinate disk operations through its
lock file; a directory's budget covers all its namespaces. Cache names separate
connection and trust configuration, database lineage, excision generation and
format. Use a dedicated directory rather than mixing EDB cache files with other
applications' files.

On a scoped immutable-block read, a miss falls back to PostgreSQL and the reader
offers successfully read canonical bytes to the SSD cache. The same read path
serves writer-side snapshots. This does not prepopulate every newly written
index or log segment, and it does not place every storage read through SSD.
Runtime contention, corruption and disk errors bypass the cache and fall back
to PostgreSQL. Unsafe directory configuration or conflicting shared limits can
fail opening; see the local cache reference for recovery details.

The directory is chosen when opening the configured reader. EDB has no
Memcached server list, SASL cache credentials or automatic cache-node discovery.
To change the cache directory, open readers with the new configuration.

### Access and Retention

New peers still authorize and authenticate their root through PostgreSQL.
An SSD hit supplies hash-authenticated canonical bytes under the captured
reader's protection; cache names are not authorization tokens. Cached blocks
do not provide disconnected live startup or indefinitely available historical
reads.

Cache files are private but not encrypted. Excision does not automatically
remove existing local copies, backups or RAM values. Use
`Connection::purge_ssd_generation(generation)` for recognized entries in the
configured namespace, and retire obsolete cache roots under your retention
policy. A purge can return `false` on a busy or error path; it is not secure
erasure or a purge of other differently configured namespaces.

### Cache Metrics

Use [observability](03_observability.md) to distinguish cache misses, storage
reads, eviction and retained memory:

| Observation | What it reports |
| --- | --- |
| `Connection::cache_stats()` / `Peer::cache_stats()` | Decoded-node hits, misses, evictions, current and peak retained entries/bytes, and oversized or disabled-retention bypasses |
| `Connection::ssd_cache_stats()` / `Peer::ssd_cache_stats()` | SSD hits, misses, puts, evictions, purges, corruption and bypass counters, physical/canonical bytes and last observed shared-directory occupancy |
| `BackgroundIndexingStats` | Live `memory_index_bytes`, frozen `indexing_bytes`, combined `total_bytes`, publication progress and back-pressure counters |
| Operation-local `ReadIoStats` | Cache-tier accesses, hits, misses, errors, bytes and elapsed/minimum/maximum read durations when diagnostics are enabled |

The active block-reader tiers are `DecodedNode`, `LocalDisk` and
`PostgresBlock`. The `Inflight` enum variant does not establish an active
coalescing cache in this path. A decoded miss can be satisfied by SSD; it is
not necessarily a PostgreSQL read. Tier durations can overlap, and returned
payload byte counts are not network traffic or filesystem block I/O. SSD
`inventory_complete` distinguishes a complete occupancy inventory from one
with recognized malformed entries that prevents writes and purge.

## Usage Notes

- All forms of database-data caching are transparent to application reads.
- Start with the ordinary memory-index settings and an appropriate decoded
  cache budget. SSD caching is optional and requires its own configuration.
  The [capacity planning](09_capacity_planning.md) documentation guides sizing.
- The transactor benefits from cached index reads in ordinary transaction
  processing. For example, it uses database indexes for cardinality and
  uniqueness checks.
- Because the decoded cache lives in your application's address space, it
  competes with your application for available memory. Take this into account
  when planning cache budgets and native process or container RAM. Include
  recent data, roots/schema, query results and retained old values separately.
- Independent RAM and SSD configuration supports task-specific cache strategies.
  For example, OLTP and analytical readers can have different RAM budgets and
  separate local SSD directories. A support engineer's machine can use its
  own local cache while reading the same database, with the appropriate
  PostgreSQL access. A local directory is not a distributed cache cluster.
