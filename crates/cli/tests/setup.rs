//! Saved onboarding stays reviewable and refuses unsafe recovery before mutations.
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::{
        fs::{MetadataExt, PermissionsExt, chown},
        process::CommandExt,
    },
    path::Path,
    process::{Command, Output},
};
const SECRET: &str = "setup-user-token-never-reflected";
fn command(root: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_enumeray"));
    c.args(["--json", "--config"])
        .arg(root)
        .env("ENUMERAY_TOKEN", SECRET);
    c
}
fn input(root: &Path, kind: &str) -> std::path::PathBuf {
    let host: Value = serde_json::from_str(match kind {
        "linux" => include_str!("../examples/host-setup-linux.json"),
        "macos" => include_str!("../examples/host-setup-macos.json"),
        _ => include_str!("../examples/host-setup-development.json"),
    })
    .unwrap();
    let file = root.join(format!("{kind}.json"));
    fs::write(&file,serde_json::to_vec(&json!({"manifest_version":1,"connector":{"id":"lab-connector","label":"Lab","max_outstanding_assignments":1},"host":host,"startup":"foreground"})).unwrap()).unwrap();
    file
}
fn json(output: &Output) -> Value {
    assert!(!String::from_utf8_lossy(&output.stdout).contains(SECRET));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(SECRET));
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&output.stderr)))
}
#[test]
fn both_modes_save_one_private_review_plan_without_profiles_or_requests() {
    let root = tempfile::tempdir().unwrap();
    for kind in ["linux", "macos", "development"] {
        let file = input(root.path(), kind);
        let directory = root.path().join(format!("setup-{kind}"));
        let call = || {
            command(root.path())
                .args(["setup", "--non-interactive", "--directory"])
                .arg(&directory)
                .arg("--file")
                .arg(&file)
                .output()
                .unwrap()
        };
        let first = call();
        assert_eq!(first.status.code(), Some(1));
        let first = json(&first);
        assert_eq!(first["status"], "review_required");
        let saved = fs::read(directory.join("setup.json")).unwrap();
        assert_eq!(json(&call())["plan_sha256"], first["plan_sha256"]);
        assert_eq!(fs::read(directory.join("setup.json")).unwrap(), saved);
        for entry in fs::read_dir(&directory).unwrap() {
            assert_eq!(
                entry.unwrap().metadata().unwrap().permissions().mode() & 0o077,
                0
            );
        }
        assert_eq!(
            fs::metadata(directory).unwrap().permissions().mode() & 0o077,
            0
        );
    }
}
#[test]
fn saved_local_inputs_cannot_change_with_the_resume_working_directory() {
    let root = tempfile::tempdir().unwrap();
    let file = input(root.path(), "development");
    let mut document: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    document["first_model"] = json!({
        "model_id":"first-model", "demo":true, "example":"starter-example",
        "files":["data=relative/input.csv"]
    });
    fs::write(&file, serde_json::to_vec(&document).unwrap()).unwrap();
    let directory = root.path().join("journey");
    let call = || {
        command(root.path())
            .args(["setup", "--non-interactive", "--directory"])
            .arg(&directory)
            .arg("--file")
            .arg(&file)
            .output()
            .unwrap()
    };
    let rejected = call();
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("absolute/path"));
    assert!(!directory.join("setup.json").exists());
    document["first_model"]["files"] = json!(["data=/absolute/input.csv"]);
    fs::write(&file, serde_json::to_vec(&document).unwrap()).unwrap();
    assert_eq!(json(&call())["status"], "review_required");
}
#[test]
fn cross_platform_ssh_requires_and_freezes_its_native_artifact_approval() {
    let root = tempfile::tempdir().unwrap();
    let file = input(root.path(), "linux");
    let mut document: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    document["host"]["connector_account"]["platform"] = json!("macos");
    document["host"]["connector_account"]["create"] = json!(false);
    let execution_user = document["host"]["execution_account"]["name"].clone();
    document["host"]["context"]["definition"]["connection"] = json!({"kind":"ssh","host":"127.0.0.1","port":2222,"user":execution_user,"credential_reference":"site-key","runner_path":"/opt/enumeray/bin/enumeray"});
    document["host"]["credentials_file"] = json!("/var/lib/enumeray/ssh.json");
    let directory = root.path().join("journey");
    let call = || {
        command(root.path())
            .args(["setup", "--non-interactive", "--directory"])
            .arg(&directory)
            .arg("--file")
            .arg(&file)
            .output()
            .unwrap()
    };
    fs::write(&file, serde_json::to_vec(&document).unwrap()).unwrap();
    let rejected = call();
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("execution executable SHA-256"));
    document["host"]["execution_executable_sha256"] = json!("a".repeat(64));
    fs::write(&file, serde_json::to_vec(&document).unwrap()).unwrap();
    assert_eq!(json(&call())["status"], "review_required");
    document["host"]["execution_executable_sha256"] = json!("b".repeat(64));
    fs::write(&file, serde_json::to_vec(&document).unwrap()).unwrap();
    let changed = call();
    assert!(!changed.status.success());
    assert!(String::from_utf8_lossy(&changed.stderr).contains("differs"));
}
#[test]
fn wrong_approval_changed_input_and_tampered_evidence_are_refused() {
    let root = tempfile::tempdir().unwrap();
    let file = input(root.path(), "development");
    let directory = root.path().join("setup");
    let initial = command(root.path())
        .args(["setup", "--non-interactive", "--directory"])
        .arg(&directory)
        .arg("--file")
        .arg(&file)
        .output()
        .unwrap();
    json(&initial);
    let before = fs::read(directory.join("state.json")).unwrap();
    let wrong = command(root.path())
        .args(["setup", "--non-interactive", "--directory"])
        .arg(&directory)
        .args(["--approve", "incorrect"])
        .output()
        .unwrap();
    assert!(!wrong.status.success());
    assert_eq!(fs::read(directory.join("state.json")).unwrap(), before);
    let mut changed: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    changed["startup"] = json!("background");
    fs::write(&file, serde_json::to_vec(&changed).unwrap()).unwrap();
    let changed = command(root.path())
        .args(["setup", "--non-interactive", "--directory"])
        .arg(&directory)
        .arg("--file")
        .arg(&file)
        .output()
        .unwrap();
    assert!(!changed.status.success());
    assert!(String::from_utf8_lossy(&changed.stderr).contains("differs"));
    let mut plan: Value =
        serde_json::from_slice(&fs::read(directory.join("setup.json")).unwrap()).unwrap();
    plan["input"]["startup"] = json!("background");
    fs::write(
        directory.join("setup.json"),
        serde_json::to_vec(&plan).unwrap(),
    )
    .unwrap();
    let tampered = command(root.path())
        .args(["setup", "--directory"])
        .arg(directory)
        .output()
        .unwrap();
    assert!(!tampered.status.success());
    assert!(String::from_utf8_lossy(&tampered.stderr).contains("evidence changed"));
}
#[test]
fn development_approval_never_implies_execution_consent() {
    let root = tempfile::tempdir().unwrap();
    let file = input(root.path(), "development");
    let directory = root.path().join("setup");
    let first = command(root.path())
        .args(["setup", "--non-interactive", "--directory"])
        .arg(&directory)
        .arg("--file")
        .arg(file)
        .output()
        .unwrap();
    let first = json(&first);
    let approved = command(root.path())
        .args(["setup", "--non-interactive", "--directory"])
        .arg(&directory)
        .arg("--approve")
        .arg(first["plan_sha256"].as_str().unwrap())
        .output()
        .unwrap();
    assert_eq!(json(&approved)["status"], "development_consent_required");
    let state: Value =
        serde_json::from_slice(&fs::read(directory.join("state.json")).unwrap()).unwrap();
    assert_eq!(state["approved"], true);
    assert_eq!(state["development_consent"], false);
    assert_eq!(state["connector_started"], false);
}
#[test]
fn host_apply_ignores_profiles_and_rejects_unapproved_or_modified_actions() {
    let root = tempfile::tempdir().unwrap();
    let file = input(root.path(), "linux");
    let root_runner = fs::metadata(root.path()).unwrap().uid() == 0;
    let executable = if root_runner {
        // Root is allowed to apply an approved managed plan. Exercise the
        // unprivileged boundary even in root-run CI, without preparing its host.
        // Copy the binary because Cargo's target directory may be root-private.
        let executable = root.path().join("enumeray");
        fs::copy(env!("CARGO_BIN_EXE_enumeray"), &executable).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        for path in [root.path(), file.as_path()] {
            chown(path, Some(65534), Some(65534)).unwrap();
        }
        executable
    } else {
        env!("CARGO_BIN_EXE_enumeray").into()
    };
    let user_command = || {
        let mut c = Command::new(&executable);
        c.args(["--json", "--config"])
            .arg(root.path())
            .env("ENUMERAY_TOKEN", SECRET);
        if root_runner {
            c.uid(65534).gid(65534);
        }
        c
    };
    let directory = root.path().join("setup");
    let first = user_command()
        .args(["setup", "--non-interactive", "--directory"])
        .arg(&directory)
        .arg("--file")
        .arg(file)
        .output()
        .unwrap();
    let first = json(&first);
    let plan = directory.join("host-plan.json");
    let apply = |approval: &str| {
        user_command()
            .args(["setup", "apply-host", "--plan"])
            .arg(&plan)
            .args(["--side", "connector", "--approve"])
            .arg(approval)
            .output()
            .unwrap()
    };
    let approval = first["host_plan_sha256"].as_str().unwrap();
    let unapproved = apply("unapproved");
    assert!(!unapproved.status.success());
    let stderr = String::from_utf8_lossy(&unapproved.stderr);
    assert!(stderr.contains("digest was not approved"), "{stderr}");
    let denied = apply(approval);
    assert!(!denied.status.success());
    let stderr = String::from_utf8_lossy(&denied.stderr);
    assert!(stderr.contains("administrator"), "{stderr}");
    let mut content: Value = serde_json::from_slice(&fs::read(&plan).unwrap()).unwrap();
    content["files"][0]["content"] = json!("arbitrary unreviewed command");
    fs::write(&plan, serde_json::to_vec(&content).unwrap()).unwrap();
    let tampered = apply(approval);
    assert!(!tampered.status.success());
    let stderr = String::from_utf8_lossy(&tampered.stderr);
    assert!(stderr.contains("plan changed"), "{stderr}");
    assert!(!stderr.contains(SECRET));
}
#[test]
fn symlinked_checkpoints_and_mode_switches_preserve_existing_files() {
    let root = tempfile::tempdir().unwrap();
    let file = input(root.path(), "development");
    let directory = root.path().join("setup");
    command(root.path())
        .args(["setup", "--directory"])
        .arg(&directory)
        .arg("--file")
        .arg(file)
        .output()
        .unwrap();
    let before = fs::read(directory.join("setup.json")).unwrap();
    let output = command(root.path())
        .args(["setup", "--mode", "managed", "--directory"])
        .arg(&directory)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(fs::read(directory.join("setup.json")).unwrap(), before);
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink(&directory, &alias).unwrap();
    let output = command(root.path())
        .args(["setup", "--directory"])
        .arg(alias)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(fs::read(directory.join("setup.json")).unwrap(), before);
}
#[test]
fn saved_runtime_requires_development_consent_before_touching_profiles_or_launching() {
    let root = tempfile::tempdir().unwrap();
    let mut host: enumeray_contracts::setup::HostSetupSpec =
        serde_json::from_str(include_str!("../examples/host-setup-development.json")).unwrap();
    host.connector_executable = "/bin/false".into();
    host.context.definition.connection = enumeray_contracts::execution::ConnectionConfig::Direct {
        launcher: vec!["/bin/false".into()],
    };
    let runtime = json!({"manifest_version":1,"plan_sha256":enumeray_connector::configuration_digest(&host).unwrap(),"spec":host,"allow_development_execution":false});
    let path = root.path().join("runtime.json");
    fs::write(&path, serde_json::to_vec(&runtime).unwrap()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let output = command(root.path())
        .args(["connector", "start", "--foreground", "--configuration"])
        .arg(path)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("development consent"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(SECRET));
}

#[test]
fn project_staging_reuses_exact_bytes_and_never_follows_a_replaced_parent() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let target = root.join("release/sub/model.py");
    enumeray_connector::stage_setup_file(&target, b"print(4)\n", true).unwrap();
    enumeray_connector::stage_setup_file(&target, b"print(4)\n", true).unwrap();
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o500
    );
    assert!(enumeray_connector::stage_setup_file(&target, b"print(5)\n", true).is_err());
    assert_eq!(fs::read(&target).unwrap(), b"print(4)\n");
    let outside = root.join("unrelated");
    fs::create_dir(&outside).unwrap();
    let alias = root.join("release/alias");
    std::os::unix::fs::symlink(&outside, &alias).unwrap();
    assert!(
        enumeray_connector::stage_setup_file(&alias.join("nested/file"), b"unapproved", false)
            .is_err()
    );
    assert!(!outside.join("nested").exists());
}

