//! Transaction-only reference input and a bounded structural admission policy.
//! Stored Values and ordinary read identifiers are deliberately unchanged.
use crate::reserved_allocation::ReservedAllocation;
use crate::{
    CallableRef, DatabaseValue, EntityRef, ErrorCategory, MapValue, RuntimeValue, Schema,
    SemanticError, TupleSpec, TxForm, TxOp, TxValue, Value, ValueType,
};

use std::collections::BTreeSet;

/// The raw value key used while resolving unique identities. Datomic permits
/// uniqueness on refs, and `ProcessExpander/get-ids` compares unresolved ref tempids
/// before replacing them (`db.clj:6712-6797, 7366-7455`). Reference tempids
/// therefore remain symbolic here: two entity tempids asserting the same
/// ref-valued identity must unify even when the referenced entity is new.
#[derive(Clone, Debug)]
pub(crate) enum UpsertIdentityValue {
    Resolved(Value),
    TempRef(String),
    Tuple(Vec<Option<UpsertIdentityValue>>),
}

impl UpsertIdentityValue {
    pub(crate) fn same_key(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Resolved(left), Self::Resolved(right)) => left.index_cmp(right).is_eq(),
            (Self::TempRef(left), Self::TempRef(right)) => left == right,
            (Self::Tuple(left), Self::Tuple(right)) => {
                left.len() == right.len()
                    && left
                        .iter()
                        .zip(right)
                        .all(|(left, right)| match (left, right) {
                            (None, None) => true,
                            (Some(left), Some(right)) => left.same_key(right),
                            _ => false,
                        })
            }
            _ => false,
        }
    }

    pub(crate) fn resolved(&self) -> Option<&Value> {
        match self {
            Self::Resolved(value) => Some(value),
            Self::TempRef(_) | Self::Tuple(_) => None,
        }
    }

    pub(crate) fn is_nan(&self) -> bool {
        self.resolved().is_some_and(Value::is_nan)
    }
}

/// Resolve only ref-typed input slots; scalar and nil slots retain their
/// stored meaning. Symbolic references remain available to identity grouping.
pub(crate) fn resolve_tuple_input(
    schema: &Schema,
    attribute: &crate::Attribute,
    slots: &[Option<TxValue>],
    mut resolve: impl FnMut(&EntityRef) -> Result<UpsertIdentityValue, SemanticError>,
) -> Result<UpsertIdentityValue, SemanticError> {
    let types = match &attribute.tuple {
        Some(TupleSpec::Homogeneous(kind)) => vec![*kind; slots.len()],
        Some(TupleSpec::Heterogeneous(types)) => types.clone(),
        Some(TupleSpec::Composite(attributes)) => attributes
            .iter()
            .map(|attribute| {
                schema
                    .attribute(*attribute)
                    .map(|attribute| attribute.value_type)
            })
            .collect::<Result<Vec<_>, _>>()?,
        None => {
            return Err(SemanticError::incorrect(
                "transaction/value-type",
                "tuple input requires a tuple attribute",
            ));
        }
    };
    if !(2..=8).contains(&slots.len()) || types.len() != slots.len() {
        return Err(SemanticError::incorrect(
            "transaction/invalid-tuple-length",
            "tuple input does not match its schema length",
        ));
    }
    let mut resolved = Vec::with_capacity(slots.len());
    for (slot, kind) in slots.iter().zip(types) {
        resolved.push(match slot {
            None => None,
            Some(TxValue::Scalar(value)) => Some(UpsertIdentityValue::Resolved(value.clone())),
            Some(TxValue::Entity(entity)) if kind == ValueType::Ref => Some(resolve(entity)?),
            Some(_) => return Err(SemanticError::incorrect("transaction/invalid-tuple-element", "tuple reference input requires a ref-typed slot; nested tuples are not scalar slots")),
        });
    }
    // A temporary ref's final numeric id cannot change tuple shape/type.
    // Validate with a type-only placeholder, never store or allocate it.
    let shape = Value::Tuple(
        resolved
            .iter()
            .map(|slot| {
                slot.as_ref().map(|value| {
                    value
                        .resolved()
                        .cloned()
                        .unwrap_or(Value::Ref(crate::DB_IDENT))
                })
            })
            .collect(),
    );
    schema.validate_value(attribute, &shape)?;
    if resolved
        .iter()
        .flatten()
        .all(|value| value.resolved().is_some())
    {
        Ok(UpsertIdentityValue::Resolved(shape))
    } else {
        Ok(UpsertIdentityValue::Tuple(resolved))
    }
}

