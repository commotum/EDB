use super::{
    BatchOutcome, CasOutcome, Guarded, MAX_REF_BYTES, ObjectId, RefChange, RefCondition, Reference,
    root::MAX_BLOCK_BYTES, validate_key, validate_limit,
};
use crate::sql_io::{OperationContext, SqlClient, SqlTransaction};
use crate::{ErrorCategory, PostgresConnectionConfig, SemanticError, sha256};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

#[path = "object_batch.rs"]
mod object_batch;

const FORMAT: &[u8] = b"edb/opaque-storage/2";

/// Actual payload reads on this connection, including re-put authentication.
/// Physical bytes are transferred payload bytes, not PostgreSQL disk usage;
/// canonical bytes and representation counts include successful decodes only.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ObjectReadStats {
    pub physical_bytes: u64,
    pub canonical_bytes: u64,
    pub compressed_hits: u64,
    pub raw_reads: u64,
    /// Wall-clock decoding/authentication time, excluding PostgreSQL wait.
    pub decode_nanos: u64,
}

/// Generic object accounting; no payload or engine metadata is decoded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectInfo {
    pub id: ObjectId,
    pub protected_epoch: u64,
    pub stored_bytes: u64,
}

/// Rust-selected authority for immutable writes within one publication attempt.
/// The provider treats every reference as opaque; callers supply writer/GC
/// guards and choose the protection epoch under their publication protocol.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WriteProtection {
    pub epoch: u64,
    pub conditions: Vec<RefCondition>,
}
impl WriteProtection {
    fn validate(&self) -> Result<(), SemanticError> {
        sql_epoch(self.epoch)?;
        super::protocol::validate_batch(&self.conditions, &[])
    }
}

/// A connection to one installed namespace. It performs no runtime DDL and never
/// discovers a legacy engine. Explicit installation requires an empty namespace.
pub struct PgBlockStore {
    client: SqlClient,
    objects: String,
    refs: String,
    namespace: String,
    write_protection: Option<Arc<WriteProtection>>,
    object_read_stats: ObjectReadStats,
    object_cache: Option<OpaqueReadCache>,
}

/// Optional bounded read-through cache of immutable bytes, scoped to an engine
/// operation. No reference values or engine-specific metadata are interpreted.
#[derive(Default)]
struct OpaqueReadCache {
    entries: std::collections::BTreeMap<ObjectId, Vec<u8>>,
    order: std::collections::VecDeque<ObjectId>,
    bytes: usize,
    budget: usize,
}
impl OpaqueReadCache {
    fn remember(&mut self, id: ObjectId, bytes: &[u8]) {
        let cost = bytes.len().saturating_add(96);
        if cost > self.budget || bytes.len() > 4096 || self.entries.contains_key(&id) {
            return;
        }
        while self.bytes.saturating_add(cost) > self.budget {
            let Some(old) = self.order.pop_front() else {
                break;
            };
            if let Some(value) = self.entries.remove(&old) {
                self.bytes -= value.len() + 96;
            }
        }
        self.bytes += cost;
        self.entries.insert(id, bytes.to_vec());
        self.order.push_back(id);
    }
}

impl PgBlockStore {
    /// The explicitly selected, fully qualified namespace for this connection.
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Check the current SQL principal's generic destructive-storage
    /// capability. Engine administration uses this without teaching PostgreSQL
    /// any catalog/lifecycle policy. It is not a sandbox for raw ref clients.
    pub(crate) fn require_object_delete_privilege(&mut self) -> Result<(), SemanticError> {
        let allowed: bool = self
            .client
            .query_one(
                "SELECT pg_catalog.has_table_privilege($1::text, 'DELETE')",
                &[&self.objects],
            )
            .map_err(read_error)?
            .get(0);
        if !allowed {
            return Err(SemanticError::new(
                ErrorCategory::Forbidden,
                "storage/operator-required",
                "This operation requires administrative object-deletion authority",
            ));
        }
        Ok(())
    }

