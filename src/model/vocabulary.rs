//! Native system vocabulary and canonical genesis information.
//!
//! The numeric IDs mirror Datomic Pro 1.0.7705 where they are fixed or can be
//! recovered deterministically from the bootstrap transactions.  The native
//! kernel replaces JVM function objects with immutable native program hashes
//! while schema, including native fulltext policy, is represented by ordinary datoms, not by this
//! module's Rust descriptors.  The descriptors are only a compact way to
//! construct and verify those canonical datoms.

use crate::SemanticError;
use crate::model::datom::{Datom, IndexOrder};
use crate::model::identity::{DB_PARTITION, eid_to_eidx, eid_to_part, t_to_tx};
use crate::model::schema::{Attribute, Cardinality, TupleSpec, Unique, ValueType};
use crate::model::value::{Keyword, Value};

pub const MAX_SCHEMA_ATTRIBUTE_ID: u32 = 1_048_576;

pub const DB_PART_DB: u64 = 0;
pub const DB_ADD: u64 = 1;
pub const DB_RETRACT: u64 = 2;
pub const DB_PART_TX: u64 = 3;
pub const DB_PART_USER: u64 = 4;

pub const DB_IDENT: u64 = 10;
pub const DB_INSTALL_PARTITION: u64 = 11;
pub const DB_INSTALL_ATTRIBUTE: u64 = 13;
/// Excision request target. Recovered fixed id from Datomic Pro 1.0.7705
/// `datomic.db/BOOT-IDS` and `bootstrap-data-upgrades`.
pub const DB_EXCISE: u64 = 15;
pub const DB_EXCISE_ATTRS: u64 = 16;
pub const DB_EXCISE_BEFORE_T: u64 = 17;
pub const DB_EXCISE_BEFORE: u64 = 18;
pub const DB_ALTER_ATTRIBUTE: u64 = 19;

pub const DB_TYPE_REF: u64 = 20;
pub const DB_TYPE_KEYWORD: u64 = 21;
pub const DB_TYPE_LONG: u64 = 22;
pub const DB_TYPE_STRING: u64 = 23;
pub const DB_TYPE_BOOLEAN: u64 = 24;
pub const DB_TYPE_INSTANT: u64 = 25;
pub const DB_TYPE_FN: u64 = 26;
pub const DB_TYPE_BYTES: u64 = 27;

pub const DB_CARDINALITY_ONE: u64 = 35;
pub const DB_CARDINALITY_MANY: u64 = 36;
pub const DB_UNIQUE_VALUE: u64 = 37;
pub const DB_UNIQUE_IDENTITY: u64 = 38;

pub const DB_VALUE_TYPE: u64 = 40;
pub const DB_CARDINALITY: u64 = 41;
pub const DB_UNIQUE: u64 = 42;
pub const DB_IS_COMPONENT: u64 = 43;
pub const DB_INDEX: u64 = 44;
pub const DB_NO_HISTORY: u64 = 45;
pub const DB_TX_INSTANT: u64 = 50;
/// Boolean immutable schema property enabling analyzed fulltext indexing.
pub const DB_FULLTEXT: u64 = 51;
pub const DB_FN: u64 = 52;

pub const DB_FN_RETRACT_ENTITY: u64 = 54;
pub const DB_FN_CAS: u64 = 55;
pub const DB_TYPE_UUID: u64 = 56;
pub const DB_TYPE_DOUBLE: u64 = 57;
pub const DB_TYPE_FLOAT: u64 = 58;
pub const DB_TYPE_URI: u64 = 59;
pub const DB_TYPE_BIGINT: u64 = 60;
pub const DB_TYPE_BIGDEC: u64 = 61;
pub const DB_DOC: u64 = 62;
pub const DB_TYPE_TUPLE: u64 = 63;
pub const DB_TYPE_SYMBOL: u64 = 64;
pub const DB_TUPLE_TYPE: u64 = 65;
pub const DB_TUPLE_TYPES: u64 = 66;
pub const DB_TUPLE_ATTRS: u64 = 67;
pub const DB_ENSURE: u64 = 68;
pub const DB_ENTITY_ATTRS: u64 = 69;
pub const DB_ENTITY_PREDS: u64 = 70;
pub const DB_ATTR_PREDS: u64 = 71;
pub const DB_TUPLE_DISCONTINUED: u64 = 72;

