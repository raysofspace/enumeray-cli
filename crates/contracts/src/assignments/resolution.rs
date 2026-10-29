use serde::{Deserialize, Serialize};
use uuid::Uuid;
pub const RESOLUTION_FAILURE_CODE: &str = "operator_resolved";
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    NeverStarted,
    Terminated,
}

impl ExecutionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NeverStarted => "never_started",
            Self::Terminated => "terminated",
        }
    }
}

/// Safe reconciliation marker. No operator identity, notes, or process IDs.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResolutionSummary {
    pub assignment_id: Uuid,
    pub attempt_id: Uuid,
    pub execution_status: ExecutionStatus,
    #[cfg_attr(feature = "openapi", schema(format = DateTime))]
    pub resolved_at: String,
}
