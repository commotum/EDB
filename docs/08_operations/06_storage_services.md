# Setting up Storage Services

This document walks through the process of provisioning a storage service for
use with EDB.

## Storage Services

Storage service options are listed here. The steps required to provision them
and detailed instructions are included below.

| Storage | Intended use | Setup |
| --- | --- | --- |
| In-memory database values | Local development and transaction speculation | [In-Memory](#in-memory) |
| PostgreSQL | Durable databases, including local development | [SQL Database](#sql-database) |

PostgreSQL is the current durable storage provider. EDB does not provide MySQL,
Oracle, DynamoDB, DynamoDB Local, or Cassandra storage protocols. A local SSD
cache is disposable block reuse, not another authoritative storage service.

Changing the connection string selects another PostgreSQL storage namespace; it
does not move a database. Use [backup and restore](01_administration.md#back-up-and-verify)
to copy a database to a separately installed compatible destination. Earlier
development databases, backups and checkpoints are not compatible with the
current engine; use a fresh installation and regenerate data through current
interfaces.

### Storage Client Dependencies

EDB depends on Rust PostgreSQL client libraries. The dependencies are declared
in the `edb-core` crate's `Cargo.toml`, with resolved versions in `Cargo.lock`.
The synchronous provider uses `postgres`; asynchronous connection handling uses
`tokio-postgres`; TLS connections use `postgres-native-tls` and `native-tls`.

In your application, add a dependency on the EDB crate, adjusting the checkout
path:

```toml
[dependencies]
edb-core = { path = "/path/to/EDB" }
```

Cargo resolves the storage client dependencies. There are no JDBC drivers or
additional driver jars to place on the application's classpath. Build the
administrative and transactor executable from the EDB checkout root:

```sh
cargo build --locked --bin edb
```

The commands below use `target/debug/edb` from that directory.

## In-Memory

In-memory storage is a convenience for local development.

EDB's durable system separates storage, transaction processing and peers. The
in-memory database allows you to run transaction assessment and reads in your
application without a separate transactor or storage service. Your Rust
application's in-process memory is used for transient storage which disappears
when your process exits.

```rust
use edb_core::Database;

let db = Database::bootstrap()?;
```

`Database::bootstrap()` creates an immutable database value containing the
built-in schema. A local `with_edn` transaction returns a report whose
`db_after` is the new database value. Retain that value for subsequent
operations; the original value remains unchanged.

> No data is persisted after the application process stops. These values have
> no named in-memory connection URI and do not create a durable database to
> reconnect to.

See the [in-memory tutorial](../01_tutorials/02_in_memory.md) for schema,
transaction, query and history examples.

## Dev Mode

For development that requires persistence across restarts, use a local
PostgreSQL database and the same explicit installation process as a deployed
database. EDB does not start an embedded SQL server inside the transactor or
provide a separate file-based dev storage protocol.

Storage, transactor and application processes can be colocated for interactive
development. Follow [SQL Database](#sql-database) to install storage, then
[run a transactor](../01_tutorials/04_run_a_transactor.md). Process exit does
not discard committed PostgreSQL storage as it does an in-memory value.

### Securing Remote Access

To allow remote peers access to storage, configure PostgreSQL's listener,
authentication and network access for the intended clients. Give the transactor
writer credentials and peers their own read credentials. Configure each
process's PostgreSQL connection string and trust roots explicitly.

TCP connections default to verified TLS, at least TLS 1.2. Certificate and
hostname verification remain mandatory. `EDB_POSTGRES_TLS_ROOT` can name a PEM
file containing an additional trust root. Unix sockets are local connections;
`sslmode=disable` explicitly selects plaintext for an intentionally chosen
local/development connection. There is no EDB `storage-access=remote` property.

Remote transaction submission is a separate connection to the transactor. Its
TLS certificate and bearer token do not replace a peer's PostgreSQL
credentials. See [remote applications](../01_tutorials/01_application_workflow.md#remote-applications).

### Choose Passwords

Create distinct dedicated PostgreSQL LOGIN roles for administration, writers
and peers. Choose and manage authentication through your PostgreSQL
administration; EDB creates no default users or passwords. Keep administrative
credentials separate from runtime credentials.

Supply connection parameters through `EDB_POSTGRES_URL` for the executable, or
`PostgresConnectionConfig` for Rust applications. Keep passwords and full
connection strings out of logs and command-line arguments. The
[application workflow](../01_tutorials/01_application_workflow.md) describes
configuration shared by the executable and native application.

### Rotating Passwords

Rotate credentials through PostgreSQL and update the affected processes'
connection configuration. New connections must authenticate with the updated
credentials; an already constructed configuration does not reread your
environment. Coordinate reconnection or restart with your application's
lifecycle. EDB has no old-password/new-password transactor properties or
automatic password rotation service.

## SQL Database

The steps to provision a SQL database as your storage service are:

- Set up a PostgreSQL database, or use an existing one.
- Create a dedicated empty PostgreSQL schema for EDB's storage namespace.
- Create dedicated administrative, writer and peer roles.
- Get the PostgreSQL connection string and explicitly select the schema.
- Install the EDB storage tables and grant runtime privileges.
- Create a logical EDB database, then start the transactor and peers with their
  respective credentials.

PostgreSQL's database and schema are physical storage selections. The logical
database name passed to `edb create --database` is an EDB catalog name within
that storage namespace. Several logical databases can share one namespace.

### Select the Storage Namespace

Use an object-owning administrative connection in `EDB_POSTGRES_URL` for
installation. EDB selects PostgreSQL's `current_schema()` at connection time,
then qualifies storage table names with that schema. Make the intended schema
the selected schema through your connection's `search_path`; configure the
same selection for the administrator, writer and peers. For example, this
password-free connection-parameter shape selects a pre-existing schema named
`edb_store`:

```text
host=db.example.com dbname=edb_storage user=edb_admin options='-c search_path=edb_store'
```

Replace the host, database and role with your deployment's values and supply
authentication securely. The schema must exist and be accessible to the
connecting role. A missing or system namespace is rejected; the installer does
not create it. Avoid relying on role-specific default schemas to select the
same namespace for different runtime users.

### Create the Storage Tables

With administrative connection parameters configured, run:

```sh
target/debug/edb install --writer-role edb_writer --peer-role edb_peer
target/debug/edb create --database my-db
target/debug/edb status --database my-db
```

The two runtime roles must already exist. `install` creates two tables:

| Table | Contents |
| --- | --- |
| `edb_objects` | SHA-256 object identity, opaque immutable payload and protection epoch |
| `edb_refs` | Reference key, monotonically advancing revision and opaque reference value |

The installer also writes the current storage-format marker and revokes public
access to the tables. It requires an empty namespace or a matching current
installation. Repeating installation on a matching store is idempotent.
Installation does not reset existing data, repair schema damage, or migrate an
older format. Runtime constructors validate the format and perform no DDL.

Installation and grants are separate committed operations. `INSTALLED` can
precede a failed grant; correct the roles or privileges and rerun the same
command. Logical database creation is also explicit and idempotent. It supplies
initial read indexes, so ordinary startup needs no consolidation step.

Rust operators can use `storage::PgBlockStore::install(&config)`, open a
`PgBlockStore`, and call `grant_runtime_privileges(writer_role, peer_role)`.
Do not substitute a hand-created lookalike table or another SQL server for the
installed provider.

### Runtime Role Configuration

The runtime-grant helper supplies these privileges:

| Role | Objects | References | Database and schema |
| --- | --- | --- | --- |
| Writer | `SELECT`, `INSERT`, `UPDATE` | `SELECT`, `INSERT`, `UPDATE`, `DELETE` | `CONNECT`, `USAGE` |
| Peer | `SELECT` | `SELECT` | `CONNECT`, `USAGE` |

The roles must be distinct. The helper rejects superusers, roles with
`CREATEROLE`, `CREATEDB`, `REPLICATION` or `BYPASSRLS`, and owners of the storage
schema or tables. It adds privileges; it neither audits nor revokes other
grants or inherited access.

These are trusted storage credentials, not per-database SQL authorization.
Writer object-update authority supports protection metadata; the Rust provider
keeps stored object payloads immutable. Raw SQL credentials are not a sandbox
against arbitrary table changes. Use separate installations or application
authorization when storage clients must not trust one another.

Ordinary peer reads perform no SQL writes. Checkpoint consumers additionally
need reference-write authority; the peer grant alone does not supply it. The
helper grants neither runtime role object deletion. Administrative lifecycle
and reclamation operations require that authority, including catalog rename
and retirement. See [database lifecycle](02_database_lifecycle.md).

### Validation Query

EDB does not provide a configurable JDBC validation query. Connecting to a
store checks the selected namespace and its `system/format` reference. A
successful `status` command observes catalog coordinates; it is not a deep
integrity check or proof of a live transactor.

For explicit deep inspection of a created database, use:

```sh
target/debug/edb inspect --database my-db
```

Inspection authenticates reachable objects and compares derived information
with canonical history. It can take time and memory proportional to retained
history. See [provisioning and inspection](01_administration.md#provisioning-and-inspection).

## Storage Requirements and Durability

PostgreSQL stores opaque objects and guarded references; Rust owns transaction
assessment, indexes, writer fencing and catalog semantics. Immutable objects
are inserted before publication. Guarded reference changes run in a SQL
transaction, checking expected revisions and committing the selected changes
together. Application datoms are not relational tables that PostgreSQL queries
or updates directly.

### Immutable Objects and Mutable References

The storage boundary separates database behavior from opaque byte storage.
`ObjectReader` reads required immutable objects, authenticates their identity,
and reports missing objects as errors. `ObjectWriter` accepts canonical bytes
and returns their content identity. Acceptance can be buffered: callers must
successfully `flush_objects` before publishing object IDs or handing them to
an independent reader. The PostgreSQL writer commits and authenticates its
objects before returning; its flush has no pending work. An object-write batch
can fail after leaving unreferenced objects, so object acceptance is separate
from publication.

The same immutable reader interface supports reads from a backup repository.
It supplies neither mutable-reference authority nor a running writer, and does
not make repository files another durable storage provider. See
[backup and restore](01_administration.md#back-up-and-verify).

`PgBlockStore` supplies single-reference and multi-reference compare-and-exchange.
Each condition names a reference and its expected revision. Guard keys are
locked in deterministic order, including keys that do not yet exist; all
conditions must match before any selected reference changes commit. A
conditional miss returns a conflict, not a successful overwrite. Each changed
reference advances its revision. Writing a null value leaves a revisioned
tombstone, so deletion followed by reuse cannot pass a stale revision check.
An expected revision of `None` means a key has never been created, not that its
current value is null.

Neither these operations nor the SQL tables interpret datoms, query expressions,
log trees or receipts. Rust selects publication guards and interprets database
structures. The provider's `protected_epoch` is numeric metadata supplied by
the caller, not a meaning inferred from an object's payload. Object caches
reuse immutable bytes; they do not cache mutable reference values.

An object's identity authenticates its canonical bytes. Storage retains raw
bytes or a versioned gzip representation when that is smaller, checking sizes,
complete decompression and the canonical SHA-256 on reads. The canonical
per-object ceiling is 64 MiB even when compression makes the stored payload
small. A corrupt authoritative object fails explicitly.

### Publication and Retry Authority

Protected writes check their reference guards and raise the protection epoch
of both newly inserted and reused objects in the same SQL transaction. Reusing
an existing object must refresh its protection too: finding an old unreferenced
object does not protect it from collection. An object's stored payload remains
immutable even when its protection metadata changes.

After a lost acknowledgment, an authenticated read proves that an object's
content exists. It does not prove that this write attempt refreshed its
protection epoch. Repeat the guarded protected operation when resolving that
uncertainty. Likewise, a provider reference conflict is distinct from a writer's
committed transaction receipt. Storage operations do not reassess transactions;
transaction request identity and receipts belong to the writer protocol.

EDB waits for PostgreSQL acknowledgment of object writes and guarded publication
before reporting a fresh durable transaction as complete. The durability of
that acknowledgment depends on the deployed PostgreSQL storage and durability
configuration. Replication, WAL retention, physical backups and PostgreSQL
failover remain deployment responsibilities; the EDB writer lease does not
provide them. See [ACID](../04_transactions/08_acid.md).

An interrupted submission can have an unknown outcome. Retry the identical
request key and content to resolve it; loss of acknowledgment is not proof of
rollback. Choose connection, statement and lock timeouts for each role using
the [transport and failure policy](00_deployment.md#transport-and-failure-policy).

Held database values do not register storage pins. Retention must cover active
reads and backup copies; cache misses still require available retained storage.
Choose a deliberate garbage-collection age and follow the
[retirement and reader-protection policy](00_deployment.md#retirement-age-and-reader-protection).

### Names, Retirement and Collection

A catalog rename moves the same logical database identity and physical route
to a new name. Retirement removes the live name, releases database roots and
writer authority, and records the retired identity. Neither operation itself
traverses and deletes the database's objects. Catalog operations reread current
state after a conflict and reevaluate their conditions within a bounded retry
loop; they do not grant stale callers unconditional authority. See
[database lifecycle](02_database_lifecycle.md).

Root publication records ownership additions and removals atomically with the
root changes. The collector seals an epoch, folds additions before removals,
protects ownership metadata, and checkpoints the folded state before deleting
objects. Shared content remains owned while any registered owner retains it.
Guarded deletion checks the collector's authority revisions and the protection
threshold atomically, preventing a displaced collector from deleting objects
under stale authority. Rust traverses object graphs; PostgreSQL performs the
opaque guarded deletion.

Collection is bounded per advance, not per complete collection cycle. Each
cycle walks the ownership-index metadata and enumerates every namespace object,
including live objects. A small `maximum_steps` limits one advance; it does not
remove the full object-enumeration cost of finishing a cycle. Retirement age
protects previously owned objects. Unknown objects that never acquired a
published owner are abandoned writes or obsolete collection metadata, protected
by epoch fencing rather than a promise that every object receives the same
age grace. Use the [catalog-wide garbage collection](01_administration.md#catalog-wide-garbage-collection)
entry point and plan for the whole namespace's retained data.

## Running on Hosted Infrastructure

Use the same PostgreSQL provisioning and role separation when storage or the
transactor runs on hosted infrastructure. EDB has no provider-specific
automation to create cloud accounts, IAM roles, databases or network rules.
There is no Cassandra replica-count requirement or DynamoDB throughput setting
for the PostgreSQL provider.

Measure your actual use when planning production capacity. Workload, retained
history, index activity and maintenance determine resource requirements; no
fixed throughput or failover bound follows from the storage setup. Once the
database and credentials are configured, continue with
[transactor setup](../01_tutorials/04_run_a_transactor.md) and
[observability](03_observability.md).
