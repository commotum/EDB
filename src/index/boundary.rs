//! Public index prefix and virtual-boundary semantics.
use crate::model::identity::validate_permanent_eid;
use crate::{Datom, IndexOrder, SemanticError, Value, t_to_tx, tx_to_t};
use std::cmp::Ordering;

/// A transaction component supplied to a raw index boundary.
///
/// Datomic accepts both a logical basis `t` and the corresponding reified
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

/// A left-contiguous sequence of zero through four typed index components.
///
/// The type parameters are the component types *in index order*. An enum is
/// used instead of four `Option`s so a gap such as `[a, nil, v]` cannot be
/// represented. `Four` deliberately stops at T: assertion/retraction is not a
/// fifth public raw-index component.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IndexComponents<First, Second, Third, Fourth> {
    Empty,
    One(First),
    Two(First, Second),
    Three(First, Second, Third),
    Four(First, Second, Third, Fourth),
}

impl<First, Second, Third, Fourth> IndexComponents<First, Second, Third, Fourth> {
    pub const fn len(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::One(_) => 1,
            Self::Two(_, _) => 2,
            Self::Three(_, _, _) => 3,
            Self::Four(_, _, _, _) => 4,
        }
    }

    pub const fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }
}

/// A typed virtual position in one of Datomic's four raw index orders.
///
/// E/A/V/T component order is encoded in each variant. E and A are already
/// resolved numeric ids and V is already normalized for the attribute. The
/// immutable database API owns the schema/ident/lookup-ref normalization that
/// produces this type; [`IndexBoundary::validate`] is the storage-independent
/// final check before a cursor uses it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IndexBoundary {
    Eavt(IndexComponents<u64, u32, Value, IndexTransaction>),
    Aevt(IndexComponents<u32, u64, Value, IndexTransaction>),
    Avet(IndexComponents<u32, Value, u64, IndexTransaction>),
    Vaet(IndexComponents<Value, u32, u64, IndexTransaction>),
}

impl IndexBoundary {
    pub const fn order(&self) -> IndexOrder {
        match self {
            Self::Eavt(_) => IndexOrder::Eavt,
            Self::Aevt(_) => IndexOrder::Aevt,
            Self::Avet(_) => IndexOrder::Avet,
            Self::Vaet(_) => IndexOrder::Vaet,
        }
    }

    pub const fn len(&self) -> usize {
        match self {
            Self::Eavt(components) => components.len(),
            Self::Aevt(components) => components.len(),
            Self::Avet(components) => components.len(),
            Self::Vaet(components) => components.len(),
        }
    }

    pub const fn is_empty(&self) -> bool {
        match self {
            Self::Eavt(components) => components.is_empty(),
            Self::Aevt(components) => components.is_empty(),
            Self::Avet(components) => components.is_empty(),
            Self::Vaet(components) => components.is_empty(),
        }
    }

    /// The qualified attribute whose physical AVET readiness must be checked.
    /// An empty AVET boundary intentionally has no attribute qualification.
    pub const fn avet_attribute(&self) -> Option<u32> {
        match self {
            Self::Avet(IndexComponents::One(attribute))
            | Self::Avet(IndexComponents::Two(attribute, _))
            | Self::Avet(IndexComponents::Three(attribute, _, _))
            | Self::Avet(IndexComponents::Four(attribute, _, _, _)) => Some(*attribute),
            _ => None,
        }
    }

    /// Validate resolved identifiers and normalize T/Tx exactly once before
    /// traversal. Schema-directed V validation and ident/lookup resolution
    /// remain at the immutable database boundary, where the exact schema and
    /// ident map are available.
    pub fn validate(&self) -> Result<(), SemanticError> {
        self.normalized().map(|_| ())
    }

