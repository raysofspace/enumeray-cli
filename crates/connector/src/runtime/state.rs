//! Read-only lifecycle projections over version-1 evidence, never saved states.
//!
//! Backend finalization, API settlement, delivery and payload cleanup are separate
//! facts. Missing history grants no launch or deletion authority. These views are
//! rebuilt after observations; callers must use the owning subsystem's checked
//! transitions before performing effects.

use crate::{
    execution::Execution,
    journal::PreparedAssignment,
    records::{Resolution, TerminalResult},
};
use enumeray_contracts::assignments::delivery::DeliveryCompletionReceipt;
use uuid::Uuid;

pub(crate) enum ExecutionView<'a> {
    MissingLaunchEvidence,
    Unsubmitted { intent: Uuid },
    LookupOnly { intent: Uuid },
    Finalized(&'a Execution),
}

pub(crate) fn execution(entry: &PreparedAssignment) -> ExecutionView<'_> {
    let Some(intent) = entry.launch_intent_id else {
        return ExecutionView::MissingLaunchEvidence;
    };
    if let Some(execution) = &entry.execution {
        ExecutionView::Finalized(execution)
    } else if entry.submission_state() == crate::journal::SubmissionState::DefinitelyUnsubmitted {
        ExecutionView::Unsubmitted { intent }
    } else {
        ExecutionView::LookupOnly { intent }
    }
}

pub(crate) enum SettlementView<'a> {
    Pending,
    ConnectorAcknowledged(&'a TerminalResult),
    Retrieved(&'a TerminalResult),
    OperatorResolved {
        result: &'a TerminalResult,
        resolution: &'a Resolution,
    },
}

pub(crate) fn settlement(entry: &PreparedAssignment) -> SettlementView<'_> {
    let Some(result) = &entry.terminal_result else {
        return SettlementView::Pending;
    };
    if let Some(resolution) = &entry.operator_resolution {
        SettlementView::OperatorResolved { result, resolution }
    } else if matches!(execution(entry), ExecutionView::Finalized(execution) if execution.acknowledged_receipt().is_some())
    {
        SettlementView::ConnectorAcknowledged(result)
    } else {
        SettlementView::Retrieved(result)
    }
}

pub(crate) fn needs_execution(entry: &PreparedAssignment) -> bool {
    matches!(settlement(entry), SettlementView::Pending)
}

pub(crate) fn allocation_released(entry: &PreparedAssignment) -> bool {
    entry
        .transfers
        .as_ref()
        .is_some_and(|state| state.allocation_released)
}

pub(crate) fn occupies_compute(entry: &PreparedAssignment) -> bool {
    // A retrieved terminal receipt does not prove that the original allocation
    // stopped. An audited resolution explicitly proves never-started/terminated
    // execution; ordinary work requires its independent release evidence.
    !allocation_released(entry)
        && !matches!(settlement(entry), SettlementView::OperatorResolved { .. })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeliveryBlocker {
    MissingLocalExecution,
    AllocationRelease,
    LiveLogs,
    Archive,
    PinnedPolicy,
}

impl DeliveryBlocker {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::MissingLocalExecution => "missing_local_execution_evidence",
            Self::AllocationRelease => "allocation_release_unconfirmed",
            Self::LiveLogs => "live_logs_pending",
            Self::Archive => "archive_pending",
            Self::PinnedPolicy => "pinned_policy_missing",
        }
    }
}

pub(crate) enum DeliveryView<'a> {
    NotTerminal,
    Blocked(DeliveryBlocker),
    Ready,
    Acknowledged(&'a DeliveryCompletionReceipt),
    Abandoned(&'a DeliveryCompletionReceipt),
}

/// `pinned_policy_present` is an observation, not permission to delete payloads.
pub(crate) fn delivery(
    entry: &PreparedAssignment,
    pinned_policy_present: bool,
) -> DeliveryView<'_> {
    if entry.terminal_result.is_none() {
        return DeliveryView::NotTerminal;
    }
    if let Some(receipt) = &entry.delivery_completion {
        return if receipt.source == "operator_abandonment" {
            DeliveryView::Abandoned(receipt)
        } else {
            DeliveryView::Acknowledged(receipt)
        };
    }
    let blocker = if entry
        .execution
        .as_ref()
        .and_then(Execution::acknowledged_receipt)
        .is_none()
    {
        Some(DeliveryBlocker::MissingLocalExecution)
    } else if !allocation_released(entry) {
        Some(DeliveryBlocker::AllocationRelease)
    } else if entry
        .logs
        .as_ref()
        .is_none_or(|logs| !logs.delivered(entry.assignment.decoded()))
    {
        Some(DeliveryBlocker::LiveLogs)
    } else if entry
        .log_archive
        .as_ref()
        .is_some_and(|archive| !archive.finished)
    {
        Some(DeliveryBlocker::Archive)
    } else if !pinned_policy_present {
        Some(DeliveryBlocker::PinnedPolicy)
    } else {
        None
    };
    blocker.map_or(DeliveryView::Ready, DeliveryView::Blocked)
}

