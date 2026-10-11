# EDB Data Model

An EDB database is a set of immutable atomic facts called *datoms*. Its application data model has no tables; instead it has a universal schema of user-defined attributes, in which any entity can possess any attribute.

EDB's Datalog queries automatically use multiple indexes to support a variety of access patterns as well as a chronological time model, identity and uniqueness, and lookup refs. Historical reads depend on retained data, as described below.

See below for explanations of these terms.

## Database

In EDB, a *database value* is a set of *datoms* and is often abbreviated as *database* or *db*. A db is a point-in-time, immutable value and will never change. If you use the same db for several queries, you will know the answers are based upon exactly the same data from a single point in time.

EDB is a database of record: contributors maintain one shared current record, with earlier assertions and changes retained in history.

The Rust read API represents this with `DatabaseValue`. A connection can acquire newer values as transactions commit; an already captured value does not refresh itself. Deriving an as-of or history view creates another value. See [composing immutable reads](00_database_values.md) for captured-value retention, entity identity, and physical index readiness.

## Datoms

A *datom* is an immutable atomic fact that represents the addition or retraction of a relation between an entity, an attribute, a value, a transaction, and a source. A datom is expressed as a six-tuple, `[e a v t s added?]`:

- an entity id (E)
- an attribute (A)
- a value for the attribute (V)
- a transaction id (Tx)
- a source entity id (S)
- a boolean (Op) indicating whether the datom is being added or retracted

The application explicitly supplies one source for each transaction, including transactions with empty data. Every assertion or retraction it produces carries that source, including function results, generated changes, and the automatic transaction timestamp. A source is an ordinary entity that can represent a person, institution, publication, LLM, or another contributor of database statements. Its description uses the same installed attributes as other entities, with its own history and attribution. Different sources contribute to the same record through separate transactions. Canonical genesis attributes its vocabulary and schema facts to the ordinary entity `:db.source/system`; internal bootstrap transactions use that source as well.

A source can be established and used in the same transaction: describe it with
a tempid in entity position and supply that tempid as the transaction's source.
Built-in `:db/ident` and `:db/doc` let an application bootstrap its first
contributor before installing custom schema. Later transactions can use the
source's persistent ident. See [bootstrapping an application source](../01_tutorials/00_edn_workflow.md#bootstrap-an-application-source).

### Example Datom

| **E** | 42 |
| --- | --- |
| **A** | :user/favorite-color |
| **V** | :blue |
| **Tx** | 1234 |
| **S** | 1001 |
| **Op** | true |

The tables in this document use small illustrative entity and transaction labels. Actual transaction entity IDs are encoded IDs: `t_to_tx(t)` converts logical transaction time T to an ID, and `tx_to_t(tx)` converts it back. Use allocated entity IDs in application transactions. Attributes are shown by their keyword names; the Rust `Datom` stores `entity: u64`, `attribute: u32`, `value: Value`, `tx: u64`, `source: u64`, and `added: bool`.

## Entities

An *entity* is a set of *datoms* that are all about the same E.

### Example Entity

An entity can be visualized as a table:

| E | A | V | Tx | S | Op |
| --- | --- | --- | --- | --- | --- |
| 42 | :user/favorite-color | :blue | 1234 | 1001 | true |
| 42 | :user/first-name | "John" | 1234 | 1001 | true |
| 42 | :user/last-name | "Doe" | 1234 | 1001 | true |
| 42 | :user/favorite-color | :blue | 2345 | 1002 | true |
| 42 | :user/favorite-color | :green | 4567 | 1002 | true |
| 42 | :user/favorite-color | :blue | 4567 | 1002 | false |

Here `:user/favorite-color` has cardinality one: asserting a different color retracts the previous color. The two datoms at transaction 4567 belong to one atomic change, not two independently visible updates.

The two assertions of `:blue` carry different sources and both remain in history. The second assertion adds source information even though the current value is still `:blue`; cardinality counts distinct values, so cardinality-one still holds. The retraction at 4567 withdraws both current contributions of `:blue`. If source 1001 is Julia and source 1002 is Jake, the retraction is attributed to Jake while Julia's earlier assertion retains her attribution.

### Point-In-Time Entity Example

