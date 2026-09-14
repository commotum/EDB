//! Exact structural request ordering, distinct from logical stored-value equality.
use super::forms::*;
use crate::{
    Attribute, CallableRef, Cardinality, Keyword, ProgramCall, TupleSpec, Unique, Value, ValueType,
};
use std::cmp::Ordering;

/// Total structural order for primitive transaction forms.
///
/// Datomic's recovered implementation compares typed transaction values; it
/// does not make formatted diagnostic text part of transaction semantics. The
/// kernel additionally canonicalizes its declarative input so tempid and
/// anonymous-map allocation cannot depend on caller collection order. This
/// comparator is that native boundary: logical value order comes first and
/// representation details break ties (notably BigDecimal scale and numeric
/// representation).
pub(crate) fn compare_tx_op(left: &TxOp, right: &TxOp) -> Ordering {
    tx_op_rank(left)
        .cmp(&tx_op_rank(right))
        .then_with(|| match (left, right) {
            (
                TxOp::Add {
                    entity: left_entity,
                    attribute: left_attribute,
                    value: left_value,
                },
                TxOp::Add {
                    entity: right_entity,
                    attribute: right_attribute,
                    value: right_value,
                },
            ) => compare_entity_ref(left_entity, right_entity)
                .then_with(|| left_attribute.cmp(right_attribute))
                .then_with(|| compare_tx_value(left_value, right_value)),
            (
                TxOp::Retract {
                    entity: left_entity,
                    attribute: left_attribute,
                    value: left_value,
                },
                TxOp::Retract {
                    entity: right_entity,
                    attribute: right_attribute,
                    value: right_value,
                },
            ) => compare_entity_ref(left_entity, right_entity)
                .then_with(|| left_attribute.cmp(right_attribute))
                .then_with(|| compare_option_by(left_value, right_value, compare_tx_value)),
            (
                TxOp::Cas {
                    entity: left_entity,
                    attribute: left_attribute,
                    old: left_old,
                    new: left_new,
                },
                TxOp::Cas {
                    entity: right_entity,
                    attribute: right_attribute,
                    old: right_old,
                    new: right_new,
                },
            ) => compare_entity_ref(left_entity, right_entity)
                .then_with(|| left_attribute.cmp(right_attribute))
                .then_with(|| compare_option_by(left_old, right_old, compare_tx_value))
                .then_with(|| compare_tx_value(left_new, right_new)),
            (TxOp::RetractEntity(left), TxOp::RetractEntity(right)) => {
                compare_entity_ref(left, right)
            }
            (
                TxOp::Ensure {
                    entity: left_entity,
                    spec: left_spec,
                },
                TxOp::Ensure {
                    entity: right_entity,
                    spec: right_spec,
                },
            ) => compare_entity_ref(left_entity, right_entity)
                .then_with(|| compare_entity_ref(left_spec, right_spec)),
            (TxOp::InstallAttribute(left), TxOp::InstallAttribute(right))
            | (TxOp::AlterAttribute(left), TxOp::AlterAttribute(right)) => {
                compare_attribute(left, right)
            }
            (
                TxOp::ForcePartition {
                    tempid: left,
                    partition: left_ref,
                },
                TxOp::ForcePartition {
                    tempid: right,
                    partition: right_ref,
                },
            )
            | (
                TxOp::MatchPartition {
                    tempid: left,
                    entity: left_ref,
                },
                TxOp::MatchPartition {
                    tempid: right,
                    entity: right_ref,
                },
            ) => compare_text(left, right).then_with(|| compare_entity_ref(left_ref, right_ref)),
            _ => Ordering::Equal,
        })
}

