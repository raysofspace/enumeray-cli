//! Scientific completion and connector delivery completion are separate evidence.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryCompletionRequest {
    pub attempt_id: Uuid,
    /// True when this assignment has a configured archive to publish or abandon.
    pub archive_expected: bool,
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryCompletionReceipt {
    pub assignment_id: Uuid,
    pub run_id: Uuid,
    pub attempt_id: Uuid,
    /// `connector` accounts for normal delivery; `operator_abandonment` records loss.
    pub source: String,
    #[cfg_attr(feature = "openapi", schema(format = DateTime))]
    pub completed_at: String,
}
