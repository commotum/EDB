# Query a backup directly

`BackupConnection::open(path)` opens the latest published point without a
transactor, PostgreSQL connection, restore or replay from genesis. `open_point(path,
&point)` selects an exact `BackupPoint` from `PortableBackup::list_backup_points`.
`db()` and `log()` keep that fixed point even when later backups are published.
Relative repository paths are anchored when opened; changing the process's
working directory does not redirect an existing handle's later reads.
Retain the point's lineage, generation, basis and manifest hash when sharing it.
An online `SnapshotReference` is not a backup locator.

The values use the existing query, Pull, entity, history/time-view, local `with`,
program-resolution and log APIs. Speculation never changes the backup. The
repository-backed block snapshot uses the same selective index, fulltext and
native-program readers as live peers. It has no live catalog route, writer
authority or database pin, and cannot create an online `SnapshotReference`.

The stock executable accepts the same EDN commands without PostgreSQL credentials:

```sh
atomic query --repository /private/backups/customers --file query.edn
atomic pull --repository /private/backups/customers --file pattern.edn --entity 12345
atomic with --repository /private/backups/customers --file transaction.edn
```

Use `--backup-basis N --backup-generation N` together to select a listed point.
`--database` and `--repository` are mutually exclusive. File `-` reads stdin;
results remain EDN. Named query sources also accept
`{:repository "/private/backups/customers" :basis 42 :generation 0 :log true}`;
omit `:log` for a database source, or both coordinates for the latest point.

## Cost and integrity

The manifest retains one canonical publication with its existing index roots,
log and unindexed tail. Capture copies those immutable objects without indexing
or rebuilding search. Offline opening uses the live reader's tail handling and
admission limits. Pending AVET or fulltext work has the same readiness behavior
as the captured live value. Index the live database before capture when a backup
needs those indexes ready. Log seeks use the shared authenticated page/skip links.

A fresh database can be backed up immediately after `atomic create`, including
an empty basis. Required canonical, receipt-base and program objects must be
available; missing provenance is an error, not permission to drop exact retries.

Opening reads root and schema/ident metadata and loads the admitted recent tail;
indexed data leaves and older log payloads are loaded on demand and authenticated
against their immutable references. Missing
or corrupt required files return errors, and failed cursors fuse. A previously
authenticated cached value remains valid. Direct reading is not a substitute
for deep semantic backup verification.

`BackupReadConfig` bounds decoded-node cache entries/bytes, including zero to
disable it. Snapshot roots/schema/idents, active cursors and caller-retained
results are separate memory. `read_stats()` reports immutable-object bytes/read
counts, including object admission; repository directory/claim/locator metadata
and OS block traffic are not included. Complete-path wall time includes them.
`cache_stats()` reports retained cache bytes, hits and evictions.

Repository file sizes and envelope lengths are checked before body allocation
against current format limits, independently of the cache budget. Current
objects have a 64 MiB physical and decoded size ceiling. A single admitted read
cannot follow a growing file beyond its accepted length; authentication and
format decoding then validate the canonical bytes. Valid objects remain
readable with a smaller or disabled node cache.

Measure capture and open/query/consume/drop separately at both small and large
entity counts. Exercise offline fulltext, native programs, history, speculation
and log reads without relying on the source database. Selective access does not
imply constant memory or latency; normal leaf size limits still apply.

These development backups are not a cross-version compatibility promise. Create
a current-version backup from a supported database; unsupported versions fail
explicitly. No tool silently resets a database. Current-version durability,
restart, exact retries and backup/restore remain required. Backup repositories
are private operator-owned directories. Excision does not erase independent
older backups or copies; retention and deletion remain explicit operations.
