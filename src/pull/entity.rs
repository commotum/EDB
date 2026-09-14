use super::*;

#[derive(Clone, Debug)]
pub struct Entity {
    database: DatabaseValue,
    id: u64,
    cache: Option<Arc<Mutex<EntityCache>>>,
}

// Reference identity deliberately excludes the lazy cache and database basis.
// Attribute comparisons belong to Pull/query results, not entity handles.
impl PartialEq for Entity {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.database.entity_origin == other.database.entity_origin
    }
}

impl Eq for Entity {}

impl std::hash::Hash for Entity {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(&self.database.entity_origin, state);
        std::hash::Hash::hash(&self.id, state);
    }
}

type EntityCache = BTreeMap<PullDirection, Option<EntityValue>>;

// A touched entity owns a potentially deep tree of cached component entities.
// Empty a uniquely owned cache before its Arc is dropped so normal destruction
// has the same stack-safety as navigation. Shared caches are drained by their
// final owner, without mutating another live entity's cache.
impl Drop for Entity {
    fn drop(&mut self) {
        fn drain(entity: &mut Entity, pending: &mut Vec<EntityValue>) {
            if let Some(cache) = entity.cache.take().and_then(Arc::into_inner) {
                let cache = cache
                    .into_inner()
                    .unwrap_or_else(|error| error.into_inner());
                pending.extend(cache.into_values().flatten());
            }
        }
        let mut pending = Vec::new();
        drain(self, &mut pending);
        while let Some(value) = pending.pop() {
            match value {
                EntityValue::Entity(mut entity) => drain(&mut entity, &mut pending),
                EntityValue::Collection(mut values) => pending.append(&mut values),
                EntityValue::Scalar(_) => {}
            }
        }
    }
}

/// One value reached through Datomic-style associative entity navigation.
/// Reference values remain lazy entities pinned to the same immutable
/// database; cardinality-many and non-component reverse navigation retain
/// collection shape even when only one value is present.
#[derive(Clone, Debug)]
pub enum EntityValue {
    Scalar(Value),
    Entity(Entity),
    Collection(Vec<EntityValue>),
}

impl Entity {
    pub fn from_database_value(database: DatabaseValue, id: u64) -> Result<Self, SemanticError> {
        database.require_point_in_time("entity")?;
        Ok(Self::from_resolved(database, id))
    }

    pub(super) fn from_resolved(database: DatabaseValue, id: u64) -> Self {
        Self {
            database,
            id,
            cache: Some(Arc::new(Mutex::new(BTreeMap::new()))),
        }
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    /// A cheap identity token suitable for map/set keys. No attributes are read.
    /// Use a snapshot key as well when the key must distinguish time views.
    pub fn identity(&self) -> crate::EntityIdentity {
        crate::EntityIdentity {
            origin: self.database.entity_origin.clone(),
            entity: self.id,
        }
    }

    pub fn database(&self) -> DatabaseValue {
        self.database.clone()
    }

    pub fn get(&self, attribute: u32) -> Result<Option<EntityValue>, SemanticError> {
        self.get_direction(&PullDirection::Forward(AttributeName::Id(attribute)))
    }

    pub fn get_named(
        &self,
        attribute: &AttributeName,
    ) -> Result<Option<EntityValue>, SemanticError> {
        self.get_direction(&PullDirection::Forward(attribute.clone()))
    }

    /// Associative forward or reverse navigation. Unknown keyword attributes
    /// behave like missing map keys; malformed numeric attributes remain an
    /// API error.
    pub fn get_direction(
        &self,
        direction: &PullDirection,
    ) -> Result<Option<EntityValue>, SemanticError> {
        if let Some(value) = self
            .cache
            .as_ref()
            .expect("live entity cache")
            .lock()
            .expect("entity cache poisoned")
            .get(direction)
        {
            return Ok(value.clone());
        }
        let value = self.read_direction(direction)?;
        self.cache
            .as_ref()
            .expect("live entity cache")
            .lock()
            .expect("entity cache poisoned")
            .insert(direction.clone(), value.clone());
        Ok(value)
    }

    fn read_direction(
        &self,
        direction: &PullDirection,
    ) -> Result<Option<EntityValue>, SemanticError> {
        self.database.require_point_in_time("entity")?;
        let (name, reverse) = direction_parts(&self.database, direction)?;
        if !reverse && is_db_id(&name) {
            return Ok(Some(EntityValue::Scalar(Value::Ref(self.id))));
        }
        let Some(attribute) = resolve_pull_attribute(&self.database, &name)? else {
            return Ok(None);
        };
        let schema = self.database.schema().attribute(attribute)?;
        if reverse && schema.value_type != ValueType::Ref {
            return Err(SemanticError::incorrect(
                "pull/reverse-non-ref",
                "reverse navigation requires a ref attribute",
            ));
        }
        let mut values = if reverse {
            self.database
                .datoms_with_prefix(&IndexPrefix::Vaet {
                    value: Value::Ref(self.id),
                    attribute: Some(attribute),
                    entity: None,
                })?
                .iter()
                .map(|datom| Value::Ref(datom.entity))
                .collect::<Vec<_>>()
        } else {
            self.database.values(self.id, attribute)?
        };
        if values.is_empty() {
            return Ok(None);
        }
        let multiple = if reverse {
            !schema.component
        } else {
            schema.cardinality == Cardinality::Many
        };
        if multiple {
            // EAVT groups equal forward values and VAET groups equal reverse
            // entities contiguously.  A filtered value can expose more than
            // one assertion event for the same logical E/A/V, but associative
            // entity navigation remains set-valued.
            values.dedup();
        }
        let mut values = values
            .into_iter()
            .map(|value| entity_navigation_value(&self.database, schema.value_type, value, reverse))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(if multiple {
            EntityValue::Collection(values)
        } else {
            values.remove(0)
        }))
    }

