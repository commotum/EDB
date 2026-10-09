# Integration design

The current engine encodes five-field facts, resolves one EAV winner, and routes
four physical orders. The source contract preserves several contributions to
one value, so changing only the datom struct would leave replay, authenticated
state, temporal readers, and persisted projections inconsistent.

The bounded problem is to carry explicit attribution through every existing
boundary while preserving one shared record. A per-source database or independent
timestamp directive would split that record and duplicate transaction policy.
Instead, use one source resolution in transaction assessment; six-field immutable
datoms thereafter; one global visibility rule; existing index machinery with two
additional orders. Current value/entity projections deduplicate logically where
their contract requires values rather than contributions.

Integrate in dependency order: model/assessment/index core, encoded storage and
read paths, public APIs/query/transport, examples and operational paths. Compile
between stages to find missed structural boundaries. Worker ownership prevents
concurrent modifications; every documentation page receives its own worker.
Tests must exercise cross-source equal values and retractions, generated facts,
retry identity, temporal reads, all six orders, persistence/recovery and source
activity. Old data formats fail explicitly. All builds/storage stay isolated
from Course_Academy. Simplicity is judged by shared mechanisms and independent
responsibilities, not line count or the number of modules.

Root review found strict-scale equality must remain separate from source
multiplicity. Numeric index ordering treats decimal scale variants as equal,
but stored-fact equality distinguishes their scale. A new-source duplicate is
deduplicated by stored equality, not by changing that established value rule.
Keep read results in index order, search only the equal numeric group for a
previously returned stored value, and apply the same equality in replay's
cardinality proofs. This avoids quadratic scans across unrelated many-values
without introducing a separate identity policy or an extra materialized set.
Regression must interleave scale variants across sources and exercise values,
Pull limits, cardinality transition and authenticated replay validation.

Mid-cutover protection check: all 349 protected original EDB files match their
recorded hashes, including the old release executable and rlib. Course_Academy
writer/reader binaries, Cargo files, EDB configuration and service scripts match.
Three unrelated Course_Academy question_capture Python files have concurrent
working-tree edits (18 insertions, 1 deletion) compared with the earlier evidence;
this task has never written there. Preserve those edits; do not refresh their
baseline or claim the whole application tree is unchanged. Historical report
SHA256 remains 337f37354fb5df17a902227fced7100bcec72bce66c348ea1eb9d8e84138afc2.

Second integrated default test run passed: 43 library tests and 55 integration
checks (plus 7 intentionally ignored live tests). Explicit durable workflows
are tracked separately. Root decimal regression passed alongside 7 Pull,
4 data-model and 6 persisted-program checks. No library compatibility fallbacks
remain for old genesis partition/fulltext profiles.

Checkpoint integrated: temp implementation commit00e61d8 fast-forwarded into
/home/jake/Developer/EDB-source source-first-class-cutover. Destination clean;
all builds continue only in temp target. Original master advanced concurrently
to4b5f7b7 by committing the prior docs preparation; its docs-2 tree exactly
matches prep6b973b9. Protected349 old EDB file hashes still match. Do not reset
master or infer this unrelated commit came from the cutover integration.
All-target --locked --offline check passed; Cargo.lock restored with no changes.

Root reviewed index API boundary coverage and authenticated sparse-routing
regression, EDN CLI acceptance, exact 20-block in-memory tutorial output, peer
overview, and transactor mode0700 clarification. Optimized release binary built
offline successfully, and both EDN workflow tests passed against that optimized
executable with the explicit disposable provider (edn-workflow-release.log).
Root corrected retained-memory commentary from five to seven compact references
to match MAX_BTSET_REFERENCES_PER_DATOM=7; no accounting policy changed.

Reviewed tutorial/index checkpoint1556442 is integrated into the separate
EDB-source branch. Strict rustdoc and all-target Clippy pass. Root reviewed
HA worker's bounded restart example changes and actual immediate-PostgreSQL
crash evidence: all six current/history index hashes, source-bearing receipt
replay before fresh tempid/basis checks, changed-source rejection, SAEVT/log
activity and cross-source replacement passed. Its second disposable server
was stopped and removed; shared test PostgreSQL remains available.

