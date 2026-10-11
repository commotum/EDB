# EDB Deployment

This guide includes advice for managing EDB deployments. For provisioning,
permissions, retention and recovery guarantees, see
[PostgreSQL operations](00_deployment.md). For executable configuration, see the
[transactor reference](07_transactor_reference.md).

## Getting Connected

A distributed EDB deployment consists of a PostgreSQL storage service, an
active transactor, optional standby transactors, and one or more peers. These
processes discover each other as follows:

- A transactor process connects to storage using its PostgreSQL connection
  configuration, resolves the logical database name to a stable route and
  lineage, and claims writer authority through a guarded lease.
- An active remote transactor publishes its advertised address in storage.
  Discovery authenticates the address against the database identity, fenced
  lease and listener instance.
- A transactor started with `--mode auto` waits while another writer holds
  authority and attempts activation after authority is released or expires.
- Peers connect directly to PostgreSQL using their own storage credentials.
  Remote transaction clients discover the transactor through storage. Local applications
  instead use the explicitly configured Unix socket, or attach to an embedded
  `TransactionService`.

Opening a peer does not require a running transactor. PostgreSQL supplies opaque
objects and guarded references; the native Rust engine interprets transactions,
indexes and queries. A remote transaction token does not supply the storage
permissions needed for peer reads.

A `Connection` combines an advancing peer with an optional transaction client.
Opening an independent read connection starts no writer. Attaching a client does
not transfer ownership of its service; an embedded connection instead retains
the `TransactionService` for the lifetime of its shared connection. Replacing an
attached writer requires the same database identity and lineage.

