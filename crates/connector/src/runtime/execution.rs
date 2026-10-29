//! Driver dispatch with immutable launch evidence independent of the connector's
//! acknowledgement. Unknown submissions are looked up only.

use std::path::Path;

use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

use super::records::{
    EventReceipt, EventRequest, Resolution, TerminalReceipt, TerminalReport, TerminalResult,
};

use super::{
    Result,
    client::{Client, http_error, parse_json},
    failure,
    journal::{Journal, PreparedAssignment, SubmissionState},
};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Execution {
    events: Vec<Event>,
    pub(crate) report: TerminalReport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) delivery_report: Option<TerminalReport>,
    receipt: Option<TerminalReceipt>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub(crate) files: std::collections::BTreeMap<String, crate::artifacts::OutputFile>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub(crate) collections: std::collections::BTreeMap<String, crate::collections::Measured>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scheduler_evidence: Option<crate::drivers::slurm::SchedulerEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    startup_evidence: Option<crate::launch::StartupFailure>,
}

impl Execution {
    pub(crate) fn acknowledged_receipt(&self) -> Option<&TerminalReceipt> {
        self.receipt.as_ref()
    }

    pub(crate) fn published_report(&self) -> &TerminalReport {
        self.delivery_report.as_ref().unwrap_or(&self.report)
    }

    pub(crate) fn publication_ready(&self) -> bool {
        self.events.iter().all(|e| e.receipt.is_some())
    }

    pub(super) fn confirmed_prelaunch_failure(&self) -> bool {
        self.startup_evidence
            .as_ref()
            .is_some_and(|s| s.stage.before_submission())
    }

    pub(super) fn confirmed_cancellation(&self) -> bool {
        self.report["outcome"] == "cancelled" && self.report["execution_terminated"] == true
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    id: Uuid,
    request: EventRequest,
    receipt: Option<EventReceipt>,
}

impl Event {
    fn acknowledge(&mut self, assignment: &Value, receipt: EventReceipt) -> Result<()> {
        validate_event_receipt(assignment, self, &receipt)?;
        if let Some(prior) = &self.receipt {
            if !crate::numeric::equivalent(prior, &receipt) {
                return Err(failure(
                    "event acknowledgement conflicts with its durable receipt",
                ));
            }
        } else {
            self.receipt = Some(receipt);
        }
        Ok(())
    }
}

impl PreparedAssignment {
    fn record_live_event(&mut self, event: Event) -> Result<()> {
        if let Some(prior) = &self.live_event {
            if prior.id == event.id && crate::numeric::equivalent(&prior.request, &event.request) {
                if event.receipt.as_ref().is_some_and(|receipt| {
                    prior
                        .receipt
                        .as_ref()
                        .is_none_or(|saved| !crate::numeric::equivalent(saved, receipt))
                }) {
                    return Err(failure(
                        "live event acknowledgement conflicts with retained evidence",
                    ));
                }
                return Ok(());
            }
            return Err(failure(
                "live event conflicts with retained launch evidence",
            ));
        }
        if !crate::state::needs_execution(self) || self.execution.is_some() {
            return Err(failure("settled execution cannot acquire a new live event"));
        }
        self.live_event = Some(event);
        if let Err(error) = validate_history(self) {
            self.live_event = None;
            return Err(error);
        }
        Ok(())
    }

    fn record_publication_report(&mut self, report: TerminalReport) -> Result<()> {
        let execution = self
            .execution
            .as_ref()
            .ok_or_else(|| failure("publication lacks local execution evidence"))?;
        if let Some(prior) = &execution.delivery_report {
            return if crate::numeric::equivalent(prior, &report) {
                Ok(())
            } else {
                Err(failure(
                    "publication report conflicts with its durable evidence",
                ))
            };
        }
        if !crate::state::needs_execution(self)
            || !execution.publication_ready()
            || !crate::state::allocation_released(self)
        {
            return Err(failure(
                "publication report lacks settled execution observations and allocation release",
            ));
        }
        self.execution
            .as_mut()
            .expect("checked execution")
            .delivery_report = Some(report);
        if let Err(error) = validate_history(self) {
            self.execution
                .as_mut()
                .expect("checked execution")
                .delivery_report = None;
            return Err(error);
        }
        Ok(())
    }

