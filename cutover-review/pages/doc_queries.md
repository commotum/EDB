# Query overview review

`docs-2/05_query_and_pull/00_queries.md` is already aligned with NORTHSTAR,
TIMESTAMP_SOURCE_DESIGN and CUTOVER: raw facts expose E/A/V/T/S/assertion,
`DataPattern.source` selects the named invocation input, and `fact_source`
selects an entity coordinate. No documentation mismatch or page-specific code
gap was found. The bounded review question is whether source survives ordinary
tuple/database/log composition without changing query result forms or captured
view semantics.

Retain the existing page and reuse the implemented mechanisms. A separate
source relation or source-specific matching adapter would couple attribution to
input selection and duplicate entity resolution. Optional overview rewrites or
duplicate tests would add no acceptance evidence. Check raw six-slot order,
named-input independence, source identity joins, global withdrawal before source
selection, find/:with distinctness, captured database projection, log tuple
shape and four-column fulltext composition against existing code/tests.

Inspected implementation: `query/model.rs` separates named source from optional
fact source; `query/sources.rs::datom_tuple` is shared by raw datoms and TxData;
`query/patterns.rs` resolves/unifies source with existing entity helpers and
selects SAEVT/SAVET; `query/join.rs` matches literal raw cells with logical
equality and retains structural width before omitting blanks. Database-value
`views.rs::visible_current` resolves complete EAV history before custom/time
selection; query cursors delegate to that visibility mechanism. `query/results.rs`
uses the existing find/:with basis; `query/execute.rs` clones supplied database
values and validates actual sources per invocation. `query/fulltext.rs` preserves
four-column output and composes with ordinary patterns. Prepared-cache keys use
the source-bearing AST encoder; schemas and inputs remain execution concerns.
Numeric domains/budgets (`numeric.rs`, `control.rs`) remain independent of source
and result basis formation. No extra adapter, compatibility path or abstraction
is needed; index locality can change without changing attribution or projection.

Inspected acceptance evidence: `tests-integrated-fourth.log` records 150 passed,
0 failed and 14 explicit live ignores. Its nine `query_source_cutover` checks
cover source identity/entity joins, set/:with contribution behavior, global
withdrawal across views/scan modes, SAEVT/SAVET/range selection, raw six-slot
literal semantics and short-row arity, prepared keys and portable encoding.
The two `database_values_source_cutover` checks include mixed raw/database
six-slot joins and deferred Pull over captured contributors after replacement;
`database-values-tests.log` separately records 2 passed. The exact documented
report query preserves retractions and datom tuple order in
`processing_transactions_cutover`; `processing-transactions-offline.log` records
1 passed and 1 explicit live ignore. Two offline `fulltext_source_cutover` checks
prove four-column output with independent named input/fact source and global
withdrawal across current/history/temporal/filtered views. Seven Pull checks
cover ordinary projection without contribution multiplicity.

Inspected root's new `log-api-live-root.log`: both actual log tests passed,
0 failed, 0 ignored (1.25s). The test bodies prove captured log exclusion of later
commits, inclusive/exclusive T/transaction-ref/instant bounds, TxIds collection
and TxData six-column/scalar/collection forms, and source-activity/log composition.
The fulltext worker's `doc_fulltext.md` records actual isolated durable fulltext
1/1 passing execution (2.08s); raw stdout for that run was not inspected here.

Documentation/code changes: none; `git diff --` for the page is empty and its
retired-form search has no matches. No new test run was needed for the unchanged
page; passing acceptance was inspected rather than duplicated. No known
page-specific parity gap or implementation dependency remains. Root retains
ownership of final integration and complete release acceptance.
