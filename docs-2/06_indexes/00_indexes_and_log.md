# Ordered indexes and the transaction log

Any system that accumulates data and offers the ability to ask questions about
that data will need an efficient way to retrieve specific information. In
databases, that takes the form of indexes. Indexes afford the database access to
the underlying data in a format that is optimized for read-time access. A
covering index is one that contains actual data values, not just pointers to
where those values can be found.

EDB maintains six covering indexes that contain ordered sets of datoms. Each
index retains and returns all six datom fields: `[e a v t s added?]`. Index names
reflect the leading sort components. E, A, V, and S sort ascending,
while T sorts descending. In the four source-independent orders, S follows T;
in SAEVT and SAVET, S leads. The final `added?` boolean sorts `true` before
`false`; stored-value representation breaks any remaining ties:

| Index | Sort order | Contains |
| --- | --- | --- |
| EAVT | entity / attribute / value / tx / source / added? | all datoms in the selected view |
| AEVT | attribute / entity / value / tx / source / added? | all datoms in the selected view |
| AVET | attribute / value / entity / tx / source / added? | `:db/unique` or `:db/index` attributes whose projection is ready |
| VAET | value / attribute / entity / tx / source / added? | `:db.type/ref` attributes |
| SAEVT | source / attribute / entity / value / tx / added? | all datoms in the selected view |
| SAVET | source / attribute / value / entity / tx / added? | the same ready indexed or unique attributes as AVET |

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
  and observing AVET/SAVET readiness.
- [Index Pull](03_index_pull.md): projecting entities in index order.
- [Index APIs](04_index_apis.md): exact prefixes, seeks and value ranges.
- [Reverse seek](05_rseek_datoms.md): traversing backward from an index boundary.

Current and history views use their respective covering trees plus the captured
recent information. History exposes assertions and retractions; current views
apply the shared record's visibility rules at the selected time. An E/A/V
retraction clears contributions from every source before source restrictions or
filters are applied. Indexing can
advance later without changing an already captured value.

## Cursor and range boundaries

`DatabaseValue::scan_cursor(order)` opens a lazy full ordered scan.
`prefix_cursor(&IndexPrefix)` restricts the selected prefix; `seek_cursor` and
`reverse_seek_cursor` begin at an `IndexBoundary` and continue beyond its supplied
components. A seek position is not an implicit prefix restriction.
`datoms` and `datoms_with_prefix` are explicitly materialized conveniences.

Build boundaries with schema-aware helpers such as `avet_boundary`,
`aevt_boundary`, `saevt_boundary`, and `savet_boundary`. Idents, references and lookup refs resolve against the same
immutable value. Partial tuple values are virtual seek positions, not legal
partial stored tuples; see [tuple boundaries](../02_core_concepts/00_database_values.md#partial-tuple-boundaries).
Validate AVET and SAVET readiness with `has_avet`; schema intent and completed
physical backfill are different observations.

Attribute-qualified AVET and SAVET boundaries and seeks reject unindexed or
pending attributes with `index/attribute-not-in-avet` or `index/avet-not-ready`.
The lower-level prefix API differs: an eager in-memory prefix for a known
unindexed attribute returns no matches, while a durable prefix rejects
unindexed or pending attributes with `peer/avet-attribute-not-indexed` or
`peer/avet-not-ready`. Unrestricted AVET and SAVET scans omit unavailable attributes.
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

## Source activity

A source's activity uses the SAEVT range `[source :db/txInstant]`. Every submitted
transaction has one immutable automatic timestamp assertion carrying its source:
`[tx :db/txInstant instant tx source true]`. Fixing S and A leaves the range
ordered by E. Here E is the transaction entity, whose IDs increase with logical
transaction order. Reverse traversal therefore yields the newest transactions
first; timestamps supply the displayed dates.

Use `saevt_boundary(IndexComponents::Two(source, attribute))` with
`reverse_seek_cursor`, stopping when either source or attribute changes. Each
entry's entity identifies the complete transaction for
`LogValue::tx_data(IndexTransaction::Tx(tx))`. Index traversal and log reads
must use the same captured database basis. Paginate by transaction order so
transactions sharing a timestamp remain distinct. Replaced or retracted domain
facts do not remove their transactions from this activity range.

## The log is not an indexed history view

`Connection::log()` or `Peer::log()` captures an immutable endpoint.
`LogValue::tx_range(start, end)` uses an inclusive start and exclusive end;
omitted bounds select the captured range. T, transaction references and instants
have explicit `TimePoint` representations. A later commit does not extend this
cursor. `tx_data` selects an exact transaction.

Log transactions retain additions and retractions with their sources even when `noHistory`
consolidation omitted facts from indexes. Excision can change retained generation;
the log is not an exception to that policy or a storage retention pin. Traversal
authenticates pages and keeps one decoded transaction plus navigation state;
caller-retained events and large legal transactions still cost memory.

Use [query log sources](../05_query_and_pull/00_queries.md#transaction-logs-and-provenance)
to join provenance, [change consumers](../07_peer_api/03_change_consumers.md) for
acknowledged replay, and the optional [local log cache](../09_optional/02_log_cache.md)
for shared cache configuration and counter meanings.
