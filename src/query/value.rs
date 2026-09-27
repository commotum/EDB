use crate::{SemanticError, Symbol, Value};
use std::hash::{Hash, Hasher};

pub enum QueryValue {
    Nil,
    Scalar(Value),
    Collection(Vec<QueryValue>),
    Tuple(Vec<QueryValue>),
    Map(Vec<(QueryValue, QueryValue)>),
    /// An unordered mathematical set; duplicate logical elements are ignored.
    Set(Vec<QueryValue>),
    Char(char),
    /// Inert data: reading or comparing a tag never invokes executable code.
    Tagged(Symbol, Box<QueryValue>),
}

impl QueryValue {
    /// Metered shape-preserving comparison for VM/query operations. Unlike a
    /// join key this does not normalize a result Tuple into a stored tuple.
    pub(crate) fn compare_with(
        &self,
        other: &Self,
        check: &mut impl FnMut(usize) -> Result<(), SemanticError>,
    ) -> Result<std::cmp::Ordering, SemanticError> {
        check(1)?;
        match (self, other) {
            (Self::Nil, Self::Nil) => return Ok(std::cmp::Ordering::Equal),
            (Self::Nil, _) => return Ok(std::cmp::Ordering::Less),
            (_, Self::Nil) => return Ok(std::cmp::Ordering::Greater),
            (Self::Scalar(left), Self::Scalar(right)) => return Ok(left.index_cmp(right)),
            (Self::Char(left), Self::Char(right)) => return Ok(left.cmp(right)),
            _ => {}
        }
        let mut arena = Vec::new();
        let left = build_node(QueryValueRef::Query(self), &mut arena, false, false, check)?;
        let right = build_node(QueryValueRef::Query(other), &mut arena, false, false, check)?;
        compare_nodes(&arena, left, right, check)
    }

    /// Deterministic total comparison used to canonicalize arbitrary pull
    /// result keys. This is deliberately a query-result ordering rather than
    /// a stored-value index ordering: container shape participates before
    /// recursively comparing contents, while scalar comparison delegates to
    /// EDB's logical value comparator.
    pub fn canonical_cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        enum Task<'a> {
            Values(&'a QueryValue, &'a QueryValue),
            Length(usize, usize),
        }
        let rank = |value: &Self| match value {
            Self::Nil => 0_u8,
            Self::Scalar(_) => 1,
            Self::Tuple(_) => 2,
            Self::Collection(_) => 3,
            Self::Map(_) => 4,
            Self::Set(_) => 5,
            Self::Char(_) => 6,
            Self::Tagged(_, _) => 7,
        };
        let mut pending = vec![Task::Values(self, other)];
        while let Some(task) = pending.pop() {
            let (left, right) = match task {
                Task::Length(left, right) => {
                    let order = left.cmp(&right);
                    if order != Ordering::Equal {
                        return order;
                    }
                    continue;
                }
                Task::Values(left, right) => (left, right),
            };
            let order = rank(left).cmp(&rank(right));
            if order != Ordering::Equal {
                return order;
            }
            match (left, right) {
                (Self::Nil, Self::Nil) => {}
                (Self::Scalar(left), Self::Scalar(right)) => {
                    let order = left.index_cmp(right);
                    if order != Ordering::Equal {
                        return order;
                    }
                }
                (Self::Tuple(left), Self::Tuple(right))
                | (Self::Collection(left), Self::Collection(right)) => {
                    pending.push(Task::Length(left.len(), right.len()));
                    for (left, right) in left.iter().zip(right).rev() {
                        pending.push(Task::Values(left, right));
                    }
                }
                (Self::Map(_), Self::Map(_)) | (Self::Set(_), Self::Set(_)) => {
                    let order = compare_unordered(left, right);
                    if order != Ordering::Equal {
                        return order;
                    }
                }
                (Self::Char(left), Self::Char(right)) => {
                    let order = left.cmp(right);
                    if !order.is_eq() {
                        return order;
                    }
                }
                (Self::Tagged(left_tag, left), Self::Tagged(right_tag, right)) => {
                    let order = left_tag.cmp(right_tag);
                    if !order.is_eq() {
                        return order;
                    }
                    pending.push(Task::Values(left, right));
                }
                _ => unreachable!("equal query-value ranks must have matching variants"),
            }
        }
        Ordering::Equal
    }

    /// Move a result container out while retaining stack-safe destruction of
    /// any other value. Borrowed pattern matching remains available as usual.
    pub fn into_map(mut self) -> Option<Vec<(Self, Self)>> {
        if let Self::Map(values) = &mut self {
            Some(std::mem::take(values))
        } else {
            None
        }
    }
    pub fn into_collection(mut self) -> Option<Vec<Self>> {
        if let Self::Collection(values) = &mut self {
            Some(std::mem::take(values))
        } else {
            None
        }
    }
    pub fn into_set(mut self) -> Option<Vec<Self>> {
        if let Self::Set(values) = &mut self {
            Some(std::mem::take(values))
        } else {
            None
        }
    }
    pub fn into_tuple(mut self) -> Option<Vec<Self>> {
        if let Self::Tuple(values) = &mut self {
            Some(std::mem::take(values))
        } else {
            None
        }
    }
    pub fn into_scalar(mut self) -> Option<Value> {
        if let Self::Scalar(value) = &mut self {
            Some(std::mem::replace(value, Value::Bool(false)))
        } else {
            None
        }
    }

    /// Insert or replace one map entry and restore canonical key order. Pull
    /// maps are semantically unordered, so neither keyword-only ordering nor
    /// selector order may leak into equality or returned representation.
    pub(crate) fn put_map_entry(
        entries: &mut Vec<(QueryValue, QueryValue)>,
        key: QueryValue,
        value: QueryValue,
    ) {
        if let Some((_, existing)) = entries
            .iter_mut()
            .find(|(candidate, _)| candidate.canonical_cmp(&key).is_eq())
        {
            *existing = value;
        } else {
            entries.push((key, value));
            entries.sort_by(|(left, _), (right, _)| left.canonical_cmp(right));
        }
    }
}

