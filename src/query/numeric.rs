//! Exact query arithmetic. Decimal exponents describe scale, not allocated zeros.
//! Admission precedes coefficient expansion; statistical output is explicitly f64.
use super::{Aggregate, Function};
use crate::{ErrorCategory, SemanticError, Value};
use bigdecimal::BigDecimal;
use num_bigint::BigInt;
use num_traits::{One, Signed, ToPrimitive, Zero};
use std::borrow::Cow;

pub(super) struct Budget<F> {
    pub max_bytes: usize,
    pub charge: F,
}

impl<F: FnMut(usize, usize) -> Result<(), SemanticError>> Budget<F> {
    fn admit(&mut self, bits: u128, quadratic: bool) -> Result<(), SemanticError> {
        // Include operands, result and multiplication/division scratch. This is
        // deliberately conservative and describes storage, not decimal precision.
        let bytes = bits
            .div_ceil(8)
            .checked_mul(8)
            .and_then(|n| n.checked_add(128))
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(capacity)?;
        if bytes > self.max_bytes {
            return Err(capacity());
        }
        let limbs = bits.div_ceil(64).max(1);
        let work = if quadratic {
            limbs.checked_mul(limbs)
        } else {
            Some(limbs)
        }
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(capacity)?;
        (self.charge)(work, bytes)
    }

    fn tick(&mut self) -> Result<(), SemanticError> {
        (self.charge)(1, 0)
    }
}

