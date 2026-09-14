# Transaction diagnostics

Use a diagnostic operation context when the cost of a transaction needs explaining:

```rust
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
```

Submission carries the context across the service queue even if the caller's
scope ends before waiting on its ticket. `OperationContext::diagnostic` enables
the same counters without a business label. Existing `new` contexts retain SQL
statistics but leave these additional semantic counters disabled. Pure
`Database::with` and `DatabaseValue::with` expose their counters through the
enclosing context's `report()` or `measure()` result.

Names are intentional operational labels: never put customer identifiers,
transaction values, credentials, or request keys in them.

## What the counters mean

- `assessments` and `input_operations` count assessment attempts and their
  expanded primitive operations. Nested function calls and repeated attempts
  accumulate, including work preceding an error.
- `identity_claims`, `identity_lookups`, and `upsert_resolutions` distinguish
  temporary unique-identity assertions, exact unique-attribute lookups, and
  matches to existing entities. Repeated matching claims count repeatedly.
- `uniqueness_checks` counts affected value groups checked by the shared
  selective assessor. Memory, speculative and durable transactions use the
  same assessment algorithm.
- `redundancy_checks` and `redundant_datoms` describe comparisons with db-before;
  `duplicate_datoms` describes duplicates removed inside the transaction.
- `composite_candidates` counts affected entity/composite pairs computed;
  `composite_datoms` counts generated assertions and retractions.
- `function_calls` counts transaction-function expansion attempts, including
  nested or rejected calls, not predicate invocations or dependency loading.
- `produced_datoms` counts material assessment output, including transaction
  metadata. Production of datoms is not itself a durability acknowledgement.

The existing expansion, assessment, encoding, commit, and report phase timings
remain in `report.stats.phases`. They are inclusive wall times, not a disjoint
CPU-time decomposition. Semantic counters exclude durable storage writes; SQL
statistics describe those separately. The receipt report's operation elapsed
time includes queue residence but stops before caller delivery/network time.

## Receipt replay and publication

`TransactionDiagnostics` contains the outcome basis, replay status, and safe
operation statistics. It contains no transaction forms, values, tempid names, or
request keys. It is ephemeral: durable receipt hashes/bytes and the native wire
receipt format are unchanged. Reconstructed remote/historical reports therefore
have `diagnostics: None`; they cannot recreate past execution costs honestly.

An exact retry resolves its receipt before current functions or schema are
assessed. A separately submitted retry has `replayed: true` and zero fresh
semantic work, although reconstructing its values can perform reads.

Configured `ServiceOptions::telemetry` enables detailed per-submission statistics
and emits basis-correlated transaction events through the bounded nonblocking
emitter. Sink callbacks run on the telemetry worker, not under transaction or
service locks. Disabled, full, or stopped telemetry never changes a commit result.
Shared receipt clones retain one `Arc` to their diagnostic snapshot.
