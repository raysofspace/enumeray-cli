//! One journal owner, bounded round-robin assignment execution, and independent heartbeats.

use super::{ErrorKind, Result, client::Client, failure, journal::Journal};
use clap::{Args, ValueEnum};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Recovery {
    Manual,
    ExclusiveLocal,
}

#[derive(Args)]
pub struct Options {
    #[arg(long)]
    journal: PathBuf,
    /// Connector-owned capture/archive policy, pinned before launch.
    #[arg(long)]
    log_policy_file: Option<PathBuf>,
    #[arg(long)]
    api_base_url: String,
    #[arg(long)]
    connector_id: String,
    #[arg(long)]
    token_file: PathBuf,
    /// Private credential-reference map; never sent to Enumeray.
    #[arg(long)]
    credentials_file: Option<PathBuf>,
    /// Bound assignments reserving compute capacity; Slurm queue entries count too.
    #[arg(long,default_value_t=1,value_parser=clap::value_parser!(u32).range(1..=64))]
    max_workers: u32,
    /// Bound simultaneous bulk I/O subprocesses independently of compute work.
    #[arg(long,default_value_t=2,value_parser=clap::value_parser!(u32).range(1..=64))]
    max_transfer_workers: u32,
    /// Persistent budget for scientific input/output transfer copies.
    #[arg(long,default_value_t=1099511627776_u64,value_parser=clap::value_parser!(u64).range(1..))]
    max_spool_bytes: u64,
    /// Bound all retained unfinished assignments, including unpublished outputs.
    #[arg(long,default_value_t=64,value_parser=clap::value_parser!(u32).range(1..=64))]
    max_pending_assignments: u32,
    /// Optional retention deadline for unpublished finalized outputs; omission retains indefinitely.
    #[arg(long,value_parser=clap::value_parser!(u64).range(1..=315360000))]
    unpublished_retention_seconds: Option<u64>,
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u64).range(1..=300))]
    poll_interval_seconds: u64,
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..=3600))]
    shutdown_timeout_seconds: u64,
    /// Cadence for bounded, low-priority completed payload cleanup.
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..=3600))]
    cleanup_interval_seconds: u64,
    /// Optional debugging grace after safe completion; zero cleans immediately.
    #[arg(long, default_value_t = 0)]
    completed_retention_seconds: u64,
    /// Only for one installation on an unshared, uncloned local volume.
    #[arg(long, value_enum, default_value = "manual")]
    recovery_mode: Recovery,
    /// Confirm an unclosed manual-recovery session's previous owner stopped everywhere.
    #[arg(long)]
    confirm_prior_process_stopped: bool,
    /// Permit bearer credentials over HTTP on a trusted container/private network.
    #[arg(long)]
    allow_insecure_http: bool,
    /// Permit trusted process/direct development contexts under this regular OS user.
    #[arg(long)]
    allow_development_execution: bool,
}

fn log(status: &str, journal: &Journal, message: Option<&str>) {
    let assignment = journal
        .data
        .prepared_assignments
        .first()
        .map(|entry| entry.assignment.decoded());
    eprintln!(
        "{}",
        json!({"status":status, "connector_id":journal.data.connector_id,
        "journal_id":journal.data.journal_id, "assignment_id":assignment.map(|a|a.assignment_id),
        "run_id":assignment.map(|a|a.run_id), "message":message})
    );
}