    pub(crate) fn normalized(&self) -> Result<NormalizedIndexBoundary, SemanticError> {
        fn entity(entity: u64) -> Result<u64, SemanticError> {
            validate_permanent_eid(entity)?;
            Ok(entity)
        }

        fn normalized_value(value: &Value) -> Result<Value, SemanticError> {
            if let Value::Ref(entity) = value {
                validate_permanent_eid(*entity)?;
            }
            Ok(value.clone())
        }

        fn vaet_value(value: &Value) -> Result<Value, SemanticError> {
            if !matches!(value, Value::Ref(_)) {
                return Err(SemanticError::incorrect(
                    "index/vaet-value-not-ref",
                    "VAET boundary value must resolve to an entity reference",
                ));
            }
            normalized_value(value)
        }

        Ok(match self {
            Self::Eavt(components) => NormalizedIndexBoundary::Eavt(match components {
                IndexComponents::Empty => IndexComponents::Empty,
                IndexComponents::One(e) => IndexComponents::One(entity(*e)?),
                IndexComponents::Two(e, a) => IndexComponents::Two(entity(*e)?, *a),
                IndexComponents::Three(e, a, v) => {
                    IndexComponents::Three(entity(*e)?, *a, normalized_value(v)?)
                }
                IndexComponents::Four(e, a, v, t) => {
                    IndexComponents::Four(entity(*e)?, *a, normalized_value(v)?, t.stored_tx()?)
                }
            }),
            Self::Aevt(components) => NormalizedIndexBoundary::Aevt(match components {
                IndexComponents::Empty => IndexComponents::Empty,
                IndexComponents::One(a) => IndexComponents::One(*a),
                IndexComponents::Two(a, e) => IndexComponents::Two(*a, entity(*e)?),
                IndexComponents::Three(a, e, v) => {
                    IndexComponents::Three(*a, entity(*e)?, normalized_value(v)?)
                }
                IndexComponents::Four(a, e, v, t) => {
                    IndexComponents::Four(*a, entity(*e)?, normalized_value(v)?, t.stored_tx()?)
                }
            }),
            Self::Avet(components) => NormalizedIndexBoundary::Avet(match components {
                IndexComponents::Empty => IndexComponents::Empty,
                IndexComponents::One(a) => IndexComponents::One(*a),
                IndexComponents::Two(a, v) => IndexComponents::Two(*a, normalized_value(v)?),
                IndexComponents::Three(a, v, e) => {
                    IndexComponents::Three(*a, normalized_value(v)?, entity(*e)?)
                }
                IndexComponents::Four(a, v, e, t) => {
                    IndexComponents::Four(*a, normalized_value(v)?, entity(*e)?, t.stored_tx()?)
                }
            }),
            Self::Vaet(components) => NormalizedIndexBoundary::Vaet(match components {
                IndexComponents::Empty => IndexComponents::Empty,
                IndexComponents::One(v) => IndexComponents::One(vaet_value(v)?),
                IndexComponents::Two(v, a) => IndexComponents::Two(vaet_value(v)?, *a),
                IndexComponents::Three(v, a, e) => {
                    IndexComponents::Three(vaet_value(v)?, *a, entity(*e)?)
                }
                IndexComponents::Four(v, a, e, t) => {
                    IndexComponents::Four(vaet_value(v)?, *a, entity(*e)?, t.stored_tx()?)
                }
            }),
        })
    }

    /// Boundary needed to derive a current forward seek from raw history.
    /// A T-qualified seek must observe the complete logical E/A/V group
    /// before deciding which filtered/current datoms survive, then apply the
    /// original four-component boundary to those survivors.
    pub(crate) fn current_group_start(&self) -> Self {
        match self {
            Self::Eavt(IndexComponents::Four(e, a, v, _)) => {
                Self::Eavt(IndexComponents::Three(*e, *a, v.clone()))
            }
            Self::Aevt(IndexComponents::Four(a, e, v, _)) => {
                Self::Aevt(IndexComponents::Three(*a, *e, v.clone()))
            }
            Self::Avet(IndexComponents::Four(a, v, e, _)) => {
                Self::Avet(IndexComponents::Three(*a, v.clone(), *e))
            }
            Self::Vaet(IndexComponents::Four(v, a, e, _)) => {
                Self::Vaet(IndexComponents::Three(v.clone(), *a, *e))
            }
            _ => self.clone(),
        }
    }
}

