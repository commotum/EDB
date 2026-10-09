# Application data cutover review

Assigned page: `docs-2/05_query_and_pull/01_application_data.md`.

The page is aligned with NORTHSTAR and the timestamp/source contract. Its
named database/relation sources describe query invocation inputs, independently
of fact attribution. No obsolete datom shape, timestamp operation, transaction
source syntax, or changed callback ABI appears. The bounded question was
whether the cutover preserves general application relations and their joins to
an exact database value. No implementation gap was found.

Retaining the page and shared evaluator preserves its existing API and local
reasoning. Adding another adapter or source glossary would introduce work
without correcting a mismatch; changing the page to enumerate unrelated
cutover internals would obscure its application-data purpose. Selected action:
leave documentation and code unchanged, inspect the existing mechanisms and
reuse concrete cutover evidence, then run its exact wide-row CLI example.

Parity and implementation inspected:

- `src/query/model.rs`, `sources.rs`, `patterns.rs`, and `join.rs`: named
  `DataPattern.source` selects the invocation's input; `fact_source` occupies
  datom column five. Raw datoms are six literal E/A/V/T/S/bool cells. Arbitrary
  relations retain their own widths and literal matching. Database patterns
  reuse entity resolution and the ordinary visibility cursor.
- `src/database_value/cursor.rs` and `views.rs`: query prefix reads delegate
  to the public captured-value path; current/as-of visibility resolves a
  complete EAV group before source, since, or custom selection.
- `src/query/extensions.rs`, `functions.rs`, `prepare.rs`, `control.rs`,
  `value.rs`; `src/edn_query.rs`, `edn_value.rs`: pure and database callbacks
  remain separate, general values retain their domain and logical equality,
  prepared queries retain structure, and invocation controls remain shared.
- `src/program_query.rs`, `program.rs`, and `encoding/program_query.rs`:
  persisted general arguments and relation sources reuse the same engine;
  exact database inputs and scalar/general output shapes remain intact.
  `src/bin/edb/data.rs` retains local relations, ordinary database descriptors,
  stdin admission, and the 16 MiB CLI value allowance.

Acceptance evidence inspected in `tests-integrated-fourth.log`: all nine
`query_source_cutover` tests pass (including six-column raw rows, short-row
arity, source joins, prepared cache keys, codec, and global withdrawal); both
`database_values_source_cutover` tests pass (mixed named/raw joins preserve
captured contributors and later replacement); the non-live
`processing_transactions_cutover` test passes (report datoms preserve source
and retraction while joining names). Relevant `edb_interfaces` and
`programming_edn_cutover` checks also pass. The combined log records 150 passed,
0 failed, and 14 explicit live ignores; no ignored test is claimed as run here.

Actual focused smoke: extracted both EDN blocks verbatim from the assigned
page, removed PostgreSQL configuration from the child environment, and invoked
the isolated `target/debug/edb`. Exit 0, no stderr, exact result
`#{["Alice" {:priority 1} #app/state :ready]}`. Inputs and result are retained in
`application-data-smoke/`. This checks the seven-column relation's nil,
character, inert tag, nested map/set, and projection without storage access.

Result: no page/code changes, no extra tests needed, no remaining gaps for this
page. Query selection, captured state, and attribution compose through the
existing paths; no compatibility mechanism or duplicated policy was added.
