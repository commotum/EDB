//! Versioned bounded advisory grammar, deliberately outside submission hashes.
use crate::{
    HintLimits, IndexPrefix, ReadHint, SemanticError, SnapshotReference, TransactionHints, Value,
};
pub(super) const MAX_WIRE_BYTES: usize = 4 * 1024 * 1024 + 256 * 1024 + 64 * 1024;
const MAGIC: &[u8; 8] = b"EDBHNT\0\x02";
pub(super) fn encode(hints: &TransactionHints) -> Result<Vec<u8>, SemanticError> {
    let mut bytes = MAGIC.to_vec();
    put(&mut bytes, &hints.origin().encode()?)?;
    bytes.extend_from_slice(&(hints.reads().len() as u32).to_be_bytes());
    for hint in hints.reads() {
        bytes.push(u8::from(hint.history));
        match &hint.prefix {
            IndexPrefix::Eavt {
                entity,
                attribute,
                value,
            } => {
                bytes.push(0);
                bytes.extend_from_slice(&entity.to_be_bytes());
                optional_u32(&mut bytes, *attribute);
                optional_value(&mut bytes, value.as_ref())?;
            }
            IndexPrefix::Aevt {
                attribute,
                entity,
                value,
            } => {
                bytes.push(1);
                bytes.extend_from_slice(&attribute.to_be_bytes());
                optional_u64(&mut bytes, *entity);
                optional_value(&mut bytes, value.as_ref())?;
            }
            IndexPrefix::Avet {
                attribute,
                value,
                entity,
            } => {
                bytes.push(2);
                bytes.extend_from_slice(&attribute.to_be_bytes());
                optional_value(&mut bytes, value.as_ref())?;
                optional_u64(&mut bytes, *entity);
            }
            IndexPrefix::Vaet {
                value,
                attribute,
                entity,
            } => {
                bytes.push(3);
                put(&mut bytes, &crate::encoding::encode_canonical_value(value)?)?;
                optional_u32(&mut bytes, *attribute);
                optional_u64(&mut bytes, *entity);
            }
            IndexPrefix::Saevt {
                source,
                attribute,
                entity,
                value,
            } => {
                bytes.push(4);
                bytes.extend_from_slice(&source.to_be_bytes());
                optional_u32(&mut bytes, *attribute);
                optional_u64(&mut bytes, *entity);
                optional_value(&mut bytes, value.as_ref())?;
            }
            IndexPrefix::Savet {
                source,
                attribute,
                value,
                entity,
            } => {
                bytes.push(5);
                bytes.extend_from_slice(&source.to_be_bytes());
                optional_u32(&mut bytes, *attribute);
                optional_value(&mut bytes, value.as_ref())?;
                optional_u64(&mut bytes, *entity);
            }
        }
        if bytes.len() > MAX_WIRE_BYTES {
            return Err(invalid());
        }
    }
    let hash = crate::sha256(&bytes);
    bytes.extend_from_slice(&hash);
    if bytes.len() > MAX_WIRE_BYTES {
        return Err(invalid());
    }
    Ok(bytes)
}
pub(super) fn decode(bytes: &[u8]) -> Result<TransactionHints, SemanticError> {
    if bytes.len() > MAX_WIRE_BYTES || bytes.len() < 44 {
        return Err(invalid());
    }
    let (body, hash) = bytes.split_at(bytes.len() - 32);
    if crate::sha256(body) != hash {
        return Err(invalid());
    }
    let mut input = Input(body);
    if input.take(8)? != MAGIC {
        return Err(invalid());
    }
    let origin = SnapshotReference::decode(input.bytes(256 * 1024)?)?;
    let count = input.u32()? as usize;
    if count > 4096 {
        return Err(invalid());
    }
    let mut reads = Vec::with_capacity(count);
    for _ in 0..count {
        let history = input.boolean()?;
        let prefix = match input.byte()? {
            0 => IndexPrefix::Eavt {
                entity: input.u64()?,
                attribute: input.optional_u32()?,
                value: input.optional_value()?,
            },
            1 => IndexPrefix::Aevt {
                attribute: input.u32()?,
                entity: input.optional_u64()?,
                value: input.optional_value()?,
            },
            2 => IndexPrefix::Avet {
                attribute: input.u32()?,
                value: input.optional_value()?,
                entity: input.optional_u64()?,
            },
            3 => IndexPrefix::Vaet {
                value: input.value()?,
                attribute: input.optional_u32()?,
                entity: input.optional_u64()?,
            },
            4 => IndexPrefix::Saevt {
                source: input.u64()?,
                attribute: input.optional_u32()?,
                entity: input.optional_u64()?,
                value: input.optional_value()?,
            },
            5 => IndexPrefix::Savet {
                source: input.u64()?,
                attribute: input.optional_u32()?,
                value: input.optional_value()?,
                entity: input.optional_u64()?,
            },
            _ => return Err(invalid()),
        };
        prefix.validate()?;
        reads.push(ReadHint { history, prefix });
    }
    if !input.0.is_empty() {
        return Err(invalid());
    }
    TransactionHints::from_reads(
        origin,
        reads,
        HintLimits {
            max_prefixes: 4096,
            max_bytes: 4 * 1024 * 1024,
        },
    )
}
fn optional_u32(bytes: &mut Vec<u8>, n: Option<u32>) {
    bytes.push(u8::from(n.is_some()));
    if let Some(n) = n {
        bytes.extend_from_slice(&n.to_be_bytes());
    }
}
fn optional_u64(bytes: &mut Vec<u8>, n: Option<u64>) {
    bytes.push(u8::from(n.is_some()));
    if let Some(n) = n {
        bytes.extend_from_slice(&n.to_be_bytes());
    }
}
fn optional_value(bytes: &mut Vec<u8>, value: Option<&Value>) -> Result<(), SemanticError> {
    bytes.push(u8::from(value.is_some()));
    if let Some(value) = value {
        put(bytes, &crate::encoding::encode_canonical_value(value)?)?;
    }
    Ok(())
}
fn put(bytes: &mut Vec<u8>, field: &[u8]) -> Result<(), SemanticError> {
    if bytes.len().saturating_add(field.len()).saturating_add(4) > MAX_WIRE_BYTES {
        return Err(invalid());
    }
    bytes.extend_from_slice(&(field.len() as u32).to_be_bytes());
    bytes.extend_from_slice(field);
    Ok(())
}
fn invalid() -> SemanticError {
    SemanticError::incorrect(
        "remote/hints",
        "invalid or unsupported bounded advisory hint encoding",
    )
}
struct Input<'a>(&'a [u8]);
impl<'a> Input<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], SemanticError> {
        if n > self.0.len() {
            return Err(invalid());
        }
        let (head, tail) = self.0.split_at(n);
        self.0 = tail;
        Ok(head)
    }
    fn byte(&mut self) -> Result<u8, SemanticError> {
        Ok(self.take(1)?[0])
    }
    fn boolean(&mut self) -> Result<bool, SemanticError> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(invalid()),
        }
    }
    fn u32(&mut self) -> Result<u32, SemanticError> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, SemanticError> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn bytes(&mut self, max: usize) -> Result<&'a [u8], SemanticError> {
        let n = self.u32()? as usize;
        if n > max {
            return Err(invalid());
        }
        self.take(n)
    }
    fn value(&mut self) -> Result<Value, SemanticError> {
        crate::encoding::decode_canonical_value(self.bytes(4 * 1024 * 1024)?)
    }
    fn optional_u32(&mut self) -> Result<Option<u32>, SemanticError> {
        if self.boolean()? {
            self.u32().map(Some)
        } else {
            Ok(None)
        }
    }
    fn optional_u64(&mut self) -> Result<Option<u64>, SemanticError> {
        if self.boolean()? {
            self.u64().map(Some)
        } else {
            Ok(None)
        }
    }
    fn optional_value(&mut self) -> Result<Option<Value>, SemanticError> {
        if self.boolean()? {
            self.value().map(Some)
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claimed_origin() -> SnapshotReference {
        // A hint origin is untrusted data, never a read capability. A structurally
        // valid genesis claim is sufficient for this codec-only test.
        let root = crate::storage::root::DatabaseValueRoot {
            identity: [1; 16],
            basis: 0,
            log: None,
            indexes: None,
            metadata: Some([2; 32]),
        }
        .encode()
        .unwrap();
        let mut bytes = b"EDBS\0\x03".to_vec();
        for name in [b"hint-test".as_slice(), b"hint-lineage".as_slice()] {
            bytes.extend_from_slice(&(name.len() as u32).to_be_bytes());
            bytes.extend_from_slice(name);
        }
        for n in [0u64, 0, crate::INITIAL_EIDX_FRONTIER] {
            bytes.extend_from_slice(&n.to_be_bytes());
        }
        bytes.extend_from_slice(&[0; 32]);
        bytes.extend_from_slice(&[2; 32]);
        bytes.extend_from_slice(&(root.len() as u16).to_be_bytes());
        bytes.extend_from_slice(&root);
        bytes.extend_from_slice(&[0; 19]); // No as-of/since bounds; current view.
        bytes.extend_from_slice(&crate::sha256(&bytes));
        SnapshotReference::decode(&bytes).unwrap()
    }

    #[test]
    fn hints2_roundtrip_preserves_all_six_prefix_orders_and_rejects_hints1() {
        let value = Value::String("Peaches".into());
        let prefixes = [
            IndexPrefix::Eavt {
                entity: 42,
                attribute: Some(1000),
                value: Some(value.clone()),
            },
            IndexPrefix::Aevt {
                attribute: 1000,
                entity: Some(42),
                value: Some(value.clone()),
            },
            IndexPrefix::Avet {
                attribute: 1000,
                value: Some(value.clone()),
                entity: Some(42),
            },
            IndexPrefix::Vaet {
                value: Value::Ref(42),
                attribute: Some(1000),
                entity: Some(41),
            },
            IndexPrefix::Saevt {
                source: 99,
                attribute: Some(1000),
                entity: Some(42),
                value: Some(value.clone()),
            },
            IndexPrefix::Savet {
                source: 98,
                attribute: Some(1000),
                value: Some(value),
                entity: Some(42),
            },
        ];
        let hints = TransactionHints::from_reads(
            claimed_origin(),
            prefixes.into_iter().flat_map(|prefix| {
                [
                    ReadHint {
                        history: false,
                        prefix: prefix.clone(),
                    },
                    ReadHint {
                        history: true,
                        prefix,
                    },
                ]
            }),
            HintLimits::default(),
        )
        .unwrap();
        let encoded = encode(&hints).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded.origin(), hints.origin());
        assert_eq!(decoded.reads(), hints.reads());
        assert_eq!(decoded.reads().len(), 12);
        assert_eq!(decoded.retained_bytes(), hints.retained_bytes());

        let mut retired = encoded[..encoded.len() - 32].to_vec();
        retired[7] = 1;
        retired.extend_from_slice(&crate::sha256(&retired));
        assert_eq!(decode(&retired).unwrap_err().code, "remote/hints");
    }
}
