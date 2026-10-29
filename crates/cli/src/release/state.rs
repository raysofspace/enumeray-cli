//! Private local checkpoints. Immutable release evidence is separate from progress.
use crate::{config, manifest::validate_publication};
use enumeray_client::{EnumerayClient, Error};
use enumeray_contracts::{
    management::{PublicationActivation, PublicationCreate},
    numeric::equivalent,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};
use uuid::Uuid;

const MAX_BYTES: usize = 8 * 1024 * 1024;

pub(super) fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Release {
    pub schema_version: u32,
    pub operation_id: Uuid,
    pub api_url: String,
    pub user_id: Uuid,
    pub model_id: String,
    pub candidate: PublicationCreate,
    pub source: Value,
    pub initial_current: Value,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Review {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority: Option<Value>,
    pub expected_current: Option<String>,
    pub candidate: Value,
    pub current: Value,
}

impl PartialEq for Review {
    fn eq(&self, other: &Self) -> bool {
        let same_authority = match (&self.authority, &other.authority) {
            (Some(left), Some(right)) => equivalent(left, right),
            (None, None) => true,
            _ => false,
        };
        self.expected_current == other.expected_current
            && same_authority
            && equivalent(&self.candidate, &other.candidate)
            && equivalent(&self.current, &other.current)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Attempt {
    pub review: Review,
    pub request: PublicationActivation,
    pub response: Option<Value>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Progress {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delegation_attempts: Vec<DelegationAttempt>,
    pub schema_version: u32,
    pub release_sha256: String,
    pub registration_started: bool,
    pub registration_response: Option<Value>,
    pub registered_identity: Option<Value>,
    pub review: Option<Review>,
    pub activation_attempts: Vec<Attempt>,
    pub observed_current: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DelegationAttempt {
    pub id: Uuid,
    pub user_id: Option<Uuid>,
    pub expected_generation: i64,
    pub response: Option<Value>,
}

pub(super) struct Directory {
    pub root: PathBuf,
    _lock: config::PrivateLock,
}

impl Directory {
    pub fn create(root: &Path) -> Result<Self, Error> {
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(root).map_err(|error| if error.kind() == std::io::ErrorKind::AlreadyExists {
            invalid("Release destination already exists; resume its saved operation or choose a new directory.")
        } else { error.into() })?;
        File::open(root)?.sync_all()?;
        File::open(
            root.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?
        .sync_all()?;
        Self::open(root)
    }

    pub fn open(root: &Path) -> Result<Self, Error> {
        let metadata = fs::symlink_metadata(root)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(invalid(
                "Release directory must be a real directory, not a symlink.",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(invalid(
                    "Release directory permits group or other access; set permissions to 0700.",
                ));
            }
        }
        let root = root.canonicalize()?;
        let lock = config::lock_private(
            &root.join("release"),
            "Release directory is already in use; resume after the other command exits.",
        )?;
        Ok(Self { root, _lock: lock })
    }

    pub fn read<T: DeserializeOwned>(&self, name: &str) -> Result<T, Error> {
        let path = self.root.join(name);
        config::require_private(&path)?;
        let mut bytes = Vec::new();
        File::open(path)?
            .take(MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_BYTES {
            return Err(invalid("Release evidence exceeds 8 MiB."));
        }
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub fn write<T: Serialize>(&self, name: &str, value: &T, replace: bool) -> Result<(), Error> {
        let bytes = serde_json::to_vec(value)?;
        if bytes.len() > MAX_BYTES {
            return Err(invalid("Release evidence exceeds 8 MiB."));
        }
        config::write_private_bytes(&self.root.join(name), &bytes, replace)
    }

    pub fn load(&self, client: &EnumerayClient) -> Result<(Release, Progress), Error> {
        let release: Release = self.read("release.json")?;
        if let Err(error) = fs::symlink_metadata(self.root.join("state.json")) {
            if error.kind() == std::io::ErrorKind::NotFound {
                return Err(invalid(
                    "Release progress state.json is missing; preserve this directory. If initialization alone was interrupted before any resume, initialize a new directory from the original author publication.json or, for a publication source, the saved source.publication in release.json. If registration may have started, restore the original progress or inspect and adopt the existing candidate with model release init MODEL --candidate PUBLICATION --directory NEW; do not create a fresh candidate to retry uncertain registration.",
                ));
            }
            return Err(error.into());
        }
        let progress: Progress = self.read("state.json")?;
        if release.schema_version != 1
            || progress.schema_version != 1
            || release.operation_id.is_nil()
        {
            return Err(invalid("Unsupported or invalid release checkpoint."));
        }
        if release.api_url != client.base_url() {
            return Err(invalid(
                "Release belongs to another API/account; use its original profile.",
            ));
        }
        if progress.release_sha256 != digest(&release)?
            || self.read::<Value>("publication.json")? != envelope(&release.candidate)
        {
            return Err(invalid(
                "Saved release evidence changed; preserve this directory and inspect the original operation.",
            ));
        }
        validate_publication(&release.candidate)?;
        if !enumeray_contracts::models::validation::valid_model_id(&release.model_id)
            || (progress.registration_response.is_some() && !progress.registration_started)
            || progress
                .registration_response
                .as_ref()
                .is_some_and(|v| v["id"] != release.candidate.id)
            || (progress.registered_identity.is_some()
                && !progress.registration_started
                && release.source["kind"] != "adopted")
            || (progress.review.is_some() && progress.registered_identity.is_none())
            || progress.review.as_ref().is_some_and(|review| {
                progress
                    .registered_identity
                    .as_ref()
                    .is_none_or(|registered| !equivalent(&review.candidate, registered))
            })
            || progress.delegation_attempts.len() > 128
            || progress.activation_attempts.len() > 128
        {
            return Err(invalid("Inconsistent release progress."));
        }
        for attempt in &progress.activation_attempts {
            let reviewed_delegation = attempt
                .review
                .authority
                .as_ref()
                .and_then(|a| a["publication_delegation_id"].as_str());
            if attempt
                .request
                .publication_delegation_id
                .map(|id| id.to_string())
                .as_deref()
                != reviewed_delegation
            {
                return Err(invalid(
                    "Saved activation delegation differs from its explicit review.",
                ));
            }

            if attempt.request.expected_current_publication_id != attempt.review.expected_current
                || progress
                    .registered_identity
                    .as_ref()
                    .is_none_or(|registered| !equivalent(&attempt.review.candidate, registered))
                || attempt
                    .response
                    .as_ref()
                    .is_some_and(|v| v["id"] != release.candidate.id || v["status"] != "published")
            {
                return Err(invalid(
                    "Saved activation evidence conflicts with this release.",
                ));
            }
        }
        Ok((release, progress))
    }
}

pub(super) fn envelope(candidate: &PublicationCreate) -> Value {
    json!({"manifest_version":1,"kind":"model-publication","spec":candidate})
}

pub(super) fn digest<T: Serialize>(value: &T) -> Result<String, Error> {
    Ok(Sha256::digest(serde_json::to_vec(value)?)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
