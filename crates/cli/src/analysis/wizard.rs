//! Inspect and submit one native analysis prepared from a model setup example.

use super::{
    collect, definition, get,
    state::{AnalysisState, Directory, Saved, invalid},
    submit, validate_run, validate_template,
};
use enumeray_client::{EnumerayClient, Error, RunPurpose};
use enumeray_contracts::{
    automation::SchemaVersion,
    collections,
    models::{self, ScientificExample},
    numeric::equivalent,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;
use uuid::Uuid;

fn load_trial(
    directory: &Directory,
    api: &str,
    user: Uuid,
    model: &str,
    candidate: &str,
) -> Result<Saved, Error> {
    if directory.exists("recovery.json")? {
        return Err(invalid(
            "Model setup needs its native trial directory. Use analysis commands for an attached or imported run.",
        ));
    }
    let AnalysisState::Local(saved) = directory.load(api, user)? else {
        return Err(invalid("Model setup needs its native trial directory."));
    };
    if saved.model_id != model
        || saved.publication_id != candidate
        || saved.purpose != RunPurpose::CandidateTrial
        || saved.publication_delegation_id.is_some()
    {
        return Err(invalid(
            "Trial must belong to this model and exact candidate and use the author's own compute grant.",
        ));
    }
    let definition = definition(&saved)?;
    if !saved.collection_bindings.is_empty()
        || !collections::declarations(&definition, false)
            .map_err(|error| invalid(error.to_string()))?
            .is_empty()
        || !collections::declarations(&definition, true)
            .map_err(|error| invalid(error.to_string()))?
            .is_empty()
    {
        return Err(invalid(
            "Model setup handles named examples with JSON values and files. Use analysis commands for collections.",
        ));
    }
    Ok(saved)
}

fn preview(directory: &Directory, saved: &Saved, inputs: &Value) -> Value {
    let input_files = saved
        .bindings
        .iter()
        .map(|(slot, binding)| {
            let upload = &binding.operation;
            json!({
                "slot": slot,
                "source": upload.source.to_string_lossy(),
                "media_type": upload.request.media_type,
                "size_bytes": upload.request.expected_size_bytes,
                "checksum": upload.request.checksum,
                "metadata": upload.request.scientific_metadata,
                "binding_id": binding.id,
                "upload_retry_key": upload.idempotency_key,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "schema_version": SchemaVersion,
        "directory": directory.root.to_string_lossy(),
        "api_url": saved.api_url,
        "user_id": saved.user_id,
        "model_id": saved.model_id,
        "publication_id": saved.publication_id,
        "grant_id": saved.publication["grant_id"],
        "context_id": saved.publication["context_id"],
        "installation": saved.publication["descriptor"],
        "execution_location": saved.publication["execution_location"],
        "disclosure": saved.publication["disclosure"],
        "retry_key": saved.run_key,
        "inputs": inputs,
        "input_files": input_files,
    })
}

fn require_named_inputs(
    saved: &Saved,
    example: &ScientificExample,
    inputs: &Value,
) -> Result<(), Error> {
    let definition = definition(saved)?;
    let frozen = definition
        .examples
        .iter()
        .find(|item| item.name == example.name)
        .ok_or_else(|| invalid("Named example is absent from the saved candidate."))?;
    let same_outputs = match (&frozen.outputs, &example.outputs) {
        (Some(frozen), Some(expected)) => equivalent(frozen, expected),
        (None, None) => true,
        _ => false,
    };
    if !equivalent(&frozen.inputs, &example.inputs) || !same_outputs {
        return Err(invalid(
            "Named example conflicts with the saved candidate; inspect the release and trial directories.",
        ));
    }
    if !equivalent(&frozen.inputs, inputs) {
        return Err(invalid(
            "Trial inputs differ from the named example. Restore the example in inputs.json before reviewing or create a new trial explicitly.",
        ));
    }
    let files = models::files::references(&definition, inputs)
        .map_err(|error| invalid(error.to_string()))?;
    if files.len() != saved.bindings.len()
        || files
            .iter()
            .any(|file| !saved.bindings.contains_key(&file.input_name))
    {
        return Err(invalid(
            "Model setup requires a local file binding for each file in the named example. Check the trial directory and its saved bindings.",
        ));
    }
    validate_template(saved, inputs)
}

fn read_review(
    directory: &Directory,
    saved: &Saved,
    example: &ScientificExample,
    approved: Option<&Value>,
) -> Result<(Value, Value), Error> {
    let inputs = if saved.submission_started {
        saved
            .frozen_inputs
            .clone()
            .ok_or_else(|| invalid("Dispatched trial omitted its frozen inputs."))?
    } else {
        directory.inputs()?
    };
    let mut observed = preview(directory, saved, &inputs);
    observed["example"] = json!(example.name);
    if approved.is_some_and(|approved| *approved != observed) {
        return Err(invalid(
            "Trial approval is stale. Inspect this trial and review its current inputs and destination again.",
        ));
    }
    require_named_inputs(saved, example, &inputs)?;
    Ok((inputs, observed))
}

pub(crate) fn wizard_preview(
    client: &EnumerayClient,
    path: &Path,
    model: &str,
    candidate: &str,
    example: &ScientificExample,
) -> Result<Value, Error> {
    let directory = Directory::open(path)?;
    let user = client.me()?.user_id;
    let _snapshot = directory.state_lock()?;
    let saved = load_trial(&directory, client.base_url(), user, model, candidate)?;
    let (_, review) = read_review(&directory, &saved, example, None)?;
    Ok(review)
}

pub(crate) fn wizard_submit(
    client: &EnumerayClient,
    path: &Path,
    model: &str,
    candidate: &str,
    example: &ScientificExample,
    approved: &Value,
) -> Result<Value, Error> {
    let mut directory = Directory::open(path)?;
    directory.claim_writer()?;
    let user = client.me()?.user_id;
    let (mut saved, inputs) = {
        let _snapshot = directory.state_lock()?;
        let saved = load_trial(&directory, client.base_url(), user, model, candidate)?;
        let (inputs, _) = read_review(&directory, &saved, example, Some(approved))?;
        (saved, inputs)
    };
    submit(client, &directory, &mut saved, Some(inputs))
}

pub(crate) fn wizard_status(
    client: &EnumerayClient,
    path: &Path,
    model: &str,
    candidate: &str,
) -> Result<Value, Error> {
    let directory = Directory::open(path)?;
    let user = client.me()?.user_id;
    let (saved, collection, evidence) = {
        let _snapshot = directory.read_snapshot()?;
        let saved = AnalysisState::Local(load_trial(
            &directory,
            client.base_url(),
            user,
            model,
            candidate,
        )?);
        let collection = collect::status(&directory, &saved)?;
        let evidence = if collection["complete"] == true {
            Some(collect::wizard_evidence(&directory, &saved)?)
        } else {
            None
        };
        (saved, collection, evidence)
    };
    let run = saved
        .run_id()
        .map(|id| get(client, &["runs", &id]))
        .transpose()?;
    if let Some(run) = &run {
        validate_run(&saved, run)?;
        if let Some(evidence) = &evidence
            && (run["state"] != evidence.inspection["state"]
                || !equivalent(&run["result"], &evidence.inspection["result"]))
        {
            return Err(invalid(
                "Collected trial result conflicts with the accepted run. Preserve the directory and inspect its evidence.",
            ));
        }
    }
    let mut summary = super::summary(&directory, &saved, run)?;
    summary["collection"] = collection;
    summary = super::validated_summary(summary)?;
    let local = saved.local().expect("native trial");
    summary["retry_key"] = json!(local.run_key);
    summary["submission_started"] = json!(local.submission_started);
    summary["frozen_review"] = local
        .frozen_inputs
        .as_ref()
        .map_or(Value::Null, |inputs| preview(&directory, local, inputs));
    summary["bundle"] = json!(evidence.as_ref().map(|_| {
        directory
            .root
            .join("bundle.json")
            .to_string_lossy()
            .into_owned()
    }));
    summary["bundle_sha256"] = match evidence {
        Some(evidence) => {
            let digest: String = Sha256::digest(serde_json::to_vec(&evidence.bundle)?)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            json!(digest)
        }
        None => Value::Null,
    };
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::{StarterDriver, StarterIo, StarterKind};
    use std::collections::BTreeMap;

    fn saved(root: &Path, io: StarterIo) -> (Saved, ScientificExample) {
        let source = root.join("source");
        crate::starter::init(
            "test-model",
            &source,
            StarterKind::Python,
            io,
            StarterDriver::Process,
        )
        .unwrap();
        let (_, source) =
            crate::manifest::publication_source(&source.join("publication.json")).unwrap();
        let example = source.definition.examples[0].clone();
        (
            Saved {
                schema_version: 1,
                api_url: "https://example.test".into(),
                user_id: Uuid::new_v4(),
                model_id: "test-model".into(),
                publication_id: "test-candidate".into(),
                purpose: RunPurpose::CandidateTrial,
                publication_delegation_id: None,
                publication: json!({
                    "definition": source.definition,
                    "execution_location": "test-site",
                    "disclosure": "test disclosure",
                    "grant_id": Uuid::new_v4(),
                    "context_id": "test-context",
                }),
                bindings: BTreeMap::new(),
                collection_bindings: BTreeMap::new(),
                collection_uploads: BTreeMap::new(),
                run_key: Uuid::new_v4().to_string(),
                frozen_inputs: None,
                input_artifacts: BTreeMap::new(),
                uploads: BTreeMap::new(),
                prepared: None,
                submission_started: false,
                receipt: None,
            },
            example,
        )
    }

    #[test]
    fn review_captures_named_inputs_and_detects_an_edit_or_a_new_retry_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let directory = Directory::create(&tmp.path().join("trial")).unwrap();
        let (mut saved, example) = saved(tmp.path(), StarterIo::Json);
        directory.write_inputs(&example.inputs).unwrap();
        let (inputs, approval) = read_review(&directory, &saved, &example, None).unwrap();
        assert_eq!(inputs, example.inputs);
        assert_eq!(approval["retry_key"], saved.run_key);
        saved.frozen_inputs = Some(inputs.clone());
        let (captured, _) = read_review(&directory, &saved, &example, Some(&approval)).unwrap();

        crate::config::write_private_bytes(
            &directory.root.join("inputs.json"),
            br#"{"value":3}"#,
            true,
        )
        .unwrap();
        assert_eq!(captured, example.inputs);
        let mut frozen = preview(&directory, &saved, saved.frozen_inputs.as_ref().unwrap());
        frozen["example"] = json!(example.name);
        assert_eq!(frozen, approval);
        assert!(
            matches!(read_review(&directory, &saved, &example, Some(&approval)), Err(Error::Invalid(s)) if s.contains("approval is stale"))
        );
        assert!(
            matches!(read_review(&directory, &saved, &example, None), Err(Error::Invalid(s)) if s.contains("differ from the named example"))
        );

        saved.submission_started = true;
        let (retained, review) =
            read_review(&directory, &saved, &example, Some(&approval)).unwrap();
        assert_eq!(retained, example.inputs);
        assert_eq!(review, approval);
        saved.submission_started = false;

        crate::config::write_private_bytes(
            &directory.root.join("inputs.json"),
            br#"{"value":2}"#,
            true,
        )
        .unwrap();
        saved.run_key = Uuid::new_v4().to_string();
        assert!(
            matches!(read_review(&directory, &saved, &example, Some(&approval)), Err(Error::Invalid(s)) if s.contains("approval is stale"))
        );
    }

    #[test]
    fn reference_accepts_server_normalization_of_expected_numbers() {
        let tmp = tempfile::tempdir().unwrap();
        let (saved, mut example) = saved(tmp.path(), StarterIo::Json);
        example.outputs = Some(json!({"doubled": 4.0}));
        require_named_inputs(&saved, &example, &example.inputs).unwrap();
        example.outputs = Some(json!({"doubled": 4.01}));
        assert!(require_named_inputs(&saved, &example, &example.inputs).is_err());
    }

    #[test]
    fn named_file_references_need_real_local_bindings() {
        let tmp = tempfile::tempdir().unwrap();
        let (saved, example) = saved(tmp.path(), StarterIo::Files);
        assert!(
            matches!(require_named_inputs(&saved, &example, &example.inputs), Err(Error::Invalid(s)) if s.contains("requires a local file binding"))
        );
    }
}
