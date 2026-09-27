//! Immutable schema/ident projections and endpoint AVET readiness, re-derived from datoms.
use super::tree::navigation::{
    prefix_start_child, validate_loaded_child_datom, validate_loaded_child_key,
};
use super::tree::{RootNode, TreeNode};
use crate::model::idents::IdentIndex;
use crate::{
    Datom, Digest, DurableTransaction, ErrorCategory, IndexOrder, IndexPrefix, SemanticError,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

pub(crate) fn changed_avet_attributes(
    base: &crate::Schema,
    endpoint: &crate::Schema,
) -> Vec<(u32, bool, bool)> {
    let mut attributes = base
        .attributes()
        .chain(endpoint.attributes())
        .map(|attribute| attribute.id)
        .collect::<Vec<_>>();
    attributes.sort_unstable();
    attributes.dedup();
    attributes
        .into_iter()
        .filter_map(|attribute| {
            let before = effective_avet(base, attribute);
            let after = effective_avet(endpoint, attribute);
            (before != after).then_some((attribute, before, after))
        })
        .collect()
}

pub(crate) fn effective_avet(schema: &crate::Schema, attribute: u32) -> bool {
    schema
        .attribute(attribute)
        .is_ok_and(|attribute| attribute.indexed || attribute.unique.is_some())
}

pub(crate) fn schema_index_member(
    schema: &crate::Schema,
    datom: &Datom,
    order: IndexOrder,
) -> Result<bool, SemanticError> {
    match order {
        IndexOrder::Eavt | IndexOrder::Aevt => Ok(true),
        IndexOrder::Avet => {
            let attribute = schema.attribute(datom.attribute)?;
            Ok(attribute.indexed || attribute.unique.is_some())
        }
        IndexOrder::Vaet => {
            Ok(schema.attribute(datom.attribute)?.value_type == crate::ValueType::Ref)
        }
    }
}

pub(crate) fn sort_dedup_datoms(datoms: &mut Vec<Datom>, order: IndexOrder) {
    datoms.sort_by(|left, right| left.cmp_in(right, order));
    datoms.dedup_by(|right, left| same_stored_datom(left, right));
}

pub(super) fn same_eav(left: &Datom, right: &Datom) -> bool {
    left.entity == right.entity
        && left.attribute == right.attribute
        && left.value.stored_eq(&right.value)
}

fn same_stored_datom(left: &Datom, right: &Datom) -> bool {
    left.entity == right.entity
        && left.attribute == right.attribute
        && left.tx == right.tx
        && left.added == right.added
        && left.value.stored_eq(&right.value)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ResidentMetadataStats {
    pub(crate) schema_attributes: usize,
    pub(crate) schema_information_datoms: usize,
    pub(crate) schema_estimated_bytes: u64,
    pub(crate) ident_names: usize,
    pub(crate) ident_entities: usize,
    pub(crate) ident_estimated_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ResidentTreeRootStats {
    pub(crate) children: usize,
    pub(crate) estimated_bytes: u64,
}

/// Small discardable projection needed to classify recent AVET/VAET datoms.
/// Both inputs remain ordinary datoms authenticated by the native manifest;
/// `Schema` and `IdentIndex` are always re-derived and are never authorities.
#[derive(Clone)]
pub(crate) struct MetadataProjection {
    pub(crate) schema_current: Arc<[Datom]>,
    pub(crate) idents: Arc<IdentIndex>,
    pub(crate) schema: Arc<crate::Schema>,
    // The projection is immutable. Compute its diagnostic sizes only when
    // deriving new metadata, never on an ordinary writer's statistics read.
    residency: ResidentMetadataStats,
}

impl MetadataProjection {
    fn from_current_and_idents(
        mut schema_current: Vec<Datom>,
        idents: IdentIndex,
    ) -> Result<Self, SemanticError> {
        sort_dedup_datoms(&mut schema_current, IndexOrder::Eavt);
        retain_schema_working_set(&mut schema_current);
        let schema = crate::Schema::derive_from_information(&schema_current, &idents)?;
        let residency = Self::compute_residency(&schema_current, &schema, &idents);
        Ok(Self {
            schema_current: schema_current.into(),
            idents: Arc::new(idents),
            schema: Arc::new(schema),
            residency,
        })
    }

    pub(crate) fn apply(&self, transactions: &[DurableTransaction]) -> Result<Self, SemanticError> {
        if !transactions.iter().any(|transaction| {
            transaction
                .tx_data
                .iter()
                .any(|datom| schema_information_attribute(datom.attribute))
        }) {
            return Ok(self.clone());
        }

        if !transactions
            .iter()
            .flat_map(|transaction| &transaction.tx_data)
            .any(|datom| {
                self.schema.datom_may_change_schema(
                    datom.entity,
                    datom.attribute,
                    &datom.value,
                    datom.added,
                )
            })
        {
            // General entity idents change the identity projection, not the
            // installed schema. Keep its validated descriptors/information and
            // size totals shared. Attribute/partition renames and repurposed
            // schema aliases deliberately take the full derivation below.
            let mut updates = transactions
                .iter()
                .flat_map(|transaction| &transaction.tx_data)
                .filter(|datom| datom.added && datom.attribute == crate::DB_IDENT as u32)
                .collect::<Vec<_>>();
            if updates.is_empty() {
                return Ok(self.clone());
            }
            updates.sort_by(|left, right| ident_assertion_cmp(left, right));
            let mut idents = (*self.idents).clone();
            for datom in updates {
                idents.apply_assertion(datom, crate::DB_IDENT as u32)?;
            }
            let mut residency = self.residency;
            residency.ident_names = idents.name_count();
            residency.ident_entities = idents.entity_count();
            residency.ident_estimated_bytes = idents.estimated_retained_bytes();
            return Ok(Self {
                schema_current: Arc::clone(&self.schema_current),
                schema: Arc::clone(&self.schema),
                idents: Arc::new(idents),
                residency,
            });
        }

        // Schema/ident edits are rare. Only those edits copy the small
        // authenticated working set and derive a replacement projection;
        // ordinary transaction successors retain all three resident Arcs.
        let mut current = self.schema_current.to_vec();
        let mut idents = (*self.idents).clone();
        for transaction in transactions {
            // Older exact genesis profiles already reserve :db/fulltext's
            // immutable t=0 ident, but the small schema working set excludes
            // its entity until the explicit descriptor upgrade. Reattach that
            // authenticated ident when its install marker arrives; never emit
            // a new logical assertion or reinterpret the old root.
            if self.schema.attribute(crate::DB_FULLTEXT as u32).is_err()
                && transaction.tx_data.iter().any(|datom| {
                    datom.added
                        && datom.entity == crate::DB_PART_DB
                        && datom.attribute == crate::DB_INSTALL_ATTRIBUTE as u32
                        && datom.value == crate::Value::Ref(crate::DB_FULLTEXT)
                })
                && !current.iter().any(|datom| {
                    datom.entity == crate::DB_FULLTEXT && datom.attribute == crate::DB_IDENT as u32
                })
                && let Some(ident) = idents.ident(crate::DB_FULLTEXT)
            {
                current.push(Datom {
                    entity: crate::DB_FULLTEXT,
                    attribute: crate::DB_IDENT as u32,
                    value: crate::Value::Keyword(ident.clone()),
                    tx: crate::t_to_tx(0)?,
                    added: true,
                });
            }
            let mut ident_updates = transaction
                .tx_data
                .iter()
                .filter(|datom| datom.added && datom.attribute == crate::DB_IDENT as u32)
                .collect::<Vec<_>>();
            ident_updates.sort_by(|left, right| ident_assertion_cmp(left, right));
            for datom in ident_updates {
                idents.apply_assertion(datom, crate::DB_IDENT as u32)?;
            }
            for datom in &transaction.tx_data {
                if !schema_information_attribute(datom.attribute) {
                    continue;
                }
                if datom.added {
                    if !current.iter().any(|fact| same_eav(fact, datom)) {
                        current.push(datom.clone());
                    }
                } else {
                    current.retain(|fact| !same_eav(fact, datom));
                }
            }
        }
        Self::from_current_and_idents(current, idents)
    }

    pub(crate) fn resident_stats(&self) -> ResidentMetadataStats {
        self.residency
    }

    fn compute_residency(
        schema_current: &[Datom],
        schema: &crate::Schema,
        idents: &IdentIndex,
    ) -> ResidentMetadataStats {
        let schema_information_bytes = schema_current.iter().fold(0_u64, |bytes, datom| {
            bytes.saturating_add(datom.retained_bytes())
        });
        ResidentMetadataStats {
            schema_attributes: schema.attributes().count(),
            schema_information_datoms: schema_current.len(),
            schema_estimated_bytes: schema
                .estimated_retained_bytes()
                .saturating_add(schema_information_bytes),
            ident_names: idents.name_count(),
            ident_entities: idents.entity_count(),
            ident_estimated_bytes: idents.estimated_retained_bytes(),
        }
    }
}

pub(crate) fn apply_metadata_and_avet_readiness<F>(
    base: &MetadataProjection,
    initial_unready: &Arc<BTreeSet<u32>>,
    transactions: &[DurableTransaction],
    mut has_base_history: F,
) -> Result<(MetadataProjection, Arc<BTreeSet<u32>>), SemanticError>
where
    F: FnMut(u32) -> Result<bool, SemanticError>,
{
    let mut metadata = base.clone();
    // Ordinary successors share even a wide pending-backfill set. Copy only
    // for an actual membership change, retaining old snapshots' readiness.
    let mut unready = Arc::clone(initial_unready);
    let mut base_history = BTreeMap::<u32, bool>::new();
    let mut prior_tail_history = BTreeSet::<u32>::new();

    for transaction in transactions {
        let endpoint = metadata.apply(std::slice::from_ref(transaction))?;
        // MetadataProjection::apply retains all three Arcs for ordinary data
        // transactions. Pointer identity is therefore a proof that no schema
        // transition exists; avoid walking every resident attribute merely to
        // rediscover that fact on every commit.
        let changed_avet = if Arc::ptr_eq(&metadata.schema, &endpoint.schema) {
            Vec::new()
        } else {
            changed_avet_attributes(&metadata.schema, &endpoint.schema)
        };
        for (attribute, before, after) in changed_avet {
            let needs_backfill = if !after {
                // Dropping AVET also drops any pending backfill. This matters
                // to an unqualified AVET scan, which must not be poisoned by
                // a no-longer-indexed attribute.
                false
            } else if !before {
                if prior_tail_history.contains(&attribute) {
                    true
                } else if let Some(had_history) = base_history.get(&attribute) {
                    *had_history
                } else {
                    let had_history = has_base_history(attribute)?;
                    base_history.insert(attribute, had_history);
                    had_history
                }
            } else {
                continue;
            };
            if unready.contains(&attribute) != needs_backfill {
                let unready = Arc::make_mut(&mut unready);
                if needs_backfill {
                    unready.insert(attribute);
                } else {
                    // Empty attributes can toggle physical AVET membership
                    // synchronously, matching recovered add-avet/add-unique.
                    unready.remove(&attribute);
                }
            }
        }
        prior_tail_history.extend(transaction.tx_data.iter().map(|datom| datom.attribute));
        metadata = endpoint;
    }

    Ok((metadata, unready))
}

fn retain_schema_working_set(current: &mut Vec<Datom>) {
    let installed = current
        .iter()
        .filter_map(|datom| {
            if datom.added
                && datom.entity == crate::DB_PART_DB
                && matches!(
                    u64::from(datom.attribute),
                    crate::DB_INSTALL_ATTRIBUTE
                        | crate::DB_ALTER_ATTRIBUTE
                        | crate::DB_INSTALL_PARTITION
                )
            {
                match &datom.value {
                    crate::Value::Ref(entity) => Some(*entity),
                    _ => None,
                }
            } else {
                None
            }
        })
        .collect::<BTreeSet<_>>();
    current.retain(|datom| {
        (datom.entity == crate::DB_PART_DB
            && matches!(
                u64::from(datom.attribute),
                crate::DB_INSTALL_ATTRIBUTE
                    | crate::DB_ALTER_ATTRIBUTE
                    | crate::DB_INSTALL_PARTITION
            ))
            || (installed.contains(&datom.entity) && schema_information_attribute(datom.attribute))
    });
}

fn schema_information_attribute(attribute: u32) -> bool {
    matches!(
        u64::from(attribute),
        crate::DB_IDENT
            | crate::DB_INSTALL_PARTITION
            | crate::DB_INSTALL_ATTRIBUTE
            | crate::DB_ALTER_ATTRIBUTE
            | crate::DB_VALUE_TYPE
            | crate::DB_CARDINALITY
            | crate::DB_UNIQUE
            | crate::DB_IS_COMPONENT
            | crate::DB_INDEX
            | crate::DB_NO_HISTORY
            | crate::DB_FULLTEXT
            | crate::DB_TUPLE_TYPE
            | crate::DB_TUPLE_TYPES
            | crate::DB_TUPLE_ATTRS
            | crate::DB_TUPLE_DISCONTINUED
            | crate::DB_ATTR_PREDS
    )
}

fn ident_assertion_cmp(left: &Datom, right: &Datom) -> std::cmp::Ordering {
    left.tx
        .cmp(&right.tx)
        .then(left.entity.cmp(&right.entity))
        .then_with(|| left.value.stored_cmp(&right.value))
        .then(left.added.cmp(&right.added))
}

fn tree_datoms_with_prefix<F>(
    root: &RootNode,
    prefix: &IndexPrefix,
    load_node: &mut F,
) -> Result<Vec<Datom>, SemanticError>
where
    F: FnMut(Digest) -> Result<Arc<TreeNode>, SemanticError>,
{
    prefix.validate()?;
    let order = prefix.order();
    if root.order != order {
        return Err(fault(
            "tree/metadata-root-order",
            "metadata prefix was routed through the wrong persistent index",
        ));
    }
    if root.directories.is_empty() {
        return Ok(Vec::new());
    }

    let mut output = Vec::new();
    let first_directory = prefix_start_child(&root.directories, prefix);
    for directory_ref in root.directories.iter().skip(first_directory) {
        if directory_ref.key.cmp_prefix(prefix).is_gt() {
            break;
        }
        let loaded_directory = load_node(directory_ref.hash)?;
        let TreeNode::Directory(directory) = loaded_directory.as_ref() else {
            return Err(fault(
                "tree/metadata-directory-kind",
                "metadata root child is not a directory",
            ));
        };
        validate_loaded_child_key(
            directory_ref,
            directory.order,
            directory.history,
            directory.count,
            directory.leaves.first().map(|child| &child.key),
            root.order,
            root.history,
        )?;
        let first_leaf = prefix_start_child(&directory.leaves, prefix);
        for leaf_ref in directory.leaves.iter().skip(first_leaf) {
            if leaf_ref.key.cmp_prefix(prefix).is_gt() {
                break;
            }
            let loaded_leaf = load_node(leaf_ref.hash)?;
            let TreeNode::Leaf(leaf) = loaded_leaf.as_ref() else {
                return Err(fault(
                    "tree/metadata-leaf-kind",
                    "metadata directory child is not a leaf",
                ));
            };
            let first = leaf.datom(0);
            validate_loaded_child_datom(
                leaf_ref,
                leaf.order,
                leaf.history,
                leaf.len() as u64,
                first.as_ref(),
                root.order,
                root.history,
            )?;
            for index in 0..leaf.len() {
                let datom = leaf.datom(index).expect("validated metadata leaf columns");
                match crate::index::compare_prefix(&datom, prefix) {
                    std::cmp::Ordering::Less => {}
                    std::cmp::Ordering::Equal => output.push(datom),
                    std::cmp::Ordering::Greater => return Ok(output),
                }
            }
        }
    }
    Ok(output)
}

pub(crate) fn derive_metadata_from_roots<F>(
    roots: &BTreeMap<(bool, u8), Arc<RootNode>>,
    mut load_node: F,
) -> Result<MetadataProjection, SemanticError>
where
    F: FnMut(Digest) -> Result<Arc<TreeNode>, SemanticError>,
{
    let root = |history: bool, order: IndexOrder| {
        roots
            .get(&(history, order_tag(order)))
            .map(Arc::as_ref)
            .ok_or_else(|| {
                fault(
                    "tree/missing-metadata-root",
                    "persistent publication omits an index required for metadata reconstruction",
                )
            })
    };
    let history_aevt = root(true, IndexOrder::Aevt)?;
    let current_aevt = root(false, IndexOrder::Aevt)?;
    let current_eavt = root(false, IndexOrder::Eavt)?;

    // Recovered `ident-setting-datoms` reads assertions from AEVT and folds
    // them in transaction order. EDB's history tree contains the complete
    // retained stream, and :db/ident is an immutable built-in without
    // :db/noHistory, so aliases and later name repurposing survive bases.
    let ident_prefix = IndexPrefix::Aevt {
        attribute: crate::DB_IDENT as u32,
        entity: None,
        value: None,
    };
    let ident_assertions = tree_datoms_with_prefix(history_aevt, &ident_prefix, &mut load_node)?
        .into_iter()
        .filter(|datom| datom.added)
        .collect::<Vec<_>>();
    let idents = IdentIndex::derive(ident_assertions.iter(), crate::DB_IDENT as u32)?;

    // As recovered `run-hooks` does, discover installed attributes from the
    // two current hook attributes, then read each complete current schema
    // entity through EAVT. General entity idents and unrelated open-entity
    // facts never become a second schema projection.
    let mut schema_current = Vec::new();
    for attribute in [
        crate::DB_INSTALL_ATTRIBUTE,
        crate::DB_ALTER_ATTRIBUTE,
        crate::DB_INSTALL_PARTITION,
    ] {
        let prefix = IndexPrefix::Aevt {
            attribute: attribute as u32,
            entity: None,
            value: None,
        };
        schema_current.extend(tree_datoms_with_prefix(
            current_aevt,
            &prefix,
            &mut load_node,
        )?);
    }
    let mut installed = BTreeSet::new();
    for datom in &schema_current {
        if !datom.added || datom.entity != crate::DB_PART_DB {
            continue;
        }
        let crate::Value::Ref(entity) = datom.value else {
            return Err(fault(
                "tree/invalid-schema-hook",
                "current schema hook does not contain an attribute entity ref",
            ));
        };
        installed.insert(entity);
    }
    for entity in installed {
        let prefix = IndexPrefix::Eavt {
            entity,
            attribute: None,
            value: None,
        };
        schema_current.extend(
            tree_datoms_with_prefix(current_eavt, &prefix, &mut load_node)?
                .into_iter()
                .filter(|datom| schema_information_attribute(datom.attribute)),
        );
    }
    MetadataProjection::from_current_and_idents(schema_current, idents)
}

fn order_tag(order: IndexOrder) -> u8 {
    match order {
        IndexOrder::Eavt => 0,
        IndexOrder::Aevt => 1,
        IndexOrder::Avet => 2,
        IndexOrder::Vaet => 3,
    }
}

fn fault(code: &'static str, message: impl Into<String>) -> SemanticError {
    SemanticError::new(ErrorCategory::Fault, code, message)
}
