//! One saved release, with explicit review and activation across uncertain replies.
mod handoff;
mod state;

use crate::{
    app::{Completion, Failure},
    args::{Args, Release as Command},
    manifest::{publication_source, validate_publication},
};
use enumeray_client::{EnumerayClient, Error, observation::ObservationClient, resource_path};
use enumeray_contracts::{
    automation::{ReleaseComparison, ReleaseSummary, SchemaVersion},
    execution::InstallationDescriptor,
    management::{
        AvailableGrant, Collection, ManagedModel, PublicationActivation,
        PublicationActivationReceipt, PublicationCreate, PublicationDetail,
        PublicationRegistration,
    },
    numeric::equivalent,
};
use serde_json::{Value, json};
use state::{Attempt, Directory, Progress, Release, Review, digest, envelope, invalid};
use std::{collections::BTreeSet, path::Path, time::Duration};
use tokio::time::{Instant, sleep, sleep_until};
use uuid::Uuid;

struct Snapshot {
    model: Value,
    publication: Option<Value>,
    grant: Option<Value>,
    grant_checked: bool,
}

pub(crate) fn execute(
    args: &Args,
    client: &EnumerayClient,
    command: &Command,
) -> Result<Completion, Failure> {
    if let Command::Init {
        model,
        directory,
        file,
        rollback_from,
        candidate,
    } = command
    {
        return init(
            client,
            model,
            directory,
            file.as_deref(),
            rollback_from.as_deref(),
            candidate.as_deref(),
        )
        .map(|mut value| {
            next_invocation(args, &mut value);
            Completion {
                value: Some(value),
                exit: 0,
            }
        })
        .map_err(Into::into);
    }
    let path = match command {
        Command::Resume { directory, .. }
        | Command::Status { directory }
        | Command::Review { directory }
        | Command::Activate { directory }
        | Command::Delegate { directory, .. }
        | Command::RevokeDelegation { directory, .. } => directory,
        Command::Init { .. } => unreachable!(),
    };
    let directory = Directory::open(path)?;
    let (release, mut progress) = directory.load(client)?;
    if let Command::Resume {
        interval, deadline, ..
    } = command
    {
        return resume(
            args,
            client,
            &directory,
            &release,
            &mut progress,
            *interval,
            *deadline,
        );
    }
    require_account(&release, client.me()?.user_id)?;
    let snapshot = inspect(client, &release)?;
    if matches!(
        command,
        Command::Delegate { .. } | Command::RevokeDelegation { .. }
    ) {
        return handoff::execute(
            client,
            command,
            &directory,
            &release,
            &mut progress,
            &snapshot,
        );
    }
    retain_identity(&directory, &release, &mut progress, &snapshot)?;
    match command {
        Command::Status { .. } => complete(args, &directory, &release, &progress, &snapshot, 0),
        Command::Review { .. } => {
            if progress.registered_identity.is_none() {
                return Err(invalid(
                    "Resume the saved registration before reviewing this release.",
                )
                .into());
            }
            require_ready(&release, &snapshot)?;
            if progress.observed_current
                || snapshot
                    .publication
                    .as_ref()
                    .is_some_and(|v| v["status"] == "published")
            {
                return Err(invalid("This candidate has already been activated; inspect status. Rollback requires a fresh candidate.").into());
            }
            // Only this explicit command may refresh a saved guard after a conflict.
            progress.review = Some(review(&snapshot));
            directory.write("state.json", &progress, true)?;
            complete(args, &directory, &release, &progress, &snapshot, 0)
        }
        Command::Activate { .. } => {
            activate(args, client, &directory, &release, &mut progress, snapshot)
        }
        _ => unreachable!(),
    }
}

