use super::*;
use crate::index::tree::{
    TreeConfig, TreeNode, build_tree, decode_tree_node, range_tree, seek_tree, validate_tree,
};
use crate::{Datom, IndexOrder, Value, t_to_tx};

const ORDERS: [IndexOrder; 6] = [
    IndexOrder::Eavt,
    IndexOrder::Aevt,
    IndexOrder::Avet,
    IndexOrder::Vaet,
    IndexOrder::Saevt,
    IndexOrder::Savet,
];
fn fact(t: u64, source: u64, added: bool) -> Datom {
    Datom {
        entity: 42,
        attribute: 100,
        value: Value::Ref(77),
        tx: t_to_tx(t).unwrap(),
        source,
        added,
    }
}

#[test]
fn index_cutover_visibility_preserves_contributors_and_global_barrier() {
    let first = fact(1, 98, true);
    let second = fact(2, 99, true);
    assert_eq!(
        current_eav_group([first.clone(), second.clone()]),
        vec![second.clone(), first.clone()]
    );
    assert!(current_eav_group([first.clone(), second.clone(), fact(3, 100, false)]).is_empty());
    let renewed = fact(4, 98, true);
    assert_eq!(
        current_eav_group([first, second, fact(3, 100, false), renewed.clone()]),
        vec![renewed]
    );
    let atomic = fact(5, 101, true);
    assert_eq!(
        current_eav_group([fact(5, 101, false), atomic.clone()]),
        vec![atomic]
    );
}

#[test]
fn index_cutover_five_component_boundaries_and_source_prefixes() {
    let d = fact(3, 99, true);
    let boundaries = [
        IndexBoundary::Eavt(IndexComponents::Five(
            d.entity,
            d.attribute,
            d.value.clone(),
            IndexTransaction::T(3),
            99,
        )),
        IndexBoundary::Aevt(IndexComponents::Five(
            d.attribute,
            d.entity,
            d.value.clone(),
            IndexTransaction::T(3),
            99,
        )),
        IndexBoundary::Avet(IndexComponents::Five(
            d.attribute,
            d.value.clone(),
            d.entity,
            IndexTransaction::T(3),
            99,
        )),
        IndexBoundary::Vaet(IndexComponents::Five(
            d.value.clone(),
            d.attribute,
            d.entity,
            IndexTransaction::T(3),
            99,
        )),
        IndexBoundary::Saevt(IndexComponents::Five(
            99,
            d.attribute,
            d.entity,
            d.value.clone(),
            IndexTransaction::T(3),
        )),
        IndexBoundary::Savet(IndexComponents::Five(
            99,
            d.attribute,
            d.value.clone(),
            d.entity,
            IndexTransaction::T(3),
        )),
    ];
    for b in boundaries {
        assert_eq!(b.len(), 5);
        assert!(b.normalized().unwrap().compare_datom(&d).is_eq());
        assert!(
            b.normalized()
                .unwrap()
                .compare_datom(&fact(4, 99, true))
                .is_lt()
        );
        assert!(
            b.current_group_start()
                .normalized()
                .unwrap()
                .compare_datom(&fact(4, 99, true))
                .is_eq()
        );
    }
    let prefix = IndexPrefix::Saevt {
        source: 99,
        attribute: Some(100),
        entity: None,
        value: None,
    };
    assert!(compare_prefix(&d, &prefix).is_eq());
    assert!(compare_prefix(&fact(3, 98, true), &prefix).is_lt());
    assert!(
        IndexPrefix::Saevt {
            source: 99,
            attribute: None,
            entity: Some(42),
            value: None
        }
        .validate()
        .is_err()
    );
    assert!(
        IndexPrefix::Savet {
            source: 99,
            attribute: Some(100),
            value: None,
            entity: Some(42)
        }
        .validate()
        .is_err()
    );
}

