mod evidence;
mod launch;
use super::*;
use crate::connections::{CommandEvidence, CommandState, run_bounded, run_command};
use crate::launch::{FailureStage, LaunchOutcome, StartupFailure};
pub(crate) use evidence::SchedulerEvidence;
pub(crate) use launch::read_script as read_batch_file;
use std::process::Command;

/// Inherited SBATCH_*, SLURM_CLUSTERS and query-format settings cannot redirect a run.
pub(crate) fn scheduler_command(program: &str) -> Command {
    let mut command = Command::new(program);
    command.env_clear();
    if program == "sacct" {
        command.env("TZ", "UTC");
    }
    for key in ["PATH", "HOME", "SLURM_CONF"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
}

pub(super) struct SlurmDriver;
/// These controller validation errors cannot be an accepted-but-lost reply.
/// Unknown/plugin/communication messages and incomplete command evidence are
/// deliberately excluded. See SchedMD src/sbatch/sbatch.c and slurm_errno.c.
pub(crate) fn rejection(output: &CommandEvidence) -> Option<&'static str> {
    if !matches!(output.state, CommandState::Exited { code: Some(code), signal: None } if code != 0)
        || !output.complete()
        || !output.stdout().is_empty()
    {
        return None;
    }
    let text = std::str::from_utf8(output.stderr()).ok()?;
    let mut lines = text.lines();
    let first = lines.next()?;
    let terminal = if let Some(partition) =
        first.strip_prefix("sbatch: error: invalid partition specified: ")
    {
        if !safe_scheduler_name(partition) {
            return None;
        }
        lines.next()?
    } else {
        first
    };
    if lines.next().is_some() {
        return None;
    }
    match terminal.strip_prefix("sbatch: error: Batch job submission failed: ")? {
        "Invalid partition name specified" => {
            Some("Slurm rejected the batch submission: invalid partition.")
        }
        "Invalid account or account/partition combination specified" => Some(
            "Slurm rejected the batch submission: invalid account or account/partition combination.",
        ),
        "Invalid qos specification" => Some("Slurm rejected the batch submission: invalid QoS."),
        _ => None,
    }
}
impl ExecutionDriver for SlurmDriver {
    fn capabilities(&self) -> DriverCapabilities {
        DriverCapabilities {
            cancel_pending: true,
            cancel_running: true,
        }
    }
    fn validate(&self) -> Result<()> {
        // Probe monitoring access only. Slurm interprets and authorizes the model's
        // directives when the installed batch file is submitted.
        run_bounded(
            scheduler_command("sacct").args([
                "--noheader",
                "--allocations",
                "--starttime=now",
                "--format=JobIDRaw",
            ]),
            None,
            Duration::from_secs(20),
        )?;
        Ok(())
    }
    fn prepare_launch(&self, directory: &Path, job: &ComputeJob) -> Result<()> {
        local_process::write_record(directory, "launch-intent.json", &launch::prepare(job)?)
    }
    fn submit(&self, directory: &Path, job: &ComputeJob) -> Result<LaunchOutcome> {
        let DriverConfig::Slurm(config) = &job.execution.driver else {
            return Err(failure("Slurm driver configuration required"));
        };
        let intent = launch::load(directory, job)?;
        let script = job
            .execution
            .slurm_script
            .as_deref()
            .ok_or_else(|| failure("Slurm batch file required"))?;
        let checked = launch::read_script(Path::new(script)).and_then(|(_, digest)| {
            if digest != intent.script_digest {
                return Err(failure(
                    "installed Slurm batch file changed after validation",
                ));
            }
            Ok(())
        });
        if let Err(error) = checked {
            return Ok(LaunchOutcome::ProvenFailure {
                evidence: Box::new(StartupFailure::new(
                    job.intent,
                    &job.assignment,
                    FailureStage::LaunchPreparation,
                    &error.message,
                    None,
                )?),
            });
        }
        let mut command = scheduler_command("sbatch");
        command
            .args([
                "--parsable",
                "--job-name",
                &format!("enumeray-{}", intent.intent),
                "--output",
            ])
            .arg(directory.join("slurm.log"))
            .arg("--error")
            .arg(directory.join("slurm.log"))
            .arg("--chdir")
            .arg(directory)
            .arg("--")
            .arg(script)
            .arg(&config.worker_path)
            .arg(directory);
        let output = run_command(&mut command, None, Duration::from_secs(20))?;
        let failed = if matches!(output.state, CommandState::NotSpawned { .. }) {
            Some((
                FailureStage::LaunchSpawn,
                "Cannot start the Slurm submission command.",
            ))
        } else {
            rejection(&output).map(|message| (FailureStage::BackendRejection, message))
        };
        if let Some((stage, message)) = failed {
            return Ok(LaunchOutcome::ProvenFailure {
                evidence: Box::new(StartupFailure::new(
                    job.intent,
                    &job.assignment,
                    stage,
                    message,
                    Some(output.diagnostics()),
                )?),
            });
        }
        let receipt = match parse_sbatch(output.stdout()) {
            Ok(receipt) => receipt,
            Err(_) => {
                return Ok(LaunchOutcome::Uncertain {
                    command: output.diagnostics(),
                });
            }
        };
        // Slurm's returned destination is authoritative; retain it for recovery.
        local_process::write_record(directory, "slurm-receipt.json", &receipt)?;
        let handle = match receipt_handle(&receipt)? {
            Some(handle) => handle,
            None => match recover_slurm_handle(&intent) {
                Ok(Some(handle)) if handle.job_id == receipt.job_id => handle,
                _ => {
                    return Ok(LaunchOutcome::Uncertain {
                        command: output.diagnostics(),
                    });
                }
            },
        };
        local_process::write_record(directory, "slurm.json", &handle)?;
        Ok(if output.successful() {
            LaunchOutcome::Accepted
        } else {
            LaunchOutcome::Uncertain {
                command: output.diagnostics(),
            }
        })
    }
    fn reconcile(&self, directory: &Path, job: &ComputeJob) -> Result<Option<ExecutionHandle>> {
        if !directory.join("launch-intent.json").exists() {
            return Ok(None);
        }
        let intent = launch::load(directory, job)?;
        let receipt: Option<SbatchReceipt> = if directory.join("slurm-receipt.json").exists() {
            Some(local_process::read_record(
                &directory.join("slurm-receipt.json"),
            )?)
        } else {
            None
        };
        let received = receipt.as_ref().map(receipt_handle).transpose()?.flatten();
        if !directory.join("slurm.json").exists() {
            let recovered = match received {
                Some(handle) => Some(handle),
                None => recover_slurm_handle(&intent)?,
            };
            if let Some(handle) = recovered {
                if receipt.as_ref().is_some_and(|r| r.job_id != handle.job_id) {
                    return Err(failure(
                        "recovered Slurm job conflicts with submission receipt",
                    ));
                }
                local_process::write_record(directory, "slurm.json", &handle)?;
            } else {
                return Ok(None);
            }
        }
        let handle: SlurmHandle = local_process::read_record(&directory.join("slurm.json"))?;
        if handle.job_id == 0
            || !safe_scheduler_name(&handle.cluster)
            || receipt.as_ref().is_some_and(|r| {
                r.job_id != handle.job_id
                    || r.cluster.as_ref().is_some_and(|c| c != &handle.cluster)
            })
        {
            return Err(failure("invalid persisted Slurm handle"));
        }
        Ok(Some(ExecutionHandle::Slurm(handle)))
    }
    fn observe(
        &self,
        directory: &Path,
        job: &ComputeJob,
        handle: &ExecutionHandle,
    ) -> Result<Observation> {
        let ExecutionHandle::Slurm(handle) = handle else {
            return Err(failure("Slurm execution handle required"));
        };
        evidence::observe(handle, &launch::load(directory, job)?)
    }
    fn request_cancel(
        &self,
        directory: &Path,
        job: &ComputeJob,
        handle: &ExecutionHandle,
        scope: CancellationScope,
    ) -> Result<CancellationOutcome> {
        let ExecutionHandle::Slurm(handle) = handle else {
            return Err(failure("Slurm execution handle required"));
        };
        match evidence::observe(handle, &launch::load(directory, job)?) {
            Ok(Observation::Stopped { .. }) => return Ok(CancellationOutcome::AlreadyTerminal),
            Err(_) => {
                return Ok(CancellationOutcome::Unresolved {
                    reason: "scheduler observation unavailable before cancellation".into(),
                });
            }
            _ => (),
        }
        let mut command = scheduler_command("scancel");
        command.args(["--ctld", "--quiet"]);
        if scope == CancellationScope::Pending {
            if !local_process::pending_cancellation(directory, job.intent)? {
                local_process::write_record(
                    directory,
                    "pending-cancellation.json",
                    &serde_json::json!({"intent":job.intent,"scope":"pending"}),
                )?;
            }
            command.arg("--state=PENDING");
        }
        command.arg(handle.job_id.to_string());
        command.args(["--clusters", &handle.cluster]);
        // The scheduler evaluates PENDING atomically. CONFIGURING/starting or
        // running work wins the race and drains. No sent marker suppresses a retry
        // after a lost reply; every repeat addresses this same persisted handle.
        Ok(
            match run_bounded(&mut command, None, Duration::from_secs(20)) {
                Ok(_) => CancellationOutcome::Accepted,
                Err(_) => CancellationOutcome::Unresolved {
                    reason: "cancellation reply absent; observe or repeat the same request".into(),
                },
            },
        )
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct SbatchReceipt {
    pub(crate) job_id: u64,
    pub(crate) cluster: Option<String>,
}

pub(crate) fn parse_sbatch(bytes: &[u8]) -> Result<SbatchReceipt> {
    let value = std::str::from_utf8(bytes)
        .map_err(|_| failure("invalid sbatch response"))?
        .trim();
    let mut fields = value.split(';');
    let job_id = fields
        .next()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .ok_or_else(|| failure("invalid Slurm job identity"))?;
    let cluster = fields.next().map(str::to_owned);
    if fields.next().is_some() || cluster.as_ref().is_some_and(|s| !safe_scheduler_name(s)) {
        return Err(failure("invalid Slurm cluster identity"));
    }
    Ok(SbatchReceipt { job_id, cluster })
}
fn receipt_handle(receipt: &SbatchReceipt) -> Result<Option<SlurmHandle>> {
    if receipt.job_id == 0 {
        return Err(failure("invalid persisted Slurm receipt"));
    }
    let cluster = match &receipt.cluster {
        Some(cluster) if safe_scheduler_name(cluster) => cluster.clone(),
        Some(_) => return Err(failure("invalid persisted Slurm receipt cluster")),
        None => return Ok(None),
    };
    Ok(Some(SlurmHandle {
        job_id: receipt.job_id,
        cluster,
    }))
}

fn recover_slurm_handle(intent: &launch::LaunchIntent) -> Result<Option<SlurmHandle>> {
    let name = format!("enumeray-{}", intent.intent);
    let start = launch::accounting_start(intent.prepared_at)?;
    let mut candidates = std::collections::BTreeSet::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let remaining = || {
        deadline
            .checked_duration_since(std::time::Instant::now())
            .ok_or_else(|| failure("Slurm recovery deadline reached; preserve unresolved launch"))
    };
    // Read Slurm's actual destinations rather than interpreting the batch file.
    // Both complete searches must succeed before accepting a unique job identity.
    let mut queue = scheduler_command("squeue");
    queue.args([
        "--clusters",
        "all",
        "--all",
        "--noheader",
        "--user",
        &intent.execution_uid,
        "--name",
        &name,
        "--Format=Cluster:128|,JobID:32|,Name:64|,UserID:32",
    ]);
    let queued = run_bounded(&mut queue, None, remaining()?)?;
    collect_candidates(&queued, &name, &intent.execution_uid, &mut candidates)?;
    let mut accounting = scheduler_command("sacct");
    accounting.args([
        "--clusters",
        "all",
        "--noheader",
        "--parsable2",
        "--allocations",
        "--duplicates",
        "--starttime",
        &start,
        "--user",
        &intent.execution_uid,
        "--name",
        &name,
        "--format=Cluster%128,JobIDRaw,JobName%64,UID",
    ]);
    let recorded = run_bounded(&mut accounting, None, remaining()?)?;
    collect_candidates(&recorded, &name, &intent.execution_uid, &mut candidates)?;
    if candidates.len() == 1 {
        let (cluster, job_id) = candidates.into_iter().next().expect("one candidate");
        Ok(Some(SlurmHandle { job_id, cluster }))
    } else {
        Ok(None)
    }
}

fn collect_candidates(
    bytes: &[u8],
    name: &str,
    uid: &str,
    candidates: &mut std::collections::BTreeSet<(String, u64)>,
) -> Result<()> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| failure("invalid Slurm recovery evidence"))?;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        // Multi-cluster squeue can print grouping labels even with --noheader.
        if let Some(cluster) = line.strip_prefix("CLUSTER: ") {
            if !safe_scheduler_name(cluster.trim()) {
                return Err(failure("invalid Slurm recovery cluster label"));
            }
            continue;
        }
        let mut fields = line.split('|').map(str::trim);
        let cluster = fields.next().unwrap_or_default();
        if !safe_scheduler_name(cluster) {
            return Err(failure("invalid Slurm recovery cluster identity"));
        }
        let id = fields.next().unwrap_or_default();
        let actual_name = fields
            .next()
            .ok_or_else(|| failure("incomplete Slurm recovery evidence"))?;
        let actual_uid = fields
            .next()
            .ok_or_else(|| failure("incomplete Slurm recovery evidence"))?;
        if fields.next().is_some() {
            return Err(failure("ambiguous Slurm recovery evidence"));
        }
        if actual_name == name && actual_uid == uid {
            let id = id
                .parse::<u64>()
                .ok()
                .filter(|id| *id > 0)
                .ok_or_else(|| failure("unsupported Slurm recovery job identity"))?;
            candidates.insert((cluster.to_owned(), id));
        }
    }
    Ok(())
}
