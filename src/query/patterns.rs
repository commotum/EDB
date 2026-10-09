//! Schema-aware datom probes and literal relation matching over the supplied source value.
use super::*;

pub(super) fn relation_data_pattern(
    pattern: &RelationPattern,
) -> Result<DataPattern, SemanticError> {
    if pattern.terms.len() > 6 {
        return Err(SemanticError::incorrect(
            "query/pattern-width",
            "database patterns have at most six datom columns",
        ));
    }
    Ok(DataPattern {
        source: pattern.source.clone(),
        entity: pattern.terms.first().cloned().unwrap_or(Term::Blank),
        attribute: pattern.terms.get(1).cloned().unwrap_or(Term::Blank),
        value: pattern.terms.get(2).cloned().unwrap_or(Term::Blank),
        transaction: pattern.terms.get(3).cloned(),
        fact_source: pattern.terms.get(4).cloned(),
        added: pattern.terms.get(5).cloned(),
    })
}

pub(super) fn evaluate_relation_pattern(
    pattern: &RelationPattern,
    rows: Vec<Row>,
    inherited: Option<&str>,
    conjunction: &[Clause],
    state: &mut State<'_>,
) -> Result<(Vec<Row>, String), SemanticError> {
    let source = effective_source(&pattern.source, inherited);
    match state.sources.get(source).copied() {
        Some(SourceRef::Tuples(tuples)) => join::evaluate_raw(
            pattern.terms.iter().enumerate().collect(),
            rows,
            join::RawRelation::Stored(tuples),
            state,
        ),
        Some(SourceRef::Relation(relation)) => join::evaluate_raw(
            pattern.terms.iter().enumerate().collect(),
            rows,
            join::RawRelation::General(relation),
            state,
        ),
        Some(SourceRef::Database(_)) => evaluate_pattern(
            &relation_data_pattern(pattern)?,
            rows,
            inherited,
            conjunction,
            state,
        ),
        Some(SourceRef::Log(_)) => Err(SemanticError::incorrect(
            "query/source-kind",
            "log sources require tx-ids/tx-data",
        )),
        None => Err(SemanticError::incorrect(
            "query/unknown-source",
            format!("unknown source {source}"),
        )),
    }
}

