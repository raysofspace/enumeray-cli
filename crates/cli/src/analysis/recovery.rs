//! Local recovery preserves original request identity; only explicit submit can POST.
use super::state::{Directory, invalid, private_dir};
use crate::{args::Args, config};
use enumeray_client::{
    EnumerayClient, Error, PreparedRun, RunPurpose, SubmissionReceipt, resource_path,
};
use enumeray_contracts::{
    execution::InstallationIdentity,
    models::{self, ScientificDefinition},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json, value::RawValue};
use std::{
    fs::{self, File},
    io::Read,
    path::Path,
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

const JOURNAL: &str = "recovery.json";
const PROVENANCE: &str = "state/accepted-provenance.json";

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Recovery {
    ImportedPrepared {
        schema_version: u32,
        operation: PreparedRun,
        receipt: Option<SubmissionReceipt>,
        accepted: Option<Accepted>,
    },
    AttachedRun {
        schema_version: u32,
        api_url: String,
        user_id: Uuid,
        run_id: Uuid,
        source_receipt: Option<SubmissionReceipt>,
        accepted: Option<Accepted>,
    },
}

/// Facts available from an authorized provenance snapshot, without current
/// publication readiness, release authority, or invented local preparation.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Accepted {
    run_id: Uuid,
    model_id: String,
    publication_id: String,
    created_by_user_id: Uuid,
    purpose: RunPurpose,
    publication_delegation_id: Option<Uuid>,
    contract_version: u32,
    requested_inputs: Value,
    effective_inputs: Value,
    created_at: String,
    publication: Value,
    definition: Value,
    input_artifacts: Vec<Value>,
    input_collections: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    terminal: Option<Terminal>,
}

/// Terminal results and pins are immutable once observed; active snapshots do
/// not make predictions about output evidence that has not arrived yet.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Terminal {
    state: String,
    result: Value,
    output_artifacts: Vec<Value>,
    output_collections: Vec<Value>,
}

impl Recovery {
    fn accepted(&self) -> Option<&Accepted> {
        match self {
            Self::ImportedPrepared { accepted, .. } | Self::AttachedRun { accepted, .. } => {
                accepted.as_ref()
            }
        }
    }

    fn set_accepted(&mut self, evidence: Accepted) {
        match self {
            Self::ImportedPrepared { accepted, .. } | Self::AttachedRun { accepted, .. } => {
                *accepted = Some(evidence)
            }
        }
    }

    pub fn api_url(&self) -> &str {
        match self {
            Self::ImportedPrepared { operation, .. } => &operation.api_url,
            Self::AttachedRun { api_url, .. } => api_url,
        }
    }

    pub fn user_id(&self) -> Uuid {
        match self {
            Self::ImportedPrepared { operation, .. } => operation.user_id,
            Self::AttachedRun { user_id, .. } => *user_id,
        }
    }

    pub fn model_id(&self) -> &str {
        self.accepted()
            .map(|a| a.model_id.as_str())
            .unwrap_or_else(|| match self {
                Self::ImportedPrepared { operation, .. } => &operation.request.model_id,
                Self::AttachedRun { .. } => "",
            })
    }

    pub fn publication_id(&self) -> &str {
        self.accepted()
            .map(|a| a.publication_id.as_str())
            .unwrap_or_else(|| match self {
                Self::ImportedPrepared { operation, .. } => {
                    &operation.request.expected_publication_id
                }
                Self::AttachedRun { .. } => "",
            })
    }

    pub fn purpose(&self) -> RunPurpose {
        self.accepted()
            .map(|a| a.purpose)
            .unwrap_or_else(|| match self {
                Self::ImportedPrepared { operation, .. } => operation.request.purpose,
                Self::AttachedRun { source_receipt, .. } => source_receipt
                    .as_ref()
                    .map(|r| r.purpose)
                    .unwrap_or_default(),
            })
    }

    pub fn publication_delegation_id(&self) -> Option<Uuid> {
        if let Some(accepted) = self.accepted() {
            return accepted.publication_delegation_id;
        }
        match self {
            Self::ImportedPrepared { operation, .. } => operation.request.publication_delegation_id,
            Self::AttachedRun { source_receipt, .. } => source_receipt
                .as_ref()
                .and_then(|r| r.publication_delegation_id),
        }
    }

    pub fn creator_id(&self) -> Option<Uuid> {
        self.accepted()
            .map(|a| a.created_by_user_id)
            .or(match self {
                Self::ImportedPrepared { operation, .. } => Some(operation.user_id),
                Self::AttachedRun { .. } => None,
            })
    }

    pub fn run_id(&self) -> Option<String> {
        match self {
            Self::ImportedPrepared { receipt, .. } => {
                receipt.as_ref().map(|r| r.run_id.to_string())
            }
            Self::AttachedRun { accepted, .. } => accepted.as_ref().map(|a| a.run_id.to_string()),
        }
    }

    fn target_id(&self) -> Option<Uuid> {
        match self {
            Self::ImportedPrepared { receipt, .. } => receipt.as_ref().map(|r| r.run_id),
            Self::AttachedRun { run_id, .. } => Some(*run_id),
        }
    }

    pub fn requested_inputs(&self) -> Result<Value, Error> {
        if let Some(accepted) = self.accepted() {
            return Ok(accepted.requested_inputs.clone());
        }
        match self {
            Self::ImportedPrepared { operation, .. } => Ok(operation.request.inputs.clone()),
            Self::AttachedRun { .. } => Err(invalid(
                "Attachment is incomplete; repeat analysis attach with this directory to recover accepted evidence.",
            )),
        }
    }

    pub fn effective_inputs(&self) -> Result<Value, Error> {
        self.accepted()
            .map(|a| Ok(a.effective_inputs.clone()))
            .unwrap_or_else(|| self.requested_inputs())
    }

    pub fn definition(&self) -> Result<ScientificDefinition, Error> {
        Ok(serde_json::from_value(self.definition_value()?)?)
    }