/// Convert a schema entity ID into the compact attribute ID used by datoms.
///
/// Installed attributes are restricted to partition 0 and to the recovered
/// 2^20 schema element-vector bound. The upper bound is inclusive.
pub fn schema_eid_to_attr_id(eid: u64) -> Result<u32, SemanticError> {
    let partition = eid_to_part(eid)?;
    if partition != DB_PARTITION {
        return Err(SemanticError::incorrect(
            "schema/attribute-outside-db-partition",
            format!(
                "schema entity {eid} belongs to partition {partition}, not partition {DB_PARTITION}"
            ),
        ));
    }

    let eidx = eid_to_eidx(eid)?;
    if eidx > u64::from(MAX_SCHEMA_ATTRIBUTE_ID) {
        return Err(SemanticError::incorrect(
            "schema/attribute-id-out-of-range",
            format!("schema attribute id {eidx} exceeds the maximum {MAX_SCHEMA_ATTRIBUTE_ID}"),
        ));
    }

    u32::try_from(eidx).map_err(|_| {
        SemanticError::incorrect(
            "schema/attribute-id-out-of-range",
            format!("schema attribute id {eidx} cannot be represented as u32"),
        )
    })
}

pub const fn value_type_entity(value_type: ValueType) -> u64 {
    match value_type {
        ValueType::Ref => DB_TYPE_REF,
        ValueType::Keyword => DB_TYPE_KEYWORD,
        ValueType::Long => DB_TYPE_LONG,
        ValueType::String => DB_TYPE_STRING,
        ValueType::Boolean => DB_TYPE_BOOLEAN,
        ValueType::Instant => DB_TYPE_INSTANT,
        ValueType::Function => DB_TYPE_FN,
        ValueType::Bytes => DB_TYPE_BYTES,
        ValueType::Uuid => DB_TYPE_UUID,
        ValueType::Double => DB_TYPE_DOUBLE,
        ValueType::Float => DB_TYPE_FLOAT,
        ValueType::Uri => DB_TYPE_URI,
        ValueType::BigInt => DB_TYPE_BIGINT,
        ValueType::BigDec => DB_TYPE_BIGDEC,
        ValueType::Tuple => DB_TYPE_TUPLE,
        ValueType::Symbol => DB_TYPE_SYMBOL,
    }
}

pub const fn value_type_for_entity(entity: u64) -> Option<ValueType> {
    match entity {
        DB_TYPE_REF => Some(ValueType::Ref),
        DB_TYPE_KEYWORD => Some(ValueType::Keyword),
        DB_TYPE_LONG => Some(ValueType::Long),
        DB_TYPE_STRING => Some(ValueType::String),
        DB_TYPE_BOOLEAN => Some(ValueType::Boolean),
        DB_TYPE_INSTANT => Some(ValueType::Instant),
        DB_TYPE_FN => Some(ValueType::Function),
        DB_TYPE_BYTES => Some(ValueType::Bytes),
        DB_TYPE_UUID => Some(ValueType::Uuid),
        DB_TYPE_DOUBLE => Some(ValueType::Double),
        DB_TYPE_FLOAT => Some(ValueType::Float),
        DB_TYPE_URI => Some(ValueType::Uri),
        DB_TYPE_BIGINT => Some(ValueType::BigInt),
        DB_TYPE_BIGDEC => Some(ValueType::BigDec),
        DB_TYPE_TUPLE => Some(ValueType::Tuple),
        DB_TYPE_SYMBOL => Some(ValueType::Symbol),
        _ => None,
    }
}

pub const fn cardinality_entity(cardinality: Cardinality) -> u64 {
    match cardinality {
        Cardinality::One => DB_CARDINALITY_ONE,
        Cardinality::Many => DB_CARDINALITY_MANY,
    }
}

pub const fn cardinality_for_entity(entity: u64) -> Option<Cardinality> {
    match entity {
        DB_CARDINALITY_ONE => Some(Cardinality::One),
        DB_CARDINALITY_MANY => Some(Cardinality::Many),
        _ => None,
    }
}

pub const fn unique_entity(unique: Unique) -> u64 {
    match unique {
        Unique::Value => DB_UNIQUE_VALUE,
        Unique::Identity => DB_UNIQUE_IDENTITY,
    }
}

