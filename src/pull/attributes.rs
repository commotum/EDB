use super::*;

pub(super) enum PreparedAttribute {
    Complete(QueryValue, Option<QueryValue>),
    Values {
        key: QueryValue,
        values: Vec<Value>,
        multiple: bool,
        value_type: ValueType,
        component: bool,
    },
}

pub(super) fn prepare_attribute(
    database: &DatabaseValue,
    entity: Option<u64>,
    selector: &PullAttribute,
    state: &mut PullState<'_, '_>,
) -> Result<PreparedAttribute, SemanticError> {
    state.check(1)?;
    let (name, reverse) = direction_parts(database, &selector.direction)?;
    if !reverse && is_db_id(&name) {
        let key = selector
            .alias
            .clone()
            .unwrap_or_else(|| keyword_key(db_id()));
        return Ok(PreparedAttribute::Complete(
            key,
            Some(entity.map_or(QueryValue::Nil, |entity| {
                QueryValue::Scalar(Value::Ref(entity))
            })),
        ));
    }
    let Some(attribute) = resolve_pull_attribute(database, &name)? else {
        let AttributeName::Ident(ident) = &name else {
            unreachable!("numeric attributes are resolved strictly")
        };
        let key = selector
            .alias
            .clone()
            .unwrap_or_else(|| keyword_key(reverse_ident(ident, reverse)));
        return Ok(PreparedAttribute::Complete(key, None));
    };
    let schema = database.schema().attribute(attribute)?;
    if reverse && schema.value_type != ValueType::Ref {
        return Err(SemanticError::incorrect(
            "pull/reverse-non-ref",
            format!("{} is not a ref attribute", schema.ident.qualified_name()),
        ));
    }
    let key = selector.alias.clone().unwrap_or_else(|| {
        let ident = match &name {
            AttributeName::Ident(ident) => ident,
            AttributeName::Id(_) => &schema.ident,
        };
        keyword_key(reverse_ident(ident, reverse))
    });
    state.charge_value_bytes(crate::query::query_value_allocation_bytes(&key))?;
    let Some(entity) = entity else {
        return Ok(PreparedAttribute::Complete(key, None));
    };
    let multiple = if reverse {
        !schema.component
    } else {
        schema.cardinality == Cardinality::Many
    };
    let limit = if multiple {
        match selector.limit {
            PullLimit::Default => 1_000,
            PullLimit::Limit(limit) => limit,
            PullLimit::Unlimited => usize::MAX,
        }
    } else {
        1
    };
    let prefix = if reverse {
        IndexPrefix::Vaet {
            value: Value::Ref(entity),
            attribute: Some(attribute),
            entity: None,
        }
    } else {
        IndexPrefix::Eavt {
            entity,
            attribute: Some(attribute),
            value: None,
        }
    };
    let mut cursor = database.prefix_cursor(&prefix)?;
    let mut values = Vec::new();
    while values.len() < limit {
        let Some(datom) = cursor
            .next_with_control(&mut |datom| state.read_candidate(datom))
            .transpose()?
        else {
            break;
        };
        // A filter callback may have canceled while accepting the last value.
        state.check(0)?;
        state.charge_value_bytes(std::mem::size_of::<Value>())?;
        values.push(if reverse {
            Value::Ref(datom.entity)
        } else {
            datom.value
        });
    }
    if values.is_empty() {
        return Ok(PreparedAttribute::Complete(key, None));
    }
    Ok(PreparedAttribute::Values {
        key,
        values,
        multiple,
        value_type: schema.value_type,
        // ATOMIC-NOTE: recovered default-spec expands components only in the
        // forward direction. Reverse component references are singular but
        // still id-only unless the caller supplies a nested selector.
        component: schema.component && !reverse,
    })
}

