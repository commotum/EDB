//! Controlled transaction semantics shared by durable publication and pure
//! native speculation. The resolver grants immutable code content only; these
//! helpers cannot acquire writer authority or follow a mutable database head.
use crate::encoding::program_call_digest;
use crate::program::ValidatedProgram;
use crate::transaction::functions::PredicateRole;
use crate::{
    CallableRef, DatabaseValue, Datom, Digest, ErrorCategory, ProgramBudget, ProgramCall,
    ProgramHash, ProgramKind, ProgramOutput, ProgramRuntime, SemanticError, TxForm, TxFunctions,
    TxValue, Value,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

pub(crate) type SharedProgramBudget = Arc<Mutex<ProgramBudget<'static>>>;
pub(crate) type ResolvedPrograms = BTreeMap<ProgramHash, Arc<ValidatedProgram>>;
pub(crate) type ProgramResolver<'a> =
    dyn FnMut(ProgramHash) -> Result<Arc<ValidatedProgram>, SemanticError> + 'a;

fn fault(code: &'static str, message: impl Into<String>) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, code, message)
}

fn qualified_program_ident(name: &str) -> Result<crate::Keyword, SemanticError> {
    let Some((namespace, local)) = name.split_once('/') else {
        return Err(SemanticError::incorrect(
            "program/unqualified-predicate",
            format!("predicate {name} must be a fully qualified symbol"),
        ));
    };
    if namespace.is_empty() || local.is_empty() || local.contains('/') {
        return Err(SemanticError::incorrect(
            "program/unqualified-predicate",
            format!("predicate {name} must be a fully qualified symbol"),
        ));
    }
    Ok(crate::Keyword::new(namespace, local))
}

/// Resolve a database function exactly at the recovered `Db.getFn` boundary:
/// first resolve its ident in this immutable db-before, then read its
/// cardinality-one `:db/fn` value. The native value is a content hash rather
/// than a JVM function object.
fn bound_program_hash(
    database: &DatabaseValue,
    ident: &crate::Keyword,
) -> Result<ProgramHash, SemanticError> {
    let entity = database.entid(ident).ok_or_else(|| {
        SemanticError::new(
            ErrorCategory::NotFound,
            "program/function-not-found",
            format!(
                "database function {} is not installed",
                ident.qualified_name()
            ),
        )
    })?;
    match database.values(entity, crate::DB_FN as u32)?.as_slice() {
        [Value::Function(hash)] => Ok(*hash),
        [] => Err(SemanticError::incorrect(
            "program/not-a-database-function",
            format!("entity {} has no :db/fn value", ident.qualified_name()),
        )),
        _ => Err(fault(
            "program/invalid-function-binding",
            format!(
                "entity {} has a malformed :db/fn value",
                ident.qualified_name()
            ),
        )),
    }
}

pub(crate) enum PredicateBinding {
    Program(ProgramHash),
    Native(crate::Symbol),
}

/// Unbound qualified symbols name compiled native predicates directly.
/// Database entities can explicitly alias native or stored code; existing
/// entities with missing or malformed bindings never fall back to native code.
pub(crate) fn resolve_predicate_binding(
    database: &DatabaseValue,
    name: &str,
) -> Result<PredicateBinding, SemanticError> {
    let ident = qualified_program_ident(name)?;
    let Some(entity) = database.entid(&ident) else {
        let symbol = crate::Symbol::new(ident.namespace.as_deref().unwrap(), &ident.name);
        crate::transaction::native::validate_name(&symbol)?;
        return Ok(PredicateBinding::Native(symbol));
    };
    let programs = database.values(entity, crate::DB_FN as u32)?;
    let native = match crate::transaction::native::deployment_attribute_id(database) {
        Some(attribute) => database.values(entity, attribute)?,
        None => Vec::new(),
    };
    if !programs.is_empty() {
        let hash = bound_program_hash(database, &ident)?;
        if !native.is_empty() && crate::transaction::native::deployment_attribute(database).is_ok()
        {
            return Err(SemanticError::incorrect(
                "native/ambiguous-binding",
                "a predicate cannot bind both :db/fn and :edb.native/deployment",
            ));
        }
        return Ok(PredicateBinding::Program(hash));
    }
    if !native.is_empty() {
        crate::transaction::native::deployment_attribute(database)?;
        let [Value::Symbol(name)] = native.as_slice() else {
            return Err(SemanticError::incorrect(
                "native/invalid-binding",
                "native deployment binding must be one qualified Symbol",
            ));
        };
        crate::transaction::native::validate_name(name)?;
        return Ok(PredicateBinding::Native(name.clone()));
    }
    bound_program_hash(database, &ident).map(PredicateBinding::Program)
}

fn database_callable_entity(
    database: &DatabaseValue,
    reference: &crate::EntityRef,
) -> Result<u64, SemanticError> {
    match reference {
        crate::EntityRef::Edn(text) => database_callable_entity(
            database,
            &crate::edn_transaction::lower_entity_ref(database, text)?,
        ),
        crate::EntityRef::Id(entity) => Ok(*entity),
        crate::EntityRef::Ident(ident) => database.entid(ident).ok_or_else(|| {
            SemanticError::new(
                ErrorCategory::NotFound,
                "program/function-not-found",
                format!(
                    "database function {} is not installed",
                    ident.qualified_name()
                ),
            )
        }),
        crate::EntityRef::Lookup { attribute, value } => {
            database.lookup(*attribute, value)?.ok_or_else(|| {
                SemanticError::new(
                    ErrorCategory::NotFound,
                    "program/function-not-found",
                    "database function lookup reference did not resolve",
                )
            })
        }
        crate::EntityRef::LookupInput { attribute, value } => database
            .resolve_lookup_input(*attribute, value)?
            .ok_or_else(|| {
                SemanticError::new(
                    ErrorCategory::NotFound,
                    "program/function-not-found",
                    "database function lookup reference did not resolve",
                )
            }),
        crate::EntityRef::Temp(_) | crate::EntityRef::Tx => Err(SemanticError::incorrect(
            "program/non-temporal-callable",
            "database functions must resolve from db-before, not a transaction-local entity",
        )),
    }
}

fn callable_hash(
    database: &DatabaseValue,
    callable: &CallableRef,
) -> Result<ProgramHash, SemanticError> {
    match callable {
        CallableRef::Database(reference) => {
            let entity = database_callable_entity(database, reference)?;
            database_program_hash_with_control(database, entity, &mut |_| Ok(true))
        }
        CallableRef::ExactHash(hash) => Ok(*hash),
        CallableRef::Local(symbol) => Err(SemanticError::new(
            ErrorCategory::NotFound,
            "program/local-function-not-found",
            format!(
                "no process-local function registry contains {}",
                symbol.qualified_name()
            ),
        )),
    }
}

/// Shared temporal binding lookup for transaction expansion and direct reads.
/// A function binding has one distinct value across all source contributions.
/// The controlled cursor also admits raw candidates hidden by a view filter.
pub(crate) fn database_program_hash_with_control(
    database: &DatabaseValue,
    entity: u64,
    control: &mut dyn FnMut(Option<&crate::Datom>) -> Result<bool, SemanticError>,
) -> Result<ProgramHash, SemanticError> {
    let mut values = database.prefix_cursor(&crate::IndexPrefix::Eavt {
        entity,
        attribute: Some(crate::DB_FN as u32),
        value: None,
    })?;
    let Some(first) = values.next_with_control(control).transpose()? else {
        return Err(SemanticError::incorrect(
            "program/not-a-database-function",
            format!("entity {entity} has no :db/fn value"),
        ));
    };
    let Value::Function(hash) = first.value else {
        return Err(fault(
            "program/invalid-function-binding",
            format!("entity {entity} has a malformed :db/fn value"),
        ));
    };
    while let Some(datom) = values.next_with_control(control).transpose()? {
        if datom.value != Value::Function(hash) {
            return Err(fault(
                "program/invalid-function-binding",
                format!("entity {entity} has a malformed :db/fn value"),
            ));
        }
    }
    Ok(hash)
}

fn execute_program_calls(
    resolve: &mut ProgramResolver<'_>,
    db_before: &DatabaseValue,
    calls: &[ProgramCall],
    budget: &mut ProgramBudget<'_>,
    native: &crate::NativeRegistry,
) -> Result<Vec<TxForm>, SemanticError> {
    let mut ordered = calls
        .iter()
        .map(|call| Ok((program_call_digest(call)?, call)))
        .collect::<Result<Vec<_>, SemanticError>>()?;
    ordered.sort_by(|left, right| left.0.cmp(&right.0));

    let mut forms = Vec::new();
    for (_, call) in ordered {
        expand_program_call(resolve, db_before, call, budget, native, 0, &mut forms)?;
    }
    Ok(forms)
}

pub(crate) fn expand_submission_forms_with_native(
    resolve: &mut ProgramResolver<'_>,
    db_before: &DatabaseValue,
    submitted: &[TxForm],
    budget: &mut ProgramBudget<'_>,
    native: &crate::NativeRegistry,
) -> Result<Vec<TxForm>, SemanticError> {
    crate::transaction::input::validate_forms_input(submitted)?;
    // This runs inside the receipt-miss branch against the locked db-before.
    // Neither schema resolution nor program rebinding can change old retries.
    let lowered;
    let submitted = if crate::transaction::forms::forms_have_edn(submitted) {
        lowered = crate::edn_transaction::lower_forms(db_before, submitted)?;
        lowered.as_slice()
    } else {
        submitted
    };
    let mut forms = Vec::with_capacity(submitted.len());
    let mut calls = Vec::new();
    for form in submitted {
        match form {
            TxForm::Op(_) | TxForm::EntityMap(_) => forms.push(form.clone()),
            TxForm::ProgramCall(call) => calls.push(call.clone()),
            TxForm::Call(_) => {
                return Err(SemanticError::incorrect(
                    "service/process-local-call",
                    "process-local Rust transaction callbacks cannot cross the authoritative service boundary",
                ));
            }
            TxForm::Edn(_) => unreachable!("EDN forms were lowered against db-before"),
        }
    }
    forms.extend(execute_program_calls(
        resolve, db_before, &calls, budget, native,
    )?);
    Ok(forms)
}

fn expand_program_call(
    resolve: &mut ProgramResolver<'_>,
    db_before: &DatabaseValue,
    call: &ProgramCall,
    budget: &mut ProgramBudget<'_>,
    native: &crate::NativeRegistry,
    depth: usize,
    output: &mut Vec<TxForm>,
) -> Result<(), SemanticError> {
    if depth > 32 {
        return Err(SemanticError::incorrect(
            "transaction/function-depth",
            "persisted transaction-function expansion exceeded 32 nested calls",
        ));
    }
    crate::transaction_stats::count(|work| &mut work.function_calls, 1);
    let forms = if let CallableRef::Local(name) = &call.function {
        native.transaction(name, db_before, &call.arguments, budget)?
    } else {
        let hash = callable_hash(db_before, &call.function)?;
        let program = resolve(hash)?;
        if program.program().kind != ProgramKind::Transaction {
            return Err(SemanticError::incorrect(
                "program/not-transaction-function",
                "transaction data called a non-transaction program",
            ));
        }
        let ProgramOutput::Transaction(forms) = ProgramRuntime
            .execute_prevalidated_runtime_exact_with_budget(
                &program,
                db_before,
                &call.arguments,
                budget,
            )?
        else {
            unreachable!("program kind was checked");
        };
        forms
    };
    let forms = if crate::transaction::forms::forms_have_edn(&forms) {
        crate::edn_transaction::lower_forms(db_before, &forms)?
    } else {
        forms
    };
    for form in forms {
        match form {
            TxForm::ProgramCall(nested) => {
                expand_program_call(
                    resolve,
                    db_before,
                    &nested,
                    budget,
                    native,
                    depth + 1,
                    output,
                )?;
            }
            TxForm::Call(_) => {
                return Err(SemanticError::incorrect(
                    "program/process-local-output",
                    "persisted transaction functions cannot emit process-local Rust callbacks",
                ));
            }
            form => output.push(form),
        }
    }
    Ok(())
}

pub(crate) fn validate_successor_program_bindings_with_native(
    resolve: &mut ProgramResolver<'_>,
    db_before: &DatabaseValue,
    db_after: &DatabaseValue,
    tx_data: &[Datom],
    native: &crate::NativeRegistry,
) -> Result<(), SemanticError> {
    let marker_before = crate::transaction::native::deployment_attribute_id(db_before);
    let marker_after = crate::transaction::native::deployment_attribute_id(db_after);
    let marker_definition_changed = marker_before != marker_after
        || tx_data.iter().any(|datom| {
            (Some(datom.entity) == marker_before.map(u64::from)
                || Some(datom.entity) == marker_after.map(u64::from))
                && matches!(
                    u64::from(datom.attribute),
                    crate::DB_IDENT
                        | crate::DB_CARDINALITY
                        | crate::DB_VALUE_TYPE
                        | crate::DB_NO_HISTORY
                )
        });
    if !marker_definition_changed
        && !tx_data.iter().any(|datom| {
            matches!(
                u64::from(datom.attribute),
                crate::DB_FN | crate::DB_IDENT | crate::DB_ATTR_PREDS | crate::DB_ENTITY_PREDS
            ) || Some(datom.attribute) == marker_before
                || Some(datom.attribute) == marker_after
        })
    {
        return Ok(());
    }
    let mut changed_function_entities = BTreeSet::new();
    let mut changed_function_bindings = BTreeSet::new();
    let mut changed_predicate_names = BTreeSet::new();
    if marker_definition_changed {
        // Only explicit marker schema/ident changes inspect its dependents.
        // This is not an ordinary-transaction or unrelated-schema scan.
        for (database, attribute) in [(db_before, marker_before), (db_after, marker_after)] {
            if let Some(attribute) = attribute {
                for datom in database.datoms_with_prefix(&crate::IndexPrefix::Aevt {
                    attribute,
                    entity: None,
                    value: None,
                })? {
                    changed_function_entities.insert(datom.entity);
                    changed_function_bindings.insert(datom.entity);
                }
            }
        }
    }

    for datom in tx_data {
        if Some(datom.attribute) == marker_before || Some(datom.attribute) == marker_after {
            changed_function_entities.insert(datom.entity);
            changed_function_bindings.insert(datom.entity);
        }
        match u64::from(datom.attribute) {
            crate::DB_FN => {
                changed_function_entities.insert(datom.entity);
                changed_function_bindings.insert(datom.entity);
            }
            crate::DB_IDENT => {
                changed_function_entities.insert(datom.entity);
            }
            crate::DB_ATTR_PREDS | crate::DB_ENTITY_PREDS => {
                let Value::Symbol(symbol) = &datom.value else {
                    return Err(SemanticError::incorrect(
                        "program/invalid-predicate-name",
                        "predicate bindings must contain symbols",
                    ));
                };
                let name = symbol.qualified_name();
                qualified_program_ident(&name)?;
                changed_predicate_names.insert(name);
            }
            _ => {}
        }
    }

    // A changed :db/fn value is content-addressed and must resolve before the
    // source transaction publishes. Db.getFn also accepts an eid, so neither
    // an ident nor a qualified ident is required for a generic function. An
    // ident rename is included only because it can break a symbol-named
    // predicate reference even when the hash itself is unchanged.
    for entity in changed_function_entities {
        for database in [db_before, db_after] {
            for value in database.values(entity, crate::DB_IDENT as u32)? {
                if let Value::Keyword(ident) = value {
                    changed_predicate_names.insert(ident.qualified_name());
                }
            }
        }

        let functions = db_after.values(entity, crate::DB_FN as u32)?;
        if let Some(attribute) = marker_after {
            let markers = db_after.values(entity, attribute)?;
            if !markers.is_empty()
                && !functions.is_empty()
                && crate::transaction::native::deployment_attribute(db_after).is_ok()
            {
                return Err(SemanticError::incorrect(
                    "native/ambiguous-binding",
                    "a function cannot bind both :db/fn and :edb.native/deployment",
                ));
            }
        }
        if functions.is_empty() {
            continue;
        }
        let [Value::Function(hash)] = functions.as_slice() else {
            return Err(fault(
                "program/invalid-function-binding",
                format!("entity {entity} has a malformed :db/fn value"),
            ));
        };
        resolve(*hash)?;
    }

    // Validate only dependency names whose binding/reference changed. An old
    // unused bad binding is not transaction input and must not become a
    // global availability gate for unrelated writes.
    // `:db/ident` renames preserve the old name as an alias. A later :db/fn
    // change therefore affects every active predicate name that resolves to
    // the function entity, not merely the entity's current :db/ident datom.
    // Resolve operative names through the immutable db-after dictionary so
    // repurposed aliases follow their new entity. Attribute dependencies have
    // a schema-owned name index, so an ident change needs only point lookups;
    // changing :db/fn must inspect the active names to include retained aliases.
    // Entity predicate declarations still use the exact, read-budgeted source
    // range below. The outer gate keeps ordinary data writes off both paths.
    let changed_roles = predicate_roles(
        db_after,
        &changed_predicate_names,
        &changed_function_bindings,
    )?;
    for (name, role) in changed_roles {
        let hash = match resolve_predicate_binding(db_after, &name)? {
            PredicateBinding::Program(hash) => hash,
            PredicateBinding::Native(deployment) => {
                native.require_predicate(
                    &deployment,
                    role.requires_attribute(),
                    role.requires_entity(),
                )?;
                continue;
            }
        };
        let program = resolve(hash)?;
        if role.requires_attribute() && !program.program().supports_attribute_predicate() {
            return Err(SemanticError::incorrect(
                "program/not-attribute-predicate",
                format!("active program {name} has no attribute-predicate body"),
            ));
        }
        if role.requires_entity() && !program.program().supports_entity_predicate() {
            return Err(SemanticError::incorrect(
                "program/not-entity-predicate",
                format!("active program {name} has no entity-predicate body"),
            ));
        }
    }
    Ok(())
}

fn predicate_roles(
    database: &DatabaseValue,
    names: &BTreeSet<String>,
    function_entities: &BTreeSet<u64>,
) -> Result<BTreeMap<String, PredicateRole>, SemanticError> {
    let mut roles = BTreeMap::<String, (bool, bool)>::new();
    for name in names {
        if database.schema().has_attribute_predicate(name) {
            roles.entry(name.clone()).or_default().0 = true;
        }
    }
    if !function_entities.is_empty() {
        // Scan distinct active predicate names, never unchanged attribute
        // descriptors. A function can have arbitrarily many retained aliases,
        // and the current ident datom alone cannot identify its dependents.
        for name in database.schema().attribute_predicate_names() {
            if !names.contains(name)
                && database
                    .entid(&qualified_program_ident(name)?)
                    .is_some_and(|entity| function_entities.contains(&entity))
            {
                roles.entry(name.to_owned()).or_default().0 = true;
            }
        }
    }
    // :db.entity/preds is data, not an attribute descriptor. Until its own
    // dependency index exists, this source range is necessary even for a new
    // non-function ident: that ident may bind a previously unresolved symbol.
    // Use the database value's observed cursor path so admission budgets still
    // account for every source datom examined.
    for datom in database.datoms_with_prefix(&crate::IndexPrefix::Aevt {
        attribute: crate::DB_ENTITY_PREDS as u32,
        entity: None,
        value: None,
    })? {
        let Value::Symbol(symbol) = &datom.value else {
            return Err(fault(
                "postgres/invalid-entity-predicate",
                "current :db.entity/preds information is not a symbol",
            ));
        };
        let name = symbol.qualified_name();
        let affected = names.contains(&name)
            || (!function_entities.is_empty()
                && database
                    .entid(&qualified_program_ident(&name)?)
                    .is_some_and(|entity| function_entities.contains(&entity)));
        if affected {
            roles.entry(name).or_default().1 = true;
        }
    }
    roles
        .into_iter()
        .map(|(name, (attribute, entity))| {
            let role = match (attribute, entity) {
                (true, false) => PredicateRole::Attribute,
                (false, true) => PredicateRole::Entity,
                (true, true) => PredicateRole::Both,
                (false, false) => unreachable!("only operative names are inserted"),
            };
            Ok((name, role))
        })
        .collect()
}

pub(crate) fn persisted_predicates_with_native(
    resolve: &mut ProgramResolver<'_>,
    database: &DatabaseValue,
    required: &BTreeMap<String, PredicateRole>,
    shared_budget: SharedProgramBudget,
    native: &crate::NativeRegistry,
) -> Result<TxFunctions, SemanticError> {
    let mut functions = TxFunctions::new();
    for (name, role) in required {
        let hash = match resolve_predicate_binding(database, name)? {
            PredicateBinding::Program(hash) => hash,
            PredicateBinding::Native(deployment) => {
                native.require_predicate(
                    &deployment,
                    role.requires_attribute(),
                    role.requires_entity(),
                )?;
                if role.requires_attribute() {
                    let registry = native.clone();
                    let deployment = deployment.clone();
                    let budget = Arc::clone(&shared_budget);
                    functions.register_attribute_value_predicate(name.clone(), move |value| {
                        let mut budget = budget.lock().map_err(|_| {
                            fault(
                                "program/budget-poisoned",
                                "native predicate budget poisoned",
                            )
                        })?;
                        registry.attribute(&deployment, value, &mut budget)
                    });
                }
                if role.requires_entity() {
                    let registry = native.clone();
                    let budget = Arc::clone(&shared_budget);
                    functions.register_entity_value_predicate(
                        name.clone(),
                        move |database, entity| {
                            let mut budget = budget.lock().map_err(|_| {
                                fault(
                                    "program/budget-poisoned",
                                    "native predicate budget poisoned",
                                )
                            })?;
                            registry.entity(&deployment, database, entity, &mut budget)
                        },
                    );
                }
                continue;
            }
        };
        let program = resolve(hash)?;
        if role.requires_attribute() {
            if !program.program().supports_attribute_predicate() {
                return Err(SemanticError::incorrect(
                    "program/not-attribute-predicate",
                    format!("active program {name} has no attribute-predicate body"),
                ));
            }
            let program = Arc::clone(&program);
            let budget = Arc::clone(&shared_budget);
            functions.register_attribute_value_predicate(name.clone(), move |value| {
                let mut budget = budget.lock().map_err(|_| {
                    fault(
                        "program/budget-poisoned",
                        "transaction program budget mutex was poisoned",
                    )
                })?;
                ProgramRuntime.execute_prevalidated_attribute_predicate_with_budget(
                    &program,
                    value,
                    &mut budget,
                )
            });
        }
        if role.requires_entity() {
            if !program.program().supports_entity_predicate() {
                return Err(SemanticError::incorrect(
                    "program/not-entity-predicate",
                    format!("active program {name} has no entity-predicate body"),
                ));
            }
            let program = Arc::clone(&program);
            let budget = Arc::clone(&shared_budget);
            functions.register_entity_value_predicate(name.clone(), move |db_after, entity| {
                let mut budget = budget.lock().map_err(|_| {
                    fault(
                        "program/budget-poisoned",
                        "transaction program budget mutex was poisoned",
                    )
                })?;
                ProgramRuntime.execute_prevalidated_entity_predicate_exact_with_budget(
                    &program,
                    db_after,
                    entity,
                    &mut budget,
                )
            });
        }
    }
    Ok(functions)
}

/// All code carried by transaction information, including retractions: history
/// also retains content identity, just as durable per-generation references do.
pub(crate) fn transaction_program_roots(datoms: &[Datom]) -> BTreeSet<Digest> {
    let mut roots = BTreeSet::new();
    for datom in datoms {
        collect_program_hashes(&datom.value, &mut roots);
    }
    roots
}

pub(crate) fn collect_program_hashes(value: &Value, output: &mut BTreeSet<Digest>) {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Value::Function(hash) => {
                output.insert(*hash);
            }
            Value::Tuple(values) => pending.extend(values.iter().flatten()),
            _ => {}
        }
    }
}

