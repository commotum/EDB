# Peer and peer entry points

```rust,ignore
pub struct Peer { /* private fields */ }
```

`edb_core::Peer` is a lower-level read handle for an existing database. It captures
immutable database values, executes queries, and explicitly observes durable
progress. It is cloneable and shares its reader and current observation with its
clones. Use [Connection](02_connection.md) as the application entry point for
background observation and, when a transaction endpoint is attached, transaction
submission.

Connection management, database administration, queries, identity helpers, and
service shutdown have separate owners in the Rust API. They are described here
together so that each operation has an explicit entry point. Fallible operations
return `Result<_, SemanticError>`; no global static `Peer` facade is required.

## Method summary

| Entry point | Successful result | Description |
| --- | --- | --- |
| [`Peer::connect`](#connect) | `Peer` | Connects a reader to the specified database. |
| [`Connection::connect`](#connect) | `Connection` | Connects an application reader with background observation. |
| [`DatabaseCatalog::create_if_absent`](#create_if_absent) | `CreateDatabaseResult` | Creates a database with the given name, if absent. |
| [`DatabaseCatalog::rename`](#rename) | `DatabaseCatalogEntry` | Renames a database. |
| [`DatabaseCatalog::retire`](#retire) | `Option<DatabaseCatalogEntry>` | Retires an active database name. |
| [`DatabaseCatalog::list`](#list) | `Vec<DatabaseCatalogEntry>` | Returns a page of active database names and identities. |
| [`PortableBackup::list_backup_points`](#list_backup_points) | `Vec<BackupPoint>` | Lists the points available in a backup directory. |
| [Administration APIs](#system-administration) | Operation-specific result | Install storage and administer a database. |
| [`squuid`](#squuid) | `u128` | Constructs a semi-sequential UUID. |
| [`squuid_time_millis`](#squuid_time_millis) | `u64` | Gets the time component of a squuid. |
| [`EntityRef::Temp`](#tempids) | `EntityRef` | Names a temporary entity within a transaction. |
| [`tx_to_t`](#tx_to_t) | `u64` | Returns the logical transaction coordinate associated with a transaction entity ID. |
| [`t_to_tx`](#t_to_tx) | `u64` | Returns the transaction entity ID associated with a logical coordinate. |
| [`partition_eid`](#partition_eid) | `u64` | Returns the partition entity ID of an entity. |
| [`Peer::query`, `DatabaseValue::query`](#query) | `QueryOutcome` | Executes a Datalog query. |
| [`DatabaseValue::query_sequence`](#query_sequence) | `QuerySequence` | Defers item transformations such as Pull until consumption. |
| [Function registration and programs](#functions) | Registry or program | Supplies application computation. |
| [Report tempid map](#resolving-tempids) | `Option<u64>` | Resolves a tempid to its assigned ID. |
| [Service shutdown](#shutdown) | `()` | Stops resources owned by the application. |
| [Cancellation controls](#cancellation) | Control-specific error | Cooperatively stops a read or rejects program execution. |

## Method details

### connect

```rust,ignore
// Peer
fn connect(
    connection: &str,
    database_id: impl Into<String>,
    cache_capacity: usize,
) -> Result<Peer, SemanticError>;
fn connect_configured(
    connection: &PostgresConnectionConfig,
    database_id: impl Into<String>,
    cache_capacity: usize,
) -> Result<Peer, SemanticError>;
```

Connects to the specified database. `connection` is a PostgreSQL connection
string; `database_id` supplies the public logical database name separately.
Storage and the logical database must already exist. Opening a reader does not
install storage, create the database, or acquire writer authority.

For example, these are the two separate arguments to a local connection:

```text
connection:  host=localhost port=5432 dbname=edb user=edb_peer
database_id: music
```

`PostgresConnectionConfig` supplies transport and I/O policy. See
[connections](../00_connections.md) for configuration. There are no DynamoDB,
Cassandra, Infinispan, embedded-transactor, or in-memory URI protocols, and no
map accepting Java data sources or connection factories. Database names are
nonempty strings without NUL characters; they are not encoded as URI path
components.

The public name resolves once to a stable database identity. Renaming a database
or reusing its old name cannot redirect an existing reader. The reader requires
no transaction service. `Peer::db()` captures the current immutable value without
storage I/O; cold reads against that value may perform I/O. Call `Peer::sync()`
to capture and adopt durable progress, or `sync_to(target_t, timeout)` to wait
through a transaction coordinate. A bare `Peer` does not start a background
observer. `Peer::snapshot()` returns a fixed `PeerSnapshot`, from which
`database_value()` retrieves the same immutable read basis.

For normal application use, `Connection::connect(connection, database_id,
cache_capacity)` returns a reader that observes progress in the background.
`Connection::connect_configured` takes an owned `PostgresConnectionConfig`.
Attach a `TransactionClient` using `Connection::attach_writer` to submit
transactions. See [Connection](02_connection.md).

Readers are shared and suitable for long-lived use; cloning a reader shares its
state. Calling `connect` repeatedly does not promise a global connection cache
or the same object. `Peer::connect_with_cache_limits` and
`connect_configured_with_cache_limits` accept separate cache entry and accounted
byte limits. The corresponding `connect_with_limits` and
`connect_configured_with_limits` also accept `RecentLimits` for recent data.
These allowances are not bounds on total process memory.

For in-process memory, use `Database::bootstrap()?.database_value()`. For a fixed,
read-only backup connection, use `BackupConnection::open(directory)` for the
latest point or `BackupConnection::open_point(directory, &point)` for an exact
point. Backup connections supply `db()` and `log()` without source PostgreSQL;
queries and Pull run on the returned database value. See
[backup reads](../../08_operations/05_backup_reads.md).

### create_if_absent

```rust,ignore
// DatabaseCatalog
fn create_if_absent(
    &mut self,
    name: &str,
    schema: Schema,
) -> Result<CreateDatabaseResult, SemanticError>;
```

Creates a database with the given name. Idempotent with respect to an existing
active name. Connect the catalog using `DatabaseCatalog::connect` or
`connect_configured` after storage installation.

**Parameters:** `name` is the database name; `schema` is the initial schema used
when creating it.

**Returns:** `created` is `true` if the database was created and `false` if it
already exists. `database` contains the catalog entry. An already-existing
database is not updated with the supplied schema. See
[database lifecycle](../../08_operations/02_database_lifecycle.md).

### rename

```rust,ignore
// DatabaseCatalog
fn rename(&mut self, name: &str, new_name: &str)
    -> Result<DatabaseCatalogEntry, SemanticError>;
```

Renames a database. `name` and `new_name` are logical database names, not URIs.
Returns the updated catalog entry. `rename_checked` additionally accepts an
expected lineage to guard the identity currently occupying the name. Existing
readers retain their resolved identity.

### retire

```rust,ignore
// DatabaseCatalog
fn retire(&mut self, name: &str)
    -> Result<Option<DatabaseCatalogEntry>, SemanticError>;
```

Retires the active database named by `name`. Returns its retired catalog entry,
or `None` if no active entry has that name. Retirement releases the public name;
it is distinct from physical deletion. `PostgresOperator` provides
`preview_retired_database_reclamation` and `reclaim_retired_database` for the
separate reclamation operation. See
[database lifecycle](../../08_operations/02_database_lifecycle.md).

### list

```rust,ignore
// DatabaseCatalog
fn list(&mut self, after_name: Option<&str>, limit: usize)
    -> Result<Vec<DatabaseCatalogEntry>, SemanticError>;
```

Returns a page of active database catalog entries in name order. Start with
`after_name: None`, then supply the last returned name to continue. `limit`
bounds the page size. Listing uses an already-connected catalog, with no
wildcard database URI. Retired entries have a separate `list_retired` API.

### list_backup_points

```rust,ignore
// PortableBackup
fn list_backup_points(directory: &std::path::Path)
    -> Result<Vec<BackupPoint>, SemanticError>;
fn list_backups(directory: &std::path::Path)
    -> Result<Vec<u64>, SemanticError>;
```

Lists all points available at the given backup directory. `list_backup_points`
sorts ascending by `(basis_t, log_generation)`. Each point contains:

| Field | Meaning |
| --- | --- |
| `basis_t` | Basis transaction coordinate of the backup. |
| `lineage_id` | Database lineage. |
| `log_generation` | Log generation of the point. |
| `manifest_hash` | Identity of its manifest. |
| `objects_written`, `objects_reused` | Object-write and reuse counts. |

Pass the directory and a point to `BackupConnection::open_point`; no connection
URI is generated. `list_backups` returns only the deduplicated basis coordinates,
also ascending. Use the full point when selecting a particular generation. See
[backup reads](../../08_operations/05_backup_reads.md).

### System administration

Administer a system through explicit APIs. `storage::PgBlockStore::install`
installs storage, `DatabaseCatalog` manages logical databases, and
`PostgresOperator` supplies inspection, consolidation, garbage collection,
fulltext rebuilds, and excision processing. Each operation returns its own result
or `SemanticError`; there is no generic `administer_system(options)` action map.
Application schema changes are transactions. See
[administration](../../08_operations/01_administration.md).

### squuid

```rust,ignore
fn squuid() -> Result<u128, SemanticError>;
fn squuid_at(unix_millis: u64) -> Result<u128, SemanticError>;
```

Constructs a semi-sequential UUID. Returns a UUID whose most significant 32 bits
are Unix epoch time in seconds; remaining non-version/non-variant bits are
random. Store the value as `Value::Uuid`.

The timestamp groups values by coarse time in index order. It does not establish
strict ordering within a second or after clock regression. `squuid_at` takes an
explicit clock in milliseconds. Out-of-range clocks and operating-system
randomness failures return errors. `uuid_v7` and `uuid_v7_at` provide UUIDs with
millisecond timestamps as a separate choice.

### squuid_time_millis

```rust,ignore
fn squuid_time_millis(uuid: u128) -> u64;
```

Gets the time component of a squuid, as Unix epoch milliseconds. Precision is one
second. The function extracts the leading bits; it cannot establish that an
arbitrary UUID was generated by `squuid`.

```rust
use edb_core::{SemanticError, squuid_at, squuid_time_millis};

fn example() -> Result<(), SemanticError> {
    let id = squuid_at(1_700_000_000_123)?;
    assert_eq!(squuid_time_millis(id), 1_700_000_000_000);
    Ok(())
}
```

### Tempids

Use `EntityRef::Temp(String)` in typed transaction forms, or string tempids in
EDN transaction data:

```edn
[{:db/id "person" :person/name "Ada"}]
```

A tempid names the same entity everywhere it appears within that transaction.
There is no global tempid constructor or reserved negative-number range for
user-created tempids in this API. Use distinct strings for distinct temporary
entities. Names may be reused independently in later transactions.

To designate an allocation partition, use `TransactionDefaults`,
`TxOp::ForcePartition`, or `TxOp::MatchPartition`. These directives affect new
allocation; they do not move an existing entity found by identity upsert. See
[transaction data](../../04_transactions/04_transaction_data.md) and
[partitions](../../04_transactions/02_partitions.md).

### tx_to_t

```rust,ignore
fn tx_to_t(tx: u64) -> Result<u64, SemanticError>;
```

Returns the logical transaction coordinate associated with `tx`, a transaction
entity ID. Rejects IDs outside the transaction partition and invalid permanent
ID representations.

### t_to_tx

```rust,ignore
fn t_to_tx(t: u64) -> Result<u64, SemanticError>;
```

Returns the transaction entity ID associated with the logical transaction
coordinate `t`. Rejects an out-of-range coordinate. This is a representation
conversion, not a check that the transaction exists in a particular database.

### partition_eid

```rust,ignore
fn partition_eid(entity: u64) -> Result<u64, SemanticError>;
fn eid_to_part(entity: u64) -> Result<u32, SemanticError>;
```

Returns the partition of this entity ID. `partition_eid` returns the partition
entity ID; `eid_to_part` returns raw partition bits. These representations differ
for implicit partitions. Both reject invalid permanent entity IDs. See
[partitions](../../04_transactions/02_partitions.md).

```rust
use edb_core::{SemanticError, TX_PARTITION, partition_eid, t_to_tx, tx_to_t};

fn example() -> Result<(), SemanticError> {
    let tx = t_to_tx(42)?;
    assert_eq!(tx_to_t(tx)?, 42);
    assert_eq!(partition_eid(tx)?, u64::from(TX_PARTITION));
    Ok(())
}
```

### query

```rust,ignore
// Peer, PeerSnapshot, DatabaseValue, and Database
fn query(
    &self,
    query: &Query,
    inputs: &[QueryInput],
    control: &QueryControl,
) -> Result<QueryOutcome, SemanticError>;
```

Executes a Datalog query. In addition to basic pattern matching, query supports:

- joins and rules;
- built-in predicates and functions, and registered application extensions;
- aggregates;
- binding options for inputs;
- find specifications for outputs;
- Pull patterns.

Query can be applied to multiple inputs, including both databases and plain data.
`DatabaseValue::query` supplies that value as source `$`; `Peer::query` captures
its current value for the call. Use `QueryEngine::execute_sources` with
`QueryDataSource` for named database, relation, and log inputs. Use
`query_with_extensions` or the corresponding engine entry point for registered
functions and predicates; arbitrary Rust symbols are not dynamically resolved.

Query runs locally in the calling process. Relational evaluation is set-based;
intermediate and final rows must fit in available memory and configured limits.
`QueryControl` supplies cooperative timeout, cancellation, work, row, allocation,
and optional diagnostic controls.

The native query is a typed `Query`. The EDN adapter also accepts:

- a map containing query sections such as `:find`, `:with`, `:in`, and `:where`;
- a vector or list representation of those sections;
- serialized EDN text of either form through `edn_query::parse_query_edn`.

Use `edn_query::query_from_edn` for an already-parsed `EdnValue`. Bind the ordered
inputs through `EdnQuery::bind`. Its arguments can supply database or log
sources, data values and relations, rules, and Pull patterns. If no `:in` section
is supplied, the EDN template expects the default database source as its single
argument. See [query grammar](../../05_query_and_pull/05_query_reference.md).

**Returns:** `QueryOutcome`, whose `result` is a `QueryResult` determined by the
find specification:

| Find shape | Result variant |
| --- | --- |
| Relation | `Relation(Vec<Vec<QueryValue>>)` |
| Collection | `Collection(Vec<QueryValue>)` |
| Scalar | `Scalar(Option<QueryValue>)` |
| Tuple | `Tuple(Option<Vec<QueryValue>>)` |

There is no separate relation-only `q` entry point. Match `QueryResult::Relation`
when that is the expected shape. The outcome also contains execution statistics,
plan information, and optional diagnostics.

There is no `query(QueryRequest)` overload. A `BoundEdnQuery` owns its prepared
query and bound arguments; its `execute(&control, extensions)` executes them.
`PreparedQuery` provides explicit reusable native preparation, and `AsyncClient`
provides future-returning execution. See
[executing queries](../../05_query_and_pull/07_executing_queries.md) and
[query requests and controls](10_query_request.md).

This complete in-memory example installs schema, transacts information, resolves
a tempid, and executes a relation query:

```rust
use edb_core::{Database, QueryControl, QueryResult, QuerySourceValue, SemanticError};
use edb_core::edn_query::{EdnQueryArgument, parse_query_edn};

fn example() -> Result<(), SemanticError> {
    let initial = Database::bootstrap()?.database_value();
    let schema = initial.with_edn(r#"[
      {:db/ident :person/name
       :db/valueType :db.type/string
       :db/cardinality :db.cardinality/one}
    ]"#, 1_000)?;
    let report = schema.db_after.with_edn(r#"[
      {:db/id "person" :person/name "Ada"}
    ]"#, 2_000)?;
    assert!(report.tempids.get("person").is_some());
    let query = parse_query_edn("[:find ?name :where [?e :person/name ?name]]")?;
    let bound = query.bind(&[EdnQueryArgument::Source(
        QuerySourceValue::Database(report.db_after),
    )])?;
    let outcome = bound.execute(&QueryControl::default(), None)?;
    let QueryResult::Relation(rows) = outcome.result else {
        panic!("expected relation find");
    };
    assert_eq!(rows.len(), 1);
    Ok(())
}
```

### query_sequence

```rust,ignore
// DatabaseValue and Database
fn query_sequence(
    &self,
    query: &Query,
    inputs: &[QueryInput],
    control: &QueryControl,
) -> Result<QuerySequence, SemanticError>;
```

Performs the query described by `query` and `inputs`, with item transformations
such as Pull deferred until the sequence is consumed. `Peer` callers first
capture `peer.db()`. Bound EDN queries expose `sequence(&control, extensions)`.

For queries with Pull, this avoids eagerly allocating all projected results and
allows processing a projected row before projecting the rest. Relational joins,
aggregates, and their rows are still eager. This is not a fully streaming query
evaluator or a promise that arbitrarily large result sets fit in memory.

**Returns:** a single-pass iterator with
`Item = Result<Vec<QueryValue>, SemanticError>`. `remaining_rows()` counts pending
relational rows without performing Pull. The original deadline and shared
budgets continue through projection. Each item can fail; after the first error,
the iterator is exhausted. The sequence owns its immutable sources, so it does
not track later changes to a live peer.

### Functions

Native Rust functions are compiled with the application and registered through
`TxFunctions`, `NativeRegistryBuilder`, or `QueryExtensions`, as appropriate to
their role. Persistable functions are typed `Program` values with a kind, arity,
and instructions. Validated programs are installed as content-addressed data and
bound through `:db/fn`.

There is no `function` constructor that compiles a map of `:lang`, `:params`,
`:code`, imports, or requires into Java or Clojure code. Native closures are not
serialized into the database. See [native computation](../02_native_computation.md)
and [transaction functions](../../04_transactions/07_transaction_functions.md)
for registration, installation, and invocation.

### Resolving tempids

Resolve a tempid to the actual ID assigned by the transaction using the report's
`tempids: BTreeMap<String, u64>`:

```rust,ignore
let entity_id: Option<u64> = report.tempids.get("person").copied();
```

Use the map from the speculative or committed transaction that allocated or
upserted that tempid. The lookup returns `None` if the name is absent. No database
argument or separate `resolve_tempid` function is required. A speculative mapping
does not reserve IDs for a later durable transaction.

### Shutdown

There is no process-global peer shutdown function or Clojure runtime to shut
down. Drop `Peer` and `Connection` handles when they are no longer needed;
dropping one clone does not release all other owners. Captured database values
have independent immutable ownership.

Stop services the application starts with the consuming
`TransactionService::shutdown(self)` and `TransactionStandby::shutdown(self)`
methods. Before process exit, wait for transaction outcomes the application
needs to know. Admitted async transactions remain owned by workers if their
waiters are dropped; dropping a waiter is not transaction cancellation. See
[async ownership](../01_async_client.md).

Holding a database value does not register a durable storage pin. Retention must
cover the lifetime of cold reads against its referenced objects. See
[retention and reclamation](../../08_operations/00_deployment.md#inspection-retention-and-reclamation).

### Cancellation

Query cancellation uses `QueryControl::cancel`, an `Arc<AtomicBool>` shared with
the query invocation:

```rust
use edb_core::QueryControl;
use std::sync::atomic::Ordering;

let control = QueryControl::default();
let cancellation = control.cancel.clone();
cancellation.store(true, Ordering::Relaxed);
```

The evaluator checks cancellation cooperatively and returns a `SemanticError`
with code `query/canceled`. It does not throw a process-global exception.
Pull has `PullControl`; programs have `ProgramControl`. A native transaction
callback rejects execution by returning an error.

For structured program rejection, `Instruction::RequireAnomaly` rejects a false
condition using a runtime anomaly map. Its required
`:cognitect.anomalies/category` must be `:cognitect.anomalies/incorrect` or
`:cognitect.anomalies/conflict`. Optional `:cognitect.anomalies/message` must be a
string and becomes the error message. All keys must be namespace-qualified
keywords. The error has code `program/rejected`, preserves structured data in
`SemanticError::anomaly`, and adds `:edb/cancelled true`. Values use EDB's typed
runtime representation; there is no Fressian serialization requirement.

Timing out a wait or dropping a transaction ticket/future does not undo an
admitted transaction. See [async admission and cancellation](../01_async_client.md)
for the distinction between read cancellation and transaction outcome handling.
