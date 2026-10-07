# Schema Data Reference

This document defines the grammar for schema data. Start with [Schema is database data](00_schema.md) for a short introduction, and [Value and identity rules](01_identity_and_values.md) for detailed equality and upsert behavior.

## Schema Grammar

### Syntax Used in Grammar

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

### Schema Map Grammar

```
attr-def    = {':db/ident' keyword
               ':db/cardinality' cardinality
               ':db/valueType' type
               (':db/doc' string)?
               (':db/index' boolean)?
               (':db/fulltext' boolean)?
               (':db/tupleType' scalar-type)?
               (':db/tupleTypes' [scalar-type scalar-type+])?
               (':db/tupleAttrs' [ident ident+])?
               (':db.tuple/discontinued' boolean)?
               (':db/unique' unique)?
               (':db/isComponent' boolean)?
               (':db/id' tx-entid)?
               (':db/noHistory' boolean)?
               (':db.attr/preds' (attr-pred | [attr-pred+]))?}
entity-spec = {(':db/ident' ident)?
               (':db.entity/attrs' (ident | [ident+]))?
               (':db.entity/preds' (ent-pred | [ent-pred+]))?}
tx-entid    = (identifier | tempid)
tempid      = string
identifier  = (eid | lookup-ref | ident)
eid         = nat-int
lookup-ref  = [identifier value]
ident       = keyword
attr-pred   = qualified symbol naming a /predicate/ of a value
ent-pred    = qualified symbol naming a /predicate/ of db-after and an eid
cardinality = (':db.cardinality/one' | ':db.cardinality/many')
type        = (':db.type/bigdec'  | ':db.type/bigint'  | ':db.type/boolean' |
               ':db.type/bytes'   | ':db.type/double'  | ':db.type/float'   |
               ':db.type/fn'      | ':db.type/instant' | ':db.type/keyword' | ':db.type/long'    |
               ':db.type/ref'     | ':db.type/string'  | ':db.type/symbol'  |
               ':db.type/tuple'   | ':db.type/uuid'    | ':db.type/uri')
unique      = (':db.unique/identity' | ':db.unique/value')
boolean     = ('true' | 'false')
```

### Grammar Notes

The grammar above shows only built-in schema attributes and the map entity identifier. Just like all other entities in EDB, schema entities are open and can have any attributes you define added to them.

The grammar shows only the transaction map form. It is also possible to define schema with the more verbose transaction list form, as these forms are semantically equivalent.

Each transaction requires one source supplied alongside its data through the request or CLI `--source` option. All application transaction examples below use source entity `99`, assumed to exist already. Every explicit and generated assertion or retraction receives that source, including the automatically recorded transaction instant. Source uses ordinary entity-reference resolution; a source tempid must also appear in entity position in that transaction.

Because schema is composed of ordinary EDB data, the schema grammar is a specialization of the transaction grammar. The shared grammar elements `tx-entid`, `identifier`, `eid`, `lookup-ref`, and `ident` are described in the [Transaction Data Reference](../04_transactions/04_transaction_data.md).

