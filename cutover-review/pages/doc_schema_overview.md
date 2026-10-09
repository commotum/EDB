# Schema overview review

Observed situation and bounded problem: `docs-2/03_schema/00_schema.md` already
describes ordinary schema maps, one request source, generated timestamp
attribution, and cardinality over the shared current record. The objective is to
verify these promises against the current implementation, not change the settled
model. No source-specific parity gap has emerged from the assessor or existing
schema evidence.

Decision and tradeoffs: retain aligned prose and verify its public entry points
against concrete acceptance evidence; change only a demonstrated mismatch.
Duplicating schema/source validation or adding an adapter would couple concerns
already composed by db-before lowering, logical expansion and the single
source-aware material boundary. Those boundaries support local reasoning and
leave schema constraints independent of attribution and physical index readiness.

Acceptance: the exact two-attribute schema vector installs ordinary attributes;
the later Alice map binds installed names and upserts by identity; schema facts,
generated installation facts and each automatic timestamp share one resolved
source. Check new-source same-value contributions, cross-source replacement,
lookup references, db-before binding, immutable schema flags, AVET/SAVET shared
readiness and retained-index noHistory behavior using existing focused evidence.
Inspect existing passing logs/code rather than rerun unchanged broad suites.

## Actual changes and results

No documentation, library or committed test changes were necessary. The complete
page and authoritative NORTHSTAR/TIMESTAMP_SOURCE_DESIGN/CUTOVER contracts were
read. Applied the supplied talks' criteria for a bounded problem, evidence-led
experiments and independent composable responsibilities. The page preserves the
shared record and ordinary-source model, and its links resolve locally.

New exact-example evidence: `schema-overview-exact.rs` extracts this page's two
EDN vectors verbatim and executes them through `DatabaseValue::with_edn` against
the current isolated debug rlib. `schema-overview-exact.log` records success:
ten schema datoms (including two generated install facts and one timestamp),
three Alice datoms, all uniformly system-attributed; subsequent exact Alice input
upserts to the same entity, with same-source redundancy producing only time;
another source produces two material contributions without multiplying values;
lookup-based replacement emits one shared retraction and one new assertion under
the replacing source; the captured earlier value remains immutable. Compilation
and execution both succeeded. This is a speculative debug proof, not a new live
CLI or release run.

Code inspected: `src/transactor/request.rs` source-bearing public request shape;
`src/edn_transaction.rs` ordinary EDN maps, db-before attribute lookup and unique
lookup refs; `src/transaction/expand.rs` map/reference lowering;
`src/transaction/assess/mod.rs` one source resolution, ordinary generated
timestamp, schema hooks/successor validation, identity upsert, shared cardinality
replacement, value/holder counts and EAVS material redundancy;
`src/model/schema.rs` required properties, open schema facts, flag/type validation
and immutable fulltext/type/tuple definitions; `src/database_value/resolve.rs`
captured physical AVET readiness; `src/index/{metadata,visibility}.rs` readiness
and global EAV barriers; `src/index/recent/mod.rs` index-only noHistory permission
and source-preserving omission witnesses. The linked `examples/native_workflow.rs`
uses ordinary request source once for schema installation and data transactions.

Reused inspected evidence: `tests-integrated-second.log` records 43 passing
library checks and 55 passing integration checks, including all 14 schema tests.
Their source covers native predicates, managed tuples, complete proposed-schema
validation, unique-holder/source deduplication, ordinary source resolution,
noHistory global withdrawals and paired AVET/SAVET backfill. Live
`edn-workflow.log` proves the supplied complete schema fixture through CLI
`--file`/`--source`, deferred authoritative binding, generated timestamp, immutable
views, exact restart retries and retired-form rejection. `native-workflow.log`
records the linked native example passing schema installation, ordinary transact,
references, reads, immutable values and reopen. The existing exact schema tutorial
proof (`transact-schema-exact.rs/.log`) also confirms later transaction binding.
Unchanged broad suites were not rerun. Page-scoped `git diff --check` passed.

No unresolved page-specific behavior gap or shared-code dependency remains. The
review adds only this note and disposable proof artifacts outside the repository.
No commits or production resources were touched.
