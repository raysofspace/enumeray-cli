//! The supported scientific-schema profile and registration-time example checks.

use std::{collections::BTreeMap, error::Error, fmt};

use jsonschema::{PatternOptions, Validator};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use serde_json::{Value, json};

use super::{ModelValidationError, ScientificDefinition};

const DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";
// Bound reference chains as well as literal JSON nesting. The dependency's
// compiler can exhaust a thread's stack on long, shallow chains of local refs.
const MAX_SCHEMA_NODES: usize = 128;
const MAX_OUTPUTS: usize = 64;
const MAX_REGEX_BYTES: usize = 1024 * 1024;
const POINTER_ESCAPES: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'/')
    .remove(b'~')
    .remove(b'$')
    .remove(b'-')
    .remove(b'_')
    .remove(b'.');

/// Which scientific schema or example value needs correction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaKind {
    Input,
    Output,
}

impl fmt::Display for SchemaKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Input => "input",
            Self::Output => "output",
        })
    }
}

/// Contract errors contain only fixed categories and example positions.
///
/// Never retain the validator's errors: their messages, paths, and Debug output
/// may contain private example values, schema contents, or reference URLs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractError {
    UnsupportedContractVersion,
    InvalidSchema { schema: SchemaKind },
    UnsupportedDialect { schema: SchemaKind },
    UnsupportedReference { schema: SchemaKind },
    UnsupportedSchemaResource { schema: SchemaKind },
    TooManySchemaNodes { schema: SchemaKind },
    InvalidOutputDeclarations,
    InvalidFileDeclarations,
    DuplicateExampleName,
    InvalidExample { schema: SchemaKind, position: usize },
}

impl fmt::Display for ContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedContractVersion => formatter.write_str(
                "scientific contract version must be supported; currently only version 1 is supported"),
            Self::InvalidSchema { schema } => write!(formatter,
                "{schema} schema must be valid Draft 2020-12 with supported regular expressions and resolvable local references"),
            Self::UnsupportedDialect { schema } => write!(formatter,
                "{schema} schema may declare only the Draft 2020-12 dialect"),
            Self::UnsupportedReference { schema } => write!(formatter,
                "{schema} schema references must be document-local JSON Pointers to declared schema positions"),
            Self::UnsupportedSchemaResource { schema } => write!(formatter,
                "{schema} schema cannot use nested IDs, anchors, dynamic references, or custom vocabularies"),
            Self::TooManySchemaNodes { schema } => write!(formatter,
                "{schema} schema exceeds 128 schema nodes"),
            Self::InvalidOutputDeclarations => formatter.write_str(
                "output schema must declare a closed object with at most 64 named outputs and unconditional required names drawn from its properties"),
            Self::InvalidFileDeclarations => formatter.write_str("files require closed named inputs or outputs with direct declarations, supported media types and byte limits; schema references in file contracts are unsupported"),
            Self::DuplicateExampleName => formatter.write_str("scientific example names must be unique"),
            Self::InvalidExample { schema, position } => write!(formatter,
                "example {position} does not satisfy the {schema} schema"),
        }
    }
}

impl Error for ContractError {}

// Author parsing first checks document byte/depth limits and numeric bounds.
// Machine registration validates the same scientific definition before compilation.
pub fn validate_contract(definition: &ScientificDefinition) -> Result<(), ModelValidationError> {
    validate(definition).map_err(ModelValidationError::InvalidContract)
}

fn validate(definition: &ScientificDefinition) -> Result<(), ContractError> {
    match definition.contract_version {
        1 => validate_profile_1(definition),
        _ => Err(ContractError::UnsupportedContractVersion),
    }
}

/// Compile the immutable input contract using the same profile as registration.
pub(super) fn compile_inputs(
    definition: &ScientificDefinition,
) -> Result<Validator, ContractError> {
    match definition.contract_version {
        1 => {
            super::files::declarations(definition)?;
            crate::collections::declarations(definition, false)?;
            crate::collections::declarations(definition, true)?;
            compile_schema(&definition.input_schema, SchemaKind::Input)
        }
        _ => Err(ContractError::UnsupportedContractVersion),
    }
}

