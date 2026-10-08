# Synchronous client API review

Read the complete assigned page, NORTHSTAR, TIMESTAMP_SOURCE_DESIGN, CUTOVER,
WORKER_RULES, and the three local talks. The page already describes the settled
model: six-field datoms, six indexes, required request-level source, ordinary
source resolution, automatic source-bearing timestamp, preserved forms and
argument order, and exact receipt-first retries. The bounded question was
whether these public signatures and source-sensitive behaviors match their
implementations. No documentation or implementation gap was found.

Decision: leave the page and code unchanged. Adding an attribution wrapper or
optional compatibility path would duplicate the existing entity/request model;
adding implementation notes to this API summary would create maintenance work
without addressing an unmet behavior. Attribution remains request data, time
selection remains clock policy, and waiting remains connection policy.
Speculation and durable submission compose the shared assessor. Captured
DatabaseValue and LogValue reads can be reasoned about independently of mutable
connection advancement and async scheduling. This separation permits transport
and execution changes without redefining source semantics.

## Code parity

- `transactor/request.rs` and `edn_transaction.rs`: `new`, `from_forms`, and
  `from_edn` require one `EntityRef`; request fields and report shapes agree.
  `transactor/authority/mod.rs:522` includes source/basis/time in canonical
  request identity and checks the exact receipt before fresh assessment/clock.
  `transaction/assess/mod.rs:388` lowers deferred EDN, allocates tempids through
  ordinary rules, resolves source, inserts one ordinary timestamp, and stamps
  material explicit/generated changes. `transaction/pipeline.rs` and
  `edn_transaction.rs` expose the documented speculative signatures and view
  preservation; no client source lookup is inserted.
- `application/connection.rs`: configured read connection/cache signatures,
  captured `db`/`log`, independent reader/writer ownership, local `sync_to`,
  PostgreSQL `sync`, and ticket wait/observation-error handling match the page.
  `storage/catalog/mod.rs` matches create/list/retire signatures and results.
- `model/datom.rs`, `database_value/{cursor,views}.rs`, and `pull/index.rs` match
  six datom fields/orders, collecting versus fallible cursor returns, global
  visibility before view selection, boundary seek semantics, and index-Pull
  selected-datom multiplicity with distinct ordinary Pull values per projection.
- `query/{control,execute,sequence}.rs`, `pull/{control,execute}.rs`,
  `database_stats.rs`, and `database_value/log.rs` match controls, defaults,
  eager relational/lazy Pull sequencing, immutable sources/shared budgets,
  history counts, and captured transaction-range bounds/fused iteration.

## Acceptance evidence inspected

Inspected existing test bodies and actual logs; no redundant suite was run.
`tests-integrated-fifth.log` records 158 passes, zero failures, 16 explicit live
ignores. Relevant passing checks include `mixed_sequence_retains_captured_contributors_and_projects_only_consumed_rows`,
all six boundary arities/global filtered visibility, source/tempid/lookup and
request identity, automatic/explicit timestamps, and Pull/index-Pull semantics.
The ignored live cases are not counted as executions.

Separate actual execution logs: `client-tutorial-live.log` has one pass for
sync/async socket source preservation and exact replay; `synchronization-live.log`
has one pass for captured immutable values/local waits/queued deadlines;
`log-api-live-root.log` has two passes for captured ranges and same-basis stable
source-activity pages. Root's unchanged-link proof, `doc-link-check.json`, checks
1563 links/fragments with zero issues.

No source/doc edits, builds, new tests, live database operations, or commits.
No remaining gap or dependency in this page's scope. The concurrent log-format
admission and PostgreSQL error-classification changes are owned by their workers
and do not justify changes to this aligned API summary.
