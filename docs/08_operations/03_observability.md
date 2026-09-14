# Operational events and I/O diagnostics

Atomic exposes native reports and an optional bounded event publisher. No
monitoring server, dashboard, JVM logging runtime or external backend is required.
Applications can send JSON lines to their existing logging/monitoring system.
Detailed read/transaction diagnostics and stock event publication are off by
default. Normal results, stored history and durable receipts are unchanged.

## Explain one operation

Use a qualified business label, not a customer ID, query value or credential:

```rust,ignore
use atomic_core::{Keyword, OperationContext, OperationKind, io_report_to_edn};

let operation = OperationContext::named(
    OperationKind::Query, Keyword::new("app", "account-summary"),
)?;
let (result, report) = operation.measure(|| database.query(&query, &inputs, &control));
// `result` is the original Result; diagnostics do not turn an error into success.
let edn = io_report_to_edn(&report);
```

`OperationContext::diagnostic(kind)` enables the same detail without a business
name. Existing `OperationContext::new(kind)` keeps its SQL-only behavior. Enter a
context around the actual consumption of lazy cursors, or call `report()` after
consumption; measuring only cursor construction does not measure future work.
Explicit context propagation preserves attribution across workers and cursors.

`child_named` creates a named sub-operation. `report.nested` groups inclusive
statistics. An unnamed phase/submission inherits its nearest named ancestor's
label in the report; a deliberately named child overrides it. Nested totals group
statistics by distinct descendant label; parents and children overlap and must
not be added together. At most 128 labels are retained per context; overflow sets
`nested_truncated` without changing total counts or the operation result. Reports
are local observations, not a global delta obtained by subtracting two concurrent
process snapshots. EDN conversion preserves unsigned counts exactly, using a
big integer above `i64::MAX`.

Stock `atomic query --io-context :app/account-summary` and
`atomic pull --io-context :app/account-summary` return I/O reports with their
ordinary result. `atomic query --query-stats` additionally explains native clause
scheduling. See [query diagnostics](../05_query_and_pull/04_diagnostics.md) for clause paths,
binding names, row flow, phase nesting and capture limits. The native optimizer
remains enabled; diagnostic warnings are investigation aids, not tuning commands.

### Interpret the counters

- SQL `calls` count driver API calls, not packets or protocol round trips. Row
  field bytes and known application payload bytes are distinct measures, neither
  a disk/network traffic measurement nor heap residency.
- `operation_elapsed_nanos` is elapsed operation wall time. SQL call durations,
  nested phase durations and cache tier durations may overlap or run concurrently;
  sums do not reconstruct end-to-end latency.
- Cache tiers identify actual decoded-node, local-disk, PostgreSQL-block and
  in-flight accesses. Hits/misses, returned physical/canonical payload bytes and
  duration statistics describe that tier. A decoded miss may be served by disk
  or another loader; it is not necessarily a PostgreSQL request.
  In-flight hits/errors refer only to the same requested node. Waiting for a
  slot occupied by another node counts as a wait/miss, not that other request's
  result or error.
- Index counters name the native order and count cursors, node accesses and
  decoded misses. A query's logical index seek is not a physical storage read.
- Semantic transaction work counts attempted assessments, identity claims/lookups,
  upsert resolutions, uniqueness/redundancy checks, discarded duplicate/redundant
  datoms, composite work, function expansion and produced datoms. These are actual
  native steps, not unique user forms or identical counts across different engines.

`ServiceTransactionReport::diagnostics`, when enabled, contains the committed
basis, replay flag and safe operation report. It is ephemeral process diagnostics,
not new durable receipt content. Exact receipt replay does not re-execute fresh
assessment work merely to reconstruct statistics. An explicitly diagnostic caller
context or configured service telemetry enables the returned detail.

## Publish from a library or stock service

```rust,ignore
use atomic_core::{TelemetryConfig, TelemetryPublisher, TelemetrySnapshot,
                  TelemetryPhase, ServiceOptions};
use std::time::Duration;

let publication = TelemetryPublisher::start(
    TelemetryConfig { enabled: true, interval: Duration::from_secs(10),
                      queue_capacity: 64, max_event_bytes: 16 * 1024,
                      ..Default::default() },
    |json_line| existing_log_sink.write_line(json_line), // std::io::Result<()>
)?;
let emitter = publication.emitter();
let options = ServiceOptions { telemetry: Some(emitter.clone()), ..Default::default() };
// Pass options to TransactionService::start_with_options or standby.

// From an existing lifecycle loop: disabled/not-due calls do not capture stats.
emitter.publish_if_due(|| TelemetrySnapshot::capture(
    Some(&client), TelemetryPhase::Active, configured_listeners_ready,
));

// Bounded wait; false means a sink is still executing, not lost writer authority.
let sink_stopped = publication.shutdown(Duration::from_millis(100));
```

