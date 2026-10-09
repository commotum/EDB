# Transact Schema acceptance

Observed situation: the complete page already teaches ordinary schema maps, one
request-level source, automatic timestamp attribution, six-field reports and
exact retries. NORTHSTAR, TIMESTAMP_SOURCE_DESIGN, CUTOVER and the three supplied
talks were read. CLI submission, deferred EDN entity references, authoritative
schema binding, schema hook generation, timestamp materialization and report
encoding match that model. Existing 14 schema tests and the live EDN workflow
establish the broader unchanged behavior.

Cause and bounded problem: the schematic report combines an earlier Hello world
transaction with numeric schema IDs and anonymous tempid mappings. Allocation
and canonical map order may make those numbers internally inconsistent; verify
the exact schema vector after the preceding Hello world input before editing.
Preserve all tutorial operations, argument order, and source semantics.

Approaches and tradeoffs: retain the page if exact execution agrees; correct only
report numbers/mappings demonstrated wrong; alter shared allocation or syntax
would unnecessarily entangle pedagogical examples with engine mechanics and
requires no change unless behavior contradicts the contract. Select a disposable
exact-example program using ordinary with_edn and the shared report encoder.
Source, schema, clock and transport remain independent concerns, composed via
existing mechanisms; no compatibility form or adapter is warranted.

Acceptance: exact three-map vector installs title/string/one, genre/string/one,
release-year/long/one plus docs and three generated installation facts; all
16 resulting datoms share resolved system source 73 with exactly one automatic
timestamp; bases1/2 follow Hello world; report tempid entries refer to their
corresponding map entities; explicit schema names bind on the next transaction.
Inspect passing schema, in-memory 20-block and live EDN logs/tests for unchanged
retry/receipt/CLI behavior. Append actual changes and results after execution.

## Actual result

No documentation, library or committed test changes were necessary. A disposable
Rust program extracted the exact movie schema vector from this page, submitted
the previous page's exact Hello world input first, then submitted the schema via
ordinary `DatabaseValue::with_edn`. It compiled against the current isolated debug
rlib and passed every assertion. `transact-schema-exact.log` records the rendered
report and successful checks; `transact-schema-exact.rs` is its reproducible input.
The initial scratch harness used a nonexistent root write_edn re-export; correcting
the harness to the documented edn module path resolved compilation without engine
changes.

The exact report matches the page after substituting 73 for its explicitly
schematic source-id. Bases are 1/2; genre/title/release-year are 1000/1001/1002;
all anonymous tempid mappings match; all 16 datoms carry source 73; the generated
three installation facts and sole timestamp are present. A subsequent movie map
using all three newly installed attribute names succeeds at basis 3 and confirms
the page's db-before binding rule. No presumed numeric mismatch was found.

Code inspected: src/bin/edb/data.rs admission, deferred --source construction,
peer socket submission and independent confirmed receipt rendering;
src/transactor/request.rs request/report shapes; src/transactor/authority/mod.rs
canonical identity including source, receipt lookup before fresh resolution and
clock selection; src/edn_transaction.rs canonical forms, schema-bound attribute
lookup and map/reference lowering; src/transaction/expand.rs canonical ordering,
anonymous entity maps and db-before attribute resolution;
src/transaction/assess/mod.rs source resolution, generated install hooks,
automatic timestamp and uniform material source boundary;
src/model/schema.rs three required schema properties;
src/model/{identity,vocabulary}.rs shown IDs and built-in scalar/ref vocabulary;
src/edn_value.rs six-coordinate datom and report encoding.

Existing inspected acceptance: tests-integrated-second.log records all 14 schema
checks passing, including canonical genesis and open schema attribution;
in-memory-tutorial.log/doc_in_memory.md prove the exact 20 movie tutorial blocks;
edn-workflow.log and tests/edn_workflow_cutover.rs prove live CLI --file schema
submission, six-coordinate reports with one source and timestamp, preserved
receipt data after restart, changed-source conflicts and rejected retired syntax.
Those unchanged suites were not rerun. This worker's exact-example proof is
speculative debug execution; it does not claim a fresh release or live CLI run.
Existing CLI live evidence supports the same ordinary public submission boundary.

No unresolved page-specific behavior gap or shared-code dependency remains.
No commits or production resources were touched; all scratch proof artifacts are
in the review directory outside the implementation tree.
