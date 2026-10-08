//! Versioned native process-boundary grammar, separate from request hashing
//! and immutable database values. Reuses their scalar/form representations.
use super::*;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ExactEndpoint {
    pub generation: u64,
    pub basis_t: u64,
    pub tx_hash: Digest,
    pub state_hash: Digest,
    pub eidx_frontier: u64,
}
use crate::{DatabaseIdentity, ServiceTransactionReport, TransactionRequest};

const REQUEST: u8 = 11;
const RESPONSE: u8 = 12;
pub(super) const VERSION: u8 = 7;

pub(crate) struct WireReport {
    pub before: ExactEndpoint,
    pub after: ExactEndpoint,
    pub before_manifest: Digest,
    pub after_manifest: Digest,
    pub tx_data: Vec<Datom>,
    pub tempids: BTreeMap<String, u64>,
    pub replayed: bool,
}

pub(crate) enum WireOutcome {
    Committed(Box<WireReport>),
    Rejected(SemanticError),
}

pub(crate) fn encode_submission(
    identity: &DatabaseIdentity,
    request: &TransactionRequest,
) -> Result<Vec<u8>, SemanticError> {
    let mut body = vec![VERSION];
    put_string(&mut body, identity.database_id())?;
    put_string(&mut body, identity.lineage_id())?;
    put_string(&mut body, &request.request_key)?;
    encode_entity_ref(&mut body, &request.source)?;
    body.push(u8::from(request.compare_basis_t.is_some()));
    if let Some(value) = request.compare_basis_t {
        put_u64(&mut body, value);
    }
    body.push(u8::from(request.tx_instant_override.is_some()));
    if let Some(value) = request.tx_instant_override {
        put_i64(&mut body, value);
    }
    put_len(&mut body, request.forms.len())?;
    for form in &request.forms {
        let mut bytes = Vec::new();
        encode_persistent_tx_form(&mut bytes, form)?;
        put_framed_bytes(&mut body, &bytes)?;
    }
    encode_blob(REQUEST, &body)
}

pub(crate) fn decode_submission(
    bytes: &[u8],
) -> Result<(DatabaseIdentity, TransactionRequest), SemanticError> {
    let mut cursor = Cursor::new(decode_blob(bytes, REQUEST)?);
    let version = cursor.u8()?;
    if version != VERSION {
        return Err(fault(
            "transport/version",
            "unsupported native submission version",
        ));
    }
    let identity = DatabaseIdentity::new(cursor.string()?, cursor.string()?);
    let request_key = cursor.string()?;
    let source = decode_entity_ref(&mut cursor)?;
    let compare_basis_t = if cursor.boolean()? {
        Some(cursor.u64()?)
    } else {
        None
    };
    let tx_instant_override = if cursor.boolean()? {
        Some(cursor.i64()?)
    } else {
        None
    };
    let count = cursor.collection_len()?;
    let mut forms = Vec::new();
    for _ in 0..count {
        let mut form = Cursor::new(framed(&mut cursor)?);
        forms.push(decode_form(&mut form)?);
        form.finish()?;
    }
    cursor.finish()?;
    Ok((
        identity,
        TransactionRequest {
            request_key,
            forms,
            source,
            compare_basis_t,
            tx_instant_override,
        },
    ))
}

fn framed<'a>(cursor: &mut Cursor<'a>) -> Result<&'a [u8], SemanticError> {
    let length = cursor.u32()? as usize;
    if length > MAX_BLOB_LEN {
        return Err(fault(
            "transport/frame-limit",
            "compound form exceeds envelope limit",
        ));
    }
    cursor.take(length)
}

fn decode_form(cursor: &mut Cursor<'_>) -> Result<TxForm, SemanticError> {
    Ok(match cursor.u8()? {
        3 => TxForm::Edn(crate::edn_transaction::EdnTransactionForm::decode(
            &cursor.string()?,
        )?),
        0 => TxForm::Op(decode_op(cursor)?),
        1 => TxForm::EntityMap(decode_map(cursor, 0)?),
        2 => {
            let function = match cursor.u8()? {
                0 => CallableRef::Database(decode_entity_ref(cursor)?),
                1 => CallableRef::ExactHash(cursor.digest()?),
                2 => CallableRef::Local(decode_symbol(cursor)?),
                tag => return Err(invalid_tag("callable", tag)),
            };
            let count = cursor.collection_len()?;
            let mut arguments = Vec::new();
            for _ in 0..count {
                arguments.push(decode_runtime(cursor, 0)?);
            }
            TxForm::ProgramCall(ProgramCall {
                function,
                arguments,
            })
        }
        tag => return Err(invalid_tag("transaction form", tag)),
    })
}

