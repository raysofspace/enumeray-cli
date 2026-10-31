//! Host setup and diagnostic contracts. A passed probe never authorizes model execution.
use crate::execution::{ConnectionConfig, DriverConfig, ExecutionContextDefinition};
use crate::management::ContextCreate;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use uuid::Uuid;

pub const MAX_REPORT_BYTES: usize = 256 * 1024;
pub const MAX_CHECKS: usize = 128;
pub const ATTEMPT_SECONDS: u64 = 120;
pub const REQUEST_SECONDS: i64 = 900;
pub const REPORT_FRESH_SECONDS: i64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostPlatform {
    Linux,
    Macos,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostAccount {
    pub platform: HostPlatform,
    pub name: String,
    pub group: String,
    pub home: String,
    pub create: bool,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostSetupSpec {
    pub manifest_version: u32,
    pub api_base_url: String,
    pub context: ContextCreate,
    pub connector_account: HostAccount,
    pub execution_account: HostAccount,
    pub connector_executable: String,
    /// Reviewed local source for an atomic managed installation, never downloaded by setup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable_sha256: Option<String>,
    /// SSH execution hosts may need another native artifact (platform or CPU).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_executable_sha256: Option<String>,
    pub journal: String,
    pub token_file: String,
    pub credentials_file: Option<String>,
    pub max_workers: u32,
    pub max_transfer_workers: u32,
    pub max_pending_assignments: u32,
    pub shutdown_timeout_seconds: u64,
    /// Explicit approval to install native background service configuration.
    #[serde(default)]
    pub install_service: bool,
}

/// Host runtime configuration contains references to private credentials, never their values.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorRuntimeConfiguration {
    pub manifest_version: u32,
    pub spec: HostSetupSpec,
    pub plan_sha256: String,
    pub allow_development_execution: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostConfigurationPlan {
    pub manifest_version: u32,
    pub spec: HostSetupSpec,
    pub actions: Vec<HostAction>,
    pub files: Vec<PlannedHostFile>,
    pub foreground_argv: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostAction {
    pub host: HostSide,
    pub responsible_role: ResponsibleRole,
    pub kind: HostActionKind,
    pub target: String,
    pub prerequisites: Vec<HostPrerequisite>,
    pub instructions: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostPrerequisite {
    pub host: HostSide,
    pub kind: HostPrerequisiteKind,
    pub target: String,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostPrerequisiteKind {
    ReviewedIdentity,
    Account,
    Executable,
    PrivateState,
    TrustedHostKey,
    Enrollment,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostSide {
    Connector,
    Execution,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostActionKind {
    Account,
    Directory,
    Launcher,
    Service,
    Ssh,
    Executable,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedHostFile {
    pub name: String,
    pub host: HostSide,
    pub destination: String,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ResponsibleRole {
    ConnectorOwner,
    HostAdministrator,
    SiteOperator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum CheckOutcome {
    Passed,
    Failed,
    Unknown,
    NotApplicable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct HostCheck {
    pub check: String,
    pub outcome: CheckOutcome,
    pub required: bool,
    pub code: String,
    pub responsible_role: ResponsibleRole,
    pub correction: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ReportSource {
    Local,
    Connector,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct HostPreflightReport {
    pub manifest_version: u32,
    pub request_id: Uuid,
    pub connector_id: String,
    pub context_id: String,
    pub definition_hash: String,
    pub execution_mode: String,
    pub observed_at: String,
    pub source: ReportSource,
    pub execution_uid: Option<String>,
    #[serde(default)]
    pub probe_id: Option<Uuid>,
    pub checks: Vec<HostCheck>,
    pub limitations: Vec<String>,
}
impl HostPreflightReport {
    pub fn ready(&self) -> bool {
        self.checks.iter().any(|c| c.required)
            && self
                .checks
                .iter()
                .filter(|c| c.required)
                .all(|c| c.outcome == CheckOutcome::Passed)
    }
    pub fn validate(&self, definition: &ExecutionContextDefinition) -> Result<(), &'static str> {
        let encoded = serde_json::to_vec(self).map_err(|_| "invalid preflight report")?;
        if self.manifest_version != 1
            || self.request_id.is_nil()
            || encoded.len() > MAX_REPORT_BYTES
            || self.checks.len() > MAX_CHECKS
            || self.definition_hash != definition_hash(definition)?
            || self.execution_mode != mode_name(definition)
            || self.observed_at.len() > 40
            || self.limitations.len() > 16
            || self.limitations.iter().any(|s| !safe_text(s, 1000))
            || self
                .execution_uid
                .as_ref()
                .is_some_and(|uid| uid.len() > 10 || uid.parse::<u32>().is_err())
        {
            return Err("invalid preflight report bounds or execution identity");
        }
        let mut names = BTreeSet::new();
        for check in &self.checks {
            if !names.insert(check.check.as_str())
                || !safe_code(&check.check)
                || !safe_code(&check.code)
                || !safe_text(&check.correction, 1000)
            {
                return Err("invalid preflight check");
            }
        }
        for required in required_checks(definition) {
            if !self.checks.iter().any(|c| {
                c.check == required && c.required && c.outcome != CheckOutcome::NotApplicable
            }) {
                return Err("preflight report omits a required host check");
            }
        }
        if self.ready()
            && self
                .execution_uid
                .as_ref()
                .is_none_or(|uid| uid.parse::<u32>().map_or(true, |uid| uid == 0))
        {
            return Err("ready host report requires a non-root execution UID");
        }
        Ok(())
    }
}
fn safe_code(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}
fn safe_text(s: &str, max: usize) -> bool {
    s.len() <= max && !s.chars().any(char::is_control)
}
pub fn mode_name(definition: &ExecutionContextDefinition) -> String {
    match definition.execution_mode {
        crate::execution::ExecutionMode::Managed => "managed",
        crate::execution::ExecutionMode::Development => "development",
    }
    .into()
}
pub fn definition_hash(definition: &ExecutionContextDefinition) -> Result<String, &'static str> {
    let bytes = serde_json::to_vec(definition).map_err(|_| "cannot encode execution definition")?;
    Ok(Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}
pub fn required_checks(definition: &ExecutionContextDefinition) -> Vec<&'static str> {
    let mut checks = vec![
        "configuration",
        "connector_identity",
        "connector_paths",
        "runner",
        "execution_identity",
        "installation_roots",
        "work_root",
        "executable_isolation",
        "credential_isolation",
    ];
    if matches!(definition.connection, ConnectionConfig::Ssh(_)) {
        checks.extend(["ssh_credentials", "ssh", "sftp"]);
    }
    if matches!(definition.driver, DriverConfig::Slurm(_)) {
        checks.extend(["slurm_tools", "slurm_queue", "slurm_accounting"]);
    }
    if definition.shared_workspace.is_some() {
        checks.push("shared_workspace");
    }
    checks
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct PreflightRequest {
    pub expected_previous_request_id: Option<Uuid>,
    pub expected_ownership_generation: i64,
    pub definition_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct PreflightAttempt {
    pub request_id: Uuid,
    pub requested_at: String,
    pub expires_at: String,
    pub state: AttemptState,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    Pending,
    Completed,
    Expired,
    Cancelled,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ReportValidity {
    Current,
    Stale,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ContextPreflightStatus {
    pub connector_id: String,
    pub context_id: String,
    pub definition_hash: String,
    pub ownership_generation: i64,
    pub attempt: Option<PreflightAttempt>,
    pub report: Option<HostPreflightReport>,
    pub validity: ReportValidity,
    pub reason: String,
    pub ready: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct PendingPreflight {
    pub request_id: Uuid,
    pub connector_id: String,
    pub context_id: String,
    #[cfg_attr(feature = "openapi", schema(schema_with = crate::management::execution_context_schema))]
    pub definition: ExecutionContextDefinition,
    pub ownership_generation: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct PreflightSubmission {
    pub session_id: Uuid,
    pub report: HostPreflightReport,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PreflightSummary {
    pub pending: i64,
    pub current_ready: i64,
    pub current_blocked: i64,
    pub stale_or_unavailable: i64,
}

/// Recent authenticated contact is independent of process-session ownership.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ConnectorContact {
    pub assessed_at: String,
    pub last_contact_at: Option<String>,
    pub age_seconds: Option<i64>,
    pub freshness: ReportValidity,
    pub credential_current: bool,
}

/// An open session has no time-based expiry and does not prove a process is alive.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ConnectorSessionStatus {
    pub open: bool,
    pub acquired_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SetupIssue {
    pub code: String,
    pub check: Option<String>,
    pub outcome: CheckOutcome,
    pub responsible_role: ResponsibleRole,
    pub correction: String,
}

/// Setup prerequisites, never publication, grant or scientific qualification.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ContextSetupReadiness {
    pub context_id: String,
    pub lifecycle_state: crate::management::ContextLifecycleState,
    pub archived: bool,
    pub configured: bool,
    pub connected: bool,
    pub ready: bool,
    pub preflight: ContextPreflightStatus,
    pub issues: Vec<SetupIssue>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn required_checks_follow_backend_and_connection_without_model_identity() {
        let file: serde_json::Value =
            serde_json::from_str(include_str!("../../cli/examples/slurm-ssh-context.json"))
                .unwrap();
        let context: ExecutionContextDefinition =
            serde_json::from_value(file["spec"]["definition"].clone()).unwrap();
        assert!(required_checks(&context).contains(&"sftp"));
        assert!(required_checks(&context).contains(&"slurm_accounting"));
        assert_eq!(definition_hash(&context).unwrap().len(), 64);
    }
}
