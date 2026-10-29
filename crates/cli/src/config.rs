use crate::args::Args;
use enumeray_client::EnumerayClient;
use enumeray_client::Error;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::time::Duration;
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

const MAX_JSON_BYTES: usize = 1024 * 1024;

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profiles {
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
    /// Independent tombstones prevent an unfinished login from undoing logout.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub credential_epochs: BTreeMap<String, uuid::Uuid>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub api_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<crate::session::OAuthSession>,
}
impl Profile {
    pub fn new(api_url: String) -> Self {
        Self {
            api_url,
            access_token: None,
            refresh_token: None,
            expires_at: None,
            oauth: None,
        }
    }
}

pub fn default_path() -> Result<PathBuf, Error> {
    if let Some(base) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(base).join("enumeray/profiles.json"));
    }
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".config/enumeray/profiles.json"))
        .ok_or_else(|| Error::Invalid("HOME is unset; specify --config.".into()))
}

pub fn load(path: &Path) -> Result<Profiles, Error> {
    if !path.exists() {
        return Ok(Profiles::default());
    }
    require_private(path)?;
    read_json(path)
}

pub fn require_private(path: &Path) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(Error::Invalid(
            "Credential and prepared-request files must be regular files, not symlinks.".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(Error::Invalid(
                "Private file permits group or other access; set its permissions to 0600.".into(),
            ));
        }
    }
    Ok(())
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, Error> {
    if !fs::metadata(path)?.is_file() {
        return Err(Error::Invalid("JSON input must be a regular file.".into()));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_JSON_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_JSON_BYTES {
        return Err(Error::Invalid("JSON file exceeds 1 MiB.".into()));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

pub(crate) fn read_inputs(path: &Path) -> Result<serde_json::Value, Error> {
    if !fs::metadata(path)?.is_file() {
        return Err(Error::Invalid(
            "Scientific inputs must be a regular file.".into(),
        ));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(enumeray_contracts::models::MAX_INPUT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    enumeray_contracts::models::parse_inputs(&bytes)
        .map_err(|error| Error::Invalid(error.to_string()))
}

/// An owned sidecar lock, released explicitly before its file is closed.
pub(crate) struct PrivateLock(fs::File);

impl Drop for PrivateLock {
    fn drop(&mut self) {
        // Closing alone can leave the lock held by retained references, including
        // transient Linux /proc descriptor inspection or inherited descriptors.
        // End ownership synchronously before closing our descriptor.
        let _ = self.0.unlock();
    }
}

/// Lock a stable sidecar rather than a state file replaced by atomic rename.
/// Keep the sidecar after release so subsequent writers always lock the same inode.
pub(crate) fn lock_private(path: &Path, message: &str) -> Result<PrivateLock, Error> {
    try_lock_private(path)?.ok_or_else(|| Error::Invalid(message.into()))
}

/// Contention is separate from invalid or inaccessible private state.
pub(crate) fn try_lock_private(path: &Path) -> Result<Option<PrivateLock>, Error> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    create_private_dir(parent)?;
    let mut lock_path = path.as_os_str().to_os_string();
    lock_path.push(".lock");
    let lock_path = PathBuf::from(lock_path);
    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = match options.create_new(true).open(&lock_path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            require_private(&lock_path)?;
            options.create_new(false).open(&lock_path)?
        }
        Err(error) => return Err(error.into()),
    };
    require_private(&lock_path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(PrivateLock(file))),
        Err(fs::TryLockError::WouldBlock) => Ok(None),
        Err(fs::TryLockError::Error(error)) => Err(error.into()),
    }
}

/// Atomic replace with an fsynced, private temporary file in the same directory.
/// New prepared requests use create-new semantics so an existing operation is never replaced.
pub fn write_private<T: Serialize>(path: &Path, value: &T, replace: bool) -> Result<(), Error> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    if bytes.len() > MAX_JSON_BYTES {
        bytes = serde_json::to_vec(value)?;
    }
    if bytes.len() > MAX_JSON_BYTES {
        return Err(Error::Invalid("JSON file exceeds 1 MiB.".into()));
    }
    write_private_bytes(path, &bytes, replace)
}

pub(crate) fn write_private_bytes(path: &Path, bytes: &[u8], replace: bool) -> Result<(), Error> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    create_private_dir(parent)?;
    if path.exists() {
        require_private(path)?;
        if !replace {
            return Err(Error::Invalid(
                "The output file already exists; submit the saved request or choose another file."
                    .into(),
            ));
        }
    }
    let temporary = parent.join(format!(".enumeray-{}.tmp", uuid::Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<(), Error> {
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        if replace {
            fs::rename(&temporary, path)?;
        } else {
            // Hard linking is atomic create-if-absent; rename would silently overwrite a racer.
            fs::hard_link(&temporary, path)?;
            fs::remove_file(&temporary)?;
        }
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(crate) fn create_private_dir(path: &Path) -> Result<(), Error> {
    if path.is_dir() {
        return Ok(());
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    Ok(())
}

pub(crate) fn client(args: &Args, profiles: &Profiles) -> Result<EnumerayClient, Error> {
    let profile = profiles.profiles.get(&args.profile);
    let api = args
        .api
        .as_deref()
        .or_else(|| profile.map(|p| p.api_url.as_str()))
        .ok_or_else(|| {
            Error::Invalid("Configure an API using `auth configure URL` or supply --api.".into())
        })?;
    let canonical = EnumerayClient::new(api, None, Duration::from_secs(args.timeout))?;
    let profile_token = profile
        .filter(|p| p.api_url == canonical.base_url())
        .and_then(|p| p.access_token.clone());
    let token = std::env::var("ENUMERAY_TOKEN")
        .ok()
        .filter(|token| !token.is_empty())
        .or(profile_token);
    if std::env::var("ENUMERAY_TOKEN").is_ok_and(|token| !token.is_empty()) {
        return EnumerayClient::new(api, token, Duration::from_secs(args.timeout));
    }
    if let Some(profile) = profile.filter(|p| p.api_url == canonical.base_url())
        && profile.refresh_token.is_some()
    {
        let path = args.config.clone().map(Ok).unwrap_or_else(default_path)?;
        return crate::session::client(args, &path, profiles, profile);
    }
    EnumerayClient::new(api, token, Duration::from_secs(args.timeout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn private_lock_drop_releases_ownership_with_a_retained_descriptor() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let owner = lock_private(&path, "locked").unwrap();
        // Model an inherited or otherwise retained reference to the open file.
        let retained = owner.0.try_clone().unwrap();
        assert!(lock_private(&path, "locked").is_err());
        drop(owner);

        let next = lock_private(&path, "locked").unwrap();
        drop(retained);
        assert!(lock_private(&path, "locked").is_err());
        drop(next);
        assert!(lock_private(&path, "locked").is_ok());
    }

    #[test]
    fn private_writes_do_not_clobber_prepared_operations() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state/request.json");
        write_private(&path, &"original", false).unwrap();
        require_private(&path).unwrap();
        assert!(write_private(&path, &"changed", false).is_err());
        assert_eq!(read_json::<String>(&path).unwrap(), "original");
        write_private(&path, &"refreshed", true).unwrap();
        assert_eq!(read_json::<String>(&path).unwrap(), "refreshed");
    }
    #[test]
    fn private_json_remains_readable_when_indentation_exceeds_the_read_limit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("request.json");
        let mut inputs = serde_json::json!(vec![0; 20_000]);
        for _ in 0..62 {
            inputs = serde_json::json!([inputs]);
        }
        enumeray_contracts::models::validate_input_value(&inputs).unwrap();
        assert!(serde_json::to_vec_pretty(&inputs).unwrap().len() > MAX_JSON_BYTES);
        write_private(&path, &inputs, false).unwrap();
        assert_eq!(read_json::<serde_json::Value>(&path).unwrap(), inputs);
        assert!(write_private(&path, &"x".repeat(MAX_JSON_BYTES), true).is_err());
        assert_eq!(read_json::<serde_json::Value>(&path).unwrap(), inputs);
    }
    #[cfg(unix)]
    #[test]
    fn credentials_refuse_symlinks_and_world_readable_files() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let directory = tempfile::tempdir().unwrap();
        let original = directory.path().join("original");
        write_private(&original, &Profiles::default(), false).unwrap();
        let link = directory.path().join("link");
        symlink(&original, &link).unwrap();
        assert!(load(&link).is_err());
        fs::set_permissions(&original, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load(&original).is_err());
    }
}
