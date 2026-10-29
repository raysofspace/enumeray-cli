use std::{collections::BTreeSet, fmt};

use serde::{
    Deserialize, Deserializer,
    de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Value, value::RawValue};

use super::{MAX_IMPORT_BYTES, ModelMetadata, ModelValidationError};

const MAX_JSON_DEPTH: usize = 64;
const MAX_INTEGER_DIGITS: i64 = 131_072;
const MAX_FRACTIONAL_DIGITS: i64 = 16_383;
const MAX_NUMBER_EXPONENT: i64 = MAX_INTEGER_DIGITS + MAX_FRACTIONAL_DIGITS;

fn reserved_key(key: &str) -> bool {
    matches!(
        key,
        "$serde_json::private::Number" | "$serde_json::private::RawValue"
    )
}

pub(super) fn preflight_json(bytes: &[u8]) -> Result<(), ModelValidationError> {
    preflight(bytes, true)
}

/// Run inputs reject duplicate decoded members before Value can discard them.
pub(crate) fn preflight_inputs(bytes: &[u8]) -> Result<(), ModelValidationError> {
    preflight(bytes, true)
}

fn preflight(bytes: &[u8], reject_duplicate_keys: bool) -> Result<(), ModelValidationError> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let mut violation = None;
    JsonPreflight {
        parent_depth: 0,
        reject_duplicate_keys,
        violation: &mut violation,
    }
    .deserialize(&mut deserializer)
    .map_err(|_| violation.unwrap_or(ModelValidationError::InvalidDocument))?;
    deserializer
        .end()
        .map_err(|_| ModelValidationError::InvalidDocument)
}

// Value interprets certain object keys as Serde's internal number/raw-value
// markers. Check decoded keys before constructing any Value, including keys in
// duplicate object members. RawValue keeps actual numbers out of deserialize_any,
// which would otherwise present arbitrary-precision numbers as synthetic maps.
struct JsonPreflight<'a> {
    parent_depth: usize,
    reject_duplicate_keys: bool,
    violation: &'a mut Option<ModelValidationError>,
}

impl<'de> DeserializeSeed<'de> for JsonPreflight<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<(), D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = <&RawValue>::deserialize(deserializer)?;
        let token = raw.get().as_bytes().first();
        if !matches!(token, Some(b'{' | b'[')) {
            return Ok(());
        }
        let depth = container_depth(self.parent_depth).map_err(|error| {
            *self.violation = Some(error);
            <D::Error as de::Error>::custom("invalid publication")
        })?;
        let mut deserializer = serde_json::Deserializer::from_str(raw.get());
        let visitor = JsonContainer {
            depth,
            reject_duplicate_keys: self.reject_duplicate_keys,
            violation: self.violation,
        };
        match token {
            Some(b'{') => deserializer.deserialize_map(visitor),
            _ => deserializer.deserialize_seq(visitor),
        }
        .map_err(de::Error::custom)
    }
}

struct JsonContainer<'a> {
    depth: usize,
    reject_duplicate_keys: bool,
    violation: &'a mut Option<ModelValidationError>,
}

impl<'de> Visitor<'de> for JsonContainer<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON object or array")
    }

    fn visit_map<A>(self, mut object: A) -> Result<(), A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut seen = BTreeSet::new();
        while let Some(key) = object.next_key::<String>()? {
            if reserved_key(&key) {
                *self.violation = Some(ModelValidationError::InvalidDefinition);
                return Err(de::Error::custom("invalid publication"));
            }
            if self.reject_duplicate_keys && !seen.insert(key) {
                *self.violation = Some(ModelValidationError::InvalidDocument);
                return Err(de::Error::custom("duplicate JSON member"));
            }
            object.next_value_seed(JsonPreflight {
                parent_depth: self.depth,
                reject_duplicate_keys: self.reject_duplicate_keys,
                violation: &mut *self.violation,
            })?;
        }
        Ok(())
    }

    fn visit_seq<A>(self, mut array: A) -> Result<(), A::Error>
    where
        A: SeqAccess<'de>,
    {
        while array
            .next_element_seed(JsonPreflight {
                parent_depth: self.depth,
                reject_duplicate_keys: self.reject_duplicate_keys,
                violation: &mut *self.violation,
            })?
            .is_some()
        {}
        Ok(())
    }
}

