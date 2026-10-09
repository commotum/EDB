use crate::{
    AttributeRef, Database, DatabaseValue, Digest, EntityMap, EntityRef, ErrorCategory,
    IndexPrefix, Keyword, MapValue, SemanticError, TxForm, TxOp, TxValue, Value,
};
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering as MemoryOrdering};
use std::time::Instant;

#[path = "program_query.rs"]
pub(crate) mod native_query;
pub use native_query::{QueryTemplateSource, QueryTemplateTime};

pub type ProgramHash = Digest;

const MAX_ARITY: u8 = 10;
const MAX_INSTRUCTIONS: usize = 4_096;
const MAX_BLOCK_DEPTH: usize = 32;
/// The single current program format, including the shared query AST.
/// Earlier development formats require fresh databases.
pub const PROGRAM_ABI_VERSION: u16 = 13;
pub const MAX_QUERY_PATTERNS: usize = 64;
pub const MAX_QUERY_VARIABLES: usize = 32;

/// Non-datom values accepted and produced while evaluating a program.
/// Stored `Value` remains restricted to legal database values; transaction
/// function arguments may additionally carry finite collections, maps,
/// explicit entity references, and nil.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeValue {
    Scalar(Value),
    Entity(EntityRef),
    Null,
    Vector(Vec<RuntimeValue>),
    /// Entries are kept in canonical stored-value order with unique keys.
    Map(Vec<(Value, RuntimeValue)>),
    /// Lossless query-only data. Not a stored value or a portable transaction
    /// argument; query programs may receive and produce this carrier.
    Query(crate::QueryValue),
}

impl From<Value> for RuntimeValue {
    fn from(value: Value) -> Self {
        Self::Scalar(value)
    }
}

impl From<EntityRef> for RuntimeValue {
    fn from(entity: EntityRef) -> Self {
        Self::Entity(entity)
    }
}

impl RuntimeValue {
    pub(crate) fn from_query_value(value: crate::QueryValue) -> Self {
        match &value {
            crate::QueryValue::Nil => Self::Null,
            crate::QueryValue::Scalar(_) => {
                Self::Scalar(value.into_scalar().expect("scalar variant"))
            }
            _ => Self::Query(value),
        }
    }
    pub fn map(mut entries: Vec<(Value, RuntimeValue)>) -> Result<Self, SemanticError> {
        for (key, _) in &entries {
            crate::transaction::input::validate_stored_input(key)?;
        }
        entries.sort_by(|left, right| left.0.stored_cmp(&right.0));
        if entries
            .windows(2)
            .any(|entries| entries[0].0.index_cmp(&entries[1].0) == Ordering::Equal)
        {
            return Err(incorrect(
                "program/duplicate-map-key",
                "runtime maps cannot contain duplicate keys",
            ));
        }
        Ok(Self::Map(entries))
    }

    /// Total structural order used for canonical declarative call ordering.
    /// Logical value comparison comes first; retained representation breaks
    /// ties so retry identity cannot depend on debug formatting.
    pub fn canonical_cmp(&self, other: &Self) -> Ordering {
        runtime_value_rank(self)
            .cmp(&runtime_value_rank(other))
            .then_with(|| match (self, other) {
                (Self::Scalar(left), Self::Scalar(right)) => left.stored_cmp(right),
                (Self::Entity(left), Self::Entity(right)) => canonical_entity_ref_cmp(left, right),
                (Self::Vector(left), Self::Vector(right)) => {
                    canonical_slice_cmp(left, right, Self::canonical_cmp)
                }
                (Self::Map(left), Self::Map(right)) => {
                    canonical_slice_cmp(left, right, |left, right| {
                        left.0
                            .stored_cmp(&right.0)
                            .then_with(|| left.1.canonical_cmp(&right.1))
                    })
                }
                (Self::Query(left), Self::Query(right)) => left.canonical_cmp(right),
                _ => Ordering::Equal,
            })
    }
}

fn runtime_value_rank(value: &RuntimeValue) -> u8 {
    match value {
        RuntimeValue::Null => 0,
        RuntimeValue::Scalar(_) => 1,
        RuntimeValue::Entity(_) => 2,
        RuntimeValue::Vector(_) => 3,
        RuntimeValue::Map(_) => 4,
        RuntimeValue::Query(_) => 5,
    }
}

fn canonical_entity_ref_cmp(left: &EntityRef, right: &EntityRef) -> Ordering {
    entity_ref_rank(left)
        .cmp(&entity_ref_rank(right))
        .then_with(|| match (left, right) {
            (EntityRef::Id(left), EntityRef::Id(right)) => left.cmp(right),
            (EntityRef::Ident(left), EntityRef::Ident(right)) => {
                canonical_optional_text_cmp(left.namespace.as_deref(), right.namespace.as_deref())
                    .then_with(|| left.name.encode_utf16().cmp(right.name.encode_utf16()))
            }
            (EntityRef::Temp(left), EntityRef::Temp(right)) => {
                left.encode_utf16().cmp(right.encode_utf16())
            }
            (EntityRef::Edn(left), EntityRef::Edn(right)) => left.cmp(right),
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
                .then_with(|| left_value.stored_cmp(right_value)),
            (
                EntityRef::LookupInput {
                    attribute: left_attribute,
                    value: left_value,
                },
                EntityRef::LookupInput {
                    attribute: right_attribute,
                    value: right_value,
                },
            ) => left_attribute
                .cmp(right_attribute)
                .then_with(|| canonical_tx_value_cmp(left_value, right_value)),
            _ => Ordering::Equal,
        })
}

fn canonical_tx_value_cmp(left: &TxValue, right: &TxValue) -> Ordering {
    let rank = |value: &TxValue| match value {
        TxValue::Scalar(_) => 0,
        TxValue::Entity(_) => 1,
        TxValue::Tuple(_) => 2,
    };
    rank(left)
        .cmp(&rank(right))
        .then_with(|| match (left, right) {
            (TxValue::Scalar(left), TxValue::Scalar(right)) => left.stored_cmp(right),
            (TxValue::Entity(left), TxValue::Entity(right)) => {
                canonical_entity_ref_cmp(left, right)
            }
            (TxValue::Tuple(left), TxValue::Tuple(right)) => {
                canonical_slice_cmp(left, right, |left, right| match (left, right) {
                    (None, None) => Ordering::Equal,
                    (None, Some(_)) => Ordering::Less,
                    (Some(_), None) => Ordering::Greater,
                    (Some(left), Some(right)) => canonical_tx_value_cmp(left, right),
                })
            }
            _ => Ordering::Equal,
        })
}

fn canonical_optional_text_cmp(left: Option<&str>, right: Option<&str>) -> Ordering {
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(left), Some(right)) => left.encode_utf16().cmp(right.encode_utf16()),
    }
}

fn entity_ref_rank(entity: &EntityRef) -> u8 {
    match entity {
        EntityRef::Id(_) => 0,
        EntityRef::Ident(_) => 1,
        EntityRef::Temp(_) => 2,
        EntityRef::Lookup { .. } => 3,
        EntityRef::Tx => 4,
        EntityRef::LookupInput { .. } => 5,
        EntityRef::Edn(_) => 6,
    }
}

fn canonical_slice_cmp<T, F>(left: &[T], right: &[T], mut compare: F) -> Ordering
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

/// One term in the deliberately small Datalog host IR used by persisted
/// programs. Variables are dense numeric slots, inputs address the enclosing
/// program's arguments, and constants are ordinary stored values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QueryTerm {
    Variable(u8),
    Input(u8),
    Constant(Value),
}

impl QueryTerm {
    fn canonical_cmp(&self, other: &Self) -> Ordering {
        query_term_rank(self)
            .cmp(&query_term_rank(other))
            .then_with(|| match (self, other) {
                (Self::Variable(left), Self::Variable(right))
                | (Self::Input(left), Self::Input(right)) => left.cmp(right),
                (Self::Constant(left), Self::Constant(right)) => left.stored_cmp(right),
                _ => Ordering::Equal,
            })
    }
}

fn query_term_rank(term: &QueryTerm) -> u8 {
    match term {
        QueryTerm::Variable(_) => 0,
        QueryTerm::Input(_) => 1,
        QueryTerm::Constant(_) => 2,
    }
}

/// Restricted EDB data-pattern clause `[e a v]`.
///
/// [`QueryTemplate::new`] lowers these authoring forms to the shared query AST.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryPattern {
    pub entity: QueryTerm,
    pub attribute: u32,
    pub value: QueryTerm,
}

impl QueryPattern {
    pub fn new(entity: QueryTerm, attribute: u32, value: QueryTerm) -> Self {
        Self {
            entity,
            attribute,
            value,
        }
    }

    fn canonical_cmp(&self, other: &Self) -> Ordering {
        self.attribute
            .cmp(&other.attribute)
            .then_with(|| self.entity.canonical_cmp(&other.entity))
            .then_with(|| self.value.canonical_cmp(&other.value))
    }
}

/// A persisted query over the shared native query AST and evaluator.
/// The compact constructor is an authoring convenience with canonical row order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryTemplate {
    native: Box<native_query::NativeQueryTemplate>,
    pub(crate) canonical_order: bool,
}

