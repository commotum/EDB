# PostgreSQL operations

Atomic's Rust engine owns transaction assessment, immutable indexes, exact
receipts, writer fencing, catalog lifecycle and maintenance. PostgreSQL stores
opaque content-addressed objects and guarded references in two tables,
`atomic_objects` and `atomic_refs`; it does not execute application semantics.

This guide covers operator guarantees and limits. See [administration](01_administration.md)
for executable commands and explicit preview/apply controls. No
deployment-independent throughput, recovery time or network-outage bound is
implied.

## Provisioning and current storage

Provision a PostgreSQL database and empty schema through your normal
administration. Use an object-owning administrative account to run
`atomic install`, or `storage::PgBlockStore::install(&config)`. Repeating
installation on a matching current store is idempotent. Installation does not
create the PostgreSQL database/schema, reset existing data, or upgrade an earlier
development engine. Unsupported storage formats fail explicitly; choose a fresh
empty target.

Create distinct dedicated LOGIN roles, then use
`atomic install --writer-role atomic_writer --peer-role atomic_peer`.
Installation and grants are separate committed operations: `INSTALLED` can
precede a failed grant. Correct the roles and rerun the command. Runtime
constructors validate the current storage and perform no DDL.

Writers can read, insert and protect immutable objects and maintain references.
Peers read objects and references; checkpoint consumers additionally need
reference-write authority. Ordinary immutable reads perform no SQL writes. Only administrative
credentials receive object-deletion authority from this helper. These are
**trusted storage credentials**, not per-database SQL authorization. The helper
rejects elevated roles and storage owners but does not audit or revoke existing
grants or inherited access. Keep runtime roles separate from administration,
and use a separate installation or an application authorization boundary when
storage clients must not trust one another.

Catalog rename and retirement require the current SQL principal's object
`DELETE` privilege, even though their immediate mutation is a reference update.
The Rust administrative APIs enforce this capability; runtime writer and peer
grants do not satisfy it. This is not a security boundary against a client using
trusted reference credentials to issue arbitrary raw SQL or reference CAS.

Use `atomic create --database NAME` for idempotent logical database creation.
Creation publishes the initial read indexes; ordinary startup does not require
a consolidation step. Names resolve once to a stable route and lineage when
opening a handle. Rename changes the public mapping, not durable identity.
Retirement fences that route; name reuse creates a fresh route and lineage.
See [database lifecycle](02_database_lifecycle.md).

Ordinary reads authenticate the selected immutable roots and loaded objects,
but do not replay the entire canonical log to prove every derived fact.
Authorized Rust publication supplies that semantic trust. Deep inspection and
backup verification additionally compare derived information with the log.

### Explicit index recovery

`atomic consolidate --database NAME` consolidates current indexes and can
explicitly reconstruct a damaged or missing **current** read index from the
canonical log. This exceptional recovery can use time and memory proportional
to the database. Normal peer/service startup fails clearly on damaged indexes;
it does not silently perform an eager recovery.

Rust operators can use `PostgresOperator::with_tree_config(TreeConfig)` to set
validated physical construction limits for consolidation and current-index
recovery. Untouched nodes remain shared; this is not a full repartitioning pass
or a change to the independent fulltext builder's limits.

The operator preserves exact receipts and retained read authorization.
Reconstructing today's index does not repair or certify every retained historical
index. Run deep inspection afterward; if retained history is damaged, recover
from an intact backup. `atomic fulltext-rebuild` replaces the derived search
attachment through guarded publication. It does not edit canonical facts or
erase already copied reader data. Cold old-value reads remain subject to retention.

### Authoritative object compression

Opaque object storage chooses raw bytes or a versioned gzip envelope when the
latter is smaller. The single stored representation authenticates against the
canonical SHA-256; it is not an optional SQL projection beside a second raw copy.
Declared sizes, decompression output, complete input consumption and canonical
hash are checked. A corrupt authoritative object fails clearly; there is no
hidden raw fallback. The canonical per-object limit is 64 MiB, independent of
whether compression makes its physical representation small.

`node_block_read_stats()` separates canonical/physical node payload bytes,
compressed/raw reads and decode time. `PgBlockStore::object_read_stats()` gives
generic per-connection object counts. These measure transferred payloads, not
PostgreSQL disk allocation, WAL, network framing or whole-process memory.
Backup repositories contain canonical objects, independent of storage compression.

## Transport and failure policy

