# Run a Transactor

## Running a Transactor

For this tutorial, you will run a transactor on your local machine. PostgreSQL
will persist your data using the configured
[storage](../08_operations/00_deployment.md#provisioning-and-current-storage).
Submitting transactions requires a transactor to be running. Local development
uses PostgreSQL too; EDB does not include an embedded development storage service.

To start a transactor, you need a PostgreSQL connection, an existing logical EDB
database, and a private local socket endpoint. EDB uses environment variables and
command-line options for this configuration. Follow [EDB setup](../00_start_here/01_setup.md)
to build the executable and provision a PostgreSQL database, empty schema, and
distinct administrative, writer and peer credentials.

This guide will use the root directory of your EDB checkout and the logical
database `edn-demo`. With `EDB_POSTGRES_URL` set to your administrative connection
and the dedicated PostgreSQL LOGIN roles already created, run:

```sh
target/release/edb install --writer-role edb_writer --peer-role edb_peer
target/release/edb create --database edn-demo
```

`install` creates EDB's storage tables; `create` creates the logical EDB database
within that storage. Installation and runtime grants are separate committed
operations. If grants fail after `INSTALLED`, correct the roles and rerun the
command. The transactor does not install storage or create databases at startup.

Set `EDB_POSTGRES_URL` to your writer connection, selecting the same PostgreSQL
database and schema. TCP connections default to verified TLS; `sslmode=disable`
explicitly selects plaintext for a chosen local development deployment. See the
[application tutorial](01_application_workflow.md) for connection settings.

Create a private endpoint directory, and keep its path for the application
terminal:

```sh
endpoint_dir=$(mktemp -d /tmp/edb-tutorial.XXXXXX)
printf '%s\n' "$endpoint_dir"
```

The directory must already exist, be owned by you, and have permissions `0700`.
The local socket serves applications running as the same OS user.

## Starting a Transactor

From your shell, run:

```sh
target/release/edb transactor --database edn-demo --endpoint "$endpoint_dir/transactor.sock"
```

Wait for the `READY` line on stdout. Its format is shown below; the storage ID,
directory and lineage depend on your installation. The `database` field reports
the stable storage ID rather than the logical name `edn-demo`:

```text
READY database="<storage-id>" endpoint="/tmp/edb-tutorial.ABC123/transactor.sock" lineage=<database-lineage>
```

Next, you will use this logical database name and endpoint to
[connect to a database](05_connect_to_a_database.md) and submit transactions.
In the application terminal, set `EDB_POSTGRES_URL` to the peer connection and
use the same endpoint path. Applications read directly from PostgreSQL and
submit transactions through the socket. If following the EDN tutorial, substitute
this socket path for its example endpoint.

The transactor process will be used for the subsequent steps in this guide. Make
sure that it is running while you submit transactions. Reads do not require the
transactor to be running.

To stop the transactor, type **Ctrl+C** in its terminal. Restart it with the same
database, writer connection and endpoint path. For applications on another
machine, follow the [remote application configuration](01_application_workflow.md#remote-applications).
