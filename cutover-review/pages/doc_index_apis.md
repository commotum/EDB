# Index APIs review

The page describes the settled six-order model: source-aware datoms, typed
zero-to-five-component boundaries, shared AVET/SAVET eligibility and readiness,
ordinary reference resolution, range seeks, and global current visibility. The
boundary and cursor paths already implement those mechanisms. Existing index,
filter, query, Pull, storage and source-activity tests cover their integration.

One concrete storage gap is visible in `validate_routing_shape`: presence of T
requires an exact key, but S is accepted on otherwise sparse original-order
keys with T omitted. S follows T in those orders. The canonical generator never
produces such keys; source-first keys are always exact. Authenticated malformed
nodes can therefore reach local decode validation with a noncontiguous source.

Bounded problem: reject source-bearing sparse routes at the existing canonical
node boundary while retaining generated sparse routes and exact six-coordinate
routes. Requiring all sparse routes to be exact would sacrifice existing compact
separators. Adding an alternate decoder would entangle format selection with
semantics. Select the existing exactness rule, extended to S: either T or S
requires an exact routing key. Root has granted bounded tree ownership.

Keep storage shape validation independent from schema normalization and global
visibility. Reuse the same validator for both directory and root decoding; no
new APIs, source visibility engine, or compatibility representation is needed.
This lets callers reason locally about a canonical routing prefix and keeps
later shape changes confined to one invariant.

Acceptance: demonstrate the public decoder rejects authenticated/checksummed
malformed current/history root and directory nodes for all four original orders;
prove valid sparse separators and exact source-first routes still roundtrip;
run focused index tests. Review remaining zero-through-five boundary normalization,
tuple/ref handling, seek/reverse, ranges and readiness against existing evidence,
adding only uncovered material checks. Append actual results after implementation.

Actual changes: `src/index/tree/mod.rs` extends its existing exactness condition
from T to T-or-S. `src/index/tree/tests.rs` authenticates malformed source-bearing
sparse nodes (both zero and nonzero S payloads) and requires the public decoder
to reject them; valid sparse and source-first exact routes roundtrip. The test
failed before the fix because `decode_tree_node` returned the malformed EAVT
root successfully. It now passes all 32 malformed combinations (four orders,
current/history, roots/directories, zero/nonzero source). No format or API was
introduced, and `stored_eq` behavior was left intact.

`tests/index_apis_source_cutover.rs` adds three portable in-memory integration
tests: all 36 six-order zero-through-five helper shapes normalize ordinary
idents/lookup refs and retain the matching attributed fact in inclusive forward
and reverse seeks; source lookup refs and ident forms agree, as do T/Tx; empty
and short tuple positions preserve lexical ordering, tuple ref slots resolve,
and overlong/mistyped tuples fail; inclusive-lower/exclusive-upper AVET/SAVET
ranges and shared unindexed eligibility agree. Native fixture subjects are
created with issued tempids, and schema is installed before it is used.

Validation: `cargo test --lib index:: -- --nocapture` passed 13 tests, including
the two new storage regressions and the 11 existing six-order tests.
`cargo test --test index_apis_source_cutover` passed all three tests. Formatting
was confined to the two new test files; owned-path `git diff --check` passed.
The aligned documentation page needs no edit. No commits or live application
resources were touched.

Parity review: all six boundary helpers share `require_index_entity`, schema
normalization, and the same raw-boundary validator; tuple refs are recursively
validated by the helper. The original four orders put T fourth/S fifth; the
source-first orders put T fifth. Prefix gap validation remains contiguous.
Current seek windows call global EAV visibility before since/predicate/boundary
selection, with strict stored-value matching; existing database-filter tests
exercise cross-source withdrawal through all six five-component boundaries.
The durable/recent direction paths share normalized comparisons and the existing
multi-directory forward/reverse tests. `IndexBoundary::avet_attribute` covers
both AVET and attribute-qualified SAVET; both call the same readiness resolver.
Source-only and unrestricted scans omit pending projections. Existing storage
backfill tests prove paired projection publication; the parent's pending
backup/restore test checks both qualified prefix failures and source-only/full
scan omissions at captured pending values. Those live tests were not rerun by
this worker. No remaining owned gap or dependency is deferred.
