# Attribute review

Read the full assigned page, worker rules, NORTHSTAR, TIMESTAMP_SOURCE_DESIGN,
CUTOVER and three supplied talks. The page already uses the mandatory ordinary
request source and describes AVET/SAVET shared readiness. No source-specific
attribute property or source-specific constraint is introduced. The bounded
question is whether the descriptor, captured-schema inspection and readiness
claims match implementation after the integrated model/schema fixes.

Decision: preserve the aligned page and implementation. Adding a source field
or a second readiness mechanism to Attribute would conflate fact attribution
with schema information. Reusing the existing descriptor projection, captured
basis and shared physical readiness allows ordinary attribute inspection to
compose with source-aware transaction/read behavior. An extra API wrapper or
new glossary text would add no demonstrated missing behavior. Acceptance is
all public field/builder shapes, ordinary derived schema/aliases, type and
constraint validation, immutable captured descriptors and paired readiness.

Inspected implementation: src/lib.rs exports all documented types;
src/model/schema.rs:61-133 defines all twelve public fields, exact constructor
and builders (including sorted/deduplicated predicate names); :254-432 validates
installation, retains renamed aliases and iterates its BTreeMap in numeric order;
:573-817 derives the descriptor from ordinary schema datoms and historical ident
information; :959-1072 validates fully qualified predicates, component/ref,
unique cardinality/bytes, fulltext/string and tuple properties. Alteration rejects
changed type/fulltext/tuple definition and resumed discontinued composites.
src/model/vocabulary.rs:84 validates compact schema IDs;
src/database_value/value.rs:293 borrows the captured in-memory schema for every
basis; src/database_value/resolve.rs:46 distinguishes schema eligibility from
captured physical readiness and resolves unknown names to the documented error.
Its SAVET boundary normalization uses the same ready-AVET attribute resolver.
src/storage/snapshot.rs shared readiness checks and src/transaction/native.rs
predicate registration preserve those responsibilities independently.

Reused actual evidence (read stdout and relevant test code):
tests-integrated-fifth.log records 14/14 schema_source_cutover checks passing,
including immutable schema restrictions, composite discontinuation, native
predicates, cardinality/uniqueness over distinct values/holders and attributed
ordinary schema metadata. It records 4/4 changing_schema_source_cutover tests
passing, including retained aliases/alias repurpose and immutable captured
schema across as-of/since/history. Its library tests pass eager AVET/SAVET
eligibility and bounded paired backfill/clear. peer-snapshot-readiness-after.log
records the actual isolated live readiness test passing 1/1; inspected test code
shows both current/history AVET/SAVET remain unavailable while pending, become
ready at the same logical basis and retain both sources, while the old captured
snapshot remains pending. The ignored live test in the integrated suite is not
counted as execution. No unchanged suite was rerun by this worker.

Result: no documentation, production code or committed test changes necessary;
no page-specific gap or shared ownership dependency found. The existing exact
schema tutorial proof also exercises the same ordinary identity-attribute
installation mechanism, but this worker did not execute the Attribute page's
Rust example verbatim. Page-scoped git diff --check passed. No commits or old
EDB/Course_Academy resources touched.
