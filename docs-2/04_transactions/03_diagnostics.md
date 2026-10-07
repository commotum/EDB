# Transaction diagnostics

## Background

Accruing a set of datoms to transact to a database value can require, for each
datom, resolving its entity ID, checking a unique value, and generating a
composite tuple. Transactions can also express sets of datoms via
[transaction function](07_transaction_functions.md) shorthands, which must be
expanded to primitive operations. Each redundant datom (about existing entities)
is eliminated, and novel datoms may generate a corresponding retraction.
Together, these tasks can take time because they need to read information from
the given database value.

## Problem

To design and maintain high performance systems, it is important to understand
the fine-grained costs of applying each transaction. [I/O stats](../07_peer_api/08_io_stats.md)
measure cache/index accesses and SQL calls for transaction processing, i.e. the
physical effects, but it is difficult to reason backwards from the effects to
their semantic cause.

## Approach: Transaction stats

EDB exposes opt-in transaction statistics describing the work performed to
accrue a set of datoms. The native `TransactionWorkStats` structure contains
counts of attempted semantic work; operation phase statistics supply processing
durations. A local service receipt returns these statistics through
`receipt.diagnostics`, and configured service telemetry can publish them as
transaction events.

Semantic transaction counters do not include metrics about making a transaction
durable, e.g. writing it to the log. The enclosing operation report also contains
SQL statistics and commit phase timing, which describe durable work separately.

## Requesting transaction stats

Use a diagnostic operation context when the cost of a transaction needs explaining:

```rust
use edb_core::{
    Keyword, OperationContext, OperationKind, SemanticError,
    ServiceTransactionReport, TransactionClient, TransactionRequest,
};
use std::time::Duration;

fn transact_observed(
    client: &TransactionClient,
    request: TransactionRequest,
) -> Result<ServiceTransactionReport, SemanticError> {
    let context = OperationContext::named(
        OperationKind::Transaction,
        Keyword::new("billing", "update-account"),
    )?;
    let receipt = {
        let _scope = context.enter();
        client.transact(request, Duration::from_secs(30))?
    };
    let diagnostics = receipt.diagnostics.as_ref().unwrap();
    assert_eq!(diagnostics.basis_t, receipt.basis_t);
    println!("{:?}", diagnostics.report.stats.transaction);
    Ok(receipt)
}
```

Submission carries the context across the service queue even if the caller's
scope ends before waiting on its ticket. `OperationContext::diagnostic` enables
the same counters without a business label. Existing `new` contexts retain SQL
statistics but leave these additional semantic counters disabled. Names are
intentional operational labels: never put customer identifiers, transaction
values, credentials, or request keys in them.

Pure `Database::with` and `DatabaseValue::with` expose their counters through the
enclosing context's `report()` or `measure()` result. For example, a transaction
with no domain changes still supplies a source for its automatic transaction
metadata:

```rust
use edb_core::{
    Database, EntityRef, Keyword, OperationContext, OperationKind, Schema,
    SemanticError, TxOp,
};

fn main() -> Result<(), SemanticError> {
    let db = Database::new(Schema::default())?;
    let source = db.entid(&Keyword::new("db.source", "system")).unwrap();
    let instant = 1_700_000_000_000;
    let operations: [TxOp; 0] = [];
    let context = OperationContext::diagnostic(OperationKind::Transaction);
    let (result, report) =
        context.measure(|| db.with(&operations, instant, EntityRef::Id(source)));
    let transaction = result?;
    assert_eq!(report.stats.transaction.assessments, 1);
    assert_eq!(report.stats.transaction.input_operations, 0);
    assert_eq!(
        report.stats.transaction.produced_datoms,
        transaction.tx_data.len() as u64,
    );
    println!("{:?}", report.stats.transaction);
    Ok(())
}
```

`measure()` returns the original operation result, including errors, together
with its report. If an operation fails, work attempted before the error remains
in the context. Create a fresh context when comparing separate operations:
reusing one accumulates observations.

## The transaction stats fields

The fields of `report.stats.transaction` have the following meanings. All are
`u64` counters. They count actual attempts, not distinct user forms; repeated
attempts and nested work accumulate.