    /// Add provider-level runtime grants to pre-existing dedicated roles.
    /// Writers may insert/protect objects, but only operators may delete them.
    /// Peers read objects; both roles maintain opaque references, including
    /// ephemeral reference cleanup. These are trusted storage credentials, not
    /// per-database authorization. Existing grants and role memberships can
    /// broaden access; this helper neither audits nor revokes them.
    pub fn grant_runtime_privileges(
        &mut self,
        writer_role: &str,
        peer_role: &str,
    ) -> Result<(), SemanticError> {
        if writer_role == peer_role {
            return Err(SemanticError::incorrect(
                "storage/runtime-roles-not-distinct",
                "Writer and peer roles must be distinct",
            ));
        }
        for role in [writer_role, peer_role] {
            if role.is_empty() || role.contains('\0') {
                return Err(SemanticError::incorrect(
                    "storage/invalid-role-name",
                    "Role names must be nonempty and contain no NUL",
                ));
            }
        }
        let mut tx = self.client.transaction().map_err(read_error)?;
        for role in [writer_role, peer_role] {
            let row = tx.query_opt(
                "SELECT r.rolsuper OR r.rolcreaterole OR r.rolcreatedb OR r.rolreplication OR r.rolbypassrls OR EXISTS (SELECT 1 FROM pg_catalog.pg_namespace n WHERE n.nspname=$2 AND n.nspowner=r.oid) OR EXISTS (SELECT 1 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=$2 AND c.relname IN ('edb_objects','edb_refs') AND c.relowner=r.oid) FROM pg_catalog.pg_roles r WHERE r.rolname=$1",
                &[&role, &self.namespace],
            ).map_err(read_error)?.ok_or_else(|| SemanticError::new(
                ErrorCategory::NotFound,
                "storage/runtime-role-not-found",
                "Runtime role does not exist",
            ))?;
            if row.get::<_, bool>(0) {
                return Err(SemanticError::incorrect(
                    "storage/runtime-role-elevated",
                    "Runtime roles must not be elevated or own the storage namespace or tables",
                ));
            }
        }
        let database: String = tx
            .query_one("SELECT current_database()", &[])
            .map_err(read_error)?
            .get(0);
        let writer = quote(writer_role);
        let peer = quote(peer_role);
        let namespace = quote(&self.namespace);
        let database = quote(&database);
        tx.batch_execute(&format!(
            "GRANT CONNECT ON DATABASE {database} TO {writer}, {peer}; \
             GRANT USAGE ON SCHEMA {namespace} TO {writer}, {peer}; \
             GRANT SELECT, INSERT, UPDATE ON TABLE {} TO {writer}; \
             GRANT SELECT ON TABLE {} TO {peer}; \
             GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE {} TO {writer}; \
             GRANT SELECT ON TABLE {} TO {peer}",
            self.objects, self.objects, self.refs, self.refs,
        ))
        .map_err(read_error)?;
        tx.commit().map_err(commit_error)
    }

    pub(crate) fn reset_object_cache(&mut self, budget: usize) {
        self.object_cache = (budget != 0).then(|| OpaqueReadCache {
            budget,
            ..Default::default()
        });
    }
    /// The authenticated PostgreSQL principal for application-level namespaces.
    /// This is connection identity, not an engine-specific authorization rule.
    pub(crate) fn principal(&mut self) -> Result<String, SemanticError> {
        self.client
            .query_one("SELECT current_user::text", &[])
            .map(|row| row.get(0))
            .map_err(read_error)
    }
    /// A generic, namespace-isolated advisory wakeup address. Neither the
    /// channel nor the provider knows what this reference means to the engine.
    pub(crate) fn notification_channel(&self, key: &str) -> Result<String, SemanticError> {
        validate_key(key)?;
        let mut bytes = self.namespace.as_bytes().to_vec();
        bytes.push(0);
        bytes.extend_from_slice(key.as_bytes());
        let hash = sha256(&bytes);
        Ok(format!(
            "edb_r_{}",
            hash[..24]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        ))
    }

    pub(crate) fn notify_reference(&mut self, key: &str) -> Result<(), SemanticError> {
        let channel = self.notification_channel(key)?;
        self.client
            .query_one("SELECT pg_catalog.pg_notify($1, '')", &[&channel])
            .map_err(read_error)?;
        Ok(())
    }

    pub fn install(config: &PostgresConnectionConfig) -> Result<(), SemanticError> {
        let mut client = config.connect_for("storage/install-connect")?;
        let namespace = storage_namespace(&mut client)?;
        let quoted = quote(&namespace);
        let mut tx = client
            .build_transaction()
            .isolation_level(postgres::IsolationLevel::ReadCommitted)
            .start()
            .map_err(read_error)?;
        let lock = lock_id(&namespace, "storage/installation");
        tx.query_one("SELECT pg_catalog.pg_advisory_xact_lock($1)", &[&lock])
            .map_err(read_error)?;
        let names = tx.query(
            "SELECT c.relname::text FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=$1 ORDER BY c.relname",
            &[&namespace],
        ).map_err(read_error)?.into_iter().map(|r| r.get::<_, String>(0)).collect::<Vec<_>>();
        if !names.is_empty() {
            let expected = [
                "edb_objects",
                "edb_objects_pkey",
                "edb_refs",
                "edb_refs_pkey",
            ];
            if names.iter().map(String::as_str).eq(expected) {
                let row = tx
                    .query_opt(
                        &format!(
                            "SELECT value=$1 FROM {quoted}.edb_refs WHERE key='system/format'"
                        ),
                        &[&FORMAT],
                    )
                    .map_err(read_error)?;
                if row.and_then(|r| r.get::<_, Option<bool>>(0)) == Some(true) {
                    tx.rollback().map_err(read_error)?;
                    return Ok(());
                }
            }
            return Err(SemanticError::incorrect(
                "storage/schema-not-empty",
                "Storage installation requires an empty namespace or the current opaque-store format",
            ));
        }
        // Reject namespace-owned routines as well; never take over a preexisting
        // application's schema just because it has no tables.
        if tx.query_one("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname=$1)", &[&namespace]).map_err(read_error)?.get::<_, bool>(0) {
            return Err(SemanticError::incorrect("storage/schema-not-empty", "Storage namespace contains existing routines"));
        }
        tx.batch_execute(&include_str!("schema.sql").replace("__SCHEMA__", &quoted))
            .map_err(read_error)?;
        tx.execute(
            &format!(
                "INSERT INTO {quoted}.edb_refs(key,revision,value) VALUES ('system/format',1,$1)"
            ),
            &[&FORMAT],
        )
        .map_err(read_error)?;
        tx.commit().map_err(commit_error)
    }

