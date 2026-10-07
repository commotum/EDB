# Processing Transactions

After a transaction data structure is built, you must submit it to the transactor for processing. The transaction service queues transactions and processes fresh transactions serially against one immutable db-before. See [Transaction Data](04_transaction_data.md) for the input forms and [Transaction Model](05_transaction_model.md) for their semantics.

## Submitting Transactions

Once a transaction data structure is built, you submit it through a connection with an attached transaction endpoint. `Connection::transact(request, timeout)` blocks until a transaction result is returned or the wait fails. `Connection::submit(request)` returns a `ConnectionTransactionTicket`; its `wait(timeout)` method retrieves the eventual result. The underlying `TransactionClient` also provides `submit` and `transact`.

A `TransactionRequest` pairs the transaction data with a caller-owned request key. Use `TransactionRequest::new` for primitive `TxOp` operations or `TransactionRequest::from_forms` for `TxForm` data, including parsed EDN. Keep the complete request until its outcome is known. The [EDN workflow](../01_tutorials/00_edn_workflow.md) and [application workflow](../01_tutorials/01_application_workflow.md) show how to connect and submit transactions.

Each assertion and retraction carries an explicitly supplied source entity id in its transaction form. One request can contain contributions from several sources. The generated `:db/txInstant` also needs an explicit source declaration; see [Transaction Data](04_transaction_data.md).

The writer resolves any existing receipt for the request key before assessing fresh intent. For a fresh transaction, it checks writer authority, any requested basis guard, and capacity limits. Maps and transaction functions expand against db-before; functions cannot observe another form's proposed changes. The shared assessor resolves identities and tempids, derives assertions and retractions, eliminates redundant facts, and validates schema and transaction constraints. Redundant assertion checks include source: matching entity, attribute, and value from another source is a distinct contribution. Attribute predicates check asserted values; entity predicates requested through `:db/ensure` check the complete proposed db-after.

After validation, EDB stores the transaction log entry, successor metadata, and exact receipt, then atomically publishes the new database root under the writer's authority. The root makes the transaction and its receipt visible together. The resulting database value combines the existing index roots with recent committed transaction data; background index publication can follow the commit. A successful transaction report does not mean that background indexing has caught up. Use `Connection::sync_index` when that separate frontier matters.

The service has a bounded submission queue. A `service/queue-full` error means that submission was not admitted; receiving a ticket means it was admitted, not that it has committed. Transaction callbacks must be pure computations over their arguments and supplied database values. Perform external effects through a workflow reacting to committed transactions.

## Computing the Proposed Database

