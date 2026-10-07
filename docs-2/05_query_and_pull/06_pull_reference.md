# Pull reference

Pull is a declarative way to make hierarchical (and possibly nested) selections of information about entities. Pull applies a *pattern* to a collection of entities, building a map for each entity. Pull is available through `Database::pull`, `DatabaseValue::pull`, `pull_many`, and [query Pull expressions](05_query_reference.md#pull-expressions).

Patterns support [forward](#attribute-names) and [reverse](#reverse-lookup) attribute navigation, [wildcarding](#wildcards), [nesting](#map-specification-nesting-example), [recursion](#recursion-limits), [naming control](#as-option), [transformation](#xform-option), [defaults](#default-option), and [limits](#limit-option) on the results returned. Entities can be passed to pull as any kind of [entity identifier](../04_transactions/04_transaction_data.md#entity-identifiers): [entity ids](../04_transactions/04_transaction_data.md#entity-ids), [idents](../04_transactions/04_transaction_data.md#idents), or [lookup refs](../04_transactions/04_transaction_data.md#lookup-refs).

Pull patterns are written in the [Extensible Data Notation](https://github.com/edn-format/edn) (edn), which is programming language neutral. In programs, you can create patterns programmatically out of your basic language data types, e.g. Rust `PullPattern` and `PullAttribute` values. Alternatively, compile a serialized EDN string with `parse_pull_edn` and pass the resulting pattern to `pull`.

The results below are written in EDN notation, and they use an ellipsis `...` where large results have been elided for brevity. Numeric entity IDs are illustrative; EDB’s lossless EDN writer prints reference values with the `#edb/ref` tag. Maps may print in a different key order.

## Example Notes

The examples use a music database with artists, releases, media, and tracks.
`:release/artists`, `:track/artists`, and `:person/friends` are cardinality-many
references. `:release/media` and `:medium/tracks` are cardinality-many component
references. `:artist/country` and `:release/country` are cardinality-one references.
The artist and track names are strings, and year and position attributes are longs.

The following helper accepts a serialized pattern and an entity identifier:

```rust
use edb_core::{DatabaseValue, EntityIdentifier, QueryValue, SemanticError};
use edb_core::edn_pull::parse_pull_edn;

fn pull(
    db: &DatabaseValue,
    pattern: &str,
    entity: impl Into<EntityIdentifier>,
) -> Result<QueryValue, SemanticError> {
    db.pull(&parse_pull_edn(pattern)?, entity)
}
```

`db` is an immutable point-in-time database value. The example variables below
are `u64` entity IDs selected by queries against that database:

| Variable | Entity |
| --- | --- |
| `dylan_harrison_sessions` | A release whose artists include Bob Dylan and George Harrison |
| `ghost_riders` | Track 11 on a medium of that release |
| `led_zeppelin` | The artist named “Led Zeppelin” |
| `mccartney` | The artist named “Paul McCartney” |
| `concert_for_bangla_desh` | The release named “The Concert for Bangla Desh” |
| `dark_side_of_the_moon` | The release named “The Dark Side of the Moon” |

For example, these queries select the sessions and track respectively:

```edn
[:find ?release
 :where [?zimbo :artist/name "Bob Dylan"]
        [?magpie :artist/name "George Harrison"]
        [?release :release/artists ?zimbo]
        [?release :release/artists ?magpie]]

[:find ?track
 :in $ ?release ?trackno
 :where [?release :release/media ?medium]
        [?medium :medium/tracks ?track]
        [?track :track/position ?trackno]]
```

Bind the second query’s `?release` to `dylan_harrison_sessions` and `?trackno` to
11. For each of the remaining variables, select the entity with the corresponding
`:artist/name` or `:release/name`. See [query execution](05_query_reference.md).

## Pull Grammar

### Grammar Syntax

```
'' literal
"" string
[] = list or vector
{} = map {k1 v1 ...}
() grouping
| choice
+ one or more
```

### Pull Pattern Grammar

```
pattern             = [attr-spec+]
attr-spec           = attr-name | wildcard | map-spec | attr-expr
attr-name           = an edn keyword that names an attr | native-attribute-id
map-spec            = { ((attr-name | attr-expr) (pattern | recursion-limit))+ }
attr-expr           = [attr-name attr-option+] | legacy-attr-expr
as-expr             = [attr-name ":as" any-value]
limit-expr          = [attr-name ":limit" (positive-integer | nil)]
default-expr        = [attr-name ":default" any-value]
xform-expr          = [attr-name ":xform" symbol]
attr-option         = ":as" any-value | ":limit" (positive-integer | nil)
                    | ":default" any-value | ":xform" symbol
wildcard            = "*" or '*'
recursion-limit     = positive-integer | '...'
legacy-attr-expr    = legacy-limit-expr | legacy-default-expr
legacy-limit-expr   = [("limit" | 'limit') attr-name (positive-integer | nil)]
legacy-default-expr = [("default" | 'default') attr-name any-value]
```

A `native-attribute-id` is a nonnegative integer within the `u32` attribute-ID range. `any-value` must be representable as an EDB `QueryValue`.

Terminals such as `limit` can be strings or symbols, e.g. `(limit :friends 100)` or `("limit" :friends 100)`. Attribute names remain keywords or native numeric attribute IDs. Options are keyword/value pairs, not nested expression vectors. An empty pattern `[]` is also accepted and selects no attributes. Recursion depths and limits must be positive integers; although parsing accepts zero, execution rejects it.

## Patterns

A pattern is a list of Attribute Specifications.

```
pattern            = [attr-spec+]
```

## Attribute Specifications

```
attr-spec           = attr-name | wildcard | map-spec | attr-expr
attr-expr           = [attr-name attr-option+] | legacy-attr-expr
attr-option         = ":as" any-value | ":limit" (positive-integer | nil)
                    | ":default" any-value | ":xform" symbol
wildcard            = "*" or '*'
recursion-limit     = positive-integer | '...'
legacy-attr-expr    = legacy-limit-expr | legacy-default-expr
legacy-limit-expr   = [("limit" | 'limit') attr-name (positive-integer | nil)]
legacy-default-expr = [("default" | 'default') attr-name any-value]
```

An attribute spec specifies an attribute to be returned, and (optionally) what additional transformations to perform on the value before it is returned. Attribute specs can be attribute names, wildcards, map specs, or attribute expressions.

## Attribute Names

```
attr-name          = an edn keyword that names an attr | native-attribute-id
```

An attribute spec names an attribute, with an optional leading underscore on the name part of the keyword to reverse the direction of navigation.

### Attribute Name Example

The following pattern uses two attribute names to return an `:artist/name` and `:artist/startYear`, pulling on `led-zeppelin`:

```
// Pattern: [:artist/name :artist/startYear]

// Example
pull(&db, r#"[:artist/name :artist/startYear]"#, led_zeppelin)?
```

```
=> {:artist/name "Led Zeppelin", :artist/startYear 1968}
```

### Reverse Lookup

An underscore prefix (`_`) on the name part of an attribute ident causes the attribute to be navigated in reverse when that exact underscore-leading attribute is not installed. An exact installed `:item/_name` wins over reverse navigation through `:item/name`. Avoid underscore-leading names when you intend the spelling to express reverse lookup. Typed callers can use explicit `PullDirection::Forward` or `PullDirection::Reverse` to select the direction unambiguously.

Normally, `pull` returns a map of attributes and values (which may be nested entities) selected from the entity supplied as the last argument to the `pull` call. For example, `pull(&db, r#"[:release/artists]"#, led_zeppelin)?` would attempt to pull the `:release/artists` attribute from the `led-zeppelin` entity.

The underscore prefix reverses the direction of a `pull`, so `pull(&db, r#"[:release/_artists]"#, led_zeppelin)?` will pull all of the entities that have a `:release/artists` attribute with the value of `led-zeppelin`.

### Attribute Name Reverse Lookup Example

As an exploratory measure `led-zeppelin` is pulled with a [wildcard](#wildcards). `:release/artists` is not part of the result.

You can navigate 'backwards' from `:release/artists` to find the releases with a reference to `led-zeppelin` by pulling `:release/_artists`

Attributes like `:artist/startYear` or `:artist/name` would not work with reverse lookup as there is no reference value.

```
pull(&db, r#"[:release/artists]"#, led_zeppelin)?
```

```
=> {}
```

```
// What does led-zeppelin have?

pull(&db, r#"[*]"#, led_zeppelin)?
```

```
=>
{:artist/sortName "Led Zeppelin",
 :artist/name "Led Zeppelin",
 :artist/type {:db/id 70746976177619070},
 :artist/country {:db/id 47850746040811801},
 :artist/gid #uuid "678d88b2-87b0-403b-b63d-5da7465aecc3",
 :artist/endDay 25,
 :artist/startYear 1968,
 :artist/endMonth 9,
 :artist/endYear 1980,
 :db/id 2458507999719892}
;; :release/artists is not there...
```

```
pull(&db, r#"[* :release/_artists]"#, led_zeppelin)?
```

```
=>
{:artist/sortName "Led Zeppelin",
 :artist/name "Led Zeppelin",
 :artist/type {:db/id 70746976177619070},
 :artist/country {:db/id 47850746040811801},
 :artist/gid #uuid "678d88b2-87b0-403b-b63d-5da7465aecc3",
 :artist/endDay 25,
 :artist/startYear 1968,
 :artist/endMonth 9,
 :release/_artists
 [{:db/id 12591607161327185}   ;; ----.
  {:db/id 13611953951903311}   ;;     |
  {:db/id 14614708556444205}   ;;     |
  {:db/id 20349761206917151}   ;;     |
  {:db/id 27505382880490028}   ;;     |
  {:db/id 30606005670815267}   ;;     |
  {:db/id 36437815344539172}   ;;     |
  {:db/id 38834750693087262}   ;;     |
  {:db/id 43703388180886059}   ;;     |-- Releases
  {:db/id 43910096366902013}   ;;     |
  {:db/id 45994770413170389}   ;;     |
  {:db/id 49236130691853680}   ;;     |
  {:db/id 51514318784597586}   ;;     |
  {:db/id 54157544737773785}   ;;     |
  {:db/id 58683134597703205}   ;;     |
  {:db/id 66718365573484112}   ;;     |
  {:db/id 71402285107818196}], ;; ----'
 :artist/endYear 1980,
 :db/id 2458507999719892}
```

## Map Specification

```
map-spec           = { ((attr-name | attr-expr) (pattern | recursion-limit))+ }
limit-expr         = [("limit" | 'limit') attr-name (positive-integer | nil)]
recursion-limit    = positive-integer | '...'
```

You can explicitly specify the handling of referenced entities by using a map instead of just an attribute name. The simplest map specification is a map specifying a specific `pattern` for a particular `attr-name`.

### Map Specification Example

The `:track/artists` attribute appears in a map spec, causing the `:db/id` and `:artist/name` to be sub-pulled for each artist on the track `ghost-riders`.

```
pull(&db, r#"[:track/name {:track/artists [:db/id :artist/name]}]"#, ghost_riders)?
```

```
=>
{:track/artists [{:db/id 17592186048186, :artist/name "Bob Dylan"}
                 {:db/id 17592186049854, :artist/name "George Harrison"}],
 :track/name "Ghost Riders in the Sky"}
```

### Map Specification Nesting Example

Map specs can nest to select deeper relationships, subject to EDN admission and explicit traversal controls. The pattern below pulls `concert-for-bangla-desh` 's media's tracks' titles and artists' names:

```
pull(&db, r#"[{:release/media
              [{:medium/tracks
                [:track/name {:track/artists [:artist/name]}]}]}]"#, concert_for_bangla_desh)?
```

```
=>
{:release/media
 [{:medium/tracks
    [{:track/artists
        [{:artist/name "Ravi Shankar"} {:artist/name "George Harrison"}],
        :track/name "George Harrison / Ravi Shankar Introduction"}
    {:track/artists [{:artist/name "Ravi Shankar"}],
        :track/name "Bangla Dhun"}]}
 {:medium/tracks
     [{:track/artists [{:artist/name "George Harrison"}],
         :track/name "Wah-Wah"}
     {:track/artists [{:artist/name "George Harrison"}],
         :track/name "My Sweet Lord"}
     {:track/artists [{:artist/name "George Harrison"}],
         :track/name "Awaiting on You All"}
     {:track/artists [{:artist/name "Billy Preston"}],
         :track/name "That's the Way God Planned It"}
             ...]}]}
```

## Attribute Spec

```
attr-spec           = attr-name | wildcard | map-spec | attr-expr
attr-expr           = [attr-name attr-option+] | legacy-attr-expr
attr-option         = ":as" any-value | ":limit" (positive-integer | nil)
                    | ":default" any-value | ":xform" symbol
```

You can use an attribute spec to declare various aspects of the corresponding values returned by Pull.

EDN lists and vectors are both accepted for patterns and attribute expressions. Rust examples use raw string literals so the EDN reader receives the pattern as data; no host-language evaluation or quoting is involved.

## :as Option

```
[attr-name ":as" any-value]
```

The `:as` option can be used to declare what an attribute should be renamed to in the result map.

### :as Option Example

The following pattern uses an `:as` option to pull an `:artist/name`, replacing the key in the result map with the string "Band Name", pulling on `led-zeppelin`.

```
pull(&db, r#"[[:artist/name :as "Band Name"]]"#, led_zeppelin)?
```

```
=> {"Band Name" "Led Zeppelin"}
```

## :limit Option

```
[attr-name ":limit" (positive-integer | nil)]
```

By default, Pull will return the first 1000 values for a cardinality-many attribute, but you can control that by providing either a positive number or `nil` for the `:limit` option. All values for a cardinality-many attribute will be returned if you explicitly provide a `nil` limit.

Limits stop attribute cursor consumption before nested projection. A limit of
10 therefore selects at most 10 visible references to expand, even if some
nested results are later omitted because they are empty. Wildcard discovery
seeks to the next attribute without exhausting the unselected many-valued tail.
Filters can require examining additional candidates to obtain the selected
visible prefix; a limit does not promise a fixed total number of reads.

### :limit Option Example

To return only 10 of `led-zeppelin` 's tracks:

```
pull(&db, r#"[:artist/name [:track/_artists :limit 10]]"#, led_zeppelin)?
```

```
=>
{:artist/name "Led Zeppelin",
 :track/_artists
 [{:db/id 17592186057344}
  {:db/id 17592186057345}
  {:db/id 17592186057346}
  {:db/id 17592186057347}
  {:db/id 17592186057348}
  {:db/id 17592186057349}
  {:db/id 17592186057350}
  {:db/id 17592186057351}
  {:db/id 17592186057352}
  {:db/id 17592186057355}]}
```

### :limit Inside a Map Specification Example

Pulling from `led-zeppelin`, you can get a limited set of nested track names with:

```
pull(&db, r#"[{[:track/_artists :limit 10] [:track/name]}]"#, led_zeppelin)?
```

```
=>
{:track/_artists
 [{:track/name "Whole Lotta Love"}
  {:track/name "What Is and What Should Never Be"}
  {:track/name "The Lemon Song"}
  {:track/name "Thank You"}
  {:track/name "Heartbreaker"}
  {:track/name "Living Loving Maid (She's Just a Woman)"}
  {:track/name "Ramble On"}
  {:track/name "Moby Dick"}
  {:track/name "Bring It on Home"}
  {:track/name "Whole Lotta Love"}]}
```

### Nil :limit Example

The pattern below returns all of Led Zeppelin's tracks, without limit:

```
pull(&db, r#"[:artist/name [:track/_artists :limit nil]]"#, led_zeppelin)?
```

```
=>
{:artist/name "Led Zeppelin",
 :track/_artists
 [{:db/id 17592186057344}
  {:db/id 17592186057345}
  {:db/id 17592186057346}
  {:db/id 17592186057347}
  {:db/id 17592186057348}
  {:db/id 17592186057349}
  {:db/id 17592186057350}
  {:db/id 17592186057351}
  {:db/id 17592186057352}
  {:db/id 17592186057355}
  {:db/id 17592186057356}
  {:db/id 17592186057357}
  {:db/id 17592186057358}
  {:db/id 17592186057359}
  {:db/id 17592186057360}
  {:db/id 17592186057361}
  {:db/id 17592186057362}
  {:db/id 17592186057363}
  {:db/id 17592186057366}
  {:db/id 17592186057367}
  ...]} ;; lots more
```

## :default Option

```
[attr-name ":default" any-value]
```

The `:default` option specifies a value to return if an entity has no value for that attribute.

### :default Option Example

The following select reports a zero `:artist/endYear` for Paul McCartney (`mccartney`), who is still active:

```
pull(&db, r#"[:artist/name [:artist/endYear :default 0]]"#, mccartney)?
```

```
=> {:artist/endYear 0, :artist/name "Paul McCartney"}
```

The default need not be of the same type as the attribute's values:

```
pull(&db, r#"[:artist/name [:artist/endYear :default "N/A"]]"#, mccartney)?
```

```
=> {:artist/endYear "N/A", :artist/name "Paul McCartney"}
```

## :xform Option

```
[attr-name ":xform" symbol]
```

The `:xform` option provides the ability to transform the value returned by pull for an attribute.

The symbol names either an explicitly registered native transform or one of the
following built-ins:

| Transform | Accepted value and result |
| --- | --- |
| `str` | Any query value to readable text; a top-level string remains unquoted and `nil` becomes `""` |
| `keyword` | A keyword unchanged, a symbol or string converted to a keyword, or `nil` unchanged |
| `symbol` | A symbol unchanged or a string converted to a symbol |
| `name` | The name of a keyword or symbol, or a string unchanged |
| `namespace` | The namespace of a keyword or symbol, or `nil` when it has none |
| `clojure.edn/read-string` | A string containing one EDN value, converted to a native query value |

The first five also accept a `clojure.core/` prefix. Names are resolved by the
EDN adapter, without loading or evaluating host-language code. To use additional
functions, register `PullTransform` callbacks and compile the selector with that
registry:

```rust
use edb_core::edn_pull::{EdnPullTransforms, parse_pull_edn_with_transforms};
use edb_core::{PullTransform, QueryValue, SemanticError, Value};

let mut transforms = EdnPullTransforms::new();
transforms.register("music/upper-name", PullTransform::new("music/upper-name", |v| {
    match v {
        QueryValue::Nil => Ok(QueryValue::Nil),
        QueryValue::Scalar(Value::String(s)) =>
            Ok(QueryValue::Scalar(Value::String(s.to_uppercase()))),
        _ => Err(SemanticError::incorrect("music/name-type", "expected a string")),
    }
}));
let pattern = parse_pull_edn_with_transforms(
    "[[:artist/name :xform music/upper-name]]", &transforms,
)?;
let result = db.pull(&pattern, led_zeppelin)?;
```

The function takes the value returned from the pull expression, which might be
`QueryValue::Nil`, and returns a `QueryValue` that will be included in the result
instead. Limits and nesting are applied first. A many-valued attribute passes
its entire selected collection to the function, not each element individually.

[`:default`](#default-option) values are not transformed by `:xform`, and a
non-nil `:xform` result takes precedence. If the transform returns nil, the
default is used when supplied; otherwise the attribute is omitted. For example,
`str` transforms a missing attribute into an empty string, so a default will not
replace it. These rules also apply when an ident or lookup reference fails to
resolve to an entity.

A callback can return `Err(SemanticError)` to stop Pull and report an error to
the caller. A panic is reported as `pull/transform-panicked`. Built-in type
mismatches and EDN reader failures are errors, not missing values.

### :xform Option Example

The following example uses the unqualified symbol `str` (from the default functions) to transform the result of pulling the `:artist/endYear` for led-zeppelin from an integer to a string:

```
pull(&db, r#"[[:artist/endYear :xform str]]"#, led_zeppelin)?
```

```
=> {:artist/endYear "1980"}
```

## Wildcards

```
wildcard           = '*'
```

The wildcard specification `*` pulls all attributes of an entity, and recursively pulls any [component attributes](#component-defaults).

### Wildcard Example

The wildcard pulls all the direct attributes of the release. It also recursively pulls `:release/media` because it is a component attribute. It does not recursively pull `:release/artists` or `:release/country`, because those are *not* component attributes.

```
pull(&db, r#"[*]"#, concert_for_bangla_desh)?
```

```
=>
{:release/name "The Concert for Bangla Desh",
 :release/artists [{:db/id 17592186049854}],
 :release/country {:db/id 17592186045504},
 :release/gid #uuid "f3bdff34-9a85-4adc-a014-922eef9cdaa5",
 :release/day 20,
 :release/status "Official",
 :release/month 12,
 :release/artistCredit "George Harrison",
 :db/id 17592186072003,
 :release/year 1971,
 :release/media
 [{:db/id 17592186072004,
   :medium/format {:db/id 17592186045741},
   :medium/position 1,
   :medium/trackCount 2,
   :medium/tracks
   [{:db/id 17592186072005,
     :track/duration 376000,
     :track/name "George Harrison / Ravi Shankar Introduction",
     :track/position 1,
     :track/artists [{:db/id 17592186048829} {:db/id 17592186049854}]}
    {:db/id 17592186072006,
     :track/duration 979000,
     :track/name "Bangla Dhun",
     :track/position 2,
     :track/artists [{:db/id 17592186048829}]}]}
  ...
  ]}
```

### Combining Wildcards and Map Specifications

A map specification can be used in conjunction with the wildcard to provide subpatterns for specific attributes.

#### Combining Wildcards and Map Specifications Example

The wildcard pulls all attributes of the `ghost-riders` track, and an explicit map uses the value of `:track/artists` to pull `:artist/name`.

```
pull(&db, r#"[* {:track/artists [:artist/name]}]"#, ghost_riders)?
```

```
=>
{:db/id 17592186063810,
 :track/duration 218506,
 :track/name "Ghost Riders in the Sky",
 :track/position 11,
 :track/artists
 [{:artist/name "Bob Dylan"} {:artist/name "George Harrison"}]}
```

## Recursion Limits

```
recursion-limit    = positive-integer | '...'
map-spec           = { ((attr-name | attr-expr) (pattern | recursion-limit))+ }
```

You can provide a positive number in a map specification to limit how deeply Pull should recur when encountering recursive references. You can optionally provide an ellipsis (…) instead of a number to allow recursion to arbitrary depth.

If a recursive subselect encounters an entity that it has already seen, it will not apply the pattern, instead returning only the `:db/id` of the entity. Thus recursive select is safe in the presence of cycles.

A recursive specification repeats the pattern containing that specification.
An explicit nested pattern instead selects its own attributes. Each recursive
specification has its own depth and cycle state: unrelated nested patterns and
other recursive specifications do not consume its allowance. A revisit on the
current recursive path returns an ID map, while the same entity reached through
a separate branch can be expanded again. Reaching a finite recursion limit also
returns an ID map for the next referenced entity.

Traversal uses an explicit work stack, so graph depth does not consume one Rust
call frame per entity. Cloning and dropping deeply nested `PullPattern` values
also use iterative traversal. Result size and work still grow with the selected
graph; use [traversal controls](#controls-and-errors) to bound expansion.

### Limited Recursion Example

The following (non-mbrainz) specification will pull the first and last names of friends-of-friends up to six degrees of separation from the original entity.

```
[:person/firstName :person/lastName {:person/friends 6}]
```

### Unlimited Recursion Example

The following specification will find all reachable friends, which might be most of the friends in the entire database.

```
[:person/firstName :person/lastName {:person/friends ...}]
```

### Empty Results

If there is no match between a pattern and an entity, then `pull` will return an empty map:

```
pull(&db, r#"[:penguins]"#, led_zeppelin)?
```

```
=> {}
```

Non-matching results will be removed entirely from the return map. Even though `ghost-riders` has artists, none of those artists have `:penguins`:

```
pull(&db, r#"[{:track/artists [:penguins]}]"#, ghost_riders)?
```

```
=> {:track/artists []}
```

## Pull Results

### Component Defaults

If a pull `attr-name` names a reference attribute, `pull` will return a map for the referenced value. If the attribute is a [component attribute](#component-defaults), the return map will contain all attributes of the related entity as well.

#### Component Defaults Example

Both `:release/media` and `:medium/tracks` are component attributes, so pulling `:release/media` will also pull related tracks. The example below pulls from `dark-side-of-the-moon`.

```
pull(&db, r#"[:release/media]"#, dark_side_of_the_moon)?
```

```
=>
{:release/media
 [{:db/id 17592186121277,
   :medium/format {:db/id 17592186045741},
   :medium/position 1,
   :medium/trackCount 10,
   :medium/tracks
   [{:db/id 17592186121278,
     :track/duration 68346,
     :track/name "Speak to Me",
     :track/position 1,
     :track/artists [{:db/id 17592186046909}]}
    {:db/id 17592186121279,
     :track/duration 168720,
     :track/name "Breathe",
     :track/position 2,
     :track/artists [{:db/id 17592186046909}]}
    {:db/id 17592186121280,
     :track/duration 230600,
     :track/name "On the Run",
     :track/position 3,
     :track/artists [{:db/id 17592186046909}]}
    ...]}]}
```

### Non-Component Defaults

If a reference is to a non-component attribute, the default is to pull only the `:db/id`. Reverse component references also default to an ID map, but produce one map rather than a collection; they do not automatically expand the parent.

#### Non-Component Defaults Example

Pulling `:artist/_country` of `:country/GB` returns only the entity ids for the artists from Great Britain:

```
pull(&db, r#"[:artist/_country]"#, edb_core::Keyword::new("country", "GB"))?
```

```
=> {:artist/_country [{:db/id 17592186045751} {:db/id 17592186045755} ...]}
```

### Multiple Results

If navigating an attribute might lead to more than one value, the pull result will be a list of the values found. These cases include:

- All forward cardinality-many references
- Reverse references for non-component attributes.

#### Multiple Results Example

Pulling `[:release/media]` of `dark-side-of-the-moon` pulls the values associated with `[:release/media]` and from inside of those results.

```
pull(&db, r#"[:release/media]"#, dark_side_of_the_moon)?
```

```
=>
{:release/media [{:db/id         23485568369468073
                  :medium/tracks [{:db/id         23485568369468074
                                   :track/artists [{:db/id 22940210601927955}]
                                   ...}]
                  ...}]}
```

### Missing Attributes

In the absence of a default, attribute specifications that do not match an entity are omitted from that entity's result map, rather than e.g. appearing with a `nil` value.

#### Missing Attributes Example

Paul McCartney has an `:artist/name` but not a `died-in-1966`, so only the former appears in a pull result:

```
pull(&db, r#"[:artist/name :died-in-1966?]"#, mccartney)?
```

```
=> {:artist/name "Paul McCartney"}
```

## Legacy Attribute Expressions

> NOTE: [Attribute Specifications](#attribute-specifications) provides a superset of the functionality of Attribute Expressions and is preferred. The legacy `limit` and `default` Attribute Expressions are also supported.

```
attr-expr           = [attr-name attr-option+] | legacy-attr-expr
legacy-attr-expr    = legacy-limit-expr | legacy-default-expr
legacy-limit-expr   = [("limit" | 'limit') attr-name (positive-integer | nil)]
legacy-default-expr = [("default" | 'default') attr-name any-value]
```

Attribute specifications can be wrapped in expressions to control the attribute's default or limit. Each is shown below.

## Legacy Limit Expression

```
legacy-limit-expr   = [("limit" | 'limit') attr-name (positive-integer | nil)]
```

The legacy limit expression is an alternate syntax for [Limit Option](#limit-option). Limit Option is preferred.

## Legacy Default Expressions

```
legacy-default-expr = [("default" | 'default') attr-name any-value]
```

The legacy default expression is an alternate syntax for [Default Option](#default-option). Default Option is preferred.

## Entity Identifiers and Missing Entities

Entities can be passed to Pull as entity IDs, idents, or lookup references:

```rust
use edb_core::{AttributeName, EntityIdentifier, Keyword, Value};

let by_id = db.pull(&pattern, led_zeppelin)?;
let by_ident = db.pull(&pattern, Keyword::new("country", "GB"))?;
let by_lookup = db.pull(&pattern, EntityIdentifier::Lookup {
    attribute: AttributeName::Ident(Keyword::new("artist", "gid")),
    value: Value::Uuid(0x678d88b287b0403bb63d5da7465aecc3),
})?;
```

The lookup attribute must be unique. `entity_identifier_from_edn` accepts a
nonnegative numeric entity ID, an ident keyword, or a two-element lookup vector
such as `[:artist/gid #uuid "678d88b2-87b0-403b-b63d-5da7465aecc3"]`.
Resolution occurs against the supplied immutable database value.

An unresolved ident or lookup reference still applies explicit defaults and
transforms. Explicit `:db/id` or a wildcard includes `:db/id nil` when resolution
fails. A numeric ID names that ID directly; selecting `:db/id` can therefore
return it even when it has no visible attributes. This does not create an entity
or facts. Without defaults, transforms, a wildcard, or `:db/id`, a pattern with
no matching attributes returns `{}`.

Forward non-component references return ID maps even when the referenced entity
has an ident. To retrieve that ident, use a nested pattern such as
`[{:artist/country [:db/id :db/ident]}]`. Lazy entity navigation has a different
value representation and caches selected attributes as they are accessed; see
the [Entities reference](../02_core_concepts/04_entities.md) for reference
navigation, component `touch`, caching, and identity across time views.

## Pull from an Index

[Index-Pull](../06_indexes/03_index_pull.md) applies a Pull
pattern lazily while walking one AVET or AEVT attribute range. The index walk
and each projection use the same captured database value. AVET projects entity
IDs; AEVT projects reference values. Repeated references can produce repeated
maps. Direction, offset, and the optional result limit control the index walk;
they are separate from attribute limits within the Pull pattern.

Each cursor advance projects at most one result, and an error ends the cursor.
The pattern and index boundary are validated when the cursor is constructed,
including for an empty range. With `index_pull_with_control`, depth and entity
limits apply separately to each projection, while cancellation also covers
scanning, rejected filter candidates, and offsets. The default index result
limit is unlimited.

## Controls and Errors

Pull requires a point-in-time database value. Current, as-of, and filtered
point-in-time values can be used; a raw history view cannot be used for entity
navigation. The selected attributes and entity identifiers are resolved against
that captured value, not a newer connection state.

Holding a database value does not grant a storage-retention pin. Storage-backed
reads remain subject to the [captured-value retention
contract](../02_core_concepts/00_database_values.md#captured-value-retention).

`pull_with_control` accepts `PullControl` with `max_depth`, `max_entities`, and
a shared cancellation flag. Root depth is zero. Entity limits count expanded
occurrences, including repeated branches, rather than distinct entity IDs.
Defaults impose no practical depth or entity cap. Explicit selector recursion
limits and cycle detection remain independent of these controls. Exceeding a
control returns an error, not a truncated success. Cancellation is checked while
reading candidates and before and after transforms; arbitrary Rust callbacks
cannot be preempted inside an invocation.

Query-projected Pull shares the query's work, deadline, cancellation, and value
allocation allowance, including results materialized lazily after relational
evaluation. Standalone Pull and lazy entity access do not inherit a query budget.
Limits select the first visible values in index traversal
order, not an application-defined sort order. Filters may require examining more
candidates to produce that number of visible values. A `:limit` does not limit a
cardinality-one attribute or a reverse component reference.

`pull_many` returns one result per input entity in input order. The pattern and
point-in-time requirement are validated even for an empty entity collection.
Each entity Pull uses default traversal controls.

| Invalid request | Result |
| --- | --- |
| Zero limit | `db.error/invalid-limit` during execution |
| Zero recursion depth | `db.error/invalid-recur-limit` during execution |
| Negative or noninteger limit/depth | EDN conversion error |
| Duplicate explicit selector for an attribute and direction | EDN conversion or `pull/duplicate-attribute` error; aliases do not make duplicates distinct |
| Repeated or unknown option | `edn/pull-option` |
| Unregistered transform symbol | `edn/pull-transform` |
| Reverse navigation through a non-reference attribute | `pull/reverse-non-ref` |
| Nested selection on an encountered non-reference value | `pull/nested-non-ref` |
| Unknown numeric attribute ID | Schema lookup error |
| Unknown attribute keyword | Missing attribute; a default or transform can supply a value |
| Exhausted traversal control | `pull/depth-limit`, `pull/entity-limit`, or `pull/canceled` |

Explicit forward and reverse specifications for the same attribute are allowed.
Wildcards and one explicit specification are also allowed: the explicit selector
controls that attribute. Use distinct output aliases when distinct entries are
required; equal result keys occupy one map entry.

EDN pattern conversion also has depth, node, and byte admission limits. The
defaults are depth 128, one million nodes, and 32 MiB of accounted input. Typed EDN
callers can supply `EdnAdapterLimits` to `pull_pattern_from_edn_with_limits`;
raising the requested depth does not remove the reader's native depth ceiling.
