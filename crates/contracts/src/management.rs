//! Self-service management requests and responses shared by the API and standalone CLI.
mod responses;
use crate::{
    execution::{ExecutionContextDefinition, InstallationSpec},
    models::{ModelMetadata, ScientificDefinition},
};
pub use responses::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ContextMetadata {
    /// Descriptive location, 1–200 UTF-8 bytes without surrounding whitespace or controls.
    #[cfg_attr(feature = "openapi", schema(min_length = 1, max_length = 200))]
    pub location: String,
    /// Connector data-access disclosure, 1–3800 UTF-8 bytes without surrounding whitespace or controls.
    #[cfg_attr(feature = "openapi", schema(min_length = 1, max_length = 3800))]
    pub disclosure: String,
}

impl ContextMetadata {
    /// Descriptive connector-owned text; never a routing or authorization input.
    pub fn validate(&self) -> Result<(), &'static str> {
        for (value, maximum) in [(&self.location, 200), (&self.disclosure, 3800)] {
            if value.is_empty()
                || value.len() > maximum
                || value.trim() != value
                || value.chars().any(char::is_control)
            {
                return Err(
                    "context metadata must be bounded nonempty text without surrounding whitespace or control characters",
                );
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextMetadataUpdate {
    pub metadata: ContextMetadata,
    pub expected_generation: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCreate {
    pub id: String,
    pub connector_id: String,
    pub definition: ExecutionContextDefinition,
    pub metadata: ContextMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantCreate {
    /// Optional durable creation identity. Replays never replace a revoked grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,
    pub context_id: String,
    pub user_id: Uuid,
    #[serde(default = "queue_limit")]
    pub max_queued_runs: u32,
    #[serde(default = "outstanding_limit")]
    pub max_outstanding_assignments: u32,
}

/// Replace both capacity limits at one observed grant generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantUpdate {
    pub expected_generation: i64,
    pub max_queued_runs: u32,
    pub max_outstanding_assignments: u32,
}

/// Current grant policy. Publications retain this identity when limits change.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ComputeGrant {
    pub id: Uuid,
    pub context_id: String,
    pub user_id: Uuid,
    pub max_queued_runs: u32,
    pub max_outstanding_assignments: u32,
    pub generation: i64,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub revoked_at: Option<String>,
}

fn queue_limit() -> u32 {
    100
}
fn outstanding_limit() -> u32 {
    4
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorCreate {
    pub id: String,
    pub label: String,
    #[serde(default = "outstanding_limit")]
    pub max_outstanding_assignments: u32,
}

/// Update connector presentation and dispatch capacity at one observed generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorUpdate {
    pub label: String,
    pub max_outstanding_assignments: u32,
    pub expected_generation: i64,
}

/// Pause fresh acceptance and claims while preserving existing assignments.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorStateChange {
    pub draining: bool,
    pub expected_generation: i64,
}

/// Independently withdraw or restore this connector's execution authority.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorAuthorityChange {
    pub withdrawn: bool,
    pub expected_generation: i64,
}

/// Counts describe durable service evidence, never scientific inputs or results.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ConnectorWorkCounts {
    #[cfg_attr(feature = "openapi", schema(minimum = 0))]
    pub retained_queued_runs: i64,
    #[cfg_attr(feature = "openapi", schema(minimum = 0))]
    pub unfinished_assignments: i64,
    #[cfg_attr(feature = "openapi", schema(minimum = 0))]
    pub pending_delivery_assignments: i64,
    #[cfg_attr(feature = "openapi", schema(minimum = 0))]
    pub pending_uploads: i64,
    #[cfg_attr(feature = "openapi", schema(minimum = 0))]
    pub pending_log_archives: i64,
    /// A subset of unfinished assignments; stale contact cannot prove completion.
    #[cfg_attr(feature = "openapi", schema(minimum = 0))]
    pub unconfirmed_assignments: i64,
    pub assignment_phases: ConnectorAssignmentPhases,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ConnectorAssignmentPhases {
    #[cfg_attr(feature = "openapi", schema(minimum = 0))]
    pub preparing: i64,
    #[cfg_attr(feature = "openapi", schema(minimum = 0))]
    pub waiting_for_backend: i64,
    #[cfg_attr(feature = "openapi", schema(minimum = 0))]
    pub executing: i64,
    #[cfg_attr(feature = "openapi", schema(minimum = 0))]
    pub collecting_results: i64,
    #[cfg_attr(feature = "openapi", schema(minimum = 0))]
    pub uploading_results: i64,
}

impl ConnectorWorkCounts {
    pub fn assigned_work_finished(&self) -> bool {
        self.unfinished_assignments == 0
            && self.unconfirmed_assignments == 0
            && self.pending_delivery_assignments == 0
            && self.pending_uploads == 0
            && self.pending_log_archives == 0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ConnectorLifecycleReceipt {
    pub connector_id: String,
    pub generation: i64,
    pub draining: bool,
    pub authority_withdrawn: bool,
    pub changed: bool,
    pub work: ConnectorWorkCounts,
    pub maintenance_complete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ConnectorLifecycleStatus {
    pub connector_id: String,
    pub owner_id: Uuid,
    pub ownership_generation: i64,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub archived_at: Option<String>,
    pub generation: i64,
    pub observed_generation: i64,
    pub configuration_pending: bool,
    pub draining: bool,
    pub authority_withdrawn: bool,
    pub work: ConnectorWorkCounts,
    pub maintenance_complete: bool,
    pub failed_publications: i64,
    pub configuration_capacity: ConfigurationCapacity,
}

/// Operational lifecycle; the immutable execution definition never changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum ContextLifecycleState {
    Active,
    Draining,
    Retired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum ContextAcceptedWorkPolicy {
    Drain,
    CancelQueued,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCutover {
    pub expected_generation: i64,
    pub replacement_context_id: Option<String>,
    pub accepted_work: ContextAcceptedWorkPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ContextLifecycleStatus {
    pub id: String,
    pub connector_id: String,
    pub generation: i64,
    pub state: ContextLifecycleState,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub replacement_context_id: Option<String>,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub accepted_work: Option<ContextAcceptedWorkPolicy>,
    pub work: ConnectorWorkCounts,
    pub live_current_publications: i64,
    pub retained_publications: i64,
    pub pending_configuration_retirements: i64,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub configuration_removal_generation: Option<i64>,
    pub connector_generation: i64,
    pub observed_generation: i64,
    pub retirement_ready: bool,
    pub connector_work: ConnectorWorkCounts,
    pub local_cleanup_state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ContextLifecycleReceipt {
    pub operation: String,
    pub context: ContextLifecycleStatus,
    pub cancelled_queued_runs: i64,
    pub requested_publications: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextDependencySection {
    Publications,
    Grants,
    Runs,
}

impl ContextDependencySection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Publications => "publications",
            Self::Grants => "grants",
            Self::Runs => "runs",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ContextReplacementPlan {
    pub context: ContextLifecycleStatus,
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub replacement: Option<ReplacementContext>,
    pub dependencies: Collection<ContextDependency>,
    pub actions: Vec<ReplacementAction>,
}

#[cfg(test)]
mod context_lifecycle_contract_tests {
    use super::{ContextAcceptedWorkPolicy, ContextCutover, GenerationChange};
    use serde_json::json;

    #[test]
    fn cutover_requires_an_explicit_queue_policy_and_rejects_execution_changes() {
        for policy in ["drain", "cancel_queued"] {
            let request = json!({"expected_generation":7,"replacement_context_id":"next","accepted_work":policy});
            let decoded: ContextCutover = serde_json::from_value(request.clone()).unwrap();
            assert_eq!(serde_json::to_value(decoded).unwrap(), request);
            let mut execution_change = request;
            execution_change["definition"] = json!({});
            assert!(serde_json::from_value::<ContextCutover>(execution_change).is_err());
        }
        assert!(
            serde_json::from_value::<ContextCutover>(json!({"expected_generation":7})).is_err()
        );
        assert!(
            serde_json::from_value::<ContextCutover>(
                json!({"expected_generation":7,"accepted_work":"cancel_assigned"})
            )
            .is_err()
        );
        let unused: ContextCutover =
            serde_json::from_value(json!({"expected_generation":7,"accepted_work":"drain"}))
                .unwrap();
        assert!(unused.replacement_context_id.is_none());
        assert_eq!(unused.accepted_work, ContextAcceptedWorkPolicy::Drain);
        assert!(
            serde_json::from_value::<GenerationChange>(
                json!({"expected_generation":7,"redirect_runs":true})
            )
            .is_err()
        );
    }
}

#[cfg(test)]
mod grant_contract_tests {
    use super::GrantUpdate;
    use serde_json::json;

    #[test]
    fn grant_updates_require_both_limits_and_reject_identity_changes() {
        let valid =
            json!({"expected_generation":7,"max_queued_runs":10,"max_outstanding_assignments":2});
        assert_eq!(
            serde_json::to_value(serde_json::from_value::<GrantUpdate>(valid.clone()).unwrap())
                .unwrap(),
            valid
        );
        for missing in [
            "expected_generation",
            "max_queued_runs",
            "max_outstanding_assignments",
        ] {
            let mut request = valid.clone();
            request.as_object_mut().unwrap().remove(missing);
            assert!(serde_json::from_value::<GrantUpdate>(request).is_err());
        }
        for extra in ["id", "context_id", "user_id", "revoked_at"] {
            let mut request = valid.clone();
            request[extra] = json!("changed");
            assert!(serde_json::from_value::<GrantUpdate>(request).is_err());
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelCreate {
    pub model: ModelMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationCreate {
    pub id: String,
    pub definition: ScientificDefinition,
    pub context_id: String,
    pub descriptor: InstallationSpec,
}

#[cfg(test)]
mod publication_contract_tests {
    use super::PublicationCreate;
    use serde_json::json;

    #[test]
    fn author_publication_accepts_simplified_fields_and_rejects_grant_identity() {
        let candidate = crate::models::parse_publication(include_bytes!(
            "../../cli/examples/scalar-publication.json"
        ))
        .unwrap()
        .candidate("test-publication")
        .unwrap();
        candidate.descriptor.validate().unwrap();
        let valid = serde_json::to_value(candidate).unwrap();
        assert!(valid.get("grant_id").is_none());
        let mut invalid = valid;
        invalid["grant_id"] = json!(uuid::Uuid::new_v4());
        assert!(serde_json::from_value::<PublicationCreate>(invalid).is_err());
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationActivation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication_delegation_id: Option<Uuid>,
    pub expected_current_publication_id: Option<String>,
}

/// Release one publication's retained installation configuration without revocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationRetirement {
    pub expected_generation: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum AccessMode {
    Open,
    Request,
    Invite,
}
impl AccessMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Request => "request",
            Self::Invite => "invite",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum Visibility {
    Listed,
    Unlisted,
}
impl Visibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Listed => "listed",
            Self::Unlisted => "unlisted",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessPolicyUpdate {
    pub mode: AccessMode,
    /// Omitted or null preserves the model's existing catalogue visibility.
    pub visibility: Option<Visibility>,
    pub expected_generation: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvitationCreate {
    pub recipient_id: Option<Uuid>,
    pub recipient_email: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvitationAcceptance {
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessRequestCreate {
    #[serde(default)]
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessRequestDecision {
    pub decision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MembershipChange {
    pub user_id: Uuid,
    pub expected_generation: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferCreate {
    pub to_user_id: Uuid,
    pub expected_generation: i64,
}

/// Optional caller intent for the global transfer-acceptance endpoint. Scoped
/// clients send both fields so a copied transfer ID cannot select another resource.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferAcceptance {
    #[serde(
        default,
        deserialize_with = "transfer_guard_string",
        skip_serializing_if = "Option::is_none"
    )]
    pub resource_kind: Option<String>,
    #[serde(
        default,
        deserialize_with = "transfer_guard_string",
        skip_serializing_if = "Option::is_none"
    )]
    pub resource_id: Option<String>,
}

fn transfer_guard_string<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
}

impl TransferAcceptance {
    pub fn validate(&self) -> Result<(), &'static str> {
        match (&self.resource_kind, &self.resource_id) {
            (None, None) => Ok(()),
            (Some(kind), Some(resource))
                if matches!(kind.as_str(), "model" | "connector")
                    && crate::models::validation::valid_model_id(resource) =>
            {
                Ok(())
            }
            _ => Err(
                "Ownership-transfer intent requires both a supported resource kind and valid resource ID.",
            ),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationChange {
    pub expected_generation: i64,
}

#[cfg(test)]
mod transfer_acceptance_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn acceptance_requires_a_complete_non_null_guard_or_an_empty_object() {
        for document in [
            json!({}),
            json!({"resource_kind":"model","resource_id":"example"}),
            json!({"resource_kind":"connector","resource_id":"lab"}),
        ] {
            let request: TransferAcceptance = serde_json::from_value(document.clone()).unwrap();
            request.validate().unwrap();
            assert_eq!(serde_json::to_value(request).unwrap(), document);
        }
        for document in [
            json!({"resource_kind":null}),
            json!({"resource_kind":null,"resource_id":null}),
            json!({"resource_kind":"model","resource_id":null}),
            json!({"resource_kind":null,"resource_id":"example"}),
            json!({"extra":true}),
        ] {
            assert!(serde_json::from_value::<TransferAcceptance>(document).is_err());
        }
        for document in [
            json!({"resource_kind":"model"}),
            json!({"resource_id":"example"}),
            json!({"resource_kind":"unknown","resource_id":"example"}),
            json!({"resource_kind":"model","resource_id":"invalid/id"}),
        ] {
            let request: TransferAcceptance = serde_json::from_value(document).unwrap();
            assert!(request.validate().is_err());
        }
        assert!(
            serde_json::from_str::<TransferAcceptance>(
                r#"{"resource_kind":"model","resource_kind":"connector","resource_id":"example"}"#
            )
            .is_err()
        );
    }
}

#[cfg(test)]
mod access_policy_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn omitted_and_null_visibility_preserve_the_existing_policy() {
        for document in [
            json!({"mode":"request","expected_generation":7}),
            json!({"mode":"request","visibility":null,"expected_generation":7}),
        ] {
            let request: AccessPolicyUpdate = serde_json::from_value(document).unwrap();
            assert_eq!(request.mode, AccessMode::Request);
            assert_eq!(request.visibility, None);
            assert_eq!(request.expected_generation, 7);
        }
        for (value, expected) in [
            ("listed", Visibility::Listed),
            ("unlisted", Visibility::Unlisted),
        ] {
            let request: AccessPolicyUpdate = serde_json::from_value(
                json!({"mode":"invite","visibility":value,"expected_generation":7}),
            )
            .unwrap();
            assert_eq!(request.visibility, Some(expected));
        }
        for document in [
            json!({"mode":"request","visibility":"private","expected_generation":7}),
            json!({"mode":"request","expected_generation":7,"default_visibility":true}),
            json!({"mode":"request"}),
        ] {
            assert!(serde_json::from_value::<AccessPolicyUpdate>(document).is_err());
        }
    }
}

#[cfg(test)]
mod context_metadata_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn bounds_count_utf8_bytes_and_reject_whitespace_and_controls() {
        let valid = ContextMetadata {
            location: "🧪".repeat(50),
            disclosure: "é".repeat(1900),
        };
        valid.validate().unwrap();
        for location in [
            "".into(),
            "🧪".repeat(51),
            " padded".into(),
            "padded\u{2003}".into(),
            "line\nfeed".into(),
        ] {
            assert!(
                ContextMetadata {
                    location,
                    disclosure: valid.disclosure.clone()
                }
                .validate()
                .is_err()
            );
        }
        for disclosure in [
            "".into(),
            "é".repeat(1901),
            " padded".into(),
            "control\u{7f}".into(),
        ] {
            assert!(
                ContextMetadata {
                    location: valid.location.clone(),
                    disclosure
                }
                .validate()
                .is_err()
            );
        }
    }
    #[test]
    fn metadata_and_context_contracts_reject_registry_fields() {
        assert!(
            serde_json::from_value::<ContextMetadata>(
                json!({"location":"Lab","disclosure":"Data access","id":"site"})
            )
            .is_err()
        );
        let mut document: serde_json::Value =
            serde_json::from_str(include_str!("../../cli/examples/local-context.json")).unwrap();
        let context: ContextCreate = serde_json::from_value(document["spec"].clone()).unwrap();
        context.metadata.validate().unwrap();
        document["spec"]["site"] = json!({"id":"legacy"});
        assert!(serde_json::from_value::<ContextCreate>(document["spec"].clone()).is_err());
        document["spec"].as_object_mut().unwrap().remove("site");
        document["spec"]["definition"]["execution_site_id"] = json!("legacy");
        assert!(serde_json::from_value::<ContextCreate>(document["spec"].clone()).is_err());
    }
}

/// Authorize a named administrator under this publication's retained compute grant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationDelegationCreate {
    pub user_id: Uuid,
    pub expected_generation: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessChange {
    pub allowed: bool,
    pub expected_generation: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceLimitsUpdate {
    pub max_queued_runs: i32,
    pub max_queued_runs_per_user: i32,
    pub expected_generation: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserSuspensionUpdate {
    pub suspended: bool,
    pub reason: String,
}
