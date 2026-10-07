# Data Modeling

## Use :db/ident For Enumerations

Many databases need to represent a set of enumerated values. In EDB, it is
idiomatic to represent enumerated values as entities with a `:db/ident` attribute.

The examples attribute their facts to an existing contributor entity with ID
`99`. The `"edb.tx"` map attributes the generated transaction time.

For example, the transaction below defines an `:artist/country` attribute, and
two initial enumerated values, `:country/CA` and `:country/JP`, ISO alpha-2 country
codes for Canada and Japan. It also defines `:artist/name` for the example that
follows:

```edn
[{:db/id "edb.tx" :db/source 99}
 {:db/ident :artist/name
  :db/source 99
  :db/valueType :db.type/string
  :db/cardinality :db.cardinality/one}
 {:db/ident :artist/country
  :db/source 99
  :db/valueType :db.type/ref
  :db/cardinality :db.cardinality/one
  :db/doc "An artist's country of residence"}
 {:db/ident :country/CA :db/source 99}
 {:db/ident :country/JP :db/source 99}]
```

The use of `:db/ident` on the enumerated value entities makes it possible to
refer to the entities using their identity keywords, `:country/CA` and
`:country/JP`.

The transaction below creates an artist named *Leonard Cohen* with the country
`:country/CA`. Submit it after the schema transaction:

```edn
[{:db/id "edb.tx" :db/source 99}
 {:db/source 99
  :artist/name "Leonard Cohen"
  :artist/country :country/CA}]
```

The `:artist/country` attribute is of type `:db.type/ref`, so its value must be a
reference to another entity. The `:country/CA` keyword can be used as a value
because it is the identity of an entity, as specified by `:db/ident` in the
previous transaction.

Attempting to use identity keywords that have not been transacted as referenced
identities will result in an error. In EDB, an unknown choice such as
`:country/ZZ` produces `transaction/unknown-ident` and prevents the transaction
from committing.

## Installation and resolution

The first two domain maps define attributes. The last two create ordinary
identified entities, not additional attributes: they have no `:db/valueType` or
`:db/cardinality`.

EDN reference idents resolve against db-before. Merely spelling a keyword does
not create the referenced entity. To introduce a new choice, transact its ident
before referring to that ident by name. New attribute names must also exist
before EDN application maps can use them.

The [EDN workflow](../01_tutorials/00_edn_workflow.md) explains how to submit these
transaction vectors with the CLI or preview them without committing. See
[schema basics](00_schema.md) for attribute installation and the
[schema reference](02_schema_reference.md) for complete property definitions.

## Query and navigate enumeration references

With a database source, this query finds artists whose country is Canada:

```edn
[:find ?name
 :where
 [?artist :artist/name ?name]
 [?artist :artist/country :country/CA]]
```

The known reference attribute tells the query engine to resolve the keyword as
an entity ident. To return the vocabulary name alongside each artist, join to
the enumeration entity's `:db/ident`:

```edn
[:find ?name ?country
 :where
 [?artist :artist/name ?name]
 [?artist :artist/country ?country-entity]
 [?country-entity :db/ident ?country]]
```

A Pull pattern can select the same name explicitly:

```edn
[:artist/name {:artist/country [:db/ident]}]
```

This produces a nested map for the country, containing its ident. A plain
`:artist/country` Pull selector returns a reference ID map instead. Lazy entity
navigation has a different convenience: reading a forward reference to an
identified entity returns its keyword. See
[Pull and entity navigation](../05_query_and_pull/02_pull_and_entities.md) for
these result shapes and [query sources](../05_query_and_pull/00_queries.md) for
the distinction between schema-aware database queries and literal raw tuples.

## Choose the representation deliberately

Use a reference when a choice should have an entity identity or associated
facts. For example, a country entity can later carry an application-defined
display label or regional grouping. Install those attributes normally, then
address the existing entity with `:db/id :country/CA` when adding its facts.

A keyword-typed attribute stores a keyword directly instead. It does not
require a corresponding ident entity, but it also does not provide reference
navigation to that entity. A string may suit unrestricted user input; it does
not by itself define a shared vocabulary.

Reference type alone does not restrict an attribute to the `country` vocabulary.
An ident from another vocabulary is still a resolvable entity reference. The
namespace is a naming convention, not a membership constraint. When enforcing
membership matters, add an explicit transaction validation rule; consult
[native predicates](../07_peer_api/02_native_computation.md) and the predicate
and entity-spec sections of the [schema reference](02_schema_reference.md).

Keep shared enumeration references noncomponent, as in the example. Component
references express ownership and participate in recursive entity retraction;
an artist does not own a shared country entity. Cardinality-one permits at most
one current country value, but does not require that an artist have one. Use a
cardinality-many reference when the domain permits a set of choices, and supply
a collection such as `[:country/CA :country/JP]` in the application map.

## Evolve the vocabulary

Adding a new identified choice is an ordinary data transaction. Changing an
entity's ident changes its name, not the target ID of existing references;
applications using the previous name need to be updated. Captured historical
database values retain their own view of the facts. See
[changing schema](03_changing_schema.md) for ident changes and
[value and identity rules](01_identity_and_values.md) for application identity.

Prefer a domain-specific unique identity attribute for ordinary business
entities such as artists or accounts. Reserve enumeration idents for vocabulary
names that application code intentionally shares. An artist name is not
automatically unique, and the example's `:artist/name` declaration does not make
it an identity key.
