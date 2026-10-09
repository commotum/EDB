# Data modeling review

Situation and bounded problem: the complete page already describes ordinary
identified enumeration entities, db-before binding, one request source, shared
cardinality, and distinct query/Pull/entity presentation. NORTHSTAR,
TIMESTAMP_SOURCE_DESIGN, CUTOVER and the three supplied talks were read. Prior
schema/Pull/entity/global-visibility proofs cover the common source mechanics;
the remaining acceptance question is whether this page's exact country examples
compose through EDN transactions and schema-aware query constants, including
ident renaming and shared noncomponent references.

Decision and tradeoffs: retain aligned prose and the existing ordinary mechanisms.
An enumeration-specific resolver or source-specific vocabulary would couple
domain naming with attribution and duplicate db-before reference resolution.
Reusing normal schema/ident resolution, the EAVS material boundary and distinct
projection permits local reasoning and lets vocabulary evolve independently.
No compatibility path or new validation mechanism is proposed. Write only a
uniquely owned exact-example acceptance test; coordinate any demonstrated
shared-code mismatch before editing it.

Acceptance: execute the five EDN blocks verbatim; attribute schema/application
facts and automatic time uniformly to existing source 99; reject unknown or
same-transaction introduced reference idents and new application attributes;
observe nested Pull ident, plain reference ID map and lazy forward keyword;
retain one country under same-value contributions and cross-source replacement;
permit another vocabulary's ident without a membership rule; keep keyword values
literal; rename a country without changing existing ref IDs, preserving the
captured historical view; retract an artist without retracting its shared country.
Reuse inspected 14-schema, 7-Pull, 2-entity and global-filter/query acceptance
evidence rather than rerun their unchanged broad suites.

## Actual changes and evidence

The assigned page remains unchanged. No library mismatch was found or shared code
edited. Added `tests/data_modeling_cutover.rs`: three independent speculative
acceptance tests extract and execute all five page EDN blocks verbatim and cover
the checks above. Both indexed and forced-scan database queries return the stated
country results; raw tuples retain literal keywords. A second contributor adds a
material country contribution without changing Pull output; a later replacement
globally withdraws it even under a source filter, while as-of preserves Canada.
Rename and artist deletion leave the country target identity and shared data
intact. Fixtures use checked schema entity conversion; an early fixture cast of
a user entity to u32 incorrectly aliased a builtin and was corrected. An initial
fixture compile error assumed EntityValue equality; scalar shape is now matched
explicitly. Neither required a production workaround.

`cargo test --offline --test data_modeling_cutover` passed 3/3, with no ignored
tests, in 0.15 seconds; `data-modeling-exact.log` retains the final successful run.
Only the owned test was formatted. Page/test whitespace and formatting checks
passed. No new live service or PostgreSQL execution was performed.

Code inspected: `src/edn_transaction.rs` db-before map attribute binding, ordinary
ref/keyword lowering and many collection shape; `src/transaction/expand.rs`
ordinary map expansion and noncomponent authoring policy;
`src/transaction/assess/mod.rs` ident resolution, upsert/value constraints, shared
cardinality/uniqueness, source-aware material_changes, and component-only recursive
retractEntity; `src/model/idents.rs` historical assertion-derived names/aliases;
`src/query/patterns.rs` typed database constant resolution and literal raw matching;
`src/pull/{attributes,entity,execute}.rs` logical-value projection, noncomponent ID
maps, explicit nesting, and lazy identified forward keyword presentation;
`src/index/visibility.rs` global EAV withdrawal before source restriction;
`src/transactor/request.rs` source-bearing ordinary request.

Inspected reused evidence: `tests-integrated-third.log` records 114 passing checks,
zero failures and 11 explicitly ignored live tests, including all 14 schema,
7 Pull, 2 entity, 9 query and 6 database-filter tests. Their relevant source covers
ordinary constraints/native membership predicates, duplicate contributions,
identity upsert, global filtered withdrawal and distinct values. The separate
`edn-workflow.log` records both tests passing with explicit isolated PostgreSQL:
CLI file/source submission, schema binding, generated timestamp, repeated sources,
exact restart retries and retired-form rejection. `native-workflow.log` records
the native schema/transaction/read/reopen example passing. Those existing live
proofs cover submission/persistence mechanics; this worker adds the page's exact
enumeration acceptance through native speculation.

No unresolved page-specific behavior gap or shared-code dependency remains. No
commits or production resources were touched.
