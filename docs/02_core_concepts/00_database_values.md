# Composing immutable reads

Capture a `DatabaseValue` once and pass it through application functions. A later
commit does not change that value. These APIs reuse the existing query, index,
entity and native-program engines; none implicitly refreshes a connection.

## Captured-value retention

Values and cursors retain immutable roots in memory. Capturing, cloning and
dropping them perform no storage writes and create no reader sessions. Normal
garbage collection retains retired objects for 30 days; choose a retention age
that covers read and copy durations. An explicitly shorter age can invalidate
cold reads through a held value. Already resident or cached bytes are not erased.

Serialized references carry a bounded value descriptor, not a pointer to an old
publication wrapper. Reopening checks its canonical log/metadata and published
index authorization, then constructs the value in memory. A peer needs SELECT
on immutable objects and references; reads require no write privileges.

Serialized references also require current-generation authorization. Obsolete
index provenance is eligible for pruning only after its publication grace and
when no published value, receipt, or other durable owner still needs that index.
The final publication checks the same ownership proof against concurrent
publication. Logical removal and later physical collection are distinct steps.

## Mixed sources, lazy projections

`QueryEngine::sequence_sources` takes the same `QueryDataSource` arguments as
`execute_sources`: named database values, ordinary tuples and captured logs.
`sequence_sources_with_extensions` additionally accepts native callbacks.
The original database-only `sequence` APIs remain available.

Sequences prepare joins and aggregates eagerly. Pull and its transforms run only
for consumed rows. `remaining_rows()` does not run projections; an error ends the
iterator. The deadline and work budget span preparation and consumption, rather
than restarting for each row. This is not streaming execution of arbitrary joins.

For an EDN query already parsed and bound to arguments, use
`BoundEdnQuery::sequence(&control, extensions)`. Its source values are captured in
the same way as eager execution. See
[query sources](../05_query_and_pull/00_queries.md).

## Entity identity is not state equality

`Entity` implements `Eq` and `Hash` by database origin and entity ID. The lazy
attribute cache and time view are deliberately irrelevant. `entity.identity()`
returns a cheap `EntityIdentity` token for set/map keys without retaining the
database or reading attributes.

Durable origins use the authenticated lineage, not a catalog name, server address
or connection. A reopened or recovered value from that lineage recognizes the
same entity. Backups/restores which retain that lineage retain entity identity;
this says nothing about equality of their possibly different states. Independent
in-memory databases are distinct even if their contents are identical. Clones and
speculative descendants retain their origin.

To compare states, compare selected Pull/query results. To distinguish committed
time views in a cache key, include a `SnapshotKey`. An identity token is not a
snapshot reference, a permission to read, or a promise that the entity still exists.

## Physical index readiness

`db.has_avet(&AttributeName::Ident(...))` (or `AttributeName::Id`) reports whether
that captured value can use AVET for the attribute. This differs from schema intent:
an index may have been requested but its background backfill is not yet published.
Known unindexed attributes return `false`; unknown attributes return an error.
Acquire a newer value to observe publication. An already captured pending value
does not silently change its answer.

## Partial tuple boundaries

`db.avet_boundary(...)` accepts a partial tuple as a virtual seek position. A
shorter tuple sorts before longer tuples sharing its prefix. This is useful for
forward and reverse traversal of compound indexes; the cursor direction retains
its normal inclusive/exclusive behavior. Nil slots and reference/lookup-ref slots
are normalized with the same schema-aware rules as full boundaries.

A virtual boundary is not a stored value. Transactions still require valid full
tuple arity; an overlong boundary or incorrectly typed slot is rejected.

## Calling stored native functions

`db.invoke(identifier, &arguments, InvokeControl::default())` resolves `:db/fn`
inside that exact value and loads its authenticated native program. The identifier
may be an entity ID, ident or lookup ref. Arguments use the existing `RuntimeValue`
domain and the returned `ProgramOutput` retains the stored role: query rows,
predicate value or transaction forms. Dual predicates need an explicit `InvokeRole`.

Returned transaction forms are inert data. Invocation neither expands nested
transaction calls nor transacts their output. Use the ordinary transaction API if
you intend to submit them, where current-basis validation happens again. No Clojure
or JVM code is executed. A standalone eager value without retained program content
cannot fetch a program from an ambient connection.

`InvokeControl` combines existing interpreter fuel/allocation/output controls with
logical read-datom/read-byte limits and an optional deadline. Logical read limits
count visible data retained by the invocation, including binding resolution;
fuel also bounds raw scan work on candidates hidden by filters. Neither is an RSS
measurement or allocator limit. Cancellation is cooperative;
it cannot forcibly preempt an in-flight SQL call or an application-supplied filter.
See [native programs](../04_transactions/01_persisted_programs.md).
