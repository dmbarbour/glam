use std::fmt;

use num_bigint::BigInt;
use num_integer::Integer;
use num_rational::BigRational;
use num_traits::{Signed, ToPrimitive, Zero};

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Number(BigRational);

impl Number {
    pub fn integer(value: i64) -> Self {
        Self(BigRational::from_integer(BigInt::from(value)))
    }

    pub fn from_u8(value: u8) -> Self {
        Self::integer(i64::from(value))
    }

    pub fn from_usize(value: usize) -> Self {
        Self(BigRational::from_integer(BigInt::from(value)))
    }

    pub fn from_ratio_i64(numerator: i64, denominator: i64) -> Option<Self> {
        (denominator != 0).then(|| {
            Self(BigRational::new(
                BigInt::from(numerator),
                BigInt::from(denominator),
            ))
        })
    }

    pub fn from_f64(value: f64) -> Option<Self> {
        value
            .is_finite()
            .then(|| BigRational::from_float(value))
            .flatten()
            .map(Self)
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let (negative, rest) =
            if let Some(rest) = text.strip_prefix('_').or_else(|| text.strip_prefix('-')) {
                (true, rest)
            } else {
                (false, text)
            };

        if rest.is_empty() {
            return Err("missing digits".to_owned());
        }

        if rest.matches('/').count() > 1 {
            return Err(format!(
                "ambiguous `/` chain in `{text}`; parenthesize or add spaces, e.g. `(3/4) / 5` or `3 / (4/5)`"
            ));
        }

        let number = if let Some((numerator, denominator)) = split_once(rest, '/') {
            let numerator = parse_grouped_bigint(numerator, 10, is_decimal_digit)?;
            let denominator = parse_grouped_bigint(denominator, 10, is_decimal_digit)?;
            if denominator.is_zero() {
                return Err("division by zero in rational literal".to_owned());
            }
            Self(BigRational::new(numerator, denominator))
        } else if let Some(hex) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
            Self(BigRational::from_integer(parse_grouped_bigint(
                hex,
                16,
                is_hex_digit,
            )?))
        } else if let Some(binary) = rest.strip_prefix("0b").or_else(|| rest.strip_prefix("0B")) {
            Self(BigRational::from_integer(parse_grouped_bigint(
                binary,
                2,
                is_binary_digit,
            )?))
        } else {
            parse_decimal_or_scientific(rest)?
        };

        Ok(if negative { number.negated() } else { number })
    }

    pub fn to_u8_if_integer(&self) -> Option<u8> {
        if !self.0.is_integer() {
            return None;
        }

        self.0.to_integer().to_u8()
    }

    pub fn to_i64_if_integer(&self) -> Option<i64> {
        if !self.0.is_integer() {
            return None;
        }

        self.0.to_integer().to_i64()
    }

    pub fn to_ratio_i64(&self) -> Option<(i64, i64)> {
        Some((self.0.numer().to_i64()?, self.0.denom().to_i64()?))
    }

    pub fn to_f64(&self) -> Option<f64> {
        self.0.to_f64().filter(|value| value.is_finite())
    }

    pub fn add(&self, other: &Self) -> Self {
        Self(&self.0 + &other.0)
    }

    pub fn sub(&self, other: &Self) -> Self {
        Self(&self.0 - &other.0)
    }

    pub fn mul(&self, other: &Self) -> Self {
        Self(&self.0 * &other.0)
    }

    pub fn checked_div(&self, other: &Self) -> Option<Self> {
        if other.0.is_zero() {
            None
        } else {
            Some(Self(&self.0 / &other.0))
        }
    }

    pub fn floor(&self) -> Self {
        Self(BigRational::from_integer(
            self.0.numer().div_floor(self.0.denom()),
        ))
    }

    pub fn checked_mod(&self, other: &Self) -> Option<Self> {
        let quotient = self.checked_div(other)?.floor();
        Some(self.sub(&other.mul(&quotient)))
    }

    pub fn to_usize_if_integer(&self) -> Option<usize> {
        if !self.0.is_integer() || self.0.is_negative() {
            return None;
        }

        self.0.to_integer().to_usize()
    }

    fn negated(self) -> Self {
        Self(-self.0)
    }
}

impl From<i64> for Number {
    fn from(value: i64) -> Self {
        Self::integer(value)
    }
}

impl fmt::Debug for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if *self.0.denom() == BigInt::from(1_u8) {
            write!(f, "{}", self.0.numer())
        } else {
            write!(f, "{}/{}", self.0.numer(), self.0.denom())
        }
    }
}

fn parse_decimal_or_scientific(text: &str) -> Result<Number, String> {
    let (mantissa, exponent) = if let Some((mantissa, exponent)) = split_once_either(text, 'e', 'E')
    {
        (mantissa, parse_exponent(exponent)?)
    } else {
        (text, 0_i64)
    };

    let (numerator, scale) = if let Some((whole, fractional)) = split_once(mantissa, '.') {
        let whole = clean_grouped_digits(whole, is_decimal_digit)?;
        let fractional = clean_grouped_digits(fractional, is_decimal_digit)?;
        let digits = format!("{whole}{fractional}");
        let numerator = BigInt::parse_bytes(digits.as_bytes(), 10)
            .ok_or_else(|| format!("invalid decimal literal `{text}`"))?;
        (numerator, fractional.len())
    } else {
        let digits = clean_grouped_digits(mantissa, is_decimal_digit)?;
        let numerator = BigInt::parse_bytes(digits.as_bytes(), 10)
            .ok_or_else(|| format!("invalid integer literal `{text}`"))?;
        (numerator, 0)
    };

    let mut rational = BigRational::new(numerator, pow10(scale));
    if exponent > 0 {
        rational *= BigRational::from_integer(pow10(exponent as usize));
    } else if exponent < 0 {
        rational /= BigRational::from_integer(pow10((-exponent) as usize));
    }

    Ok(Number(rational))
}

