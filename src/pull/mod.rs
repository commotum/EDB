//! Hierarchical projection and lazy entity navigation over captured values.
//!
//! ATOMIC-NOTE: recovered pull selectors, attribute access and graph traversal
//! have distinct owners here. Entity navigation shares attribute resolution,
//! not projection caches; database lineage identity remains a database primitive.

mod attributes;
mod control;
mod entity;
mod execute;
mod index;
mod model;
mod transform;

pub use control::PullControl;
pub(crate) use control::QueryPullBudget;
pub use entity::{Entity, EntityValue};
pub use index::{IndexPullCursor, IndexPullOptions};
pub use model::{
    AttributeName, EntityIdentifier, PullAttribute, PullDirection, PullLimit, PullNested,
    PullPattern,
};
pub use transform::PullTransform;

use crate::{
    Cardinality, Database, DatabaseValue, Datom, ErrorCategory, IndexBoundary, IndexComponents,
    IndexPrefix, Keyword, QueryValue, SemanticError, Symbol, Value, ValueType,
    schema_eid_to_attr_id,
};
use attributes::*;
use control::{PullState, RecursionKey};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;
use transform::finish_attribute;

fn fault(code: &'static str, message: impl Into<String>) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, code, message)
}