    fn record_terminal(
        &mut self,
        result: TerminalResult,
        resolution: Option<Resolution>,
    ) -> Result<()> {
        validate_receipt(
            &self.assignment,
            &json!({"outcome":result.decoded().receipt.decoded().outcome}),
            &result["receipt"],
        )?;
        if let Some(resolution) = resolution {
            validate_resolution(&self.assignment, &result, &resolution)?;
            if resolution.decoded().execution_status
                == enumeray_contracts::assignments::resolution::ExecutionStatus::NeverStarted
                && (self.execution.is_some()
                    || self.live_event.is_some()
                    || self.submission_state() == SubmissionState::Submitted)
            {
                return Err(failure(
                    "never-started operator resolution conflicts with retained backend execution evidence",
                ));
            }
            if self
                .execution
                .as_ref()
                .is_some_and(|execution| execution.receipt.is_some())
                || (self.terminal_result.is_some() && self.operator_resolution.is_none())
            {
                return Err(failure(
                    "operator resolution conflicts with an acknowledged local terminal result; preserve both histories",
                ));
            }
            if self
                .operator_resolution
                .as_ref()
                .is_some_and(|prior| !crate::numeric::equivalent(prior, &resolution))
                || self
                    .terminal_result
                    .as_ref()
                    .is_some_and(|prior| !crate::numeric::equivalent(prior, &result))
            {
                return Err(failure(
                    "operator resolution conflicts with durable resolution evidence; preserve both histories",
                ));
            }
            if self.operator_resolution.is_none() {
                self.operator_resolution = Some(resolution);
            }
        } else {
            if self.operator_resolution.is_some() {
                return Err(failure(
                    "operator resolution is missing its audited summary; preserve both histories",
                ));
            }
            if let Some(execution) = &self.execution {
                let report = execution.published_report();
                validate_receipt(&self.assignment, report, &result["receipt"])?;
                for field in [
                    "outputs",
                    "files",
                    "collections",
                    "optional_output_failures",
                    "failure",
                ] {
                    if !crate::numeric::equivalent(&result[field], &report[field]) {
                        return Err(failure(
                            "terminal result conflicts with the durable local report",
                        ));
                    }
                }
                if execution
                    .receipt
                    .as_ref()
                    .is_some_and(|prior| !crate::numeric::equivalent(prior, &result["receipt"]))
                {
                    return Err(failure(
                        "terminal result conflicts with the durable local receipt",
                    ));
                }
            }
            if self
                .terminal_result
                .as_ref()
                .is_some_and(|prior| !crate::numeric::equivalent(prior, &result))
            {
                return Err(failure(
                    "terminal result conflicts with the durable local receipt",
                ));
            }
            // All comparisons precede mutation: a conflicting retrieved result
            // must never acknowledge a pending report even in memory.
            if let Some(execution) = &mut self.execution
                && execution.receipt.is_none()
            {
                execution.receipt = Some(
                    self.terminal_result
                        .as_ref()
                        .unwrap_or(&result)
                        .decoded()
                        .receipt
                        .clone(),
                );
            }
        }
        if self.terminal_result.is_none() {
            self.terminal_result = Some(result);
        }
        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Job {
    launch_intent_id: Uuid,
    assignment: Value,
    backend_handle: Value,
    observed_at: String,
    report: TerminalReport,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    files: std::collections::BTreeMap<String, crate::artifacts::OutputFile>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub(crate) collections: std::collections::BTreeMap<String, crate::collections::Measured>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scheduler_evidence: Option<crate::drivers::slurm::SchedulerEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    startup_evidence: Option<crate::launch::StartupFailure>,
}

pub fn validate_history(entry: &PreparedAssignment) -> Result<()> {
    if let Some(receipt) = &entry.delivery_completion {
        validate_delivery(entry, receipt)?;
    }
    if let Some(event) = &entry.live_event {
        let intent = entry
            .launch_intent_id
            .ok_or_else(|| failure("live event has no launch identity"))?;
        if entry.submission_state() == SubmissionState::DefinitelyUnsubmitted
            || event.id.is_nil()
            || event.request.decoded().sequence != 1
            || event.request.decoded().attempt_id != entry.assignment.decoded().attempt_id
            || event.request.decoded().phase != Some(enumeray_contracts::runs::RunPhase::Executing)
            || event.request["backend_handle"] != backend_handle(&entry.assignment, intent)
            || entry.execution.as_ref().is_some_and(|execution| {
                execution
                    .events
                    .first()
                    .is_none_or(|saved| saved.id != event.id || *saved.request != *event.request)
            })
        {
            return Err(failure(
                "live execution event conflicts with launch history",
            ));
        }
    }
    if let Some(resolution) = &entry.operator_resolution {
        let result = entry.terminal_result.as_ref().ok_or_else(|| {
            failure("operator resolution is missing its terminal result; preserve the journal")
        })?;
        validate_resolution(&entry.assignment, result, resolution)?;
        if resolution.decoded().execution_status
            == enumeray_contracts::assignments::resolution::ExecutionStatus::NeverStarted
            && (entry.execution.is_some()
                || entry.live_event.is_some()
                || entry.submission_state() == SubmissionState::Submitted)
        {
            return Err(failure(
                "never-started operator resolution conflicts with retained backend execution evidence",
            ));
        }
        if entry
            .execution
            .as_ref()
            .is_some_and(|execution| execution.receipt.is_some())
        {
            return Err(failure(
                "operator resolution cannot acknowledge a connector terminal report; preserve both histories",
            ));
        }
    }
    if let Some(execution) = &entry.execution {
        let invalid = || {
            failure("journal contains invalid execution evidence; preserve it for reconciliation")
        };
        let intent = entry.launch_intent_id.ok_or_else(invalid)?;
        let cancelled = execution.confirmed_cancellation();
        let scheduler = execution.scheduler_evidence.is_some();
        let startup = execution.startup_evidence.is_some();
        if let Some(evidence) = &execution.startup_evidence {
            evidence.validate_report(intent, &entry.assignment, &execution.report)?;
            if !(execution.files.is_empty() && execution.collections.is_empty())
                || (!evidence.stage.before_submission()
                    && entry.assignment["requirements"]["driver"] == "slurm"
                    && !scheduler)
                || (evidence.stage.before_submission() && (scheduler || entry.live_event.is_some()))
            {
                return Err(invalid());
            }
        }
        if let Some(evidence) = &execution.scheduler_evidence {
            if startup {
                evidence.validate_startup_report(intent, &execution.report)?;
            } else {
                evidence.validate_report(intent, &execution.report)?;
            }
            if entry.assignment["requirements"]["driver"] != "slurm"
                || !(execution.files.is_empty() && execution.collections.is_empty())
            {
                return Err(invalid());
            }
        }
        let expected_events = if cancelled || scheduler || startup {
            1 + usize::from(entry.live_event.is_some())
        } else {
            2
        };
        if entry.submission_state() != SubmissionState::Submitted
            || execution.events.len() != expected_events
            || (execution.events.len() == 2 && execution.events[0].id == execution.events[1].id)
            || execution.report.decoded().attempt_id != entry.assignment.decoded().attempt_id
            || !execution.report.decoded().execution_terminated
            || execution.report["attempt_id"] != entry.assignment["attempt_id"]
            || !matches!(
                execution.report["outcome"].as_str(),
                Some("succeeded" | "failed" | "cancelled")
            )
        {
            return Err(invalid());
        }
        crate::collections::validate_measured(
            &entry.assignment,
            &execution.report,
            execution.files.len(),
            &execution.collections,
        )?;
        for (sequence, event) in execution.events.iter().enumerate() {
            let diagnostic_event =
                (cancelled || scheduler || startup) && sequence + 1 == execution.events.len();
            let expected_phase = if diagnostic_event {
                None
            } else {
                Some(
                    [
                        enumeray_contracts::runs::RunPhase::Executing,
                        enumeray_contracts::runs::RunPhase::CollectingResults,
                    ][sequence],
                )
            };
            if event.id.is_nil()
                || event.request.decoded().attempt_id != entry.assignment.decoded().attempt_id
                || event.request.decoded().sequence != (sequence + 1) as i64
                || event.request.decoded().phase != expected_phase
                || event.request["attempt_id"] != entry.assignment["attempt_id"]
                || !event.request["observed_at"].is_string()
                || (diagnostic_event
                    && (!event.request["phase"].is_null()
                        || !event.request["backend_handle"].is_null()
                        || event.request["diagnostic"]["code"]
                            != if scheduler || startup {
                                execution.report["failure"]["code"].clone()
                            } else {
                                json!("user_cancelled")
                            }
                        || ((scheduler || startup)
                            && event.request["diagnostic"]["message"]
                                != execution.report["failure"]["message"])
                        || execution.startup_evidence.as_ref().is_some_and(|evidence| {
                            event.request["observed_at"] != evidence.observed_at
                        })))
                || (!diagnostic_event
                    && (!event.request["phase"].is_string()
                        || event.request["backend_handle"]
                            != backend_handle(&entry.assignment, intent)))
            {
                return Err(invalid());
            }
        }
        if let Some(delivery) = &execution.delivery_report {
            if (execution.files.is_empty() && execution.collections.is_empty())
                || delivery["attempt_id"] != execution.report["attempt_id"]
                || delivery["execution_terminated"] != true
            {
                return Err(invalid());
            }
            if delivery["outcome"] == "succeeded" {
                let mut expected = (*execution.report).clone();
                for name in execution.files.keys() {
                    let reference = delivery["files"][name].clone();
                    if reference["artifact_id"]
                        .as_str()
                        .and_then(|s| Uuid::parse_str(s).ok())
                        .is_none_or(|id| id.is_nil())
                    {
                        return Err(invalid());
                    }
                    expected["files"][name] = reference.clone();
                    expected["outputs"][name] = reference;
                }
                for name in execution.collections.keys() {
                    let reference = delivery["collections"][name].clone();
                    if enumeray_contracts::collections::reference(&reference).is_none() {
                        return Err(invalid());
                    }
                    expected["collections"][name] = reference.clone();
                    expected["outputs"][name] = reference;
                }
                if !crate::numeric::equivalent(&expected, delivery) {
                    return Err(invalid());
                }
            } else if delivery["outcome"] != "failed"
                || delivery["failure"]["code"] != "output_retention_expired"
                || ["outputs", "files", "optional_output_failures"]
                    .iter()
                    .any(|field| delivery[*field].as_object().is_none_or(|v| !v.is_empty()))
                || entry
                    .transfers
                    .as_ref()
                    .is_none_or(|s| s.expires_at.is_none())
            {
                return Err(invalid());
            }
        }
        if let Some(receipt) = &execution.receipt {
            validate_receipt(
                &entry.assignment,
                execution
                    .delivery_report
                    .as_ref()
                    .unwrap_or(&execution.report),
                receipt,
            )?;
            if entry
                .terminal_result
                .as_ref()
                .is_none_or(|result| result["receipt"] != **receipt)
            {
                return Err(invalid());
            }
        }
    }
    if let Some(result) = &entry.terminal_result {
        validate_receipt(
            &entry.assignment,
            &json!({"outcome":result.decoded().receipt.decoded().outcome}),
            &result["receipt"],
        )?;
    }
    Ok(())
}

fn validate_delivery(
    entry: &PreparedAssignment,
    receipt: &enumeray_contracts::assignments::delivery::DeliveryCompletionReceipt,
) -> Result<()> {
    let assignment = entry.assignment.decoded();
    if entry.terminal_result.is_none()
        || receipt.assignment_id != assignment.assignment_id
        || receipt.run_id != assignment.run_id
        || receipt.attempt_id != assignment.attempt_id
        || !matches!(
            receipt.source.as_str(),
            "connector" | "operator_abandonment"
        )
        || OffsetDateTime::parse(&receipt.completed_at, &Rfc3339).is_err()
    {
        return Err(failure(
            "delivery completion conflicts with retained assignment evidence",
        ));
    }
    Ok(())
}

pub fn reconcile_delivery(
    journal: &mut Journal,
    id: Uuid,
    receipt: enumeray_contracts::assignments::delivery::DeliveryCompletionReceipt,
) -> Result<()> {
    let entry = journal
        .data
        .prepared_assignments
        .iter_mut()
        .find(|entry| entry.assignment.decoded().assignment_id == id)
        .ok_or_else(|| failure("delivery completion has no retained assignment"))?;
    validate_delivery(entry, &receipt)?;
    if entry
        .delivery_completion
        .as_ref()
        .is_some_and(|prior| prior != &receipt)
    {
        return Err(failure(
            "delivery completion conflicts with its durable receipt",
        ));
    }
    entry.delivery_completion = Some(receipt);
    journal.save()
}

/// Only local delivery proofs authorize this acknowledgement. A retrieved
/// scientific receipt alone cannot establish whether retained archives exist.
pub fn delivery_blocker(entry: &PreparedAssignment, journal: &Path) -> Option<&'static str> {
    let policy_present = entry.launch_intent_id.is_some_and(|intent| {
        super::local_process::job_directory(journal, intent)
            .join("log-policy.json")
            .is_file()
    });
    match crate::state::delivery(entry, policy_present) {
        crate::state::DeliveryView::Blocked(reason) => Some(reason.code()),
        _ => None,
    }
}

pub fn delivery_next_action(blocker: &str) -> &'static str {
    match blocker {
        "archive_pending" => {
            "Keep the connector running or use publish-log-archives to finish retained archive delivery."
        }
        "allocation_release_unconfirmed" => {
            "Recover the original backend allocation release evidence; preserve its execution identity."
        }
        "live_logs_pending" => {
            "Restore the original journal and retained log delivery receipts, or explicitly abandon delivery with audited operator evidence."
        }
        _ => {
            "Restore the original connector journal and retained files, or explicitly abandon delivery with audited operator evidence."
        }
    }
}

pub fn complete_deliveries(client: &Client, journal: &mut Journal, session: Uuid) -> Result<()> {
    for index in 0..journal.data.prepared_assignments.len() {
        let entry = &journal.data.prepared_assignments[index];
        let policy_present = entry.launch_intent_id.is_some_and(|intent| {
            super::local_process::job_directory(&journal.directory, intent)
                .join("log-policy.json")
                .is_file()
        });
        if !matches!(
            crate::state::delivery(entry, policy_present),
            crate::state::DeliveryView::Ready
        ) {
            continue;
        }
        let Some(intent) = entry.launch_intent_id else {
            continue;
        };
        let directory = super::local_process::job_directory(&journal.directory, intent);
        if !directory
            .join("log-policy.json")
            .try_exists()
            .map_err(|_| failure("cannot inspect pinned delivery policy"))?
        {
            continue;
        }
        let policy = crate::capture::policy(&directory)?;
        if policy.archive_enabled != entry.log_archive.is_some() {
            return Err(failure(
                "archive delivery evidence conflicts with its pinned policy",
            ));
        }
        let request = enumeray_contracts::assignments::delivery::DeliveryCompletionRequest {
            attempt_id: entry.assignment.decoded().attempt_id,
            archive_expected: policy.archive_enabled,
        };
        let id = entry.assignment.decoded().assignment_id;
        let body = serde_json::to_value(request)
            .map_err(|_| failure("cannot encode delivery completion"))?;
        let (status, bytes) = client.request(
            Method::PUT,
            &format!("/connector/v1/assignments/{id}/delivery"),
            Some(session),
            None,
            Some(&body),
        )?;
        if status != StatusCode::OK {
            if status == StatusCode::CONFLICT
                && serde_json::from_slice::<Value>(&bytes)
                    .is_ok_and(|body| body["code"] == "invalid_transition")
            {
                // Expiring unused reservations can outlive the scientific
                // result. Other assignments must keep progressing meanwhile.
                continue;
            }
            return Err(http_error("delivery completion", status));
        }
        let receipt = serde_json::from_slice(&bytes)
            .map_err(|_| failure("invalid delivery completion receipt"))?;
        reconcile_delivery(journal, id, receipt)?;
    }
    Ok(())
}

pub fn execute(
    client: &Client,
    journal: &mut Journal,
    session: Uuid,
    lose_ack: bool,
    compiled_installations: Option<&Path>,
) -> Result<Value> {
    let Some(index) = journal
        .data
        .prepared_assignments
        .iter()
        .position(|entry| entry.terminal_result.is_none())
    else {
        return Ok(json!({"status": "idle", "journal_id": journal.data.journal_id}));
    };
    execute_at(
        client,
        journal,
        session,
        lose_ack,
        compiled_installations,
        index,
    )
}

pub fn execute_at(
    client: &Client,
    journal: &mut Journal,
    session: Uuid,
    lose_ack: bool,
    compiled_installations: Option<&Path>,
    index: usize,
) -> Result<Value> {
    let entry = &journal.data.prepared_assignments[index];
    let intent = entry
        .launch_intent_id
        .ok_or_else(|| failure("missing launch history; execution remains blocked"))?;
    let assignment = entry.assignment.clone();
    let id = assignment.decoded().assignment_id;

    let capture_directory = super::local_process::job_directory(&journal.directory, intent);
    super::local_process::create_private_directory(&journal.directory.join("local-jobs"))?;
    super::local_process::create_private_directory(&capture_directory)?;
    if !capture_directory.join("log-policy.json").exists() {
        super::local_process::write_record(
            &capture_directory,
            "log-policy.json",
            &client.log_policy,
        )?;
    }

    let pinned_policy = crate::capture::policy(&capture_directory)?;
    if pinned_policy.archive_enabled
        && journal.data.prepared_assignments[index]
            .log_archive
            .is_none()
    {
        journal.data.prepared_assignments[index].log_archive = Some(Default::default());
        journal.save()?;
    }
    // Optional archive metadata cannot veto scientific launch or completion.
    let _ = crate::log_archive::register(client, journal, index, session);

    let entry = &journal.data.prepared_assignments[index];
    if entry.execution.is_none() {
        let job = match crate::state::execution(entry) {
            crate::state::ExecutionView::Unsubmitted { intent } => 'prelaunch: {
                {
                    if client.assignment_control(id)?.cancel_running {
                        break 'prelaunch local_job(
                            intent,
                            &assignment,
                            crate::compute::cancel_before_launch(
                                &journal.directory,
                                intent,
                                &assignment,
                            )?,
                        );
                    }
                    client.require_launch(id)?;
                }
                let prepared = journal.data.prepared_assignments[index]
                    .transfers
                    .as_ref()
                    .and_then(|s| s.installation.clone());
                let installation = match prepared
                    .map(Ok)
                    .unwrap_or_else(|| {
                        crate::compute::prepare(
                            compiled_installations.ok_or_else(|| {
                                failure("execution requires reconciled API configuration")
                            })?,
                            &assignment,
                        )
                    })
                    .and_then(|installation| {
                        crate::compute::validate_assignment_installation(
                            &installation,
                            &assignment,
                        )?;
                        Ok(installation)
                    }) {
                    Ok(installation) => installation,
                    Err(error) => {
                        let evidence = crate::launch::StartupFailure::new(
                            intent,
                            &assignment,
                            crate::launch::FailureStage::LaunchPreparation,
                            &error.message,
                            None,
                        )?;
                        break 'prelaunch local_job(
                            intent,
                            &assignment,
                            evidence.completion(&assignment)?,
                        );
                    }
                };
                if client.service {
                    if journal.data.prepared_assignments[index].transfers.is_none() {
                        crate::transfers::prepare_staging(
                            &mut journal.data.prepared_assignments[index],
                            installation.clone(),
                            crate::artifacts::input_ids(&assignment)?.is_empty()
                                && !crate::collections::has_inputs(&assignment)?,
                        )?;
                        journal.save()?;
                    }
                    if !journal.data.prepared_assignments[index]
                        .transfers
                        .as_ref()
                        .expect("transfer")
                        .runner_ready
                    {
                        return Err(failure("input staging awaiting transfer worker")
                            .with_kind(super::ErrorKind::Waiting));
                    }
                } else {
                    crate::artifacts::stage_inputs(
                        client,
                        &journal.directory,
                        intent,
                        &assignment,
                    )?;
                }
                {
                    if client.assignment_control(id)?.cancel_running {
                        break 'prelaunch local_job(
                            intent,
                            &assignment,
                            crate::compute::cancel_before_launch(
                                &journal.directory,
                                intent,
                                &assignment,
                            )?,
                        );
                    }
                    client.require_launch(id)?;
                }
                // The intent and unknown outcome must reach stable storage BEFORE
                // crossing the backend boundary, including for this local fixture.
                let intent = journal.begin_submission(id, intent)?.launch_intent_id();
                let completion = wait_for_completion(
                    client,
                    journal,
                    index,
                    session,
                    intent,
                    &assignment,
                    if client.service {
                        crate::compute::start_prepared(&journal.directory, intent, &assignment)?
                    } else {
                        crate::compute::launch(
                            &journal.directory,
                            intent,
                            &assignment,
                            installation,
                        )?
                    },
                )?;
                let job = local_job(intent, &assignment, completion);
                if lose_ack {
                    return Err(failure(
                        "simulated lost backend submission acknowledgement; recover this session and look up the same launch identity",
                    ));
                }
                job
            }
            crate::state::ExecutionView::LookupOnly { intent } => {
                let cancel = cancellation_scope(&client.assignment_control(id)?);
                if client.service
                    && cancel.is_none()
                    && crate::compute::needs_input_staging(&journal.directory, intent)
                {
                    let installation =
                        crate::compute::retained_installation(&journal.directory, intent)?;
                    crate::transfers::recover_runner_inputs(
                        &mut journal.data.prepared_assignments[index],
                        installation,
                    )?;
                    journal.save()?;
                    return Err(failure("recovering runner input staging")
                        .with_kind(super::ErrorKind::Waiting));
                }
                let completion =
                    crate::compute::recover(&journal.directory, intent, &assignment, cancel)?;
                local_job(
                    intent,
                    &assignment,
                    wait_for_completion(
                        client,
                        journal,
                        index,
                        session,
                        intent,
                        &assignment,
                        completion,
                    )?,
                )
            }
            crate::state::ExecutionView::MissingLaunchEvidence => {
                return Err(failure("missing launch history; execution remains blocked"));
            }
            crate::state::ExecutionView::Finalized(_) => {
                unreachable!("checked finalized execution")
            }
        };
        let mut events = Vec::new();
        if job.report["outcome"] == "cancelled"
            || job.scheduler_evidence.is_some()
            || job.startup_evidence.is_some()
        {
            if let Some(event) = &journal.data.prepared_assignments[index].live_event {
                events.push(event.clone());
            }
            events.push(Event { id: Uuid::new_v4(), receipt: None,
                request: EventRequest::decode(json!({"attempt_id":assignment.decoded().attempt_id,
                    "sequence":events.len()+1,"observed_at":job.observed_at,"phase":null,"backend_handle":null,
                    "diagnostic": if job.scheduler_evidence.is_some() || job.startup_evidence.is_some() {
                        json!({"level":if job.report["outcome"] == "cancelled" { "info" } else { "error" },
                            "code":job.report["failure"]["code"], "message":job.report["failure"]["message"]})
                    } else { json!({"level":"info","code":"user_cancelled","message":"User cancellation confirmed after execution stopped."}) }}))?,
            });
        } else {
            events = ["executing", "collecting_results"].into_iter().enumerate().map(|(sequence, phase)| Ok(Event {
                id: Uuid::new_v4(),
                request: EventRequest::decode(json!({"attempt_id": assignment.decoded().attempt_id, "sequence": sequence + 1,
                    "observed_at": job.observed_at, "phase": phase, "backend_handle": job.backend_handle}))?,
                receipt: None,
            })).collect::<Result<Vec<_>>>()?;
            if let Some(event) = &journal.data.prepared_assignments[index].live_event {
                events[0] = event.clone();
            }
        }
        journal.data.prepared_assignments[index].record_execution(Execution {
            events,
            report: job.report,
            delivery_report: None,
            files: job.files,
            collections: job.collections,
            scheduler_evidence: job.scheduler_evidence,
            startup_evidence: job.startup_evidence,
            receipt: None,
        })?;
        journal.save()?;
    }

    {
        let entry = &journal.data.prepared_assignments[index];
        if entry
            .transfers
            .as_ref()
            .is_none_or(|s| s.finalized_at.is_none())
        {
            let finalized = entry
                .execution
                .as_ref()
                .expect("execution")
                .events
                .last()
                .expect("event")
                .request["observed_at"]
                .as_str()
                .and_then(|t| OffsetDateTime::parse(t, &Rfc3339).ok())
                .map(|t| t.unix_timestamp())
                .ok_or_else(|| failure("invalid finalization time"))?;
            crate::transfers::record_finalization(
                &mut journal.data.prepared_assignments[index],
                finalized,
                client.unpublished_retention_seconds,
            )?;
            journal.save()?;
        }
        let entry = &journal.data.prepared_assignments[index];
        let released = entry.transfers.as_ref().expect("state").allocation_released;
        if !released {
            let execution = entry.execution.as_ref().expect("execution");
            let proven = execution.confirmed_prelaunch_failure()
                || execution.scheduler_evidence.is_some()
                || execution.confirmed_cancellation()
                    && !crate::compute::has_start_request(&journal.directory, intent);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while !proven
                && !crate::compute::allocation_released(&journal.directory, intent, &assignment)?
            {
                if !client.service && std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    continue;
                }
                return Err(failure("awaiting compute allocation release")
                    .with_kind(super::ErrorKind::Waiting));
            }
            crate::transfers::record_allocation_release(
                &mut journal.data.prepared_assignments[index],
            )?;
            journal.save()?;
        }
    }

    if let Some(archive) = journal.data.prepared_assignments[index]
        .log_archive
        .as_mut()
        && archive.finalized_at.is_none()
    {
        let finalized = journal.data.prepared_assignments[index]
            .transfers
            .as_ref()
            .expect("state")
            .finalized_at
            .expect("finalized");
        let at = OffsetDateTime::from_unix_timestamp(finalized)
            .map_err(|_| failure("invalid archive finalization time"))?
            .format(&Rfc3339)
            .map_err(|_| failure("invalid archive finalization time"))?;
        journal.data.prepared_assignments[index]
            .log_archive
            .as_mut()
            .expect("archive")
            .finalize(at)?;
        journal.save()?;
    }
    let _ = crate::log_archive::register(client, journal, index, session);

    // Logs must reach durable ingestion before reporting the final result. A
    // failed delivery leaves the same execution and pending request for retry.
    super::logs::forward(client, journal, index, session, true)?;

    let count = journal.data.prepared_assignments[index]
        .execution
        .as_ref()
        .expect("execution")
        .events
        .len();
    for position in 0..count {
        let event = &journal.data.prepared_assignments[index]
            .execution
            .as_ref()
            .expect("execution")
            .events[position];
        if event.receipt.is_some() {
            continue;
        }
        let receipt = send_event(client, &assignment, event, session)?;
        journal.data.prepared_assignments[index]
            .execution
            .as_mut()
            .expect("execution")
            .events[position]
            .acknowledge(&assignment, receipt)?;
        journal.save()?;
    }
    if client.service {
        let entry = &journal.data.prepared_assignments[index];
        let execution = entry.execution.as_ref().expect("execution");
        let state = entry.transfers.as_ref().expect("state");
        if !(execution.files.is_empty() && execution.collections.is_empty())
            && execution.delivery_report.is_none()
        {
            let report = if state.published.len()
                == execution.files.len() + execution.collections.len()
            {
                let mut report = (*execution.report).clone();
                for (name, reference) in &state.published {
                    report["outputs"][name] = reference.clone();
                    let kind = if execution.collections.contains_key(name) {
                        "collections"
                    } else {
                        "files"
                    };
                    if kind == "collections" && report.get(kind).is_none() {
                        report[kind] = json!({});
                    }
                    report[kind][name] = reference.clone();
                }
                report
            } else if state
                .expires_at
                .is_some_and(|deadline| OffsetDateTime::now_utc().unix_timestamp() >= deadline)
            {
                json!({"attempt_id":assignment["attempt_id"],"outcome":"failed","execution_terminated":true,"outputs":{},"files":{},"optional_output_failures":{},"failure":{"code":"output_retention_expired","message":"Finalized output publication exceeded its configured retention deadline."}})
            } else {
                return Err(failure("output publication awaiting transfer worker")
                    .with_kind(super::ErrorKind::Waiting));
            };
            journal.data.prepared_assignments[index]
                .record_publication_report(TerminalReport::decode(report)?)?;
            journal.save()?;
        }
    }
    let execution = journal.data.prepared_assignments[index]
        .execution
        .as_ref()
        .expect("execution");
    if !client.service
        && !(execution.files.is_empty() && execution.collections.is_empty())
        && execution.delivery_report.is_none()
    {
        let mut updated = crate::artifacts::publish(
            client,
            &journal.directory,
            intent,
            &assignment,
            session,
            &execution.files,
            &execution.report,
        )?;
        for name in execution.collections.keys() {
            let reference = crate::collections::publish(
                client,
                &journal.directory,
                intent,
                &assignment,
                session,
                name,
            )?;
            if updated.get("collections").is_none() {
                updated["collections"] = json!({});
            }
            updated["outputs"][name] = reference.clone();
            updated["collections"][name] = reference;
        }
        journal.data.prepared_assignments[index]
            .record_publication_report(TerminalReport::decode(updated)?)?;
        journal.save()?;
    }
    let execution = journal.data.prepared_assignments[index]
        .execution
        .as_ref()
        .expect("execution");
    let report = execution
        .delivery_report
        .as_ref()
        .unwrap_or(&execution.report)
        .clone();
    let (status, body) = client.request(
        Method::PUT,
        &format!("/connector/v1/assignments/{id}/result"),
        Some(session),
        None,
        Some(&report),
    )?;
    if status != StatusCode::OK {
        return Err(http_error("terminal report", status));
    }
    let receipt = TerminalReceipt::decode(parse_json(&body)?)?;
    validate_receipt(&assignment, &report, &receipt)?;
    let result = acknowledged_result(&report, &receipt)?;
    journal.data.prepared_assignments[index].record_terminal(result, None)?;
    journal.save()?;
    Ok(
        json!({"status": "completed", "assignment_id": assignment.decoded().assignment_id, "run_id": assignment.decoded().run_id,
        "attempt_id": assignment.decoded().attempt_id, "launch_intent_id": intent, "result_receipt": receipt}),
    )
}