To provision a system and get it up and running, follow
[storage services](06_storage_services.md) and
[running the transactor](07_transactor_reference.md#running-the-transactor).
The stock executable serves one logical database per process. Use separate
processes for independent failure and resource boundaries.

## Getting Reconnected

Processes become unavailable for various reasons:

- A process or machine crashes.
- A process loses network connectivity to other processes.
- A process stops operating in a timely manner.
- An operator kills a process deliberately, for example as part of a deployment.

EDB uses writer fencing, immutable read values and exact transaction receipts
to handle these failures. Application availability also depends on storage,
process supervision and the selected transport.

### Transactor Failover

Run a standby transactor to reduce the interruption caused by writer failure.
Instructions are in [writer authority and standby](07_transactor_reference.md#writer-authority-and-standby).
Each contender needs its own socket path or TCP listen address and the same
database identity and intended resource policies.

The current defaults are a 5,000 ms lease, a 1,000 ms renewal interval and a
250 ms standby polling interval. Takeover uses lease eligibility and a new
epoch, not a fixed count of missed heartbeats. Guarded publication rejects stale
writer authority even if work began before takeover. These settings do not
establish a recovery-time guarantee: PostgreSQL access, process scheduling,
activation and listener startup also take time.

The stock transactor exits unsuccessfully if its service or a configured
listener becomes unavailable after activation. Run it under a supervisor that
restarts it. It does not silently return to standby. Embedded applications own
the service and listener lifecycle after `TransactionStandby` transfers the
activated service to them.

A transactor failover is not necessarily a bug, but frequent failovers indicate
an operational problem. Investigate the cause before increasing timeouts.
PostgreSQL replication and failover remain separate deployment responsibilities;
a standby EDB writer does not repair unavailable PostgreSQL storage.

### Monitoring Writer Availability

Use `STANDBY` and `READY` lifecycle output, process exit status, and optional
health probes to monitor activation. With `--health-listen` configured,
`/health` reports liveness and `/ready` reports active writer/listener readiness.
A waiting standby is live but not ready. Probe responses are local sampled
state, not a fresh storage check or a guarantee about the next transaction.

`--telemetry-ms 10000` enables bounded JSON operational events. Watch
`edb.warning.writer_unavailable`, `edb.warning.not_ready`, queue pressure and
index failures, and correlate by storage ID, lineage and lease epoch where
available. The publisher does not expose heartbeat or lease-renewal latency
metrics, and its sampling interval is not a measurement of either. Events are
best effort; drain stdout and monitor the processes and PostgreSQL independently.
See [observability](03_observability.md) for counters and delivery limits.

### Peer Failover

During writer unavailability, `Connection::db()` returns the most recent
immutable database value observed by that connection. Previously captured
values remain unchanged. Peers can continue to read and query without the
transactor, provided any required storage objects are available and retained.
A cached database handle is not a promise that all data needed by a future query
is already in memory.

Capturing `db()` performs no storage I/O and sends no request to the writer.
Use one captured value across related reads: repeatedly capturing the connection
can return different transaction bases. Holding a value does not pin its objects
against retirement or collection. See
[retirement age and reader protection](00_deployment.md#retirement-age-and-reader-protection).

Read synchronization is independent of writer connectivity. `Connection::sync()`
reads the current durable publication; `sync_to(t, timeout)` waits to observe at
least a specified transaction basis. Neither call means “wait until a transactor
is running.” If reaching the requested basis requires another write, progress
requires a writer. The peer synchronization path reconnects to storage and
retries once after an unavailable error; continuing failure is still an error.
The connection's background observer also reconnects its notification listener
and periodically checks durable storage for progress. Inspect
`Connection::observation_error()` when that observation is failing; automatic
observation does not guarantee that a disconnected peer is current.
See [synchronization](../04_transactions/09_synchronization.md) and
[connections](../07_peer_api/00_connections.md).

For remote submission, `Connection::remote_writer` caches a verified route. If
that route fails before request transmission, it can rediscover and try once
against a successor. It does not automatically resubmit an ambiguous request.
Local socket clients must select the endpoint of the process that became active;
there is no automatic local socket failover address.

Application code should be prepared for transient write failure and implement
appropriate retry strategies. Not all transactions are idempotent, and a
transaction may have succeeded even if communication back to the peer failed.
EDB provides exact receipts keyed by request identity to resolve this case:

- Preserve the request key and complete request content before submission.
- On `UnknownOutcome`, explicitly retry the identical request under the same
  key. An already committed request resolves to its retained exact receipt
  instead of repeating its effects.
- Do not create a new key or change the request to resolve an uncertain outcome.
  A new key identifies new work.
- If the application needs to inspect business effects, synchronize to the
  known committed basis before querying. Transaction metadata and business
  identifiers can help correlate effects, but do not replace exact retry.

A confirmed commit remains committed if constructing the local report or
observing the resulting publication fails. Keep those failures separate from
transaction success. See [transaction processing](../04_transactions/06_processing_transactions.md)
for receipt and publication semantics.

### Observing Transactions and Indexes

The connection's peer opens a published root and its indexed base and recent
transaction tail. The connection's background observer connects a storage notification listener and
checks durable state after that connection succeeds, after reconnecting, and
periodically. Notifications prompt catch-up; they are not authoritative commit
data. Missing a notification does not remove a committed transaction from the
durable log.

When the physical index and excision generation are unchanged, catch-up
authenticates the new root and verifies that the log extends the captured
prefix. It applies only the missing canonical transactions to the shared recent
tier, deriving schema, identifiers and AVET/SAVET readiness from those transactions.
It does not rerun the submitted transaction forms. A changed physical index or
generation opens the new indexed base and its recent tail. Index adoption
replaces the connection's next capture; previously returned values remain
unchanged. Opening an indexed base does not eagerly load every index block.

With transaction reports enabled, storage catch-up reconstructs ordered exact
reports from durable receipts and validates the entire batch before publishing
the new observation. Retained excision-generation handoffs allow a lagged peer
to recover reports across generations while the required history remains
available. Report reconstruction preserves each receipt's exact before and
after endpoints, even when adjacent endpoints have the same logical basis but
different physical index references. It reuses authenticated recent prefixes
across adjacent endpoints; receipt and log checks still require per-report work.
This does not establish a fixed storage-read count or latency guarantee.

An attached in-process writer can also deliver a committed report directly.
The observer validates that it is a contiguous successor before adopting it;
gaps fall back to storage catch-up. This bounded, nonblocking delivery can lose
notices without losing commits. Dropping a transaction ticket does not suppress
background observation.

There is one optional, unbounded report queue per shared connection. Enabling
it starts with subsequently adopted transactions, without replaying an already
observed prefix. Drain or remove the queue to release its retained reports and
database values. Do not depend on delivery ordering between a ticket waiter and
the queue. A report queue is an in-memory observation mechanism, not a durable
subscription; use [change consumers](../07_peer_api/03_change_consumers.md) for
resumable log delivery with explicit acknowledgments.

### Waiting for a Specific Completion

Transaction observation and background completion have separate targets:

| Wait | Completion condition |
| --- | --- |
| `sync_to(t, timeout)` | The connection has locally adopted at least basis `t`. |
| `sync_schema(t, timeout)` | The observed basis covers `t`, and AVET/SAVET additions required through `t` are ready. |
| `sync_index(t, timeout)` | The indexed basis covers `t`, with required AVET/SAVET additions and removals through `t` complete. |
| `sync_excise(t, timeout)` | The observed basis covers `t`, and excision requests through `t` have committed completion. |

Later work does not extend the requested target. A physical index descriptor
can contain partially completed AVET/SAVET work, so its basis alone does not prove
readiness. Readiness checks account for pending work at that descriptor's basis
separately from later schema transitions: a later disable/re-enable cycle cannot
hide unfinished earlier work. Adding uniqueness to an attribute with historical
values still requires physically ready AVET/SAVET indexes; waiting does not provide
an asynchronous uniqueness backfill.

`Connection::sync_to` waits on a condition variable whose predicate is protected
by the same mutex used to publish observation. The background observer performs
the storage reads; an already satisfied target returns immediately. The other
completion waits actively synchronize with storage. These waits do not schedule
maintenance, and a timeout does not cancel shared background work. See the
[native API reference](../07_peer_api/04_native_api_reference.md#connectionsync_index)
for indexing requests and completion APIs.

## Troubleshooting

Use logs and metrics to monitor your system and gather the information necessary
to characterize the problem. Record the build revision, lifecycle state, safe
error category/code, relevant basis and lease epoch, and recent deployment
changes. Keep credentials, request keys and subject data out of logs.

### Do You Have Enough Memory?

EDB uses timeouts and bounded admission in several places:

- A writer that fails to renew authority stops accepting work.
- Storage statement and lock deadlines can interrupt individual operations.
- Transport deadlines bound request/response delivery and can leave an
  admitted transaction with an unknown outcome.

If timeouts occur even though the network and storage layers seem healthy,
inspect CPU scheduling, memory pressure, swapping and process pauses. EDB is a
native Rust process; JVM garbage-collection flags do not apply. When looking for
the culprit, keep the following points in mind:

- Peers run application code and may retain many immutable values or large
  query results. Node cache limits do not bound total process memory.
- Transactor pressure can come from large transactions, transaction programs,
  indexing, excision or application callbacks in an embedded service.
- Queue, transaction, cache and recent-index limits bound different resources.
  Accounted bytes are not allocator RSS, and transport concurrency admits
  additional simultaneous work.

If a process has a memory problem that you do not yet understand, do not conceal
it merely by configuring the process with more memory. More memory may relieve
the immediate symptom without fixing a leak or unbounded retention. Measure the
workload and use [admission and memory controls](07_transactor_reference.md#admission-and-memory)
and [maintenance controls](04_maintenance_controls.md) deliberately.

### Are Your Processes Isolated?

EDB can distribute storage, transaction processing and application reads across
processes and machines. As a convenience for development, you can run storage,
transactor and peer on one developer machine. Those processes compete for the
same resources, and a single machine failure brings all of them down.

System tests should reproduce the intended deployment's resource and failure
boundaries. To test independence from writer failure, keep storage and the peer
outside the writer's failure boundary. For separate machines, use the remote TLS
transaction transport; a Unix socket is a local transport. Isolate staging from
production as described below. Process separation alone establishes neither a
performance nor an availability guarantee.

### Transactor Fails to Start

If a transactor process fails to start, check its error category and code and
verify the configuration:

- Storage must already be installed, and the logical database must exist.
  Startup validates storage and does not perform installation DDL.
- The writer credentials must reach the intended PostgreSQL database and schema
  with the required privileges. Verify reachability with `psql` under the same
  intended transport policy. Check certificate validity, hostname and trust root.
- `storage/writer-active` means another writer holds authority. Use
  `--mode auto` for an intended standby, with a distinct endpoint.
- For local sockets, verify directory ownership, owner-only permissions and the
  process user. Do not remove an unrecognized socket or lock to bypass checks.
- For remote listeners, verify bind and advertised addresses, TLS identity,
  token and private-file permissions. Choose exactly one of `--endpoint` and
  `--listen`.
- Verify resource limits and positive timing values; the renewal interval must
  be less than the lease duration.

The [transactor reference](07_transactor_reference.md#troubleshooting-connections)
describes the relevant configuration and diagnostics. A catalog entry from
`edb status` is not proof of a running writer. Wait for `READY` and check the
intended transaction endpoint.

### Transactor Starts Then Fails Later

If a transactor fails after running successfully for some time, investigate
storage unavailability, worker failure, listener failure or a pause of the
transactor process. For storage problems, possible causes include:

- Network partition or reachability loss between transactor and PostgreSQL.
- PostgreSQL restart or failover.
- Storage latency, disk saturation, lock contention or exhausted resources.
- Maintenance such as backup, replication work or VACUUM competing with normal
  traffic.

For process pauses, correlate gaps with OS and container logs, CPU throttling,
swapping, virtualization pauses and other work on the same host. Large native
callbacks are not preempted by a lease duration or SQL statement timeout.
Correlate `TRANSACTOR_INDEX_FAILURE`, excision diagnostics and operational events
with the first loss of availability. An excision admission pause can leave the
writer available with the request pending; it is not by itself proof of lease
loss.

Use PostgreSQL monitoring and operation I/O diagnostics to distinguish storage
waits from local work. Configure [transport and failure policy](00_deployment.md#transport-and-failure-policy)
for the role: a lock timeout is Busy and SQL cancellation is Interrupted, not
necessarily transport loss. Increasing a lease or timeout may tolerate a measured
pause, but does not fix the underlying resource or availability problem.

### Peer Fails to Connect

Peers must first connect to storage. Transaction submission additionally needs
the selected transactor endpoint. Diagnose these connections separately.
PostgreSQL errors carry a safe operation code and, when available, SQLSTATE;
remote transaction failures use native transport diagnostics.

For remote submission, verify the address supplied through `--advertise` is
reachable from the peer machine. A wildcard bind such as `0.0.0.0:7443` is not
an advertised destination. The TLS server name must match the certificate,
the client must trust its issuer, and both sides must use the same token.
There is no `host`/`alt-host` fallback pair.

In a containerized or dynamic environment, generate the advertised address at
launch from the actual reachable endpoint and provision the matching certificate
and server name. If storage and the transactor configuration are correct,
investigate peer network routes, firewall rules and permissions. For local
submission, the peer must run as the same OS user with access to the private
endpoint directory.

### Peer Connects Then Fails Later

If a peer connects successfully and later loses access:

- Correlate with writer lifecycle events: was there a failover, listener restart,
  transaction surge or index-pressure increase?
- Check PostgreSQL availability separately from transaction-listener availability.
- Inspect application and writer memory, CPU, I/O and scheduling pauses.
- Check certificate/token changes and whether the published address is still
  reachable from this peer.
- Distinguish a failed cold read or expired retained object from a failed write.
  A held database value does not pin its objects in storage.
- Reconcile uncertain submissions using identical keys and content before
  issuing replacement business work.

Retain the last captured database value when its basis is suitable for the
application, or explicitly synchronize to the required basis. Do not infer
freshness from a previously successful connection.

## Common Problems

### Unsupported Protocol or Format

Use a PostgreSQL DSN and the native local or verified TLS transaction transport.
EDB does not select alternative storage providers using a database URI protocol
or optional dependency. TCP PostgreSQL connections default to verified TLS;
`sslmode=disable` explicitly selects plaintext. Conflicting explicit transport
settings report `postgres/conflicting-transport`.

A storage-format error is a different problem. `storage/format` means the
selected namespace is not the current opaque storage format. Unsupported native
encodings can report `encoding/unsupported-version`. Installing again, renaming
tables or editing markers does not convert old objects. Follow
[release notices](../00_start_here/06_release_notices.md).

### Misconfigured Data Directory

PostgreSQL owns durable database storage. The EDB transactor has no `data-dir`
setting for a separate development database. Its local socket directory must
exist and meet ownership and permission requirements. Optional SSD caching,
backup repositories and administrative temporary storage have their own paths
and permissions; diagnose the path used by the failed operation.

For cache failures, consult [local SSD cache](../09_optional/00_local_cache.md).
Cached objects do not replace authoritative storage or a verified backup.
For repository paths and restore targets, see
[administration](01_administration.md).

### Health Endpoint Fails to Start

The native health endpoint uses two fixed request workers and an eight-socket
queue. It does not calculate a JVM thread pool from container CPU counts and has
no `ping-concurrency` setting. For `cli/health-listener`, check the selected IP
address, port availability, bind permissions and process/thread resource limits.
Excess admitted traffic is bounded; overload can close probe connections. See
[health check endpoint](07_transactor_reference.md#health-check-endpoint).

### Storage-Level Problems

If storage cannot return an object required for a read, that operation fails.
Missing, corrupt or unauthenticated objects are not silently replaced with
another representation. Check whether the problem is storage reachability,
permissions, corruption, or a cold read from a value whose objects are no longer
retained. EDB does not treat missing authoritative objects as an eventually
consistent provider's normal temporary response.

Normal startup does not eagerly rebuild damaged indexes. Explicit
`edb consolidate` can reconstruct a missing or damaged current read index from
the canonical log; it does not certify or repair every retained historical
index. Run deep inspection after recovery, and recover damaged retained history
from an intact backup. See [explicit index recovery](00_deployment.md#explicit-index-recovery)
and [retirement age and reader protection](00_deployment.md#retirement-age-and-reader-protection).

### Peers Fail to Connect to the Transactor

Storage access does not prove that the advertised transaction address is
reachable. Check the endpoint from the actual peer machine, the authenticated
discovery identity, lease state, TLS server name and token. A cached endpoint
is not proof of a live lease. For local sockets, select the active process's
socket after failover. See [peer fails to connect](#peer-fails-to-connect) and
[connecting to the transactor](07_transactor_reference.md#connecting-to-the-transactor).

### Work Fails After Shutdown

EDB has no JVM-wide peer thread pool or `shutdown-agents` operation. Service
shutdown stops admission, rejects remaining queued work, joins workers and
attempts to release writer authority. A stopped submission service must be
replaced or reattached as appropriate. Reads through independently held immutable
database values remain subject to storage availability and retention.

An admitted request whose worker disconnects before a result is delivered can
have `UnknownOutcome`. Shutdown is not evidence of rollback. Reconcile that
request with the replacement writer using the same key and content. See
[stopping and restarting](07_transactor_reference.md#stopping-and-restarting).

## Upgrading EDB Schema

Every EDB database starts with a fixed native base schema, including identifiers
namespaced with `:db`. Application schema is installed through ordinary
transactions. See [schema](../03_schema/00_schema.md) and
[changing schema](../03_schema/03_changing_schema.md) for supported alterations.

Application schema changes and engine format changes are separate operations.
There is no `:upgrade-schema` administrative action or published table of minimum
release versions for base-schema features. The current engine reads its current
native formats; earlier development databases, backups and checkpoints require
fresh storage and regenerated data. `edb install` is idempotent for a matching
current installation, but does not upgrade an earlier engine.

Do not use ordinary schema transactions or edit stored format markers to imitate
an engine upgrade. A matching PostgreSQL storage marker alone does not establish
compatibility of roots, indexes, durable logs or stored native programs. Consult
[release notices](../00_start_here/06_release_notices.md) before changing builds.

## Upgrading a Live System

EDB 0.1.0 is an experimental release. The package version alone does not
establish compatibility between source revisions. There is no general supported
rolling upgrade or mixed-build deployment order. Use peer libraries and
transactors built from the same source revision and test the intended change
before applying it to an existing installation.

Always test a new build in a staging environment before changing your deployment.
Writer standby and remote discovery allow process replacement within a compatible
deployment, but do not establish storage or wire compatibility between builds.
For changes without an established compatibility path, schedule coordinated
application/writer downtime and use a fresh installation when required by the
format boundary. Preserve the original environment and recovery artifacts until
the new deployment is verified.

### Testing Upgrades in a Staging Environment

Make your staging system as similar to the intended deployment as you feasibly
can. Keep staging and production fully separated:

- Do not connect one peer process to both environments; enforce isolation with
  separate credentials and network access rules.
- Use separate PostgreSQL instances or clusters when testing independent failure
  and resource behavior, as well as separate EDB installations and credentials.
- Use distinct listener endpoints, private directories and secrets.

With a suitable staging configuration, follow these steps:

1. [Back up and verify](01_administration.md#back-up-and-verify) the existing
   system. Retain the source revision and build that can read the backup.
2. [Restore](01_administration.md#restore-into-a-separately-selected-destination)
   into staging with a compatible build. A backup is not a format converter;
   for an incompatible development change, install fresh storage and regenerate
   the intended test data instead.
3. Start staging transactors and peers. Use the same stop/start, configuration
   and data preparation procedure planned for the real change.
4. Verify application queries, cold reads, historical reads and writes. Exercise
   exact request retries, standby takeover, client reconnection and maintenance
   relevant to the application.
5. Verify the recovery procedure, including the exact build and data needed to
   return to the prior environment. Measure interruption and resource use.

### Monitoring Process Replacement

For replacement processes built from the same revision, start an intended
contender with `--mode auto` and a distinct endpoint. Confirm it reaches
`STANDBY` with the expected storage identity and configuration before deliberately
stopping the active writer. That waiting state is not a promise that later
activation will succeed.

Stop the old writer through orderly shutdown, then monitor the successor for
`READY`, `/ready` success when configured, and successful application operations.
If release fails, takeover can wait for lease expiry. In the failover window,
clients may receive unavailable or unknown-outcome errors; reconcile uncertain
writes as described above. Remote clients can discover the successor, while
local clients need its socket path.

Observe queue pressure, index progress, storage errors and warning events after
activation. EDB publishes no standby heartbeat-count metric whose value proves
that a build change is safe. Do not substitute aggregate telemetry or `/health`
success for activation and application checks.

### Compatibility Across Builds

Review [release notices](../00_start_here/06_release_notices.md), the
[change log](../00_start_here/05_change_log.md), and all relevant source changes
between the deployed and proposed revisions. Use release tags to identify
released revisions; a shared package version is not a compatibility declaration.

Backups contain native immutable objects. Their successful capture does not
establish that another build can read them. Keep the original compatible build
alongside retained backups and test restore before relying on it for recovery.
There is no historical list of EDB releases with guaranteed rolling or full-stop
upgrade procedures.

## Downgrading

Do not assume that replacing peers or transactors with an older build is a safe
downgrade. Older readers may reject newer formats or lack behavior required by
stored schema and programs. There is no old-format reader, downgrade converter
or log downgrade command.

If returning to an earlier deployment is necessary, use the separately preserved
compatible build and its verified data or backup in an isolated target. Test
that recovery procedure before the change. Restoring a prior point can discard
later business work; account for those writes explicitly rather than assuming
that a binary rollback preserves them. Current restore guards also reject
retreating or divergent activation over an existing target; follow the documented
[restore procedure](01_administration.md#restore-into-a-separately-selected-destination).
