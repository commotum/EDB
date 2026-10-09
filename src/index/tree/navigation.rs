//! Shared sparse-boundary positioning and authenticated parent/child checks.
use crate::index::NormalizedIndexBoundary;
use crate::index::tree::{ChildRef, LeafSegment};
use crate::{Datom, ErrorCategory, IndexOrder, IndexPrefix, SemanticError};

/// One normalized seek boundary, shared by routing and leaf comparisons.
#[derive(Clone, Debug)]
pub(crate) struct TreeBoundary {
    pub(crate) normalized: NormalizedIndexBoundary,
}
impl TreeBoundary {
    pub(crate) fn new(normalized: NormalizedIndexBoundary) -> Self {
        Self { normalized }
    }
    fn compare_routing_key(&self, key: &crate::index::tree::RoutingKey) -> std::cmp::Ordering {
        key.cmp_boundary(&self.normalized)
    }
}

pub(crate) fn validate_loaded_child_key(
    reference: &ChildRef,
    actual_order: IndexOrder,
    actual_history: bool,
    actual_count: u64,
    actual_first: Option<&crate::index::tree::RoutingKey>,
    expected_order: IndexOrder,
    expected_history: bool,
) -> Result<(), SemanticError> {
    if actual_order != expected_order
        || actual_history != expected_history
        || actual_count != reference.count
        || actual_first.is_none_or(|first| reference.key.cmp_key(first, expected_order).is_gt())
    {
        return Err(fault(
            "peer/tree-child-reference",
            "loaded tree child disagrees with its authenticated parent reference",
        ));
    }
    Ok(())
}

pub(crate) fn validate_loaded_child_datom(
    reference: &ChildRef,
    actual_order: IndexOrder,
    actual_history: bool,
    actual_count: u64,
    actual_first: Option<&Datom>,
    expected_order: IndexOrder,
    expected_history: bool,
) -> Result<(), SemanticError> {
    if actual_order != expected_order
        || actual_history != expected_history
        || actual_count != reference.count
        || actual_first.is_none_or(|first| reference.key.cmp_datom(first, expected_order).is_gt())
    {
        return Err(fault(
            "peer/tree-child-reference",
            "loaded tree child disagrees with its authenticated parent reference",
        ));
    }
    Ok(())
}

pub(crate) fn floor_tree_child(children: &[ChildRef], key: &Datom, order: IndexOrder) -> usize {
    children
        .partition_point(|child| !child.key.cmp_datom(key, order).is_gt())
        .saturating_sub(1)
}

pub(crate) fn prefix_start_child(children: &[ChildRef], prefix: &IndexPrefix) -> usize {
    children
        .partition_point(|child| child.key.cmp_prefix(prefix).is_lt())
        .saturating_sub(1)
}

pub(crate) fn boundary_floor_child(
    children: &[ChildRef],
    boundary: &TreeBoundary,
) -> Option<usize> {
    children
        .partition_point(|child| !boundary.compare_routing_key(&child.key).is_gt())
        .checked_sub(1)
}

pub(crate) fn boundary_start_child(children: &[ChildRef], boundary: &TreeBoundary) -> usize {
    // Equal virtual keys can span children (operation and stored-value ties
    // are omitted from the boundary). Forward seeks must start before all of
    // them; reverse seeks use the last equal child instead.
    children
        .partition_point(|child| boundary.compare_routing_key(&child.key).is_lt())
        .saturating_sub(1)
}

pub(crate) fn leaf_lower_bound(leaf: &LeafSegment, key: &Datom, order: IndexOrder) -> usize {
    let mut low = 0;
    let mut high = leaf.len();
    while low < high {
        let middle = low + (high - low) / 2;
        let datom = leaf.datom(middle).expect("validated parallel leaf columns");
        if datom.cmp_in(key, order).is_lt() {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    low
}

pub(crate) fn leaf_prefix_lower_bound(leaf: &LeafSegment, prefix: &IndexPrefix) -> usize {
    let mut low = 0;
    let mut high = leaf.len();
    while low < high {
        let middle = low + (high - low) / 2;
        let datom = leaf.datom(middle).expect("validated parallel leaf columns");
        if crate::index::compare_prefix(&datom, prefix).is_lt() {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    low
}

pub(crate) fn leaf_boundary_lower_bound(leaf: &LeafSegment, boundary: &TreeBoundary) -> usize {
    let mut low = 0;
    let mut high = leaf.len();
    while low < high {
        let middle = low + (high - low) / 2;
        let datom = leaf.datom(middle).expect("validated parallel leaf columns");
        if boundary.normalized.compare_datom(&datom).is_lt() {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    low
}

pub(crate) fn leaf_reverse_upper_bound(leaf: &LeafSegment, boundary: &TreeBoundary) -> usize {
    let mut low = 0;
    let mut high = leaf.len();
    while low < high {
        let middle = low + (high - low) / 2;
        let datom = leaf.datom(middle).expect("validated parallel leaf columns");
        if !boundary.normalized.compare_datom(&datom).is_gt() {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    low
}

fn fault(code: &'static str, message: &'static str) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, code, message)
}
