# Prefetch and maintenance controls

These controls trade background work against CPU and storage contention.
Defaults use single-lane hint/index preparation and unpaced maintenance.
They do not change transaction meaning or request identity.

Optional hint generation and worker concurrency belong to the
[transaction hints guide](../09_optional/01_transaction_hints.md). The controls
below govern required index preparation and explicit maintenance.

## Incremental index preparation

`ServiceOptions.index_preparation_parallelism` accepts 1–8, defaulting to one.
One background job captures a protected immutable prefix, prepares eight
current/history covering indexes on its own transport, and returns a candidate
to the writer for guarded adoption. Writes can advance while it prepares;
adoption preserves newer transactions and receipts. A stale source or GC guard
causes bounded retry, never publication of an unprotected candidate.

Within preparation, selected physical-index edit streams are prepared in groups
of the configured width. Sorting and canonical edit preparation can overlap;
object loading, tree merging and publication remain ordered. Every scoped
preparation worker is joined, including on error. Only the selected group's
prepared edits coexist, so higher parallelism can increase temporary memory.
This is not a process-memory ceiling or a parallel-connection count.

AVET backfill persists bounded progress against its frozen source. `noHistory`
omissions use one consistent witness across the covering indexes. The coherent
job also prepares a source-bound fulltext attachment when required: a search
failure is visible and does not become a successful index publication with
missing search data.

`PreparedIndex::stats()` returns `BlockIndexStats`: input and projection datoms,
projection steps, nodes/bytes read and written, reused subtrees, noHistory pairs,
preloaded-byte peak and preparation-worker counts. These are algorithm counters,
not measured RSS or CPU utilization. `background_indexing_stats().fulltext`
observes the whole coherent index/search job, including non-search failures.
Only adoption advances `checked_basis_t`; retries increment whole-job attempts
through the indexing scheduler. Compare complete preparation/adoption time,
I/O and foreground latency before raising parallelism.

`ServiceOptions.fulltext_build_limits` configures the shared search-build policy
for indexing and automatic excision, including standby takeover.
`PostgresOperator::with_fulltext_build_limits` applies it to consolidation,
explicit current-index recovery, search rebuild and administrative excision.
The limits cover sort/page/record admission and cumulative spill writes, not
whole-process RSS. Invalid settings reject before work; a larger valid policy
can resume durable work. Defaults are unchanged. See [fulltext](../05_query_and_pull/03_fulltext.md).

## Cooperative maintenance pacing

`MaintenanceControl::new(pause, cancel_flag)` provides shared cancellation and
a pause after each completed batch. Attach it with
`PostgresOperator::with_maintenance_control` or
`PortableBackup::with_maintenance_control`. An external maintenance loop can
call `check()` before work and `after_batch()` after a completed safe batch.
Do not hold a SQL write transaction or exclusive storage lock across the pause.

Clones share counters/cancellation, but each caller pauses independently.
Pacing is not a global bandwidth limiter. It checks cancellation between short
sleeps; I/O and CPU work within a batch retain their own interruption rules.
Cancellation returns `maintenance/canceled` without undoing completed work.
Incomplete immutable uploads are never exposed by an incomplete root. Protected
resumable work retains its completed graph; unowned abandoned uploads eventually
become ordinary GC candidates.

Batch sizes depend on the operation, not one universal byte budget. Backup
traversal, restore checkpoints, index steps and GC each define their own safe
boundaries. Restore resumes the same point/target after cancellation. One GC
call advances a bounded portion of a persisted cycle; follow its progress until
that cycle completes. See [operations](00_deployment.md).

`MaintenanceStats` records completed batches, pauses and actual pacing wall
time. A completed batch is counted even if cancellation is noticed immediately
afterward. Zero pause performs no sleep. The final batch can also pause before
the following publication or return. Driver deadlines separately bound SQL
statements/lock waits; neither pacing nor cancellation promises preemption of
arbitrary Rust work.