#[test]
fn every_managed_backend_produces_a_reviewable_setup_without_requests() {
    let root = tempfile::tempdir().unwrap();
    for (driver, connection, source) in [
        (
            "process",
            "direct",
            include_str!("../examples/process-direct-context.json"),
        ),
        (
            "process",
            "ssh",
            include_str!("../examples/process-ssh-context.json"),
        ),
        (
            "slurm",
            "direct",
            include_str!("../examples/slurm-direct-context.json"),
        ),
        (
            "slurm",
            "ssh",
            include_str!("../examples/slurm-ssh-context.json"),
        ),
    ] {
        let mut host: Value =
            serde_json::from_str(include_str!("../examples/host-setup-linux.json")).unwrap();
        let template: Value = serde_json::from_str(source).unwrap();
        host["context"] = template["spec"].clone();
        if connection == "direct" {
            host["context"]["definition"]["connection"]["launcher"] = json!([
                "/usr/bin/sudo",
                "-n",
                "-u",
                "model-runner",
                "--",
                "/opt/enumeray/bin/enumeray"
            ]);
        } else {
            host["credentials_file"] = json!("/var/lib/enumeray-connect/ssh.json");
        }
        let path = root.path().join(format!("{driver}-{connection}.json"));
        fs::write(&path,serde_json::to_vec(&json!({"manifest_version":1,"host":host,"connector":{"id":"lab-connector","label":"Matrix","max_outstanding_assignments":1},"startup":"foreground"})).unwrap()).unwrap();
        let output = command(root.path())
            .args(["setup", "--non-interactive", "--file"])
            .arg(path)
            .arg("--directory")
            .arg(root.path().join(format!("op-{driver}-{connection}")))
            .output()
            .unwrap();
        assert_eq!(json(&output)["status"], "review_required");
    }
}

#[test]
fn copied_parent_checkpoint_cannot_fork_fresh_child_operations() {
    let root = tempfile::tempdir().unwrap();
    let source = input(root.path(), "development");
    let original = root.path().join("original");
    let output = command(root.path())
        .args(["setup", "--non-interactive", "--directory"])
        .arg(&original)
        .arg("--file")
        .arg(source)
        .output()
        .unwrap();
    json(&output);
    let copied = root.path().join("copied");
    fs::create_dir(&copied).unwrap();
    fs::set_permissions(&copied, fs::Permissions::from_mode(0o700)).unwrap();
    for name in ["setup.json", "state.json", "host-plan.json"] {
        fs::copy(original.join(name), copied.join(name)).unwrap();
    }
    let before = fs::read(copied.join("state.json")).unwrap();
    let output = command(root.path())
        .args(["setup", "--non-interactive", "--directory"])
        .arg(&copied)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("original directory"));
    assert_eq!(fs::read(copied.join("state.json")).unwrap(), before);
}
