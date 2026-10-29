//! Supervised bulk I/O. Only the service owns journal.json and scientific submission.
use crate::{
    ErrorKind, Result, failure,
    journal::{Journal, PreparedAssignment, SubmissionState},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "stage", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Task {
    Inputs,
    RunnerInputs,
    Output { name: String },
    Collection { name: String },
    LogArchive,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct State {
    pub installation: Option<crate::compute::ComputeInstallation>,
    pub inputs_ready: bool,
    pub runner_ready: bool,
    pub allocation_released: bool,
    pub finalized_at: Option<i64>,
    pub expires_at: Option<i64>,
    pub task: Option<Task>,
    pub task_id: Option<Uuid>,
    pub attempts: u32,
    #[serde(default)]
    pub retries: u64,
    pub next_retry_at: i64,
    pub last_progress_at: Option<i64>,
    pub bytes: u64,
    pub total_bytes: u64,
    pub completed_units: u64,
    #[serde(default)]
    pub payload: enumeray_client::bulk::TransferProgress,
    pub diagnostic: Option<String>,
    pub published: BTreeMap<String, Value>,
}
impl State {
    /// Omit installation configuration, credential references and scientific inputs.
    pub fn summary(&self) -> Value {
        json!({"inputs_ready":self.inputs_ready,"runner_ready":self.runner_ready,"allocation_released":self.allocation_released,
            "finalized_at":self.finalized_at,"expires_at":self.expires_at,"task":self.task,"task_id":self.task_id,
            "attempts":self.attempts,"retries":self.retries,"next_retry_at":self.next_retry_at,"last_progress_at":self.last_progress_at,
            "bytes":self.bytes,"total_bytes":self.total_bytes,"completed_units":self.completed_units,"payload":self.payload,"diagnostic":self.diagnostic,
            "published":self.published})
    }
    pub fn validate(&self, entry: &PreparedAssignment) -> Result<()> {
        if self.task.is_some() != self.task_id.is_some()
            || self.task_id.is_some_and(|id| id.is_nil())
            || self.runner_ready && !self.inputs_ready
            || self.bytes > self.total_bytes
            || self.published.len() > 64
            || self.diagnostic.as_ref().is_some_and(|d| d.len() > 2048)
            || self
                .expires_at
                .is_some_and(|t| self.finalized_at.is_none_or(|f| t < f))
            || (self.finalized_at.is_some()
                || self.allocation_released
                || !self.published.is_empty())
                && entry.execution.is_none()
        {
            return Err(failure(
                "transfer history conflicts with retained assignment evidence",
            ));
        }
        if let Some(execution) = &entry.execution {
            for (name, reference) in &self.published {
                if (!execution.files.contains_key(name)
                    && !execution.collections.contains_key(name))
                    || reference[if execution.collections.contains_key(name) {
                        "collection_id"
                    } else {
                        "artifact_id"
                    }]
                    .as_str()
                    .and_then(|s| Uuid::parse_str(s).ok())
                    .is_none_or(|id| id.is_nil())
                {
                    return Err(failure(
                        "publication reference conflicts with retained outputs",
                    ));
                }
            }
        }
        Ok(())
    }
}

/// The journal records attempts, not live process ownership. A recorded active
/// attempt may be resumed after restart, using a new worker identity and the
/// existing durable payload checkpoints.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum View<'a> {
    Idle,
    Required(Task),
    Active { task: &'a Task, id: Uuid },
    Deferred { task: Task, retry_at: i64 },
}

pub(crate) fn view(entry: &PreparedAssignment, now: i64) -> View<'_> {
    let Some(state) = &entry.transfers else {
        return View::Idle;
    };
    if let (Some(task), Some(id)) = (&state.task, state.task_id) {
        return View::Active { task, id };
    }
    match required_task(entry, now) {
        Some(task) if state.next_retry_at > now => View::Deferred {
            task,
            retry_at: state.next_retry_at,
        },
        Some(task) => View::Required(task),
        None => View::Idle,
    }
}

fn ready_task(entry: &PreparedAssignment, now: i64) -> Option<Task> {
    match view(entry, now) {
        View::Required(task) => Some(task),
        // A service restart fences its prior worker and resumes the same work
        // with a fresh attempt, retaining its payload checkpoints.
        View::Active { .. } if entry.transfers.as_ref()?.next_retry_at <= now => {
            required_task(entry, now)
        }
        _ => None,
    }
}

fn required_task(entry: &PreparedAssignment, now: i64) -> Option<Task> {
    if entry.terminal_result.is_some() {
        return entry
            .log_archive
            .as_ref()
            .filter(|a| !a.finished && a.finalized_at.is_some())
            .map(|_| Task::LogArchive);
    }
    let state = entry.transfers.as_ref()?;
    if entry.execution.is_none() {
        if entry.submission_state() != SubmissionState::DefinitelyUnsubmitted && !state.inputs_ready
        {
            return None;
        }
        if !state.inputs_ready {
            Some(Task::Inputs)
        } else if !state.runner_ready {
            Some(Task::RunnerInputs)
        } else {
            None
        }
    } else if state.allocation_released
        && entry.execution.as_ref()?.publication_ready()
        && entry.execution.as_ref()?.delivery_report.is_none()
        && state.expires_at.is_none_or(|t| now < t)
    {
        entry
            .execution
            .as_ref()?
            .files
            .keys()
            .find(|name| !state.published.contains_key(*name))
            .map(|name| Task::Output { name: name.clone() })
            .or_else(|| {
                entry
                    .execution
                    .as_ref()?
                    .collections
                    .keys()
                    .find(|name| !state.published.contains_key(*name))
                    .map(|name| Task::Collection { name: name.clone() })
            })
    } else {
        None
    }
}

/// A worker result has authority only for the exact assignment, launch and
/// current attempt. This identity is deliberately not part of the v1 journal.
#[derive(Clone, Debug)]
struct Attempt {
    assignment: Uuid,
    intent: Uuid,
    id: Uuid,
    task: Task,
}
impl Attempt {
    fn matches(&self, entry: &PreparedAssignment) -> bool {
        entry.assignment.decoded().assignment_id == self.assignment
            && entry.launch_intent_id == Some(self.intent)
            && entry.transfers.as_ref().is_some_and(|state| {
                state.task_id == Some(self.id) && state.task.as_ref() == Some(&self.task)
            })
    }

    fn check(&self, entry: &PreparedAssignment) -> Result<()> {
        if !self.matches(entry) {
            return Err(failure(
                "transfer response conflicts with the current launch or attempt",
            ));
        }
        entry
            .transfers
            .as_ref()
            .expect("matching transfer")
            .validate(entry)
    }
}

fn begin_attempt(
    entry: &mut PreparedAssignment,
    task: Task,
    id: Uuid,
    now: i64,
) -> Result<Attempt> {
    let intent = entry
        .launch_intent_id
        .filter(|id| !id.is_nil())
        .ok_or_else(|| failure("transfer has no retained launch identity"))?;
    let state = entry
        .transfers
        .as_ref()
        .ok_or_else(|| failure("transfer state is unavailable"))?;
    state.validate(entry)?;
    if id.is_nil()
        || required_task(entry, now).as_ref() != Some(&task)
        || state.next_retry_at > now
        || state
            .task
            .as_ref()
            .is_some_and(|recorded| recorded != &task)
    {
        return Err(failure("transfer attempt conflicts with required work"));
    }
    let attempt = Attempt {
        assignment: entry.assignment.decoded().assignment_id,
        intent,
        id,
        task,
    };
    let state = entry.transfers.as_mut().expect("checked transfer");
    state.task = Some(attempt.task.clone());
    state.task_id = Some(id);
    state.attempts = state.attempts.saturating_add(1);
    state.bytes = 0;
    state.total_bytes = 0;
    state.completed_units = 0;
    Ok(attempt)
}

/// Cancelling a worker never removes resumable payload evidence or publication
/// receipts. The caller fences its worker before saving this transition.
pub(crate) fn interrupt(entry: &mut PreparedAssignment) -> Result<()> {
    let Some(state) = entry.transfers.as_ref() else {
        return Ok(());
    };
    state.validate(entry)?;
    let state = entry.transfers.as_mut().expect("checked transfer");
    state.task = None;
    state.task_id = None;
    state.next_retry_at = 0;
    Ok(())
}

pub(crate) fn prepare_staging(
    entry: &mut PreparedAssignment,
    installation: crate::compute::ComputeInstallation,
    inputs_ready: bool,
) -> Result<()> {
    if entry.launch_intent_id.is_none_or(|id| id.is_nil())
        || entry.execution.is_some()
        || entry.terminal_result.is_some()
    {
        return Err(failure(
            "staging conflicts with retained execution evidence",
        ));
    }
    check_installation(entry, &installation)?;
    if entry.transfers.is_none() {
        entry.transfers = Some(State {
            installation: Some(installation),
            inputs_ready,
            ..Default::default()
        });
    }
    Ok(())
}

fn check_installation(
    entry: &PreparedAssignment,
    installation: &crate::compute::ComputeInstallation,
) -> Result<()> {
    if let Some(state) = &entry.transfers {
        state.validate(entry)?;
        if let Some(recorded) = &state.installation
            && serde_json::to_value(recorded)
                .map_err(|_| failure("cannot encode pinned installation"))?
                != serde_json::to_value(installation)
                    .map_err(|_| failure("cannot encode installation"))?
        {
            return Err(failure(
                "transfer installation conflicts with its retained pin",
            ));
        }
    }
    Ok(())
}

pub(crate) fn recover_runner_inputs(
    entry: &mut PreparedAssignment,
    installation: crate::compute::ComputeInstallation,
) -> Result<()> {
    if entry.launch_intent_id.is_none_or(|id| id.is_nil())
        || entry.submission_state() == SubmissionState::DefinitelyUnsubmitted
        || entry.execution.is_some()
        || entry.terminal_result.is_some()
    {
        return Err(failure(
            "runner staging recovery has no retained submission identity",
        ));
    }
    check_installation(entry, &installation)?;
    let state = entry.transfers.get_or_insert_with(Default::default);
    state.installation = Some(installation);
    state.inputs_ready = true;
    state.runner_ready = false;
    Ok(())
}

pub(crate) fn record_finalization(
    entry: &mut PreparedAssignment,
    finalized_at: i64,
    retention_seconds: Option<u64>,
) -> Result<()> {
    if entry.execution.is_none() || entry.launch_intent_id.is_none_or(|id| id.is_nil()) {
        return Err(failure("transfer finalization has no retained execution"));
    }
    if let Some(state) = &entry.transfers {
        state.validate(entry)?;
        if let Some(recorded) = state.finalized_at {
            return if recorded == finalized_at {
                Ok(())
            } else {
                Err(failure(
                    "transfer finalization conflicts with retained evidence",
                ))
            };
        }
    }
    let state = entry.transfers.get_or_insert_with(Default::default);
    state.finalized_at = Some(finalized_at);
    state.expires_at = retention_seconds
        .map(|seconds| finalized_at.saturating_add(i64::try_from(seconds).unwrap_or(i64::MAX)));
    Ok(())
}

pub(crate) fn record_allocation_release(entry: &mut PreparedAssignment) -> Result<()> {
    let state = entry
        .transfers
        .as_ref()
        .ok_or_else(|| failure("allocation release has no transfer history"))?;
    state.validate(entry)?;
    if entry.execution.is_none() || state.finalized_at.is_none() {
        return Err(failure("allocation release has no finalized execution"));
    }
    entry
        .transfers
        .as_mut()
        .expect("checked transfer")
        .allocation_released = true;
    Ok(())
}

fn record_progress(
    entry: &mut PreparedAssignment,
    attempt: &Attempt,
    bytes: u64,
    total_bytes: u64,
    completed_units: u64,
    payload: enumeray_client::bulk::TransferProgress,
    now: i64,
) -> Result<()> {
    attempt.check(entry)?;
    let state = entry.transfers.as_mut().expect("checked transfer");
    if bytes > total_bytes || bytes < state.bytes || completed_units < state.completed_units {
        return Err(failure(
            "transfer progress conflicts with retained observations",
        ));
    }
    state.bytes = bytes;
    state.total_bytes = total_bytes;
    state.completed_units = completed_units;
    state.payload = payload;
    state.last_progress_at = Some(now);
    Ok(())
}

fn complete_attempt(
    entry: &mut PreparedAssignment,
    attempt: &Attempt,
    verified_at: i64,
    reference: Option<Value>,
    now: i64,
) -> Result<()> {
    attempt.check(entry)?;
    let mut archive_status = None;
    match &attempt.task {
        Task::Inputs | Task::RunnerInputs => {
            if entry.execution.is_some()
                || entry.terminal_result.is_some()
                || reference.is_some()
                || matches!(attempt.task, Task::RunnerInputs)
                    && !entry
                        .transfers
                        .as_ref()
                        .expect("checked transfer")
                        .inputs_ready
            {
                return Err(failure(
                    "input completion conflicts with staging prerequisites",
                ));
            }
        }
        Task::LogArchive => {
            let status: enumeray_contracts::logs::LogArchiveStatus = serde_json::from_value(
                reference
                    .clone()
                    .ok_or_else(|| failure("archive worker omitted status"))?,
            )
            .map_err(|_| failure("invalid archive worker status"))?;
            crate::log_archive::validate_completion(entry, &status)?;
            archive_status = Some(status);
        }
        Task::Output { name } | Task::Collection { name } => {
            if verified_at > now.saturating_add(5) {
                return Err(failure("invalid publication time"));
            }
            let state = entry.transfers.as_ref().expect("checked transfer");
            if state.expires_at.is_some_and(|t| verified_at >= t) {
                return defer_attempt(
                    entry,
                    attempt,
                    "output_retention_expired",
                    "Publication verification finished after its retention deadline.",
                );
            }
            let reference = reference
                .as_ref()
                .ok_or_else(|| failure("transfer omitted publication reference"))?;
            let execution = entry
                .execution
                .as_ref()
                .ok_or_else(|| failure("publication has no finalized execution"))?;
            let collection = matches!(attempt.task, Task::Collection { .. });
            if !state.allocation_released
                || entry.terminal_result.is_some()
                || !execution.publication_ready()
                || execution.delivery_report.is_some()
                || if collection {
                    !execution.collections.contains_key(name)
                } else {
                    !execution.files.contains_key(name)
                }
                || reference.as_object().is_none_or(|v| v.len() != 1)
                || reference[if collection {
                    "collection_id"
                } else {
                    "artifact_id"
                }]
                .as_str()
                .and_then(|s| Uuid::parse_str(s).ok())
                .is_none_or(|id| id.is_nil())
                || state
                    .published
                    .get(name)
                    .is_some_and(|recorded| recorded != reference)
            {
                return Err(failure(
                    "transfer returned conflicting publication evidence",
                ));
            }
        }
    }
    match &attempt.task {
        Task::Inputs => {
            entry
                .transfers
                .as_mut()
                .expect("checked transfer")
                .inputs_ready = true
        }
        Task::RunnerInputs => {
            entry
                .transfers
                .as_mut()
                .expect("checked transfer")
                .runner_ready = true
        }
        Task::LogArchive => {
            crate::log_archive::complete(entry, archive_status.expect("checked status"))?;
        }
        Task::Output { name } | Task::Collection { name } => {
            entry
                .transfers
                .as_mut()
                .expect("checked transfer")
                .published
                .entry(name.clone())
                .or_insert_with(|| reference.expect("checked publication"));
        }
    }
    let state = entry.transfers.as_mut().expect("checked transfer");
    state.task = None;
    state.task_id = None;
    state.diagnostic = None;
    state.next_retry_at = 0;
    state.attempts = 0;
    Ok(())
}

fn defer_attempt(
    entry: &mut PreparedAssignment,
    attempt: &Attempt,
    code: &str,
    message: &str,
) -> Result<()> {
    attempt.check(entry)?;
    defer(
        entry.transfers.as_mut().expect("checked transfer"),
        code,
        message,
    );
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: Uuid,
    intent: Uuid,
    task: Task,
    journal: PathBuf,
    assignment: Value,
    installation: Option<crate::compute::ComputeInstallation>,
    files: BTreeMap<String, crate::artifacts::OutputFile>,
    session: Uuid,
    origin: String,
    token_file: PathBuf,
    allow_http: bool,
    archive_finalized_at: Option<String>,
    #[serde(default = "default_spool_limit")]
    spool_limit_bytes: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Message {
    Progress {
        id: Uuid,
        bytes: u64,
        total_bytes: u64,
        completed_units: u64,
        #[serde(default)]
        payload: enumeray_client::bulk::TransferProgress,
    },
    Done {
        id: Uuid,
        verified_at: i64,
        reference: Option<Value>,
    },
    Failed {
        id: Uuid,
        retry: bool,
        fatal: bool,
        message: String,
    },
}
struct Active {
    attempt: Attempt,
    child: Child,
    _input: ChildStdin,
    messages: mpsc::Receiver<Message>,
    reader: Option<thread::JoinHandle<()>>,
    started: Instant,
    progressed: Instant,
    bytes: u64,
    units: u64,
}
impl Drop for Active {
    fn drop(&mut self) {
        kill_group(self.child.id());
        let _ = self.child.wait();
        // Dropping the receiver also unblocks a reader waiting on a full queue.
        let (_send, empty) = mpsc::sync_channel(1);
        let old = std::mem::replace(&mut self.messages, empty);
        drop(old);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
fn kill_group(id: u32) {
    #[cfg(unix)]
    {
        let _ = Command::new("/bin/kill")
            .args(["-KILL", "--", &format!("-{id}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn default_spool_limit() -> u64 {
    1099511627776
}

pub(crate) struct Pool {
    active: Vec<Active>,
    token_file: PathBuf,
    limit: usize,
    spool_limit_bytes: u64,
    cursor: usize,
    budgets: enumeray_contracts::transfers::TransferBudgets,
}
impl Pool {
    pub fn new(token_file: PathBuf, limit: usize) -> Self {
        Self {
            active: Vec::new(),
            token_file,
            limit,
            spool_limit_bytes: default_spool_limit(),
            cursor: 0,
            budgets: Default::default(),
        }
    }
    pub fn with_budgets(mut self, budgets: enumeray_contracts::transfers::TransferBudgets) -> Self {
        self.budgets = budgets;
        self
    }
    pub fn with_spool_limit(mut self, limit: u64) -> Self {
        self.spool_limit_bytes = limit;
        self
    }
    pub fn stop_all(&mut self) {
        self.active.clear();
    }
    pub fn is_active(&self, id: Uuid) -> bool {
        self.active.iter().any(|a| a.attempt.assignment == id)
    }
    pub fn cancel(&mut self, id: Uuid) {
        self.active.retain(|task| task.attempt.assignment != id);
    }
    fn prepare_attempt(
        &self,
        entry: &mut PreparedAssignment,
        task: Task,
        id: Uuid,
        now: i64,
    ) -> Result<Attempt> {
        if self.is_active(entry.assignment.decoded().assignment_id)
            || entry.launch_intent_id.is_none_or(|intent| intent.is_nil())
            || id.is_nil()
            || ready_task(entry, now).as_ref() != Some(&task)
        {
            return Err(failure("transfer replacement has no scheduling authority"));
        }
        let state = entry
            .transfers
            .as_ref()
            .ok_or_else(|| failure("transfer state is unavailable"))?;
        state.validate(entry)?;
        if state
            .task
            .as_ref()
            .is_some_and(|recorded| recorded != &task)
        {
            // Inventory reconciliation can settle science while a stopped
            // process still has an output attempt recorded in its journal.
            // This pool has no live worker for it, so fence that orphan before
            // scheduling the now-required archive, retaining all checkpoints.
            interrupt(entry)?;
        }
        begin_attempt(entry, task, id, now)
    }
    pub fn tick(&mut self, journal: &mut Journal) -> Result<()> {
        let mut remove = Vec::new();
        let mut changed = false;
        for (position, active) in self.active.iter_mut().enumerate() {
            let Some(entry) = journal.data.prepared_assignments.iter_mut().find(|e| {
                e.assignment.decoded().assignment_id == active.attempt.assignment
                    && (e.terminal_result.is_none()
                        || e.log_archive.as_ref().is_some_and(|a| !a.finished))
            }) else {
                remove.push(position);
                continue;
            };
            // Ignore queued messages from workers fenced by replacement,
            // cancellation or a different launch of the same assignment.
            if !active.attempt.matches(entry) {
                remove.push(position);
                continue;
            }
            let mut finished = false;
            let mut disconnected = false;
            loop {
                let message = match active.messages.try_recv() {
                    Ok(message) => message,
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                };
                match message {
                    Message::Progress {
                        id,
                        bytes,
                        total_bytes,
                        completed_units,
                        payload,
                    } if id == active.attempt.id => {
                        record_progress(
                            entry,
                            &active.attempt,
                            bytes,
                            total_bytes,
                            completed_units,
                            payload,
                            OffsetDateTime::now_utc().unix_timestamp(),
                        )?;
                        if bytes > active.bytes || completed_units > active.units {
                            active.progressed = Instant::now();
                            active.bytes = bytes;
                            active.units = completed_units;
                        }
                        changed = true;
                        eprintln!(
                            "{}",
                            json!({"status":"transfer_progress","assignment_id":active.attempt.assignment,"task_id":id,"task":active.attempt.task,"bytes":bytes,"total_bytes":total_bytes,"completed_units":completed_units,"payload":payload})
                        );
                    }
                    Message::Done {
                        id,
                        verified_at,
                        reference,
                    } if id == active.attempt.id => {
                        complete_attempt(
                            entry,
                            &active.attempt,
                            verified_at,
                            reference,
                            OffsetDateTime::now_utc().unix_timestamp(),
                        )?;
                        changed = true;
                        finished = true;
                    }
                    Message::Failed {
                        id,
                        retry,
                        fatal,
                        message,
                    } if id == active.attempt.id => {
                        if fatal {
                            return Err(failure(message).with_kind(ErrorKind::Fatal));
                        }
                        defer_attempt(
                            entry,
                            &active.attempt,
                            if message.contains("reservation expired") {
                                "output_reservation_expired"
                            } else if message.contains("capability expired") {
                                "transfer_capability_expired"
                            } else if retry {
                                "transfer_interrupted"
                            } else {
                                "transfer_blocked"
                            },
                            &message,
                        )?;
                        changed = true;
                        finished = true;
                    }
                    _ => return Err(failure("invalid transfer worker response")),
                }
                if finished {
                    break;
                }
            }
            if !finished
                && (active.started.elapsed() >= self.budgets.overall
                    || active.progressed.elapsed() >= self.budgets.stall)
            {
                defer_attempt(
                    entry,
                    &active.attempt,
                    if active.started.elapsed() >= self.budgets.overall {
                        "transfer_attempt_expired"
                    } else {
                        "transfer_stalled"
                    },
                    "Bulk I/O made no progress or exceeded the attempt deadline.",
                )?;
                changed = true;
                finished = true;
            }
            if !finished && disconnected {
                // Keep the child unreaped until Drop fences its process group,
                // so the PID cannot be reused before signalling descendants.
                defer_attempt(
                    entry,
                    &active.attempt,
                    "transfer_interrupted",
                    "Transfer worker stopped before confirming completion.",
                )?;
                changed = true;
                finished = true;
            }
            if finished {
                remove.push(position);
            }
        }
        for position in remove.into_iter().rev() {
            self.active.remove(position);
        }
        if changed {
            journal.save()?;
        }
        Ok(())
    }
    pub fn schedule(&mut self, journal: &mut Journal, session: Uuid) -> Result<()> {
        let count = journal.data.prepared_assignments.len();
        // Optional work yields its slot when required transfers arrive. All
        // publication requests/bytes survive interruption and can be replayed.
        let required = journal.data.prepared_assignments.iter().any(|entry| {
            !self.is_active(entry.assignment.decoded().assignment_id)
                && ready_task(entry, OffsetDateTime::now_utc().unix_timestamp())
                    .is_some_and(|task| !matches!(task, Task::LogArchive))
        });
        if required && self.active.len() == self.limit {
            let archive = self.active.iter().position(|active| {
                journal.data.prepared_assignments.iter().any(|entry| {
                    active.attempt.matches(entry) && matches!(active.attempt.task, Task::LogArchive)
                })
            });
            if let Some(position) = archive {
                let active = self.active.remove(position);
                let assignment = active.attempt.assignment;
                drop(active);
                let entry = journal
                    .data
                    .prepared_assignments
                    .iter_mut()
                    .find(|entry| entry.assignment.decoded().assignment_id == assignment)
                    .expect("active assignment");
                interrupt(entry)?;
                journal.save()?;
            }
        }
        for pass in 0..2 {
            for _ in 0..count {
                if self.active.len() >= self.limit {
                    break;
                }
                let index = self.cursor % count;
                self.cursor = self.cursor.wrapping_add(1);
                let entry = &journal.data.prepared_assignments[index];
                let assignment_id = entry.assignment.decoded().assignment_id;
                if self.is_active(assignment_id) {
                    continue;
                }
                let Some(task) = ready_task(entry, OffsetDateTime::now_utc().unix_timestamp())
                else {
                    continue;
                };
                if matches!(task, Task::LogArchive) != (pass == 1) {
                    continue;
                }
                let state = entry.transfers.as_ref().expect("transfer");
                let id = Uuid::new_v4();
                let files = match &task {
                    Task::Output { name } => BTreeMap::from([(
                        name.clone(),
                        entry.execution.as_ref().expect("execution").files[name].clone(),
                    )]),
                    _ => BTreeMap::new(),
                };
                let request = Request {
                    id,
                    intent: entry
                        .launch_intent_id
                        .ok_or_else(|| failure("transfer has no retained launch identity"))?,
                    task: task.clone(),
                    journal: journal.directory.clone(),
                    assignment: (*entry.assignment).clone(),
                    installation: state.installation.clone(),
                    files,
                    session,
                    origin: journal.data.api_base_url.clone(),
                    token_file: self.token_file.clone(),
                    allow_http: journal.data.allow_insecure_http,
                    spool_limit_bytes: self.spool_limit_bytes,
                    archive_finalized_at: entry
                        .log_archive
                        .as_ref()
                        .and_then(|a| a.finalized_at.clone()),
                };
                let attempt = self.prepare_attempt(
                    &mut journal.data.prepared_assignments[index],
                    task,
                    id,
                    OffsetDateTime::now_utc().unix_timestamp(),
                )?;
                journal.save()?;
                match spawn(request, attempt.clone()) {
                    Ok(active) => self.active.push(active),
                    Err(error) => {
                        defer_attempt(
                            &mut journal.data.prepared_assignments[index],
                            &attempt,
                            "transfer_spawn_failed",
                            &error.message,
                        )?;
                        journal.save()?;
                    }
                }
            }
        }
        Ok(())
    }
}
fn defer(state: &mut State, code: &str, message: &str) {
    state.retries = state.retries.saturating_add(1);
    let delay = (1_i64 << state.attempts.saturating_sub(1).min(5)).min(30);
    state.next_retry_at = OffsetDateTime::now_utc().unix_timestamp() + delay;
    state.diagnostic = Some(format!("{code}: {message}").chars().take(1024).collect());
    state.task = None;
    state.task_id = None;
    eprintln!(
        "{}",
        json!({"status":code,"attempts":state.attempts,"next_retry_at":state.next_retry_at,"message":state.diagnostic})
    );
}
fn spawn(request: Request, attempt: Attempt) -> Result<Active> {
    let mut command = Command::new(
        std::env::current_exe().map_err(|_| failure("cannot locate transfer binary"))?,
    );
    command
        .arg("transfer-worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|_| failure("cannot spawn transfer worker"))?;
    let input = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let (send, messages) = mpsc::sync_channel(8);
    let reader = thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut bytes = Vec::new();
            if reader
                .by_ref()
                .take(128 * 1024 + 1)
                .read_until(b'\n', &mut bytes)
                .is_err()
                || bytes.is_empty()
                || bytes.len() > 128 * 1024
            {
                break;
            }
            let Ok(message) = serde_json::from_slice(&bytes) else {
                break;
            };
            if send.send(message).is_err() {
                break;
            }
        }
    });
    let active = Active {
        attempt,
        child,
        _input: input,
        messages,
        reader: Some(reader),
        started: Instant::now(),
        progressed: Instant::now(),
        bytes: 0,
        units: 0,
    };
    let mut active = active;
    serde_json::to_writer(&mut active._input, &request)
        .map_err(|_| failure("cannot encode transfer task"))?;
    active
        ._input
        .write_all(b"\n")
        .map_err(|_| failure("cannot send transfer task"))?;
    Ok(active)
}

// Progress exists only in the bulk worker; ordinary runner/supervisor hashing stays silent.
struct Progress {
    id: Uuid,
    bytes: u64,
    total: u64,
    units: u64,
    last: Instant,
    payload: enumeray_client::bulk::TransferProgress,
}
static PROGRESS: std::sync::Mutex<Option<Progress>> = std::sync::Mutex::new(None);
pub(crate) fn begin_file(size: u64) {
    if let Some(p) = PROGRESS.lock().expect("progress").as_mut() {
        p.total = p.total.saturating_add(size);
        p.payload = enumeray_client::bulk::TransferProgress {
            total_bytes: size,
            ..Default::default()
        };
    }
}
pub(crate) fn payload_progress(progress: enumeray_client::bulk::TransferProgress) {
    if let Some(p) = PROGRESS.lock().expect("progress").as_mut() {
        p.bytes = p.bytes.saturating_add(
            progress
                .transferred_bytes
                .saturating_sub(p.payload.transferred_bytes),
        );
        p.payload = progress;
        if p.last.elapsed() >= Duration::from_secs(1) {
            emit_progress(p);
        }
    }
}
/// A storage stream already reports processed I/O through its reader. Publish
/// payload evidence without counting those same bytes a second time.
pub(crate) fn payload_observation(progress: enumeray_client::bulk::TransferProgress) {
    if let Some(p) = PROGRESS.lock().expect("progress").as_mut() {
        p.payload = progress;
        if p.last.elapsed() >= Duration::from_secs(1) {
            emit_progress(p);
        }
    }
}
pub(crate) fn verified_payload(size: u64) {
    if let Some(p) = PROGRESS.lock().expect("progress").as_mut() {
        if p.payload
            .resumed_bytes
            .checked_add(p.payload.transferred_bytes)
            != Some(size)
        {
            // Publication/verification can resume with no new storage write.
            p.payload.resumed_bytes = size;
            p.payload.transferred_bytes = 0;
        }
        p.payload.total_bytes = size;
        p.payload.verified_bytes = size;
        emit_progress(p);
    }
}
pub(crate) fn advance(bytes: u64) {
    if let Some(p) = PROGRESS.lock().expect("progress").as_mut() {
        p.bytes = p.bytes.saturating_add(bytes);
        if p.last.elapsed() >= Duration::from_secs(1) {
            emit_progress(p);
        }
    }
}
pub(crate) fn end_file() {
    if let Some(p) = PROGRESS.lock().expect("progress").as_mut() {
        p.units += 1;
        emit_progress(p);
    }
}
fn emit_progress(p: &mut Progress) {
    emit(&Message::Progress {
        id: p.id,
        bytes: p.bytes,
        total_bytes: p.total.max(p.bytes),
        completed_units: p.units,
        payload: p.payload,
    });
    p.last = Instant::now();
}
fn emit(message: &Message) {
    let mut out = std::io::stdout().lock();
    let _ = serde_json::to_writer(&mut out, message);
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

pub(crate) fn worker() -> Result<Value> {
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(4 * 1024 * 1024 + 1)
        .read_until(b'\n', &mut bytes)
        .map_err(|_| failure("cannot read transfer request"))?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(failure("transfer request exceeds bound"));
    }
    let request: Request =
        serde_json::from_slice(&bytes).map_err(|_| failure("invalid transfer request"))?;
    thread::spawn(move || {
        let mut byte = [0];
        let _ = reader.read(&mut byte);
        kill_group(std::process::id());
        std::process::exit(1);
    });
    *PROGRESS.lock().expect("progress") = Some(Progress {
        id: request.id,
        bytes: 0,
        total: 0,
        units: 0,
        last: Instant::now(),
        payload: Default::default(),
    });
    let outcome = (|| -> Result<Option<Value>> {
        let locks = request.journal.join("bulk-locks");
        crate::local_process::create_private_directory(&locks)?;
        let lock_path = locks.join(format!("{}.lock", request.intent));
        if lock_path.exists() {
            crate::journal::check_private_path(&lock_path, false)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options
            .open(&lock_path)
            .map_err(|_| failure("cannot open bulk ownership lock"))?;
        lock.try_lock().map_err(|_| {
            failure("prior transfer worker still owns this launch").with_kind(ErrorKind::Retry)
        })?;
        let client = crate::client::Client::configured(
            &request.origin,
            Some(&request.token_file),
            request.allow_http,
        )?;
        crate::spool::set_limit(request.spool_limit_bytes);
        match request.task {
            Task::Inputs => {
                crate::artifacts::stage_inputs(
                    &client,
                    &request.journal,
                    request.intent,
                    &request.assignment,
                )?;
                Ok(None)
            }
            Task::RunnerInputs => {
                crate::compute::stage_runner(
                    &request.journal,
                    request.intent,
                    &request.assignment,
                    request
                        .installation
                        .ok_or_else(|| failure("missing pinned installation"))?,
                )?;
                Ok(None)
            }
            Task::LogArchive => crate::log_archive::publish(
                &client,
                &request.journal,
                request.intent,
                &request.assignment,
                request.session,
                request
                    .archive_finalized_at
                    .as_deref()
                    .ok_or_else(|| failure("archive task has no finalization time"))?,
            )
            .map(Some),
            Task::Collection { name } => crate::collections::publish(
                &client,
                &request.journal,
                request.intent,
                &request.assignment,
                request.session,
                &name,
            )
            .map(Some),
            Task::Output { name } => crate::artifacts::publish(
                &client,
                &request.journal,
                request.intent,
                &request.assignment,
                request.session,
                &request.files,
                &json!({"outputs":{},"files":{}}),
            )
            .map(|report| Some(report["files"][name].clone())),
        }
    })();
    match outcome {
        Ok(reference) => emit(&Message::Done {
            id: request.id,
            verified_at: OffsetDateTime::now_utc().unix_timestamp(),
            reference,
        }),
        Err(error) => emit(&Message::Failed {
            id: request.id,
            retry: error.kind == ErrorKind::Retry,
            fatal: error.kind == ErrorKind::Fatal,
            message: error.message,
        }),
    }
    // main's trailing JSON is not part of the worker protocol; terminate here.
    std::process::exit(0)
}

/// Counts streaming I/O without buffering a whole object.
pub(crate) struct ProgressReader<R>(pub R);
impl<R: Read> Read for ProgressReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let size = self.0.read(buffer)?;
        advance(size as u64);
        Ok(size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn retained_entry() -> PreparedAssignment {
        let fixture: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/journal-minimal-evidence.json"
        ))
        .unwrap();
        serde_json::from_value(fixture["prepared_assignments"][0].clone()).unwrap()
    }

    fn staging_entry(submission: &str) -> PreparedAssignment {
        let mut value = serde_json::to_value(retained_entry()).unwrap();
        value.as_object_mut().unwrap().remove("execution");
        value.as_object_mut().unwrap().remove("terminal_result");
        value["submission_state"] = json!(submission);
        let mut entry: PreparedAssignment = serde_json::from_value(value).unwrap();
        entry.transfers = Some(State::default());
        entry
    }

    fn output_entry() -> PreparedAssignment {
        let mut entry = retained_entry();
        entry.terminal_result = None;
        entry.execution.as_mut().unwrap().files.insert(
            "result".into(),
            crate::artifacts::OutputFile {
                path: "result.bin".into(),
                media_type: "application/octet-stream".into(),
                scientific_metadata: json!({}),
                size_bytes: 4,
                sha256: "0".repeat(64),
            },
        );
        entry.transfers = Some(State {
            inputs_ready: true,
            runner_ready: true,
            finalized_at: Some(100),
            allocation_released: true,
            ..Default::default()
        });
        entry
    }

    fn serialized(entry: &PreparedAssignment) -> Value {
        serde_json::to_value(entry).unwrap()
    }

    fn installation(entry: &PreparedAssignment) -> crate::compute::ComputeInstallation {
        let descriptor: enumeray_contracts::execution::InstallationDescriptor =
            serde_json::from_value(entry.assignment["descriptor"].clone()).unwrap();
        crate::compute::ComputeInstallation {
            execution_mode: Default::default(),
            provider_binding_id: descriptor.provider_binding_id.clone(),
            context_id: "test-context".into(),
            installation: crate::local_process::Installation {
                provider_binding_id: descriptor.provider_binding_id.clone(),
                installation: descriptor.installation.clone(),
                argv: vec!["/opt/models/scalar-double/run-model".into()],
                environment: descriptor.environment.clone(),
                file_digests: Default::default(),
            },
            descriptor,
            work_root: "/tmp/retained-work".into(),
            connection: enumeray_contracts::execution::ConnectionConfig::Direct {
                launcher: vec!["/usr/bin/unused-runner".into()],
            },
            identity_file: None,
            driver: enumeray_contracts::execution::DriverConfig::Process { scratch: None },
            slurm_script: None,
            shared_workspace: None,
        }
    }

    #[test]
    fn staging_recovery_keeps_installation_pins_and_checkpoint_evidence() {
        let mut entry = staging_entry("unknown");
        entry.transfers = None;
        let pinned = installation(&entry);
        recover_runner_inputs(&mut entry, pinned.clone()).unwrap();
        let first = begin_attempt(&mut entry, Task::RunnerInputs, Uuid::new_v4(), 100).unwrap();
        record_progress(&mut entry, &first, 5, 10, 1, Default::default(), 101).unwrap();
        recover_runner_inputs(&mut entry, pinned.clone()).unwrap();
        assert!(first.matches(&entry));
        assert_eq!(entry.transfers.as_ref().unwrap().bytes, 5);
        let before = serialized(&entry);
        let mut replacement = pinned.clone();
        replacement.work_root = "/different/installation".into();
        assert!(recover_runner_inputs(&mut entry, replacement).is_err());
        assert_eq!(serialized(&entry), before);
        complete_attempt(&mut entry, &first, 102, None, 102).unwrap();
        prepare_staging(&mut entry, pinned, false).unwrap();
        assert!(entry.transfers.as_ref().unwrap().runner_ready);
    }

    #[test]
    fn new_staging_requires_launch_evidence_and_recovery_cannot_create_submission_authority() {
        let mut entry = staging_entry("definitely_unsubmitted");
        let pinned = installation(&entry);
        entry.transfers = None;
        let before = serialized(&entry);
        assert!(recover_runner_inputs(&mut entry, pinned.clone()).is_err());
        assert_eq!(serialized(&entry), before);
        prepare_staging(&mut entry, pinned.clone(), false).unwrap();
        assert_eq!(view(&entry, 100), View::Required(Task::Inputs));
        entry.launch_intent_id = None;
        let before = serialized(&entry);
        assert!(prepare_staging(&mut entry, pinned, true).is_err());
        assert!(begin_attempt(&mut entry, Task::Inputs, Uuid::new_v4(), 100).is_err());
        assert_eq!(serialized(&entry), before);
    }

    #[test]
    fn transfer_views_keep_submission_staging_and_retry_independent() {
        for (submission, inputs, runner, retry_at, expected) in [
            (
                "definitely_unsubmitted",
                false,
                false,
                0,
                View::Required(Task::Inputs),
            ),
            (
                "definitely_unsubmitted",
                true,
                false,
                0,
                View::Required(Task::RunnerInputs),
            ),
            ("definitely_unsubmitted", true, true, 0, View::Idle),
            ("unknown", false, false, 0, View::Idle),
            ("submitted", false, false, 0, View::Idle),
            (
                "unknown",
                true,
                false,
                0,
                View::Required(Task::RunnerInputs),
            ),
            (
                "definitely_unsubmitted",
                false,
                false,
                101,
                View::Deferred {
                    task: Task::Inputs,
                    retry_at: 101,
                },
            ),
        ] {
            let mut entry = staging_entry(submission);
            let state = entry.transfers.as_mut().unwrap();
            state.inputs_ready = inputs;
            state.runner_ready = runner;
            state.next_retry_at = retry_at;
            let before = serialized(&entry);
            assert_eq!(view(&entry, 100), expected);
            assert_eq!(serialized(&entry), before);
        }
        let mut legacy = staging_entry("unknown");
        legacy.transfers = None;
        assert_eq!(view(&legacy, 100), View::Idle);
        assert!(begin_attempt(&mut legacy, Task::Inputs, Uuid::new_v4(), 100).is_err());
    }

    #[test]
    fn stale_assignment_launch_task_and_worker_results_preserve_every_record() {
        for changed in ["assignment", "launch", "task", "worker"] {
            let mut entry = staging_entry("definitely_unsubmitted");
            let attempt = begin_attempt(&mut entry, Task::Inputs, Uuid::new_v4(), 100).unwrap();
            match changed {
                "assignment" => {
                    let mut assignment = (*entry.assignment).clone();
                    assignment["assignment_id"] = json!(Uuid::new_v4());
                    entry.assignment = serde_json::from_value(assignment).unwrap();
                }
                "launch" => entry.launch_intent_id = Some(Uuid::new_v4()),
                "task" => entry.transfers.as_mut().unwrap().task = Some(Task::RunnerInputs),
                "worker" => entry.transfers.as_mut().unwrap().task_id = Some(Uuid::new_v4()),
                _ => unreachable!(),
            }
            let before = serialized(&entry);
            assert!(
                record_progress(&mut entry, &attempt, 1, 2, 0, Default::default(), 101).is_err(),
                "{changed}"
            );
            assert!(
                complete_attempt(&mut entry, &attempt, 101, None, 101).is_err(),
                "{changed}"
            );
            assert!(
                defer_attempt(&mut entry, &attempt, "stale", "stale worker").is_err(),
                "{changed}"
            );
            assert_eq!(serialized(&entry), before, "{changed}");
        }
    }

    #[test]
    fn restart_preemption_and_retry_preserve_payload_checkpoints_and_fence_old_attempts() {
        let mut entry = staging_entry("definitely_unsubmitted");
        let first = begin_attempt(&mut entry, Task::Inputs, Uuid::new_v4(), 100).unwrap();
        let payload = enumeray_client::bulk::TransferProgress {
            resumed_bytes: 12,
            transferred_bytes: 4,
            total_bytes: 20,
            ..Default::default()
        };
        record_progress(&mut entry, &first, 4, 20, 1, payload, 101).unwrap();
        interrupt(&mut entry).unwrap();
        interrupt(&mut entry).unwrap();
        let checkpoint = serde_json::to_value(entry.transfers.as_ref().unwrap().payload).unwrap();
        assert_eq!(entry.transfers.as_ref().unwrap().bytes, 4);
        let second = begin_attempt(&mut entry, Task::Inputs, Uuid::new_v4(), 102).unwrap();
        assert_eq!(
            serde_json::to_value(entry.transfers.as_ref().unwrap().payload).unwrap(),
            checkpoint
        );
        assert!(complete_attempt(&mut entry, &first, 103, None, 103).is_err());
        defer_attempt(&mut entry, &second, "transfer_interrupted", "retry").unwrap();
        assert_eq!(entry.transfers.as_ref().unwrap().retries, 1);
        assert_eq!(
            serde_json::to_value(entry.transfers.as_ref().unwrap().payload).unwrap(),
            checkpoint
        );
        let mut restored: PreparedAssignment = serde_json::from_value(serialized(&entry)).unwrap();
        let retry_at = restored.transfers.as_ref().unwrap().next_retry_at;
        let before = serialized(&restored);
        assert!(begin_attempt(&mut restored, Task::Inputs, Uuid::new_v4(), retry_at - 1).is_err());
        assert_eq!(serialized(&restored), before);
        let resumed = begin_attempt(&mut restored, Task::Inputs, Uuid::new_v4(), retry_at).unwrap();
        complete_attempt(&mut restored, &resumed, retry_at, None, retry_at).unwrap();
        assert_eq!(
            view(&restored, retry_at),
            View::Required(Task::RunnerInputs)
        );
    }

    #[test]
    fn invalid_or_regressing_progress_never_replaces_observed_bytes() {
        let mut entry = staging_entry("definitely_unsubmitted");
        let attempt = begin_attempt(&mut entry, Task::Inputs, Uuid::new_v4(), 100).unwrap();
        record_progress(&mut entry, &attempt, 5, 10, 2, Default::default(), 101).unwrap();
        for (bytes, total, units) in [(11, 10, 2), (4, 10, 2), (5, 10, 1)] {
            let before = serialized(&entry);
            assert!(
                record_progress(
                    &mut entry,
                    &attempt,
                    bytes,
                    total,
                    units,
                    Default::default(),
                    102
                )
                .is_err()
            );
            assert_eq!(serialized(&entry), before);
        }
    }

    #[test]
    fn publication_replay_is_idempotent_and_conflicts_keep_original_evidence() {
        for matching in [true, false] {
            let mut entry = output_entry();
            let task = Task::Output {
                name: "result".into(),
            };
            let attempt = begin_attempt(&mut entry, task, Uuid::new_v4(), 100).unwrap();
            let recorded = json!({"artifact_id":Uuid::new_v4()});
            entry
                .transfers
                .as_mut()
                .unwrap()
                .published
                .insert("result".into(), recorded.clone());
            let incoming = if matching {
                recorded.clone()
            } else {
                json!({"artifact_id":Uuid::new_v4()})
            };
            let before = serialized(&entry);
            let result = complete_attempt(&mut entry, &attempt, 101, Some(incoming), 101);
            if matching {
                result.unwrap();
                assert!(entry.transfers.as_ref().unwrap().task.is_none());
            } else {
                assert!(result.is_err());
                assert_eq!(serialized(&entry), before);
            }
            assert_eq!(
                entry.transfers.as_ref().unwrap().published["result"],
                recorded
            );
            assert_eq!(
                entry.execution.as_ref().unwrap().report["additional_evidence"]["kept"],
                true
            );
        }
    }

    #[test]
    fn retention_and_allocation_remain_separate_from_scientific_completion() {
        let mut entry = output_entry();
        entry.transfers.as_mut().unwrap().allocation_released = false;
        assert_eq!(view(&entry, 100), View::Idle);
        record_allocation_release(&mut entry).unwrap();
        assert!(matches!(
            view(&entry, 100),
            View::Required(Task::Output { .. })
        ));
        entry.transfers.as_mut().unwrap().expires_at = Some(102);
        let attempt = begin_attempt(
            &mut entry,
            Task::Output {
                name: "result".into(),
            },
            Uuid::new_v4(),
            101,
        )
        .unwrap();
        complete_attempt(
            &mut entry,
            &attempt,
            102,
            Some(json!({"artifact_id":Uuid::new_v4()})),
            102,
        )
        .unwrap();
        assert!(entry.transfers.as_ref().unwrap().published.is_empty());
        assert!(
            entry
                .transfers
                .as_ref()
                .unwrap()
                .diagnostic
                .as_ref()
                .unwrap()
                .starts_with("output_retention_expired:")
        );
        assert_eq!(view(&entry, 102), View::Idle);
        assert!(entry.terminal_result.is_none());
        assert!(entry.delivery_completion.is_none());
    }

    #[test]
    fn finalization_replays_preserve_original_retention_and_conflicts_are_atomic() {
        let mut entry = output_entry();
        entry.transfers.as_mut().unwrap().expires_at = Some(200);
        record_finalization(&mut entry, 100, Some(999)).unwrap();
        assert_eq!(entry.transfers.as_ref().unwrap().expires_at, Some(200));
        let before = serialized(&entry);
        assert!(record_finalization(&mut entry, 101, None).is_err());
        assert_eq!(serialized(&entry), before);
        let mut unsubmitted = staging_entry("definitely_unsubmitted");
        let before = serialized(&unsubmitted);
        assert!(record_finalization(&mut unsubmitted, 100, None).is_err());
        assert!(record_allocation_release(&mut unsubmitted).is_err());
        assert_eq!(serialized(&unsubmitted), before);
    }

    #[test]
    fn archive_completion_rejects_foreign_manifest_without_finishing_delivery() {
        let mut entry = retained_entry();
        entry.transfers = Some(State::default());
        entry.log_archive = Some(crate::log_archive::Progress {
            finalized_at: Some("2026-09-21T12:00:00Z".into()),
            ..Default::default()
        });
        let attempt = begin_attempt(&mut entry, Task::LogArchive, Uuid::new_v4(), 100).unwrap();
        let mut status = json!({"state":"ready","artifact_id":Uuid::new_v4(),"manifest":{"version":1,"assignment_id":attempt.assignment,"attempt_id":entry.assignment.decoded().attempt_id,"launch_intent_id":Uuid::new_v4(),"sources":{},"complete":true},"expires_at":null,"diagnostic":null});
        let before = serialized(&entry);
        assert!(complete_attempt(&mut entry, &attempt, 101, Some(status.clone()), 101).is_err());
        assert_eq!(serialized(&entry), before);
        status["manifest"]["launch_intent_id"] = json!(attempt.intent);
        complete_attempt(&mut entry, &attempt, 101, Some(status), 101).unwrap();
        assert!(entry.log_archive.as_ref().unwrap().finished);
        assert!(entry.delivery_completion.is_none());
    }

    #[test]
    fn interrupted_recorded_attempt_restarts_only_the_same_required_work() {
        let mut entry = staging_entry("definitely_unsubmitted");
        let old = begin_attempt(&mut entry, Task::Inputs, Uuid::new_v4(), 100).unwrap();
        assert_eq!(
            view(&entry, 101),
            View::Active {
                task: &Task::Inputs,
                id: old.id
            }
        );
        let mut restored: PreparedAssignment = serde_json::from_value(serialized(&entry)).unwrap();
        let resumed = begin_attempt(&mut restored, Task::Inputs, Uuid::new_v4(), 101).unwrap();
        assert!(!old.matches(&restored));
        assert!(resumed.matches(&restored));
        let before = serialized(&restored);
        assert!(begin_attempt(&mut restored, Task::RunnerInputs, Uuid::new_v4(), 102).is_err());
        assert_eq!(serialized(&restored), before);
    }

    #[test]
    fn terminal_reconciliation_replaces_orphaned_output_with_archive_without_losing_checkpoints() {
        let mut entry = output_entry();
        let earlier_file = entry.execution.as_ref().unwrap().files["result"].clone();
        entry
            .execution
            .as_mut()
            .unwrap()
            .files
            .insert("earlier".into(), earlier_file);
        entry
            .transfers
            .as_mut()
            .unwrap()
            .published
            .insert("earlier".into(), json!({"artifact_id":Uuid::new_v4()}));
        let old = begin_attempt(
            &mut entry,
            Task::Output {
                name: "result".into(),
            },
            Uuid::new_v4(),
            100,
        )
        .unwrap();
        record_progress(
            &mut entry,
            &old,
            4,
            20,
            1,
            enumeray_client::bulk::TransferProgress {
                resumed_bytes: 12,
                transferred_bytes: 4,
                total_bytes: 20,
                ..Default::default()
            },
            101,
        )
        .unwrap();
        let pool = Pool::new(PathBuf::from("/unused/token"), 1);
        let before = serialized(&entry);
        assert!(
            pool.prepare_attempt(&mut entry, Task::LogArchive, Uuid::new_v4(), 102)
                .is_err()
        );
        assert_eq!(serialized(&entry), before);

        // A recovered inventory can settle the scientific result before the
        // old process's output worker ever acknowledged completion.
        entry.terminal_result = retained_entry().terminal_result;
        entry.log_archive = Some(crate::log_archive::Progress {
            finalized_at: Some("2026-09-21T12:00:00Z".into()),
            ..Default::default()
        });
        let mut restored: PreparedAssignment = serde_json::from_value(serialized(&entry)).unwrap();
        let launch = restored.launch_intent_id;
        let report = (*restored.execution.as_ref().unwrap().report).clone();
        let published = restored.transfers.as_ref().unwrap().published.clone();
        let payload = restored.transfers.as_ref().unwrap().payload;
        assert_eq!(ready_task(&restored, 102), Some(Task::LogArchive));
        let archive = pool
            .prepare_attempt(&mut restored, Task::LogArchive, Uuid::new_v4(), 102)
            .unwrap();
        assert!(archive.matches(&restored));
        assert!(!old.matches(&restored));
        assert_eq!(restored.launch_intent_id, launch);
        assert_eq!(*restored.execution.as_ref().unwrap().report, report);
        assert_eq!(restored.transfers.as_ref().unwrap().published, published);
        assert_eq!(restored.transfers.as_ref().unwrap().payload, payload);
        complete_attempt(
            &mut restored,
            &archive,
            103,
            Some(json!({"state":"unavailable","artifact_id":null,"manifest":null,"expires_at":null,"diagnostic":"fixture archive unavailable"})),
            103,
        ).unwrap();
        assert!(restored.log_archive.as_ref().unwrap().finished);
    }
}