fn init(
    client: &EnumerayClient,
    model: &str,
    path: &Path,
    file: Option<&Path>,
    rollback: Option<&str>,
    adopted: Option<&str>,
) -> Result<Value, Error> {
    if std::fs::symlink_metadata(path).is_ok() {
        return Err(invalid(
            "Release destination already exists; resume its saved operation or choose a new directory.",
        ));
    }
    if !enumeray_contracts::models::validation::valid_model_id(model) {
        return Err(invalid("Invalid model ID."));
    }
    let user = client.me()?.user_id;
    let mut current = management_json(client.management().managed_model(model)?)?;
    if current["model_id"] != model {
        return Err(invalid("Service returned another model."));
    }
    if current["can_manage"] != true {
        return Err(invalid("Model administration is required for a release."));
    }
    if let Some(id) = current_id(&current)? {
        let publication: Value = management_json(client.management().publication(model, &id)?)?;
        validate_current(&publication, model, &id)?;
        current["current_publication"] = identity(&publication);
    }
    let (candidate, source) = if let Some(id) = adopted {
        let publication: Value = management_json(client.management().publication(model, id)?)?;
        validate_current(&publication, model, id)?;
        let descriptor: InstallationDescriptor =
            serde_json::from_value(publication["descriptor"].clone())?;
        let candidate = PublicationCreate {
            id: id.into(),
            context_id: required_string(&publication, "context_id")?.into(),
            definition: serde_json::from_value(publication["definition"].clone())?,
            descriptor: descriptor.spec(),
        };
        (
            candidate,
            json!({"kind":"adopted","publication":publication}),
        )
    } else if let Some(file) = file {
        let (source, parsed) = publication_source(file)?;
        if parsed.model.id != model {
            return Err(invalid(
                "Publication source model ID differs from the selected release model.",
            ));
        }
        let candidate = parsed
            .candidate(format!("pub-{}", Uuid::new_v4()))
            .map_err(|error| invalid(error.to_string()))?;
        (
            candidate,
            json!({"kind":"publication","publication":source}),
        )
    } else if let Some(id) = rollback {
        let prior: Value = management_json(client.management().publication(model, id)?)?;
        if prior["id"] != id || prior["model_id"] != model {
            return Err(invalid(
                "Rollback source does not match the selected model/publication.",
            ));
        }
        if !prior["installation_revoked_at"].is_null()
            || prior["rollback_availability"]["unavailable_reason"] == "installation_revoked"
        {
            return Err(invalid(
                "Rollback installation is permanently revoked; use a new authorized installation.",
            ));
        }
        let descriptor: InstallationDescriptor =
            serde_json::from_value(prior["descriptor"].clone())?;
        let candidate = PublicationCreate {
            id: format!("pub-{}", Uuid::new_v4()),
            definition: serde_json::from_value(prior["definition"].clone())?,
            context_id: required_string(&prior, "context_id")?.into(),
            descriptor: descriptor.spec(),
        };
        (
            candidate,
            json!({"kind":"rollback","publication":prior,"requires_revalidation":true}),
        )
    } else {
        return Err(invalid(
            "Select a publication source, existing candidate, or rollback publication.",
        ));
    };
    validate_publication(&candidate)?;
    let release = Release {
        schema_version: 1,
        operation_id: Uuid::new_v4(),
        api_url: client.base_url().into(),
        user_id: user,
        model_id: model.into(),
        candidate,
        source,
        initial_current: current,
    };
    let progress = Progress {
        delegation_attempts: vec![],
        schema_version: 1,
        release_sha256: digest(&release)?,
        registration_started: false,
        registration_response: None,
        registered_identity: adopted.map(|_| identity(&release.source["publication"])),
        review: None,
        activation_attempts: vec![],
        observed_current: false,
    };
    // No server mutation is possible before all three files are durable.
    let directory = Directory::create(path)?;
    directory.write("release.json", &release, false)?;
    directory.write("publication.json", &envelope(&release.candidate), false)?;
    directory.write("state.json", &progress, false)?;
    let snapshot = Snapshot {
        model: release.initial_current.clone(),
        publication: adopted.map(|_| release.source["publication"].clone()),
        grant: None,
        grant_checked: false,
    };
    summary(&directory, &release, &progress, &snapshot)
}

