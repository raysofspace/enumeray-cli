//! Restartable collection, pinned to terminal provenance rather than catalogue discovery.
use super::{
    accepted_id, add_identity, canonical_id, get, run_delegation, run_purpose,
    state::{AnalysisState, Directory, invalid},
    validate_run,
};
use crate::{config, transfers};
use enumeray_client::{EnumerayClient, Error, resource_path};
use enumeray_contracts::{
    automation::{AnalysisCollectionSummary, CollectedFile, SchemaVersion},
    models,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs::File};
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Output {
    artifact_id: Uuid,
    logical_name: String,
    media_type: String,
    version_id: String,
    size_bytes: u64,
    checksum: Value,
    verified: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    schema_version: u32,
    run_id: String,
    publication_id: String,
    inspection: Value,
    provenance: Value,
    outputs: BTreeMap<String, Output>,
    complete: bool,
}

pub(super) fn status(directory: &Directory, saved: &AnalysisState) -> Result<Value, Error> {
    if !directory.exists("state/collection.json")? {
        return Ok(Value::Null);
    }
    let plan: Plan = directory.read("state/collection.json")?;
    if plan.schema_version != 1
        || plan.run_id != accepted_id(saved)?
        || plan.publication_id != saved.publication_id()
        || (plan.complete && plan.outputs.values().any(|output| !output.verified))
    {
        return Err(invalid(
            "Collection progress has an unsupported or inconsistent identity/state.",
        ));
    }
    Ok(
        json!({"complete":plan.complete,"verified_files":plan.outputs.values().filter(|output| output.verified).count(),"total_files":plan.outputs.len()}),
    )
}

