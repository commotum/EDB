# Transactor Reference

## Running the Transactor

An EDB transactor performs ACID transactions for a logical database. You can
launch a transactor with the `edb` executable from your EDB checkout:

```sh
cargo build --locked --bin edb
```

Once your storage service is configured, you can start the transactor locally
by running `target/debug/edb transactor`. It takes command-line options and
PostgreSQL connection settings from the environment. The stock executable
serves one logical database per process; run separate processes for independent
database configuration, capacity and failure boundaries. Rust applications can
embed multiple `TransactionService`s when they deliberately want shared process
fate, with separate listeners and resource budgets.

### Preparing storage

Provision the PostgreSQL database, empty schema and distinct administrative,
writer and peer LOGIN roles as described in [setup](../00_start_here/01_setup.md).
With the administrative connection in `EDB_POSTGRES_URL`, install storage and
create a logical database:

```sh
target/debug/edb install --writer-role edb_writer --peer-role edb_peer
target/debug/edb create --database transactor-demo
```

Installation and grants are separate committed operations. If grants fail after
`INSTALLED`, correct the roles and rerun installation. The transactor validates
existing storage; it does not install tables or create databases during startup.
These commands use PostgreSQL in development as well as production. For storage
permissions and format requirements, see [storage services](06_storage_services.md).

Set `EDB_POSTGRES_URL` to the writer connection, selecting the same PostgreSQL
database and schema. The connection string selects transport: TCP defaults to
verified TLS; `sslmode=disable` explicitly selects plaintext. Unix sockets use
local transport. Keep credentials in the process environment or your secret
management system, rather than command-line arguments.

Create a private endpoint directory, then start the transactor:

```sh
endpoint_dir=$(mktemp -d /tmp/edb-transactor.XXXXXX)
target/debug/edb transactor --database transactor-demo \
  --endpoint "$endpoint_dir/transactor.sock"
```

The directory must already exist, be owned by the process user, and have
owner-only access. The socket serves applications running as that same OS user.
EDB maintains an ownership lock beside the socket; do not remove or replace the
lock while the endpoint is in use. On restart EDB can reclaim a recognized stale
socket, but refuses unsafe or unrecognized paths.

If console output includes a line of the following form, writer activation and
listener startup have completed:

```text
READY database="<storage-id>" endpoint="/tmp/edb-transactor.ABC123/transactor.sock" lineage=<database-lineage>
```

The `database` field reports the stable storage ID, not the public catalog name
`transactor-demo`. Database names resolve once at service startup; a rename or
subsequent reuse of a name cannot redirect an existing service. `edb status`
reports catalog state, not proof that a transactor is running or write-ready.

Run the foreground process under your OS or container supervisor. Its process
identity, restart policy and signal handling provide process management; the
stock executable does not write a PID file. EDB is a native Rust process. Size
its CPU, memory and I/O allocation using the resource controls below and measured
workload behavior, rather than JVM heap or garbage-collector flags.

### Health Check Endpoint

A web endpoint is not required to acquire or renew writer authority. However,
some deployment tools require every process to provide a health check endpoint.
If you need one, add a numeric IP address and port to the transactor command:

```sh
target/debug/edb transactor --database transactor-demo \
  --endpoint "$endpoint_dir/transactor.sock" \
  --health-listen 127.0.0.1:8081
```

When enabled, the status-only listener supports `GET` and `HEAD`:

```sh
curl -i http://127.0.0.1:8081/health
curl -i http://127.0.0.1:8081/ready
```

| Endpoint | HTTP 200 | HTTP 503 |
| --- | --- | --- |
| `/health` | Starting, standby or active | Stopping or failed |
| `/ready` | Active writer and transaction listener | Any other lifecycle state |

Both return small JSON objects with `live`, `ready` and `state`, for example
`{"live":true,"ready":true,"state":"active"}`. Health responses expose no database
names, credentials or transaction data and perform no database calls. Availability
is sampled by the lifecycle loop; a successful probe does not guarantee that
the next transaction will succeed.

The health listener uses plaintext HTTP. Bind it to a trusted probe network and
use an ingress for TLS if externally exposed. Two workers, eight queued sockets,
1 KiB request buffers and short cumulative read deadlines bound its resources;
overload closes connections. This listener provides neither a query interface
nor an administrative API.

