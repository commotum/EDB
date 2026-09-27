# Client Getting Started

Getting started with the [client APIs](../00_start_here/02_peer_library.md#accessing-the-client-apis)
is similar to [getting started with the peer](03_peer_tutorial.md), with a few
differences in how you submit and wait for operations.

Once you have your [development environment](#local-development-setup) set up
and have [connected to a database](#connect-to-a-database), follow the
instructions on this page. The examples use the movie schema from the peer
tutorial and a local Unix socket for transactions.

## Installing the Client Library

The EDB library includes both the synchronous native API and the
[asynchronous API](../07_peer_api/01_async_client.md). Both are available from
the `edb-core` Cargo package. The current package is unpublished; use a path
dependency on your EDB checkout.

### Cargo

Create a Rust application with `cargo new hello-edb`. Add the following to its
`Cargo.toml` dependencies, replacing the path with your checkout:

```toml
[dependencies]
edb-core = { path = "/path/to/EDB" }
```

Use a Rust toolchain supporting Rust 2024 and the package's dependencies, as
described in [setup](../00_start_here/01_setup.md). Cargo makes the library
available as `edb_core`. Run `cargo check` from your application's directory to
check the dependency and your code; run the completed program with `cargo run`.

### EDN Command Line

You can also submit the same transaction data through the `edb` executable.
From the root of the EDB checkout, build it with:

```sh
cargo build --release --locked --bin edb
```

The [EDN workflow](00_edn_workflow.md) covers its transaction, query and Pull
commands. Building the executable and adding the Rust dependency are separate
steps; neither provisions storage or starts a transactor.

## Local Development Setup

EDB clients read directly from PostgreSQL and submit writes to a transactor.
The transactor communicates with storage to commit transactions. The native
connection provides the read API in your application process.

Follow [setup](../00_start_here/01_setup.md) to prepare PostgreSQL storage and
distinct administrative, writer and peer credentials, then
[run a transactor](04_run_a_transactor.md) for the logical database `edn-demo`.
That guide creates the database and a private socket directory before starting
the writer:

```sh
target/release/edb transactor --database edn-demo --endpoint "$endpoint_dir/transactor.sock"
```

This starts a transactor with:

| Setting | Example value | Description |
| --- | --- | --- |
| `EDB_POSTGRES_URL` | Your writer connection | PostgreSQL database, schema and writer credentials |
| `--database` | `edn-demo` | The logical EDB database already created in that storage |
| `--endpoint` | `"$endpoint_dir/transactor.sock"` | A socket in an existing private directory owned by your OS user |

Wait for its `READY` line. The process occupies its terminal until stopped;
Ctrl+C stops it. Continuing with transactions in this tutorial requires the
transactor to remain running. PostgreSQL persists the data beyond the life of
the transactor process. Reads do not require a running transactor.

For a database that lives only in memory, use the
[in-memory tutorial](02_in_memory.md). Those local immutable values do not
provide a socket service to which this client can connect.

### Integrate the Client Library

The client library must be integrated into your application as described in
[Cargo setup](../00_start_here/02_peer_library.md). In the application terminal,
set `EDB_POSTGRES_URL` to your **peer** connection, selecting the same PostgreSQL
database and schema. Set `EDB_TUTORIAL_ENDPOINT` to the actual socket path from
the transactor terminal; replace the example directory:

```sh
export EDB_TUTORIAL_ENDPOINT=/tmp/edb-tutorial.ABC123/transactor.sock
```

The local socket authenticates applications running as the same OS user. For
applications on another machine, see
[remote applications](01_application_workflow.md#remote-applications).

## Connect to a Database

The first step to interacting with EDB through the client API is opening a
native connection with `Connection::connect_configured`. This takes:

- A PostgreSQL configuration, here loaded with `postgres_config_from_env()`.
- The logical database name, `edn-demo` in this tutorial.
- An immutable-node cache entry capacity, here `64`.

Create the connection in `src/main.rs`:

```rust
use edb_core::{Connection, TransactionRequest, postgres_config_from_env};
use edb_core::edn::write_edn;
use edb_core::edn_value::transaction_report_to_edn;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = postgres_config_from_env()?;
    let conn = Connection::connect_configured(cfg, "edn-demo", 64)?;

    println!("Database: {}", conn.identity().database_id());
    println!("Basis: {}", conn.db().basis_t());

    // Insert the transaction code below here.
    Ok(())
}
```

The variable `conn` holds your database connection. Inspecting it this way shows
the stable storage ID of the database and the basis of its current immutable
database value. A newly created database has basis `0`; prior tutorial
transactions will have advanced it. You can now use `conn` as an input to future
operations. Opening the connection does not start a writer.

To use the asynchronous API, create a worker pool and a client around that same
connection during application setup:

```rust
let workers = edb_core::AsyncExecutor::new(Default::default())?;
let client = edb_core::AsyncClient::new(&conn, &workers)?;
```

Create the connection and worker pool outside an async executor task. The
client performs admitted blocking work on its fixed worker pool and returns
Rust futures and streams. You supply the executor that polls those futures.

## Transacting

The client's transaction methods take a `TransactionRequest`. Create one from
the EDN transaction collection and a request key with
`TransactionRequest::from_edn`.

`movie-schema.edn` is defined in the peer tutorial's
[transacting schema](06_transact_schema.md) section. Save that file in your
application's working directory. It defines `:movie/title` and `:movie/genre`
as cardinality-one strings and `:movie/release-year` as a cardinality-one long,
along with their documentation strings. Follow that guide starting from the
schema section, using these client calls to submit the same data.

Insert this code before `Ok(())` in `main`:

```rust
let endpoint = std::env::var("EDB_TUTORIAL_ENDPOINT")?;
let movie_schema = std::fs::read_to_string("movie-schema.edn")?;
let request = TransactionRequest::from_edn("tutorial-movie-schema", &movie_schema)?;
let committed = conn.transact_socket(&endpoint, request, Duration::from_secs(30))?;

println!("Committed at {}, replayed={}", committed.basis_t, committed.replayed);
let report = committed.report?;
println!("{}", write_edn(&transaction_report_to_edn(
    report.db_before.basis_t(),
    report.db_after.basis_t(),
    &report.tx_data,
    &report.tempids,
)?)?);
```

`transact_socket` blocks while waiting for the transaction result. Its
`CommittedTransaction` contains the committed basis, transaction hash, replay
flag and a separately fallible local report. The report contains:

| Rust field | Printed EDN field | Meaning |
| --- | --- | --- |
| `db_before` | `:edb/db-before-t` | Database before the transaction; EDN prints its basis |
| `db_after` | `:edb/db-after-t` | Database after the transaction; EDN prints its basis |
| `tx_data` | `:edb/tx-data` | Datoms asserted or retracted by the transaction |
| `tempids` | `:edb/tempids` | Temporary IDs mapped to assigned entity IDs |

For a freshly created database, the printed report has this shape. IDs, time
and basis depend on your database; the timestamp below is illustrative:

```edn
{:edb/db-before-t 0
 :edb/db-after-t 1
 :edb/tx-data
 [[0 13 #edb/ref 1000 13194139533313 true]
  [0 13 #edb/ref 1001 13194139533313 true]
  [0 13 #edb/ref 1002 13194139533313 true]
  [1000 10 :movie/genre 13194139533313 true]
  [1000 40 #edb/ref 23 13194139533313 true]
  [1000 41 #edb/ref 35 13194139533313 true]
  [1000 62 "The genre of the movie" 13194139533313 true]
  [1001 10 :movie/title 13194139533313 true]
  [1001 40 #edb/ref 23 13194139533313 true]
  [1001 41 #edb/ref 35 13194139533313 true]
  [1001 62 "The title of the movie" 13194139533313 true]
  [1002 10 :movie/release-year 13194139533313 true]
  [1002 40 #edb/ref 22 13194139533313 true]
  [1002 41 #edb/ref 35 13194139533313 true]
  [1002 62 "The year the movie was released in theaters" 13194139533313 true]
  [13194139533313 50 #inst "1970-01-01T00:00:01.000Z" 13194139533313 true]]
 :edb/tempids {"__map/00000000000000000000" 1000
              "__map/00000000000000000001" 1001
              "__map/00000000000000000002" 1002}}
```

The report includes three attribute definitions, their schema installation
facts, and the transaction time. Each datom is
`[entity attribute value transaction added?]`; `#edb/ref` marks an entity
reference. The schema maps omit `:db/id`, so EDB generates temporary IDs. Their
names and allocation order are implementation details.

For the asynchronous version, add Tokio to your application's dependencies as
one possible future executor:

```toml
tokio = { version = "1", features = ["rt"] }
```

Create `workers` and `client` as above, then replace the synchronous
`let committed = ...` line with:

```rust
let runtime = tokio::runtime::Builder::new_current_thread().build()?;
let committed = runtime.block_on(async {
    client.transact_socket(&endpoint, request, Duration::from_secs(30))?.await
})?;
```

The first `?` handles admission to the worker pool; `.await` waits for the
transaction result. The report code stays outside the async block because the
returned database values retain their synchronous APIs. The 30-second deadline
includes time in the worker queue. Dropping an admitted transaction future does
not cancel the transaction; see [async clients](../07_peer_api/01_async_client.md)
for admission, deadlines and cancellation.

From the EDB checkout, the equivalent command is:

```sh
target/release/edb transact --database edn-demo \
  --endpoint "$EDB_TUTORIAL_ENDPOINT" \
  --request-key tutorial-movie-schema --file /path/to/hello-edb/movie-schema.edn
```

Keep the same request key and transaction data when retrying after a lost reply.
These Rust and CLI examples share the peer tutorial's key, so repeating the same
request retrieves its retained receipt. Use a new key for a new transaction.
A confirmed commit remains committed if `committed.report` is an error; report
failure is not a transaction rejection. See
[transactions and exact retries](00_edn_workflow.md#transactions-and-exact-retries).

The rest of the getting-started guide applies with these calling conventions in
mind. Continue with [transacting movie data](07_transact_data.md), substituting
`first-movies.edn` and the key `tutorial-first-movies`, then
[querying the data](08_query_the_data.md) and
[seeing historic data](09_see_historic_data.md). Submit the movie data only after
the schema transaction has committed; the new attributes must exist before
that transaction begins.