/// Durable marking needs authenticated reachability but not resident program
/// bodies. Share traversal with speculation without changing that footprint.
pub(crate) fn visit_program_closure(
    resolve: &mut ProgramResolver<'_>,
    roots: BTreeSet<Digest>,
    visit: &mut dyn FnMut(ProgramHash, Arc<ValidatedProgram>),
) -> Result<BTreeSet<Digest>, SemanticError> {
    let mut pending = roots.into_iter().collect::<Vec<_>>();
    let mut reachable = BTreeSet::new();
    while let Some(hash) = pending.pop() {
        if !reachable.insert(hash) {
            continue;
        }
        let program = resolve(hash)?;
        collect_fixed_program_dependencies(&program.program().instructions, &mut pending);
        visit(hash, program);
    }
    Ok(reachable)
}

pub(crate) fn collect_fixed_program_dependencies(
    instructions: &[crate::program::Instruction],
    output: &mut Vec<Digest>,
) {
    use crate::program::Instruction;
    let mut pending = instructions.iter().collect::<Vec<_>>();
    while let Some(instruction) = pending.pop() {
        match instruction {
            Instruction::PushConstant(value) => collect_program_hashes_vec(value, output),
            Instruction::PushEntity(reference) => collect_entity_program_hashes(reference, output),
            Instruction::If {
                then_branch,
                else_branch,
            } => {
                pending.extend(then_branch);
                pending.extend(else_branch);
            }
            Instruction::PredicateDispatch { attribute, entity } => {
                pending.extend(attribute);
                pending.extend(entity);
            }
            Instruction::ForEach { body } => pending.extend(body),
            Instruction::Query(query) => {
                collect_native_query_dependencies(query.native_query(), output);
            }
            Instruction::EmitCall { function, .. } => match function {
                CallableRef::ExactHash(hash) => output.push(*hash),
                CallableRef::Database(reference) => {
                    collect_entity_program_hashes(reference, output)
                }
                CallableRef::Local(_) => {}
            },
            _ => {}
        }
    }
}

