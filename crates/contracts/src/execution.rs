use serde::{Deserialize, Serialize};

/// A stable provider identity. Credentials and live observations are separate.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorRegistration {
    pub id: String,
    pub label: String,
    /// Bounds unfinished assignments across all publication installations; claims enforce it.
    pub max_outstanding_assignments: u32,
}

/// Execution mechanisms, independent of the wrapper's request/result protocol.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionDriver {
    Process,
    Slurm,
}

/// Explicit capabilities understood by this registry version.
/// ArtifactInputs promises verified staging; ArtifactOutputs promises collection
/// and verified publication of declared output files.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    JsonInputs,
    JsonOutputs,
    ArtifactInputs,
    ArtifactOutputs,
    LargeArtifacts,
    CollectionInputs,
    CollectionOutputs,
}

/// Requirements derived from an immutable context and scientific contract.
/// These are execution needs, not observations of backend support.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionRequirements {
    pub driver: ExecutionDriver,
    pub capabilities: Vec<Capability>,
    pub wrapper_protocol_version: u32,
}

impl ExecutionRequirements {
    pub fn derive(
        driver: ExecutionDriver,
        definition: &crate::models::ScientificDefinition,
    ) -> Self {
        let mut capabilities = vec![Capability::JsonInputs, Capability::JsonOutputs];
        if crate::models::requires_artifact_inputs(definition) {
            capabilities.push(Capability::ArtifactInputs);
        }
        if crate::models::requires_artifact_outputs(definition) {
            capabilities.push(Capability::ArtifactOutputs);
        }
        if crate::collections::requires(definition, false) {
            capabilities.push(Capability::CollectionInputs);
        }
        if crate::collections::requires(definition, true) {
            capabilities.push(Capability::CollectionOutputs);
        }
        if crate::models::files::requires_large_artifacts(definition) {
            capabilities.push(Capability::LargeArtifacts);
        }
        Self {
            driver,
            capabilities,
            wrapper_protocol_version: definition.wrapper_protocol_version,
        }
    }
}

/// Identity claims derived from the retained scientific definition. They do not
/// independently attest the provider's installed software.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InstallationIdentity {
    pub model_identity: String,
    pub wrapper_identity: String,
    pub runtime_identity: String,
    pub wrapper_protocol_version: u32,
    pub dataset_identities: Vec<String>,
}

impl InstallationIdentity {
    pub fn derive(definition: &crate::models::ScientificDefinition) -> Self {
        Self {
            model_identity: definition.model_identity.clone(),
            wrapper_identity: definition.wrapper_identity.clone(),
            runtime_identity: definition.runtime_identity.clone(),
            wrapper_protocol_version: definition.wrapper_protocol_version,
            dataset_identities: definition.dataset_identities.clone(),
        }
    }
}

/// Isolation policy approved by the context owner; development also needs local host consent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    #[default]
    Managed,
    Development,
}

impl ExecutionMode {
    pub fn is_managed(&self) -> bool {
        *self == Self::Managed
    }

    pub fn validate(
        &self,
        driver: &DriverConfig,
        connection: &ConnectionConfig,
    ) -> Result<(), &'static str> {
        if *self == Self::Development
            && !matches!(
                (driver, connection),
                (
                    DriverConfig::Process { .. },
                    ConnectionConfig::Direct { .. }
                )
            )
        {
            return Err(
                "development execution requires the process driver and a direct connection",
            );
        }
        Ok(())
    }
}

/// Immutable execution settings approved by the infrastructure owner. Credential
/// references name host secrets; secret values must never be sent through the API.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionContextDefinition {
    #[serde(default, skip_serializing_if = "ExecutionMode::is_managed")]
    pub execution_mode: ExecutionMode,
    pub driver: DriverConfig,
    pub connection: ConnectionConfig,
    pub installation_roots: Vec<String>,
    pub work_root: String,
    pub max_outstanding_assignments: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_workspace: Option<SharedWorkspace>,
}

