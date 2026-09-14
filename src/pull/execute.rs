use super::*;

impl Database {
    /// Resolve against the eager database's immutable value.
    pub fn resolve_entity_identifier(
        &self,
        identifier: &EntityIdentifier,
    ) -> Result<Option<u64>, SemanticError> {
        self.database_value().resolve_entity_identifier(identifier)
    }

    pub fn entity(
        &self,
        identifier: impl Into<EntityIdentifier>,
    ) -> Result<Option<Entity>, SemanticError> {
        self.database_value().entity(identifier)
    }

    pub fn pull(
        &self,
        pattern: &PullPattern,
        entity: impl Into<EntityIdentifier>,
    ) -> Result<QueryValue, SemanticError> {
        self.database_value().pull(pattern, entity)
    }

    pub fn pull_with_control(
        &self,
        pattern: &PullPattern,
        entity: impl Into<EntityIdentifier>,
        control: &PullControl,
    ) -> Result<QueryValue, SemanticError> {
        self.database_value()
            .pull_with_control(pattern, entity, control)
    }

    pub fn pull_many<I, E>(
        &self,
        pattern: &PullPattern,
        entities: I,
    ) -> Result<Vec<QueryValue>, SemanticError>
    where
        I: IntoIterator<Item = E>,
        E: Into<EntityIdentifier>,
    {
        self.database_value().pull_many(pattern, entities)
    }
}

impl DatabaseValue {
    /// Construct a lazy associative entity over this exact immutable value.
    /// A history value spans time and therefore cannot back an entity.
    pub fn entity(
        &self,
        identifier: impl Into<EntityIdentifier>,
    ) -> Result<Option<Entity>, SemanticError> {
        self.require_point_in_time("entity")?;
        Ok(self
            .resolve_entity_identifier(&identifier.into())?
            .map(|entity| Entity::from_resolved(self.clone(), entity)))
    }

    /// Project attributes from this captured point-in-time value. Even an
    /// unresolved ident or lookup reference applies explicit attribute
    /// transforms to nil, then defaults only when the transform returns nil.
    /// Defaults are never transformed. An otherwise empty selection is an
    /// empty map; an explicitly requested `:db/id` remains nil when unresolved.
    pub fn pull(
        &self,
        pattern: &PullPattern,
        entity: impl Into<EntityIdentifier>,
    ) -> Result<QueryValue, SemanticError> {
        self.pull_with_control(pattern, entity, &PullControl::default())
    }

    pub fn pull_with_control(
        &self,
        pattern: &PullPattern,
        entity: impl Into<EntityIdentifier>,
        control: &PullControl,
    ) -> Result<QueryValue, SemanticError> {
        validate_pattern(self, pattern)?;
        self.require_point_in_time("pull")?;
        let mut state = PullState {
            control,
            query_budget: None,
            recursions: BTreeMap::new(),
            entities: 0,
        };
        state.check(0)?;
        let Some(entity) = self
            .resolve_entity_identifier_with_control(&entity.into(), &mut |datom| {
                state.read_candidate(datom)
            })?
        else {
            return unresolved_pull(self, pattern, &mut state);
        };
        state.check(0)?;
        pull_entity(self, entity, pattern, &[], &mut state, 0)
    }

    /// Evaluate a find-pull expression under the enclosing query's shared
    /// cancellation, deadline, and work budget.
    pub(crate) fn pull_for_query(
        &self,
        pattern: &PullPattern,
        entity: u64,
        budget: &mut QueryPullBudget,
    ) -> Result<QueryValue, SemanticError> {
        validate_pattern(self, pattern)?;
        self.require_point_in_time("pull")?;
        let control = PullControl {
            cancel: Arc::clone(&budget.cancel),
            ..PullControl::default()
        };
        let mut state = PullState {
            control: &control,
            query_budget: Some(budget),
            recursions: BTreeMap::new(),
            entities: 0,
        };
        pull_entity(self, entity, pattern, &[], &mut state, 0)
    }