    pub fn reverse(&self, attribute: u32) -> Result<Vec<Entity>, SemanticError> {
        let value = self.get_direction(&PullDirection::Reverse(AttributeName::Id(attribute)))?;
        Ok(match value {
            None => Vec::new(),
            Some(EntityValue::Entity(entity)) => vec![entity],
            Some(EntityValue::Collection(values)) => values
                .into_iter()
                .map(|value| match value {
                    EntityValue::Entity(entity) => Ok(entity),
                    _ => Err(fault(
                        "entity/reverse-shape",
                        "reverse ref navigation did not produce entities",
                    )),
                })
                .collect::<Result<Vec<_>, _>>()?,
            Some(EntityValue::Scalar(_)) => {
                return Err(fault(
                    "entity/reverse-shape",
                    "reverse ref navigation did not produce entities",
                ));
            }
        })
    }

    /// Return available forward keys without realizing their values.
    pub fn keys(&self) -> Result<BTreeSet<Keyword>, SemanticError> {
        let mut keys = BTreeSet::from([db_id()]);
        for datom in self.database.datoms_with_prefix(&IndexPrefix::Eavt {
            entity: self.id,
            attribute: None,
            value: None,
        })? {
            keys.insert(
                self.database
                    .schema()
                    .attribute(datom.attribute)?
                    .ident
                    .clone(),
            );
        }
        Ok(keys)
    }

    /// Realize every direct attribute and recursively realize component
    /// entities. Non-component references remain lazy. Cyclic component paths
    /// stop at the revisited entity; traversal and cache destruction are iterative.
    pub fn touch(&self) -> Result<Self, SemanticError> {
        enum Task {
            Entity(Entity),
            Value(EntityValue),
            Leave(u64),
        }
        let mut pending = vec![Task::Entity(self.clone())];
        let mut path = BTreeSet::new();
        while let Some(task) = pending.pop() {
            match task {
                Task::Entity(entity) => {
                    if !path.insert(entity.id) {
                        continue;
                    }
                    pending.push(Task::Leave(entity.id));
                    let attributes = entity
                        .database
                        .datoms_with_prefix(&IndexPrefix::Eavt {
                            entity: entity.id,
                            attribute: None,
                            value: None,
                        })?
                        .iter()
                        .map(|datom| datom.attribute)
                        .collect::<BTreeSet<_>>();
                    for attribute in attributes {
                        let schema = entity.database.schema().attribute(attribute)?;
                        let value = entity.get(attribute)?;
                        if schema.component
                            && let Some(value) = value
                        {
                            pending.push(Task::Value(value));
                        }
                    }
                }
                Task::Value(EntityValue::Entity(entity)) => pending.push(Task::Entity(entity)),
                Task::Value(EntityValue::Collection(values)) => {
                    pending.extend(values.into_iter().rev().map(Task::Value));
                }
                Task::Value(EntityValue::Scalar(_)) => {}
                Task::Leave(entity) => {
                    path.remove(&entity);
                }
            }
        }
        Ok(self.clone())
    }

    pub fn pull(&self, pattern: &PullPattern) -> Result<QueryValue, SemanticError> {
        self.database.pull(pattern, self.id)
    }
}

fn entity_navigation_value(
    database: &DatabaseValue,
    value_type: ValueType,
    value: Value,
    reverse: bool,
) -> Result<EntityValue, SemanticError> {
    if value_type != ValueType::Ref {
        return Ok(EntityValue::Scalar(value));
    }
    let Value::Ref(entity) = value else {
        return Err(fault(
            "entity/schema-value-mismatch",
            "ref attribute contains a non-ref value",
        ));
    };
    // ATOMIC-NOTE: recovered rae always wraps incoming entity ids with emap;
    // only forward eav/ref-val presents identified reference values as keywords.
    if !reverse && let Some(ident) = database.ident(entity) {
        Ok(EntityValue::Scalar(Value::Keyword(ident.clone())))
    } else {
        Ok(EntityValue::Entity(Entity::from_resolved(
            database.clone(),
            entity,
        )))
    }
}
