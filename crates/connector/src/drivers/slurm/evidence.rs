//! Scheduler accounting is termination evidence, never scientific success evidence.
use super::{launch, scheduler_command};
use crate::connections::run_bounded;
use crate::{
    Result,
    drivers::{CancellationScope, Observation, SlurmHandle},
    failure, local_process,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AccountingRecord {
    pub job_id: String,
    pub job_name: String,
    pub uid: String,
    pub state: String,
    pub exit_status: u8,
    pub signal: u8,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct SchedulerEvidence {
    pub intent: Uuid,
    pub handle: SlurmHandle,
    pub execution_uid: String,
    pub prepared_at: i64,
    pub allocation: AccountingRecord,
    pub steps: Vec<AccountingRecord>,
    pub cancellation_scope: Option<CancellationScope>,
    pub worker_started: bool,
}

fn terminal(state: &str) -> bool {
    matches!(
        state,
        "COMPLETED"
            | "CANCELLED"
            | "FAILED"
            | "TIMEOUT"
            | "OUT_OF_MEMORY"
            | "NODE_FAIL"
            | "BOOT_FAIL"
            | "DEADLINE"
            | "PREEMPTED"
    )
}

fn normalize_state(value: &str) -> Result<String> {
    let mut fields = value.split_whitespace();
    let token = fields.next().unwrap_or_default();
    let state = token.strip_suffix('+').unwrap_or(token);
    if !matches!(
        state,
        "COMPLETED"
            | "CANCELLED"
            | "FAILED"
            | "TIMEOUT"
            | "OUT_OF_MEMORY"
            | "NODE_FAIL"
            | "BOOT_FAIL"
            | "DEADLINE"
            | "PREEMPTED"
            | "PENDING"
            | "CONFIGURING"
            | "RUNNING"
            | "COMPLETING"
            | "SUSPENDED"
            | "REQUEUED"
            | "REQUEUE_HOLD"
            | "REQUEUE_FED"
            | "RESIZING"
            | "SIGNALING"
            | "STOPPED"
            | "STAGE_OUT"
            | "UPDATE_DB"
    ) {
        return Err(failure("unknown or truncated scheduler state"));
    }
    if let Some(qualifier) = fields.next()
        && (state != "CANCELLED"
            || qualifier != "by"
            || fields.next().and_then(|v| v.parse::<u32>().ok()).is_none()
            || fields.next().is_some())
    {
        return Err(failure("invalid scheduler state qualifier"));
    }
    Ok(state.to_owned())
}

fn timestamp(value: &str) -> Result<Option<i64>> {
    if matches!(value, "Unknown" | "None") {
        return Ok(None);
    }
    let date = OffsetDateTime::parse(&format!("{value}Z"), &Rfc3339)
        .map_err(|_| failure("invalid scheduler timestamp"))?;
    if date.unix_timestamp() <= 0 {
        return Err(failure("invalid scheduler timestamp"));
    }
    Ok(Some(date.unix_timestamp()))
}

fn step_id(id: &str, allocation: &str) -> bool {
    id.strip_prefix(allocation)
        .and_then(|suffix| suffix.strip_prefix('.'))
        .is_some_and(|suffix| {
            matches!(suffix, "batch" | "extern")
                || (!suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()))
        })
}

fn records(
    bytes: &[u8],
    handle: &SlurmHandle,
    intent: &launch::LaunchIntent,
) -> Result<SchedulerEvidence> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| failure("invalid scheduler accounting encoding"))?;
    let target = handle.job_id.to_string();
    let mut rows = BTreeMap::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let fields: Vec<_> = line.split('|').map(str::trim).collect();
        if fields.len() != 7 || (fields[0] != target && !step_id(fields[0], &target)) {
            return Err(failure("accounting row conflicts with scheduler handle"));
        }
        let (status, signal) = fields[4]
            .split_once(':')
            .ok_or_else(|| failure("missing scheduler exit status"))?;
        let record = AccountingRecord {
            job_id: fields[0].into(),
            job_name: fields[1].into(),
            uid: fields[2].into(),
            state: normalize_state(fields[3])?,
            exit_status: status
                .parse()
                .map_err(|_| failure("invalid scheduler exit status"))?,
            signal: signal
                .parse()
                .map_err(|_| failure("invalid scheduler signal"))?,
            started_at: timestamp(fields[5])?,
            ended_at: timestamp(fields[6])?,
        };
        if let Some(prior) = rows.insert(record.job_id.clone(), record.clone())
            && prior != record
        {
            return Err(failure("conflicting duplicate scheduler records"));
        }
    }
    let allocation = rows
        .remove(&target)
        .ok_or_else(|| failure("scheduler allocation accounting absent"))?;
    let evidence = SchedulerEvidence {
        intent: intent.intent,
        handle: handle.clone(),
        execution_uid: intent.execution_uid.clone(),
        prepared_at: intent.prepared_at,
        allocation,
        steps: rows.into_values().collect(),
        cancellation_scope: None,
        worker_started: false,
    };
    evidence.validate_identity(intent.intent)?;
    Ok(evidence)
}

