# Entity type review

Reviewed `docs-2/07_peer_api/types/06_entity.md` in full with NORTHSTAR,
TIMESTAMP_SOURCE_DESIGN, CUTOVER, WORKER_RULES and the three supplied talks.
The page is aligned: Entity navigates the shared current record through a
captured point-in-time value; contribution count does not multiply attribute
values or incoming holders. Identity excludes time and realized state. No
remaining page-specific documentation or implementation gap was found.

Decision: retain the page and implementation. The earlier Entity decimal gap
was a second logical-equality deduplication after `DatabaseValue::values` had
already projected distinct stored values. Its existing fix now deduplicates
only reverse reference IDs. Repeating projection policy here, adding source
arguments to Entity, or writing another facade test would entangle independent
visibility, value identity and navigation concerns without covering a new
failure. Reuse the concrete regressions and their recorded results.

Code parity inspected:

- Entire `src/pull/entity.rs`: documented constructors/signatures/shapes;
  shared direction cache including absence; independent numeric/keyword keys;
  keyword forward refs and entity reverse refs; forward component-only touch,
  cycle-path protection and iterative cache destruction; uncached key
  enumeration; captured database/pull and origin/ID equality/hash.
- `src/pull/model.rs`, `execute.rs` and `attributes.rs`: numeric/ident/lookup
  construction, history rejection, direct-ID validity versus lower-level
  wrapping, underscore exact-schema precedence, strict numeric attributes,
  missing keyword attributes, and delegated ordinary Pull behavior.
- `src/database_value/resolve.rs::values` and lookup/identifier paths,
  `value.rs` captured origins/schema/idents, `views.rs` current visibility and
  point-in-time guard, cursor entry/collection paths, and entire
  `src/index/visibility.rs`: stored decimal scale survives value projection;
  global withdrawal precedes since/custom predicates; eager/block/overlay use
  the same semantic boundary. `src/entity_identity.rs` preserves lineage or
  local-origin identity independently of state and cache.

Acceptance evidence inspected, not newly executed:

- `doc-entities-before.log` records the original decimal regression failure;
  `doc-entities-after.log` records both direct Entity tests, seven Pull tests
  and the stored-value test passing (10 total).
- Read `tests/entities_source_cutover.rs` and
  `tests/stored_value_source_cutover.rs` in full, plus the Entity-focused Pull
  and database-value tests. They cover stored scales 1/2 under interleaved
  contributions, touch, eager/speculative bases, filtered/as-of/since views,
  history rejection, forward refs/components, reverse holders and global
  withdrawals. Identity tests distinguish captured attribute state and
  independent origins while preserving handle/token equality across changes.
- `tests-integrated-sixth.log` confirms those Entity2, Pull7, stored-value1
  and database-value2 tests passed in the latest combined check.
  `doc-entities-clippy.log` and `clippy-entity-recovery.log` record strict
  focused/all-target Clippy success. `doc-link-check.json` records 1563 checked
  links/fragments and no issues; assigned-page links are unchanged.

Actual outcome: no doc/code/test edits, no new test execution, no production
access and no commits. Assigned-page diff is empty. No outstanding dependency
or page-specific parity gap; no new durable Entity run was performed. Root
owns final integrated release checks, including live evidence affected by the
recent authenticated-log format change.
