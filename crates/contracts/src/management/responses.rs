//! Management response contracts. Opaque scientific and immutable evidence JSON remains unmodified.
//! Timestamp strings retain each endpoint's established PostgreSQL representation.
use super::*;
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Collection<T> {
    pub items: Vec<T>,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ContextSummary {
    pub id: String,
    pub connector_id: String,
    #[cfg_attr(feature = "openapi", schema(schema_with = execution_context_schema))]
    pub definition: Value,
    pub metadata: ContextMetadata,
    pub state: ContextLifecycleState,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub generation: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ContextRegistration {
    pub id: String,
    pub connector_id: String,
    #[cfg_attr(feature = "openapi", schema(schema_with = execution_context_schema))]
    pub definition: Value,
    pub metadata: ContextMetadata,
    pub generation: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ContextMetadataReceipt {
    pub id: String,
    pub metadata: ContextMetadata,
    pub generation: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RevokedGrant {
    pub id: Uuid,
    pub revoked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AvailableGrant {
    pub id: Uuid,
    pub context_id: String,
    pub connector_id: String,
    pub connector_label: String,
    pub user_id: Uuid,
    pub generation: i64,
    pub max_queued_runs: u32,
    pub max_outstanding_assignments: u32,
    pub context: AvailableContext,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AvailableContext {
    pub id: String,
    pub driver: String,
    pub connection: String,
    pub installation_roots: Vec<String>,
    pub metadata: ContextMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct OperationalRun {
    pub run_id: Uuid,
    pub model_id: String,
    pub publication_id: String,
    pub context_id: String,
    pub execution_location: String,
    pub connector_id: String,
    pub state: String,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub phase: Option<String>,
    pub created_at: String,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub blocked_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ManagedRun {
    pub run_id: Uuid,
    pub model_id: String,
    pub publication_id: String,
    pub purpose: String,
    pub created_by_user_id: Uuid,
    pub state: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AccessPolicy {
    pub mode: AccessMode,
    pub visibility: Visibility,
    pub generation: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AccessReceipt {
    pub user_id: Uuid,
    pub allowed: bool,
    pub generation: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct InvitationReceipt {
    pub id: Uuid,
    pub model_id: String,
    pub token: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AcceptanceReceipt {
    pub id: Uuid,
    pub accepted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AccessRequestSummary {
    pub id: Uuid,
    pub user_id: Uuid,
    pub status: String,
    pub message: String,
    pub created_at: String,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub resolved_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AccessRequestReceipt {
    pub id: Uuid,
    pub status: String,
    pub model_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AccessDecisionReceipt {
    pub id: Uuid,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct InvitationSummary {
    pub id: Uuid,
    pub model_id: String,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub recipient_id: Option<Uuid>,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub recipient_email: Option<String>,
    pub expires_at: String,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub accepted_by: Option<Uuid>,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub accepted_at: Option<String>,
    pub created_at: String,
    pub status: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct ConnectorEnrollment {
    pub id: Uuid,
    pub connector_id: String,
    pub enrollment_token: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct ConnectorAlreadyRegistered {
    pub connector_id: String,
    pub already_registered: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ConnectorUpdateReceipt {
    pub connector_id: String,
    pub label: String,
    pub max_outstanding_assignments: u32,
    pub generation: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ConnectorSummary {
    pub connector_id: String,
    pub owner_id: Uuid,
    pub ownership_generation: i64,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub archived_at: Option<String>,
    pub label: String,
    pub generation: i64,
    pub observed_generation: i64,
    pub draining: bool,
    pub authority_withdrawn: bool,
    pub max_outstanding_assignments: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ConnectorRevocationReceipt {
    pub connector_id: String,
    pub revoked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ConfigurationAcknowledgmentReceipt {
    pub generation: i64,
    pub acknowledged: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PublicationRegistration {
    pub id: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PublicationActivationReceipt {
    pub id: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PublicationSummary {
    pub id: String,
    #[cfg_attr(feature = "openapi", schema(value_type = crate::models::ScientificDefinition))]
    pub definition: Value,
    pub admission_open: bool,
    pub context_id: String,
    pub grant_id: Uuid,
    pub generation: i64,
    #[cfg_attr(feature = "openapi", schema(value_type = crate::execution::InstallationDescriptor))]
    pub descriptor: Value,
    pub status: String,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub validation_message: Option<String>,
    pub disclosure: String,
    pub execution_location: String,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub revoked_at: Option<String>,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub installation_revoked_at: Option<String>,
    pub created_at: String,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub published_at: Option<String>,
    pub configuration_retention: PublicationConfigurationRetention,
    pub rollback_availability: PublicationRollbackAvailability,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PublicationDetail {
    pub trial_availability: Availability,
    pub trial_evidence: Value,
    pub id: String,
    pub admission_open: bool,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub admission_updated_at: Option<String>,
    pub model_id: String,
    pub context_id: String,
    pub execution_location: String,
    pub grant_id: Uuid,
    pub generation: i64,
    #[cfg_attr(feature = "openapi", schema(value_type = crate::execution::InstallationDescriptor))]
    pub descriptor: Value,
    pub status: String,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub validation_message: Option<String>,
    pub disclosure: String,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub revoked_at: Option<String>,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub installation_revoked_at: Option<String>,
    pub created_at: String,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub published_at: Option<String>,
    #[cfg_attr(feature = "openapi", schema(value_type = crate::models::ScientificDefinition))]
    pub definition: Value,
    pub configuration_retention: PublicationConfigurationRetention,
    pub rollback_availability: PublicationRollbackAvailability,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub validation_evidence: Option<Value>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_authority"
    )]
    #[cfg_attr(feature = "openapi", schema(required = true, value_type = PublicationReleaseAuthority))]
    pub release_authority: Option<PublicationReleaseAuthority>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_authority"
    )]
    #[cfg_attr(feature = "openapi", schema(required = true, value_type = PublicationReleaseAuthority))]
    pub activation_availability: Option<PublicationReleaseAuthority>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Availability {
    pub available: bool,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PublicationReleaseAuthority {
    pub grant_holder_user_id: Uuid,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub publication_delegation_id: Option<Uuid>,
    pub can_delegate: bool,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub unavailable_reason: Option<String>,
    pub available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ManagedModel {
    pub model_id: String,
    pub name: String,
    pub description: String,
    pub owner_id: Uuid,
    pub generation: i64,
    pub access_mode: AccessMode,
    pub visibility: Visibility,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub current_publication_id: Option<String>,
    pub can_run: bool,
    pub can_manage: bool,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub current_publication: Option<CurrentPublication>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CurrentPublication {
    pub id: String,
    pub publication_id: String,
    #[cfg_attr(feature = "openapi", schema(value_type = crate::models::ScientificDefinition))]
    pub definition: Value,
    pub disclosure: String,
    pub execution_location: String,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub revoked_at: Option<String>,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub installation_revoked_at: Option<String>,
    pub context_id: String,
    pub status: String,
    pub available: bool,
    pub submission_availability: SubmissionAvailability,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SubmissionAvailability {
    pub admission_open: bool,
    pub has_approved_execution_path: bool,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub unavailable_reason: Option<SubmissionUnavailableReason>,
    pub execution_destination: ExecutionDestination,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ExecutionDestination {
    pub context_id: String,
    pub execution_location: String,
    pub disclosure: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PublicationRetirementReceipt {
    pub publication_id: String,
    pub model_id: String,
    pub requested_at: String,
    pub generation: i64,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PublicationDelegation {
    pub id: Uuid,
    pub publication_id: String,
    pub grant_id: Uuid,
    pub created_by: Uuid,
    pub user_id: Uuid,
    pub expected_generation: i64,
    pub created_at: String,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub revoked_at: Option<String>,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub revocation_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ResourceMembers {
    pub owner_id: Uuid,
    pub generation: i64,
    pub co_admins: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MembershipReceipt {
    pub user_id: Uuid,
    pub co_admin: bool,
    pub generation: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ArchiveReceipt {
    pub id: String,
    pub archived: bool,
    pub generation: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TransferProposalReceipt {
    pub id: Uuid,
    pub to_user_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TransferAcceptanceReceipt {
    pub id: Uuid,
    pub accepted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ServiceLimits {
    pub max_queued_runs: i32,
    pub max_queued_runs_per_user: i32,
    pub generation: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UserSuspensionReceipt {
    pub user_id: Uuid,
    pub suspended: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AuditEvent {
    pub id: Uuid,
    pub actor_kind: String,
    pub actor_id: String,
    pub action: String,
    pub details: Value,
    pub occurred_at: String,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ServiceAuditEvent {
    pub id: Uuid,
    pub actor_kind: String,
    pub actor_id: String,
    pub resource_kind: String,
    pub resource_id: String,
    pub action: String,
    pub details: Value,
    pub occurred_at: String,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ServiceStatus {
    pub queued_runs: i64,
    pub running_runs: i64,
    pub blocked_runs: i64,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub oldest_queued_at: Option<String>,
    #[cfg_attr(feature = "openapi", schema(value_type = f64))]
    pub oldest_queue_age_seconds: serde_json::Number,
    pub configuration_lag: i64,
    pub offline_connectors: i64,
    pub expired_enrollments: i64,
    pub failed_publications: i64,
    pub unconfirmed_executions: i64,
    pub unconfirmed_result_transfers: i64,
    pub pending_revocations: i64,
    pub draining_revocations: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ConfigurationCounts {
    #[cfg_attr(feature = "openapi", schema(minimum = 0))]
    pub historical_publications: i64,
    #[cfg_attr(feature = "openapi", schema(minimum = 0))]
    pub retained_publications: i64,
    #[cfg_attr(feature = "openapi", schema(minimum = 0))]
    pub retirement_requested: i64,
    #[cfg_attr(feature = "openapi", schema(minimum = 0))]
    pub retired_publications: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ConfigurationCapacity {
    pub desired_bytes: usize,
    pub maximum_desired_bytes: usize,
    pub maximum_retained_publications: usize,
    pub maximum_connector_cache_bytes: usize,
    pub counts: ConfigurationCounts,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ConnectorDiagnostics {
    #[serde(flatten)]
    pub snapshot: ConnectorLifecycleStatus,
    pub unfinished_assignments: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReplacementContext {
    pub id: String,
    pub connector_id: String,
    pub state: ContextLifecycleState,
    pub generation: i64,
    pub driver: String,
    pub connection: String,
    pub metadata: ContextMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReplacementAction {
    pub role: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocking_publications: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_cleanup_state: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PublicationDependency {
    pub id: String,
    pub model_id: String,
    pub model_owner_id: Uuid,
    pub grant_id: Uuid,
    pub grant_holder_user_id: Uuid,
    pub status: String,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub current: Option<bool>,
    pub model_archived: bool,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub revoked_at: Option<String>,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub installation_revoked_at: Option<String>,
    pub configuration_retention: PublicationConfigurationRetention,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct GrantDependency {
    pub id: Uuid,
    pub context_id: String,
    pub user_id: Uuid,
    pub max_queued_runs: u32,
    pub max_outstanding_assignments: u32,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub revoked_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RunDependency {
    pub id: Uuid,
    pub model_id: String,
    pub publication_id: String,
    pub state: String,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub phase: Option<String>,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub blocked_reason: Option<String>,
}

/// First registration returns a one-time secret; exact retries never mint a replacement.
#[derive(Clone, Serialize, Deserialize)]
#[serde(untagged)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum ConnectorRegistration {
    Enrollment(ConnectorEnrollment),
    AlreadyRegistered(ConnectorAlreadyRegistered),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum ContextDependency {
    Publication(PublicationDependency),
    Grant(GrantDependency),
    Run(RunDependency),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct StoredDesiredConfiguration {
    pub generation: i64,
    pub contexts: Vec<StoredDesiredContext>,
    pub publications: Vec<StoredDesiredPublication>,
    pub run_payload_namespace_reserved: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct StoredDesiredContext {
    pub id: String,
    #[cfg_attr(feature = "openapi", schema(schema_with = execution_context_schema))]
    pub definition: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct StoredDesiredPublication {
    pub publication_id: String,
    pub context_id: String,
    pub generation: u64,
    #[cfg_attr(feature = "openapi", schema(value_type = crate::execution::InstallationDescriptor))]
    pub descriptor: Value,
    #[cfg_attr(feature = "openapi", schema(value_type = crate::execution::ExecutionRequirements))]
    pub requirements: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PublicationConfigurationRetention {
    pub state: String,
    pub requested_at: Option<String>,
    pub retired_at: Option<String>,
    pub retirement_blocked: bool,
    pub blockers: PublicationRetirementBlockers,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PublicationRetirementBlockers {
    #[cfg_attr(feature = "openapi", schema(minimum = 0))]
    pub queued_runs: i64,
    #[cfg_attr(feature = "openapi", schema(minimum = 0))]
    pub running_runs: i64,
    #[cfg_attr(feature = "openapi", schema(minimum = 0))]
    pub pending_delivery_assignments: i64,
    #[cfg_attr(feature = "openapi", schema(minimum = 0))]
    pub pending_log_archives: i64,
    #[cfg_attr(feature = "openapi", schema(minimum = 0))]
    pub pending_assignment_uploads: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PublicationRollbackAvailability {
    pub available: bool,
    pub unavailable_reason: Option<String>,
    pub requires_revalidation: bool,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum RevocationScope {
    Publication,
    Installation,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct PublicationRevocationReceipt {
    pub scope: RevocationScope,
    pub affected_publication_ids: Vec<String>,
    pub publication_id: String,
    #[cfg_attr(feature = "openapi", schema(format = DateTime))]
    pub revoked_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn exact_retry_receipts_preserve_omitted_generation() {
        for value in [
            json!({"id":"candidate","status":"ready"}),
            json!({"id":"candidate","status":"validating","generation":9}),
        ] {
            let record: PublicationRegistration = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(record).unwrap(), value);
        }
        let value = json!({"id":Uuid::nil(),"accepted":true});
        let record: TransferAcceptanceReceipt = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(record).unwrap(), value);
    }
    #[test]
    fn opaque_scientific_configuration_preserves_unknown_fields_and_numeric_precision() {
        let value:Value=serde_json::from_str(r#"{"generation":4,"contexts":[{"id":"cpu","definition":{"legacy":null,"precision":123456789012345678901234567890.1234567890123456789}}],"publications":[],"run_payload_namespace_reserved":true}"#).unwrap();
        let decoded: StoredDesiredConfiguration = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), value);
    }
    #[test]
    fn nullable_collection_cursor_and_timestamps_round_trip_without_normalization() {
        let value = json!({"items":[{"connector_id":"worker","owner_id":Uuid::nil(),"ownership_generation":1,"label":"Lab","generation":1,"observed_generation":0,"draining":false,"authority_withdrawn":false,"max_outstanding_assignments":4,"archived_at":"2026-10-17T12:13:14.123456+00:00"}],"next_cursor":null});
        let decoded: Collection<ConnectorSummary> = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), value);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum SubmissionUnavailableReason {
    PublicationRevoked,
    InstallationRevoked,
    AdmissionClosed,
    NoApprovedExecutionPath,
    ConnectorMaintenance,
    ContextNotActive,
}

/// The service documents execution configuration explicitly, including retained legacy contexts.
#[cfg(feature = "openapi")]
fn execution_context_schema() -> utoipa::openapi::Ref {
    utoipa::openapi::Ref::from_schema_name("ExecutionContextDefinition")
}

#[cfg(test)]
mod enrollment_contract_tests {
    use super::*;
    #[test]
    fn secret_enrollment_documents_never_discard_ambiguous_replay_fields() {
        let value = serde_json::json!({"id":Uuid::nil(),"connector_id":"worker","enrollment_token":"secret","expires_at":"2026-10-17 00:00:00+00","already_registered":true});
        assert!(serde_json::from_value::<ConnectorRegistration>(value).is_err());
    }
}

// Older services omit these fields. Explicit null or malformed authority must
// fail closed, rather than acquiring the legacy missing-field fallback.
fn present_authority<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<PublicationReleaseAuthority>, D::Error> {
    PublicationReleaseAuthority::deserialize(deserializer).map(Some)
}
