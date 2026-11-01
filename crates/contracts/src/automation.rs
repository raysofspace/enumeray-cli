//! Versioned CLI automation projections, separate from scientific JSON and saved journals.
//!
//! Readers may ignore additional object fields. Missing decision fields, unknown
//! enum values and unsupported versions must stop automation before mutation.
//! During the unreleased candidate, version 1 evolves in place; matching this
//! number alone does not establish compatibility between different builds.
use crate::{
    execution::{InstallationDescriptor, InstallationIdentity},
    management::{
        Availability, PublicationActivationReceipt, PublicationReleaseAuthority,
        PublicationTrialEvidence,
    },
    runs::{RunPhase, RunPurpose, RunState},
};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use uuid::Uuid;

/// A required version marker. Unknown versions never deserialize successfully.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct SchemaVersion;
#[cfg(feature = "openapi")]
impl utoipa::PartialSchema for SchemaVersion {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::schema::{ObjectBuilder, SchemaType, Type};
        ObjectBuilder::new()
            .schema_type(SchemaType::Type(Type::Integer))
            .minimum(Some(1.0))
            .maximum(Some(1.0))
            .into()
    }
}
#[cfg(feature = "openapi")]
impl utoipa::ToSchema for SchemaVersion {}
impl TryFrom<u32> for SchemaVersion {
    type Error = &'static str;
    fn try_from(value: u32) -> Result<Self, Self::Error> {
        if value == 1 {
            Ok(Self)
        } else {
            Err("unsupported CLI output schema_version")
        }
    }
}
impl From<SchemaVersion> for u32 {
    fn from(_: SchemaVersion) -> Self {
        1
    }
}

fn required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

macro_rules! string_enum {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        #[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
        pub enum $name { $($variant),+ }
    };
}
string_enum!(AnalysisStage {
    Editable,
    Uploading,
    SubmissionUncertain,
    Accepted,
    AttachmentIncomplete
});
string_enum!(ReleaseStage {
    Prepared,
    Validating,
    Ready,
    Reviewed,
    Blocked,
    Activated
});
string_enum!(PublicationStatus {
    Validating,
    Ready,
    Failed,
    Published
});
string_enum!(CancellationAttempt {
    Pending,
    Acknowledged,
    Uncertain,
    Rejected
});
string_enum!(CancellationStatus {
    Requested,
    Confirmed,
    Completed
});
string_enum!(CancellationScope {
    PendingAndRunning,
    QueuedOnly
});
string_enum!(EvidenceAvailability {
    Retained,
    Unavailable
});
string_enum!(AnalysisOrigin {
    Local,
    ImportedPrepared,
    AttachedRun
});
string_enum!(ExecutionControlState {
    Authorized,
    Blocked,
    CancellationPending,
    Draining,
    Completed
});
string_enum!(ExecutionAuthorizationReason {
    ModelAccessRevoked,
    ComputeAccessRevoked,
    ComputeUnavailable,
    PublicationRevoked,
    InstallationRevoked
});
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ExecutionControl {
    pub state: ExecutionControlState,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub authorization_reason: Option<ExecutionAuthorizationReason>,
}

/// The original acceptance observation; it is never updated to current run state.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AcceptanceReceipt {
    pub run_id: Uuid,
    pub model_id: String,
    pub publication_id: String,
    pub purpose: RunPurpose,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication_delegation_id: Option<Uuid>,
    pub state: AcceptanceState,
    pub created_at: String,
}
string_enum!(AcceptanceState { Queued });

