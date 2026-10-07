# Asynchronous client API

The asynchronous Rust API for EDB is exported by `edb_core`. See also the
[synchronous client API](09_client_api_sync.md) and the detailed
[client library usage reference](01_async_client.md).

`AsyncClient` operates on an existing native `Connection`; `AsyncExecutor`
also executes queries against explicit database, relation and log sources.
Blocking work runs on a fixed worker pool. Queries and Pull execute in the
application process and may read PostgreSQL. The facade does not provide a
remote query service.

Asynchronous operations return `Result<AsyncOperation<T>, SemanticError>`:
the outer result reports admission, and awaiting the operation returns
`Result<T, SemanticError>`. Transactions use `AsyncTransaction` with the same
two error boundaries. Errors contain `category`, `code`, `message`, `details`
and an optional structured `anomaly`; see [error handling](07_error_handling.md).
`operation.on_complete(callback)` consumes the future and calls the callback
once on a worker instead of awaiting it. See the
[async operation types](types/07_async_operation.md).

There is no common argument map. Timeout settings are typed Rust arguments:

| Operation | Timeout |
| --- | --- |
| Query | `QueryControl::timeout: Option<Duration>`, default `None`. |
| Query sequence or transaction range | `AsyncStreamOptions::timeout`, default `None`; query sequences use the smaller of query and stream timeouts. |
| Pull, datom collection, snapshot reopening, `sync` | Explicit `Option<Duration>`; `None` sets no facade deadline. |
| Transactions and finite-target synchronization | Required `Duration`. |
| `db`, `log`, `request_index` | No facade timeout argument. |

Deadlines start at admission and include queue delay. Expired queued work
returns `async/deadline`; query execution also has cooperative timeout and
cancellation checks. A deadline does not forcibly interrupt a blocked driver
call or arbitrary callback. Configure native storage I/O policies separately.
An unknown transaction outcome does not imply rollback.

Functions returning sequences return an `AsyncStream<T>` of individual
`Result<T, SemanticError>` items. Use `stream.next().await` or the
`futures_core::Stream` interface. Results are buffered internally in chunks;
the caller does not receive vectors of chunks. An error is delivered after
any successful rows preceding it in that chunk, then the stream ends.

| Sequence option | Meaning | Default |
| --- | --- | --- |
| `chunk_rows` | Maximum rows advanced per worker job; must be positive. | `64` |
| `timeout` | Optional deadline for the entire stream. | `None` |
| Offset | Consume and check the rows to omit in application code. | No implicit offset. |
| Limit | Stop consuming after the desired number of rows. | No implicit limit or `-1` sentinel. |

