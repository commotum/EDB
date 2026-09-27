# Query Reference

This topic documents the data format for EDB Datalog queries and rules. Queries
are EDN data, accepted by `parse_query_edn`, or native Rust `Query` values. The
music examples use artists, tracks, releases and media; their results illustrate
an example database, not data installed automatically with EDB. Entity IDs,
counts, random samples and result order depend on the supplied database.

Examples show query data followed by `;; inputs:` comments in `:in` order. `db`
means a captured `DatabaseValue`. These comments describe arguments; they are
not forms to evaluate. Pass every argument to `EdnQuery::bind` as
`EdnQueryArgument::Source` or `EdnQueryArgument::Data`, then execute the bound
query and render its result:

```rust
use edb_core::{QueryControl, QuerySourceValue};
use edb_core::edn::read_edn;
use edb_core::edn_query::{parse_query_edn, EdnQueryArgument};

let query = parse_query_edn(
    r#"[:find ?e :in $ ?name :where [?e :artist/name ?name]]"#,
)?;
let bound = query.bind(&[
    EdnQueryArgument::Source(QuerySourceValue::Database(db.clone())),
    EdnQueryArgument::Data(read_edn(r#""The Beatles""#)?),
])?;
let outcome = bound.execute(&QueryControl::default(), None)?;
let result = bound.result_to_edn(&outcome.result)?;
```

For the command line, save query data in a file and run
`edb query --database music --file query.edn`. A file passed through `--inputs`
contains a vector of the non-source arguments, in order. Named relation sources
and database-free examples use `--sources`; see
[Query application data](01_application_data.md) and the
[EDN workflow](../01_tutorials/00_edn_workflow.md).

## Query Grammar

### Syntax Used In Grammar

```
'' literal
"" string
[] = list or vector
{} = map {k1 v1 ...}
() grouping
| choice
? zero or one
+ one or more
```

### Query Arg Grammar

```
query             = [find-spec return-map-spec? with-clause? inputs? where-clauses?]
find-spec         = ':find' (find-rel | find-coll | find-tuple | find-scalar)
find-rel          = find-elem+
find-coll         = [find-elem '...']
find-scalar       = find-elem '.'
find-tuple        = [find-elem+]
find-elem         = (variable | pull-expr | aggregate)
variable          = symbol starting with "?"
pull-expr         = ['pull' src-var? variable pattern]
pattern           = (pattern-name | pattern-data-literal)
pattern-name      = plain-symbol
plain-symbol      = symbol that does not begin with "$", "?", or "%"
aggregate         = [aggregate-fn-name fn-arg+]
fn-arg            = (variable | constant | src-var)
constant          = any non-variable data literal
src-var           = symbol starting with "$"
return-map-spec   = (return-keys | return-syms | return-strs)
return-keys       = ':keys' plain-symbol+
return-syms       = ':syms' plain-symbol+
return-strs       = ':strs' plain-symbol+
with-clause       = ':with' variable+
inputs            = ':in' (src-var | binding | pattern-name | rules-var)+
binding           = (bind-scalar | bind-tuple | bind-coll | bind-rel)
bind-scalar       = variable
bind-tuple        = [ (variable | '_')+]
bind-coll         = [variable '...']
bind-rel          = [ [(variable | '_')+] ]
rules-var         = '%'
where-clauses     = ':where' clause+
clause            = (not-clause | not-join-clause | or-clause | or-join-clause | expression-clause)
not-clause        = [ src-var? 'not' clause+ ]
not-join-clause   = [ src-var? 'not-join' [variable+] clause+ ]
or-clause         = [ src-var? 'or' (clause | and-clause)+]
or-join-clause    = [ src-var? 'or-join' [variable+] (clause | and-clause)+ ]
and-clause        = [ 'and' clause+ ]
expression-clause = (data-pattern | pred-expr | fn-expr | rule-expr)
data-pattern      = [ src-var? (variable | constant | '_')+ ]
pred-expr         = [ [pred fn-arg+] ]
fn-expr           = [ [fn fn-arg+] binding]
rule-expr         = [ src-var? rule-name (variable | constant | '_')+]
rule-name         = plain-symbol
```

