# Source-aware immutable filters

Observed: the database-filter page already documents six-field history and distinct projected values. The read engine still collapses to one EAV winner in reverse views, forwards old assertions indiscriminately, and overlay merging treats two sources as one duplicate. Boundary resolvers accept four components and omit source-first orders.

Cause: current visibility and physical index ordering were coupled under the original single-contribution model. Source-first ordering splits an EAV across ranges, so collapsing only the selected source can miss another source's withdrawal.

Bounded problem: preserve immutable basis/time/filter behavior while exposing all live EAVS contributions, six boundary orders, and distinct values through value projection. Retractions must be evaluated over complete EAV history before source-index selection. NORTHSTAR is authoritative: arbitrary custom predicates cannot hide a retraction to restore withdrawn contributions. The earlier pre-collapse paragraph is revised.

Approaches: buffering whole indexes and sorting is straightforward but loses selective reads and bounded memory. Grouping the original four orders remains efficient but cannot resolve source-first visibility locally. A shared complete-EAV resolver plus incremental group caching separates visibility from order and composes with prefixes, reverse boundaries, temporal windows, and overlays.

Selected: reuse index::current_eav_group for complete EAV groups up to as_of; all window cursors probe/cache a complete EAVT group, independently of direction and boundary. Apply since and custom predicates to survivors. Preserve stored decimal representation equality. Extend boundary normalization naturally with source fifth on original orders and source first on SAEVT/SAVET. Extend overlay membership/backfill to SAVET and preserve same-value contributions from different sources.

Acceptance: focused in-memory tests for two same-value sources, third-source withdrawal, source-first prefix/current/as-of/since/history and forward/reverse boundaries, distinct values and overlay contributions. Run cargo check --lib --offline and focused tests once integrated compile permits; record failures without claiming parity.

## Changes and results

Implemented global complete-EAV visibility for temporal/custom forward and reverse windows through `current_eav_group`, keeping only the current group's survivor cache. `as_of` bounds its contributing history; `since`, arbitrary predicates, source prefixes and boundaries select survivors. Historical views retain raw event selection. Hidden EAV probes poll cancellation without supplying out-of-order range candidates.

Extended all original boundary normalizers to five components and added SAEVT/SAVET normalizers with ordinary entity/ident/lookup-ref resolution. `values()` projects distinct stored values; lookup still selects one distinct holder. Overlay current duplicates require equal source; EAV retractions still remove all sources. AVET/SAVET share schema-enablement backfill, including source-only SAVET prefixes. Backfill suppression now uses the shared source-distinct `EavsSet`. Prefix memo weights include source-first value payloads.

Revised only the conflicting custom-filter paragraph on the assigned page: arbitrary filters cannot resurrect withdrawn facts. The same obsolete claim remains outside ownership in `docs-2/07_peer_api/types/04_database_predicate.md:60`; reported to parent. No compatibility or snapshot framing changes were added.

Added `tests/database_filters_source_cutover.rs` with five focused tests: six-index temporal/current/history semantics; cross-source withdrawals under custom predicates; immutable speculative overlays; five-component boundary normalization and forward/reverse visibility; source-only and qualified SAVET enablement backfill.

`git diff --check` for owned paths passed. `cargo check --lib --offline` and the focused cargo test were attempted using this copy's default local target. They are currently blocked before test execution by external non-exhaustive `EntityRef::Edn` matches in `src/program_bindings.rs:126,866` (earlier external prefix/order/Pull match failures were also reported and are now fixed). No owned-file compiler errors remain in these attempts. Focused tests have not yet executed; no full parity is claimed. No production storage was accessed.
