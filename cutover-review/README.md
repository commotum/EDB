# Source cutover review

The source cutover is implemented and verified for its first private release on the tested Linux environment. All 111 files present in `docs-2` at review time received a separate gpt-6.1-sol high review, followed by parent review and integration. 25 pages required edits; 86 were unchanged at review time. No unresolved documentation or implementation parity finding remains from this review.

The implementation is in `/home/jake/Developer/EDB-source`, branch `source-first-class-cutover`. The complete ordinary/live suites exercised product code `5b0c46be9e7bebf1103ee857c0ad6ac9a21b78d4`. Follow-up `246286350a389011b7b021d20942353891154705` changes only two test assertion expressions; its focused regression, strict lint, docs and optimized release checks passed. Product code is identical across those revisions; the subsequent review-record commit changes no product code. The [page matrix](page-reviews.md) links every assigned plan, inspected code path, result and design decision. [Integration notes](integration-notes.md) record shared decisions and how findings were closed.

## Model and implementation

- Datoms retain `[e a v t s added?]`. A request supplies one ordinary source reference, resolved once for every explicit and generated assertion or retraction. Time selection remains independent and generates the ordinary timestamp with that same source.
- Equal facts from different sources remain distinct contributions. Cardinality, uniqueness, CAS and ordinary Pull count values or holders. Retraction withdraws the shared value across contributors. Current visibility is resolved globally before source selection, including temporal views and source-first index scans.
- EAVT, AEVT, AVET and VAET remain; SAEVT and SAVET use the same index machinery. AVET/SAVET share eligibility and readiness. Activity composes the SAEVT timestamp range with a log captured at the same basis; no separate activity tree is required.
- Canonical request identity includes source. Accepted receipts are checked before fresh source resolution, clock selection and admission. New wire, log, index and program formats reject incompatible representations; there is no compatibility adapter.
- The dedicated timestamp-source operation, map declaration and per-operation sources are removed. Their remaining input examples are rejection tests. Ordinary request source and timestamp behavior is documented in the [Transaction Data reference](../docs-2/04_transactions/04_transaction_data.md#reified-transactions). The redundant standalone `TIMESTAMP_SOURCE_DESIGN.md` was removed after this review.

The review also repaired dependent identity resolution, pending SAVET admission, durable log payload admission, shared integrity verification, cancellation of administrative work, and retired-lineage reclamation fencing. The last finding was a component cascade that could retract a transaction entity: the existing rejection now lives in the shared traversal, protecting timestamp activity without another timestamp mechanism.

The design process followed [Design in Practice](https://github.com/matthiasn/talk-transcripts/blob/master/Hickey_Rich/DesignInPractice.md): describe the observed gap, identify its cause, bound the problem, compare approaches, implement, and test the claimed behavior. The structural criterion from [Simple Made Easy](https://github.com/matthiasn/talk-transcripts/blob/master/Hickey_Rich/SimpleMadeEasy.md) and [Simplicity Matters](https://github.com/matthiasn/talk-transcripts/blob/master/Hickey_Rich/SimplicityMatters.md) favored shared mechanisms with distinct responsibilities: attribution, value constraints, visibility, time selection and physical access remain independently understandable. Per-page notes contain the concrete choices rather than a second architecture.

## Final verification

| Check | Result | Evidence |
| --- | --- | --- |
| All-target ordinary suite | 189 passed; 0 failed; 32 live tests explicitly deferred to the next run | [Log](evidence/final-default.log) |
| All deferred PostgreSQL/runtime tests | 32 passed; 0 failed; 0 ignored | [Log](evidence/final-live.log) |
| Rust doctest command | Completed; 0 doctests present, 0 failed | [Log](evidence/final-doctests.log) |
| Formatting and strict all-target Clippy | Passed | [Formatting](evidence/final-format.log), [Clippy](evidence/final-clippy.log) |
| Rustdoc with warnings denied | Passed | [Log](evidence/final-rustdoc.log) |
| Optimized library and CLI build | Passed | [Log](evidence/final-release-build.log) |
| Optimized CLI workflow, persistence and exact retry | 1 passed; 0 failed | [Log](evidence/final-release-smoke.log) |

The live checks exercise source-bearing indexing, recovery, backup/restore, offline backup reads, fulltext, excision, clients, hints, caches, lifecycle and integrity failures. Additional current-format acceptance includes [actual PostgreSQL crash/WAL recovery](evidence/ha-current-workflow.log), [local/TLS application use and standby takeover](evidence/app-current-harness.log), and completed collection followed by healthy source/restored inspections. The final timestamp traversal repair is additionally covered by eager and overlay regressions in the final ordinary suite.

The documented source distribution was transferred with vendored dependencies, verified against 16,588 file checksums, and built both standalone and as a fresh consumer with an empty isolated Cargo home. [Consumer build](evidence/distribution-consumer-frozen-build.log), [library build](evidence/distribution-library-frozen-build.log), [public API execution](evidence/distribution-consumer-run.log). That packaging proof preceded the final narrow timestamp fix; final optimized builds and tests above use the final code. It is evidence for the distribution method, not a published final archive.

Validation used Linux x86_64, Rust/Cargo 1.90.0 and isolated PostgreSQL 15.11, with local and authenticated TLS transports. AWS provisioning and other operating systems were not exercised. No external release, package publication or application migration was performed. Earlier focused logs include exploratory failures and their corrections; the final logs and [check manifest](evidence/final-checks.json) identify the accepted revision and results.

The optimized CLI is available at `/home/jake/Developer/EDB-source/target/cutover-release/edb`. Its copied bytes match the tested executable; [the artifact manifest](evidence/release-artifact.json) records the code revision, platform and SHA-256. Library consumers build the source branch using the documented Cargo workflow. The disposable PostgreSQL test server was stopped after acceptance; Course_Academy's server remains active.

## Existing application

Course_Academy continues to use `/home/jake/Developer/EDB` on `master`. Its database, writer socket, services, binaries and native-library paths were not changed. Final read-only verification found all 349 protected EDB file hashes unchanged, all six original process executables still present, and the application, writer and PostgreSQL services active. [Runtime verification](evidence/runtime-protection-final.json).

Three concurrent Course_Academy Python-file edits were already present outside this task; they were preserved. All cutover builds and database tests used separate artifacts and disposable storage. Keep the existing application on its old runtime until its separate migration is deliberately undertaken.
