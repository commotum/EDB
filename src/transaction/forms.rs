//! Declarative transaction input; unresolved references are not stored values.
use crate::{Keyword, ProgramCall, Value};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EntityRef {
    Id(u64),
    Ident(crate::Keyword),
    Temp(String),
    Lookup {
        attribute: u32,
        value: Value,
    },
    /// A lookup key whose ref or tuple-ref slots use transaction entity forms.
    /// Resolution is always against db-before, never transaction-local tempids.
    LookupInput {
        attribute: u32,
        value: Box<TxValue>,
    },
    Tx,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TxValue {
    Scalar(Value),
    Entity(EntityRef),
    /// Transaction-only tuple slots. References remain symbolic until entity
    /// allocation; `None` is a tuple nil. Stored tuple values never contain
    /// unresolved identities, and nested tuple slots are not valid schema data.
    Tuple(Vec<Option<TxValue>>),
}

impl From<Value> for TxValue {
    fn from(value: Value) -> Self {
        Self::Scalar(value)
    }
}

#[derive(Clone, Debug)]
pub enum TxOp {
    Add {
        entity: EntityRef,
        attribute: u32,
        value: TxValue,
    },
    Retract {
        entity: EntityRef,
        attribute: u32,
        value: Option<TxValue>,
    },
    Cas {
        entity: EntityRef,
        attribute: u32,
        old: Option<TxValue>,
        new: TxValue,
    },
    RetractEntity(EntityRef),
    Ensure {
        entity: EntityRef,
        spec: EntityRef,
    },
    /// Transaction-local allocation policy, never a stored datom. Existing
    /// upsert identities keep their entity IDs regardless of this directive.
    ForcePartition {
        tempid: String,
        partition: EntityRef,
    },
    /// Assign a new tempid to the partition of an existing entity or another
    /// transaction tempid's assignment. Affinity cycles are invalid.
    MatchPartition {
        tempid: String,
        entity: EntityRef,
    },
    /// Native normalized form of an ordinary schema installation transaction.
    InstallAttribute(crate::Attribute),
    /// Native normalized form of an ordinary schema alteration transaction.
    AlterAttribute(crate::Attribute),
}

#[derive(Clone, Debug)]
pub enum AttributeRef {
    Id(u32),
    Ident(Keyword),
    ReverseId(u32),
    ReverseIdent(Keyword),
}

#[derive(Clone, Debug)]
pub enum MapValue {
    Value(TxValue),
    Nested(Box<EntityMap>),
    Many(Vec<MapValue>),
}

impl From<TxValue> for MapValue {
    fn from(value: TxValue) -> Self {
        Self::Value(value)
    }
}

#[derive(Clone, Debug)]
pub struct EntityMap {
    pub id: Option<EntityRef>,
    pub attributes: Vec<(AttributeRef, MapValue)>,
}

#[derive(Clone, Debug)]
pub struct TxCall {
    pub function: String,
    pub arguments: Vec<TxValue>,
}

#[derive(Clone, Debug)]
pub enum TxForm {
    Op(TxOp),
    EntityMap(EntityMap),
    /// A persisted/native database-function call. The authoritative
    /// transactor resolves this against the immutable db-before before the
    /// ordinary transaction normalizer runs.
    ProgramCall(ProgramCall),
    /// A process-local Rust callback used by speculative `with_forms` calls.
    /// This is deliberately distinct from a temporal database function.
    Call(TxCall),
    /// Schema-independent EDN data, lowered only against authoritative db-before.
    Edn(crate::edn_transaction::EdnTransactionForm),
}

pub(crate) fn forms_have_edn(forms: &[TxForm]) -> bool {
    forms.iter().any(|form| matches!(form, TxForm::Edn(_)))
}
