# Synchronous client API

The synchronous Rust API for interacting with EDB is exported by `edb_core`.
Use `Connection` for an advancing read connection and transaction submission,
and `DatabaseValue` for immutable reads. These are blocking native APIs;
[AsyncClient](01_async_client.md) runs blocking operations on bounded workers.
The synchronous API does not wrap the async API.

Queries, Pull and index traversal execute in the application process. Live
values can read PostgreSQL when data is not resident. Unix-socket and TLS
transaction transports submit to a separate writer; they do not provide remote
query, Pull, catalog or index endpoints. A transaction credential does not grant
PostgreSQL read access.

Fallible operations return `Result<T, SemanticError>`. Errors expose `category`,
`code`, `message`, `details` and an optional structured `anomaly`. Both cursor
construction and later iteration can fail. Inspect errors rather than parsing
display text; see [error handling](07_error_handling.md).

There is no common argument map or global timeout. Query execution accepts
`QueryControl`, whose `timeout: Option<Duration>` defaults to `None`. Transaction
and synchronization waits take explicit durations. Standalone Pull accepts
cancellation and traversal limits through `PullControl`, but no timeout field.
Storage I/O policies are configured separately. Cooperative query deadlines do
not promise to interrupt an in-flight storage call at an exact instant.

There is also no universal offset/limit pair or default of 1,000 results:

| Operation | Pagination and defaults |
| --- | --- |
| Query | No offset/limit request fields. Execute and select result rows in the application. `max_result_rows` is a failure threshold, not pagination. |
| Query sequence | Prepared rows are consumed through a fallible iterator; no request-level offset/limit. |
| Index Pull | `offset: usize` defaults to `0`; `limit: Option<usize>` defaults to `None` (unlimited). `Some(0)` validates without visiting datoms. |
| Datoms, seeks, transaction log | Cursors have no implicit result cap; callers bound consumption or choose narrower ranges. |
| Database listing | Explicit `after_name` continuation and `limit` in `1..=4096`. |

Handle each `Result` before discarding a row for an offset: applying ordinary
`Iterator::skip` directly to a fallible cursor can discard an error. A limit on
consumed query results does not reduce work already done preparing the relation.
Keep the same immutable database value across index pages when consistent
traversal is required. Query relations have set semantics; choose an explicit
application ordering before presenting offset-based pages.

The signatures below belong to the named type; method bodies and imports are
omitted. See the [native API reference](04_native_api_reference.md) and
[type reference](05_types.md) for the wider API.

## System administration

EDB has no `administer_system` action map or base-schema upgrade action.
`storage::PgBlockStore::install(&PostgresConnectionConfig)` installs storage;
`DatabaseCatalog` manages logical databases; `PostgresOperator` supplies
maintenance operations. Connection startup does not install storage or create
a database. Application schema changes are transactions. See
[administration](../08_operations/01_administration.md) and
[changing schema](../03_schema/03_changing_schema.md).

## As-of: `DatabaseValue::as_of`

```rust,ignore
fn as_of(self, t: u64) -> DatabaseValue;
fn as_of_time_point(self, time_point: TimePoint)
    -> Result<DatabaseValue, SemanticError>;
```