Speculative `DatabaseValue::with`, `with_forms`, and `with_edn` calls and durable submission share transaction assessment. The assessor gathers the full delta, checks within-transaction conflicts and successor constraints, and creates an immutable proposed db-after. Speculation returns the before/after values, material datoms and tempids without durable publication. The durable caller separately publishes the validated result. See [Transaction Model](05_transaction_model.md#with-and-transact).

Functions see db-before; entity predicates see complete db-after. Returned forms reenter expansion with that same db-before, so even a nested function cannot read its parent's generated facts. Entity specs and predicate bindings are read from db-before, while entity predicates run against the complete proposed successor. Only literal `true` accepts a predicate result. Both [persisted programs](01_persisted_programs.md) and explicitly deployed [native callbacks](../07_peer_api/02_native_computation.md) use this validation path. Native callbacks are trusted cooperative Rust code: their call context can enforce reported work and allocation, but cannot preempt noncooperative host code.

Identity and clock selection belong to shared assessment. Identity assertions unify tempids before their references are resolved; a tempid used only as a reference value or source is rejected. Durable clock checks occur after expansion, so a function-generated `:db/txInstant` participates in the same checks as a directly submitted value without running functions twice. An explicit instant must be no earlier than db-before's last transaction instant and no later than the transactor's clock. The default time cannot move backward. Pure speculation instead takes an explicit clock value from the caller. See [Tempids](04_transaction_data.md#tempids) and [Explicit :db/txInstant](04_transaction_data.md#explicit-dbtxinstant).

## Transaction Timeouts

When an admitted transaction times out, the application does not know whether the transaction succeeded. `TransactionTicket::wait` reports this as `ErrorCategory::UnknownOutcome` with code `service/unknown-outcome`. A timeout does not cancel the transaction or prove rollback.

Retry the identical request under the same request key to resolve its outcome. A matching durable receipt returns the original report with `replayed: true`, without applying the transaction again. Reusing the key for different intent conflicts. Preserve the original forms, including their source fields and declarations, and explicit basis/time options as well as the key. Querying a recent database value can help discover what happened, but generating a new request key is not an exact retry. See [Transactions, Reports and Exact Retries](00_transactions.md), including receipt retention and report-opening errors after confirmed commits.

Waiting through `Connection` also attempts to observe the committed basis locally. If that observation times out after a successful result arrives, the connection still returns the successful report and records an `observation_error`. Use the report's exact `db_after` when you need the result of that transaction.

## Monitoring Transactions

Connections can monitor committed transactions, including those submitted by other connections. `Connection::enable_transaction_reports` enables the connection's transaction report queue. Reports begin with the next transaction adopted after the queue is enabled; it does not manufacture reports for the prefix already observed. Peer catch-up reads the authenticated log and reconstructs reports for intervening transactions.

Reports are `ServiceTransactionReport` values with the following fields:

| Field | Usage |
| --- | --- |
| `db_before` | Exact database value before the transaction |
| `db_after` | Exact database value after the transaction |
| `tx_data` | Datoms produced by the transaction, including assertions and retractions with their source entity ids |
| `tempids` | Map from submitted tempid strings to resolved entity ids |
| `basis_t` | Logical transaction order |
| `tx_hash` | Identity of the stored transaction log entry |
| `replayed` | Whether this result was recovered from an existing request receipt |
| `diagnostics` | Optional operation diagnostics; not part of the durable receipt |

It is the responsibility of your application to empty the queue. When you are done monitoring it, remove it by calling `remove_transaction_report_queue`; otherwise it continues to accumulate reports and retain their immutable database values. The queue is unbounded. Use [change consumers](../07_peer_api/03_change_consumers.md) for bounded consumption with durable replay checkpoints.

This example shows how to enable the notification queue and retrieve a transaction report from it. It assumes an existing `Connection`; if no transaction is adopted during the wait, the result is `None`:

```rust
use edb_core::{Connection, SemanticError, ServiceTransactionReport};
use std::time::Duration;

fn next_report(conn: &Connection) -> Result<Option<ServiceTransactionReport>, SemanticError> {
    conn.enable_transaction_reports();
    let result = conn.next_transaction_report(Duration::from_secs(30));
    conn.remove_transaction_report_queue();
    result
}
```

The `tx_data` of a transaction report contains the set of datoms created by a transaction, both assertions and retractions, in `[e a v t s added]` form. A retraction carries the retracting source without changing the earlier assertion's attribution. It can be used as an input source for a query. The query below uses `tx_data` and the database value after the transaction to show changed values with their attribute names, omitting transaction and source ids:

```edn
[:find ?e ?aname ?v ?added
 :in $ $tx
 :where
 [$tx ?e ?a ?v _ _ ?added]
 [?a :db/ident ?aname]]
```

The query expects `db_after` as `$` and the report's datoms as `$tx`, in that order. In Rust, bind the former as `QuerySourceValue::Database(report.db_after.clone())` and obtain the latter from `QueryDataSource::datoms("$tx", report.tx_data.clone()).value`. Pass both as `EdnQueryArgument::Source` values to the parsed query's `bind` method.

The datom input source converts entity, attribute, transaction and source ids to numeric refs. The database source supplies the attribute-name lookup. Reading the changed facts from `$tx` preserves retractions that are absent from the current state in db-after. The name join returns rows whose attributes have an ident in db-after.

See [Querying Databases and Application Data](../05_query_and_pull/00_queries.md) for raw datom sources and the [Query Reference](../05_query_and_pull/05_query_reference.md) for input bindings and EDN query execution.
