use super::{claims::AssignmentReceipt, observations::BackendHandle};
use crate::runs::{ObservationFreshness, RunPhase, RunState};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Bounded identity and progress only; scientific inputs are available in detail.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AssignmentSummary {
    pub assignment_id: Uuid,
    pub run_id: Uuid,
    pub attempt_id: Uuid,
    pub journal_id: Uuid,
    pub state: RunState,
    pub phase: Option<RunPhase>,
    #[cfg_attr(feature = "openapi", schema(format = DateTime))]
    pub created_at: String,
}

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Deserialize, Serialize)]
pub struct AssignmentList {
    pub items: Vec<AssignmentSummary>,
    /// Opaque continuation over a live inventory, oldest assignments first.
    pub next_cursor: Option<String>,
}

/// Immutable original claim receipt beside current progress. No launch authority.
/// Deliberately omit Debug because the specification contains private inputs.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Clone, Deserialize, Serialize)]
pub struct AssignmentDetail {
    /// Delivery settlement is independent from the immutable scientific result.
    #[serde(default)]
    pub delivery_completion: Option<super::delivery::DeliveryCompletionReceipt>,
    /// Safe marker distinguishing operator resolution from connector reporting.
    pub resolution: Option<super::resolution::ResolutionSummary>,
    /// Retained terminal result; null while work remains outstanding.
    pub result: Option<super::results::RunResult>,
    pub assignment: AssignmentReceipt,
    pub state: RunState,
    pub phase: Option<RunPhase>,
    /// Greatest accepted event sequence, including diagnostic-only events.
    pub last_event_sequence: Option<i64>,
    /// Immutable backend identity reported for this assignment, if known.
    pub backend_handle: Option<BackendHandle>,
    /// API receipt time of the latest qualifying execution observation.
    #[cfg_attr(feature = "openapi", schema(format = DateTime))]
    pub last_observed_at: Option<String>,
    /// API confirmation age: fresh below 60 seconds, stale at or above 60 seconds.
    /// Missing or future confirmation times are unknown.
    pub observation_freshness: ObservationFreshness,
}