Reviewed checkpoint72efdec integrated into separate EDB-source branch. Root
verified Entity decimal stored equality fix, full-current source index recovery
proof (including retained-history-damage limit) and EDN public-domain tests.
114 default tests pass,0fail,11 live ignored; every such live test has its
separate executed evidence. Root final integrated live pass still pending.
Shared test catalog has two known ownership-fault fixtures from an abandoned
recovery-test injection; exact names in doc_deployment.md. Shipped test uses
unique consolidated data and RAII descriptor restoration. Final whole-provider
GC/ownership acceptance must use fresh isolated storage, never this catalog.

Diagnostics page and full new test personally reviewed: independent counters/quota admission, same/new-source materiality, generated facts, and receipt-before-source resolution are sound. Actual four-test live run and strict Clippy inspected. Partitions four-test file reviewed earlier; acceptance awaits ref/tuple identity dependency composition.

Processing Transactions: personally reviewed exact doc-derived six-slot raw-report/name-join query and independent observer queue tests. Actual live queue log passes, including removal/re-enable and receipt replay not becoming a new event. Page remains unchanged; final acceptance waits resolver integration. Timestamp selector/validator equal-value dedup fix and three new tests reviewed; live timestamp evidence passes, worker still investigating nested-map depth.

Root personally reviewed identity dependency worklist, canonical ref/tuple aliases, linked union members, preserved minimum representatives and deterministic allocation. Eight new tests cover dependent existing upserts,64-hop order-independent chains, new-only aliases/cycles, late binding propagation, and conflicts. Ninth reproduced/fixes transaction db/ident lookup differing from resident alias dictionary. Explicit ident retractions already retain aliases under existing dictionary rules; ordinary unique-key withdrawal and zero fresh work still provide receipt-first evidence. Map depth encoder/decoder/admission fixes and malformed32/33/nested-Many tests reviewed; ordinary equal timestamps dedup without changing source/time responsibilities. No direct per-operation attribution introduced. Identity final suite logs and transaction-data focused/live/Clippy logs inspected. Processing page identity dependency now covered; partition composition acceptance in progress.

Fourth integrated default suite passes150 tests,0failed,14explicit live ignores across32binaries. Root personally reviewed transaction-functions public tests and private retained-code binding test: six input orders keep db-before, generated source propagates once, call budget remains shared, same-transaction rebinding takes effect only on later calls. Four focused cases and combined suite pass; no production edits.

Partition dependency closed: root reviewed fifth source-ref-upsert/named+implicit-partition composition test and actual5-test passing log. Original account/source IDs preserved, matching new entity follows original tempid policy, eager/overlay results identical, allocation frontier advances only for related entity. Identity, Transaction Data, Processing, Partitions and Functions pages accepted pending final global release checks.

Checkpoint81419b9 integrated into clean EDB-source branch. All-target strict Clippy passes after identity/map/timestamp/function changes (clippy-transaction-boundaries.log). ACID page and trace/evidence reviewed unchanged. Source log2live tests rerun and raw stdout preserved as log-api-live-root.log because earlier worker retained only note/tool-result proof. Midpass host verification: all349 old-checkout hashes unchanged; original6 process executables still active/matching and Course application/writer/PostgreSQL services running. Three previously known unrelated application Python edits remain; no runtime artifacts changed (runtime-protection-midpass.json).

Query overview reviewed unchanged: six-coordinate raw sources and named-input/fact-source separation match existing engine and tests; saved live log2/2 confirms timeline composition. Synchronization unique test personally reviewed including guaranteed worker-side on_complete registration; controlled gate makes expiry deterministic. Actual1live pass and focused Clippy inspected; no source-specific synchronization mechanism.

Application-data and Pull/Entity overviews reviewed unchanged with concrete existing code/test evidence. Exact7-column EDN query example executes offline with expected map/tag projection (application-data-smoke/result.json). Pull overview correctly preserves stored scales and limits before traversal while raw index-pull keeps contribution rows. No new runtime mechanism needed.

Executing Queries overview reviewed unchanged; existing source/captured-sequence/Pull tests cover established shapes and budgets. New cross-page QA finding: raw PeerSnapshot ensure_order still gates only AVET, whereas Index APIs explicitly applies peer/avet-not-ready to SAVET too. Index/log worker owns reproduction/narrow repair; reopen prior index API acceptance until this dependency closes. DatabaseValue unbounded scans intentionally omit pending projections; do not conflate that surface with raw PeerSnapshot guard.

Query diagnostic regression personally reviewed: three source access modes capture existing counters without changing results/admission; zero/partial capture does not alter semantic budgets, raw values absent from reports, exact CLI wrapper/tie schedule matches documented behavior. Actual3/3 proof and strict focused Clippy pass. No new diagnostic mechanism or runtime edits.

