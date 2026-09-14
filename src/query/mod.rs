//! Native Datalog over explicit immutable database, relation and log values.
//!
//! ATOMIC-NOTE: The recovered query/datalog owners separate preparation,
//! relation execution and find projection. These children preserve that split
//! while retaining Rust's bounded joins and shared invocation controls.

mod bindings;
mod control;
mod custom_aggregate;
mod dependencies;
mod diagnostics;
mod execute;
mod extensions;
mod fulltext;
mod functions;
mod join;
mod model;
mod nested;
mod numeric;
mod patterns;
mod prepare;
mod ranges;
mod results;
mod return_maps;
mod rules;
mod sequence;
mod sources;
mod value;
mod value_debug;

pub use control::QueryControl;
pub use custom_aggregate::{
    AggregateArg, AggregateCall, AggregateGroup, AggregateSource, AggregateValue,
};
pub use diagnostics::{
    QueryClauseStep, QueryDiagnosticOptions, QueryDiagnostics, QueryPhase, QueryPhaseKind,
    QueryStepStatus, QueryStepWork, QueryWarning, query_diagnostics_to_edn,
};
pub use execute::QueryEngine;
pub use extensions::QueryExtensions;
pub use model::{
    Aggregate, Binding, Clause, DataPattern, FindElement, FindSpec, Function, InputSpec, PlanStep,
    Predicate, Query, QueryInput, QueryOutcome, QueryResult, QueryStats, RelationPattern, Rule,
    Term, Variable,
};
pub use prepare::{PreparedQuery, PreparedQueryCache, PreparedQueryCacheStats};
pub use return_maps::{ReturnMap, ReturnMapShape, ReturnMaps};
pub use sequence::QuerySequence;
pub use sources::{QueryDataSource, QuerySource, QuerySourceValue};
pub use value::QueryValue;

pub(crate) use control::query_value_allocation_bytes;
pub(crate) use prepare::validate_program_query;
pub(crate) use value::{QueryValueRef, QueryValueSize};

use bindings::*;
use control::{State, fault, resource};
use dependencies::EvaluationResult;
use execute::{clause_name, evaluate_clauses};
use extensions::{QueryExtension, QueryExtensionImplementation};
use functions::{evaluate_function, evaluate_predicate};
use model::{binding_variables, find_elements, find_variables, variables_in_clauses};
use patterns::*;
use prepare::{validate_or, validate_query, validate_query_values};
use results::*;
use rules::*;
use sources::{
    SourceRef, effective_source, find_source, source_database, validate_consumed_sources,
};

use crate::pull::QueryPullBudget;
use crate::{
    Database, DatabaseValue, ErrorCategory, IndexOrder, IndexPrefix, Program, ProgramControl,
    ProgramHash, ProgramKind, ProgramOutput, ProgramRuntime, PullPattern, SemanticError, Value,
    schema_eid_to_attr_id,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering as AtomicOrdering},
};
use std::time::{Duration, Instant};
