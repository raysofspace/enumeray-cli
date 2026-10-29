//! Activity-driven OAuth renewal. Only this module exchanges saved refresh tokens.
use crate::{
    args::Args,
    config::{self, Profile, Profiles},
};
use enumeray_client::{
    AuthConfig, CancellationToken, CredentialLease, CredentialSource, EnumerayClient, Error,
    Identity, OAuthFailureKind, OAuthTokens, SessionErrorCode,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OAuthSession {
    pub user_id: Option<Uuid>,
    issuer: String,
    client_id: String,
    audience: String,
    token_endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    renew_after: Option<u64>,
    /// Allows concurrent commands opened before the one-time legacy migration
    /// to recognize its lineage even after the first process rotates tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    legacy_binding: Option<String>,
    pub renewal: RenewalState,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum RenewalState {
    Ready,
    Exchanging { attempt_id: Uuid },
    VerifyPending { attempt_id: Uuid },
    ReauthRequired { reason: String },
}

impl OAuthSession {
    pub(crate) fn new(auth: &AuthConfig, user_id: Option<Uuid>) -> Self {
        Self {
            user_id,
            issuer: auth.issuer.clone(),
            client_id: auth.client_id.clone(),
            audience: auth.audience.clone(),
            token_endpoint: auth.token_endpoint.clone(),
            renew_after: None,
            legacy_binding: None,
            renewal: RenewalState::Ready,
        }
    }

    fn matches(&self, auth: &AuthConfig) -> bool {
        self.issuer == auth.issuer
            && self.client_id == auth.client_id
            && self.audience == auth.audience
            && self.token_endpoint == auth.token_endpoint
    }
}

fn failure(code: SessionErrorCode, message: &str) -> Error {
    Error::Session {
        code,
        message: message.into(),
    }
}

fn epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn remaining(deadline: Instant) -> Result<Duration, Error> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        Err(failure(
            SessionErrorCode::ProviderUnavailable,
            "Authentication deadline elapsed; retry the same operation when identity services are available.",
        ))
    } else {
        Ok(remaining)
    }
}

fn anonymous(api: &str, deadline: Instant) -> Result<EnumerayClient, Error> {
    Ok(EnumerayClient::new(api, None, remaining(deadline)?)?.with_deadline(deadline))
}

fn identity(profile: &Profile, deadline: Instant) -> Result<Identity, Error> {
    EnumerayClient::new(
        &profile.api_url,
        profile.access_token.clone(),
        remaining(deadline)?,
    )?
    .with_deadline(deadline)
    .me()
}

fn persist(path: &Path, profiles: &Profiles) -> Result<(), Error> {
    config::write_private(path, profiles, true).map_err(|_| failure(
        SessionErrorCode::Persistence,
        "Could not save private authentication state. Preserve the profile file; do not retry an uncertain refresh exchange. Log in again if renewal was interrupted.",
    ))
}

fn cancelled(cancellation: Option<&CancellationToken>) -> Result<(), Error> {
    if cancellation.is_some_and(CancellationToken::is_cancelled) {
        Err(Error::Invalid(
            "Authentication interrupted; resume the same operation.".into(),
        ))
    } else {
        Ok(())
    }
}

fn lock(
    path: &Path,
    deadline: Instant,
    cancellation: Option<&CancellationToken>,
) -> Result<config::PrivateLock, Error> {
    let mut pause = Duration::from_millis(50);
    loop {
        cancelled(cancellation)?;
        if let Some(lock) = config::try_lock_private(path)? {
            return Ok(lock);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(failure(
                SessionErrorCode::ProfileBusy,
                "Another command is changing this profile. Retry the same operation after it finishes.",
            ));
        }
        std::thread::sleep(pause.min(left));
        pause = (pause * 2).min(Duration::from_millis(250));
    }
}

