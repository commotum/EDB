//! EDN transaction syntax, retained unresolved until the authoritative db-before.
//!
//! This is an additive request representation, not a durable datom encoding or
//! another transaction evaluator. Existing typed requests keep their old hashes.
use crate::edn::{EdnValue, read_edn, write_edn};
use crate::{
    Attribute, AttributeRef, CallableRef, Cardinality, Database, DatabaseValue, EntityMap,
    EntityRef, Keyword, MapValue, ProgramCall, RuntimeValue, SemanticError, TransactionRequest,
    TupleSpec, TxForm, TxFunctions, TxOp, TxValue, Value, ValueType,
};
use std::sync::Arc;

/// A validated, canonical, schema-independent transaction form. Fields are
/// private so native callers cannot bypass EDN structural admission.
#[derive(Clone, Debug)]
pub struct EdnTransactionForm {
    value: Arc<EdnValue>,
    canonical: Arc<str>,
}

impl EdnTransactionForm {
    pub fn from_value(value: EdnValue) -> Result<Self, SemanticError> {
        // Validate depth, byte limits and EDN duplicate keys before recursion.
        write_edn(&value)?;
        if !matches!(
            value,
            EdnValue::Map(_) | EdnValue::List(_) | EdnValue::Vector(_)
        ) {
            return Err(error(
                "edn/transaction-form",
                "a transaction form must be a map, list or vector",
            ));
        }
        let value = canonicalize(value)?;
        let canonical = write_edn(&value)?.into();
        Ok(Self {
            value: Arc::new(value),
            canonical,
        })
    }

    pub fn value(&self) -> &EdnValue {
        &self.value
    }
    pub fn canonical_edn(&self) -> &str {
        &self.canonical
    }

    pub(crate) fn decode(text: &str) -> Result<Self, SemanticError> {
        let form = Self::from_value(read_edn(text)?)?;
        if form.canonical_edn() != text {
            return Err(error(
                "transport/noncanonical-edn",
                "EDN transaction form is not canonical",
            ));
        }
        Ok(form)
    }
}

fn canonicalize(value: EdnValue) -> Result<EdnValue, SemanticError> {
    Ok(match value {
        EdnValue::Map(entries) => {
            let mut entries = entries
                .into_iter()
                .map(|(key, value)| {
                    let key = canonicalize(key)?;
                    let value = canonicalize(value)?;
                    Ok((write_edn(&key)?, key, value))
                })
                .collect::<Result<Vec<_>, SemanticError>>()?;
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            EdnValue::Map(
                entries
                    .into_iter()
                    .map(|(_, key, value)| (key, value))
                    .collect(),
            )
        }
        EdnValue::Set(values) => {
            let mut values = values
                .into_iter()
                .map(|value| {
                    let value = canonicalize(value)?;
                    Ok((write_edn(&value)?, value))
                })
                .collect::<Result<Vec<_>, SemanticError>>()?;
            values.sort_by(|left, right| left.0.cmp(&right.0));
            EdnValue::Set(values.into_iter().map(|(_, value)| value).collect())
        }
        EdnValue::List(values) => EdnValue::List(
            values
                .into_iter()
                .map(canonicalize)
                .collect::<Result<_, _>>()?,
        ),
        EdnValue::Vector(values) => EdnValue::Vector(
            values
                .into_iter()
                .map(canonicalize)
                .collect::<Result<_, _>>()?,
        ),
        EdnValue::Tagged(tag, value) => EdnValue::Tagged(tag, Box::new(canonicalize(*value)?)),
        value => value,
    })
}

/// Read one transaction collection. Whitespace/comments, map order, set order
/// and outer transaction order do not affect its request identity. Ordered
/// list/vector contents and numeric representations remain request data.
pub fn read_edn_transaction(text: &str) -> Result<Vec<TxForm>, SemanticError> {
    let value = read_edn(text)?;
    let values = match value {
        EdnValue::List(values) | EdnValue::Vector(values) => values,
        _ => {
            return Err(error(
                "edn/transaction-data",
                "transaction data must be a list or vector of forms",
            ));
        }
    };
    values
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            EdnTransactionForm::from_value(value)
                .map(TxForm::Edn)
                .map_err(|error| form_error(error, index))
        })
        .collect()
}

impl TransactionRequest {
    pub fn from_edn(request_key: impl Into<String>, text: &str) -> Result<Self, SemanticError> {
        Ok(Self::from_forms(request_key, read_edn_transaction(text)?))
    }
}

