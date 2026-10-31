//! Access to a credential-free runner; no scheduler branching belongs here.
use crate::compute::{ComputeInstallation, ComputeTarget};
use crate::{ErrorKind, Result, failure, journal};
use base64::{Engine, engine::general_purpose::STANDARD};
use enumeray_contracts::execution::{ConnectionConfig, SshTarget};
use serde_json::{Value, json};
use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};
use uuid::Uuid;

fn probe(
    binding: &ComputeInstallation,
    directory: &str,
    target: crate::protocol::TransferTarget,
    offset: u64,
    length: u64,
) -> Result<crate::protocol::TransferProof> {
    let request = crate::protocol::TransferProbe {
        target,
        offset,
        length,
    };
    let value = invoke(
        binding,
        "transfer-probe",
        directory,
        Some(&serde_json::to_vec(&request).map_err(|_| failure("cannot encode transfer probe"))?),
    )?;
    let proof: crate::protocol::TransferProof = serde_json::from_value(value["transfer"].clone())
        .map_err(|_| failure("invalid transfer proof"))?;
    if proof.offset != offset
        || proof.length != length
        || proof.retained_bytes > proof.size_bytes
        || offset
            .checked_add(length)
            .is_none_or(|end| end > proof.retained_bytes)
    {
        return Err(failure("transfer proof conflicts with requested range"));
    }
    Ok(proof)
}

/// Bind a retained copy to its exact connection/job without copying launcher
/// arguments or credential references into transfer checkpoint metadata.
pub(crate) fn transfer_scope(binding: &ComputeInstallation, directory: &str) -> Result<String> {
    use sha2::{Digest, Sha256};
    let identity = serde_json::to_vec(&(directory, &binding.connection))
        .map_err(|_| failure("invalid transfer connection identity"))?;
    Ok(format!(
        "runner/{}",
        STANDARD.encode(Sha256::digest(identity))
    ))
}

fn verify_input_prefix(
    binding: &ComputeInstallation,
    directory: &str,
    target: &crate::protocol::TransferTarget,
    source: &mut std::fs::File,
    retained: &crate::protocol::TransferProof,
    deadline: std::time::Instant,
) -> Result<()> {
    use sha2::{Digest, Sha256};
    use std::io::{Seek, SeekFrom};
    source
        .seek(SeekFrom::Start(0))
        .map_err(|_| failure("cannot seek input prefix"))?;
    let mut offset = 0;
    let mut full = Sha256::new();
    let mut buffer = [0; 65536];
    while offset < retained.retained_bytes {
        let length = (retained.retained_bytes - offset).min(enumeray_client::bulk::RANGE_BYTES);
        let proof = probe(binding, directory, target.clone(), offset, length)?;
        let mut digest = Sha256::new();
        let mut remaining = length;
        while remaining > 0 {
            if std::time::Instant::now() >= deadline {
                return Err(
                    failure("input verification deadline reached").with_kind(ErrorKind::Retry)
                );
            }
            let n = source
                .read(&mut buffer[..remaining.min(65536) as usize])
                .map_err(|_| failure("cannot read input prefix"))?;
            if n == 0 {
                return Err(failure("input source truncated"));
            }
            digest.update(&buffer[..n]);
            full.update(&buffer[..n]);
            remaining -= n as u64;
            crate::transfers::advance(n as u64);
        }
        if proof.size_bytes != retained.size_bytes
            || proof.checksum_sha256 != retained.checksum_sha256
            || proof.retained_bytes != retained.retained_bytes
            || STANDARD.encode(digest.finalize()) != proof.range_sha256
        {
            return Err(failure(
                "retained runner input differs from pinned source; preserve it for inspection",
            ));
        }
        offset += length;
    }
    if source
        .metadata()
        .map_err(|_| failure("cannot inspect input source"))?
        .len()
        != retained.size_bytes
        || retained.retained_bytes == retained.size_bytes
            && STANDARD.encode(full.finalize()) != retained.checksum_sha256
    {
        return Err(failure("input source changed during staging"));
    }
    Ok(())
}