pub fn validate_actor(actor: &str) -> Result<&str, ModelValidationError> {
    let normalized = actor.trim();
    if normalized.is_empty() || normalized.len() > 200 || actor.chars().any(char::is_control) {
        return Err(ModelValidationError::InvalidActor);
    }
    Ok(normalized)
}

/// Validate mutable catalogue metadata independently from an immutable publication.
pub fn validate_metadata(model: &ModelMetadata) -> Result<(), ModelValidationError> {
    if !valid_model_id(&model.id) {
        return Err(ModelValidationError::InvalidModelId);
    }
    if !valid_name(&model.name)
        || model.description.len() > 16384
        || model.description.contains('\0')
    {
        return Err(ModelValidationError::InvalidMetadata);
    }
    Ok(())
}

/// Check a scientific definition's identities, nested values and storage bounds.
/// Schema compilation is separate and must precede publication registration writes.
pub fn validate_definition(
    definition: &super::ScientificDefinition,
) -> Result<(), ModelValidationError> {
    if !valid_name(&definition.model_identity)
        || !valid_name(&definition.wrapper_identity)
        || !valid_name(&definition.runtime_identity)
        || definition.wrapper_protocol_version == 0
        || definition.dataset_identities.len() > 100
        || definition
            .dataset_identities
            .iter()
            .any(|id| !valid_name(id))
        || definition.examples.len() > 20
        || crate::logs::validate_log_files(&definition.log_files).is_err()
        || !matches!(definition.input_schema, Value::Object(_) | Value::Bool(_))
        || !matches!(definition.output_schema, Value::Object(_) | Value::Bool(_))
    {
        return Err(ModelValidationError::InvalidDefinition);
    }

    // The containing document and definition occupy two object levels.
    validate_json(&definition.input_schema, 2)?;
    validate_json(&definition.output_schema, 2)?;
    for example in &definition.examples {
        if !valid_name(&example.name) {
            return Err(ModelValidationError::InvalidDefinition);
        }
        // The examples array and this example's object add two more levels.
        validate_json(&example.inputs, 4)?;
        if let Some(outputs) = &example.outputs {
            validate_json(outputs, 4)?;
        }
    }

    let serialized =
        serde_json::to_vec(definition).map_err(|_| ModelValidationError::InvalidDocument)?;
    if serialized.len() > MAX_IMPORT_BYTES {
        return Err(ModelValidationError::ImportTooLarge);
    }
    Ok(())
}

pub fn valid_model_id(value: &str) -> bool {
    let Some((&first, rest)) = value.as_bytes().split_first() else {
        return false;
    };
    value.len() <= 64
        && first.is_ascii_lowercase()
        && rest
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-".contains(byte))
}

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

pub(super) fn validate_json(
    value: &Value,
    parent_depth: usize,
) -> Result<(), ModelValidationError> {
    match value {
        Value::Object(object) => {
            let depth = container_depth(parent_depth)?;
            for (key, value) in object {
                if key.contains('\0') || reserved_key(key) {
                    return Err(ModelValidationError::InvalidDefinition);
                }
                validate_json(value, depth)?;
            }
        }
        Value::Array(array) => {
            let depth = container_depth(parent_depth)?;
            for value in array {
                validate_json(value, depth)?;
            }
        }
        Value::String(value) if value.contains('\0') => {
            return Err(ModelValidationError::InvalidDefinition);
        }
        Value::Number(number) if number_storage_bytes(number).is_none() => {
            return Err(ModelValidationError::InvalidDefinition);
        }
        _ => {}
    }
    Ok(())
}

