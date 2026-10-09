# Forward/reverse seek review

`docs-2/06_indexes/05_rseek_datoms.md` matches NORTHSTAR: six-field facts,
six orders, T descending/S ascending in their established positions, inclusive
zero-through-five boundaries, global current visibility before selection, and
SAEVT timestamp ranges composed with a same-basis log. Leave the page unchanged.

Observed gap: existing API tests prove valid T/Tx equivalence but do not reject
invalid native transaction components at seek boundaries. The normalizer already
uses `IndexTransaction::stored_tx` (`t_to_tx`/`tx_to_t`); no implementation fault
was found. Bound the work to proving that schema-aware helpers and both raw seek
directions reject a nontransaction Tx and an out-of-domain T across all six
orders. Keeping only valid-form evidence leaves this boundary unchecked; changing
the API or adding adapters would duplicate an existing invariant. Select one
portable focused test file, reusing the shared normalizer and public cursors.

Local reasoning: entity resolution, transaction normalization, cursor direction,
projection readiness, and global EAV visibility remain separate. The view window
probes a complete EAVT history group before since/predicate/boundary selection;
source-first reads use that same path. This avoids a source-specific visibility
engine and confines future domain changes to the normalizer. No shared edits.

Acceptance evidence inspected: `tests-integrated-fourth.log` records 150 passes,
0 failures, 14 explicit live ignores, including index API 3/3, query source 9/9,
Pull source 7/7 and database filter 6/6. Index library tests cover native
five-coordinate comparisons and both durable directions. `log-api-live-root.log`
records 2/2 actual live passes, including stable reverse source-activity pages
using captured log state. Those suites were not rerun for this aligned page.
Paths reviewed: `src/index/boundary.rs`, `src/index/tests.rs`,
`src/database_value/{cursor,resolve,views}.rs`, `src/database.rs`, and the
corresponding integration fixtures. The AVET/SAVET pending-projection guard gap
in `src/peer/snapshot.rs` is owned by `doc_indexes_log`; this page's readiness
claim depends on its completed fix and tests. `doc_index_pull` owns additional
five-component current/as-of/source-view reverse tests; root owns broad reverse
view composition. Append exact focused results and dependency status below.

Actual change: `tests/rseek_source_cutover.rs` adds one focused rejection test.
All 12 invalid native transaction boundaries (six orders, nontransaction
`Tx(42)` and out-of-domain `T(u64::MAX)`) fail at `validate`, forward/reverse
cursor construction, and their schema-aware helpers with the respective
`identity/not-a-transaction-id` and `identity/eidx-out-of-range` codes. Existing
valid T/Tx equivalence evidence is reused. `cargo test --test
rseek_source_cutover` passed 1/1, 0 failed/ignored in 0.11s; see
`rseek-native-boundaries.log`. The initial compile caught an incorrect fixture
builder call; setting the existing `Attribute.indexed` field fixed it. Targeted
rustfmt and owned-file whitespace checks passed. The page is unchanged.

Readiness dependency closed: inspected `peer-snapshot-readiness-after.log`
with one actual live pass, 0 failures/ignores in 1.47s, and strict Clippy success
in `peer-snapshot-readiness-clippy.log`; the peer guard now includes both AVET
and SAVET. Root reports this fix/test integrated as `da92541`. Additional
index-pull/view coverage remains a root integration concern and is not claimed
as executed here. No remaining owned code gap; no commits or production touches.
