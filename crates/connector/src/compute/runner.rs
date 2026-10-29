//! Credential-free compute operations. JSON is decoded at the protocol boundary.

use super::{ComputeJob, ValidationRequest, completion_response, execution_uid, remote_directory};
use crate::{
    Result,
    drivers::{CancellationOutcome, CancellationScope, Observation},
    failure, journal, local_process,
    protocol::{CancellationResponse, InputChunk, Operation, OutputRequest},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
};

pub(crate) fn run(operation: Operation, directory: &Path) -> Result<Value> {
    let _payload = if !matches!(
        &operation,
        Operation::Identity(_)
            | Operation::Validate(_)
            | Operation::Prepare(_)
            | Operation::CleanupRun(_)
            | Operation::ProtectInstallations(_)
    ) {
        Some(crate::cleanup::payload_fence(directory, true)?)
    } else {
        None
    };
    match operation {
        Operation::Identity(_) => Ok(json!({"status":"ready", "execution_uid":execution_uid()?})),
        Operation::ProtectInstallations(request) => {
            if request.references.len() > 128 {
                return Err(failure(
                    "installation protection exceeds its reference bound",
                ));
            }
            let _fences = crate::cleanup::pin_references(&request.references)?;
            Ok(json!({"status":"completed"}))
        }
        Operation::CleanupRun(request) => cleanup_run(directory, &load_job(directory)?, request),
        Operation::Validate(validation) => validate(directory, validation),
        Operation::Prepare(job) => prepare(directory, job),
        Operation::WriteInput(chunk) => write_input(directory, &load_job(directory)?, chunk),
        Operation::WriteCollection(page) => {
            let _lock = submission_lock(directory)?;
            input_open(directory)?;
            crate::collections::install_page(directory, &load_job(directory)?.assignment, page)
        }
        Operation::ReadCollection(request) => {
            let job = load_job(directory)?;
            let done = local_process::recover_worker(directory, job.intent, &job.assignment)?
                .ok_or_else(|| failure("collection not completed"))?;
            crate::collections::output_page(directory, &done, request)
        }
        Operation::TransferProbe(request) => {
            transfer_probe(directory, &load_job(directory)?, request)
        }
        Operation::ReadOutput(request) => read_output(directory, &load_job(directory)?, request),
        Operation::BuildLogArchive(_) => {
            let job = load_job(directory)?;
            refuse_retired_payload(directory)?;
            if allocation_status(directory, &job)?["status"] != "completed" {
                return Err(failure("archive requires released allocation"));
            }
            if matches!(
                job.execution.driver,
                enumeray_contracts::execution::DriverConfig::Slurm(_)
            ) {
                crate::capture::collect_batch(
                    directory,
                    &crate::capture::allowance(&job.log_policy, &job.assignment)?,
                    true,
                )?;
            }
            let _ =
                crate::capture::collect_named(directory, &job.assignment, &job.log_policy, true);
            let built =
                crate::log_archive::build(directory, job.intent, &job.assignment, &job.log_policy)?;
            Ok(json!({"status":"completed","archive":built}))
        }
        Operation::ReadLogArchive(request) => {
            let _job = load_job(directory)?;
            refuse_retired_payload(directory)?;
            let built: crate::log_archive::Built =
                local_process::read_record(&directory.join("log-archives/archive.json"))?;
            if request.offset > built.file.size_bytes {
                return Err(failure("archive offset exceeds finalized size"));
            }
            use std::io::{Read, Seek, SeekFrom};
            let path = crate::artifacts::confined_file(directory, "log-archives/logs.tar.gz")?;
            let mut file =
                fs::File::open(path).map_err(|_| failure("cannot open finalized archive"))?;
            file.seek(SeekFrom::Start(request.offset))
                .map_err(|_| failure("cannot seek archive"))?;
            let mut bytes = Vec::new();
            file.take(256 * 1024)
                .read_to_end(&mut bytes)
                .map_err(|_| failure("cannot read archive"))?;
            Ok(json!({"data":STANDARD.encode(bytes)}))
        }
        Operation::CleanupLogArchive(_) => {
            let job = load_job(directory)?;
            if allocation_status(directory, &job)?["status"] != "completed" {
                return Err(failure("archive cleanup requires released allocation"));
            }
            crate::log_archive::cleanup(directory)?;
            Ok(json!({"status":"completed"}))
        }
        Operation::ReadLogs(mut request) => {
            let job = load_job(directory)?;
            refuse_retired_payload(directory)?;
            if !crate::capture::sources(&job.assignment)?.contains(&request.source) {
                return Err(failure("undeclared log source"));
            }
            if matches!(
                request.source,
                enumeray_contracts::logs::LogSource::Model(_)
            ) {
                let released = request.complete
                    && allocation_status(directory, &job)?["status"] == "completed";
                if released {
                    let _ = crate::capture::collect_named(
                        directory,
                        &job.assignment,
                        &job.log_policy,
                        true,
                    );
                }
                request.complete = released;
            }
            if request.source == enumeray_contracts::logs::LogSource::SlurmBatch {
                if !matches!(
                    job.execution.driver,
                    enumeray_contracts::execution::DriverConfig::Slurm(_)
                ) {
                    return Err(failure("batch source requires Slurm"));
                }
                // complete is a request to verify the fence, never authority to close.
                let released = request.complete
                    && allocation_status(directory, &job)?["status"] == "completed";
                crate::capture::collect_batch(
                    directory,
                    &crate::capture::allowance(&job.log_policy, &job.assignment)?,
                    released,
                )?;
                request.complete = released;
            }
            serde_json::to_value(crate::capture::read(directory, &request)?)
                .map_err(|_| failure("cannot encode log chunk"))
        }
        Operation::Start(_) => {
            let job = load_job(directory)?;
            start(directory, &job)?;
            status(directory, &job)
        }
        Operation::Status(_) => status(directory, &load_job(directory)?),
        Operation::AllocationStatus(_) => allocation_status(directory, &load_job(directory)?),
        Operation::RequestCancel(request) => {
            let outcome = request_cancel(directory, &load_job(directory)?, request.scope)?;
            serde_json::to_value(CancellationResponse {
                cancellation: outcome,
            })
            .map_err(|_| failure("cannot encode cancellation outcome"))
        }
    }
}