/// Cursor-ready form with the public T/Tx choice normalized to the canonical
/// transaction entity stored by [`Datom`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum NormalizedIndexBoundary {
    Eavt(IndexComponents<u64, u32, Value, u64>),
    Aevt(IndexComponents<u32, u64, Value, u64>),
    Avet(IndexComponents<u32, Value, u64, u64>),
    Vaet(IndexComponents<Value, u32, u64, u64>),
    /// Internal forward position after every datom equal to the supplied
    /// logical prefix. It changes neither public components nor stored keys.
    After(Box<Self>),
}

impl NormalizedIndexBoundary {
    pub(crate) fn order(&self) -> IndexOrder {
        match self {
            Self::Eavt(_) => IndexOrder::Eavt,
            Self::Aevt(_) => IndexOrder::Aevt,
            Self::Avet(_) => IndexOrder::Avet,
            Self::Vaet(_) => IndexOrder::Vaet,
            Self::After(boundary) => boundary.order(),
        }
    }

    pub(crate) fn after_prefix(self) -> Self {
        match self {
            Self::After(_) => self,
            _ => Self::After(Box::new(self)),
        }
    }

    pub(crate) fn is_after_prefix(&self) -> bool {
        matches!(self, Self::After(_))
    }

    pub(crate) fn unbiased(&self) -> &Self {
        match self {
            Self::After(boundary) => boundary,
            _ => self,
        }
    }

    /// Compare an actual datom to this virtual boundary in index order.
    /// Missing suffix components compare equal, which is the semantic key to
    /// placing forward seek before the lowest match and reverse seek after the
    /// highest match without manufacturing minimum/maximum Values. This is the
    /// direct Rust form of recovered `reverse-datum-spec` (`db.clj:2040-2063`),
    /// whose increments/MAX fields and `asserting=false` manufacture the same
    /// upper position for a concrete-datum tree API.
    pub(crate) fn compare_datom(&self, datom: &Datom) -> Ordering {
        fn tx(datom: &Datom, boundary_tx: u64) -> Ordering {
            // Transaction sorts descending in every native index.
            boundary_tx.cmp(&datom.tx)
        }

        match self {
            Self::After(boundary) => boundary.compare_datom(datom).then(Ordering::Less),
            Self::Eavt(components) => match components {
                IndexComponents::Empty => Ordering::Equal,
                IndexComponents::One(e) => datom.entity.cmp(e),
                IndexComponents::Two(e, a) => {
                    datom.entity.cmp(e).then_with(|| datom.attribute.cmp(a))
                }
                IndexComponents::Three(e, a, v) => datom
                    .entity
                    .cmp(e)
                    .then_with(|| datom.attribute.cmp(a))
                    .then_with(|| datom.value.index_cmp(v)),
                IndexComponents::Four(e, a, v, t) => datom
                    .entity
                    .cmp(e)
                    .then_with(|| datom.attribute.cmp(a))
                    .then_with(|| datom.value.index_cmp(v))
                    .then_with(|| tx(datom, *t)),
            },
            Self::Aevt(components) => match components {
                IndexComponents::Empty => Ordering::Equal,
                IndexComponents::One(a) => datom.attribute.cmp(a),
                IndexComponents::Two(a, e) => {
                    datom.attribute.cmp(a).then_with(|| datom.entity.cmp(e))
                }
                IndexComponents::Three(a, e, v) => datom
                    .attribute
                    .cmp(a)
                    .then_with(|| datom.entity.cmp(e))
                    .then_with(|| datom.value.index_cmp(v)),
                IndexComponents::Four(a, e, v, t) => datom
                    .attribute
                    .cmp(a)
                    .then_with(|| datom.entity.cmp(e))
                    .then_with(|| datom.value.index_cmp(v))
                    .then_with(|| tx(datom, *t)),
            },
            Self::Avet(components) => match components {
                IndexComponents::Empty => Ordering::Equal,
                IndexComponents::One(a) => datom.attribute.cmp(a),
                IndexComponents::Two(a, v) => datom
                    .attribute
                    .cmp(a)
                    .then_with(|| datom.value.index_cmp(v)),
                IndexComponents::Three(a, v, e) => datom
                    .attribute
                    .cmp(a)
                    .then_with(|| datom.value.index_cmp(v))
                    .then_with(|| datom.entity.cmp(e)),
                IndexComponents::Four(a, v, e, t) => datom
                    .attribute
                    .cmp(a)
                    .then_with(|| datom.value.index_cmp(v))
                    .then_with(|| datom.entity.cmp(e))
                    .then_with(|| tx(datom, *t)),
            },
            Self::Vaet(components) => match components {
                IndexComponents::Empty => Ordering::Equal,
                IndexComponents::One(v) => datom.value.index_cmp(v),
                IndexComponents::Two(v, a) => datom
                    .value
                    .index_cmp(v)
                    .then_with(|| datom.attribute.cmp(a)),
                IndexComponents::Three(v, a, e) => datom
                    .value
                    .index_cmp(v)
                    .then_with(|| datom.attribute.cmp(a))
                    .then_with(|| datom.entity.cmp(e)),
                IndexComponents::Four(v, a, e, t) => datom
                    .value
                    .index_cmp(v)
                    .then_with(|| datom.attribute.cmp(a))
                    .then_with(|| datom.entity.cmp(e))
                    .then_with(|| tx(datom, *t)),
            },
        }
    }