fn stage_retained_input(
    binding: &ComputeInstallation,
    directory: &str,
    id: Uuid,
    source: &Path,
) -> Result<()> {
    use std::io::{Seek, SeekFrom};
    let budgets = enumeray_contracts::transfers::TransferBudgets::from_env().map_err(failure)?;
    let deadline = std::time::Instant::now() + budgets.overall;
    let (size, checksum) = crate::artifacts::file_digest(source)?;
    crate::transfers::begin_file(size);
    let target = crate::protocol::TransferTarget::Input { artifact_id: id };
    let retained = probe(binding, directory, target.clone(), 0, 0)?;
    if retained.size_bytes != size || retained.checksum_sha256 != checksum {
        return Err(failure("input source changed since preparation"));
    }
    let mut file = std::fs::File::open(source).map_err(|_| failure("cannot open input source"))?;
    verify_input_prefix(binding, directory, &target, &mut file, &retained, deadline)?;
    let mut offset = retained.retained_bytes;
    let mut buffer = [0; 256 * 1024];
    crate::transfers::payload_progress(enumeray_client::bulk::TransferProgress {
        resumed_bytes: offset,
        total_bytes: size,
        ..Default::default()
    });
    if size == 0 {
        let body = serde_json::to_vec(&json!({"artifact_id":id,"offset":0,"data":""}))
            .map_err(|_| failure("invalid empty input"))?;
        invoke(binding, "write-input", directory, Some(&body))?;
    }
    if matches!(binding.connection, ConnectionConfig::Ssh(_)) {
        if offset < size {
            transfer(
                binding,
                source,
                &format!("{directory}/inputs/{id}"),
                true,
                size,
                deadline,
                offset,
            )?;
        }
    } else {
        file.seek(SeekFrom::Start(offset))
            .map_err(|_| failure("cannot seek staged input"))?;
        while offset < size {
            if std::time::Instant::now() >= deadline {
                return Err(failure("input staging deadline reached").with_kind(ErrorKind::Retry));
            }
            let n = file
                .read(&mut buffer[..(size - offset).min(256 * 1024) as usize])
                .map_err(|_| failure("cannot read input chunk"))?;
            if n == 0 {
                return Err(failure("input source truncated"));
            }
            let body = serde_json::to_vec(
                &json!({"artifact_id":id,"offset":offset,"data":STANDARD.encode(&buffer[..n])}),
            )
            .map_err(|_| failure("cannot encode input chunk"))?;
            invoke(binding, "write-input", directory, Some(&body))?;
            offset += n as u64;
            crate::transfers::payload_progress(enumeray_client::bulk::TransferProgress {
                resumed_bytes: retained.retained_bytes,
                transferred_bytes: offset - retained.retained_bytes,
                total_bytes: size,
                verified_bytes: 0,
            });
        }
    }
    let final_proof = probe(binding, directory, target.clone(), 0, 0)?;
    if final_proof.retained_bytes != size
        || final_proof.size_bytes != size
        || final_proof.checksum_sha256 != checksum
    {
        return Err(failure("runner input is incomplete"));
    }
    verify_input_prefix(
        binding,
        directory,
        &target,
        &mut file,
        &final_proof,
        deadline,
    )?;
    crate::transfers::payload_progress(enumeray_client::bulk::TransferProgress {
        resumed_bytes: retained.retained_bytes,
        transferred_bytes: size - retained.retained_bytes,
        total_bytes: size,
        verified_bytes: size,
    });
    crate::transfers::end_file();
    Ok(())
}