A point-in-time (as-of) view of an entity considers the assertions that still hold as of a certain transaction. Earlier assertions and retractions remain available in history with their sources. In the example above, John no longer prefers `:blue` as of 4567, so the point-in-time view as of 4567 is:

| E | A | V | Tx | S | Op |
| --- | --- | --- | --- | --- | --- |
| 42 | :user/first-name | "John" | 1234 | 1001 | true |
| 42 | :user/last-name | "Doe" | 1234 | 1001 | true |
| 42 | :user/favorite-color | :green | 4567 | 1002 | true |

`DatabaseValue::as_of(t)` takes logical T and has an inclusive upper boundary. `history()` exposes retained assertions and retractions rather than collapsing them into current facts. A historical view cannot reconstruct facts already omitted by retention or excision.

### Map View Example

It is often convenient to consider a point-in-time view as only a three-tuple with *Tx*, *S*, and *Op* elided:

| E | A | V |
| --- | --- | --- |
| 42 | :user/first-name | "John" |
| 42 | :user/last-name | "Doe" |
| 42 | :user/favorite-color | :green |

This three-tuple view is very similar to a programming language object where the *E* is analogous to *this* or *self*. The *map view* of an entity at a particular point in time captures this information more compactly, using the reserved pseudo-attribute name `:db/id` for *E*:

```edn
{:db/id 42
 :user/favorite-color :green
 :user/first-name "John"
 :user/last-name "Doe"}
```

`:db/id` identifies the entity; it is not another stored application attribute. [Pull](../05_query_and_pull/06_pull_reference.md) projects entities into maps, while the [entity API](../05_query_and_pull/02_pull_and_entities.md) provides lazy navigation. A map used as transaction input asserts its supplied attributes; omitting an attribute does not retract it. The enclosing transaction supplies the source for those assertions.

## Universal Schema

In a relational database, you must specify a table schema that enumerates in advance the attributes (columns) an entity can have. By contrast, EDB requires only that you specify the properties of individual attributes. Any entity can then have any attribute. Because all datoms are part of a single relation, this is called a *universal schema*.

For example, consider storing an inventory database in EDB. All inventory items have a unique string identifier, so you create an `:inv/id` attribute. In addition, you create other named attributes, specifying the [types and cardinalities](../03_schema/02_schema_reference.md) of each.

You can then store various inventory items in the database, each with different attributes, as shown in the following table.

| E | A | V |
| --- | --- | --- |
| 7 | :inv/id | "SKU-1234" |
| 7 | :inv/color | :green |
| 8 | :inv/id | "SKU-5678" |
| 8 | :inv/watts | 60000 |
| 8 | :doc/url | "http:…" |

Notice that, other than `:inv/id`, entities 7 and 8 have entirely disjoint attributes. The abbreviated URL is illustrative string data. EDB's PostgreSQL storage tables are an implementation detail, not the schema of these inventory entities.

## Defining Schema

Each EDB database has a schema that describes the set and kind of attributes that can be associated with your domain entities.

A schema only defines the characteristics of the attributes themselves. It does not define which attributes can be associated with which entities. Decisions about which attributes apply to which entities are made by your application.

This gives applications a great degree of freedom to evolve over time. For example, an application that wants to model a person as an entity does not have to decide up front whether the person is an employee or a customer. It can associate a combination of attributes describing customers and attributes describing employees with the same entity. An application can determine whether an entity represents a particular abstraction, customer or employee, simply by looking for the presence of the appropriate attributes.

There are two kinds of attributes in EDB:

- **Domain attributes** describe aspects of your domain data. You use domain attributes to describe the data about your domain entities.
- **Schema attributes** describe aspects of the schema itself. EDB's built-in schema vocabulary supplies the supported schema semantics. You use schema attributes to define your domain attributes; custom metadata can be added to schema entities without introducing new built-in schema behavior.

Schema entities are ordinary database data and are installed through transactions. Submit the transaction excerpt below with source entity `99`, assumed to already exist, through the request or CLI. EDB automatically records the transaction instant with that same source. For example, install this schema before submitting facts that use the attribute:

```edn
[{:db/ident :inv/id
  :db/valueType :db.type/string
  :db/cardinality :db.cardinality/one
  :db/unique :db.unique/identity}]
```

