//! Durable connector enrollment and execution context readiness.
mod prompt;
use crate::{
    app::{Completion, Failure},
    args::{Args, Auth, Connector, Setup, SetupCommand, SetupMode},
    config,
};
use enumeray_client::{EnumerayClient, Error};
use enumeray_contracts::{
    execution::{ConnectionConfig, DriverConfig, ExecutionMode},
    management::{ConnectorCreate, ConnectorDiagnostics, ConnectorEnrollmentFile},
    setup::*,
};
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

const INPUT_VERSION: u32 = 1;
const CHECKPOINT_VERSION: u32 = 2;

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
    #[serde(default)]
    enrollment_paths: Vec<PathBuf>,
    #[serde(default)]
    pending_replacement: Option<PathBuf>,
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
fn read_input(path: &Path) -> Result<Input, Error> {
    match config::read_json(path) {
        Err(Error::Json(_))
            if config::read_json::<Value>(path)
                .is_ok_and(|value| value.get("first_model").is_some()) =>
        {
            Err(invalid(
                "Remove first_model from the setup input. Once the context is ready, use model release and analysis commands to prepare and trial a model.",
            ))
        }
        result => result,
    }
}

fn check_checkpoint_version(path: &Path) -> Result<(), Error> {
    #[derive(Deserialize)]
    struct Version {
        manifest_version: u32,
    }
    match read::<Version>(path)?.manifest_version {
        CHECKPOINT_VERSION => Ok(()),
        1 => Err(invalid(
            "This version-1 setup directory belongs to an older CLI. Keep it and use a new directory with enumeray connector setup. Use model release commands with any saved first-release directory and analysis commands with any saved first-trial directory.",
        )),
        _ => Err(invalid(
            "Unsupported setup checkpoint version. Use a matching CLI to inspect this directory.",
        )),
    }
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
    let plan_path = directory.join("setup.json");
    match fs::symlink_metadata(&plan_path) {
        Ok(_) => check_checkpoint_version(&plan_path)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    // Inspection reads atomic state without waiting for the writer's network operations.
    let _writer = if inspecting {
        None
    } else {
        Some(config::lock_private(
            &directory.join("setup"),
            "This setup is already being resumed; inspect status or wait for its writer.",
        )?)
    };
    let (plan, mut progress) = if plan_path.exists() {
        let plan: Plan = read(&plan_path)?;
        let p: Progress = read(&directory.join("state.json"))?;
        if plan.manifest_version != CHECKPOINT_VERSION
            || p.manifest_version != CHECKPOINT_VERSION
            || plan.operation_id.is_nil()
            || p.plan_sha256 != hash(&plan)?
        {
            return Err(invalid(
                "Setup evidence changed; preserve this directory and inspect the original plan.",
            )
            .into());
        }
        if plan.directory != directory {
            return Err(invalid("This setup checkpoint belongs to its original directory; restore it at that path to retain its registration and enrollment progress.").into());
        }
        if plan.profile != args.profile || plan.profile_file != profile_path(args)? {
            return Err(
                invalid("Use this setup's original profile and private profile file.").into(),
            );
        }
        if let Some(file) = &options.file {
            let supplied = read_input(file)?;
            if hash(&supplied)? != hash(&plan.input)? {
                return Err(invalid(
                    "Input differs from the frozen setup plan; use a new setup directory.",
                )
                .into());
            }
        }
        (plan, p)
    } else {
        if !new && fs::read_dir(&directory)?.any(|e| e.is_ok_and(|e| e.file_name() != "setup.lock"))
        {
            return Err(invalid("Existing directory has no complete setup checkpoint; preserve it and select a new directory.").into());
        }
        let input = if let Some(file) = &options.file {
            read_input(file)?
        } else if interactive(options) {
            prompt::input(args, options)?
        } else {
            return Err(invalid("Supply --file with a complete setup input, or run setup in a terminal for guidance.").into());
        };
        validate(&input)?;
        let host =
            enumeray_connector::host_configuration_plan(input.host.clone()).map_err(invalid)?;
        let plan = Plan {
            manifest_version: CHECKPOINT_VERSION,
            operation_id: Uuid::new_v4(),
            directory: directory.clone(),
            profile: args.profile.clone(),
            profile_file: profile_path(args)?,
            input,
        };
        let progress = Progress {
            manifest_version: CHECKPOINT_VERSION,
            plan_sha256: hash(&plan)?,
            user_id: None,
            approved: false,
            development_consent: false,
            connector_started: false,
            connector_registered: false,
            context_started: false,
            context_registered: false,
            enrollment_paths: vec![],
            pending_replacement: None,
        };
        // Publish progress first; a missing plan can never conceal an API mutation.
        config::write_private(&directory.join("state.json"), &progress, false)?;
        config::write_private(&plan_path, &plan, false)?;
        config::write_private(&directory.join("host-plan.json"), &host, false)?;
        (plan, progress)
    };
    validate(&plan.input)?;
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
    if options
        .approve
        .as_ref()
        .is_some_and(|digest| digest != &progress.plan_sha256)
    {
        return Err(invalid("Approval does not match the owner setup plan SHA-256.").into());
    }
    if !progress.approved {
        let overview = summary(&directory, &plan, &progress, "review_required");
        render_review(&plan, &progress, &host, &plan_path, &host_path)?;
        let approve = options.approve.as_deref() == Some(progress.plan_sha256.as_str())
            || (interactive(options)
                && prompt::yes(
                    "Approve the saved connector setup plan?",
                    "Use these settings to start or continue connector registration.",
                    "Keep the plan for review and pause setup.",
                    false,
                )?);
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
                && prompt::yes(
                    "Allow the connector to run trusted model code under your OS account?",
                    "Allow execution with the same file and credential access as your account.",
                    "Pause setup before preparing or running this development connector.",
                    false,
                )?))
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
            let Some(enrollment) = enrollment_path(&progress) else {
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
    let Some(readiness) = &diag.setup else {
        return complete(
            with_diagnostics(summary(&directory, &plan, &progress, "host_blocked"), diag)?,
            1,
        );
    };
    if !readiness.connected {
        let needs_enrollment = readiness.issues.iter().any(|issue| {
            matches!(
                issue.code.as_str(),
                "enrollment_incomplete" | "connector_credential_revoked"
            )
        });
        let status = if needs_enrollment && enrollment_path(&progress).is_none() {
            "enrollment_required"
        } else {
            "start_required"
        };
        return complete(
            with_diagnostics(summary(&directory, &plan, &progress, status), diag)?,
            1,
        );
    }
    if !readiness.ready {
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
            return Ok(preflight_completion(
                with_diagnostics(summary(&directory, &plan, &progress, "host_blocked"), diag)?,
                result,
            ));
        }
        let diag = client
            .management()
            .connector_context_diagnostics(&plan.input.connector.id, &plan.input.host.context.id)?;
        return readiness_completion(&directory, &plan, &progress, diag);
    }
    readiness_completion(&directory, &plan, &progress, diag)
}

