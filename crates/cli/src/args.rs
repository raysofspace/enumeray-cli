use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "enumeray",
    version,
    about = "Model execution and connector management"
)]
pub struct Args {
    /// User API origin override; credentials never cross origins. Host/runner commands use explicit configuration.
    #[arg(long, global = true)]
    pub api: Option<String>,
    /// Named local user API/login profile; host/runner commands use explicit configuration.
    #[arg(long, default_value = "default", global = true)]
    pub profile: String,
    /// Private user profiles file; defaults to $XDG_CONFIG_HOME/enumeray/profiles.json or ~/.config/enumeray/profiles.json.
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
    /// Emit user API results as JSON (NDJSON for following logs) and runtime errors on stderr; host/runner output follows its own protocol.
    #[arg(long, global = true)]
    pub json: bool,
    /// User API HTTP timeout in seconds; live logs use it for read inactivity. Does not limit execution time.
    #[arg(long, default_value_t = 60, global = true)]
    pub timeout: u64,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Guide or resume connector setup and a first model candidate trial.
    Setup(Setup),
    #[command(flatten)]
    Runtime(enumeray_connector::RuntimeCommand),
    /// Inspect service status and administer queue limits, suspensions and audit history.
    #[command(subcommand)]
    Service(Service),
    /// Configure API profiles, sign in and manage delegated tokens.
    #[command(subcommand)]
    Auth(Auth),
    /// Register and administer connectors or operate their local host runtime.
    #[command(subcommand)]
    Connector(Connector),
    /// Create author projects and manage model publications, ownership and history.
    #[command(subcommand)]
    Model(Model),
    /// Manage model access policies, invitations, requests and per-user blocks.
    #[command(subcommand)]
    Access(Access),
    /// Inspect accepted run history, diagnostics and lifecycle by ID; execute through analysis directories.
    #[command(subcommand)]
    Run(Run),
    /// Upload, inspect and download individual verified files.
    #[command(subcommand)]
    Artifact(Artifact),
    /// Upload, inspect and selectively download immutable file collections.
    #[command(subcommand)]
    Collection(Collection),
    /// Keep one analysis's inputs, retry identities, run and verified results together.
    #[command(subcommand)]
    Analysis(Analysis),
    /// Explicit HTTP escape hatch for new management APIs; limited to this API origin.
    Api {
        /// HTTP method supported by the target API endpoint.
        #[arg(value_parser=["GET", "POST", "PUT", "PATCH", "DELETE"])]
        method: String,
        /// API-relative path beginning /api/v1/, including any query string; full URLs are refused.
        path: String,
        /// Optional JSON request body file matching the endpoint schema.
        #[arg(long)]
        file: Option<PathBuf>,
        /// Persisted request key for endpoints that document idempotency; reuse with the exact same body.
        #[arg(long)]
        idempotency_key: Option<String>,
    },
}