/// Compile the pinned output schema using the supported v1 contract.
pub fn compile_outputs(definition: &ScientificDefinition) -> Result<Validator, ContractError> {
    match definition.contract_version {
        1 => {
            super::files::declarations(definition)?;
            crate::collections::declarations(definition, false)?;
            crate::collections::declarations(definition, true)?;
            compile_schema(&definition.output_schema, SchemaKind::Output)
        }
        _ => Err(ContractError::UnsupportedContractVersion),
    }
}

// The unreleased v1 contract evolves with its implementations and fixtures.
// Only explicitly supported schema declarations establish file behavior.
fn validate_profile_1(definition: &ScientificDefinition) -> Result<(), ContractError> {
    let input = compile_inputs(definition)?;
    let output = compile_schema(&definition.output_schema, SchemaKind::Output)?;
    validate_output_declarations(&definition.output_schema)?;

    let mut names = std::collections::HashSet::new();
    for (index, example) in definition.examples.iter().enumerate() {
        if !names.insert(&example.name) {
            return Err(ContractError::DuplicateExampleName);
        }
        if super::files::references(definition, &example.inputs).is_err()
            || crate::collections::references(definition, &example.inputs).is_err()
        {
            return Err(ContractError::InvalidExample {
                schema: SchemaKind::Input,
                position: index + 1,
            });
        }
        for (schema, validator, value) in [
            (SchemaKind::Input, &input, Some(&example.inputs)),
            // The existing import format treats omitted/null outputs as absent.
            (
                SchemaKind::Output,
                &output,
                example.outputs.as_ref().filter(|value| !value.is_null()),
            ),
        ] {
            if value.is_some_and(|value| !validator.is_valid(value)) {
                return Err(ContractError::InvalidExample {
                    schema,
                    position: index + 1,
                });
            }
        }
    }
    Ok(())
}

fn schema_options() -> jsonschema::ValidationOptions<'static> {
    jsonschema::draft202012::options()
        .offline()
        .should_validate_formats(false)
        .with_pattern_options(
            PatternOptions::regex()
                .size_limit(MAX_REGEX_BYTES)
                .dfa_size_limit(MAX_REGEX_BYTES),
        )
}

fn compile_schema(document: &Value, kind: SchemaKind) -> Result<Validator, ContractError> {
    let invalid = || ContractError::InvalidSchema { schema: kind };
    let mut schemas = BTreeMap::new();
    collect_schemas(document, String::new(), kind, &mut schemas)?;
    for schema in schemas.values() {
        if let Some(reference) = schema.get("$ref") {
            let target = reference
                .as_str()
                .and_then(|reference| reference.strip_prefix('#'))
                .filter(|fragment| fragment.is_empty() || fragment.starts_with('/'))
                .and_then(|fragment| percent_decode_str(fragment).decode_utf8().ok());
            if !target.is_some_and(|target| schemas.contains_key(target.as_ref())) {
                return Err(ContractError::UnsupportedReference { schema: kind });
            }
        }
    }

    // Building checks the Draft 2020-12 meta-schema and compiles reachable rules.
    let validator = schema_options().build(document).map_err(|_| invalid())?;
    if schemas.len() > 1 {
        // Unused $defs and conditional branches can otherwise escape compilation.
        // Append probes to a clone without moving any JSON Pointer targets. This
        // validator is discarded; only the original rules validate examples.
        let mut probe = document.clone();
        let all_of = probe
            .as_object_mut()
            .ok_or_else(invalid)?
            .entry("allOf")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(invalid)?;
        for pointer in schemas.keys().filter(|pointer| !pointer.is_empty()) {
            let fragment = utf8_percent_encode(pointer, POINTER_ESCAPES);
            all_of.push(json!({"$ref": format!("#{fragment}")}));
        }
        schema_options().build(&probe).map_err(|_| invalid())?;
    }
    Ok(validator)
}

