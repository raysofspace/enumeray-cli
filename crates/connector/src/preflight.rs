//! Bounded host diagnostics. No model descriptors, assignments or launch actions.
mod sftp;

use crate::{Result, compute::ComputeTarget, connections, failure};
use enumeray_contracts::{
    execution::{ConnectionConfig, DriverConfig, ExecutionContextDefinition, ExecutionMode},
    setup::*,
};
use rustix::fs::{self as rfs, Access, AtFlags, Mode, OFlags};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::Path,
    process::Command,
    time::Duration,
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HostEnvironment {
    pub account: String,
    pub executable: String,
    pub journal: String,
    pub token_file: String,
    pub credentials_file: Option<String>,
    pub allow_development_execution: bool,
    #[serde(default)]
    pub service_file: Option<PlannedHostFile>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkerRequest {
    pub request_id: Uuid,
    pub probe_id: Uuid,
    pub connector_id: String,
    pub context_id: String,
    pub definition: ExecutionContextDefinition,
    pub environment: HostEnvironment,
    pub source: ReportSource,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunnerRequest {
    probe_id: Uuid,
    definition: ExecutionContextDefinition,
    connector_executable: Option<String>,
    connector_paths: Vec<String>,
    phase: ProbePhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected_probe_identity: Option<String>,
}
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProbePhase {
    Inspect,
    Cleanup,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunnerReport {
    execution_uid: String,
    checks: Vec<HostCheck>,
    probe_created: bool,
    probe_identity: Option<String>,
}

fn check(
    name: &str,
    outcome: CheckOutcome,
    required: bool,
    code: &str,
    role: ResponsibleRole,
    correction: &str,
) -> HostCheck {
    HostCheck {
        check: name.into(),
        outcome,
        required,
        code: code.into(),
        responsible_role: role,
        correction: correction.into(),
    }
}
fn observed(name: &str, success: bool, role: ResponsibleRole, correction: &str) -> HostCheck {
    check(
        name,
        if success {
            CheckOutcome::Passed
        } else {
            CheckOutcome::Failed
        },
        true,
        if success {
            "passed"
        } else {
            "prerequisite_failed"
        },
        role,
        if success { "" } else { correction },
    )
}
fn put(report: &mut HostPreflightReport, result: HostCheck) {
    if let Some(old) = report.checks.iter_mut().find(|c| c.check == result.check) {
        *old = result;
    } else {
        report.checks.push(result);
    }
}
fn prevented_by(report: &mut HostPreflightReport, prerequisite: &str) {
    for check in &mut report.checks {
        if check.outcome == CheckOutcome::Unknown {
            check.code = "dependency_unavailable".into();
            check.correction =
                format!("Resolve required check {prerequisite} before probing this prerequisite.");
        }
    }
}
fn runner_failure(error: Option<&crate::ConnectorError>) -> HostCheck {
    use connections::CommandState;
    let (outcome, code) = match error.and_then(|e| e.command.as_ref()).map(|d| &d.state) {
        Some(CommandState::NotSpawned { reason }) if reason == "not_found" => {
            (CheckOutcome::Failed, "runner_executable_missing")
        }
        Some(CommandState::NotSpawned { reason }) if reason == "permission_denied" => {
            (CheckOutcome::Failed, "runner_executable_inaccessible")
        }
        Some(CommandState::Exited {
            code: Some(127), ..
        }) => (CheckOutcome::Failed, "runner_executable_missing"),
        Some(CommandState::Exited {
            code: Some(126), ..
        }) => (CheckOutcome::Failed, "runner_executable_inaccessible"),
        Some(CommandState::TimedOut) => (CheckOutcome::Unknown, "runner_timed_out"),
        Some(CommandState::Exited { code: Some(0), .. }) => {
            (CheckOutcome::Failed, "runner_protocol_incompatible")
        }
        None if error.is_none_or(|e| {
            e.message.contains("runner response") || e.message.contains("compute-runner protocol")
        }) =>
        {
            (CheckOutcome::Failed, "runner_protocol_incompatible")
        }
        _ => (CheckOutcome::Unknown, "runner_transport_unavailable"),
    };
    check(
        "runner",
        outcome,
        true,
        code,
        ResponsibleRole::SiteOperator,
        "Install matching Enumeray binaries with preflight support and verify the configured executable, launcher and SSH transport. No model was launched.",
    )
}
fn initial(request: &WorkerRequest) -> Result<HostPreflightReport> {
    let checks = required_checks(&request.definition)
        .into_iter()
        .map(|name| {
            check(
                name,
                CheckOutcome::Unknown,
                true,
                "prerequisite_unverified",
                ResponsibleRole::ConnectorOwner,
                "Resolve the preceding prerequisite and repeat this host check.",
            )
        })
        .collect();
    let limitations = if matches!(request.definition.driver, DriverConfig::Slurm(_)) {
        vec!["No scheduler jobs were submitted. Compute-node runner/path visibility and allocation execution remain unverified; the site operator must qualify them separately.".into()]
    } else {
        vec!["Host prerequisites do not qualify scientific software, installed model releases or scientific results.".into()]
    };
    Ok(HostPreflightReport {
        manifest_version: 1,
        request_id: request.request_id,
        connector_id: request.connector_id.clone(),
        context_id: request.context_id.clone(),
        definition_hash: definition_hash(&request.definition).map_err(failure)?,
        execution_mode: mode_name(&request.definition),
        observed_at: OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(|_| failure("cannot record preflight time"))?,
        source: request.source,
        execution_uid: None,
        probe_id: Some(request.probe_id),
        checks,
        limitations,
    })
}

pub(crate) fn local(plan: &Path, consent: bool) -> Result<Value> {
    let plan: HostConfigurationPlan = crate::setup::read_json(plan)?;
    if plan.manifest_version != 1 {
        return Err(failure("unsupported host plan"));
    }
    crate::setup::validate(&plan.spec)?;
    let spec = plan.spec;
    let service_file = plan
        .files
        .into_iter()
        .find(|f| f.name.ends_with(".service") || f.name.ends_with(".plist"));
    let request = WorkerRequest {
        request_id: Uuid::new_v4(),
        probe_id: Uuid::new_v4(),
        connector_id: spec.context.connector_id,
        context_id: spec.context.id,
        definition: spec.context.definition,
        source: ReportSource::Local,
        environment: HostEnvironment {
            account: spec.connector_account.name,
            executable: spec.connector_executable,
            journal: spec.journal,
            token_file: spec.token_file,
            credentials_file: spec.credentials_file,
            allow_development_execution: consent,
            service_file,
        },
    };
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stopping = stop.clone();
    let work = request.clone();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| failure("cannot supervise local preflight"))?;
    let (report,interrupted)=runtime.block_on(async {
        let mut worker=tokio::task::spawn_blocking(move||supervise_cancellable(&work,&stopping));
        tokio::select! {
            biased;
            signal=tokio::signal::ctrl_c()=>{
                signal.map_err(|_|failure("cannot observe preflight interruption"))?;
                stop.store(true,std::sync::atomic::Ordering::Release);
                let report=worker.await.map_err(|_|failure("preflight worker stopped"))??;
                Ok::<_,crate::ConnectorError>((report,true))
            },
            result=&mut worker=>Ok((result.map_err(|_|failure("preflight worker stopped"))??,false)),
        }
    })?;
    runtime.shutdown_background();
    Ok(json!({"ready":report.ready() && !interrupted,"report":report,"interrupted":interrupted}))
}

/// A hard parent deadline also bounds blocked filesystem calls in diagnostic code.
pub(crate) fn supervise_cancellable(
    request: &WorkerRequest,
    stop: &std::sync::atomic::AtomicBool,
) -> Result<HostPreflightReport> {
    let mut command = Command::new(
        std::env::current_exe().map_err(|_| failure("cannot locate preflight worker"))?,
    );
    command.arg("preflight-worker");
    let bytes = serde_json::to_vec(request).map_err(|_| failure("cannot encode preflight work"))?;
    let output = connections::run_cancellable(
        &mut command,
        Some(&bytes),
        Duration::from_secs(ATTEMPT_SECONDS),
        stop,
    )?;
    if output.successful() && output.complete() && output.stdout().len() <= MAX_REPORT_BYTES {
        let report: HostPreflightReport = serde_json::from_slice(output.stdout())
            .map_err(|_| failure("invalid preflight worker report"))?;
        report.validate(&request.definition).map_err(failure)?;
        if report.request_id != request.request_id
            || report.connector_id != request.connector_id
            || report.context_id != request.context_id
            || report.source != request.source
            || report.probe_id != Some(request.probe_id)
        {
            return Err(failure("preflight worker report identity mismatch"));
        }
        return Ok(report);
    }
    let mut report = initial(request)?;
    report.limitations.push("The bounded host-check worker did not complete. Inspect connectivity or host storage and repeat the diagnostic; no model execution was requested.".into());
    report.limitations.push("Probe cleanup is unconfirmed. Inspect only .enumeray-preflight-PROBE_ID under the approved work root, using this report's probe_id; preserve unexpected files.".into());
    for item in &mut report.checks {
        item.code = "worker_unavailable_or_timed_out".into();
    }
    Ok(report)
}

pub(crate) fn worker() -> Result<Value> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| failure("cannot read preflight worker request"))?;
    if bytes.len() > 1024 * 1024 {
        return Err(failure("preflight worker request exceeds limit"));
    }
    let request: WorkerRequest =
        serde_json::from_slice(&bytes).map_err(|_| failure("invalid preflight worker request"))?;
    serde_json::to_value(run(&request)?).map_err(|_| failure("cannot encode host report"))
}