pub const fn unique_for_entity(entity: u64) -> Option<Unique> {
    match entity {
        DB_UNIQUE_VALUE => Some(Unique::Value),
        DB_UNIQUE_IDENTITY => Some(Unique::Identity),
        _ => None,
    }
}

/// Every system entity whose ident is retained by the native kernel.
///
/// JVM Fressian tags, language/code metadata, install-function, and
/// JVM install-function machinery are intentionally absent. `:db/fn`
/// remains as an ordinary attribute whose value is a native program hash.
pub fn supported_system_idents() -> Vec<(u64, Keyword)> {
    SYSTEM_IDENT_SPECS
        .iter()
        .map(|&(entity, namespace, name)| (entity, Keyword::new(namespace, name)))
        .collect()
}

/// Typed projections of the installed native system attributes.
///
/// These values are construction/verification aids.  Once genesis is loaded,
/// runtime schema caches must be re-derived from its ordinary datoms.
pub fn supported_system_attributes() -> Vec<Attribute> {
    vec![
        Attribute::new(
            attr_id(DB_IDENT),
            kw("db", "ident"),
            ValueType::Keyword,
            Cardinality::One,
        )
        .unique(Unique::Identity),
        partition_install_attribute(),
        Attribute::new(
            attr_id(DB_INSTALL_ATTRIBUTE),
            kw("db.install", "attribute"),
            ValueType::Ref,
            Cardinality::Many,
        ),
        Attribute::new(
            attr_id(DB_EXCISE),
            kw("db", "excise"),
            ValueType::Ref,
            Cardinality::One,
        ),
        Attribute::new(
            attr_id(DB_EXCISE_ATTRS),
            kw("db.excise", "attrs"),
            ValueType::Ref,
            Cardinality::Many,
        ),
        Attribute::new(
            attr_id(DB_EXCISE_BEFORE_T),
            kw("db.excise", "beforeT"),
            ValueType::Long,
            Cardinality::One,
        ),
        Attribute::new(
            attr_id(DB_EXCISE_BEFORE),
            kw("db.excise", "before"),
            ValueType::Instant,
            Cardinality::One,
        ),
        Attribute::new(
            attr_id(DB_ALTER_ATTRIBUTE),
            kw("db.alter", "attribute"),
            ValueType::Ref,
            Cardinality::Many,
        ),
        Attribute::new(
            attr_id(DB_VALUE_TYPE),
            kw("db", "valueType"),
            ValueType::Ref,
            Cardinality::One,
        ),
        Attribute::new(
            attr_id(DB_CARDINALITY),
            kw("db", "cardinality"),
            ValueType::Ref,
            Cardinality::One,
        ),
        Attribute::new(
            attr_id(DB_UNIQUE),
            kw("db", "unique"),
            ValueType::Ref,
            Cardinality::One,
        ),
        Attribute::new(
            attr_id(DB_IS_COMPONENT),
            kw("db", "isComponent"),
            ValueType::Boolean,
            Cardinality::One,
        ),
        Attribute::new(
            attr_id(DB_INDEX),
            kw("db", "index"),
            ValueType::Boolean,
            Cardinality::One,
        ),
        Attribute::new(
            attr_id(DB_NO_HISTORY),
            kw("db", "noHistory"),
            ValueType::Boolean,
            Cardinality::One,
        ),
        indexed(Attribute::new(
            attr_id(DB_TX_INSTANT),
            kw("db", "txInstant"),
            ValueType::Instant,
            Cardinality::One,
        )),
        fulltext_attribute(),
        Attribute::new(
            attr_id(DB_FN),
            kw("db", "fn"),
            ValueType::Function,
            Cardinality::One,
        ),
        Attribute::new(
            attr_id(DB_DOC),
            kw("db", "doc"),
            ValueType::String,
            Cardinality::One,
        ),
        Attribute::new(
            attr_id(DB_TUPLE_TYPE),
            kw("db", "tupleType"),
            ValueType::Keyword,
            Cardinality::One,
        ),
        Attribute::new(
            attr_id(DB_TUPLE_TYPES),
            kw("db", "tupleTypes"),
            ValueType::Tuple,
            Cardinality::One,
        )
        .tuple(TupleSpec::Homogeneous(ValueType::Keyword)),
        Attribute::new(
            attr_id(DB_TUPLE_ATTRS),
            kw("db", "tupleAttrs"),
            ValueType::Tuple,
            Cardinality::One,
        )
        .tuple(TupleSpec::Homogeneous(ValueType::Keyword)),
        Attribute::new(
            attr_id(DB_ENSURE),
            kw("db", "ensure"),
            ValueType::Ref,
            Cardinality::Many,
        ),
        Attribute::new(
            attr_id(DB_ENTITY_ATTRS),
            kw("db.entity", "attrs"),
            ValueType::Keyword,
            Cardinality::Many,
        ),
        Attribute::new(
            attr_id(DB_ENTITY_PREDS),
            kw("db.entity", "preds"),
            ValueType::Symbol,
            Cardinality::Many,
        ),
        Attribute::new(
            attr_id(DB_ATTR_PREDS),
            kw("db.attr", "preds"),
            ValueType::Symbol,
            Cardinality::Many,
        ),
        Attribute::new(
            attr_id(DB_TUPLE_DISCONTINUED),
            kw("db.tuple", "discontinued"),
            ValueType::Boolean,
            Cardinality::One,
        ),
    ]
}

