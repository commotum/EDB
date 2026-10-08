# Native API reference cutover review

Read the full assigned page, NORTHSTAR, TIMESTAMP_SOURCE_DESIGN, CUTOVER,
WORKER_RULES, and the three supplied talks. The page is already aligned.
The bounded question was whether its existing Rust names, argument order,
return shapes, and source-sensitive behavior match the public implementation.
No documentation or implementation gap was found; no cause requiring a fix
remains in this page's scope.

Keep the page unchanged. Adding another attribution API would duplicate ordinary
entity resolution; adding optional source or timestamp compatibility forms would
weaken the settled contract. Expanding this reference with already linked setup
and implementation details would increase maintenance without resolving a gap.
The existing request carries attribution as data, separately from time selection,
execution controls, writer authority, and asynchronous scheduling. Speculation
and durable submission reuse the same assessor, so callers can reason locally
about one source without understanding storage publication. No adapter or new
mechanism is selected.

## Implementation and parity checks

- Compared every displayed signature to its owning public implementation:
  `application/connection.rs`, `database_value/{value,views,cursor,resolve,log,invoke}.rs`,
  `database_stats.rs`, `transaction/pipeline.rs`, `edn_transaction.rs`,
  `pull/{execute,entity,index}.rs`, `query/{execute,sequence}.rs`,
  `runtime/executor/mod.rs`, `storage/catalog/mod.rs`, `operations/operator.rs`,
  `backup/capture.rs`, `model/{schema,identity}.rs`, `uuid.rs`, and `database.rs`.
  Also checked the prose-named constructors, boundary helpers, controls, backup,
  maintenance, query binding, shutdown, and report APIs. Names, semantic argument
  order, and return shapes agree.
- `transactor/request.rs` requires `source: EntityRef` in `new` and `from_forms`;
  `edn_transaction.rs` requires it in `from_edn`. All documented speculative
  variants put source after time and before controls/defaults. `transaction/forms.rs`
  retains ordinary operation/map fields, and `transaction/assess/mod.rs` resolves
  source through the ordinary resolver after tempid allocation, adds exactly one
  ordinary timestamp, and stamps material explicit/generated datoms uniformly.
  `transactor/authority/mod.rs` checks receipts before fresh resolution/time selection.
- `model/datom.rs` has E/A/V/T/S/bool and six orders; `index/boundary.rs` has
  the documented original/source-first prefixes. Boundary helpers preserve
  source position and ordinary entity resolution. `database_value/views.rs`
  resolves global EAV visibility before since/predicates/boundaries.
  AVET/SAVET readiness shares `physical_avet_ready`; unrestricted value scans
  omit pending projections. The raw PeerSnapshot fix is present.
- `query/model.rs` and the EDN reader keep named database source separate from
  fifth-slot fact source and sixth-slot boolean. Pull retains its existing
  result shapes and distinct values; index Pull retains selected-datom
  multiplicity. Log and report fields remain unchanged apart from six-field datoms.

## Acceptance evidence and result

Inspected existing test bodies and logs instead of repeating passing suites.
`tests-integrated-fourth.log` records 150 passes, zero failures, and 14 explicitly
ignored live tests across 32 binaries. Relevant passing checks cover ordinary
source/tempid/lookup resolution, callback/composite attribution, explicit and
empty timestamps, every six-order boundary arity, global filtered visibility,
query datom slots, Pull, and index-Pull multiplicity.

Separate actual live logs record: `client-tutorial-live.log` 1 pass for sync/async
socket source and exact retries; `synchronization-live.log` 1 pass for immutable
values/local waits/queued deadlines; `processing-transactions-live.log` 1 pass
for exact independent report queues and removal; `log-api-live-root.log` 2 passes
for captured log bounds and same-basis stable source-activity pages;
`peer-snapshot-readiness-after.log` 1 pass for shared AVET/SAVET readiness before
and after publication. These are executions, not the combined suite's ignores.
The existing `in-memory-tutorial.log` also demonstrates ordinary schema,
speculation, source-bearing reports, queries, and historical views. Unchanged
links reuse root's `doc-link-check.json` (1563 checks, zero issues).

No page/source edits, new tests, builds, live database access, or commits were
necessary. No remaining gap or dependency was found. The root's fifth combined
run is separate integration evidence and is not claimed as a worker execution.
