//! Bounded JSON values and explicit v1 artifact-reference shapes.

use std::{error::Error, fmt, io};

use serde_json::Value;

use super::{
    ContractError, ModelValidationError, ScientificDefinition, ValidationDetails, contract,
    input_diagnostics, validation,
};

/// Maximum compact JSON bytes, also allowing for expanded decimal numbers.
pub const MAX_INPUT_BYTES: usize = 64 * 1024;

/// Safe categories and guidance that never retain submitted values or raw errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputError {
    InvalidJson,
    InvalidFileReference,
    InvalidCollectionReference,
    TooLarge,
    TooDeep,
    UnsupportedValue,
    InvalidContract(ContractError),
    SchemaMismatch(Box<ValidationDetails>),
}

impl fmt::Display for InputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCollectionReference=>formatter.write_str("declared collections require a collection_id containing a canonical lowercase UUID"),
            Self::InvalidFileReference => formatter.write_str(
                "declared files require an artifact_id containing a canonical lowercase UUID",
            ),
            Self::InvalidJson => formatter.write_str("inputs must be valid JSON"),
            Self::TooLarge => {
                formatter.write_str("inputs exceed the 64 KiB JSON or decimal storage budget")
            }
            Self::TooDeep => formatter.write_str("inputs exceed 64 levels of JSON nesting"),
            Self::UnsupportedValue => {
                formatter.write_str("inputs contain a value unsupported by JSON storage")
            }
            Self::InvalidContract(_) => {
                formatter.write_str("scientific input contract is invalid or unsupported")
            }
            Self::SchemaMismatch(_) => {
                formatter.write_str("inputs do not satisfy the scientific input schema")
            }
        }
    }
}

impl Error for InputError {}

/// Parse an input value without applying defaults or interpreting file references.
///
/// Use this at raw JSON boundaries: Serde's reserved object keys must be rejected
/// before deserializing a `Value` can normalize them into a number. Acceptance
/// subsequently validates this value against the requested publication.
pub fn parse_inputs(bytes: &[u8]) -> Result<Value, InputError> {
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(InputError::TooLarge);
    }
    validation::preflight_inputs(bytes).map_err(storage_error)?;
    let inputs = serde_json::from_slice(bytes).map_err(|_| InputError::InvalidJson)?;
    validate_value(&inputs)?;
    Ok(inputs)
}

/// Validate inputs against a previously registered, immutable scientific definition.
///
/// This repeats input storage checks even for directly constructed `Value`s.
/// Contract version 1 checks declared file reference shapes; undeclared reference
/// objects and defaults remain ordinary JSON. Ownership and readiness are checked
/// atomically during acceptance. Supplied values are never changed. Run this before
/// opening the acceptance transaction.
pub fn validate_inputs(
    definition: &ScientificDefinition,
    inputs: &Value,
) -> Result<(), InputError> {
    validate_value(inputs)?;
    let validator = contract::compile_inputs(definition).map_err(InputError::InvalidContract)?;
    if let Some(details) = input_diagnostics::describe(
        &definition.input_schema,
        inputs,
        validator.iter_errors(inputs),
    ) {
        return Err(InputError::SchemaMismatch(Box::new(details)));
    }
    super::files::references(definition, inputs)?;
    crate::collections::references(definition, inputs)?;
    Ok(())
}

pub fn validate_value(inputs: &Value) -> Result<(), InputError> {
    // Check depth before recursive serialization, and numeric bounds before a
    // schema compiler or PostgreSQL could expand an extreme exponent.
    validation::validate_json(inputs, 0).map_err(storage_error)?;
    let mut counter = InputBytes { written: 0 };
    serde_json::to_writer(&mut counter, inputs).map_err(|_| InputError::TooLarge)?;
    let mut remaining = MAX_INPUT_BYTES - counter.written;
    reserve_decimal_expansion(inputs, &mut remaining)
}

fn storage_error(error: ModelValidationError) -> InputError {
    match error {
        ModelValidationError::JsonTooDeep => InputError::TooDeep,
        ModelValidationError::InvalidDocument => InputError::InvalidJson,
        _ => InputError::UnsupportedValue,
    }
}

// Counting into a bounded writer avoids allocating a second potentially large
// JSON document when callers construct input values directly.
struct InputBytes {
    written: usize,
}

impl io::Write for InputBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_INPUT_BYTES - self.written {
            return Err(io::Error::other("input byte limit exceeded"));
        }
        self.written += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// A short exponent representation can become a large stored decimal. Budget
