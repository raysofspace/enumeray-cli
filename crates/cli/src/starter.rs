//! Credential-free author scaffolding. These are local files, not a deployment API.

use crate::args::{StarterDriver, StarterIo, StarterKind};
use enumeray_client::Error;
use enumeray_contracts::models::{
    ModelMetadata, ParsedPublication, PublicationExecution, ScientificDefinition, ScientificExample,
};
use serde_json::{Value, json};
use std::{fs, io::Write, path::Path};

fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}

fn file_schema() -> Value {
    json!({"type":"object","properties":{"artifact_id":{"type":"string"}},
        "required":["artifact_id"],"additionalProperties":false,
        "x-enumeray-file":{"media_types":["text/csv"],"max_bytes":1048576}})
}

pub(crate) fn init(
    id: &str,
    directory: &Path,
    kind: StarterKind,
    io: StarterIo,
    driver: StarterDriver,
) -> Result<Value, Error> {
    let files = matches!(io, StarterIo::Files);
    let artifact = "55555555-5555-4555-8555-555555555555";
    let inputs = if files {
        json!({"dataset":{"artifact_id":artifact}})
    } else {
        json!({"value":2})
    };
    let outputs = if files {
        json!({"sample_count":2,"processed_dataset":{"artifact_id":artifact}})
    } else {
        json!({"doubled":4})
    };
    let slurm = matches!(driver, StarterDriver::Slurm);
    let source = ParsedPublication {
        model: ModelMetadata {
            id: id.into(),
            name: id.into(),
            description: "Editable protocol demonstration; replace with your scientific model."
                .into(),
        },
        definition: ScientificDefinition {
            log_files: Default::default(),
            contract_version: 1,
            model_identity: format!("{id}@v1"),
            wrapper_identity: format!("{id}-wrapper@v1"),
            runtime_identity: "author-example:python3-standard-library:1".into(),
            wrapper_protocol_version: 1,
            dataset_identities: vec![],
            input_schema: json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object",
                "properties":if files { json!({"dataset":file_schema()}) } else {json!({"value":{"type":"number"}})},
                "required":if files {vec!["dataset"]} else {vec!["value"]},"additionalProperties":false}),
            output_schema: json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object",
                "properties":if files {json!({"sample_count":{"type":"integer"},"processed_dataset":file_schema()})}
                    else {json!({"doubled":{"type":"number"}})},
                "required":if files {vec!["sample_count","processed_dataset"]} else {vec!["doubled"]},"additionalProperties":false}),
            examples: vec![ScientificExample {
                name: "starter-example".into(),
                inputs: inputs.clone(),
                outputs: Some(outputs),
            }],
        },
        execution: PublicationExecution {
            context_id: None,
            release_path: None,
            entrypoint: vec!["wrapper.py".into(), "execute".into()],
            slurm_script: slurm.then(|| "model.sbatch".into()),
            environment: Default::default(),
        },
    };
    source
        .validate()
        .map_err(|error| invalid(&error.to_string()))?;
    let mut publication = source.author_document();
    // Teach the author shape without repeating its fixed protocol and empty defaults.
    publication
        .as_object_mut()
        .expect("object")
        .remove("contract_version");
    publication
        .as_object_mut()
        .expect("object")
        .remove("wrapper_protocol_version");
    publication["identities"]
        .as_object_mut()
        .expect("object")
        .remove("datasets");
    publication["execution"]
        .as_object_mut()
        .expect("object")
        .remove("environment");
    publication["execution"]["context_id"] = Value::Null;
    publication["execution"]["release_path"] = Value::Null;
    let mut request = json!({"wrapper_protocol_version":1,"run_id":"11111111-1111-4111-8111-111111111111",
        "assignment_id":"22222222-2222-4222-8222-222222222222","attempt_id":"33333333-3333-4333-8333-333333333333",
        "launch_intent_id":"44444444-4444-4444-8444-444444444444","model_id":id,"publication_id":"starter-publication",
        "inputs":inputs,"input_files":{}});
    if files {
        request["input_files"] =
            json!({"dataset":{"artifact_id":artifact,"path":"inputs/dataset.csv"}});
    }
    let mut expected = request.clone();
    for field in ["model_id", "publication_id", "inputs", "input_files"] {
        expected.as_object_mut().expect("object").remove(field);
    }
    expected["outcome"] = json!("succeeded");
    expected["failure"] = Value::Null;
    expected["outputs"] = if files {
        json!({"sample_count":2})
    } else {
        json!({"doubled":4})
    };
    expected["files"] = if files {
        json!({"processed_dataset":{"path":"processed.csv","media_type":"text/csv","scientific_metadata":{}}})
    } else {
        json!({})
    };
    let pretty = |value: &Value| -> Result<String, Error> {
        Ok(serde_json::to_string_pretty(value)? + "\n")
    };
    let mut assets = vec![
        ("publication.json", pretty(&publication)?, false),
        ("fixtures/request.json", pretty(&request)?, false),
        ("fixtures/expected-result.json", pretty(&expected)?, false),
        (
            "protocol.py",
            include_str!("../starter-kit/protocol.py").into(),
            false,
        ),
        (
            "wrapper.py",
            include_str!("../starter-kit/wrapper.py").into(),
            true,
        ),
        (
            "model.py",
            include_str!("../starter-kit/model.py").into(),
            false,
        ),
        (
            "implementation.py",
            match kind {
                StarterKind::Python => include_str!("../starter-kit/python.py"),
                StarterKind::Executable => include_str!("../starter-kit/executable.py"),
            }
            .into(),
            false,
        ),
        (
            "test_starter.py",
            include_str!("../starter-kit/test_starter.py").into(),
            false,
        ),
        (
            "test_protocol.py",
            include_str!("../starter-kit/test_protocol.py").into(),
            false,
        ),
        (
            "README.md",
            include_str!("../starter-kit/README.md")
                .replace("{{MODEL_ID}}", id)
                .replace(
                    "{{ANALYSIS_BINDINGS}}",
                    if files {
                        " --file dataset=fixtures/dataset.csv"
                    } else {
                        ""
                    },
                ),
            false,
        ),
        (
            "AUTHOR_GUIDE.md",
            include_str!("../AUTHOR_GUIDE.md").into(),
            false,
        ),
        (
            "ANALYSIS_GUIDE.md",
            include_str!("../ANALYSIS_GUIDE.md").into(),
            false,
        ),
    ];
    if files {
        assets.push((
            "fixtures/dataset.csv",
            "index,value\n0,1\n1,2\n".into(),
            false,
        ));
        assets.push((
            "fixtures/expected.csv",
            "index,value\n0,2\n1,4\n".into(),
            false,
        ));
    }
    if matches!(kind, StarterKind::Executable) {
        assets.push((
            "demo_solver.py",
            include_str!("../starter-kit/demo_solver.py").into(),
            true,
        ));
    }
    if slurm {
        assets.push((
            "model.sbatch",
            include_str!("../starter-kit/model.sbatch").into(),
            true,
        ));
    }
    // create_dir exclusively claims this destination, including against symlinks.
    fs::create_dir(directory)?;
    let mut created_files = Vec::new();
    let mut created_dirs = Vec::new();
    let result = (|| -> Result<(), Error> {
        fs::create_dir(directory.join("fixtures"))?;
        created_dirs.push(directory.join("fixtures"));
        for (name, content, executable) in &assets {
            let path = directory.join(name);
            let mut target = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            created_files.push(path.clone());
            target.write_all(content.as_bytes())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                target.set_permissions(fs::Permissions::from_mode(if *executable {
                    0o755
                } else {
                    0o644
                }))?;
            }
            #[cfg(not(unix))]
            let _ = executable;
        }
        Ok(())
    })();
    if let Err(error) = result {
        for path in created_files.iter().rev() {
            let _ = fs::remove_file(path);
        }
        for path in created_dirs.iter().rev() {
            let _ = fs::remove_dir(path);
        }
        let _ = fs::remove_dir(directory);
        return Err(error);
    }
    Ok(
        json!({"project":directory,"model_id":id,"files":assets.iter().map(|a|a.0).collect::<Vec<_>>(),"publication":"publication.json"}),
    )
}
