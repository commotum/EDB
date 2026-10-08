# Read Only Connections

Some tasks need only a database value, with none of the transaction processing
or coordination of an online writer. EDB provides writer-independent reads
directly from PostgreSQL storage and fixed reads directly from a portable
backup. No transactor is required to read already published data.

These readers extend the immutable database values central to EDB's design,
making them accessible for validating backups, performing analytical work on a
separate machine, inspecting a database value during an incident, and more.

### Objective

Users need to access database values for a wide range of tasks that have
nothing to do with writing or coordinating changes:

- Validate that a backup contains the expected data.
- Run analytical or ETL workloads against a past database value.
- Inspect a database value during an incident without touching production,
  using an independent backup repository.
- Run performance tests against a real dataset while configuring the reader's
  cache resources to match the workload being tested.

What these use cases share is they need a database value, not a running writer.

### Obstacles

Transactors require resources and operational management. Launching and keeping
one alive for work that performs no transactions adds unnecessary work. Restoring
a backup to live storage also means provisioning storage, running a restore,
waiting for the restore, and tearing it all down afterward.

EDB's backup reader avoids that restore. Its live readers avoid a dependency on
the transactor, but still access PostgreSQL. Reading production storage can
consume production storage resources; use a copied backup repository when the
work must be independent of production. A captured live value may read immutable
objects lazily, so capturing it is not equivalent to making an offline copy.

### Approach

Choose the reader according to whether the work needs a fixed value or ongoing
observation:

| Reader | Storage | Advancement |
| --- | --- | --- |
| `Peer::connect` | PostgreSQL | Captures a value at opening; explicit synchronization adopts later publications. |
| `Connection::connect` | PostgreSQL | Starts an independent background observer; later `db()` calls can return newer values. |
| `BackupConnection::open` or `open_point` | Portable backup directory | Fixed at the selected backup point. |

Neither live constructor starts a transaction service or acquires its writer
lease. `BackupConnection` needs neither PostgreSQL nor a transactor, and queries
the backup without restoring it.

Live readers accept cache limits through `connect_with_cache_limits` and their
configured variants. Backup readers accept `BackupReadConfig` through
`open_configured`. Separately opened readers have their own decoded-node caches;
clones share their reader's state. Live readers can also use the optional
[local SSD cache](00_local_cache.md). Choose a separate cache directory when
analytical work should not compete for a shared SSD cache's capacity. These
cache limits do not bound total process memory or all caller-retained values.

The SSD cache holds optional copies of immutable objects, not the authority for
the current database. Entries are bounded, hash-authenticated and scoped to the
reader's access configuration, database lineage and excision generation. Reusing
the cache can preserve warm data across restarts; recoverable cache corruption
is treated as a miss and the reader fetches the object from storage. Cache
contents do not replace storage access or extend the retention of a captured
value.

## Connection Syntax

Use native Rust constructors with a PostgreSQL connection configuration and an
existing logical database name or identifier. There is no EDB `read-only=true`
URI mode. For example, assuming the storage namespace and database already exist:

```rust
use edb_core::{Connection, Peer};

let peer = Peer::connect(
    "host=localhost user=edb_peer dbname=edb",
    "packages",
    256,
)?;
let fixed_db = peer.db();

// Use an application Connection when observation should continue in the background.
let conn = Connection::connect(
    "host=localhost user=edb_peer dbname=edb",
    "packages",
    256,
)?;
let observed_db = conn.db();
```

Configure transport, authentication and the storage namespace as described in
[Storage Services](../08_operations/06_storage_services.md). Successful reading
does not prove a transactor is running. A standby transactor does not add writer
availability until it acquires authority and completes activation; see
[High Availability](../08_operations/10_high_availability.md). Likewise,
writer-independent reading is not a guarantee about PostgreSQL standby
deployment, replication lag or failover. Storage must remain accessible and the
required publication and objects must be available.

### Read Access and Write Authorization

A `Connection` with no attached writer returns `connection/no-writer` for
transaction submission. That is a property of the connection's composition,
not an authorization boundary: an application can attach a transaction client
later if it possesses one.

