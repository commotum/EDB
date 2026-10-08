# Internal library distribution review

Observed situation: the assigned page describes `edb-core` as a private source
snapshot with a lockfile, dependency sources, Cargo source configuration and
native build prerequisites. Its local/Git dependency examples do not expose
retired timestamp or per-operation source forms. `Cargo.toml`, `src/lib.rs`,
`src/model/datom.rs`, `src/transactor/request.rs` and transaction forms agree:
the same library exports six-field datoms and mandatory ordinary request source
without a feature flag, default source adapter or separate package.

Bounded problem: establish that the documented source distribution can retain
the files and dependency resolution needed by the source-aware public library.
There is no observed documentation or implementation gap. Existing release and
application compilation evidence covers unchanged targets; the remaining
question is the vendor/configuration and transfer boundary itself.

Approaches and tradeoffs: adding release automation or changing package metadata
would entangle distribution policy with the database model without an unmet
need. Repeating the optimized release build would add no evidence about local
dependency resolution. Preserve the aligned page and inspect existing public
API/build acceptance, then use an isolated vendor/transfer/offline-resolution
probe if no equivalent evidence already exists. Coordinate artifact checks with
root; no shared code ownership is needed unless an actual gap emerges.

Simplicity assessment: Cargo supplies source acquisition and dependency
selection; the database library supplies source attribution. The consumer's
lockfile/configuration belongs to its build root rather than the dependency's
runtime state. These independent responsibilities compose through an ordinary
path dependency and preserve local reasoning and future changes in storage or
distribution. No compatibility path or attribution-specific distribution layer
is justified.

Acceptance checks: inspect manifest/lockfile/target metadata, SQL embedding and
public API exports; inspect existing passing distribution install, default-suite
and native application results; verify the documented source staging includes
hidden Cargo configuration and SQL, vendor checksums survive archive transfer,
and frozen metadata resolves both the standalone package and a consumer from
their respective roots. Root owns fresh optimized compilation and private
PostgreSQL smoke acceptance. Append exact executed checks and any limitations.

Actual changes and results: assigned page and repository code remain unchanged.
Root confirmed no earlier vendor proof and approved a fresh consuming build.
Created only ignored proof artifacts under
`target/distribution-proof/`, plus review evidence in this directory. Staged the
library source, embedded SQL, examples/tests, manifest, lockfile and docs; recorded
the current source revision, local changes and Rust/Cargo 1.90.0 tool versions in
`distribution-provenance.txt`. No original lockfile, Cargo cache/configuration,
production checkout, runtime or database was changed by this worker.

- Both standalone and consumer `cargo vendor --locked --offline
  --versioned-dirs vendor` completed using cached dependency sources. The
  consumer first resolved its own lockfile. Each root received its generated
  `.cargo/config.toml`; the sibling path dependency remained in the bundle.
- Transferred through an archive and unpacked into a different directory.
  `distribution-transfer.json` records the 32,495,366-byte archive, SHA-256 and
  successful verification of all 16,588 vendored file checksums across 256 crate
  copies. Hidden configurations and embedded SQL survived. The actual documented
  `sha256sum --check` command passed (`distribution-checksum.log`). Dependency
  license/notice files remained intact through those complete vendor checksums.
- A fresh consumer `cargo build --frozen` passed in 35.44s from the restored
  bundle with an empty isolated `CARGO_HOME` and its own target directory
  (`distribution-consumer-frozen-build.log`). The standalone documented
  `cargo build --frozen --lib` also passed from the restored EDB root
  (`distribution-library-frozen-build.log`), sharing only this proof's target
  directory. Both transferred lockfiles remain byte-identical.
- The compiled consumer ran successfully (`distribution-consumer-run.log`;
  source retained in `distribution-consumer.rs`). It imports the public datom,
  request/entity-reference and six index-order APIs. Schema/generated datoms and
  the automatic timestamp share a source established as a same-transaction
  tempid; an empty later transaction resolves that ordinary ident and yields its
  one source-bearing timestamp. This is executable public-library acceptance,
  not a private module fixture or a production database smoke.
- Removing only the restored consumer's configuration caused frozen resolution
  to fail at the empty crates.io cache despite the dependency's configuration
  remaining present (`distribution-missing-consumer-config.log`). Restoring it
  retained the successful bundle. An initial probe expected the first missing
  dependency to be `bigdecimal`; Cargo named `flate2`. The assertion was corrected
  to require the registry-resolution failure rather than an incidental package
  ordering. No library or documentation change followed that probe correction.

Inspected `src/storage/postgres.rs`'s compile-time SQL inclusion,
`src/edn_transaction.rs`'s public source-aware request/speculation constructors,
and `src/transaction/assess/mod.rs`'s ordinary deferred source lowering,
allocation/resolution and uniform material attribution. Existing
`release_distribution_cutover.rs` and `releases-cli-install-live.log` establish
the actual idempotent installed-format output. `tests-integrated-seventh.log`
and strict `clippy-seventh.log` cover unchanged integrated public behavior;
explicit live ignores were not counted as execution. Existing application
build/native workflow logs are supplemental; root owns refreshed optimized
release and live workflow acceptance.

The two local destinations/fragments pass (`distribution-links.json`); assigned
page diff/whitespace checks pass with no changes. The linked primary
[Cargo vendor reference](https://doc.rust-lang.org/cargo/commands/cargo-vendor.html),
[path/Git dependency reference](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html)
and [configuration reference](https://doc.rust-lang.org/cargo/reference/config.html#config-relative-paths)
also confirm the page's vendor, frozen, full-revision, and configuration-root
claims. No private Git remote, target-platform matrix or external distribution
service was exercised; the page assigns those infrastructure choices to its
reader. No unresolved implementation/documentation parity gap remains for this
page. No publish, tag, deployment, commit or compatibility path was introduced.
