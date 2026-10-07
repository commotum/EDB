# EDB documentation

EDB stores facts, schema and transactions as database data. Start with the
[introduction](00_start_here/00_introduction.md), follow a tutorial, or use the
reference pages below for syntax, APIs and operational behavior.

## Start here

- [Start with EDB](00_start_here/00_introduction.md)
- [EDB setup](00_start_here/01_setup.md)
- [Accessing the peer library](00_start_here/02_peer_library.md)
- [Language Support](00_start_here/03_language_support.md)
- [EDB releases](00_start_here/04_releases.md)
- [EDB Change Log](00_start_here/05_change_log.md)
- [Release Notices](00_start_here/06_release_notices.md)

## Tutorials

- [EDN data frontend](01_tutorials/00_edn_workflow.md)
- [Run a separate native application](01_tutorials/01_application_workflow.md)
- [In-Memory Database Getting Started](01_tutorials/02_in_memory.md)
- [Peer Tutorial](01_tutorials/03_peer_tutorial.md)
- [Run a Transactor](01_tutorials/04_run_a_transactor.md)
- [Connect to a Database](01_tutorials/05_connect_to_a_database.md)
- [Transact Schema](01_tutorials/06_transact_schema.md)
- [Transact Data](01_tutorials/07_transact_data.md)
- [Query the Data](01_tutorials/08_query_the_data.md)
- [See Historic Data](01_tutorials/09_see_historic_data.md)
- [Client Getting Started](01_tutorials/10_client_workflow.md)

## Core concepts

- [Composing immutable reads](02_core_concepts/00_database_values.md)
- [EDB Data Model](02_core_concepts/01_data_model.md)
- [Programming with Data and EDN](02_core_concepts/02_programming_with_data_and_edn.md)
- [Database Filters](02_core_concepts/03_database_filters.md)
- [Entities](02_core_concepts/04_entities.md)
- [Best practices](02_core_concepts/05_best_practices.md)
- [Glossary](02_core_concepts/06_glossary.md)

## Schema

- [Schema is database data](03_schema/00_schema.md)
- [Value and identity rules](03_schema/01_identity_and_values.md)
- [Schema Data Reference](03_schema/02_schema_reference.md)
- [Changing schema](03_schema/03_changing_schema.md)
- [Data Modeling](03_schema/04_data_modeling.md)
- [Identity and Uniqueness](03_schema/05_identity_and_uniqueness.md)

## Transactions

- [Transactions, reports and exact retries](04_transactions/00_transactions.md)
- [Persisted native queries](04_transactions/01_persisted_programs.md)
- [Partitions and time-local UUIDs](04_transactions/02_partitions.md)
- [Transaction diagnostics](04_transactions/03_diagnostics.md)
- [Transaction Data](04_transactions/04_transaction_data.md)
- [Transaction Model](04_transactions/05_transaction_model.md)
- [Processing Transactions](04_transactions/06_processing_transactions.md)
- [Transaction Functions](04_transactions/07_transaction_functions.md)
- [ACID](04_transactions/08_acid.md)
- [Synchronization](04_transactions/09_synchronization.md)

## Query and Pull

- [Query](05_query_and_pull/00_queries.md)
- [Query application data](05_query_and_pull/01_application_data.md)
- [Pull and entity navigation](05_query_and_pull/02_pull_and_entities.md)
- [Native fulltext search](05_query_and_pull/03_fulltext.md)
- [Query diagnostics](05_query_and_pull/04_diagnostics.md)
- [Query Reference](05_query_and_pull/05_query_reference.md)
- [Pull reference](05_query_and_pull/06_pull_reference.md)
- [Executing Queries](05_query_and_pull/07_executing_queries.md)

## Indexes