    pub fn connect(config: &PostgresConnectionConfig) -> Result<Self, SemanticError> {
        let mut client = config.connect_for("storage/connect")?;
        let namespace = storage_namespace(&mut client)?;
        let objects = format!("{}.edb_objects", quote(&namespace));
        let refs = format!("{}.edb_refs", quote(&namespace));
        let row = client
            .query_opt(
                &format!("SELECT value=$1 FROM {refs} WHERE key='system/format'"),
                &[&FORMAT],
            )
            .map_err(read_error)?;
        if row.and_then(|r| r.get::<_, Option<bool>>(0)) != Some(true) {
            return Err(SemanticError::incorrect(
                "storage/format",
                "Namespace is not the current opaque storage format",
            ));
        }
        Ok(Self {
            client,
            objects,
            refs,
            namespace,
            write_protection: None,
            object_read_stats: ObjectReadStats::default(),
            object_cache: None,
        })
    }

    /// Set the authority used by ordinary `put` calls for this attempt. Invalid
    /// replacements leave the previous context intact. Clear it explicitly for
    /// unprotected fixture/admin work; this is not a writer or GC policy engine.
    pub fn set_write_protection(
        &mut self,
        protection: Option<WriteProtection>,
    ) -> Result<(), SemanticError> {
        if let Some(protection) = &protection {
            protection.validate()?;
        }
        self.write_protection = protection.map(Arc::new);
        Ok(())
    }

    pub fn write_protection(&self) -> Option<&WriteProtection> {
        self.write_protection.as_deref()
    }

    pub fn object_read_stats(&self) -> ObjectReadStats {
        self.object_read_stats
    }

    /// Idempotent content-addressed insert. Never overwrites an existing payload.
    /// With a configured write context, both new and reused objects receive its
    /// epoch protection under all supplied guards. Lost guards return Conflict.
    /// Without a context this is the unprotected fixture/admin primitive.
    /// After a lost acknowledgement, authenticated `get(id)` proves content
    /// presence only. With protection enabled, repeat the guarded operation:
    /// a pre-existing object does not prove this attempt refreshed its epoch.
    pub fn put(&mut self, payload: &[u8]) -> Result<ObjectId, SemanticError> {
        if let Some(protection) = self.write_protection.clone() {
            return match self.put_protected(payload, protection.epoch, &protection.conditions)? {
                Guarded::Applied(id) => Ok(id),
                Guarded::Conflict(_) => Err(SemanticError::conflict(
                    "storage/write-protection-conflict",
                    "Immutable write authority changed during the publication attempt",
                )),
            };
        }
        validate_payload(payload)?;
        let id = sha256(payload);
        let physical = crate::storage::codec::encode_block(payload)?;
        self.client
            .execute(
                &format!(
                    "INSERT INTO {}(id,payload) VALUES ($1,$2) ON CONFLICT(id) DO NOTHING",
                    self.objects
                ),
                &[&&id[..], &&physical[..]],
            )
            .map_err(commit_error)?;
        record_write(physical.len());
        if self.get(id)?.as_deref() != Some(payload) {
            return Err(corrupt());
        }
        Ok(id)
    }

    /// Indivisibly checks opaque reference guards and protects the inserted OR
    /// reused object against older collection epochs. Even deduplicated objects
    /// must be touched: finding an old orphan does not protect it from sweeping.
    pub fn put_protected(
        &mut self,
        payload: &[u8],
        epoch: u64,
        conditions: &[RefCondition],
    ) -> Result<Guarded<ObjectId>, SemanticError> {
        validate_payload(payload)?;
        super::protocol::validate_batch(conditions, &[])?;
        let epoch = sql_epoch(epoch)?;
        let id = sha256(payload);
        let physical = crate::storage::codec::encode_block(payload)?;
        let mut tx = self
            .client
            .build_transaction()
            .isolation_level(postgres::IsolationLevel::ReadCommitted)
            .start()
            .map_err(read_error)?;
        let current = guarded_refs(&mut tx, &self.namespace, &self.refs, conditions)?;
        if conditions
            .iter()
            .any(|guard| current[&guard.key].as_ref().map(|r| r.revision) != guard.expected)
        {
            tx.rollback().map_err(read_error)?;
            return Ok(Guarded::Conflict(current.into_iter().collect()));
        }
        // Representation is not identity: a valid raw object and an adaptive
        // gzip envelope may name the same canonical bytes. Keep the existing
        // payload immutable, return it bounded, then authenticate before commit.
        let row = tx.query_one(&format!("INSERT INTO {} AS objects(id,payload,protected_epoch) VALUES ($1,$2,$3) ON CONFLICT(id) DO UPDATE SET protected_epoch=GREATEST(objects.protected_epoch,EXCLUDED.protected_epoch) RETURNING CASE WHEN octet_length(objects.payload) <= $4::bigint THEN objects.payload ELSE NULL END", self.objects), &[&&id[..], &&physical[..], &epoch, &(MAX_BLOCK_BYTES as i64)]).map_err(read_error)?;
        let existing: Option<Vec<u8>> = row.get(0);
        let Some(existing) = existing else {
            return Err(corrupt());
        };
        let decoded = decode_objects(&[id], vec![Some(existing)], &mut self.object_read_stats)?;
        if decoded[0].as_deref() != Some(payload) {
            return Err(corrupt());
        }
        tx.commit().map_err(commit_error)?;
        record_write(physical.len());
        Ok(Guarded::Applied(id))
    }