`PostgresConnectionConfig::parse` and the string convenience constructors use
the DSN as the transport authority. TCP defaults to verified TLS 1.2 or newer;
Unix sockets are local, and `sslmode=disable` explicitly selects plaintext.
The `require_tls` and `plaintext` builders reject conflicting DSN settings.
Certificate and hostname verification remain mandatory; private trust roots can
be added explicitly. `ATOMIC_POSTGRES_TRANSPORT` is no longer accepted.

`TransactionServiceConfig.connection` holds this typed configuration. Services
and standbys accept that one configuration, shared by their writer, indexer and
reader connections; there is no separate connection argument or ignored DSN.

Choose `PostgresIoPolicy` per role. Defaults preserve driver/server settings;
there is no hidden universal production timeout. For example, a runtime policy
can set statement and lock deadlines while a separate administrative policy
allows longer backup/index work. All durations must be positive. The policy
is shared by configured plaintext and TLS connections and preserves unrelated
startup options.

| Setting | What it bounds | What it does not bound |
| --- | --- | --- |
| `statement_timeout` | Each server-side SQL statement | Entire transaction, local query, arbitrary Rust callback |
| `lock_timeout` | Each server-side lock wait | All work surrounding that wait |
| `connect_timeout` | Each socket/address attempt; minimum of configured, policy and caller caps | DNS, TLS/auth/startup, all addresses combined |
| TCP user timeout/keepalives | OS-specific liveness behavior | A measured end-to-end outage SLA; Unix sockets |
| Query/Pull controls | Cooperative evaluator/interpreter/projection work | Preempting arbitrary Rust or an already blocked SQL call |

SQL cancellation (`57014`) is Interrupted; lock timeout (`55P03`) is Busy.
These are not transport-loss claims. A delivery timeout or connection loss
after attempted submission can still be `UnknownOutcome`: reconcile/retry the
identical request key and content. Do not submit under a new key to resolve an
unknown result. A known commit remains committed if opening its local report
fails; the native socket result separates those facts. Report observation
failure is likewise separate from transaction success.

Do not log credentials, plaintext request keys or subject data. Use structured
categories and lineage-bound request digests for diagnostics. Configure process
supervision and measure failover in the target environment; peers retain
immutable database handles during writer unavailability, while cache misses
still require available storage. PostgreSQL HA/WAL shipping is a separate
deployment responsibility, not implemented by the Rust transaction lease.

Opt-in `atomic transactor --telemetry-ms 10000` publishes bounded JSON events to
stdout from the stock lifecycle loop. See [observability](03_observability.md) for
metrics, warning configuration, privacy and slow-sink/shutdown limits.

## Backup, verify and restore

`PortableBackup::backup_database` captures one immutable publication. Storage
retention must cover the copy duration. It does not hold a relational repeatable-read
snapshot or write source indexes. A private repository is claimed by one lineage;
content-addressed files are authenticated and atomically published without
overwriting existing files. The point is published only after its reachable
objects are durable. Atomic does not encrypt the repository; media encryption
and retention remain deployment responsibilities.

Each point links one canonical publication, preserving its existing indexes and
log tail. Capture does not run indexing or build search. Offline query/Pull/log
reads use the same tree readers and admitted recent-tail handling as live peers,
including pending-index readiness. No source database is needed. Earlier backup
formats are rejected rather than converted.

Copy success and unchanged-point reuse authenticate reachable immutable objects
and program dependencies; neither is a semantic proof. `verify_backup_presence`
and `verify_backup_point_presence` perform that weaker graph check.
`verify_backup(..., deep)` always replays the canonical log to return a verified
database value; `deep=true` additionally checks coherent log skip links and
compares every retained covering index and search attachment against canonical
information. Receipt before/after values must match exact canonical prefixes,
and metadata must match ordinary/reserved allocation checkpoints. Legitimate
no-history omissions and pending AVET prefixes are checked, not ignored.
This semantic audit is explicit; ordinary restore authenticates copied objects
and checks guarded publication without replaying history.

Deep verification is intentionally broad: it retains one replayed database,
small per-basis schedules and comparison datoms, not a database copy per receipt.
Search comparison uses the shared analyzer and bounded spill sorter. Whole-job
time, memory and temporary-disk usage still depend on retained history, receipts
and index count. `BackupVerification.objects_read` counts unique objects in the
authenticated closure, not every repeated physical read during comparison.

`list_backup_points` distinguishes physical generations at the same basis.
The explicit API order remains
`restore_backup_point(directory, basis_t, generation, target_name)`.
Install the target namespace and stop its writer before applying restore.
Existing readers remain subject to storage retention; target/lease/catalog guards prevent
stale activation. A new target is reserved as a headless route while copying,
so catalog discovery can precede a readable database. An unrelated lineage,
retreating or divergent log, or loss of acknowledged request identities is
rejected. Only one active route per lineage is allowed in a namespace. Restoring
a retired lineage uses a fresh route; retired handles never silently retarget.

