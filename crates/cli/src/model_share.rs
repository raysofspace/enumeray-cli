//! Own the author checkpoint and foreground lifetime of one locally shared model.

use crate::{
    app::{Completion, Failure},
    args::{Args, Connector, ModelShare},
    commands, config, manifest,
    output::safe_text,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use enumeray_client::{EnumerayClient, Error, resource_path};
use enumeray_connector::{ShareServiceHandle, ShareServiceOptions, ShareSnapshot};
use enumeray_contracts::{
    execution::{
        ConnectionConfig, DriverConfig, ExecutionContextDefinition, ExecutionMode,
        InstallationDescriptor, ProcessCache, ProcessScratchPolicy, ScratchSymlinks,
    },
    management::{
        AccessMode, ConnectorEnrollmentFile, ContextMetadata, InvitationReceipt, ManagedModel,
        PublicationActivation, PublicationCreate, PublicationRegistration, ShareBootstrap,
        ShareInvitation, ShareInvitationReceipt, ShareRegistration, Visibility,
    },
    models::ParsedPublication,
    numeric::equivalent,
};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    io::{self, IsTerminal, Write},
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

const READINESS_LIMIT: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_secs(1);
const DISCLOSURE: &str = "This model runs on the author's computer under the author's regular OS account. The author can read files staged for the run. The submitter and model administrators can inspect retained inputs, outputs and logs through Enumeray.";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    schema_version: u32,
    project: PathBuf,
    origin: String,
    user_id: Uuid,
    uid: u32,
    share_id: Uuid,
    bootstrap: ShareBootstrap,
    registration: Option<ShareRegistration>,
    current: Option<Release>,
    pending: Option<PendingRelease>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    superseded: Vec<Release>,
    invitations: Vec<SavedInvitation>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Release {
    id: String,
    sha256: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingRelease {
    release: Release,
    expected_current: Option<String>,
    open: bool,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Recipient {
    Email(String),
    User(Uuid),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedInvitation {
    id: Uuid,
    recipient: Recipient,
    token: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    declined: bool,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}

fn sha256(bytes: impl AsRef<[u8]>) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn path_text(path: &Path) -> Result<&str, Error> {
    path.to_str()
        .ok_or_else(|| invalid("Sharing requires UTF-8 project and state paths."))
}

fn private_directory(path: &Path, uid: u32) -> Result<(), Error> {
    config::create_private_dir(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.permissions().mode() & 0o077 != 0 {
        return Err(invalid("Share state requires a private, owned directory."));
    }
    Ok(())
}

fn state_directory(project: &Path, uid: u32) -> Result<PathBuf, Error> {
    let base = env::var_os("XDG_STATE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .ok_or_else(|| invalid("Set XDG_STATE_HOME or HOME to a private state location."))?;
    if !base.is_absolute() {
        return Err(invalid("The share state location must be absolute."));
    }
    let root = base.join("enumeray/model-shares");
    let digest = sha256(path_text(project)?);
    // Check the existing prefix before creating directories. The entire tree
    // being shared must exclude credentials, work, and staged releases.
    let existing = root
        .ancestors()
        .find(|path| path.exists())
        .ok_or_else(|| invalid("Cannot locate the share state directory."))?;
    let projected = existing
        .canonicalize()?
        .join(root.strip_prefix(existing).expect("existing ancestor"));
    if projected.starts_with(project) {
        return Err(invalid(
            "Keep the share state directory outside the model project.",
        ));
    }
    private_directory(&root, uid)?;
    let directory = root.canonicalize()?.join(digest);
    private_directory(&directory, uid)?;
    Ok(directory)
}

fn os_account() -> Result<(u32, String), Error> {
    fn id(flag: &str) -> Result<String, Error> {
        let output = Command::new("/usr/bin/id").arg(flag).output()?;
        if !output.status.success() {
            return Err(invalid("Cannot identify the current OS account."));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }
    let uid = id("-u")?
        .parse::<u32>()
        .map_err(|_| invalid("Cannot identify the current OS account."))?;
    if uid == 0 {
        return Err(invalid("Run model share as your regular OS account."));
    }
    Ok((uid, id("-un")?))
}

fn ensure_launcher(path: &Path) -> Result<(), Error> {
    let executable = env::current_exe()?.canonicalize()?;
    match fs::read_link(path) {
        Ok(current) if current == executable => return Ok(()),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    // The context retains one launcher path across normal binary upgrades.
    let temp = path.with_file_name(format!("runner-{}.tmp", Uuid::new_v4()));
    symlink(executable, &temp)?;
    let result = fs::rename(&temp, path);
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result?;
    Ok(())
}

fn bootstrap(source: &ParsedPublication, root: &Path) -> Result<ShareBootstrap, Error> {
    let definition = ExecutionContextDefinition {
        execution_mode: ExecutionMode::Development,
        driver: DriverConfig::Process {
            scratch: Some(ProcessScratchPolicy {
                max_bytes: 1_073_741_824,
                max_entries: 10_000,
                max_depth: 32,
                scan_interval_ms: 1_000,
                cache: ProcessCache::PerRun,
                symlinks: ScratchSymlinks::Forbid,
            }),
        },
        connection: ConnectionConfig::Direct {
            launcher: vec![path_text(&root.join("runner"))?.into()],
        },
        installation_roots: vec![path_text(&root.join("releases"))?.into()],
        work_root: path_text(&root.join("work"))?.into(),
        max_outstanding_assignments: 1,
        shared_workspace: None,
    };
    definition.validate_execution().map_err(invalid)?;
    Ok(ShareBootstrap {
        model: source.model.clone(),
        definition,
        metadata: ContextMetadata {
            location: "Author's computer".into(),
            disclosure: DISCLOSURE.into(),
        },
    })
}

fn save(root: &Path, state: &State) -> Result<(), Error> {
    config::write_private(&root.join("share.json"), state, true)
}

fn selected_recipient(options: &ModelShare) -> Result<Option<Recipient>, Error> {
    if let Some(address) = &options.invite_email {
        let address = address.trim().to_lowercase();
        if address.len() > 320 || !address.contains('@') || address.chars().any(char::is_control) {
            return Err(invalid("Supply a valid invitation email address."));
        }
        return Ok(Some(Recipient::Email(address)));
    }
    if options.invite_user.is_some_and(|user| user.is_nil()) {
        return Err(invalid("Supply a valid recipient account UUID."));
    }
    Ok(options.invite_user.map(Recipient::User))
}

fn policy_label(access: AccessMode, visibility: Visibility) -> String {
    let mode = match access {
        AccessMode::Invite => "invite-only",
        AccessMode::Open => "open",
        AccessMode::Request => "request approval",
    };
    format!("{mode}, {}", visibility.as_str())
}

fn confirm(
    state: &State,
    observed: Option<&ManagedModel>,
    account: &str,
    sha256: &str,
    open: bool,
    recipient: Option<&Recipient>,
) -> Result<bool, Error> {
    let (access, visibility) = if state.current.is_none() {
        if open {
            (AccessMode::Open, Visibility::Listed)
        } else {
            (AccessMode::Invite, Visibility::Unlisted)
        }
    } else {
        let model = observed.ok_or_else(|| invalid("Current share is unavailable."))?;
        (model.access_mode, model.visibility)
    };
    let mut output = io::stderr().lock();
    let model_id = state
        .registration
        .as_ref()
        .map_or(state.bootstrap.model.id.as_str(), |value| {
            value.model_id.as_str()
        });
    writeln!(
        output,
        "Share {} on {}",
        safe_text(model_id, false),
        safe_text(&state.origin, false)
    )?;
    writeln!(output, "Local release SHA-256: {sha256}")?;
    writeln!(output, "OS account: {}", safe_text(account, false))?;
    writeln!(output, "{DISCLOSURE}")?;
    writeln!(
        output,
        "Only share code you trust. The wrapper can access files and credentials available to your OS account."
    )?;
    writeln!(output, "Access: {}", policy_label(access, visibility))?;
    if let Some(recipient) = recipient {
        let address = match recipient {
            Recipient::Email(address) => address.clone(),
            Recipient::User(user) => user.to_string(),
        };
        writeln!(
            output,
            "Invitation recipient: {}",
            safe_text(&address, false)
        )?;
    }
    write!(output, "Activate this local share? [y/N] ")?;
    output.flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn model(client: &EnumerayClient, state: &State) -> Result<Option<ManagedModel>, Error> {
    let Some(registration) = &state.registration else {
        return Ok(None);
    };
    let model = client.management().managed_model(&registration.model_id)?;
    if model.owner_id != state.user_id
        || !model.can_manage
        || model.model_id != registration.model_id
    {
        return Err(invalid(
            "The saved model share is no longer owned by this account.",
        ));
    }
    Ok(Some(model))
}

fn reconcile_current(
    root: &Path,
    state: &mut State,
    model: Option<&ManagedModel>,
) -> Result<(), Error> {
    let observed = model.and_then(|value| value.current_publication_id.as_deref());
    if state
        .pending
        .as_ref()
        .is_some_and(|pending| Some(pending.release.id.as_str()) == observed)
    {
        state.current = state.pending.take().map(|pending| pending.release);
        save(root, state)?;
    }
    if state.current.as_ref().map(|release| release.id.as_str()) != observed {
        let position = state
            .superseded
            .iter()
            .position(|release| Some(release.id.as_str()) == observed);
        let Some(position) = position else {
            return Err(invalid(
                "The current publication changed outside this local share. Inspect the model on the selected service.",
            ));
        };
        state.current = Some(state.superseded.remove(position));
        save(root, state)?;
    }
    Ok(())
}

fn retain_invitation(state: &mut State, recipient: Option<&Recipient>) -> Result<(), Error> {
    if let Some(recipient) = recipient {
        if let Some(saved) = state
            .invitations
            .iter_mut()
            .find(|saved| &saved.recipient == recipient)
        {
            saved.declined = false;
            return Ok(());
        }
        let mut bytes = [0_u8; 32];
        getrandom::fill(&mut bytes).map_err(|_| invalid("Cannot generate an invitation token."))?;
        state.invitations.push(SavedInvitation {
            id: Uuid::new_v4(),
            recipient: recipient.clone(),
            token: URL_SAFE_NO_PAD.encode(bytes),
            declined: false,
        });
    }
    Ok(())
}

fn register(
    client: &EnumerayClient,
    root: &Path,
    state: &mut State,
) -> Result<ShareRegistration, Error> {
    if state.registration.is_none() {
        let share_id = state.share_id.to_string();
        let reply: ShareRegistration = client.request(
            Method::PUT,
            &resource_path(&["model-shares", &share_id])?,
            Some(&state.bootstrap),
            None,
        )?;
        if reply.share_id != state.share_id
            || !enumeray_contracts::models::validation::valid_model_id(&reply.model_id)
            || reply.wrapper_model_id != state.bootstrap.model.id
            || reply.connector_id != format!("share-connector-{}", state.share_id.simple())
            || reply.context_id != format!("share-context-{}", state.share_id.simple())
            || reply.grant_id.is_nil()
        {
            return Err(invalid(
                "Service returned an inconsistent local share registration.",
            ));
        }
        state.registration = Some(reply);
        save(root, state)?;
    }
    Ok(state.registration.clone().expect("registered share"))
}

fn enroll(
    client: &EnumerayClient,
    root: &Path,
    registration: &ShareRegistration,
) -> Result<PathBuf, Error> {
    let credentials = root.join("credentials");
    let token = credentials.join("token");
    if token.try_exists()? {
        config::require_private(&token)?;
        return Ok(token);
    }
    let enrollment = root.join("enrollment.json");
    let document = enrollment_document(client, registration, &enrollment)?;
    if let Err(error) =
        enumeray_connector::enroll_share(client.base_url(), &credentials, &enrollment, false)
    {
        if !matches!(error, enumeray_connector::ShareEnrollmentError::Missing) {
            return Err(invalid(error.to_string()));
        }
        // Consumed documents accept an exact exchange even after expiry. A
        // definitive 404 permits another document for this same identity.
        let prior = root.join(format!("enrollment-{}.json", document.enrollment.id));
        fs::rename(&enrollment, prior)?;
        enrollment_document(client, registration, &enrollment)?;
        enumeray_connector::enroll_share(client.base_url(), &credentials, &enrollment, false)
            .map_err(|error| invalid(error.to_string()))?;
    }
    config::require_private(&token)?;
    Ok(token)
}

fn enrollment_document(
    client: &EnumerayClient,
    registration: &ShareRegistration,
    path: &Path,
) -> Result<ConnectorEnrollmentFile, Error> {
    if !path.try_exists()? {
        commands::connector(
            client,
            &Connector::Enrollment {
                id: registration.connector_id.clone(),
                output: path.to_path_buf(),
            },
            true,
        )?;
    }
    config::require_private(path)?;
    let document: ConnectorEnrollmentFile = config::read_json(path)?;
    if !commands::valid_enrollment(&document.enrollment, &registration.connector_id)
        || document.api_base_url.as_deref() != Some(client.base_url().trim_end_matches('/'))
    {
        return Err(invalid(
            "The retained enrollment does not match this share.",
        ));
    }
    Ok(document)
}

fn check_running(service: &ShareServiceHandle, until: Option<Instant>) -> Result<(), Failure> {
    if service.is_stopping() {
        return Err(Failure {
            error: invalid("Share interrupted. Rerun the same command to resume."),
            exit: 130,
        });
    }
    if service.is_finished() {
        return Err(invalid("The foreground connector stopped before the share was ready.").into());
    }
    if until.is_some_and(|deadline| Instant::now() >= deadline) {
        return Err(Failure {
            error: invalid(
                "The local share did not become ready in time. Rerun the same command to resume.",
            ),
            exit: 5,
        });
    }
    Ok(())
}

fn activate(
    client: &EnumerayClient,
    registration: &ShareRegistration,
    pending: &PendingRelease,
    mut source: ParsedPublication,
    snapshot: &ShareSnapshot,
    service: &ShareServiceHandle,
    until: Instant,
) -> Result<(), Failure> {
    source.execution.context_id = Some(registration.context_id.clone());
    source.execution.release_path = Some(path_text(&snapshot.path)?.into());
    source.execution.snapshot_sha256 = Some(snapshot.sha256.clone());
    let candidate = source
        .candidate(&pending.release.id)
        .map_err(|error| invalid(error.to_string()))?;
    let model = registration.model_id.as_str();
    let id = candidate.id.as_str();
    check_running(service, Some(until))?;
    let reply: PublicationRegistration = client.request(
        Method::POST,
        &resource_path(&["models", model, "publications"])?,
        Some(&candidate),
        None,
    )?;
    if reply.id != id
        || !matches!(
            reply.status.as_str(),
            "validating" | "ready" | "failed" | "published"
        )
    {
        return Err(
            invalid("Service returned an inconsistent share publication registration.").into(),
        );
    }
    loop {
        check_running(service, Some(until))?;
        let publication = client.management().publication(model, id)?;
        verify_candidate(registration, &candidate, &publication)?;
        match publication.status.as_str() {
            "ready" if service.is_online() => {
                if publication.validation_evidence.as_ref()
                    .and_then(|evidence| evidence.verified_digests.get(".")) != Some(&snapshot.sha256) {
                    return Err(invalid("Connector validation omitted the staged release digest.").into());
                }
                break;
            }
            "ready" | "validating" => thread::sleep(POLL_INTERVAL),
            "failed" => return Err(invalid("Connector validation failed for the staged release. Check the connector diagnostics, then share a corrected version.").into()),
            _ => return Err(invalid("The selected share publication is in an unexpected state.").into()),
        }
    }
    check_running(service, Some(until))?;
    let request = PublicationActivation {
        publication_delegation_id: None,
        expected_current_publication_id: pending.expected_current.clone(),
        share_open_access: pending.open.then_some(true),
    };
    let receipt = client
        .management()
        .activate_publication(model, id, &request)?;
    if receipt.id != id || receipt.status != "published" {
        return Err(invalid("Service returned an inconsistent share activation receipt.").into());
    }
    Ok(())
}

fn verify_candidate(
    registration: &ShareRegistration,
    candidate: &PublicationCreate,
    publication: &enumeray_contracts::management::PublicationDetail,
) -> Result<(), Error> {
    let descriptor: InstallationDescriptor =
        serde_json::from_value(publication.descriptor.clone())?;
    if publication.model_id != registration.model_id
        || publication.id != candidate.id
        || publication.context_id != registration.context_id
        || publication.grant_id != registration.grant_id
        || descriptor.wrapper_model_id.as_deref() != Some(&registration.wrapper_model_id)
        || !equivalent(
            &serde_json::to_value(descriptor.spec())?,
            &serde_json::to_value(&candidate.descriptor)?,
        )
        || !equivalent(
            &publication.definition,
            &serde_json::to_value(&candidate.definition)?,
        )
    {
        return Err(invalid(
            "Service publication differs from the staged local release.",
        ));
    }
    Ok(())
}

fn invitation_file(
    client: &EnumerayClient,
    root: &Path,
    share: &str,
    uid: u32,
    model: &str,
    saved: &SavedInvitation,
) -> Result<PathBuf, Error> {
    let id = saved.id.to_string();
    let directory = root.join("invitations");
    private_directory(&directory, uid)?;
    let path = directory.join(format!("{id}.json"));
    if path.try_exists()? {
        config::require_private(&path)?;
        let receipt: InvitationReceipt = config::read_json(&path)?;
        if receipt.id != saved.id || receipt.model_id != model || receipt.token != saved.token {
            return Err(invalid(
                "The private invitation file differs from the saved share invitation.",
            ));
        }
        return Ok(path);
    }
    let (recipient_id, recipient_email) = match &saved.recipient {
        Recipient::Email(address) => (None, Some(address.clone())),
        Recipient::User(user) => (Some(*user), None),
    };
    let receipt: ShareInvitationReceipt = client.request(
        Method::PUT,
        &resource_path(&["model-shares", share, "invitations", &id])?,
        Some(&ShareInvitation {
            recipient_id,
            recipient_email,
            token_sha256: sha256(&saved.token),
        }),
        None,
    )?;
    if receipt.id != saved.id || receipt.model_id != model {
        return Err(invalid(
            "Service returned an inconsistent share invitation receipt.",
        ));
    }
    config::write_private(
        &path,
        &InvitationReceipt {
            id: saved.id,
            model_id: model.into(),
            token: saved.token.clone(),
            expires_at: receipt.expires_at,
        },
        false,
    )?;
    Ok(path)
}

fn invitations(
    client: &EnumerayClient,
    root: &Path,
    state: &mut State,
    model: &str,
) -> Vec<PathBuf> {
    let share = state.share_id.to_string();
    let mut paths = Vec::new();
    let mut changed = false;
    for saved in state.invitations.iter_mut().filter(|saved| !saved.declined) {
        match invitation_file(client, root, &share, state.uid, model, saved) {
            Ok(path) => paths.push(path),
            Err(error) => {
                saved.declined = matches!(
                    error,
                    Error::Api {
                        status: 400 | 404 | 409 | 422,
                        ..
                    }
                );
                changed |= saved.declined;
                eprintln!(
                    "Invitation could not be saved. {}",
                    safe_text(&error.to_string(), false)
                );
                if saved.declined {
                    eprintln!(
                        "Check the recipient and use an invitation option the next time you start the share."
                    );
                } else {
                    eprintln!("The invitation will be retried when you restart this share.");
                }
            }
        }
    }
    if changed && let Err(error) = save(root, state) {
        eprintln!(
            "Invitation progress could not be saved. {}",
            safe_text(&error.to_string(), false)
        );
    }
    paths
}

fn serve(
    client: &EnumerayClient,
    root: &Path,
    state: &mut State,
    source: ParsedPublication,
    snapshot: &ShareSnapshot,
    service: &ShareServiceHandle,
) -> Result<(), Failure> {
    let until = Instant::now() + READINESS_LIMIT;
    let client = client.with_deadline(until);
    let registration = state.registration.as_ref().expect("registered share");
    if let Some(pending) = &state.pending
        && pending.release.sha256 == snapshot.sha256
    {
        activate(
            &client,
            registration,
            pending,
            source,
            snapshot,
            service,
            until,
        )?;
        state.current = state.pending.take().map(|pending| pending.release);
        save(root, state)?;
    }
    let model = loop {
        check_running(service, Some(until))?;
        let model = model(&client, state)?.expect("registered share");
        if model.current_publication_id.as_deref()
            != state.current.as_ref().map(|current| current.id.as_str())
        {
            return Err(invalid("The share's current publication changed during startup.").into());
        }
        let online = model
            .current_publication
            .as_ref()
            .and_then(|publication| publication.submission_availability.share_online);
        match online {
            Some(true) if service.is_online() => break model,
            Some(_) => thread::sleep(POLL_INTERVAL),
            None => {
                return Err(invalid(
                    "The selected service did not report local share availability.",
                )
                .into());
            }
        }
    };
    let invitations = invitations(&client, root, state, &model.model_id);
    check_running(service, None)?;
    let mut output = io::stdout().lock();
    writeln!(output, "Model ID: {}", safe_text(&model.model_id, false))?;
    writeln!(output, "Online: yes")?;
    writeln!(
        output,
        "Access: {}",
        policy_label(model.access_mode, model.visibility)
    )?;
    for path in invitations {
        writeln!(
            output,
            "Invitation file: {}",
            safe_text(path_text(&path)?, false)
        )?;
    }
    writeln!(output, "Press Ctrl+C to stop accepting new runs.")?;
    output.flush()?;
    Ok(())
}

pub(crate) fn execute(args: &Args, options: &ModelShare) -> Result<Completion, Failure> {
    if args.json || !io::stdin().is_terminal() {
        return Err(invalid("Run model share in an interactive terminal.").into());
    }
    let recipient = selected_recipient(options)?;
    let (uid, account) = os_account()?;
    let project = options.project.canonicalize()?;
    let profile = args
        .config
        .clone()
        .map(Ok)
        .unwrap_or_else(config::default_path)?;
    let client = config::client(args, &config::load(&profile)?)?;
    let user_id = client.me()?.user_id;
    let root = state_directory(&project, uid)?;
    let _lock = config::lock_private(
        &root.join("share"),
        "This model share is already running in another terminal.",
    )?;
    let releases = root.join("releases");
    private_directory(&releases, uid)?;
    let snapshot =
        enumeray_connector::stage_share_snapshot(&project, &releases).map_err(invalid)?;
    let (_, source) = manifest::publication_source(&snapshot.path.join("publication.json"))?;
    if source.execution.slurm_script.is_some() || source.execution.snapshot_sha256.is_some() {
        return Err(invalid("Model share requires a process declaration without a Slurm script or a custom snapshot digest.").into());
    }
    let entrypoint = snapshot.path.join(&source.execution.entrypoint[0]);
    let metadata = fs::symlink_metadata(entrypoint)
        .map_err(|_| invalid("The project entrypoint is missing from the staged release."))?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(
            invalid("The staged project entrypoint must be a regular executable file.").into(),
        );
    }
    let mut state = if root.join("share.json").try_exists()? {
        config::require_private(&root.join("share.json"))?;
        let state: State = config::read_json(&root.join("share.json"))?;
        if state.schema_version != 1
            || state.project != project
            || state.origin != client.base_url()
            || state.user_id != user_id
            || state.uid != uid
            || state.bootstrap.model.id != source.model.id
        {
            return Err(invalid("This project already has a share bound to another service, account, or declared model ID. Restore its original settings to resume.").into());
        }
        state
    } else {
        State {
            schema_version: 1,
            project,
            origin: client.base_url().into(),
            user_id,
            uid,
            share_id: Uuid::new_v4(),
            bootstrap: bootstrap(&source, &root)?,
            registration: None,
            current: None,
            pending: None,
            superseded: Vec::new(),
            invitations: Vec::new(),
        }
    };
    let observed = model(&client, &state)?;
    reconcile_current(&root, &mut state, observed.as_ref())?;
    if state.current.is_some()
        && options.open
        && observed
            .as_ref()
            .is_none_or(|model| model.access_mode != AccessMode::Open)
    {
        return Err(invalid("The saved share is invite-only. Use the model access policy to open an already activated share.").into());
    }
    let restart = state
        .current
        .as_ref()
        .is_some_and(|release| release.sha256 == snapshot.sha256);
    let approved = state.pending.as_ref().is_some_and(|pending| {
        pending.release.sha256 == snapshot.sha256
            && pending.expected_current.as_ref()
                == state.current.as_ref().map(|release| &release.id)
            && (!options.open || pending.open || state.current.is_some())
    });
    if !restart && !approved {
        let initial_open = state.current.is_none() && options.open;
        if !confirm(
            &state,
            observed.as_ref(),
            &account,
            &snapshot.sha256,
            initial_open,
            recipient.as_ref(),
        )? {
            return Ok(Completion {
                value: None,
                exit: 0,
            });
        }
        if let Some(previous) = state.pending.take() {
            state.superseded.push(previous.release);
        }
        state.pending = Some(PendingRelease {
            release: Release {
                id: format!("pub-{}", Uuid::new_v4()),
                sha256: snapshot.sha256.clone(),
            },
            expected_current: state.current.as_ref().map(|release| release.id.clone()),
            open: initial_open,
        });
    }
    retain_invitation(&mut state, recipient.as_ref())?;
    save(&root, &state)?;
    ensure_launcher(&root.join("runner"))?;
    let registration = register(&client, &root, &mut state)?;
    let token = enroll(&client, &root, &registration)?;
    for name in ["journal", "work"] {
        private_directory(&root.join(name), uid)?;
    }
    let service = enumeray_connector::start_share_service(ShareServiceOptions {
        journal: root.join("journal"),
        api_base_url: client.base_url().into(),
        connector_id: registration.connector_id,
        token_file: token,
        allow_insecure_http: false,
    })
    .map_err(invalid)?;
    let result = serve(&client, &root, &mut state, source, &snapshot, &service);
    if result.is_err() {
        service.request_stop();
    }
    let shutdown = service.wait();
    result?;
    shutdown.map_err(invalid)?;
    Ok(Completion {
        value: None,
        exit: 0,
    })
}