#[derive(clap::Args)]
pub struct Setup {
    /// Private setup checkpoint; defaults to ./enumeray-setup. Reuse it to resume the same operation.
    #[arg(long)]
    pub directory: Option<PathBuf>,
    /// Execution identity mode; must match the setup input or saved plan.
    #[arg(long, value_enum)]
    pub mode: Option<SetupMode>,
    /// Version-1 setup input containing host, connector and first-model selections.
    #[arg(long)]
    pub file: Option<PathBuf>,
    /// Require complete input and explicit approvals; never prompt.
    #[arg(long)]
    pub non_interactive: bool,
    /// Approve the exact saved plan SHA-256 shown by setup.
    #[arg(long)]
    pub approve: Option<String>,
    /// Acknowledge that trusted model code can access this OS user's files and credentials.
    #[arg(long)]
    pub allow_development_execution: bool,
    /// Explicitly submit/resume the saved first-model candidate trial.
    #[arg(long)]
    pub submit_trial: bool,
    /// Confirm site-specific Slurm batch/runtime preparation before freezing a generated starter.
    #[arg(long)]
    pub confirm_model_preparation: bool,
    /// Deliberately issue a new enrollment document when no usable secret was retained.
    #[arg(long)]
    pub replace_enrollment: bool,
    /// Import an explicitly issued enrollment after recovering an uncertain secret response.
    #[arg(long)]
    pub enrollment_file: Option<PathBuf>,
    /// Explicitly adopt matching existing registration, host and model records.
    #[arg(long)]
    pub adopt_existing: bool,
    #[command(subcommand)]
    pub command: Option<SetupCommand>,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SetupMode {
    /// Run trusted model code under one regular OS account.
    Development,
    /// Separate connector and model execution identities.
    Managed,
}

#[derive(Subcommand)]
pub enum SetupCommand {
    /// Inspect saved progress and authenticated readiness without requesting checks.
    Status {
        /// Existing private setup checkpoint directory to inspect.
        #[arg(long)]
        directory: PathBuf,
    },
    /// Apply reviewed managed changes locally as an administrator; never loads a user profile.
    ApplyHost {
        /// Generated host-plan.json whose typed actions and digest will be revalidated.
        #[arg(long)]
        plan: PathBuf,
        /// Host to prepare locally: connector, or execution for the SSH execution host.
        #[arg(long, value_parser=["connector","execution"])]
        side: String,
        /// Exact host plan SHA-256 from setup review; separate from the owner setup plan digest.
        #[arg(long)]
        approve: String,
        /// Private one-time enrollment JSON document; supply only on the connector host.
        #[arg(long)]
        enrollment_file: Option<PathBuf>,
        /// Local copy of the reviewed executable on this host; its approved SHA-256 must match.
        #[arg(long)]
        executable_source: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
pub enum Analysis {
    /// Reconnect to an accepted run without preparing or submitting another execution.
    Attach {
        /// Accepted run UUID; provide either this ID or --receipt.
        #[arg(required_unless_present = "receipt", conflicts_with = "receipt")]
        run_id: Option<String>,
        /// Original saved acceptance receipt; its identity is checked against the service.
        #[arg(long)]
        receipt: Option<PathBuf>,
        /// New private analysis directory, or the same interrupted attachment destination; parent must exist.
        #[arg(long)]
        directory: PathBuf,
    },
    /// Import an original prepared request locally. Submit explicitly to reconcile acceptance.
    Import {
        /// Original private prepared-operation JSON file from the former run prepare workflow.
        #[arg(long)]
        prepared: PathBuf,
        /// New private analysis directory, or the same interrupted import destination.
        #[arg(long)]
        directory: PathBuf,
    },
    /// Save a current publication or exact candidate and inputs for review. Does not upload or execute.
    Init {
        /// Stable model ID from model list/get or supplied by its author.
        model: String,
        /// Trial this exact ready or published candidate using its captured compute authority.
        #[arg(long, value_name = "PUBLICATION")]
        candidate: Option<String>,
        /// New private analysis directory; parent must exist and existing paths are refused.
        #[arg(long)]
        directory: PathBuf,
        /// Named input example declared by the selected publication; use either --example or --inputs.
        #[arg(long, required_unless_present = "inputs", conflicts_with = "inputs")]
        example: Option<String>,
        /// Scientific input JSON object file; use either --inputs or --example. Local slot bindings override references.
        #[arg(long, required_unless_present = "example")]
        inputs: Option<PathBuf>,
        /// Bind a declared file slot to an original local source: SLOT=PATH. Repeat for multiple slots.
        #[arg(long, value_name = "SLOT=PATH")]
        file: Vec<String>,
        /// Bind a collection slot to a local source directory: SLOT=PATH. Repeat for multiple slots.
        #[arg(long, value_name = "SLOT=PATH")]
        collection: Vec<String>,
        /// Bind a collection slot to a member manifest: SLOT=PATH. Repeat for multiple slots.
        #[arg(long, value_name = "SLOT=PATH")]
        collection_manifest: Vec<String>,
        /// Select a declared media type: SLOT=TYPE. Repeat per slot; inferred only for a sole allowed type.
        #[arg(long, value_name = "SLOT=TYPE")]
        media_type: Vec<String>,
        /// Scientific upload metadata JSON object file: SLOT=PATH. Repeat per slot; defaults to {}.
        #[arg(long, value_name = "SLOT=PATH")]
        metadata: Vec<String>,
    },
    /// Freeze inputs, upload files and submit. Repeat after interruption to resume this operation.
    Submit {
        /// Existing private analysis directory bound to this API and account.
        directory: PathBuf,
    },
    /// Inspect saved preparation and current accepted run progress.
    Status {
        /// Existing private analysis directory bound to this API and account.
        directory: PathBuf,
    },
    /// Read retained logs or follow the accepted run using this directory's private checkpoint.
    Logs {
        /// Existing private analysis directory bound to this API and account.
        directory: PathBuf,
        #[command(flatten)]
        page: PageArgs,
        /// Follow resumable live logs, then drain retained logs after terminal inspection.
        #[arg(long, conflicts_with_all = ["limit", "cursor"])]
        follow: bool,
        /// Run inspection interval in seconds (default 2 when following).
        #[arg(long, requires = "follow", value_parser = clap::value_parser!(u64).range(1..))]
        interval: Option<u64>,
        /// Local observation deadline in seconds (default 3600 when following).
        #[arg(long, requires = "follow", value_parser = clap::value_parser!(u64).range(1..))]
        deadline: Option<u64>,
        /// Explicitly acknowledge missing retained history and continue from its recovery cursor.
        #[arg(long, requires = "follow")]
        allow_gaps: bool,
    },
    /// Request cancellation of the accepted run. Repeat after an uncertain result.
    Cancel {
        /// Existing private analysis directory bound to this API and account.
        directory: PathBuf,
    },
    /// Wait for the accepted run; a local deadline leaves execution unchanged.
    Wait {
        /// Existing private analysis directory bound to this API and account.
        directory: PathBuf,
        /// Positive polling interval in seconds.
        #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u64).range(1..))]
        interval: u64,
        /// Positive local wait budget in seconds; expiry stops observation without cancelling work.
        #[arg(long, default_value_t = 3600, value_parser = clap::value_parser!(u64).range(1..))]
        deadline: u64,
        /// Exit 3 for failed, 4 for cancelled, or 5 for the local deadline.
        #[arg(long)]
        check_outcome: bool,
    },
    /// Collect terminal evidence and checksum-verified files; resume without overwriting conflicts.
    Collect {
        /// Existing private analysis directory bound to this API and account.
        directory: PathBuf,
        /// Select a collection output member: SLOT=PATH with a collection-relative path. Repeat for several members; other output bytes remain deferred.
        #[arg(long, value_name = "SLOT=PATH")]
        member: Vec<String>,
        /// Save terminal evidence and complete manifests without downloading scientific bytes.
        #[arg(long)]
        evidence_only: bool,
    },
}

/// One bounded API page. Omitted options retain the endpoint's defaults.
#[derive(clap::Args, Default)]
pub struct PageArgs {
    /// Maximum entries on this page (1–100); omit for the API default.
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..=100))]
    pub limit: Option<u16>,
    /// Continuation from the preceding page; pass unchanged with the same filters.
    #[arg(long)]
    pub cursor: Option<String>,
}

impl PageArgs {
    pub fn query(&self) -> Vec<(&'static str, String)> {
        let mut query = Vec::new();
        if let Some(limit) = self.limit {
            query.push(("limit", limit.to_string()));
        }
        if let Some(cursor) = &self.cursor {
            query.push(("cursor", cursor.clone()));
        }
        query
    }
}

#[derive(clap::Args, Default)]
pub struct RunFilters {
    /// Exact model ID in your own run history.
    #[arg(long)]
    pub model: Option<String>,
    /// Current public run state.
    #[arg(long, value_parser = ["queued", "running", "succeeded", "failed", "cancelled"])]
    pub status: Option<String>,
    /// Exclusive creation-time lower bound in RFC3339 (at most six fractional digits).
    #[arg(long)]
    pub created_after: Option<String>,
    /// Exclusive creation-time upper bound in RFC3339 (at most six fractional digits).
    #[arg(long)]
    pub created_before: Option<String>,
}

impl RunFilters {
    pub fn query(&self) -> Vec<(&'static str, String)> {
        [
            ("model_id", &self.model),
            ("status", &self.status),
            ("created_after", &self.created_after),
            ("created_before", &self.created_before),
        ]
        .into_iter()
        .filter_map(|(key, value)| value.as_ref().map(|value| (key, value.clone())))
        .collect()
    }
}

