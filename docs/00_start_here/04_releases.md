# EDB releases

EDB is in first-release development. The current source checkout declares
version `0.1.0`; this is a package version, not a published release history.

## EDB software

The `edb-core` Cargo package contains the Rust peer library and the `edb`
executable. It is MIT-licensed and marked `publish = false`. Use a local source
checkout as described in [setup](01_setup.md).

The package includes:

- [Peer connections and immutable database values](../07_peer_api/00_connections.md)
- [Datalog queries](../05_query_and_pull/00_queries.md) and
  [Pull](../05_query_and_pull/06_pull_reference.md)
- [Local caching](../09_optional/00_local_cache.md)
- [PostgreSQL storage and transactor operation](../08_operations/00_deployment.md)

No Cargo feature flags are needed to enable these APIs. See each feature's
documentation for configuration and operating limits.

Use the links below or build directly from the checkout root, with the
[build requirements](01_setup.md#rust-and-native-build-requirements) installed:

```sh
cargo build --locked --release --bin edb
target/release/edb --version
```

The version command prints `edb 0.1.0` for this package version.

| Artifact | Version | Acquisition |
| --- | --- | --- |
| [EDB executable](01_setup.md) (`edb`) | 0.1.0 | Build locally as above; executable: `target/release/edb` |
| [Peer library](02_peer_library.md) (`edb_core`) | 0.1.0 | Add `edb-core = { path = "/path/to/EDB" }` under `[dependencies]` in your application's `Cargo.toml` |

The library and executable share the package version. For current storage-format
requirements, see [provisioning and current storage](../08_operations/00_deployment.md#provisioning-and-current-storage).
