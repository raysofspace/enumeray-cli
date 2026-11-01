//! Initial v1 contract: finite, unconditional named artifact inputs and outputs. Public references
//! never contain provider paths, object keys, or temporary transfer capabilities.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::{ContractError, InputError, SchemaKind, ScientificDefinition, contract};

/// Closed public stored-file reference, never a path or transfer capability.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactReference {
    #[serde(deserialize_with = "canonical_reference_id")]
    #[cfg_attr(
        feature = "openapi",
        schema(
            pattern = "^(?!00000000-0000-0000-0000-000000000000$)[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"
        )
    )]
    pub artifact_id: Uuid,
}

pub(crate) fn canonical_reference_id<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Uuid, D::Error> {
    let text = String::deserialize(deserializer)?;
    let id = Uuid::parse_str(&text).map_err(serde::de::Error::custom)?;
    if id.is_nil() || id.to_string() != text {
        return Err(serde::de::Error::custom(
            "expected a non-nil canonical UUID",
        ));
    }
    Ok(id)
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileDeclaration {
    pub media_types: Vec<String>,
    pub max_bytes: u64,
}

pub struct FileReference {
    pub input_name: String,
    pub artifact_id: Uuid,
    pub declaration: FileDeclaration,
}

pub fn declarations(
    definition: &ScientificDefinition,
) -> Result<BTreeMap<String, FileDeclaration>, ContractError> {
    if definition.contract_version != 1 {
        return Err(ContractError::UnsupportedContractVersion);
    }
    // Validate both directions even when only one map is requested.
    let inputs = schema_declarations(&definition.input_schema, SchemaKind::Input)?;
    output_declarations(definition)?;
    Ok(inputs)
}

pub fn output_declarations(
    definition: &ScientificDefinition,
) -> Result<BTreeMap<String, FileDeclaration>, ContractError> {
    schema_declarations(&definition.output_schema, SchemaKind::Output)
}

fn schema_declarations(
    root: &Value,
    kind: SchemaKind,
) -> Result<BTreeMap<String, FileDeclaration>, ContractError> {
    let invalid = || ContractError::InvalidFileDeclarations;
    let mut files = BTreeMap::new();
    let mut has_references = false;
    {
        let mut schemas = BTreeMap::new();
        contract::collect_schemas(root, String::new(), kind, &mut schemas)?;
        for (pointer, schema) in schemas {
            has_references |= schema.get("$ref").is_some();
            let Some(annotation) = schema.get("x-enumeray-file") else {
                continue;
            };
            let name = root
                .get("properties")
                .and_then(Value::as_object)
                .and_then(|properties| {
                    properties
                        .keys()
                        .find(|name| pointer == contract::join_pointer("/properties", name))
                });
            let Some(name) = name else {
                return Err(invalid());
            };
            let declaration: FileDeclaration =
                serde_json::from_value(annotation.clone()).map_err(|_| invalid())?;
            if declaration.media_types.is_empty()
                || declaration.media_types.len() > 16
                || declaration.max_bytes > super::MAX_ARTIFACT_BYTES
                || declaration.media_types.iter().any(|media_type| {
                    !crate::media::valid_media_type(media_type)
                        || declaration
                            .media_types
                            .iter()
                            .filter(|v| *v == media_type)
                            .count()
                            != 1
                })
                || schema["type"] != "object"
                || schema["additionalProperties"] != false
                || schema["required"] != serde_json::json!(["artifact_id"])
                || schema["properties"]
                    .as_object()
                    .is_none_or(|p| p.len() != 1)
                || schema["properties"]["artifact_id"]["type"] != "string"
            {
                return Err(invalid());
            }
            files.insert(name.clone(), declaration);
        }
    }
    if !files.is_empty() {
        // These restrictions apply only to contracts declaring files. Ordinary
        // JSON schemas retain boolean roots, composition and local references.
        contract::validate_output_declarations(root).map_err(|_| invalid())?;
        if has_references {
            return Err(invalid());
        }
    }
    Ok(files)
}

/// Capability requirement of a validated scientific definition, including optional files.
/// Match the direct schema-position predicate in assignment claim selection.
/// Input values and annotation data never establish execution capabilities.
pub fn requires_artifact_inputs(definition: &ScientificDefinition) -> bool {
    definition
        .input_schema
        .get("properties")
        .and_then(Value::as_object)
        .is_some_and(|properties| {
            properties
                .values()
                .any(|schema| schema.get("x-enumeray-file").is_some())
        })
}

/// Output declarations require explicit connector collection/publication support.
pub fn requires_artifact_outputs(definition: &ScientificDefinition) -> bool {
    definition
        .output_schema
        .get("properties")
        .and_then(Value::as_object)
        .is_some_and(|properties| {
            properties
                .values()
                .any(|schema| schema.get("x-enumeray-file").is_some())
        })
}

pub fn references(
    definition: &ScientificDefinition,
    inputs: &Value,
) -> Result<Vec<FileReference>, InputError> {
    let declarations = declarations(definition).map_err(InputError::InvalidContract)?;
    let mut references = Vec::new();
    for (name, declaration) in declarations {
        let Some(value) = inputs.get(&name) else {
            continue;
        };
        let id = value
            .get("artifact_id")
            .and_then(Value::as_str)
            .ok_or(InputError::InvalidFileReference)?;
        let artifact_id = Uuid::parse_str(id).map_err(|_| InputError::InvalidFileReference)?;
        if value.as_object().is_none_or(|object| object.len() != 1) || artifact_id.to_string() != id
        {
            return Err(InputError::InvalidFileReference);
        }
        references.push(FileReference {
            input_name: name,
            artifact_id,
            declaration,
        });
    }
    // Every transaction acquires shared artifact locks in the same order.
    references.sort_by_key(|reference| reference.artifact_id);
    Ok(references)
}

/// Includes optional files: support must be known before accepting an assignment.
pub fn requires_large_artifacts(definition: &ScientificDefinition) -> bool {
    [&definition.input_schema, &definition.output_schema]
        .into_iter()
        .any(|root| {
            root.get("properties")
                .and_then(Value::as_object)
                .is_some_and(|properties| {
                    properties.values().any(|schema| {
                        schema
                            .get("x-enumeray-file")
                            .and_then(|file| file.get("max_bytes"))
                            .or_else(|| {
                                schema
                                    .get("x-enumeray-collection")
                                    .and_then(|c| c.get("max_file_bytes"))
                            })
                            .and_then(Value::as_u64)
                            .is_some_and(|size| size > super::MAX_SINGLE_UPLOAD_BYTES)
                    })
                })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{self, ModelValidationError};
    use serde_json::json;

    fn fixture() -> models::ParsedPublication {
        models::parse_publication(include_bytes!("../../../../examples/artifact-input.json"))
            .unwrap()
    }

    #[test]
    fn large_optional_files_require_explicit_support_without_changing_v1() {
        let mut model = fixture();
        assert!(!requires_large_artifacts(&model.definition));
        model.definition.input_schema["required"] = serde_json::json!([]);
        model.definition.input_schema["properties"]["dataset"]["x-enumeray-file"]["max_bytes"] =
            serde_json::json!(super::super::MAX_SINGLE_UPLOAD_BYTES + 1);
        contract::validate_contract(&model.definition).unwrap();
        let requirements = crate::execution::ExecutionRequirements::derive(
            crate::execution::ExecutionDriver::Process,
            &model.definition,
        );
        assert!(
            requirements
                .capabilities
                .contains(&crate::execution::Capability::LargeArtifacts)
        );
        assert_eq!(model.definition.contract_version, 1);
    }

    #[test]
    fn outputs_use_named_bounded_file_declarations_and_require_collection_capability() {
        let mut model =
            models::parse_publication(include_bytes!("../../../../examples/artifact-output.json"))
                .unwrap();
        contract::validate_contract(&model.definition).unwrap();
        assert!(requires_artifact_outputs(&model.definition));
        assert!(!requires_artifact_inputs(&model.definition));
        assert_eq!(output_declarations(&model.definition).unwrap().len(), 2);
        // Optional files still require collection; an annotation in ordinary
        // metadata does not. References may not alias a declared file position.
        model.definition.output_schema["required"] = json!([]);
        assert!(requires_artifact_outputs(&model.definition));
        model.definition.output_schema["properties"]["alias"] =
            json!({"$ref":"#/properties/result"});
        assert!(contract::validate_contract(&model.definition).is_err());
    }

    #[test]
    fn initial_contract_validates_named_public_references_without_resolving_examples() {
        let mut model = fixture();
        contract::validate_contract(&model.definition).unwrap();
        let id = Uuid::new_v4();
        let inputs = json!({"dataset":{"artifact_id":id},"label":"test"});
        models::validate_inputs(&model.definition, &inputs).unwrap();
        let pins = references(&model.definition, &inputs).unwrap();
        assert_eq!(pins.len(), 1);
        assert_eq!(pins[0].input_name, "dataset");
        assert_eq!(pins[0].artifact_id, id);
        // Missing optional file positions create no dependency; defaults stay inert.
        model.definition.input_schema["required"] = json!([]);
        model.definition.input_schema["properties"]["dataset"]["default"] =
            inputs["dataset"].clone();
        models::validate_inputs(&model.definition, &json!({})).unwrap();
        assert!(
            references(&model.definition, &json!({}))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn provider_paths_attachments_urls_and_noncanonical_ids_are_not_public_references() {
        let definition = fixture().definition;
        for value in [
            json!("/provider/input.csv"),
            json!({"path":"inputs/data.csv"}),
            json!({"attachment":"dataset"}),
            json!({"artifact_id":"https://storage.invalid/data"}),
            json!({"artifact_id":"AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA"}),
            json!({"artifact_id":"00000000000040008000000000000001"}),
            json!({"artifact_id":Uuid::new_v4(), "path":"inputs/data.csv"}),
            json!(null),
        ] {
            assert!(models::validate_inputs(&definition, &json!({"dataset":value})).is_err());
        }
    }

    #[test]
    fn file_declarations_are_bounded_and_unambiguous() {
        let original = fixture();
        for annotation in [
            json!(true),
            json!({}),
            json!({"media_types":[],"max_bytes":1}),
            json!({"media_types":["text/*"],"max_bytes":1}),
            json!({"media_types":["text/csv","text/csv"],"max_bytes":1}),
            json!({"media_types":["text/csv"],"max_bytes":super::super::MAX_ARTIFACT_BYTES + 1}),
            json!({"media_types":["text/csv"],"max_bytes":-1}),
            json!({"media_types":["text/csv"],"max_bytes":1,"secret":true}),
        ] {
            let mut model = original.clone();
            model.definition.input_schema["properties"]["dataset"]["x-enumeray-file"] = annotation;
            assert_eq!(
                contract::validate_contract(&model.definition),
                Err(ModelValidationError::InvalidContract(
                    ContractError::InvalidFileDeclarations
                ))
            );
        }
        for pointer in [
            "/$defs/unused",
            "/properties/group/properties/nested",
            "/allOf/0",
        ] {
            let mut model = original.clone();
            model.definition.input_schema = match pointer {
                "/$defs/unused" => {
                    json!({"type":"object","properties":{},"additionalProperties":false,
                    "$defs":{"unused":{"x-enumeray-file":true}}})
                }
                "/allOf/0" => json!({"type":"object","properties":{},"additionalProperties":false,
                    "allOf":[{"x-enumeray-file":true}]}),
                _ => {
                    json!({"type":"object","properties":{"group":{"properties":{"nested":{"x-enumeray-file":true}}}},"additionalProperties":false})
                }
            };
            assert!(declarations(&model.definition).is_err());
        }
        let mut model = original.clone();
        model.definition.output_schema["properties"]["value"]["x-enumeray-file"] = json!(true);
        assert!(declarations(&model.definition).is_err());
        model = original.clone();
        model.definition.input_schema["properties"]["alias"] =
            json!({"$ref":"#/properties/dataset"});
        assert!(declarations(&model.definition).is_err());
        model = original;
        model.definition.examples[0].inputs = json!({"dataset":{"artifact_id":"private-value"}});
        assert!(matches!(
            contract::validate_contract(&model.definition),
            Err(ModelValidationError::InvalidContract(
                ContractError::InvalidExample { .. }
            ))
        ));
    }

    #[test]
    fn undeclared_references_and_annotation_data_never_acquire_file_authority() {
        let mut model = fixture();
        model.definition.input_schema["properties"]["dataset"]
            .as_object_mut()
            .unwrap()
            .remove("x-enumeray-file");
        model.definition.examples[0].inputs = json!({"dataset":{"artifact_id":"arbitrary-text"}});
        contract::validate_contract(&model.definition).unwrap();
        assert!(!requires_artifact_inputs(&model.definition));
        assert!(
            references(&model.definition, &model.definition.examples[0].inputs)
                .unwrap()
                .is_empty()
        );
        let mut model = fixture();
        model.definition.input_schema["default"] = json!({"x-enumeray-file":true});
        model.definition.input_schema["x-note"] =
            json!({"x-enumeray-file":true,"$ref":"https://invalid"});
        contract::validate_contract(&model.definition).unwrap();
        assert_eq!(declarations(&model.definition).unwrap().len(), 1);
    }

    #[test]
    fn file_free_v1_keeps_boolean_composed_and_referenced_input_schemas() {
        let mut model = fixture();
        model.definition.examples.clear();
        for schema in [
            json!(true),
            json!({"allOf":[{"type":"integer"},{"minimum":0}]}),
            json!({"$defs":{"value":{"type":"number"}},"$ref":"#/$defs/value"}),
            json!({"properties":{"data":{"const":{"x-enumeray-file":true}}},
                "x-note":{"properties":{"file":{"x-enumeray-file":true}}}}),
        ] {
            model.definition.input_schema = schema;
            contract::validate_contract(&model.definition).unwrap();
            assert!(!requires_artifact_inputs(&model.definition));
            assert!(declarations(&model.definition).unwrap().is_empty());
        }
        let mut model = fixture();
        model.definition.input_schema["required"] = json!([]);
        assert!(requires_artifact_inputs(&model.definition));
        models::validate_inputs(&model.definition, &json!({})).unwrap();
        assert!(
            references(&model.definition, &json!({}))
                .unwrap()
                .is_empty()
        );
    }
}
