//! Bounded client guidance derived without formatting validator errors.

use jsonschema::{
    JsonType, ValidationError,
    error::{TypeKind, ValidationErrorKind},
};
use serde::Serialize;
use serde_json::Value;

const MAX_VIOLATIONS: usize = 8;
const MAX_POSITION_BYTES: usize = 128;
const MAX_PROPERTY_BYTES: usize = 64;
const MAX_DETAILS_BYTES: usize = 4096;

/// Safe, bounded guidance for inputs that do not satisfy a publication schema.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValidationDetails {
    /// At most eight violations, in validator traversal order.
    #[cfg_attr(feature = "openapi", schema(max_items = 8))]
    pub violations: Vec<InputViolation>,
    /// More violations exist than fit in this response.
    pub truncated: bool,
}

/// A schema failure without submitted values or arbitrary property names.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InputViolation {
    /// JSON Pointer relative to `inputs`; the empty string denotes its root.
    /// Only ordinary, explicitly declared property names and array indices appear.
    #[cfg_attr(feature = "openapi", schema(max_length = 128, example = "/value"))]
    pub position: String,
    /// The position stops at a safe ancestor instead of naming the exact field.
    pub position_redacted: bool,
    pub rule: InputRule,
    /// Fixed guidance; actual bounds and allowed values remain in the publication schema.
    #[cfg_attr(feature = "openapi", schema(max_length = 160))]
    pub correction: &'static str,
}

/// Stable categories independent of the validator's free-form diagnostics.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum InputRule {
    Type,
    Required,
    AdditionalProperties,
    PropertyNames,
    UnevaluatedProperties,
    Minimum,
    Maximum,
    ExclusiveMinimum,
    ExclusiveMaximum,
    MultipleOf,
    MinLength,
    MaxLength,
    Pattern,
    Format,
    Enum,
    Const,
    MinItems,
    MaxItems,
    UniqueItems,
    AdditionalItems,
    UnevaluatedItems,
    Contains,
    MinProperties,
    MaxProperties,
    AnyOf,
    OneOf,
    Not,
    ContentEncoding,
    ContentMediaType,
    Schema,
}

/// Inspect at most nine errors: eight reported entries and one lookahead.
/// Nested validator causes are deliberately never traversed or retained.
pub(super) fn describe<'a>(
    schema: &Value,
    inputs: &Value,
    errors: impl Iterator<Item = ValidationError<'a>>,
) -> Option<ValidationDetails> {
    let mut details = ValidationDetails {
        violations: Vec::new(),
        truncated: false,
    };
    let mut saw_error = false;
    // Include the envelope, comma separators, and JSON escaping in the budget.
    // `false` is longer than `true`, so this also bounds a truncated response.
    let mut bytes = br#"{"violations":[],"truncated":false}"#.len();
    for error in errors.take(MAX_VIOLATIONS + 1) {
        saw_error = true;
        if details.violations.len() == MAX_VIOLATIONS {
            details.truncated = true;
            break;
        }
        let (position, position_redacted) = safe_position(schema, inputs, &error);
        let (rule, correction) = guidance(error.kind());
        let violation = InputViolation {
            position,
            position_redacted,
            rule,
            correction,
        };
        let size = serde_json::to_vec(&violation)
            .map_or(MAX_DETAILS_BYTES, |encoded| encoded.len())
            + usize::from(!details.violations.is_empty());
        if size > MAX_DETAILS_BYTES - bytes {
            details.truncated = true;
            break;
        }
        bytes += size;
        details.violations.push(violation);
    }
    saw_error.then_some(details)
}

