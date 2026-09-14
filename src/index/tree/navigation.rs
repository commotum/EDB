//! Shared sparse-boundary positioning and authenticated parent/child checks.
use crate::index::tree::{ChildRef, LeafSegment};
use crate::index::{IndexComponents, NormalizedIndexBoundary};
use crate::{Datom, ErrorCategory, IndexOrder, IndexPrefix, SemanticError};

/// One normalized raw seek boundary plus the prefix form needed to compare
/// sparse persistent-tree routing keys. A full E/A/V/T boundary compares a
/// routing key's T only after its logical three-component prefix; `added` and
/// stored-value ties intentionally remain outside the public boundary.
#[derive(Clone, Debug)]
pub(crate) struct TreeBoundary {
    pub(crate) normalized: NormalizedIndexBoundary,
    routing_prefix: Option<IndexPrefix>,
    tx: Option<u64>,
}

impl TreeBoundary {
    pub(crate) fn new(normalized: NormalizedIndexBoundary) -> Self {
        let (routing_prefix, tx) = match normalized.unbiased() {
            NormalizedIndexBoundary::Eavt(components) => match components {
                IndexComponents::Empty => (None, None),
                IndexComponents::One(e) => (
                    Some(IndexPrefix::Eavt {
                        entity: *e,
                        attribute: None,
                        value: None,
                    }),
                    None,
                ),
                IndexComponents::Two(e, a) => (
                    Some(IndexPrefix::Eavt {
                        entity: *e,
                        attribute: Some(*a),
                        value: None,
                    }),
                    None,
                ),
                IndexComponents::Three(e, a, v) => (
                    Some(IndexPrefix::Eavt {
                        entity: *e,
                        attribute: Some(*a),
                        value: Some(v.clone()),
                    }),
                    None,
                ),
                IndexComponents::Four(e, a, v, t) => (
                    Some(IndexPrefix::Eavt {
                        entity: *e,
                        attribute: Some(*a),
                        value: Some(v.clone()),
                    }),
                    Some(*t),
                ),
            },
            NormalizedIndexBoundary::Aevt(components) => match components {
                IndexComponents::Empty => (None, None),
                IndexComponents::One(a) => (
                    Some(IndexPrefix::Aevt {
                        attribute: *a,
                        entity: None,
                        value: None,
                    }),
                    None,
                ),
                IndexComponents::Two(a, e) => (
                    Some(IndexPrefix::Aevt {
                        attribute: *a,
                        entity: Some(*e),
                        value: None,
                    }),
                    None,
                ),
                IndexComponents::Three(a, e, v) => (
                    Some(IndexPrefix::Aevt {
                        attribute: *a,
                        entity: Some(*e),
                        value: Some(v.clone()),
                    }),
                    None,
                ),
                IndexComponents::Four(a, e, v, t) => (
                    Some(IndexPrefix::Aevt {
                        attribute: *a,
                        entity: Some(*e),
                        value: Some(v.clone()),
                    }),
                    Some(*t),
                ),
            },
            NormalizedIndexBoundary::Avet(components) => match components {
                IndexComponents::Empty => (None, None),
                IndexComponents::One(a) => (
                    Some(IndexPrefix::Avet {
                        attribute: *a,
                        value: None,
                        entity: None,
                    }),
                    None,
                ),
                IndexComponents::Two(a, v) => (
                    Some(IndexPrefix::Avet {
                        attribute: *a,
                        value: Some(v.clone()),
                        entity: None,
                    }),
                    None,
                ),
                IndexComponents::Three(a, v, e) => (
                    Some(IndexPrefix::Avet {
                        attribute: *a,
                        value: Some(v.clone()),
                        entity: Some(*e),
                    }),
                    None,
                ),
                IndexComponents::Four(a, v, e, t) => (
                    Some(IndexPrefix::Avet {
                        attribute: *a,
                        value: Some(v.clone()),
                        entity: Some(*e),
                    }),
                    Some(*t),
                ),
            },
            NormalizedIndexBoundary::Vaet(components) => match components {
                IndexComponents::Empty => (None, None),
                IndexComponents::One(v) => (
                    Some(IndexPrefix::Vaet {
                        value: v.clone(),
                        attribute: None,
                        entity: None,
                    }),
                    None,
                ),
                IndexComponents::Two(v, a) => (
                    Some(IndexPrefix::Vaet {
                        value: v.clone(),
                        attribute: Some(*a),
                        entity: None,
                    }),
                    None,
                ),
                IndexComponents::Three(v, a, e) => (
                    Some(IndexPrefix::Vaet {
                        value: v.clone(),
                        attribute: Some(*a),
                        entity: Some(*e),
                    }),
                    None,
                ),
                IndexComponents::Four(v, a, e, t) => (
                    Some(IndexPrefix::Vaet {
                        value: v.clone(),
                        attribute: Some(*a),
                        entity: Some(*e),
                    }),
                    Some(*t),
                ),
            },
            NormalizedIndexBoundary::After(_) => {
                unreachable!("after_prefix creates at most one internal bias wrapper")
            }
        };
        Self {
            normalized,
            routing_prefix,
            tx,
        }
    }

    fn compare_routing_key(&self, key: &crate::index::tree::RoutingKey) -> std::cmp::Ordering {
        let primary = self
            .routing_prefix
            .as_ref()
            .map_or(std::cmp::Ordering::Equal, |prefix| key.cmp_prefix(prefix));
        let comparison = if primary.is_ne() || self.tx.is_none() {
            primary
        } else if key.tx == 0 {
            // A validated sparse routing key with omitted T is below every
            // concrete member of the tied logical prefix.
            std::cmp::Ordering::Less
        } else {
            // T sorts descending. Equality intentionally covers assertion,
            // retraction, and every strict stored representation at this T.
            self.tx
                .expect("transaction comparison selected")
                .cmp(&key.tx)
        };
        if self.normalized.is_after_prefix() {
            // Apply strictness to routing keys too: ties can span arbitrarily
            // many leaves/directories and must not be visited one by one.
            comparison.then(std::cmp::Ordering::Less)
        } else {
            comparison
        }
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
