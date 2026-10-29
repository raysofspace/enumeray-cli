//! Trusted local installations behind the same durable assignment boundary.
//! A separate supervisor retains the child and its evidence if the connector dies.

#[path = "local_process/scratch.rs"]
mod scratch;
#[path = "local_process/supervisor.rs"]
mod supervisor;

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
};

use enumeray_contracts::{assignments::results, execution::InstallationIdentity, models};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Result, failure, journal, numeric, records::TerminalReport};

pub(crate) const MAX_RECORD_BYTES: u64 = 4 * 1024 * 1024;
pub(super) const MAX_RESULT_BYTES: u64 = 64 * 1024;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Installation {
    pub(crate) provider_binding_id: String,
    pub(crate) installation: InstallationIdentity,
    pub(crate) argv: Vec<String>,
    pub(crate) environment: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) file_digests: BTreeMap<String, String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Job {
    version: u32,
    launch_intent_id: Uuid,
    assignment: Value,
    installation: Installation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scratch: Option<enumeray_contracts::execution::ProcessScratchPolicy>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Completion {
    pub observed_at: String,
    pub report: TerminalReport,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub files: BTreeMap<String, crate::artifacts::OutputFile>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) collections: BTreeMap<String, crate::collections::Measured>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) scheduler_evidence: Option<crate::drivers::slurm::SchedulerEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) startup_evidence: Option<crate::launch::StartupFailure>,
}