impl QueryTemplate {
    pub fn new(find: Vec<u8>, mut patterns: Vec<QueryPattern>) -> Result<Self, SemanticError> {
        patterns.sort_by(QueryPattern::canonical_cmp);
        if find.is_empty() || find.len() > MAX_QUERY_VARIABLES {
            return Err(incorrect(
                "program/query-find-shape",
                format!("query find must contain 1 to {MAX_QUERY_VARIABLES} variable slots"),
            ));
        }
        if patterns.is_empty() || patterns.len() > MAX_QUERY_PATTERNS {
            return Err(incorrect(
                "program/query-pattern-limit",
                format!("query must contain 1 to {MAX_QUERY_PATTERNS} data patterns"),
            ));
        }
        if patterns
            .windows(2)
            .any(|pair| pair[0].canonical_cmp(&pair[1]) != Ordering::Less)
        {
            return Err(incorrect(
                "program/noncanonical-query",
                "query patterns must be unique",
            ));
        }
        let variables = patterns
            .iter()
            .flat_map(|pattern| [&pattern.entity, &pattern.value])
            .filter_map(|term| match term {
                QueryTerm::Variable(variable) => Some(*variable),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        if variables.len() > MAX_QUERY_VARIABLES
            || variables
                .iter()
                .copied()
                .ne((0..variables.len()).map(|variable| variable as u8))
        {
            return Err(incorrect(
                "program/noncanonical-query-variables",
                "query variables must use dense slots beginning at zero",
            ));
        }
        if find.iter().any(|variable| !variables.contains(variable)) {
            return Err(incorrect(
                "program/unbound-query-find",
                "every query find slot must be bound by a data pattern",
            ));
        }
        let arguments = patterns
            .iter()
            .flat_map(|pattern| [&pattern.entity, &pattern.value])
            .filter_map(|term| match term {
                QueryTerm::Input(index) => Some(*index),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        fn term(term: QueryTerm) -> crate::Term {
            match term {
                QueryTerm::Variable(index) => crate::Term::Variable(variable("v", index)),
                QueryTerm::Input(index) => crate::Term::Variable(variable("input", index)),
                QueryTerm::Constant(value) => crate::Term::Constant(value),
            }
        }
        fn variable(prefix: &str, index: u8) -> crate::Variable {
            crate::Variable::new(format!("{prefix}{index}")).expect("generated variable name")
        }
        let mut query = crate::Query::new(
            crate::FindSpec::Relation(
                find.into_iter()
                    .map(|index| crate::FindElement::Variable(variable("v", index)))
                    .collect(),
            ),
            patterns
                .into_iter()
                .map(|pattern| {
                    crate::Clause::Pattern(Box::new(crate::DataPattern::new(
                        term(pattern.entity),
                        crate::Term::Constant(Value::Ref(u64::from(pattern.attribute))),
                        term(pattern.value),
                    )))
                })
                .collect(),
        );
        query.inputs = arguments
            .iter()
            .map(|index| crate::InputSpec::Scalar(variable("input", *index)))
            .collect();
        let mut template = Self::native(query, arguments.into_iter().collect(), vec![])?;
        template.canonical_order = true;
        Ok(template)
    }

    /// Bind ordinary native query inputs to program arguments. Named sources
    /// are immutable views of this invocation's database.
    pub fn native(
        query: crate::Query,
        input_arguments: Vec<u8>,
        sources: Vec<QueryTemplateSource>,
    ) -> Result<Self, SemanticError> {
        Ok(Self {
            native: Box::new(native_query::NativeQueryTemplate::new(
                query,
                input_arguments,
                sources,
            )?),
            canonical_order: false,
        })
    }

    pub fn native_query(&self) -> &crate::Query {
        &self.native.query
    }

    pub(crate) fn native_spec(&self) -> &native_query::NativeQueryTemplate {
        &self.native
    }

    fn validate(&self, arity: u8) -> Result<(), SemanticError> {
        if self.canonical_order && !matches!(self.native.query.find, crate::FindSpec::Relation(_)) {
            return Err(incorrect(
                "program/query-result-shape",
                "canonical query ordering requires a relation",
            ));
        }
        self.native.validate()?;
        self.native.validate_arity(arity)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgramKind {
    Transaction,
    AttributePredicate,
    EntityPredicate,
    Query,
    /// One immutable symbol that is callable in both documented predicate
    /// positions. The two roles intentionally have separate bodies: an
    /// attribute predicate receives only its value, while an entity predicate
    /// receives the exact db-after capability plus its entity id.
    DualPredicate,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Instruction {
    PushArgument(u8),
    PushConstant(Value),
    PushEntity(EntityRef),
    PushNull,
    Duplicate,
    Pop,
    Swap,
    MakeVector(u8),
    MakeMap(u8),
    Get,
    ContainsKey,
    Length,
    Unpack(u8),
    If {
        then_branch: Vec<Instruction>,
        else_branch: Vec<Instruction>,
    },
    /// Canonical bodies for [`ProgramKind::DualPredicate`]. This instruction
    /// is legal only as the first instruction of the exact top-level shape
    /// `[PredicateDispatch, Return]`; it can never be reached as ordinary
    /// bytecode without an explicit predicate-role invocation.
    PredicateDispatch {
        attribute: Vec<Instruction>,
        entity: Vec<Instruction>,
    },
    /// Consume a finite vector/map and run the body once per item. Each body
    /// invocation starts with only the current item on its stack and must
    /// consume it completely. `PushArgument` continues to address the outer
    /// function arguments.
    ForEach {
        body: Vec<Instruction>,
    },
    LoadOne(u32),
    LoadMany(u32),
    Exists(u32),
    /// Execute a bounded conjunctive Datalog template against the exact
    /// immutable db-before and push its relation as vector-of-vector values.
    Query(QueryTemplate),
    Add,
    Subtract,
    Multiply,
    Divide,
    Equal,
    LessThan,
    GreaterThan,
    Not,
    And,
    Or,
    Require {
        category: ErrorCategory,
        message: String,
    },
    /// Consume `[condition, anomaly-map]`. Literal true continues; literal
    /// false raises the incorrect/conflict anomaly described by
    /// the map. No truthiness coercion is permitted.
    RequireAnomaly,
    /// Emit an add with a scalar, entity reference, or 2–8-slot runtime
    /// vector. A vector becomes a transaction tuple; its entity references
    /// resolve under the attribute's tuple schema, and nil slots stay nil.
    EmitAdd(u32),
    /// Emit a value-specific retraction using the same value grammar as add.
    EmitRetract(u32),
    /// Emit the value-less `[:db/retract e a]` convenience form. Expansion to
    /// matching db-before values remains the ordinary transaction kernel's
    /// responsibility.
    EmitRetractAll(u32),
    /// Emit compare-and-swap using the same value grammar as add. A nil old
    /// value means the attribute must be absent, not an all-nil tuple.
    EmitCas(u32),
    EmitRetractEntity,
    EmitEnsure,
    /// Consume `[tempid, partition]` and emit allocation policy, not datoms.
    /// The tempid may be a native Temp reference or a nonempty string;
    /// a named partition may be an Ident reference or a qualified keyword.
    EmitForcePartition,
    /// Consume `[tempid, entity]` to place a newly allocated tempid in the
    /// same partition as another new or existing entity.
    EmitMatchPartition,
    /// Consume a runtime map and emit a canonical transaction entity-map.
    /// Map vectors retain their collection-of-values meaning; use explicit
    /// add/retract/CAS emission for tuples containing entity references.
    EmitEntityMap,
    /// Emit a nested transaction-function call as declarative transaction
    /// data. The caller recursively expands it against the same db-before.
    EmitCall {
        function: CallableRef,
        argument_count: u8,
    },
    EmitRow(u8),
    Return,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Program {
    pub kind: ProgramKind,
    pub arity: u8,
    pub instructions: Vec<Instruction>,
}

/// An internal capability proving that a program crossed a canonical
/// validation boundary before reuse.
///
/// Persisted programs enter through `decode_program`, and locally deployed
/// programs enter through `encode_program`; both validate before PostgreSQL
/// constructs this wrapper. Keeping the field private prevents a raw public
/// `Program` from reaching the unchecked execution entry points.
#[derive(Debug)]
pub(crate) struct ValidatedProgram {
    program: Program,
}

impl ValidatedProgram {
    pub(crate) fn from_canonical(program: Program) -> Self {
        Self { program }
    }

    pub(crate) fn program(&self) -> &Program {
        &self.program
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProgramInvocation {
    pub hash: ProgramHash,
    pub arguments: Vec<Value>,
}

/// Typed callable identity for declarative transaction data.
///
/// Database callables resolve `:db/fn` from the locked db-before. Exact hashes
/// are an explicit native pinning extension. Local symbols are reserved for a
/// process registry and are never confused with temporal database functions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CallableRef {
    Database(EntityRef),
    ExactHash(ProgramHash),
    Local(crate::Symbol),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProgramCall {
    pub function: CallableRef,
    pub arguments: Vec<RuntimeValue>,
}

#[derive(Clone, Copy, Debug)]
pub struct ProgramControl<'a> {
    pub fuel: u64,
    pub max_stack: usize,
    /// Maximum cumulative runtime-value allocation/clone bytes.
    pub max_value_bytes: usize,
    /// Maximum elements in any runtime vector or map.
    pub max_collection_items: usize,
    /// Maximum emitted forms or rows across one transaction execution.
    pub max_forms: usize,
    /// Maximum canonical payload bytes emitted across one transaction
    /// execution. This is independent of `max_forms`, so neither many tiny
    /// forms nor one very large value can evade accounting.
    pub max_output: usize,
    /// Maximum interpreted invocations sharing this budget. Recursive callers
    /// still enforce a separate nesting-depth limit; this bound prevents many
    /// sibling calls from resetting execution quotas.
    pub max_calls: usize,
    pub cancelled: Option<&'a AtomicBool>,
}

/// Owned resource policy suitable for a long-lived transaction service.
/// `ProgramControl` additionally carries a borrowed cancellation flag for
/// one direct invocation; persisted service work uses these durable numeric
/// limits and shares one budget across calls, predicates, and query hosts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProgramLimits {
    pub fuel: u64,
    pub max_stack: usize,
    pub max_value_bytes: usize,
    pub max_collection_items: usize,
    pub max_forms: usize,
    pub max_output: usize,
    pub max_calls: usize,
}

impl Default for ProgramLimits {
    fn default() -> Self {
        let control = ProgramControl::default();
        Self {
            fuel: control.fuel,
            max_stack: control.max_stack,
            max_value_bytes: control.max_value_bytes,
            max_collection_items: control.max_collection_items,
            max_forms: control.max_forms,
            max_output: control.max_output,
            max_calls: control.max_calls,
        }
    }
}

impl ProgramLimits {
    pub(crate) fn control(self) -> ProgramControl<'static> {
        ProgramControl {
            fuel: self.fuel,
            max_stack: self.max_stack,
            max_value_bytes: self.max_value_bytes,
            max_collection_items: self.max_collection_items,
            max_forms: self.max_forms,
            max_output: self.max_output,
            max_calls: self.max_calls,
            cancelled: None,
        }
    }
}

impl Default for ProgramControl<'_> {
    fn default() -> Self {
        Self {
            fuel: 100_000,
            max_stack: 1_024,
            max_value_bytes: 4 * 1024 * 1024,
            max_collection_items: 100_000,
            max_forms: 100_000,
            max_output: 100_000,
            max_calls: 10_000,
            cancelled: None,
        }
    }
}

/// Mutable accounting shared by every initial call, recursively emitted call,
/// predicate, and query host operation in one transaction attempt. Peer query
/// extensions also use this accounting and charge their consumed fuel back to
/// the enclosing query's shared allowance.
///
/// `ProgramControl` is only configuration. Creating a new budget per
/// invocation is intentionally a convenience for isolated evaluation; the
/// authoritative transaction path must create one budget and repeatedly call
/// `execute_with_budget`.
#[derive(Debug)]
pub struct ProgramBudget<'a> {
    fuel: u64,
    max_stack: usize,
    max_value_bytes: usize,
    max_collection_items: usize,
    max_forms: usize,
    max_output_bytes: usize,
    max_calls: usize,
    emitted_forms: usize,
    emitted_bytes: usize,
    value_bytes: usize,
    calls: usize,
    cancelled: Option<&'a AtomicBool>,
    deadline: Option<Instant>,
    query_numeric_bytes: usize,
}

impl<'a> ProgramBudget<'a> {
    pub fn new(control: ProgramControl<'a>) -> Result<Self, SemanticError> {
        if control.max_stack == 0
            || control.max_value_bytes == 0
            || control.max_collection_items == 0
            || control.max_forms == 0
            || control.max_output == 0
            || control.max_calls == 0
        {
            return Err(busy(
                "program/resource-limit",
                "program stack, form, byte, and call limits must be positive",
            ));
        }
        Ok(Self {
            fuel: control.fuel,
            max_stack: control.max_stack,
            max_value_bytes: control.max_value_bytes,
            max_collection_items: control.max_collection_items,
            max_forms: control.max_forms,
            max_output_bytes: control.max_output,
            max_calls: control.max_calls,
            emitted_forms: 0,
            emitted_bytes: 0,
            value_bytes: 0,
            calls: 0,
            cancelled: control.cancelled,
            deadline: None,
            query_numeric_bytes: 16 * 1024 * 1024,
        })
    }

    /// Internal read-side bridge for an enclosing query's absolute deadline.
    /// Transaction program controls and persisted bytecode retain their ABI.
    pub(crate) fn with_deadline(mut self, deadline: Option<Instant>) -> Self {
        self.deadline = deadline;
        self
    }

    /// Preserve an enclosing query's numeric admission policy without changing
    /// the persisted program ABI or transaction-facing ProgramControl.
    pub(crate) fn with_query_numeric_bytes(mut self, bytes: usize) -> Self {
        self.query_numeric_bytes = bytes;
        self
    }

    pub(crate) fn query_numeric_bytes(&self) -> usize {
        self.query_numeric_bytes
            .min(self.query_remaining_value_bytes())
    }

    pub fn remaining_fuel(&self) -> u64 {
        self.fuel
    }

    pub fn emitted_forms(&self) -> usize {
        self.emitted_forms
    }

    pub fn emitted_bytes(&self) -> usize {
        self.emitted_bytes
    }

    pub fn calls(&self) -> usize {
        self.calls
    }

    pub fn value_bytes(&self) -> usize {
        self.value_bytes
    }

    pub(crate) fn query_cancelled(&self) -> Option<&AtomicBool> {
        self.cancelled
    }

    pub(crate) fn query_deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub(crate) fn query_row_limit(&self) -> usize {
        self.max_collection_items
    }

    pub(crate) fn query_remaining_value_bytes(&self) -> usize {
        self.max_value_bytes.saturating_sub(self.value_bytes)
    }

    /// Debit already executed native query work even on cancellation/error.
    pub(crate) fn charge_query_work(&mut self, work: u64) -> Result<(), SemanticError> {
        let available = self.fuel;
        self.fuel = available.saturating_sub(work);
        if work > available {
            return Err(busy("program/fuel-exhausted", "program exhausted its fuel"));
        }
        Ok(())
    }

    pub(crate) fn charge_query_bytes(&mut self, bytes: usize) -> Result<(), SemanticError> {
        let next = self.value_bytes.checked_add(bytes);
        if next.is_none_or(|next| next > self.max_value_bytes) {
            self.value_bytes = self.max_value_bytes;
            return Err(busy(
                "program/value-byte-limit",
                "queries exceeded their shared runtime-value byte limit",
            ));
        }
        self.value_bytes = next.unwrap();
        Ok(())
    }

    pub(crate) fn native_check(&mut self, work: u64) -> Result<(), SemanticError> {
        self.charge(work)
    }

    pub(crate) fn native_begin(&mut self, arguments: &[RuntimeValue]) -> Result<(), SemanticError> {
        self.check_cancel()?;
        self.begin_call()?;
        for argument in arguments {
            self.charge_runtime_value(argument)?;
        }
        Ok(())
    }

    pub(crate) fn native_result(&mut self, result: &RuntimeValue) -> Result<(), SemanticError> {
        self.check_cancel()?;
        self.charge_runtime_value(result)
    }

    pub(crate) fn native_forms(&mut self, forms: &[TxForm]) -> Result<(), SemanticError> {
        self.check_cancel()?;
        crate::transaction::input::validate_forms_input(forms)?;
        for form in forms {
            let bytes = crate::encoding::persistent_tx_form_bytes(form)?;
            self.charge(usize_as_u64(bytes)?)?;
            self.reserve_form(bytes)?;
        }
        Ok(())
    }

    fn begin_call(&mut self) -> Result<(), SemanticError> {
        self.calls = self.calls.checked_add(1).ok_or_else(|| {
            busy(
                "program/resource-limit",
                "program call accounting overflowed",
            )
        })?;
        if self.calls > self.max_calls {
            return Err(busy(
                "program/call-limit",
                "transaction programs exceeded their shared call limit",
            ));
        }
        Ok(())
    }

    fn check_cancel(&self) -> Result<(), SemanticError> {
        check_cancel(self.cancelled)?;
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(SemanticError::new(
                ErrorCategory::Interrupted,
                "program/timeout",
                "program execution exceeded its enclosing query deadline",
            ));
        }
        Ok(())
    }

    fn charge(&mut self, amount: u64) -> Result<(), SemanticError> {
        self.check_cancel()?;
        charge(&mut self.fuel, amount)
    }

    fn charge_runtime_value(&mut self, value: &RuntimeValue) -> Result<(), SemanticError> {
        self.check_query_carriers(value)?;
        validate_runtime_value(value, self.max_collection_items, 0)?;
        let bytes = runtime_value_bytes(value, 0)?;
        self.charge(usize_as_u64(bytes)?)?;
        let next = checked_size_add(self.value_bytes, bytes)?;
        if next > self.max_value_bytes {
            return Err(busy(
                "program/value-byte-limit",
                "programs exceeded their shared runtime-value byte limit",
            ));
        }
        self.value_bytes = next;
        Ok(())
    }

    fn check_query_data(&mut self, value: &crate::QueryValue) -> Result<usize, SemanticError> {
        let size = value.measure_with(&mut |work| self.charge(usize_as_u64(work)?))?;
        if size.retained_bytes.saturating_add(size.canonical_bytes)
            > self.max_value_bytes.saturating_sub(self.value_bytes)
        {
            return Err(busy(
                "program/value-byte-limit",
                "general query data exceeds remaining runtime retention",
            ));
        }
        validate_general_runtime(value, self.max_collection_items, 0)?;
        value.validate_with(&mut |work| self.charge(usize_as_u64(work)?))?;
        Ok(size.retained_bytes)
    }

    fn check_query_carriers(&mut self, value: &RuntimeValue) -> Result<(), SemanticError> {
        self.check_query_carriers_at(value, 0)
    }
    fn check_query_carriers_at(
        &mut self,
        value: &RuntimeValue,
        depth: usize,
    ) -> Result<(), SemanticError> {
        if depth > 16 {
            return Err(incorrect(
                "program/value-depth",
                "runtime values may contain at most 16 collection levels",
            ));
        }
        match value {
            RuntimeValue::Query(value) => self.check_query_data(value).map(|_| ()),
            RuntimeValue::Vector(values) => {
                for value in values {
                    self.check_query_carriers_at(value, depth + 1)?;
                }
                Ok(())
            }
            RuntimeValue::Map(entries) => {
                for (_, value) in entries {
                    self.check_query_carriers_at(value, depth + 1)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn reserve_form(&mut self, bytes: usize) -> Result<(), SemanticError> {
        let forms = self.emitted_forms.checked_add(1).ok_or_else(|| {
            busy(
                "program/resource-limit",
                "program form accounting overflowed",
            )
        })?;
        if forms > self.max_forms {
            return Err(busy(
                "program/form-limit",
                "programs exceeded their shared emitted-form limit",
            ));
        }
        reserve_output(&mut self.emitted_bytes, bytes, self.max_output_bytes)?;
        self.emitted_forms = forms;
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub enum ProgramOutput {
    /// Full declarative transaction data. Nested persisted calls remain forms
    /// so the authoritative expander can resolve and recurse using db-before.
    Transaction(Vec<TxForm>),
    /// Predicate results retain their exact value. Only `Bool(true)` succeeds;
    /// callers report any other value rather than coercing or discarding it.
    AttributePredicate(RuntimeValue),
    EntityPredicate(RuntimeValue),
    Query(Vec<Vec<Value>>),
    /// Query-only cells which cannot be represented as stored `Value`s.
    GeneralQuery(Vec<Vec<crate::QueryValue>>),
}

pub fn is_exact_true(value: &RuntimeValue) -> bool {
    matches!(value, RuntimeValue::Scalar(Value::Bool(true)))
}

pub fn require_exact_true(
    value: &RuntimeValue,
    predicate: impl Into<String>,
) -> Result<(), SemanticError> {
    if is_exact_true(value) {
        return Ok(());
    }
    let predicate = predicate.into();
    Err(incorrect(
        "program/predicate-failed",
        format!("predicate {predicate} returned a value other than true"),
    )
    .detail("predicate", predicate)
    .detail("result", format!("{value:?}")))
}

#[derive(Clone, Debug, Default)]
pub struct ProgramRuntime;

/// The two concrete read capabilities needed by persisted programs.
///
/// The eager variant preserves the semantic oracle and public convenience
/// APIs. Authoritative persisted transactions, predicates, and queries use an
/// exact `DatabaseValue`, whose eager/native representation and
/// temporal/filter state remain hidden behind the same small set of reads.
/// This is deliberately not a storage trait: PostgreSQL peer snapshots are
/// already one concrete database value.
#[derive(Clone, Copy)]
enum ProgramRead<'a> {
    AttributePredicate,
    Eager(&'a Database),
    Exact(&'a DatabaseValue),
}

enum ProgramPrefixCursor<'a> {
    Eager(std::iter::Cloned<std::slice::Iter<'a, crate::Datom>>),
    Exact(Box<crate::DatabaseValuePrefixCursor<'a>>),
}

impl ProgramPrefixCursor<'_> {
    /// Keep the exact cursor's raw-candidate and hidden merge-loop polls.
    /// Erasing it to Iterator would make a rejecting filter postpone fuel
    /// admission and cancellation until the next visible row (or exhaustion).
    fn next_with_budget(
        &mut self,
        budget: &mut ProgramBudget<'_>,
    ) -> Result<Option<crate::Datom>, SemanticError> {
        let mut poll = |_: Option<&crate::Datom>| {
            budget.check_cancel()?;
            budget.charge(1)?;
            Ok(true)
        };
        match self {
            Self::Eager(cursor) => {
                poll(None)?;
                let datom = cursor.next();
                if let Some(datom) = &datom {
                    poll(Some(datom))?;
                }
                Ok(datom)
            }
            Self::Exact(cursor) => cursor.next_with_control(&mut poll).transpose(),
        }
    }
}

impl<'a> ProgramRead<'a> {
    fn entid(self, ident: &Keyword) -> Option<u64> {
        match self {
            Self::AttributePredicate => {
                unreachable!("validated attribute predicate attempted a database read")
            }
            Self::Eager(database) => database.entid(ident),
            Self::Exact(database) => database.entid(ident),
        }
    }

    fn lookup(
        self,
        attribute: u32,
        value: &Value,
        budget: &mut ProgramBudget<'_>,
    ) -> Result<Option<u64>, SemanticError> {
        budget.check_cancel()?;
        budget.charge(1)?;
        match self {
            Self::AttributePredicate => {
                unreachable!("validated attribute predicate attempted a database read")
            }
            Self::Eager(database) => database.lookup(attribute, value),
            Self::Exact(database) => database.lookup_with_control(attribute, value, &mut |_| {
                budget.check_cancel()?;
                budget.charge(1)?;
                Ok(true)
            }),
        }
    }

    fn lookup_input(
        self,
        attribute: u32,
        value: &TxValue,
        budget: &mut ProgramBudget<'_>,
    ) -> Result<Option<u64>, SemanticError> {
        match self {
            Self::AttributePredicate => {
                unreachable!("validated attribute predicate attempted a database read")
            }
            Self::Eager(database) => database.database_value().resolve_lookup_input_with(
                attribute,
                value,
                &mut |attribute, value| self.lookup(attribute, value, budget),
            ),
            Self::Exact(database) => {
                database.resolve_lookup_input_with(attribute, value, &mut |attribute, value| {
                    self.lookup(attribute, value, budget)
                })
            }
        }
    }

    fn prefix_cursor(self, prefix: &IndexPrefix) -> Result<ProgramPrefixCursor<'a>, SemanticError> {
        match self {
            Self::AttributePredicate => {
                unreachable!("validated attribute predicate attempted a database read")
            }
            Self::Eager(database) => Ok(ProgramPrefixCursor::Eager(
                database.datoms_with_prefix(prefix)?.iter().cloned(),
            )),
            Self::Exact(database) => Ok(ProgramPrefixCursor::Exact(Box::new(
                database.query_prefix_cursor(prefix)?,
            ))),
        }
    }
}

impl Program {
    pub fn validate(&self) -> Result<(), SemanticError> {
        if self.arity > MAX_ARITY {
            return Err(incorrect(
                "program/arity-limit",
                format!("program arity exceeds {MAX_ARITY}"),
            ));
        }
        if self.instructions.is_empty() {
            return Err(incorrect(
                "program/instruction-limit",
                format!("program must contain 1 to {MAX_INSTRUCTIONS} instructions"),
            ));
        }
        if !matches!(self.instructions.last(), Some(Instruction::Return))
            || self.instructions[..self.instructions.len() - 1]
                .iter()
                .any(|instruction| matches!(instruction, Instruction::Return))
        {
            return Err(incorrect(
                "program/noncanonical-return",
                "program must contain exactly one final return instruction",
            ));
        }

        if self.kind == ProgramKind::DualPredicate {
            return self.validate_dual_predicate();
        }

        let mut validation = Validation {
            kind: self.kind,
            arity: self.arity,
            instruction_count: 0,
            emits_rows: false,
        };
        let depth = validation.block(&self.instructions, 0, 0, true)?;
        let valid_result = match self.kind {
            ProgramKind::Transaction => depth == 0,
            ProgramKind::AttributePredicate | ProgramKind::EntityPredicate => {
                self.arity == 1 && depth == 1
            }
            ProgramKind::Query => depth > 0 || validation.emits_rows,
            ProgramKind::DualPredicate => unreachable!("validated by the role-specific path"),
        };
        if !valid_result {
            return Err(incorrect(
                "program/result-shape",
                "final stack does not match the program kind",
            ));
        }
        Ok(())
    }

    fn validate_dual_predicate(&self) -> Result<(), SemanticError> {
        if self.arity != 1 {
            return Err(incorrect(
                "program/result-shape",
                "dual predicates must have one explicit argument in each role",
            ));
        }
        let [
            Instruction::PredicateDispatch { attribute, entity },
            Instruction::Return,
        ] = self.instructions.as_slice()
        else {
            return Err(incorrect(
                "program/dual-predicate-shape",
                "dual predicates must contain exactly one predicate dispatch and a final return",
            ));
        };

        // Count the dispatch and final return once, then both bodies. Sharing
        // one counter prevents a dual program from doubling every structural
        // resource ceiling merely by splitting code across roles.
        let mut attribute_validation = Validation {
            kind: ProgramKind::AttributePredicate,
            arity: 1,
            instruction_count: 2,
            emits_rows: false,
        };
        if attribute_validation.block(attribute, 0, 0, false)? != 1 {
            return Err(incorrect(
                "program/result-shape",
                "dual predicate attribute body must leave exactly one result",
            ));
        }
        let mut entity_validation = Validation {
            kind: ProgramKind::EntityPredicate,
            arity: 1,
            instruction_count: attribute_validation.instruction_count,
            emits_rows: false,
        };
        if entity_validation.block(entity, 0, 0, false)? != 1 {
            return Err(incorrect(
                "program/result-shape",
                "dual predicate entity body must leave exactly one result",
            ));
        }
        Ok(())
    }

    pub(crate) fn supports_attribute_predicate(&self) -> bool {
        matches!(
            self.kind,
            ProgramKind::AttributePredicate | ProgramKind::DualPredicate
        )
    }

    pub(crate) fn supports_entity_predicate(&self) -> bool {
        matches!(
            self.kind,
            ProgramKind::EntityPredicate | ProgramKind::DualPredicate
        )
    }

    fn predicate_body(
        &self,
        role: PredicateExecutionRole,
    ) -> Result<&[Instruction], SemanticError> {
        match (self.kind, role) {
            (ProgramKind::AttributePredicate, PredicateExecutionRole::Attribute)
            | (ProgramKind::EntityPredicate, PredicateExecutionRole::Entity) => {
                Ok(&self.instructions)
            }
            (ProgramKind::DualPredicate, role) => {
                let Some(Instruction::PredicateDispatch { attribute, entity }) =
                    self.instructions.first()
                else {
                    return Err(incorrect(
                        "program/dual-predicate-shape",
                        "validated dual predicate has no dispatch",
                    ));
                };
                Ok(match role {
                    PredicateExecutionRole::Attribute => attribute,
                    PredicateExecutionRole::Entity => entity,
                })
            }
            (_, PredicateExecutionRole::Attribute) => Err(incorrect(
                "program/not-attribute-predicate",
                "program does not declare an attribute-predicate body",
            )),
            (_, PredicateExecutionRole::Entity) => Err(incorrect(
                "program/not-entity-predicate",
                "program does not declare an entity-predicate body",
            )),
        }
    }
}

#[derive(Clone, Copy)]
enum PredicateExecutionRole {
    Attribute,
    Entity,
}

struct Validation {
    kind: ProgramKind,
    arity: u8,
    instruction_count: usize,
    emits_rows: bool,
}

impl Validation {
    fn block(
        &mut self,
        instructions: &[Instruction],
        mut stack_depth: isize,
        block_depth: usize,
        top_level: bool,
    ) -> Result<isize, SemanticError> {
        if block_depth > MAX_BLOCK_DEPTH {
            return Err(incorrect(
                "program/block-depth",
                format!("program blocks may nest at most {MAX_BLOCK_DEPTH} levels"),
            ));
        }
        for (offset, instruction) in instructions.iter().enumerate() {
            self.instruction_count = self.instruction_count.checked_add(1).ok_or_else(|| {
                incorrect(
                    "program/instruction-limit",
                    "program instruction count overflowed",
                )
            })?;
            if self.instruction_count > MAX_INSTRUCTIONS {
                return Err(incorrect(
                    "program/instruction-limit",
                    format!("program may contain at most {MAX_INSTRUCTIONS} instructions"),
                ));
            }
            if matches!(instruction, Instruction::Return)
                && (!top_level || offset + 1 != instructions.len())
            {
                return Err(incorrect(
                    "program/noncanonical-return",
                    "return is permitted only as the final top-level instruction",
                ));
            }
            self.validate_instruction(instruction)?;
            match instruction {
                Instruction::If {
                    then_branch,
                    else_branch,
                } => {
                    require_stack(stack_depth, 1)?;
                    let input_depth = stack_depth - 1;
                    let then_depth =
                        self.block(then_branch, input_depth, block_depth + 1, false)?;
                    let else_depth =
                        self.block(else_branch, input_depth, block_depth + 1, false)?;
                    if then_depth != else_depth {
                        return Err(incorrect(
                            "program/branch-stack-shape",
                            "both branches must leave the same stack depth",
                        ));
                    }
                    stack_depth = then_depth;
                }
                Instruction::ForEach { body } => {
                    require_stack(stack_depth, 1)?;
                    let body_depth = self.block(body, 1, block_depth + 1, false)?;
                    if body_depth != 0 {
                        return Err(incorrect(
                            "program/foreach-stack-shape",
                            "for-each bodies must consume their current item",
                        ));
                    }
                    stack_depth -= 1;
                }
                _ => {
                    let (needed, delta) = stack_effect(instruction);
                    require_stack(stack_depth, needed)?;
                    stack_depth += delta;
                }
            }
        }
        Ok(stack_depth)
    }

    fn validate_instruction(&mut self, instruction: &Instruction) -> Result<(), SemanticError> {
        if matches!(instruction, Instruction::PredicateDispatch { .. }) {
            return Err(incorrect(
                "program/dual-predicate-shape",
                "predicate dispatch is permitted only at the top level of a dual predicate",
            ));
        }
        if let Instruction::PushArgument(index) = instruction
            && *index >= self.arity
        {
            return Err(incorrect(
                "program/argument-index",
                format!("argument {index} is outside arity {}", self.arity),
            ));
        }
        // New recursive reference literals are checked before execution can
        // clone them, including literals in an untaken branch. Arguments are
        // checked separately by the shared runtime budget.
        match instruction {
            Instruction::PushEntity(
                entity @ (EntityRef::LookupInput { .. } | EntityRef::Edn(_)),
            )
            | Instruction::EmitCall {
                function:
                    CallableRef::Database(entity @ (EntityRef::LookupInput { .. } | EntityRef::Edn(_))),
                ..
            } => {
                entity_ref_bytes(entity)?;
            }
            _ => {}
        }
        if self.kind == ProgramKind::AttributePredicate
            && matches!(
                instruction,
                Instruction::LoadOne(_)
                    | Instruction::LoadMany(_)
                    | Instruction::Exists(_)
                    | Instruction::Query(_)
            )
        {
            return Err(incorrect(
                "program/predicate-database-read",
                "attribute predicates may inspect only their value argument",
            ));
        }
        if self.kind != ProgramKind::Transaction
            && matches!(
                instruction,
                Instruction::EmitAdd(_)
                    | Instruction::EmitRetract(_)
                    | Instruction::EmitRetractAll(_)
                    | Instruction::EmitCas(_)
                    | Instruction::EmitRetractEntity
                    | Instruction::EmitEnsure
                    | Instruction::EmitForcePartition
                    | Instruction::EmitMatchPartition
                    | Instruction::EmitEntityMap
                    | Instruction::EmitCall { .. }
            )
        {
            return Err(incorrect(
                "program/output-kind",
                "only transaction programs may emit transaction forms",
            ));
        }
        if matches!(instruction, Instruction::EmitRow(0)) {
            return Err(incorrect(
                "program/empty-row",
                "query rows must contain at least one value",
            ));
        }
        if self.kind != ProgramKind::Query && matches!(instruction, Instruction::EmitRow(_)) {
            return Err(incorrect(
                "program/output-kind",
                "only query programs may emit result rows",
            ));
        }
        if let Instruction::Require { category, .. } = instruction {
            allowed_rejection_category(*category)?;
        }
        if let Instruction::EmitCall { argument_count, .. } = instruction
            && *argument_count > MAX_ARITY
        {
            return Err(incorrect(
                "program/call-arity-limit",
                format!("nested call arity exceeds {MAX_ARITY}"),
            ));
        }
        if let Instruction::Query(template) = instruction {
            template.validate(self.arity)?;
        }
        self.emits_rows |= matches!(instruction, Instruction::EmitRow(_));
        Ok(())
    }
}

fn require_stack(actual: isize, needed: isize) -> Result<(), SemanticError> {
    if actual < needed {
        Err(incorrect(
            "program/static-stack-underflow",
            "instruction consumes more values than are available",
        ))
    } else {
        Ok(())
    }
}

impl ProgramRuntime {
    pub fn execute_query_general(
        &self,
        program: &Program,
        database: &DatabaseValue,
        arguments: &[crate::QueryValue],
        control: ProgramControl<'_>,
    ) -> Result<ProgramOutput, SemanticError> {
        self.execute_query_general_with_budget(
            program,
            database,
            arguments,
            &mut ProgramBudget::new(control)?,
        )
    }

    /// General query data with the same exact database and shared accounting
    /// as the scalar-input query entry point.
    pub fn execute_query_general_with_budget(
        &self,
        program: &Program,
        database: &DatabaseValue,
        arguments: &[crate::QueryValue],
        budget: &mut ProgramBudget<'_>,
    ) -> Result<ProgramOutput, SemanticError> {
        budget.check_cancel()?;
        if program.kind != ProgramKind::Query {
            return Err(incorrect(
                "program/not-query-program",
                "general query execution accepts only query programs",
            ));
        }
        // Validate borrowed trees before cloning caller-owned data.
        let mut retained = 0usize;
        for argument in arguments {
            retained = checked_size_add(retained, budget.check_query_data(argument)?)?;
            if retained > budget.max_value_bytes.saturating_sub(budget.value_bytes) {
                return Err(busy(
                    "program/value-byte-limit",
                    "general query arguments exceed remaining runtime retention",
                ));
            }
        }
        let arguments = arguments
            .iter()
            .map(|value| match value {
                crate::QueryValue::Scalar(value) => RuntimeValue::Scalar(value.clone()),
                crate::QueryValue::Nil => RuntimeValue::Null,
                _ => RuntimeValue::Query(value.clone()),
            })
            .collect::<Vec<_>>();
        contain_runtime_panic(|| {
            program.validate()?;
            self.execute_validated_runtime_with_budget_inner(
                program,
                ProgramRead::Exact(database),
                &arguments,
                budget,
            )
        })
    }
    pub fn execute(
        &self,
        program: &Program,
        database: &Database,
        arguments: &[Value],
        control: ProgramControl<'_>,
    ) -> Result<ProgramOutput, SemanticError> {
        let arguments = arguments
            .iter()
            .cloned()
            .map(RuntimeValue::Scalar)
            .collect::<Vec<_>>();
        self.execute_runtime(program, database, &arguments, control)
    }

    /// Execute a persisted query program against one exact immutable database
    /// value. Every database instruction observes that value's basis,
    /// temporal window, history flag, custom predicates, schema, and idents;
    /// native peer values remain lazy and are never converted to `Database`.
    pub fn execute_query(
        &self,
        program: &Program,
        database: &DatabaseValue,
        arguments: &[Value],
        control: ProgramControl<'_>,
    ) -> Result<ProgramOutput, SemanticError> {
        let mut budget = ProgramBudget::new(control)?;
        self.execute_query_with_budget(program, database, arguments, &mut budget)
    }

    /// Exact-value query execution with caller-owned accounting. Reuse this
    /// budget across invocations, or charge its consumed fuel back to the
    /// enclosing query before constructing a subsequent invocation's budget.
    pub fn execute_query_with_budget(
        &self,
        program: &Program,
        database: &DatabaseValue,
        arguments: &[Value],
        budget: &mut ProgramBudget<'_>,
    ) -> Result<ProgramOutput, SemanticError> {
        budget.check_cancel()?;
        if program.kind != ProgramKind::Query {
            return Err(incorrect(
                "program/not-query-program",
                "exact database-value execution accepts only query programs",
            ));
        }
        let arguments = arguments
            .iter()
            .cloned()
            .map(RuntimeValue::Scalar)
            .collect::<Vec<_>>();
        contain_runtime_panic(|| {
            program.validate()?;
            budget.check_cancel()?;
            self.execute_validated_runtime_with_budget_inner(
                program,
                ProgramRead::Exact(database),
                &arguments,
                budget,
            )
        })
    }

    pub fn execute_runtime(
        &self,
        program: &Program,
        database: &Database,
        arguments: &[RuntimeValue],
        control: ProgramControl<'_>,
    ) -> Result<ProgramOutput, SemanticError> {
        let mut budget = ProgramBudget::new(control)?;
        self.execute_runtime_with_budget(program, database, arguments, &mut budget)
    }

    /// Execute against accounting shared by the whole transaction attempt.
    /// Reuse the same budget for initial, sibling, nested, and predicate
    /// invocations; `execute` is deliberately only a one-shot convenience.
    pub fn execute_with_budget(
        &self,
        program: &Program,
        database: &Database,
        arguments: &[Value],
        budget: &mut ProgramBudget<'_>,
    ) -> Result<ProgramOutput, SemanticError> {
        let arguments = arguments
            .iter()
            .cloned()
            .map(RuntimeValue::Scalar)
            .collect::<Vec<_>>();
        self.execute_runtime_with_budget(program, database, &arguments, budget)
    }

    pub fn execute_runtime_with_budget(
        &self,
        program: &Program,
        database: &Database,
        arguments: &[RuntimeValue],
        budget: &mut ProgramBudget<'_>,
    ) -> Result<ProgramOutput, SemanticError> {
        contain_runtime_panic(|| {
            program.validate()?;
            self.execute_validated_runtime_with_budget_inner(
                program,
                ProgramRead::Eager(database),
                arguments,
                budget,
            )
        })
    }

    /// Invoke an attribute predicate with only its documented value argument.
    /// No db-before or db-after capability exists on this execution path.
    pub(crate) fn execute_prevalidated_attribute_predicate_with_budget(
        &self,
        program: &ValidatedProgram,
        value: &Value,
        budget: &mut ProgramBudget<'_>,
    ) -> Result<RuntimeValue, SemanticError> {
        contain_runtime_panic(|| {
            let body = program
                .program()
                .predicate_body(PredicateExecutionRole::Attribute)?;
            self.execute_predicate_body_with_budget(
                body,
                ProgramRead::AttributePredicate,
                &[RuntimeValue::Scalar(value.clone())],
                budget,
            )
        })
    }

    /// Invoke an entity predicate with the exact proposed db-after. The
    /// program identity itself remains selected from db-before by the caller.
    pub(crate) fn execute_prevalidated_entity_predicate_exact_with_budget(
        &self,
        program: &ValidatedProgram,
        db_after: &DatabaseValue,
        entity: u64,
        budget: &mut ProgramBudget<'_>,
    ) -> Result<RuntimeValue, SemanticError> {
        contain_runtime_panic(|| {
            let body = program
                .program()
                .predicate_body(PredicateExecutionRole::Entity)?;
            self.execute_predicate_body_with_budget(
                body,
                ProgramRead::Exact(db_after),
                &[RuntimeValue::Scalar(Value::Ref(entity))],
                budget,
            )
        })
    }

    pub(crate) fn execute_prevalidated_runtime_exact_with_budget(
        &self,
        program: &ValidatedProgram,
        database: &DatabaseValue,
        arguments: &[RuntimeValue],
        budget: &mut ProgramBudget<'_>,
    ) -> Result<ProgramOutput, SemanticError> {
        contain_runtime_panic(|| {
            self.execute_validated_runtime_with_budget_inner(
                program.program(),
                ProgramRead::Exact(database),
                arguments,
                budget,
            )
        })
    }

    fn execute_validated_runtime_with_budget_inner(
        &self,
        program: &Program,
        database: ProgramRead<'_>,
        arguments: &[RuntimeValue],
        budget: &mut ProgramBudget<'_>,
    ) -> Result<ProgramOutput, SemanticError> {
        if arguments.len() != usize::from(program.arity) {
            return Err(incorrect(
                "program/arity",
                format!(
                    "program expects {} arguments, received {}",
                    program.arity,
                    arguments.len()
                ),
            ));
        }
        budget.begin_call()?;
        // Account for the submitted values even when the program never reads
        // them. Otherwise an unused large collection could bypass the shared
        // heap and collection ceilings after already being decoded.
        for argument in arguments {
            budget.charge_runtime_value(argument)?;
        }
        let mut stack = Vec::new();
        let mut evaluation = Evaluation {
            database,
            arguments,
            budget,
            forms: Vec::new(),
            query_rows: Vec::new(),
        };
        evaluation.block(&program.instructions, &mut stack, 0)?;

        match program.kind {
            ProgramKind::Transaction => Ok(ProgramOutput::Transaction(evaluation.forms)),
            ProgramKind::AttributePredicate => {
                Ok(ProgramOutput::AttributePredicate(pop(&mut stack)?))
            }
            ProgramKind::EntityPredicate => Ok(ProgramOutput::EntityPredicate(pop(&mut stack)?)),
            ProgramKind::Query => {
                if !stack.is_empty() {
                    evaluation.emit_row(stack)?;
                }
                if evaluation
                    .query_rows
                    .iter()
                    .flatten()
                    .all(|value| matches!(value, crate::QueryValue::Scalar(_)))
                {
                    Ok(ProgramOutput::Query(
                        evaluation
                            .query_rows
                            .into_iter()
                            .map(|row| {
                                row.into_iter()
                                    .map(|value| value.into_scalar().expect("checked scalar"))
                                    .collect()
                            })
                            .collect(),
                    ))
                } else {
                    Ok(ProgramOutput::GeneralQuery(evaluation.query_rows))
                }
            }
            ProgramKind::DualPredicate => Err(incorrect(
                "program/predicate-role-required",
                "dual predicates require an explicit attribute or entity invocation role",
            )),
        }
    }

    fn execute_predicate_body_with_budget(
        &self,
        body: &[Instruction],
        database: ProgramRead<'_>,
        arguments: &[RuntimeValue],
        budget: &mut ProgramBudget<'_>,
    ) -> Result<RuntimeValue, SemanticError> {
        budget.begin_call()?;
        for argument in arguments {
            budget.charge_runtime_value(argument)?;
        }
        let mut stack = Vec::new();
        let mut evaluation = Evaluation {
            database,
            arguments,
            budget,
            forms: Vec::new(),
            query_rows: Vec::new(),
        };
        evaluation.block(body, &mut stack, 0)?;
        pop(&mut stack)
    }
}

fn contain_runtime_panic<T>(
    evaluate: impl FnOnce() -> Result<T, SemanticError>,
) -> Result<T, SemanticError> {
    catch_unwind(AssertUnwindSafe(evaluate)).map_err(|_| {
        SemanticError::new(
            ErrorCategory::Fault,
            "program/runtime-panicked",
            "persisted program runtime panicked",
        )
    })?
}

struct Evaluation<'db, 'args, 'budget, 'control> {
    database: ProgramRead<'db>,
    arguments: &'args [RuntimeValue],
    budget: &'budget mut ProgramBudget<'control>,
    forms: Vec<TxForm>,
    query_rows: Vec<Vec<crate::QueryValue>>,
}

impl Evaluation<'_, '_, '_, '_> {
    fn block(
        &mut self,
        instructions: &[Instruction],
        stack: &mut Vec<RuntimeValue>,
        block_depth: usize,
    ) -> Result<(), SemanticError> {
        if block_depth > MAX_BLOCK_DEPTH {
            return Err(busy(
                "program/block-depth",
                "program exceeded its block-depth limit",
            ));
        }
        for instruction in instructions {
            self.budget.check_cancel()?;
            self.budget.charge(1)?;
            match instruction {
                Instruction::PushArgument(index) => {
                    let value = &self.arguments[usize::from(*index)];
                    self.push(stack, value.clone())?;
                }
                Instruction::PushConstant(value) => {
                    self.push(stack, RuntimeValue::Scalar(value.clone()))?;
                }
                Instruction::PushEntity(entity) => {
                    self.push(stack, RuntimeValue::Entity(entity.clone()))?;
                }
                Instruction::PushNull => self.push(stack, RuntimeValue::Null)?,
                Instruction::Duplicate => {
                    let value = top(stack)?.clone();
                    self.push(stack, value)?;
                }
                Instruction::Pop => {
                    pop(stack)?;
                }
                Instruction::Swap => {
                    if stack.len() < 2 {
                        return Err(incorrect(
                            "program/stack-underflow",
                            "swap requires two stack values",
                        ));
                    }
                    let end = stack.len();
                    stack.swap(end - 1, end - 2);
                }
                Instruction::MakeVector(width) => {
                    let values = split_stack(stack, usize::from(*width), "vector")?;
                    self.push(stack, RuntimeValue::Vector(values))?;
                }
                Instruction::MakeMap(pair_count) => {
                    let values = split_stack(stack, usize::from(*pair_count) * 2, "map entries")?;
                    let mut entries = Vec::with_capacity(usize::from(*pair_count));
                    let mut values = values.into_iter();
                    while let Some(key) = values.next() {
                        let value = values.next().expect("validated even map entry count");
                        entries.push((scalar(key)?, value));
                    }
                    self.push(stack, RuntimeValue::map(entries)?)?;
                }
                Instruction::Get => {
                    let key = pop(stack)?;
                    let collection = pop(stack)?;
                    let value = runtime_get(collection, key, self.budget)?;
                    self.push(stack, value)?;
                }
                Instruction::ContainsKey => {
                    let key = pop(stack)?;
                    let map = pop(stack)?;
                    let exists = match map {
                        RuntimeValue::Map(entries) => {
                            let key = scalar(key)?;
                            entries.iter().any(|entry| entry.0.index_cmp(&key).is_eq())
                        }
                        RuntimeValue::Query(value) => {
                            let crate::QueryValue::Map(entries) = &value else {
                                return Err(incorrect(
                                    "program/type",
                                    "contains-key requires a map",
                                ));
                            };
                            let key = native_query::runtime_query_value(&key)?;
                            let mut found = false;
                            for (candidate, _) in entries {
                                if candidate
                                    .compare_with(&key, &mut |work| {
                                        self.budget.charge(usize_as_u64(work)?)
                                    })?
                                    .is_eq()
                                {
                                    found = true;
                                    break;
                                }
                            }
                            found
                        }
                        _ => return Err(incorrect("program/type", "contains-key requires a map")),
                    };
                    self.push(stack, RuntimeValue::Scalar(Value::Bool(exists)))?;
                }
                Instruction::Length => {
                    let length = match pop(stack)? {
                        RuntimeValue::Query(value) => match &value {
                            crate::QueryValue::Tuple(values)
                            | crate::QueryValue::Collection(values) => values.len(),
                            crate::QueryValue::Set(_) => {
                                finite_items(RuntimeValue::Query(value), self.budget)?.len()
                            }
                            crate::QueryValue::Map(entries) => entries.len(),
                            _ => {
                                return Err(incorrect(
                                    "program/type",
                                    "length requires a query collection",
                                ));
                            }
                        },
                        RuntimeValue::Vector(values) => values.len(),
                        RuntimeValue::Map(entries) => entries.len(),
                        // Clojure delegates string `count` to Java's String
                        // length, whose observable unit is UTF-16 code units.
                        RuntimeValue::Scalar(Value::String(value)) => value.encode_utf16().count(),
                        _ => {
                            return Err(incorrect(
                                "program/type",
                                "length requires a string, runtime vector, or map",
                            ));
                        }
                    };
                    self.push(
                        stack,
                        RuntimeValue::Scalar(Value::Long(i64::try_from(length).map_err(|_| {
                            busy("program/resource-limit", "collection length exceeds i64")
                        })?)),
                    )?;
                }
                Instruction::Unpack(width) => {
                    let values = match pop(stack)? {
                        RuntimeValue::Vector(values) => values,
                        RuntimeValue::Query(mut value) => {
                            let (crate::QueryValue::Tuple(values)
                            | crate::QueryValue::Collection(values)) = &mut value
                            else {
                                return Err(incorrect(
                                    "program/type",
                                    "unpack requires a sequence",
                                ));
                            };
                            std::mem::take(values)
                                .into_iter()
                                .map(RuntimeValue::from_query_value)
                                .collect()
                        }
                        _ => return Err(incorrect("program/type", "unpack requires a sequence")),
                    };
                    if values.len() != usize::from(*width) {
                        return Err(incorrect(
                            "program/unpack-width",
                            format!(
                                "unpack expected {} values, received {}",
                                width,
                                values.len()
                            ),
                        ));
                    }
                    stack.extend(values);
                }
                Instruction::If {
                    then_branch,
                    else_branch,
                } => {
                    let condition = boolean(pop(stack)?)?;
                    self.block(
                        if condition { then_branch } else { else_branch },
                        stack,
                        block_depth + 1,
                    )?;
                }
                Instruction::PredicateDispatch { .. } => {
                    return Err(incorrect(
                        "program/predicate-role-required",
                        "predicate dispatch requires an explicit invocation role",
                    ));
                }
                Instruction::ForEach { body } => {
                    let items = finite_items(pop(stack)?, self.budget)?;
                    if items.len() > self.budget.max_collection_items {
                        return Err(busy(
                            "program/collection-limit",
                            "for-each collection exceeds its item limit",
                        ));
                    }
                    for item in items {
                        self.budget.check_cancel()?;
                        self.budget.charge(1)?;
                        let mut item_stack = Vec::with_capacity(1);
                        self.push(&mut item_stack, item)?;
                        self.block(body, &mut item_stack, block_depth + 1)?;
                        if !item_stack.is_empty() {
                            return Err(incorrect(
                                "program/foreach-stack-shape",
                                "for-each body did not consume its current item",
                            ));
                        }
                    }
                }
                Instruction::LoadOne(attribute) => {
                    let entity = database_entity_id(self.database, pop(stack)?, self.budget)?;
                    let mut datoms = self.database.prefix_cursor(&IndexPrefix::Eavt {
                        entity,
                        attribute: Some(*attribute),
                        value: None,
                    })?;
                    let first = datoms.next_with_budget(self.budget)?;
                    if first.is_some() {
                        self.budget.charge(1)?;
                    }
                    match first {
                        Some(datom) => {
                            while let Some(contribution) = datoms.next_with_budget(self.budget)? {
                                self.budget.charge(1)?;
                                if !datom.value.stored_eq(&contribution.value) {
                                    return Err(incorrect(
                                        "program/not-cardinality-one",
                                        format!("attribute {attribute} has multiple values"),
                                    ));
                                }
                            }
                            self.push(stack, RuntimeValue::Scalar(datom.value))?;
                        }
                        None => {
                            return Err(incorrect(
                                "program/missing-value",
                                format!("entity {entity} has no value for attribute {attribute}"),
                            ));
                        }
                    }
                }
                Instruction::LoadMany(attribute) => {
                    let entity = database_entity_id(self.database, pop(stack)?, self.budget)?;
                    let mut datoms = self.database.prefix_cursor(&IndexPrefix::Eavt {
                        entity,
                        attribute: Some(*attribute),
                        value: None,
                    })?;
                    let mut values = Vec::new();
                    while let Some(datom) = datoms.next_with_budget(self.budget)? {
                        self.budget.charge(1)?;
                        // EAVT groups equal values before their source
                        // contributions. Count values, while admitting every
                        // candidate through the ordinary controlled cursor.
                        if values
                            .iter()
                            .rev()
                            .take_while(|previous| {
                                matches!(previous, RuntimeValue::Scalar(value)
                                if value.index_cmp(&datom.value).is_eq())
                            })
                            .any(|previous| {
                                matches!(previous, RuntimeValue::Scalar(value)
                                if value.stored_eq(&datom.value))
                            })
                        {
                            continue;
                        }
                        if values.len() >= self.budget.max_collection_items {
                            return Err(busy(
                                "program/collection-limit",
                                "cardinality-many read exceeds its item limit",
                            ));
                        }
                        values.push(RuntimeValue::Scalar(datom.value));
                    }
                    self.push(stack, RuntimeValue::Vector(values))?;
                }
                Instruction::Exists(attribute) => {
                    let entity = database_entity_id(self.database, pop(stack)?, self.budget)?;
                    let mut datoms = self.database.prefix_cursor(&IndexPrefix::Eavt {
                        entity,
                        attribute: Some(*attribute),
                        value: None,
                    })?;
                    let exists = datoms.next_with_budget(self.budget)?.is_some();
                    if exists {
                        self.budget.charge(1)?;
                    }
                    self.push(stack, RuntimeValue::Scalar(Value::Bool(exists)))?;
                }
                Instruction::Query(template) => {
                    let relation = execute_query_template(
                        self.database,
                        self.arguments,
                        self.budget,
                        template,
                    )?;
                    self.push(stack, relation)?;
                }
                Instruction::Add => {
                    runtime_binary_long(stack, "add", i64::checked_add)?;
                    self.charge_top(stack)?;
                }
                Instruction::Subtract => {
                    runtime_binary_long(stack, "subtract", i64::checked_sub)?;
                    self.charge_top(stack)?;
                }
                Instruction::Multiply => {
                    runtime_binary_long(stack, "multiply", i64::checked_mul)?;
                    self.charge_top(stack)?;
                }
                Instruction::Divide => {
                    let right = long(pop(stack)?)?;
                    let left = long(pop(stack)?)?;
                    let result = left.checked_div(right).ok_or_else(|| {
                        incorrect("program/arithmetic", "division by zero or integer overflow")
                    })?;
                    self.push(stack, RuntimeValue::Scalar(Value::Long(result)))?;
                }
                Instruction::Equal => {
                    charge_top_runtime_values(self.budget, stack, 2)?;
                    let right = pop(stack)?;
                    let left = pop(stack)?;
                    self.push(stack, RuntimeValue::Scalar(Value::Bool(left == right)))?;
                }
                Instruction::LessThan => {
                    charge_top_runtime_values(self.budget, stack, 2)?;
                    runtime_compare(stack, Ordering::Less)?;
                    self.charge_top(stack)?;
                }
                Instruction::GreaterThan => {
                    charge_top_runtime_values(self.budget, stack, 2)?;
                    runtime_compare(stack, Ordering::Greater)?;
                    self.charge_top(stack)?;
                }
                Instruction::Not => {
                    let value = boolean(pop(stack)?)?;
                    self.push(stack, RuntimeValue::Scalar(Value::Bool(!value)))?;
                }
                Instruction::And => {
                    runtime_binary_bool(stack, |left, right| left && right)?;
                    self.charge_top(stack)?;
                }
                Instruction::Or => {
                    runtime_binary_bool(stack, |left, right| left || right)?;
                    self.charge_top(stack)?;
                }
                Instruction::Require { category, message } => {
                    if !boolean(pop(stack)?)? {
                        return Err(SemanticError::new(
                            allowed_rejection_category(*category)?,
                            "program/rejected",
                            message.clone(),
                        ));
                    }
                }
                Instruction::RequireAnomaly => {
                    let anomaly = pop(stack)?;
                    if !matches!(anomaly, RuntimeValue::Map(_)) {
                        return Err(incorrect(
                            "program/cancel-anomaly",
                            "cancel anomaly must be a runtime map",
                        ));
                    }
                    if !boolean(pop(stack)?)? {
                        return Err(cancel_anomaly(anomaly)?);
                    }
                }
                Instruction::EmitAdd(attribute) => {
                    let value = tx_value(pop(stack)?)?;
                    let entity = entity_ref(pop(stack)?)?;
                    self.emit_op(TxOp::Add {
                        entity,
                        attribute: *attribute,
                        value,
                    })?;
                }
                Instruction::EmitRetract(attribute) => {
                    let value = tx_value(pop(stack)?)?;
                    let entity = entity_ref(pop(stack)?)?;
                    self.emit_op(TxOp::Retract {
                        entity,
                        attribute: *attribute,
                        value: Some(value),
                    })?;
                }
                Instruction::EmitRetractAll(attribute) => {
                    let entity = entity_ref(pop(stack)?)?;
                    self.emit_op(TxOp::Retract {
                        entity,
                        attribute: *attribute,
                        value: None,
                    })?;
                }
                Instruction::EmitCas(attribute) => {
                    let new = tx_value(pop(stack)?)?;
                    let old = match pop(stack)? {
                        RuntimeValue::Null => None,
                        value => Some(tx_value(value)?),
                    };
                    let entity = entity_ref(pop(stack)?)?;
                    self.emit_op(TxOp::Cas {
                        entity,
                        attribute: *attribute,
                        old,
                        new,
                    })?;
                }
                Instruction::EmitRetractEntity => {
                    let entity = entity_ref(pop(stack)?)?;
                    self.emit_op(TxOp::RetractEntity(entity))?;
                }
                Instruction::EmitEnsure => {
                    let spec = entity_ref(pop(stack)?)?;
                    let entity = entity_ref(pop(stack)?)?;
                    self.emit_op(TxOp::Ensure { entity, spec })?;
                }
                Instruction::EmitForcePartition | Instruction::EmitMatchPartition => {
                    let target = entity_map_id(pop(stack)?)?;
                    let EntityRef::Temp(tempid) = entity_map_id(pop(stack)?)? else {
                        return Err(incorrect(
                            "program/partition-tempid",
                            "partition policy must target a tempid",
                        ));
                    };
                    self.emit_op(if matches!(instruction, Instruction::EmitForcePartition) {
                        TxOp::ForcePartition {
                            tempid,
                            partition: target,
                        }
                    } else {
                        TxOp::MatchPartition {
                            tempid,
                            entity: target,
                        }
                    })?;
                }
                Instruction::EmitEntityMap => {
                    let map = runtime_entity_map(pop(stack)?, 0)?;
                    let form = TxForm::EntityMap(map);
                    let bytes = crate::encoding::persistent_tx_form_bytes(&form)?;
                    self.budget.charge(usize_as_u64(bytes)?)?;
                    self.budget.reserve_form(bytes)?;
                    self.forms.push(form);
                }
                Instruction::EmitCall {
                    function,
                    argument_count,
                } => {
                    let arguments =
                        split_stack(stack, usize::from(*argument_count), "nested call arguments")?;
                    let emitted_bytes = call_output_bytes(function, &arguments)?;
                    self.budget.charge(usize_as_u64(emitted_bytes)?)?;
                    self.budget.reserve_form(emitted_bytes)?;
                    let mut function = function.clone();
                    if let CallableRef::Database(reference) = &mut function {
                        canonicalize_runtime_entity_ref(reference)?;
                    }
                    self.forms.push(TxForm::ProgramCall(ProgramCall {
                        function,
                        arguments,
                    }));
                }
                Instruction::EmitRow(width) => {
                    let values = split_stack(stack, usize::from(*width), "query row")?;
                    self.emit_row(values)?;
                }
                Instruction::Return => {}
            }
            if stack.len() > self.budget.max_stack {
                return Err(busy(
                    "program/stack-limit",
                    "program exceeded its stack limit",
                ));
            }
        }
        Ok(())
    }

    fn push(
        &mut self,
        stack: &mut Vec<RuntimeValue>,
        mut value: RuntimeValue,
    ) -> Result<(), SemanticError> {
        self.budget.charge_runtime_value(&value)?;
        // Canonical wire identity must also determine VM equality and inert
        // output. Keep references unresolved, but discard author spelling at
        // this common boundary for both literals and nested argument values.
        canonicalize_runtime_references(&mut value)?;
        stack.push(value);
        Ok(())
    }

    fn charge_top(&mut self, stack: &[RuntimeValue]) -> Result<(), SemanticError> {
        self.budget.charge_runtime_value(top(stack)?)
    }

    fn emit_op(&mut self, operation: TxOp) -> Result<(), SemanticError> {
        let bytes = tx_op_output_bytes(&operation)?;
        self.budget.charge(usize_as_u64(bytes)?)?;
        self.budget.reserve_form(bytes)?;
        self.forms.push(TxForm::Op(operation));
        Ok(())
    }

    fn emit_row(&mut self, values: Vec<RuntimeValue>) -> Result<(), SemanticError> {
        let mut bytes = 8usize;
        for value in &values {
            bytes = checked_size_add(
                bytes,
                match value {
                    RuntimeValue::Scalar(value) => encoded_value_bytes(value, 0)?,
                    RuntimeValue::Entity(EntityRef::Id(entity)) => {
                        encoded_value_bytes(&Value::Ref(*entity), 0)?
                    }
                    _ => runtime_value_bytes(value, 0)?,
                },
            )?;
        }
        self.budget.charge(usize_as_u64(bytes)?)?;
        self.budget.reserve_form(bytes)?;
        let values = values
            .iter()
            .map(native_query::runtime_query_value)
            .collect::<Result<Vec<_>, SemanticError>>()?;
        self.query_rows.push(values);
        Ok(())
    }
}

fn stack_effect(instruction: &Instruction) -> (isize, isize) {
    match instruction {
        Instruction::PushArgument(_)
        | Instruction::PushConstant(_)
        | Instruction::PushEntity(_)
        | Instruction::PushNull => (0, 1),
        Instruction::Duplicate => (1, 1),
        Instruction::Pop | Instruction::Require { .. } => (1, -1),
        Instruction::RequireAnomaly => (2, -2),
        Instruction::Swap => (2, 0),
        Instruction::MakeVector(width) => {
            let width = isize::from(*width);
            (width, 1 - width)
        }
        Instruction::MakeMap(pair_count) => {
            let width = isize::from(*pair_count) * 2;
            (width, 1 - width)
        }
        Instruction::Get | Instruction::ContainsKey => (2, -1),
        Instruction::Length
        | Instruction::LoadOne(_)
        | Instruction::LoadMany(_)
        | Instruction::Exists(_)
        | Instruction::Not => (1, 0),
        Instruction::Query(_) => (0, 1),
        Instruction::Unpack(width) => (1, isize::from(*width) - 1),
        Instruction::Add
        | Instruction::Subtract
        | Instruction::Multiply
        | Instruction::Divide
        | Instruction::Equal
        | Instruction::LessThan
        | Instruction::GreaterThan
        | Instruction::And
        | Instruction::Or => (2, -1),
        Instruction::EmitAdd(_)
        | Instruction::EmitRetract(_)
        | Instruction::EmitEnsure
        | Instruction::EmitForcePartition
        | Instruction::EmitMatchPartition => (2, -2),
        Instruction::EmitRetractAll(_) | Instruction::EmitEntityMap => (1, -1),
        Instruction::EmitCas(_) => (3, -3),
        Instruction::EmitRetractEntity => (1, -1),
        Instruction::EmitCall { argument_count, .. } => {
            let count = isize::from(*argument_count);
            (count, -count)
        }
        Instruction::EmitRow(width) => (isize::from(*width), -isize::from(*width)),
        Instruction::Return => (0, 0),
        Instruction::If { .. }
        | Instruction::PredicateDispatch { .. }
        | Instruction::ForEach { .. } => {
            unreachable!("structured instructions are validated separately")
        }
    }
}

fn canonicalize_runtime_references(value: &mut RuntimeValue) -> Result<(), SemanticError> {
    match value {
        RuntimeValue::Entity(reference) => canonicalize_runtime_entity_ref(reference)?,
        RuntimeValue::Vector(values) => {
            for value in values {
                canonicalize_runtime_references(value)?;
            }
        }
        RuntimeValue::Map(entries) => {
            for (_, value) in entries {
                canonicalize_runtime_references(value)?;
            }
        }
        RuntimeValue::Scalar(_) | RuntimeValue::Null | RuntimeValue::Query(_) => {}
    }
    Ok(())
}

fn canonicalize_runtime_entity_ref(reference: &mut EntityRef) -> Result<(), SemanticError> {
    match reference {
        EntityRef::Edn(text) => *text = crate::edn_transaction::canonical_entity_ref(text)?,
        EntityRef::LookupInput { value, .. } => canonicalize_runtime_tx_value(value)?,
        _ => {}
    }
    Ok(())
}

fn canonicalize_runtime_tx_value(value: &mut TxValue) -> Result<(), SemanticError> {
    match value {
        TxValue::Entity(reference) => canonicalize_runtime_entity_ref(reference)?,
        TxValue::Tuple(slots) => {
            for value in slots.iter_mut().flatten() {
                canonicalize_runtime_tx_value(value)?;
            }
        }
        TxValue::Scalar(_) => {}
    }
    Ok(())
}

fn pop(stack: &mut Vec<RuntimeValue>) -> Result<RuntimeValue, SemanticError> {
    stack
        .pop()
        .ok_or_else(|| incorrect("program/stack-underflow", "program stack is empty"))
}

fn top(stack: &[RuntimeValue]) -> Result<&RuntimeValue, SemanticError> {
    stack
        .last()
        .ok_or_else(|| incorrect("program/stack-underflow", "program stack is empty"))
}

fn scalar(value: RuntimeValue) -> Result<Value, SemanticError> {
    if let RuntimeValue::Scalar(value) = value {
        Ok(value)
    } else {
        Err(incorrect(
            "program/type",
            "operation requires a scalar database value",
        ))
    }
}

fn long(value: RuntimeValue) -> Result<i64, SemanticError> {
    if let RuntimeValue::Scalar(Value::Long(value)) = value {
        Ok(value)
    } else {
        Err(incorrect(
            "program/type",
            "integer arithmetic requires long values",
        ))
    }
}

fn boolean(value: RuntimeValue) -> Result<bool, SemanticError> {
    if let RuntimeValue::Scalar(Value::Bool(value)) = value {
        Ok(value)
    } else {
        Err(incorrect(
            "program/type",
            "boolean operation requires boolean values",
        ))
    }
}

fn entity_ref(value: RuntimeValue) -> Result<EntityRef, SemanticError> {
    match value {
        RuntimeValue::Entity(entity) => Ok(entity),
        RuntimeValue::Scalar(Value::Ref(entity)) => Ok(EntityRef::Id(entity)),
        _ => Err(incorrect(
            "program/type",
            "operation requires an entity reference",
        )),
    }
}

fn runtime_entity_map(value: RuntimeValue, depth: usize) -> Result<EntityMap, SemanticError> {
    if depth > 32 {
        return Err(incorrect(
            "program/entity-map-depth",
            "emitted entity maps may contain at most 32 nested map levels",
        ));
    }
    let RuntimeValue::Map(entries) = value else {
        return Err(incorrect(
            "program/entity-map-shape",
            "entity-map emission requires a runtime map",
        ));
    };

    let mut id = None;
    let mut attributes = Vec::with_capacity(entries.len());
    for (key, value) in entries {
        let Value::Keyword(keyword) = key else {
            return Err(incorrect(
                "program/entity-map-key",
                "entity-map keys must be qualified keywords",
            ));
        };
        if keyword.namespace.as_deref() == Some("db") && keyword.name == "id" {
            id = Some(entity_map_id(value)?);
            continue;
        }
        require_qualified_keyword(&keyword, "program/entity-map-key")?;
        let attribute = if let Some(name) = keyword.name.strip_prefix('_') {
            if name.is_empty() {
                return Err(incorrect(
                    "program/entity-map-key",
                    "a reverse entity-map keyword must name an attribute after '_'",
                ));
            }
            AttributeRef::ReverseIdent(Keyword {
                namespace: keyword.namespace,
                name: name.to_owned(),
            })
        } else {
            AttributeRef::Ident(keyword)
        };
        attributes.push((attribute, runtime_map_value(value, depth + 1, true)?));
    }
    Ok(EntityMap { id, attributes })
}

fn entity_map_id(value: RuntimeValue) -> Result<EntityRef, SemanticError> {
    match value {
        RuntimeValue::Entity(entity) => Ok(entity),
        RuntimeValue::Scalar(Value::Ref(entity)) => Ok(EntityRef::Id(entity)),
        RuntimeValue::Scalar(Value::Keyword(ident)) => {
            require_qualified_keyword(&ident, "program/entity-map-id")?;
            Ok(EntityRef::Ident(ident))
        }
        RuntimeValue::Scalar(Value::String(tempid)) if !tempid.is_empty() => {
            Ok(EntityRef::Temp(tempid))
        }
        RuntimeValue::Scalar(Value::Long(entity)) if entity >= 0 => Ok(EntityRef::Id(
            u64::try_from(entity).expect("nonnegative i64 always fits u64"),
        )),
        _ => Err(incorrect(
            "program/entity-map-id",
            ":db/id must be an entity reference, nonnegative entity id, ident, or tempid string",
        )),
    }
}

fn runtime_map_value(
    value: RuntimeValue,
    depth: usize,
    allow_many: bool,
) -> Result<MapValue, SemanticError> {
    if depth > 32 {
        return Err(incorrect(
            "program/entity-map-depth",
            "emitted entity maps may contain at most 32 nested map levels",
        ));
    }
    match value {
        RuntimeValue::Scalar(value) => Ok(MapValue::Value(TxValue::Scalar(value))),
        RuntimeValue::Entity(entity) => Ok(MapValue::Value(TxValue::Entity(entity))),
        RuntimeValue::Map(entries) => Ok(MapValue::Nested(Box::new(runtime_entity_map(
            RuntimeValue::Map(entries),
            depth + 1,
        )?))),
        RuntimeValue::Vector(values) if allow_many => {
            let values = values
                .into_iter()
                .map(|value| runtime_map_value(value, depth + 1, false))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(MapValue::Many(values))
        }
        RuntimeValue::Vector(_) => Err(incorrect(
            "program/entity-map-collection",
            "entity-map value collections cannot contain nested collections",
        )),
        RuntimeValue::Null => Err(incorrect(
            "program/entity-map-value",
            "nil is not a valid entity-map attribute value",
        )),
        RuntimeValue::Query(_) => Err(incorrect(
            "program/query-only-value",
            "general query data is not an entity-map attribute value",
        )),
    }
}

fn require_qualified_keyword(keyword: &Keyword, code: &'static str) -> Result<(), SemanticError> {
    let qualified = keyword
        .namespace
        .as_deref()
        .is_some_and(|namespace| !namespace.is_empty() && !namespace.contains('/'))
        && !keyword.name.is_empty()
        && !keyword.name.contains('/');
    if qualified {
        Ok(())
    } else {
        Err(incorrect(code, "keyword must be namespace-qualified"))
    }
}

fn cancel_anomaly(value: RuntimeValue) -> Result<SemanticError, SemanticError> {
    let RuntimeValue::Map(entries) = value else {
        return Err(incorrect(
            "program/cancel-anomaly",
            "cancel anomaly must be a runtime map",
        ));
    };
    let mut typed_entries = entries.clone();
    typed_entries.retain(|(key, _)| {
        !matches!(
            key,
            Value::Keyword(keyword) if keyword.qualified_name() == "edb/cancelled"
        )
    });
    typed_entries.push((
        Value::Keyword(Keyword::new("edb", "cancelled")),
        RuntimeValue::Scalar(Value::Bool(true)),
    ));
    let typed_anomaly = RuntimeValue::map(typed_entries)?;
    let mut category = None;
    let mut message = None;
    let mut details = Vec::with_capacity(entries.len());

    for (key, value) in entries {
        let Value::Keyword(key) = key else {
            return Err(incorrect(
                "program/cancel-anomaly-key",
                "cancel anomaly keys must be namespace-qualified keywords",
            ));
        };
        require_qualified_keyword(&key, "program/cancel-anomaly-key")?;
        let name = key.qualified_name();
        match name.as_str() {
            "cognitect.anomalies/category" => {
                let RuntimeValue::Scalar(Value::Keyword(value)) = &value else {
                    return Err(incorrect(
                        "program/cancel-category",
                        "cancel anomaly category must be a keyword",
                    ));
                };
                category = Some(match value.qualified_name().as_str() {
                    "cognitect.anomalies/incorrect" => ErrorCategory::Incorrect,
                    "cognitect.anomalies/conflict" => ErrorCategory::Conflict,
                    _ => {
                        return Err(incorrect(
                            "program/cancel-category",
                            "cancel anomaly category must be cognitect.anomalies/incorrect or cognitect.anomalies/conflict",
                        ));
                    }
                });
            }
            "cognitect.anomalies/message" => {
                let RuntimeValue::Scalar(Value::String(value)) = &value else {
                    return Err(incorrect(
                        "program/cancel-message",
                        "cancel anomaly message must be a string",
                    ));
                };
                message = Some(value.clone());
            }
            _ => {}
        }
        details.push((name, runtime_detail_text(&value)));
    }

    let category = category.ok_or_else(|| {
        incorrect(
            "program/cancel-category",
            "cancel requires cognitect.anomalies/category",
        )
    })?;
    let mut error = SemanticError::new(
        category,
        "program/rejected",
        message.unwrap_or_else(|| "Operation Cancelled".to_owned()),
    );
    for (key, value) in details {
        error = error.detail(key, value);
    }
    Ok(error
        .detail("edb/cancelled", "true")
        .with_anomaly(typed_anomaly))
}

fn runtime_detail_text(value: &RuntimeValue) -> String {
    match value {
        RuntimeValue::Scalar(Value::String(value) | Value::Uri(value)) => value.clone(),
        RuntimeValue::Scalar(Value::Keyword(value)) => format!(":{}", value.qualified_name()),
        RuntimeValue::Scalar(Value::Symbol(value)) => value.qualified_name(),
        RuntimeValue::Scalar(Value::Bool(value)) => value.to_string(),
        RuntimeValue::Scalar(Value::Long(value)) => value.to_string(),
        RuntimeValue::Scalar(value) => format!("{value:?}"),
        RuntimeValue::Entity(value) => format!("{value:?}"),
        RuntimeValue::Null => "nil".to_owned(),
        RuntimeValue::Query(value) => format!("{value:?}"),
        RuntimeValue::Vector(values) => format!(
            "[{}]",
            values
                .iter()
                .map(runtime_detail_text)
                .collect::<Vec<_>>()
                .join(" ")
        ),
        RuntimeValue::Map(entries) => format!(
            "{{{}}}",
            entries
                .iter()
                .map(|(key, value)| format!("{:?} {}", key, runtime_detail_text(value)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn database_entity_id(
    database: ProgramRead<'_>,
    value: RuntimeValue,
    budget: &mut ProgramBudget<'_>,
) -> Result<u64, SemanticError> {
    match entity_ref(value)? {
        EntityRef::Edn(text) => {
            budget.charge(1)?;
            let entity = match database {
                ProgramRead::Eager(db) => {
                    crate::edn_transaction::lower_entity_ref(&db.database_value(), &text)?
                }
                ProgramRead::Exact(db) => crate::edn_transaction::lower_entity_ref(db, &text)?,
                ProgramRead::AttributePredicate => {
                    return Err(incorrect(
                        "program/predicate-database-read",
                        "attribute predicates cannot resolve database references",
                    ));
                }
            };
            database_entity_id(database, RuntimeValue::Entity(entity), budget)
        }
        EntityRef::Id(entity) => Ok(entity),
        EntityRef::Ident(ident) => database.entid(&ident).ok_or_else(|| {
            incorrect(
                "program/entity-not-found",
                format!("database ident {} did not resolve", ident.qualified_name()),
            )
        }),
        EntityRef::Lookup { attribute, value } => {
            database.lookup(attribute, &value, budget)?.ok_or_else(|| {
                incorrect(
                    "program/entity-not-found",
                    "database lookup reference did not resolve",
                )
            })
        }
        EntityRef::LookupInput { attribute, value } => database
            .lookup_input(attribute, &value, budget)?
            .ok_or_else(|| {
                incorrect(
                    "program/entity-not-found",
                    "database lookup reference did not resolve",
                )
            }),
        EntityRef::Temp(_) | EntityRef::Tx => Err(incorrect(
            "program/unresolved-entity-read",
            "transaction-local entity references cannot be read from db-before",
        )),
    }
}

fn tx_value(value: RuntimeValue) -> Result<TxValue, SemanticError> {
    match value {
        RuntimeValue::Scalar(value) => Ok(TxValue::Scalar(value)),
        RuntimeValue::Entity(entity) => Ok(TxValue::Entity(entity)),
        RuntimeValue::Vector(values) => {
            if !(2..=8).contains(&values.len()) {
                return Err(incorrect(
                    "program/invalid-input-tuple",
                    "input tuples require 2–8 scalar or reference slots",
                ));
            }
            let slots = values
                .into_iter()
                .map(|value| match value {
                    RuntimeValue::Null => Ok(None),
                    RuntimeValue::Scalar(Value::Tuple(_))
                    | RuntimeValue::Vector(_)
                    | RuntimeValue::Query(_)
                    | RuntimeValue::Map(_) => Err(incorrect(
                        "program/invalid-input-tuple",
                        "input tuple slots cannot contain nested collections or tuples",
                    )),
                    RuntimeValue::Scalar(value) => Ok(Some(TxValue::Scalar(value))),
                    RuntimeValue::Entity(entity) => Ok(Some(TxValue::Entity(entity))),
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(TxValue::Tuple(slots))
        }
        RuntimeValue::Null | RuntimeValue::Map(_) | RuntimeValue::Query(_) => Err(incorrect(
            "program/type",
            "transaction values must be scalars, entity references, or tuple vectors",
        )),
    }
}

fn execute_query_template(
    database: ProgramRead<'_>,
    arguments: &[RuntimeValue],
    budget: &mut ProgramBudget<'_>,
    template: &QueryTemplate,
) -> Result<RuntimeValue, SemanticError> {
    let mut result = native_query::execute(database, arguments, budget, template.native_spec())?;
    if template.canonical_order {
        let RuntimeValue::Vector(rows) = &mut result else {
            return Err(incorrect(
                "program/query-result-shape",
                "canonical query ordering requires a relation",
            ));
        };
        // Charge comparisons before sorting, including cancellation and deadline
        // checks. The shared evaluator already admits all retained result values.
        let comparisons = rows.len().saturating_mul(
            usize::try_from(usize::BITS - rows.len().leading_zeros()).unwrap_or(usize::MAX),
        );
        budget.charge(usize_as_u64(comparisons)?)?;
        budget.check_cancel()?;
        rows.sort_by(RuntimeValue::canonical_cmp);
        rows.dedup();
    }
    Ok(result)
}

fn runtime_binary_long(
    stack: &mut Vec<RuntimeValue>,
    operation: &'static str,
    function: fn(i64, i64) -> Option<i64>,
) -> Result<(), SemanticError> {
    let right = long(pop(stack)?)?;
    let left = long(pop(stack)?)?;
    stack.push(RuntimeValue::Scalar(Value::Long(
        function(left, right).ok_or_else(|| {
            incorrect(
                "program/arithmetic",
                format!("integer overflow during {operation}"),
            )
        })?,
    )));
    Ok(())
}

fn runtime_binary_bool(
    stack: &mut Vec<RuntimeValue>,
    function: fn(bool, bool) -> bool,
) -> Result<(), SemanticError> {
    let right = boolean(pop(stack)?)?;
    let left = boolean(pop(stack)?)?;
    stack.push(RuntimeValue::Scalar(Value::Bool(function(left, right))));
    Ok(())
}

fn runtime_compare(stack: &mut Vec<RuntimeValue>, expected: Ordering) -> Result<(), SemanticError> {
    let right = scalar(pop(stack)?)?;
    let left = scalar(pop(stack)?)?;
    stack.push(RuntimeValue::Scalar(Value::Bool(
        left.index_cmp(&right) == expected,
    )));
    Ok(())
}

fn charge_top_runtime_values(
    budget: &mut ProgramBudget<'_>,
    stack: &[RuntimeValue],
    count: usize,
) -> Result<(), SemanticError> {
    let at = stack.len().checked_sub(count).ok_or_else(|| {
        incorrect(
            "program/stack-underflow",
            "comparison consumes more values than are available",
        )
    })?;
    for value in &stack[at..] {
        budget.charge(usize_as_u64(runtime_value_bytes(value, 0)?)?)?;
    }
    Ok(())
}

fn split_stack(
    stack: &mut Vec<RuntimeValue>,
    width: usize,
    context: &str,
) -> Result<Vec<RuntimeValue>, SemanticError> {
    let at = stack.len().checked_sub(width).ok_or_else(|| {
        incorrect(
            "program/stack-underflow",
            format!("{context} exceeds the program stack"),
        )
    })?;
    Ok(stack.split_off(at))
}

fn runtime_get(
    collection: RuntimeValue,
    key: RuntimeValue,
    budget: &mut ProgramBudget<'_>,
) -> Result<RuntimeValue, SemanticError> {
    match (collection, key) {
        (RuntimeValue::Query(value), key) => {
            let key = native_query::runtime_query_value(&key)?;
            let result = match &value {
                crate::QueryValue::Map(entries) => {
                    let mut found = None;
                    for (candidate, value) in entries {
                        if candidate
                            .compare_with(&key, &mut |work| budget.charge(usize_as_u64(work)?))?
                            .is_eq()
                        {
                            found = Some(value.clone());
                            break;
                        }
                    }
                    found
                }
                crate::QueryValue::Tuple(values) | crate::QueryValue::Collection(values) => {
                    match &key {
                        crate::QueryValue::Scalar(Value::Long(index)) if *index >= 0 => {
                            usize::try_from(*index)
                                .ok()
                                .and_then(|index| values.get(index))
                                .cloned()
                        }
                        _ => {
                            return Err(incorrect(
                                "program/index",
                                "sequence index must be a nonnegative integer",
                            ));
                        }
                    }
                }
                _ => return Err(incorrect("program/type", "get requires a map or sequence")),
            };
            Ok(result.map_or(RuntimeValue::Null, RuntimeValue::from_query_value))
        }
        (RuntimeValue::Vector(values), RuntimeValue::Scalar(Value::Long(index))) => {
            let index = usize::try_from(index).map_err(|_| {
                incorrect(
                    "program/index",
                    "vector index must be a nonnegative integer",
                )
            })?;
            Ok(values.into_iter().nth(index).unwrap_or(RuntimeValue::Null))
        }
        (RuntimeValue::Map(entries), RuntimeValue::Scalar(key)) => Ok(entries
            .into_iter()
            .find(|entry| entry.0.index_cmp(&key) == Ordering::Equal)
            .map_or(RuntimeValue::Null, |entry| entry.1)),
        _ => Err(incorrect(
            "program/type",
            "get requires a vector/integer or map/scalar pair",
        )),
    }
}

fn finite_items(
    value: RuntimeValue,
    budget: &mut ProgramBudget<'_>,
) -> Result<Vec<RuntimeValue>, SemanticError> {
    match value {
        RuntimeValue::Query(mut value) => match &mut value {
            crate::QueryValue::Tuple(values) | crate::QueryValue::Collection(values) => {
                Ok(std::mem::take(values)
                    .into_iter()
                    .map(RuntimeValue::from_query_value)
                    .collect())
            }
            crate::QueryValue::Set(values) => {
                let mut values = std::mem::take(values);
                let mut failure = None;
                values.sort_by(|left, right| {
                    if failure.is_some() {
                        return Ordering::Equal;
                    }
                    match left.compare_with(right, &mut |work| budget.charge(usize_as_u64(work)?)) {
                        Ok(order) => order,
                        Err(error) => {
                            failure = Some(error);
                            Ordering::Equal
                        }
                    }
                });
                if let Some(error) = failure.take() {
                    return Err(error);
                }
                values.dedup_by(|left, right| {
                    match left.compare_with(right, &mut |work| budget.charge(usize_as_u64(work)?)) {
                        Ok(order) => order.is_eq(),
                        Err(error) => {
                            failure = Some(error);
                            false
                        }
                    }
                });
                if let Some(error) = failure {
                    return Err(error);
                }
                Ok(values
                    .into_iter()
                    .map(RuntimeValue::from_query_value)
                    .collect())
            }
            crate::QueryValue::Map(entries) => Ok(std::mem::take(entries)
                .into_iter()
                .map(|(key, value)| {
                    RuntimeValue::Vector(vec![
                        RuntimeValue::from_query_value(key),
                        RuntimeValue::from_query_value(value),
                    ])
                })
                .collect()),
            _ => Err(incorrect(
                "program/type",
                "for-each requires a finite query collection",
            )),
        },
        RuntimeValue::Vector(values) => Ok(values),
        RuntimeValue::Map(entries) => Ok(entries
            .into_iter()
            .map(|(key, value)| RuntimeValue::Vector(vec![RuntimeValue::Scalar(key), value]))
            .collect()),
        RuntimeValue::Null => Ok(Vec::new()),
        _ => Err(incorrect(
            "program/type",
            "for-each requires a finite runtime vector or map",
        )),
    }
}

fn validate_runtime_value(
    value: &RuntimeValue,
    max_collection_items: usize,
    depth: usize,
) -> Result<(), SemanticError> {
    if depth > 16 {
        return Err(incorrect(
            "program/value-depth",
            "runtime values may contain at most 16 collection levels",
        ));
    }
    match value {
        RuntimeValue::Scalar(value) => {
            encoded_value_bytes(value, depth)?;
        }
        RuntimeValue::Entity(entity) => {
            entity_ref_bytes_at_depth(entity, depth)?;
        }
        RuntimeValue::Null => {}
        RuntimeValue::Query(value) => {
            validate_general_runtime(value, max_collection_items, depth)?;
        }
        RuntimeValue::Vector(values) => {
            if values.len() > max_collection_items {
                return Err(busy(
                    "program/collection-limit",
                    "runtime vector exceeds its item limit",
                ));
            }
            for value in values {
                validate_runtime_value(value, max_collection_items, depth + 1)?;
            }
        }
        RuntimeValue::Map(entries) => {
            if entries.len() > max_collection_items {
                return Err(busy(
                    "program/collection-limit",
                    "runtime map exceeds its entry limit",
                ));
            }
            if entries.windows(2).any(|entries| {
                entries[0].0.stored_cmp(&entries[1].0) != Ordering::Less
                    || entries[0].0.index_cmp(&entries[1].0) == Ordering::Equal
            }) {
                return Err(incorrect(
                    "program/noncanonical-map",
                    "runtime map keys must be unique and canonically ordered",
                ));
            }
            for (key, value) in entries {
                encoded_value_bytes(key, depth + 1)?;
                validate_runtime_value(value, max_collection_items, depth + 1)?;
            }
        }
    }
    Ok(())
}

fn validate_general_runtime(
    value: &crate::QueryValue,
    max_items: usize,
    depth: usize,
) -> Result<(), SemanticError> {
    use crate::QueryValue;
    if depth > 16 {
        return Err(incorrect(
            "program/value-depth",
            "general runtime values may contain at most 16 collection levels",
        ));
    }
    match value {
        QueryValue::Tuple(values) | QueryValue::Collection(values) | QueryValue::Set(values) => {
            if values.len() > max_items {
                return Err(busy(
                    "program/collection-limit",
                    "general runtime collection exceeds its item limit",
                ));
            }
            for value in values {
                validate_general_runtime(value, max_items, depth + 1)?;
            }
        }
        QueryValue::Map(entries) => {
            if entries.len() > max_items {
                return Err(busy(
                    "program/collection-limit",
                    "general runtime map exceeds its item limit",
                ));
            }
            for (key, value) in entries {
                validate_general_runtime(key, max_items, depth + 1)?;
                validate_general_runtime(value, max_items, depth + 1)?;
            }
        }
        QueryValue::Tagged(_, value) => validate_general_runtime(value, max_items, depth + 1)?,
        QueryValue::Scalar(value) => {
            encoded_value_bytes(value, depth)?;
        }
        QueryValue::Nil | QueryValue::Char(_) => {}
    }
    Ok(())
}

fn general_runtime_bytes(value: &crate::QueryValue, _depth: usize) -> Result<usize, SemanticError> {
    Ok(value.measure_with(&mut |_| Ok(()))?.retained_bytes)
}

fn runtime_value_bytes(value: &RuntimeValue, depth: usize) -> Result<usize, SemanticError> {
    if depth > 16 {
        return Err(incorrect(
            "program/value-depth",
            "runtime values may contain at most 16 collection levels",
        ));
    }
    match value {
        RuntimeValue::Scalar(value) => checked_size_add(1, encoded_value_bytes(value, depth)?),
        RuntimeValue::Entity(entity) => {
            checked_size_add(1, entity_ref_bytes_at_depth(entity, depth)?)
        }
        RuntimeValue::Null => Ok(1),
        RuntimeValue::Query(value) => general_runtime_bytes(value, depth),
        RuntimeValue::Vector(values) => {
            let mut bytes = 5usize;
            for value in values {
                bytes = checked_size_add(bytes, runtime_value_bytes(value, depth + 1)?)?;
            }
            Ok(bytes)
        }
        RuntimeValue::Map(entries) => {
            let mut bytes = 5usize;
            for (key, value) in entries {
                bytes = checked_size_add(bytes, encoded_value_bytes(key, depth + 1)?)?;
                bytes = checked_size_add(bytes, runtime_value_bytes(value, depth + 1)?)?;
            }
            Ok(bytes)
        }
    }
}

/// Return a deterministic upper bound for the bytes occupied by a value in
/// EDB's canonical encoding. Big integers include one possible sign byte,
/// so this is deliberately conservative without allocating a second integer
/// byte buffer merely to meter it.
fn encoded_value_bytes(value: &Value, depth: usize) -> Result<usize, SemanticError> {
    if depth > 16 {
        return Err(incorrect(
            "program/value-depth",
            "program values may contain at most 16 nested tuple levels",
        ));
    }

    match value {
        Value::BigDec(value) => {
            let (integer, _) = value.as_bigint_and_exponent();
            checked_size_add(9, encoded_bigint_bytes(&integer)?)
        }
        Value::BigInt(value) => checked_size_add(1, encoded_bigint_bytes(value)?),
        Value::Bool(_) => Ok(2),
        Value::Bytes(value) => checked_size_add(5, value.len()),
        Value::Double(_) | Value::Instant(_) | Value::Long(_) | Value::Ref(_) => Ok(9),
        Value::Float(_) => Ok(5),
        Value::Function(_) => Ok(33),
        Value::Keyword(value) => {
            let namespace = value
                .namespace
                .as_ref()
                .map_or(Ok(1), |namespace| checked_size_add(5, namespace.len()))?;
            checked_size_add(checked_size_add(5, value.name.len())?, namespace)
        }
        Value::String(value) | Value::Uri(value) => checked_size_add(5, value.len()),
        Value::Symbol(value) => {
            let namespace = value
                .namespace
                .as_ref()
                .map_or(Ok(1), |namespace| checked_size_add(5, namespace.len()))?;
            checked_size_add(checked_size_add(5, value.name.len())?, namespace)
        }
        Value::Tuple(values) => {
            let mut bytes = 5usize;
            for value in values {
                bytes = checked_size_add(bytes, 1)?;
                if let Some(value) = value {
                    bytes = checked_size_add(bytes, encoded_value_bytes(value, depth + 1)?)?;
                }
            }
            Ok(bytes)
        }
        Value::Uuid(_) => Ok(17),
    }
}

fn encoded_bigint_bytes(value: &num_bigint::BigInt) -> Result<usize, SemanticError> {
    let magnitude_bytes = value.bits().div_ceil(8).max(1);
    let magnitude_bytes = usize::try_from(magnitude_bytes).map_err(|_| {
        busy(
            "program/resource-limit",
            "program integer size does not fit this platform",
        )
    })?;
    // Four-byte length prefix plus a possible two's-complement sign byte.
    checked_size_add(5, magnitude_bytes)
}

fn tx_op_output_bytes(operation: &TxOp) -> Result<usize, SemanticError> {
    match operation {
        TxOp::Add { entity, value, .. } => checked_size_add(
            checked_size_add(6, entity_ref_bytes(entity)?)?,
            tx_value_bytes(value)?,
        ),
        TxOp::Retract { entity, value, .. } => {
            let mut bytes = checked_size_add(7, entity_ref_bytes(entity)?)?;
            if let Some(value) = value {
                bytes = checked_size_add(bytes, tx_value_bytes(value)?)?;
            }
            Ok(bytes)
        }
        TxOp::Cas {
            entity, old, new, ..
        } => {
            let mut bytes = checked_size_add(7, entity_ref_bytes(entity)?)?;
            if let Some(old) = old {
                bytes = checked_size_add(bytes, tx_value_bytes(old)?)?;
            }
            checked_size_add(bytes, tx_value_bytes(new)?)
        }
        TxOp::RetractEntity(entity) => checked_size_add(2, entity_ref_bytes(entity)?),
        TxOp::Ensure { entity, spec } => checked_size_add(
            checked_size_add(2, entity_ref_bytes(entity)?)?,
            entity_ref_bytes(spec)?,
        ),
        TxOp::ForcePartition {
            tempid,
            partition: entity,
        }
        | TxOp::MatchPartition { tempid, entity } => checked_size_add(
            checked_size_add(6, tempid.len())?,
            entity_ref_bytes(entity)?,
        ),
        TxOp::InstallAttribute(_) | TxOp::AlterAttribute(_) => Err(incorrect(
            "program/unsupported-schema-form",
            "programs cannot emit native schema descriptor side channels",
        )),
    }
}

fn tx_value_bytes(value: &TxValue) -> Result<usize, SemanticError> {
    tx_value_bytes_at_depth(value, 0)
}

fn tx_value_bytes_at_depth(value: &TxValue, depth: usize) -> Result<usize, SemanticError> {
    if depth > 16 {
        return Err(incorrect(
            "program/value-depth",
            "program values may contain at most 16 collection or reference levels",
        ));
    }
    match value {
        TxValue::Scalar(value) => checked_size_add(1, encoded_value_bytes(value, depth)?),
        TxValue::Entity(entity) => checked_size_add(1, entity_ref_bytes_at_depth(entity, depth)?),
        TxValue::Tuple(slots) => {
            if !(2..=8).contains(&slots.len())
                || slots.iter().flatten().any(|slot| {
                    matches!(slot, TxValue::Tuple(_) | TxValue::Scalar(Value::Tuple(_)))
                })
            {
                return Err(incorrect(
                    "program/invalid-input-tuple",
                    "input tuples require 2–8 scalar or reference slots",
                ));
            }
            let mut bytes = 5;
            for slot in slots {
                bytes = checked_size_add(bytes, 1)?;
                if let Some(slot) = slot {
                    bytes = checked_size_add(bytes, tx_value_bytes_at_depth(slot, depth + 1)?)?;
                }
            }
            Ok(bytes)
        }
    }
}

fn call_output_bytes(
    function: &CallableRef,
    arguments: &[RuntimeValue],
) -> Result<usize, SemanticError> {
    // Form tag + callable tag + collection length. This is a deterministic
    // upper bound of the canonical call encoding and avoids materializing a
    // second copy merely to meter it.
    let mut bytes = checked_size_add(10, callable_ref_bytes(function)?)?;
    for argument in arguments {
        bytes = checked_size_add(bytes, runtime_value_bytes(argument, 0)?)?;
    }
    Ok(bytes)
}

fn callable_ref_bytes(function: &CallableRef) -> Result<usize, SemanticError> {
    match function {
        CallableRef::Database(entity) => entity_ref_bytes(entity),
        CallableRef::ExactHash(_) => Ok(33),
        CallableRef::Local(symbol) => {
            let namespace = symbol
                .namespace
                .as_ref()
                .map_or(Ok(1), |namespace| checked_size_add(5, namespace.len()))?;
            checked_size_add(checked_size_add(6, symbol.name.len())?, namespace)
        }
    }
}

fn entity_ref_bytes(entity: &EntityRef) -> Result<usize, SemanticError> {
    entity_ref_bytes_at_depth(entity, 0)
}

fn entity_ref_bytes_at_depth(entity: &EntityRef, depth: usize) -> Result<usize, SemanticError> {
    if depth > 16 {
        return Err(incorrect(
            "program/value-depth",
            "program values may contain at most 16 collection or reference levels",
        ));
    }
    match entity {
        EntityRef::Id(_) | EntityRef::Tx => Ok(9),
        EntityRef::Ident(keyword) => {
            let namespace = keyword
                .namespace
                .as_ref()
                .map_or(Ok(1), |namespace| checked_size_add(5, namespace.len()))?;
            checked_size_add(checked_size_add(6, keyword.name.len())?, namespace)
        }
        EntityRef::Temp(tempid) => checked_size_add(6, tempid.len()),
        EntityRef::Edn(text) => {
            let canonical = crate::edn_transaction::canonical_entity_ref(text)?;
            // Admission also accounts for the retained caller spelling;
            // comments/whitespace can outweigh its canonical wire payload.
            checked_size_add(6, text.len().max(canonical.len()))
        }
        EntityRef::Lookup { value, .. } => checked_size_add(6, encoded_value_bytes(value, depth)?),
        EntityRef::LookupInput { value, .. } => {
            checked_size_add(6, tx_value_bytes_at_depth(value, depth + 1)?)
        }
    }
}

fn reserve_output(used: &mut usize, amount: usize, limit: usize) -> Result<(), SemanticError> {
    let next = checked_size_add(*used, amount)?;
    if next > limit {
        return Err(busy(
            "program/output-limit",
            "program exceeded its output byte limit",
        ));
    }
    *used = next;
    Ok(())
}

fn checked_size_add(left: usize, right: usize) -> Result<usize, SemanticError> {
    left.checked_add(right).ok_or_else(|| {
        busy(
            "program/resource-limit",
            "program resource accounting overflowed",
        )
    })
}

fn usize_as_u64(value: usize) -> Result<u64, SemanticError> {
    u64::try_from(value).map_err(|_| {
        busy(
            "program/resource-limit",
            "program resource accounting does not fit the fuel counter",
        )
    })
}

fn charge(fuel: &mut u64, amount: u64) -> Result<(), SemanticError> {
    *fuel = fuel
        .checked_sub(amount)
        .ok_or_else(|| busy("program/fuel-exhausted", "program exhausted its fuel"))?;
    Ok(())
}

fn check_cancel(cancelled: Option<&AtomicBool>) -> Result<(), SemanticError> {
    if cancelled.is_some_and(|flag| flag.load(MemoryOrdering::Relaxed)) {
        Err(SemanticError::new(
            ErrorCategory::Interrupted,
            "program/cancelled",
            "program execution was cancelled",
        ))
    } else {
        Ok(())
    }
}

fn allowed_rejection_category(category: ErrorCategory) -> Result<ErrorCategory, SemanticError> {
    match category {
        ErrorCategory::Incorrect | ErrorCategory::Conflict => Ok(category),
        _ => Err(incorrect(
            "program/rejection-category",
            "program rejection category must be incorrect or conflict",
        )),
    }
}

fn incorrect(code: &'static str, message: impl Into<String>) -> SemanticError {
    SemanticError::new(ErrorCategory::Incorrect, code, message)
}

fn busy(code: &'static str, message: impl Into<String>) -> SemanticError {
    SemanticError::new(ErrorCategory::Busy, code, message)
}