fn collect_retained(
    binding: &ComputeInstallation,
    directory: &str,
    target: crate::protocol::TransferTarget,
    output: &crate::artifacts::OutputFile,
    destination: &Path,
) -> Result<()> {
    use enumeray_client::bulk::{DownloadEvidence, RetainedTransfer};
    let budgets = enumeray_contracts::transfers::TransferBudgets::from_env().map_err(failure)?;
    let deadline = std::time::Instant::now() + budgets.overall;
    let evidence = DownloadEvidence {
        scope: transfer_scope(binding, directory)?,
        source: output.path.clone(),
        version_id: None,
        size_bytes: output.size_bytes,
        checksum_sha256: output.sha256.clone(),
    };
    crate::transfers::begin_file(output.size_bytes);
    let mut retained =
        RetainedTransfer::open(destination, evidence, deadline, crate::transfers::advance)
            .map_err(|e| {
                failure(format!(
                    "Collection checkpoint beside {}: {e}",
                    destination.display()
                ))
            })?;
    if !retained.is_published() {
        let verify = |offset, length| -> std::result::Result<String, enumeray_client::Error> {
            let proof = probe(binding, directory, target.clone(), offset, length)
                .map_err(|e| enumeray_client::Error::Invalid(e.message))?;
            if proof.size_bytes != output.size_bytes
                || proof.checksum_sha256 != output.sha256
                || proof.retained_bytes != output.size_bytes
            {
                return Err(enumeray_client::Error::Invalid(
                    "remote source differs from finalized evidence".into(),
                ));
            }
            Ok(proof.range_sha256)
        };
        retained
            .reconcile_external(verify, crate::transfers::advance)
            .map_err(|e| failure(e.to_string()))?;
        retained.mark_resumed();
        crate::transfers::payload_progress(retained.progress());
        if matches!(binding.connection, ConnectionConfig::Ssh(_)) {
            let remote = match &target {
                crate::protocol::TransferTarget::Output { path } => {
                    format!("{directory}/workspace/{path}")
                }
                crate::protocol::TransferTarget::Archive => {
                    format!("{directory}/log-archives/logs.tar.gz")
                }
                _ => return Err(failure("invalid collection target")),
            };
            if retained.offset() < output.size_bytes {
                transfer(
                    binding,
                    &retained.payload_path(),
                    &remote,
                    false,
                    output.size_bytes,
                    deadline,
                    retained.offset(),
                )?;
                retained
                    .reconcile_external(verify, crate::transfers::advance)
                    .map_err(|e| failure(e.to_string()))?;
            }
        } else {
            retained
                .discard_uncommitted()
                .map_err(|e| failure(e.to_string()))?;
            while retained.offset() < output.size_bytes {
                let offset = retained.offset();
                let (action, body) = match &target {
                    crate::protocol::TransferTarget::Output { path } => {
                        ("read-output", json!({"path":path,"offset":offset}))
                    }
                    crate::protocol::TransferTarget::Archive => {
                        ("read-log-archive", json!({"offset":offset}))
                    }
                    _ => return Err(failure("invalid collection target")),
                };
                let result = invoke(
                    binding,
                    action,
                    directory,
                    Some(
                        &serde_json::to_vec(&body)
                            .map_err(|_| failure("invalid collection request"))?,
                    ),
                );
                let reply = match result {
                    Ok(reply) => reply,
                    Err(error) => {
                        retained.checkpoint().map_err(|e| failure(e.to_string()))?;
                        return Err(error);
                    }
                };
                let bytes = STANDARD
                    .decode(
                        reply["data"]
                            .as_str()
                            .ok_or_else(|| failure("output chunk missing"))?,
                    )
                    .map_err(|_| failure("invalid output chunk"))?;
                if bytes.is_empty() || bytes.len() > 256 * 1024 {
                    return Err(failure("invalid output chunk length"));
                }
                retained
                    .append(&bytes)
                    .map_err(|e| failure(e.to_string()))?;
                crate::transfers::payload_progress(retained.progress());
            }
        }
    }
    let progress = retained
        .finish_with_progress(crate::transfers::advance)
        .map_err(|e| failure(e.to_string()))?;
    crate::transfers::payload_progress(progress);
    crate::transfers::end_file();
    Ok(())
}
impl ComputeInstallation {
    fn ssh(&self) -> Option<&SshTarget> {
        match &self.connection {
            ConnectionConfig::Ssh(t) => Some(t),
            _ => None,
        }
    }
    fn launcher(&self) -> &[String] {
        match &self.connection {
            ConnectionConfig::Direct { launcher } => launcher,
            _ => &[],
        }
    }
}
pub(crate) trait ComputeConnection {
    fn validate(&self, binding: &ComputeInstallation) -> Result<()>;
    fn invoke(
        &self,
        binding: &ComputeInstallation,
        action: &str,
        directory: &str,
        input: Option<&[u8]>,
    ) -> Result<Value> {
        invoke(binding, action, directory, input)
    }
    fn download_archive(
        &self,
        binding: &ComputeInstallation,
        directory: &str,
        file: &crate::artifacts::OutputFile,
        target: &Path,
    ) -> Result<()> {
        collect_retained(
            binding,
            directory,
            crate::protocol::TransferTarget::Archive,
            file,
            target,
        )
    }
    fn upload_input(
        &self,
        binding: &ComputeInstallation,
        directory: &str,
        id: Uuid,
        source: &Path,
    ) -> Result<()>;
    fn download_output(
        &self,
        binding: &ComputeInstallation,
        directory: &str,
        file: &crate::artifacts::OutputFile,
        target: &Path,
    ) -> Result<()>;
}
struct DirectConnection;
struct SshConnection;
pub(crate) fn resolve(binding: &ComputeInstallation) -> &'static dyn ComputeConnection {
    match binding.connection {
        ConnectionConfig::Direct { .. } => &DirectConnection,
        ConnectionConfig::Ssh(_) => &SshConnection,
    }
}
impl ComputeConnection for DirectConnection {
    fn validate(&self, binding: &ComputeInstallation) -> Result<()> {
        if binding.launcher().is_empty() || !Path::new(&binding.launcher()[0]).is_absolute() {
            return Err(failure(
                "direct connection requires an absolute restricted launcher",
            ));
        }
        Ok(())
    }
    fn upload_input(
        &self,
        binding: &ComputeInstallation,
        directory: &str,
        id: Uuid,
        source: &Path,
    ) -> Result<()> {
        stage_retained_input(binding, directory, id, source)
    }

