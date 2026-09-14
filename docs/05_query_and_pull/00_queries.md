# Native query sources and reuse

`QueryEngine` evaluates native Rust query data locally. Databases, raw tuples and
immutable transaction logs can be explicit named arguments to the same query.
Existing `DatabaseValue::query`, `QuerySource`, relation inputs, rules and nested
queries continue to work.

## Numeric operations

Arithmetic supports Long, BigInt, BigDec, Float and Double. Long-only operations
retain checked overflow and truncating integer division. BigInt participation
promotes integral operations to BigInt; BigDec with exact operands produces exact
decimals. Decimal division must terminate or returns an arithmetic error: the
engine does not silently round an exact result or add a ratio value type.
Calculated zero/one shortcuts preserve the nonzero operand's compact representation
instead of padding to the other operand's preferred scale. Intermediate scales
outside `i64` are rebalanced only when the exact coefficient/result is representable
and admitted; this is not JVM preferred-scale parity.
Explicit floating inputs select Double approximation; out-of-range exact-to-double
conversions fail explicitly. Numeric refs remain valid identity/join keys, not
arithmetic operands.

`sum` preserves the group's exact integer/decimal domain unless a floating value
selects Double. Median keeps truncation for integral middle pairs and computes an
exact midpoint for decimal pairs. `avg`, `variance` and `stddev` deliberately return
approximate Double statistics. Numeric logical equality still unifies equivalent
representations; changing a top-level stored decimal's scale remains a real fact
change. Query arithmetic does not rewrite persisted representations or receipts.
Statistics widen integral intermediates and center/scale exact values before
approximation, so an overflowing intermediate does not reject a finite result.
Genuine final range failures remain explicit.

Aggregate result shape is part of the data contract. `distinct` returns
`QueryValue::Set` and readable `#{...}` EDN; `min n` and `max n` return ordered
collections. `:with` variables participate in the distinct basis before they are
removed for grouping, so equal projected values from different identities can
still contribute more than once to `count`, while `distinct` removes them inside
the completed group.

`QueryControl.max_numeric_bytes` (default 16 MiB) bounds per-operation temporary coefficient work
separately from `max_join_bytes`; coefficient growth is admitted before large
alignment or multiplication. Numeric work also observes query work/cancellation
controls, and transaction-program queries share their remaining resource budget.
Compact decimal exponents do not themselves require padding or a scale-sized
allocation in comparisons or logical join hashes.

## Raw tuples and datoms

Use `QueryDataSource::tuples` for ordinary E/A/V rows, optionally followed by
transaction and assertion/retraction fields:

```rust
use atomic_core::{
    Clause, DataPattern, FindElement, FindSpec, Keyword, Query, QueryControl,
    QueryDataSource, QueryEngine, Term, Value,
};

let query = Query::new(
    FindSpec::Relation(vec![
        FindElement::Variable("entity".into()),
        FindElement::Variable("name".into()),
    ]),
    vec![Clause::Pattern(Box::new(DataPattern::new(
        Term::var("entity"),
        Term::Constant(Value::Keyword(Keyword::new("person", "name"))),
        Term::var("name"),
    )))],
);
let sources = [QueryDataSource::tuples("$", vec![vec![
    Value::String("fred".into()),
    Value::Keyword(Keyword::new("person", "name")),
    Value::String("Fred".into()),
]])];
let outcome = QueryEngine::execute_sources(
    &query, &sources, &[], &QueryControl::default(),
)?;
```

Raw tuples have **no schema resolution**: keyword attributes, entity names and
lookup-ref-shaped tuples remain literal values. Repeated variables and constants
use the native logical value comparator, including cross-numeric equality.
Rows too short for a pattern do not match. Extra columns are ignored by that
pattern. Arbitrary-width relational inputs remain available through
`InputSpec::Relation` and `QueryInput::Relation`.

