# Deployment reference review

Observed: the page separates storage access, writer authority, immutable peer
values, exact retry, maintenance completion and build compatibility. Those
operational boundaries already agree with NORTHSTAR. Five references describe
AVET completion alone although SAVET now shares that same eligibility/readiness
frontier. Root confirmed the minimal paired wording; no new API or metric name
is required.

Bounded problem: make the existing completion guarantees describe both value
projections and verify discovery, observer, process-failure and explicit-recovery
behavior remains valid for source-bearing requests and six-order stored data.
The cause is pre-source wording, not a second readiness mechanism. Retaining the
old wording would obscure SAVET; introducing independent SAVET waits would
couple maintenance to redundant state. Select AVET/SAVET wording and reuse the
existing shared frontier, canonical replay, fencing and exact receipt paths.

Acceptance: inspect runtime/health/standby, Connection/Peer observation and waits,
remote route retry provenance, canonical format rejection, explicit index
recovery and restore guards; inspect actual isolated crash/takeover and latest
kind14 live logs; change only the assigned page; diff whitespace check. Reuse
passing unchanged-behavior evidence, with no broad suite rerun or new fixture.

Simplicity: source attribution remains ordinary immutable datom information,
independent of transport, lease epoch and process policy. Captured values and
exact request content permit local reasoning; the shared projection frontier
composes with existing synchronization without new state or adapters. Build
replacement retains an explicit format boundary and avoids compatibility paths.


## Changes and evidence

Changed only five necessary AVET/SAVET phrases in the assigned page: catch-up
readiness, sync_schema, sync_index, partial maintenance and uniqueness readiness.
Preserved actual API names and all failure, retention and format promises.

Inspected implementation paths:
- application/connection.rs: resident immutable db capture, optional retained
  embedded service, same-lineage attach, notification reconnect/periodic durable
  catch-up, local condition-variable wait and separate observation errors.
- peer/live.rs: global whole-value publication, unavailable reconnect-once,
  complete exact report-batch validation before publication, contiguous direct
  adoption, one optional report queue and target-relative projection waits.
- transactor/authority/mod.rs, writer.rs, standby.rs, service.rs: stable identity,
  conditional root/lease claim, exact-token recovery, epoch fencing, canonical
  receipt lookup before fresh source/time resolution, renewal failure admission
  stop, queued-work draining and one-shot activated service handoff.
- bin/edb/runtime.rs and health.rs: documented 5000/1000/250ms defaults,
  STANDBY/READY lifecycle, unhealthy listener/service terminal failure, sampled
  local health phase, two workers/eight sockets and bounded request handling.
- transport/remote/{routing,client}.rs and local/endpoint.rs: authenticated
  route/lineage/lease/listener matching; local attempted-write provenance limits
  rediscovery retry to pre-transmission errors; confirmed remote commit keeps
  report errors separate; owned 0700 directory and recognized stale-socket
  reclamation.
- storage/indexing.rs: one bounded datom witness updates both AVET/SAVET before
  shared pending_avet/avet_work completion advances. index_recovery.rs replays
  canonical source-bearing log into twelve trees; ordinary open has no repair.
- encoding/mod.rs, encoding/submission.rs, index/tree/mod.rs,
  storage/{descriptors,log,postgres}.rs: current-version/shape rejection;
  no old-format source fabrication or alternate readers. backup/restore.rs
  validates lineage, nonretreating basis/generation, log prefix and exact receipt
  occupancy before activation over a prior publication.

Actual existing evidence inspected, not newly rerun:
- live-integrated-kind14.log:21 actual live passes,0failures, including exact
  independent report queue, immutable/local causal waits, paired raw snapshot
  readiness, explicit source-aware recovery, portable backup/restore through
  partial twelve-tree publications, CLI restart retries and source activity/log.
  Relevant test assertions were inspected directly in synchronization_cutover,
  processing_transactions_cutover, peer_snapshot_readiness_source_cutover and
  deployment_source_recovery.
- application-standby-takeover.log: successor runs all application markers,
  exact receipt replay, current/history attribution and paired indexing through
  basis11; doc_application_workflow records independently observed waiting and
  active health responses and signal shutdown. application local/TLS restart
  and writer-offline reference logs substantiate transport/immutable-read
  boundaries; these older runs predate kind14 and are historical workflow proof,
  not fresh current-format release validation.
- ha-restart-workflow.log and ha-postgres-crash.log: actual second-instance
  immediate PostgreSQL crash/WAL recovery, six-order current/history fingerprints,
  deferred-source receipt replay, changed-source conflict and cross-source
  replacement pass. That older run also predates kind14; root owns required
  final release reruns. It does not prove replication, region failover or a
  rolling mixed-build deployment.
- Root doc-link-check.json records1563 local links/heading fragments,0issues;
  this edit changes no links or headings.

No page-specific runtime gap found. No new implementation/test mechanism is
needed for these unchanged operating behaviors; existing shared paths carry
first-class source without deployment-specific state. Root remains responsible
for current-revision combined release checks and any required older-example
reruns. No production/shared PostgreSQL manipulation, global GC, commits or
formatting was performed.

Final focused validation: assigned-file git diff --check passed. Only the five
paired-projection replacements appear in its diff; links/headings unchanged.