pub(crate) fn store_tokens(profile: &mut Profile, tokens: OAuthTokens, started_at: u64) {
    profile.access_token = Some(tokens.access_token);
    if tokens.refresh_token.is_some() {
        profile.refresh_token = tokens.refresh_token;
    }
    profile.expires_at = tokens
        .expires_in
        .map(|seconds| started_at.saturating_add(seconds));
    if let Some(oauth) = &mut profile.oauth {
        oauth.renew_after = tokens
            .expires_in
            .map(|seconds| started_at.saturating_add(seconds.saturating_sub(30.min(seconds / 10))));
    }
}

fn renewal_at(profile: &Profile) -> Option<u64> {
    profile
        .oauth
        .as_ref()
        .and_then(|oauth| oauth.renew_after)
        .or_else(|| profile.expires_at.map(|expiry| expiry.saturating_sub(30)))
}

fn lease(profile: &Profile) -> CredentialLease {
    let now = Instant::now();
    let renewal = renewal_at(profile)
        .and_then(|at| now.checked_add(Duration::from_secs(at.saturating_sub(epoch()))));
    CredentialLease::new(profile.access_token.clone(), renewal)
}

fn legacy_binding(profile: &Profile) -> String {
    let bytes = serde_json::to_vec(&(
        &profile.api_url,
        &profile.access_token,
        &profile.refresh_token,
    ))
    .expect("Credential strings serialize");
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

struct ProfileSource {
    path: PathBuf,
    name: String,
    api: String,
    initial: Profile,
    expected_epoch: Mutex<Option<Uuid>>,
}

pub(crate) fn client(
    args: &Args,
    path: &Path,
    profiles: &Profiles,
    profile: &Profile,
) -> Result<EnumerayClient, Error> {
    EnumerayClient::with_credentials(
        &profile.api_url,
        Arc::new(ProfileSource {
            path: path.to_owned(),
            name: args.profile.clone(),
            api: profile.api_url.clone(),
            initial: profile.clone(),
            expected_epoch: Mutex::new(profiles.credential_epochs.get(&args.profile).copied()),
        }),
        Duration::from_secs(args.timeout),
    )
}

impl CredentialSource for ProfileSource {
    fn acquire(&self, api_origin: &str, deadline: Instant) -> Result<CredentialLease, Error> {
        self.acquire_inner(api_origin, deadline, None)
    }

    fn acquire_cancellable(
        &self,
        api_origin: &str,
        deadline: Instant,
        cancellation: &CancellationToken,
    ) -> Result<CredentialLease, Error> {
        self.acquire_inner(api_origin, deadline, Some(cancellation))
    }
}

impl ProfileSource {
    fn acquire_inner(
        &self,
        api_origin: &str,
        deadline: Instant,
        cancellation: Option<&CancellationToken>,
    ) -> Result<CredentialLease, Error> {
        cancelled(cancellation)?;
        if api_origin != self.api {
            return Err(failure(
                SessionErrorCode::Changed,
                "Authentication belongs to another API; select its original profile.",
            ));
        }
        let mut expected = loop {
            cancelled(cancellation)?;
            match self.expected_epoch.try_lock() {
                Ok(guard) => break guard,
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    return Err(failure(
                        SessionErrorCode::Changed,
                        "Authentication state is unavailable; retry this command.",
                    ));
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Err(failure(
                            SessionErrorCode::ProfileBusy,
                            "Another request is renewing this profile; retry the same operation after it finishes.",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(50).min(left));
                }
            }
        };
        let _lock = lock(&self.path, deadline, cancellation)?;
        let mut profiles = config::load(&self.path)?;
        let mut profile = profiles.profiles.get(&self.name).cloned().ok_or_else(|| failure(SessionErrorCode::Changed, "This profile changed or was removed; select the original API/account and repeat the same operation."))?;
        if profile.api_url != self.api {
            return Err(failure(
                SessionErrorCode::Changed,
                "This profile now selects another API; use the original API/account and repeat the same operation.",
            ));
        }
        let current_epoch = profiles.credential_epochs.get(&self.name).copied();
        if let Some(wanted) = *expected {
            if current_epoch != Some(wanted) {
                return Err(changed());
            }
        } else if current_epoch.is_some() {
            // A second process may have bootstrapped this unchanged legacy profile.
            let same_tokens = profile.access_token == self.initial.access_token
                && profile.refresh_token == self.initial.refresh_token;
            let migrated = profile
                .oauth
                .as_ref()
                .and_then(|oauth| oauth.legacy_binding.as_deref())
                == Some(legacy_binding(&self.initial).as_str());
            if !same_tokens && !migrated {
                return Err(changed());
            }
            *expected = current_epoch;
        } else if profile.access_token != self.initial.access_token
            || profile.refresh_token != self.initial.refresh_token
        {
            return Err(changed());
        }
        let bootstrapped = profile.oauth.is_none();
        let mut bootstrap_auth = None;
        if bootstrapped {
            if profile.expires_at.is_some_and(|at| at <= epoch()) {
                return Err(failure(
                    SessionErrorCode::LoginRequired,
                    "This legacy profile has no verified account binding. Run auth refresh once with the original profile to enable automatic renewal; log in again if refresh is rejected.",
                ));
            }
            let auth = anonymous(&self.api, deadline)?.auth_config()?;
            cancelled(cancellation)?;
            let user = identity(&profile, deadline)?;
            let mut oauth = OAuthSession::new(&auth, Some(user.user_id));
            oauth.legacy_binding = Some(legacy_binding(&profile));
            profile.oauth = Some(oauth);
            bootstrap_auth = Some(auth);
        }
        if profile.oauth.as_ref().is_some_and(|oauth| {
            oauth.user_id.is_none() && matches!(oauth.renewal, RenewalState::Ready)
        }) {
            return Err(failure(
                SessionErrorCode::LoginRequired,
                "This profile's account binding has not been verified. Complete auth refresh explicitly with the original profile before automatic renewal.",
            ));
        }
        if expected.is_none() {
            let binding = Uuid::new_v4();
            profiles
                .credential_epochs
                .insert(self.name.clone(), binding);
            *expected = Some(binding);
        }
        if bootstrapped || current_epoch.is_none() {
            profiles.profiles.insert(self.name.clone(), profile.clone());
            persist(&self.path, &profiles)?;
        }
        let (profile, _) = usable(
            &self.path,
            &mut profiles,
            &self.name,
            profile,
            RenewalRequest {
                deadline,
                force: false,
                cancellation,
                auth: bootstrap_auth,
            },
        )?;
        Ok(lease(&profile))
    }
}

fn changed() -> Error {
    failure(
        SessionErrorCode::Changed,
        "The selected profile was logged out, reconfigured, or replaced during this command. Select the original API/account and repeat the same saved operation.",
    )
}

struct RenewalRequest<'a> {
    deadline: Instant,
    force: bool,
    cancellation: Option<&'a CancellationToken>,
    auth: Option<AuthConfig>,
}