/// Archiving retires active state, not cleanup authority. A connector receipt
/// still waits for local archive settlement; audited abandonment may retire it.
pub(crate) fn delivery_finished(entry: &PreparedAssignment) -> bool {
    match delivery(entry, false) {
        DeliveryView::Abandoned(receipt) => receipt.source == "operator_abandonment",
        DeliveryView::Acknowledged(receipt) => {
            receipt.source == "connector"
                && entry
                    .log_archive
                    .as_ref()
                    .is_none_or(|archive| archive.finished)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn fixture() -> Value {
        let data: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/journal-minimal-evidence.json"
        ))
        .unwrap();
        data["prepared_assignments"][0].clone()
    }

    #[test]
    fn execution_and_settlement_views_do_not_promote_incomplete_history() {
        for (submission, intent, finalized, terminal, expected) in [
            (None, false, false, false, "missing"),
            (None, true, false, false, "lookup"),
            (Some("unknown"), true, false, false, "lookup"),
            (
                Some("definitely_unsubmitted"),
                true,
                false,
                false,
                "unsubmitted",
            ),
            (Some("submitted"), true, true, false, "finalized"),
            (Some("unknown"), false, false, true, "missing"),
        ] {
            let mut value = fixture();
            value.as_object_mut().unwrap().remove("submission_state");
            if let Some(submission) = submission {
                value["submission_state"] = json!(submission);
            }
            if !intent {
                value["launch_intent_id"] = Value::Null;
            }
            if !finalized {
                value["execution"] = Value::Null;
            }
            if !terminal {
                value["terminal_result"] = Value::Null;
            }
            let entry: PreparedAssignment = serde_json::from_value(value).unwrap();
            let before = serde_json::to_value(&entry).unwrap();
            let observed = match execution(&entry) {
                ExecutionView::MissingLaunchEvidence => "missing",
                ExecutionView::Unsubmitted { .. } => "unsubmitted",
                ExecutionView::LookupOnly { .. } => "lookup",
                ExecutionView::Finalized(_) => "finalized",
            };
            assert_eq!(observed, expected);
            assert_eq!(needs_execution(&entry), !terminal);
            assert!(occupies_compute(&entry));
            assert!(!delivery_finished(&entry));
            assert_eq!(serde_json::to_value(&entry).unwrap(), before);
        }
    }

    #[test]
    fn retrieved_terminal_receipt_and_delivery_retirement_are_independent() {
        let mut value = fixture();
        value["execution"] = Value::Null;
        let mut entry: PreparedAssignment = serde_json::from_value(value).unwrap();
        assert!(matches!(settlement(&entry), SettlementView::Retrieved(_)));
        assert!(matches!(
            delivery(&entry, true),
            DeliveryView::Blocked(DeliveryBlocker::MissingLocalExecution)
        ));
        assert!(!needs_execution(&entry));
        assert!(!delivery_finished(&entry));
        let assignment = entry.assignment.decoded();
        entry.delivery_completion = Some(DeliveryCompletionReceipt {
            assignment_id: assignment.assignment_id,
            run_id: assignment.run_id,
            attempt_id: assignment.attempt_id,
            source: "operator_abandonment".into(),
            completed_at: "2026-09-21T12:00:00Z".into(),
        });
        assert!(delivery_finished(&entry));
        // Abandoning delivery retires active bookkeeping without inventing
        // local execution or changing uncertainty about the backend.
        assert!(matches!(
            execution(&entry),
            ExecutionView::LookupOnly { .. }
        ));
        assert!(entry.execution.is_none());
    }

    #[test]
    fn acknowledged_delivery_still_waits_for_local_archive_settlement() {
        let mut entry: PreparedAssignment = serde_json::from_value(fixture()).unwrap();
        let assignment = entry.assignment.decoded();
        entry.delivery_completion = Some(DeliveryCompletionReceipt {
            assignment_id: assignment.assignment_id,
            run_id: assignment.run_id,
            attempt_id: assignment.attempt_id,
            source: "connector".into(),
            completed_at: "2026-09-21T12:00:00Z".into(),
        });
        entry.log_archive = Some(Default::default());
        assert!(!delivery_finished(&entry));
        entry.log_archive.as_mut().unwrap().finished = true;
        assert!(delivery_finished(&entry));
    }
}