fn acknowledged_result(report: &Value, receipt: &TerminalReceipt) -> Result<TerminalResult> {
    let mut result = json!({"receipt":receipt,"outputs":report["outputs"],"files":report["files"],
        "optional_output_failures":report["optional_output_failures"],"failure":report["failure"]});
    if let Some(collections) = report.get("collections") {
        result["collections"] = collections.clone();
    }
    TerminalResult::decode(result)
}

pub fn reconcile_terminal(journal: &mut Journal, id: Uuid, detail: &Value) -> Result<()> {
    if detail["resolution"]["execution_status"] == "never_started" {
        let entry = journal
            .data
            .prepared_assignments
            .iter()
            .find(|entry| entry.assignment.decoded().assignment_id == id)
            .expect("reconciled assignment");
        validate_resolution(&entry.assignment, &detail["result"], &detail["resolution"])?;
        let recorded_launch = if let Some(intent) = entry.launch_intent_id {
            crate::compute::has_evidence(&journal.directory, intent)
        } else {
            false
        };
        if entry.execution.is_some()
            || entry.live_event.is_some()
            || entry.submission_state() == SubmissionState::Submitted
            || recorded_launch
        {
            return Err(failure(
                "never-started operator resolution conflicts with retained backend execution evidence",
            ));
        }
    }
    let entry = journal
        .data
        .prepared_assignments
        .iter_mut()
        .find(|entry| entry.assignment.decoded().assignment_id == id)
        .expect("reconciled assignment");
    let result = detail
        .get("result")
        .filter(|result| result.is_object())
        .ok_or_else(|| {
            failure(
                "terminal assignment is missing durable result evidence; preserve local history",
            )
        })?;
    validate_receipt(
        &entry.assignment,
        &json!({"outcome": detail["state"]}),
        &result["receipt"],
    )?;
    let resolution = detail
        .get("resolution")
        .filter(|resolution| !resolution.is_null())
        .map(|resolution| Resolution::decode(resolution.clone()))
        .transpose()?;
    entry.record_terminal(TerminalResult::decode(result.clone())?, resolution)?;
    journal.save()
}