fn usable(
    path: &Path,
    profiles: &mut Profiles,
    name: &str,
    mut profile: Profile,
    request: RenewalRequest<'_>,
) -> Result<(Profile, Option<Identity>), Error> {
    let RenewalRequest {
        deadline,
        force,
        cancellation,
        auth,
    } = request;
    let state = profile
        .oauth
        .as_ref()
        .expect("OAuth metadata established")
        .renewal
        .clone();
    match state {
        RenewalState::Exchanging { .. } => {
            return Err(failure(
                SessionErrorCode::RenewalUncertain,
                "A previous refresh exchange was interrupted and may have consumed its refresh token. Run auth login again with the same API/account; preserve and resume the original operation.",
            ));
        }
        RenewalState::ReauthRequired { .. } => {
            return Err(failure(
                SessionErrorCode::LoginRequired,
                "This session requires another login. Run auth login with the same API/account, then resume the original operation.",
            ));
        }
        RenewalState::VerifyPending { .. } => {
            // The saved replacement refresh token is known, even when an API
            // outage outlasted its access token. Renew that current credential
            // while retaining the original expected account, then verify again.
            if profile.expires_at.is_none_or(|at| at > epoch()) {
                let user = verify(path, profiles, name, &mut profile, deadline)?;
                if !force {
                    return Ok((profile, Some(user)));
                }
            }
        }
        RenewalState::Ready => {}
    }
    if !force && !renewal_at(&profile).is_some_and(|at| at <= epoch()) {
        return Ok((profile, None));
    }
    cancelled(cancellation)?;
    let auth = match auth {
        Some(auth) => auth,
        None => anonymous(&profile.api_url, deadline)?.auth_config()?,
    };
    if !profile
        .oauth
        .as_ref()
        .expect("OAuth metadata")
        .matches(&auth)
    {
        return Err(failure(
            SessionErrorCode::Changed,
            "The API's identity provider or OAuth client changed. Run auth login explicitly; saved refresh credentials were not sent to the new provider.",
        ));
    }
    let refresh = profile.refresh_token.clone().ok_or_else(|| {
        failure(
            SessionErrorCode::LoginRequired,
            "This profile has no refresh token; log in again or replace its scoped token.",
        )
    })?;
    let attempt_id = Uuid::new_v4();
    cancelled(cancellation)?;
    remaining(deadline)?;
    profile.oauth.as_mut().expect("OAuth metadata").renewal =
        RenewalState::Exchanging { attempt_id };
    profiles.profiles.insert(name.into(), profile.clone());
    persist(path, profiles)?;
    if let Err(error) = cancelled(cancellation).and_then(|()| remaining(deadline).map(|_| ())) {
        profile.oauth.as_mut().expect("OAuth metadata").renewal = RenewalState::Ready;
        profiles.profiles.insert(name.into(), profile);
        persist(path, profiles)?;
        return Err(error);
    }
    let started_at = epoch();
    let result = enumeray_client::auth::refresh_until(&auth, &refresh, deadline);
    let tokens = match result {
        Ok(tokens) => tokens,
        Err(error) => {
            let (state, output) = match &error {
                Error::OAuth {
                    kind: OAuthFailureKind::InvalidGrant,
                    ..
                } => (
                    RenewalState::ReauthRequired {
                        reason: "refresh_rejected".into(),
                    },
                    failure(
                        SessionErrorCode::LoginRequired,
                        "The refresh credential expired or was revoked. Run auth login again with the same API/account, then resume the original operation.",
                    ),
                ),
                Error::OAuth {
                    kind: OAuthFailureKind::TemporarilyUnavailable,
                    ..
                } => (
                    RenewalState::Ready,
                    failure(
                        SessionErrorCode::ProviderUnavailable,
                        "The identity provider is temporarily unavailable. Retry the same operation later; no business request was sent.",
                    ),
                ),
                Error::OAuth {
                    kind: OAuthFailureKind::Configuration,
                    ..
                } => (
                    RenewalState::Ready,
                    failure(
                        SessionErrorCode::ProviderUnavailable,
                        "The identity provider refused this OAuth client configuration. Ask the service administrator to restore the configured client; credentials were retained.",
                    ),
                ),
                Error::Transport(transport) if transport.is_connect() => (
                    RenewalState::Ready,
                    failure(
                        SessionErrorCode::ProviderUnavailable,
                        "The identity provider could not be reached. Retry the same operation later; credentials were retained.",
                    ),
                ),
                _ => (
                    RenewalState::Exchanging { attempt_id },
                    failure(
                        SessionErrorCode::RenewalUncertain,
                        "The refresh exchange outcome is unknown. Do not reuse the old refresh token. Run auth login again with the same API/account, then resume the original operation.",
                    ),
                ),
            };
            profile.oauth.as_mut().expect("OAuth metadata").renewal = state;
            profiles.profiles.insert(name.into(), profile);
            persist(path, profiles)?;
            return Err(output);
        }
    };
    store_tokens(&mut profile, tokens, started_at);
    profile.oauth.as_mut().expect("OAuth metadata").renewal =
        RenewalState::VerifyPending { attempt_id };
    profiles.profiles.insert(name.into(), profile.clone());
    persist(path, profiles)?;
    cancelled(cancellation)?;
    let user = verify(path, profiles, name, &mut profile, deadline)?;
    Ok((profile, Some(user)))
}

