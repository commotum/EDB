# Administrative CLI

`edb` exposes the existing native operators as commands. Use a separately
controlled administrative account and an explicitly chosen PostgreSQL catalog.
Commands do not install PostgreSQL, create PostgreSQL databases, migrate on
startup, or silently repair a damaged database. Provision the storage service and
credentials separately; then use the commands below for the EDB catalog.

Build with `cargo build --bin edb`. Set `EDB_POSTGRES_URL`, using
`sslmode=disable` only for an explicit plaintext connection, as described in
[application.md](../01_tutorials/01_application_workflow.md). Verified TLS/root certificates and the same
connection/statement/lock settings apply to administrative connections. Offline
`list-backups` and `verify-backup` need no PostgreSQL credentials. Never paste
connection strings into logs or shell command arguments.

## Provisioning and inspection

```sh
edb install --writer-role edb_writer --peer-role edb_peer
edb create --database application
edb status --database application
edb inspect --database application
```

The two distinct runtime roles must already exist and satisfy restricted-role
requirements. `INSTALLED` and `GRANTED` are separate committed results: grant
failure does not undo installation. Installation creates the current opaque
storage format in two tables and recognizes a matching installation idempotently.
It does not repair schema damage, create the PostgreSQL database/schema, reset
data or upgrade earlier development catalogs. Choose a fresh empty namespace for
an unsupported format. See [operations.md](00_deployment.md).

`status` is a lightweight catalog-coordinate observation, not a deep integrity
check or proof of a live transactor. `inspect` captures one immutable publication
and checks its retained graph and derived data deeply by default; `--shallow`
checks root/log/index/metadata coordinates without graph traversal and canonical
replay. Inspection can use memory and time proportional to retained history.
A nonzero `INTEGRITY_PROBLEM`
count exits unsuccessfully. Diagnostic labels are printed, not subject-bearing
problem messages.

`pending_avet_projections` and ordinary `index_lag` are reported separately,
alongside the captured basis, index basis, generation and publication revision.
Authenticated pending maintenance is not by itself corruption. Do not infer
global storage health from one database's status, or force an unhealthy report
to green by suppressing a problem.

Create is idempotent (`CREATED` or `EXISTS`) without loading an existing database.
For paginated listing, identity-preserving rename, retirement and separate bounded
reclamation, see [database lifecycle](02_database_lifecycle.md). Destructive lifecycle
commands preview by default and require the previewed lineage when applying a
name-based action; they never erase the entire PostgreSQL catalog.

## Back up and verify

```sh
edb backup --database application --repository /private/backups/application
edb list-backups --repository /private/backups/application
edb verify-backup --repository /private/backups/application --basis 42 --generation 0
```

Use a private repository owned by the process user. Backup checks paths and
permissions, claims the repository for one lineage, and publishes a point only
after its immutable object closure is durable. Individual files are indivisibly
published without overwriting existing content; there is no repository-wide
SQL transaction or generation ledger. Do not mix lineages in one repository.
Repeating capture authenticates and reuses unchanged objects. The reported
write/reuse counts describe object operations, not saved network round trips.
Capture reads one immutable live publication while ordinary writers continue.
Copy success is not semantic verification. Media encryption and repository
retention remain deployment responsibilities.

[Direct backup reads](05_backup_reads.md) expose fixed database/log values and EDN
query/Pull/preview commands without restore. Each point links one canonical
publication with its existing indexes and log tail. Capture copies its objects;
offline readers handle the recent tail and index readiness as live peers do.
Indexing and search preparation remain explicit live maintenance operations.

`backup`, `restore` and `gc` accept `--maintenance-pause-ms N` (default 0).
Capture paces at 128-object traversal boundaries and phase completion; restore
paces after a persisted batch of at most 64 traversal/copy steps, not 64
transactions or a fixed byte count. One admitted object may be large. GC paces
after a committed collector call. `MAINTENANCE` reports observed batches,
pauses and time; cancellation is cooperative between safe work boundaries.

`list-backups` prints exact basis/generation pairs and manifest hashes. Generation
matters when excision produces different physical information at one basis;
verification/restore never silently select a different generation. Both zero
basis and zero generation are valid when listed.

Verification is offline and deep by default. It authenticates the object graph,
checks log skip consistency and receipt/provenance coordinates, and compares
retained covering trees and search records with canonical replay. Allocation
frontiers, no-history omissions and partial AVET copies are checked explicitly.
`--presence-only` checks authenticated reachability, including program
dependencies, and prints `semantic=false`; it does not prove semantic agreement.
Neither command changes repository contents. Deep verification can use resources
proportional to retained history and indexes; it is not an ordinary read.

## Restore into a separately selected destination

For this command, `EDB_POSTGRES_URL` is the **destination connection**. The
repository is the source; the live source need not be reachable. Explicitly
install the current opaque-object namespace first. Stop the target writer before
applying a restore; target, lease and catalog guards reject a concurrent change.
Existing readers remain subject to the configured storage retention grace. The repository must
remain an operator-controlled immutable source throughout verification/copy.

```sh
# First select the destination credentials/transport in your environment.
edb install
edb restore --repository /private/backups/application --basis 42 --generation 0 \
  --target-database recovered --postgres-database edb_recovery --catalog-schema public

# The same exact selection, with explicit activation authority:
edb restore --repository /private/backups/application --basis 42 --generation 0 \
  --target-database recovered --postgres-database edb_recovery --catalog-schema public --apply
edb inspect --database recovered
```

`--postgres-database` names PostgreSQL's database, `--catalog-schema` names the
namespace containing the installed `edb_objects` and `edb_refs` tables, and
`--target-database` names the logical EDB database. The first two names must
match the configured destination; this protects against a wrong connection or
an earlier empty schema in `search_path`. No SQL identifiers are interpolated
from these flags.

