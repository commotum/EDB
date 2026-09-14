//! Map and callback expansion against one immutable db-before.
use super::canonical::{compare_map_entry, compare_map_value, compare_tx_form};
use super::forms::*;
use super::functions::TxFunctions;
use super::input::validate_forms_input;
use crate::{Cardinality, DatabaseValue, Keyword, SemanticError, Unique, Value, ValueType};
use std::collections::BTreeSet;

pub(super) fn normalize_forms_against(
    db_before: &DatabaseValue,
    forms: &[TxForm],
    functions: Option<&TxFunctions>,
    max_primitive_ops: usize,
) -> Result<Vec<TxOp>, SemanticError> {
    let lowered;
    let forms = if forms_have_edn(forms) {
        lowered = crate::edn_transaction::lower_forms(db_before, forms)?;
        lowered.as_slice()
    } else {
        forms
    };
    validate_forms_input(forms)?;
    let mut forms = forms.to_vec();
    forms.sort_by(compare_tx_form);
    // A local callback may return many forms (and more callbacks). Preserve
    // incremental primitive admission while discovering that complete input.
    // Only this finite-budget, local-callback path needs a preflight; persisted
    // programs enforce their expansion budget before reaching this normalizer.
    let mut admission = (max_primitive_ops != usize::MAX
        && forms.iter().any(|form| matches!(form, TxForm::Call(_))))
    .then(|| {
        (
            Normalizer {
                db_before,
                explicit_tempids: BTreeSet::new(),
                next_anonymous: 0,
                primitive_count: 0,
                max_primitive_ops,
            },
            Vec::new(),
        )
    });
    let mut expanded = Vec::new();
    expand_local_calls(
        db_before,
        functions,
        forms,
        0,
        &mut expanded,
        &mut admission,
    )?;
    let mut normalizer = Normalizer {
        db_before,
        explicit_tempids: explicit_tempids(&expanded),
        next_anonymous: 0,
        primitive_count: 0,
        max_primitive_ops,
    };
    let mut ops = Vec::new();
    for form in expanded {
        normalizer.expand_form(&form, &mut ops)?;
    }
    Ok(ops)
}