    fn download_output(
        &self,
        binding: &ComputeInstallation,
        directory: &str,
        file: &crate::artifacts::OutputFile,
        target: &Path,
    ) -> Result<()> {
        collect_retained(
            binding,
            directory,
            crate::protocol::TransferTarget::Output {
                path: file.path.clone(),
            },
            file,
            target,
        )
    }
}

impl ComputeConnection for SshConnection {
    fn validate(&self, binding: &ComputeInstallation) -> Result<()> {
        let key = binding
            .identity_file
            .as_deref()
            .ok_or_else(|| failure("SSH credential reference is not configured on this host"))?;
        journal::check_private_path(key, false)
    }
    fn upload_input(
        &self,
        binding: &ComputeInstallation,
        directory: &str,
        id: Uuid,
        source: &Path,
    ) -> Result<()> {
        stage_retained_input(binding, directory, id, source)
    }

    fn download_output(
        &self,
        binding: &ComputeInstallation,
        directory: &str,
        file: &crate::artifacts::OutputFile,
        target: &Path,
    ) -> Result<()> {
        collect_retained(
            binding,
            directory,
            crate::protocol::TransferTarget::Output {
                path: file.path.clone(),
            },
            file,
            target,
        )
    }
}

pub(crate) fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
fn safe_absolute(s: &str) -> bool {
    Path::new(s).is_absolute()
        && !s.contains(['\0', '\n', '\r'])
        && !Path::new(s)
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
}
#[cfg(test)]
pub(crate) fn ssh_command(binding: &ComputeInstallation) -> Result<Command> {
    ssh_target_command(&binding.target())
}
pub(crate) fn ssh_target_command(binding: &ComputeTarget) -> Result<Command> {
    ssh_target_options(binding, false)
}
pub(crate) fn sftp_target_command(binding: &ComputeTarget) -> Result<Command> {
    ssh_target_options(binding, true)
}
fn ssh_target_options(binding: &ComputeTarget, subsystem: bool) -> Result<Command> {
    let ssh = binding.ssh().ok_or_else(|| failure("missing SSH target"))?;
    let budgets = enumeray_contracts::transfers::TransferBudgets::from_env().map_err(failure)?;
    let mut command = Command::new("ssh");
    command
        .args([
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            &format!("ConnectTimeout={}", budgets.connect.as_secs()),
            "-o",
            "IdentitiesOnly=yes",
            "-p",
            &ssh.port.to_string(),
            "-i",
        ])
        .arg(
            binding
                .identity_file
                .as_ref()
                .ok_or_else(|| failure("SSH identity is unavailable"))?,
        );
    if subsystem {
        command.arg("-s");
    }
    command.arg(format!("{}@{}", ssh.user, ssh.host));
    if subsystem {
        command.arg("sftp");
    }
    Ok(command)
}

fn invoke(
    binding: &ComputeInstallation,
    action: &str,
    directory: &str,
    input: Option<&[u8]>,
) -> Result<Value> {
    invoke_target(&binding.target(), action, directory, input)
}

pub(crate) fn invoke_target(
    binding: &ComputeTarget,
    action: &str,
    directory: &str,
    input: Option<&[u8]>,
) -> Result<Value> {
    invoke_target_budget(binding, action, directory, input, Duration::MAX)
}

pub(crate) fn invoke_preflight(
    binding: &ComputeTarget,
    input: &[u8],
    remaining: Duration,
) -> Result<Value> {
    invoke_target_budget(
        binding,
        "preflight",
        &binding.work_root,
        Some(input),
        remaining,
    )
}