impl Completion {
    pub(crate) fn validate(&self, intent: Uuid, assignment: &Value) -> Result<()> {
        let bytes =
            serde_json::to_vec(&self.report).map_err(|_| failure("invalid local completion"))?;
        results::parse_request(&bytes).map_err(|_| failure("invalid local completion report"))?;
        crate::collections::validate_measured(
            assignment,
            &self.report,
            self.files.len(),
            &self.collections,
        )?;
        if self.report["attempt_id"] != assignment["attempt_id"] {
            return Err(failure(
                "local completion conflicts with the assigned attempt",
            ));
        }
        if let Some(startup) = &self.startup_evidence {
            startup.validate_report(intent, assignment, &self.report)?;
            if self.observed_at != startup.observed_at
                || !self.files.is_empty()
                || (!startup.stage.before_submission()
                    && assignment["requirements"]["driver"] == "slurm"
                    && self.scheduler_evidence.is_none())
            {
                return Err(failure("startup report lacks stopped scheduler evidence"));
            }
        }
        if let Some(evidence) = &self.scheduler_evidence {
            if self.startup_evidence.is_some() {
                evidence.validate_startup_report(intent, &self.report)?;
            } else {
                evidence.validate_report(intent, &self.report)?;
            }
            if assignment["requirements"]["driver"] != "slurm"
                || !self.files.is_empty()
                || self
                    .startup_evidence
                    .as_ref()
                    .is_some_and(|s| s.stage.before_submission())
            {
                return Err(failure(
                    "scheduler evidence conflicts with worker completion",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerMarker {
    intent: Uuid,
}

pub(crate) fn wrapper_started(directory: &Path, intent: Uuid) -> Result<bool> {
    // Legacy workers only have `started`; absence of newer evidence cannot
    // retrospectively prove that their scientific execution never happened.
    if directory
        .join("worker-attempt.json")
        .try_exists()
        .map_err(|_| failure("cannot inspect worker attempt"))?
    {
        let attempt: WorkerMarker = read_record(&directory.join("worker-attempt.json"))?;
        if attempt.intent != intent {
            return Err(failure("worker attempt conflicts with launch identity"));
        }
        let path = directory.join("wrapper-started.json");
        if path
            .try_exists()
            .map_err(|_| failure("cannot inspect wrapper start"))?
        {
            let marker: WorkerMarker = read_record(&path)?;
            if marker.intent != intent {
                return Err(failure("wrapper start conflicts with launch identity"));
            }
            return Ok(true);
        }
        return Ok(false);
    }
    if directory.join("wrapper-started.json").exists() {
        return Err(failure("wrapper start has no worker attempt evidence"));
    }
    let started = directory
        .join("started")
        .try_exists()
        .map_err(|_| failure("cannot inspect worker start"))?;
    if started {
        journal::check_private_path(&directory.join("started"), false)?;
    }
    Ok(started)
}

fn retain_worker_failure(
    directory: &Path,
    job: &Job,
    stage: crate::launch::FailureStage,
    message: &str,
) -> Result<()> {
    let evidence = crate::launch::StartupFailure::new(
        job.launch_intent_id,
        &job.assignment,
        stage,
        message,
        None,
    )?;
    crate::launch::retain_failure(directory, &evidence, &job.assignment)?;
    // A Slurm worker must exit unsuccessfully; the common runner waits for
    // stopped allocation/step evidence before constructing its terminal report.
    if job.assignment["requirements"]["driver"] == "slurm" {
        return Err(failure(
            "worker startup failed; retained proof awaits scheduler termination",
        ));
    }
    Ok(())
}

/// A held ownership file fences completion publication against worker startup.
pub(crate) enum WorkerRecovery {
    Completed(Box<Completion>),
    Absent { _owner: Option<File> },
    Live,
    Stopped { _owner: File },
}

pub(crate) fn validate_environment(installation: &Installation, assignment: &Value) -> Result<()> {
    let descriptor_environment: BTreeMap<String, String> = assignment["descriptor"]
        .get("environment")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| failure("invalid pinned installation environment"))?
        .unwrap_or_default();
    enumeray_contracts::execution::validate_installation_environment(&descriptor_environment)
        .map_err(failure)?;
    enumeray_contracts::execution::validate_installation_environment(&installation.environment)
        .map_err(failure)
}

fn validate_scratch(job: &Job) -> Result<()> {
    if let Some(policy) = &job.scratch {
        policy.validate().map_err(failure)?;
        if job.assignment["requirements"]["driver"] != "process" {
            return Err(failure("process scratch policy on a non-process job"));
        }
        let environment = job.assignment["descriptor"]
            .get("environment")
            .cloned()
            .map(serde_json::from_value::<BTreeMap<String, String>>)
            .transpose()
            .map_err(|_| failure("invalid pinned installation environment"))?
            .unwrap_or_default();
        enumeray_contracts::execution::validate_process_environment(&environment)
            .map_err(failure)?;
        enumeray_contracts::execution::validate_process_environment(&job.installation.environment)
            .map_err(failure)?;
    }
    Ok(())
}

pub(crate) fn validate_installation(installation: &Installation, assignment: &Value) -> Result<()> {
    validate_environment(installation, assignment)?;
    let _: models::ScientificDefinition = serde_json::from_value(assignment["definition"].clone())
        .map_err(|_| failure("invalid pinned publication"))?;
    let expected: InstallationIdentity =
        serde_json::from_value(assignment["descriptor"]["installation"].clone())
            .map_err(|_| failure("invalid pinned installation"))?;
    if installation.installation != expected
        || assignment["definition"]["contract_version"] != 1
        || expected.wrapper_protocol_version != 1
        || assignment["definition"]["wrapper_protocol_version"] != expected.wrapper_protocol_version
        || !matches!(
            assignment["requirements"]["driver"].as_str(),
            Some("process" | "slurm")
        )
        || installation.argv.is_empty()
        || installation.argv.len() > 32
        || installation
            .argv
            .iter()
            .any(|argument| argument.is_empty() || argument.len() > 4096 || argument.contains('\0'))
        || !Path::new(&installation.argv[0]).is_absolute()
    {
        return Err(failure(
            "local installation does not match the approved protocol, resources, or command",
        ));
    }
    for path in installation
        .argv
        .iter()
        .take(2)
        .filter(|path| Path::new(path).is_absolute())
    {
        if !fs::metadata(path).is_ok_and(|metadata| metadata.is_file()) {
            return Err(failure("local executable or wrapper is not installed"));
        }
    }
    for (path, expected) in &installation.file_digests {
        if crate::artifacts::file_digest(Path::new(path))?.1 != *expected {
            return Err(failure("installed file content changed after validation"));
        }
    }
    let request_bytes = serde_json::to_vec(&request_value(assignment, Uuid::nil()))
        .map_err(|_| failure("cannot encode local wrapper request"))?;
    models::parse_inputs(&request_bytes)
        .map_err(|_| failure("local wrapper request exceeds the supported envelope limits"))?;
    Ok(())
}

pub(super) fn job_directory(directory: &Path, intent: Uuid) -> PathBuf {
    directory.join("local-jobs").join(intent.to_string())
}

#[cfg(test)]
pub fn recover(directory: &Path, intent: Uuid, assignment: &Value) -> Result<Option<Completion>> {
    let directory = job_directory(directory, intent);
    if !directory
        .try_exists()
        .map_err(|_| failure("cannot inspect local launch evidence"))?
    {
        return Ok(None);
    }
    recover_worker(&directory, intent, assignment)
}

/// Even an incomplete record contradicts a claim that no launch could occur.
#[cfg(test)]
pub fn has_launch_evidence(directory: &Path, intent: Uuid, _assignment: &Value) -> Result<bool> {
    job_directory(directory, intent)
        .try_exists()
        .map_err(|_| failure("cannot inspect local launch evidence"))
}

pub(crate) fn recover_worker(
    directory: &Path,
    intent: Uuid,
    assignment: &Value,
) -> Result<Option<Completion>> {
    match inspect_worker(directory, intent, assignment)? {
        WorkerRecovery::Completed(completion) => Ok(Some(*completion)),
        WorkerRecovery::Absent { .. } | WorkerRecovery::Live => Ok(None),
        WorkerRecovery::Stopped { .. } => Err(failure(
            "local supervisor stopped without confirmed completion; preserve launch evidence",
        )),
    }
}

pub(crate) fn inspect_worker(
    directory: &Path,
    intent: Uuid,
    assignment: &Value,
) -> Result<WorkerRecovery> {
    inspect_worker_ownership(directory, intent, assignment, false)
}

/// Only after terminal scheduler accounting may observation fence an idle
/// worker lock. Earlier acquisition can race the worker's initial try_lock.
pub(crate) fn inspect_stopped_worker(
    directory: &Path,
    intent: Uuid,
    assignment: &Value,
) -> Result<WorkerRecovery> {
    inspect_worker_ownership(directory, intent, assignment, true)
}

fn inspect_worker_ownership(
    directory: &Path,
    intent: Uuid,
    assignment: &Value,
    fence_idle: bool,
) -> Result<WorkerRecovery> {
    journal::check_private_path(directory, true)?;
    let job: Job = read_record(&directory.join("job.json"))?;
    if job.version != 1
        || job.launch_intent_id != intent
        || !numeric::equivalent(&job.assignment, assignment)
    {
        return Err(failure(
            "local launch record conflicts with the pinned assignment",
        ));
    }
    if let Some(completion) = read_completion(directory, intent, assignment)? {
        return Ok(WorkerRecovery::Completed(Box::new(completion)));
    }
    let started = directory
        .join("started")
        .try_exists()
        .map_err(|_| failure("cannot inspect local supervisor evidence"))?;
    if started {
        journal::check_private_path(&directory.join("started"), false)?;
    }
    let owner_path = directory.join("worker.lock");
    if !owner_path
        .try_exists()
        .map_err(|_| failure("cannot inspect local supervisor ownership"))?
    {
        if started {
            return Err(failure("started worker has no ownership evidence"));
        }
        return Ok(WorkerRecovery::Absent { _owner: None });
    }
    journal::check_private_path(&owner_path, false)?;
    if !started && !fence_idle {
        return Ok(WorkerRecovery::Absent { _owner: None });
    }
    let owner = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&owner_path)
        .map_err(|_| failure("cannot inspect local supervisor ownership"))?;
    match owner.try_lock() {
        Ok(()) => (),
        Err(fs::TryLockError::WouldBlock) => return Ok(WorkerRecovery::Live),
        Err(_) => return Err(failure("cannot inspect local supervisor ownership lock")),
    }
    // The worker publishes before releasing ownership. Reread under that lock.
    if let Some(completion) = read_completion(directory, intent, assignment)? {
        return Ok(WorkerRecovery::Completed(Box::new(completion)));
    }
    Ok(if started {
        WorkerRecovery::Stopped { _owner: owner }
    } else {
        WorkerRecovery::Absent {
            _owner: Some(owner),
        }
    })
}

pub(crate) fn read_completion(
    directory: &Path,
    intent: Uuid,
    assignment: &Value,
) -> Result<Option<Completion>> {
    let path = directory.join("completion.json");
    if !path
        .try_exists()
        .map_err(|_| failure("cannot inspect local completion"))?
    {
        return Ok(None);
    }
    let completion: Completion = read_record(&path)?;
    completion.validate(intent, assignment)?;
    if completion.startup_evidence.is_some() && wrapper_started(directory, intent)? {
        return Err(failure(
            "startup completion conflicts with wrapper execution evidence",
        ));
    }
    if let Some(retained) = crate::launch::read_failure(directory, intent, assignment)? {
        retained.validate_report(intent, assignment, &completion.report)?;
        if completion.startup_evidence.as_ref().is_none_or(|startup| {
            serde_json::to_value(startup).ok() != serde_json::to_value(&retained).ok()
        }) {
            return Err(failure(
                "startup failure conflicts with retained wrapper completion",
            ));
        }
    }
    Ok(Some(completion))
}

/// Hidden supervisor entry point. A permanent start marker forbids its replay.
pub fn worker(directory: &Path) -> Result<()> {
    journal::check_private_path(directory, true)?;
    let job: Job = read_record(&directory.join("job.json"))?;
    if job.version != 1 || job.launch_intent_id.is_nil() {
        return Err(failure("invalid local job snapshot"));
    }
    // Decode trusted identity before reporting failures; malformed snapshots
    // never become fabricated terminal results.
    let _ = crate::launch::StartupFailure::new(
        job.launch_intent_id,
        &job.assignment,
        crate::launch::FailureStage::WorkerValidation,
        "Worker identity verified.",
        None,
    )?;
    let owner_path = directory.join("worker.lock");
    if owner_path
        .try_exists()
        .map_err(|_| failure("cannot inspect local supervisor ownership"))?
    {
        journal::check_private_path(&owner_path, false)?;
    }
    let lock = private_options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(owner_path)
        .map_err(|_| failure("cannot open local supervisor ownership"))?;
    lock.try_lock()
        .map_err(|_| failure("local supervisor already owns this launch"))?;
    if directory.join("worker-attempt.json").exists()
        || directory.join("startup-failure.json").exists()
        || directory.join("completion.json").exists()
        || crate::launch::read_failure(directory, job.launch_intent_id, &job.assignment)?.is_some()
    {
        return Err(failure("retained execution evidence forbids worker replay"));
    }
    if directory
        .join("started")
        .try_exists()
        .map_err(|_| failure("cannot inspect worker replay fence"))?
    {
        return Err(failure(
            "local supervisor already started; execution will not repeat",
        ));
    }
    write_record(
        directory,
        "worker-attempt.json",
        &json!({"intent":job.launch_intent_id}),
    )?;
    let started = private_options()
        .write(true)
        .create_new(true)
        .open(directory.join("started"))
        .map_err(|_| failure("local supervisor already started; execution will not repeat"))?;
    started
        .sync_all()
        .map_err(|_| failure("cannot persist local start evidence"))?;
    sync_directory(directory)?;
    let stage = crate::launch::FailureStage::WorkerValidation;
    if let Err(error) = validate_scratch(&job) {
        return retain_worker_failure(directory, &job, stage, &error.message);
    }
    if let Err(error) = validate_installation(&job.installation, &job.assignment) {
        return retain_worker_failure(directory, &job, stage, &error.message);
    }
    let workspace = directory.join("workspace");
    let preparation = (|| -> Result<()> {
        create_private_directory(&workspace)?;
        crate::artifacts::copy_inputs(
            &directory.join("inputs"),
            &workspace.join("inputs"),
            &job.assignment,
        )?;
        if let Some(policy) = &job.scratch {
            create_private_directory(&workspace.join(".enumeray"))?;
            create_private_directory(&workspace.join(".enumeray/tmp"))?;
            if policy.cache == enumeray_contracts::execution::ProcessCache::PerRun {
                create_private_directory(&workspace.join(".enumeray/cache"))?;
            }
        }
        let mut request = request(&job);
        let collections =
            crate::collections::prepare_workspace(directory, &workspace, &job.assignment)?;
        if collections.as_object().is_some_and(|m| !m.is_empty()) {
            request["input_collections"] = collections;
        }
        write_record(&workspace, "request.json", &request)
    })();
    if let Err(error) = preparation {
        return retain_worker_failure(
            directory,
            &job,
            crate::launch::FailureStage::WorkerPreparation,
            &error.message,
        );
    }
    let completion = match supervisor::execute(&workspace, &job) {
        Ok(completion) => completion,
        Err(error) => {
            if let Some(stage) = error.startup_stage {
                return retain_worker_failure(directory, &job, stage, &error.message);
            }
            return Err(error);
        }
    };
    write_record(directory, "completion.json", &completion)
}

fn request(job: &Job) -> Value {
    request_value(&job.assignment, job.launch_intent_id)
}

fn request_value(assignment: &Value, intent: Uuid) -> Value {
    let version = assignment["definition"]["wrapper_protocol_version"]
        .as_u64()
        .unwrap_or(1);
    let mut request = json!({"wrapper_protocol_version":version, "launch_intent_id":intent});
    request["input_files"] =
        crate::artifacts::request_files(assignment).unwrap_or_else(|_| json!({}));
    for field in [
        "run_id",
        "assignment_id",
        "attempt_id",
        "model_id",
        "publication_id",
        "inputs",
    ] {
        request[field] = assignment[field].clone();
    }
    request
}

pub(crate) fn read_record<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    serde_json::from_slice(&read_private(path, MAX_RECORD_BYTES)?).map_err(|_| {
        failure("local execution evidence is malformed; preserve it for reconciliation")
    })
}

pub(crate) fn read_private(path: &Path, limit: u64) -> Result<Vec<u8>> {
    journal::check_private_path(path, false)?;
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(limit + 1).read_to_end(&mut bytes))
        .map_err(|_| failure("cannot read private local execution data"))?;
    if bytes.len() as u64 > limit {
        return Err(failure("local execution data exceeds its byte limit"));
    }
    Ok(bytes)
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

pub(crate) fn create_private_directory(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(path) {
        Ok(()) => sync_directory(
            path.parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new(".")),
        )?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(_) => return Err(failure("cannot create private local execution directory")),
    }
    journal::check_private_path(path, true)
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|_| failure("cannot sync local execution directory"))
}

/// Atomic publication without replacing prior evidence.
pub(crate) fn write_record(path: &Path, name: &str, value: &impl Serialize) -> Result<()> {
    let bytes =
        serde_json::to_vec(value).map_err(|_| failure("cannot encode local execution evidence"))?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(failure("local execution evidence exceeds its limit"));
    }
    let temporary = path.join(format!(".{}.tmp", Uuid::new_v4()));
    let mut file = private_options()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|_| failure("cannot create local execution evidence"))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| failure("cannot persist local execution evidence"))?;
    fs::hard_link(&temporary, path.join(name))
        .map_err(|_| failure("local execution evidence already exists or cannot be published"))?;
    fs::remove_file(&temporary).map_err(|_| failure("cannot finish local execution evidence"))?;
    sync_directory(path)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn new_relative_directory_syncs_the_current_directory() {
        let path = PathBuf::from(format!("enumeray-relative-directory-{}", Uuid::new_v4()));
        create_private_directory(&path).unwrap();
        journal::check_private_path(&path, true).unwrap();
        fs::remove_dir(&path).unwrap();
    }

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let directory =
                std::env::temp_dir().join(format!("enumeray-local-driver-{}", Uuid::new_v4()));
            create_private_directory(&directory).unwrap();
            Self(directory)
        }

        fn job(&self, code: Option<&str>) -> Job {
            let model: Value = serde_json::to_value(
                enumeray_contracts::models::parse_publication(include_bytes!(
                    "../../../../examples/local-scalar-model.json"
                ))
                .unwrap(),
            )
            .unwrap();
            let binding: Value = serde_json::from_str(include_str!(
                "../../../../examples/local-scalar-installation.json"
            ))
            .unwrap();
            let deployment: Value = serde_json::from_str(include_str!(
                "../../../../examples/local-scalar-requirements.json"
            ))
            .unwrap();
            let assignment = json!({"assignment_id":Uuid::new_v4(), "attempt_id":Uuid::new_v4(),
                "run_id":Uuid::new_v4(), "model_id":"scalar-double", "publication_id":"local-v1",
                "definition":model["definition"], "descriptor":binding,
                "requirements":deployment,
                "inputs":serde_json::from_str::<Value>(r#"{"value":9007199254740993.1}"#).unwrap()});
            let wrapper = if let Some(code) = code {
                let path = self.0.join("custom.py");
                fs::write(&path, code).unwrap();
                path
            } else {
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/scalar_model/wrapper.py")
            };
            let installation = Installation {
                provider_binding_id: binding["provider_binding_id"].as_str().unwrap().into(),
                installation: serde_json::from_value(binding["installation"].clone()).unwrap(),
                argv: vec![
                    "/usr/bin/python3".into(),
                    wrapper.to_str().unwrap().into(),
                    "execute".into(),
                ],
                environment: BTreeMap::new(),
                file_digests: BTreeMap::new(),
            };
            Job {
                version: 1,
                launch_intent_id: Uuid::new_v4(),
                assignment,
                installation,
                scratch: None,
            }
        }

        fn run(&self, job: &Job) -> Completion {
            create_private_directory(&self.0.join("local-jobs")).unwrap();
            let directory = job_directory(&self.0, job.launch_intent_id);
            create_private_directory(&directory).unwrap();
            write_record(&directory, "job.json", job).unwrap();
            worker(&directory).unwrap();
            recover(&self.0, job.launch_intent_id, &job.assignment)
                .unwrap()
                .unwrap()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn scratch_policy() -> enumeray_contracts::execution::ProcessScratchPolicy {
        serde_json::from_value(json!({"max_bytes":8 * 1024 * 1024,"max_entries":2000,"max_depth":32,"scan_interval_ms":1000,"cache":"per_run","symlinks":"workspace_only"})).unwrap()
    }
    const SUCCESS: &str = "r=json.loads(Path('request.json').read_text())\nfor key in ['inputs','model_id','publication_id','input_files']: r.pop(key,None)\nr.update(outcome='succeeded',outputs={'doubled':4},failure=None,files={})\nPath('result.json').write_text(json.dumps(r))\n";
    #[test]
    fn named_diagnostics_do_not_change_success_or_failure() {
        for mode in ["success", "failure"] {
            let scratch = Scratch::new();
            let ending = if mode == "failure" {
                "raise SystemExit(2)".to_owned()
            } else {
                SUCCESS.to_owned()
            };
            let code = format!(
                "import json,time\nfrom pathlib import Path\nPath('logs').mkdir()\nPath('logs/solver.log').write_bytes(b'converged\\x00\\xff')\nPath('logs/nonfile.log').mkdir()\n{ending}\n"
            );
            let mut job = scratch.job(Some(&code));
            job.assignment["definition"]["log_files"] = json!({"solver":{"path":"logs/solver.log"},"missing":{"path":"logs/absent.log"},"nonfile":{"path":"logs/nonfile.log"}});
            let done = scratch.run(&job);
            assert_eq!(
                done.report["outcome"],
                if mode == "success" {
                    "succeeded"
                } else {
                    "failed"
                }
            );
            let root = job_directory(&scratch.0, job.launch_intent_id);
            for name in ["solver", "missing", "nonfile"] {
                let m = crate::capture::manifest(&crate::capture::source_directory(
                    &root,
                    enumeray_contracts::logs::LogSource::model(name).unwrap(),
                ))
                .unwrap()
                .unwrap();
                assert!(m.closed);
                assert_eq!(m.unavailable, name != "solver");
            }
        }
    }

    #[test]
    fn configured_scratch_supports_large_workloads_and_managed_cache_modes() {
        let inherited = Command::new("/usr/bin/python3")
            .args([
                "-c",
                "import resource; print(resource.getrlimit(resource.RLIMIT_FSIZE)[0])",
            ])
            .output()
            .unwrap();
        assert!(inherited.status.success());
        let inherited = String::from_utf8(inherited.stdout).unwrap();
        let inherited = inherited.trim();
        for cache in [
            enumeray_contracts::execution::ProcessCache::PerRun,
            enumeray_contracts::execution::ProcessCache::None,
        ] {
            let scratch = Scratch::new();
            let code = format!(
                "import json,os,resource\nfrom pathlib import Path\nassert resource.getrlimit(resource.RLIMIT_FSIZE)[0] == {inherited}\nassert Path(os.environ['TMPDIR']).resolve() == Path('.enumeray/tmp').resolve()\nassert os.environ['TMP'] == os.environ['TEMP'] == os.environ['TMPDIR']\nassert ('XDG_CACHE_HOME' in os.environ) == {}\nPath('large').write_bytes(b'x' * (4 * 1024 * 1024))\nfor i in range(600): Path(str(i)).touch()\nPath('deep/' + '/'.join(['d'] * 12)).mkdir(parents=True)\nPath('alias').symlink_to('large')\n{SUCCESS}",
                if cache == enumeray_contracts::execution::ProcessCache::PerRun {
                    "True"
                } else {
                    "False"
                }
            );
            let mut job = scratch.job(Some(&code));
            let mut policy = scratch_policy();
            policy.cache = cache;
            job.scratch = Some(policy);
            let completion = scratch.run(&job);
            assert_eq!(
                completion.report["outcome"], "succeeded",
                "{}",
                completion.report["failure"]
            );
            assert_eq!(completion.report["outputs"]["doubled"], 4);
        }
    }
    #[test]
    fn configured_scratch_failure_names_limits_even_after_a_short_run() {
        for (code, expected) in [
            (
                "Path('large').write_bytes(b'x' * (9 * 1024 * 1024))",
                "scratch_bytes",
            ),
            (
                "for i in range(2100): Path(str(i)).touch()",
                "scratch_entries",
            ),
            (
                "Path('deep/' + '/'.join(['d'] * 33)).mkdir(parents=True)",
                "scratch_depth",
            ),
            ("Path('escape').symlink_to('/etc')", "scratch_symlink"),
        ] {
            let scratch = Scratch::new();
            let mut job = scratch.job(Some(&format!(
                "import json,time\nfrom pathlib import Path\ntime.sleep(0.05)\n{code}\n{SUCCESS}"
            )));
            let mut policy = scratch_policy();
            policy.scan_interval_ms = 60_000;
            job.scratch = Some(policy);
            let completion = scratch.run(&job);
            assert_eq!(completion.report["failure"]["code"], expected);
            assert_eq!(completion.report["execution_terminated"], true);
            assert!(completion.files.is_empty());
        }
    }
    #[test]
    fn scratch_policy_and_managed_environment_are_rechecked_before_launch() {
        for key in ["TMPDIR", "TMP", "TEMP", "XDG_CACHE_HOME"] {
            let scratch = Scratch::new();
            let mut job = scratch.job(None);
            job.scratch = Some(scratch_policy());
            job.installation
                .environment
                .insert(key.into(), "/outside".into());
            let completion = scratch.run(&job);
            assert_eq!(completion.report["outcome"], "failed");
            assert!(
                !job_directory(&scratch.0, job.launch_intent_id)
                    .join("wrapper-started.json")
                    .exists()
            );
        }
        let scratch = Scratch::new();
        let mut job = scratch.job(None);
        assert!(
            !serde_json::to_value(&job)
                .unwrap()
                .as_object()
                .unwrap()
                .contains_key("scratch")
        );
        let mut policy = scratch_policy();
        policy.max_bytes = 0;
        job.scratch = Some(policy);
        assert!(validate_scratch(&job).is_err());
    }

    #[test]
    fn validation_and_preparation_failures_are_immutable_without_wrapper_capture() {
        for validation in [true, false] {
            let scratch = Scratch::new();
            let mut job = scratch.job(Some("raise RuntimeError('must not execute')\n"));
            if validation {
                job.installation.argv[0] = scratch.0.join("missing").to_string_lossy().into();
            }
            create_private_directory(&scratch.0.join("local-jobs")).unwrap();
            let directory = job_directory(&scratch.0, job.launch_intent_id);
            create_private_directory(&directory).unwrap();
            write_record(&directory, "job.json", &job).unwrap();
            if !validation {
                fs::write(directory.join("workspace"), b"not a directory").unwrap();
            }
            worker(&directory).unwrap();
            let completion = recover_worker(&directory, job.launch_intent_id, &job.assignment)
                .unwrap()
                .unwrap();
            assert_eq!(
                completion.report["failure"]["code"],
                if validation {
                    "worker_validation_failed"
                } else {
                    "worker_preparation_failed"
                }
            );
            assert!(!wrapper_started(&directory, job.launch_intent_id).unwrap());
            assert!(!directory.join("wrapper-started.json").exists());
            assert!(!directory.join("workspace/stdout.log").exists());
            let original = fs::read(directory.join("startup-failure.json")).unwrap();
            assert!(worker(&directory).is_err());
            assert_eq!(
                fs::read(directory.join("startup-failure.json")).unwrap(),
                original
            );
            let replay = recover_worker(&directory, job.launch_intent_id, &job.assignment)
                .unwrap()
                .unwrap();
            assert_eq!(replay.observed_at, completion.observed_at);
            assert!(replay.report == *completion.report);
        }
    }

    #[test]
    fn worker_environment_checks_both_maps_without_repeating_execution() {
        for descriptor_map in [true, false] {
            let scratch = Scratch::new();
            let mut job = scratch.job(Some("raise RuntimeError('must not execute')\n"));
            if descriptor_map {
                job.assignment["descriptor"]["environment"] = json!({"HOME":"untrusted-value"});
            } else {
                job.installation
                    .environment
                    .insert("HOME".into(), "untrusted-value".into());
            }
            let error = validate_installation(&job.installation, &job.assignment).unwrap_err();
            assert!(error.message.contains("HOME"));
            assert!(!error.message.contains("untrusted-value"));
            let completion = scratch.run(&job);
            assert_eq!(
                completion.report["failure"]["code"],
                "worker_validation_failed"
            );
            assert!(
                completion.report["failure"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("HOME")
            );
            let directory = job_directory(&scratch.0, job.launch_intent_id);
            assert!(!directory.join("wrapper-started.json").exists());
            assert!(!directory.join("workspace").exists());
            assert!(worker(&directory).is_err());
            let recovered = recover_worker(&directory, job.launch_intent_id, &job.assignment)
                .unwrap()
                .unwrap();
            assert!(recovered.report == *completion.report);
        }
    }

    #[test]
    fn pinned_environment_defaults_empty_and_rejects_invalid_map_shapes() {
        let scratch = Scratch::new();
        let mut job = scratch.job(None);
        job.assignment["descriptor"]
            .as_object_mut()
            .unwrap()
            .remove("environment");
        validate_installation(&job.installation, &job.assignment).unwrap();
        for invalid in [Value::Null, json!([]), json!({"MODEL_SETTING":42})] {
            job.assignment["descriptor"]["environment"] = invalid;
            let error = validate_installation(&job.installation, &job.assignment).unwrap_err();
            assert_eq!(error.message, "invalid pinned installation environment");
        }
    }

    #[test]
    fn supervisor_rechecks_environment_immediately_before_spawn() {
        for descriptor_map in [true, false] {
            let scratch = Scratch::new();
            let mut job = scratch.job(Some("raise RuntimeError('must not execute')\n"));
            if descriptor_map {
                job.assignment["descriptor"]["environment"] =
                    json!({"PMIX_NAMESPACE":"untrusted-value"});
            } else {
                job.installation
                    .environment
                    .insert("PMIX_NAMESPACE".into(), "untrusted-value".into());
            }
            create_private_directory(&scratch.0.join("local-jobs")).unwrap();
            let directory = job_directory(&scratch.0, job.launch_intent_id);
            create_private_directory(&directory).unwrap();
            let workspace = directory.join("workspace");
            create_private_directory(&workspace).unwrap();
            let error = supervisor::execute(&workspace, &job).err().unwrap();
            assert!(error.message.contains("PMIX_NAMESPACE"));
            assert_eq!(
                error.startup_stage,
                Some(crate::launch::FailureStage::WorkerValidation)
            );
            assert!(!directory.join("wrapper-started.json").exists());
            assert!(!workspace.join(".wrapper-exit").exists());
        }
    }

    #[test]
    fn missing_worker_program_proves_no_detached_worker_was_spawned() {
        let scratch = Scratch::new();
        assert!(spawn_worker_program(&scratch.0, &scratch.0.join("unavailable-worker")).is_err());
        assert!(!scratch.0.join("started").exists());
    }

    #[test]
    fn external_wrapper_preserves_exact_decimal_and_worker_cannot_repeat() {
        let scratch = Scratch::new();
        let job = scratch.job(None);
        let completion = scratch.run(&job);
        assert_eq!(completion.report["outcome"], "succeeded");
        assert!(numeric::equivalent(
            &completion.report["outputs"]["doubled"],
            &serde_json::from_str("18014398509481986.2").unwrap()
        ));
        assert!(worker(&job_directory(&scratch.0, job.launch_intent_id)).is_err());
        let replayed = recover(&scratch.0, job.launch_intent_id, &job.assignment)
            .unwrap()
            .unwrap();
        assert!(numeric::equivalent(&replayed.report, &completion.report));
        let mut changed = job.assignment.clone();
        changed["inputs"] = json!({"value":1});
        assert!(recover(&scratch.0, job.launch_intent_id, &changed).is_err());
    }

    #[test]
    fn single_executable_entrypoint_runs_without_a_fixed_subcommand() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new();
        let code = "#!/usr/bin/python3\nimport json\nfrom pathlib import Path\nr=json.loads(Path('request.json').read_text())\nfor key in ['inputs','model_id','publication_id','input_files']: r.pop(key)\nr.update(outcome='succeeded',outputs={'doubled':4},failure=None,files={})\nPath('result.json').write_text(json.dumps(r))\n";
        let mut job = scratch.job(Some(code));
        let wrapper = scratch.0.join("custom.py");
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
        job.installation.argv = vec![wrapper.to_str().unwrap().to_owned()];
        let completion = scratch.run(&job);
        assert_eq!(
            completion.report["outcome"], "succeeded",
            "{}",
            completion.report["failure"]
        );
        assert_eq!(completion.report["outputs"]["doubled"], 4);
    }

    #[test]
    fn protocol_one_stages_declared_input_file_paths() {
        let scratch = Scratch::new();
        let code = "import json\nfrom pathlib import Path\nr=json.loads(Path('request.json').read_text())\nvalue=int(Path(r['input_files']['dataset']['path']).read_text())\nfor key in ['inputs','model_id','publication_id','input_files']: r.pop(key,None)\nr.update(outcome='succeeded',outputs={'doubled':value*2},failure=None,files={})\nPath('result.json').write_text(json.dumps(r))\n";
        let mut job = scratch.job(Some(code));
        let model: Value = serde_json::to_value(
            enumeray_contracts::models::parse_publication(include_bytes!(
                "../../../../examples/artifact-input.json"
            ))
            .unwrap(),
        )
        .unwrap();
        job.assignment["definition"]["input_schema"] = model["definition"]["input_schema"].clone();
        job.assignment["definition"]["wrapper_protocol_version"] = json!(1);
        job.assignment["descriptor"]["installation"]["wrapper_protocol_version"] = json!(1);
        job.installation.installation.wrapper_protocol_version = 1;
        let artifact = Uuid::new_v4();
        job.assignment["inputs"] = json!({"dataset":{"artifact_id":artifact}});
        create_private_directory(&scratch.0.join("local-jobs")).unwrap();
        let directory = job_directory(&scratch.0, job.launch_intent_id);
        create_private_directory(&directory).unwrap();
        create_private_directory(&directory.join("inputs")).unwrap();
        fs::write(directory.join("inputs").join(artifact.to_string()), b"42").unwrap();
        let completion = scratch.run(&job);
        assert_eq!(completion.report["outcome"], "succeeded");
        assert_eq!(completion.report["outputs"]["doubled"], 84);
    }

    #[test]
    fn protocol_one_collects_only_declared_confined_file_outputs() {
        let scratch = Scratch::new();
        let code = "import json\nfrom pathlib import Path\nr=json.loads(Path('request.json').read_text())\nfor key in ['inputs','model_id','publication_id','input_files']: r.pop(key,None)\nPath('answer.txt').write_text('abc')\nr.update(outcome='succeeded',outputs={},failure=None,files={'result':{'path':'answer.txt','media_type':'text/plain','scientific_metadata':{}}})\nPath('result.json').write_text(json.dumps(r))\n";
        let mut job = scratch.job(Some(code));
        let model: Value = serde_json::to_value(
            enumeray_contracts::models::parse_publication(include_bytes!(
                "../../../../examples/artifact-output.json"
            ))
            .unwrap(),
        )
        .unwrap();
        job.assignment["definition"]["output_schema"] =
            model["definition"]["output_schema"].clone();
        job.assignment["definition"]["wrapper_protocol_version"] = json!(1);
        job.assignment["descriptor"]["installation"]["wrapper_protocol_version"] = json!(1);
        job.installation.installation.wrapper_protocol_version = 1;
        let completion = scratch.run(&job);
        assert_eq!(completion.report["outcome"], "succeeded");
        assert_eq!(completion.files["result"].size_bytes, 3);
        assert_eq!(completion.files["result"].path, "answer.txt");
        assert_eq!(
            completion.files["result"].sha256,
            "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0="
        );
    }

    #[test]
    fn wrapper_files_reject_claimed_integrity_and_duplicate_json_outputs() {
        for conflict in [
            "f['size_bytes']=999",
            "f['sha256']='f'*64",
            "outputs['result']={'artifact_id':'00000000-0000-0000-0000-000000000001'}",
        ] {
            let scratch = Scratch::new();
            let code = format!(
                "import json\nfrom pathlib import Path\nr=json.loads(Path('request.json').read_text())\nfor key in ['inputs','model_id','publication_id','input_files']: r.pop(key,None)\nPath('answer.txt').write_text('abc')\nf={{'path':'answer.txt','media_type':'text/plain'}}\noutputs={{}}\n{conflict}\nr.update(outcome='succeeded',outputs=outputs,failure=None,files={{'result':f}})\nPath('result.json').write_text(json.dumps(r))\n"
            );
            let mut job = scratch.job(Some(&code));
            let model: Value = serde_json::to_value(
                enumeray_contracts::models::parse_publication(include_bytes!(
                    "../../../../examples/artifact-output.json"
                ))
                .unwrap(),
            )
            .unwrap();
            job.assignment["definition"]["output_schema"] =
                model["definition"]["output_schema"].clone();
            let completion = scratch.run(&job);
            assert_eq!(completion.report["outcome"], "failed", "{conflict}");
            assert!(completion.files.is_empty(), "{conflict}");
        }
    }

    #[test]
    fn missing_results_are_confirmed_failures() {
        let scratch = Scratch::new();
        let job = scratch.job(Some("pass\n"));
        let completion = scratch.run(&job);
        assert_eq!(completion.report["outcome"], "failed");
        assert_eq!(completion.report["failure"]["code"], "invalid_result");
        assert_eq!(completion.report["execution_terminated"], true);
    }

    #[test]
    fn process_wrappers_own_their_scientific_runtime() {
        let scratch = Scratch::new();
        let job = scratch.job(Some("import json,time\nfrom pathlib import Path\ntime.sleep(1.2)\nr=json.loads(Path('request.json').read_text())\nfor key in ['inputs','model_id','publication_id','input_files']: r.pop(key,None)\nr.update(outcome='succeeded',outputs={'doubled':4},failure=None,files={})\nPath('result.json').write_text(json.dumps(r))\n"));
        let completion = scratch.run(&job);
        assert_eq!(completion.report["outcome"], "succeeded");
        assert_eq!(completion.report["outputs"]["doubled"], 4);
        assert_eq!(completion.report["execution_terminated"], true);
    }

    #[test]
    fn slurm_supervision_preserves_scheduler_limits_and_allows_scientific_scratch() {
        let scratch = Scratch::new();
        let inherited = Command::new("/usr/bin/python3")
            .args([
                "-c",
                "import resource; print(resource.getrlimit(resource.RLIMIT_FSIZE)[0])",
            ])
            .output()
            .unwrap();
        assert!(inherited.status.success());
        let limit = String::from_utf8(inherited.stdout).unwrap();
        let code = format!(
            "import json,resource,time\nfrom pathlib import Path\nassert resource.getrlimit(resource.RLIMIT_FSIZE)[0] == {}\nPath('scratch.bin').write_bytes(b'x' * (2 * 1024 * 1024))\nfor i in range(300): Path(str(i)).touch()\ntime.sleep(1.2)\nr=json.loads(Path('request.json').read_text())\nfor key in ['inputs','model_id','publication_id','input_files']: r.pop(key,None)\nr.update(outcome='succeeded',outputs={{'doubled':4}},failure=None,files={{}})\nPath('result.json').write_text(json.dumps(r))\n",
            limit.trim()
        );
        let mut job = scratch.job(Some(&code));
        job.assignment["requirements"]["driver"] = json!("slurm");
        let completion = scratch.run(&job);
        assert_eq!(completion.report["outcome"], "succeeded");
        assert_eq!(completion.report["outputs"]["doubled"], 4);
        assert_eq!(completion.report["execution_terminated"], true);
    }

    #[test]
    fn verbose_capture_preserves_valid_results_and_resource_limits() {
        let prefix = "import json,sys,time\nfrom pathlib import Path\nsys.stdout.buffer.write(b'o' * (2 * 1024 * 1024 + 17))\nsys.stdout.buffer.flush()\nsys.stderr.buffer.write(b'e' * (2 * 1024 * 1024 + 31))\nsys.stderr.buffer.flush()\n";
        for (action, expected) in [
            (
                "r=json.loads(Path('request.json').read_text())\nfor key in ['inputs','model_id','publication_id','input_files']: r.pop(key,None)\nr.update(outcome='succeeded',outputs={'doubled':4},failure=None,files={})\nPath('result.json').write_text(json.dumps(r))\n",
                None,
            ),
            (
                "Path('scratch.bin').write_bytes(b'x' * (2 * 1024 * 1024))\ntime.sleep(10)\n",
                Some("workspace_limit"),
            ),
        ] {
            let scratch = Scratch::new();
            let job = scratch.job(Some(&format!("{prefix}{action}")));
            let completion = scratch.run(&job);
            if let Some(code) = expected {
                assert_eq!(completion.report["outcome"], "failed");
                assert_eq!(completion.report["failure"]["code"], code);
            } else {
                assert_eq!(completion.report["outcome"], "succeeded");
                assert_eq!(completion.report["outputs"]["doubled"], 4);
            }
            assert_eq!(completion.report["execution_terminated"], true);
            let directory = job_directory(&scratch.0, job.launch_intent_id);
            for (source, byte, produced) in [
                ("stdout", b'o', 2 * 1024 * 1024 + 17_u64),
                ("stderr", b'e', 2 * 1024 * 1024 + 31_u64),
            ] {
                let source: enumeray_contracts::logs::LogSource =
                    serde_json::from_value(json!(source)).unwrap();
                let root = crate::capture::source_directory(&directory, source);
                let manifest = crate::capture::manifest(&root).unwrap().unwrap();
                assert_eq!(manifest.end, produced);
                assert!(manifest.closed);
                assert_eq!(manifest.segments.len(), 16);
                assert!(manifest.end - manifest.earliest <= 1024 * 1024);
                let last = manifest.segments.last().unwrap();
                assert_eq!(
                    fs::read(root.join(format!("1-{}.segment", last.offset))).unwrap(),
                    vec![byte; last.bytes as usize]
                );
            }
            // Completion and its loss evidence survive a fresh recovery read.
            let replay = recover(&scratch.0, job.launch_intent_id, &job.assignment)
                .unwrap()
                .unwrap();
            assert!(numeric::equivalent(&replay.report, &completion.report));
        }
    }

    #[test]
    fn invalid_wrapper_results_and_workspace_overflow_cannot_succeed() {
        let envelope = "import json,sys\nfrom pathlib import Path\nr=json.loads(Path('request.json').read_text())\nfor key in ['inputs','model_id','publication_id','input_files']: r.pop(key)\nr.update(outcome='succeeded',outputs={'doubled':4},failure=None,files={})\n";
        for (action, exit, expected) in [
            (
                "Path('result.json').write_text('{broken')",
                0,
                "invalid_result",
            ),
            (
                "Path('result.json').write_text(' ' * 65537)",
                0,
                "invalid_result",
            ),
            (
                "r['attempt_id']='00000000-0000-0000-0000-000000000001'; Path('result.json').write_text(json.dumps(r))",
                0,
                "invalid_result",
            ),
            (
                "r['outputs']={'doubled':'wrong'}; Path('result.json').write_text(json.dumps(r))",
                0,
                "invalid_result",
            ),
            (
                "r.update(outcome='cancelled',outputs={},failure={'code':'user_cancelled','message':'forged'}); Path('result.json').write_text(json.dumps(r))",
                0,
                "invalid_result",
            ),
            (
                "r.update(outcome='failed',outputs={},failure={'code':'operator_resolved','message':'forbidden'}); Path('result.json').write_text(json.dumps(r))",
                1,
                "invalid_result",
            ),
            (
                "Path('result.json').write_text(json.dumps(r))",
                1,
                "wrapper_exit",
            ),
            (
                "[Path('excess-'+str(i)).touch() for i in range(300)]",
                0,
                "workspace_limit",
            ),
        ] {
            let scratch = Scratch::new();
            let job = scratch.job(Some(&format!("{envelope}{action}\nsys.exit({exit})\n")));
            let completion = scratch.run(&job);
            assert_eq!(completion.report["outcome"], "failed", "{action}");
            assert_eq!(completion.report["failure"]["code"], expected, "{action}");
        }
    }

    #[test]
    fn cleanup_stops_descendants_even_after_their_output_pipes_close() {
        let scratch = Scratch::new();
        let job = scratch.job(Some("import subprocess,sys\nfrom pathlib import Path\np=subprocess.Popen([sys.executable,'-c','import time; time.sleep(10)'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)\nPath('child-pid').write_text(str(p.pid))\n"));
        let completion = scratch.run(&job);
        assert_eq!(completion.report["execution_terminated"], true);
        let pid = fs::read_to_string(
            job_directory(&scratch.0, job.launch_intent_id).join("workspace/child-pid"),
        )
        .unwrap();
        let observed = Command::new("/bin/ps")
            .args(["-p", &pid, "-o", "stat="])
            .output()
            .unwrap();
        assert!(
            String::from_utf8(observed.stdout)
                .unwrap()
                .split_whitespace()
                .all(|state| state.starts_with('Z'))
        );
    }

    #[test]
    fn configuration_is_pinned_and_incomplete_evidence_never_launches() {
        let scratch = Scratch::new();
        let job = scratch.job(None);
        assert!(validate_installation(&job.installation, &job.assignment).is_ok());
        let mut changed = job.assignment.clone();
        changed["definition"]["contract_version"] = json!(2);
        assert!(validate_installation(&job.installation, &changed).is_err());
        changed = job.assignment.clone();
        let file_model: Value = serde_json::to_value(
            enumeray_contracts::models::parse_publication(include_bytes!(
                "../../../../examples/artifact-input.json"
            ))
            .unwrap(),
        )
        .unwrap();
        changed["definition"]["input_schema"] = file_model["definition"]["input_schema"].clone();
        assert!(validate_installation(&job.installation, &changed).is_ok());
        assert!(!scratch.0.join("local-jobs").exists());
        changed = job.assignment.clone();
        changed["descriptor"]["installation"]["model_identity"] = json!("different");
        assert!(validate_installation(&job.installation, &changed).is_err());
        let script = scratch.0.join("model.sbatch");
        fs::write(&script, b"#!/bin/sh\n#SBATCH --mem=128M\n").unwrap();
        let mut pinned = job.installation.clone();
        pinned.file_digests.insert(
            script.to_string_lossy().into_owned(),
            crate::artifacts::file_digest(&script).unwrap().1,
        );
        changed = job.assignment.clone();
        changed["requirements"]["driver"] = json!("slurm");
        validate_installation(&pinned, &changed).unwrap();
        fs::write(&script, b"#!/bin/sh\n#SBATCH --mem=256M\n").unwrap();
        assert!(validate_installation(&pinned, &changed).is_err());
        // Declared support in the registry cannot make this implementation
        // interpret a wire version it does not implement.
        let mut future = job.installation.clone();
        future.installation.wrapper_protocol_version = 3;
        changed = job.assignment.clone();
        changed["definition"]["wrapper_protocol_version"] = json!(3);
        changed["descriptor"]["installation"]["wrapper_protocol_version"] = json!(3);
        assert!(validate_installation(&future, &changed).is_err());
        create_private_directory(&scratch.0.join("local-jobs")).unwrap();
        let directory = job_directory(&scratch.0, job.launch_intent_id);
        create_private_directory(&directory).unwrap();
        assert!(has_launch_evidence(&scratch.0, job.launch_intent_id, &job.assignment).unwrap());
        assert!(recover(&scratch.0, job.launch_intent_id, &job.assignment).is_err());
        assert!(!directory.join("started").exists());
        write_record(&directory, "job.json", &job).unwrap();
        write_record(&directory, "started", &json!({})).unwrap();
        write_record(&directory, "worker.lock", &json!({})).unwrap();
        assert!(recover(&scratch.0, job.launch_intent_id, &job.assignment).is_err());
        assert!(worker(&directory).is_err());
        assert!(!directory.join("workspace").exists());
        changed = job.assignment.clone();
        changed["inputs"] = json!({"value":"x".repeat(65500)});
        assert!(validate_installation(&job.installation, &changed).is_err());
    }
}

pub(crate) fn spawn_worker(directory: &Path) -> Result<()> {
    spawn_worker_program(
        directory,
        &std::env::current_exe().map_err(|_| failure("cannot locate local supervisor"))?,
    )
}

fn spawn_worker_program(directory: &Path, executable: &Path) -> Result<()> {
    let mut command = Command::new(executable);
    command
        .args(["local-worker", "--directory"])
        .arg(directory)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command.spawn().map_err(|_| {
        failure("cannot spawn the local supervisor; scientific execution did not start")
    })?;
    // Reaping is independent of waiting for durable completion. Dropping this
    // connector (including SIGKILL) does not stop the supervisor or its pipes.
    thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
    Ok(())
}

pub(crate) fn prepare_worker(
    directory: &Path,
    intent: Uuid,
    assignment: &Value,
    installation: Installation,
    scratch: Option<enumeray_contracts::execution::ProcessScratchPolicy>,
) -> Result<()> {
    let job = Job {
        version: 1,
        launch_intent_id: intent,
        assignment: assignment.clone(),
        installation,
        scratch,
    };
    write_record(directory, "job.json", &job)?;
    validate_scratch(&job)?;
    validate_installation(&job.installation, assignment)
}
pub(crate) fn failure_completion(
    assignment: &Value,
    code: &str,
    message: &str,
) -> Result<Completion> {
    use time::{OffsetDateTime, format_description::well_known::Rfc3339};
    Ok(Completion {
        observed_at: OffsetDateTime::now_utc()
            .replace_nanosecond(0)
            .map_err(|_| failure("invalid completion timestamp"))?
            .format(&Rfc3339)
            .map_err(|_| failure("invalid completion timestamp"))?,
        files: BTreeMap::new(),
        collections: BTreeMap::new(),
        scheduler_evidence: None,
        startup_evidence: None,
        report: TerminalReport::decode(
            json!({"attempt_id":assignment["attempt_id"],"outcome":"failed","execution_terminated":true,"outputs":{},"files":{},"optional_output_failures":{},"failure":{"code":code,"message":message}}),
        )?,
    })
}

/// Cancellation is emitted only by the runner after proving no launch or termination.
pub(crate) fn cancelled_completion(assignment: &Value) -> Result<Completion> {
    let mut completion = failure_completion(
        assignment,
        "user_cancelled",
        "Execution stopped following user cancellation.",
    )?;
    let mut report = (*completion.report).clone();
    report["outcome"] = json!("cancelled");
    completion.report = TerminalReport::decode(report)?;
    Ok(completion)
}

pub(crate) fn user_cancellation(directory: &Path, intent: Uuid) -> Result<bool> {
    cancellation_intent(directory, intent, "user-cancellation.json", "running")
}

pub(crate) fn pending_cancellation(directory: &Path, intent: Uuid) -> Result<bool> {
    cancellation_intent(directory, intent, "pending-cancellation.json", "pending")
}

fn cancellation_intent(directory: &Path, intent: Uuid, name: &str, scope: &str) -> Result<bool> {
    let path = directory.join(name);
    if !path
        .try_exists()
        .map_err(|_| failure("cannot inspect cancellation intent"))?
    {
        return Ok(false);
    }
    let value: Value = read_record(&path)?;
    if value != json!({"intent":intent,"scope":scope}) {
        return Err(failure("cancellation intent conflicts with owned launch"));
    }
    Ok(true)
}
