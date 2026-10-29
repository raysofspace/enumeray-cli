use super::*;
pub(super) struct ProcessDriver;
impl ExecutionDriver for ProcessDriver {
    fn capabilities(&self) -> DriverCapabilities {
        DriverCapabilities {
            cancel_pending: false,
            cancel_running: true,
        }
    }
    fn validate(&self) -> Result<()> {
        Ok(())
    }
    fn submit(&self, directory: &Path, job: &ComputeJob) -> Result<crate::launch::LaunchOutcome> {
        use crate::launch::{FailureStage, LaunchOutcome, StartupFailure};
        Ok(match local_process::spawn_worker(directory) {
            Ok(()) => LaunchOutcome::Accepted,
            Err(error) => LaunchOutcome::ProvenFailure {
                evidence: Box::new(StartupFailure::new(
                    job.intent,
                    &job.assignment,
                    FailureStage::LaunchSpawn,
                    &error.message,
                    None,
                )?),
            },
        })
    }
    fn reconcile(&self, directory: &Path, job: &ComputeJob) -> Result<Option<ExecutionHandle>> {
        Ok(directory
            .join("launch-intent.json")
            .exists()
            .then_some(ExecutionHandle::Process { intent: job.intent }))
    }
    fn observe(
        &self,
        directory: &Path,
        job: &ComputeJob,
        handle: &ExecutionHandle,
    ) -> Result<Observation> {
        if !matches!(handle, ExecutionHandle::Process { intent } if *intent == job.intent) {
            return Err(failure(
                "process execution handle conflicts with launch identity",
            ));
        }
        if local_process::wrapper_started(directory, job.intent)? {
            return Ok(Observation::Running);
        }
        if directory.join("started").exists() {
            return Ok(Observation::Pending);
        }
        // A successful spawn precedes the worker's first marker. Keep observing
        // that accepted launch while the supervisor is waiting to be scheduled.
        let outcome = directory.join("launch-outcome.json");
        if outcome
            .try_exists()
            .map_err(|_| failure("cannot inspect process launch outcome"))?
            && matches!(
                local_process::read_record(&outcome)?,
                crate::launch::LaunchOutcome::Accepted
            )
        {
            return Ok(Observation::Pending);
        }
        Ok(Observation::Unresolved {
            reason: "process start not yet confirmed".into(),
        })
    }
    fn request_cancel(
        &self,
        _directory: &Path,
        _job: &ComputeJob,
        handle: &ExecutionHandle,
        scope: CancellationScope,
    ) -> Result<CancellationOutcome> {
        if !matches!(handle, ExecutionHandle::Process { .. }) {
            return Err(failure("process execution handle required"));
        }
        Ok(if scope == CancellationScope::Running {
            CancellationOutcome::Accepted
        } else {
            CancellationOutcome::Unsupported
        })
    }
}