fn decode_op(cursor: &mut Cursor<'_>) -> Result<TxOp, SemanticError> {
    Ok(match cursor.u8()? {
        0 => TxOp::Add {
            entity: decode_entity_ref(cursor)?,
            attribute: cursor.u32()?,
            value: decode_tx_value(cursor)?,
        },
        1 => TxOp::Retract {
            entity: decode_entity_ref(cursor)?,
            attribute: cursor.u32()?,
            value: if cursor.boolean()? {
                Some(decode_tx_value(cursor)?)
            } else {
                None
            },
        },
        2 => TxOp::Cas {
            entity: decode_entity_ref(cursor)?,
            attribute: cursor.u32()?,
            old: if cursor.boolean()? {
                Some(decode_tx_value(cursor)?)
            } else {
                None
            },
            new: decode_tx_value(cursor)?,
        },
        3 => TxOp::RetractEntity(decode_entity_ref(cursor)?),
        4 => TxOp::Ensure {
            entity: decode_entity_ref(cursor)?,
            spec: decode_entity_ref(cursor)?,
        },
        5 => TxOp::InstallAttribute(decode_attribute(cursor)?),
        6 => TxOp::AlterAttribute(decode_attribute(cursor)?),
        7 => TxOp::ForcePartition {
            tempid: cursor.string()?,
            partition: decode_entity_ref(cursor)?,
        },
        8 => TxOp::MatchPartition {
            tempid: cursor.string()?,
            entity: decode_entity_ref(cursor)?,
        },
        tag => return Err(invalid_tag("transaction operation", tag)),
    })
}

fn decode_map(cursor: &mut Cursor<'_>, depth: usize) -> Result<EntityMap, SemanticError> {
    depth_limit(depth, 32)?;
    let id = if cursor.boolean()? {
        Some(decode_entity_ref(cursor)?)
    } else {
        None
    };
    let count = cursor.collection_len()?;
    let mut attributes = Vec::new();
    for _ in 0..count {
        let mut entry = Cursor::new(framed(cursor)?);
        let attribute = match entry.u8()? {
            0 => AttributeRef::Id(entry.u32()?),
            1 => AttributeRef::Ident(decode_keyword(&mut entry)?),
            2 => AttributeRef::ReverseId(entry.u32()?),
            3 => AttributeRef::ReverseIdent(decode_keyword(&mut entry)?),
            tag => return Err(invalid_tag("attribute reference", tag)),
        };
        attributes.push((attribute, decode_map_value(&mut entry, depth + 1)?));
        entry.finish()?;
    }
    Ok(EntityMap { id, attributes })
}

fn decode_map_value(cursor: &mut Cursor<'_>, depth: usize) -> Result<MapValue, SemanticError> {
    depth_limit(depth, 32)?;
    Ok(match cursor.u8()? {
        0 => MapValue::Value(decode_tx_value(cursor)?),
        1 => MapValue::Nested(Box::new(decode_map(cursor, depth + 1)?)),
        2 => {
            let count = cursor.collection_len()?;
            let mut values = Vec::new();
            for _ in 0..count {
                let mut value = Cursor::new(framed(cursor)?);
                values.push(decode_map_value(&mut value, depth + 1)?);
                value.finish()?;
            }
            MapValue::Many(values)
        }
        tag => return Err(invalid_tag("map value", tag)),
    })
}

fn decode_attribute(cursor: &mut Cursor<'_>) -> Result<Attribute, SemanticError> {
    let mut attribute = Attribute::new(
        cursor.u32()?,
        decode_keyword(cursor)?,
        decode_value_type(cursor)?,
        match cursor.u8()? {
            0 => Cardinality::One,
            1 => Cardinality::Many,
            tag => return Err(invalid_tag("cardinality", tag)),
        },
    );
    attribute.unique = match cursor.u8()? {
        0 => None,
        1 => Some(Unique::Identity),
        2 => Some(Unique::Value),
        tag => return Err(invalid_tag("uniqueness", tag)),
    };
    attribute.indexed = cursor.boolean()?;
    attribute.component = cursor.boolean()?;
    attribute.no_history = cursor.boolean()?;
    attribute.fulltext = cursor.boolean()?;
    attribute.tuple = match cursor.u8()? {
        0 => None,
        1 => Some(TupleSpec::Homogeneous(decode_value_type(cursor)?)),
        2 => {
            let count = cursor.collection_len()?;
            let mut values = Vec::new();
            for _ in 0..count {
                values.push(decode_value_type(cursor)?);
            }
            Some(TupleSpec::Heterogeneous(values))
        }
        3 => {
            let count = cursor.collection_len()?;
            let mut values = Vec::new();
            for _ in 0..count {
                values.push(cursor.u32()?);
            }
            Some(TupleSpec::Composite(values))
        }
        tag => return Err(invalid_tag("tuple specification", tag)),
    };
    attribute.tuple_discontinued = cursor.boolean()?;
    let count = cursor.collection_len()?;
    for _ in 0..count {
        attribute.predicates.push(cursor.string()?);
    }
    Ok(attribute)
}

