//! Scientific contracts and bounded, exact JSON validation.

pub mod contract;
pub mod files;
mod input;
mod input_diagnostics;
mod publication;
pub mod validation;

pub use contract::{ContractError, SchemaKind, compile_outputs};
pub use files::{requires_artifact_inputs, requires_artifact_outputs};
pub use input::validate_value as validate_input_value;
pub use input::{InputError, MAX_INPUT_BYTES, parse_inputs, validate_inputs};
pub use input_diagnostics::{InputRule, InputViolation, ValidationDetails};
pub use publication::{
    ParsedPublication, PublicationExecution, parse_publication, validate_publication,
    validate_publication_definition, validate_publication_protocol,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{error::Error, fmt};

/// Maximum size of an author publication and its normalized scientific definition.
pub const MAX_IMPORT_BYTES: usize = 1024 * 1024;

/// Descriptive catalog information that may change without a new publication.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelMetadata {
    pub id: String,
    pub name: String,
    pub description: String,
}

/// Pinned identities and scientific schemas checked during registration.
///
/// Contract validation checks schemas and examples, not scientific correctness.
/// Registration does not activate a publication or verify the claimed identities.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScientificDefinition {
    /// Enumeray's interpretation of schema restrictions and input/output values.
    /// This is independent of the wrapper protocol and JSON Schema dialect.
    pub contract_version: u32,
    pub model_identity: String,
    pub wrapper_identity: String,
    pub runtime_identity: String,
    pub wrapper_protocol_version: u32,
    pub dataset_identities: Vec<String>,
    pub input_schema: Value,
    pub output_schema: Value,
    pub examples: Vec<ScientificExample>,
    /// Up to eight optional diagnostics, keyed by [a-z][a-z0-9_-]{0,63}.
    /// Paths are confined to the run workspace; diagnostics are independent of results.
    #[serde(
        default,
        skip_serializing_if = "std::collections::BTreeMap::is_empty",
        deserialize_with = "crate::logs::deserialize_log_files"
    )]
    pub log_files: std::collections::BTreeMap<String, crate::logs::LogFileDeclaration>,
}

/// A named example validated at registration and preserved for discovery.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScientificExample {
    pub name: String,
    pub inputs: Value,
    #[serde(skip_serializing_if = "outputs_absent")]
    pub outputs: Option<Value>,
}

fn outputs_absent(outputs: &Option<Value>) -> bool {
    matches!(outputs, None | Some(Value::Null))
}

/// Safe publication failures without parser details, database errors, or input values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelValidationError {
    InvalidActor,
    InvalidDocument,
    InvalidModelId,
    InvalidMetadata,
    InvalidDefinition,
    InvalidContract(ContractError),
    UnsupportedPublicationVersion,
    UnsupportedAuthorFormat,
    InvalidPublicationIds,
    IncompletePublication(&'static str),
    InvalidInstallation(String),
    ArtifactProtocolRequired,
    ImportTooLarge,
    JsonTooDeep,
}

impl fmt::Display for ModelValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidActor => formatter.write_str(
                "operator actor must contain 1 through 200 UTF-8 bytes after trimming and no control characters",
            ),
            Self::InvalidDocument => formatter.write_str(
                "publication must be a JSON document with the documented fields and types",
            ),
            Self::InvalidModelId => formatter.write_str(
                "model ID must start with a lowercase ASCII letter and contain at most 64 ASCII letters, digits, underscores, or hyphens",
            ),
            Self::InvalidMetadata => formatter.write_str(
                "model name must contain 1 through 200 UTF-8 bytes without control characters or outer whitespace; description must contain at most 16384 UTF-8 bytes without NUL",
            ),
            Self::InvalidDefinition => formatter.write_str(
                "scientific definition does not meet the documented structure, value, or storage limits",
            ),
            Self::InvalidContract(error) => fmt::Display::fmt(error, formatter),
            Self::UnsupportedPublicationVersion => formatter.write_str("publication_version must be 1"),
            Self::UnsupportedAuthorFormat => formatter.write_str("split scientific.json/binding.json documents and generated manifest envelopes are unsupported author inputs; edit one publication.json with publication_version, model, identities, execution, input_schema and output_schema, then initialize a release"),
            Self::InvalidPublicationIds => formatter.write_str("publication requires valid publication and context IDs"),
            Self::IncompletePublication(field) => write!(formatter, "set execution.{field} before initializing a release candidate"),
            Self::InvalidInstallation(message) => formatter.write_str(message),
            Self::ArtifactProtocolRequired => formatter.write_str("publications with file inputs or outputs require wrapper protocol 1"),
            Self::ImportTooLarge => formatter.write_str("publication exceeds the 1 MiB limit"),
            Self::JsonTooDeep => formatter.write_str("publication exceeds 64 JSON nesting levels"),

        }
    }
}

impl Error for ModelValidationError {}

/// Structural storage bound; deployment admission policy may be substantially lower.
pub const MAX_ARTIFACT_BYTES: u64 = 10_000 * 5 * 1024 * 1024 * 1024;
pub const MAX_SINGLE_UPLOAD_BYTES: u64 = 5 * 1024 * 1024 * 1024;