fn verify(
    path: &Path,
    profiles: &mut Profiles,
    name: &str,
    profile: &mut Profile,
    deadline: Instant,
) -> Result<Identity, Error> {
    let user = match identity(profile, deadline) {
        Ok(user) => user,
        Err(
            error @ Error::Api {
                status: 401 | 403, ..
            },
        ) => {
            profile.oauth.as_mut().expect("OAuth metadata").renewal =
                RenewalState::ReauthRequired {
                    reason: "access_rejected".into(),
                };
            profiles.profiles.insert(name.into(), profile.clone());
            persist(path, profiles)?;
            return Err(error);
        }
        Err(error) => return Err(error),
    };
    let oauth = profile.oauth.as_mut().expect("OAuth metadata");
    if oauth
        .user_id
        .is_some_and(|expected| expected != user.user_id)
    {
        oauth.renewal = RenewalState::ReauthRequired {
            reason: "account_mismatch".into(),
        };
        profiles.profiles.insert(name.into(), profile.clone());
        persist(path, profiles)?;
        return Err(failure(
            SessionErrorCode::AccountMismatch,
            "Renewed credentials identify another account. No operation was sent. Log in explicitly with the original API/account before resuming.",
        ));
    }
    oauth.user_id = Some(user.user_id);
    oauth.renewal = RenewalState::Ready;
    profiles.profiles.insert(name.into(), profile.clone());
    persist(path, profiles)?;
    Ok(user)
}