/// Canonical self-describing native database information at logical t=0.
///
/// Ordering is EAV/value order, independent of construction order.  Genesis
/// contains no transaction-instant datum because it is a protocol base, not a
/// committed user transaction.
pub fn canonical_genesis_datoms() -> Vec<Datom> {
    let tx = t_to_tx(0).expect("logical genesis t is always representable");
    let mut datoms = Vec::new();

    for (entity, ident) in supported_system_idents() {
        datoms.push(assertion(entity, DB_IDENT, Value::Keyword(ident), tx));
    }

    for attribute in supported_system_attributes() {
        let entity = u64::from(attribute.id);
        datoms.push(assertion(
            entity,
            DB_VALUE_TYPE,
            Value::Ref(value_type_entity(attribute.value_type)),
            tx,
        ));
        datoms.push(assertion(
            entity,
            DB_CARDINALITY,
            Value::Ref(cardinality_entity(attribute.cardinality)),
            tx,
        ));

        if let Some(unique) = attribute.unique {
            datoms.push(assertion(
                entity,
                DB_UNIQUE,
                Value::Ref(unique_entity(unique)),
                tx,
            ));
        }
        if attribute.indexed {
            datoms.push(assertion(entity, DB_INDEX, Value::Bool(true), tx));
        }
        if attribute.component {
            datoms.push(assertion(entity, DB_IS_COMPONENT, Value::Bool(true), tx));
        }
        if attribute.no_history {
            datoms.push(assertion(entity, DB_NO_HISTORY, Value::Bool(true), tx));
        }
        if let Some(TupleSpec::Homogeneous(value_type)) = attribute.tuple {
            datoms.push(assertion(
                entity,
                DB_TUPLE_TYPE,
                Value::Keyword(value_type_ident(value_type)),
                tx,
            ));
        }
        if attribute.tuple_discontinued {
            datoms.push(assertion(
                entity,
                DB_TUPLE_DISCONTINUED,
                Value::Bool(true),
                tx,
            ));
        }

        datoms.push(assertion(
            DB_PART_DB,
            DB_INSTALL_ATTRIBUTE,
            Value::Ref(entity),
            tx,
        ));
    }

    for part in [DB_PART_DB, DB_PART_TX, DB_PART_USER] {
        datoms.push(assertion(
            DB_PART_DB,
            DB_INSTALL_PARTITION,
            Value::Ref(part),
            tx,
        ));
    }
    datoms.sort_by(|left, right| left.cmp_in(right, IndexOrder::Eavt));
    debug_assert!(datoms.windows(2).all(|pair| pair[0] != pair[1]));
    datoms
}

pub(crate) fn fulltext_attribute() -> Attribute {
    Attribute::new(
        DB_FULLTEXT as u32,
        kw("db", "fulltext"),
        ValueType::Boolean,
        Cardinality::One,
    )
}

pub(crate) fn partition_install_attribute() -> Attribute {
    Attribute::new(
        DB_INSTALL_PARTITION as u32,
        kw("db.install", "partition"),
        ValueType::Ref,
        Cardinality::Many,
    )
}

fn indexed(mut attribute: Attribute) -> Attribute {
    attribute.indexed = true;
    attribute
}

