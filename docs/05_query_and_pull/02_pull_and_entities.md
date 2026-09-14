# Pull and entity navigation

Pull returns a selected data shape from one immutable point-in-time value.
An EDN pattern such as `[:db/id :person/name {:person/friends [:person/name]}]`
selects attributes and explicitly nests references. `[*]` selects installed
attributes; explicit selectors can override wildcard selection. Typed callers
build `PullPattern` and `PullAttribute`; text callers use
`atomic_core::edn_pull::parse_pull_edn`.

Use `database.pull(&pattern, identifier)` for a plain read or `pull_with_control`
for explicit controls. `PullAttribute` supports output aliases, defaults, limits,
nested patterns/recursion and registered transforms. Recursion and component
expansion observe depth/entity controls and stop cycles with identity maps.
Results use native `QueryValue` maps and collections; readable EDN keeps those
shapes rather than printing Rust debug text.

## Bounded Pull navigation

`PullLimit::Limit(n)` stops after the requested visible values;
`PullLimit::Default` returns at most 1,000 values for a many-valued attribute and
`PullLimit::Unlimited` requests them all. Limits must be positive. Forward,
reverse and nested navigation consume lazy index cursors. Wildcard discovery
seeks between attributes instead of walking an unused many-valued tail. Transforms
receive the selected value after limits/nesting, with defaults applied afterward.

Reverse references produce `:db/id` maps unless explicitly nested. A reverse
component reference produces one map, not an implicitly expanded parent. EDN
underscore-leading names resolve against the captured schema: an exact installed
`:item/_name` wins over reverse navigation through `:item/name`. Typed callers can
request this behavior with `PullDirection::SchemaResolved`; explicit `Forward`
and `Reverse` remain unambiguous. Unresolved entity idents and lookup references
still apply each explicit attribute's transform to nil, then use its default only
if the result is nil. Defaults themselves are never transformed.

Filters may require examining additional candidates to find enough visible values;
proving absence through an opaque filter can require exhausting the range. Pull
cancellation is checked during consumption. Query-projected Pull also shares the
query's work, deadline and original `max_value_bytes` admission, including values
materialized after the relational basis has been prepared. These are cooperative controls,
not preemption of arbitrary Rust callbacks or an interrupt inside one node decode.
Pull requires a point-in-time database value; raw history remains available to
queries/index access, not entity navigation.

Small logical reads may still fetch an entire immutable node on a cache miss.
`Connection::load_stats()` reports cursor node/SQL/byte activity after cursors are
completed or dropped. Cache hits avoid those SQL reads but still incur cache
bookkeeping; keyed hash lookup plus linked-slot recency has expected constant work
in the configured resident entries. Accounted bytes are not RSS, and retained slot
capacity follows peak occupancy until the cache itself is dropped.


## Lazy entities

`database.entity(identifier)` returns an optional lazy `Entity`. `get` loads a
selected attribute into that entity's local cache; `touch` loads its ordinary
attributes and component descendants. Neither operation refreshes its captured
database. Navigation of references can return entities, while forward references
to identified entities use keyword values; reverse navigation retains entities.
Missing entities and unresolved lookup/ident selectors are not fabricated facts.

Entity equality means origin and ID, not equal attribute state. See
[identity and immutable values](../02_core_concepts/00_database_values.md#entity-identity-is-not-state-equality).
For ordered projected traversal use [index-Pull](../06_indexes/00_indexes_and_log.md).