fn load_job(directory: &Path) -> Result<ComputeJob> {
    let job: ComputeJob = local_process::read_record(&directory.join("remote.json"))?;
    if job.version != 1 || directory != Path::new(&remote_directory(&job.execution, job.intent)) {
        return Err(failure("remote directory mismatch"));
    }
    Ok(job)
}

fn validate(directory: &Path, validation: ValidationRequest) -> Result<Value> {
    let descriptor = validation.descriptor;
    let _installation_fences = crate::cleanup::pin_installation(&descriptor)?;
    validation
        .driver
        .validate_environment(&descriptor.environment)
        .map_err(failure)?;
    descriptor
        .validate_driver(validation.driver.kind())
        .map_err(failure)?;
    crate::control::check_release(&validation.installation_roots, &descriptor)?;
    crate::drivers::resolve(&validation.driver).validate()?;
    let executable = Path::new(&descriptor.release_path).join(&descriptor.entrypoint[0]);
    let mut digests = std::collections::BTreeMap::from([(
        executable.to_string_lossy().into_owned(),
        crate::artifacts::file_digest(&executable)?.1,
    )]);
    if let Some(script) = &descriptor.slurm_script {
        let script = Path::new(&descriptor.release_path).join(script);
        digests.insert(
            script.to_string_lossy().into_owned(),
            crate::artifacts::file_digest(&script)?.1,
        );
    }
    local_process::create_private_directory(directory)?;
    Ok(json!({"status":"ready","file_digests":digests,"execution_uid":execution_uid()?}))
}

