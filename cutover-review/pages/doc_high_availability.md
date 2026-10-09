# High availability review

Observed: the assigned HA page already separates writer authority, listener
publication, storage availability, immutable database values, activation and
application retry. Its retry identity includes transaction source. Existing
application local/TLS restart and standby-takeover logs exercise ordinary
source data, report replay and historical reads. The owned restart example
checks an immediate PostgreSQL crash but currently uses only the bootstrap
source and hashes EAVT; it has not been executed in this cutover.

Cause: crash acceptance predates first-class source and therefore does not
explicitly challenge restored source access paths or deferred request source
resolution. This is an evidence gap rather than a reason to soften the HA page.

Bounded problem: prove acknowledged six-coordinate facts, all six index orders,
retained values/log and exact source-bearing receipts survive crash/restart,
then prove a different contributor can replace the recovered shared value.

Alternatives: rerunning the already-passing application workflow would cover
transport takeover again but leave PostgreSQL crash untested. A new recovery
engine would entangle attribution with lease/lifecycle mechanisms. Extending
the existing public-API crash example reuses canonical datom encoding, native
bounded cursors and ordinary transaction source resolution.

Selected: use a tempid source established through ordinary facts in the
acknowledged transaction; fingerprint each current/history index independently;
check exact replay and rejection of changed source; validate timestamp/source
activity and successor retraction attribution. Run against a second disposable
PostgreSQL instance with fsync, synchronous_commit and full_page_writes on,
never the parent fixture or Course_Academy storage.

Design: source information remains independent of writer epoch and transport.
The example composes existing connection, lease, cursor and transaction APIs;
each fingerprint reasons about one immutable basis and one index order. No
adapter, special source recovery structure or format compatibility path is
introduced. Keeping the server lifecycle opt-in and isolated permits recovery
policy changes without changing attribution or query behavior. This applies
Design in Practice's situation/cause/problem/alternatives sequence, Simple Made
Easy's independence criterion and Simplicity Matters' ability-to-change test.

Acceptance: build the example; seed an indexed cardinality-one Long workload;
run real immediate crash/restart; compare source-bearing complete current and
history hashes for every index before/after and retained/as-of values; replay
same receipt without fresh resolution; reject changed source; verify recovered
SAEVT/SAVET and source timeline; replace through another source and preserve old
history; reopen independently; stop and clean up only this disposable fixture.

## Changes and results

The assigned HA page remains unchanged: its existing statements already agree
with NORTHSTAR and current source/retry/authority behavior. The implementation
change is confined to `examples/restart_workflow.rs`. The example now validates
its Long/cardinality-one fixture attribute, uses an ordinary tempid contributor
established in the acknowledged transaction, records twelve bounded fingerprints
(six orders x current/history) per phase, compares complete replay tx_data,
rejects a changed source under the same key, traverses recovered SAEVT timestamp
activity into its same-basis log, and checks cross-source retraction visibility.
Its original explicit disposable-server guards remain required.

Focused static validation:
- `cargo build --offline --example restart_workflow`: pass;
  `ha-restart-build.log`.
- `cargo clippy --offline --example restart_workflow -- -D warnings`: pass;
  `ha-restart-clippy.log`.
- Assigned-file `git diff --check`: pass.

Actual runtime acceptance:
- Own separate PostgreSQL15.11 instance: `/tmp/edb-cutover-ha-pg-20261007`,
  socket-only port55438, databasepostgres, logical nameha-crash-source. Explicit
  `EDB_RESTART_POSTGRES_URL`/`EDB_ALLOW_DISPOSABLE_PG_CRASH=1` and exact verified
  data-directory configuration selected this fixture; no shared/production
  resource was used. fsync/synchronous_commit/full_page_writes were all on.
- Temporary `ha-seed.rs`/`ha-seed` seeded256 rows with indexed Long attribute1000
  and reference attribute1001, two ordinary contributor entities, corroboration
  and cross-source replacement, then explicitly published durable indexing.
  `ha-seed.log`: basis8; EAVT/AEVT/SAEVT current671/history673;
  AVET/SAVET current323/history325; VAET344. Initial fixture-only attempt used
  noncontiguous fresh attribute1002 and was rejected before any database was
  created; corrected to ordinary contiguous1000/1001 before acceptance.
- `ha-restart-workflow.log`: exit0 and PASS. The example performed actual
  `pg_ctl -m immediate stop`, PostgreSQL WAL recovery, replacement activation
  (durable base8 plus one recent transaction through9), replay of the exact
  tempid-source request despite its now-stale basis condition, changed-source
  rejection, successor cross-source replacement through10 and independent reopen.
  All96 fingerprint records across eight phases and six current/history orders
  matched their required pre-crash/retained/as-of/reopen comparisons. Attribution
  includes the automatic timestamp. Source prefix after replacement is empty
  for the withdrawn original contributor. The source timeline identifies the
  correct log transaction at the same captured basis.
- Observed storage_restart_ms427; replacement_start_ms2678; base_t8; target_t9;
  tail_transactions1; tail_range_reads1. These are fixture observations, not HA
  guarantees. Native eager database values/history datoms remain0.
- `ha-postgres-crash.log` independently records immediate shutdown and automatic
  WAL recovery. `ha-pg-cleanup.log`: successful fast shutdown; verified no
  postmaster.pid before removing only the second fixture directory. The parent's
  shared PostgreSQL and Course_Academy/original EDB remain untouched.

Code parity traced through stock runtime config/READY/failure handling,
TransactionStandby's one-shot service handoff, authority's conditional root+lease
claim and exact-token acknowledgment recovery, epoch/token fencing, activation's
captured indexed state/log tail, receipt lookup before admission/source resolution,
source-aware submission encoding, remote route rediscovery only before attempted
transmission, and local immutable Connection::db. Existing local/TLS restart and
standby takeover logs substantiate unchanged transport behavior; the application
worker's note records actual standby readiness/lease takeover checks. Backup and
explicit recovery are shared existing mechanisms and remain separately covered
by their assigned workers; no HA-specific source gap or code dependency remains.

Scope limit: the crash fixture proves PostgreSQL restart durability and successor
writer activation; it does not simulate PostgreSQL replication, region failover,
network partitions or a rolling mixed-version deployment. The HA page explicitly
makes those guarantees conditional or absent. No format adapter/compatibility
path or dedicated source recovery mechanism was introduced. No commits made.