pub(crate) fn refresh(args: &Args, path: &Path) -> Result<Identity, Error> {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(args.timeout))
        .ok_or_else(|| Error::Invalid("HTTP timeout is too large.".into()))?;
    let _lock = lock(path, deadline, None)?;
    let mut profiles = config::load(path)?;
    let mut profile = profiles
        .profiles
        .get(&args.profile)
        .cloned()
        .ok_or_else(|| Error::Invalid("This profile is not configured.".into()))?;
    if let Some(api) = &args.api
        && anonymous(api, deadline)?.base_url() != profile.api_url
    {
        return Err(Error::Invalid(
            "Refresh must use the profile's original API.".into(),
        ));
    }
    if profile.refresh_token.is_none() {
        return Err(Error::Invalid("No refresh token; log in again.".into()));
    }
    let mut bootstrap_auth = None;
    if profile.oauth.is_none() {
        let auth = anonymous(&profile.api_url, deadline)?.auth_config()?;
        let mut oauth = OAuthSession::new(&auth, None);
        oauth.legacy_binding = Some(legacy_binding(&profile));
        profile.oauth = Some(oauth);
        bootstrap_auth = Some(auth);
    }
    profiles
        .credential_epochs
        .entry(args.profile.clone())
        .or_insert_with(Uuid::new_v4);
    let was_pending = matches!(
        profile.oauth.as_ref().expect("OAuth metadata").renewal,
        RenewalState::VerifyPending { .. }
    );
    let (_, user) = usable(
        path,
        &mut profiles,
        &args.profile,
        profile,
        RenewalRequest {
            deadline,
            force: !was_pending,
            cancellation: None,
            auth: bootstrap_auth,
        },
    )?;
    Ok(user.expect("Explicit refresh verifies identity"))
}

pub(crate) fn login_snapshot(path: &Path, name: &str) -> Result<(Profiles, Uuid), Error> {
    let _lock = config::lock_private(
        path,
        "Profiles are already being changed by another command; retry after it exits.",
    )?;
    let mut profiles = config::load(path)?;
    let binding = *profiles
        .credential_epochs
        .entry(name.into())
        .or_insert_with(Uuid::new_v4);
    persist(path, &profiles)?;
    Ok((profiles, binding))
}

