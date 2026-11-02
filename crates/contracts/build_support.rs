//! Shared build-script provenance; no runtime filesystem discovery.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fs, path::Path, process::Command};

fn git(root: &Path, arguments: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}
fn text(root: &Path, arguments: &[&str]) -> Option<String> {
    String::from_utf8(git(root, arguments)?)
        .ok()
        .map(|value| value.trim().to_owned())
}
fn field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}
fn content(root: &Path, paths: &BTreeSet<String>) -> String {
    let mut digest = Sha256::new();
    for relative in paths {
        let path = root.join(relative);
        println!("cargo:rerun-if-changed={}", path.display());
        let (mode, bytes) = match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => (
                "120000",
                fs::read_link(&path)
                    .unwrap()
                    .as_os_str()
                    .as_encoded_bytes()
                    .to_vec(),
            ),
            Ok(metadata) if metadata.is_dir() => (
                "160000",
                text(&path, &["rev-parse", "HEAD"])
                    .unwrap_or_default()
                    .into_bytes(),
            ),
            Ok(metadata) => {
                #[cfg(unix)]
                let executable = {
                    use std::os::unix::fs::PermissionsExt;
                    metadata.permissions().mode() & 0o111 != 0
                };
                #[cfg(not(unix))]
                let executable = false;
                (
                    if executable { "100755" } else { "100644" },
                    fs::read(&path).unwrap(),
                )
            }
            Err(_) => ("missing", Vec::new()),
        };
        field(&mut digest, relative.as_bytes());
        field(&mut digest, mode.as_bytes());
        field(&mut digest, &bytes);
    }
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn archive_paths(root: &Path, path: &Path, paths: &mut BTreeSet<String>) {
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            if matches!(
                name.to_str(),
                Some(".git" | "target" | "__pycache__" | ".DS_Store")
            ) {
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if matches!(
                relative.as_str(),
                "release-source.json" | "release-source-files.json" | "THIRD_PARTY_NOTICES.json"
            ) {
                continue;
            }
            if path.is_dir() && !path.is_symlink() {
                archive_paths(root, &path, paths);
            } else {
                paths.insert(relative);
            }
        }
    }
}

pub fn source_identity(root: &Path, repository: &str) -> Value {
    let root = root.canonicalize().expect("source root");
    let own_git = text(&root, &["rev-parse", "--show-toplevel"])
        .is_some_and(|value| Path::new(&value) == root);
    if !own_git {
        for name in ["release-source.json", "release-source-files.json"] {
            println!("cargo:rerun-if-changed={}", root.join(name).display());
        }
    }
    // Watch directories as well as files so newly added source files invalidate Cargo's cache.
    for name in [
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        "crates",
        "src",
        "scripts",
        "examples",
        "migrations",
        "release",
        ".git",
        "release-source.json",
        "release-source-files.json",
    ] {
        let path = root.join(name);
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    let (revision, tree, clean, paths) = if own_git {
        // Linked worktrees' .git file is stable; watch the actual ref and index too.
        for name in ["HEAD", "index"] {
            if let Some(path) = text(&root, &["rev-parse", "--git-path", name]) {
                println!("cargo:rerun-if-changed={}", root.join(path).display());
            }
        }
        if let Some(reference) = text(&root, &["symbolic-ref", "-q", "HEAD"])
            && let Some(path) = text(&root, &["rev-parse", "--git-path", &reference])
        {
            println!("cargo:rerun-if-changed={}", root.join(path).display());
        }
        let paths = git(
            &root,
            &[
                "ls-files",
                "--cached",
                "--others",
                "--exclude-standard",
                "-z",
            ],
        )
        .unwrap_or_default()
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8(path.to_vec()).expect("UTF-8 source paths"))
        .collect();
        (
            text(&root, &["rev-parse", "HEAD"]),
            text(&root, &["rev-parse", "HEAD^{tree}"]),
            git(&root, &["status", "--porcelain", "--untracked-files=all"])
                .is_some_and(|value| value.is_empty()),
            paths,
        )
    } else {
        let record: Value = fs::read(root.join("release-source.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or(Value::Null);
        let mut paths = BTreeSet::new();
        archive_paths(&root, &root, &mut paths);
        let inventory: Value = fs::read(root.join("release-source-files.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or(Value::Null);
        let expected: BTreeSet<String> = inventory["files"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
        let hash = content(&root, &paths);
        let clean = !expected.is_empty()
            && paths == expected
            && inventory["content_sha256"].as_str() == Some(&hash)
            && record["repository"] == repository
            && record["clean"] == true;
        (
            record["revision"].as_str().map(str::to_owned),
            record["tree"].as_str().map(str::to_owned),
            clean,
            paths,
        )
    };
    let lockfile = fs::read(root.join("Cargo.lock")).ok().map(|bytes| {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    });
    json!({"repository":repository,"revision":revision,"tree":tree,"clean":clean,"lockfile_sha256":lockfile,"content_sha256":content(&root,&paths)})
}
