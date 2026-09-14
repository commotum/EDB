//! Shared transaction computation and clock policy, without writer authority or publication.
use super::expand::normalize_forms_against;
use super::forms::{EntityRef, TxForm, TxOp, TxValue};
use super::functions::TxFunctions;
use super::input::{validate_forms_input, validate_ops_input};
use crate::{Database, DatabaseValue, SemanticError, TxReport, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

impl Database {
    /// Expand structured and process-local transaction forms against this one
    /// immutable db-before without applying them. Persisted database calls are
    /// deliberately resolved by the authoritative transactor first.
    pub(crate) fn normalize_forms(
        &self,
        forms: &[TxForm],
        functions: &TxFunctions,
    ) -> Result<Vec<TxOp>, SemanticError> {
        self.normalize_forms_with_limit(forms, functions, usize::MAX)
    }

    pub(crate) fn normalize_forms_with_limit(
        &self,
        forms: &[TxForm],
        functions: &TxFunctions,
        max_primitive_ops: usize,
    ) -> Result<Vec<TxOp>, SemanticError> {
        normalize_forms_against(
            &self.database_value(),
            forms,
            Some(functions),
            max_primitive_ops,
        )
    }

    /// Normalize map and function forms against one db-before, then apply the
    /// resulting complete primitive information set through `with`.
    pub fn with_forms(
        &self,
        forms: &[TxForm],
        functions: &TxFunctions,
        tx_instant: i64,
    ) -> Result<TxReport, SemanticError> {
        self.with_forms_with_defaults(
            forms,
            functions,
            tx_instant,
            &crate::TransactionDefaults::default(),
        )
    }

    pub fn with_forms_with_defaults(
        &self,
        forms: &[TxForm],
        functions: &TxFunctions,
        tx_instant: i64,
        defaults: &crate::TransactionDefaults,
    ) -> Result<TxReport, SemanticError> {
        let ops = self.normalize_forms(forms, functions)?;
        let assessed = assess_operations(
            &self.database_value(),
            &ops,
            tx_instant,
            defaults,
            Some(functions),
        )?;
        self.materialize_assessment(assessed)
    }
}

impl DatabaseValue {
    /// Apply process-local callbacks to this exact value using the same
    /// transaction assessor as memory and durable transactions.
    pub fn with_functions(
        &self,
        forms: &[TxForm],
        functions: &TxFunctions,
        tx_instant: i64,
    ) -> Result<crate::SpeculativeTransactionReport, SemanticError> {
        self.with_functions_and_defaults(
            forms,
            functions,
            tx_instant,
            &crate::TransactionDefaults::default(),
        )
    }

    pub fn with_functions_and_defaults(
        &self,
        forms: &[TxForm],
        functions: &TxFunctions,
        tx_instant: i64,
        defaults: &crate::TransactionDefaults,
    ) -> Result<crate::SpeculativeTransactionReport, SemanticError> {
        let base = self.speculation_base()?;
        let ops = normalize_forms_against(&base, forms, Some(functions), usize::MAX)?;
        let assessed = assess_operations(&base, &ops, tx_instant, defaults, Some(functions))?;
        Ok(crate::SpeculativeTransactionReport {
            db_before: self.clone(),
            db_after: assessed.db_after.with_speculation_view(self),
            tx_data: assessed.tx_data,
            tempids: assessed.tempids,
        })
    }

    /// Speculate using explicit allocation defaults, never environment state.
    pub fn with_defaults(
        &self,
        ops: &[TxOp],
        tx_instant: i64,
        defaults: &crate::TransactionDefaults,
    ) -> Result<crate::SpeculativeTransactionReport, SemanticError> {
        validate_ops_input(ops)?;
        self.with_forms_with_defaults(
            &ops.iter().cloned().map(TxForm::Op).collect::<Vec<_>>(),
            tx_instant,
            defaults,
        )
    }

    /// Apply primitive transaction information without persisting it. The
    /// caller supplies time, as for `Database::with`.
    pub fn with(
        &self,
        ops: &[TxOp],
        tx_instant: i64,
    ) -> Result<crate::SpeculativeTransactionReport, SemanticError> {
        validate_ops_input(ops)?;
        self.with_forms(
            &ops.iter().cloned().map(TxForm::Op).collect::<Vec<_>>(),
            tx_instant,
        )
    }

    /// Pure native transaction forms, including persisted controlled calls.
    /// All initial and generated calls see this same immutable db-before;
    /// predicates use the same validation path as committed transactions.
    /// Read filters (including as-of/since) are applied to the result, not to
    /// transaction generation or invariants. History values cannot transact.
    pub fn with_forms(
        &self,
        forms: &[TxForm],
        tx_instant: i64,
    ) -> Result<crate::SpeculativeTransactionReport, SemanticError> {
        self.with_forms_with_limits(forms, tx_instant, SpeculationLimits::default())
    }

    pub fn with_forms_with_limits(
        &self,
        forms: &[TxForm],
        tx_instant: i64,
        limits: SpeculationLimits,
    ) -> Result<crate::SpeculativeTransactionReport, SemanticError> {
        self.with_forms_with_limits_and_defaults(
            forms,
            tx_instant,
            limits,
            &crate::TransactionDefaults::default(),
        )
    }

    pub fn with_forms_with_defaults(
        &self,
        forms: &[TxForm],
        tx_instant: i64,
        defaults: &crate::TransactionDefaults,
    ) -> Result<crate::SpeculativeTransactionReport, SemanticError> {
        self.with_forms_with_limits_and_defaults(
            forms,
            tx_instant,
            SpeculationLimits::default(),
            defaults,
        )
    }

    /// Allocation policy and capacity remain independently configurable.
    pub fn with_forms_with_limits_and_defaults(
        &self,
        forms: &[TxForm],
        tx_instant: i64,
        limits: SpeculationLimits,
        defaults: &crate::TransactionDefaults,
    ) -> Result<crate::SpeculativeTransactionReport, SemanticError> {
        self.with_forms_with_execution_options(
            forms,
            tx_instant,
            limits,
            &crate::TransactionExecutionOptions {
                defaults: defaults.clone(),
                native: crate::NativeRegistry::default(),
            },
        )
    }

    /// Exact speculation using the same explicit native deployment snapshot
    /// as a compiled transactor host. Every function reads one db-before;
    /// ensured entity predicates read the complete proposed db-after.
    pub fn with_forms_with_execution_options(
        &self,
        forms: &[TxForm],
        tx_instant: i64,
        limits: SpeculationLimits,
        options: &crate::TransactionExecutionOptions,
    ) -> Result<crate::SpeculativeTransactionReport, SemanticError> {
        let assessed = assess_forms(self, forms, tx_instant, limits, options)?;
        Ok(crate::SpeculativeTransactionReport {
            db_before: self.clone(),
            db_after: assessed.db_after.with_speculation_view(self),
            tx_data: assessed.tx_data,
            tempids: assessed.tempids,
        })
    }

    /// Normalize already expanded persistent transaction forms against one
    /// exact db-before. Entity-map attribute resolution consults only the
    /// immutable schema/ident caches; primitive forms pass through unchanged.
    /// Process-local calls are expanded before entering this primitive normalizer.
    pub(crate) fn normalize_persisted_forms_with_limit(
        &self,
        forms: &[TxForm],
        max_primitive_ops: usize,
    ) -> Result<Vec<TxOp>, SemanticError> {
        normalize_forms_against(self, forms, None, max_primitive_ops)
    }
}

/// Assess normalized operations independently of their storage representation.
/// Local callbacks and persisted callbacks share this exact semantic boundary.
pub(crate) fn assess_operations(
    base: &DatabaseValue,
    ops: &[TxOp],
    tx_instant: i64,
    defaults: &crate::TransactionDefaults,
    functions: Option<&TxFunctions>,
) -> Result<crate::transaction::assess::TieredAssessment, SemanticError> {
    let mut assessed =
        crate::transaction::assess::assess_tiered_with_remaining_limits_and_defaults(
            base,
            ops,
            tx_instant,
            crate::transaction::assess::AssessmentLimits {
                max_read_datoms: u64::MAX,
                max_read_bytes: u64::MAX,
            },
            defaults,
        )?;
    assessed.validate_exact(functions)?;
    assessed.db_before = assessed.db_before.without_transaction_read_context();
    assessed.db_after = assessed.db_after.without_transaction_read_context();
    Ok(assessed)
}

/// Receipt-free outcome of the shared selective semantic pipeline. These are
/// proposed values, not durable publication acknowledgements. Attempt-local
/// memo/observer state is stripped before this handoff.
pub(crate) struct ValidatedTransaction {
    pub(crate) db_before: DatabaseValue,
    pub(crate) db_after: DatabaseValue,
    pub(crate) basis_t: u64,
    pub(crate) eidx_frontier: u64,
    pub(crate) tx_data: Vec<crate::Datom>,
    pub(crate) tempids: BTreeMap<String, u64>,
    pub(crate) read_work: crate::database_value::read_context::TransactionReadWork,
    pub(crate) assessment_work: crate::transaction::assess::AssessmentReadWork,
    /// Resolved fixed dependency closure. Publication must protect any newly
    /// referenced objects even when their immutable bytes already exist.
    pub(crate) retained_programs: crate::program_bindings::ResolvedPrograms,
}

/// Assess transaction forms against one immutable db-before, without acquiring
/// writer authority, looking up receipts, or publishing anything. A durable
/// caller must resolve an existing receipt and check current authority before
/// entering this fresh-work path, then separately protect/publish the outcome.
/// Public speculation restores caller views only after this exact validation.
pub(crate) fn assess_forms(
    base: &DatabaseValue,
    forms: &[TxForm],
    tx_instant: i64,
    limits: SpeculationLimits,
    options: &crate::TransactionExecutionOptions,
) -> Result<ValidatedTransaction, SemanticError> {
    assess_forms_with_clock(base, forms, TxClock::Fixed(tx_instant), limits, options)
}

/// Durable admission selects the transaction clock only after functions and
/// maps have produced normalized operations. Expansion still runs exactly once.
pub(crate) fn assess_durable_forms(
    base: &DatabaseValue,
    forms: &[TxForm],
    server_now: i64,
    option_override: Option<i64>,
    limits: SpeculationLimits,
    options: &crate::TransactionExecutionOptions,
) -> Result<ValidatedTransaction, SemanticError> {
    assess_forms_with_clock(
        base,
        forms,
        TxClock::Durable {
            server_now,
            option_override,
        },
        limits,
        options,
    )
}

enum TxClock {
    Fixed(i64),
    Durable {
        server_now: i64,
        option_override: Option<i64>,
    },
}

fn assess_forms_with_clock(
    base: &DatabaseValue,
    forms: &[TxForm],
    clock: TxClock,
    limits: SpeculationLimits,
    options: &crate::TransactionExecutionOptions,
) -> Result<ValidatedTransaction, SemanticError> {
    use crate::database_value::read_context::TransactionReadContext;
    use crate::program_bindings::{
        expand_submission_forms_with_native, persisted_predicates_with_native,
        transaction_program_roots, validate_successor_program_bindings_with_native,
        visit_program_closure,
    };
    use std::sync::Mutex;
    validate_forms_input(forms)?;
    if limits.max_operations == 0
        || limits.max_read_datoms == 0
        || limits.max_read_bytes == 0
        || limits.max_program_dependencies == 0
        || limits.max_program_bytes == 0
    {
        return Err(SemanticError::incorrect(
            "transaction/invalid-speculation-capacity",
            "speculative operation, read and code dependency limits must be positive",
        ));
    }
    let context = Arc::new(TransactionReadContext::new(
        limits.max_read_datoms,
        limits.max_read_bytes,
    ));
    let before = base
        .speculation_base()?
        .with_transaction_read_context(Arc::clone(&context));
    let budget = Arc::new(Mutex::new(crate::ProgramBudget::new(
        limits.program.control(),
    )?));
    let mut retained = BTreeMap::new();
    let mut program_bytes = 0usize;
    let mut resolve = |hash| {
        if let Some(program) = retained.get(&hash) {
            return Ok(Arc::clone(program));
        }
        if retained.len() >= limits.max_program_dependencies {
            return Err(SemanticError::new(
                crate::ErrorCategory::Busy,
                "transaction/program-dependency-capacity",
                "speculative attempt exceeds its immutable code dependency count",
            ));
        }
        let program = base.resolve_program(hash)?;
        // Same canonical-payload + fixed-overhead proxy as the durable
        // program cache. At most one bounded blob is decoded before the
        // byte limit rejects it; dependency traversal never escapes this
        // resolver, including dormant fixed calls in newly bound code.
        let weight = crate::encode_program(program.program())?
            .len()
            .saturating_add(1_024 + std::mem::size_of::<crate::ProgramHash>());
        program_bytes = program_bytes
            .checked_add(weight)
            .filter(|bytes| *bytes <= limits.max_program_bytes)
            .ok_or_else(|| {
                SemanticError::new(
                    crate::ErrorCategory::Busy,
                    "transaction/program-dependency-capacity",
                    "speculative attempt exceeds its immutable code byte allowance",
                )
            })?;
        retained.insert(hash, Arc::clone(&program));
        Ok(program)
    };
    let operation = crate::OperationContext::current_or_process();
    let expansion_phase = operation.phase(crate::OperationKind::TransactionExpansion);
    let expanded = {
        let mut budget = budget.lock().map_err(|_| {
            SemanticError::new(
                crate::ErrorCategory::Fault,
                "program/budget-poisoned",
                "speculation budget mutex poisoned",
            )
        })?;
        expand_submission_forms_with_native(
            &mut resolve,
            &before,
            forms,
            &mut budget,
            &options.native,
        )?
    };
    let ops = before.normalize_persisted_forms_with_limit(&expanded, limits.max_operations)?;
    drop(expansion_phase);
    let _assessment_phase = operation.phase(crate::OperationKind::TransactionAssessment);
    let tx_instant = match clock {
        TxClock::Fixed(instant) => instant,
        TxClock::Durable {
            server_now,
            option_override,
        } => select_tx_instant(&before, server_now, option_override, &ops)?,
    };
    let remaining = context.remaining()?;
    let assessed = crate::transaction::assess::assess_tiered_with_remaining_limits_and_defaults(
        &before,
        &ops,
        tx_instant,
        crate::transaction::assess::AssessmentLimits {
            max_read_datoms: remaining.datoms,
            max_read_bytes: remaining.retained_bytes,
        },
        &options.defaults,
    )?;
    validate_successor_program_bindings_with_native(
        &mut resolve,
        &assessed.db_before,
        &assessed.db_after,
        &assessed.tx_data,
        &options.native,
    )?;
    let functions = persisted_predicates_with_native(
        &mut resolve,
        &assessed.db_before,
        &assessed.predicate_requirements()?,
        Arc::clone(&budget),
        &options.native,
    )?;
    assessed.validate_exact(Some(&functions))?;
    visit_program_closure(
        &mut resolve,
        transaction_program_roots(&assessed.tx_data),
        &mut |_, _| {},
    )?;
    Ok(ValidatedTransaction {
        db_before: assessed.db_before.without_transaction_read_context(),
        db_after: assessed
            .db_after
            .without_transaction_read_context()
            .retain_programs(retained.clone()),
        basis_t: assessed.basis_t,
        eidx_frontier: assessed.eidx_frontier,
        tx_data: assessed.tx_data,
        tempids: assessed.tempids,
        read_work: context.snapshot()?,
        assessment_work: assessed.read_work,
        retained_programs: retained,
    })
}

/// Clock policy shared by durable backends; this consumes normalized operations
/// rather than raw forms so generated and map-form assertions have one meaning.
pub(crate) fn select_tx_instant(
    db_before: &DatabaseValue,
    server_now: i64,
    option_override: Option<i64>,
    ops: &[TxOp],
) -> Result<i64, SemanticError> {
    let mut data_override = None;
    for op in ops {
        if let TxOp::Add {
            entity: EntityRef::Tx,
            attribute,
            value: TxValue::Scalar(Value::Instant(instant)),
        } = op
            && *attribute == crate::DB_TX_INSTANT as u32
            && data_override.replace(*instant).is_some()
        {
            return Err(SemanticError::incorrect(
                "transaction/multiple-tx-instants",
                ":db/txInstant may be specified only once",
            ));
        }
    }
    let explicit = match (option_override, data_override) {
        (Some(left), Some(right)) if left != right => {
            return Err(SemanticError::incorrect(
                "transaction/tx-instant-mismatch",
                "transaction option and transaction data specify different instants",
            ));
        }
        (Some(instant), _) | (_, Some(instant)) => Some(instant),
        (None, None) => None,
    };
    let previous = db_before.last_tx_instant();
    if let Some(instant) = explicit {
        if instant > server_now {
            return Err(SemanticError::incorrect(
                "transaction/future-tx-instant",
                format!("transaction instant {instant} exceeds transactor clock {server_now}"),
            ));
        }
        if previous.is_some_and(|basis| instant < basis) {
            return Err(SemanticError::incorrect(
                "transaction/past-tx-instant",
                format!("transaction instant {instant} precedes basis instant {previous:?}"),
            ));
        }
        Ok(instant)
    } else {
        Ok(previous.map_or(server_now, |basis| basis.max(server_now)))
    }
}

/// Explicit per-attempt resource policy, independent of transaction semantics.
/// The defaults match the durable service's operation, read and program limits.
#[derive(Clone, Copy, Debug)]
pub struct SpeculationLimits {
    pub max_operations: usize,
    pub max_read_datoms: u64,
    pub max_read_bytes: u64,
    pub program: crate::ProgramLimits,
    /// Distinct immutable programs resolved by this attempt, including the
    /// complete fixed dependency closure of newly referenced code.
    pub max_program_dependencies: usize,
    /// Canonical payload bytes plus 1 KiB/key overhead per resolved program.
    /// The cumulative speculative database may retain code from earlier calls.
    pub max_program_bytes: usize,
}

impl Default for SpeculationLimits {
    fn default() -> Self {
        let durable = crate::CapacityLimits::default();
        Self {
            max_operations: durable.max_transaction_ops,
            max_read_datoms: durable.max_transaction_read_datoms,
            max_read_bytes: durable.max_transaction_read_bytes,
            program: durable.program,
            max_program_dependencies: 1_024,
            max_program_bytes: 64 * 1024 * 1024,
        }
    }
}