fn required_string<'a>(value: &'a Value, key: &str) -> Result<&'a str, Error> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| invalid(format!("Service omitted {key}.")))
}

fn require_account(release: &Release, user: Uuid) -> Result<(), Error> {
    if release.user_id != user {
        return Err(invalid(
            "Release belongs to another API/account; use its original profile.",
        ));
    }
    Ok(())
}

fn current_id(model: &Value) -> Result<Option<String>, Error> {
    match model.get("current_publication_id") {
        Some(Value::Null) => Ok(None),
        Some(Value::String(id)) if !id.is_empty() => Ok(Some(id.clone())),
        _ => Err(invalid(
            "Service omitted a valid current publication guard.",
        )),
    }
}

fn runtime() -> Result<tokio::runtime::Runtime, Error> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(Into::into)
}

fn inspect(client: &EnumerayClient, release: &Release) -> Result<Snapshot, Failure> {
    let reader = client.observation_client()?;
    runtime()?.block_on(observe(&reader, release))
}

async fn observe(reader: &ObservationClient, release: &Release) -> Result<Snapshot, Failure> {
    let mut model =
        read_management::<ManagedModel>(reader, &resource_path(&["models", &release.model_id])?)
            .await?;
    if model["model_id"] != release.model_id {
        return Err(invalid("Service returned another model.").into());
    }
    current_id(&model)?;
    let publication: Option<Value> = match reader
        .get::<PublicationDetail>(&resource_path(&[
            "models",
            &release.model_id,
            "publications",
            &release.candidate.id,
        ])?)
        .await
    {
        Ok(value) => Some(management_json(value)?),
        Err(enumeray_client::observation::ObservationError::Request(Error::Api {
            status: 404,
            ..
        })) => None,
        Err(error) => return Err(error.into()),
    };
    if let Some(id) = current_id(&model)? {
        let current = if id == release.candidate.id {
            publication
                .clone()
                .ok_or_else(|| invalid("Current candidate is no longer readable."))?
        } else {
            management_json(
                reader
                    .get::<PublicationDetail>(&resource_path(&[
                        "models",
                        &release.model_id,
                        "publications",
                        &id,
                    ])?)
                    .await?,
            )?
        };
        validate_current(&current, &release.model_id, &id)?;
        model["current_publication"] = identity(&current);
    }
    let grant_id = publication.as_ref().and_then(|p| p["grant_id"].as_str());
    let server_authority = publication
        .as_ref()
        .is_some_and(|p| p["release_authority"].is_object());
    if release.source["kind"] == "adopted" && !server_authority {
        return Err(invalid("Service omitted candidate handoff authority.").into());
    }
    let grant = if server_authority {
        None
    } else {
        active_grant(reader, &release.candidate.context_id, grant_id).await?
    };
    if grant
        .as_ref()
        .is_some_and(|g| g["user_id"] != release.user_id.to_string())
    {
        return Err(invalid("Service returned compute authority for another account.").into());
    }
    Ok(Snapshot {
        model,
        publication,
        grant,
        grant_checked: !server_authority,
    })
}

async fn active_grant(
    reader: &ObservationClient,
    context: &str,
    exact: Option<&str>,
) -> Result<Option<Value>, Failure> {
    let mut cursors = BTreeSet::new();
    let mut url = reqwest::Url::parse("https://enumeray.invalid/api/v1/compute-grants?limit=100")
        .expect("constant URL");
    for _ in 0..1024 {
        let page = read_management::<Collection<AvailableGrant>>(
            reader,
            &format!("{}?{}", url.path(), url.query().unwrap_or_default()),
        )
        .await?;
        let items = page["items"]
            .as_array()
            .ok_or_else(|| invalid("Service omitted compute grant inventory."))?;
        if let Some(grant) = items
            .iter()
            .find(|g| g["context_id"] == context && exact.is_none_or(|id| g["id"] == id))
        {
            return Ok(Some(grant.clone()));
        }
        match page.get("next_cursor") {
            Some(Value::Null) => return Ok(None),
            Some(Value::String(cursor)) if !cursor.is_empty() && cursors.insert(cursor.clone()) => {
                url.set_query(None);
                url.query_pairs_mut()
                    .append_pair("limit", "100")
                    .append_pair("cursor", cursor);
            }
            _ => return Err(invalid("Invalid or repeated compute grant cursor.").into()),
        }
    }
    Err(invalid("Compute grant inventory exceeded the release inspection limit.").into())
}