fn prepare(directory: &Path, job: ComputeJob) -> Result<Value> {
    if job.version != 1 || directory != Path::new(&remote_directory(&job.execution, job.intent)) {
        return Err(failure("remote job path conflicts with launch identity"));
    }
    local_process::create_private_directory(
        directory
            .parent()
            .ok_or_else(|| failure("invalid remote work root"))?,
    )?;
    local_process::create_private_directory(directory)?;
    let _lock = submission_lock(directory)?;
    refuse_retired_payload(directory)?;
    if directory.join("remote.json").exists() {
        let existing: ComputeJob = local_process::read_record(&directory.join("remote.json"))?;
        if serde_json::to_value(&existing).ok() != serde_json::to_value(&job).ok() {
            return Err(failure(
                "preparation conflicts with existing launch identity",
            ));
        }
        if let Some(evidence) = crate::launch::read_failure(directory, job.intent, &job.assignment)?
        {
            if !evidence.stage.before_submission() {
                return Err(failure("preparation cannot mutate submitted work"));
            }
            return completion_response(
                directory,
                pre_submission_completion(directory, &job, &evidence)?,
            );
        }
        if directory.join("launch-intent.json").exists()
            || directory.join("start-attempt.json").exists()
        {
            return Err(failure("preparation cannot mutate submitted work"));
        }
    } else {
        local_process::write_record(directory, "remote.json", &job)?;
    }
    let preparation = (|| -> Result<()> {
        if let enumeray_contracts::execution::DriverConfig::Process {
            scratch: Some(policy),
        } = &job.execution.driver
        {
            policy.validate().map_err(failure)?;
            enumeray_contracts::execution::validate_process_environment(
                &job.execution.installation.environment,
            )
            .map_err(failure)?;
        }
        local_process::validate_environment(&job.execution.installation, &job.assignment)?;
        job.log_policy.validate().map_err(failure)?;
        if !directory.join("log-policy.json").exists() {
            local_process::write_record(directory, "log-policy.json", &job.log_policy)?;
        }
        local_process::create_private_directory(&directory.join("inputs"))?;
        if !directory.join("job.json").exists() {
            local_process::prepare_worker(
                directory,
                job.intent,
                &job.assignment,
                job.execution.installation.clone(),
                match &job.execution.driver {
                    enumeray_contracts::execution::DriverConfig::Process { scratch } => {
                        scratch.clone()
                    }
                    _ => None,
                },
            )?;
        }
        Ok(())
    })();
    if let Err(error) = preparation {
        let evidence = crate::launch::StartupFailure::new(
            job.intent,
            &job.assignment,
            crate::launch::FailureStage::LaunchPreparation,
            &error.message,
            None,
        )?;
        crate::launch::retain_failure(directory, &evidence, &job.assignment)?;
        return completion_response(directory, evidence.completion(&job.assignment)?);
    }
    Ok(json!({"status":"prepared"}))
}

fn input_open(directory: &Path) -> Result<()> {
    refuse_retired_payload(directory)?;
    if [
        "launch-intent.json",
        "start-attempt.json",
        "startup-failure.json",
    ]
    .iter()
    .any(|name| directory.join(name).exists())
    {
        return Err(failure("input staging is closed after launch intent"));
    }
    Ok(())
}

