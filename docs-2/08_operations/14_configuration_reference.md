# Configuration Reference

EDB configuration is supplied through command-line options, environment
variables named `EDB_SOME_SETTING`, and explicit Rust configuration values.
The stock `edb` executable does not read a Java properties file or accept JVM
`-D` options. Transactor options use hyphen-delimited lowercase names, such as
`--index-threshold-bytes`.

Memory settings on the command line and in the environment are integer bytes.
They do not accept `k`, `m`, `g`, or `MiB` suffixes. For example, specify
`33554432` for 32 MiB. Numeric transactor options are positive integers except
`--hint-workers`, which also accepts zero. Unknown, duplicate, and misplaced
command-line options are errors.

## Reading Configuration

Each setting below describes its use, units and default. These terms describe
whether a value must be supplied:

| Setting status | Meaning |
| --- | --- |
| Required | No reasonable default; you must set this value for the indicated operation |
| Defaulted | The executable or Rust configuration supplies the listed value |
| Generated | The executable constructs a value at startup |
| Optional | The setting is not required; the indicated feature may remain disabled |

The transactor reads its arguments and constructs its configuration at startup.
`postgres_config_from_env()` reads the current environment when called and
returns an owned `PostgresConnectionConfig`. Rust callers pass that value to
configured connection APIs; constructing a connection directly from a connection
string does not implicitly read the `EDB_*` environment. Existing connections
and services do not reload changed environment variables or credential files.

For a complete launch example, see the [transactor reference](07_transactor_reference.md).
Run `target/debug/edb --help` for the executable's accepted commands.

### PostgreSQL Settings

Peers, transactors and online administrative commands use the same connection
configuration. Offline backup readers and data-only queries do not require
PostgreSQL configuration.

| Environment variable | Use | Units / format | Default / status |
| --- | --- | --- | --- |
| `EDB_POSTGRES_URL` | PostgreSQL connection parameters, including database, credentials and schema selection | Connection string | Required for live PostgreSQL commands |
| `EDB_POSTGRES_TLS_ROOT` | Additional trust root alongside platform roots | PEM file path | Optional; requires TLS |
| `EDB_CONNECT_TIMEOUT_MS` | Timeout for each socket/address connection attempt | Milliseconds | Optional; preserve connection-string/driver policy |
| `EDB_STATEMENT_TIMEOUT_MS` | Server-side timeout for each SQL statement | Milliseconds | Optional; preserve server policy |
| `EDB_LOCK_TIMEOUT_MS` | Server-side timeout for each lock wait | Milliseconds | Optional; preserve server policy |
| `EDB_TCP_USER_TIMEOUT_MS` | OS-dependent limit on unacknowledged TCP data | Milliseconds | Optional; preserve connection-string/driver policy |
| `EDB_KEEPALIVES` | Enable or disable TCP keepalives | Exactly `true` or `false` | Optional; preserve connection-string/driver policy |
| `EDB_KEEPALIVES_IDLE_SECONDS` | Idle interval before keepalive probes | Seconds | Optional; preserve connection-string/driver policy |
| `EDB_KEEPALIVES_INTERVAL_SECONDS` | Interval between keepalive probes | Seconds | Optional; preserve connection-string/driver policy |
| `EDB_KEEPALIVES_RETRIES` | Maximum unanswered probes | Count | Optional; preserve connection-string/driver policy |

All numeric environment values in this table must be positive. Statement,
lock and TCP-user timeouts are limited to `2147483647` milliseconds; keepalive
idle/interval values to `2147483647` seconds; retries to `2147483647`.
The OS may impose tighter constraints. TCP settings have no effect on Unix
sockets. Connection timeout does not cover DNS, TLS/authentication/startup or
the whole multi-host operation. SQL timeouts do not cover arbitrary Rust work
or a complete application transaction.