fn validate_resolution(assignment: &Value, result: &Value, resolution: &Value) -> Result<()> {
    let invalid =
        || failure("invalid operator resolution evidence; preserve the journal for reconciliation");
    let fields = [
        "assignment_id",
        "attempt_id",
        "execution_status",
        "resolved_at",
    ];
    if resolution.as_object().is_none_or(|object| {
        object.len() != fields.len() || fields.iter().any(|field| !object.contains_key(*field))
    }) || result.as_object().is_none_or(|object| {
        object.len() != 5
            || [
                "receipt",
                "outputs",
                "files",
                "optional_output_failures",
                "failure",
            ]
            .iter()
            .any(|field| !object.contains_key(*field))
    }) || result["receipt"].as_object().is_none_or(|object| {
        object.len() != 5
            || [
                "assignment_id",
                "run_id",
                "attempt_id",
                "outcome",
                "completed_at",
            ]
            .iter()
            .any(|field| !object.contains_key(*field))
    }) || resolution["assignment_id"] != assignment["assignment_id"]
        || resolution["attempt_id"] != assignment["attempt_id"]
        || !matches!(
            resolution["execution_status"].as_str(),
            Some("never_started" | "terminated")
        )
        || resolution["resolved_at"] != result["receipt"]["completed_at"]
        || result["receipt"]["outcome"] != "failed"
        || ["outputs", "files", "optional_output_failures"]
            .iter()
            .any(|field| {
                result[field]
                    .as_object()
                    .is_none_or(|object| !object.is_empty())
            })
        || result["failure"]
            != json!({"code":"operator_resolved", "message":"An operator confirmed execution termination and resolved this assignment."})
    {
        return Err(invalid());
    }
    let timestamp = resolution["resolved_at"].as_str().ok_or_else(invalid)?;
    let parsed = OffsetDateTime::parse(timestamp, &Rfc3339).map_err(|_| invalid())?;
    if timestamp.len() > 64
        || !(1..=9999).contains(&parsed.year())
        || parsed.nanosecond() % 1000 != 0
    {
        return Err(invalid());
    }
    validate_receipt(assignment, &json!({"outcome":"failed"}), &result["receipt"])
}

