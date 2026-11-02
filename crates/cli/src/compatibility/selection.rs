use super::{comparison, digest, full_hash, unavailable};
use crate::{args::CompatibilityCheck as Options, config};
use enumeray_client::Error;
use enumeray_contracts::installation::{
    CompatibilityCheck, CompatibilityStatus as Status, CompatibilityTrust as Trust,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    process::Command,
};

const PUBLIC: &str = "raysofspace/enumeray-cli";
const SERVICE: &str = "raysofspace/enumeray";
const ISSUER: &str = "https://token.actions.githubusercontent.com";
const TARGETS: [&str; 4] = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-gnu",
];

fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}

fn valid_pair(value: &Value) -> bool {
    value["schema_version"] == 1
        && value["public_repository"] == PUBLIC
        && value["service_repository"] == SERVICE
        && full_hash(&value["public_revision"], 40)
        && full_hash(&value["service_revision"], 40)
        && value["version"].as_str().is_some_and(|v| {
            !v.is_empty()
                && v.len() <= 100
                && v.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
        })
        && value["baseline"]["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("persistent-v"))
        && full_hash(&value["baseline"]["sha256"], 64)
        && matches!(
            value["baseline"]["status"].as_str(),
            Some("candidate" | "released")
        )
        && (value["baseline"]["status"] == "released"
            || value["version"].as_str().is_some_and(|v| v.contains('-')))
}

fn signature(
    source: &Path,
    bundle: &Path,
    private: bool,
    record: &Value,
    checks: &mut Vec<CompatibilityCheck>,
    verifier: &impl Fn(&mut Command) -> std::io::Result<std::process::Output>,
) {
    let component = if private {
        "service_publication"
    } else {
        "release_manifest"
    };
    if !source.is_file() || !bundle.is_file() {
        unavailable(
            checks,
            component,
            "signature",
            "Required Sigstore bundle is unavailable; no release authenticity was established.",
        );
        return;
    }
    let reference = if private {
        format!(
            "refs/tags/cli-v{}",
            record["version"].as_str().unwrap_or("")
        )
    } else {
        "refs/heads/main".into()
    };
    let repository = if private { SERVICE } else { PUBLIC };
    let identity =
        format!("https://github.com/{repository}/.github/workflows/release.yml@{reference}");
    let mut command = Command::new("cosign");
    command.args(["verify-blob", "--bundle"]).arg(bundle).args([
        "--certificate-identity",
        &identity,
        "--certificate-oidc-issuer",
        ISSUER,
        "--certificate-github-workflow-repository",
        repository,
        "--certificate-github-workflow-ref",
        &reference,
        "--certificate-github-workflow-trigger",
        if private {
            "push"
        } else {
            "repository_dispatch"
        },
    ]);
    let revision = if private {
        &record["service_revision"]
    } else {
        &record["builder_workflow_revision"]
    };
    if full_hash(revision, 40) {
        command.args([
            "--certificate-github-workflow-sha",
            revision.as_str().unwrap(),
        ]);
    }
    match verifier(command.arg(source)) {
        Ok(output) => comparison(
            checks,
            component,
            "signature_verified",
            json!(true),
            json!(output.status.success()),
        ),
        Err(_) => unavailable(
            checks,
            component,
            "signature",
            "Separately trusted Cosign v3 is unavailable; install it on the trusted verification host.",
        ),
    }
}

fn sibling_bundle(path: &Path, service: bool) -> PathBuf {
    if service {
        path.with_extension("sigstore.json")
    } else {
        PathBuf::from(format!("{}.sigstore.json", path.display()))
    }
}

fn report_path(directory: &Path, name: &str) -> Result<PathBuf, Error> {
    if name.is_empty()
        || name.len() > 200
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        || matches!(name, "." | "..")
    {
        return Err(invalid("Release manifest contains an invalid asset name."));
    }
    Ok(directory.join(name))
}

pub(super) fn load(options: &Options) -> Result<(Value, Trust, Vec<CompatibilityCheck>), Error> {
    load_with_verifier(options, &|command| command.output())
}

fn load_with_verifier(
    options: &Options,
    verifier: &impl Fn(&mut Command) -> std::io::Result<std::process::Output>,
) -> Result<(Value, Trust, Vec<CompatibilityCheck>), Error> {
    let mut checks = Vec::new();
    if let Some(path) = &options.development_selection {
        let selected: Value = config::read_json(path)?;
        if !valid_pair(&selected) {
            return Err(invalid(
                "Development selection must name the exact service/public revisions, version and provisional baseline using the existing qualification identity fields.",
            ));
        }
        return Ok((selected, Trust::UnsignedDevelopment, checks));
    }
    if let Some(path) = &options.transferred_receipt {
        let expected = options.transferred_sha256.as_deref().unwrap_or("");
        if !full_hash(&json!(expected), 64) {
            return Err(invalid(
                "An independently conveyed full transferred-receipt SHA-256 is required.",
            ));
        }
        comparison(
            &mut checks,
            "authenticated_transfer",
            "receipt_sha256",
            json!(expected),
            json!(digest(path)?),
        );
        let original: enumeray_contracts::installation::CompatibilityReceipt =
            config::read_json(path)?;
        if original.status != Status::Match
            || original.trust != Trust::SignedRelease
            || !valid_pair(&original.selected)
            || !complete_transfer(&original)
        {
            return Err(invalid(
                "Transfer requires an original successful signed-release receipt; unsigned or incomplete evidence cannot become release-qualified.",
            ));
        }
        return Ok((original.selected, Trust::AuthenticatedTransfer, checks));
    }
    let path = options
        .manifest
        .as_ref()
        .ok_or_else(|| invalid("An explicit identity selection is required."))?;
    let service_path = options
        .service_receipt
        .as_ref()
        .ok_or_else(|| invalid("Signed service publication receipt is required."))?;
    let manifest: Value = config::read_json(path)?;
    let service: Value = config::read_json(service_path)?;
    if !valid_pair(&service)
        || service["purpose"] != "publish"
        || !full_hash(&service["manifest_sha256"], 64)
        || !service["qualification_run"]
            .as_str()
            .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(invalid(
            "Service receipt is not an exact publication authorization.",
        ));
    }
    if manifest["schema_version"] != 1
        || manifest["public_repository"] != PUBLIC
        || !full_hash(&manifest["source_tree"], 40)
        || !full_hash(&manifest["lockfile_sha256"], 64)
        || !full_hash(&manifest["builder_workflow_revision"], 40)
    {
        return Err(invalid(
            "Release manifest identity is missing or malformed.",
        ));
    }
    signature(
        path,
        &sibling_bundle(path, false),
        false,
        &manifest,
        &mut checks,
        verifier,
    );
    signature(
        service_path,
        &sibling_bundle(service_path, true),
        true,
        &service,
        &mut checks,
        verifier,
    );
    comparison(
        &mut checks,
        "service_publication",
        "manifest_sha256",
        service["manifest_sha256"].clone(),
        json!(digest(path)?),
    );
    for field in ["public_revision", "version", "baseline"] {
        comparison(
            &mut checks,
            "release_manifest",
            field,
            service[field].clone(),
            manifest[field].clone(),
        );
    }
    comparison(
        &mut checks,
        "release_manifest",
        "tag",
        json!(format!("v{}", service["version"].as_str().unwrap())),
        manifest["tag"].clone(),
    );
    let directory = path.parent().unwrap_or(Path::new("."));
    let files = manifest["files"]
        .as_array()
        .ok_or_else(|| invalid("Release manifest files are missing."))?;
    let mut names = BTreeSet::new();
    for entry in files {
        let name = entry["name"]
            .as_str()
            .ok_or_else(|| invalid("Invalid release asset."))?;
        report_path(directory, name)?;
        if !names.insert(name) || !full_hash(&entry["sha256"], 64) {
            return Err(invalid("Duplicate or invalid manifest asset identity."));
        }
    }
    let mut artifacts = serde_json::Map::new();
    for target in TARGETS {
        let target_entry = &manifest["targets"][target];
        let component = format!("release_{target}");
        let Some(name) = target_entry["qualification"].as_str() else {
            unavailable(
                &mut checks,
                &component,
                "native_report",
                "Native target qualification report is missing.",
            );
            continue;
        };
        let report_path = report_path(directory, name)?;
        let manifest_entry = files
            .iter()
            .find(|e| e["name"] == name && e["kind"] == "qualification");
        let Some(entry) = manifest_entry else {
            return Err(invalid(
                "Native report is not bound by the signed manifest.",
            ));
        };
        let report = match config::read_json::<Value>(&report_path) {
            Ok(report) => report,
            Err(_) => {
                unavailable(
                    &mut checks,
                    &component,
                    "native_report",
                    "Native report is unavailable; retain/download its signed-manifest-bound evidence.",
                );
                continue;
            }
        };
        comparison(
            &mut checks,
            &component,
            "report_sha256",
            entry["sha256"].clone(),
            json!(digest(&report_path)?),
        );
        comparison(
            &mut checks,
            &component,
            "target",
            json!(target),
            report["target"].clone(),
        );
        comparison(
            &mut checks,
            &component,
            "platform_qualified",
            json!(true),
            report["platform_qualified"].clone(),
        );
        comparison(
            &mut checks,
            &component,
            "source_revision",
            manifest["public_revision"].clone(),
            report["source"]["revision"].clone(),
        );
        comparison(
            &mut checks,
            &component,
            "source_tree",
            manifest["source_tree"].clone(),
            report["source"]["tree"].clone(),
        );
        comparison(
            &mut checks,
            &component,
            "archive",
            target_entry["archive"].clone(),
            report["archive"].clone(),
        );
        let expected_archive = format!(
            "enumeray-{}-{target}.tar.gz",
            manifest["version"].as_str().unwrap_or("")
        );
        comparison(
            &mut checks,
            &component,
            "version",
            manifest["version"].clone(),
            report["version"].clone(),
        );
        comparison(
            &mut checks,
            &component,
            "archive_name",
            json!(expected_archive),
            target_entry["archive"].clone(),
        );
        let archive_entry = files.iter().find(|e| {
            e["name"] == expected_archive && e["kind"] == "binary" && e["target"] == target
        });
        comparison(
            &mut checks,
            &component,
            "archive_sha256",
            archive_entry
                .map(|e| e["sha256"].clone())
                .unwrap_or(Value::Null),
            report["sha256"].clone(),
        );
        comparison(
            &mut checks,
            &component,
            "source_clean",
            json!(true),
            report["source"]["clean"].clone(),
        );
        comparison(
            &mut checks,
            &component,
            "source_lockfile_sha256",
            manifest["lockfile_sha256"].clone(),
            report["source"]["lockfile_sha256"].clone(),
        );
        let minimum = if target.ends_with("apple-darwin") {
            json!({"os":"macos","version":"15.0"})
        } else {
            json!({"os":"linux","glibc":"2.28"})
        };
        comparison(
            &mut checks,
            &component,
            "minimum_platform",
            minimum,
            report["minimum_platform"].clone(),
        );
        let identity = serde_json::from_value::<enumeray_contracts::compatibility::BuildIdentity>(
            report["build_identity"].clone(),
        )
        .ok();
        comparison(
            &mut checks,
            &component,
            "valid_build_identity",
            json!(true),
            json!(identity.as_ref().is_some_and(|v| v.validate())),
        );
        comparison(
            &mut checks,
            &component,
            "build_target",
            json!(target),
            report["build_identity"]["target"].clone(),
        );
        comparison(
            &mut checks,
            &component,
            "build_source",
            report["source"].clone(),
            {
                let mut source = report["build_identity"]["source"].clone();
                // Existing signed source metadata has five fields; content identity
                // is carried by the executable/native build report separately.
                if let Some(object) = source.as_object_mut() {
                    object.remove("content_sha256");
                }
                source
            },
        );
        let binary = report["binary_sha256"].clone();
        if !full_hash(&binary, 64) {
            unavailable(
                &mut checks,
                &component,
                "binary_sha256",
                "Native report lacks the exact extracted executable digest.",
            );
        }
        artifacts.insert(target.into(),json!({"binary_sha256":binary,"build_identity":report["build_identity"],"report_sha256":entry["sha256"]}));
    }
    let mut selected = service;
    selected["source"] = json!({"revision":manifest["public_revision"],"tree":manifest["source_tree"],"lockfile_sha256":manifest["lockfile_sha256"]});
    selected["artifacts"] = Value::Object(artifacts);
    selected["manifest_sha256"] = json!(digest(path)?);
    selected["service_receipt_sha256"] = json!(digest(service_path)?);
    let trust = if checks
        .iter()
        .filter(|c| c.field == "signature_verified")
        .count()
        == 2
        && checks
            .iter()
            .filter(|c| c.field == "signature_verified")
            .all(|c| c.status == Status::Match)
        && !checks.iter().any(|c| c.field == "signature")
    {
        Trust::SignedRelease
    } else {
        Trust::Unverified
    };
    Ok((selected, trust, checks))
}

