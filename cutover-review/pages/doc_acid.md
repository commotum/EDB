# ACID review

Observed: the complete assigned page already agrees with NORTHSTAR and the
timestamp/source contract. It describes one shared atomic successor, receipt
identity, immutable captured reads, serial assessment, and PostgreSQL-dependent
durability. Source requires no ACID-specific path or explanatory rewrite.

Bounded problem: establish that these promises still apply to six-coordinate
facts, rather than inferring durability from speculative tests. No page-specific
implementation defect was found. The possible gap was evidence coverage, since
default tests intentionally ignore live storage checks.

Decision: retain the page and implementation, trace the actual publication and
observation boundaries, and inspect the existing passing live evidence. Another
broad crash exercise would duplicate the HA worker's completed immediate-crash
proof. A separate source publication or replay adapter would duplicate authority.
Acceptance is coherent source-bearing successors, unchanged captured values,
receipt-first retries after reference changes, six-index recovery, and the
documented separation of known commit from local observation.

## Code parity and evidence

- `src/transactor/authority/mod.rs:529–813` hashes source with forms, basis and
  time; checks an existing exact receipt before fresh gates, epoch, lease,
  capacity, clock and source resolution; assesses one predecessor; stages log,
  metadata, value descriptors and receipt; validates the successor after the
  upload barrier; then publishes root and renewed lease under captured guards.
  Receipts are indexed by request key and basis; value descriptors omit receipt
  and authority links. `src/storage/snapshot.rs` authenticates replay identity,
  digest, before/after coordinates and log transaction before returning a report.
- `src/transaction/pipeline.rs` shares assessment with speculation.
  `src/transaction/assess/mod.rs` resolves ordinary source once, adds the ordinary
  timestamp, applies global cardinality/uniqueness rules, and validates attribute
  predicates and explicitly requested ensures against the exact before/after
  pair. Default suite evidence includes rejection/panic immutability, atomic
  cardinality cleanup, distinct-value constraints and cross-source withdrawal.
- `src/storage/object_io/staging.rs`, `object_batch.rs`, `protocol.rs`,
  `postgres.rs`, and `ownership.rs` keep uploads independent from publication:
  128 objects/64 MiB maximum, 4 MiB pending target, sticky barrier failure,
  authentication/protection of reused bytes, sorted namespace-qualified advisory
  locks, READ COMMITTED, guarded changes and revisioned tombstones. SQL commit
  acknowledgment is awaited. Failed preparation can leave orphans, never a root.
- `src/transactor/writer.rs` consumes one fresh assessment/upload/publication at
  a time and reconciles ambiguity before maintenance or subsequent work.
  `src/transactor/client.rs:170–188` sends nonblocking bounded local hints.
  `src/peer/live.rs:352–439,686–746` atomically adopts whole snapshots, rejects
  basis regression and noncontiguous reports, and leaves held values immutable.
  `src/application/connection.rs:397–431,640–657` captures locally, waits for
  requested basis, and records observation timeout without rejecting a known
  commit. `src/storage/snapshot.rs:1310–1436` rejects incomplete captured tails.
- `src/storage/log.rs` retains 64-entry immutable pages, authenticated skip links
  and navigation anchors; read errors fuse traversal. `index_publication.rs`
  checks index lineage and authenticated log endpoint and retains current log,
  receipts and metadata when adopting a prepared projection.

Inspected actual results: `tests-integrated-fourth.log` passes 150 tests, 0 failures,
14 explicitly ignored live tests; `tests-durable.log` separately passes both
transaction-model live tests, including replay after ident/lookup withdrawal,
changed-source conflict, index publication, immutable old values and reopen.
`processing-transactions-live.log` passes its independent source-report queue
check. `deployment-source-recovery.log` passes explicit recovery.
`ha-restart-workflow.log` ends PASS after actual immediate PostgreSQL shutdown,
WAL recovery and writer activation: all six current/history fingerprints,
retained/as-of values, exact deferred-source replay, changed-source rejection and
cross-source successor replacement pass. `operations-workflow-fixed.log` reports
healthy deep integrity and completed backup/offline/restore acceptance.
`doc-link-check.json` records 1563 links with 0 issues.

No source, documentation or test changes were necessary; no new tests were run
or commits made. Existing staged publication, canonical datoms and ordinary
read/replay paths keep attribution independent of authority, peer progress and
physical indexing, permitting each concern to change without a second source
mechanism. No remaining ACID-specific parity gap or shared-edit dependency was
found. Runtime evidence does not claim replication/region failover, arbitrary
native callback rollback, or PostgreSQL durability with weaker configuration;
the page already states those boundaries. No global ownership/GC proof was
attempted on the shared disposable provider.