/// Discover the complete author-supplied identity namespace before allocating
/// anonymous maps. A callback can emit an explicit tempid after an earlier
/// anonymous map, including through another callback. Every call still sees
/// the same db-before and executes exactly once. Keep the previous canonical
/// depth-first form order so noncolliding requests retain their allocations.
/// Persisted calls have already expanded at the authoritative boundary.
fn expand_local_calls(
    db_before: &DatabaseValue,
    functions: Option<&TxFunctions>,
    forms: Vec<TxForm>,
    depth: usize,
    output: &mut Vec<TxForm>,
    admission: &mut Option<(Normalizer<'_>, Vec<TxOp>)>,
) -> Result<(), SemanticError> {
    for form in forms {
        if depth > 32 {
            return Err(SemanticError::incorrect(
                "transaction/function-depth",
                "transaction function expansion exceeded 32 nested calls",
            ));
        }
        match form {
            TxForm::Call(call) => {
                let Some(functions) = functions else {
                    return Err(SemanticError::incorrect(
                        "transaction/missing-function-context",
                        "process-local transaction callbacks require an explicit function registry",
                    ));
                };
                let mut generated = functions.invoke(db_before, &call)?;
                validate_forms_input(&generated)?;
                generated.sort_by(compare_tx_form);
                expand_local_calls(
                    db_before,
                    Some(functions),
                    generated,
                    depth + 1,
                    output,
                    admission,
                )?;
            }
            TxForm::ProgramCall(_) | TxForm::Edn(_) => {
                return Err(SemanticError::incorrect(
                    "transaction/unresolved-database-function",
                    "persisted database-function calls must be resolved against db-before by the transactor",
                ));
            }
            form => {
                if let Some((normalizer, scratch)) = admission {
                    normalizer.expand_form(&form, scratch)?;
                    scratch.clear();
                }
                output.push(form);
            }
        }
    }
    Ok(())
}

fn explicit_tempids(forms: &[TxForm]) -> BTreeSet<String> {
    fn entity(reference: &EntityRef, names: &mut BTreeSet<String>) {
        match reference {
            EntityRef::Temp(name) => {
                names.insert(name.clone());
            }
            EntityRef::LookupInput { value: input, .. } => value(input, names),
            _ => {}
        }
    }
    fn value(input: &TxValue, names: &mut BTreeSet<String>) {
        match input {
            TxValue::Entity(reference) => entity(reference, names),
            TxValue::Tuple(slots) => {
                for slot in slots.iter().flatten() {
                    value(slot, names);
                }
            }
            TxValue::Scalar(_) => {}
        }
    }
    fn map(input: &EntityMap, names: &mut BTreeSet<String>) {
        if let Some(reference) = &input.id {
            entity(reference, names);
        }
        for (_, input) in &input.attributes {
            map_value(input, names);
        }
    }
    fn map_value(input: &MapValue, names: &mut BTreeSet<String>) {
        match input {
            MapValue::Value(input) => value(input, names),
            MapValue::Nested(input) => map(input, names),
            MapValue::Many(inputs) => {
                for input in inputs {
                    map_value(input, names);
                }
            }
        }
    }
    let mut names = BTreeSet::new();
    for form in forms {
        match form {
            TxForm::EntityMap(input) => map(input, &mut names),
            TxForm::Op(op) => match op {
                TxOp::Add {
                    entity: id,
                    value: input,
                    ..
                } => {
                    entity(id, &mut names);
                    value(input, &mut names);
                }
                TxOp::Retract {
                    entity: id,
                    value: input,
                    ..
                } => {
                    entity(id, &mut names);
                    if let Some(input) = input {
                        value(input, &mut names);
                    }
                }
                TxOp::Cas {
                    entity: id,
                    old,
                    new,
                    ..
                } => {
                    entity(id, &mut names);
                    if let Some(old) = old {
                        value(old, &mut names);
                    }
                    value(new, &mut names);
                }
                TxOp::RetractEntity(id) => entity(id, &mut names),
                TxOp::Ensure { entity: id, spec } => {
                    entity(id, &mut names);
                    entity(spec, &mut names);
                }
                TxOp::ForcePartition { tempid, partition } => {
                    names.insert(tempid.clone());
                    entity(partition, &mut names);
                }
                TxOp::MatchPartition { tempid, entity: id } => {
                    names.insert(tempid.clone());
                    entity(id, &mut names);
                }
                TxOp::InstallAttribute(_) | TxOp::AlterAttribute(_) => {}
            },
            TxForm::Call(_) | TxForm::ProgramCall(_) | TxForm::Edn(_) => {
                unreachable!("all calls were expanded before anonymous allocation")
            }
        }
    }
    names
}

struct Normalizer<'a> {
    db_before: &'a DatabaseValue,
    explicit_tempids: BTreeSet<String>,
    next_anonymous: u64,
    primitive_count: usize,
    max_primitive_ops: usize,
}

