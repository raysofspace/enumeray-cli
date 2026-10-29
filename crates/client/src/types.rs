//! Client-owned wire types. They do not depend on the server implementation.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiFailure {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    pub issuer: String,
    pub client_id: String,
    pub audience: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub scopes: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct OAuthTokens {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    pub token_type: String,
    #[serde(default)]
    pub expires_in: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Identity {
    pub user_id: Uuid,
    pub label: String,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub credential_scopes: Vec<String>,
    #[serde(default)]
    pub is_platform_admin: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RunSubmission {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication_delegation_id: Option<uuid::Uuid>,
    #[serde(
        default,
        skip_serializing_if = "enumeray_contracts::runs::RunPurpose::is_standard"
    )]
    pub purpose: enumeray_contracts::runs::RunPurpose,
    pub model_id: String,
    pub expected_publication_id: String,
    pub inputs: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmissionReceipt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication_delegation_id: Option<Uuid>,
    #[serde(default)]
    pub purpose: enumeray_contracts::runs::RunPurpose,
    pub run_id: Uuid,
    pub model_id: String,

    pub publication_id: String,
    pub state: String,
    pub created_at: String,
}

/// Persist this before issuing the first POST. Retries never rediscover a model.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedRun {
    pub schema_version: u32,
    pub api_url: String,
    pub user_id: Uuid,
    pub idempotency_key: String,
    pub request: RunSubmission,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadChecksum {
    pub algorithm: String,
    pub checksum_type: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadRequest {
    pub logical_name: String,
    pub media_type: String,
    pub scientific_metadata: Value,
    pub expected_size_bytes: u64,
    pub checksum: UploadChecksum,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadReceipt {
    pub upload_id: Uuid,
    pub artifact_id: Uuid,
}

#[derive(Clone, Deserialize)]
pub struct UploadView {
    pub state: String,
    pub reservation: UploadReceipt,
    pub transfer: UploadTransfer,
    pub completion: Option<Value>,
}

#[derive(Clone, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum UploadTransfer {
    Available { request: ScopedTransfer },
    Unavailable,
    Multipart { progress: MultipartProgress },
    Expired,
    Completed,
}

#[derive(Clone, Deserialize)]
pub struct ScopedTransfer {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MultipartProgress {
    pub phase: String,
    pub part_size_bytes: u64,
    pub part_count: i32,
    pub confirmed_bytes: u64,
    pub diagnostic: Option<String>,
}
#[derive(Debug, Clone, Deserialize)]
pub struct UploadCompletionReceipt {
    pub upload_id: Uuid,
    pub artifact_id: Uuid,
    pub version_id: String,
    pub size_bytes: u64,
    pub checksum: UploadChecksum,
    pub completed_at: String,
}
/// Both successful completion responses: HTTP 200 receipt or HTTP 202 progress.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum UploadCompletion {
    Completed(UploadCompletionReceipt),
    Pending(MultipartProgress),
}
#[derive(Clone, Deserialize)]
pub struct PartAuthorization {
    pub part_number: i32,
    pub size_bytes: u64,
    pub checksum_sha256: String,
    pub request: ScopedTransfer,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UploadPart {
    pub part_number: i32,
    pub size_bytes: u64,
    pub checksum_sha256: String,
    pub etag: Option<String>,
}
#[derive(Deserialize)]
pub struct UploadParts {
    pub parts: Vec<UploadPart>,
    pub next_after: Option<i32>,
}
