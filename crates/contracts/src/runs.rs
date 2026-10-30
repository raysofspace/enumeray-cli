use std::{
    error::Error,
    fmt,
    time::{Duration, SystemTime},
};

use serde::{Deserialize, Serialize};

/// Immutable submission intent. Candidate trials retain author/compute authority checks.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunPurpose {
    #[default]
    Standard,
    CandidateTrial,
}

impl RunPurpose {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::CandidateTrial => "candidate_trial",
        }
    }

    /// Omitting standard intent preserves existing prepared operations and fingerprints.
    pub fn is_standard(&self) -> bool {
        *self == Self::Standard
    }
}

/// Public execution state. Delegated preparation and collection are `running`.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl RunState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Monotonic progress within an assignment, not proof that an outcome is final.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunPhase {
    Preparing,
    WaitingForBackend,
    Executing,
    CollectingResults,
    UploadingResults,
}

impl RunPhase {
    fn progress_order(self) -> u8 {
        match self {
            Self::Preparing => 0,
            Self::WaitingForBackend => 1,
            Self::Executing => 2,
            Self::CollectingResults => 3,
            Self::UploadingResults => 4,
        }
    }
}

/// Final public outcome after application-level validation and reconciliation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalOutcome {
    Succeeded,
    Failed,
    Cancelled,
}

/// A queued run cannot currently be assigned through an approved execution path.
///
/// Capacity exhaustion and temporary disconnection are ordinary queue waiting.
/// Closing new admission does not block already accepted runs.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueBlockReason {
    PublicationRevoked,
    InstallationRevoked,
    NoApprovedExecutionPath,
    ModelAccessRevoked,
    ComputeAccessRevoked,
    ComputeUnavailable,
    ConnectorMaintenance,
    /// This context is cancelling its previously accepted queued work during cutover.
    ContextCutover,
}

/// Whether a valid operation changed lifecycle state or repeated its current value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionChange {
    Changed,
    Unchanged,
}

/// Only queued runs carry queue diagnostics, and only running runs carry a phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Queued {
        blocked_reason: Option<QueueBlockReason>,
    },
    Running(RunPhase),
    Terminal(TerminalOutcome),
}

/// Lifecycle state whose invariants can only be changed through checked methods.
///
/// It deliberately has no deserialization implementation. Future persistence and
/// HTTP code must not construct arbitrary state/phase/outcome combinations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunLifecycle {
    stage: Stage,
}

impl RunLifecycle {
    /// Initial state after acceptance; it does not imply an assignment exists.
    pub fn queued() -> Self {
        Self {
            stage: Stage::Queued {
                blocked_reason: None,
            },
        }
    }

    pub fn state(&self) -> RunState {
        match self.stage {
            Stage::Queued { .. } => RunState::Queued,
            Stage::Running(_) => RunState::Running,
            Stage::Terminal(TerminalOutcome::Succeeded) => RunState::Succeeded,
            Stage::Terminal(TerminalOutcome::Failed) => RunState::Failed,
            Stage::Terminal(TerminalOutcome::Cancelled) => RunState::Cancelled,
        }
    }

    pub fn phase(&self) -> Option<RunPhase> {
        match self.stage {
            Stage::Running(phase) => Some(phase),
            _ => None,
        }
    }

    pub fn outcome(&self) -> Option<TerminalOutcome> {
        match self.stage {
            Stage::Terminal(outcome) => Some(outcome),
            _ => None,
        }
    }

    pub fn blocked_reason(&self) -> Option<QueueBlockReason> {
        match self.stage {
            Stage::Queued { blocked_reason } => blocked_reason,
            _ => None,
        }
    }

    /// Record or clear a queue diagnostic after checking execution eligibility.
    /// This does not alter the run's accepted publication, inputs, or requirements.
    pub fn set_queue_blocked(
        &mut self,
        reason: Option<QueueBlockReason>,
    ) -> Result<TransitionChange, TransitionError> {
        let Stage::Queued { blocked_reason } = self.stage else {
            return Err(TransitionError::NotQueued);
        };
        if blocked_reason == reason {
            return Ok(TransitionChange::Unchanged);
        }
        self.stage = Stage::Queued {
            blocked_reason: reason,
        };
        Ok(TransitionChange::Changed)
    }