fn run(request: &WorkerRequest) -> Result<HostPreflightReport> {
    let started = std::time::Instant::now();
    let remaining = || Duration::from_secs(ATTEMPT_SECONDS).saturating_sub(started.elapsed());
    let mut report = initial(request)?;
    let definition = &request.definition;
    let valid = !request.request_id.is_nil()
        && definition.validate_execution().is_ok()
        && (1..=1024).contains(&definition.max_outstanding_assignments);
    put(
        &mut report,
        observed(
            "configuration",
            valid,
            ResponsibleRole::ConnectorOwner,
            "Review the registered execution context and supported field limits.",
        ),
    );
    if !valid {
        prevented_by(&mut report, "configuration");
        return Ok(report);
    }
    let uid = crate::compute::execution_uid()?;
    let identity =
        uid != "0" && account_name().is_ok_and(|name| name == request.environment.account);
    put(
        &mut report,
        observed(
            "connector_identity",
            identity,
            ResponsibleRole::HostAdministrator,
            "Run this check as the intended non-root connector account.",
        ),
    );
    if !identity {
        prevented_by(&mut report, "connector_identity");
        return Ok(report);
    }
    let paths = connector_paths(&request.environment);
    let private = paths
        .iter()
        .all(|path| private_path_or_parent(Path::new(path), uid.parse().unwrap_or(0)))
        && (matches!(definition.connection, ConnectionConfig::Ssh(_))
            || Path::new(&request.environment.journal) != Path::new(&definition.work_root));
    put(
        &mut report,
        observed(
            "connector_paths",
            private,
            ResponsibleRole::HostAdministrator,
            "Prepare existing private connector state/credential paths owned by the connector account, separate from the execution work root.",
        ),
    );
    if crate::execution_policy::require_consent(
        definition.execution_mode,
        request.environment.allow_development_execution,
    )
    .is_err()
    {
        put(
            &mut report,
            check(
                "execution_identity",
                CheckOutcome::Failed,
                true,
                "development_consent_required",
                ResponsibleRole::ConnectorOwner,
                "Repeat with explicit --allow-development-execution only for trusted same-user models.",
            ),
        );
        prevented_by(&mut report, "execution_identity");
        return Ok(report);
    }
    let identity_file = match &definition.connection {
        ConnectionConfig::Direct { .. } => None,
        ConnectionConfig::Ssh(target) => {
            let credentials = request.environment.credentials_file.as_ref().and_then(|p| {
                let bytes = crate::local_process::read_private(
                    Path::new(p),
                    crate::local_process::MAX_RECORD_BYTES,
                )
                .ok()?;
                serde_json::from_slice::<crate::control::HostCredentials>(&bytes).ok()
            });
            let key = credentials.and_then(|c| c.ssh.get(&target.credential_reference).cloned());
            let available = key
                .as_ref()
                .is_some_and(|p| crate::journal::check_private_path(p, false).is_ok());
            put(
                &mut report,
                observed(
                    "ssh_credentials",
                    available,
                    ResponsibleRole::ConnectorOwner,
                    "Configure the named SSH reference in a private credential map and keep its private key accessible only to the connector account.",
                ),
            );
            if !available {
                prevented_by(&mut report, "ssh_credentials");
                return Ok(report);
            }
            key
        }
    };
    let target = ComputeTarget {
        execution_mode: definition.execution_mode,
        connection: definition.connection.clone(),
        work_root: definition.work_root.clone(),
        driver: definition.driver.clone(),
        identity_file,
    };
    let probe_id = request.probe_id;
    let direct = matches!(definition.connection, ConnectionConfig::Direct { .. });
    let mut runner_request = RunnerRequest {
        probe_id,
        definition: definition.clone(),
        connector_executable: direct.then(|| request.environment.executable.clone()),
        connector_paths: if direct { paths } else { vec![] },
        phase: ProbePhase::Inspect,
        expected_probe_identity: None,
    };
    let input =
        serde_json::to_vec(&runner_request).map_err(|_| failure("cannot encode runner probe"))?;
    let reply = connections::invoke_preflight(&target, &input, remaining());
    if !direct {
        put(
            &mut report,
            observed(
                "ssh",
                reply.is_ok()
                    || reply
                        .as_ref()
                        .err()
                        .and_then(|e| e.command.as_ref())
                        .is_some_and(|d| {
                            matches!(
                                d.state,
                                connections::CommandState::Exited {
                                    code: Some(0),
                                    signal: None
                                }
                            )
                        }),
                ResponsibleRole::SiteOperator,
                "Verify the approved SSH account, private key, trusted host key and remote runner path; keep batch authentication and strict host-key checking enabled.",
            ),
        );
    }
    let runner = reply
        .as_ref()
        .ok()
        .and_then(|v| serde_json::from_value::<RunnerReport>(v["preflight"].clone()).ok());
    let Some(runner) = runner else {
        put(&mut report, runner_failure(reply.as_ref().err()));
        prevented_by(&mut report, "runner");
        report.limitations.push("Probe creation and cleanup could not be confirmed. Inspect only .enumeray-preflight-PROBE_ID under the approved work root using this report's probe_id; preserve unexpected files.".into());
        return Ok(report);
    };
    put(
        &mut report,
        observed("runner", true, ResponsibleRole::SiteOperator, ""),
    );
    report.execution_uid = Some(runner.execution_uid.clone());
    let separated = if direct {
        runner.execution_uid != "0"
            && crate::execution_policy::check_identity(
                definition.execution_mode,
                &uid,
                &runner.execution_uid,
            )
            .is_ok()
    } else {
        runner.execution_uid != "0"
    };
    put(
        &mut report,
        observed(
            "execution_identity",
            separated,
            ResponsibleRole::HostAdministrator,
            "Configure the approved non-root execution identity; managed direct execution must use a different account, and development must use the same account.",
        ),
    );
    for item in runner.checks {
        put(&mut report, item);
    }
    if definition.execution_mode == ExecutionMode::Development {
        for name in ["executable_isolation", "credential_isolation"] {
            put(
                &mut report,
                observed(name, true, ResponsibleRole::ConnectorOwner, ""),
            );
        }
        report
            .limitations
            .push(crate::execution_policy::WARNING.into());
    }
    if runner.probe_created {
        if !direct {
            put(
                &mut report,
                observed(
                    "sftp",
                    find_program("sftp") && sftp_probe(&target, probe_id, remaining()).is_ok(),
                    ResponsibleRole::SiteOperator,
                    "Enable SFTP for the approved SSH execution account and verify access to the private work-root probe.",
                ),
            );
        }
        if let Some(mapping) = &definition.shared_workspace {
            let path = Path::new(&mapping.connector_root)
                .join(probe_name(probe_id))
                .join("sentinel");
            let visible = read_sentinel(&path, probe_id);
            put(
                &mut report,
                observed(
                    "shared_workspace",
                    visible,
                    ResponsibleRole::SiteOperator,
                    "Correct the approved connector/runner shared-workspace mapping. Compute-node visibility requires separate site qualification.",
                ),
            );
        }
        runner_request.phase = ProbePhase::Cleanup;
        runner_request.expected_probe_identity = runner.probe_identity;
        let input = serde_json::to_vec(&runner_request)
            .map_err(|_| failure("cannot encode probe cleanup"))?;
        let cleaned = connections::invoke_preflight(&target, &input, remaining())
            .is_ok_and(|v| v["cleaned"] == true);
        put(
            &mut report,
            observed(
                "probe_cleanup",
                cleaned,
                ResponsibleRole::SiteOperator,
                "Remove only the identified private preflight probe after inspecting its retained identity; do not remove model workspaces.",
            ),
        );
    }
    if let Some(file) = &request.environment.service_file {
        let installed = fs::File::open(&file.destination).and_then(|f| {
            let mut content = String::new();
            f.take(1024 * 1024 + 1).read_to_string(&mut content)?;
            if content.len() > 1024 * 1024 {
                return Err(std::io::Error::other("service configuration exceeds bound"));
            }
            Ok(content)
        });
        let outcome = match &installed {
            Ok(content) if content == &file.content => CheckOutcome::Passed,
            Ok(_) => CheckOutcome::Failed,
            Err(_) => CheckOutcome::Unknown,
        };
        put(
            &mut report,
            check(
                "service_configuration",
                outcome,
                false,
                if outcome == CheckOutcome::Passed {
                    "service_configuration_matches"
                } else {
                    "service_configuration_missing_or_changed"
                },
                ResponsibleRole::HostAdministrator,
                "Review the generated native service file and its installed destination. Foreground startup remains available; preflight does not install or replace services.",
            ),
        );
    } else {
        put(
            &mut report,
            check(
                "service_configuration",
                CheckOutcome::Unknown,
                false,
                "service_configuration_unavailable",
                ResponsibleRole::HostAdministrator,
                "No reviewed service file was supplied to this check. Inspect the applicable native service definition separately; foreground startup is supported.",
            ),
        );
    }
    put(
        &mut report,
        service_state(
            &request.connector_id,
            definition.execution_mode,
            remaining(),
        ),
    );
    report.validate(definition).map_err(failure)?;
    Ok(report)
}

