use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Identity of an entity, independent of its attributes, cache, or time view.
///
/// Durable values use the authenticated database lineage, not its catalog name
/// or connection. Independent in-memory databases have distinct local origins;
/// clones and speculative descendants retain their origin. This token does not
/// authorize reads and is not a portable snapshot reference.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct EntityIdentity {
    pub(crate) origin: DatabaseOrigin,
    pub(crate) entity: u64,
}

impl EntityIdentity {
    pub fn entity_id(&self) -> u64 {
        self.entity
    }

    /// Durable database lineage, or `None` for a standalone in-memory value.
    pub fn lineage_id(&self) -> Option<&str> {
        match &self.origin {
            DatabaseOrigin::Durable(lineage) => Some(lineage),
            DatabaseOrigin::Memory(_) => None,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum DatabaseOrigin {
    // Retaining the allocation prevents address reuse while an identity exists.
    Memory(Arc<()>),
    Durable(Arc<str>),
}

impl DatabaseOrigin {
    pub(crate) fn memory() -> Self {
        Self::Memory(Arc::new(()))
    }

    pub(crate) fn durable(lineage: &str) -> Self {
        Self::Durable(Arc::from(lineage))
    }
}

impl PartialEq for DatabaseOrigin {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Memory(left), Self::Memory(right)) => Arc::ptr_eq(left, right),
            (Self::Durable(left), Self::Durable(right)) => left == right,
            _ => false,
        }
    }
}

impl Eq for DatabaseOrigin {}

impl Hash for DatabaseOrigin {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Self::Memory(token) => Arc::as_ptr(token).hash(state),
            Self::Durable(lineage) => lineage.hash(state),
        }
    }
}
