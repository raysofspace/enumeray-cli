//! The installed executable chooses a credential domain before reading profiles.
use serde_json::Value;
use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

const USER_SECRET: &str = "user-token-must-not-reach-host-or-runner";

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
fn one_help_tree_and_version_work_without_profiles() {
    let root = tempfile::tempdir().unwrap();
    for args in [
        vec!["--help"],
        vec!["--version"],
        vec!["connector", "--help"],
        vec!["connector", "run", "--help"],
        vec!["compute-runner", "--help"],
        vec!["local-worker", "--help"],
        vec!["transfer-worker", "--help"],
    ] {
        let output = isolated(root.path()).args(&args).output().unwrap();
        assert!(
            output.status.success(),
            "{:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        no_secret(&output);
    }
    let output = isolated(root.path()).arg("--help").output().unwrap();
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("compute-runner"));
    assert!(!help.contains("local-worker"));
    assert!(!help.contains("transfer-worker"));
    let output = isolated(root.path())
        .args(["connector", "--help"])
        .output()
        .unwrap();
    let help = String::from_utf8(output.stdout).unwrap();
    for command in [
        "create",
        "maintenance",
        "enroll",
        "run",
        "inspect",
        "cleanup",
        "reconcile",
    ] {
        assert!(help.contains(command), "missing {command}");
    }
}

#[test]
fn local_journal_commands_ignore_user_profiles_and_preserve_identity() {
    let root = tempfile::tempdir().unwrap();
    let journal = root.path().join("journal");
    let output = isolated(root.path())
        .args(["connector", "init", "--journal"])
        .arg(&journal)
        .args([
            "--api-base-url",
            "http://127.0.0.1:8080",
            "--connector-id",
            "unified-test",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    no_secret(&output);
    let initialized: Value = serde_json::from_slice(&output.stdout).unwrap();
    let original = std::fs::read(journal.join("journal.json")).unwrap();
    for args in [
        vec!["connector", "inspect"],
        vec!["connector", "cleanup", "--dry-run"],
    ] {
        let output = isolated(root.path())
            .args(args)
            .arg("--journal")
            .arg(&journal)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        no_secret(&output);
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        if value.get("journal_id").is_some() {
            assert_eq!(value["journal_id"], initialized["journal_id"]);
        }
        assert_eq!(
            std::fs::read(journal.join("journal.json")).unwrap(),
            original
        );
    }
    assert!(!root.path().join("absent-home").exists());
    assert!(!root.path().join("absent-config").exists());
}

#[test]
fn runner_rejects_incompatible_protocol_without_loading_credentials() {
    let root = tempfile::tempdir().unwrap();
    let mut child = isolated(root.path())
        .args(["compute-runner", "status", "--directory"])
        .arg(root.path().join("job"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"version":999,"action":"status","payload":null}"#)
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("incompatible compute-runner protocol")
    );
    no_secret(&output);
    assert!(!root.path().join("job").exists());
}

#[test]
fn malformed_host_and_worker_arguments_are_not_reflected() {
    let root = tempfile::tempdir().unwrap();
    for args in [
        vec!["connector", "enroll", USER_SECRET],
        vec!["connector", "run", "--recovery-mode", USER_SECRET],
        vec!["compute-runner", USER_SECRET, "--directory", "/unused"],
        vec!["local-worker", USER_SECRET, "--directory", "/unused"],
        vec!["transfer-worker", USER_SECRET],
    ] {
        let output = isolated(root.path()).args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("invalid connector or runner command")
        );
        no_secret(&output);
    }
}

#[test]
fn management_commands_keep_their_argument_diagnostics() {
    let root = tempfile::tempdir().unwrap();
    let output = isolated(root.path())
        .args(["connector", "get"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("required arguments were not provided"));
    assert!(error.contains("<INFRA>"));
    assert!(!error.contains("invalid connector or runner command"));
}
