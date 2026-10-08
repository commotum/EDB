# TIMESTAMP_SOURCE_DESIGN review

The page already agrees with NORTHSTAR: one mandatory request source, ordinary
entity references, automatic timestamp with the same source, immutable explicit
import time, exact receipt before fresh resolution/time, and canonical genesis
without a timestamp. Leave the page unchanged. The implementation centralizes
attribution in `transaction/assess::material_changes` and time policy in
`transaction/pipeline::select_tx_instant`; no retired dedicated timestamp or
per-operation/map-source path remains in the native input types or program VM.

The bounded gap is acceptance evidence for a writer clock behind db-before.
Current tests cover explicit past/future/mismatch, but not the automatic clamp
with an explicit value under the same backward clock. Retain the implementation
and add one focused private pipeline test using the real durable assessor with
an injected server time. A live clock change would require affecting writer
lease clocks and add unrelated nondeterminism; a test of only the selector would
miss automatic timestamp materialization. The selected test checks one ordinary
source-attributed timestamp at the previous instant, rejects explicit values
above the writer clock and below db-before, and accepts the boundary when the
writer clock catches up. This adds no adapter or alternate policy and keeps
clock choice, contribution identity, and publication independent.

Code reviewed: `transactor/request.rs`; `transactor/authority/mod.rs:527-615`
(receipt lookup precedes gate/authority/basis/capacity/clock); canonical request
encoding in `encoding/mod.rs:567-617`; transaction forms, expansion and shared
pipeline; `transaction/assess/mod.rs:390-520,2648-2736` (ordinary source lowering,
resolution, timestamp deduplication and immutable explicit-time checks).

Inspected proof: `tests-integrated-seventh.log` records source/tempid/lookup,
empty, generated facts, explicit time, deferred EDN and source redundancy tests
passing. `live-integrated-final.log:294-299` records the real durable duplicate
explicit-time bounds/retry test passing; `:321-327` records two durable transaction
model tests passing, including empty transactions and exact retries after the
source ident/lookup fact is removed. `:118-123` records the CLI workflow passing,
including rejection of retired timestamp, operation-source and map-source input.
`tests/schema_source_cutover.rs:1086` explicitly checks system-attributed genesis
has no timestamp. No broad-suite rerun is warranted for the aligned page.

Implemented the approved private pipeline test.
`cargo test --offline --lib transaction::pipeline::tests::durable_backward_clock_clamps_automatic_time_but_keeps_explicit_bounds -- --exact`
passes 1 test, 0 failed/ignored, 61 filtered out; raw evidence is
`timestamp-source-clock.log`. `git diff --check -- src/transaction/pipeline.rs`
passes. Only the test module was added; production code and the page are unchanged.

The NORTHSTAR audit found a real immutable timestamp hole: component
`RetractEntity` could visit an old transaction and generate its timestamp
retraction. The NORTHSTAR owner repaired the shared recursive `Work::Visit`
boundary in `transaction/assess/mod.rs`; I inspected that guard before every
recursive visit. I read their final tests and `northstar-component-after.log`: 2 passed, 0 failed/ignored.
The tests reject direct and nested transaction-entity retractions in eager and
overlay paths while preserving basis, current/history, the component graph, and
original source-attributed activity. An ordinary cascade still retracts all
contributors with the new source, preserving old assertions and matching all six
indexes. This closes the page's generated-retraction timestamp concern. No
remaining page parity gap was found. Isolated `rustfmt --edition 2024 --check
src/transaction/pipeline.rs` also passes.