fn transfer_probe(
    directory: &Path,
    job: &ComputeJob,
    request: crate::protocol::TransferProbe,
) -> Result<Value> {
    use crate::protocol::{TransferProof, TransferTarget};
    use sha2::{Digest, Sha256};
    use std::io::{Seek, SeekFrom};
    let _lock = submission_lock(directory)?;
    refuse_retired_payload(directory)?;
    if request.length > enumeray_client::bulk::RANGE_BYTES {
        return Err(failure("transfer proof exceeds its range bound"));
    }
    let (path, size, checksum, input) = match request.target {
        TransferTarget::Input { artifact_id } => {
            input_open(directory)?;
            let (size, checksum) = job
                .input_digests
                .get(&artifact_id)
                .cloned()
                .map(Ok)
                .unwrap_or_else(|| crate::collections::input_digest(directory, artifact_id))?;
            (
                directory.join("inputs").join(artifact_id.to_string()),
                size,
                checksum,
                true,
            )
        }
        TransferTarget::Output { path } => {
            let done = local_process::recover_worker(directory, job.intent, &job.assignment)?
                .ok_or_else(|| failure("output not finalized"))?;
            let file = crate::collections::output_file(directory, &done, &path)?;
            (
                crate::artifacts::confined_file(&directory.join("workspace"), &path)?,
                file.size_bytes,
                file.sha256.clone(),
                false,
            )
        }
        TransferTarget::Archive => {
            let built: crate::log_archive::Built =
                local_process::read_record(&directory.join("log-archives/archive.json"))?;
            (
                crate::artifacts::confined_file(directory, "log-archives/logs.tar.gz")?,
                built.file.size_bytes,
                built.file.sha256,
                false,
            )
        }
    };
    let mut digest = Sha256::new();
    let retained = if path
        .try_exists()
        .map_err(|_| failure("cannot inspect transfer source"))?
    {
        if input {
            journal::check_private_path(&path, false)?;
        }
        let mut file =
            fs::File::open(&path).map_err(|_| failure("cannot inspect transfer prefix"))?;
        let length = file
            .metadata()
            .map_err(|_| failure("cannot inspect transfer length"))?
            .len();
        if length > size
            || !input && length != size
            || request
                .offset
                .checked_add(request.length)
                .is_none_or(|end| end > length)
        {
            return Err(failure("transfer source size changed"));
        }
        file.seek(SeekFrom::Start(request.offset))
            .map_err(|_| failure("cannot seek transfer prefix"))?;
        let mut remaining = request.length;
        let mut buffer = [0; 65536];
        while remaining > 0 {
            let count = file
                .read(&mut buffer[..remaining.min(65536) as usize])
                .map_err(|_| failure("cannot read transfer prefix"))?;
            if count == 0 {
                return Err(failure("transfer prefix truncated"));
            }
            digest.update(&buffer[..count]);
            remaining -= count as u64;
        }
        if input {
            file.sync_all()
                .map_err(|_| failure("cannot sync retained input"))?;
        }
        length
    } else if input && request.offset == 0 && request.length == 0 {
        0
    } else {
        return Err(failure("transfer source missing"));
    };
    Ok(
        json!({"transfer":TransferProof { size_bytes: size, checksum_sha256: checksum, retained_bytes: retained,
        offset: request.offset, length: request.length, range_sha256: STANDARD.encode(digest.finalize()) }}),
    )
}

fn write_input(directory: &Path, job: &ComputeJob, chunk: InputChunk) -> Result<Value> {
    use std::io::{Seek, SeekFrom};
    let _lock = submission_lock(directory)?;
    input_open(directory)?;
    let id = chunk.artifact_id;
    let (size, _) = job
        .input_digests
        .get(&id)
        .cloned()
        .map(Ok)
        .unwrap_or_else(|| crate::collections::input_digest(directory, id))?;
    let offset = chunk.offset;
    let bytes = STANDARD
        .decode(&chunk.data)
        .map_err(|_| failure("invalid chunk encoding"))?;
    if bytes.len() > 256 * 1024 || offset.saturating_add(bytes.len() as u64) > size {
        return Err(failure("input chunk exceeds declared size"));
    }
    let path = directory.join("inputs").join(id.to_string());
    if path.exists() {
        journal::check_private_path(&path, false)?;
    }
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|_| failure("cannot open staged input"))?;
    let retained = file
        .metadata()
        .map_err(|_| failure("cannot inspect staged input"))?
        .len();
    if retained < offset {
        return Err(failure("input chunk has a gap"));
    }
    if offset < retained {
        if offset + bytes.len() as u64 > retained {
            return Err(failure("input replay overlaps an unconfirmed tail"));
        }
        let mut existing = vec![0; bytes.len()];
        file.seek(SeekFrom::Start(offset))
            .and_then(|_| file.read_exact(&mut existing))
            .map_err(|_| failure("cannot reconcile input replay"))?;
        if existing != bytes {
            return Err(failure("input replay conflicts with retained bytes"));
        }
        file.sync_all()
            .map_err(|_| failure("cannot persist reconciled input"))?;
        return Ok(json!({"status":"staged"}));
    }
    file.seek(SeekFrom::Start(offset))
        .and_then(|_| file.write_all(&bytes))
        .and_then(|_| file.sync_all())
        .map_err(|_| failure("cannot persist input chunk"))?;
    Ok(json!({"status":"staged"}))
}