fn validate_receipt(assignment: &Value, report: &Value, receipt: &Value) -> Result<()> {
    if ["assignment_id", "attempt_id", "run_id"]
        .into_iter()
        .any(|field| receipt[field] != assignment[field])
        || receipt["outcome"] != report["outcome"]
        || !matches!(
            receipt["outcome"].as_str(),
            Some("succeeded" | "failed" | "cancelled")
        )
        || !receipt["completed_at"].is_string()
    {
        return Err(failure(
            "terminal acknowledgement conflicts with its durable request",
        ));
    }
    Ok(())
}

fn wait_for_completion(
    client: &Client,
    journal: &mut Journal,
    index: usize,
    session: Uuid,
    intent: Uuid,
    assignment: &Value,
    mut completion: Option<super::local_process::Completion>,
) -> Result<super::local_process::Completion> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if completion
            .as_ref()
            .is_some_and(|done| done.startup_evidence.is_some())
        {
            return Ok(completion.expect("startup completion"));
        }
        progress(client, journal, index, session, completion.is_some())?;
        if let Some(done) = completion {
            return Ok(done);
        }
        if client.service || std::time::Instant::now() >= deadline {
            return Err(
                failure("computation awaiting completion").with_kind(super::ErrorKind::Waiting)
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        let control = client.assignment_control(
            assignment["assignment_id"]
                .as_str()
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| failure("invalid assignment identity"))?,
        )?;
        let cancel = cancellation_scope(&control);
        completion = crate::compute::recover(&journal.directory, intent, assignment, cancel)?;
    }
}