impl Clone for QueryValue {
    fn clone(&self) -> Self {
        enum Task<'a> {
            Value(&'a QueryValue),
            Collection(usize),
            Tuple(usize),
            Map(usize),
            Set(usize),
            Tagged(&'a Symbol),
        }
        let mut pending = vec![Task::Value(self)];
        let mut values = Vec::new();
        while let Some(task) = pending.pop() {
            match task {
                Task::Value(Self::Nil) => values.push(Self::Nil),
                Task::Value(Self::Char(value)) => values.push(Self::Char(*value)),
                Task::Value(Self::Tagged(tag, value)) => {
                    pending.push(Task::Tagged(tag));
                    pending.push(Task::Value(value));
                }
                Task::Value(Self::Set(children)) => {
                    pending.push(Task::Set(children.len()));
                    pending.extend(children.iter().rev().map(Task::Value));
                }
                Task::Value(Self::Scalar(value)) => values.push(Self::Scalar(value.clone())),
                Task::Value(Self::Collection(children)) | Task::Value(Self::Tuple(children)) => {
                    pending.push(if matches!(task, Task::Value(Self::Collection(_))) {
                        Task::Collection(children.len())
                    } else {
                        Task::Tuple(children.len())
                    });
                    pending.extend(children.iter().rev().map(Task::Value));
                }
                Task::Value(Self::Map(entries)) => {
                    pending.push(Task::Map(entries.len()));
                    for (key, value) in entries.iter().rev() {
                        pending.push(Task::Value(value));
                        pending.push(Task::Value(key));
                    }
                }
                Task::Tagged(tag) => {
                    let child = values.pop().expect("tag payload");
                    values.push(Self::Tagged(tag.clone(), Box::new(child)));
                }
                Task::Set(count) => {
                    let children = values.split_off(values.len() - count);
                    values.push(Self::Set(children));
                }
                Task::Collection(count) => {
                    let children = values.split_off(values.len() - count);
                    values.push(Self::Collection(children));
                }
                Task::Tuple(count) => {
                    let children = values.split_off(values.len() - count);
                    values.push(Self::Tuple(children));
                }
                Task::Map(count) => {
                    let mut children = values.split_off(values.len() - 2 * count).into_iter();
                    let entries = (0..count)
                        .map(|_| (children.next().unwrap(), children.next().unwrap()))
                        .collect();
                    values.push(Self::Map(entries));
                }
            }
        }
        values.pop().expect("one cloned result")
    }
}

impl Drop for QueryValue {
    fn drop(&mut self) {
        fn drain(value: &mut QueryValue, pending: &mut Vec<QueryValue>) {
            match value {
                QueryValue::Collection(values)
                | QueryValue::Tuple(values)
                | QueryValue::Set(values) => pending.append(values),
                QueryValue::Tagged(_, value) => {
                    if !matches!(value.as_ref(), QueryValue::Nil) {
                        pending.push(std::mem::replace(value.as_mut(), QueryValue::Nil));
                    }
                }
                QueryValue::Map(entries) => {
                    for (key, value) in std::mem::take(entries) {
                        pending.push(key);
                        pending.push(value);
                    }
                }
                _ => {}
            }
        }
        let mut pending = Vec::new();
        drain(self, &mut pending);
        while let Some(mut value) = pending.pop() {
            drain(&mut value, &mut pending);
        }
    }
}

impl PartialEq for QueryValue {
    fn eq(&self, other: &Self) -> bool {
        self.canonical_cmp(other).is_eq()
    }
}

impl Eq for QueryValue {}

/// Conservative owned-value retention, not process RSS. Capacities count even
/// when a set has duplicate elements or a container has spare allocation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct QueryValueSize {
    pub nodes: usize,
    pub retained_bytes: usize,
    pub depth: usize,
    /// Conservative per-operation canonical arena/order/hash scratch proxy.
    /// Independent of owned input retention; not measured allocator bytes.
    pub canonical_bytes: usize,
}