fn connector_paths(environment: &HostEnvironment) -> Vec<String> {
    let mut paths = vec![environment.journal.clone(), environment.token_file.clone()];
    if let Some(path) = &environment.credentials_file {
        paths.push(path.clone());
    }
    paths
}
fn private_path_or_parent(path: &Path, uid: u32) -> bool {
    match fs::symlink_metadata(path) {
        Ok(meta) => !meta.file_type().is_symlink() && meta.uid() == uid && meta.mode() & 0o077 == 0,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            path.parent().is_some_and(|p| {
                fs::symlink_metadata(p).is_ok_and(|m| {
                    m.is_dir()
                        && !m.file_type().is_symlink()
                        && m.uid() == uid
                        && m.mode() & 0o077 == 0
                })
            })
        }
        _ => false,
    }
}
fn account_name() -> Result<String> {
    let output = connections::run_command(
        Command::new("/usr/bin/id").arg("-un"),
        None,
        Duration::from_secs(5),
    )?;
    if !output.successful() || !output.complete() {
        return Err(failure("cannot inspect host account"));
    }
    String::from_utf8(output.stdout().to_vec())
        .map(|s| s.trim().to_owned())
        .map_err(|_| failure("invalid account identity"))
}
pub(crate) fn current_account() -> Result<String> {
    account_name()
}