    /// Lower-bound comparison used while deriving a current value from raw
    /// recent history. A four-component boundary must begin at the start of
    /// its complete logical E/A/V group: only after selecting that group's
    /// endpoint winner is it safe to apply the T boundary. Starting within
    /// the history group could resurrect an older assertion hidden by a
    /// newer operation that sorts just before the requested T.
    pub(crate) fn compare_current_group_start(&self, datom: &Datom) -> Ordering {
        match self {
            Self::After(boundary)
                if matches!(
                    boundary.as_ref(),
                    Self::Eavt(IndexComponents::Four(..))
                        | Self::Aevt(IndexComponents::Four(..))
                        | Self::Avet(IndexComponents::Four(..))
                        | Self::Vaet(IndexComponents::Four(..))
                ) =>
            {
                // T-qualified current reads must still select the winner
                // from its entire group before applying an exclusive T.
                boundary.compare_current_group_start(datom)
            }
            Self::Eavt(IndexComponents::Four(e, a, v, _)) => datom
                .entity
                .cmp(e)
                .then_with(|| datom.attribute.cmp(a))
                .then_with(|| datom.value.index_cmp(v)),
            Self::Aevt(IndexComponents::Four(a, e, v, _)) => datom
                .attribute
                .cmp(a)
                .then_with(|| datom.entity.cmp(e))
                .then_with(|| datom.value.index_cmp(v)),
            Self::Avet(IndexComponents::Four(a, v, e, _)) => datom
                .attribute
                .cmp(a)
                .then_with(|| datom.value.index_cmp(v))
                .then_with(|| datom.entity.cmp(e)),
            Self::Vaet(IndexComponents::Four(v, a, e, _)) => datom
                .value
                .index_cmp(v)
                .then_with(|| datom.attribute.cmp(a))
                .then_with(|| datom.entity.cmp(e)),
            _ => self.compare_datom(datom),
        }
    }
}

/// A typed prefix for one of Datomic's four index orders.
///
/// The shape prevents callers from accidentally supplying components in the
/// wrong order. Optional components must be contiguous from the left.
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
}

