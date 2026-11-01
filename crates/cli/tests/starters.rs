use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn cli(config: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_enumeray"))
        .args(["--json", "--api", "://not-an-api", "--config"])
        .arg(config)
        .args(args)
        .env("ENUMERAY_TOKEN", "unused-secret")
        .output()
        .unwrap()
}
fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn source() -> Value {
    serde_json::from_str(include_str!("../examples/scalar-publication.json")).unwrap()
}

#[test]
fn every_starter_is_offline_and_uses_one_editable_publication_source() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("invalid-profile.json");
    fs::write(&config, b"not a profile").unwrap();
    for kind in ["python", "executable"] {
        for io in ["json", "files"] {
            for driver in ["process", "slurm"] {
                let id = format!("starter-{kind}-{io}-{driver}");
                let project = directory.path().join(&id);
                let created = success(cli(
                    &config,
                    &[
                        "model",
                        "init",
                        &id,
                        "--directory",
                        project.to_str().unwrap(),
                        "--kind",
                        kind,
                        "--io",
                        io,
                        "--driver",
                        driver,
                    ],
                ));
                assert_eq!(created["publication"], "publication.json");
                for removed in ["scientific.json", "binding.json", "binding.example.json"] {
                    assert!(!project.join(removed).exists());
                }
                assert_eq!(project.join("model.sbatch").exists(), driver == "slurm");
                assert_eq!(
                    project.join("demo_solver.py").exists(),
                    kind == "executable"
                );
                let file = project.join("publication.json");
                let mut publication: Value =
                    serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
                assert_eq!(publication["identities"]["model"], format!("{id}@v1"));
                let readme = fs::read_to_string(project.join("README.md")).unwrap();
                assert!(readme.contains(&format!("model create {id} --file publication.json")));
                assert!(readme.contains(&format!("analysis init {id}")));
                assert!(!readme.contains("{{"));
                assert!(!readme.contains("model create example"));
                assert_eq!(
                    readme.contains("--file dataset=fixtures/dataset.csv"),
                    io == "files"
                );
                assert!(readme.contains("model release review release-r1"));
                assert!(readme.contains("model release activate release-r1"));
                for guide in ["AUTHOR_GUIDE.md", "ANALYSIS_GUIDE.md"] {
                    assert!(project.join(guide).is_file());
                    assert!(readme.contains(&format!("]({guide})")));
                }
                assert_eq!(
                    fs::read_to_string(project.join("ANALYSIS_GUIDE.md")).unwrap(),
                    include_str!("../ANALYSIS_GUIDE.md")
                );
                assert!(publication.get("id").is_none());
                assert!(publication.get("spec").is_none());
                let validate = ["model", "validate", "--file", file.to_str().unwrap()];
                let incomplete = cli(&config, &validate);
                assert!(!incomplete.status.success());
                let error: Value = serde_json::from_slice(&incomplete.stderr).unwrap();
                assert!(
                    error["error"]["message"]
                        .as_str()
                        .unwrap()
                        .contains("context_id")
                );
                // Draft science and wrapper tests need neither credentials nor site settings.
                let check = Command::new("python3")
                    .args([
                        "-B",
                        "-m",
                        "unittest",
                        "discover",
                        "-s",
                        ".",
                        "-p",
                        "test_*.py",
                    ])
                    .current_dir(&project)
                    .env("PYTHONDONTWRITEBYTECODE", "1")
                    .output()
                    .unwrap();
                assert!(
                    check.status.success(),
                    "{kind}/{io}/{driver}: {}",
                    String::from_utf8_lossy(&check.stderr)
                );
                publication["execution"]["context_id"] = json!("lab-cpu");
                publication["execution"]["release_path"] = json!("/opt/enumeray/models/example/r1");
                fs::write(&file, publication.to_string()).unwrap();
                let saved = fs::read(&file).unwrap();
                let validated = success(cli(&config, &validate));
                assert_eq!(validated["model_id"], id);
                assert!(validated.get("publication_id").is_none());
                assert_eq!(validated["authorization_validation"], "requires_api");
                assert_eq!(validated["context_validation"], "requires_api");
                assert_eq!(validated["installation_validation"], "requires_connector");
                success(cli(&config, &validate));
                assert_eq!(fs::read(&file).unwrap(), saved);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    assert_eq!(
                        fs::metadata(project.join("wrapper.py"))
                            .unwrap()
                            .permissions()
                            .mode()
                            & 0o777,
                        0o755
                    );
                }
            }
        }
    }
}

#[test]
fn source_validation_rejects_invalid_settings_and_removed_identity_fields() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("absent");
    let file = directory.path().join("publication.json");
    let args = ["model", "validate", "--file", file.to_str().unwrap()];
    let good = source();
    for (field, value) in [
        ("context_id", Value::Null),
        ("release_path", Value::Null),
        ("release_path", json!("relative")),
        ("release_path", json!("/opt/../escape")),
        ("release_path", json!("/opt/release\0")),
        ("release_path", json!("/opt/release\n")),
        ("entrypoint", json!([])),
        ("entrypoint", json!(["/bin/wrapper"])),
        ("entrypoint", json!(["../wrapper"])),
        ("entrypoint", json!(["."])),
        ("entrypoint", json!(vec!["wrapper"; 33])),
        ("environment", json!({"bad=name":"value"})),
        ("environment", json!({"GOOD":"bad\0value"})),
        ("slurm_script", json!("../model.sbatch")),
        ("driver", json!("process")),
        ("binding_version", json!(1)),
        ("grant_id", json!("grant")),
        ("provider_binding_id", json!("key")),
        ("installation", json!({"model_identity":"duplicate"})),
    ] {
        let mut bad = good.clone();
        bad["execution"][field] = value;
        fs::write(&file, bad.to_string()).unwrap();
        assert!(!cli(&config, &args).status.success(), "{field}");
    }
    for field in [
        "id",
        "manifest_version",
        "kind",
        "spec",
        "scientific",
        "binding",
        "grant_id",
    ] {
        let mut bad = good.clone();
        bad[field] = json!("removed");
        fs::write(&file, bad.to_string()).unwrap();
        assert!(!cli(&config, &args).status.success(), "{field}");
    }
    let mut bad = good.clone();
    bad["examples"][0]["outputs"] = json!({"wrong":4});
    fs::write(&file, bad.to_string()).unwrap();
    assert!(!cli(&config, &args).status.success());
    let mut slurm = good;
    slurm["execution"]["slurm_script"] = json!("model.sbatch");
    fs::write(&file, slurm.to_string()).unwrap();
    assert_eq!(
        success(cli(&config, &args))["context_validation"],
        "requires_api"
    );
}

