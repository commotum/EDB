# Changing schema review

Read the entire assigned page, NORTHSTAR, TIMESTAMP_SOURCE_DESIGN, CUTOVER,
worker rules, and the three supplied talks. The page already supplies one
request source and describes shared current constraints correctly; preserve it.
The common assessor binds transaction behavior to db-before, derives a separate
successor schema, validates the complete successor, and applies source attribution
at material_changes. IdentIndex retains historical aliases independently of the
current ident fact. Logical schema/read shape and physical index readiness remain
separate concerns, making captured-value behavior locally understandable.

Bounded question: existing 14 schema regressions and paired AVET/SAVET live
backfill/backup proofs cover most of the page, but its rename/alias repurpose,
captured temporal entity shape, and same-transaction cardinality/component/unique
changes lack a direct page-scoped source-bearing proof. No failure is established.
Reuse the common EDN assessor in a unique focused test file; extending production
schema machinery without a failing case adds unnecessary coupling, while merely
repeating broad suites does not answer these uncovered claims.

Acceptance: ordinary EDN examples preserve renamed aliases and later repurpose;
application datoms retain attribute IDs/source; cleanup changes are atomic;
new time-filtered values retain captured schema and projection shape; old schema
controls cardinality replacement, tempid upsert, and component expansion in the
change transaction. All generated datoms share the single request source.
Inspect existing noHistory and durable readiness evidence instead of rerunning
those unchanged suites. Coordinate with root before any shared-code edit.

Implemented only `tests/changing_schema_source_cutover.rs` (four tests). Ordinary
EDN proves rename/alias repurpose without rewriting application datoms; immutable
captured names; failed many-to-one cleanup without partial changes; successful
cross-source cleanup; scalar navigation under the new captured schema despite
two historical values; original collection shape on the older captured value;
old one-value replacement and identity upsert in a transaction removing those
constraints; subsequent many-valued/separate-entity behavior; enabling/disabling
components with both retractEntity spellings using the old flag. Generated
schema hooks, replacements, cascades and timestamps share the request source.
No production-code change was justified; the aligned page remains byte-for-byte
unchanged. No shared files were edited.

Actual focused command: `CARGO_TARGET_DIR=/tmp/edb-cutover-impl-20261007-target
cargo test --offline --test changing_schema_source_cutover` passed 4/4, no ignored
checks (0.16s runtime); output retained in `changing-schema-focused.log`.
Page/test-scoped `git diff --check` passed; rustfmt ran only on the new test file.

Inspected code: assessor preparation/hooks, db-before expansion/retract traversal,
successor derivation and streaming cardinality/unique validators, material_changes;
model Schema tuple/attribute validators and historical IdentIndex; eager schema
transition validation; DatabaseValue captured schema/views/readiness; Entity
scalar/collection projection; Peer sync_schema/sync_index with target-bound paired
projection work; storage advance_projection shared AVET/SAVET chunks; metadata
eligibility; history_edits and complete-group noHistory consolidation witnesses.
The common mechanisms preserve independent value/holder constraints, attribution,
schema interpretation and index publication. They compose without a second schema
transaction path or source-specific index/readiness state.

Reused evidence read directly: `tests-integrated-second.log` records all 14 schema
regressions and shared noHistory/paired backfill storage checks passing (43 library,
55 integration checks). Read the paired enabling/disabling storage test and the
portable ignored backup live test, including assert_pending and its immutable
partial publications. `operations-workflow-fixed.log` independently records ready
backup/deep verification/restore and healthy inspection passing. The pending and
partial live backup proof's 79.68s pass is recorded by its original worker in
`doc_backup_restore.md` and parent tool transcript; its raw stdout was not retained
as a review logfile, so this worker does not claim to have independently read one.
Root will include that live proof in final isolated serial checks with a retained
logfile. No broad suites or live PostgreSQL tests were rerun by this worker.

No known page-specific behavior gap remains. No compatibility path, commits,
original EDB edits, Course_Academy edits, or live application resources were used.