/// The portable native AST is already admitted by the program codec's node,
/// depth and byte limits. Walk borrowed nodes iteratively; fixed function
/// values are dependencies even in dormant branches, map keys, or Pull data.
fn collect_native_query_dependencies(query: &crate::Query, output: &mut Vec<Digest>) {
    use crate::{Clause, FindElement, FindSpec, Function, PullNested, QueryValue, Term};
    enum Node<'a> {
        Query(&'a crate::Query),
        Clause(&'a Clause),
        Term(&'a Term),
        Find(&'a FindElement),
        Pull(&'a crate::PullPattern),
        Value(&'a QueryValue),
    }
    let mut pending = vec![Node::Query(query)];
    while let Some(node) = pending.pop() {
        match node {
            Node::Query(query) => {
                match &query.find {
                    FindSpec::Relation(elements) | FindSpec::Tuple(elements) => {
                        pending.extend(elements.iter().map(Node::Find))
                    }
                    FindSpec::Scalar(element) | FindSpec::Collection(element) => {
                        pending.push(Node::Find(element))
                    }
                }
                pending.extend(query.clauses.iter().map(Node::Clause));
                for rule in &query.rules {
                    pending.extend(rule.clauses.iter().map(Node::Clause));
                }
            }
            Node::Clause(clause) => match clause {
                Clause::Pattern(pattern) => {
                    pending.extend(
                        [&pattern.entity, &pattern.attribute, &pattern.value]
                            .into_iter()
                            .map(Node::Term),
                    );
                    pending.extend(
                        pattern
                            .transaction
                            .iter()
                            .chain(pattern.fact_source.iter())
                            .chain(pattern.added.iter())
                            .map(Node::Term),
                    );
                }
                Clause::RelationPattern(pattern) => {
                    pending.extend(pattern.terms.iter().map(Node::Term))
                }
                Clause::Predicate { args, .. } | Clause::Rule { args, .. } => {
                    pending.extend(args.iter().map(Node::Term))
                }
                Clause::Function { function, args, .. } => {
                    pending.extend(args.iter().map(Node::Term));
                    if let Function::Query(query) = function {
                        pending.push(Node::Query(query));
                    }
                }
                Clause::Not { clauses, .. } => pending.extend(clauses.iter().map(Node::Clause)),
                Clause::Or { branches, .. } => {
                    for branch in branches {
                        pending.extend(branch.iter().map(Node::Clause));
                    }
                }
            },
            Node::Term(term) => match term {
                Term::Constant(value) => collect_program_hashes_vec(value, output),
                Term::QueryConstant(value) => pending.push(Node::Value(value)),
                Term::Variable(_) | Term::Nil | Term::Blank => {}
            },
            Node::Find(element) => match element {
                FindElement::Pull { pattern, .. } => pending.push(Node::Pull(pattern)),
                FindElement::CustomAggregate(call) => {
                    for arg in &call.args {
                        if let crate::AggregateArg::Constant(value) = arg {
                            pending.push(Node::Value(value));
                        }
                    }
                }
                FindElement::Variable(_) | FindElement::Aggregate { .. } => {}
            },
            Node::Pull(pattern) => {
                for attribute in &pattern.attributes {
                    pending.extend(
                        attribute
                            .alias
                            .iter()
                            .chain(attribute.default.iter())
                            .map(Node::Value),
                    );
                    if let Some(PullNested::Pattern(pattern)) = &attribute.nested {
                        pending.push(Node::Pull(pattern));
                    }
                }
            }
            Node::Value(value) => match value {
                QueryValue::Scalar(value) => collect_program_hashes_vec(value, output),
                QueryValue::Tuple(values)
                | QueryValue::Collection(values)
                | QueryValue::Set(values) => pending.extend(values.iter().map(Node::Value)),
                QueryValue::Map(entries) => {
                    for (key, value) in entries {
                        pending.push(Node::Value(key));
                        pending.push(Node::Value(value));
                    }
                }
                QueryValue::Tagged(_, value) => pending.push(Node::Value(value)),
                QueryValue::Nil | QueryValue::Char(_) => {}
            },
        }
    }
}

fn collect_entity_program_hashes(reference: &crate::EntityRef, output: &mut Vec<Digest>) {
    match reference {
        crate::EntityRef::Edn(text) => {
            if let Ok(value) = crate::edn::read_edn(text) {
                collect_edn_program_hashes(&value, output);
            }
        }
        crate::EntityRef::Lookup { value, .. } => collect_program_hashes_vec(value, output),
        crate::EntityRef::LookupInput { value, .. } => collect_input_program_hashes(value, output),
        crate::EntityRef::Id(_)
        | crate::EntityRef::Ident(_)
        | crate::EntityRef::Temp(_)
        | crate::EntityRef::Tx => {}
    }
}

fn collect_edn_program_hashes(value: &crate::edn::EdnValue, output: &mut Vec<Digest>) {
    use crate::edn::EdnValue;
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            EdnValue::List(values) | EdnValue::Vector(values) | EdnValue::Set(values) => {
                pending.extend(values);
            }
            EdnValue::Map(entries) => {
                for (key, value) in entries {
                    pending.push(key);
                    pending.push(value);
                }
            }
            EdnValue::Tagged(_, payload) => {
                if let Ok(Value::Function(hash)) = crate::edn_value::edn_to_value(value) {
                    output.push(hash);
                } else {
                    pending.push(payload);
                }
            }
            _ => {}
        }
    }
}

fn collect_input_program_hashes(value: &TxValue, output: &mut Vec<Digest>) {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            TxValue::Scalar(value) => collect_program_hashes_vec(value, output),
            TxValue::Tuple(values) => pending.extend(values.iter().flatten()),
            TxValue::Entity(crate::EntityRef::LookupInput { value, .. }) => pending.push(value),
            TxValue::Entity(reference) => collect_entity_program_hashes(reference, output),
        }
    }
}

fn collect_program_hashes_vec(value: &Value, output: &mut Vec<Digest>) {
    let mut hashes = BTreeSet::new();
    collect_program_hashes(value, &mut hashes);
    output.extend(hashes);
}

#[cfg(test)]
mod source_cutover_tests {
    use super::*;
    use crate::{
        Attribute, Cardinality, Database, EntityRef, Instruction, Keyword, Program, RuntimeValue,
        Schema, TxOp, Unique, ValueType,
    };

    fn database() -> Database {
        let mut schema = Schema::new();
        schema
            .install(
                Attribute::new(
                    100,
                    Keyword::new("item", "name"),
                    ValueType::String,
                    Cardinality::One,
                )
                .unique(Unique::Identity),
            )
            .unwrap();
        schema
            .install(Attribute::new(
                101,
                Keyword::new("item", "value"),
                ValueType::Long,
                Cardinality::One,
            ))
            .unwrap();
        let mut component = Attribute::new(
            102,
            Keyword::new("item", "child"),
            ValueType::Ref,
            Cardinality::One,
        );
        component.component = true;
        schema.install(component).unwrap();
        schema
            .install(Attribute::new(
                103,
                Keyword::new("item", "tag"),
                ValueType::Long,
                Cardinality::Many,
            ))
            .unwrap();
        Database::new(schema).unwrap()
    }

    fn add(entity: u64, attribute: u32, value: Value) -> TxOp {
        TxOp::Add {
            entity: EntityRef::Id(entity),
            attribute,
            value: value.into(),
        }
    }

    fn validated(program: Program) -> (ProgramHash, Arc<ValidatedProgram>) {
        let bytes = crate::encode_program(&program).unwrap();
        (
            crate::sha256(&bytes),
            Arc::new(ValidatedProgram::from_canonical(
                crate::decode_program(&bytes).unwrap(),
            )),
        )
    }

    #[test]
    fn duplicate_binding_contributions_resolve_invoke_and_expand_once() {
        let (hash, program) = validated(Program {
            kind: ProgramKind::Transaction,
            arity: 0,
            instructions: vec![
                Instruction::PushEntity(EntityRef::Id(242)),
                Instruction::PushConstant(Value::Long(2)),
                Instruction::EmitAdd(101),
                Instruction::Return,
            ],
        });
        let ops = [
            add(
                260,
                crate::DB_IDENT as u32,
                Value::Keyword(Keyword::new("test", "write")),
            ),
            add(260, crate::DB_FN as u32, Value::Function(hash)),
        ];
        let first = database().with(&ops, 1, EntityRef::Id(99)).unwrap();
        let second = first.db_after.with(&ops, 2, EntityRef::Id(98)).unwrap();
        let db = second
            .db_after
            .database_value()
            .retain_programs(BTreeMap::from([(hash, program)]));
        assert_eq!(
            db.values(260, crate::DB_FN as u32).unwrap(),
            vec![Value::Function(hash)]
        );
        let mut candidates = 0;
        assert_eq!(
            database_program_hash_with_control(&db, 260, &mut |candidate| {
                candidates += usize::from(candidate.is_some());
                Ok(true)
            })
            .unwrap(),
            hash
        );
        assert!(candidates >= 2, "every source contribution is admitted");
        assert_eq!(
            bound_program_hash(&db, &Keyword::new("test", "write")).unwrap(),
            hash
        );
        let ProgramOutput::Transaction(forms) = db
            .invoke(
                Keyword::new("test", "write"),
                &[],
                crate::InvokeControl::default(),
            )
            .unwrap()
        else {
            panic!("expected transaction forms");
        };
        assert_eq!(forms.len(), 1);
        let calls = [TxForm::ProgramCall(ProgramCall {
            function: CallableRef::Database(EntityRef::Edn(":test/write".into())),
            arguments: vec![],
        })];
        let report = db.with_forms(&calls, 3, EntityRef::Id(97)).unwrap();
        assert_eq!(report.tx_data.len(), 2);
        assert!(report.tx_data.iter().all(|datom| datom.source == 97));
        assert_eq!(
            report.db_after.values(242, 101).unwrap(),
            vec![Value::Long(2)]
        );
        assert_eq!(
            report.db_after.values(260, crate::DB_FN as u32).unwrap(),
            vec![Value::Function(hash)]
        );
        // A withdrawal is global; a filtered value cannot revive the binding.
        let withdrawn = report
            .db_after
            .with(
                &[TxOp::Retract {
                    entity: EntityRef::Id(260),
                    attribute: crate::DB_FN as u32,
                    value: Some(Value::Function(hash).into()),
                }],
                4,
                EntityRef::Id(96),
            )
            .unwrap();
        assert_eq!(
            database_program_hash_with_control(
                &withdrawn
                    .db_after
                    .clone()
                    .filter(|_, datom| datom.source == 99),
                260,
                &mut |_| Ok(true)
            )
            .unwrap_err()
            .code,
            "program/not-a-database-function"
        );
    }

    #[test]
    fn nested_calls_maps_and_generated_retractions_inherit_the_request_source() {
        let (child_hash, child) = validated(Program {
            kind: ProgramKind::Transaction,
            arity: 0,
            instructions: vec![
                Instruction::PushEntity(EntityRef::Id(242)),
                Instruction::PushConstant(Value::Long(2)),
                Instruction::EmitAdd(101),
                Instruction::PushEntity(EntityRef::Id(242)),
                Instruction::PushConstant(Value::Long(7)),
                Instruction::EmitRetract(103),
                Instruction::PushEntity(EntityRef::Id(243)),
                Instruction::PushConstant(Value::Long(9)),
                Instruction::PushConstant(Value::Long(10)),
                Instruction::EmitCas(101),
                Instruction::PushEntity(EntityRef::Id(244)),
                Instruction::EmitRetractEntity,
                Instruction::PushEntity(EntityRef::Id(245)),
                Instruction::EmitRetractAll(103),
                Instruction::Return,
            ],
        });
        let (parent_hash, parent) = validated(Program {
            kind: ProgramKind::Transaction,
            arity: 1,
            instructions: vec![
                Instruction::PushArgument(0),
                Instruction::EmitEntityMap,
                Instruction::EmitCall {
                    function: CallableRef::ExactHash(child_hash),
                    argument_count: 0,
                },
                Instruction::Return,
            ],
        });
        let initial = database()
            .with(
                &[
                    add(242, 101, Value::Long(1)),
                    add(242, 103, Value::Long(7)),
                    add(243, 101, Value::Long(9)),
                    add(244, 101, Value::Long(1)),
                    add(245, 103, Value::Long(3)),
                    add(245, 103, Value::Long(4)),
                ],
                1,
                EntityRef::Id(99),
            )
            .unwrap();
        let repeated = initial
            .db_after
            .with(&[add(242, 101, Value::Long(1))], 2, EntityRef::Id(98))
            .unwrap();
        let db = repeated
            .db_after
            .database_value()
            .retain_programs(BTreeMap::from([(child_hash, child), (parent_hash, parent)]));
        let map = RuntimeValue::map(vec![
            (
                Value::Keyword(Keyword::new("db", "id")),
                RuntimeValue::Entity(EntityRef::Temp("author".into())),
            ),
            (
                Value::Keyword(Keyword::new("item", "name")),
                Value::String("Contributor".into()).into(),
            ),
            (
                Value::Keyword(Keyword::new("item", "child")),
                RuntimeValue::map(vec![(
                    Value::Keyword(Keyword::new("item", "value")),
                    Value::Long(5).into(),
                )])
                .unwrap(),
            ),
        ])
        .unwrap();
        let forms = [TxForm::ProgramCall(ProgramCall {
            function: CallableRef::ExactHash(parent_hash),
            arguments: vec![map],
        })];
        let report = db
            .with_forms(&forms, 3, EntityRef::Edn("\"author\"".into()))
            .unwrap();
        let source = report.tempids["author"];
        assert!(report.tx_data.iter().all(|datom| datom.source == source));
        assert_eq!(
            report
                .tx_data
                .iter()
                .filter(|datom| datom.attribute == crate::DB_TX_INSTANT as u32)
                .count(),
            1
        );
        assert_eq!(
            report.tx_data.iter().filter(|datom| !datom.added).count(),
            6
        );
        assert_eq!(
            report.db_after.values(242, 101).unwrap(),
            vec![Value::Long(2)]
        );
        assert_eq!(
            report.db_after.values(243, 101).unwrap(),
            vec![Value::Long(10)]
        );
        for (entity, attribute) in [(242, 103), (244, 101), (245, 103)] {
            assert!(
                report
                    .db_after
                    .values(entity, attribute)
                    .unwrap()
                    .is_empty()
            );
        }
        let nested_values = report.db_after.values(source, 102).unwrap();
        let [Value::Ref(nested)] = nested_values.as_slice() else {
            panic!("expected one nested child");
        };
        assert_eq!(
            report.db_after.values(*nested, 101).unwrap(),
            vec![Value::Long(5)]
        );
    }

    #[test]
    fn fixed_dependencies_include_raw_edn_lookup_values_and_query_source_terms() {
        let hash = [7; 32];
        let tag =
            crate::edn::write_edn(&crate::edn_value::value_to_edn(&Value::Function(hash)).unwrap())
                .unwrap();
        let raw = format!("[:item/code #opaque/wrapper {{:functions #{{{tag}}}}}]");
        let instructions = vec![Instruction::If {
            then_branch: vec![Instruction::PushEntity(EntityRef::Edn(raw.clone()))],
            else_branch: vec![Instruction::EmitCall {
                function: CallableRef::Database(EntityRef::Edn(raw)),
                argument_count: 0,
            }],
        }];
        let mut dependencies = Vec::new();
        collect_fixed_program_dependencies(&instructions, &mut dependencies);
        assert_eq!(dependencies, vec![hash, hash]);
        let mut pattern = crate::DataPattern::new(
            crate::Term::var("entity"),
            crate::Term::Constant(Value::Ref(101)),
            crate::Term::var("value"),
        );
        pattern.fact_source = Some(crate::Term::Constant(Value::Function([8; 32])));
        let query = crate::Query::new(
            crate::FindSpec::Relation(vec![crate::FindElement::Variable("entity".into())]),
            vec![crate::Clause::Pattern(Box::new(pattern))],
        );
        let template = crate::QueryTemplate::native(query, vec![], vec![]).unwrap();
        let mut dependencies = Vec::new();
        collect_fixed_program_dependencies(&[Instruction::Query(template)], &mut dependencies);
        assert_eq!(dependencies, vec![[8; 32]]);
    }

    #[test]
    fn raw_function_lookup_closure_identity_survives_program_round_trip() {
        let (dependency_hash, dependency) = validated(Program {
            kind: ProgramKind::Transaction,
            arity: 0,
            instructions: vec![Instruction::Return],
        });
        let tag = crate::edn::write_edn(
            &crate::edn_value::value_to_edn(&Value::Function(dependency_hash)).unwrap(),
        )
        .unwrap();
        let program = |raw| Program {
            kind: ProgramKind::Query,
            arity: 0,
            instructions: vec![
                Instruction::PushEntity(EntityRef::Edn(raw)),
                Instruction::LoadOne(101),
                Instruction::Return,
            ],
        };
        let left = program(format!("[:item/code {tag}]"));
        let right = program(format!("[ :item/code ; spelling is not identity\n {tag} ]"));
        assert_eq!(
            crate::program_hash(&left).unwrap(),
            crate::program_hash(&right).unwrap()
        );
        let (_, decoded) = validated(right);
        let mut fixed = Vec::new();
        collect_fixed_program_dependencies(&decoded.program().instructions, &mut fixed);
        assert_eq!(fixed, vec![dependency_hash]);
        let root_hash = crate::program_hash(decoded.program()).unwrap();
        let all = BTreeMap::from([(root_hash, decoded), (dependency_hash, dependency)]);
        let mut visited = BTreeSet::new();
        assert_eq!(
            visit_program_closure(
                &mut |hash| Ok(Arc::clone(&all[&hash])),
                BTreeSet::from([root_hash]),
                &mut |hash, _| {
                    visited.insert(hash);
                }
            )
            .unwrap(),
            BTreeSet::from([root_hash, dependency_hash])
        );
        assert_eq!(visited, BTreeSet::from([root_hash, dependency_hash]));
    }
}