TCP defaults to verified TLS. `sslmode=disable` in the connection string
explicitly selects plaintext; local Unix sockets need no TLS. The environment
reader rejects `EDB_POSTGRES_TRANSPORT`; select transport using `sslmode`.
Credentials remain in the connection configuration and are redacted from its
debug output. See [storage services](06_storage_services.md) for provisioning,
roles, schema selection and transport details.

Rust applications can use `PostgresConnectionConfig::with_io_policy` with a
`PostgresIoPolicy`. Its fields are optional; `None` preserves the existing
connection-string, driver or server policy. Explicit SQL timeouts override the
corresponding startup options, while the socket connection timeout only
tightens an existing connection-string cap. EDB adds no universal production
deadline when these values are omitted.

### Peer Settings

Peer cache settings are supplied when opening the reader. Transaction and
synchronization timeouts are supplied for the individual operation.

| Setting | Use | Units | Default / where to set |
| --- | --- | --- | --- |
| `cache_entries`, `cache_bytes` | Retained decoded tree-node cache limits | Entries, bytes | Explicit arguments to `Connection::connect_configured_with_cache_limits` and the corresponding `Peer` API |
| `cache_capacity` | Entry limit with a derived byte limit | Entries | Required argument to `Connection::connect_configured`; bytes are `cache_capacity * 512 KiB`, saturating at the platform limit |
| `BlockReadConfig.cache_entries` | Lower-level reader cache entry limit | Entries | 256 |
| `BlockReadConfig.cache_bytes` | Lower-level reader cache byte limit | Bytes | `67108864` (64 MiB) |
| `BlockReadConfig.max_recent_transactions`, `max_recent_bytes`, `max_recent_datoms` | Optional admission limits on already committed recent data | Transactions, bytes, datoms | Each is `usize::MAX` (unspecified) |
| `EDB_SSD_CACHE_DIR` | Private disposable local block-cache directory | Path | Optional; no SSD cache unless configured |
| `EDB_SSD_CACHE_ENTRIES` | Shared directory's entry limit | Entries | 4096 when SSD caching is enabled |
| `EDB_SSD_CACHE_BYTES` | Shared directory's physical block-file length limit, including framing | Bytes | `1073741824` (1 GiB) when enabled |
| `edb transact --timeout-ms` | Client transaction exchange deadline | Milliseconds | 30000 |
| `edb query --timeout-ms` | Query evaluation timeout | Milliseconds | 30000 |
| Rust transaction and synchronization timeout arguments | Bound the caller's wait for that operation | `Duration` | Supplied by the caller |

For example, choose independent cache budgets when opening an application
connection:

```rust
use edb_core::{Connection, PostgresConnectionConfig, SemanticError};

fn connect(config: PostgresConnectionConfig) -> Result<Connection, SemanticError> {
    Connection::connect_configured_with_cache_limits(
        config, "application", 4096, 64 * 1024 * 1024,
    )
}
```

Zero for either Rust decoded-cache limit disables retention. The stock data
commands open live readers with 128 entries and a derived 64 MiB byte budget;
they do not expose peer RAM-cache flags. Recent-data admission limits can reject
a capture but cannot shrink committed data or make the transactor index sooner.
See [memory and caching](12_memory_and_caching.md).

SSD entry and byte environment values must be positive and require
`EDB_SSD_CACHE_DIR`. The directory must already exist and be private; processes
sharing it must agree on limits. The same configuration applies to writer-side
readers. See the [local cache reference](../09_optional/00_local_cache.md) for
directory admission, retention and failure behavior.

A transaction timeout after admission can leave the outcome unknown. Retain
the original request key and intent for retry; a timeout does not establish
rollback. See [error handling](../07_peer_api/07_error_handling.md). Query limits
and Pull limits are operation-specific; see the
[EDN workflow](../01_tutorials/00_edn_workflow.md).

### Transactor Settings

The `edb transactor` command takes options directly. Supply exactly one of a
local socket endpoint and a TLS listener. There is no final properties-file
argument.