    pub fn definition_value(&self) -> Result<Value, Error> {
        self.accepted().map(|a| a.definition.clone()).ok_or_else(|| invalid("Accepted contract evidence is not yet available; retain this directory and repeat an authorized status/collect command after submission, or repeat analysis attach."))
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::ImportedPrepared { .. } => "imported_prepared",
            Self::AttachedRun { .. } => "attached_run",
        }
    }

    fn initial(&self) -> Self {
        let mut initial = self.clone();
        match &mut initial {
            Self::ImportedPrepared {
                receipt, accepted, ..
            } => {
                *receipt = None;
                *accepted = None;
            }
            Self::AttachedRun { accepted, .. } => *accepted = None,
        }
        initial
    }

    pub fn same_source(&self, other: &Self) -> Result<bool, Error> {
        Ok(serde_json::to_value(self.initial())? == serde_json::to_value(other.initial())?)
    }

    pub fn validate(&self) -> Result<(), Error> {
        let schema = match self {
            Self::ImportedPrepared { schema_version, .. }
            | Self::AttachedRun { schema_version, .. } => *schema_version,
        };
        if schema != 1 || self.user_id().is_nil() {
            return Err(invalid(
                "Unsupported or inconsistent analysis recovery format.",
            ));
        }
        validate_api(self.api_url())?;
        match self {
            Self::ImportedPrepared {
                operation,
                receipt,
                accepted,
                ..
            } => {
                validate_prepared(operation)?;
                if let Some(receipt) = receipt {
                    validate_operation_receipt(operation, receipt)?;
                }
                if accepted.is_some() && receipt.is_none() {
                    return Err(invalid(
                        "Imported accepted evidence has no acceptance receipt.",
                    ));
                }
            }
            Self::AttachedRun {
                run_id,
                source_receipt,
                ..
            } => {
                if run_id.is_nil() {
                    return Err(invalid("Attachment requires a nonempty run ID."));
                }
                if let Some(receipt) = source_receipt {
                    validate_source_receipt(receipt)?;
                    if receipt.run_id != *run_id {
                        return Err(invalid(
                            "Attachment receipt conflicts with the requested run.",
                        ));
                    }
                }
            }
        }
        if let Some(accepted) = self.accepted() {
            validate_accepted(self, accepted)?;
        }
        Ok(())
    }

    pub fn identity(&self) -> Value {
        let publication = self.accepted().map(|a| &a.publication);
        let field = |name: &str| {
            publication
                .and_then(|p| p.get(name))
                .cloned()
                .unwrap_or(Value::Null)
        };
        let has_receipt = match self {
            Self::ImportedPrepared { receipt, .. } => receipt.is_some(),
            Self::AttachedRun { source_receipt, .. } => source_receipt.is_some(),
        };
        json!({"api_url":self.api_url(),"user_id":self.user_id(),"created_by_user_id":self.creator_id(),
            "model_id":self.model_id(),"publication_id":self.publication_id(),"purpose":self.purpose(),
            "grant_id":field("compute_grant_id"),"publication_delegation_id":self.publication_delegation_id(),
            "context_id":field("context_id"),"execution_location":field("execution_location"),"disclosure":field("disclosure"),
            "installation_status":null,"installation":null,"installation_identity":field("installation"),"origin":self.kind(),
            "evidence":{"prepared_operation":if matches!(self,Self::ImportedPrepared {..}) {"retained"} else {"unavailable"},
                "local_sources":"unavailable","source_fingerprints":"unavailable","upload_checkpoints":"unavailable",
                "acceptance_receipt":if has_receipt {"retained"} else {"unavailable"},"accepted_provenance":if self.accepted().is_some() {"retained"} else {"unavailable"},
                "current_publication_authority":"unavailable"}})
    }
}

fn validate_api(api: &str) -> Result<(), Error> {
    let canonical = enumeray_client::validate_url(api)?;
    if canonical.path() != "/" || canonical.query().is_some() || canonical.as_str() != api {
        return Err(invalid(
            "Saved API URL must use its original canonical origin, without a path or query.",
        ));
    }
    Ok(())
}

fn validate_prepared(operation: &PreparedRun) -> Result<(), Error> {
    if operation.schema_version != 1
        || operation.user_id.is_nil()
        || !models::validation::valid_model_id(&operation.request.model_id)
        || !models::validation::valid_model_id(&operation.request.expected_publication_id)
        || operation.idempotency_key.is_empty()
        || operation.idempotency_key.len() > 128
        || !operation
            .idempotency_key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        || (operation.request.purpose.is_standard()
            && operation.request.publication_delegation_id.is_some())
        || operation
            .request
            .publication_delegation_id
            .is_some_and(|id| id.is_nil())
    {
        return Err(invalid(
            "Prepared operation has an unsupported format or invalid original identity.",
        ));
    }
    validate_api(&operation.api_url)?;
    models::validate_input_value(&operation.request.inputs).map_err(|e| invalid(e.to_string()))
}

fn validate_source_receipt(receipt: &SubmissionReceipt) -> Result<(), Error> {
    if receipt.run_id.is_nil()
        || !models::validation::valid_model_id(&receipt.model_id)
        || !models::validation::valid_model_id(&receipt.publication_id)
        || receipt.state != "queued"
        || OffsetDateTime::parse(&receipt.created_at, &Rfc3339).is_err()
        || (receipt.purpose.is_standard() && receipt.publication_delegation_id.is_some())
        || receipt
            .publication_delegation_id
            .is_some_and(|id| id.is_nil())
    {
        return Err(invalid(
            "Saved receipt has an invalid acceptance identity or format.",
        ));
    }
    Ok(())
}

fn validate_operation_receipt(
    operation: &PreparedRun,
    receipt: &SubmissionReceipt,
) -> Result<(), Error> {
    validate_source_receipt(receipt)?;
    if receipt.model_id != operation.request.model_id
        || receipt.publication_id != operation.request.expected_publication_id
        || receipt.purpose != operation.request.purpose
        || receipt.publication_delegation_id != operation.request.publication_delegation_id
    {
        return Err(invalid(
            "Acceptance receipt conflicts with the imported original request; retain the operation and reply.",
        ));
    }
    Ok(())
}