// each number at the larger of its original and expanded representations; this
// keeps arrays of individually valid numbers bounded as well.
fn reserve_decimal_expansion(value: &Value, remaining: &mut usize) -> Result<(), InputError> {
    match value {
        Value::Number(number) => {
            let expanded =
                validation::number_storage_bytes(number).ok_or(InputError::UnsupportedValue)?;
            let extra = expanded.saturating_sub(number.to_string().len());
            *remaining = remaining.checked_sub(extra).ok_or(InputError::TooLarge)?;
        }
        Value::Array(values) => {
            for value in values {
                reserve_decimal_expansion(value, remaining)?;
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                reserve_decimal_expansion(value, remaining)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use serde_json::json;

    use super::*;
    use crate::models::InputRule;

    fn definition() -> ScientificDefinition {
        super::super::parse_publication(include_bytes!("../../../../examples/scalar-double.json"))
            .unwrap()
            .definition
    }

    fn details(definition: &ScientificDefinition, inputs: &Value) -> ValidationDetails {
        match validate_inputs(definition, inputs) {
            Err(InputError::SchemaMismatch(details)) => *details,
            result => panic!("expected schema mismatch, got {result:?}"),
        }
    }

    #[test]
    fn raw_inputs_reject_duplicate_decoded_members_at_every_depth() {
        for input in [
            r#"{"value":1,"value":2}"#,
            r#"{"nested":[{"x":1,"\u0078":2}]}"#,
            r#"{"x":{"a":0,"a":0}}"#,
        ] {
            assert_eq!(parse_inputs(input.as_bytes()), Err(InputError::InvalidJson));
        }
    }

    #[test]
    fn validates_required_fields_types_ranges_and_extra_fields() {
        let mut definition = definition();
        definition.input_schema["properties"]["value"]["minimum"] = json!(0);
        definition.input_schema["properties"]["value"]["maximum"] = json!(10);
        assert_eq!(validate_inputs(&definition, &json!({"value": 0})), Ok(()));
        assert_eq!(validate_inputs(&definition, &json!({"value": 10})), Ok(()));
        for inputs in [
            json!({}),
            json!({"value": "2"}),
            json!({"value": null}),
            json!({"value": -1}),
            json!({"value": 11}),
            json!({"value": 2, "extra": 1}),
            json!([2]),
        ] {
            assert!(matches!(
                validate_inputs(&definition, &inputs),
                Err(InputError::SchemaMismatch(_))
            ));
        }
    }

    #[test]
    fn validates_nested_arrays_through_document_local_references() {
        let mut definition = definition();
        definition.input_schema = json!({
            "$defs": {"sample": {"type": "integer", "minimum": 1}},
            "type": "object",
            "properties": {
                "samples": {
                    "type": "array", "minItems": 1,
                    "items": {"type": "array", "items": {"$ref": "#/$defs/sample"}}
                }
            },
            "required": ["samples"], "additionalProperties": false
        });
        assert_eq!(
            validate_inputs(&definition, &json!({"samples": [[1, 2], [3]]})),
            Ok(())
        );
        for inputs in [
            json!({"samples": []}),
            json!({"samples": [[0]]}),
            json!({"samples": [["1"]]}),
        ] {
            assert!(matches!(
                validate_inputs(&definition, &inputs),
                Err(InputError::SchemaMismatch(_))
            ));
        }
    }

    #[test]
    fn defaults_and_file_shaped_values_remain_ordinary_json() {
        let mut definition = definition();
        definition.input_schema["properties"]["value"]["default"] = json!(2);
        assert!(matches!(
            validate_inputs(&definition, &json!({})),
            Err(InputError::SchemaMismatch(_))
        ));
        definition.input_schema = json!({
            "type": "object",
            "properties": {
                "config": {"type": "object", "x-example-file": true},
                "optional": {"type": "integer", "default": 5}
            },
            "required": ["config"]
        });
        let inputs = json!({"config": {"artifact_id": "ordinary-text", "attachment": "also-text"}});
        let original = inputs.clone();
        assert_eq!(validate_inputs(&definition, &inputs), Ok(()));
        assert_eq!(inputs, original);
    }

    #[test]
    fn uses_only_the_registered_offline_contract_profile() {
        let mut definition = definition();
        for reference in ["https://example.invalid/private", "file:///tmp/private"] {
            definition.input_schema = json!({"$ref": reference});
            assert!(matches!(
                validate_inputs(&definition, &json!({})),
                Err(InputError::InvalidContract(_))
            ));
        }
        definition.input_schema = json!(true);
        definition.contract_version = 2;
        assert_eq!(
            validate_inputs(&definition, &json!({})),
            Err(InputError::InvalidContract(
                ContractError::UnsupportedContractVersion
            ))
        );
    }

    #[test]
    fn validates_exact_arbitrary_precision_numbers() {
        let mut definition = definition();
        let exact = parse_inputs(br#"90071992547409921234567890.123456789"#).unwrap();
        definition.input_schema = json!({"const": exact});
        assert_eq!(validate_inputs(&definition, &exact), Ok(()));
        let different = parse_inputs(br#"90071992547409921234567890.123456788"#).unwrap();
        assert!(matches!(
            validate_inputs(&definition, &different),
            Err(InputError::SchemaMismatch(_))
        ));
        assert_eq!(exact.to_string(), "90071992547409921234567890.123456789");
    }

    #[test]
    fn rejects_unsafe_storage_values_before_schema_validation() {
        let mut definition = definition();
        definition.input_schema = json!(true);
        for value in [
            json!({"nested": ["private\0value"]}),
            json!({"private\0key": true}),
            serde_json::from_str("1e131072").unwrap(),
            serde_json::from_str("1e-16384").unwrap(),
            serde_json::from_str("1e999999999999999999999999999999999").unwrap(),
        ] {
            assert_eq!(
                validate_inputs(&definition, &value),
                Err(InputError::UnsupportedValue)
            );
        }
    }

    #[test]
    fn rejects_reserved_keys_in_raw_json_and_directly_constructed_values() {
        let mut definition = definition();
        definition.input_schema = json!(true);
        for key in [
            "$serde_json::private::Number",
            "$serde_json::private::RawValue",
        ] {
            let value = Value::Object(serde_json::Map::from_iter([(key.to_owned(), json!("1"))]));
            assert_eq!(
                validate_inputs(&definition, &value),
                Err(InputError::UnsupportedValue)
            );
            for key in [key.to_owned(), key.replace('$', "\\u0024")] {
                let bytes = format!(r#"{{"x":{{"{key}":"1"}},"x":2}}"#);
                assert_eq!(
                    parse_inputs(bytes.as_bytes()),
                    Err(InputError::UnsupportedValue)
                );
            }
        }
    }

    #[test]
    fn enforces_raw_and_normalized_size_and_depth_limits() {
        let mut definition = definition();
        definition.input_schema = json!(true);
        let maximum = Value::String("x".repeat(MAX_INPUT_BYTES - 2));
        assert_eq!(validate_inputs(&definition, &maximum), Ok(()));
        assert_eq!(
            parse_inputs(&serde_json::to_vec(&maximum).unwrap()),
            Ok(maximum)
        );
        let oversized = Value::String("x".repeat(MAX_INPUT_BYTES - 1));
        assert_eq!(
            validate_inputs(&definition, &oversized),
            Err(InputError::TooLarge)
        );
        assert_eq!(
            parse_inputs(&vec![b' '; MAX_INPUT_BYTES + 1]),
            Err(InputError::TooLarge)
        );

        let mut nested = Value::Null;
        for _ in 0..64 {
            nested = Value::Array(vec![nested]);
        }
        assert_eq!(validate_inputs(&definition, &nested), Ok(()));
        assert_eq!(
            parse_inputs(&serde_json::to_vec(&nested).unwrap()),
            Ok(nested.clone())
        );
        nested = Value::Array(vec![nested]);
        assert_eq!(
            validate_inputs(&definition, &nested),
            Err(InputError::TooDeep)
        );
        assert_eq!(
            parse_inputs(&serde_json::to_vec(&nested).unwrap()),
            Err(InputError::TooDeep)
        );
    }

    #[test]
    fn budgets_numeric_expansion_across_the_complete_input() {
        assert!(parse_inputs(b"1e65535").is_ok());
        assert_eq!(parse_inputs(b"1e65536"), Err(InputError::TooLarge));
        assert_eq!(
            parse_inputs(b"[1e40000,1e40000]"),
            Err(InputError::TooLarge)
        );
        assert!(parse_inputs(b"[1e1000,1e-1000]").is_ok());
    }

    #[test]
    fn malformed_json_and_validation_errors_do_not_expose_input_values() {
        assert_eq!(
            parse_inputs(br#"{"private": "secret""#),
            Err(InputError::InvalidJson)
        );
        let error = validate_inputs(&definition(), &json!({"value": "sensitive-private-value"}))
            .unwrap_err();
        for rendered in [error.to_string(), format!("{error:?}")] {
            assert!(!rendered.contains("sensitive-private-value"));
        }
    }

    #[test]
    fn schema_guidance_identifies_declared_fields_without_echoing_values_or_bounds() {
        let mut definition = definition();
        definition.input_schema["properties"]["value"]["minimum"] = json!(987654);
        for (inputs, position, rule, redacted) in [
            (json!({}), "/value", InputRule::Required, false),
            (
                json!({"value": "private-input"}),
                "/value",
                InputRule::Type,
                false,
            ),
            (json!({"value": 12}), "/value", InputRule::Minimum, false),
            (
                json!({"value": 987655, "private-key": "private-input"}),
                "",
                InputRule::AdditionalProperties,
                true,
            ),
        ] {
            let details = details(&definition, &inputs);
            assert_eq!(details.violations.len(), 1);
            assert!(!details.truncated);
            let violation = &details.violations[0];
            assert_eq!(violation.position, position);
            assert_eq!(violation.position_redacted, redacted);
            assert_eq!(violation.rule, rule);
            let encoded = serde_json::to_string(&details).unwrap();
            for private in ["private-input", "private-key", "987654", "987655"] {
                assert!(!encoded.contains(private));
            }
        }
        assert_eq!(
            details(&definition, &json!({"value": "private-input"})).violations[0].correction,
            "Use a number."
        );
    }

    #[test]
    fn paths_follow_declared_nested_fields_array_items_and_tuple_positions() {
        let mut definition = definition();
        definition.input_schema = json!({
            "type": "object",
            "properties": {
                "rows": {"type": "array", "items": {
                    "type": "object", "properties": {"value": {"type": "number"}},
                    "required": ["value"]
                }},
                "tuple": {"type": "array", "prefixItems": [
                    {"type": "boolean"}, {"type": "integer"}
                ], "items": {"type": "number"}}
            }
        });
        for (inputs, expected) in [
            (json!({"rows": [{}]}), "/rows/0/value"),
            (
                json!({"rows": [{"value": "private-input"}]}),
                "/rows/0/value",
            ),
            (json!({"tuple": [true, "private-input"]}), "/tuple/1"),
            (json!({"tuple": [true, 1, "private-input"]}), "/tuple/2"),
        ] {
            let details = details(&definition, &inputs);
            assert_eq!(details.violations[0].position, expected);
            assert!(!details.violations[0].position_redacted);
        }
    }

    #[test]
    fn dynamic_object_keys_are_never_copied_into_guidance() {
        let mut definition = definition();
        for (case, map_schema) in [
            json!({"additionalProperties": {"type": "number"}}),
            json!({"patternProperties": {"^private": {"type": "number"}}}),
            json!({"propertyNames": {"pattern": "^allowed$"}}),
            json!({"additionalProperties": false}),
            json!({"unevaluatedProperties": false}),
        ]
        .into_iter()
        .enumerate()
        {
            definition.input_schema = json!({"properties": {"samples": map_schema}});
            let inputs = json!({"samples": {"private-user-key": "private-input"}});
            let details = details(&definition, &inputs);
            assert!(
                details.violations.iter().all(|violation| {
                    violation.position == "/samples" && violation.position_redacted
                }),
                "case {case}: {details:?}"
            );
            for rendered in [
                serde_json::to_string(&details).unwrap(),
                format!("{details:?}"),
            ] {
                assert!(!rendered.contains("private-user-key"));
                assert!(!rendered.contains("private-input"));
                assert!(!rendered.contains("^allowed$"));
            }
        }
    }

    #[test]
    fn unusual_declared_names_and_pointer_escaping_are_redacted_at_the_parent() {
        let mut definition = definition();
        for name in [
            String::new(),
            "private/name".into(),
            "private~name".into(),
            "private\nname".into(),
            "private\"name".into(),
            "privaté".into(),
            "123".into(),
            "x".repeat(65),
        ] {
            definition.input_schema = json!({"properties": {"config": {
                "properties": {name.clone(): {"type": "number"}}, "required": [name.clone()]
            }}});
            for inputs in [
                json!({"config": {name.clone(): "private-input"}}),
                json!({"config": {}}),
            ] {
                let details = details(&definition, &inputs);
                assert_eq!(details.violations[0].position, "/config");
                assert!(details.violations[0].position_redacted);
                assert!(
                    !serde_json::to_string(&details)
                        .unwrap()
                        .contains("private-input")
                );
            }
        }
    }

    #[test]
    fn an_empty_unknown_key_cannot_be_mistaken_for_a_declared_path() {
        let mut definition = definition();
        definition.input_schema = json!({
            "properties": {"value": {"type": "number"}},
            "additionalProperties": {"properties": {"value": {"type": "number"}}}
        });
        let details = details(&definition, &json!({"": {"value": "private-input"}}));
        assert_eq!(details.violations[0].position, "");
        assert!(details.violations[0].position_redacted);
    }

    #[test]
    fn references_and_compositions_fall_back_to_the_nearest_proven_position() {
        let mut definition = definition();
        let child = json!({"properties": {"value": {"type": "number"}}, "required": ["value"]});
        definition.input_schema = json!({
            "$defs": {"child": child}, "properties": {"config": {"$ref": "#/$defs/child"}}
        });
        for inputs in [
            json!({"config": {"value": "private-input"}}),
            json!({"config": {}}),
        ] {
            let details = details(&definition, &inputs);
            assert_eq!(details.violations[0].position, "/config");
            assert!(details.violations[0].position_redacted);
        }
        definition.input_schema = json!({"allOf": [child]});
        let details = details(&definition, &json!({"value": "private-input"}));
        assert_eq!(details.violations[0].position, "");
        assert!(details.violations[0].position_redacted);
    }

    #[test]
    fn undeclared_required_names_and_recursive_refs_do_not_expand_positions() {
        let mut definition = definition();
        definition.input_schema = json!({"required": ["private-required-field"]});
        let missing = details(&definition, &json!({}));
        assert_eq!(missing.violations[0].position, "");
        assert!(missing.violations[0].position_redacted);
        assert!(
            !serde_json::to_string(&missing)
                .unwrap()
                .contains("private-required-field")
        );

        definition.input_schema = json!({
            "$defs": {"node": {"properties": {
                "next": {"$ref": "#/$defs/node"}, "value": {"type": "number"}
            }}},
            "properties": {"root": {"$ref": "#/$defs/node"}}
        });
        let recursive = details(
            &definition,
            &json!({"root": {"next": {"next": {"value": "private-input"}}}}),
        );
        assert_eq!(recursive.violations[0].position, "/root");
        assert!(recursive.violations[0].position_redacted);
    }

    #[test]
    fn schema_literals_and_nested_validator_causes_never_enter_guidance() {
        let mut definition = definition();
        for schema in [
            json!({"const": {"properties": {"private-schema-name": "private-schema-value"}}}),
            json!({"enum": ["private-schema-value"]}),
            json!({"pattern": "^private-schema-value$"}),
            json!({"anyOf": [{"const": "private-schema-value"}, {"type": "number"}]}),
            json!({"oneOf": [{"const": "private-schema-value"}, {"type": "number"}]}),
            json!({"not": {"const": "private-input"}}),
        ] {
            definition.input_schema = schema;
            let details = details(&definition, &json!("private-input"));
            for rendered in [
                serde_json::to_string(&details).unwrap(),
                format!("{details:?}"),
            ] {
                for private in [
                    "private-schema-name",
                    "private-schema-value",
                    "private-input",
                ] {
                    assert!(!rendered.contains(private));
                }
            }
        }
    }

    #[test]
    fn guidance_has_finite_entry_string_and_serialized_byte_limits() {
        let mut definition = definition();
        let first = "a".repeat(60);
        let second = "b".repeat(60);
        definition.input_schema = json!({"properties": {first.clone(): {
            "properties": {second.clone(): {"items": {
                "properties": {"longer_name": {"type": "number"}}
            }}}
        }}});
        let inputs = json!({first: {second: vec![json!({"longer_name": "private-input"}); 40]}});
        let expected = details(&definition, &inputs);
        assert_eq!(expected.violations.len(), 8);
        assert!(expected.truncated);
        assert!(serde_json::to_vec(&expected).unwrap().len() <= 4096);
        for violation in &expected.violations {
            assert!(violation.position.len() <= 128);
            assert!(violation.position_redacted);
            assert!(violation.correction.len() <= 160);
            assert!(serde_json::to_string(&violation.rule).unwrap().len() - 2 <= 32);
            // Ancestor paths remain valid pointers rather than cut string prefixes.
            assert!(inputs.pointer(&violation.position).is_some());
        }
        assert_eq!(details(&definition, &inputs), expected);
        let validator = contract::compile_inputs(&definition).unwrap();
        let observed = Cell::new(0);
        let errors = validator
            .iter_errors(&inputs)
            .inspect(|_| observed.set(observed.get() + 1));
        assert_eq!(
            input_diagnostics::describe(&definition.input_schema, &inputs, errors),
            Some(expected)
        );
        assert_eq!(observed.get(), 9);
    }
}
