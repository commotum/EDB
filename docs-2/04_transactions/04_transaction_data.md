# Transaction Data

Transaction data (tx-data) is the data representation of an EDB transaction. This page assumes that you are familiar with the semantics of EDB transactions, and provides a detailed reference for transaction data.

## Transaction Data Grammar

### Syntax

```
'' literal
"" string
[] = list or vector
{} = map {k1 v1 ...}
() grouping
| choice
? zero or one
+ one or more
* zero or more
```

The symbols `keyword`, `string`, `boolean`, `instant`, `uuid`, `long`, `bigint`, `float`, `double`, and `bigdec` are the [primitive value types](../03_schema/02_schema_reference.md#dbvaluetype) supported by EDB. Symbols, bytes, URIs, tuples, and stored function values are also supported. EDN tags distinguish types that do not have a standard EDN literal.

### Grammar

```
tx-data      = [tx-form*]
tx-form      = (list-form | map-form | builtin-form | tx-fn)
list-form    = ([':db/add' tx-entid identifier value]
              | [':db/retract' tx-entid identifier value?])
map-form     = {(':db/id' (tx-entid | nil))?
                (keyword map-value)*}
map-value    = (value | map-form | [map-value*])
tx-entid     = (identifier | tempid)
tempid       = nonempty string not beginning with ':'
identifier   = (eid | lookup-ref | ident)
eid          = nonnegative-long | #edb/ref nonnegative-integer
lookup-ref   = [identifier value]
ident        = keyword
value        = (string | keyword | symbol | boolean | ref | instant | uuid
              | number | bytes | uri | tuple | function)
ref          = tx-entid
number       = (long | bigint | float | double | bigdec)
builtin-form = ([':db/cas' tx-entid identifier (value | nil) value]
              | [':db/retractEntity' tx-entid]
              | [':db/ensure' tx-entid identifier])
tx-fn        = [tx-fn-name tx-fn-arg*]
tx-fn-name   = (identifier | qualified-symbol | function)
tx-fn-arg    = (value | nil | [tx-fn-arg*] | {(value tx-fn-arg)*})
```

## Transaction Data

```
tx-data                    = [tx-form*]
```

EDB represents transaction data as [EDN data structures](../01_tutorials/00_edn_workflow.md). This is a significant difference from SQL databases, where requests are submitted as strings. Using data instead of strings makes it easier to build requests programmatically.

Transaction data is *semantically* an unordered set of datoms, all of which are added to a database at an atomic moment in time. EDB provides a number of *mechanical* conveniences for writing such sets of datoms. First, transaction data is represented as an ordered list. This ordering is entirely for the convenience of transaction authors, who can produce (and print/read!) data in any order that is convenient for human understanding of the transaction.

Three kinds of forms can appear in a transaction data list:

- primitive list forms specify a single datom
- map forms provide a concise representation for multiple assertions about the same entity
- transaction functions allow functional expansions and validations of input data with reference to *db-before*, the database value at the start of the transaction; built-in forms provide compare-and-swap, entity retraction, and entity-spec checks

Inside these forms, there are several ways to specify entities:

- entity ids (E) are the actual entity ids stored in the database
- temporary ids (tempids) specify entities where a transaction author does not know or care to provide an entity id
- lookup refs specify entities by a domain unique identifier
- idents specify entities by a qualified keyword for use in programs, and are used to name e.g. attributes
- the reserved tempid "edb.tx" names the [current transaction](#reified-transactions)

Each of these data forms is described in detail below. Schema attributes, idents, lookup refs, and callable bindings resolve against db-before. Install an attribute in one transaction before using it in a later transaction.

Each transaction explicitly supplies one source entity reference alongside its data, resolved to an ordinary entity id in every resulting datom. All assertions and retractions, including generated changes, carry that source. Source references use the same entity resolution rules as other entity positions; they are not free-form labels.

Submit each transaction-data example below with `99` as its illustrative existing source entity id, supplied once through the request or speculative call (or CLI `--source 99`). The forms show the data relevant to each example. EDB automatically asserts the [transaction instant](#reified-transactions) with that same source.

The grammar describes the ordinary forms. EDN sets can also supply cardinality-many map values. Top-level partition directive maps are described in [Partitions](02_partitions.md). A transaction with no domain changes is valid, still requires an explicit transaction source, and receives its timestamp automatically. EDN input is data, not evaluated Clojure: bare symbols do not substitute application variables.

### Native Forms and Request Identity

Rust callers can construct `TxOp` and `TxForm` values directly, including `EntityMap`, `MapValue`, and unresolved `EntityRef` values. `read_edn_transaction` reads EDN into transaction forms without resolving attributes or entities. For a fresh durable request, resolution uses the authoritative db-before; for `with_edn`, it uses the database value supplied by the caller. Both representations use the same map expansion and transaction assessment rules.

`TxOp::Add`, `Retract`, and `Cas` retain their ordinary operation fields; `RetractEntity(E)`, `InstallAttribute(attribute)`, and `AlterAttribute(attribute)` retain their ordinary arguments. `EntityMap` contains its id and attribute values. These forms receive source from their enclosing transaction. Supply the required `source: EntityRef` once on `TransactionRequest`, using `TransactionRequest::new(key, operations, source)`, `TransactionRequest::from_forms(key, forms, source)`, or `TransactionRequest::from_edn(key, text, source)`. Speculation likewise supplies it through `DatabaseValue::with(ops, tx_instant, source)`, `with_forms(forms, tx_instant, source)`, or `with_edn(text, tx_instant, source)`. Limits, defaults, and execution options follow source in variants that accept them. `read_edn_transaction(text)` parses forms only; source belongs to its caller's request. An ordinary `TxOp::Add` for `:db/txInstant` may supply an explicit value matching the call's instant; EDB still records exactly one timestamp assertion.

Equivalent effects do not imply identical requests. EDN request identity ignores whitespace, comments, map-entry order, set-member order, and the order of the outer transaction forms. It retains the order and list/vector representation inside each form, numeric type distinctions, and decimal scales. The separately supplied transaction source is also part of request identity. Replacing an EDN map with primitive additions, changing a nested vector's order, or rebuilding EDN as typed Rust forms can therefore change request identity even when the resulting facts are the same.

For an [exact retry](00_transactions.md#interpret-the-outcome), retain the original request representation, request key, transaction source, and explicit basis/time options. A recorded receipt is resolved before fresh source, schema, ident, or function resolution or clock selection, so subsequent changes to those bindings do not reinterpret a committed request.

## Assert and Retract

```
list-form                  = ([':db/add' tx-entid identifier value]
                             | [':db/retract' tx-entid identifier value?])
```

A primitive list form asserts the addition (*:db/add*) or retraction (*:db/retract*) of a single datom. An addition indicates that a fact is true at a point in time, and a retraction indicates that a fact is not true at a point in time.

Both forms include an entity id, attribute, and value, with the value being optional for retractions. Their source comes from the enclosing transaction:

```
[:db/add entity-id attribute value]
[:db/retract entity-id attribute value?]
```

The example below asserts that 42's primary email is "edb@example.com":

```
[:db/add 42 :email/primary-address "edb@example.com"]
```

The example below asserts that 42's primary email is *no longer* "edb@example.com":

```
[:db/retract 42 :email/primary-address "edb@example.com"]
```

As a convenience, you can omit the value in a retract form, for example:

```
[:db/retract 42 :email/primary-address]
```

EDB will expand such forms to zero or more retractions, matching all additions of that E/A combination present in db-before. The transaction source identifies the retractor, not the source of the assertions being withdrawn. Retractions match E/A/V independently of the original source, and earlier assertions retain their attribution.

Note that adds and retracts in tx-data do not include the transaction id because it is not known until EDB assigns it.

While the names "add" and "retract" are verbs, tx-data forms describe facts rather than an ordered script. Previewing with `DatabaseValue::with`, `with_forms`, or `with_edn` expands and assesses the forms to produce a speculative db-after without publishing a transaction to storage. A primitive form acquires the selected transaction id during assessment:

```edn
;; Input form
[:db/retract 42 :email/primary-address "edb@example.com"]

;; Resulting datom, if the fact exists (illustrative transaction entity id)
[42 :email/primary-address "edb@example.com" 13194139635723 99 false]
```

For a cardinality-one attribute, adding a different value also retracts the previous value with the transaction's source. Omitting an attribute from transaction data leaves it unchanged. Cardinality counts distinct values, so different sources can assert the same value in separate transactions without a conflict. Adding two different values to the same cardinality-one attribute does not mean “last one wins”; the resulting conflict fails the transaction. Other derived assertions and retractions, including composite tuples, likewise carry the enclosing transaction's source.

### Redundancy Elimination

EDB will automatically exclude redundant datoms in tx-data. Datoms are considered redundant if they are equal to a current datom in every component except for tx-id, including source. An assertion with the same E/A/V from a different transaction source is retained. A retraction that does not match the E/A/V of a current assertion is redundant, as the absence of a datom already (and efficiently!) indicates that a fact is not true.

Note that entire transactions are never redundant, as the [transaction time](#reified-transactions) is always a new fact.

Some system attributes trigger schema processing. In particular, an assertion of `:db.alter/attribute` remains material even when the same fact is already present. See [Changing Schema](../03_schema/03_changing_schema.md). Reasserting a constituent attribute can also trigger composite-tuple population even when its own value is redundant.

## Map Forms

```
map-form                   = {(':db/id' (tx-entid | nil))?
                              (keyword map-value)*}
```

The map form is a shorthand for a set of additions. The map has an optional *:db/id* key identifying the entity, plus any number of attribute/value pairs. Its assertions receive the enclosing transaction's source.

```
{:db/id entity-id
 attribute value
 attribute value
 ... }
```

The map form is equivalent to a set of primitive additions, where the entity-id is taken from the *:db/id* key in the map (if present) or assigned an (anonymous) temporary id. An explicit `:db/id nil` is treated like an omitted id. `:db/id` is map syntax, not a stored attribute. Each resulting assertion carries the transaction's source. To contribute different attributes of one entity from different sources, submit separate transactions for that entity.

A map containing only `{:db/id "edb.tx"}` emits no datoms during map expansion. A populated `"edb.tx"` map expands its supplied attributes; EDB automatically records the transaction timestamp regardless of these maps.

Omitting `:db/id` does not guarantee a distinct new entity: a unique-identity assertion can resolve the anonymous tempid to an existing entity. Anonymous tempids do not collide with explicit tempid names in the submitted forms.

```
[:db/add entity-id attribute value]
[:db/add entity-id attribute value]
...
```

A map adds the supplied attributes; omitted attributes remain unchanged. For a cardinality-many attribute, supplied values add members to the existing set. Replacing that set requires explicit retractions of the unwanted members.

You can specify multiple values for a *:db.cardinality/many* attribute with a list in the value position. The following example transacts a person named "Bob" with multiple aliases:

```
[{:person/name "Bob"
  :person/aliases ["Robert" "Bert" "Bobby" "Curly"]}]
```

A single value can also be used for a cardinality-many attribute. To assert a single lookup reference on a cardinality-many ref attribute, wrap the lookup reference in an outer collection:

```edn
[{:db/id "jane"
  :person/friends [[:person/email "joe@example.com"]]}]
```

For a cardinality-one ref, the lookup reference is the value directly. For a cardinality-one tuple, a vector is one tuple value; for a cardinality-many tuple, supply a collection of tuple vectors. Scalar cardinality-one values are not unwrapped from singleton lists or vectors.

### Nested Maps

Nested entity assertions and the references connecting them to their parents all receive the enclosing transaction's source.

If an attribute has *:db.type/ref*, you can specify the related entity by nesting a map or maps in the value position. This can be used to specify a parent/child relationship. The map below specifies three entities in one transaction with source `99`. It assumes `:order/line-items` is a cardinality-many component ref, `:line-item/product` is a ref, and products named "Chocolate" and "Whisky" already exist with unique `:product/name` values:

```
[{:order/line-items [{:line-item/product [:product/name "Chocolate"]
                      :line-item/quantity 1}
                     {:line-item/product [:product/name "Whisky"]
                      :line-item/quantity 2}]}]
```

This map is equivalent to writing three separate maps:

```
[{:db/id "order"
  :order/line-items ["item-1", "item-2"]}
 {:db/id "item-1"
  :line-item/product [:product/name "Chocolate"]
  :line-item/quantity 1}
 {:db/id "item-2"
  :line-item/product [:product/name "Whisky"]
  :line-item/quantity 2}]
```

Which in turn is equivalent to writing each datom as a list form:

```
[[:db/add "order" :order/line-items "item-1"]
 [:db/add "order" :order/line-items "item-2"]
 [:db/add "item-1" :line-item/product [:product/name "Chocolate"]]
 [:db/add "item-1" :line-item/quantity 1]
 [:db/add "item-2" :line-item/product [:product/name "Whisky"]]
 [:db/add "item-2" :line-item/quantity 2]]
```

EDB requires that an anonymous nested map either be referenced by a forward [component attribute](../03_schema/02_schema_reference.md#dbiscomponent) or include a [unique](../03_schema/01_identity_and_values.md) attribute. A nested map with explicit `:db/id` can identify its entity directly without either condition. Either unique identity or unique value satisfies the anonymous-map guard; only unique identity supplies upsert semantics. This constraint prevents the accidental creation of easily-orphaned entities that have no unique identity or relation to other entities.

A newly allocated child in a nested component map receives partition affinity with its owner. Writing an ordinary primitive reference assertion does not supply this allocation policy. Existing entities identified by upsert retain their ids. See [Partitions](02_partitions.md) for allocation directives and defaults.

### Reverse References

A map key beginning with an underscore in the name portion reverses a ref attribute. For example, `:person/_friends` expresses the incoming `:person/friends` relationship:

```edn
[{:db/id [:person/email "joe@example.com"]
  :person/_friends [[:person/email "jane@example.com"]]}]
```

This asserts that Jane is friends with Joe. Reverse attributes must name ref-valued attributes. A reverse nested map creates or identifies the owner of the forward reference. A reverse component edge does not make that owner a component of the outer entity, so an anonymous reverse nested map still needs a unique attribute. Nested entity maps have a maximum depth of 32.

## Transaction Functions

```
tx-fn                      = [tx-fn-name tx-fn-arg*]
tx-fn-name                 = (identifier | qualified-symbol | function)
tx-fn-arg                  = (value | nil | [tx-fn-arg*] | {(value tx-fn-arg)*})
```

A list beginning with anything other than a built-in operation names a transaction function. A database entity identifier resolves a stored `:db/fn` binding. A `#edb/function` content hash names an already deployed native program. A qualified symbol names an explicitly registered native Rust callback. Reading transaction data does not evaluate Clojure code or load JVM classpath functions.

Function arguments may contain scalar values, `nil`, lists/vectors, and maps whose keys are stored values. A `#edb/ref` argument provides an explicit runtime entity reference. String and keyword arguments are ordinary values unless the function interprets them otherwise.

The built-in forms are:

```edn
[:db/cas entity attribute old-value new-value]
[:db/retractEntity entity]
[:db/ensure entity spec]
```

`:db.fn/cas` and `:db.fn/retractEntity` are accepted aliases. Compare-and-swap requires a cardinality-one attribute and compares its distinct value against db-before; `nil` in the old-value position means absence. The transaction's source applies to both the new assertion and any generated retraction. Entity retraction also retracts component descendants, attributing the retractions to the transaction's source. `:db/ensure` requests checks defined by an entity spec; it is virtual transaction input rather than a stored fact.

See [native computation](../07_peer_api/02_native_computation.md) for compiled callbacks and [persisted programs](01_persisted_programs.md) for durable program deployment. All function expansion runs against the transaction's immutable db-before and shares the transaction's resource limits. Functions return ordinary forms under the same rules as directly supplied forms; all their results receive the enclosing transaction's source.

The [transaction functions reference](07_transaction_functions.md) covers when
to use functions, built-in operations, cancellation, testing and deployment.

## Entity ids

```
eid                        = nonnegative-long | #edb/ref nonnegative-integer
```

Semantically, entity ids are opaque values uniquely identifying an entity within a database.

Programs can obtain entity ids for existing entities via query, pull, etc. Programs can also obtain entity ids for datoms added in a transaction via the return value from transact.

Use the report's `db_after` to query or pull the exact transaction result. It remains the same database value as later transactions commit. `Connection::db` captures the connection's most recently adopted value, which can have a different basis. An authoring map is transaction input, not a database query source.

Having obtained an entity id, you can use it anywhere in transaction data that an entity is expected. The example below shows equivalent list and map forms asserting that entity 42 is an active customer:

```
;; list form
[:db/add 42 :customer/status :active]
;; equivalent map form
{:db/id 42
 :customer/status :active}
```

For brevity, examples often show entity ids as famous small numbers such as '42' above. These are illustrative placeholders, not ids to reuse in a database; low numeric ids can be reserved system entities. In practice, most entity ids are not-so-famous large numbers due to the use of high bits for partitioning, e.g.:

```
{:db/id 17592186046416
 :customer/status :active}
```

An explicit numeric id must use a supported installed partition and an index below the database's issued-id frontier. Supplying an arbitrary integer does not allocate a new entity; use a tempid for allocation.

## Tempids

```
tempid                     = nonempty string not beginning with ':'
```

Temporary ids (tempids) specify entities without requiring an entity id. Syntactically, a temporary id is a nonempty string that must not begin with a colon. The exact string `"edb.tx"` is reserved for the current transaction.

There are many situations where tempids may be more convenient than specifying an entity id:

- no entity id exists yet, because the entity is "new"
- the transaction author does not know the entity id
- the specific entity id is unimportant, but it needs to be stated more than once in transaction data to relate different entities

Tempids can be used in transaction data wherever an entity (E) is expected. The examples below show a few uses of tempids in both list and map forms:

```
;; using "jdoe" to connect two datoms about the same entity
[[:db/add "jdoe" :person/first "Jan"]
 [:db/add "jdoe" :person/last "Doe"]]

;; using "jdoe" twice, to relate three different entities ceo->jdoe->intern
[[:db/add "ceo" :hr/manages "jdoe"]
 [:db/add "jdoe" :hr/manages "intern"]
 [:db/add "intern" :person/first "Alex"]]

;; using "jdoe" to relate two different map forms
[{:person/email "shaftoe@example.com"
  :person/friends ["jdoe"]}
 {:db/id "jdoe"
  :person/email "jdoe@example.com"}]
```

During transaction assessment, EDB assigns an entity id to every distinct tempid established in entity position. Both instances of "jdoe" below resolve to the same entity id:

```edn
[[:db/add "jdoe" :person/first "Jane"]
 [:db/add "jdoe" :person/last "Doe"]]

;; Resulting datoms (illustrative entity and transaction entity ids)
[[42 :person/first "Jane" 13194139635723 99 true]
 [42 :person/last "Doe" 13194139635723 99 true]]
```

A tempid used as a reference value or source must also be established in entity position in the same transaction; a reference-only or source-only tempid fails with `transaction/tempid-not-an-entity`. The intern example includes an assertion about "intern" for this reason. A new source can be described and used as a source in the same transaction, including as the source of its own description. The report's tempid map supplies the resolved ids. Tempids are local to one transaction; reusing a string in a later transaction does not identify the previous entity.

An assertion of a unique identity can resolve a tempid to an existing entity rather than allocate a new one. Unique value does not supply this upsert behavior. Multiple identities for one tempid must agree on the same existing entity. See [Value and Identity Rules](../03_schema/01_identity_and_values.md), particularly the rules for composite identity.

## Idents

Idents name entities with keywords that are unique within a database. Idents are intended for entities that will be named as literals in source code.

Idents are named via the *:db/ident* attribute, and are most often used to name schema attributes. The example below creates an entity named *:address/city*.

```
{:db/ident :address/city
 :db/valueType :db.type/string
 :db/cardinality :db.cardinality/one}
```

Idents can be used in EDB data anywhere an entity is expected. Idents are resolved against a database value, i.e. the explicit db passed to a query or the db-before of a transaction.

For example, in any transaction against a database including the *:address/city* entity above, you can use *:address/city* in the attribute position of a datom, for example:

```
[:db/add 42 :address/city "Olin"]
```

In fact, since idents name attributes, every example of transaction data on this page is an example of idents in use.

Idents can be used for other kinds of programmatic names beyond EDB schema, e.g. for categoric names of a domain value type (enumerations). Idents are for programmatic names only; for domain unique identifiers use unique identities and lookup refs.

EDB exposes ident resolution through the captured database value. Use `DatabaseValue::entid` to resolve a keyword and preserve that value’s basis when related reads must agree.

## Lookup Refs

```
lookup-ref                 = [identifier value]
```

A *lookup ref* allows you to specify entities by their domain-unique identities. Inside a lookup ref, the *identifier* names a [unique attribute](../03_schema/02_schema_reference.md#dbunique) in the database, and a value is a valid value for that attribute. For example, this lookup ref specifies the entity with the *:person/email* value "joe@example.com".

```
[:person/email "joe@example.com"]
```

EDB will expand a lookup ref to an entity id, returning an error if the A/V pair does not match any entity in db-before. Either unique identity or unique value can be used. A value asserted for the first time in this transaction cannot be found by a lookup ref in the same transaction; use a shared tempid instead. Lookup refs also work in ref-valued positions and supported ref slots in tuples.

## Entity Identifiers

```
tx-entid                   = (identifier | tempid)
tempid                     = nonempty string not beginning with ':'
identifier                 = eid | lookup-ref | ident
eid                        = nonnegative-long | #edb/ref nonnegative-integer
lookup-ref                 = [identifier value]
ident                      = keyword
```

Every datom is about an entity. There are three possible ways to identify an existing entity:

- an [eid](#entity-ids) for an entity that is already in the database
- an [ident](#idents) for an entity that has a `:db/ident`
- a [lookup ref](#lookup-refs) for an entity that has a unique attribute

In transactions, there is a fourth option for identifying an entity that might not exist yet:

- a [tempid](#tempids) for a new entity being added to the database

## Values

```
value                      = (string | keyword | symbol | boolean | ref | instant
                            | uuid | number | bytes | uri | tuple | function)
```

The value type of a datom is dictated by its attribute's schema. The value types are defined and demonstrated in the [Schema Data Reference](../03_schema/02_schema_reference.md#dbvaluetype).

EDN does not coerce a long into a double merely because the attribute expects a double. Use the appropriate literal or native tag. Common forms include `#inst "2026-09-26T12:00:00Z"`, `#uuid "f40e770e-9ad5-11e7-abc4-cec278b6b50a"`, `7N`, `1.0M`, `#edb/float "1.0"`, `#edb/uri "https://example.com"`, and `#edb/bytes "010203"`. See the [EDN frontend](../01_tutorials/00_edn_workflow.md) for interchange details.

`nil` is not a stored scalar value. It is allowed in tuple slots, an absent map id, the old-value position of compare-and-swap, and native function arguments. Arbitrary nested maps are not stored values: entity maps expand into facts and refs; function-argument maps are runtime data.

## Reified Transactions

When the transactor processes a transaction, it creates a transaction entity to represent it. This entity has a *:db/txInstant* attribute whose value is the transaction's selected instant. For a durable submission without an explicit instant, EDB selects the later of the preceding transaction's instant and the server clock. In addition, every datom in a transaction refers to the transaction entity through its *:tx*.

Every submission requires one explicit transaction source. EDB automatically records exactly one assertion of `:db/txInstant` with that source, including for empty transaction data. EDB selects the instant once; the order of tx-data does not affect it or trigger a separate clock read. An explicit time supplied for a historical import is validated under the bounds below.

The source uses the ordinary `tx-entid` resolution rules, including [tempids](#tempids) established in entity position in the same transaction. EDB does not infer it from credentials or other facts. Canonical genesis statements use the ordinary built-in source entity `:db.source/system` and have no transaction timestamp. Internal bootstrap transactions use that source; application transactions explicitly supply their own source.

A transaction has one source and one timestamp assertion. Explicit timestamp data supplies the selected value, rather than an additional timestamp contribution.

You can add additional attributes to a transaction entity to capture other useful information, such as the purpose of the transaction, the application that executed it, or the user who caused it to execute. These describe the transaction as a whole; every assertion or retraction records that transaction's source in the datom's source coordinate.

EDB will resolve the reserved [tempid](#tempids) "edb.tx" to the current transaction. You can use this tempid to make assertions about the current transaction as you would for any other entity.

The example below annotates a transaction imported from a file with a *:data/src* attribute to record the import URL. Supply source entity `99` through the request to identify the contributor of the assertions, independently of that URL.

```
[{:db/id "edb.tx"
  :data/src "https://example.com/catalogs/catalog-2_29_2012.xml"}
 {:product/name "Marbles"}]
```

You can query transaction entities like any other entities in the system.

### Explicit :db/txInstant

If you are importing past data that includes known transaction times, you may want EDB's *:db/txInstant* to reflect those times. You can include an explicit *:db/txInstant* addition in tx-data, overriding the transactor's clock time. This is useful when you are importing past data and want to assert when that information was known.

Explicit additions of *:db/txInstant* must respect the monotonic ordering of wall-clock time, i.e. you must choose a *:db/txInstant* value that is not older than any existing transaction, and not newer than the transactor's clock time:

```edn
[{:db/id "edb.tx"
  :db/txInstant #inst "2026-09-26T12:00:00Z"
  :data/src "https://example.com/catalogs/catalog-2_29_2012.xml"}]
```

Install the metadata attribute `:data/src` before transacting the example, and supply source `99` through its request. An ordinary `:db/add` can also supply the explicit timestamp value:

```edn
[[:db/add "edb.tx" :db/txInstant #inst "2026-09-26T12:00:00Z"]]
```

Repeated explicit additions of the same instant produce just one timestamp assertion with the transaction's source. Different explicit instants fail the transaction. All `:db/txInstant` assertions must target the current transaction. Retraction and compare-and-swap of `:db/txInstant` are rejected. If both transaction data and transaction options specify an instant, they must agree.

A speculative `with_edn` call uses the fixed instant supplied by its caller. An explicit `:db/txInstant` in its data must match that fixed instant and cannot precede db-before. A time override or fixed-time argument supplies the value; the caller also supplies the transaction source, and EDB records the timestamp automatically. Durable submission additionally checks the transactor's clock. Transaction instants may be equal across adjacent transactions; `basis_t` provides their unambiguous logical ordering. See [Transactions, Reports and Exact Retries](00_transactions.md).