/// Bound decimal magnitude and scale before PostgreSQL or the schema validator
/// can interpret a number. Trailing fractional zeros count toward the scale.
/// The exponent cap also rejects extreme encodings of otherwise small values.
/// A conservative length for PostgreSQL's expanded decimal representation.
/// Returns `None` when the number exceeds PostgreSQL's magnitude/scale bounds.
pub(super) fn number_storage_bytes(number: &serde_json::Number) -> Option<usize> {
    let text = number.to_string();
    let unsigned = text.strip_prefix('-').unwrap_or(&text);
    let (mantissa, exponent) = match unsigned.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => {
            let exponent = exponent.parse::<i64>().ok()?;
            if !(-MAX_NUMBER_EXPONENT..=MAX_NUMBER_EXPONENT).contains(&exponent) {
                return None;
            }
            (mantissa, exponent)
        }
        None => (unsigned, 0),
    };
    let (integer, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let integer_digits = i64::try_from(integer.len()).ok()?;
    let fractional_digits = i64::try_from(fraction.len()).ok()?;
    let leading_zeros = mantissa
        .bytes()
        .filter(|byte| *byte != b'.')
        .take_while(|byte| *byte == b'0')
        .count();
    let leading_zeros = i64::try_from(leading_zeros).ok()?;
    let integer_places = integer_digits
        .checked_add(exponent)
        .and_then(|position| position.checked_sub(leading_zeros))?;
    let fractional_places = fractional_digits.checked_sub(exponent)?;
    // Negative counts mean the decimal point lies beyond that side of the value.
    if integer_places > MAX_INTEGER_DIGITS || fractional_places > MAX_FRACTIONAL_DIGITS {
        return None;
    }
    let decimal_point = i64::from(fractional_places > 0);
    let sign = i64::from(text.starts_with('-'));
    usize::try_from(integer_places.max(1) + fractional_places.max(0) + decimal_point + sign).ok()
}