fn decode_value_type(cursor: &mut Cursor<'_>) -> Result<ValueType, SemanticError> {
    Ok(match cursor.u8()? {
        0 => ValueType::BigDec,
        1 => ValueType::BigInt,
        2 => ValueType::Boolean,
        3 => ValueType::Bytes,
        4 => ValueType::Double,
        5 => ValueType::Float,
        6 => ValueType::Instant,
        7 => ValueType::Keyword,
        8 => ValueType::Long,
        9 => ValueType::Ref,
        10 => ValueType::String,
        11 => ValueType::Symbol,
        12 => ValueType::Tuple,
        13 => ValueType::Uuid,
        14 => ValueType::Uri,
        15 => ValueType::Function,
        tag => return Err(invalid_tag("value type", tag)),
    })
}

fn depth_limit(depth: usize, maximum: usize) -> Result<(), SemanticError> {
    if depth > maximum {
        return Err(fault(
            "transport/value-depth",
            "native request nesting limit exceeded",
        ));
    }
    Ok(())
}

fn decode_runtime(cursor: &mut Cursor<'_>, depth: usize) -> Result<RuntimeValue, SemanticError> {
    depth_limit(depth, 16)?;
    Ok(match cursor.u8()? {
        0 => RuntimeValue::Scalar(decode_value(cursor, 0)?),
        1 => RuntimeValue::Entity(decode_entity_ref(cursor)?),
        2 => RuntimeValue::Null,
        3 => {
            let count = cursor.collection_len()?;
            let mut values = Vec::new();
            for _ in 0..count {
                values.push(decode_runtime(cursor, depth + 1)?);
            }
            RuntimeValue::Vector(values)
        }
        4 => {
            let count = cursor.collection_len()?;
            let mut values = Vec::new();
            for _ in 0..count {
                values.push((decode_value(cursor, 0)?, decode_runtime(cursor, depth + 1)?));
            }
            RuntimeValue::Map(values)
        }
        tag => return Err(invalid_tag("runtime value", tag)),
    })
}

fn put_endpoint(bytes: &mut Vec<u8>, endpoint: ExactEndpoint) {
    put_u64(bytes, endpoint.generation);
    put_u64(bytes, endpoint.basis_t);
    bytes.extend_from_slice(&endpoint.tx_hash);
    bytes.extend_from_slice(&endpoint.state_hash);
    put_u64(bytes, endpoint.eidx_frontier);
}

fn endpoint(cursor: &mut Cursor<'_>) -> Result<ExactEndpoint, SemanticError> {
    Ok(ExactEndpoint {
        generation: cursor.u64()?,
        basis_t: cursor.u64()?,
        tx_hash: cursor.digest()?,
        state_hash: cursor.digest()?,
        eidx_frontier: cursor.u64()?,
    })
}

pub(crate) fn encode_submission_outcome(
    result: &Result<ServiceTransactionReport, SemanticError>,
) -> Result<Vec<u8>, SemanticError> {
    let mut bytes = vec![VERSION];
    match result {
        Ok(report) => {
            bytes.push(0);
            for database in [&report.db_before, &report.db_after] {
                let (snapshot, ..) = database.committed_block_parts()?;
                let key = database.snapshot_key()?;
                put_endpoint(
                    &mut bytes,
                    ExactEndpoint {
                        generation: key.generation(),
                        basis_t: key.basis_t(),
                        tx_hash: key.transaction_hash(),
                        state_hash: key.state_hash(),
                        eidx_frontier: key.eidx_frontier(),
                    },
                );
                bytes.extend_from_slice(&snapshot.storage_root_id());
            }
            bytes.push(u8::from(report.replayed));
            put_len(&mut bytes, report.tx_data.len())?;
            for datom in &report.tx_data {
                encode_datom(&mut bytes, datom)?;
            }
            put_len(&mut bytes, report.tempids.len())?;
            for (name, entity) in &report.tempids {
                put_string(&mut bytes, name)?;
                put_u64(&mut bytes, *entity);
            }
        }
        Err(error) => {
            bytes.push(1);
            let categories = categories();
            bytes.push(
                categories
                    .iter()
                    .position(|category| *category == error.category)
                    .expect("all categories covered") as u8,
            );
            put_string(&mut bytes, error.code)?;
            put_string(&mut bytes, &error.message)?;
            put_len(&mut bytes, error.details.len())?;
            for (key, value) in &error.details {
                put_string(&mut bytes, key)?;
                put_string(&mut bytes, value)?;
            }
            bytes.push(u8::from(error.anomaly.is_some()));
            if let Some(anomaly) = &error.anomaly {
                encode_runtime_value(&mut bytes, anomaly, 0)?;
            }
        }
    }
    encode_blob(RESPONSE, &bytes)
}

