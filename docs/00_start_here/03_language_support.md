# Language Support

## Rust

EDB provides a native Rust API and an EDN data frontend. Applications can construct transactions, queries and Pull patterns with Rust types, or read the corresponding data from EDN text. Both forms use the same native database engine.

### The Rust API

The `edb-core` crate gives Rust users access to the peer library. Its public types include `Connection`, `DatabaseValue`, `TransactionRequest`, `QueryEngine` and `PullPattern`. Additionally, the `edn`, `edn_transaction`, `edn_query`, `edn_pull` and `edn_value` modules contain functions for creating, reading and working with EDB's commonly used data structures.

Rust imports use the crate name `edb_core`. See [setup](01_setup.md) for building the repository, [connections](../07_peer_api/00_connections.md) for connecting to a durable database, and the [asynchronous API](../07_peer_api/01_async_client.md) for asynchronous operations.

### Rust Types and EDN

The EDN query adapter is a data-oriented interface to the Rust query engine. It reads query data into a template, binds database values and other inputs, and prepares the native query for execution. It does not evaluate Clojure code.

The code below illustrates the translation of a typed Rust query to EDN. Both examples use a bootstrapped in-memory database value and find the IDs of entities with a `:db/doc` attribute. Run the snippets inside a function returning `Result<(), edb_core::SemanticError>`.

Here is the query constructed with Rust types:

```rust
use edb_core::{
    Clause, DataPattern, Database, FindElement, FindSpec, Keyword,
    Query, QueryControl, QueryDataSource, QueryEngine, Term, Value,
};

let db = Database::bootstrap()?.database_value();
let query = Query::new(
    FindSpec::Relation(vec![FindElement::Variable("?e".into())]),
    vec![Clause::Pattern(Box::new(DataPattern::new(
        Term::var("?e"),
        Term::Constant(Value::Keyword(Keyword::new("db", "doc"))),
        Term::Blank,
    )))],
);
let results = QueryEngine::execute_sources(
    &query,
    &[QueryDataSource::database("$", db)],
    &[],
    &QueryControl::default(),
)?;
```

And the equivalent query read from EDN:

```rust
use edb_core::{Database, QueryControl, QuerySourceValue};
use edb_core::edn::write_edn;
use edb_core::edn_query::{EdnQueryArgument, parse_query_edn};

let db = Database::bootstrap()?.database_value();
let query = parse_query_edn("[:find ?e :where [?e :db/doc]]")?;
let bound = query.bind(&[
    EdnQueryArgument::Source(QuerySourceValue::Database(db)),
])?;
let results = bound.execute(&QueryControl::default(), None)?;
let text = write_edn(&bound.result_to_edn(&results.result)?)?;
```

The EDN result is a set of tuples. These examples query only the bootstrap data; they do not open a durable database. For transactions, queries and Pull over application data, see the [EDN workflow](../01_tutorials/00_edn_workflow.md).

## Other Languages

Programs in other languages can invoke the `edb` executable and exchange EDN through files or standard input and output. EDB does not supply Java or Clojure SDKs, a language-neutral HTTP REST service, or a C ABI in this repository. Its transaction transport is used by the Rust client; it is not an HTTP query endpoint.

### The Command-Line Interface

The executable accepts transaction, query and Pull data as EDN and responds to these data commands with EDN. Use `edb help` for the available commands and options. For example, after configuring access to a database named `edn-demo`:

```sh
printf '%s\n' '[:find ?e :where [?e :db/doc]]' |
  target/release/edb query --database edn-demo --file -
```

`--file -` reads standard input; results are written to standard output. Queries execute in the CLI process. Durable transactions require a configured transactor and a caller-supplied `--request-key`. See the [application workflow](../01_tutorials/01_application_workflow.md) for starting the transactor and configuring access, and the [EDN workflow](../01_tutorials/00_edn_workflow.md) for data command options and exact retries.

### EDN

EDN is an extensible data notation, pronounced “eed-n.” EDB's [programming with data and EDN](../02_core_concepts/02_programming_with_data_and_edn.md) guide describes the notation used throughout this documentation.

A full-featured integration should support round-trip conversions between native data structures and EDN, the data format that the executable sends and receives. Doing so allows users of the native language to build transactions and queries using native data structures, just like users of the Rust library.

Keep keywords, symbols, strings, numbers and collection types distinct when converting values. Preserve tagged values, including EDB's namespaced tags for native types such as entity references and bytes. Parsed EDN, stored attribute values and general query values have different domains; valid EDN is not necessarily a valid stored attribute value. See the [EDN type and tag reference](../01_tutorials/00_edn_workflow.md#edn-types-versus-stored-values) for those distinctions.

## Third-Party Libraries

No third-party language client is supplied or validated by this repository. A general EDN reader/writer can help implement an integration, but it must preserve the types and tags described above. Libraries written for another database require adaptation to EDB's commands, request handling and result formats.
