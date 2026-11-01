use crate::models::{self, ScientificDefinition};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, value::RawValue};
use std::collections::BTreeMap;
use uuid::Uuid;

pub const MAX_RESULT_BYTES: usize = 64 * 1024;

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultOutcome {
    Succeeded,
    Failed,
    Cancelled,
}

impl ResultOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Private bounded failure details, never included in diagnostics or tracing.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResultFailure {
    #[cfg_attr(
        feature = "openapi",
        schema(min_length = 1, max_length = 64, pattern = "^[a-z][a-z0-9_]*$")
    )]
    pub code: String,
    /// At most 2048 UTF-8 bytes. Only tab and newline control characters allowed.
    #[cfg_attr(feature = "openapi", schema(min_length = 1, max_length = 2048))]
    pub message: String,
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResultRequest {
    pub attempt_id: Uuid,
    #[serde(deserialize_with = "outcome")]
    pub outcome: ResultOutcome,
    /// Required true: execution has definitely stopped (or never started).
    /// Lost contact or an uncertain submission acknowledgement cannot assert this.
    pub execution_terminated: bool,
    /// Schema-valid output values, including declared file artifact references.
    pub outputs: Value,
    /// Declared file names mapped to {artifact_id}, matching outputs exactly.
    /// Each artifact must be verified, ready and produced by this assignment.
    #[cfg_attr(feature = "openapi", schema(value_type = BTreeMap<String, crate::models::files::ArtifactReference>))]
    pub files: Value,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[cfg_attr(feature = "openapi", schema(value_type = BTreeMap<String, crate::collections::CollectionReference>))]
    pub collections: BTreeMap<String, Value>,
    pub optional_output_failures: BTreeMap<String, ResultFailure>,
    #[serde(default, deserialize_with = "failure")]
    pub failure: Option<ResultFailure>,
}

fn outcome<'de, D: Deserializer<'de>>(deserializer: D) -> Result<ResultOutcome, D::Error> {
    let value = String::deserialize(deserializer)?;
    serde_json::from_value(value.into()).map_err(serde::de::Error::custom)
}

fn failure<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<ResultFailure>, D::Error> {
    let value = Option::<Box<RawValue>>::deserialize(deserializer)?;
    value
        .map(|raw| {
            if !raw.get().starts_with('{') {
                return Err(serde::de::Error::custom("expected failure object"));
            }
            serde_json::from_str(raw.get()).map_err(serde::de::Error::custom)
        })
        .transpose()
}

/// Parse before constructing any Value: duplicate decoded keys and Serde's
/// reserved synthetic markers must not disappear in normalization.
pub fn parse_request(bytes: &[u8]) -> Result<ResultRequest, ResultValidationError> {
    let value = models::parse_inputs(bytes).map_err(|_| ResultValidationError::InvalidRequest)?;
    if !value.is_object() {
        return Err(ResultValidationError::InvalidRequest);
    }
    // Validate nested failure shapes before typed Serde can accept tuple arrays.
    let failures = value
        .get("optional_output_failures")
        .and_then(Value::as_object)
        .ok_or(ResultValidationError::InvalidRequest)?;
    if failures.values().any(|failure| !failure.is_object()) {
        return Err(ResultValidationError::InvalidRequest);
    }
    let request: ResultRequest =
        serde_json::from_slice(bytes).map_err(|_| ResultValidationError::InvalidRequest)?;
    request.validate()?;
    Ok(request)
}

impl ResultRequest {
    pub fn validate(&self) -> Result<(), ResultValidationError> {
        let invalid = || ResultValidationError::InvalidRequest;
        let empty = |value: &Value| value.as_object().is_some_and(|object| object.is_empty());
        if self.attempt_id.is_nil()
            || !self.execution_terminated
            || !self.outputs.is_object()
            || self.files.as_object().is_none_or(|files| files.len() > 64)
            || self.optional_output_failures.len() > 64
            || self.files.as_object().map_or(0, |f| f.len()) + self.collections.len() > 64
            || self
                .collections
                .values()
                .any(|r| crate::collections::reference(r).is_none())
            || (self.outcome == ResultOutcome::Succeeded && self.failure.is_some())
            || (matches!(
                self.outcome,
                ResultOutcome::Failed | ResultOutcome::Cancelled
            ) && (self.failure.is_none()
                || !empty(&self.outputs)
                || !empty(&self.files)
                || !self.collections.is_empty()
                || !self.optional_output_failures.is_empty()))
        {
            return Err(invalid());
        }
        for (name, file) in self.files.as_object().ok_or_else(invalid)? {
            if name.is_empty() || name.len() > 128 || file_reference(file).is_none() {
                return Err(invalid());
            }
        }
        for failure in self
            .failure
            .iter()
            .chain(self.optional_output_failures.values())
        {
            if failure.code.is_empty()
                || failure.code.len() > 64
                || !failure.code.as_bytes()[0].is_ascii_lowercase()
                || !failure
                    .code
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
                || failure.message.is_empty()
                || failure.message.len() > 2048
                || failure
                    .message
                    .chars()
                    .any(|c| c.is_control() && c != '\n' && c != '\t')
            {
                return Err(invalid());
            }
        }
        let value = serde_json::to_value(self).map_err(|_| invalid())?;
        models::validate_input_value(&value).map_err(|_| invalid())
    }
}

