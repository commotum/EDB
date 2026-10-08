# Historical Datomic time and source report review

Assigned file: `docs-2/DATOMIC_TIME_AND_SOURCE_REPORT.md` in the isolated
`/tmp/edb-cutover-impl-20261007` implementation copy. Read the report,
NORTHSTAR, TIMESTAMP_SOURCE_DESIGN, CUTOVER, WORKER_RULES, and the three supplied
Hickey transcripts in full.

The report is a preserved argument from the original five-field EDB/Datomic
baseline toward six-field sourced facts. Its five-field datoms, four-index
discussion, and EAV redundancy description explicitly concern Datomic or the
original EDB. The later six-slot examples and global-withdrawal rule describe
the source proposal. The closing recommendation to evaluate additional indexes
after cutover is historical; the authoritative NORTHSTAR now specifies SAEVT,
SAVET, their shared readiness, and source activity through SAEVT plus the log.
No report inconsistency requires a current-code change. Retrofitting the current
indexes or source into its baseline would destroy the comparison and violate
the requested verbatim preservation. The bounded decision is to retain the
file, validate its bytes and citations, and review the relevant current paths.

The original checkout remains read-only. All twelve absolute local `:line`
references exist, are within file bounds, and point to supporting definitions:
original datom and four orders (`model/datom.rs:8`); t/tx conversion
(`identity.rs:88`); leaf transaction storage (`tree/mod.rs:3838`); log effects
(`storage/log.rs:29`); shared recent transaction references (`recent/btset.rs:14`);
assessment and durable publication (`assess/mod.rs:415`, `authority/mod.rs:653`);
immutable values (`database_value/value.rs:16`); EAV materiality
(`assess/mod.rs:2448`); same-transaction conflict (`assess/mod.rs:2704`); and the
two local Datomic introductions. Inspected each referenced code/text neighborhood,
including the full original materiality function. The historical paths correctly
continue to target the unchanged baseline, rather than the cutover worktree.

Current evidence: `model/datom.rs` carries source and six orders;
`assess/mod.rs` resolves the request source through ordinary entity resolution,
adds the ordinary automatic instant, and attributes material changes uniformly.
Its `material_changes` checks live EAVS for assertions and global EAV existence
for withdrawals. `index/visibility.rs::current_eav_group` keeps each source's
newest surviving assertion after the EAV withdrawal barrier.
`database_value/views.rs::visible_current` probes complete EAV history, applies
the as-of basis, then `next_controlled` applies since/custom/source selection
and boundaries. These mechanisms preserve local reasoning: value visibility
is independent of the chosen physical index or callback predicate. The typed
datom composes with existing immutable leaves (`tree/mod.rs:4625`), shared recent
references, log datoms, immutable database values, and durable root publication;
there is no historical compatibility adapter or alternate source model.

Acceptance evidence inspected: `tests-integrated-seventh.log` records all four
`data_model_source_cutover` tests passing, including same-source redundancy,
another-source materiality, global withdrawal, and cross-source replacement.
Its six `database_filters_source_cutover` tests pass, including current/as-of/
history behavior, all six access paths, predicate non-revival, speculative
immutability, and seek visibility. Read the corresponding focused test bodies;
no new test or repeated whole suite is justified for this unchanged historical
artifact. Existing `doc-link-check.json` records 1,563 links and zero issues.

Actual audit: `historical-report-audit.json` records the twelve checked local
references, an empty report diff, and SHA-256
`337f37354fb5df17a902227fced7100bcec72bce66c348ea1eb9d8e84138afc2`
for both the implementation copy and integration worktree report. No docs/code
were edited and no build, runtime, PostgreSQL, or Course_Academy resources were
touched. No uncovered report-specific contract gap remains. Parent final QA
owns the in-progress component-cascade timestamp guard and private clock clamp
work; this review neither changed those paths nor substitutes for their results.