Root reviewed raw PeerSnapshot readiness fix (Avet|Savet shared guard with established error code) and deterministic manual-writer/live regression. Before proof fails specifically on accepted pendingSAVET; after1/1passes current/historyrange/seek/datoms, independent DatabaseValue omission, ready attributed contributions and retained-old pending snapshot. Focused strictClippy pass. This closes reopened Index APIs acceptance. Root reviewed3passing index-pull composition tests and strictClippy; view/boundary selection precedes same-view projection, zero-limit admission/cancellation semantics preserved.

Rseek page accepted after root review of all-six-order invalid nativeT/Tx boundary tests and actual1/1log; existing valid-boundary tests reused. Combined source peer/index fixes integrated destinationda92541; original runtime unchanged.

Fifth root combined suite passes158 tests,0failed,16explicit live ignores across37binaries (tests-integrated-fifth.log), after source-index guard and latest diagnostic/index/sync/boundary regressions. Connections and Async client overviews reviewed unchanged; existing actual sync/report/async-retry/restart logs support unchanged facade/capture semantics. Native registry removal/retry boundary test assigned to native-computation worker, not claimed covered by stored-program proof.

Native API and types overview: reviewed unchanged pages, worker signature/shape checks and underlying code/test evidence; accepted. Native computation: reviewed new portable ignored test and actual live 1-pass log plus focused Clippy. It restarts an isolated service without native registry and proves exact accepted source-bearing request replay, fresh deployment failure, changed-source conflict, and unchanged captured/live bases. No production changes. Consumer proof reviewed but admission gap remains open.

I/O stats page accepted unchanged. Root reviewed direct durable SAEVT/SAVET current/history OperationContext test and actual1livepass/strictClippy logs: lazy cursor consumption returns to captured context, parent inclusion and sibling isolation hold, cold/warm accounting and generic EDN serialization remain shared mechanisms. No source-specific diagnostics code.

Error-handling page accepted unchanged after narrow shared classifier repair. Root reviewed full diff/classifier, focused driver and11controlledserverSQLSTATE/WrongType tests and actual live1pass/Clippy logs. Categories and storagecode stable; commit unavailable still unknownoutcome. Marker restores generic diagnostics; existing writer/consumer code fallbacks already preserve reconnect policy. No test claims actual mid-write fault injection.

Consumer admission closed with uniform kind14 metadata/chunk entries. Root reviewed entire codec diff, exact40-byte metadata/old-kind rejection/count/body-length guards, empty-retained entry, memory-reader no-body checks and actual consumer2/recovery1/log2 live passes plus strictClippy. Extra object/read for smalltransactions buys independent authenticated metadata/body admission, removing inline branch without reader/provider API. ACID storage sentence minimally aligned by root. Final globalbackup/ownership requires freshprovider with kind14; mixedsharedfixture not suitable.

Latest root integration: tests-integrated-sixth.log passes162, fails0, explicitly ignores21 live checks across40binaries. Includes uniformkind14 log metadata/chunks and sharedPg diagnostics. Actual kind14 consumer2/recovery1/log2 passes in change-consumers-uniform-live.log; other live proofs older than format change must rerun for final release.

Synchronous client API accepted unchanged. Root inspected page/worker concrete signature trace and reused immutable-query/sequence/report/log/synchronization evidence. No new source wrapper or API required.

Attribute type page accepted unchanged: field/builder/derivedschema/alias/readiness trace and focused prior14schema+4changing-schema+actualrawreadiness1pass reviewed. Sixth all-target strictClippy passes. Fresh separate edb_cutover_log14/log14_restore catalogs created for kind14 backup and provider ownership proof; initial sharedmixedcatalog untouched.

Async API accepted unchanged after root review of gatedworker capture test. One soleworker callback forces materialized/prepared jobs to wait; stream created but notpolled; caller replaces original sources with newer cross-source withdrawal. Exact capturedA98/A99 rows persist; freshcurrent/filter excludes withdrawnfacts; freshhistory has R97, streamfuses. Actual1pass/focusedstrictClippy inspected. No corechanges or source-specificasyncpolicy.