fn read_output(directory: &Path, job: &ComputeJob, request: OutputRequest) -> Result<Value> {
    use std::io::{Seek, SeekFrom};
    refuse_retired_payload(directory)?;
    let path = request.path.as_str();
    let done = local_process::recover_worker(directory, job.intent, &job.assignment)?
        .ok_or_else(|| failure("output not completed"))?;
    crate::collections::output_file(directory, &done, path)?;
    let path = crate::artifacts::confined_file(&directory.join("workspace"), path)?;
    let offset = request.offset;
    let mut file = fs::File::open(path).map_err(|_| failure("cannot open output"))?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|_| failure("cannot seek output"))?;
    let mut bytes = Vec::new();
    file.take(256 * 1024)
        .read_to_end(&mut bytes)
        .map_err(|_| failure("cannot read output"))?;
    Ok(json!({"data":STANDARD.encode(bytes)}))
}

fn start(directory: &Path, job: &ComputeJob) -> Result<()> {
    let _lock = submission_lock(directory)?;
    refuse_retired_payload(directory)?;
    if directory.join("launch-intent.json").exists()
        || directory.join("start-attempt.json").exists()
        || directory.join("startup-failure.json").exists()
    {
        return Err(failure("launch already requested; start cannot repeat"));
    }
    if local_process::user_cancellation(directory, job.intent)? {
        if !directory.join("completion.json").exists() {
            local_process::write_record(
                directory,
                "completion.json",
                &local_process::cancelled_completion(&job.assignment)?,
            )?;
        }
        return Ok(());
    }
    if directory.join("completion.json").exists() {
        return Err(failure("completed execution cannot start again"));
    }
    local_process::write_record(
        directory,
        "start-attempt.json",
        &json!({"intent":job.intent}),
    )?;
    let driver = crate::drivers::resolve(&job.execution.driver);
    let preparation = (|| -> Result<()> {
        local_process::validate_installation(&job.execution.installation, &job.assignment)?;
        for (id, (size, checksum)) in &job.input_digests {
            crate::artifacts::verify_file(
                &directory.join("inputs").join(id.to_string()),
                *size,
                checksum,
            )?;
        }
        crate::collections::verify_inputs(directory, &job.assignment)?;
        driver.prepare_launch(directory, job)
    })();
    let outcome = match preparation {
        Ok(()) => driver.submit(directory, job)?,
        Err(error) => crate::launch::LaunchOutcome::ProvenFailure {
            evidence: Box::new(crate::launch::StartupFailure::new(
                job.intent,
                &job.assignment,
                crate::launch::FailureStage::LaunchPreparation,
                &error.message,
                None,
            )?),
        },
    };
    local_process::write_record(directory, "launch-outcome.json", &outcome)?;
    if let crate::launch::LaunchOutcome::ProvenFailure { evidence } = outcome {
        crate::launch::retain_failure(directory, &evidence, &job.assignment)?;
    }
    Ok(())
}

