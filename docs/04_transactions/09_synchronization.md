# Synchronization

## Background

EDB stores data immutably. When you ask a connection for a db value, you are given its most recently adopted value of the database. Callers see a valid, [consistent](08_acid.md) view of committed transactions. A captured value does not expose a partial transaction, and later transactions cannot change it. Different connections can have adopted different prefixes of the transaction history.

`Connection::db()` captures that resident value without PostgreSQL I/O or coordination with the transactor. It does not establish that the connection has observed the latest durable transaction. Reading data through the captured value can still require storage I/O. See [Connections and captured reads](../07_peer_api/00_connections.md).

## Motivation

That does not mean that every process sees the same thing simultaneously. In a live distributed system, propagation and observation take time. You might convey a message to me about X at the speed of light, but I can only perceive X at the speed of sound. Thus, I know X is coming, but I might have to wait for it.

This means that some caller A might commit a transaction and tell caller B about it before B is informed via the normal channels. This is an interesting case, as it has to do with perception and propagation delays. It is not a question of the consistency of either captured value; it is a question of communication synchronization.

This comes up when you would like to read your own writes across connections, for example when requests reach different application processes through a load balancer, and when there is out-of-band communication of writes: A tells B about its write before EDB's propagation does.

Even on the submitting connection, a successful transaction result and local observation are separate events. A connection ticket wait attempts to wait for observation within its remaining timeout. If a successful report arrives but local observation times out, the successful report is returned and `Connection::observation_error()` records the failure. A subsequent `db()` is therefore not an unconditional read-after-write guarantee. Use the report's exact `db_after`, or successfully synchronize to `report.basis_t` before capturing a new value. See [ACID guarantees](08_acid.md).

## Sync

`Connection::sync_to(target_t, timeout)` takes a basis point `t` and returns a database value that includes point `t`, or an error if the connection cannot observe that basis within the timeout. It saves you from having to poll for arrival. Use it to synchronize between callers:

- Caller A gets the basis `t` for some transaction of interest.
- Caller A records the `t` value in its communication to B, for example via a cookie or a response field.
- Caller B calls `sync_to(t, timeout)` on its connection to the same database lineage and uses the returned value.

You can get the basis `t` for any database value you have in hand using `DatabaseValue::basis_t()`. A transaction report supplies `report.basis_t`. Logical transaction time is distinct from the transaction entity id and wall-clock `:db/txInstant`; pass the logical basis rather than substituting either of those values.

```rust
use edb_core::{Connection, DatabaseValue, SemanticError};
use std::time::Duration;

// Caller A sends report.basis_t to caller B with the application response.
// Caller B reads a value that includes that transaction:
fn read_after(
    connection: &Connection,
    received_t: u64,
) -> Result<DatabaseValue, SemanticError> {
    let db = connection.sync_to(received_t, Duration::from_secs(5))?;
    assert!(db.basis_t() >= received_t);
    Ok(db)
}
```

EDB distinguishes waiting for a known basis from capturing a fresh durable head:

| API | Behavior |
| --- | --- |
| `Connection::sync_to(t, timeout)` | Waits on local adoption. The waiting call does not contact a separate transactor endpoint or issue storage reads; the background observer performs storage I/O to advance the connection. A stalled observer read cannot extend this local wait. |
| `Connection::sync()` | Captures and adopts the newest durable head visible to the call, performing storage I/O. It takes no target basis. |
| `Peer::sync_to(t, timeout)` | Actively calls storage synchronization until the target is reached or the wait fails. Its loop checks the timeout between synchronization attempts; blocking storage reads remain subject to native I/O policies. |
| `Peer::sync()` | Captures and adopts a durable head through storage synchronization. |
| `AsyncClient::sync_to(t, timeout)` | Runs the connection wait on a worker, passing the time remaining after queueing. Returns an `AsyncOperation<DatabaseValue>` to await. |
| `AsyncClient::sync(Some(timeout))` | Captures a fresh durable head on a worker. Its deadline also rejects expired queued work; blocking storage I/O remains subject to native I/O policies. `None` omits the operation deadline. |

Thus, synchronization is not a general promise of no additional network traffic. Connection target waits use the existing observation mechanism, whereas fresh-head synchronization and lower-level peer target waits read storage. See the [async client](../07_peer_api/01_async_client.md) for operation admission and deadlines.

The target is a minimum: a successful result may include transactions later than `t`. Synchronizing does not alter any database value you captured earlier, and logical synchronization does not wait for index, schema-index, or excision completion. Those conditions have their own [`sync_index`](../07_peer_api/04_native_api_reference.md#connectionsync_index), [`sync_schema`](../07_peer_api/04_native_api_reference.md#connectionsync_schema), and [`sync_excise`](../07_peer_api/04_native_api_reference.md#connectionsync_excise) APIs.

## Comparison to As-Of

`sync_to` ensures, by maybe waiting, that you have up to some time point `t`. `as_of` ensures, by filtering, that the view exposes transaction data no later than some time point `t`. In combination, they let separate processes align the transaction boundary of their reads:

```rust
let db = connection.sync_to(t, Duration::from_secs(5))?.as_of(t);
```

An `as_of(t)` call alone cannot make an older value catch up. Conversely, `sync_to(t, timeout)` alone can return a value whose basis is later than `t`.

An as-of view retains the basis, schema, and ident dictionary of its underlying immutable value. `basis_t()` still reports that underlying basis; `as_of_t()` reports the view's upper boundary. Two synchronized as-of views can therefore share a transaction-data boundary without having identical underlying bases or schemas. When you need the same exact retained value, use a `SnapshotReference` and `Connection::reopen_snapshot`; reopening can fail if that value is no longer supported or retained. See [Composing immutable reads](../02_core_concepts/00_database_values.md).

## Conclusion

Synchronization removes the need to find a particular application process to handle a causal handoff between callers that can read the same database lineage. That said, use `sync_to` only when necessary. Immutable values allow independent reads without coordinating every request with the writer. Use synchronization when an application needs to enforce a cross-caller causal relationship, and continue using the returned immutable value for the reads that depend on it.
