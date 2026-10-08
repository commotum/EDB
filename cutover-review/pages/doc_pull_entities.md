# Pull and entities overview

Reviewed `docs-2/05_query_and_pull/02_pull_and_entities.md` in full against
NORTHSTAR, TIMESTAMP_SOURCE_DESIGN and CUTOVER. The overview is aligned; leave
it unchanged. The cutover concern is projecting logical values and incoming
holders from attributed datoms without multiplying limits, navigation or
component expansion. No remaining implementation gap was found.

Keep attribution in raw cursors and project at the read boundary. Dropping
source upstream would lose index/history information; deduplicating final maps
would count limits and expand references incorrectly. Existing implementation
selects distinct stored values before limits/nesting (`src/pull/attributes.rs`),
and `DatabaseValue::values` (`src/database_value/resolve.rs`) serves forward
Entity navigation. Both use `stored_eq` inside an index-equal group, preserving
top-level decimal scales even when transaction order interleaves them. Entity
reverse navigation deduplicates holder IDs (`src/pull/entity.rs`). Raw
index-Pull intentionally keeps one result per selected datom and datom offsets
(`src/pull/index.rs`); each projected map uses ordinary Pull.

Inspected `execute.rs`, `control.rs`, `transform.rs` and `attributes.rs` under
`src/pull/`: captured-value entry points, iterative component/recursion tasks,
depth/entity controls, nil-transform-default ordering, wildcard attribute seeks,
schema-resolved underscore names and candidate cancellation remain intact.
Query-projected Pull receives the original shared query budget in
`src/query/execute.rs`; its candidate/value admission is in `PullState`.
`src/database_value/views.rs::visible_current/next_controlled` resolves global
EAV visibility before since/custom predicates, so a filtered source cannot
resurrect another source's withdrawn contribution. `Entity` caches retain the
captured database; equality/hash use origin and ID. Cache statement matches
`src/collections/lru.rs` (keyed hash lookup, linked reusable slots).

Acceptance evidence inspected, not newly executed:

- `tests-integrated-fourth.log`: all 7 `pull_source_cutover` tests passed,
  covering forward/reverse/nested/wildcard/component/recursive shapes,
  default/explicit limits and transform ordering, eager/overlay contributions,
  filtered current/as-of/since reads, cross-source withdrawal and subsequent
  assertion, history rejection, query Pull, datom index-Pull multiplicity and
  offsets, and cancellation on duplicate source candidates.
- Both `entities_source_cutover` tests passed: stored decimal-scale projection
  across source/views and global withdrawal for forward refs/components and
  reverse holders, including eager and overlay bases.
- `stored_value_source_cutover` passed: interleaved decimal representations
  retain scales and correct Pull prefixes under limits 1/2. Both
  `database_values_source_cutover` tests passed, including source changes with
  entity identity distinct from attribute state or database origin.

The supplied talks support the bounded choice: keep source attribution,
visibility, projection and traversal controls independently reasoned; reuse the
existing read mechanisms rather than add adapters or duplicate projection
policy. Completion audit found no page-specific unmet criterion. No doc/code
changes, new tests, production access or commits. No outstanding dependency;
root owns final integrated release checks and any broader durable acceptance.