    /// Generic guarded deletion. Rust selects unreachable IDs and a safe epoch;
    /// this operation only checks authority revisions and numeric protection.
    /// A stale collector cannot delete after the engine replaces its guard.
    pub fn remove_unprotected(
        &mut self,
        ids: &[ObjectId],
        before_epoch: u64,
        conditions: &[RefCondition],
    ) -> Result<Guarded<usize>, SemanticError> {
        if !ids.is_empty() {
            validate_limit(ids.len())?;
        }
        super::protocol::validate_batch(conditions, &[])?;
        let epoch = sql_epoch(before_epoch)?;
        let keys: Vec<&[u8]> = ids.iter().map(|id| &id[..]).collect();
        let mut tx = self
            .client
            .build_transaction()
            .isolation_level(postgres::IsolationLevel::ReadCommitted)
            .start()
            .map_err(read_error)?;
        let current = guarded_refs(&mut tx, &self.namespace, &self.refs, conditions)?;
        if conditions
            .iter()
            .any(|guard| current[&guard.key].as_ref().map(|r| r.revision) != guard.expected)
        {
            tx.rollback().map_err(read_error)?;
            return Ok(Guarded::Conflict(current.into_iter().collect()));
        }
        let removed = tx
            .execute(
                &format!(
                    "DELETE FROM {} WHERE id=ANY($1::bytea[]) AND protected_epoch<$2",
                    self.objects
                ),
                &[&keys, &epoch],
            )
            .map_err(read_error)?;
        tx.commit().map_err(commit_error)?;
        if let Some(cache) = &mut self.object_cache {
            for id in ids {
                if let Some(bytes) = cache.entries.remove(id) {
                    cache.bytes -= bytes.len() + 96;
                }
            }
            cache.order.retain(|id| cache.entries.contains_key(id));
        }
        Ok(Guarded::Applied(removed as usize))
    }

    pub fn get(&mut self, id: ObjectId) -> Result<Option<Vec<u8>>, SemanticError> {
        if let Some(bytes) = self
            .object_cache
            .as_ref()
            .and_then(|cache| cache.entries.get(&id))
        {
            return Ok(Some(bytes.clone()));
        }
        let mut values = self.get_many(&[id])?;
        let value = values.remove(0);
        if let (Some(cache), Some(bytes)) = (&mut self.object_cache, &value) {
            cache.remember(id, bytes);
        }
        Ok(value)
    }

    pub(crate) fn object_exists(&mut self, id: ObjectId) -> Result<bool, SemanticError> {
        self.client
            .query_one(
                &format!("SELECT EXISTS(SELECT 1 FROM {} WHERE id=$1)", self.objects),
                &[&&id[..]],
            )
            .map(|row| row.get(0))
            .map_err(read_error)
    }

