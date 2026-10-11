# EDB setup

This page is for users who have [chosen EDB](00_introduction.md) and covers
building EDB as well as running a transactor.

## Get EDB

Build EDB from a local source checkout. The current Cargo package is `edb-core`,
with the library named `edb_core` and the executable named `edb`. The package is
MIT-licensed and marked `publish = false`; these instructions use the local
checkout rather than a package-registry release.

For the 0.1.0 source release:

```sh
git clone https://github.com/commotum/EDB.git
cd EDB
git checkout v0.1.0
```

Throughout the documentation, shell commands are run from the root directory
of the EDB checkout. Change to this directory now, for example:

```sh
cd /home/user/EDB
```

With the [build requirements](#rust-and-native-build-requirements) available,
build the executable:

```sh
cargo build --locked --release --bin edb
target/release/edb --version
target/release/edb --help
```

The executable is `target/release/edb`. To install it into Cargo's executable
directory instead, run:

```sh
cargo install --locked --path . --bin edb
```

With that directory on your `PATH`, you can use `edb` in place of
`target/release/edb` in the commands below.

## Run a transactor

This section is for users who have [built EDB](#get-edb) and covers running a
transactor. Before running a transactor, you must
[set up PostgreSQL storage](../08_operations/00_deployment.md#provisioning-and-current-storage)
and configure the connection. Local development also uses PostgreSQL; the
executable does not include an embedded development storage service.

Provision a PostgreSQL database and empty schema, and distinct administrative,
writer and peer credentials. Set `EDB_POSTGRES_URL` to your administrative
connection string, selecting that database and schema. TCP connections default
to verified TLS; `sslmode=disable` explicitly selects plaintext for a chosen
local development deployment. See the
[application tutorial](../01_tutorials/01_application_workflow.md) for connection
settings and runtime role requirements.

Using the administrative connection and pre-existing dedicated LOGIN roles:

```sh
target/release/edb install --writer-role edb_writer --peer-role edb_peer
target/release/edb create --database edn-demo
```

`install` creates EDB's storage tables, not the PostgreSQL database or schema.
Storage installation and runtime grants are separate committed operations; if
grants fail after `INSTALLED`, correct the roles and rerun the command.
`create` creates a logical EDB database within that storage. Runtime startup
does neither operation. Use a fresh storage installation for the current
development format; installation does not upgrade earlier formats.

### Starting a transactor

To start a transactor, run `edb transactor`, passing a logical database name and
an endpoint. This command uses a private local socket for applications running
as the same OS user. Set `EDB_POSTGRES_URL` to the writer connection first:

```sh
endpoint_dir=$(mktemp -d /tmp/edb-setup.XXXXXX)
target/release/edb transactor --database edn-demo --endpoint "$endpoint_dir/transactor.sock"
```

The endpoint directory must already exist, be owned by you, and allow access
only to you. Keep its path for the application terminal. The transactor is ready
once the `READY` line appears on stdout. Leave this process running while
applications submit transactions. Applications use peer PostgreSQL credentials
for their direct reads and the endpoint for transaction submission.

For a transactor on another machine, use the
[remote application configuration](../01_tutorials/01_application_workflow.md#remote-applications)
with verified TLS and authenticated discovery.

### Stopping a transactor

To stop a transactor, type **Ctrl+C** in the shell where you ran `edb transactor`,
or send `SIGTERM` to its process ID:

```sh
kill -TERM TRANSACTOR_PROCESS_ID
```

Replace `TRANSACTOR_PROCESS_ID` with the numeric process ID. The CLI stops
admission, drains bounded local requests and shuts down the writer. Restart
using the same database, writer connection and endpoint path.

Once you are able to start and stop EDB processes, you can
[start using EDN](../01_tutorials/00_edn_workflow.md) with this `edn-demo`
database, or follow the
[Rust application tutorial](../01_tutorials/01_application_workflow.md).

## Integrate the library

Add the checkout as a local dependency in your application's `Cargo.toml`,
replacing the path with your checkout's location:

```toml
[dependencies]
edb-core = { path = "/home/user/EDB" }
```

Import its APIs with `use edb_core::...`. See
[integrating the peer library](02_peer_library.md) and
[connections](../07_peer_api/00_connections.md) for opening a database and
submitting transactions. Local immutable database values created with
`Database::bootstrap()` can be used without PostgreSQL for speculative work;
they do not provide durable storage or start a transactor.

## Rust and native build requirements

EDB requires Rust and Cargo 1.90 or newer and uses Rust's 2024 edition. Build
with the dependencies selected by `Cargo.lock`. CI checks Rust 1.90.0 and the
current stable toolchain.

The current executable and local transport use Unix APIs. A native build needs
a linker and the OpenSSL development libraries and headers used by the
`openssl` dependency. On Linux, OpenSSL discovery commonly uses `pkg-config`.
These prerequisites are separate from the PostgreSQL server used for durable
storage. EDB does not require Java or Clojure.

The 0.1.0 release is verified on x86-64 Linux with PostgreSQL 15. Other Unix
platforms have not been verified for this release; Windows is not supported.
