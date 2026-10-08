# Local SSD cache review

The assigned page already separates immutable-byte reuse from database authority,
captured-value retention, decoded RAM policy and authoritative PostgreSQL writes.
It matches NORTHSTAR and needs no prose change. `src/ssd_cache.rs`, connection
environment/configuration, and `src/storage/snapshot.rs` implement its private
directory, cooperative limit, framed/hash-authenticated entry, access/lineage/
generation namespace, and fallback contract. No production gap is identified.

The remaining evidence gap is SSD-specific: the inspected source-index I/O test
and current application harness use decoded RAM but do not enable SSD; existing
passing suite logs therefore do not prove disk restart reuse or cache authority
boundaries. Bound the work to a new `tests/local_cache_source_cutover.rs`.
Reusing unrelated passing tests would leave that gap; adding adapters or changing
the established cache design would entangle acceleration with authority. Select
focused tests of the existing public cache and an ignored, explicitly configured
isolated PostgreSQL test. Keep bytes/cache policy independent of datom semantics;
verify the shared snapshot reader composes them without a second read model.

Acceptance: offline checks exercise authenticated admission/read, reopen warmth,
namespace-isolated purge with shared occupancy/limits, payload corruption versus
preserved malformed framing, and private-root refusal. Live acceptance writes
source-bearing facts, consolidates six indexes, captures and scans current/history
source indexes, reopens with the same cache, sees SSD hits/reduced block reads,
and proves populated disk entries cannot authorize a new peer with unavailable
PostgreSQL. Reuse integrated kind14 live evidence for unchanged backup/recovery,
and the existing link-check result for unchanged links. Parent owns final QA.

Completed: only `tests/local_cache_source_cutover.rs` added. The page and all
production code remain unchanged. Three offline tests pass in
`local-cache-offline.log`; the portable ignored live test actually passes in
`local-cache-live.log` (1 pass), using the supplied disposable PostgreSQL and a
unique logical database. `local-cache-clippy.log` passes focused strict Clippy.
Fixtures explicitly create private mode-0700 roots outside the group-writable
implementation directory. Tests ran escalated because sandbox projects `/` and
`/tmp` ownership as nobody; the real ancestors are root-owned. Initial fixture/
instrumentation assertions were corrected, not production safety rules.

The live check verifies both same-value source contributions in current/history
SAEVT/SAVET after peer replacement with decoded RAM admission disabled. Operation
diagnostics show LocalDisk hits and no PostgresBlock accesses; lazy cursor SQL
reads remain zero. Changing only raw connection application-name parameters
creates cache misses/new files; its generation purge preserves every original
access-namespace file and subsequent original-reader disk-only reads. An
unavailable PostgreSQL endpoint cannot open a new peer despite a populated root.
Cold opening can itself hit files admitted earlier in that same opening; the
test correctly uses misses/new files to prove isolation rather than assuming
the whole opening has zero hits.

Relevant integration paths inspected: `PostgresConnectionConfig::ssd_access_namespace`
length-frames raw connection parameters, transport and trust roots;
`BlockNodeSource::cache_namespace` adds database identity and excision generation;
`BlockReader::capture_reference/capture_immutable` authenticates new live roots
through PostgreSQL before scoped reuse; `BlockNodeSource::read_with_stats/load_node`
provides RAM/disk/PostgreSQL ordering, authentication and write-error fallback.
The transactor authority and `operations/index.rs` use this same BlockReader.
Directory validation, inventory, staging, eviction and purge are confined to
`ssd_cache.rs`; neither fact-source semantics nor publication authority depends
on cache membership. The native tree/log decoders retain format rejection after
cache hash authentication. This keeps local reasoning about cache safety and
capacity separate from the shared datom model, and leaves byte representation
or cache policy changeable without another query/authorization path.

Unchanged behavior evidence inspected: `live-integrated-kind14.log` includes
actual source index I/O, backup, recovery and other live passes;
`backup-common-integrity-live.log` and `recovery-common-integrity-live.log` each
record 1 pass; `app-current-harness.log` confirms current local/TLS/retry/standby/
offline workflows. These establish their own boundaries, not SSD-specific proof.
The unchanged navigation reuses `doc-link-check.json` (1563 links, zero issues).
No identified production/documentation gaps remain for this page.
