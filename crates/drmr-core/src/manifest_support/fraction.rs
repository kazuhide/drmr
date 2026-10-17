//! `fractions.Fraction` as `CPython` 3.11 implements the parts the manifest modules use: exact
//! normalized rationals over unbounded integers, the string grammar (`'30000/1001'`,
//! `'29.97'`, `'1e3'`, …), `ZeroDivisionError('Fraction(n, 0)')`, and `float` / `int` /
//! `math.floor` / `math.ceil` with Python's rounding.

use super::py::{PyErr, decimal_value, is_space, parse_int, repr_str};
use num_bigint::BigInt;
use num_integer::Integer;
use num_rational::BigRational;
use num_traits::{One, Signed, ToPrimitive, Zero};
use std::cmp::Ordering;
use std::fmt;
use std::ops::{Add, Mul, Neg, Sub};

/// An exact rational number, normalized (lowest terms, positive denominator).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Fraction(BigRational);

impl Fraction {
    /// `Fraction(numerator, denominator)`; a zero denominator is
    /// `ZeroDivisionError('Fraction(<numerator>, 0)')`.
    pub fn new(
        numerator: impl Into<BigInt>,
        denominator: impl Into<BigInt>,
    ) -> Result<Self, PyErr> {
        let numerator = numerator.into();
        let denominator = denominator.into();
        if denominator.is_zero() {
            return Err(PyErr::ZeroDivision(format!("Fraction({numerator}, 0)")));
        }
        Ok(Fraction(BigRational::new(numerator, denominator)))
    }

    /// `Fraction(n)` of an int.
    pub fn from_int(value: impl Into<BigInt>) -> Self {
        Fraction(BigRational::from_integer(value.into()))
    }

    /// `Fraction(text)` with `CPython` 3.11's grammar: optional whitespace, a sign, then either
    /// `num/denom` (no whitespace around `/` — 3.12 added it) or a decimal with an optional
    /// fraction and exponent; digits are Unicode decimals with single `_` between them.
    pub fn parse(text: &str) -> Result<Self, PyErr> {
        let invalid = || PyErr::Value(format!("Invalid literal for Fraction: {}", repr_str(text)));
        let chars: Vec<char> = text.chars().collect();
        let mut position = chars.iter().take_while(|c| is_space(**c)).count();
        let negative = match chars.get(position) {
            Some('-') => {
                position += 1;
                true
            }
            Some('+') => {
                position += 1;
                false
            }
            _ => false,
        };
        let starts_number = match chars.get(position) {
            Some('.') => chars
                .get(position + 1)
                .is_some_and(|c| decimal_value(*c).is_some()),
            Some(c) => decimal_value(*c).is_some(),
            None => false,
        };
        if !starts_number {
            return Err(invalid());
        }
        let numerator_digits = digit_group(&chars, &mut position);
        let mut numerator = digits_value(&numerator_digits)?;
        let mut denominator = BigInt::one();
        if chars.get(position) == Some(&'/') {
            position += 1;
            let digits = digit_group(&chars, &mut position);
            if digits.is_empty() {
                return Err(invalid());
            }
            denominator = digits_value(&digits)?;
        } else {
            if chars.get(position) == Some(&'.') {
                position += 1;
                let decimal = digit_group(&chars, &mut position);
                if !decimal.is_empty() {
                    let length = u32::try_from(decimal.len()).map_err(|_| invalid())?;
                    let scale = BigInt::from(10u32).pow(length);
                    numerator = numerator * &scale + digits_value(&decimal)?;
                    denominator *= scale;
                }
            }
            if matches!(chars.get(position), Some('e' | 'E')) {
                let mark = position;
                position += 1;
                let exponent_negative = match chars.get(position) {
                    Some('-') => {
                        position += 1;
                        true
                    }
                    Some('+') => {
                        position += 1;
                        false
                    }
                    _ => false,
                };
                let digits = digit_group(&chars, &mut position);
                if digits.is_empty() {
                    position = mark;
                } else {
                    let exponent = digits_value(&digits)?;
                    let power = BigInt::from(10u32).pow(exponent.to_u32().ok_or_else(invalid)?);
                    if exponent_negative {
                        denominator *= power;
                    } else {
                        numerator *= power;
                    }
                }
            }
        }
        position += chars[position..]
            .iter()
            .take_while(|c| is_space(**c))
            .count();
        if position != chars.len() {
            return Err(invalid());
        }
        if negative {
            numerator = -numerator;
        }
        Fraction::new(numerator, denominator)
    }

