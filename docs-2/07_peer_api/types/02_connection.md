# Connection

```rust,ignore
pub struct Connection { /* private fields */ }
```

A connection to a database for submitting and monitoring transactions, and
retrieving the current value of the database. `edb_core::Connection` is cloneable,
thread-safe, and intended for long-lived use. Clones share one advancing reader,
optional transaction endpoint, and transaction report queue.

`Connection::connect` opens a reader for an existing PostgreSQL database.
`attach_writer` attaches a transaction client to that reader; `attach` opens a
reader with an existing client; `start` also owns an embedded transaction service.
A read connection observes durable progress independently of a writer. See
[connections](../00_connections.md) and the
[native API reference](../04_native_api_reference.md#connectionconnect) for
construction, transport configuration, cache limits, and stable database identity.

## Report fields

Connection fields are private. Transaction results use the public fields of
`ServiceTransactionReport`, rather than keys in a result map:

| Field | Type | Description |
| --- | --- | --- |
| `db_before` | `DatabaseValue` | Exact immutable database value assessed before the transaction. |
| `db_after` | `DatabaseValue` | Exact immutable successor installed after durable publication. |
| `tx_data` | `Vec<Datom>` | Datoms produced by the transaction, including assertions and retractions. |
| `tempids` | `BTreeMap<String, u64>` | Temporary ID to permanent entity ID mapping. |
| `basis_t` | `u64` | Logical transaction coordinate of the committed result. |
| `tx_hash` | `Digest` | Committed transaction hash. |
| `replayed` | `bool` | Whether the result was recovered from an existing request receipt. |
| `diagnostics` | `Option<Arc<TransactionDiagnostics>>` | Optional diagnostic information; not encoded in durable receipts or reconstructed remote reports. |

Resolve a temporary ID directly with `report.tempids.get("temporary-id")`.
The report's database values describe the committed transition even if the
connection has subsequently advanced or has not yet observed that transition.
See [transaction processing](../../04_transactions/06_processing_transactions.md).

## Method summary

All methods below take `&self`; parameters and result types are abbreviated in
this table and given in full in the details. Fallible calls return
`Result<_, SemanticError>`.

| Method | Successful result | Description |
| --- | --- | --- |
| [`db`](#db) | `DatabaseValue` | Retrieves the current database value. |
| [`log`](#log) | `LogValue` | Retrieves the current value of the log. |
| [`request_index`](#request_index) | `IndexRequest` | Requests background indexing at a finite committed target. |
| [`sync`](#sync) | `DatabaseValue` | Captures and adopts the newest durable head visible to the call. |
| [`sync_to`](#sync_to) | `DatabaseValue` | Waits for local observation through a supplied logical transaction coordinate. |
| [`sync_index`](#sync_index) | `DatabaseValue` | Waits for indexing through a supplied coordinate. |
| [`sync_schema`](#sync_schema) | `DatabaseValue` | Waits for schema-index readiness through a supplied coordinate. |
| [`sync_excise`](#sync_excise) | `DatabaseValue` | Waits for committed excision completion through a supplied coordinate. |
| [`transact`](#transact) | `ServiceTransactionReport` | Submits a transaction and waits for its result. |
| [`submit`](#submit) | `ConnectionTransactionTicket` | Submits a transaction without waiting for its result. |
| [`enable_transaction_reports`](#transaction-report-queue) | `bool` | Creates the single report queue if necessary. |
| [`try_next_transaction_report`](#transaction-report-queue) | `Option<ServiceTransactionReport>` | Takes the next queued report without waiting. |
| [`next_transaction_report`](#transaction-report-queue) | `Option<ServiceTransactionReport>` | Waits for and takes the next report. |
| [`remove_transaction_report_queue`](#remove_transaction_report_queue) | `bool` | Removes the queue and releases unconsumed reports. |
| [`observation_error`](#observation_error) | `Option<SemanticError>` | Retrieves the most recent background or adoption failure. |

Storage reclamation uses [`PostgresOperator::collect_garbage`](#storage-garbage-collection).
Releasing a connection uses [Rust ownership](#releasing-a-connection).

## Method details

### request_index

```rust,ignore
fn request_index(&self) -> Result<IndexRequest, SemanticError>;
```

Requests that a [background indexing job](../../06_indexes/02_background_indexing.md)
begin at a finite committed target. Background indexing happens asynchronously.
Track completion with `sync_index(receipt.target_t, timeout)`.

The returned `IndexRequest` has two public fields:

| Field | Type | Description |
| --- | --- | --- |
| `target_t` | `u64` | Committed frontier captured by this request. Later transactions do not extend this target. |
| `scheduled` | `bool` | `true` if this call added indexing demand; `false` if existing demand covers the target or it is already published. |

Neither value of `scheduled` is a completion receipt. Dropping the receipt or
timing out a wait does not cancel shared background work. A connection without
an attached in-process writer returns `connection/no-writer`; the local
transaction socket does not carry maintenance requests.

### db

```rust,ignore
fn db(&self) -> DatabaseValue;
```

Retrieves the current database value. Capturing it performs no PostgreSQL I/O
and does not communicate with the transaction service. Returns an immutable
value; advancing the connection never changes an earlier value. Subsequent cold
reads through that value can require storage I/O. See
[database values](../../02_core_concepts/00_database_values.md).

### log

```rust,ignore
fn log(&self) -> LogValue;
```

Retrieves the current value of the log. Captures an immutable log at the
connection's current endpoint without contacting the writer. Reading its
transactions can require storage I/O. See
[LogValue](08_log.md).

### sync

```rust,ignore
fn sync(&self) -> Result<DatabaseValue, SemanticError>;
```

Captures and adopts the newest durable head visible to the call's storage read.
`sync` is a primitive for coordinating activity across peer processes. It
communicates with PostgreSQL and should only be used for coordination when peers
have no way to agree on a basis `t`.

If you do not require coordination, prefer `db()`. If peers can share a known
basis `t`, prefer `sync_to(t, timeout)`. This synchronous method has no timeout
argument; blocking storage calls are subject to configured I/O policies.
`AsyncClient::sync` runs the operation on a worker and accepts an optional
operation deadline. See [synchronization](../../04_transactions/09_synchronization.md)
and [async clients](../01_async_client.md).

### sync_to

```rust,ignore
fn sync_to(
    &self,
    target_t: u64,
    timeout: Duration,
) -> Result<DatabaseValue, SemanticError>;
```

Retrieves a database value that includes all transactions up to and including
`target_t`. This is a minimum: the returned value can include later transactions.
`target_t` is a logical database coordinate, not a transaction entity ID or a
wall-clock instant.

`sync_to` coordinates activity across peer processes. The waiting call does not
communicate with the transactor or perform storage reads, but it can block if the
connection has not yet observed transactions through the target. The background
observer performs storage I/O to advance the connection. A stalled observer read
cannot extend this local wait beyond its timeout.

If you do not require coordination, prefer `db()`. If peers do not share a basis
`t`, use `sync()`. A target that has not committed may never be reached; supply a
finite timeout. Returns the database value on success or a `SemanticError` if the
wait fails. Previously captured values remain unchanged.

### sync_index

```rust,ignore
fn sync_index(
    &self,
    target_t: u64,
    timeout: Duration,
) -> Result<DatabaseValue, SemanticError>;
```

Retrieves a database value indexed through the supplied database coordinate.
Completion requires the logical basis and the physical indexed-through frontier
to reach the target, with relevant AVET additions and removals through that
target complete. Logical basis advancement alone is insufficient.

This call observes progress through storage and does not schedule indexing.
Use `request_index` to request work, then wait on the returned finite target.
Completion need not wait for schema transitions strictly after that target.
The call can complete on its first check if the condition is satisfied.
Timeout checks occur between storage attempts; blocking I/O remains subject to
configured I/O policies. Timing out does not cancel indexing.

### sync_schema

```rust,ignore
fn sync_schema(
    &self,
    target_t: u64,
    timeout: Duration,
) -> Result<DatabaseValue, SemanticError>;
```

Retrieves a database value aware of schema changes through the supplied database
coordinate and ready for the AVET additions required through that target.
The returned value has reached the logical basis and has no pending relevant
AVET addition at or before the target. This includes readiness associated with
indexing or uniqueness changes.

It need not wait for later schema transitions, AVET removals, or the general
indexed-through frontier required by `sync_index`. This is a wait, not a request
to perform a migration. It performs storage observation and can complete on its
first check. Timeout checks occur between storage attempts. See
[changing schema](../../03_schema/03_changing_schema.md).

### sync_excise

```rust,ignore
fn sync_excise(
    &self,
    target_t: u64,
    timeout: Duration,
) -> Result<DatabaseValue, SemanticError>;
```

Retrieves a database value aware of committed excision processing through the
supplied database coordinate. Completion requires the logical basis to reach
the target and the completion metadata to cover excision requests asserted at
or before it. This does not itself request excision work or garbage collection.

It performs storage observation and can complete on its first check. Use a finite
timeout; timeout checks occur between storage attempts and do not interrupt a
blocking read. See [maintenance controls](../../08_operations/04_maintenance_controls.md).

### transact

```rust,ignore
fn transact(
    &self,
    request: TransactionRequest,
    timeout: Duration,
) -> Result<ServiceTransactionReport, SemanticError>;
```

Submits a [transaction](../../04_transactions/00_transactions.md), blocking until
a result or wait error is available. Requires an attached transaction endpoint.
Construct a `TransactionRequest` with `new(request_key, operations)`,
`from_forms(request_key, forms)`, or `from_edn(request_key, text)`.
The request key identifies the submission for exact retries.

Transaction data can contain any combination of assertions, retractions,
transaction functions, or entity maps. These EDN examples assume the named
attributes and transaction function have been installed:

| Type | Example |
| --- | --- |
| Assertion | `[:db/add 1001 :some-attr/name "Some attr value"]` |
| Retraction | `[:db/retract 1001 :some-attr/name "Some attr value"]` |
| Transaction function | `[:some/fn-name "argument"]` |
| Entity map | `{:db/id "new-entity" :some-attr/name "some value" :another-attr/name 42}` |

Wrap forms in an outer vector for `from_edn`. See
[transaction data](../../04_transactions/04_transaction_data.md) and
[transaction functions](../../04_transactions/07_transaction_functions.md).

If the transaction commits, the result is a `ServiceTransactionReport` with the
[fields above](#report-fields). The connection attempts ordered local observation
within the time remaining after receiving that report. If local observation
fails or times out after known durable success, the method **returns the
successful report** and records the failure in `observation_error()`.
Use `report.db_after` for reads at the exact committed result; a subsequent
`connection.db()` is not an unconditional read-after-write guarantee.

If execution rejects the transaction, the result is a `SemanticError`. A ticket
wait timeout or worker disconnection after admission returns category
`UnknownOutcome`, code `service/unknown-outcome`. This does not establish that
the transaction aborted. Retain the same request key and transaction data when
retrying an unknown outcome. See
[transaction diagnostics](../../04_transactions/03_diagnostics.md).

The timeout is explicit; there is no global default transaction timeout. It
covers the ticket wait and remaining local-observation wait, beginning after
submission. Request validation and encoding happen before that wait begins.

### submit

```rust,ignore
fn submit(
    &self,
    request: TransactionRequest,
) -> Result<ConnectionTransactionTicket, SemanticError>;

// ConnectionTransactionTicket
fn wait(self, timeout: Duration) -> Result<ServiceTransactionReport, SemanticError>;
fn request_key(&self) -> &str;
```

Like `transact`, but returns a ticket after admission without waiting for the
transaction result. `wait` consumes the ticket and follows the same durable
success and local-observation rules as `transact`. `request_key` retrieves the
submission's key.

Admission validates the key and canonically encodes the request before attempting
to enqueue it. The service queue is bounded: a full queue returns category `Busy`,
code `service/queue-full`, without waiting for capacity. A successful admission
is not proof of a durable commit. Dropping a ticket or timing out its wait does
not cancel the admitted transaction; the observer can advance independently of
ticket waits.

For a Rust async executor, `AsyncClient::transact` returns an `AsyncTransaction`
that can be awaited or consumed with `on_complete`. It also reserves a bounded
worker slot; `async/operation-capacity` means the call did not submit the request.
See [AsyncOperation and AsyncTransaction](07_async_operation.md) and
[async admission and ownership](../01_async_client.md). Advisory prefetch
uses `submit_with_hints`; see [transaction hints](../../09_optional/01_transaction_hints.md).

### Transaction report queue

```rust,ignore
fn enable_transaction_reports(&self) -> bool;
fn try_next_transaction_report(&self) -> Option<ServiceTransactionReport>;
fn next_transaction_report(
    &self,
    timeout: Duration,
) -> Result<Option<ServiceTransactionReport>, SemanticError>;
```

Enables the single transaction report queue associated with this shared
connection, creating it if necessary. Returns `true` if a queue was created and
`false` if it was already enabled. At any point, zero or one queue is associated
with the connection. Reports begin with the next adopted transaction; enabling
the queue does not manufacture reports for already-observed transactions.

The queue receives transactions observed for this database, including those
submitted by other connections. Reports have the same fields as transaction
results, with exact `db_before`, `db_after`, and `tx_data`. Reports are added
after the connection's database value has been updated. Do not depend on relative
delivery ordering between a ticket waiter and the report queue.

The queue may be consumed from more than one thread through connection clones;
consumers compete for reports. It is unbounded, does not apply commit
backpressure, and consumes memory, including retained database values, until
reports are consumed or the queue is removed.

`try_next_transaction_report` takes the next report or returns `None` if the queue
is empty or disabled. `next_transaction_report` waits up to the supplied timeout,
returning `Ok(Some(report))` when a report is available, or `Ok(None)` on timeout
or when the queue is disabled. Neither method enables the queue implicitly.

The queue is an in-memory observation facility, not a durable subscription.
Use [change consumers](../03_change_consumers.md) for resumable log delivery and
explicit durable acknowledgments.

### remove_transaction_report_queue

```rust,ignore
fn remove_transaction_report_queue(&self) -> bool;
```

Removes the queue associated with this connection. Returns `true` if a queue was
removed and `false` if none existed. Removal releases all unconsumed reports,
including their immutable database values, and wakes queue waiters. Calling
`enable_transaction_reports` again starts a new queue for future adoption.

### observation_error

```rust,ignore
fn observation_error(&self) -> Option<SemanticError>;
```

Retrieves the most recent background or adoption failure, separately from the
outcome of any transaction. A subsequent successful catch-up clears it. Captured
values remain immutable while the connection cannot advance. Background commit
notifications are wakeup hints; the observer repairs gaps by reading durable
state after reconnect and periodically. See
[observability](../../08_operations/03_observability.md).

### Storage garbage collection

```rust,ignore
// PostgresOperator
fn collect_garbage(
    &mut self,
    older_than: Duration,
) -> Result<GarbageInventory, SemanticError>;
```

Reclaims storage garbage older than a certain age. Storage maintenance belongs
to `PostgresOperator`, not `Connection`; `older_than` is an age duration rather
than a date. As part of capacity planning, schedule collection according to your
retention and maintenance policy.

Collection is bounded and resumable. A completed cycle does not imply global
quiescence or revoke still-owned values. `garbage_inventory(older_than)` inspects
collector state without starting a cycle. See
[maintenance controls](../../08_operations/04_maintenance_controls.md).

### Releasing a connection

Drop connection handles when they are no longer needed. There is no explicit
`Connection::release` method. Connections are thread-safe and long lived; they
do not require an acquire/use/release pattern for each read or transaction.
Separate `connect` calls do not promise a process-wide cached connection.

Clones share ownership, so dropping one clone does not release other owners.
The final connection owner requests observer shutdown; an observer already
inside a storage read can finish that read before exiting. An embedded service
started with `Connection::start` is retained by the connection's shared ownership.
Other owned resources can require synchronous cleanup, so dropping the last
native owner is not a universal immediate-return guarantee. See
[async ownership](../01_async_client.md).

Captured database values have independent ownership. Holding a value does not
register a storage retention pin; cold reads still require referenced durable
objects to exist. Configure retention for active reads. See
[deployment and retention](../../08_operations/00_deployment.md#inspection-retention-and-reclamation).
