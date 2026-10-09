# Query the Data acceptance

Observed situation: the complete introductory query page uses a captured
DatabaseValue, the existing EDN parser/binder/executor and query-result encoder,
short E/A patterns, blanks, joins, and set-of-tuples results. It does not supply
transactions or show the old five-field datom shape. NORTHSTAR, the timestamp
reference and CUTOVER require first-class attribution while allowing ordinary
queries to omit source. Existing exact in-memory movie output, exact schema
acceptance and nine query-cutover checks already establish most of this page.

Cause and bounded problem: source adds a coordinate to underlying facts, but
beginner queries intentionally project entity/value rather than source. Verify
that this remains composable through the existing short-pattern mechanism and
that every Rust fragment and output on this exact page agrees with the current
public APIs. No engine discrepancy has been found; do not create changes merely
to produce a diff or weaken documentation to accommodate one.

Approaches and tradeoffs: reuse passing general evidence and inspect the engine;
compile the exact concatenation of this page's Rust blocks, then execute those
same helpers and four queries against ordinary speculative schema/movie data.
A fresh live writer suite duplicates unchanged connection/CLI evidence; a new
source-aware helper would entangle the introductory result projection with
attribution. Choose bounded exact-fragment checks and reuse existing live EDN
and connection proof for transport. Coordinate any demonstrated shared-code gap
with root before editing.

Concrete acceptance: the unchanged complete Rust program compiles against the
current isolated library. Its helpers and queries return three entity IDs,
three titles, the two 1985 titles and the two title/year/genre tuples exactly as
shown (relation order is irrelevant). The same captured value remains stable
after a speculative update. Existing contribution-distinctness/query tests
prove omitted source does not multiply projected values; existing short-pattern
checks prove E/A syntax; source filtering never resurrects globally withdrawn
facts. No retired timestamp/source operation appears on this page.

Design evaluation following the supplied talks: distinguish connection capture,
query syntax, physical clause planning, source attribution and result projection.
Each has its existing responsibility; q composes parsing, binding and execution,
while show composes encoding and rendering. Keep immutable database values as
the basis for local reasoning. This preserves independent change in index
selection and source tracking without changing query/result forms or adding
compatibility paths. Append actual results, not merely the plan.

## Actual changes and results

No documentation or engine changes were needed. All eleven Rust blocks from
the assigned page were concatenated verbatim into `query-data-exact.rs` and
compiled successfully against the latest isolated debug rlib. A second scratch
program replaced only connection acquisition with the existing in-memory movie
schema/data setup, retained the page's exact q/show helpers and all four query
fragments, and compared its rendered relations to every exact EDN result on the
page as unordered row sets. All checks passed. Its additional update assertion
confirmed that the captured original DatabaseValue retains the original genre
while db-after exposes the replacement. `query-data-exact.log` records both
successful compilations and successful execution; `query-data-exact.py` retains
the reproducible extraction and assertions.

The exact rendered results contain the three documented IDs, all three titles,
the two 1985 titles, and the two title/year/genre tuples. Source is omitted from
these projections through the existing find-basis mechanism. The untouched
complete program compiled with no warnings; the scratch in-memory program has
only unused connection/environment imports retained from the exact helper
block. An initial scratch attempt called database_value on an already captured
DatabaseValue; correcting that harness-only mistake resolved compilation and
exposed no library gap. The shell provides python3 rather than python.

Code inspected: `application/connection.rs` configured read connection and db
capture; `peer/live.rs` immutable snapshot capture;
`postgres_connection/environment.rs` documented environment acquisition;
`database_value/value.rs` shared immutable basis ownership and speculative report;
`database_value/views.rs` global EAV visibility upstream of read selection;
`edn_query.rs` default $ input, parse/bind/execute and short-pattern compilation;
`query/patterns.rs` existing short relation-to-datom probes, schema-aware
attribute matching, source/entity unification and index selection;
`query/{execute,dependencies}.rs` ready-clause scheduling;
`query/results.rs` find/:with basis and set projection;
`edn_value.rs` EDN relation result encoding.

Evidence reused after inspection: `in-memory-tutorial.log` proves all 20 previous
movie tutorial blocks with these helpers and equivalent queries;
`transact-schema-exact.log` proves the exact preceding schema after Hello world;
`doc_query_reference.md` and `tests-integrated-second.log` record all nine query
checks passing, and inspected `tests/query_source_cutover.rs` verifies short E/A
syntax, omitted-source projection distinctness, source identity resolution and
no resurrection after a globally attributed withdrawal. The live EDN workflow
log/test proves CLI queries use the same parser/binder/executor, actual peer
reads, and current/as-of/since/history behavior. Existing configured connection
and application evidence supports the unchanged connection acquisition. This
worker compiled but did not run this exact page's durable Connection program;
its fresh execution proof uses speculative in-memory data. No fresh release or
live PostgreSQL run is claimed, and no passing broad suite was duplicated.

Owned-page git diff and diff-check are clean. Scratch files are exclusively in
the review directory; no committed tests, shared code, original EDB checkout or
Course_Academy resources were changed. No unresolved page-specific parity gap or
shared-code dependency remains.
