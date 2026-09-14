# Persistent native log cache

`PostgresConnectionConfig::with_ssd_cache` enables the same disposable local
cache for immutable tree, log and other scoped read objects. `Peer::log()` and
its `tx_range`/`tx_data` reads need no separate cache configuration. SSD
hardware is not required; use a supported private local filesystem.

A captured log retains its immutable endpoint; storage retention uses the
configured GC grace, not an indefinite per-reader pin. Transaction
membership is authenticated through that root's pages and links, not through
mutable SQL transaction rows or cache filenames. A new peer still opens through
PostgreSQL authority, but a warm retained log can read entirely from authenticated
local objects. Cache possession is neither authorization nor an offline database.

The namespace includes access configuration, lineage, excision generation and
format. New generations cannot read old-generation entries by accident.
Already-held old values may continue reading their original graph.
`Peer::purge_ssd_generation` removes recognized entries in that cache namespace;
it does not erase held values, backups, other cache directories or filesystem
remnants. A retained old value can populate its cache again after a purge.

Corrupt, unavailable, disabled or capacity-rejected cache entries fall back to
PostgreSQL. An authoritative object that also fails authentication is an error,
not an empty transaction. Entry/physical-byte limits apply to all objects in the
shared directory together. Cache admission is per object; a chunked transaction
can have a mixture of cached and uncached objects.

Log traversal keeps at most one decoded transaction and logarithmically many
forward page anchors beyond the captured snapshot and caller-retained results.
It does not restart its seek for each transaction. Navigation and decoding
have format bounds, but a large transaction still requires correspondingly
large memory. Physical and canonical buffers can coexist. Cache limits are
accounted residency, not an RSS ceiling or total query budget. Integer-bounded
cursor construction performs no payload reads; instant bounds may consult the
captured transaction-time index.

## Counters

`LogCursorStats::payload_bytes_read` counts successfully decoded transaction
bytes: the entry header and canonical datoms.
`postgres_payload_bytes_read` counts actual canonical bytes fetched from
PostgreSQL for the record lookup, including navigation pages and entry/chunk
envelopes. These counters have different scopes and need not be equal on a cold
read. They are not compressed physical bytes or wire traffic.

`cache_hits` counts a transaction only when every immutable object read for its
lookup came from SSD. A mixed hit/miss lookup is not a complete hit, but still
reports only the PostgreSQL bytes actually fetched. Failed lookups increment
`range_reads` without adding successful payload/transaction counts.
Repository-backed offline reads report neither PostgreSQL bytes nor SSD hits;
their file I/O is not mislabeled as a live cache.

Use `OperationContext` for actual SQL calls/result-cell bytes and
`Peer::ssd_cache_stats()` for shared persistent-cache physical I/O, corruption,
admission and residency. Those broader observations may include work outside one
log cursor. Warm reads can eliminate all lookup SQL when all required objects
fit, not just the transaction payload transfer.

## Measuring the benefit

Use a fresh private cache and one unchanged captured log. Measure cold iteration,
repeat iteration and a new-peer reopen separately, consume/check the same data,
then include result drop and cleanup in the complete-path timer. Record cache
limits, source size, SQL/cache counters and PostgreSQL/OS cache conditions.
A cache-disabled or oversized-object run is a useful comparison, not an error
to hide by silently raising capacity.

See [application cache configuration](00_local_cache.md)
and [offline backup reads](../08_operations/05_backup_reads.md).
