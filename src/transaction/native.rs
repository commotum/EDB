//! Explicit compiled Rust deployments. These are trusted cooperative callbacks,
//! not serialized closures, a dynamic loader, or a sandbox.
use crate::{
    Attribute, Cardinality, DatabaseValue, ErrorCategory, Keyword, ProgramBudget, RuntimeValue,
    SemanticError, Symbol, TransactionDefaults, TxForm, Value, ValueType,
};
use std::collections::BTreeMap;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

type Transaction = dyn for<'a, 'b> Fn(
        &DatabaseValue,
        &[RuntimeValue],
        &mut NativeCallContext<'a, 'b>,
    ) -> Result<Vec<TxForm>, SemanticError>
    + Send
    + Sync;
type AttributePredicate = dyn for<'a, 'b> Fn(&Value, &mut NativeCallContext<'a, 'b>) -> Result<RuntimeValue, SemanticError>
    + Send
    + Sync;
type EntityPredicate = dyn for<'a, 'b> Fn(
        &DatabaseValue,
        u64,
        &mut NativeCallContext<'a, 'b>,
    ) -> Result<RuntimeValue, SemanticError>
    + Send
    + Sync;

/// Shared transaction controls. Call `check` in loops and `reserve` before
/// substantial temporary allocation. Noncooperative host code cannot be preempted.
pub struct NativeCallContext<'call, 'budget> {
    budget: &'call mut ProgramBudget<'budget>,
    failure: Option<SemanticError>,
}

impl NativeCallContext<'_, '_> {
    pub fn check(&mut self, work: u64) -> Result<(), SemanticError> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        let result = self.budget.native_check(work);
        self.record(result)
    }
    pub fn reserve(&mut self, bytes: usize) -> Result<(), SemanticError> {
        self.check(0)?;
        let result = self.budget.charge_query_bytes(bytes);
        self.record(result)
    }
    pub fn remaining_work(&self) -> u64 {
        self.budget.remaining_fuel()
    }
    fn record(&mut self, result: Result<(), SemanticError>) -> Result<(), SemanticError> {
        if let Err(error) = &result {
            self.failure = Some(error.clone());
        }
        result
    }
    fn finish<T>(&mut self, result: Result<T, SemanticError>) -> Result<T, SemanticError> {
        match self.failure.take() {
            Some(error) => Err(error),
            None => result,
        }
    }
}

#[derive(Default)]
struct Entries {
    transactions: BTreeMap<Symbol, Arc<Transaction>>,
    attributes: BTreeMap<Symbol, Arc<AttributePredicate>>,
    entities: BTreeMap<Symbol, Arc<EntityPredicate>>,
}

/// An immutable deployment snapshot; cloning shares the same callbacks.
#[derive(Clone, Default)]
pub struct NativeRegistry(Arc<Entries>);