`TelemetryPublisher::stdout(config)` writes and flushes one JSON line per event.
Stock `atomic transactor --telemetry-ms 10000` enables publication from the existing
lifecycle loop and enables transaction events. Other startup/status lines on
stdout retain their existing format; select JSON event lines in a collector.
The collector must drain stdout during normal operation: startup/status output
uses ordinary synchronous stdout. If the telemetry worker does not stop within
the shutdown wait, the CLI omits its final `STOPPED` stdout line instead of
blocking behind that worker. Telemetry delivery is best effort, not a durable log.
Library users can customize queue/event-byte admission and `TelemetryWarnings`.
There is no hidden periodic sampler: callers choose when to drive
`publish_if_due`. Explicit `try_publish` samples immediately. An
`operation_callback()` or `publish_operation(&report)` bridges operation reports
to the same queue; no sink executes inside the operation/storage lock.

JSON events include `version`, `event`, `level`, publisher-local `sequence` and
wall-clock `unix_millis`. Sequences can have gaps after admission drops and are
not durable order guarantees across processes. Service events correlate by
immutable storage ID/lineage, lease epoch where available, and transaction basis.
No public catalog-name lookup is done during publication; a storage ID initially
chosen from a name may still be recognizable. Named operation labels are explicit
application metadata. Events exclude request keys, tempids, datom values, complete
queries, callback payloads, connection strings and error messages. Error codes
remain available for diagnosis. Keep secrets out of deliberate labels and custom
sink code; custom callback panics still use Rust's configured panic hook.

### Events and warnings

`atomic.metrics` reports service queue/processed/rejection totals, index publication
and pressure, fulltext attempts/failures, excision progress and publisher outcomes.
Its `process.sql.*` totals are process-wide, not attributed to a single database.
`atomic.operation` and `atomic.transaction` contain operation-local metrics and
named descendant groups. Counter names are an open set; consumers should ignore
unrecognized fields. Units are in field names. No host RAM, heartbeat latency or
lease-renewal latency is inferred from a sampling interval.

Configurable warnings are emitted at sampling opportunities:

- `writer_unavailable`: an active/failed lifecycle reports unavailable writer
  authority. This can follow lease loss or another worker failure; it does not
  assert a specific cause or perform a fresh SQL lease check.
- `not_ready`: an active lifecycle lacks writer availability or listener readiness.
- `backpressure`: rejected admission increased since the preceding sample for
  that identity, or the optional configured queued-request threshold is met.
- `indexing_failed` / `fulltext_failed`: a corresponding current failure exists.
- `excision_paused`: an excision failure/admission pause remains observable.

The complete event name has the `atomic.warning.` prefix. Persistent conditions
may warn at each interval; rejected-count increases are compared with the previous
sample. Recovered state is visible in subsequent snapshots. These events are not
a paging service or an independent health/lease oracle. Configure thresholds for
your deployment, and retain `/health` versus `/ready` for supervisor checks.

### Cost, backpressure and shutdown

One optional worker invokes the sink. Producers encode a bounded JSON line, take
a short in-memory admission lock and use `try_send`; they never wait for sink I/O.
The configured queue holds at most `queue_capacity` lines, each with an encoded
limit of `max_event_bytes`, plus one active sink event and temporary producer
encoding buffers. Concurrent producers and a callback's own memory are not covered
by this queue bound; it is not an allocator/RSS guarantee. Sampling uses fixed
service/index aggregates, does not scan queued reports, makes no SQL calls and
does not copy failure messages. Existing local locks can briefly contend.

Oversized or full-queue events are dropped as whole events, never truncated into
invalid JSON. `TelemetryStats` and subsequent metrics expose accepted/delivered,
full/oversize/stopped/shutdown drops, sink errors/panics and delivered bytes.
Accepted means queued, not durably logged. Sink failures do not fail a transaction;
panic containment allows later attempts, subject to the application's panic hook.

`request_stop`, Drop and `shutdown` stop new admission and discard pending events.
`shutdown(timeout)` waits at most for its completion notification; if a callback,
its destructor or stdout write does not return, the worker is detached and owns
that work until it finishes. It cannot be forcibly cancelled safely. Do not
repeatedly create publishers around permanently blocked sinks. Disabled mode
starts no worker and `publish_if_due` does not invoke its snapshot closure.

Use existing application logging/monitoring sinks; no vendor backend or
particular logging framework is required.
