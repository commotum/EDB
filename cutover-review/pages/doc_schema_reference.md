# Schema reference cutover evidence

Read `docs-2/03_schema/02_schema_reference.md` (all 747 lines), NORTHSTAR,
TIMESTAMP_SOURCE_DESIGN, CUTOVER, worker rules, and all three supplied talks.
Read complete schema model, assessor and native registry before implementation.

Observed situation: the six-field model and the assessor's final material boundary
already assign the resolved request source uniformly. Schema readers, CAS,
cardinality transitions and unique ownership already deduplicate source
contributions. Canonical genesis is system-attributed; temporary synthesized
schema metadata carries system attribution only inside a discarded EAV projection.
The reference retains the settled shared-record model and needs no source redesign.

Cause and bounded problem: composite derivation omits its logical assertion when
the resulting tuple equals db-before, deciding redundancy before source is known.
This loses the derived contribution when another source reasserts constituents.
Descriptor lowering also checks EAV existence before the material boundary for
changed schema properties; test whether partial predicate changes preserve new
source assertions for overlapping members. All documented schema features must
remain governed by db-before input bindings and complete proposed-schema/fact checks.

Approaches and tradeoffs:
- Keep early value-based suppression: smaller delta, but source information is lost.
- Thread source into every schema/tuple derivation branch: works, but entangles
  logical derivation with attribution and duplicates material redundancy logic.
- Emit applicable logical facts and let material_changes make EAVS redundancy
  decisions: reuses the existing mechanism, preserves independent responsibilities,
  and keeps source changes locally understandable. Selected approach, bounded to
  affected generated assertions after reproducing failure.

Acceptance checks: source-distinct unchanged composite contributions; same-source
redundancy; global tuple replacement/retraction; paced late-composite backfill;
permanent discontinuation; schema installation/alter metadata attribution; duplicate
source values through many-to-one/uniqueness; predicates from db-before with no
retroactivity and retraction bypass; entity specs over complete db-after including
managed tuples; builtin immutability and system genesis; tuple slot/type/arity limits.
Run focused isolated-copy unit/integration tests. Durable tests, if necessary, use
explicit ignored EDB_TEST_POSTGRES_URL and unique logical database resources only.

Dependencies: native predicate registry has no source-specific forms; no edit
planned. Fulltext/index readiness/noHistory retention are implemented in other
owned storage modules; report any concrete gap to root instead of modifying them.

## Implemented changes

Reproduced two initial regressions (both tests failed before the fix): a second
source's unchanged composite assertion was absent, and a changed predicate set
with overlap emitted only its new symbol rather than both changed-property
assertions. Logical derivation now emits applicable assertions; the single
`material_changes` boundary applies source-aware redundancy to both.

Reproduced a required entity-spec name resolving to a nonattribute entity and
incorrectly producing `transaction/entity-spec`. The resolver now requires an
installed descriptor and reports `transaction/unknown-spec-attribute`.

Reproduced a discontinued composite accepting a direct tuple assertion. All
composite definitions now retain managed direct-form semantics; discontinuation
still removes reverse maintenance dependencies and remains irreversible.

Removed unreachable older-genesis partition synthesis in the schema projection
and its matching missing-db-before predicate exception. Current canonical genesis
already installs the partition marker attribute and rejects alternative profiles.
Corrected an obsolete predicate deployment comment. Kept transient synthesized
schema Datoms under system source: they are discarded after EAV schema projection,
while committed schema datoms receive the resolved enclosing source.

The document's source model needed no change. Corrected its claim that
DatabaseValue::with_functions accepts primitive operations (it accepts TxForm
input) and clarified that discontinuation retains managed tuple semantics.
No native registry edits were necessary.

## Concrete acceptance evidence

`tests/schema_source_cutover.rs` contains 14 independent behavior tests covering:

- Source-only composite contribution, same-source redundancy, shared tuple
  cardinality/uniqueness, cross-source replacement and all-constituent retraction.
- Native descriptor changes with overlapping multi-valued predicate information.
- Late composite backfill through a redundant reassertion, nil partial keys,
  irreversible discontinuation, no install-time discontinuation, retained stored
  tuples and ineffective direct tuple edits after discontinuation.
- Many-to-one and unique transitions over duplicate source contributions;
  tempid upsert, unique-holder transfer, transition conflict cleanup and retained
  historical nonuniqueness; prior AVET readiness required when history exists.
- db-before attribute predicate definitions, no retroactive scan, same-source
  redundant assertion versus fresh source validation, removal not bypassing old
  predicates, retractions not invoking value predicates, complete rejection details.
- Entity specs over complete generated db-after, distinct values shared by sources,
  duplicate predicate definitions called once, virtual ensure not persisted,
  required keys before predicates, no automatic ongoing enforcement, db-before
  spec changes, and nonattribute required names rejected.
- Native deployments: literal true only, diagnostic return captured, missing code
  rejected, panic contained, failed assessment leaves immutable base unchanged.
- Immutable type/fulltext/tuple definitions, builtin protection, bytes uniqueness,
  uniqueness cardinality, component ref contract and active composite constituents.
- Tuple exact slot types, nil arity, UTF-16 string size and signed integer bounds.
- Canonical system-attributed genesis without timestamp, all ordinary schema
  material datoms attributed to request source, open application metadata, and no
  accidental implicit :db/source attribute.
- Two distinct top-level decimal scales each shared by two sources: stored value
  counts preserve both, many-to-one rejects, exact-scale retraction clears that
  representation across sources while retaining the other. Root independently
  fixed eager read/replay and Pull scale parity; this test verifies that dependency.

Observed passing runs before the final discontinued-direct-write patch:
`cargo test --test schema_source_cutover --test data_model_source_cutover --lib`
passed 42 library tests, 13 schema tests and 4 foundation integration tests.
After adding the decimal regression, `cargo test --test schema_source_cutover`
passed all 14. One intermediate combined run encountered another worker's
transient missing src/backup/tests.rs after its module declaration; the schema-only
run subsequently succeeded. The final combined run is recorded below.

Storage dependencies: AVET/SAVET physical readiness, noHistory retained-index
projection, fulltext analysis/index lifecycle and durable recovery live in root/
other worker-owned modules. Existing library suites cover paired backfill and
source retention. No unresolved schema-assessment dependencies found. No live
PostgreSQL test was run by this worker; no production resources were touched.

Final results: `cargo test --test schema_source_cutover --test data_model_source_cutover`
passed all 14 schema tests and all 4 foundation integration tests after every final
code edit. `git diff --check` on owned code/document files passed. The last combined
`--lib` attempt encountered a concurrent backup worker test compile error at
src/backup/tests.rs:64 (`AttributeName: From<u32>`); root was notified. Library tests
passed (42/42) after legacy fallback removal and before the final managed-direct-
form fix. No schema test failures remain. Root owns complete-suite integration and
the unrelated backup compile issue. No commits made.