fn identity(publication: &Value) -> Value {
    json!({"id":publication["id"],"model_id":publication["model_id"],"definition":publication["definition"],"context_id":publication["context_id"],
        "grant_id":publication["grant_id"],"descriptor":publication["descriptor"],"execution_location":publication["execution_location"],"disclosure":publication["disclosure"]})
}

fn validate_current(publication: &Value, model: &str, id: &str) -> Result<(), Error> {
    if publication["id"] != id || publication["model_id"] != model {
        return Err(invalid(
            "Current publication inspection returned conflicting identity.",
        ));
    }
    let _: InstallationDescriptor = serde_json::from_value(publication["descriptor"].clone())?;
    let _: enumeray_contracts::models::ScientificDefinition =
        serde_json::from_value(publication["definition"].clone())?;
    required_string(publication, "disclosure")?;
    required_string(publication, "execution_location")?;
    Ok(())
}

fn retain_identity(
    directory: &Directory,
    release: &Release,
    progress: &mut Progress,
    snapshot: &Snapshot,
) -> Result<(), Error> {
    if let Some(publication) = &snapshot.publication {
        if !publication.get("configuration_retention").is_some_and(|v| {
            matches!(
                v["state"].as_str(),
                Some("retained" | "retirement_requested" | "retired")
            )
        }) || !publication
            .get("rollback_availability")
            .is_some_and(|v| v.get("unavailable_reason").is_some())
            || publication.get("revoked_at").is_none()
            || publication.get("installation_revoked_at").is_none()
        {
            return Err(invalid("Publication omitted lifecycle authority evidence."));
        }
        let descriptor: InstallationDescriptor =
            serde_json::from_value(publication["descriptor"].clone())?;
        let spec = PublicationCreate {
            id: required_string(publication, "id")?.into(),
            context_id: required_string(publication, "context_id")?.into(),
            definition: serde_json::from_value(publication["definition"].clone())?,
            descriptor: descriptor.spec(),
        };
        if publication["model_id"] != release.model_id
            || !equivalent(&envelope(&spec), &envelope(&release.candidate))
        {
            return Err(invalid(
                "Registered publication conflicts with the saved candidate; preserve the original manifest.",
            ));
        }
        required_string(publication, "grant_id")?;
        required_string(publication, "execution_location")?;
        required_string(publication, "disclosure")?;
        if !matches!(
            publication["status"].as_str(),
            Some("validating" | "ready" | "failed" | "published")
        ) {
            return Err(invalid(
                "Publication omitted a recognized validation status.",
            ));
        }
        if (progress.observed_current
            || progress
                .activation_attempts
                .iter()
                .any(|a| a.response.is_some()))
            && publication["status"] != "published"
            && publication["published_at"].is_null()
        {
            return Err(invalid(
                "Saved activation evidence conflicts with the candidate's publication state.",
            ));
        }
        let observed = identity(publication);
        if let Some(saved) = &progress.registered_identity {
            if !equivalent(saved, &observed) {
                return Err(invalid(
                    "Registered candidate identity changed; review the retained release evidence.",
                ));
            }
        } else if progress.registration_started || release.source["kind"] == "adopted" {
            progress.registered_identity = Some(observed);
        }
        if current_id(&snapshot.model)?.as_deref() == Some(&release.candidate.id) {
            progress.observed_current = true;
        }
    } else if progress.registered_identity.is_some() {
        return Err(invalid(
            "Saved candidate is no longer readable; restore model authority before resuming.",
        ));
    }
    directory.write("state.json", progress, true)
}

