//! One user-authorized execution, with local durable checkpoints at every mutation boundary.
mod cancel;
mod collect;
pub(crate) mod recovery;
pub(crate) mod state;

use crate::{
    app::{Completion, Failure},
    args::{Analysis, Args},
    observation,
    transfers::{self, UploadOperation},
};
use enumeray_client::{
    EnumerayClient, Error, PreparedRun, RunSubmission, UploadChecksum, UploadRequest, resource_path,
};
use enumeray_contracts::{
    automation::{
        AnalysisIdentity, AnalysisStage, AnalysisSummary, InputCollectionProgress,
        InputFileProgress, SchemaVersion,
    },
    models::{self, ScientificDefinition},
};
use serde_json::{Value, json};
use state::{
    AnalysisState, Binding, Directory, Saved, invalid, upload_path, validate_receipt, verify_source,
};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    path::{Path, PathBuf},
};
use uuid::Uuid;

/// An immutable, account-validated observation snapshot. The observer resolves it
/// only after its deadline and signal handling are active.
pub(crate) struct ObservationTarget {
    pub run_id: String,
    pub root: PathBuf,
    directory: Directory,
    saved: AnalysisState,
}

impl ObservationTarget {
    pub(crate) fn validate(&self, run: &Value) -> Result<(), Error> {
        validate_run(&self.saved, run)
    }

    pub(crate) fn finish(&self, run: &Value, save: bool) -> Result<(), Error> {
        self.validate(run)?;
        if save {
            self.directory.write("state/last-run.json", run)?;
        }
        Ok(())
    }
}

pub(crate) fn observation_target(
    api_url: &str,
    user_id: Uuid,
    path: &Path,
) -> Result<ObservationTarget, Error> {
    let directory = Directory::open(path)?;
    let saved = {
        let _snapshot = directory.state_lock()?;
        directory.load(api_url, user_id)?
    };
    Ok(ObservationTarget {
        run_id: accepted_id(&saved)?,
        root: directory.root.clone(),
        directory,
        saved,
    })
}

pub(crate) fn execute(
    args: &Args,
    client: &EnumerayClient,
    command: &Analysis,
) -> Result<Completion, Failure> {
    execute_inner(args, client, command, None)
}

/// A parent workflow must retain the original retry identity even if a directory
/// is replaced with another otherwise-valid analysis for the same candidate.
pub(crate) fn execute_bound(
    args: &Args,
    client: &EnumerayClient,
    command: &Analysis,
    key: &str,
) -> Result<Completion, Failure> {
    execute_inner(args, client, command, Some(key))
}

pub(crate) fn local_run_key(client: &EnumerayClient, path: &Path) -> Result<String, Error> {
    let user = client.me()?.user_id;
    let directory = Directory::open(path)?;
    let _snapshot = directory.state_lock()?;
    let saved = directory.load(client.base_url(), user)?;
    saved
        .local()
        .map(|s| s.run_key.clone())
        .ok_or_else(|| invalid("Guided trials require their original locally prepared analysis."))
}

