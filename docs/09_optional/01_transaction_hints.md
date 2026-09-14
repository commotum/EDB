# Advisory transaction hints

`DatabaseValue::with_forms_with_hints` runs ordinary pure speculation with the
supplied `SpeculationLimits` and an additional bounded `HintLimits` read trace.
It returns the unchanged kind of speculative report, optional `TransactionHints`
and trace statistics. Broad scans are counted, not expanded into unbounded hint
lists. Unsupported portable origins still speculate normally but return no hints.
Keep the original logical forms for the actual transaction; preview IDs/datoms
are not a safe replacement for that intent.

An attached in-process `TransactionClient` or `Connection` accepts
`submit_with_hints(request, hints, HintPrefetchOptions)` and returns the ordinary
ticket plus `HintExecution`. The canonical request hash does not include hints.
Stale, absent, unrelated or failed prefetch never changes transaction meaning.
The writer reads its own authenticated current value, not caller-supplied blocks.
`Connection::transact_remote_with_hints` carries hints in a separate versioned
channel, retaining the same canonical request. The application example currently
generates hints without transporting them.

Prefetch has its own driver and cold-miss lanes and shares only bounded
authenticated RAM index cache. It does not use the SSD path. The default is one
worker per writer; `ServiceOptions::hint_prefetch` can disable it or select up to
eight. At most eight per process can remain active, including stalled workers
after service replacement. Saturation skips hints; authority never joins a
worker before acknowledgement. Cancellation after processing stops subsequent
reads but does not claim to interrupt synchronous driver calls or connection
startup. `HintExecution::snapshot()` can change after acknowledgement while
`active_workers` is nonzero. Waiting for measurements is optional, not commit work.

`peak_active_workers` measures overlapping worker lifetimes. `prefetch_nanos`
and `overlap_nanos` sum lane intervals, not end-to-end wall time. Compare
processing latency and actual I/O before increasing concurrency.

Client/server limits are independent and clamped. The read-byte budget is checked
between datoms and shared across the request's workers; each active lane may have
one already-admitted datom beyond the byte allowance. The largest delivered datom
is reported as `max_inflight_datom_bytes`. The datom-count allowance is shared and
does not multiply with workers. Native block/cache limits still apply independently.
Trace weights and delivered-datom bytes are not allocator/RSS bounds. Compare
hint generation, queue/processing/overlap latency and total SQL/cache effects on
your workload: an already warm transaction can pay more I/O and gain nothing.
See [maintenance controls](../08_operations/04_maintenance_controls.md) for the
separate index-preparation and administrative pacing policies.