fn blockers(release: &Release, snapshot: &Snapshot) -> Vec<String> {
    let mut reasons = Vec::new();
    if snapshot.model["can_manage"] != true {
        reasons.push("model_administration_required".into());
    }
    if snapshot.grant_checked && snapshot.grant.is_none() {
        reasons.push(
            if snapshot.publication.is_some() {
                "captured_grant_unavailable"
            } else {
                "compute_authorization_unavailable"
            }
            .into(),
        );
    }
    if let Some(p) = &snapshot.publication {
        if let Some(authority) = p.get("release_authority")
            && authority["available"] != true
        {
            reasons.push(
                authority["unavailable_reason"]
                    .as_str()
                    .unwrap_or("publication_authority_unavailable")
                    .into(),
            );
        }
        if !p["revoked_at"].is_null() {
            reasons.push("publication_revoked".into());
        }
        if !p["installation_revoked_at"].is_null()
            || p["rollback_availability"]["unavailable_reason"] == "installation_revoked"
        {
            reasons.push("installation_revoked".into());
        }
        if p["configuration_retention"]["state"]
            .as_str()
            .is_some_and(|state| state != "retained")
        {
            reasons.push("configuration_retired".into());
        }
        if p["rollback_availability"]["unavailable_reason"] == "compute_authorization_unavailable" {
            reasons.push("compute_authorization_unavailable".into());
        }
        if p["status"] == "failed" {
            reasons.push("installation_validation_failed".into());
        }
        if p["status"] == "published"
            && snapshot.model["current_publication_id"] != release.candidate.id
        {
            reasons.push("historical_publication_requires_fresh_candidate".into());
        }
    }
    reasons
}

fn require_ready(release: &Release, snapshot: &Snapshot) -> Result<(), Error> {
    let reasons = blockers(release, snapshot);
    if !reasons.is_empty() {
        return Err(invalid(format!(
            "Release blocked: {}. Inspect status; restore authority or prepare a fresh candidate as indicated.",
            reasons.join(", ")
        )));
    }
    if snapshot
        .publication
        .as_ref()
        .is_none_or(|p| p["status"] != "ready")
    {
        return Err(invalid(
            "Candidate is not ready; resume registration/readiness before review or activation.",
        ));
    }
    Ok(())
}

fn review(snapshot: &Snapshot) -> Review {
    Review {
        authority: snapshot
            .publication
            .as_ref()
            .and_then(|p| p.get("release_authority"))
            .cloned(),
        expected_current: current_id(&snapshot.model).expect("validated guard"),
        candidate: identity(snapshot.publication.as_ref().expect("ready candidate")),
        current: snapshot.model["current_publication"].clone(),
    }
}

