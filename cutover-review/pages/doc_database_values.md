# Database values review

The page describes immutable read composition accurately. Source attribution is
independent of database capture, entity identity and lazy projection; none needs
a new interface or a compatibility adapter. `has_avet` correctly documents AVET
readiness, and `savet_boundary` reuses that readiness and tuple normalization in
`src/database_value/resolve.rs`. No documentation change is necessary.

The bounded evidence gap is composition: existing source-cutover tests establish
query and Pull separately, but do not exercise a six-slot raw/database join through
the documented lazy sequence while later transactions replace the fact. Reusing
those passing logs alone leaves the combination unproven; changing sequence code
without a failure would add risk. Selected approach: one focused public-API test
using `BoundEdnQuery::sequence`, native Pull transforms, and captured values.
Acceptance: preparation/remaining-row checks never run transforms; consumed rows
retain each source and one logical Pull value from the captured basis after a
cross-source replacement; the newer basis finds no withdrawn contribution. A
second check distinguishes entity identity from attributed state and independent
memory origins. This keeps capture, attribution and projection locally reasoned
through their existing components.

Inspected paths: `src/query/sequence.rs`, the EDN binding/sequence entry points,
`src/database_value/{resolve,invoke,reference}.rs`, `src/pull/entity.rs`,
`src/entity_identity.rs`, value/overlay origin construction,
`src/storage/read_authorization.rs` pruning and generation authentication, and
`src/operations/model.rs`'s 30-day retention constant. Reused
`tests-integrated-second.log`: database-filter (6), query (9), Pull (7), persisted
program (6), schema (14), and storage/index library tests pass. Their live tests
remain explicitly ignored in that log. No new retention or PostgreSQL claim is
made from this focused in-memory check.

Results: added `tests/database_values_source_cutover.rs` with the two acceptance
checks above; `cargo test --test database_values_source_cutover` passed both
(0 failed, 0 ignored). An initial compilation corrected test-only API usage
before execution. Formatted only this owned new test file. The existing page and
production code remain unchanged. No contract gap was found in the reviewed
paths; no PostgreSQL test or broad-suite rerun was required. Captured-log and
durable authorization/retention lifecycle validation remains covered by the
parent's integration/live evidence, rather than claimed from these two checks.
