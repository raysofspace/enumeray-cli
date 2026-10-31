//! Publication-independent host configuration and bounded diagnostic commands.
use serde_json::Value;
use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

const USER_SECRET: &str = "user-token-must-not-reach-host-or-runner";

#[test]
fn host_plan_generation_is_private_atomic_and_independent_of_user_profiles() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    for (index, source) in [
        include_str!("../examples/host-setup-linux.json"),
        include_str!("../examples/host-setup-macos.json"),
        include_str!("../examples/host-setup-development.json"),
    ]
    .into_iter()
    .enumerate()
    {
        let input = root.path().join(format!("setup-{index}.json"));
        std::fs::write(&input, source).unwrap();
        let destination = root.path().join(format!("plan-{index}"));
        let invoke = || {
            isolated(root.path())
                .args(["connector", "configuration", "generate", "--file"])
                .arg(&input)
                .arg("--directory")
                .arg(&destination)
                .output()
                .unwrap()
        };
        let output = invoke();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        no_secret(&output);
        assert_eq!(
            std::fs::metadata(&destination)
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
        for entry in std::fs::read_dir(&destination).unwrap() {
            let path = entry.unwrap().path();
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o077,
                0
            );
            if cfg!(target_os = "macos") && path.extension().is_some_and(|e| e == "plist") {
                assert!(
                    Command::new("/usr/bin/plutil")
                        .args(["-lint", "--"])
                        .arg(&path)
                        .status()
                        .unwrap()
                        .success()
                );
            }
            if cfg!(target_os = "macos") && path.extension().is_some_and(|e| e == "sudoers") {
                assert!(
                    Command::new("/usr/sbin/visudo")
                        .arg("-cf")
                        .arg(&path)
                        .status()
                        .unwrap()
                        .success()
                );
            }
        }
        let before = std::fs::read(destination.join("plan.json")).unwrap();
        assert!(!invoke().status.success());
        assert_eq!(
            before,
            std::fs::read(destination.join("plan.json")).unwrap()
        );
    }
}

fn isolated(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_enumeray"));
    command
        .args(["--config"])
        .arg(root) // A directory cannot be a valid profile file.
        .env("HOME", root.join("absent-home"))
        .env("XDG_CONFIG_HOME", root.join("absent-config"))
        .env("ENUMERAY_TOKEN", USER_SECRET);
    command
}

fn no_secret(output: &Output) {
    assert!(!String::from_utf8_lossy(&output.stdout).contains(USER_SECRET));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(USER_SECRET));
}

