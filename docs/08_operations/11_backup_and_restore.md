# Backup and Restore

[Backup](#backing-up) copies an EDB database to a filesystem directory for which
you have appropriate [permissions](#permissions).

[Restore](#restoring) copies an EDB database backup to an installed PostgreSQL
catalog.

Backup and restore are useful for:

- Disaster recovery
- Moving databases from one PostgreSQL catalog to another

Backups can be performed against a live system. Repeat backups to the same
directory take advantage of [differential backup](#differential-backup).
Each completed backup is an immutable point with a database lineage, transaction
basis `t`, log generation and manifest hash.

## Backing Up

You can use the `backup` command to back up a named database to a
[backup directory](#backup-directory-syntax):

```sh
edb backup --database application --repository /private/backups/application
```

`EDB_POSTGRES_URL` selects the source PostgreSQL connection. Build the executable
with `cargo build --bin edb`; see [administration](01_administration.md) for
connection and installation setup. The parent directory must already exist.
Backup creates the repository and its `objects` and `points` directories as
needed, without creating arbitrary ancestor directories.

Backup captures one immutable live publication and copies its reachable object
graph, including existing indexes, log, recent tail, metadata, receipts,
read-authorization records and program dependencies. Ordinary writers can
continue. The captured point does not include transactions committed after that
publication. Capture does not
consolidate indexes or rebuild search; complete live indexing first when the
backup needs those indexes ready.

The backup process needs memory for object traversal and admitted object bytes.
Its visited-object set grows with the reachable graph. EDB has no fixed
memory-per-datom sizing rule for this operation; measure the retained graph and
leave room for the largest admitted objects. Configure source storage retention
to cover the copy duration: captured values do not register indefinite storage
pins. See [retention and reclamation](00_deployment.md#inspection-retention-and-reclamation).

Backup directories are per database lineage. You can back up the same database
at different points in time to a single directory. Renaming a database does not
change that lineage.

The repository's lineage claim identifies its database; it does not act as an
exclusive backup-job lock.

> You cannot back up different database lineages to a single backup directory.
>
> Attempting this returns `backup/claim-failed`: the repository belongs to
> another lineage, or its identity claim is corrupt.

Objects are authenticated and durably published before the point becomes
visible. Individual files are published without overwriting existing content;
there is no repository-wide transaction. A failed attempt can leave copied
objects, and a failure after point publication can leave a completed point.
List the points and retry the backup as needed. A successful copy is not deep
semantic verification.

An object file left by an interrupted backup does not prove that its children
were copied. Only a published point advertises a completed capture with all its
dependencies; do not construct a point from staged objects manually.

Backups are not encrypted by this command. Encryption at rest, protected media
and retention are deployment responsibilities.

### Differential Backup

The first time you perform a backup it must copy the entire reachable database
graph.

Repeat backups to the same directory do not need to write unchanged objects
again. The repository stores objects by content hash and authenticates existing
objects before reusing them. Repeatedly backing up the same database to the same
directory reduces duplicate storage and copying. The graph is still traversed;
reported `objects_written` and `objects_reused` describe object operations, not
saved network round trips or a guaranteed backup duration.

If the same basis and generation already exist, capture checks that the
committed information agrees and returns the existing point. It does not
replace that point solely because live derived indexes have advanced.

## Listing Backups

You can use `list-backups` to list the available points that can be restored:

```sh
edb list-backups --repository /private/backups/application
```

This is an offline command and needs no PostgreSQL credentials. Each
`BACKUP_POINT` reports the lineage, basis, generation and manifest hash, followed
by a `BACKUPS` count. Points are ordered by basis and then generation.

Keep the exact basis/generation pair. Excision can produce a different generation
at the same basis. Both zero basis and zero generation are valid when listed.
In the examples below, `42` and `0` stand for a pair obtained from this command.

## Restoring

You can use the `restore` command to restore a database.

To restore a database you must first do the following:

- Select the **destination** connection in `EDB_POSTGRES_URL` and install the
  current EDB storage in the destination PostgreSQL database/schema.
- Stop the target writer before applying the restore. Target, lease and catalog
  guards reject concurrent changes.
- Obtain an exact basis and generation from [listing backups](#listing-backups).
- Keep the repository an operator-controlled immutable source throughout
  verification and copy; do not delete or modify its files during restore.

The live source database need not be reachable. PostgreSQL at the destination
must be running, but a target EDB transactor is not needed for restore. Use the
explicit physical target names to protect against selecting the wrong catalog:

```sh
# EDB_POSTGRES_URL must already select the destination connection.
edb install
edb restore --repository /private/backups/application --basis 42 --generation 0 \
  --target-database recovered --postgres-database edb_recovery --catalog-schema public
```

`--postgres-database` names the PostgreSQL database, `--catalog-schema` names
the installed table namespace, and `--target-database` names the logical EDB
database. The physical names must match the configured destination.

By default, restore previews the selected point. It authenticates the reachable
object graph and observes the destination catalog without staging or activating
a database. Target lineage compatibility is checked when applying. To apply
the same selection:

```sh
edb restore --repository /private/backups/application --basis 42 --generation 0 \
  --target-database recovered --postgres-database edb_recovery --catalog-schema public --apply
edb inspect --database recovered
```

The CLI requires an explicit point; it does not default to the most recent
backup. You can select any listed point for a new destination. Restoring over an
existing database additionally requires the same lineage, a non-retreating basis
and generation, compatible log history and preservation of acknowledged request
identities. An older backup is not an in-place rollback mechanism. An unrelated
existing lineage returns `backup/target-lineage-conflict`.

Restore can give a database a different name. However, you cannot restore a
single lineage to two active names within the same PostgreSQL catalog. Use an
independent PostgreSQL database/catalog for a simultaneous copy of a live source.
Restoring a retired lineage creates a fresh route; it does not reactivate retired
handles. Lineage and transaction identities are preserved.

It is not necessary to delete old tables or files before or after restoring.
The destination must have the current installed EDB format. Restoring into a
fresh catalog avoids retaining unrelated old objects; ordinary storage
reclamation remains separate from restore.

### Interrupted Restores

Restore copies objects in persisted batches and conditionally activates the
selected point. A new target name can be visible while its database is still
unavailable, with `backup/database-restoring`. A checkpoint owns only objects
whose children have been copied; it preserves progress without publishing an
incomplete database.

An interrupted or ambiguous attempt may have committed staging or activation.
Retry the same repository, basis, generation and destination. Do not manually
publish staged heads or assume a process failure rolled back earlier commits.
Activation and the completion record are one guarded change. An exact completed
retry returns its recorded activated root without rolling back a newer target
head.

After success, start the target transactor and open new clients. Check the
application's current and historical reads, queries and exact transaction retry
workflow. Existing captured values remain subject to storage retention grace;
they do not automatically become the restored value.

## Verifying Backups

You can use `verify-backup` to verify a completed backup offline:

```sh
edb verify-backup --repository /private/backups/application --basis 42 --generation 0
```

Verification is deep by default. It authenticates the reachable object graph,
checks log structure, read authorization and receipt/provenance coordinates,
replays the canonical log, and compares retained indexes and search records
with that replay.
Allocation frontiers, no-history omissions and partial AVET projections receive
explicit checks. This work can use memory and I/O proportional to retained
history and indexes.

For authenticated reachability without semantic replay:

```sh
edb verify-backup --repository /private/backups/application --basis 42 --generation 0 \
  --presence-only
```

`--presence-only` still reads and authenticates reachable objects, including
program dependencies. It is not a filename-existence check and does not prove
semantic agreement. The result prints `semantic=false`; deep verification
prints `semantic=true`. Neither mode changes the repository. Restore preview
uses the presence check, and restore apply authenticates copied objects; use
the separate deep verification command when semantic verification is required.

## Reading from Backups

You can open backup points as fixed database and log values without restoring
them. `BackupConnection::open(path)` opens the latest point by basis and
generation. `BackupConnection::open_point(path, &point)` opens an exact listed
point and checks its lineage and manifest hash. Later backups do not change
those values.

Backup values use the same `DatabaseValue`, schema, exact-value, query and Pull
semantics as live values. The repository supplies immutable objects to the
shared read engine; opening it does not grant live database mutation authority.

The CLI supports query, Pull and local transaction speculation against a
repository without PostgreSQL credentials:

```sh
edb query --repository /private/backups/application --file query.edn
edb pull --repository /private/backups/application --file pattern.edn --entity 12345
edb with --repository /private/backups/application --file transaction.edn
```

Add `--backup-basis 42 --backup-generation 0` together for an exact point.
Speculation does not modify the repository. Direct reads use the captured
indexes and recent tail and do not replay from genesis on open. Their readiness
and admission limits match live reads. See [reading backups](05_backup_reads.md)
for query sources, caching and integrity behavior.

## Rust API

`PortableBackup` exposes the same capture, listing, verification and restore
operations. This example captures from one configured catalog, verifies the
exact point offline, and restores it to a separately configured destination:

```rust
use edb_core::{PortableBackup, PostgresConnectionConfig, RestoreResult, SemanticError};
use std::path::Path;

fn copy_database(
    source: &PostgresConnectionConfig,
    destination: &PostgresConnectionConfig,
    directory: &Path,
) -> Result<RestoreResult, SemanticError> {
    let mut backup = PortableBackup::connect_configured(source)?;
    let point = backup.backup_database("application", directory)?;

    for available in PortableBackup::list_backup_points(directory)? {
        println!("{} {}", available.basis_t, available.log_generation);
    }
    let verified = PortableBackup::verify_backup_point(
        directory, point.basis_t, point.log_generation, true,
    )?;
    assert_eq!(verified.point.manifest_hash, point.manifest_hash);

    let mut restore = PortableBackup::connect_configured(destination)?;
    restore.restore_backup_point(
        directory, point.basis_t, point.log_generation, "recovered",
    )
}
```

The destination must already be installed and the target writer stopped. The
Rust restore method applies immediately; the CLI's preview and physical-target
confirmation flags are command-level behavior. `RestoreResult` contains
`point`, `target` and `activated_root`; open a peer to read the restored database.

`list_backups(directory)` returns only deduplicated basis coordinates.
Basis-only methods `restore_backup`, `verify_backup` and
`verify_backup_presence` select the highest available generation at that basis.
Prefer the explicit `*_point` methods when preserving an exact selection.
`verify_backup_point_presence(directory, basis, generation)` returns a
`BackupPoint`. `verify_backup_point(directory, basis, generation, deep)` returns
a `BackupVerification` with the point, replayed `database` and `objects_read`.
Both values of `deep` perform canonical replay; `false` is not the presence-only
operation. Deep mode adds structural and derived-projection checks.

## Backup, Restore, and Verify Performance

Backup and restore accept `--maintenance-pause-ms N`, default `0`, to pace work.
Capture pauses at 128-object traversal boundaries and phase completion; restore
pauses after a persisted batch of at most 64 traversal/copy steps. These are not
byte, transaction-count or total-memory bounds. One admitted object can be
large. Cancellation is cooperative at safe boundaries.

The Rust equivalent is `with_maintenance_control(MaintenanceControl)`. See
[maintenance controls](04_maintenance_controls.md) for cancellation and metrics.
Verification exposes no CLI pacing flag. Its explicit replay and graph work
should be budgeted separately from ordinary selective reads.

## Deleting Backups

Backup space can be reclaimed by deleting the entire content hierarchy at a
particular backup directory's location after all users of that repository have
finished.

This reclaims the repository's space and deletes all the different point-in-time
backups associated with it. Deleting a single point and collecting only its
unshared objects is not a supported backup-management operation. Objects are
shared between points; do not delete object files individually.

Excision in a live database does not erase independent older backups or copies.
Their retention and deletion remain explicit operator responsibilities.

## Backup Directory Syntax

The `--repository` argument is a filesystem path:

| Destination | Syntax |
| --- | --- |
| Absolute directory | `/private/backups/application` |
| Relative directory | `backups/application` |

Use an absolute path for an unambiguous operational target. Relative paths are
accepted; backup connections anchor them when opened. `file:` and `s3:` URIs
are not backup destination protocols. The current implementation supplies a
filesystem repository, not a cloud object-storage sink.

## Permissions

The backup process needs source catalog read access and write access to the
repository. Restore needs read access to the repository and destination catalog
authority to insert/protect objects and publish restore/catalog references.
Provision the destination with administrative credentials; see
[storage services](06_storage_services.md) for the storage roles and trust
boundary. Offline listing, verification and reading need only filesystem access.

Keep repositories private and under operator control. On Unix, newly created
repository directories use mode `0700`, and admission rejects group/world
writable repository directories. The repository and its `objects` and `points`
directories must be real directories, not symlinks; entries read as files must
be regular files. Reads enforce current-format file-size limits and reject
files whose length changes during the read. These checks do not replace securing
the parent path and filesystem. The commands do not configure cloud IAM policies
or encryption.

## Limitations

Backup and restore are not suitable for cloning an active database under a
second name in one catalog. Use a separately installed destination catalog.

Portable backup captures durable PostgreSQL databases. It does not back up an
in-memory `Database` or a local speculative value. Current-format backups are
not a cross-version compatibility promise; unsupported formats fail explicitly.
No command silently resets or upgrades a database to make a backup readable.
