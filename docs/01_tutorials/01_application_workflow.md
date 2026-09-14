# Run a separate native application

The supported `atomic` executable owns the transactor process. The
`application_workflow` example is an ordinary separate client: it receives a
logical database and local endpoint or authenticated remote discovery, submits application schema/data through
public APIs, and never provisions PostgreSQL or starts a writer.

Build both programs:

```sh
cargo build --locked --bin atomic --example application_workflow
```

Use a dedicated PostgreSQL installation and distinct administrative, writer and
peer credentials. `ATOMIC_POSTGRES_URL` contains the connection parameters and
owns the transport selection. TCP defaults to verified TLS; `sslmode=disable`
explicitly selects plaintext. TLS verifies
certificates and hostnames and optionally accepts one PEM trust root through
`ATOMIC_POSTGRES_TLS_ROOT`. Plaintext is for an explicitly chosen local/development
deployment. The example and CLI share `postgres_config_from_env()`.

Optional `ATOMIC_CONNECT_TIMEOUT_MS`, `ATOMIC_STATEMENT_TIMEOUT_MS` and
`ATOMIC_LOCK_TIMEOUT_MS` select positive per-connection/statement/lock bounds.
Omitting them retains the existing policy. These are not a single end-to-end
deadline; the example separately uses a 20-second transaction/query wait budget.
See `atomic --help` and [transport policy](../08_operations/00_deployment.md#transport-and-failure-policy)
for the other existing I/O settings and their scope. Diagnostics omit connection
strings, subject values and detailed transaction errors.

## Explicit setup and runtime

Create dedicated PostgreSQL LOGIN roles through your normal administration,
then run the following with the object-owning administrative connection in
`ATOMIC_POSTGRES_URL`. Roles must satisfy the
[runtime credential requirements](../08_operations/00_deployment.md#provisioning-and-current-storage).

```sh
# For this local example, include sslmode=disable in ATOMIC_POSTGRES_URL.
target/debug/atomic install --writer-role atomic_writer --peer-role atomic_peer
target/debug/atomic create --database application-demo
target/debug/atomic status --database application-demo
```

Installation and runtime grants are separate committed actions. `INSTALLED`
confirms the two-table storage installation even if subsequent role validation
or grants fail. Correct the roles and rerun the same command. Runtime roles are trusted clients
of this shared storage, not per-database SQL authorization; grants are additive
and do not audit inherited access.

The sample reserves IDs 1000–1011 (including its named partition entity) and versioned request keys within
this dedicated logical database. Use a fresh logical database for the sample. Installation and creation are
explicit administrative actions; runtime processes do neither. Creation supplies
initial indexes. Consolidation is explicit administrative maintenance/recovery,
not a startup fallback for damaged read indexes.

Create a private endpoint directory and keep its path for the next terminal:

```sh
endpoint_dir=$(mktemp -d /tmp/atomic-application.XXXXXX)
```

With the writer connection in `ATOMIC_POSTGRES_URL`, run:

```sh
target/debug/atomic transactor --database application-demo --index-threshold-bytes 1 --endpoint "$endpoint_dir/transactor.sock"
```

The one-byte novelty threshold is an explicit small-fixture policy that exercises
background indexing promptly. Fulltext also merges admitted recent facts before
indexing; the threshold is not needed to make those facts visible and is not a
recommended production setting.
Wait for the `READY` line. In another terminal, set `ATOMIC_POSTGRES_URL` to the
peer connection, select the explicit transport policy, set `endpoint_dir` to
that same private directory, and run:

```sh
target/debug/examples/application_workflow --database application-demo --endpoint "$endpoint_dir/transactor.sock"
```

The local endpoint is for processes running as the same OS user. PostgreSQL
credentials remain distinct. Stop the transactor with Ctrl-C or SIGTERM and
restart it using the same database, writer configuration and endpoint path.
Rerun the application: its fixed request keys resolve the exact prior schema,
seed and update receipts. The second run reports `seed_replayed=true` and
`update_replayed=true`. It does not silently repeat changes under new keys.

## Remote applications

Remote transaction submission uses verified TLS (at least TLS 1.2) and a shared
32-byte bearer token. PostgreSQL credentials still authorize each peer's direct
storage reads; possessing the transaction token does not grant storage access.
Use your normal certificate provisioning and private secret distribution. The
server needs `ATOMIC_REMOTE_TLS_CERT` (PEM certificate chain),
`ATOMIC_REMOTE_TLS_KEY` (PKCS8 PEM private key), and `ATOMIC_REMOTE_TOKEN_FILE`
(64 hexadecimal digits). Key/token files must be regular, owned by the process
user and inaccessible to group/other users, typically mode 0600. Secrets are not
command-line arguments and are omitted from diagnostics.

With writer PostgreSQL credentials and those environment variables configured:

```sh
target/debug/atomic transactor --database application-demo --listen 0.0.0.0:7443 --advertise 192.0.2.10:7443 --tls-server-name atomic.example.com --index-threshold-bytes 1
```

Replace the example address/name with a reachable numeric unicast address and
the certificate's DNS name. The listen address may be wildcard; the advertised
address may not. The small indexing threshold has the same fixture-only purpose
as in the local example above. Do not supply `--endpoint` for a remote listener.
The writer publishes its endpoint through a guarded opaque reference, bound to
the database route/lineage, current fenced lease and a new listener instance. A peer does not accept a
stale endpoint merely because its address is reused.

Writer ownership is a Rust lease/epoch protocol over conditional references.
A successor claims a new epoch; every fresh publication checks the captured root,
lease and GC protection. A stale writer cannot commit merely because its work
started before takeover. Lease duration is not an application callback timeout;
use transaction and I/O controls separately. Receipt lookup can resolve an
already committed request without reassessing it or reviving writer authority.

In the application environment, set peer PostgreSQL credentials and the same
`ATOMIC_REMOTE_TOKEN_FILE`. System certificate roots are used; optionally set
`ATOMIC_REMOTE_TLS_ROOT` to a public PEM trust anchor. Hostname verification
cannot be disabled. Then run:

```sh
target/debug/examples/application_workflow --database application-demo --remote
```

The application keeps a `Connection::remote_writer(postgres, tls)` handle. It
captures the connection's stable database identity and caches a verified route;
renaming or reusing a public name cannot redirect it. When a cached endpoint is
unavailable or stale **before request transmission**, it rediscovers and tries
the successor once. Construction and `cached_endpoint()` do no I/O; `refresh`
explicitly discovers. Cloned handles share the route, without holding its lock
during network work.

An unknown transaction outcome is returned unchanged, not automatically replayed.
Retry explicitly with identical content and key, never a newly generated key.
The lower-level `Connection::discover_remote_writer`, `transact_remote`, and
`transact_remote_with_hints` remain supported. A confirmed commit remains confirmed
if opening its local report subsequently fails (`CommittedTransaction::report`).
Discovery consumes the submission time budget and tightens configured driver
limits to its remainder; this is not a preemptive bound on DNS, authentication,
SQL or report construction. Use the async facade on executor threads.

Transport admission, frame and timeout limits are configurable through
`RemoteTransportConfig` (the executable exposes its common transaction limits in
`--help`). Transport deadlines cover TLS handshake and response/acknowledgment;
PostgreSQL discovery and local report construction have their separate configured
I/O policies. Valid optional hints above the configured admission limit are
ignored; malformed framing is rejected. Hints never form part of request identity.

For transaction reactions and durable consumer checkpoints, see
[change consumers](../07_peer_api/03_change_consumers.md). For provisioning, backup/verification,
restore, inspection, GC and search repair, see [administration](../08_operations/01_administration.md).

## Persistent standby and health

Add `--mode auto` to each transactor's normal command to wait for authority and
take over when the active lease is released or expires. Each process needs its
own local socket or TCP listen address; do not bind two contenders to the same
address. Configure the same database, deployment defaults and verified TLS policy
on contenders. Indexing, excision admission, transaction defaults and transport
settings survive the handoff. The default `--mode active` retains fail-fast
startup when another writer owns authority.

`STANDBY` is not write readiness. Only `READY` means writer authority and its
configured transaction listener are active. `--standby-poll-ms` defaults to 250;
SIGTERM interrupts the wait rather than sleeping out a long poll interval.
In-flight PostgreSQL startup/cleanup still obeys its synchronous I/O policy.
Once active, loss of service/listener availability exits unsuccessfully for the
supervisor to restart; automatic election does not conceal an unhealthy writer.
Keep the same stable configuration when supervising its restart.

Optionally add `--health-listen 127.0.0.1:8081` (numeric `IP:PORT`). This is a
plaintext, status-only listener, not a query or administration gateway. Restrict
it to a trusted probe network; use your ingress for TLS if externally exposed.
`GET /health` returns 200 while starting, waiting or active; `GET /ready` returns 200
only while active and 503 otherwise. Both return small JSON with `live`, `ready`
and `state`; `HEAD` is supported. Availability is sampled by the lifecycle loop,
not a linearizable promise that the next transaction succeeds. No database calls,
names, credentials or transaction data are exposed by probes. Two workers, eight
queued sockets, 1 KiB request buffers and short cumulative read deadlines bound
listener resources; overload closes connections.

Run foreground processes under your OS/container supervisor: its process identity,
signal handling and restart policy replace hand-maintained PID files. The stock
binary intentionally serves one logical database per process, giving independent
failure, configuration and capacity boundaries. Applications can compose multiple
`Connection`s, or embed multiple `TransactionService`s when they deliberately want
shared process fate; they must own separate listeners and resource budgets.
This is native process composition, not a missing multi-database storage engine.

For Rust Future/Stream methods, bounded worker admission, exact read capture and
drop/cancellation semantics, see [async clients](../07_peer_api/01_async_client.md). The evolving
application runs a current-value query and a one-row-at-a-time old-value stream
on a single-thread Tokio runtime, reporting `ASYNC_READ_OK`; native database and
connection lifecycle remain outside its executor task.

## What the application demonstrates

`calculation(&DatabaseValue)` queries projects and follows each project's owner
reference through the lazy Entity API. It operates on one captured seed snapshot.
The same function runs against a complete in-memory database constructed with
`Database::new`, `with` and `database_value`, including schema and references.
The fixture uses the existing semantic engine; it is not a replacement store.

The example changes one project's hours, reruns the calculation on the retained
value, checks an as-of view and assertion/retraction history, and independently
reopens the native connection. Old total hours remain 8; the current total is 12.
Native values remain backed by PostgreSQL: a local `db()` capture is not an
offline-query guarantee. A speculative `with` result is also not a guarantee
that a later transaction will commit against a changed live database.

A lost response after attempted delivery is an unknown outcome: retry identical
content under the same request key. A confirmed commit whose local report fails
to open remains committed. The example exits on a failure; rerunning these same
requests performs exact reconciliation through their durable receipts.

The executable prints `PLANNING_OK`. It compares two pure branches,
discards them without writes, then exercises a real intervening transaction.
The stale protected plan rejects; the application replans against the intervening
receipt and submits its logical intent (lookup refs and tempids), not speculative
allocated IDs or a datom diff. The committed ID demonstrably differs from the
first preview. A final normal transaction restores the example's current facts;
history still records the intervention, selected plan and restoration. All three
use fixed request keys and reconcile on restart.

`PARTITIONS_OK` installs a named tenant partition and transacts a tenant plus its
nested component, checking both allocations share that partition. UUIDv7 and
SQUUID values survive exact receipt/reference round-trips. Fixed request keys and
the fixture's unique tenant identity preserve the same UUID intent on restart;
a general application should persist its request in an outbox before submission.
See [partitions](../04_transactions/02_partitions.md).

`FULLTEXT_OK` installs a fulltext article attribute and two articles, then joins
native fulltext candidates against the structured public-status fact. Only the
public article remains. It demonstrates a locally encoded/decoded native query
program without granting the read-only application program-deployment authority.
Index/search preparation publishes one coherent descriptor. The sample
synchronizes physical indexing to the article basis before querying and
uses a finite deadline. Search includes that value's admitted recent facts;
an older captured value does not silently advance.
See [fulltext.md](../05_query_and_pull/03_fulltext.md) for search grammar, limits and consistency caveats.

The calculation/statistics use the exact update receipt,
not a newer value returned by sync after a rerun. `PLANNING_OK` reports its own
additional time and foreground SQL after the baseline counters. Its generated
hints are generated locally; the sample does not transport them. The remote SDK
provides the separate versioned hint channel described below.

`APPLICATION_OK` reports the semantic checks. One explicit `QUERY_WARMUP`
calculation reports its own time and SQL before the 20-calculation
loop. The configured eight-entry cache is used throughout. `QUERY_SQL` measures that
loop directly with a child `OperationContext`; this resident fixture asserts
zero SQL calls/errors/cell bytes after warmup, not an offline-database guarantee.
`BASELINE` separately reports `stats_scan_us` and the history-datom count, plus
elapsed time, Linux RSS, cursor-node counters and cache accounting. Statistics
scan history; their cost is outside the calculation loop.

`APPLICATION_SQL` includes foreground opening, receipt reads, queries, reopening
and statistics. `PROCESS_SQL` additionally includes independent background work,
with `PeerObservation` classified separately. These are snapshots before teardown,
not process-wide deltas or measurements of the separate transactor process.
Driver calls and returned binary cell bytes are not SQL-statement counts,
network round trips, disk I/O or total memory; cursor counters remain a narrower
node-read subset. No timing threshold determines success or proves scale.

`DIAGNOSTICS_OK` checks named per-operation index attribution without changing
the captured database value. See [observability](../08_operations/03_observability.md),
[query diagnostics](../05_query_and_pull/04_diagnostics.md), and
[transaction diagnostics](../04_transactions/03_diagnostics.md) for optional typed/EDN
reports and their cost/interpretation. Normal API and CLI results remain unchanged
when detailed diagnostics are disabled.

For an attached writer, a caller's `OperationContext` follows submitted work
across the service queue. Its `phases` include transaction expansion, assessment,
encoding/object preparation, guarded publication and report construction;
these wall durations are separate from SQL-call elapsed time. Outer transaction
time is inclusive, and phase invocations are not transaction counts.
Retries and partial failures record only executed work;
unknown-outcome reconciliation can add measurements after the initial response.
Metric callbacks run only when the caller explicitly calls `publish()`.

## Captured values, references and query programs

An enabled transaction report queue follows every transaction observed while
the peer remains connected, including transactions from other writers. A live
peer can catch up across local excision rewrites without losing the original
before/after values, transaction datoms or tempids. Excision retains a generation handoff for the configured GC grace, independent
of connected readers. A lagging observer must catch up within that grace.
A held database value or drained report creates no storage retention pin. Remove unused queues and drop unused peers/connections.

A same-route restore can import an excision generation whose original reports
never existed on that route. If a required handoff is absent, an enabled queue
returns `peer/report-history-unavailable` without advancing its value or
silently omitting reports. Disconnect and reopen against the restored state;
this is not reconstruction of erased reports from a sanitized backup.

`DatabaseValue::snapshot_key()` returns a cheap comparable/hashable committed
view key: database lineage, excision generation, commit identity and temporal/
history modifiers. Constructing or cloning it performs no SQL or fact scan.
Physical index roots and cache placement are deliberately absent. It identifies
a supported logical view, not arbitrary query results or the retained history
policy: queries, inputs, rules, extensions and `noHistory` retention still matter.
An as-of view on a newer basis is not an exact older database reopening.

`snapshot_reference()` adds a versioned encoded handoff containing the stable
database route and its exact retained index witness. Decode it with
`SnapshotReference::decode`, then use `Peer::reopen_snapshot`,
`Connection::reopen_snapshot`, or `reference.open(&config, entries, bytes)`.
Reopening authenticates current publication authority, lineage, generation,
canonical log/metadata coordinates and the authorized retained index; it does not advance the peer or recover the latest
database first. References are not credentials and do not hold retention pins.
If GC collected the witness, reopening fails instead of substituting another
tree. This matters especially when `noHistory` consolidation changes retained
historical exposure while preserving the logical commit key. Existing held values
remain subject to the configured storage retention. New reopening of pre-excision references is
rejected, even if old artifacts remain; this does not erase already held copies.
Cache entry/byte arguments bound the node cache, not the memory or time needed
to replay an exact retained transaction tail. Opening a reference can perform
storage I/O and is not itself an untrusted-work admission budget.

The executable example can save its captured seed reference at basis2 while
running the normal local or remote workflow:

```sh
target/debug/examples/application_workflow --database application-demo --remote --reference-out /private/path/seed.reference
target/debug/examples/application_workflow --database application-demo --reference-in /private/path/seed.reference
```

Output uses a new private file and refuses overwrite. The second command needs
only authorized PostgreSQL access, not a running transactor or transaction token.
It verifies the original key and seed calculation even after the normal workflow
has advanced data and schema. The file neither grants authorization
nor preserves retention. GC or excision can make a later reopening unavailable;
the program fails explicitly instead of choosing the latest database.

Speculative/in-memory values and opaque predicate filters explicitly reject
portable committed keys/references rather than inventing equality for closures
or uncommitted branches. They remain usable ordinary database values. The common
application demonstrates pure branching and exact reference round-trip; the
[query sources/reuse guide](../05_query_and_pull/00_queries.md) explains native database/tuple/log joins;
the [persisted program API](../04_transactions/01_persisted_programs.md) exposes richer native query authoring with
current canonical codecs and shared resource controls.

### Protected program deployment

An administrator can call
`PostgresOperator::deploy_program(&Program) -> ProgramHash`. Deployment checks
the fixed-dependency closure and stores a temporary staging root that expires
according to the collection cutoff. Dependencies must already exist; callers
receive a hash and have no deployment handle to release.

`install_program(client, request_key, ident, &program, &dependencies, timeout)`
stages the supplied closure and transacts the binding. An exact retry resolves
its acknowledged receipt before requiring deployment dependencies again. Once a
committed graph references the program, ordinary graph ownership retains it.
Unbound staging roots expire under the configured retention policy.

The read-only application example does not need this administrative capability
for its local encode/decode demonstration.

## Optional tuning

[Transaction hints](../09_optional/01_transaction_hints.md) and the
[local SSD cache](../09_optional/00_local_cache.md) are opt-in policies. They do not
change request identity, authorization or the storage retention contract.