/// Decision-bearing run fields are typed; scientific and diagnostic additions
/// remain available in the same object without being mistaken for authority.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RunObservation {
    pub run_id: Uuid,
    pub model_id: String,
    pub publication_id: String,
    pub state: RunState,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub phase: Option<RunPhase>,
    pub execution_control: ExecutionControl,
    #[serde(flatten)]
    pub details: BTreeMap<String, Value>,
}
impl RunObservation {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.run_id.is_nil()
            || self.model_id.is_empty()
            || self.publication_id.is_empty()
            || (self.state != RunState::Running && self.phase.is_some())
        {
            return Err("inconsistent run observation identity or phase");
        }
        let control = &self.execution_control;
        if (control.state == ExecutionControlState::Authorized
            && control.authorization_reason.is_some())
            || (matches!(
                self.state,
                RunState::Succeeded | RunState::Failed | RunState::Cancelled
            ) && control.state != ExecutionControlState::Completed)
        {
            return Err("execution control conflicts with run state or authority");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AnalysisIdentity {
    pub model_id: String,
    pub publication_id: String,
    pub purpose: RunPurpose,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub grant_id: Option<Uuid>,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub publication_delegation_id: Option<Uuid>,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub context_id: Option<String>,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub installation_status: Option<PublicationStatus>,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub installation: Option<InstallationDescriptor>,
    #[serde(flatten)]
    pub source_evidence: SourceEvidence,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SourceEvidence {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by_user_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<AnalysisOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation_identity: Option<InstallationIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<BTreeMap<String, EvidenceAvailability>>,
    // Recovery collection summaries retain their accepted destination as well.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disclosure: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct InputFileProgress {
    pub slot: String,
    pub source: String,
    pub size_bytes: u64,
    pub media_type: String,
    pub completed: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct InputCollectionProgress {
    pub slot: String,
    pub completed: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CollectionProgress {
    pub complete: bool,
    pub verified_files: usize,
    pub total_files: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CancellationProgress {
    pub attempt: CancellationAttempt,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub receipt: Option<CancellationReceipt>,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub error_code: Option<String>,
    #[serde(flatten)]
    pub details: BTreeMap<String, Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CancellationReceipt {
    pub run_id: Uuid,
    pub requested_by_user_id: Uuid,
    pub state: RunState,
    pub status: CancellationStatus,
    pub scope: CancellationScope,
    pub requested_at: String,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub reason: Option<String>,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub cancelled_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AnalysisSummary {
    pub schema_version: SchemaVersion,
    pub directory: String,
    #[serde(flatten)]
    pub identity: AnalysisIdentity,
    pub stage: AnalysisStage,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub run_id: Option<Uuid>,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub execution_location: Option<String>,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub disclosure: Option<String>,
    pub input_files: Vec<InputFileProgress>,
    pub input_collections: Vec<InputCollectionProgress>,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub receipt: Option<AcceptanceReceipt>,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub run: Option<RunObservation>,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub collection: Option<CollectionProgress>,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub cancellation: Option<CancellationProgress>,
    pub next_action: String,
}
impl AnalysisSummary {
    pub fn validate(&self) -> Result<(), &'static str> {
        if (self.stage == AnalysisStage::Accepted) != self.run_id.is_some() {
            return Err("analysis stage conflicts with accepted run identity");
        }
        if let Some(receipt) = &self.receipt
            && (Some(receipt.run_id) != self.run_id
                || receipt.model_id != self.identity.model_id
                || receipt.publication_id != self.identity.publication_id)
        {
            return Err("acceptance receipt conflicts with analysis identity");
        }
        if let Some(run) = &self.run {
            run.validate()?;
            if Some(run.run_id) != self.run_id
                || run.model_id != self.identity.model_id
                || run.publication_id != self.identity.publication_id
            {
                return Err("current run conflicts with analysis identity");
            }
        }
        if self
            .cancellation
            .as_ref()
            .and_then(|c| c.receipt.as_ref())
            .is_some_and(|receipt| Some(receipt.run_id) != self.run_id)
        {
            return Err("cancellation receipt conflicts with accepted run identity");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CollectedFile {
    pub artifact_id: Uuid,
    pub logical_name: String,
    pub media_type: String,
    pub version_id: String,
    pub size_bytes: u64,
    /// Immutable full-object SHA-256 descriptor.
    pub checksum: FullObjectChecksum,
    pub path: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FullObjectChecksum {
    pub algorithm: ChecksumAlgorithm,
    pub checksum_type: ChecksumType,
    #[cfg_attr(
        feature = "openapi",
        schema(
            min_length = 44,
            max_length = 44,
            pattern = "^[A-Za-z0-9+/]{42}[AEIMQUYcgkosw048]=$"
        )
    )]
    pub value: String,
}
string_enum!(ChecksumAlgorithm { Sha256 });
string_enum!(ChecksumType { FullObject });
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CollectedCollection {
    pub collection_id: Uuid,
    pub manifest_digest: String,
    pub manifest: String,
    pub root: String,
    pub complete: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AnalysisCollectionSummary {
    pub schema_version: SchemaVersion,
    pub directory: String,
    #[serde(flatten)]
    pub identity: AnalysisIdentity,
    pub run_id: Uuid,
    pub complete: bool,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub bundle: Option<String>,
    pub scientific_state: RunState,
    pub outputs: BTreeMap<String, CollectedFile>,
    pub collections: BTreeMap<String, CollectedCollection>,
}
impl AnalysisCollectionSummary {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.complete != self.bundle.is_some()
            || !matches!(
                self.scientific_state,
                RunState::Succeeded | RunState::Failed | RunState::Cancelled
            )
            || (self.complete && self.collections.values().any(|c| !c.complete))
        {
            return Err("collection completeness conflicts with bundle or scientific state");
        }
        if self.outputs.values().any(|output| {
            output.artifact_id.is_nil()
                || output.version_id.is_empty()
                || !crate::collections::valid_checksum(&output.checksum.value)
        }) || self.collections.values().any(|collection| {
            collection.collection_id.is_nil()
                || !crate::collections::valid_checksum(&collection.manifest_digest)
        }) {
            return Err("collected output omitted a valid immutable identity or checksum");
        }
        Ok(())
    }
}

/// Scientific comparison evidence. Control decisions use the typed stage,
/// authority, guard and uncertainty fields rather than arbitrary scientific JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReleaseComparison {
    pub current: Value,
    pub candidate: Value,
    pub observed_current: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReleaseDelegationAttempt {
    pub id: Uuid,
    pub user_id: Option<Uuid>,
    pub expected_generation: i64,
    pub response: Option<crate::management::PublicationDelegation>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReleaseSummary {
    pub schema_version: SchemaVersion,
    pub directory: String,
    pub operation_id: Uuid,
    pub model_id: String,
    pub publication_id: String,
    pub stage: ReleaseStage,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub candidate_status: Option<PublicationStatus>,
    pub blockers: Vec<String>,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub validation_message: Option<String>,
    pub model_generation: i64,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub release_authority: Option<PublicationReleaseAuthority>,
    pub delegation_attempts: Vec<ReleaseDelegationAttempt>,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub current_publication_id: Option<String>,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub expected_current_publication_id: Option<String>,
    pub guard_changed: bool,
    pub candidate_is_current: bool,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub activation_receipt: Option<PublicationActivationReceipt>,
    pub activation_uncertain: bool,
    pub registration_uncertain: bool,
    pub comparison: ReleaseComparison,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub execution_location: Option<String>,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub disclosure: Option<String>,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub trial_availability: Option<Availability>,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub trial_evidence: Option<PublicationTrialEvidence>,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub trial_init_command: Option<String>,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub rollback_source: Option<Value>,
    pub next_command: String,
    pub next_action: String,
}
impl ReleaseSummary {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.operation_id.is_nil()
            || self.model_id.is_empty()
            || self.publication_id.is_empty()
            || (matches!(self.stage, ReleaseStage::Ready | ReleaseStage::Reviewed)
                && (self.candidate_status != Some(PublicationStatus::Ready)
                    || !self.blockers.is_empty()))
            || (self.stage == ReleaseStage::Reviewed && self.guard_changed)
            || (self.stage == ReleaseStage::Blocked && self.blockers.is_empty())
        {
            return Err("release stage conflicts with candidate, blockers or review guard");
        }
        if self.trial_evidence.as_ref().is_some_and(|evidence| {
            evidence
                .latest_run
                .iter()
                .chain(evidence.latest_succeeded_run.iter())
                .any(|run| run.publication_id != self.publication_id)
        }) {
            return Err("trial evidence belongs to another candidate");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CliErrorDetail {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CliRuntimeError {
    pub schema_version: SchemaVersion,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub status: Option<u16>,
    pub error: CliErrorDetail,
    #[serde(deserialize_with = "required_option")]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub retry_after: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn analysis() -> Value {
        json!({"schema_version":1,"directory":"analysis","model_id":"example","publication_id":"release-one","purpose":"standard",
            "grant_id":null,"publication_delegation_id":null,"context_id":"context","installation_status":null,"installation":null,
            "stage":"editable","run_id":null,"execution_location":"Research compute","disclosure":"Reviewed disclosure.",
            "input_files":[],"input_collections":[],"receipt":null,"run":null,"collection":null,"cancellation":null,"next_action":"Review before submission."})
    }

    #[test]
    fn automation_decisions_require_version_fields_types_and_known_enums() {
        for key in [
            "schema_version",
            "stage",
            "run_id",
            "receipt",
            "run",
            "collection",
            "cancellation",
            "publication_id",
        ] {
            let mut value = analysis();
            value.as_object_mut().unwrap().remove(key);
            assert!(
                serde_json::from_value::<AnalysisSummary>(value).is_err(),
                "missing {key}"
            );
        }
        for (key, value) in [
            ("schema_version", json!(2)),
            ("stage", json!("future_stage")),
            ("run_id", json!(true)),
            ("purpose", json!("rerun")),
        ] {
            let mut document = analysis();
            document[key] = value;
            assert!(
                serde_json::from_value::<AnalysisSummary>(document).is_err(),
                "unexpected {key}"
            );
        }
        let mut value = analysis();
        value["future_diagnostic"] = json!({"value":true});
        serde_json::from_value::<AnalysisSummary>(value)
            .unwrap()
            .validate()
            .unwrap();
    }

    #[test]
    fn accepted_identity_and_original_receipt_are_not_current_status() {
        let mut value = analysis();
        value["stage"] = json!("accepted");
        value["run_id"] = json!("33333333-3333-4333-8333-333333333333");
        value["receipt"] = json!({"run_id":value["run_id"],"model_id":"example","publication_id":"release-one","purpose":"standard","state":"queued","created_at":"2026-11-02T10:00:00Z"});
        value["run"] = json!({"run_id":value["run_id"],"model_id":"example","publication_id":"release-one","state":"succeeded","phase":null,"execution_control":{"state":"completed","authorization_reason":null}});
        serde_json::from_value::<AnalysisSummary>(value.clone())
            .unwrap()
            .validate()
            .unwrap();
        value["run"]["publication_id"] = json!("another-publication");
        assert!(
            serde_json::from_value::<AnalysisSummary>(value)
                .unwrap()
                .validate()
                .is_err()
        );
    }

    #[test]
    fn run_phase_and_control_cannot_teach_contradictory_observations() {
        let mut run = json!({"run_id":"33333333-3333-4333-8333-333333333333","model_id":"example","publication_id":"release-one","state":"queued","phase":"preparing","execution_control":{"state":"authorized","authorization_reason":null}});
        assert!(
            serde_json::from_value::<RunObservation>(run.clone())
                .unwrap()
                .validate()
                .is_err()
        );
        run["phase"] = Value::Null;
        run["execution_control"] =
            json!({"state":"authorized","authorization_reason":"publication_revoked"});
        assert!(
            serde_json::from_value::<RunObservation>(run)
                .unwrap()
                .validate()
                .is_err()
        );
    }

    #[test]
    fn successful_collection_step_does_not_imply_scientific_success_or_full_outputs() {
        let mut value = analysis();
        for key in [
            "stage",
            "receipt",
            "run",
            "run_id",
            "execution_location",
            "disclosure",
            "input_files",
            "input_collections",
            "collection",
            "cancellation",
            "next_action",
        ] {
            value.as_object_mut().unwrap().remove(key);
        }
        value["run_id"] = json!("33333333-3333-4333-8333-333333333333");
        value["complete"] = json!(false);
        value["bundle"] = Value::Null;
        value["scientific_state"] = json!("failed");
        value["outputs"] = json!({});
        value["collections"] = json!({});
        serde_json::from_value::<AnalysisCollectionSummary>(value.clone())
            .unwrap()
            .validate()
            .unwrap();
        value["complete"] = json!(true);
        assert!(
            serde_json::from_value::<AnalysisCollectionSummary>(value.clone())
                .unwrap()
                .validate()
                .is_err()
        );
        value["bundle"] = json!("analysis/bundle.json");
        serde_json::from_value::<AnalysisCollectionSummary>(value)
            .unwrap()
            .validate()
            .unwrap();
    }

    #[test]
    fn runtime_errors_require_the_version_status_and_retry_contract() {
        let value = json!({"schema_version":1,"status":null,"error":{"code":"client_error","message":"Invalid saved evidence."},"retry_after":null});
        serde_json::from_value::<CliRuntimeError>(value.clone()).unwrap();
        for key in ["schema_version", "status", "retry_after"] {
            let mut incomplete = value.clone();
            incomplete.as_object_mut().unwrap().remove(key);
            assert!(serde_json::from_value::<CliRuntimeError>(incomplete).is_err());
        }
    }

    #[test]
    fn release_automation_rejects_missing_guards_unknown_stages_and_wrong_trial_identity() {
        let transcript: Value = serde_json::from_str(include_str!(
            "../../cli/examples/automation/release-ready.json"
        ))
        .unwrap();
        let value = transcript["stdout"].clone();
        serde_json::from_value::<ReleaseSummary>(value.clone())
            .unwrap()
            .validate()
            .unwrap();
        for field in [
            "guard_changed",
            "activation_uncertain",
            "registration_uncertain",
            "current_publication_id",
            "release_authority",
        ] {
            let mut missing = value.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(
                serde_json::from_value::<ReleaseSummary>(missing).is_err(),
                "missing {field}"
            );
        }
        let mut unknown = value.clone();
        unknown["stage"] = json!("future_stage");
        assert!(serde_json::from_value::<ReleaseSummary>(unknown).is_err());
        let mut conflicting = value;
        conflicting["trial_evidence"]["latest_run"] = json!({"run_id":"33333333-3333-4333-8333-333333333333","publication_id":"another-candidate","purpose":"candidate_trial","state":"succeeded","created_at":"2026-11-02T10:00:00Z"});
        assert!(
            serde_json::from_value::<ReleaseSummary>(conflicting)
                .unwrap()
                .validate()
                .is_err()
        );
    }
}