#[allow(clippy::too_many_arguments)]
fn resume(
    args: &Args,
    client: &EnumerayClient,
    directory: &Directory,
    release: &Release,
    progress: &mut Progress,
    interval: u64,
    seconds: u64,
) -> Result<Completion, Failure> {
    let until = Instant::now()
        .checked_add(Duration::from_secs(seconds))
        .ok_or_else(|| invalid("Release deadline is too large."))?;
    let cancellation = enumeray_client::CancellationToken::new();
    let client = client
        .with_cancellation(cancellation.clone())
        .with_deadline(until.into_std());
    let reader = client.observation_client()?;
    let runtime = runtime()?;
    let result = runtime.block_on(async {
        tokio::select! {
            biased;
            signal = tokio::signal::ctrl_c() => {
                cancellation.cancel();
                signal.map_err(Error::from)?;
                Err(Failure { error: invalid("Release interrupted; resume the same directory to reconcile registration. Activation has not been requested."), exit: 130 })
            },
            () = sleep_until(until) => {
                cancellation.cancel();
                Err(Failure { error: invalid("Release readiness deadline elapsed; resume the same directory to reconcile registration. Activation has not been requested."), exit: 5 })
            },
            result = async {
                require_account(release, reader.me().await?.user_id)?;
                if progress.registered_identity.is_none() {
                    let snapshot = observe(&reader, release).await?;
                    retain_identity(directory, release, progress, &snapshot)?;
                    if progress.registered_identity.is_none() {
                        if !blockers(release, &snapshot).is_empty() { return complete(args, directory, release, progress, &snapshot, 1); }
                        progress.registration_started = true;
                        directory.write("state.json", progress, true)?;
                        let request_client = client.with_timeout_limit(until.saturating_duration_since(Instant::now()))?;
                        let path = resource_path(&["models", &release.model_id, "publications"])?;
                        let candidate = release.candidate.clone();
                        // This checkpointed request may finish saving rotated credentials.
                        // Its cancellation gate prevents later business dispatch; an
                        // already-dispatched registration uses the saved recovery journal.
                        let reply = management_json(tokio::task::spawn_blocking(move || request_client.request::<PublicationRegistration, _>(reqwest::Method::POST, &path, Some(&candidate), None))
                            .await.map_err(|_| invalid("Registration worker stopped; resume the saved candidate."))??)?;
                        if reply["id"] != release.candidate.id || !matches!(reply["status"].as_str(), Some("validating" | "ready" | "failed" | "published")) {
                            return Err(invalid("Registration reply conflicts with the saved publication; resume this same directory.").into());
                        }
                        progress.registration_response = Some(reply);
                        directory.write("state.json", progress, true)?;
                    }
                }
                loop {
                    let snapshot = observe(&reader, release).await?;
                    retain_identity(directory, release, progress, &snapshot)?;
                    let blocked = !blockers(release, &snapshot).is_empty();
                    if blocked || snapshot.publication.as_ref().is_some_and(|p| p["status"] == "ready" || p["status"] == "published") {
                        return complete(args, directory, release, progress, &snapshot, u8::from(blocked));
                    }
                    sleep(Duration::from_secs(interval)).await;
                }
            } => result,
        }
    });
    cancellation.cancel();
    runtime.shutdown_background();
    result
}

fn activate(
    args: &Args,
    client: &EnumerayClient,
    directory: &Directory,
    release: &Release,
    progress: &mut Progress,
    snapshot: Snapshot,
) -> Result<Completion, Failure> {
    let saved = progress.review.clone().ok_or_else(|| invalid("Explicit release review is required before activation; run model release review with this directory."))?;
    // An observed current candidate is evidence, not an invented activation receipt.
    if progress.observed_current
        || progress
            .activation_attempts
            .iter()
            .any(|attempt| attempt.response.is_some())
    {
        return complete(args, directory, release, progress, &snapshot, 0);
    }
    require_ready(release, &snapshot)?;
    if saved != review(&snapshot) {
        return Err(invalid("Current publication or reviewed evidence changed; run model release review again before explicitly activating.").into());
    }
    let reuse = progress
        .activation_attempts
        .last()
        .is_some_and(|a| a.response.is_none() && a.review == saved);
    if !reuse {
        if progress.activation_attempts.len() >= 128 {
            return Err(invalid("Release activation history is full; preserve this evidence. Initialize another private operation with model release init MODEL --candidate PUBLICATION --directory NEW and review again. A historical published release requires --rollback-from and a fresh authorized candidate.").into());
        }
        progress.activation_attempts.push(Attempt {
            request: PublicationActivation {
                publication_delegation_id: saved
                    .authority
                    .as_ref()
                    .and_then(|a| a["publication_delegation_id"].as_str())
                    .map(str::parse)
                    .transpose()
                    .map_err(|_| invalid("Invalid reviewed delegation identity."))?,
                expected_current_publication_id: saved.expected_current.clone(),
            },
            review: saved,
            response: None,
        });
    }
    directory.write("state.json", progress, true)?;
    let request = &progress
        .activation_attempts
        .last()
        .expect("saved attempt")
        .request;
    let reply = management_json(client.request::<PublicationActivationReceipt, _>(
        reqwest::Method::POST,
        &resource_path(&[
            "models",
            &release.model_id,
            "publications",
            &release.candidate.id,
            "activation",
        ])?,
        Some(request),
        None,
    )?)?;
    if reply["id"] != release.candidate.id || reply["status"] != "published" {
        return Err(invalid("Activation reply could not be verified; inspect status using the same release directory.").into());
    }
    progress
        .activation_attempts
        .last_mut()
        .expect("saved attempt")
        .response = Some(reply);
    directory.write("state.json", progress, true)?;
    let snapshot = inspect(client, release)?;
    retain_identity(directory, release, progress, &snapshot)?;
    complete(args, directory, release, progress, &snapshot, 0)
}