pub fn run(options: Options) -> Result<Value> {
    if options.max_pending_assignments < options.max_workers {
        return Err(failure(
            "max-pending-assignments must be at least max-workers",
        ));
    }
    let origin =
        super::client::validate_origin(&options.api_base_url, options.allow_insecure_http)?;
    let mut journal = if options
        .journal
        .join("journal.json")
        .try_exists()
        .map_err(|_| failure("cannot inspect journal"))?
    {
        Journal::open(&options.journal)?
    } else {
        Journal::initialize_service(
            &options.journal,
            origin.clone(),
            options.connector_id.clone(),
            options.recovery_mode == Recovery::ExclusiveLocal,
            options.allow_insecure_http,
        )?
    };
    if journal.data.api_base_url != origin || journal.data.connector_id != options.connector_id {
        return Err(failure(
            "service configuration conflicts with the existing journal identity",
        ));
    }
    let exclusive = options.recovery_mode == Recovery::ExclusiveLocal;
    if journal.data.active_session_id.is_some()
        && !(options.confirm_prior_process_stopped || (exclusive && journal.data.exclusive_local))
    {
        return Err(failure(
            "recovery requires --confirm-prior-process-stopped for a manual-recovery journal",
        ));
    }
    journal.configure_service(exclusive, options.allow_insecure_http)?;
    let mut client = Client::configured(
        &origin,
        Some(&options.token_file),
        options.allow_insecure_http,
    )?;
    client.allow_development_execution = options.allow_development_execution;
    journal.record_execution_policy(options.allow_development_execution)?;
    crate::execution_policy::warn(options.allow_development_execution);
    client.log_policy = crate::capture::load_policy(options.log_policy_file.as_deref())?;
    client.service = true;
    client.unpublished_retention_seconds = options.unpublished_retention_seconds;
    let mut transfers = crate::transfers::Pool::new(
        options.token_file.clone(),
        options.max_transfer_workers as usize,
    )
    .with_spool_limit(options.max_spool_bytes)
    .with_budgets(enumeray_contracts::transfers::TransferBudgets::from_env().map_err(failure)?);
    let _signals = signals(&client, options.shutdown_timeout_seconds)?;
    let (heartbeat, failures, ready) = Heartbeat::start(client.clone());
    let _heartbeat = heartbeat;
    log("starting", &journal, None);
    let mut ownership = OwnershipPhase::Acquiring;
    let mut next_reconcile = Instant::now();
    let mut backoff = 1;
    let mut last_status = String::new();
    let mut draining = false;
    let mut configuration = ConfigurationState::Unavailable;
    let (configuration_worker, configuration_results) = ConfigurationWorker::start(
        client.clone(),
        journal.directory.clone(),
        options.credentials_file.clone(),
    );
    let mut cleanup_blockers = BTreeMap::new();
    let mut cleanup_failure = None;
    let (cleanup_worker, cleanup_results) = CleanupWorker::start(
        journal.directory.clone(),
        journal.data.journal_id,
        journal.data.connector_id.clone(),
        options.completed_retention_seconds,
    );
    let mut next_cleanup = Instant::now();
    let preflight_worker = crate::preflight_worker::Worker::start(
        client.clone(),
        journal.directory.clone(),
        crate::preflight::HostEnvironment {
            account: String::new(),
            executable: std::env::current_exe()
                .map_err(|_| failure("cannot inspect connector executable"))?
                .to_string_lossy()
                .into_owned(),
            journal: journal.directory.to_string_lossy().into_owned(),
            token_file: options.token_file.to_string_lossy().into_owned(),
            credentials_file: options
                .credentials_file
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
            allow_development_execution: options.allow_development_execution,
            service_file: None,
        },
    );
    let mut next_preflight = Instant::now();
    let mut next_claim = Instant::now();
    let mut next_delivery = Instant::now();
    let mut delivery_failure = None;
    let mut delivery_blockers = BTreeMap::new();
    let mut assignment_cursor = 0_usize;
    let poll = Duration::from_secs(options.poll_interval_seconds);
    loop {
        if let Ok(error) = failures.try_recv()
            && error.kind != ErrorKind::Stopping
        {
            return Err(error);
        }
        let deadline = *client.shutdown.lock().expect("shutdown lock");
        if deadline.is_some() && !draining {
            log("draining", &journal, None);
            draining = true;
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            transfers.stop_all();
            log(
                "drain_expired",
                &journal,
                Some("journal retained for recovery"),
            );
            return Ok(json!({"status":"stopped", "drained":false}));
        }
        let outcome = (|| -> Result<(String, Duration)> {
            if matches!(ownership, OwnershipPhase::Acquiring) {
                if deadline.is_some() {
                    return Err(failure("stopping during startup").with_kind(ErrorKind::Stopping));
                }
                // The initial ownership assertion was checked above. Subsequent
                // retries recover only this process's own registration requests.
                ownership = OwnershipPhase::Reconciling(
                    client.acquire_recovery_session(&mut journal, true)?,
                );
            }
            let session = match ownership {
                OwnershipPhase::Reconciling(session) | OwnershipPhase::Ready(session) => session,
                OwnershipPhase::Acquiring => unreachable!("registered ownership"),
            };
            if deadline.is_none()
                && matches!(ownership, OwnershipPhase::Ready(_))
                && Instant::now() >= next_preflight
            {
                preflight_worker.schedule(session);
                next_preflight = Instant::now() + poll;
            }
            if matches!(ownership, OwnershipPhase::Reconciling(_)) {
                client.reconcile_inventory(&mut journal)?;
                // Recovery fenced the old session. Never replay its pending claim.
                journal.inventory_reconciled(session)?;
                ownership = OwnershipPhase::Ready(session);
            }
            if deadline.is_some() && journal.data.pending_claim_key.is_some() {
                // Retrying an uncertain claim could delegate new work while
                // draining. Fence this process's old session, then retrieve its
                // inventory under the replacement owner before discarding the
                // key. Any already-delegated work remains available to drain.
                ownership = OwnershipPhase::Reconciling(
                    client.acquire_recovery_session(&mut journal, true)?,
                );
                return Ok(("draining".into(), Duration::ZERO));
            }
            while let Ok(outcome) = configuration_results.try_recv() {
                match outcome {
                    ConfigurationOutcome::Prepared(Ok(prepared)) => {
                        configuration = ConfigurationState::Unavailable;
                        let committed =
                            super::control::apply_configuration(&journal.directory, *prepared);
                        if let Ok(committed) = &committed {
                            configuration = ConfigurationState::AwaitingAcknowledgment {
                                generation: committed.generation(),
                                path: committed.path().to_owned(),
                            };
                        }
                        if let Err(error) = &committed {
                            log("configuration_unavailable", &journal, Some(&error.message));
                        }
                        if let Ok(diagnostic) = super::control::summary(&journal.directory) {
                            let blocker = diagnostic["release_protection_blocker"]
                                .as_str()
                                .filter(|_| diagnostic["release_protection_ready"] == false);
                            if let Some(blocker) = blocker {
                                if cleanup_failure.as_deref() != Some(blocker) {
                                    log("cleanup_blocked", &journal, Some(&diagnostic.to_string()));
                                }
                                cleanup_failure = Some(blocker.to_owned());
                            } else {
                                cleanup_failure = None;
                            }
                        }
                        configuration_worker.acknowledge(committed);
                    }
                    ConfigurationOutcome::Acknowledged(Ok(acknowledged)) => {
                        configuration.acknowledge(acknowledged)?;
                    }
                    ConfigurationOutcome::Prepared(Err(error))
                    | ConfigurationOutcome::Acknowledged(Err(error)) => {
                        configuration = ConfigurationState::Unavailable;
                        log("configuration_unavailable", &journal, Some(&error.message));
                    }
                }
            }
            while let Ok(result) = cleanup_results.try_recv() {
                match result {
                    Ok(report) => {
                        cleanup_failure = None;
                        for diagnostic in changed_cleanup_blockers(&report, &mut cleanup_blockers) {
                            log("cleanup_blocked", &journal, Some(&diagnostic));
                        }
                        if report["newly_completed"].as_u64().unwrap_or(0) > 0 {
                            log("cleanup_completed", &journal, Some(&report.to_string()));
                        }
                    }
                    Err(error) => {
                        if cleanup_failure.as_deref() != Some(&error.message) {
                            log("cleanup_blocked", &journal, Some(&error.message));
                        }
                        cleanup_failure = Some(error.message);
                    }
                }
            }
            transfers.tick(&mut journal)?;
            let current_delivery_blockers: BTreeMap<_, _> = journal
                .data
                .prepared_assignments
                .iter()
                .filter_map(|entry| {
                    super::execution::delivery_blocker(entry, &journal.directory)
                        .map(|reason| (entry.assignment.decoded().assignment_id, reason))
                })
                .collect();
            for (assignment, reason) in &current_delivery_blockers {
                if delivery_blockers.get(assignment) != Some(reason) {
                    log("delivery_blocked", &journal, Some(&json!({"assignment_id":assignment,"blocker":reason,"next_action":super::execution::delivery_next_action(reason)}).to_string()));
                }
            }
            delivery_blockers = current_delivery_blockers;
            if Instant::now() >= next_delivery {
                next_delivery = Instant::now() + poll;
                match super::execution::complete_deliveries(&client, &mut journal, session) {
                    Ok(()) => delivery_failure = None,
                    Err(error) if error.kind == ErrorKind::Fatal => return Err(error),
                    Err(error) => {
                        if delivery_failure.as_ref() != Some(&error.message) {
                            log("delivery_pending", &journal, Some(&error.message));
                        }
                        delivery_failure = Some(error.message);
                    }
                }
            }
            journal.archive_completed()?;
            if deadline.is_none() && configuration.ready() && Instant::now() >= next_cleanup {
                if super::cleanup::protection_ready(&journal.directory) {
                    cleanup_worker.schedule();
                }
                next_cleanup =
                    Instant::now() + Duration::from_secs(options.cleanup_interval_seconds);
            }
            transfers.schedule(&mut journal, session)?;
            let outstanding = journal
                .data
                .prepared_assignments
                .iter()
                .any(|entry| !entry.delivery_finished());
            if !outstanding && deadline.is_some() {
                client.release_session(&mut journal, session)?;
                return Ok(("stopped".into(), Duration::ZERO));
            }
            let active_count = journal
                .data
                .prepared_assignments
                .iter()
                .filter(|entry| crate::state::occupies_compute(entry))
                .count();
            // Resolving an existing request consumes no additional admission
            // authority. A retained candidate can already occupy the last slot.
            if (journal.data.pending_claim_key.is_some()
                || journal.data.prepared_assignments.len()
                    < options.max_pending_assignments as usize
                    && active_count < options.max_workers as usize)
                && deadline.is_none()
                && configuration.ready()
                && ready.load(Ordering::Acquire)
                && Instant::now() >= next_claim
            {
                let claimed = client.claim(&mut journal, session)?;
                next_claim = Instant::now() + poll;
                if claimed["status"] != "empty" {
                    return Ok(("claimed".into(), Duration::ZERO));
                }
            }
            if outstanding {
                if Instant::now() >= next_reconcile {
                    client.reconcile_inventory(&mut journal)?;
                    next_reconcile = Instant::now() + Duration::from_secs(30);
                }
                let mut unfinished = Vec::new();
                for (index, entry) in journal.data.prepared_assignments.iter().enumerate() {
                    if crate::state::needs_execution(entry)
                        && (configuration.ready()
                            || entry
                                .transfers
                                .as_ref()
                                .is_some_and(|state| state.installation.is_some())
                            || entry.submission_state()
                                != super::journal::SubmissionState::DefinitelyUnsubmitted
                            || client
                                .assignment_control(entry.assignment.decoded().assignment_id)?
                                .cancel_running)
                    {
                        unfinished.push(index);
                    }
                }
                if unfinished.is_empty() {
                    if !configuration.ready() {
                        return Ok(("waiting_for_configuration".into(), poll));
                    }
                    next_claim = Instant::now();
                    let status = if delivery_blockers
                        .values()
                        .any(|reason| *reason != "archive_pending")
                    {
                        "delivery_blocked"
                    } else if journal.data.prepared_assignments.iter().any(|entry| {
                        entry
                            .log_archive
                            .as_ref()
                            .is_some_and(|archive| !archive.finished)
                    }) {
                        "publishing_archives"
                    } else {
                        "waiting_for_delivery"
                    };
                    return Ok((status.into(), poll));
                }
                let index = unfinished[assignment_cursor % unfinished.len()];
                assignment_cursor = assignment_cursor.wrapping_add(1);
                let id = journal.data.prepared_assignments[index]
                    .assignment
                    .decoded()
                    .assignment_id;
                if transfers.is_active(id) {
                    let expired = journal.data.prepared_assignments[index]
                        .transfers
                        .as_ref()
                        .is_some_and(|s| {
                            s.expires_at.is_some_and(|t| {
                                time::OffsetDateTime::now_utc().unix_timestamp() >= t
                            })
                        });
                    if expired
                        || client.assignment_control(id)?.cancel_running
                            && journal.data.prepared_assignments[index].execution.is_none()
                    {
                        transfers.cancel(id);
                        crate::transfers::interrupt(&mut journal.data.prepared_assignments[index])?;
                        journal.save()?;
                    }
                }
                let result = super::execution::execute_at(
                    &client,
                    &mut journal,
                    session,
                    false,
                    configuration.installations(),
                    index,
                )?;
                let status = if result["status"] == "completed" {
                    transfers.cancel(id);
                    next_claim = Instant::now();
                    "completed"
                } else {
                    "executing"
                };
                Ok((status.into(), Duration::ZERO))
            } else if !configuration.ready() {
                Ok(("waiting_for_configuration".into(), poll))
            } else if ready.load(Ordering::Acquire) {
                Ok(("idle".into(), poll))
            } else {
                Ok(("waiting_for_api".into(), poll))
            }
        })();
        let (status, delay, message) = match outcome {
            Ok((status, delay)) => {
                backoff = 1;
                (status, delay, None)
            }
            Err(error) => match error.kind {
                ErrorKind::Fatal => return Err(error),
                ErrorKind::Stopping => return Ok(json!({"status":"stopped", "drained":false})),
                ErrorKind::Waiting => {
                    backoff = 1;
                    ("executing".into(), Duration::from_millis(25), None)
                }
                ErrorKind::Retry => {
                    let delay = Duration::from_secs(backoff);
                    backoff = (backoff * 2).min(30);
                    (
                        "retrying".into(),
                        if journal.data.prepared_assignments.len() > 1 {
                            Duration::from_millis(25)
                        } else {
                            delay
                        },
                        Some(error.message),
                    )
                }
                ErrorKind::Blocked => {
                    next_reconcile = Instant::now();
                    (
                        "blocked".into(),
                        Duration::from_millis(250),
                        Some(error.message),
                    )
                }
            },
        };
        if status != last_status {
            log(&status, &journal, message.as_deref());
            last_status = status.clone();
        }
        if status == "stopped" {
            return Ok(json!({"status":"stopped", "drained":true}));
        }
        // Wake immediately when a new stop signal starts draining, and at its
        // deadline even while API backoff or operator reconciliation is pending.
        let end = Instant::now() + delay;
        while Instant::now() < end {
            let current = *client.shutdown.lock().expect("shutdown lock");
            if current != deadline || current.is_some_and(|d| Instant::now() >= d) {
                break;
            }
            thread::sleep(
                Duration::from_millis(25).min(end.saturating_duration_since(Instant::now())),
            );
        }
    }
}