fn parse_exponent(text: &str) -> Result<i64, String> {
    let (negative, rest) =
        if let Some(rest) = text.strip_prefix('_').or_else(|| text.strip_prefix('-')) {
            (true, rest)
        } else {
            (false, text)
        };
    let digits = clean_grouped_digits(rest, is_decimal_digit)?;
    let exponent = digits
        .parse::<i64>()
        .map_err(|err| format!("invalid exponent `{text}`: {err}"))?;
    Ok(if negative { -exponent } else { exponent })
}

fn parse_grouped_bigint(
    text: &str,
    radix: u32,
    is_digit: impl Fn(char) -> bool,
) -> Result<BigInt, String> {
    let digits = clean_grouped_digits(text, is_digit)?;
    BigInt::parse_bytes(digits.as_bytes(), radix)
        .ok_or_else(|| format!("invalid numeric literal `{text}`"))
}

fn clean_grouped_digits(text: &str, is_digit: impl Fn(char) -> bool) -> Result<String, String> {
    if text.is_empty() {
        return Err("missing digits".to_owned());
    }

    let mut cleaned = String::with_capacity(text.len());
    let mut previous_was_underscore = false;
    let mut saw_digit = false;

    for ch in text.chars() {
        if is_digit(ch) {
            cleaned.push(ch);
            previous_was_underscore = false;
            saw_digit = true;
        } else if ch == '_' {
            if !saw_digit || previous_was_underscore {
                return Err(format!("invalid digit separator placement in `{text}`"));
            }
            previous_was_underscore = true;
        } else {
            return Err(format!("invalid digit `{ch}` in `{text}`"));
        }
    }

    if previous_was_underscore {
        return Err(format!("invalid digit separator placement in `{text}`"));
    }

    Ok(cleaned)
}

fn pow10(exponent: usize) -> BigInt {
    BigInt::from(10_u8).pow(exponent as u32)
}

fn split_once(text: &str, needle: char) -> Option<(&str, &str)> {
    let mut parts = text.split(needle);
    let left = parts.next()?;
    let right = parts.next()?;
    if parts.next().is_some() {
        None
    } else {
        Some((left, right))
    }
}

fn split_once_either(text: &str, left: char, right: char) -> Option<(&str, &str)> {
    let left_index = text.find(left);
    let right_index = text.find(right);
    let index = match (left_index, right_index) {
        (Some(left_index), Some(right_index)) => left_index.min(right_index),
        (Some(index), None) | (None, Some(index)) => index,
        (None, None) => return None,
    };
    let (prefix, suffix) = text.split_at(index);
    Some((prefix, &suffix[1..]))
}

fn is_decimal_digit(ch: char) -> bool {
    ch.is_ascii_digit()
}

fn is_binary_digit(ch: char) -> bool {
    matches!(ch, '0' | '1')
}

fn is_hex_digit(ch: char) -> bool {
    ch.is_ascii_hexdigit()
}

#[cfg(test)]
mod tests {
    use super::Number;

    #[test]
    fn parses_integer_forms() {
        assert_eq!(Number::parse("42").unwrap().to_string(), "42");
        assert_eq!(Number::parse("_42").unwrap().to_string(), "-42");
        assert_eq!(Number::parse("1_000_000").unwrap().to_string(), "1000000");
        assert_eq!(Number::parse("0xc0de").unwrap().to_string(), "49374");
        assert_eq!(Number::parse("_0b1011_1010").unwrap().to_string(), "-186");
    }

    #[test]
    fn parses_rational_and_scientific_forms() {
        assert_eq!(Number::parse("1/6").unwrap().to_string(), "1/6");
        assert_eq!(Number::parse("-3/2").unwrap().to_string(), "-3/2");
        assert_eq!(Number::parse("1.234").unwrap().to_string(), "617/500");
        assert_eq!(
            Number::parse("1.234e_7").unwrap().to_string(),
            "617/5000000000"
        );
        assert_eq!(Number::parse("12e3").unwrap().to_string(), "12000");
    }

    #[test]
    fn converts_through_small_and_lossy_public_shapes() {
        let ratio = Number::from_ratio_i64(-6, -4).unwrap();
        assert_eq!(ratio.to_string(), "3/2");
        assert_eq!(ratio.to_ratio_i64(), Some((3, 2)));
        assert_eq!(ratio.to_i64_if_integer(), None);
        assert_eq!(ratio.to_f64(), Some(1.5));

        assert_eq!(Number::from_ratio_i64(1, 0), None);
        assert_eq!(Number::from_f64(f64::NAN), None);
        assert_eq!(Number::from_f64(f64::INFINITY), None);
        assert_eq!(Number::from_f64(f64::NEG_INFINITY), None);
        assert_eq!(Number::from_f64(1.5).unwrap(), ratio);
    }

    #[test]
    fn floors_and_mods_rationals() {
        assert_eq!(Number::parse("7/2").unwrap().floor().to_string(), "3");
        assert_eq!(Number::parse("_7/2").unwrap().floor().to_string(), "-4");
        assert_eq!(
            Number::parse("17/5")
                .unwrap()
                .checked_mod(&Number::parse("3/2").unwrap())
                .unwrap()
                .to_string(),
            "2/5"
        );
        assert_eq!(
            Number::parse("17/5")
                .unwrap()
                .checked_mod(&Number::parse("_3/2").unwrap())
                .unwrap()
                .to_string(),
            "-11/10"
        );
    }

    #[test]
    fn rejects_ambiguous_slash_chains() {
        let err = Number::parse("3/4/5").unwrap_err();

        assert!(err.contains("ambiguous `/` chain"));
    }
}