#[test]
fn slurm_host_probe_only_uses_monitoring_commands_and_does_not_submit_or_cancel() {
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    std::fs::set_permissions(&root_path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let bin = root_path.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let log = root_path.join("commands");
    for name in ["sbatch", "squeue", "sacct", "scancel"] {
        let path = bin.join(name);
        let quoted = log.to_str().unwrap().replace('\'', "'\\''");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' {name} >> '{quoted}'\ntest -z \"${{SLURM_CLUSTERS:-}}\" || exit 9\nexit {}\n",
            if matches!(name, "sbatch" | "scancel") {
                99
            } else {
                0
            }
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut definition: Value =
        serde_json::from_str::<Value>(include_str!("../examples/slurm-direct-context.json"))
            .unwrap()["spec"]["definition"]
            .clone();
    definition["work_root"] = json!(root_path);
    definition["installation_roots"] = json!([root_path]);
    definition["driver"]["worker_path"] = json!(env!("CARGO_BIN_EXE_enumeray"));
    let probe = uuid::Uuid::new_v4();
    let payload = json!({"probe_id":probe,"definition":definition,"connector_executable":null,"connector_paths":[],"phase":"inspect"});
    let request = json!({"version":1,"action":"preflight","payload":payload});
    let mut child = isolated(&root_path)
        .args(["compute-runner", "preflight", "--directory"])
        .arg(&root_path)
        .env("PATH", &bin)
        .env("SLURM_CLUSTERS", "must-not-redirect-monitoring")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    no_secret(&output);
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    for name in ["slurm_tools", "slurm_queue", "slurm_accounting"] {
        assert!(
            result["payload"]["preflight"]["checks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["check"] == name && c["outcome"] == "passed"),
            "{result}"
        );
    }
    assert_eq!(std::fs::read_to_string(log).unwrap(), "squeue\nsacct\n");
}

#[test]
fn local_preflight_requires_fresh_consent_ignores_profiles_and_preserves_journals() {
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;
    let uid = Command::new("/usr/bin/id")
        .arg("-u")
        .output()
        .unwrap()
        .stdout;
    if uid == b"0\n" {
        return;
    } // Non-root success; root rejection has policy coverage.
    let account = String::from_utf8(
        Command::new("/usr/bin/id")
            .arg("-un")
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();
    let group = String::from_utf8(
        Command::new("/usr/bin/id")
            .arg("-gn")
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
    for name in ["models", "work"] {
        let path = base.join(name);
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut spec: Value =
        serde_json::from_str(include_str!("../examples/host-setup-development.json")).unwrap();
    for name in ["connector_account", "execution_account"] {
        spec[name]["name"] = json!(account);
        spec[name]["group"] = json!(group);
        spec[name]["home"] = json!(base);
        spec[name]["platform"] = json!(if cfg!(target_os = "macos") {
            "macos"
        } else {
            "linux"
        });
    }
    spec["connector_executable"] = json!(env!("CARGO_BIN_EXE_enumeray"));
    spec["context"]["definition"]["connection"]["launcher"] =
        json!([env!("CARGO_BIN_EXE_enumeray")]);
    spec["context"]["definition"]["work_root"] = json!(base.join("work"));
    spec["context"]["definition"]["installation_roots"] = json!([base.join("models")]);
    spec["journal"] = json!(base.join("retained-journal"));
    spec["token_file"] = json!(base.join("token"));
    let input = base.join("setup.json");
    std::fs::write(&input, spec.to_string()).unwrap();
    let destination = base.join("plan");
    let generated = isolated(&base)
        .args(["connector", "configuration", "generate", "--file"])
        .arg(&input)
        .arg("--directory")
        .arg(&destination)
        .output()
        .unwrap();
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let invoke = |consent| {
        let mut command = isolated(&base);
        command
            .args(["connector", "preflight", "--plan"])
            .arg(destination.join("plan.json"));
        if consent {
            command.arg("--allow-development-execution");
        }
        command.output().unwrap()
    };
    let blocked = invoke(false);
    assert_eq!(blocked.status.code(), Some(1));
    no_secret(&blocked);
    let blocked: Value = serde_json::from_slice(&blocked.stdout).unwrap();
    assert_eq!(blocked["ready"], false);
    assert!(
        blocked["report"]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["code"] == "development_consent_required")
    );
    let ready = invoke(true);
    no_secret(&ready);
    assert!(
        ready.status.success(),
        "{}",
        String::from_utf8_lossy(&ready.stderr)
    );
    let ready: Value = serde_json::from_slice(&ready.stdout).unwrap();
    assert_eq!(ready["ready"], true);
    assert_eq!(ready["report"]["source"], "local");
    assert_eq!(std::fs::read_dir(base.join("work")).unwrap().count(), 0);
    assert!(!base.join("retained-journal").exists());
    assert!(!base.join("token").exists());
    let path = destination.join("plan.json");
    let mut plan: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    plan["spec"]["context"]["definition"]["installation_roots"] =
        json!([base.join("missing-installation-root")]);
    std::fs::write(&path, plan.to_string()).unwrap();
    let failed = invoke(true);
    assert_eq!(failed.status.code(), Some(1));
    let failed: Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert!(
        failed["report"]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["check"] == "installation_roots"
                && c["outcome"] == "failed"
                && c["responsible_role"] == "host_administrator")
    );
    plan["spec"]["context"]["definition"]["installation_roots"] = json!([base.join("models")]);
    let fake = base.join("fake-runner");
    for (mode, content, cause) in [
        (None, "", "runner_executable_missing"),
        (
            Some(0o600),
            "#!/bin/sh\nexit 0\n",
            "runner_executable_inaccessible",
        ),
        (
            Some(0o700),
            "#!/bin/sh\nprintf '%s\\n' '{\"version\":999,\"payload\":{\"status\":\"ready\"}}'\n",
            "runner_protocol_incompatible",
        ),
    ] {
        if let Some(mode) = mode {
            std::fs::write(&fake, content).unwrap();
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        plan["spec"]["context"]["definition"]["connection"]["launcher"] = json!([fake]);
        std::fs::write(&path, plan.to_string()).unwrap();
        let output = invoke(true);
        assert_eq!(output.status.code(), Some(1));
        no_secret(&output);
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(
            report["report"]["checks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["check"] == "runner" && c["code"] == cause && c["outcome"] == "failed"),
            "{report}"
        );
        assert_eq!(std::fs::read_dir(base.join("work")).unwrap().count(), 0);
    }
}