fn validate_accepted(recovery: &Recovery, accepted: &Accepted) -> Result<(), Error> {
    if Some(accepted.run_id) != recovery.target_id()
        || accepted.created_by_user_id.is_nil()
        || !models::validation::valid_model_id(&accepted.model_id)
        || !models::validation::valid_model_id(&accepted.publication_id)
        || accepted.contract_version != 1
        || OffsetDateTime::parse(&accepted.created_at, &Rfc3339).is_err()
        || (accepted.purpose.is_standard() && accepted.publication_delegation_id.is_some())
        || accepted
            .publication_delegation_id
            .is_some_and(|id| id.is_nil())
    {
        return Err(invalid(
            "Accepted run evidence has an inconsistent identity.",
        ));
    }
    let definition: ScientificDefinition = serde_json::from_value(accepted.definition.clone())?;
    models::validation::validate_definition(&definition).map_err(|e| invalid(e.to_string()))?;
    models::validate_input_value(&accepted.requested_inputs).map_err(|e| invalid(e.to_string()))?;
    models::validate_inputs(&definition, &accepted.effective_inputs)
        .map_err(|e| invalid(e.to_string()))?;
    let publication = &accepted.publication;
    if publication["publication_id"] != accepted.publication_id
        || publication["context_id"]
            .as_str()
            .is_none_or(|s| !models::validation::valid_model_id(s))
        || publication["compute_grant_id"]
            .as_str()
            .and_then(|s| s.parse::<Uuid>().ok())
            .is_none_or(|id| id.is_nil())
        || publication["execution_location"]
            .as_str()
            .is_none_or(|s| s.trim().is_empty())
        || publication["disclosure"]
            .as_str()
            .is_none_or(|s| s.trim().is_empty())
        || publication["installation"]
            != serde_json::to_value(InstallationIdentity::derive(&definition))?
    {
        return Err(invalid(
            "Accepted publication evidence conflicts with the retained contract.",
        ));
    }
    validate_input_pins(accepted, &definition)?;
    if let Some(terminal) = &accepted.terminal
        && (!matches!(
            terminal.state.as_str(),
            "succeeded" | "failed" | "cancelled"
        ) || (!terminal.result.is_object() && !terminal.result.is_null())
            || (terminal.result.is_null()
                && (!terminal.output_artifacts.is_empty()
                    || !terminal.output_collections.is_empty())))
    {
        return Err(invalid(
            "Recovered terminal evidence has an inconsistent outcome or result.",
        ));
    }
    match recovery {
        Recovery::ImportedPrepared {
            operation, receipt, ..
        } => {
            if accepted.model_id != operation.request.model_id
                || accepted.publication_id != operation.request.expected_publication_id
                || accepted.created_by_user_id != operation.user_id
                || accepted.purpose != operation.request.purpose
                || accepted.publication_delegation_id != operation.request.publication_delegation_id
                || !enumeray_contracts::numeric::equivalent(
                    &accepted.requested_inputs,
                    &operation.request.inputs,
                )
                || !enumeray_contracts::numeric::equivalent(
                    &accepted.effective_inputs,
                    &operation.request.inputs,
                )
                || receipt
                    .as_ref()
                    .is_none_or(|r| r.created_at != accepted.created_at)
            {
                return Err(invalid(
                    "Accepted provenance conflicts with the imported original operation.",
                ));
            }
        }
        Recovery::AttachedRun {
            source_receipt: Some(receipt),
            ..
        } => {
            if accepted.model_id != receipt.model_id
                || accepted.publication_id != receipt.publication_id
                || accepted.purpose != receipt.purpose
                || accepted.publication_delegation_id != receipt.publication_delegation_id
                || accepted.created_at != receipt.created_at
            {
                return Err(invalid(
                    "Authorized run provenance conflicts with the saved receipt.",
                ));
            }
        }
        Recovery::AttachedRun { .. } => (),
    }
    Ok(())
}

fn validate_input_pins(
    accepted: &Accepted,
    definition: &ScientificDefinition,
) -> Result<(), Error> {
    let files = models::files::references(definition, &accepted.effective_inputs)
        .map_err(|e| invalid(e.to_string()))?;
    if files.len() != accepted.input_artifacts.len() {
        return Err(invalid(
            "Accepted provenance omitted or duplicated input artifact pins.",
        ));
    }
    for reference in files {
        let pins = accepted
            .input_artifacts
            .iter()
            .filter(|pin| pin["name"] == reference.input_name)
            .collect::<Vec<_>>();
        let Some(pin) = pins.first().filter(|_| pins.len() == 1) else {
            return Err(invalid(
                "Accepted input artifact pins have conflicting slot identities.",
            ));
        };
        if pin["artifact_id"] != reference.artifact_id.to_string()
            || pin["version_id"].as_str().is_none_or(str::is_empty)
            || pin["size_bytes"]
                .as_u64()
                .is_none_or(|n| n > reference.declaration.max_bytes)
            || pin["media_type"].as_str().is_none_or(|media| {
                !reference
                    .declaration
                    .media_types
                    .iter()
                    .any(|allowed| allowed == media)
            })
            || pin["checksum"]["algorithm"] != "sha256"
            || pin["checksum"]["checksum_type"] != "full_object"
            || pin["checksum"]["value"]
                .as_str()
                .is_none_or(|s| !enumeray_contracts::collections::valid_checksum(s))
        {
            return Err(invalid(
                "Accepted input artifact evidence conflicts with its reference or contract.",
            ));
        }
    }
    let collections =
        enumeray_contracts::collections::references(definition, &accepted.effective_inputs)
            .map_err(|e| invalid(e.to_string()))?;
    if collections.len() != accepted.input_collections.len() {
        return Err(invalid(
            "Accepted provenance omitted or duplicated input collection pins.",
        ));
    }
    for (name, id, declaration) in collections {
        let pins = accepted
            .input_collections
            .iter()
            .filter(|pin| pin["name"] == name)
            .collect::<Vec<_>>();
        let Some(pin) = pins.first().filter(|_| pins.len() == 1) else {
            return Err(invalid(
                "Accepted input collection pins have conflicting slot identities.",
            ));
        };
        if pin["collection_id"] != id.to_string()
            || pin["manifest_digest"]
                .as_str()
                .is_none_or(|s| !enumeray_contracts::collections::valid_checksum(s))
            || pin["member_count"]
                .as_u64()
                .is_none_or(|n| n > u64::from(declaration.max_members))
            || pin["total_bytes"]
                .as_u64()
                .is_none_or(|n| n > declaration.max_total_bytes)
        {
            return Err(invalid(
                "Accepted input collection evidence conflicts with its reference or contract.",
            ));
        }
    }
    Ok(())
}

