use crate::{
    execution::{ExecutionRequirements, InstallationDescriptor},
    models::ScientificDefinition,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Complete pinned specification. Inputs are private; deliberately omit Debug.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AssignmentReceipt {
    pub assignment_id: Uuid,
    pub run_id: Uuid,
    pub attempt_id: Uuid,
    pub connector_id: String,
    pub journal_id: Uuid,
    pub model_id: String,

    pub contract_version: u32,
    /// Immutable accepted publication and execution context.
    pub publication_id: String,
    pub context_id: String,
    pub descriptor: InstallationDescriptor,
    pub requirements: ExecutionRequirements,
    pub definition: ScientificDefinition,
    pub inputs: Value,
    /// Pinned at acceptance; omitted from historical receipts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_destination_id: Option<String>,
    #[cfg_attr(feature = "openapi", schema(format = DateTime))]
    pub created_at: String,
}
