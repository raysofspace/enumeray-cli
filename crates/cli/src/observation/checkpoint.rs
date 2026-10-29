//! One writer per private checkpoint. The stable lock file survives atomic replacements.
use crate::config;
use enumeray_client::Error;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    schema_version: u32,
    api_url: String,
    user_id: Uuid,
    run_id: String,
    cursor: Option<String>,
    last_position: i64,
}

pub(super) struct Checkpoint {
    path: Option<PathBuf>,
    saved: Saved,
    _lock: Option<config::PrivateLock>,
}

impl Checkpoint {
    pub(super) fn open(
        path: Option<&Path>,
        api: &str,
        user: Uuid,
        run: &str,
        cursor: Option<&str>,
    ) -> Result<Self, Error> {
        let mut saved = Saved {
            schema_version: 1,
            api_url: api.into(),
            user_id: user,
            run_id: run.into(),
            cursor: cursor.map(str::to_owned),
            last_position: 0,
        };
        let mut lock = None;
        if let Some(path) = path {
            lock = Some(config::lock_private(
                path,
                "Log checkpoint is already in use or cannot be locked; use one observer per state file.",
            )?);
            match std::fs::symlink_metadata(path) {
                Ok(_) => {
                    config::require_private(path)?;
                    let retained: Saved = config::read_json(path)?;
                    if retained.schema_version != 1
                        || retained.api_url != api
                        || retained.user_id != user
                        || retained.run_id != run
                        || retained.last_position < 0
                        || retained.cursor.as_deref().is_some_and(|s| {
                            s.is_empty() || s.len() > 1024 || s.chars().any(char::is_control)
                        })
                    {
                        return Err(Error::Invalid("Log checkpoint belongs to another API, account or run, or has an unsupported/invalid format.".into()));
                    }
                    saved = retained;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    config::write_private(path, &saved, false)?
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(Self {
            path: path.map(Path::to_path_buf),
            saved,
            _lock: lock,
        })
    }

    pub(super) fn cursor(&self) -> Option<&str> {
        self.saved.cursor.as_deref()
    }
    pub(super) fn position(&self) -> i64 {
        self.saved.last_position
    }

    /// Called only after the corresponding output has been flushed successfully.
    pub(super) fn advance(&mut self, cursor: Option<&str>, position: i64) -> Result<(), Error> {
        let mut next = self.saved.clone();
        if let Some(cursor) = cursor {
            next.cursor = Some(cursor.into());
        }
        next.last_position = position.max(next.last_position);
        if let Some(path) = &self.path {
            config::write_private(path, &next, true)?;
        }
        self.saved = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const API: &str = "https://example.org/";

    #[test]
    fn checkpoint_is_bound_private_atomic_and_exclusively_locked() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("private/state.json");
        let user = Uuid::new_v4();
        let mut state = Checkpoint::open(Some(&path), API, user, "run", None).unwrap();
        assert!(Checkpoint::open(Some(&path), API, user, "run", None).is_err());
        state.advance(Some("cursor-one"), 7).unwrap();
        assert!(Checkpoint::open(Some(&path), API, user, "run", None).is_err());
        drop(state);
        let state = Checkpoint::open(Some(&path), API, user, "run", None).unwrap();
        assert_eq!(state.cursor(), Some("cursor-one"));
        assert_eq!(state.position(), 7);
        drop(state);
        for (api, account, run) in [
            ("https://elsewhere.org/", user, "run"),
            (API, Uuid::new_v4(), "run"),
            (API, user, "other"),
        ] {
            assert!(Checkpoint::open(Some(&path), api, account, run, None).is_err());
            // Failed validation must also release ownership for the next observer.
            let state = Checkpoint::open(Some(&path), API, user, "run", None).unwrap();
            assert_eq!(state.cursor(), Some("cursor-one"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(Checkpoint::open(Some(&path), API, user, "run", None).is_err());
        }
    }

    #[test]
    fn failed_checkpoint_replacement_retains_committed_cursor() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let mut state = Checkpoint::open(Some(&path), API, Uuid::nil(), "run", None).unwrap();
        state.advance(Some("old"), 1).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(state.advance(Some("new"), 2).is_err());
        assert_eq!(state.cursor(), Some("old"));
        assert_eq!(state.position(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn state_and_lock_symlinks_are_rejected_without_touching_targets() {
        use std::os::unix::fs::symlink;
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        std::fs::write(&target, "original").unwrap();
        for name in ["state.json", "state.json.lock"] {
            let path = directory.path().join("state.json");
            let link = directory.path().join(name);
            symlink(&target, &link).unwrap();
            assert!(Checkpoint::open(Some(&path), API, Uuid::nil(), "run", None).is_err());
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "original");
            std::fs::remove_file(&link).unwrap();
            let _ = std::fs::remove_file(directory.path().join("state.json.lock"));
        }
    }
}