fn request_cancel(
    directory: &Path,
    job: &ComputeJob,
    scope: CancellationScope,
) -> Result<CancellationOutcome> {
    let _lock = submission_lock(directory)?;
    if let Some(evidence) = crate::launch::read_failure(directory, job.intent, &job.assignment)?
        && evidence.stage.before_submission()
    {
        pre_submission_completion(directory, job, &evidence)?;
        return Ok(CancellationOutcome::AlreadyTerminal);
    }
    let stopped_worker = match local_process::inspect_worker(directory, job.intent, &job.assignment)
    {
        Ok(local_process::WorkerRecovery::Completed(_)) => {
            return Ok(CancellationOutcome::AlreadyTerminal);
        }
        Ok(local_process::WorkerRecovery::Stopped { .. }) => true,
        Ok(_) => false,
        Err(_) => {
            return Ok(CancellationOutcome::Unresolved {
                reason: "wrapper evidence is invalid; preserve the original execution".into(),
            });
        }
    };
    let driver = crate::drivers::resolve(&job.execution.driver);
    let capabilities = driver.capabilities();
    if !match scope {
        CancellationScope::Pending => capabilities.cancel_pending,
        CancellationScope::Running => capabilities.cancel_running,
    } {
        return Ok(CancellationOutcome::Unsupported);
    }
    if scope == CancellationScope::Running {
        if !local_process::user_cancellation(directory, job.intent)? {
            local_process::write_record(
                directory,
                "user-cancellation.json",
                &json!({"intent":job.intent,"scope":"running"}),
            )?;
        }
        if !directory.join("launch-intent.json").exists() {
            if !directory.join("completion.json").exists() {
                local_process::write_record(
                    directory,
                    "completion.json",
                    &local_process::cancelled_completion(&job.assignment)?,
                )?;
            }
            return Ok(CancellationOutcome::AlreadyTerminal);
        }
    }
    if stopped_worker
        && (scope != CancellationScope::Running
            || !matches!(
                job.execution.driver,
                enumeray_contracts::execution::DriverConfig::Slurm(_)
            ))
    {
        return Ok(CancellationOutcome::Unresolved {
            reason: "wrapper completion evidence is unresolved; preserve the original execution"
                .into(),
        });
    }
    let handle = match driver.reconcile(directory, job) {
        Ok(Some(handle)) => handle,
        Ok(None) | Err(_) => {
            return Ok(CancellationOutcome::Unresolved {
                reason: "persisted execution identity unavailable; submission will not repeat"
                    .into(),
            });
        }
    };
    driver.request_cancel(directory, job, &handle, scope)
}

fn allocation_status(directory: &Path, job: &ComputeJob) -> Result<Value> {
    let _lock = submission_lock(directory)?;
    allocation_status_locked(directory, job)
}
fn allocation_status_locked(directory: &Path, job: &ComputeJob) -> Result<Value> {
    let completed = local_process::read_completion(directory, job.intent, &job.assignment)?
        .ok_or_else(|| failure("allocation release requires retained completion"))?;
    if completed
        .startup_evidence
        .as_ref()
        .is_some_and(|e| e.stage.before_submission())
    {
        return Ok(json!({"status":"completed"}));
    }
    let released = match job.execution.driver {
        enumeray_contracts::execution::DriverConfig::Process { .. } => {
            let path = directory.join("worker.lock");
            if !path.exists() {
                completed.startup_evidence.is_some()
            } else {
                journal::check_private_path(&path, false)?;
                let owner = fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(path)
                    .map_err(|_| failure("cannot inspect worker ownership"))?;
                match owner.try_lock() {
                    Ok(()) => true,
                    Err(fs::TryLockError::WouldBlock) => false,
                    Err(_) => return Err(failure("cannot inspect worker ownership")),
                }
            }
        }
        enumeray_contracts::execution::DriverConfig::Slurm(_) => {
            let driver = crate::drivers::resolve(&job.execution.driver);
            if let Some(handle) = driver.reconcile(directory, job)? {
                matches!(
                    driver.observe(directory, job, &handle)?,
                    Observation::Stopped { .. }
                )
            } else {
                completed
                    .startup_evidence
                    .as_ref()
                    .is_some_and(|e| e.stage.before_submission())
            }
        }
    };
    Ok(json!({"status":if released {"completed"} else {"running"}}))
}

