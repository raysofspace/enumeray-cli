//! Command workflows and shared request/secret boundary helpers.

mod access;
mod artifact;
mod auth;
mod connector;
mod connector_resource;
mod model;
mod run;
mod service;

pub(crate) use access::access;
pub(crate) use artifact::artifact;
pub(crate) use auth::auth;
pub(crate) use connector::connector;

pub(crate) use model::model;
pub(crate) use run::run;
pub(crate) use service::service;

use enumeray_client::{EnumerayClient, Error, resource_path};
use enumeray_contracts::management::{GenerationChange, MembershipChange};
use reqwest::Method;
use serde::Serialize;
use serde_json::Value;
use std::{io::Read, path::Path};

fn get(client: &EnumerayClient, segments: &[&str]) -> Result<Value, Error> {
    client.get(&resource_path(segments)?)
}

fn mutate<T: Serialize>(
    client: &EnumerayClient,
    method: Method,
    segments: &[&str],
    body: &T,
) -> Result<Value, Error> {
    client.request(method, &resource_path(segments)?, Some(body), None)
}

pub(crate) fn paged_get(
    client: &EnumerayClient,
    segments: &[&str],
    page: &crate::args::PageArgs,
) -> Result<Value, Error> {
    query_get(client, segments, page.query())
}

fn query_get(
    client: &EnumerayClient,
    segments: &[&str],
    query: Vec<(&str, String)>,
) -> Result<Value, Error> {
    let path = resource_path(segments)?;
    if query.is_empty() {
        return client.get(&path);
    }
    // Serialize every value once; cursors and filter timestamps remain opaque.
    let mut url = reqwest::Url::parse(&format!("https://enumeray.invalid{path}"))
        .expect("encoded resource path");
    url.query_pairs_mut().extend_pairs(query);
    client.get(&format!(
        "{}?{}",
        url.path(),
        url.query().unwrap_or_default()
    ))
}

fn member(
    client: &EnumerayClient,
    kind: &str,
    id: &str,
    user: uuid::Uuid,
    generation: i64,
    add: bool,
) -> Result<Value, Error> {
    if add {
        typed_mutate::<enumeray_contracts::management::MembershipReceipt, _>(
            client,
            Method::POST,
            &[kind, id, "members"],
            &MembershipChange {
                user_id: user,
                expected_generation: generation,
            },
        )
    } else {
        typed_mutate::<enumeray_contracts::management::MembershipReceipt, _>(
            client,
            Method::DELETE,
            &[kind, id, "members", &user.to_string()],
            &GenerationChange {
                expected_generation: generation,
            },
        )
    }
}

fn accept_transfer(
    client: &EnumerayClient,
    kind: &str,
    resource: &str,
    transfer: &str,
) -> Result<Value, Error> {
    typed_mutate::<enumeray_contracts::management::TransferAcceptanceReceipt, _>(
        client,
        Method::POST,
        &["ownership-transfers", transfer, "accept"],
        &serde_json::json!({"resource_kind":kind,"resource_id":resource}),
    )
}

fn stdin_secret() -> Result<String, Error> {
    let mut token = String::new();
    std::io::stdin().take(65537).read_to_string(&mut token)?;
    let token = token.trim();
    if token.is_empty() || token.len() > 65536 || token.chars().any(char::is_whitespace) {
        return Err(Error::Invalid(
            "Expected one nonempty token on stdin.".into(),
        ));
    }
    Ok(token.to_owned())
}

fn require_new_output(path: &Path) -> Result<crate::config::PrivateLock, Error> {
    require_absent_secret_output(path)?;
    let lock = crate::config::lock_private(
        path,
        "Secret output path is already in use; retry after the other command exits.",
    )?;
    require_absent_secret_output(path)?;
    Ok(lock)
}

fn require_absent_secret_output(path: &Path) -> Result<(), Error> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Err(Error::Invalid(
            "Secret output path already exists; preserve it and inspect/use the saved file, or choose a new file for an explicit replacement.".into(),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn save_secret_output(
    path: &Path,
    value: &Value,
    action: &str,
    recovery: &str,
) -> Result<(), Error> {
    let id = value["id"]
        .as_str()
        .filter(|id| uuid::Uuid::parse_str(id).is_ok_and(|id| !id.is_nil()));
    if id.is_none()
        || value["token"].as_str().is_none_or(|token| {
            token.is_empty() || token.len() > 65536 || token.chars().any(char::is_whitespace)
        })
        || value["expires_at"].as_str().is_none_or(str::is_empty)
    {
        return Err(Error::Invalid(format!(
            "{action} returned an invalid secret response; no secret file was saved. The request may have committed. {recovery}"
        )));
    }
    crate::config::write_private(path, value, false).map_err(|_| {
        Error::Invalid(format!("{action} was confirmed (ID {}) but saving was not confirmed. Preserve and inspect the output privately. {recovery}", id.expect("validated above")))
    })
}

fn secret_request_error(error: Error, action: &str, recovery: &str) -> Error {
    match error {
        error @ Error::Api {
            status: 400..=499, ..
        } => error,
        _ => Error::Invalid(format!(
            "{action} outcome is unknown; no secret file was saved. {recovery}"
        )),
    }
}

// Serialize only at the CLI presentation boundary after shared-contract decoding.
fn typed_get<T: serde::de::DeserializeOwned + Serialize>(
    client: &EnumerayClient,
    segments: &[&str],
) -> Result<Value, Error> {
    Ok(serde_json::to_value(
        client.get::<T>(&resource_path(segments)?)?,
    )?)
}
fn typed_mutate<T: serde::de::DeserializeOwned + Serialize, B: Serialize>(
    client: &EnumerayClient,
    method: Method,
    segments: &[&str],
    body: &B,
) -> Result<Value, Error> {
    Ok(serde_json::to_value(client.request::<T, B>(
        method,
        &resource_path(segments)?,
        Some(body),
        None,
    )?)?)
}
fn typed_paged_get<T: serde::de::DeserializeOwned + Serialize>(
    client: &EnumerayClient,
    segments: &[&str],
    page: &crate::args::PageArgs,
) -> Result<Value, Error> {
    typed_query_get::<T>(client, segments, page.query())
}
fn typed_query_get<T: serde::de::DeserializeOwned + Serialize>(
    client: &EnumerayClient,
    segments: &[&str],
    query: Vec<(&str, String)>,
) -> Result<Value, Error> {
    if query.is_empty() {
        return typed_get::<T>(client, segments);
    }
    let path = resource_path(segments)?;
    let mut url = reqwest::Url::parse(&format!("https://enumeray.invalid{path}"))
        .expect("encoded resource path");
    url.query_pairs_mut().extend_pairs(query);
    Ok(serde_json::to_value(client.get::<T>(&format!(
        "{}{}",
        url.path(),
        url.query().map(|q| format!("?{q}")).unwrap_or_default()
    ))?)?)
}