    pub fn pull_many<I, E>(
        &self,
        pattern: &PullPattern,
        entities: I,
    ) -> Result<Vec<QueryValue>, SemanticError>
    where
        I: IntoIterator<Item = E>,
        E: Into<EntityIdentifier>,
    {
        // Pattern compilation/validation precedes entity traversal in the
        // recovered implementation, including for an empty input collection.
        validate_pattern(self, pattern)?;
        self.require_point_in_time("pull")?;
        entities
            .into_iter()
            .map(|entity| self.pull(pattern, entity))
            .collect()
    }
}

fn unresolved_pull(
    database: &DatabaseValue,
    pattern: &PullPattern,
    state: &mut PullState<'_, '_>,
) -> Result<QueryValue, SemanticError> {
    let mut result = Vec::new();
    if pattern.wildcard {
        put(&mut result, keyword_key(db_id()), QueryValue::Nil);
    }
    for selector in &pattern.attributes {
        let (name, reverse) = direction_parts(database, &selector.direction)?;
        if !reverse && is_db_id(&name) {
            let key = selector
                .alias
                .clone()
                .unwrap_or_else(|| keyword_key(db_id()));
            put(&mut result, key, QueryValue::Nil);
        } else {
            // ATOMIC-NOTE: recovered pull* still calls each attribute's valfn
            // with nil after failed entity resolution. No wildcard scan or
            // fabricated entity is needed to apply xform, then default.
            let PreparedAttribute::Complete(key, value) =
                prepare_attribute(database, None, selector, state)?
            else {
                unreachable!("an unresolved entity cannot supply attribute values")
            };
            if let Some((key, value)) = finish_attribute(
                key,
                value,
                selector.default.as_ref(),
                selector.transform.as_ref(),
                state,
            )? {
                put(&mut result, key, value);
            }
        }
    }
    state.check(0)?;
    Ok(QueryValue::Map(result))
}

#[derive(Clone)]
struct PullContext<'a> {
    pattern: &'a PullPattern,
    scope: Vec<usize>,
    depth: usize,
}

#[derive(Clone, Copy)]
enum Selector<'a> {
    Explicit(usize, &'a PullAttribute),
    Wildcard(u32),
}

enum PullTask<'a> {
    Entity(u64, PullContext<'a>),
    Attribute(u64, Selector<'a>, PullContext<'a>),
    Value {
        source: u64,
        value: Value,
        value_type: ValueType,
        component: bool,
        nested: Option<&'a PullNested>,
        selector_index: Option<usize>,
        context: PullContext<'a>,
    },
    FinishMap {
        start: usize,
        entries: Vec<(QueryValue, QueryValue)>,
    },
    FinishAttribute {
        start: usize,
        key: QueryValue,
        default: Option<&'a QueryValue>,
        transform: Option<&'a PullTransform>,
        multiple: bool,
        omit_empty: bool,
    },
    LeaveRecursion {
        key: RecursionKey,
        entity: u64,
        new_traversal: bool,
    },
}

enum PullOutput {
    Value(QueryValue),
    Attribute(Option<(QueryValue, QueryValue)>),
}

