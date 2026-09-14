//! Process-local numeric identity. Durable value encodings remain in the codec.
//!
//! All supported finite numbers have an exact terminating decimal expansion.
//! Keep only its significant coefficient digits and a base-ten exponent; never
//! materialize the scale-sized run of zeroes. Decimal scale is i64, so i128 also
//! leaves room for normalization at either scale boundary. Binary floats need at
//! most the fixed IEEE exponent span, not the span of a user-supplied decimal.

use super::Value;
use num_bigint::{BigInt, BigUint, Sign};
use num_traits::ToPrimitive;
use std::cmp::Ordering;
use std::hash::{Hash, Hasher};

fn is_numeric(value: &Value) -> bool {
    matches!(
        value,
        Value::Long(_)
            | Value::Ref(_)
            | Value::BigInt(_)
            | Value::BigDec(_)
            | Value::Float(_)
            | Value::Double(_)
    )
}

pub(super) fn compare(left: &Value, right: &Value) -> Option<Ordering> {
    match (is_numeric(left), is_numeric(right)) {
        (false, false) => return None,
        (true, false) => return Some(Ordering::Less),
        (false, true) => return Some(Ordering::Greater),
        (true, true) => {}
    }
    // The common index hot paths need neither allocation nor conversion.
    let ordering = match (left, right) {
        (Value::Long(left), Value::Long(right)) => left.cmp(right),
        (Value::Ref(left), Value::Ref(right)) => left.cmp(right),
        (Value::Long(left), Value::Ref(right)) => long_ref(*left, *right),
        (Value::Ref(left), Value::Long(right)) => long_ref(*right, *left).reverse(),
        (Value::BigInt(left), Value::BigInt(right)) => left.cmp(right),
        (Value::BigDec(left), Value::BigDec(right))
            if left.fractional_digit_count() == right.fractional_digit_count() =>
        {
            left.as_bigint_and_scale()
                .0
                .cmp(&right.as_bigint_and_scale().0)
        }
        (Value::Float(left), Value::Float(right)) => float_cmp(f64::from(*left), f64::from(*right)),
        (Value::Float(left), Value::Double(right)) => float_cmp(f64::from(*left), *right),
        (Value::Double(left), Value::Float(right)) => float_cmp(*left, f64::from(*right)),
        (Value::Double(left), Value::Double(right)) => float_cmp(*left, *right),
        _ => Numeric::from_value(left)
            .expect("numeric variant")
            .cmp(&Numeric::from_value(right).expect("numeric variant")),
    };
    Some(ordering)
}

fn long_ref(left: i64, right: u64) -> Ordering {
    if left < 0 {
        Ordering::Less
    } else {
        (left as u64).cmp(&right)
    }
}

fn float_cmp(left: f64, right: f64) -> Ordering {
    // Unlike total_cmp, logical equality unifies signed zero and NaN payloads.
    left.partial_cmp(&right)
        .unwrap_or_else(|| left.is_nan().cmp(&right.is_nan()))
}

pub(super) fn hash(value: &Value, state: &mut impl Hasher) -> bool {
    let Some(number) = Numeric::from_value(value) else {
        return false;
    };
    0_u8.hash(state); // Same numeric type rank as index_cmp.
    match number {
        Numeric::NegativeInfinity => 0_u8.hash(state),
        Numeric::Finite(decimal) => {
            1_u8.hash(state);
            decimal.negative.hash(state);
            decimal.exponent.hash(state);
            decimal.digits.as_slice().hash(state);
        }
        Numeric::PositiveInfinity => 2_u8.hash(state),
        Numeric::NaN => 3_u8.hash(state),
    }
    true
}

enum Numeric {
    NegativeInfinity,
    Finite(Decimal),
    PositiveInfinity,
    NaN,
}

impl Numeric {
    fn from_value(value: &Value) -> Option<Self> {
        Some(match value {
            Value::Long(value) => Self::Finite(Decimal::small(value.unsigned_abs(), *value < 0, 0)),
            Value::Ref(value) => Self::Finite(Decimal::small(*value, false, 0)),
            Value::BigInt(value) => Self::Finite(Decimal::integer(value, 0)),
            Value::BigDec(value) => {
                let (coefficient, scale) = value.as_bigint_and_scale();
                Self::Finite(Decimal::integer(&coefficient, -i128::from(scale)))
            }
            // Widening f32 to f64 is exact, including subnormals.
            Value::Float(value) => Self::float(f64::from(*value)),
            Value::Double(value) => Self::float(*value),
            _ => return None,
        })
    }

    fn float(value: f64) -> Self {
        if value.is_nan() {
            return Self::NaN;
        }
        if value == f64::NEG_INFINITY {
            return Self::NegativeInfinity;
        }
        if value == f64::INFINITY {
            return Self::PositiveInfinity;
        }
        let bits = value.to_bits();
        let exponent_bits = ((bits >> 52) & 0x7ff) as i32;
        let fraction = bits & 0x000f_ffff_ffff_ffff;
        let (mantissa, exponent) = if exponent_bits == 0 {
            (fraction, 1 - 1023 - 52)
        } else {
            ((1_u64 << 52) | fraction, exponent_bits - 1023 - 52)
        };
        Self::Finite(Decimal::binary(mantissa, exponent, bits >> 63 != 0))
    }

