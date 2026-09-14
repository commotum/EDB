//! Shared database identity, catalog mapping and finite genesis creation.
//! Name resolution and creation do not start or grant a running writer lease.
use super::validate_name;
use crate::storage::descriptors::{IndexDescriptor, SnapshotMetadata};
use crate::storage::log::{LogEntry, LogRoot};
use crate::storage::protection::protection;
use crate::storage::root::{Block, DatabaseRoot, DatabaseValueRoot};
use crate::storage::{BatchOutcome, ObjectId, PgBlockStore, RefChange, RefCondition, Reference};
use crate::{
    Database, ErrorCategory, IndexOrder, PostgresConnectionConfig, Schema, SemanticError, View,
};

const NAME_KIND: u16 = 5;

/// The identity is fixed at name resolution; later renaming cannot redirect it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockDatabase {
    /// Immutable canonical lineage, retained byte-for-byte by backups.
    pub identity: [u8; 16],
    /// Permanently issued live route. Retirement never reactivates this UUID.
    pub route: [u8; 16],
    pub name: String,
}
impl BlockDatabase {
    pub fn reference_key(&self) -> String {
        format!("databases/{}", identity_string(self.route))
    }
    /// Creation only materializes the finite genesis/schema, never user data.
    /// Installation is separate and refuses an existing unrelated schema.
    pub fn create(
        config: &PostgresConnectionConfig,
        name: &str,
        schema: Schema,
    ) -> Result<Self, SemanticError> {
        Self::create_with_status(config, name, schema).map(|(database, _)| database)
    }
    pub(crate) fn create_with_status(
        config: &PostgresConnectionConfig,
        name: &str,
        schema: Schema,
    ) -> Result<(Self, bool), SemanticError> {
        validate_name(name)?;
        let mut store = PgBlockStore::connect(config)?;
        if let Some(existing) = crate::storage::catalog::existing_database(&mut store, name)? {
            return Ok((existing, false));
        }
        let identity = crate::uuid_v7()?.to_be_bytes();
        let database = Self {
            identity,
            route: identity,
            name: name.to_owned(),
        };
        let name_key = name_key(name);
        let old_name = store.read_ref(&name_key)?;
        // A creator may have won since the initial lookup. A live mapping is
        // never replaceable by create, even if its revision is now known.
        if let Some(bytes) = old_name.as_ref().and_then(|r| r.value.as_deref()) {
            return Self::decode_mapping(&mut store, name, bytes).map(|database| (database, false));
        }
        let mut guards = vec![
            condition(&name_key, old_name.as_ref()),
            RefCondition {
                key: database.reference_key(),
                expected: None,
            },
        ];
        guards.extend(crate::storage::catalog::creation_conditions(
            &mut store, &database,
        )?);
        let protection = protection(&mut store, &guards)?;
        store.set_write_protection(Some(protection.clone()))?;
        let genesis = Database::new(schema)?;
        // The convenience constructor installs nonempty application schema as
        // transaction 1. Preserve that history; never relabel it as t=0.
        let basis = genesis.basis_t();
        let reserved_frontier = genesis
            .reserved_allocation()
            .ok_or_else(|| {
                fault(
                    "storage/allocation",
                    "Initial database has no reserved checkpoint",
                )
            })?
            .frontier();
        let log = if basis == 0 {
            None
        } else {
            Some(
                LogRoot::empty().append(
                    &mut store,
                    &LogEntry {
                        basis_t: basis,
                        eidx_frontier: genesis.eidx_frontier(),
                        reserved_frontier,
                        tx_data: genesis
                            .datoms(View::History, IndexOrder::Eavt)
                            .into_iter()
                            .filter(|d| crate::tx_to_t(d.tx).ok() == Some(basis))
                            .collect(),
                    },
                )?,
            )
        };
        let mut trees = Vec::with_capacity(8);
        for history in [false, true] {
            for order in [
                IndexOrder::Eavt,
                IndexOrder::Aevt,
                IndexOrder::Avet,
                IndexOrder::Vaet,
            ] {
                let built = crate::index::tree::build_tree(
                    order,
                    history,
                    genesis.datoms(
                        if history {
                            View::History
                        } else {
                            View::Current
                        },
                        order,
                    ),
                    &crate::index::tree::TreeConfig::default(),
                )?;
                for (id, bytes) in built.nodes.iter() {
                    if store.put(bytes)? != *id {
                        return Err(fault("storage/tree-identity", "Genesis tree hash differs"));
                    }
                }
                trees.push(built.descriptor);
            }
        }
        let mut descriptor = IndexDescriptor {
            identity,
            basis,
            generation: 0,
            trees,
            pending_avet: Vec::new(),
            avet_work: Vec::new(),
            fulltext: None,
        };
        descriptor.fulltext = crate::storage::fulltext::build_for_descriptor(
            &mut store,
            &descriptor,
            genesis.schema(),
            None,
            &crate::FulltextBuildLimits::default(),
        )?
        .map(|(attachment, _)| attachment);
        let indexes = store.put(&descriptor.encode()?)?;
        let metadata = store.put(
            &SnapshotMetadata {
                identity,
                basis,
                generation: 0,
                eidx_frontier: genesis.eidx_frontier(),
                reserved_frontier,
                last_tx_instant: genesis.last_tx_instant(),
                excision: None,
            }
            .encode()?,
        )?;
        let mut root = DatabaseRoot {
            identity,
            basis,
            writer_epoch: 0,
            log: log.as_ref().and_then(LogRoot::head),
            indexes: Some(indexes),
            receipts: None,
            metadata: Some(metadata),
            read_authorization: None,
        };
        let initial = DatabaseValueRoot::from(&root);
        let initial_id = store.put(&initial.encode()?)?;
        root.read_authorization = Some(
            crate::storage::read_authorization::ReadAuthorization::create(
                &mut store, &initial, initial_id,
            )?,
        );
        let root_id = store.put(&root.encode()?)?;
        let mut payload = identity.to_vec();
        payload.extend_from_slice(&database.route);
        payload.extend_from_slice(name.as_bytes());
        let name_id = store.put(
            &Block {
                kind: NAME_KIND,
                links: vec![],
                payload,
            }
            .encode()?,
        )?;
        let mut changes = vec![
            RefChange {
                key: database.reference_key(),
                value: Some(root_id.to_vec()),
            },
            RefChange {
                key: name_key,
                value: Some(name_id.to_vec()),
            },
        ];
        changes.extend(crate::storage::catalog::creation_changes(
            &mut store, &database, name_id,
        )?);
        match crate::storage::ownership::publish_refs(&mut store, &protection.conditions, &changes)?
        {
            BatchOutcome::Applied(_) => Ok((database, true)),
            BatchOutcome::Conflict(_) => Err(conflict(
                "storage/create-conflict",
                "Database creation raced another publication",
            )),
        }
    }
    pub fn resolve(config: &PostgresConnectionConfig, name: &str) -> Result<Self, SemanticError> {
        Self::resolve_in(&mut PgBlockStore::connect(config)?, name)?.ok_or_else(|| {
            SemanticError::new(
                ErrorCategory::NotFound,
                "catalog/name-not-found",
                format!("No active database is named {name}"),
            )
        })
    }
    pub(crate) fn resolve_in(
        store: &mut PgBlockStore,
        name: &str,
    ) -> Result<Option<Self>, SemanticError> {
        crate::storage::catalog::resolve_database(store, name)
    }
    pub(crate) fn decode_mapping(
        store: &mut PgBlockStore,
        name: &str,
        bytes: &[u8],
    ) -> Result<Self, SemanticError> {
        let id = object_id(bytes)?;
        let block = Block::decode(&id, &required(store, id)?)?;
        if block.kind != NAME_KIND
            || !block.links.is_empty()
            || block.payload.len() < 32
            || &block.payload[32..] != name.as_bytes()
        {
            return Err(fault(
                "catalog/name-content",
                "Name reference differs from its authenticated mapping",
            ));
        }
        let identity = block.payload[..16].try_into().unwrap();
        let route = block.payload[16..32].try_into().unwrap();
        if identity == [0; 16] || route == [0; 16] {
            return Err(fault(
                "catalog/identity",
                "Database has an invalid identity",
            ));
        }
        Ok(Self {
            identity,
            route,
            name: name.to_owned(),
        })
    }
}

fn condition(key: &str, reference: Option<&Reference>) -> RefCondition {
    RefCondition {
        key: key.to_owned(),
        expected: reference.map(|r| r.revision),
    }
}

fn required(store: &mut PgBlockStore, id: ObjectId) -> Result<Vec<u8>, SemanticError> {
    store.get(id)?.ok_or_else(|| {
        fault(
            "storage/missing-object",
            "Referenced immutable object is absent",
        )
    })
}
fn object_id(bytes: &[u8]) -> Result<ObjectId, SemanticError> {
    bytes.try_into().map_err(|_| {
        fault(
            "storage/root-reference",
            "Reference must contain one object identity",
        )
    })
}
pub(crate) fn name_key(name: &str) -> String {
    format!("names/{}", hex(&crate::sha256(name.as_bytes())))
}
pub(crate) fn identity_string(identity: [u8; 16]) -> String {
    let h = hex(&identity);
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn fault(code: &'static str, message: &str) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, code, message)
}
fn conflict(code: &'static str, message: &str) -> SemanticError {
    SemanticError::conflict(code, message)
}