Enforce storage read access with dedicated PostgreSQL credentials. The runtime
grant helper gives the peer role `SELECT` on both object and reference tables,
plus database `CONNECT` and schema `USAGE`. Ordinary peer reads perform no SQL
writes. The helper adds grants; inherited permissions or previous grants can
broaden authority. These are trusted storage credentials, not per-logical-database
authorization. See [Runtime Role Configuration](../08_operations/06_storage_services.md#runtime-role-configuration).

A backup reader has no transaction endpoint or live catalog route. Protect its
repository with filesystem permissions and retain it for as long as lazy reads
need its files. The reader API does not prevent someone with separate filesystem
write authority from changing or deleting repository files.

### Backup Locations and Points

Backup constructors take filesystem paths, not backup URIs. `open(path)` selects
the latest completed point, ordered by basis and then log generation.
`open_point(path, &point)` selects an exact `BackupPoint` returned by
`PortableBackup::list_backup_points`. The point includes its lineage, basis,
generation and manifest hash. A basis alone need not identify a unique point.

```rust
use edb_core::{BackupConnection, PortableBackup};
use std::path::Path;

let repository = Path::new("/private/backups/packages");
let points = PortableBackup::list_backup_points(repository)?;
for point in &points {
    println!("basis={} generation={}", point.basis_t, point.log_generation);
}

// Latest completed point at the time of opening.
let latest = BackupConnection::open(repository)?;

// Select an exact listed point; these coordinates are illustrative.
if let Some(point) = points.iter().find(|p| p.basis_t == 148352 && p.log_generation == 0) {
    let selected = BackupConnection::open_point(repository, point)?;
    let db = selected.db();
}
```

`PortableBackup::list_backups` also returns sorted, deduplicated basis values,
but use full points for exact selection. See
[Backup and Restore](../08_operations/11_backup_and_restore.md) for capture,
verification and retention, and [Query a Backup Directly](../08_operations/05_backup_reads.md)
for command-line reads and configured cache limits.

## Liveness

Backup connections are not live. Each call to `db()` or `log()` returns the same
logical value: the selected backup point. Publishing a later backup does not
advance an existing handle; open another handle to select it.

A standalone `Peer` retains its observed state until explicitly advanced, for
example by `sync()`. An application `Connection` observes storage in the
background. Its `db()` returns the most recently adopted value without storage
I/O at that call; subsequent queries can require lazy reads. In every case, a
database value or log already captured remains immutable when the reader
advances. Retain one value throughout an analytical job that requires a fixed
basis. Capture the log from that value with `db.log_value()?` when its basis
must match exactly, rather than making separate calls on an advancing connection.

Synchronization observes published work. `sync_to` waits for a transaction
frontier; `sync_index` waits for the requested indexed frontier and relevant AVET
readiness. Neither starts a writer or schedules index work. Without a writer
publishing the required work, a wait can time out. Backup reads preserve the
captured index readiness, including pending AVET or fulltext work: prepare the
needed indexes before capture. See [Synchronization](../04_transactions/09_synchronization.md).

Held live values do not register storage pins. Their lazy reads depend on the
[reader-protection and retirement policy](../08_operations/00_deployment.md#retirement-age-and-reader-protection).

## Capabilities

Read values support the same query, Pull, entity, history and time-view APIs as
other database values. Captured logs support log reads. Local `with` speculation
produces an immutable hypothetical result without durably transacting or
modifying the source database or backup. Speculation requires one explicit
transaction source; every resulting assertion and retraction, including the
automatic timestamp assertion, carries it. A read workload can therefore test
a transaction's effects without possessing a submission endpoint.

Release handles with normal Rust ownership and `drop`. Clones and retained
database values can keep shared resources alive. Dropping the last application
connection requests its observer to stop; an in-progress storage read can finish
before the worker exits.

## Examples

### Connecting to a Database Without a Transactor

The following helper queries a database value for delivered packages. It assumes
`:package/delivered-at` exists in that database's schema.

```rust
use edb_core::{DatabaseValue, Peer, QueryControl, QuerySourceValue, SemanticError};
use edb_core::edn::write_edn;
use edb_core::edn_query::{parse_query_edn, EdnQueryArgument};

fn delivered_packages(db: DatabaseValue) -> Result<String, SemanticError> {
    let query = parse_query_edn(
        "[:find ?e :where [?e :package/delivered-at]]",
    )?;
    let bound = query.bind(&[
        EdnQueryArgument::Source(QuerySourceValue::Database(db)),
    ])?;
    let outcome = bound.execute(&QueryControl::default(), None)?;
    write_edn(&bound.result_to_edn(&outcome.result)?)
}

let peer = Peer::connect(
    "host=localhost user=edb_peer dbname=edb",
    "packages",
    256,
)?;
println!("{}", delivered_packages(peer.db())?);
```

### Connecting and Querying From a Backup

Use the same query helper with a backup value; no PostgreSQL connection is needed:

```rust
use edb_core::BackupConnection;

let conn = BackupConnection::open("/private/backups/packages")?;
let db = conn.db();
println!("{}", delivered_packages(db)?);
```

To query a particular point, replace `open` with `open_point` using a point
discovered above. Opening and querying authenticate required objects as they
are read; this does not replace deep semantic backup verification.
