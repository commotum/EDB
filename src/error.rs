use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use crate::program::RuntimeValue;

/// Stable top-level failure classes. Storage-specific retry information will
/// be added at the service boundary; the semantic kernel mainly emits
/// `Incorrect` and `Conflict`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorCategory {
    Incorrect,
    Forbidden,
    Unsupported,
    NotFound,
    Conflict,
    Busy,
    Unavailable,
    Interrupted,
    Fault,
    UnknownOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticError {
    pub category: ErrorCategory,
    pub code: &'static str,
    pub message: String,
    pub details: BTreeMap<String, String>,
    /// Original structured data supplied to a program cancellation.
    ///
    /// `details` remains a compact textual diagnostic surface for ordinary
    /// errors.  Flattening a `d/cancel` anomaly into that map, however, loses
    /// scalar types and nested data, so the exact bounded runtime map is also
    /// retained here.
    pub anomaly: Option<Box<RuntimeValue>>,
}

impl SemanticError {
    pub fn incorrect(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(ErrorCategory::Incorrect, code, message)
    }

    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(ErrorCategory::Conflict, code, message)
    }

    pub fn new(category: ErrorCategory, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            category,
            code,
            message: message.into(),
            details: BTreeMap::new(),
            anomaly: None,
        }
    }

    pub fn detail(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.details.insert(key.into(), value.into());
        self
    }

    pub(crate) fn with_anomaly(mut self, anomaly: RuntimeValue) -> Self {
        self.anomaly = Some(Box::new(anomaly));
        self
    }
}

impl fmt::Display for SemanticError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl Error for SemanticError {}