    /// Preserves input order and duplicates. Both count and aggregate result
    /// physical payload are bounded before the driver receives object bytes.
    /// Aggregate declared canonical output is separately admitted before any
    /// decompression, counting duplicate requested objects as duplicate output.
    pub fn get_many(&mut self, ids: &[ObjectId]) -> Result<Vec<Option<Vec<u8>>>, SemanticError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        validate_limit(ids.len())?;
        let keys: Vec<&[u8]> = ids.iter().map(|id| &id[..]).collect();
        let cap = MAX_BLOCK_BYTES as i64;
        let rows = self.client.query(&format!(
            "WITH selected AS (SELECT wanted.ord,o.payload FROM unnest($1::bytea[]) WITH ORDINALITY wanted(id,ord) LEFT JOIN {} o ON o.id=wanted.id) SELECT payload IS NOT NULL, CASE WHEN sum(octet_length(payload)::bigint) OVER () <= $2::bigint THEN payload ELSE NULL END FROM selected ORDER BY ord", self.objects), &[&keys, &cap]).map_err(read_error)?;
        let physical = rows
            .into_iter()
            .map(|row| {
                let present: bool = row.get(0);
                let bytes: Option<Vec<u8>> = row.get(1);
                if present && bytes.is_none() {
                    return Err(SemanticError::incorrect(
                        "storage/read-budget",
                        "Object batch exceeds the 64 MiB result budget; split the batch",
                    ));
                }
                Ok(bytes)
            })
            .collect::<Result<Vec<_>, SemanticError>>()?;
        decode_objects(ids, physical, &mut self.object_read_stats)
    }

    pub fn read_ref(&mut self, key: &str) -> Result<Option<Reference>, SemanticError> {
        validate_key(key)?;
        self.client.query_opt(&format!("SELECT revision,CASE WHEN octet_length(value) <= $2 THEN value ELSE NULL END,value IS NOT NULL FROM {} WHERE key=$1", self.refs), &[&key, &(MAX_REF_BYTES as i32)]).map_err(read_error)?.map(decode_ref).transpose()
    }

    pub fn compare_exchange(
        &mut self,
        key: &str,
        expected: Option<u64>,
        value: Option<&[u8]>,
    ) -> Result<CasOutcome, SemanticError> {
        validate_key(key)?;
        if value.is_some_and(|v| v.len() > MAX_REF_BYTES) {
            return Err(SemanticError::incorrect(
                "storage/reference-size",
                "Reference payload exceeds the 1 MiB bound",
            ));
        }
        let condition = RefCondition {
            key: key.to_owned(),
            expected,
        };
        let change = RefChange {
            key: key.to_owned(),
            value: value.map(<[u8]>::to_vec),
        };
        match self.compare_exchange_many(&[condition], &[change])? {
            BatchOutcome::Applied(mut values) => Ok(CasOutcome::Applied(values.remove(0).1)),
            BatchOutcome::Conflict(mut values) => Ok(CasOutcome::Conflict(values.remove(0).1)),
        }
    }

    /// Short generic transaction: lock guard keys in deterministic order, compare
    /// their revisions in Rust, then replace the requested opaque reference bytes.
    /// Advisory locks also serialize guards for as-yet absent keys. No engine
    /// state or maintenance policy is interpreted by SQL.
    pub fn compare_exchange_many(
        &mut self,
        conditions: &[RefCondition],
        changes: &[RefChange],
    ) -> Result<BatchOutcome, SemanticError> {
        super::protocol::validate_batch(conditions, changes)?;
        let mut tx = self
            .client
            .build_transaction()
            .isolation_level(postgres::IsolationLevel::ReadCommitted)
            .start()
            .map_err(read_error)?;
        let current = guarded_refs(&mut tx, &self.namespace, &self.refs, conditions)?;
        if conditions
            .iter()
            .any(|guard| current[&guard.key].as_ref().map(|r| r.revision) != guard.expected)
        {
            tx.rollback().map_err(read_error)?;
            return Ok(BatchOutcome::Conflict(current.into_iter().collect()));
        }
        let mut applied = Vec::with_capacity(changes.len());
        for change in changes {
            let revision = current[&change.key].as_ref().map_or(1, |r| r.revision + 1);
            let sql_revision = i64::try_from(revision).map_err(|_| {
                SemanticError::new(
                    ErrorCategory::Fault,
                    "storage/revision-exhausted",
                    "Reference revision is exhausted",
                )
            })?;
            let prior_revision = sql_revision - 1;
            let changed = tx.execute(&format!("INSERT INTO {} AS refs(key,revision,value) VALUES ($1,$2,$3) ON CONFLICT(key) DO UPDATE SET revision=EXCLUDED.revision,value=EXCLUDED.value WHERE refs.revision=$4", self.refs), &[&change.key, &sql_revision, &change.value, &prior_revision]).map_err(read_error)?;
            if changed != 1 {
                return Err(SemanticError::conflict(
                    "storage/reference-conflict",
                    "Reference changed outside the guarded storage protocol",
                ));
            }
            applied.push((
                change.key.clone(),
                Reference {
                    revision,
                    value: change.value.clone(),
                },
            ));
        }
        tx.commit().map_err(commit_error)?;
        for change in changes {
            record_write(change.value.as_ref().map_or(0, Vec::len));
        }
        Ok(BatchOutcome::Applied(applied))
    }

    pub fn list_objects(
        &mut self,
        after: Option<ObjectId>,
        limit: usize,
    ) -> Result<Vec<ObjectId>, SemanticError> {
        validate_limit(limit)?;
        let after = after.as_ref().map(|id| &id[..]);
        self.client
            .query(
                &format!(
                    "SELECT id FROM {} WHERE ($1::bytea IS NULL OR id>$1) ORDER BY id LIMIT $2",
                    self.objects
                ),
                &[&after, &(limit as i64)],
            )
            .map_err(read_error)?
            .into_iter()
            .map(|r| r.get::<_, Vec<u8>>(0).try_into().map_err(|_| corrupt()))
            .collect()
    }

    pub fn list_object_info(
        &mut self,
        after: Option<ObjectId>,
        limit: usize,
    ) -> Result<Vec<ObjectInfo>, SemanticError> {
        validate_limit(limit)?;
        let after = after.as_ref().map(|id| &id[..]);
        self.client.query(&format!(
            "SELECT id,protected_epoch,octet_length(payload)::bigint FROM {} WHERE ($1::bytea IS NULL OR id>$1) ORDER BY id LIMIT $2", self.objects),
            &[&after, &(limit as i64)]).map_err(read_error)?.into_iter().map(|row| {
                Ok(ObjectInfo { id: row.get::<_,Vec<u8>>(0).try_into().map_err(|_| corrupt())?,
                    protected_epoch: row.get::<_,i64>(1) as u64, stored_bytes: row.get::<_,i64>(2) as u64 })
            }).collect()
    }

    /// Raise numeric protection without transferring immutable payloads. Used
    /// for engine-owned metadata after Rust has authenticated its child links.
    pub(crate) fn protect_existing(
        &mut self,
        ids: &[ObjectId],
        epoch: u64,
        conditions: &[RefCondition],
    ) -> Result<Guarded<usize>, SemanticError> {
        if ids.is_empty() {
            return Ok(Guarded::Applied(0));
        }
        validate_limit(ids.len())?;
        super::protocol::validate_batch(conditions, &[])?;
        let epoch = sql_epoch(epoch)?;
        let keys: Vec<&[u8]> = ids.iter().map(|id| &id[..]).collect();
        let mut tx = self
            .client
            .build_transaction()
            .isolation_level(postgres::IsolationLevel::ReadCommitted)
            .start()
            .map_err(read_error)?;
        let current = guarded_refs(&mut tx, &self.namespace, &self.refs, conditions)?;
        if conditions
            .iter()
            .any(|g| current[&g.key].as_ref().map(|r| r.revision) != g.expected)
        {
            tx.rollback().map_err(read_error)?;
            return Ok(Guarded::Conflict(current.into_iter().collect()));
        }
        let count = tx.execute(&format!("UPDATE {} SET protected_epoch=GREATEST(protected_epoch,$2) WHERE id=ANY($1::bytea[])", self.objects), &[&keys,&epoch]).map_err(read_error)?;
        if count as usize != ids.iter().collect::<std::collections::BTreeSet<_>>().len() {
            return Err(corrupt());
        }
        tx.commit().map_err(commit_error)?;
        Ok(Guarded::Applied(count as usize))
    }

    pub fn list_refs(
        &mut self,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, Reference)>, SemanticError> {
        validate_limit(limit)?;
        if let Some(key) = after {
            validate_key(key)?;
        }
        self.client.query(&format!("SELECT revision,CASE WHEN octet_length(value) <= $3 THEN value ELSE NULL END,value IS NOT NULL,key FROM {} WHERE ($1::text IS NULL OR key COLLATE \"C\">$1 COLLATE \"C\") ORDER BY key COLLATE \"C\" LIMIT $2", self.refs), &[&after, &(limit as i64), &(MAX_REF_BYTES as i32)]).map_err(read_error)?.into_iter().map(|r| {
            let key = r.get(3); Ok((key, decode_ref(r)?))
        }).collect()
    }

    /// Bounded opaque prefix enumeration. Rust owns prefix meaning and paging;
    /// byte order is independent of the database's configured text collation.
    pub fn list_live_refs(
        &mut self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, Reference)>, SemanticError> {
        validate_limit(limit)?;
        if !prefix.is_empty() {
            validate_key(prefix)?;
        }
        if let Some(after) = after {
            validate_key(after)?;
        }
        let mut pattern = String::new();
        for c in prefix.chars() {
            if matches!(c, '%' | '_' | '\\') {
                pattern.push('\\');
            }
            pattern.push(c);
        }
        pattern.push('%');
        self.client.query(&format!(
            "SELECT revision,CASE WHEN octet_length(value)<=$4 THEN value ELSE NULL END,value IS NOT NULL,key FROM {} WHERE value IS NOT NULL AND key COLLATE \"C\" LIKE $1 ESCAPE E'\\\\' AND ($2::text IS NULL OR key COLLATE \"C\">$2 COLLATE \"C\") ORDER BY key COLLATE \"C\" LIMIT $3", self.refs),
            &[&pattern, &after, &(limit as i64), &(MAX_REF_BYTES as i32)]
        ).map_err(read_error)?.into_iter().map(|row| {
            let key = row.get(3); Ok((key, decode_ref(row)?))
        }).collect()
    }

    /// Forget one-use coordination keys after their owner has revoked them.
    /// Unlike ordinary CAS tombstones this is physical removal: the engine must
    /// guarantee these tokens are never reused, and guard any late creator by
    /// a displaced collection epoch. Public reusable keys must retain tombstones.
    pub(crate) fn forget_ephemeral_refs(
        &mut self,
        keys: &[String],
        conditions: &[RefCondition],
    ) -> Result<Guarded<usize>, SemanticError> {
        if keys.is_empty() {
            return Ok(Guarded::Applied(0));
        }
        validate_limit(keys.len())?;
        super::protocol::validate_batch(conditions, &[])?;
        for key in keys {
            validate_key(key)?;
            if !conditions
                .iter()
                .any(|g| &g.key == key && g.expected.is_some())
            {
                return Err(SemanticError::incorrect(
                    "storage/unguarded-removal",
                    "Ephemeral removal requires each exact existing revision",
                ));
            }
        }
        let mut tx = self
            .client
            .build_transaction()
            .isolation_level(postgres::IsolationLevel::ReadCommitted)
            .start()
            .map_err(read_error)?;
        let current = guarded_refs(&mut tx, &self.namespace, &self.refs, conditions)?;
        if conditions
            .iter()
            .any(|g| current[&g.key].as_ref().map(|r| r.revision) != g.expected)
        {
            tx.rollback().map_err(read_error)?;
            return Ok(Guarded::Conflict(current.into_iter().collect()));
        }
        let count = tx
            .execute(
                &format!("DELETE FROM {} WHERE key=ANY($1::text[])", self.refs),
                &[&keys],
            )
            .map_err(read_error)?;
        tx.commit().map_err(commit_error)?;
        Ok(Guarded::Applied(count as usize))
    }

    /// Administrative primitive, not a garbage collector. The engine must prove
    /// reachability/authority; live collection uses guarded conditional removal.
    pub fn remove_objects(&mut self, ids: &[ObjectId]) -> Result<usize, SemanticError> {
        if ids.is_empty() {
            return Ok(0);
        }
        validate_limit(ids.len())?;
        let keys: Vec<&[u8]> = ids.iter().map(|id| &id[..]).collect();
        self.client
            .execute(
                &format!("DELETE FROM {} WHERE id=ANY($1::bytea[])", self.objects),
                &[&keys],
            )
            .map(|n| n as usize)
            .map_err(commit_error)
    }
}