impl fmt::Debug for NativeRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeRegistry")
            .field(
                "transactions",
                &self.0.transactions.keys().collect::<Vec<_>>(),
            )
            .field("attributes", &self.0.attributes.keys().collect::<Vec<_>>())
            .field("entities", &self.0.entities.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Mutable construction only; duplicate role/name registrations are errors.
#[derive(Default)]
pub struct NativeRegistryBuilder(Entries);

/// Execution policy is external deployment configuration, never request data.
#[derive(Clone, Debug, Default)]
pub struct TransactionExecutionOptions {
    pub defaults: TransactionDefaults,
    pub native: NativeRegistry,
}

pub fn native_deployment_ident() -> Keyword {
    Keyword::new("atomic.native", "deployment")
}

/// Install this ordinary schema attribute once, using an available schema ID.
/// A predicate entity carries either this Symbol marker or `:db/fn`, never both.
pub fn native_deployment_attribute(id: u32) -> Attribute {
    Attribute::new(
        id,
        native_deployment_ident(),
        ValueType::Symbol,
        Cardinality::One,
    )
}

pub(crate) fn validate_name(name: &Symbol) -> Result<(), SemanticError> {
    let qualified = name.qualified_name();
    if name
        .namespace
        .as_deref()
        .is_some_and(|namespace| !namespace.is_empty())
        && !name.name.is_empty()
        && !name.name.contains('/')
        && !name.namespace.as_deref().unwrap_or_default().contains('/')
        && !qualified.chars().any(char::is_whitespace)
    {
        Ok(())
    } else {
        Err(SemanticError::incorrect(
            "native/invalid-deployment-name",
            format!("native deployment {qualified} must use a fully qualified symbol"),
        ))
    }
}

pub(crate) fn deployment_attribute(database: &DatabaseValue) -> Result<Option<u32>, SemanticError> {
    let Some(id) = database.entid(&native_deployment_ident()) else {
        return Ok(None);
    };
    let id = u32::try_from(id).map_err(|_| {
        SemanticError::incorrect(
            "native/invalid-marker-schema",
            "native deployment marker is not a schema attribute",
        )
    })?;
    let attr = database.schema().attribute(id).map_err(|_| {
        SemanticError::incorrect(
            "native/invalid-marker-schema",
            "native deployment marker is not a schema attribute",
        )
    })?;
    if attr.value_type != ValueType::Symbol
        || attr.cardinality != Cardinality::One
        || attr.no_history
    {
        return Err(SemanticError::incorrect(
            "native/invalid-marker-schema",
            "native deployment marker requires historical Symbol/cardinality-one schema",
        ));
    }
    Ok(Some(id))
}

pub(crate) fn deployment_attribute_id(database: &DatabaseValue) -> Option<u32> {
    database
        .entid(&native_deployment_ident())
        .and_then(|id| u32::try_from(id).ok())
        .filter(|id| database.schema().attribute(*id).is_ok())
}

fn duplicate(name: &Symbol) -> SemanticError {
    SemanticError::incorrect(
        "native/duplicate-deployment",
        format!(
            "native role {} is already registered",
            name.qualified_name()
        ),
    )
}

impl NativeRegistryBuilder {
    pub fn transaction<F>(&mut self, name: Symbol, callback: F) -> Result<&mut Self, SemanticError>
    where
        F: for<'a, 'b> Fn(
                &DatabaseValue,
                &[RuntimeValue],
                &mut NativeCallContext<'a, 'b>,
            ) -> Result<Vec<TxForm>, SemanticError>
            + Send
            + Sync
            + 'static,
    {
        validate_name(&name)?;
        if self.0.transactions.contains_key(&name) {
            return Err(duplicate(&name));
        }
        self.0.transactions.insert(name, Arc::new(callback));
        Ok(self)
    }
    pub fn attribute_predicate<F>(
        &mut self,
        name: Symbol,
        callback: F,
    ) -> Result<&mut Self, SemanticError>
    where
        F: for<'a, 'b> Fn(
                &Value,
                &mut NativeCallContext<'a, 'b>,
            ) -> Result<RuntimeValue, SemanticError>
            + Send
            + Sync
            + 'static,
    {
        validate_name(&name)?;
        if self.0.attributes.contains_key(&name) {
            return Err(duplicate(&name));
        }
        self.0.attributes.insert(name, Arc::new(callback));
        Ok(self)
    }
    pub fn entity_predicate<F>(
        &mut self,
        name: Symbol,
        callback: F,
    ) -> Result<&mut Self, SemanticError>
    where
        F: for<'a, 'b> Fn(
                &DatabaseValue,
                u64,
                &mut NativeCallContext<'a, 'b>,
            ) -> Result<RuntimeValue, SemanticError>
            + Send
            + Sync
            + 'static,
    {
        validate_name(&name)?;
        if self.0.entities.contains_key(&name) {
            return Err(duplicate(&name));
        }
        self.0.entities.insert(name, Arc::new(callback));
        Ok(self)
    }
    pub fn build(self) -> NativeRegistry {
        NativeRegistry(Arc::new(self.0))
    }
}

fn missing(name: &Symbol, role: &str) -> SemanticError {
    SemanticError::new(
        ErrorCategory::NotFound,
        "native/deployment-not-found",
        format!(
            "native {role} {} is not deployed in this host",
            name.qualified_name()
        ),
    )
}

fn contain<T>(
    name: &Symbol,
    callback: impl FnOnce() -> Result<T, SemanticError>,
) -> Result<T, SemanticError> {
    catch_unwind(AssertUnwindSafe(callback)).map_err(|_| {
        SemanticError::new(
            ErrorCategory::Fault,
            "native/callback-panic",
            format!("native deployment {} panicked", name.qualified_name()),
        )
    })?
}

impl NativeRegistry {
    pub fn builder() -> NativeRegistryBuilder {
        NativeRegistryBuilder::default()
    }
    pub(crate) fn require_predicate(
        &self,
        name: &Symbol,
        attribute: bool,
        entity: bool,
    ) -> Result<(), SemanticError> {
        validate_name(name)?;
        if attribute && !self.0.attributes.contains_key(name) {
            return Err(missing(name, "attribute predicate"));
        }
        if entity && !self.0.entities.contains_key(name) {
            return Err(missing(name, "entity predicate"));
        }
        Ok(())
    }
    pub(crate) fn transaction(
        &self,
        name: &Symbol,
        db_before: &DatabaseValue,
        args: &[RuntimeValue],
        budget: &mut ProgramBudget<'_>,
    ) -> Result<Vec<TxForm>, SemanticError> {
        validate_name(name)?;
        let callback = self
            .0
            .transactions
            .get(name)
            .ok_or_else(|| missing(name, "transaction function"))?;
        budget.native_begin(args)?;
        let mut context = NativeCallContext {
            budget,
            failure: None,
        };
        let result = contain(name, || callback(db_before, args, &mut context));
        let forms = context.finish(result)?;
        budget.native_forms(&forms)?;
        Ok(forms)
    }
    pub(crate) fn attribute(
        &self,
        name: &Symbol,
        value: &Value,
        budget: &mut ProgramBudget<'_>,
    ) -> Result<RuntimeValue, SemanticError> {
        let callback = self
            .0
            .attributes
            .get(name)
            .ok_or_else(|| missing(name, "attribute predicate"))?;
        budget.native_begin(&[RuntimeValue::Scalar(value.clone())])?;
        let mut context = NativeCallContext {
            budget,
            failure: None,
        };
        let result = contain(name, || callback(value, &mut context));
        let result = context.finish(result)?;
        budget.native_result(&result)?;
        Ok(result)
    }
    pub(crate) fn entity(
        &self,
        name: &Symbol,
        db_after: &DatabaseValue,
        entity: u64,
        budget: &mut ProgramBudget<'_>,
    ) -> Result<RuntimeValue, SemanticError> {
        let callback = self
            .0
            .entities
            .get(name)
            .ok_or_else(|| missing(name, "entity predicate"))?;
        budget.native_begin(&[RuntimeValue::Scalar(Value::Ref(entity))])?;
        let mut context = NativeCallContext {
            budget,
            failure: None,
        };
        let result = contain(name, || callback(db_after, entity, &mut context));
        let result = context.finish(result)?;
        budget.native_result(&result)?;
        Ok(result)
    }
}