| Field | Semantic | Unit |
| --- | --- | --- |
| `assessments` | Number of transaction assessment attempts. | attempts |
| `input_operations` | Number of primitive operations entering assessment, after map/function expansion. | operations |
| `identity_claims` | Number of unique-identity assertions on temporary entities considered for upsert. | assertions |
| `identity_lookups` | Number of exact unique-attribute lookups during transaction identity resolution. | lookups |
| `upsert_resolutions` | Number of existing-entity matches of identity assertions, including repeated matching claims. | matches |
| `uniqueness_checks` | Number of affected value groups checked for successor uniqueness, including schema changes. | value groups |
| `redundancy_checks` | Number of final logical datoms compared with db-before for material change. | datoms |
| `redundant_datoms` | Number of logical assertions/retractions eliminated as redundant with db-before. | datoms |
| `duplicate_datoms` | Number of identical logical datoms removed by transaction-local deduplication. | datoms |
| `composite_candidates` | Number of affected entity/composite pairs whose tuple value was computed. | pairs |
| `composite_datoms` | Number of composite assertions and retractions actually generated. | datoms |
| `function_calls` | Number of transaction-function expansion attempts, including nested or rejected calls. | calls |
| `produced_datoms` | Number of material datoms produced by assessment, including automatic transaction metadata. | datoms |

Assertion redundancy and datom deduplication include source: matching EAV from
a different source is retained. Retractions of a missing EAV remain redundant.

Memory, speculative and durable transactions use the same selective assessment
algorithm. `function_calls` does not count attribute/entity predicate invocations
or durable dependency loading. Production of datoms is not itself a durability
acknowledgement.

The expansion, assessment, encoding, commit, and report phase timings remain in
`report.stats.phases`. Each entry contains `invocations` and `elapsed_nanos`.
These are inclusive wall times, not a disjoint CPU-time decomposition. There are
no separate semantic prefetch/thread-duration fields for identity resolution,
composite generation, redundancy checks or uniqueness checks. Inspect the
semantic counts alongside phase timings and I/O statistics.

The receipt report's operation elapsed time includes queue residence but stops
before caller delivery/network time. See [relating I/O stats to transaction
time](../07_peer_api/08_io_stats.md#relating-io-stats-to-transaction-time) for the
differences between phase time, SQL time and caller-visible latency.

## Transaction events

Configured `ServiceOptions::telemetry` enables detailed per-submission statistics
and emits basis-correlated `edb.transaction` events through the bounded
nonblocking emitter. Publication is optional. The following is an illustrative
excerpt; other envelope and metric fields are omitted:

```json
{
  "event": "edb.transaction",
  "basis_t": 1008,
  "replayed": false,
  "operation": "Transaction",
  "context": ":billing/update-account",
  "operation_elapsed_nanos": 5000000,
  "metrics": {
    "transaction.assessments": 1,
    "transaction.input_operations": 0,
    "transaction.identity_claims": 0,
    "transaction.identity_lookups": 0,
    "transaction.upsert_resolutions": 0,
    "transaction.uniqueness_checks": 0,
    "transaction.redundancy_checks": 1,
    "transaction.redundant_datoms": 0,
    "transaction.duplicate_datoms": 0,
    "transaction.composite_candidates": 0,
    "transaction.composite_datoms": 0,
    "transaction.function_calls": 0,
    "transaction.produced_datoms": 1
  },
  "nested": [],
  "nested_truncated": false
}
```

JSON metrics use the `transaction.` prefix followed by the native field name.
`io_report_to_edn` exposes the same counters as keyword keys such as
`:transaction.produced_datoms` in the report's `:metrics` map. Consumers should
treat metric names as an open set and allow additional fields. See
[I/O stats](../07_peer_api/08_io_stats.md#the-io-stats-map) for the complete report
shape and [operational events](../08_operations/03_observability.md) for publisher
configuration.

Sink callbacks run on the telemetry worker, not under transaction or service
locks. Disabled, full, or stopped telemetry never changes a commit result.

## Receipt replay and publication

`TransactionDiagnostics` contains the outcome `basis_t`, `replayed` status, and
safe operation statistics in `report`. It contains no transaction forms, values,
tempid names, or request keys. It is ephemeral: diagnostics are not part of
durable receipt hashes/bytes or the native wire receipt format. Reconstructed
remote/historical reports therefore have `diagnostics: None`; they cannot
recreate past execution costs honestly.

An exact retry includes the same forms and explicit transaction source and resolves
its receipt before fresh source resolution, clock selection, or assessment of
current functions or schema. A separately submitted retry has
`replayed: true` and zero fresh semantic work, although reconstructing its values
can perform reads. Shared receipt clones retain one `Arc` to their diagnostic
snapshot.