fn tx_op_rank(op: &TxOp) -> u8 {
    match op {
        TxOp::Add { .. } => 0,
        TxOp::Retract { .. } => 1,
        TxOp::Cas { .. } => 2,
        TxOp::RetractEntity(_) => 3,
        TxOp::Ensure { .. } => 4,
        TxOp::InstallAttribute(_) => 5,
        TxOp::AlterAttribute(_) => 6,
        TxOp::ForcePartition { .. } => 7,
        TxOp::MatchPartition { .. } => 8,
    }
}

pub(super) fn compare_tx_form(left: &TxForm, right: &TxForm) -> Ordering {
    tx_form_rank(left)
        .cmp(&tx_form_rank(right))
        .then_with(|| match (left, right) {
            (TxForm::Op(left), TxForm::Op(right)) => compare_tx_op(left, right),
            (TxForm::EntityMap(left), TxForm::EntityMap(right)) => compare_entity_map(left, right),
            (TxForm::ProgramCall(left), TxForm::ProgramCall(right)) => {
                compare_program_call(left, right)
            }
            (TxForm::Call(left), TxForm::Call(right)) => {
                compare_text(&left.function, &right.function).then_with(|| {
                    compare_slice_by(&left.arguments, &right.arguments, compare_tx_value)
                })
            }
            (TxForm::Edn(left), TxForm::Edn(right)) => {
                left.canonical_edn().cmp(right.canonical_edn())
            }
            _ => Ordering::Equal,
        })
}

fn tx_form_rank(form: &TxForm) -> u8 {
    match form {
        TxForm::Op(_) => 0,
        TxForm::EntityMap(_) => 1,
        TxForm::ProgramCall(_) => 2,
        TxForm::Call(_) => 3,
        TxForm::Edn(_) => 4,
    }
}

fn compare_program_call(left: &ProgramCall, right: &ProgramCall) -> Ordering {
    compare_callable_ref(&left.function, &right.function).then_with(|| {
        compare_slice_by(&left.arguments, &right.arguments, |left, right| {
            left.canonical_cmp(right)
        })
    })
}

fn compare_callable_ref(left: &CallableRef, right: &CallableRef) -> Ordering {
    callable_ref_rank(left)
        .cmp(&callable_ref_rank(right))
        .then_with(|| match (left, right) {
            (CallableRef::Database(left), CallableRef::Database(right)) => {
                compare_entity_ref(left, right)
            }
            (CallableRef::ExactHash(left), CallableRef::ExactHash(right)) => left.cmp(right),
            (CallableRef::Local(left), CallableRef::Local(right)) => left.cmp(right),
            _ => Ordering::Equal,
        })
}

fn callable_ref_rank(callable: &CallableRef) -> u8 {
    match callable {
        CallableRef::Database(_) => 0,
        CallableRef::ExactHash(_) => 1,
        CallableRef::Local(_) => 2,
    }
}

pub(super) fn compare_entity_map(left: &EntityMap, right: &EntityMap) -> Ordering {
    compare_option_by(&left.id, &right.id, compare_entity_ref)
        .then_with(|| compare_map_entries(&left.attributes, &right.attributes))
}

fn compare_map_entries(
    left: &[(AttributeRef, MapValue)],
    right: &[(AttributeRef, MapValue)],
) -> Ordering {
    let mut left: Vec<_> = left.iter().collect();
    let mut right: Vec<_> = right.iter().collect();
    left.sort_by(|left, right| compare_map_entry(left, right));
    right.sort_by(|left, right| compare_map_entry(left, right));
    compare_slice_by(&left, &right, |left, right| compare_map_entry(left, right))
}

pub(super) fn compare_map_entry(
    left: &(AttributeRef, MapValue),
    right: &(AttributeRef, MapValue),
) -> Ordering {
    compare_attribute_ref(&left.0, &right.0).then_with(|| compare_map_value(&left.1, &right.1))
}