fn complete_transfer(receipt: &enumeray_contracts::installation::CompatibilityReceipt) -> bool {
    if receipt.checks.is_empty() || receipt.checks.iter().any(|c| c.status != Status::Match) {
        return false;
    }
    for component in ["release_manifest", "service_publication"] {
        if !receipt.checks.iter().any(|c| {
            c.component == component
                && c.field == "signature_verified"
                && c.expected == true
                && c.observed == true
        }) {
            return false;
        }
    }
    if !full_hash(&receipt.selected["manifest_sha256"], 64)
        || !full_hash(&receipt.selected["service_receipt_sha256"], 64)
    {
        return false;
    }
    let Some(artifacts) = receipt.selected["artifacts"].as_object() else {
        return false;
    };
    if artifacts.len() != TARGETS.len() {
        return false;
    }
    for target in TARGETS {
        let artifact = &receipt.selected["artifacts"][target];
        let Ok(identity) = serde_json::from_value::<enumeray_contracts::compatibility::BuildIdentity>(
            artifact["build_identity"].clone(),
        ) else {
            return false;
        };
        if !full_hash(&artifact["binary_sha256"], 64)
            || !full_hash(&artifact["report_sha256"], 64)
            || !identity.validate()
            || !identity.source.clean
            || identity.target != target
            || identity.source.revision.as_deref() != receipt.selected["public_revision"].as_str()
            || identity.version != receipt.selected["version"].as_str().unwrap_or("")
        {
            return false;
        }
        if !receipt.checks.iter().any(|c| {
            c.component == format!("release_{target}")
                && c.field == "report_sha256"
                && c.observed == artifact["report_sha256"]
                && c.expected == c.observed
        }) {
            return false;
        }
    }
    receipt.checks.iter().any(|c| {
        c.component == "local_executable"
            && c.field == "archive_binary_sha256"
            && full_hash(&c.observed, 64)
            && c.expected == c.observed
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        cell::RefCell,
        fs,
        os::unix::process::ExitStatusExt,
        process::{ExitStatus, Output},
    };
    use tempfile::TempDir;

    fn write(path: &Path, value: &Value) {
        fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
    }
    fn result(code: i32) -> Output {
        Output {
            status: ExitStatus::from_raw(code << 8),
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    }
    fn fixture() -> (TempDir, Options) {
        let directory = TempDir::new().unwrap();
        let root = directory.path();
        let baseline = json!({"id":"persistent-v1","status":"candidate","sha256":"e".repeat(64)});
        let source = json!({"repository":"https://github.com/raysofspace/enumeray-cli","revision":"a".repeat(40),"tree":"b".repeat(40),"lockfile_sha256":"c".repeat(64),"clean":true});
        let version = "0.1.0-alpha.1";
        let mut files = Vec::new();
        let mut targets = serde_json::Map::new();
        for target in TARGETS {
            let archive = format!("enumeray-{version}-{target}.tar.gz");
            let name = format!("qualification-{target}.json");
            fs::write(root.join(&archive), target.as_bytes()).unwrap();
            let minimum = if target.contains("apple") {
                json!({"os":"macos","version":"15.0"})
            } else {
                json!({"os":"linux","glibc":"2.28"})
            };
            let mut build_source = source.clone();
            build_source["content_sha256"] = json!("d".repeat(64));
            let build = json!({"schema_version":1,"package":"enumeray-cli","version":version,"target":target,"source":build_source,"api":"v1","connector_protocol":1,"runner_protocol":1,"wrapper_protocols":[1]});
            let mut report = json!({"schema_version":1,"target":target,"version":version,"archive":archive,"sha256":digest(&root.join(&archive)).unwrap(),"source":source,"minimum_platform":minimum,"platform_qualified":true,"binary_sha256":"f".repeat(64),"build_identity":build});
            if target.contains("apple") {
                report["channel_qualification"] = json!({"homebrew":{"schema_version":1,"channel":"homebrew","state":"passed","minimum_platform_qualified":true,"target":target,"version":version,"archive":archive,"sha256":report["sha256"],"binary_sha256":"f".repeat(64),"lifecycle":["clean-install-and-formula-test","same-candidate-revision-upgrade-and-formula-test","uninstall-preserving-private-state"],"runtime":{"system":"Darwin","macos":"15.7.1","machine":if target.starts_with("aarch64") {"arm64"} else {"x86_64"}}}});
            }
            write(&root.join(&name), &report);
            files.push(json!({"name":name,"kind":"qualification","sha256":digest(&root.join(&name)).unwrap()}));
            files.push(json!({"name":archive,"kind":"binary","target":target,"sha256":digest(&root.join(&archive)).unwrap()}));
            let minimum_field = if target.contains("apple") {
                "minimum_macos"
            } else {
                "minimum_glibc"
            };
            let mut selected = json!({"archive":archive,"qualification":name});
            selected[minimum_field] = json!(if target.contains("apple") {
                "15.0"
            } else {
                "2.28"
            });
            targets.insert(target.into(), selected);
        }
        let manifest = json!({"schema_version":1,"public_repository":PUBLIC,"public_revision":"a".repeat(40),"source_tree":"b".repeat(40),"lockfile_sha256":"c".repeat(64),"builder_workflow_revision":"1".repeat(40),"version":version,"tag":format!("v{version}"),"baseline":baseline,"prepare_receipt_sha256":"2".repeat(64),"rust_toolchain":"1.98.1","files":files,"targets":targets});
        let manifest_path = root.join("release-manifest.json");
        write(&manifest_path, &manifest);
        fs::write(sibling_bundle(&manifest_path, false), b"{}").unwrap();
        let service_path = root.join("service-publish.json");
        let service = json!({"schema_version":1,"purpose":"publish","service_repository":SERVICE,"public_repository":PUBLIC,"service_revision":"3".repeat(40),"public_revision":"a".repeat(40),"version":version,"baseline":baseline,"qualification_run":"12345","manifest_sha256":digest(&manifest_path).unwrap()});
        write(&service_path, &service);
        fs::write(sibling_bundle(&service_path, true), b"{}").unwrap();
        let options = Options {
            manifest: Some(manifest_path),
            service_receipt: Some(service_path),
            development_selection: None,
            transferred_receipt: None,
            transferred_sha256: None,
            receipt: root.join("receipt.json"),
            local_only: true,
            peer: Vec::new(),
            docs: None,
            examples: None,
            connector: None,
            context: Vec::new(),
        };
        (directory, options)
    }
    fn any(checks: &[CompatibilityCheck], field: &str, status: Status) -> bool {
        checks
            .iter()
            .any(|check| check.field == field && check.status == status)
    }

    #[test]
    fn release_verification_pins_both_authorities_and_binds_native_report_bytes() {
        let (_directory, options) = fixture();
        let calls = RefCell::new(Vec::new());
        let (selected, trust, checks) = load_with_verifier(&options, &|command| {
            assert_eq!(command.get_program(), "cosign");
            calls.borrow_mut().push(
                command
                    .get_args()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect::<Vec<_>>(),
            );
            Ok(result(0))
        })
        .unwrap();
        assert_eq!(trust, Trust::SignedRelease);
        assert!(
            checks.iter().all(|check| check.status == Status::Match),
            "{checks:?}"
        );
        assert_eq!(selected["artifacts"].as_object().unwrap().len(), 4);
        let calls = calls.borrow();
        assert_eq!(calls.len(), 2);
        assert!(calls[0].iter().any(|arg|arg=="https://github.com/raysofspace/enumeray-cli/.github/workflows/release.yml@refs/heads/main"));
        assert!(calls[0].iter().any(|arg| arg == "repository_dispatch"));
        assert!(calls[0].iter().any(|arg| arg == &"1".repeat(40)));
        assert!(calls[1].iter().any(|arg|arg=="https://github.com/raysofspace/enumeray/.github/workflows/release.yml@refs/tags/cli-v0.1.0-alpha.1"));
        assert!(calls[1].iter().any(|arg| arg == "push"));
        assert!(calls[1].iter().any(|arg| arg == &"3".repeat(40)));
        assert!(
            calls
                .iter()
                .all(|args| args.iter().any(|arg| arg == ISSUER))
        );
    }
    #[test]
    fn bad_or_unavailable_signature_verification_never_matches() {
        let (_directory, options) = fixture();
        let (_, trust, checks) = load_with_verifier(&options, &|_| Ok(result(1))).unwrap();
        assert_eq!(trust, Trust::Unverified);
        assert!(any(&checks, "signature_verified", Status::Mismatch));
        let (_, trust, checks) =
            load_with_verifier(&options, &|_| Err(std::io::ErrorKind::NotFound.into())).unwrap();
        assert_eq!(trust, Trust::Unverified);
        assert!(any(&checks, "signature", Status::MissingEvidence));
    }
    #[test]
    fn tampered_report_wrong_target_and_missing_reports_never_match() {
        let (directory, options) = fixture();
        let path = directory
            .path()
            .join(format!("qualification-{}.json", TARGETS[0]));
        let mut report: Value = config::read_json(&path).unwrap();
        report["target"] = json!(TARGETS[1]);
        write(&path, &report);
        let (_, _, checks) = load_with_verifier(&options, &|_| Ok(result(0))).unwrap();
        assert!(any(&checks, "report_sha256", Status::Mismatch));
        assert!(any(&checks, "target", Status::Mismatch));
        fs::remove_file(path).unwrap();
        let (_, _, checks) = load_with_verifier(&options, &|_| Ok(result(0))).unwrap();
        assert!(any(&checks, "native_report", Status::MissingEvidence));
    }
    #[test]
    fn manifest_bytes_must_match_signed_service_authorization() {
        let (_directory, options) = fixture();
        let path = options.manifest.as_ref().unwrap();
        let mut manifest: Value = config::read_json(path).unwrap();
        manifest["public_revision"] = json!("9".repeat(40));
        write(path, &manifest);
        let (_, _, checks) = load_with_verifier(&options, &|_| Ok(result(0))).unwrap();
        assert!(any(&checks, "manifest_sha256", Status::Mismatch));
        assert!(any(&checks, "public_revision", Status::Mismatch));
    }

    fn accepted_receipt(
        options: &Options,
    ) -> enumeray_contracts::installation::CompatibilityReceipt {
        let (selected, trust, mut checks) =
            load_with_verifier(options, &|_| Ok(result(0))).unwrap();
        comparison(
            &mut checks,
            "local_executable",
            "archive_binary_sha256",
            json!("f".repeat(64)),
            json!("f".repeat(64)),
        );
        enumeray_contracts::installation::CompatibilityReceipt {
            schema_version: enumeray_contracts::automation::SchemaVersion,
            status: Status::Match,
            trust,
            observed_at: "2026-11-02T00:00:00Z".into(),
            selected,
            checks,
            limits: vec!["Authenticity verified by the trusted connected sender.".into()],
        }
    }
    #[test]
    fn offline_transfer_requires_complete_original_evidence_and_independent_digest() {
        let (directory, mut options) = fixture();
        let original = accepted_receipt(&options);
        assert!(complete_transfer(&original));
        let path = directory.path().join("connected-verification.json");
        write(&path, &serde_json::to_value(&original).unwrap());
        options.manifest = None;
        options.service_receipt = None;
        options.transferred_receipt = Some(path.clone());
        options.transferred_sha256 = Some(digest(&path).unwrap());
        let (selected, trust, checks) = load_with_verifier(&options, &|_| {
            panic!("offline transfer must not invoke Cosign")
        })
        .unwrap();
        assert_eq!(trust, Trust::AuthenticatedTransfer);
        assert_eq!(selected, original.selected);
        assert!(checks.iter().all(|check| check.status == Status::Match));
        options.transferred_sha256 = Some("0".repeat(64));
        let (_, _, checks) = load_with_verifier(&options, &|_| {
            panic!("offline transfer must not invoke Cosign")
        })
        .unwrap();
        assert!(any(&checks, "receipt_sha256", Status::Mismatch));
        let mut relabeled = original;
        relabeled.checks.clear();
        write(&path, &serde_json::to_value(relabeled).unwrap());
        options.transferred_sha256 = Some(digest(&path).unwrap());
        assert!(
            load_with_verifier(&options, &|_| panic!(
                "offline transfer must not invoke Cosign"
            ))
            .is_err()
        );
    }
    #[test]
    fn signed_header_cannot_replace_missing_or_conflicting_transfer_proofs() {
        let (_directory, options) = fixture();
        let original = accepted_receipt(&options);
        for failure in [
            "signature",
            "target",
            "dirty",
            "report_digest",
            "missing_target",
            "local_digest",
        ] {
            let mut altered = original.clone();
            match failure {
                "signature" => {
                    altered
                        .checks
                        .iter_mut()
                        .find(|check| check.field == "signature_verified")
                        .unwrap()
                        .observed = json!(false)
                }
                "target" => {
                    altered.selected["artifacts"][TARGETS[0]]["build_identity"]["target"] =
                        json!(TARGETS[1])
                }
                "dirty" => {
                    altered.selected["artifacts"][TARGETS[0]]["build_identity"]["source"]["clean"] =
                        json!(false)
                }
                "report_digest" => {
                    altered.selected["artifacts"][TARGETS[0]]["report_sha256"] =
                        json!("0".repeat(64))
                }
                "missing_target" => {
                    altered.selected["artifacts"]
                        .as_object_mut()
                        .unwrap()
                        .remove(TARGETS[0]);
                }
                "local_digest" => altered
                    .checks
                    .retain(|check| check.component != "local_executable"),
                _ => unreachable!(),
            }
            assert!(!complete_transfer(&altered), "{failure}");
        }
    }
    #[test]
    fn authentic_report_still_requires_qualified_platform_archive_and_source_identity() {
        for (field, check) in [
            ("minimum_platform", "minimum_platform"),
            ("sha256", "archive_sha256"),
            ("source", "source_lockfile_sha256"),
        ] {
            let (directory, options) = fixture();
            let path = directory
                .path()
                .join(format!("qualification-{}.json", TARGETS[0]));
            let mut report: Value = config::read_json(&path).unwrap();
            match field {
                "minimum_platform" => report[field] = json!({"os":"macos","version":"14.0"}),
                "sha256" => report[field] = json!("0".repeat(64)),
                "source" => report[field]["lockfile_sha256"] = json!("0".repeat(64)),
                _ => unreachable!(),
            }
            write(&path, &report);
            let manifest_path = options.manifest.as_ref().unwrap();
            let mut manifest: Value = config::read_json(manifest_path).unwrap();
            let name = path.file_name().unwrap().to_str().unwrap();
            manifest["files"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|entry| entry["name"] == name)
                .unwrap()["sha256"] = json!(digest(&path).unwrap());
            write(manifest_path, &manifest);
            let service_path = options.service_receipt.as_ref().unwrap();
            let mut service: Value = config::read_json(service_path).unwrap();
            service["manifest_sha256"] = json!(digest(manifest_path).unwrap());
            write(service_path, &service);
            let (_, trust, checks) = load_with_verifier(&options, &|_| Ok(result(0))).unwrap();
            assert_eq!(trust, Trust::SignedRelease);
            assert!(any(&checks, check, Status::Mismatch), "{field}: {checks:?}");
        }
    }
}
