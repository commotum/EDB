# Configuring logging

## Transactor logging

The EDB transactor writes startup and lifecycle messages to standard output and
errors to standard error. Add `--telemetry-ms` to publish JSON operational events
at the specified sampling interval in milliseconds and enable transaction events.
The interval must be a positive integer. Without this option, the stock transactor
does not publish telemetry; ordinary lifecycle and error output remains enabled.

For a local deployment with storage already installed, a database named `app`
already created, and `EDB_POSTGRES_URL` configured for the writer role:

```sh
umask 077
mkdir -p run log
chmod 700 run
edb transactor --database app --endpoint "$PWD/run/app.sock" \
  --telemetry-ms 10000 >>log/transactor.out 2>>log/transactor.err
```

This runs the transactor in the foreground and appends output to files selected by
the shell. EDB has no `log-dir` property or default log directory. For a supervised
deployment, configure the supervisor or log collector to capture both streams.
See [running a transactor](../01_tutorials/04_run_a_transactor.md) and the
[transactor reference](07_transactor_reference.md) for provisioning and transport
configuration.

Standard output contains ordinary lines such as `READY`, `STANDBY`, and `STOPPED`
alongside JSON events. Configure a collector to recognize the JSON lines without
treating the entire stream as JSON. The collector must continue draining output:
startup and lifecycle messages use synchronous writes, even though telemetry
delivery uses a separate worker.

Telemetry events include `edb.metrics`, `edb.transaction`, and applicable
`edb.warning.*` events. JSON includes an event name, level, publisher-local
sequence, and timestamp. Telemetry excludes connection strings, query text,
datom values, and error messages; intentional application labels still need to be
chosen appropriately. See [observability](03_observability.md) for event fields,
metrics, warning conditions, and delivery limits.

### Error output

On command failure, the CLI writes `ERROR category=... code=...` to standard
error. It includes numeric source positions when available and prints the error
message for `cli/` and `config/` errors. Arbitrary storage and transaction error
messages are not printed. Incorrect-input errors exit with status 2; other
reported errors exit with status 1.

During operation, the transactor also reports excision failure and recovery with
`TRANSACTOR_EXCISION_FAILURE` and `TRANSACTOR_EXCISION_RECOVERED`. When availability
is lost, a recorded indexing failure can produce `TRANSACTOR_INDEX_FAILURE`.
Collect standard error even when JSON telemetry is enabled. A failed observation
does not imply that a submitted transaction rolled back.

## Transactor log rotation and archiving

Configure file rotation, retention, and archive uploads in the supervisor or log
collector. EDB does not automatically rotate output files or migrate them to S3.
If you need more flexible access to logs, configure a collector or implement a
native telemetry sink.

When redirecting output as above, renaming a file does not change the process's
open output descriptor. Arrange rotation through a collector that manages its own
files, or coordinate reopening with the deployment's process lifecycle. Plan
retention and available disk space for both output streams. Archive completed
files through your deployment's storage tooling.

## Peer logging

The EDB peer library returns errors to the application and exposes operation
reports. The application chooses which errors to log and where to send reports.
There is no JVM logging configuration, classpath dependency, or global logging
switch. Adding a Rust logging dependency alone does not enable EDB telemetry.

To configure publication in a Rust application:

- Create an enabled `TelemetryPublisher` with a standard-output or custom sink.
- Publish operation reports through its emitter, or attach the emitter to
  `ServiceOptions::telemetry` when embedding a transaction service.
- Keep the publisher alive for the application's publication lifetime.

### Peer logging with standard output

`TelemetryPublisher::stdout(config)` writes and flushes one JSON line per event.
Use `TelemetryConfig { enabled: true, ..Default::default() }` to enable it. The
default queue holds 64 waiting events, excluding the active sink call; each event
is limited to 16 KiB including its newline. Periodic sampling defaults to 60
seconds, but library applications must call `publish_if_due` from their own
lifecycle loop. Creating a publisher does not start a sampler.

### Peer logging with a custom sink

`TelemetryPublisher::start` accepts a callback returning `std::io::Result<()>`.
You can write a custom sink for arbitrary integrations, including an existing
application logging system. For example, this function configures an append-only
file sink; its parent directory must already exist:

```rust
use edb_core::{TelemetryConfig, TelemetryPublisher};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

fn file_logging(path: &Path)
    -> Result<TelemetryPublisher, Box<dyn std::error::Error>>
{
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let publisher = TelemetryPublisher::start(
        TelemetryConfig { enabled: true, ..Default::default() },
        move |line| {
            file.write_all(line.as_bytes())?;
            file.flush()
        },
    )?;
    Ok(publisher)
}
```

Publish a collected operation report with
`publisher.emitter().publish_operation(&report)`. Alternatively, pass
`publisher.emitter().operation_callback()` to
`OperationContext::with_callback`, then explicitly call the context's `publish()`
after releasing application and storage locks. Dropping a context does not
publish a report. See [I/O Stats](../07_peer_api/08_io_stats.md) for collecting
reports around actual operations.

The sink runs on a dedicated worker. A full queue or oversized event is dropped;
sink errors and panics are counted, without retrying that event. Inspect
`publisher.stats()` for delivery, drop, and sink-failure counters. A successful
enqueue is not a delivery acknowledgement, and file flushing is not a durable
audit-log guarantee.

At shutdown, `publisher.shutdown(timeout)` stops admission and discards pending
events. It returns false if the sink has not finished within the wait; a blocked
worker is detached, not forcibly terminated. Dropping the publisher requests stop
without waiting. Account for this best-effort delivery when troubleshooting
missing events, and keep application error handling independent of telemetry.
