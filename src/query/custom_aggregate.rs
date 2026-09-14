//! Borrowed grouped callbacks, separate from row functions and built-in reducers.
use super::*;
use std::cell::RefCell;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AggregateArg {
    Variable(Variable),
    Constant(QueryValue),
    Source(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AggregateCall {
    pub name: String,
    pub args: Vec<AggregateArg>,
}

impl AggregateCall {
    pub fn new(name: impl Into<String>, args: Vec<AggregateArg>) -> Self {
        Self {
            name: name.into(),
            args,
        }
    }
}

/// One borrowed exact cell. General query values are never narrowed to stored
/// values merely to pass them to an aggregate.
#[derive(Clone, Copy, Debug)]
pub enum AggregateValue<'a> {
    Nil,
    Stored(&'a Value),
    Query(&'a QueryValue),
}

/// An explicitly supplied immutable source. A callback can inspect the exact
/// database basis or consume tuple/relation data without a hidden default DB.
#[derive(Clone, Copy)]
pub enum AggregateSource<'a> {
    Database(&'a DatabaseValue),
    Tuples(&'a [Vec<Value>]),
    Relation(&'a [Vec<QueryValue>]),
    Log(&'a crate::LogValue),
}

/// Aligned rows from the query's distinct find/:with basis. `:with` values keep
/// otherwise identical aggregate rows distinct. A source or constant argument
/// does not create another column or change that multiplicity.
///
/// Accessors borrow input data; they do not execute user code or clone cells.
/// Trusted callbacks must cooperate through `check` during substantial work.
/// The engine checks before/after invocation, but cannot preempt Rust code.
pub struct AggregateGroup<'a> {
    args: &'a [AggregateArg],
    rows: &'a [&'a Vec<BoundValue>],
    positions: &'a [Option<usize>],
    sources: &'a BTreeMap<&'a str, SourceRef<'a>>,
    check: &'a dyn Fn(usize) -> Result<(), SemanticError>,
}

impl<'a> AggregateGroup<'a> {
    pub fn len(&self) -> usize {
        self.rows.len()
    }
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
    pub fn args(&self) -> &[AggregateArg] {
        self.args
    }

    pub fn check(&self, work: usize) -> Result<(), SemanticError> {
        (self.check)(work)
    }

    pub fn value(&self, row: usize, arg: usize) -> Option<AggregateValue<'a>> {
        let values = *self.rows.get(row)?;
        match self.args.get(arg)? {
            AggregateArg::Variable(_) => Some(match &values[self.positions[arg]?] {
                BoundValue::Nil => AggregateValue::Nil,
                BoundValue::Stored(value) => AggregateValue::Stored(value),
                BoundValue::Query(value) => AggregateValue::Query(value),
            }),
            AggregateArg::Constant(value) => Some(AggregateValue::Query(value)),
            AggregateArg::Source(_) => None,
        }
    }

    /// Iterate an argument's values in the same row order as every other column.
    /// Constants repeat per row; sources have no value column.
    pub fn column(
        &self,
        arg: usize,
    ) -> Option<impl ExactSizeIterator<Item = AggregateValue<'a>> + '_> {
        if matches!(self.args.get(arg)?, AggregateArg::Source(_)) {
            return None;
        }
        Some(
            (0..self.len())
                .map(move |row| self.value(row, arg).expect("validated aggregate column")),
        )
    }

    pub fn constant(&self, arg: usize) -> Option<&'a QueryValue> {
        match self.args.get(arg)? {
            AggregateArg::Constant(value) => Some(value),
            _ => None,
        }
    }

    pub fn source(&self, arg: usize) -> Option<AggregateSource<'a>> {
        let AggregateArg::Source(name) = self.args.get(arg)? else {
            return None;
        };
        Some(match self.sources.get(name.as_str())? {
            SourceRef::Database(value) => AggregateSource::Database(value),
            SourceRef::Tuples(value) => AggregateSource::Tuples(value),
            SourceRef::Relation(value) => AggregateSource::Relation(value),
            SourceRef::Log(value) => AggregateSource::Log(value),
        })
    }
}

pub(super) type NativeAggregate =
    dyn Fn(&AggregateGroup<'_>, &QueryControl) -> Result<QueryValue, SemanticError> + Send + Sync;

pub(super) struct Context<'a> {
    pub extensions: Option<&'a QueryExtensions>,
    pub control: &'a QueryControl,
    pub deadline: Option<Instant>,
}

pub(super) fn validate(
    call: &AggregateCall,
    extensions: Option<&QueryExtensions>,
) -> Result<(), SemanticError> {
    if extensions.is_some_and(|extensions| extensions.aggregates.contains_key(&call.name)) {
        Ok(())
    } else {
        Err(SemanticError::incorrect(
            "query/unknown-aggregate",
            format!("aggregate {} is not registered", call.name),
        ))
    }
}

pub(super) fn invoke(
    call: &AggregateCall,
    rows: &[&Vec<BoundValue>],
    basis: &[Variable],
    sources: &BTreeMap<&str, SourceRef<'_>>,
    budget: &mut QueryPullBudget<'_>,
    context: &Context<'_>,
) -> Result<QueryValue, SemanticError> {
    validate(call, context.extensions)?;
    budget.check(1)?;
    budget.charge_value_bytes(
        call.args
            .len()
            .saturating_mul(std::mem::size_of::<Option<usize>>()),
    )?;
    let positions = call
        .args
        .iter()
        .map(|arg| match arg {
            AggregateArg::Variable(variable) => basis
                .iter()
                .position(|candidate| candidate == variable)
                .map(Some)
                .ok_or_else(|| {
                    SemanticError::incorrect(
                        "query/unbound-aggregate-variable",
                        format!("aggregate variable {} is unbound", variable.name()),
                    )
                }),
            _ => Ok(None),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let control = QueryControl {
        timeout: context
            .deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now())),
        max_work: context.control.max_work.saturating_sub(budget.work()),
        max_value_bytes: budget.remaining_value_bytes(),
        ..context.control.clone()
    };
    let callback = &context.extensions.expect("validated registry").aggregates[&call.name];
    let result = {
        let shared = RefCell::new(&mut *budget);
        let check = |work| shared.borrow_mut().check(work);
        let group = AggregateGroup {
            args: &call.args,
            rows,
            positions: &positions,
            sources,
            check: &check,
        };
        catch_unwind(AssertUnwindSafe(|| callback(&group, &control))).map_err(|_| {
            fault(
                "query/local-aggregate-panicked",
                "a local aggregate panicked",
            )
        })?
    };
    // A callback can return after cancellation or exceeding its deadline, and
    // cannot bypass those controls by returning a successful value.
    budget.check(0)?;
    let value = result?;
    let size = value.measure_with(&mut |work| budget.check(work))?;
    budget.charge_value_bytes(size.retained_bytes.saturating_add(size.canonical_bytes))?;
    value.validate_with(&mut |work| budget.check(work))?;
    Ok(value)
}