fn refuse_retired_payload(directory: &Path) -> Result<()> {
    if directory
        .join("cleanup.json")
        .try_exists()
        .map_err(|_| failure("cannot inspect cleanup replay fence"))?
    {
        return Err(failure(
            "run payload retirement forbids preparation, staging and execution replay",
        ));
    }
    Ok(())
}
fn cleanup_run(
    directory: &Path,
    job: &ComputeJob,
    request: crate::cleanup::Request,
) -> Result<Value> {
    request.validate(&job.assignment, job.log_policy.archive_enabled)?;
    let _payload = crate::cleanup::payload_fence(directory, false)?;
    if !request.retained_paths(directory)?.is_empty() {
        return Err(failure(
            "run payload contains a retained installation reference; preserve its workspace",
        ));
    }
    let _submission = submission_lock(directory)?;
    let path = directory.join("cleanup.json");
    let mut state: crate::cleanup::RemoteState = if path.exists() {
        let state: crate::cleanup::RemoteState = local_process::read_record(&path)?;
        if state.version != 1
            || state.launch_intent_id != job.intent
            || !state.allocation_released
            || !state.request.same_delivery(&request)?
        {
            return Err(failure(
                "run cleanup conflicts with retained retirement authority",
            ));
        }
        state
    } else {
        if allocation_status_locked(directory, job)?["status"] != "completed" {
            return Err(failure("cleanup requires released allocation"));
        }
        crate::cleanup::RemoteState {
            version: 1,
            launch_intent_id: job.intent,
            request,
            allocation_released: true,
            complete: false,
            removed_entries: 0,
            removed_bytes: 0,
        }
    };
    // The current catalogue was checked under the payload fence above and any
    // overlapping installation references have their own durable release pins.
    // Retain delivery authority and replay guards without copying an unrelated
    // connector-wide catalogue into every run's permanent checkpoint. This also
    // compacts checkpoints written by older runners when cleanup is retried.
    state.request.protection = None;
    let _worker = crate::cleanup::worker_fence(directory)?;
    if let Some(manifest) = &state
        .request
        .archive_status
        .as_ref()
        .and_then(|status| status.manifest.as_ref())
        && manifest.launch_intent_id != job.intent
    {
        return Err(failure(
            "archive delivery evidence conflicts with cleanup launch identity",
        ));
    }
    crate::cleanup::replace(directory, "cleanup.json", &state)?;
    if !state.complete {
        let mut budget = crate::cleanup::Budget::new();
        state.complete = crate::cleanup::payloads(directory, &mut budget)?;
        state.removed_entries = state.removed_entries.saturating_add(budget.removed_entries);
        state.removed_bytes = state.removed_bytes.saturating_add(budget.removed_bytes);
        crate::cleanup::replace(directory, "cleanup.json", &state)?;
    }
    Ok(
        json!({"status":if state.complete {"completed"}else{"pending"},"removed_entries":state.removed_entries,"removed_bytes":state.removed_bytes}),
    )
}