#[test]
fn index_cutover_six_tree_orders_roundtrip_source_and_reject_old_version() {
    let config = TreeConfig {
        max_leaf_datoms: 2,
        max_leaves_per_directory: 2,
        ..TreeConfig::default()
    };
    for order in ORDERS {
        let mut input: Vec<_> = (1..=12)
            .map(|t| {
                let mut d = fact(t, 98 + t % 3, t % 4 != 0);
                d.entity += t % 3;
                d.value = Value::Ref(77 + t % 2);
                d
            })
            .collect();
        input.sort_by(|a, b| a.cmp_in(b, order));
        let built = build_tree(order, true, input.clone(), &config).unwrap();
        validate_tree(&built.descriptor, &built.nodes).unwrap();
        assert_eq!(
            range_tree(&built.descriptor, &built.nodes, None, None)
                .unwrap()
                .datoms,
            input
        );
        for expected in &input {
            assert_eq!(
                seek_tree(&built.descriptor, &built.nodes, expected)
                    .unwrap()
                    .datom
                    .as_ref(),
                Some(expected)
            );
        }
        for (hash, bytes) in built.nodes.iter() {
            let node = decode_tree_node(hash, bytes).unwrap();
            if let TreeNode::Leaf(leaf) = node {
                assert_eq!(leaf.sources.len(), leaf.entities.len());
            }
            let mut old = bytes.to_vec();
            old[4..6].copy_from_slice(&5u16.to_be_bytes());
            assert!(decode_tree_node(&crate::sha256(&old), &old).is_err());
        }
    }
}

#[test]
fn index_cutover_overlay_retains_distinct_sources_then_retracts_all() {
    let mut overlay = super::overlay::OverlayIndexes::default();
    overlay.extend(&[fact(1, 98, true)], Vec::new());
    overlay.extend(&[fact(2, 99, true)], Vec::new());
    for order in ORDERS {
        assert_eq!(
            overlay
                .cursor(false, order, |_| std::cmp::Ordering::Equal, false, None)
                .count(),
            2
        );
    }
    overlay.extend(&[fact(3, 100, false)], Vec::new());
    for order in ORDERS {
        assert_eq!(
            overlay
                .cursor(false, order, |_| std::cmp::Ordering::Equal, false, None)
                .count(),
            0
        );
        assert_eq!(
            overlay
                .cursor(true, order, |_| std::cmp::Ordering::Equal, false, None)
                .count(),
            3
        );
    }
}

#[test]
fn index_cutover_nohistory_shared_barrier_removes_all_withdrawn_sources() {
    let input = vec![fact(1, 98, true), fact(2, 99, true), fact(3, 100, false)];
    let witnesses = super::visibility::no_history_eav_group(input.clone());
    assert_eq!(witnesses.len(), 2);
    assert!(
        witnesses
            .iter()
            .all(|p| p.retraction == fact(3, 100, false))
    );
    for order in ORDERS {
        let mut sorted = input.clone();
        sorted.sort_by(|a, b| a.cmp_in(b, order));
        let built = build_tree(order, true, sorted, &TreeConfig::default()).unwrap();
        let mut pairs = witnesses.clone();
        pairs.sort_by(|a, b| {
            a.retraction
                .cmp_in(&b.retraction, order)
                .then_with(|| a.assertion.cmp_in(&b.assertion, order))
        });
        let edits = super::tree::TreeMergeEdits {
            no_history_pairs: pairs,
            ..Default::default()
        };
        let merged = super::tree::merge_tree(
            &built.descriptor,
            &built.nodes,
            &edits,
            &TreeConfig::default(),
        )
        .unwrap();
        assert_eq!(merged.descriptor.count, 0);
        validate_tree(&merged.descriptor, &merged.new_nodes).unwrap();
    }
}

#[test]
fn index_cutover_eager_eligibility_matches_avet_and_savet() {
    use crate::{Attribute, Cardinality, Keyword, Schema, ValueType};
    let mut schema = Schema::new();
    let mut indexed = Attribute::new(
        100,
        Keyword::new("test", "ref"),
        ValueType::Ref,
        Cardinality::Many,
    );
    indexed.indexed = true;
    schema.install(indexed).unwrap();
    schema
        .install(Attribute::new(
            101,
            Keyword::new("test", "plain"),
            ValueType::Long,
            Cardinality::Many,
        ))
        .unwrap();
    let mut plain = fact(1, 98, true);
    plain.attribute = 101;
    plain.value = Value::Long(1);
    let roots = super::eager::IndexRoots::build(&schema, [fact(1, 99, true), plain]);
    assert_eq!(roots.get(IndexOrder::Eavt).len(), 2);
    assert_eq!(roots.get(IndexOrder::Saevt).len(), 2);
    assert_eq!(roots.get(IndexOrder::Avet).len(), 1);
    assert_eq!(roots.get(IndexOrder::Savet).len(), 1);
    assert_eq!(roots.get(IndexOrder::Vaet).len(), 1);
}