fn compare_attribute_ref(left: &AttributeRef, right: &AttributeRef) -> Ordering {
    attribute_ref_rank(left)
        .cmp(&attribute_ref_rank(right))
        .then_with(|| match (left, right) {
            (AttributeRef::Id(left), AttributeRef::Id(right))
            | (AttributeRef::ReverseId(left), AttributeRef::ReverseId(right)) => left.cmp(right),
            (AttributeRef::Ident(left), AttributeRef::Ident(right))
            | (AttributeRef::ReverseIdent(left), AttributeRef::ReverseIdent(right)) => {
                compare_keyword(left, right)
            }
            _ => Ordering::Equal,
        })
}

fn attribute_ref_rank(attribute: &AttributeRef) -> u8 {
    match attribute {
        AttributeRef::Id(_) => 0,
        AttributeRef::Ident(_) => 1,
        AttributeRef::ReverseId(_) => 2,
        AttributeRef::ReverseIdent(_) => 3,
    }
}

pub(super) fn compare_map_value(left: &MapValue, right: &MapValue) -> Ordering {
    map_value_rank(left)
        .cmp(&map_value_rank(right))
        .then_with(|| match (left, right) {
            (MapValue::Value(left), MapValue::Value(right)) => compare_tx_value(left, right),
            (MapValue::Nested(left), MapValue::Nested(right)) => compare_entity_map(left, right),
            (MapValue::Many(left), MapValue::Many(right)) => {
                compare_unordered_map_values(left, right)
            }
            _ => Ordering::Equal,
        })
}

fn map_value_rank(value: &MapValue) -> u8 {
    match value {
        MapValue::Value(_) => 0,
        MapValue::Nested(_) => 1,
        MapValue::Many(_) => 2,
    }
}

fn compare_unordered_map_values(left: &[MapValue], right: &[MapValue]) -> Ordering {
    let mut left: Vec<_> = left.iter().collect();
    let mut right: Vec<_> = right.iter().collect();
    left.sort_by(|left, right| compare_map_value(left, right));
    right.sort_by(|left, right| compare_map_value(left, right));
    compare_slice_by(&left, &right, |left, right| compare_map_value(left, right))
}

fn compare_entity_ref(left: &EntityRef, right: &EntityRef) -> Ordering {
    entity_ref_rank(left)
        .cmp(&entity_ref_rank(right))
        .then_with(|| match (left, right) {
            (EntityRef::Id(left), EntityRef::Id(right)) => left.cmp(right),
            (EntityRef::Ident(left), EntityRef::Ident(right)) => compare_keyword(left, right),
            (EntityRef::Temp(left), EntityRef::Temp(right)) => compare_text(left, right),
            (
                EntityRef::Lookup {
                    attribute: left_attribute,
                    value: left_value,
                },
                EntityRef::Lookup {
                    attribute: right_attribute,
                    value: right_value,
                },
            ) => left_attribute
                .cmp(right_attribute)
                .then_with(|| compare_value(left_value, right_value)),
            (EntityRef::Tx, EntityRef::Tx) => Ordering::Equal,
            (
                EntityRef::LookupInput {
                    attribute: left_attribute,
                    value: left,
                },
                EntityRef::LookupInput {
                    attribute: right_attribute,
                    value: right,
                },
            ) => left_attribute
                .cmp(right_attribute)
                .then_with(|| compare_tx_value(left, right)),
            _ => Ordering::Equal,
        })
}

fn entity_ref_rank(entity: &EntityRef) -> u8 {
    match entity {
        EntityRef::Id(_) => 0,
        EntityRef::Ident(_) => 1,
        EntityRef::Temp(_) => 2,
        EntityRef::Lookup { .. } => 3,
        EntityRef::Tx => 4,
        EntityRef::LookupInput { .. } => 5,
    }
}

fn compare_tx_value(left: &TxValue, right: &TxValue) -> Ordering {
    tx_value_rank(left)
        .cmp(&tx_value_rank(right))
        .then_with(|| match (left, right) {
            (TxValue::Scalar(left), TxValue::Scalar(right)) => compare_value(left, right),
            (TxValue::Entity(left), TxValue::Entity(right)) => compare_entity_ref(left, right),
            (TxValue::Tuple(left), TxValue::Tuple(right)) => {
                compare_slice_by(left, right, |left, right| {
                    compare_option_by(left, right, compare_tx_value)
                })
            }
            _ => Ordering::Equal,
        })
}