### TLS certificates and trust roots

There are two independently configured connections: the transactor's connection
to PostgreSQL, and a remote application's connection to the transactor.

For PostgreSQL, `EDB_POSTGRES_TLS_ROOT` optionally supplies one PEM trust root in
addition to system roots. Certificate and hostname verification remain required
for TLS. All service-owned writer, indexer and reader connections share the
same typed connection and I/O policy. `EDB_POSTGRES_TRANSPORT` is not accepted;
configure transport in `EDB_POSTGRES_URL`.

For remote transaction submission, configure the server with:

| Environment variable | Contents |
| --- | --- |
| `EDB_REMOTE_TLS_CERT` | Path to a PEM certificate chain |
| `EDB_REMOTE_TLS_KEY` | Path to a private PKCS8 PEM key |
| `EDB_REMOTE_TOKEN_FILE` | Path to a shared 32-byte bearer token encoded as 64 hexadecimal digits |

Key and token files must be regular files owned by the process user and
inaccessible to group and other users, typically mode `0600`. Distribute them
through your normal private secret provisioning. Remote clients use the same
token file and optionally `EDB_REMOTE_TLS_ROOT`, a PEM trust anchor. Server-name
verification is mandatory. The transaction listener uses verified TLS 1.2 or
newer and has no plaintext TCP mode.

To start a remote listener, replace the example address and name below with a
reachable numeric unicast address and the DNS name in your certificate:

```sh
target/debug/edb transactor --database transactor-demo \
  --listen 0.0.0.0:7443 --advertise 192.0.2.10:7443 \
  --tls-server-name edb.example.com
```

Choose exactly one of `--endpoint` or `--listen`. The bind address may be
wildcard; the advertised address may not. The remote `READY` line reports
`transport=tls authenticated_discovery=true` instead of a local endpoint path.

## Connecting to the Transactor

After the transactor is running, you can test the configuration using the EDB
command-line interface or a native application. Applications need the logical
database name and their own peer PostgreSQL credentials. For local submission,
they also need the socket path. With peer credentials in `EDB_POSTGRES_URL`,
submit a small transaction using the built-in `:db/doc` attribute:

```sh
target/debug/edb transact --database transactor-demo \
  --endpoint "$endpoint_dir/transactor.sock" \
  --request-key transactor-reference-hello --file - <<'EDN'
[[:db/txInstant :db.source/system]
 {:db/id "hello" :db/source :db.source/system :db/doc "Hello world"}]
EDN
```

This writes one entity to the database, explicitly attributing its `:db/doc`
assertion and the transaction instant to `:db.source/system`.
Keep the same key and content when
retrying this example: EDB returns its exact retained receipt rather than
repeating the transaction. The result includes `:edb/committed true`, basis,
transaction hash and replay status. See [connecting to a database](../01_tutorials/05_connect_to_a_database.md)
for the equivalent `Connection::transact_socket` example and report details.

Peers read directly from PostgreSQL. Reads do not require a running transactor,
although cold reads still need available storage and retained immutable objects.
A remote bearer token permits transaction submission; it does not grant the
peer PostgreSQL access needed to read database values.

