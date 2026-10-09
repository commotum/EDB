# Persistent native log cache review

The page already matches NORTHSTAR and uniform kind14 metadata/body chunks; leave
it unchanged. Inspection finds no production gap: `database_value/log.rs` captures
one immutable snapshot and increments successful logical payload/cursor counters;
`storage/log.rs` authenticates page membership, metadata and chunk identities with
one transaction plus retained forward anchors; `storage/snapshot.rs` measures each
read through the same generation-scoped object reader used by trees. PostgreSQL
canonical bytes include page/envelope bytes, full-hit status requires all requested
objects from SSD, and repositories deliberately report neither PostgreSQL nor SSD.
`ssd_cache.rs`, connection namespace and peer purge preserve the separate policy
and authority boundary. Captured endpoints/GC grace do not depend on cache presence.

Existing actual evidence: `change-consumers-uniform-live.log` passes consumer2,
recovery1 and log2 against kind14; `ha-current-workflow.log` passes crash recovery,
held old values/logs and six-index fingerprints; `app-current-harness.log` passes
local/TLS/retry/standby/offline. Those runs do not establish SSD log lookup behavior.
Bound the remaining work to `tests/log_cache_source_cutover.rs`, owned with root
approval. Coordinate generic cache policy evidence with the local-cache worker.

Select focused public-path live tests, explicitly configured and ignored without
disposable PostgreSQL: cold/warm/reopened-peer kind14 log iteration, lazy integer
bounds and successful counters, source-bearing data/captured endpoint, and mixed
metadata/body admission. This exercises composition of independent cache policy,
authenticated log navigation and attribution without another storage abstraction
or adapters. Assert complete hits eliminate lookup SQL; compare identical results
when a body cannot be admitted while metadata remains reusable. Parent owns broad
QA/integration; append actual results before completion.

Actual changes: added only `tests/log_cache_source_cutover.rs` (two portable ignored
live checks requiring EDB_TEST_POSTGRES_URL; private mode0700 temp roots). No code or
page edits. `cargo test --test log_cache_source_cutover --no-run` compiled; actual
isolated live run passed2/2 in `log-cache-source-live.log`; strict focused Clippy
passed in `log-cache-source-clippy.log`. Across130 source-attributed transactions
and3pages, cold iteration fetched23724 canonical PostgreSQL bytes/262SQL calls,
versus10140 successfully decoded transaction bytes; warm and new-peer iterations
made0SQL calls, reported130 complete hits, and returned identical data. Captured
old log remained at130 after transaction131. Warm tx_data also made0SQL calls.
A varied128KiB body with32KiB physical capacity kept metadata warm while rejecting
its chunk: repeat lookup dropped2→1SQL calls and131256→131164PostgreSQL bytes,
returned identical source98 datoms, and correctly reported0 complete cache hits.

Inspected coordinated proof, rather than relying on future work:
`local-cache-offline.log` passes3 with1explicit live ignore; `local-cache-live.log`
actually passes1. These cover shared namespace/capacity/purge, corruption versus
preserved malformed framing, unsafe roots, source-index disk restart, access
namespace isolation and unavailable-authority startup refusal. Reuse root's
unchanged-link check. No remaining page-specific implementation gap identified.
The log's ObjectReader boundary leaves authentication/navigation unchanged when
cache policy or backend changes; policy operates on immutable bytes, attribution
on canonical datoms, and the public cursor retains the captured endpoint. Tests
exercise that composition directly without a parallel read engine or compatibility
path. Root retains complete-suite/release QA ownership.