    /// Enter delegated preparation as part of a future atomic assignment claim.
    ///
    /// The caller must recheck approval and limits in that transaction. A cleared
    /// diagnostic alone does not establish eligibility. Claim retries retrieve
    /// their existing assignment; calling this again cannot restart a run.
    pub fn assign(&mut self) -> Result<TransitionChange, TransitionError> {
        match self.stage {
            Stage::Queued {
                blocked_reason: None,
            } => {
                self.stage = Stage::Running(RunPhase::Preparing);
                Ok(TransitionChange::Changed)
            }
            Stage::Queued { .. } => Err(TransitionError::QueueBlocked),
            _ => Err(TransitionError::NotQueued),
        }
    }

    /// Apply confirmed progress. Sparse reports may skip intermediate phases.
    ///
    /// Repeated phases are harmless. A late observation or transfer retry cannot
    /// return to an earlier phase. Event identity and ordering are checked by the
    /// future observation-ingestion operation, not by comparing these phases.
    pub fn advance_phase(
        &mut self,
        requested: RunPhase,
    ) -> Result<TransitionChange, TransitionError> {
        let Stage::Running(current) = self.stage else {
            return Err(TransitionError::NotRunning);
        };
        if requested == current {
            return Ok(TransitionChange::Unchanged);
        }
        if requested.progress_order() < current.progress_order() {
            return Err(TransitionError::PhaseRegression { current, requested });
        }
        self.stage = Stage::Running(requested);
        Ok(TransitionChange::Changed)
    }

    /// Finalize an authoritative outcome; an outcome can never be replaced.
    ///
    /// Success requires a collection phase AND application-verified required
    /// results, including scalar validation when no files are required. This
    /// module checks the phase only; it cannot verify results or backend evidence.
    /// Failure/cancellation after assignment requires confirmed termination or
    /// explicit safe resolution, never merely a request, timeout, or lost contact.
    ///
    /// An identical outcome is a lifecycle no-op. The application must separately
    /// compare terminal report contents before acknowledging an identical retry.
    pub fn finalize(
        &mut self,
        requested: TerminalOutcome,
    ) -> Result<TransitionChange, TransitionError> {
        if let Stage::Terminal(current) = self.stage {
            return if requested == current {
                Ok(TransitionChange::Unchanged)
            } else {
                Err(TransitionError::TerminalOutcomeConflict { current, requested })
            };
        }
        if requested == TerminalOutcome::Succeeded
            && !matches!(
                self.stage,
                Stage::Running(RunPhase::CollectingResults | RunPhase::UploadingResults)
            )
        {
            return Err(TransitionError::SuccessBeforeCollection);
        }
        self.stage = Stage::Terminal(requested);
        Ok(TransitionChange::Changed)
    }
}

/// Lifecycle rejection; every rejected operation leaves the run unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionError {
    NotQueued,
    QueueBlocked,
    NotRunning,
    PhaseRegression {
        current: RunPhase,
        requested: RunPhase,
    },
    SuccessBeforeCollection,
    TerminalOutcomeConflict {
        current: TerminalOutcome,
        requested: TerminalOutcome,
    },
}

impl fmt::Display for TransitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NotQueued => "operation requires a queued run",
            Self::QueueBlocked => "run has no approved execution path",
            Self::NotRunning => "phase observations require a running run",
            Self::PhaseRegression { .. } => "run progress cannot move to an earlier phase",
            Self::SuccessBeforeCollection => "run success requires result collection",
            Self::TerminalOutcomeConflict { .. } => "a terminal run cannot change its outcome",
        })
    }
}

impl Error for TransitionError {}

/// Recency of confirmed run observations, independent of execution and connectivity.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationFreshness {
    Unknown,
    Fresh,
    Stale,
}

impl ObservationFreshness {
    /// Classify using API confirmation time, never a connector-supplied clock.
    ///
    /// The caller records this time only for an accepted current run observation;
    /// a connector heartbeat or duplicate/stale report does not refresh it.
    /// Missing or future confirmation times are unknown (including clock rollback).
    /// At the threshold the observation is stale; a zero threshold is immediately
    /// stale. Configuration of a positive operational threshold arrives later.
    pub fn classify(
        last_confirmed_at: Option<SystemTime>,
        now: SystemTime,
        stale_after: Duration,
    ) -> Self {
        let Some(age) = last_confirmed_at.and_then(|time| now.duration_since(time).ok()) else {
            return Self::Unknown;
        };
        if age < stale_after {
            Self::Fresh
        } else {
            Self::Stale
        }
    }
}
