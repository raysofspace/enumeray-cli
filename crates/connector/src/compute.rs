//! Durable execution on SSH hosts and Slurm submission hosts. Only a fresh,
//! fsynced launch record permits submission; recovery never resubmits.
mod runner;

pub(crate) use runner::run as runner;

use crate::connections::resolve as connection;
use crate::drivers::{CancellationOutcome, CancellationScope};
use crate::protocol::{CancellationRequest, CancellationResponse};
use crate::{Result, failure, local_process};
use enumeray_contracts::execution::{ConnectionConfig, DriverConfig, ExecutionMode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use uuid::Uuid;

/// Host-only view. Retained installation serialization remains unchanged.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ComputeTarget {
    pub execution_mode: ExecutionMode,
    pub connection: ConnectionConfig,
    pub work_root: String,
    pub driver: DriverConfig,
    pub identity_file: Option<PathBuf>,
}
impl ComputeTarget {
    pub(crate) fn ssh(&self) -> Option<&enumeray_contracts::execution::SshTarget> {
        match &self.connection {
            ConnectionConfig::Ssh(target) => Some(target),
            _ => None,
        }
    }
    pub(crate) fn launcher(&self) -> &[String] {
        match &self.connection {
            ConnectionConfig::Direct { launcher } => launcher,
            _ => &[],
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputeInstallation {
    #[serde(default, skip_serializing_if = "ExecutionMode::is_managed")]
    pub execution_mode: ExecutionMode,
    pub descriptor: enumeray_contracts::execution::InstallationDescriptor,
    pub provider_binding_id: String,
    pub context_id: String,
    pub installation: local_process::Installation,
    pub work_root: String,
    pub connection: ConnectionConfig,
    pub identity_file: Option<PathBuf>,
    pub driver: DriverConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slurm_script: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_workspace: Option<enumeray_contracts::execution::SharedWorkspace>,
}

impl ComputeInstallation {
    pub(crate) fn target(&self) -> ComputeTarget {
        ComputeTarget {
            execution_mode: self.execution_mode,
            connection: self.connection.clone(),
            work_root: self.work_root.clone(),
            driver: self.driver.clone(),
            identity_file: self.identity_file.clone(),
        }
    }
    fn validate_environment(&self) -> Result<()> {
        self.driver
            .validate_environment(&self.descriptor.environment)
            .map_err(failure)?;
        self.driver
            .validate_environment(&self.installation.environment)
            .map_err(failure)
    }

    fn execution_snapshot(&self) -> ExecutionSnapshot {
        ExecutionSnapshot {
            execution_mode: self.execution_mode,
            installation: self.installation.clone(),
            work_root: self.work_root.clone(),
            driver: self.driver.clone(),
            slurm_script: self.slurm_script.clone(),
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExecutionSnapshot {
    #[serde(default, skip_serializing_if = "ExecutionMode::is_managed")]
    pub execution_mode: ExecutionMode,
    pub installation: local_process::Installation,
    pub work_root: String,
    pub driver: DriverConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slurm_script: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ValidationRequest {
    installation_roots: Vec<String>,
    driver: DriverConfig,
    descriptor: enumeray_contracts::execution::InstallationDescriptor,
}
pub(crate) fn invoke(
    journal: &Path,
    job: &ComputeJob,
    action: &str,
    input: Option<&[u8]>,
) -> Result<Value> {
    let binding: ComputeInstallation = local_process::read_record(
        &evidence_directory(journal, job.intent).join("connection.json"),
    )?;
    let result = connection(&binding).invoke(
        &binding,
        action,
        &remote_directory(&job.execution, job.intent),
        input,
    );
    if let Err(error) = &result
        && let Some(command) = &error.command
    {
        let directory = evidence_directory(journal, job.intent);
        let name = format!("transport-{action}-failure.json");
        if !directory
            .join(&name)
            .try_exists()
            .map_err(|_| failure("cannot inspect transport diagnostics"))?
        {
            local_process::write_record(&directory, &name, command)?;
        }
        if action == "start"
            && matches!(
                command.state,
                crate::connections::CommandState::NotSpawned { .. }
            )
        {
            let evidence = crate::launch::StartupFailure::new(
                job.intent,
                &job.assignment,
                crate::launch::FailureStage::LaunchSpawn,
                "Cannot start the runner transport; scientific execution did not start.",
                Some(command.as_ref().clone()),
            )?;
            crate::launch::retain_failure(&directory, &evidence, &job.assignment)?;
            return completion_response(&directory, evidence.completion(&job.assignment)?);
        }
    }
    result
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ComputeJob {
    #[serde(default)]
    pub(crate) log_policy: enumeray_contracts::logs::LogPolicy,
    pub(crate) version: u32,
    pub(crate) intent: Uuid,
    pub(crate) assignment: Value,
    pub(crate) execution: ExecutionSnapshot,
    #[serde(default)]
    input_digests: std::collections::BTreeMap<Uuid, (u64, String)>,
}

pub fn prepare(config: &Path, assignment: &Value) -> Result<ComputeInstallation> {
    let definition = serde_json::from_value(assignment["definition"].clone())
        .map_err(|_| failure("invalid pinned model definition"))?;
    enumeray_contracts::models::validate_inputs(&definition, &assignment["inputs"])
        .map_err(|_| failure("inputs do not match pinned model schema"))?;
    let value: Value = serde_json::from_slice(&local_process::read_private(
        config,
        local_process::MAX_RECORD_BYTES,
    )?)
    .map_err(|_| failure("invalid execution configuration"))?;
    if value["version"] != 1 {
        return Err(failure(
            "unsupported installation cache; reset the development connector configuration",
        ));
    }
    let binding: ComputeInstallation = value["installations"]
        .as_array()
        .and_then(|bindings| {
            bindings.iter().find(|item| {
                item["provider_binding_id"] == assignment["descriptor"]["provider_binding_id"]
            })
        })
        .cloned()
        .and_then(|item| serde_json::from_value(item).ok())
        .ok_or_else(|| failure("no remote installation for assigned publication"))?;
    validate_assignment_installation(&binding, assignment)?;
    Ok(binding)
}

/// Match the retained installation to the assignment's immutable destination.
/// Recovery uses the same snapshot rather than selecting current configuration.
pub(crate) fn validate_assignment_installation(
    binding: &ComputeInstallation,
    assignment: &Value,
) -> Result<()> {
    binding.validate_environment()?;
    validate_remote(binding)?;
    if serde_json::from_value::<enumeray_contracts::execution::InstallationDescriptor>(
        assignment["descriptor"].clone(),
    )
    .ok()
    .as_ref()
        != Some(&binding.descriptor)
    {
        return Err(failure(
            "installation descriptor differs from pinned publication",
        ));
    }
    if assignment["descriptor"]["provider_binding_id"].as_str()
        != Some(binding.provider_binding_id.as_str())
        || binding.installation.provider_binding_id != binding.provider_binding_id
        || binding.context_id.is_empty()
        || assignment["context_id"].as_str() != Some(binding.context_id.as_str())
        || assignment["publication_id"]
            .as_str()
            .is_none_or(str::is_empty)
    {
        return Err(failure(
            "installation destination differs from pinned assignment",
        ));
    }
    if serde_json::to_value(&binding.installation.installation)
        .ok()
        .as_ref()
        != Some(&assignment["descriptor"]["installation"])
    {
        return Err(failure(
            "remote installation identity differs from pinned publication",
        ));
    }
    if serde_json::to_value(binding.driver.kind()).ok().as_ref()
        != Some(&assignment["requirements"]["driver"])
    {
        return Err(failure("remote driver conflicts with pinned requirements"));
    }
    Ok(())
}

fn safe_absolute(value: &str) -> bool {
    Path::new(value).is_absolute()
        && !value.contains(['\0', '\n', '\r'])
        && !Path::new(value)
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
}

fn validate_remote(binding: &ComputeInstallation) -> Result<()> {
    binding
        .execution_mode
        .validate(&binding.driver, &binding.connection)
        .map_err(failure)?;
    if let DriverConfig::Process {
        scratch: Some(policy),
    } = &binding.driver
    {
        policy.validate().map_err(failure)?;
    }
    if !safe_absolute(&binding.work_root) {
        return Err(failure("compute work root must be absolute"));
    }
    match (&binding.driver, binding.slurm_script.as_deref()) {
        (DriverConfig::Slurm(_), Some(script)) if safe_absolute(script) => (),
        (DriverConfig::Process { .. }, None) => (),
        _ => {
            return Err(failure(
                "compute installation has no valid Slurm batch file or an unexpected one",
            ));
        }
    }
    if let Some(mapping) = &binding.shared_workspace {
        mapping.validate(&binding.work_root).map_err(failure)?;
    }
    connection(binding).validate(binding)
}
pub(crate) fn remote_directory(binding: &ExecutionSnapshot, intent: Uuid) -> String {
    format!("{}/{}", binding.work_root.trim_end_matches('/'), intent)
}
pub(crate) fn evidence_directory(journal: &Path, intent: Uuid) -> PathBuf {
    journal.join("compute-jobs").join(intent.to_string())
}

pub fn launch(
    journal: &Path,
    intent: Uuid,
    assignment: &Value,
    binding: ComputeInstallation,
    allow_development_execution: bool,
) -> Result<Option<local_process::Completion>> {
    require_launch_policy(&binding, allow_development_execution)?;
    let directory = evidence_directory(journal, intent);
    if directory.exists() {
        return Err(failure(
            "remote launch evidence already exists; recovery cannot resubmit",
        ));
    }
    local_process::create_private_directory(&journal.join("compute-jobs"))?;
    local_process::create_private_directory(&directory)?;
    let job = ComputeJob {
        log_policy: crate::capture::policy(&local_process::job_directory(journal, intent))?,
        version: 1,
        intent,
        assignment: assignment.clone(),
        execution: binding.execution_snapshot(),
        input_digests: crate::artifacts::input_ids(assignment)?
            .into_iter()
            .map(|id| {
                crate::artifacts::file_digest(
                    &crate::artifacts::input_directory(journal, intent).join(id.to_string()),
                )
                .map(|digest| (id, digest))
            })
            .collect::<Result<_>>()?,
    };
    local_process::write_record(&directory, "job.json", &job)?;
    local_process::write_record(&directory, "connection.json", &binding)?;
    prepare_inputs(journal, &job)?;
    start_prepared(journal, intent, assignment, allow_development_execution)
}

pub(crate) fn prepare_inputs(journal: &Path, job: &ComputeJob) -> Result<()> {
    let binding: ComputeInstallation = local_process::read_record(
        &evidence_directory(journal, job.intent).join("connection.json"),
    )?;
    // Remote prepare never launches. The following start call may have an unknown
    // outcome, therefore it is intentionally never replayed by recover().
    let prepared = invoke(
        journal,
        job,
        "prepare",
        Some(&serde_json::to_vec(&job).map_err(|_| failure("cannot encode remote job"))?),
    )?;
    if prepared["status"] == "completed" {
        let completion: local_process::Completion =
            serde_json::from_value(prepared["completion"].clone())
                .map_err(|_| failure("invalid preparation failure"))?;
        completion.validate(job.intent, &job.assignment)?;
        if !completion
            .startup_evidence
            .as_ref()
            .is_some_and(|e| e.stage.before_submission())
        {
            return Err(failure(
                "preparation returned unsupported completion evidence",
            ));
        }
        local_process::write_record(
            &evidence_directory(journal, job.intent),
            "preparation-failure.json",
            &completion,
        )?;
        return Ok(());
    }
    if prepared["status"] != "prepared" {
        return Err(failure("preparation did not confirm readiness"));
    }
    let source = crate::artifacts::input_directory(journal, job.intent);
    let remote = remote_directory(&job.execution, job.intent);
    crate::collections::stage_runner(journal, job, &binding)?;
    for id in crate::artifacts::input_ids(&job.assignment)? {
        connection(&binding).upload_input(&binding, &remote, id, &source.join(id.to_string()))?;
    }
    local_process::write_record(
        &evidence_directory(journal, job.intent),
        "inputs-ready.json",
        &json!({"intent":job.intent}),
    )?;
    Ok(())
}

/// Preparation is recoverable and never invokes start.
pub(crate) fn stage_runner(
    journal: &Path,
    intent: Uuid,
    assignment: &Value,
    binding: ComputeInstallation,
) -> Result<()> {
    validate_assignment_installation(&binding, assignment)?;
    let directory = evidence_directory(journal, intent);
    if !directory.exists() {
        local_process::create_private_directory(&journal.join("compute-jobs"))?;
        local_process::create_private_directory(&directory)?;
    }
    let path = directory.join("job.json");
    let job = if path.exists() {
        let job: ComputeJob = local_process::read_record(&path)?;
        if job.intent != intent || !crate::numeric::equivalent(&job.assignment, assignment) {
            return Err(failure("prepared job conflicts with assignment"));
        }
        job
    } else {
        let job = ComputeJob {
            log_policy: crate::capture::policy(&local_process::job_directory(journal, intent))?,
            version: 1,
            intent,
            assignment: assignment.clone(),
            execution: binding.execution_snapshot(),
            input_digests: crate::artifacts::input_ids(assignment)?
                .into_iter()
                .map(|id| {
                    crate::artifacts::file_digest(
                        &crate::artifacts::input_directory(journal, intent).join(id.to_string()),
                    )
                    .map(|digest| (id, digest))
                })
                .collect::<Result<_>>()?,
        };
        // Connection first: restart can complete preparation from the original binding.
        local_process::write_record(&directory, "connection.json", &binding)?;
        local_process::write_record(&directory, "job.json", &job)?;
        job
    };
    if directory.join("start-requested.json").exists() {
        return Err(failure("input staging cannot reopen submitted work"));
    }
    prepare_inputs(journal, &job)
}

pub(crate) fn start_prepared(
    journal: &Path,
    intent: Uuid,
    assignment: &Value,
    allow_development_execution: bool,
) -> Result<Option<local_process::Completion>> {
    let directory = evidence_directory(journal, intent);
    let job: ComputeJob = local_process::read_record(&directory.join("job.json"))?;
    if job.intent != intent || !crate::numeric::equivalent(&job.assignment, assignment) {
        return Err(failure("start conflicts with prepared assignment"));
    }
    if directory.join("preparation-failure.json").exists() {
        return recover(
            journal,
            intent,
            assignment,
            None,
            allow_development_execution,
        );
    }
    if !directory.join("inputs-ready.json").exists()
        || directory.join("start-requested.json").exists()
    {
        return Err(failure("start requires fresh prepared input evidence"));
    }
    let binding = retained_installation(journal, intent)?;
    require_launch_policy(&binding, allow_development_execution)?;
    local_process::write_record(
        &directory,
        "start-requested.json",
        &json!({"intent":intent}),
    )?;
    invoke(journal, &job, "start", None)?;
    recover(
        journal,
        intent,
        assignment,
        None,
        allow_development_execution,
    )
}

pub(crate) fn allocation_released(
    journal: &Path,
    intent: Uuid,
    assignment: &Value,
) -> Result<bool> {
    let job: ComputeJob =
        local_process::read_record(&evidence_directory(journal, intent).join("job.json"))?;
    if job.intent != intent || !crate::numeric::equivalent(&job.assignment, assignment) {
        return Err(failure("allocation observation conflicts with assignment"));
    }
    let reply = invoke(journal, &job, "allocation-status", None)?;
    Ok(reply["status"] == "completed")
}

pub(crate) fn needs_input_staging(journal: &Path, intent: Uuid) -> bool {
    let directory = evidence_directory(journal, intent);
    directory.join("job.json").exists()
        && !directory.join("start-requested.json").exists()
        && !directory.join("inputs-ready.json").exists()
        && !directory.join("preparation-failure.json").exists()
        && !directory.join("startup-failure.json").exists()
}
pub(crate) fn retained_installation(journal: &Path, intent: Uuid) -> Result<ComputeInstallation> {
    local_process::read_record(&evidence_directory(journal, intent).join("connection.json"))
}

pub(crate) fn has_start_request(journal: &Path, intent: Uuid) -> bool {
    evidence_directory(journal, intent)
        .join("start-requested.json")
        .exists()
}

pub fn has_evidence(journal: &Path, intent: Uuid) -> bool {
    evidence_directory(journal, intent).exists()
}

/// Staging directories are launch evidence, but cannot address runner log reads.
pub(crate) fn has_capture_job(journal: &Path, intent: Uuid) -> bool {
    evidence_directory(journal, intent)
        .join("job.json")
        .is_file()
}

pub fn recover(
    journal: &Path,
    intent: Uuid,
    assignment: &Value,
    cancel: Option<CancellationScope>,
    allow_development_execution: bool,
) -> Result<Option<local_process::Completion>> {
    let path = evidence_directory(journal, intent).join("job.json");
    if !path.exists() {
        return Err(failure(
            "backend launch evidence is missing; unknown submission remains blocked and will never be resubmitted",
        ));
    }
    let job: ComputeJob = local_process::read_record(&path)?;
    if job.version != 1
        || job.intent != intent
        || !crate::numeric::equivalent(&job.assignment, assignment)
    {
        return Err(failure("remote launch evidence conflicts with assignment"));
    }
    if let Some(evidence) =
        crate::launch::read_failure(&evidence_directory(journal, intent), intent, assignment)?
    {
        if !evidence.stage.before_submission() {
            return Err(failure(
                "connector launch proof is not a pre-submission failure",
            ));
        }
        return Ok(Some(evidence.completion(assignment)?));
    }
    let preparation = evidence_directory(journal, intent).join("preparation-failure.json");
    if preparation
        .try_exists()
        .map_err(|_| failure("cannot inspect preparation outcome"))?
    {
        let completion: local_process::Completion = local_process::read_record(&preparation)?;
        completion.validate(intent, assignment)?;
        if !completion
            .startup_evidence
            .as_ref()
            .is_some_and(|e| e.stage.before_submission())
        {
            return Err(failure(
                "retained preparation failure lacks pre-submission proof",
            ));
        }
        return Ok(Some(completion));
    }
    if !evidence_directory(journal, intent)
        .join("start-requested.json")
        .exists()
    {
        let cancelled = evidence_directory(journal, intent).join("prelaunch-cancelled.json");
        if cancel.is_some() || cancelled.exists() {
            let completion = if cancelled.exists() {
                local_process::read_record(&cancelled)?
            } else {
                let completion = if cancel == Some(CancellationScope::Running) {
                    local_process::cancelled_completion(assignment)?
                } else {
                    local_process::failure_completion(
                        assignment,
                        "access_revoked",
                        "Execution authorization was revoked before backend submission.",
                    )?
                };
                local_process::write_record(
                    &evidence_directory(journal, intent),
                    "prelaunch-cancelled.json",
                    &completion,
                )?;
                completion
            };
            return Ok(Some(completion));
        }
        crate::execution_policy::require_consent(
            retained_installation(journal, intent)?.execution_mode,
            allow_development_execution,
        )?;
        if !evidence_directory(journal, intent)
            .join("inputs-ready.json")
            .exists()
        {
            prepare_inputs(journal, &job)?;
        }
        if !preparation.exists() {
            return start_prepared(journal, intent, assignment, allow_development_execution);
        }
        if preparation.exists() {
            let completion: local_process::Completion = local_process::read_record(&preparation)?;
            completion.validate(intent, assignment)?;
            return Ok(Some(completion));
        }
    }
    if let Some(scope) = cancel {
        // A request (including an accepted one) is never termination evidence.
        // Even a lost cancellation reply must not prevent observing the job.
        let _ = request_cancel(journal, &job, scope);
    }
    let value = invoke(journal, &job, "status", None)?;
    match value["status"].as_str() {
        Some("completed") => {
            let completion: local_process::Completion =
                serde_json::from_value(value["completion"].clone())
                    .map_err(|_| failure("invalid remote completion"))?;
            completion.validate(intent, assignment)?;
            Ok(Some(completion))
        }
        Some("running" | "pending") => {
            if value["started"] == true {
                let dir = local_process::job_directory(journal, intent);
                // Runner observation must not depend on log mirrors or on the
                // service having initialized local capture metadata first.
                local_process::create_private_directory(&journal.join("local-jobs"))?;
                local_process::create_private_directory(&dir)?;
                if !dir.join("started").exists() {
                    local_process::write_record(&dir, "started", &true)?;
                }
            }
            Ok(None)
        }
        _ => Err(failure(
            "remote submission unresolved; retained launch will never be resubmitted",
        )),
    }
}

/// A definitely unsubmitted assignment has an empty capture, which must still
/// exist for durable finalization and replay through the ordinary log pipeline.
pub(crate) fn cancel_before_launch(
    journal: &Path,
    intent: Uuid,
    assignment: &Value,
) -> Result<local_process::Completion> {
    let completion = local_process::cancelled_completion(assignment)?;
    let directory = evidence_directory(journal, intent);
    if directory.exists() {
        if directory.join("start-requested.json").exists() {
            return Err(failure(
                "prelaunch cancellation conflicts with submitted work",
            ));
        }
        local_process::write_record(&directory, "prelaunch-cancelled.json", &completion)?;
    }
    Ok(completion)
}

fn request_cancel(
    journal: &Path,
    job: &ComputeJob,
    scope: CancellationScope,
) -> Result<CancellationOutcome> {
    let payload = serde_json::to_vec(&CancellationRequest { scope })
        .map_err(|_| failure("cannot encode cancellation request"))?;
    let response: CancellationResponse =
        serde_json::from_value(invoke(journal, job, "request-cancel", Some(&payload))?)
            .map_err(|_| failure("invalid cancellation response"))?;
    Ok(response.cancellation)
}

pub(crate) fn execution_uid() -> Result<String> {
    let result = Command::new("/usr/bin/id")
        .arg("-u")
        .output()
        .map_err(|_| failure("cannot inspect execution identity"))?;
    if !result.status.success() {
        return Err(failure("cannot inspect execution identity"));
    }
    Ok(String::from_utf8_lossy(&result.stdout).trim().to_owned())
}

/// Recheck the live direct launcher immediately before every fresh submission.
/// Observation/cancellation of an existing start never requires fresh consent.
pub(crate) fn require_launch_policy(binding: &ComputeInstallation, allowed: bool) -> Result<()> {
    validate_remote(binding)?;
    crate::execution_policy::require_consent(binding.execution_mode, allowed)?;
    if matches!(binding.connection, ConnectionConfig::Direct { .. }) {
        let reply = connection(binding).invoke(binding, "identity", &binding.work_root, None)?;
        crate::execution_policy::check_identity(
            binding.execution_mode,
            &execution_uid()?,
            reply["execution_uid"].as_str().unwrap_or(""),
        )?;
    }
    Ok(())
}

pub fn validate_context(
    binding: &ComputeInstallation,
    context: &enumeray_contracts::execution::ExecutionContextDefinition,
    descriptor: &enumeray_contracts::execution::InstallationDescriptor,
    allow_development_execution: bool,
) -> Result<std::collections::BTreeMap<String, String>> {
    crate::execution_policy::require_consent(binding.execution_mode, allow_development_execution)?;
    context
        .driver
        .validate_environment(&descriptor.environment)
        .map_err(failure)?;
    binding.validate_environment()?;
    context.validate_execution().map_err(failure)?;
    validate_remote(binding)?;
    let validation = ValidationRequest {
        installation_roots: context.installation_roots.clone(),
        driver: context.driver.clone(),
        descriptor: descriptor.clone(),
    };
    let value = connection(binding).invoke(
        binding,
        "validate",
        &binding.work_root,
        Some(&serde_json::to_vec(&validation).map_err(|_| failure("invalid validation request"))?),
    )?;
    if value["status"] != "ready" {
        return Err(failure("compute installation not ready"));
    }
    if matches!(binding.connection, ConnectionConfig::Direct { .. }) {
        crate::execution_policy::check_identity(
            binding.execution_mode,
            &execution_uid()?,
            value["execution_uid"].as_str().unwrap_or(""),
        )?;
    }
    serde_json::from_value(value["file_digests"].clone())
        .map_err(|_| failure("validation lacks installed file digests"))
}

/// Seed permanent overlap pins on the binding's original compute transport.
pub(crate) fn protect_installations(
    binding: &ComputeInstallation,
    references: &[crate::cleanup::ReleaseReference],
) -> Result<()> {
    if references.len() > 128 {
        return Err(failure(
            "installation protection exceeds its reference bound",
        ));
    }
    let request = crate::protocol::InstallationProtectionRequest {
        references: references.to_vec(),
    };
    let input = serde_json::to_vec(&request)
        .map_err(|_| failure("cannot encode installation protection request"))?;
    if input.len() > 1024 * 1024 {
        return Err(failure(
            "installation protection request exceeds its 1 MiB bound",
        ));
    }
    let reply = connection(binding).invoke(
        binding,
        "protect-installations",
        &binding.work_root,
        Some(&input),
    )?;
    if reply["status"] != "completed" {
        return Err(failure("compute installation protection is incomplete"));
    }
    Ok(())
}

pub fn collect_files(
    journal: &Path,
    intent: Uuid,
    assignment: &Value,
    files: &std::collections::BTreeMap<String, crate::artifacts::OutputFile>,
) -> Result<PathBuf> {
    let job: ComputeJob =
        local_process::read_record(&evidence_directory(journal, intent).join("job.json"))?;
    let binding: ComputeInstallation =
        local_process::read_record(&evidence_directory(journal, intent).join("connection.json"))?;
    if !crate::numeric::equivalent(&job.assignment, assignment) {
        return Err(failure("output collection assignment mismatch"));
    }
    if let Some(mapping) = &binding.shared_workspace {
        mapping.validate(&binding.work_root).map_err(failure)?;
        let root = Path::new(&mapping.connector_root);
        for ancestor in root.ancestors() {
            if fs::symlink_metadata(ancestor)
                .map_err(|_| {
                    failure("approved shared workspace mount unavailable")
                        .with_kind(crate::ErrorKind::Retry)
                })?
                .file_type()
                .is_symlink()
            {
                return Err(failure(
                    "approved shared workspace root traverses a symbolic link",
                ));
            }
        }
        let workspace = root.join(intent.to_string()).join("workspace");
        for file in files.values() {
            let relative = format!("{intent}/workspace/{}", file.path);
            let path = crate::artifacts::confined_file(root, &relative)?;
            crate::artifacts::verify_file(&path, file.size_bytes, &file.sha256)?;
        }
        return Ok(workspace);
    }
    let collection_scope = crate::collections::output_scope(journal, intent);
    let destination = collection_scope
        .as_ref()
        .map(|s| s.0.clone())
        .unwrap_or_else(|| evidence_directory(journal, intent).join("outputs"));
    crate::collections::private_tree(&destination)?;
    local_process::create_private_directory(&destination)?;
    // Temporary files must not occupy the wrapper's output namespace: an
    // author may legitimately return any extension or the old partial suffix.
    let transfer_root = journal.join("artifact-transfers");
    local_process::create_private_directory(&transfer_root)?;
    let transfer_root = transfer_root.join(intent.to_string());
    local_process::create_private_directory(&transfer_root)?;
    let staging = collection_scope
        .as_ref()
        .map(|s| s.1.clone())
        .unwrap_or_else(|| transfer_root.join("output-staging"));
    crate::collections::private_tree(&staging)?;
    let _collection_reservation = collection_scope
        .as_ref()
        .map(|s| crate::spool::collection(journal, intent, &s.0, &s.1, s.2))
        .transpose()?;
    local_process::create_private_directory(&staging)?;
    for file in files.values() {
        let relative = Path::new(&file.path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|p| !matches!(p, std::path::Component::Normal(_)))
        {
            return Err(failure("remote output path escapes workspace"));
        }
        let path = destination.join(relative);
        let transfer_path = staging.join(format!(
            "{}.part",
            crate::artifacts::transfer_key(intent, &file.path, false, 0)
        ));
        let partial = enumeray_client::bulk::checkpoint_directory(&transfer_path)
            .map_err(|e| failure(e.to_string()))?
            .join("payload.part");
        crate::spool::reserve_copy(journal, intent, &path, file.size_bytes, &partial)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|_| failure("cannot create output staging"))?;
        }
        if path.exists() {
            crate::artifacts::verify_file(&path, file.size_bytes, &file.sha256)?;
            continue;
        }
        connection(&binding).download_output(
            &binding,
            &remote_directory(&job.execution, intent),
            file,
            &transfer_path,
        )?;
        crate::artifacts::verify_file(&transfer_path, file.size_bytes, &file.sha256)?;
        fs::File::open(&transfer_path)
            .and_then(|file| file.sync_all())
            .map_err(|_| failure("cannot persist output transfer"))?;
        fs::rename(&transfer_path, &path)
            .map_err(|_| failure("cannot commit verified output transfer"))?;
        fs::File::open(&staging)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| failure("cannot sync output transfer staging"))?;
        fs::File::open(path.parent().expect("output parent"))
            .and_then(|f| f.sync_all())
            .map_err(|_| failure("cannot sync output collection directory"))?;
    }
    Ok(destination)
}

pub(crate) fn completion_response(
    directory: &Path,
    completion: local_process::Completion,
) -> Result<Value> {
    let _ = directory;
    Ok(json!({"status":"completed","completion":completion}))
}

pub(crate) fn read_logs(
    journal: &Path,
    intent: Uuid,
    request: &enumeray_contracts::logs::ReadLogsRequest,
) -> Result<enumeray_contracts::logs::ReadLogsResponse> {
    let job: ComputeJob =
        local_process::read_record(&evidence_directory(journal, intent).join("job.json"))?;
    let input = serde_json::to_vec(request).map_err(|_| failure("invalid log read request"))?;
    serde_json::from_value(invoke(journal, &job, "read-logs", Some(&input))?)
        .map_err(|_| failure("invalid runner log response"))
}

pub(crate) fn collect_archive(
    journal: &Path,
    intent: Uuid,
) -> Result<(PathBuf, crate::log_archive::Built)> {
    let directory = evidence_directory(journal, intent);
    let job: ComputeJob = local_process::read_record(&directory.join("job.json"))?;
    let value = invoke(journal, &job, "build-log-archive", None)?;
    let built: crate::log_archive::Built = serde_json::from_value(value["archive"].clone())
        .map_err(|_| failure("invalid finalized archive"))?;
    if built.file.path != "log-archives/logs.tar.gz"
        || built.file.size_bytes > job.log_policy.archive_compressed_bytes
        || built.manifest.launch_intent_id != intent
    {
        return Err(failure("archive conflicts with pinned launch"));
    }
    let binding: ComputeInstallation =
        local_process::read_record(&directory.join("connection.json"))?;
    let root = directory.join("archive-copy");
    local_process::create_private_directory(&root)?;
    local_process::create_private_directory(&root.join("log-archives"))?;
    let path = root.join(&built.file.path);
    if !path.exists() {
        let temporary = path.with_extension("partial");
        connection(&binding).download_archive(
            &binding,
            &remote_directory(&job.execution, intent),
            &built.file,
            &temporary,
        )?;
        crate::artifacts::verify_file(&temporary, built.file.size_bytes, &built.file.sha256)?;
        fs::File::open(&temporary)
            .and_then(|file| file.sync_all())
            .map_err(|_| failure("cannot persist archive transfer"))?;
        fs::rename(temporary, &path).map_err(|_| failure("cannot commit archive transfer"))?;
        fs::File::open(path.parent().expect("archive parent"))
            .and_then(|f| f.sync_all())
            .map_err(|_| failure("cannot sync archive transfer"))?;
    }
    crate::artifacts::verify_file(&path, built.file.size_bytes, &built.file.sha256)?;
    Ok((root, built))
}
pub(crate) fn cleanup_archive(journal: &Path, intent: Uuid) -> Result<()> {
    let directory = evidence_directory(journal, intent);
    let job: ComputeJob = local_process::read_record(&directory.join("job.json"))?;
    invoke(journal, &job, "cleanup-log-archive", None)?;
    crate::log_archive::cleanup(&directory.join("archive-copy"))
}

pub(crate) fn cleanup_run(
    journal: &Path,
    intent: Uuid,
    assignment: &Value,
    request: &crate::cleanup::Request,
) -> Result<Value> {
    let job: ComputeJob =
        local_process::read_record(&evidence_directory(journal, intent).join("job.json"))?;
    if job.intent != intent || !crate::numeric::equivalent(&job.assignment, assignment) {
        return Err(failure("cleanup job conflicts with archived assignment"));
    }
    request.validate(assignment, job.log_policy.archive_enabled)?;
    let input =
        serde_json::to_vec(request).map_err(|_| failure("cannot encode run cleanup request"))?;
    invoke(journal, &job, "cleanup-run", Some(&input))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorKind;
    use crate::connections::{quote, run_bounded, ssh_command};
    use crate::drivers::slurm::{SbatchReceipt, parse_sbatch};
    use crate::journal;
    use enumeray_contracts::execution::{SlurmConfig, SshTarget};
    use std::time::Duration;

    #[cfg(unix)]
    #[test]
    fn output_transfer_staging_never_overwrites_another_declared_output() {
        let root = std::env::temp_dir().join(format!("enumeray-output-staging-{}", Uuid::new_v4()));
        local_process::create_private_directory(&root).unwrap();
        let launcher = root.join("launcher.py");
        fs::write(
            &launcher,
            r#"import base64, json, sys
request = json.load(sys.stdin)
assert request['action'] == 'read-output'
contents = {'answer.enumeray-transfer-partial': b'original', 'answer.csv': b'new'}
data = contents[request['payload']['path']][request['payload']['offset']:]
print(json.dumps({'version': 1, 'payload': {'data': base64.b64encode(data).decode()}}))
"#,
        )
        .unwrap();
        let descriptor: Value = serde_json::from_str(include_str!(
            "../../../examples/local-scalar-installation.json"
        ))
        .unwrap();
        let installation = ComputeInstallation {
            execution_mode: Default::default(),
            descriptor: serde_json::from_value(descriptor.clone()).unwrap(),
            provider_binding_id: "test".into(),
            context_id: "test-context".into(),
            installation: local_process::Installation {
                provider_binding_id: "test".into(),
                installation: serde_json::from_value(descriptor["installation"].clone()).unwrap(),
                argv: vec!["/unused/model".into()],
                environment: Default::default(),
                file_digests: Default::default(),
            },
            work_root: root.to_string_lossy().into_owned(),
            connection: ConnectionConfig::Direct {
                launcher: vec![
                    "/usr/bin/python3".into(),
                    launcher.to_string_lossy().into_owned(),
                ],
            },
            identity_file: None,
            driver: DriverConfig::Process { scratch: None },
            slurm_script: None,
            shared_workspace: None,
        };
        let intent = Uuid::new_v4();
        let assignment = json!({"attempt_id":Uuid::new_v4()});
        let job = ComputeJob {
            log_policy: Default::default(),
            version: 1,
            intent,
            assignment: assignment.clone(),
            execution: installation.execution_snapshot(),
            input_digests: Default::default(),
        };
        let evidence = evidence_directory(&root, intent);
        local_process::create_private_directory(evidence.parent().unwrap()).unwrap();
        local_process::create_private_directory(&evidence).unwrap();
        local_process::write_record(&evidence, "job.json", &job).unwrap();
        local_process::write_record(&evidence, "connection.json", &installation).unwrap();
        let mut files = std::collections::BTreeMap::new();
        for (name, path, content) in [
            (
                "first",
                "answer.enumeray-transfer-partial",
                b"original".as_slice(),
            ),
            ("second", "answer.csv", b"new".as_slice()),
        ] {
            let source = root.join(name);
            fs::write(&source, content).unwrap();
            let (size_bytes, sha256) = crate::artifacts::file_digest(&source).unwrap();
            files.insert(
                name.into(),
                crate::artifacts::OutputFile {
                    path: path.into(),
                    media_type: "text/plain".into(),
                    scientific_metadata: json!({}),
                    size_bytes,
                    sha256,
                },
            );
        }
        let outputs = collect_files(&root, intent, &assignment, &files).unwrap();
        assert_eq!(
            fs::read(outputs.join("answer.enumeray-transfer-partial")).unwrap(),
            b"original"
        );
        assert_eq!(fs::read(outputs.join("answer.csv")).unwrap(), b"new");
        // Retrying either publication must still verify its original bytes.
        collect_files(&root, intent, &assignment, &files).unwrap();
        fs::remove_file(outputs.join("answer.csv")).unwrap();
        let partial = root
            .join("artifact-transfers")
            .join(intent.to_string())
            .join("output-staging")
            .join(format!(
                "{}.part",
                crate::artifacts::transfer_key(intent, "answer.csv", false, 0)
            ));
        fs::write(&partial, b"interrupted").unwrap();
        // Conflicting partial evidence is retained and requires explicit removal.
        assert!(collect_files(&root, intent, &assignment, &files).is_err());
        assert_eq!(fs::read(&partial).unwrap(), b"interrupted");
        fs::remove_file(&partial).unwrap();
        collect_files(&root, intent, &assignment, &files).unwrap();
        assert!(!partial.exists());
        assert_eq!(fs::read(outputs.join("answer.csv")).unwrap(), b"new");
        assert_eq!(
            fs::read(outputs.join("answer.enumeray-transfer-partial")).unwrap(),
            b"original"
        );
        journal::check_private_path(&outputs.join("answer.csv"), false).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn configured_and_retained_installations_preserve_assignment_destination() {
        let binding: Value = serde_json::from_str(include_str!(
            "../../../examples/scalar-double-installation.json"
        ))
        .unwrap();
        let installation: ComputeInstallation = serde_json::from_value(json!({
            "descriptor":binding,
            "provider_binding_id":binding["provider_binding_id"],

            "context_id":"test-context",
            "installation":{
                "provider_binding_id":binding["provider_binding_id"],
                "installation":binding["installation"],
                "argv":["/usr/bin/python3"],"environment":{}
            },
            "work_root":"/tmp/jobs","driver":{"kind":"process"},
            "connection":{"kind":"direct","launcher":["/usr/local/bin/runner"]},
            "identity_file":null
        }))
        .unwrap();
        let mut assignment = json!({"descriptor":binding,"context_id":"test-context","publication_id":"test-publication",
            "requirements":{"driver":"process"}});
        validate_assignment_installation(&installation, &assignment).unwrap();

        let context = serde_json::from_value(json!({
            "driver":installation.driver,"connection":installation.connection,
            "installation_roots":["/opt/models"],"work_root":installation.work_root,
            "max_outstanding_assignments":1
        }))
        .unwrap();
        for key in ["HOME", "SLURM_JOB_ID", "PMI_RANK", "PMIX_NAMESPACE"] {
            for descriptor_map in [true, false] {
                let mut unsafe_installation = installation.clone();
                if descriptor_map {
                    unsafe_installation
                        .descriptor
                        .environment
                        .insert(key.into(), "untrusted-value".into());
                } else {
                    unsafe_installation
                        .installation
                        .environment
                        .insert(key.into(), "untrusted-value".into());
                }
                let mut pinned = assignment.clone();
                pinned["descriptor"] =
                    serde_json::to_value(&unsafe_installation.descriptor).unwrap();
                let error =
                    validate_assignment_installation(&unsafe_installation, &pinned).unwrap_err();
                assert!(error.message.contains(key));
                assert!(!error.message.contains("untrusted-value"));
                let error = validate_context(
                    &unsafe_installation,
                    &context,
                    &installation.descriptor,
                    false,
                )
                .unwrap_err();
                assert!(error.message.contains(key));
                let journal =
                    std::env::temp_dir().join(format!("enumeray-environment-{}", Uuid::new_v4()));
                let error = stage_runner(&journal, Uuid::new_v4(), &pinned, unsafe_installation)
                    .unwrap_err();
                assert!(error.message.contains(key));
                assert!(!journal.exists());
            }
            let mut descriptor = installation.descriptor.clone();
            descriptor
                .environment
                .insert(key.into(), "untrusted-value".into());
            let error = validate_context(&installation, &context, &descriptor, false).unwrap_err();
            assert!(error.message.contains(key));
        }

        for (field, replacement) in [
            ("release_path", json!("/opt/models/other-release")),
            ("entrypoint", json!(["other-wrapper"])),
            ("environment", json!({"MODEL_MODE":"different"})),
        ] {
            let mut changed = assignment.clone();
            changed["descriptor"][field] = replacement;
            assert!(validate_assignment_installation(&installation, &changed).is_err());
        }

        let mut redirected = installation.clone();
        redirected.context_id = "other".into();
        assert!(validate_assignment_installation(&redirected, &assignment).is_err());
        let journal = std::env::temp_dir().join(format!("enumeray-destination-{}", Uuid::new_v4()));
        assert!(stage_runner(&journal, Uuid::new_v4(), &assignment, redirected).is_err());
        assert!(!journal.exists());

        let mut other_provider = installation.clone();
        other_provider.provider_binding_id = "other".into();
        assert!(validate_assignment_installation(&other_provider, &assignment).is_err());
        let mut other_inner_provider = installation.clone();
        other_inner_provider.installation.provider_binding_id = "other".into();
        assert!(validate_assignment_installation(&other_inner_provider, &assignment).is_err());

        let mut other_context = installation.clone();
        other_context.context_id = "context-one".into();
        assert!(validate_assignment_installation(&other_context, &assignment).is_err());
        assignment["publication_id"] = json!("release-one");
        assert!(validate_assignment_installation(&other_context, &assignment).is_err());
        assignment["context_id"] = json!("context-one");
        validate_assignment_installation(&other_context, &assignment).unwrap();
        assignment["context_id"] = json!("context-two");
        assert!(validate_assignment_installation(&other_context, &assignment).is_err());

        let mut legacy = serde_json::to_value(&installation).unwrap();
        legacy["execution_site_id"] = json!("legacy-site");
        assert!(serde_json::from_value::<ComputeInstallation>(legacy).is_err());
        let mut serialized = serde_json::to_value(&installation).unwrap();
        serialized.as_object_mut().unwrap().remove("context_id");
        assert!(serde_json::from_value::<ComputeInstallation>(serialized).is_err());
    }

    #[test]
    #[ignore = "requires the real-backends Docker environment"]
    fn real_ssh_and_slurm() {
        assert_eq!(std::env::var("ENUMERAY_REAL_BACKENDS").as_deref(), Ok("1"));
        let directory =
            std::env::temp_dir().join(format!("enumeray-real-backends-{}", Uuid::new_v4()));
        local_process::create_private_directory(&directory).unwrap();
        let model: Value = serde_json::to_value(
            enumeray_contracts::models::parse_publication(include_bytes!(
                "../../../examples/local-scalar-model.json"
            ))
            .unwrap(),
        )
        .unwrap();
        let binding: Value = serde_json::from_str(include_str!(
            "../../../examples/local-scalar-installation.json"
        ))
        .unwrap();
        let deployment: Value = serde_json::from_str(include_str!(
            "../../../examples/local-scalar-requirements.json"
        ))
        .unwrap();
        let mut assignment = json!({"assignment_id":Uuid::new_v4(),"attempt_id":Uuid::new_v4(),"run_id":Uuid::new_v4(),"model_id":"scalar-double","publication_id":"local-v1","definition":model["definition"],"descriptor":binding,"requirements":deployment,"inputs":{"value":2}});
        assignment["publication_id"] = json!("test-publication");
        assignment["context_id"] = json!("test-context");
        assignment["descriptor"]["provider_binding_id"] = json!("real-test");
        let mut remote = ComputeInstallation {
            execution_mode: Default::default(),
            descriptor: serde_json::from_value(assignment["descriptor"].clone()).unwrap(),
            provider_binding_id: "real-test".into(),
            context_id: "test-context".into(),
            installation: local_process::Installation {
                provider_binding_id: "real-test".into(),
                installation: serde_json::from_value(binding["installation"].clone()).unwrap(),
                argv: vec![
                    "/usr/bin/python3".into(),
                    "/opt/enumeray/scalar-model-v1/wrapper.py".into(),
                    "execute".into(),
                ],
                environment: Default::default(),
                file_digests: Default::default(),
            },
            work_root: "/srv/enumeray-jobs".into(),
            connection: ConnectionConfig::Ssh(SshTarget {
                host: "127.0.0.1".into(),
                port: 2222,
                user: "model".into(),
                credential_reference: "test".into(),
                runner_path: "/usr/local/bin/enumeray".into(),
            }),
            identity_file: Some("/home/enumeray/.ssh/id_ed25519".into()),
            driver: DriverConfig::Process { scratch: None },
            slurm_script: None,
            shared_workspace: None,
        };
        fn finish(
            root: &Path,
            intent: Uuid,
            assignment: &Value,
            cancel: bool,
        ) -> local_process::Completion {
            let deadline = std::time::Instant::now() + Duration::from_secs(60);
            loop {
                match recover(
                    root,
                    intent,
                    assignment,
                    cancel.then_some(CancellationScope::Pending),
                    false,
                ) {
                    Ok(Some(done)) => return done,
                    Ok(None) => (),
                    // Scheduler handoff/accounting gaps may be unresolved. Keep
                    // observing this same launch until durable completion arrives.
                    Err(error)
                        if matches!(error.kind, ErrorKind::Waiting | ErrorKind::Retry)
                            || error.message
                                == "remote submission unresolved; retained launch will never be resubmitted" =>
                        {}
                    Err(error) => panic!("backend recovery failed: {error}"),
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "real backend did not finish"
                );
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        let ssh_target = remote.connection.clone();
        for (driver, access) in [
            ("process", "direct"),
            ("process", "ssh"),
            ("slurm", "direct"),
            ("slurm", "ssh"),
        ] {
            assignment["requirements"]["driver"] = json!(driver);
            remote.connection = if access == "direct" {
                ConnectionConfig::Direct {
                    launcher: vec![
                        "/usr/bin/sudo".into(),
                        "-n".into(),
                        "-u".into(),
                        "model".into(),
                        "--".into(),
                        "/usr/local/bin/enumeray".into(),
                    ],
                }
            } else {
                ssh_target.clone()
            };
            remote.driver = if driver == "slurm" {
                DriverConfig::Slurm(SlurmConfig {
                    worker_path: "/usr/local/bin/enumeray".into(),
                })
            } else {
                DriverConfig::Process { scratch: None }
            };
            remote.slurm_script =
                (driver == "slurm").then(|| "/opt/enumeray/scalar-model-v1/model.sbatch".into());
            let context = serde_json::from_value(json!({"driver":remote.driver,"connection":remote.connection,"installation_roots":["/opt/enumeray"],"work_root":remote.work_root,"max_outstanding_assignments":1})).unwrap();
            // Host diagnostics need no installed descriptor, publication or assignment.
            let probe_id = Uuid::new_v4();
            let mut probe = json!({"probe_id":probe_id,"definition":context,
                "connector_executable":(access=="direct").then_some("/usr/local/bin/enumeray"),
                "connector_paths":if access=="direct" {vec!["/home/enumeray/.ssh/id_ed25519"]} else {vec![]},"phase":"inspect"});
            let inspected = crate::connections::invoke_target(
                &remote.target(),
                "preflight",
                &remote.work_root,
                Some(&serde_json::to_vec(&probe).unwrap()),
            )
            .unwrap();
            assert_eq!(
                inspected["preflight"]["probe_created"], true,
                "{driver}/{access}: {inspected}"
            );
            assert!(
                inspected["preflight"]["checks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|c| c["outcome"] == "passed"),
                "{driver}/{access}: {inspected}"
            );
            probe["phase"] = json!("cleanup");
            probe["expected_probe_identity"] = inspected["preflight"]["probe_identity"].clone();
            assert_eq!(
                crate::connections::invoke_target(
                    &remote.target(),
                    "preflight",
                    &remote.work_root,
                    Some(&serde_json::to_vec(&probe).unwrap())
                )
                .unwrap()["cleaned"],
                true
            );
            let mut descriptor: enumeray_contracts::execution::InstallationDescriptor = serde_json::from_value(json!({"provider_binding_id":"real-test","installation":remote.installation.installation,"release_path":"/opt/enumeray/scalar-model-v1","entrypoint":["wrapper.py","execute"],"slurm_script":(driver == "slurm").then_some("model.sbatch"),"environment":{}})).unwrap();
            remote.installation.file_digests =
                validate_context(&remote, &context, &descriptor, false).unwrap();
            remote.descriptor = descriptor.clone();
            assignment["descriptor"] = serde_json::to_value(&descriptor).unwrap();
            validate_assignment_installation(&remote, &assignment).unwrap();
            assert!(!remote.installation.file_digests.is_empty());
            for key in ["HOME", "SLURM_JOB_ID"] {
                let mut unsafe_descriptor = descriptor.clone();
                unsafe_descriptor
                    .environment
                    .insert(key.into(), "private-override-value".into());
                let validation = ValidationRequest {
                    installation_roots: context.installation_roots.clone(),
                    driver: context.driver.clone(),
                    descriptor: unsafe_descriptor.clone(),
                };
                let error = connection(&remote)
                    .invoke(
                        &remote,
                        "validate",
                        &remote.work_root,
                        Some(&serde_json::to_vec(&validation).unwrap()),
                    )
                    .unwrap_err();
                let diagnostics = serde_json::to_value(
                    error.command.as_ref().expect("retained runner diagnostics"),
                )
                .unwrap();
                let stderr = base64::Engine::decode(
                    &base64::engine::general_purpose::STANDARD,
                    diagnostics["stderr"].as_str().unwrap(),
                )
                .unwrap();
                let stderr = String::from_utf8(stderr).unwrap();
                assert!(stderr.contains(key), "{driver}/{access}: {stderr}");
                assert!(!stderr.contains("private-override-value"));
                assert!(!error.message.contains("private-override-value"));
                let mut unsafe_binding = remote.clone();
                unsafe_binding.descriptor = unsafe_descriptor.clone();
                unsafe_binding.installation.environment = unsafe_descriptor.environment.clone();
                let mut unsafe_assignment = assignment.clone();
                unsafe_assignment["descriptor"] = serde_json::to_value(&unsafe_descriptor).unwrap();
                let unsafe_intent = Uuid::new_v4();
                let failed = launch(
                    &directory,
                    unsafe_intent,
                    &unsafe_assignment,
                    unsafe_binding.clone(),
                    false,
                )
                .unwrap()
                .unwrap();
                assert_eq!(
                    failed.report["failure"]["code"],
                    "launch_preparation_failed"
                );
                assert!(
                    failed.report["failure"]["message"]
                        .as_str()
                        .unwrap()
                        .contains(key)
                );
                assert!(failed.files.is_empty());
                assert!(failed.scheduler_evidence.is_none());
                assert_eq!(
                    finish(&directory, unsafe_intent, &unsafe_assignment, false).observed_at,
                    failed.observed_at
                );
                assert!(
                    launch(
                        &directory,
                        unsafe_intent,
                        &unsafe_assignment,
                        unsafe_binding,
                        false
                    )
                    .is_err()
                );
                let remote_directory =
                    remote_directory(&remote.execution_snapshot(), unsafe_intent);
                let failure_state = connection(&remote)
                    .invoke(&remote, "status", &remote_directory, None)
                    .unwrap();
                assert_eq!(failure_state["status"], "completed");
                if access == "direct" {
                    assert!(
                        !Path::new(&remote_directory)
                            .join("launch-intent.json")
                            .exists()
                    );
                    assert!(
                        !Path::new(&remote_directory)
                            .join("wrapper-started.json")
                            .exists()
                    );
                } else {
                    let mut inspect = ssh_command(&remote).unwrap();
                    inspect.arg(format!(
                        "test ! -e {}/launch-intent.json && test ! -e {}/wrapper-started.json",
                        quote(&remote_directory),
                        quote(&remote_directory)
                    ));
                    run_bounded(&mut inspect, None, Duration::from_secs(30)).unwrap();
                }
            }
            if access == "direct" {
                let mut unsafe_binding = remote.clone();
                unsafe_binding.connection = ConnectionConfig::Direct {
                    launcher: vec!["/usr/local/bin/enumeray".into()],
                };
                assert!(validate_context(&unsafe_binding, &context, &descriptor, false).is_err());
            }
            let intent = Uuid::new_v4();
            let outcome = launch(&directory, intent, &assignment, remote.clone(), false);
            assert!(
                outcome.is_ok()
                    || outcome
                        .as_ref()
                        .is_err_and(|e| e.kind == ErrorKind::Waiting
                            || e.message == "remote submission unresolved; retained launch will never be resubmitted"),
                "{driver}: {}",
                outcome.err().map(|e| e.message).unwrap_or_default()
            );
            let done = finish(&directory, intent, &assignment, false);
            assert_eq!(done.report["outputs"]["doubled"], 4);
            assert!(launch(&directory, intent, &assignment, remote.clone(), false).is_err());
            let replay = finish(&directory, intent, &assignment, false);
            assert_eq!(done.report["outputs"], replay.report["outputs"]);
            // TASK-040: execute actual CLI-generated projects in this existing
            // driver/connection matrix without replacing its recovery fixtures.
            for io in ["json", "files"] {
                let release = format!("/opt/enumeray/generated-{io}");
                let scientific = enumeray_contracts::models::parse_publication(
                    &fs::read(format!("{release}/publication.json")).unwrap(),
                )
                .unwrap();
                let scientific = serde_json::to_value(scientific).unwrap();
                let definition = scientific["definition"].clone();
                let identities: enumeray_contracts::execution::InstallationIdentity = serde_json::from_value(json!({
                    "model_identity":definition["model_identity"], "wrapper_identity":definition["wrapper_identity"],
                    "runtime_identity":definition["runtime_identity"], "wrapper_protocol_version":definition["wrapper_protocol_version"],
                    "dataset_identities":definition["dataset_identities"]
                })).unwrap();
                let mut generated_binding = remote.clone();
                generated_binding.provider_binding_id = format!("generated-{io}");
                generated_binding.installation.provider_binding_id = format!("generated-{io}");
                generated_binding.installation.installation = identities.clone();
                generated_binding.installation.argv = vec![
                    "/usr/bin/python3".into(),
                    format!("{release}/wrapper.py"),
                    "execute".into(),
                ];
                generated_binding.slurm_script =
                    (driver == "slurm").then(|| format!("{release}/model.sbatch"));
                let generated_descriptor = enumeray_contracts::execution::InstallationDescriptor {
                    provider_binding_id: format!("generated-{io}"),
                    installation: identities,
                    release_path: release.clone(),
                    entrypoint: vec!["wrapper.py".into(), "execute".into()],
                    environment: Default::default(),
                    slurm_script: (driver == "slurm").then(|| "model.sbatch".into()),
                };
                generated_binding.installation.file_digests =
                    validate_context(&generated_binding, &context, &generated_descriptor, false)
                        .unwrap();
                generated_binding.descriptor = generated_descriptor.clone();
                let mut generated_assignment = assignment.clone();
                generated_assignment["model_id"] = scientific["model"]["id"].clone();
                generated_assignment["publication_id"] = json!("starter-publication").clone();

                generated_assignment["descriptor"] =
                    serde_json::to_value(&generated_descriptor).unwrap();
                generated_assignment["definition"] = definition.clone();
                generated_assignment["requirements"] = serde_json::to_value(
                    enumeray_contracts::execution::ExecutionRequirements::derive(
                        generated_binding.driver.kind(),
                        &serde_json::from_value(definition.clone()).unwrap(),
                    ),
                )
                .unwrap();
                generated_assignment["inputs"] = definition["examples"][0]["inputs"].clone();
                for key in ["assignment_id", "run_id", "attempt_id"] {
                    generated_assignment[key] = json!(Uuid::new_v4());
                }
                let generated_intent = Uuid::new_v4();
                if io == "files" {
                    let artifact = Uuid::parse_str(
                        generated_assignment["inputs"]["dataset"]["artifact_id"]
                            .as_str()
                            .unwrap(),
                    )
                    .unwrap();
                    local_process::create_private_directory(&directory.join("input-staging"))
                        .unwrap();
                    let staged = crate::artifacts::input_directory(&directory, generated_intent);
                    local_process::create_private_directory(&staged).unwrap();
                    fs::copy(
                        format!("{release}/fixtures/dataset.csv"),
                        staged.join(artifact.to_string()),
                    )
                    .unwrap();
                }
                let launch_result = launch(
                    &directory,
                    generated_intent,
                    &generated_assignment,
                    generated_binding,
                    false,
                );
                assert!(launch_result.is_ok() || launch_result.as_ref().is_err_and(|e|e.kind==ErrorKind::Waiting || e.message=="remote submission unresolved; retained launch will never be resubmitted"),"{driver}/{access}/{io}: {}",launch_result.err().map(|e|e.message).unwrap_or_default());
                let completed = finish(&directory, generated_intent, &generated_assignment, false);
                assert_eq!(
                    completed.report["outcome"], "succeeded",
                    "{driver}/{access}/{io}: {}",
                    completed.report["failure"]
                );
                if io == "json" {
                    assert_eq!(completed.report["outputs"]["doubled"], 4);
                } else {
                    assert_eq!(completed.report["outputs"]["sample_count"], 2);
                    let collected = collect_files(
                        &directory,
                        generated_intent,
                        &generated_assignment,
                        &completed.files,
                    )
                    .unwrap();
                    assert_eq!(
                        fs::read(collected.join("processed.csv")).unwrap(),
                        fs::read(format!("{release}/fixtures/expected.csv")).unwrap()
                    );
                }
            }
            // Every access path must retain rejection of changed pinned files
            // without asking the backend to execute scientific work.
            let mut changed_binding = remote.clone();
            changed_binding.installation.file_digests.insert(
                "/opt/enumeray/scalar-model-v1/wrapper.py".into(),
                "invalid-pinned-digest".into(),
            );
            let changed_intent = Uuid::new_v4();
            let changed = launch(
                &directory,
                changed_intent,
                &assignment,
                changed_binding.clone(),
                false,
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                changed.report["failure"]["code"], "launch_preparation_failed",
                "{driver}/{access}"
            );
            assert_eq!(
                finish(&directory, changed_intent, &assignment, false).observed_at,
                changed.observed_at
            );
            assert!(
                launch(
                    &directory,
                    changed_intent,
                    &assignment,
                    changed_binding,
                    false
                )
                .is_err()
            );
            if driver == "slurm" {
                for (script, code) in [
                    ("rejected.sbatch", "launch_rejected"),
                    (
                        "worker-preparation-failure.sbatch",
                        "worker_preparation_failed",
                    ),
                ] {
                    let mut failed_binding = remote.clone();
                    failed_binding.slurm_script =
                        Some(format!("/opt/enumeray/scalar-model-v1/{script}"));
                    let mut failed_descriptor = descriptor.clone();
                    failed_descriptor.slurm_script = Some(script.into());
                    failed_binding.installation.file_digests =
                        validate_context(&failed_binding, &context, &failed_descriptor, false)
                            .unwrap();
                    failed_binding.descriptor = failed_descriptor.clone();
                    let mut failed_assignment = assignment.clone();
                    failed_assignment["descriptor"] =
                        serde_json::to_value(&failed_descriptor).unwrap();
                    let failed_intent = Uuid::new_v4();
                    let _ = launch(
                        &directory,
                        failed_intent,
                        &failed_assignment,
                        failed_binding.clone(),
                        false,
                    );
                    let failed = finish(&directory, failed_intent, &failed_assignment, false);
                    assert_eq!(failed.report["failure"]["code"], code, "{access}/{script}");
                    assert_eq!(failed.report["outcome"], "failed");
                    assert!(failed.startup_evidence.is_some());
                    assert!(failed.files.is_empty());
                    assert_eq!(
                        failed.scheduler_evidence.is_some(),
                        code == "worker_preparation_failed"
                    );
                    let replay = finish(&directory, failed_intent, &failed_assignment, false);
                    assert_eq!(replay.observed_at, failed.observed_at);
                    assert!(replay.report == *failed.report);
                    assert!(
                        launch(
                            &directory,
                            failed_intent,
                            &failed_assignment,
                            failed_binding,
                            false
                        )
                        .is_err()
                    );
                }
            }
            if driver == "slurm" {
                let mut failed_binding = remote.clone();
                failed_binding.slurm_script =
                    Some("/opt/enumeray/scalar-model-v1/failure-before-worker.sbatch".into());
                let mut failed_descriptor = descriptor.clone();
                failed_descriptor.slurm_script = Some("failure-before-worker.sbatch".into());
                failed_binding.installation.file_digests =
                    validate_context(&failed_binding, &context, &failed_descriptor, false).unwrap();
                failed_binding.descriptor = failed_descriptor.clone();
                let mut failed_assignment = assignment.clone();
                failed_assignment["descriptor"] = serde_json::to_value(&failed_descriptor).unwrap();
                let failed_intent = Uuid::new_v4();
                let _ = launch(
                    &directory,
                    failed_intent,
                    &failed_assignment,
                    failed_binding.clone(),
                    false,
                );
                let failed = finish(&directory, failed_intent, &failed_assignment, false);
                assert_eq!(failed.report["failure"]["code"], "slurm_failed", "{access}");
                assert_eq!(failed.report["execution_terminated"], true);
                let evidence = failed.scheduler_evidence.as_ref().unwrap();
                assert!(!evidence.worker_started);
                assert_eq!(evidence.allocation.exit_status, 23);
                assert!(failed.files.is_empty());
                assert!(
                    launch(
                        &directory,
                        failed_intent,
                        &failed_assignment,
                        failed_binding,
                        false
                    )
                    .is_err()
                );
                let replay = finish(&directory, failed_intent, &failed_assignment, false);
                assert_eq!(replay.observed_at, failed.observed_at);
                assert_eq!(replay.scheduler_evidence, failed.scheduler_evidence);
            }
            let mut file_assignment = assignment.clone();
            let input_model: Value = serde_json::to_value(
                enumeray_contracts::models::parse_publication(include_bytes!(
                    "../../../examples/artifact-input.json"
                ))
                .unwrap(),
            )
            .unwrap();
            let output_model: Value = serde_json::to_value(
                enumeray_contracts::models::parse_publication(include_bytes!(
                    "../../../examples/artifact-output.json"
                ))
                .unwrap(),
            )
            .unwrap();
            file_assignment["definition"]["input_schema"] =
                input_model["definition"]["input_schema"].clone();
            file_assignment["definition"]["output_schema"] =
                output_model["definition"]["output_schema"].clone();
            file_assignment["definition"]["wrapper_protocol_version"] = json!(1);
            file_assignment["definition"]["log_files"] = json!({"solver":{"path":"solver.log"}});
            file_assignment["descriptor"]["installation"]["wrapper_protocol_version"] = json!(1);
            let artifact = Uuid::new_v4();
            file_assignment["inputs"] = json!({"dataset":{"artifact_id":artifact}});
            let file_intent = Uuid::new_v4();
            local_process::create_private_directory(&directory.join("input-staging")).unwrap();
            let staged = crate::artifacts::input_directory(&directory, file_intent);
            local_process::create_private_directory(&staged).unwrap();
            // Exercise complete SFTP packets as well as a resumed prefix.
            let mut input_bytes = vec![b' '; 1024 * 1024];
            input_bytes[..2].copy_from_slice(b"42");
            fs::write(staged.join(artifact.to_string()), &input_bytes).unwrap();
            let mut file_binding = remote.clone();
            file_binding
                .installation
                .installation
                .wrapper_protocol_version = 1;
            file_binding.installation.argv[1] = "/opt/enumeray/file-model-v1/wrapper.py".into();
            file_binding.slurm_script =
                (driver == "slurm").then(|| "/opt/enumeray/file-model-v1/model.sbatch".into());
            file_binding
                .installation
                .environment
                .insert("ENUMERAY_ACCEPTANCE_DELAY".into(), "4".into());
            if driver == "slurm" {
                file_binding
                    .installation
                    .environment
                    .insert("ENUMERAY_ACCEPTANCE_EXPECT_SLURM_SETUP".into(), "1".into());
            }
            descriptor.release_path = "/opt/enumeray/file-model-v1".into();
            descriptor.environment = file_binding.installation.environment.clone();
            file_binding.descriptor = descriptor.clone();
            file_assignment["descriptor"] = serde_json::to_value(&descriptor).unwrap();
            file_assignment["requirements"] = serde_json::to_value(
                enumeray_contracts::execution::ExecutionRequirements::derive(
                    file_binding.driver.kind(),
                    &serde_json::from_value(file_assignment["definition"].clone()).unwrap(),
                ),
            )
            .unwrap();
            file_binding.installation.file_digests =
                validate_context(&file_binding, &context, &descriptor, false).unwrap();
            // Simulate a crash during input transfer, before any start request.
            let evidence = evidence_directory(&directory, file_intent);
            local_process::create_private_directory(&evidence).unwrap();
            let job = ComputeJob {
                log_policy: enumeray_contracts::logs::LogPolicy {
                    archive_enabled: true,
                    ..Default::default()
                },
                version: 1,
                intent: file_intent,
                assignment: file_assignment.clone(),
                execution: file_binding.execution_snapshot(),
                input_digests: std::collections::BTreeMap::from([(
                    artifact,
                    crate::artifacts::file_digest(&staged.join(artifact.to_string())).unwrap(),
                )]),
            };
            local_process::write_record(&evidence, "job.json", &job).unwrap();
            local_process::write_record(&evidence, "connection.json", &file_binding).unwrap();
            invoke(
                &directory,
                &job,
                "prepare",
                Some(&serde_json::to_vec(&job).unwrap()),
            )
            .unwrap();
            invoke(&directory, &job, "write-input", Some(&serde_json::to_vec(&json!({
                "artifact_id": artifact, "offset": 0, "data": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"4")
            })).unwrap())).unwrap();
            // Resume staging, then revoke after the wrapper starts. Both drivers
            // must drain this existing run through direct and SSH connections.
            let _ = recover(&directory, file_intent, &file_assignment, None, false);
            let deadline = std::time::Instant::now() + Duration::from_secs(30);
            loop {
                let value = invoke(&directory, &job, "status", None).unwrap();
                if value["started"] == true {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "wrapper did not start"
                );
                std::thread::sleep(Duration::from_millis(100));
            }
            assert_eq!(
                request_cancel(&directory, &job, CancellationScope::Pending).unwrap(),
                if driver == "slurm" {
                    CancellationOutcome::Accepted
                } else {
                    CancellationOutcome::Unsupported
                }
            );
            let done = finish(&directory, file_intent, &file_assignment, true);
            assert_eq!(
                done.report["outcome"], "succeeded",
                "{driver} file wrapper: {}",
                done.report["failure"]
            );
            // Wrapper completion can precede Slurm allocation release. Named
            // logs may only close after that independent fence is confirmed.
            let deadline = std::time::Instant::now() + Duration::from_secs(60);
            while !allocation_released(&directory, file_intent, &file_assignment).unwrap() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "{driver}/{access}: allocation was not released after wrapper completion"
                );
                std::thread::sleep(Duration::from_millis(100));
            }
            let named = read_logs(
                &directory,
                file_intent,
                &enumeray_contracts::logs::ReadLogsRequest {
                    source: enumeray_contracts::logs::LogSource::model("solver").unwrap(),
                    epoch: 1,
                    offset: 0,
                    complete: true,
                },
            )
            .unwrap();
            assert!(named.closed && !named.unavailable);
            assert_eq!(
                base64::Engine::decode(&base64::engine::general_purpose::STANDARD, named.data)
                    .unwrap(),
                b"solver converged\0\xff\n"
            );
            for (source, byte, produced) in [
                (
                    enumeray_contracts::logs::LogSource::Stdout,
                    b'o',
                    2 * 1024 * 1024 + 17_u64,
                ),
                (
                    enumeray_contracts::logs::LogSource::Stderr,
                    b'e',
                    2 * 1024 * 1024 + 31_u64,
                ),
            ] {
                let chunk = read_logs(
                    &directory,
                    file_intent,
                    &enumeray_contracts::logs::ReadLogsRequest {
                        source,
                        epoch: 1,
                        offset: produced - 17,
                        complete: true,
                    },
                )
                .unwrap();
                assert_eq!(chunk.end, produced);
                assert!(chunk.earliest > 0);
                assert!(chunk.closed);
                assert_eq!(
                    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, chunk.data)
                        .unwrap(),
                    vec![byte; 17]
                );
                assert!(
                    !local_process::job_directory(&directory, file_intent)
                        .join("workspace/stdout.log")
                        .exists()
                );
            }
            // Retain one verified byte before collection on every driver/access
            // combination. Recovery must read only the remaining suffix.
            let transfer_root = directory.join("artifact-transfers");
            local_process::create_private_directory(&transfer_root).unwrap();
            let transfer_root = transfer_root.join(file_intent.to_string());
            local_process::create_private_directory(&transfer_root).unwrap();
            let staging = transfer_root.join("output-staging");
            local_process::create_private_directory(&staging).unwrap();
            let partial = staging.join(format!(
                "{}.part",
                crate::artifacts::transfer_key(file_intent, "answer.txt", false, 0)
            ));
            let file = done
                .files
                .values()
                .find(|file| file.path == "answer.txt")
                .unwrap();
            let evidence = enumeray_client::bulk::DownloadEvidence {
                scope: crate::connections::transfer_scope(
                    &file_binding,
                    &remote_directory(&job.execution, file_intent),
                )
                .unwrap(),
                source: file.path.clone(),
                version_id: None,
                size_bytes: file.size_bytes,
                checksum_sha256: file.sha256.clone(),
            };
            let mut retained = enumeray_client::bulk::RetainedTransfer::open(
                &partial,
                evidence,
                std::time::Instant::now() + Duration::from_secs(60),
                |_| {},
            )
            .unwrap();
            retained.append(b"8").unwrap();
            retained.checkpoint().unwrap();
            drop(retained);
            let outputs =
                collect_files(&directory, file_intent, &file_assignment, &done.files).unwrap();
            assert_eq!(fs::read(outputs.join("answer.txt")).unwrap(), b"84");
            // Archive collection has the same prefix proof on all four paths,
            // while retaining its independent finalization and cleanup fences.
            let archive = invoke(&directory, &job, "build-log-archive", None).unwrap();
            let built: crate::log_archive::Built =
                serde_json::from_value(archive["archive"].clone()).unwrap();
            let chunk = invoke(
                &directory,
                &job,
                "read-log-archive",
                Some(br#"{"offset":0}"#),
            )
            .unwrap();
            let prefix = base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                chunk["data"].as_str().unwrap(),
            )
            .unwrap();
            let archive_root =
                evidence_directory(&directory, file_intent).join("archive-copy/log-archives");
            local_process::create_private_directory(archive_root.parent().unwrap()).unwrap();
            local_process::create_private_directory(&archive_root).unwrap();
            let temporary = archive_root.join("logs.tar.partial");
            let evidence = enumeray_client::bulk::DownloadEvidence {
                scope: crate::connections::transfer_scope(
                    &file_binding,
                    &remote_directory(&job.execution, file_intent),
                )
                .unwrap(),
                source: built.file.path.clone(),
                version_id: None,
                size_bytes: built.file.size_bytes,
                checksum_sha256: built.file.sha256.clone(),
            };
            let mut retained = enumeray_client::bulk::RetainedTransfer::open(
                &temporary,
                evidence,
                deadline,
                |_| {},
            )
            .unwrap();
            retained.append(&prefix[..8]).unwrap();
            retained.checkpoint().unwrap();
            drop(retained);
            let (archive_root, collected) = collect_archive(&directory, file_intent).unwrap();
            assert_eq!(collected.file.sha256, built.file.sha256);
            assert_eq!(
                &fs::read(archive_root.join(&built.file.path)).unwrap()[..8],
                &prefix[..8]
            );
            // Recover file collection without starting a second model process.
            fs::remove_file(outputs.join("answer.txt")).unwrap();
            fs::write(outputs.join("answer.enumeray-transfer-partial"), b"partial").unwrap();
            let outputs =
                collect_files(&directory, file_intent, &file_assignment, &done.files).unwrap();
            assert_eq!(fs::read(outputs.join("answer.txt")).unwrap(), b"84");
            // User cancellation, unlike revocation, stops running work on every connection.
            let cancel_intent = Uuid::new_v4();
            let mut cancel_assignment = file_assignment.clone();
            for key in ["assignment_id", "run_id", "attempt_id"] {
                cancel_assignment[key] = json!(Uuid::new_v4());
            }
            let mut cancel_binding = file_binding.clone();
            cancel_binding
                .installation
                .environment
                .insert("ENUMERAY_ACCEPTANCE_DELAY".into(), "30".into());
            cancel_binding.descriptor.environment = cancel_binding.installation.environment.clone();
            cancel_assignment["descriptor"] =
                serde_json::to_value(&cancel_binding.descriptor).unwrap();
            let staged = crate::artifacts::input_directory(&directory, cancel_intent);
            local_process::create_private_directory(&staged).unwrap();
            fs::write(staged.join(artifact.to_string()), &input_bytes).unwrap();
            let _ = launch(
                &directory,
                cancel_intent,
                &cancel_assignment,
                cancel_binding.clone(),
                false,
            );
            let cancel_job: ComputeJob = local_process::read_record(
                &evidence_directory(&directory, cancel_intent).join("job.json"),
            )
            .unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(60);
            loop {
                if invoke(&directory, &cancel_job, "status", None).unwrap()["started"] == true {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "cancelled wrapper did not start"
                );
                std::thread::sleep(Duration::from_millis(100));
            }
            // Ignore acceptance, then recover repeatedly through new runner invocations.
            let _ = request_cancel(&directory, &cancel_job, CancellationScope::Running);
            let cancelled = loop {
                if let Ok(Some(done)) = recover(
                    &directory,
                    cancel_intent,
                    &cancel_assignment,
                    Some(CancellationScope::Running),
                    false,
                ) {
                    break done;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "user cancellation not confirmed for {driver}/{access}"
                );
                std::thread::sleep(Duration::from_millis(100));
            };
            assert_eq!(
                cancelled.report["outcome"], "cancelled",
                "{driver}/{access}"
            );
            assert_eq!(cancelled.report["execution_terminated"], true);
            assert_eq!(
                request_cancel(&directory, &cancel_job, CancellationScope::Running).unwrap(),
                CancellationOutcome::AlreadyTerminal
            );
            assert!(
                launch(
                    &directory,
                    cancel_intent,
                    &cancel_assignment,
                    remote.clone(),
                    false
                )
                .is_err()
            );
            if driver == "slurm" {
                // Cancel outside the connector: there must be no user intent
                // and a stopped worker without completion must still reconcile.
                let external_intent = Uuid::new_v4();
                let staged = crate::artifacts::input_directory(&directory, external_intent);
                local_process::create_private_directory(&staged).unwrap();
                fs::write(staged.join(artifact.to_string()), b"42").unwrap();
                let _ = launch(
                    &directory,
                    external_intent,
                    &cancel_assignment,
                    cancel_binding.clone(),
                    false,
                );
                let external_job: ComputeJob = local_process::read_record(
                    &evidence_directory(&directory, external_intent).join("job.json"),
                )
                .unwrap();
                let deadline = std::time::Instant::now() + Duration::from_secs(60);
                loop {
                    if invoke(&directory, &external_job, "status", None).unwrap()["started"] == true
                    {
                        break;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "external cancellation worker did not start"
                    );
                    std::thread::sleep(Duration::from_millis(100));
                }
                let mut scheduler_access = remote.clone();
                scheduler_access.connection = ssh_target.clone();
                let mut inspect = ssh_command(&scheduler_access).unwrap();
                inspect.arg(format!(
                    "cat {}/slurm.json",
                    quote(&remote_directory(&external_job.execution, external_intent))
                ));
                let handle: Value = serde_json::from_slice(
                    &run_bounded(&mut inspect, None, Duration::from_secs(20)).unwrap(),
                )
                .unwrap();
                let mut cancel = ssh_command(&scheduler_access).unwrap();
                cancel.arg(format!(
                    "scancel --clusters={} {}",
                    quote(handle["cluster"].as_str().unwrap()),
                    handle["job_id"].as_u64().unwrap()
                ));
                run_bounded(&mut cancel, None, Duration::from_secs(20)).unwrap();
                let done = finish(&directory, external_intent, &cancel_assignment, false);
                assert_eq!(done.report["outcome"], "failed", "{access}");
                assert_eq!(done.report["failure"]["code"], "slurm_cancelled");
                assert_eq!(done.report["execution_terminated"], true);
                let evidence = done.scheduler_evidence.as_ref().unwrap();
                assert!(evidence.worker_started);
                assert_eq!(evidence.cancellation_scope, None);
                assert!(done.files.is_empty());
                assert!(
                    launch(
                        &directory,
                        external_intent,
                        &cancel_assignment,
                        cancel_binding,
                        false
                    )
                    .is_err()
                );
                assert_eq!(
                    finish(&directory, external_intent, &cancel_assignment, false).observed_at,
                    done.observed_at
                );
            }
        }
        // Revocation observed before transport submission remains final even if
        // authorization is restored before the next recovery cycle.
        let revoked_intent = Uuid::new_v4();
        let revoked_evidence = evidence_directory(&directory, revoked_intent);
        local_process::create_private_directory(&revoked_evidence).unwrap();
        local_process::write_record(
            &revoked_evidence,
            "job.json",
            &ComputeJob {
                log_policy: Default::default(),
                version: 1,
                intent: revoked_intent,
                assignment: assignment.clone(),
                execution: remote.execution_snapshot(),
                input_digests: Default::default(),
            },
        )
        .unwrap();
        let revoked = recover(
            &directory,
            revoked_intent,
            &assignment,
            Some(CancellationScope::Pending),
            false,
        )
        .unwrap()
        .unwrap();
        let replay = recover(&directory, revoked_intent, &assignment, None, false)
            .unwrap()
            .unwrap();
        assert_eq!(revoked.report["failure"]["code"], "access_revoked");
        assert_eq!(revoked.observed_at, replay.observed_at);
        assert!(!revoked_evidence.join("start-requested.json").exists());
        // Occupy the only allocated CPU, creating an actual scheduler-pending job.
        let mut command = ssh_command(&remote).unwrap();
        command.arg("sbatch --parsable --account=research --partition=debug --cpus-per-task=1 --mem=128 --time=2 --wrap='sleep 90'");
        let blocker =
            parse_sbatch(&run_bounded(&mut command, None, Duration::from_secs(20)).unwrap())
                .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            let mut query = ssh_command(&remote).unwrap();
            query.arg(format!(
                "squeue --noheader --jobs {} --format=%T",
                blocker.job_id
            ));
            if String::from_utf8_lossy(
                &run_bounded(&mut query, None, Duration::from_secs(20)).unwrap(),
            )
            .trim()
                == "RUNNING"
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "blocker did not start"
            );
            std::thread::sleep(Duration::from_millis(500));
        }
        let intent = Uuid::new_v4();
        let _ = launch(&directory, intent, &assignment, remote.clone(), false);
        // Lose the sbatch receipt after a real submission; unique job-name lookup
        // must recover the existing pending scheduler job without resubmission.
        let mut lose_receipt = ssh_command(&remote).unwrap();
        let job_dir = quote(&remote_directory(&remote.execution_snapshot(), intent));
        lose_receipt.arg(format!(
            "rm {job_dir}/slurm.json {job_dir}/slurm-receipt.json"
        ));
        run_bounded(&mut lose_receipt, None, Duration::from_secs(20)).unwrap();
        let pending_job: ComputeJob =
            local_process::read_record(&evidence_directory(&directory, intent).join("job.json"))
                .unwrap();
        assert_eq!(
            invoke(&directory, &pending_job, "status", None).unwrap()["status"],
            "pending"
        );
        let mut recovered_handle = ssh_command(&remote).unwrap();
        recovered_handle.arg(format!("cat {job_dir}/slurm.json"));
        let handle: Value = serde_json::from_slice(
            &run_bounded(&mut recovered_handle, None, Duration::from_secs(20)).unwrap(),
        )
        .unwrap();
        assert_eq!(handle["cluster"], "enumeray-acceptance");
        assert!(handle["job_id"].as_u64().is_some_and(|id| id > 0));
        // Discard a real cancellation reply, then repeat with the recovered,
        // durable handle. Neither reply can publish terminal completion.
        assert_eq!(
            request_cancel(&directory, &pending_job, CancellationScope::Pending).unwrap(),
            CancellationOutcome::Accepted
        );
        let repeated =
            request_cancel(&directory, &pending_job, CancellationScope::Pending).unwrap();
        // Queue removal can precede accounting publication. A repeated request
        // may be unresolved during that gap; only later observation completes it.
        assert!(
            matches!(
                repeated,
                CancellationOutcome::Accepted | CancellationOutcome::AlreadyTerminal
            ) || matches!(&repeated, CancellationOutcome::Unresolved { reason }
                    if reason == "scheduler observation unavailable before cancellation"
                        || reason == "cancellation reply absent; observe or repeat the same request"),
            "{repeated:?}"
        );
        let cancelled = finish(&directory, intent, &assignment, true);
        let stopped = cancelled.scheduler_evidence.as_ref().unwrap();
        assert!(!stopped.worker_started);
        assert_eq!(stopped.cancellation_scope, Some(CancellationScope::Pending));
        assert_eq!(stopped.allocation.state, "CANCELLED");
        // Some Slurm versions fill Start at cancellation for an unallocated job.
        // Preserve that evidence and use the conservative scheduler failure code.
        assert_eq!(
            cancelled.report["failure"]["code"],
            if stopped.allocation.started_at.is_none() {
                "access_revoked"
            } else {
                "slurm_cancelled"
            }
        );
        let mut query = ssh_command(&remote).unwrap();
        query.arg(format!(
            "squeue --noheader --jobs {} --format=%T",
            blocker.job_id
        ));
        let state = run_bounded(&mut query, None, Duration::from_secs(20)).unwrap();
        assert_eq!(String::from_utf8_lossy(&state).trim(), "RUNNING");
        let mut cleanup = ssh_command(&remote).unwrap();
        cleanup.arg(format!("scancel {}", blocker.job_id));
        run_bounded(&mut cleanup, None, Duration::from_secs(20)).unwrap();
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn lost_cancel_reply_still_observes_the_original_execution() {
        let root =
            std::env::temp_dir().join(format!("enumeray-cancel-transport-{}", Uuid::new_v4()));
        local_process::create_private_directory(&root).unwrap();
        let launcher = root.join("launcher.py");
        fs::write(
            &launcher,
            r#"import json, sys
from pathlib import Path
root = Path(__file__).parent
action = sys.argv[2]
request = json.load(sys.stdin)
with (root / 'calls').open('a') as calls:
    calls.write(action + '\n')
if action == 'request-cancel':
    assert request['payload'] == {'scope': 'pending'}
    sys.exit(1)
assert action == 'status'
print(json.dumps({'version': 1, 'payload': {'status': 'running', 'started': True}}))
"#,
        )
        .unwrap();
        let binding: Value = serde_json::from_str(include_str!(
            "../../../examples/local-scalar-installation.json"
        ))
        .unwrap();
        let installation = ComputeInstallation {
            execution_mode: Default::default(),
            descriptor: serde_json::from_value(binding.clone()).unwrap(),
            provider_binding_id: "test".into(),
            context_id: "test-context".into(),
            installation: local_process::Installation {
                provider_binding_id: "test".into(),
                installation: serde_json::from_value(binding["installation"].clone()).unwrap(),
                argv: vec!["/unused/model".into()],
                environment: Default::default(),
                file_digests: Default::default(),
            },
            work_root: root.to_string_lossy().into_owned(),
            connection: ConnectionConfig::Direct {
                launcher: vec![
                    "/usr/bin/python3".into(),
                    launcher.to_string_lossy().into_owned(),
                ],
            },
            identity_file: None,
            driver: DriverConfig::Process { scratch: None },
            slurm_script: None,
            shared_workspace: None,
        };
        let intent = Uuid::new_v4();
        let assignment = json!({"attempt_id":Uuid::new_v4()});
        let job = ComputeJob {
            log_policy: Default::default(),
            version: 1,
            intent,
            assignment: assignment.clone(),
            execution: installation.execution_snapshot(),
            input_digests: Default::default(),
        };
        let evidence = evidence_directory(&root, intent);
        local_process::create_private_directory(evidence.parent().unwrap()).unwrap();
        local_process::create_private_directory(&evidence).unwrap();
        local_process::write_record(&evidence, "job.json", &job).unwrap();
        local_process::write_record(&evidence, "connection.json", &installation).unwrap();
        local_process::write_record(&evidence, "start-requested.json", &json!({"intent":intent}))
            .unwrap();
        for _ in 0..2 {
            assert!(
                recover(
                    &root,
                    intent,
                    &assignment,
                    Some(CancellationScope::Pending),
                    false
                )
                .unwrap()
                .is_none()
            );
        }
        assert_eq!(
            fs::read_to_string(root.join("calls")).unwrap(),
            "request-cancel\nstatus\nrequest-cancel\nstatus\n"
        );
        let local = local_process::job_directory(&root, intent);
        assert!(local_process::read_record::<bool>(&local.join("started")).unwrap());
        assert!(!local.join("workspace").exists());
        assert!(!local.join("log-captures").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scheduler_receipts_require_exact_unambiguous_identity() {
        assert_eq!(
            parse_sbatch(b"123;cluster-a\n").unwrap(),
            SbatchReceipt {
                job_id: 123,
                cluster: Some("cluster-a".into())
            }
        );
        for invalid in [
            "0",
            "123;bad cluster",
            "123;a;b",
            "123\n456",
            "Submitted batch job 123",
        ] {
            assert!(parse_sbatch(invalid.as_bytes()).is_err(), "{invalid}");
        }
    }
    #[test]
    fn shell_arguments_are_single_quoted_and_paths_are_confined() {
        assert_eq!(quote("a'b"), "'a'\\''b'");
        assert!(!safe_absolute("/jobs/../secrets"));
        assert!(!safe_absolute("/jobs\nlaunch"));
        assert!(safe_absolute("/jobs/releases/v1"));
    }
}
