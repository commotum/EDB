# Transaction model cutover

Situation: the assigned page states the settled transaction model accurately: one request source, complete immutable successors, EAVS redundancy, shared-value constraints and receipt-first exact retries. Authority already hashes `request.source` in canonical intent and checks its receipt before fresh assessment. The isolated native workflow nevertheless committed basis 4 and failed waiting for peer observation.

Problem: establish durable parity for this contract and diagnose why peer observation stops advancing after a known commit, without coupling source resolution to retries or weakening publication semantics.

Approaches: reuse the authority's existing shared assessor, log and exact receipt mechanisms; add focused isolated durable acceptance coverage. Patching waits or dropping observations could hide a storage/adoption error, so first expose the underlying observation error. No compatibility path or new timestamp protocol is appropriate.

Selection: inspect observation adoption and storage catch-up, fix the precise mechanism only after evidence identifies it; exercise source references and generated facts through ordinary durable requests. Independent responsibilities stay separated: request identity is immutable caller data, assessment resolves fresh entities, publication owns durability, and observation adopts complete values.

Acceptance: isolated PostgreSQL only; empty timestamp attributed to source; same-value sources retained without multiplying values/cardinality; replacement and global retract preserve history; reopen preserves attribution; source ident/lookup references removed after commit still permit exact accepted retries; changed source with the same request key conflicts. Build/test only this implementation copy's target directory.

## Changes and results

Reviewed the page in full against NORTHSTAR, timestamp reference and CUTOVER; retained its prose because it already accurately specifies the contract. Inspected shared assessment/pipeline, authority publication and receipt lookup, client request hashing, peer adoption/catch-up, durable capture and log encoding. Authority and client already compose the correct source-bearing request identity and receipt-first lookup; no duplicate source resolver or retry adapter was added.

The reproduced observation error was `observation/invalid-channel`. Provider `notification_channel` emits `edb_r_` plus 48 lowercase hex digits (54 total characters). The listener incorrectly demanded 57 total and inspected a suffix at byte 9. This rejected every legitimate LISTEN channel. Independent readers then had only the default 30-second anti-entropy interval, exceeding the workflow's 10-second wait. Parent approved ownership of `src/observation/notices.rs`. Corrected validation with `strip_prefix` and an exact safe lowercase hexadecimal suffix; it remains independent of interpolation and safely rejects malformed/non-ASCII text. Added validator regression coverage for valid provider shape, uppercase, wrong prefix, short/non-ASCII suffix and SQL-looking trailing text.

Added `tests/transaction_model_durable_cutover.rs` (ignored by default, explicitly bound to the authorized isolated PostgreSQL socket and unique logical database names). It verifies one automatic timestamp for empty input, source attribution on all schema/descriptive/tempid/map/component facts, generated component cascade and CAS retractions, two source assertions of one cardinality-one value, EAVS redundancy, global visibility before SAEVT source filtering, as-of attribution, consolidation with shared-value contributions, both ident and lookup source exact retries after their identifying fact is retracted, differing-source request identity conflicts, global EAV retraction with two live sources, and consolidated reopen/history retention.

Observed validation:
- `cargo test --lib provider_notice_channel_accepts_only_the_safe_identifier_shape`: 1 passed.
- `cargo test --test transaction_model_durable_cutover -- --ignored --nocapture`: 2 passed, including nested expansion/cascade and durable consolidation.
- `EDB_POSTGRES_URL='host=/tmp/edb-cutover-pg-20261007/socket port=55437 user=jake dbname=edb_cutover sslmode=disable' cargo run --example native_workflow`: PASS at t=4; previously failed observation now completes, including persisted function expansion, tuple references, lookup keys, query/Pull, immutable log/index-pull, deferred transforms, history, chained speculation and reopen.

All builds used `/tmp/edb-cutover-impl-20261007/target`. No existing EDB/Course_Academy resources were touched. No commit was made. Parent owns integration and full release validation. No outstanding page-specific code gap was found after these checks; broader backup/excision/failover guarantees remain in their assigned scopes.
