//! Inspectable build provenance. Identity matching does not attest scientific software.
use serde::{Deserialize, Serialize};

/// Supported protocols come from the same constants used by identity reporting.
pub const API: &str = "v1";
pub const CONNECTOR_PROTOCOL: u32 = 1;
pub const RUNNER_PROTOCOL: u32 = 1;
pub const WRAPPER_PROTOCOLS: &[u32] = &[1];

/// Exact build inputs; dirty builds retain an immutable content fingerprint.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceIdentity {
    pub repository: String,
    pub revision: Option<String>,
    pub tree: Option<String>,
    pub lockfile_sha256: Option<String>,
    pub clean: bool,
    pub content_sha256: String,
}

/// Public executable identity shared by CLI, connector and compute runner.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildIdentity {
    pub schema_version: u32,
    pub package: String,
    pub version: String,
    pub target: String,
    pub source: SourceIdentity,
    pub api: String,
    pub connector_protocol: u32,
    pub runner_protocol: u32,
    pub wrapper_protocols: Vec<u32>,
}

impl BuildIdentity {
    /// Validate bounded, supported identity facts; this is not signature verification.
    pub fn validate(&self) -> bool {
        fn hash(value: &str, length: usize) -> bool {
            value.len() == length
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }
        self.schema_version == 1
            && self.package == "enumeray-cli"
            && !self.version.is_empty()
            && self.version.len() <= 100
            && self.version.bytes().all(|b| b.is_ascii_graphic())
            && !self.target.is_empty()
            && self.target.len() <= 100
            && self
                .target
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            && self.api == API
            && self.connector_protocol == CONNECTOR_PROTOCOL
            && self.runner_protocol == RUNNER_PROTOCOL
            && self.wrapper_protocols == WRAPPER_PROTOCOLS
            && self.source.repository == "https://github.com/raysofspace/enumeray-cli"
            && hash(&self.source.content_sha256, 64)
            && self.source.revision.as_ref().is_none_or(|v| hash(v, 40))
            && self.source.tree.as_ref().is_none_or(|v| hash(v, 40))
            && self
                .source
                .lockfile_sha256
                .as_ref()
                .is_none_or(|v| hash(v, 64))
            && (!self.source.clean
                || (self.source.revision.is_some()
                    && self.source.tree.is_some()
                    && self.source.lockfile_sha256.is_some()))
    }
}

/// Return compiled provenance, without reading profiles, journals or current sources.
pub fn build_identity() -> BuildIdentity {
    BuildIdentity {
        schema_version: 1,
        package: "enumeray-cli".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        target: env!("ENUMERAY_BUILD_TARGET").into(),
        source: serde_json::from_str(include_str!(concat!(
            env!("OUT_DIR"),
            "/source-identity.json"
        )))
        .expect("build-time source identity"),
        api: API.into(),
        connector_protocol: CONNECTOR_PROTOCOL,
        runner_protocol: RUNNER_PROTOCOL,
        wrapper_protocols: WRAPPER_PROTOCOLS.to_vec(),
    }
}

/// Conservative unreleased-state ownership, independent of compiler output bytes.
pub fn state_identity() -> String {
    state_identity_for(&build_identity())
}

fn state_identity_for(identity: &BuildIdentity) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(b"enumeray-unreleased-state-v1\0");
    digest.update(identity.source.content_sha256.as_bytes());
    digest.update(identity.connector_protocol.to_be_bytes());
    digest.update(identity.runner_protocol.to_be_bytes());
    for protocol in &identity.wrapper_protocols {
        digest.update(protocol.to_be_bytes());
    }
    format!(
        "sha256:{}",
        digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

#[cfg(test)]
#[path = "../build_support.rs"]
mod build_provenance;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn build_fingerprint_detects_changes_and_survives_verified_source_export() {
        use std::{fs, process::Command};
        let root = std::env::temp_dir().join(format!(
            "enumeray-provenance-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("Cargo.lock"), "version = 4\n").unwrap();
        fs::write(root.join("source.rs"), "fn main() {}\n").unwrap();
        for arguments in [
            vec!["init", "-q"],
            vec!["add", "."],
            vec![
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.org",
                "commit",
                "-qm",
                "Fixture",
            ],
        ] {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(&root)
                    .args(arguments)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let repository = "https://github.com/raysofspace/enumeray-cli";
        let first = super::build_provenance::source_identity(&root, repository);
        assert_eq!(first["clean"], true);
        fs::write(root.join("source.rs"), "fn main() { panic!() }\n").unwrap();
        let dirty = super::build_provenance::source_identity(&root, repository);
        assert_eq!(dirty["clean"], false);
        assert_ne!(first["content_sha256"], dirty["content_sha256"]);
        fs::write(root.join("source.rs"), "fn main() {}\n").unwrap();
        assert_eq!(
            first,
            super::build_provenance::source_identity(&root, repository)
        );
        fs::remove_dir_all(root.join(".git")).unwrap();
        fs::write(
            root.join("release-source.json"),
            serde_json::to_vec(&first).unwrap(),
        )
        .unwrap();
        fs::write(root.join("release-source-files.json"), serde_json::to_vec(&serde_json::json!({"files":["Cargo.lock","source.rs"],"content_sha256":first["content_sha256"]})).unwrap()).unwrap();
        assert_eq!(
            first,
            super::build_provenance::source_identity(&root, repository)
        );
        fs::write(root.join("extra.rs"), "unexpected input").unwrap();
        let changed = super::build_provenance::source_identity(&root, repository);
        assert_eq!(changed["clean"], false);
        assert_ne!(first["content_sha256"], changed["content_sha256"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn state_ownership_is_portable_across_supported_targets_but_not_source_or_contracts() {
        let original = build_identity();
        let expected = state_identity_for(&original);
        for target in [
            "aarch64-apple-darwin",
            "x86_64-apple-darwin",
            "aarch64-unknown-linux-gnu",
            "x86_64-unknown-linux-gnu",
        ] {
            let mut remote = original.clone();
            remote.target = target.into();
            assert_eq!(state_identity_for(&remote), expected, "{target}");
        }
        let mut changed = original.clone();
        changed.source.content_sha256 = "0".repeat(64);
        if changed.source.content_sha256 == original.source.content_sha256 {
            changed.source.content_sha256 = "1".repeat(64);
        }
        assert_ne!(state_identity_for(&changed), expected);
        let mut changed = original;
        changed.runner_protocol += 1;
        assert_ne!(state_identity_for(&changed), expected);
    }

    #[test]
    fn compiled_identity_is_complete_and_round_trips_without_runtime_sources() {
        let identity = build_identity();
        assert_eq!(identity.wrapper_protocols, vec![1]);
        assert!(identity.validate());
        assert_eq!(identity.source.content_sha256.len(), 64);
        assert!(!identity.target.is_empty());
        assert_eq!(
            serde_json::from_value::<BuildIdentity>(serde_json::to_value(&identity).unwrap())
                .unwrap(),
            identity
        );
        assert_eq!(state_identity(), state_identity());
        assert!(state_identity().starts_with("sha256:"));
    }
}
