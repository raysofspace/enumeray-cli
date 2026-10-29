use crate::runs::RunPhase;
use serde::{Deserialize, Deserializer, Serialize, de::DeserializeOwned};
use serde_json::value::RawValue;
use time::{OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};
use uuid::Uuid;
/// Backend-defined job identity within the assignment's pinned execution context.
/// Namespace distinguishes clusters/accounts that may reuse job identifiers.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BackendHandle {
    #[cfg_attr(
        feature = "openapi",
        schema(min_length = 1, max_length = 128, pattern = "^[!-~]+$")
    )]
    pub namespace: String,
    #[cfg_attr(
        feature = "openapi",
        schema(min_length = 1, max_length = 128, pattern = "^[!-~]+$")
    )]
    pub job_id: String,
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticLevel {
    Info,
    Warning,
    Error,
}

/// Private retained text, never an operational trace field or a terminal result.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Diagnostic {
    #[serde(deserialize_with = "string_enum")]
    pub level: DiagnosticLevel,
    #[cfg_attr(
        feature = "openapi",
        schema(min_length = 1, max_length = 64, pattern = "^[a-z][a-z0-9_]*$")
    )]
    pub code: String,
    /// At most 2048 UTF-8 bytes; only tab and newline control characters allowed.
    #[cfg_attr(feature = "openapi", schema(min_length = 1, max_length = 2048))]
    pub message: String,
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EventRequest {
    pub attempt_id: Uuid,
    #[cfg_attr(feature = "openapi", schema(minimum = 1))]
    pub sequence: i64,
    /// Actual evidence capture time, not retry time. Never used as server time.
    #[cfg_attr(feature = "openapi", schema(format = DateTime))]
    pub observed_at: String,
    #[serde(default, deserialize_with = "optional_string_enum")]
    pub phase: Option<RunPhase>,
    #[serde(default, deserialize_with = "optional_object")]
    pub backend_handle: Option<BackendHandle>,
    #[serde(default, deserialize_with = "optional_object")]
    pub diagnostic: Option<Diagnostic>,
}

fn string_enum<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = String::deserialize(deserializer)?;
    serde_json::from_value(value.into()).map_err(serde::de::Error::custom)
}

fn optional_string_enum<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    Option::<String>::deserialize(deserializer)?
        .map(|value| serde_json::from_value(value.into()).map_err(serde::de::Error::custom))
        .transpose()
}

fn optional_object<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let raw = Option::<Box<RawValue>>::deserialize(deserializer)?;
    raw.map(|raw| {
        if !raw.get().starts_with('{') {
            return Err(serde::de::Error::custom("expected an object"));
        }
        serde_json::from_str(raw.get()).map_err(serde::de::Error::custom)
    })
    .transpose()
}

impl EventRequest {
    pub fn normalize(mut self) -> Result<Self, EventValidationError> {
        let invalid = || EventValidationError::InvalidRequest;
        if self.attempt_id.is_nil()
            || self.sequence < 1
            || self.observed_at.len() > 64
            || (self.phase.is_none() && self.diagnostic.is_none())
            || (self.backend_handle.is_some() && self.phase.is_none())
            || (self.phase.is_some_and(|phase| phase != RunPhase::Preparing)
                && self.backend_handle.is_none())
        {
            return Err(invalid());
        }
        let timestamp = OffsetDateTime::parse(&self.observed_at, &Rfc3339)
            .map_err(|_| invalid())?
            .to_offset(UtcOffset::UTC);
        if !(1..=9999).contains(&timestamp.year()) || timestamp.nanosecond() % 1000 != 0 {
            return Err(invalid());
        }
        self.observed_at = timestamp.format(&Rfc3339).map_err(|_| invalid())?;
        if let Some(handle) = &self.backend_handle {
            for value in [&handle.namespace, &handle.job_id] {
                if value.is_empty()
                    || value.len() > 128
                    || !value.bytes().all(|byte| byte.is_ascii_graphic())
                {
                    return Err(invalid());
                }
            }
        }
        if let Some(diagnostic) = &self.diagnostic
            && (diagnostic.code.is_empty()
                || diagnostic.code.len() > 64
                || !diagnostic.code.as_bytes()[0].is_ascii_lowercase()
                || !diagnostic
                    .code
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
                || diagnostic.message.is_empty()
                || diagnostic.message.len() > 2048
                || diagnostic
                    .message
                    .chars()
                    .any(|c| c.is_control() && c != '\n' && c != '\t'))
        {
            return Err(invalid());
        }
        Ok(self)
    }
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventDisposition {
    Applied,
    Stale,
}

/// Immutable acknowledgement, not a current-state response. Replays preserve it.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct EventReceipt {
    pub assignment_id: Uuid,
    pub event_id: Uuid,
    pub sequence: i64,
    pub disposition: EventDisposition,
    pub phase: Option<RunPhase>,
    #[cfg_attr(feature = "openapi", schema(format = DateTime))]
    pub received_at: String,
    pub observation_confirmed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventValidationError {
    InvalidRequest,
}
