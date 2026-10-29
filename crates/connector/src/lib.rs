//! Durable connector commands and the continuous service runtime.

mod artifacts;
mod capture;
mod cleanup;
#[path = "runtime/client.rs"]
mod client;
mod collections;
mod compute;
mod connections;
mod control;
mod drivers;
#[path = "runtime/execution.rs"]
mod execution;
mod execution_policy;
#[path = "runtime/journal.rs"]
mod journal;
mod launch;
#[path = "runtime/local_process.rs"]
mod local_process;
mod log_archive;
#[path = "runtime/logs.rs"]
mod logs;
mod multipart;
#[path = "runtime/numeric.rs"]
mod numeric;
mod protocol;
#[path = "runtime/records.rs"]
mod records;
mod service;
mod spool;
#[path = "runtime/state.rs"]
mod state;
mod transfers;

use std::{fmt, path::PathBuf, process::ExitCode};

use clap::Subcommand;
use serde_json::json;

pub use control::EnrollOptions;
pub use service::Options as RunOptions;

#[derive(Subcommand)]
pub enum HostCommand {
    /// Enroll using a single-use credential, retaining identity across retries.
    Enroll(EnrollOptions),
    /// Continuously claim and execute assignments over HTTP.
    Run(RunOptions),
    /// Create a journal bound to one connector and a loopback HTTP API.
    Init {
        #[arg(long)]
        journal: PathBuf,
        #[arg(long)]
        api_base_url: String,
        #[arg(long)]
        connector_id: String,
    },
    /// Register a process session, send a heartbeat, and prepare at most one assignment.
    /// Read the bearer token from stdin unless --token-file is supplied.
    ClaimOnce {
        #[arg(long)]
        journal: PathBuf,
        #[arg(long)]
        token_file: Option<PathBuf>,
        /// Private credential-reference map used while compiling API configuration.
        #[arg(long)]
        credentials_file: Option<PathBuf>,
        /// Permit trusted process/direct development contexts under this regular OS user.
        #[arg(long)]
        allow_development_execution: bool,
    },
    /// Retrieve existing work and reconcile ownership without claiming or launching work.
    Reconcile {
        #[arg(long)]
        journal: PathBuf,
        #[arg(long)]
        token_file: Option<PathBuf>,
        /// Assert that the previous process has stopped, including on other hosts.
        #[arg(long)]
        confirm_prior_process_stopped: bool,
    },
    /// Execute or resume one prepared assignment, retaining durable backend evidence.
    ExecuteOnce {
        #[arg(long)]
        log_policy_file: Option<PathBuf>,
        #[arg(long)]
        journal: PathBuf,
        #[arg(long)]
        token_file: Option<PathBuf>,
        #[arg(long)]
        confirm_prior_process_stopped: bool,
        /// Exercise recovery after a backend commit whose acknowledgement is lost.
        #[arg(long)]
        simulate_lost_submission_ack: bool,
        /// Private credential-reference map used while compiling API configuration.
        #[arg(long)]
        credentials_file: Option<PathBuf>,
        /// Permit trusted process/direct development contexts under this regular OS user.
        #[arg(long)]
        allow_development_execution: bool,
    },
    /// Publish retained terminal log archives without executing scientific work.
    PublishLogArchives {
        #[arg(long)]
        journal: PathBuf,
        #[arg(long)]
        token_file: Option<PathBuf>,
        #[arg(long)]
        confirm_prior_process_stopped: bool,
    },
    /// Reclaim confirmed, delivered run payloads; preserve evidence and launch fences.
    Cleanup {
        #[arg(long)]
        journal: PathBuf,
        /// Inspect eligibility and payload usage without changing durable state.
        #[arg(long)]
        dry_run: bool,
        #[arg(long, default_value_t = 8, value_parser = clap::value_parser!(u32).range(1..=64))]
        limit: u32,
        /// Keep safely completed payloads for this optional debugging grace period.
        #[arg(long, default_value_t = 0)]
        completed_retention_seconds: u64,
    },
    /// Show journal identities and prepared assignment IDs, without inputs or secrets.
    Inspect {
        #[arg(long)]
        journal: PathBuf,
    },
}

