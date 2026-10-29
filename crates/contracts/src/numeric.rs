//! Exact JSON decimal normalization and semantic equality.
use serde_json::{Number, Value};
pub struct Decimal {
    negative: bool,
    coefficient: String,
    exponent: i64,
}

impl Decimal {
    pub fn negative(&self) -> bool {
        self.negative
    }
    pub fn coefficient(&self) -> &str {
        &self.coefficient
    }
    pub fn exponent(&self) -> i64 {
        self.exponent
    }

    pub fn parse(number: &Number) -> Option<Self> {
        let text = number.to_string();
        let (negative, unsigned) = text
            .strip_prefix('-')
            .map_or((false, text.as_str()), |value| (true, value));
        let (mantissa, exponent) = match unsigned.split_once(['e', 'E']) {
            Some((mantissa, exponent)) => (mantissa, exponent.parse::<i64>().ok()?),
            None => (unsigned, 0),
        };
        let fraction = mantissa.split_once('.').map_or(0, |(_, value)| value.len());
        let digits: String = mantissa.chars().filter(|value| *value != '.').collect();
        if digits.is_empty() || !digits.bytes().all(|value| value.is_ascii_digit()) {
            return None;
        }
        let significant = digits.trim_start_matches('0');
        if significant.is_empty() {
            return Some(Self {
                negative: false,
                coefficient: "0".into(),
                exponent: 0,
            });
        }
        let coefficient = significant.trim_end_matches('0');
        let zeros = significant.len() - coefficient.len();
        Some(Self {
            negative,
            coefficient: coefficient.to_owned(),
            exponent: exponent
                .checked_sub(i64::try_from(fraction).ok()?)?
                .checked_add(i64::try_from(zeros).ok()?)?,
        })
    }
}

/// Compare JSON values exactly, treating object order and decimal spelling as
/// insignificant. Types, array ordering, and string contents remain significant.
pub fn equivalent(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => {
            match (Decimal::parse(left), Decimal::parse(right)) {
                (Some(left), Some(right)) => {
                    left.negative == right.negative
                        && left.coefficient == right.coefficient
                        && left.exponent == right.exponent
                }
                _ => false,
            }
        }
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| equivalent(left, right))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .all(|(key, left)| right.get(key).is_some_and(|right| equivalent(left, right)))
        }
        _ => left == right,
    }
}