#[derive(Subcommand)]
pub enum Service {
    /// Inspect service status using the authenticated account.
    Status,
    /// Inspect service-wide and per-user queue limits and their generation.
    Limits,
    /// Replace both queue limits at the observed service generation.
    SetLimits {
        /// Maximum accepted queued runs covered by this limit; lowering it preserves accepted work.
        #[arg(long)]
        max_queued_runs: u32,
        /// Per-account queue limit; must not exceed --max-queued-runs.
        #[arg(long)]
        max_queued_runs_per_user: u32,
        /// Generation returned by service limits; prevents overwriting an intervening change.
        #[arg(long)]
        generation: i64,
    },
    /// Suspend an account and record the reason in the service audit.
    Suspend {
        /// Stable UUID of the target account; use auth whoami to find your own ID.
        user: uuid::Uuid,
        /// Nonempty explanation recorded in the service audit history.
        #[arg(long)]
        reason: String,
    },
    /// Remove an account suspension and record the reason in the service audit.
    Resume {
        /// Stable UUID of the target account; use auth whoami to find your own ID.
        user: uuid::Uuid,
        /// Nonempty explanation recorded in the service audit history.
        #[arg(long)]
        reason: String,
    },
    /// Read one page of service administration audit events.
    Audit {
        #[command(flatten)]
        page: PageArgs,
    },
}

#[derive(Subcommand)]
pub enum Auth {
    /// Browser-assisted authorization code login with PKCE; API must expose auth/config.
    Login {
        /// Read an existing API token from stdin instead of using a browser.
        #[arg(long)]
        token_stdin: bool,
        /// Print the authorization URL but do not launch a browser.
        #[arg(long)]
        no_browser: bool,
    },
    /// Renew the selected browser login using its saved refresh token.
    Refresh,
    /// Clear this profile's local credentials without revoking server tokens.
    Logout,
    /// Inspect the authenticated account, stable user ID and authority.
    Whoami,
    /// List configured profiles without credentials.
    Profiles,
    /// Create/select this profile's API origin; changing origin clears old credentials.
    Configure {
        /// API origin to save in this profile, for example https://enumeray.example.org.
        api_url: String,
    },
    /// Create a scoped, expiring token and save the secret to a private file.
    TokenCreate {
        /// Descriptive delegated-token name shown by auth token-list.
        name: String,
        /// Comma-separated delegated scopes; repeatable. The token cannot exceed your own authority.
        #[arg(long, value_delimiter = ',', required = true)]
        scopes: Vec<String>,
        /// Delegated-token lifetime in seconds; default is 30 days.
        #[arg(long, default_value_t = 2_592_000)]
        expires_in_seconds: u64,
        /// New private JSON file for the one-time token response; existing paths are refused.
        #[arg(long)]
        output: PathBuf,
    },
    /// List your delegated-token metadata without revealing secrets.
    TokenList {
        #[command(flatten)]
        page: PageArgs,
    },
    /// Permanently revoke one of your delegated tokens by ID.
    TokenRevoke {
        /// Delegated-token ID from auth token-list/create; never supply the plaintext token.
        id: String,
    },
}