    /// The numerator (carries the sign).
    pub fn numerator(&self) -> &BigInt {
        self.0.numer()
    }

    /// The denominator (positive).
    pub fn denominator(&self) -> &BigInt {
        self.0.denom()
    }

    /// `float(fraction)`: the correctly rounded quotient (Python's `int / int`); a result too
    /// large for a float is `OverflowError('integer division result too large for a float')`.
    pub fn to_f64(&self) -> Result<f64, PyErr> {
        let value = self.0.to_f64().unwrap_or(f64::NAN);
        if value.is_finite() {
            Ok(value)
        } else {
            Err(PyErr::Overflow(
                "integer division result too large for a float".to_string(),
            ))
        }
    }

    /// `int(fraction)`: truncated toward zero.
    pub fn trunc(&self) -> BigInt {
        self.0.to_integer()
    }

    /// `math.floor(fraction)`.
    pub fn floor(&self) -> BigInt {
        self.numerator().div_floor(self.denominator())
    }

    /// `math.ceil(fraction)`.
    pub fn ceil(&self) -> BigInt {
        -((-self.numerator()).div_floor(self.denominator()))
    }

    /// `abs(fraction)`.
    #[must_use]
    pub fn abs(&self) -> Fraction {
        Fraction(self.0.abs())
    }

    /// Whether the value is an integer (`denominator == 1`).
    pub fn is_integer(&self) -> bool {
        self.0.is_integer()
    }

    /// Compare with zero.
    pub fn sign_cmp(&self) -> Ordering {
        self.numerator().cmp(&BigInt::zero())
    }
}

/// The run of digits and single underscores starting at `position` (the longest that is a
/// valid group: `1_000`, never a trailing `_`), returned as its digits.
fn digit_group(chars: &[char], position: &mut usize) -> Vec<char> {
    let mut digits = Vec::new();
    let mut end = *position;
    let mut cursor = *position;
    while let Some(character) = chars.get(cursor) {
        if decimal_value(*character).is_some() {
            digits.push(*character);
            cursor += 1;
            end = cursor;
        } else if *character == '_'
            && end == cursor
            && !digits.is_empty()
            && chars
                .get(cursor + 1)
                .is_some_and(|c| decimal_value(*c).is_some())
        {
            cursor += 1;
        } else {
            break;
        }
    }
    *position = end;
    digits
}

/// `int` of a run of Unicode decimal digits (an empty run is zero: Python's `int(num or
/// '0')`); more than 4300 digits is Python's `int` limit error.
fn digits_value(digits: &[char]) -> Result<BigInt, PyErr> {
    if digits.is_empty() {
        return Ok(BigInt::zero());
    }
    let text: String = digits.iter().collect();
    parse_int(&text)
}

impl fmt::Debug for Fraction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fraction({}, {})", self.numerator(), self.denominator())
    }
}

impl fmt::Display for Fraction {
    /// `str(fraction)`: `n` or `n/d`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_integer() {
            write!(f, "{}", self.numerator())
        } else {
            write!(f, "{}/{}", self.numerator(), self.denominator())
        }
    }
}

impl Add for &Fraction {
    type Output = Fraction;
    fn add(self, other: &Fraction) -> Fraction {
        Fraction(&self.0 + &other.0)
    }
}

impl Sub for &Fraction {
    type Output = Fraction;
    fn sub(self, other: &Fraction) -> Fraction {
        Fraction(&self.0 - &other.0)
    }
}

impl Mul for &Fraction {
    type Output = Fraction;
    fn mul(self, other: &Fraction) -> Fraction {
        Fraction(&self.0 * &other.0)
    }
}

impl Mul<&BigInt> for &Fraction {
    type Output = Fraction;
    fn mul(self, other: &BigInt) -> Fraction {
        Fraction(&self.0 * BigRational::from_integer(other.clone()))
    }
}

impl Neg for &Fraction {
    type Output = Fraction;
    fn neg(self) -> Fraction {
        Fraction(-&self.0)
    }
}