For remote applications, the writer publishes its advertised address through a
guarded storage reference. Discovery authenticates that address against the
stable database route, lineage, current fenced lease and listener instance.
Configure `Connection::remote_writer(postgres, tls)` or the lower-level remote
APIs as described in [remote applications](../01_tutorials/01_application_workflow.md#remote-applications).
There is no separate alternate-host fallback: publish the address clients can
actually reach. A cached route that fails before request transmission can be
rediscovered and tried once against its successor.

A timeout or lost response after attempted transmission may have an unknown
outcome. Reconcile explicitly with identical request content and key; do not
use a new key to resolve uncertainty. A confirmed commit remains committed even
if constructing the caller's local report fails. Transport timeouts are not
commit or rollback guarantees.

## Writer authority and standby

The default `--mode active` fails startup when another writer holds authority.
To wait for authority and take over when it is released or expires, add
`--mode auto` to each contender's normal command:

```sh
target/debug/edb transactor --database transactor-demo \
  --endpoint "$endpoint_dir/standby.sock" --mode auto \
  --standby-poll-ms 250
```

Each process needs its own socket path or TCP listen address. Configure the same
database, resource policies, transaction defaults and verified TLS settings on
contenders. `STANDBY` reports a waiting contender, not write readiness. Only
`READY` reports acquired writer authority and an active transaction listener.
Remote clients can discover the new published endpoint; local clients must
select the socket of the process that became active.

Writer ownership is a Rust lease and epoch protocol over guarded storage
references. A successor claims a new epoch. Fresh publications check the
captured root, lease and GC protection; a stale writer cannot commit work simply
because it began before takeover. Lease timestamps determine takeover
eligibility; guarded root and lease revisions provide fencing even under clock
skew. Lease duration is not a deadline for arbitrary transaction callbacks.

The stock defaults are a 5,000 ms lease, 1,000 ms renewal interval and 250 ms
standby poll interval. Renewal must be positive and less than the lease duration.
Choose settings together with your I/O policy and measured deployment behavior;
these intervals alone do not establish a failover-time guarantee. PostgreSQL
availability and replication remain separate deployment responsibilities.

Once active, loss of the service or a configured listener causes an unsuccessful
exit for the supervisor to restart. The process does not silently return to
standby after losing availability. Rust embedders use
`TransactionStandby::start_with_options`; its successful activation transfers
one `TransactionService` to the caller. After that handoff the caller owns the
service and listener lifecycle.

## Configuring options

Once you have verified that your transactor is working with your storage
configuration, configure and test additional options. `edb --help` lists the
stock settings. Configuration is read at process startup; restart the process
to apply changes.

### Admission and memory

| Option | Default | Meaning |
| --- | --- | --- |
| `--queue-capacity` | 64 | Bounded service admission queue |
| `--max-in-flight` | 4 | Transport concurrency; supported range 1–256 |
| `--request-timeout-ms` | 30,000 | Transport request/response deadline |
| `--max-frame-bytes` | 67,108,912 | Transport frame bound; range 48 through this default |
| `--tree-cache-entries` | 4,096 | Writer tree-cache entry limit |
| `--tree-cache-bytes` | 67,108,864 | Writer tree-cache byte limit |
| `--index-threshold-bytes` | 33,554,432 | Recent-memory accounting threshold for background indexing |
| `--index-max-bytes` | 536,870,912 | Recent-memory accounting threshold for back-pressure |
| `--index-workers` | 1 | Index edit-preparation lanes; range 1–8 |
| `--hint-workers` | 1 | Optional transaction-hint lanes; range 0–8; zero disables |
| `--default-partition` | `:db.part/user` | Placement policy for new ordinary entities |
| `--holder` | `edb-<process-id>` | Lease holder label |

Numeric limits must be positive except for `--hint-workers`. The index threshold
must not exceed the index maximum. Install a named partition before selecting it
as the default; this setting does not change a stored exact retry's result.

Recent-memory bytes are a deterministic native accounting measure, not allocator
RSS. Above the maximum the writer parks ordinary dequeue, continues lease renewal
and prioritizes indexing. A genuinely full admission queue returns Busy. Already
committed retries that reach the worker can still resolve without extending the
log. Higher index parallelism can increase temporary memory; it does not create
parallel SQL publication or a whole-process memory ceiling.

Rust embedders can set `TransactionServiceConfig.capacity_limits`. Defaults
include 100,000 transaction operations, 64 MiB transaction bytes, 1,000,000
transaction-read datoms and 64 MiB transaction-read bytes, plus program limits.
The CLI exposes the writer cache settings from that structure; it does not
provide a flag for every library limit. See [transaction processing](../04_transactions/06_processing_transactions.md)
for assessment and admission semantics.

### Storage I/O and caching

Optional environment settings include `EDB_CONNECT_TIMEOUT_MS`,
`EDB_STATEMENT_TIMEOUT_MS`, `EDB_LOCK_TIMEOUT_MS`, `EDB_TCP_USER_TIMEOUT_MS`,
`EDB_KEEPALIVES`, `EDB_KEEPALIVES_IDLE_SECONDS`,
`EDB_KEEPALIVES_INTERVAL_SECONDS` and `EDB_KEEPALIVES_RETRIES`. Numeric values
are positive; `EDB_KEEPALIVES` accepts `true` or `false`. Omitted settings preserve
the driver/server policy rather than introducing a universal production timeout.

Statement and lock deadlines bound individual SQL work. Connect timeout bounds
individual socket/address attempts, not DNS, TLS/authentication or all addresses
combined. These controls do not preempt arbitrary Rust callbacks. See
[transport and failure policy](00_deployment.md#transport-and-failure-policy).

To enable the optional local SSD cache, set `EDB_SSD_CACHE_DIR` to a dedicated
private directory. `EDB_SSD_CACHE_ENTRIES` and `EDB_SSD_CACHE_BYTES` bound its
shared directory. All processes sharing it must use the same limits. On restart,
service connections use that policy. Cached objects remain subject to storage
authorization and retention; cache files are not an offline-startup guarantee.
See [local SSD cache](../09_optional/00_local_cache.md) for defaults, failure
behavior and cleanup. EDB does not expose a Memcached configuration option.

### Maintenance and operational events

The service prepares indexes in the background and schedules committed excision
requests. `--excision-max-bytes` defaults to 512 MiB of accounted admission bytes;
`--excision-log-batch` defaults to 256 replay transactions, with range 1–4096.
These are admission and cooperative work controls, not measured RSS or a total
rewrite-time ceiling. An admitted automatic excision parks fresh writes while
lease renewal, receipt resolution and independent reads continue.

Admission failure leaves the request pending and ordinary writes available.
`TRANSACTOR_EXCISION_FAILURE` reports a redacted category/code. Increase the
allowance deliberately and restart to resume, or use authorized administrative
maintenance. See [excision](00_deployment.md#excision) and
[maintenance controls](04_maintenance_controls.md) for completion, resource and
retention semantics.

Consolidation, explicit index recovery, backup, restore and garbage collection
have separate [administrative commands](01_administration.md). Ordinary startup
does not silently rebuild damaged read indexes. Administrative excision needs
writer authority and reports Busy while a service owns the lease.

Add `--telemetry-ms 10000` to publish bounded JSON operational events to stdout.
The output also contains ordinary startup and lifecycle lines. Drain stdout in
your supervisor and use the [observability guide](03_observability.md) to interpret
counters and delivery limits. No fixed throughput or memory sizing follows from
the default settings; measure foreground latency, index progress and storage
work on your deployment before tuning.

## Stopping and restarting

Send SIGINT (Ctrl+C) or SIGTERM for orderly shutdown. The lifecycle stops
transport admission and joins its bounded in-flight transport workers, then
stops the service, rejects remaining queued work, joins background workers and
attempts to release writer authority. Restart with the same database and intended
endpoint configuration. On ordinary shutdown EDB removes its own local socket
but retains the caller's directory and ownership lock.

Stopping a standby wakes idle polling. In-flight synchronous PostgreSQL
activation or cleanup still follows its configured I/O policy. Shutdown is not
a preemptive deadline for SQL or arbitrary application work. A failed release
can leave takeover waiting for lease expiry; clients must still reconcile
unknown outcomes using the same request keys. If a telemetry sink remains
stalled beyond its shutdown wait, the CLI can omit `STOPPED` to avoid blocking
behind that sink.

## Troubleshooting connections

If the transactor cannot connect to PostgreSQL, verify host, port, database,
schema and credentials with your normal PostgreSQL tools under the same intended
transport policy. For TLS, check certificate validity, hostname matching and
the trust root supplied through `EDB_POSTGRES_TLS_ROOT`. A private root supplied
alongside plaintext configuration is rejected. Successful storage access alone
does not establish transaction-listener readiness.

For local endpoint errors, check directory ownership and permissions, the
selected process user, and whether another process owns the endpoint. Preserve
unrecognized lock or socket artifacts for inspection rather than deleting them
to bypass ownership checks.

For remote errors, verify the advertised address is reachable from clients,
`--tls-server-name` matches the certificate, the client trusts its issuer, and
both sides use the same token. `config/remote-private-file` indicates unacceptable
secret-file ownership or permissions; `config/remote-tls-identity` indicates an
unusable certificate/key identity. Keep credentials and subject data out of logs.

A `storage/writer-active` startup error means another writer holds the lease;
use a distinct endpoint and `--mode auto` for an intended standby.
`cli/transactor-unavailable` means an activated process lost availability and
requires supervisor restart. Investigate any accompanying redacted index or
excision failure code. A failing `/ready` with a healthy `/health` is expected
while a contender waits; it does not by itself mean storage is damaged.
