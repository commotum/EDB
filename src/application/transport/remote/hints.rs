//! Versioned bounded advisory grammar, deliberately outside submission hashes.
use crate::{
    HintLimits, IndexPrefix, ReadHint, SemanticError, SnapshotReference, TransactionHints, Value,
};
pub(super) const MAX_WIRE_BYTES: usize = 4 * 1024 * 1024 + 256 * 1024 + 64 * 1024;
const MAGIC: &[u8; 8] = b"ATHINT\0\x01";
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
