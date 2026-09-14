//! Exact-value identity/lookup resolution and raw-index boundary normalization.
use super::value::{DatabaseValue, ReadBasis};
use crate::{
    AttributeName, DB_IDENT, Datom, EntityIdentifier, ErrorCategory, IndexBoundary,
    IndexComponents, IndexPrefix, IndexTransaction, Schema, SemanticError, TupleSpec, Value,
    ValueType, eid_to_eidx, schema_eid_to_attr_id,
};

/// A value component accepted at the raw-index API boundary.
///
/// `Stored` keeps an ordinary value unambiguous, while `Entity` requests
/// ident or lookup-ref resolution against this exact immutable database
/// value. `Tuple` permits that same distinction recursively in ref-typed
/// tuple slots; `None` slots remain tuple nils. Read boundaries deliberately
/// have no tempid form. Tuple boundaries may contain a leading subset of the
/// schema's slots, including an empty subset. They are virtual positions, not
/// stored assertions: a shorter tuple sorts before its longer extensions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RawIndexValue {
    Stored(Value),
    Entity(EntityIdentifier),
    Tuple(Vec<Option<RawIndexValue>>),
}

impl From<Value> for RawIndexValue {
    fn from(value: Value) -> Self {
        Self::Stored(value)
    }
}

impl From<EntityIdentifier> for RawIndexValue {
    fn from(identifier: EntityIdentifier) -> Self {
        Self::Entity(identifier)
    }
}

impl DatabaseValue {
    /// Whether an attribute's AVET projection is usable at this exact database
    /// value. This differs from the attribute's configured `indexed`/`unique`
    /// schema flags while native background backfill is pending.
    ///
    /// Known attributes without AVET return `false`; unknown attribute ids or
    /// idents return `database/unknown-attribute`. This does not wait for an
    /// index build or refresh this immutable value. Temporal/filter views
    /// retain their captured basis's schema and physical readiness.
    pub fn has_avet(&self, attribute: &AttributeName) -> Result<bool, SemanticError> {
        Ok(self.physical_avet_ready(self.resolve_attribute(attribute)?))
    }

    /// Whether the physical AVET projection for one logically indexed
    /// attribute is complete at this immutable basis. Logical schema alone is
    /// insufficient while a background backfill is pending.
    pub(crate) fn physical_avet_ready(&self, attribute: u32) -> bool {
        if !schema_has_avet(self.schema(), attribute) {
            return false;
        }
        match &self.basis {
            ReadBasis::Eager(_) => true,
            ReadBasis::Block(snapshot) => snapshot.avet_ready(attribute),
            ReadBasis::TransactionOverlay(overlay) => {
                overlay.newly_enabled_avet.binary_search(&attribute).is_ok()
                    || overlay.base.physical_avet_ready(attribute)
            }
        }
    }

    pub fn values(&self, entity: u64, attribute: u32) -> Result<Vec<Value>, SemanticError> {
        Ok(self
            .datoms_with_prefix(&IndexPrefix::Eavt {
                entity,
                attribute: Some(attribute),
                value: None,
            })?
            .into_iter()
            .map(|datom| datom.value)
            .collect())
    }

    /// Resolve a lookup ref against this exact value. `:db/ident` follows the
    /// recovered dictionary fast path; all other identities use windowed AVET.
    pub fn lookup(&self, attribute: u32, value: &Value) -> Result<Option<u64>, SemanticError> {
        self.lookup_with_control(attribute, value, &mut |_| Ok(true))
    }

    pub(crate) fn lookup_with_control(
        &self,
        attribute: u32,
        value: &Value,
        control: &mut dyn FnMut(Option<&Datom>) -> Result<bool, SemanticError>,
    ) -> Result<Option<u64>, SemanticError> {
        let schema = self.schema().attribute(attribute)?;
        if schema.unique.is_none() {
            return Err(SemanticError::incorrect(
                "transaction/lookup-non-unique",
                format!("attribute {} is not unique", schema.ident.qualified_name()),
            ));
        }
        self.schema().validate_value(schema, value)?;
        if u64::from(attribute) == DB_IDENT {
            let Value::Keyword(ident) = value else {
                return Ok(None);
            };
            return Ok(self.entid(ident));
        }
        Ok(self
            .prefix_cursor(&IndexPrefix::Avet {
                attribute,
                value: Some(value.clone()),
                entity: None,
            })?
            .next_with_control(control)
            .transpose()?
            .map(|datom| datom.entity))
    }

