//! Conjunctive range hints, not rewritten predicates. Raw AVET boundaries use
//! the same logical comparator as query comparisons; original predicate clauses
//! remain authoritative, including arity/type/binding diagnostics.
use super::*;
use crate::{
    DatabaseValuePrefixCursor, DatabaseValueScanCursor, Datom, IndexBoundary, IndexComponents,
};

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct Bound {
    value: BoundValue,
    inclusive: bool,
}

impl Bound {
    fn stored(&self) -> &Value {
        self.value
            .stored()
            .expect("range endpoints are stored scalars")
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd)]
pub(super) struct Span {
    lower: Option<Bound>,
    upper: Option<Bound>,
}

impl Span {
    fn retained_bytes(&self) -> usize {
        [&self.lower, &self.upper]
            .into_iter()
            .flatten()
            .fold(std::mem::size_of::<Self>(), |bytes, bound| {
                bytes.saturating_add(join::bound_bytes(&bound.value))
            })
    }

    fn lower(&mut self, bound: Bound) {
        if self.lower.as_ref().is_none_or(|old| {
            old.value < bound.value || (old.value.cmp(&bound.value).is_eq() && !bound.inclusive)
        }) {
            self.lower = Some(bound);
        }
    }

    fn upper(&mut self, bound: Bound) {
        if self.upper.as_ref().is_none_or(|old| {
            old.value > bound.value || (old.value.cmp(&bound.value).is_eq() && !bound.inclusive)
        }) {
            self.upper = Some(bound);
        }
    }

    fn empty(&self) -> bool {
        self.lower
            .as_ref()
            .zip(self.upper.as_ref())
            .is_some_and(|(lower, upper)| {
                let order = lower.value.cmp(&upper.value);
                order.is_gt() || (order.is_eq() && !(lower.inclusive && upper.inclusive))
            })
    }

    fn above_lower(&self, value: &Value) -> bool {
        self.lower.as_ref().is_none_or(|lower| {
            let order = value.index_cmp(lower.stored());
            order.is_gt() || (order.is_eq() && lower.inclusive)
        })
    }