Connection type page accepted after rootreview of paired AVET/SAVET readiness wording and sharedfrontier implementation evidence. No extra wait or readinessstate. Fresh log14 backup proof passed1actual83.56sec across pending/currentpartial/historypartial/completepublications, deep/offlinereads, exactsource retry and corruptedbackuprejection. Raw stdout retained backup-log14-live.log. Root uniformkind14provider GC now inprogress on freshcatalogonly.

Datom type page accepted unchanged: root reviewed exactexample/evidence and comparator/stored-equality trace. Example executed with existingisolatedlib; no redundant committedtest. Freshkind14sourceGC completed4boundedbatches, then CLI deepinspection healthytrue problems0 atbasis13/index13. Restoredcatalogfirstcycle stillprocessing larger retainedownership graph.

Entity type page accepted unchanged with previous actualdecimal/Entity/Pull/visibility/identity evidence. Database type page unchanged; root reviewed three new publicboundary tests for rawhistory statistics/control, equalinstant+entid_at in eager/overlay, filteredCAS againstfullbasis withview restoration. Worker reports5focusedpasses and strictClippy; new rawstdout notretained in reviewdir, nextcombinedrun suppliesrootretainedproof. No implementationchange.

Reviewed checkpoint8f47bc7 integrated isolated EDB-source cleanbranch. Fresh kind14 source/restored catalogs both completed collection cycles (4/15boundedbatches); postcollection deep CLI inspections healthytrue, problems0, basis13/index13, reachable253/279. Logs gc-log14-{source,restore}.log and inspect-log14-{source,restore}-after-gc.log. This proves complete sealedcycles and reachabledata integrity, not globalquiescence. No heldapplicationreaders or productionresources involved.

AsyncOperation page and Logtypepage reviewedacceptedunchanged. Asyncownership/callback guarantees useexisting executor; code-only guarantees identifiedas such. Logtype rawcapturedcoordinate/shapes/range/retention match uniformkind14 implementation and latestactualadmission/consumer/recovery/log/backup proofs.

Peertypepage acceptedunchanged: reviewed four exactexample compile/execute outputs and concrete API parity note. Integrated all21gatedlive checks started onfreshrootexclusive edb_cutover_release + _restore, serialized; log live-integrated-kind14.log. This refreshes olderformatproofs, future rootmust inspect completion andtestcounts.

Query request type page reviewedacceptedunchanged: shared preparedstructure/invocations/bindings/control/querysource-vs-factsource codeandconcrete9query/3diagnostic/1asynccapture proof inspected. No wrappers or readsourcecontrol added.

Datautilitiespage acceptedunchanged afterrootreview of2newpassingconstructedEDN/cumulativeadmission tests; meaningfulpublicboundaries no corechange. All21gatedlive tests now passed0failed0ignored onfreshkind14release+restorecatalogs (live-integrated-kind14.log). Thisrefreshes olderformat source/model/clock/schema/readiness/queue/sync/native/CLI/fulltext/excision/log/backup evidence. Subsequent sharedintegrity/catalog corrections requireaffectedreruns. Administrationaudit identified live deepinspect narrowerthanbackupverifier, so previous healthyCLI results prove thenimplementedlatestchecks only; rootapprovedsharedoperations::integrity extraction and targetedretained/fulltext proofs.

Operations integration: personally reviewed maintenance/observability checks, process-signal wiring and shared integrity extraction against the former backup verifier. Point admission still authenticates publication/metadata/index coordinates; common policy preserves authorization, receipts, canonical replay, retained index and fulltext comparisons. CLI handlers are installed only on paths consuming the shared cancellation token; actual SIGINT/SIGTERM backup interruption leaves no completed point, and retry succeeds (admin-signals-live.log, 1 pass). Maintenance pause cancellation proof passes; three observability tests pass. Removed incidental formatting-only data.rs and Cargo.lock changes before committing.

Personally reviewed catalog retirement change and all four lifecycle tests. Retirement witnesses are payload rather than strong links; they prove admission after a completed sweep without pinning facts or demanding global writer quiescence. Collector checkpoint guard participates in route reservation only. Old issued route remains permanently dead. Live suite passes 4/4 in22.35s; strict focused Clippy passes. Retired flag1 encoding rejected. Committed reviewed changes through8f40151 and integrated only the separate source worktree. Broader backup/common-integrity regression is running; no release claim yet.

