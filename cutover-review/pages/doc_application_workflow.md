# Application workflow parity

## Observed situation and diagnosis

Read the assigned page, NORTHSTAR, timestamp reference, CUTOVER and the three supplied talks. The page describes a separate client using public requests, exact receipts, retained/as-of/history reads, local/TLS transport, restart, pure planning, partitions, fulltext/program codecs, async reads and portable references. `application_workflow.rs` implements these paths but the mechanical source extension supplies the system entity for all application requests and checks only assertion/retraction counts. It neither creates an application contributor nor verifies source and automatic timestamp consistency. `Database::new` performs a system-attributed schema bootstrap; this remains appropriate internal bootstrap behavior.

## Bounded problem

Make the documented executable an ordinary source-aware application and execute its complete supported workflow on disposable storage, including restart/exact retry and read-only reference reopening. Keep calculation logic independent of transport and contributor description; keep original APIs, forms, request keys and outputs. Investigate live failures in claimed features and coordinate shared-code fixes with root instead of weakening the page.

## Approaches and tradeoffs

| Approach | Attribution | Composition and local reasoning | Change cost |
| --- | --- | --- | --- |
| Retain system source everywhere | Explicit but describes system bootstrap rather than the example application | No verification of request-wide attribution | Few edits, poor executable teaching example |
| Reserve a numeric application source | Ordinary entity | Couples a new ID to schema/partition fixture allocation | Requires expanding reserved-ID policy |
| Establish ordinary source tempid with ident | Ordinary entity; schema transaction can attribute its own source description | Uses existing tempid/upsert/ident resolution, no client prelookup or adapter | A few fixture changes; schema/source distribution differs between durable and in-memory setup |

Select the tempid/ident approach. Put the source once on each request/speculation. Reuse native transaction reports to check all emitted datoms and the single automatic timestamp. Check existing current/history reads and SAEVT source selections. Use the existing source-free calculation unchanged; contributor attribution and project ownership remain independent.

## Acceptance checks

- Build `edb` and `application_workflow` in `/tmp/edb-cutover-impl-20261007/target`.
- Every successful application receipt (including retries, nested/component, generated replacement/retraction and schema facts) carries the resolved application source and exactly one correctly shaped `:db/txInstant` assertion.
- Native and in-memory calculations remain 8 hours before and 12 after, retained/as-of/history semantics unchanged; speculative datoms retain source.
- Fresh isolated local run completes every existing success marker with zero warmed calculation SQL.
- Stop/restart the isolated stock CLI transactor on the same endpoint and rerun: seed/update/partition/fulltext exact retries succeed.
- Save exact seed reference, stop writer, open reference read-only and reproduce 8 hours.
- Exercise isolated TLS run and its exact retries if credential provisioning is feasible; report limits honestly.
- Inspect stock standby/health lifecycle and reference/program APIs supporting the page, coordinate outside-ownership gaps.

## Changes and results

Implemented in `examples/application_workflow.rs` and seven natural documentation lines in `docs-2/01_tutorials/01_application_workflow.md`. No shared core, CLI, native-host or other page changes. No commit.

- Schema request establishes `:application.source/demo` in entity position and supplies its tempid as the mandatory request source. All later requests/speculations explicitly supply its ident.
- Receipt checks validate every datom's resolved source and transaction, plus one asserted timestamp on the current transaction with the selected instant. Confirmed commit shape is preserved: a failed local check remains a report error on its confirmed receipt.
- In-memory seed establishes the same ordinary contributor with a tempid; seed/update and both planning branches/replan check attribution. `Database::new` retains the existing internal system-attributed bootstrap.
- Retained current/history reads check ordinary contributor attribution; SAEVT source+hours sees only current replacement values. Calculation and ownership-reference logic remain source-independent.
- Live execution found an existing example bug: `$targets` supplied two-column rows to a three-column `DataPattern`. The underlying raw matcher correctly requires the specified width. Use existing two-term `RelationPattern` for this join. No alternate engine, adapter or weakened success criterion.

Build passed with `cargo build --locked --bin edb --example application_workflow`. Final `git diff --check` passed. Runtime checks use the exact shared disposable physical configuration in WORKER_RULES and the distinct LOGIN roles `edb_cutover_app_writer` / `edb_cutover_app_peer`, provisioned through the object-owning `jake` administrative connection. Installation/grants/create/status used stock `target/debug/edb` commands. Logical databases and all local/TCP endpoints are unique to this worker.

| Live check | Result and evidence |
| --- | --- |
| Local complete run | All success markers passed, `QUERY_SQL sql_calls=0 errors=0 result_cell_bytes=0`; log `application-local-retry-after-fix.log`. Initial `application-local-first.log` records the tuple-width failure before repair, after schema/seed/update and exact reference output already succeeded. |
| Local stock-writer restart | All markers passed. `APPLICATION_OK basis_t=3 old_basis_t=2 seed_replayed=true update_replayed=true`; partition/fulltext retries retain basis9/basis11, IDs and UUID intent; log `application-local-restart.log`. |
| Saved local seed reference, writer stopped | `REFERENCE_OK basis_t=2 projects=2 hours=8 exact_key=true read_only=true`; log `application-reference-offline.log`. Reference mode0600. |
| Fresh authenticated TLS discovery | All markers passed; `seed_replayed=false update_replayed=false`, source/speculation/current/history assertions and warmed SQL zero. Locally signed disposable DNS certificate, mandatory trust root/hostname verification and mode0600 PKCS8/token files; log `application-tls-first.log`. |
| TLS stock-writer restart | All markers passed, seed/update/tenant/fulltext receipts replayed=true with exact IDs/bases; log `application-tls-restart.log`. |
| TLS seed reference, writer stopped, no token environment | Exact basis2 and eight-hour result; log `application-tls-reference-offline.log`. Reference mode0600. |
| Persistent standby | Separate socket/listener with modeauto and poll30000 prints STANDBY while active writer owns lease; healthGET200/live=true/ready=false/state=standby, readyGET503. After primary SIGTERM prints READY and full application passes on successor socket; log `application-standby-takeover.log`. |
| Active health after takeover | Both GET and HEAD /health and /ready return200; GET reports live/ready=true/state=active, HEAD body empty. |
| Waiting standby shutdown | A separate standby with poll60000 exits0/STOPPED in27ms after SIGTERM. |

Reviewed stock runtime/health, remote routing, source-bearing request/report types, in-memory bootstrap/speculation, query/raw-relation matching, protected program operator signatures, exact reopen connection APIs and generation report-handoff error path. The tutorial's listed public forms and source argument order match these mechanisms. The local/TLS executable demonstrated existing pure planning, native Entity/reference reads, as-of/history, snapshots, partitions/UUIDs, fulltext/native query codec, async single-thread reads, cache/SQL diagnostics and retry behavior; no implementation gap remains known from this page review. Fault-injected unknown outcomes, GC/excision/restore queue transitions and program-deployment administrative behavior were reviewed in code but were not injected by this worker; these are not claimed as runtime checks above.

All application test writers stopped with SIGTERM/STOPPED. Shared disposable PostgreSQL remains running under root ownership. Only temporary logical databases, unique test roles and private `/tmp/edb-cutover-application-doc-20261007` artifacts were used. Course_Academy, its checkout/services/databases and production storage were untouched.
