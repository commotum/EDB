use bigdecimal::BigDecimal;
use num_bigint::BigInt;
use std::cmp::Ordering;
use std::hash::{Hash, Hasher};
use std::mem::size_of;

mod numeric;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Keyword {
    pub namespace: Option<String>,
    pub name: String,
}

impl Keyword {
    pub fn new(namespace: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            namespace: Some(namespace.into()),
            name: name.into(),
        }
    }

    pub fn unqualified(name: impl Into<String>) -> Self {
        Self {
            namespace: None,
            name: name.into(),
        }
    }

    pub fn qualified_name(&self) -> String {
        match &self.namespace {
            Some(namespace) => format!("{namespace}/{}", self.name),
            None => self.name.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Symbol {
    pub namespace: Option<String>,
    pub name: String,
}

impl Symbol {
    pub fn new(namespace: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            namespace: Some(namespace.into()),
            name: name.into(),
        }
    }

    pub fn unqualified(name: impl Into<String>) -> Self {
        Self {
            namespace: None,
            name: name.into(),
        }
    }

    pub fn qualified_name(&self) -> String {
        match &self.namespace {
            Some(namespace) => format!("{namespace}/{}", self.name),
            None => self.name.clone(),
        }
    }
}

/// Logical values accepted by the semantic model.
///
/// `Instant` is milliseconds since the Unix epoch, matching the documented
/// representation. UUIDs are stored as their 128 bits. URIs retain their exact
/// input spelling while logical equality, ordering and hashing use components.
/// Admission validates URI syntax; unchecked public construction still has a
/// deterministic comparison without panicking.
#[derive(Clone, Debug)]
pub enum Value {
    BigDec(BigDecimal),
    BigInt(BigInt),
    Bool(bool),
    Bytes(Vec<u8>),
    Double(f64),
    Float(f32),
    /// Content hash of an immutable native database-function program.
    ///
    /// This deliberately names native program content rather than retaining
    /// Datomic's JVM function object or serialized Clojure/Java code.
    Function([u8; 32]),
    Instant(i64),
    Keyword(Keyword),
    Long(i64),
    Ref(u64),
    String(String),
    Symbol(Symbol),
    Tuple(Vec<Option<Value>>),
    Uuid(u128),
    Uri(String),
}

impl Value {
    /// Hash under logical index equality (not storage identity). In particular
    /// integral refs, decimals and binary floats share a hash when index_cmp
    /// considers them equal. This is internal: no persisted hash format changes.
    pub(crate) fn logical_hash(&self, state: &mut impl Hasher) {
        if numeric::hash(self, state) {
            return;
        }
        self.non_numeric_rank().hash(state);
        match self {
            Self::Bool(value) => value.hash(state),
            Self::Bytes(value) => value.hash(state),
            Self::Function(value) => value.hash(state),
            Self::Instant(value) => value.hash(state),
            Self::Keyword(value) => value.hash(state),
            Self::Symbol(value) => value.hash(state),
            Self::String(value) => value.hash(state),
            Self::Uri(value) => super::uri::hash(value, state),
            Self::Uuid(value) => value.hash(state),
            Self::Tuple(values) => {
                values.len().hash(state);
                for value in values {
                    value.is_some().hash(state);
                    if let Some(value) = value {
                        value.logical_hash(state);
                    }
                }
            }
            Self::Long(_)
            | Self::Ref(_)
            | Self::BigInt(_)
            | Self::BigDec(_)
            | Self::Float(_)
            | Self::Double(_) => unreachable!("numeric hash handled above"),
        }
    }
    /// Bytes retained outside the inline `Value` enum allocation.
    ///
    /// This is an allocator-independent capacity account used for memory-index
    /// admission, not a claim about process RSS. Container capacities and
    /// recursively owned values are included so a large tuple of `nil`s
    /// cannot evade a byte limit merely because its canonical encoding is
    /// compact.
    pub(crate) fn retained_heap_bytes(&self) -> u64 {
        fn string_bytes(value: &String) -> u64 {
            value.capacity() as u64
        }

        match self {
            Self::BigDec(value) => value.digits(),
            Self::BigInt(value) => value.bits().div_ceil(8).max(1),
            Self::Bytes(value) => value.capacity() as u64,
            Self::Keyword(value) => value
                .namespace
                .as_ref()
                .map_or(0, string_bytes)
                .saturating_add(string_bytes(&value.name)),
            Self::String(value) | Self::Uri(value) => string_bytes(value),
            Self::Symbol(value) => value
                .namespace
                .as_ref()
                .map_or(0, string_bytes)
                .saturating_add(string_bytes(&value.name)),
            Self::Tuple(values) => (values.capacity() as u64)
                .saturating_mul(size_of::<Option<Value>>() as u64)
                .saturating_add(
                    values
                        .iter()
                        .filter_map(Option::as_ref)
                        .fold(0_u64, |total, value| {
                            total.saturating_add(value.retained_heap_bytes())
                        }),
                ),
            Self::Bool(_)
            | Self::Double(_)
            | Self::Float(_)
            | Self::Function(_)
            | Self::Instant(_)
            | Self::Long(_)
            | Self::Ref(_)
            | Self::Uuid(_) => 0,
        }
    }

    /// Datomic-style comparison used by indexes and identity checks.
    ///
    /// It follows recovered `datomic.common/compare`: numeric variants compare
    /// across types, byte arrays are length-first then signed-byte lexical, and
    /// tuples compare lexicographically with nil below every value. We use an
    /// explicit type rank for the supported non-numeric types instead of JVM
    /// class-name ordering.
    pub fn index_cmp(&self, other: &Self) -> Ordering {
        if let Some(ordering) = numeric::compare(self, other) {
            return ordering;
        }

        let rank = self.non_numeric_rank().cmp(&other.non_numeric_rank());
        if rank != Ordering::Equal {
            return rank;
        }

        match (self, other) {
            (Self::Bytes(left), Self::Bytes(right)) => compare_bytes(left, right),
            (Self::Keyword(left), Self::Keyword(right)) => compare_named(
                left.namespace.as_deref(),
                &left.name,
                right.namespace.as_deref(),
                &right.name,
            ),
            (Self::Symbol(left), Self::Symbol(right)) => compare_named(
                left.namespace.as_deref(),
                &left.name,
                right.namespace.as_deref(),
                &right.name,
            ),
            (Self::Function(left), Self::Function(right)) => left.cmp(right),
            (Self::Bool(left), Self::Bool(right)) => left.cmp(right),
            (Self::String(left), Self::String(right)) => compare_utf16(left, right),
            (Self::Uri(left), Self::Uri(right)) => super::uri::compare(left, right),
            (Self::Instant(left), Self::Instant(right)) => left.cmp(right),
            (Self::Uuid(left), Self::Uuid(right)) => compare_uuid(*left, *right),
            (Self::Tuple(left), Self::Tuple(right)) => compare_tuple(left, right),
            _ => unreachable!("equal value ranks must have matching variants"),
        }
    }

    /// Equality used to decide whether a stored fact is redundant. Recovered
    /// `equals-with-strict-scale` adds scale only when both top-level values
    /// are BigDecimal. Tuple/list equality continues through Clojure's logical
    /// comparison, so nested `1.0M` and `1.00M` remain equal.
    pub fn stored_eq(&self, other: &Self) -> bool {
        self.index_cmp(other) == Ordering::Equal
            && match (self, other) {
                (Self::BigDec(left), Self::BigDec(right)) => {
                    left.fractional_digit_count() == right.fractional_digit_count()
                }
                _ => true,
            }
    }

    /// Ordering for recovered stored-fact equality boundaries.
    ///
    /// Datomic's logical comparator intentionally considers numerically equal
    /// values equal across representations. Its stored equality adds the
    /// BigDecimal scale distinction only for top-level BigDecimals; tuples
    /// retain logical recursive comparison.
    pub fn stored_cmp(&self, other: &Self) -> Ordering {
        self.index_cmp(other).then_with(|| match (self, other) {
            (Self::BigDec(left), Self::BigDec(right)) => left
                .fractional_digit_count()
                .cmp(&right.fractional_digit_count()),
            _ => Ordering::Equal,
        })
    }

    pub fn is_nan(&self) -> bool {
        matches!(self, Self::Float(value) if value.is_nan())
            || matches!(self, Self::Double(value) if value.is_nan())
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Self::BigDec(_) => "bigdec",
            Self::BigInt(_) => "bigint",
            Self::Bool(_) => "boolean",
            Self::Bytes(_) => "bytes",
            Self::Double(_) => "double",
            Self::Float(_) => "float",
            Self::Function(_) => "function",
            Self::Instant(_) => "instant",
            Self::Keyword(_) => "keyword",
            Self::Long(_) => "long",
            Self::Ref(_) => "ref",
            Self::String(_) => "string",
            Self::Symbol(_) => "symbol",
            Self::Tuple(_) => "tuple",
            Self::Uuid(_) => "uuid",
            Self::Uri(_) => "uri",
        }
    }

    fn non_numeric_rank(&self) -> u8 {
        match self {
            Self::Bytes(_) => 1,
            Self::Keyword(_) => 2,
            Self::Symbol(_) => 3,
            Self::Bool(_) => 4,
            Self::String(_) => 5,
            Self::Uri(_) => 6,
            Self::Instant(_) => 7,
            Self::Uuid(_) => 8,
            Self::Tuple(_) => 9,
            // Keep all existing native ranks stable as this value type is
            // added; hashes have a deterministic bytewise order at the end.
            Self::Function(_) => 10,
            Self::BigDec(_)
            | Self::BigInt(_)
            | Self::Double(_)
            | Self::Float(_)
            | Self::Long(_)
            | Self::Ref(_) => 0,
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        self.index_cmp(other) == Ordering::Equal
    }
}

impl Eq for Value {}

fn compare_bytes(left: &[u8], right: &[u8]) -> Ordering {
    left.len().cmp(&right.len()).then_with(|| {
        left.iter()
            .map(|byte| *byte as i8)
            .cmp(right.iter().map(|byte| *byte as i8))
    })
}

fn compare_utf16(left: &str, right: &str) -> Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

fn compare_named(
    left_namespace: Option<&str>,
    left_name: &str,
    right_namespace: Option<&str>,
    right_name: &str,
) -> Ordering {
    match (left_namespace, right_namespace) {
        (None, None) => compare_utf16(left_name, right_name),
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(left), Some(right)) => {
            compare_utf16(left, right).then_with(|| compare_utf16(left_name, right_name))
        }
    }
}

fn compare_uuid(left: u128, right: u128) -> Ordering {
    let left = ((left >> 64) as u64 as i64, left as u64 as i64);
    let right = ((right >> 64) as u64 as i64, right as u64 as i64);
    left.cmp(&right)
}

fn compare_tuple(left: &[Option<Value>], right: &[Option<Value>]) -> Ordering {
    for (left, right) in left.iter().zip(right) {
        let ordering = match (left, right) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (Some(left), Some(right)) => left.index_cmp(right),
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    left.len().cmp(&right.len())
}