Returns the value of the database as of some time-point, inclusive.
`TimePoint::T` supplies a logical transaction coordinate, `TimePoint::Tx` a
transaction entity ID, and `TimePoint::Instant` Unix epoch milliseconds.
Clone the value first to retain the original handle. Repeated calls replace the
upper boundary; the captured basis, schema and ident dictionary stay unchanged.
An exact instant shared by multiple transactions selects the earliest matching
transaction. Use `T` or `Tx` for an exact transaction boundary. See
[database views](types/03_database.md#as_of).

## Client configuration

```rust,ignore
// PostgresConnectionConfig
fn parse(parameters: impl Into<String>) -> Result<Self, SemanticError>;
```

Create a typed PostgreSQL connection configuration before opening a native
connection. Parsing configuration does not connect to a server. There is no
system-wide `client` factory or `server-type` argument. Supply the PostgreSQL
connection parameters, database name and cache capacity to `Connection`.
TLS, certificates and I/O policies belong to `PostgresConnectionConfig`; see
[connection configuration](04_native_api_reference.md#connectionconnect).

An in-memory `Database::bootstrap()` provides a value for fixtures and pure
computation; it is not a local directory of named databases. A separately running
writer uses an explicit socket endpoint or remote writer endpoint and
`RemoteClientConfig`. See [client workflow](../01_tutorials/10_client_workflow.md)
for authenticated transaction transport. Native reads still require their own
storage configuration.

## Connect: `Connection::connect_configured`

```rust,ignore
fn connect_configured(
    connection: PostgresConnectionConfig,
    database_id: impl Into<String>,
    cache_capacity: usize,
) -> Result<Connection, SemanticError>;
fn connect(
    connection: &str,
    database_id: impl Into<String>,
    cache_capacity: usize,
) -> Result<Connection, SemanticError>;
```

Connects to an existing logical database. Pass its public database name as
`database_id`. Returns a cloneable connection with an independently advancing
reader. Name resolution binds the handle to a stable database identity;
renaming a database or reusing a name does not redirect it. Use
`connection.identity()` for identity information rather than map-key lookup.

The convenience cache capacity counts retained immutable-node entries and
derives a byte allowance of `cache_capacity.saturating_mul(512 * 1024)`.
`connect_configured_with_cache_limits` accepts independent entry and byte
limits. Zero disables cache retention; these limits do not cap total process
memory or caller-retained values.

A read connection does not start a writer. Use `attach_writer` to attach an
in-process `TransactionClient`, `attach` to open a reader with an existing
client, or `start` to own an embedded service. Transport submission methods
accept their endpoint explicitly. See [Connection](types/02_connection.md).

## Create database: `DatabaseCatalog::create_if_absent`

```rust,ignore
fn create_if_absent(
    &mut self,
    name: &str,
    schema: Schema,
) -> Result<CreateDatabaseResult, SemanticError>;
```

Creates a named logical database if it is absent. Open the catalog with
`DatabaseCatalog::connect_configured(&config)`. Storage installation is a
separate prerequisite. Names must be nonempty and contain no NUL.

Returns `CreateDatabaseResult { created, database }`, where `created` indicates
whether this call created the database and `database` is its catalog entry.
An existing name returns its existing entry rather than replacing it or applying
the supplied schema. Creating a database does not start a writer. See
[database lifecycle](../08_operations/02_database_lifecycle.md).

## Datoms: `DatabaseValue::datoms`

```rust,ignore
fn datoms(&self, order: IndexOrder) -> Result<Vec<Datom>, SemanticError>;
fn datoms_with_prefix(&self, prefix: &IndexPrefix)
    -> Result<Vec<Datom>, SemanticError>;
fn scan_cursor(&self, order: IndexOrder)
    -> Result<DatabaseValueScanCursor<'_>, SemanticError>;
fn prefix_cursor(&self, prefix: &IndexPrefix)
    -> Result<DatabaseValuePrefixCursor<'_>, SemanticError>;
```

Returns datoms from an index. `IndexOrder` is one of `Eavt`, `Aevt`, `Avet`,
`Vaet`, `Saevt` or `Savet`. An optional prefix supplies leading components in
index order to narrow the result. Collecting methods materialize a vector; cursors yield
`Result<Datom, SemanticError>` incrementally. Prefix traversal terminates when
the supplied components no longer match.

Datoms expose named Rust fields, rather than keyword or positional lookup:

| Field | Type | Value |
| --- | --- | --- |
| `entity` | `u64` | Entity ID. |
| `attribute` | `u32` | Attribute ID. |
| `value` | `Value` | Attribute value. |
| `tx` | `u64` | Transaction entity ID; use `tx_to_t` for its logical coordinate. |
| `source` | `u64` | ID of the entity to whom the assertion or retraction is attributed. |
| `added` | `bool` | `true` for assertion, `false` for retraction. |

EAVT, AEVT and SAEVT cover all attributes. AVET and SAVET share available
indexed/unique projections, and VAET covers references.
Check `has_avet` before relying on an attribute projection during an index
transition. See [raw index access](../06_indexes/04_index_apis.md) for prefix
construction, index readiness and temporal views, and [Datom](types/05_datom.md)
for ordering and representation.

## Database value: `Connection::db`

```rust,ignore
fn db(&self) -> DatabaseValue;
```

Returns the current database value for a connection. Capturing it performs no
I/O; cold reads through it can require storage access. Subsequent connection
advancement cannot change a captured value.

Use typed accessors rather than map-key lookup:

| Information | Accessor |
| --- | --- |
| Connection's resolved identity | `connection.identity()` |
| Database basis | `db.basis_t()` |
| As-of point | `db.as_of_t()` |
| Since point | `db.since_t()` |
| History view | `db.is_history()` |

The as-of and since accessors return `Option<u64>`. The captured basis remains
unchanged by view filters. See [database values](types/03_database.md).

## Database statistics: `DatabaseValue::db_stats`

```rust,ignore
fn db_stats(&self) -> Result<DatabaseStats, SemanticError>;
fn db_stats_with_control(&self, control: &QueryControl)
    -> Result<DatabaseStats, SemanticError>;
```

Queries for database statistics. Returns `DatabaseStats` with `datoms: u64`,
the total number of retained history datoms, and
`attrs: BTreeMap<u32, AttributeStats>`, whose `count` field gives the per-attribute
count. Assertions and retractions both contribute. Temporal and custom filters
remain in force.

This performs a history scan rather than reading a cached constant-time count.
The controlled form applies timeout, cancellation and visible-history-datom
work limits; query row limits do not apply. Errors return no partial count.

## Delete database: `DatabaseCatalog::retire`

```rust,ignore
fn retire(&mut self, name: &str)
    -> Result<Option<DatabaseCatalogEntry>, SemanticError>;
fn retire_checked(&mut self, name: &str, expected_lineage: &str)
    -> Result<Option<DatabaseCatalogEntry>, SemanticError>;
```

Retires the named database and removes its active name. Returns `Some(entry)`
when retirement occurs, or `None` if the active name is absent. The checked form
guards the lineage currently associated with the name. Retirement and physical
storage reclamation are separate operations; retirement is not immediate
erasure of every stored object. See
[database retirement](../08_operations/02_database_lifecycle.md).

## History: `DatabaseValue::history`

```rust,ignore
fn history(self) -> DatabaseValue;
```

Returns a database value containing retained assertions and retractions across
time. A history value can be passed to datom cursors, bounded index scans and
queries, but not to speculative transactions or Pull. Queries against history
include retractions as well as assertions. The sixth datom field, `added`, is
true for assertions and false for retractions.

Temporal and custom filters remain in force. Index history is subject to
no-history consolidation and excision; it is not an unconditional record of
every fact ever asserted. The transaction log has separate retention behavior.
See [history views](types/03_database.md#history) and [log](06_log_api.md).

## Index Pull: `DatabaseValue::index_pull`

```rust,ignore
fn index_pull(&self, options: IndexPullOptions)
    -> Result<IndexPullCursor<'_>, SemanticError>;
fn index_pull_with_control(&self, options: IndexPullOptions, control: &PullControl)
    -> Result<IndexPullCursor<'_>, SemanticError>;
```

Walks an index, pulling entities using the selector, returning a lazy iterator
of results. Construct options with `IndexPullOptions::new(start, selector)`:

| Field | Meaning |
| --- | --- |
| `start` | AVET or AEVT `IndexBoundary` in index order. At least the attribute must be specified. |
| `selector` | A `PullPattern`. |
| `reverse` | When true, iterate in reverse order; defaults to false. |
| `offset` | Number of datoms to skip before projection; defaults to zero. |
| `limit` | Maximum projected results; defaults to `None` for unlimited. |

Entities are pulled from the third component: E for AVET, V for AEVT.
AEVT requires a cardinality-many reference attribute. AVET requires a starting
value when the attribute is cardinality-many; construction otherwise fails with
`index-pull/missing-start-value`.

Iteration is limited to datoms matching the starting attribute. Supplying the
second component positions the initial seek; it does not restrict later results
to that component. Results are not deduplicated, so many-to-many relationships
can return repeated entities. Use an explicitly bounded scan if a second
component must remain fixed. Each `next` projects at most one result, and errors
terminate the cursor. History values are rejected. `PullControl` applies to each
projection and shares cancellation with index traversal, including skipped
datoms. See [index Pull](../06_indexes/03_index_pull.md).

## Index range

```rust,ignore
// Eager Database
fn avet_range(
    &self,
    attribute: u32,
    start: Option<&Value>,
    end: Option<&Value>,
) -> Result<&[Datom], SemanticError>;
```

Returns datoms from the AVET index for an indexed or unique attribute.
`start` is inclusive and defaults to the beginning of that attribute when
`None`. `end` is exclusive and defaults to the end when `None`. Equal bounds
return an empty range; a start greater than the end is rejected.

`DatabaseValue` has no `index_range` method. Build an `avet_boundary`, call
`seek_cursor`, and stop when the attribute changes or the value reaches the
exclusive end. Compare values using `Value::index_cmp`. This supports temporal,
filtered and history values. See the complete
[index range examples](../06_indexes/04_index_apis.md#index-range-api).

## List databases: `DatabaseCatalog::list`

```rust,ignore
fn list(&mut self, after_name: Option<&str>, limit: usize)
    -> Result<Vec<DatabaseCatalogEntry>, SemanticError>;
```

Lists active databases in name order. Start with `after_name: None`; continue
after the last returned name. The continuation is exclusive. Supply a page size
in `1..=4096`; there is no implicit default or negative unlimited sentinel.

Each entry contains `name: Option<String>`, `database_id`, `lineage_id` and
`retired`. Active listings have names; retired entries are available separately
through `list_retired`. Catalog pages observe mutable catalog state, not one
immutable database snapshot. See [database lifecycle](../08_operations/02_database_lifecycle.md).

## Pull: `DatabaseValue::pull`

```rust,ignore
fn pull(&self, pattern: &PullPattern, entity: impl Into<EntityIdentifier>)
    -> Result<QueryValue, SemanticError>;
fn pull_with_control(
    &self,
    pattern: &PullPattern,
    entity: impl Into<EntityIdentifier>,
    control: &PullControl,
) -> Result<QueryValue, SemanticError>;
```

Returns a hierarchical selection described by the selector and entity.
Parse an EDN selector with `edb_core::edn_pull::parse_pull_edn`, or construct a
typed `PullPattern`. The entity may be a numeric ID, ident or lookup reference.
Returns a `QueryValue::Map`. For selector syntax, defaults, recursion and
transforms, see the [Pull reference](../05_query_and_pull/06_pull_reference.md).

`PullControl` supplies `max_depth`, `max_entities` and a shared cancellation flag.
The two limits default to `usize::MAX`; cancellation defaults to false. It has
no timeout field. Pull inside a query shares that query's deadline and budgets.

To get information about I/O reads, enter a diagnostic `OperationContext` around
the call and read its `SqlIoReport`. Native Pull continues to return the selection;
there is no `:io-context` argument-map overload or result wrapper. The report
contains the context, operation kind and elapsed time, SQL/cache/index attribution
and nested diagnostics. See [I/O statistics](08_io_stats.md).

## Query: `DatabaseValue::query`

```rust,ignore
fn query(&self, query: &Query, inputs: &[QueryInput], control: &QueryControl)
    -> Result<QueryOutcome, SemanticError>;
```

Performs the query described by `query` and its inputs, using this database as
the default `$` source. For EDN, `parse_query_edn(text)` reads a query template
from a string; `query_from_edn(&value)` accepts an existing EDN value. Bind data
sources, rules and other inputs with `EdnQuery::bind`, then call
`BoundEdnQuery::execute(&control, extensions)`. Pass `None` for extensions when
no local callbacks are needed. Native multi-source execution uses
`QueryEngine::execute_sources` or `PreparedQuery::execute`.

The query sequence form is:

```edn
[:find ?var1 ?var2
 :with ?var3
 :in $src1 $src2
 :where clause1 clause2]
```

`:find` specifies the tuples to be returned. `:with` is optional and names
variables retained in the aggregation basis but not returned. `:in` is optional;
omitting it is equivalent to `:in $`. `:where` limits the result. Map-form
queries are also supported. See the
[query reference](../05_query_and_pull/05_query_reference.md).

Returns `QueryOutcome`, containing a `QueryResult` relation, collection,
optional tuple or optional scalar according to the find specification, plus
`stats`, `plan` and optional `diagnostics`. EDN relation rendering uses a set of
tuples. Enable detailed diagnostics with `QueryControl::diagnostics`; this is
separate from an I/O `OperationContext`. Neither `:query-stats` nor `:io-context`
is a native query section. See [query requests](types/10_query_request.md) for
binding examples, controls, diagnostic fields and CLI output wrappers.

## Query sequence: `DatabaseValue::query_sequence`

```rust,ignore
fn query_sequence(&self, query: &Query, inputs: &[QueryInput], control: &QueryControl)
    -> Result<QuerySequence, SemanticError>;
```

Performs the query as above, returning a single-pass sequence of
`Result<Vec<QueryValue>, SemanticError>`. `QueryEngine::sequence_sources` accepts
explicit database, relation and log sources. A bound EDN query provides
`bound.sequence(&control, extensions)`, with `None` when no local callbacks are
needed. Collection and scalar results
become one-element rows; an absent optional tuple or scalar produces no row.

Relational joins, aggregation and deduplication of prepared bindings run eagerly.
Item transformations such as Pull are deferred until the sequence is consumed.
This avoids constructing all Pull maps before returning the sequence or retaining
maps the application never consumes. It does not stream the relational evaluator
or eliminate its memory requirements. Different entity bindings remain separate
even when their projected maps compare equal.

`remaining_rows()` efficiently counts prepared rows without performing Pull.
It is an upper bound on further successful results: a projection error terminates
the sequence. Ordinary iterator `count()` consumes it and performs projections.
The sequence owns immutable source values and shares the original query deadline,
cancellation flag and remaining work/value budgets through consumption. Time
spent waiting before consumption can therefore cause a timeout. Read
`stats()` after consumption for lazy Pull work; `diagnostics()` describes
relational preparation. See [query execution](../05_query_and_pull/07_executing_queries.md).

This complete in-memory example consumes one prepared row at a time. It requires
no live database and leaves the earlier value unchanged:

```rust
use edb_core::{Database, EntityRef, Keyword, QueryControl, QuerySourceValue, SemanticError};
use edb_core::edn_query::{EdnQueryArgument, parse_query_edn};
use edb_core::edn_pull::parse_pull_edn;

fn main() -> Result<(), SemanticError> {
    let initial = Database::bootstrap()?.database_value();
    let source = EntityRef::Ident(Keyword::new("db.source", "system"));
    let schema = initial.with_edn(r#"[
      {:db/ident :person/name
       :db/valueType :db.type/string
       :db/cardinality :db.cardinality/one}
    ]"#, 1_000, source.clone())?;
    let report = schema.db_after.with_edn(r#"[
      {:db/id "ada" :person/name "Ada"}
      {:db/id "grace" :person/name "Grace"}
    ]"#, 2_000, source)?;
    let pattern = parse_pull_edn("[:person/name]")?;
    let expected = report.db_after.pull(&pattern, report.tempids["ada"])?;
    let query = parse_query_edn(
        "[:find (pull ?e [:person/name]) :where [?e :person/name _]]",
    )?;
    let bound = query.bind(&[
        EdnQueryArgument::Source(QuerySourceValue::Database(report.db_after.clone())),
    ])?;
    let mut rows = bound.sequence(&QueryControl::default(), None)?;
    assert_eq!(rows.remaining_rows(), 2);
    let mut found_ada = false;
    for row in rows.by_ref() {
        let row = row?;
        found_ada |= row[0] == expected;
    }
    assert!(found_ada);
    assert_eq!(rows.remaining_rows(), 0);
    assert_eq!(initial.basis_t(), 0);
    Ok(())
}
```

## Reverse seek: `DatabaseValue::reverse_seek_cursor`

```rust,ignore
fn reverse_seek_cursor(&self, boundary: &IndexBoundary)
    -> Result<DatabaseValueScanCursor<'_>, SemanticError>;
```

Like `seek_cursor`, but iterates the index in reverse, beginning at or before
the point where the supplied components would reside. Missing suffix components
position after the highest matching suffix. Traversal only terminates at the
start of the index; callers must supply their own termination logic.

The boundary selects EAVT, AEVT, AVET, VAET, SAEVT or SAVET and zero through
five leading components in index order. See [reverse seek](../06_indexes/05_rseek_datoms.md).

## Seek: `DatabaseValue::seek_cursor`

```rust,ignore
fn seek_cursor(&self, boundary: &IndexBoundary)
    -> Result<DatabaseValueScanCursor<'_>, SemanticError>;
```

Raw access to index data, starting at or after the point where the components
would reside, iterating forward to the end of the index. An exact match need not
exist. Callers must supply their own termination logic: seek does not filter to
the boundary of the supplied components.

Build schema-aware boundaries with `eavt_boundary`, `aevt_boundary`,
`avet_boundary`, `vaet_boundary`, `saevt_boundary` or `savet_boundary`.
`IndexComponents` specifies zero through five leading components. The original
orders put `IndexTransaction` fourth and optional source `EntityIdentifier`
fifth; source-first orders put source first and transaction fifth. Source
identifiers resolve like entity identifiers. The cursor yields
fallible datoms. `collect_seek_datoms` and `collect_reverse_seek_datoms`
materialize the corresponding traversal. See [seek APIs](../06_indexes/04_index_apis.md#seek-apis).

## Since: `DatabaseValue::since`

```rust,ignore
fn since(self, t: u64) -> DatabaseValue;
fn since_time_point(self, time_point: TimePoint)
    -> Result<DatabaseValue, SemanticError>;
```

Returns the value of the database since some time-point, exclusive. The typed
form accepts a logical coordinate, transaction entity ID or instant. Repeated
calls replace the lower boundary. Basis, schema and ident dictionary remain
unchanged. An instant resolving to the earliest transaction in a millisecond
can leave later transactions in that millisecond visible. See
[since views](types/03_database.md#since).

## Synchronize: `Connection::sync_to`

```rust,ignore
fn sync_to(&self, target_t: u64, timeout: Duration)
    -> Result<DatabaseValue, SemanticError>;
fn sync(&self) -> Result<DatabaseValue, SemanticError>;
```

Used to coordinate with other clients. `sync_to` waits for a database value with
`basis_t() >= target_t`. The calling wait observes local progress and does not
make a remote call. The connection's background observer performs storage I/O
to advance the reader. A stalled observer cannot extend this local wait beyond
its timeout. A future target may never be reached; supply a finite duration.

`sync` instead reads PostgreSQL and adopts the newest durable head visible to
that read. It has no timeout argument; configured storage I/O policies apply.
When coordination is unnecessary, use `db()`. Previously captured values remain
unchanged by either operation. See [synchronization](../04_transactions/09_synchronization.md)
for index, schema and excision waits and the differing lower-level Peer behavior.

## Transact: `Connection::transact`

```rust,ignore
fn transact(&self, request: TransactionRequest, timeout: Duration)
    -> Result<ServiceTransactionReport, SemanticError>;
```

Submits a transaction and waits for its result. Construct the request using
`TransactionRequest::new(request_key, operations, source)`,
`from_forms(request_key, forms, source)` or `from_edn(request_key, text, source)`.
The required `source: EntityRef` is supplied once for the transaction, including
when its data is empty. Source follows ordinary entity-reference resolution;
a source tempid must be established in entity position in the same transaction.
EDN transaction data is an outer vector of ordinary operation vectors, entity
maps and transaction calls. Every assertion and retraction, including generated
changes and function results, receives the transaction's source. EDB
automatically asserts exactly one `:db/txInstant` on
the current transaction with that source. The writer normally selects its
current clock value, clamped to the previous transaction instant. An explicit
timestamp value in an ordinary map or add form must not precede the previous
transaction instant or exceed the writer's clock, and must agree with any
request-level time override.
Timestamp data applies only to the current transaction; retraction and
compare-and-swap of `:db/txInstant` are prohibited. See
[transaction data](../04_transactions/04_transaction_data.md).

Requires an attached transaction client. Returns a `ServiceTransactionReport`:

| Field | Value |
| --- | --- |
| `db_before` | Exact database value before the transaction. |
| `db_after` | Exact database value after durable publication. |
| `tx_data` | `Vec<Datom>` produced by the transaction. |
| `tempids` | `BTreeMap<String, u64>` mapping temporary IDs to resolved entity IDs. |
| `basis_t`, `tx_hash` | Committed logical coordinate and transaction hash. |
| `replayed` | Whether an existing request receipt supplied the result. |
| `diagnostics` | Optional `Arc<TransactionDiagnostics>` for this submission. |

The connection attempts ordered local observation in the time remaining after
receiving the report. An observation failure after known durable success still
returns the successful report and records `observation_error()`. Read
`report.db_after` for the exact committed result; `connection.db()` need not yet
include it.

There is no default transaction timeout. The supplied duration covers the ticket
wait and remaining observation wait, beginning after submission. Validation and
encoding precede that wait. Timeout or disconnection after admission can mean
`UnknownOutcome`; it does not prove rollback. Preserve the same request key,
transaction data, source, basis condition and explicit time option for exact
retries. Exact retries return the accepted result before fresh entity resolution
or clock selection. `submit` returns a ticket whose `wait(timeout)`
has the same outcome rules. Dropping a ticket does not cancel admitted work.

For a separate writer, `transact_socket` and `transact_remote` return a
`CommittedTransaction`: a known commit can coexist with an error opening its
local report. See [Connection](types/02_connection.md) and
[transaction diagnostics](../04_transactions/03_diagnostics.md).

To request local transaction I/O diagnostics, enter a diagnostic
`OperationContext` around submission. Submission carries the context to the
service. `report.diagnostics` contains a `TransactionDiagnostics` with `basis_t`,
`replayed` and its I/O `report`. These diagnostics are ephemeral, absent from
durable receipt bytes and reconstructed remote reports. Detailed reports include
the operation/context, elapsed time, SQL/cache/index attribution and nested work;
interpret timing by its operation scope rather than as end-to-end client latency.
Configured service telemetry can emit transaction events; logging every transaction
is not automatic. See [I/O statistics](08_io_stats.md) for fields, context labels
and telemetry configuration.

## Transaction range: `LogValue::tx_range`

```rust,ignore
fn tx_range(&self, start: Option<TimePoint>, end: Option<TimePoint>)
    -> Result<LogCursor, SemanticError>;
```

Retrieve a range of transactions from `connection.log()`. `start` is inclusive;
`None` starts at the beginning of the log. `end` is exclusive; `None` runs through
the captured log endpoint. Bounds accept logical transaction coordinates,
transaction entity IDs or instants. Instant bounds select the first transaction
at or after the supplied millisecond. Reversed or disjoint ranges are empty.

Returns a cursor of `Result<LogTransaction, SemanticError>`. Each transaction has
`t: u64`, its logical coordinate, and `data: Vec<Datom>`, its assertions and
retractions. The cursor has no offset/limit or timeout argument and fuses after
an error. Bound the range or consumption explicitly, handling each result.
No-history index consolidation does not remove retained log transaction data;
retention and excision still apply. See [LogValue](types/08_log.md).

## Speculative transaction: `DatabaseValue::with`

```rust,ignore
fn with(&self, ops: &[TxOp], tx_instant: i64, source: EntityRef)
    -> Result<SpeculativeTransactionReport, SemanticError>;
fn with_forms(&self, forms: &[TxForm], tx_instant: i64, source: EntityRef)
    -> Result<SpeculativeTransactionReport, SemanticError>;
fn with_edn(&self, text: &str, tx_instant: i64, source: EntityRef)
    -> Result<SpeculativeTransactionReport, SemanticError>;
```

Applies transaction data to a database value or the `db_after` of a prior
speculative transaction. Returns a value as if the data had been transacted,
while leaving the durable database unaffected. The report contains `db_before`,
`db_after`, `tx_data` and `tempids`.

The data forms follow the same transaction semantics as durable submission;
speculation does not need a request key and requires an explicit transaction
instant in Unix milliseconds and one source. EDB automatically asserts the
instant with that source, even when the data is empty. An explicit timestamp
value in ordinary transaction data must agree with the instant argument, which
must not precede db-before. History values cannot transact. Read filters,
including as-of/since, apply to the resulting view rather than to transaction
generation or invariant checking. Speculation does not advance a connection or
reserve IDs for a later durable transaction. See
[speculative transactions](04_native_api_reference.md#speculative-transactions-databasevaluewith).

## Database for speculation

EDB needs no separate `with_db` call or special connection value. Capture
`connection.db()` and call `with`, `with_forms` or `with_edn`; chain subsequent
calls from `report.db_after`. `Database::bootstrap()?.database_value()` supplies
an in-memory starting point. See the complete
[in-memory example](04_native_api_reference.md#speculative-transactions-databasevaluewith).
