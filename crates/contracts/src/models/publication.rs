//! The single editable author document, normalized before release identity is assigned.

use super::{
    MAX_IMPORT_BYTES, ModelMetadata, ModelValidationError, ScientificDefinition, ScientificExample,
    contract, validation,
};
use crate::{execution::InstallationSpec, management::PublicationCreate};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Editable installation selection. An uninstalled author draft may omit its context/path.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationExecution {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_path: Option<String>,
    pub entrypoint: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slurm_script: Option<String>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
}

/// Expanded author configuration; never a registered candidate until `candidate` succeeds.
/// Serialization is an internal normalized view; `author_document` emits editable source.
#[derive(Debug, Clone, Serialize)]
pub struct ParsedPublication {
    pub model: ModelMetadata,
    pub definition: ScientificDefinition,
    pub execution: PublicationExecution,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorPublication {
    publication_version: u32,
    model: AuthorModel,
    identities: Identities,
    execution: PublicationExecution,
    input_schema: Value,
    output_schema: Value,
    #[serde(default)]
    examples: Vec<ScientificExample>,
    #[serde(default, deserialize_with = "crate::logs::deserialize_log_files")]
    log_files: BTreeMap<String, crate::logs::LogFileDeclaration>,
    #[serde(default = "version_one")]
    contract_version: u32,
    #[serde(default = "version_one")]
    wrapper_protocol_version: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorModel {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Identities {
    model: String,
    wrapper: String,
    runtime: String,
    #[serde(default)]
    datasets: Vec<String>,
}

fn version_one() -> u32 {
    1
}

/// Parse bounded editable source, rejecting legacy formats and validating scientific contracts.
/// Missing context/release path describes a local draft; supplied installation values are checked.
pub fn parse_publication(bytes: &[u8]) -> Result<ParsedPublication, ModelValidationError> {
    if bytes.len() > MAX_IMPORT_BYTES {
        return Err(ModelValidationError::ImportTooLarge);
    }
    validation::preflight_json(bytes)?;
    let document: Value =
        serde_json::from_slice(bytes).map_err(|_| ModelValidationError::InvalidDocument)?;
    if document.as_object().is_some_and(|fields| {
        [
            "definition",
            "descriptor",
            "manifest_version",
            "binding_version",
        ]
        .iter()
        .any(|key| fields.contains_key(*key))
    }) {
        return Err(ModelValidationError::UnsupportedAuthorFormat);
    }
    let source: AuthorPublication =
        serde_json::from_value(document).map_err(|_| ModelValidationError::InvalidDocument)?;
    if source.publication_version != 1 {
        return Err(ModelValidationError::UnsupportedPublicationVersion);
    }
    let parsed = ParsedPublication {
        model: ModelMetadata {
            name: source.model.name.unwrap_or_else(|| source.model.id.clone()),
            id: source.model.id,
            description: source.model.description,
        },
        definition: ScientificDefinition {
            contract_version: source.contract_version,
            model_identity: source.identities.model,
            wrapper_identity: source.identities.wrapper,
            runtime_identity: source.identities.runtime,
            wrapper_protocol_version: source.wrapper_protocol_version,
            dataset_identities: source.identities.datasets,
            input_schema: source.input_schema,
            output_schema: source.output_schema,
            examples: source.examples,
            log_files: source.log_files,
        },
        execution: source.execution,
    };
    parsed.validate()?;
    Ok(parsed)
}

impl ParsedPublication {
    /// Validate complete science and any supplied draft installation settings, without API access.
    pub fn validate(&self) -> Result<(), ModelValidationError> {
        validation::validate_metadata(&self.model)?;
        validate_science(&self.definition)?;
        if self
            .execution
            .context_id
            .as_deref()
            .is_some_and(|id| !validation::valid_model_id(id))
        {
            return Err(ModelValidationError::InvalidPublicationIds);
        }
        // This placeholder is validation-only and is never retained as an installation path.
        self.installation(self.execution.release_path.as_deref().unwrap_or("/draft"))
            .validate()
            .map_err(ModelValidationError::InvalidInstallation)?;
        if serde_json::to_vec(self)
            .map_err(|_| ModelValidationError::InvalidDocument)?
            .len()
            > MAX_IMPORT_BYTES
        {
            return Err(ModelValidationError::ImportTooLarge);
        }
        Ok(())
    }

    /// Materialize an explicitly allocated stable candidate; this never assigns or changes its ID.
    pub fn candidate(
        &self,
        id: impl Into<String>,
    ) -> Result<PublicationCreate, ModelValidationError> {
        self.validate()?;
        let context_id = self
            .execution
            .context_id
            .clone()
            .ok_or(ModelValidationError::IncompletePublication("context_id"))?;
        let release_path = self
            .execution
            .release_path
            .as_deref()
            .ok_or(ModelValidationError::IncompletePublication("release_path"))?;
        let candidate = PublicationCreate {
            id: id.into(),
            definition: self.definition.clone(),
            context_id,
            descriptor: self.installation(release_path),
        };
        validate_publication(&candidate)?;
        Ok(candidate)
    }

    fn installation(&self, release_path: &str) -> InstallationSpec {
        InstallationSpec {
            release_path: release_path.into(),
            entrypoint: self.execution.entrypoint.clone(),
            slurm_script: self.execution.slurm_script.clone(),
            environment: self.execution.environment.clone(),
        }
    }

    /// Emit the author shape with normalized defaults; generated operation state stays separate.
    pub fn author_document(&self) -> Value {
        let mut document = json!({
            "publication_version": 1,
            "model": self.model,
            "identities": {
                "model": self.definition.model_identity,
                "wrapper": self.definition.wrapper_identity,
                "runtime": self.definition.runtime_identity,
                "datasets": self.definition.dataset_identities,
            },
            "execution": self.execution,
            "input_schema": self.definition.input_schema,
            "output_schema": self.definition.output_schema,
            "examples": self.definition.examples,
            "contract_version": self.definition.contract_version,
            "wrapper_protocol_version": self.definition.wrapper_protocol_version,
        });
        if !self.definition.log_files.is_empty() {
            document["log_files"] = json!(self.definition.log_files);
        }
        document
    }
}

/// Validate the immutable scientific shape and supported schemas before registration writes.
pub fn validate_publication_definition(
    definition: &ScientificDefinition,
) -> Result<(), ModelValidationError> {
    validation::validate_definition(definition)?;
    contract::validate_contract(definition)
}

/// Check file/collection protocol requirements independently of historical retry lookup.
pub fn validate_publication_protocol(
    definition: &ScientificDefinition,
) -> Result<(), ModelValidationError> {
    if definition.wrapper_protocol_version != 1
        && (super::requires_artifact_inputs(definition)
            || super::requires_artifact_outputs(definition)
            || crate::collections::requires(definition, false)
            || crate::collections::requires(definition, true))
    {
        return Err(ModelValidationError::ArtifactProtocolRequired);
    }
    Ok(())
}

fn validate_science(definition: &ScientificDefinition) -> Result<(), ModelValidationError> {
    validate_publication_definition(definition)?;
    validate_publication_protocol(definition)
}

/// Shared semantic validation for normalized machine registration and frozen release snapshots.
/// Context authority, driver matching and installed-content readiness remain server/connector checks.
pub fn validate_publication(candidate: &PublicationCreate) -> Result<(), ModelValidationError> {
    validate_science(&candidate.definition)?;
    if !validation::valid_model_id(&candidate.id)
        || !validation::valid_model_id(&candidate.context_id)
    {
        return Err(ModelValidationError::InvalidPublicationIds);
    }
    candidate
        .descriptor
        .validate()
        .map_err(ModelValidationError::InvalidInstallation)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal() -> Value {
        json!({
            "publication_version": 1,
            "model": {"id": "example"},
            "identities": {"model": "solver@1", "wrapper": "wrapper@1", "runtime": "python@3"},
            "execution": {"entrypoint": ["wrapper.py", "execute"]},
            "input_schema": {"type":"object", "properties":{"value":{"type":"number", "description":"Distance", "x-unit":"km"}}, "required":["value"], "additionalProperties":false},
            "output_schema": {"type":"object", "properties":{"doubled":{"type":"number"}}, "required":["doubled"], "additionalProperties":false}
        })
    }

    fn parse(value: &Value) -> Result<ParsedPublication, ModelValidationError> {
        parse_publication(&serde_json::to_vec(value).unwrap())
    }

    #[test]
    fn science_valid_drafts_expand_defaults_without_allocating_candidate_identity() {
        let parsed = parse(&minimal()).unwrap();
        assert_eq!(parsed.model.name, "example");
        assert_eq!(parsed.model.description, "");
        assert_eq!(parsed.definition.contract_version, 1);
        assert_eq!(parsed.definition.wrapper_protocol_version, 1);
        assert!(parsed.definition.dataset_identities.is_empty());
        assert!(parsed.definition.examples.is_empty());
        assert!(parsed.definition.log_files.is_empty());
        assert!(parsed.execution.environment.is_empty());
        assert!(parsed.execution.slurm_script.is_none());
        assert!(matches!(
            parsed.candidate("candidate-one"),
            Err(ModelValidationError::IncompletePublication("context_id"))
        ));
        assert!(parsed.author_document().get("id").is_none());
        assert_eq!(
            parsed.definition.input_schema["properties"]["value"]["x-unit"],
            "km"
        );
        let reparsed = parse(&parsed.author_document()).unwrap();
        assert_eq!(
            serde_json::to_value(parsed).unwrap(),
            serde_json::to_value(reparsed).unwrap()
        );
    }

    #[test]
    fn candidate_requires_installation_and_retains_explicit_identity_and_expanded_science() {
        let mut document = minimal();
        document["execution"]["context_id"] = json!("lab-cpu");
        assert!(matches!(
            parse(&document).unwrap().candidate("candidate-one"),
            Err(ModelValidationError::IncompletePublication("release_path"))
        ));
        document["execution"]["release_path"] = json!("/opt/models/example/r1");
        let parsed = parse(&document).unwrap();
        let candidate = parsed.candidate("candidate-one").unwrap();
        assert_eq!(candidate.id, "candidate-one");
        assert_eq!(candidate.context_id, "lab-cpu");
        let wire = serde_json::to_value(candidate).unwrap();
        assert_eq!(wire["definition"]["contract_version"], 1);
        assert_eq!(wire["definition"]["wrapper_protocol_version"], 1);
        assert_eq!(wire["definition"]["dataset_identities"], json!([]));
        assert_eq!(wire["descriptor"]["environment"], json!({}));
        for author_only in [
            "model",
            "identities",
            "execution",
            "publication_version",
            "input_schema",
        ] {
            assert!(wire.get(author_only).is_none());
        }
        assert!(matches!(
            parsed.candidate("INVALID"),
            Err(ModelValidationError::InvalidPublicationIds)
        ));
    }

    #[test]
    fn legacy_sources_and_internal_authority_fields_are_rejected() {
        let normalized = serde_json::to_value(parse(&minimal()).unwrap()).unwrap();
        assert!(parse(&normalized).is_err());
        for field in [
            "id",
            "definition",
            "descriptor",
            "manifest_version",
            "kind",
            "spec",
            "binding_version",
            "grant_id",
            "provider_binding_id",
            "installation",
        ] {
            let mut document = minimal();
            document[field] = json!("legacy");
            assert!(
                matches!(
                    parse(&document),
                    Err(ModelValidationError::InvalidDocument
                        | ModelValidationError::UnsupportedAuthorFormat)
                ),
                "{field}"
            );
        }
        for (section, field) in [
            ("model", "extra"),
            ("identities", "installation"),
            ("execution", "driver"),
            ("execution", "connection"),
            ("execution", "grant_id"),
        ] {
            let mut document = minimal();
            document[section][field] = json!("unknown");
            assert!(
                matches!(parse(&document), Err(ModelValidationError::InvalidDocument)),
                "{section}.{field}"
            );
        }
    }

    #[test]
    fn removed_formats_explain_the_single_source_migration_without_echoing_values() {
        for document in [
            json!({"model":{"id":"private-model"}, "definition":{"private":"secret"}}),
            json!({"binding_version":1, "context_id":"private-site"}),
            json!({"manifest_version":1, "kind":"model-publication", "spec":{"id":"private-candidate"}}),
        ] {
            let error = parse(&document).unwrap_err();
            assert_eq!(error, ModelValidationError::UnsupportedAuthorFormat);
            let message = error.to_string();
            assert!(message.contains("publication.json"));
            assert!(message.contains("initialize a release"));
            assert!(!message.contains("private"));
            assert!(!message.contains("secret"));
        }
    }

    #[test]
    fn required_fields_and_explicit_versions_fail_clearly() {
        for field in [
            "publication_version",
            "model",
            "identities",
            "execution",
            "input_schema",
            "output_schema",
        ] {
            let mut document = minimal();
            document.as_object_mut().unwrap().remove(field);
            assert!(
                matches!(parse(&document), Err(ModelValidationError::InvalidDocument)),
                "{field}"
            );
        }
        for version in [json!(0), json!(2)] {
            let mut document = minimal();
            document["publication_version"] = version;
            assert!(matches!(
                parse(&document),
                Err(ModelValidationError::UnsupportedPublicationVersion)
            ));
        }
        for version in [json!(null), json!("1"), json!(-1), json!(1.5)] {
            for field in [
                "publication_version",
                "contract_version",
                "wrapper_protocol_version",
            ] {
                let mut document = minimal();
                document[field] = version.clone();
                assert!(
                    matches!(parse(&document), Err(ModelValidationError::InvalidDocument)),
                    "{field}={version}"
                );
            }
        }
        let mut document = minimal();
        document["contract_version"] = json!(2);
        assert!(matches!(
            parse(&document),
            Err(ModelValidationError::InvalidContract(
                super::super::ContractError::UnsupportedContractVersion
            ))
        ));
    }

    #[test]
    fn draft_paths_commands_and_environment_are_checked_before_installation() {
        for (field, value) in [
            ("context_id", json!("INVALID")),
            ("release_path", json!("relative")),
            ("release_path", json!("/opt/../secret")),
            ("entrypoint", json!([])),
            ("entrypoint", json!(["/bin/wrapper"])),
            ("slurm_script", json!("../model.sbatch")),
        ] {
            let mut document = minimal();
            document["execution"][field] = value;
            assert!(parse(&document).is_err(), "{field}");
        }
        let mut document = minimal();
        document["execution"]["environment"] = json!({"LD_PRELOAD":"private-value"});
        let diagnostic = parse(&document).unwrap_err().to_string();
        assert!(diagnostic.contains("LD_PRELOAD"));
        assert!(!diagnostic.contains("private-value"));
    }

    #[test]
    fn schemas_and_examples_are_validated_when_parsing_drafts() {
        let mut document = minimal();
        document["examples"] = json!([{"name":"bad", "inputs":{"value":"wrong"}}]);
        assert!(matches!(
            parse(&document),
            Err(ModelValidationError::InvalidContract(_))
        ));
        document["examples"] = json!([]);
        document["output_schema"] = json!({"type":"object"});
        assert!(matches!(
            parse(&document),
            Err(ModelValidationError::InvalidContract(_))
        ));
    }

    #[test]
    fn artifact_protocol_validation_is_shared_with_machine_candidates() {
        let mut document: Value =
            serde_json::from_slice(include_bytes!("../../../../examples/artifact-input.json"))
                .unwrap();
        document["wrapper_protocol_version"] = json!(2);
        assert!(matches!(
            parse(&document),
            Err(ModelValidationError::ArtifactProtocolRequired)
        ));
        let parsed =
            parse_publication(include_bytes!("../../../../examples/artifact-input.json")).unwrap();
        let mut candidate = parsed.candidate("artifact-candidate").unwrap();
        candidate.definition.wrapper_protocol_version = 2;
        assert!(validate_publication_definition(&candidate.definition).is_ok());
        assert_eq!(
            validate_publication_protocol(&candidate.definition).unwrap_err(),
            ModelValidationError::ArtifactProtocolRequired
        );
        assert_eq!(
            validate_publication(&candidate).unwrap_err(),
            ModelValidationError::ArtifactProtocolRequired
        );
        let mut scalar = minimal();
        scalar["wrapper_protocol_version"] = json!(2);
        assert_eq!(
            parse(&scalar).unwrap().definition.wrapper_protocol_version,
            2
        );
    }

    #[test]
    fn catalogue_text_remains_separate_from_machine_candidate_semantics() {
        let mut document = minimal();
        document["execution"]["context_id"] = json!("lab-cpu");
        document["execution"]["release_path"] = json!("/opt/models/example/r1");
        let mut parsed = parse(&document).unwrap();
        let original = serde_json::to_value(parsed.candidate("candidate-one").unwrap()).unwrap();
        parsed.model.name = "Revised catalogue name".into();
        parsed.model.description = "A clearer catalogue explanation.".into();
        assert_eq!(
            serde_json::to_value(parsed.candidate("candidate-one").unwrap()).unwrap(),
            original
        );
        parsed.definition.model_identity = "solver@2".into();
        assert_ne!(
            serde_json::to_value(parsed.candidate("candidate-one").unwrap()).unwrap(),
            original
        );
    }

    #[test]
    fn duplicate_decoded_members_are_rejected_before_normalization() {
        let source = serde_json::to_string(&minimal()).unwrap();
        let duplicate = source.replace("\"x-unit\":\"km\"", "\"x-unit\":\"m\",\"x-unit\":\"km\"");
        assert_ne!(duplicate, source);
        assert!(matches!(
            parse_publication(duplicate.as_bytes()),
            Err(ModelValidationError::InvalidDocument)
        ));
        let duplicate = source.replace("\"value\":{", "\"value\":false,\"va\\u006cue\":{");
        assert!(matches!(
            parse_publication(duplicate.as_bytes()),
            Err(ModelValidationError::InvalidDocument)
        ));
    }

    #[test]
    fn null_site_selection_is_a_draft_but_not_a_release_candidate() {
        let mut document = minimal();
        document["execution"]["context_id"] = Value::Null;
        document["execution"]["release_path"] = Value::Null;
        let parsed = parse(&document).unwrap();
        assert!(parsed.execution.context_id.is_none());
        assert!(parsed.execution.release_path.is_none());
        assert!(parsed.candidate("new-candidate").is_err());
    }
}
