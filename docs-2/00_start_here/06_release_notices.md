# Release Notices

This document is a list of critical release and compatibility notices that all
EDB users should be aware of.

This document **is not** a comprehensive change log. See the
[change log](05_change_log.md) for development status and
[EDB releases](04_releases.md) for the current package and build instructions.

## Current Development Version: 0.1.0

EDB is in first-release development. The `edb-core` package declares version
`0.1.0`, with `publish = false` and license `UNLICENSED`. The peer library and
`edb` executable share this package version. It does not identify a published
release, release date, or compatibility guarantee between development builds.

Build requirements are described in [setup](01_setup.md#rust-and-native-build-requirements).
EDB uses a Rust library and executable; there is no JVM or Clojure runtime
requirement.

## Earlier Development Databases Require Fresh Storage

The current engine reads its current native formats. Earlier development
databases, backups, and checkpoints are not compatible. There is no old-format
reader, administrative upgrade converter, or log downgrade command. Use a fresh
installation and regenerate data with the current interfaces.

The EDB naming update affects the crate (`edb-core`), executable (`edb`),
environment variables (`EDB_*`), storage tables, format markers, and content
hash domains. Renaming tables or changing a format marker does not convert
stored objects. See [provisioning and current storage](../08_operations/00_deployment.md#provisioning-and-current-storage).

`edb install` requires an empty PostgreSQL schema or a matching current
installation. Repeating installation on a matching store is idempotent. It does
not create the PostgreSQL database or schema, reset existing data, or upgrade
an earlier engine. Runtime connections validate the storage format and perform
no DDL.

Storage installation and database compatibility are separate checks. A matching
PostgreSQL storage marker does not establish that the database roots, indexes,
durable log objects, or stored native programs are readable by a different
build. Their formats are checked independently. For example, unsupported
durable encodings report `encoding/unsupported-version`; a namespace with an
incompatible storage marker reports `storage/format` when connecting.

Do not infer a safe rolling upgrade order or downgrade path from the shared
`0.1.0` package version. Use peer libraries and transactors built from the same
source revision, and validate the intended deployment before changing builds.

## Backups and Changes of Build

Test new builds in a staging environment first. We recommend taking a backup
before adopting a new build against an existing database. Follow
[back up and verify](../08_operations/01_administration.md#back-up-and-verify)
and exercise restore into a separately selected destination before relying on
that backup for recovery.

Current backup repositories retain immutable objects in their native formats.
Backup and restore do not convert earlier development formats. Direct backup
reads use the current readers and have the same compatibility boundary. Keep
the source revision and build needed to read a retained backup; creating a
backup does not establish that a newer or older build can read it.

Copy success is not semantic verification. Use the explicit verification
operation described in [backup, verify and restore](../08_operations/00_deployment.md#backup-verify-and-restore).
Ordinary restore authenticates copied objects and checks publication guards;
it does not replay history as a substitute for that verification.

## Recovery and Excision

Current-format index recovery is described in
[explicit index recovery](../08_operations/00_deployment.md#explicit-index-recovery).
`edb consolidate` can reconstruct a damaged or missing current read index from
the canonical log. This does not convert an unsupported database or repair and
certify every retained historical index. Run deep inspection after recovery;
recover damaged retained history from an intact backup.

For removal of data, follow the [excision procedure](../08_operations/00_deployment.md#excision).
A committed excision request is not proof of completed removal; establish
completion with `sync_excise` or the connection's corresponding synchronization.
Discard old held database and log values and synchronize controlled clients
before claiming removal. Excision does not erase independent backups, replicas,
WAL, exports, or already copied process memory. Their retention and deletion
remain separate operations.

## Current Query and Entity-Scan Limitations

A query that joins a variable attribute through `:db/ident` can bind an entity
that is not an attribute and fail with `query/unknown-attribute`, rather than
treating that binding as a nonmatch. Constrain the attribute variable with
`[?a :db/valueType _]` when using this pattern. The
[history example](../02_core_concepts/03_database_filters.md#history) includes
this guard.

`DatabaseValue::entid_at` does not provide a creation-time cutoff for ordinary
entity allocation. It puts the transaction coordinate into the entity-index
field, while ordinary allocation uses an independent frontier; an EAVT scan
from that boundary can include older entities. Use transaction-log data or an
explicit creation marker when selecting newly created entities. See
[new entity scans](../04_transactions/02_partitions.md#new-entity-scans).