    /// Normalize a zero-to-four-component EAVT boundary against this exact
    /// database value.
    pub fn eavt_boundary(
        &self,
        components: IndexComponents<
            EntityIdentifier,
            AttributeName,
            RawIndexValue,
            IndexTransaction,
        >,
    ) -> Result<IndexBoundary, SemanticError> {
        let components = match components {
            IndexComponents::Empty => IndexComponents::Empty,
            IndexComponents::One(entity) => {
                IndexComponents::One(self.require_index_entity(&entity)?)
            }
            IndexComponents::Two(entity, attribute) => IndexComponents::Two(
                self.require_index_entity(&entity)?,
                self.resolve_attribute(&attribute)?,
            ),
            IndexComponents::Three(entity, attribute, value) => {
                let entity = self.require_index_entity(&entity)?;
                let attribute = self.resolve_attribute(&attribute)?;
                let value = self.normalize_index_value(attribute, value)?;
                IndexComponents::Three(entity, attribute, value)
            }
            IndexComponents::Four(entity, attribute, value, transaction) => {
                let entity = self.require_index_entity(&entity)?;
                let attribute = self.resolve_attribute(&attribute)?;
                let value = self.normalize_index_value(attribute, value)?;
                IndexComponents::Four(entity, attribute, value, transaction)
            }
        };
        Self::validated_boundary(IndexBoundary::Eavt(components))
    }

    /// Normalize a zero-to-four-component AEVT boundary against this exact
    /// database value.
    pub fn aevt_boundary(
        &self,
        components: IndexComponents<
            AttributeName,
            EntityIdentifier,
            RawIndexValue,
            IndexTransaction,
        >,
    ) -> Result<IndexBoundary, SemanticError> {
        let components = match components {
            IndexComponents::Empty => IndexComponents::Empty,
            IndexComponents::One(attribute) => {
                IndexComponents::One(self.resolve_attribute(&attribute)?)
            }
            IndexComponents::Two(attribute, entity) => IndexComponents::Two(
                self.resolve_attribute(&attribute)?,
                self.require_index_entity(&entity)?,
            ),
            IndexComponents::Three(attribute, entity, value) => {
                let attribute = self.resolve_attribute(&attribute)?;
                let entity = self.require_index_entity(&entity)?;
                let value = self.normalize_index_value(attribute, value)?;
                IndexComponents::Three(attribute, entity, value)
            }
            IndexComponents::Four(attribute, entity, value, transaction) => {
                let attribute = self.resolve_attribute(&attribute)?;
                let entity = self.require_index_entity(&entity)?;
                let value = self.normalize_index_value(attribute, value)?;
                IndexComponents::Four(attribute, entity, value, transaction)
            }
        };
        Self::validated_boundary(IndexBoundary::Aevt(components))
    }

    /// Normalize a zero-to-four-component AVET boundary and require the
    /// qualified attribute's logical membership and physical readiness.
    pub fn avet_boundary(
        &self,
        components: IndexComponents<
            AttributeName,
            RawIndexValue,
            EntityIdentifier,
            IndexTransaction,
        >,
    ) -> Result<IndexBoundary, SemanticError> {
        let components = match components {
            IndexComponents::Empty => IndexComponents::Empty,
            IndexComponents::One(attribute) => {
                IndexComponents::One(self.resolve_ready_avet_attribute(&attribute)?)
            }
            IndexComponents::Two(attribute, value) => {
                let attribute = self.resolve_ready_avet_attribute(&attribute)?;
                let value = self.normalize_index_value(attribute, value)?;
                IndexComponents::Two(attribute, value)
            }
            IndexComponents::Three(attribute, value, entity) => {
                let attribute = self.resolve_ready_avet_attribute(&attribute)?;
                let value = self.normalize_index_value(attribute, value)?;
                let entity = self.require_index_entity(&entity)?;
                IndexComponents::Three(attribute, value, entity)
            }
            IndexComponents::Four(attribute, value, entity, transaction) => {
                let attribute = self.resolve_ready_avet_attribute(&attribute)?;
                let value = self.normalize_index_value(attribute, value)?;
                let entity = self.require_index_entity(&entity)?;
                IndexComponents::Four(attribute, value, entity, transaction)
            }
        };
        Self::validated_boundary(IndexBoundary::Avet(components))
    }

    /// Normalize a zero-to-four-component VAET boundary. Its leading value is
    /// always an entity reference, independent of whether an attribute suffix
    /// is present.
    pub fn vaet_boundary(
        &self,
        components: IndexComponents<
            RawIndexValue,
            AttributeName,
            EntityIdentifier,
            IndexTransaction,
        >,
    ) -> Result<IndexBoundary, SemanticError> {
        let components = match components {
            IndexComponents::Empty => IndexComponents::Empty,
            IndexComponents::One(value) => IndexComponents::One(self.normalize_vaet_value(value)?),
            IndexComponents::Two(value, attribute) => IndexComponents::Two(
                self.normalize_vaet_value(value)?,
                self.resolve_attribute(&attribute)?,
            ),
            IndexComponents::Three(value, attribute, entity) => IndexComponents::Three(
                self.normalize_vaet_value(value)?,
                self.resolve_attribute(&attribute)?,
                self.require_index_entity(&entity)?,
            ),
            IndexComponents::Four(value, attribute, entity, transaction) => IndexComponents::Four(
                self.normalize_vaet_value(value)?,
                self.resolve_attribute(&attribute)?,
                self.require_index_entity(&entity)?,
                transaction,
            ),
        };
        Self::validated_boundary(IndexBoundary::Vaet(components))
    }

