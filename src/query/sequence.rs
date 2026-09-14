//! Eager relational evaluation with lazy, exact-source Pull projection.
use super::*;

/// A single-pass result sequence. Relational joins and aggregates are prepared
/// eagerly; Pull (including its transforms) runs only for consumed rows. Distinct
/// entity bindings remain separate even when their projected maps compare equal.
/// This owns immutable source values, not a live connection or writer lock.
pub struct QuerySequence {
    rows: std::vec::IntoIter<Vec<QueryValue>>,
    elements: Vec<FindElement>,
    sources: Vec<QueryDataSource>,
    budget: QueryPullBudget<'static>,
    stats: QueryStats,
    plan: Vec<PlanStep>,
    diagnostics: Option<QueryDiagnostics>,
    failed: bool,
}

impl fmt::Debug for QuerySequence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QuerySequence")
            .field("remaining_rows", &self.remaining_rows())
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl QuerySequence {
    /// Count prepared tuples without realizing any Pull or transform. A later
    /// projection failure can terminate iteration before this many successful rows.
    pub fn remaining_rows(&self) -> usize {
        self.rows.len()
    }
    pub fn stats(&self) -> &QueryStats {
        &self.stats
    }
    pub fn plan(&self) -> &[PlanStep] {
        &self.plan
    }
    /// Relational preparation diagnostics. Lazy Pull work is reflected in
    /// `stats()` as consumed, not misrepresented as a scheduled where clause.
    pub fn diagnostics(&self) -> Option<&QueryDiagnostics> {
        self.diagnostics.as_ref()
    }
}

impl Iterator for QuerySequence {
    type Item = Result<Vec<QueryValue>, SemanticError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        let mut row = self.rows.next()?;
        let result = (|| {
            self.budget.check(0)?;
            for (value, element) in row.iter_mut().zip(&self.elements) {
                if let FindElement::Pull {
                    source, pattern, ..
                } = element
                {
                    let entity = match value {
                        QueryValue::Scalar(value) => entity_id(value),
                        _ => None,
                    }
                    .ok_or_else(|| {
                        SemanticError::incorrect(
                            "query/pull-entity",
                            "pull expression variable must bind an entity id",
                        )
                    })?;
                    let source = self
                        .sources
                        .iter()
                        .find(|candidate| candidate.name == *source)
                        .expect("original source validation precedes preparation");
                    let QuerySourceValue::Database(database) = &source.value else {
                        unreachable!("original Pull source validation precedes preparation")
                    };
                    *value = database.pull_for_query(pattern, entity, &mut self.budget)?;
                }
            }
            Ok(row)
        })();
        self.stats.work = self.budget.work() as u64;
        self.stats.allocated_value_bytes = self.budget.value_bytes();
        if result.is_ok() {
            self.stats.rows_produced += 1;
        } else {
            self.failed = true;
            self.rows = Vec::new().into_iter();
        }
        Some(result)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(self.remaining_rows()))
    }
}
impl std::iter::FusedIterator for QuerySequence {}

impl QueryEngine {
    pub fn sequence(
        query: &Query,
        sources: &[QuerySource],
        inputs: &[QueryInput],
        control: &QueryControl,
    ) -> Result<QuerySequence, SemanticError> {
        Self::sequence_with_extensions(query, sources, inputs, control, None)
    }

    pub fn sequence_with_extensions(
        query: &Query,
        sources: &[QuerySource],
        inputs: &[QueryInput],
        control: &QueryControl,
        extensions: Option<&QueryExtensions>,
    ) -> Result<QuerySequence, SemanticError> {
        let sources: Vec<_> = sources
            .iter()
            .map(|source| QueryDataSource::database(&source.name, source.database.clone()))
            .collect();
        Self::sequence_sources_with_extensions(query, &sources, inputs, control, extensions)
    }

