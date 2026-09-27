# Accessing the peer library

This page covers integrating the [EDB peer library](../07_peer_api/00_connections.md)
into your Rust project. It assumes you have an EDB source checkout and the build
prerequisites described in the [setup guide](01_setup.md).

The peer library must be a dependency of your application to interact with EDB
through the native API.

## Cargo setup

The EDB checkout contains the `edb-core` Cargo package, including the peer library
and the `edb` executable. The current package has `publish = false`; use a local
path dependency to build against the checkout.

Use a Rust toolchain that supports the package's Rust 2024 edition and its
dependencies. The package defines no Cargo features, so no EDB feature flags are
needed to access the peer API.

## Using from Cargo.toml

To include the EDB peer library in a Cargo project, declare the following
dependency in the `[dependencies]` section of your `Cargo.toml`:

```toml
[dependencies]
edb-core = { path = "../EDB" }
```

The path is relative to your application's `Cargo.toml`. Replace `../EDB` with
the path to the checkout directory containing EDB's `Cargo.toml`.

The package name is `edb-core`; its Rust library name is `edb_core`. For example,
the following `src/main.rs` makes native peer types available to your program:

```rust
use edb_core::{Connection, DatabaseValue};

fn capture_db(connection: &Connection) -> DatabaseValue {
    connection.db()
}

fn main() {
    let _ = capture_db;
}
```

From your application directory, check the dependency and imports with:

```sh
cargo check
```

Cargo builds the library as part of your application. Installing the `edb`
executable does not add the library to your application's dependencies. Adding
the library does not install PostgreSQL storage, create a database or start a
transactor; those are explicit setup steps.

## Accessing the client APIs

The EDB library includes both the synchronous native APIs and the
[asynchronous client API](../07_peer_api/01_async_client.md). The same
`edb-core` dependency makes `Connection`, `DatabaseValue`, `AsyncClient` and
`AsyncExecutor` available from `edb_core`; no separate client package is required.

To use the asynchronous API, create a native `Connection` and an `AsyncExecutor`
during application setup, then construct the client with
`AsyncClient::new(&connection, &workers)`. The client adapts that connection's
native operations to Rust futures and streams. See
[async native clients](../07_peer_api/01_async_client.md) for construction,
worker configuration and operation examples. The
[client tutorial](../01_tutorials/10_client_workflow.md) walks through connection
setup and synchronous or asynchronous schema submission.

## Next steps

You now have access to the EDB peer API!

Now that you have the EDB peer library integrated into your project, you can
start the [native application tutorial](../01_tutorials/01_application_workflow.md).
See [connections and captured reads](../07_peer_api/00_connections.md) for opening
an existing database and capturing an immutable database value.
