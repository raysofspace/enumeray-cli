//! Runner-side job lifecycles. Connections and API credentials never enter this module.
mod process;
pub(crate) mod slurm;
use crate::compute::ComputeJob;
use crate::{Result, failure, local_process};
use enumeray_contracts::execution::DriverConfig;
use serde::{Deserialize, Serialize};
use std::{path::Path, time::Duration};
use uuid::Uuid;

#[derive(Debug)]
pub(crate) struct DriverCapabilities {
    pub cancel_pending: bool,
    pub cancel_running: bool,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CancellationScope {
    Pending,
    /// User cancellation includes pending/starting work as well as running work.
    Running,
}
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum CancellationOutcome {
    Accepted,
    Unsupported,
    AlreadyTerminal,
    Unresolved { reason: String },
}
/// Resolved from durable launch evidence, never supplied by a cancellation caller.
pub(crate) enum ExecutionHandle {
    Process { intent: Uuid },
    Slurm(SlurmHandle),
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Observation {
    Pending,
    Running,
    Unresolved {
        reason: String,
    },
    /// Consumed by the common runner before encoding a protocol response.
    Stopped {
        evidence: Box<slurm::SchedulerEvidence>,
    },
}
pub(crate) trait ExecutionDriver {
    fn capabilities(&self) -> DriverCapabilities;
    fn validate(&self) -> Result<()>;
    /// Publish durable launch identity before any backend submission.
    fn prepare_launch(&self, directory: &Path, job: &ComputeJob) -> Result<()> {
        local_process::write_record(
            directory,
            "launch-intent.json",
            &serde_json::json!({"intent":job.intent}),
        )
    }
    fn submit(&self, directory: &Path, job: &ComputeJob) -> Result<crate::launch::LaunchOutcome>;
    /// Recover and persist the original backend identity without submitting again.
    fn reconcile(&self, directory: &Path, job: &ComputeJob) -> Result<Option<ExecutionHandle>>;
    /// Read backend evidence. Observation must never request cancellation or write records.
    fn observe(
        &self,
        directory: &Path,
        job: &ComputeJob,
        handle: &ExecutionHandle,
    ) -> Result<Observation>;
    /// Acceptance does not prove termination. Repeating a request must be safe,
    /// including when the backend accepted it but its reply was lost.
    fn request_cancel(
        &self,
        directory: &Path,
        job: &ComputeJob,
        handle: &ExecutionHandle,
        scope: CancellationScope,
    ) -> Result<CancellationOutcome>;
}
pub(crate) fn resolve(config: &DriverConfig) -> &'static dyn ExecutionDriver {
    match config {
        DriverConfig::Process { .. } => &process::ProcessDriver,
        DriverConfig::Slurm(_) => &slurm::SlurmDriver,
    }
}
fn safe_scheduler_name(s: &str) -> bool {
    !s.is_empty()
        && !s.eq_ignore_ascii_case("all")
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct SlurmHandle {
    pub(crate) job_id: u64,
    pub(crate) cluster: String,
}