fn tx_value_rank(value: &TxValue) -> u8 {
    match value {
        TxValue::Scalar(_) => 0,
        TxValue::Entity(_) => 1,
        TxValue::Tuple(_) => 2,
    }
}

pub(super) fn compare_value(left: &Value, right: &Value) -> Ordering {
    left.stored_cmp(right)
        .then_with(|| value_rank(left).cmp(&value_rank(right)))
        .then_with(|| match (left, right) {
            (Value::BigDec(left), Value::BigDec(right)) => left
                .as_bigint_and_exponent()
                .cmp(&right.as_bigint_and_exponent()),
            (Value::BigInt(left), Value::BigInt(right)) => left.cmp(right),
            (Value::Bool(left), Value::Bool(right)) => left.cmp(right),
            (Value::Bytes(left), Value::Bytes(right)) => left.cmp(right),
            (Value::Double(left), Value::Double(right)) => left.total_cmp(right),
            (Value::Float(left), Value::Float(right)) => left.total_cmp(right),
            (Value::Function(left), Value::Function(right)) => left.cmp(right),
            (Value::Instant(left), Value::Instant(right)) => left.cmp(right),
            (Value::Keyword(left), Value::Keyword(right)) => compare_keyword(left, right),
            (Value::Long(left), Value::Long(right)) => left.cmp(right),
            (Value::Ref(left), Value::Ref(right)) => left.cmp(right),
            (Value::String(left), Value::String(right)) => compare_text(left, right),
            (Value::Symbol(left), Value::Symbol(right)) => compare_names(
                left.namespace.as_deref(),
                &left.name,
                right.namespace.as_deref(),
                &right.name,
            ),
            (Value::Tuple(left), Value::Tuple(right)) => {
                compare_slice_by(left, right, |left, right| {
                    compare_option_by(left, right, compare_value)
                })
            }
            (Value::Uuid(left), Value::Uuid(right)) => left.cmp(right),
            (Value::Uri(left), Value::Uri(right)) => compare_text(left, right),
            _ => Ordering::Equal,
        })
}

fn value_rank(value: &Value) -> u8 {
    match value {
        Value::BigDec(_) => 0,
        Value::BigInt(_) => 1,
        Value::Bool(_) => 2,
        Value::Bytes(_) => 3,
        Value::Double(_) => 4,
        Value::Float(_) => 5,
        Value::Instant(_) => 6,
        Value::Keyword(_) => 7,
        Value::Long(_) => 8,
        Value::Ref(_) => 9,
        Value::String(_) => 10,
        Value::Symbol(_) => 11,
        Value::Tuple(_) => 12,
        Value::Uuid(_) => 13,
        Value::Uri(_) => 14,
        Value::Function(_) => 15,
    }
}

fn compare_attribute(left: &Attribute, right: &Attribute) -> Ordering {
    left.id
        .cmp(&right.id)
        .then_with(|| compare_keyword(&left.ident, &right.ident))
        .then_with(|| value_type_rank(left.value_type).cmp(&value_type_rank(right.value_type)))
        .then_with(|| cardinality_rank(left.cardinality).cmp(&cardinality_rank(right.cardinality)))
        .then_with(|| {
            compare_option_by(&left.unique, &right.unique, |left, right| {
                unique_rank(*left).cmp(&unique_rank(*right))
            })
        })
        .then_with(|| left.indexed.cmp(&right.indexed))
        .then_with(|| left.component.cmp(&right.component))
        .then_with(|| left.no_history.cmp(&right.no_history))
        .then_with(|| left.fulltext.cmp(&right.fulltext))
        .then_with(|| compare_option_by(&left.tuple, &right.tuple, compare_tuple_spec))
        .then_with(|| left.tuple_discontinued.cmp(&right.tuple_discontinued))
        .then_with(|| compare_unordered_text(&left.predicates, &right.predicates))
}

