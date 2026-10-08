# Async client review

Read the complete assigned page, NORTHSTAR, TIMESTAMP_SOURCE_DESIGN, CUTOVER
and three supplied talks. The page already uses one source-bearing native
`TransactionRequest` for both sync and async writes, retains exact source on
retry, and distinguishes named query inputs from transaction attribution.
The bounded concern was whether the facade introduces independent source
resolution, request identity, cancellation or deadline behavior. Inspection
found no cutover gap; the page remains unchanged.

Decision: retain the existing facade over native data and operations. Resolving
source before worker submission would couple client capture to authoritative
assessment and defeat receipt-first retry. A second async request/deadline or
compatibility mechanism would duplicate established policy. `AsyncClient`
passes the original request to native attached/socket/TLS/routed writes;
`Reservation::run` owns admission/deadlines and `Owned` owns deferred cleanup.
Attribution, immutable capture, execution scheduling and transport authority
remain independently understandable and composable. No implementation edit
or extra test was warranted for this aligned overview.

Concrete acceptance paths inspected:

- `src/application/asynchronous/client.rs`: fixed native capture before polling;
  unchanged source-bearing request forwarding; remaining deadlines after queue
  delay; transaction jobs marked non-cancelable; routed identity check; explicit
  database/relation/log query sources and native prepared execution.
- `src/runtime/executor/mod.rs`: positive 4/64/64 defaults, capacity permits
  retained through unconsumed completion/cleanup, expiry before invocation,
  read abandonment versus admitted writes, worker-only callbacks and disposal,
  contained panics, no blocking join in executor drop. `stream.rs` uses the same
  reservations, one outstanding job, admission wakeups, step deadlines and
  worker-owned cursor cleanup; successful rows precede terminal errors.
- Native `connection.rs`, `transactor/request.rs`, `encoding/mod.rs`,
  `transactor/authority/mod.rs` and `transaction/assess/mod.rs`: ordinary source
  in complete request identity, matching receipt before fresh gates/clock/source
  resolution, one resolved source for generated facts and automatic timestamp,
  known committed reports preserved through observation failure.
- `remote/routing.rs`, local transport client, `database_value/log.rs`,
  `query/sequence.rs` and catalog implementation: stable identity and no replay
  after attempted remote submission, confirmed-commit/report separation,
  captured log endpoint, eager joins with demanded Pull projection, checked
  administrative catalog pagination and retirement.

Actual evidence reused after inspecting test bodies/logs: synchronization-live.log
records 1 actual pass, 0 ignored (0.62s), with callback-controlled occupied worker,
expired queued sync/sync_to returning async/deadline and zero caller SQL.
client-tutorial-live.log records 1 actual pass, 0 ignored (0.69s), preserving
six-field reports, sourced generated schema/timestamp facts, async exact retry
and changed-source conflict; doc_client_workflow.md records its implementation.
The suggested client-workflow-source-live.log is absent; no result is attributed
to it. tests-integrated-fourth.log independently totals 150 passed, 0 failed,
14 explicitly ignored live tests across 32 binaries. No fresh suite was rerun.
The assigned-page diff is empty and its focused whitespace check passed.

No code, documentation or test edits; no known remaining page-specific gap.
Queued write abandonment and blocked-driver cancellation limits were inspected,
not newly fault-injected. Adjacent connection review agrees with the same capture
and receipt boundaries. Root owns integration and final release checks; no broader
release-readiness claim, production resources or commits.