fn invoke_target_budget(
    binding: &ComputeTarget,
    action: &str,
    directory: &str,
    input: Option<&[u8]>,
    remaining: Duration,
) -> Result<Value> {
    let envelope = crate::protocol::Request::new(action, input)?;
    let request =
        serde_json::to_vec(&envelope).map_err(|_| failure("cannot encode runner request"))?;
    let mut command = match &binding.connection {
        ConnectionConfig::Direct { .. } => launcher_command(binding, action, directory)?,
        ConnectionConfig::Ssh(ssh) => {
            let mut command = ssh_target_command(binding)?;
            command.arg(format!(
                "{} compute-runner {} --directory {}",
                quote(&ssh.runner_path),
                quote(action),
                quote(directory)
            ));
            command
        }
    };
    let budgets = enumeray_contracts::transfers::TransferBudgets::from_env().map_err(failure)?;
    let budget = match action {
        "prepare" | "start" | "build-log-archive" => budgets.overall,
        "transfer-probe" | "write-input" | "read-output" | "read-log-archive" => {
            budgets.stall.min(budgets.overall)
        }
        _ => Duration::from_secs(30),
    };
    let output = if action == "preflight" {
        run_command_progress(
            &mut command,
            Some(&request),
            budget.min(remaining),
            false,
            None,
            enumeray_contracts::setup::MAX_REPORT_BYTES,
        )?
    } else {
        run_command(&mut command, Some(&request), budget.min(remaining))?
    };
    if action == "preflight" && output.stdout().len() > enumeray_contracts::setup::MAX_REPORT_BYTES
    {
        return Err(failure("preflight runner response exceeds its bound"));
    }
    if !output.successful() {
        return Err(
            failure("runner transport failed; preserve the original launch")
                .with_command(output.diagnostics()),
        );
    }
    crate::protocol::Response::decode(output.stdout())
        .map_err(|error| error.with_command(output.diagnostics()))
}