fn accessible(path: &Path, access: Access) -> bool {
    rfs::accessat(rfs::CWD, path, access, AtFlags::EACCESS).is_ok()
}
fn modifiable_ancestor(path: &Path) -> bool {
    path.ancestors()
        .any(|part| accessible(part, Access::WRITE_OK))
        || fs::canonicalize(path).is_ok_and(|resolved| {
            resolved
                .ancestors()
                .any(|part| accessible(part, Access::WRITE_OK))
        })
}
fn probe_name(id: Uuid) -> String {
    format!(".enumeray-preflight-{id}")
}
fn sentinel(id: Uuid) -> Vec<u8> {
    format!("enumeray-preflight-v1:{id}\n").into_bytes()
}
fn root_fd(path: &str) -> Result<std::os::fd::OwnedFd> {
    rfs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| failure("cannot open approved work root"))
}
fn create_probe(root: &str, id: Uuid) -> Result<String> {
    let fd = root_fd(root)?;
    let stat = rfs::fstat(&fd).map_err(|_| failure("cannot inspect work root"))?;
    let uid = crate::compute::execution_uid()?
        .parse::<u32>()
        .map_err(|_| failure("invalid execution UID"))?;
    if stat.st_uid != uid || stat.st_mode & 0o077 != 0 {
        return Err(failure(
            "work root must be private to the execution account",
        ));
    }
    let name = probe_name(id);
    rfs::mkdirat(&fd, name.as_str(), Mode::from_raw_mode(0o700))
        .map_err(|_| failure("cannot create a unique private probe"))?;
    let probe = rfs::openat(
        &fd,
        name.as_str(),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| failure("cannot open private probe"))?;
    let file = rfs::openat(
        &probe,
        "sentinel",
        OFlags::CREATE | OFlags::EXCL | OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|_| failure("cannot create probe sentinel"))?;
    let mut file = fs::File::from(file);
    file.write_all(&sentinel(id))
        .and_then(|()| file.sync_all())
        .map_err(|_| failure("cannot persist sentinel"))?;
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(0))
        .map_err(|_| failure("cannot seek sentinel"))?;
    let mut bytes = Vec::new();
    file.take(256)
        .read_to_end(&mut bytes)
        .map_err(|_| failure("cannot verify sentinel"))?;
    if bytes != sentinel(id) {
        return Err(failure("sentinel verification failed"));
    }
    probe_identity(&probe)
}
fn probe_identity(fd: &std::os::fd::OwnedFd) -> Result<String> {
    let stat = rfs::fstat(fd).map_err(|_| failure("cannot inspect private probe identity"))?;
    Ok(format!("{}:{}", stat.st_dev, stat.st_ino))
}
fn cleanup_probe(root: &str, id: Uuid, expected: &str) -> Result<()> {
    let fd = root_fd(root)?;
    let name = probe_name(id);
    let probe = rfs::openat(
        &fd,
        name.as_str(),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| failure("cannot reopen private probe"))?;
    if probe_identity(&probe)? != expected {
        return Err(failure("probe directory identity changed; preserve it"));
    }
    let file = rfs::openat(
        &probe,
        "sentinel",
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| failure("cannot verify probe identity"))?;
    if !rfs::fstat(&file).is_ok_and(|m| rfs::FileType::from_raw_mode(m.st_mode).is_file()) {
        return Err(failure("probe identity is not a regular file; preserve it"));
    }
    let mut bytes = Vec::new();
    fs::File::from(file)
        .take(256)
        .read_to_end(&mut bytes)
        .map_err(|_| failure("cannot read probe identity"))?;
    if bytes != sentinel(id) {
        return Err(failure("probe identity changed; preserve it"));
    }
    // Never recurse or follow a link; unexpected entries make rmdir fail safely.
    rfs::unlinkat(&probe, "sentinel", AtFlags::empty())
        .map_err(|_| failure("probe cleanup incomplete"))?;
    rfs::unlinkat(&fd, name.as_str(), AtFlags::REMOVEDIR)
        .map_err(|_| failure("probe cleanup incomplete"))
}

