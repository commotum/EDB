# Identity and values review

Situation/objective: the assigned page already separates logical value identity,
stored decimal scale and datom attribution. Source is one ordinary entity
coordinate; distinct contributions must preserve stored representations without
multiplying unique holders or ordinary projected values. Prior schema (14 tests),
Pull, stored-decimal and Entity proofs cover the main source-sensitive branches.
No demonstrated documentation mismatch has emerged.

Bounded review: inspect value comparison/hash/storage equality, coordinate checks,
URI identity and exact submission encoding, schema value constraints, assessor
upsert/redundancy/NaN/composite behavior, and distinct-value read projections.
Look for an uncovered source-sensitive path rather than repeat passing suites.
Retain aligned prose. If a concrete gap occurs, reproduce it in a focused uniquely
owned test and coordinate shared code before changing the existing mechanism.

Decision/tradeoffs: adding source to logical value equality would change joins and
identities; removing source from facts would discard information. Keep these
independent through existing logical comparisons, stored comparisons and the
single EAVS material boundary. Reusing these policies supports composability,
local reasoning and independent changes to attribution and projections. New
compatibility adapters or parallel comparison policy are excluded.

Acceptance: logical equality/hash ignore decimal scale and source; stored equality
retains top-level scale but not nested scale; same-source aliases are redundant,
new-source contributions material; global retraction clears exact stored values;
unique holders and Pull/Entity count distinct stored values. Inspect checked ID
bounds, URI exact retry behavior, schema size/type limits, NaN rejection on unique
attributes, immutable schema/ident context and paired index readiness. Record
exact code and existing/new evidence below after review.

## Actual changes and evidence

The complete page, NORTHSTAR, timestamp/source reference, CUTOVER, worker rules,
and all three supplied talks were read. The assigned documentation is aligned
and remains byte-for-byte unchanged. No library implementation edit or
compatibility path was necessary.

Added only `tests/identity_values_source_cutover.rs` (three portable in-memory
integration tests) to close concrete acceptance coverage absent from the prior
integration sources:

- URI component aliases upsert/lookup to one unique identity and one current
  value while retaining both exact spellings under distinct source contributions.
  Same-source alias reassertion emits only automatic time; alias retraction records
  its supplied spelling/source, withdraws both contributions, and leaves captured
  and as-of values intact. Exact request digests distinguish spelling and source.
- Float and Double NaN payload aliases are redundant for the same source and
  material for another source without multiplying values. Finite replacement
  emits one global NaN withdrawal and one finite assertion under the replacing
  source; reverse replacement and alias retraction also work on eager and
  speculative overlay bases. Source predicates cannot revive withdrawn values.
- Both uniqueness modes reject NaN in explicit-entity replacement and tempid
  claims, retaining the earlier finite key unchanged.

Code reviewed: complete `src/model/value/{mod,numeric}.rs`, `uri.rs`, `datom.rs`,
`identity.rs` and `idents.rs`; schema type/tuple/bignumber/bytes-unique validators;
assessor source lowering, identity grouping/upsert, stored-value successor and
holder counts, cardinality replacement, composite derivation and material boundary;
`src/database.rs` eager projection/replay; `src/database_value/resolve.rs` values,
lookup and captured AVET readiness; Pull selector projection and Entity forward
navigation; `src/query/value.rs` logical comparison/hash delegation;
`src/encoding/mod.rs` exact URI/source request bytes; `src/index/visibility.rs`
stored-value global barriers and `metadata.rs` shared AVET/SAVET admission.
Checked representation bounds and comparison rank/order directly against the page.

Inspected prior `doc_schema_reference.md` and its 14-test source/log evidence,
`doc_schema_overview.md`, `stored-value-tests.log`, `doc_entities.md` and
`doc-entities-after.log`, `doc_pull_reference.md`, and
`tests-integrated-second.log`. These prove decimal scale projection/limits,
cardinality/uniqueness, managed composite identity, tuple limits, deferred source
resolution, ordinary timestamps, canonical genesis and paired index readiness.
The broad unchanged suites were not repeated.

Final verification: `cargo test --offline --test identity_values_source_cutover`
passed 3/3 (0.46 seconds); `cargo clippy --offline --test
identity_values_source_cutover -- -D warnings` passed. Logs:
`identity-values-tests.log`, `identity-values-clippy.log`. Owned-document diff and
new-test no-index whitespace checks passed. An initial fixture compile found a
moved `finite` value, corrected by cloning; strict Clippy then requested
`slice::from_ref`, also corrected. These were test fixture issues; no behavior
regression was reproduced.

No unresolved page-specific behavior gap or shared-code dependency remains.
New acceptance tests exercise eager and speculative overlay paths; this worker
ran no new PostgreSQL persistence test. Root owns full-suite and durable
integration. No commits, production resources or other documentation changed.
