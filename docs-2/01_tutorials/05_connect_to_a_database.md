# Connect to a Database

Previously you learned how to [run a transactor](04_run_a_transactor.md).
This section guides you through connecting to a database and submitting your
first transaction.

## Creating a Database

EDB identifies a database using a PostgreSQL connection configuration and a
logical database name. The PostgreSQL connection selects the storage database
and schema; the logical name selects one EDB database within that storage.
Applications also specify a transaction endpoint when submitting writes.

The database for this tutorial is `edn-demo`, which you created before starting
the transactor in the previous step. The creation command, run from the root of
your EDB checkout with administrative credentials in `EDB_POSTGRES_URL`, was:

```sh
target/release/edb create --database edn-demo
```

This creates a logical database in already installed EDB storage. It prints
`CREATED` with the database's catalog information when newly created, or `EXISTS`
when the database already exists. You do not need to create it again to connect.
Creation does not provision the PostgreSQL database, schema or roles; see
[setup](../00_start_here/01_setup.md) if you have not completed that step.

## Create a Connection

Once you have [created a database](#creating-a-database), you can use the EDB peer
library to interact with it. Leave the transactor running in its terminal. In
your application terminal, set `EDB_POSTGRES_URL` to the **peer** connection from
setup, selecting the same PostgreSQL database and schema. Use the private socket
path printed in the previous step. For example, replace the directory below
with your actual directory:

```sh
export EDB_TUTORIAL_ENDPOINT=/tmp/edb-tutorial.ABC123/transactor.sock
```

With the [peer library](../00_start_here/02_peer_library.md) available in your Rust
project, create a connection:

```rust
use edb_core::{Connection, EntityRef, Keyword, TransactionRequest, postgres_config_from_env};
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let database_name = "edn-demo";
    let conn = Connection::connect_configured(
        postgres_config_from_env()?, database_name, 64,
    )?;

    println!("Connected at basis {}", conn.db().basis_t());

    // Add the transaction code below here.
    Ok(())
}
```

The variable `conn` holds your database connection. The final argument, `64`,
sets the immutable-node cache entry capacity. `conn.db()` captures the
connection's current immutable database value; printing its basis inspects
which committed logical version it represents. A newly created database starts
at basis `0`. The connection reads directly from PostgreSQL; opening it does
not start a writer.

Any successfully committed transactions submitted through this connection will
be persisted in the PostgreSQL storage that you chose when creating your
database.

Try adding a new entity with the `:db/doc` value `"Hello world"`. Add this code
inside `main`, before `Ok(())`:

```rust
let endpoint = std::env::var("EDB_TUTORIAL_ENDPOINT")?;
let request = TransactionRequest::from_edn(
    "tutorial-hello-world",
    r#"[{:db/id "hello" :db/doc "Hello world"}]"#,
    EntityRef::Ident(Keyword::new("db.source", "system")),
)?;
let committed = conn.transact_socket(&endpoint, request, Duration::from_secs(30))?;
println!("Committed at basis {}, replayed={}", committed.basis_t, committed.replayed);
let report = committed.report?;
println!("db-before: {}", report.db_before.basis_t());
println!("db-after: {}", report.db_after.basis_t());
println!("tx-data: {:?}", report.tx_data);
println!("tempids: {:?}", report.tempids);
```

`:db/doc` is a built-in string attribute, so this transaction needs no custom
schema. `"hello"` is a temporary ID that the report maps to the new entity's ID.
The request supplies the built-in source entity `:db.source/system` once for
the transaction. Every resulting datom carries that source, including the
automatically recorded transaction instant.
The transaction report contains the exact immutable database values before and
after the transaction, the datoms asserted or retracted, and the temporary ID
mapping.

You can submit the same transaction from the executable instead. It opens a
connection using the peer configuration in your environment:

```sh
target/release/edb transact --database edn-demo \
  --endpoint "$EDB_TUTORIAL_ENDPOINT" --request-key tutorial-hello-world \
  --source ':db.source/system' --file - <<'EDN'
[{:db/id "hello" :db/doc "Hello world"}]
EDN
```

The executable prints EDN with `:edb/committed true`, the committed basis,
transaction hash, and replay flag. Its report fields contain basis numbers in
place of Rust database values. An illustrative report portion for the first
transaction is shown below; IDs, time and basis depend on your database:

```text
{:edb/db-before-t 0
 :edb/db-after-t 1
 :edb/tx-data [[13194139533313 50 #inst "2023-11-14T22:13:20.000Z" 13194139533313 source-id true]
               [17592186045416 62 "Hello world" 13194139533313 source-id true]]
 :edb/tempids {"hello" 17592186045416}}
```

Each datom is `[entity attribute value transaction source added?]`. The first
records the transaction time; the second asserts `"Hello world"` on your new
entity. Here, `source-id` stands for the numeric entity ID resolved from
`:db.source/system`.

Keep the request key `tutorial-hello-world` when retrying this exact transaction
after a lost reply. Running both examples uses the same EDN request, key, and
source, so the second submission replays the retained receipt. Use a new key
for a new transaction. A confirmed commit remains committed even if opening its
local report fails: Rust exposes that failure through `committed.report`, and the CLI
can return `:edb/report-error`. See [transactions and exact retries](00_edn_workflow.md#transactions-and-exact-retries).

## Interact with EDB

The next step will be to define some [schema for your new database](06_transact_schema.md).