impl SchedulerEvidence {
    fn validate_identity(&self, intent: Uuid) -> Result<()> {
        let allocation = &self.allocation;
        let target = self.handle.job_id.to_string();
        if self.intent != intent
            || intent.is_nil()
            || self.handle.job_id == 0
            || !super::safe_scheduler_name(&self.handle.cluster)
            || self.prepared_at <= 0
            || self.execution_uid.is_empty()
            || !self.execution_uid.bytes().all(|b| b.is_ascii_digit())
            || allocation.job_id != target
            || allocation.job_name != format!("enumeray-{intent}")
            || allocation.uid != self.execution_uid
        {
            return Err(failure("scheduler allocation conflicts with pinned launch"));
        }
        let mut ids = std::collections::BTreeSet::new();
        for row in std::iter::once(allocation).chain(&self.steps) {
            if !ids.insert(&row.job_id)
                || (row.job_id != target && !step_id(&row.job_id, &target))
                || (!row.uid.is_empty() && row.uid != self.execution_uid)
                || normalize_state(&row.state)? != row.state
                || row
                    .started_at
                    .is_some_and(|t| t < self.prepared_at.saturating_sub(300))
                || row
                    .ended_at
                    .is_some_and(|t| t < self.prepared_at.saturating_sub(300))
                || row
                    .started_at
                    .zip(row.ended_at)
                    .is_some_and(|(start, end)| end < start)
                || (row.state == "COMPLETED" && (row.exit_status != 0 || row.signal != 0))
            {
                return Err(failure("scheduler records conflict with launch evidence"));
            }
        }
        Ok(())
    }

    fn stopped(&self) -> bool {
        std::iter::once(&self.allocation)
            .chain(&self.steps)
            .all(|row| terminal(&row.state) && row.ended_at.is_some())
    }

    pub(crate) fn outcome_code(&self) -> (bool, String) {
        if self.allocation.state == "CANCELLED" {
            if self.cancellation_scope == Some(CancellationScope::Running) {
                return (true, "user_cancelled".into());
            }
            if self.cancellation_scope == Some(CancellationScope::Pending)
                && !self.worker_started
                && self.allocation.started_at.is_none()
                && self.steps.iter().all(|step| step.started_at.is_none())
            {
                return (false, "access_revoked".into());
            }
        }
        (
            false,
            format!("slurm_{}", self.allocation.state.to_ascii_lowercase()),
        )
    }

    pub(crate) fn validate_report(&self, intent: Uuid, report: &Value) -> Result<()> {
        self.validate_identity(intent)?;
        let bytes =
            serde_json::to_vec(report).map_err(|_| failure("invalid scheduler terminal report"))?;
        enumeray_contracts::assignments::results::parse_request(&bytes)
            .map_err(|_| failure("invalid scheduler terminal report"))?;
        let (cancelled, code) = self.outcome_code();
        if !self.stopped()
            || self.allocation.state == "COMPLETED"
            || report["outcome"] != if cancelled { "cancelled" } else { "failed" }
            || report["failure"]["code"] != code
            || report["execution_terminated"] != true
            || report["outputs"] != serde_json::json!({})
            || report["files"] != serde_json::json!({})
            || report["optional_output_failures"] != serde_json::json!({})
        {
            return Err(failure("scheduler evidence conflicts with terminal report"));
        }
        Ok(())
    }

    pub(crate) fn validate_startup_report(&self, intent: Uuid, report: &Value) -> Result<()> {
        self.validate_identity(intent)?;
        let bytes =
            serde_json::to_vec(report).map_err(|_| failure("invalid startup terminal report"))?;
        enumeray_contracts::assignments::results::parse_request(&bytes)
            .map_err(|_| failure("invalid startup terminal report"))?;
        if !self.stopped()
            || report["outcome"] != "failed"
            || report["execution_terminated"] != true
        {
            return Err(failure(
                "startup failure has no stopped allocation evidence",
            ));
        }
        Ok(())
    }