#[test]
fn source_validation_rejects_duplicate_keys_and_old_formats_without_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("absent");
    let file = directory.path().join("publication.json");
    for raw in [
        r#"{"publication_version":1,"publication_version":1}"#,
        r#"{"model":{},"definition":{}}"#,
        r#"{"binding_version":1,"context_id":"cpu"}"#,
        r#"{"manifest_version":1,"kind":"model-publication","spec":{}}"#,
    ] {
        fs::write(&file, raw).unwrap();
        assert!(
            !cli(
                &config,
                &["model", "validate", "--file", file.to_str().unwrap()]
            )
            .status
            .success()
        );
        assert_eq!(fs::read_to_string(&file).unwrap(), raw);
    }
    for args in [
        vec![
            "model",
            "manifest",
            "--project",
            ".",
            "--binding",
            "binding.json",
            "--output",
            "publication.json",
        ],
        vec!["model", "register", "example", "--file", "publication.json"],
        vec![
            "model",
            "release",
            "init",
            "example",
            "--project",
            ".",
            "--binding",
            "binding.json",
            "--directory",
            "release",
        ],
    ] {
        assert!(!cli(&config, &args).status.success());
    }
    assert!(!directory.path().join("release").exists());
}

#[test]
fn local_validation_rejects_future_file_protocols_but_keeps_scalar_protocols() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("absent");
    let file = directory.path().join("publication.json");
    let args = ["model", "validate", "--file", file.to_str().unwrap()];
    let mut scalar = source();
    scalar["wrapper_protocol_version"] = json!(2);
    fs::write(&file, scalar.to_string()).unwrap();
    success(cli(&config, &args));
    let schema = json!({"type":"object","properties":{"dataset":{"type":"object","properties":{"artifact_id":{"type":"string"}},"required":["artifact_id"],"additionalProperties":false,"x-enumeray-file":{"media_types":["text/csv"],"max_bytes":1048576}}},"required":["dataset"],"additionalProperties":false});
    for direction in ["input_schema", "output_schema"] {
        let mut publication = scalar.clone();
        publication[direction] = schema.clone();
        publication["examples"] = json!([]);
        fs::write(&file, publication.to_string()).unwrap();
        assert!(!cli(&config, &args).status.success(), "{direction}");
        publication["wrapper_protocol_version"] = json!(1);
        fs::write(&file, publication.to_string()).unwrap();
        success(cli(&config, &args));
    }
}

#[test]
fn local_validation_reports_protected_environment_keys_without_values() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("absent");
    let file = directory.path().join("publication.json");
    let args = ["model", "validate", "--file", file.to_str().unwrap()];
    for key in [
        "HOME",
        "PATH",
        "PYTHONDONTWRITEBYTECODE",
        "SLURM_NTASKS",
        "PMI_RANK",
        "PMIX_MCA_custom",
        "LD_PRELOAD",
        "DYLD_LIBRARY_PATH",
    ] {
        let mut candidate = source();
        candidate["execution"]["environment"] = json!({key:"private-env-value"});
        fs::write(&file, candidate.to_string()).unwrap();
        let output = cli(&config, &args);
        assert!(!output.status.success(), "{key}");
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        let message = error["error"]["message"].as_str().unwrap();
        assert!(message.contains(key), "{message}");
        assert!(!message.contains("private-env-value"), "{message}");
    }
    let mut valid = source();
    valid["execution"]["environment"] = json!({"OMP_NUM_THREADS":"4","DATASET_ROOT":"/data/r1"});
    fs::write(&file, valid.to_string()).unwrap();
    success(cli(&config, &args));
}

#[test]
fn invalid_scientific_ids_and_existing_destinations_preserve_the_project() {
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("project");
    let config = directory.path().join("absent");
    for id in ["Invalid", "bad/model"] {
        assert!(
            !cli(
                &config,
                &[
                    "model",
                    "init",
                    id,
                    "--directory",
                    project.to_str().unwrap()
                ]
            )
            .status
            .success()
        );
        assert!(!project.exists());
    }
    let args = [
        "model",
        "init",
        "example",
        "--directory",
        project.to_str().unwrap(),
    ];
    success(cli(&config, &args));
    let original = fs::read(project.join("publication.json")).unwrap();
    assert!(!cli(&config, &args).status.success());
    assert_eq!(
        fs::read(project.join("publication.json")).unwrap(),
        original
    );
}
#[cfg(unix)]
#[test]
fn symlink_destinations_are_never_followed() {
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("linked");
    let target = directory.path().join("absent-target");
    std::os::unix::fs::symlink(&target, &project).unwrap();
    assert!(
        !cli(
            &directory.path().join("absent"),
            &[
                "model",
                "init",
                "example",
                "--directory",
                project.to_str().unwrap()
            ]
        )
        .status
        .success()
    );
    assert!(project.is_symlink());
    assert!(!target.exists());
}