Without `--apply`, restore authenticates the selected object graph and observes
the target catalog. It does not stage, create or activate a logical database;
the preview explicitly leaves target-lineage compatibility to the apply-time
guard. Apply authenticates objects as it copies them, persists an inactive
resumable checkpoint, and conditionally activates the exact point. Semantic
replay is available through the separate `verify-backup` command. A new target name
can be visible while its database is still unavailable (`backup/database-restoring`).
Apply rejects an unrelated existing lineage, a retreating/incompatible log, or
loss of acknowledged request identities. It cannot duplicate an active lineage
under a second name in the same namespace.
Use an independent PostgreSQL database/catalog for a simultaneous copy of a live
source. Restoring a retired lineage creates a fresh route; previously retired
handles do not become valid again. Immutable lineage and transaction identities
are preserved rather than rewritten onto that route.

An interrupted/ambiguous restore may have durably staged or activated work.
Retry the same repository/basis/generation/destination; do not manually publish
staged heads or assume process failure rolled back earlier commits. The library
resumes persisted postorder object-copy steps. Only child-complete objects become
strongly owned by the checkpoint; source objects not yet copied are weak pending
identities. Publication and restore completion are one guarded indivisible change.
An exact completed retry returns its recorded activated root without replaying
history, rolling back a newer target head or rerunning hooks. The library returns
`RestoreResult { point, target, activated_root }`; open a peer to read the result.
After success, open new clients and test the application's current/history/query
and exact transaction retry workflow.

## Explicit derived-index maintenance

```sh
edb consolidate --database application
edb fulltext-rebuild --database application
```

`consolidate` advances a captured transaction prefix using the same index/search
preparation and guarded publication as the background worker. Projection work
is completed in bounded steps; the explicit call can perform multiple steps.
If current indexes are damaged or missing, this operator-only path can replay
the authenticated canonical log to reconstruct them. That recovery may retain
an eager database and one index's datom vector; ordinary startup/read paths
never perform this fallback. Existing receipts and index authorizations are
preserved, so current-index recovery does **not** certify or repair every
historical retained index. Use deep inspection to assess those separately.

`fulltext-rebuild` builds search afresh from the current indexed history without
reading the old search attachment. It does not advance unindexed transactions:
consolidate/request indexing first when that is intended. Canonical and search
roots are published together; a failed preparation publishes neither.

For a diagnosed corrupt search projection, select the exact current index
descriptor and authorize replacing its search attachment:

```sh
edb fulltext-rebuild --database application \
  --discard-manifest EXACT_64_HEX_INDEX_DESCRIPTOR --apply --batches 1
```

Obtain the descriptor from `INDEXED descriptor=...` or
`FULLTEXT_REBUILT source_manifest=...`. The supplied digest is checked before
preparation and against the publication being replaced. A competing index
publication reports a target conflict instead of repairing another index.
Ordinary transactions may continue: publication preserves newer log, receipt,
metadata and writer-epoch fields and guards both root and lease revisions.
The background worker discards and recaptures a candidate superseded by an
operator. No second writer lease or transaction evaluator is introduced.

The `--discard-manifest` spelling remains an explicit target guard, but no
physical discard phase is needed: one rebuild satisfies `--batches 1`.
Canonical facts/history and retained old search objects are untouched. Unowned
derived objects become eligible only for ordinary GC; this is neither a
privacy-erasure guarantee nor a rollback of committed data. Cancellation and
repeated publication conflicts return an error without weakening these guards.

## Catalog-wide garbage collection

```sh
edb gc --postgres-database edb_storage --catalog-schema public --older-than-seconds 2592000
edb gc --postgres-database edb_storage --catalog-schema public --older-than-seconds 2592000 \
  --apply --batches 3
```

GC operates on the entire shared EDB catalog, **not one logical database**.
The explicit physical target names are verified before collection. Preview is
the default and changes no ownership or payloads. Apply recomputes/checks its
own transactional candidates; a separate preview is not a frozen deletion list
and may legitimately differ after concurrent activity.

Retention is mandatory and measured as an age in seconds. Thirty days is the
documented normal margin; a shorter age (including zero for controlled imports
or isolated tests) is allowed and reported as `short_retention=true`, not silently
substituted. Snapshots do not register pins; retention must cover active read
and copy durations. A shorter age can invalidate held snapshots. It does
not erase already observed values or live receipt-owned information.

`--batches` is a positive finite number, default 1, and applies only with
`--apply`. Each existing collector call limits victim/ownership work (typically
512 items for node phases, with separate fixed limits for other owners).
Candidate searches can still scan large catalogs; this is not a total-memory,
SQL-row or wall-clock bound. See the measured caveats in
[operations.md](00_deployment.md#inspection-retention-and-reclamation).

Progress includes receipt-archive conversion and log-generation work even when
no physical bytes are deleted. A completed requested batch count does not prove
global quiescence; output says `global_quiescence=not-established`. Repeat under
the same retention policy as needed and inspect the relevant owner state. A
signal or connection failure can leave already committed batches; retrying
continues from durable state, never a command-local cursor. Removed unreferenced
physical values are not recoverable through an undo command; retained backups
and authoritative facts provide the supported recovery path.

## Evidence and process output

Commands emit coarse `PROGRESS` phase starts, completion receipts and per-batch
counts. They do not estimate an unmeasured percentage or hide a long internal
verify/scan. CLI errors include category/code and return nonzero; no failed
observation implies rollback of earlier committed work. Success with explicitly
pending search repair or completed bounded GC batches is not whole-system
completion.

Exercise fresh installation, namespace isolation, inspection, collection,
restricted storage roles, backup verification, interrupted retry and
post-restore reads on explicitly disposable targets. The product has no SQL
fault triggers.