impl DatabaseValue {
    pub fn with_edn_with_defaults(
        &self,
        text: &str,
        tx_instant: i64,
        defaults: &crate::TransactionDefaults,
    ) -> Result<crate::SpeculativeTransactionReport, SemanticError> {
        self.with_forms_with_defaults(&read_edn_transaction(text)?, tx_instant, defaults)
    }

    pub fn with_edn(
        &self,
        text: &str,
        tx_instant: i64,
    ) -> Result<crate::SpeculativeTransactionReport, SemanticError> {
        self.with_forms(&read_edn_transaction(text)?, tx_instant)
    }

    /// The existing operation/read/program admission applies to the complete
    /// authoritative-style expansion, including schema lookup and generated
    /// forms. Text uses the shared reader's bounded default admission.
    pub fn with_edn_with_limits(
        &self,
        text: &str,
        tx_instant: i64,
        limits: crate::SpeculationLimits,
    ) -> Result<crate::SpeculativeTransactionReport, SemanticError> {
        self.with_forms_with_limits(&read_edn_transaction(text)?, tx_instant, limits)
    }
}

impl Database {
    pub fn with_edn_with_defaults(
        &self,
        text: &str,
        tx_instant: i64,
        defaults: &crate::TransactionDefaults,
    ) -> Result<crate::TxReport, SemanticError> {
        self.with_forms_with_defaults(
            &read_edn_transaction(text)?,
            &TxFunctions::new(),
            tx_instant,
            defaults,
        )
    }

    pub fn with_edn(&self, text: &str, tx_instant: i64) -> Result<crate::TxReport, SemanticError> {
        self.with_forms(
            &read_edn_transaction(text)?,
            &TxFunctions::new(),
            tx_instant,
        )
    }
}

pub(crate) fn lower_forms(
    database: &DatabaseValue,
    forms: &[TxForm],
) -> Result<Vec<TxForm>, SemanticError> {
    let mut lowered = Vec::with_capacity(forms.len());
    let adapter = Adapter { database };
    for (index, form) in forms.iter().enumerate() {
        if let TxForm::Edn(form) = form {
            adapter
                .form(form.value(), &mut lowered)
                .map_err(|error| form_error(error, index))?;
        } else {
            lowered.push(form.clone());
        }
    }
    crate::transaction::input::validate_forms_input(&lowered)?;
    Ok(lowered)
}

struct Adapter<'a> {
    database: &'a DatabaseValue,
}