fn safe_position(schema: &Value, inputs: &Value, error: &ValidationError<'_>) -> (String, bool) {
    let mut position = String::new();
    let mut schema = schema;
    let mut instance = inputs;
    // Preserve empty segments. Some location helpers omit them, which could
    // mistake an unknown empty property name for a different declared path.
    for segment in error.instance_path().as_str().split('/').skip(1) {
        if !simple_location(schema) {
            return (position, true);
        }
        let next = match instance {
            Value::Object(properties) if ordinary_property(segment) => schema
                .get("properties")
                .and_then(|properties| properties.get(segment))
                .zip(properties.get(segment)),
            Value::Array(items) => segment.parse::<usize>().ok().and_then(|index| {
                let item_schema = schema
                    .get("prefixItems")
                    .and_then(|items| items.get(index))
                    .or_else(|| schema.get("items"));
                item_schema.zip(items.get(index))
            }),
            _ => None,
        };
        let Some((next_schema, next_instance)) = next else {
            return (position, true);
        };
        if !append_segment(&mut position, segment) {
            return (position, true);
        }
        schema = next_schema;
        instance = next_instance;
    }
    if let ValidationErrorKind::Required { property } = error.kind() {
        let Some(property) = property.as_str().filter(|property| {
            simple_location(schema)
                && ordinary_property(property)
                && schema
                    .get("properties")
                    .is_some_and(|properties| properties.get(*property).is_some())
        }) else {
            return (position, true);
        };
        if !append_segment(&mut position, property) {
            return (position, true);
        }
    }
    // The validator may optimize a blanket property prohibition to FalseSchema
    // at the object itself. Its omitted names need the same redaction marker.
    let omitted_property = matches!(
        error.kind(),
        ValidationErrorKind::AdditionalProperties { .. }
            | ValidationErrorKind::PropertyNames { .. }
            | ValidationErrorKind::UnevaluatedProperties { .. }
    ) || (matches!(error.kind(), ValidationErrorKind::FalseSchema)
        && [
            "additionalProperties",
            "unevaluatedProperties",
            "propertyNames",
        ]
        .iter()
        .any(|keyword| schema.get(*keyword) == Some(&Value::Bool(false))));
    (position, omitted_property)
}

/// Deliberately avoid interpreting references and applicators just to display a
/// path. Validation still uses the complete schema through `jsonschema`.
fn simple_location(schema: &Value) -> bool {
    ![
        "$ref",
        "$dynamicRef",
        "allOf",
        "anyOf",
        "oneOf",
        "not",
        "if",
        "then",
        "else",
        "dependentSchemas",
    ]
    .iter()
    .any(|keyword| schema.get(*keyword).is_some())
}