/// Infrastructure-approved mapping of a runner work root into the connector filesystem.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SharedWorkspace {
    pub runner_root: String,
    pub connector_root: String,
}
impl SharedWorkspace {
    pub fn validate(&self, work_root: &str) -> Result<(), &'static str> {
        fn safe(s: &str) -> bool {
            std::path::Path::new(s).is_absolute()
                && s.len() <= 4096
                && !s.contains(['\0', '\n', '\r'])
                && !std::path::Path::new(s)
                    .components()
                    .any(|p| matches!(p, std::path::Component::ParentDir))
        }
        if !safe(&self.runner_root)
            || !safe(&self.connector_root)
            || std::path::Path::new(&self.runner_root) != std::path::Path::new(work_root)
        {
            return Err(
                "shared workspace requires absolute roots and the configured runner work root",
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SshTarget {
    pub host: String,
    pub user: String,
    pub port: u16,
    pub credential_reference: String,
    /// Absolute path to the independently installed connector binary.
    pub runner_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SlurmConfig {
    /// Absolute path to the connector binary visible on compute nodes.
    pub worker_path: String,
}

/// Infrastructure-owned limits on all process workspace files, independent of artifacts/logs.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessScratchPolicy {
    pub max_bytes: u64,
    pub max_entries: u32,
    pub max_depth: u8,
    pub scan_interval_ms: u32,
    pub cache: ProcessCache,
    pub symlinks: ScratchSymlinks,
}
impl ProcessScratchPolicy {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.max_bytes == 0
            || !(1..=1_000_000).contains(&self.max_entries)
            || !(1..=64).contains(&self.max_depth)
            || !(100..=60_000).contains(&self.scan_interval_ms)
        {
            return Err("invalid process scratch limits");
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessCache {
    None,
    PerRun,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScratchSymlinks {
    Forbid,
    WorkspaceOnly,
}

fn scratch_object<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<ProcessScratchPolicy>, D::Error> {
    struct Object;
    impl<'de> serde::de::Visitor<'de> for Object {
        type Value = ProcessScratchPolicy;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a process scratch policy object")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
            ProcessScratchPolicy::deserialize(serde::de::value::MapAccessDeserializer::new(map))
        }
    }
    deserializer.deserialize_map(Object).map(Some)
}

/// Backend configuration is independent of connection locality.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DriverConfig {
    Process {
        /// Missing only in retained legacy contexts and execution snapshots.
        #[serde(
            default,
            deserialize_with = "scratch_object",
            skip_serializing_if = "Option::is_none"
        )]
        scratch: Option<ProcessScratchPolicy>,
    },
    Slurm(SlurmConfig),
}
impl DriverConfig {
    /// Legacy contexts retain their original environment; configured process paths are runner-owned.
    pub fn validate_environment(
        &self,
        environment: &std::collections::BTreeMap<String, String>,
    ) -> Result<(), String> {
        validate_installation_environment(environment)?;
        if matches!(self, Self::Process { scratch: Some(_) }) {
            validate_process_environment(environment).map_err(str::to_owned)?;
        }
        Ok(())
    }
    pub fn kind(&self) -> ExecutionDriver {
        match self {
            Self::Process { .. } => ExecutionDriver::Process,
            Self::Slurm(_) => ExecutionDriver::Slurm,
        }
    }
}

/// Only the connector resolves credentials; this configuration is never sent to a worker.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConnectionConfig {
    Direct { launcher: Vec<String> },
    Ssh(SshTarget),
}
impl ExecutionContextDefinition {
    /// Structural checks shared by registration and installation validation.
    pub fn validate_execution(&self) -> Result<(), &'static str> {
        self.execution_mode
            .validate(&self.driver, &self.connection)?;
        fn absolute(s: &str) -> bool {
            std::path::Path::new(s).is_absolute()
                && s.len() <= 4096
                && !s.contains(['\0', '\n', '\r'])
                && !std::path::Path::new(s)
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
        }
        if let DriverConfig::Process {
            scratch: Some(policy),
        } = &self.driver
        {
            policy.validate()?;
        }
        if let Some(mapping) = &self.shared_workspace {
            mapping.validate(&self.work_root)?;
        }
        if self.installation_roots.is_empty()
            || self.installation_roots.len() > 32
            || self.installation_roots.iter().any(|root| {
                !absolute(root) || std::path::Path::new(root) == std::path::Path::new("/")
            })
        {
            return Err(
                "installation roots require 1–32 bounded absolute directories without parent traversal",
            );
        }
        match &self.connection {
            ConnectionConfig::Direct { launcher } => {
                if launcher.is_empty()
                    || launcher.len() > 32
                    || !absolute(&launcher[0])
                    || launcher
                        .iter()
                        .any(|s| s.is_empty() || s.len() > 4096 || s.contains('\0'))
                {
                    return Err(
                        "direct connection requires an absolute restricted runner launcher",
                    );
                }
            }
            ConnectionConfig::Ssh(target) => {
                if target.host.is_empty()
                    || target.host.starts_with('-')
                    || !target
                        .host
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b".-_:".contains(&b))
                    || target.user.is_empty()
                    || !target
                        .user
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
                    || target.port == 0
                    || target.credential_reference.is_empty()
                    || target.credential_reference.len() > 4096
                    || target.credential_reference.chars().any(char::is_control)
                    || !absolute(&target.runner_path)
                {
                    return Err(
                        "SSH connection requires a valid host, account, credential reference and absolute runner path",
                    );
                }
            }
        }
        if let DriverConfig::Slurm(config) = &self.driver
            && !absolute(&config.worker_path)
        {
            return Err("Slurm requires an absolute compute-node worker path");
        }
        if !absolute(&self.work_root) {
            return Err("invalid work root");
        }
        Ok(())
    }
}