fn recent_facts(events: &[Datom], base: u64) -> super::recent::RecentTier {
    use crate::{Attribute, Cardinality, DurableTransaction, Keyword, Schema, ValueType};
    use std::collections::BTreeMap;
    let mut schema = Schema::new();
    let mut attr = Attribute::new(
        100,
        Keyword::new("test", "ref"),
        ValueType::Ref,
        Cardinality::Many,
    );
    attr.indexed = true;
    attr.no_history = true;
    schema.install(attr).unwrap();
    let mut previous = [0; 32];
    let mut transactions = Vec::new();
    for event in events {
        let transaction = DurableTransaction {
            database_id: "index-cutover".into(),
            basis_t: crate::tx_to_t(event.tx).unwrap(),
            previous_hash: previous,
            eidx_frontier: 1000,
            tempids: BTreeMap::new(),
            tx_data: vec![event.clone()],
        };
        previous = crate::transaction_hash(&crate::encode_transaction(&transaction).unwrap());
        transactions.push(transaction);
    }
    super::recent::RecentTier::new(
        "index-cutover",
        base,
        [0; 32],
        transactions,
        super::recent::EndpointProjection::new(schema),
        super::recent::RecentLimits::default(),
    )
    .unwrap()
}

#[test]
fn index_cutover_recent_source_ranges_apply_global_visibility() {
    let assertions = vec![fact(1, 98, true), fact(2, 99, true)];
    let active = recent_facts(&assertions, 0);
    for order in ORDERS {
        assert_eq!(active.current_datoms(order).len(), 2);
    }
    let mut withdrawn = assertions.clone();
    withdrawn.push(fact(3, 100, false));
    let inactive = recent_facts(&withdrawn, 0);
    for order in ORDERS {
        assert!(inactive.current_datoms(order).is_empty());
        assert_eq!(inactive.no_history_pairs(order, &[]).unwrap().len(), 2);
        assert!(
            inactive
                .consolidate_history_range(order, &[], &super::recent::RecentRange::unbounded())
                .unwrap()
                .is_empty()
        );
    }
    assert!(
        inactive
            .prefix_cursor(
                false,
                &IndexPrefix::Saevt {
                    source: 98,
                    attribute: None,
                    entity: None,
                    value: None
                }
            )
            .unwrap()
            .next()
            .is_none()
    );
    withdrawn.push(fact(4, 99, true));
    let renewed = recent_facts(&withdrawn, 0);
    for order in ORDERS {
        assert_eq!(renewed.current_datoms(order).as_ref(), &[fact(4, 99, true)]);
    }
    let reverse = IndexBoundary::Saevt(IndexComponents::One(99))
        .normalized()
        .unwrap();
    assert_eq!(
        renewed.reverse_boundary_cursor(false, &reverse).next(),
        Some(fact(4, 99, true))
    );
    let only_other_source = recent_facts(&[fact(3, 100, true)], 2);
    for order in ORDERS {
        let mut durable = assertions.clone();
        durable.sort_by(|a, b| a.cmp_in(b, order));
        let merged = only_other_source
            .merge_current_range(order, &durable, &super::recent::RecentRange::unbounded())
            .unwrap();
        assert_eq!(merged.len(), 3);
    }
    let retract = recent_facts(&[fact(3, 100, false)], 2);
    for order in ORDERS {
        let mut durable = assertions.clone();
        durable.sort_by(|a, b| a.cmp_in(b, order));
        assert!(
            retract
                .merge_current_range(order, &durable, &super::recent::RecentRange::unbounded())
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn index_cutover_descriptor_requires_twelve_trees_and_rejects_legacy_kind() {
    use crate::storage::descriptors::IndexDescriptor;
    let mut trees = Vec::new();
    for history in [false, true] {
        for order in ORDERS {
            trees.push(
                build_tree(order, history, [], &TreeConfig::default())
                    .unwrap()
                    .descriptor,
            );
        }
    }
    let descriptor = IndexDescriptor {
        identity: [1; 16],
        basis: 0,
        generation: 0,
        trees,
        pending_avet: Vec::new(),
        avet_work: Vec::new(),
        fulltext: None,
    };
    let encoded = descriptor.encode().unwrap();
    assert_eq!(
        IndexDescriptor::decode(&crate::sha256(&encoded), &encoded).unwrap(),
        descriptor
    );
    let mut partial = descriptor.clone();
    partial.trees.truncate(8);
    assert!(partial.encode().is_err());
    let mut block =
        crate::storage::root::Block::decode(&crate::sha256(&encoded), &encoded).unwrap();
    block.kind = 3;
    let legacy = block.encode().unwrap();
    assert!(IndexDescriptor::decode(&crate::sha256(&legacy), &legacy).is_err());
}

#[test]
fn index_cutover_nohistory_discovery_requires_complete_eav_groups() {
    use std::collections::BTreeSet;
    let config = TreeConfig {
        max_leaf_datoms: 1,
        max_leaves_per_directory: 2,
        ..TreeConfig::default()
    };
    let mut input = vec![fact(1, 98, true), fact(2, 99, true), fact(3, 100, false)];
    input.sort_by(|a, b| a.cmp_in(b, IndexOrder::Eavt));
    let built = build_tree(IndexOrder::Eavt, true, input, &config).unwrap();
    let complete = super::tree::TreeMergeEdits {
        no_history_attributes: BTreeSet::from([100]),
        affected_ranges: vec![super::tree::TreeAffectedRange {
            start: None,
            end: None,
        }],
        ..Default::default()
    };
    assert_eq!(
        super::tree::discover_merge_no_history_pairs(&built.descriptor, &built.nodes, &complete)
            .unwrap()
            .len(),
        2
    );
    let merged =
        super::tree::merge_tree(&built.descriptor, &built.nodes, &complete, &config).unwrap();
    assert_eq!(merged.descriptor.count, 0);
    let partial = super::tree::TreeMergeEdits {
        no_history_attributes: BTreeSet::from([100]),
        affected_ranges: vec![super::tree::TreeAffectedRange {
            start: Some(fact(3, 100, false)),
            end: Some(fact(2, 99, true)),
        }],
        ..Default::default()
    };
    assert!(
        super::tree::discover_merge_no_history_pairs(&built.descriptor, &built.nodes, &partial)
            .unwrap()
            .is_empty()
    );
    let retained =
        super::tree::merge_tree(&built.descriptor, &built.nodes, &partial, &config).unwrap();
    assert_eq!(retained.descriptor.count, 3);
}

#[derive(Clone)]
struct MemoryTreeSource(super::tree::TreeNodeSet);
impl super::cursor::DurableTreeSource for MemoryTreeSource {
    fn load_node(
        &self,
        hash: crate::Digest,
        _: &mut super::tree::TreeReadStats,
    ) -> Result<std::sync::Arc<TreeNode>, crate::SemanticError> {
        Ok(std::sync::Arc::new(
            decode_tree_node(&hash, self.0.get(&hash).expect("built node")).unwrap(),
        ))
    }
}

#[test]
fn index_cutover_source_activity_prefix_and_reverse_follow_transaction_order() {
    use std::sync::Arc;
    let config = TreeConfig {
        max_leaf_datoms: 1,
        max_leaves_per_directory: 2,
        ..TreeConfig::default()
    };
    let mut datoms = Vec::new();
    for source in [98, 99, 100] {
        for t in 1..=4 {
            datoms.push(Datom {
                entity: t_to_tx(t).unwrap(),
                attribute: crate::DB_TX_INSTANT as u32,
                value: Value::Instant(1000 - t as i64),
                tx: t_to_tx(t).unwrap(),
                source,
                added: true,
            });
        }
    }
    datoms.sort_by(|a, b| a.cmp_in(b, IndexOrder::Saevt));
    let built = build_tree(IndexOrder::Saevt, false, datoms, &config).unwrap();
    let TreeNode::Root(root) = decode_tree_node(
        &built.descriptor.root_hash,
        built.nodes.get(&built.descriptor.root_hash).unwrap(),
    )
    .unwrap() else {
        panic!("root")
    };
    let root = Arc::new(root);
    let source = MemoryTreeSource(built.nodes);
    let mut prefix = super::cursor::DurableTreeCursor::new_prefix(
        source.clone(),
        root.clone(),
        false,
        IndexPrefix::Saevt {
            source: 99,
            attribute: Some(crate::DB_TX_INSTANT as u32),
            entity: None,
            value: None,
        },
    );
    let mut forward = Vec::new();
    while let Some(d) = prefix.next_datom().unwrap() {
        assert_eq!(d.source, 99);
        forward.push(crate::tx_to_t(d.entity).unwrap());
    }
    assert_eq!(forward, [1, 2, 3, 4]);
    let mut reverse = super::cursor::DurableTreeCursor::new_reverse(
        source,
        root,
        false,
        IndexBoundary::Saevt(IndexComponents::Two(99, crate::DB_TX_INSTANT as u32))
            .normalized()
            .unwrap(),
    );
    let mut newest = Vec::new();
    while let Some(d) = reverse.next_datom().unwrap() {
        if d.source != 99 {
            break;
        }
        newest.push(crate::tx_to_t(d.entity).unwrap());
    }
    assert_eq!(newest, [4, 3, 2, 1]);
}

#[test]
fn index_cutover_durable_five_component_boundaries_seek_both_directions() {
    use std::sync::Arc;
    let config = TreeConfig {
        max_leaf_datoms: 1,
        max_leaves_per_directory: 2,
        ..TreeConfig::default()
    };
    for order in ORDERS {
        let mut input: Vec<_> = (1..=12)
            .map(|t| {
                let mut d = fact(t, 98 + t % 3, true);
                d.entity += t % 2;
                d
            })
            .collect();
        input.sort_by(|a, b| a.cmp_in(b, order));
        let built = build_tree(order, true, input.clone(), &config).unwrap();
        let TreeNode::Root(root) = decode_tree_node(
            &built.descriptor.root_hash,
            built.nodes.get(&built.descriptor.root_hash).unwrap(),
        )
        .unwrap() else {
            panic!("root")
        };
        let root = Arc::new(root);
        let source = MemoryTreeSource(built.nodes);
        for d in &input {
            let tx = IndexTransaction::Tx(d.tx);
            let boundary = match order {
                IndexOrder::Eavt => IndexBoundary::Eavt(IndexComponents::Five(
                    d.entity,
                    d.attribute,
                    d.value.clone(),
                    tx,
                    d.source,
                )),
                IndexOrder::Aevt => IndexBoundary::Aevt(IndexComponents::Five(
                    d.attribute,
                    d.entity,
                    d.value.clone(),
                    tx,
                    d.source,
                )),
                IndexOrder::Avet => IndexBoundary::Avet(IndexComponents::Five(
                    d.attribute,
                    d.value.clone(),
                    d.entity,
                    tx,
                    d.source,
                )),
                IndexOrder::Vaet => IndexBoundary::Vaet(IndexComponents::Five(
                    d.value.clone(),
                    d.attribute,
                    d.entity,
                    tx,
                    d.source,
                )),
                IndexOrder::Saevt => IndexBoundary::Saevt(IndexComponents::Five(
                    d.source,
                    d.attribute,
                    d.entity,
                    d.value.clone(),
                    tx,
                )),
                IndexOrder::Savet => IndexBoundary::Savet(IndexComponents::Five(
                    d.source,
                    d.attribute,
                    d.value.clone(),
                    d.entity,
                    tx,
                )),
            }
            .normalized()
            .unwrap();
            let mut forward = super::cursor::DurableTreeCursor::new_forward_boundary(
                source.clone(),
                root.clone(),
                true,
                boundary.clone(),
            );
            let mut actual = Vec::new();
            while let Some(datom) = forward.next_datom().unwrap() {
                actual.push(datom);
            }
            let expected: Vec<_> = input
                .iter()
                .filter(|d| !boundary.compare_datom(d).is_lt())
                .cloned()
                .collect();
            assert_eq!(actual, expected, "forward {order:?}");
            let mut reverse = super::cursor::DurableTreeCursor::new_reverse(
                source.clone(),
                root.clone(),
                true,
                boundary.clone(),
            );
            let mut actual = Vec::new();
            while let Some(datom) = reverse.next_datom().unwrap() {
                actual.push(datom);
            }
            let expected: Vec<_> = input
                .iter()
                .rev()
                .filter(|d| !boundary.compare_datom(d).is_gt())
                .cloned()
                .collect();
            assert_eq!(actual, expected, "reverse {order:?}");
        }
    }
}
