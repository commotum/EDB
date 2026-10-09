//! Public index prefix and virtual-boundary semantics.
use crate::model::identity::validate_permanent_eid;
use crate::{Datom, IndexOrder, SemanticError, Value, t_to_tx, tx_to_t};
use std::cmp::Ordering;

/// A transaction component supplied to a raw index boundary.
///
/// EDB accepts both a logical basis `t` and the corresponding reified
/// transaction entity. Keeping those forms distinct prevents an arbitrary
/// entity id from being silently treated as a transaction while still
/// preserving the recovered `eid->eidx` normalization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexTransaction {
    T(u64),
    Tx(u64),
}

impl IndexTransaction {
    /// Normalize either public form to the transaction entity stored in a
    /// native datom. Both paths are checked: a `T` must fit the recovered
    /// entity-index domain and a `Tx` must belong to the transaction partition.
    pub fn stored_tx(self) -> Result<u64, SemanticError> {
        match self {
            Self::T(t) => t_to_tx(t),
            Self::Tx(tx) => {
                tx_to_t(tx)?;
                Ok(tx)
            }
        }
    }
}

/// A left-contiguous sequence of zero through five typed index components.
/// The final assertion/retraction boolean is not a boundary component.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IndexComponents<First, Second, Third, Fourth, Fifth = std::convert::Infallible> {
    Empty,
    One(First),
    Two(First, Second),
    Three(First, Second, Third),
    Four(First, Second, Third, Fourth),
    Five(First, Second, Third, Fourth, Fifth),
}
impl<F, S, T, U, V> IndexComponents<F, S, T, U, V> {
    pub const fn len(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::One(_) => 1,
            Self::Two(..) => 2,
            Self::Three(..) => 3,
            Self::Four(..) => 4,
            Self::Five(..) => 5,
        }
    }
    pub const fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }
}
/// A resolved virtual position in one of EDB's six index orders.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IndexBoundary {
    Eavt(IndexComponents<u64, u32, Value, IndexTransaction, u64>),
    Aevt(IndexComponents<u32, u64, Value, IndexTransaction, u64>),
    Avet(IndexComponents<u32, Value, u64, IndexTransaction, u64>),
    Vaet(IndexComponents<Value, u32, u64, IndexTransaction, u64>),
    Saevt(IndexComponents<u64, u32, u64, Value, IndexTransaction>),
    Savet(IndexComponents<u64, u32, Value, u64, IndexTransaction>),
}
impl IndexBoundary {
    pub const fn order(&self) -> IndexOrder {
        match self {
            Self::Eavt(_) => IndexOrder::Eavt,
            Self::Aevt(_) => IndexOrder::Aevt,
            Self::Avet(_) => IndexOrder::Avet,
            Self::Vaet(_) => IndexOrder::Vaet,
            Self::Saevt(_) => IndexOrder::Saevt,
            Self::Savet(_) => IndexOrder::Savet,
        }
    }
    pub const fn len(&self) -> usize {
        match self {
            Self::Eavt(c) => c.len(),
            Self::Aevt(c) => c.len(),
            Self::Avet(c) => c.len(),
            Self::Vaet(c) => c.len(),
            Self::Saevt(c) => c.len(),
            Self::Savet(c) => c.len(),
        }
    }
    pub const fn is_empty(&self) -> bool {
        match self {
            Self::Eavt(c) => c.is_empty(),
            Self::Aevt(c) => c.is_empty(),
            Self::Avet(c) => c.is_empty(),
            Self::Vaet(c) => c.is_empty(),
            Self::Saevt(c) => c.is_empty(),
            Self::Savet(c) => c.is_empty(),
        }
    }
    pub const fn avet_attribute(&self) -> Option<u32> {
        match self {
            Self::Avet(IndexComponents::One(a)) => Some(*a),
            Self::Avet(IndexComponents::Two(a, _)) => Some(*a),
            Self::Avet(IndexComponents::Three(a, _, _)) => Some(*a),
            Self::Avet(IndexComponents::Four(a, _, _, _)) => Some(*a),
            Self::Avet(IndexComponents::Five(a, _, _, _, _)) => Some(*a),
            Self::Savet(IndexComponents::Two(_, a)) => Some(*a),
            Self::Savet(IndexComponents::Three(_, a, _)) => Some(*a),
            Self::Savet(IndexComponents::Four(_, a, _, _)) => Some(*a),
            Self::Savet(IndexComponents::Five(_, a, _, _, _)) => Some(*a),
            _ => None,
        }
    }
    pub fn validate(&self) -> Result<(), SemanticError> {
        self.normalized().map(|_| ())
    }
    pub(crate) fn normalized(&self) -> Result<NormalizedIndexBoundary, SemanticError> {
        fn eid(e: u64) -> Result<u64, SemanticError> {
            validate_permanent_eid(e)?;
            Ok(e)
        }
        fn value(v: &Value, vaet: bool) -> Result<Value, SemanticError> {
            if vaet && !matches!(v, Value::Ref(_)) {
                return Err(SemanticError::incorrect(
                    "index/vaet-value-not-ref",
                    "VAET boundary value must resolve to an entity reference",
                ));
            }
            if let Value::Ref(e) = v {
                validate_permanent_eid(*e)?;
            }
            Ok(v.clone())
        }
        Ok(match self {
            Self::Eavt(c) => NormalizedIndexBoundary::Eavt(match c {
                IndexComponents::Empty => IndexComponents::Empty,
                IndexComponents::One(entity) => IndexComponents::One(eid(*entity)?),
                IndexComponents::Two(entity, attribute) => {
                    IndexComponents::Two(eid(*entity)?, *attribute)
                }
                IndexComponents::Three(entity, attribute, v) => {
                    IndexComponents::Three(eid(*entity)?, *attribute, value(v, false)?)
                }
                IndexComponents::Four(entity, attribute, v, tx) => IndexComponents::Four(
                    eid(*entity)?,
                    *attribute,
                    value(v, false)?,
                    tx.stored_tx()?,
                ),
                IndexComponents::Five(entity, attribute, v, tx, source) => IndexComponents::Five(
                    eid(*entity)?,
                    *attribute,
                    value(v, false)?,
                    tx.stored_tx()?,
                    eid(*source)?,
                ),
            }),
            Self::Aevt(c) => NormalizedIndexBoundary::Aevt(match c {
                IndexComponents::Empty => IndexComponents::Empty,
                IndexComponents::One(attribute) => IndexComponents::One(*attribute),
                IndexComponents::Two(attribute, entity) => {
                    IndexComponents::Two(*attribute, eid(*entity)?)
                }
                IndexComponents::Three(attribute, entity, v) => {
                    IndexComponents::Three(*attribute, eid(*entity)?, value(v, false)?)
                }
                IndexComponents::Four(attribute, entity, v, tx) => IndexComponents::Four(
                    *attribute,
                    eid(*entity)?,
                    value(v, false)?,
                    tx.stored_tx()?,
                ),
                IndexComponents::Five(attribute, entity, v, tx, source) => IndexComponents::Five(
                    *attribute,
                    eid(*entity)?,
                    value(v, false)?,
                    tx.stored_tx()?,
                    eid(*source)?,
                ),
            }),
            Self::Avet(c) => NormalizedIndexBoundary::Avet(match c {
                IndexComponents::Empty => IndexComponents::Empty,
                IndexComponents::One(attribute) => IndexComponents::One(*attribute),
                IndexComponents::Two(attribute, v) => {
                    IndexComponents::Two(*attribute, value(v, false)?)
                }
                IndexComponents::Three(attribute, v, entity) => {
                    IndexComponents::Three(*attribute, value(v, false)?, eid(*entity)?)
                }
                IndexComponents::Four(attribute, v, entity, tx) => IndexComponents::Four(
                    *attribute,
                    value(v, false)?,
                    eid(*entity)?,
                    tx.stored_tx()?,
                ),
                IndexComponents::Five(attribute, v, entity, tx, source) => IndexComponents::Five(
                    *attribute,
                    value(v, false)?,
                    eid(*entity)?,
                    tx.stored_tx()?,
                    eid(*source)?,
                ),
            }),
            Self::Vaet(c) => NormalizedIndexBoundary::Vaet(match c {
                IndexComponents::Empty => IndexComponents::Empty,
                IndexComponents::One(v) => IndexComponents::One(value(v, true)?),
                IndexComponents::Two(v, attribute) => {
                    IndexComponents::Two(value(v, true)?, *attribute)
                }
                IndexComponents::Three(v, attribute, entity) => {
                    IndexComponents::Three(value(v, true)?, *attribute, eid(*entity)?)
                }
                IndexComponents::Four(v, attribute, entity, tx) => IndexComponents::Four(
                    value(v, true)?,
                    *attribute,
                    eid(*entity)?,
                    tx.stored_tx()?,
                ),
                IndexComponents::Five(v, attribute, entity, tx, source) => IndexComponents::Five(
                    value(v, true)?,
                    *attribute,
                    eid(*entity)?,
                    tx.stored_tx()?,
                    eid(*source)?,
                ),
            }),
            Self::Saevt(c) => NormalizedIndexBoundary::Saevt(match c {
                IndexComponents::Empty => IndexComponents::Empty,
                IndexComponents::One(source) => IndexComponents::One(eid(*source)?),
                IndexComponents::Two(source, attribute) => {
                    IndexComponents::Two(eid(*source)?, *attribute)
                }
                IndexComponents::Three(source, attribute, entity) => {
                    IndexComponents::Three(eid(*source)?, *attribute, eid(*entity)?)
                }
                IndexComponents::Four(source, attribute, entity, v) => IndexComponents::Four(
                    eid(*source)?,
                    *attribute,
                    eid(*entity)?,
                    value(v, false)?,
                ),
                IndexComponents::Five(source, attribute, entity, v, tx) => IndexComponents::Five(
                    eid(*source)?,
                    *attribute,
                    eid(*entity)?,
                    value(v, false)?,
                    tx.stored_tx()?,
                ),
            }),
            Self::Savet(c) => NormalizedIndexBoundary::Savet(match c {
                IndexComponents::Empty => IndexComponents::Empty,
                IndexComponents::One(source) => IndexComponents::One(eid(*source)?),
                IndexComponents::Two(source, attribute) => {
                    IndexComponents::Two(eid(*source)?, *attribute)
                }
                IndexComponents::Three(source, attribute, v) => {
                    IndexComponents::Three(eid(*source)?, *attribute, value(v, false)?)
                }
                IndexComponents::Four(source, attribute, v, entity) => IndexComponents::Four(
                    eid(*source)?,
                    *attribute,
                    value(v, false)?,
                    eid(*entity)?,
                ),
                IndexComponents::Five(source, attribute, v, entity, tx) => IndexComponents::Five(
                    eid(*source)?,
                    *attribute,
                    value(v, false)?,
                    eid(*entity)?,
                    tx.stored_tx()?,
                ),
            }),
        })
    }
    pub(crate) fn current_group_start(&self) -> Self {
        match self {
            Self::Eavt(IndexComponents::Four(entity, attribute, v, _)) => {
                Self::Eavt(IndexComponents::Three(*entity, *attribute, v.clone()))
            }
            Self::Eavt(IndexComponents::Five(entity, attribute, v, _, _)) => {
                Self::Eavt(IndexComponents::Three(*entity, *attribute, v.clone()))
            }
            Self::Aevt(IndexComponents::Four(attribute, entity, v, _)) => {
                Self::Aevt(IndexComponents::Three(*attribute, *entity, v.clone()))
            }
            Self::Aevt(IndexComponents::Five(attribute, entity, v, _, _)) => {
                Self::Aevt(IndexComponents::Three(*attribute, *entity, v.clone()))
            }
            Self::Avet(IndexComponents::Four(attribute, v, entity, _)) => {
                Self::Avet(IndexComponents::Three(*attribute, v.clone(), *entity))
            }
            Self::Avet(IndexComponents::Five(attribute, v, entity, _, _)) => {
                Self::Avet(IndexComponents::Three(*attribute, v.clone(), *entity))
            }
            Self::Vaet(IndexComponents::Four(v, attribute, entity, _)) => {
                Self::Vaet(IndexComponents::Three(v.clone(), *attribute, *entity))
            }
            Self::Vaet(IndexComponents::Five(v, attribute, entity, _, _)) => {
                Self::Vaet(IndexComponents::Three(v.clone(), *attribute, *entity))
            }
            Self::Saevt(IndexComponents::Five(source, attribute, entity, v, _)) => Self::Saevt(
                IndexComponents::Four(*source, *attribute, *entity, v.clone()),
            ),
            Self::Savet(IndexComponents::Five(source, attribute, v, entity, _)) => Self::Savet(
                IndexComponents::Four(*source, *attribute, v.clone(), *entity),
            ),
            _ => self.clone(),
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum NormalizedIndexBoundary {
    Eavt(IndexComponents<u64, u32, Value, u64, u64>),
    Aevt(IndexComponents<u32, u64, Value, u64, u64>),
    Avet(IndexComponents<u32, Value, u64, u64, u64>),
    Vaet(IndexComponents<Value, u32, u64, u64, u64>),
    Saevt(IndexComponents<u64, u32, u64, Value, u64>),
    Savet(IndexComponents<u64, u32, Value, u64, u64>),
    After(Box<Self>),
}
impl NormalizedIndexBoundary {
    pub(crate) fn order(&self) -> IndexOrder {
        match self {
            Self::Eavt(_) => IndexOrder::Eavt,
            Self::Aevt(_) => IndexOrder::Aevt,
            Self::Avet(_) => IndexOrder::Avet,
            Self::Vaet(_) => IndexOrder::Vaet,
            Self::Saevt(_) => IndexOrder::Saevt,
            Self::Savet(_) => IndexOrder::Savet,
            Self::After(b) => b.order(),
        }
    }
    pub(crate) fn after_prefix(self) -> Self {
        match self {
            Self::After(_) => self,
            _ => Self::After(Box::new(self)),
        }
    }
    pub(crate) fn compare_datom(&self, d: &Datom) -> Ordering {
        match self {
            Self::After(b) => b.compare_datom(d).then(Ordering::Less),
            Self::Eavt(c) => match c {
                IndexComponents::Empty => Ordering::Equal,
                IndexComponents::One(entity) => d.entity.cmp(entity),
                IndexComponents::Two(entity, attribute) => d
                    .entity
                    .cmp(entity)
                    .then_with(|| d.attribute.cmp(attribute)),
                IndexComponents::Three(entity, attribute, value) => d
                    .entity
                    .cmp(entity)
                    .then_with(|| d.attribute.cmp(attribute))
                    .then_with(|| d.value.index_cmp(value)),
                IndexComponents::Four(entity, attribute, value, tx) => d
                    .entity
                    .cmp(entity)
                    .then_with(|| d.attribute.cmp(attribute))
                    .then_with(|| d.value.index_cmp(value))
                    .then_with(|| tx.cmp(&d.tx)),
                IndexComponents::Five(entity, attribute, value, tx, source) => d
                    .entity
                    .cmp(entity)
                    .then_with(|| d.attribute.cmp(attribute))
                    .then_with(|| d.value.index_cmp(value))
                    .then_with(|| tx.cmp(&d.tx))
                    .then_with(|| d.source.cmp(source)),
            },
            Self::Aevt(c) => match c {
                IndexComponents::Empty => Ordering::Equal,
                IndexComponents::One(attribute) => d.attribute.cmp(attribute),
                IndexComponents::Two(attribute, entity) => d
                    .attribute
                    .cmp(attribute)
                    .then_with(|| d.entity.cmp(entity)),
                IndexComponents::Three(attribute, entity, value) => d
                    .attribute
                    .cmp(attribute)
                    .then_with(|| d.entity.cmp(entity))
                    .then_with(|| d.value.index_cmp(value)),
                IndexComponents::Four(attribute, entity, value, tx) => d
                    .attribute
                    .cmp(attribute)
                    .then_with(|| d.entity.cmp(entity))
                    .then_with(|| d.value.index_cmp(value))
                    .then_with(|| tx.cmp(&d.tx)),
                IndexComponents::Five(attribute, entity, value, tx, source) => d
                    .attribute
                    .cmp(attribute)
                    .then_with(|| d.entity.cmp(entity))
                    .then_with(|| d.value.index_cmp(value))
                    .then_with(|| tx.cmp(&d.tx))
                    .then_with(|| d.source.cmp(source)),
            },
            Self::Avet(c) => match c {
                IndexComponents::Empty => Ordering::Equal,
                IndexComponents::One(attribute) => d.attribute.cmp(attribute),
                IndexComponents::Two(attribute, value) => d
                    .attribute
                    .cmp(attribute)
                    .then_with(|| d.value.index_cmp(value)),
                IndexComponents::Three(attribute, value, entity) => d
                    .attribute
                    .cmp(attribute)
                    .then_with(|| d.value.index_cmp(value))
                    .then_with(|| d.entity.cmp(entity)),
                IndexComponents::Four(attribute, value, entity, tx) => d
                    .attribute
                    .cmp(attribute)
                    .then_with(|| d.value.index_cmp(value))
                    .then_with(|| d.entity.cmp(entity))
                    .then_with(|| tx.cmp(&d.tx)),
                IndexComponents::Five(attribute, value, entity, tx, source) => d
                    .attribute
                    .cmp(attribute)
                    .then_with(|| d.value.index_cmp(value))
                    .then_with(|| d.entity.cmp(entity))
                    .then_with(|| tx.cmp(&d.tx))
                    .then_with(|| d.source.cmp(source)),
            },
            Self::Vaet(c) => match c {
                IndexComponents::Empty => Ordering::Equal,
                IndexComponents::One(value) => d.value.index_cmp(value),
                IndexComponents::Two(value, attribute) => d
                    .value
                    .index_cmp(value)
                    .then_with(|| d.attribute.cmp(attribute)),
                IndexComponents::Three(value, attribute, entity) => d
                    .value
                    .index_cmp(value)
                    .then_with(|| d.attribute.cmp(attribute))
                    .then_with(|| d.entity.cmp(entity)),
                IndexComponents::Four(value, attribute, entity, tx) => d
                    .value
                    .index_cmp(value)
                    .then_with(|| d.attribute.cmp(attribute))
                    .then_with(|| d.entity.cmp(entity))
                    .then_with(|| tx.cmp(&d.tx)),
                IndexComponents::Five(value, attribute, entity, tx, source) => d
                    .value
                    .index_cmp(value)
                    .then_with(|| d.attribute.cmp(attribute))
                    .then_with(|| d.entity.cmp(entity))
                    .then_with(|| tx.cmp(&d.tx))
                    .then_with(|| d.source.cmp(source)),
            },
            Self::Saevt(c) => match c {
                IndexComponents::Empty => Ordering::Equal,
                IndexComponents::One(source) => d.source.cmp(source),
                IndexComponents::Two(source, attribute) => d
                    .source
                    .cmp(source)
                    .then_with(|| d.attribute.cmp(attribute)),
                IndexComponents::Three(source, attribute, entity) => d
                    .source
                    .cmp(source)
                    .then_with(|| d.attribute.cmp(attribute))
                    .then_with(|| d.entity.cmp(entity)),
                IndexComponents::Four(source, attribute, entity, value) => d
                    .source
                    .cmp(source)
                    .then_with(|| d.attribute.cmp(attribute))
                    .then_with(|| d.entity.cmp(entity))
                    .then_with(|| d.value.index_cmp(value)),
                IndexComponents::Five(source, attribute, entity, value, tx) => d
                    .source
                    .cmp(source)
                    .then_with(|| d.attribute.cmp(attribute))
                    .then_with(|| d.entity.cmp(entity))
                    .then_with(|| d.value.index_cmp(value))
                    .then_with(|| tx.cmp(&d.tx)),
            },
            Self::Savet(c) => match c {
                IndexComponents::Empty => Ordering::Equal,
                IndexComponents::One(source) => d.source.cmp(source),
                IndexComponents::Two(source, attribute) => d
                    .source
                    .cmp(source)
                    .then_with(|| d.attribute.cmp(attribute)),
                IndexComponents::Three(source, attribute, value) => d
                    .source
                    .cmp(source)
                    .then_with(|| d.attribute.cmp(attribute))
                    .then_with(|| d.value.index_cmp(value)),
                IndexComponents::Four(source, attribute, value, entity) => d
                    .source
                    .cmp(source)
                    .then_with(|| d.attribute.cmp(attribute))
                    .then_with(|| d.value.index_cmp(value))
                    .then_with(|| d.entity.cmp(entity)),
                IndexComponents::Five(source, attribute, value, entity, tx) => d
                    .source
                    .cmp(source)
                    .then_with(|| d.attribute.cmp(attribute))
                    .then_with(|| d.value.index_cmp(value))
                    .then_with(|| d.entity.cmp(entity))
                    .then_with(|| tx.cmp(&d.tx)),
            },
        }
    }
    pub(crate) fn compare_current_group_start(&self, d: &Datom) -> Ordering {
        match self {
            Self::After(b) => b.compare_current_group_start(d),
            Self::Eavt(IndexComponents::Four(entity, attribute, value, _)) => {
                Self::Eavt(IndexComponents::Three(*entity, *attribute, value.clone()))
                    .compare_datom(d)
            }
            Self::Eavt(IndexComponents::Five(entity, attribute, value, _, _)) => {
                Self::Eavt(IndexComponents::Three(*entity, *attribute, value.clone()))
                    .compare_datom(d)
            }
            Self::Aevt(IndexComponents::Four(attribute, entity, value, _)) => {
                Self::Aevt(IndexComponents::Three(*attribute, *entity, value.clone()))
                    .compare_datom(d)
            }
            Self::Aevt(IndexComponents::Five(attribute, entity, value, _, _)) => {
                Self::Aevt(IndexComponents::Three(*attribute, *entity, value.clone()))
                    .compare_datom(d)
            }
            Self::Avet(IndexComponents::Four(attribute, value, entity, _)) => {
                Self::Avet(IndexComponents::Three(*attribute, value.clone(), *entity))
                    .compare_datom(d)
            }
            Self::Avet(IndexComponents::Five(attribute, value, entity, _, _)) => {
                Self::Avet(IndexComponents::Three(*attribute, value.clone(), *entity))
                    .compare_datom(d)
            }
            Self::Vaet(IndexComponents::Four(value, attribute, entity, _)) => {
                Self::Vaet(IndexComponents::Three(value.clone(), *attribute, *entity))
                    .compare_datom(d)
            }
            Self::Vaet(IndexComponents::Five(value, attribute, entity, _, _)) => {
                Self::Vaet(IndexComponents::Three(value.clone(), *attribute, *entity))
                    .compare_datom(d)
            }
            Self::Saevt(IndexComponents::Five(source, attribute, entity, value, _)) => Self::Saevt(
                IndexComponents::Four(*source, *attribute, *entity, value.clone()),
            )
            .compare_datom(d),
            Self::Savet(IndexComponents::Five(source, attribute, value, entity, _)) => Self::Savet(
                IndexComponents::Four(*source, *attribute, value.clone(), *entity),
            )
            .compare_datom(d),
            _ => self.compare_datom(d),
        }
    }
}
/// A matching prefix. Optional components must be contiguous from the left.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IndexPrefix {
    Eavt {
        entity: u64,
        attribute: Option<u32>,
        value: Option<Value>,
    },
    Aevt {
        attribute: u32,
        entity: Option<u64>,
        value: Option<Value>,
    },
    Avet {
        attribute: u32,
        value: Option<Value>,
        entity: Option<u64>,
    },
    Vaet {
        value: Value,
        attribute: Option<u32>,
        entity: Option<u64>,
    },
    Saevt {
        source: u64,
        attribute: Option<u32>,
        entity: Option<u64>,
        value: Option<Value>,
    },
    Savet {
        source: u64,
        attribute: Option<u32>,
        value: Option<Value>,
        entity: Option<u64>,
    },
}
impl IndexPrefix {
    pub fn order(&self) -> IndexOrder {
        match self {
            Self::Eavt { .. } => IndexOrder::Eavt,
            Self::Aevt { .. } => IndexOrder::Aevt,
            Self::Avet { .. } => IndexOrder::Avet,
            Self::Vaet { .. } => IndexOrder::Vaet,
            Self::Saevt { .. } => IndexOrder::Saevt,
            Self::Savet { .. } => IndexOrder::Savet,
        }
    }
    pub(crate) fn validate(&self) -> Result<(), SemanticError> {
        let gap = match self {
            Self::Eavt {
                attribute, value, ..
            } => attribute.is_none() && value.is_some(),
            Self::Aevt { entity, value, .. } => entity.is_none() && value.is_some(),
            Self::Avet { value, entity, .. } => value.is_none() && entity.is_some(),
            Self::Vaet {
                attribute, entity, ..
            } => attribute.is_none() && entity.is_some(),
            Self::Saevt {
                attribute,
                entity,
                value,
                ..
            } => {
                (attribute.is_none() && (entity.is_some() || value.is_some()))
                    || entity.is_none() && value.is_some()
            }
            Self::Savet {
                attribute,
                value,
                entity,
                ..
            } => {
                (attribute.is_none() && (value.is_some() || entity.is_some()))
                    || value.is_none() && entity.is_some()
            }
        };
        if gap {
            return Err(SemanticError::incorrect(
                "index/non-contiguous-prefix",
                "index prefix components must be contiguous from the left",
            ));
        }
        Ok(())
    }
}
impl TryFrom<&IndexPrefix> for IndexBoundary {
    type Error = SemanticError;
    fn try_from(p: &IndexPrefix) -> Result<Self, SemanticError> {
        p.validate()?;
        Ok(match p {
            IndexPrefix::Eavt {
                entity,
                attribute: None,
                value: None,
            } => Self::Eavt(IndexComponents::One(*entity)),
            IndexPrefix::Eavt {
                entity,
                attribute: Some(attribute),
                value: None,
            } => Self::Eavt(IndexComponents::Two(*entity, *attribute)),
            IndexPrefix::Eavt {
                entity,
                attribute: Some(attribute),
                value: Some(value),
            } => Self::Eavt(IndexComponents::Three(*entity, *attribute, value.clone())),
            IndexPrefix::Aevt {
                attribute,
                entity: None,
                value: None,
            } => Self::Aevt(IndexComponents::One(*attribute)),
            IndexPrefix::Aevt {
                attribute,
                entity: Some(entity),
                value: None,
            } => Self::Aevt(IndexComponents::Two(*attribute, *entity)),
            IndexPrefix::Aevt {
                attribute,
                entity: Some(entity),
                value: Some(value),
            } => Self::Aevt(IndexComponents::Three(*attribute, *entity, value.clone())),
            IndexPrefix::Avet {
                attribute,
                value: None,
                entity: None,
            } => Self::Avet(IndexComponents::One(*attribute)),
            IndexPrefix::Avet {
                attribute,
                value: Some(value),
                entity: None,
            } => Self::Avet(IndexComponents::Two(*attribute, value.clone())),
            IndexPrefix::Avet {
                attribute,
                value: Some(value),
                entity: Some(entity),
            } => Self::Avet(IndexComponents::Three(*attribute, value.clone(), *entity)),
            IndexPrefix::Vaet {
                value,
                attribute: None,
                entity: None,
            } => Self::Vaet(IndexComponents::One(value.clone())),
            IndexPrefix::Vaet {
                value,
                attribute: Some(attribute),
                entity: None,
            } => Self::Vaet(IndexComponents::Two(value.clone(), *attribute)),
            IndexPrefix::Vaet {
                value,
                attribute: Some(attribute),
                entity: Some(entity),
            } => Self::Vaet(IndexComponents::Three(value.clone(), *attribute, *entity)),
            IndexPrefix::Saevt {
                source,
                attribute: None,
                entity: None,
                value: None,
            } => Self::Saevt(IndexComponents::One(*source)),
            IndexPrefix::Saevt {
                source,
                attribute: Some(attribute),
                entity: None,
                value: None,
            } => Self::Saevt(IndexComponents::Two(*source, *attribute)),
            IndexPrefix::Saevt {
                source,
                attribute: Some(attribute),
                entity: Some(entity),
                value: None,
            } => Self::Saevt(IndexComponents::Three(*source, *attribute, *entity)),
            IndexPrefix::Saevt {
                source,
                attribute: Some(attribute),
                entity: Some(entity),
                value: Some(value),
            } => Self::Saevt(IndexComponents::Four(
                *source,
                *attribute,
                *entity,
                value.clone(),
            )),
            IndexPrefix::Savet {
                source,
                attribute: None,
                value: None,
                entity: None,
            } => Self::Savet(IndexComponents::One(*source)),
            IndexPrefix::Savet {
                source,
                attribute: Some(attribute),
                value: None,
                entity: None,
            } => Self::Savet(IndexComponents::Two(*source, *attribute)),
            IndexPrefix::Savet {
                source,
                attribute: Some(attribute),
                value: Some(value),
                entity: None,
            } => Self::Savet(IndexComponents::Three(*source, *attribute, value.clone())),
            IndexPrefix::Savet {
                source,
                attribute: Some(attribute),
                value: Some(value),
                entity: Some(entity),
            } => Self::Savet(IndexComponents::Four(
                *source,
                *attribute,
                value.clone(),
                *entity,
            )),
            _ => unreachable!("validated prefix has no gap"),
        })
    }
}
fn optional_value(a: &Option<Value>, b: &Option<Value>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(a), Some(b)) => a.index_cmp(b),
    }
}
impl Ord for IndexPrefix {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.order() as u8)
            .cmp(&(other.order() as u8))
            .then_with(|| match (self, other) {
                (
                    Self::Eavt {
                        entity: l_entity,
                        attribute: l_attribute,
                        value: l_value,
                    },
                    Self::Eavt {
                        entity: r_entity,
                        attribute: r_attribute,
                        value: r_value,
                    },
                ) => l_entity
                    .cmp(r_entity)
                    .then_with(|| l_attribute.cmp(r_attribute))
                    .then_with(|| optional_value(l_value, r_value)),
                (
                    Self::Aevt {
                        attribute: l_attribute,
                        entity: l_entity,
                        value: l_value,
                    },
                    Self::Aevt {
                        attribute: r_attribute,
                        entity: r_entity,
                        value: r_value,
                    },
                ) => l_attribute
                    .cmp(r_attribute)
                    .then_with(|| l_entity.cmp(r_entity))
                    .then_with(|| optional_value(l_value, r_value)),
                (
                    Self::Avet {
                        attribute: l_attribute,
                        value: l_value,
                        entity: l_entity,
                    },
                    Self::Avet {
                        attribute: r_attribute,
                        value: r_value,
                        entity: r_entity,
                    },
                ) => l_attribute
                    .cmp(r_attribute)
                    .then_with(|| optional_value(l_value, r_value))
                    .then_with(|| l_entity.cmp(r_entity)),
                (
                    Self::Vaet {
                        value: l_value,
                        attribute: l_attribute,
                        entity: l_entity,
                    },
                    Self::Vaet {
                        value: r_value,
                        attribute: r_attribute,
                        entity: r_entity,
                    },
                ) => l_value
                    .index_cmp(r_value)
                    .then_with(|| l_attribute.cmp(r_attribute))
                    .then_with(|| l_entity.cmp(r_entity)),
                (
                    Self::Saevt {
                        source: l_source,
                        attribute: l_attribute,
                        entity: l_entity,
                        value: l_value,
                    },
                    Self::Saevt {
                        source: r_source,
                        attribute: r_attribute,
                        entity: r_entity,
                        value: r_value,
                    },
                ) => l_source
                    .cmp(r_source)
                    .then_with(|| l_attribute.cmp(r_attribute))
                    .then_with(|| l_entity.cmp(r_entity))
                    .then_with(|| optional_value(l_value, r_value)),
                (
                    Self::Savet {
                        source: l_source,
                        attribute: l_attribute,
                        value: l_value,
                        entity: l_entity,
                    },
                    Self::Savet {
                        source: r_source,
                        attribute: r_attribute,
                        value: r_value,
                        entity: r_entity,
                    },
                ) => l_source
                    .cmp(r_source)
                    .then_with(|| l_attribute.cmp(r_attribute))
                    .then_with(|| optional_value(l_value, r_value))
                    .then_with(|| l_entity.cmp(r_entity)),
                _ => Ordering::Equal,
            })
    }
}
impl PartialOrd for IndexPrefix {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
pub(crate) fn compare_prefix(d: &Datom, p: &IndexPrefix) -> Ordering {
    match p {
        IndexPrefix::Eavt {
            entity,
            attribute,
            value,
        } => d
            .entity
            .cmp(entity)
            .then_with(|| attribute.map_or(Ordering::Equal, |v| d.attribute.cmp(&v)))
            .then_with(|| {
                value
                    .as_ref()
                    .map_or(Ordering::Equal, |v| d.value.index_cmp(v))
            }),
        IndexPrefix::Aevt {
            attribute,
            entity,
            value,
        } => d
            .attribute
            .cmp(attribute)
            .then_with(|| entity.map_or(Ordering::Equal, |v| d.entity.cmp(&v)))
            .then_with(|| {
                value
                    .as_ref()
                    .map_or(Ordering::Equal, |v| d.value.index_cmp(v))
            }),
        IndexPrefix::Avet {
            attribute,
            value,
            entity,
        } => d
            .attribute
            .cmp(attribute)
            .then_with(|| {
                value
                    .as_ref()
                    .map_or(Ordering::Equal, |v| d.value.index_cmp(v))
            })
            .then_with(|| entity.map_or(Ordering::Equal, |v| d.entity.cmp(&v))),
        IndexPrefix::Vaet {
            value,
            attribute,
            entity,
        } => d
            .value
            .index_cmp(value)
            .then_with(|| attribute.map_or(Ordering::Equal, |v| d.attribute.cmp(&v)))
            .then_with(|| entity.map_or(Ordering::Equal, |v| d.entity.cmp(&v))),
        IndexPrefix::Saevt {
            source,
            attribute,
            entity,
            value,
        } => d
            .source
            .cmp(source)
            .then_with(|| attribute.map_or(Ordering::Equal, |v| d.attribute.cmp(&v)))
            .then_with(|| entity.map_or(Ordering::Equal, |v| d.entity.cmp(&v)))
            .then_with(|| {
                value
                    .as_ref()
                    .map_or(Ordering::Equal, |v| d.value.index_cmp(v))
            }),
        IndexPrefix::Savet {
            source,
            attribute,
            value,
            entity,
        } => d
            .source
            .cmp(source)
            .then_with(|| attribute.map_or(Ordering::Equal, |v| d.attribute.cmp(&v)))
            .then_with(|| {
                value
                    .as_ref()
                    .map_or(Ordering::Equal, |v| d.value.index_cmp(v))
            })
            .then_with(|| entity.map_or(Ordering::Equal, |v| d.entity.cmp(&v))),
    }
}