/// Reserve all explicit partition-zero operands before allocating any tempid.
/// A no-op retraction or a reference-only argument still acknowledges an ID;
/// it need not survive as material transaction data. This is input-sized work,
/// independent of the database's accumulated schema or ordinary allocation.
pub(crate) fn observe_reserved_transaction_inputs(
    reserved: &mut ReservedAllocation,
    ops: &[TxOp],
) -> Result<(), SemanticError> {
    enum Input<'a> {
        Entity(&'a EntityRef),
        Value(&'a TxValue),
    }
    let mut pending = Vec::new();
    for op in ops {
        match op {
            TxOp::Add {
                entity,
                attribute,
                value,
            } => {
                reserved.observe_attribute(*attribute)?;
                pending.push(Input::Entity(entity));
                pending.push(Input::Value(value));
            }
            TxOp::Retract {
                entity,
                attribute,
                value,
            } => {
                reserved.observe_attribute(*attribute)?;
                pending.push(Input::Entity(entity));
                if let Some(value) = value {
                    pending.push(Input::Value(value));
                }
            }
            TxOp::Cas {
                entity,
                attribute,
                old,
                new,
            } => {
                reserved.observe_attribute(*attribute)?;
                pending.push(Input::Entity(entity));
                pending.push(Input::Value(new));
                if let Some(value) = old {
                    pending.push(Input::Value(value));
                }
            }
            TxOp::RetractEntity(entity) | TxOp::MatchPartition { entity, .. } => {
                pending.push(Input::Entity(entity))
            }
            TxOp::ForcePartition { partition, .. } => pending.push(Input::Entity(partition)),
            TxOp::Ensure { entity, spec } => {
                pending.push(Input::Entity(entity));
                pending.push(Input::Entity(spec));
            }
            TxOp::InstallAttribute(attribute) | TxOp::AlterAttribute(attribute) => {
                reserved.observe_attribute(attribute.id)?;
                if let Some(TupleSpec::Composite(attributes)) = &attribute.tuple {
                    for attribute in attributes {
                        reserved.observe_attribute(*attribute)?;
                    }
                }
            }
        }
        while let Some(input) = pending.pop() {
            match input {
                Input::Entity(EntityRef::Id(entity)) => reserved.observe_entity(*entity)?,
                Input::Entity(EntityRef::Lookup { attribute, value }) => {
                    reserved.observe_attribute(*attribute)?;
                    reserved.observe_value(value)?;
                }
                Input::Entity(EntityRef::LookupInput { attribute, value }) => {
                    reserved.observe_attribute(*attribute)?;
                    pending.push(Input::Value(value));
                }
                Input::Entity(EntityRef::Ident(_) | EntityRef::Temp(_) | EntityRef::Tx) => {}
                Input::Value(TxValue::Scalar(value)) => reserved.observe_value(value)?,
                Input::Value(TxValue::Entity(entity)) => pending.push(Input::Entity(entity)),
                Input::Value(TxValue::Tuple(slots)) => {
                    pending.extend(slots.iter().flatten().map(Input::Value))
                }
            }
        }
    }
    Ok(())
}

/// Match recovered `ProcessExpander`: permanent ids are assigned to tempids in
/// E position. A reference tempid may be used in V only when another
/// normalized datom establishes it as an entity; otherwise `replace_tempid`
/// raises `:db.error/tempid-not-an-entity` (`db.clj:7376-7390`). Running this
/// after form normalization also covers tempids introduced by nested maps.
pub(crate) fn validated_entity_tempids(ops: &[TxOp]) -> Result<BTreeSet<String>, SemanticError> {
    let mut entities = BTreeSet::new();
    let mut values = BTreeSet::new();
    for op in ops {
        collect_tempids_op(op, &mut entities, &mut values);
    }
    if let Some(tempid) = values.difference(&entities).next() {
        return Err(SemanticError::incorrect(
            "transaction/tempid-not-an-entity",
            format!("tempid '{tempid}' is used only as a value in transaction data"),
        )
        .detail("tempid", tempid.clone()));
    }
    Ok(entities)
}

fn collect_tempids_op(op: &TxOp, entities: &mut BTreeSet<String>, values: &mut BTreeSet<String>) {
    match op {
        TxOp::Add { entity, value, .. } => {
            collect_tempids_entity(entity, entities);
            collect_tempids_value(value, values);
        }
        TxOp::Retract { entity, value, .. } => {
            collect_tempids_entity(entity, entities);
            if let Some(value) = value {
                collect_tempids_value(value, values);
            }
        }
        TxOp::Cas {
            entity, old, new, ..
        } => {
            collect_tempids_entity(entity, entities);
            if let Some(old) = old {
                collect_tempids_value(old, values);
            }
            collect_tempids_value(new, values);
        }
        TxOp::RetractEntity(entity) => collect_tempids_entity(entity, entities),
        TxOp::Ensure { entity, spec } => {
            collect_tempids_entity(entity, entities);
            collect_tempids_entity(spec, values);
        }
        TxOp::InstallAttribute(_)
        | TxOp::AlterAttribute(_)
        | TxOp::ForcePartition { .. }
        | TxOp::MatchPartition { .. } => {}
    }
}

fn collect_tempids_value(value: &TxValue, output: &mut BTreeSet<String>) {
    match value {
        TxValue::Entity(entity) => collect_tempids_entity(entity, output),
        TxValue::Tuple(slots) => {
            for value in slots.iter().flatten() {
                if let TxValue::Entity(entity) = value {
                    collect_tempids_entity(entity, output);
                }
            }
        }
        TxValue::Scalar(_) => {}
    }
}

fn collect_tempids_entity(entity: &EntityRef, output: &mut BTreeSet<String>) {
    if let EntityRef::Temp(tempid) = entity {
        output.insert(tempid.clone());
    }
}

// Like existing map/runtime depth policies, this bounds untrusted request
// structure, not the number of graph edges a query can traverse.
const MAX_REFERENCE_INPUT_DEPTH: usize = 32;
type LookupFn<'a> = dyn FnMut(u32, &Value) -> Result<Option<u64>, SemanticError> + 'a;

/// Match the existing stored-value decoding policy before a native caller's
/// owned values can enter recursive cloning, comparison, or encoding.
pub(crate) fn validate_stored_input(value: &Value) -> Result<(), SemanticError> {
    if !matches!(value, Value::Tuple(_) | Value::Uri(_)) {
        return Ok(());
    }
    let mut pending = vec![(value, 0usize)];
    while let Some((value, depth)) = pending.pop() {
        if depth > 16 {
            return Err(SemanticError::incorrect(
                "encoding/value-depth",
                "stored values exceed the 16-level admission policy",
            ));
        }
        match value {
            Value::Tuple(slots) => {
                pending.extend(slots.iter().flatten().map(|value| (value, depth + 1)));
            }
            Value::Uri(uri) if !crate::model::uri::validate(uri) => {
                return Err(SemanticError::incorrect(
                    "value/invalid-uri",
                    "URI value has invalid syntax or escaping",
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

enum Input<'a> {
    Entity(&'a EntityRef, usize),
    Value(&'a TxValue, usize, bool),
}

fn validate(mut pending: Vec<Input<'_>>) -> Result<(), SemanticError> {
    while let Some(input) = pending.pop() {
        let depth = match input {
            Input::Entity(_, depth) | Input::Value(_, depth, _) => depth,
        };
        if depth > MAX_REFERENCE_INPUT_DEPTH {
            return Err(SemanticError::new(
                ErrorCategory::Busy,
                "transaction/input-depth",
                "transaction reference input exceeds the 32-level admission policy",
            ));
        }
        match input {
            Input::Entity(EntityRef::Lookup { value, .. }, _)
            | Input::Value(TxValue::Scalar(value), _, _) => validate_stored_input(value)?,
            Input::Entity(EntityRef::LookupInput { value, .. }, depth) => {
                pending.push(Input::Value(value, depth + 1, false))
            }
            Input::Value(TxValue::Entity(entity), depth, _) => {
                pending.push(Input::Entity(entity, depth + 1))
            }
            Input::Value(TxValue::Tuple(slots), depth, nested) => {
                if nested || !(2..=8).contains(&slots.len()) {
                    return Err(SemanticError::incorrect(
                        "transaction/invalid-input-tuple",
                        "transaction tuples require 2–8 non-tuple slots",
                    ));
                }
                for slot in slots.iter().flatten() {
                    pending.push(Input::Value(slot, depth + 1, true));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

pub(crate) fn validate_entity_input(entity: &EntityRef) -> Result<(), SemanticError> {
    validate(vec![Input::Entity(entity, 0)])
}
pub(crate) fn validate_value_input(value: &TxValue) -> Result<(), SemanticError> {
    validate(vec![Input::Value(value, 0, false)])
}
pub(crate) fn validate_ops_input(ops: &[TxOp]) -> Result<(), SemanticError> {
    for op in ops {
        match op {
            TxOp::Add { entity, value, .. } => {
                validate_entity_input(entity)?;
                validate_value_input(value)?;
            }
            TxOp::Retract { entity, value, .. } => {
                validate_entity_input(entity)?;
                if let Some(value) = value {
                    validate_value_input(value)?;
                }
            }
            TxOp::Cas {
                entity, old, new, ..
            } => {
                validate_entity_input(entity)?;
                if let Some(old) = old {
                    validate_value_input(old)?;
                }
                validate_value_input(new)?;
            }
            TxOp::RetractEntity(entity) => validate_entity_input(entity)?,
            TxOp::Ensure { entity, spec } => {
                validate_entity_input(entity)?;
                validate_entity_input(spec)?;
            }
            TxOp::ForcePartition { partition, .. } => validate_entity_input(partition)?,
            TxOp::MatchPartition { entity, .. } => validate_entity_input(entity)?,
            TxOp::InstallAttribute(_) | TxOp::AlterAttribute(_) => {}
        }
    }
    Ok(())
}

pub(crate) fn validate_forms_input(forms: &[TxForm]) -> Result<(), SemanticError> {
    fn runtime(value: &RuntimeValue, depth: usize) -> Result<(), SemanticError> {
        if depth > 16 {
            return Err(SemanticError::incorrect(
                "program/value-depth",
                "runtime collection nesting exceeds the 16-level policy",
            ));
        }
        match value {
            RuntimeValue::Scalar(value) => validate_stored_input(value)?,
            RuntimeValue::Entity(entity) => validate_entity_input(entity)?,
            RuntimeValue::Query(_) => {
                return Err(SemanticError::incorrect(
                    "program/query-only-value",
                    "general query data is not a transaction argument",
                ));
            }
            RuntimeValue::Vector(values) => {
                for value in values {
                    runtime(value, depth + 1)?;
                }
            }
            RuntimeValue::Map(entries) => {
                for (key, value) in entries {
                    validate_stored_input(key)?;
                    runtime(value, depth + 1)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn map(map: &crate::EntityMap, depth: usize) -> Result<(), SemanticError> {
        if let Some(id) = &map.id {
            validate_entity_input(id)?;
        }
        for (_, value) in &map.attributes {
            map_value(value, depth + 1)?;
        }
        Ok(())
    }
    fn map_value(value: &MapValue, depth: usize) -> Result<(), SemanticError> {
        if depth > 32 {
            return Err(SemanticError::incorrect(
                "transaction/map-depth",
                "transaction maps exceed the 32-level policy",
            ));
        }
        match value {
            MapValue::Value(value) => validate_value_input(value)?,
            MapValue::Nested(value) => map(value, depth)?,
            MapValue::Many(values) => {
                for value in values {
                    map_value(value, depth + 1)?;
                }
            }
        }
        Ok(())
    }
    for form in forms {
        match form {
            TxForm::Edn(_) => {} // Private construction validates EDN shape/depth/bytes.
            TxForm::Op(op) => validate_ops_input(std::slice::from_ref(op))?,
            TxForm::EntityMap(value) => map(value, 0)?,
            TxForm::Call(call) => {
                for value in &call.arguments {
                    validate_value_input(value)?;
                }
            }
            TxForm::ProgramCall(call) => {
                if let CallableRef::Database(entity) = &call.function {
                    validate_entity_input(entity)?;
                }
                for value in &call.arguments {
                    runtime(value, 0)?;
                }
            }
        }
    }
    Ok(())
}

impl DatabaseValue {
    /// Normalize a transaction-only reference-shaped lookup key against this
    /// exact db-before. Read context/cursor charging is retained throughout.
    pub(crate) fn resolve_lookup_input(
        &self,
        attribute: u32,
        input: &TxValue,
    ) -> Result<Option<u64>, SemanticError> {
        self.resolve_lookup_input_with(attribute, input, &mut |attribute, value| {
            self.lookup(attribute, value)
        })
    }

    pub(crate) fn resolve_lookup_input_with(
        &self,
        attribute: u32,
        input: &TxValue,
        lookup: &mut LookupFn<'_>,
    ) -> Result<Option<u64>, SemanticError> {
        validate_value_input(input)?;
        let descriptor = self.schema().attribute(attribute)?;
        if descriptor.unique.is_none() {
            return Err(SemanticError::incorrect(
                "transaction/lookup-non-unique",
                "lookup ref requires a unique attribute",
            ));
        }
        let value = self.lookup_input_value(attribute, input, lookup)?;
        lookup(attribute, &value)
    }

    fn lookup_input_value(
        &self,
        attribute: u32,
        input: &TxValue,
        lookup: &mut LookupFn<'_>,
    ) -> Result<Value, SemanticError> {
        let descriptor = self.schema().attribute(attribute)?;
        let value = match input {
            TxValue::Scalar(value) => {
                self.validate_lookup_stored_refs(value)?;
                value.clone()
            }
            TxValue::Entity(entity) if descriptor.value_type == ValueType::Ref => {
                Value::Ref(self.lookup_input_entity(entity, lookup)?)
            }
            TxValue::Tuple(slots) if descriptor.value_type == ValueType::Tuple => {
                for slot in slots.iter().flatten() {
                    if let TxValue::Scalar(value) = slot {
                        self.validate_lookup_stored_refs(value)?;
                    }
                }
                resolve_tuple_input(self.schema(), descriptor, slots, |entity| {
                    self.lookup_input_entity(entity, lookup)
                        .map(|entity| UpsertIdentityValue::Resolved(Value::Ref(entity)))
                })?
                .resolved()
                .expect("lookup input cannot retain tempids")
                .clone()
            }
            _ => {
                return Err(SemanticError::incorrect(
                    "transaction/value-type",
                    "lookup key does not match its attribute type",
                ));
            }
        };
        self.schema().validate_value(descriptor, &value)?;
        Ok(value)
    }

    fn lookup_input_entity(
        &self,
        entity: &EntityRef,
        lookup: &mut LookupFn<'_>,
    ) -> Result<u64, SemanticError> {
        let resolved = match entity {
            EntityRef::Id(entity) => {
                self.validate_lookup_stored_refs(&Value::Ref(*entity))?;
                Some(*entity)
            }
            EntityRef::Ident(ident) => self.entid(ident),
            EntityRef::Lookup { attribute, value } => lookup(*attribute, value)?,
            EntityRef::LookupInput { attribute, value } => {
                self.resolve_lookup_input_with(*attribute, value, lookup)?
            }
            EntityRef::Temp(_) | EntityRef::Tx => {
                return Err(SemanticError::incorrect(
                    "transaction/lookup-local-entity",
                    "lookup keys resolve only db-before entities, not tempids or the current transaction",
                ));
            }
        };
        resolved.ok_or_else(|| {
            SemanticError::incorrect(
                "transaction/lookup-not-found",
                "lookup key reference did not resolve in db-before",
            )
        })
    }

    fn validate_lookup_stored_refs(&self, value: &Value) -> Result<(), SemanticError> {
        match value {
            Value::Ref(entity) => {
                crate::model::identity::validate_supported_eid(*entity)?;
                self.schema()
                    .validate_partition_bits(crate::eid_to_part(*entity)?)?;
                if crate::eid_to_eidx(*entity)? >= self.eidx_frontier() {
                    return Err(SemanticError::incorrect(
                        "transaction/invalid-entity-id",
                        "lookup key contains an unissued entity id",
                    ));
                }
            }
            Value::Tuple(slots) => {
                for value in slots.iter().flatten() {
                    self.validate_lookup_stored_refs(value)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}