fn compare_tuple_spec(left: &TupleSpec, right: &TupleSpec) -> Ordering {
    tuple_spec_rank(left)
        .cmp(&tuple_spec_rank(right))
        .then_with(|| match (left, right) {
            (TupleSpec::Homogeneous(left), TupleSpec::Homogeneous(right)) => {
                value_type_rank(*left).cmp(&value_type_rank(*right))
            }
            (TupleSpec::Heterogeneous(left), TupleSpec::Heterogeneous(right)) => {
                compare_slice_by(left, right, |left, right| {
                    value_type_rank(*left).cmp(&value_type_rank(*right))
                })
            }
            (TupleSpec::Composite(left), TupleSpec::Composite(right)) => left.cmp(right),
            _ => Ordering::Equal,
        })
}

fn tuple_spec_rank(spec: &TupleSpec) -> u8 {
    match spec {
        TupleSpec::Homogeneous(_) => 0,
        TupleSpec::Heterogeneous(_) => 1,
        TupleSpec::Composite(_) => 2,
    }
}

fn value_type_rank(value_type: ValueType) -> u8 {
    match value_type {
        ValueType::BigDec => 0,
        ValueType::BigInt => 1,
        ValueType::Boolean => 2,
        ValueType::Bytes => 3,
        ValueType::Double => 4,
        ValueType::Float => 5,
        ValueType::Instant => 6,
        ValueType::Keyword => 7,
        ValueType::Long => 8,
        ValueType::Ref => 9,
        ValueType::String => 10,
        ValueType::Symbol => 11,
        ValueType::Tuple => 12,
        ValueType::Uuid => 13,
        ValueType::Uri => 14,
        ValueType::Function => 15,
    }
}

fn cardinality_rank(cardinality: Cardinality) -> u8 {
    match cardinality {
        Cardinality::One => 0,
        Cardinality::Many => 1,
    }
}

fn unique_rank(unique: Unique) -> u8 {
    match unique {
        Unique::Identity => 0,
        Unique::Value => 1,
    }
}

fn compare_keyword(left: &Keyword, right: &Keyword) -> Ordering {
    compare_names(
        left.namespace.as_deref(),
        &left.name,
        right.namespace.as_deref(),
        &right.name,
    )
}

fn compare_names(
    left_namespace: Option<&str>,
    left_name: &str,
    right_namespace: Option<&str>,
    right_name: &str,
) -> Ordering {
    compare_option_by(&left_namespace, &right_namespace, |left, right| {
        compare_text(left, right)
    })
    .then_with(|| compare_text(left_name, right_name))
}

fn compare_text(left: &str, right: &str) -> Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

fn compare_unordered_text(left: &[String], right: &[String]) -> Ordering {
    let mut left: Vec<_> = left.iter().map(String::as_str).collect();
    let mut right: Vec<_> = right.iter().map(String::as_str).collect();
    left.sort_by(|left, right| compare_text(left, right));
    right.sort_by(|left, right| compare_text(left, right));
    compare_slice_by(&left, &right, |left, right| compare_text(left, right))
}

fn compare_option_by<T, F>(left: &Option<T>, right: &Option<T>, compare: F) -> Ordering
where
    F: FnOnce(&T, &T) -> Ordering,
{
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(left), Some(right)) => compare(left, right),
    }
}

fn compare_slice_by<T, F>(left: &[T], right: &[T], mut compare: F) -> Ordering
where
    F: FnMut(&T, &T) -> Ordering,
{
    for (left, right) in left.iter().zip(right) {
        let ordering = compare(left, right);
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    left.len().cmp(&right.len())
}