fn ordinary_property(property: &str) -> bool {
    !property.is_empty()
        && property.len() <= MAX_PROPERTY_BYTES
        && property
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        && property
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn append_segment(position: &mut String, segment: &str) -> bool {
    if position.len() + 1 + segment.len() > MAX_POSITION_BYTES {
        return false;
    }
    // Ordinary names exclude '/' and '~'; numeric array indices need no escaping.
    position.push('/');
    position.push_str(segment);
    true
}

fn guidance(kind: &ValidationErrorKind) -> (InputRule, &'static str) {
    use InputRule as Rule;
    use ValidationErrorKind as Error;
    match kind {
        Error::Type { kind } => (Rule::Type, type_correction(kind)),
        Error::Required { .. } => (
            Rule::Required,
            "Provide the required field declared in the publication schema.",
        ),
        Error::AdditionalProperties { .. } => (
            Rule::AdditionalProperties,
            "Remove fields that are not allowed by the publication schema.",
        ),
        Error::PropertyNames { .. } => (
            Rule::PropertyNames,
            "Use property names allowed by the publication schema.",
        ),
        Error::UnevaluatedProperties { .. } => (
            Rule::UnevaluatedProperties,
            "Remove fields that are not allowed by the publication schema.",
        ),
        Error::Minimum { .. } => (
            Rule::Minimum,
            "Use a number at or above the lower bound in the publication schema.",
        ),
        Error::Maximum { .. } => (
            Rule::Maximum,
            "Use a number at or below the upper bound in the publication schema.",
        ),
        Error::ExclusiveMinimum { .. } => (
            Rule::ExclusiveMinimum,
            "Use a number above the exclusive lower bound in the publication schema.",
        ),
        Error::ExclusiveMaximum { .. } => (
            Rule::ExclusiveMaximum,
            "Use a number below the exclusive upper bound in the publication schema.",
        ),
        Error::MultipleOf { .. } => (
            Rule::MultipleOf,
            "Use a multiple of the number specified in the publication schema.",
        ),
        Error::MinLength { .. } => (
            Rule::MinLength,
            "Use a string meeting the minimum length in the publication schema.",
        ),
        Error::MaxLength { .. } => (
            Rule::MaxLength,
            "Use a string within the maximum length in the publication schema.",
        ),
        Error::Pattern { .. } => (
            Rule::Pattern,
            "Use a string matching the pattern in the publication schema.",
        ),
        Error::Format { .. } => (
            Rule::Format,
            "Use the string format specified in the publication schema.",
        ),
        Error::Enum { .. } => (
            Rule::Enum,
            "Use one of the allowed values in the publication schema.",
        ),
        Error::Constant { .. } => (
            Rule::Const,
            "Use the constant value specified in the publication schema.",
        ),
        Error::MinItems { .. } => (
            Rule::MinItems,
            "Add items to meet the minimum array length in the publication schema.",
        ),
        Error::MaxItems { .. } => (
            Rule::MaxItems,
            "Remove items to meet the maximum array length in the publication schema.",
        ),
        Error::UniqueItems => (Rule::UniqueItems, "Remove duplicate array items."),
        Error::AdditionalItems { .. } => (
            Rule::AdditionalItems,
            "Remove array items not allowed by the publication schema.",
        ),
        Error::UnevaluatedItems { .. } => (
            Rule::UnevaluatedItems,
            "Remove array items not allowed by the publication schema.",
        ),
        Error::Contains => (
            Rule::Contains,
            "Include array items meeting the contains rule in the publication schema.",
        ),
        Error::MinProperties { .. } => (
            Rule::MinProperties,
            "Add fields to meet the minimum property count in the publication schema.",
        ),
        Error::MaxProperties { .. } => (
            Rule::MaxProperties,
            "Remove fields to meet the maximum property count in the publication schema.",
        ),
        Error::AnyOf { .. } => (
            Rule::AnyOf,
            "Satisfy at least one alternative in the publication schema.",
        ),
        Error::OneOfMultipleValid { .. } | Error::OneOfNotValid { .. } => (
            Rule::OneOf,
            "Satisfy exactly one alternative in the publication schema.",
        ),
        Error::Not { .. } => (Rule::Not, "Use a value outside the excluded schema."),
        Error::ContentEncoding { .. } | Error::FromUtf8 { .. } => (
            Rule::ContentEncoding,
            "Use the content encoding specified in the publication schema.",
        ),
        Error::ContentMediaType { .. } => (
            Rule::ContentMediaType,
            "Use the content media type specified in the publication schema.",
        ),
        Error::FalseSchema
        | Error::Custom { .. }
        | Error::Referencing(_)
        | Error::BacktrackLimitExceeded { .. }
        | Error::RegexEngineFailure { .. } => (
            Rule::Schema,
            "Check this input against the publication schema.",
        ),
    }
}

fn type_correction(kind: &TypeKind) -> &'static str {
    match kind {
        TypeKind::Single(JsonType::Number) => "Use a number.",
        TypeKind::Single(JsonType::Integer) => "Use an integer.",
        TypeKind::Single(JsonType::String) => "Use a string.",
        TypeKind::Single(JsonType::Boolean) => "Use a boolean.",
        TypeKind::Single(JsonType::Null) => "Use null.",
        TypeKind::Single(JsonType::Array) => "Use an array.",
        TypeKind::Single(JsonType::Object) => "Use an object.",
        TypeKind::Multiple(_) => "Use one of the types allowed by the publication schema.",
    }
}