For named relations of six or more columns use `RelationPattern`; it also accepts
shorter patterns. `QueryDataSource::relation` and `QueryInput::General` extend
cells/arguments beyond stored values to nil, maps, sets, characters and inert tags.
See [general query data](01_application_data.md), including database-free queries and pure
Rust callbacks. Existing typed tuple and datom APIs remain available.

`QueryDataSource::datoms(name, datoms)` converts native `Datom` values into
E/A/V/T/assertion tuples. Entity, attribute and transaction positions contain
numeric refs. A query using an attribute ID can run unchanged over these tuples
and a native database. An attribute keyword is resolved against a native
database's schema, but is not automatically resolved against raw datom tuples.

Use `QueryDataSource::database(name, database_value)` for exact database values.
Set `DataPattern::source` to the corresponding name when combining sources.
History/as-of/since/filter semantics belong to the supplied database value;
the query does not substitute a live connection's latest database.

## Transaction logs and provenance

`QueryDataSource::log("$log", connection.log())` supplies a captured immutable
log. Two native function clauses integrate it with ordinary joins:

- `Function::TxIds`: `source = "$log"`, arguments `[start, end]`, usually a
  `Binding::Collection` of transaction refs. Start is inclusive and end exclusive.
  Bounds accept `Term::Nil`, nonnegative `Value::Long` T, `Value::Ref` transaction
  entity, or `Value::Instant` Unix milliseconds.
- `Function::TxData`: the same named log source, one T or transaction-ref
  argument, and usually a five-column `Binding::Relation` for E/A/V/T/assertion.

Join the transaction variable with a database pattern to retrieve
`:db/txInstant` or application-defined transaction provenance. Log reads retain
transaction assertions and retractions even when `noHistory` has removed earlier
facts from the indexed history view. Captured logs exclude subsequent commits.
They perform authenticated storage reads as needed; they are not an in-memory
or zero-I/O promise.

## Fulltext and structured facts

Fulltext search composes with these same sources and bindings. Use
`Function::Fulltext` with the clause's database source and arguments
`[attribute, search_string]`; the usual result is a four-column
`Binding::Relation` for entity ref, original string, assertion transaction ref,
and a double relevance score. Attribute arguments accept an attribute keyword
or numeric ID and must name a fulltext-enabled string attribute. Structured
patterns can then join the returned entity or transaction normally.

```rust
Clause::Function {
    function: Function::Fulltext,
    source: "$".into(),
    args: vec![
        Term::Constant(Value::Keyword(Keyword::new("article", "text"))),
        Term::var("search"),
    ],
    binding: Binding::Relation(vec![
        Some("entity".into()), Some("text".into()),
        Some("transaction".into()), Some("score".into()),
    ]),
}
```

Fulltext merges the captured persisted projection with bounded committed recent
and speculative assertions. Every returned fact is checked against the supplied
current/history/temporal/filtered/speculative value. Physical indexing lag does
not silently omit recent matches; an identical logical snapshot key still does
not promise identical corpus statistics or scores after indexing. Query relation order is not a ranking contract; use the score
column or the dedicated search API for relevance-aware application logic.
`QueryStats` exposes search calls, lagging/truncated searches and read bytes.
The Datalog function uses the search API's default candidate limit; row/work/
cancellation controls still fail closed rather than silently becoming a smaller
top-k request. Raw tuple and log sources cannot replace its database argument.

Fulltext can appear in portable native query templates, including nested queries
and transaction programs using the shared current program format. Such programs
are not deterministic solely from the logical db-before value. The authoritative
writer re-executes them and commits the resulting ordinary facts. Exact-value
business invariants must use structured indexes, not analyzed or top-k-limited
search results.

## Prepared queries

```rust
use atomic_core::{PreparedQuery, PreparedQueryCache, QueryControl};

// `query`, `sources` and `inputs` are ordinary native query values.
let prepared = PreparedQuery::new(&query)?;
let outcome = prepared.execute(&sources, &inputs, &QueryControl::default())?;

// Explicit cache ownership and bounded retention policy.
let mut cache = PreparedQueryCache::new(64, 4 * 1024 * 1024);
let prepared = cache.prepare(&query)?;
let stats = cache.stats();
```