pub(crate) fn runner(directory: &Path, request: RunnerRequest) -> Result<Value> {
    request.definition.validate_execution().map_err(failure)?;
    if request.probe_id.is_nil()
        || directory != Path::new(&request.definition.work_root)
        || request.connector_paths.len() > 3
        || request
            .connector_paths
            .iter()
            .any(|p| !crate::setup::absolute(p))
    {
        return Err(failure("preflight request exceeds its approved host scope"));
    }
    if matches!(request.phase, ProbePhase::Cleanup) {
        cleanup_probe(
            &request.definition.work_root,
            request.probe_id,
            request
                .expected_probe_identity
                .as_deref()
                .ok_or_else(|| failure("probe cleanup requires its retained directory identity"))?,
        )?;
        return Ok(json!({"status":"ready","cleaned":true}));
    }
    let managed = request.definition.execution_mode == ExecutionMode::Managed;
    let roots = request.definition.installation_roots.iter().all(|root| {
        fs::metadata(root).is_ok_and(|m| m.is_dir())
            && accessible(Path::new(root), Access::READ_OK | Access::EXEC_OK)
    });
    let mut checks = vec![observed(
        "installation_roots",
        roots,
        ResponsibleRole::HostAdministrator,
        "Prepare every approved installation root with execution-account traversal/read access; installing a scientific model remains separate.",
    )];
    let binary = request.connector_executable.or_else(|| {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.to_str().map(str::to_owned))
    });
    let executable_private = !managed
        || binary.as_ref().is_some_and(|p| {
            Path::new(p).is_absolute()
                && fs::canonicalize(p).is_ok()
                && !modifiable_ancestor(Path::new(p))
        });
    checks.push(observed("executable_isolation", executable_private, ResponsibleRole::HostAdministrator, "Install the managed Enumeray executable under site-controlled ancestors that the execution account cannot modify; user-owned package paths may be unsuitable."));
    let credential_private = !managed
        || request.connector_paths.iter().all(|p| {
            let path = Path::new(p);
            if path.exists() {
                !accessible(path, Access::READ_OK)
                    && !accessible(path, Access::WRITE_OK)
                    && path.parent().is_some_and(|p| !modifiable_ancestor(p))
            } else {
                path.parent().is_some_and(|p| {
                    !accessible(p, Access::READ_OK) && !accessible(p, Access::WRITE_OK)
                })
            }
        });
    checks.push(observed("credential_isolation", credential_private, ResponsibleRole::HostAdministrator, "Keep connector credentials and journal inaccessible to the model execution account. Do not relax managed account separation."));
    let execution_uid = crate::compute::execution_uid()?;
    let non_root = execution_uid != "0";
    if !non_root {
        checks.push(observed(
            "execution_identity",
            false,
            ResponsibleRole::HostAdministrator,
            "Configure a non-root execution account before running writable probes.",
        ));
    }
    let probe_identity = if non_root {
        create_probe(&request.definition.work_root, request.probe_id).ok()
    } else {
        None
    };
    let created = probe_identity.is_some();
    if !created
        && Path::new(&request.definition.work_root)
            .join(probe_name(request.probe_id))
            .try_exists()
            .unwrap_or(true)
    {
        checks.push(check("probe_cleanup",CheckOutcome::Failed,true,"probe_cleanup_incomplete",ResponsibleRole::SiteOperator,
            "Inspect only the private .enumeray-preflight-PROBE_ID directory identified by this report; preserve unexpected entries and do not remove model workspaces."));
    }
    checks.push(observed("work_root", created, ResponsibleRole::HostAdministrator, "Prepare an existing private execution-account work root, with effective write/read access. Inspect any incomplete private preflight probe."));
    if let DriverConfig::Slurm(config) = &request.definition.driver {
        let available = ["sbatch", "squeue", "sacct", "scancel"]
            .iter()
            .all(|name| find_program(name))
            && accessible(Path::new(&config.worker_path), Access::EXEC_OK);
        checks.push(observed("slurm_tools", available, ResponsibleRole::SiteOperator, "Install the required Slurm tools and matching worker at the configured path on the submission host; compute-node visibility is separately unverified."));
        for (name, program, args) in [
            ("slurm_queue", "squeue", vec!["--me", "--noheader"]),
            (
                "slurm_accounting",
                "sacct",
                vec![
                    "--noheader",
                    "--allocations",
                    "--starttime=now",
                    "--format=JobIDRaw",
                ],
            ),
        ] {
            let result = connections::run_command(
                crate::drivers::slurm::scheduler_command(program)
                    .args(args)
                    .env("TZ", "UTC"),
                None,
                Duration::from_secs(5),
            );
            checks.push(observed(name, result.is_ok_and(|o| o.successful() && o.complete()), ResponsibleRole::SiteOperator, "Restore bounded Slurm monitoring/accounting access for the execution account. An empty successful response is valid; no job submission was attempted."));
        }
    }
    if let ConnectionConfig::Ssh(target) = &request.definition.connection {
        checks.push(observed(
            "execution_identity",
            non_root && account_name().is_ok_and(|n| n == target.user),
            ResponsibleRole::SiteOperator,
            "Use the configured SSH execution account.",
        ));
    }
    Ok(
        json!({"status":"ready","preflight":RunnerReport {execution_uid, checks, probe_created: created, probe_identity}}),
    )
}