Restore walks objects in postorder. A persisted checkpoint strongly owns only
completed child-closed copies; not-yet-copied identities are weak stack data.
Each batch advances at most 64 traversal/copy steps, with cooperative cancellation
and optional pacing after checkpoint publication. This is not a transaction,
byte, SQL-call or total-memory ceiling. No feature-specific PostgreSQL staging
tables, tree membership folds or log-generation activation procedures are used.
Final publication, lease fencing, completion and checkpoint removal use one
guarded reference batch. A concurrent target change fails closed.

Retry an interrupted or ambiguous restore with the same point and destination.
It resumes the durable checkpoint or recognizes the exact completion; a completed
retry returns its recorded activated root without replay, rolling back a newer
head or rerunning activation hooks. The result is `RestoreResult` with `point`,
`target` and `activated_root` fields; open a peer for database reads. Do not edit
work references manually. After success, open new clients and exercise cold
current/history/query/Pull and exact transaction retries. Retain independent
physical PostgreSQL backups/WAL under a separately tested recovery policy.

## Inspection, retention and reclamation

The current operator uses the Rust block engine. PostgreSQL stores opaque
immutable objects and guarded references; it does not interpret application
datoms or run feature-specific GC/excision procedures.
`PostgresOperator::inspect_database(database_id, deep)` captures one immutable
publication, using the stable catalog ID rather than resolving a name again.
Shallow inspection reports authenticated coordinates and index counts. Deep
inspection additionally walks reachable objects, replays the canonical log and
compares schemas, index projections and exact receipt coordinates. It is an
explicit broad operation, not a normal-read prerequisite or constant-memory
check. Inspect reported problems; successful decoding alone is not proof that
a derived index agrees with the log. Optional metrics such as transaction bytes,
request counts and reachable-object counts are `None` when not measured, not zero.

`garbage_inventory(age)` is read-only: it reports provider object/physical-byte
totals, pending ownership-event counts and the persisted collector phase. It
pages provider metadata and can inspect the whole namespace; it is not a cheap
prediction of the next deletion list. It neither starts nor advances a cycle.
Separate inventory and collection calls can observe concurrent activity.

`collect_garbage(age)` advances at most 4,096 graph/metadata/object work steps,
then persists a resumable checkpoint. Its counters describe that call's work:
ownership steps, newly owned objects and authenticated payload bytes, metadata
protection, objects examined/removed, physical bytes removed, ownership events
removed. Applied reports omit unmeasured whole-store
totals. The bound is not a wall-clock, SQL-statement or allocator-RSS ceiling;
one admitted object can still be up to the provider's 64 MiB canonical limit.
`MaintenanceControl` checks cancellation and optional pacing between safe
boundaries; PostgreSQL I/O policy bounds individual I/O calls.

The collector seals a protection epoch, folds added owners before removed
owners, protects its retained metadata, sweeps eligible objects and clears
completed events. Rust authenticates a newly owned object's children once and
retains that ownership information, including Function values in trees/logs and
fixed program dependencies. Unchanged shared subgraphs do not require a fresh
live-data traversal every cycle. Uncertain/missing/corrupt ownership evidence
fails closed. Root publication and protected work participate in the same guarded
ownership protocol; a losing writer or collector cannot publish stale progress.

The CLI defaults to one bounded apply call. `GC_APPLIED` reports work, while
`GC_PROGRESS cycle_complete=false` means invoke the same command again or choose
an explicit larger `--batches` count. A successful process exit or zero deleted
objects is not proof of completed cleanup. `cycle_complete=true` means only that
the sealed cycle finished: later publications, staging expiry or authorization
pruning can require another cycle. Reopening the operator resumes its checkpoint.
The cycle fixes its retirement cutoff when it begins; changing the supplied age
does not retroactively change an already running cycle.

### Retirement age and reader protection

`RECOMMENDED_GARBAGE_COLLECTION_AGE` is 30 days. Choose a deliberate retention
policy; zero age is for controlled disposable tests or imports, not the routine
recommendation. The minimum age applies after a **previously owned** object loses
its last published owner. It does not promise retention for never-published
uploads: an abandoned upload fenced by a newer protection epoch, or obsolete
collector scratch metadata, can be reclaimed regardless of that age. Active
protected uploads remain fenced against the current sweep.

Held database values, logs and cursors retain roots in memory and create no
reader pins or sessions. Capturing, cloning and dropping them perform no SQL
writes. Retention must cover active read and copy durations; an explicitly short
age can invalidate cold reads from a held value. Reopening authenticates the
current publication and may reject retired identities or old excision generations.