fn progress(
    client: &Client,
    journal: &mut Journal,
    index: usize,
    session: Uuid,
    complete: bool,
) -> Result<()> {
    if client.service {
        let entry = &journal.data.prepared_assignments[index];
        let intent = entry.launch_intent_id.expect("validated intent");
        let started = super::local_process::job_directory(&journal.directory, intent)
            .join("started")
            .try_exists()
            .map_err(|_| failure("cannot inspect local start evidence"))?;
        if started {
            if entry.live_event.is_none() {
                let event = Event {
                    id: Uuid::new_v4(),
                    request: EventRequest::decode(
                        json!({"attempt_id":entry.assignment.decoded().attempt_id,
                        "sequence":1, "observed_at":OffsetDateTime::now_utc().replace_nanosecond(0).expect("zero nanoseconds").format(&Rfc3339).map_err(|_| failure("cannot format observation time"))?,
                        "phase":"executing", "backend_handle":backend_handle(&entry.assignment, intent)}),
                    )?,
                    receipt: None,
                };
                journal.data.prepared_assignments[index].record_live_event(event)?;
                journal.save()?;
            }
            let entry = &journal.data.prepared_assignments[index];
            let event = entry.live_event.as_ref().expect("saved event");
            if event.receipt.is_none() {
                let receipt = send_event(client, &entry.assignment, event, session)?;
                let assignment = journal.data.prepared_assignments[index].assignment.clone();
                journal.data.prepared_assignments[index]
                    .live_event
                    .as_mut()
                    .expect("saved event")
                    .acknowledge(&assignment, receipt)?;
                journal.save()?;
            }
        }
    }
    super::logs::forward(client, journal, index, session, complete)?;
    if client.service && !complete {
        return Err(failure("local model is running").with_kind(super::ErrorKind::Waiting));
    }
    Ok(())
}

fn send_event(
    client: &Client,
    assignment: &super::records::Assignment,
    event: &Event,
    session: Uuid,
) -> Result<EventReceipt> {
    let id = assignment.decoded().assignment_id;
    let (status, body) = client.request(
        Method::PUT,
        &format!("/connector/v1/assignments/{id}/events/{}", event.id),
        Some(session),
        None,
        Some(&event.request),
    )?;
    if status != StatusCode::OK {
        return Err(http_error("execution event", status));
    }
    let receipt = EventReceipt::decode(parse_json(&body)?)?;
    validate_event_receipt(assignment, event, &receipt)?;
    Ok(receipt)
}

fn validate_event_receipt(assignment: &Value, event: &Event, receipt: &EventReceipt) -> Result<()> {
    let id = assignment["assignment_id"]
        .as_str()
        .and_then(|id| id.parse::<Uuid>().ok())
        .ok_or_else(|| failure("event acknowledgement lacks assignment identity"))?;
    if receipt["assignment_id"] != assignment["assignment_id"]
        || receipt["event_id"] != json!(event.id)
        || receipt.decoded().assignment_id != Some(id)
        || receipt.decoded().event_id != Some(event.id)
        || receipt.decoded().sequence != Some(event.request.decoded().sequence)
        || (event.request.decoded().phase.is_some()
            && receipt.decoded().phase != event.request.decoded().phase)
        || receipt.decoded().disposition
            != Some(enumeray_contracts::assignments::observations::EventDisposition::Applied)
    {
        return Err(failure(
            "execution event acknowledgement conflicts with its durable request",
        ));
    }
    Ok(())
}

fn backend_handle(assignment: &Value, intent: Uuid) -> Value {
    json!({"namespace":format!("enumeray-{}-v1", assignment["requirements"]["driver"].as_str().unwrap_or("unknown")), "job_id":intent})
}

fn local_job(
    intent: Uuid,
    assignment: &Value,
    completion: super::local_process::Completion,
) -> Job {
    Job {
        launch_intent_id: intent,
        assignment: assignment.clone(),
        backend_handle: backend_handle(assignment, intent),
        observed_at: completion.observed_at,
        report: completion.report,
        files: completion.files,
        collections: completion.collections,
        scheduler_evidence: completion.scheduler_evidence,
        startup_evidence: completion.startup_evidence,
    }
}

fn cancellation_scope(
    control: &enumeray_contracts::execution::AssignmentControl,
) -> Option<crate::drivers::CancellationScope> {
    if control.cancel_running {
        Some(crate::drivers::CancellationScope::Running)
    } else if control.cancel_pending {
        Some(crate::drivers::CancellationScope::Pending)
    } else {
        None
    }
}