fn summary(
    directory: &Directory,
    release: &Release,
    progress: &Progress,
    snapshot: &Snapshot,
) -> Result<Value, Error> {
    let p = snapshot.publication.as_ref();
    let reasons = blockers(release, snapshot);
    let current = current_id(&snapshot.model).expect("validated guard");
    let reviewed = progress.review.as_ref();
    let guard_changed = reviewed.is_some_and(|r| {
        r.expected_current != current
            || r.authority != p.and_then(|p| p.get("release_authority")).cloned()
    });
    let activated = progress.observed_current
        || progress
            .activation_attempts
            .iter()
            .any(|a| a.response.is_some());
    let (stage, next) = if activated {
        ("activated", "status")
    } else if !reasons.is_empty() {
        ("blocked", "status")
    } else if progress.registered_identity.is_some() && p.is_some_and(|p| p["status"] == "ready") {
        if reviewed.is_some() && !guard_changed {
            ("reviewed", "activate")
        } else {
            ("ready", "review")
        }
    } else if progress.registration_started {
        ("validating", "resume")
    } else {
        ("prepared", "resume")
    };
    let baseline = reviewed
        .map(|r| r.current.clone())
        .unwrap_or_else(|| release.initial_current["current_publication"].clone());
    let field = |name: &str| p.and_then(|p| p.get(name)).cloned().unwrap_or(Value::Null);
    let summary = ReleaseSummary {
        schema_version: SchemaVersion, directory: directory.root.to_string_lossy().into_owned(), operation_id: release.operation_id,
        model_id: release.model_id.clone(), publication_id: release.candidate.id.clone(),
        stage: serde_json::from_value(json!(stage))?, candidate_status: serde_json::from_value(field("status"))?,
        validation_message: serde_json::from_value(field("validation_message"))?,
        model_generation: snapshot.model["generation"].as_i64().ok_or_else(|| invalid("Service omitted model generation."))?,
        release_authority: serde_json::from_value(field("release_authority"))?,
        delegation_attempts: progress.delegation_attempts.iter().map(|attempt| Ok(enumeray_contracts::automation::ReleaseDelegationAttempt {
            id: attempt.id, user_id: attempt.user_id, expected_generation: attempt.expected_generation,
            response: attempt.response.clone().map(serde_json::from_value).transpose()?,
        })).collect::<Result<_, Error>>()?,
        current_publication_id: current, expected_current_publication_id: reviewed.and_then(|r| r.expected_current.clone()), guard_changed,
        candidate_is_current: snapshot.model["current_publication_id"] == release.candidate.id,
        activation_receipt: progress.activation_attempts.last().and_then(|a| a.response.clone()).map(serde_json::from_value).transpose()?,
        activation_uncertain: progress.activation_attempts.last().is_some_and(|a| a.response.is_none()) && !progress.observed_current,
        registration_uncertain: progress.registration_started && progress.registered_identity.is_none(),
        comparison: ReleaseComparison {current:baseline,candidate:p.map(identity).unwrap_or_else(|| json!({"id":release.candidate.id,"context_id":release.candidate.context_id,"definition":release.candidate.definition,"descriptor":release.candidate.descriptor})),observed_current:snapshot.model["current_publication"].clone()},
        execution_location: serde_json::from_value(field("execution_location"))?, disclosure: serde_json::from_value(field("disclosure"))?,
        trial_availability: serde_json::from_value(field("trial_availability"))?, trial_evidence: serde_json::from_value(field("trial_evidence"))?,
        trial_init_command: p.filter(|p| p["trial_availability"]["available"] == true).map(|_| format!("enumeray analysis init {} --candidate {} --inputs INPUTS.json --directory NEW_TRIAL_DIR", release.model_id, release.candidate.id)),
        rollback_source: release.source.get("publication").cloned(), next_command: format!("enumeray model release {next} {}",directory.root.display()),
        next_action: if activated { "Inspect current availability; accepted work retains its original publication." } else if !reasons.is_empty() { blocked_action(&reasons) } else if next == "review" { "Review trial evidence and scientific checks, then save the activation comparison." } else if next == "activate" { "Activate explicitly after reviewing the saved comparison and disclosure." } else { "Resume this exact saved candidate; readiness never activates it." }.into(),
        blockers: reasons,
    };
    summary.validate().map_err(invalid)?;
    Ok(serde_json::to_value(summary)?)
}

