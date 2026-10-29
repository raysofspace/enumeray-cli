use super::*;

fn definition() -> ScientificDefinition {
    super::super::parse_publication(include_bytes!("../../../../../examples/scalar-double.json"))
        .expect("documented import fixture")
        .definition
}

#[test]
fn accepts_the_documented_scalar_contract() {
    assert_eq!(validate(&definition()), Ok(()));
}

#[test]
fn unsupported_contract_versions_never_fall_back_to_profile_one() {
    for version in [0, 2, u32::MAX] {
        let mut definition = definition();
        definition.contract_version = version;
        assert_eq!(
            validate(&definition),
            Err(ContractError::UnsupportedContractVersion)
        );
    }
}

#[test]
fn unknown_annotations_and_defaults_remain_inert() {
    let mut definition = definition();
    definition.input_schema["properties"]["value"]["x-example-file"] =
        json!({"$ref": "https://example.invalid/file"});
    definition.input_schema["properties"]["value"]["default"] = json!(2);
    definition.input_schema["x-enumeray-defaults"] = json!({"value": 2});
    assert_eq!(validate(&definition), Ok(()));
    definition.examples[0].inputs = json!({});
    assert_eq!(
        validate(&definition),
        Err(ContractError::InvalidExample {
            schema: SchemaKind::Input,
            position: 1,
        })
    );
}

#[test]
fn invalid_schemas_are_rejected_even_without_examples() {
    for invalid in [
        json!({"type": "unknown"}),
        json!({"minimum": "zero"}),
        json!({"required": "value"}),
        json!({"required": ["value", "value"]}),
        json!({"$defs": {"unused": {"type": 7}}}),
        json!({"items": [{"type": "string"}]}),
    ] {
        let mut definition = definition();
        definition.examples.clear();
        definition.input_schema = invalid;
        assert!(matches!(
            validate(&definition),
            Err(ContractError::InvalidSchema {
                schema: SchemaKind::Input
            })
        ));
    }
}

#[test]
fn rejects_foreign_dialects_at_every_schema_position() {
    for dialect in [
        json!("http://json-schema.org/draft-07/schema#"),
        json!("https://example.invalid/custom"),
        json!(5),
    ] {
        for nested in [false, true] {
            let mut definition = definition();
            definition.input_schema = if nested {
                json!({"$defs": {"unused": {"$schema": dialect}}})
            } else {
                json!({"$schema": dialect})
            };
            assert_eq!(
                validate(&definition),
                Err(ContractError::UnsupportedDialect {
                    schema: SchemaKind::Input
                })
            );
        }
    }
}

#[test]
fn permits_local_pointer_refs_and_root_schema_identity() {
    let mut definition = definition();
    definition.input_schema = json!({
        "$id": "https://schemas.example.invalid/scalar-input",
        "$defs": {"number": {"type": "number"}},
        "type": "object",
        "properties": {"value": {"$ref": "#/$defs/number"}},
        "required": ["value"]
    });
    assert_eq!(validate(&definition), Ok(()));
    definition.examples[0].inputs = json!({"value": "wrong"});
    assert_eq!(
        validate(&definition),
        Err(ContractError::InvalidExample {
            schema: SchemaKind::Input,
            position: 1
        })
    );
}

#[test]
fn decodes_uri_fragments_and_json_pointer_tokens() {
    let mut definition = definition();
    let name = "a/b~c #%α";
    let pointer = join_pointer("/$defs", name);
    let reference = format!("#{}", utf8_percent_encode(&pointer, POINTER_ESCAPES));
    definition.input_schema = json!({
        "$defs": {(name): {"type": "number"}},
        "properties": {"value": {"$ref": reference}}
    });
    assert_eq!(validate(&definition), Ok(()));
}

#[test]
fn rejects_external_and_missing_references_in_unused_definitions() {
    for reference in [
        "https://example.invalid/private-schema",
        "http://127.0.0.1:1/schema",
        "file:///tmp/private-schema.json",
        "other.json",
        "/other.json",
        "//example.invalid/schema",
        "",
        "#/$defs/missing",
        "#named-anchor",
        "#%FF",
        "#%2F$defs%2Fmissing",
    ] {
        let mut definition = definition();
        definition.input_schema = json!({"$defs": {"unused": {"$ref": reference}}});
        assert_eq!(
            validate(&definition),
            Err(ContractError::UnsupportedReference {
                schema: SchemaKind::Input
            })
        );
    }
}