    pub(crate) fn completion(self, assignment: &Value) -> Result<local_process::Completion> {
        let (cancelled, code) = self.outcome_code();
        let allocation = &self.allocation;
        let mut message = format!(
            "Slurm allocation {} on {} stopped: {} (exit {}:{}); no verified wrapper completion.",
            allocation.job_id,
            self.handle.cluster,
            allocation.state,
            allocation.exit_status,
            allocation.signal
        );
        for (index, step) in self.steps.iter().enumerate() {
            let detail = format!(
                " Step {}: {} (exit {}:{}).",
                step.job_id, step.state, step.exit_status, step.signal
            );
            if message.len() + detail.len() > 1950 {
                message.push_str(&format!(
                    " {} further step records retained.",
                    self.steps.len() - index
                ));
                break;
            }
            message.push_str(&detail);
        }
        let mut completion = local_process::failure_completion(assignment, &code, &message)?;
        if cancelled {
            let mut report = (*completion.report).clone();
            report["outcome"] = "cancelled".into();
            completion.report = crate::records::TerminalReport::decode(report)?;
        }
        self.validate_report(self.intent, &completion.report)?;
        completion.scheduler_evidence = Some(self);
        Ok(completion)
    }
}

fn queue(
    handle: &SlurmHandle,
    intent: &launch::LaunchIntent,
    timeout: Duration,
) -> Result<Option<Observation>> {
    let mut command = scheduler_command("squeue");
    // A single --jobs query fails after Slurm purges a terminal allocation from
    // the controller. Query this execution identity's queue instead, then match
    // the durable job ID. Complete bounded command output remains mandatory;
    // absence alone never proves termination without matching accounting.
    command.args([
        "--noheader",
        "--all",
        "--states=all",
        "--user",
        &intent.execution_uid,
        "--format=%i|%T",
        "--clusters",
        &handle.cluster,
    ]);
    let bytes = run_bounded(&mut command, None, timeout)?;
    let text =
        std::str::from_utf8(&bytes).map_err(|_| failure("invalid scheduler queue encoding"))?;
    let mut lines = Vec::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if let Some(cluster) = line.strip_prefix("CLUSTER: ") {
            if cluster.trim() != handle.cluster {
                return Ok(Some(Observation::Unresolved {
                    reason: "scheduler queue cluster conflicts with handle".into(),
                }));
            }
        } else if let Some((id, state)) = line.split_once('|') {
            if id.trim().is_empty() || state.trim().is_empty() {
                return Ok(Some(Observation::Unresolved {
                    reason: "scheduler queue identity or state is ambiguous".into(),
                }));
            }
            if id.trim() == handle.job_id.to_string() {
                lines.push(line);
            }
        } else {
            return Ok(Some(Observation::Unresolved {
                reason: "scheduler queue identity or state is ambiguous".into(),
            }));
        }
    }
    if lines.is_empty() {
        return Ok(None);
    }
    if lines.len() == 1
        && let Some((id, state)) = lines[0].split_once('|')
        && id.trim() == handle.job_id.to_string()
    {
        if terminal(state.trim()) {
            return Ok(None);
        }
        return Ok(Some(match state.trim() {
            "PENDING" | "CONFIGURING" => Observation::Pending,
            "RUNNING" | "COMPLETING" | "SUSPENDED" => Observation::Running,
            _ => Observation::Unresolved {
                reason: "scheduler queue state cannot establish termination".into(),
            },
        }));
    }
    Ok(Some(Observation::Unresolved {
        reason: "scheduler queue identity or state is ambiguous".into(),
    }))
}