fn signals(client: &Client, seconds: u64) -> Result<tokio::runtime::Runtime> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .map_err(|_| failure("cannot initialize signal handling"))?;
    let shutdown = client.shutdown.clone();
    #[cfg(unix)]
    {
        let _entered = runtime.enter();
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .map_err(|_| failure("cannot register SIGTERM"))?;
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                .map_err(|_| failure("cannot register SIGINT"))?;
        runtime.spawn(async move {
            tokio::select! { _ = terminate.recv() => (), _ = interrupt.recv() => () }
            *shutdown.lock().expect("shutdown lock") =
                Some(Instant::now() + Duration::from_secs(seconds));
        });
    }
    Ok(runtime)
}

struct Heartbeat {
    stop: mpsc::Sender<()>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Heartbeat {
    fn start(client: Client) -> (Self, mpsc::Receiver<super::ConnectorError>, Arc<AtomicBool>) {
        let (stop, stopped) = mpsc::channel();
        let (errors, failures) = mpsc::channel();
        let ready = Arc::new(AtomicBool::new(false));
        let worker_ready = ready.clone();
        let worker = thread::spawn(move || {
            let mut backoff = 1;
            loop {
                let delay = match client.heartbeat() {
                    Ok(interval) => {
                        worker_ready.store(true, Ordering::Release);
                        backoff = 1;
                        interval
                    }
                    Err(error) if error.kind == ErrorKind::Retry => {
                        worker_ready.store(false, Ordering::Release);
                        let delay = Duration::from_secs(backoff);
                        backoff = (backoff * 2).min(30);
                        delay
                    }
                    Err(error) => {
                        let _ = errors.send(error);
                        return;
                    }
                };
                if stopped.recv_timeout(delay) != Err(mpsc::RecvTimeoutError::Timeout) {
                    return;
                }
            }
        });
        (
            Self {
                stop,
                worker: Some(worker),
            },
            failures,
            ready,
        )
    }
}
impl Drop for Heartbeat {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

enum OwnershipPhase {
    Acquiring,
    Reconciling(uuid::Uuid),
    Ready(uuid::Uuid),
}

/// Local commit and remote acknowledgement are distinct authority boundaries.
/// Retained assignments use their pinned installations while new claims wait.
enum ConfigurationState {
    Unavailable,
    AwaitingAcknowledgment { generation: u64, path: PathBuf },
    Ready(super::control::AcknowledgedConfiguration),
}

impl ConfigurationState {
    fn ready(&self) -> bool {
        matches!(self, Self::Ready(_))
    }