fn accepted_from_provenance(recovery: &Recovery, provenance: &Value) -> Result<Accepted, Error> {
    if provenance["format_version"] != 1 {
        return Err(invalid("Unsupported accepted provenance format."));
    }
    #[derive(Deserialize)]
    struct Run {
        run_id: Uuid,
        model_id: String,
        publication_id: String,
        created_by_user_id: Uuid,
        #[serde(default)]
        purpose: RunPurpose,
        #[serde(default)]
        publication_delegation_id: Option<Uuid>,
        contract_version: u32,
        requested_inputs: Value,
        effective_inputs: Value,
        created_at: String,
        state: String,
    }
    let run: Run = serde_json::from_value(provenance["run"].clone())?;
    if !matches!(
        run.state.as_str(),
        "queued" | "running" | "succeeded" | "failed" | "cancelled"
    ) {
        return Err(invalid("Accepted provenance has an invalid run state."));
    }
    let terminal = if matches!(run.state.as_str(), "succeeded" | "failed" | "cancelled") {
        Some(Terminal {
            state: run.state.clone(),
            result: provenance["result"].clone(),
            output_artifacts: serde_json::from_value(provenance["output_artifacts"].clone())?,
            output_collections: provenance
                .get("output_collections")
                .map(|v| serde_json::from_value(v.clone()))
                .transpose()?
                .unwrap_or_default(),
        })
    } else {
        None
    };
    let source = &provenance["publication"];
    let accepted = Accepted {
        run_id: run.run_id,
        model_id: run.model_id,
        publication_id: run.publication_id,
        created_by_user_id: run.created_by_user_id,
        purpose: run.purpose,
        publication_delegation_id: run.publication_delegation_id,
        contract_version: run.contract_version,
        requested_inputs: run.requested_inputs,
        effective_inputs: run.effective_inputs,
        created_at: run.created_at,
        publication: json!({"publication_id":source["publication_id"],"context_id":source["context_id"],
            "compute_grant_id":source["compute_grant_id"],"execution_location":source["execution_location"],"disclosure":source["disclosure"],"installation":source["installation"]}),
        definition: provenance["definition"].clone(),
        input_artifacts: serde_json::from_value(provenance["input_artifacts"].clone())?,
        input_collections: provenance
            .get("input_collections")
            .map(|v| serde_json::from_value(v.clone()))
            .transpose()?
            .unwrap_or_default(),
        terminal,
    };
    validate_accepted(recovery, &accepted)?;
    Ok(accepted)
}

pub(super) fn validate_provenance(recovery: &Recovery, provenance: &Value) -> Result<(), Error> {
    let expected = recovery.accepted().ok_or_else(|| {
        invalid(
            "Missing retained accepted provenance; recover authorized evidence before collecting.",
        )
    })?;
    let actual = accepted_from_provenance(recovery, provenance)?;
    if !same_retained_evidence(expected, &actual)? {
        return Err(invalid(
            "Provenance conflicts with immutable recovered acceptance evidence; preserve both histories.",
        ));
    }
    Ok(())
}

/// Compare all acceptance facts, and fence terminal evidence only when the
/// retained snapshot already observed it. This permits active-to-terminal progress.
fn same_retained_evidence(expected: &Accepted, actual: &Accepted) -> Result<bool, Error> {
    let mut expected_value = serde_json::to_value(expected)?;
    let mut actual_value = serde_json::to_value(actual)?;
    expected_value
        .as_object_mut()
        .expect("accepted object")
        .remove("terminal");
    actual_value
        .as_object_mut()
        .expect("accepted object")
        .remove("terminal");
    if expected_value != actual_value {
        return Ok(false);
    }
    Ok(match &expected.terminal {
        Some(terminal) => {
            serde_json::to_value(terminal)? == serde_json::to_value(&actual.terminal)?
        }
        None => true,
    })
}

fn consistent_concurrent_evidence(left: &Accepted, right: &Accepted) -> Result<bool, Error> {
    // An older active response can arrive after another reader has published a
    // terminal snapshot. Keep the terminal snapshot without treating absence
    // of older evidence as a contradictory terminal result.
    if right.terminal.is_none() {
        same_retained_evidence(right, left)
    } else {
        same_retained_evidence(left, right)
    }
}

/// Caller holds the short state lock. Sidecar publication precedes the journal.
pub(super) fn load(
    directory: &Directory,
    api_url: &str,
    user_id: Uuid,
    repair: bool,
) -> Result<Recovery, Error> {
    let initial: Recovery = directory.read(JOURNAL)?;
    initial.validate()?;
    if serde_json::to_value(initial.initial())? != serde_json::to_value(&initial)? {
        return Err(invalid(
            "Recovery source journal contains mutable execution evidence.",
        ));
    }
    let mut saved: Recovery = if directory.exists("analysis.json")? {
        directory.read("analysis.json")?
    } else {
        initial.clone()
    };
    saved.validate()?;
    if !saved.same_source(&initial)? || saved.api_url() != api_url || saved.user_id() != user_id {
        return Err(invalid(
            "Analysis recovery belongs to another API/account or conflicts with its original journal.",
        ));
    }
    if let Recovery::ImportedPrepared {
        operation, receipt, ..
    } = &mut saved
    {
        directory.check_evidence("state/prepared-run.json", operation, repair)?;
        if directory.exists("state/receipt.json")? {
            let published: SubmissionReceipt = directory.read("state/receipt.json")?;
            validate_operation_receipt(operation, &published)?;
            if let Some(retained) = receipt.as_ref()
                && serde_json::to_value(retained)? != serde_json::to_value(&published)?
            {
                return Err(invalid("Imported acceptance receipt changed."));
            }
            *receipt = Some(published);
        } else if receipt.is_some() {
            return Err(invalid("Imported acceptance receipt is missing."));
        }
    } else if directory.exists("state/receipt.json")? {
        return Err(invalid(
            "Attached run unexpectedly contains local submission receipt evidence.",
        ));
    }
    if directory.exists(PROVENANCE)? {
        let provenance: Value = directory.read(PROVENANCE)?;
        let accepted = accepted_from_provenance(&saved, &provenance)?;
        if let Some(retained) = saved.accepted()
            && !same_retained_evidence(retained, &accepted)?
        {
            return Err(invalid("Recovered accepted evidence changed."));
        }
        saved.set_accepted(accepted);
    } else if saved.accepted().is_some() {
        return Err(invalid(
            "Recovered accepted provenance evidence is missing.",
        ));
    }
    if let Ok(inputs) = saved.effective_inputs() {
        if directory.exists("inputs.json")? {
            if !enumeray_contracts::numeric::equivalent(&directory.inputs()?, &inputs) {
                return Err(invalid(
                    "Recovered accepted inputs changed; restore the retained inputs and retry this directory.",
                ));
            }
        } else if repair {
            directory.write_inputs(&inputs)?;
        }
    }
    saved.validate()?;
    if repair {
        directory.write_locked("analysis.json", &saved)?;
    }
    Ok(saved)
}

