//! Process-local callback roles and adapters for shared successor validation.
use super::forms::{TxCall, TxForm, TxValue};
use crate::{DatabaseValue, RuntimeValue, SemanticError, Value};
use std::collections::BTreeMap;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PredicateRole {
    Attribute,
    Entity,
    Both,
}

impl PredicateRole {
    pub(crate) fn include(self, role: Self) -> Self {
        if self == role { self } else { Self::Both }
    }

    pub(crate) fn requires_attribute(self) -> bool {
        matches!(self, Self::Attribute | Self::Both)
    }

    pub(crate) fn requires_entity(self) -> bool {
        matches!(self, Self::Entity | Self::Both)
    }
}

type NativeFunction =
    dyn Fn(&DatabaseValue, &[TxValue]) -> Result<Vec<TxForm>, SemanticError> + Send + Sync;
type NativePredicate = dyn Fn(&crate::Value) -> Result<RuntimeValue, SemanticError> + Send + Sync;
type NativeEntityPredicate =
    dyn Fn(&DatabaseValue, u64) -> Result<RuntimeValue, SemanticError> + Send + Sync;

/// Process-local deterministic transaction functions.
///
/// Persisted/sandboxed functions are deliberately not represented here. This
/// boundary proves the documented `[db-before, args] -> tx-data` semantics
/// without importing JVM classpath behavior into the native kernel.
#[derive(Clone, Default)]
pub struct TxFunctions {
    functions: BTreeMap<String, Arc<NativeFunction>>,
    predicates: BTreeMap<String, Arc<NativePredicate>>,
    entity_predicates: BTreeMap<String, Arc<NativeEntityPredicate>>,
}

impl fmt::Debug for TxFunctions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TxFunctions")
            .field("names", &self.functions.keys().collect::<Vec<_>>())
            .field("predicates", &self.predicates.keys().collect::<Vec<_>>())
            .field(
                "entity_predicates",
                &self.entity_predicates.keys().collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl TxFunctions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<F>(&mut self, name: impl Into<String>, function: F)
    where
        F: Fn(&DatabaseValue, &[TxValue]) -> Result<Vec<TxForm>, SemanticError>
            + Send
            + Sync
            + 'static,
    {
        self.functions.insert(name.into(), Arc::new(function));
    }

    pub fn register_attribute_predicate<F>(&mut self, name: impl Into<String>, predicate: F)
    where
        F: Fn(&crate::Value) -> Result<bool, SemanticError> + Send + Sync + 'static,
    {
        self.predicates.insert(
            name.into(),
            Arc::new(move |value| {
                predicate(value).map(|value| RuntimeValue::Scalar(Value::Bool(value)))
            }),
        );
    }

    /// Register a predicate whose exact returned value is semantically
    /// observable. Datomic accepts only literal true and retains any other
    /// result in the rejection data; persisted programs use this boundary.
    pub(crate) fn register_attribute_value_predicate<F>(
        &mut self,
        name: impl Into<String>,
        predicate: F,
    ) where
        F: Fn(&crate::Value) -> Result<RuntimeValue, SemanticError> + Send + Sync + 'static,
    {
        self.predicates.insert(name.into(), Arc::new(predicate));
    }

    /// Register an entity-spec predicate. The recovered `ensure-entity!`
    /// boundary invokes these with the complete proposed db-after and the
    /// resolved entity id, while the spec itself was resolved in db-before.
    pub fn register_entity_predicate<F>(&mut self, name: impl Into<String>, predicate: F)
    where
        F: Fn(&DatabaseValue, u64) -> Result<bool, SemanticError> + Send + Sync + 'static,
    {
        self.entity_predicates.insert(
            name.into(),
            Arc::new(move |database, entity| {
                predicate(database, entity).map(|value| RuntimeValue::Scalar(Value::Bool(value)))
            }),
        );
    }

    pub(crate) fn register_entity_value_predicate<F>(
        &mut self,
        name: impl Into<String>,
        predicate: F,
    ) where
        F: Fn(&DatabaseValue, u64) -> Result<RuntimeValue, SemanticError> + Send + Sync + 'static,
    {
        self.entity_predicates
            .insert(name.into(), Arc::new(predicate));
    }

    pub(crate) fn validate_attribute_predicate(
        &self,
        name: &str,
        value: &crate::Value,
    ) -> Result<RuntimeValue, SemanticError> {
        let predicate = self.predicates.get(name).ok_or_else(|| {
            SemanticError::incorrect(
                "transaction/unknown-attribute-predicate",
                format!("unknown attribute predicate {name}"),
            )
        })?;
        catch_unwind(AssertUnwindSafe(|| predicate(value))).map_err(|_| {
            SemanticError::new(
                crate::ErrorCategory::Fault,
                "transaction/predicate-panic",
                format!("attribute predicate {name} panicked"),
            )
        })?
    }

    pub(crate) fn validate_entity_predicate(
        &self,
        name: &str,
        db_after: &DatabaseValue,
        entity: u64,
    ) -> Result<RuntimeValue, SemanticError> {
        let predicate = self.entity_predicates.get(name).ok_or_else(|| {
            SemanticError::incorrect(
                "transaction/unknown-entity-predicate",
                format!("unknown entity predicate {name}"),
            )
        })?;
        catch_unwind(AssertUnwindSafe(|| predicate(db_after, entity)))
            .map_err(|_| entity_predicate_panic(name))?
    }

    pub(super) fn invoke(
        &self,
        db_before: &DatabaseValue,
        call: &TxCall,
    ) -> Result<Vec<TxForm>, SemanticError> {
        crate::transaction_stats::count(|work| &mut work.function_calls, 1);
        let function = self.functions.get(&call.function).ok_or_else(|| {
            SemanticError::incorrect(
                "transaction/unknown-function",
                format!("unknown transaction function {}", call.function),
            )
        })?;
        catch_unwind(AssertUnwindSafe(|| function(db_before, &call.arguments))).map_err(|_| {
            SemanticError::new(
                crate::ErrorCategory::Fault,
                "transaction/function-panic",
                format!("transaction function {} panicked", call.function),
            )
        })?
    }
}

fn entity_predicate_panic(name: &str) -> SemanticError {
    SemanticError::new(
        crate::ErrorCategory::Fault,
        "transaction/predicate-panic",
        format!("entity predicate {name} panicked"),
    )
}