/// Author-supplied physical installation settings, constrained by the selected context.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InstallationSpec {
    pub release_path: String,
    /// First argument is an executable path relative to release_path.
    pub entrypoint: Vec<String>,
    /// Model-supplied batch file relative to release_path; required for Slurm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slurm_script: Option<String>,
    #[serde(default)]
    pub environment: std::collections::BTreeMap<String, String>,
}

impl InstallationSpec {
    /// Structural validation requires no API access or installed-content inspection.
    pub fn validate(&self) -> Result<(), String> {
        use std::path::{Component, Path};
        let release = Path::new(&self.release_path);
        if self.release_path.len() > 4096
            || self.release_path.contains(['\0', '\n', '\r'])
            || !release.is_absolute()
            || release
                .components()
                .any(|part| matches!(part, Component::ParentDir))
        {
            return Err(
                "installation requires an absolute immutable release directory without parent traversal".into(),
            );
        }
        if self.entrypoint.is_empty()
            || self.entrypoint.len() > 32
            || self
                .entrypoint
                .iter()
                .any(|s| s.is_empty() || s.len() > 4096 || s.contains('\0'))
            || !Path::new(&self.entrypoint[0])
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
        {
            return Err(
                "installation requires a release-relative entrypoint and bounded environment entries".into(),
            );
        }
        validate_installation_environment(&self.environment)?;
        if let Some(script) = self.slurm_script.as_deref()
            && !valid_slurm_script(script)
        {
            return Err(
                "slurm_script must name a batch file relative to the release directory without parent traversal".into(),
            );
        }
        Ok(())
    }

    /// Validate scheduler-file shape against the driver selected by the API context.
    pub fn validate_driver(&self, driver: ExecutionDriver) -> Result<(), &'static str> {
        validate_installation_driver(driver, self.slurm_script.as_deref())
    }

    /// Registration resolves the internal key and derives identity from scientific data.
    pub fn resolve(
        self,
        provider_binding_id: String,
        definition: &crate::models::ScientificDefinition,
    ) -> InstallationDescriptor {
        InstallationDescriptor {
            provider_binding_id,
            installation: InstallationIdentity::derive(definition),
            release_path: self.release_path,
            entrypoint: self.entrypoint,
            slurm_script: self.slurm_script,
            environment: self.environment,
        }
    }
}

/// Validate author-supplied entries without restricting the inherited runtime environment.
/// Scheduler and MPI namespaces belong to the batch/runtime setup for every driver.
pub fn validate_installation_environment(
    environment: &std::collections::BTreeMap<String, String>,
) -> Result<(), String> {
    if environment.len() > 32 {
        return Err("installation environment exceeds the 32-entry limit".into());
    }
    for (key, value) in environment {
        if key.is_empty()
            || key.len() > 128
            || key.contains(['=', '\0'])
            || value.len() > 4096
            || value.contains('\0')
        {
            return Err("installation environment requires bounded keys and values without NUL bytes or '=' in keys".into());
        }
        if matches!(key.as_str(), "HOME" | "PATH" | "PYTHONDONTWRITEBYTECODE")
            || ["SLURM_", "PMI_", "PMIX_", "LD_", "DYLD_"]
                .iter()
                .any(|prefix| key.starts_with(prefix))
        {
            return Err(format!(
                "installation environment key {key:?} is reserved for the runner or inherited runtime"
            ));
        }
    }
    Ok(())
}

/// Paths for process temporary files and caches belong to the infrastructure policy.
pub fn validate_process_environment(
    environment: &std::collections::BTreeMap<String, String>,
) -> Result<(), &'static str> {
    if environment
        .keys()
        .any(|key| matches!(key.as_str(), "TMPDIR" | "TMP" | "TEMP" | "XDG_CACHE_HOME"))
    {
        return Err("process temporary/cache environment belongs to the runner");
    }
    Ok(())
}