pub(super) fn evaluate_pattern(
    pattern: &DataPattern,
    rows: Vec<Row>,
    inherited_source: Option<&str>,
    conjunction: &[Clause],
    state: &mut State<'_>,
) -> Result<(Vec<Row>, String), SemanticError> {
    let source = effective_source(&pattern.source, inherited_source);
    let source_value = state.sources.get(source).copied().ok_or_else(|| {
        SemanticError::incorrect("query/unknown-source", format!("unknown source {source}"))
    })?;
    let database = match source_value {
        SourceRef::Database(database) => database,
        SourceRef::Tuples(tuples) => return join::evaluate_tuples(pattern, rows, tuples, state),
        SourceRef::Relation(relation) => {
            return join::evaluate_raw(
                join::pattern_terms(pattern),
                rows,
                join::RawRelation::General(relation),
                state,
            );
        }
        SourceRef::Log(_) => {
            return Err(SemanticError::incorrect(
                "query/source-kind",
                "log sources must be consumed with tx-ids/tx-data",
            ));
        }
    };
    let mut next = Vec::new();
    let mut access = "EAVT scan".to_owned();
    let mut input = rows.into_iter().peekable();
    while input.peek().is_some() {
        // Group identical *resolved* probes only within a bounded batch. The
        // selected native index remains the same as in the one-row evaluator.
        let mut groups = BTreeMap::<_, Vec<Row>>::new();
        let mut bytes = 0usize;
        for row in input.by_ref() {
            state.check(1)?;
            let entity = resolve_entity(database, &pattern.entity, &row)?;
            // A bound lookup ref that does not resolve denotes no entity.  Keep it
            // distinct from a blank or unbound entity term, which intentionally
            // leaves the E position open for an index scan.
            if entity.is_none() && term_is_bound(&pattern.entity, &row) {
                continue;
            }
            let fact_source = pattern
                .fact_source
                .as_ref()
                .map(|term| resolve_entity(database, term, &row))
                .transpose()?
                .flatten();
            if fact_source.is_none()
                && pattern
                    .fact_source
                    .as_ref()
                    .is_some_and(|term| term_is_bound(term, &row))
            {
                continue;
            }
            let attribute = resolve_attribute(database, &pattern.attribute, &row)?;
            let bound_value = term_bound_value(&pattern.value, &row);
            let value = bound_value
                .as_ref()
                .and_then(BoundValue::stored)
                .map(|value| resolve_pattern_value(database, attribute, value))
                .transpose()?;
            let range = if !state.control.force_scan
                && entity.is_none()
                && bound_value.is_none()
                && attribute.is_some_and(|attribute| database.physical_avet_ready(attribute))
            {
                ranges::constraints(pattern, conjunction, &row, state)?
            } else {
                None
            };
            let key = (
                entity,
                attribute,
                value.map(BoundValue::Stored),
                range,
                fact_source,
            );
            bytes = bytes.saturating_add(
                std::mem::size_of::<Row>()
                    + std::mem::size_of_val(&key)
                    + key.2.as_ref().map_or(0, join::bound_bytes)
                    + key.3.as_ref().map_or(0, ranges::Range::retained_bytes),
            );
            groups.entry(key).or_default().push(row);
            if state.control.force_scan || bytes >= state.control.max_join_bytes {
                break;
            }
        }
        state.stats.peak_join_bytes = state.stats.peak_join_bytes.max(bytes);
        for ((entity, attribute, value, range, fact_source), rows) in groups {
            let value = value.as_ref().and_then(BoundValue::stored);
            state.stats.grouped_probes_saved += rows.len().saturating_sub(1) as u64;
            let (mut datoms, selected) = select_datoms(
                database,
                entity,
                attribute,
                value,
                range,
                fact_source,
                state.control.force_scan,
            )?;
            access = selected;
            state.stats.index_seeks += datoms.initial_seeks();
            while let Some(datom) = datoms.next(state) {
                let datom = datom?;
                for row in &rows {
                    state.check(1)?;
                    state.stats.join_candidates += 1;
                    let mut candidate = row.clone();
                    if unify_entity_term(
                        database,
                        &mut candidate,
                        &pattern.entity,
                        entity,
                        datom.entity,
                    )? && unify_attribute_term(
                        database,
                        &mut candidate,
                        &pattern.attribute,
                        attribute,
                        datom.attribute,
                    )? && unify_value_term(
                        database,
                        &mut candidate,
                        &pattern.value,
                        attribute,
                        value,
                        &datom.value,
                    )? && pattern.transaction.as_ref().map_or(Ok(true), |term| {
                        unify_term_checked(
                            &mut candidate,
                            term,
                            &BoundValue::Stored(Value::Ref(datom.tx)),
                            state,
                        )
                    })? && pattern.fact_source.as_ref().map_or(Ok(true), |term| {
                        unify_entity_term(database, &mut candidate, term, fact_source, datom.source)
                    })? && pattern.added.as_ref().map_or(Ok(true), |term| {
                        unify_term_checked(
                            &mut candidate,
                            term,
                            &BoundValue::Stored(Value::Bool(datom.added)),
                            state,
                        )
                    })? {
                        state.push_row(&mut next, candidate)?;
                    }
                }
            }
        }
    }
    Ok((next, access))
}