fn guarded_refs(
    tx: &mut SqlTransaction<'_>,
    namespace: &str,
    refs: &str,
    conditions: &[RefCondition],
) -> Result<BTreeMap<String, Option<Reference>>, SemanticError> {
    let mut locks: Vec<i64> = conditions
        .iter()
        .map(|c| lock_id(namespace, &c.key))
        .collect();
    locks.sort_unstable();
    locks.dedup();
    for lock in locks {
        tx.query_one("SELECT pg_catalog.pg_advisory_xact_lock($1)", &[&lock])
            .map_err(read_error)?;
    }
    let mut current = BTreeMap::new();
    for guard in conditions {
        let value = tx.query_opt(&format!("SELECT revision,CASE WHEN octet_length(value) <= $2 THEN value ELSE NULL END,value IS NOT NULL FROM {refs} WHERE key=$1 FOR UPDATE"), &[&guard.key, &(MAX_REF_BYTES as i32)]).map_err(read_error)?.map(decode_ref).transpose()?;
        current.insert(guard.key.clone(), value);
    }
    Ok(current)
}

fn decode_ref(row: postgres::Row) -> Result<Reference, SemanticError> {
    let revision: i64 = row.get(0);
    let value: Option<Vec<u8>> = row.get(1);
    if revision <= 0 || (row.get::<_, bool>(2) && value.is_none()) {
        return Err(SemanticError::new(
            ErrorCategory::Fault,
            "storage/reference-corrupt",
            "Reference revision or payload exceeds the current format bounds",
        ));
    }
    if let Some(value) = &value {
        record_read(value.len());
    }
    Ok(Reference {
        revision: revision as u64,
        value,
    })
}