fn valid_slurm_script(script: &str) -> bool {
    !script.is_empty()
        && script.len() <= 4096
        && !script.contains(['\0', '\n', '\r'])
        && std::path::Path::new(script)
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
}

fn validate_installation_driver(
    driver: ExecutionDriver,
    slurm_script: Option<&str>,
) -> Result<(), &'static str> {
    match (driver, slurm_script) {
        (ExecutionDriver::Slurm, Some(script)) if valid_slurm_script(script) => Ok(()),
        (ExecutionDriver::Process, None) => Ok(()),
        _ => Err(
            "Slurm installations require a batch file relative to their release directory; process installations must omit slurm_script",
        ),
    }
}

/// Internal registration snapshot used by connector configuration and execution evidence.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InstallationDescriptor {
    pub provider_binding_id: String,
    pub installation: InstallationIdentity,
    pub release_path: String,
    /// First argument is an executable path relative to release_path.
    pub entrypoint: Vec<String>,
    /// Model-supplied batch file relative to release_path; required for Slurm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slurm_script: Option<String>,
    #[serde(default)]
    pub environment: std::collections::BTreeMap<String, String>,
}

impl InstallationDescriptor {
    pub fn spec(&self) -> InstallationSpec {
        InstallationSpec {
            release_path: self.release_path.clone(),
            entrypoint: self.entrypoint.clone(),
            slurm_script: self.slurm_script.clone(),
            environment: self.environment.clone(),
        }
    }