The productions describe semantic forms, not every reader spelling. Transaction input is an outer vector or list of maps and operation vectors/lists. Individual forms below are shown without their outer transaction wrapper where useful. An attribute requires exactly one tuple specification when its type is tuple and none otherwise. `scalar-type` is one of the permitted types listed under [Tuples](#tuples). Composite and heterogeneous definitions contain 2–8 slots; homogeneous tuple values also contain 2–8 slots. Entity specs are ordinary entities, not necessarily attributes: they do not need `:db/valueType` or `:db/cardinality`.

Tempids are nonempty strings that do not start with `:`. The string `"edb.tx"` names the current transaction. A numeric entity ID must be an issued ID in an installed partition; a nonnegative integer alone is not sufficient. `#edb/ref` is also accepted for explicit IDs. Lookup refs require an installed unique attribute and resolve against db-before. An absent or `nil` map `:db/id` requests an implicit tempid.

## Defining Schema

Attributes are defined using the same data model used for application data. That is, attributes are themselves defined by entities with associated attributes and are added to the database through an ordinary transaction. Use the [EDN workflow](../01_tutorials/00_edn_workflow.md) for CLI submission or the [native workflow](../../examples/native_workflow.rs) for Rust APIs. Install attributes before submitting application data that uses them: transaction attribute names resolve against db-before.

| Name | Purpose | Required? |
| --- | --- | --- |
| `:db/ident` | specifies a unique programmatic name for an entity (normally a schema entity) | Required for schema entities |
| `:db/cardinality` | specifies whether an attribute associates a single value or a set of values | Required |
| `:db/valueType` | specifies the type of value that can be associated with an attribute | Required |
| `:db/unique` | specifies a uniqueness constraint for the values of an attribute | Optional |
| `:db/index` | specifies a boolean value indicating that an index should be generated for this attribute. Defaults to false. | Optional |
| `:db/isComponent` | specifies whether an attribute is a ref to a component entity | Optional |
| `:db/noHistory` | specifies whether historical values should be forgotten for an attribute | Optional |
| `:db/doc` | specifies a documentation string for an attribute | Optional |
| `:db.attr/preds` | specifies one or more predicates that constrain an attribute's value by more than just its value type | Optional |
| `:db/fulltext` | specifies a boolean value indicating that a derived fulltext search index should be generated for the string attribute. Defaults to false. | Optional; immutable after installation |
| `:db/tupleType` | specifies the scalar type of a homogeneous tuple | One tuple specification required for tuple attributes |
| `:db/tupleTypes` | specifies the scalar types of a heterogeneous tuple | One tuple specification required for tuple attributes |
| `:db/tupleAttrs` | specifies the constituent attributes of a composite tuple | One tuple specification required for tuple attributes |
| `:db.tuple/discontinued` | permanently stops maintaining an installed composite tuple | Optional; defaults to false |
| `:db.entity/attrs` | names the required attributes of an entity spec | Optional on a spec |
| `:db.entity/preds` | names the predicates of an entity spec | Optional on a spec |

The example map form below shows an attribute that represents a person's name:

```
{:db/ident       :person/name
 :db/valueType   :db.type/string
 :db/cardinality :db.cardinality/one
 :db/doc         "A person's name"}
```

## :db/cardinality

```
{':db/cardinality' cardinality}
cardinality = (':db.cardinality/one' | ':db.cardinality/many')
```

The required `:db/cardinality` attribute specifies whether an attribute associates a single value or a set of values with an entity. It has no default value.

The values allowed for `:db/cardinality` are:

- `:db.cardinality/one` – the attribute is single-valued, it associates a single value with an entity.
- `:db.cardinality/many` – the attribute is multi-valued, it associates a set of values with an entity.

Cardinality-one assertion replaces a different existing value by retracting the old fact and asserting the new one. Both statements carry the transaction's supplied source; earlier assertions retain their original attribution. Multiple distinct resulting values fail with `transaction/cardinality-one-conflict`. Cardinality counts distinct values, so assertions of the same value from different sources in separate transactions preserve separate contributions without violating cardinality-one. For either cardinality, reasserting an existing value from the same source is redundant. For cardinality-many, a map may supply one scalar or a collection of values. A tuple or lookup reference on a many-valued attribute needs an outer collection:

```edn
;; :person/friends is a cardinality-many ref attribute.
[{:db/id [:person/email "alice@example.com"]
  :person/friends [[:person/email "bob@example.com"]]}]
```

Omitting an attribute from a transaction map leaves its existing facts unchanged. `nil` is not a stored scalar value; retract a fact to remove it.

## :db/doc

```
':db/doc' = string
```

The optional `:db/doc` specifies a documentation string, and can be any string value.

## :db/index

```text
{':db/index' boolean}
```

The optional `:db/index` requests AVET and SAVET access for an attribute. It defaults to false. Unique attributes participate in both independently of the explicit flag. SAVET uses the same eligibility and readiness, adding source-first value lookup and ranges. AVET provides value-oriented lookup and range access; it does not change the attribute's value type or cardinality.

Schema intent and physical index readiness are different. Setting `:db/index` on an existing attribute can require background backfill. Check `DatabaseValue::has_avet` or synchronize schema/index work before relying on the new access path. An already captured database value does not acquire a new attachment in place. See [Indexes and log](../06_indexes/00_indexes_and_log.md).

## :db/fulltext

```text
{':db/fulltext' boolean}
```

The optional `:db/fulltext` attribute specifies that a derived fulltext search index should be generated for the attribute. It defaults to false, requires `:db.type/string`, and cannot be changed after installation. Cardinality-one and cardinality-many strings are supported.

Fulltext analysis is case insensitive, removes apostrophes and English possessive endings, and filters the following common English stop words:

```text
a an and are as at be but by for if in into is it no not of on or such that
the their then there these they this to was will with
```

Accents remain and there is no stemming. Query expressions support terms, phrases, parentheses, Boolean operators and trailing prefix wildcards. The engine evaluates retained index data together with the captured committed tail and speculative assertions; background indexing affects work and corpus statistics. Scores are relative native BM25 relevance, not an identity constraint. Use structured equality for exact values. See [Native fulltext search](../05_query_and_pull/03_fulltext.md) for the complete grammar, visibility, budgets and indexing lifecycle.

## :db/id

```
{':db/id' tx-entid}
tx-entid       = (identifier | tempid)
identifier     = (eid | lookup-ref | ident)
eid            = nat-int
lookup-ref     = [identifier value]
ident          = keyword
```

`:db/id` is not an attribute; rather, it is syntactic sugar for specifying the entity identifier in a map form. For example, the following two forms are equivalent:

```
{:db/id "alan"
 :person/name "Alan Turing"}

[:db/add "alan" :person/name "Alan Turing"]
```

## :db/ident

```
{':db/ident' keyword}
```

The `:db/ident` attribute specifies a unique programmatic name for an entity. Idents are required for schema entities and are optional for all other entities.

Idents should be used for two purposes: to name schema entities and to represent enumerated values. To support these usages, idents are designed to be extremely fast and always available. EDB derives an ident lookup map from schema information and historical ident assertions for each database value.

When an entity has an ident, you can use that ident in place of the eid, e.g.

```
;; assuming that :person/loves is entity id 1007, these are equivalent
[:db/add "alan" :person/loves "pizza"]
[:db/add "alan" 1007 "pizza"]
```

These characteristics also imply situations where idents should *not* be used:

- Idents should not be used as unique names or ids on ordinary domain entities. Such entity names should be implemented with a domain-specific attribute that is a unique identity.
- Idents should not be used as names for test data. Your real data will not have such names, and you don't want test data to behave differently than the real data it simulates.

Use `DatabaseValue::entid` to resolve a keyword to an entity ID. Transactions accept idents in entity and attribute positions, in reference-valued positions, and as the supplied transaction source. A keyword on a keyword-valued attribute remains a keyword value. Query and Pull adapters resolve identifiers in their schema-aware positions; see [Queries](../05_query_and_pull/00_queries.md) and [Pull and entities](../05_query_and_pull/02_pull_and_entities.md).

Renaming an ident does not change the entity ID. Historical ident assertions preserve old names as aliases; retracting the current ident does not erase those aliases. A later assertion can repurpose an old name for another entity. Captured database values retain their own mapping. Prefer domain-specific unique attributes for ordinary application identities rather than growing the ident vocabulary with every record.

### Allowable Values

The allowable value of `:db/ident` is an EDN keyword (`Keyword` in Rust). It is idiomatic to namespace-qualify all idents you define. Namespaces can be hierarchical, with segments separated by ".", as in `:<namespace>.<nested-namespace>/<name>`.

Treat the `:db` namespace and all `:db.*` namespaces as reserved system vocabulary. Use application namespaces for your attributes. Built-in attribute definitions are protected against alteration; this is not a general parser prohibition on every application keyword beginning with `db`. Use built-in properties only for their documented purposes, including `:db/doc`, `:db/ident`, entity specs and virtual `:db/ensure`.

If using underscores in `:db/ident` values, do not use as the first character in the name portion of the keyword as this will prevent you from using reverse lookups.

## :db/isComponent

```
{':db/isComponent' boolean}
```

A component entity is one that exists only as part of a larger parent entity.

The optional `:db/isComponent` attribute specifies that an attribute whose [:db/valueType](#dbvaluetype) is [`:db.type/ref`](#dbvaluetype) refers to a sub-component of the entity to which the attribute is applied. When you retract an entity with `:db.fn/retractEntity`, all sub-components are also retracted, with each retraction attributed to the transaction's supplied source. Ordinary references do not establish this cascading ownership relationship. Nested component maps may create anonymous child entities; a nested map through a noncomponent reference requires its own explicit identity or a unique identity attribute. Components also affect default Pull expansion. See [Transactions](../04_transactions/00_transactions.md) and [Pull and entities](../05_query_and_pull/02_pull_and_entities.md).

Omitting `:db/isComponent` for an entity is semantically equivalent to setting it to `false`.

## :db/noHistory

```
{':db/noHistory' boolean}
```

### Description and Use Cases

By default, EDB maintains all historical values of an attribute. To disable this, set `:db/noHistory` to true. The purpose of `:db/noHistory` is to conserve storage, not to make semantic guarantees about removing information.

`:db/noHistory` is often used for high churn attributes along with attributes that you do not require a history of.

Changing `:db/noHistory` does not synchronously erase past values. Older facts may be omitted when retained indexes are built, but transaction logs, exact receipts, captured database values, backups and external copies have their own retention. Turning it off cannot reconstruct information already omitted. Use [excision](../08_operations/00_deployment.md#excision) for the separate controlled rewrite procedure; neither operation is a promise to erase independently held copies.

## :db/unique

```
{':db/unique' unique}
unique         = (':db.unique/identity' | ':db.unique/value')
```

The `:db/unique` attribute specifies a uniqueness constraint for the values of an attribute. EDB will reject a transaction if the resulting database would contain multiple entities with the same value for a unique attribute (of either type), regardless of source. Multiple sources may assert the same unique value for the same entity in separate transactions. You can submit transaction data with a unique attribute without specifying an existing entity id. When you do, one of two things could happen if that unique AV already exists in the database: unify or reject.

To add a uniqueness constraint to an attribute:

- The attribute must have a `:db/cardinality` of `:db.cardinality/one`
- If there are values present for that attribute, each value must belong to only one entity in the *current* database.
- Bytes attributes cannot be unique, and NaN cannot participate in uniqueness.
- If the attribute has historical values, AVET must already be physically ready before uniqueness is added. Setting `:db/index true` in the same transaction is not enough to backfill history.

Adding a unique constraint does not change history, therefore historical databases may contain non-unique values. Code that expects to find a unique value may find multiple values when querying against history.

### :db.unique/identity

Unique identity is specified through an attribute with `:db/unique` set to `:db.unique/identity`. Unique identity is appropriate whenever you want to assert a database-wide unique identifier for an entity. Common use cases include email addresses, account names, product codes/skus, and UUIDs. If transaction data includes a tempid + unique identity, and an entity with that identity already exists in the database, EDB will unify the new transaction data with the existing entity id. This enables "upsert", e.g. (entity 42 below stands for an actual issued ID, and `:person/age` is assumed installed):

```
{:db/ident       :person/email
 :db/unique      :db.unique/identity
 :db/valueType   :db.type/string
 :db/cardinality :db.cardinality/one}

;; existing in db (entity/attribute/value projection)
[42 :person/email "johndoe@example.com"]

;; transaction data in map form
{:db/id        "upsert-age"
 :person/email "johndoe@example.com"
 :person/age   50}

;; example of what map form might become after resolving tempid via unique identity
[:db/add 42 :person/age 50]
```

An entity can have multiple different unique attributes, however, this creates the possibility of encountering a conflict error. A transaction fails if it tries to upsert a tempid into two *different* existing entities. Using illustrative IDs, if entity 42 has the unique email `johndoe@example.com`, and entity 43 has the unique account number `1007`, then a transaction cannot claim that a new entity has both an email of `johndoe@example.com` and an account number of `1007`.

### :db.unique/value

Unique value is specified through an attribute with `:db/unique` set to `:db.unique/value`. This enforces distinct ownership without tempid upsert. If transaction data includes a tempid + unique value, and an entity with that value already exists, EDB will reject the transaction.

## :db/valueType

```
{':db/valueType' type}
type           = (':db.type/bigdec'  | ':db.type/bigint'  | ':db.type/boolean' |
                  ':db.type/bytes'   | ':db.type/double'  | ':db.type/float'   |
                  ':db.type/fn'      | ':db.type/instant' | ':db.type/keyword' | ':db.type/long'    |
                  ':db.type/ref'     | ':db.type/string'  | ':db.type/symbol'  |
                  ':db.type/tuple'   | ':db.type/uuid'    | ':db.type/uri')
```

The `:db/valueType` attribute specifies the type of value that can be associated with an attribute. The type is one of the keywords in the table below.

`:db/valueType` cannot be updated after an attribute is created.

| Value type | Description | Rust `Value` representation | EDN example |
| --- | --- | --- | --- |
| `:db.type/bigdec` | Arbitrary precision decimal | `BigDec(BigDecimal)` | `1.0M` |
| `:db.type/bigint` | Arbitrary precision integer | `BigInt(BigInt)` | `7N` |
| `:db.type/boolean` | Boolean | `Bool(bool)` | `true` |
| `:db.type/bytes` | Binary data | `Bytes(Vec<u8>)` | `#edb/bytes "010203"` |
| `:db.type/double` | 64-bit IEEE 754 floating point | `Double(f64)` | `1.0` |
| `:db.type/float` | 32-bit IEEE 754 floating point | `Float(f32)` | `#edb/float "1.0"` |
| `:db.type/fn` | Immutable stored-program content hash | `Function([u8; 32])` | `#edb/function "<64 hexadecimal digits>"` |
| `:db.type/instant` | Milliseconds since the Unix epoch | `Instant(i64)` | `#inst "2017-09-16T11:43:32.450Z"` |
| `:db.type/keyword` | Optional namespace and name | `Keyword(Keyword)` | `:yellow` |
| `:db.type/long` | Signed 64-bit integer | `Long(i64)` | `42` |
| `:db.type/ref` | Reference to another entity | `Ref(u64)` | issued ID, ident or lookup ref |
| `:db.type/string` | Unicode string | `String(String)` | `"Foo"` |
| `:db.type/symbol` | Optional namespace and name | `Symbol(Symbol)` | `app/validate` |
| `:db.type/tuple` | Tuple of scalar values | `Tuple(Vec<Option<Value>>)` | `[42 12 "foo"]` |
| `:db.type/uuid` | 128-bit universally unique identifier | `Uuid(u128)` | `#uuid "f40e770e-9ad5-11e7-abc4-cec278b6b50a"` |
| `:db.type/uri` | URI with component-based identity | `Uri(String)` | `#edb/uri "https://example.com/data"` |

The function hash above is a placeholder, not a deployable program. Deploy native program content before binding its hash to `:db/fn`; see [Persisted native queries](../04_transactions/01_persisted_programs.md).

### Notes on Value Types

- Value types are exact admission contracts. `42`, `42N`, `42.0` and `42M` have different stored types even when numeric comparison relates them. A Double does not satisfy a Float attribute.
- Instants store milliseconds since the epoch. `#edb/instant-millis` accepts a signed 64-bit integer directly.
- Ordinary strings have no 4096-character limit in schema validation. EDN conversion, transactions and storage still have their own resource limits; an accepted schema type is not a promise of unlimited value size.
- BigDecimals are limited to 1024 digits; BigIntegers to 8192 signed bits excluding the sign bit. At N bits the admitted interval is `[-2^N, 2^N - 1]`.
- Numeric comparison crosses supported numeric domains. Exact decimal comparisons do not require a uniform application scale; choose a consistent scale when the domain itself requires one.
- URI input retains its spelling while identity compares components. This is not general URL normalization. See [URI identity and exact input](01_identity_and_values.md#uri-identity-and-exact-input).
- Nonfinite floating-point values can be expressed with `#edb/float-bits` or `#edb/double-bits` and their hexadecimal IEEE bits. `#edb/float` accepts finite values and rejects overflow and nonzero underflow.
- EDN `nil`, characters, maps and sets are not stored scalar types. Collections can instead be transaction syntax or general query data. Tuple slots may be `nil`.

### Tuples

Tuples can be used to create multi-attribute unique keys on domain entities. Tuples can be used to optimize queries that otherwise would have to join two or more high-population attributes.

A tuple is a collection of 2-8 scalar values, represented in Rust as `Value::Tuple(Vec<Option<Value>>)` and in EDN as a vector. There are three kinds of tuples:

- [Composite tuples](#composite-tuples) are derived from other attributes of the same entity. Composite tuple types have a `:db/tupleAttrs` attribute, whose value is 2-8 keywords naming other attributes.
- [Heterogeneous fixed length tuples](#heterogeneous-tuples) have a `:db/tupleTypes` attribute, whose value is a vector of 2-8 scalar value types.
- [Homogeneous variable length tuples](#homogeneous-tuples) have a `:db/tupleType` attribute, whose value is a keyword naming a scalar value type.

The following types are considered scalar types suitable for use in a tuple:

```
:db.type/bigdec :db.type/bigint :db.type/boolean :db.type/double
:db.type/instant :db.type/keyword :db.type/long :db.type/string
:db.type/symbol :db.type/ref :db.type/uri :db.type/uuid
```

String values within a tuple are limited to 256 UTF-16 code units (128 supplementary Unicode characters such as emoji). Tuple BigIntegers are limited to 256 signed bits excluding the sign bit, and tuple BigDecimals to 256 digits. Float, bytes, function and tuple are not permitted slot types. Non-nil slots must exactly match their declared type; `nil` does not bypass tuple arity checks.

`nil` is a legal value for any slot in a tuple. This facilitates using tuples in range searches, where `nil` sorts lowest.

EDB includes the query helpers `tuple` and `untuple` for working with tuples in [queries](../05_query_and_pull/00_queries.md).

### Composite Tuples

Composite tuples are applicable in the following situations:

- When a domain entity has a multi-attribute key
- To optimize a query that joins more than one high-population attribute on the same entity

For example, consider the domain of course registrations, modeled with the following entity types:

- Courses represent a course, e.g. Algebra II
- Semesters represent a period in time when a course is run, e.g. "fall 2019"
- Students can take courses in particular semesters

```
[{:db/ident :student/first
  :db/valueType :db.type/string
  :db/cardinality :db.cardinality/one}
 {:db/ident :student/last
  :db/valueType :db.type/string
  :db/cardinality :db.cardinality/one}
 {:db/ident :student/email
  :db/valueType :db.type/string
  :db/cardinality :db.cardinality/one
  :db/unique :db.unique/identity}
 {:db/ident :semester/year
  :db/valueType :db.type/long
  :db/cardinality :db.cardinality/one}
 {:db/ident :semester/season
  :db/valueType :db.type/keyword
  :db/cardinality :db.cardinality/one}
 {:db/ident :semester/year+season
  :db/valueType :db.type/tuple
  :db/tupleAttrs [:semester/year :semester/season]
  :db/cardinality :db.cardinality/one
  :db/unique :db.unique/identity}
 {:db/ident :course/id
  :db/valueType :db.type/string
  :db/unique :db.unique/identity
  :db/cardinality :db.cardinality/one}
 {:db/ident :course/name
  :db/valueType :db.type/string
  :db/cardinality :db.cardinality/one}]
```

A *registration* entity is a unique combination of a student, semester, and course. In EDB schema:

```
{:db/ident :reg/course
 :db/valueType :db.type/ref
 :db/cardinality :db.cardinality/one}
{:db/ident :reg/semester
 :db/valueType :db.type/ref
 :db/cardinality :db.cardinality/one}
{:db/ident :reg/student
 :db/valueType :db.type/ref
 :db/cardinality :db.cardinality/one}
```

A given course/semester/student combination is unique in the database. To model this, you can create a composite tuple whose `:db/tupleAttrs` are:

```
{:db/ident :reg/course+semester+student
 :db/valueType :db.type/tuple
 :db/tupleAttrs [:reg/course :reg/semester :reg/student]
 :db/cardinality :db.cardinality/one
 :db/unique :db.unique/identity}
```

With this composite installed, EDB enforces unique ownership of each derived semester/course/student key. Updating an existing registration requires its entity ID, a lookup reference, another unique identity, or an explicit composite identity hint. Constituent-only assertions on a fresh tempid do not automatically select the existing registration; a duplicate derived key fails with `transaction/unique-conflict`.

Each active composite constituent must be an installed cardinality-one attribute of a permitted tuple scalar type. The composite attribute itself must have cardinality one. Constituents may be declared in the same schema transaction; validation sees the complete proposed schema.

Composite attributes are managed by EDB. Direct assertions and retractions do not set or clear the stored composite. An explicit assertion of a composite with `:db.unique/identity` can serve as an upsert hint during tempid resolution; the constituents still determine the stored tuple. A nonexistent hint without constituents does not populate either the composite or its constituents. See [Composite identity](01_identity_and_values.md#composite-identity-derive-values-supply-identity-when-upserting). Whenever you assert or retract any attribute that is part of a composite, EDB will automatically populate the composite value. Derived assertions and retractions carry the transaction's source. Contributions from several sources arrive in separate transactions and remain distinct while cardinality and uniqueness still apply to the tuple value.

Given a database with the courses and semesters schema, add some seed data:

```
[{:semester/year 2018
  :semester/season :fall}
 {:course/id "BIO-101"}
 {:student/first "John"
  :student/last "Doe"
  :student/email "johndoe@university.edu"}]
```

Now if you register John for Bio 101 in the fall of 2018 by transacting:

```
[{:reg/course [:course/id "BIO-101"]
  :reg/semester [:semester/year+season [2018 :fall]]
  :reg/student [:student/email "johndoe@university.edu"]}]
```

EDB will also add a composite tuple datom. Shown schematically as `[entity attribute value tx source added]`:

```
[registration-eid composite-aid [course-eid semester-eid student-eid] tx-eid 99 true]
```

Note that your entity IDs will differ from those in the example above.

If the current value of an entity does not include all attributes of a composite, the missing attributes will be nil. For example, given a composite 4-tuple `:reg/course+semester+student+grade` that also includes a student’s grade, the assertions above would cause EDB to populate:

```
[registration-eid composite-aid [course-eid semester-eid student-eid nil] tx-eid 99 true]
```

Note that nil sorts lower than all other values, so tuples with trailing nils can be useful for range queries.

If you retract all constituents of a composite, EDB will retract the composite. For example, transacting:

```
[[:db/retract 20736789299855447 :reg/course [:course/id "BIO-101"]]
 [:db/retract 20736789299855447 :reg/semester [:semester/year+season [2018 :fall]]]
 [:db/retract 20736789299855447 :reg/student [:student/email "johndoe@university.edu"]]]
```

will cause EDB to retract the composite:

```
[registration-eid composite-aid [course-eid semester-eid student-eid] next-tx-eid 99 false]
```

Again, note that you will need to substitute the entity IDs from your initial transaction to replicate this example in your system.

### Adding Composites to Existing Entities

Adding a composite tuple to a database that contains existing data using those attributes will **not** immediately generate values for the new tuple. The composite tuple will be populated the next time any of the composite member attributes are transacted. This includes otherwise redundant reassertions of the same attribute value from the same source. This design allows you to add composite tuples in a systematic and paced manner, so as not to overwhelm a running system.

To populate existing data, read current entity/value pairs for a constituent and reassert them in bounded transactions, each with an explicit source, after installing the composite. Track progress explicitly and account for uniqueness collisions, including partial keys with nil slots. Adding the schema alone does not certify that every old entity already has its new composite key.

### Discontinuing Composite Tuples

You discontinue a composite tuple attribute by adding `:db.tuple/discontinued` `true` to it:

```
[:db/add :semester/year+season :db.tuple/discontinued true]
```

When you discontinue a composite tuple attribute, EDB permanently stops generating datoms when any of its constituents are transacted. Existing composite tuple datoms remain in the database.

`:db.tuple/discontinued` only applies to installed composite tuple attributes. It cannot be set to true in the transaction that installs the composite. Discontinuing a composite tuple attribute is permanent. Create a new composite tuple attribute if you later decide you need the discontinued one.

### Heterogeneous Tuples

Heterogeneous tuples have a `:db/tupleTypes` attribute, with a value specified as a vector of 2-8 scalar types.

For example, you could model a location in a 2D game with the following tuple attribute:

```
[{:db/ident       :player/handle
  :db/valueType   :db.type/string
  :db/cardinality :db.cardinality/one
  :db/unique      :db.unique/identity}
 {:db/ident       :player/location
  :db/valueType   :db.type/tuple
  :db/tupleTypes  [:db.type/long :db.type/long]
  :db/cardinality :db.cardinality/one}]
```

You can then explicitly assert a player's location with a vector of the appropriate tuple types:

```
[{:player/handle   "Argent Adept"
  :player/location [100 0]}]
```

### Homogeneous Tuples

Homogeneous tuples provide variable-length composites of a single attribute type. The type of a homogeneous tuple is specified by the keyword attribute `:db/tupleType`.

EDB itself includes a good example of homogeneous tuples in the definitions of the other tuple types. Both `:db/tupleTypes` and `:db/tupleAttrs` are declared as homogeneous tuples of `:db/tupleType` `:db.type/keyword`. These built-in definitions are attributed to `:db.source/system`:

```
;; built in to EDB
[{:db/ident       :db/tupleAttrs
  :db/valueType   :db.type/tuple
  :db/tupleType   :db.type/keyword
  :db/cardinality :db.cardinality/one}
 {:db/ident       :db/tupleTypes
  :db/valueType   :db.type/tuple
  :db/tupleType   :db.type/keyword
  :db/cardinality :db.cardinality/one}]
```

## Attribute Predicates

You may want to constrain an attribute value by more than just its value type. For example, an email address is not just a string, but a string with a particular format. In EDB, you can assert *attribute predicates* about an attribute. Attribute predicates are asserted via the `:db.attr/preds` attribute, and are fully qualified symbols naming a predicate of a value. Predicates return literal `true` to indicate success. Every other return value indicates failure and is reported as a transaction error.

Inside a transaction, EDB calls the applicable attribute predicates for values in added transaction datoms and aborts if a predicate fails. Retractions are not value assertions. Redundant reassertions from the same source do not necessarily produce added datoms, so do not use a no-op assertion as a retroactive validation procedure.

For example, the following Rust deployment validates that a user name contains 3–15 Unicode scalar values:

```rust
use edb_core::{NativeRegistry, RuntimeValue, SemanticError, Symbol, Value};

fn name_predicates() -> Result<NativeRegistry, SemanticError> {
    let mut builder = NativeRegistry::builder();
    builder.attribute_predicate(
        Symbol::new("app.people", "user-name?"),
        |value, context| {
            context.check(1)?;
            let valid = match value {
                Value::String(name) => {
                    let length = name.chars().take(16).count();
                    context.check(length as u64)?;
                    (3..=15).contains(&length)
                }
                _ => false,
            };
            Ok(RuntimeValue::Scalar(Value::Bool(valid)))
        },
    )?;
    Ok(builder.build())
}
```

Supply the registry in `TransactionExecutionOptions` to the executing service or speculative API. Registering it in an unrelated client process does not deploy it to the writer. The stock service has an empty native registry. See [Native application computation](../07_peer_api/02_native_computation.md) for service startup, stored-program bindings, optional native aliases and deployment versioning.

To install the predicate, add a `:db.attr/preds` value to an attribute:

```edn
[{:db/ident :user/name
  :db/valueType :db.type/string
  :db/cardinality :db.cardinality/one
  :db.attr/preds app.people/user-name?}]
```

Symbols in EDN are data and need no Clojure quoting. A collection can install several predicates. Each must be available in the correct predicate role when a transaction needs it.

A later transaction containing `{:user/name "This-name-is-too-long"}` fails with `transaction/attribute-predicate` in the Incorrect category. Its details include `entity`, `attribute`, `value`, `predicate` and `pred_return`. These are EDB error details; they are not a Clojure exception map. Literal false, nil or a diagnostic runtime value can reject; a predicate may also return a `SemanticError` directly to cancel assessment. No part of the failed transaction commits.

Attribute predicates can be asserted or retracted at any time. Definitions come from db-before, so a change takes effect on the following transaction. Adding a predicate does not validate values already present. Removing a predicate in the same transaction as new values does not bypass the old predicate. To audit old values, read them and explicitly evaluate the intended rule, or use an appropriate entity-level validation transaction.

Native predicate implementations must be deployed to every writer, standby or speculative process that will execute them. Missing code is an error, not a skipped check. Keep callbacks pure and use `NativeCallContext` to debit work and observe cancellation. Stored programs are a separate durable implementation option; schema symbols do not load arbitrary source code or a JVM classpath.

## Entity Specs

You may want to ensure properties of an entity being asserted, for example:

- Required keys.
- Presence of a derived tuple in the resulting entity.
- Satisfaction of properties that cut across attributes and the database.

An *entity spec* is an EDB entity having one or more of:

- Usually, a `:db/ident`.
- `:db.entity/attrs` naming required attributes.
- `:db.entity/preds` naming entity predicates.

You can then ensure an entity spec by asserting the `:db/ensure` attribute for an entity. For example, the following transaction ensures entity `"new-account-1"` with entity spec `:new-account`:

```edn
[{:db/id "new-account-1"
  :db/ensure :new-account
  ;; other data that must be a valid :new-account
  }]
```

`:db/ensure` is a virtual attribute. It is not added to the database; instead, it requests checks based on the named spec. It is cardinality-many: a map may request several specs with `:db/ensure [:account/required :account/balanced]`. It cannot be retracted or compared and swapped.

Entity specs can be changed at any time. Their definitions are read from db-before; checks inspect the complete proposed db-after. Install a new named spec in an earlier transaction before using its ident. Changing a spec has no retroactive effect and does not attach an ongoing constraint to previously ensured entities.

Entity specs and `:db/ensure` are not table-wide SQL constraints:

- Specs enforce shapes on particular entities without imposing one overall record structure on the database.
- Entity predicates can examine the proposed entity and other facts in the proposed database.
- Enforcement must be requested explicitly in each transaction through `:db/ensure`; it is never automatic or retroactive.

Required attributes are checked before predicate execution. Predicate checks cannot fill in missing keys or create extra transaction data. A transaction function may generate facts before assessment, and composite derivation may supply managed tuple facts, but the spec itself only accepts or rejects the result.

### Required Attributes

The `:db.entity/attrs` attribute is a multi-valued attribute of keywords, where each keyword names a required attribute.

For example, after installing `:user/name` and `:user/email`, this transaction creates a spec requiring both:

```edn
[{:db/ident :user/validate
  :db.entity/attrs [:user/name :user/email]}]
```

The `:user/validate` entity can then be used in a later transaction to ensure all required attributes are present. This transaction fails for its new entity:

```edn
[{:user/name "John Doe"
  :db/ensure :user/validate}]
```

When a required attribute is missing, EDB returns `transaction/entity-spec` in the Incorrect category. The message identifies the entity, missing attribute IDs and spec ID. Unknown required attribute names fail with `transaction/unknown-spec-attribute` when the spec is resolved.

Presence means at least one current resulting value. An existing value can satisfy the requirement without being reasserted. A retraction in the same transaction can make a requirement fail. Cardinality-many attributes need at least one remaining value. The required-key check does not impose additional content rules such as nonempty strings; use a predicate for those.

### Entity Predicates

The `:db.entity/preds` attribute is a multi-valued attribute of symbols. Each symbol names a predicate of database value and entity ID. EDB calls all requested predicates and aborts the transaction if any result is not literal `true`.

For example, deploy a predicate that reads `:score/low` and `:score/high` from its supplied database and accepts only when low is less than or equal to high. A local speculative callback can be registered as follows:

```rust
use edb_core::{Keyword, SemanticError, TxFunctions, Value};

fn score_predicates() -> TxFunctions {
    let mut functions = TxFunctions::new();
    functions.register_entity_predicate("app.scores/ordered?", |db_after, entity| {
        let attribute = |name| {
            db_after.schema().resolve_ident(&Keyword::new("score", name))
                .ok_or_else(|| SemanticError::incorrect(
                    "app/missing-schema", "score attribute is not installed"))
        };
        let low = db_after.values(entity, attribute("low")?)?;
        let high = db_after.values(entity, attribute("high")?)?;
        Ok(matches!((low.as_slice(), high.as_slice()),
            ([Value::Long(low)], [Value::Long(high)]) if low <= high))
    });
    functions
}
```

Supply these callbacks to the local `Database::with_forms` call (or use `DatabaseValue::with_functions` for primitive operations). For a running writer, register the same check with `NativeRegistryBuilder::entity_predicate` and its cooperative context instead. Both receive the complete proposed db-after, while the entity spec and deployment binding are resolved using db-before.

After installing the score attributes as cardinality-one Long attributes, install the guard:

```edn
[{:db/ident :score/guard
  :db.entity/attrs [:score/low :score/high]
  :db.entity/preds app.scores/ordered?}]
```

Given an invalid entity that requests `:score/guard`:

```edn
[{:score/low 100
  :score/high 20
  :db/ensure :score/guard}]
```

EDB returns `transaction/entity-predicate` in the Incorrect category. The message identifies the entity, predicate and spec, and `pred_return` records the rejected return value. A runtime value richer than false may be used to explain the failure. Returning a `SemanticError` directly also cancels the transaction. A panic is contained as a predicate fault; missing implementations fail explicitly.

### Number of Schema Elements

EDB restricts installed attribute entities to the database partition and an entity index at most `MAX_SCHEMA_ATTRIBUTE_ID`, currently 1,048,576 (`2^20`, inclusive). IDs outside that partition or range fail with `schema/attribute-outside-db-partition` or `schema/attribute-id-out-of-range`. This is an attribute-ID bound, not a promise that exactly one million user attributes can be installed: the system vocabulary and allocation rules share the space.

### Schema Element Restrictions

Use `:db` and `:db.*` vocabulary only for documented system purposes; namespace application attributes separately. Built-in attribute definitions cannot be altered. Installed attributes cannot be removed by retracting their definition. Attribute installation and alteration are derived from ordinary schema facts; callers normally do not need explicit install/alter markers.

Schema entities are open and may carry application-defined metadata such as `:app.schema/owner`, provided that metadata attribute was itself installed before use. `:db/doc` is ordinary optional string documentation, not a validation predicate.

### Limitations of NaN

NaN cannot be used as a unique identity or unique value. Such assertions fail with `transaction/nan-cannot-identify`.

On a nonunique cardinality-one Float or Double attribute, a finite value can replace NaN in one ordinary transaction. The result contains the old-value retraction and new assertion, just like other replacements. EDB gives NaN stable logical equality for indexing and redundancy: another NaN payload can retract an existing NaN, and reasserting a logically equal NaN from the same source is redundant. An assertion from a different source in a later transaction preserves that separate contribution. This does not promise IEEE `==` semantics or preservation of arbitrary NaN payloads. See [NaN rules](01_identity_and_values.md#nan-replace-ordinary-values-never-use-as-a-unique-identity).

## Schema Evolution

See [Changing Schema](03_changing_schema.md) for migration examples.

Schema changes are transactions, but their interpretation is split deliberately: identifiers, input attributes, predicate definitions and entity specs resolve from db-before; transition validation checks the complete proposed schema and resulting facts.

| Property | Change after installation |
| --- | --- |
| `:db/ident` | Rename is supported; old names remain aliases until repurposed. |
| `:db/doc` and application metadata | Ordinary attribute transactions. |
| `:db/valueType` | Immutable; create a new attribute for another type. |
| `:db/cardinality` | Many-to-one requires at most one resulting value per entity. One-to-many must remain compatible with uniqueness and active composite definitions. |
| `:db/unique` | Must remain cardinality-one and non-bytes. Adding uniqueness validates resulting current values and requires ready AVET if history exists. |
| `:db/index` | Mutable intent; newly required physical index work can lag. |
| `:db/fulltext` | Immutable, including false-to-true changes. |
| `:db/isComponent` | Mutable, but true requires a ref attribute; use care when changing future cascade behavior. |
| `:db/noHistory` | Mutable retention policy; cannot restore omitted history. |
| `:db/tupleType`, `:db/tupleTypes`, `:db/tupleAttrs` | Installed tuple definitions are immutable. |
| `:db.tuple/discontinued` | False-to-true only, on an already installed composite. |
| `:db.attr/preds`, entity spec facts | Mutable; db-before definitions govern the current transaction. |
| Attribute installation | An installed attribute cannot be removed. |

A many-to-one change may retract excess values in the same transaction so that the resulting data satisfies the new cardinality. A type migration requires a new attribute, a controlled copy/conversion, and application cutover; changing `:db/valueType` in place is rejected even if the attribute currently has no data. Adding uniqueness does not rewrite old historical views.

Common definition errors include `schema/unique-must-be-cardinality-one`, `schema/bytes-cannot-be-unique`, `schema/component-must-be-ref`, `schema/fulltext-must-be-string`, `schema/missing-tuple-spec`, `schema/invalid-tuple-element-type`, and `schema/invalid-tuple-attributes`. Transition errors include `schema/value-type-immutable`, `schema/tuple-definition-immutable`, `schema/fulltext-immutable`, `schema/unique-requires-avet` and `schema/tuple-discontinuation-irreversible`. These are semantic failures of the transaction, not partial schema updates.

## Bytes

The `:db.type/bytes` type stores a byte vector. `#edb/bytes` uses a hexadecimal string; for example, `#edb/bytes "010203"` represents three bytes. EDB compares byte contents for value identity and gives them deterministic ordering. The type is not deprecated by EDB.

Bytes attributes cannot be unique and therefore cannot be lookup-reference keys. Bytes also cannot be tuple slots. These are explicit schema restrictions, not a consequence of Java array identity. Use a separate string, UUID or other permitted unique key to identify an entity carrying binary data. Ordinary EDN conversion and transaction/storage budgets still apply to binary payloads.
