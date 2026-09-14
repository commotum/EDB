//! Authenticated page encoding, routing proofs and bounded field decoding.
use super::*;

#[derive(Clone, Debug)]
pub(crate) struct Child {
    pub(crate) first: Vec<u8>,
    pub(crate) last: Vec<u8>,
    pub(crate) hash: Digest,
    pub(crate) count: u64,
}
#[derive(Clone, Debug)]
pub(crate) enum Page {
    Leaf(Vec<FulltextRecord>),
    Branch(Vec<Child>),
}
impl Page {
    pub(crate) fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + match self {
                Self::Leaf(records) => {
                    records.capacity() * std::mem::size_of::<FulltextRecord>()
                        + records
                            .iter()
                            .map(|r| r.key.capacity() + r.value.capacity())
                            .sum::<usize>()
                }
                Self::Branch(children) => {
                    children.capacity() * std::mem::size_of::<Child>()
                        + children
                            .iter()
                            .map(|c| c.first.capacity() + c.last.capacity())
                            .sum::<usize>()
                }
            }
    }
    pub(crate) fn encode(&self) -> Result<Vec<u8>, SemanticError> {
        let length = 13usize.saturating_add(match self {
            Self::Leaf(records) => records
                .iter()
                .map(|r| r.key.len().saturating_add(r.value.len()).saturating_add(8))
                .sum::<usize>(),
            Self::Branch(children) => children
                .iter()
                .map(|c| {
                    c.first
                        .len()
                        .saturating_add(c.last.len())
                        .saturating_add(48)
                })
                .sum::<usize>(),
        });
        if length > MAX_PAGE {
            return Err(incorrect(
                "fulltext/page-size",
                "search page exceeds physical bound",
            ));
        }
        let mut b = Vec::with_capacity(length);
        b.extend_from_slice(PAGE_MAGIC);
        b.extend_from_slice(&FORMAT.to_be_bytes());
        match self {
            Self::Leaf(records) => {
                b.push(0);
                b.extend_from_slice(&(records.len() as u32).to_be_bytes());
                for r in records {
                    put_bytes(&mut b, &r.key)?;
                    put_bytes(&mut b, &r.value)?;
                }
            }
            Self::Branch(children) => {
                b.push(1);
                b.extend_from_slice(&(children.len() as u32).to_be_bytes());
                for c in children {
                    put_bytes(&mut b, &c.first)?;
                    put_bytes(&mut b, &c.last)?;
                    b.extend_from_slice(&c.hash);
                    b.extend_from_slice(&c.count.to_be_bytes());
                }
            }
        }
        if b.len() > MAX_PAGE {
            return Err(incorrect(
                "fulltext/page-size",
                "search page exceeds physical bound",
            ));
        }
        Ok(b)
    }
    pub(crate) fn decode(hash: Digest, bytes: &[u8]) -> Result<Self, SemanticError> {
        if bytes.len() > MAX_PAGE || sha256(bytes) != hash {
            return Err(fault(
                "fulltext/page-hash",
                "search page hash/length mismatch",
            ));
        }
        let mut d = Decoder::new(bytes);
        if d.take(4)? != PAGE_MAGIC || d.u32()? != FORMAT {
            return Err(fault("fulltext/page-format", "unsupported search page"));
        }
        let tag = d.take(1)?[0];
        let count = d.u32()? as usize;
        if count > FANOUT {
            return Err(fault(
                "fulltext/page-count",
                "search page count exceeds bound",
            ));
        }
        let page = match tag {
            0 => {
                let mut records = Vec::with_capacity(count);
                for _ in 0..count {
                    let key = d.bytes(MAX_KEY)?;
                    let value = d.bytes(MAX_RECORD)?;
                    if key.len().saturating_add(value.len()) > MAX_RECORD {
                        return Err(fault(
                            "fulltext/page-size",
                            "search record exceeds physical bound",
                        ));
                    }
                    if key.is_empty()
                        || records
                            .last()
                            .is_some_and(|r: &FulltextRecord| r.key >= key)
                    {
                        return Err(fault(
                            "fulltext/page-order",
                            "search leaf keys are not strictly ordered",
                        ));
                    }
                    records.push(FulltextRecord { key, value });
                }
                Self::Leaf(records)
            }
            1 if count > 0 => {
                let mut children = Vec::with_capacity(count);
                let mut total = 0u64;
                for _ in 0..count {
                    let first = d.bytes(MAX_KEY)?;
                    let last = d.bytes(MAX_KEY)?;
                    let hash = d.digest()?;
                    let count = d.u64()?;
                    total = total.checked_add(count).ok_or_else(|| {
                        fault("fulltext/page-count", "search child counts overflow")
                    })?;
                    if first.is_empty()
                        || first > last
                        || count == 0
                        || children.last().is_some_and(|c: &Child| c.last >= first)
                    {
                        return Err(fault(
                            "fulltext/page-order",
                            "search child ranges overlap or are invalid",
                        ));
                    }
                    children.push(Child {
                        first,
                        last,
                        hash,
                        count,
                    });
                }
                Self::Branch(children)
            }
            _ => return Err(fault("fulltext/page-kind", "invalid search page kind")),
        };
        d.finish()?;
        Ok(page)
    }
    pub(crate) fn descriptor(&self, hash: Digest) -> Child {
        match self {
            Self::Leaf(r) => Child {
                first: r.first().map_or_else(Vec::new, |r| r.key.clone()),
                last: r.last().map_or_else(Vec::new, |r| r.key.clone()),
                hash,
                count: r.len() as u64,
            },
            Self::Branch(c) => Child {
                first: c.first().unwrap().first.clone(),
                last: c.last().unwrap().last.clone(),
                hash,
                count: c.iter().map(|c| c.count).sum(),
            },
        }
    }
    pub(super) fn count(&self) -> u64 {
        match self {
            Self::Leaf(records) => records.len() as u64,
            Self::Branch(children) => children.iter().map(|child| child.count).sum(),
        }
    }
    pub(super) fn validate_child(&self, expected: &Child) -> Result<(), SemanticError> {
        let (first, last) = match self {
            Self::Leaf(records) => (
                records
                    .first()
                    .map(|r| r.key.as_slice())
                    .unwrap_or_default(),
                records.last().map(|r| r.key.as_slice()).unwrap_or_default(),
            ),
            Self::Branch(children) => (
                children.first().unwrap().first.as_slice(),
                children.last().unwrap().last.as_slice(),
            ),
        };
        if first != expected.first || last != expected.last || self.count() != expected.count {
            return Err(fault(
                "fulltext/child-binding",
                "search child differs from authenticated range/count",
            ));
        }
        Ok(())
    }
}

