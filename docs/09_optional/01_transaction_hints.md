# Reducing latency with transaction hints

EDB creates a new database value by [accreting a database with a set of new
information](../04_transactions/05_transaction_model.md). In order to accomplish
this goal, EDB reads db-before to resolve identities in datoms, maintain composite
tuple attributes, uphold invariants (e.g. unique values, cardinality-one
retractions, redundancy elimination), as well as to expand information sets or
assert application invariants expressed through transaction functions and entity
predicates. Entity predicates check the complete proposed db-after.

Such reads against db-before contribute to transaction latency, and thus it is
important for data to be locally available before it is necessary. Certain data
dependencies limit the amount of prefetching possible: checking datom invariants
depends on resolving entity IDs, and transaction functions can perform reads that
are not evident from the submitted forms alone.

With transaction hints, applications can analyze a transaction to convey hints
that enable the writer to prefetch data while processing the transaction. This
requires speculatively running the transaction in the application prior to
submission. Applications can independently and concurrently generate hints;
EDB records a bounded set of index-prefix reads, rather than promising exhaustive
prefetch. Whether this reduces queueing or processing time depends on the
workload and cache state. Measure hint generation and execution together with
[transaction diagnostics](../04_transactions/03_diagnostics.md) and
[I/O statistics](../08_operations/03_observability.md).

## Using transaction hints

To calculate transaction hints from a captured `DatabaseValue`, use
`DatabaseValue::with_forms_with_hints`. Given `db`, the original
`TransactionRequest` named `request`, and a proposed transaction instant in
milliseconds named `preview_instant`:

```rust
let hinted = db.with_forms_with_hints(
    &request.forms,
    preview_instant,
    request.source.clone(),
    edb_core::SpeculationLimits::default(),
    edb_core::HintLimits::default(),
)?;
```

This runs ordinary pure speculation with the supplied `SpeculationLimits` and an
additional bounded `HintLimits` read trace. The request's required source applies
to every fact produced by the preview, including its automatic transaction
timestamp. The returned `HintedSpeculation`
contains the ordinary speculative `report`, optional `hints`, and trace `stats`.
No data is written. Eager in-memory values, speculative values and opaque-filter
origins do not provide a portable committed origin: they still speculate
normally, but return no hints. Broad scans and boundary seeks are counted, not
expanded into unbounded hint lists; omitted reads do not invalidate the preview.

The hints can be passed to an attached in-process `Connection` or
`TransactionClient` using `submit_with_hints`. For a `Connection` named `conn`:

```rust
let (ticket, execution) = match hinted.hints {
    Some(hints) => {
        let (ticket, execution) = conn.submit_with_hints(
            request,
            hints,
            edb_core::HintPrefetchOptions::default(),
        )?;
        (ticket, Some(execution))
    }
    None => (conn.submit(request)?, None),
};
let report = ticket.wait(std::time::Duration::from_secs(30))?;
if let Some(execution) = execution {
    println!("{:?}", execution.snapshot());
}
```

Keep the original logical forms for the actual transaction; preview IDs and
datoms are not a safe replacement for that intent. Transaction semantics are
unchanged when using hints, even though the database used to create hints and
the database into which the transaction is durably incorporated may differ.
Hints do not lock a basis or reserve identities. Use an explicit request basis
guard when a plan requires one; see [transactions and exact
retries](../04_transactions/00_transactions.md).

The canonical request hash includes the forms, required source, basis condition,
and explicit time override, but does not include hints. Stale, absent, unrelated or
failed prefetch never changes transaction meaning. A stale basis in the same
lineage and generation may still be useful; hints from another lineage or
generation are ignored. The writer reads its own authenticated current value,
not caller-supplied blocks.

`Connection::transact_remote_with_hints` carries hints in a separate versioned
channel, retaining the same canonical request; it returns a
`CommittedTransaction`, without the in-process `HintExecution` handle.
`AsyncClient::transact_with_hints` provides asynchronous in-process
submission and returns the report together with that handle; see the
[async client](../07_peer_api/01_async_client.md).
The [application example](../../examples/application_workflow.rs) generates
hints without transporting them.

Transaction functions and predicates needed for speculation must be available
in the speculative environment. This hint-generation wrapper uses default
transaction allocation options and an empty native registry; it does not accept
`TransactionExecutionOptions`. If the transaction requires compiled Rust
callbacks, use `with_forms_with_execution_options` for an exact preview with the
required registry; that API does not return hints. See [compiled Rust transaction
deployments](../07_peer_api/02_native_computation.md#compiled-rust-transaction-deployments).

## Prefetch concurrency and limits

Prefetch has its own driver and cold-miss lanes and shares bounded authenticated
RAM caches and immutable snapshot state. It does not use the SSD path. The default
is one worker per writer; `ServiceOptions::hint_prefetch` can disable it or select
up to eight using `HintPrefetchConcurrency::max_workers`. At most eight per
process can remain active, including stalled workers after service replacement.
Saturation skips hints; transaction completion never joins a worker before
acknowledgement. There is no API requirement for a minimum CPU-core count.

Client trace limits and server prefetch limits are independent and clamped:

| Limit | Default | Maximum |
| --- | --- | --- |
| Retained hint prefixes (`HintLimits::max_prefixes`) | 128 | 4,096 |
| Retained prefix weight (`HintLimits::max_bytes`) | 64 KiB | 4 MiB |
| Delivered prefetch datoms (`HintPrefetchOptions::max_datoms`) | 1,024 | 16,384 |
| Delivered prefetch weight (`HintPrefetchOptions::max_read_bytes`) | 4 MiB | 16 MiB |
| Cooperative prefetch timeout (`HintPrefetchOptions::timeout`) | 20 ms | 1 s |

The hint origin has a separate 256 KiB admission limit. Prefix weights and
delivered-datom bytes are not allocator or RSS bounds. The read-byte budget is
checked between datoms and shared across the request's workers; each active lane
may have one already-admitted datom beyond the byte allowance. The largest
delivered datom is reported as `max_inflight_datom_bytes`. The datom-count
allowance is shared and does not multiply with workers. Native block and cache
limits still apply independently.

`HintPrefetchOptions::cancel` cancels prefetch only, never the transaction.
Cancellation after processing stops subsequent reads but does not claim to
interrupt synchronous driver calls or connection startup. Likewise, the
cooperative timeout is not an end-to-end wall-clock bound on a synchronous
PostgreSQL read.

## Measuring hints

`HintTraceStats` reports prefix requests, admitted prefixes, retained weight,
omitted requests, broad reads and generation time. `HintExecution::snapshot()`
reports prefetch reads, errors, ignored origins, saturation, worker activity and
queue/processing/prefetch/overlap timing. Its values can change after
acknowledgement while `active_workers` is nonzero. Waiting for measurements is
optional, not commit work.

`peak_active_workers` measures overlapping worker lifetimes. `prefetch_nanos`
and `overlap_nanos` sum lane intervals, not end-to-end wall time. Compare
processing latency and actual I/O before increasing concurrency. Include hint
generation, queue and processing time, overlap, and total SQL/cache effects in
your workload measurements: an already warm transaction can pay more I/O and
gain nothing. No speedup is guaranteed.

See [maintenance controls](../08_operations/04_maintenance_controls.md) for the
separate index-preparation and administrative pacing policies.