fn container_depth(parent_depth: usize) -> Result<usize, ModelValidationError> {
    let depth = parent_depth + 1;
    if depth > MAX_JSON_DEPTH {
        return Err(ModelValidationError::JsonTooDeep);
    }
    Ok(depth)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::models::{ParsedPublication, parse_publication};

    fn document() -> ParsedPublication {
        parse_publication(
            br#"{
  "publication_version": 1,
  "model": {"id": "scalar-double", "name": "Scalar double"},
  "identities": {"model": "scalar-double@1", "wrapper": "test-wrapper@1", "runtime": "test-runtime@1"},
  "execution": {"entrypoint": ["wrapper.py", "execute"]},
  "input_schema": true,
  "output_schema": {"type":"object", "properties":{}, "additionalProperties":false},
  "examples": [{"name": "One value", "inputs": {"value": 2}}]
}"#,
        )
        .unwrap()
    }

    // Structural checks remain independent from schema compilation and installation readiness.
    fn validate_document(document: &ParsedPublication) -> Result<(), ModelValidationError> {
        validate_metadata(&document.model)?;
        validate_definition(&document.definition)?;
        if serde_json::to_vec(document).unwrap().len() > MAX_IMPORT_BYTES {
            return Err(ModelValidationError::ImportTooLarge);
        }
        Ok(())
    }

    #[test]
    fn machine_contract_version_is_explicit_and_unsigned() {
        for version in [
            None,
            Some(json!(null)),
            Some(json!("1")),
            Some(json!(-1)),
            Some(json!(1.5)),
        ] {
            let mut value = serde_json::to_value(document()).unwrap();
            let definition = value["definition"].as_object_mut().unwrap();
            match version {
                Some(version) => {
                    definition.insert("contract_version".to_owned(), version);
                }
                None => {
                    definition.remove("contract_version");
                }
            }
            assert!(
                serde_json::from_value::<crate::models::ScientificDefinition>(
                    value["definition"].clone()
                )
                .is_err()
            );
        }
    }

    #[test]
    fn rejects_unknown_and_missing_fields() {
        let mut value = document().author_document();
        value["extra"] = json!("unknown");
        assert_eq!(
            parse_publication(&serde_json::to_vec(&value).unwrap()).unwrap_err(),
            ModelValidationError::InvalidDocument
        );
        value.as_object_mut().unwrap().remove("extra");
        value["identities"].as_object_mut().unwrap().remove("model");
        assert_eq!(
            parse_publication(&serde_json::to_vec(&value).unwrap()).unwrap_err(),
            ModelValidationError::InvalidDocument
        );
    }

    #[test]
    fn absent_and_null_example_outputs_normalize_to_absence() {
        let mut value = document().author_document();
        value["examples"][0]["outputs"] = Value::Null;
        let parsed = parse_publication(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(parsed.definition.examples[0].outputs.is_none());
        let normalized = serde_json::to_value(parsed).unwrap();
        assert!(
            normalized["definition"]["examples"][0]
                .get("outputs")
                .is_none()
        );

        let mut typed = document();
        typed.definition.examples[0].outputs = Some(Value::Null);
        let normalized = serde_json::to_value(typed).unwrap();
        assert!(
            normalized["definition"]["examples"][0]
                .get("outputs")
                .is_none()
        );
    }

    #[test]
    fn identifiers_and_names_have_explicit_byte_boundaries() {
        assert!(valid_model_id(&format!("a{}", "0".repeat(63))));
        assert!(!valid_model_id(&"a".repeat(65)));
        for value in ["", "Model", "1model", "a.b", "a/b", "a\n", "模型"] {
            assert!(!valid_model_id(value));
        }

        assert!(valid_name(&"é".repeat(100)));
        assert!(!valid_name(&"é".repeat(101)));
        for value in ["", " ", " name", "name\u{2003}", "a\nb", "a\0b"] {
            assert!(!valid_name(value));
        }
        assert_eq!(validate_actor(" operator ").unwrap(), "operator");
        assert_eq!(
            validate_actor("operator\n"),
            Err(ModelValidationError::InvalidActor)
        );
    }

    #[test]
    fn rejects_nul_in_nested_values_and_keys() {
        for value in [
            json!({"nested": ["invalid\0value"]}),
            json!({"invalid\0key": true}),
        ] {
            let mut document = document();
            document.definition.input_schema = value;
            assert_eq!(
                validate_document(&document),
                Err(ModelValidationError::InvalidDefinition)
            );
        }
    }

    #[test]
    fn rejects_reserved_keys_before_raw_json_can_normalize_them() {
        let original = serde_json::to_string(&document().author_document()).unwrap();
        for key in [
            "$serde_json::private::Number",
            "$serde_json::private::RawValue",
        ] {
            for spelling in [
                key.to_owned(),
                key.replace('$', "\\u0024"),
                key.replacen("::", "\\u003a\\u003a", 1),
            ] {
                for replacement in [
                    format!(r#""value":[{{"{spelling}":"1"}}]"#),
                    // A later duplicate must not hide an earlier forbidden key.
                    format!(r#""value":{{"nested":{{"{spelling}":"1"}}}},"value":2"#),
                ] {
                    let raw = original.replace(r#""value":2"#, &replacement);
                    assert_ne!(raw, original);
                    let error = parse_publication(raw.as_bytes()).unwrap_err();
                    assert_eq!(error, ModelValidationError::InvalidDefinition);
                    assert!(!error.to_string().contains(key));
                    assert!(!format!("{error:?}").contains(key));
                }
            }
        }
    }

    #[test]
    fn typed_publications_reject_reserved_keys_in_every_dynamic_field() {
        for key in [
            "$serde_json::private::Number",
            "$serde_json::private::RawValue",
        ] {
            let forbidden = Value::Object(serde_json::Map::from_iter([(
                key.to_owned(),
                Value::String("1".to_owned()),
            )]));
            let nested = Value::Object(serde_json::Map::from_iter([(
                "nested".to_owned(),
                Value::Array(vec![forbidden]),
            )]));
            for field in 0..4 {
                let mut document = document();
                let definition = &mut document.definition;
                match field {
                    0 => definition.input_schema = nested.clone(),
                    1 => definition.output_schema = nested.clone(),
                    2 => definition.examples[0].inputs = nested.clone(),
                    _ => definition.examples[0].outputs = Some(nested.clone()),
                }
                assert_eq!(
                    validate_document(&document),
                    Err(ModelValidationError::InvalidDefinition)
                );
            }
        }
    }

    #[test]
    fn preflight_preserves_actual_numbers_and_marker_text_values() {
        let original = serde_json::to_string(&document().author_document()).unwrap();
        let numbers = r#"[90071992547409921234567890.123456789,1.0,1e100000,-2,true,false,null,"$serde_json::private::Number","$serde_json::private::RawValue"]"#;
        let raw = original.replace(r#""value":2"#, &format!(r#""value":{numbers}"#));
        let parsed = parse_publication(raw.as_bytes()).unwrap();
        let expected: Value = serde_json::from_str(numbers).unwrap();
        assert_eq!(parsed.definition.examples[0].inputs["value"], expected);
    }

    #[test]
    fn decimal_digit_boundaries_include_exponent_placement() {
        for number in [
            "0".to_owned(),
            "-0.0".to_owned(),
            "90071992547409921234567890.123456789".to_owned(),
            "1e131071".to_owned(),
            "-1E+131071".to_owned(),
            "1e-16383".to_owned(),
            "-1e-16383".to_owned(),
            "1".repeat(131_072),
            format!("{}e-1", "1".repeat(131_073)),
            format!("0.{}1e1", "0".repeat(16_383)),
        ] {
            let value: Value = serde_json::from_str(&number).unwrap();
            assert_eq!(validate_json(&value, 0), Ok(()));
        }
        for number in [
            "1e131072".to_owned(),
            "-1e131072".to_owned(),
            "1e-16384".to_owned(),
            "-1e-16384".to_owned(),
            "1".repeat(131_073),
            format!("{}e-1", "1".repeat(131_074)),
            format!("0.{}1e1", "0".repeat(16_384)),
            // Written trailing zeros retain scale even when the value is zero.
            format!("0.{}", "0".repeat(16_384)),
        ] {
            let value: Value = serde_json::from_str(&number).unwrap();
            assert_eq!(
                validate_json(&value, 0),
                Err(ModelValidationError::InvalidDefinition)
            );
        }
    }

    #[test]
    fn extreme_exponents_are_rejected_without_expansion_or_overflow() {
        for number in [
            "0.01e-9223372036854775807",
            "1e9223372036854775807",
            "1e-9223372036854775808",
            "1e9223372036854775808",
            "1e-9223372036854775809",
            "-1e99999999999999999999999999999999999999",
            "-1e-99999999999999999999999999999999999999",
            "0e9223372036854775807",
            "0e-9223372036854775808",
        ] {
            let value: Value = serde_json::from_str(number).unwrap();
            assert_eq!(
                validate_json(&value, 0),
                Err(ModelValidationError::InvalidDefinition)
            );
        }
        // Both values equal one; the second encoding exceeds the exponent cap.
        for (exponent, expected) in [
            (MAX_NUMBER_EXPONENT, Ok(())),
            (
                MAX_NUMBER_EXPONENT + 1,
                Err(ModelValidationError::InvalidDefinition),
            ),
        ] {
            let number = format!("0.{}1e{exponent}", "0".repeat(exponent as usize - 1));
            let value: Value = serde_json::from_str(&number).unwrap();
            assert_eq!(validate_json(&value, 0), expected);
        }
    }

    #[test]
    fn numeric_limits_apply_to_raw_and_typed_publications_in_every_dynamic_field() {
        let invalid: Value = serde_json::from_str("0.01e-9223372036854775807").unwrap();
        for field in 0..4 {
            let mut document = document();
            let definition = &mut document.definition;
            match field {
                0 => definition.input_schema = json!({"default": invalid}),
                1 => definition.output_schema = json!({"default": invalid}),
                2 => definition.examples[0].inputs = json!({"value": invalid}),
                _ => definition.examples[0].outputs = Some(json!({"value": invalid})),
            }
            assert_eq!(
                validate_document(&document),
                Err(ModelValidationError::InvalidDefinition)
            );
            assert_eq!(
                parse_publication(&serde_json::to_vec(&document.author_document()).unwrap())
                    .unwrap_err(),
                ModelValidationError::InvalidDefinition
            );
        }
    }

    #[test]
    fn enforces_total_document_depth_and_size() {
        let mut document = document();
        let mut nested = Value::Null;
        for _ in 0..60 {
            nested = Value::Array(vec![nested]);
        }
        document.definition.examples[0].inputs = nested;
        assert!(validate_document(&document).is_ok());
        assert!(
            parse_publication(&serde_json::to_vec(&document.author_document()).unwrap()).is_ok()
        );
        document.definition.examples[0].inputs =
            Value::Array(vec![document.definition.examples[0].inputs.clone()]);
        assert_eq!(
            validate_document(&document),
            Err(ModelValidationError::JsonTooDeep)
        );
        assert_eq!(
            parse_publication(&serde_json::to_vec(&document.author_document()).unwrap())
                .unwrap_err(),
            ModelValidationError::JsonTooDeep
        );

        assert_eq!(
            parse_publication(&vec![b' '; MAX_IMPORT_BYTES + 1]).unwrap_err(),
            ModelValidationError::ImportTooLarge
        );
        document.definition.examples[0].inputs = Value::String("x".repeat(MAX_IMPORT_BYTES));
        assert_eq!(
            validate_document(&document),
            Err(ModelValidationError::ImportTooLarge)
        );
    }

    #[test]
    fn validates_structure_without_compiling_schemas() {
        let mut document = document();
        document.definition.input_schema = json!({"$ref": "https://example.invalid/schema"});
        document.definition.output_schema = json!({"type": "not-a-json-schema-type"});
        assert!(validate_document(&document).is_ok());
        document.definition.output_schema = json!([]);
        assert_eq!(
            validate_document(&document),
            Err(ModelValidationError::InvalidDefinition)
        );
    }

    #[test]
    fn bounds_protocol_lists_and_descriptive_metadata() {
        let mut document = document();
        document.model.description = "line one\nline two".to_owned();
        document.definition.dataset_identities = vec!["data@1".to_owned(); 100];
        document.definition.examples = vec![document.definition.examples[0].clone(); 20];
        assert!(validate_document(&document).is_ok());
        document
            .definition
            .examples
            .push(document.definition.examples[0].clone());
        assert_eq!(
            validate_document(&document),
            Err(ModelValidationError::InvalidDefinition)
        );
        document.definition.examples.pop();
        document
            .definition
            .dataset_identities
            .push("data@2".to_owned());
        assert_eq!(
            validate_document(&document),
            Err(ModelValidationError::InvalidDefinition)
        );
        document.definition.dataset_identities.pop();
        document.definition.wrapper_protocol_version = 0;
        assert_eq!(
            validate_document(&document),
            Err(ModelValidationError::InvalidDefinition)
        );
        document.definition.wrapper_protocol_version = 1;
        document.model.description = "x".repeat(16385);
        assert_eq!(
            validate_document(&document),
            Err(ModelValidationError::InvalidMetadata)
        );
    }
}
