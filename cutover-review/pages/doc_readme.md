# docs-2/README.md review

Read README, NORTHSTAR, TIMESTAMP_SOURCE_DESIGN, CUTOVER, WORKER_RULES, and all
three supplied Hickey transcripts in full. README is a navigation page with
one substantive opening claim: facts, schema, and transactions are database
data. It already sends new readers to the introduction, offers tutorials and
grouped references, names NORTHSTAR, and links the transaction source/timestamp
reference under Transactions. No old timestamp-source syntax or unsupported
API signature appears here; no mismatch was found.

Bounded question: can a reader reach the settled model and documented entry
points without a stale link or contradictory overview? Keeping this aligned
index preserves one navigation responsibility. Repeating transaction rules in
the introduction would duplicate the authoritative reference and introduce a
second maintenance point; reorganizing the sections would change familiar
navigation without solving an observed problem. Selected plan: verify links,
trace the opening claim and named API surfaces, inspect existing acceptance
evidence, and leave the page unchanged if all checks agree. No code gap was
identified and no code ownership request was necessary.

Evidence and results:

- A fresh Python read of README found 110 links to 109 distinct targets, zero
  missing files, and coverage of every docs-2 Markdown page except README and
  the intentionally historical report. Inspected doc-link-check.json records
  1,563 local links/fragments with zero issues.
- Cargo.toml names edb-core; src/lib.rs exports the native, synchronous,
  asynchronous, database, datom, entity, log, query, attribute and peer types
  named by the index. src/model/datom.rs retains six datom fields and six
  index orders. src/transactor/request.rs requires one source; no default or
  per-operation source appears in src/transaction/forms.rs. The EDN request
  and speculation entry points in src/edn_transaction.rs carry that source.
- src/transaction/assess/mod.rs:393-530 resolves source through ordinary entity
  resolution, derives schema from information, adds the ordinary timestamp,
  and passes one resolved source into material_changes (2650 onward).
  src/model/vocabulary.rs installs :db/txInstant as an ordinary instant,
  cardinality-one attribute. validate_tx_instant_forms rejects timestamp
  mutation; expand_retract_entity now checks transaction identity at every
  component visit, protecting timestamp facts through recursive traversal.
- Inspected tests/data_model_source_cutover.rs and tests-integrated-sixth.log:
  canonical genesis/codec, distinct source contributions, and global
  withdrawal tests pass. The log also records generated facts sharing source,
  ordinary source resolution, and source-first activity traversal passing.
  timestamp-source-clock.log records the backward-clock/explicit-bounds test
  passing. live-integrated-kind14.log records actual source-aware backup,
  recovery, consumer, and client tutorial checks passing, distinct from
  explicitly ignored live tests in the ordinary integrated suite.

Actual changes: this review note only; git diff for README is empty. No new
Cargo runs or shared edits were made. No README-specific parity gap remains.
The evidence above supports an aligned navigation page, not a replacement for
root's final integrated checks of the current working tree. Keeping reference
content in its existing pages maintains independent concerns, composes through
ordinary links, supports local reasoning, and lets an API page change without
creating another API description in the index.
