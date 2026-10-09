# Asynchronous client API review

Read assigned page `docs-2/07_peer_api/10_client_api_async.md` in full, NORTHSTAR,
timestamp reference, CUTOVER, and all three supplied talks. Page already states
one request source, ordinary reference resolution, six datom slots/orders,
automatic ordinary timestamp, exact-retry identity, and source after the existing
speculation arguments. Leave it unchanged unless code evidence reveals a mismatch.

Observed: `application/asynchronous/client.rs` reserves before capture, moves the
complete request unchanged to native transaction/transport APIs, delegates query
and collection to captured native values, and never resolves request sources.
`runtime/executor/mod.rs` owns bounded admission, worker cleanup, callbacks, and
queued deadlines; `asynchronous/stream.rs` advances one demand-driven chunk with
successful rows before a terminal error. Source remains native information while
scheduling remains an independent concern. Adding source adapters or duplicate
visibility logic would entangle these responsibilities and inhibit change.

Bounded concern: existing native query tests prove source/current/history semantics,
and live client/synchronization tests prove transaction replay and queue deadlines,
but offline acceptance does not exercise source-slot queries through all three
public AsyncExecutor query methods. Prefer focused async acceptance over facade
changes or repeating passing live suites. Requested root ownership of new
`tests/async_api_source_cutover.rs`; no core edits proposed. Acceptance: materialized,
prepared, and chunked queries preserve distinct source contributions, globally
withdrawn current values stay withdrawn even when the source filter excludes the
retractor, and history still exposes original assertions plus attributed withdrawal.

Inspected existing evidence (not rerun): `client-tutorial-live.log` has 1 actual live
pass for sync/async socket exact retries and source mismatch; its test checks six
report slots and one source-attributed timestamp. `synchronization-live.log` has
1 actual live pass for frozen views, async callback gating, expired queued sync
without SQL, and successful later synchronization. `tests-integrated-fifth.log`
records passing native query/source, filter, index, Pull, identity, timestamp and
function checks; live ignores there are not counted as executions.

Root approved the unique test only for a concrete async capture boundary. Refined
check: hold the sole worker in a once-only completion callback; admit materialized,
prepared and demand-driven sequence queries using the original source-bearing
snapshot; replace/drop the caller's source handles with a newer cross-source
withdrawal before releasing the worker. All operations must retain the old basis
and six-slot source/added meaning. Fresh current/history queries then distinguish
the withdrawal. This tests ownership/capture before execution (including stream
factory capture before first poll), rather than duplicating synchronous query tests.

Result: added only `tests/async_api_source_cutover.rs`. The sole-worker callback
gate proves materialized/prepared work cannot execute before caller source
replacement; the sequence is admitted before replacement and first polled after
replacement. Each returns both original sources with `added=true`, terminates,
and remains terminated on another next call. Later current and source-filtered
queries are empty; history exposes sources 98/99 assertions and source 97 withdrawal.
No runtime defect was observed, so no production code or documentation changed.

Actual new checks: `cargo test --test async_api_source_cutover` passes 1 test,
0 failed/ignored (0.13s), recorded in `doc_async_api-test.log`.
`cargo clippy --test async_api_source_cutover -- -D warnings` exits 0, recorded
in `doc_async_api-clippy.log`. Focused diff whitespace check exits 0; assigned
page diff is empty. Only the new test file was formatted.

Additional concrete parity reviewed: `model/datom.rs` retains six public fields
and six sort orders; `transactor/request.rs` and `edn_transaction.rs` preserve
constructor/speculation arities and required request-level source;
`transactor/authority/mod.rs:529-615` includes source in canonical identity and
looks up exact receipts before fresh gates and clock/source assessment;
`transaction/pipeline.rs` shares durable/speculative generation;
`database_value/cursor.rs` dispatches native current/history windows and prefix,
forward/reverse seek; `database_value/views.rs` preserves immutable boundaries;
`pull/index.rs` retains raw datom multiplicity with native projection;
`database_stats.rs` counts controlled history; `query/sequence.rs` retains source
values while demanding Pull projection; `storage/catalog/mod.rs` exposes the
specified synchronous create/list/checked-retirement methods. None introduce an
async-specific source policy or retired timestamp/source input path.

No known remaining page-specific gap or ownership dependency. Bounded-log chunk
admission remains the consumer worker's separate concern; this page explicitly
states transaction-count chunking is not a byte cap. Existing live logs were
inspected and reused, not rerun. Root owns integration/full-suite release checks;
no production resources, historical report edits, or commits.