After the common verifier extraction: backup-common-integrity-live.log passes actual source-bearing partial/ready backup/restore (1 test,91.83s). recovery-common-integrity-live.log passes explicit repair, source/receipt preservation and retained damage detection (1 test,3.39s). Fresh deep inspections of the prior reclaimed source and restored fixtures both remain healthy, basis13, zero problems (inspect-log14-{source,restore}-common.log). Root's first CLI attempt supplied a nonexistent --deep-derived flag and exited with usage; corrected invocation uses default deep mode. No failed command was counted as validation.

Root reviewed all370 lines of backup_reads_source_cutover.rs, its focusedlive/clippy logs and the perfile note. Covers offline fulltext, program closure, source-sensitive reads/speculation, fixed point+log, no live credentials, cache/authentication boundary and fused failures. Committed ee71184. Transactor reference unchanged after fullworker review; root checked runtime/health implementation and actual health/TLS/standby evidence scope, plus shell syntax results. No need for another HTTP interface or duplicated attribution layer.

Refreshed operational acceptance on currentkind14/core: dedicated separate PostgreSQL crash instance /tmp/edb-cutover-ha-current-20261007, port55438, explicit matched data-directory and durable settings. ha-current-workflow.log passes actual immediate crash/WAL recovery, all six current/history fingerprints, receipt-first deferred-source retry, changed-source rejection, cross-source replacement, old values/log and independent reopen; example exits0. HA harness always stops only its own fixture; cleanup succeeded.

Refreshed stock application against fresh logical databases in disposable edb_cutover_release: app-current-{local,tls}-{first,retry,offline}.log allpass. Authenticated TLS uses only existing disposable cert/token paths and correct DNSname; no secrets emitted. Local standby takeover succeeds and full application passes on successor. app-current-harness.log proves standby live200/ready503 then active GET/HEAD200, empty HEAD bodies. Both writers and standby stopped; exact saved references reopen with writers stopped and all remote credential env removed. This refresh supersedes older pre-kind14 operational evidence; no Course_Academy resource was involved.

Root reviewed complete SSD, log cache and transaction hints regression files, all focused live/offline logs, and codec test changes. SSD3offline+1live pass with real filesystem ownership (sandbox projects ancestors as nobody); no production policy relaxed. Namespace proof uses distinct files and selective purge rather than incorrectly expecting zero reuse within a single cold open. Log2live pass over130transactions/3pages: warm/reopened iteration zeroSQL, partial chunk caching reports actual fetch bytes without claiming whole transaction hits. Hints eager preview and twelve-prefix roundtrip pass; real PostgreSQL/TLS test proves receipt identity independent of advisory hints after source lookup withdrawal, changed-source conflict, valid oversize hints ignored. Root focused strict hints Clippy passes. Product pages unchanged. No production behavior changes in these commits; root final combined tests remain due.

Seventh integrated all-target default run passed182tests,0failures,32explicitliveignores across57binaries (tests-integrated-seventh.log); strict all-target Clippy passes (clippy-seventh.log). Ran under normal host filesystem ownership to preserve SSD private-directory checks instead of weakening them for projected sandbox metadata. Every newly introduced live check already has actual focused execution; final combined live refresh and later doc workers remain outstanding. Remote/AWS deployment references reviewed unchanged, with current operational evidence and explicit distinction between EDB local mechanisms and unperformed AWS provisioning.

Root reviewed the final six assigned notes, source diffs, and focused evidence: atomic update sets, missing-attribute query/Pull behavior, byte equality, actual offline vendored distribution transfer/build, timestamp clock boundaries and historical verbatim preservation. Historical report remains hash-identical. The recursive component defect is repaired at the shared Visit boundary rather than by adding another timestamp mechanism; eager and overlay rejection are atomic, and ordinary multi-level retractions preserve all source history. Exact positive/negative test bodies inspected. Formatted only existing Rust style differences and committed 5b0c46b. README navigation review is complete and root-inspected: unchanged, no missing target. All111 per-file workers have finished; final code suite is now running on5b0c46b. No unresolved parity finding remains.

Final integrated default suite passes189tests,0fail,32explicitliveignores across60binaries; separate final-live suite passesall32,0fail,0ignored in170.39s, on5b0c46b. StrictClippy caught two err().expect() expressions in the final regression file; root replaced them with expect_err, reran both regressions successfully, and committed test-only2462863. Strictall-targetClippy now passes; productcodeunchanged. Remainingdocs/releasechecks resume on2462863 with per-check revisions recorded. Final host read-only protection confirms349protectedfilesunchanged,6originalprocess executablesmatching and3productionservicesactive.
