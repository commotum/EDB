# Persisted programs: source cutover review

Observed: the page already specifies ordinary program write operands, enclosing request source, automatic timestamps, explicit install source, canonical programs, complete fixed dependency closures, and immutable db-before execution. ABI 13 and raw EDN entity references have been partially wired in. Direct binding lookup now counts distinct function values across source datoms. Source must remain independent of instruction operands and binding attribution.

Diagnosis and bounded problem: finish source-safe persisted program execution without a fallback source or old ABI path. Raw references must retain canonical identity before db-before lowering; closure marking must find fixed Function values in every query term, including the source coordinate. Multiple identical binding contributions must neither break resolution nor change program output.

Approaches: repeating source on VM writes would entangle bytecode, binding attribution, and transaction attribution and violate ordinary form arities. A wrapper/default would hide request information. Reusing canonical EDN encoding, ordinary reference lowering, common transaction expansion, and existing closure traversal keeps those concerns independently testable and permits schema/reference changes without a second resolver.

Selected implementation: inspect and repair only program/runtime and binding traversal gaps, preserve the documented names and shapes, and add focused tests for raw-reference canonical bytes/hash/ABI rejection, deferred reference lowering, dependency collection, duplicate binding source contributions, and generated nested forms receiving the enclosing source. Coordinate operator/encoding/transaction gaps with their owners.

Acceptance: focused Rust tests pass in the isolated target directory; ABI 12 bytes are rejected; canonically equivalent raw references encode/hash equally; schema-dependent raw refs resolve at invocation db-before; Function hashes in raw refs and source query terms remain reachable; identical :db/fn from two sources resolves once; every emitted write plus timestamp has the submitted source.

Changes and validation: pending.

Implemented and reviewed:
- Reviewed source-explicit operator installation, canonical request digest before staging/dependency inspection, ordinary write operands, nested call expansion at the same immutable db-before, and existing automatic timestamp generation. No default source or retired timestamp instruction was found in the relevant program/install paths.
- Completed VM `LoadOne`/`LoadMany` distinct-value behavior while admitting and charging every source candidate. Existing direct function binding lookup was verified against identical Function contributions from two sources, direct invocation, speculation, and global retraction followed by a source filter.
- Added query pattern `fact_source` to the existing fixed-dependency walk. Verified raw EDN Function tags (including nested inert tags, map/set contents, dormant branches and Database callable references) survive canonical program round trips and complete closure traversal.
- Canonicalized unresolved EDN references at the common VM push boundary, recursively through runtime collections and LookupInput transaction values, so canonical-hash-equivalent raw literal spellings cannot produce different equality results. Emitted Database callable literals use the same reference normalizer. Canonicalization does not resolve entities or consult mutable state.
- Accounted for retained raw EDN spelling as well as canonical bytes, preventing large comments/whitespace from evading runtime retention limits. Caller-spelling admission may use more resources than canonical stored code; admitted ordinary VM results are identical.
- Added a small natural page clarification: function binding resolution and database instructions count distinct values, independently of contributor count.

Actual validation in `/tmp/edb-cutover-impl-20261007` using its own `target`:
- `cargo test --lib source_cutover_tests -- --nocapture`: 4 passed, 0 failed.
- `cargo test --test persisted_program_source_cutover -- --nocapture`: 6 passed, 0 failed.
- Tests cover explicit-source nested generated writes, duplicate binding contributions, controlled binding candidates, global binding withdrawal under a source filter, eager/exact reads and as-of capture, fresh-db raw lookup resolution, canonical program bytes/hashes and round trips, authenticated ABI 12 rejection, runtime literal/nested-argument equality, canonical inert callable output, raw retention admission, and complete dependency marking.
- Initial test failures were fixture errors (builtin entity ids used for schema-sensitive binding/retract tests, and expecting scalar rows without unpacking LoadMany); fixtures now use non-vocabulary entities and ordinary VM row emission. A compile-time field typo was corrected from named database `source` to datom coordinate `fact_source`. All recorded final focused checks pass.

Remaining validation: no PostgreSQL install/retry/restart/GC test was run by this worker. The root/durable workers own isolated release validation. Query evaluator and native query codec changes are owned by the query worker; their source-aware AST compiles with this dependency traversal. No code outside owned program files and the new focused test file was edited. No commits.