    fn installations(&self) -> Option<&std::path::Path> {
        match self {
            Self::Ready(acknowledged) => Some(&acknowledged.path),
            _ => None,
        }
    }

    fn acknowledge(
        &mut self,
        acknowledged: super::control::AcknowledgedConfiguration,
    ) -> Result<()> {
        let matches = match self {
            Self::AwaitingAcknowledgment { generation, path } => {
                *generation == acknowledged.generation && *path == acknowledged.path
            }
            Self::Ready(prior) => {
                prior.generation == acknowledged.generation && prior.path == acknowledged.path
            }
            Self::Unavailable => false,
        };
        if !matches {
            return Err(failure(
                "configuration acknowledgement conflicts with the committed generation",
            ));
        }
        *self = Self::Ready(acknowledged);
        Ok(())
    }
}

enum ConfigurationOutcome {
    Prepared(Result<Box<super::control::PreparedConfiguration>>),
    Acknowledged(Result<super::control::AcknowledgedConfiguration>),
}

enum ConfigurationRequest {
    Acknowledge(Result<super::control::CommittedConfiguration>),
    Stop,
}

struct ConfigurationWorker {
    request: mpsc::Sender<ConfigurationRequest>,
    worker: Option<thread::JoinHandle<()>>,
}
impl ConfigurationWorker {
    fn start(
        client: Client,
        directory: PathBuf,
        credentials: Option<PathBuf>,
    ) -> (Self, mpsc::Receiver<ConfigurationOutcome>) {
        let (request, requests) = mpsc::channel();
        let (send, results) = mpsc::channel();
        let worker = thread::spawn(move || {
            loop {
                let prepared = super::control::prepare_configuration(
                    &client,
                    &directory,
                    credentials.as_deref(),
                );
                let prepared_ok = prepared.is_ok();
                if send
                    .send(ConfigurationOutcome::Prepared(prepared.map(Box::new)))
                    .is_err()
                {
                    return;
                }
                if prepared_ok {
                    // Only the journal owner may commit and recheck pins. Wait
                    // for that durable commit before making any acknowledgment.
                    match requests.recv() {
                        Ok(ConfigurationRequest::Acknowledge(Ok(committed))) => {
                            let result =
                                super::control::acknowledge_configuration(&client, committed);
                            if send
                                .send(ConfigurationOutcome::Acknowledged(result))
                                .is_err()
                            {
                                return;
                            }
                        }
                        Ok(ConfigurationRequest::Acknowledge(Err(_))) => (),
                        Ok(ConfigurationRequest::Stop) | Err(_) => return,
                    }
                }
                if !matches!(
                    requests.recv_timeout(Duration::from_secs(30)),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) {
                    return;
                }
            }
        });
        (
            Self {
                request,
                worker: Some(worker),
            },
            results,
        )
    }
    fn acknowledge(&self, committed: Result<super::control::CommittedConfiguration>) {
        let _ = self
            .request
            .send(ConfigurationRequest::Acknowledge(committed));
    }
}
impl Drop for ConfigurationWorker {
    fn drop(&mut self) {
        let _ = self.request.send(ConfigurationRequest::Stop);
        if let Some(worker) = self.worker.take()
            && worker.is_finished()
        {
            // A delayed HTTP reply must not extend the shutdown deadline. The
            // only local writes remain serialized in the journal-owning thread.
            let _ = worker.join();
        }
    }
}

fn changed_cleanup_blockers(report: &Value, known: &mut BTreeMap<String, String>) -> Vec<String> {
    let mut changed = Vec::new();
    for run in report["discovery"]["runs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|run| run["state"] == "blocked")
        .chain(report["runs"].as_array().into_iter().flatten())
    {
        let Some(id) = run["assignment_id"].as_str() else {
            continue;
        };
        if run["state"] == "blocked" {
            let diagnostic = run["diagnostic"].as_str().unwrap_or("cleanup blocked");
            if known.get(id).map(String::as_str) != Some(diagnostic) {
                changed.push(run.to_string());
                known.insert(id.to_owned(), diagnostic.to_owned());
            }
        } else {
            known.remove(id);
        }
    }
    changed
}

/// The journal owner schedules work but never waits for filesystem walks or SSH.
struct CleanupWorker {
    request: mpsc::Sender<bool>,
    busy: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl CleanupWorker {
    fn start(
        directory: PathBuf,
        journal_id: uuid::Uuid,
        connector_id: String,
        retention_seconds: u64,
    ) -> (Self, mpsc::Receiver<Result<Value>>) {
        let (request, requests) = mpsc::channel();
        let (send, results) = mpsc::channel();
        let busy = Arc::new(AtomicBool::new(false));
        let running = busy.clone();
        let worker = thread::spawn(move || {
            while let Ok(true) = requests.recv() {
                let result = super::cleanup::run(
                    &directory,
                    journal_id,
                    &connector_id,
                    8,
                    retention_seconds,
                );
                running.store(false, Ordering::Release);
                if send.send(result).is_err() {
                    return;
                }
            }
        });
        (
            Self {
                request,
                busy,
                worker: Some(worker),
            },
            results,
        )
    }
    fn schedule(&self) {
        if self
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            && self.request.send(true).is_err()
        {
            self.busy.store(false, Ordering::Release);
        }
    }
}
impl Drop for CleanupWorker {
    fn drop(&mut self) {
        let _ = self.request.send(false);
        if let Some(worker) = self.worker.take() {
            // An in-flight pass has durable checkpoints. Do not extend the
            // service shutdown deadline waiting for an unavailable remote host.
            if worker.is_finished() {
                let _ = worker.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_claim_authority_requires_the_matching_committed_acknowledgement() {
        let acknowledgment =
            |generation, path: &str| super::super::control::AcknowledgedConfiguration {
                generation,
                path: PathBuf::from(path),
            };
        let mut state = ConfigurationState::Unavailable;
        assert!(state.acknowledge(acknowledgment(1, "one")).is_err());
        assert!(!state.ready());
        state = ConfigurationState::AwaitingAcknowledgment {
            generation: 2,
            path: PathBuf::from("two"),
        };
        for (generation, path) in [(1, "two"), (2, "wrong")] {
            assert!(state.acknowledge(acknowledgment(generation, path)).is_err());
            assert!(!state.ready());
            assert!(state.installations().is_none());
        }
        state.acknowledge(acknowledgment(2, "two")).unwrap();
        state.acknowledge(acknowledgment(2, "two")).unwrap();
        assert!(state.ready());
        assert_eq!(state.installations(), Some(std::path::Path::new("two")));
        assert!(state.acknowledge(acknowledgment(1, "one")).is_err());
        assert_eq!(state.installations(), Some(std::path::Path::new("two")));
        state = ConfigurationState::AwaitingAcknowledgment {
            generation: 3,
            path: PathBuf::from("three"),
        };
        assert!(!state.ready());
        assert!(state.acknowledge(acknowledgment(2, "two")).is_err());
    }

    #[test]
    fn blocked_runs_are_reported_once_until_their_diagnostic_or_state_changes() {
        let mut known = BTreeMap::new();
        let blocked = json!({"runs":[{"assignment_id":"one","state":"blocked","diagnostic":"host unavailable"}]});
        assert_eq!(changed_cleanup_blockers(&blocked, &mut known).len(), 1);
        assert!(changed_cleanup_blockers(&blocked, &mut known).is_empty());
        assert!(
            changed_cleanup_blockers(&json!({"discovery":blocked,"runs":[]}), &mut known)
                .is_empty()
        );
        // Other passes may visit a different part of the durable pending queue.
        assert!(changed_cleanup_blockers(&json!({"runs":[]}), &mut known).is_empty());
        let changed = json!({"runs":[{"assignment_id":"one","state":"blocked","diagnostic":"delivery unfinished"}]});
        assert_eq!(changed_cleanup_blockers(&changed, &mut known).len(), 1);
        let complete = json!({"runs":[{"assignment_id":"one","state":"complete"}]});
        assert!(changed_cleanup_blockers(&complete, &mut known).is_empty());
        assert!(known.is_empty());
        assert_eq!(changed_cleanup_blockers(&blocked, &mut known).len(), 1);
    }
}