pub(crate) fn commit_login(
    path: &Path,
    name: &str,
    expected: Uuid,
    profile: Profile,
) -> Result<(), Error> {
    let _lock = config::lock_private(
        path,
        "Profiles are already being changed by another command; retry after it exits.",
    )?;
    let mut profiles = config::load(path)?;
    if profiles.credential_epochs.get(name) != Some(&expected) {
        return Err(changed());
    }
    profiles.profiles.insert(name.into(), profile);
    profiles
        .credential_epochs
        .insert(name.into(), Uuid::new_v4());
    persist(path, &profiles)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn original() -> Profile {
        let mut profile = Profile::new("https://api.example/".into());
        profile.access_token = Some("original-access".into());
        profile.refresh_token = Some("original-refresh".into());
        profile.expires_at = Some(u64::MAX);
        profile
    }

    fn source(path: &Path, initial: Profile) -> ProfileSource {
        ProfileSource {
            path: path.into(),
            name: "default".into(),
            api: initial.api_url.clone(),
            initial,
            expected_epoch: Mutex::new(None),
        }
    }

    #[test]
    fn concurrent_legacy_snapshot_recognizes_migration_after_tokens_have_rotated() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("profiles.json");
        let original = original();
        let source = source(&path, original.clone());
        let mut migrated = original.clone();
        let mut oauth = OAuthSession::new(
            &AuthConfig {
                issuer: "https://identity.example/".into(),
                client_id: "cli".into(),
                audience: "api".into(),
                authorization_endpoint: "https://identity.example/auth".into(),
                token_endpoint: "https://identity.example/token".into(),
                scopes: vec![],
            },
            Some(Uuid::new_v4()),
        );
        oauth.legacy_binding = Some(legacy_binding(&original));
        oauth.renew_after = Some(u64::MAX);
        migrated.oauth = Some(oauth);
        migrated.access_token = Some("replacement-access".into());
        migrated.refresh_token = Some("replacement-refresh".into());
        let mut profiles = Profiles::default();
        let binding = Uuid::new_v4();
        profiles.credential_epochs.insert("default".into(), binding);
        profiles.profiles.insert("default".into(), migrated);
        config::write_private(&path, &profiles, true).unwrap();
        source
            .acquire(
                "https://api.example/",
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(*source.expected_epoch.lock().unwrap(), Some(binding));
        let saved = config::load(&path).unwrap();
        assert_eq!(
            saved.profiles["default"].refresh_token.as_deref(),
            Some("replacement-refresh")
        );
    }

    #[test]
    fn cancelled_file_wait_and_bounded_in_process_wait_do_not_start_renewal() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("profiles.json");
        let source = Arc::new(source(&path, original()));
        let cancellation = CancellationToken::new();
        let canceller = cancellation.clone();
        let _file_lock = config::lock_private(&path, "locked").unwrap();
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            canceller.cancel();
        });
        let started = Instant::now();
        assert!(
            source
                .acquire_cancellable(
                    "https://api.example/",
                    started + Duration::from_secs(1),
                    &cancellation
                )
                .is_err()
        );
        worker.join().unwrap();
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(!path.exists());

        let _memory_lock = source.expected_epoch.lock().unwrap();
        let waiter = source.clone();
        let started = Instant::now();
        let result = std::thread::spawn(move || {
            waiter.acquire(
                "https://api.example/",
                Instant::now() + Duration::from_millis(20),
            )
        })
        .join()
        .unwrap();
        assert!(matches!(
            result,
            Err(Error::Session {
                code: SessionErrorCode::ProfileBusy,
                ..
            })
        ));
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(!path.exists());
    }

    #[test]
    fn failed_legacy_refresh_cannot_enable_unverified_automatic_credentials() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("profiles.json");
        let mut profile = original();
        profile.oauth = Some(OAuthSession::new(
            &AuthConfig {
                issuer: "https://identity.example/".into(),
                client_id: "cli".into(),
                audience: "api".into(),
                authorization_endpoint: "https://identity.example/auth".into(),
                token_endpoint: "https://identity.example/token".into(),
                scopes: vec![],
            },
            None,
        ));
        let source = source(&path, profile.clone());
        let mut profiles = Profiles::default();
        profiles.profiles.insert("default".into(), profile);
        config::write_private(&path, &profiles, true).unwrap();
        let result = source.acquire(
            "https://api.example/",
            Instant::now() + Duration::from_secs(1),
        );
        assert!(matches!(
            result,
            Err(Error::Session {
                code: SessionErrorCode::LoginRequired,
                ..
            })
        ));
    }
}
