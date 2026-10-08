# Ordered indexes and transaction log review

Reviewed `docs-2/06_indexes/00_indexes_and_log.md`, NORTHSTAR, TIMESTAMP_SOURCE_DESIGN,
CUTOVER, and all three supplied talks. The page already describes six complete
datom orders, shared current visibility, paired AVET/SAVET readiness, and source
activity composed from SAEVT timestamps and a captured log. The bounded question
was whether those access paths implement that model, particularly when another
source retracts a selected contribution. No gap was found in the page's named
DatabaseValue/log APIs. Leave the aligned documentation unchanged; optional
clarifications would add surface without changing behavior. A separate activity
tree would duplicate existing index/log responsibilities and their publication
coordination. Retaining the existing composition keeps ordering, visibility,
projection readiness, and log retention independently understandable and permits
physical implementation changes behind the same data APIs.

Acceptance evidence inspected:

- `src/model/datom.rs::cmp_in` retains E/A/V/T/S/bool, implements all six orders,
  descending T, true-before-false, and stored-value ties. `src/index/eager.rs`
  derives SAVET from AVET eligibility; `src/storage/descriptors.rs` requires twelve
  current/history trees. `src/storage/indexing.rs::advance_projection` applies
  each bounded chunk to AVET/SAVET together and clears their single readiness
  marker after both current/history projections complete.
- `src/index/visibility.rs::current_eav_group`,
  `src/database_value/views.rs::visible_current`, and cursor/boundary/resolve
  paths resolve complete EAV history before since/predicates/source selection
  and seek boundaries. Qualified durable AVET/SAVET prefixes share
  `src/storage/snapshot.rs::ensure_avet_ready`; schema-aware boundaries share
  `resolve_ready_avet_attribute`. Cursor errors terminate traversal.
- `src/pull/index.rs` retains one projection per selected datom, datom offsets,
  the attribute fence, unlimited default, zero-limit validation, and shared
  cancellation. Ordinary projected Pull values are separately deduplicated.
- `src/database_value/log.rs`, `reference.rs::log_value`, and connection/peer
  captures preserve one endpoint; bounds are inclusive/exclusive and payload
  traversal authenticates one transaction at a time. Index noHistory omission
  does not rewrite recorded transaction payloads; excision explicitly rewrites
  retained log generations in `src/storage/excision.rs`. No ST/STEAV structure
  appears in `src/`.

Reused actual proof, without rerunning unchanged suites:
`tests-integrated-fourth.log` records 150 passed, 0 failed, 14 explicitly ignored
live tests. Relevant passing checks include six-tree codec/descriptor rejection,
recent/global visibility, durable all-order current/history merging, noHistory
withdrawal witnesses, bounded paired backfill/clear, all boundary arities, temporal
and predicate visibility, query source ranges, and index Pull multiplicity.
`log-api-live-root.log` records **2 passed, 0 failed, 0 ignored** for
`tests/log_source_cutover.rs`: tied timestamps, empty/redundant transactions,
cross-source withdrawal, reverse transaction pagination, captured endpoint after a
later commit, same-basis `tx_data`, range bounds, six-field query bindings, and
lazy cursor statistics. `doc-link-check.json` records 1,563 links, zero issues.
Those checks cover the unchanged overview behavior.

Further review identified an adjacent public API gap:
`PeerSnapshot::range_cursor` uses `ensure_order`, which special-cases AVET only.
The cause is the surviving four-index gate, allowing SAVET to bypass shared
readiness. The bounded problem is raw current/history range/seek/datoms parity
during pending projection. Applying the existing gate to both orders preserves
the raw API behavior; changing raw AVET to omission would alter its contract,
and adding another readiness state would duplicate publication policy. Root
authorized the narrow `src/peer/snapshot.rs` change. The selected implementation
uses one matches expression and a paired error message. Acceptance: reproduce
pending SAVET acceptance with the regression before fixing; both orders reject
all raw range/seek conveniences while pending, ordinary DatabaseValue scans
omit pending facts, completion returns equivalent attributed projections, and
retained pending snapshots do not gain readiness from later publication.

Implemented the shared AVET/SAVET gate and message in `src/peer/snapshot.rs`,
preserving `peer/avet-not-ready`. Added the portable, explicitly ignored
`tests/peer_snapshot_readiness_source_cutover.rs`; it requires
`EDB_TEST_POSTGRES_URL`, uses a unique logical database, and drives an ordinary
manual writer/operator publication without scheduling races. The actual live
before run failed with `pending Savet raw range was accepted, history=false`
after AVET rejected the same basis (`peer-snapshot-readiness-before.log`).
The fixed actual live run passed **1 test, 0 failures, 0 ignored**
(`peer-snapshot-readiness-after.log`). It checks current/history raw
range_cursor/seek/range/datoms for both orders, DatabaseValue pending omission,
paired complete projections with both original source contributions, and old
snapshot readiness after completion. No remaining gap identified; root owns
combined release checks. The assigned page and historical report remain unchanged.
Focused `cargo clippy --test peer_snapshot_readiness_source_cutover -- -D warnings`
passed (`peer-snapshot-readiness-clippy.log`); the owned diff passes `git diff --check`.