fn storage_namespace(client: &mut SqlClient) -> Result<String, SemanticError> {
    let namespace: Option<String> = client
        .query_one("SELECT pg_catalog.current_schema()::text", &[])
        .map_err(read_error)?
        .get(0);
    let namespace = namespace
        .filter(|n| !n.starts_with("pg_") && n != "information_schema")
        .ok_or_else(|| {
            SemanticError::incorrect(
                "storage/namespace",
                "Select a non-system PostgreSQL namespace explicitly",
            )
        })?;
    // Fully qualified table names plus pg_catalog first prevent temp/function
    // shadowing. Never interpolate an unquoted namespace from configuration.
    let path = format!("pg_catalog, {}, pg_temp", quote(&namespace));
    client
        .query_one(
            "SELECT pg_catalog.set_config('search_path',$1,false)",
            &[&path],
        )
        .map_err(read_error)?;
    Ok(namespace)
}

fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}
fn lock_id(namespace: &str, key: &str) -> i64 {
    let digest = sha256(format!("edb-storage\0{namespace}\0{key}").as_bytes());
    i64::from_be_bytes(digest[..8].try_into().unwrap())
}
fn validate_payload(payload: &[u8]) -> Result<(), SemanticError> {
    if payload.len() > MAX_BLOCK_BYTES {
        return Err(SemanticError::incorrect(
            "storage/object-size",
            "An object exceeds the current 64 MiB format bound",
        ));
    }
    Ok(())
}
fn sql_epoch(epoch: u64) -> Result<i64, SemanticError> {
    i64::try_from(epoch).map_err(|_| {
        SemanticError::incorrect(
            "storage/invalid-epoch",
            "Protection epoch exceeds the storage revision range",
        )
    })
}
fn decode_objects(
    ids: &[ObjectId],
    physical: Vec<Option<Vec<u8>>>,
    stats: &mut ObjectReadStats,
) -> Result<Vec<Option<Vec<u8>>>, SemanticError> {
    for bytes in physical.iter().flatten() {
        stats.physical_bytes = stats.physical_bytes.saturating_add(bytes.len() as u64);
        record_read(bytes.len());
    }
    let start = Instant::now();
    let result = (|| {
        let mut total = 0usize;
        let mut plans = Vec::with_capacity(ids.len());
        for (id, bytes) in ids.iter().zip(&physical) {
            let decoder = bytes
                .as_ref()
                .map(|bytes| {
                    crate::storage::codec::inspect_block(id, bytes)
                        .map_err(|error| corrupt().detail("codec", error.code))
                })
                .transpose()?;
            total = total
                .checked_add(
                    decoder
                        .as_ref()
                        .map_or(0, |decoder| decoder.canonical_len()),
                )
                .ok_or_else(read_budget)?;
            if total > MAX_BLOCK_BYTES {
                return Err(read_budget());
            }
            plans.push(decoder);
        }
        plans
            .into_iter()
            .map(|plan| {
                let Some(plan) = plan else {
                    return Ok(None);
                };
                let compressed = plan.is_compressed();
                let canonical = plan
                    .decode()
                    .map_err(|error| corrupt().detail("codec", error.code))?;
                stats.canonical_bytes =
                    stats.canonical_bytes.saturating_add(canonical.len() as u64);
                if compressed {
                    stats.compressed_hits = stats.compressed_hits.saturating_add(1);
                } else {
                    stats.raw_reads = stats.raw_reads.saturating_add(1);
                }
                Ok(Some(canonical))
            })
            .collect()
    })();
    stats.decode_nanos = stats
        .decode_nanos
        .saturating_add(start.elapsed().as_nanos().min(u64::MAX as u128) as u64);
    result
}

