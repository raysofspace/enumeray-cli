//! One private journal and an exclusive process lock per connector installation.
//!
//! Writes use a fresh file, fsync, atomic replacement, then directory fsync.
//! Recovery preserves execution evidence; retrieving a receipt never creates it.

use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use enumeray_contracts::assignments::claims::AssignmentReceipt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{
    Result, failure,
    records::{Assignment, Resolution, TerminalResult},
};

const MAX_JOURNAL_BYTES: u64 = 16 * 1024 * 1024;
const MAX_PREPARED_ASSIGNMENTS: usize = 64;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JournalData {
    version: u32,
    pub journal_id: Uuid,
    pub connector_id: String,
    pub api_base_url: String,
    pub active_session_id: Option<Uuid>,
    pub pending_claim_key: Option<Uuid>,
    #[serde(default)]
    pub pending_recovery: Option<PendingRecovery>,
    pub prepared_assignments: Vec<PreparedAssignment>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub exclusive_local: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_insecure_http: bool,
    /// Historical inspection only; never authorizes a later invocation.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub last_allow_development_execution: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PendingRecovery {
    pub previous_session_id: Uuid,
    pub session_id: Uuid,
}

/// A borrowed projection of retained ownership evidence. A recovery keeps the
/// predecessor's pending claim fenced until inventory reconciliation completes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnershipState<'a> {
    NoSession,
    Recorded(&'a Uuid),
    Recovering(&'a PendingRecovery),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnershipView<'a> {
    pub state: OwnershipState<'a>,
    pub pending_claim_key: Option<&'a Uuid>,
}

/// Authority is returned only after unknown-submission evidence reaches the
/// journal. It is deliberately neither serialized nor recoverable from a view.
#[derive(Debug)]
pub struct SubmissionAuthority {
    launch_intent_id: Uuid,
}

impl SubmissionAuthority {
    pub fn launch_intent_id(&self) -> Uuid {
        self.launch_intent_id
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionState {
    DefinitelyUnsubmitted,
    Submitted,
    #[default]
    Unknown,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedAssignment {
    pub assignment: Assignment,
    pub launch_intent_id: Option<Uuid>,
    // Missing legacy history is never evidence that execution did not start.
    #[serde(default)]
    submission_state: SubmissionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<super::execution::Execution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_result: Option<TerminalResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_completion:
        Option<enumeray_contracts::assignments::delivery::DeliveryCompletionReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_resolution: Option<Resolution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logs: Option<super::logs::Progress>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_archive: Option<crate::log_archive::Progress>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_event: Option<super::execution::Event>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transfers: Option<crate::transfers::State>,
}

impl PreparedAssignment {
    pub fn delivery_finished(&self) -> bool {
        crate::state::delivery_finished(self)
    }

    pub fn submission_state(&self) -> SubmissionState {
        self.submission_state
    }

    /// Persist Unknown before calling any backend, including the local fixture.
    fn begin_submission(&mut self) -> Result<()> {
        if self.submission_state != SubmissionState::DefinitelyUnsubmitted
            || self.launch_intent_id.is_none()
            || self.execution.is_some()
            || self.terminal_result.is_some()
        {
            return Err(failure(
                "assignment has no authority for a new backend submission",
            ));
        }
        self.submission_state = SubmissionState::Unknown;
        Ok(())
    }

    /// Restored definitely-unsubmitted evidence cannot override a server handle.
    pub fn observe_backend_evidence(&mut self) {
        self.submission_state = SubmissionState::Unknown;
    }

    pub fn record_execution(&mut self, execution: super::execution::Execution) -> Result<()> {
        if let Some(prior) = &self.execution {
            let prior = serde_json::to_value(prior)
                .map_err(|_| failure("cannot inspect retained execution evidence"))?;
            let candidate = serde_json::to_value(&execution)
                .map_err(|_| failure("cannot inspect execution evidence"))?;
            return if super::numeric::equivalent(&prior, &candidate) {
                Ok(())
            } else {
                Err(failure(
                    "execution evidence conflicts with its durable record",
                ))
            };
        }
        if (self.submission_state == SubmissionState::DefinitelyUnsubmitted
            && !execution.confirmed_cancellation()
            && !execution.confirmed_prelaunch_failure())
            || self.launch_intent_id.is_none()
            || self.terminal_result.is_some()
            || self.operator_resolution.is_some()
        {
            return Err(failure(
                "execution evidence conflicts with the durable launch history",
            ));
        }
        let prior_submission_state = self.submission_state;
        self.execution = Some(execution);
        self.submission_state = SubmissionState::Submitted;
        if let Err(error) = super::execution::validate_history(self) {
            self.execution = None;
            self.submission_state = prior_submission_state;
            return Err(error);
        }
        Ok(())
    }
}

pub struct Journal {
    pub directory: PathBuf,
    // The OS releases this advisory lock when the process exits, including a crash.
    _lock: File,
    pub data: JournalData,
}

impl Journal {
    pub fn ownership(&self) -> OwnershipView<'_> {
        OwnershipView {
            state: match &self.data.pending_recovery {
                Some(recovery) => OwnershipState::Recovering(recovery),
                None => match &self.data.active_session_id {
                    Some(session_id) => OwnershipState::Recorded(session_id),
                    None => OwnershipState::NoSession,
                },
            },
            pending_claim_key: self.data.pending_claim_key.as_ref(),
        }
    }

    pub fn require_session(&self, session_id: Uuid) -> Result<()> {
        if self.ownership().state != OwnershipState::Recorded(&session_id) {
            return Err(failure("session does not match the durable journal owner"));
        }
        Ok(())
    }

    /// Same-owner retries persist and return the original request identity.
    pub fn begin_claim(&mut self, session_id: Uuid) -> Result<Uuid> {
        self.require_session(session_id)?;
        if self.data.pending_claim_key.is_none()
            && self.data.prepared_assignments.len() >= MAX_PREPARED_ASSIGNMENTS
        {
            return Err(failure(
                "journal has reached its 64-assignment preparation limit",
            ));
        }
        let claim_key = *self.data.pending_claim_key.get_or_insert_with(Uuid::new_v4);
        self.save()?;
        Ok(claim_key)
    }

    fn require_claim(&self, session_id: Uuid, claim_key: Uuid) -> Result<()> {
        self.require_session(session_id)?;
        if self.data.pending_claim_key != Some(claim_key) || claim_key.is_nil() {
            return Err(failure(
                "claim acknowledgement conflicts with pending ownership",
            ));
        }
        Ok(())
    }

    pub fn complete_empty_claim(&mut self, session_id: Uuid, claim_key: Uuid) -> Result<()> {
        self.require_claim(session_id, claim_key)?;
        self.clear_claim()
    }

    pub fn prepare_claim_assignment(
        &mut self,
        session_id: Uuid,
        claim_key: Uuid,
        assignment: Value,
    ) -> Result<Value> {
        self.require_claim(session_id, claim_key)?;
        self.prepare_assignment(assignment)
    }

    /// A predecessor key can be discarded only after its owner is fenced and
    /// every inventory page/detail has reconciled successfully.
    pub fn inventory_reconciled(&mut self, session_id: Uuid) -> Result<()> {
        self.require_session(session_id)?;
        self.clear_claim()
    }

    fn clear_claim(&mut self) -> Result<()> {
        let pending = self.data.pending_claim_key.take();
        if let Err(error) = self.save() {
            self.data.pending_claim_key = pending;
            return Err(error);
        }
        Ok(())
    }

    pub fn prepare_recovery(&mut self, previous_session_id: Uuid) -> Result<PendingRecovery> {
        if self.data.active_session_id != Some(previous_session_id) || previous_session_id.is_nil()
        {
            return Err(failure(
                "recovery predecessor conflicts with journal ownership",
            ));
        }
        if self.data.pending_recovery.is_none() {
            self.data.pending_recovery = Some(PendingRecovery {
                previous_session_id,
                session_id: Uuid::new_v4(),
            });
        }
        let recovery = self
            .data
            .pending_recovery
            .as_ref()
            .expect("pending recovery");
        if recovery.previous_session_id != previous_session_id {
            return Err(failure(
                "recovery predecessor conflicts with pending recovery",
            ));
        }
        let recovery = recovery.clone();
        self.save()?;
        Ok(recovery)
    }

    pub fn complete_recovery(&mut self, previous_session_id: Uuid, session_id: Uuid) -> Result<()> {
        if self.data.pending_recovery.is_none()
            && self.data.active_session_id == Some(session_id)
            && !session_id.is_nil()
            && session_id != previous_session_id
        {
            return Ok(());
        }
        let expected = PendingRecovery {
            previous_session_id,
            session_id,
        };
        if self.data.active_session_id != Some(previous_session_id)
            || self.data.pending_recovery.as_ref() != Some(&expected)
        {
            return Err(failure(
                "recovery acknowledgement conflicts with pending ownership",
            ));
        }
        self.data.active_session_id = Some(session_id);
        let recovery = self.data.pending_recovery.take();
        if let Err(error) = self.save() {
            self.data.active_session_id = Some(previous_session_id);
            self.data.pending_recovery = recovery;
            return Err(error);
        }
        Ok(())
    }

    pub fn complete_session(&mut self, session_id: Uuid) -> Result<()> {
        self.require_session(session_id)?;
        if self.data.pending_claim_key.is_some() {
            return Err(failure("cannot release a session with an unresolved claim"));
        }
        self.data.active_session_id = None;
        if let Err(error) = self.save() {
            self.data.active_session_id = Some(session_id);
            return Err(error);
        }
        Ok(())
    }

    pub fn begin_submission(
        &mut self,
        assignment_id: Uuid,
        launch_intent_id: Uuid,
    ) -> Result<SubmissionAuthority> {
        let entry = self
            .data
            .prepared_assignments
            .iter_mut()
            .find(|entry| entry.assignment.decoded().assignment_id == assignment_id)
            .ok_or_else(|| failure("submission assignment is missing from the durable journal"))?;
        if entry.launch_intent_id != Some(launch_intent_id) || launch_intent_id.is_nil() {
            return Err(failure(
                "submission identity conflicts with the durable launch history",
            ));
        }
        entry.begin_submission()?;
        // On any persistence failure retain Unknown in memory too. No caller
        // receives authority, including when replacement may already have run.
        self.save()?;
        Ok(SubmissionAuthority { launch_intent_id })
    }

    pub fn observe_backend_evidence(&mut self, assignment_id: Uuid) -> Result<()> {
        let entry = self
            .data
            .prepared_assignments
            .iter_mut()
            .find(|entry| entry.assignment.decoded().assignment_id == assignment_id)
            .ok_or_else(|| failure("backend evidence has no matching assignment"))?;
        if entry.submission_state() == SubmissionState::DefinitelyUnsubmitted {
            // Server evidence removes new-submission authority. A subsequent
            // matching local backend lookup must establish execution evidence.
            entry.observe_backend_evidence();
            self.save()?;
        }
        Ok(())
    }

    pub fn initialize(
        directory: &Path,
        api_base_url: String,
        connector_id: String,
    ) -> Result<Self> {
        Self::initialize_with_policy(directory, api_base_url, connector_id, None)
    }

    pub fn initialize_service(
        directory: &Path,
        api_base_url: String,
        connector_id: String,
        exclusive_local: bool,
        allow_http: bool,
    ) -> Result<Self> {
        Self::initialize_with_policy(
            directory,
            api_base_url,
            connector_id,
            Some((exclusive_local, allow_http)),
        )
    }

    fn initialize_with_policy(
        directory: &Path,
        api_base_url: String,
        connector_id: String,
        service: Option<(bool, bool)>,
    ) -> Result<Self> {
        super::client::validate_origin(&api_base_url, service.is_some_and(|(_, http)| http))?;
        if connector_id.is_empty()
            || connector_id.len() > 100
            || !connector_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
        {
            return Err(failure(
                "connector ID must be 1–100 ASCII letters, digits, underscores, or hyphens",
            ));
        }
        create_private_directory(directory)?;
        let lock = lock_directory(directory)?;
        if directory
            .join("journal.json")
            .try_exists()
            .map_err(|_| failure("cannot inspect journal"))?
        {
            return Err(failure("journal already exists; use inspect or claim-once"));
        }
        let journal = Self {
            directory: directory.to_owned(),
            _lock: lock,
            data: JournalData {
                version: 1,
                journal_id: Uuid::new_v4(),
                connector_id,
                api_base_url,
                active_session_id: None,
                pending_claim_key: None,
                pending_recovery: None,
                prepared_assignments: Vec::new(),
                exclusive_local: service.is_some_and(|(exclusive, _)| exclusive),
                allow_insecure_http: service.is_some_and(|(_, http)| http),
                last_allow_development_execution: false,
            },
        };
        journal.save()?;
        Ok(journal)
    }

    pub fn open(directory: &Path) -> Result<Self> {
        check_private_path(directory, true)?;
        let lock = lock_directory(directory)?;
        Self::load(directory, lock)
    }

    /// Atomic journal snapshots for inspection never create ownership files or
    /// contend with the service owner. Return data rather than a writable Journal.
    pub(crate) fn snapshot(directory: &Path) -> Result<(JournalData, Value)> {
        check_private_path(directory, true)?;
        let path = directory.join("journal.json");
        check_private_path(&path, false)?;
        let file = File::open(path).map_err(|_| failure("cannot read journal snapshot"))?;
        let journal = Self::load(directory, file)?;
        let summary = journal.summary();
        Ok((journal.data, summary))
    }

    fn load(directory: &Path, lock: File) -> Result<Self> {
        let data_path = directory.join("journal.json");
        check_private_path(&data_path, false)?;
        let file = File::open(data_path).map_err(|_| failure("cannot open journal"))?;
        let mut bytes = Vec::new();
        file.take(MAX_JOURNAL_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| failure("cannot read journal"))?;
        if bytes.len() as u64 > MAX_JOURNAL_BYTES {
            return Err(failure("journal exceeds the 16 MiB limit"));
        }
        let envelope: Value =
            serde_json::from_slice(&bytes).map_err(|_| failure("malformed journal"))?;
        if envelope["version"] != 1 {
            return Err(failure(
                "unsupported journal version; preserve the complete state and use the matching release or an explicitly supported offline migration",
            ));
        }
        let data: JournalData = serde_json::from_slice(&bytes)
            .map_err(|_| failure("journal is malformed; preserve it for reconciliation"))?;
        if data.version != 1 || data.prepared_assignments.len() > MAX_PREPARED_ASSIGNMENTS {
            return Err(failure(
                "unsupported journal version or assignment count; preserve the complete state and use the matching release or an explicitly supported offline migration",
            ));
        }
        if data.journal_id.is_nil()
            || data.active_session_id.is_some_and(|id| id.is_nil())
            || data.pending_claim_key.is_some_and(|id| id.is_nil())
            || data.pending_recovery.as_ref().is_some_and(|recovery| {
                recovery.session_id.is_nil()
                    || recovery.previous_session_id.is_nil()
                    || recovery.session_id == recovery.previous_session_id
                    || data.active_session_id != Some(recovery.previous_session_id)
            })
        {
            return Err(failure("journal contains invalid ownership identities"));
        }
        super::client::validate_origin(&data.api_base_url, data.allow_insecure_http)?;
        let mut assignment_ids = HashSet::new();
        let mut run_ids = HashSet::new();
        let mut attempt_ids = HashSet::new();
        let mut launch_intents = HashSet::new();
        for prepared in &data.prepared_assignments {
            let receipt = validate_assignment(&prepared.assignment, &data)?;
            super::execution::validate_history(prepared)?;
            super::logs::validate_history(prepared)?;
            if prepared.log_archive.as_ref().is_some_and(|a| {
                a.finalization_registered && a.finalized_at.is_none()
                    || a.finished && prepared.terminal_result.is_none()
            }) {
                return Err(failure(
                    "archive progress conflicts with scientific execution history",
                ));
            }
            if let Some(state) = &prepared.transfers {
                state.validate(prepared)?;
            }
            if !assignment_ids.insert(receipt.assignment_id)
                || !run_ids.insert(receipt.run_id)
                || !attempt_ids.insert(receipt.attempt_id)
                || prepared
                    .launch_intent_id
                    .is_some_and(|id| id.is_nil() || !launch_intents.insert(id))
                || (prepared.submission_state == SubmissionState::DefinitelyUnsubmitted
                    && prepared.launch_intent_id.is_none())
            {
                return Err(failure(
                    "journal contains duplicate execution identities; preserve it for reconciliation",
                ));
            }
        }
        Ok(Self {
            directory: directory.to_owned(),
            _lock: lock,
            data,
        })
    }

    pub fn configure_service(&mut self, exclusive_local: bool, allow_http: bool) -> Result<()> {
        self.data.version = 1;
        self.data.exclusive_local = exclusive_local;
        self.data.allow_insecure_http = allow_http;
        self.save()
    }

    /// Archive first, then remove from the active journal. A crash may leave both
    /// copies; the next pass verifies them before completing the removal.
    pub fn archive_completed(&mut self) -> Result<()> {
        // An ambiguous claim-snapshot write may retain its prepared candidate
        // beside the original request key. Until that request is settled, its
        // matching replay must still find the original local launch identity.
        if self.data.pending_claim_key.is_some() {
            return Ok(());
        }
        let completed: Vec<_> = self
            .data
            .prepared_assignments
            .iter()
            .filter(|entry| entry.delivery_finished())
            .collect();
        if completed.is_empty() {
            return Ok(());
        }
        let directory = self.directory.join("archive");
        create_private_directory(&directory)?;
        for entry in &completed {
            let path = directory.join(format!("{}.json", entry.assignment.decoded().assignment_id));
            let value =
                serde_json::json!({"version":1, "journal_id":self.data.journal_id, "entry":entry});
            if path
                .try_exists()
                .map_err(|_| failure("cannot inspect archive"))?
            {
                check_private_path(&path, false)?;
                let file = File::open(&path).map_err(|_| failure("cannot read archive"))?;
                let mut bytes = Vec::new();
                file.take(MAX_JOURNAL_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|_| failure("cannot read archive"))?;
                let prior: Value = serde_json::from_slice(&bytes)
                    .map_err(|_| failure("invalid archived evidence"))?;
                if !super::numeric::equivalent(&prior, &value) {
                    return Err(failure("archived evidence conflicts with active journal"));
                }
            } else {
                let staging = directory.join(format!(".{}.tmp", Uuid::new_v4()));
                let mut options = OpenOptions::new();
                options.write(true).create_new(true);
                private_file_options(&mut options);
                let mut file = options
                    .open(&staging)
                    .map_err(|_| failure("cannot create archive"))?;
                let bytes =
                    serde_json::to_vec(&value).map_err(|_| failure("cannot encode archive"))?;
                file.write_all(&bytes)
                    .and_then(|()| file.sync_all())
                    .map_err(|_| failure("cannot persist archive"))?;
                fs::rename(staging, &path).map_err(|_| failure("cannot publish archive"))?;
            }
        }
        File::open(directory)
            .and_then(|file| file.sync_all())
            .map_err(|_| failure("cannot sync archive"))?;
        for entry in completed {
            super::cleanup::enqueue(&self.directory, entry.assignment.decoded().assignment_id)?;
        }
        let original = std::mem::take(&mut self.data.prepared_assignments);
        let (completed, pending): (Vec<_>, Vec<_>) = original
            .into_iter()
            .partition(|entry| entry.delivery_finished());
        self.data.prepared_assignments = pending;
        if let Err(error) = self.save() {
            self.data.prepared_assignments.extend(completed);
            return Err(error);
        }
        Ok(())
    }

    pub fn begin_session(&mut self) -> Result<Uuid> {
        if self.data.active_session_id.is_some()
            || self.data.pending_claim_key.is_some()
            || self.data.pending_recovery.is_some()
        {
            return Err(failure(
                "journal has unresolved process ownership; use reconcile after stopping the prior process",
            ));
        }
        if self.has_unknown_submissions() {
            return Err(failure(
                "journal contains unknown submission outcomes; preserve it for evidence-based resolution",
            ));
        }
        if self.data.prepared_assignments.len() >= MAX_PREPARED_ASSIGNMENTS {
            return Err(failure(
                "journal has reached its 64-assignment preparation limit",
            ));
        }
        self.begin_reconciliation_session()
    }

    pub fn begin_reconciliation_session(&mut self) -> Result<Uuid> {
        if self.data.active_session_id.is_some() || self.data.pending_recovery.is_some() {
            return Err(failure("journal already has unresolved process ownership"));
        }
        let session_id = Uuid::new_v4();
        self.data.active_session_id = Some(session_id);
        self.save()?;
        Ok(session_id)
    }

    pub fn save(&self) -> Result<()> {
        self.save_bytes(&self.encoded()?)
    }

    fn encoded(&self) -> Result<Vec<u8>> {
        let bytes = serde_json::to_vec_pretty(&self.data)
            .map_err(|_| failure("cannot serialize journal"))?;
        if bytes.len() as u64 > MAX_JOURNAL_BYTES {
            return Err(failure(
                "journal exceeds the 16 MiB limit; preserve pending ownership for reconciliation",
            ));
        }
        Ok(bytes)
    }

    fn save_bytes(&self, bytes: &[u8]) -> Result<()> {
        let staging_path = self
            .directory
            .join(format!(".journal-{}.tmp", Uuid::new_v4()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        private_file_options(&mut options);
        let mut staging = options
            .open(&staging_path)
            .map_err(|_| failure("cannot create private journal staging file"))?;
        staging
            .write_all(bytes)
            .and_then(|()| staging.sync_all())
            .map_err(|_| failure("cannot persist journal staging file"))?;
        fs::rename(&staging_path, self.directory.join("journal.json"))
            .map_err(|_| failure("cannot atomically replace journal"))?;
        File::open(&self.directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| failure("cannot sync journal directory"))
    }

    pub fn prepare_assignment(&mut self, assignment: Value) -> Result<Value> {
        let receipt = validate_assignment(&assignment, &self.data)?;
        let original_length = self.data.prepared_assignments.len();
        let existing = self.data.prepared_assignments.iter().find(|prepared| {
            prepared.assignment.decoded().assignment_id == receipt.assignment_id
                || prepared.assignment.decoded().attempt_id == receipt.attempt_id
                || prepared.assignment.decoded().run_id == receipt.run_id
        });
        let launch_intent_id = if let Some(existing) = existing {
            if !super::numeric::equivalent(&existing.assignment, &assignment) {
                return Err(failure(
                    "assignment conflicts with the durable local record; preserve it for reconciliation",
                ));
            }
            existing.launch_intent_id
        } else {
            if self.data.prepared_assignments.len() >= MAX_PREPARED_ASSIGNMENTS {
                return Err(failure(
                    "journal has reached its 64-assignment preparation limit",
                ));
            }
            let launch_intent_id = Uuid::new_v4();
            self.data.prepared_assignments.push(PreparedAssignment {
                assignment: Assignment::decode(assignment)?,
                launch_intent_id: Some(launch_intent_id),
                submission_state: SubmissionState::DefinitelyUnsubmitted,
                execution: None,
                terminal_result: None,
                delivery_completion: None,
                operator_resolution: None,
                logs: None,
                log_archive: None,
                live_event: None,
                transfers: None,
            });
            Some(launch_intent_id)
        };
        let pending = self.data.pending_claim_key.take();
        let bytes = match self.encoded() {
            Ok(bytes) => bytes,
            Err(error) => {
                self.data.pending_claim_key = pending;
                self.data.prepared_assignments.truncate(original_length);
                return Err(error);
            }
        };
        if let Err(error) = self.save_bytes(&bytes) {
            // Replacement may have committed before a sync error. Retain its
            // immutable launch identity while retrying the original claim key.
            self.data.pending_claim_key = pending;
            return Err(error);
        }
        Ok(json!({
            "status": "prepared", "assignment_id": receipt.assignment_id, "run_id": receipt.run_id,
            "attempt_id": receipt.attempt_id, "journal_id": self.data.journal_id,
            "launch_intent_id": launch_intent_id
        }))
    }

    pub fn has_unknown_submissions(&self) -> bool {
        self.data.prepared_assignments.iter().any(|entry| {
            entry.submission_state == SubmissionState::Unknown && entry.terminal_result.is_none()
        })
    }

    /// A server receipt is not local evidence of submission. Missing entries get
    /// no launch identity and remain blocked even after repeated reconciliation.
    pub fn reconcile_assignment(&mut self, assignment: Value) -> Result<()> {
        let receipt = validate_assignment(&assignment, &self.data)?;
        let original_length = self.data.prepared_assignments.len();
        let existing = self.data.prepared_assignments.iter().find(|entry| {
            entry.assignment.decoded().assignment_id == receipt.assignment_id
                || entry.assignment.decoded().run_id == receipt.run_id
                || entry.assignment.decoded().attempt_id == receipt.attempt_id
        });
        if let Some(existing) = existing {
            if !super::numeric::equivalent(&existing.assignment, &assignment) {
                return Err(failure(
                    "assignment conflicts with the durable local record; preserve it for reconciliation",
                ));
            }
        } else {
            if self.data.prepared_assignments.len() >= MAX_PREPARED_ASSIGNMENTS {
                return Err(failure(
                    "journal has reached its 64-assignment preparation limit",
                ));
            }
            self.data.prepared_assignments.push(PreparedAssignment {
                assignment: Assignment::decode(assignment)?,
                launch_intent_id: None,
                submission_state: SubmissionState::Unknown,
                execution: None,
                terminal_result: None,
                delivery_completion: None,
                operator_resolution: None,
                logs: None,
                log_archive: None,
                live_event: None,
                transfers: None,
            });
        }
        let bytes = match self.encoded() {
            Ok(bytes) => bytes,
            Err(error) => {
                self.data.prepared_assignments.truncate(original_length);
                return Err(error);
            }
        };
        self.save_bytes(&bytes)
    }

    pub fn record_execution_policy(&mut self, allowed: bool) -> Result<()> {
        self.data.last_allow_development_execution = allowed;
        self.save()
    }

    pub fn summary(&self) -> Value {
        json!({
            "last_allow_development_execution": self.data.last_allow_development_execution,
            "execution_policy_note": "Historical policy from the last launch-capable invocation; explicit startup opt-in is always required.",
            "journal_id": self.data.journal_id, "connector_id": self.data.connector_id,
            "api_base_url": self.data.api_base_url, "active_session_id": self.data.active_session_id,
            "pending_claim_key": self.data.pending_claim_key,
            "pending_recovery": self.data.pending_recovery,
            "prepared_assignments": self.data.prepared_assignments.iter().map(|prepared| json!({
                "assignment_id": prepared.assignment.decoded().assignment_id, "run_id": prepared.assignment.decoded().run_id,
                "attempt_id": prepared.assignment.decoded().attempt_id, "launch_intent_id": prepared.launch_intent_id,
                "submission_state": prepared.submission_state
                ,"result_receipt": prepared.terminal_result.as_ref().map(|result| &result["receipt"]),
                "delivery_completion":prepared.delivery_completion,
                "delivery_blocker":super::execution::delivery_blocker(prepared, &self.directory),
                "delivery_next_action":super::execution::delivery_blocker(prepared, &self.directory).map(super::execution::delivery_next_action),
                "live_logs_delivered":prepared.logs.as_ref().is_some_and(|logs| logs.delivered(prepared.assignment.decoded())),
                "logs":prepared.logs.as_ref().map(super::logs::Progress::summary),
                "transfers":prepared.transfers.as_ref().map(crate::transfers::State::summary),
                "log_archive":prepared.log_archive,
                "operator_resolution": prepared.operator_resolution
            })).collect::<Vec<_>>()
        })
    }
}

fn validate_assignment(value: &Value, journal: &JournalData) -> Result<AssignmentReceipt> {
    let receipt: AssignmentReceipt = serde_json::from_value(value.clone()).map_err(|_| {
        failure("assignment has missing or invalid contract fields; preserve pending ownership")
    })?;
    if receipt.connector_id != journal.connector_id || receipt.journal_id != journal.journal_id {
        return Err(failure(
            "assignment does not belong to this connector journal; preserve pending ownership",
        ));
    }
    if receipt.assignment_id.is_nil() || receipt.run_id.is_nil() || receipt.attempt_id.is_nil() {
        return Err(failure("assignment contains invalid execution identities"));
    }
    if receipt.publication_id.is_empty()
        || receipt.context_id.is_empty()
        || receipt.definition.contract_version != receipt.contract_version
        || receipt.requirements
            != enumeray_contracts::execution::ExecutionRequirements::derive(
                receipt.requirements.driver,
                &receipt.definition,
            )
    {
        return Err(failure(
            "assignment contains inconsistent execution identities; preserve pending ownership",
        ));
    }
    Ok(receipt)
}

fn lock_directory(directory: &Path) -> Result<File> {
    let lock_path = directory.join("owner.lock");
    if lock_path
        .try_exists()
        .map_err(|_| failure("cannot inspect journal lock"))?
    {
        check_private_path(&lock_path, false)?;
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    private_file_options(&mut options);
    let lock = options
        .open(lock_path)
        .map_err(|_| failure("cannot open journal lock"))?;
    lock.try_lock()
        .map_err(|_| failure("journal is already owned by another process"))?;
    Ok(lock)
}

#[cfg(unix)]
fn private_file_options(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn private_file_options(_options: &mut OpenOptions) {}

fn create_private_directory(directory: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(directory) {
        Ok(()) => {
            let parent = directory
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| failure("cannot sync new journal directory entry"))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(_) => {
            return Err(failure(
                "cannot create journal directory; its parent must exist",
            ));
        }
    }
    check_private_path(directory, true)
}

pub fn check_private_path(path: &Path, directory: bool) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| failure("cannot inspect private file or directory"))?;
    if metadata.file_type().is_symlink()
        || (directory && !metadata.is_dir())
        || (!directory && !metadata.is_file())
    {
        return Err(failure(
            "journal and token paths must be regular files/directories, without symbolic links",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(failure(
                "journal and token paths must be private: directories 0700, files 0600",
            ));
        }
    }
    #[cfg(not(unix))]
    return Err(failure(
        "the test connector currently requires Unix private-file permissions",
    ));
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn persistent_v1_journal_and_archive_preserve_execution_evidence() {
        let document: Value =
            serde_json::from_str(include_str!("../../../../release/fixtures/journal-v1.json"))
                .unwrap();
        let archive: Value =
            serde_json::from_str(include_str!("../../../../release/fixtures/archive-v1.json"))
                .unwrap();
        let directory =
            std::env::temp_dir().join(format!("enumeray-persistent-baseline-{}", Uuid::new_v4()));
        let journal = Journal::initialize(
            &directory,
            "http://127.0.0.1:8080".into(),
            "test-connector".into(),
        )
        .unwrap();
        drop(journal);
        fs::write(
            directory.join("journal.json"),
            serde_json::to_vec(&document).unwrap(),
        )
        .unwrap();
        let mut journal = Journal::open(&directory).unwrap();
        assert_eq!(journal.data.journal_id.to_string(), document["journal_id"]);
        let prepared = &mut journal.data.prepared_assignments[0];
        assert_eq!(
            serde_json::to_value(&prepared.assignment).unwrap(),
            document["prepared_assignments"][0]["assignment"]
        );
        assert!(prepared.begin_submission().is_err());
        journal.save().unwrap();
        drop(journal);
        let mut journal = Journal::open(&directory).unwrap();
        // A pre-existing retained archive must reconcile with the original
        // journal rather than regenerate receipts or a scientific launch.
        let archive_directory = directory.join("archive");
        create_private_directory(&archive_directory).unwrap();
        let archive_path = archive_directory.join(format!(
            "{}.json",
            archive["entry"]["assignment"]["assignment_id"]
                .as_str()
                .unwrap()
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        private_file_options(&mut options);
        let mut file = options.open(&archive_path).unwrap();
        file.write_all(&serde_json::to_vec(&archive).unwrap())
            .unwrap();
        drop(file);
        journal.archive_completed().unwrap();
        assert!(journal.data.prepared_assignments.is_empty());
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(archive_path).unwrap()).unwrap(),
            archive
        );
        drop(journal);
        let journal = Journal::open(&directory).unwrap();
        assert_eq!(journal.data.journal_id.to_string(), document["journal_id"]);
        assert!(journal.data.prepared_assignments.is_empty());
        drop(journal);
        fs::remove_dir_all(directory).unwrap();
    }

    fn fresh_journal(label: &str) -> Journal {
        let path = std::env::temp_dir().join(format!("enumeray-{label}-{}", Uuid::new_v4()));
        Journal::initialize(
            &path,
            "http://127.0.0.1:8080".into(),
            "test-connector".into(),
        )
        .unwrap()
    }

    fn assignment_for(journal: &Journal) -> Value {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/journal-minimal-evidence.json"
        ))
        .unwrap();
        let mut assignment = fixture["prepared_assignments"][0]["assignment"].clone();
        assignment["journal_id"] = json!(journal.data.journal_id);
        for field in ["assignment_id", "run_id", "attempt_id"] {
            assignment[field] = json!(Uuid::new_v4());
        }
        assignment
    }

    #[test]
    fn checked_ownership_preserves_claim_and_recovery_identities_until_reconciled() {
        let mut journal = fresh_journal("ownership");
        let path = journal.directory.clone();
        assert_eq!(journal.ownership().state, OwnershipState::NoSession);
        let session = journal.begin_session().unwrap();
        let claim = journal.begin_claim(session).unwrap();
        assert_eq!(journal.begin_claim(session).unwrap(), claim);
        let recorded = serde_json::to_value(&journal.data).unwrap();
        for invalid in [Uuid::nil(), Uuid::new_v4()] {
            assert!(journal.begin_claim(invalid).is_err());
            assert!(journal.prepare_recovery(invalid).is_err());
            assert!(journal.complete_empty_claim(session, invalid).is_err());
            assert!(journal.inventory_reconciled(invalid).is_err());
            assert!(journal.complete_session(invalid).is_err());
            assert_eq!(serde_json::to_value(&journal.data).unwrap(), recorded);
        }
        let recovery = journal.prepare_recovery(session).unwrap();
        assert_eq!(journal.prepare_recovery(session).unwrap(), recovery);
        assert_eq!(
            journal.ownership().state,
            OwnershipState::Recovering(&recovery)
        );
        let recovering = serde_json::to_value(&journal.data).unwrap();
        assert!(journal.begin_claim(session).is_err());
        assert!(journal.inventory_reconciled(session).is_err());
        assert!(journal.complete_recovery(session, Uuid::new_v4()).is_err());
        assert_eq!(serde_json::to_value(&journal.data).unwrap(), recovering);
        drop(journal);
        let mut journal = Journal::open(&path).unwrap();
        assert_eq!(journal.prepare_recovery(session).unwrap(), recovery);
        journal
            .complete_recovery(session, recovery.session_id)
            .unwrap();
        journal
            .complete_recovery(session, recovery.session_id)
            .unwrap();
        assert_eq!(journal.data.pending_claim_key, Some(claim));
        assert!(journal.inventory_reconciled(session).is_err());
        journal.inventory_reconciled(recovery.session_id).unwrap();
        assert!(journal.data.pending_claim_key.is_none());
        journal.complete_session(recovery.session_id).unwrap();
        drop(journal);
        assert_eq!(
            Journal::open(&path).unwrap().ownership().state,
            OwnershipState::NoSession
        );
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn backend_submission_authority_requires_matching_durable_launch_identity() {
        let mut journal = fresh_journal("submission-authority");
        let path = journal.directory.clone();
        let assignment = assignment_for(&journal);
        let id = assignment["assignment_id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        journal.prepare_assignment(assignment).unwrap();
        let intent = journal.data.prepared_assignments[0]
            .launch_intent_id
            .unwrap();
        for (assignment_id, launch_id) in [
            (Uuid::new_v4(), intent),
            (id, Uuid::new_v4()),
            (id, Uuid::nil()),
        ] {
            assert!(journal.begin_submission(assignment_id, launch_id).is_err());
            assert_eq!(
                journal.data.prepared_assignments[0].submission_state(),
                SubmissionState::DefinitelyUnsubmitted
            );
        }
        assert_eq!(
            journal
                .begin_submission(id, intent)
                .unwrap()
                .launch_intent_id(),
            intent
        );
        assert!(journal.begin_submission(id, intent).is_err());
        drop(journal);
        let mut journal = Journal::open(&path).unwrap();
        assert_eq!(
            journal.data.prepared_assignments[0].launch_intent_id,
            Some(intent)
        );
        assert_eq!(
            journal.data.prepared_assignments[0].submission_state(),
            SubmissionState::Unknown
        );
        assert!(journal.begin_submission(id, intent).is_err());
        drop(journal);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn execution_replay_is_idempotent_and_conflicting_candidates_preserve_history() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/journal-minimal-evidence.json"
        ))
        .unwrap();
        let retained = fixture["prepared_assignments"][0].clone();
        let mut entry: PreparedAssignment = serde_json::from_value(retained.clone()).unwrap();
        entry
            .record_execution(serde_json::from_value(retained["execution"].clone()).unwrap())
            .unwrap();
        assert_eq!(serde_json::to_value(&entry).unwrap(), retained);
        let mut conflicting = retained["execution"].clone();
        conflicting["report"]["attempt_id"] = json!(Uuid::new_v4());
        assert!(
            entry
                .record_execution(serde_json::from_value(conflicting.clone()).unwrap())
                .is_err()
        );
        assert_eq!(serde_json::to_value(&entry).unwrap(), retained);

        entry.execution = None;
        entry.terminal_result = None;
        entry.submission_state = SubmissionState::Unknown;
        let awaiting = serde_json::to_value(&entry).unwrap();
        conflicting["receipt"] = Value::Null;
        assert!(
            entry
                .record_execution(serde_json::from_value(conflicting).unwrap())
                .is_err()
        );
        assert_eq!(serde_json::to_value(&entry).unwrap(), awaiting);
        let mut finalized = retained["execution"].clone();
        finalized["receipt"] = Value::Null;
        // This historical fixture retains its retired scalar-fixture namespace;
        // a newly observed process execution must use the current backend handle.
        for event in finalized["events"].as_array_mut().unwrap() {
            event["request"]["backend_handle"]["namespace"] = json!("enumeray-process-v1");
        }
        entry
            .record_execution(serde_json::from_value(finalized).unwrap())
            .unwrap();
        assert_eq!(entry.submission_state(), SubmissionState::Submitted);
    }

    #[test]
    fn failed_submission_save_returns_no_authority_and_blocks_in_process_retry() {
        let mut journal = fresh_journal("submission-save-failure");
        let path = journal.directory.clone();
        let assignment = assignment_for(&journal);
        let id = assignment["assignment_id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        journal.prepare_assignment(assignment).unwrap();
        let intent = journal.data.prepared_assignments[0]
            .launch_intent_id
            .unwrap();
        let original = fs::read(path.join("journal.json")).unwrap();
        fs::rename(path.join("journal.json"), path.join("saved-journal.json")).unwrap();
        fs::create_dir(path.join("journal.json")).unwrap();
        assert!(journal.begin_submission(id, intent).is_err());
        assert_eq!(
            journal.data.prepared_assignments[0].submission_state(),
            SubmissionState::Unknown
        );
        assert!(journal.begin_submission(id, intent).is_err());
        assert_eq!(fs::read(path.join("saved-journal.json")).unwrap(), original);
        fs::remove_dir(path.join("journal.json")).unwrap();
        fs::rename(path.join("saved-journal.json"), path.join("journal.json")).unwrap();
        drop(journal);
        Journal::open(&path).unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn preparation_capacity_allows_matching_replay_and_blocks_the_next_assignment() {
        let mut journal = fresh_journal("preparation-capacity");
        let path = journal.directory.clone();
        let session = journal.begin_session().unwrap();
        let mut last = Value::Null;
        for _ in 0..MAX_PREPARED_ASSIGNMENTS {
            last = assignment_for(&journal);
            journal.prepare_assignment(last.clone()).unwrap();
        }
        let original = fs::read(path.join("journal.json")).unwrap();
        let intent = journal
            .data
            .prepared_assignments
            .last()
            .unwrap()
            .launch_intent_id;
        journal.prepare_assignment(last).unwrap();
        assert_eq!(
            journal
                .data
                .prepared_assignments
                .last()
                .unwrap()
                .launch_intent_id,
            intent
        );
        assert!(
            journal
                .prepare_assignment(assignment_for(&journal))
                .is_err()
        );
        assert!(journal.begin_claim(session).is_err());
        assert_eq!(
            journal.data.prepared_assignments.len(),
            MAX_PREPARED_ASSIGNMENTS
        );
        assert_eq!(fs::read(path.join("journal.json")).unwrap(), original);
        drop(journal);
        Journal::open(&path).unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn oversized_claim_keeps_the_original_pending_key_and_readable_snapshot() {
        let mut journal = fresh_journal("oversized-claim");
        let path = journal.directory.clone();
        let session = journal.begin_session().unwrap();
        let key = journal.begin_claim(session).unwrap();
        let original = fs::read(path.join("journal.json")).unwrap();
        let mut assignment = assignment_for(&journal);
        assignment["definition"]["input_schema"]["description"] =
            json!("x".repeat(MAX_JOURNAL_BYTES as usize));
        let error = journal
            .prepare_claim_assignment(session, key, assignment)
            .unwrap_err();
        assert!(error.to_string().contains("16 MiB"));
        assert_eq!(journal.data.pending_claim_key, Some(key));
        assert!(journal.data.prepared_assignments.is_empty());
        assert_eq!(fs::read(path.join("journal.json")).unwrap(), original);
        assert_eq!(journal.begin_claim(session).unwrap(), key);
        drop(journal);
        Journal::open(&path).unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn failed_claim_snapshot_retains_its_launch_identity_and_original_request_key() {
        let mut journal = fresh_journal("claim-save-failure");
        let path = journal.directory.clone();
        let session = journal.begin_session().unwrap();
        let key = journal.begin_claim(session).unwrap();
        let assignment = assignment_for(&journal);
        fs::rename(path.join("journal.json"), path.join("saved-journal.json")).unwrap();
        fs::create_dir(path.join("journal.json")).unwrap();
        assert!(
            journal
                .prepare_claim_assignment(session, key, assignment.clone())
                .is_err()
        );
        let intent = journal.data.prepared_assignments[0].launch_intent_id;
        assert!(intent.is_some());
        assert_eq!(journal.data.pending_claim_key, Some(key));
        let receipt = json!({
            "assignment_id": assignment["assignment_id"],
            "run_id": assignment["run_id"],
            "attempt_id": assignment["attempt_id"],
            "outcome":"succeeded", "completed_at":"2026-09-21T12:00:00Z"
        });
        journal.data.prepared_assignments[0].terminal_result =
            Some(TerminalResult::decode(json!({"receipt":receipt})).unwrap());
        journal.data.prepared_assignments[0].delivery_completion = Some(
            serde_json::from_value(json!({
                "assignment_id":assignment["assignment_id"],
                "run_id":assignment["run_id"],
                "attempt_id":assignment["attempt_id"],
                "source":"connector", "completed_at":"2026-09-21T12:00:00Z"
            }))
            .unwrap(),
        );
        journal.archive_completed().unwrap();
        assert_eq!(journal.data.prepared_assignments.len(), 1);
        assert!(!path.join("archive").exists());
        fs::remove_dir(path.join("journal.json")).unwrap();
        fs::rename(path.join("saved-journal.json"), path.join("journal.json")).unwrap();
        assert_eq!(journal.begin_claim(session).unwrap(), key);
        journal
            .prepare_claim_assignment(session, key, assignment.clone())
            .unwrap();
        assert_eq!(
            journal.data.prepared_assignments[0].launch_intent_id,
            intent
        );
        drop(journal);
        let journal = Journal::open(&path).unwrap();
        assert_eq!(
            journal.data.prepared_assignments[0].launch_intent_id,
            intent
        );
        assert!(journal.data.pending_claim_key.is_none());
        let mut journal = journal;
        journal.archive_completed().unwrap();
        assert!(journal.data.prepared_assignments.is_empty());
        let archived: Value = serde_json::from_slice(
            &fs::read(path.join("archive").join(format!(
                "{}.json",
                assignment["assignment_id"].as_str().unwrap()
            )))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(archived["entry"]["launch_intent_id"], json!(intent));
        drop(journal);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn journal_byte_limit_accepts_exactly_sixteen_mib_and_preserves_it_on_overflow() {
        let mut journal = fresh_journal("journal-byte-boundary");
        let path = journal.directory.clone();
        let mut assignment = assignment_for(&journal);
        assignment["definition"]["input_schema"]["description"] = json!("");
        journal.prepare_assignment(assignment.clone()).unwrap();
        let length = serde_json::to_vec_pretty(&journal.data).unwrap().len();
        assignment["definition"]["input_schema"]["description"] =
            json!("x".repeat(MAX_JOURNAL_BYTES as usize - length));
        journal.data.prepared_assignments[0].assignment =
            Assignment::decode(assignment.clone()).unwrap();
        assert_eq!(
            serde_json::to_vec_pretty(&journal.data).unwrap().len(),
            MAX_JOURNAL_BYTES as usize
        );
        journal.save().unwrap();
        let original = fs::read(path.join("journal.json")).unwrap();
        assignment["definition"]["input_schema"]["description"] = json!(format!(
            "{}x",
            assignment["definition"]["input_schema"]["description"]
                .as_str()
                .unwrap()
        ));
        journal.data.prepared_assignments[0].assignment = Assignment::decode(assignment).unwrap();
        assert!(journal.save().is_err());
        assert_eq!(fs::read(path.join("journal.json")).unwrap(), original);
        drop(journal);
        Journal::open(&path).unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn inspection_does_not_acquire_or_create_journal_ownership() {
        let path = std::env::temp_dir().join(format!("enumeray-inspection-{}", Uuid::new_v4()));
        let owner =
            Journal::initialize(&path, "http://127.0.0.1:8080".into(), "connector".into()).unwrap();
        let original = fs::read(path.join("journal.json")).unwrap();
        let (data, summary) = Journal::snapshot(&path).unwrap();
        assert_eq!(data.journal_id, owner.data.journal_id);
        assert_eq!(summary["journal_id"], json!(owner.data.journal_id));
        assert!(Journal::open(&path).is_err());
        drop(owner);
        fs::remove_file(path.join("owner.lock")).unwrap();
        Journal::snapshot(&path).unwrap();
        assert!(!path.join("owner.lock").exists());
        assert_eq!(fs::read(path.join("journal.json")).unwrap(), original);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn publication_and_context_references_are_required_and_preserved() {
        let document: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/journal-minimal-evidence.json"
        ))
        .unwrap();
        let data: JournalData = serde_json::from_value(document.clone()).unwrap();
        let mut assignment = document["prepared_assignments"][0]["assignment"].clone();
        validate_assignment(&assignment, &data).unwrap();
        assignment["execution_policy_id"] = json!("retired-policy");
        assert!(serde_json::from_value::<AssignmentReceipt>(assignment.clone()).is_err());
        assert!(validate_assignment(&assignment, &data).is_err());
        assignment
            .as_object_mut()
            .unwrap()
            .remove("execution_policy_id");
        assignment["publication_id"] = Value::Null;
        assignment["context_id"] = Value::Null;
        assert!(validate_assignment(&assignment, &data).is_err());
        assignment.as_object_mut().unwrap().remove("context_id");
        assignment["publication_id"] = json!("release-one");
        assert!(validate_assignment(&assignment, &data).is_err());
        assignment["context_id"] = json!("context-one");
        let receipt = validate_assignment(&assignment, &data).unwrap();
        assert_eq!(receipt.publication_id, "release-one");
        assert_eq!(receipt.context_id, "context-one");
        assignment.as_object_mut().unwrap().remove("publication_id");
        assert!(validate_assignment(&assignment, &data).is_err());
    }

    #[test]
    fn service_initialization_persists_transport_and_recovery_policy_atomically() {
        let path =
            std::env::temp_dir().join(format!("enumeray-service-journal-{}", Uuid::new_v4()));
        let journal = Journal::initialize_service(
            &path,
            "http://api:8080".into(),
            "connector".into(),
            true,
            true,
        )
        .unwrap();
        let id = journal.data.journal_id;
        drop(journal);
        let reopened = Journal::open(&path).unwrap();
        assert_eq!(reopened.data.version, 1);
        assert_eq!(reopened.data.journal_id, id);
        assert!(reopened.data.exclusive_local && reopened.data.allow_insecure_http);
        drop(reopened);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn archiving_a_full_journal_preserves_receipts_and_releases_preparation_capacity() {
        let path = std::env::temp_dir().join(format!("enumeray-full-journal-{}", Uuid::new_v4()));
        let mut journal = Journal::initialize(
            &path,
            "http://127.0.0.1:8080".into(),
            "test-connector".into(),
        )
        .unwrap();
        let fixture: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/journal-minimal-evidence.json"
        ))
        .unwrap();
        let mut assignment = fixture["prepared_assignments"][0]["assignment"].clone();
        assignment["journal_id"] = json!(journal.data.journal_id);
        for _ in 0..MAX_PREPARED_ASSIGNMENTS {
            for identity in ["assignment_id", "run_id", "attempt_id"] {
                assignment[identity] = json!(Uuid::new_v4());
            }
            // A reconciled terminal receipt needs no local backend launch.
            journal.data.prepared_assignments.push(
                serde_json::from_value(json!({
                    "assignment":assignment,
                    "launch_intent_id":null,
                    "submission_state":"unknown",
                    "delivery_completion":{"assignment_id":assignment["assignment_id"],"run_id":assignment["run_id"],"attempt_id":assignment["attempt_id"],"source":"connector","completed_at":"2026-09-21T12:00:00Z"},
                    "terminal_result":{"receipt":{
                        "assignment_id":assignment["assignment_id"],
                        "run_id":assignment["run_id"],
                        "attempt_id":assignment["attempt_id"],
                        "outcome":"succeeded",
                        "completed_at":"2026-09-21T12:00:00Z"
                    }}
                }))
                .unwrap(),
            );
        }
        journal.save().unwrap();
        drop(journal);
        let mut journal = Journal::open(&path).unwrap();
        assert!(journal.begin_session().is_err());
        let retained = serde_json::to_value(&journal.data.prepared_assignments[0]).unwrap();
        // Archive and queue publication survive a failed active-journal save.
        fs::rename(path.join("journal.json"), path.join("saved-journal.json")).unwrap();
        fs::create_dir(path.join("journal.json")).unwrap();
        assert!(journal.archive_completed().is_err());
        assert_eq!(
            journal.data.prepared_assignments.len(),
            MAX_PREPARED_ASSIGNMENTS
        );
        assert_eq!(
            fs::read_dir(path.join("cleanup/pending")).unwrap().count(),
            MAX_PREPARED_ASSIGNMENTS
        );
        fs::remove_dir(path.join("journal.json")).unwrap();
        fs::rename(path.join("saved-journal.json"), path.join("journal.json")).unwrap();
        journal.archive_completed().unwrap();
        assert!(journal.data.prepared_assignments.is_empty());
        assert_eq!(
            fs::read_dir(path.join("archive")).unwrap().count(),
            MAX_PREPARED_ASSIGNMENTS
        );
        let archived: Value = serde_json::from_slice(
            &fs::read(path.join("archive").join(format!(
                "{}.json",
                retained["assignment"]["assignment_id"].as_str().unwrap()
            )))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(archived["entry"], retained);
        journal.begin_session().unwrap();
        for identity in ["assignment_id", "run_id", "attempt_id"] {
            assignment[identity] = json!(Uuid::new_v4());
        }
        journal.prepare_assignment(assignment).unwrap();
        drop(journal);
        let journal = Journal::open(&path).unwrap();
        assert_eq!(journal.data.prepared_assignments.len(), 1);
        drop(journal);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn unsupported_versions_are_rejected_without_modification() {
        // Unknown versions must leave the retained evidence intact.
        let original: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/journal-minimal-evidence.json"
        ))
        .unwrap();
        for version in [0, 2, 6, u32::MAX] {
            let directory =
                std::env::temp_dir().join(format!("enumeray-legacy-{}", Uuid::new_v4()));
            let mut fixture = original.clone();
            fixture["version"] = json!(version);
            let journal = Journal::initialize(
                &directory,
                "http://127.0.0.1:8080".into(),
                "test-connector".into(),
            )
            .unwrap();
            drop(journal);
            fs::write(
                directory.join("journal.json"),
                serde_json::to_vec(&fixture).unwrap(),
            )
            .unwrap();
            assert!(Journal::open(&directory).is_err());
            let saved: Value =
                serde_json::from_slice(&fs::read(directory.join("journal.json")).unwrap()).unwrap();
            assert_eq!(saved, fixture);
            fs::remove_dir_all(directory).unwrap();
        }
    }
}