#[test]
fn validates_subschemas_in_every_supported_keyword_position() {
    for keyword in [
        "$defs",
        "definitions",
        "properties",
        "patternProperties",
        "dependentSchemas",
    ] {
        let mut definition = definition();
        definition.input_schema = json!({(keyword): {"unused": {"$ref": "file:///tmp/schema"}}});
        assert_eq!(
            validate(&definition),
            Err(ContractError::UnsupportedReference {
                schema: SchemaKind::Input
            }),
            "{keyword}"
        );
    }
    for keyword in ["allOf", "anyOf", "oneOf", "prefixItems"] {
        let mut definition = definition();
        definition.input_schema = json!({(keyword): [{"$ref": "file:///tmp/schema"}]});
        assert_eq!(
            validate(&definition),
            Err(ContractError::UnsupportedReference {
                schema: SchemaKind::Input
            }),
            "{keyword}"
        );
    }
    for keyword in [
        "additionalProperties",
        "unevaluatedProperties",
        "propertyNames",
        "items",
        "contains",
        "unevaluatedItems",
        "not",
        "if",
        "then",
        "else",
        "contentSchema",
    ] {
        let mut definition = definition();
        definition.input_schema = json!({(keyword): {"$ref": "file:///tmp/schema"}});
        assert_eq!(
            validate(&definition),
            Err(ContractError::UnsupportedReference {
                schema: SchemaKind::Input
            }),
            "{keyword}"
        );
    }
}

#[test]
fn ordinary_json_data_with_reference_keys_is_not_a_schema() {
    let mut definition = definition();
    let data = json!({"$ref": "file:///tmp/not-a-schema", "$schema": "not-a-dialect"});
    definition.input_schema = json!({
        "const": data, "default": data, "examples": [data], "x-documentation": data
    });
    definition.examples[0].inputs = data;
    assert_eq!(validate(&definition), Ok(()));
}

#[test]
fn refs_cannot_turn_annotation_data_into_executable_schemas() {
    for (keyword, value, reference) in [
        (
            "const",
            json!({"$ref": "https://example.invalid/secret"}),
            "#/const",
        ),
        ("default", json!({"type": "string"}), "#/default"),
        ("examples", json!([{"type": "string"}]), "#/examples/0"),
        ("x-annotation", json!({"type": "string"}), "#/x-annotation"),
    ] {
        let mut definition = definition();
        definition.input_schema = json!({(keyword): value, "$ref": reference});
        assert_eq!(
            validate(&definition),
            Err(ContractError::UnsupportedReference {
                schema: SchemaKind::Input
            })
        );
    }
}

#[test]
fn rejects_unsupported_resource_scopes_even_when_unused() {
    for schema in [
        json!({"$id": "nested"}),
        json!({"$anchor": "node"}),
        json!({"$dynamicRef": "#"}),
        json!({"$dynamicAnchor": "node"}),
        json!({"$recursiveRef": "#"}),
        json!({"$recursiveAnchor": true}),
        json!({"$vocabulary": {"https://example.invalid/vocabulary": true}}),
    ] {
        let mut definition = definition();
        definition.input_schema = json!({"$defs": {"unused": schema}});
        assert_eq!(
            validate(&definition),
            Err(ContractError::UnsupportedSchemaResource {
                schema: SchemaKind::Input
            })
        );
    }
}

#[test]
fn compilation_reaches_unused_patterns_and_does_not_change_validation() {
    let mut definition = definition();
    // These would reject the example if compile-only probes were used to validate it.
    definition.input_schema["$defs"] =
        json!({"unused_false": false, "unused_string": {"type": "string"}});
    definition.input_schema["then"] = json!({"type": "string"});
    assert_eq!(validate(&definition), Ok(()));

    for pattern in ["[", "(?=a)a", r"(a)\1", "a{10000000}"] {
        definition.input_schema["$defs"]["unused_pattern"] = json!({"pattern": pattern});
        assert!(matches!(
            validate(&definition),
            Err(ContractError::InvalidSchema {
                schema: SchemaKind::Input
            })
        ));
    }
}

#[test]
fn local_recursive_schemas_validate_finite_examples() {
    let mut definition = definition();
    definition.input_schema = json!({
        "type": "object", "properties": {"child": {"$ref": "#"}}, "additionalProperties": false
    });
    definition.examples[0].inputs = json!({"child": {"child": {}}});
    assert_eq!(validate(&definition), Ok(()));
    definition.examples[0].inputs = json!({"child": "invalid"});
    assert!(matches!(
        validate(&definition),
        Err(ContractError::InvalidExample {
            schema: SchemaKind::Input,
            ..
        })
    ));
}