impl Normalizer<'_> {
    fn expand_form(&mut self, form: &TxForm, output: &mut Vec<TxOp>) -> Result<(), SemanticError> {
        match form {
            TxForm::Op(op) => self.push(op.clone(), output),
            TxForm::EntityMap(map) => {
                self.expand_map(map, None, 0, output)?;
                Ok(())
            }
            TxForm::Call(_) | TxForm::ProgramCall(_) | TxForm::Edn(_) => {
                unreachable!("all calls were expanded before anonymous allocation")
            }
        }
    }

    fn anonymous_tempid(&mut self) -> Result<EntityRef, SemanticError> {
        // Retain the existing deterministic receipt names where they do not
        // collide. Submitted forms (not these normalized names) determine the
        // request digest; already-committed retries return their stored receipt
        // before reaching normalization, including historical collisions.
        loop {
            let name = format!("__map/{:020}", self.next_anonymous);
            self.next_anonymous = self.next_anonymous.checked_add(1).ok_or_else(|| {
                SemanticError::new(
                    crate::ErrorCategory::Busy,
                    "transaction/anonymous-id-overflow",
                    "anonymous transaction identities exhausted their allocation range",
                )
            })?;
            if !self.explicit_tempids.contains(&name) {
                return Ok(EntityRef::Temp(name));
            }
        }
    }

    fn push(&mut self, op: TxOp, output: &mut Vec<TxOp>) -> Result<(), SemanticError> {
        self.primitive_count = self.primitive_count.checked_add(1).ok_or_else(|| {
            SemanticError::new(
                crate::ErrorCategory::Busy,
                "transaction/expansion-overflow",
                "transaction expansion operation count overflowed",
            )
        })?;
        if self.primitive_count > self.max_primitive_ops {
            return Err(SemanticError::new(
                crate::ErrorCategory::Busy,
                "postgres/transaction-op-capacity",
                format!(
                    "transaction expansion exceeds the configured {}-operation limit",
                    self.max_primitive_ops
                ),
            ));
        }
        output.push(op);
        Ok(())
    }

    fn expand_map(
        &mut self,
        map: &EntityMap,
        forced_id: Option<EntityRef>,
        depth: usize,
        output: &mut Vec<TxOp>,
    ) -> Result<EntityRef, SemanticError> {
        if depth > 32 {
            return Err(SemanticError::incorrect(
                "transaction/map-depth",
                "transaction entity maps may contain at most 32 nested maps",
            ));
        }
        let entity = match forced_id.or_else(|| map.id.clone()) {
            Some(entity) => entity,
            None => self.anonymous_tempid()?,
        };
        let mut attributes = map.attributes.clone();
        attributes.sort_by(compare_map_entry);
        for (attribute_ref, value) in &attributes {
            let (attribute_id, reverse) = self.resolve_attribute(attribute_ref)?;
            let attribute = self.db_before.schema().attribute(attribute_id)?;
            self.expand_map_value(
                entity.clone(),
                attribute_id,
                reverse,
                attribute.cardinality,
                attribute.value_type,
                attribute.component,
                value,
                depth,
                output,
            )?;
        }
        Ok(entity)
    }

    #[allow(clippy::too_many_arguments)]
    fn expand_map_value(
        &mut self,
        owner: EntityRef,
        attribute: u32,
        reverse: bool,
        cardinality: Cardinality,
        value_type: ValueType,
        component: bool,
        value: &MapValue,
        depth: usize,
        output: &mut Vec<TxOp>,
    ) -> Result<(), SemanticError> {
        match value {
            MapValue::Many(values) => {
                if !reverse && cardinality != Cardinality::Many {
                    return Err(SemanticError::incorrect(
                        "transaction/collection-on-cardinality-one",
                        "a collection map value requires a cardinality-many attribute",
                    ));
                }
                let mut values = values.clone();
                values.sort_by(compare_map_value);
                for value in &values {
                    if matches!(value, MapValue::Many(_)) {
                        return Err(SemanticError::incorrect(
                            "transaction/nested-collection",
                            "map value collections cannot contain collections",
                        ));
                    }
                    self.expand_map_value(
                        owner.clone(),
                        attribute,
                        reverse,
                        cardinality,
                        value_type,
                        component,
                        value,
                        depth,
                        output,
                    )?;
                }
                Ok(())
            }
            MapValue::Value(value) => {
                if reverse {
                    // Installation maps naturally name the database partition
                    // with a keyword, including maps emitted by stored programs.
                    // This does not install vocabulary absent from db-before.
                    let source = match value {
                        TxValue::Entity(source) => source.clone(),
                        TxValue::Scalar(Value::Keyword(ident))
                            if u64::from(attribute) == crate::DB_INSTALL_PARTITION =>
                        {
                            EntityRef::Ident(ident.clone())
                        }
                        TxValue::Scalar(Value::Ref(id))
                            if u64::from(attribute) == crate::DB_INSTALL_PARTITION =>
                        {
                            EntityRef::Id(*id)
                        }
                        _ => {
                            return Err(SemanticError::incorrect(
                                "transaction/reverse-value-must-be-entity",
                                "a reverse attribute value must identify an entity",
                            ));
                        }
                    };
                    self.push(
                        TxOp::Add {
                            entity: source,
                            attribute,
                            value: TxValue::Entity(owner),
                        },
                        output,
                    )
                } else {
                    self.push(
                        TxOp::Add {
                            entity: owner,
                            attribute,
                            value: value.clone(),
                        },
                        output,
                    )
                }
            }
            MapValue::Nested(nested) => {
                if value_type != ValueType::Ref {
                    return Err(SemanticError::incorrect(
                        "transaction/nested-map-requires-ref",
                        "nested maps require a ref-valued attribute",
                    ));
                }
                // Source expand-submap selects an explicit :db/id before
                // make-child-id's anonymous-creation guard. Identifying a
                // child is not conditional on also asserting its domain key.
                // Only a forward component edge owns the nested entity: in
                // reverse form the nested entity would own this outer one.
                let owned_child = component && !reverse;
                if nested.id.is_none() && !owned_child && !self.has_unique_attribute(nested)? {
                    return Err(SemanticError::incorrect(
                        "transaction/orphan-nested-map",
                        "a nested map requires :db/id, a forward component edge, or a unique attribute",
                    ));
                }
                if reverse {
                    let source = self.expand_map(nested, None, depth + 1, output)?;
                    self.push(
                        TxOp::Add {
                            entity: source,
                            attribute,
                            value: TxValue::Entity(owner),
                        },
                        output,
                    )
                } else {
                    let child = self.expand_map(nested, None, depth + 1, output)?;
                    // The source's map expander gives nested component maps
                    // affinity with their owner. Ordinary primitive ref edges
                    // deliberately do not acquire this authoring policy.
                    if component && let EntityRef::Temp(tempid) = &child {
                        self.push(
                            TxOp::MatchPartition {
                                tempid: tempid.clone(),
                                entity: owner.clone(),
                            },
                            output,
                        )?;
                    }
                    self.push(
                        TxOp::Add {
                            entity: owner,
                            attribute,
                            value: TxValue::Entity(child),
                        },
                        output,
                    )
                }
            }
        }
    }

    fn has_unique_attribute(&self, map: &EntityMap) -> Result<bool, SemanticError> {
        // Retain the documented component-or-unique authoring rule. The
        // recovered source's has-unique-id? is narrower (identity only), while
        // Atomic also admits unique-value keys. Their collision policy stays
        // in the assessor: a fresh unique-value tempid never becomes an upsert.
        for (attribute, value) in &map.attributes {
            if matches!(value, MapValue::Many(_) | MapValue::Nested(_)) {
                continue;
            }
            let (attribute, reverse) = self.resolve_attribute(attribute)?;
            if !reverse
                && self
                    .db_before
                    .schema()
                    .attribute(attribute)?
                    .unique
                    .is_some_and(|unique| matches!(unique, Unique::Identity | Unique::Value))
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn resolve_attribute(&self, attribute: &AttributeRef) -> Result<(u32, bool), SemanticError> {
        match attribute {
            AttributeRef::Id(id) => Ok((*id, false)),
            AttributeRef::ReverseId(id) => Ok((*id, true)),
            AttributeRef::Ident(ident) | AttributeRef::ReverseIdent(ident) => {
                let installation_reverse = matches!(attribute, AttributeRef::Ident(_))
                    && ident == &Keyword::new("db.install", "_partition");
                let normalized = if installation_reverse {
                    Keyword::new("db.install", "partition")
                } else {
                    ident.clone()
                };
                let entity = self.db_before.entid(&normalized).ok_or_else(|| {
                    SemanticError::incorrect(
                        "schema/unknown-attribute",
                        format!("unknown attribute {}", ident.qualified_name()),
                    )
                })?;
                let id = crate::schema_eid_to_attr_id(entity)?;
                self.db_before.schema().attribute(id)?;
                Ok((
                    id,
                    installation_reverse || matches!(attribute, AttributeRef::ReverseIdent(_)),
                ))
            }
        }
    }
}