pub(super) fn collect(
    client: &EnumerayClient,
    directory: &Directory,
    saved: &AnalysisState,
    members: &[String],
    evidence_only: bool,
) -> Result<Value, Error> {
    let run_id = accepted_id(saved)?;
    let mut plan: Plan = if directory.exists("state/collection.json")? {
        directory.read("state/collection.json")?
    } else {
        let inspection = get(client, &["runs", &run_id])?;
        validate_run(saved, &inspection)?;
        if !matches!(
            inspection["state"].as_str(),
            Some("succeeded" | "failed" | "cancelled")
        ) {
            return Err(invalid(
                "Analysis run is still active; use analysis wait DIR before collecting terminal evidence.",
            ));
        }
        let provenance = get(client, &["runs", &run_id, "provenance"])?;
        let mut plan = Plan {
            schema_version: 1,
            run_id,
            publication_id: saved.publication_id().to_owned(),
            inspection,
            provenance,
            outputs: BTreeMap::new(),
            complete: false,
        };
        plan.outputs = outputs(saved, &plan)?;
        directory.write("state/collection.json", &plan)?;
        plan
    };
    if plan.schema_version != 1
        || plan.run_id != accepted_id(saved)?
        || plan.publication_id != saved.publication_id()
    {
        return Err(invalid(
            "Collection belongs to another run or uses an unsupported format.",
        ));
    }
    validate_run(saved, &plan.inspection)?;
    let expected = outputs(saved, &plan)?;
    if plan.outputs.len() != expected.len()
        || plan.outputs.iter().any(|(name, output)| {
            expected.get(name).is_none_or(|retained| {
                output.artifact_id != retained.artifact_id
                    || output.logical_name != retained.logical_name
                    || output.media_type != retained.media_type
                    || output.version_id != retained.version_id
                    || output.size_bytes != retained.size_bytes
                    || output.checksum != retained.checksum
            })
        })
    {
        return Err(invalid(
            "Collection plan conflicts with retained provenance.",
        ));
    }
    directory.evidence("results/inspection.json", &plan.inspection)?;
    directory.evidence("results/provenance.json", &plan.provenance)?;
    directory.evidence(
        "results/effective-inputs.json",
        &plan.inspection["effective_inputs"],
    )?;
    directory.evidence(
        "results/outputs.json",
        &plan.inspection["result"]["outputs"],
    )?;
    let mut input_collections = BTreeMap::new();
    if let Some(inputs) = plan.provenance["input_collections"].as_array() {
        if !inputs.is_empty() {
            crate::collections::private_dir(&directory.root.join("results/input-collections"))?;
        }
        for input in inputs {
            let id = canonical_id(
                input["collection_id"]
                    .as_str()
                    .ok_or_else(|| invalid("Missing input collection identity."))?,
            )?;
            let evidence = crate::collections::download(
                client,
                id,
                &directory
                    .root
                    .join("results/input-collections")
                    .join(id.to_string()),
                &[],
                true,
            )?;
            if evidence["manifest_digest"] != input["manifest_digest"] {
                return Err(invalid(
                    "Input collection conflicts with accepted provenance.",
                ));
            }
            input_collections.insert(input["name"].as_str().ok_or_else(||invalid("Missing collection slot."))?.to_owned(),json!({"collection_id":id,"manifest_digest":input["manifest_digest"],"manifest":format!("results/input-collections/{id}/manifest.json")}));
        }
    }
    // Recheck even previously completed files. A verified flag never substitutes for bytes.
    plan.complete = false;
    for output in plan.outputs.values_mut() {
        output.verified = false;
    }
    directory.write("state/collection.json", &plan)?;
    for name in plan
        .outputs
        .keys()
        .filter(|_| !evidence_only && members.is_empty())
        .cloned()
        .collect::<Vec<_>>()
    {
        let output = &plan.outputs[&name];
        let relative = format!("results/files/{}", output.artifact_id);
        let checksum = output.checksum["value"]
            .as_str()
            .ok_or_else(|| invalid("Missing artifact checksum."))?
            .to_owned();
        if !directory.exists(&relative)? {
            let artifact = get(client, &["artifacts", &output.artifact_id.to_string()])?;
            if artifact["artifact_id"] != output.artifact_id.to_string()
                || artifact["availability"] != "ready"
                || artifact["description"]["media_type"] != output.media_type
                || artifact["content"]["version_id"] != output.version_id
                || artifact["content"]["size_bytes"] != output.size_bytes
                || artifact["content"]["checksum"] != output.checksum
            {
                return Err(invalid(
                    "Output artifact is unavailable or conflicts with the pinned collection evidence; retry this directory after recovery.",
                ));
            }
            transfers::download(
                client,
                &resource_path(&["artifacts", &output.artifact_id.to_string(), "content"])?,
                &directory.root.join(&relative),
                Some((
                    output.size_bytes,
                    checksum.clone(),
                    output.version_id.clone(),
                )),
            )?;
        }
        config::require_private(&directory.root.join(&relative))?;
        let (size, actual) = transfers::checksum(&mut File::open(directory.root.join(&relative))?)?;
        if size != output.size_bytes || actual != checksum {
            return Err(invalid(format!(
                "Existing result file conflicts with verified evidence at {relative}; preserve or move it before retrying collection."
            )));
        }
        plan.outputs.get_mut(&name).expect("retained name").verified = true;
        directory.write("state/collection.json", &plan)?;
    }
    let mut selections: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for member in members {
        let (slot, path) = member
            .split_once('=')
            .ok_or_else(|| invalid("Select collection members with SLOT=PATH."))?;
        selections.entry(slot.into()).or_default().push(path.into());
    }
    let references = plan.inspection["result"]["collections"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    if selections.keys().any(|name| !references.contains_key(name)) {
        return Err(invalid(
            "Member selection targets an absent output collection.",
        ));
    }
    let mut collection_outputs = BTreeMap::new();
    let mut all_collections = true;
    if !references.is_empty() {
        crate::collections::private_dir(&directory.root.join("results/collections"))?;
    }
    for (name, reference) in references {
        let id = enumeray_contracts::collections::reference(&reference)
            .ok_or_else(|| invalid("Invalid result collection reference."))?;
        let expected = plan.provenance["output_collections"]
            .as_array()
            .and_then(|a| a.iter().find(|c| c["name"] == name))
            .ok_or_else(|| invalid("Provenance omitted the collection pin."))?;
        let skip = evidence_only || (!selections.is_empty() && !selections.contains_key(&name));
        let progress = crate::collections::download(
            client,
            id,
            &directory
                .root
                .join("results/collections")
                .join(id.to_string()),
            selections.get(&name).map(Vec::as_slice).unwrap_or(&[]),
            skip,
        )?;
        if progress["manifest_digest"] != expected["manifest_digest"]
            || expected["collection_id"] != id.to_string()
        {
            return Err(invalid(
                "Collection conflicts with pinned terminal provenance.",
            ));
        }
        all_collections &= progress["complete"] == true;
        collection_outputs.insert(name,json!({"collection_id":id,"manifest_digest":progress["manifest_digest"],"manifest":format!("results/collections/{id}/manifest.json"),"root":format!("results/collections/{id}/files"),"complete":progress["complete"]}));
    }
    if !all_collections || plan.outputs.values().any(|o| !o.verified) {
        directory.write("state/collection.json", &plan)?;
        return collection_summary(directory, saved, &plan, collection_outputs, false);
    }
    let bundle = json!({"schema_version":1,"complete":true,"run_id":plan.run_id,"publication_id":plan.publication_id,
        "scientific_state":plan.inspection["state"],"inspection":"results/inspection.json","provenance":"results/provenance.json",
        "effective_inputs":"results/effective-inputs.json","scientific_outputs":"results/outputs.json",
        "optional_output_failures":plan.inspection["result"]["optional_output_failures"],
        "original_input_files":saved.local().into_iter().flat_map(|local| local.bindings.iter()).map(|(name, binding)| json!({"slot":name,"source":binding.operation.source,"size_bytes":binding.operation.request.expected_size_bytes,"checksum":binding.operation.request.checksum})).collect::<Vec<_>>(),
        "outputs":plan.outputs.iter().map(|(name, output)| (name.clone(),json!({"artifact_id":output.artifact_id,"logical_name":output.logical_name,"media_type":output.media_type,"version_id":output.version_id,"size_bytes":output.size_bytes,"checksum":output.checksum,"path":format!("results/files/{}",output.artifact_id)}))).collect::<BTreeMap<_,_>>()});
    let mut bundle = bundle;
    // Existing standard bundles are immutable retained evidence. Only trials
    // introduce additional persisted authority fields in this version-1 format.
    if !saved.purpose().is_standard() || saved.local().is_none() {
        add_identity(saved, &mut bundle);
    }
    if !input_collections.is_empty() {
        bundle["input_collections"] = serde_json::to_value(&input_collections)?;
    }
    if !collection_outputs.is_empty() {
        bundle["collections"] = serde_json::to_value(&collection_outputs)?;
    }
    directory.evidence("bundle.json", &bundle)?;
    plan.complete = true;
    directory.write("state/collection.json", &plan)?;
    collection_summary(directory, saved, &plan, collection_outputs, true)
}

fn collection_summary(
    directory: &Directory,
    saved: &AnalysisState,
    plan: &Plan,
    collections: BTreeMap<String, Value>,
    complete: bool,
) -> Result<Value, Error> {
    let outputs = plan
        .outputs
        .iter()
        .filter(|(_, output)| output.verified)
        .map(|(name, output)| {
            Ok((
                name.clone(),
                CollectedFile {
                    artifact_id: output.artifact_id,
                    logical_name: output.logical_name.clone(),
                    media_type: output.media_type.clone(),
                    version_id: output.version_id.clone(),
                    size_bytes: output.size_bytes,
                    checksum: serde_json::from_value(output.checksum.clone())?,
                    path: format!("results/files/{}", output.artifact_id),
                },
            ))
        })
        .collect::<Result<BTreeMap<_, _>, Error>>()?;
    let summary = AnalysisCollectionSummary {
        schema_version: SchemaVersion,
        directory: directory.root.to_string_lossy().into_owned(),
        identity: serde_json::from_value(saved.identity())?,
        run_id: canonical_id(&plan.run_id)?,
        complete,
        bundle: complete.then(|| {
            directory
                .root
                .join("bundle.json")
                .to_string_lossy()
                .into_owned()
        }),
        scientific_state: serde_json::from_value(plan.inspection["state"].clone())?,
        outputs,
        collections: collections
            .into_iter()
            .map(|(name, collection)| Ok((name, serde_json::from_value(collection)?)))
            .collect::<Result<_, Error>>()?,
    };
    summary.validate().map_err(invalid)?;
    Ok(serde_json::to_value(summary)?)
}

fn outputs(saved: &AnalysisState, plan: &Plan) -> Result<BTreeMap<String, Output>, Error> {
    let provenance = &plan.provenance;
    if let AnalysisState::Recovery(recovered) = saved {
        super::recovery::validate_provenance(recovered, provenance)?;
    }
    if !matches!(
        plan.inspection["state"].as_str(),
        Some("succeeded" | "failed" | "cancelled")
    ) || provenance["format_version"] != 1
        || provenance["run"]["run_id"] != plan.run_id
        || provenance["run"]["model_id"] != saved.model_id()
        || provenance["run"]["created_by_user_id"]
            != saved
                .creator_id()
                .ok_or_else(|| invalid("Missing accepted run creator."))?
                .to_string()
        || run_purpose(&provenance["run"])? != saved.purpose()
        || run_delegation(&provenance["run"])? != saved.publication_delegation_id()
        || provenance["run"]["publication_id"] != saved.publication_id()
        || provenance["publication"]["publication_id"] != saved.publication_id()
        || !enumeray_contracts::numeric::equivalent(
            &provenance["run"]["requested_inputs"],
            &saved.requested_inputs()?,
        )
        || !enumeray_contracts::numeric::equivalent(
            &provenance["run"]["effective_inputs"],
            &saved.effective_inputs()?,
        )
        || provenance["run"]["state"] != plan.inspection["state"]
        || provenance["result"] != plan.inspection["result"]
        || provenance["definition"] != saved.definition_value()?
        || (!saved.purpose().is_standard()
            && provenance["publication"]["compute_grant_id"] != saved.identity()["grant_id"])
    {
        return Err(invalid(
            "Terminal provenance conflicts with the saved analysis or inspection.",
        ));
    }
    let declarations = models::files::output_declarations(&saved.definition()?)
        .map_err(|error| invalid(error.to_string()))?;
    let artifacts = provenance["output_artifacts"]
        .as_array()
        .ok_or_else(|| invalid("Provenance omitted output artifact evidence."))?;
    let mut result = BTreeMap::new();
    for artifact in artifacts {
        let name = artifact["name"]
            .as_str()
            .ok_or_else(|| invalid("Output evidence omitted its declared name."))?;
        let declaration = declarations
            .get(name)
            .ok_or_else(|| invalid("Provenance contains an undeclared output file."))?;
        let id = canonical_id(
            artifact["artifact_id"]
                .as_str()
                .ok_or_else(|| invalid("Output omitted its artifact identity."))?,
        )?;
        if plan.inspection["result"]["files"][name]["artifact_id"] != id.to_string()
            || plan.inspection["result"]["outputs"][name]["artifact_id"] != id.to_string()
        {
            return Err(invalid(
                "Output reference conflicts with its provenance artifact.",
            ));
        }
        let size_bytes = artifact["size_bytes"]
            .as_u64()
            .filter(|size| *size <= declaration.max_bytes)
            .ok_or_else(|| invalid("Output size violates its declaration."))?;
        let media_type = artifact["media_type"]
            .as_str()
            .filter(|media| {
                declaration
                    .media_types
                    .iter()
                    .any(|allowed| allowed == media)
            })
            .ok_or_else(|| invalid("Output media type violates its declaration."))?
            .to_owned();
        if artifact["checksum"]["algorithm"] != "sha256"
            || artifact["checksum"]["checksum_type"] != "full_object"
            || artifact["checksum"]["value"]
                .as_str()
                .is_none_or(str::is_empty)
        {
            return Err(invalid("Output omitted verified SHA-256 evidence."));
        }
        let output = Output {
            artifact_id: id,
            logical_name: artifact["logical_name"]
                .as_str()
                .ok_or_else(|| invalid("Missing output logical name."))?
                .into(),
            media_type,
            version_id: artifact["version_id"]
                .as_str()
                .filter(|version| !version.is_empty())
                .ok_or_else(|| invalid("Missing immutable output version."))?
                .into(),
            size_bytes,
            checksum: artifact["checksum"].clone(),
            verified: false,
        };
        if result.insert(name.into(), output).is_some() {
            return Err(invalid("Duplicate declared output in provenance."));
        }
    }
    let result_files = &plan.inspection["result"]["files"];
    if result_files
        .as_object()
        .is_some_and(|files| files.len() != result.len())
    {
        return Err(invalid("Provenance is missing result file evidence."));
    }
    if plan.inspection["result"].is_object() && !result_files.is_object() {
        return Err(invalid("Terminal result omitted its file manifest."));
    }
    for name in declarations.keys() {
        if plan.inspection["result"]["outputs"].get(name).is_some() && !result.contains_key(name) {
            return Err(invalid(
                "Declared file output is missing verified provenance evidence.",
            ));
        }
    }
    if plan.inspection["state"] == "succeeded" {
        let scientific = saved.definition()?;
        let validator =
            models::compile_outputs(&scientific).map_err(|error| invalid(error.to_string()))?;
        if !validator.is_valid(&plan.inspection["result"]["outputs"]) {
            return Err(invalid(
                "Successful result does not satisfy its retained output contract.",
            ));
        }
    }
    Ok(result)
}