#[cfg(test)]
fn scalar_report(assignment: &Value) -> Value {
    let report = super::numeric::double(&assignment["inputs"]["value"]).map(|doubled|
        json!({"attempt_id":assignment["attempt_id"], "outcome":"succeeded", "execution_terminated":true,
            "outputs":{"doubled":doubled}, "files":{}, "optional_output_failures":{}, "failure":null}));
    if let Some(report) = report {
        // The expanded output and the complete terminal envelope share a 64 KiB
        // bound. Oversize exact results become a durable failure, not an unknown
        // launch or a terminal request the API can never accept.
        if serde_json::to_vec(&report).ok().is_some_and(|bytes| {
            enumeray_contracts::assignments::results::parse_request(&bytes).is_ok()
        }) {
            return report;
        }
    }
    json!({"attempt_id":assignment["attempt_id"], "outcome":"failed", "execution_terminated":true,
            "outputs":{}, "files":{}, "optional_output_failures":{},
            "failure":{"code":"scalar_overflow", "message":"The exact doubled value exceeds the supported terminal result bounds."}})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal;

    fn retained_entry() -> PreparedAssignment {
        let data: journal::JournalData = serde_json::from_str(include_str!(
            "../../tests/fixtures/journal-minimal-evidence.json"
        ))
        .unwrap();
        let mut entry = data.prepared_assignments.into_iter().next().unwrap();
        let handle = backend_handle(&entry.assignment, entry.launch_intent_id.unwrap());
        for event in &mut entry.execution.as_mut().unwrap().events {
            let mut request = (*event.request).clone();
            request["backend_handle"] = handle.clone();
            event.request = EventRequest::decode(request).unwrap();
        }
        entry
    }

    #[test]
    fn conflicting_terminal_reconciliation_cannot_acknowledge_pending_local_execution() {
        let mut entry = retained_entry();
        entry.execution.as_mut().unwrap().receipt = None;
        // A prior server result may be retained without a local acknowledgement.
        let mut prior = (**entry.terminal_result.as_ref().unwrap()).clone();
        prior["receipt"]["completed_at"] = json!("2026-09-21T12:01:00Z");
        entry.terminal_result = Some(TerminalResult::decode(prior).unwrap());
        let before = serde_json::to_value(&entry).unwrap();
        let result = acknowledged_result(&entry.execution.as_ref().unwrap().report,
            &TerminalReceipt::decode(json!({
                "assignment_id":entry.assignment["assignment_id"], "run_id":entry.assignment["run_id"],
                "attempt_id":entry.assignment["attempt_id"], "outcome":"succeeded",
                "completed_at":"2026-09-21T12:00:00Z"
            })).unwrap()).unwrap();
        assert!(entry.record_terminal(result, None).is_err());
        assert_eq!(serde_json::to_value(&entry).unwrap(), before);
        assert!(
            entry
                .execution
                .as_ref()
                .unwrap()
                .acknowledged_receipt()
                .is_none()
        );
    }

    #[test]
    fn matching_terminal_replay_preserves_original_additive_and_exact_evidence() {
        let mut entry = retained_entry();
        let original = serde_json::to_value(&entry).unwrap();
        let result = entry.terminal_result.as_ref().unwrap().clone();
        entry.record_terminal(result, None).unwrap();
        assert_eq!(serde_json::to_value(&entry).unwrap(), original);
        let mut changed = (**entry.terminal_result.as_ref().unwrap()).clone();
        changed["outputs"]["doubled"] = json!(999);
        assert!(
            entry
                .record_terminal(TerminalResult::decode(changed).unwrap(), None)
                .is_err()
        );
        assert_eq!(serde_json::to_value(&entry).unwrap(), original);
    }

    #[test]
    fn equivalent_retrieved_receipt_uses_the_retained_original_when_acknowledging_execution() {
        let mut value = serde_json::to_value(retained_entry()).unwrap();
        value["terminal_result"]["receipt"]["future_measurement"] =
            serde_json::from_str("1.0").unwrap();
        value["execution"]["receipt"] = Value::Null;
        let mut entry: PreparedAssignment = serde_json::from_value(value).unwrap();
        let retained = (**entry.terminal_result.as_ref().unwrap()).clone();
        let mut replay = retained.clone();
        replay["receipt"]["future_measurement"] = json!(1);
        entry
            .record_terminal(TerminalResult::decode(replay).unwrap(), None)
            .unwrap();
        assert_eq!(**entry.terminal_result.as_ref().unwrap(), retained);
        assert_eq!(
            **entry
                .execution
                .as_ref()
                .unwrap()
                .acknowledged_receipt()
                .unwrap(),
            retained["receipt"]
        );
        validate_history(&entry).unwrap();
    }

    #[test]
    fn event_transition_rejects_foreign_or_conflicting_acknowledgements_without_mutation() {
        let entry = retained_entry();
        let mut event = entry.execution.as_ref().unwrap().events[0].clone();
        let before = serde_json::to_value(&event).unwrap();
        let mut receipt = (**event.receipt.as_ref().unwrap()).clone();
        receipt["event_id"] = json!(Uuid::new_v4());
        assert!(
            event
                .acknowledge(&entry.assignment, EventReceipt::decode(receipt).unwrap())
                .is_err()
        );
        assert_eq!(serde_json::to_value(&event).unwrap(), before);
        event
            .acknowledge(&entry.assignment, event.receipt.clone().unwrap())
            .unwrap();
        assert_eq!(serde_json::to_value(&event).unwrap(), before);
    }

    #[test]
    fn live_event_and_publication_conflicts_preserve_prior_evidence() {
        let mut entry = retained_entry();
        let event = entry.execution.as_ref().unwrap().events[0].clone();
        let before = serde_json::to_value(&entry).unwrap();
        assert!(entry.record_live_event(event).is_err());
        let report = entry.execution.as_ref().unwrap().report.clone();
        assert!(entry.record_publication_report(report).is_err());
        assert_eq!(serde_json::to_value(&entry).unwrap(), before);
    }

    #[test]
    fn acknowledged_results_retain_collection_cleanup_evidence_and_legacy_shape() {
        let original: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/journal-minimal-evidence.json"
        ))
        .unwrap();
        let execution = &original["prepared_assignments"][0]["execution"];
        let receipt = TerminalReceipt::decode(execution["receipt"].clone()).unwrap();
        let mut report = execution["report"].clone();
        assert!(
            acknowledged_result(&report, &receipt)
                .unwrap()
                .get("collections")
                .is_none()
        );
        report["collections"] = json!({"snapshots":{"collection_id":Uuid::new_v4()}});
        let result = acknowledged_result(&report, &receipt).unwrap();
        assert_eq!(result["collections"], report["collections"]);
    }

    #[test]
    fn retained_history_rejects_non_object_and_non_string_control_fields() {
        let original: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/journal-minimal-evidence.json"
        ))
        .unwrap();
        let entry = &original["prepared_assignments"][0];
        for (path, replacement) in [
            (
                "/prepared_assignments/0/terminal_result",
                json!([entry["terminal_result"]["receipt"]]),
            ),
            (
                "/prepared_assignments/0/execution/report",
                json!([entry["assignment"]["attempt_id"], true]),
            ),
            (
                "/prepared_assignments/0/execution/events/0/request/phase",
                json!({"executing": null}),
            ),
            (
                "/prepared_assignments/0/execution/report/outcome",
                json!({"succeeded": null}),
            ),
        ] {
            let mut document = original.clone();
            *document.pointer_mut(path).unwrap() = replacement;
            // Serde accepts these tuple/tagged forms for the typed projections;
            // original journal evidence checks must still reject them.
            let journal: journal::JournalData = serde_json::from_value(document).unwrap();
            assert!(
                validate_history(&journal.prepared_assignments[0]).is_err(),
                "{path}"
            );
        }
    }

    #[test]
    fn cancellation_before_submission_round_trips_without_inventing_execution_progress() {
        let mut document: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/journal-minimal-evidence.json"
        ))
        .unwrap();
        let entry = &mut document["prepared_assignments"][0];
        let attempt = entry["assignment"]["attempt_id"].clone();
        let mut event = entry["execution"]["events"][0].clone();
        event["receipt"] = Value::Null;
        event["request"]["phase"] = Value::Null;
        event["request"]["backend_handle"] = Value::Null;
        event["request"]["diagnostic"] =
            json!({"level":"info","code":"user_cancelled","message":"Stopped."});
        let completion =
            super::super::local_process::cancelled_completion(&json!({"attempt_id":attempt}))
                .unwrap();
        let execution: Execution = serde_json::from_value(
            json!({"events":[event],"report":completion.report,"receipt":null}),
        )
        .unwrap();
        entry["submission_state"] = json!("definitely_unsubmitted");
        entry["execution"] = Value::Null;
        entry["terminal_result"] = Value::Null;
        let mut data: journal::JournalData = serde_json::from_value(document).unwrap();
        data.prepared_assignments[0]
            .record_execution(execution)
            .unwrap();
        validate_history(&data.prepared_assignments[0]).unwrap();
        let restored: journal::JournalData =
            serde_json::from_value(serde_json::to_value(data).unwrap()).unwrap();
        validate_history(&restored.prepared_assignments[0]).unwrap();
    }

    #[test]
    fn exact_computation_is_validated_against_the_complete_terminal_budget() {
        let mut assignment = json!({"attempt_id":Uuid::new_v4(), "inputs":{"value":0}});
        for (input, expected) in [
            ("9007199254740993.1", "18014398509481986.2"),
            ("1e-1000", "2e-1000"),
        ] {
            assignment["inputs"]["value"] = serde_json::from_str(input).unwrap();
            let report = scalar_report(&assignment);
            assert_eq!(report["outcome"], "succeeded");
            assert!(super::super::numeric::equivalent(
                &report["outputs"]["doubled"],
                &serde_json::from_str(expected).unwrap()
            ));
        }
        // This input fits the input budget, but its expanded exact output plus
        // result identities and termination fields exceeds the terminal budget.
        assignment["inputs"]["value"] = serde_json::from_str("9e65400").unwrap();
        assert!(
            enumeray_contracts::models::parse_inputs(
                &serde_json::to_vec(&assignment["inputs"]).unwrap()
            )
            .is_ok()
        );
        let report = scalar_report(&assignment);
        assert_eq!(report["outcome"], "failed");
        assert_eq!(report["failure"]["code"], "scalar_overflow");
        assert!(
            enumeray_contracts::assignments::results::parse_request(
                &serde_json::to_vec(&report).unwrap()
            )
            .is_ok()
        );
    }

    #[test]
    fn startup_failure_history_requires_proof_and_diagnostic_only_events() {
        let mut document: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/journal-minimal-evidence.json"
        ))
        .unwrap();
        let entry = &mut document["prepared_assignments"][0];
        let intent: Uuid = serde_json::from_value(entry["launch_intent_id"].clone()).unwrap();
        let evidence = crate::launch::StartupFailure::new(
            intent,
            &entry["assignment"],
            crate::launch::FailureStage::LaunchPreparation,
            "Pinned executable changed before launch.",
            None,
        )
        .unwrap();
        let completion = evidence.completion(&entry["assignment"]).unwrap();
        let mut event = entry["execution"]["events"][0].clone();
        event["receipt"] = Value::Null;
        event["request"]["phase"] = Value::Null;
        event["request"]["backend_handle"] = Value::Null;
        event["request"]["observed_at"] = json!(evidence.observed_at);
        event["request"]["diagnostic"] =
            json!({"level":"error","code":evidence.stage.code(),"message":evidence.message});
        entry["terminal_result"] = Value::Null;
        entry["execution"] = json!({"events":[event],"report":completion.report,"receipt":null,"startup_evidence":evidence});
        let data: journal::JournalData = serde_json::from_value(document.clone()).unwrap();
        validate_history(&data.prepared_assignments[0]).unwrap();
        let restored: journal::JournalData =
            serde_json::from_value(serde_json::to_value(data).unwrap()).unwrap();
        validate_history(&restored.prepared_assignments[0]).unwrap();
        for (pointer, value) in [
            ("/execution/events/0/request/phase", json!("executing")),
            ("/execution/startup_evidence/intent", json!(Uuid::new_v4())),
            (
                "/execution/startup_evidence/attempt_id",
                json!(Uuid::new_v4()),
            ),
            ("/execution/startup_evidence/driver", json!("slurm")),
            ("/execution/startup_evidence/stage", json!("worker_spawn")),
            (
                "/execution/startup_evidence/observed_at",
                json!("2026-01-02T00:00:00Z"),
            ),
            (
                "/execution/report/failure/message",
                json!("Changed failure message."),
            ),
        ] {
            let mut changed = document.clone();
            *changed["prepared_assignments"][0]
                .pointer_mut(pointer)
                .unwrap() = value;
            let data: journal::JournalData = serde_json::from_value(changed).unwrap();
            assert!(
                validate_history(&data.prepared_assignments[0]).is_err(),
                "{pointer}"
            );
        }
    }

    #[test]
    fn scheduler_history_rejects_fabricated_phases_and_changed_terminal_evidence() {
        let mut document: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/journal-minimal-evidence.json"
        ))
        .unwrap();
        let entry = &mut document["prepared_assignments"][0];
        let intent = entry["launch_intent_id"].as_str().unwrap().to_owned();
        entry["assignment"]["requirements"]["driver"] = json!("slurm");
        entry["terminal_result"] = Value::Null;
        let report = super::super::local_process::failure_completion(
            &entry["assignment"],
            "slurm_failed",
            "Allocation stopped: exit 23:0.",
        )
        .unwrap()
        .report;
        let mut event = entry["execution"]["events"][0].clone();
        event["receipt"] = Value::Null;
        event["request"]["phase"] = Value::Null;
        event["request"]["backend_handle"] = Value::Null;
        event["request"]["diagnostic"] =
            json!({"level":"error", "code":"slurm_failed", "message":report["failure"]["message"]});
        entry["execution"] = json!({"events":[event],"report":report,"receipt":null,
            "scheduler_evidence":{"intent":intent,"handle":{"job_id":123,"cluster":"cluster-a"},
                "execution_uid":"42","prepared_at":1700000000,"steps":[],"cancellation_scope":null,"worker_started":false,
                "allocation":{"job_id":"123","job_name":format!("enumeray-{intent}"),"uid":"42","state":"FAILED",
                    "exit_status":23,"signal":0,"started_at":null,"ended_at":1700000001}}});
        let data: journal::JournalData = serde_json::from_value(document.clone()).unwrap();
        validate_history(&data.prepared_assignments[0]).unwrap();
        let restored: journal::JournalData =
            serde_json::from_value(serde_json::to_value(data).unwrap()).unwrap();
        validate_history(&restored.prepared_assignments[0]).unwrap();
        for (pointer, value) in [
            (
                "/execution/events/0/request/phase",
                json!("collecting_results"),
            ),
            (
                "/execution/events/0/request/diagnostic/code",
                json!("slurm_timeout"),
            ),
            (
                "/execution/scheduler_evidence/intent",
                json!(Uuid::new_v4()),
            ),
            (
                "/execution/scheduler_evidence/allocation/state",
                json!("COMPLETED"),
            ),
            (
                "/execution/scheduler_evidence/allocation/ended_at",
                Value::Null,
            ),
            ("/execution/report/outputs", json!({"doubled":6})),
            ("/execution/report/failure/message", Value::Null),
        ] {
            let mut changed = document.clone();
            *changed["prepared_assignments"][0]
                .pointer_mut(pointer)
                .unwrap() = value;
            let data: journal::JournalData = serde_json::from_value(changed).unwrap();
            assert!(
                validate_history(&data.prepared_assignments[0]).is_err(),
                "{pointer}"
            );
        }
    }
}