#[test]
fn schema_node_count_is_bounded() {
    let mut definition = definition();
    definition.input_schema = json!({"$defs": (0..MAX_SCHEMA_NODES).map(|index| (index.to_string(), json!(true))).collect::<serde_json::Map<_, _>>()});
    assert_eq!(
        validate(&definition),
        Err(ContractError::TooManySchemaNodes {
            schema: SchemaKind::Input
        })
    );
}

#[test]
fn reference_chains_are_bounded_before_compilation() {
    let mut definition = definition();
    definition.examples.clear();
    for count in [MAX_SCHEMA_NODES - 1, 512] {
        let mut definitions = serde_json::Map::new();
        for index in 0..count {
            let schema = if index + 1 == count {
                json!({"type": "number"})
            } else {
                json!({"$ref": format!("#/$defs/{}", index + 1)})
            };
            definitions.insert(index.to_string(), schema);
        }
        definition.input_schema = json!({"$defs": definitions, "$ref": "#/$defs/0"});
        let result = validate(&definition);
        if count < MAX_SCHEMA_NODES {
            assert_eq!(result, Ok(()));
        } else {
            assert_eq!(
                result,
                Err(ContractError::TooManySchemaNodes {
                    schema: SchemaKind::Input
                })
            );
        }
    }
}

#[test]
fn output_requiredness_is_explicit_and_optional_values_are_validated() {
    let mut definition = definition();
    definition.output_schema["properties"]["note"] = json!({"type": "string"});
    assert_eq!(validate(&definition), Ok(()));
    definition.examples[0].outputs = Some(json!({"doubled": 4, "note": "optional"}));
    assert_eq!(validate(&definition), Ok(()));
    for invalid in [
        json!({}),
        json!({"doubled": 4, "note": 7}),
        json!({"doubled": 4, "unexpected": true}),
    ] {
        definition.examples[0].outputs = Some(invalid);
        assert_eq!(
            validate(&definition),
            Err(ContractError::InvalidExample {
                schema: SchemaKind::Output,
                position: 1
            })
        );
    }
}

#[test]
fn no_outputs_and_all_optional_outputs_are_supported() {
    let mut definition = definition();
    definition.output_schema =
        json!({"type": "object", "properties": {}, "additionalProperties": false});
    definition.examples[0].outputs = Some(json!({}));
    assert_eq!(validate(&definition), Ok(()));
    definition.output_schema["properties"] = json!({"optional": {"type": "number"}});
    assert_eq!(validate(&definition), Ok(()));
}

#[test]
fn property_schemas_can_use_composition_and_bounded_collections() {
    let mut definition = definition();
    definition.output_schema["properties"]["doubled"] = json!({
        "allOf": [{"type": "number"}, {"minimum": 0}]
    });
    definition.output_schema["properties"]["series"] = json!({
        "type": "array", "maxItems": 3, "items": {"type": "number"}
    });
    definition.examples[0].outputs = Some(json!({"doubled": 4, "series": [1, 2]}));
    assert_eq!(validate(&definition), Ok(()));
    definition.examples[0].outputs = Some(json!({"doubled": 4, "series": [1, 2, 3, 4]}));
    assert!(matches!(
        validate(&definition),
        Err(ContractError::InvalidExample {
            schema: SchemaKind::Output,
            ..
        })
    ));
}

#[test]
fn output_declaration_errors_are_rejected_without_examples() {
    for schema in [
        json!(true),
        json!({"type": "number"}),
        json!({"type": "object", "properties": {}}),
        json!({"type": "object", "properties": {}, "additionalProperties": true}),
        json!({"type": "object", "properties": {}, "additionalProperties": false, "required": ["typo"]}),
        json!({"type": "object", "properties": {"": true}, "additionalProperties": false}),
        json!({"type": "object", "properties": {" outer-space": true}, "additionalProperties": false}),
    ] {
        let mut definition = definition();
        definition.examples.clear();
        definition.output_schema = schema;
        assert_eq!(
            validate(&definition),
            Err(ContractError::InvalidOutputDeclarations)
        );
    }
}

