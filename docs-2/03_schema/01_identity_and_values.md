# Value and identity rules

Value equality determines grouping and identity throughout EDB: indexes,
unique keys, lookup refs and query joins use the same logical comparison.
Stored-fact equality additionally distinguishes the scale of top-level decimal
values. These rules explain when two representations identify the same key and
when asserting a value changes the stored information.

Source does not change value equality, unique keys or lookup-ref identity.
Different sources can contribute the same stored value without adding distinct
current values for cardinality.

For entity identifiers, idents, upserts and lookup-ref syntax, see
[identity and uniqueness](05_identity_and_uniqueness.md). The
[schema reference](02_schema_reference.md) defines value types and tuple shapes.

## Immutable facts and transaction coordinates

A datom has six fields: entity (`entity`), attribute (`attribute`), value
(`value`), transaction (`tx`), source (`source`) and assertion/retraction (`added`).
`added: true` asserts a fact; `added: false` retracts it. Transactions produce new
immutable database values. A previously captured value keeps its own view of
those facts.

Source is an ordinary entity ID supplied explicitly for each assertion or
retraction. One transaction can contain statements from several sources.
Datom equality includes all six fields; asserting the same EAV from a different
source preserves a distinct contribution. A retraction changes the current fact
regardless of its original source and records the retracting source, leaving the
earlier assertion's attribution intact.

Logical transaction time and the transaction entity ID are distinct coordinates.
`Datom::tx` is the transaction entity ID. `t_to_tx(t)` constructs that ID in the
transaction partition, and `tx_to_t(tx)` recovers logical time, rejecting IDs
outside that partition. Neither coordinate is a wall-clock timestamp; transaction
time in milliseconds is represented by the transaction's `:db/txInstant` fact.

Permanent entity IDs encode a partition and a nonnegative entity index.
`eid_to_part` returns the raw partition bits, `eid_to_eidx` returns the entity
index, and `partition_eid` returns the partition's entity ID. These are checked
operations. There are 20 partition bits and a 42-bit entity-index field, whose
high bit is reserved: accepted permanent indexes run from zero through
`2^41 - 1`. `make_eid` constructs a coordinate without issuing or reserving it.
Use transaction tempids to request new entities; see
[partitions](../04_transactions/02_partitions.md).

Index orders group their leading coordinates as E/A/V, A/E/V, A/V/E or V/A/E.
Within the same logical key, transactions sort descending, then sources
ascending, with assertions before retractions at the same transaction and source.
Only after those coordinates does stored-value ordering distinguish decimal
scales. The public six-field datom shape does not imply ordinary field-by-field
ascending order.

## Value equality is shared by indexes and identities

`Value::index_cmp` compares logical values; `Value` equality follows that same
comparison. Numeric variants compare across types by their exact numeric value,
including integer, decimal and binary floating-point representations. Equal
logical values receive equal internal logical hashes. Schema validation still
requires the attribute's declared value type: equality does not make a `Double`
an acceptable assertion for a `Long` attribute.

The main ordering rules are:

- Strings compare lexicographically by UTF-16 code units. This is neither locale
  collation nor Rust's UTF-8 byte order.
- Byte arrays compare by length first, then lexicographically by signed byte
  values. Thus `[255]` sorts before `[0]`, and every one-byte array sorts before
  every two-byte array.
- Tuples compare lexicographically, with `nil` below every value. Their elements
  use logical value comparison; a shorter tuple sorts before its extension.
- Keywords and symbols compare namespace first and then name, using UTF-16
  ordering. An absent namespace sorts before a present one.
- UUIDs compare their high and low 64-bit halves as signed integers. Their
  ordering is not an unsigned comparison of the entire 128-bit value.

For unlike nonnumeric types, EDB uses a fixed rank. The complete rank order is
numbers (including refs), bytes, keywords, symbols, booleans, strings, URIs,
instants, UUIDs, tuples and function content hashes. An attribute's schema
normally limits its values to one type; the broader order matters for generic
value comparison and query values.

### Decimal equality and stored scale

Logical comparison treats `1.0M` and `1.00M` as the same numeric value. They
identify the same unique key and lookup-ref value. Stored-fact equality adds
scale only when both top-level values are BigDecimals: `Value::stored_eq`
distinguishes these two representations, and `Value::stored_cmp` orders equal
magnitudes by scale.

Consequently a cardinality-many decimal attribute can retain both `1.0M` and
`1.00M`, even though logical comparison groups them together. A retraction of
one scale does not retract the other. Cardinality-one replacement can change
the stored scale. This distinction does not recurse into tuples: `[1.0M nil]` and
`[1.00M nil]` compare equal for both logical and stored-fact purposes.

## URI identity and exact input

URI values retain their original spelling but use component comparison for
identity, redundancy, index order and query joins. Scheme and server-host ASCII
case, percent-escape hex case and numeric port padding do not create different
identities. URI normalization is not performed: default ports, dot path segments
and escaped/unescaped characters remain distinct. An equivalent reassertion from
the same source is redundant; a retraction may record the alias spelling supplied
with that new fact.
Previously captured values remain unchanged. Exact retry digests distinguish the
submitted spellings and source declarations even when they identify the same
logical URI.

For native ordered indexes, absent authority sorts before server authority,
which sorts before registry authority. Within server authorities, user info,
host and numeric port are compared separately; registry authorities compare
their raw text with case preserved except for percent-escape hex digits.
Hierarchical URIs compare authority, path and query separately; opaque URIs
compare their scheme-specific part. Fragments compare last. An absent component
sorts before a present one, including an empty component. For the same scheme,
hierarchical values sort before opaque values.

