use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "enumeray",
    version,
    about = "Model execution and connector management"
)]
pub struct Args {
    /// API origin. Overrides the selected profile (credentials never cross API origins).
    #[arg(long, global = true)]
    pub api: Option<String>,
    #[arg(long, default_value = "default", global = true)]
    pub profile: String,
    /// Private profile file; defaults to $XDG_CONFIG_HOME/enumeray/profiles.json.
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
    /// Emit machine-readable JSON (NDJSON for following logs); JSON errors go to stderr.
    #[arg(long, global = true)]
    pub json: bool,
    #[arg(long, default_value_t = 60, global = true)]
    pub timeout: u64,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    #[command(flatten)]
    Runtime(enumeray_connector::RuntimeCommand),
    #[command(subcommand)]
    Service(Service),
    #[command(subcommand)]
    Auth(Auth),
    #[command(subcommand)]
    Connector(Connector),
    #[command(subcommand)]
    Model(Model),
    #[command(subcommand)]
    Access(Access),
    /// Inspect accepted run history, diagnostics and lifecycle by ID; execute through analysis directories.
    #[command(subcommand)]
    Run(Run),
    #[command(subcommand)]
    Artifact(Artifact),
    #[command(subcommand)]
    Collection(Collection),
    /// Keep one analysis's inputs, retry identities, run and verified results together.
    #[command(subcommand)]
    Analysis(Analysis),
    /// Explicit HTTP escape hatch for new management APIs; limited to this API origin.
    Api {
        #[arg(value_parser=["GET", "POST", "PUT", "PATCH", "DELETE"])]
        method: String,
        path: String,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long)]
        idempotency_key: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum Analysis {
    /// Reconnect to an accepted run without preparing or submitting another execution.
    Attach {
        #[arg(required_unless_present = "receipt", conflicts_with = "receipt")]
        run_id: Option<String>,
        /// Original saved acceptance receipt; its identity is checked against the service.
        #[arg(long)]
        receipt: Option<PathBuf>,
        #[arg(long)]
        directory: PathBuf,
    },
    /// Import an original prepared request locally. Submit explicitly to reconcile acceptance.
    Import {
        #[arg(long)]
        prepared: PathBuf,
        #[arg(long)]
        directory: PathBuf,
    },
    /// Save a current publication or exact candidate and inputs for review. Does not upload or execute.
    Init {
        model: String,
        /// Trial this exact ready or published candidate using its captured compute authority.
        #[arg(long, value_name = "PUBLICATION")]
        candidate: Option<String>,
        #[arg(long)]
        directory: PathBuf,
        #[arg(long, required_unless_present = "inputs", conflicts_with = "inputs")]
        example: Option<String>,
        #[arg(long, required_unless_present = "example")]
        inputs: Option<PathBuf>,
        /// Bind a declared file slot to an original local source: SLOT=PATH.
        #[arg(long, value_name = "SLOT=PATH")]
        file: Vec<String>,
        /// Bind a collection slot to a local directory: SLOT=PATH.
        #[arg(long, value_name = "SLOT=PATH")]
        collection: Vec<String>,
        /// Bind a collection slot to a member manifest: SLOT=PATH.
        #[arg(long, value_name = "SLOT=PATH")]
        collection_manifest: Vec<String>,
        /// Select a declared media type: SLOT=TYPE. Inferred only for a sole allowed type.
        #[arg(long, value_name = "SLOT=TYPE")]
        media_type: Vec<String>,
        /// Scientific upload metadata JSON object: SLOT=PATH. Defaults to {}.
        #[arg(long, value_name = "SLOT=PATH")]
        metadata: Vec<String>,
    },
    /// Freeze inputs, upload files and submit. Repeat after interruption to resume this operation.
    Submit { directory: PathBuf },
    /// Inspect saved preparation and current accepted run progress.
    Status { directory: PathBuf },
    /// Read retained logs or follow the accepted run using this directory's private checkpoint.
    Logs {
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
    Cancel { directory: PathBuf },
    /// Wait for the accepted run; a local deadline leaves execution unchanged.
    Wait {
        directory: PathBuf,
        #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u64).range(1..))]
        interval: u64,
        #[arg(long, default_value_t = 3600, value_parser = clap::value_parser!(u64).range(1..))]
        deadline: u64,
        /// Exit 3 for failed, 4 for cancelled, or 5 for the local deadline.
        #[arg(long)]
        check_outcome: bool,
    },
    /// Collect terminal evidence and checksum-verified files; resume without overwriting conflicts.
    Collect {
        directory: PathBuf,
        /// Download these exact collection members: SLOT=PATH. Other output bytes remain deferred.
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
    Status,
    Limits,
    SetLimits {
        #[arg(long)]
        max_queued_runs: u32,
        #[arg(long)]
        max_queued_runs_per_user: u32,
        #[arg(long)]
        generation: i64,
    },
    Suspend {
        user: uuid::Uuid,
        #[arg(long)]
        reason: String,
    },
    Resume {
        user: uuid::Uuid,
        #[arg(long)]
        reason: String,
    },
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
    Refresh,
    Logout,
    Whoami,
    /// List configured profiles without credentials.
    Profiles,
    /// Create/select this profile's API origin; changing origin clears old credentials.
    Configure {
        api_url: String,
    },
    /// Create a scoped, expiring token and save the secret to a private file.
    TokenCreate {
        name: String,
        #[arg(long, value_delimiter = ',', required = true)]
        scopes: Vec<String>,
        #[arg(long, default_value_t = 2_592_000)]
        expires_in_seconds: u64,
        #[arg(long)]
        output: PathBuf,
    },
    TokenList {
        #[command(flatten)]
        page: PageArgs,
    },
    TokenRevoke {
        id: String,
    },
}

#[derive(Subcommand)]
pub enum ConnectorContext {
    /// Request/resume a publication-independent host check using a saved identity.
    Preflight {
        connector: String,
        context: String,
        #[arg(long)]
        directory: PathBuf,
        #[arg(long,default_value_t=2,value_parser=clap::value_parser!(u64).range(1..=60))]
        interval: u64,
        #[arg(long,default_value_t=300,value_parser=clap::value_parser!(u64).range(1..=3600))]
        deadline: u64,
    },
    /// Read the latest host-check attempt/report without initiating a check.
    PreflightStatus { connector: String, context: String },
    /// List execution contexts visible under this connector.
    List {
        #[command(flatten)]
        page: PageArgs,
        id: String,
    },
    /// Register a context from a versioned execution-context manifest.
    Create {
        id: String,
        #[arg(long)]
        file: PathBuf,
    },
    /// Change descriptive location and data-access disclosure.
    Update {
        connector: String,
        context: String,
        #[arg(long)]
        location: String,
        #[arg(long)]
        disclosure: String,
        #[arg(long)]
        generation: i64,
    },
    /// Inspect context lifecycle, retained work and retirement readiness.
    Get { connector: String, context: String },
    /// Inspect replacement dependencies and actions for connector owners and authors.
    ReplacementPlan {
        connector: String,
        context: String,
        #[arg(long)]
        replacement: Option<String>,
        #[arg(long, value_enum, default_value = "publications")]
        section: ContextDependencySection,
        #[command(flatten)]
        page: PageArgs,
    },
    /// Close new work and explicitly drain or cancel queued runs.
    Cutover {
        connector: String,
        context: String,
        #[arg(long)]
        replacement: Option<String>,
        #[arg(long, value_enum)]
        accepted_work: ContextAcceptedWork,
        #[arg(long)]
        generation: i64,
        #[arg(long)]
        dry_run: bool,
    },
    /// Reopen future operations while retaining cancellations and configuration releases.
    Reopen {
        connector: String,
        context: String,
        #[arg(long)]
        generation: i64,
        #[arg(long)]
        dry_run: bool,
    },
    /// Request retirement of obsolete publication configuration on a draining context.
    ReleaseConfigurations {
        connector: String,
        context: String,
        #[arg(long)]
        generation: i64,
        #[arg(long)]
        dry_run: bool,
    },
    /// Permanently archive a context after work, delivery and configuration finish.
    Retire {
        connector: String,
        context: String,
        #[arg(long)]
        generation: i64,
        #[arg(long)]
        dry_run: bool,
    },
    /// Wait for context retirement readiness without shutting down the connector.
    WaitRetirement {
        connector: String,
        context: String,
        #[arg(long)]
        generation: i64,
        #[arg(long, default_value_t = 2)]
        interval: u64,
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
        id: String,
    },
    /// Give a user explicit compute access to one execution context.
    Create {
        id: String,
        #[arg(long)]
        context: String,
        #[arg(long)]
        user: uuid::Uuid,
        #[arg(long, default_value_t = 100)]
        max_queued_runs: u32,
        #[arg(long, default_value_t = 4)]
        max_outstanding_assignments: u32,
    },
    /// Inspect current limits, generation and permanent revocation state.
    Get {
        connector: String,
        grant_id: uuid::Uuid,
    },
    /// Replace both capacity limits at the observed grant generation.
    Update {
        connector: String,
        grant_id: uuid::Uuid,
        #[arg(long)]
        max_queued_runs: u32,
        #[arg(long)]
        max_outstanding_assignments: u32,
        #[arg(long)]
        generation: i64,
    },
    /// Permanently revoke one grant without substituting another grant.
    Revoke { id: String, grant_id: String },
    /// Discover the compute grants available to your authenticated account.
    Available {
        #[command(flatten)]
        page: PageArgs,
    },
}

#[derive(Subcommand)]
pub enum ConnectorMember {
    /// Inspect the owner, co-administrators and ownership generation.
    List { id: String },
    /// Add a co-administrator using the observed ownership generation.
    Add {
        id: String,
        #[arg(long)]
        user: uuid::Uuid,
        /// Observed ownership generation; independent of execution configuration.
        #[arg(long)]
        generation: i64,
    },
    /// Remove a co-administrator without revoking independent compute grants.
    Remove {
        id: String,
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
        id: String,
        #[arg(long)]
        to: uuid::Uuid,
        /// Observed ownership generation; independent of execution configuration.
        #[arg(long)]
        generation: i64,
    },
    /// Accept a transfer only for the explicitly selected connector.
    Accept { id: String, transfer_id: String },
}

#[derive(Clone, Copy, ValueEnum)]
pub enum ContextDependencySection {
    Publications,
    Grants,
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
    Drain,
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
    List {
        #[command(flatten)]
        page: PageArgs,
    },
    /// Create a connector enrollment and save its one-time secret privately.
    Create {
        id: String,
        #[arg(long)]
        label: String,
        #[arg(long, default_value_t = 4)]
        max_outstanding_assignments: u32,
        #[arg(long)]
        output: PathBuf,
    },
    /// Issue a replacement one-time enrollment credential and save it privately.
    Enrollment {
        id: String,
        #[arg(long)]
        output: PathBuf,
    },
    Get {
        id: String,
    },
    /// Change connector label and dispatch capacity at the observed generation.
    Configure {
        id: String,
        #[arg(long)]
        label: String,
        #[arg(long)]
        max_outstanding_assignments: u32,
        #[arg(long)]
        generation: i64,
    },
    /// Stop new acceptance and claims while assigned work and delivery finish.
    Maintenance {
        id: String,
        #[arg(long)]
        generation: i64,
        #[arg(long)]
        dry_run: bool,
    },
    /// Permit acceptance and claims without restoring withdrawn authority.
    Resume {
        id: String,
        #[arg(long)]
        generation: i64,
        #[arg(long)]
        dry_run: bool,
    },
    /// Withdraw connector execution authority independently of maintenance.
    WithdrawAuthority {
        id: String,
        #[arg(long)]
        generation: i64,
        #[arg(long)]
        dry_run: bool,
    },
    /// Restore connector authority without clearing maintenance or other revocations.
    RestoreAuthority {
        id: String,
        #[arg(long)]
        generation: i64,
        #[arg(long)]
        dry_run: bool,
    },
    /// Wait for assigned work and delivery to finish at one maintenance generation.
    WaitMaintenance {
        id: String,
        #[arg(long)]
        generation: i64,
        #[arg(long, default_value_t = 2)]
        interval: u64,
        #[arg(long, default_value_t = 3600)]
        deadline: u64,
    },
    /// Apply a guarded maintenance state from a JSON request; prefer maintenance/resume.
    Update {
        id: String,
        #[arg(long)]
        file: PathBuf,
    },
    Diagnostics {
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
    Runs {
        #[command(flatten)]
        page: PageArgs,
        id: String,
    },
    Audit {
        #[command(flatten)]
        page: PageArgs,
        id: String,
    },
    Archive {
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
        id: String,
        #[arg(long)]
        directory: PathBuf,
        #[arg(long, value_enum, default_value_t = StarterKind::Python)]
        kind: StarterKind,
        #[arg(long, value_enum, default_value_t = StarterIo::Json)]
        io: StarterIo,
        #[arg(long, value_enum, default_value_t = StarterDriver::Process)]
        driver: StarterDriver,
    },
    List {
        #[command(flatten)]
        page: PageArgs,
    },
    Get {
        id: String,
    },
    Create {
        id: String,
        #[arg(long, required_unless_present = "file", conflicts_with = "file")]
        name: Option<String>,
        #[arg(long, default_value = "", conflicts_with = "file")]
        description: String,
        /// Import catalogue metadata from the editable publication source.
        #[arg(long, conflicts_with_all = ["name", "description"])]
        file: Option<PathBuf>,
    },
    /// Validate an editable publication source locally without credentials.
    Validate {
        #[arg(long)]
        file: PathBuf,
    },
    Publications {
        #[command(flatten)]
        page: PageArgs,
        id: String,
    },
    Publication {
        id: String,
        publication: String,
    },
    /// Permanently revoke this publication while preserving its accepted history.
    RevokePublication {
        id: String,
        publication: String,
    },
    /// Retire this publication's installation configuration after its work drains.
    RetirePublication {
        id: String,
        publication: String,
        #[arg(long)]
        generation: i64,
    },
    /// Activate a validated candidate; omit --expected-current for first publication.
    Publish {
        id: String,
        publication: String,
        #[arg(long)]
        expected_current: Option<String>,
    },
    Runs {
        #[command(flatten)]
        page: PageArgs,
        id: String,
    },
    Audit {
        #[command(flatten)]
        page: PageArgs,
        id: String,
    },
    Members {
        id: String,
    },
    MemberAdd {
        id: String,
        #[arg(long)]
        user: uuid::Uuid,
        #[arg(long)]
        generation: i64,
    },
    MemberRemove {
        id: String,
        #[arg(long)]
        user: uuid::Uuid,
        #[arg(long)]
        generation: i64,
    },
    Transfer {
        id: String,
        #[arg(long)]
        to: uuid::Uuid,
        #[arg(long)]
        generation: i64,
    },
    AcceptTransfer {
        id: String,
        transfer_id: String,
    },
    Archive {
        id: String,
        #[arg(long)]
        generation: i64,
    },
}

#[derive(Subcommand)]
pub enum Release {
    /// Freeze a locally validated candidate. Does not register or activate it.
    Init {
        model: String,
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
        directory: PathBuf,
        #[arg(long)]
        user: uuid::Uuid,
        #[arg(long)]
        generation: i64,
        #[arg(long)]
        dry_run: bool,
    },
    /// Permanently revoke a retained candidate handoff.
    RevokeDelegation {
        directory: PathBuf,
        delegation_id: uuid::Uuid,
        #[arg(long)]
        generation: i64,
        #[arg(long)]
        dry_run: bool,
    },
    /// Register the exact saved candidate and wait for readiness. Never activates.
    Resume {
        directory: PathBuf,
        #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u64).range(1..))]
        interval: u64,
        #[arg(long, default_value_t = 3600, value_parser = clap::value_parser!(u64).range(1..))]
        deadline: u64,
    },
    /// Inspect progress and trial evidence without changing the activation guard.
    Status { directory: PathBuf },
    /// Review the comparison and explicitly refresh the expected-current guard.
    Review { directory: PathBuf },
    /// Explicitly activate using the saved review and expected-current guard.
    Activate { directory: PathBuf },
}

#[derive(Clone, Copy, ValueEnum)]
pub enum StarterKind {
    Python,
    Executable,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum StarterIo {
    Json,
    Files,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum StarterDriver {
    Process,
    Slurm,
}

#[derive(Subcommand)]
pub enum Access {
    Policy {
        model: String,
    },
    /// Preview or update access policy while preserving omitted visibility.
    SetPolicy {
        model: String,
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
    Invite {
        model: String,
        #[arg(long, conflicts_with = "email", required_unless_present = "email")]
        user: Option<uuid::Uuid>,
        #[arg(long, conflicts_with = "user")]
        email: Option<String>,
        #[arg(long)]
        output: PathBuf,
    },
    Invitations {
        #[command(flatten)]
        page: PageArgs,
        model: String,
    },
    /// Read an invitation token from stdin; tokens never appear in process arguments.
    Accept {
        model: String,
    },
    Request {
        model: String,
        #[arg(long, default_value = "")]
        message: String,
    },
    Requests {
        #[command(flatten)]
        page: PageArgs,
        model: String,
    },
    Decide {
        model: String,
        request: String,
        #[arg(value_parser=["approved", "rejected", "withdrawn"])]
        decision: String,
    },
    Block {
        model: String,
        user: uuid::Uuid,
        #[arg(long)]
        generation: i64,
    },
    Unblock {
        model: String,
        user: uuid::Uuid,
        #[arg(long)]
        generation: i64,
    },
}

#[derive(Subcommand)]
pub enum Run {
    /// List immutable output collections.
    Collections {
        id: String,
    },
    List {
        #[command(flatten)]
        page: PageArgs,
        #[command(flatten)]
        filters: RunFilters,
    },
    Get {
        id: String,
    },
    Wait {
        id: String,
        #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u64).range(1..))]
        interval: u64,
        #[arg(long, default_value_t = 3600, value_parser = clap::value_parser!(u64).range(1..))]
        deadline: u64,
        /// Exit 3 for failed, 4 for cancelled, or 5 for the local deadline; still emit the terminal run.
        #[arg(long)]
        check_outcome: bool,
    },
    LogArchive {
        id: String,
    },
    Logs {
        #[command(flatten)]
        page: PageArgs,
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
    Events {
        #[command(flatten)]
        page: PageArgs,
        id: String,
    },
    Provenance {
        id: String,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    Artifacts {
        #[command(flatten)]
        page: PageArgs,
        id: String,
    },
    Cancel {
        id: String,
    },
}

#[derive(Subcommand)]
pub enum Artifact {
    Get {
        id: String,
    },
    Download {
        id: String,
        #[arg(long)]
        output: PathBuf,
    },
    /// Prepare a verified upload and save retry state before contacting the API.
    Upload {
        file: PathBuf,
        #[arg(long)]
        name: String,
        #[arg(long)]
        media_type: String,
        #[arg(long)]
        metadata: Option<PathBuf>,
        #[arg(long)]
        state: PathBuf,
    },
    /// Resume reservation, transfer, or completion using the saved operation.
    Resume {
        #[arg(long)]
        state: PathBuf,
    },
}

#[derive(Subcommand)]
pub enum Collection {
    /// Freeze and upload a directory or explicit member manifest. Retain --state for retries.
    Upload {
        #[arg(
            long,
            required_unless_present = "manifest",
            conflicts_with = "manifest"
        )]
        directory: Option<PathBuf>,
        #[arg(long)]
        manifest: Option<PathBuf>,
        /// Manifest paths are relative to this root; defaults to the manifest's parent.
        #[arg(long, requires = "manifest")]
        root: Option<PathBuf>,
        #[arg(long)]
        name: String,
        #[arg(long, required_unless_present = "manifest")]
        media_type: Option<String>,
        #[arg(long)]
        state: PathBuf,
    },
    Resume {
        #[arg(long)]
        state: PathBuf,
    },
    Get {
        id: uuid::Uuid,
    },
    Members {
        id: uuid::Uuid,
        #[arg(long, default_value_t = 0)]
        cursor: u32,
    },
    Download {
        id: uuid::Uuid,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        member: Vec<String>,
    },
}