#[test]
fn output_root_cannot_hide_requiredness_in_other_constraints() {
    for (keyword, value) in [
        ("allOf", json!([true])),
        ("anyOf", json!([true])),
        ("oneOf", json!([true])),
        ("if", json!(true)),
        ("then", json!(true)),
        ("else", json!(true)),
        ("not", json!(false)),
        ("dependentRequired", json!({})),
        ("dependentSchemas", json!({})),
        ("patternProperties", json!({})),
        ("propertyNames", json!(true)),
        ("minProperties", json!(1)),
        ("maxProperties", json!(1)),
        ("const", json!({})),
        ("enum", json!([{}])),
        ("$ref", json!("#")),
    ] {
        let mut definition = definition();
        definition.examples.clear();
        definition.output_schema[keyword] = value;
        assert_eq!(
            validate(&definition),
            Err(ContractError::InvalidOutputDeclarations),
            "{keyword}"
        );
    }
}

#[test]
fn output_count_and_name_size_are_bounded() {
    for properties in [
        (0..=MAX_OUTPUTS)
            .map(|index| (format!("output{index}"), json!(true)))
            .collect::<serde_json::Map<_, _>>(),
        [("a".repeat(129), json!(true))].into_iter().collect(),
    ] {
        let mut definition = definition();
        definition.examples.clear();
        definition.output_schema =
            json!({"type": "object", "properties": properties, "additionalProperties": false});
        assert_eq!(
            validate(&definition),
            Err(ContractError::InvalidOutputDeclarations)
        );
    }
}

#[test]
fn example_names_are_unique_and_errors_identify_the_position() {
    let mut definition = definition();
    definition.examples.push(definition.examples[0].clone());
    assert_eq!(
        validate(&definition),
        Err(ContractError::DuplicateExampleName)
    );
    definition.examples[1].name = "second".into();
    definition.examples[1].inputs = json!({"value": "invalid"});
    assert_eq!(
        validate(&definition),
        Err(ContractError::InvalidExample {
            schema: SchemaKind::Input,
            position: 2
        })
    );
}

#[test]
fn omitted_or_null_example_outputs_do_not_claim_a_result() {
    let mut definition = definition();
    for absent in [None, Some(Value::Null)] {
        definition.examples[0].outputs = absent;
        assert_eq!(validate(&definition), Ok(()));
    }
}

#[test]
fn defaults_do_not_fill_inputs_and_annotations_do_not_assert_formats() {
    let mut definition = definition();
    definition.input_schema["properties"]["value"]["default"] = json!(2);
    definition.examples[0].inputs = json!({});
    assert!(matches!(
        validate(&definition),
        Err(ContractError::InvalidExample {
            schema: SchemaKind::Input,
            ..
        })
    ));
    assert_eq!(definition.examples[0].inputs, json!({}));

    definition.input_schema = json!({"type": "string", "format": "email", "default": 123});
    definition.examples[0].inputs = json!("not an email");
    assert_eq!(validate(&definition), Ok(()));
}

#[test]
fn preserves_exact_numeric_constraints_and_examples() {
    let mut definition = definition();
    definition.input_schema =
        serde_json::from_str(r#"{"minimum":90071992547409921234567890.123456790}"#).unwrap();
    definition.examples[0].inputs =
        serde_json::from_str("90071992547409921234567890.123456789").unwrap();
    assert!(matches!(
        validate(&definition),
        Err(ContractError::InvalidExample {
            schema: SchemaKind::Input,
            ..
        })
    ));
    definition.examples[0].inputs =
        serde_json::from_str("90071992547409921234567890.123456790").unwrap();
    assert_eq!(validate(&definition), Ok(()));

    definition.input_schema = serde_json::from_str(r#"{"multipleOf":0.1}"#).unwrap();
    definition.examples[0].inputs = serde_json::from_str("0.3").unwrap();
    assert_eq!(validate(&definition), Ok(()));
}

#[test]
fn diagnostics_do_not_expose_private_schema_or_example_values() {
    let mut definition = definition();
    definition.examples[0].name = "private-example-name".into();
    definition.examples[0].inputs = json!({"value": "private-input-value"});
    let error = validate(&definition).unwrap_err();
    for message in [error.to_string(), format!("{error:?}")] {
        assert!(!message.contains("private-"));
    }
    definition.input_schema = json!({"$ref": "https://example.invalid/private-reference"});
    let error = validate(&definition).unwrap_err();
    for message in [error.to_string(), format!("{error:?}")] {
        assert!(!message.contains("example.invalid"));
        assert!(!message.contains("private-reference"));
    }
}