fn execute_inner(
    args: &Args,
    client: &EnumerayClient,
    command: &Analysis,
    expected_key: Option<&str>,
) -> Result<Completion, Failure> {
    if let Analysis::Init {
        model,
        candidate,
        directory,
        example,
        inputs,
        file,
        collection,
        collection_manifest,
        media_type,
        metadata,
    } = command
    {
        return init_with_collections(
            client,
            model,
            candidate.as_deref(),
            directory,
            example.as_deref(),
            inputs.as_deref(),
            file,
            media_type,
            metadata,
            collection,
            collection_manifest,
        )
        .map(|value| Completion {
            value: Some(value),
            exit: 0,
        })
        .map_err(Into::into);
    }
    if let Analysis::Attach {
        run_id,
        receipt,
        directory,
    } = command
    {
        return recovery::attach(client, run_id.as_deref(), receipt.as_deref(), directory)
            .map(|value| Completion {
                value: Some(value),
                exit: 0,
            })
            .map_err(Into::into);
    }
    if matches!(
        command,
        Analysis::Wait { .. } | Analysis::Logs { follow: true, .. }
    ) {
        if let Some(key) = expected_key {
            let path = match command {
                Analysis::Wait { directory, .. } | Analysis::Logs { directory, .. } => directory,
                _ => unreachable!(),
            };
            if local_run_key(client, path)? != key {
                return Err(invalid("Saved analysis retry identity differs from its parent workflow; no replacement execution was authorized.").into());
            }
        }
        return observation::analysis(args, client);
    }
    let directory = match command {
        Analysis::Submit { directory }
        | Analysis::Status { directory }
        | Analysis::Wait { directory, .. }
        | Analysis::Logs { directory, .. }
        | Analysis::Cancel { directory }
        | Analysis::Collect { directory, .. } => directory,
        Analysis::Init { .. } | Analysis::Attach { .. } | Analysis::Import { .. } => unreachable!(),
    };
    let mut directory = Directory::open(directory)?;
    if matches!(command, Analysis::Submit { .. } | Analysis::Collect { .. }) {
        directory.claim_writer()?;
    }
    // Account discovery must not hold any state lock: expired or slow credentials
    // cannot block another process's status, cancellation, or evidence publication.
    let user_id = client
        .me()
        .map_err(|error| {
            if matches!(command, Analysis::Collect { .. }) {
                transfers::download_authorization_hint(error)
            } else {
                if matches!(&error, Error::Api { status: 401 | 403, .. }) {
                    eprintln!("Analysis account authorization is unavailable. Renew the same account with auth refresh/login and repeat this directory command. Retain this directory; do not create a replacement execution.");
                }
                error
            }
        })?
        .user_id;
    let mut saved = {
        let _snapshot = directory.state_lock()?;
        directory.load(client.base_url(), user_id)?
    };
    if expected_key.is_some_and(|key| saved.local().is_none_or(|s| s.run_key != key)) {
        return Err(invalid("Saved analysis retry identity differs from its parent workflow; no replacement execution was authorized.").into());
    }
    if matches!(command, Analysis::Status { .. } | Analysis::Collect { .. })
        && let AnalysisState::Recovery(recovered) = &mut saved
        && recovered.run_id().is_some()
    {
        recovery::hydrate(client, &directory, recovered)?;
    }
    let (collection, cancellation) = if matches!(command, Analysis::Status { .. }) {
        let _snapshot = directory.state_lock()?;
        (
            collect::status(&directory, &saved)?,
            cancel::status(&directory, &saved)?,
        )
    } else {
        (Value::Null, Value::Null)
    };
    let value = match command {
        Analysis::Submit { .. } => match &mut saved {
            AnalysisState::Local(local) => submit(client, &directory, local)?,
            AnalysisState::Recovery(recovered) => recovery::submit(client, &directory, recovered)?,
        },
        Analysis::Status { .. } => {
            let run = saved
                .run_id()
                .map(|id| get(client, &["runs", &id]))
                .transpose()?;
            if let Some(run) = &run {
                validate_run(&saved, run)?;
            }
            let mut summary = summary(&directory, &saved, run)?;
            summary["collection"] = collection;
            summary["cancellation"] = cancellation;
            if matches!(
                summary["cancellation"]["attempt"].as_str(),
                Some("pending" | "uncertain")
            ) {
                summary["next_action"] = json!(
                    "Local cancellation has no confirmed reply. Inspect the authoritative run state; if cancellation is still intended, explicitly repeat analysis cancel DIR for the same run. Status never retries cancellation."
                );
            }
            validated_summary(summary)?
        }
        Analysis::Logs { page, .. } => {
            let id = accepted_id(&saved)?;
            let mut logs = crate::commands::paged_get(client, &["runs", &id, "logs"], page)?;
            add_identity(&saved, &mut logs);
            logs["run_id"] = json!(id);
            logs
        }
        Analysis::Cancel { .. } => cancel::cancel(client, &directory, &saved)?,
        Analysis::Wait { .. } => unreachable!("guarded observation dispatch"),
        Analysis::Collect {
            member,
            evidence_only,
            ..
        } => collect::collect(client, &directory, &saved, member, *evidence_only)
            .map_err(transfers::download_authorization_hint)?,
        Analysis::Init { .. } | Analysis::Attach { .. } | Analysis::Import { .. } => unreachable!(),
    };
    Ok(Completion {
        value: Some(value),
        exit: 0,
    })
}

