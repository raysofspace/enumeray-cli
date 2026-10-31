//! One durable owner workflow; host credentials and scientific executions retain
//! their existing independent identities and authorization boundaries.
mod prompt;
mod trial;
use crate::{
    app::{Completion, Failure},
    args::{Args, Auth, Connector, Setup, SetupCommand, SetupMode},
    config,
};
use enumeray_client::{EnumerayClient, Error};
use enumeray_contracts::{execution::ExecutionMode, management::*, setup::*};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, IsTerminal},
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};
use uuid::Uuid;
fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    manifest_version: u32,
    host: HostSetupSpec,
    connector: ConnectorCreate,
    startup: String,
    #[serde(default)]
    first_model: Option<FirstModel>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FirstModel {
    model_id: String,
    #[serde(default)]
    project: Option<PathBuf>,
    #[serde(default)]
    demo: bool,
    /// Optional already installed release. Otherwise choose a new child of an approved root.
    #[serde(default)]
    installed_release: Option<PathBuf>,
    #[serde(default)]
    inputs: Option<PathBuf>,
    #[serde(default)]
    example: Option<String>,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    collections: Vec<String>,
    #[serde(default)]
    collection_manifests: Vec<String>,
    #[serde(default)]
    media_types: Vec<String>,
    #[serde(default)]
    metadata: Vec<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    manifest_version: u32,
    operation_id: Uuid,
    directory: PathBuf,
    profile: String,
    profile_file: PathBuf,
    input: Input,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Progress {
    manifest_version: u32,
    plan_sha256: String,
    user_id: Option<Uuid>,
    approved: bool,
    development_consent: bool,
    connector_started: bool,
    connector_registered: bool,
    context_started: bool,
    context_registered: bool,
    grant: Option<GrantCreate>,
    grant_registered: bool,
    #[serde(default)]
    enrollment_paths: Vec<PathBuf>,
    #[serde(default)]
    pending_replacement: Option<PathBuf>,
    #[serde(default)]
    trial_approved: bool,
    #[serde(default)]
    trial: trial::Progress,
}
fn digest_bytes(bytes: impl AsRef<[u8]>) -> String {
    Sha256::digest(bytes.as_ref())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn hash(value: &impl Serialize) -> Result<String, Error> {
    Ok(digest_bytes(serde_json::to_vec(value)?))
}
fn profile_path(args: &Args) -> Result<PathBuf, Error> {
    let p = args
        .config
        .clone()
        .map(Ok)
        .unwrap_or_else(config::default_path)?;
    if p.is_absolute() {
        Ok(p)
    } else {
        Ok(std::env::current_dir()?.join(p))
    }
}
fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, Error> {
    config::require_private(path)?;
    config::read_json(path)
}
fn check_directory(path: &Path) -> Result<(), Error> {
    let m = fs::symlink_metadata(path)?;
    if !m.is_dir() || m.file_type().is_symlink() || m.permissions().mode() & 0o077 != 0 {
        return Err(invalid(
            "Setup directory must be a private real directory (0700).",
        ));
    }
    Ok(())
}
fn save(directory: &Path, progress: &Progress) -> Result<(), Error> {
    config::write_private(&directory.join("state.json"), progress, true)
}
fn complete(value: Value, exit: u8) -> Result<Completion, Failure> {
    Ok(Completion {
        value: Some(value),
        exit,
    })
}
fn interactive(options: &Setup) -> bool {
    !options.non_interactive && io::stdin().is_terminal()
}
pub(crate) fn execute(args: &Args, options: &Setup) -> Result<Completion, Failure> {
    if let Some(SetupCommand::ApplyHost {
        plan,
        side,
        approve,
        enrollment_file,
        executable_source,
    }) = &options.command
    {
        return enumeray_connector::apply_host_configuration(
            plan,
            if side == "connector" {
                HostSide::Connector
            } else {
                HostSide::Execution
            },
            approve,
            enrollment_file.as_deref(),
            executable_source.as_deref(),
        )
        .map_err(invalid)
        .map(|v| Completion {
            value: Some(v),
            exit: 0,
        })
        .map_err(Into::into);
    }
    let inspecting = matches!(options.command, Some(SetupCommand::Status { .. }));
    let directory = match &options.command {
        Some(SetupCommand::Status { directory }) => directory.clone(),
        _ => options
            .directory
            .clone()
            .unwrap_or(std::env::current_dir()?.join("enumeray-setup")),
    };
    let new = !directory.exists();
    if new && inspecting {
        return Err(invalid("Setup directory does not exist.").into());
    }
    if new {
        fs::DirBuilder::new().mode(0o700).create(&directory)?;
    }
    check_directory(&directory)?;
    let directory = directory.canonicalize()?;
    // Inspection reads atomic state without waiting for the writer's network operations.
    let _writer = if inspecting {
        None
    } else {
        Some(config::lock_private(
            &directory.join("setup"),
            "This setup is already being resumed; inspect status or wait for its writer.",
        )?)
    };
    let plan_path = directory.join("setup.json");
    let (plan, mut progress) = if plan_path.exists() {
        let plan: Plan = read(&plan_path)?;
        let p: Progress = read(&directory.join("state.json"))?;
        if plan.manifest_version != 1
            || p.manifest_version != 1
            || plan.operation_id.is_nil()
            || p.plan_sha256 != hash(&plan)?
        {
            return Err(invalid(
                "Setup evidence changed; preserve this directory and inspect the original plan.",
            )
            .into());
        }
        if plan.directory != directory {
            return Err(invalid("This setup checkpoint belongs to its original directory; restore it at that path rather than forking its candidate and trial identities.").into());
        }
        if plan.profile != args.profile || plan.profile_file != profile_path(args)? {
            return Err(
                invalid("Use this setup's original profile and private profile file.").into(),
            );
        }
        if let Some(file) = &options.file {
            let supplied: Input = config::read_json(file)?;
            if hash(&supplied)? != hash(&plan.input)? {
                return Err(invalid(
                    "Input differs from the frozen setup plan; use a new setup directory.",
                )
                .into());
            }
        }
        if options.mode.is_some_and(|m| {
            (m == SetupMode::Development)
                != (plan.input.host.context.definition.execution_mode == ExecutionMode::Development)
        }) {
            return Err(invalid("Selected mode differs from the saved setup.").into());
        }
        (plan, p)
    } else {
        if !new && fs::read_dir(&directory)?.any(|e| e.is_ok_and(|e| e.file_name() != "setup.lock"))
        {
            return Err(invalid("Existing directory has no complete setup checkpoint; preserve it and select a new directory.").into());
        }
        let input = if let Some(file) = &options.file {
            config::read_json(file)?
        } else if interactive(options) {
            prompt::input(args, options)?
        } else {
            return Err(invalid("Supply --file with a complete setup input, or run setup in a terminal for guidance.").into());
        };
        validate(&input)?;
        let plan = Plan {
            manifest_version: 1,
            operation_id: Uuid::new_v4(),
            directory: directory.clone(),
            profile: args.profile.clone(),
            profile_file: profile_path(args)?,
            input,
        };
        let progress = Progress {
            manifest_version: 1,
            plan_sha256: hash(&plan)?,
            user_id: None,
            approved: false,
            development_consent: false,
            connector_started: false,
            connector_registered: false,
            context_started: false,
            context_registered: false,
            grant: None,
            grant_registered: false,
            enrollment_paths: vec![],
            pending_replacement: None,
            trial_approved: false,
            trial: Default::default(),
        };
        // Publish progress first; a missing plan can never conceal an API mutation.
        config::write_private(&directory.join("state.json"), &progress, false)?;
        config::write_private(&plan_path, &plan, false)?;
        let host = enumeray_connector::host_configuration_plan(plan.input.host.clone())
            .map_err(invalid)?;
        config::write_private(&directory.join("host-plan.json"), &host, false)?;
        (plan, progress)
    };
    validate(&plan.input)?;
    if options
        .approve
        .as_ref()
        .is_some_and(|digest| digest != &progress.plan_sha256)
    {
        return Err(invalid("Approval does not match the saved setup plan digest.").into());
    }
    if options.mode.is_some_and(|mode| {
        (mode == SetupMode::Development)
            != (plan.input.host.context.definition.execution_mode == ExecutionMode::Development)
    }) {
        return Err(invalid("Selected mode differs from the saved setup.").into());
    }
    let host =
        enumeray_connector::host_configuration_plan(plan.input.host.clone()).map_err(invalid)?;
    let host_path = directory.join("host-plan.json");
    if !host_path.exists() && !inspecting {
        config::write_private(&host_path, &host, false)?;
    }
    if host_path.exists() && hash(&read::<HostConfigurationPlan>(&host_path)?)? != hash(&host)? {
        return Err(invalid("Saved host plan differs from the approved setup evidence.").into());
    }
    if let Some(api) = &args.api {
        let selected = EnumerayClient::new(api, None, Duration::from_secs(args.timeout))?;
        if selected.base_url().trim_end_matches('/')
            != plan.input.host.api_base_url.trim_end_matches('/')
        {
            return Err(invalid("The selected API differs from the saved setup origin.").into());
        }
    }
    if inspecting {
        return inspect(args, &directory, &plan, &progress);
    }
    if !progress.approved {
        let overview = summary(&directory, &plan, &progress, "review_required");
        eprintln!(
            "Review connector {}, context {}, mode {}, startup {}. Host actions:\n{}\nPlan SHA-256: {}",
            plan.input.connector.id,
            plan.input.host.context.id,
            enumeray_contracts::setup::mode_name(&plan.input.host.context.definition),
            plan.input.startup,
            serde_json::to_string_pretty(
                &enumeray_connector::host_configuration_plan(plan.input.host.clone())
                    .map_err(invalid)?
                    .actions
            )?,
            progress.plan_sha256
        );
        let approve = options.approve.as_deref() == Some(progress.plan_sha256.as_str())
            || (interactive(options) && prompt::yes("Approve this exact setup plan", false)?);
        if options.approve.is_some() && !approve {
            return Err(invalid("Approval does not match the saved setup plan digest.").into());
        }
        if !approve {
            return complete(overview, 1);
        }
        progress.approved = true;
        save(&directory, &progress)?;
    }
    if plan.input.host.context.definition.execution_mode == ExecutionMode::Development
        && !progress.development_consent
    {
        eprintln!(
            "Trusted development: model code runs as your OS user and can access that user's files and connector credentials."
        );
        if !(options.allow_development_execution
            || (interactive(options)
                && prompt::yes("Allow trusted same-user development execution", false)?))
        {
            return complete(
                summary(&directory, &plan, &progress, "development_consent_required"),
                1,
            );
        }
        progress.development_consent = true;
        save(&directory, &progress)?;
    }
    let client = login(args, &plan, options)?;
    let user = client.me()?.user_id;
    if progress.user_id.is_some_and(|id| id != user) {
        return Err(invalid(
            "Setup belongs to another account; renew its original login and resume this directory.",
        )
        .into());
    }
    progress.user_id = Some(user);
    save(&directory, &progress)?;
    register(&client, options, &directory, &plan, &mut progress)?;
    if plan.input.host.context.definition.execution_mode == ExecutionMode::Development {
        enumeray_connector::prepare_development(&plan.input.host, progress.development_consent)
            .map_err(invalid)?;
        let token = Path::new(&plan.input.host.token_file);
        if !token.exists() {
            let Some(enrollment) = progress.enrollment_paths.last() else {
                return complete(
                    summary(&directory, &plan, &progress, "enrollment_required"),
                    1,
                );
            };
            let output = Command::new(&plan.input.host.connector_executable)
                .args(["connector", "enroll", "--directory"])
                .arg(token.parent().unwrap())
                .args([
                    "--api-base-url",
                    &plan.input.host.api_base_url,
                    "--enrollment-file",
                ])
                .arg(enrollment)
                .env_remove("ENUMERAY_TOKEN")
                .stdout(Stdio::null())
                .status()?;
            if !output.success() {
                return Err(invalid("Enrollment was not confirmed; retain its identity directory and enrollment document, then resume this setup.").into());
            }
        }
        let output = Command::new(&plan.input.host.connector_executable)
            .args(["connector", "preflight", "--plan"])
            .arg(directory.join("host-plan.json"))
            .arg("--allow-development-execution")
            .env_remove("ENUMERAY_TOKEN")
            .output()?;
        let report: Value = serde_json::from_slice(&output.stdout).map_err(|_| {
            invalid(
                "Local preflight did not return a report; inspect its host prerequisite failure.",
            )
        })?;
        config::write_private(&directory.join("local-preflight.json"), &report, true)?;
        if !output.status.success() {
            let mut v = summary(&directory, &plan, &progress, "host_blocked");
            v["local_preflight"] = report;
            return complete(v, 1);
        }
    }
    let diag = client
        .management()
        .connector_context_diagnostics(&plan.input.connector.id, &plan.input.host.context.id)?;
    let ready = diag.setup.as_ref().is_some_and(|s| s.ready);
    let connected = diag.setup.as_ref().is_some_and(|s| s.connected);
    if !connected {
        let mut v = summary(&directory, &plan, &progress, "start_required");
        v["diagnostics"] = serde_json::to_value(diag)?;
        return complete(v, 1);
    }
    if !ready {
        let mut attempts = fs::read_dir(&directory)?
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with("preflight-"))
            .map(|e| e.path())
            .collect::<Vec<_>>();
        attempts.sort();
        let mut op = attempts
            .last()
            .cloned()
            .unwrap_or(directory.join("preflight-0000"));
        let status = client
            .management()
            .context_preflight(&plan.input.connector.id, &plan.input.host.context.id)?;
        if needs_new_preflight(&status) {
            op = directory.join(format!("preflight-{:04}", attempts.len()));
        }
        let result = crate::preflight::execute(
            &client,
            &plan.input.connector.id,
            &plan.input.host.context.id,
            &op,
            2,
            300,
        )?;
        if result.exit != 0 {
            return Ok(result);
        }
    }
    let diag = client
        .management()
        .connector_context_diagnostics(&plan.input.connector.id, &plan.input.host.context.id)?;
    if !diag.setup.as_ref().is_some_and(|s| s.ready) {
        let mut v = summary(&directory, &plan, &progress, "host_blocked");
        v["diagnostics"] = serde_json::to_value(diag)?;
        return complete(v, 1);
    }
    if plan.input.first_model.is_some() {
        return trial::execute(args, options, &client, &directory, &plan, &mut progress);
    }
    complete(summary(&directory, &plan, &progress, "ready"), 0)
}
fn needs_new_preflight(status: &ContextPreflightStatus) -> bool {
    status
        .attempt
        .as_ref()
        .is_some_and(|a| a.state != AttemptState::Pending)
        && (!status.ready || status.validity != ReportValidity::Current)
}
fn validate(input: &Input) -> Result<(), Error> {
    if input.manifest_version != 1
        || input.connector.id != input.host.context.connector_id
        || !matches!(input.startup.as_str(), "foreground" | "background")
        || input.host.install_service != (input.startup == "background")
    {
        return Err(invalid(
            "Setup requires version 1, matching connector identities and explicit foreground/background startup.",
        ));
    }
    enumeray_connector::configuration_digest(&input.host).map_err(invalid)?;
    let api = EnumerayClient::new(&input.host.api_base_url, None, Duration::from_secs(60))?;
    let _ = api;
    if let Some(first) = &input.first_model {
        if first.demo == first.project.is_some()
            || !enumeray_contracts::models::validation::valid_model_id(&first.model_id)
        {
            return Err(invalid(
                "Select exactly one demo or project and a valid first-model ID.",
            ));
        }
        for path in [&first.project, &first.inputs, &first.installed_release]
            .into_iter()
            .flatten()
        {
            if !path.is_absolute()
                || path
                    .components()
                    .any(|p| matches!(p, std::path::Component::ParentDir))
            {
                return Err(invalid(
                    "Saved project, input and installation paths must be absolute.",
                ));
            }
        }
        for binding in first
            .files
            .iter()
            .chain(&first.collections)
            .chain(&first.collection_manifests)
        {
            let (slot, value) = binding
                .split_once('=')
                .filter(|(slot, value)| !slot.is_empty() && !value.is_empty())
                .ok_or_else(|| invalid("Local input bindings must use SLOT=/absolute/path."))?;
            let path = Path::new(value);
            if !path.is_absolute()
                || path
                    .components()
                    .any(|p| matches!(p, std::path::Component::ParentDir))
                || slot.trim() != slot
            {
                return Err(invalid(
                    "Saved local input bindings must use SLOT=/absolute/path without parent traversal.",
                ));
            }
        }
        if first.example.is_some() == first.inputs.is_some() {
            return Err(invalid(
                "Choose exactly one declared input example or scientific inputs file for the first trial.",
            ));
        }
    }
    Ok(())
}
fn login(args: &Args, plan: &Plan, options: &Setup) -> Result<EnumerayClient, Error> {
    let profiles = config::load(&plan.profile_file)?;
    if let Some(profile) = profiles.profiles.get(&plan.profile)
        && profile.api_url.trim_end_matches('/')
            != plan.input.host.api_base_url.trim_end_matches('/')
    {
        return Err(invalid(
            "The saved profile now selects another API; restore the original origin without overwriting its credentials.",
        ));
    }
    if !profiles.profiles.contains_key(&plan.profile) {
        if !interactive(options) {
            return Err(invalid(
                "Configure and authenticate the saved profile, then resume this setup.",
            ));
        }
        crate::commands::auth(
            args,
            &Auth::Configure {
                api_url: plan.input.host.api_base_url.clone(),
            },
            &plan.profile_file,
        )?;
    }
    let client = config::client(args, &config::load(&plan.profile_file)?)?;
    match client.me() {
        Ok(_) => Ok(client),
        Err(e) if interactive(options) => {
            eprintln!("Authentication needs attention: {e}");
            if !prompt::yes("Log in again to the original API/account", false)? {
                return Err(e);
            }
            crate::commands::auth(
                args,
                &Auth::Login {
                    token_stdin: false,
                    no_browser: false,
                },
                &plan.profile_file,
            )?;
            config::client(args, &config::load(&plan.profile_file)?)
        }
        Err(e) => Err(e),
    }
}
fn register(
    client: &EnumerayClient,
    options: &Setup,
    directory: &Path,
    plan: &Plan,
    p: &mut Progress,
) -> Result<(), Error> {
    let id = &plan.input.connector.id;
    if !p.connector_registered {
        if !p.connector_started {
            match client.management().connector(id) {
                Ok(existing) => {
                    if !options.adopt_existing
                        && !(interactive(options)
                            && prompt::yes(
                                "Adopt this existing connector without changing its settings",
                                false,
                            )?)
                    {
                        return Err(invalid(
                            "Connector already exists; inspect it and explicitly use --adopt-existing.",
                        ));
                    }
                    if existing.connector_id != *id
                        || existing.label != plan.input.connector.label
                        || existing.max_outstanding_assignments
                            != plan.input.connector.max_outstanding_assignments
                        || existing.archived_at.is_some()
                    {
                        return Err(invalid(
                            "Existing connector differs from the reviewed plan.",
                        ));
                    }
                    p.connector_registered = true;
                }
                Err(Error::Api { status: 404, .. }) => {}
                Err(e) => return Err(e),
            }
            p.connector_started = true;
            save(directory, p)?;
        }
        if !p.connector_registered {
            let output = directory.join("enrollment-0000.json");
            if !output.exists()
                && let Ok(existing) = client.management().connector(id)
            {
                if Some(existing.owner_id) != p.user_id
                    || existing.label != plan.input.connector.label
                    || existing.max_outstanding_assignments
                        != plan.input.connector.max_outstanding_assignments
                    || existing.archived_at.is_some()
                {
                    return Err(invalid(
                        "Registration reconciliation conflicts with the original connector request.",
                    ));
                }
                p.connector_registered = true;
                save(directory, p)?;
            }
            if p.connector_registered { /* Registration may be known without recovering its one-time secret. */
            } else if output.exists() {
                config::require_private(&output)?;
                let document: ConnectorEnrollment = read(&output)?;
                if !crate::commands::valid_enrollment(&document, id) {
                    return Err(invalid(
                        "Retained enrollment document does not match this connector.",
                    ));
                }
                p.enrollment_paths.push(output);
                p.connector_registered = true;
                save(directory, p)?;
            } else {
                crate::commands::connector(
                    client,
                    &Connector::Create {
                        id: id.clone(),
                        label: plan.input.connector.label.clone(),
                        max_outstanding_assignments: plan
                            .input
                            .connector
                            .max_outstanding_assignments,
                        output: output.clone(),
                    },
                    true,
                )?;
                p.enrollment_paths.push(output);
                p.connector_registered = true;
                save(directory, p)?;
            }
        }
    }
    if let Some(source) = &options.enrollment_file {
        let document: ConnectorEnrollment = read(source)?;
        if !crate::commands::valid_enrollment(&document, id) {
            return Err(invalid(
                "Imported enrollment does not match this connector or its strict credential contract.",
            ));
        }
        let target = directory.join(format!("enrollment-import-{}.json", document.id));
        if !target.exists() {
            config::write_private(&target, &document, false)?;
        }
        if !p.enrollment_paths.contains(&target) {
            p.enrollment_paths.push(target);
        }
        p.pending_replacement = None;
        save(directory, p)?;
    }
    if options.replace_enrollment {
        if let Some(output) = &p.pending_replacement {
            if output.exists() {
                let document: ConnectorEnrollment = read(output)?;
                if !crate::commands::valid_enrollment(&document, id) {
                    return Err(invalid(
                        "Retained replacement enrollment is invalid; preserve it.",
                    ));
                }
                p.enrollment_paths.push(output.clone());
                p.pending_replacement = None;
                save(directory, p)?;
            } else {
                return Err(invalid(
                    "Previous replacement enrollment outcome is unknown. Inspect connector enrollment history; explicitly issue a new document with connector enrollment and import it using setup --enrollment-file NEW. No replacement was issued automatically.",
                ));
            }
        } else {
            if interactive(options)
                && !prompt::yes(
                    "Issue a replacement enrollment; this invalidates earlier unused documents",
                    false,
                )?
            {
                return Err(invalid("Replacement enrollment was not approved."));
            }
            let output = directory.join(format!("enrollment-{}.json", Uuid::new_v4()));
            p.pending_replacement = Some(output.clone());
            save(directory, p)?;
            crate::commands::connector(
                client,
                &Connector::Enrollment {
                    id: id.clone(),
                    output: output.clone(),
                },
                true,
            )?;
            p.enrollment_paths.push(output);
            p.pending_replacement = None;
            save(directory, p)?;
        }
    }
    if !p.context_registered {
        if !p.context_started {
            match client.management().context(id, &plan.input.host.context.id) {
                Ok(_) => {
                    if !options.adopt_existing
                        && !(interactive(options)
                            && prompt::yes(
                                "Adopt this existing context after checking its immutable definition",
                                false,
                            )?)
                    {
                        return Err(invalid(
                            "Context already exists; inspect it and explicitly use --adopt-existing.",
                        ));
                    }
                }
                Err(Error::Api { status: 404, .. }) => {}
                Err(e) => return Err(e),
            }
            p.context_started = true;
            save(directory, p)?;
        }
        let registered = client
            .management()
            .create_context(id, &plan.input.host.context)?;
        if registered.id != plan.input.host.context.id
            || registered.connector_id != *id
            || registered.definition != serde_json::to_value(&plan.input.host.context.definition)?
        {
            return Err(invalid("Context response differs from the saved request."));
        }
        p.context_registered = true;
        save(directory, p)?;
    }
    if p.grant.is_none() {
        let mut cursor = None::<String>;
        let mut existing = None;
        loop {
            let query = cursor
                .as_deref()
                .map(|c| vec![("cursor", c)])
                .unwrap_or_default();
            let page = client.management().compute_grants(id, &query)?;
            for grant in page.items {
                if grant.context_id == plan.input.host.context.id
                    && Some(grant.user_id) == p.user_id
                    && grant.revoked_at.is_none()
                {
                    existing = Some(grant);
                }
            }
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        let grant = if let Some(grant) = existing {
            if !options.adopt_existing
                && !(interactive(options)
                    && prompt::yes(
                        "Adopt the existing active compute grant with its current limits",
                        false,
                    )?)
            {
                return Err(invalid(
                    "An active compute grant exists; explicitly adopt it rather than changing authority.",
                ));
            }
            GrantCreate {
                id: Some(grant.id),
                context_id: grant.context_id,
                user_id: grant.user_id,
                max_queued_runs: grant.max_queued_runs,
                max_outstanding_assignments: grant.max_outstanding_assignments,
            }
        } else {
            GrantCreate {
                id: Some(Uuid::new_v4()),
                context_id: plan.input.host.context.id.clone(),
                user_id: p.user_id.unwrap(),
                max_queued_runs: 10,
                max_outstanding_assignments: plan
                    .input
                    .host
                    .context
                    .definition
                    .max_outstanding_assignments,
            }
        };
        p.grant = Some(grant);
        save(directory, p)?;
    }
    let request = p.grant.as_ref().unwrap();
    let grant = client.management().grant_compute(id, request)?;
    if Some(grant.id) != request.id
        || grant.context_id != request.context_id
        || grant.user_id != request.user_id
        || grant.revoked_at.is_some()
    {
        return Err(invalid(
            "Grant response conflicts with the saved creation identity.",
        ));
    }
    p.grant_registered = true;
    save(directory, p)?;
    Ok(())
}
fn summary(directory: &Path, plan: &Plan, p: &Progress, status: &str) -> Value {
    let host = &plan.input.host;
    let runtime = Path::new(&host.journal)
        .parent()
        .unwrap()
        .join("runtime.json");
    let mut value = json!({"directory":directory,"status":status,"plan_sha256":p.plan_sha256,"host_plan_sha256":enumeray_connector::configuration_digest(host).ok(),"connector_id":plan.input.connector.id,"context_id":host.context.id,"startup":plan.input.startup,"configuration":runtime,
        "host_plan":directory.join("host-plan.json"),"enrollment_file":p.enrollment_paths.last(),"execution_account":host.execution_account.name,"connector_account":host.connector_account.name,
        "next_action":match status {"review_required"=>"Review the saved setup and host plan; approve its plan SHA-256 to continue.","start_required"=>"For managed mode apply the host plan explicitly as administrator with its host digest and retained enrollment document. Run connector preflight as the connector account, then connector start --configuration FILE with --foreground or --background. Resume this setup in another terminal.","enrollment_required"=>"Inspect existing enrollment and credentials. If no usable document exists, explicitly request --replace-enrollment, then resume with the same durable identity directory.","development_consent_required"=>"Acknowledge trusted same-user execution with --allow-development-execution.","ready"=>"Host readiness is established. Scientific installation and qualification remain author/site responsibilities.",_=>"Inspect current diagnostics and follow each responsible role's correction; retain this setup directory."},"trial":p.trial });
    let foreground = plan.input.startup == "foreground";
    let mut start = vec![
        host.connector_executable.clone(),
        "connector".into(),
        "start".into(),
        "--configuration".into(),
        runtime.to_string_lossy().into_owned(),
        format!("--{}", plan.input.startup),
    ];
    if host.context.definition.execution_mode == ExecutionMode::Managed {
        let mut prefix = if foreground {
            vec![
                "sudo".into(),
                "-u".into(),
                host.connector_account.name.clone(),
                "--".into(),
            ]
        } else {
            vec!["sudo".into(), "--".into()]
        };
        prefix.extend(start);
        start = prefix;
    }
    let mut apply = vec![
        "sudo".to_owned(),
        "--".into(),
        std::env::current_exe()
            .unwrap_or_else(|_| PathBuf::from(&host.connector_executable))
            .to_string_lossy()
            .into_owned(),
        "setup".into(),
        "apply-host".into(),
        "--plan".into(),
        directory
            .join("host-plan.json")
            .to_string_lossy()
            .into_owned(),
        "--side".into(),
        "connector".into(),
        "--approve".into(),
        enumeray_connector::configuration_digest(host).unwrap_or_default(),
    ];
    if let Some(enrollment) = p.enrollment_paths.last() {
        apply.extend([
            "--enrollment-file".into(),
            enrollment.to_string_lossy().into_owned(),
        ]);
    }
    value["commands"] = json!({"start":start,"resume":["enumeray","--config",plan.profile_file.to_string_lossy(),"--profile",plan.profile,"setup","--directory",directory.to_string_lossy()]});
    if host.context.definition.execution_mode == ExecutionMode::Managed {
        value["commands"]["apply_host"] = json!(apply);
    }
    value
}
fn inspect(
    args: &Args,
    directory: &Path,
    plan: &Plan,
    p: &Progress,
) -> Result<Completion, Failure> {
    let mut v = summary(directory, plan, p, "saved");
    let observed = (|| -> Result<Value, Error> {
        let client = config::client(args, &config::load(&plan.profile_file)?)?;
        let user = client.me()?.user_id;
        if client.base_url().trim_end_matches('/')
            != plan.input.host.api_base_url.trim_end_matches('/')
            || p.user_id.is_some_and(|id| id != user)
        {
            return Err(invalid("Status requires the saved API/account."));
        }
        Ok(serde_json::to_value(
            client.management().connector_context_diagnostics(
                &plan.input.connector.id,
                &plan.input.host.context.id,
            )?,
        )?)
    })();
    match observed {
        Ok(d) => v["diagnostics"] = d,
        Err(e) => {
            v["diagnostics"] = json!({"state":"unknown","message":e.to_string()});
        }
    }
    complete(v, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrected_hosts_get_a_fresh_check_without_reissuing_pending_attempts() {
        let mut status = ContextPreflightStatus {
            connector_id: "connector".into(),
            context_id: "context".into(),
            definition_hash: "a".repeat(64),
            ownership_generation: 1,
            attempt: Some(PreflightAttempt {
                request_id: Uuid::new_v4(),
                requested_at: "2026-10-31T00:00:00Z".into(),
                expires_at: "2026-10-31T01:00:00Z".into(),
                state: AttemptState::Completed,
            }),
            report: None,
            validity: ReportValidity::Current,
            reason: "host prerequisites failed".into(),
            ready: false,
        };
        assert!(needs_new_preflight(&status));
        status.attempt.as_mut().unwrap().state = AttemptState::Pending;
        assert!(!needs_new_preflight(&status));
        status.attempt.as_mut().unwrap().state = AttemptState::Completed;
        status.ready = true;
        assert!(!needs_new_preflight(&status));
        status.validity = ReportValidity::Stale;
        assert!(needs_new_preflight(&status));
        status.attempt = None;
        assert!(!needs_new_preflight(&status));
    }
}
