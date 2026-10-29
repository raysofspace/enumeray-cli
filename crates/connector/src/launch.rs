//! Private proof of a failed launch, independent of command transport errors.
use crate::{Result, failure, local_process};
use enumeray_contracts::execution::{ExecutionDriver, InstallationIdentity};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FailureStage {
    LaunchPreparation,
    LaunchSpawn,
    BackendRejection,
    WorkerValidation,
    WorkerPreparation,
    WorkerSpawn,
}

impl FailureStage {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::LaunchPreparation => "launch_preparation_failed",
            Self::LaunchSpawn => "launch_spawn_failed",
            Self::BackendRejection => "launch_rejected",
            Self::WorkerValidation => "worker_validation_failed",
            Self::WorkerPreparation => "worker_preparation_failed",
            Self::WorkerSpawn => "worker_spawn_failed",
        }
    }
    pub(crate) fn before_submission(self) -> bool {
        matches!(
            self,
            Self::LaunchPreparation | Self::LaunchSpawn | Self::BackendRejection
        )
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StartupFailure {
    pub(crate) intent: Uuid,
    attempt_id: Uuid,
    driver: ExecutionDriver,
    installation: InstallationIdentity,
    pub(crate) stage: FailureStage,
    pub(crate) observed_at: String,
    pub(crate) message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) command: Option<crate::connections::CommandDiagnostics>,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum LaunchOutcome {
    Accepted,
    ProvenFailure {
        evidence: Box<StartupFailure>,
    },
    Uncertain {
        command: crate::connections::CommandDiagnostics,
    },
}

impl StartupFailure {
    pub(crate) fn new(
        intent: Uuid,
        assignment: &Value,
        stage: FailureStage,
        message: &str,
        command: Option<crate::connections::CommandDiagnostics>,
    ) -> Result<Self> {
        let record = Self {
            intent,
            attempt_id: serde_json::from_value(assignment["attempt_id"].clone())
                .map_err(|_| failure("startup failure has no trusted attempt identity"))?,
            driver: serde_json::from_value(assignment["requirements"]["driver"].clone())
                .map_err(|_| failure("startup failure has no trusted driver identity"))?,
            installation: serde_json::from_value(assignment["descriptor"]["installation"].clone())
                .map_err(|_| failure("startup failure has no trusted installation identity"))?,
            stage,
            observed_at: OffsetDateTime::now_utc()
                .replace_nanosecond(0)
                .map_err(|_| failure("invalid startup observation time"))?
                .format(&Rfc3339)
                .map_err(|_| failure("invalid startup observation time"))?,
            message: message.to_owned(),
            command,
        };
        record.validate(intent, assignment)?;
        Ok(record)
    }

    pub(crate) fn validate(&self, intent: Uuid, assignment: &Value) -> Result<()> {
        if intent.is_nil()
            || self.intent != intent
            || self.attempt_id.is_nil()
            || serde_json::to_value(self.attempt_id).ok().as_ref()
                != Some(&assignment["attempt_id"])
            || serde_json::to_value(self.driver).ok().as_ref()
                != Some(&assignment["requirements"]["driver"])
            || serde_json::to_value(&self.installation).ok().as_ref()
                != Some(&assignment["descriptor"]["installation"])
            || OffsetDateTime::parse(&self.observed_at, &Rfc3339).is_err()
            || self.message.is_empty()
            || self.message.len() > 2048
            || self
                .message
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t')
            || (self.stage == FailureStage::BackendRejection
                && self.driver != ExecutionDriver::Slurm)
        {
            return Err(failure(
                "startup failure conflicts with pinned launch identity",
            ));
        }
        if let Some(command) = &self.command {
            command.validate()?;
            if self.stage == FailureStage::LaunchSpawn
                && !matches!(
                    command.state,
                    crate::connections::CommandState::NotSpawned { .. }
                )
            {
                return Err(failure("launch spawn failure has no command spawn proof"));
            }
        }
        if self.stage == FailureStage::BackendRejection
            && self
                .command
                .as_ref()
                .map(|c| c.rejected_message())
                .transpose()?
                .flatten()
                != Some(self.message.as_str())
        {
            return Err(failure(
                "scheduler rejection has no supported command proof",
            ));
        }
        Ok(())
    }

    pub(crate) fn validate_report(
        &self,
        intent: Uuid,
        assignment: &Value,
        report: &Value,
    ) -> Result<()> {
        self.validate(intent, assignment)?;
        if report["outcome"] != "failed"
            || report["execution_terminated"] != true
            || report["failure"]["code"] != self.stage.code()
            || report["failure"]["message"] != self.message
            || report["outputs"] != serde_json::json!({})
            || report["files"] != serde_json::json!({})
            || report["optional_output_failures"] != serde_json::json!({})
        {
            return Err(failure("startup failure conflicts with retained report"));
        }
        Ok(())
    }

    pub(crate) fn completion(&self, assignment: &Value) -> Result<local_process::Completion> {
        self.validate(self.intent, assignment)?;
        let mut completion =
            local_process::failure_completion(assignment, self.stage.code(), &self.message)?;
        completion.observed_at = self.observed_at.clone();
        completion.startup_evidence = Some(self.clone());
        Ok(completion)
    }
}

pub(crate) fn read_failure(
    directory: &Path,
    intent: Uuid,
    assignment: &Value,
) -> Result<Option<StartupFailure>> {
    let path = directory.join("startup-failure.json");
    let evidence: StartupFailure = if path
        .try_exists()
        .map_err(|_| failure("cannot inspect startup failure evidence"))?
    {
        local_process::read_record(&path)?
    } else {
        let outcome = directory.join("launch-outcome.json");
        if !outcome
            .try_exists()
            .map_err(|_| failure("cannot inspect launch outcome"))?
        {
            return Ok(None);
        }
        match local_process::read_record::<LaunchOutcome>(&outcome)? {
            LaunchOutcome::ProvenFailure { evidence } => *evidence,
            LaunchOutcome::Accepted => return Ok(None),
            LaunchOutcome::Uncertain { command } => {
                command.validate()?;
                return Ok(None);
            }
        }
    };
    evidence.validate(intent, assignment)?;
    let outcome = directory.join("launch-outcome.json");
    if outcome
        .try_exists()
        .map_err(|_| failure("cannot inspect launch outcome"))?
    {
        match local_process::read_record::<LaunchOutcome>(&outcome)? {
            LaunchOutcome::ProvenFailure { evidence: original } => {
                if serde_json::to_value(&evidence).ok() != serde_json::to_value(original).ok() {
                    return Err(failure(
                        "startup failure conflicts with retained launch outcome",
                    ));
                }
            }
            LaunchOutcome::Accepted if evidence.stage.before_submission() => {
                return Err(failure(
                    "pre-submission failure conflicts with launch acceptance",
                ));
            }
            LaunchOutcome::Uncertain { command } => {
                command.validate()?;
                if evidence.stage.before_submission() {
                    return Err(failure(
                        "pre-submission failure conflicts with uncertain submission",
                    ));
                }
            }
            _ => (),
        }
    }
    Ok(Some(evidence))
}

pub(crate) fn retain_failure(
    directory: &Path,
    evidence: &StartupFailure,
    assignment: &Value,
) -> Result<()> {
    evidence.validate(evidence.intent, assignment)?;
    local_process::write_record(directory, "startup-failure.json", evidence)?;
    if evidence.stage.before_submission() || assignment["requirements"]["driver"] == "process" {
        local_process::write_record(
            directory,
            "completion.json",
            &evidence.completion(assignment)?,
        )?;
    }
    Ok(())
}
