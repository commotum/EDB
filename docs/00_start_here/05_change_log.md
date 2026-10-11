# EDB Change Log

## Read This First

- EDB is in first-release development. There are no published release entries
  recorded here. See [EDB releases](04_releases.md) for the current package and
  build instructions.
- Test new builds in a staging environment first.
- Take a [backup](../08_operations/01_administration.md#back-up-and-verify)
  before adopting a new build against an existing database.

## Current Development Version: 0.1.0

The `edb-core` Cargo package declares version `0.1.0`, with `publish = false`
and license `MIT`. The Rust peer library and `edb` executable share this
package version. It does not identify a published release or a release date.

Current behavior and operating limits are described in the feature references:

- [Peer connections and immutable database values](../07_peer_api/00_connections.md)
- [Schema](../03_schema/02_schema_reference.md) and
  [transaction data](../04_transactions/04_transaction_data.md)
- [Datalog queries](../05_query_and_pull/05_query_reference.md) and
  [Pull](../05_query_and_pull/06_pull_reference.md)
- [Temporal database views](../02_core_concepts/03_database_filters.md)
- [Index access](../06_indexes/04_index_apis.md)
- [PostgreSQL storage and transactor operation](../08_operations/00_deployment.md)

For storage-format requirements when changing builds, see
[provisioning and current storage](../08_operations/00_deployment.md#provisioning-and-current-storage).
