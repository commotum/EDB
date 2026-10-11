# Distributing the library internally

## Overview

EDB is acquired as a Rust source checkout containing the `edb-core` Cargo
package. You may wish to maintain your own private copy to maintain control
over the availability of the dependencies and to take ownership of the
availability of the artifacts.

An internal distribution can be as simple as restricted storage containing an
EDB source snapshot, its Cargo manifest and lockfile, and the dependency sources
needed to build it. This document describes how to gather the necessary files
and where to put them. Choosing storage and managing access to that storage is
the responsibility of the user.

The current manifest declares version `0.1.0`, `publish = false`, and
`license = "MIT"`. It does not configure a public or private package
registry. EDB's [MIT License](../../../LICENSE) permits redistribution subject
to its terms, including retaining the copyright and permission notices. These
instructions do not change the package's publication settings. Retain the
license files and notices supplied with third-party dependencies.

## Gathering Dependencies

You will need to gather the EDB source, `Cargo.toml`, `Cargo.lock`, and the
dependency sources selected by the lockfile. Unlike a precompiled library
archive, this bundle is built by Cargo in the consuming environment.

The instructions below give an overview of the process. They can be integrated
into the automation systems you have in place for consuming new EDB revisions.

1. Prepare a separate staging directory containing the reviewed EDB source
   snapshot. Include the complete source tree, including SQL files under
   `src/`, the manifest, lockfile, `LICENSE`, and any examples or tests you intend to build.
   Keep the original checkout separate from this staging directory.
2. Record the source revision and any local changes included in the snapshot.
   The package version alone does not distinguish different development
   snapshots. Record `rustc --version` and `cargo --version` for the build
   environment as well.
3. Navigate to the staged EDB directory. With access to the dependency sources,
   gather the locked dependencies using
   [Cargo's vendor command](https://doc.rust-lang.org/cargo/commands/cargo-vendor.html):

   ```sh
   cargo vendor --locked --versioned-dirs vendor > vendor-config.toml
   ```

4. Add the generated source configuration to `.cargo/config.toml`. For a new
   staging directory with no existing Cargo configuration:

   ```sh
   mkdir -p .cargo
   mv vendor-config.toml .cargo/config.toml
   ```

   If that configuration already exists, merge the generated source tables
   into it instead of replacing unrelated settings. The generated configuration
   tells Cargo to use the local `vendor` directory. Keep the vendored files,
   including their `.cargo-checksum.json` files, intact.
5. Verify that the staged library builds with the lockfile and vendored sources:

   ```sh
   cargo build --frozen --lib
   ```

   `--frozen` combines `--locked` and `--offline`: Cargo must neither change the
   lockfile nor fetch dependencies. This still requires a suitable Rust
   toolchain, linker, and native libraries. Vendoring does not include those
   tools or the OpenSSL development headers and libraries; see
   [build requirements](../../00_start_here/01_setup.md#rust-and-native-build-requirements).

You should now have a source directory with the files needed for an internal
EDB library build. Do not omit hidden `.cargo` configuration when copying it.
Build outputs under `target/` are not required in the source distribution.

## Deploy

Deploying the source bundle requires copying the staged EDB source, manifest,
lockfile, `.cargo/config.toml`, and `vendor/` directory to your preferred
restricted storage. If you archive these files for transfer, generate and
distribute a checksum for that archive and verify it after download. For an
archive named `edb-source.tar.gz`, Linux systems can use:

```sh
sha256sum edb-source.tar.gz > edb-source.tar.gz.sha256
sha256sum --check edb-source.tar.gz.sha256
```

Keep build credentials out of the bundle. Copying a vendor directory to an
HTTP server does not make it a Cargo registry: consumers download and unpack
this bundle before building. No repository index is required for a local path
dependency.

An existing private Git repository is another way to make an authorized EDB
source revision available to trusted build environments. Git access credentials
and repository availability remain part of your build infrastructure. Keeping
EDB in Git alone does not mirror its external dependencies.

## Usage

Once the source is available from trusted environments, add it to your
application's `Cargo.toml`. For a downloaded bundle or local checkout next to
the application directory:

```toml
[dependencies]
edb-core = { path = "../EDB" }
```

The path is relative to the application's manifest. If you use an existing
private Git repository instead, replace the illustrative URL and revision:

```toml
[dependencies]
edb-core = { git = "ssh://git@git.example.com/team/EDB.git", rev = "FULL_COMMIT_ID" }
```

Use a full commit ID to select the intended source snapshot. These are Cargo
[path and Git dependencies](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html),
not registry releases. Import the library as `edb_core`; see
[accessing the peer library](../../00_start_here/02_peer_library.md).

For a consuming application, resolve and retain the application's own
`Cargo.lock`. The dependency's lockfile does not control the application's
resolution. To prepare an offline application build, run the gathering steps
from the application root with its lockfile, and retain all local path
dependencies alongside it. Vendoring remote dependencies does not copy a local
path dependency such as `../EDB`.

Place the generated source configuration in the application's `.cargo/config.toml`:
Cargo does not load configuration from a dependency's directory when building
the application. Relative configuration paths follow Cargo's
[configuration rules](https://doc.rust-lang.org/cargo/reference/config.html#config-relative-paths).
Verify the complete application bundle from the application directory:

```sh
cargo build --frozen
```

This check covers the application's selected dependency graph. A successful
standalone EDB library build does not establish that every consuming
application or target platform has all of its build prerequisites.