For example, `HTTP://EXAMPLE.COM/x` and `http://example.com/x` identify the same
URI, but `http://example.com/X` is different. `http://example.com/x` also differs
from `http://example.com/x?`: the latter has a present, empty query. Scheme and
host case folding does not extend to path, query, fragment or user info.

URI syntax is validated for scalar assertions and tuple slots. Invalid escapes
such as `%zz` are rejected. Parsing may recognize a registry authority when its
text is not a valid server authority. Validation does not fetch the resource,
perform DNS resolution or convert Unicode hostnames.

URI separators retain exact values, so very long URIs also
consume space in index parent nodes; ordinary parent-size limits still apply.

Current tree and database-root formats include these comparison semantics.
Earlier development databases require fresh databases, not an in-place upgrade.

## Tuple and large-number boundaries

Tuple strings are limited to 256 UTF-16 code units (128 supplementary Unicode
characters such as emoji). This
storage bound is separate from the [native query string functions](../07_peer_api/02_native_computation.md).
Tuple integers use at most 256 bits excluding the sign bit; ordinary BigIntegers
use at most 8192. For a limit of N bits the interval is `[-2^N, 2^N - 1]`.
For example, tuple BigInteger `-2^256` fits; `2^256` and `-2^256 - 1` do not.

Ordinary BigDecimals are limited to 1024 coefficient digits; tuple BigDecimals
are limited to 256. This measures precision, not the number of positions after
the decimal point. Tuple slots cannot contain Float, bytes, function values or
nested tuples. See [tuples](02_schema_reference.md#tuples) for supported tuple
forms and constituent restrictions.

## Bytes cannot identify entities

Byte-content comparison supports indexing and fact equality, but bytes
attributes cannot have either `Unique::Identity` or `Unique::Value`. Schema
admission rejects them with `schema/bytes-cannot-be-unique`. Lookup refs require
a unique attribute, so bytes cannot be lookup-ref keys either. Use another
value type for a domain identity; byte ordering does not extend the permitted
uniqueness domain.

## Idents and schema are derived from information

Idents and attribute descriptors are derived from ordinary database facts.
The resident ident dictionary folds historical `:db/ident` assertions in
transaction order. Retractions do not remove historical names from that
dictionary; a rename leaves the old name available as an alias, and a later
assertion of an old name on another entity repurposes it. Current `:db/ident`
datoms and the alias dictionary therefore answer different questions.

Schema and ident dictionaries are rebuildable projections of that information,
not additional schema facts to maintain yourself. A captured database value
keeps its schema and name-resolution context. Time-filtered views use the
schema of the value from which they were created. See
[changing schema](03_changing_schema.md) for rename, alias and time-view rules.

## Schema configuration is not physical index readiness

An explicit `:db/index` fact is distinct from the AVET membership implied by
uniqueness. Both request value indexing, but installed schema does not make
background physical index work synchronous.

`DatabaseValue::has_avet` reports readiness at that exact captured value. It
does not wait for an index build or refresh the value. An eager database value
has complete in-memory coverage for its logically indexed attributes; a block
value uses its snapshot's coverage. Attribute-qualified AVET boundaries, seeks
and durable prefix cursors reject an unready attribute. An eager prefix for a
known unindexed attribute returns no matches. See
[index API readiness](../06_indexes/04_index_apis.md#readiness-and-database-views)
for the distinctions between these APIs.

Adding uniqueness requires the proposed current values to be unique. If AVET
is not physically ready, immediate enablement is allowed only when the
attribute has no historical values. “No current values” is not the same as
“nothing to backfill”: retracting all values does not satisfy this exception.
Follow the index synchronization and uniqueness procedures in
[changing schema](03_changing_schema.md).

## Composite identity: derive values, supply identity when upserting

A composite tuple is derived from its constituent attributes. Declaring the tuple
`Unique::Identity` makes that derived key unique; it does not make a fresh tempid
implicitly resolve from constituent-only assertions.

For an existing entity with `:item/year` 2026 and `:item/season` `"fall"`, and an
existing source entity `99`:

```edn
;; Explicit composite value selects the existing entity before derivation.
[{:db/id "edb.tx" :db/source 99}
 {:db/id "semester" :db/source 99
  :item/year-season [2026 "fall"]
  :item/label "Updated"}]
```

The `edb.tx` map attributes the generated transaction instant; the `semester`
map attributes its own assertions.

The composite assertion is an **upsert hint**, not a direct stored write. The
constituents still determine the actual tuple. Supplying a nonexistent composite
hint without constituents does not populate the tuple or those constituents.
An existing entity can also be addressed by its entity ID or composite lookup ref.

By contrast, a fresh tempid asserting only the same year and season produces a
derived-key collision and fails indivisibly with `transaction/unique-conflict`.
To update a constituent, first select the existing identity (including with the old
composite hint); the engine then derives the new key and enforces its uniqueness.


## NaN: replace ordinary values, never use as a unique identity

For a nonunique cardinality-one `Float` or `Double`, asserting a finite value over
NaN works in one transaction. The report/history contains both the NaN retraction
and finite assertion at that transaction, both attributed to the replacing
assertion's source. Retained earlier database values still show NaN. Replacing a
finite value with NaN works the same way.

EDB gives NaN stable logical equality for indexing and redundancy: reasserting
NaN with different payload bits from the same source is redundant, and a different
NaN payload can retract the existing NaN. This is a native value-semantic choice,
not IEEE `==` or a promise to preserve arbitrary NaN payload bits.

NaN remains disallowed on attributes with either `Unique::Identity` or
`Unique::Value`; assertions fail with `transaction/nan-cannot-identify`, including
replacement of a previously finite unique key.