fn blocked_action(reasons: &[String]) -> &'static str {
    match reasons.first().map(String::as_str) {
        Some("model_administration_required") => {
            "Ask the model owner to restore this account's administration before resuming this directory."
        }
        Some("captured_grant_or_delegation_required" | "publication_authority_unavailable") => {
            "Ask the captured grant holder for a fresh named delegation, then review this release again."
        }
        Some("captured_grant_unavailable") => {
            "Ask the connector owner about the exact captured grant. A reissued grant requires a fresh candidate in a new release directory."
        }
        Some("installation_revoked") => {
            "Use a new authorized installation and a fresh candidate; permanent installation revocation cannot be reversed."
        }
        Some(
            "publication_revoked"
            | "configuration_retired"
            | "historical_publication_requires_fresh_candidate",
        ) => {
            "Initialize a fresh candidate in a new release directory with current authorization and installation validation."
        }
        Some("installation_validation_failed") => {
            "Ask the connector owner to resolve the validation message, then resume. Changed science or installation settings require a fresh candidate."
        }
        _ => {
            "Ask the connector owner to restore context/connector compute authority, then inspect this saved release again."
        }
    }
}

fn complete(
    args: &Args,
    directory: &Directory,
    release: &Release,
    progress: &Progress,
    snapshot: &Snapshot,
    exit: u8,
) -> Result<Completion, Failure> {
    let mut value = summary(directory, release, progress, snapshot)?;
    next_invocation(args, &mut value);
    Ok(Completion {
        value: Some(value),
        exit,
    })
}

fn next_invocation(args: &Args, value: &mut Value) {
    let next = match value["stage"].as_str() {
        Some("activated" | "blocked") => "status",
        Some("ready") => "review",
        Some("reviewed") => "activate",
        _ => "resume",
    };
    let words = [
        "model",
        "release",
        next,
        value["directory"].as_str().expect("directory"),
    ]
    .map(str::to_owned);
    value["next_command"] = json!(crate::output::invocation(args, &words));
    if value["trial_init_command"].is_string() {
        let words = [
            "analysis",
            "init",
            value["model_id"].as_str().expect("model"),
            "--candidate",
            value["publication_id"].as_str().expect("publication"),
            "--inputs",
            "INPUTS.json",
            "--directory",
            "NEW_TRIAL_DIR",
        ]
        .map(str::to_owned);
        value["trial_init_command"] = json!(crate::output::invocation(args, &words));
    }
}

async fn read_management<T: serde::de::DeserializeOwned + serde::Serialize>(
    reader: &ObservationClient,
    path: &str,
) -> Result<Value, Failure> {
    Ok(management_json(reader.get::<T>(path).await?)?)
}

fn management_json<T: serde::Serialize>(value: T) -> Result<Value, Error> {
    serde_json::to_value(value).map_err(Error::from)
}
