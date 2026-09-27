# Ordered indexes and the transaction log

Any system that accumulates data and offers the ability to ask questions about
that data will need an efficient way to retrieve specific information. In
databases, that takes the form of indexes. Indexes afford the database access to
the underlying data in a format that is optimized for read-time access. A
covering index is one that contains actual data values, not just pointers to
where those values can be found.

EDB maintains four covering indexes that contain ordered sets of datoms. Each
of these indexes is named based on the sort order used. E, A, and V are sorted
in ascending order, while T is sorted in descending order:

| Index | Sort order | Contains |
| --- | --- | --- |
| EAVT | entity / attribute / value / tx | all datoms in the selected view |
| AEVT | attribute / entity / value / tx | all datoms in the selected view |
| AVET | attribute / value / entity / tx | `:db/unique` or `:db/index` attributes whose projection is ready |
| VAET | value / attribute / entity / tx | `:db.type/ref` attributes |

EDB indexes are used behind the scenes in
[Query](../05_query_and_pull/00_queries.md), the
[Entity API](../05_query_and_pull/02_pull_and_entities.md), and
[Pull](../05_query_and_pull/06_pull_reference.md). For access patterns that
aren't well suited for query, EDB also provides [index APIs](04_index_apis.md)
that offer the ability to retrieve data directly from indexes. Most application
reads should use Datalog or Pull; raw index cursors are useful when order, range
traversal or integration needs an explicit datom stream.

This section documents what EDB's indexes are, how they are maintained, and how
to access them:

- [Index model](01_index_model.md): each ordering, its uses and its implementation.
- [Background indexing](02_background_indexing.md): maintaining covering trees
  and observing AVET readiness.
- [Index Pull](03_index_pull.md): projecting entities in index order.
- [Index APIs](04_index_apis.md): exact prefixes, seeks and value ranges.
- [Reverse seek](05_rseek_datoms.md): traversing backward from an index boundary.

Current and history views use their respective covering trees plus the captured
recent information. History exposes assertions and retractions; current views
collapse them according to the supplied time window and filters. Indexing can
advance later without changing an already captured value.

## Cursor and range boundaries

`DatabaseValue::scan_cursor(order)` opens a lazy full ordered scan.
`prefix_cursor(&IndexPrefix)` restricts the selected prefix; `seek_cursor` and
`reverse_seek_cursor` begin at an `IndexBoundary` and continue beyond its supplied
components. A seek position is not an implicit prefix restriction.
`datoms` and `datoms_with_prefix` are explicitly materialized conveniences.

Build boundaries with schema-aware helpers such as `avet_boundary` and
`aevt_boundary`. Idents, references and lookup refs resolve against the same
immutable value. Partial tuple values are virtual seek positions, not legal
partial stored tuples; see [tuple boundaries](../02_core_concepts/00_database_values.md#partial-tuple-boundaries).
Validate AVET readiness with `has_avet`; schema intent and completed physical
backfill are different observations.

Attribute-qualified AVET boundaries and seeks reject unindexed or pending
attributes with `index/attribute-not-in-avet` or `index/avet-not-ready`.
The lower-level prefix API differs: an eager in-memory prefix for a known
unindexed attribute returns no matches, while a durable prefix rejects
unindexed or pending attributes with `peer/avet-attribute-not-indexed` or
`peer/avet-not-ready`. Unrestricted AVET scans omit unavailable attributes.
See [readiness and database views](04_index_apis.md#readiness-and-database-views).

Consume fallible cursors and handle errors. Construction does not imply the
future traversal is I/O-free, and an error ends that cursor. Cache misses can
load an entire admitted node; cache accounting is neither RSS nor a total read
budget. Measure consumption, not just construction.

## Ordered Pull

`IndexPullOptions::new(start, selector)` traverses one AVET or AEVT attribute
range and projects each selected datom lazily. AVET pulls the entity component;
AEVT reference traversal pulls the referenced entity. It stops when the attribute
changes, not merely when the supplied second component changes. Repeated
references may yield repeated projections; there is no hidden entity deduplication.

Options select direction, offset and an optional result limit; the native default
is unlimited. Limit zero validates without visiting datoms. Use
`index_pull_with_control` for cooperative cancellation across traversal and Pull.
Raw history is not a point-in-time entity projection. See
[index Pull](03_index_pull.md) and
[Pull](../05_query_and_pull/02_pull_and_entities.md).

## The log is not an indexed history view

`Connection::log()` or `Peer::log()` captures an immutable endpoint.
`LogValue::tx_range(start, end)` uses an inclusive start and exclusive end;
omitted bounds select the captured range. T, transaction references and instants
have explicit `TimePoint` representations. A later commit does not extend this
cursor. `tx_data` selects an exact transaction.

Log transactions retain additions and retractions even when `noHistory`
consolidation omitted facts from indexes. Excision can change retained generation;
the log is not an exception to that policy or a storage retention pin. Traversal
authenticates pages and keeps one decoded transaction plus navigation state;
caller-retained events and large legal transactions still cost memory.

Use [query log sources](../05_query_and_pull/00_queries.md#transaction-logs-and-provenance)
to join provenance, [change consumers](../07_peer_api/03_change_consumers.md) for
acknowledged replay, and the optional [local log cache](../09_optional/02_log_cache.md)
for shared cache configuration and counter meanings.