pub(super) fn select_datoms<'a>(
    database: &'a DatabaseValue,
    entity: Option<u64>,
    attribute: Option<u32>,
    value: Option<&Value>,
    range: Option<ranges::Range>,
    fact_source: Option<u64>,
    force_scan: bool,
) -> Result<(ranges::Datoms<'a>, String), SemanticError> {
    if !force_scan {
        if let Some(entity) = entity {
            let prefix = IndexPrefix::Eavt {
                entity,
                attribute,
                value: attribute.and(value).cloned(),
            };
            return Ok((
                ranges::Datoms::prefix(database.query_prefix_cursor(&prefix)?),
                "EAVT seek".into(),
            ));
        }
        if let Some(source) = fact_source {
            if let Some(attribute) = attribute
                && let Some(range) = range
                && database.physical_avet_ready(attribute)
            {
                return Ok((
                    ranges::Datoms::range(database, attribute, range, Some(source))?,
                    "SAVET range".into(),
                ));
            }
            let (prefix, label) = if let Some(attribute) = attribute
                && let Some(value) = value
                && database.physical_avet_ready(attribute)
            {
                (
                    IndexPrefix::Savet {
                        source,
                        attribute: Some(attribute),
                        value: Some(value.clone()),
                        entity: None,
                    },
                    "SAVET seek",
                )
            } else {
                (
                    IndexPrefix::Saevt {
                        source,
                        attribute,
                        entity: None,
                        value: None,
                    },
                    "SAEVT seek",
                )
            };
            return Ok((
                ranges::Datoms::prefix(database.query_prefix_cursor(&prefix)?),
                label.into(),
            ));
        }
        if let Some(attribute) = attribute {
            if let Some(value) = value
                && database.physical_avet_ready(attribute)
            {
                let prefix = IndexPrefix::Avet {
                    attribute,
                    value: Some(value.clone()),
                    entity: None,
                };
                return Ok((
                    ranges::Datoms::prefix(database.query_prefix_cursor(&prefix)?),
                    "AVET seek".into(),
                ));
            }
            if let Some(range) = range {
                return Ok((
                    ranges::Datoms::range(database, attribute, range, None)?,
                    "AVET range".into(),
                ));
            }
            let prefix = IndexPrefix::Aevt {
                attribute,
                entity: None,
                value: None,
            };
            return Ok((
                ranges::Datoms::prefix(database.query_prefix_cursor(&prefix)?),
                "AEVT seek".into(),
            ));
        }
        if let Some(Value::Ref(referenced)) = value {
            let prefix = IndexPrefix::Vaet {
                value: Value::Ref(*referenced),
                attribute: None,
                entity: None,
            };
            return Ok((
                ranges::Datoms::prefix(database.query_prefix_cursor(&prefix)?),
                "VAET seek".into(),
            ));
        }
    }
    // EDB-NOTE: recovered DbRel rejects wholly unbound database scans.
    // Native callers may request them under the same cooperative work/row/value
    // controls; force_scan selects this EAVT path for planner diagnostics.
    Ok((
        ranges::Datoms::scan(database.query_scan_cursor(IndexOrder::Eavt)?),
        "EAVT scan".into(),
    ))
}

