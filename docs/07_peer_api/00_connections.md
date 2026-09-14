# Connections and captured reads

`Connection::connect_configured(postgres_config, database_name, cache_entries)`
opens an existing logical database. `Peer` is the lower-level read handle;
`Database::bootstrap` is useful for pure in-memory fixtures. Live startup does
not install storage, create databases or start an attached writer implicitly.
See the [application setup](../01_tutorials/01_application_workflow.md).

`Connection::db()` captures the resident immutable value without I/O. Pass that
value to query, Pull, entity and index APIs. Later commits or connection
advancement cannot change it; a cold read can still need PostgreSQL.
The connection resolves its public name once to stable identity, so catalog
rename or name reuse cannot redirect an existing handle.

`sync` observes durable progress; `sync_to` waits for a specified logical basis.
`sync_index`, `sync_schema` and `sync_excise` wait for their respective physical
or maintenance completion conditions. Use finite targets/deadlines deliberately.
Background notices only prompt authenticated catch-up and are not commit data.

An exact `SnapshotReference` reopens an authorized retained witness, not whichever
head happens to be latest. It is neither a credential nor a retention pin.
Retirement, excision or collection can make reopening fail. `SnapshotKey` is a
logical committed-view key, not a universal query/fulltext result-cache key.
See [immutable values](../02_core_concepts/00_database_values.md).

For executor threads use [AsyncClient](01_async_client.md), which delegates
blocking work to bounded workers. For application logic use
[native computation](02_native_computation.md). Transaction report queues are
unbounded in-memory observations; [ChangeConsumer](03_change_consumers.md) adds
one-at-a-time log delivery and explicit durable acknowledgments without promising
exactly-once external effects or retention pins.

Live storage, transaction transport and native callbacks have distinct authority
boundaries. A remote transaction token does not grant PostgreSQL reads. Use
[backup connections](../08_operations/05_backup_reads.md) for source-independent
offline query, Pull, history, log and speculative reads.
