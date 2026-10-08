# Transact Data acceptance

Observed situation: the complete page submits three ordinary movie maps through
the peer CLI using one request-level `:db.source/system`, shows six-field datoms
and one automatically attributed timestamp, and describes anonymous map tempids,
exact retries and fresh-key duplicate creation. NORTHSTAR, TIMESTAMP_SOURCE_DESIGN,
CUTOVER and the supplied design/simplicity talks were read. Prior exact-schema and
in-memory evidence confirms the vocabulary and most displayed movie values.
The optimized live EDN workflow tests establish the unchanged CLI boundary.

Cause and bounded problem: an illustrative numeric report follows two earlier
transactions and could be inconsistent with allocation/canonical map ordering;
the claim about submitting the same nonunique maps again needs direct evidence.
Verify these two page-specific statements using the exact tutorial inputs.
Do not weaken the source model or change public API conventions.

Approaches and tradeoffs: retain the page if source review and exact execution
agree; correct demonstrated local report errors; edit shared implementation only
for a contract violation and after coordinating ownership. Repeating the live
process suite would duplicate existing acceptance; use its inspected passing
release log for unchanged receipt/retry/CLI behavior and a small speculative
program for exact movie data and duplicate creation. No adapter or compatibility
form is justified. Source, clock, domain schema, allocation and transport remain
independent; ordinary maps compose with the same transaction mechanism and do
not couple user data to per-operation attribution or a special timestamp form.

Selected implementation: extract the exact movie schema and movies from the
document heredocs, run Hello world then schema then movies through `with_edn`,
and compare the entire encoded report against the page after substituting the
documented system source ID. Submit the same movies again and verify three new
entity IDs and six total current title facts. Preserve all feature depth.

Acceptance: report bases 2/3, nine movie assertions and one timestamp, uniform
source 73, exact displayed fields/tempids/IDs/instant, and a subsequent repeat
with no identity attributes creates three disjoint entities and leaves the
earlier immutable value unchanged. Reuse inspected edn-workflow-release.log and
tests/edn_workflow_cutover.rs for durable CLI source, receipt and replay behavior.
Append actual outcomes and any remaining gaps after execution.

## Actual changes and results

No documentation or shared library changes were necessary. The page's complete
displayed report matches exact execution after replacing its explicitly
schematic `source-id` with 73. The scratch program extracted the real document
heredocs, submitted Hello world then schema then movies through the ordinary
deferred-EDN source path, and compared the entire structured report. Bases are
2/3; genre/title/release-year are 1000/1001/1002; movies are Commando
17592186045419, The Goonies 17592186045420 and Repo Man 17592186045421;
all nine movie assertions and the one timestamp carry system source 73.

Repeating the identical movie maps advances to basis 4, produces nine assertions
on three new IDs plus one timestamp, and has no IDs in common with the first
submission. The resulting current database has six title facts; the captured
earlier value still has three. This verifies the page's fresh-request duplicate
creation claim through its underlying transaction mechanism; exact-retry request
keys belong to the durable boundary tested separately below.

Fresh evidence: `transact-data-exact.rs` compiled against the current isolated
debug rlib and `transact-data-exact.log` contains the exact encoded report and
passing assertions. An initial scratch extraction treated the indented Markdown
heredoc delimiter as EDN and failed with trailing input; detecting the delimiter
by trimmed line corrected the harness. No product failure or code change was
involved. This fresh execution is speculative/debug, not a new live CLI run.

Reused evidence inspected: `edn-workflow-release.log` records both optimized
workflow tests passing, including the live process test with no ignored tests.
Its `tests/edn_workflow_cutover.rs` uses an explicitly configured disposable
PostgreSQL database, `--file` submissions, six-slot report assertions, one
timestamp, mandatory source, restart replay of original tx-data/tempids/hash,
source-change conflicts and rejection of retired forms. `transact-schema-exact`
and the in-memory tutorial logs establish exact preceding schema and broader
movie query behavior. `tests-integrated-second.log` records all 43 library and
55 integration checks passing, with live checks explicitly ignored there; those
unchanged suites were not rerun or counted as new execution.

Code inspected: src/bin/edb/data.rs parses mandatory `--source`, retains it as a
canonical deferred EntityRef, opens an active peer and submits to the socket;
its independent receipt fields survive full report conversion/open failures.
src/transactor/request.rs retains ordinary forms with one source;
src/transactor/authority/mod.rs includes source in identity and checks receipts
before fresh resolution/time selection; src/edn_transaction.rs lowers maps and
attributes against authoritative db-before; src/transaction/expand.rs canonically
orders maps and allocates anonymous tempids; src/transaction/assess/mod.rs resolves
source once and materializes explicit/generated facts and the automatic instant
uniformly; src/edn_value.rs preserves the report keys and six coordinates.
The `:edb/report-error` fallback was source-inspected, not fault-injected in this
worker's run. The existing tested successful CLI boundary is unchanged.

No unresolved page-specific behavior gap or shared-code dependency remains.
The page retained all domain-data, tempid, retry and receipt behavior. No commits,
production resources, or implementation-tree files were changed; scratch proof
and this note remain in the review directory.