fn capacity() -> SemanticError {
    SemanticError::new(
        ErrorCategory::Busy,
        "query/numeric-capacity",
        "numeric coefficient work exceeds the query numeric allocation allowance",
    )
}
fn arithmetic(message: &str) -> SemanticError {
    SemanticError::incorrect("query/arithmetic", message)
}
fn range() -> SemanticError {
    SemanticError::incorrect(
        "query/arithmetic-range",
        "numeric result cannot be represented in the selected result domain",
    )
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Domain {
    Long,
    BigInt,
    Decimal,
    Float,
}

fn domain(value: &Value) -> Result<Domain, SemanticError> {
    match value {
        Value::Long(_) => Ok(Domain::Long),
        Value::BigInt(_) => Ok(Domain::BigInt),
        Value::BigDec(_) => Ok(Domain::Decimal),
        Value::Float(_) | Value::Double(_) => Ok(Domain::Float),
        _ => Err(SemanticError::incorrect(
            "query/numeric-type",
            "numeric operation requires long, bigint, bigdec, float, or double",
        )),
    }
}

fn coefficient(value: &Value) -> (Cow<'_, BigInt>, i64) {
    match value {
        Value::Long(n) => (Cow::Owned(BigInt::from(*n)), 0),
        Value::BigInt(n) => (Cow::Borrowed(n), 0),
        Value::BigDec(n) => n.as_bigint_and_scale(),
        _ => unreachable!(),
    }
}

fn power<F: FnMut(usize, usize) -> Result<(), SemanticError>>(
    base: u8,
    mut exponent: u64,
    budget: &mut Budget<F>,
) -> Result<BigInt, SemanticError> {
    budget.admit(u128::from(exponent) * 4 + 1, true)?;
    let mut result = BigInt::one();
    let mut factor = BigInt::from(base);
    while exponent != 0 {
        budget.tick()?;
        if exponent & 1 != 0 {
            result *= &factor;
        }
        exponent >>= 1;
        if exponent != 0 {
            factor = &factor * &factor;
        }
    }
    Ok(result)
}

fn decimal_result<F: FnMut(usize, usize) -> Result<(), SemanticError>>(
    mut coefficient: BigInt,
    mut scale: i128,
    budget: &mut Budget<F>,
) -> Result<Value, SemanticError> {
    if coefficient.is_zero() {
        return Ok(Value::BigDec(BigDecimal::new(
            coefficient,
            i64::try_from(scale).unwrap_or(0),
        )));
    }
    // Rebalance only at the representation boundary. Ordinary calculated
    // decimal scale remains untouched; compact valid results must not fail on
    // an overflowing intermediate scale.
    while scale > i128::from(i64::MAX) {
        budget.admit(u128::from(coefficient.bits()), false)?;
        if !(&coefficient % 10_u8).is_zero() {
            return Err(range());
        }
        coefficient /= 10_u8;
        scale -= 1;
    }
    if scale < i128::from(i64::MIN) {
        let places = u64::try_from(i128::from(i64::MIN) - scale).map_err(|_| capacity())?;
        budget.admit(
            u128::from(coefficient.bits()) + u128::from(places) * 4,
            true,
        )?;
        coefficient *= power(10, places, budget)?;
        scale = i128::from(i64::MIN);
    }
    let scale = i64::try_from(scale).map_err(|_| range())?;
    Ok(Value::BigDec(BigDecimal::new(coefficient, scale)))
}

fn decimal<F: FnMut(usize, usize) -> Result<(), SemanticError>>(
    function: &Function,
    left: &Value,
    right: &Value,
    budget: &mut Budget<F>,
) -> Result<Value, SemanticError> {
    let (a, sa) = coefficient(left);
    let (b, sb) = coefficient(right);
    let bits = u128::from(a.bits().max(b.bits()));
    budget.admit(bits + 1, false)?;
    if matches!(function, Function::Divide) && b.is_zero() {
        return Err(arithmetic("division by zero"));
    }
    // These cases must precede scale arithmetic, including decimal 1.0 at
    // either extreme of the representable scale range.
    if matches!(function, Function::Multiply | Function::Divide)
        && right.index_cmp(&Value::Long(1)).is_eq()
    {
        return decimal_result(a.into_owned(), i128::from(sa), budget);
    }
    if matches!(function, Function::Multiply) && left.index_cmp(&Value::Long(1)).is_eq() {
        return decimal_result(b.into_owned(), i128::from(sb), budget);
    }
    if matches!(function, Function::Multiply | Function::Divide) && a.is_zero()
        || matches!(function, Function::Multiply) && b.is_zero()
    {
        return decimal_result(BigInt::zero(), 0, budget);
    }
    if matches!(function, Function::Add | Function::Subtract) && b.is_zero() {
        return decimal_result(a.into_owned(), i128::from(sa), budget);
    }
    if matches!(function, Function::Add) && a.is_zero() {
        return decimal_result(b.into_owned(), i128::from(sb), budget);
    }
    if matches!(function, Function::Subtract) && a.is_zero() {
        return decimal_result(-b.into_owned(), i128::from(sb), budget);
    }
    if matches!(function, Function::Subtract | Function::Divide) && left.index_cmp(right).is_eq() {
        return decimal_result(
            BigInt::from(i32::from(matches!(function, Function::Divide))),
            0,
            budget,
        );
    }
    match function {
        Function::Add | Function::Subtract => {
            let scale = sa.max(sb);
            let da = u64::try_from(i128::from(scale) - i128::from(sa)).map_err(|_| capacity())?;
            let db = u64::try_from(i128::from(scale) - i128::from(sb)).map_err(|_| capacity())?;
            let growth = (u128::from(a.bits()) + u128::from(da) * 4)
                .max(u128::from(b.bits()) + u128::from(db) * 4)
                + 1;
            budget.admit(growth, false)?;
            let a = if da == 0 {
                a.into_owned()
            } else {
                a.as_ref() * power(10, da, budget)?
            };
            let b = if db == 0 {
                b.into_owned()
            } else {
                b.as_ref() * power(10, db, budget)?
            };
            decimal_result(
                if matches!(function, Function::Add) {
                    a + b
                } else {
                    a - b
                },
                i128::from(scale),
                budget,
            )
        }
        Function::Multiply => {
            budget.admit(u128::from(a.bits()) + u128::from(b.bits()), true)?;
            decimal_result(
                a.as_ref() * b.as_ref(),
                i128::from(sa) + i128::from(sb),
                budget,
            )
        }
        Function::Divide => {
            budget.admit(bits * 2 + 1, true)?;
            let mut gcd = a.abs();
            let mut remainder = b.abs();
            while !remainder.is_zero() {
                budget.admit(u128::from(gcd.bits().max(remainder.bits())), true)?;
                let next = &gcd % &remainder;
                gcd = remainder;
                remainder = next;
            }
            let mut numerator = a.as_ref() / &gcd;
            let mut denominator = b.as_ref() / gcd;
            if denominator.is_negative() {
                denominator = -denominator;
                numerator = -numerator;
            }
            let twos = denominator.trailing_zeros().unwrap_or(0);
            denominator >>= usize::try_from(twos).map_err(|_| capacity())?;
            let mut fives = 0_u64;
            while (&denominator % 5_u8).is_zero() {
                budget.admit(u128::from(denominator.bits()), false)?;
                denominator /= 5_u8;
                fives += 1;
            }
            if !denominator.is_one() {
                return Err(SemanticError::incorrect(
                    "query/non-terminating-decimal-division",
                    "exact decimal division has a non-terminating expansion",
                ));
            }
            let places = twos.max(fives);
            budget.admit(
                u128::from(numerator.bits()) + u128::from(places) * 4 + 1,
                true,
            )?;
            if places > twos {
                numerator *= power(2, places - twos, budget)?;
            }
            if places > fives {
                numerator *= power(5, places - fives, budget)?;
            }
            decimal_result(
                numerator,
                i128::from(sa) - i128::from(sb) + i128::from(places),
                budget,
            )
        }
        _ => unreachable!(),
    }
}

pub(super) fn binary<F: FnMut(usize, usize) -> Result<(), SemanticError>>(
    function: &Function,
    left: &Value,
    right: &Value,
    budget: &mut Budget<F>,
) -> Result<Value, SemanticError> {
    let selected = domain(left)?.max(domain(right)?);
    budget.tick()?;
    match selected {
        Domain::Long => {
            let (Value::Long(a), Value::Long(b)) = (left, right) else {
                unreachable!()
            };
            let value = match function {
                Function::Add => a.checked_add(*b),
                Function::Subtract => a.checked_sub(*b),
                Function::Multiply => a.checked_mul(*b),
                Function::Divide => a.checked_div(*b),
                _ => unreachable!(),
            }
            .ok_or_else(|| arithmetic("integer arithmetic overflow or division by zero"))?;
            Ok(Value::Long(value))
        }
        Domain::BigInt => {
            let (a, _) = coefficient(left);
            let (b, _) = coefficient(right);
            budget.admit(
                u128::from(a.bits()) + u128::from(b.bits()) + 1,
                matches!(function, Function::Multiply | Function::Divide),
            )?;
            Ok(Value::BigInt(match function {
                Function::Add => a.as_ref() + b.as_ref(),
                Function::Subtract => a.as_ref() - b.as_ref(),
                Function::Multiply => a.as_ref() * b.as_ref(),
                Function::Divide if !b.is_zero() => a.as_ref() / b.as_ref(),
                Function::Divide => return Err(arithmetic("division by zero")),
                _ => unreachable!(),
            }))
        }
        Domain::Decimal => decimal(function, left, right, budget),
        Domain::Float => {
            let a = to_float(left, budget)?;
            let b = to_float(right, budget)?;
            if matches!(function, Function::Divide) && b == 0.0 {
                return Err(arithmetic("division by zero"));
            }
            Ok(Value::Double(match function {
                Function::Add => a + b,
                Function::Subtract => a - b,
                Function::Multiply => a * b,
                Function::Divide => a / b,
                _ => unreachable!(),
            }))
        }
    }
}

/// Integer quotient, truncated toward zero, without converting exact operands
/// through floating point. Decimal integer results may retain a compact negative
/// scale, so `quot(1e100000, 2)` need not allocate one hundred thousand zeros.
pub(super) fn quotient<F: FnMut(usize, usize) -> Result<(), SemanticError>>(
    left: &Value,
    right: &Value,
    budget: &mut Budget<F>,
) -> Result<Value, SemanticError> {
    match domain(left)?.max(domain(right)?) {
        Domain::Long | Domain::BigInt => binary(&Function::Divide, left, right, budget),
        Domain::Float => {
            budget.tick()?;
            let a = to_float(left, budget)?;
            let b = to_float(right, budget)?;
            if !a.is_finite() || !b.is_finite() {
                return Err(arithmetic("quot requires finite numeric operands"));
            }
            if b == 0.0 {
                return Err(arithmetic("division by zero"));
            }
            let quotient = (a / b).trunc();
            if !quotient.is_finite() {
                return Err(range());
            }
            Ok(Value::Double(quotient))
        }
        Domain::Decimal => {
            budget.tick()?;
            let (a, sa) = coefficient(left);
            let (b, sb) = coefficient(right);
            budget.admit(u128::from(a.bits()) + u128::from(b.bits()) + 1, false)?;
            if b.is_zero() {
                return Err(arithmetic("division by zero"));
            }
            // Exact comparison is exponent-bounded; zero quotients must not
            // expand a denominator with a compact extreme exponent.
            let magnitude_a = Value::BigDec(BigDecimal::new(a.abs(), sa));
            let magnitude_b = Value::BigDec(BigDecimal::new(b.abs(), sb));
            if magnitude_a.index_cmp(&magnitude_b).is_lt() {
                return decimal_result(BigInt::zero(), 0, budget);
            }
            match decimal(&Function::Divide, left, right, budget) {
                Ok(Value::BigDec(exact)) => {
                    let (coefficient, scale) = exact.as_bigint_and_scale();
                    if scale <= 0 {
                        return Ok(Value::BigDec(exact));
                    }
                    let power = power(10, scale as u64, budget)?;
                    budget.admit(u128::from(coefficient.bits()), true)?;
                    decimal_result(coefficient.as_ref() / power, 0, budget)
                }
                Ok(_) => unreachable!("decimal arithmetic returns a decimal"),
                Err(error) if error.code == "query/non-terminating-decimal-division" => {
                    let shift = i128::from(sb) - i128::from(sa);
                    let places = u64::try_from(shift.unsigned_abs()).map_err(|_| capacity())?;
                    budget.admit(
                        u128::from(a.bits()) + u128::from(b.bits()) + u128::from(places) * 4,
                        true,
                    )?;
                    let factor = power(10, places, budget)?;
                    let quotient = if shift >= 0 {
                        (a.as_ref() * factor) / b.as_ref()
                    } else {
                        a.as_ref() / (b.as_ref() * factor)
                    };
                    decimal_result(quotient, 0, budget)
                }
                Err(error) => Err(error),
            }
        }
    }
}

fn scientific<F: FnMut(usize, usize) -> Result<(), SemanticError>>(
    coefficient: &BigInt,
    scale: i128,
    budget: &mut Budget<F>,
) -> Result<f64, SemanticError> {
    scientific_with_underflow(coefficient, scale, false, budget)
}

fn scientific_with_underflow<F: FnMut(usize, usize) -> Result<(), SemanticError>>(
    coefficient: &BigInt,
    scale: i128,
    allow_underflow: bool,
    budget: &mut Budget<F>,
) -> Result<f64, SemanticError> {
    budget.admit(u128::from(coefficient.bits()) * 4 + 1, false)?;
    if coefficient.is_zero() {
        return Ok(0.0);
    }
    let text = format!("{coefficient}e{}", -scale);
    let value = text.parse::<f64>().map_err(|_| range())?;
    if !value.is_finite() || value == 0.0 && !allow_underflow {
        return Err(range());
    }
    Ok(value)
}

fn to_float<F: FnMut(usize, usize) -> Result<(), SemanticError>>(
    value: &Value,
    budget: &mut Budget<F>,
) -> Result<f64, SemanticError> {
    match value {
        Value::Long(n) => Ok(*n as f64),
        Value::Float(n) => Ok(f64::from(*n)),
        Value::Double(n) => Ok(*n),
        Value::BigInt(n) => {
            budget.admit(u128::from(n.bits()) + 1, false)?;
            n.to_f64().filter(|v| v.is_finite()).ok_or_else(range)
        }
        Value::BigDec(n) => {
            let (n, scale) = n.as_bigint_and_scale();
            scientific(&n, i128::from(scale), budget)
        }
        _ => {
            domain(value)?;
            unreachable!()
        }
    }
}

fn zero(domain: Domain) -> Value {
    match domain {
        Domain::Long => Value::Long(0),
        Domain::BigInt => Value::BigInt(BigInt::zero()),
        Domain::Decimal => Value::BigDec(BigDecimal::new(BigInt::zero(), 0)),
        Domain::Float => Value::Double(0.0),
    }
}

fn sum<F: FnMut(usize, usize) -> Result<(), SemanticError>>(
    values: &[&Value],
    selected: Domain,
    budget: &mut Budget<F>,
) -> Result<Value, SemanticError> {
    values.iter().try_fold(zero(selected), |total, value| {
        binary(&Function::Add, &total, value, budget)
    })
}

fn mean<F: FnMut(usize, usize) -> Result<(), SemanticError>>(
    values: &[&Value],
    selected: Domain,
    budget: &mut Budget<F>,
) -> Result<f64, SemanticError> {
    // Unlike checked Long sum, statistics widen their exact accumulator. Divide
    // before floating conversion: large cancellation and large finite averages
    // must not overflow merely because individual terms/the sum exceed f64.
    let total = sum(values, selected.max(Domain::BigInt), budget)?;
    let (numerator, scale) = coefficient(&total);
    approximate_ratio(
        &numerator,
        &BigInt::from(values.len()),
        i128::from(scale),
        false,
        budget,
    )
}

fn approximate_ratio<F: FnMut(usize, usize) -> Result<(), SemanticError>>(
    numerator: &BigInt,
    denominator: &BigInt,
    scale: i128,
    allow_underflow: bool,
    budget: &mut Budget<F>,
) -> Result<f64, SemanticError> {
    if numerator.is_zero() {
        return Ok(0.0);
    }
    budget.admit(
        u128::from(numerator.bits().max(denominator.bits())) * 4 + 256,
        false,
    )?;
    let digits = numerator.to_str_radix(10).trim_start_matches('-').len();
    let denominator_digits = denominator.to_str_radix(10).trim_start_matches('-').len();
    // Statistical results are approximate Doubles. Forty decimal significant
    // digits here provide guard digits without imposing precision on exact ops.
    let shift = (40 + denominator_digits).saturating_sub(digits) as u64;
    let numerator = if shift == 0 {
        numerator.clone()
    } else {
        numerator * power(10, shift, budget)?
    };
    budget.admit(
        u128::from(numerator.bits().max(denominator.bits())) + 1,
        true,
    )?;
    scientific_with_underflow(
        &(numerator / denominator),
        scale + i128::from(shift),
        allow_underflow,
        budget,
    )
}

fn centered_value<F: FnMut(usize, usize) -> Result<(), SemanticError>>(
    value: &Value,
    center: &Value,
    budget: &mut Budget<F>,
) -> Result<Value, SemanticError> {
    binary(&Function::Subtract, value, center, budget)
}

fn scaled_exact_statistic<F: FnMut(usize, usize) -> Result<(), SemanticError>>(
    function: Aggregate,
    values: &[&Value],
    center: &Value,
    budget: &mut Budget<F>,
) -> Result<Value, SemanticError> {
    // At most the center, largest difference and current difference are live.
    // Normalize exact coefficients before converting: 2e308 may be an invalid
    // Double delta while the standard deviation of [-1e308,1e308] is 1e308.
    let mut largest = Value::Long(0);
    for value in values {
        let difference = centered_value(value, center, budget)?;
        let (n, scale) = coefficient(&difference);
        budget.admit(u128::from(n.bits()) + 1, false)?;
        let magnitude = Value::BigDec(BigDecimal::new(n.abs(), scale));
        if magnitude.index_cmp(&largest).is_gt() {
            largest = magnitude;
        }
    }
    let (maximum, maximum_scale) = coefficient(&largest);
    if maximum.is_zero() {
        return Ok(Value::Double(0.0));
    }
    let mut normalized = Vec::with_capacity(values.len());
    for value in values {
        let difference = centered_value(value, center, budget)?;
        let (n, scale) = coefficient(&difference);
        normalized.push(approximate_ratio(
            &n,
            &maximum,
            i128::from(scale) - i128::from(maximum_scale),
            true,
            budget,
        )?);
    }
    let mean = normalized.iter().sum::<f64>() / normalized.len() as f64;
    let variance = normalized
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f64>()
        / normalized.len() as f64;
    let factor = if function == Aggregate::StandardDeviation {
        variance.sqrt()
    } else {
        variance
    };
    // Convert only after combining the exact scale with the approximate factor.
    // No intermediate max/delta needs to fit a Double.
    // IEEE f64 -> exact decimal needs fewer than 4096 coefficient bits,
    // including the smallest subnormal (5^1074).
    budget.admit(4096, false)?;
    let factor = BigDecimal::try_from(factor).map_err(|_| range())?;
    let (factor, factor_scale) = factor.as_bigint_and_scale();
    let exponent = if function == Aggregate::StandardDeviation {
        1
    } else {
        2
    };
    budget.admit(
        u128::from(maximum.bits()) * exponent + u128::from(factor.bits()) + 1,
        true,
    )?;
    let coefficient = if exponent == 1 {
        maximum.as_ref() * factor.as_ref()
    } else {
        maximum.as_ref() * maximum.as_ref() * factor.as_ref()
    };
    scientific(
        &coefficient,
        i128::from(maximum_scale) * exponent as i128 + i128::from(factor_scale),
        budget,
    )
    .map(Value::Double)
}

pub(super) fn aggregate<F: FnMut(usize, usize) -> Result<(), SemanticError>>(
    function: Aggregate,
    mut values: Vec<&Value>,
    budget: &mut Budget<F>,
) -> Result<Value, SemanticError> {
    if values.is_empty() {
        return if function == Aggregate::Sum {
            Ok(Value::Long(0))
        } else {
            Err(SemanticError::incorrect(
                "query/empty-aggregate",
                "numeric aggregate has no values",
            ))
        };
    }
    let selected = values.iter().try_fold(Domain::Long, |selected, value| {
        Ok::<_, SemanticError>(selected.max(domain(value)?))
    })?;
    if function == Aggregate::Sum {
        return sum(&values, selected, budget);
    }
    if function == Aggregate::Median && selected != Domain::Float {
        budget.admit((values.len() as u128) * 64, false)?;
        values.sort_by(|a, b| a.index_cmp(b));
        let middle = values.len() / 2;
        if values.len() % 2 == 1 {
            let (n, _) = coefficient(values[middle]);
            budget.admit(u128::from(n.bits()) + 1, false)?;
            return Ok(values[middle].clone());
        }
        if let (Value::Long(a), Value::Long(b)) = (values[middle - 1], values[middle]) {
            return Ok(Value::Long(((i128::from(*a) + i128::from(*b)) / 2) as i64));
        }
        let pair = binary(&Function::Add, values[middle - 1], values[middle], budget)?;
        return binary(&Function::Divide, &pair, &Value::Long(2), budget);
    }
    if function == Aggregate::Average && selected != Domain::Float {
        return mean(&values, selected, budget).map(Value::Double);
    }
    budget.admit((values.len() as u128) * 64, false)?;
    // Center arbitrary-precision statistics exactly, so very large nearby
    // integers/decimals do not collapse to identical floating values first.
    let mut numeric = if selected != Domain::Float
        && matches!(function, Aggregate::Variance | Aggregate::StandardDeviation)
    {
        let (first, _) = coefficient(values[0]);
        budget.admit(u128::from(first.bits()) + 1, false)?;
        let center = match values[0] {
            Value::Long(n) => Value::BigInt(BigInt::from(*n)),
            value => value.clone(),
        };
        let numeric = values
            .iter()
            .map(|value| {
                let centered = centered_value(value, &center, budget)?;
                to_float(&centered, budget)
            })
            .collect::<Result<Vec<_>, _>>();
        match numeric {
            Ok(numeric) => numeric,
            Err(error) if error.code == "query/arithmetic-range" => {
                return scaled_exact_statistic(function, &values, &center, budget);
            }
            Err(error) => return Err(error),
        }
    } else {
        values
            .iter()
            .map(|value| to_float(value, budget))
            .collect::<Result<Vec<_>, _>>()?
    };
    let average = numeric.iter().sum::<f64>() / numeric.len() as f64;
    let mut result = match function {
        Aggregate::Average => average,
        Aggregate::Median => {
            numeric.sort_by(f64::total_cmp);
            let m = numeric.len() / 2;
            if numeric.len() % 2 == 1 {
                numeric[m]
            } else {
                (numeric[m - 1] + numeric[m]) / 2.0
            }
        }
        Aggregate::Variance | Aggregate::StandardDeviation => {
            let variance = numeric
                .iter()
                .map(|value| (value - average).powi(2))
                .sum::<f64>()
                / numeric.len() as f64;
            if function == Aggregate::StandardDeviation {
                variance.sqrt()
            } else {
                variance
            }
        }
        _ => unreachable!(),
    };
    if selected != Domain::Float
        && matches!(function, Aggregate::Variance | Aggregate::StandardDeviation)
        && (!result.is_finite() || result == 0.0)
    {
        // A representable standard deviation can have a variance outside f64.
        // Rescale only on range trouble, retaining normal-case operation order.
        let scale = numeric
            .iter()
            .fold(0.0_f64, |scale, value| scale.max(value.abs()));
        if scale != 0.0 {
            let center =
                numeric.iter().map(|value| value / scale).sum::<f64>() / numeric.len() as f64;
            let variance = numeric
                .iter()
                .map(|value| (value / scale - center).powi(2))
                .sum::<f64>()
                / numeric.len() as f64;
            result = if function == Aggregate::StandardDeviation {
                variance.sqrt() * scale
            } else {
                (variance * scale) * scale
            };
            if variance != 0.0 && result == 0.0 {
                return Err(range());
            }
        }
    }
    if selected != Domain::Float && !result.is_finite() {
        return Err(range());
    }
    Ok(Value::Double(result))
}
