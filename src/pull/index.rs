//! Lazy projection of one AVET or AEVT attribute range.
//!
//! Datomic's Index Pull semantics select the third
//! component after a raw seek/rseek and stops when the attribute changes. It
//! does not deduplicate projected entities or restrict traversal to the
//! supplied second component.

use crate::{
    Cardinality, DatabaseValue, DatabaseValueScanCursor, ErrorCategory, IndexBoundary,
    IndexComponents, PullControl, PullPattern, QueryValue, SemanticError, Value, ValueType,
};
use std::fmt;
use std::iter::FusedIterator;
use std::sync::atomic::Ordering;

/// A normalized start position and a lazy pull selector.
///
/// Build `start` with [`DatabaseValue::avet_boundary`] or
/// [`DatabaseValue::aevt_boundary`] to resolve attribute names, idents, and
/// lookup refs against the same immutable database value.
#[derive(Clone, Debug)]
pub struct IndexPullOptions {
    pub start: IndexBoundary,
    pub selector: PullPattern,
    pub reverse: bool,
    /// Skip datoms in the direction of traversal without pulling them.
    pub offset: usize,
    /// Maximum projected results, or `None` for no limit. This native peer
    /// API defaults to unlimited; Datomic's separate Client API defaults to
    /// 1,000 results. `Some(0)` validates the request but visits no datoms.
    pub limit: Option<usize>,
}

impl IndexPullOptions {
    pub fn new(start: IndexBoundary, selector: PullPattern) -> Self {
        Self {
            start,
            selector,
            reverse: false,
            offset: 0,
            limit: None,
        }
    }
}

/// One exact immutable database and an incremental attribute-range cursor.
///
/// Every `next` pulls at most one result. Errors terminate the cursor; there
/// is no hidden result buffer or eager index materialization. AEVT may pull
/// the same referenced entity more than once, as documented for many-to-many
/// relationships. `PullControl` limits each projection and shares its cancel
/// flag across index traversal (including offsets) and nested pull work.
pub struct IndexPullCursor<'a> {
    database: &'a DatabaseValue,
    cursor: Option<DatabaseValueScanCursor<'a>>,
    selector: PullPattern,
    control: PullControl,
    attribute: u32,
    pull_entity_component: bool,
    offset: usize,
    remaining: Option<usize>,
}

impl fmt::Debug for IndexPullCursor<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IndexPullCursor")
            .field("attribute", &self.attribute)
            .field("pull_entity_component", &self.pull_entity_component)
            .field("offset", &self.offset)
            .field("remaining", &self.remaining)
            .field("finished", &self.cursor.is_none())
            .finish_non_exhaustive()
    }
}

impl DatabaseValue {
    pub fn index_pull(
        &self,
        options: IndexPullOptions,
    ) -> Result<IndexPullCursor<'_>, SemanticError> {
        self.index_pull_with_control(options, &PullControl::default())
    }

    /// Open an attribute-scoped raw-index walk with lazy pull projection.
    /// Selector and boundary validation happen at construction, including
    /// when the range is empty. Construction never scans index datoms.
    pub fn index_pull_with_control(
        &self,
        options: IndexPullOptions,
        control: &PullControl,
    ) -> Result<IndexPullCursor<'_>, SemanticError> {
        self.require_point_in_time("index-pull")?;
        let (attribute, pull_entity_component, second_supplied, value) = match &options.start {
            IndexBoundary::Avet(components) => match components {
                IndexComponents::Empty => return Err(missing_attribute()),
                IndexComponents::One(attribute) => (*attribute, true, false, None),
                IndexComponents::Two(attribute, value)
                | IndexComponents::Three(attribute, value, _)
                | IndexComponents::Four(attribute, value, _, _) => {
                    (*attribute, true, true, Some(value))
                }
            },
            IndexBoundary::Aevt(components) => match components {
                IndexComponents::Empty => return Err(missing_attribute()),
                IndexComponents::One(attribute) => (*attribute, false, false, None),
                IndexComponents::Two(attribute, _) => (*attribute, false, true, None),
                IndexComponents::Three(attribute, _, value)
                | IndexComponents::Four(attribute, _, value, _) => {
                    (*attribute, false, true, Some(value))
                }
            },
            _ => {
                return Err(SemanticError::incorrect(
                    "index-pull/invalid-index",
                    "index-pull requires AVET or AEVT",
                ));
            }
        };
        let descriptor = self.schema().attribute(attribute)?;
        if pull_entity_component && descriptor.cardinality == Cardinality::Many && !second_supplied
        {
            return Err(SemanticError::incorrect(
                "index-pull/missing-start-value",
                "AVET index-pull of a cardinality-many attribute requires a starting value",
            ));
        }
        if !pull_entity_component
            && (descriptor.cardinality != Cardinality::Many
                || descriptor.value_type != ValueType::Ref)
        {
            return Err(SemanticError::incorrect(
                "index-pull/aevt-not-ref-many",
                "AEVT index-pull requires a cardinality-many reference attribute",
            ));
        }
        if let Some(value) = value {
            self.schema().validate_value(descriptor, value)?;
        }
        super::attributes::validate_pattern(self, &options.selector)?;
        let cursor = if options.reverse {
            self.reverse_seek_cursor(&options.start)?
        } else {
            self.seek_cursor(&options.start)?
        };
        Ok(IndexPullCursor {
            database: self,
            cursor: Some(cursor),
            selector: options.selector,
            control: control.clone(),
            attribute,
            pull_entity_component,
            offset: options.offset,
            remaining: options.limit,
        })
    }
}