Install other attributes in the same way. Attribute names in a transaction resolve against db-before, so submit application facts using newly installed attributes in a later transaction. Applications can also define entity specs and request their checks explicitly with `:db/ensure`; these do not turn every entity into a closed record type.

For more information see the [schema documentation](../03_schema/02_schema_reference.md), [changing schema](../03_schema/03_changing_schema.md), and [transaction data](../04_transactions/04_transaction_data.md).

## Time Model

EDB's time model is applied automatically as committed transactions are recorded, ensuring that the following properties hold.

- Every committed transaction is assigned a transaction id that is unique within the database.
- Committed transactions are serialized, and transaction ids increase over time, so they provide a total ordering of the database's committed transactions. Logical T is the corresponding sequential time coordinate.
- Every datom in the database includes the id of the transaction that asserted or retracted that datom.
- Transaction ids are themselves entity ids, so transactions can have datoms that describe them. EDB automatically records one `:db/txInstant` assertion for every transaction after genesis, so datoms can be joined to the recorded wall-clock instant of their transaction. That assertion carries the transaction's source.
- Transactions can have other attributes. For example, a system might record why a transaction corrects an earlier one. Every assertion or retraction, including transaction metadata, carries the source supplied once for the transaction.

For example, imagine setting the count of an item in an inventory database in one transaction, then updating that count later. In addition to recording the item counts and their sources, EDB also records the transactions that made the changes, when the changes happened, plus any additional transaction metadata you choose to include. This might be used to, for example, record that a particular transaction corrects another transaction that was made in error.

### Time Model Example

The example below shows that *Tx* acts as a global ordering of transactions within a database. It also shows using a custom attribute to add audit information to a transaction: `:correction/for` shows that *Tx* 1235 is fixing a mistake recorded in *Tx* 1234.

| E | A | V | Tx | S | Op |
| --- | --- | --- | --- | --- | --- |
| 42 | :election/winner | "Dewey" | 1234 | 1001 | true |
| 42 | :election/winner | "Dewey" | 1235 | 1002 | false |
| 42 | :election/winner | "Truman" | 1235 | 1002 | true |
| 1235 | :correction/for | 1234 | 1235 | 1002 | true |

Define `:correction/for` as a reference attribute and use the special transaction tempid `"edb.tx"` to attach such metadata to the current transaction. The reference value must identify the actual earlier transaction entity, not the illustrative label in this table.

EDB's time model allows recovery of data "as of" a point in retained history, and it also allows queries that look at differences between two or more points in time. `since(t)` uses an exclusive lower boundary; combining it with a history value includes retained assertions and retractions after T. Queries can join multiple named database values or a captured transaction log.

Transaction instants are nondecreasing and need not be distinct; T and transaction IDs provide the total order. Speculative `with_edn` results are immutable candidate databases, not committed transactions in that order. For more information, see [transactions](../04_transactions/00_transactions.md), [indexes and the transaction log](../06_indexes/00_indexes_and_log.md), and [query sources](../05_query_and_pull/00_queries.md).

## Identity and Uniqueness

EDB provides a number of tools for dealing with identity and uniqueness.

- When you create an entity through a tempid or an implicit map ID, EDB allocates a database-unique [entity id](../03_schema/05_identity_and_uniqueness.md). All datoms about that entity share its ID; a new datom does not require a new entity ID.
- The built-in `:db/ident` attribute can be used for programmatic names.
- The built-in `:db/unique` attribute declares uniqueness. Adding or changing uniqueness on an existing attribute must satisfy EDB's [schema change rules](../03_schema/03_changing_schema.md), including checks on existing data and index readiness. `:db.unique/identity` allows transactions to work with domain notions of identity instead of entity ids through tempid upsert, such as a primary contact email or SKU number. `:db.unique/value` enforces a single holder of an identifying key without upsert.
- The `:db.type/uuid` value type stores globally unique identifiers as 128-bit UUID values. Declare uniqueness separately if EDB must enforce one entity per UUID.
- Lookup refs represent a lookup on a unique attribute as an attribute-value pair.

An *entity identifier* is any one of the three ways that EDB can uniquely identify an entity:

- an entity id (`:db/id` in map notation)
- an ident (`:db/ident`)
- a lookup ref.

Rust APIs taking `EntityIdentifier` accept `Id`, `Ident`, or `Lookup`. APIs taking a numeric entity or attribute ID require that numeric ID; resolve other identifiers first. See [identity and uniqueness](../03_schema/05_identity_and_uniqueness.md) for the details.

## Lookup Refs

In many databases, entities have unique identifiers in the problem domain like an email address or an order number. Applications often need to find entities based on these external keys. You can do this with query, but it's easier to use a *lookup ref*. A lookup ref is a pair containing an attribute identifier and a value. It identifies the entity with the given unique attribute value. For example, this lookup ref:

```edn
[:person/email "joe@example.com"]
```

identifies the entity with the `:person/email` value "joe@example.com".

You can use lookup refs to retrieve entities using the *pull* and *entity* APIs. For raw *datoms* access, resolve the lookup ref to an entity ID and construct the numeric index prefix. Database query patterns also support lookup refs in the entity position:

```edn
[:find ?name
 :where [[:person/email "joe@example.com"] :person/name ?name]]
```

You can also use lookup refs to refer to existing entities in transactions, avoiding extra lookup code. Submit this example with source entity `99` through the request or CLI:

```edn
[{:db/id [:person/email "joe@example.com"]
  :person/loves :pizza}]
```

This transaction asserts that the entity with the value "joe@example.com" for the `:person/email` attribute also loves pizza. Install `:person/loves` as a keyword-valued attribute for this example, and first create the entity's email fact.

Lookup refs have the following restrictions:

- The specified attribute must be defined as either `:db.unique/value` or `:db.unique/identity`.
- When used in a transaction, the lookup ref is evaluated against the database as it exists before the transaction is processed, so you cannot use a lookup ref to look up an entity being defined in the same transaction. Use a shared tempid for that case.
- Database query entity positions accept literal lookup refs and lookup refs supplied through inputs to a [parameterized query](../05_query_and_pull/05_query_reference.md). Query reference-value positions do not resolve lookup-ref vectors; join through the entity position instead.
- Read lookups use the supplied database value. Ordinary unique lookups require a ready AVET index; schema intent alone does not guarantee index readiness.

## Chronological Change and Retention

EDB represents ordinary change chronologically. Information accumulates over time, and change is represented by accumulating the new, not by modifying the old datom. For example, ordinary "removing" occurs not by taking a fact away, but by adding a retraction.

This differs from the Create/Read/Update/Delete (CRUD) paradigm:

| CRUD | EDB |
| --- | --- |
| Create | Assert |
| Read | Read |
| Update | Accumulate |
| Delete | Retract |

- *Assertions* are granular statements of fact.
- *Reads* are performed against an immutable database value at a particular point in time. Committed time is globally ordered within a database by ACID transactions.
- Ordinary new transactions *accumulate* changes. Existing datoms never change in place.
- *Retractions* state that an assertion no longer holds at some later point in time. A retraction carries its transaction's source; the original assertion and its source remain unchanged while retained.

Assert/Read/Accumulate/Retract (ARAR) can be pronounced doubled and in a pirate voice "Ar Ar Ar Ar".

Chronological change describes semantics, rather than requiring an append-only storage implementation or promising its performance characteristics. EDB also does not promise unconditional, indelible retention. `:db/noHistory` permits older attribute facts to be omitted from retained indexes; it does not erase copies in logs, receipts, or backups. [Excision](../08_operations/00_deployment.md#excision) is a separate controlled operation. Physical garbage collection and authorization also affect whether old captured or serialized values can still retrieve their backing data; see [captured-value retention](00_database_values.md#captured-value-retention).

## Data and Programs

Schema, application facts, source descriptions, and transaction metadata use the same data model. Stored native functions can also be named by entities: `:db/fn` holds a function value referring to immutable native program content. This is EDB's native program model, not a serialized Clojure or JVM function.

`DatabaseValue::invoke` resolves the function using that exact database value. Returned transaction forms are data; invoking a function does not commit them. Submit those forms through the transaction API to assess and commit a change. See [persisted programs](../04_transactions/01_persisted_programs.md) for program roles, installation, and invocation controls.
