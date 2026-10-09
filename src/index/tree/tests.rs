use super::*;

fn fact() -> Datom {
    Datom {
        entity: 42,
        attribute: 100,
        value: Value::Ref(77),
        tx: crate::t_to_tx(1).unwrap(),
        source: 98,
        added: true,
    }
}

fn routing_node(
    order: IndexOrder,
    history: bool,
    directory: bool,
    keys: [RoutingKey; 2],
) -> TreeNode {
    let children = keys
        .into_iter()
        .map(|key| ChildRef {
            key,
            hash: [1; 32],
            count: 1,
        })
        .collect();
    if directory {
        TreeNode::Directory(DirectoryNode {
            order,
            history,
            count: 2,
            leaves: children,
        })
    } else {
        TreeNode::Root(RootNode {
            order,
            history,
            count: 2,
            directories: children,
        })
    }
}

#[test]
fn authenticated_nodes_reject_source_after_an_omitted_transaction() {
    for order in [
        IndexOrder::Eavt,
        IndexOrder::Aevt,
        IndexOrder::Avet,
        IndexOrder::Vaet,
    ] {
        let prior = fact();
        let mut current = prior.clone();
        match order {
            IndexOrder::Eavt => current.entity += 1,
            IndexOrder::Aevt | IndexOrder::Avet => current.attribute += 1,
            IndexOrder::Vaet => current.value = Value::Ref(78),
            _ => unreachable!(),
        }
        let first = RoutingKey::exact(&prior);
        let sparse = sparse_routing_key(order, &prior, &current);
        assert!(!sparse.has(ROUTING_TX_PRESENT));
        assert!(!sparse.has(ROUTING_SOURCE_PRESENT));
        let mut first_bytes = Vec::new();
        encode_routing_key(&mut first_bytes, &first).unwrap();
        let mut sparse_bytes = Vec::new();
        encode_routing_key(&mut sparse_bytes, &sparse).unwrap();
        // Body: total count, child count, first key/hash/count, second key.
        let second_key = HEADER_LEN + 8 + 4 + first_bytes.len() + 32 + 8;
        let source_at = second_key + sparse_bytes.len() - 10;
        let mask_at = second_key + sparse_bytes.len() - 1;
        for history in [false, true] {
            for directory in [false, true] {
                let node = routing_node(order, history, directory, [first.clone(), sparse.clone()]);
                let canonical = encode_tree_node(&node).unwrap();
                assert_eq!(
                    decode_tree_node(&sha256(&canonical), &canonical).unwrap(),
                    node
                );
                // Keep both authenticators valid so rejection tests the local
                // routing shape, rather than incidental corruption detection.
                for source in [0_u64, 99] {
                    let mut malformed = canonical.clone();
                    malformed[source_at..source_at + 8].copy_from_slice(&source.to_be_bytes());
                    malformed[mask_at] |= ROUTING_SOURCE_PRESENT;
                    let checksum_at = malformed.len() - CHECKSUM_LEN;
                    let checksum = sha256(&malformed[..checksum_at]);
                    malformed[checksum_at..].copy_from_slice(&checksum);
                    let error = decode_tree_node(&sha256(&malformed), &malformed)
                        .expect_err("source cannot follow an omitted transaction");
                    assert_eq!(error.code, "tree/invalid-routing-key", "{order:?}");
                }
            }
        }
    }
}

#[test]
fn source_first_routing_remains_exact_and_decodable() {
    let prior = fact();
    let mut current = prior.clone();
    current.source += 1;
    for order in [IndexOrder::Saevt, IndexOrder::Savet] {
        let key = sparse_routing_key(order, &prior, &current);
        assert!(key.is_exact());
        for history in [false, true] {
            for directory in [false, true] {
                let node = routing_node(
                    order,
                    history,
                    directory,
                    [RoutingKey::exact(&prior), key.clone()],
                );
                let encoded = encode_tree_node(&node).unwrap();
                assert_eq!(decode_tree_node(&sha256(&encoded), &encoded).unwrap(), node);
            }
        }
    }
}
