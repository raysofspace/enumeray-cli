use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionRequest {
    pub journal_id: Uuid,
    pub session_id: Uuid,
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SessionReceipt {
    pub connector_id: String,
    pub journal_id: Uuid,
    pub session_id: Uuid,
}

/// Persist this exact transition before sending it. The stopped-process
/// assertion is a prerequisite established by the caller, never a heartbeat or
/// lease inference, and does not assert that any backend execution has stopped.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionRecoveryRequest {
    pub journal_id: Uuid,
    pub previous_session_id: Uuid,
    pub session_id: Uuid,
    pub prior_process_stopped: bool,
}
