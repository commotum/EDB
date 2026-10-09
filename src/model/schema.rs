//! Attribute contracts projected from ordinary schema datoms.
//!
//! Descriptors and ident indexes are derived read models, not a second catalog.
//! Validation is shared by speculative, in-memory and durable assessment; this
//! module owns no transaction service, PostgreSQL connection or publication.

use crate::model::idents::IdentIndex;
use crate::model::vocabulary::{
    DB_ALTER_ATTRIBUTE, DB_ATTR_PREDS, DB_CARDINALITY, DB_IDENT, DB_INDEX, DB_INSTALL_ATTRIBUTE,
    DB_IS_COMPONENT, DB_NO_HISTORY, DB_PART_DB, DB_TUPLE_ATTRS, DB_TUPLE_DISCONTINUED,
    DB_TUPLE_TYPE, DB_TUPLE_TYPES, DB_UNIQUE, DB_VALUE_TYPE, cardinality_entity,
    cardinality_for_entity, schema_eid_to_attr_id, unique_entity, unique_for_entity,
    value_type_entity, value_type_for_entity, value_type_ident,
};
use crate::{Datom, ErrorCategory, IndexOrder, Keyword, SemanticError, Symbol, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::mem::size_of;

pub type AttrId = u32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Cardinality {
    One,
    Many,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Unique {
    Identity,
    Value,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValueType {
    BigDec,
    BigInt,
    Boolean,
    Bytes,
    Double,
    Float,
    Function,
    Instant,
    Keyword,
    Long,
    Ref,
    String,
    Symbol,
    Tuple,
    Uuid,
    Uri,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TupleSpec {
    Homogeneous(ValueType),
    Heterogeneous(Vec<ValueType>),
    Composite(Vec<AttrId>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Attribute {
    pub id: AttrId,
    pub ident: Keyword,
    pub value_type: ValueType,
    pub cardinality: Cardinality,
    pub unique: Option<Unique>,
    pub indexed: bool,
    pub component: bool,
    pub no_history: bool,
    /// Eventually consistent analyzed search; immutable after installation.
    pub fulltext: bool,
    pub tuple: Option<TupleSpec>,
    pub tuple_discontinued: bool,
    /// Names of deterministic value predicates. The executing host supplies
    /// native deployments or resolves stored-program bindings.
    pub predicates: Vec<String>,
}

impl Attribute {
    pub fn new(
        id: AttrId,
        ident: Keyword,
        value_type: ValueType,
        cardinality: Cardinality,
    ) -> Self {
        Self {
            id,
            ident,
            value_type,
            cardinality,
            unique: None,
            indexed: false,
            component: false,
            no_history: false,
            fulltext: false,
            tuple: None,
            tuple_discontinued: false,
            predicates: Vec::new(),
        }
    }

    pub fn unique(mut self, unique: Unique) -> Self {
        self.unique = Some(unique);
        self
    }

    pub fn component(mut self) -> Self {
        self.component = true;
        self
    }

    pub fn fulltext(mut self) -> Self {
        self.fulltext = true;
        self
    }

    pub fn tuple(mut self, tuple: TupleSpec) -> Self {
        self.tuple = Some(tuple);
        self
    }

    pub fn predicate(mut self, name: impl Into<String>) -> Self {
        self.predicates.push(name.into());
        self.predicates.sort();
        self.predicates.dedup();
        self
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Schema {
    attributes: BTreeMap<AttrId, Attribute>,
    current_idents: BTreeMap<Keyword, AttrId>,
    ident_aliases: BTreeMap<Keyword, AttrId>,
    /// Active composite attributes keyed by each constituent. Recovered
    /// `Db.constituents` uses this exact reverse projection so ordinary
    /// transaction expansion touches only composites affected by an E/A
    /// change rather than scanning the complete schema.
    constituents: BTreeMap<AttrId, BTreeSet<AttrId>>,
    /// Derived dependency names, counted so changing one attribute does not
    /// remove a predicate still used by another. Ordinary ident transactions
    /// can test dependency without enumerating all attribute definitions.
    attribute_predicate_refs: BTreeMap<String, usize>,
    /// Named partition installation is ordinary information, not a new store.
    partitions: BTreeMap<u32, Keyword>,
}

/// Actual visits inside schema definition validation, not an estimate from
/// schema width. Native assessment uses this across every validation phase.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct SchemaValidationWork {
    pub(crate) attributes: u64,
    pub(crate) predicates: u64,
    pub(crate) tuple_types: u64,
    pub(crate) tuple_constituents: u64,
    pub(crate) installations: u64,
}

fn active_composite_constituents(attribute: &Attribute) -> Vec<AttrId> {
    if attribute.tuple_discontinued {
        return Vec::new();
    }
    match &attribute.tuple {
        Some(TupleSpec::Composite(constituents)) => constituents.clone(),
        _ => Vec::new(),
    }
}

impl Schema {
    /// Whether one datom can change the operative schema projection. General
    /// idents still update the separate identity index, but need not rebuild
    /// unchanged attributes. Retargeting an attribute's current/old name does
    /// affect schema aliases and potentially composite constituent resolution.
    pub(crate) fn datom_may_change_schema(
        &self,
        entity: u64,
        attribute: AttrId,
        value: &Value,
        added: bool,
    ) -> bool {
        if u64::from(attribute) == DB_IDENT {
            let installed_target = u32::try_from(entity).is_ok_and(|id| {
                self.attributes.contains_key(&id) || self.partitions.contains_key(&id)
            });
            return installed_target
                || added
                    && match value {
                        Value::Keyword(ident) => self.resolve_ident(ident).is_some(),
                        _ => true,
                    };
        }
        matches!(
            u64::from(attribute),
            crate::DB_INSTALL_PARTITION
                | DB_INSTALL_ATTRIBUTE
                | DB_ALTER_ATTRIBUTE
                | DB_VALUE_TYPE
                | DB_CARDINALITY
                | DB_UNIQUE
                | DB_IS_COMPONENT
                | DB_INDEX
                | DB_NO_HISTORY
                | crate::DB_FULLTEXT
                | DB_TUPLE_TYPE
                | DB_TUPLE_TYPES
                | DB_TUPLE_ATTRS
                | DB_TUPLE_DISCONTINUED
                | DB_ATTR_PREDS
        )
    }

    /// Installed named partitions, including the three builtin partitions.
    /// Implicit partitions are computed by `implicit_part`, not enumerated.
    pub fn partitions(&self) -> impl Iterator<Item = (u32, &Keyword)> {
        self.partitions.iter().map(|(id, ident)| (*id, ident))
    }

    pub(crate) fn validate_partition_bits(&self, bits: u32) -> Result<(), SemanticError> {
        if matches!(
            bits,
            crate::DB_PARTITION | crate::TX_PARTITION | crate::USER_PARTITION
        ) || (crate::model::identity::IMPLICIT_PARTITION_BASE..=crate::MAX_PARTITION)
            .contains(&bits)
            || self.partitions.contains_key(&bits)
        {
            return Ok(());
        }
        Err(SemanticError::incorrect(
            "transaction/not-a-partition",
            format!("partition {bits} is not installed in db-before"),
        ))
    }

    pub(crate) fn validate_partition_successor(
        &self,
        successor: &Self,
    ) -> Result<(), SemanticError> {
        if self
            .partitions
            .keys()
            .any(|partition| !successor.partitions.contains_key(partition))
        {
            return Err(SemanticError::incorrect(
                "schema/partition-install-immutable",
                "installed partitions cannot be removed",
            ));
        }
        Ok(())
    }
    pub fn new() -> Self {
        Self::default()
    }

    pub fn install(&mut self, attribute: Attribute) -> Result<(), SemanticError> {
        self.install_with_work(attribute, &mut SchemaValidationWork::default())
    }

    pub(crate) fn install_with_work(
        &mut self,
        attribute: Attribute,
        work: &mut SchemaValidationWork,
    ) -> Result<(), SemanticError> {
        self.validate_attribute_with_work(&attribute, work)?;
        if self.attributes.contains_key(&attribute.id) {
            return Err(SemanticError::conflict(
                "schema/attribute-id-exists",
                format!("attribute id {} is already installed", attribute.id),
            ));
        }
        if let Some(existing) = self.current_idents.get(&attribute.ident) {
            return Err(SemanticError::conflict(
                "schema/ident-exists",
                format!(
                    "ident {} already names attribute {existing}",
                    attribute.ident.qualified_name()
                ),
            ));
        }
        self.current_idents
            .insert(attribute.ident.clone(), attribute.id);
        let attribute_id = attribute.id;
        let constituent_ids = active_composite_constituents(&attribute);
        for predicate in &attribute.predicates {
            *self
                .attribute_predicate_refs
                .entry(predicate.clone())
                .or_default() += 1;
        }
        self.attributes.insert(attribute_id, attribute);
        for constituent in constituent_ids {
            self.constituents
                .entry(constituent)
                .or_default()
                .insert(attribute_id);
        }
        Ok(())
    }

    /// Renaming preserves the old ident as an alias, matching documented
    /// entity resolution while making the new ident canonical.
    pub fn rename(&mut self, id: AttrId, ident: Keyword) -> Result<(), SemanticError> {
        if let Some(existing) = self.current_idents.get(&ident)
            && *existing != id
        {
            return Err(SemanticError::conflict(
                "schema/ident-exists",
                format!("ident {} is already in use", ident.qualified_name()),
            ));
        }
        let attribute = self.attributes.get_mut(&id).ok_or_else(|| {
            SemanticError::incorrect(
                "schema/unknown-attribute",
                format!("unknown attribute id {id}"),
            )
        })?;
        let old = std::mem::replace(&mut attribute.ident, ident.clone());
        self.current_idents.remove(&old);
        self.ident_aliases.insert(old, id);
        self.current_idents.insert(ident, id);
        Ok(())
    }

    pub(crate) fn alter_with_work(
        &mut self,
        proposed: Attribute,
        work: &mut SchemaValidationWork,
    ) -> Result<(), SemanticError> {
        self.validate_attribute_with_work(&proposed, work)?;
        let current = self.attributes.get(&proposed.id).ok_or_else(|| {
            SemanticError::incorrect(
                "schema/unknown-attribute",
                format!("unknown attribute id {}", proposed.id),
            )
        })?;
        if current.value_type != proposed.value_type {
            return Err(SemanticError::incorrect(
                "schema/value-type-immutable",
                "an installed attribute's value type cannot change",
            ));
        }
        if current.fulltext != proposed.fulltext {
            return Err(SemanticError::incorrect(
                "schema/fulltext-immutable",
                "an installed attribute's fulltext property cannot change",
            ));
        }
        if current.tuple != proposed.tuple {
            return Err(SemanticError::incorrect(
                "schema/tuple-definition-immutable",
                "an installed tuple definition cannot change",
            ));
        }
        if current.tuple_discontinued && !proposed.tuple_discontinued {
            return Err(SemanticError::incorrect(
                "schema/tuple-discontinuation-irreversible",
                "a discontinued composite tuple cannot be resumed",
            ));
        }

        if current.ident != proposed.ident {
            if let Some(existing) = self.current_idents.get(&proposed.ident)
                && *existing != proposed.id
            {
                return Err(SemanticError::conflict(
                    "schema/ident-exists",
                    format!(
                        "ident {} is already in use",
                        proposed.ident.qualified_name()
                    ),
                ));
            }
            let old_ident = current.ident.clone();
            self.current_idents.remove(&old_ident);
            self.ident_aliases.insert(old_ident, proposed.id);
            self.current_idents
                .insert(proposed.ident.clone(), proposed.id);
        }
        let previous_constituents = active_composite_constituents(current);
        for predicate in &current.predicates {
            let count = self
                .attribute_predicate_refs
                .get_mut(predicate)
                .expect("installed attribute predicate has a dependency count");
            *count -= 1;
            if *count == 0 {
                self.attribute_predicate_refs.remove(predicate);
            }
        }
        for predicate in &proposed.predicates {
            *self
                .attribute_predicate_refs
                .entry(predicate.clone())
                .or_default() += 1;
        }
        let proposed_id = proposed.id;
        let proposed_constituents = active_composite_constituents(&proposed);
        self.attributes.insert(proposed_id, proposed);
        for constituent in previous_constituents {
            if let Some(composites) = self.constituents.get_mut(&constituent) {
                composites.remove(&proposed_id);
                if composites.is_empty() {
                    self.constituents.remove(&constituent);
                }
            }
        }
        for constituent in proposed_constituents {
            self.constituents
                .entry(constituent)
                .or_default()
                .insert(proposed_id);
        }
        Ok(())
    }

    pub fn attribute(&self, id: AttrId) -> Result<&Attribute, SemanticError> {
        self.attributes.get(&id).ok_or_else(|| {
            SemanticError::incorrect(
                "schema/unknown-attribute",
                format!("unknown attribute id {id}"),
            )
        })
    }

    pub fn resolve_ident(&self, ident: &Keyword) -> Option<AttrId> {
        self.current_idents
            .get(ident)
            .or_else(|| self.ident_aliases.get(ident))
            .copied()
    }

    pub fn attributes(&self) -> impl Iterator<Item = &Attribute> {
        self.attributes.values()
    }

    pub(crate) fn attribute_count(&self) -> usize {
        self.attributes.len()
    }

    pub(crate) fn has_attribute_predicate(&self, name: &str) -> bool {
        self.attribute_predicate_refs.contains_key(name)
    }

    pub(crate) fn attribute_predicate_names(&self) -> impl Iterator<Item = &str> {
        self.attribute_predicate_refs.keys().map(String::as_str)
    }

    pub(crate) fn estimated_retained_bytes(&self) -> u64 {
        fn keyword_heap_bytes(keyword: &Keyword) -> u64 {
            keyword
                .namespace
                .as_ref()
                .map_or(0, |namespace| namespace.capacity() as u64)
                .saturating_add(keyword.name.capacity() as u64)
        }

        fn keyword_bytes(keyword: &Keyword) -> u64 {
            (size_of::<Keyword>() as u64).saturating_add(keyword_heap_bytes(keyword))
        }

        fn attribute_bytes(attribute: &Attribute) -> u64 {
            let tuple = match &attribute.tuple {
                Some(TupleSpec::Heterogeneous(types)) => {
                    (types.capacity() as u64).saturating_mul(size_of::<ValueType>() as u64)
                }
                Some(TupleSpec::Composite(attributes)) => {
                    (attributes.capacity() as u64).saturating_mul(size_of::<AttrId>() as u64)
                }
                Some(TupleSpec::Homogeneous(_)) | None => 0,
            };
            let predicates = (attribute.predicates.capacity() as u64)
                .saturating_mul(size_of::<String>() as u64)
                .saturating_add(attribute.predicates.iter().fold(0_u64, |bytes, predicate| {
                    bytes.saturating_add(predicate.capacity() as u64)
                }));
            (size_of::<Attribute>() as u64)
                .saturating_add(keyword_heap_bytes(&attribute.ident))
                .saturating_add(tuple)
                .saturating_add(predicates)
        }

        let attributes = self.attributes.values().fold(0_u64, |bytes, attribute| {
            bytes
                .saturating_add(size_of::<AttrId>() as u64)
                .saturating_add(attribute_bytes(attribute))
        });
        let current_idents = self.current_idents.keys().fold(0_u64, |bytes, ident| {
            bytes
                .saturating_add(keyword_bytes(ident))
                .saturating_add(size_of::<AttrId>() as u64)
        });
        let ident_aliases = self.ident_aliases.keys().fold(0_u64, |bytes, ident| {
            bytes
                .saturating_add(keyword_bytes(ident))
                .saturating_add(size_of::<AttrId>() as u64)
        });
        let constituents = self.constituents.values().fold(0_u64, |bytes, composites| {
            bytes
                .saturating_add(size_of::<AttrId>() as u64)
                .saturating_add(
                    (composites.len() as u64).saturating_mul(size_of::<AttrId>() as u64),
                )
        });
        let partitions = self.partitions.values().fold(0_u64, |bytes, ident| {
            bytes
                .saturating_add(size_of::<u32>() as u64)
                .saturating_add(keyword_bytes(ident))
        });
        let predicate_refs = self
            .attribute_predicate_refs
            .keys()
            .fold(0_u64, |bytes, name| {
                bytes
                    .saturating_add(size_of::<String>() as u64)
                    .saturating_add(name.capacity() as u64)
                    .saturating_add(size_of::<usize>() as u64)
            });
        (size_of::<Self>() as u64)
            .saturating_add(attributes)
            .saturating_add(current_idents)
            .saturating_add(ident_aliases)
            .saturating_add(constituents)
            .saturating_add(partitions)
            .saturating_add(predicate_refs)
    }

    /// Active composite attributes that depend on `constituent`.
    ///
    /// Discontinued composites are absent, matching recovered
    /// `discontinue-composite`, which removes their reverse links while
    /// retaining the immutable tuple definition as schema information.
    pub(crate) fn composites_for_constituent(
        &self,
        constituent: AttrId,
    ) -> impl Iterator<Item = AttrId> + '_ {
        self.constituents
            .get(&constituent)
            .into_iter()
            .flat_map(|composites| composites.iter().copied())
    }

    pub(crate) fn ident_aliases(&self) -> impl Iterator<Item = (&Keyword, AttrId)> {
        self.ident_aliases
            .iter()
            .map(|(ident, attribute)| (ident, *attribute))
    }

    pub(crate) fn install_ident_alias(
        &mut self,
        ident: Keyword,
        attribute: AttrId,
    ) -> Result<(), SemanticError> {
        if !self.attributes.contains_key(&attribute) {
            return Err(SemanticError::incorrect(
                "schema/unknown-attribute",
                format!("alias target attribute {attribute} is not installed"),
            ));
        }
        if self.current_idents.contains_key(&ident) || self.ident_aliases.contains_key(&ident) {
            return Err(SemanticError::conflict(
                "schema/ident-exists",
                format!("ident {} is already in use", ident.qualified_name()),
            ));
        }
        self.ident_aliases.insert(ident, attribute);
        Ok(())
    }

    /// Rebuild the operative attribute cache from ordinary current facts.
    ///
    /// `idents` is itself derived from all historical `:db/ident`
    /// assertions. The recovered peer performs these two passes in the same
    /// order (`ident-setting-datoms` followed by install/alter hooks), so this
    /// routine is the only authoritative projection used by construction,
    /// transaction successors, and recovery.
    pub(crate) fn derive_from_information(
        current: &[Datom],
        idents: &IdentIndex,
    ) -> Result<Self, SemanticError> {
        Self::derive_from_information_with_work(
            current,
            idents,
            &mut SchemaValidationWork::default(),
        )
    }

    pub(crate) fn derive_from_information_with_work(
        current: &[Datom],
        idents: &IdentIndex,
        work: &mut SchemaValidationWork,
    ) -> Result<Self, SemanticError> {
        let mut installed = BTreeSet::new();
        for datom in current.iter().filter(|datom| {
            datom.added
                && datom.entity == DB_PART_DB
                && (u64::from(datom.attribute) == DB_INSTALL_ATTRIBUTE
                    || u64::from(datom.attribute) == DB_ALTER_ATTRIBUTE)
        }) {
            let Value::Ref(entity) = datom.value else {
                return Err(schema_information_error(
                    "schema/invalid-install-marker",
                    "attribute install and alter markers must contain refs",
                ));
            };
            installed.insert(entity);
        }

        // Property readers intentionally retain their simple, ordered checks,
        // but only over the owning schema entity's facts. Scanning all user
        // information for every one of its properties would multiply each
        // recovery prefix by the installed schema size.
        let mut by_entity: BTreeMap<_, Vec<_>> = installed
            .iter()
            .copied()
            .map(|entity| (entity, Vec::new()))
            .collect();
        for datom in current {
            if let Some(facts) = by_entity.get_mut(&datom.entity) {
                facts.push(datom);
            }
        }
        Self::derive_from_entity_information(installed, idents, current, work, |entity| {
            by_entity[&entity].as_slice()
        })
    }

    fn derive_from_entity_information<'a>(
        installed: BTreeSet<u64>,
        idents: &IdentIndex,
        partition_information: &[Datom],
        work: &mut SchemaValidationWork,
        entity_facts: impl Fn(u64) -> &'a [&'a Datom],
    ) -> Result<Self, SemanticError> {
        let mut schema = Self::new();
        for entity in installed {
            let current = entity_facts(entity);
            let id = schema_eid_to_attr_id(entity)?;
            let ident = required_keyword(current, entity, DB_IDENT, ":db/ident")?;
            let value_type_entity = required_ref(current, entity, DB_VALUE_TYPE, ":db/valueType")?;
            let value_type = value_type_for_entity(value_type_entity).ok_or_else(|| {
                schema_information_error(
                    "schema/unknown-value-type",
                    format!("attribute {entity} names unknown value type {value_type_entity}"),
                )
            })?;
            let cardinality_entity =
                required_ref(current, entity, DB_CARDINALITY, ":db/cardinality")?;
            let cardinality = cardinality_for_entity(cardinality_entity).ok_or_else(|| {
                schema_information_error(
                    "schema/unknown-cardinality",
                    format!("attribute {entity} names unknown cardinality {cardinality_entity}"),
                )
            })?;
            let unique = optional_ref(current, entity, DB_UNIQUE, ":db/unique")?
                .map(|unique| {
                    unique_for_entity(unique).ok_or_else(|| {
                        schema_information_error(
                            "schema/unknown-unique",
                            format!("attribute {entity} names unknown uniqueness {unique}"),
                        )
                    })
                })
                .transpose()?;
            let explicitly_indexed =
                optional_bool(current, entity, DB_INDEX, ":db/index")?.unwrap_or(false);
            let component = optional_bool(current, entity, DB_IS_COMPONENT, ":db/isComponent")?
                .unwrap_or(false);
            let no_history =
                optional_bool(current, entity, DB_NO_HISTORY, ":db/noHistory")?.unwrap_or(false);
            let fulltext = optional_bool(current, entity, crate::DB_FULLTEXT, ":db/fulltext")?
                .unwrap_or(false);

            let homogeneous = optional_keyword(current, entity, DB_TUPLE_TYPE, ":db/tupleType")?;
            let heterogeneous = optional_tuple(current, entity, DB_TUPLE_TYPES, ":db/tupleTypes")?;
            let composite = optional_tuple(current, entity, DB_TUPLE_ATTRS, ":db/tupleAttrs")?;
            if usize::from(homogeneous.is_some())
                + usize::from(heterogeneous.is_some())
                + usize::from(composite.is_some())
                > 1
            {
                return Err(schema_information_error(
                    "schema/multiple-tuple-specs",
                    format!("attribute {entity} has more than one tuple definition"),
                ));
            }
            let tuple = if let Some(value_type) = homogeneous {
                Some(TupleSpec::Homogeneous(value_type_from_keyword(
                    idents,
                    &value_type,
                )?))
            } else if let Some(values) = heterogeneous {
                Some(TupleSpec::Heterogeneous(
                    tuple_keywords(values, ":db/tupleTypes")?
                        .into_iter()
                        .map(|ident| value_type_from_keyword(idents, &ident))
                        .collect::<Result<Vec<_>, _>>()?,
                ))
            } else if let Some(values) = composite {
                Some(TupleSpec::Composite(
                    tuple_keywords(values, ":db/tupleAttrs")?
                        .into_iter()
                        .map(|ident| {
                            let entity = idents.resolve(&ident).ok_or_else(|| {
                                schema_information_error(
                                    "schema/unknown-tuple-attribute",
                                    format!(
                                        "tuple constituent {} does not resolve",
                                        ident.qualified_name()
                                    ),
                                )
                            })?;
                            schema_eid_to_attr_id(entity)
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                ))
            } else {
                None
            };
            let tuple_discontinued = optional_bool(
                current,
                entity,
                DB_TUPLE_DISCONTINUED,
                ":db.tuple/discontinued",
            )?
            .unwrap_or(false);
            let mut predicates = values(current, entity, DB_ATTR_PREDS)
                .into_iter()
                .map(|value| match value {
                    Value::Symbol(symbol) if qualified_symbol(symbol) => Ok(symbol_name(symbol)),
                    Value::Symbol(_) => Err(schema_information_error(
                        "schema/unqualified-attribute-predicate",
                        ":db.attr/preds values must be fully qualified symbols",
                    )),
                    _ => Err(schema_information_error(
                        "schema/invalid-attribute-predicate",
                        ":db.attr/preds values must be symbols",
                    )),
                })
                .collect::<Result<Vec<_>, _>>()?;
            predicates.sort();
            predicates.dedup();

            schema.install_with_work(
                Attribute {
                    id,
                    ident,
                    value_type,
                    cardinality,
                    unique,
                    // `indexed` records the explicit :db/index fact. Unique
                    // attributes are effective AVET members independently.
                    indexed: explicitly_indexed,
                    component,
                    no_history,
                    fulltext,
                    tuple,
                    tuple_discontinued,
                    predicates,
                },
                work,
            )?;
        }

        // Attribute aliases are the subset of the general historical ident
        // index whose winning target is an installed attribute and whose name
        // is not that entity's current ident.
        for (ident, entity) in idents.aliases() {
            let Ok(attribute) = schema_eid_to_attr_id(entity) else {
                continue;
            };
            let Some(current) = schema.attributes.get(&attribute) else {
                continue;
            };
            if &current.ident != ident {
                schema.install_ident_alias(ident.clone(), attribute)?;
            }
        }
        for datom in partition_information
            .iter()
            .filter(|datom| u64::from(datom.attribute) == crate::DB_INSTALL_PARTITION)
        {
            let Value::Ref(entity) = datom.value else {
                return Err(schema_information_error(
                    "schema/invalid-partition-install",
                    "partition installation requires an entity ref",
                ));
            };
            if !datom.added
                || datom.entity != DB_PART_DB
                || crate::eid_to_part(entity)? != crate::DB_PARTITION
                || entity >= u64::from(crate::model::identity::IMPLICIT_PARTITION_BASE)
                || schema.attributes.contains_key(&(entity as u32))
            {
                return Err(schema_information_error(
                    "schema/invalid-partition-install",
                    "named partitions require distinct system-partition entities below the implicit partition range",
                ));
            }
            let ident = idents.ident(entity).ok_or_else(|| {
                schema_information_error(
                    "schema/partition-ident-required",
                    "named partition needs an ident",
                )
            })?;
            schema.partitions.insert(entity as u32, ident.clone());
        }
        if [crate::DB_PART_DB, crate::DB_PART_TX, crate::DB_PART_USER]
            .iter()
            .any(|part| !schema.partitions.contains_key(&(*part as u32)))
        {
            return Err(schema_information_error(
                "schema/invalid-partition-install",
                "the three builtin partition markers must remain installed",
            ));
        }
        schema.validate_tuple_definitions_with_work(work)?;
        Ok(schema)
    }

    pub fn validate_value(
        &self,
        attribute: &Attribute,
        value: &Value,
    ) -> Result<(), SemanticError> {
        if !matches_value_type(attribute.value_type, value) {
            return Err(SemanticError::incorrect(
                "transaction/value-type",
                format!(
                    "attribute {} requires {:?}, got {}",
                    attribute.ident.qualified_name(),
                    attribute.value_type,
                    value.type_name()
                ),
            ));
        }

        match value {
            Value::Uri(uri) if !crate::model::uri::validate(uri) => Err(SemanticError::incorrect(
                "value/invalid-uri",
                "URI value has invalid syntax or escaping",
            )),
            Value::BigInt(value) if bigint_bit_length(value) > 8192 => {
                Err(SemanticError::incorrect(
                    "transaction/bigint-too-large",
                    "BigInteger values are limited to 8192 bits",
                ))
            }
            Value::BigDec(value) if value.digits() > 1024 => Err(SemanticError::incorrect(
                "transaction/bigdec-too-large",
                "BigDecimal values are limited to 1024 digits",
            )),
            Value::Tuple(slots) => self.validate_tuple(attribute, slots),
            _ => Ok(()),
        }
    }

    /// Validate tuple definitions that depend on the complete successor
    /// schema rather than on one attribute in isolation.
    ///
    /// This is intentionally separate from `validate_attribute`: a schema
    /// transaction may install a composite before one of its constituents in
    /// canonical operation order. The recovered transactor validates
    /// `:db/tupleAttrs` against its fully formed db-after value. Callers that
    /// assemble or transition a schema must therefore invoke this after all
    /// proposed attributes have been installed or altered.
    pub fn validate_tuple_definitions(&self) -> Result<(), SemanticError> {
        self.validate_tuple_definitions_with_work(&mut SchemaValidationWork::default())
    }

    pub(crate) fn validate_tuple_definitions_with_work(
        &self,
        work: &mut SchemaValidationWork,
    ) -> Result<(), SemanticError> {
        for attribute in self.attributes.values() {
            self.validate_attribute_with_work(attribute, work)?;

            let Some(TupleSpec::Composite(constituents)) = &attribute.tuple else {
                continue;
            };

            // Discontinuation removes the reverse constituent links.
            // Former constituents may subsequently change
            // cardinality, while the immutable tuple definition remains as
            // historical schema information.
            if attribute.tuple_discontinued {
                continue;
            }

            for constituent_id in constituents {
                work.tuple_constituents = work.tuple_constituents.saturating_add(1);
                let Some(constituent) = self.attributes.get(constituent_id) else {
                    return Err(SemanticError::incorrect(
                        "schema/invalid-tuple-attributes",
                        format!(
                            "composite {} references unknown constituent attribute {}",
                            attribute.ident.qualified_name(),
                            constituent_id
                        ),
                    ));
                };

                if constituent.cardinality != Cardinality::One
                    || !is_tuple_scalar_type(constituent.value_type)
                {
                    return Err(SemanticError::incorrect(
                        "schema/invalid-tuple-attributes",
                        format!(
                            "composite {} constituent {} must be a cardinality-one tuple scalar",
                            attribute.ident.qualified_name(),
                            constituent.ident.qualified_name()
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Validate tuple installation rules against a complete proposed schema.
    ///
    /// The installed IDs distinguish an attempted install-time
    /// `:db.tuple/discontinued` assertion from decoding a legitimate schema in
    /// which an older composite has already been discontinued. This method is
    /// the single validation hook for the transaction boundary after all
    /// schema operations have been applied to its successor candidate.
    pub fn validate_tuple_installations(
        &self,
        installed_attributes: &[AttrId],
    ) -> Result<(), SemanticError> {
        self.validate_tuple_installations_with_work(
            installed_attributes,
            &mut SchemaValidationWork::default(),
        )
    }

    pub(crate) fn validate_tuple_installations_with_work(
        &self,
        installed_attributes: &[AttrId],
        work: &mut SchemaValidationWork,
    ) -> Result<(), SemanticError> {
        for attribute_id in installed_attributes {
            work.installations = work.installations.saturating_add(1);
            let attribute = self.attribute(*attribute_id)?;
            if attribute.tuple_discontinued {
                return Err(SemanticError::incorrect(
                    "schema/cannot-discontinue-at-install",
                    "a composite tuple cannot be discontinued when it is installed",
                ));
            }
        }
        self.validate_tuple_definitions_with_work(work)
    }

    pub(crate) fn validate_attribute(&self, attribute: &Attribute) -> Result<(), SemanticError> {
        self.validate_attribute_with_work(attribute, &mut SchemaValidationWork::default())
    }

    fn validate_attribute_with_work(
        &self,
        attribute: &Attribute,
        work: &mut SchemaValidationWork,
    ) -> Result<(), SemanticError> {
        work.attributes = work.attributes.saturating_add(1);
        if attribute.fulltext && attribute.value_type != ValueType::String {
            return Err(SemanticError::incorrect(
                "schema/fulltext-must-be-string",
                "fulltext indexing requires string value type",
            ));
        }
        if attribute.predicates.iter().any(|predicate| {
            work.predicates = work.predicates.saturating_add(1);
            let Some((namespace, name)) = predicate.split_once('/') else {
                return true;
            };
            namespace.is_empty() || name.is_empty() || name.contains('/')
        }) {
            return Err(SemanticError::incorrect(
                "schema/unqualified-attribute-predicate",
                "attribute predicates must be fully qualified symbols",
            ));
        }
        if attribute.unique.is_some() && attribute.cardinality != Cardinality::One {
            return Err(SemanticError::incorrect(
                "schema/unique-must-be-cardinality-one",
                "unique attributes must have cardinality one",
            ));
        }
        if attribute.unique.is_some() && attribute.value_type == ValueType::Bytes {
            return Err(SemanticError::incorrect(
                "schema/bytes-cannot-be-unique",
                "bytes attributes cannot be unique",
            ));
        }
        if attribute.component && attribute.value_type != ValueType::Ref {
            return Err(SemanticError::incorrect(
                "schema/component-must-be-ref",
                "component attributes must have ref value type",
            ));
        }
        if attribute.tuple_discontinued && !matches!(attribute.tuple, Some(TupleSpec::Composite(_)))
        {
            return Err(SemanticError::incorrect(
                "schema/not-a-composite-tuple",
                "tuple discontinuation applies only to composite tuple attributes",
            ));
        }
        if matches!(
            (&attribute.value_type, &attribute.tuple),
            (ValueType::Tuple, Some(TupleSpec::Homogeneous(_)))
        ) {
            work.tuple_types = work.tuple_types.saturating_add(1);
        }
        match (&attribute.value_type, &attribute.tuple) {
            (ValueType::Tuple, Some(TupleSpec::Homogeneous(value_type)))
                if !is_tuple_scalar_type(*value_type) =>
            {
                Err(SemanticError::incorrect(
                    "schema/invalid-tuple-element-type",
                    "homogeneous tuples require a permitted scalar value type",
                ))
            }
            (ValueType::Tuple, Some(TupleSpec::Heterogeneous(types)))
                if !(2..=8).contains(&types.len()) =>
            {
                Err(SemanticError::incorrect(
                    "schema/invalid-tuple-length",
                    "heterogeneous tuples require 2 through 8 slots",
                ))
            }
            (ValueType::Tuple, Some(TupleSpec::Heterogeneous(types)))
                if types.iter().any(|value_type| {
                    work.tuple_types = work.tuple_types.saturating_add(1);
                    !is_tuple_scalar_type(*value_type)
                }) =>
            {
                Err(SemanticError::incorrect(
                    "schema/invalid-tuple-element-type",
                    "heterogeneous tuple slots require permitted scalar value types",
                ))
            }
            (ValueType::Tuple, Some(TupleSpec::Composite(attributes)))
                if !(2..=8).contains(&attributes.len()) =>
            {
                Err(SemanticError::incorrect(
                    "schema/invalid-tuple-length",
                    "composite tuples require 2 through 8 attributes",
                ))
            }
            (ValueType::Tuple, Some(TupleSpec::Composite(_)))
                if attribute.cardinality != Cardinality::One =>
            {
                Err(SemanticError::incorrect(
                    "schema/composite-must-be-cardinality-one",
                    "composite tuple attributes must have cardinality one",
                ))
            }
            (ValueType::Tuple, Some(_)) => Ok(()),
            (ValueType::Tuple, None) => Err(SemanticError::incorrect(
                "schema/missing-tuple-spec",
                "tuple attributes require one tuple specification",
            )),
            (_, Some(_)) => Err(SemanticError::incorrect(
                "schema/tuple-spec-on-scalar",
                "tuple specifications are valid only for tuple attributes",
            )),
            (_, None) => Ok(()),
        }
    }

    fn validate_tuple(
        &self,
        attribute: &Attribute,
        slots: &[Option<Value>],
    ) -> Result<(), SemanticError> {
        let tuple = attribute.tuple.as_ref().ok_or_else(|| {
            SemanticError::incorrect("schema/missing-tuple-spec", "tuple specification missing")
        })?;
        match tuple {
            TupleSpec::Homogeneous(value_type) => {
                if !(2..=8).contains(&slots.len()) {
                    return Err(SemanticError::incorrect(
                        "transaction/invalid-tuple-length",
                        "homogeneous tuples require 2 through 8 slots",
                    ));
                }
                for slot in slots.iter().flatten() {
                    validate_tuple_slot(*value_type, slot)?;
                }
            }
            TupleSpec::Heterogeneous(types) => {
                if slots.len() != types.len() {
                    return Err(SemanticError::incorrect(
                        "transaction/invalid-tuple-length",
                        format!("tuple requires {} slots", types.len()),
                    ));
                }
                for (value_type, slot) in types
                    .iter()
                    .zip(slots)
                    .filter_map(|(value_type, slot)| slot.as_ref().map(|slot| (value_type, slot)))
                {
                    validate_tuple_slot(*value_type, slot)?;
                }
            }
            TupleSpec::Composite(attributes) => {
                if slots.len() != attributes.len() {
                    return Err(SemanticError::incorrect(
                        "transaction/invalid-tuple-length",
                        format!("composite tuple requires {} slots", attributes.len()),
                    ));
                }
                for (attribute_id, slot) in attributes.iter().zip(slots) {
                    if let Some(slot) = slot {
                        let constituent = self.attribute(*attribute_id)?;
                        validate_tuple_slot(constituent.value_type, slot)?;
                    }
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn attribute_information_datoms(
    attribute: &Attribute,
    names: &Schema,
    tx: u64,
    source: u64,
) -> Result<Vec<Datom>, SemanticError> {
    schema_eid_to_attr_id(u64::from(attribute.id))?;
    let entity = u64::from(attribute.id);
    let mut datoms = vec![
        information_assertion(
            entity,
            DB_IDENT,
            Value::Keyword(attribute.ident.clone()),
            tx,
            source,
        )?,
        information_assertion(
            entity,
            DB_VALUE_TYPE,
            Value::Ref(value_type_entity(attribute.value_type)),
            tx,
            source,
        )?,
        information_assertion(
            entity,
            DB_CARDINALITY,
            Value::Ref(cardinality_entity(attribute.cardinality)),
            tx,
            source,
        )?,
    ];
    if let Some(unique) = attribute.unique {
        datoms.push(information_assertion(
            entity,
            DB_UNIQUE,
            Value::Ref(unique_entity(unique)),
            tx,
            source,
        )?);
    }
    if attribute.indexed {
        datoms.push(information_assertion(
            entity,
            DB_INDEX,
            Value::Bool(true),
            tx,
            source,
        )?);
    }
    if attribute.component {
        datoms.push(information_assertion(
            entity,
            DB_IS_COMPONENT,
            Value::Bool(true),
            tx,
            source,
        )?);
    }
    if attribute.no_history {
        datoms.push(information_assertion(
            entity,
            DB_NO_HISTORY,
            Value::Bool(true),
            tx,
            source,
        )?);
    }
    if attribute.fulltext {
        datoms.push(information_assertion(
            entity,
            crate::DB_FULLTEXT,
            Value::Bool(true),
            tx,
            source,
        )?);
    }
    match &attribute.tuple {
        Some(TupleSpec::Homogeneous(value_type)) => datoms.push(information_assertion(
            entity,
            DB_TUPLE_TYPE,
            Value::Keyword(value_type_ident(*value_type)),
            tx,
            source,
        )?),
        Some(TupleSpec::Heterogeneous(types)) => datoms.push(information_assertion(
            entity,
            DB_TUPLE_TYPES,
            Value::Tuple(
                types
                    .iter()
                    .map(|value_type| Some(Value::Keyword(value_type_ident(*value_type))))
                    .collect(),
            ),
            tx,
            source,
        )?),
        Some(TupleSpec::Composite(attributes)) => {
            let idents = attributes
                .iter()
                .map(|id| {
                    names
                        .attribute(*id)
                        .map(|attribute| Some(Value::Keyword(attribute.ident.clone())))
                })
                .collect::<Result<Vec<_>, _>>()?;
            datoms.push(information_assertion(
                entity,
                DB_TUPLE_ATTRS,
                Value::Tuple(idents),
                tx,
                source,
            )?);
        }
        None => {}
    }
    if attribute.tuple_discontinued {
        datoms.push(information_assertion(
            entity,
            DB_TUPLE_DISCONTINUED,
            Value::Bool(true),
            tx,
            source,
        )?);
    }
    for predicate in &attribute.predicates {
        datoms.push(information_assertion(
            entity,
            DB_ATTR_PREDS,
            Value::Symbol(parse_symbol(predicate)),
            tx,
            source,
        )?);
    }
    datoms.sort_by(|left, right| left.cmp_in(right, IndexOrder::Eavt));
    Ok(datoms)
}

fn information_assertion(
    entity: u64,
    attribute: u64,
    value: Value,
    tx: u64,
    source: u64,
) -> Result<Datom, SemanticError> {
    Ok(Datom {
        entity,
        attribute: schema_eid_to_attr_id(attribute)?,
        value,
        tx,
        source,
        added: true,
    })
}

fn schema_information_error(code: &'static str, message: impl Into<String>) -> SemanticError {
    SemanticError::new(ErrorCategory::Incorrect, code, message)
}

fn values<'a>(current: &[&'a Datom], entity: u64, attribute: u64) -> Vec<&'a Value> {
    let mut values = Vec::<&Value>::new();
    for datom in current.iter().filter(|datom| {
        datom.added && datom.entity == entity && u64::from(datom.attribute) == attribute
    }) {
        if !values
            .iter()
            .any(|value| value.index_cmp(&datom.value).is_eq())
        {
            values.push(&datom.value);
        }
    }
    values
}

fn exactly_zero_or_one<'a>(
    current: &[&'a Datom],
    entity: u64,
    attribute: u64,
    label: &str,
) -> Result<Option<&'a Value>, SemanticError> {
    let found = values(current, entity, attribute);
    match found.as_slice() {
        [] => Ok(None),
        [value] => Ok(Some(*value)),
        _ => Err(schema_information_error(
            "schema/conflicting-functional-property",
            format!("entity {entity} has multiple current {label} values"),
        )),
    }
}

fn required_keyword(
    current: &[&Datom],
    entity: u64,
    attribute: u64,
    label: &str,
) -> Result<Keyword, SemanticError> {
    match exactly_zero_or_one(current, entity, attribute, label)? {
        Some(Value::Keyword(value)) => Ok(value.clone()),
        Some(_) => Err(schema_information_error(
            "schema/invalid-property-type",
            format!("entity {entity} {label} must be a keyword"),
        )),
        None => Err(schema_information_error(
            "schema/missing-property",
            format!("entity {entity} must specify {label}"),
        )),
    }
}

fn required_ref(
    current: &[&Datom],
    entity: u64,
    attribute: u64,
    label: &str,
) -> Result<u64, SemanticError> {
    match exactly_zero_or_one(current, entity, attribute, label)? {
        Some(Value::Ref(value)) => Ok(*value),
        Some(_) => Err(schema_information_error(
            "schema/invalid-property-type",
            format!("entity {entity} {label} must be a ref"),
        )),
        None => Err(schema_information_error(
            "schema/missing-property",
            format!("entity {entity} must specify {label}"),
        )),
    }
}

fn optional_ref(
    current: &[&Datom],
    entity: u64,
    attribute: u64,
    label: &str,
) -> Result<Option<u64>, SemanticError> {
    match exactly_zero_or_one(current, entity, attribute, label)? {
        Some(Value::Ref(value)) => Ok(Some(*value)),
        Some(_) => Err(schema_information_error(
            "schema/invalid-property-type",
            format!("entity {entity} {label} must be a ref"),
        )),
        None => Ok(None),
    }
}

fn optional_bool(
    current: &[&Datom],
    entity: u64,
    attribute: u64,
    label: &str,
) -> Result<Option<bool>, SemanticError> {
    match exactly_zero_or_one(current, entity, attribute, label)? {
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(schema_information_error(
            "schema/invalid-property-type",
            format!("entity {entity} {label} must be a boolean"),
        )),
        None => Ok(None),
    }
}

fn optional_keyword(
    current: &[&Datom],
    entity: u64,
    attribute: u64,
    label: &str,
) -> Result<Option<Keyword>, SemanticError> {
    match exactly_zero_or_one(current, entity, attribute, label)? {
        Some(Value::Keyword(value)) => Ok(Some(value.clone())),
        Some(_) => Err(schema_information_error(
            "schema/invalid-property-type",
            format!("entity {entity} {label} must be a keyword"),
        )),
        None => Ok(None),
    }
}

fn optional_tuple<'a>(
    current: &[&'a Datom],
    entity: u64,
    attribute: u64,
    label: &str,
) -> Result<Option<&'a [Option<Value>]>, SemanticError> {
    match exactly_zero_or_one(current, entity, attribute, label)? {
        Some(Value::Tuple(values)) => Ok(Some(values)),
        Some(_) => Err(schema_information_error(
            "schema/invalid-property-type",
            format!("entity {entity} {label} must be a tuple"),
        )),
        None => Ok(None),
    }
}

fn tuple_keywords(values: &[Option<Value>], label: &str) -> Result<Vec<Keyword>, SemanticError> {
    values
        .iter()
        .map(|value| match value {
            Some(Value::Keyword(value)) => Ok(value.clone()),
            _ => Err(schema_information_error(
                "schema/invalid-tuple-property",
                format!("{label} must contain only non-nil keywords"),
            )),
        })
        .collect()
}

fn value_type_from_keyword(
    idents: &IdentIndex,
    ident: &Keyword,
) -> Result<ValueType, SemanticError> {
    idents
        .resolve(ident)
        .and_then(value_type_for_entity)
        .ok_or_else(|| {
            schema_information_error(
                "schema/unknown-tuple-value-type",
                format!("unknown tuple value type {}", ident.qualified_name()),
            )
        })
}

fn symbol_name(symbol: &Symbol) -> String {
    match &symbol.namespace {
        Some(namespace) => format!("{namespace}/{}", symbol.name),
        None => symbol.name.clone(),
    }
}

fn qualified_symbol(symbol: &Symbol) -> bool {
    symbol
        .namespace
        .as_deref()
        .is_some_and(|namespace| !namespace.is_empty())
        && !symbol.name.is_empty()
        && !symbol.name.contains('/')
}

fn parse_symbol(value: &str) -> Symbol {
    match value.split_once('/') {
        Some((namespace, name)) => Symbol::new(namespace, name),
        None => Symbol::unqualified(value),
    }
}

fn matches_value_type(value_type: ValueType, value: &Value) -> bool {
    matches!(
        (value_type, value),
        (ValueType::BigDec, Value::BigDec(_))
            | (ValueType::BigInt, Value::BigInt(_))
            | (ValueType::Boolean, Value::Bool(_))
            | (ValueType::Bytes, Value::Bytes(_))
            | (ValueType::Double, Value::Double(_))
            | (ValueType::Float, Value::Float(_))
            | (ValueType::Function, Value::Function(_))
            | (ValueType::Instant, Value::Instant(_))
            | (ValueType::Keyword, Value::Keyword(_))
            | (ValueType::Long, Value::Long(_))
            | (ValueType::Ref, Value::Ref(_))
            | (ValueType::String, Value::String(_))
            | (ValueType::Symbol, Value::Symbol(_))
            | (ValueType::Tuple, Value::Tuple(_))
            | (ValueType::Uuid, Value::Uuid(_))
            | (ValueType::Uri, Value::Uri(_))
    )
}

/// Supported scalar types for tuple slots.
/// Float, bytes, function, and tuple are ordinary EDB value types but are
/// not legal tuple slot types.
fn is_tuple_scalar_type(value_type: ValueType) -> bool {
    matches!(
        value_type,
        ValueType::BigDec
            | ValueType::BigInt
            | ValueType::Boolean
            | ValueType::Double
            | ValueType::Instant
            | ValueType::Keyword
            | ValueType::Long
            | ValueType::Ref
            | ValueType::String
            | ValueType::Symbol
            | ValueType::Uuid
            | ValueType::Uri
    )
}

fn validate_tuple_slot(value_type: ValueType, value: &Value) -> Result<(), SemanticError> {
    if !is_tuple_scalar_type(value_type) {
        return Err(SemanticError::incorrect(
            "schema/invalid-tuple-element-type",
            "float, bytes, function, and tuple cannot be tuple elements",
        ));
    }
    if !matches_value_type(value_type, value) {
        return Err(SemanticError::incorrect(
            "transaction/invalid-tuple-element",
            format!(
                "tuple slot requires {value_type:?}, got {}",
                value.type_name()
            ),
        ));
    }
    match value {
        Value::Uri(uri) if !crate::model::uri::validate(uri) => Err(SemanticError::incorrect(
            "value/invalid-uri",
            "URI value has invalid syntax or escaping",
        )),
        Value::String(value) if value.encode_utf16().take(257).count() > 256 => {
            Err(SemanticError::incorrect(
                "transaction/tuple-string-too-large",
                "tuple strings are limited to 256 UTF-16 code units",
            ))
        }
        Value::BigInt(value) if bigint_bit_length(value) > 256 => Err(SemanticError::incorrect(
            "transaction/tuple-bigint-too-large",
            "tuple BigIntegers are limited to 256 bits",
        )),
        Value::BigDec(value) if value.digits() > 256 => Err(SemanticError::incorrect(
            "transaction/tuple-bigdec-too-large",
            "tuple BigDecimals are limited to 256 digits",
        )),
        _ => Ok(()),
    }
}

// The tuple scalar limit uses signed integer bit length. It excludes the sign bit: negative powers
// of two need one fewer bit than their absolute magnitude. num_bigint::bits
// alone counts magnitude. Detect the power without allocating a complement.
fn bigint_bit_length(value: &num_bigint::BigInt) -> u64 {
    let bits = value.bits();
    bits - u64::from(
        value.sign() == num_bigint::Sign::Minus && value.trailing_zeros() == Some(bits - 1),
    )
}