    pub(super) fn validated_boundary(
        boundary: IndexBoundary,
    ) -> Result<IndexBoundary, SemanticError> {
        boundary.validate()?;
        Ok(boundary)
    }

    pub(super) fn require_index_entity(
        &self,
        identifier: &EntityIdentifier,
    ) -> Result<u64, SemanticError> {
        self.resolve_entity_identifier(identifier)?.ok_or_else(|| {
            SemanticError::incorrect(
                "index/unresolved-entity",
                "raw index boundary entity does not resolve in this database value",
            )
        })
    }

    pub(super) fn resolve_ready_avet_attribute(
        &self,
        attribute: &AttributeName,
    ) -> Result<u32, SemanticError> {
        let attribute = self.resolve_attribute(attribute)?;
        let schema = self.schema().attribute(attribute)?;
        if !(schema.indexed || schema.unique.is_some()) {
            return Err(SemanticError::incorrect(
                "index/attribute-not-in-avet",
                format!(
                    "attribute {} is not present in AVET",
                    schema.ident.qualified_name()
                ),
            ));
        }
        if !self.physical_avet_ready(attribute) {
            return Err(SemanticError::new(
                ErrorCategory::Unavailable,
                "index/avet-not-ready",
                format!(
                    "AVET backfill is not ready for attribute {}",
                    schema.ident.qualified_name()
                ),
            ));
        }
        Ok(attribute)
    }

    pub(super) fn normalize_vaet_value(
        &self,
        value: RawIndexValue,
    ) -> Result<Value, SemanticError> {
        match value {
            RawIndexValue::Entity(identifier) => {
                self.require_index_entity(&identifier).map(Value::Ref)
            }
            RawIndexValue::Stored(Value::Ref(entity)) => {
                eid_to_eidx(entity)?;
                Ok(Value::Ref(entity))
            }
            RawIndexValue::Stored(_) | RawIndexValue::Tuple(_) => Err(SemanticError::incorrect(
                "index/vaet-value-not-ref",
                "VAET boundary value must be an entity identifier or stored reference",
            )),
        }
    }

    pub(super) fn normalize_index_value(
        &self,
        attribute: u32,
        value: RawIndexValue,
    ) -> Result<Value, SemanticError> {
        let schema = self.schema().attribute(attribute)?;
        let mut value = match (schema.value_type, value) {
            (ValueType::Ref, RawIndexValue::Entity(identifier)) => {
                Value::Ref(self.require_index_entity(&identifier)?)
            }
            (ValueType::Tuple, RawIndexValue::Tuple(slots)) => {
                self.normalize_index_tuple(schema, slots)?
            }
            (_, RawIndexValue::Stored(value)) => {
                Self::validate_stored_index_refs(&value)?;
                value
            }
            (_, RawIndexValue::Entity(identifier)) => {
                Value::Ref(self.require_index_entity(&identifier)?)
            }
            (_, RawIndexValue::Tuple(_)) => {
                return Err(SemanticError::incorrect(
                    "transaction/value-type",
                    format!(
                        "attribute {} requires {:?}, got tuple",
                        schema.ident.qualified_name(),
                        schema.value_type
                    ),
                ));
            }
        };
        // A virtual tuple start is allowed to omit trailing slots, unlike an
        // assertion. Pad only for the existing schema validator, so all slot
        // types, scalar limits and reference checks remain unchanged. At most
        // eight slots are admitted and no supplied payload is cloned. Remove
        // the padding before comparison: [x] must sort before [x, nil].
        let prefix_length =
            if let (ValueType::Tuple, Value::Tuple(slots)) = (schema.value_type, &mut value) {
                let length = slots.len();
                let stored_length = match schema.tuple.as_ref() {
                    Some(TupleSpec::Homogeneous(_)) if length <= 8 => length.max(2),
                    Some(TupleSpec::Heterogeneous(types)) if length <= types.len() => types.len(),
                    Some(TupleSpec::Composite(attributes)) if length <= attributes.len() => {
                        attributes.len()
                    }
                    // Leave invalid oversized values untouched: the ordinary
                    // validator supplies its established arity error below.
                    _ => length,
                };
                slots.resize_with(stored_length, || None);
                Some(length)
            } else {
                None
            };
        self.schema().validate_value(schema, &value)?;
        if let (Some(length), Value::Tuple(slots)) = (prefix_length, &mut value) {
            slots.truncate(length);
        }
        Ok(value)
    }

