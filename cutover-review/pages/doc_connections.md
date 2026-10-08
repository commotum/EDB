# Connections and captured reads review

Read `docs-2/07_peer_api/00_connections.md`, NORTHSTAR, TIMESTAMP_SOURCE_DESIGN,
CUTOVER, and the three supplied talks. The page describes an advancing connection
that yields immutable values, explicit startup authority, stable database identity,
distinct synchronization conditions, retained snapshot witnesses, and observation
queues. It contains no obsolete timestamp/source input or four-index assumption.
The bounded problem was whether source cutover preserves those connection
boundaries and captured results. Inspection found no page or implementation gap.

Decision: preserve the aligned page and existing mechanisms. Adding source
resolution to a connection would entangle read capture, request identity and writer
assessment, and make exact retries depend on current facts. An extra capture or
compatibility wrapper would duplicate the immutable value/reader path. The selected
implementation already carries `TransactionRequest.source: EntityRef` as data,
includes it in canonical identity, and resolves it during fresh assessment after
receipt handling. Existing argument order puts source after semantic data/time and
before resource options. This keeps request attribution, durable authority,
connection advancement, and captured-value evaluation locally understandable and
composable; writer/storage implementation changes need not alter read consumers.

Concrete acceptance paths inspected:

- `src/application/connection.rs::connect_configured` opens only a peer and
  observer; `src/peer/live.rs` resolves the public name to `BlockDatabase` once.
  `Connection::db` and `Peer::database_value` clone the resident captured value.
  Background notices trigger `sync_database_value`/`refresh_index`; they supply
  no commit facts. Later publication replaces peer state rather than retained
  snapshots. `Connection::sync_to` waits on local advancement; peer sync methods
  authenticate storage and independently test logical basis, paired AVET/SAVET
  readiness, and excision completion.
- `src/database_value/reference.rs::open_with_reader` authenticates route,
  lineage, excision generation, exact log endpoint and retained value witness;
  reopening does not advance the connection. `SnapshotKey` omits physical
  publication/cache allocation and is explicitly only database-input identity.
- `src/transactor/client.rs` canonicalizes the request without database source
  lookup. `src/transactor/authority/mod.rs::transact_captured` returns a matching
  receipt before fresh gates, clock selection or assessment;
  `src/transaction/assess/mod.rs` lowers deferred EDN and resolves source with
  ordinary entity/tempid/lookup rules. Read and transaction `:db/ident` lookups
  both use the captured dictionary, retaining aliases after explicit retraction.
- Connection queues delegate to the peer's unbounded `VecDeque`; enable/removal
  and replay suppression are separate from durable log delivery.
  `src/observation/consumer.rs` allows only one pending event and requires the
  delivered checkpoint for acknowledgment. `src/backup/snapshot.rs` opens a
  repository-backed `BlockSnapshot` and shares DatabaseValue/log evaluation.
  AsyncClient delegates blocking operations to the existing bounded executor;
  request transport and PostgreSQL read authority remain separate.

Actual proof reused, without rerunning unchanged suites: `tests-integrated-fourth.log`
records 150 passed, zero failures, 14 explicit live ignores.
`synchronization-live.log` records one actual pass, zero ignored: retained old
value, causal target, source-bearing replacement, zero SQL for `db`/local waits,
and expired queued async operations. `processing-transactions-live.log` records
one actual pass, zero ignored: exact attributed before/after reports, ordering,
replay suppression, queue removal/re-enable. `client-tutorial-live.log` records
one actual pass, zero ignored: synchronous/async socket submissions preserve six
slots, one sourced timestamp and exact retry; changed source conflicts. Inspected
the corresponding test bodies. Both `application-local-restart.log` and
`application-tls-restart.log` contain `SOURCE_OK`, `APPLICATION_OK` with accepted
seed/update retries, retained old values, and `PLANNING_OK`/`PARTITIONS_OK` exact
references. Root's local-link proof reports 1,563 links and zero issues.

No code, page, or test changes were necessary; no remaining gap identified in
this page. The adjacent raw PeerSnapshot SAVET readiness gap is already fixed by
`doc_indexes_log` with actual pending/complete/retained-pending live proof; reviewed
its note without modifying that worker's files. The historical report is untouched.
Root owns integrated release checks and integration; this note makes no broader
release-readiness claim.