fn find_program(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|path| accessible(&path.join(program), Access::EXEC_OK))
    })
}
fn sftp_probe(target: &ComputeTarget, id: Uuid, remaining: Duration) -> Result<()> {
    sftp::probe(target, id, remaining)
}

fn read_sentinel(path: &Path, id: Uuid) -> bool {
    let Ok(fd) = rfs::open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) else {
        return false;
    };
    let file = fs::File::from(fd);
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return false;
    }
    let mut bytes = Vec::new();
    file.take(256).read_to_end(&mut bytes).is_ok() && bytes == sentinel(id)
}
fn service_state(connector: &str, mode: ExecutionMode, remaining: Duration) -> HostCheck {
    let mut command = if cfg!(target_os = "macos") {
        let mut command = Command::new("/bin/launchctl");
        let domain = if mode == ExecutionMode::Development {
            format!(
                "gui/{}",
                crate::compute::execution_uid().unwrap_or_default()
            )
        } else {
            "system".into()
        };
        command.args([
            "print",
            &format!("{domain}/org.enumeray.connector.{connector}"),
        ]);
        command
    } else {
        let mut command = Command::new("systemctl");
        if mode == ExecutionMode::Development {
            command.arg("--user");
        }
        command.args([
            "show",
            &format!("enumeray-{connector}.service"),
            "--property=LoadState",
            "--property=ActiveState",
        ]);
        command
    };
    let known = connections::run_command(&mut command, None, Duration::from_secs(5).min(remaining))
        .is_ok_and(|o| o.successful() && o.complete());
    check(
        "service_state",
        if known {
            CheckOutcome::Passed
        } else {
            CheckOutcome::Unknown
        },
        false,
        if known {
            "service_state_observed"
        } else {
            "service_state_unavailable"
        },
        ResponsibleRole::ConnectorOwner,
        "Foreground startup is supported. Review native service installation/status separately; this observation does not prove host prerequisites or scientific readiness.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::DirBuilderExt;
    struct Scratch(std::path::PathBuf);
    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join(format!("host-preflight-test-{}", Uuid::new_v4()));
            fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn request(root: &Path) -> RunnerRequest {
        let spec: HostSetupSpec = serde_json::from_str(include_str!(
            "../../cli/examples/host-setup-development.json"
        ))
        .unwrap();
        let mut definition = spec.context.definition;
        definition.work_root = root.to_str().unwrap().into();
        definition.installation_roots = vec![root.to_str().unwrap().into()];
        RunnerRequest {
            probe_id: Uuid::new_v4(),
            definition,
            connector_executable: None,
            connector_paths: vec![],
            phase: ProbePhase::Inspect,
            expected_probe_identity: None,
        }
    }
    #[test]
    fn host_probe_needs_no_publication_and_cleans_only_its_private_sentinel() {
        let root = Scratch::new();
        let mut input = request(&root.0);
        let result = runner(&root.0, input.clone()).unwrap();
        if crate::compute::execution_uid().unwrap() == "0" {
            assert_eq!(result["preflight"]["probe_created"], false);
            assert!(!root.0.join(probe_name(input.probe_id)).exists());
            return;
        }
        assert_eq!(result["preflight"]["probe_created"], true);
        assert_eq!(
            fs::read(root.0.join(probe_name(input.probe_id)).join("sentinel")).unwrap(),
            sentinel(input.probe_id)
        );
        fs::write(root.0.join("unrelated"), b"preserve").unwrap();
        input.phase = ProbePhase::Cleanup;
        input.expected_probe_identity = result["preflight"]["probe_identity"]
            .as_str()
            .map(str::to_owned);
        assert_eq!(runner(&root.0, input).unwrap()["cleaned"], true);
        assert_eq!(fs::read(root.0.join("unrelated")).unwrap(), b"preserve");
    }
    #[test]
    fn changed_probe_identity_and_symlink_roots_are_preserved() {
        let root = Scratch::new();
        let input = request(&root.0);
        let identity = create_probe(root.0.to_str().unwrap(), input.probe_id).unwrap();
        let probe = root.0.join(probe_name(input.probe_id));
        fs::write(probe.join("sentinel"), b"changed").unwrap();
        assert!(cleanup_probe(root.0.to_str().unwrap(), input.probe_id, &identity).is_err());
        assert!(probe.exists());
        let link = root.0.join("alias");
        std::os::unix::fs::symlink(&root.0, &link).unwrap();
        assert!(create_probe(link.to_str().unwrap(), Uuid::new_v4()).is_err());
    }
    #[test]
    fn substituted_probe_directories_are_preserved_even_with_the_same_nonce() {
        let root = Scratch::new();
        let id = Uuid::new_v4();
        let original = create_probe(root.0.to_str().unwrap(), id).unwrap();
        let path = root.0.join(probe_name(id));
        fs::rename(&path, root.0.join("retained-probe")).unwrap();
        let replacement = create_probe(root.0.to_str().unwrap(), id).unwrap();
        assert_ne!(original, replacement);
        assert!(cleanup_probe(root.0.to_str().unwrap(), id, &original).is_err());
        assert!(path.join("sentinel").exists());
        assert!(root.0.join("retained-probe/sentinel").exists());
    }
    #[test]
    fn execution_account_access_to_connector_state_is_a_managed_failure() {
        let root = Scratch::new();
        let mut input = request(&root.0);
        input.definition.execution_mode = ExecutionMode::Managed;
        input.connector_paths = vec![root.0.to_string_lossy().into_owned()];
        let result = runner(&root.0, input.clone()).unwrap();
        let checks = result["preflight"]["checks"].as_array().unwrap();
        assert!(
            checks
                .iter()
                .any(|c| c["check"] == "credential_isolation" && c["outcome"] == "failed")
        );
        if result["preflight"]["probe_created"] == true {
            cleanup_probe(
                root.0.to_str().unwrap(),
                input.probe_id,
                result["preflight"]["probe_identity"].as_str().unwrap(),
            )
            .unwrap();
        }
    }
    #[test]
    fn cancellation_bounds_diagnostics_without_waiting_for_the_command() {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopping = stop.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            stopping.store(true, std::sync::atomic::Ordering::Release);
        });
        let start = std::time::Instant::now();
        let evidence = connections::run_cancellable(
            Command::new("/bin/sh").args(["-c", "sleep 10 & printf '%s\\n' \"$!\"; wait"]),
            None,
            Duration::from_secs(10),
            &stop,
        )
        .unwrap();
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(!evidence.successful());
        let descendant = std::str::from_utf8(evidence.stdout()).unwrap().trim();
        assert!(descendant.parse::<u32>().unwrap() > 1);
        let observed = connections::run_command(
            Command::new("/bin/ps").args(["-p", descendant, "-o", "stat="]),
            None,
            Duration::from_secs(1),
        )
        .unwrap();
        assert!(
            !observed.successful()
                || String::from_utf8_lossy(observed.stdout())
                    .trim_start()
                    .starts_with('Z')
        );
        thread.join().unwrap();
    }
    #[test]
    fn diagnostic_capture_rejects_output_above_the_report_bound() {
        let stop = std::sync::atomic::AtomicBool::new(false);
        let output = connections::run_cancellable(
            Command::new("/bin/sh").args(["-c", "head -c 400000 /dev/zero"]),
            None,
            Duration::from_secs(2),
            &stop,
        )
        .unwrap();
        assert_eq!(output.stdout().len(), MAX_REPORT_BYTES);
        assert!(!output.complete());
        assert!(!output.successful());
    }
}