fn read_initial(path: &Path) -> Result<Recovery, Error> {
    private_dir(path)?;
    let journal = path.join(JOURNAL);
    config::require_private(&journal)?;
    let mut bytes = Vec::new();
    File::open(journal)?
        .take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err(invalid("Recovery journal exceeds 8 MiB."));
    }
    let saved: Recovery = serde_json::from_slice(&bytes)?;
    saved.validate()?;
    Ok(saved.initial())
}

pub(crate) fn import(args: &Args, prepared: &Path, path: &Path) -> Result<Value, Error> {
    let operation = match fs::symlink_metadata(prepared) {
        Ok(_) => read_prepared(prepared)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => match read_initial(path)? {
            Recovery::ImportedPrepared { operation, .. } => operation,
            Recovery::AttachedRun { .. } => {
                return Err(invalid(
                    "Existing directory recovers an attached run, not this prepared operation.",
                ));
            }
        },
        Err(error) => return Err(error.into()),
    };
    validate_prepared(&operation)?;
    if let Some(api) = &args.api {
        let selected = enumeray_client::validate_url(api)?;
        if selected.path() != "/"
            || selected.query().is_some()
            || selected.as_str() != operation.api_url
        {
            return Err(invalid(
                "Selected API conflicts with the prepared operation's original API; retain the original operation.",
            ));
        }
    }
    let initial = Recovery::ImportedPrepared {
        schema_version: 1,
        operation,
        receipt: None,
        accepted: None,
    };
    let directory = Directory::recovery(path, &initial)?;
    let saved = {
        let _snapshot = directory.state_lock()?;
        load(&directory, initial.api_url(), initial.user_id(), true)?
    };
    Ok(summary(&directory, &saved, None))
}

