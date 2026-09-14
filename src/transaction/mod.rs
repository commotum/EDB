//! Declarative transaction semantics shared by memory, peers and durable writers.
//!
//! Expansion and assessment compute proposed immutable values. Writer authority,
//! receipts and durable publication belong to their callers, not this module.
pub(crate) mod assess;
pub(crate) mod canonical;
mod expand;
pub(crate) mod forms;
pub(crate) mod functions;
pub(crate) mod input;
pub(crate) mod native;
pub(crate) mod pipeline;

pub use forms::{AttributeRef, EntityMap, EntityRef, MapValue, TxCall, TxForm, TxOp, TxValue};
pub use functions::TxFunctions;
pub use pipeline::SpeculationLimits;