fn pairs(values: &[String]) -> Result<BTreeMap<String, String>, Error> {
    let mut result = BTreeMap::new();
    for value in values {
        let (slot, value) = value
            .split_once('=')
            .filter(|(slot, value)| !slot.is_empty() && !value.is_empty())
            .ok_or_else(|| {
                invalid("File bindings, media types and metadata must use SLOT=VALUE.")
            })?;
        if result.insert(slot.into(), value.into()).is_some() {
            return Err(invalid("Duplicate analysis slot option."));
        }
    }
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
fn init_with_collections(
    client: &EnumerayClient,
    model: &str,
    candidate: Option<&str>,
    path: &Path,
    example: Option<&str>,
    inputs_path: Option<&Path>,
    files: &[String],
    media_types: &[String],
    metadata: &[String],
    collection_dirs: &[String],
    collection_manifests: &[String],
) -> Result<Value, Error> {
    if fs::symlink_metadata(path).is_ok() {
        return Err(invalid(
            "Analysis destination already exists; use its saved operation or choose a new directory.",
        ));
    }
    let collection_dirs = pairs(collection_dirs)?;
    let collection_manifests = pairs(collection_manifests)?;
    if collection_dirs
        .keys()
        .any(|k| collection_manifests.contains_key(k))
    {
        return Err(invalid(
            "A collection binding selects either a directory or manifest.",
        ));
    }
    let files = pairs(files)?;
    let media_types = pairs(media_types)?;
    let metadata = pairs(metadata)?;
    if media_types.keys().chain(metadata.keys()).any(|key| {
        !files.contains_key(key)
            && !collection_dirs.contains_key(key)
            && !collection_manifests.contains_key(key)
    }) {
        return Err(invalid(
            "Media type and metadata options require a local --file binding for that slot.",
        ));
    }
    let user_id = client.me()?.user_id;
    let (mut publication, publication_id, purpose, publication_delegation_id) =
        if let Some(id) = candidate {
            let selected = crate::candidate::inspect(client, model, id)?;
            crate::candidate::validate_actor(&selected.publication, user_id)?;
            (
                selected.publication,
                id.to_owned(),
                enumeray_client::RunPurpose::CandidateTrial,
                selected.publication_delegation_id,
            )
        } else {
            let discovery = get(client, &["models", model])?;
            let publication = discovery
                .get("current_publication")
                .filter(|value| value.is_object())
                .cloned()
                .ok_or_else(|| invalid("Model has no accessible current publication."))?;
            let publication_id = discovery
                .get("current_publication_id")
                .or_else(|| publication.get("publication_id"))
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or_else(|| invalid("Model has no current publication."))?
                .to_owned();
            (
                publication,
                publication_id,
                enumeray_client::RunPurpose::Standard,
                None,
            )
        };
    publication["publication_id"] = json!(publication_id);
    if publication["execution_location"]
        .as_str()
        .is_none_or(str::is_empty)
        || publication["disclosure"].as_str().is_none_or(str::is_empty)
    {
        return Err(invalid(
            "Publication omitted its destination or disclosure.",
        ));
    }
    let definition: ScientificDefinition =
        serde_json::from_value(publication["definition"].clone())?;
    let declarations =
        models::files::declarations(&definition).map_err(|error| invalid(error.to_string()))?;
    let inputs = if let Some(path) = inputs_path {
        read_inputs(path)?
    } else {
        let name = example.ok_or_else(|| invalid("Select --example or --inputs."))?;
        definition
            .examples
            .iter()
            .find(|item| item.name == name)
            .ok_or_else(|| invalid("Named example is absent from the selected publication."))?
            .inputs
            .clone()
    };
    let mut bindings = BTreeMap::new();
    for (name, source) in files {
        let declaration = declarations
            .get(&name)
            .ok_or_else(|| invalid("Local file binding targets an undeclared file slot."))?;
        let source = PathBuf::from(source);
        let source_metadata = fs::symlink_metadata(&source)?;
        if !source_metadata.is_file() || source_metadata.file_type().is_symlink() {
            return Err(invalid(
                "Local file source must be a regular file, not a symlink.",
            ));
        }
        let source = source.canonicalize()?;
        let (size, checksum) = transfers::checksum(&mut File::open(&source)?)?;
        if size > declaration.max_bytes {
            return Err(invalid("Local file exceeds the declared slot byte limit."));
        }
        let media_type = media_types
            .get(&name)
            .cloned()
            .or_else(|| {
                (declaration.media_types.len() == 1).then(|| declaration.media_types[0].clone())
            })
            .ok_or_else(|| {
                invalid("Slot allows several media types; supply --media-type SLOT=TYPE.")
            })?;
        if !declaration.media_types.contains(&media_type) {
            return Err(invalid(
                "Media type is not permitted by the declared file slot.",
            ));
        }
        let scientific_metadata: Value = metadata
            .get(&name)
            .map(|path| read_inputs(Path::new(path)))
            .transpose()?
            .unwrap_or_else(|| json!({}));
        if !scientific_metadata.is_object() {
            return Err(invalid("Upload metadata must be a JSON object."));
        }
        models::validate_input_value(&scientific_metadata)
            .map_err(|error| invalid(error.to_string()))?;
        bindings.insert(
            name.clone(),
            Binding {
                id: Uuid::new_v4(),
                operation: UploadOperation {
                    schema_version: 1,
                    api_url: client.base_url().into(),
                    user_id,
                    source,
                    idempotency_key: Uuid::new_v4().to_string(),
                    request: UploadRequest {
                        logical_name: name,
                        media_type,
                        scientific_metadata,
                        expected_size_bytes: size,
                        checksum: UploadChecksum {
                            algorithm: "sha256".into(),
                            checksum_type: "full_object".into(),
                            value: checksum,
                        },
                    },
                    reservation: None,
                    version_id: None,
                    multipart: None,
                },
            },
        );
    }
    let saved = Saved {
        schema_version: 1,
        api_url: client.base_url().into(),
        user_id,
        model_id: model.into(),
        publication_id,
        purpose,
        publication_delegation_id,
        publication,
        bindings,
        collection_bindings: collection_dirs
            .keys()
            .chain(collection_manifests.keys())
            .map(|name| {
                (
                    name.clone(),
                    state::CollectionBinding {
                        id: Uuid::new_v4(),
                        operation: format!("state/collections/{}", Uuid::new_v4()),
                    },
                )
            })
            .collect(),
        collection_uploads: BTreeMap::new(),
        run_key: Uuid::new_v4().to_string(),
        frozen_inputs: None,
        input_artifacts: BTreeMap::new(),
        uploads: BTreeMap::new(),
        prepared: None,
        submission_started: false,
        receipt: None,
    };
    validate_template(&saved, &inputs)?;
    // Validate every document's bounds before allocating the destination.
    if serde_json::to_vec(&saved)?.len() > 8 * 1024 * 1024 {
        return Err(invalid("Analysis operation exceeds 8 MiB."));
    }
    let directory = Directory::create(path)?;
    directory.write("state/publication.json", &saved.publication)?;
    directory.write_inputs(&inputs)?;
    for binding in saved.bindings.values() {
        directory.write(&upload_path(binding), &binding.operation)?;
    }
    if !saved.collection_bindings.is_empty() {
        crate::collections::private_dir(&directory.root.join("state/collections"))?;
        let declarations = enumeray_contracts::collections::declarations(&definition, false)
            .map_err(|e| invalid(e.to_string()))?;
        for (name, binding) in &saved.collection_bindings {
            let declaration = declarations
                .get(name)
                .ok_or_else(|| invalid("Collection binding targets an undeclared slot."))?;
            let media = media_types.get(name).cloned();
            let dir = collection_dirs.get(name).map(Path::new);
            let manifest = collection_manifests.get(name).map(Path::new);
            crate::collections::prepare(
                client,
                dir,
                manifest,
                None,
                name,
                media.as_deref(),
                &directory.root.join(&binding.operation),
                false,
            )?;
            let members: Vec<enumeray_contracts::collections::MemberSpec> =
                crate::collections::read(
                    &directory
                        .root
                        .join(&binding.operation)
                        .join("manifest.json"),
                )?;
            if !declaration.permits(&members) {
                return Err(invalid(
                    "Local collection exceeds its declared limits or media types.",
                ));
            }
        }
    }
    directory.save(&saved)?;
    local_summary(&directory, &saved, None)
}

fn read_inputs(path: &Path) -> Result<Value, Error> {
    crate::config::read_inputs(path)
}

fn definition(saved: &Saved) -> Result<ScientificDefinition, Error> {
    Ok(serde_json::from_value(
        saved.publication["definition"].clone(),
    )?)
}

fn validate_template(saved: &Saved, inputs: &Value) -> Result<(), Error> {
    let mut provisional = inputs.clone();
    for (name, binding) in &saved.bindings {
        let object = provisional
            .as_object_mut()
            .ok_or_else(|| invalid("File bindings require named object inputs."))?;
        object.insert(name.clone(), json!({"artifact_id":binding.id}));
    }
    for (name, binding) in &saved.collection_bindings {
        provisional[name] = json!({"collection_id":binding.id});
    }
    models::validate_inputs(&definition(saved)?, &provisional)
        .map_err(|error| invalid(error.to_string()))
}

fn effective_inputs(saved: &Saved) -> Result<Value, Error> {
    let mut inputs = saved
        .frozen_inputs
        .clone()
        .ok_or_else(|| invalid("Analysis has no frozen inputs."))?;
    for name in saved.bindings.keys() {
        let id = saved
            .uploads
            .get(name)
            .and_then(|receipt| receipt["artifact_id"].as_str())
            .ok_or_else(|| invalid("A bound input upload has not completed."))?;
        canonical_id(id)?;
        inputs
            .as_object_mut()
            .ok_or_else(|| invalid("File inputs must be an object."))?
            .insert(name.clone(), json!({"artifact_id":id}));
    }
    for name in saved.collection_bindings.keys() {
        let reference = saved
            .collection_uploads
            .get(name)
            .and_then(|v| v.get("collection_id"))
            .ok_or_else(|| invalid("A bound collection has not completed."))?;
        inputs[name] = json!({"collection_id":reference});
    }
    Ok(inputs)
}

fn submit(
    client: &EnumerayClient,
    directory: &Directory,
    saved: &mut Saved,
) -> Result<Value, Error> {
    if saved.submission_started {
        // Do not require source files or mutable discovery to reconcile a possibly accepted POST.
        if saved.receipt.is_none() {
            accept(client, directory, saved)?;
        }
        return local_summary(directory, saved, None);
    }
    let inputs = directory.inputs()?;
    models::validate_input_value(&inputs).map_err(|error| invalid(error.to_string()))?;
    validate_template(saved, &inputs)?;
    if saved
        .frozen_inputs
        .as_ref()
        .is_some_and(|frozen| frozen != &inputs)
    {
        return Err(invalid(
            "Frozen analysis inputs changed; restore them or initialize a new directory.",
        ));
    }
    for (name, binding) in &saved.bindings {
        if saved.uploads.contains_key(name) {
            continue;
        }
        let operation: UploadOperation = directory.read(&upload_path(binding))?;
        if let Some(reservation) = &operation.reservation {
            let upload: enumeray_client::UploadView = client.get(&resource_path(&[
                "uploads",
                &reservation.upload_id.to_string(),
            ])?)?;
            if upload.reservation.upload_id != reservation.upload_id
                || upload.reservation.artifact_id != reservation.artifact_id
            {
                return Err(invalid(
                    "Upload status conflicts with the retained reservation.",
                ));
            }
            if upload.state == "completed" {
                // Transfer recovery validates the retained completion and artifact
                // below. Original bytes are needed only for unfinished uploads.
                continue;
            }
        }
        verify_source(&binding.operation).map_err(|error| {
            if matches!(&error, Error::Io(io) if io.kind() == std::io::ErrorKind::NotFound) {
                invalid(format!("Original input source is unavailable at {}. Restore the unchanged source at its saved path before uploading; retain this directory and retry analysis submit. No replacement execution or upload has been created.", binding.operation.source.display()))
            } else { error }
        })?;
    }
    if saved.purpose.is_standard() {
        let current = get(client, &["models", &saved.model_id])?;
        if current["current_publication_id"] != saved.publication_id {
            return Err(invalid(
                "Current publication changed; initialize a new directory and review its destination/disclosure.",
            ));
        }
    } else {
        let current = crate::candidate::inspect(client, &saved.model_id, &saved.publication_id)?;
        crate::candidate::validate_actor(&current.publication, saved.user_id)?;
        if crate::candidate::identity(&current.publication)
            != crate::candidate::identity(&saved.publication)
            || current.publication_delegation_id != saved.publication_delegation_id
        {
            return Err(invalid(
                "Candidate identity or captured trial authority changed; retain this directory and review the exact candidate before preparing another operation.",
            ));
        }
    }
    let definition = definition(saved)?;
    // Check all existing artifact references before any upload reservation.
    let mut provisional = inputs.clone();
    for (name, binding) in &saved.bindings {
        provisional[name] = json!({"artifact_id":binding.id});
    }
    let mut artifacts = BTreeMap::new();
    for reference in models::files::references(&definition, &provisional)
        .map_err(|error| invalid(error.to_string()))?
    {
        if saved.bindings.contains_key(&reference.input_name) {
            continue;
        }
        let artifact = get(client, &["artifacts", &reference.artifact_id.to_string()])?;
        validate_artifact(&artifact, reference.artifact_id, &reference.declaration)?;
        if let Some(retained) = saved.input_artifacts.get(&reference.input_name)
            && retained["content"] != artifact["content"]
        {
            return Err(invalid("Referenced input artifact evidence changed."));
        }
        artifacts.insert(reference.input_name, artifact);
    }
    if saved.frozen_inputs.is_none() {
        directory.evidence("state/frozen-inputs.json", &inputs)?;
        saved.frozen_inputs = Some(inputs);
        saved.input_artifacts = artifacts;
        directory.save(saved)?;
    }
    for (name, binding) in &saved.bindings {
        let receipt = transfers::resume(client, &directory.root.join(upload_path(binding)))?;
        let id = receipt["artifact_id"]
            .as_str()
            .ok_or_else(|| invalid("Upload completion omitted its artifact identity."))?;
        let id = canonical_id(id)?;
        let operation: UploadOperation = directory.read(&upload_path(binding))?;
        if operation
            .reservation
            .as_ref()
            .is_none_or(|reservation| reservation.artifact_id != id)
        {
            return Err(invalid(
                "Upload completion conflicts with its saved reservation.",
            ));
        }
        let artifact = get(client, &["artifacts", &id.to_string()])?;
        validate_artifact(
            &artifact,
            id,
            models::files::declarations(&definition)
                .map_err(|error| invalid(error.to_string()))?
                .get(name)
                .ok_or_else(|| invalid("Saved file declaration is missing."))?,
        )?;
        if artifact["content"]["size_bytes"] != binding.operation.request.expected_size_bytes
            || artifact["content"]["checksum"]["value"] != binding.operation.request.checksum.value
        {
            return Err(invalid(
                "Uploaded artifact does not match the original source evidence.",
            ));
        }
        directory.evidence(
            &format!("state/uploads/{}.receipt.json", binding.id),
            &receipt,
        )?;
        saved.uploads.insert(name.clone(), receipt);
        // Journal each completed file before proceeding to the next upload.
        directory.save(saved)?;
    }
    for (name, binding) in &saved.collection_bindings {
        let receipt = crate::collections::resume(client, &directory.root.join(&binding.operation))
            .map_err(|error| {
                if matches!(&error, Error::Io(io) if io.kind() == std::io::ErrorKind::NotFound) {
                    invalid(format!("Collection input {name} needs its unchanged original source files to finish uploading. Inspect the retained collection operation at {}; restore missing sources at their saved paths and retry this directory. Completed artifacts and collections keep their original identities.", directory.root.join(&binding.operation).display()))
                } else { error }
            })?;
        saved.collection_uploads.insert(name.clone(), receipt);
    }
    directory.save(saved)?;
    let effective = effective_inputs(saved)?;
    models::validate_inputs(&definition, &effective).map_err(|error| invalid(error.to_string()))?;
    let prepared = PreparedRun {
        schema_version: 1,
        api_url: saved.api_url.clone(),
        user_id: saved.user_id,
        idempotency_key: saved.run_key.clone(),
        request: RunSubmission {
            publication_delegation_id: saved.publication_delegation_id,
            purpose: saved.purpose,
            model_id: saved.model_id.clone(),
            expected_publication_id: saved.publication_id.clone(),
            inputs: effective,
        },
    };
    directory.evidence("state/prepared-run.json", &prepared)?;
    saved.prepared = Some(prepared);
    saved.submission_started = true;
    directory.save(saved)?;
    accept(client, directory, saved)?;
    local_summary(directory, saved, None)
}

fn accept(client: &EnumerayClient, directory: &Directory, saved: &mut Saved) -> Result<(), Error> {
    let receipt = client.submit(
        saved
            .prepared
            .as_ref()
            .ok_or_else(|| invalid("Missing saved submission; retain this directory."))?,
    )?;
    validate_receipt(saved, &receipt)?;
    directory.evidence("state/receipt.json", &receipt)?;
    saved.receipt = Some(receipt);
    directory.save(saved)
}

fn canonical_id(text: &str) -> Result<Uuid, Error> {
    let id = Uuid::parse_str(text).map_err(|_| invalid("Artifact identity is invalid."))?;
    if id.is_nil() || id.to_string() != text {
        return Err(invalid(
            "Artifact identity must be a canonical nonempty UUID.",
        ));
    }
    Ok(id)
}

fn validate_artifact(
    artifact: &Value,
    id: Uuid,
    declaration: &models::files::FileDeclaration,
) -> Result<(), Error> {
    if artifact["artifact_id"] != id.to_string()
        || artifact["availability"] != "ready"
        || artifact["content"]["size_bytes"]
            .as_u64()
            .is_none_or(|size| size > declaration.max_bytes)
        || artifact["description"]["media_type"]
            .as_str()
            .is_none_or(|media| {
                !declaration
                    .media_types
                    .iter()
                    .any(|allowed| allowed == media)
            })
        || artifact["content"]["version_id"]
            .as_str()
            .is_none_or(str::is_empty)
        || artifact["content"]["checksum"]["algorithm"] != "sha256"
        || artifact["content"]["checksum"]["checksum_type"] != "full_object"
        || artifact["content"]["checksum"]["value"]
            .as_str()
            .is_none_or(str::is_empty)
    {
        return Err(invalid(
            "Input artifact is unavailable or does not match the declared file slot.",
        ));
    }
    Ok(())
}

fn get(client: &EnumerayClient, segments: &[&str]) -> Result<Value, Error> {
    client.get(&resource_path(segments)?)
}
fn accepted_id(saved: &AnalysisState) -> Result<String, Error> {
    saved.run_id().ok_or_else(|| invalid("Analysis has no accepted run identity; use analysis submit DIR to submit or reconcile the same operation, or repeat analysis attach for an interrupted attachment."))
}

fn validate_run(saved: &AnalysisState, run: &Value) -> Result<(), Error> {
    if run["run_id"] != accepted_id(saved)?
        || run["model_id"] != saved.model_id()
        || run["publication_id"] != saved.publication_id()
        || saved
            .creator_id()
            .is_some_and(|id| run["created_by_user_id"] != id.to_string())
        || run_purpose(run)? != saved.purpose()
        || run_delegation(run)? != saved.publication_delegation_id()
        || !enumeray_contracts::numeric::equivalent(
            &run["requested_inputs"],
            &saved.requested_inputs()?,
        )
        || !enumeray_contracts::numeric::equivalent(
            &run["effective_inputs"],
            &saved.effective_inputs()?,
        )
    {
        return Err(invalid(
            "Run inspection conflicts with the saved analysis identity or inputs.",
        ));
    }
    Ok(())
}

fn run_purpose(run: &Value) -> Result<enumeray_client::RunPurpose, Error> {
    run.get("purpose")
        .map(|purpose| serde_json::from_value(purpose.clone()).map_err(Into::into))
        .unwrap_or_else(|| Ok(Default::default()))
}

fn run_delegation(run: &Value) -> Result<Option<Uuid>, Error> {
    run.get("publication_delegation_id")
        .map(|delegation| serde_json::from_value(delegation.clone()).map_err(Into::into))
        .unwrap_or(Ok(None))
}

fn add_identity(saved: &AnalysisState, value: &mut Value) {
    if let Some(identity) = saved.identity().as_object() {
        for (key, field) in identity {
            value[key] = field.clone();
        }
    }
}

fn summary(
    directory: &Directory,
    saved: &AnalysisState,
    run: Option<Value>,
) -> Result<Value, Error> {
    match saved {
        AnalysisState::Local(local) => local_summary(directory, local, run),
        AnalysisState::Recovery(recovered) => recovery::summary(directory, recovered, run),
    }
}

fn validated_summary(value: Value) -> Result<Value, Error> {
    let summary: AnalysisSummary = serde_json::from_value(value)?;
    summary.validate().map_err(invalid)?;
    Ok(serde_json::to_value(summary)?)
}

fn local_summary(directory: &Directory, saved: &Saved, run: Option<Value>) -> Result<Value, Error> {
    let receipt = saved
        .receipt
        .as_ref()
        .map(|receipt| serde_json::from_value(serde_json::to_value(receipt)?))
        .transpose()?;
    let summary = AnalysisSummary {
        schema_version: SchemaVersion,
        directory: directory.root.to_string_lossy().into_owned(),
        identity: AnalysisIdentity {
            model_id: saved.model_id.clone(), publication_id: saved.publication_id.clone(), purpose: saved.purpose,
            grant_id: serde_json::from_value(saved.publication["grant_id"].clone())?,
            publication_delegation_id: saved.publication_delegation_id,
            context_id: serde_json::from_value(saved.publication["context_id"].clone())?,
            installation_status: serde_json::from_value(saved.publication["status"].clone())?,
            installation: serde_json::from_value(saved.publication["descriptor"].clone())?,
            source_evidence: Default::default(),
        },
        stage: if saved.receipt.is_some() { AnalysisStage::Accepted } else if saved.submission_started { AnalysisStage::SubmissionUncertain } else if saved.frozen_inputs.is_some() { AnalysisStage::Uploading } else { AnalysisStage::Editable },
        run_id: saved.receipt.as_ref().map(|r| r.run_id),
        execution_location: serde_json::from_value(saved.publication["execution_location"].clone())?,
        disclosure: serde_json::from_value(saved.publication["disclosure"].clone())?,
        input_collections: saved.collection_bindings.keys().map(|name| InputCollectionProgress {slot:name.clone(),completed:saved.collection_uploads.contains_key(name)}).collect(),
        input_files: saved.bindings.iter().map(|(name, binding)| InputFileProgress {slot:name.clone(),source:binding.operation.source.to_string_lossy().into_owned(),size_bytes:binding.operation.request.expected_size_bytes,media_type:binding.operation.request.media_type.clone(),completed:saved.uploads.contains_key(name)}).collect(),
        receipt, run: run.map(serde_json::from_value).transpose()?, collection: None, cancellation: None,
        next_action: if saved.receipt.is_some() {"Observe with analysis status/wait DIR; collect terminal evidence with analysis collect DIR."} else if saved.submission_started {"Repeat analysis submit DIR to reconcile the exact saved submission."} else if saved.frozen_inputs.is_some() {"Repeat analysis submit DIR to resume the frozen operation."} else {"Edit inputs.json as needed, review the destination/disclosure, then explicitly authorize execution with analysis submit DIR."}.into(),
    };
    summary.validate().map_err(invalid)?;
    Ok(serde_json::to_value(summary)?)
}