impl PartialOrd for IndexPrefix {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for IndexPrefix {
    fn cmp(&self, other: &Self) -> Ordering {
        prefix_rank(self)
            .cmp(&prefix_rank(other))
            .then_with(|| match (self, other) {
                (
                    Self::Eavt {
                        entity: left_entity,
                        attribute: left_attribute,
                        value: left_value,
                    },
                    Self::Eavt {
                        entity: right_entity,
                        attribute: right_attribute,
                        value: right_value,
                    },
                ) => left_entity
                    .cmp(right_entity)
                    .then_with(|| left_attribute.cmp(right_attribute))
                    .then_with(|| compare_optional_value(left_value, right_value)),
                (
                    Self::Aevt {
                        attribute: left_attribute,
                        entity: left_entity,
                        value: left_value,
                    },
                    Self::Aevt {
                        attribute: right_attribute,
                        entity: right_entity,
                        value: right_value,
                    },
                ) => left_attribute
                    .cmp(right_attribute)
                    .then_with(|| left_entity.cmp(right_entity))
                    .then_with(|| compare_optional_value(left_value, right_value)),
                (
                    Self::Avet {
                        attribute: left_attribute,
                        value: left_value,
                        entity: left_entity,
                    },
                    Self::Avet {
                        attribute: right_attribute,
                        value: right_value,
                        entity: right_entity,
                    },
                ) => left_attribute
                    .cmp(right_attribute)
                    .then_with(|| compare_optional_value(left_value, right_value))
                    .then_with(|| left_entity.cmp(right_entity)),
                (
                    Self::Vaet {
                        value: left_value,
                        attribute: left_attribute,
                        entity: left_entity,
                    },
                    Self::Vaet {
                        value: right_value,
                        attribute: right_attribute,
                        entity: right_entity,
                    },
                ) => left_value
                    .index_cmp(right_value)
                    .then_with(|| left_attribute.cmp(right_attribute))
                    .then_with(|| left_entity.cmp(right_entity)),
                _ => Ordering::Equal,
            })
    }
}

fn prefix_rank(prefix: &IndexPrefix) -> u8 {
    match prefix {
        IndexPrefix::Eavt { .. } => 0,
        IndexPrefix::Aevt { .. } => 1,
        IndexPrefix::Avet { .. } => 2,
        IndexPrefix::Vaet { .. } => 3,
    }
}

fn compare_optional_value(left: &Option<Value>, right: &Option<Value>) -> Ordering {
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(left), Some(right)) => left.index_cmp(right),
    }
}

impl IndexPrefix {
    pub fn order(&self) -> IndexOrder {
        match self {
            Self::Eavt { .. } => IndexOrder::Eavt,
            Self::Aevt { .. } => IndexOrder::Aevt,
            Self::Avet { .. } => IndexOrder::Avet,
            Self::Vaet { .. } => IndexOrder::Vaet,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), SemanticError> {
        let has_gap = match self {
            Self::Eavt {
                attribute, value, ..
            } => attribute.is_none() && value.is_some(),
            Self::Aevt { entity, value, .. } => entity.is_none() && value.is_some(),
            Self::Avet { value, entity, .. } => value.is_none() && entity.is_some(),
            Self::Vaet {
                attribute, entity, ..
            } => attribute.is_none() && entity.is_some(),
        };
        if has_gap {
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

    fn try_from(prefix: &IndexPrefix) -> Result<Self, Self::Error> {
        prefix.validate()?;
        Ok(match prefix {
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
            // `validate` rejects these gap shapes before this match. Keeping
            // the arm explicit prevents a future prefix variant from silently
            // acquiring different matching semantics.
            _ => unreachable!("validated IndexPrefix has no component gap"),
        })
    }
}

pub(crate) fn compare_prefix(datom: &Datom, prefix: &IndexPrefix) -> Ordering {
    match prefix {
        IndexPrefix::Eavt {
            entity,
            attribute,
            value,
        } => datom
            .entity
            .cmp(entity)
            .then_with(|| attribute.map_or(Ordering::Equal, |a| datom.attribute.cmp(&a)))
            .then_with(|| {
                value
                    .as_ref()
                    .map_or(Ordering::Equal, |v| datom.value.index_cmp(v))
            }),
        IndexPrefix::Aevt {
            attribute,
            entity,
            value,
        } => datom
            .attribute
            .cmp(attribute)
            .then_with(|| entity.map_or(Ordering::Equal, |e| datom.entity.cmp(&e)))
            .then_with(|| {
                value
                    .as_ref()
                    .map_or(Ordering::Equal, |v| datom.value.index_cmp(v))
            }),
        IndexPrefix::Avet {
            attribute,
            value,
            entity,
        } => datom
            .attribute
            .cmp(attribute)
            .then_with(|| {
                value
                    .as_ref()
                    .map_or(Ordering::Equal, |v| datom.value.index_cmp(v))
            })
            .then_with(|| entity.map_or(Ordering::Equal, |e| datom.entity.cmp(&e))),
        IndexPrefix::Vaet {
            value,
            attribute,
            entity,
        } => datom
            .value
            .index_cmp(value)
            .then_with(|| attribute.map_or(Ordering::Equal, |a| datom.attribute.cmp(&a)))
            .then_with(|| entity.map_or(Ordering::Equal, |e| datom.entity.cmp(&e))),
    }
}