/// Original committed acknowledgement, independent of later inspection time.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ResultReceipt {
    pub assignment_id: Uuid,
    pub run_id: Uuid,
    pub attempt_id: Uuid,
    pub outcome: ResultOutcome,
    #[cfg_attr(feature = "openapi", schema(format = DateTime))]
    pub completed_at: String,
    /// True only after this result and every file pin have committed. It permits
    /// removal of provider work files, never deletion of API-retained artifacts.
    /// Historical receipts and operator resolutions carry no cleanup authority.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub provider_cleanup_acknowledged: bool,
}

/// Creator-private result and file manifest; also visible to the owning connector.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Clone, Deserialize, Serialize)]
pub struct RunResult {
    pub receipt: ResultReceipt,
    pub outputs: Value,
    #[cfg_attr(feature = "openapi", schema(value_type = BTreeMap<String, crate::models::files::ArtifactReference>))]
    pub files: Value,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[cfg_attr(feature = "openapi", schema(value_type = BTreeMap<String, crate::collections::CollectionReference>))]
    pub collections: BTreeMap<String, Value>,
    pub optional_output_failures: BTreeMap<String, ResultFailure>,
    pub failure: Option<ResultFailure>,
}

fn file_reference(file: &Value) -> Option<Uuid> {
    if file.as_object()?.len() != 1 {
        return None;
    }
    let text = file.get("artifact_id")?.as_str()?;
    let id = Uuid::parse_str(text).ok()?;
    (!id.is_nil() && id.to_string() == text).then_some(id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultValidationError {
    InvalidRequest,
}
impl std::fmt::Display for ResultValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the connector request is invalid")
    }
}
impl std::error::Error for ResultValidationError {}

pub fn validate_outputs(
    definition: &ScientificDefinition,
    request: &ResultRequest,
) -> Result<Vec<OutputReference>, OutputValidationError> {
    let validator =
        models::compile_outputs(definition).map_err(|_| OutputValidationError::InvalidContract)?;
    if !validator.is_valid(&request.outputs) {
        return Err(OutputValidationError::InvalidOutputs);
    }
    let properties = definition
        .output_schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or(OutputValidationError::InvalidContract)?;
    let required = definition
        .output_schema
        .get("required")
        .and_then(Value::as_array);
    for name in request.optional_output_failures.keys() {
        if !properties.contains_key(name)
            || request.outputs.get(name).is_some()
            || required
                .is_some_and(|required| required.iter().any(|value| value.as_str() == Some(name)))
        {
            return Err(OutputValidationError::InvalidOutputs);
        }
    }
    let declarations = models::files::output_declarations(definition)
        .map_err(|_| OutputValidationError::InvalidContract)?;
    let manifest = request
        .files
        .as_object()
        .ok_or(OutputValidationError::InvalidOutputs)?;
    if manifest.keys().any(|name| !declarations.contains_key(name)) {
        return Err(OutputValidationError::InvalidOutputs);
    }
    let collections = crate::collections::declarations(definition, true)
        .map_err(|_| OutputValidationError::InvalidContract)?;
    if request
        .collections
        .keys()
        .any(|k| !collections.contains_key(k))
    {
        return Err(OutputValidationError::InvalidOutputs);
    }
    for name in collections.keys() {
        match (request.outputs.get(name), request.collections.get(name)) {
            (None, None) => {}
            (Some(o), Some(r)) if o == r && crate::collections::reference(r).is_some() => {}
            _ => return Err(OutputValidationError::InvalidOutputs),
        }
    }
    let mut files = Vec::new();
    for (name, declaration) in declarations {
        match (request.outputs.get(&name), manifest.get(&name)) {
            (None, None) => {}
            (Some(output), Some(file)) if output == file => {
                let artifact_id =
                    file_reference(file).ok_or(OutputValidationError::InvalidOutputs)?;
                files.push(OutputReference {
                    name,
                    artifact_id,
                    declaration,
                });
            }
            _ => return Err(OutputValidationError::InvalidOutputs),
        }
    }
    // Coordinate with availability transitions using the same deterministic
    // shared-artifact lock order as input acceptance.
    files.sort_by_key(|file| file.artifact_id);
    Ok(files)
}

pub struct OutputReference {
    pub name: String,
    pub artifact_id: Uuid,
    pub declaration: models::files::FileDeclaration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputValidationError {
    InvalidContract,
    InvalidOutputs,
}
