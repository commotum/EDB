# `edb_core` library

The EDB peer library is designed to be embedded in applications. It is the
gateway to the rest of the database, submitting
[transactions](../04_transactions/00_transactions.md) through a configured
transaction endpoint and observing durable database progress. It also provides
local query execution and caching of immutable database data, including
[Datalog query](../05_query_and_pull/00_queries.md), Pull, and other facilities
for managing your data. The library contains the communication components for
connecting to the transactor and
[PostgreSQL storage](../08_operations/06_storage_services.md).

The library can also be used for standalone, in-memory work with
`Database::bootstrap()`, without PostgreSQL or a transaction service. See the
[in-memory tutorial](../01_tutorials/02_in_memory.md).

The Cargo package is `edb-core`; Rust programs import it as `edb_core`. The
principal types below are exported at the crate root, except where a module
path is shown. See the [native Rust API reference](04_native_api_reference.md)
for operations organized by function and method.

## Type summary

| Type | Description |
| --- | --- |
| [Attribute](types/01_attribute.md) | Programmatic representation of a [schema attribute](../03_schema/02_schema_reference.md). |
| [Connection](types/02_connection.md) | A connection to a database for submitting and monitoring transactions, and retrieving the current value of the database. Submission uses an attached or explicitly selected transaction endpoint. |
| [DatabaseValue, Database](types/03_database.md) | An immutable, point-in-time database value. `DatabaseValue` is the captured read value; `Database` supplies the eager in-memory representation. |
| [Datom](types/05_datom.md) | A point-in-time fact: `[entity, attribute, value, transaction, added]`. Facts in a database value are immutable; owned Rust datoms have public fields. |
| [Entity](types/06_entity.md) | Lazy, associative access to an entity's attributes in a database value. |
| [AsyncOperation<T>, AsyncTransaction](types/07_async_operation.md) | Futures that support a completion callback. |
| [LogValue, LogTransaction](types/08_log.md) | Access to the transaction log and its transaction records. |
| [Peer and peer entry points](types/09_peer.md) | `Peer` is a lower-level reader for capturing database values and querying. `Connection` is the application entry point; `DatabaseCatalog` manages database names and creation. |
| [Query, QueryInput, QueryControl](types/10_query_request.md) | Parameters for query execution. `edn_query::BoundEdnQuery` contains a prepared EDN query and its bound inputs. |
| [Value, Keyword, Symbol, RuntimeValue, edn::EdnValue](types/11_data_utilities.md) | Data representations for stored values, names, runtime computation, and EDN. Rust collections and the public `edn`, `edn_value`, `edn_query`, `edn_pull`, and `edn_transaction` modules supply data construction, reading, and conversion utilities. |

## Callback summary

| Callback | Description |
| --- | --- |
| [Database filter predicate](types/04_database_predicate.md) | Boolean-valued function for filtering a database: `Fn(&DatabaseValue, &Datom) -> bool + Send + Sync + 'static`. Pass a function or closure to `DatabaseValue::filter`. |
