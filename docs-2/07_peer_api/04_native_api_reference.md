# Native Rust API reference

The native peer API is exported by the `edb_core` crate. A `Connection` manages
an advancing read handle and, when attached, a transaction endpoint. A
`DatabaseValue` is an immutable value for query, Pull, entity navigation, temporal
views, and raw index access. `Database` supplies an eager in-memory value for
fixtures and pure computation.

Methods below use Rust types and return `Result<T, SemanticError>` where an
operation can fail. `Option<T>` represents an absent value. Signatures are shown
with their owning type; imports and implementation bodies are omitted. See
[connections](00_connections.md), [async clients](01_async_client.md), and
[native computation](02_native_computation.md) for application setup and callback
registration.

For fields and methods organized by type, see the [library type reference](05_types.md).

`SemanticError` exposes a stable `category`, a `code`, a human-readable
`message`, textual `details`, and an optional structured `anomaly` supplied by
program cancellation. Inspect these fields rather than parsing the displayed
message. Transported semantic errors preserve their category and structured
data; both Unix socket and TLS transaction transports record the original remote code in
`details["remote_code"]`. Errors are Rust values, with no Java exception-class
reconstruction or wrapped-exception unwrapping step. A transaction error can
describe an unknown outcome, so receipt of an error alone does not establish
that the transaction failed to commit. See [transaction outcomes](#connectiontransact)
and [error handling](07_error_handling.md).

## Completion listeners: `AsyncOperation::on_complete`

```rust,ignore
fn on_complete(self, callback: impl FnOnce(Result<T, SemanticError>) + Send + 'static);
```

Register a completion callback for an async operation. The callback runs once,
when the operation's work is complete. It consumes the operation instead of
awaiting it. Even if the operation has completed already, the callback runs on a
blocking worker, not immediately on the calling thread. Callback panics are
contained. There is no multiple-listener registration or listener ordering
contract. `AsyncTransaction::on_complete` provides the corresponding transaction
report callback.

## System administration

EDB has no `administer_system` action map, global object-cache release action, or
base-schema upgrade action. Use explicit administration types and methods:

- `storage::PgBlockStore::install(&PostgresConnectionConfig)` installs storage.
- `DatabaseCatalog` manages logical database creation, names, and retirement.
- `PostgresOperator` performs integrity inspection, consolidation, garbage
  collection, fulltext rebuilds, and excision processing.

Normal connection startup does not install storage or create databases.
Application schema changes are transactions. See
[administration](../08_operations/01_administration.md) and
[changing schema](../03_schema/03_changing_schema.md).

## `DatabaseValue::as_of`

```rust,ignore
fn as_of(self, t: u64) -> DatabaseValue;
fn as_of_time_point(self, time_point: TimePoint) -> Result<DatabaseValue, SemanticError>;
```

Returns the value of the database as of some point `t`, inclusive. The typed
`TimePoint` form accepts a transaction number (`T`), transaction entity ID (`Tx`),
or Unix epoch milliseconds (`Instant`). Clone the database value first to retain
the original handle. Repeated calls replace the prior upper boundary.

The immutable basis, schema, and ident dictionary remain those of the captured
value. Instants are less precise than transaction coordinates: an exact
millisecond match selects the earliest matching transaction when several share
that instant. Use `T` or `Tx` for an exact transaction boundary. See
[database values](../02_core_concepts/00_database_values.md).

## `DatabaseValue::as_of_t`

```rust,ignore
fn as_of_t(&self) -> Option<u64>;
```

Returns the as-of point, or `None` if none.

## Attribute information

```rust,ignore
// DatabaseValue
fn schema(&self) -> &Schema;
fn has_avet(&self, attribute: &AttributeName) -> Result<bool, SemanticError>;
// Schema
fn attribute(&self, id: u32) -> Result<&Attribute, SemanticError>;
fn resolve_ident(&self, ident: &Keyword) -> Option<u32>;
```

Returns information about an attribute by numeric attribute ID. To use an ident,
resolve it through the schema first. `Attribute` exposes named Rust fields:

| Field | Meaning |
| --- | --- |
| `id`, `ident` | Numeric attribute identity and symbolic keyword. |
| `cardinality`, `value_type` | Cardinality and stored value type. |
| `unique` | Optional identity or value uniqueness. |
| `indexed` | Explicit AVET indexing setting. |
| `no_history` | Historical index retention setting. |
| `component` | Whether reference values are component entities. |
| `fulltext` | Whether analyzed fulltext search is enabled. |
| `tuple`, `tuple_discontinued` | Tuple definition and maintenance setting. |
| `predicates` | Attribute predicate names. |

`has_avet` reports whether the AVET projection is usable at this exact captured
value. Indexed or unique schema flags alone do not establish readiness while
durable backfill is pending. The check does not wait or refresh the value.
Attribute IDs are `u32`, distinct from general `u64` entity IDs. See
[Attribute](types/01_attribute.md) for descriptor fields and construction methods,
[the schema reference](../03_schema/02_schema_reference.md) and
[index readiness](../06_indexes/04_index_apis.md#readiness-and-database-views).

## `DatabaseValue::basis_t`

```rust,ignore
fn basis_t(&self) -> u64;
```

Returns the transaction coordinate of the underlying immutable database value.
An as-of or since view does not lower this basis, even when it exposes less
information. Use `as_of_t` and `since_t` to inspect view boundaries.

## Cancellation

EDB has no process-global `cancel` function or anomaly-map throw operation.
Queries use `QueryControl`, whose `cancel` field is an `Arc<AtomicBool>`:

```rust
use edb_core::QueryControl;
use std::sync::atomic::Ordering;

let control = QueryControl::default();
let cancellation = control.cancel.clone();
cancellation.store(true, Ordering::Relaxed);
```

Cancellation is cooperative and returns a `SemanticError` at a control check.
Pull uses `PullControl`; stored and native application computation have their
own controls. A transaction callback rejects an operation by returning an error.
Dropping a transaction future or timing out a ticket wait does not undo an
admitted transaction. See [async admission and cancellation](01_async_client.md).

## `Connection::connect`

```rust,ignore
fn connect(
    connection: &str,
    database_id: impl Into<String>,
    cache_capacity: usize,
) -> Result<Connection, SemanticError>;

fn connect_configured(
    connection: PostgresConnectionConfig,
    database_id: impl Into<String>,
    cache_capacity: usize,
) -> Result<Connection, SemanticError>;
```

Connects to an existing logical database, returning a read connection. The
connection string is a PostgreSQL connection string; the logical database name
is supplied separately. `PostgresConnectionConfig` supplies explicit transport
and I/O policy. There are no DynamoDB, Cassandra, Couchbase, Infinispan, embedded
transactor, or in-memory URI protocols.

The public name resolves once to a stable database identity. Renaming a database
or reusing its old name cannot redirect the connection. Read connections do not
require an attached transaction service and independently observe durable
progress. They are not frozen at connection time. Use `Connection::attach_writer`
with a `TransactionClient` to enable submissions; constructing a read connection
does not acquire writer authority.

Connections are cloneable, shared, and suitable for long-lived use. Repeated
`connect` calls do not promise a process-wide URI cache or the same object.
`connect_configured_with_cache_limits` accepts separate node-cache entry and
accounted-byte limits; these are not limits on total process memory.

For in-memory work, use `Database::bootstrap()` and capture
`database.database_value()`. For offline read-only backup access, use
`BackupConnection::open(directory)` or `open_point(directory, &point)`.
Backup connections support database and log reads without source PostgreSQL.
See [connections](00_connections.md),
[setup](../01_tutorials/01_application_workflow.md), and
[backup reads](../08_operations/05_backup_reads.md).

## Creating a database: `DatabaseCatalog::create_if_absent`

```rust,ignore
fn create_if_absent(
    &mut self,
    name: &str,
    schema: Schema,
) -> Result<CreateDatabaseResult, SemanticError>;
```

Creates the database specified by `name`. The result's `created` field is `true`
if it was created and `false` if it already exists; `database` contains the catalog
entry. Connect the catalog to explicitly installed PostgreSQL storage first.
Creation and connection are separate operations. See
[database lifecycle](../08_operations/02_database_lifecycle.md).

## Datoms: `DatabaseValue::datoms` and `prefix_cursor`

```rust,ignore
fn datoms(&self, order: IndexOrder) -> Result<Vec<Datom>, SemanticError>;
fn scan_cursor(&self, order: IndexOrder) -> Result<DatabaseValueScanCursor<'_>, SemanticError>;
fn datoms_with_prefix(&self, prefix: &IndexPrefix) -> Result<Vec<Datom>, SemanticError>;
fn prefix_cursor(&self, prefix: &IndexPrefix) -> Result<DatabaseValuePrefixCursor<'_>, SemanticError>;
```

Raw access to index data, by index. Supply an `IndexOrder` for an entire index,
or an `IndexPrefix` with leading components to narrow the result.

- `Eavt` and `Aevt` contain all datoms visible in the selected view.
- `Avet` contains datoms for indexed or unique attributes whose projection is
  available at the captured value.
- `Vaet` contains datoms for reference attributes; it is the reverse reference
  index.

Collecting methods return `Vec<Datom>`. Cursors yield
`Result<Datom, SemanticError>` incrementally; a read can fail during iteration.
Datoms have these named fields:

| Field | Type | Value |
| --- | --- | --- |
| `entity` | `u64` | Entity ID. |
| `attribute` | `u32` | Attribute ID. |
| `value` | `Value` | Stored value. |
| `tx` | `u64` | Reified transaction entity ID. |
| `source` | `u64` | Source entity ID for the assertion or retraction. |
| `added` | `bool` | `true` for assertion, `false` for retraction. |

Datom reads retain distinct source contributions for the same entity, attribute,
and value.

AVET readiness and error behavior differ between eager prefixes, durable
prefixes, and attribute-qualified seek boundaries. Check `has_avet` when relying
on an attribute projection; unrestricted scans omit unavailable projections. See
[index readiness](../06_indexes/04_index_apis.md#readiness-and-database-views).

Prefixes restrict the entire result to matching components. They support up to
E/A/V in the chosen index order. For a transaction component or a nonmatching
starting position, use a seek boundary. See
[index APIs](../06_indexes/04_index_apis.md).

## `Connection::db`

```rust,ignore
fn db(&self) -> DatabaseValue;
```

Retrieves a value of the database for reading. Capturing it performs no
PostgreSQL I/O and does not communicate with the transaction service. The value
is immutable and remains unchanged as the connection advances. Subsequent cold
reads against the value can require storage I/O.

## `DatabaseValue::db_stats`

```rust,ignore
fn db_stats(&self) -> Result<DatabaseStats, SemanticError>;
fn db_stats_with_control(&self, control: &QueryControl) -> Result<DatabaseStats, SemanticError>;
```

Queries for database statistics. `DatabaseStats.datoms` is the total count of
history datoms, including assertions and retractions; `attrs` maps numeric
attribute IDs to `AttributeStats { count }`. Temporal and custom filters remain
in force. This is a history scan, not a constant-time metadata lookup, entity
count, or storage-size estimate. The controlled form observes cancellation,
timeout, and visible-datom work limits.

## Deleting a database: retirement and reclamation

```rust,ignore
// DatabaseCatalog
fn retire(&mut self, name: &str) -> Result<Option<DatabaseCatalogEntry>, SemanticError>;
```

Retires the active database named by `name`, returning its retired catalog entry,
or `None` when no active name exists. Retirement is a logical lifecycle operation;
it is distinct from physical reclamation. `PostgresOperator` provides
`preview_retired_database_reclamation` and `reclaim_retired_database` for that
separate task. See [database lifecycle](../08_operations/02_database_lifecycle.md).

## `DatabaseValue::entid`

```rust,ignore
fn entid(&self, ident: &Keyword) -> Option<u64>;
fn resolve_entity_identifier(&self, identifier: &EntityIdentifier) -> Result<Option<u64>, SemanticError>;
```

Returns the entity ID associated with a symbolic keyword, or `None` if absent.
For an ID, ident, or lookup reference through one API, use
`resolve_entity_identifier`. Rust `entid` itself accepts only a keyword.

## `DatabaseValue::entid_at`

```rust,ignore
fn entid_at(
    &self,
    partition: &EntityIdentifier,
    time_point: TimePoint,
) -> Result<u64, SemanticError>;
```

Returns a fabricated entity ID in the supplied named or implicit partition,
placing the resolved transaction coordinate in its entity-index component.
For an instant, it resolves the first transaction at or after that instant.

Entity IDs sort by partition, then entity-index component. Ordinary EDB entity
allocation uses an independent issuance frontier, so this fabricated ID is
**not a creation-time cutoff** for ordinary entities. Do not use it to infer
which entities were created after a transaction. Use the transaction log or an
explicit indexed creation-time attribute for that purpose. See
[partitions](../04_transactions/02_partitions.md).

## `DatabaseValue::entity`

```rust,ignore
fn entity(&self, identifier: impl Into<EntityIdentifier>) -> Result<Option<Entity>, SemanticError>;
```

Returns a lazy entity view for the given ID, ident, or lookup reference. An
unresolved ident or lookup reference returns `None`. Numeric IDs can produce an
entity view even when it has no facts. `Entity` supplies `get`, `get_named`,
`keys`, reverse navigation, and `pull`; it does not implement Clojure collection
interfaces. Reference values navigate to entities backed by the same immutable
database. History values cannot back an entity. See
[entities](../02_core_concepts/04_entities.md).

## Entity database: `Entity::database`

```rust,ignore
fn database(&self) -> DatabaseValue;
```

Returns the database value that is the basis for this entity.

## `DatabaseValue::filter`

```rust,ignore
fn filter<F>(self, predicate: F) -> DatabaseValue
where
    F: Fn(&DatabaseValue, &Datom) -> bool + Send + Sync + 'static;
```

Returns the value of the database containing only datoms satisfying the
predicate. The predicate receives two arguments: the same temporal/history
value with custom predicates removed, and a datom. Chained calls compose the
predicates with logical AND. Keep predicates pure. Capturing a filtered value
does not eagerly scan it; predicates are evaluated as data is read. See
[database filters](../02_core_concepts/03_database_filters.md).

## Function construction

EDB does not compile a map containing `:lang`, `:params`, `:code`, `:imports`, or
`:requires` into a Clojure or Java function. Native Rust callbacks are registered
with `TxFunctions`, `NativeRegistryBuilder`, or `QueryExtensions`. Persistable
functions are typed `Program` values with a kind, arity, and instructions;
they are validated and installed as content-addressed program data, then bound
through `:db/fn`.

Use [native computation](02_native_computation.md) for compiled application
functions and [persisted programs](../04_transactions/01_persisted_programs.md)
for portable program deployment. Native closures are not serialized into the
database.

## Garbage collection: `PostgresOperator::collect_garbage`

```rust,ignore
fn collect_garbage(&mut self, older_than: Duration) -> Result<GarbageInventory, SemanticError>;
```

Allows storage to reclaim garbage older than the supplied age. Collection is
bounded and resumable; a completed cycle does not imply global quiescence or
revoke still-owned values. `garbage_inventory(older_than)` inspects collector
state without starting a cycle. See
[maintenance controls](../08_operations/04_maintenance_controls.md).

## Database names: `DatabaseCatalog::list`

```rust,ignore
fn list(&mut self, after_name: Option<&str>, limit: usize) -> Result<Vec<DatabaseCatalogEntry>, SemanticError>;
```

Returns a page of active database catalog entries in name order. Supply the last
name as `after_name` to continue. EDB does not use a wildcard database URI for
listing. Retired entries have a separate `list_retired` API.

## `DatabaseValue::history`

```rust,ignore
fn history(self) -> DatabaseValue;
```

Returns a special database containing retained assertions and retractions across
time. This value can be used for raw index access and queries, but not for
entity navigation, Pull, or transactions. As-of, since, and custom predicate
bounds remain in force. Queries can inspect source in the fifth datom field and
distinguish assertions and retractions with the sixth:

```edn
[?e ?a ?v ?tx ?source ?added]
```

`?source` is the entity ID to which the statement is attributed; a retraction's
source does not change the source of the earlier assertion. `?added` is `true`
for an assertion. Historical indexes can omit data affected by
no-history retention or excision. The captured transaction log is a separate
API. See [indexes and log](../06_indexes/00_indexes_and_log.md).

## `DatabaseValue::ident`

```rust,ignore
fn ident(&self, entity: u64) -> Option<&Keyword>;
```

Returns the keyword associated with an entity ID, or `None` when no ident exists.
The Rust method takes an ID; an already known keyword needs no conversion.

## `implicit_part`

```rust,ignore
fn implicit_part(number: u32) -> Result<u64, SemanticError>;
```

Returns the implicit partition entity ID corresponding to `number`, where
`0 <= number < 524288`. It requires no installation transaction. The result is
an entity ID, not raw partition bits.

## `implicit_part_id`

```rust,ignore
fn implicit_part_id(partition: u64) -> Result<Option<u32>, SemanticError>;
```

Returns the number of the given implicit partition, in the same range. Returns
`None` when the valid entity ID is not an implicit partition base; malformed
entity representations return an error.

## `DatabaseValue::index_pull`

```rust,ignore
fn index_pull(&self, options: IndexPullOptions) -> Result<IndexPullCursor<'_>, SemanticError>;
```

Walks an index, pulling entities via E in AVET or reference V in AEVT using a
selector, returning a lazy fallible iterator.

| `IndexPullOptions` field | Meaning |
| --- | --- |
| `start` | AVET or AEVT `IndexBoundary`; at least the attribute must be supplied. |
| `selector` | A `PullPattern`. |
| `reverse` | When true, iterate in reverse order; default false. |
| `offset` | Skip this many datoms before projection; default zero. |
| `limit` | Maximum projected results; `None` means unlimited. |

Iteration is limited to datoms matching the starting attribute, not the whole
supplied prefix. AEVT requires a reference attribute. Results are not deduplicated.
`index_pull_with_control` additionally accepts `&PullControl`. See
[index Pull](../06_indexes/03_index_pull.md).

## Index ranges

```rust,ignore
// Eager Database
fn avet_range(
    &self,
    attribute: u32,
    start: Option<&Value>,
    end: Option<&Value>,
) -> Result<&[Datom], SemanticError>;
```

Returns datoms for an AVET attribute starting at `start`, or from the beginning
if `None`, and ending before `end`, or through the attribute's end if `None`.
The attribute must be indexed or unique. Equal bounds return an empty range;
a reversed range is rejected.

For an immutable `DatabaseValue`, use an AVET `seek_cursor` and stop when the
attribute changes or the value reaches the exclusive upper bound. This preserves
temporal and filter semantics and can avoid materializing the range. There is
no exported `DatabaseValue::index_range` method. See
[index-range examples](../06_indexes/04_index_apis.md#index-range-api).

## `DatabaseValue::invoke`

```rust,ignore
fn invoke(
    &self,
    function: impl Into<EntityIdentifier>,
    arguments: &[RuntimeValue],
    control: InvokeControl<'_>,
) -> Result<ProgramOutput, SemanticError>;
```

Looks up the stored database function named by ID, ident, or lookup reference
and calls it with arguments against this exact database value. This does not
advance a connection or change durable data. Query programs return rows;
predicates return their runtime value; transaction programs return inert
transaction forms. Returned forms and nested calls are not automatically
transacted or recursively invoked. Required stored program content must be
available; native deployment symbols are not arbitrary dynamic Rust lookup.
See [transaction functions](../04_transactions/07_transaction_functions.md).

## `DatabaseValue::is_filtered`

```rust,ignore
fn is_filtered(&self) -> bool;
```

Returns true if a custom predicate has been set with `filter`. Temporal bounds
alone do not make this return true.

## Listing backups

```rust,ignore
// PortableBackup
fn list_backup_points(directory: &std::path::Path) -> Result<Vec<BackupPoint>, SemanticError>;
fn list_backups(directory: &std::path::Path) -> Result<Vec<u64>, SemanticError>;
```

Lists points available in a backup directory. `list_backup_points` sorts
ascending by `(basis_t, log_generation)`. Each point identifies its lineage,
generation, basis, and manifest hash, with object-write/reuse counts.
`list_backups` returns deduplicated basis coordinates in ascending order.

Pass the directory and a `&BackupPoint` to `BackupConnection::open_point`
to select an exact point. The listing contains no generated connection URI.
See [backup reads](../08_operations/05_backup_reads.md).

## `Connection::log`

```rust,ignore
fn log(&self) -> LogValue;
```

Retrieves an immutable value of the transaction log for `tx_range` or queries.
It captures the connection's current endpoint without contacting the writer.
`QueryDataSource::log` supplies it as a named query source. `Peer` and
`BackupConnection` also provide `log`; durable database values can obtain it
through `DatabaseValue::log_value()`.

## Next transaction coordinate

There is no public `DatabaseValue::next_t` method. Compute
`db.basis_t().checked_add(1)` when an exclusive successor boundary is needed.
This is a coordinate calculation, not a transaction reservation or a promise
about a future commit. Use `None` as a log-range end to read through the captured
log's end.

## Partition: `partition_eid`

```rust,ignore
fn partition_eid(entity: u64) -> Result<u64, SemanticError>;
fn eid_to_part(eid: u64) -> Result<u32, SemanticError>;
```

`partition_eid` returns the partition entity ID associated with an entity ID.
`eid_to_part` returns the raw partition bits. These are distinct representations,
particularly for implicit partitions. See
[partitions](../04_transactions/02_partitions.md).

## `DatabaseValue::pull`

```rust,ignore
fn pull(&self, pattern: &PullPattern, entity: impl Into<EntityIdentifier>) -> Result<QueryValue, SemanticError>;
```

Like `pull_many`, but takes a single entity identifier and returns one hierarchical
selection. `pull_with_control` additionally accepts `&PullControl` for traversal
limits and cooperative cancellation.

## `DatabaseValue::pull_many`

```rust,ignore
fn pull_many<I, E>(&self, pattern: &PullPattern, entities: I) -> Result<Vec<QueryValue>, SemanticError>
where
    I: IntoIterator<Item = E>,
    E: Into<EntityIdentifier>;
```

Returns hierarchical selections of attributes for the supplied entity
identifiers, in input order. `PullPattern` is the native selector type;
`edn_pull::parse_pull_edn` parses an EDN selector. Pattern validation precedes
entity traversal, including for an empty input collection. History values are
not valid Pull sources. See [the Pull reference](../05_query_and_pull/06_pull_reference.md).

Pull does not accept an `:io-context` options map or change its return shape to
`{:ret ... :io-stats ...}`. Native read diagnostics use operation contexts and
the diagnostic types described in
[observability](../08_operations/03_observability.md).

## Query execution: `DatabaseValue::query`

```rust,ignore
fn query(
    &self,
    query: &Query,
    inputs: &[QueryInput],
    control: &QueryControl,
) -> Result<QueryOutcome, SemanticError>;
```

Executes a query against the captured database, supplied as source `$`, and
additional inputs. `QueryEngine::execute_sources` accepts named database,
relation, or log sources through `QueryDataSource`. Inputs can bind scalars,
tuples, collections, or relations; rules and query clauses are represented in
the native `Query`.

EDN query text can be a vector/list or map:

```edn
[:find ?name
 :in $ ?minimum
 :where [?person :person/age ?age]
        [(>= ?age ?minimum)]
        [?person :person/name ?name]]
```

```edn
{:find [?name]
 :in [$ ?minimum]
 :where [[?person :person/age ?age]
         [(>= ?age ?minimum)]
         [?person :person/name ?name]]}
```

Use `edn_query::parse_query_edn(text)`, bind its ordered `EdnQueryArgument`
inputs, then call `BoundEdnQuery::execute`. If no `:in` section is supplied, the
EDN template expects the default database source. `:with` is optional and names
variables retained in the aggregation set but not returned.

`QueryOutcome.result` is a `QueryResult` with relation, collection, scalar, or
tuple shape according to the find specification. Outcomes also carry statistics,
plans, and optional diagnostics. Native engine execution uses a bounded shared
prepared-query cache for eligible structural query forms. `PreparedQuery` and
`PreparedQueryCache` also provide explicit reusable preparation. EDN parsing
itself is a separate step; a bound EDN query retains its prepared form.
See [query execution](../05_query_and_pull/07_executing_queries.md) and
[the query reference](../05_query_and_pull/05_query_reference.md).

## Query sequences: `DatabaseValue::query_sequence`

```rust,ignore
fn query_sequence(
    &self,
    query: &Query,
    inputs: &[QueryInput],
    control: &QueryControl,
) -> Result<QuerySequence, SemanticError>;
```

Performs relational query work, returning a fallible sequence of result tuples.
Item transformations such as Pull are deferred until the sequence is consumed.
This avoids eagerly allocating every pulled result and allows processing one
projected row at a time. Relational work and its rows are still eager; this is
not a fully streaming query evaluator.

`QuerySequence::remaining_rows()` returns the number of pending relational rows
without performing their Pull projections. Each iterator item can fail, and the
original query deadline and shared budgets continue through lazy projection.
Bound EDN queries expose the corresponding `sequence(control, extensions)` API.

## Query options and diagnostics

`QueryControl` supplies `timeout: Option<Duration>`, cancellation, work and row
limits, allocation allowances, and optional `QueryDiagnosticOptions`. Pass it
to `query`, a prepared query, or bound EDN execution; there is no separate native
query-options map API.

The optional timeout is the duration after which a query may be stopped. Timeout
is cooperative and approximate; it does not guarantee interruption at precisely
the duration specified, particularly inside a storage call or native callback.
EDB returns `QueryOutcome` rather than an I/O-wrapped EDN result map. See
[query diagnostics](../05_query_and_pull/04_diagnostics.md) for execution reports
and [observability](../08_operations/03_observability.md) for storage I/O reports.

For `AsyncClient::query`, the timeout includes time waiting in the worker queue.
The worker receives only the remaining time; work whose deadline has already
elapsed is rejected before query execution. Dropping an ordinary read future
can skip a read that has not started. A running read still observes its own
cooperative cancellation and work controls.

## Releasing a connection

There is no explicit `Connection::release` method. Drop connection handles when
they are no longer needed. Clones share the connection; dropping one clone does
not release all other owners. Captured database values have their own immutable
ownership. Native last-drop cleanup can join associated resources; see
[async ownership](01_async_client.md) when working on an executor thread.

Holding a database value does not register a storage pin or extend the retention
grace period. Its immutable identity remains unchanged, but cold reads still
require the referenced durable objects to exist. Configure retention to cover
the lifetime of active reads; see
[retention and reclamation](../08_operations/00_deployment.md#inspection-retention-and-reclamation).

## `Connection::remove_transaction_report_queue`

```rust,ignore
fn remove_transaction_report_queue(&self) -> bool;
```

Removes the queue associated with this connection and releases its unconsumed
reports, including their immutable db-before/db-after values. Returns whether a
queue was removed.

## Renaming a database: `DatabaseCatalog::rename`

```rust,ignore
fn rename(&mut self, name: &str, new_name: &str) -> Result<DatabaseCatalogEntry, SemanticError>;
```

Renames the database specified by `name` and returns its updated catalog entry.
`rename_checked` also accepts an expected lineage to guard the identity currently
under that name. Existing connections keep their resolved identity. See
[database lifecycle](../08_operations/02_database_lifecycle.md).

## `Connection::request_index`

```rust,ignore
fn request_index(&self) -> Result<IndexRequest, SemanticError>;
```

Schedules asynchronous indexing through an attached writer. The receipt contains
`target_t`, a finite committed target, and `scheduled`, indicating whether this
call added demand. A false `scheduled` can mean existing work already covers the
target or the target is already published. Neither value indicates completion.
Use `sync_index(target_t, timeout)` to wait for it.

A writer-independent connection has no maintenance control endpoint and returns
`connection/no-writer`. Dropping the receipt or timing out the wait does not
cancel shared background work. See
[background indexing](../06_indexes/02_background_indexing.md).

## Resolving tempids

Transaction reports expose `tempids: BTreeMap<String, u64>`. Resolve the tempid
using the map from the transaction that allocated or upserted it:

```rust,ignore
let entity_id = report.tempids.get("person").copied();
```

The map must come from that speculative or committed transaction report.
There is no separate database-dependent `resolve_tempid` function. A speculative
mapping does not reserve IDs for a later durable transaction.

## Reverse seek: `DatabaseValue::reverse_seek_cursor`

```rust,ignore
fn reverse_seek_cursor(&self, boundary: &IndexBoundary) -> Result<DatabaseValueScanCursor<'_>, SemanticError>;
```

Like `seek_cursor`, but iterates the index in reverse, beginning at or before
the position specified by the supplied components. With a partial boundary,
reverse traversal starts after the highest matching suffix. It terminates only
at the start of the index; supply your own range termination logic.
`collect_reverse_seek_datoms` collects the same traversal. See
[reverse seek](../06_indexes/05_rseek_datoms.md).

## Seek: `DatabaseValue::seek_cursor`

```rust,ignore
fn seek_cursor(&self, boundary: &IndexBoundary) -> Result<DatabaseValueScanCursor<'_>, SemanticError>;
```

Raw index access beginning at or after the position where the supplied leading
components would reside. There need not be an exact match. Unlike a prefix
cursor, traversal is not bounded by the supplied components and only terminates
at the end of the index. Supply your own termination logic, since applications
rarely want the entire remainder. Prefer a prefix cursor wherever it is adequate.

Build schema-aware boundaries with `eavt_boundary`, `aevt_boundary`,
`avet_boundary`, or `vaet_boundary`. `IndexComponents` accepts zero through four
leading components in index order; the fourth is an `IndexTransaction`.
`collect_seek_datoms` explicitly materializes the traversal. Index coverage and
returned datom fields are described under [Datoms](#datoms-databasevaluedatoms-and-prefix_cursor).

## Shutdown

EDB has no process-global peer shutdown function or Clojure runtime to shut down.
Own and stop the resources your application starts. `TransactionService::shutdown`
and `TransactionStandby::shutdown` consume and stop those services. Connections
use Rust ownership; async workers remain alive for admitted work and cleanup.
Before process exit, wait for any transaction outcome your application needs to
know. See [async clients](01_async_client.md).

## `DatabaseValue::since`

```rust,ignore
fn since(self, t: u64) -> DatabaseValue;
fn since_time_point(self, time_point: TimePoint) -> Result<DatabaseValue, SemanticError>;
```

Returns the value of the database since some point `t`, exclusive. The typed
form accepts a transaction number, transaction entity ID, or instant in Unix
milliseconds. Repeated calls replace the lower boundary. The immutable basis,
schema, and ident dictionary are unchanged. An instant resolving to the earliest
transaction in a millisecond can leave later transactions from that same
millisecond visible. Use transaction coordinates when that distinction matters.

## `DatabaseValue::since_t`

```rust,ignore
fn since_t(&self) -> Option<u64>;
```

Returns the since point, or `None` if none.

## `squuid`

```rust,ignore
fn squuid() -> Result<u128, SemanticError>;
fn squuid_at(unix_millis: u64) -> Result<u128, SemanticError>;
```

Constructs a semi-sequential UUID with Unix epoch seconds in the most significant
32 bits and randomness in the remaining non-version/non-variant bits. This groups
UUIDs by coarse time in index order; it does not guarantee strict ordering within
a second or after clock regression. Store it as `Value::Uuid`.

`squuid_at` supplies an explicit clock. Clock range and operating-system
randomness failures return errors. EDB also exports `uuid_v7` and `uuid_v7_at`
for millisecond timestamp UUIDs.

## `squuid_time_millis`

```rust,ignore
fn squuid_time_millis(uuid: u128) -> u64;
```

Gets the time part of a UUID created by `squuid`, as Unix epoch milliseconds.
Its precision is one second. This merely extracts the leading bits; it cannot
prove that an arbitrary UUID was generated by `squuid`.

## `Connection::sync` and `sync_to`

```rust,ignore
fn sync(&self) -> Result<DatabaseValue, SemanticError>;
fn sync_to(&self, target_t: u64, timeout: Duration) -> Result<DatabaseValue, SemanticError>;
```

Used to coordinate with other peers. `sync` captures and adopts the newest
durable head visible to its storage read. It communicates with PostgreSQL,
not a separate transaction endpoint.

`sync_to` waits until the connection has adopted a database value with
`basis_t >= target_t`. Its wait uses locally observed progress; the background
observer performs storage catch-up. It does not send a request to a separate
transactor endpoint. Supply a finite timeout.

`db` is the preferred way to get a database value when no coordination is
required, because capture performs no I/O and need not wait. When a transaction
basis is available, pass it to `sync_to`. These native methods are synchronous;
`AsyncClient` provides future-returning counterparts. See
[synchronization](../04_transactions/09_synchronization.md).

## `Connection::sync_excise`

```rust,ignore
fn sync_excise(&self, target_t: u64, timeout: Duration) -> Result<DatabaseValue, SemanticError>;
```

Coordinates with background excision. Waits for a value aware of excision
processing through the supplied target. It does not itself request new excision
work. Completion can be immediate when the condition is already satisfied.
Use a finite timeout; storage observation can perform I/O. See
[synchronization](../04_transactions/09_synchronization.md).

## `Connection::sync_index`

```rust,ignore
fn sync_index(&self, target_t: u64, timeout: Duration) -> Result<DatabaseValue, SemanticError>;
```

Coordinates with background indexing. Waits for physical indexed-through
progress covering the target; logical basis advancement alone is insufficient.
It does not schedule indexing. Use `request_index` when an attached writer
should schedule work, then wait on its finite target. A timeout does not cancel
that work.

## `Connection::sync_schema`

```rust,ignore
fn sync_schema(&self, target_t: u64, timeout: Duration) -> Result<DatabaseValue, SemanticError>;
```

Coordinates with schema-index readiness through the supplied target. The returned
value has reached the target basis and has no pending AVET addition at or before
that target. It need not wait for later schema transitions or for the general
indexed-through frontier required by `sync_index`. This is a wait, not a request
to perform a migration. See
[background indexing](../06_indexes/02_background_indexing.md) and
[synchronization](../04_transactions/09_synchronization.md).

## Transaction ID: `t_to_tx`

```rust,ignore
fn t_to_tx(t: u64) -> Result<u64, SemanticError>;
```

Returns the reified transaction entity ID associated with a logical transaction
coordinate. Rejects an out-of-range coordinate.

## Tempids: `EntityRef::Temp`

Use `EntityRef::Temp(String)` in typed transaction forms, or string tempids in
EDN transaction data. Within the scope of one transaction, a tempid maps
consistently to a permanent ID. There is no global tempid generator or reserved
negative-number tempid range in this Rust API.

For partition allocation, use `TransactionDefaults`, `TxOp::ForcePartition`, or
`TxOp::MatchPartition`. Partition directives affect new allocation; they do not
move an existing entity found by identity upsert. See
[transaction data](../04_transactions/04_transaction_data.md) and
[partitions](../04_transactions/02_partitions.md).

## `Entity::touch`

```rust,ignore
fn touch(&self) -> Result<Entity, SemanticError>;
```

Touches all forward attributes of the entity, recursively including component
entities, and returns the entity with its lazy cache populated. Ordinary
noncomponent references remain entity references; reverse attributes are not
recursively expanded. Use Pull when a specific returned data shape is required.

## `Connection::transact`

```rust,ignore
fn transact(
    &self,
    request: TransactionRequest,
    timeout: Duration,
) -> Result<ServiceTransactionReport, SemanticError>;
```

Given a connection with an attached transaction endpoint and a set of information
in a `TransactionRequest`, submits a transaction and blocks until a result or
wait error is available. Construct requests from primitive operations, forms,
or EDN using `TransactionRequest::new`, `from_forms`, or `from_edn`. Every durable
request includes a request key for exact retry handling. Each assertion and
retraction supplies a source entity ID in its transaction form; a single request
can include several sources. Supply `TxOp::TxInstant { source }` (or
`[:db/txInstant source]` in EDN) to attribute the selected transaction instant,
or assert `:db/txInstant` explicitly with a source.

A successful report includes `db_before`, `db_after`, `tx_data`, and `tempids`,
plus `basis_t`, `tx_hash`, `replayed`, and optional diagnostics. These database
values describe the exact committed transition. The connection attempts ordered
local observation before returning a successful report; an observation fault is
separate from durable success and is exposed by `observation_error()`.

An error may mean rejection or an unknown transaction outcome after a wait or
communication failure. Do not assume that a timeout rolled back the transaction.
Retain the same request key and transaction data when retrying an unknown outcome.
See [transaction processing](../04_transactions/06_processing_transactions.md)
and [diagnostics](../04_transactions/03_diagnostics.md).

## Asynchronous submission: `Connection::submit`

```rust,ignore
fn submit(&self, request: TransactionRequest) -> Result<ConnectionTransactionTicket, SemanticError>;
// ConnectionTransactionTicket
fn wait(self, timeout: Duration) -> Result<ServiceTransactionReport, SemanticError>;
```

Admits a transaction and returns a ticket without waiting for its result.
`wait` consumes the ticket and waits for the report, coordinating local adoption
within the remaining deadline. `AsyncClient::transact` provides an
`AsyncTransaction` that can be awaited or consumed through `on_complete` on a
Rust async executor. See [async clients](01_async_client.md).

Admission is bounded. `Connection::submit` performs request validation and
canonical encoding before attempting to enqueue; a full service queue returns
`Busy` with code `service/queue-full` instead of waiting for capacity.
`AsyncClient::transact` also reserves a bounded worker slot and returns
`async/operation-capacity` when none is available. That admission error means
the request was not submitted by this call. An `AsyncTransaction` is a worker
admission receipt, not proof of a durable commit.

Once admitted to the async worker, a transaction remains owned even if its
waiter is dropped. Its deadline can still expire before execution, and execution
can still reject it. Dropping the waiter abandons the result; it does not request
transaction cancellation. Abandoned async results and retained resources are
disposed on workers.

## Transaction coordinate: `tx_to_t`

```rust,ignore
fn tx_to_t(tx: u64) -> Result<u64, SemanticError>;
```

Returns the logical transaction coordinate associated with a transaction entity
ID. Rejects IDs outside the transaction partition.

## `LogValue::tx_range`

```rust,ignore
fn tx_range(&self, start: Option<TimePoint>, end: Option<TimePoint>) -> Result<LogCursor, SemanticError>;
```

Returns a range of transactions in the captured log, starting at `start`, or
from the beginning if `None`, and ending before `end`, or through the captured
end if `None`. Bounds can be transaction coordinates, transaction entity IDs,
or instants. Instant bounds select the first transaction at or after the given
millisecond. Reversed or disjoint ranges are empty.

The cursor yields `Result<LogTransaction, SemanticError>`. Each transaction has:

| Field | Meaning |
| --- | --- |
| `t` | Logical transaction coordinate. |
| `data` | `Vec<Datom>` asserted or retracted by that transaction. |

The log includes retained transaction data even when no-history index
consolidation has removed it from history indexes. Excision generation and
retention still matter. See [indexes and log](../06_indexes/00_indexes_and_log.md).

## Transaction report queue

```rust,ignore
// Connection
fn enable_transaction_reports(&self) -> bool;
fn try_next_transaction_report(&self) -> Option<ServiceTransactionReport>;
fn next_transaction_report(&self, timeout: Duration) -> Result<Option<ServiceTransactionReport>, SemanticError>;
```

Enables the single data queue associated with this connection, creating it if
necessary. At any point, zero or one queue is associated with the shared
connection. Reports begin with the next adopted transaction; enabling the queue
does not manufacture reports for already-observed transactions.

The queue is unbounded and does not apply backpressure to commits. It consumes
memory, including retained database values, until reports are consumed or the
queue is removed. Reports carry the same exact `db_before`, `db_after`, and
`tx_data` fields as transaction results. Do not depend on relative delivery
ordering between a ticket waiter and the report queue.

For resumable log delivery with durable acknowledgments, use
[change consumers](03_change_consumers.md). A report queue alone is not a durable
subscription.

Commit notifications are separate, discardable wakeup hints. They do not carry
authoritative transaction data, and losing one does not change the transaction
outcome. The observer repairs gaps by reading durable state after reconnect and
at periodic catch-up intervals. Change consumers replay the authenticated log
from their persisted checkpoint; acknowledging progress writes checkpoint
references and requires credentials authorized for those writes. A read-only
peer login cannot acknowledge a consumer, and a checkpoint is not a retention
pin or a guarantee of exactly-once external effects.

## Speculative transactions: `DatabaseValue::with`

```rust,ignore
fn with(&self, ops: &[TxOp], tx_instant: i64) -> Result<SpeculativeTransactionReport, SemanticError>;
fn with_forms(&self, forms: &[TxForm], tx_instant: i64) -> Result<SpeculativeTransactionReport, SemanticError>;
fn with_edn(&self, text: &str, tx_instant: i64) -> Result<SpeculativeTransactionReport, SemanticError>;
```

`with` is a pure function that takes a database value and a set of information
held to be true at a supplied transaction instant, and returns a new database
value incorporating that information. It does not persist a transaction,
advance a connection, or reserve entity IDs.

Transaction data is semantically an unordered set of information. Syntactically,
`with` accepts primitive operations; `with_forms` also accepts entity maps and
transaction calls; `with_edn` parses the EDN representation. Each assertion and
retraction carries an explicit source. Both durable and speculative transactions
also supply attribution for `:db/txInstant`: use `TxOp::TxInstant { source }`,
where `source` is an `EntityRef`, or an explicit sourced `TxOp::Add` of the
transaction instant. Wrap the operation in `TxForm::Op` when supplying forms.
The EDN form `[:db/txInstant source]` is shown below. It asserts the selected
instant and supplies no default source for other forms. A source-only entity
map emits no assertion.

Each transaction has one distinct instant. Contributions from different sources
at that instant are retained; assertions of different instants are rejected.
The `tx_instant` argument supplies the value, so the transaction data must still
supply a source through the dedicated form or an explicit timestamp assertion.
If the data is valid, the report contains:

| Field | Meaning |
| --- | --- |
| `db_before` | Database value before the transaction. |
| `db_after` | Database value after the transaction. |
| `tx_data` | Datoms produced by the transaction. |
| `tempids` | Tempid-to-entity-ID mapping. |

Supply Unix epoch milliseconds explicitly as `tx_instant`. History values
cannot transact. Read filters, including as-of/since, apply to the returned
view, not to transaction generation or invariant checking. `with_functions`
supplies process-local callbacks; `with_forms_with_execution_options` supplies
an explicit native deployment registry and allocation defaults. Limits are
available through `SpeculationLimits`.

There is no `:io-context` or `:return-hints` options map. Diagnostics and optional
prefetch hints have separate native APIs; see
[transaction hints](../09_optional/01_transaction_hints.md).

This complete in-memory example installs schema, applies information, resolves a
tempid, reads an entity, and retains an earlier view:

```rust
use edb_core::{Database, Keyword, SemanticError};

fn example() -> Result<(), SemanticError> {
    let initial = Database::bootstrap()?.database_value();
    let schema = initial.with_edn(r#"[
      [:db/txInstant :db.source/system]
      {:db/ident :person/name
       :db/source :db.source/system
       :db/valueType :db.type/string
       :db/cardinality :db.cardinality/one}
    ]"#, 1_000)?;
    let report = schema.db_after.with_edn(r#"[
      [:db/txInstant :db.source/system]
      {:db/id "person" :db/source :db.source/system :person/name "Ada"}
    ]"#, 2_000)?;
    let id = report.tempids["person"];
    let entity = report.db_after.entity(id)?.expect("resolved entity ID");
    assert!(entity.get_named(&edb_core::AttributeName::Ident(
        Keyword::new("person", "name"),
    ))?.is_some());
    assert_eq!(entity.database().basis_t(), report.db_after.basis_t());
    let earlier = report.db_after.clone().as_of(schema.db_after.basis_t());
    assert_eq!(earlier.as_of_t(), Some(schema.db_after.basis_t()));
    assert_eq!(earlier.basis_t(), report.db_after.basis_t());
    Ok(())
}
```