// Only these keyword values contain schemas. Annotation values, including
// defaults, examples, const, enum, and unknown keywords, remain ordinary data.
pub(crate) fn collect_schemas<'a>(
    value: &'a Value,
    pointer: String,
    kind: SchemaKind,
    schemas: &mut BTreeMap<String, &'a Value>,
) -> Result<(), ContractError> {
    if schemas.len() == MAX_SCHEMA_NODES {
        return Err(ContractError::TooManySchemaNodes { schema: kind });
    }
    let root = pointer.is_empty();
    schemas.insert(pointer.clone(), value);
    if value.is_boolean() {
        return Ok(());
    }
    let object = value
        .as_object()
        .ok_or(ContractError::InvalidSchema { schema: kind })?;
    if object
        .get("$schema")
        .is_some_and(|value| value.as_str() != Some(DIALECT))
    {
        return Err(ContractError::UnsupportedDialect { schema: kind });
    }
    if (!root && object.contains_key("$id"))
        || [
            "$anchor",
            "$dynamicAnchor",
            "$dynamicRef",
            "$recursiveAnchor",
            "$recursiveRef",
            "$vocabulary",
        ]
        .iter()
        .any(|keyword| object.contains_key(*keyword))
    {
        return Err(ContractError::UnsupportedSchemaResource { schema: kind });
    }

    for (keyword, child) in object {
        let child_pointer = join_pointer(&pointer, keyword);
        match keyword.as_str() {
            "$defs" | "definitions" | "properties" | "patternProperties" | "dependentSchemas" => {
                let children = child
                    .as_object()
                    .ok_or(ContractError::InvalidSchema { schema: kind })?;
                for (name, schema) in children {
                    collect_schemas(schema, join_pointer(&child_pointer, name), kind, schemas)?;
                }
            }
            "allOf" | "anyOf" | "oneOf" | "prefixItems" => {
                let children = child
                    .as_array()
                    .ok_or(ContractError::InvalidSchema { schema: kind })?;
                for (index, schema) in children.iter().enumerate() {
                    collect_schemas(
                        schema,
                        join_pointer(&child_pointer, &index.to_string()),
                        kind,
                        schemas,
                    )?;
                }
            }
            "additionalProperties"
            | "unevaluatedProperties"
            | "propertyNames"
            | "items"
            | "contains"
            | "unevaluatedItems"
            | "not"
            | "if"
            | "then"
            | "else"
            | "contentSchema" => {
                collect_schemas(child, child_pointer, kind, schemas)?;
            }
            _ => {}
        }
    }
    Ok(())
}

pub(crate) fn join_pointer(parent: &str, token: &str) -> String {
    format!("{parent}/{}", token.replace('~', "~0").replace('/', "~1"))
}

pub(crate) fn validate_output_declarations(schema: &Value) -> Result<(), ContractError> {
    let invalid = ContractError::InvalidOutputDeclarations;
    let object = schema.as_object().ok_or_else(|| invalid.clone())?;
    let properties = object
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid.clone())?;
    if schema["type"] != "object"
        || schema["additionalProperties"] != false
        || properties.len() > MAX_OUTPUTS
        || properties.keys().any(|name| {
            name.is_empty()
                || name.len() > 128
                || name.trim() != name
                || name.chars().any(char::is_control)
        })
        || object.keys().any(|keyword| !output_root_keyword(keyword))
    {
        return Err(invalid);
    }
    // The meta-schema already enforces a unique array of strings for required.
    if let Some(required) = object.get("required").and_then(Value::as_array)
        && required.iter().any(|name| {
            !name
                .as_str()
                .is_some_and(|name| properties.contains_key(name))
        })
    {
        return Err(invalid);
    }
    Ok(())
}

fn output_root_keyword(keyword: &str) -> bool {
    matches!(
        keyword,
        "$schema"
            | "$id"
            | "$defs"
            | "definitions"
            | "$comment"
            | "type"
            | "properties"
            | "required"
            | "additionalProperties"
            | "title"
            | "description"
            | "default"
            | "examples"
            | "deprecated"
            | "readOnly"
            | "writeOnly"
    ) || keyword.starts_with("x-")
}

#[cfg(test)]
mod tests;