GC preserves objects still owned by current roots, exact receipts, protected work
or another database. In-memory reader ownership alone does not prevent collection
after the grace period. None of these controls erase backups, replicas, WAL,
application exports or already copied process memory.

## Excision

Excision is rare privacy/retention work, not correction; ordinary retraction
preserves the history needed to explain past decisions. Submit an ordinary
`:db/excise` request with optional selected attributes and one strict cutoff,
then establish completion with `sync_excise` or the connection's corresponding
synchronization. The stock service automatically schedules committed requests
with indexing and resumes interrupted jobs; a committed request is not itself
proof of completed removal. `PostgresOperator::process_excision_requests` remains
an explicit administrative driver for the same rewrite algorithm. It claims a
normal writer lease and reports Busy when a running service owns that lease;
let the service finish or stop it before administrative processing. Operator
methods take the catalog's stable `database_id`, not a mutable database name. Request facts
are protected audit information. Atomic freezes the complete predicate at its A=15 assertion;
later ordinary edits do not rewrite an already queued request.

The Rust worker rewrites an inactive generation into immutable objects, stages
all eight indexes and fulltext, then atomically activates the completed root.
A protected work reference retains bounded persisted checkpoints across worker
or process interruption. Large excisions can cost proportionally to the whole
database. After successful admission, the service parks fresh transactions for
the rewrite; lease renewal, receipt resolution and independent reads continue.
An old held database/log value remains an immutable copy: discard it and sync
controlled clients before claiming removal. New serialized opens of an older
generation are rejected. Reclamation follows durable ownership and retention.

Excision publishes reclaimable generation handoffs so lagging transaction-report
queues can recover original outcomes across a rewrite. Handoffs survive for the
configured GC grace period regardless of connected readers. Bounded pruning
releases expired handoffs; subsequent GC folds those ownership changes. Ordinary
snapshot reads do not consult handoffs. A same-route restore without the original
generation handoffs makes a lagging report queue fail explicitly with
`peer/report-history-unavailable`; disconnect and reopen it on restored state.

Every pre-excision idempotency key stays occupied by a tombstone, including keys
whose transactions did not touch the selected facts. Such retries return
`postgres/idempotency-predates-excision` without rerunning transaction functions.
The tombstones contain no original request digest, tempid name, transaction
payload or old value-root links. New requests retain ordinary exact-retry
behavior. This prevents both forgotten-key reinvocation and receipt-based
reopening of erased history.

`ServiceOptions::excision` sets the automatic worker's admission policy. The stock
flags are `--excision-max-bytes` (default 512 MiB accounted bytes) and
`--excision-log-batch` (default 256, range 1–4096 transactions). One resumable job
holds its state between cooperative steps; it does not rerun the whole rewrite
for each scheduling tick. Replay checks each transaction and tree uploads yield
in bounded batches. Each log batch accounts 1 MiB plus 64 times its canonical
source bytes; individual transaction admission precedes chunk loading. Predicate
discovery and index preparation also have admission checks. This is a policy
account, not measured RSS or a proven allocator/time ceiling. Planning and total
rewrite work can still be proportional to the database; configured driver
timeouts apply to I/O and cancellation is checked at safe boundaries.

Admission failure leaves the committed request pending and ordinary writes
available. The service exposes `background_indexing_stats().excision` and
`excision_failure`; the stock process emits a redacted
`TRANSACTOR_EXCISION_FAILURE` category/code rather than silently claiming success.
Increase the allowance deliberately and restart to resume, or run authorized
administrative maintenance. Progress includes checkpointed rewrite counts and
`fresh_write_pause_nanos`, the measured pause of the latest automatic activation
attempt (not a persisted total across restarts). No partially completed successor
is activated: completion evidence and the new generation become visible together.

The Rust engine validates predicates, immutable links and generation coordinates.
The generic object/ref provider checks publication and ownership CAS guards;
there are no excision-specific SQL tables, triggers or staging procedures.

Excision does not erase application exports, logs, old process memory, replicas,
PostgreSQL WAL or portable/physical backups. Apply the same retention decision
to those separately. A pre-excision backup is recommended for operational
recovery, but retaining it also retains the removed information; it is not an
authorization token or a physical-erasure guarantee. Fulltext is rebuilt from
the successor generation before activation; new current/history searches exclude excised facts,
while already-held authorized old values retain the same pre-excision policy.
Search copies in old values or backups are not magically erased; see the
[fulltext ownership and recovery policy](../05_query_and_pull/03_fulltext.md).