pub(super) fn entity_id(value: &Value) -> Option<u64> {
    match value {
        Value::Ref(value) => Some(*value),
        Value::Long(value) => u64::try_from(*value).ok(),
        _ => None,
    }
}
pub(super) fn entity_value(
    database: &DatabaseValue,
    value: &Value,
) -> Result<Option<u64>, SemanticError> {
    match value {
        Value::Keyword(keyword) => database.entid(keyword).map(Some).ok_or_else(|| {
            SemanticError::incorrect(
                "query/unknown-ident",
                format!("unknown ident {}", keyword.qualified_name()),
            )
        }),
        Value::Tuple(parts) => match parts.as_slice() {
            [Some(attribute), Some(value)] => {
                let attribute = attribute_value(database, attribute)?;
                database.lookup(attribute, value)
            }
            _ => Err(SemanticError::incorrect(
                "query/entity-value",
                "lookup ref must contain exactly an attribute and value",
            )),
        },
        _ => Ok(entity_id(value)),
    }
}
pub(super) fn resolve_entity(
    database: &DatabaseValue,
    term: &Term,
    row: &Row,
) -> Result<Option<u64>, SemanticError> {
    match term_bound_value(term, row) {
        Some(BoundValue::Stored(value)) => entity_value(database, &value),
        Some(BoundValue::Nil | BoundValue::Query(_)) | None => Ok(None),
    }
}
pub(super) fn resolve_attribute(
    database: &DatabaseValue,
    term: &Term,
    row: &Row,
) -> Result<Option<u32>, SemanticError> {
    match term_bound_value(term, row) {
        Some(BoundValue::Stored(value)) => attribute_value(database, &value).map(Some),
        Some(BoundValue::Nil) | None => Ok(None),
        Some(BoundValue::Query(_)) => Err(SemanticError::incorrect(
            "query/attribute-value",
            "a query container cannot identify an attribute",
        )),
    }
}
pub(super) fn attribute_value(
    database: &DatabaseValue,
    value: &Value,
) -> Result<u32, SemanticError> {
    let entity = match value {
        Value::Keyword(keyword) => database.entid(keyword).ok_or_else(|| {
            SemanticError::incorrect(
                "query/unknown-attribute",
                format!("unknown attribute {}", keyword.qualified_name()),
            )
        })?,
        Value::Ref(value) => *value,
        Value::Long(value) => u64::try_from(*value).map_err(|_| {
            SemanticError::incorrect("query/attribute-value", "attribute id is out of range")
        })?,
        _ => Err(SemanticError::incorrect(
            "query/attribute-value",
            "attribute must be a keyword or id",
        ))?,
    };
    let attribute = schema_eid_to_attr_id(entity).map_err(|_| {
        SemanticError::incorrect(
            "query/attribute-value",
            format!("entity {entity} cannot identify an installed attribute"),
        )
    })?;
    database.schema().attribute(attribute).map_err(|_| {
        SemanticError::incorrect(
            "query/unknown-attribute",
            format!("unknown attribute id {attribute}"),
        )
    })?;
    Ok(attribute)
}
pub(super) fn resolve_pattern_value(
    database: &DatabaseValue,
    attribute: Option<u32>,
    value: &Value,
) -> Result<Value, SemanticError> {
    let Some(attribute) = attribute else {
        return Ok(value.clone());
    };
    if database.schema().attribute(attribute)?.value_type == crate::ValueType::Ref {
        return match value {
            Value::Keyword(keyword) => database.entid(keyword).map(Value::Ref).ok_or_else(|| {
                SemanticError::incorrect(
                    "query/unknown-ident",
                    format!("unknown ident {}", keyword.qualified_name()),
                )
            }),
            Value::Long(entity) => u64::try_from(*entity).map(Value::Ref).map_err(|_| {
                SemanticError::incorrect(
                    "query/entity-value",
                    "ref value entity id is out of range",
                )
            }),
            _ => Ok(value.clone()),
        };
    }
    Ok(value.clone())
}
pub(super) fn unify_entity_term(
    database: &DatabaseValue,
    row: &mut Row,
    term: &Term,
    resolved: Option<u64>,
    entity: u64,
) -> Result<bool, SemanticError> {
    match term {
        Term::Blank => Ok(true),
        Term::Nil => Ok(false),
        Term::Constant(_) | Term::QueryConstant(_) => Ok(resolved == Some(entity)),
        Term::Variable(variable) => match row.get(variable) {
            Some(BoundValue::Nil | BoundValue::Query(_)) => Ok(false),
            Some(BoundValue::Stored(expected)) => Ok(match resolved {
                Some(resolved) => resolved == entity,
                None => entity_value(database, expected)? == Some(entity),
            }),
            None => {
                row.insert(variable.clone(), BoundValue::Stored(Value::Ref(entity)));
                Ok(true)
            }
        },
    }
}
pub(super) fn unify_attribute_term(
    database: &DatabaseValue,
    row: &mut Row,
    term: &Term,
    resolved: Option<u32>,
    attribute: u32,
) -> Result<bool, SemanticError> {
    match term {
        Term::Blank => Ok(true),
        Term::Nil => Ok(false),
        Term::Constant(_) | Term::QueryConstant(_) => Ok(resolved == Some(attribute)),
        Term::Variable(variable) => match row.get(variable) {
            Some(BoundValue::Nil | BoundValue::Query(_)) => Ok(false),
            Some(BoundValue::Stored(expected)) => Ok(match resolved {
                Some(resolved) => resolved == attribute,
                None => attribute_value(database, expected)? == attribute,
            }),
            None => {
                row.insert(
                    variable.clone(),
                    BoundValue::Stored(Value::Ref(u64::from(attribute))),
                );
                Ok(true)
            }
        },
    }
}
pub(super) fn unify_value_term(
    database: &DatabaseValue,
    row: &mut Row,
    term: &Term,
    resolved_attribute: Option<u32>,
    resolved: Option<&Value>,
    value: &Value,
) -> Result<bool, SemanticError> {
    match term {
        Term::Blank => Ok(true),
        Term::Nil => Ok(false),
        Term::Constant(_) | Term::QueryConstant(_) => Ok(resolved == Some(value)),
        Term::Variable(variable) => match row.get(variable) {
            Some(BoundValue::Nil | BoundValue::Query(_)) => Ok(false),
            Some(BoundValue::Stored(expected)) => Ok(match resolved {
                Some(resolved) => resolved == value,
                None => resolve_pattern_value(database, resolved_attribute, expected)? == *value,
            }),
            None => {
                row.insert(variable.clone(), BoundValue::Stored(value.clone()));
                Ok(true)
            }
        },
    }
}
