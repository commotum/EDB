//! Native return maps preserve both named access and `:find` column order.
//!
//! This result adapter is the Rust equivalent of the documented `:keys`,
//! `:strs`, and `:syms` return-map clauses. Callers supply the final typed keys;
//! the adapter does not parse query syntax or change query execution.

use super::{QueryResult, QueryValue};
use crate::{SemanticError, Value};
use std::ops::Index;
use std::sync::Arc;

/// The original query result shape, including when no row matched.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReturnMapShape {
    Relation,
    Tuple,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ReturnMapKeys {
    ordered: Vec<Value>,
    // Indices sorted by the native value comparator provide logarithmic
    // lookup without cloning string keys or imposing Rust Ord on all Value.
    positions: Vec<usize>,
}

/// One named result row, retaining the original `:find` column order.
///
/// All rows from one conversion share their immutable key table. Values are
/// moved from the query result without flattening pull maps or other nested
/// query values. Equality includes the positional order as well as the keys.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReturnMap {
    keys: Arc<ReturnMapKeys>,
    values: Vec<QueryValue>,
}

impl ReturnMap {
    pub fn get(&self, key: &Value) -> Option<&QueryValue> {
        self.keys
            .positions
            .binary_search_by(|position| self.keys.ordered[*position].index_cmp(key))
            .ok()
            .map(|position| &self.values[self.keys.positions[position]])
    }

    pub fn keys(&self) -> &[Value] {
        &self.keys.ordered
    }

    /// Values in original `:find` order, also suitable for slice destructuring.
    pub fn values(&self) -> &[QueryValue] {
        &self.values
    }

    /// Key/value pairs in original `:find` order, not sorted map-key order.
    pub fn iter(
        &self,
    ) -> impl ExactSizeIterator<Item = (&Value, &QueryValue)> + DoubleEndedIterator {
        self.keys.ordered.iter().zip(&self.values)
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

impl Index<usize> for ReturnMap {
    type Output = QueryValue;

    fn index(&self, index: usize) -> &Self::Output {
        &self.values[index]
    }
}

/// Named rows from either a relation or a tuple query.
///
/// A tuple contributes zero or one row; `shape()` distinguishes an absent
/// tuple from an empty relation. Relation row order is retained, without
/// adding any ordering guarantee to the original query result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReturnMaps {
    shape: ReturnMapShape,
    keys: Arc<ReturnMapKeys>,
    rows: Vec<ReturnMap>,
}

impl ReturnMaps {
    pub fn shape(&self) -> ReturnMapShape {
        self.shape
    }

    /// The ordered key schema remains available even for an empty result.
    pub fn keys(&self) -> &[Value] {
        &self.keys.ordered
    }

    pub fn rows(&self) -> &[ReturnMap] {
        &self.rows
    }

    pub fn iter(&self) -> std::slice::Iter<'_, ReturnMap> {
        self.rows.iter()
    }

    pub fn into_rows(self) -> Vec<ReturnMap> {
        self.rows
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

impl Index<usize> for ReturnMaps {
    type Output = ReturnMap;

    fn index(&self, index: usize) -> &Self::Output {
        &self.rows[index]
    }
}

impl IntoIterator for ReturnMaps {
    type Item = ReturnMap;
    type IntoIter = std::vec::IntoIter<ReturnMap>;

    fn into_iter(self) -> Self::IntoIter {
        self.rows.into_iter()
    }
}

impl<'a> IntoIterator for &'a ReturnMaps {
    type Item = &'a ReturnMap;
    type IntoIter = std::slice::Iter<'a, ReturnMap>;

    fn into_iter(self) -> Self::IntoIter {
        self.rows.iter()
    }
}

impl QueryResult {
    /// Name the columns of a relation or tuple result.
    ///
    /// Keys must be distinct keyword, string, or symbol values. Their number
    /// must match every result row's `:find` arity. Key types are preserved:
    /// keyword `:name`, string `"name"`, and symbol `name` are different keys.
    /// Scalar and collection results are rejected instead of reshaped.
    ///
    /// Empty results do not retain `:find` arity in `QueryResult`; in that
    /// case this method validates the key schema but cannot check its width.
    /// Use `into_return_maps_with_arity` to validate an empty result against
    /// the original query's known `:find` width as well.
    pub fn into_return_maps(self, keys: Vec<Value>) -> Result<ReturnMaps, SemanticError> {
        convert(self, keys, None)
    }

    /// As `into_return_maps`, also checking a known `:find` arity when no rows
    /// matched. Every present row is checked against that arity too.
    pub fn into_return_maps_with_arity(
        self,
        keys: Vec<Value>,
        find_arity: usize,
    ) -> Result<ReturnMaps, SemanticError> {
        convert(self, keys, Some(find_arity))
    }
}

fn convert(
    result: QueryResult,
    keys: Vec<Value>,
    find_arity: Option<usize>,
) -> Result<ReturnMaps, SemanticError> {
    let (shape, rows) = match result {
        QueryResult::Relation(rows) => (ReturnMapShape::Relation, rows),
        QueryResult::Tuple(row) => (ReturnMapShape::Tuple, row.into_iter().collect()),
        QueryResult::Scalar(_) | QueryResult::Collection(_) => {
            return Err(SemanticError::incorrect(
                "query/return-map-shape",
                "return maps require a relation or tuple query result",
            ));
        }
    };

    for (position, key) in keys.iter().enumerate() {
        if !matches!(key, Value::Keyword(_) | Value::String(_) | Value::Symbol(_)) {
            return Err(SemanticError::incorrect(
                "query/return-map-key-type",
                "return-map keys must be keyword, string, or symbol values",
            )
            .detail("position", position.to_string()));
        }
    }
    let mut positions: Vec<_> = (0..keys.len()).collect();
    positions.sort_unstable_by(|left, right| keys[*left].index_cmp(&keys[*right]));
    for pair in positions.windows(2) {
        if keys[pair[0]] == keys[pair[1]] {
            return Err(SemanticError::incorrect(
                "query/return-map-duplicate-key",
                "return-map keys must be distinct",
            )
            .detail("position", pair[0].max(pair[1]).to_string()));
        }
    }
    if let Some(arity) = find_arity
        && arity != keys.len()
    {
        return Err(arity_error(keys.len(), arity));
    }
    for (position, row) in rows.iter().enumerate() {
        if row.len() != keys.len() {
            return Err(arity_error(keys.len(), row.len()).detail("row", position.to_string()));
        }
    }

    let keys = Arc::new(ReturnMapKeys {
        ordered: keys,
        positions,
    });
    let rows = rows
        .into_iter()
        .map(|values| ReturnMap {
            keys: Arc::clone(&keys),
            values,
        })
        .collect();
    Ok(ReturnMaps { shape, keys, rows })
}

fn arity_error(keys: usize, find_arity: usize) -> SemanticError {
    SemanticError::incorrect(
        "query/return-map-arity",
        "return-map key count must match the find arity",
    )
    .detail("keys", keys.to_string())
    .detail("find_arity", find_arity.to_string())
}