See [the pull pattern grammar](06_pull_reference.md#pull-pattern-grammar) for the description of the pattern-data-literal rule.

### Query Rule Grammar

Note that the rule grammar reuses some terms from the query grammar above.

```
rule                       = [ [rule-head clause+]+ ]
rule-head                  = [rule-name rule-vars]
rule-name                  = plain-symbol
rule-vars                  = variable+ | ([variable+] variable*)
```

## Queries

```
query                      = [find-spec return-map-spec? with-clause? inputs? where-clauses?]
```

A query consists of:

- a *find-spec* that specifies variables and aggregates to return
- an optional *with-clause* to control how duplicate find values are handled
- an optional *inputs* clause that names the databases, data, and rules available to the query engine
- optional *where-clauses* that constrain and transform data

A query needs at least one variable input binding or where clause; merely
naming a database source is not enough.

EDB offers eager execution with `BoundEdnQuery::execute`, native
`QueryEngine::execute_sources`, and database-value query methods. Prepared
queries reuse validated query structure. `BoundEdnQuery::sequence` and native
query sequences defer Pull projection; relational work is eager. See
[query sources and reuse](00_queries.md) for native APIs and controls.

A query can also be a map whose section values are lists or vectors:

```edn
{:find [?name]
 :in [$]
 :where [[?artist :artist/name ?name]]}
```

Section names must be recognized and cannot repeat. `:find` must be nonempty.
`:rules` may contain static rule definitions instead of supplying `%`; do not
supply both. Omitted `:in` defaults to `$`; explicit empty `:in` supplies no
inputs, useful for source-free function queries.

### Query Example

This query limits datoms to `:artist/name` "The Beatles", and returns the entity ids for such results:

```
[:find ?e
 :where [?e :artist/name "The Beatles"]]
;; inputs: db
```

```
=> #{[26757714973567138]} ;; Result. Your exact value may differ.
```

EDN query files contain the vector directly. Do not add a Clojure quote or
wrap the query in a host-language function call; the EDN reader reads data.

## Find Specs

```
find-spec                  = ':find' (find-rel | find-coll | find-tuple | find-scalar)
find-rel                   = find-elem+
find-coll                  = [find-elem '...']
find-scalar                = find-elem '.'
find-tuple                 = [find-elem+]
find-elem                  = (variable | pull-expr | aggregate)
```

| Find Spec | Grammar | Returns |
| --- | --- | --- |
| `:find ?a ?b` | `find-rel` | relation of result tuples |
| `:find [?a ...]` | `find-coll` | collection of values |
| `:find ?a .` | `find-scalar` | one value, or nil when no row matches |
| `:find [?a ?b]` | `find-tuple` | one tuple, or nil when no row matches |

A *find-spec* is the literal `:find` followed by one or more *find-elems*, which
can be variables, Pull expressions or aggregates. All four forms are supported
by the native and EDN query APIs. A scalar or tuple find takes the first produced
result; when several rows match, it is not a uniqueness check and does not
provide a deterministic choice. Use predicates to identify the intended row.

Relations and collections do not promise presentation order. Sort results in
application code when order matters. The EDN adapter represents relations as sets of tuples (`#{[...] ...}`);
illustrative results below also use sequence notation to display their rows.
Native ref values render with `#edb/ref`; examples abbreviate them as integers.
`:with` can preserve repeated projected values, as described below.

The order of *find-elems* determines the order variables appear in a result tuple.

## Variables

```
variable                   = symbol starting with "?"
```

A *variable* is a symbol that begins with `?`. In a *find-spec*, variables control which variables are returned, and what order those variables appear in the result tuple.

Variables are case-sensitive e.g. `?track` and `?Track` are different variables.

### Find Variables Example

The query below specifies that the result tuples should contain the track name and duration. Note that the `?track` and `?e` variables are used in the query, but are not returned.

```
[:find ?name ?duration
  :where [?e :artist/name "The Beatles"]
         [?track :track/artists ?e]
         [?track :track/name ?name]
         [?track :track/duration ?duration]]
;; inputs: db
```

```
=>
[["Here Comes the Sun" 186000]
 ["Come Together" 257000]
 ["Hey Jude" 428000]
 ...]
```

In this example, the variable `?track` [unifies](#implicit-joins). The clauses for `:track/artists`, `:track/name` and `:track/duration` all must have `?track` in the entity slot to unify around the same entity.

## Pull Expressions

```
pull-expr                  = ['pull' src-var? variable pattern]
pattern                    = (pattern-name | pattern-data-literal)
```

A pull expression returns information about a variable as specified by a pattern. Each variable can appear in at most one pull expression. Pull expressions are fully described in the [Pull reference](02_pull_and_entities.md).

### Finding Pull Expression Example

Rather than returning just a variable, this query uses a pull expression to specify which attributes to return values for about the entity whose `:artist/name` is "The Beatles":

```
[:find (pull ?e [:artist/startYear :artist/endYear])
 :where [?e :artist/name "The Beatles"]]
;; inputs: db
```

```
=>
[[{:artist/startYear 1957 :artist/endYear 1970}]]
```

### Separation of Concerns

The Pull API provides a declarative interface where you specify *what* information you want for an entity without specifying *how* to find it. Pull expressions can be used in queries to find entities and return an explicit map with the specified information about each entity.

This example uses `songs-by-artist` to find all tracks for an artist, then uses different pull patterns to pull different information about the resulting entities.

```
;; songs-by-artist
[:find (pull ?t pattern)
    :in $ pattern ?artist-name
    :where
    [?a :artist/name ?artist-name]
    [?t :track/artists ?a]]

;; track-releases-and-artists
[:track/name
   {:medium/_tracks
    [{:release/_media
      [{:release/artists [:artist/name]}
       :release/name]}]}]

;; Pull only the :track/name
;; query: songs-by-artist
;; inputs: db [:track/name] "Bob Dylan"
```

```
=>
([{:track/name "California"}]
 [{:track/name "Grasshoppers in My Pillow"}]
 [{:track/name "Baby Please Don't Go"}]
 [{:track/name "Man of Constant Sorrow"}]
 [{:track/name "Only a Hobo"}]
...)
```

```
;; Use a different pull pattern to get the track name, the release name, and the artists on the release.
;; query: songs-by-artist
;; inputs: db track-releases-and-artists "Bob Dylan"
```

```
=>
([{:track/name "California",
   :medium/_tracks
   {:release/_media {:release/artists [{:artist/name "Bob Dylan"}]
                    :release/name "A Rare Batch of Little White Wonder"}}}]
 [{:track/name "Grasshoppers in My Pillow",
   :medium/_tracks
   {:release/_media {:release/artists [{:artist/name "Bob Dylan"}]
                    :release/name "A Rare Batch of Little White Wonder"}}}]
 [{:track/name "Baby Please Don't Go",
   :medium/_tracks
   {:release/_media {:release/artists [{:artist/name "Bob Dylan"}]
                    :release/name "A Rare Batch of Little White Wonder"}}}]
 [{:track/name "Man of Constant Sorrow",
   :medium/_tracks
   {:release/_media {:release/artists [{:artist/name "Bob Dylan"}]
                    :release/name "A Rare Batch of Little White Wonder"}}}]
 [{:track/name "Only a Hobo",
   :medium/_tracks
   {:release/_media {:release/artists [{:artist/name "Bob Dylan"}]
                    :release/name "A Rare Batch of Little White Wonder"}}}]
 ...)
```

### Deferred Pull Projection

`BoundEdnQuery::sequence`, `QueryEngine::sequence_sources` and
`DatabaseValue::query_sequence` expose a single-pass `QuerySequence`. Relational
joins and aggregates are prepared eagerly; Pull, including its transforms,
runs only for consumed rows. Each item is a
`Result<Vec<QueryValue>, SemanticError>`, including for scalar and collection
finds, whose values occupy one-column tuples.

`remaining_rows()` counts prepared tuples without realizing Pull or transforms.
A later projection failure can terminate iteration before this many successful
rows. After returning an error, the iterator returns no further items. A new
query invocation can still reuse the immutable database and prepared query.

Relational preparation and deferred projection share the original absolute
deadline, cancellation, work allowance and `max_value_bytes`, including bytes
already accounted during preparation. Time spent before consuming another row
counts against the deadline. `stats()` continues accumulating work and value
bytes as rows are consumed; `diagnostics()` describes relational preparation.
See [query diagnostics](04_diagnostics.md) for counter scope.

Pull is a projection of distinct entity bindings. Two distinct entities may
Pull to equal maps; native eager and sequence results retain both rows.

## Custom Query Functions

You can write your own custom functions for use as [aggregate](#aggregates),
[predicate](#predicates), or [function](#functions) clauses in a query. Supply a
caller-owned `QueryExtensions` registry when executing:

- `register_pure` supplies a source-free Rust function from query arguments to
  one `QueryValue`; the binding form destructures its result.
- `register_local` supplies a database-aware Rust function from a database and
  stored scalar arguments to rows of stored values.
- `register_program` supplies a validated native query program under its name
  and exact program hash.
- `register_aggregate` supplies a Rust function over an `AggregateGroup`; its
  aligned argument rows, constants and explicit source arguments preserve the
  group's basis and argument order.

Use an explicit symbol such as `app/normalize` in the query. Names resolve in
that invocation's registry, not through JVM classes, a classpath or arbitrary
language evaluation. The stock CLI has built-ins; custom callbacks need a Rust
host. See [native computation](../07_peer_api/02_native_computation.md).

Functions must be pure. Callbacks can stop the query by returning a
`SemanticError`, and expensive callbacks must cooperate with cancellation and
deadline controls. Native programs share the enclosing query's fuel and deadline;
arbitrary Rust callbacks cannot be forcibly preempted. Aggregate callbacks use
`group.check(work)` while doing substantial work.

## Return Maps

Supplying a return-map will cause the query to return maps instead of tuples. Each entry in the `:keys` / `:strs` / `:syms` clause will become a key mapped to the corresponding item in the `:find` clause.

| keyword | symbols become |
| --- | --- |
| :keys | keyword keys |
| :strs | string keys |
| :syms | symbol keys |

In the example below, the `:keys` `artist` and `release` are used to construct a map for each row returned.

```
[:find ?artist-name ?release-name
 :keys artist release
 :where [?release :release/name ?release-name]
        [?release :release/artists ?artist]
        [?artist :artist/name ?artist-name]]
;; inputs: db
```

```
=>
#{{:artist "George Jones" :release "With Love"}
  {:artist "Shocking Blue" :release "Hello Darkness / Pickin' Tomatoes"}
  {:artist "Junipher Greene" :release "Friendship"}
  ...}
```

Return maps also preserve the order of the `:find` clause. Native `ReturnMap`
values support key access with `get`, positional indexing with `[index]`, and
slice destructuring through `values()`:

```rust
let artist = row.get(&edb_core::Value::Keyword(
    edb_core::Keyword { namespace: None, name: "artist".into() },
));
let first_column = &row[0];
let columns = row.values();
```

Construct unqualified keys with `Keyword { namespace: None, name: "artist".into() }`
when calling `get`. EDN `:keys artist release` does this automatically. Return
maps require a relation or tuple find, exactly one key per find element, and
distinct plain-symbol keys. `:keys`, `:strs`, and `:syms` are mutually exclusive.
`BoundEdnQuery::result_to_edn` applies the adapter; execution itself retains the
native result. `QueryResult::into_return_maps_with_arity` performs the equivalent
conversion for native callers, including arity validation for empty results.

Native `ReturnMaps::keys()` retains the ordered key schema even when no rows
match. `shape()` distinguishes an absent tuple from an empty relation. Conversion
preserves nested values and rejects duplicate keys or rows of the wrong width.

## Aggregates

```
aggregate                  = [aggregate-fn-name fn-arg+]
fn-arg                     = (variable | constant | src-var)
```

An aggregate function appears in the find clause and transforms a result. Aggregate functions can take variables, constants, or src-vars as arguments.

Built-in aggregates take one variable, optionally preceded by a nonnegative
literal integer count for `min`, `max`, `rand` and `sample`. Custom aggregates
can take multiple variables, constants and explicit source variables.

Aggregates appear as lists in a find-spec. Query variables not in aggregate expressions will group the results and appear intact in the result.

### Example Aggregate

This query binds `?a ?b ?c ?d`, then groups by `?a` and `?c`, and produces a result for each aggregate expression for each group, yielding 5-tuples.

```
[:find ?a (min ?b) (max ?b) ?c (sample 12 ?d)
 :where ...]
```

### Built-In Aggregates

Each of these is described in more detail below.

| aggregate | \# returned | notes |
| --- | --- | --- |
| avg | 1 |  |
| count | 1 | counts duplicates |
| count-distinct | 1 | counts only unique values |
| distinct | n | set of distinct values |
| max | 1 | compares all types, not just numbers |
| max n | n | returns up to n largest |
| median | 1 |  |
| min | 1 | compares all types, not just numbers |
| min n | n | returns up to n smallest |
| rand n | n | exactly n random selections from a nonempty group, with replacement |
| sample n | n | sample up to n, no duplicates |
| stddev | 1 |  |
| sum | 1 |  |
| variance | 1 |  |

### Aggregates Returning a Single Value

The aggregation functions that return a single value are listed below, and all behave as their names suggest.

- *min* and *max*
	The following query finds the smallest and largest track lengths:

	```
	[:find (min ?dur) (max ?dur)
	       :where [_ :track/duration ?dur]]
	;; inputs: db
	```

	```
	=> [[3000 3894000]]
	```

	The *min* and *max* aggregation functions use native logical value ordering, not just numeric comparison.

- *sum*
	The following query uses *sum* to find the total number of tracks on all media in the database.

	```
	[:find (sum ?count)
	       :with ?medium
	       :where [?medium :medium/trackCount ?count]]
	;; inputs: db
	```

	```
	=> [[100759]]
	```

- *count* and *count-distinct*
	More than one artist can have the same name. The following query uses *count* to report the total number of artist names, and *count-distinct* to report the total number of unique artist names.

	```
	[:find (count ?name) (count-distinct ?name)
	       :with ?artist
	       :where [?artist :artist/name ?name]]
	;; inputs: db
	```

	```
	=> [[4601 4588]]
	```

	Note the use of a [with-clause](#with-clauses) so that equal names do not coalesce.

- Statistics: *median*, *avg*, *variance*, and *stddev*
	Are musicians becoming more verbose when naming songs? The following query reports the *median*, *avg*, and *stddev* of song title lengths (in characters), and includes *year* in the find set to break out the results by year.

	```
	[:find ?year (median ?namelen) (avg ?namelen) (stddev ?namelen)
	       :with ?track
	       :where [?track :track/name ?name]
	              [(count ?name) ?namelen]
	              [?medium :medium/tracks ?track]
	              [?release :release/media ?medium]
	              [?release :release/year ?year]]
	;; inputs: db
	```

	```
	=>
	[[1968 16 18.92181098534824 12.898760656290333]
	 [1969 16 18.147895557287608 11.263945894977244]
	 [1970 15 18.007481296758105 12.076103750401026]
	 [1971 15 18.203682039283294 13.715552693168124]
	 [1972 15 17.907170949841063 11.712941060399375]
	 [1973 16 18.19300100438759 12.656827911058622]]
	```

Numeric aggregates follow EDB's numeric domains. `sum` keeps exact integer or
decimal arithmetic unless a floating input selects Double approximation.
Integral median pairs use truncating integer division; decimal pairs use an
exact midpoint. `avg`, `variance` and `stddev` return approximate Double values.
Arithmetic range failures and numeric work limits are errors rather than silent
rounding. See [numeric operations](00_queries.md#numeric-operations).

### Aggregates Returning Collections

Collection shape is part of the result: `distinct` returns `QueryValue::Set`
(EDN `#{...}`), while bounded `min` and `max`, `rand` and `sample` return
collections. Distinctness and ordering also support general query values,
including nested sets, maps, characters and inert tags supplied as
[application data](01_application_data.md).

- *distinct*
	The *distinct* aggregate returns the set of distinct values in the collection.

	```
	[:find (distinct ?sort-name)
	       :with ?artist
	       :where [?artist :artist/name "Fire"]
	              [?artist :artist/sortName ?sort-name]]
	;; inputs: db
	```

	```
	=> [[#{"Fire"}]]
	```

- *min n* and *max n*
	The *min n* and *max n* aggregates return up to n least/greatest items. The following query returns the five shortest and five longest track lengths in the database.

	```
	[:find (min 5 ?millis) (max 5 ?millis)
	       :where [?track :track/duration ?millis]]
	;; inputs: db
	```

	```
	=>
	[[[3000 4000 5000 6000 7000]
	  [3894000 3407000 2928000 2802000 2775000]]]
	```

- *rand n and sample n*
	The *rand n* aggregate selects exactly *n* items with potential for duplicates, and the *sample n* aggregate returns up to *n* distinct items.
	The following query returns two random and two sampled artist names.

	```
	[:find (rand 2 ?name) (sample 2 ?name)
	       :where [_ :artist/name ?name]]
	;; inputs: db
	```

	```
	=>
	[[["Four Tops" "Ethel McCoy"]
	 ["Gábor Szabó" "Zapata"]]]
	```

## Inputs

```
inputs                     = ':in' (src-var | binding | pattern-name | rules-var)+
```

The inputs clause names and orders the inputs to a query. Inputs can be

- a database name, i.e. a symbol starting with `$`
- a variable binding, e.g. a symbol starting with `?`
- a pattern name, i.e. a plain symbol
- the rules var, i.e. the symbol `%`

A query has as many inputs as the arguments passed to `EdnQuery::bind`, and the inputs bind those argument values for use inside the query.

### Default Inputs

Most queries operate against a single database. So as a convenience, the inputs clause can be elided, and will default to a single database whose name is the dollar sign *$*.

For example, the following three queries are equivalent when given the same
source as their sole argument:

```edn
;; with only $ as input, the :in clause can be dropped
[:find ?e :where [?e :age ?age]]

;; use the shorter name $, which can be omitted from where clauses
[:find ?e :in $ :where [?e :age ?age]]

;; use $data to name the database
[:find ?e :in $data :where [$data ?e :age ?age]]
```

To restrict `?age` to a supplied value, add `?age` to `:in` and pass that value
as a second argument. Merely mentioning a variable in `:where` binds values
found in the source; it does not create an input parameter.

### Inputs Example

The query below takes the artist name as an input, so that this parameterized query can be re-used with different artist names.

Inside the query, `$` is bound to `db`, and `?name` is bound to "The Beatles".

```
[:find (pull ?e [:artist/startYear :artist/endYear])
 :in $ ?name
 :where [?e :artist/name ?name]]
;; inputs: db "The Beatles"
```

```
=> [[{:artist/startYear 1957 :artist/endYear 1970}]]
```

### Pattern Inputs

An input can be a pattern var, specifying a pattern to be used in pull expressions in the find clause.

The query below binds `pattern` to the artist's start year and end year.

```
[:find (pull ?e pattern)
 :in $ ?name pattern
 :where [?e :artist/name ?name]]
;; inputs: db "The Beatles" [:artist/startYear :artist/endYear]
```

```
=> [[{:artist/startYear 1957 :artist/endYear 1970}]]
```

## Binding Forms

A *binding form* tells how to map data onto variables. A variable name like *?artist-name* is the simplest kind of *binding*, assigning its value directly to variable. Other forms support destructuring the data into a tuple, a collection, or a relation:

| Binding Form | Binds |
| --- | --- |
| ?a | scalar |
| \[?a?b\] | tuple |
| \[?a …\] | collection |
| \[ \[?a?b \] \] | relation |

### Tuple Binding

```
bind-tuple                 = [ (variable | '_')+]
```

A tuple binding binds a set of variables to a single value each, passed in as a positional list or vector. A set cannot supply a positional tuple; use `_` to ignore a slot. The query below binds both artist name and release name to find the entity ids for releases of John Lennon's *Mind Games*:

```
[:find ?release
 :in $ [?artist-name ?release-name]
 :where [?artist :artist/name ?artist-name]
        [?release :release/artists ?artist]
        [?release :release/name ?release-name]]
;; inputs: db ["John Lennon" "Mind Games"]
```

```
=>
#{[17592186157686]
  [17592186157672]
  [17592186157690]
  [17592186157658]}
```

### Collection Binding

```
bind-coll                  = [variable '...']
```

A collection binding binds a single variable to multiple values passed in as a collection. This can be used to ask "or" questions involving the values of the collection binding.

This query shows how to ask "What releases are associated with either Paul McCartney *or* George Harrison?"

```
[:find ?release-name
  :in $ [?artist-name ...]
  :where [?artist :artist/name ?artist-name]
         [?release :release/artists ?artist]
         [?release :release/name ?release-name]]
;; inputs: db ["Paul McCartney" "George Harrison"]
```

```
=>
#{["My Sweet Lord"]
  ["Electronic Sound"]
  ["Give Me Love (Give Me Peace on Earth)"]
  ["All Things Must Pass"]
  ...}
```

### Relation Binding

```
bind-rel                   = [ [(variable | '_')+] ]
```

A relation binding is fully general, binding multiple variables positionally to a relation (collection of tuples) passed in. Each row must be a positional list or vector, not an unordered set. This can be used to ask "or" questions involving variables in the relation binding. For example, what releases are associated with either John Lennon's *Mind Games* or Paul McCartney's *Ram*?

```
[:find ?release
 :in $ [[?artist-name ?release-name]]
 :where [?artist :artist/name ?artist-name]
        [?release :release/artists ?artist]
        [?release :release/name ?release-name]]
;; inputs: db [["John Lennon" "Mind Games"] ["Paul McCartney" "Ram"]]
```

```
=>
#{[17592186157686]
  [17592186157672]
  [17592186157690]
  [17592186157658]
  [17592186063566]}
```

## Where Clauses

```
where-clauses              = ':where' clause+
clause                     = (not-clause | not-join-clause | or-clause | or-join-clause | expression-clause)
expression-clause          = (data-pattern | pred-expr | fn-expr | rule-expr)
```

A where clause limits the results returned. The most common kind of where clause is a data pattern that is matched against datoms in the database, but there are many other kinds of clauses to support negation, disjunction, predicates, and functions.

EDB selects among clauses whose required inputs are ready using a bound-value
score. Execution order can differ from written order; it is not a statistical
estimate of result cardinality. A wholly unbound database pattern can use an
EAVT scan, subject to the same query controls as selective index access. Use
[query diagnostics](04_diagnostics.md) to inspect the actual clause order and
access paths.

## Implicit Joins

`where` clauses implicitly join. If a variable appears in multiple clauses, those matches must [unify](#implicit-joins).

To start we'll form two queries to find the years of releases of The Beatles and Janis Joplin separately. (Remember the database covers only from 1968 to 1973).

```
;; query to find years of all Beatles Releases
[:find ?year
 :where [?artist :artist/name "The Beatles"]
        [?release :release/artists ?artist]
        [?release :release/year ?year]]
;; inputs: db
```

```
=> [[1969] [1970] [1973] [1968]] ;; (trivia question: where is that 1973 result from?)
```

```
;; query to find years of all Janis Joplin Releases
[:find ?year
 :where [?artist :artist/name "Janis Joplin"]
        [?release :release/artists ?artist]
        [?release :release/year ?year]]
;; inputs: db
```

```
=> [[1969] [1971] [1972] [1973]]
```

We can take advantage of implicit joins by combining these queries but utilizing the same `?year` variable in the `:release/year` clause while looking for the artists separately

Now the years when both The Beatles and Janis Joplin released an album can be found.

```
[:find ?year
 :where [?artist :artist/name "The Beatles"]
        [?release :release/artists ?artist]
        [?release :release/year ?year]

        [?artist2 :artist/name "Janis Joplin"]
        [?release2 :release/artists ?artist2]
        [?release2 :release/year ?year]]
;; inputs: db
```

```
=> [[1969] [1973]]
```

`?year` was matched for both `?release` and `?release2`.

## Data Patterns

```
data-pattern               = [ src-var? (variable | constant | '_')+ ]
```

A data pattern is a tuple that begins with an optional src-var which binds to a relation. The src-var is followed one or more elements that match the tuples of that relation in order. The relation is often an EDB database, so the components are E, A, V, Tx, and Op. The elements of data pattern can be

- variables, which unify and bind to values
- constants, which limit results to tuples that match the constant
- the blank `_` which matches anything

The example below has a single data pattern which operates as follows:

- `$mbrainz` binds to the db argument
- the constant `:artist/name` limits results to datoms with that value in their Attribute (A) position
- the constant "The Beatles" limits results to datoms with that value in their Value (V) position
- the variables `?e`, `?tx`, and `?op` bind to those positions in the matching datoms, if any

```
[:find ?e ?tx ?op
 :in $mbrainz
 :where [$mbrainz ?e :artist/name "The Beatles" ?tx ?op]]
;; inputs: db
```

```
=> [[26757714973567138 13194139533421 true]]
```

A database resolves attribute keywords, idents and lookup references against
its schema and current identity view. Raw relation sources contain literal data:
an attribute keyword or lookup-reference-shaped value is matched as data and
is not resolved through a schema. Raw rows shorter than the pattern do not
match; extra row columns are ignored. Raw relations can have more than five
columns. Database datom patterns use only E/A/V/Tx/Op positions.

Repeated variables within one pattern also unify. Each blank is independent.
A supplied database's current, history, as-of, since and filtered semantics are
preserved; a query never substitutes a newer connection value for its source.

A variable used in attribute position must identify an installed attribute
when it is bound. In particular, `[?a :db/ident ?name]` can bind entities that
are not attributes. Using those bindings as `?a` in `[?e ?a ?v]` can fail with
`query/unknown-attribute` instead of treating them as nonmatches. Constrain an
attribute inventory with `[?a :db/valueType _]` before using its entities as
attributes.

## Blanks

Sometimes you don't care about certain elements of the tuples in a query, but you must put something in the clause in order to get to the positions that you **do** care about. The underscore symbol (`_`) is a blank placeholder, and matches anything without [binding](#binding-forms) or [unifying](#implicit-joins).

For example, if you wanted a random artist name, you would need a data pattern that talked about A and V, but you would not care about the E component which precedes them. The following query uses the blank in the E position:

```
[:find (sample 1 ?name)
 :where [_ :artist/name ?name]]
;; inputs: db
```

```
=> [[["Aerosmith"]]]
```

*Do not use a dummy variable instead of the blank.* This will make the query engine do extra work by tracking binding and unification for a variable that you never intend to use. It will also make human readers do extra work, puzzling out that the dummy variable is intentionally not used.

Blanks do not cause [unification.](#implicit-joins) Clauses with multiple blanks will not unify despite appearing to have the same symbol used.

### Implicit Blanks

In data patterns, you should elide any trailing components you don't care about, rather than explicitly padding with blanks. The previous examples already demonstrates this by omitting the Tx and Op components from the pattern

```
;; unnecessary trailing blanks
[_ :artist/name ?name _ _]

;; better
[_ :artist/name ?name]
```

## Predicates

```
pred-expr                  = [ [pred fn-arg+] ]
```

A predicate is a built-in native operation or a registered query function. Predicates must be pure functions, i.e. they must be free of side effects and always return the same thing given the same arguments.

Predicates are invoked against variables that are already bound to further constrain the result set. If the predicate returns `false` or `nil` for a set of variable bindings, that set is removed.

### Predicate Example

The query below uses the built-in predicates `<=` and `<` to limit the results to artists whose name sorts greater than or equal to "Q" and less than "R", i.e. the artists whose name begins with "Q":

```
[:find ?name
 :where [_ :artist/name ?name]
        [(<= "Q" ?name)]
        [(< ?name "R")]]
;; inputs: db
```

```
=>
[["Quiet World"]
 ["Queen"]
 ["Quintessence"]
 ...]
```

You can use a registered pure function as a predicate. Only `false` and `nil` reject a binding; zero and empty collections are truthy.

### Range Predicates

The predicates `=`, `!=` (also `not=`), `<=`, `<`, `>`, and `>=` are special:
direct constraints on a data pattern's value can take advantage of a ready AVET
index. Bounds may be literals or already-bound inputs. This can avoid examining
every value that would be considered by an equivalent string predicate:

```edn
;; can narrow a ready AVET index
[(<= "Q" ?name)]
[(< ?name "R")]

;; ordinary string predicate
[(starts-with? ?name "Q")]
```

An attribute without a ready AVET projection uses the ordinary fallback. An
index flag alone does not imply completed backfill. Original predicate checks
remain in the query, and temporal and filtered source semantics still apply.
All comparison predicates require exactly two arguments and use native logical
value comparison, including cross-numeric equality.

The section [Built-in Predicates and Functions](#built-in-predicates-and-functions)
lists the built-in operations.

## Functions

```
fn-expr                    = [ [fn fn-arg+] binding]
```

Queries can call built-in native functions and functions supplied through `QueryExtensions`. Such functions must be pure functions, i.e. they must be free of side effects and always return the same thing given the same arguments.

Functions are invoked against variables are that are already bound, and their results are interpreted via [binding forms](#binding-forms) to bind additional variables.

### Function Example

The example below uses the division function `quot` call to convert track lengths from milliseconds to minutes:

```
[:find ?track-name ?minutes
 :in $ ?artist-name
 :where [?artist :artist/name ?artist-name]
        [?track :track/artists ?artist]
        [?track :track/duration ?millis]
        [(quot ?millis 60000) ?minutes]
        [?track :track/name ?track-name]]
;; inputs: db "John Lennon"
```

```
=>
#{["Crippled Inside" 3]
  ["Working Class Hero" 3]
  ["Sisters, O Sisters" 3]
  ["Only People" 3]
  ...}
```

An alternate example utilizing a [predicate](#predicates) with a function binding to find artists with names under 7 characters and show the number of characters in their name.

```
[:find ?name ?count
 :where [_ :artist/name ?name]
        [(<= "Q" ?name)]
        [(count ?name) ?count]
        [(< ?count 7)]]
;; inputs: db
```

```
=>
[["井上陽水" 4]
 ["Spring" 6]
 ["頭脳警察" 4]
 ["Sonoma" 6]
 ["Selda" 5]
 ["Tony" 4]
 ["Smile" 5]
 ["Sun Ra" 6]
 ...]
```

The section [Built-in Predicates and Functions](#built-in-predicates-and-functions) lists all built-in functions.

## Built-in Predicates and Functions

EDB provides the following built-in expression functions and predicates:

| Kind | Names | Arguments |
| --- | --- | --- |
| Comparison predicates | `=`, `!=`, `not=`, `<`, `<=`, `>`, `>=` | exactly two values |
| Arithmetic | `+`, `-`, `*`, `/`, `quot` | exactly two numeric values |
| Counting | `count` | nil, string, bytes or collection |
| String construction | `str` | zero or more values |
| Substring | `subs` | string, start, optional end |
| String predicates | `starts-with?`, `ends-with?`, `includes?` | string and search string |
| Data operations | `ground`, `tuple`, `untuple` | documented below |
| Database operations | `fulltext`, `get-else`, `get-some`, `missing?` | explicit database source followed by arguments |
| Nested queries | `q`, `edb.api/q` | literal query and its arguments |
| Log operations | `tx-ids`, `tx-data` | explicit log source followed by arguments |

Arithmetic, counting, substring, string construction, comparison, data, database
and log built-ins also accept their `clojure.core/` names. String predicates use
`clojure.string/` aliases; nested queries accept only `q` and `edb.api/q`. These are native
name aliases; EDB does not make the rest of Clojure's runtime available. An
unrecognized function name resolves through the extension registry and fails
with `query/unknown-extension` if none is registered.

`count` counts Unicode scalar values in strings, bytes in byte arrays, and
members in collections; nil has count zero. `subs` uses Unicode scalar positions
with an inclusive start and exclusive end. Its indices must be nonnegative
Long or BigInt values within the string, and end cannot precede start.

Arithmetic supports Long, BigInt, BigDec, Float and Double. Integral `/` truncates
toward zero, avoiding a ratio value type. Exact decimal division must terminate;
it does not silently round. Long overflow, division by zero and invalid numeric
arguments fail. Explicit floating arguments choose Double approximation. Numeric
entity refs are identity values, not arithmetic operands.

### fulltext

```
[(fulltext src-var attr search) [[?ent ?val ?tx ?score]]]
```

The *fulltext* function takes a database, an attribute, and a search expression, and returns a relation of four-tuples: entity, value, transaction, and score. The attribute must be a fulltext-enabled string attribute.

The following query finds all the artists whose name includes "Jane":

```
;; query
[:find ?entity ?name ?tx ?score
 :in $ ?search
 :where [(fulltext $ :artist/name ?search) [[?entity ?name ?tx ?score]]]]

;; inputs
db, "Jane"
```

```
=>
#{[17592186047274 "Jane Birkin" 2839 0.625]
  [17592186046687 "Jane" 2267 1.0]
  [17592186047500 "Mary Jane Hooper" 3073 0.5]}
```

Fulltext runs against the supplied database value and composes with ordinary
joins. Its default candidate limit and corpus statistics affect results; do not
treat query relation order as score order. Recent committed and speculative
assertions are merged with the captured persisted projection. See
[Fulltext search](03_fulltext.md) for analyzer behavior, limits and consistency.

### get-else

```
[(get-else src-var ent attr default) ?val-or-default]
```

The *get-else* function takes a database, [an entity identifier](../04_transactions/00_transactions.md), a cardinality-one attribute, and a default value. It returns that entity's value for the attribute, or the default value if the entity does not have a value. The default cannot be nil.

The query below reports "N/A" whenever an artist's *startYear* is not in the database:

```
[:find ?artist-name ?year
 :in $ [?artist-name ...]
 :where [?artist :artist/name ?artist-name]
        [(get-else $ ?artist :artist/startYear "N/A") ?year]]
;; inputs: db ["Crosby, Stills & Nash" "Crosby & Nash"]
```

```
=>
#{["Crosby, Stills & Nash" 1968]
  ["Crosby & Nash" "N/A"]}
```

### get-some

```
[(get-some src-var ent attr+) [?attr ?val]]
```

The *get-some* function takes a database, [an entity identifier](../04_transactions/00_transactions.md), and one or more cardinality-one attributes, returning a tuple of the attribute id and value for the first attribute possessed by the entity. If none of the attributes is present, it produces no tuple and that input binding does not survive the clause.

The query below tries to find a *:country/name* for an entity, and then falls back to *:artist/name*:

```
[:find ?e ?attr ?name
 :in $ ?e
 :where [(get-some $ ?e :country/name :artist/name) [?attr ?name]]]
;; inputs: db :country/US
```

```
=> [[:country/US 84 "United States"]]
```

### ground

```
[(ground const) binding]
```

The *ground* function takes a single argument, which must be a constant, and returns that same argument. Programs that know information at query time should use *ground* to introduce literal data directly, without an application callback. Its result supports all four binding forms.

```
[(ground [:a :e :i :o :u]) [?vowel ...]]
```

### missing?

```
[(missing? src-var ent attr)]
```

The *missing?* predicate takes a database, [an entity identifier](../04_transactions/00_transactions.md), and an attribute and returns true if the entity has no value for attribute in the database.

The following query finds all artists whose start year is not recorded in the database.

```
[:find ?name
 :where [?artist :artist/name ?name]
        [(missing? $ ?artist :artist/startYear)]]
;; inputs: db
```

```
=>
#{["Sigmund Snopek III"]
  ["De Labanda's"]
  ["Baby Whale"]
  ...}
```

### q

The *q* function allows you to perform nested queries. Its first argument must
be literal query data, followed by arguments matching the nested query's `:in`.
A source argument names an enclosing immutable source. Pattern arguments may be
literal or refer to enclosing patterns; `%` can forward enclosing rules. The
nested query returns its find shape for destructuring. Return-map clauses apply
only to top-level results and are rejected inside `q`.

The example below shows using a nested query to bind the the `?duration` variable for use by an enclosing query that returns the entity id and name of the shortest tracks:

```
[:find ?track ?name ?duration
 :where
 [(q [:find (min ?duration)
       :where [_ :track/duration ?duration]]
     $) [[?duration]]]
 [?track :track/duration ?duration]
 [?track :track/name ?name]]
;; inputs: db
```

```
=>
[[8334298138708635 "Nutopian International Anthem" 3000]
 [9007199254790299 "Nutopian International Anthem" 3000]]
```

### tuple

```
[(tuple ?a ...) ?tup]
```

Given one or more values, the *tuple* function returns a [tuple](../03_schema/01_identity_and_values.md) containing each value. See also [untuple](#untuple).

```
[:find ?tup
 :in ?a ?b
 :where [(tuple ?a ?b) ?tup]]
;; inputs: 1 2
```

```
=> #{[[1 2]]}
```

### untuple

```
[(untuple ?tup) [?a ?b]]
```

Given a [tuple](../03_schema/01_identity_and_values.md), the untuple function can be used to name each element of the tuple. See also [tuple](#tuple).

```
[:find ?b
 :in ?tup
 :where [(untuple ?tup) [?a ?b]]]
;; inputs: [1 2]
```

```
=> #{[2]}
```

### tx-ids and tx-data

```edn
[(tx-ids $log start end) [?tx ...]]
[(tx-data $log ?tx) [[?e ?a ?v ?tx ?added]]]
```

`tx-ids` returns transaction refs in an inclusive-start, exclusive-end range of
an immutable log. Either bound may be nil; other bounds are nonnegative T
values, transaction refs or instants. `tx-data` takes one T or transaction ref
and returns E/A/V/Tx/Op tuples for that transaction.

```edn
[:find ?tx ?e ?a ?v ?added
 :in $log ?start ?end
 :where [(tx-ids $log ?start ?end) [?tx ...]]
        [(tx-data $log ?tx) [[?e ?a ?v ?tx ?added]]]]
;; inputs: captured-log 0 nil
```

Supply a captured log with `QuerySourceValue::Log(connection.log())` or
`QueryDataSource::log`. A database or raw relation cannot substitute for the log
source. Captured logs exclude later commits and may perform storage reads.
Join `?tx` to a database pattern for `:db/txInstant` or application-defined
transaction provenance. See [transaction logs](../06_indexes/00_indexes_and_log.md).

## Calling Native Functions

Native functions can be used as query expression functions and predicates.
Register application functions in the Rust process executing the query. EDN
function symbols do not load Java methods, evaluate Clojure, or compile Rust.
Prefer an existing built-in when it expresses the operation you need.

### Registering Application Functions

A pure function needs no database source. The callback below wraps a value in
a tuple, which can then be destructured with a tuple binding:

```rust
let mut extensions = edb_core::QueryExtensions::new();
extensions.register_pure("app/wrap", |arguments, _control| {
    let [value] = arguments else {
        return Err(edb_core::SemanticError::incorrect(
            "app/arity", "expected one argument",
        ));
    };
    Ok(edb_core::QueryValue::Tuple(vec![value.clone()]))
});
```

```edn
[:find ?wrapped
 :in ?input
 :where [(app/wrap ?input) [?wrapped]]]
;; inputs: "hello"
;; execute with Some(&extensions)
```

### Calling String Predicates

The following query finds artists whose name contains "woo":

```edn
[:find ?name
 :where [_ :artist/name ?name]
        [(includes? ?name "woo")]]
;; inputs: db
```

```edn
[["Lee Hazlewood"] ["Cottonwood"] ["Chris Harwood"] ["Mirkwood"]
 ["Under Milkwood"] ["Dorothy Norwood"] ["Fleetwood Mac"]]
```

The built-in requires strings and is case-sensitive. It does not require a host
runtime type hint. Use the fulltext function for analyzed text search.

### Calling Expression Functions

The example below uses *subs* as an expression function to extract prefixes of
words:

```edn
[:find ?prefix
 :in [?word ...]
 :where [(subs ?word 0 5) ?prefix]]
;; inputs: ["hello" "antidisestablishmentarianism"]
```

```edn
[["hello"] ["antid"]]
```

## Not Clauses

```
not-clause                 = [ src-var? 'not' clause+ ]
```

With *not* clauses, you can express that one or more logic variables inside a query must **not** satisfy all of a set of predicates. A not clause removes already-bound tuples that satisfy its *clauses*. Unless you specify an explicit *src-var*, *not* clauses will target a source named *$*.

### Not Example

The following query uses a *not* clause to find the count of all artists who are not Canadian:

```
[:find (count ?eid)
 :where [?eid :artist/name]
        (not [?eid :artist/country :country/CA])]
;; inputs: db
```

```
=> [[4538]]
```

### How Not Clauses Work

Not clauses are evaluated like a subquery and return a set of tuples that is used to remove tuples from the query's result set via set difference. Since the removal of tuples from the query's result set is performed using set logic, not clauses have the potential to be much more efficient than expression predicates which must be applied iteratively to each tuple in the result set instead of to the entire result set.

When the subquery uses recursive rules, EDB completes its positive result set
before applying set difference. An intermediate recursive result cannot be
negated. Nested queries, repeated rule rounds and negative subqueries share
the enclosing execution's resource controls.

### Insufficient Binding for a Not Clause

All variables used in a *not* clause will unify with the surrounding query. This includes both the arguments to nested expression clauses as well as any bindings made by nested function expressions. EDB will attempt to push the *not* clause down until all necessary variables are bound, and returns an Incorrect `SemanticError` with code `query/insufficient-binding` if that is not possible.

The query below demonstrates the problem. It attempts to remove eids that are not associated with an `:artist/country`, without ever finding a set of eids to begin with:

```
[:find (count ?eid)
 :where (not [?eid :artist/country :country/CA])]
;; inputs: db

```

```
=> query/insufficient-binding: no remaining clause has its required variables bound
```

## Not-join Clauses

```
not-join-clause            = (src-var? 'not-join' [var+] clause+)
```

A *not-join* clause works exactly like a *not* clause, but also allows you to specify which variables should unify with the surrounding clause; only this list of variables needs binding before the clause can run.

*var* specifies which variables should unify.

### Not-join Example

In this next query, which returns the number of artists who didn't release an album in 1970, `?artist` is in the *var* clause and must unify with the surrounding query. `?release` is used only inside the *not-join* clause and will not unify.

```
[:find (count ?artist)
 :where [?artist :artist/name]
        (not-join [?artist]
                  [?release :release/artists ?artist]
                  [?release :release/year 1970])]
;; inputs: db
```

```
=> [[3263]]
```

### Multiple Clauses In not Or not-join

When more than one clause is supplied to not or not-join, they are evaluated as if connected by an [and clause](#and-clause).

The following query counts the number of releases named "Live at Carnegie Hall" that were not by Bill Withers.

```
[:find (count ?r)
 :where [?r :release/name "Live at Carnegie Hall"]
        (not-join [?r]
                  [?r :release/artists ?a]
                  [?a :artist/name "Bill Withers"])]
;; inputs: db
```

```
=> [[2]]
```

## Or Clauses

```
or-clause                  = [ src-var? 'or' (clause | and-clause)+]
```

With *or* clauses, you can express that one or more logic variables inside a query satisfy at least one of a set of predicates. An *or* clause constrains the result to tuples that satisfy at least one of its *clause* or *and-clauses*

The following query uses an *or* clause to find the count of all vinyl media by listing the complete set of media that make up vinyl in the *or* clause:

### Or Clause Example

```
[:find (count ?medium)
 :where (or [?medium :medium/format :medium.format/vinyl7]
            [?medium :medium/format :medium.format/vinyl10]
            [?medium :medium/format :medium.format/vinyl12]
            [?medium :medium/format :medium.format/vinyl])]
;; inputs: db
```

```
=> [[74751]]
```

### Or Clause Variables

All clauses used in an *or* clause must use the same set of variables, which will unify with the surrounding query. This includes both the arguments to nested expression clauses as well as any bindings made by nested function expressions. EDB will attempt to push the *or* clause down until all necessary variables are bound, and returns a `SemanticError` with code `query/insufficient-binding` if that is not possible.

### How Or Clauses Work

One can imagine *or* clauses turn into an invocation of an anonymous rule whose predicates comprise the *or* clauses. An explicit source on the enclosing *or* clause is inherited by its branches. Individual nested clauses can also specify named sources; each source must be supplied to the query.

## And Clause

```
and-clause                 = [ 'and' clause+ ]
```

Inside an *or* clause, you may use an *and* clause to specify conjunction. Conjunction is already the default outside *or*. EDB also accepts an explicit *and* there and flattens its clauses.

### And Clause Example

The following query uses an *and* clause inside the *or* clause to find the number of artists who are either groups or females:

```
[:find (count ?artist)
 :where (or [?artist :artist/type :artist.type/group]
            (and [?artist :artist/type :artist.type/person]
                 [?artist :artist/gender :artist.gender/female]))]
;; inputs: db
```

```
=> [[2323]]
```

## Or-join Clause

```
or-join-clause             = [ src-var? 'or-join' [variable+] (clause | and-clause)+ ]
```

An *or-join* clause is similar to an *or* clause, but it allows you to specify which variables should unify with the surrounding clause. `[variable+]` specifies which logic variables should unify; variables not listed are local to each branch. Unlike *not-join*, an *or-join* can bind previously unbound join variables through its branches. Each branch still needs the inputs required by its own predicates, functions and rules.

### Or-join Example

In this query, which returns the number of releases that are either by Canadian artists or released in 1970, `?artist` is only used inside the *or* clause and doesn't need to unify with the outer clause. *or-join* is used to specify that only `?release` needs unifying.

```
[:find (count ?release)
 :where [?release :release/name]
        (or-join [?release]
                 (and [?release :release/artists ?artist]
                      [?artist :artist/country :country/CA])
                 [?release :release/year 1970])]
;; inputs: db
```

```
=> [[2124]]
```

## With Clauses

A *with-clause* considers additional variables not named in the *find-spec* when forming the basis set for a query result. The with variables are then removed, leaving a bag (not a set!) of values to be consumed by the *find-spec*. This is particularly useful when finding aggregates.

The basis is still a set: identical input rows coalesce even with `:with`.
Equal values from distinct identities remain separate observations. For example,
this query counts two observations of `"same"`, despite three input rows:

```edn
[:find (count ?name) .
 :with ?id
 :in [[?id ?name]]]
;; inputs: [[1 "same"] [1 "same"] [2 "same"]]
;; result: 2
```

### Example with-clause

Consider the following example, where our intention is to find out the years of every Bob Dylan release.

```
;; incorrect query
[:find ?year
 :where [?artist :artist/name "Bob Dylan"]
        [?release :release/artists ?artist]
        [?release :release/year ?year]]
;; inputs: db
```

```
=> [[1969] [1970] [1971] [1973] [1968]]
```

Bob Dylan was clearly a more prolific artist than this. The query returned *the years Bob Dylan released* records, rather than the release years of each of the records.

Set logic combines all of the releases that came out in the same year, and this is not what is wanted for the particular query.

A *with-clause* corrects this query:

```
;; fixed query
[:find ?year
 :with ?release
 :where [?artist :artist/name "Bob Dylan"]
        [?release :release/artists ?artist]
        [?release :release/year ?year]]
;; inputs: db
```

```
=> [[1973] [1971] [1973] [1973] [1970] [1968] [1971] [1969] [1968] [1970] [1973] [1970] [1971] [1970] [1973] [1968] [1971] [1973] [1970] [1969] [1971] [1970]]
```

This result is more like what we wanted. This is a list of the year of *each release* between 1968 and 1973.

## Rules

EDB Datalog allows you to package up sets of `:where` clauses into named *rules*. These rules make query logic reusable, and also composable, meaning that you can bind portions of a query's logic at query time.

### Defining a Rule

```
rule                       = [ [rule-head clause+]+ ]
rule-head                  = [rule-name rule-vars]
rule-name                  = plain-symbol
rule-vars                  = [variable+ | ([variable+] variable*)]
```

As with transactions and queries, rules are described using data structures. A rule is a list of lists. The first list in the rule is the *rule-head*. It names the rule and specifies its *rule-vars*. The rest of the lists are clauses that make up the body of the rule.

In the example below, the rule-head is `track-info`, and the three clauses of the rule body join artists to name and duration information about tracks:

```
[(track-info ?artist ?name ?duration)
 [?track :track/artists ?artist]
 [?track :track/name ?name]
 [?track :track/duration ?duration]]
```

### Using a Rule

```
inputs                     = ':in' (src-var | binding | pattern-name | rules-var)+
rules-var                  = the symbol "%"
rule-expr                  = [ src-var? rule-name (variable | constant | '_')+]
```

You have to do two things to use a rule in a query. First, you have to pass a *rule set* (collection of rules) as an input source and reference it in the *:in* section of your query using the '%' symbol. Second, you have to invoke one or more rules with a *rule-expr* in the *:where* section of your query.

The example below puts the `track-info` rule into a collection and names the rules with `%`. It then invokes the rule `track-info` by name in the where clause:

```
;; rules
[[(track-info ?artist ?name ?duration)
     [?track :track/artists ?artist]
     [?track :track/name ?name]
     [?track :track/duration ?duration]]]

[:find ?name ?duration
 :in $ % ?aname
 :where [?artist :artist/name ?aname]
        (track-info ?artist ?name ?duration)]
;; inputs: db rules "The Beatles"
```

```
=>
[["Here Comes the Sun" 186000]
 ["Hey Jude" 428000]
 ["Come Together" 257000]
 ...]
```

### Multiple Rule Heads

Rules with multiple definitions will evaluate them as different logical paths to the same conclusion (i.e. logical OR). In the rule below, the rule name `benelux` is defined three times. As a result, the rule matches artists from any of the three Benelux countries:

```
;; rules
[[(benelux ?artist)
     [?artist :artist/country :country/BE]]
    [(benelux ?artist)
     [?artist :artist/country :country/NL]]
    [(benelux ?artist)
     [?artist :artist/country :country/LU]]]

[:find ?name
 :in $ %
 :where (benelux ?artist)
        [?artist :artist/name ?name]]
;; inputs: db rules
```

```
=>
[["Earth and Fire"]
 ["André Brasseur"]
 ["Nico Gomez & His Afro Percussion Inc."]
  ...]
```

### Required Bindings

Rules normally operate exactly like other items in a where clause. They must unify with the variables already bound, and must bind any variables not already bound.

But sometimes you know that a rule will only be correct, or only be efficient, if some variables are already bound. You can require that some variables be bound before a rule can fire by enclosing the required variables in a vector or list as the first argument to the rule. If the required variables are not bound, EDB reports an Incorrect `SemanticError` with code `query/insufficient-binding`.

The native Rust `Rule::required` field specifies zero-based argument positions
directly; those positions need not form a contiguous prefix.

In the example below, the `track-info` rule has `?artist` as a required binding, and a query that does not bind `?artist` fails:

```
;; rules
[[(track-info [?artist] ?name ?duration)
     [?track :track/artists ?artist]
     [?track :track/name ?name]
     [?track :track/duration ?duration]]]

[:find ?artist ?name ?duration
 :in $ %
 :where (track-info ?artist ?name ?duration)]
;; inputs: db rules

```

```
=> query/insufficient-binding: no remaining clause has its required variables bound
```

This can be fixed easily:

```
;; rules
[[(track-info [?artist] ?name ?duration)
     [?track :track/artists ?artist]
     [?track :track/name ?name]
     [?track :track/duration ?duration]]]

[:find ?artist ?name ?duration
 :in $ %
 :where [?artist :artist/name "The Rolling Stones"]
        (track-info ?artist ?name ?duration)]
;; inputs: db rules
```

```
=>
[[62821696364619130 "Sittin’ on a Fence" 184040]
 [62821696364619130 "2000 Light Years From Home" 287000]
 [62821696364619130 "Jumpin' Jack Flash" 242066]
 [62821696364619130 "What to Do" 158000] ...]
```

### Rule Database Scoping

```
rule-expr                  = [ src-var? rule-name (variable | constant | '_')+]
```

By default, rules operate against the default database named by `$`. As with other *where* clauses, you may specify a database as a `src-var` before the rule-name to scope the rule to that database. Databases cannot be used as arguments in a rule.

The example below passes in two sources: the `$mbrainz` database, and an `$artists` relation. There is no default `$` input, so the data patterns and rule invocation explicitly name their sources:

```
;; rules
[[(track-info [?artist] ?name ?duration)
     [?track :track/artists ?artist]
     [?track :track/name ?name]
     [?track :track/duration ?duration]]]

[:find ?name ?duration
 :in $mbrainz $artists %
 :where [$artists ?aname]
        [$mbrainz ?artist :artist/name ?aname]
        ($mbrainz track-info ?artist ?name ?duration)]
;; inputs: db [["The Beatles"]] rules
```

```
=>
[["Here Comes the Sun" 186000]
 ["Come Together" 257000]
 ["Hey Jude" 428000]
...]
```

### Rule Generality

In all the examples above, the body of each rule is made up solely of data clauses. However, rules can contain any type of clause that a where clause might contain: data, expressions, or even other rule invocations.

Rules can invoke themselves or other rules recursively. EDB evaluates demanded
rule relations until their result sets stabilize. Negation must be stratified;
a recursive dependency cycle through negation fails with
`query/unstratified-negation`. Definitions sharing a name must agree on arity
and required-binding positions. An unbound rule-head variable is an error, not
an implicitly generated value.

Rule answers are scoped to the effective source and the invocation's bound
arguments. Calling the same rule against two named databases keeps their
answers separate, and a source prefix is inherited by nested rule calls unless
they explicitly name another source.

## Query Preparation and Reuse

Prepared-query reuse retains validated structure, not database values, input
arguments, callback registries, result rows or rule answers. Successful
source-independent rule-negation analysis may be reused; canceled or failed
analysis does not publish success. Each execution still resolves its supplied
sources and schedules clauses with its current bindings.

Structural equality is not semantic canonicalization: differently written
queries that happen to compute the same result need not share a cache entry.
See [prepared queries](00_queries.md#prepared-queries) for explicit prepared
handles and bounded cache ownership.

## Query Errors and Controls

Query operations return `Result<_, SemanticError>`. Inspect `category`, `code`,
`message`, and `details`; messages add context, while codes identify the failure.

| Error code | Meaning |
| --- | --- |
| `edn/query-input-arity` | argument count does not match `:in` |
| `edn/query-input-shape` | input has the wrong collection or positional shape |
| `edn/query-return-map-arity` | return keys do not match find width |
| `query/insufficient-binding` | no remaining clause has its required variables bound |
| `query/or-variable-mismatch` | ordinary or branches use different variable sets |
| `query/unknown-attribute` | an attribute term does not identify an installed attribute |
| `query/unbound-find-variable` | a find variable has no value in a result binding |
| `query/duplicate-pull-variable` | one variable occurs in more than one Pull expression |
| `query/unknown-extension` | function name is absent from the invocation registry |
| `query/predicate-arity`, `query/function-arity` | wrong argument count |
| `query/function-type` | unsupported argument type for an operation |
| `query/ground-not-constant` | ground received a variable instead of literal data |
| `query/rule-arity`, `query/rule-required-mismatch` | rule definitions disagree |
| `query/unstratified-negation` | recursive rule dependency crosses negation |

`QueryControl` provides work, intermediate-row, result-row, allocation, timeout,
and cancellation controls. Exceeding a limit fails the query; it does not
silently return a shortened relation. Eager execution and deferred Pull share
controls, and a sequence can fail while projecting a later row. Use explicit
limits for application-supplied queries, and use
[query diagnostics](04_diagnostics.md) to inspect clause execution and binding
failures.

**Next:** [Pull](06_pull_reference.md)