fn categories() -> [ErrorCategory; 10] {
    use ErrorCategory::*;
    [
        Incorrect,
        Forbidden,
        Unsupported,
        NotFound,
        Conflict,
        Busy,
        Unavailable,
        Interrupted,
        Fault,
        UnknownOutcome,
    ]
}

pub(crate) fn decode_submission_outcome(bytes: &[u8]) -> Result<WireOutcome, SemanticError> {
    let mut cursor = Cursor::new(decode_blob(bytes, RESPONSE)?);
    let version = cursor.u8()?;
    if version != VERSION {
        return Err(fault(
            "transport/version",
            "unsupported native outcome version",
        ));
    }
    let result = match cursor.u8()? {
        0 => {
            let before = endpoint(&mut cursor)?;
            let before_manifest = cursor.digest()?;
            let after = endpoint(&mut cursor)?;
            let after_manifest = cursor.digest()?;
            let replayed = cursor.boolean()?;
            let count = cursor.collection_len()?;
            let mut tx_data = Vec::new();
            for _ in 0..count {
                tx_data.push(decode_datom(&mut cursor)?);
            }
            let count = cursor.collection_len()?;
            let mut tempids = BTreeMap::new();
            for _ in 0..count {
                if tempids.insert(cursor.string()?, cursor.u64()?).is_some() {
                    return Err(fault(
                        "transport/duplicate-tempid",
                        "duplicate receipt tempid",
                    ));
                }
            }
            WireOutcome::Committed(Box::new(WireReport {
                before,
                after,
                before_manifest,
                after_manifest,
                tx_data,
                tempids,
                replayed,
            }))
        }
        1 => {
            let tag = cursor.u8()?;
            let category = *categories()
                .get(usize::from(tag))
                .ok_or_else(|| invalid_tag("error category", tag))?;
            let remote_code = cursor.string()?;
            let mut error =
                SemanticError::new(category, "transport/remote-error", cursor.string()?);
            let count = cursor.collection_len()?;
            for _ in 0..count {
                error.details.insert(cursor.string()?, cursor.string()?);
            }
            error.details.insert("remote_code".into(), remote_code);
            if cursor.boolean()? {
                error.anomaly = Some(Box::new(decode_runtime(&mut cursor, 0)?));
            }
            WireOutcome::Rejected(error)
        }
        tag => return Err(invalid_tag("transaction outcome", tag)),
    };
    cursor.finish()?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip_preserves_explicit_source_and_identity() {
        let identity = DatabaseIdentity::new("catalog", "lineage");
        for source in [
            EntityRef::Id(99),
            EntityRef::Edn(r#"[:source/name "julia"]"#.into()),
            EntityRef::Ident(Keyword::new("contributor", "julia")),
            EntityRef::Temp("contributor".into()),
            EntityRef::Lookup {
                attribute: crate::DB_IDENT as u32,
                value: Value::Keyword(Keyword::new("contributor", "julia")),
            },
        ] {
            let request = TransactionRequest::new("key", Vec::new(), source.clone())
                .comparing_basis(12)
                .with_tx_instant(34);
            let (_, decoded) =
                decode_submission(&encode_submission(&identity, &request).unwrap()).unwrap();
            assert_eq!(decoded.source, source);
            assert_eq!(decoded.request_key, "key");
            assert_eq!(decoded.compare_basis_t, Some(12));
            assert_eq!(decoded.tx_instant_override, Some(34));
        }
        let a = crate::submission_request_digest(&[], &EntityRef::Id(99), None, None).unwrap();
        let b = crate::submission_request_digest(&[], &EntityRef::Id(98), None, None).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn old_submission_version_is_rejected_without_inventing_source() {
        let old = encode_blob(REQUEST, &[VERSION - 1]).unwrap();
        assert_eq!(
            decode_submission(&old).unwrap_err().code,
            "transport/version"
        );
    }
}

#[cfg(test)]
#[test]
fn deferred_edn_source_identity_is_canonical_without_database_resolution() {
    let a = crate::submission_request_digest(
        &[],
        &EntityRef::Edn(r#"[:source/name "julia"]"#.into()),
        None,
        None,
    )
    .unwrap();
    let b = crate::submission_request_digest(
        &[],
        &EntityRef::Edn("[  :source/name   \"julia\"  ]".into()),
        None,
        None,
    )
    .unwrap();
    assert_eq!(a, b);
    let changed = crate::submission_request_digest(
        &[],
        &EntityRef::Edn(r#"[:source/name "jake"]"#.into()),
        None,
        None,
    )
    .unwrap();
    assert_ne!(a, changed);
}