pub(super) fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), SemanticError> {
    let n = u32::try_from(bytes.len())
        .map_err(|_| incorrect("fulltext/encoding-size", "field exceeds u32"))?;
    out.extend_from_slice(&n.to_be_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}
pub(super) struct Decoder<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl<'a> Decoder<'a> {
    pub(super) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }
    pub(super) fn take(&mut self, n: usize) -> Result<&'a [u8], SemanticError> {
        let end = self
            .offset
            .checked_add(n)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| fault("fulltext/truncated", "truncated search encoding"))?;
        let value = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(value)
    }
    pub(super) fn u32(&mut self) -> Result<u32, SemanticError> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub(super) fn u64(&mut self) -> Result<u64, SemanticError> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub(super) fn digest(&mut self) -> Result<Digest, SemanticError> {
        Ok(self.take(32)?.try_into().unwrap())
    }
    pub(super) fn bytes(&mut self, max: usize) -> Result<Vec<u8>, SemanticError> {
        let n = self.u32()? as usize;
        if n > max {
            return Err(fault("fulltext/field-size", "search field exceeds bound"));
        }
        Ok(self.take(n)?.to_vec())
    }
    pub(super) fn finish(self) -> Result<(), SemanticError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(fault(
                "fulltext/trailing-data",
                "search encoding has trailing data",
            ))
        }
    }
}
