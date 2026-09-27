-- Fresh first-release storage. Values are opaque to PostgreSQL.
-- A protection stamp is generic storage metadata, never an interpretation of
-- the immutable payload. Rust supplies and checks the publication/GC protocol.
CREATE TABLE __SCHEMA__.edb_objects (
    id BYTEA PRIMARY KEY CHECK (octet_length(id) = 32),
    payload BYTEA NOT NULL,
    protected_epoch BIGINT NOT NULL DEFAULT 0 CHECK (protected_epoch >= 0)
);
CREATE TABLE __SCHEMA__.edb_refs (
    key TEXT COLLATE "C" PRIMARY KEY,
    revision BIGINT NOT NULL CHECK (revision > 0),
    value BYTEA
);
REVOKE ALL ON __SCHEMA__.edb_objects, __SCHEMA__.edb_refs FROM PUBLIC;