    /// Validate the model-owned scheduler file without interpreting its directives.
    pub fn validate_driver(&self, driver: ExecutionDriver) -> Result<(), &'static str> {
        validate_installation_driver(driver, self.slurm_script.as_deref())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DesiredContext {
    pub id: String,
    pub definition: ExecutionContextDefinition,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DesiredPublication {
    pub publication_id: String,
    pub context_id: String,
    pub generation: u64,
    pub descriptor: InstallationDescriptor,
    pub requirements: ExecutionRequirements,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DesiredConfiguration {
    pub generation: u64,
    pub contexts: Vec<DesiredContext>,
    pub publications: Vec<DesiredPublication>,
    /// New release directories and declared executable/batch files cannot occupy
    /// a reserved work_root/UUID tree.
    /// Missing capability on an older service grants no payload-cleanup authority.
    #[serde(default)]
    pub run_payload_namespace_reserved: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationValidation {
    pub publication_id: String,
    pub generation: u64,
    pub ready: bool,
    pub message: Option<String>,
    /// Connector-observed entrypoint content hashes; separate from provider claims.
    #[serde(default)]
    /// Absolute entrypoint paths mapped to independently observed lowercase hex SHA256.
    pub verified_digests: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationAcknowledgment {
    pub validations: Vec<PublicationValidation>,
}

pub const MAX_RETENTION_BINDINGS: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedInstallationKey {
    pub context_id: String,
    pub provider_binding_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationRetentionRequest {
    pub bindings: Vec<RetainedInstallationKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallationDispositionState {
    Eligible,
    RetainedReference,
    WorkOrDeliveryPinned,
    Unmanaged,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InstallationDisposition {
    pub context_id: String,
    pub provider_binding_id: String,
    pub state: InstallationDispositionState,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationRetentionReceipt {
    pub generation: u64,
    /// A changed generation requires refetching configuration before cache pruning.
    pub configuration_changed: bool,
    pub dispositions: Vec<InstallationDisposition>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AssignmentControl {
    pub allow_launch: bool,
    pub cancel_pending: bool,
    /// User cancellation covers pending, starting and running work.
    #[serde(default)]
    pub cancel_running: bool,
    pub reason: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollmentRequest {
    pub enrollment_token: String,
    pub machine_id: uuid::Uuid,
    pub credential_id: uuid::Uuid,
    /// Lower-case hex SHA-256 of the complete connector bearer token.
    pub token_verifier: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollmentReceipt {
    pub connector_id: String,
    pub credential_id: uuid::Uuid,
}

#[cfg(test)]
mod installation_spec_tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn spec() -> InstallationSpec {
        InstallationSpec {
            release_path: "/opt/models/example/r1".into(),
            entrypoint: vec!["bin/wrapper".into(), "execute".into()],
            slurm_script: None,
            environment: BTreeMap::from([("DATASET_ROOT".into(), "/data/release-1".into())]),
        }
    }

    #[test]
    fn older_configuration_does_not_claim_run_namespace_protection() {
        let legacy = serde_json::json!({"generation":1,"contexts":[],"publications":[]});
        let configuration: DesiredConfiguration = serde_json::from_value(legacy.clone()).unwrap();
        assert!(!configuration.run_payload_namespace_reserved);
        let mut protected = legacy;
        protected["run_payload_namespace_reserved"] = serde_json::json!(true);
        assert!(
            serde_json::from_value::<DesiredConfiguration>(protected)
                .unwrap()
                .run_payload_namespace_reserved
        );
    }

    #[test]
    fn resolving_an_installation_derives_every_identity_from_scientific_data() {
        let mut definition = crate::models::parse_publication(include_bytes!(
            "../../cli/examples/scalar-publication.json"
        ))
        .unwrap()
        .definition;
        definition.dataset_identities = vec!["dataset-b@r2".into(), "dataset-a@r1".into()];
        let settings = spec();
        let resolved = settings
            .clone()
            .resolve("bind-retained".into(), &definition);
        assert_eq!(resolved.provider_binding_id, "bind-retained");
        assert_eq!(resolved.spec(), settings);
        let identity = serde_json::to_value(&resolved.installation).unwrap();
        let definition = serde_json::to_value(&definition).unwrap();
        for field in [
            "model_identity",
            "wrapper_identity",
            "runtime_identity",
            "wrapper_protocol_version",
            "dataset_identities",
        ] {
            assert_eq!(identity[field], definition[field]);
        }
        let serialized = serde_json::to_value(settings).unwrap();
        assert!(serialized.get("installation").is_none());
        assert!(serialized.get("provider_binding_id").is_none());
    }

    #[test]
    fn physical_settings_enforce_local_bounds_without_context_access() {
        let valid = spec();
        valid.validate().unwrap();
        valid.validate_driver(ExecutionDriver::Process).unwrap();
        for release in [
            "relative",
            "/opt/../secret",
            "/opt/release\0",
            "/opt/release\n",
            "/opt/release\r",
        ] {
            let mut invalid = valid.clone();
            invalid.release_path = release.into();
            assert!(invalid.validate().is_err(), "{release:?}");
        }
        for entrypoint in ["", ".", "..", "../wrapper", "/bin/wrapper"] {
            let mut invalid = valid.clone();
            invalid.entrypoint = vec![entrypoint.into()];
            assert!(invalid.validate().is_err(), "{entrypoint:?}");
        }
        let mut invalid = valid.clone();
        invalid.entrypoint = vec!["wrapper".into(); 33];
        assert!(invalid.validate().is_err());
        invalid = valid.clone();
        invalid.environment = (0..33)
            .map(|n| (format!("KEY{n}"), String::new()))
            .collect();
        assert!(invalid.validate().is_err());
        invalid = valid.clone();
        invalid.environment.insert("KEY".into(), "x".repeat(4097));
        assert!(invalid.validate().is_err());
        invalid = valid.clone();
        invalid.slurm_script = Some("../model.sbatch".into());
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn installation_environment_protects_runner_scheduler_and_mpi_keys() {
        for key in [
            "HOME",
            "PATH",
            "PYTHONDONTWRITEBYTECODE",
            "SLURM_JOB_ID",
            "SLURM_NTASKS",
            "SLURM_CONF",
            "SLURM_NEW_VARIABLE",
            "PMI_RANK",
            "PMI_BUFFER_SIZE",
            "PMI_NEW_VARIABLE",
            "PMIX_RANK",
            "PMIX_MCA_custom",
            "PMIX_NEW_VARIABLE",
            "LD_LIBRARY_PATH",
            "DYLD_INSERT_LIBRARIES",
        ] {
            let mut invalid = spec();
            invalid.environment = BTreeMap::from([(key.into(), "private-value".into())]);
            let error = invalid.validate().unwrap_err();
            assert!(error.contains(&format!("{key:?}")), "{error}");
            assert!(!error.contains("private-value"), "{error}");
        }
        let valid = BTreeMap::from([
            ("DATASET_ROOT".into(), "/data/release-1".into()),
            ("OMP_NUM_THREADS".into(), "4".into()),
            ("MODEL_MODE".into(), "normal".into()),
            ("home".into(), "case-sensitive".into()),
            ("PMIX".into(), "prefix-without-underscore".into()),
        ]);
        validate_installation_environment(&valid).unwrap();
    }

    #[test]
    fn installation_environment_diagnostics_are_deterministic_and_escape_keys() {
        let mut environment = BTreeMap::from([
            ("SLURM_JOB_ID".into(), "hidden-slurm-value".into()),
            ("HOME".into(), "hidden-home-value".into()),
        ]);
        let error = validate_installation_environment(&environment).unwrap_err();
        assert!(error.contains("\"HOME\""), "{error}");
        assert!(!error.contains("SLURM_JOB_ID"), "{error}");
        assert!(!error.contains("hidden"), "{error}");

        environment = BTreeMap::from([("SLURM_\n\t\u{1b}\"".into(), "hidden-value".into())]);
        let error = validate_installation_environment(&environment).unwrap_err();
        assert!(!error.chars().any(char::is_control), "{error:?}");
        assert!(error.contains("\\n\\t\\u{1b}\\\""), "{error}");
        assert!(!error.contains("hidden-value"), "{error}");
    }

    #[test]
    fn shared_environment_validation_retains_structural_limits() {
        for (key, value) in [
            (String::new(), "value".into()),
            ("K".repeat(129), "value".into()),
            ("bad=name".into(), "value".into()),
            ("bad\0name".into(), "value".into()),
            ("KEY".into(), "x".repeat(4097)),
            ("KEY".into(), "bad\0value".into()),
        ] {
            assert!(validate_installation_environment(&BTreeMap::from([(key, value)])).is_err());
        }
        let environment = (0..33)
            .map(|n| (format!("KEY{n}"), String::new()))
            .collect();
        assert!(validate_installation_environment(&environment).is_err());
        validate_installation_environment(&BTreeMap::from([("K".repeat(128), "x".repeat(4096))]))
            .unwrap();
    }

    #[test]
    fn author_settings_reject_retained_registry_fields() {
        let valid = serde_json::to_value(spec()).unwrap();
        for (field, value) in [
            ("provider_binding_id", json!("bind-retained")),
            (
                "installation",
                json!({"model_identity":"independent-claim"}),
            ),
            ("grant_id", json!(uuid::Uuid::new_v4())),
        ] {
            let mut invalid = valid.clone();
            invalid[field] = value;
            assert!(serde_json::from_value::<InstallationSpec>(invalid).is_err());
        }
    }
}

#[cfg(test)]
mod context_tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn development_requires_process_direct_and_managed_defaults_preserve_wire_identity() {
        let manifest: Value = serde_json::from_str(include_str!(
            "../../cli/examples/process-direct-context.json"
        ))
        .unwrap();
        let legacy = &manifest["spec"]["definition"];
        let mut context: ExecutionContextDefinition =
            serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(context.execution_mode, ExecutionMode::Managed);
        assert_eq!(serde_json::to_value(&context).unwrap(), *legacy);
        let mut explicit = legacy.clone();
        explicit["execution_mode"] = json!("managed");
        assert_eq!(
            serde_json::from_value::<ExecutionContextDefinition>(explicit).unwrap(),
            context
        );
        context.execution_mode = ExecutionMode::Development;
        context.validate_execution().unwrap();
        assert_eq!(
            serde_json::from_value::<ExecutionContextDefinition>(
                serde_json::to_value(&context).unwrap()
            )
            .unwrap(),
            context
        );
        context.driver = DriverConfig::Slurm(SlurmConfig {
            worker_path: "/bin/enumeray".into(),
        });
        assert!(context.validate_execution().is_err());
        context.driver = DriverConfig::Process { scratch: None };
        context.connection = ConnectionConfig::Ssh(SshTarget {
            host: "example.org".into(),
            user: "model".into(),
            port: 22,
            credential_reference: "key".into(),
            runner_path: "/bin/enumeray".into(),
        });
        assert!(context.validate_execution().is_err());
        for mode in [json!(null), json!(true), json!("automatic")] {
            let mut invalid = legacy.clone();
            invalid["execution_mode"] = mode;
            assert!(serde_json::from_value::<ExecutionContextDefinition>(invalid).is_err());
        }
    }

    #[test]
    fn process_scratch_policy_is_explicit_bounded_and_legacy_compatible() {
        let manifest: Value = serde_json::from_str(include_str!(
            "../../cli/examples/process-direct-context.json"
        ))
        .unwrap();
        let policy = manifest["spec"]["definition"]["driver"]["scratch"].clone();
        serde_json::from_value::<ProcessScratchPolicy>(policy.clone())
            .unwrap()
            .validate()
            .unwrap();
        for (key, value) in [
            ("max_bytes", 0),
            ("max_entries", 0),
            ("max_entries", 1_000_001),
            ("max_depth", 0),
            ("max_depth", 65),
            ("scan_interval_ms", 99),
            ("scan_interval_ms", 60_001),
        ] {
            let mut invalid = policy.clone();
            invalid[key] = json!(value);
            assert!(
                serde_json::from_value::<ProcessScratchPolicy>(invalid)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
        for key in [
            "max_bytes",
            "max_entries",
            "max_depth",
            "scan_interval_ms",
            "cache",
            "symlinks",
        ] {
            let mut invalid = policy.clone();
            invalid.as_object_mut().unwrap().remove(key);
            assert!(serde_json::from_value::<ProcessScratchPolicy>(invalid).is_err());
        }
        for invalid in [
            json!(null),
            json!([1073741824, 10000, 32, 1000, "per_run", "forbid"]),
        ] {
            assert!(
                serde_json::from_value::<DriverConfig>(json!({"kind":"process","scratch":invalid}))
                    .is_err()
            );
        }
        assert!(serde_json::from_str::<DriverConfig>(r#"{"kind":"process","scratch":{"max_bytes":1,"max_bytes":2,"max_entries":10,"max_depth":10,"scan_interval_ms":100,"cache":"none","symlinks":"forbid"}}"#).is_err());
        let driver: DriverConfig = serde_json::from_value(json!({"kind":"process"})).unwrap();
        assert_eq!(
            serde_json::to_value(driver).unwrap(),
            json!({"kind":"process"})
        );
        assert!(
            serde_json::from_value::<DriverConfig>(
                json!({"kind":"slurm","worker_path":"/bin/worker","scratch":policy})
            )
            .is_err()
        );
    }
    #[test]
    fn configured_process_paths_are_reserved_without_changing_legacy_or_slurm() {
        let manifest: serde_json::Value = serde_json::from_str(include_str!(
            "../../cli/examples/process-direct-context.json"
        ))
        .unwrap();
        let configured: DriverConfig =
            serde_json::from_value(manifest["spec"]["definition"]["driver"].clone()).unwrap();
        for key in ["TMPDIR", "TMP", "TEMP", "XDG_CACHE_HOME"] {
            let environment = std::collections::BTreeMap::from([(key.into(), "/outside".into())]);
            assert!(configured.validate_environment(&environment).is_err());
            DriverConfig::Process { scratch: None }
                .validate_environment(&environment)
                .unwrap();
            DriverConfig::Slurm(SlurmConfig {
                worker_path: "/bin/worker".into(),
            })
            .validate_environment(&environment)
            .unwrap();
        }
    }

    #[test]
    fn slurm_configuration_leaves_allocation_policy_to_the_batch_file() {
        let manifest: Value =
            serde_json::from_str(include_str!("../../cli/examples/slurm-direct-context.json"))
                .unwrap();
        let mut context: ExecutionContextDefinition =
            serde_json::from_value(manifest["spec"]["definition"].clone()).unwrap();
        context.validate_execution().unwrap();
        context.driver = DriverConfig::Slurm(SlurmConfig {
            worker_path: "relative".into(),
        });
        assert!(context.validate_execution().is_err());
    }

    #[test]
    fn slurm_installations_require_a_confined_model_batch_file() {
        let mut descriptor = crate::models::parse_publication(include_bytes!(
            "../../cli/examples/scalar-publication.json"
        ))
        .unwrap()
        .candidate("test-publication")
        .unwrap()
        .descriptor;
        descriptor
            .validate_driver(ExecutionDriver::Process)
            .unwrap();
        assert!(descriptor.validate_driver(ExecutionDriver::Slurm).is_err());
        for valid in ["model.sbatch", "scheduler/model.sh"] {
            descriptor.slurm_script = Some(valid.into());
            descriptor.validate_driver(ExecutionDriver::Slurm).unwrap();
            assert!(
                descriptor
                    .validate_driver(ExecutionDriver::Process)
                    .is_err()
            );
        }
        for invalid in [
            "",
            ".",
            "..",
            "../model.sbatch",
            "scheduler/../model.sh",
            "/tmp/model.sh",
            "model\n.sh",
            "model\r.sh",
            "model\0.sh",
        ] {
            descriptor.slurm_script = Some(invalid.into());
            assert!(
                descriptor.validate_driver(ExecutionDriver::Slurm).is_err(),
                "{invalid:?}"
            );
        }
        assert!(
            serde_json::from_value::<DriverConfig>(json!({
                "kind":"slurm", "worker_path":"/bin/worker", "account":"legacy"
            }))
            .is_err()
        );
    }

    #[test]
    fn documented_matrix_round_trips_and_rejects_mixed_configuration() {
        for example in [
            include_str!("../../cli/examples/process-direct-context.json"),
            include_str!("../../cli/examples/process-ssh-context.json"),
            include_str!("../../cli/examples/slurm-direct-context.json"),
            include_str!("../../cli/examples/slurm-ssh-context.json"),
        ] {
            let manifest: Value = serde_json::from_str(example).unwrap();
            let definition = manifest["spec"]["definition"].clone();
            let context: ExecutionContextDefinition =
                serde_json::from_value(definition.clone()).unwrap();
            context.validate_execution().unwrap();
            assert_eq!(serde_json::to_value(&context).unwrap(), definition);
            let mut invalid = definition.clone();
            invalid["resource_presets"] = json!({"small": {"cpu_count": 1}});
            assert!(serde_json::from_value::<ExecutionContextDefinition>(invalid).is_err());
            let mut invalid = definition.clone();
            invalid["driver"]["kind"] = json!("ssh");
            assert!(serde_json::from_value::<ExecutionContextDefinition>(invalid).is_err());
            let mut invalid = definition.clone();
            invalid["connection"]["kind"] = json!("slurm");
            assert!(serde_json::from_value::<ExecutionContextDefinition>(invalid).is_err());
            let mut invalid = definition.clone();
            invalid["driver"] = json!({"kind":"process","account":"unexpected"});
            assert!(serde_json::from_value::<ExecutionContextDefinition>(invalid).is_err());
            let mut invalid = definition;
            invalid["launcher"] = json!(["/legacy-launcher"]);
            assert!(serde_json::from_value::<ExecutionContextDefinition>(invalid).is_err());
        }
    }
    #[test]
    fn work_roots_must_be_absolute_on_both_connections() {
        let manifest: Value = serde_json::from_str(include_str!(
            "../../cli/examples/process-direct-context.json"
        ))
        .unwrap();
        let mut context: ExecutionContextDefinition =
            serde_json::from_value(manifest["spec"]["definition"].clone()).unwrap();
        context.work_root = "relative".into();
        assert!(context.validate_execution().is_err());
        context.connection = ConnectionConfig::Ssh(SshTarget {
            host: "cluster.example".into(),
            user: "model".into(),
            port: 22,
            credential_reference: "cluster".into(),
            runner_path: "/opt/runner".into(),
        });
        assert!(context.validate_execution().is_err());
        context.work_root = "/srv/jobs".into();
        context.validate_execution().unwrap();
    }

    #[test]
    fn rejects_unusable_installation_roots_and_credential_references_before_registration() {
        let manifest: Value =
            serde_json::from_str(include_str!("../../cli/examples/process-ssh-context.json"))
                .unwrap();
        let context: ExecutionContextDefinition =
            serde_json::from_value(manifest["spec"]["definition"].clone()).unwrap();
        for root in [
            "relative",
            "/",
            "/./",
            "/srv/../models",
            "/srv/models\n",
            "/srv/\0models",
        ] {
            let mut invalid = context.clone();
            invalid.installation_roots = vec![root.into()];
            assert!(invalid.validate_execution().is_err(), "{root:?}");
        }
        let mut invalid = context.clone();
        invalid.installation_roots = vec![];
        assert!(invalid.validate_execution().is_err());
        invalid.installation_roots = vec![format!("/{}", "a".repeat(4096))];
        assert!(invalid.validate_execution().is_err());
        for reference in ["\0credential", "credential\n", ""] {
            let mut invalid = context.clone();
            let ConnectionConfig::Ssh(target) = &mut invalid.connection else {
                unreachable!()
            };
            target.credential_reference = reference.into();
            assert!(invalid.validate_execution().is_err(), "{reference:?}");
        }
        context.validate_execution().unwrap();
    }
}

#[cfg(test)]
mod shared_workspace_tests {
    use super::*;
    #[test]
    fn mapping_is_explicit_absolute_and_bound_to_the_runner_work_root() {
        let valid = SharedWorkspace {
            runner_root: "/srv/jobs".into(),
            connector_root: "/mnt/jobs".into(),
        };
        assert!(valid.validate("/srv/jobs").is_ok());
        assert!(valid.validate("/other/jobs").is_err());
        for root in ["relative", "/mnt/../secret", "/mnt/jobs\nsecret"] {
            let mapping = SharedWorkspace {
                connector_root: root.into(),
                ..valid.clone()
            };
            assert!(mapping.validate("/srv/jobs").is_err());
        }
        assert!(serde_json::from_value::<SharedWorkspace>(serde_json::json!({"runner_root":"/srv/jobs","connector_root":"/mnt/jobs","credentials":"rejected"})).is_err());
    }
}