impl QueryValue {
    pub(crate) fn retained_bytes(&self) -> usize {
        self.measure_with(&mut |_| Ok(()))
            .expect("infallible measurement")
            .retained_bytes
    }

    /// Measure before cloning/canonicalizing caller-owned data. Traversal uses
    /// one continuation per nesting level rather than enqueueing all siblings.
    /// Every visited query/stored node is charged through the caller's shared
    /// work/deadline/cancellation policy. The caller then admits retained_bytes
    /// before validate_with allocates its bounded canonical arena.
    pub(crate) fn measure_with(
        &self,
        check: &mut impl FnMut(usize) -> Result<(), SemanticError>,
    ) -> Result<QueryValueSize, SemanticError> {
        use std::mem::size_of;
        enum Frame<'a> {
            Query(&'a QueryValue, usize),
            Stored(&'a Value, usize),
            Sequence(std::slice::Iter<'a, QueryValue>, usize),
            Map(std::slice::Iter<'a, (QueryValue, QueryValue)>, usize),
            StoredSequence(std::slice::Iter<'a, Option<Value>>, usize),
        }
        let mut size = QueryValueSize {
            retained_bytes: size_of::<Self>(),
            ..Default::default()
        };
        let mut pending = vec![Frame::Query(self, 1)];
        while let Some(frame) = pending.pop() {
            let (node_depth, added) = match frame {
                Frame::Sequence(mut values, depth) => {
                    if let Some(value) = values.next() {
                        pending.push(Frame::Sequence(values, depth));
                        pending.push(Frame::Query(value, depth));
                    }
                    continue;
                }
                Frame::Map(mut entries, depth) => {
                    if let Some((key, value)) = entries.next() {
                        pending.push(Frame::Map(entries, depth));
                        pending.push(Frame::Query(value, depth));
                        pending.push(Frame::Query(key, depth));
                    }
                    continue;
                }
                Frame::StoredSequence(mut values, depth) => {
                    if let Some(value) = values.next() {
                        pending.push(Frame::StoredSequence(values, depth));
                        if let Some(value) = value {
                            pending.push(Frame::Stored(value, depth));
                        } else {
                            check(1)?;
                            size.nodes = size.nodes.saturating_add(1);
                            size.depth = size.depth.max(depth);
                        }
                    }
                    continue;
                }
                Frame::Query(value, depth) => {
                    check(1)?;
                    let bytes = match value {
                        Self::Nil | Self::Char(_) => 0,
                        Self::Scalar(value) => {
                            pending.push(Frame::Stored(value, depth));
                            0
                        }
                        Self::Tuple(values) | Self::Collection(values) | Self::Set(values) => {
                            pending.push(Frame::Sequence(values.iter(), depth.saturating_add(1)));
                            values.capacity().saturating_mul(size_of::<Self>())
                        }
                        Self::Map(entries) => {
                            pending.push(Frame::Map(entries.iter(), depth.saturating_add(1)));
                            entries.capacity().saturating_mul(size_of::<(Self, Self)>())
                        }
                        Self::Tagged(tag, value) => {
                            pending.push(Frame::Query(value, depth.saturating_add(1)));
                            size_of::<Self>()
                                .saturating_add(tag.name.capacity())
                                .saturating_add(tag.namespace.as_ref().map_or(0, String::capacity))
                        }
                    };
                    (depth, bytes)
                }
                Frame::Stored(value, depth) => {
                    check(1)?;
                    let bytes = match value {
                        Value::Tuple(values) => {
                            pending.push(Frame::StoredSequence(
                                values.iter(),
                                depth.saturating_add(1),
                            ));
                            values.capacity().saturating_mul(size_of::<Option<Value>>())
                        }
                        _ => usize::try_from(value.retained_heap_bytes()).unwrap_or(usize::MAX),
                    };
                    (depth, bytes)
                }
            };
            size.nodes = size.nodes.saturating_add(1);
            size.depth = size.depth.max(node_depth);
            size.retained_bytes = size.retained_bytes.saturating_add(added);
        }
        size.canonical_bytes = match self {
            Self::Nil => 0,
            Self::Scalar(value) if !matches!(value, Value::Tuple(_)) => 0,
            _ => size
                .nodes
                .saturating_mul(2 * size_of::<Node<'_>>() + 32 * size_of::<usize>()),
        };
        Ok(size)
    }

    /// Reject ambiguous maps after retention admission. Sets deduplicate under
    /// logical equality when compared/hashed, so duplicates are valid data.
    pub(crate) fn validate_with(
        &self,
        check: &mut impl FnMut(usize) -> Result<(), SemanticError>,
    ) -> Result<(), SemanticError> {
        if matches!(self, Self::Nil | Self::Scalar(_) | Self::Char(_)) {
            return check(1);
        }
        let mut arena = Vec::new();
        build_node(QueryValueRef::Query(self), &mut arena, false, true, check)?;
        Ok(())
    }
}

// Canonical child ordering is computed bottom-up. Sorting nested unordered
// values must never recursively enter QueryValue comparison on the Rust stack.
enum Node<'a> {
    Nil,
    Scalar(&'a Value),
    Tuple(Vec<usize>),
    Collection(Vec<usize>),
    Map(Vec<(usize, usize)>),
    Set(Vec<usize>),
    Char(char),
    Tagged(&'a Symbol, usize),
    StoredTuple(Vec<usize>),
}

#[derive(Clone, Copy)]
pub(crate) enum QueryValueRef<'a> {
    Nil,
    Stored(&'a Value),
    Query(&'a QueryValue),
}

impl QueryValueRef<'_> {
    fn kind(
        self,
        check: &mut impl FnMut(usize) -> Result<(), SemanticError>,
    ) -> Result<u8, SemanticError> {
        check(1)?;
        Ok(match self {
            Self::Nil | Self::Query(QueryValue::Nil) => 0,
            Self::Stored(_) | Self::Query(QueryValue::Scalar(_)) => 1,
            Self::Query(value @ QueryValue::Tuple(_)) if stored_tuple(value, check)? => 1,
            Self::Query(_) => 2,
        })
    }

    /// Join-key ordering follows BoundValue normalization, without cloning a
    /// borrowed relation cell or allocating ordinary scalar keys.
    pub(crate) fn logical_cmp(self, other: Self) -> std::cmp::Ordering {
        self.logical_cmp_with(other, &mut |_| Ok(()))
            .expect("infallible comparison")
    }

    pub(crate) fn logical_cmp_with(
        self,
        other: Self,
        check: &mut impl FnMut(usize) -> Result<(), SemanticError>,
    ) -> Result<std::cmp::Ordering, SemanticError> {
        let left_kind = self.kind(check)?;
        let right_kind = other.kind(check)?;
        let order = left_kind.cmp(&right_kind);
        if !order.is_eq() {
            return Ok(order);
        }
        if left_kind == 0 {
            return Ok(std::cmp::Ordering::Equal);
        }
        let scalar = |value| match value {
            Self::Stored(value) | Self::Query(QueryValue::Scalar(value)) => Some(value),
            _ => None,
        };
        if let (Some(left), Some(right)) = (scalar(self), scalar(other))
            && !matches!(left, Value::Tuple(_))
            && !matches!(right, Value::Tuple(_))
        {
            return Ok(left.index_cmp(right));
        }
        let mut arena = Vec::new();
        let left = build_node(self, &mut arena, left_kind == 1, false, check)?;
        let right = build_node(other, &mut arena, right_kind == 1, false, check)?;
        compare_nodes(&arena, left, right, check)
    }

    pub(crate) fn logical_hash_with(
        self,
        state: &mut impl Hasher,
        check: &mut impl FnMut(usize) -> Result<(), SemanticError>,
    ) -> Result<(), SemanticError> {
        let kind = self.kind(check)?;
        kind.hash(state);
        if kind == 0 {
            0u8.hash(state);
            return Ok(());
        }
        let scalar = match self {
            Self::Stored(value) | Self::Query(QueryValue::Scalar(value)) => Some(value),
            _ => None,
        };
        if let Some(value) = scalar
            && !matches!(value, Value::Tuple(_))
        {
            1u8.hash(state);
            value.logical_hash(state);
            return Ok(());
        }
        let mut arena = Vec::new();
        let root = build_node(self, &mut arena, kind == 1, false, check)?;
        let mut pending = vec![root];
        while let Some(index) = pending.pop() {
            check(1)?;
            node_rank(&arena[index]).hash(state);
            match &arena[index] {
                Node::Nil => {}
                Node::Scalar(value) => value.logical_hash(state),
                Node::Char(value) => value.hash(state),
                Node::Tagged(tag, value) => {
                    tag.hash(state);
                    pending.push(*value);
                }
                Node::Tuple(values)
                | Node::Collection(values)
                | Node::Set(values)
                | Node::StoredTuple(values) => {
                    values.len().hash(state);
                    pending.extend(values.iter().rev().copied());
                }
                Node::Map(entries) => {
                    entries.len().hash(state);
                    for (key, value) in entries.iter().rev() {
                        pending.push(*value);
                        pending.push(*key);
                    }
                }
            }
        }
        Ok(())
    }
}

fn stored_tuple(
    value: &QueryValue,
    check: &mut impl FnMut(usize) -> Result<(), SemanticError>,
) -> Result<bool, SemanticError> {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        check(1)?;
        match value {
            QueryValue::Nil | QueryValue::Scalar(_) => {}
            QueryValue::Tuple(values) => {
                check(values.len())?;
                pending.extend(values);
            }
            _ => return Ok(false),
        }
    }
    Ok(true)
}

fn compare_unordered(left: &QueryValue, right: &QueryValue) -> std::cmp::Ordering {
    let mut arena = Vec::new();
    let mut check = |_| Ok(());
    let left = build_node(
        QueryValueRef::Query(left),
        &mut arena,
        false,
        false,
        &mut check,
    )
    .unwrap();
    let right = build_node(
        QueryValueRef::Query(right),
        &mut arena,
        false,
        false,
        &mut check,
    )
    .unwrap();
    compare_nodes(&arena, left, right, &mut check).unwrap()
}

fn build_node<'a>(
    value: QueryValueRef<'a>,
    arena: &mut Vec<Node<'a>>,
    normalized: bool,
    validate_maps: bool,
    check: &mut impl FnMut(usize) -> Result<(), SemanticError>,
) -> Result<usize, SemanticError> {
    enum Task<'a> {
        Value(QueryValueRef<'a>),
        Sequence(u8, usize),
        Map(usize),
        Tagged(&'a Symbol),
    }
    let mut pending = vec![Task::Value(value)];
    let mut completed = Vec::new();
    while let Some(task) = pending.pop() {
        check(1)?;
        let node = match task {
            Task::Value(QueryValueRef::Nil | QueryValueRef::Query(QueryValue::Nil)) => Node::Nil,
            Task::Value(
                QueryValueRef::Stored(Value::Tuple(values))
                | QueryValueRef::Query(QueryValue::Scalar(Value::Tuple(values))),
            ) if normalized => {
                pending.push(Task::Sequence(8, values.len()));
                check(values.len())?;
                pending.extend(values.iter().rev().map(|value| {
                    Task::Value(
                        value
                            .as_ref()
                            .map_or(QueryValueRef::Nil, QueryValueRef::Stored),
                    )
                }));
                continue;
            }
            Task::Value(
                QueryValueRef::Stored(value) | QueryValueRef::Query(QueryValue::Scalar(value)),
            ) => Node::Scalar(value),
            Task::Value(QueryValueRef::Query(QueryValue::Char(value))) => Node::Char(*value),
            Task::Value(QueryValueRef::Query(QueryValue::Tagged(tag, value))) => {
                pending.push(Task::Tagged(tag));
                pending.push(Task::Value(QueryValueRef::Query(value)));
                continue;
            }
            Task::Value(QueryValueRef::Query(
                value @ (QueryValue::Tuple(_) | QueryValue::Collection(_) | QueryValue::Set(_)),
            )) => {
                let (rank, values) = match value {
                    QueryValue::Tuple(values) => (if normalized { 8 } else { 2 }, values),
                    QueryValue::Collection(values) => (3, values),
                    QueryValue::Set(values) => (5, values),
                    _ => unreachable!(),
                };
                pending.push(Task::Sequence(rank, values.len()));
                check(values.len())?;
                pending.extend(
                    values
                        .iter()
                        .rev()
                        .map(|value| Task::Value(QueryValueRef::Query(value))),
                );
                continue;
            }
            Task::Value(QueryValueRef::Query(QueryValue::Map(entries))) => {
                pending.push(Task::Map(entries.len()));
                check(entries.len().saturating_mul(2))?;
                for (key, value) in entries.iter().rev() {
                    pending.push(Task::Value(QueryValueRef::Query(value)));
                    pending.push(Task::Value(QueryValueRef::Query(key)));
                }
                continue;
            }
            Task::Tagged(tag) => Node::Tagged(tag, completed.pop().expect("tag child")),
            Task::Sequence(rank, count) => {
                let mut values = completed.split_off(completed.len() - count);
                if rank == 5 {
                    let mut failure = None;
                    values.sort_unstable_by(|left, right| {
                        if failure.is_some() {
                            return std::cmp::Ordering::Equal;
                        }
                        compare_nodes(arena, *left, *right, check).unwrap_or_else(|error| {
                            failure = Some(error);
                            std::cmp::Ordering::Equal
                        })
                    });
                    if let Some(error) = failure {
                        return Err(error);
                    }
                    let mut unique = Vec::with_capacity(values.len());
                    for value in values {
                        if let Some(prior) = unique.last()
                            && compare_nodes(arena, *prior, value, check)?.is_eq()
                        {
                            continue;
                        }
                        unique.push(value);
                    }
                    values = unique;
                }
                match rank {
                    2 => Node::Tuple(values),
                    3 => Node::Collection(values),
                    5 => Node::Set(values),
                    8 => Node::StoredTuple(values),
                    _ => unreachable!(),
                }
            }
            Task::Map(count) => {
                let children = completed.split_off(completed.len() - 2 * count);
                let mut entries = children
                    .chunks_exact(2)
                    .map(|pair| (pair[0], pair[1]))
                    .collect::<Vec<_>>();
                let mut failure = None;
                entries.sort_by(|(lk, lv), (rk, rv)| {
                    if failure.is_some() {
                        return std::cmp::Ordering::Equal;
                    }
                    let result = compare_nodes(arena, *lk, *rk, check).and_then(|order| {
                        if order.is_eq() {
                            compare_nodes(arena, *lv, *rv, check)
                        } else {
                            Ok(order)
                        }
                    });
                    result.unwrap_or_else(|error| {
                        failure = Some(error);
                        std::cmp::Ordering::Equal
                    })
                });
                if let Some(error) = failure {
                    return Err(error);
                }
                if validate_maps {
                    for pair in entries.windows(2) {
                        if compare_nodes(arena, pair[0].0, pair[1].0, check)?.is_eq() {
                            return Err(SemanticError::incorrect(
                                "query/duplicate-map-key",
                                "query maps require distinct logical keys",
                            ));
                        }
                    }
                }
                Node::Map(entries)
            }
        };
        completed.push(arena.len());
        arena.push(node);
    }
    Ok(completed.pop().expect("one canonical value root"))
}

fn node_rank(node: &Node<'_>) -> u8 {
    match node {
        Node::Nil => 0,
        Node::Scalar(_) => 1,
        Node::Tuple(_) => 2,
        Node::Collection(_) => 3,
        Node::Map(_) => 4,
        Node::Set(_) => 5,
        Node::Char(_) => 6,
        Node::Tagged(_, _) => 7,
        Node::StoredTuple(_) => 8,
    }
}

fn compare_nodes(
    arena: &[Node<'_>],
    left: usize,
    right: usize,
    check: &mut impl FnMut(usize) -> Result<(), SemanticError>,
) -> Result<std::cmp::Ordering, SemanticError> {
    use std::cmp::Ordering;
    enum Task {
        Nodes(usize, usize),
        Length(usize, usize),
    }
    let mut pending = vec![Task::Nodes(left, right)];
    while let Some(task) = pending.pop() {
        check(1)?;
        let (left, right) = match task {
            Task::Length(left, right) => {
                let order = left.cmp(&right);
                if !order.is_eq() {
                    return Ok(order);
                }
                continue;
            }
            Task::Nodes(left, right) if left == right => continue,
            Task::Nodes(left, right) => (&arena[left], &arena[right]),
        };
        // Stored tuple keys retain Value's cross-type rank, not QueryValue's
        // container-shape rank. No tuple allocation or recursion is needed.
        let order = match (left, right) {
            (Node::StoredTuple(_), Node::Scalar(right)) => {
                Value::Tuple(Vec::new()).index_cmp(right)
            }
            (Node::Scalar(left), Node::StoredTuple(_)) => left.index_cmp(&Value::Tuple(Vec::new())),
            _ => node_rank(left).cmp(&node_rank(right)),
        };
        if !order.is_eq() {
            return Ok(order);
        }
        match (left, right) {
            (Node::Nil, Node::Nil) => {}
            (Node::Scalar(left), Node::Scalar(right)) => {
                let order = left.index_cmp(right);
                if !order.is_eq() {
                    return Ok(order);
                }
            }
            (Node::Char(left), Node::Char(right)) => {
                let order = left.cmp(right);
                if !order.is_eq() {
                    return Ok(order);
                }
            }
            (Node::Tagged(lt, left), Node::Tagged(rt, right)) => {
                let order = lt.cmp(rt);
                if !order.is_eq() {
                    return Ok(order);
                }
                pending.push(Task::Nodes(*left, *right));
            }
            (Node::Tuple(left), Node::Tuple(right))
            | (Node::Collection(left), Node::Collection(right))
            | (Node::Set(left), Node::Set(right))
            | (Node::StoredTuple(left), Node::StoredTuple(right)) => {
                pending.push(Task::Length(left.len(), right.len()));
                pending.extend(
                    left.iter()
                        .zip(right)
                        .rev()
                        .map(|(left, right)| Task::Nodes(*left, *right)),
                );
            }
            (Node::Map(left), Node::Map(right)) => {
                pending.push(Task::Length(left.len(), right.len()));
                for ((lk, lv), (rk, rv)) in left.iter().zip(right).rev() {
                    pending.push(Task::Nodes(*lv, *rv));
                    pending.push(Task::Nodes(*lk, *rk));
                }
            }
            _ => unreachable!("equal canonical ranks have matching nodes"),
        }
    }
    Ok(Ordering::Equal)
}