    pub(super) fn normalize_index_tuple(
        &self,
        attribute: &crate::Attribute,
        slots: Vec<Option<RawIndexValue>>,
    ) -> Result<Value, SemanticError> {
        let value_types = match attribute.tuple.as_ref() {
            Some(TupleSpec::Homogeneous(value_type)) => vec![*value_type; 8],
            Some(TupleSpec::Heterogeneous(value_types)) => value_types.clone(),
            Some(TupleSpec::Composite(attributes)) => attributes
                .iter()
                .map(|attribute| self.schema().attribute(*attribute).map(|a| a.value_type))
                .collect::<Result<Vec<_>, _>>()?,
            None => {
                return Err(SemanticError::incorrect(
                    "schema/missing-tuple-spec",
                    "tuple specification missing",
                ));
            }
        };
        if slots.len() > value_types.len() {
            return Err(SemanticError::incorrect(
                "transaction/invalid-tuple-length",
                format!("tuple boundary accepts at most {} slots", value_types.len()),
            ));
        }
        let normalized = value_types
            .into_iter()
            .zip(slots)
            .map(|(value_type, slot)| {
                slot.map(|slot| self.normalize_index_tuple_slot(value_type, slot))
                    .transpose()
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Value::Tuple(normalized))
    }

    pub(super) fn normalize_index_tuple_slot(
        &self,
        value_type: ValueType,
        value: RawIndexValue,
    ) -> Result<Value, SemanticError> {
        match value {
            RawIndexValue::Entity(identifier) => {
                self.require_index_entity(&identifier).map(Value::Ref)
            }
            RawIndexValue::Stored(value) => {
                Self::validate_stored_index_refs(&value)?;
                Ok(value)
            }
            RawIndexValue::Tuple(_) => Err(SemanticError::incorrect(
                "transaction/invalid-tuple-element",
                format!("tuple slot requires {value_type:?}, got tuple"),
            )),
        }
    }

    pub(super) fn validate_stored_index_refs(value: &Value) -> Result<(), SemanticError> {
        match value {
            Value::Ref(entity) => {
                eid_to_eidx(*entity)?;
            }
            Value::Tuple(slots) => {
                for value in slots.iter().flatten() {
                    Self::validate_stored_index_refs(value)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Resolve the public eid/ident/lookup-ref forms against this exact value.
    pub fn resolve_entity_identifier(
        &self,
        identifier: &EntityIdentifier,
    ) -> Result<Option<u64>, SemanticError> {
        self.resolve_entity_identifier_with_control(identifier, &mut |_| Ok(true))
    }

    pub(crate) fn resolve_entity_identifier_with_control(
        &self,
        identifier: &EntityIdentifier,
        control: &mut dyn FnMut(Option<&Datom>) -> Result<bool, SemanticError>,
    ) -> Result<Option<u64>, SemanticError> {
        match identifier {
            EntityIdentifier::Id(entity) => {
                eid_to_eidx(*entity)?;
                Ok(Some(*entity))
            }
            EntityIdentifier::Ident(ident) => Ok(self.entid(ident)),
            EntityIdentifier::Lookup { attribute, value } => {
                let attribute = self.resolve_attribute(attribute)?;
                self.lookup_with_control(attribute, value, control)
            }
        }
    }

    pub(super) fn resolve_attribute(&self, name: &AttributeName) -> Result<u32, SemanticError> {
        let attribute = match name {
            AttributeName::Id(attribute) => *attribute,
            AttributeName::Ident(ident) => {
                let entity = self.entid(ident).ok_or_else(|| {
                    SemanticError::incorrect(
                        "database/unknown-attribute",
                        format!("unknown attribute {}", ident.qualified_name()),
                    )
                })?;
                schema_eid_to_attr_id(entity).map_err(|_| {
                    SemanticError::incorrect(
                        "database/unknown-attribute",
                        format!("{} does not identify an attribute", ident.qualified_name()),
                    )
                })?
            }
        };
        self.schema().attribute(attribute).map_err(|_| {
            SemanticError::incorrect(
                "database/unknown-attribute",
                format!("unknown attribute id {attribute}"),
            )
        })?;
        Ok(attribute)
    }

    pub(super) fn validate_raw_boundary_access(
        &self,
        boundary: &IndexBoundary,
    ) -> Result<(), SemanticError> {
        boundary.validate()?;
        if let Some(attribute) = boundary.avet_attribute() {
            self.resolve_ready_avet_attribute(&AttributeName::Id(attribute))?;
        }
        Ok(())
    }
}

pub(super) fn schema_has_avet(schema: &Schema, attribute: u32) -> bool {
    schema
        .attribute(attribute)
        .is_ok_and(|attribute| attribute.indexed || attribute.unique.is_some())
}
