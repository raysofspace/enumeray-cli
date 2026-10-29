//! The fixed shell is a process-group guard, never a user-programmed command.
//! Its unreaped PID reserves the group identity until cleanup is confirmed.

use std::{
    fs,
    io::Read,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use enumeray_contracts::{
    assignments::results,
    models::{self, ScientificDefinition},
};
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::{Completion, Job, MAX_RESULT_BYTES, Result, failure, read_private, write_record};

const WORKSPACE_BYTES: u64 = 1024 * 1024;
// "$@" preserves the approved argv exactly. The guard remains alive after the
// wrapper exits so group cleanup never targets a PID that may have been reused.
const GUARD: &str = r#"
umask 077
"$@"
status=$?
printf '%s\n' "$status" > .wrapper-exit
while :; do sleep 3600; done
"#;

pub(super) fn execute(workspace: &Path, job: &Job) -> Result<Completion> {
    let directory = workspace.parent().ok_or_else(|| {
        failure("missing job directory")
            .before_wrapper(crate::launch::FailureStage::WorkerPreparation)
    })?;
    let raw_policy = crate::capture::policy(directory)?;
    let policy = crate::capture::allowance(&raw_policy, &job.assignment)?;
    let stdout = crate::capture::Writer::new(
        directory,
        enumeray_contracts::logs::LogSource::Stdout,
        policy.clone(),
    )?;
    let stderr = crate::capture::Writer::new(
        directory,
        enumeray_contracts::logs::LogSource::Stderr,
        policy,
    )?;
    let slurm = job.assignment["requirements"]["driver"] == "slurm";
    if !slurm
        && super::user_cancellation(directory, job.launch_intent_id)
            .map_err(|error| error.before_wrapper(crate::launch::FailureStage::WorkerPreparation))?
    {
        return super::cancelled_completion(&job.assignment);
    }
    let mut command = Command::new("/bin/sh");
    if !slurm {
        command.env_clear();
    }
    // Slurm workers inherit the batch file's module/runtime setup and scheduler
    // variables. Submission already excludes connector credentials.
    let definition: ScientificDefinition =
        serde_json::from_value(job.assignment["definition"].clone())
            .map_err(|_| failure("invalid definition"))?;
    let collection_counts = [false, true]
        .into_iter()
        .filter_map(|out| enumeray_contracts::collections::declarations(&definition, out).ok())
        .flat_map(|d| d.into_values())
        .map(|d| d.max_members as usize)
        .sum::<usize>();
    let workspace_entries = 256usize.saturating_add(collection_counts.saturating_mul(8));
    let workspace_limit = if slurm || job.scratch.is_some() {
        None
    } else {
        let definition: ScientificDefinition =
            serde_json::from_value(job.assignment["definition"].clone()).map_err(|_| {
                failure("invalid assigned definition")
                    .before_wrapper(crate::launch::FailureStage::WorkerPreparation)
            })?;
        let inputs = models::files::declarations(&definition).map_err(|_| {
            failure("invalid input declarations")
                .before_wrapper(crate::launch::FailureStage::WorkerPreparation)
        })?;
        let outputs = models::files::output_declarations(&definition).map_err(|_| {
            failure("invalid output declarations")
                .before_wrapper(crate::launch::FailureStage::WorkerPreparation)
        })?;
        Some(
            inputs.values().chain(outputs.values()).fold(
                WORKSPACE_BYTES.saturating_add(
                    [false, true]
                        .into_iter()
                        .filter_map(|output| {
                            enumeray_contracts::collections::declarations(&definition, output).ok()
                        })
                        .flat_map(|d| d.into_values())
                        .fold(0u64, |n, d| n.saturating_add(d.max_total_bytes)),
                ),
                |total, item| total.saturating_add(item.max_bytes),
            ),
        )
    };
    // Only legacy process jobs impose the old shell file-size limit. Configured
    // jobs use workspace monitoring; Slurm preserves site-owned inherited limits.
    let guard = if slurm || job.scratch.is_some() {
        GUARD.to_owned()
    } else {
        format!("ulimit -f 10485760 || exit 125\n{GUARD}")
    };
    command
        .args(["-c", &guard, "enumeray-process-guard"])
        .args(&job.installation.argv)
        .args(["--request", "request.json", "--result", "result.json"])
        .current_dir(workspace)
        .envs(&job.installation.environment)
        .env("HOME", workspace)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if !slurm {
        command.env("PATH", "/usr/bin:/bin");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    if let Some(policy) = &job.scratch {
        let tmp = workspace.join(".enumeray/tmp");
        command
            .env("TMPDIR", &tmp)
            .env("TMP", &tmp)
            .env("TEMP", &tmp);
        if policy.cache == enumeray_contracts::execution::ProcessCache::PerRun {
            command.env("XDG_CACHE_HOME", workspace.join(".enumeray/cache"));
        }
        let before_spawn = |error: super::scratch::Violation| {
            failure(&error.message).before_wrapper(crate::launch::FailureStage::WorkerPreparation)
        };
        let mut scan = super::scratch::Scan::new(workspace, policy).map_err(before_spawn)?;
        loop {
            if super::user_cancellation(directory, job.launch_intent_id)? {
                return super::cancelled_completion(&job.assignment);
            }
            if scan.step().map_err(before_spawn)? {
                break;
            }
        }
    }
    super::validate_scratch(job)
        .map_err(|error| error.before_wrapper(crate::launch::FailureStage::WorkerValidation))?;
    super::validate_environment(&job.installation, &job.assignment)
        .map_err(|error| error.before_wrapper(crate::launch::FailureStage::WorkerValidation))?;
    let mut child = spawn_guard(&mut command)?;
    write_record(
        directory,
        "wrapper-started.json",
        &json!({"intent":job.launch_intent_id}),
    )?;
    let out = capture(child.stdout.take().expect("piped stdout"), stdout);
    let err = capture(child.stderr.take().expect("piped stderr"), stderr);
    let mut wrapper_exit = None;
    let mut termination_failure: Option<(&str, String)> = None;
    let mut monitor = job.scratch.as_ref().map(super::scratch::Monitor::new);
    let mut cancelled = false;
    let mut next_logs = Instant::now();
    loop {
        if !definition.log_files.is_empty() && Instant::now() >= next_logs {
            let _ = crate::capture::collect_named(directory, &job.assignment, &raw_policy, false);
            next_logs = Instant::now() + Duration::from_millis(100);
        }
        // A premature guard exit loses our group identity. Never signal that PID
        // after try_wait has reaped it, or pretend its descendants have stopped.
        if child
            .try_wait()
            .map_err(|_| failure("cannot observe wrapper guard"))?
            .is_some()
        {
            return Err(failure(
                "local wrapper guard exited unexpectedly; execution remains unresolved",
            ));
        }
        if job.scratch.is_some() && super::user_cancellation(directory, job.launch_intent_id)? {
            cancelled = true;
            break;
        }
        if let Some(monitor) = &mut monitor
            && let Err(error) = monitor.tick(workspace)
        {
            termination_failure = Some((error.code, error.message));
            break;
        }
        if workspace_limit
            .is_some_and(|limit| !workspace_bounded(workspace, limit, workspace_entries))
        {
            termination_failure = Some((
                "workspace_limit",
                "Local wrapper workspace exceeded its supported limits.".into(),
            ));
            break;
        }
        match exit_status(workspace) {
            Ok(Some(status)) => {
                wrapper_exit = Some(status);
                break;
            }
            Ok(None) => (),
            Err(_) => {
                termination_failure = Some((
                    "invalid_result",
                    "Local wrapper exit evidence is invalid.".into(),
                ));
                break;
            }
        }
        // Preserve the legacy ordering between completion, quota failure and cancellation.
        if !slurm
            && job.scratch.is_none()
            && super::user_cancellation(directory, job.launch_intent_id)?
        {
            cancelled = true;
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    // child was not reaped: even if the guard exits concurrently, its reserved
    // PID cannot be reassigned before this wait. Trusted wrappers must neither
    // daemonize nor move descendants out of the owned process group.
    if !terminate_group(child.id())? {
        return Err(failure(
            "local wrapper group termination was not confirmed; execution remains unresolved",
        ));
    }
    // SIGKILL being accepted is not proof of termination (for example, a child
    // may be stuck in kernel I/O). Keep the group leader unreaped while proving
    // that every member has stopped, so the process-group ID remains reserved.
    confirm_group_stopped(child.id())?;
    let guard_status = child
        .wait()
        .map_err(|_| failure("cannot confirm local wrapper guard termination"))?;
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if guard_status.signal() != Some(9) {
            return Err(failure(
                "local wrapper guard termination conflicts with cleanup evidence",
            ));
        }
    }
    let capture_deadline = Instant::now() + Duration::from_secs(3);
    while !out.is_finished() || !err.is_finished() {
        if Instant::now() >= capture_deadline {
            return Err(failure(
                "local output pipes remain open; execution remains unresolved",
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
    for stream in [out, err] {
        stream
            .join()
            .map_err(|_| failure("local output capture thread failed"))?
            .map_err(|_| failure("local output pipes could not be drained"))?;
    }
    let _ = crate::capture::collect_named(directory, &job.assignment, &raw_policy, !slurm);
    if cancelled {
        return super::cancelled_completion(&job.assignment);
    }
    if termination_failure.is_none()
        && let Some(policy) = &job.scratch
        && let Err(error) = super::scratch::check(workspace, policy)
    {
        termination_failure = Some((error.code, error.message));
    }
    if workspace_limit.is_some_and(|limit| !workspace_bounded(workspace, limit, workspace_entries))
        && termination_failure.is_none()
    {
        termination_failure = Some((
            "workspace_limit",
            "Local wrapper workspace exceeded its supported limits.".into(),
        ));
    }
    let mut report = if let Some((code, message)) = termination_failure {
        failed_report(job, code, &message)
    } else {
        result_report(workspace, job, wrapper_exit.expect("wrapper exited"))
    };
    let mut files = if report["outcome"] == "succeeded" {
        match output_files(workspace, job) {
            Ok(files) => files,
            Err(_) => {
                report = failed_report(
                    job,
                    "invalid_result",
                    "Declared output file is missing, invalid, or outside the workspace.",
                );
                Default::default()
            }
        }
    } else {
        Default::default()
    };
    let collections = if report["outcome"] == "succeeded" {
        match crate::collections::measure(workspace, &job.assignment) {
            Ok(c) => c,
            Err(_) => {
                files.clear();
                report = failed_report(
                    job,
                    "invalid_result",
                    "Declared output collection is invalid or unavailable.",
                );
                Default::default()
            }
        }
    } else {
        Default::default()
    };
    Ok(Completion {
        collections,
        files,
        scheduler_evidence: None,
        startup_evidence: None,
        observed_at: OffsetDateTime::now_utc()
            .replace_nanosecond(0)
            .map_err(|_| failure("invalid local completion time"))?
            .format(&Rfc3339)
            .map_err(|_| failure("invalid local completion time"))?,
        report: super::super::records::TerminalReport::decode(report)?,
    })
}

fn spawn_guard(command: &mut Command) -> Result<std::process::Child> {
    command.spawn().map_err(|_| {
        failure("cannot spawn the wrapper guard; scientific execution did not start")
            .before_wrapper(crate::launch::FailureStage::WorkerSpawn)
    })
}

fn terminate_group(group: u32) -> Result<bool> {
    let killed = Command::new("/bin/kill")
        .args(["-KILL", "--", &format!("-{group}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| {
            failure("cannot terminate local wrapper group; execution remains unresolved")
        })?;
    Ok(killed.success())
}

fn confirm_group_stopped(group: u32) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let output = Command::new("/bin/ps")
            .args(["-axo", "pid=,pgid=,stat="])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|_| failure("cannot inspect local process group termination"))?;
        if !output.status.success() || output.stdout.len() > 4 * 1024 * 1024 {
            return Err(failure("cannot confirm local process group termination"));
        }
        let text = std::str::from_utf8(&output.stdout)
            .map_err(|_| failure("invalid local process group observation"))?;
        let mut guard_seen = false;
        let mut live = false;
        for line in text.lines() {
            let mut columns = line.split_whitespace();
            let pid = columns.next().and_then(|value| value.parse::<u32>().ok());
            let pgid = columns.next().and_then(|value| value.parse::<u32>().ok());
            let state = columns.next();
            if pid.is_none() || pgid.is_none() || state.is_none() || columns.next().is_some() {
                return Err(failure("invalid local process group observation"));
            }
            if pgid == Some(group) {
                guard_seen |= pid == Some(group);
                live |= !state.expect("checked process state").starts_with('Z');
            }
        }
        if guard_seen && !live {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(failure(
                "local process group has not confirmed termination; execution remains unresolved",
            ));
        }
        if guard_seen && live {
            // A member can fork concurrently with the first group signal.
            // Re-signal survivors only while the unreaped guard still reserves
            // this identity; accepted signals alone never prove termination.
            // The last live member may stop between ps and kill (macOS can
            // reject signals to a zombie-only group). Observe again regardless
            // of the signal status; only a stopped snapshot permits completion.
            let _ = terminate_group(group)?;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn capture(
    mut source: impl Read + Send + 'static,
    mut target: crate::capture::Writer,
) -> thread::JoinHandle<std::io::Result<()>> {
    thread::spawn(move || {
        let mut unavailable = false;
        let mut buffer = [0; 8192];
        loop {
            let size = source.read(&mut buffer)?;
            if size == 0 {
                let _ = target.finish(unavailable);
                return Ok(());
            }
            // A capture/storage failure must never close the wrapper's pipe.
            // Retained metadata marks unmeasurable loss if storage recovers.
            if !unavailable && target.append(&buffer[..size]).is_err() {
                unavailable = true;
            }
        }
    })
}

fn exit_status(workspace: &Path) -> Result<Option<i32>> {
    let path = workspace.join(".wrapper-exit");
    if !path
        .try_exists()
        .map_err(|_| failure("cannot inspect wrapper exit status"))?
    {
        return Ok(None);
    }
    let bytes = read_private(&path, 16)?;
    // The guard can be observed between creation and its tiny write.
    if bytes.is_empty() || !bytes.ends_with(b"\n") {
        return Ok(None);
    }
    let status = std::str::from_utf8(&bytes)
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .filter(|status| (0..=255).contains(status))
        .ok_or_else(|| failure("invalid wrapper exit status"))?;
    Ok(Some(status))
}

fn workspace_bounded(path: &Path, limit: u64, max_count: usize) -> bool {
    fn visit(
        path: &Path,
        depth: u8,
        count: &mut usize,
        bytes: &mut u64,
        limit: u64,
        max_count: usize,
    ) -> std::io::Result<bool> {
        if depth > if max_count > 256 { 72 } else { 8 } {
            return Ok(false);
        }
        let entries = match fs::read_dir(path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            *count += 1;
            if *count > max_count {
                return Ok(false);
            }
            let metadata = match fs::symlink_metadata(entry.path()) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            if metadata.is_dir() {
                if !visit(&entry.path(), depth + 1, count, bytes, limit, max_count)? {
                    return Ok(false);
                }
            } else if metadata.is_file() {
                *bytes = bytes.saturating_add(metadata.len());
                if *bytes > limit {
                    return Ok(false);
                }
            } else {
                return Ok(false);
            }
        }
        Ok(true)
    }
    visit(path, 0, &mut 0, &mut 0, limit, max_count).unwrap_or(false)
}

fn result_report(workspace: &Path, job: &Job, exit: i32) -> Value {
    let invalid = || {
        failed_report(
            job,
            "invalid_result",
            "Local wrapper returned missing, malformed, or conflicting results.",
        )
    };
    let Ok(bytes) = read_private(&workspace.join("result.json"), MAX_RESULT_BYTES) else {
        return invalid();
    };
    let Ok(result) = models::parse_inputs(&bytes) else {
        return invalid();
    };
    let fields = [
        "wrapper_protocol_version",
        "run_id",
        "assignment_id",
        "attempt_id",
        "launch_intent_id",
        "outcome",
        "outputs",
        "failure",
        "files",
    ];
    let version = job.installation.installation.wrapper_protocol_version;

    if result.as_object().is_none_or(|object| {
        (object.len() != fields.len()
            && !(object.len() == fields.len() + 1 && object.contains_key("collections")))
            || fields.iter().any(|field| !object.contains_key(*field))
    }) || result["wrapper_protocol_version"] != version
        || result["launch_intent_id"] != json!(job.launch_intent_id)
        || ["run_id", "assignment_id", "attempt_id"]
            .iter()
            .any(|field| result[*field] != job.assignment[*field])
    {
        return invalid();
    }
    let mut report = json!({"attempt_id":job.assignment["attempt_id"], "execution_terminated":true,
        "outcome":result["outcome"], "outputs":result["outputs"], "failure":result["failure"],
        "files":{}, "optional_output_failures":{}});
    let Some(files) = result["files"].as_object() else {
        return invalid();
    };
    let Ok(definition) =
        serde_json::from_value::<ScientificDefinition>(job.assignment["definition"].clone())
    else {
        return invalid();
    };
    let Ok(declarations) = models::files::output_declarations(&definition) else {
        return invalid();
    };
    let Some(outputs) = result["outputs"].as_object() else {
        return invalid();
    };
    let Ok(collection_declarations) =
        enumeray_contracts::collections::declarations(&definition, true)
    else {
        return invalid();
    };
    if outputs
        .keys()
        .any(|name| declarations.contains_key(name) || collection_declarations.contains_key(name))
    {
        return invalid();
    }
    for name in files.keys() {
        let reference = json!({"artifact_id":"00000000-0000-0000-0000-000000000001"});
        report["outputs"][name] = reference.clone();
        report["files"][name] = reference;
    }
    if let Some(collections) = result.get("collections") {
        let Some(collections) = collections.as_object() else {
            return invalid();
        };
        if !collections.is_empty() {
            report["collections"] = json!({});
        }
        for name in collections.keys() {
            let r = json!({"collection_id":"00000000-0000-0000-0000-000000000001"});
            report["outputs"][name] = r.clone();
            report["collections"][name] = r;
        }
    }
    if result["outcome"] == "cancelled" {
        return invalid();
    }
    if result["failure"]["code"]
        == enumeray_contracts::assignments::resolution::RESOLUTION_FAILURE_CODE
    {
        return invalid();
    }
    if exit != 0 && result["outcome"] != "failed" {
        return failed_report(job, "wrapper_exit", "Local wrapper exited unsuccessfully.");
    }
    if result["outcome"] == "succeeded"
        && !models::compile_outputs(&definition)
            .ok()
            .is_some_and(|validator| validator.is_valid(&report["outputs"]))
    {
        return invalid();
    }
    let valid = serde_json::to_vec(&report)
        .ok()
        .is_some_and(|bytes| results::parse_request(&bytes).is_ok());
    if !valid {
        report = invalid();
    }
    report
}

fn failed_report(job: &Job, code: &str, message: &str) -> Value {
    json!({"attempt_id":job.assignment["attempt_id"], "outcome":"failed", "execution_terminated":true,
        "outputs":{}, "files":{}, "optional_output_failures":{}, "failure":{"code":code,"message":message}})
}

fn output_files(
    workspace: &Path,
    job: &Job,
) -> Result<std::collections::BTreeMap<String, crate::artifacts::OutputFile>> {
    let bytes = read_private(&workspace.join("result.json"), MAX_RESULT_BYTES)?;
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| failure("invalid wrapper result"))?;
    let declarations_by_name: std::collections::BTreeMap<
        String,
        crate::artifacts::OutputFileDeclaration,
    > = serde_json::from_value(value["files"].clone())
        .map_err(|_| failure("invalid output file manifest"))?;
    let definition: ScientificDefinition =
        serde_json::from_value(job.assignment["definition"].clone())
            .map_err(|_| failure("invalid contract"))?;
    let declarations = models::files::output_declarations(&definition)
        .map_err(|_| failure("invalid output declarations"))?;
    let mut files = std::collections::BTreeMap::new();
    for (name, file) in declarations_by_name {
        let declaration = declarations
            .get(&name)
            .ok_or_else(|| failure("undeclared output file"))?;
        if !declaration.media_types.contains(&file.media_type)
            || !file.scientific_metadata.is_object()
        {
            return Err(failure("output media type or metadata is invalid"));
        }
        let path = crate::artifacts::confined_file(workspace, &file.path)?;
        let (size, sha256) = crate::artifacts::file_digest(&path)?;
        if size > declaration.max_bytes {
            return Err(failure("output file exceeds declared maximum"));
        }
        files.insert(
            name,
            crate::artifacts::OutputFile {
                path: file.path,
                media_type: file.media_type,
                scientific_metadata: file.scientific_metadata,
                size_bytes: size,
                sha256,
            },
        );
    }
    Ok(files)
}

#[cfg(test)]
mod startup_tests {
    use super::*;
    #[test]
    fn failed_guard_spawn_carries_pre_wrapper_proof() {
        let error = spawn_guard(&mut Command::new("/enumeray/missing-guard")).unwrap_err();
        assert_eq!(
            error.startup_stage,
            Some(crate::launch::FailureStage::WorkerSpawn)
        );
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::unix::process::CommandExt;

    struct OwnedGroup(std::process::Child);

    impl Drop for OwnedGroup {
        fn drop(&mut self) {
            let _ = Command::new("/bin/kill")
                .args(["-KILL", "--", &format!("-{}", self.0.id())])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn cleanup_terminates_a_surviving_descendant_while_the_guard_is_unreaped() {
        let mut group = OwnedGroup(
            Command::new("/bin/sh")
                .args([
                    "-c",
                    "/bin/sleep 60 </dev/null >/dev/null 2>&1 & printf '%s\\n' \"$!\"; wait",
                ])
                .process_group(0)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let mut pid = String::new();
        BufReader::new(group.0.stdout.take().unwrap())
            .read_line(&mut pid)
            .unwrap();
        assert!(pid.trim().parse::<u32>().is_ok());
        // Reproduce the post-signal state directly: the guard has been killed,
        // but a member of its reserved group still needs to be terminated.
        group.0.kill().unwrap();
        confirm_group_stopped(group.0.id()).unwrap();
        // The cleanup owner signals/reaps only its still-reserved group on drop.
    }
}
