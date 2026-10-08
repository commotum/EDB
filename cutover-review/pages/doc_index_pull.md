# Index-Pull review

The page already follows NORTHSTAR: AVET/AEVT select attributed datoms, preserve
their multiplicity and offsets, then ordinary Pull projects the captured shared
record. Source-first selection composes raw cursors with Pull. No documentation
or production change is indicated. The remaining uncertainty is acceptance
coverage: existing index-pull tests cover source-bearing full starts and raw
multiplicity, but do not exercise index-pull through cross-source withdrawals,
temporal/source views, or projection from that same view.

Bounded objective: prove those compositions in both directions, without adding
an index-pull-specific visibility path. Retaining the shared seek/window/Pull
mechanisms keeps traversal, visibility, and projection independent and locally
understandable; deduplicating cursor results would break documented pagination,
and special source adapters would duplicate visibility policy. Add a uniquely
owned focused test file; change shared code only if it exposes a failure.

Acceptance: AVET and AEVT full boundaries must not resurrect contributions
withdrawn outside the selected source/time suffix; as-of must retain the former
record; source/since views must govern both selected roots and projected fields.
Zero-limit construction must validate without invoking filters. Index-pull's
entity control must reset per projection, and cancellation during skipped or
rejected candidates must terminate the cursor. Reuse existing multiplicity and
ordinary Pull tests.

Inspected paths: `src/pull/index.rs`, `src/pull/attributes.rs`,
`src/pull/execute.rs`, `src/database_value/cursor.rs`,
`src/database_value/views.rs`, `tests/pull_source_cutover.rs`.
`tests-integrated-fourth.log` records all 7 pull-source tests passing, including
raw multiplicity/offsets and distinct map values (150 total passes, 14 explicit
live ignores). The PeerSnapshot SAVET readiness concern belongs to
`doc_indexes_log`; this page's index-pull only accepts AVET/AEVT.

Actual change: added `tests/index_pull_source_cutover.rs` only. The three tests
cover eager and transaction-overlay AVET/AEVT full-source boundaries in both
directions after cross-source withdrawal; captured as-of/source/since views;
raw-history rejection; old/current/source-filtered map fields; entity allowance
reset for each index result; zero-limit validation without filter visits; and
cancellation through accepted-offset and rejected candidates with fused error
termination. `cargo test --test index_pull_source_cutover` passed **3 tests,
0 failures, 0 ignores** (0.15 s), recorded in
`tests-index-pull-composition.log`. The initial test fixture referenced a
nonexistent report field; it now uses the report's actual datom transaction id.
`cargo clippy --test index_pull_source_cutover -- -D warnings` also passed;
its output is in `clippy-index-pull-composition.log`.

Reused live evidence: `native-workflow.log` reports PASS through reopen at T4.
Its `examples/native_workflow.rs` captures an index-pull cursor before another
commit, consumes it after writer shutdown, verifies the original projection,
then verifies index-pull from the reopened newer basis. This is actual native
retention/reopen proof, rather than an ignored durable test. No new live test
was run here. Owned-path diff checks pass; the assigned page remains unchanged.
No production or shared implementation changes, commits, or remaining owned
behavior gaps are known. Root retains final integrated acceptance; the other
worker's raw SAVET readiness fix is independent of this AVET/AEVT API.
