# Executing Queries

## Querying a Database

In order to query a database, you must acquire a
[database value](../02_core_concepts/00_database_values.md). To get a database
value, call `db` on an application `Connection`. This captures the connection's
most recently adopted immutable value.

```rust
use edb_core::{QueryControl, QuerySourceValue};
use edb_core::edn::write_edn;
use edb_core::edn_query::{parse_query_edn, EdnQueryArgument};

// Get a database value from an existing Connection.
let db = conn.db();

// Query that value.
let query = parse_query_edn(
    r#"[:find ?release-name
        :where [_ :release/name ?release-name]]"#,
)?;
let bound = query.bind(&[
    EdnQueryArgument::Source(QuerySourceValue::Database(db.clone())),
])?;
let outcome = bound.execute(&QueryControl::default(), None)?;
println!("{}", write_edn(&bound.result_to_edn(&outcome.result)?)?);
```

For a music database, an illustrative result is:

```edn
#{["Osmium"]
  ["Hela roept de akela"]
  ["Ali Baba"]
  ["The Power of the True Love Knot"]}
```

The music data is not installed automatically. Relations have set semantics and
no guaranteed row order; their EDN rendering uses a set of tuples. A captured
database value does not change when the connection adopts a later transaction.

The arguments and grammar are documented in the
[Query Reference](05_query_reference.md).

## Query Execution