| Command-line option | Use | Units / format | Default / status |
| --- | --- | --- | --- |
| `--database` | Public catalog name resolved once at startup to a stable storage ID | String | Required |
| `--endpoint` | Same-user local transaction socket | Path in an existing private directory | Required for local mode |
| `--listen` | Remote transaction listener | Numeric `IP:port` | Required for remote mode |
| `--advertise` | Address published for remote discovery | Numeric `IP:port`; zero port adopts the bound port | Required with `--listen` |
| `--tls-server-name` | Certificate name published for remote discovery | String | Required with `--listen` |
| `--mode` | Fail on writer contention or wait as standby | `active` or `auto` | `active` |
| `--standby-poll-ms` | Contender polling interval in auto mode | Milliseconds | 250 |
| `--holder` | Lease-holder label | String | Generated as `edb-<process-id>` |
| `--default-partition` | Default placement for new ordinary entities | EDN keyword | `:db.part/user`; named partitions must be installed |
| `--lease-ms` | Writer lease duration | Milliseconds | 5000 |
| `--renew-ms` | Lease renewal interval | Milliseconds | 1000; must be less than lease duration |
| `--queue-capacity` | Bounded service admission queue | Requests | 64 |
| `--max-in-flight` | Concurrent transport exchanges | Count | 4; range 1–256 |
| `--request-timeout-ms` | Server request/response exchange deadline | Milliseconds | 30000; must fit the monotonic clock |
| `--max-frame-bytes` | Maximum transport frame size | Bytes | `67108912` (64 MiB + 48); range 48–67108912 |
| `--index-threshold-bytes` | Accounted recent size that schedules ordinary indexing when exceeded | Bytes | `33554432` (32 MiB) |
| `--index-max-bytes` | Accounted recent size that applies fresh-write back pressure when exceeded | Bytes | `536870912` (512 MiB); at least the threshold |
| `--index-workers` | Independent index-edit preparation lanes; SQL remains serial | Count | 1; range 1–8 |
| `--hint-workers` | Advisory transaction-hint lanes | Count | 1; range 0–8; zero disables |
| `--tree-cache-entries` | Writer decoded-node cache entry limit | Entries | 4096 |
| `--tree-cache-bytes` | Writer decoded-node cache byte limit | Bytes | `67108864` (64 MiB) |
| `--excision-max-bytes` | Excision maintenance allocation admission account | Bytes | `536870912` (512 MiB) |
| `--excision-log-batch` | Maximum replay transactions per excision step | Transactions | 256; range 1–4096 |
| `--health-listen` | Plaintext status-only `/health` and `/ready` listener | Numeric `IP:port` | Optional; disabled |
| `--telemetry-ms` | Opt into bounded JSON operational events on stdout at this interval | Milliseconds | Optional; disabled |

All these options are read at startup. Restart the service to change them.
Rust embeddings pass `TransactionServiceConfig` and `ServiceOptions` to
`TransactionService::start_with_options`; they supply the database, connection,
holder, lease, renewal and queue values explicitly. The CLI defaults for those
values are not a `TransactionServiceConfig::default()` implementation.

The indexing threshold and maximum compare with strict `>`: equality alone
does not trigger scheduling or back pressure. The byte account includes both
the frozen prefix being indexed and the live suffix. These limits, the cache
budgets and excision admission are accounting policies, not process RSS limits.
See [capacity planning](09_capacity_planning.md) and
[background indexing](../06_indexes/02_background_indexing.md).

Additional admission policies are available to Rust embeddings through
`TransactionServiceConfig.capacity_limits`:

| `CapacityLimits` field | Use | Default |
| --- | --- | --- |
| `max_transaction_ops` | Transaction operation admission | 100000 |
| `max_transaction_bytes` | Transaction byte admission | `67108864` (64 MiB) |
| `max_history_transactions` | History transaction admission | `9223372036854775807` (`i64::MAX`) |
| `max_transaction_read_datoms` | Transaction read-datom admission | 1000000 |
| `max_transaction_read_bytes` | Transaction read-byte admission | `67108864` (64 MiB) |
| `writer_tree_cache_entries`, `writer_tree_cache_bytes` | Writer cache retention | 4096 entries, 64 MiB |
| `program` | Native transaction-program limits | `ProgramLimits::default()` |

The stock executable exposes only the writer cache fields from this table.
`ServiceOptions` also carries execution, fulltext-build, excision, indexing,
hint and telemetry policies. See the [native API reference](../07_peer_api/04_native_api_reference.md)
and [native computation](../07_peer_api/02_native_computation.md).

### Remote Transaction Credentials

These settings configure the TLS transaction protocol separately from the
PostgreSQL connection. Server credentials are read at listener startup; clients
read theirs when `remote_client_config_from_env()` is called.

| Environment variable | Use | Format | Default / status |
| --- | --- | --- | --- |
| `EDB_REMOTE_TLS_CERT` | Server certificate chain | PEM file path | Required on remote server |
| `EDB_REMOTE_TLS_KEY` | Server private key | PKCS8 PEM file path | Required on remote server |
| `EDB_REMOTE_TOKEN_FILE` | Shared authentication token | Private file containing 64 hex digits, with optional surrounding whitespace | Required on server and client |
| `EDB_REMOTE_TLS_ROOT` | Additional client trust root | PEM file path | Optional; platform roots otherwise |

Files must be regular files, not symlinks. Key and token files must be owned
by the effective user with no group/other permissions. PEM files are limited
to 1 MiB and the token file to 1024 bytes. Certificate and server-name
verification are mandatory; there is no plaintext TCP transaction mode. See
[deployment](08_deployment_reference.md) and [high availability](10_high_availability.md).

### Backup Settings

| Setting | Use | Units | Default / where to set |
| --- | --- | --- | --- |
| `--maintenance-pause-ms` | Pause after each successfully copied or committed maintenance batch | Milliseconds | 0; accepted by `backup`, `restore` and `gc` |
| `MaintenanceControl::new(pause, cancel)` | Rust pacing and shared cancellation control | `Duration`, cancellation flag | `MaintenanceControl::default()` has zero pause |
| `BackupReadConfig.cache_entries` | Decoded-node cache for an offline backup reader | Entries | 256; `BackupConnection::open_configured` |
| `BackupReadConfig.cache_bytes` | Decoded-node cache for an offline backup reader | Bytes | `67108864` (64 MiB); `BackupConnection::open_configured` |

Note that:

- Maintenance pacing can reduce I/O pressure by slowing work between batches.
  Zero is valid; a nonzero pause must fit the monotonic clock.
- The pause holds no SQL lock. It is cooperative, applies to each caller and
  is not a process-wide bandwidth limit or a deadline for work inside a batch.
- Backup-reader cache settings affect reads after opening an existing portable
  backup. They do not set backup-copy concurrency.
- EDB exposes no file-backup or S3-backup thread-count property. Portable backup
  uses a filesystem repository; PostgreSQL physical-backup tooling has its own
  configuration.

See [backup and restore](11_backup_and_restore.md),
[backup reads](05_backup_reads.md) and [maintenance controls](04_maintenance_controls.md).

### Configuration Changes

Update the process environment and command line, then restart a transactor to
apply new startup settings. Open a new reader to change its connection or cache
configuration. Existing captured database values remain immutable and may retain
their earlier reader resources. Per-operation limits apply to the operation
that receives them.

EDB has no JVM heap, GC, license-key, Memcached discovery, JDBC validation-query,
or Java metrics-callback properties. Use native process memory budgets,
PostgreSQL connection policy, optional local caching and
[telemetry](03_observability.md). Defaults above describe the current interfaces;
they are not a history of compatibility aliases or release migrations.