fn assertion(entity: u64, attribute: u64, value: Value, tx: u64) -> Datom {
    Datom {
        entity,
        attribute: attr_id(attribute),
        value,
        tx,
        added: true,
    }
}

fn attr_id(entity: u64) -> u32 {
    schema_eid_to_attr_id(entity).expect("native vocabulary attribute IDs are valid")
}

fn kw(namespace: &str, name: &str) -> Keyword {
    Keyword::new(namespace, name)
}

pub(crate) fn value_type_ident(value_type: ValueType) -> Keyword {
    let entity = value_type_entity(value_type);
    SYSTEM_IDENT_SPECS
        .iter()
        .find_map(|&(candidate, namespace, name)| {
            (candidate == entity).then(|| kw(namespace, name))
        })
        .expect("every supported value type has a system ident")
}

const SYSTEM_IDENT_SPECS: &[(u64, &str, &str)] = &[
    (DB_PART_DB, "db.part", "db"),
    (DB_ADD, "db", "add"),
    (DB_RETRACT, "db", "retract"),
    (DB_PART_TX, "db.part", "tx"),
    (DB_PART_USER, "db.part", "user"),
    (DB_IDENT, "db", "ident"),
    (DB_INSTALL_PARTITION, "db.install", "partition"),
    (DB_INSTALL_ATTRIBUTE, "db.install", "attribute"),
    (DB_EXCISE, "db", "excise"),
    (DB_EXCISE_ATTRS, "db.excise", "attrs"),
    (DB_EXCISE_BEFORE_T, "db.excise", "beforeT"),
    (DB_EXCISE_BEFORE, "db.excise", "before"),
    (DB_ALTER_ATTRIBUTE, "db.alter", "attribute"),
    (DB_TYPE_REF, "db.type", "ref"),
    (DB_TYPE_KEYWORD, "db.type", "keyword"),
    (DB_TYPE_LONG, "db.type", "long"),
    (DB_TYPE_STRING, "db.type", "string"),
    (DB_TYPE_BOOLEAN, "db.type", "boolean"),
    (DB_TYPE_INSTANT, "db.type", "instant"),
    (DB_TYPE_FN, "db.type", "fn"),
    (DB_TYPE_BYTES, "db.type", "bytes"),
    (DB_CARDINALITY_ONE, "db.cardinality", "one"),
    (DB_CARDINALITY_MANY, "db.cardinality", "many"),
    (DB_UNIQUE_VALUE, "db.unique", "value"),
    (DB_UNIQUE_IDENTITY, "db.unique", "identity"),
    (DB_VALUE_TYPE, "db", "valueType"),
    (DB_CARDINALITY, "db", "cardinality"),
    (DB_UNIQUE, "db", "unique"),
    (DB_IS_COMPONENT, "db", "isComponent"),
    (DB_INDEX, "db", "index"),
    (DB_NO_HISTORY, "db", "noHistory"),
    (DB_TX_INSTANT, "db", "txInstant"),
    (DB_FULLTEXT, "db", "fulltext"),
    (DB_FN, "db", "fn"),
    (DB_FN_RETRACT_ENTITY, "db.fn", "retractEntity"),
    (DB_FN_CAS, "db.fn", "cas"),
    (DB_TYPE_UUID, "db.type", "uuid"),
    (DB_TYPE_DOUBLE, "db.type", "double"),
    (DB_TYPE_FLOAT, "db.type", "float"),
    (DB_TYPE_URI, "db.type", "uri"),
    (DB_TYPE_BIGINT, "db.type", "bigint"),
    (DB_TYPE_BIGDEC, "db.type", "bigdec"),
    (DB_DOC, "db", "doc"),
    (DB_TYPE_TUPLE, "db.type", "tuple"),
    (DB_TYPE_SYMBOL, "db.type", "symbol"),
    (DB_TUPLE_TYPE, "db", "tupleType"),
    (DB_TUPLE_TYPES, "db", "tupleTypes"),
    (DB_TUPLE_ATTRS, "db", "tupleAttrs"),
    (DB_ENSURE, "db", "ensure"),
    (DB_ENTITY_ATTRS, "db.entity", "attrs"),
    (DB_ENTITY_PREDS, "db.entity", "preds"),
    (DB_ATTR_PREDS, "db.attr", "preds"),
    (DB_TUPLE_DISCONTINUED, "db.tuple", "discontinued"),
];
