//! Direct invocation of immutable, authenticated native database functions.

use crate::database_value::read_context::TransactionReadContext;
use crate::program_bindings::database_program_hash_with_control;
use crate::{
    DatabaseValue, EntityIdentifier, ErrorCategory, ProgramBudget, ProgramControl, ProgramKind,
    ProgramOutput, ProgramRuntime, RuntimeValue, SemanticError, Value,
};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

/// The stored kind normally determines the result. Dual predicates require
/// an explicit role because their two validated bodies have different read
/// capabilities and argument meanings.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum InvokeRole {
    #[default]
    Inferred,
    AttributePredicate,
    EntityPredicate,
}

/// One read-only invocation's resource policy. Program fuel, arguments,
/// allocations and emitted output use the existing native evaluator's bounds.
/// Read limits additionally cover binding resolution and the logical datoms
/// and retained bytes returned by database reads. Raw historical/filter
/// candidates and hidden merge work consume program fuel, even when no
/// logical datom is returned.
/// Cancellation/deadlines are cooperative; an in-flight SQL read or caller
/// supplied filter must return before Rust can observe cancellation.
#[derive(Clone, Copy, Debug)]
pub struct InvokeControl<'a> {
    pub program: ProgramControl<'a>,
    pub max_read_datoms: u64,
    pub max_read_bytes: u64,
    pub deadline: Option<Instant>,
    pub role: InvokeRole,
}

impl Default for InvokeControl<'_> {
    fn default() -> Self {
        Self {
            program: ProgramControl::default(),
            max_read_datoms: 100_000,
            max_read_bytes: 16 * 1024 * 1024,
            deadline: None,
            role: InvokeRole::Inferred,
        }
    }
}

impl InvokeControl<'_> {
    fn check(&self) -> Result<(), SemanticError> {
        if self
            .program
            .cancelled
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
        {
            return Err(SemanticError::new(
                ErrorCategory::Interrupted,
                "program/cancelled",
                "program execution was cancelled",
            ));
        }
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(SemanticError::new(
                ErrorCategory::Interrupted,
                "program/timeout",
                "program invocation exceeded its deadline",
            ));
        }
        Ok(())
    }
}

impl DatabaseValue {
    /// Resolve `:db/fn` by eid, ident or lookup ref in this exact immutable
    /// value and invoke its authenticated native program. This never refreshes
    /// a connection, acquires writer authority or changes durable data.
    ///
    /// Query programs return rows; predicates return their exact runtime value
    /// (not a truthiness conversion). Predicate roles take one stored scalar,
    /// with an entity predicate specifically taking `Value::Ref(entity)`.
    /// Transaction programs return inert declarative forms: nested calls are
    /// not recursively executed and the forms are not transacted. An eager
    /// value lacking retained program content returns `postgres/program-not-found`.
    /// Existing interpreter errors are preserved, including the shared logical
    /// read observer's `transaction/read-capacity` resource error.
    pub fn invoke(
        &self,
        function: impl Into<EntityIdentifier>,
        arguments: &[RuntimeValue],
        control: InvokeControl<'_>,
    ) -> Result<ProgramOutput, SemanticError> {
        control.check()?;
        let mut budget = ProgramBudget::new(control.program)?.with_deadline(control.deadline);
        let database =
            self.clone()
                .with_transaction_read_context(Arc::new(TransactionReadContext::new(
                    control.max_read_datoms,
                    control.max_read_bytes,
                )));
        let mut check = |_: Option<&crate::Datom>| {
            control.check()?;
            budget.charge_query_work(1)?;
            Ok(true)
        };
        let entity = database
            .resolve_entity_identifier_with_control(&function.into(), &mut check)?
            .ok_or_else(|| {
                SemanticError::new(
                    ErrorCategory::NotFound,
                    "program/function-not-found",
                    "database function identifier did not resolve in this database value",
                )
            })?;
        let hash = database_program_hash_with_control(&database, entity, &mut check)?;
        control.check()?;
        // Existing canonical program decoding bounds content to 4 MiB and
        // 4,096 instructions, and verifies the immutable content hash. No
        // dependency closure is needed: emitted calls remain returned data.
        let program = database.resolve_program(hash)?;
        control.check()?;
        let role = match (control.role, program.program().kind) {
            (InvokeRole::Inferred, ProgramKind::AttributePredicate) => {
                InvokeRole::AttributePredicate
            }
            (InvokeRole::Inferred, ProgramKind::EntityPredicate) => InvokeRole::EntityPredicate,
            (InvokeRole::Inferred, ProgramKind::DualPredicate) => {
                return Err(SemanticError::incorrect(
                    "program/predicate-role-required",
                    "dual predicates require an explicit attribute or entity invocation role",
                ));
            }
            (role, _) => role,
        };
        let output = match role {
            InvokeRole::Inferred => ProgramRuntime.execute_prevalidated_runtime_exact_with_budget(
                &program,
                &database,
                arguments,
                &mut budget,
            )?,
            InvokeRole::AttributePredicate => {
                let value = predicate_argument(arguments)?;
                ProgramOutput::AttributePredicate(
                    ProgramRuntime.execute_prevalidated_attribute_predicate_with_budget(
                        &program,
                        value,
                        &mut budget,
                    )?,
                )
            }
            InvokeRole::EntityPredicate => {
                let Value::Ref(entity) = predicate_argument(arguments)? else {
                    return Err(SemanticError::incorrect(
                        "program/predicate-argument",
                        "an entity predicate requires one entity reference value",
                    ));
                };
                ProgramOutput::EntityPredicate(
                    ProgramRuntime.execute_prevalidated_entity_predicate_exact_with_budget(
                        &program,
                        &database,
                        *entity,
                        &mut budget,
                    )?,
                )
            }
        };
        control.check()?;
        Ok(output)
    }
}

fn predicate_argument(arguments: &[RuntimeValue]) -> Result<&Value, SemanticError> {
    match arguments {
        [RuntimeValue::Scalar(value)] => Ok(value),
        [_] => Err(SemanticError::incorrect(
            "program/predicate-argument",
            "a predicate requires one stored scalar argument",
        )),
        _ => Err(SemanticError::incorrect(
            "program/arity",
            "a predicate requires exactly one argument",
        )),
    }
}