fn read_prepared(path: &Path) -> Result<PreparedRun, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Request {
        #[serde(default)]
        publication_delegation_id: Option<Uuid>,
        #[serde(default)]
        purpose: RunPurpose,
        model_id: String,
        expected_publication_id: String,
        inputs: Box<RawValue>,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Document {
        schema_version: u32,
        api_url: String,
        user_id: Uuid,
        idempotency_key: String,
        request: Request,
    }
    config::require_private(path)?;
    let document: Document = config::read_json(path)?;
    let inputs = models::parse_inputs(document.request.inputs.get().as_bytes())
        .map_err(|e| invalid(e.to_string()))?;
    Ok(PreparedRun {
        schema_version: document.schema_version,
        api_url: document.api_url,
        user_id: document.user_id,
        idempotency_key: document.idempotency_key,
        request: enumeray_client::RunSubmission {
            publication_delegation_id: document.request.publication_delegation_id,
            purpose: document.request.purpose,
            model_id: document.request.model_id,
            expected_publication_id: document.request.expected_publication_id,
            inputs,
        },
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptFile {
    #[serde(default)]
    publication_delegation_id: Option<Uuid>,
    #[serde(default)]
    purpose: RunPurpose,
    run_id: Uuid,
    model_id: String,
    publication_id: String,
    state: String,
    created_at: String,
}

fn read_receipt(path: &Path) -> Result<SubmissionReceipt, Error> {
    config::require_private(path)?;
    let receipt: ReceiptFile = config::read_json(path)?;
    let receipt = SubmissionReceipt {
        publication_delegation_id: receipt.publication_delegation_id,
        purpose: receipt.purpose,
        run_id: receipt.run_id,
        model_id: receipt.model_id,
        publication_id: receipt.publication_id,
        state: receipt.state,
        created_at: receipt.created_at,
    };
    validate_source_receipt(&receipt)?;
    Ok(receipt)
}

pub(super) fn attach(
    client: &EnumerayClient,
    run_id: Option<&str>,
    receipt: Option<&Path>,
    path: &Path,
) -> Result<Value, Error> {
    if run_id.is_some() == receipt.is_some() {
        return Err(invalid("Select either an accepted run ID or --receipt."));
    }
    let source_receipt = if let Some(source) = receipt {
        match fs::symlink_metadata(source) {
            Ok(_) => Some(read_receipt(source)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => match read_initial(path)?
            {
                Recovery::AttachedRun {
                    source_receipt: Some(receipt),
                    ..
                } => Some(receipt),
                _ => {
                    return Err(invalid(
                        "The receipt source is unavailable and this directory has no matching receipt journal.",
                    ));
                }
            },
            Err(error) => return Err(error.into()),
        }
    } else {
        None
    };
    let run_id = match run_id {
        Some(id) => {
            let parsed = Uuid::parse_str(id)
                .map_err(|_| invalid("Accepted run ID must be a canonical nonempty UUID."))?;
            if parsed.is_nil() || parsed.to_string() != id {
                return Err(invalid(
                    "Accepted run ID must be a canonical nonempty UUID.",
                ));
            }
            parsed
        }
        None => {
            source_receipt
                .as_ref()
                .expect("receipt selection checked")
                .run_id
        }
    };
    let initial = Recovery::AttachedRun {
        schema_version: 1,
        api_url: client.base_url().into(),
        user_id: client.me()?.user_id,
        run_id,
        source_receipt,
        accepted: None,
    };
    initial.validate()?;
    let directory = Directory::recovery(path, &initial)?;
    let mut saved = {
        let _snapshot = directory.state_lock()?;
        load(&directory, client.base_url(), initial.user_id(), true)?
    };
    hydrate(client, &directory, &mut saved)?;
    Ok(summary(&directory, &saved, None))
}

pub(super) fn hydrate(
    client: &EnumerayClient,
    directory: &Directory,
    saved: &mut Recovery,
) -> Result<(), Error> {
    if saved.accepted().is_some() {
        return Ok(());
    }
    let id = saved.target_id().ok_or_else(|| invalid("Imported submission outcome is unknown; explicitly use analysis submit DIR to reconcile the original request."))?;
    let provenance: Value =
        client.get(&resource_path(&["runs", &id.to_string(), "provenance"])?)?;
    let accepted = accepted_from_provenance(saved, &provenance)?;
    // Two readers can fetch different progress snapshots. The first snapshot is
    // retained verbatim; only immutable acceptance facts must agree.
    let _snapshot = directory.state_lock()?;
    let mut current = load(directory, saved.api_url(), saved.user_id(), false)?;
    if let Some(retained) = current.accepted() {
        if !consistent_concurrent_evidence(retained, &accepted)? {
            return Err(invalid(
                "Concurrent recovery returned conflicting accepted evidence; preserve this directory.",
            ));
        }
    } else {
        directory.write_locked(PROVENANCE, &provenance)?;
        current.set_accepted(accepted);
    }
    *saved = current;
    let inputs = saved.effective_inputs()?;
    if directory.exists("inputs.json")? {
        if !enumeray_contracts::numeric::equivalent(&directory.inputs()?, &inputs) {
            return Err(invalid(
                "Recovered inputs conflict with accepted provenance; preserve this directory.",
            ));
        }
    } else {
        directory.write_inputs(&inputs)?;
    }
    directory.write_locked("analysis.json", saved)
}

pub(super) fn submit(
    client: &EnumerayClient,
    directory: &Directory,
    saved: &mut Recovery,
) -> Result<Value, Error> {
    if let Recovery::ImportedPrepared {
        operation, receipt, ..
    } = saved
    {
        if receipt.is_none() {
            let accepted = client.submit(operation)?;
            validate_operation_receipt(operation, &accepted)?;
            directory.evidence("state/receipt.json", &accepted)?;
            *receipt = Some(accepted);
            directory.save_recovery(saved)?;
        }
    } else if saved.accepted().is_none() {
        return Err(invalid(
            "Attachment is incomplete; repeat analysis attach with this directory. Attached runs cannot submit executions.",
        ));
    }
    Ok(summary(directory, saved, None))
}

pub(super) fn summary(directory: &Directory, saved: &Recovery, run: Option<Value>) -> Value {
    let receipt = match saved {
        Recovery::ImportedPrepared { receipt, .. } => {
            serde_json::to_value(receipt).unwrap_or(Value::Null)
        }
        Recovery::AttachedRun { .. } => Value::Null,
    };
    let accepted = saved.run_id().is_some();
    let mut value = json!({"directory":directory.root,"stage":if accepted {"accepted"} else if matches!(saved,Recovery::ImportedPrepared {..}) {"submission_uncertain"} else {"attachment_incomplete"},
        "run_id":saved.run_id(),"receipt":receipt,"run":run,"input_files":[],"input_collections":[],
        "next_action":if accepted {"Observe with analysis status/wait DIR; collect terminal evidence with analysis collect DIR."}
            else if matches!(saved,Recovery::ImportedPrepared {..}) {"The original submission outcome is unknown. Explicitly use analysis submit DIR to submit or reconcile the unchanged original request and key."}
            else {"Repeat analysis attach with this directory to recover accepted evidence; attachment cannot request an execution."}});
    for (key, field) in saved.identity().as_object().expect("identity object") {
        value[key] = field.clone();
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn operation() -> PreparedRun {
        PreparedRun {
            schema_version: 1,
            api_url: "http://127.0.0.1:1/".into(),
            user_id: Uuid::new_v4(),
            idempotency_key: "original_non_uuid-key".into(),
            request: enumeray_client::RunSubmission {
                publication_delegation_id: None,
                purpose: RunPurpose::Standard,
                model_id: "model".into(),
                expected_publication_id: "release-one".into(),
                inputs: json!({"value":2}),
            },
        }
    }

    #[test]
    fn original_non_uuid_keys_and_api_account_are_retained_without_discovery() {
        let operation = operation();
        validate_prepared(&operation).unwrap();
        let recovery = Recovery::ImportedPrepared {
            schema_version: 1,
            operation: operation.clone(),
            receipt: None,
            accepted: None,
        };
        recovery.validate().unwrap();
        assert_eq!(recovery.api_url(), operation.api_url);
        assert_eq!(recovery.user_id(), operation.user_id);
        assert_eq!(recovery.run_id(), None);
        assert!(recovery.definition().is_err());
        let temp = tempfile::tempdir().unwrap();
        let directory = Directory::recovery(&temp.path().join("analysis"), &recovery).unwrap();
        let restored = {
            let _snapshot = directory.state_lock().unwrap();
            load(&directory, recovery.api_url(), recovery.user_id(), true).unwrap()
        };
        assert!(restored.same_source(&recovery).unwrap());
        assert!(directory.exists("state/prepared-run.json").unwrap());
        assert_eq!(directory.inputs().unwrap(), operation.request.inputs);
        assert!(
            directory.read::<Value>("analysis.json").unwrap()["operation"]["idempotency_key"]
                == operation.idempotency_key
        );
    }

    #[test]
    fn early_journal_resumes_missing_derived_files_and_preserves_conflicting_destinations() {
        let recovery = Recovery::ImportedPrepared {
            schema_version: 1,
            operation: operation(),
            receipt: None,
            accepted: None,
        };
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("analysis");
        {
            let directory = Directory::recovery(&path, &recovery).unwrap();
            assert!(directory.exists(JOURNAL).unwrap());
        }
        fs::remove_dir(path.join("state/uploads")).unwrap();
        fs::remove_dir(path.join("state")).unwrap();
        let directory = Directory::recovery(&path, &recovery).unwrap();
        {
            let _snapshot = directory.state_lock().unwrap();
            load(&directory, recovery.api_url(), recovery.user_id(), true).unwrap();
        }
        drop(directory);
        let mut conflicting = recovery.clone();
        if let Recovery::ImportedPrepared { operation, .. } = &mut conflicting {
            operation.idempotency_key = "another-key".into();
        }
        assert!(Directory::recovery(&path, &conflicting).is_err());
        let retained = read_initial(&path).unwrap();
        assert!(retained.same_source(&recovery).unwrap());
        let existing = temp.path().join("existing");
        fs::create_dir(&existing).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&existing, fs::Permissions::from_mode(0o700)).unwrap();
        }
        assert!(Directory::recovery(&existing, &recovery).is_err());
        assert_eq!(fs::read_dir(&existing).unwrap().count(), 0);
    }

    #[test]
    fn receipt_before_journal_recovers_without_inventing_submission_evidence() {
        let operation = operation();
        let recovery = Recovery::ImportedPrepared {
            schema_version: 1,
            operation: operation.clone(),
            receipt: None,
            accepted: None,
        };
        let temp = tempfile::tempdir().unwrap();
        let directory = Directory::recovery(&temp.path().join("analysis"), &recovery).unwrap();
        let receipt = SubmissionReceipt {
            publication_delegation_id: None,
            purpose: RunPurpose::Standard,
            run_id: Uuid::new_v4(),
            model_id: operation.request.model_id,
            publication_id: operation.request.expected_publication_id,
            state: "queued".into(),
            created_at: "2026-10-19T12:00:00Z".into(),
        };
        directory.evidence("state/receipt.json", &receipt).unwrap();
        let restored = {
            let _snapshot = directory.state_lock().unwrap();
            load(&directory, recovery.api_url(), recovery.user_id(), true).unwrap()
        };
        assert_eq!(restored.run_id(), Some(receipt.run_id.to_string()));
        assert!(restored.accepted().is_none());
        assert!(restored.definition().is_err());
        assert!(load_wrong_account(&directory, &recovery));
    }

    fn load_wrong_account(directory: &Directory, recovery: &Recovery) -> bool {
        let _snapshot = directory.state_lock().unwrap();
        load(directory, recovery.api_url(), Uuid::new_v4(), false).is_err()
    }

    fn attached_provenance() -> (Recovery, Value) {
        let definition =
            models::parse_publication(include_bytes!("../../examples/scalar-publication.json"))
                .unwrap()
                .definition;
        let run_id = Uuid::new_v4();
        let creator = Uuid::new_v4();
        let recovery = Recovery::AttachedRun {
            schema_version: 1,
            api_url: "http://127.0.0.1:1/".into(),
            user_id: Uuid::new_v4(),
            run_id,
            source_receipt: None,
            accepted: None,
        };
        let mut raw_definition = serde_json::to_value(&definition).unwrap();
        raw_definition["log_files"] = json!({});
        let provenance = json!({"format_version":1,"run":{"run_id":run_id,"model_id":"model","publication_id":"release-one",
            "created_by_user_id":creator,"contract_version":1,"requested_inputs":{"value":2},"effective_inputs":{"value":2},
            "created_at":"2026-10-19T12:00:00Z","state":"running"},"publication":{"publication_id":"release-one","context_id":"context-one",
            "compute_grant_id":Uuid::new_v4(),"execution_location":"Research compute","disclosure":"Reviewed destination disclosure.",
            "installation":InstallationIdentity::derive(&definition),"validation":null},"definition":raw_definition,
            "input_artifacts":[],"output_artifacts":[],"result":null,"execution":null});
        (recovery, provenance)
    }

    #[test]
    fn accepted_projection_preserves_raw_contract_and_separates_administrator_from_submitter() {
        let (mut recovery, mut provenance) = attached_provenance();
        recovery.set_accepted(accepted_from_provenance(&recovery, &provenance).unwrap());
        recovery.validate().unwrap();
        assert_ne!(recovery.creator_id(), Some(recovery.user_id()));
        assert_eq!(
            recovery.definition_value().unwrap(),
            provenance["definition"]
        );
        assert!(
            recovery
                .definition_value()
                .unwrap()
                .get("log_files")
                .is_some()
        );
        assert!(recovery.initial().same_source(&recovery).unwrap());
        provenance["run"]["state"] = json!("succeeded");
        provenance["result"] = json!({"outputs":{"doubled":4}});
        provenance["execution"] = json!({"attempt_id":Uuid::new_v4()});
        provenance["publication"]["validation"] = json!({"observed_at":"later"});
        validate_provenance(&recovery, &provenance).unwrap();
        provenance["publication"]["disclosure"] = json!("Changed disclosure");
        assert!(validate_provenance(&recovery, &provenance).is_err());
    }

    #[test]
    fn recovered_artifact_versions_and_input_contracts_are_immutable() {
        use base64::Engine as _;
        let (mut recovery, mut provenance) = attached_provenance();
        let id = Uuid::new_v4();
        let file = json!({"type":"object","properties":{"artifact_id":{"type":"string"}},"required":["artifact_id"],
            "additionalProperties":false,"x-enumeray-file":{"max_bytes":1024,"media_types":["text/csv"]}});
        provenance["definition"]["input_schema"] = json!({"type":"object","properties":{"dataset":file},"required":["dataset"],"additionalProperties":false});
        provenance["definition"]["examples"] =
            json!([{"name":"example","inputs":{"dataset":{"artifact_id":id}}}]);
        provenance["run"]["requested_inputs"] = json!({"dataset":{"artifact_id":id}});
        provenance["run"]["effective_inputs"] = provenance["run"]["requested_inputs"].clone();
        let definition: ScientificDefinition =
            serde_json::from_value(provenance["definition"].clone()).unwrap();
        provenance["publication"]["installation"] =
            serde_json::to_value(InstallationIdentity::derive(&definition)).unwrap();
        provenance["input_artifacts"] = json!([{"name":"dataset","artifact_id":id,"version_id":"accepted-version","size_bytes":3,"media_type":"text/csv",
            "checksum":{"algorithm":"sha256","checksum_type":"full_object","value":base64::engine::general_purpose::STANDARD.encode([0;32])}}]);
        recovery.set_accepted(accepted_from_provenance(&recovery, &provenance).unwrap());
        validate_provenance(&recovery, &provenance).unwrap();
        for (pointer, value) in [
            (
                "/input_artifacts/0/version_id",
                json!("replacement-version"),
            ),
            (
                "/run/effective_inputs/dataset/artifact_id",
                json!(Uuid::new_v4()),
            ),
            (
                "/definition/input_schema/properties/dataset/x-enumeray-file/max_bytes",
                json!(2048),
            ),
            ("/publication/compute_grant_id", json!(Uuid::new_v4())),
        ] {
            let mut changed = provenance.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert!(
                validate_provenance(&recovery, &changed).is_err(),
                "{pointer}"
            );
        }
        provenance["input_artifacts"][0] = Value::Null;
        assert!(validate_provenance(&recovery, &provenance).is_err());
    }

    #[test]
    fn active_snapshots_allow_terminal_progress_but_observed_terminal_outputs_are_fenced() {
        use base64::Engine as _;
        let (mut active, mut provenance) = attached_provenance();
        let output_id = Uuid::new_v4();
        let file = json!({"type":"object","properties":{"artifact_id":{"type":"string"}},"required":["artifact_id"],
            "additionalProperties":false,"x-enumeray-file":{"max_bytes":1024,"media_types":["text/csv"]}});
        provenance["definition"]["output_schema"] = json!({"type":"object","properties":{"dataset":file},"required":["dataset"],"additionalProperties":false});
        provenance["definition"]["examples"] = json!([{"name":"example","inputs":{"value":2}}]);
        let definition: ScientificDefinition =
            serde_json::from_value(provenance["definition"].clone()).unwrap();
        provenance["publication"]["installation"] =
            serde_json::to_value(InstallationIdentity::derive(&definition)).unwrap();
        active.set_accepted(accepted_from_provenance(&active, &provenance).unwrap());
        provenance["run"]["state"] = json!("succeeded");
        provenance["result"] = json!({"outputs":{"dataset":{"artifact_id":output_id}},"files":{"dataset":{"artifact_id":output_id}},"optional_output_failures":{}});
        provenance["output_artifacts"] = json!([{"name":"dataset","artifact_id":output_id,"logical_name":"dataset","version_id":"result-version-one",
            "size_bytes":3,"media_type":"text/csv","checksum":{"algorithm":"sha256","checksum_type":"full_object","value":base64::engine::general_purpose::STANDARD.encode([0;32])}}]);
        validate_provenance(&active, &provenance).unwrap();
        let mut terminal = active.initial();
        terminal.set_accepted(accepted_from_provenance(&terminal, &provenance).unwrap());
        validate_provenance(&terminal, &provenance).unwrap();
        assert!(
            consistent_concurrent_evidence(
                active.accepted().unwrap(),
                terminal.accepted().unwrap()
            )
            .unwrap()
        );
        assert!(
            consistent_concurrent_evidence(
                terminal.accepted().unwrap(),
                active.accepted().unwrap()
            )
            .unwrap()
        );
        for (pointer, value) in [
            (
                "/output_artifacts/0/version_id",
                json!("result-version-two"),
            ),
            ("/result/outputs/dataset/artifact_id", json!(Uuid::new_v4())),
            ("/run/state", json!("failed")),
        ] {
            let mut changed = provenance.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert!(
                validate_provenance(&terminal, &changed).is_err(),
                "{pointer}"
            );
        }
        provenance["output_collections"] =
            json!([{"name":"extra","collection_id":Uuid::new_v4(),"manifest_digest":"changed"}]);
        assert!(validate_provenance(&terminal, &provenance).is_err());
    }

    #[test]
    fn imported_decimal_spelling_survives_authoritative_storage_normalization() {
        let (_, mut provenance) = attached_provenance();
        let mut original = operation();
        original.request.inputs = serde_json::from_str(r#"{"value":2.00}"#).unwrap();
        provenance["run"]["created_by_user_id"] = json!(original.user_id);
        let receipt = SubmissionReceipt {
            publication_delegation_id: None,
            purpose: RunPurpose::Standard,
            run_id: serde_json::from_value(provenance["run"]["run_id"].clone()).unwrap(),
            model_id: "model".into(),
            publication_id: "release-one".into(),
            state: "queued".into(),
            created_at: "2026-10-19T12:00:00Z".into(),
        };
        let mut recovered = Recovery::ImportedPrepared {
            schema_version: 1,
            operation: original.clone(),
            receipt: Some(receipt.clone()),
            accepted: None,
        };
        recovered.set_accepted(accepted_from_provenance(&recovered, &provenance).unwrap());
        let temp = tempfile::tempdir().unwrap();
        let directory =
            Directory::recovery(&temp.path().join("analysis"), &recovered.initial()).unwrap();
        directory.write_inputs(&original.request.inputs).unwrap();
        directory.evidence("state/receipt.json", &receipt).unwrap();
        directory.evidence(PROVENANCE, &provenance).unwrap();
        directory.save_recovery(&recovered).unwrap();
        let loaded = {
            let _snapshot = directory.state_lock().unwrap();
            load(&directory, recovered.api_url(), recovered.user_id(), true).unwrap()
        };
        assert_eq!(loaded.effective_inputs().unwrap(), json!({"value":2}));
        assert_eq!(directory.inputs().unwrap(), original.request.inputs);
        let Recovery::ImportedPrepared { operation, .. } = loaded else {
            panic!("imported operation expected")
        };
        assert_eq!(operation.request.inputs.to_string(), r#"{"value":2.00}"#);
    }

    #[test]
    fn imported_inputs_reject_duplicate_and_reserved_raw_keys_without_normalization() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("prepared.json");
        let operation = operation();
        let mut envelope = serde_json::to_value(&operation).unwrap();
        envelope["request"]["inputs"] = json!("REPLACE_INPUTS");
        let template = serde_json::to_string(&envelope).unwrap();
        for input in [
            r#"{"value":1,"value":2}"#,
            r#"{"nested":[{"$serde_json::private::Number":"2"}]}"#,
        ] {
            let document = template.replace("\"REPLACE_INPUTS\"", input);
            config::write_private_bytes(&path, document.as_bytes(), path.exists()).unwrap();
            assert!(read_prepared(&path).is_err());
        }
        let document = template.replace("\"REPLACE_INPUTS\"", "[1,2]");
        config::write_private_bytes(&path, document.as_bytes(), true).unwrap();
        let prepared = read_prepared(&path).unwrap();
        validate_prepared(&prepared).unwrap();
        assert_eq!(prepared.request.inputs, json!([1, 2]));
    }
}