    fn below_upper(&self, value: &Value) -> bool {
        self.upper.as_ref().is_none_or(|upper| {
            let order = value.index_cmp(upper.stored());
            order.is_lt() || (order.is_eq() && upper.inclusive)
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(super) struct Range {
    spans: Vec<Span>,
}

impl Range {
    pub(super) fn retained_bytes(&self) -> usize {
        self.spans
            .iter()
            .fold(std::mem::size_of::<Self>(), |bytes, span| {
                bytes.saturating_add(span.retained_bytes())
            })
    }
}

pub(super) fn constraints(
    pattern: &DataPattern,
    conjunction: &[Clause],
    row: &Row,
    state: &mut State<'_>,
) -> Result<Option<Range>, SemanticError> {
    let Term::Variable(variable) = &pattern.value else {
        return Ok(None);
    };
    let mut range = Span::default();
    let mut excluded = BTreeSet::new();
    for clause in conjunction {
        state.check(1)?;
        let Clause::Predicate {
            predicate, args, ..
        } = clause
        else {
            continue;
        };
        // Do not let a hint empty the input before an ill-formed comparison's
        // ordinary evaluator reports its arity error.
        let [left, right] = args.as_slice() else {
            return Ok(None);
        };
        let (predicate, endpoint) = if matches!(left, Term::Variable(candidate) if candidate == variable)
        {
            (*predicate, right)
        } else if matches!(right, Term::Variable(candidate) if candidate == variable) {
            let reverse = match predicate {
                Predicate::Less => Predicate::Greater,
                Predicate::LessOrEqual => Predicate::GreaterOrEqual,
                Predicate::Greater => Predicate::Less,
                Predicate::GreaterOrEqual => Predicate::LessOrEqual,
                other => *other,
            };
            (reverse, left)
        } else {
            continue;
        };
        if !matches!(
            predicate,
            Predicate::Eq
                | Predicate::NotEq
                | Predicate::Less
                | Predicate::LessOrEqual
                | Predicate::Greater
                | Predicate::GreaterOrEqual
        ) {
            continue;
        }
        let Some(BoundValue::Stored(value)) = term_bound_value(endpoint, row) else {
            continue;
        };
        // Ref comparison operands are numeric, including arbitrary u64 values;
        // raw index APIs validate actual EIDs. Preserve comparison semantics by
        // declining an unusable raw boundary, never inventing an EID/coercion.
        if IndexBoundary::Avet(IndexComponents::Two(0, value.clone()))
            .validate()
            .is_err()
        {
            continue;
        }
        let bound = Bound {
            value: BoundValue::Stored(value),
            inclusive: matches!(
                predicate,
                Predicate::Eq | Predicate::LessOrEqual | Predicate::GreaterOrEqual
            ),
        };
        state.charge_value_bytes(join::bound_bytes(&bound.value))?;
        match predicate {
            Predicate::Greater | Predicate::GreaterOrEqual => range.lower(bound),
            Predicate::Less | Predicate::LessOrEqual => range.upper(bound),
            Predicate::Eq => {
                state.charge_value_bytes(join::bound_bytes(&bound.value))?;
                range.lower(bound.clone());
                range.upper(bound);
            }
            Predicate::NotEq => {
                state.charge_value_bytes(
                    std::mem::size_of::<BoundValue>() + 3 * std::mem::size_of::<usize>(),
                )?;
                excluded.insert(bound.value);
            }
            _ => unreachable!("only comparison predicates contribute ranges"),
        }
    }
    if range.lower.is_none() && range.upper.is_none() && excluded.is_empty() {
        return Ok(None);
    }
    let mut spans = Vec::new();
    if !range.empty() {
        // Subtract each distinct excluded logical value. Each resulting span
        // seeks independently, so a million equal excluded facts remain one
        // index boundary rather than a million candidates to discard.
        let mut lower = range.lower.clone();
        for value in excluded {
            state.check(1)?;
            let stored = value.stored().expect("stored excluded value");
            if !range.above_lower(stored) || !range.below_upper(stored) {
                continue;
            }
            let before = Span {
                lower,
                upper: Some(Bound {
                    value: value.clone(),
                    inclusive: false,
                }),
            };
            state.charge_value_bytes(before.retained_bytes())?;
            if !before.empty() {
                spans.push(before);
            }
            lower = Some(Bound {
                value,
                inclusive: false,
            });
        }
        let tail = Span {
            lower,
            upper: range.upper,
        };
        state.charge_value_bytes(tail.retained_bytes())?;
        if !tail.empty() {
            spans.push(tail);
        }
    }
    Ok(Some(Range { spans }))
}

pub(super) enum Datoms<'a> {
    Prefix(Box<DatabaseValuePrefixCursor<'a>>),
    Scan(Box<DatabaseValueScanCursor<'a>>),
    Range(Box<RangeDatoms<'a>>),
    Empty,
}

// One allocation owns the range state and its inline cursor for the complete
// probe. Moving to another span replaces that cursor without allocating a new
// cursor box; individual datoms are never boxed here.
pub(super) struct RangeDatoms<'a> {
    database: &'a DatabaseValue,
    cursor: DatabaseValueScanCursor<'a>,
    attribute: u32,
    source: Option<u64>,
    span: Span,
    remaining: std::vec::IntoIter<Span>,
}

impl<'a> Datoms<'a> {
    pub(super) fn prefix(cursor: DatabaseValuePrefixCursor<'a>) -> Self {
        Self::Prefix(Box::new(cursor))
    }
    pub(super) fn scan(cursor: DatabaseValueScanCursor<'a>) -> Self {
        Self::Scan(Box::new(cursor))
    }

    pub(super) fn range(
        database: &'a DatabaseValue,
        attribute: u32,
        range: Range,
        source: Option<u64>,
    ) -> Result<Self, SemanticError> {
        let mut remaining = range.spans.into_iter();
        let Some(span) = remaining.next() else {
            return Ok(Self::Empty);
        };
        Ok(Self::Range(Box::new(RangeDatoms {
            database,
            cursor: start_cursor(database, attribute, &span, source)?,
            attribute,
            source,
            span,
            remaining,
        })))
    }

    pub(super) fn initial_seeks(&self) -> u64 {
        u64::from(matches!(self, Self::Prefix(_) | Self::Range(_)))
    }

    pub(super) fn next(&mut self, state: &mut State<'_>) -> Option<Result<Datom, SemanticError>> {
        match self {
            Self::Prefix(cursor) => cursor.next_with_control(&mut |datom| examine(datom, state)),
            Self::Scan(cursor) => cursor.next_with_control(&mut |datom| examine(datom, state)),
            Self::Empty => None,
            Self::Range(range) => {
                let RangeDatoms {
                    database,
                    cursor,
                    attribute,
                    source,
                    span,
                    remaining,
                } = range.as_mut();
                loop {
                    // Stop before custom/temporal filters can consume an arbitrary
                    // suffix looking for a visible datom past the upper bound.
                    // The terminal raw candidate remains counted, even if hidden.
                    let next = cursor.next_with_control(&mut |datom| {
                        examine(datom, state)?;
                        Ok(datom.is_none_or(|datom| {
                            datom.attribute == *attribute
                                && source.is_none_or(|source| datom.source == source)
                                && span.below_upper(&datom.value)
                        }))
                    });
                    match next {
                        Some(Ok(datom)) if span.above_lower(&datom.value) => {
                            return Some(Ok(datom));
                        }
                        Some(Ok(_)) => {}
                        Some(Err(error)) => return Some(Err(error)),
                        None => {
                            *span = remaining.next()?;
                            match start_cursor(database, *attribute, span, *source) {
                                Ok(next) => {
                                    *cursor = next;
                                    state.stats.index_seeks += 1;
                                }
                                Err(error) => return Some(Err(error)),
                            }
                        }
                    }
                }
            }
        }
    }
}

fn examine(datom: Option<&Datom>, state: &mut State<'_>) -> Result<bool, SemanticError> {
    // Polls include hidden overlay removal/disabled-index traversal, not only
    // yielded values. Charge them as work as well as checking cancellation.
    state.check(1)?;
    if datom.is_some() {
        state.stats.datoms_examined = state.stats.datoms_examined.saturating_add(1);
    }
    Ok(true)
}

fn start_cursor<'a>(
    database: &'a DatabaseValue,
    attribute: u32,
    span: &Span,
    source: Option<u64>,
) -> Result<DatabaseValueScanCursor<'a>, SemanticError> {
    let lower = span
        .lower
        .as_ref()
        .map(|lower| (lower.stored(), lower.inclusive));
    match source {
        Some(source) => database.query_savet_start_cursor(source, attribute, lower),
        None => database.query_avet_start_cursor(attribute, lower),
    }
}
