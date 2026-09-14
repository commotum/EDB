//! Mutable names and permanent database identities over opaque references.
//! Normal name pages are selective. Names too long for a provider key use an
//! overflow stream: listing retains only a page but reads all overflow metadata.
pub(crate) mod database;
use crate::storage::protection::protection;
use crate::storage::root::Block;
use crate::storage::{BatchOutcome, ObjectId, PgBlockStore, RefChange, RefCondition, Reference};
use crate::{ErrorCategory, PostgresConnectionConfig, Schema, SemanticError};
pub use database::BlockDatabase;
pub(crate) use database::{identity_string, name_key};
use std::collections::BTreeMap;

const IDENTITY_KIND: u16 = 6;
const NAMES: &str = "catalog/names/";
const OVERFLOW: &str = "catalog/overflow/";
const RETIRED: &str = "catalog/retired/";
const ATTEMPTS: usize = 16;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DatabaseCatalogEntry {
    pub name: Option<String>,
    pub database_id: String,
    pub lineage_id: String,
    pub retired: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateDatabaseResult {
    pub created: bool,
    pub database: DatabaseCatalogEntry,
}

pub struct DatabaseCatalog {
    config: PostgresConnectionConfig,
    store: PgBlockStore,
}

pub(crate) fn validate_name(name: &str) -> Result<(), SemanticError> {
    if name.is_empty() || name.contains('\0') {
        return Err(SemanticError::incorrect(
            "catalog/invalid-name",
            "Database names must be nonempty and contain no NUL",
        ));
    }
    Ok(())
}

pub(crate) fn identity_key(identity: [u8; 16]) -> String {
    format!("catalog/identities/{}", identity_string(identity))
}
pub(crate) fn lineage_key(identity: [u8; 16]) -> String {
    format!("catalog/lineages/{}", identity_string(identity))
}
fn listing_key(name: &str) -> String {
    if NAMES.len() + name.len() <= 1024 {
        format!("{NAMES}{name}")
    } else {
        format!("{OVERFLOW}{}", &name_key(name)[6..])
    }
}
fn guard(key: &str, reference: Option<&Reference>) -> RefCondition {
    RefCondition {
        key: key.to_owned(),
        expected: reference.map(|r| r.revision),
    }
}
fn change(key: String, id: Option<ObjectId>) -> RefChange {
    RefChange {
        key,
        value: id.map(|id| id.to_vec()),
    }
}
fn object_id(bytes: &[u8]) -> Result<ObjectId, SemanticError> {
    bytes
        .try_into()
        .map_err(|_| fault("Malformed catalog object reference"))
}
fn block(store: &mut PgBlockStore, reference: &Reference) -> Result<Block, SemanticError> {
    let id = object_id(
        reference
            .value
            .as_deref()
            .ok_or_else(|| fault("Catalog object is retired"))?,
    )?;
    let bytes = store
        .get(id)?
        .ok_or_else(|| fault("Catalog object is absent"))?;
    Block::decode(&id, &bytes)
}
fn entry(identity: [u8; 16], route: [u8; 16], name: Option<String>) -> DatabaseCatalogEntry {
    let database_id = identity_string(route);
    DatabaseCatalogEntry {
        retired: name.is_none(),
        name,
        lineage_id: identity_string(identity),
        database_id,
    }
}
fn decode_identity(
    store: &mut PgBlockStore,
    route: [u8; 16],
    reference: &Reference,
) -> Result<DatabaseCatalogEntry, SemanticError> {
    let block = block(store, reference)?;
    if block.kind != IDENTITY_KIND
        || !block.links.is_empty()
        || block.payload.len() < 33
        || block.payload[16..32] != route
        || route == [0; 16]
        || block.payload[..16] == [0; 16]
    {
        return Err(fault("Catalog identity descriptor differs from its key"));
    }
    let identity = block.payload[..16].try_into().unwrap();
    let name = match block.payload[32] {
        0 | 2 => {
            let name = std::str::from_utf8(&block.payload[33..])
                .map_err(|_| fault("Catalog name is not UTF8"))?;
            validate_name(name)?;
            Some(name.to_owned())
        }
        1 if block.payload.len() == 33 => None,
        _ => return Err(fault("Malformed catalog retirement descriptor")),
    };
    Ok(entry(identity, route, name))
}
fn put_identity(
    store: &mut PgBlockStore,
    identity: [u8; 16],
    route: [u8; 16],
    name: Option<&str>,
) -> Result<ObjectId, SemanticError> {
    let mut payload = identity.to_vec();
    payload.extend_from_slice(&route);
    payload.push(u8::from(name.is_none()));
    if let Some(name) = name {
        validate_name(name)?;
        payload.extend_from_slice(name.as_bytes());
    }
    store.put(
        &Block {
            kind: IDENTITY_KIND,
            links: vec![],
            payload,
        }
        .encode()?,
    )
}
pub(crate) fn put_name(
    store: &mut PgBlockStore,
    database: &BlockDatabase,
) -> Result<ObjectId, SemanticError> {
    let mut payload = database.identity.to_vec();
    payload.extend_from_slice(&database.route);
    payload.extend_from_slice(database.name.as_bytes());
    store.put(
        &Block {
            kind: 5,
            links: vec![],
            payload,
        }
        .encode()?,
    )
}

pub(crate) fn creation_conditions(
    store: &mut PgBlockStore,
    database: &BlockDatabase,
) -> Result<Vec<RefCondition>, SemanticError> {
    let listing = listing_key(&database.name);
    let previous = store.read_ref(&listing)?;
    if previous.as_ref().is_some_and(|r| r.value.is_some()) {
        return Err(busy());
    }
    let lineage = lineage_key(database.identity);
    let binding = store.read_ref(&lineage)?;
    if binding.as_ref().is_some_and(|r| r.value.is_some()) {
        return Err(SemanticError::conflict(
            "backup/lineage-exists",
            "This lineage already has an active or reserved route",
        ));
    }
    Ok(vec![
        guard(&listing, previous.as_ref()),
        guard(&identity_key(database.route), None),
        guard(&lineage, binding.as_ref()),
    ])
}
pub(crate) fn creation_changes(
    store: &mut PgBlockStore,
    database: &BlockDatabase,
    name_id: ObjectId,
) -> Result<Vec<RefChange>, SemanticError> {
    let descriptor = put_identity(
        store,
        database.identity,
        database.route,
        Some(&database.name),
    )?;
    Ok(vec![
        change(identity_key(database.route), Some(descriptor)),
        change(lineage_key(database.identity), Some(descriptor)),
        change(listing_key(&database.name), Some(name_id)),
    ])
}

struct Resolved {
    database: BlockDatabase,
    entry: DatabaseCatalogEntry,
    mapping: Reference,
    identity: Reference,
}
fn resolve_current(
    store: &mut PgBlockStore,
    name: &str,
) -> Result<Option<Resolved>, SemanticError> {
    validate_name(name)?;
    for _ in 0..ATTEMPTS {
        let Some(mapping) = store
            .read_ref(&name_key(name))?
            .filter(|r| r.value.is_some())
        else {
            return Ok(None);
        };
        let database =
            match BlockDatabase::decode_mapping(store, name, mapping.value.as_deref().unwrap()) {
                Ok(database) => database,
                Err(error) => {
                    // An obsolete metadata object may be reclaimed immediately
                    // after its reference moves. Retry that race, not corruption
                    // at an unchanged authoritative reference.
                    if store.read_ref(&name_key(name))? != Some(mapping) {
                        continue;
                    }
                    return Err(error);
                }
            };
        let identity = store
            .read_ref(&identity_key(database.route))?
            .filter(|r| r.value.is_some())
            .ok_or_else(|| fault("Name has no issued identity"))?;
        let entry = match decode_identity(store, database.route, &identity) {
            Ok(entry) => entry,
            Err(error) => {
                if store.read_ref(&identity_key(database.route))? != Some(identity) {
                    continue;
                }
                return Err(error);
            }
        };
        // Multi-reference reads may straddle a rename. Retry the mapping; do
        // not report a transient mismatch as corruption or follow another ID.
        if entry.name.as_deref() != Some(name)
            || entry.lineage_id != identity_string(database.identity)
        {
            if store.read_ref(&name_key(name))? != Some(mapping) {
                continue;
            }
            return Err(fault("Name and identity descriptors disagree"));
        }
        return Ok(Some(Resolved {
            database,
            entry,
            mapping,
            identity,
        }));
    }
    Err(busy())
}
pub(crate) fn resolve_database(
    store: &mut PgBlockStore,
    name: &str,
) -> Result<Option<BlockDatabase>, SemanticError> {
    let Some(current) = resolve_current(store, name)? else {
        return Ok(None);
    };
    if store.read_ref(&current.database.reference_key())?.is_none() {
        return Err(missing(
            "backup/database-restoring",
            "Database name is reserved by an incomplete restore",
        ));
    }
    Ok(Some(current.database))
}
pub(crate) fn existing_database(
    store: &mut PgBlockStore,
    name: &str,
) -> Result<Option<BlockDatabase>, SemanticError> {
    Ok(resolve_current(store, name)?.map(|current| current.database))
}
/// Restore resolves visible headless reservations as well as active routes.
/// The caller still guards this exact mapping throughout staging/activation.
pub(crate) fn restore_binding(
    store: &mut PgBlockStore,
    name: &str,
    identity: [u8; 16],
) -> Result<Option<BlockDatabase>, SemanticError> {
    let Some(current) = resolve_current(store, name)? else {
        return Ok(None);
    };
    if current.database.identity != identity {
        return Err(SemanticError::conflict(
            "backup/target-lineage-conflict",
            "Restore target belongs to another lineage",
        ));
    }
    Ok(Some(current.database))
}
pub(crate) fn require_active(
    store: &mut PgBlockStore,
    identity: [u8; 16],
) -> Result<DatabaseCatalogEntry, SemanticError> {
    let entry = route_entry(store, identity)?;
    if entry.retired {
        return Err(missing(
            "catalog/database-retired",
            "Database route is retired",
        ));
    }
    Ok(entry)
}
pub(crate) fn route_entry(
    store: &mut PgBlockStore,
    identity: [u8; 16],
) -> Result<DatabaseCatalogEntry, SemanticError> {
    for _ in 0..ATTEMPTS {
        let reference = store
            .read_ref(&identity_key(identity))?
            .filter(|r| r.value.is_some())
            .ok_or_else(|| {
                missing(
                    "catalog/database-not-found",
                    "Database identity was never issued",
                )
            })?;
        let entry = match decode_identity(store, identity, &reference) {
            Ok(entry) => entry,
            Err(error) => {
                if store.read_ref(&identity_key(identity))? != Some(reference) {
                    continue;
                }
                return Err(error);
            }
        };
        return Ok(entry);
    }
    Err(busy())
}

impl DatabaseCatalog {
    pub fn connect(connection: &str) -> Result<Self, SemanticError> {
        Self::connect_configured(&PostgresConnectionConfig::parse(connection)?)
    }
    pub fn connect_configured(config: &PostgresConnectionConfig) -> Result<Self, SemanticError> {
        Ok(Self {
            config: config.clone(),
            store: PgBlockStore::connect(config)?,
        })
    }
    pub fn resolve(&mut self, name: &str) -> Result<DatabaseCatalogEntry, SemanticError> {
        resolve_current(&mut self.store, name)?
            .map(|r| r.entry)
            .ok_or_else(|| missing("catalog/name-not-found", "No active database has that name"))
    }
    pub fn require_active_id(
        &mut self,
        database_id: &str,
    ) -> Result<DatabaseCatalogEntry, SemanticError> {
        require_active(&mut self.store, parse_identity(database_id)?)
    }
    pub fn create_if_absent(
        &mut self,
        name: &str,
        schema: Schema,
    ) -> Result<CreateDatabaseResult, SemanticError> {
        for _ in 0..ATTEMPTS {
            match BlockDatabase::create_with_status(&self.config, name, schema.clone()) {
                Ok((database, created)) => {
                    return Ok(CreateDatabaseResult {
                        created,
                        database: entry(database.identity, database.route, Some(database.name)),
                    });
                }
                Err(error) if error.category == ErrorCategory::Conflict => std::thread::yield_now(),
                Err(error) => return Err(error),
            }
        }
        Err(busy())
    }
    /// Normal pages read only selected catalog keys. Exceptionally long names
    /// require scanning their overflow metadata, retaining at most `limit`.
    pub fn list(
        &mut self,
        after_name: Option<&str>,
        limit: usize,
    ) -> Result<Vec<DatabaseCatalogEntry>, SemanticError> {
        page_limit(limit)?;
        let mut selected = BTreeMap::new();
        // An overlong continuation cannot be a provider key. Seek its bounded
        // UTF8 prefix, then discard names <= the original full continuation.
        let mut after = after_name.map(|name| {
            let mut end = name.len().min(1024 - NAMES.len());
            while !name.is_char_boundary(end) {
                end -= 1;
            }
            format!("{NAMES}{}", &name[..end])
        });
        loop {
            let page = self
                .store
                .list_live_refs(NAMES, after.as_deref(), limit.min(128))?;
            if page.is_empty() {
                break;
            }
            for (key, _) in &page {
                let name = key
                    .strip_prefix(NAMES)
                    .ok_or_else(|| fault("Name page escaped its prefix"))?;
                if after_name.is_none_or(|after| name > after)
                    && let Some(resolved) = resolve_current(&mut self.store, name)?
                {
                    selected.insert(name.to_owned(), resolved.entry);
                    if selected.len() == limit {
                        break;
                    }
                }
            }
            after = page.last().map(|(key, _)| key.clone());
            if selected.len() == limit {
                break;
            }
        }
        let mut after = None;
        loop {
            let page = self.store.list_live_refs(OVERFLOW, after.as_deref(), 128)?;
            if page.is_empty() {
                break;
            }
            for (key, reference) in &page {
                let mapping = match block(&mut self.store, reference) {
                    Ok(mapping) => mapping,
                    Err(error) => {
                        if self.store.read_ref(key)?.as_ref() != Some(reference) {
                            continue;
                        }
                        return Err(error);
                    }
                };
                if mapping.kind != 5 || !mapping.links.is_empty() || mapping.payload.len() < 33 {
                    return Err(fault("Malformed overflow name mapping"));
                }
                let name = std::str::from_utf8(&mapping.payload[32..])
                    .map_err(|_| fault("Overflow name is not UTF8"))?;
                if after_name.is_none_or(|after| name > after)
                    && let Some(resolved) = resolve_current(&mut self.store, name)?
                {
                    selected.insert(name.to_owned(), resolved.entry);
                    if selected.len() > limit {
                        selected.pop_last();
                    }
                }
            }
            after = page.last().map(|(key, _)| key.clone());
        }
        Ok(selected.into_values().collect())
    }
    pub fn list_retired(
        &mut self,
        after_storage_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<DatabaseCatalogEntry>, SemanticError> {
        page_limit(limit)?;
        let mut after = after_storage_id.map(|id| format!("{RETIRED}{id}"));
        let mut entries = Vec::with_capacity(limit);
        while entries.len() < limit {
            let page = self.store.list_live_refs(
                RETIRED,
                after.as_deref(),
                (limit - entries.len()).min(128),
            )?;
            if page.is_empty() {
                break;
            }
            for (key, reference) in &page {
                let id = parse_identity(
                    key.strip_prefix(RETIRED)
                        .ok_or_else(|| fault("Retired page escaped its prefix"))?,
                )?;
                let entry = decode_identity(&mut self.store, id, reference)?;
                if !entry.retired {
                    return Err(fault("Retired listing contains an active identity"));
                }
                entries.push(entry);
            }
            after = page.last().map(|(key, _)| key.clone());
        }
        Ok(entries)
    }
    pub fn rename(
        &mut self,
        name: &str,
        new_name: &str,
    ) -> Result<DatabaseCatalogEntry, SemanticError> {
        self.rename_inner(name, new_name, None)
    }
    /// Guard the logical lineage currently under `name`, not a captured route
    /// capability. Restoring that lineage may issue a different physical route.
    pub fn rename_checked(
        &mut self,
        name: &str,
        new_name: &str,
        expected_lineage: &str,
    ) -> Result<DatabaseCatalogEntry, SemanticError> {
        self.rename_inner(name, new_name, Some(expected_lineage))
    }
    fn rename_inner(
        &mut self,
        name: &str,
        new_name: &str,
        expected: Option<&str>,
    ) -> Result<DatabaseCatalogEntry, SemanticError> {
        self.store.require_object_delete_privilege()?;
        validate_name(new_name)?;
        for _ in 0..ATTEMPTS {
            let current = resolve_current(&mut self.store, name)?.ok_or_else(|| {
                missing("catalog/name-not-found", "No active database has that name")
            })?;
            check_identity(&current.entry, expected)?;
            if name == new_name {
                return Ok(current.entry);
            }
            let target = self.store.read_ref(&name_key(new_name))?;
            if target.as_ref().is_some_and(|r| r.value.is_some()) {
                return Err(SemanticError::conflict(
                    "catalog/name-exists",
                    "Rename target is already active",
                ));
            }
            let mut conditions = vec![
                guard(&name_key(name), Some(&current.mapping)),
                guard(&name_key(new_name), target.as_ref()),
                guard(
                    &identity_key(current.database.route),
                    Some(&current.identity),
                ),
            ];
            for key in [listing_key(name), listing_key(new_name)] {
                conditions.push(guard(&key, self.store.read_ref(&key)?.as_ref()));
            }
            let protection = protection(&mut self.store, &conditions)?;
            self.store.set_write_protection(Some(protection.clone()))?;
            let result = (|| {
                let database = BlockDatabase {
                    identity: current.database.identity,
                    route: current.database.route,
                    name: new_name.to_owned(),
                };
                let mapping = put_name(&mut self.store, &database)?;
                let identity = put_identity(
                    &mut self.store,
                    database.identity,
                    database.route,
                    Some(new_name),
                )?;
                let changes = vec![
                    change(name_key(name), None),
                    change(listing_key(name), None),
                    change(name_key(new_name), Some(mapping)),
                    change(listing_key(new_name), Some(mapping)),
                    change(identity_key(database.route), Some(identity)),
                ];
                crate::storage::ownership::publish_refs(
                    &mut self.store,
                    &protection.conditions,
                    &changes,
                )
            })();
            self.store.set_write_protection(None)?;
            match result {
                Ok(BatchOutcome::Applied(_)) => {
                    return Ok(entry(
                        current.database.identity,
                        current.database.route,
                        Some(new_name.to_owned()),
                    ));
                }
                Ok(BatchOutcome::Conflict(_)) => {}
                Err(error) if error.category == ErrorCategory::Conflict => {}
                Err(error) => return Err(error),
            }
        }
        Err(busy())
    }
    pub fn retire(&mut self, name: &str) -> Result<Option<DatabaseCatalogEntry>, SemanticError> {
        self.retire_inner(name, None)
    }
    /// Guard the logical lineage currently under `name`. A matching restored
    /// lineage is intentionally eligible; old runtime handles remain route-bound.
    pub fn retire_checked(
        &mut self,
        name: &str,
        expected_lineage: &str,
    ) -> Result<Option<DatabaseCatalogEntry>, SemanticError> {
        self.retire_inner(name, Some(expected_lineage))
    }
    fn retire_inner(
        &mut self,
        name: &str,
        expected: Option<&str>,
    ) -> Result<Option<DatabaseCatalogEntry>, SemanticError> {
        self.store.require_object_delete_privilege()?;
        for _ in 0..ATTEMPTS {
            let Some(current) = resolve_current(&mut self.store, name)? else {
                return Ok(None);
            };
            check_identity(&current.entry, expected)?;
            let identity = current.database.identity;
            let route = current.database.route;
            let root_key = current.database.reference_key();
            let lease_key = current.database.lease_key();
            let excision_key = crate::storage::excision::work_key(&route);
            let restore_key = format!("restores/{}", identity_string(route));
            let completions_key = format!("restores/completed/{}", identity_string(route));
            let lineage = lineage_key(identity);
            let retired_key = format!("{RETIRED}{}", identity_string(route));
            let listing = listing_key(name);
            let mut conditions = vec![
                guard(&name_key(name), Some(&current.mapping)),
                guard(&identity_key(route), Some(&current.identity)),
            ];
            for key in [
                &root_key,
                &lease_key,
                &excision_key,
                &restore_key,
                &completions_key,
                &lineage,
                &retired_key,
                &listing,
            ] {
                conditions.push(guard(key, self.store.read_ref(key)?.as_ref()));
            }
            let protection = protection(&mut self.store, &conditions)?;
            self.store.set_write_protection(Some(protection.clone()))?;
            let result = (|| {
                let retired = put_identity(&mut self.store, identity, route, None)?;
                let changes = vec![
                    change(name_key(name), None),
                    change(listing, None),
                    change(root_key, None),
                    change(lease_key, None),
                    change(excision_key, None),
                    change(restore_key, None),
                    change(completions_key, None),
                    change(lineage, None),
                    change(identity_key(route), Some(retired)),
                    change(retired_key, Some(retired)),
                ];
                crate::storage::ownership::publish_refs(
                    &mut self.store,
                    &protection.conditions,
                    &changes,
                )
            })();
            self.store.set_write_protection(None)?;
            match result {
                Ok(BatchOutcome::Applied(_)) => return Ok(Some(entry(identity, route, None))),
                Ok(BatchOutcome::Conflict(_)) => {}
                Err(error) if error.category == ErrorCategory::Conflict => {}
                Err(error) => return Err(error),
            }
        }
        Err(busy())
    }
}
pub(crate) fn parse_identity(id: &str) -> Result<[u8; 16], SemanticError> {
    if id.len() != 36 {
        return Err(missing(
            "catalog/database-not-found",
            "Database identity is not a canonical UUID",
        ));
    }
    let value = u128::from_str_radix(&id.replace('-', ""), 16).map_err(|_| {
        missing(
            "catalog/database-not-found",
            "Database identity is not a canonical UUID",
        )
    })?;
    let identity = value.to_be_bytes();
    if identity == [0; 16] || identity_string(identity) != id {
        return Err(missing(
            "catalog/database-not-found",
            "Database identity is not a canonical UUID",
        ));
    }
    Ok(identity)
}
fn check_identity(
    entry: &DatabaseCatalogEntry,
    expected: Option<&str>,
) -> Result<(), SemanticError> {
    if expected.is_some_and(|id| id != entry.lineage_id) {
        return Err(SemanticError::conflict(
            "catalog/identity-mismatch",
            "Name no longer identifies the expected lineage",
        ));
    }
    Ok(())
}
fn page_limit(limit: usize) -> Result<(), SemanticError> {
    if !(1..=4096).contains(&limit) {
        return Err(SemanticError::incorrect(
            "catalog/page-limit",
            "Catalog page limit must be in 1..=4096",
        ));
    }
    Ok(())
}
fn missing(code: &'static str, message: &str) -> SemanticError {
    SemanticError::new(ErrorCategory::NotFound, code, message)
}
fn fault(message: &str) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, "catalog/content", message)
}
fn busy() -> SemanticError {
    SemanticError::conflict(
        "catalog/publication-conflict",
        "Catalog authority changed during publication",
    )
}