#[derive(Subcommand)]
pub enum ConnectorContext {
    /// Request/resume a publication-independent host check using a saved identity.
    Preflight {
        /// Owning connector ID from connector list/get.
        connector: String,
        /// Execution-context ID registered under the selected connector.
        context: String,
        /// Private saved check operation; reuse after interruption to reconcile the same request.
        #[arg(long)]
        directory: PathBuf,
        /// Host-check polling interval in seconds (1-60).
        #[arg(long,default_value_t=2,value_parser=clap::value_parser!(u64).range(1..=60))]
        interval: u64,
        /// Local check wait budget in seconds (1-3600); expiry leaves the saved attempt available.
        #[arg(long,default_value_t=300,value_parser=clap::value_parser!(u64).range(1..=3600))]
        deadline: u64,
    },
    /// Read the latest host-check attempt/report without initiating a check.
    PreflightStatus {
        /// Owning connector ID from connector list/get.
        connector: String,
        /// Execution-context ID registered under the selected connector.
        context: String,
    },
    /// List execution contexts visible under this connector.
    List {
        #[command(flatten)]
        page: PageArgs,
        /// Stable connector ID from connector list/get.
        id: String,
    },
    /// Register a context from a versioned execution-context manifest.
    Create {
        /// Stable connector ID from connector list/get.
        id: String,
        /// Version-1 execution-context JSON envelope; connector_id must match the selected connector.
        #[arg(long)]
        file: PathBuf,
    },
    /// Change descriptive location and data-access disclosure.
    Update {
        /// Owning connector ID from connector list/get.
        connector: String,
        /// Execution-context ID registered under the selected connector.
        context: String,
        /// Nonempty description of the execution location shown to model users; does not route work.
        #[arg(long)]
        location: String,
        /// Nonempty description of data access and handling; accepted runs retain their original snapshot.
        #[arg(long)]
        disclosure: String,
        /// Observed context generation from connector context list/get; shared by metadata and lifecycle changes.
        #[arg(long)]
        generation: i64,
    },
    /// Inspect context lifecycle, retained work and retirement readiness.
    Get {
        /// Owning connector ID from connector list/get.
        connector: String,
        /// Execution-context ID registered under the selected connector.
        context: String,
    },
    /// Inspect replacement dependencies and actions for connector owners and authors.
    ReplacementPlan {
        /// Owning connector ID from connector list/get.
        connector: String,
        /// Execution-context ID registered under the selected connector.
        context: String,
        /// Optional replacement context ID; requires administration of its connector and does not migrate publications or grants.
        #[arg(long)]
        replacement: Option<String>,
        /// Dependency collection to inspect on this page.
        #[arg(long, value_enum, default_value = "publications")]
        section: ContextDependencySection,
        #[command(flatten)]
        page: PageArgs,
    },
    /// Close new work and explicitly drain or cancel queued runs.
    Cutover {
        /// Owning connector ID from connector list/get.
        connector: String,
        /// Execution-context ID registered under the selected connector.
        context: String,
        /// Optional replacement context ID; requires administration of its connector. Authors must explicitly publish replacements.
        #[arg(long)]
        replacement: Option<String>,
        /// Policy for queued accepted work: drain it on the source, or cancel it while assigned work continues.
        #[arg(long, value_enum)]
        accepted_work: ContextAcceptedWork,
        /// Observed source context lifecycle generation from connector context get.
        #[arg(long)]
        generation: i64,
        /// Inspect the observed and proposed change without applying it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Reopen future operations while retaining cancellations and configuration releases.
    Reopen {
        /// Owning connector ID from connector list/get.
        connector: String,
        /// Execution-context ID registered under the selected connector.
        context: String,
        /// Observed source context lifecycle generation from connector context get.
        #[arg(long)]
        generation: i64,
        /// Inspect the observed and proposed change without applying it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Request retirement of obsolete publication configuration on a draining context.
    ReleaseConfigurations {
        /// Owning connector ID from connector list/get.
        connector: String,
        /// Execution-context ID registered under the selected connector.
        context: String,
        /// Observed source context lifecycle generation from connector context get.
        #[arg(long)]
        generation: i64,
        /// Inspect the observed and proposed change without applying it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Permanently archive a context after work, delivery and configuration finish.
    Retire {
        /// Owning connector ID from connector list/get.
        connector: String,
        /// Execution-context ID registered under the selected connector.
        context: String,
        /// Observed source context lifecycle generation from connector context get.
        #[arg(long)]
        generation: i64,
        /// Inspect the observed and proposed change without applying it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Wait for context retirement readiness without shutting down the connector.
    WaitRetirement {
        /// Owning connector ID from connector list/get.
        connector: String,
        /// Execution-context ID registered under the selected connector.
        context: String,
        /// Exact source lifecycle generation from context get/cutover; any later change stops the wait.
        #[arg(long)]
        generation: i64,
        /// Positive polling interval in seconds.
        #[arg(long, default_value_t = 2)]
        interval: u64,
        /// Positive local wait budget in seconds; expiry stops observation without cancelling work.
        #[arg(long, default_value_t = 3600)]
        deadline: u64,
    },
}

#[derive(Subcommand)]
pub enum ConnectorGrant {
    /// List this connector's context-scoped compute grants.
    List {
        #[command(flatten)]
        page: PageArgs,
        /// Stable connector ID from connector list/get.
        id: String,
    },
    /// Give a user explicit compute access to one execution context.
    Create {
        /// Stable connector ID from connector list/get.
        id: String,
        /// Execution-context ID registered under the selected connector.
        #[arg(long)]
        context: String,
        /// Stable UUID of the account receiving compute access to this context.
        #[arg(long)]
        user: uuid::Uuid,
        /// Maximum accepted queued runs covered by this limit; lowering it preserves accepted work.
        #[arg(long, default_value_t = 100)]
        max_queued_runs: u32,
        /// Dispatch capacity for unfinished assignments, including queued Slurm allocations.
        #[arg(long, default_value_t = 4)]
        max_outstanding_assignments: u32,
    },
    /// Inspect current limits, generation and permanent revocation state.
    Get {
        /// Owning connector ID from connector list/get.
        connector: String,
        /// Compute-grant ID from connector grant list/create, not a context ID.
        grant_id: uuid::Uuid,
    },
    /// Replace both capacity limits at the observed grant generation.
    Update {
        /// Owning connector ID from connector list/get.
        connector: String,
        /// Compute-grant ID from connector grant list/create, not a context ID.
        grant_id: uuid::Uuid,
        /// Maximum accepted queued runs covered by this limit; lowering it preserves accepted work.
        #[arg(long)]
        max_queued_runs: u32,
        /// Dispatch capacity for unfinished assignments, including queued Slurm allocations.
        #[arg(long)]
        max_outstanding_assignments: u32,
        /// Observed compute-grant generation from connector grant get.
        #[arg(long)]
        generation: i64,
    },
    /// Permanently revoke one grant without substituting another grant.
    Revoke {
        /// Stable connector ID from connector list/get.
        id: String,
        /// Compute-grant ID from connector grant list/create, not a context ID.
        grant_id: String,
    },
    /// Discover the compute grants available to your authenticated account.
    Available {
        #[command(flatten)]
        page: PageArgs,
    },
}

#[derive(Subcommand)]
pub enum ConnectorMember {
    /// Inspect the owner, co-administrators and ownership generation.
    List {
        /// Stable connector ID from connector list/get.
        id: String,
    },
    /// Add a co-administrator using the observed ownership generation.
    Add {
        /// Stable connector ID from connector list/get.
        id: String,
        /// Stable UUID of the target account; use auth whoami to find your own ID.
        #[arg(long)]
        user: uuid::Uuid,
        /// Observed ownership generation; independent of execution configuration.
        #[arg(long)]
        generation: i64,
    },
    /// Remove a co-administrator without revoking independent compute grants.
    Remove {
        /// Stable connector ID from connector list/get.
        id: String,
        /// Stable UUID of the target account; use auth whoami to find your own ID.
        #[arg(long)]
        user: uuid::Uuid,
        /// Observed ownership generation; independent of execution configuration.
        #[arg(long)]
        generation: i64,
    },
}

#[derive(Subcommand)]
pub enum ConnectorOwnership {
    /// Propose transfer to a named recipient using the ownership generation.
    Transfer {
        /// Stable connector ID from connector list/get.
        id: String,
        /// Stable UUID of the proposed owner, who must separately accept the transfer.
        #[arg(long)]
        to: uuid::Uuid,
        /// Observed ownership generation; independent of execution configuration.
        #[arg(long)]
        generation: i64,
    },
    /// Accept a transfer only for the explicitly selected connector.
    Accept {
        /// Stable connector ID from connector list/get.
        id: String,
        /// Ownership-transfer ID returned by the transfer proposal; only its recipient may accept.
        transfer_id: String,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub enum ContextDependencySection {
    /// Dependent publications and author handoff actions.
    Publications,
    /// Context-scoped compute grants and their replacement actions.
    Grants,
    /// Accepted runs and outstanding execution or delivery obligations.
    Runs,
}

impl From<ContextDependencySection> for enumeray_contracts::management::ContextDependencySection {
    fn from(value: ContextDependencySection) -> Self {
        match value {
            ContextDependencySection::Publications => Self::Publications,
            ContextDependencySection::Grants => Self::Grants,
            ContextDependencySection::Runs => Self::Runs,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
pub enum ContextAcceptedWork {
    /// Let already accepted queued work execute on the source context.
    Drain,
    /// Cancel queued work; already assigned work retains its execution identity.
    CancelQueued,
}

impl From<ContextAcceptedWork> for enumeray_contracts::management::ContextAcceptedWorkPolicy {
    fn from(value: ContextAcceptedWork) -> Self {
        match value {
            ContextAcceptedWork::Drain => Self::Drain,
            ContextAcceptedWork::CancelQueued => Self::CancelQueued,
        }
    }
}

#[derive(Subcommand)]
pub enum Connector {
    #[command(flatten)]
    Host(enumeray_connector::HostCommand),
    /// List connectors you may administer, one page at a time.
    List {
        #[command(flatten)]
        page: PageArgs,
    },
    /// Create a connector enrollment and save its one-time secret privately.
    Create {
        /// New stable connector ID; independent of its display label.
        id: String,
        /// Human-readable connector label; commands continue to use its stable ID.
        #[arg(long)]
        label: String,
        /// Dispatch capacity for unfinished assignments, including queued Slurm allocations.
        #[arg(long, default_value_t = 4)]
        max_outstanding_assignments: u32,
        /// New private JSON file containing the one-time enrollment secret; existing paths are refused.
        #[arg(long)]
        output: PathBuf,
    },
    /// Issue a replacement one-time enrollment credential and save it privately.
    Enrollment {
        /// Stable connector ID from connector list/get.
        id: String,
        /// New private JSON file for the replacement one-time enrollment secret.
        #[arg(long)]
        output: PathBuf,
    },
    /// Inspect connector registration, capacity and current configuration.
    Get {
        /// Stable connector ID from connector list/get.
        id: String,
    },
    /// Change connector label and dispatch capacity at the observed generation.
    Configure {
        /// Stable connector ID from connector list/get.
        id: String,
        /// Human-readable connector label; commands continue to use its stable ID.
        #[arg(long)]
        label: String,
        /// Dispatch capacity for unfinished assignments, including queued Slurm allocations.
        #[arg(long)]
        max_outstanding_assignments: u32,
        /// Observed connector configuration generation from connector diagnostics/get.
        #[arg(long)]
        generation: i64,
    },
    /// Stop new acceptance and claims while assigned work and delivery finish.
    Maintenance {
        /// Stable connector ID from connector list/get.
        id: String,
        /// Observed connector configuration generation from connector diagnostics/get.
        #[arg(long)]
        generation: i64,
        /// Inspect the observed and proposed change without applying it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Permit acceptance and claims without restoring withdrawn authority.
    Resume {
        /// Stable connector ID from connector list/get.
        id: String,
        /// Observed connector configuration generation from connector diagnostics/get.
        #[arg(long)]
        generation: i64,
        /// Inspect the observed and proposed change without applying it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Withdraw connector execution authority independently of maintenance.
    WithdrawAuthority {
        /// Stable connector ID from connector list/get.
        id: String,
        /// Observed connector configuration generation from connector diagnostics/get.
        #[arg(long)]
        generation: i64,
        /// Inspect the observed and proposed change without applying it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Restore connector authority without clearing maintenance or other revocations.
    RestoreAuthority {
        /// Stable connector ID from connector list/get.
        id: String,
        /// Observed connector configuration generation from connector diagnostics/get.
        #[arg(long)]
        generation: i64,
        /// Inspect the observed and proposed change without applying it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Wait for assigned work and delivery to finish at one maintenance generation.
    WaitMaintenance {
        /// Stable connector ID from connector list/get.
        id: String,
        /// Exact connector generation after entering maintenance; any later change stops this wait.
        #[arg(long)]
        generation: i64,
        /// Positive polling interval in seconds.
        #[arg(long, default_value_t = 2)]
        interval: u64,
        /// Positive local wait budget in seconds; expiry stops observation without cancelling work.
        #[arg(long, default_value_t = 3600)]
        deadline: u64,
    },
    /// Apply a guarded maintenance state from a JSON request; prefer maintenance/resume.
    Update {
        /// Stable connector ID from connector list/get.
        id: String,
        /// JSON object with expected_generation and boolean draining; prefer maintenance/resume.
        #[arg(long)]
        file: PathBuf,
    },
    /// Inspect connector readiness, retained work and delivery blockers.
    Diagnostics {
        /// Stable connector ID from connector list/get.
        id: String,
        /// Explain setup readiness for one context, without requesting checks.
        #[arg(long)]
        context: Option<String>,
    },
    /// Register and manage this connector's execution contexts.
    #[command(subcommand)]
    Context(ConnectorContext),
    /// Grant explicit compute access to one context or discover your available grants.
    #[command(subcommand)]
    Grant(ConnectorGrant),
    /// Inspect co-administrators; owner changes use the ownership generation.
    #[command(subcommand)]
    Member(ConnectorMember),
    /// Propose or accept an explicit transfer of this connector's ownership.
    #[command(subcommand)]
    Ownership(ConnectorOwnership),
    /// List operational run records for this connector.
    Runs {
        #[command(flatten)]
        page: PageArgs,
        /// Stable connector ID from connector list/get.
        id: String,
    },
    /// Read one page of this connector's audit history.
    Audit {
        #[command(flatten)]
        page: PageArgs,
        /// Stable connector ID from connector list/get.
        id: String,
    },
    /// Permanently archive this connector using its ownership generation.
    Archive {
        /// Stable connector ID from connector list/get.
        id: String,
        /// Observed ownership generation; archive is permanent.
        #[arg(long)]
        generation: i64,
    },
}

#[derive(Subcommand)]
pub enum Model {
    /// Save, resume, review and explicitly activate one publication release.
    #[command(subcommand)]
    Release(Release),
    /// Create a runnable author project locally, without credentials or installation.
    Init {
        /// Stable model ID embedded in the generated publication source.
        id: String,
        /// New local starter project directory; any existing path is refused.
        #[arg(long)]
        directory: PathBuf,
        /// Starter wrapper language; adapt the generated wrapper to your scientific code.
        #[arg(long, value_enum, default_value_t = StarterKind::Python)]
        kind: StarterKind,
        /// Starter input/output contract: named JSON values or declared file slots.
        #[arg(long, value_enum, default_value_t = StarterIo::Json)]
        io: StarterIo,
        /// Starter execution driver; Slurm requires site-specific batch/runtime preparation.
        #[arg(long, value_enum, default_value_t = StarterDriver::Process)]
        driver: StarterDriver,
    },
    /// List the model catalogue visible to your account, one page at a time.
    List {
        #[command(flatten)]
        page: PageArgs,
    },
    /// Inspect model metadata, current publication and execution destination.
    Get {
        /// Stable model ID from model list/get or supplied by its author.
        id: String,
    },
    /// Create model catalogue metadata from explicit fields or a publication source.
    Create {
        /// New stable catalogue model ID; must match --file when importing metadata.
        id: String,
        /// Catalogue display name; required unless --file supplies model metadata.
        #[arg(long, required_unless_present = "file", conflicts_with = "file")]
        name: Option<String>,
        /// Catalogue description; defaults to empty and cannot be combined with --file.
        #[arg(long, default_value = "", conflicts_with = "file")]
        description: String,
        /// Import catalogue metadata from the editable publication source.
        #[arg(long, conflicts_with_all = ["name", "description"])]
        file: Option<PathBuf>,
    },
    /// Validate an editable publication source locally without credentials.
    Validate {
        /// Editable publication source JSON; validates local contracts without registering or executing.
        #[arg(long)]
        file: PathBuf,
    },
    /// List this model's immutable publications and candidates.
    Publications {
        #[command(flatten)]
        page: PageArgs,
        /// Stable model ID from model list/get or supplied by its author.
        id: String,
    },
    /// Inspect an exact publication, readiness and captured execution settings.
    Publication {
        /// Stable model ID from model list/get or supplied by its author.
        id: String,
        /// Immutable publication ID from model publications or model release status.
        publication: String,
    },
    /// Permanently revoke this publication while preserving its accepted history.
    RevokePublication {
        /// Stable model ID from model list/get or supplied by its author.
        id: String,
        /// Immutable publication ID from model publications or model release status.
        publication: String,
    },
    /// Retire this publication's installation configuration after its work drains.
    RetirePublication {
        /// Stable model ID from model list/get or supplied by its author.
        id: String,
        /// Immutable publication ID from model publications or model release status.
        publication: String,
        /// Observed model generation from model get; preserve this exact guard when retrying a lost reply.
        #[arg(long)]
        generation: i64,
    },
    /// Activate a validated candidate; omit --expected-current for first publication.
    Publish {
        /// Stable model ID from model list/get or supplied by its author.
        id: String,
        /// Immutable publication ID from model publications or model release status.
        publication: String,
        /// Observed current publication ID; omit only for first activation to guard against concurrent replacement.
        #[arg(long)]
        expected_current: Option<String>,
    },
    /// List this model's run history with model administrator authority.
    Runs {
        #[command(flatten)]
        page: PageArgs,
        /// Stable model ID from model list/get or supplied by its author.
        id: String,
    },
    /// Read one page of this model's audit history.
    Audit {
        #[command(flatten)]
        page: PageArgs,
        /// Stable model ID from model list/get or supplied by its author.
        id: String,
    },
    /// Inspect the model owner, co-administrators and current model generation.
    Members {
        /// Stable model ID from model list/get or supplied by its author.
        id: String,
    },
    /// Add a model co-administrator at the observed model generation.
    MemberAdd {
        /// Stable model ID from model list/get or supplied by its author.
        id: String,
        /// Stable UUID of the target account; use auth whoami to find your own ID.
        #[arg(long)]
        user: uuid::Uuid,
        /// Observed model generation from model get/members; shared by access and governance changes.
        #[arg(long)]
        generation: i64,
    },
    /// Remove a model co-administrator at the observed model generation.
    MemberRemove {
        /// Stable model ID from model list/get or supplied by its author.
        id: String,
        /// Stable UUID of the target account; use auth whoami to find your own ID.
        #[arg(long)]
        user: uuid::Uuid,
        /// Observed model generation from model get/members; shared by access and governance changes.
        #[arg(long)]
        generation: i64,
    },
    /// Propose model ownership transfer to an account that must accept it.
    Transfer {
        /// Stable model ID from model list/get or supplied by its author.
        id: String,
        /// Stable UUID of the proposed owner, who must separately accept the transfer.
        #[arg(long)]
        to: uuid::Uuid,
        /// Observed model generation from model get/members; shared by access and governance changes.
        #[arg(long)]
        generation: i64,
    },
    /// Accept the named model ownership transfer as its recipient.
    AcceptTransfer {
        /// Stable model ID from model list/get or supplied by its author.
        id: String,
        /// Ownership-transfer ID returned by the transfer proposal; only its recipient may accept.
        transfer_id: String,
    },
    /// Permanently archive the model at its observed model generation.
    Archive {
        /// Stable model ID from model list/get or supplied by its author.
        id: String,
        /// Observed model generation from model get/members; shared by access and governance changes.
        #[arg(long)]
        generation: i64,
    },
}

#[derive(Subcommand)]
pub enum Release {
    /// Freeze a locally validated candidate. Does not register or activate it.
    Init {
        /// Stable model ID from model list/get or supplied by its author.
        model: String,
        /// New private release operation directory; freezes one exact candidate and its retry identities.
        #[arg(long)]
        directory: PathBuf,
        /// Editable publication source; initialization freezes a fresh candidate.
        #[arg(long, conflicts_with_all = ["rollback_from", "candidate"], required_unless_present_any = ["rollback_from", "candidate"])]
        file: Option<PathBuf>,
        /// Copy a retained publication's settings into a fresh authorized candidate.
        #[arg(long, conflicts_with_all = ["file", "candidate"])]
        rollback_from: Option<String>,
        /// Adopt an existing publication into this account's private release operation.
        #[arg(long, conflicts_with_all = ["file", "rollback_from"])]
        candidate: Option<String>,
    },
    /// Authorize a named model administrator under the captured compute grant.
    Delegate {
        /// Existing private release operation directory created by model release init.
        directory: PathBuf,
        /// Stable UUID of the model administrator receiving the candidate handoff.
        #[arg(long)]
        user: uuid::Uuid,
        /// Observed model_generation from model release status; shared by model governance changes.
        #[arg(long)]
        generation: i64,
        /// Inspect the observed and proposed change without applying it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Permanently revoke a retained candidate handoff.
    RevokeDelegation {
        /// Existing private release operation directory created by model release init.
        directory: PathBuf,
        /// Candidate handoff UUID retained in model release status.
        delegation_id: uuid::Uuid,
        /// Observed model_generation from model release status; shared by model governance changes.
        #[arg(long)]
        generation: i64,
        /// Inspect the observed and proposed change without applying it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Register the exact saved candidate and wait for readiness. Never activates.
    Resume {
        /// Existing private release operation directory created by model release init.
        directory: PathBuf,
        /// Positive polling interval in seconds.
        #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u64).range(1..))]
        interval: u64,
        /// Positive local wait budget in seconds; expiry stops observation without cancelling work.
        #[arg(long, default_value_t = 3600, value_parser = clap::value_parser!(u64).range(1..))]
        deadline: u64,
    },
    /// Inspect progress and trial evidence without changing the activation guard.
    Status {
        /// Existing private release operation directory created by model release init.
        directory: PathBuf,
    },
    /// Review the comparison and explicitly refresh the expected-current guard.
    Review {
        /// Existing private release operation directory created by model release init.
        directory: PathBuf,
    },
    /// Explicitly activate using the saved review and expected-current guard.
    Activate {
        /// Existing private release operation directory created by model release init.
        directory: PathBuf,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub enum StarterKind {
    /// Generate a Python model wrapper and local tests.
    Python,
    /// Generate an external executable wrapper and local tests.
    Executable,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum StarterIo {
    /// Use named scalar and array JSON inputs and outputs.
    Json,
    /// Include declared file inputs, outputs and local fixtures.
    Files,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum StarterDriver {
    /// Execute the installed wrapper as a direct process.
    Process,
    /// Generate a batch wrapper for site-specific Slurm preparation.
    Slurm,
}

#[derive(Subcommand)]
pub enum Access {
    /// Inspect the model's access mode, catalogue visibility and generation.
    Policy {
        /// Stable model ID from model list/get or supplied by its author.
        model: String,
    },
    /// Preview or update access policy while preserving omitted visibility.
    SetPolicy {
        /// Stable model ID from model list/get or supplied by its author.
        model: String,
        /// open admits eligible users, request requires approval, invite requires an accepted invitation; blocks still apply.
        #[arg(long, value_parser=["open", "request", "invite"])]
        mode: String,
        /// Catalogue visibility; omission preserves the observed value.
        #[arg(long, value_parser=["listed", "unlisted"])]
        visibility: Option<String>,
        /// Previously observed model generation; a changed policy stops the command.
        #[arg(long)]
        generation: i64,
        /// Show the observed and proposed policy without changing it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Create a user or email invitation and save its one-time secret privately.
    Invite {
        /// Stable model ID from model list/get or supplied by its author.
        model: String,
        /// Recipient account UUID; provide exactly one of --user or --email.
        #[arg(long, conflicts_with = "email", required_unless_present = "email")]
        user: Option<uuid::Uuid>,
        /// Recipient email; redemption requires the matching verified identity. Alternative to --user.
        #[arg(long, conflicts_with = "user")]
        email: Option<String>,
        /// New private JSON file containing the invitation secret; existing paths are refused.
        #[arg(long)]
        output: PathBuf,
    },
    /// List this model's invitation metadata without invitation secrets.
    Invitations {
        #[command(flatten)]
        page: PageArgs,
        /// Stable model ID from model list/get or supplied by its author.
        model: String,
    },
    /// Read an invitation token from stdin; tokens never appear in process arguments.
    Accept {
        /// Stable model ID from model list/get or supplied by its author.
        model: String,
    },
    /// Request access to a model, optionally explaining the intended use.
    Request {
        /// Stable model ID from model list/get or supplied by its author.
        model: String,
        /// Optional explanation shown to model administrators; defaults to empty.
        #[arg(long, default_value = "")]
        message: String,
    },
    /// List access requests visible to your account for this model.
    Requests {
        #[command(flatten)]
        page: PageArgs,
        /// Stable model ID from model list/get or supplied by its author.
        model: String,
    },
    /// Approve, reject or withdraw a model access request.
    Decide {
        /// Stable model ID from model list/get or supplied by its author.
        model: String,
        /// Access-request ID from access requests or access request.
        request: String,
        /// approved/rejected for a model administrator; withdrawn for the requesting account.
        #[arg(value_parser=["approved", "rejected", "withdrawn"])]
        decision: String,
    },
    /// Block one account's model access at the observed model generation.
    Block {
        /// Stable model ID from model list/get or supplied by its author.
        model: String,
        /// Stable UUID of the target account; use auth whoami to find your own ID.
        user: uuid::Uuid,
        /// Observed model generation from model get/access policy; shared by access and governance changes.
        #[arg(long)]
        generation: i64,
    },
    /// Grant an account model access and remove its block at the observed model generation.
    Unblock {
        /// Stable model ID from model list/get or supplied by its author.
        model: String,
        /// Stable UUID of the target account; use auth whoami to find your own ID.
        user: uuid::Uuid,
        /// Observed model generation from model get/access policy; shared by access and governance changes.
        #[arg(long)]
        generation: i64,
    },
}

#[derive(Subcommand)]
pub enum Run {
    /// List immutable output collections.
    Collections {
        /// Accepted run UUID from run list/get or the saved acceptance receipt.
        id: String,
    },
    /// List your accepted run history with optional model, state and time filters.
    List {
        #[command(flatten)]
        page: PageArgs,
        #[command(flatten)]
        filters: RunFilters,
    },
    /// Inspect an accepted run by its stable UUID.
    Get {
        /// Accepted run UUID from run list/get or the saved acceptance receipt.
        id: String,
    },
    /// Wait for a run to finish; a local deadline leaves execution unchanged.
    Wait {
        /// Accepted run UUID from run list/get or the saved acceptance receipt.
        id: String,
        /// Positive polling interval in seconds.
        #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u64).range(1..))]
        interval: u64,
        /// Positive local wait budget in seconds; expiry stops observation without cancelling work.
        #[arg(long, default_value_t = 3600, value_parser = clap::value_parser!(u64).range(1..))]
        deadline: u64,
        /// Exit 3 for failed, 4 for cancelled, or 5 for the local deadline; still emit the terminal run.
        #[arg(long)]
        check_outcome: bool,
    },
    /// Inspect the retained terminal diagnostic log archive reference.
    LogArchive {
        /// Accepted run UUID from run list/get or the saved acceptance receipt.
        id: String,
    },
    /// Read one retained log page or follow resumable live run diagnostics.
    Logs {
        #[command(flatten)]
        page: PageArgs,
        /// Accepted run UUID from run list/get or the saved acceptance receipt.
        id: String,
        /// Follow resumable live logs, then drain retained logs after terminal inspection.
        #[arg(long, conflicts_with = "limit")]
        follow: bool,
        /// Run inspection interval in seconds (default 2 when following).
        #[arg(long, requires = "follow", value_parser = clap::value_parser!(u64).range(1..))]
        interval: Option<u64>,
        /// Local observation deadline in seconds (default 3600 when following).
        #[arg(long, requires = "follow", value_parser = clap::value_parser!(u64).range(1..))]
        deadline: Option<u64>,
        /// Create or resume a private checkpoint bound to this API, account and run.
        #[arg(long, requires = "follow", conflicts_with = "cursor")]
        state: Option<PathBuf>,
        /// Explicitly acknowledge missing retained history and continue from its recovery cursor.
        #[arg(long, requires = "follow")]
        allow_gaps: bool,
    },
    /// Read one page of retained run lifecycle events.
    Events {
        #[command(flatten)]
        page: PageArgs,
        /// Accepted run UUID from run list/get or the saved acceptance receipt.
        id: String,
    },
    /// Inspect or save immutable run provenance without downloading output bytes.
    Provenance {
        /// Accepted run UUID from run list/get or the saved acceptance receipt.
        id: String,
        /// New private provenance JSON file; omit to print provenance without saving a file.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// List scientific output artifacts; input files and log archives are separate.
    Artifacts {
        #[command(flatten)]
        page: PageArgs,
        /// Accepted run UUID from run list/get or the saved acceptance receipt.
        id: String,
    },
    /// Request cancellation of an accepted run; repeat after an uncertain reply.
    Cancel {
        /// Accepted run UUID from run list/get or the saved acceptance receipt.
        id: String,
    },
}

#[derive(Subcommand)]
pub enum Artifact {
    /// Inspect artifact metadata, verified content identity and availability.
    Get {
        /// Artifact UUID from upload completion, run artifacts or run log-archive; not a run ID.
        id: String,
    },
    /// Download an exact artifact version with resumable size and SHA-256 verification.
    Download {
        /// Artifact UUID from upload completion, run artifacts or run log-archive; not a run ID.
        id: String,
        /// Local destination file; parent must exist. Reuse this path to resume; conflicting files are preserved.
        #[arg(long)]
        output: PathBuf,
    },
    /// Prepare a verified upload and save retry state before contacting the API.
    Upload {
        /// Existing local source file; retain its path and exact bytes until upload completes.
        file: PathBuf,
        /// Artifact filename stored as metadata, for example dataset.csv; independent of the local source path.
        #[arg(long)]
        name: String,
        /// Media type of uploaded bytes, such as text/csv; does not convert their contents.
        #[arg(long)]
        media_type: String,
        /// Optional scientific metadata JSON object file; defaults to {}.
        #[arg(long)]
        metadata: Option<PathBuf>,
        /// New private upload-state JSON file, saved before API contact; use artifact resume after interruption.
        #[arg(long)]
        state: PathBuf,
    },
    /// Resume reservation, transfer, or completion using the saved operation.
    Resume {
        /// Original private upload-state JSON file; reuses the source digest and request identity.
        #[arg(long)]
        state: PathBuf,
    },
}

#[derive(Subcommand)]
pub enum Collection {
    /// Freeze and upload a directory or explicit member manifest. Retain --state for retries.
    Upload {
        /// Source tree of regular files; alternative to --manifest and requires a uniform --media-type.
        #[arg(
            long,
            required_unless_present = "manifest",
            conflicts_with = "manifest"
        )]
        directory: Option<PathBuf>,
        /// JSON array of relative member paths, media_type and optional scientific_metadata; alternative to --directory.
        #[arg(long)]
        manifest: Option<PathBuf>,
        /// Manifest paths are relative to this root; defaults to the manifest's parent.
        #[arg(long, requires = "manifest")]
        root: Option<PathBuf>,
        /// Logical collection name; independent of member paths.
        #[arg(long)]
        name: String,
        /// Uniform media type for a directory upload; manifest entries declare their own types.
        #[arg(long, required_unless_present = "manifest")]
        media_type: Option<String>,
        /// New private upload-operation directory outside the source tree; retain for retries.
        #[arg(long)]
        state: PathBuf,
    },
    /// Resume the original frozen collection upload using its saved state directory.
    Resume {
        /// Original private collection upload-operation directory; preserves frozen membership and retry identities.
        #[arg(long)]
        state: PathBuf,
    },
    /// Inspect collection identity, sealed digest and current availability.
    Get {
        /// Immutable collection UUID from upload completion or a run output.
        id: uuid::Uuid,
    },
    /// Read one bounded page of immutable collection members.
    Members {
        /// Immutable collection UUID from upload completion or a run output.
        id: uuid::Uuid,
        /// Zero-based next_cursor returned for this immutable collection; omit for its first page.
        #[arg(long, default_value_t = 0)]
        cursor: u32,
    },
    /// Download all or selected members with manifest and checksum verification.
    Download {
        /// Immutable collection UUID from upload completion or a run output.
        id: uuid::Uuid,
        /// New or matching private download directory; verified members appear beneath files/.
        #[arg(long)]
        output: PathBuf,
        /// Exact relative member path; repeat to select several members, or omit to download all.
        #[arg(long)]
        member: Vec<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::Args;
    use clap::CommandFactory;

    #[test]
    fn every_visible_command_flag_and_positional_has_help() {
        fn check(command: &mut clap::Command, path: &str) {
            command.build();
            assert!(
                command
                    .get_about()
                    .is_some_and(|text| !text.to_string().trim().is_empty()),
                "Missing command description: {path}",
            );
            for argument in command.get_arguments().filter(|arg| !arg.is_hide_set()) {
                assert!(
                    argument
                        .get_help()
                        .is_some_and(|text| !text.to_string().trim().is_empty()),
                    "Missing parameter description: {path} {}",
                    argument.get_id(),
                );
            }
            for child in command
                .get_subcommands_mut()
                .filter(|cmd| !cmd.is_hide_set())
            {
                let child_path = format!("{path} {}", child.get_name());
                check(child, &child_path);
            }
        }

        let mut command = Args::command();
        command.clone().debug_assert();
        check(&mut command, "enumeray");
    }
}