    /// Evaluate the relation using the same immutable database, raw-tuple and
    /// log sources as eager queries, then project Pull lazily per consumed row.
    pub fn sequence_sources(
        query: &Query,
        sources: &[QueryDataSource],
        inputs: &[QueryInput],
        control: &QueryControl,
    ) -> Result<QuerySequence, SemanticError> {
        Self::sequence_sources_with_extensions(query, sources, inputs, control, None)
    }

    /// Callbacks in relational clauses run during preparation; Pull callbacks
    /// run only during iteration. Both phases share the original time/work
    /// allowance. No live connection or mutable source is retained.
    pub fn sequence_sources_with_extensions(
        query: &Query,
        sources: &[QueryDataSource],
        inputs: &[QueryInput],
        control: &QueryControl,
        extensions: Option<&QueryExtensions>,
    ) -> Result<QuerySequence, SemanticError> {
        let started = Instant::now();
        validate_query(query, inputs.len())?;
        let mut source_map = BTreeMap::new();
        for source in sources {
            if source_map
                .insert(source.name.as_str(), source.borrowed())
                .is_some()
            {
                return Err(SemanticError::incorrect(
                    "query/duplicate-source",
                    format!("source {} is supplied more than once", source.name),
                ));
            }
        }
        validate_consumed_sources(query, &source_map, extensions)?;
        let mut prepared = query.clone();
        let elements = find_elements(&query.find).to_vec();
        let replace = |element: &mut FindElement| {
            if let FindElement::Pull { variable, .. } = element {
                *element = FindElement::Variable(variable.clone());
            }
        };
        match &mut prepared.find {
            FindSpec::Relation(elements) | FindSpec::Tuple(elements) => {
                elements.iter_mut().for_each(replace)
            }
            FindSpec::Collection(element) | FindSpec::Scalar(element) => replace(element),
        }
        let deadline = control
            .timeout
            .and_then(|timeout| started.checked_add(timeout));
        let mut preparation_control = control.clone();
        preparation_control.timeout =
            deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
        let mut outcome = Self::execute_sources_with_extensions(
            &prepared,
            sources,
            inputs,
            &preparation_control,
            extensions,
        )?;
        let rows = match outcome.result {
            QueryResult::Relation(rows) => rows,
            QueryResult::Tuple(row) => row.into_iter().collect(),
            QueryResult::Collection(values) => {
                values.into_iter().map(|value| vec![value]).collect()
            }
            QueryResult::Scalar(value) => value.into_iter().map(|value| vec![value]).collect(),
        };
        let mut budget = QueryPullBudget::new(
            Arc::clone(&control.cancel),
            deadline,
            control.max_work,
            usize::try_from(outcome.stats.work).unwrap_or(usize::MAX),
        )
        // ATOMIC-NOTE: qseq defers Pull, so consumption continues the original
        // invocation's allocation allowance after relational preparation.
        .with_value_budget(outcome.stats.allocated_value_bytes, control.max_value_bytes);
        budget.check(0)?;
        outcome.stats.rows_produced = 0;
        Ok(QuerySequence {
            rows: rows.into_iter(),
            elements,
            sources: sources.to_vec(),
            budget,
            stats: outcome.stats,
            plan: outcome.plan,
            diagnostics: outcome.diagnostics,
            failed: false,
        })
    }
}

impl DatabaseValue {
    pub fn query_sequence(
        &self,
        query: &Query,
        inputs: &[QueryInput],
        control: &QueryControl,
    ) -> Result<QuerySequence, SemanticError> {
        QueryEngine::sequence(
            query,
            &[QuerySource {
                name: "$".into(),
                database: self.clone(),
            }],
            inputs,
            control,
        )
    }
}
impl Database {
    pub fn query_sequence(
        &self,
        query: &Query,
        inputs: &[QueryInput],
        control: &QueryControl,
    ) -> Result<QuerySequence, SemanticError> {
        self.database_value().query_sequence(query, inputs, control)
    }
}