Ordinary engine execution also uses a process-local bounded preparation cache.
Preparation reuses validated query structure and successful static rule-negation
analysis. Analysis is first performed under execution controls and shared by
cloned handles; interrupted analysis is retried. Every execution binds the
supplied inputs again and validates its actual sources and schema. Dynamic attribute resolution still occurs during
execution. Extension registries are supplied per invocation; use
`PreparedQuery::execute_with_extensions` when appropriate.

Cache keys reuse the bounded structural AST encoder and preserve literal numeric
representations, decimal scales and exact float bits. Queries containing opaque
local Pull callbacks bypass caching, even
when two callbacks have the same display name. Deep/large structures and keys
that exceed cache admission limits also bypass automatic caching; they are not
rejected as invalid queries. Truncated keys are never stored. Explicit prepared
handles remain owned by their callers, so retaining such handles can retain more
memory than a cache's own entries.

Prepared-query recency uses the same keyed linked-slot LRU mechanism as the node,
program and fulltext caches. Successful lookup/promotion and eviction have
expected constant work in the configured resident count. Capacity and retained
weight remain explicit cache policy; this is not Caffeine admission-policy parity
or a bound on allocator RSS.

The cache byte setting is a conservative structural-retention weight, not a
measurement of allocator usage or process RSS. This is a process-local native
cache, not a persisted compiled-query or JVM compatibility format.

## Joins, limits and observations

Shared-key input relations and raw tuple patterns can use hash joins. Their
candidate comparisons still verify full logical equality. Native database
patterns group identical resolved probes within bounded batches and retain
selective EAVT/AEVT/AVET/VAET access; they do not replace selective seeks with
whole-database scans. Existing demand-driven recursive-rule evaluation remains
in use.

Direct `Eq`, `NotEq`, `Less`, `LessOrEqual`, `Greater` and `GreaterOrEqual`
constraints on a pattern's value can narrow an available AVET index. Bounds may
be literals or already-bound input values; they use logical value ordering,
including cross-numeric equality, without coercing the bound to the attribute's
stored type. Strict bounds skip the entire equal-value prefix, and `NotEq` can
use disjoint ranges. Original predicate clauses remain in the query. Unindexed
attributes and AVET projections awaiting backfill retain the ordinary fallback;
marking an attribute indexed does not imply that its physical projection is ready.
The supplied history, time window and filters remain authoritative.

`QueryControl::max_join_bytes` controls auxiliary retention (default 4 MiB).
Larger joins process multiple chunks. Zero disables hash joins and groups native
probes one row at a time. A single ordinary native probe remains admissible even
when its key exceeds that allowance: the setting is not a new maximum valid
datom size. Explicit work, intermediate-row, result-row, timeout and cancellation
controls remain available. Persisted native query programs share their enclosing
program's fuel, cancellation, deadline and value-allocation allowance, including
when evaluation fails.

Query relations have set semantics and no guaranteed row order; top-level find
collections likewise do not provide a presentation-order contract. Join strategy can
change enumeration order. Applications needing presentation order should sort
explicitly. Result shape and `:with`/aggregate multiplicity remain meaningful;
do not deduplicate or reorder nested tuple/collection values as if they were
top-level relation rows.

`QueryStats` reports candidate work, hash-build/probe rows, saved native probes,
seeks, examined datoms and conservative auxiliary/value-allocation accounting.
Use `OperationContext` for actual SQL API-call attribution. These counters are
not network round trips, measured allocator peaks or general throughput claims.

## Pull projection

See [Pull and entity navigation](02_pull_and_entities.md) for limits, transforms,
reverse/component behavior, unresolved identities and deferred query projection.

Use operation statistics with an actual application workload when measuring
performance; bounded local measurements do not establish a universal speedup.