fn missing_attribute() -> SemanticError {
    SemanticError::incorrect(
        "index-pull/missing-attribute",
        "index-pull start must include an attribute",
    )
}

impl IndexPullCursor<'_> {
    fn fail(&mut self, error: SemanticError) -> Option<Result<QueryValue, SemanticError>> {
        self.cursor = None;
        Some(Err(error))
    }

    fn canceled(&self) -> bool {
        self.control.cancel.load(Ordering::Relaxed)
    }
}

impl Iterator for IndexPullCursor<'_> {
    type Item = Result<QueryValue, SemanticError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == Some(0) {
            self.cursor = None;
        }
        loop {
            self.cursor.as_ref()?;
            if self.canceled() {
                return self.fail(SemanticError::new(
                    ErrorCategory::Interrupted,
                    "pull/canceled",
                    "pull was canceled",
                ));
            }
            // ATOMIC-NOTE: a single visible result can hide arbitrarily many
            // rejected/overlaid datoms. Poll below the view and apply the
            // attribute fence before filters can wander into another range.
            let cancel = &self.control.cancel;
            let attribute = self.attribute;
            let next = self.cursor.as_mut()?.next_with_control(&mut |datom| {
                if cancel.load(Ordering::Relaxed) {
                    return Err(SemanticError::new(
                        ErrorCategory::Interrupted,
                        "pull/canceled",
                        "pull was canceled",
                    ));
                }
                Ok(datom.is_none_or(|datom| datom.attribute == attribute))
            });
            if self.canceled() {
                return self.fail(SemanticError::new(
                    ErrorCategory::Interrupted,
                    "pull/canceled",
                    "pull was canceled",
                ));
            }
            let datom = match next {
                Some(Ok(datom)) => datom,
                Some(Err(error)) => return self.fail(error),
                None => {
                    self.cursor = None;
                    return None;
                }
            };
            if datom.attribute != self.attribute {
                self.cursor = None;
                return None;
            }
            if self.offset > 0 {
                self.offset -= 1;
                continue;
            }
            let entity = if self.pull_entity_component {
                datom.entity
            } else if let Value::Ref(entity) = datom.value {
                entity
            } else {
                return self.fail(SemanticError::new(
                    ErrorCategory::Fault,
                    "index-pull/non-reference-datom",
                    "AEVT reference attribute contains a non-reference value",
                ));
            };
            match self
                .database
                .pull_with_control(&self.selector, entity, &self.control)
            {
                Ok(value) => {
                    if let Some(remaining) = &mut self.remaining {
                        *remaining -= 1;
                    }
                    if self.remaining == Some(0) {
                        self.cursor = None;
                    }
                    return Some(Ok(value));
                }
                Err(error) => return self.fail(error),
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        if self.cursor.is_none() {
            (0, Some(0))
        } else {
            (0, self.remaining)
        }
    }
}

impl FusedIterator for IndexPullCursor<'_> {}