pub(super) fn observe(handle: &SlurmHandle, intent: &launch::LaunchIntent) -> Result<Observation> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let remaining = || {
        deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| failure("scheduler observation deadline reached"))
    };
    if let Some(active) = queue(handle, intent, remaining()?)? {
        return Ok(active);
    }
    let mut command = scheduler_command("sacct");
    command.args([
        "--noheader",
        "--parsable2",
        "--duplicates",
        "--jobs",
        &handle.job_id.to_string(),
        "--starttime",
        &launch::accounting_start(intent.prepared_at)?,
        "--format=JobIDRaw%64,JobName%64,UID,State%64,ExitCode,Start,End",
        "--clusters",
        &handle.cluster,
    ]);
    let output = run_bounded(&mut command, None, remaining()?)?;
    let evidence = match records(&output, handle, intent) {
        Ok(evidence) => evidence,
        Err(error) => {
            return Ok(Observation::Unresolved {
                reason: error.message,
            });
        }
    };
    if !evidence.stopped() {
        return Ok(Observation::Unresolved {
            reason: "allocation or step termination is not confirmed".into(),
        });
    }
    if queue(handle, intent, remaining()?)?.is_some() {
        return Ok(Observation::Unresolved {
            reason: "terminal accounting conflicts with scheduler queue evidence".into(),
        });
    }
    Ok(Observation::Stopped {
        evidence: Box::new(evidence),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch() -> (SlurmHandle, launch::LaunchIntent) {
        (
            SlurmHandle {
                job_id: 123,
                cluster: "cluster-a".into(),
            },
            launch::LaunchIntent {
                intent: Uuid::new_v4(),
                script_digest: "pinned".into(),
                execution_uid: "42".into(),
                prepared_at: 1_700_000_000,
            },
        )
    }
    fn row(intent: &launch::LaunchIntent, id: &str, state: &str, exit: &str) -> String {
        let name = if id == "123" {
            format!("enumeray-{}", intent.intent)
        } else {
            "batch".into()
        };
        format!("{id}|{name}|42|{state}|{exit}|2023-11-14T22:13:20|2023-11-14T22:13:22")
    }

    #[test]
    fn retains_each_allocation_failure_and_step_exit_status() {
        let (handle, intent) = launch();
        for state in [
            "FAILED",
            "TIMEOUT",
            "OUT_OF_MEMORY",
            "NODE_FAIL",
            "BOOT_FAIL",
            "DEADLINE",
            "PREEMPTED",
            "CANCELLED",
        ] {
            let input = format!(
                "{}\n{}\n{}",
                row(&intent, "123", state, "1:9"),
                row(&intent, "123.batch", "CANCELLED by 42", "0:15"),
                row(&intent, "123.0", "COMPLETED", "0:0")
            );
            let evidence = records(input.as_bytes(), &handle, &intent).unwrap();
            assert!(evidence.stopped());
            assert_eq!(evidence.allocation.state, state);
            assert_eq!(evidence.allocation.exit_status, 1);
            assert_eq!(evidence.allocation.signal, 9);
            assert_eq!(evidence.steps.len(), 2);
            let done = evidence
                .completion(&serde_json::json!({"attempt_id":Uuid::new_v4()}))
                .unwrap();
            assert_eq!(done.report["outcome"], "failed");
            assert_eq!(
                done.report["failure"]["code"],
                format!("slurm_{}", state.to_ascii_lowercase())
            );
            assert!(
                done.report["failure"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("exit 1:9")
            );
        }
    }

    #[test]
    fn normalizes_only_complete_states_and_documented_qualifiers() {
        assert_eq!(normalize_state("OUT_OF_MEMORY+").unwrap(), "OUT_OF_MEMORY");
        assert_eq!(normalize_state("CANCELLED+ by 42").unwrap(), "CANCELLED");
        for state in [
            "OUT_OF_ME+",
            "CANCELLED by nobody",
            "FAILED by 42",
            "FAILED++",
            "F",
            "NEW_STATE",
        ] {
            assert!(normalize_state(state).is_err(), "{state}");
        }
    }

    #[test]
    fn missing_conflicting_or_malformed_records_cannot_prove_termination() {
        let (handle, intent) = launch();
        let valid = row(&intent, "123", "FAILED", "1:0");
        for input in [
            String::new(),
            row(&intent, "123.batch", "FAILED", "1:0"),
            row(&intent, "1234", "FAILED", "1:0"),
            row(&intent, "123.bad", "FAILED", "1:0"),
            row(&intent, "123", "FAILED", "256:0"),
            row(&intent, "123", "FAILED", "1:-9"),
            valid.replace("|42|", "|43|"),
            valid.replace("enumeray-", "another-"),
            format!("{valid}\n{}", row(&intent, "123", "RUNNING", "0:0")),
            valid.replace("2023-11-14T22:13:22", "bad-time"),
            valid.replace("2023-11-14T22:13:22", "2023-11-14T22:13:19"),
            row(&intent, "123", "COMPLETED", "1:0"),
        ] {
            assert!(
                records(input.as_bytes(), &handle, &intent).is_err(),
                "{input}"
            );
        }
        assert!(
            records(format!("{valid}\n{valid}").as_bytes(), &handle, &intent)
                .unwrap()
                .stopped()
        );
        assert!(records(b"\xff", &handle, &intent).is_err());
    }

    #[test]
    fn active_steps_and_absent_end_times_remain_unresolved() {
        let (handle, intent) = launch();
        for state in [
            "RUNNING",
            "COMPLETING",
            "SUSPENDED",
            "REQUEUED",
            "REQUEUE_HOLD",
        ] {
            let input = format!(
                "{}\n{}",
                row(&intent, "123", "FAILED", "1:0"),
                row(&intent, "123.0", state, "0:0")
            );
            assert!(
                !records(input.as_bytes(), &handle, &intent)
                    .unwrap()
                    .stopped()
            );
        }
        let input = row(&intent, "123", "FAILED", "1:0").replace("2023-11-14T22:13:22", "Unknown");
        assert!(
            !records(input.as_bytes(), &handle, &intent)
                .unwrap()
                .stopped()
        );
        let input = format!(
            "{}\n{}",
            row(&intent, "123", "COMPLETED", "0:0"),
            row(&intent, "123.0", "FAILED", "1:0")
        );
        let evidence = records(input.as_bytes(), &handle, &intent).unwrap();
        assert!(evidence.stopped());
        assert!(
            evidence
                .completion(&serde_json::json!({"attempt_id":Uuid::new_v4()}))
                .is_err()
        );
    }
}
