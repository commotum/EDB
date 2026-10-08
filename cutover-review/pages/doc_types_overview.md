# Peer type overview review

Assigned page: `docs-2/07_peer_api/05_types.md` (read in full), with full
NORTHSTAR, TIMESTAMP_SOURCE_DESIGN, CUTOVER, and the three supplied talks.

The page already describes the six-field datom, captured immutable read value,
eager memory value, explicit/attached endpoints, lazy Entity, log records,
query inputs, EDN modules, and filter callback accurately. The bounded question
was whether any listed type/name/shape or embedding claim had lost parity in
the cutover. No discrepancy was found. Keeping the overview unchanged retains
its useful separation of type responsibilities; expanding it into transaction
policy or adding wrappers/glossary material would duplicate the detailed pages
without improving composability or local reasoning. Selected implementation:
no documentation or code edits, and no redundant new tests.

Concrete parity checks:

- `Cargo.toml` names `edb-core`; `src/lib.rs` exports every principal type in
  the table and all five named public EDN modules. `BoundEdnQuery` is correctly
  qualified under `edn_query` and retains prepared query plus bound inputs.
- `model/datom.rs` has public E/A/V/T/S/bool fields; `database.rs` owns private
  immutable eager state and `bootstrap() -> Result<Database, SemanticError>`.
  `database_value/value.rs` captures eager, block, or speculative immutable
  bases. `pull/entity.rs` pins lazy associative access to that same value.
- `application/connection.rs` captures `DatabaseValue` through its independent
  advancing peer, submits attached requests, and returns original report shapes.
  `application/transport/{local,remote}/client.rs` supplies explicitly selected
  socket/remote endpoints. `peer/live.rs` and `peer/snapshot.rs` delegate local
  query/Pull to captured values; cached immutable reads remain in the peer.
  `storage/catalog/mod.rs` supplies naming and creation.
- `runtime/executor/mod.rs` and `application/asynchronous/client.rs` implement
  Future with Result outputs and consuming `on_complete` callbacks.
  `database_value/views.rs:529` has exactly the documented filter bound.
  `database_value/log.rs` retains `LogTransaction { t, data: Vec<Datom> }` and
  a captured `LogValue`, preserving source-bearing transaction records.

Evidence inspected, not rerun: `tests-integrated-fifth.log` contains 37 result
sections totaling 158 passed, 0 failed, 16 explicitly ignored. Relevant test
bodies/results include `programming_edn_cutover` (4 passed: standalone public
API construction, sourced transaction/timestamp, captured basis, BoundEdnQuery),
`database_filters_source_cutover` (6 passed: global withdrawals before source
predicates and temporal reads), and `entities_source_cutover` (2 passed:
value projection and lazy references/components). Actual isolated live stdout:
`client-tutorial-live.log` 1 passed (sync/async socket source, six-slot report,
exact retry); `peer-snapshot-readiness-after.log` 1 passed (paired readiness);
`log-api-live-root.log` 2 passed (captured log/query shapes and same-basis source
activity). Existing `doc-link-check.json` checks 1563 links/fragments, 0 issues.

Results: assigned page unchanged; no repository files changed; no new commands
executed against PostgreSQL. No remaining gap or edit dependency for this
overview. Related detailed API and log resource admission reviews remain owned
by `doc_native_api_reference` and `doc_change_consumers`; this review does not
claim to complete those separate contracts.