impl Adapter<'_> {
    fn form(&self, value: &EdnValue, output: &mut Vec<TxForm>) -> Result<(), SemanticError> {
        if let EdnValue::Map(entries) = value {
            // Partition policy maps are authoring directives, never attributes.
            let mut attributes = Vec::new();
            for (key, value) in entries {
                let directive = keyword_name(key);
                if matches!(
                    directive.as_deref(),
                    Some("db/force-partition" | "db/match-partition")
                ) {
                    let EdnValue::Map(policies) = value else {
                        return Err(error(
                            "edn/partition-directive",
                            "partition directives require a tempid-to-entity map",
                        ));
                    };
                    for (tempid, target) in policies {
                        let EntityRef::Temp(tempid) = self.entity(tempid)? else {
                            return Err(error(
                                "edn/partition-tempid",
                                "partition directive keys must be ordinary string tempids",
                            ));
                        };
                        let target = self.entity(target)?;
                        output.push(TxForm::Op(
                            if directive.as_deref() == Some("db/force-partition") {
                                TxOp::ForcePartition {
                                    tempid,
                                    partition: target,
                                }
                            } else {
                                TxOp::MatchPartition {
                                    tempid,
                                    entity: target,
                                }
                            },
                        ));
                    }
                } else {
                    attributes.push((key.clone(), value.clone()));
                }
            }
            if !attributes.is_empty() || entries.is_empty() {
                output.push(TxForm::EntityMap(self.entity_map(&attributes)?));
            }
            return Ok(());
        }
        let values = sequence(value).ok_or_else(|| {
            error(
                "edn/transaction-form",
                "transaction form must be a map or sequential form",
            )
        })?;
        let Some(head) = values.first() else {
            return Err(error(
                "edn/transaction-arity",
                "transaction form cannot be empty",
            ));
        };
        let operation = keyword_name(head);
        let op = match operation.as_deref() {
            Some("db/add" | "db/retract") => {
                let add = operation.as_deref() == Some("db/add");
                if (add && values.len() != 4) || (!add && !(3..=4).contains(&values.len())) {
                    return Err(error(
                        "edn/transaction-arity",
                        "add requires E/A/V; retract requires E/A and optional V",
                    ));
                }
                let entity = self.entity(&values[1])?;
                let attribute = self.attribute(&values[2])?;
                let value = values
                    .get(3)
                    .map(|value| self.tx_value(self.database.schema().attribute(attribute)?, value))
                    .transpose()?;
                if add {
                    TxOp::Add {
                        entity,
                        attribute,
                        value: value.expect("add arity checked"),
                    }
                } else {
                    TxOp::Retract {
                        entity,
                        attribute,
                        value,
                    }
                }
            }
            Some("db/cas" | "db.fn/cas") => {
                require_arity(values, 5)?;
                let attribute = self.attribute(&values[2])?;
                let descriptor = self.database.schema().attribute(attribute)?;
                TxOp::Cas {
                    entity: self.entity(&values[1])?,
                    attribute,
                    old: if matches!(values[3], EdnValue::Nil) {
                        None
                    } else {
                        Some(self.tx_value(descriptor, &values[3])?)
                    },
                    new: self.tx_value(descriptor, &values[4])?,
                }
            }
            Some("db/retractEntity" | "db.fn/retractEntity") => {
                require_arity(values, 2)?;
                TxOp::RetractEntity(self.entity(&values[1])?)
            }
            Some("db/ensure") => {
                require_arity(values, 3)?;
                TxOp::Ensure {
                    entity: self.entity(&values[1])?,
                    spec: self.entity(&values[2])?,
                }
            }
            _ => {
                let function = if let EdnValue::Symbol(symbol) = head {
                    // The existing native explicit registry decides whether this
                    // symbol is callable. Reading it never loads/evaluates code.
                    CallableRef::Local(symbol.clone())
                } else if let EdnValue::Tagged(tag, _) = head {
                    if tag.qualified_name() == "atomic/function" {
                        let Value::Function(hash) = crate::edn_value::edn_to_value(head)? else {
                            unreachable!()
                        };
                        CallableRef::ExactHash(hash)
                    } else {
                        CallableRef::Database(self.entity(head)?)
                    }
                } else {
                    CallableRef::Database(self.entity(head)?)
                };
                let arguments = values[1..]
                    .iter()
                    .map(|value| self.runtime(value))
                    .collect::<Result<_, _>>()?;
                output.push(TxForm::ProgramCall(ProgramCall {
                    function,
                    arguments,
                }));
                return Ok(());
            }
        };
        output.push(TxForm::Op(op));
        Ok(())
    }

    fn attribute(&self, value: &EdnValue) -> Result<u32, SemanticError> {
        let entity = self.entity(value)?;
        let id = match entity {
            EntityRef::Id(id) => Some(id),
            EntityRef::Ident(ident) => self.database.entid(&ident),
            EntityRef::Lookup { attribute, value } => self.database.lookup(attribute, &value)?,
            EntityRef::LookupInput { attribute, value } => {
                self.database.resolve_lookup_input(attribute, &value)?
            }
            _ => {
                return Err(error(
                    "edn/attribute",
                    "attribute identifiers cannot be tempids or the current transaction",
                ));
            }
        }
        .ok_or_else(|| {
            error(
                "schema/unknown-attribute",
                "EDN attribute does not resolve in db-before",
            )
        })?;
        let attribute = crate::schema_eid_to_attr_id(id)?;
        self.database.schema().attribute(attribute)?;
        Ok(attribute)
    }

    fn entity(&self, value: &EdnValue) -> Result<EntityRef, SemanticError> {
        Ok(match value {
            EdnValue::Long(id) if *id >= 0 => EntityRef::Id(*id as u64),
            EdnValue::Keyword(ident) => EntityRef::Ident(ident.clone()),
            EdnValue::String(tempid) if tempid == "datomic.tx" => EntityRef::Tx,
            EdnValue::String(tempid) if !tempid.is_empty() && !tempid.starts_with(':') => {
                EntityRef::Temp(tempid.clone())
            }
            EdnValue::List(values) | EdnValue::Vector(values) if values.len() == 2 => {
                let attribute = self.attribute(&values[0])?;
                let descriptor = self.database.schema().attribute(attribute)?;
                if descriptor.unique.is_none() {
                    return Err(error(
                        "transaction/lookup-non-unique",
                        "lookup ref requires a unique attribute",
                    ));
                }
                let value = self.tx_value(descriptor, &values[1])?;
                match value {
                    TxValue::Scalar(value) => EntityRef::Lookup { attribute, value },
                    value => EntityRef::LookupInput {
                        attribute,
                        value: Box::new(value),
                    },
                }
            }
            EdnValue::Tagged(tag, _) if tag.qualified_name() == "atomic/ref" => {
                let Value::Ref(id) = crate::edn_value::edn_to_value(value)? else {
                    unreachable!()
                };
                EntityRef::Id(id)
            }
            _ => {
                return Err(error(
                    "edn/entity",
                    "entity requires an existing ID, ident, tempid string or lookup reference",
                ));
            }
        })
    }

    fn entity_map(&self, entries: &[(EdnValue, EdnValue)]) -> Result<EntityMap, SemanticError> {
        let mut id = None;
        let mut attributes = Vec::with_capacity(entries.len());
        for (key, value) in entries {
            let EdnValue::Keyword(ident) = key else {
                return Err(error(
                    "edn/map-key",
                    "transaction map keys must be attribute keywords",
                ));
            };
            if ident == &Keyword::new("db", "id") {
                // Recovered expand-map treats an absent/nil :db/id alike.
                id = if matches!(value, EdnValue::Nil) {
                    None
                } else {
                    Some(self.entity(value)?)
                };
                continue;
            }
            let (ident, reverse) = if let Some(name) = ident.name.strip_prefix('_') {
                if name.is_empty() {
                    return Err(error(
                        "edn/reverse-attribute",
                        "reverse attribute must name an attribute after '_'",
                    ));
                }
                (
                    Keyword {
                        namespace: ident.namespace.clone(),
                        name: name.into(),
                    },
                    true,
                )
            } else {
                (ident.clone(), false)
            };
            let attribute = self.attribute(&EdnValue::Keyword(ident.clone()))?;
            let descriptor = self.database.schema().attribute(attribute)?;
            let value = self.map_value(descriptor, reverse, value)?;
            attributes.push((
                if reverse {
                    AttributeRef::ReverseIdent(ident)
                } else {
                    AttributeRef::Ident(ident)
                },
                value,
            ));
        }
        Ok(EntityMap { id, attributes })
    }

    fn map_value(
        &self,
        attribute: &Attribute,
        reverse: bool,
        value: &EdnValue,
    ) -> Result<MapValue, SemanticError> {
        if let EdnValue::Map(entries) = value {
            return Ok(MapValue::Nested(Box::new(self.entity_map(entries)?)));
        }
        if let Some(values) = collection(value) {
            // Recovered expand-map enumerates forward sequences only for Many;
            // a many-ref lookup is therefore wrapped in an outer collection.
            // One-valued tuple/ref sequences stay a single value. Reverse map
            // navigation also retains the native explicit multi-owner surface.
            let reverse_lookup = reverse
                && sequence(value).is_some()
                && values.len() == 2
                && self.lookup_attribute(&values[0])?.is_some();
            let many = matches!(value, EdnValue::Set(_))
                || (!reverse && attribute.cardinality == Cardinality::Many)
                || (reverse && !reverse_lookup);
            if many {
                return Ok(MapValue::Many(
                    values
                        .iter()
                        .map(|value| {
                            if let EdnValue::Map(entries) = value {
                                Ok(MapValue::Nested(Box::new(self.entity_map(entries)?)))
                            } else if reverse {
                                Ok(MapValue::Value(TxValue::Entity(self.entity(value)?)))
                            } else {
                                Ok(MapValue::Value(self.tx_value(attribute, value)?))
                            }
                        })
                        .collect::<Result<_, _>>()?,
                ));
            }
        }
        if reverse {
            Ok(MapValue::Value(TxValue::Entity(self.entity(value)?)))
        } else {
            Ok(MapValue::Value(self.tx_value(attribute, value)?))
        }
    }

    // Ordinary refs need only metadata to distinguish a lookup from Many.
    // A nested lookup can itself identify an attribute; that read must retain
    // its exact-view admission and must never swallow a storage/budget error.
    fn lookup_attribute(&self, value: &EdnValue) -> Result<Option<u32>, SemanticError> {
        let id = match value {
            EdnValue::Long(id) if *id >= 0 => Some(*id as u64),
            EdnValue::Keyword(ident) => self.database.entid(ident),
            EdnValue::List(values) | EdnValue::Vector(values) if values.len() == 2 => {
                match self.entity(value)? {
                    EntityRef::Lookup { attribute, value } => {
                        self.database.lookup(attribute, &value)?
                    }
                    EntityRef::LookupInput { attribute, value } => {
                        self.database.resolve_lookup_input(attribute, &value)?
                    }
                    _ => unreachable!("sequential identifiers are lookup refs"),
                }
            }
            _ => None,
        };
        Ok(id.and_then(|id| u32::try_from(id).ok()).filter(|id| {
            self.database
                .schema()
                .attribute(*id)
                .is_ok_and(|a| a.unique.is_some())
        }))
    }

    fn tx_value(&self, attribute: &Attribute, value: &EdnValue) -> Result<TxValue, SemanticError> {
        if attribute.value_type == ValueType::Ref {
            return Ok(TxValue::Entity(self.entity(value)?));
        }
        if attribute.value_type == ValueType::Tuple {
            let values = sequence(value)
                .ok_or_else(|| error("edn/tuple", "tuple values require a list or vector"))?;
            let types = match &attribute.tuple {
                Some(TupleSpec::Homogeneous(kind)) => vec![*kind; values.len()],
                Some(TupleSpec::Heterogeneous(kinds)) => kinds.clone(),
                Some(TupleSpec::Composite(attributes)) => attributes
                    .iter()
                    .map(|id| self.database.schema().attribute(*id).map(|a| a.value_type))
                    .collect::<Result<_, _>>()?,
                None => {
                    return Err(error(
                        "edn/tuple-schema",
                        "tuple attribute has no tuple specification",
                    ));
                }
            };
            if !(2..=8).contains(&values.len()) || types.len() != values.len() {
                return Err(error(
                    "transaction/invalid-input-tuple",
                    "tuple arity must match its 2–8 slot schema",
                ));
            }
            return Ok(TxValue::Tuple(
                values
                    .iter()
                    .zip(types)
                    .map(|(value, kind)| {
                        if matches!(value, EdnValue::Nil) {
                            Ok(None)
                        } else if kind == ValueType::Ref {
                            self.entity(value).map(TxValue::Entity).map(Some)
                        } else {
                            crate::edn_value::edn_to_value(value)
                                .map(TxValue::Scalar)
                                .map(Some)
                        }
                    })
                    .collect::<Result<_, _>>()?,
            ));
        }
        Ok(TxValue::Scalar(crate::edn_value::edn_to_value(value)?))
    }

    fn runtime(&self, value: &EdnValue) -> Result<RuntimeValue, SemanticError> {
        Ok(match value {
            EdnValue::Nil => RuntimeValue::Null,
            EdnValue::List(values) | EdnValue::Vector(values) => RuntimeValue::Vector(
                values
                    .iter()
                    .map(|value| self.runtime(value))
                    .collect::<Result<_, _>>()?,
            ),
            EdnValue::Map(entries) => RuntimeValue::map(
                entries
                    .iter()
                    .map(|(key, value)| {
                        Ok((crate::edn_value::edn_to_value(key)?, self.runtime(value)?))
                    })
                    .collect::<Result<_, SemanticError>>()?,
            )?,
            EdnValue::Tagged(tag, _) if tag.qualified_name() == "atomic/ref" => {
                RuntimeValue::Entity(self.entity(value)?)
            }
            _ => RuntimeValue::Scalar(crate::edn_value::edn_to_value(value)?),
        })
    }
}

fn sequence(value: &EdnValue) -> Option<&[EdnValue]> {
    match value {
        EdnValue::List(values) | EdnValue::Vector(values) => Some(values),
        _ => None,
    }
}
fn collection(value: &EdnValue) -> Option<&[EdnValue]> {
    match value {
        EdnValue::Set(values) => Some(values),
        _ => sequence(value),
    }
}
fn keyword_name(value: &EdnValue) -> Option<String> {
    if let EdnValue::Keyword(keyword) = value {
        Some(keyword.qualified_name())
    } else {
        None
    }
}
fn require_arity(values: &[EdnValue], arity: usize) -> Result<(), SemanticError> {
    if values.len() == arity {
        Ok(())
    } else {
        Err(error(
            "edn/transaction-arity",
            "transaction function has the wrong argument count",
        ))
    }
}
fn error(code: &'static str, message: &'static str) -> SemanticError {
    SemanticError::incorrect(code, message)
}

fn form_error(mut error: SemanticError, index: usize) -> SemanticError {
    error
        .details
        .entry("edn_path".into())
        .or_insert_with(|| format!("tx[{index}]"));
    error
}
