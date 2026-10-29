//! Exact decimal arithmetic and value comparison for the deterministic fixture.
//! PostgreSQL may change decimal spelling; it must not change recovery identity.

#[cfg(test)]
use serde_json::{Number, Value};

#[cfg(test)]
use enumeray_contracts::numeric::Decimal;
pub use enumeray_contracts::numeric::equivalent;

/// Multiply a JSON decimal by two without a floating-point conversion. Keep
/// exponents compact; the caller enforces the terminal report's storage budget.
#[cfg(test)]
pub fn double(value: &Value) -> Option<Value> {
    let Value::Number(number) = value else {
        return None;
    };
    let decimal = Decimal::parse(number)?;
    let mut carry = 0;
    let mut digits = Vec::with_capacity(decimal.coefficient().len() + 1);
    for digit in decimal.coefficient().bytes().rev() {
        let product = (digit - b'0') * 2 + carry;
        digits.push(b'0' + product % 10);
        carry = product / 10;
    }
    if carry != 0 {
        digits.push(b'0' + carry);
    }
    digits.reverse();
    let coefficient = String::from_utf8(digits).ok()?;
    let sign = if decimal.negative() { "-" } else { "" };
    let text = if decimal.exponent() == 0 {
        format!("{sign}{coefficient}")
    } else {
        format!("{sign}{coefficient}e{}", decimal.exponent())
    };
    text.parse::<Number>().ok().map(Value::Number)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn decimal_equality_survives_postgres_spelling_without_losing_type_or_precision() {
        for (left, right) in [
            ("4e+20", "400000000000000000000"),
            ("-0.000e17", "0"),
            ("1.200", "12e-1"),
            ("9007199254740993.1", "90071992547409931e-1"),
            (r#"{"a":[1,2.0],"b":-0}"#, r#"{"b":0.0,"a":[1e0,2]}"#),
        ] {
            assert!(equivalent(&json(left), &json(right)), "{left} != {right}");
        }
        for (left, right) in [
            ("9007199254740993.1", "9007199254740993.2"),
            ("1e-320", "0"),
            ("-1", "1"),
            ("1", "\"1\""),
            ("[1,2]", "[2,1]"),
            (r#"{"a":1}"#, r#"{"b":1}"#),
        ] {
            assert!(!equivalent(&json(left), &json(right)), "{left} == {right}");
        }
    }

    #[test]
    fn doubles_exact_decimals_beyond_float_precision_and_range() {
        for (input, expected) in [
            ("2", "4"),
            ("5", "10"),
            ("0.3", "0.6"),
            ("-0.0", "0"),
            ("-15.75", "-31.5"),
            ("9007199254740993.1", "18014398509481986.2"),
            ("18446744073709551615", "36893488147419103230"),
            ("1e-1000", "2e-1000"),
            ("9e1000", "18e1000"),
            ("999.99", "1999.98"),
        ] {
            let actual = double(&json(input)).unwrap();
            assert!(
                equivalent(&actual, &json(expected)),
                "{input} doubled to {actual}"
            );
        }
        assert!(double(&Value::Null).is_none());
        assert!(double(&json("\"2\"")).is_none());
    }
}