pub(super) fn next_pull_attribute(
    database: &DatabaseValue,
    entity: u64,
    after: Option<u32>,
    state: &mut PullState<'_, '_>,
) -> Result<Option<u32>, SemanticError> {
    state.check(0)?;
    let boundary = match after {
        None => IndexBoundary::Eavt(IndexComponents::One(entity)),
        Some(attribute) => match attribute.checked_add(1) {
            Some(next) => IndexBoundary::Eavt(IndexComponents::Two(entity, next)),
            None => return Ok(None),
        },
    };
    let mut cursor = database.seek_cursor(&boundary)?;
    let datom = cursor
        .next_with_control(&mut |datom| {
            state.read_candidate(datom)?;
            // Apply the entity fence before filtering so an empty remainder
            // cannot wander through other entities while looking for a match.
            Ok(datom.is_none_or(|datom| datom.entity == entity))
        })
        .transpose()?;
    state.check(0)?;
    Ok(datom.map(|datom| datom.attribute))
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum PullAttributeIdentity {
    DbId,
    Installed(u32),
    Unknown(Keyword),
}

pub(super) fn validate_pattern(
    database: &DatabaseValue,
    pattern: &PullPattern,
) -> Result<(), SemanticError> {
    let mut pending = vec![pattern];
    while let Some(pattern) = pending.pop() {
        let mut selectors = BTreeSet::new();
        for selector in &pattern.attributes {
            if selector.limit == PullLimit::Limit(0) {
                return Err(SemanticError::incorrect(
                    "db.error/invalid-limit",
                    "pull limits must be positive or unlimited",
                ));
            }
            match selector.nested.as_ref() {
                Some(PullNested::Pattern(pattern)) => pending.push(pattern),
                Some(PullNested::Recursion(Some(0))) => {
                    return Err(SemanticError::incorrect(
                        "db.error/invalid-recur-limit",
                        "recursive pull limits must be positive",
                    ));
                }
                _ => {}
            }

            let (name, reverse) = direction_parts(database, &selector.direction)?;
            let identity = if !reverse && is_db_id(&name) {
                PullAttributeIdentity::DbId
            } else {
                match resolve_pull_attribute(database, &name)? {
                    Some(attribute) => PullAttributeIdentity::Installed(attribute),
                    None => match &name {
                        AttributeName::Ident(ident) => {
                            PullAttributeIdentity::Unknown(ident.clone())
                        }
                        AttributeName::Id(_) => unreachable!("numeric attributes resolve strictly"),
                    },
                }
            };
            if !selectors.insert((reverse, identity)) {
                return Err(SemanticError::incorrect(
                    "pull/duplicate-attribute",
                    "a pull pattern may specify an attribute only once per direction",
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn selector_selects_forward(
    database: &DatabaseValue,
    selector: &PullAttribute,
    attribute: u32,
) -> bool {
    let Ok((name, false)) = direction_parts(database, &selector.direction) else {
        return false;
    };
    resolve_pull_attribute(database, &name).ok().flatten() == Some(attribute)
}

pub(super) fn direction_parts(
    database: &DatabaseValue,
    direction: &PullDirection,
) -> Result<(AttributeName, bool), SemanticError> {
    Ok(match direction {
        PullDirection::Forward(name) => (name.clone(), false),
        PullDirection::Reverse(name) => (name.clone(), true),
        PullDirection::SchemaResolved(ident) => {
            let name = AttributeName::Ident(ident.clone());
            // ATOMIC-NOTE: normalization retains spelling until this exact
            // database can apply recovered reverse-lookup? schema precedence.
            match ident.name.strip_prefix('_') {
                Some(forward) if resolve_pull_attribute(database, &name)?.is_none() => {
                    if forward.is_empty() {
                        return Err(SemanticError::incorrect(
                            "edn/pull-attribute",
                            "empty reverse attribute name",
                        ));
                    }
                    (
                        AttributeName::Ident(Keyword {
                            namespace: ident.namespace.clone(),
                            name: forward.to_owned(),
                        }),
                        true,
                    )
                }
                _ => (name, false),
            }
        }
    })
}

pub(super) fn resolve_pull_attribute(
    database: &DatabaseValue,
    name: &AttributeName,
) -> Result<Option<u32>, SemanticError> {
    match name {
        AttributeName::Id(attribute) => {
            database.schema().attribute(*attribute)?;
            Ok(Some(*attribute))
        }
        AttributeName::Ident(ident) => {
            let Some(entity) = database.entid(ident) else {
                return Ok(None);
            };
            let Ok(attribute) = schema_eid_to_attr_id(entity) else {
                return Ok(None);
            };
            Ok(database
                .schema()
                .attribute(attribute)
                .ok()
                .map(|_| attribute))
        }
    }
}

pub(super) fn is_db_id(name: &AttributeName) -> bool {
    matches!(name, AttributeName::Ident(ident) if *ident == db_id())
}

pub(super) fn is_empty_map(value: &QueryValue) -> bool {
    matches!(value, QueryValue::Map(entries) if entries.is_empty())
}

pub(super) fn reverse_ident(ident: &Keyword, reverse: bool) -> Keyword {
    if !reverse {
        return ident.clone();
    }
    Keyword {
        namespace: ident.namespace.clone(),
        name: format!("_{}", ident.name),
    }
}

pub(super) fn put(entries: &mut Vec<(QueryValue, QueryValue)>, key: QueryValue, value: QueryValue) {
    QueryValue::put_map_entry(entries, key, value);
}

pub(super) fn id_map(entity: u64) -> QueryValue {
    QueryValue::Map(vec![(
        keyword_key(db_id()),
        QueryValue::Scalar(Value::Ref(entity)),
    )])
}

pub(super) fn keyword_key(keyword: Keyword) -> QueryValue {
    QueryValue::Scalar(Value::Keyword(keyword))
}

pub(super) fn db_id() -> Keyword {
    Keyword::new("db", "id")
}