fn read_budget() -> SemanticError {
    SemanticError::incorrect(
        "storage/read-budget",
        "Object batch exceeds the 64 MiB canonical result budget; split the batch",
    )
}

fn corrupt() -> SemanticError {
    SemanticError::new(
        ErrorCategory::Fault,
        "storage/object-corrupt",
        "Object payload does not match its immutable identity",
    )
}
fn record_read(bytes: usize) {
    if let Some(c) = OperationContext::current() {
        c.record_payload_read(bytes as u64);
    }
}
fn record_write(bytes: usize) {
    if let Some(c) = OperationContext::current() {
        c.record_payload_write(bytes as u64);
    }
}

fn read_error(error: postgres::Error) -> SemanticError {
    use std::error::Error;
    if error
        .source()
        .is_some_and(|source| source.is::<postgres::types::WrongType>())
    {
        return SemanticError::new(
            ErrorCategory::Fault,
            "storage/driver-type",
            "Storage SQL parameter type does not match its Rust encoding",
        );
    }
    crate::postgres_connection::postgres_error("storage/postgres", error)
}
fn commit_error(error: postgres::Error) -> SemanticError {
    let mut result = read_error(error);
    if result.category == ErrorCategory::Unavailable {
        result.category = ErrorCategory::UnknownOutcome;
        result.code = "storage/unknown-outcome";
        result.message = "Storage acknowledgement was lost; resolve the intended object/reference before retrying".to_owned();
    }
    result
}

#[cfg(test)]
mod error_tests {
    use super::*;

    fn driver_error() -> postgres::Error {
        "port=invalid-transport-diagnostic"
            .parse::<postgres::Config>()
            .unwrap_err()
    }

    #[test]
    fn storage_driver_failure_retains_transport_diagnostics_and_commit_knowledge() {
        let read = read_error(driver_error());
        assert_eq!(read.category, ErrorCategory::Unavailable);
        assert_eq!(read.code, "storage/postgres");
        assert_eq!(
            read.details.get("postgres_transport").map(String::as_str),
            Some("true")
        );
        assert_eq!(read.message, "PostgreSQL transport error");
        assert!(!read.message.contains("invalid-transport-diagnostic"));
        let commit = commit_error(driver_error());
        assert_eq!(commit.category, ErrorCategory::UnknownOutcome);
        assert_eq!(commit.code, "storage/unknown-outcome");
        assert_eq!(commit.details, read.details);
    }

    #[test]
    #[ignore = "requires EDB_TEST_POSTGRES_URL for a disposable PostgreSQL database"]
    fn storage_server_errors_preserve_categories_and_distinguish_commit_uncertainty() {
        let mut client = postgres::Client::connect(
            &std::env::var("EDB_TEST_POSTGRES_URL")
                .expect("EDB_TEST_POSTGRES_URL must name a disposable PostgreSQL database"),
            postgres::NoTls,
        )
        .unwrap();
        for (state, category, transport) in [
            ("23505", ErrorCategory::Conflict, false),
            ("40001", ErrorCategory::Conflict, false),
            ("40P01", ErrorCategory::Conflict, false),
            ("42501", ErrorCategory::Forbidden, false),
            ("57014", ErrorCategory::Interrupted, false),
            ("55P03", ErrorCategory::Busy, false),
            ("22012", ErrorCategory::Fault, false),
            ("08006", ErrorCategory::Unavailable, true),
            ("57P01", ErrorCategory::Unavailable, true),
            ("57P02", ErrorCategory::Unavailable, true),
            ("57P03", ErrorCategory::Unavailable, true),
        ] {
            // Controlled server diagnostics without mutating shared storage.
            let sql = format!(
                "DO $$BEGIN RAISE EXCEPTION USING ERRCODE='{state}', MESSAGE='rejected ''private'' value'; END$$"
            );
            let read = read_error(client.batch_execute(&sql).unwrap_err());
            assert_eq!(read.category, category, "{state}");
            assert_eq!(read.code, "storage/postgres");
            assert_eq!(
                read.details.get("postgres_sqlstate").map(String::as_str),
                Some(state)
            );
            assert_eq!(
                read.details.contains_key("postgres_transport"),
                transport,
                "{state}"
            );
            assert!(!read.message.contains("private"));
            let commit = commit_error(client.batch_execute(&sql).unwrap_err());
            assert_eq!(
                commit.category,
                if transport {
                    ErrorCategory::UnknownOutcome
                } else {
                    category
                },
                "{state}"
            );
            assert_eq!(
                commit.code,
                if transport {
                    "storage/unknown-outcome"
                } else {
                    "storage/postgres"
                }
            );
        }
        let wrong_type = read_error(
            client
                .query("SELECT $1::integer", &[&"wrong type"])
                .unwrap_err(),
        );
        assert_eq!(wrong_type.category, ErrorCategory::Fault);
        assert_eq!(wrong_type.code, "storage/driver-type");
        assert!(!wrong_type.details.contains_key("postgres_transport"));
    }
}