There is no automatic 1,000-result cap. Chunk size counts rows, not bytes.
At most one chunk job is outstanding per stream; a slow consumer does not
leave a worker blocked on a full output channel. Query resource budgets fail
execution when exceeded; they are not pagination. See the
[offset and limit example](01_async_client.md#offset-and-limit-example).

Dropping a read future can skip unstarted work. Dropping a transaction waiter
abandons its result, but does not cancel its admitted transaction. Dropping a
stream stops future advancement, subject to work already running. Abandoned
facade-owned resources are cleaned on workers. Consumed native values and
reports retain their synchronous methods and final-drop behavior; keep their
potentially blocking work off the future executor thread.

The signatures below belong to the named types; bodies and imports are omitted.
Entries without an async facade method identify their native equivalent
explicitly. `AsyncExecutor` does not expose a general public job-submission
method for wrapping arbitrary native operations.

## System administration

There is no `administer_system` method or `upgrade-schema` action map.
`storage::PgBlockStore::install(&config)` installs storage, `DatabaseCatalog`
manages logical databases, and `PostgresOperator` supplies maintenance APIs.
These are synchronous administrative operations. Opening a connection does
not install storage or create a database. Application schema changes are
transactions. See [administration](../08_operations/01_administration.md).

## As-of: `DatabaseValue::as_of`

```rust,ignore
fn as_of(self, t: u64) -> DatabaseValue;
fn as_of_time_point(self, point: TimePoint) -> Result<DatabaseValue, SemanticError>;
```

Returns the value of the database as of some time-point, inclusive. This is a
native value operation, not an async method. `as_of(t)` derives the view without
I/O; resolving an instant with `as_of_time_point` can read storage.
`TimePoint` accepts a logical coordinate (`T`), transaction entity ID (`Tx`),
or Unix milliseconds (`Instant`). Repeated calls replace the upper boundary;
basis, schema and ident dictionary remain unchanged. Use `T` or `Tx` for an
exact transaction boundary: a shared exact instant selects the earliest
transaction at that millisecond.

Pass the derived value as `QueryDataSource::database("$", view)` to
`AsyncExecutor::query_sources` or `query_sequence_sources`. See
[database views](types/03_database.md#as_of).

## Client: `AsyncClient::new`

```rust,ignore
// AsyncExecutor
fn new(config: AsyncConfig) -> Result<AsyncExecutor, SemanticError>;
// AsyncClient
fn new(connection: &Connection, executor: &AsyncExecutor)
    -> Result<AsyncClient, SemanticError>;
```

Create an executor during application setup and retain an existing connection:

```rust,ignore
let workers = edb_core::AsyncExecutor::new(Default::default())?;
let client = edb_core::AsyncClient::new(&connection, &workers)?;
```

`AsyncClient::new` performs no I/O, but can fail to reserve a resource slot.
`AsyncExecutor::new` starts fixed worker threads and requires positive counts:

| `AsyncConfig` field | Meaning | Default |
| --- | --- | --- |
| `workers` | Blocking worker threads. | `4` |
| `max_operations` | Queued, running and completed-but-unconsumed operations. | `64` |
| `max_resources` | Retained clients and streams. | `64` |

Capacity exhaustion reports `Busy` with `async/operation-capacity` or
`async/resource-capacity`. `workers.stats()` reports occupancy and activity.
Consume or release retained results before admitting more work.

There is no `server-type` selector. Cloud `region`, `system`, `endpoint`, AWS
credential provider/profile, local `storage-dir`, and peer-server
`access-key`, `secret`, or `validate-hostnames` are not client configuration
fields. Native reads use `PostgresConnectionConfig`, including credentials,
TLS and I/O policy. An in-memory `Database::bootstrap()` supplies a value for
pure computation, not a directory of named databases. Separate writers use
explicit socket or verified TLS endpoints and transport configuration; their
credentials do not authorize PostgreSQL reads. See
[configuration](../08_operations/14_configuration_reference.md).

## Connect: `Connection::connect_configured`

```rust,ignore
fn connect_configured(
    config: PostgresConnectionConfig,
    database_id: impl Into<String>,
    cache_capacity: usize,
) -> Result<Connection, SemanticError>;
```

Connects to an existing database, using its public name as `database_id`.
Connection opening is synchronous and accesses storage; do it during setup or
on an application blocking worker, then construct the facade. There is no
async `connect` method.

Returned connections are cloneable shared handles bound to stable database
identity. Name reuse or rename does not redirect them. `connection.identity()`
and `client.identity()` expose identity information, rather than `:db-name`
lookup. Reads do not require an attached writer; native `transact` does.
See [Connection](types/02_connection.md).

## Create database: `DatabaseCatalog::create_if_absent`

```rust,ignore
fn create_if_absent(&mut self, name: &str, schema: Schema)
    -> Result<CreateDatabaseResult, SemanticError>;
```

Creates the named database if absent. Open the catalog separately with
`DatabaseCatalog::connect_configured(&admin_config)`. Returns
`CreateDatabaseResult { created, database }`, where `created` says whether
creation occurred and `database` is the catalog entry. An existing name returns
its existing entry. This is a synchronous administrative API; `AsyncClient`
has no creation method. See [database lifecycle](../08_operations/02_database_lifecycle.md).

## Datoms: `AsyncClient::collect_datoms`

```rust,ignore
fn collect_datoms(&self, order: IndexOrder, timeout: Option<Duration>)
    -> Result<AsyncOperation<Vec<Datom>>, SemanticError>;
fn collect_datoms_with_prefix(&self, prefix: IndexPrefix, timeout: Option<Duration>)
    -> Result<AsyncOperation<Vec<Datom>>, SemanticError>;
```

Returns datoms from an index: `Eavt`, `Aevt`, `Avet` or `Vaet`.
An optional `IndexPrefix` supplies leading components in index order to
narrow the result; the prefix scan ends when components no longer match.
Both methods capture the connection value at the call and materialize a
vector on a worker. They have no chunk, offset or limit option and do not
accept a derived database view. Native `DatabaseValue` cursors provide
incremental traversal on explicit values.

Datoms are typed structures with public fields, not associative or indexed
collections:

| Logical position | Rust field | Value |
| --- | --- | --- |
| 0: E | `entity` | Entity ID, `u64`. |
| 1: A | `attribute` | Attribute ID, `u32`. |
| 2: V | `value` | Typed `Value`. |
| 3: TX | `tx` | Transaction entity ID, `u64`; use `tx_to_t` for logical T. |
| 4: S | `source` | Source entity ID, `u64`. |
| 5: added | `added` | `true` for assertion, `false` for retraction. |

See [Datom](types/05_datom.md) and [index APIs](../06_indexes/04_index_apis.md).

## Database: `AsyncClient::db`

```rust,ignore
fn db(&self) -> Result<AsyncOperation<DatabaseValue>, SemanticError>;
```

Returns the current database value for the connection. Capture occurs at the
call, not on first poll. Subsequent connection advancement does not alter the
captured value. The returned value is a native `DatabaseValue`; its methods
are synchronous, and cold reads may access storage.

Use the connection's `identity()` for stable database identity, `basis_t()`
for the value's basis, `as_of_t()` and `since_t()` for optional temporal bounds, and `is_history()`
for history status. There is no keyword lookup interface. Native
`connection.db()` captures the value without I/O or async admission.
See [DatabaseValue](types/03_database.md).

## Database statistics: `DatabaseValue::db_stats`

```rust,ignore
fn db_stats(&self) -> Result<DatabaseStats, SemanticError>;
fn db_stats_with_control(&self, control: &QueryControl)
    -> Result<DatabaseStats, SemanticError>;
```

Counts datoms in the history of this value. Returns `DatabaseStats` with
`datoms: u64` and `attrs: BTreeMap<u32, AttributeStats>`, whose per-attribute
`count` includes assertions and retractions. Temporal/custom filters remain
in force; no-history consolidation and excision limit retained history.
This is a scan, not a constant-time metadata lookup or current entity count.

There is no async statistics method. Run the native call on an application
blocking worker for a storage-backed value. Controlled counting supports
`timeout`, `cancel` and `max_work`; errors return no partial count.

## Delete database: `DatabaseCatalog::retire_checked`

```rust,ignore
fn retire_checked(&mut self, name: &str, expected_lineage: &str)
    -> Result<Option<DatabaseCatalogEntry>, SemanticError>;
```

Retires the database matching the name and expected lineage, removing its
active name and fencing its writer. Returns `Some(entry)` on retirement or
`None` when the active name is absent. There is no async deletion method.
Retirement and physical reclamation are separate administrative operations;
retirement does not immediately erase every stored object. See
[database lifecycle](../08_operations/02_database_lifecycle.md).

## History: `DatabaseValue::history`

```rust,ignore
fn history(self) -> DatabaseValue;
```

Returns a database value containing retained assertions and retractions
across time. This native view construction performs no I/O. Pass it to
`AsyncExecutor` query methods as an explicit database source, or use native
datom cursors and bounded scans on a blocking worker. History cannot be used
for Pull or speculative transactions.

Queries against history include retractions as well as assertions; the sixth
datom field, `added`, distinguishes them. Temporal/custom filters remain in
force. History is subject to no-history consolidation and excision; the log
has separate retention behavior. See [history views](types/03_database.md#history).

## Index Pull: `DatabaseValue::index_pull`

```rust,ignore
fn index_pull(&self, options: IndexPullOptions)
    -> Result<IndexPullCursor<'_>, SemanticError>;
```

Walks an index, pulling entities using the selector. There is no async
index-Pull method. Construct native options with
`IndexPullOptions::new(start, selector)` and consume its fallible iterator on
a blocking worker:

| Field | Meaning |
| --- | --- |
| `start` | AVET or AEVT `IndexBoundary` in index order; at least A must be specified. |
| `selector` | `PullPattern` describing the selection. |
| `reverse` | Iterate in reverse order; defaults to `false`. |
| `offset` | Datoms omitted before projection; defaults to `0`. |
| `limit` | Maximum projected results; defaults to `None` for unlimited. |

Pull uses E for AVET and V for AEVT. AEVT requires a cardinality-many reference
attribute. AVET on a cardinality-many attribute requires a starting value.
Iteration remains within the starting attribute, but additional boundary
components only position the initial seek. Results can repeat an entity.
History is rejected. `index_pull_with_control` accepts a `PullControl` for
per-projection limits and shared cancellation. See
[index Pull](../06_indexes/03_index_pull.md).

## Index range

There is no async `index_range` method. For a native `DatabaseValue`, build an
AVET boundary, use `seek_cursor`, and stop when the attribute changes or the
value reaches the exclusive end. `start` is inclusive; omitting it starts at
the beginning of the attribute. Omitting `end` runs to that attribute's end.
Compare values with `Value::index_cmp`; execute storage-backed traversal on
a blocking worker. This also supports temporal, filtered and history views.

The eager in-memory `Database` has
`avet_range(attribute, start: Option<&Value>, end: Option<&Value>)`, returning
`Result<&[Datom], SemanticError>` for an indexed or unique attribute.
See [index range examples](../06_indexes/04_index_apis.md#index-range-api).

## List databases: `DatabaseCatalog::list`

```rust,ignore
fn list(&mut self, after_name: Option<&str>, limit: usize)
    -> Result<Vec<DatabaseCatalogEntry>, SemanticError>;
```

Lists active databases in name order. This synchronous administrative API
has no async facade method. Start with `None`, then pass the last returned
name as an exclusive continuation. `limit` must be `1..=4096`.
Entries contain `name`, `database_id`, `lineage_id` and `retired`, rather than
only names. `list_retired` lists retired entries separately. Catalog pages
observe mutable catalog state. See [database lifecycle](../08_operations/02_database_lifecycle.md).

## Pull: `AsyncClient::pull`

```rust,ignore
fn pull(
    &self, pattern: PullPattern, entity: impl Into<EntityIdentifier>,
    control: PullControl, timeout: Option<Duration>,
) -> Result<AsyncOperation<QueryValue>, SemanticError>;
```

Returns a hierarchical selection described by the selector and entity.
Parse the selector with `edb_core::edn_pull::parse_pull_edn`; the entity can
be a numeric ID, ident or lookup reference. The future yields a
`QueryValue::Map`. `PullControl` supplies depth/entity limits and cancellation;
the separate timeout is the facade deadline. See the
[Pull reference](../05_query_and_pull/06_pull_reference.md).

Pull captures the connection value at the call. To Pull an explicit derived
value, call its synchronous native method on a blocking worker, or use a Pull
expression in an `AsyncExecutor` query against that value.

I/O statistics use `OperationContext`, not a `:io-context` argument or a
`{:ret ... :io-stats ...}` return wrapper. Enter a diagnostic or named context
while admitting the operation; the facade carries it to the worker. End the
thread-local scope before awaiting, then obtain `context.report()` after
completion. Its `SqlIoReport` contains the context label, operation kind,
elapsed time, SQL/cache/index attribution and nested diagnostics. See
[I/O statistics](08_io_stats.md).

## Query: `AsyncClient::query`

```rust,ignore
fn query(&self, query: Query, inputs: Vec<QueryInput>, control: QueryControl)
    -> Result<AsyncOperation<QueryOutcome>, SemanticError>;
// AsyncExecutor
fn query_sources(
    &self, query: Query, sources: &[QueryDataSource], inputs: Vec<QueryInput>,
    control: QueryControl, extensions: Option<QueryExtensions>,
) -> Result<AsyncOperation<QueryOutcome>, SemanticError>;
```

Performs the query described by `query` and its inputs. `AsyncClient::query`
captures its connection's current value as `$`. `query_sources` accepts
explicit database, relation and immutable-log sources, including derived
views and speculative values. `query_prepared` accepts a `PreparedQuery`
with the same source/input/control/extension arguments. Client queries with
callbacks use `query_with_extensions`.

The query sequence form is:

```edn
[:find ?var1 ?var2
 :with ?var3
 :in $src1 $src2
 :where clause1 clause2]
```

`:find` specifies the tuples returned. `:with` is optional and names variables
retained in the aggregation basis but not returned. `:in` is optional;
omitting it is equivalent to `:in $`. `:where` limits the result. Map-form
queries are supported. Parse string input with `parse_query_edn`, or an
existing EDN value with `query_from_edn`, then bind sources, ordinary inputs
and rules using `EdnQuery::bind`. Parsing and binding are synchronous and can
fail before async admission. See the [query reference](../05_query_and_pull/05_query_reference.md).

The future yields a materialized `QueryOutcome`, containing the `QueryResult`
relation, collection, optional tuple or optional scalar, together with
`stats`, `plan` and optional `diagnostics`. It does not yield chunks; use
`query_sequence` for streamed rows. Relations have set semantics.
Enable detailed query diagnostics through `QueryControl::diagnostics`.
For I/O statistics, enter an `OperationContext` at admission and read its
report after completion, as for Pull; this does not change the query result
type or add an `:io-context` query section.

This pure async function needs no live database. Await it using your Rust
future runtime after creating `workers` during setup:

```rust
use edb_core::{AsyncExecutor, QueryControl, QueryOutcome, SemanticError};
use edb_core::edn::read_edn;
use edb_core::edn_query::{EdnQueryArgument, parse_query_edn};

async fn names(workers: &AsyncExecutor) -> Result<QueryOutcome, SemanticError> {
    let bound = parse_query_edn("[:find ?name :in [?name ...]]")?
        .bind(&[EdnQueryArgument::Data(read_edn(r#"["Ada" "Grace"]"#)?)])?;
    workers.query_sources(
        bound.query().clone(), &bound.sources, bound.inputs.clone(),
        QueryControl::default(), None,
    )?.await
}
```

The result is a relation containing the one-column tuples `"Ada"` and
`"Grace"`; no application ordering is implied.

## Query sequence: `AsyncClient::query_sequence`

```rust,ignore
fn query_sequence(
    &self, query: Query, inputs: Vec<QueryInput>, control: QueryControl,
    extensions: Option<QueryExtensions>, options: AsyncStreamOptions,
) -> Result<AsyncStream<Vec<QueryValue>>, SemanticError>;
// AsyncExecutor
fn query_sequence_sources(
    &self, query: Query, sources: &[QueryDataSource], inputs: Vec<QueryInput>,
    control: QueryControl, extensions: Option<QueryExtensions>,
    options: AsyncStreamOptions,
) -> Result<AsyncStream<Vec<QueryValue>>, SemanticError>;
```

Performs the query as above, returning a stream of rows that ends when all
results are returned. Each row is `Result<Vec<QueryValue>, SemanticError>`.
Item transformations such as Pull are deferred until chunks are demanded.
For queries with Pull, this avoids constructing and retaining every map
before the first rows can be consumed. Joins, aggregation and prepared-binding
deduplication remain eager; chunking does not make relational work stream in
bounded memory.

The stream retains its captured sources and deadline through consumption.
Dropping it stops future advancement, but a worker may already have projected
the remainder of its current chunk. See the complete
[chunked results example](01_async_client.md#chunked-results-example).

## Reverse seek: `DatabaseValue::reverse_seek_cursor`

```rust,ignore
fn reverse_seek_cursor(&self, boundary: &IndexBoundary)
    -> Result<DatabaseValueScanCursor<'_>, SemanticError>;
```

Like `seek_cursor`, but iterates the index in reverse, beginning at or before
the point where the supplied components would reside. The boundary selects
EAVT, AEVT, AVET or VAET and zero through four leading components in index
order. Missing suffix components position after the highest matching suffix.
Callers supply termination logic; traversal otherwise continues to the
beginning of the index.

There is no async reverse-seek stream. Consume the native fallible cursor
on a blocking worker. See [reverse seek](../06_indexes/05_rseek_datoms.md).

## Seek: `DatabaseValue::seek_cursor`

```rust,ignore
fn seek_cursor(&self, boundary: &IndexBoundary)
    -> Result<DatabaseValueScanCursor<'_>, SemanticError>;
```

Returns datoms starting at or after the point in the index where the components
would reside, iterating forward to the end of the index. Callers must supply
their own termination logic; seek does not stay within a prefix.

Build EAVT, AEVT, AVET or VAET boundaries using schema-aware
`eavt_boundary`, `aevt_boundary`, `avet_boundary` or `vaet_boundary` methods.
`IndexComponents` supplies zero through four leading components; the fourth
is `IndexTransaction`. This is a synchronous fallible cursor, with no async
seek method. `collect_seek_datoms` materializes native traversal;
`AsyncClient::collect_datoms_with_prefix` is a different, prefix-bounded
operation. See [seek APIs](../06_indexes/04_index_apis.md#seek-apis).

## Since: `DatabaseValue::since`

```rust,ignore
fn since(self, t: u64) -> DatabaseValue;
fn since_time_point(self, point: TimePoint) -> Result<DatabaseValue, SemanticError>;
```

Returns the value of the database since some time-point, exclusive.
`since(t)` constructs a native view without I/O; typed instant resolution can
read storage. Repeated calls replace the lower boundary without changing the
basis, schema or ident dictionary. An instant shared by several transactions
can leave later transactions at that millisecond visible. Pass this view to
an executor query as an explicit database source. See
[since views](types/03_database.md#since).

## Synchronize: `AsyncClient::sync_to`

```rust,ignore
fn sync_to(&self, target: u64, timeout: Duration)
    -> Result<AsyncOperation<DatabaseValue>, SemanticError>;
fn sync(&self, timeout: Option<Duration>)
    -> Result<AsyncOperation<DatabaseValue>, SemanticError>;
```

Used to coordinate with other clients. `sync_to` yields a database value
with `basis_t() >= target`. Its worker waits for local observation; the
connection's background observer performs storage I/O. The finite duration
includes queue delay, and the native wait receives the remaining time.

`sync` instead reads the fresh durable head on a worker. It includes commits
completed before admission; later commits may also be included. Thus `sync`
can perform remote storage I/O. Captured database values remain unchanged.
`sync_index`, `sync_schema` and `sync_excise` take the same target/duration
arguments as `sync_to`, with distinct completion conditions. See
[synchronization](../04_transactions/09_synchronization.md).

## Transact: `AsyncClient::transact`

```rust,ignore
fn transact(&self, request: TransactionRequest, timeout: Duration)
    -> Result<AsyncTransaction, SemanticError>;
```

Submits a transaction through the attached writer. Construct a request with
`TransactionRequest::new(request_key, operations)`, `from_forms(request_key,
forms)` or `from_edn(request_key, text)`. EDN transaction data is a collection
of operation vectors, entity maps and transaction calls. Each assertion and
retraction carries an explicitly supplied source entity ID. A metadata-only
`{:db/id "edb.tx" :db/source S}` map supplies the source for the generated
transaction instant, without supplying a default for other forms; see
[transaction data](../04_transactions/04_transaction_data.md).

The admission result is a waiter, not proof of durability. Awaiting it yields
`ServiceTransactionReport`:

| Field | Value |
| --- | --- |
| `db_before` | Exact database value before the transaction. |
| `db_after` | Exact database value after durable publication. |
| `tx_data` | `Vec<Datom>` produced by the transaction. |
| `tempids` | Map from string tempids to resolved entity IDs. |
| `basis_t`, `tx_hash` | Committed logical coordinate and transaction hash. |
| `replayed` | Whether an existing receipt supplied the result. |
| `diagnostics` | Optional per-submission transaction diagnostics. |

For example, assuming the movie schema and attached writer are installed
and entity `99` identifies the contributor:

```rust,ignore
let request = edb_core::TransactionRequest::from_edn(
    "add-goonies",
    r#"[{:db/id "edb.tx" :db/source 99}
        {:db/id "goonies" :db/source 99 :movie/title "The Goonies"}]"#,
)?;
let report = client.transact(request, std::time::Duration::from_secs(30))?.await?;
let goonies = report.tempids["goonies"];
```

Keep the request key and exact data for retries; use a new key for a new
logical request. `AsyncTransaction::request_key()` exposes the retained key.
Busy admission means no submission occurred. After admission, dropping the
waiter does not cancel the transaction. A deadline expired before submission
prevents it, but timeout/disconnection after submission can mean
`UnknownOutcome`. Known durable success remains success if local observation
fails; use `report.db_after` for the exact committed result.

On Unix, `transact_socket`, `transact_remote` and `transact_routed` support
separate writers and yield `CommittedTransaction`. A known committed basis
and hash can accompany `report: Err` when opening the local report fails.
Hint variants preserve native advisory hints; routed calls check stable
database identity. See [transaction transports](01_async_client.md#transaction-transports).

For I/O statistics, enter a diagnostic `OperationContext` at admission and
leave its thread-local scope before awaiting. The context propagates through
the worker and local service; `report.diagnostics` can contain a
`TransactionDiagnostics` with basis, replay status and an I/O report.
It includes operation/context, elapsed time, SQL/cache/index attribution and
nested work. These reports are ephemeral, not durable receipt fields or
reconstructed remote diagnostics. Interpret timings by their operation scope,
not as total queue, storage and network latency. Configured service telemetry
can emit transaction events; every transaction is not automatically logged
with I/O statistics. See [I/O statistics](08_io_stats.md).

## Transaction range: `AsyncClient::tx_range`

```rust,ignore
fn tx_range(
    &self, start: Option<TimePoint>, end: Option<TimePoint>,
    options: AsyncStreamOptions,
) -> Result<AsyncStream<LogTransaction>, SemanticError>;
```

Retrieves transactions from the log. `start` is inclusive; `None` starts at
the beginning. `end` is exclusive; `None` runs through the captured endpoint.
Bounds accept logical T, transaction entity IDs or instants. Instant bounds
select the first transaction at or after the millisecond. Reversed or disjoint
ranges are empty.

The log endpoint is captured at the method call and retained across all
chunks. Each successful item is `LogTransaction { t, data }`, where `t` is
the logical transaction coordinate and `data: Vec<Datom>` contains its
assertions and retractions. The stream ends after the requested range or
after delivering an error. `chunk_rows` counts transactions, not datoms or
bytes. Apply offset/limit while consuming if needed, checking skipped items
for errors. See [LogValue](types/08_log.md) for retention and authentication.

## Speculative transaction: `DatabaseValue::with`

```rust,ignore
fn with(&self, ops: &[TxOp], tx_instant: i64)
    -> Result<SpeculativeTransactionReport, SemanticError>;
fn with_forms(&self, forms: &[TxForm], tx_instant: i64)
    -> Result<SpeculativeTransactionReport, SemanticError>;
fn with_edn(&self, text: &str, tx_instant: i64)
    -> Result<SpeculativeTransactionReport, SemanticError>;
```

Applies transaction data to a database value or the `db_after` of a prior
speculative transaction. Returns a database value as if the data were applied
in a transaction, leaving the durable database unaffected. The report has
`db_before`, `db_after`, `tx_data` and `tempids`.

There is no async speculative transaction method. Use the native method on a
blocking worker for storage-backed values. Data forms use the same transaction
semantics, but need an explicit Unix-millisecond instant and no request key.
History is rejected. Temporal/custom read filters apply to the resulting view,
not transaction generation or invariant checking. Speculation does not advance
the connection or reserve durable IDs. Its `db_after` can be queried through
`AsyncExecutor` as an explicit database source. See
[speculation](04_native_api_reference.md#speculative-transactions-databasevaluewith).

## Database for speculation

There is no separate `with_db` preparation or special async database type.
Capture a value with `client.db()?.await?`, then apply native `with`,
`with_forms` or `with_edn` off the future executor thread. Chain later
speculation from `report.db_after`. `Database::bootstrap()?.database_value()`
supplies an in-memory starting point; see the complete
[in-memory example](04_native_api_reference.md#speculative-transactions-databasevaluewith).