`BoundEdnQuery::execute` performs the query described by the bound query and
arguments. It returns a `QueryOutcome`, whose `result` is a relation, collection,
tuple or scalar according to the find specification.
For a relation or tuple with [return-map keys](05_query_reference.md#return-maps),
`BoundEdnQuery::result_to_edn` renders the result as maps using the declared keys.

- The query to perform is EDN data: a map, list or vector. Use `parse_query_edn`
  for text, or `query_from_edn` for an existing `EdnValue`.
- `:find` specifies the tuples or other result shape to be returned.
- `:with` is optional, and names variables to be kept in the aggregation basis
  but not returned.
- `:in` is optional. Omitting `:in` is the same as specifying `:in $`.
- `:where` limits the result returned.
- Arguments supply data sources, such as database values retrieved from a
  [call to `db`](#querying-a-database), ordinary data bindings, and rules or Pull
  patterns declared in `:in`.

Pass arguments to `EdnQuery::bind` in `:in` order. Database and log arguments use
`EdnQueryArgument::Source`; EDN data arguments use `EdnQueryArgument::Data`.
Raw relation sources can also be supplied as EDN data. See
[Query application data](01_application_data.md).

For a native Rust `Query`, use `DatabaseValue::query(&query, &inputs, &control)`
with the default `$` database source. Use `QueryEngine::execute_sources` for
explicit named database, relation and log sources. Native rules belong to the
`Query`; source arguments are separate from its ordinary `QueryInput` bindings.

## Query Sequences

`BoundEdnQuery::sequence` is a variant of eager execution that
[pulls](06_pull_reference.md) and applies Pull
[transforms](06_pull_reference.md#xform-option) lazily as you consume query
results. It uses the same bound query and arguments as `execute`:

```rust
let query = parse_query_edn(
    r#"[:find (pull ?release [:release/name])
        :where [?release :release/name _]]"#,
)?;
let bound = query.bind(&[
    EdnQueryArgument::Source(QuerySourceValue::Database(db.clone())),
])?;
let mut rows = bound.sequence(&QueryControl::default(), None)?;
let prepared_count = rows.remaining_rows();
for row in rows.by_ref().take(10) {
    let tuple = row?;
    println!("{tuple:?}");
}
```

A sequence is primarily useful when you know in advance that you do not need or
want a realized collection of projected results: you are only going to make a
single pass, or partial pass, over the result data. Item transformations such as
Pull are deferred until the sequence is consumed. This avoids projecting
unconsumed rows and retaining every projected map before consumption starts.

Relational joins and aggregates are still evaluated eagerly, and the prepared
rows are retained in memory. This is not streaming relational evaluation.
`remaining_rows()` counts the remaining prepared tuples without realizing Pull
or transforms; calling the standard iterator `count()` consumes the iterator.
A later projection error can terminate iteration before that many successful
rows are returned. Each item is `Result<Vec<QueryValue>, SemanticError>`, and the
iterator stops after an error. Scalar and collection finds are exposed as
one-column tuples during iteration.

The sequence owns immutable source values. Its lazy work shares the original
execution's deadline, work and value-allocation allowances, including time
between iterator calls. `stats()` updates as rows are consumed;
`diagnostics()` describes the eager relational preparation. For native queries,
use `DatabaseValue::query_sequence` or `QueryEngine::sequence_sources`.

For asynchronous consumption, `AsyncClient::query_sequence` captures the
connection's database value when called and projects results on workers in
demanded chunks. `AsyncExecutor::query_sequence_sources` accepts explicit
sources. Chunking preserves eager relational preparation; it does not make joins
streaming. A stream waiting for worker admission resumes when a slot becomes
available. Successful rows preceding an error in a chunk are delivered before
the error, after which the stream ends. See
[Async native clients](../07_peer_api/01_async_client.md#exact-reads-and-streams)
for chunk sizes, deadlines and admission limits.

## Unification

Unification occurs when a variable appears in more than one data pattern. In
the following query, *?e* appears twice:

```edn
;; Which 42-year-olds like what?
[:find ?e ?x
 :where [?e :age 42] [?e :likes ?x]]
```

Matches for the variable *?e* must *unify*, i.e. represent the same value in
every clause in order to satisfy the set of clauses. So a matching *?e* must
have both *:age* 42 and *:likes* for some *?x*. For example, querying a raw
relation with string entity names can return:

```edn
#{["fred" "pizza"] ["ethel" "sushi"]}
```

Against a database source, the entity column instead contains entity IDs.

Byte-array bindings use content equality. Two separately allocated
`Value::Bytes` values containing the same bytes unify; an application's buffer
identity does not affect matching. Byte-valued attributes can therefore be
matched against byte-valued query inputs using ordinary data patterns.

## Selecting Optional Attributes

A data pattern in `:where` requires a matching datom. If an attribute is
optional, putting it in such a pattern excludes entities without that attribute.
Instead, query for the entities you want and use a
[Pull expression](05_query_reference.md#pull-expressions) to select their
attributes. A missing attribute is normally omitted from the resulting map;
its absence does not remove the entity from the query's matches.

When you need a value in the query relation itself, use
[`get-else`](05_query_reference.md#get-else) to supply a default for a missing
cardinality-one attribute. The default cannot be nil. Pull projection and
`get-else` operate at different stages: Pull shapes the selected entities'
results, while `get-else` binds a value that subsequent query clauses can use.

## List Form vs. Map Form

Queries written by humans typically use a sequential form, and the various
keyword arguments are inferred by position. Both EDN lists and vectors are
accepted. For example, this query has one `:find` argument, three `:in`
arguments, and two `:where` arguments:

```edn
[:find ?e
 :in $ ?fname ?lname
 :where [?e :user/firstName ?fname]
        [?e :user/lastName ?lname]]
```

While most people find the positional syntax easy to read, it makes extra work
for programmatic readers and writers, which have to keep track of what keyword
is currently "active" and interpret tokens accordingly. For such cases, queries
can be specified more simply as maps. The query above becomes:

```edn
{:find [?e]
 :in [$ ?fname ?lname]
 :where [[?e :user/firstName ?fname]
         [?e :user/lastName ?lname]]}
```

Map section values must be lists or vectors. Both forms compile to native
`Query` data; EDN text is read as data, not evaluated as Rust or Clojure code.

## Timeout

Users can protect against long-running queries via EDB's query timeout
functionality. Supply a duration in `QueryControl::timeout` to `execute`,
`sequence`, or the native query entry points. The default is no timeout.

The example below lists all movie genres in the database with a one-millisecond
execution timeout:

```rust
use std::time::Duration;

let query = parse_query_edn(
    r#"[:find ?movie-genre
        :where [_ :movie/genre ?movie-genre]]"#,
)?;
let bound = query.bind(&[
    EdnQueryArgument::Source(QuerySourceValue::Database(db.clone())),
])?;
let control = QueryControl {
    timeout: Some(Duration::from_millis(1)),
    ..Default::default()
};
let outcome = bound.execute(&control, None)?;
```

Whether this times out depends on the data and execution. When a deadline check
observes expiration, execution returns `SemanticError` with category
`Interrupted`, code `query/timeout`, and message `query deadline elapsed`.
Timeout checks are cooperative, not a hard wall-clock interruption guarantee.
Application callbacks must cooperate during expensive work. Parsing and binding
the EDN query happen before this execution control applies; `:timeout` is not a
query section. Cancellation, work, row and allocation controls are also
available through `QueryControl`.

## Clause Order

To minimize the amount of work the query engine must do, give it selective or
narrowing patterns and bound inputs. EDB schedules ready clauses using bound
terms and a selectivity heuristic; textual `:where` order does not force
execution order.

[Query diagnostics](04_diagnostics.md) provide information about the actual
schedule, input/output rows and access paths that can be used to investigate
clause selectivity.

As an example, consider the following two queries looking for Paul McCartney's
releases. The first `:where` clause is a data pattern
(`[?release :release/name ?name]`) with low selectivity since neither `?release`
nor `?name` has a value bound to it. Evaluated on its own, it considers any
release with some value for `:release/name`:

```edn
[:find ?name
 :in $ ?artist
 :where [?release :release/name ?name]
        [?release :release/artists ?artist]]
;; inputs: db, McCartney's entity ID
```

An illustrative result is:

```edn
#{["McCartney"] ["Another Day / Oh Woman Oh Why"] ["Ram"]}
```

The following equivalent query reorders the `:where` clauses, leading with a
much more selective pattern (`[?release :release/artists ?artist]`) that is
limited in this context to the single `?artist` passed in:

```edn
[:find ?name
 :in $ ?artist
 :where [?release :release/artists ?artist]
        [?release :release/name ?name]]
;; inputs and result are the same as above
```

EDB's scheduler can choose the artist pattern first in either query. Writing
selective patterns first can make intent clearer, but reordering these clauses
does not imply a speedup. Inspect the actual schedule and measure the complete
query on your own data.

## Query Caching

Ordinary native query execution uses a process-local, bounded cache of prepared
query representations. Caching is based on the structure of the native `Query`,
including variable names, clause order and literal representations. To take
advantage of reuse, programs should:

- Use parameterized queries, that is, queries with multiple inputs, instead of
  building a different query for each input value.
- When building dynamic queries, use a canonical approach to naming and
  ordering such that equivalent queries have the same structure.

In the example below, an explicit cache prepares the query for artists on first
use. The prepared handle can then be executed with different input values:

```rust
use edb_core::{PreparedQueryCache, QueryInput, Value};
use edb_core::edn::read_edn;

let query = parse_query_edn(
    r#"[:find ?e
        :in $ ?name
        :where [?e :artist/name ?name]]"#,
)?;
let bound = query.bind(&[
    EdnQueryArgument::Source(QuerySourceValue::Database(db.clone())),
    EdnQueryArgument::Data(read_edn(r#""The Beatles""#)?),
])?;
let mut cache = PreparedQueryCache::new(64, 4 * 1024 * 1024);

// First use prepares and caches the native query structure.
let prepared = cache.prepare(bound.query())?;
let beatles = prepared.execute(
    &bound.sources, &bound.inputs, &QueryControl::default(),
)?;

// Same structure, different input; the cache can return the prepared handle.
let prepared = cache.prepare(bound.query())?;
let who = prepared.execute(
    &bound.sources,
    &[QueryInput::Scalar(Value::String("The Who".into()))],
    &QueryControl::default(),
)?;
```

A semantically equivalent query with different variable names is separately
prepared and cached:

```edn
;; Not an identical query: ?artist-name instead of ?name.
[:find ?e
 :in $ ?artist-name
 :where [?e :artist/name ?artist-name]]
```

The cache retains validated structure and successful static rule-negation
analysis. It does not retain database values, input bindings, results or a fixed
execution schedule. Each execution checks its actual sources and schema.
`EdnQuery::bind` creates its own prepared handle; reusing a parsed EDN template
avoids rereading text, and reusing a bound query avoids repeating binding. The
explicit cache above demonstrates native structural reuse across executions.

Opaque local Pull callbacks and structures that exceed key admission limits
bypass automatic caching. Cache weight is a conservative retention allowance,
not measured allocator usage or process RSS. See
[Prepared queries](00_queries.md#prepared-queries) for cache ownership and limits.