fn readiness_completion(
    directory: &Path,
    plan: &Plan,
    progress: &Progress,
    diagnostics: ConnectorDiagnostics,
) -> Result<Completion, Failure> {
    let (status, exit) = if diagnostics.setup.as_ref().is_some_and(|s| s.ready) {
        ("ready", 0)
    } else {
        ("host_blocked", 1)
    };
    complete(
        with_diagnostics(summary(directory, plan, progress, status), diagnostics)?,
        exit,
    )
}

fn with_diagnostics(mut result: Value, diagnostics: ConnectorDiagnostics) -> Result<Value, Error> {
    if diagnostics.setup.is_none() {
        result["readiness_unavailable"] = json!(
            "This service returned no context readiness information. Check that the CLI and service are compatible."
        );
    }
    result["diagnostics"] = serde_json::to_value(diagnostics)?;
    Ok(result)
}

fn enrollment_path(progress: &Progress) -> Option<&Path> {
    progress
        .enrollment_paths
        .last()
        .filter(|path| path.is_file())
        .map(PathBuf::as_path)
}
fn preflight_completion(mut overview: Value, result: Completion) -> Completion {
    overview["preflight"] = result.value.unwrap_or(Value::Null);
    Completion {
        value: Some(overview),
        exit: result.exit,
    }
}
fn needs_new_preflight(status: &ContextPreflightStatus) -> bool {
    status
        .attempt
        .as_ref()
        .is_some_and(|a| a.state != AttemptState::Pending)
        && (!status.ready || status.validity != ReportValidity::Current)
}
fn validate(input: &Input) -> Result<(), Error> {
    if input.manifest_version != INPUT_VERSION {
        return Err(invalid(
            "The scripted setup input requires manifest_version 1.",
        ));
    }
    if input.connector.id != input.host.context.connector_id {
        return Err(invalid(
            "Use the same connector ID in connector.id and host.context.connector_id.",
        ));
    }
    if !matches!(input.startup.as_str(), "foreground" | "background") {
        return Err(invalid(
            "Set startup to foreground for a terminal or background for a service.",
        ));
    }
    if input.host.install_service != (input.startup == "background") {
        return Err(invalid(
            "Set host.install_service to true for background startup and false for foreground startup.",
        ));
    }
    if !(1..=1024).contains(&input.connector.max_outstanding_assignments) {
        return Err(invalid(
            "Set connector.max_outstanding_assignments to a whole number from 1 to 1024.",
        ));
    }
    let label = &input.connector.label;
    if label.is_empty()
        || label.len() > 200
        || label.trim() != label
        || label.chars().any(char::is_control)
    {
        return Err(invalid(
            "Set connector.label to 1–200 bytes of display text without leading or trailing whitespace or control characters.",
        ));
    }
    enumeray_connector::configuration_digest(&input.host).map_err(invalid)?;
    EnumerayClient::new(&input.host.api_base_url, None, Duration::from_secs(60))?;
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
        let selected = EnumerayClient::new(
            config::api_origin(args, None),
            None,
            Duration::from_secs(args.timeout),
        )?;
        if selected.base_url().trim_end_matches('/')
            != plan.input.host.api_base_url.trim_end_matches('/')
        {
            crate::commands::auth(
                args,
                &Auth::Configure {
                    api_url: plan.input.host.api_base_url.clone(),
                },
                &plan.profile_file,
            )?;
        }
    }
    let client = config::client(args, &config::load(&plan.profile_file)?)?;
    match client.me() {
        Ok(_) => Ok(client),
        Err(e) if interactive(options) => {
            eprintln!("Authentication needs attention: {e}");
            if !prompt::yes(
                &format!("Open browser sign-in for profile {:?}?", plan.profile),
                "Sign in through your browser and continue with this profile.",
                "Pause setup. Sign in separately and resume with this profile.",
                false,
            )? {
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
fn valid_setup_enrollment(
    document: &ConnectorEnrollmentFile,
    connector: &str,
    client: &EnumerayClient,
) -> bool {
    crate::commands::valid_enrollment(&document.enrollment, connector)
        && document.api_base_url.as_deref().is_none_or(|origin| {
            origin.trim_end_matches('/') == client.base_url().trim_end_matches('/')
        })
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
                    if existing.connector_id != *id || existing.archived_at.is_some() {
                        return Err(invalid(
                            "Choose an active connector that matches the reviewed connector ID.",
                        ));
                    }
                    if existing.label != plan.input.connector.label {
                        return Err(invalid(
                            "The existing connector's display label differs from the reviewed plan.",
                        ));
                    }
                    if existing.max_outstanding_assignments
                        != plan.input.connector.max_outstanding_assignments
                    {
                        return Err(invalid(
                            "The existing connector's dispatch limit differs from the reviewed plan.",
                        ));
                    }
                    if !options.adopt_existing
                        && !(interactive(options)
                            && prompt::yes(
                                "Reuse the existing connector with these settings?",
                                "Use this connector and keep its existing registration.",
                                "Pause setup so you can inspect the connector or choose another ID.",
                                false,
                            )?)
                    {
                        return Err(invalid(
                            "Connector already exists; inspect it and explicitly use --adopt-existing.",
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
                let document: ConnectorEnrollmentFile = read(&output)?;
                if !valid_setup_enrollment(&document, id, client) {
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
        let document: ConnectorEnrollmentFile = read(source)?;
        if !valid_setup_enrollment(&document, id, client) {
            return Err(invalid(
                "Imported enrollment does not match this connector or its strict credential contract.",
            ));
        }
        let target = directory.join(format!("enrollment-import-{}.json", document.enrollment.id));
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
                let document: ConnectorEnrollmentFile = read(output)?;
                if !valid_setup_enrollment(&document, id, client) {
                    return Err(invalid(
                        "Retained replacement enrollment is invalid; preserve it.",
                    ));
                }
                p.enrollment_paths.push(output.clone());
                p.pending_replacement = None;
                save(directory, p)?;
            } else {
                return Err(invalid(
                    "Previous replacement enrollment outcome is unknown. Inspect connector enrollment history; explicitly issue a new document with connector enrollment and import it using connector setup --enrollment-file NEW. No replacement was issued automatically.",
                ));
            }
        } else {
            if interactive(options)
                && !prompt::yes(
                    "Issue a replacement one-time enrollment document?",
                    "Save a new document and invalidate any earlier unused ones.",
                    "Pause setup and keep the existing enrollment documents.",
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
                                "Reuse the existing context if the service confirms the settings match?",
                                "Check the saved definition and disclosure against this plan and keep the existing context if they match.",
                                "Pause setup so you can inspect the context or choose another ID.",
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
            || registered.metadata.location != plan.input.host.context.metadata.location
            || registered.metadata.disclosure != plan.input.host.context.metadata.disclosure
        {
            return Err(invalid("Context response differs from the saved request."));
        }
        p.context_registered = true;
        save(directory, p)?;
    }
    Ok(())
}

fn render_review(
    plan: &Plan,
    progress: &Progress,
    host_plan: &HostConfigurationPlan,
    plan_path: &Path,
    host_path: &Path,
) -> Result<(), Error> {
    let host = &host_plan.spec;
    let context = &host.context;
    let definition = &context.definition;
    eprintln!(
        "Review connector setup\n  API: {} (profile {:?})\n  Connector: {} ({})\n  Context: {} at {}\n  Data access disclosure: {}\n  Execution mode: {}",
        host.api_base_url,
        plan.profile,
        context.connector_id,
        plan.input.connector.label,
        context.id,
        context.metadata.location,
        context.metadata.disclosure,
        mode_name(definition),
    );
    for (label, account) in [
        ("Connector OS account", &host.connector_account),
        ("Execution OS account", &host.execution_account),
    ] {
        let handling = if account.create {
            "create if absent"
        } else {
            "use existing"
        };
        eprintln!(
            "  {label}: {} (group {}, home {}, {handling})",
            account.name, account.group, account.home,
        );
        if let (Some(uid), Some(gid)) = (account.uid, account.gid) {
            eprintln!("    User ID: {uid}, group ID: {gid}");
        }
    }
    match &definition.driver {
        DriverConfig::Process { scratch } => {
            eprintln!("  Job driver: Process");
            if let Some(scratch) = scratch {
                eprintln!(
                    "  Process scratch limits: {} bytes, {} entries, {} directory levels",
                    scratch.max_bytes, scratch.max_entries, scratch.max_depth,
                );
            }
        }
        DriverConfig::Slurm(slurm) => {
            eprintln!(
                "  Job driver: Slurm (compute node executable {})",
                slurm.worker_path
            );
        }
    }
    match &definition.connection {
        ConnectionConfig::Direct { launcher } => {
            eprintln!("  Connection: Direct on the connector host");
            eprintln!(
                "  Runner launcher: {}",
                launcher
                    .iter()
                    .map(|arg| crate::output::shell_word(arg))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
        ConnectionConfig::Ssh(ssh) => {
            eprintln!(
                "  Connection: SSH to {} at {} on port {}",
                ssh.user, ssh.host, ssh.port
            );
            eprintln!("  Remote runner: {}", ssh.runner_path);
            eprintln!("  SSH credential reference: {}", ssh.credential_reference);
        }
    }
    for root in &definition.installation_roots {
        eprintln!("  Approved release directory: {root}");
    }
    eprintln!(
        "  Execution work directory: {}\n  Connector executable: {}\n  Connector journal: {}\n  Runtime credential path: {}",
        definition.work_root, host.connector_executable, host.journal, host.token_file,
    );
    if let Some(path) = &host.credentials_file {
        eprintln!("  Private SSH credential map: {path}");
    }
    if let (Some(source), Some(digest)) = (&host.executable_source, &host.executable_sha256) {
        eprintln!("  Approved connector executable: {source} (SHA-256 {digest})");
    }
    if let Some(digest) = &host.execution_executable_sha256 {
        eprintln!("  Approved SSH runner SHA-256: {digest}");
    }
    eprintln!(
        "  Local workers: {}\n  Connector dispatch limit: {}\n  Context admission limit: {}\n  Transfer workers: {}\n  Pending assignment limit: {}\n  Shutdown timeout: {} seconds\n  Startup: {}",
        host.max_workers,
        plan.input.connector.max_outstanding_assignments,
        definition.max_outstanding_assignments,
        host.max_transfer_workers,
        host.max_pending_assignments,
        host.shutdown_timeout_seconds,
        plan.input.startup,
    );
    let development = definition.execution_mode == ExecutionMode::Development;
    if development {
        eprintln!(
            "  Your current OS account prepares the local directories, enrolls and starts the connector."
        );
    } else {
        if matches!(definition.connection, ConnectionConfig::Ssh(_)) {
            eprintln!(
                "  The execution host administrator prepares the remote runner, account and directories first."
            );
        }
        eprintln!(
            "  The connector host administrator applies the host plan and starts the connector."
        );
    }
    if plan.input.startup == "foreground" {
        eprintln!("  Keep the connector terminal open and resume setup in a separate terminal.");
    } else {
        eprintln!("  Start the connector explicitly using the selected OS service manager.");
    }
    eprintln!("Planned host actions");
    for action in &host_plan.actions {
        if matches!(action.kind, HostActionKind::Service) && plan.input.startup == "foreground" {
            continue;
        }
        let side = match action.host {
            HostSide::Connector => "connector host",
            HostSide::Execution
                if matches!(definition.connection, ConnectionConfig::Direct { .. }) =>
            {
                "connector host, execution account"
            }
            HostSide::Execution => "SSH execution host",
        };
        let role = if development {
            "your current OS account"
        } else {
            match action.responsible_role {
                ResponsibleRole::ConnectorOwner => "connector owner",
                ResponsibleRole::HostAdministrator => "host administrator",
                ResponsibleRole::SiteOperator => "site operator",
            }
        };
        eprintln!("  {} ({role} on the {side})", action.target);
        for instruction in &action.instructions {
            eprintln!("    {instruction}");
        }
    }
    eprintln!(
        "Saved setup plan: {}\nSaved host plan: {}\nOwner setup plan SHA-256: {}",
        plan_path.display(),
        host_path.display(),
        progress.plan_sha256,
    );
    if !development {
        eprintln!(
            "Administrator host plan SHA-256: {}",
            enumeray_connector::configuration_digest(host).map_err(invalid)?,
        );
    }
    Ok(())
}

fn summary(directory: &Path, plan: &Plan, p: &Progress, status: &str) -> Value {
    let host = &plan.input.host;
    let managed = host.context.definition.execution_mode == ExecutionMode::Managed;
    let next_action = match status {
        "review_required" => {
            "Review the settings and host actions. Use the owner setup plan SHA-256 to approve a noninteractive resume."
        }
        "start_required" if !managed => {
            "Local preflight passed. If the connector is stopped, run the start command as your current OS user. Resume as the connector owner. For foreground startup, resume in a separate terminal."
        }
        "start_required"
            if matches!(
                host.context.definition.connection,
                enumeray_contracts::execution::ConnectionConfig::Ssh(_)
            ) =>
        {
            "For initial preparation, give the SSH execution host administrator the copied host plan and its SHA-256. Use the execution host command there with the local paths to the copied plan and reviewed native CLI. Apply the plan on the connector host next and check local preflight. Keep enrollment on the connector host. If the connector is stopped, start it, then resume as the connector owner."
        }
        "start_required" => {
            "For initial preparation, have the connector host administrator apply the plan and review local preflight. If the connector is stopped, start it. Resume as the connector owner. For foreground startup, resume in a separate terminal."
        }
        "enrollment_required" => {
            "Inspect existing enrollment and credentials. If no usable document exists, explicitly request --replace-enrollment, then resume with the same durable identity directory."
        }
        "development_consent_required" => {
            "Acknowledge trusted same-user execution with --allow-development-execution."
        }
        "ready" => {
            "Share the ready context ID with the intended author. Use connector grant create when you are ready to give them access."
        }
        _ => {
            "Inspect current diagnostics and follow each responsible role's correction. Retain this setup directory."
        }
    };
    let runtime = Path::new(&host.journal)
        .parent()
        .unwrap()
        .join("runtime.json");
    let host_plan_path = directory.join("host-plan.json");
    let host_digest = enumeray_connector::configuration_digest(host).unwrap_or_default();
    let mut value = json!({
        "directory": directory,
        "status": status,
        "plan_sha256": p.plan_sha256,
        "host_plan_sha256": host_digest,
        "connector_id": plan.input.connector.id,
        "context_id": host.context.id,
        "startup": plan.input.startup,
        "configuration": runtime,
        "host_plan": host_plan_path,
        "enrollment_file": enrollment_path(p),
        "execution_account": host.execution_account.name,
        "connector_account": host.connector_account.name,
        "mode": mode_name(&host.context.definition),
        "profile": plan.profile,
        "api_url": host.api_base_url,
        "next_action": next_action
    });
    let foreground = plan.input.startup == "foreground";
    let mut start = vec![
        host.connector_executable.clone(),
        "connector".into(),
        "start".into(),
        "--configuration".into(),
        runtime.to_string_lossy().into_owned(),
        format!("--{}", plan.input.startup),
    ];
    if managed {
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
    value["commands"] = json!({
        "start": start,
        "resume": ["enumeray", "--config", plan.profile_file.to_string_lossy(), "--profile",
            plan.profile, "connector", "setup", "--directory", directory.to_string_lossy(),
            "--api", host.api_base_url]
    });
    if managed {
        let apply_command = |executable: &Path, host_plan: &Path, side: &str| {
            vec![
                "sudo".to_owned(),
                "--".into(),
                executable.to_string_lossy().into_owned(),
                "connector".into(),
                "setup".into(),
                "apply-host".into(),
                "--plan".into(),
                host_plan.to_string_lossy().into_owned(),
                "--side".into(),
                side.into(),
                "--approve".into(),
                host_digest.clone(),
            ]
        };
        let local_cli =
            std::env::current_exe().unwrap_or_else(|_| PathBuf::from(&host.connector_executable));
        let mut apply = apply_command(&local_cli, &host_plan_path, "connector");
        if let Some(enrollment) = enrollment_path(p) {
            apply.extend([
                "--enrollment-file".into(),
                enrollment.to_string_lossy().into_owned(),
            ]);
        }
        value["commands"]["apply_host"] = json!(apply);
        if matches!(host.context.definition.connection, ConnectionConfig::Ssh(_)) {
            let mut remote = apply_command(
                Path::new("/absolute/path/to/reviewed-enumeray"),
                Path::new("/absolute/path/to/copied-host-plan.json"),
                "execution",
            );
            remote.extend([
                "--executable-source".into(),
                "/absolute/path/to/reviewed-enumeray".into(),
            ]);
            value["commands"]["apply_execution_host"] = json!(remote);
        }
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
    let observed = (|| -> Result<ConnectorDiagnostics, Error> {
        let client = config::client(args, &config::load(&plan.profile_file)?)?;
        let user = client.me()?.user_id;
        if client.base_url().trim_end_matches('/')
            != plan.input.host.api_base_url.trim_end_matches('/')
            || p.user_id.is_some_and(|id| id != user)
        {
            return Err(invalid("Status requires the saved API/account."));
        }
        client
            .management()
            .connector_context_diagnostics(&plan.input.connector.id, &plan.input.host.context.id)
    })();
    match observed {
        Ok(d) => v = with_diagnostics(v, d)?,
        Err(e) => {
            v["diagnostics"] = json!({"state":"unknown","message":e.to_string()});
            v["readiness_unavailable"] = json!(e.to_string());
        }
    }
    complete(v, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocked_preflight_preserves_parent_resume_and_displays_the_responsible_role() {
        use clap::Parser;
        let args = Args::parse_from(["enumeray", "connector", "setup", "--directory", "/setup"]);
        for exit in [1, 5, 130] {
            let completion = preflight_completion(
                json!({"directory":"/setup","status":"host_blocked","commands":{"resume":["enumeray","connector","setup","--directory","/setup"]}}),
                Completion {
                    value: Some(
                        json!({"directory":"/setup/preflight-0000","report":{"checks":[{"check":"work_root","required":true,"outcome":"failed","responsible_role":"host_administrator","correction":"Prepare the private execution work directory."}]}}),
                    ),
                    exit,
                },
            );
            assert_eq!(completion.exit, exit);
            let value = completion.value.unwrap();
            assert_eq!(value["directory"], "/setup");
            assert_eq!(value["commands"]["resume"][4], "/setup");
            let rendered = crate::output::render(&args, &value);
            assert!(rendered.contains("host_administrator"));
            assert!(rendered.contains("Prepare the private execution work directory."));
        }
    }

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