fn launcher_command(binding: &ComputeTarget, action: &str, directory: &str) -> Result<Command> {
    let executable = binding
        .launcher()
        .first()
        .ok_or_else(|| failure("restricted launcher missing"))?;
    if !Path::new(executable).is_absolute() {
        return Err(failure("restricted launcher must be absolute"));
    }
    let mut command = Command::new(executable);
    command
        .args(&binding.launcher()[1..])
        .args(["compute-runner", action, "--directory", directory])
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin");
    Ok(command)
}
/// OpenSSH SFTP is used with a batch passed on stdin, never a shell program.
fn transfer(
    binding: &ComputeInstallation,
    local: &Path,
    remote: &str,
    upload: bool,
    size: u64,
    deadline: std::time::Instant,
    offset: u64,
) -> Result<()> {
    let ssh = binding
        .ssh()
        .ok_or_else(|| failure("missing SSH transfer target"))?;
    if !safe_absolute(remote)
        || local
            .to_str()
            .is_none_or(|s| s.contains(['\n', '\r', '\0']))
    {
        return Err(failure("invalid transfer path"));
    }
    fn batch_quote(s: &str) -> String {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    }
    let (source, destination) = if upload {
        (batch_quote(&local.to_string_lossy()), batch_quote(remote))
    } else {
        (batch_quote(remote), batch_quote(&local.to_string_lossy()))
    };
    let batch = format!(
        "{} {source} {destination}\n",
        match (upload, offset > 0) {
            (true, true) => "reput",
            (true, false) => "put",
            (false, true) => "reget",
            (false, false) => "get",
        }
    );
    let budgets = enumeray_contracts::transfers::TransferBudgets::from_env().map_err(failure)?;
    let connect_option = format!("ConnectTimeout={}", budgets.connect.as_secs());
    let mut command = Command::new("sftp");
    command
        .args([
            "-vvv",
            "-R",
            "1",
            "-B",
            "65536",
            "-b",
            "-",
            "-o",
            "BatchMode=yes",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            &connect_option,
            "-P",
            &ssh.port.to_string(),
            "-i",
        ])
        .arg(
            binding
                .identity_file
                .as_ref()
                .ok_or_else(|| failure("SSH identity unavailable"))?,
        )
        .arg(format!("{}@{}", ssh.user, ssh.host));
    crate::transfers::begin_file(size);
    crate::transfers::payload_observation(enumeray_client::bulk::TransferProgress {
        resumed_bytes: offset,
        total_bytes: size,
        ..Default::default()
    });
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    if remaining.is_zero() {
        return Err(failure("SFTP attempt deadline reached").with_kind(ErrorKind::Retry));
    }
    let evidence = run_command_progress(
        &mut command,
        Some(batch.as_bytes()),
        remaining.min(budgets.overall),
        true,
        None,
        4 * 1024 * 1024,
    )?;
    if !evidence.successful() {
        return Err(failure("SFTP transfer interrupted").with_kind(ErrorKind::Retry));
    }
    crate::transfers::payload_observation(enumeray_client::bulk::TransferProgress {
        resumed_bytes: offset,
        transferred_bytes: size - offset,
        total_bytes: size,
        verified_bytes: 0,
    });
    crate::transfers::end_file();
    Ok(())
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum CommandState {
    NotSpawned {
        reason: String,
    },
    Exited {
        code: Option<i32>,
        signal: Option<i32>,
    },
    TimedOut,
    ObservationFailed,
}

#[derive(Clone, Default)]
struct Capture {
    bytes: Vec<u8>,
    dropped: u64,
    complete: bool,
}

pub(crate) struct CommandEvidence {
    pub(crate) state: CommandState,
    stdout: Capture,
    stderr: Capture,
    input_complete: bool,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommandDiagnostics {
    pub(crate) state: CommandState,
    stdout: String,
    stderr: String,
    stdout_dropped: u64,
    stderr_dropped: u64,
    stdout_complete: bool,
    stderr_complete: bool,
    input_complete: bool,
}

impl CommandDiagnostics {
    fn evidence(&self) -> Result<CommandEvidence> {
        self.validate()?;
        Ok(CommandEvidence {
            state: self.state.clone(),
            stdout: Capture {
                bytes: STANDARD
                    .decode(&self.stdout)
                    .map_err(|_| failure("invalid stdout evidence"))?,
                dropped: self.stdout_dropped,
                complete: self.stdout_complete,
            },
            stderr: Capture {
                bytes: STANDARD
                    .decode(&self.stderr)
                    .map_err(|_| failure("invalid stderr evidence"))?,
                dropped: self.stderr_dropped,
                complete: self.stderr_complete,
            },
            input_complete: self.input_complete,
        })
    }

    pub(crate) fn rejected_message(&self) -> Result<Option<&'static str>> {
        Ok(crate::drivers::slurm::rejection(&self.evidence()?))
    }
    pub(crate) fn validate(&self) -> Result<()> {
        for data in [&self.stdout, &self.stderr] {
            if STANDARD
                .decode(data)
                .map_or(true, |bytes| bytes.len() > 65536)
            {
                return Err(failure("invalid bounded command diagnostics"));
            }
        }
        if let CommandState::NotSpawned { reason } = &self.state
            && !matches!(reason.as_str(), "not_found" | "permission_denied" | "other")
        {
            return Err(failure("invalid command spawn evidence"));
        }
        Ok(())
    }
}

impl CommandEvidence {
    pub(crate) fn diagnostics(&self) -> CommandDiagnostics {
        CommandDiagnostics {
            state: self.state.clone(),
            stdout: STANDARD.encode(&self.stdout.bytes[..self.stdout.bytes.len().min(65536)]),
            stderr: STANDARD.encode(&self.stderr.bytes),
            stdout_dropped: self
                .stdout
                .dropped
                .saturating_add(self.stdout.bytes.len().saturating_sub(65536) as u64),
            stderr_dropped: self.stderr.dropped,
            stdout_complete: self.stdout.complete,
            stderr_complete: self.stderr.complete,
            input_complete: self.input_complete,
        }
    }
    pub(crate) fn stdout(&self) -> &[u8] {
        &self.stdout.bytes
    }
    pub(crate) fn stderr(&self) -> &[u8] {
        &self.stderr.bytes
    }
    pub(crate) fn complete(&self) -> bool {
        self.input_complete
            && self.stdout.complete
            && self.stderr.complete
            && self.stdout.dropped == 0
            && self.stderr.dropped == 0
    }
    pub(crate) fn successful(&self) -> bool {
        matches!(
            self.state,
            CommandState::Exited {
                code: Some(0),
                signal: None
            }
        ) && self.input_complete
            && self.stdout.complete
            && self.stdout.dropped == 0
    }
}

/// Keep byte bounds while draining both streams. Dropping the runtime at the
/// deadline closes inherited pipes instead of joining an indefinitely blocked reader.
pub(crate) fn run_command(
    command: &mut Command,
    input: Option<&[u8]>,
    timeout: Duration,
) -> Result<CommandEvidence> {
    run_command_progress(command, input, timeout, false, None, 4 * 1024 * 1024)
}

pub(crate) fn run_cancellable(
    command: &mut Command,
    input: Option<&[u8]>,
    timeout: Duration,
    stop: &std::sync::atomic::AtomicBool,
) -> Result<CommandEvidence> {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
    run_command_progress(
        command,
        input,
        timeout,
        false,
        Some(stop),
        enumeray_contracts::setup::MAX_REPORT_BYTES,
    )
}

fn run_command_progress(
    command: &mut Command,
    input: Option<&[u8]>,
    timeout: Duration,
    sftp_progress: bool,
    stop: Option<&std::sync::atomic::AtomicBool>,
    stdout_limit: usize,
) -> Result<CommandEvidence> {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let stall = if sftp_progress {
        enumeray_contracts::transfers::TransferBudgets::from_env()
            .map_err(failure)?
            .stall
    } else {
        Duration::MAX
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| failure("cannot create bounded command transport"))?;
    let mut child = match command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return Ok(CommandEvidence {
                state: CommandState::NotSpawned {
                    reason: match error.kind() {
                        std::io::ErrorKind::NotFound => "not_found",
                        std::io::ErrorKind::PermissionDenied => "permission_denied",
                        _ => "other",
                    }
                    .into(),
                },
                stdout: Capture::default(),
                stderr: Capture::default(),
                input_complete: false,
            });
        }
    };
    let input = input.unwrap_or_default().to_vec();
    let wire = Arc::new(AtomicU64::new(0));
    let stdout = Arc::new(Mutex::new(Capture::default()));
    let stderr = Arc::new(Mutex::new(Capture::default()));
    let result: Result<(CommandState, bool)> = runtime.block_on(async {
        let mut stdin = tokio::process::ChildStdin::from_std(child.stdin.take().expect("stdin"))
            .map_err(|_| failure("cannot observe command input"))?;
        let out = tokio::process::ChildStdout::from_std(child.stdout.take().expect("stdout"))
            .map_err(|_| failure("cannot observe command output"))?;
        let err = tokio::process::ChildStderr::from_std(child.stderr.take().expect("stderr"))
            .map_err(|_| failure("cannot observe command diagnostics"))?;
        async fn capture(
            mut pipe: impl tokio::io::AsyncRead + Unpin,
            shared: Arc<Mutex<Capture>>,
            limit: usize,
            sftp_progress: bool,
            wire: Arc<AtomicU64>,
        ) {
            let mut buffer = [0; 8192];
            let mut line = Vec::new();
            loop {
                let size = match pipe.read(&mut buffer).await {
                    Ok(size) => size,
                    Err(_) => return,
                };
                if sftp_progress {
                    for byte in &buffer[..size] {
                        if *byte == b'\n' {
                            if let Ok(line) = std::str::from_utf8(&line)
                                && let Some(size) = sftp_progress_bytes(line)
                            {
                                crate::transfers::advance(size);
                                wire.fetch_add(size, Ordering::Relaxed);
                            }
                            line.clear();
                        } else if line.len() < 8192 {
                            line.push(*byte);
                        }
                    }
                }
                let mut saved = shared.lock().expect("command capture");
                if size == 0 {
                    saved.complete = true;
                    return;
                }
                let retain = size.min(limit - saved.bytes.len());
                saved.bytes.extend_from_slice(&buffer[..retain]);
                saved.dropped = saved.dropped.saturating_add((size - retain) as u64);
            }
        }
        let writer = tokio::spawn(async move { stdin.write_all(&input).await.is_ok() });
        let reader = tokio::spawn(capture(
            out,
            stdout.clone(),
            stdout_limit,
            false,
            wire.clone(),
        ));
        let errors = tokio::spawn(capture(
            err,
            stderr.clone(),
            65536,
            sftp_progress,
            wire.clone(),
        ));
        let deadline = std::time::Instant::now() + timeout;
        let mut progressed = std::time::Instant::now();
        let mut observed = 0;
        let mut exited = None;
        let state = loop {
            if exited.is_none() {
                match child.try_wait() {
                    Ok(Some(status)) => exited = Some(status),
                    Ok(None) => (),
                    Err(_) => break CommandState::ObservationFailed,
                }
            }
            if let Some(status) = exited
                && writer.is_finished()
                && reader.is_finished()
                && errors.is_finished()
            {
                #[cfg(unix)]
                let signal = {
                    use std::os::unix::process::ExitStatusExt;
                    status.signal()
                };
                #[cfg(not(unix))]
                let signal = None;
                break CommandState::Exited {
                    code: status.code(),
                    signal,
                };
            }
            let current = wire.load(Ordering::Relaxed);
            if current > observed {
                observed = current;
                progressed = std::time::Instant::now();
            }
            if std::time::Instant::now() >= deadline
                || sftp_progress && progressed.elapsed() >= stall
                || stop.is_some_and(|s| s.load(Ordering::Acquire))
            {
                break CommandState::TimedOut;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let input_complete = writer.is_finished() && writer.await.unwrap_or(false);
        Ok((state, input_complete))
    });
    let (state, input_complete) = match result {
        Ok(result) => result,
        Err(_) => (CommandState::ObservationFailed, false),
    };
    if !matches!(state, CommandState::Exited { .. }) {
        if stop.is_some() {
            crate::transfers::kill_group(child.id());
        }
        let _ = child.kill();
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
    drop(runtime);
    Ok(CommandEvidence {
        state,
        input_complete,
        stdout: stdout.lock().expect("command capture").clone(),
        stderr: stderr.lock().expect("command capture").clone(),
    })
}

/// Count numeric OpenSSH protocol diagnostics, never forward their raw text.
/// These are processed bytes, not resumable offsets or integrity evidence.
fn sftp_progress_bytes(line: &str) -> Option<u64> {
    if let Some((_, ack)) = line.split_once("In write loop, ack for ") {
        let parts = ack.split_whitespace().collect::<Vec<_>>();
        if parts.len() == 5 && parts[2] == "bytes" && parts[3] == "at" {
            return parts[1].parse().ok();
        }
    }
    if let Some((_, range)) = line.split_once("Received data ") {
        let (start, end) = range.split_once(" -> ")?;
        return end
            .trim()
            .parse::<u64>()
            .ok()?
            .checked_sub(start.parse().ok()?)?
            .checked_add(1);
    }
    None
}

pub(crate) fn run_bounded(
    command: &mut Command,
    input: Option<&[u8]>,
    timeout: Duration,
) -> Result<Vec<u8>> {
    let evidence = run_command(command, input, timeout)?;
    if evidence.successful() {
        return Ok(evidence.stdout.bytes);
    }
    Err(
        failure("execution transport failed; submission outcome may be unknown").with_kind(
            if evidence.state == CommandState::TimedOut {
                ErrorKind::Retry
            } else {
                ErrorKind::Blocked
            },
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sftp_progress_counts_acknowledged_numeric_data_without_exposing_diagnostics() {
        assert_eq!(
            sftp_progress_bytes("debug3: In write loop, ack for 7 32768 bytes at 0"),
            Some(32768)
        );
        assert_eq!(
            sftp_progress_bytes("debug3: Received data 32768 -> 65535"),
            Some(32768)
        );
        assert_eq!(sftp_progress_bytes("debug3: Received data 40 -> 2"), None);
        assert_eq!(sftp_progress_bytes("private path or key"), None);
    }

    #[test]
    fn commands_retain_bounded_diagnostics_while_draining_both_streams() {
        let evidence = run_command(Command::new("/usr/bin/python3").args(["-c",
            "import sys;sys.stdout.buffer.write(b'x'*2097152);sys.stderr.buffer.write(b'y'*131072)"]), None, Duration::from_secs(5)).unwrap();
        assert!(evidence.successful());
        assert_eq!(evidence.stdout().len(), 2097152);
        assert_eq!(evidence.stderr().len(), 65536);
        assert_eq!(evidence.stderr.dropped, 65536);
        assert!(!evidence.complete());
        let diagnostics = evidence.diagnostics();
        diagnostics.validate().unwrap();
        assert_eq!(diagnostics.stdout_dropped, 2097152 - 65536);
        assert_eq!(diagnostics.stderr_dropped, 65536);
    }

    #[test]
    fn inherited_open_pipes_do_not_extend_the_command_deadline() {
        let start = std::time::Instant::now();
        let evidence = run_command(
            Command::new("/bin/sh").args(["-c", "sleep 2 & exit 0"]),
            None,
            Duration::from_millis(100),
        )
        .unwrap();
        assert_eq!(evidence.state, CommandState::TimedOut);
        assert!(!evidence.complete());
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn command_spawn_failure_is_distinct_from_an_ambiguous_nonzero_exit() {
        let missing = run_command(
            &mut Command::new("/enumeray/missing-command"),
            None,
            Duration::from_secs(1),
        )
        .unwrap();
        assert!(matches!(missing.state, CommandState::NotSpawned { .. }));
        let failed = run_command(
            Command::new("/bin/sh").args(["-c", "printf unknown >&2; exit 1"]),
            None,
            Duration::from_secs(1),
        )
        .unwrap();
        assert!(matches!(
            failed.state,
            CommandState::Exited { code: Some(1), .. }
        ));
        assert_eq!(failed.stderr(), b"unknown");
        assert!(failed.complete());
        assert!(failed.diagnostics().rejected_message().unwrap().is_none());
    }
}