    fn cmp(&self, other: &Self) -> Ordering {
        use Numeric::*;
        match (self, other) {
            (NaN, NaN) => Ordering::Equal,
            (NaN, _) => Ordering::Greater,
            (_, NaN) => Ordering::Less,
            (NegativeInfinity, NegativeInfinity) | (PositiveInfinity, PositiveInfinity) => {
                Ordering::Equal
            }
            (NegativeInfinity, _) | (_, PositiveInfinity) => Ordering::Less,
            (PositiveInfinity, _) | (_, NegativeInfinity) => Ordering::Greater,
            (Finite(left), Finite(right)) => left.cmp(right),
        }
    }
}

struct Decimal {
    digits: Digits,
    negative: bool,
    exponent: i128,
}

impl Decimal {
    fn small(coefficient: u64, negative: bool, exponent: i128) -> Self {
        Self::new(Digits::small(coefficient), negative, exponent)
    }

    fn integer(coefficient: &BigInt, exponent: i128) -> Self {
        Self::magnitude(
            coefficient.magnitude(),
            coefficient.sign() == Sign::Minus,
            exponent,
        )
    }

    fn magnitude(coefficient: &BigUint, negative: bool, exponent: i128) -> Self {
        let digits = match coefficient.to_u64() {
            Some(value) => Digits::small(value),
            None => Digits::Large(coefficient.to_radix_be(10)),
        };
        Self::new(digits, negative, exponent)
    }

    fn new(mut digits: Digits, negative: bool, mut exponent: i128) -> Self {
        if digits.as_slice() == [0] {
            return Self {
                digits,
                negative: false,
                exponent: 0,
            };
        }
        let significant_len = digits
            .as_slice()
            .iter()
            .rposition(|digit| *digit != 0)
            .expect("nonzero coefficient")
            + 1;
        exponent += (digits.as_slice().len() - significant_len) as i128;
        digits.truncate(significant_len);
        Self {
            digits,
            negative,
            exponent,
        }
    }

    fn binary(mut mantissa: u64, mut exponent: i32, negative: bool) -> Self {
        if mantissa == 0 {
            return Self::small(0, false, 0);
        }
        let trailing = mantissa.trailing_zeros();
        mantissa >>= trailing;
        exponent += trailing as i32;
        if exponent >= 0 {
            if exponent < 64 && mantissa.leading_zeros() >= exponent as u32 {
                return Self::small(mantissa << exponent, negative, 0);
            }
            return Self::magnitude(&(BigUint::from(mantissa) << exponent as usize), negative, 0);
        }
        // m / 2^k == (m * 5^k) * 10^-k, with k <= 1074 for f64.
        let coefficient =
            BigUint::from(mantissa) * BigUint::from(5_u8).pow(exponent.unsigned_abs());
        Self::magnitude(&coefficient, negative, i128::from(exponent))
    }

    fn cmp(&self, other: &Self) -> Ordering {
        let left = self.digits.as_slice();
        let right = other.digits.as_slice();
        match (left == [0], right == [0]) {
            (true, true) => return Ordering::Equal,
            (true, false) => {
                return if other.negative {
                    Ordering::Greater
                } else {
                    Ordering::Less
                };
            }
            (false, true) => {
                return if self.negative {
                    Ordering::Less
                } else {
                    Ordering::Greater
                };
            }
            (false, false) => {}
        }
        if self.negative != other.negative {
            return other.negative.cmp(&self.negative);
        }
        let ordering = (self.exponent + left.len() as i128)
            .cmp(&(other.exponent + right.len() as i128))
            .then_with(|| {
                // Equal magnitude spans: compare only represented digits. The
                // shorter coefficient has virtual trailing zeroes, not an
                // exponent-sized allocation or traversal.
                for index in 0..left.len().max(right.len()) {
                    let ordering = left
                        .get(index)
                        .unwrap_or(&0)
                        .cmp(right.get(index).unwrap_or(&0));
                    if ordering != Ordering::Equal {
                        return ordering;
                    }
                }
                Ordering::Equal
            });
        if self.negative {
            ordering.reverse()
        } else {
            ordering
        }
    }
}

enum Digits {
    Small { data: [u8; 20], start: u8, end: u8 },
    Large(Vec<u8>),
}

impl Digits {
    fn small(mut value: u64) -> Self {
        let mut data = [0_u8; 20];
        let mut start = data.len();
        loop {
            start -= 1;
            data[start] = (value % 10) as u8;
            value /= 10;
            if value == 0 {
                break;
            }
        }
        Self::Small {
            data,
            start: start as u8,
            end: 20,
        }
    }

    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Small { data, start, end } => &data[usize::from(*start)..usize::from(*end)],
            Self::Large(data) => data,
        }
    }

    fn truncate(&mut self, len: usize) {
        match self {
            Self::Small { start, end, .. } => *end = *start + len as u8,
            Self::Large(data) => data.truncate(len),
        }
    }
}
