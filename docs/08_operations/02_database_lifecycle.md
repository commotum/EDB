# Logical database lifecycle

A public name is a way to find a database, not the database's identity. Atomic keeps
the storage ID and lineage stable while its public name changes. Facts, transaction
receipts, database values and canonical manifest bytes are not rewritten by rename.
Reusing an old name creates a different database with a new storage ID and lineage.

## Provision and rename

Treat catalog mutations as administrative engine operations and restrict access
to the corresponding credentials and APIs. The generic PostgreSQL object/ref
provider is not a feature-specific SQL authorization engine. The usual explicit
PostgreSQL/TLS configuration applies; listing and resolution do not mutate names.

```sh
atomic create --database customers
atomic create --database customers  # EXISTS, not an error or a schema change
atomic list-databases --limit 100
atomic list-databases --after customers --limit 100
atomic rename --database customers --new-name people
```

Rename previews its source identity. Apply with the exact lineage from that output:

```sh
atomic rename --database customers --new-name people --lineage UUID-FROM-PREVIEW --apply
```

The lineage is checked atomically with the name change. A changed/reused source name
cannot silently target a different database. An occupied destination is an error;
there are no implicit historical aliases. Existing handles and the active writer
keep their captured identity. New applications connect using `people`; attempting
to open `customers` fails until that name is deliberately created again.

Listings are keyset-paginated: `--after` is exclusive, default limit 1000, maximum
4096. A full page does not prove another page exists; continue from its last name.
For `--retired`, the cursor is the last storage ID, since there is no active name.

Rust applications use `DatabaseCatalog::{connect_configured,create_if_absent,
resolve,list,rename_checked,retire_checked}`. `CreateDatabaseResult` distinguishes
creation from an existing entry without recovering all data. The lower-level
`storage::BlockDatabase::create` uses the same immutable-root publication protocol.
User-facing `Connection`/`Peer` and service startup names resolve once. Low-level
storage IDs and exact snapshot-reference routes are not public-name aliases.
Do not feed `connection.identity().database_id()` back into a named connect after
rename. Keep the connection, use the current public name, or reopen an exact
snapshot reference while the identity is active.

## Retire, then reclaim

```sh
atomic delete --database people
atomic delete --database people --lineage UUID-FROM-PREVIEW --apply
atomic list-databases --retired
```

Delete removes the active name and fences its writer. New connections and exact
snapshot reopening are rejected. Captured immutable values remain readable within
storage retention: deletion does not mutate a value your application already holds.
Retirement does not immediately erase stored facts or invalidate completed portable
backups. Readers create no storage pins or sessions, and dropping values requires
no SQL cleanup. Normal GC keeps retired objects for 30 days; applications must
finish their reads within the configured grace. A shorter explicit retention age
can invalidate held values. New opening requires a current authorized identity
and rejects retirement and old excision generations.

Storage reclamation is a separate owner operation against an exact retired storage
ID and lineage. Supply the physical PostgreSQL database and catalog schema as an
additional target check. Choose a retention interval appropriate to your recovery
policy; zero is suitable for a disposable test fixture, not a default recommendation.

```sh
atomic gc-deleted --storage-id ID --lineage UUID --postgres-database atomic \
  --catalog-schema public --older-than-seconds 2592000
atomic gc-deleted --storage-id ID --lineage UUID --postgres-database atomic \
  --catalog-schema public --older-than-seconds 2592000 --apply --batches 10
```

Preview is read-only and reports provider metadata plus the persisted collector
checkpoint. Apply validates the exact retired identity, then advances the same
catalog-wide Rust collector as ordinary `gc`; it is not a separate SQL teardown
pipeline. One operator call advances at most 4,096 graph/metadata/object steps.
Reports include the phase, ownership steps, objects examined/removed and whether
the sealed cycle completed. These are work counters, not SQL row counts, elapsed
time bounds or measured peak memory. A legal individual object can be up to
64 MiB. The CLI defaults to one apply call; repeat it or choose an explicit
`--batches` count when `cycle_complete=false`.

`cycle_complete=true` is not a claim that this database's exclusive bytes are all
gone or that global storage is quiescent. Shared durable content, outstanding ownership changes and the chosen age can
retain objects. Expired maintenance roots and old read-index authorization
pruning can add work for the next cycle; held values create no storage pins. Retirement
removes the publication and fences its writer and pending excision work in one
guarded change; issued-identity tombstones prevent old-route reuse.

The minimum age applies to previously published objects after their last owner
is retired, not their original upload date. Never-published abandoned uploads
fenced by a later protection epoch have no retirement-age promise. Live owners
and active protected uploads remain protected. Missing or corrupt ownership
evidence fails closed rather than authorizing speculative deletion. Neither
command erases external backups, PostgreSQL backups/WAL, replicas or copied
process memory. See [operations](00_deployment.md#retirement-age-and-reader-protection)
for reader revocation, pacing and checkpoint details.

This is not excision of selected facts and not `DROP SCHEMA`. Both `gc` and
`gc-deleted` use the shared collector; the latter additionally checks the retired
target and lineage.

### Backup/restore integration notes

An unfinished restore reserves a visible name even before
its first readable database value exists. Listing is not a readiness check.
Create returns `EXISTS` for that reservation; operators can retire it, and a late
restore publication must fail instead of reviving it.

Completed backups remain independently verifiable. Restore retains canonical
lineage. Restoring a retired lineage into the same catalog requires finishing its
reclamation first, or choosing another catalog. After reclamation, restore allocates
a fresh storage route while retaining the backup's lineage; old exact-reference
routes remain retired. See the restore checks in [admin.md](01_administration.md).

## Catalog initialization

The block provider installs into a fresh PostgreSQL namespace through
`storage::PgBlockStore::install(&config)`. Catalog identities, public names,
publication roots and maintenance checkpoints use its opaque object/ref protocol;
there are no separate lifecycle tables or triggers. Existing unrelated schemas
are not overwritten or adopted. Historical schema upgrades are unsupported.
Runtime opens validate the installed provider and perform no schema DDL.
