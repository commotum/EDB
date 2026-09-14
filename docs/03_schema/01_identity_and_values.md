# Value and identity rules

## URI identity and exact input

URI values retain their original spelling but use component comparison for
identity, redundancy, index order and query joins. Scheme and server-host ASCII
case, percent-escape hex case and numeric port padding do not create different
identities. URI normalization is not performed: default ports, dot path segments
and escaped/unescaped characters remain distinct. An equivalent reassertion is
redundant; a retraction may record the alias spelling supplied with that new fact.
Previously captured values remain unchanged. Exact retry digests distinguish the
submitted spellings even when they identify the same logical URI.

For native ordered indexes, absent authority sorts before server authority,
which sorts before registry authority. This intentional total-order rule avoids
the source platform's inconsistent mixed-authority fallback without changing
component equality. URI separators retain exact values, so very long URIs also
consume space in index parent nodes; ordinary parent-size limits still apply.

Current tree and database-root formats include these comparison semantics.
Earlier development databases require fresh databases, not an in-place upgrade.

## Tuple and large-number boundaries

Tuple strings are limited to 256 UTF-16 code units (128 supplementary Unicode
characters such as emoji). This
storage bound is separate from the [native query string functions](../07_peer_api/02_native_computation.md).
Tuple integers use at most 256 bits excluding the sign bit; ordinary BigIntegers
use at most 8192. For a limit of N bits the interval is `[-2^N, 2^N - 1]`.

## Composite identity: derive values, supply identity when upserting

A composite tuple is derived from its constituent attributes. Declaring the tuple
`Unique::Identity` makes that derived key unique; it does not make a fresh tempid
implicitly resolve from constituent-only assertions.

For an existing entity with `:item/year` 2026 and `:item/season` `"fall"`:

```clojure
;; Explicit composite value selects the existing entity before derivation.
[{:db/id "semester"
  :item/year-season [2026 "fall"]
  :item/label "Updated"}]
```

The composite assertion is an **upsert hint**, not a direct stored write. The
constituents still determine the actual tuple. Supplying a nonexistent composite
hint without constituents does not populate the tuple or those constituents.
An existing entity can also be addressed by its entity ID or composite lookup ref.

By contrast, a fresh tempid asserting only the same year and season produces a
derived-key collision and fails atomically with `transaction/unique-conflict`.
To update a constituent, first select the existing identity (including with the old
composite hint); the engine then derives the new key and enforces its uniqueness.


## NaN: replace ordinary values, never use as a unique identity

For a nonunique cardinality-one `Float` or `Double`, asserting a finite value over
NaN works in one transaction. The report/history contains both the NaN retraction
and finite assertion at that transaction. Retained earlier database values still
show NaN. Replacing a finite value with NaN works the same way.

Atomic gives NaN stable logical equality for indexing and redundancy: reasserting
NaN with different payload bits is redundant, and a different NaN payload can
retract the existing NaN. This is a native value-semantic choice, not IEEE `==` or
a promise to preserve arbitrary NaN payload bits.

NaN remains disallowed on attributes with either `Unique::Identity` or
`Unique::Value`; assertions fail with `transaction/nan-cannot-identify`, including
replacement of a previously finite unique key.