/// Explicit continuations keep native stack depth independent of graph depth.
/// A recursive selector's visited set is removed at its lexical return point,
/// so siblings and unrelated nested patterns do not share accidental state.
fn pull_entity(
    database: &DatabaseValue,
    entity: u64,
    pattern: &PullPattern,
    pattern_scope: &[usize],
    state: &mut PullState<'_, '_>,
    depth: usize,
) -> Result<QueryValue, SemanticError> {
    let wildcard = PullPattern::wildcard();
    let mut tasks = vec![PullTask::Entity(
        entity,
        PullContext {
            pattern,
            scope: pattern_scope.to_vec(),
            depth,
        },
    )];
    let mut output = Vec::new();
    while let Some(task) = tasks.pop() {
        match task {
            PullTask::Entity(entity, context) => {
                state.check(1)?;
                state.charge_value_bytes(
                    std::mem::size_of::<PullTask>() + std::mem::size_of::<QueryValue>() * 3,
                )?;
                if context.depth > state.control.max_depth {
                    return Err(SemanticError::new(
                        ErrorCategory::Busy,
                        "pull/depth-limit",
                        "pull exceeded its recursion depth limit",
                    ));
                }
                state.entities = state.entities.checked_add(1).ok_or_else(|| {
                    SemanticError::new(
                        ErrorCategory::Busy,
                        "pull/entity-limit",
                        "pull entity count overflowed",
                    )
                })?;
                if state.entities > state.control.max_entities {
                    return Err(SemanticError::new(
                        ErrorCategory::Busy,
                        "pull/entity-limit",
                        "pull exceeded its entity traversal limit",
                    ));
                }
                let mut entries = Vec::new();
                let mut selectors = Vec::new();
                let mut wildcard_processed = BTreeSet::new();
                if context.pattern.wildcard {
                    put(
                        &mut entries,
                        keyword_key(db_id()),
                        QueryValue::Scalar(Value::Ref(entity)),
                    );
                    // Recovered `a-iter`/`next-a` seeks to the next attribute,
                    // not the next datom. A many-valued attribute's unselected
                    // tail must not be scanned just to discover its neighbor.
                    let mut after = None;
                    while let Some(attribute) = next_pull_attribute(database, entity, after, state)?
                    {
                        after = Some(attribute);
                        let explicit =
                            context
                                .pattern
                                .attributes
                                .iter()
                                .enumerate()
                                .find(|(_, selector)| {
                                    selector_selects_forward(database, selector, attribute)
                                });
                        state.charge_value_bytes(std::mem::size_of::<Selector>())?;
                        selectors.push(match explicit {
                            Some((index, selector)) => {
                                wildcard_processed.insert(index);
                                Selector::Explicit(index, selector)
                            }
                            None => Selector::Wildcard(attribute),
                        });
                    }
                }
                for (index, selector) in context.pattern.attributes.iter().enumerate() {
                    if !wildcard_processed.contains(&index) {
                        selectors.push(Selector::Explicit(index, selector));
                    }
                }
                tasks.push(PullTask::FinishMap {
                    start: output.len(),
                    entries,
                });
                for selector in selectors.into_iter().rev() {
                    tasks.push(PullTask::Attribute(entity, selector, context.clone()));
                }
            }
            PullTask::Attribute(entity, selector, context) => {
                let default_selector;
                let (selector_index, nested, default, transform, selector) = match selector {
                    Selector::Explicit(index, selector) => (
                        Some(index),
                        selector.nested.as_ref(),
                        selector.default.as_ref(),
                        selector.transform.as_ref(),
                        selector,
                    ),
                    Selector::Wildcard(attribute) => {
                        default_selector = PullAttribute::forward(AttributeName::Id(attribute));
                        (None, None, None, None, &default_selector)
                    }
                };
                match prepare_attribute(database, Some(entity), selector, state)? {
                    PreparedAttribute::Complete(key, value) => output.push(PullOutput::Attribute(
                        finish_attribute(key, value, default, transform, state)?,
                    )),
                    PreparedAttribute::Values {
                        key,
                        values,
                        multiple,
                        value_type,
                        component,
                    } => {
                        tasks.push(PullTask::FinishAttribute {
                            start: output.len(),
                            key,
                            default,
                            transform,
                            multiple,
                            omit_empty: nested.is_some(),
                        });
                        for value in values.into_iter().rev() {
                            tasks.push(PullTask::Value {
                                source: entity,
                                value,
                                value_type,
                                component,
                                nested,
                                selector_index,
                                context: context.clone(),
                            });
                        }
                    }
                }
            }
            PullTask::Value {
                source,
                value,
                value_type,
                component,
                nested,
                selector_index,
                context,
            } => {
                let Value::Ref(entity) = value else {
                    if nested.is_some() {
                        return Err(SemanticError::incorrect(
                            "pull/nested-non-ref",
                            "nested pull requires a ref attribute",
                        ));
                    }
                    output.push(PullOutput::Value(QueryValue::Scalar(value)));
                    continue;
                };
                if value_type != ValueType::Ref {
                    return Err(fault(
                        "pull/schema-value-mismatch",
                        "ref value belongs to a non-ref attribute",
                    ));
                }
                let mut child_context = context.clone();
                child_context.depth = context.depth.checked_add(1).ok_or_else(|| {
                    SemanticError::new(
                        ErrorCategory::Busy,
                        "pull/depth-limit",
                        "pull traversal depth overflowed",
                    )
                })?;
                let recursion = match nested {
                    Some(PullNested::Pattern(pattern)) => {
                        child_context.pattern = pattern;
                        child_context
                            .scope
                            .push(selector_index.expect("nested selector has an index"));
                        None
                    }
                    Some(PullNested::Recursion(limit)) => {
                        let mut path = context.scope.clone();
                        path.push(selector_index.expect("recursive selector has an index"));
                        Some((RecursionKey::Selector(path), *limit))
                    }
                    None if component => {
                        child_context.pattern = &wildcard;
                        Some((RecursionKey::Component(context.scope.clone()), None))
                    }
                    None => {
                        output.push(PullOutput::Value(id_map(entity)));
                        continue;
                    }
                };
                if let Some((key, limit)) = recursion {
                    let new_traversal = !state.recursions.contains_key(&key);
                    let recursion = state.recursions.entry(key.clone()).or_default();
                    if new_traversal {
                        recursion.seen.insert(source);
                    }
                    if limit.is_some_and(|limit| recursion.depth >= limit)
                        || recursion.seen.contains(&entity)
                    {
                        if new_traversal {
                            state.recursions.remove(&key);
                        }
                        output.push(PullOutput::Value(id_map(entity)));
                        continue;
                    }
                    recursion.depth += 1;
                    recursion.seen.insert(entity);
                    tasks.push(PullTask::LeaveRecursion {
                        key,
                        entity,
                        new_traversal,
                    });
                }
                tasks.push(PullTask::Entity(entity, child_context));
            }
            PullTask::FinishMap { start, mut entries } => {
                // ATOMIC-NOTE: the native typed API retains an empty Map where
                // recovered nilify-empty returns nil; nested selection handles
                // emptiness explicitly without changing this established shape.
                for item in output.drain(start..) {
                    let PullOutput::Attribute(attribute) = item else {
                        unreachable!("entity continuation requires attribute results")
                    };
                    if let Some((key, value)) = attribute {
                        put(&mut entries, key, value);
                    }
                }
                state.charge_value_bytes(
                    entries
                        .len()
                        .saturating_mul(std::mem::size_of::<(QueryValue, QueryValue)>()),
                )?;
                output.push(PullOutput::Value(QueryValue::Map(entries)));
            }
            PullTask::FinishAttribute {
                start,
                key,
                default,
                transform,
                multiple,
                omit_empty,
            } => {
                let mut values = Vec::new();
                for item in output.drain(start..) {
                    let PullOutput::Value(value) = item else {
                        unreachable!("attribute continuation requires value results")
                    };
                    if !omit_empty || !is_empty_map(&value) {
                        values.push(value);
                    }
                }
                state.charge_value_bytes(
                    values
                        .len()
                        .saturating_mul(std::mem::size_of::<QueryValue>()),
                )?;
                let value = if multiple {
                    Some(QueryValue::Collection(values))
                } else if values.is_empty() {
                    None
                } else {
                    Some(values.remove(0))
                };
                output.push(PullOutput::Attribute(finish_attribute(
                    key, value, default, transform, state,
                )?));
            }
            PullTask::LeaveRecursion {
                key,
                entity,
                new_traversal,
            } => {
                let recursion = state
                    .recursions
                    .get_mut(&key)
                    .expect("recursive selector remains active until its continuation");
                recursion.depth -= 1;
                recursion.seen.remove(&entity);
                if new_traversal {
                    state.recursions.remove(&key);
                }
            }
        }
    }
    match output.pop() {
        Some(PullOutput::Value(value)) => Ok(value),
        _ => unreachable!("root pull must return one value"),
    }
}
