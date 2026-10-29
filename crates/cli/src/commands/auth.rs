//! Auth command workflows.

use super::{
    mutate, paged_get, require_new_output, save_secret_output, secret_request_error, stdin_secret,
};
use crate::{
    args::{Args, Auth},
    config::{self, client},
    session,
};
use enumeray_client::auth::LoginAttempt;
use enumeray_client::{EnumerayClient, Error};
use reqwest::Method;
use serde_json::{Value, json};
use std::{
    path::Path,
    time::{Duration, UNIX_EPOCH},
};

pub(crate) fn auth(args: &Args, command: &Auth, path: &Path) -> Result<Value, Error> {
    let profiles = config::load(path)?;
    match command {
        Auth::Profiles => Ok(
            json!({"profiles":profiles.profiles.iter().map(|(name,p)| json!({"name":name,"api_url":p.api_url,"authenticated":p.access_token.is_some(),"expires_at":p.expires_at})).collect::<Vec<_>>()}),
        ),
        Auth::Configure { api_url } => {
            let _lock = config::lock_private(
                path,
                "Profiles are already being changed by another command; retry after it exits.",
            )?;
            let mut profiles = config::load(path)?;
            let new = EnumerayClient::new(api_url, None, Duration::from_secs(args.timeout))?;
            if !profiles
                .profiles
                .get(&args.profile)
                .is_some_and(|p| p.api_url == new.base_url())
            {
                profiles.profiles.insert(
                    args.profile.clone(),
                    config::Profile::new(new.base_url().to_owned()),
                );
                profiles
                    .credential_epochs
                    .insert(args.profile.clone(), uuid::Uuid::new_v4());
            }
            config::write_private(path, &profiles, true)?;
            Ok(json!({"profile":args.profile,"api_url":new.base_url()}))
        }
        Auth::Logout => {
            let _lock = config::lock_private(
                path,
                "Profiles are already being changed by another command; retry after it exits.",
            )?;
            let mut profiles = config::load(path)?;
            if let Some(profile) = profiles.profiles.get_mut(&args.profile) {
                profile.access_token = None;
                profile.refresh_token = None;
                profile.expires_at = None;
                profile.oauth = None;
            }
            profiles
                .credential_epochs
                .insert(args.profile.clone(), uuid::Uuid::new_v4());
            config::write_private(path, &profiles, true)?;
            Ok(json!({"logged_out":true,"profile":args.profile,"server_tokens_revoked":false}))
        }
        Auth::Login {
            token_stdin,
            no_browser,
        } => {
            let (profiles, binding) = session::login_snapshot(path, &args.profile)?;
            let api = args
                .api
                .as_deref()
                .or_else(|| {
                    profiles
                        .profiles
                        .get(&args.profile)
                        .map(|p| p.api_url.as_str())
                })
                .ok_or_else(|| {
                    Error::Invalid("Use --api or auth configure before login.".into())
                })?;
            let anonymous = EnumerayClient::new(api, None, Duration::from_secs(args.timeout))?;
            let mut profile = config::Profile::new(anonymous.base_url().to_owned());
            if *token_stdin {
                profile.access_token = Some(stdin_secret()?);
            } else {
                let auth_config = anonymous.auth_config()?;
                let attempt = LoginAttempt::begin(&auth_config)?;
                eprintln!(
                    "Complete login in your browser:\n{}",
                    attempt.authorization_url()
                );
                if !no_browser {
                    open_browser(attempt.authorization_url());
                }
                let (tokens, exchanged_at) =
                    attempt.finish_with_timing(&auth_config, Duration::from_secs(180))?;
                profile.oauth = Some(session::OAuthSession::new(&auth_config, None));
                session::store_tokens(
                    &mut profile,
                    tokens,
                    exchanged_at
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                );
            }
            let identity = EnumerayClient::new(
                &profile.api_url,
                profile.access_token.clone(),
                Duration::from_secs(args.timeout),
            )?
            .me()?;
            if let Some(oauth) = &mut profile.oauth {
                oauth.user_id = Some(identity.user_id);
            }
            session::commit_login(path, &args.profile, binding, profile)?;
            Ok(json!({"authenticated":true,"profile":args.profile,"identity":identity}))
        }
        Auth::Refresh => {
            let identity = session::refresh(args, path)?;
            Ok(json!({"refreshed":true,"identity":identity}))
        }
        Auth::Whoami => Ok(serde_json::to_value(client(args, &profiles)?.me()?)?),
        Auth::TokenList { page } => paged_get(&client(args, &profiles)?, &["tokens"], page),
        Auth::TokenCreate {
            name,
            scopes,
            expires_in_seconds,
            output,
        } => {
            let _output_lock = require_new_output(output)?;
            let recovery = "Inspect auth token-list and revoke unused tokens before explicitly creating another token.";
            let value: Value = client(args, &profiles)?
                .post(
                    "/api/v1/tokens",
                    &json!({"name":name,"scopes":scopes,"expires_in_seconds":expires_in_seconds}),
                )
                .map_err(|error| secret_request_error(error, "Token creation", recovery))?;
            save_secret_output(output, &value, "Token creation", recovery)?;
            Ok(json!({"saved_to":output,"id":value.get("id"),"expires_at":value.get("expires_at")}))
        }
        Auth::TokenRevoke { id } => mutate(
            &client(args, &profiles)?,
            Method::DELETE,
            &["tokens", id],
            &Value::Null,
        ),
    }
}

fn open_browser(url: &str) {
    let command = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    match std::process::Command::new(command)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        Ok(status) if status.success() => {}
        _ => eprintln!("Open the URL above manually; browser launch is unavailable."),
    }
}
