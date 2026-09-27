# Index-Pull

`DatabaseValue::index_pull` walks an [index](#indexes),
[pulling](../05_query_and_pull/06_pull_reference.md) each entity using the
[pattern](../05_query_and_pull/06_pull_reference.md#patterns), returning a lazy
iterator of the results.

`index_pull` takes an `IndexPullOptions` with the following required fields:

- [`start`](#start) — An AVET or AEVT `IndexBoundary` indicating the initial
  position. At least the attribute must be specified. The boundary selects the
  index; entities are pulled from its third component.
- [`selector`](#selector) — A `PullPattern`, which can be compiled from an EDN
  Pull selector with `parse_pull_edn`.

Optionally:

- `offset` — Number of results to omit from the beginning of the returned data.
  Defaults to zero. Skipped datoms are not pulled.
- `limit` — Maximum total number of results to return. Specify `None` for no
  limit, which is the default. `Some(0)` validates the request but visits no
  datoms.
- [`reverse`](#reverse) — Walks the index in reverse. Defaults to `false`.

Construct options with `IndexPullOptions::new(start, selector)` and set optional
fields as needed. The return type is
`Result<IndexPullCursor<'_>, SemanticError>`; each iterator item is itself a
`Result<QueryValue, SemanticError>`. Consume the iterator to perform the reads
and handle traversal or Pull errors.

The native API has no timeout field. `index_pull_with_control` accepts a
`PullControl` for per-projection limits and cooperative cancellation; see
[controls and errors](#controls-and-errors).

## Indexes

The examples use the [music schema and entity identifiers from the Pull
reference](../05_query_and_pull/06_pull_reference.md#example-notes). The music
results are illustrative: they describe that example corpus, rather than a
dataset installed with EDB. Results are written in EDN notation; map key order
may differ. `db` is a captured `DatabaseValue`, and the Rust fragments run in a
function that returns `Result` so they can propagate errors with `?`.

All examples use these imports and attribute-name helper:

```rust
use edb_core::{
    AttributeName, EntityIdentifier, IndexComponents, IndexPullOptions,
    Keyword, QueryValue, Value,
};
use edb_core::edn_pull::parse_pull_edn;

fn attr(namespace: &str, name: &str) -> AttributeName {
    AttributeName::Ident(Keyword::new(namespace, name))
}
```

### AVET

An AVET boundary walks the specified attribute, starting at an optional value,
pulling against `e`. The attribute must have a ready AVET projection. Configure
`:db/index true` or uniqueness as appropriate and check `db.has_avet(&attribute)`
when readiness matters; see [ordered indexes](00_indexes_and_log.md).

If the attribute is `:db.cardinality/many`, a starting value must be specified.
This requirement does not deduplicate results: the walk continues beyond that
value, and an entity with several values can be pulled more than once.

First let's pull the names, starting years and ending years of artists, starting
at `:artist/startYear`:

```rust
let start = db.avet_boundary(IndexComponents::One(attr("artist", "startYear")))?;
let selector = parse_pull_edn("[:artist/name :artist/startYear :artist/endYear]")?;
let mut options = IndexPullOptions::new(start, selector);
options.limit = Some(5);
let artists = db.index_pull(options)?.collect::<Result<Vec<_>, _>>()?;
```

```edn
[{:artist/name "Choir of King's College, Cambridge", :artist/startYear 1441}
 {:artist/name "Heinrich Schütz", :artist/startYear 1585, :artist/endYear 1672}
 {:artist/name "Antonio Vivaldi", :artist/startYear 1678, :artist/endYear 1741}
 {:artist/name "Johann Sebastian Bach", :artist/startYear 1685, :artist/endYear 1750}
 {:artist/name "Georg Friedrich Händel", :artist/startYear 1685, :artist/endYear 1759}]
```

Interesting result for a database covering releases from 1968–1973.

Starting at the year 1800, the artists' releases and release years can be pulled
to investigate why these older artists are in the database:

```rust
let start = db.avet_boundary(IndexComponents::Two(
    attr("artist", "startYear"), Value::Long(1800).into(),
))?;
let selector = parse_pull_edn(
    "[:artist/name :artist/startYear {:release/_artists [:release/year]}]",
)?;
let mut options = IndexPullOptions::new(start, selector);
options.limit = Some(5);
let artists = db.index_pull(options)?.collect::<Result<Vec<_>, _>>()?;
```

```edn
[{:artist/name "Hector Berlioz", :artist/startYear 1803,
  :release/_artists [{:release/year 1970} {:release/year 1973} {:release/year 1969}]}
 {:artist/name "Felix Mendelssohn", :artist/startYear 1809,
  :release/_artists [{:release/year 1973}]}
 {:artist/name "Frédéric Chopin", :artist/startYear 1810,
  :release/_artists [{:release/year 1968} {:release/year 1970}]}
 {:artist/name "Franz Liszt", :artist/startYear 1811,
  :release/_artists [{:release/year 1968}]}
 {:artist/name "Richard Wagner", :artist/startYear 1813,
  :release/_artists [{:release/year 1968} {:release/year 1972}
                    {:release/year 1971} {:release/year 1970}]}]
```

Since the database contains artists with starting years prior to 1800, this
example starts the index pull near the middle of the attribute range by
specifying a value of 1800. Despite these artists being from 1800 onward, they
had posthumous releases in the 1968–1973 era.

This index pull does not pull only artists with a `:artist/startYear` of `1800`,
but returns an iterator starting at that point in the index. The example starts
at a `:artist/startYear` of 1800, but returns ascending results from 1803, 1809,
and so on. It stops when the attribute changes.

### AEVT

An AEVT boundary pulls the reference values of an entity for an attribute. The
attribute must be `:db.type/ref` and
[`:db.cardinality/many`](../03_schema/02_schema_reference.md#dbcardinality).

This example walks the index starting at `:release/artists`, which is a
cardinality-many reference attribute, and pulls the artists' names:

```rust
let start = db.aevt_boundary(IndexComponents::One(attr("release", "artists")))?;
let mut options = IndexPullOptions::new(start, parse_pull_edn("[:artist/name]")?);
options.limit = Some(5);
let artists = db.index_pull(options)?.collect::<Result<Vec<_>, _>>()?;
```

```edn
[{:artist/name "A1"}
 {:artist/name "Antonín Dvořák"}
 {:artist/name "Rob Lamothe"}
 {:artist/name "Craig Erickson"}
 {:artist/name "Frédéric Chopin"}]
```

The following example walks the AEVT index, starting at a release whose artists
include Bob Dylan and George Harrison. With an entity ID in hand for that
release, `dylan_harrison_sessions`, index pull starts at that release and pulls
the artist names (plural) and
[two tracks](../05_query_and_pull/06_pull_reference.md#limit-option) by each artist.
It then continues to subsequent releases in entity-ID order.

```rust
let start = db.aevt_boundary(IndexComponents::Two(
    attr("release", "artists"), EntityIdentifier::Id(dylan_harrison_sessions),
))?;
let selector = parse_pull_edn(
    "[:artist/name {[:track/_artists :limit 2] [:track/name]}]",
)?;
let mut dylan_harrison_index_pull = db.index_pull(IndexPullOptions::new(start, selector))?;
let first_five = dylan_harrison_index_pull.by_ref().take(5)
    .collect::<Result<Vec<_>, _>>()?;
```

```edn
[{:artist/name "Bob Dylan",
  :track/_artists [{:track/name "California"} {:track/name "Grasshoppers in My Pillow"}]}
 {:artist/name "George Harrison",
  :track/_artists [{:track/name "Give Me Love (Give Me Peace on Earth)"}
                   {:track/name "Miss O'Dell"}]}
 {:artist/name "Bonnie St. Claire",
  :track/_artists [{:track/name "Planet of Love"} {:track/name "Mañana mañana"}]}
 {:artist/name "The Byrds",
  :track/_artists [{:track/name "This Wheel's on Fire"} {:track/name "Old Blue"}]}
 {:artist/name "Gilberto Gil",
  :track/_artists [{:track/name "Cinema Olympia"} {:track/name "Frevo Rasgado"}]}]
```

However, care must be taken since Bob Dylan was an artist on many releases. The
artist entity for Bob Dylan can be pulled multiple times. The two-track limit
limits a nested attribute in each map; it does not limit the number of index
results or restrict those tracks to the release being walked.

Unlike a reusable sequence, consuming a Rust iterator advances it. To examine
the complete walk, including the first five results, combine those results with
the remainder before filtering:

```rust
let mut all_artists = first_five;
all_artists.extend(dylan_harrison_index_pull.collect::<Result<Vec<_>, _>>()?);
let name_key = QueryValue::Scalar(Value::Keyword(Keyword::new("artist", "name")));
let dylan_name = QueryValue::Scalar(Value::String("Bob Dylan".into()));
let dylan_results: Vec<_> = all_artists.iter().filter(|value| {
    matches!(value, QueryValue::Map(entries)
        if entries.iter().any(|(key, value)| key == &name_key && value == &dylan_name))
}).collect();
```

For a range containing six releases referencing Bob Dylan, this returns six
copies of the same projection:

```edn
[{:artist/name "Bob Dylan", :track/_artists [{:track/name "California"} {:track/name "Grasshoppers in My Pillow"}]}
 {:artist/name "Bob Dylan", :track/_artists [{:track/name "California"} {:track/name "Grasshoppers in My Pillow"}]}
 {:artist/name "Bob Dylan", :track/_artists [{:track/name "California"} {:track/name "Grasshoppers in My Pillow"}]}
 {:artist/name "Bob Dylan", :track/_artists [{:track/name "California"} {:track/name "Grasshoppers in My Pillow"}]}
 {:artist/name "Bob Dylan", :track/_artists [{:track/name "California"} {:track/name "Grasshoppers in My Pillow"}]}
 {:artist/name "Bob Dylan", :track/_artists [{:track/name "California"} {:track/name "Grasshoppers in My Pillow"}]}]
```

## Start

`start` provides an attribute and optionally a value (for AVET) or entity (for
AEVT) as an initial point for walking the index. Build it with `avet_boundary`
or `aevt_boundary` against the same immutable database value used for the pull.
These helpers resolve attribute names, idents and lookup references and
validate values against that database's schema.

Components are supplied in index order. `IndexComponents::One(attribute)` starts
at the beginning of that attribute in forward traversal, or its end in reverse
traversal. `Two` also specifies the value or entity; `Three` and `Four` can
include the remaining components for a more precise position. Boundaries are
inclusive: forward traversal starts at or above the supplied position, and
reverse traversal starts at or below it. The supplied position need not itself
be an existing datom.

For AVET on a cardinality-many attribute, EDB requires the second component.
AEVT permits an attribute-only start, but requires a cardinality-many reference
attribute. Many-to-many relationships can return duplicates even with a second
component: `start` is a seek position, not an equality constraint, and Index-Pull
does not deduplicate entities. To restrict traversal to an exact entity or value,
use a [prefix cursor](00_indexes_and_log.md#cursor-and-range-boundaries) and pull
the appropriate entity component, or use a query.

## Selector

The selector is a [Pull pattern](../05_query_and_pull/06_pull_reference.md#patterns).
The selector is pulled against each `e` for AVET or `v` for AEVT. It can select
attributes, navigate reverse references, nest other patterns and set attribute
limits just as ordinary Pull can. The selected datom's entity or reference
value determines the root; the pattern can navigate other attributes and
entities in the same captured database value.

The cursor validates the pattern and boundary at construction, including for an
empty range or `limit = Some(0)`. Each `next()` projects at most one result.
Collecting the cursor into a `Vec` materializes all consumed results; simply
constructing it does not scan index datoms.

## Reverse

Setting `reverse = true` causes the index to be walked in reverse.

When `reverse` is `true`, any `offset` is applied in the direction of iteration.
The following examples show how `reverse` and `offset` affect iteration. Assume
an indexed, cardinality-one long attribute `:example/number` with one entity for
each positive integer from 1 to `n`, where `n` is at least 10.

```rust
let start = db.avet_boundary(IndexComponents::One(attr("example", "number")))?;
let selector = parse_pull_edn("[:example/number]")?;
let mut options = IndexPullOptions::new(start, selector);
options.limit = Some(10);
let numbers = db.index_pull(options)?.collect::<Result<Vec<_>, _>>()?;
```

```edn
[{:example/number 1} {:example/number 2} {:example/number 3}
 {:example/number 4} {:example/number 5} {:example/number 6}
 {:example/number 7} {:example/number 8} {:example/number 9}
 {:example/number 10}]
```

No limit is required in the next example: the iterator terminates at the
beginning of the attribute range.

```rust
let start = db.avet_boundary(IndexComponents::Two(
    attr("example", "number"), Value::Long(5).into(),
))?;
let mut options = IndexPullOptions::new(start, parse_pull_edn("[:example/number]")?);
options.reverse = true;
let numbers = db.index_pull(options)?.collect::<Result<Vec<_>, _>>()?;
```

```edn
[{:example/number 5} {:example/number 4} {:example/number 3}
 {:example/number 2} {:example/number 1}]
```

The offset is applied after selecting the reverse direction. Skip three entries
starting at 10, then return five results:

```rust
let start = db.avet_boundary(IndexComponents::Two(
    attr("example", "number"), Value::Long(10).into(),
))?;
let mut options = IndexPullOptions::new(start, parse_pull_edn("[:example/number]")?);
options.offset = 3;
options.reverse = true;
options.limit = Some(5);
let numbers = db.index_pull(options)?.collect::<Result<Vec<_>, _>>()?;
```

```edn
[{:example/number 7} {:example/number 6} {:example/number 5}
 {:example/number 4} {:example/number 3}]
```

## Controls and Errors

Index-Pull requires a point-in-time database value. Current, as-of and filtered
point-in-time values can be used; raw history cannot. Both the index walk and
every Pull projection use the same captured value. Storage-backed reads remain
subject to the [captured-value retention
contract](../02_core_concepts/00_database_values.md#captured-value-retention).

Use `db.index_pull_with_control(options, &control)` to supply `PullControl`.
`max_depth` and `max_entities` apply separately to each projection, not as a
shared allowance across the cursor. Root depth is zero; entity limits count
expanded occurrences, including repeated branches. Their defaults impose no
practical cap. An exceeded limit returns an error, not a truncated success.
These controls do not impose a total index-scan work, byte or timeout budget.

The shared `cancel` flag covers index traversal, offsets, rejected filter
candidates and nested Pull work. Set it to `true` to request cooperative
cancellation. Arbitrary Rust callbacks must return before a cancellation check
can continue; this is not preemption. See [Pull controls and
errors](../05_query_and_pull/06_pull_reference.md#controls-and-errors).

Construction errors include a missing attribute, an index other than AVET or
AEVT, an AVET cardinality-many start without a value, an AEVT attribute that is
not a cardinality-many reference, invalid selectors and unavailable AVET
projections. Traversal or projection errors are returned as iterator items;
after an error, the cursor is finished. Previously yielded results remain
results of the same immutable database value.