- [Ordered indexes and the transaction log](06_indexes/00_indexes_and_log.md)
- [Indexes](06_indexes/01_index_model.md)
- [Background Indexing](06_indexes/02_background_indexing.md)
- [Index-Pull](06_indexes/03_index_pull.md)
- [Index APIs](06_indexes/04_index_apis.md)
- [Forward and reverse seek](06_indexes/05_rseek_datoms.md)

## Peer API

- [Connections and captured reads](07_peer_api/00_connections.md)
- [Client library reference](07_peer_api/01_async_client.md)
- [Native application computation](07_peer_api/02_native_computation.md)
- [Reacting to transactions](07_peer_api/03_change_consumers.md)
- [Native Rust API reference](07_peer_api/04_native_api_reference.md)
- [`edb_core` library](07_peer_api/05_types.md)
- [Log API](07_peer_api/06_log_api.md)
- [Error handling](07_peer_api/07_error_handling.md)
- [I/O Stats](07_peer_api/08_io_stats.md)
- [Synchronous client API](07_peer_api/09_client_api_sync.md)
- [Asynchronous client API](07_peer_api/10_client_api_async.md)
- [Attribute](07_peer_api/types/01_attribute.md)
- [Connection](07_peer_api/types/02_connection.md)
- [DatabaseValue and Database](07_peer_api/types/03_database.md)
- [Database Filter Predicate](07_peer_api/types/04_database_predicate.md)
- [Datom](07_peer_api/types/05_datom.md)
- [Entity](07_peer_api/types/06_entity.md)
- [AsyncOperation and AsyncTransaction](07_peer_api/types/07_async_operation.md)
- [LogValue](07_peer_api/types/08_log.md)
- [Peer and peer entry points](07_peer_api/types/09_peer.md)
- [Query requests and controls](07_peer_api/types/10_query_request.md)
- [Data utilities](07_peer_api/types/11_data_utilities.md)

## Operations

- [PostgreSQL operations](08_operations/00_deployment.md)
- [Administrative CLI](08_operations/01_administration.md)
- [Logical database lifecycle](08_operations/02_database_lifecycle.md)
- [Monitoring and Performance](08_operations/03_observability.md)
- [Prefetch and maintenance controls](08_operations/04_maintenance_controls.md)
- [Query a backup directly](08_operations/05_backup_reads.md)
- [Setting up Storage Services](08_operations/06_storage_services.md)
- [Transactor Reference](08_operations/07_transactor_reference.md)
- [EDB Deployment](08_operations/08_deployment_reference.md)
- [Capacity Planning](08_operations/09_capacity_planning.md)
- [High Availability (HA)](08_operations/10_high_availability.md)
- [Backup and Restore](08_operations/11_backup_and_restore.md)
- [Memory and Caching](08_operations/12_memory_and_caching.md)
- [Configuring logging](08_operations/13_logging.md)
- [Configuration Reference](08_operations/14_configuration_reference.md)

## Optional features and technical notes

- [Optional local SSD cache](09_optional/00_local_cache.md)
- [Reducing latency with transaction hints](09_optional/01_transaction_hints.md)
- [Persistent native log cache](09_optional/02_log_cache.md)
- [Database tools](09_optional/03_database_tools.md)
- [Remote Transactions](09_optional/04_remote_transactions.md)
- [Running on AWS](09_optional/05_running_on_aws.md)
- [AWS Access Control](09_optional/06_aws_access_control.md)
- [Read Only Connections](09_optional/07_read_only_connections.md)
- [Excision](09_optional/08_excision.md)
- [Writing a Problem Report](09_optional/09_problem_reports.md)
- [Comparison with Updating Transactions](09_optional/technical_notes/00_updating_transactions.md)
- [Composing Transactions, by Example](09_optional/technical_notes/01_composing_transactions.md)
- [Distributing the library internally](09_optional/technical_notes/02_distributing_the_library.md)
- [Querying on byte array attributes](09_optional/technical_notes/03_querying_bytes.md)
- [Outer Joins](09_optional/technical_notes/04_outer_joins.md)
