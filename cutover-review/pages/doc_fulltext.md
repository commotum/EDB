# Native fulltext: preserve attributed search across views

Situation: the page promises immutable captured search, exact supplied-view validation, original `[entity, value, tx, score]` hits, and incremental historical assertion documents. The cutover already adds source to document payloads and rejects version 1 search objects; acceptance reuses the ordinary EAV cursor.

Cause to guard: derived documents formerly reconstructed source-free datoms. Source filtering before global EAV visibility could resurrect a contribution withdrawn by another source. Multiple live sources must remain separate assertion transactions in results without changing the public hit tuple.

Problem: verify that eager, speculative, persisted, and recent search consistently preserve source attribution through reconstruction, ranking corpus, global visibility, retained history, and physical noHistory removal.

Approaches: extending public hits with source would change the established query relation unnecessarily. Matching only a candidate's tx/source would change ordinary candidate-to-visible-EAV semantics and add another visibility policy. Preserve the EAV validation mechanism and source-bearing document payload; exercise that composed boundary with focused tests. Document source-filtered global withdrawal and contribution hits in natural reference prose.

Selected implementation: keep analysis/scoring, storage identity, view visibility, and public projection independent. E/T/text remains the document key because all facts in a transaction share one source; the payload retains that source for canonical reconstruction. Search candidate acceptance uses the existing global visibility cursor and exposes its assertion txs, with E/value/tx deduplication.

Acceptance: source duplicates produce two current tx hits; same-source repetition is redundant; source filters select current/historical contributions but cannot hide another source's global withdrawal; as-of/since/speculative remain correct; persisted decoding agrees with eager results; unindexed tail searches new assertions; noHistory incremental indexing removes all withdrawn contributions; old version search encodings reject. Existing budgets, analyzer, ordering and return shapes stay intact.

Implemented and verified: source-bearing payloads in bulk and delta records plus format 2 search pages/attachments (root's initial cutover implementation) remain unchanged. Added reconstruction/source-id validation and legacy-format rejection unit coverage. Kept established EAV candidate acceptance and annotated its global-view boundary. Updated only two natural paragraphs of the reference to distinguish database sources from contribution sources and explain global withdrawal under filters. Added tests/fulltext_source_cutover.rs, with a portable ignored durable test requiring EDB_TEST_POSTGRES_URL.

Results:
- `cargo test --lib fulltext:: -- --nocapture`: 2 passed, 0 failed.
- `cargo test --test fulltext_source_cutover -- --nocapture`: 2 passed, 1 explicitly ignored durable test, 0 failed.
- Explicit isolated PostgreSQL invocation `cargo test --test fulltext_source_cutover durable_fulltext -- --ignored --nocapture`: 1 passed, 0 failed (2.08 seconds).
- Focused git diff whitespace check: passed.

Coverage includes distinct same-EAV contribution txs, redundant same-source additions, current/history/as-of/since/source predicates, reassertion, typed four-column query relation and named database source joins, persisted source payloads, committed recent tail, persisted speculative withdrawal, captured old attachment behavior, reopen, and incremental noHistory document removal. No compatibility branch, public hit-shape change, query-planner edit, or independent visibility implementation was added. No remaining fulltext/source gap identified by these checks; broad integration remains the parent's release check.