/// Credential-free compute actions and private subprocess entry points.
#[derive(Subcommand)]
pub enum RuntimeCommand {
    #[command(hide = true)]
    TransferWorker,
    #[command(hide = true)]
    LocalWorker {
        #[arg(long)]
        directory: PathBuf,
    },
    /// Credential-free runner on an SSH host or Slurm submission host.
    ComputeRunner {
        #[arg(value_parser = ["identity", "prepare", "start", "status", "request-cancel", "validate", "protect-installations", "write-input", "write-collection", "read-collection", "transfer-probe", "read-output", "allocation-status", "read-logs", "build-log-archive", "read-log-archive", "cleanup-log-archive", "cleanup-run"])]
        action: String,
        #[arg(long)]
        directory: PathBuf,
    },
}

struct ConnectorError {
    message: String,
    kind: ErrorKind,
    startup_stage: Option<launch::FailureStage>,
    command: Option<Box<connections::CommandDiagnostics>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ErrorKind {
    Blocked,
    Retry,
    Waiting,
    Fatal,
    Stopping,
}

impl ConnectorError {
    fn with_command(mut self, command: connections::CommandDiagnostics) -> Self {
        self.command = Some(Box::new(command));
        self
    }
    fn before_wrapper(mut self, stage: launch::FailureStage) -> Self {
        self.startup_stage = Some(stage);
        self
    }
    fn with_kind(mut self, kind: ErrorKind) -> Self {
        self.kind = kind;
        self
    }
}

impl fmt::Debug for ConnectorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConnectorError")
            .field("message", &self.message)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for ConnectorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ConnectorError {}

type Result<T> = std::result::Result<T, ConnectorError>;

fn failure(message: impl Into<String>) -> ConnectorError {
    ConnectorError {
        message: message.into(),
        kind: ErrorKind::Blocked,
        startup_stage: None,
        command: None,
    }
}

/// Run a host command without consulting user CLI profiles or credentials.
pub fn execute_host(command: HostCommand) -> ExitCode {
    let service = matches!(command, HostCommand::Run(_));
    finish(run_host(command), service)
}

/// Run the compute protocol or a worker without loading connector credentials.
pub fn execute_runtime(command: RuntimeCommand) -> ExitCode {
    finish(run_runtime(command), false)
}

fn finish(result: Result<serde_json::Value>, service: bool) -> ExitCode {
    match result {
        Ok(output) => {
            println!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            if service {
                eprintln!("{}", json!({"status":"fatal", "message":error.to_string()}));
            } else {
                eprintln!("Enumeray connector: {error}");
            }
            ExitCode::FAILURE
        }
    }
}

fn run_host(command: HostCommand) -> Result<serde_json::Value> {
    match command {
        HostCommand::Run(options) => service::run(options),
        HostCommand::Cleanup {
            journal: directory,
            dry_run,
            limit,
            completed_retention_seconds,
        } => {
            if dry_run {
                let (data, _) = journal::Journal::snapshot(&directory)?;
                cleanup::inspect(
                    &directory,
                    data.journal_id,
                    &data.connector_id,
                    limit as usize,
                    completed_retention_seconds,
                )
            } else {
                let journal = journal::Journal::open(&directory)?;
                cleanup::run(
                    &journal.directory,
                    journal.data.journal_id,
                    &journal.data.connector_id,
                    limit as usize,
                    completed_retention_seconds,
                )
            }
        }
        HostCommand::Enroll(options) => control::enroll(options),
        HostCommand::Init {
            journal,
            api_base_url,
            connector_id,
        } => {
            let api_base_url = client::validate_api_url(&api_base_url)?;
            let journal = journal::Journal::initialize(&journal, api_base_url, connector_id)?;
            Ok(json!({"status": "initialized", "journal_id": journal.data.journal_id}))
        }
        HostCommand::ClaimOnce {
            journal,
            token_file,
            credentials_file,
            allow_development_execution,
        } => {
            let mut journal = journal::Journal::open(&journal)?;
            let mut client = client::Client::configured(
                &journal.data.api_base_url,
                token_file.as_deref(),
                journal.data.allow_insecure_http,
            )?;
            client.allow_development_execution = allow_development_execution;
            journal.record_execution_policy(allow_development_execution)?;
            execution_policy::warn(allow_development_execution);
            client.claim_once(&mut journal, credentials_file.as_deref())
        }
        HostCommand::Inspect { journal: directory } => {
            let (data, mut summary) = journal::Journal::snapshot(&directory)?;
            summary["configuration_retention"] = control::summary(&directory)?;
            summary["cleanup"] =
                cleanup::inspect(&directory, data.journal_id, &data.connector_id, 8, 0)?;
            Ok(summary)
        }
        HostCommand::Reconcile {
            journal,
            token_file,
            confirm_prior_process_stopped,
        } => {
            let mut journal = journal::Journal::open(&journal)?;
            let client = client::Client::configured(
                &journal.data.api_base_url,
                token_file.as_deref(),
                journal.data.allow_insecure_http,
            )?;
            client.reconcile(&mut journal, confirm_prior_process_stopped)
        }
        HostCommand::ExecuteOnce {
            log_policy_file,
            journal,
            token_file,
            confirm_prior_process_stopped,
            simulate_lost_submission_ack,
            credentials_file,
            allow_development_execution,
        } => {
            let mut journal = journal::Journal::open(&journal)?;
            let mut client = client::Client::configured(
                &journal.data.api_base_url,
                token_file.as_deref(),
                journal.data.allow_insecure_http,
            )?;
            client.allow_development_execution = allow_development_execution;
            journal.record_execution_policy(allow_development_execution)?;
            execution_policy::warn(allow_development_execution);
            client.log_policy = capture::load_policy(log_policy_file.as_deref())?;
            client.execute_once(
                &mut journal,
                confirm_prior_process_stopped,
                simulate_lost_submission_ack,
                credentials_file.as_deref(),
            )
        }
        HostCommand::PublishLogArchives {
            journal,
            token_file,
            confirm_prior_process_stopped,
        } => {
            let mut journal = journal::Journal::open(&journal)?;
            let client = client::Client::configured(
                &journal.data.api_base_url,
                token_file.as_deref(),
                journal.data.allow_insecure_http,
            )?;
            let session =
                client.acquire_recovery_session(&mut journal, confirm_prior_process_stopped)?;
            client.reconcile_inventory(&mut journal)?;
            journal.inventory_reconciled(session)?;
            let mut published = 0;
            for index in 0..journal.data.prepared_assignments.len() {
                let entry = &journal.data.prepared_assignments[index];
                if entry.delivery_finished() {
                    continue;
                }
                let Some(progress) = entry.log_archive.as_ref().filter(|p| !p.finished) else {
                    continue;
                };
                if entry.terminal_result.is_none() {
                    continue;
                }
                let at = progress
                    .finalized_at
                    .as_deref()
                    .ok_or_else(|| failure("archive has no finalization evidence"))?;
                let value = log_archive::publish(
                    &client,
                    &journal.directory,
                    entry.launch_intent_id.expect("intent"),
                    &entry.assignment,
                    session,
                    at,
                )?;
                let status = serde_json::from_value(value)
                    .map_err(|_| failure("invalid archive publication status"))?;
                log_archive::complete(&mut journal.data.prepared_assignments[index], status)?;
                journal.save()?;
                published += 1;
            }
            execution::complete_deliveries(&client, &mut journal, session)?;
            client.release_session(&mut journal, session)?;
            journal.archive_completed()?;
            Ok(json!({"status":"published_log_archives","resolved":published}))
        }
    }
}

fn run_runtime(command: RuntimeCommand) -> Result<serde_json::Value> {
    match command {
        RuntimeCommand::TransferWorker => transfers::worker(),
        RuntimeCommand::ComputeRunner { action, directory } => {
            use std::io::Read;
            let mut input = Vec::new();
            std::io::stdin()
                .take(4 * 1024 * 1024 + 1)
                .read_to_end(&mut input)
                .map_err(|_| failure("cannot read runner request"))?;
            if input.len() > 4 * 1024 * 1024 {
                return Err(failure("runner request exceeds bound"));
            }
            protocol::run(&action, &directory, Some(&input))
        }
        RuntimeCommand::LocalWorker { directory } => {
            local_process::worker(&directory)?;
            Ok(serde_json::json!({"status":"worker_completed"}))
        }
    }
}