fn status(directory: &Path, job: &ComputeJob) -> Result<Value> {
    let _status_lock = submission_lock(directory)?;
    let startup = crate::launch::read_failure(directory, job.intent, &job.assignment)?;
    if let Some(evidence) = &startup
        && evidence.stage.before_submission()
    {
        return completion_response(
            directory,
            pre_submission_completion(directory, job, evidence)?,
        );
    }
    match local_process::inspect_worker(directory, job.intent, &job.assignment)? {
        local_process::WorkerRecovery::Completed(completion) => {
            return completion_response(directory, *completion);
        }
        local_process::WorkerRecovery::Stopped { _owner }
            if !matches!(
                job.execution.driver,
                enumeray_contracts::execution::DriverConfig::Slurm(_)
            ) =>
        {
            if let Some(evidence) = &startup {
                if local_process::wrapper_started(directory, job.intent)? {
                    return Err(failure(
                        "startup failure conflicts with wrapper execution evidence",
                    ));
                }
                let completion = evidence.completion(&job.assignment)?;
                local_process::write_record(directory, "completion.json", &completion)?;
                return completion_response(directory, completion);
            }
            return Err(failure(
                "local supervisor stopped without confirmed completion; preserve launch evidence",
            ));
        }
        _ => (),
    }
    let driver = crate::drivers::resolve(&job.execution.driver);
    let observation = match driver.reconcile(directory, job)? {
        Some(handle) => driver.observe(directory, job, &handle)?,
        None => Observation::Unresolved {
            reason: "submission acknowledgment absent".into(),
        },
    };
    // The driver only reads evidence. Retain confirmed completion at the common
    // runner boundary so restart/replay returns the original observation time.
    let observation = if let Observation::Stopped { mut evidence } = observation {
        // Retained wrapper results take precedence, including a completion
        // published while the scheduler commands were in flight.
        let owner = local_process::inspect_stopped_worker(directory, job.intent, &job.assignment)?;
        // Worker failure evidence may also arrive during those commands. Read
        // it again under the ownership fence before selecting the final report.
        let startup = match &owner {
            local_process::WorkerRecovery::Absent { .. }
            | local_process::WorkerRecovery::Stopped { .. } => {
                crate::launch::read_failure(directory, job.intent, &job.assignment)?
            }
            _ => startup,
        };
        match owner {
            local_process::WorkerRecovery::Completed(completion) => {
                return completion_response(directory, *completion);
            }
            local_process::WorkerRecovery::Live => Observation::Unresolved {
                reason: "terminal scheduler accounting conflicts with a live worker".into(),
            },
            _ if evidence.allocation.state == "COMPLETED" && startup.is_none() => {
                Observation::Unresolved {
                    reason: "scheduler completion has no verified wrapper result".into(),
                }
            }
            _ => {
                // Keep ownership acquired by inspect_worker until immutable
                // publication finishes. Invalid cancellation records also block.
                evidence.worker_started = directory
                    .join("started")
                    .try_exists()
                    .map_err(|_| failure("cannot inspect worker start evidence"))?;
                evidence.cancellation_scope =
                    if local_process::user_cancellation(directory, job.intent)? {
                        Some(CancellationScope::Running)
                    } else if local_process::pending_cancellation(directory, job.intent)? {
                        Some(CancellationScope::Pending)
                    } else {
                        None
                    };
                let completion = if let Some(startup) = &startup {
                    if local_process::wrapper_started(directory, job.intent)? {
                        return Err(failure(
                            "startup failure conflicts with wrapper execution evidence",
                        ));
                    }
                    let mut completion = startup.completion(&job.assignment)?;
                    evidence.validate_startup_report(job.intent, &completion.report)?;
                    completion.scheduler_evidence = Some(*evidence);
                    completion
                } else {
                    (*evidence).completion(&job.assignment)?
                };
                local_process::write_record(directory, "completion.json", &completion)?;
                let response = completion_response(directory, completion);
                drop(owner);
                return response;
            }
        }
    } else {
        observation
    };
    let mut observation = serde_json::to_value(observation)
        .map_err(|_| failure("cannot encode driver observation"))?;
    if directory.join("workspace").is_dir() && observation["status"] != "completed" {
        observation["started"] = json!(local_process::wrapper_started(directory, job.intent)?);
    }
    Ok(observation)
}

fn pre_submission_completion(
    directory: &Path,
    job: &ComputeJob,
    evidence: &crate::launch::StartupFailure,
) -> Result<local_process::Completion> {
    if directory.join("started").exists()
        || directory.join("worker-attempt.json").exists()
        || directory.join("wrapper-started.json").exists()
        || directory.join("slurm-receipt.json").exists()
        || directory.join("slurm.json").exists()
    {
        return Err(failure(
            "pre-submission failure conflicts with backend execution evidence",
        ));
    }
    match local_process::read_completion(directory, job.intent, &job.assignment)? {
        Some(completion) => Ok(completion),
        None => {
            let completion = evidence.completion(&job.assignment)?;
            local_process::write_record(directory, "completion.json", &completion)?;
            Ok(completion)
        }
    }
}

// Serialize cancellation-before-start with submission; ownership survives process death.
fn submission_lock(directory: &Path) -> Result<fs::File> {
    let path = directory.join("submission.lock");
    if path.exists() {
        journal::check_private_path(&path, false)?;
    }
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(path)
        .map_err(|_| failure("cannot open submission ownership"))?;
    file.lock()
        .map_err(|_| failure("cannot acquire submission ownership"))?;
    Ok(file)
}
