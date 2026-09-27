# Glossary

## ACID

Atomic, Consistent, Isolated, and Durable. See [ACID](../04_transactions/08_acid.md)
for the guarantees of durable EDB transactions and the deployment conditions on
which durability depends.

## asOf

A database value as of a point in time. With `as_of`, you can reuse existing
queries and rules to ask questions about points in time other than the present.
Use `DatabaseValue::as_of(t)` for logical time or `as_of_time_point` for a
[time-point](#time-point). See [database filters](03_database_filters.md).

## Assertion

An atomic fact in the database, associating an [entity](#entity),
[attribute](#attribute), [value](#value), and a [tx](#tx). Opposite of a
[retraction](#retraction). Its datom has `added = true`.

## Attribute

Something that can be said about an [entity](#entity). An attribute has a name,
e.g. `:person/first-name`, and a value type, e.g. `:db.type/long`, and a
cardinality. See the [schema reference](../03_schema/02_schema_reference.md).

## Attribute Identifier

An entity identifier that refers to an attribute. Native attribute APIs use
their documented attribute representation, such as `AttributeName::Id` or
`AttributeName::Ident`; not every attribute position accepts every entity
identifier form.

## Backup Repository

A local filesystem directory containing portable backup points and their
immutable objects. EDB backup APIs take repository paths, not a vendor-specific
backup URI. See [backup, verify and restore](../08_operations/00_deployment.md#backup-verify-and-restore)
and [query a backup directly](../08_operations/05_backup_reads.md).

## Basis-t

The database [t](#t) that is the basis for the current database value, i.e. the
most recent point in time that this database value has seen. `basis_t()` remains
the basis of the underlying captured value even when an as-of or since view
exposes less information.

## Cardinality

Property of an [attribute](#attribute) that specifies how many values of the
attribute can be associated with a single entity. Possible values are
`:db.cardinality/one` and `:db.cardinality/many`.

## Caches

EDB caches immutable data in peers and the transaction service. Caches include
decoded index nodes and native programs; a local SSD cache of immutable object
bytes is optional. Cache limits are not limits on total process memory or on
values retained by application code. See [connections](../07_peer_api/00_connections.md)
and the [optional local SSD cache](../09_optional/00_local_cache.md).

## Client

A process that uses an EDB library to obtain a [connection](#connection) to
interact with one or more [databases](#database). The asynchronous transaction
client is also available separately from a peer; see the
[async client](../07_peer_api/01_async_client.md).

## Closed World Assumption

Assumption that truth is what the database knows. Databases that intend to store
data of record typically make the closed world assumption. EDB query constraints
operate on the supplied facts and rules: a negative clause tests for the absence
of a match in those sources, not for truth outside the database.

## Component

A [reference attribute](#reference) that represents a part of its
[entity](#entity). E.g. your arm is a component of you, but your sister isn't.
An attribute is a component if it has `:db/isComponent` set to true. Component
references support nested entity maps, recursive Pull and recursive entity
retraction; see [schema](../03_schema/02_schema_reference.md).

## Component Attribute

See [component](#component).

## Connection

Client object that provides access to a database. Programs can use a connection
to submit transactions. EDB's `Connection` combines a live peer with an optional
transaction client. `Connection::db()` captures the resident immutable value;
it does not refresh it from storage. See
[connections and captured reads](../07_peer_api/00_connections.md).

## Coordination

The ability of a group of processes to negotiate who is responsible for the
various [roles](#role) in a [system](#system). EDB uses PostgreSQL-backed writer
leases and guarded publication to establish transaction authority. See
[deployment](../08_operations/00_deployment.md).

## Covering Index

A covering index contains (rather than points to) the data. EDB datom indexes
are covering indexes. See [indexes](../06_indexes/00_indexes_and_log.md).

## Credentials

Information used to authenticate for a particular task. EDB's PostgreSQL
deployment can separate reader, writer and operator credentials. These are
trusted storage credentials, not per-database application authorization. See
[provisioning](../08_operations/00_deployment.md#provisioning-and-current-storage).

## Database

A database is a set of [datoms](#datom). A database value represents an immutable
view of that information. A live database can produce successive values through
transactions without changing a value already held by an application. See
[database values](00_database_values.md).

## Datom

An atomic fact in a database, composed of entity/attribute/value/transaction/added.
Pronounced like "datum", but pluralized as datoms. The `added` flag distinguishes
assertions from retractions. See the [data model](01_data_model.md).

## Database Identity

The identity of a logical database, distinct from its reusable catalog name or
PostgreSQL connection address. `DatabaseIdentity` retains a storage database ID
and lineage ID. Renaming a database does not change that identity. See
[database lifecycle](../08_operations/02_database_lifecycle.md).

## Data Function

A function installed in a database. EDB binds a stored program to an entity's
`:db/fn` attribute, whose value type is `:db.type/fn`. The value identifies
authenticated native program content; it is not Clojure or JVM source code.
See [persisted programs](../04_transactions/01_persisted_programs.md).

## Datalog

A deductive query system, typically consisting of:

- A database of facts
- A set of rules for deriving new facts from existing facts
- A query processor that, given some partial specification of a fact or rule,
  finds all instances of that specification implied by the database and rules,
  i.e. all the matching facts

EDB's built-in [query](#query) is an implementation of Datalog.

## Domain attribute

An attribute used to model something in your application domain.

## EDN

Extensible data notation, used by EDB and other applications as a data transfer
format. EDB uses EDN for transaction data, queries, Pull patterns and command-line
results. See [programming with data and EDN](02_programming_with_data_and_edn.md)
for the accepted notation and tags.

## Entity

The first component of a [datom](#datom), specifying who or what the datom is
about. Also the collection of datoms associated with a single entity, as in the
Rust type `Entity`. See [Pull and entities](../05_query_and_pull/02_pull_and_entities.md).

## Entity id

An opaque identifier assigned by EDB that uniquely identifies an entity within
its database. Entity IDs are integers for efficiency, but application programs
should treat them as opaque IDs. Supported partition helpers expose placement;
numeric entity order does not generally establish creation time. See
[partitions](../04_transactions/02_partitions.md).

## Entity Identifier

A value that identifies an entity. Can be an entity ID, an ident, or a lookup ref.
See [identity and values](../03_schema/01_identity_and_values.md).

## Environment

An instantiated set of all the resources needed to run an application.

## External Key

A unique identifier external to EDB. Typical external key types are email
address, UUID, and URI. External key attributes intended to support identity
upserts should be declared as `:db.unique/identity`. See
[identity and uniqueness](../03_schema/05_identity_and_uniqueness.md).

## Epoch

A bounded generation of work or authority. In EDB, a writer epoch identifies a
writer's authority, while storage protection epochs participate in safe object
publication and collection. Neither is database logical time. Background
indexing publishes persistent indexes independently of transaction commits; see
[indexes](../06_indexes/00_indexes_and_log.md) and
[maintenance controls](../08_operations/04_maintenance_controls.md).

## Excision

Removal of selected [datoms](#datom), including retained history, from the
successor database generation. Excision should be a very infrequent operation,
and should not be used to correct erroneous data. It does not erase previously
captured RAM values, independent backups or local cache copies. Logical excision
and later physical collection are separate operations. See
[excision](../08_operations/00_deployment.md#excision).

## Fact

See [datom](#datom).

## Ident

A keyword value of the `:db/ident` attribute that uniquely identifies an
[entity](#entity). `:db/ident` is an attribute, not a value type. See
[identity and uniqueness](../03_schema/05_identity_and_uniqueness.md).

## Index

Sorted collection of datoms. Indexes are named by the order in which datom
components are used for sort. E.g. an index that sorts first by [entity](#entity),
then [attribute](#attribute), then [value](#value), then [tx](#tx) is called EAVT.
EDB also provides AEVT, AVET and VAET, with different inclusion rules. Transaction
coordinates sort descending within the preceding components. See
[indexes and the transaction log](../06_indexes/00_indexes_and_log.md).

## Infrastructure

The set of all environments for an application.

## Keyword

Data type representing a name, e.g. `:email` or (with namespace)
`:customer/email`.

## Lookup-ref

A two-element sequence containing a unique attribute and a value that identifies
an entity, e.g. `[:customer/email "person@example.com"]`. See
[identity and uniqueness](../03_schema/05_identity_and_uniqueness.md).

## LRU

Least Recently Used, a cache replacement policy.

## Metrics

Statistics used to measure the health of a running system. EDB exposes native
reports and an optional bounded event publisher; applications choose how to
export these observations. See [observability](../08_operations/03_observability.md).

## Namespace

Prefix portion of a [keyword](#keyword) used to make the keyword globally unique.
Namespaces serve a similar function to table names in a relational store,
without imposing any obligations or limitations, e.g. an entity can have
attributes from more than one namespace.

## Object Cache

An in-memory cache of decoded immutable objects, such as index nodes. Peers can
reuse decoded nodes across captured values. Cache capacity does not include all
application-retained results, active cursors or snapshot metadata. See
[connections](../07_peer_api/00_connections.md).

## Parameters

Named slots for application configuration data. EDB services and connections
receive configuration through their documented native configuration types and
command-line options; see [deployment](../08_operations/00_deployment.md).

## Partition

A logical grouping of entities in a database. Every [entity](#entity) belongs to
a partition that is assigned when the entity is created. Named partitions have
unique qualified names; implicit partitions can be selected without an
installation transaction. Partitions act as a locality hint, so that larger
systems can plan ahead for entities that are frequently accessed together.
Partitions are typically coarser grained than relational tables. Partitioning
is invisible to the query system and does not require partition-specific query
code. It is not an authorization boundary or an independent transaction stream.
See [partitions](../04_transactions/02_partitions.md).

## Peer

A process that uses the EDB library to interact with a system and obtain
[connections](#connection) to one or more [databases](#database). Peers have
in-memory access to database values and an integrated Datalog query engine.
EDB's `Peer` captures immutable roots, caches data and reads cold data from
storage as needed. See [connections](../07_peer_api/00_connections.md).

## Pull

A declarative way to make hierarchical selections of information about entities.
See the [Pull reference](../05_query_and_pull/06_pull_reference.md).

## Query

EDB's Datalog system. A query finds [values](#value) in a [database](#database)
subject to the given constraints, and can be specified as [EDN](#edn) or native
Rust query data. See the [query reference](../05_query_and_pull/05_query_reference.md).

## Reference

An attribute that refers to another entity. References always have the value
type `:db.type/ref`.

## Reference Attribute

See [reference](#reference).

## Retraction

An atomic fact in the database, dissociating an [entity](#entity) from a
particular [value](#value) of an [attribute](#attribute). Opposite of an
[assertion](#assertion). Its datom has `added = false`. Retraction preserves
history subject to retention and `:db/noHistory`; it is distinct from excision.

## Role

Generic name for a responsibility such as transactor, peer or persistence
service. In an EDB deployment these responsibilities are implemented by the
transaction service, application peers and PostgreSQL storage. PostgreSQL
credential roles are a separate use of the word; see [credentials](#credentials).

## Rule

A named group of [query](#query) constraints, to allow reuse of logic across
queries. See [rules](../05_query_and_pull/05_query_reference.md).

## Schema

The set of possible attributes that can be associated with entities. Any entity
can have any attribute, subject to the attribute's constraints and transaction
validation. See [schema](../03_schema/02_schema_reference.md).

## Schema Attribute

A built-in attribute used to define schema, e.g. all attributes are named by
`:db/ident`.

## Segment

An immutable portion of an index. EDB's persistent index tree has roots,
directories and leaves; leaf nodes contain datoms. Leaf sizing is bounded by
both datom count and bytes, rather than by a fixed number of facts. See
[indexes](../06_indexes/00_indexes_and_log.md).

## Serialized Object Cache

A cache that stores encoded immutable object bytes instead of decoded objects.
EDB's optional local SSD cache stores authenticated canonical bytes and falls
back to PostgreSQL on runtime cache failures. See
[local SSD cache](../09_optional/00_local_cache.md).

## Storage Resources

The durable elements managed by an EDB system, including immutable objects and
the mutable references that publish them. See
[deployment](../08_operations/00_deployment.md).

## Storage Service

Subsystem responsible for persistence. EDB's current live storage provider is
PostgreSQL. Portable backups can also supply immutable database values for
offline reads. See [deployment](../08_operations/00_deployment.md) and
[backup reads](../08_operations/05_backup_reads.md).

## Stringified Keyword

A string containing the printed representation of a keyword, e.g. `":name"`
or `":person/name"`. An EDN string and an EDN keyword are distinct values;
use the representation required by the API.

## System

A complete EDB installation, consisting of PostgreSQL storage, transaction
services and application peers. Deployment and PostgreSQL availability are
operator responsibilities; see [deployment](../08_operations/00_deployment.md).

## Time-point

Data structure that can be resolved to a point in time in a database. Can be a
database [t](#t), a [tx](#tx), or an instant. The native variants are
`TimePoint::T`, `TimePoint::Tx` and `TimePoint::Instant`; instants use Unix epoch
milliseconds. See [database filters](03_database_filters.md).

## t

A point in time in a database. Every fresh committed transaction is assigned a
numeric t value greater than any previous t in the database. Peers can observe
different prefixes of that ordered history; use synchronization when a required
basis must be visible. See [synchronization](../04_transactions/09_synchronization.md).

## tx

An [entity](#entity) representing a [transaction](#transaction). Every
[datom](#datom) includes the tx that created its assertion or retraction, allowing
recovery of retained history. Transactions are automatically associated with
wall-clock time through `:db/txInstant`, but are otherwise ordinary entities.
In particular, application code can make additional assertions about
transactions. `t_to_tx` and `tx_to_t` convert between logical time and transaction
entity IDs. See [transaction data](../04_transactions/04_transaction_data.md).

## Transaction

An atomic unit of work in a database. Durable EDB writes are transactional,
fully serialized per database, and ACID. Local `with` evaluates a speculative
transaction without committing it. See [ACID](../04_transactions/08_acid.md)
and the [transaction model](../04_transactions/05_transaction_model.md).

## Transaction Function

A function that runs inside a transaction, taking the current database value
plus user arguments and expanding into data to be added by the transaction.
EDB supports stored programs and explicitly registered native Rust callbacks.
See [transaction functions](../04_transactions/07_transaction_functions.md).

## Transaction Log

An accumulate-only log of transactions. EDB stores the log as immutable objects
in PostgreSQL; excision can publish a successor generation with selected history
removed. A captured log exposes its fixed committed prefix. See
[indexes and the transaction log](../06_indexes/00_indexes_and_log.md).

## Transactor

A process with the ability to commit transactions for a given database. EDB
allows one authoritative writer at a time for a database lineage, with any number
of [peers](#peer). Writer leases and guarded publication prevent stale writers
from publishing. A database need not have an active writer in order to be read.
See [deployment](../08_operations/00_deployment.md).

## Tuple

An ordered list of elements. Relation-shaped queries return sets of tuples;
EDB also supports scalar, collection and single-tuple find shapes. A schema
attribute can store a tuple value. See
[query find specifications](../05_query_and_pull/05_query_reference.md) and
[schema](../03_schema/02_schema_reference.md).

## Unique

Attribute of an [attribute](#attribute). Each [entity](#entity) that has a
[value](#value) for a `:db/unique` attribute must have a different value.
`:db/unique` has two possible values:

- `:db.unique/value`: attempts to assert a duplicate value will fail.
- `:db.unique/identity`: assertions on tempids can resolve to an existing entity
  through [upsert](#upsert). Conflicting identity claims still fail.

See [identity and uniqueness](../03_schema/05_identity_and_uniqueness.md).

## Upsert

Either insert or update an [entity](#entity), depending on whether the unique
entity already exists. EDB resolves transaction tempids through unique identity
attributes; it does not merge arbitrary existing entity IDs. See
[identity and uniqueness](../03_schema/05_identity_and_uniqueness.md).

## Value

Something that does not change, e.g. `42`, `"John"`, or
`#inst "2012-02-29T00:00:00Z"`. A [datom](#datom) relates an [entity](#entity)
to a value through an [attribute](#attribute).

## Value type

Attribute of an [attribute](#attribute) that specifies the data structure that
can be stored in the attribute. The value type determines how a [value](#value)
is validated and represented, e.g. `:db.type/string`, `:db.type/long`, or
`:db.type/ref`. See the [schema reference](../03_schema/02_schema_reference.md).
