use super::{comparison, full_hash, unavailable};
use crate::{
    args::{Args, CompatibilityCheck as Options},
    config,
};
use enumeray_client::{EnumerayClient, Error};
use enumeray_contracts::{
    compatibility::{API, CONNECTOR_PROTOCOL, RUNNER_PROTOCOL, WRAPPER_PROTOCOLS},
    installation::CompatibilityCheck,
};
use serde_json::{Value, json};
use std::{process::Command, time::Duration};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

/// An explicitly selected content identity constrains clean and dirty observations.
/// Dirty observations additionally require such a selection; the clean flag cannot
/// substitute for the bytes selected by the caller.
pub(super) fn content(
    checks: &mut Vec<CompatibilityCheck>,
    component: &str,
    field: &str,
    observed: &Value,
    selected: &Value,
) {
    if !selected.is_null() || observed["clean"] == false {
        comparison(
            checks,
            component,
            field,
            selected.clone(),
            observed["content_sha256"].clone(),
        );
    }
}

pub(super) fn source(
    checks: &mut Vec<CompatibilityCheck>,
    component: &str,
    observed: &Value,
    selected: &Value,
) {
    comparison(
        checks,
        component,
        "public_repository",
        json!("https://github.com/raysofspace/enumeray-cli"),
        observed["repository"].clone(),
    );
    comparison(
        checks,
        component,
        "source_revision",
        selected["public_revision"].clone(),
        observed["revision"].clone(),
    );
    for (field, length) in [("tree", 40), ("lockfile_sha256", 64)] {
        if !full_hash(&observed[field], length) {
            unavailable(
                checks,
                component,
                field,
                "Required source tree or lockfile identity is unavailable or malformed.",
            );
        }
    }
    for field in ["tree", "lockfile_sha256"] {
        if !selected["source"][field].is_null() {
            comparison(
                checks,
                component,
                field,
                selected["source"][field].clone(),
                observed[field].clone(),
            );
        }
    }
    if !observed["clean"].is_boolean() {
        unavailable(
            checks,
            component,
            "source_clean",
            "Build cleanliness is unavailable.",
        );
    } else if selected.get("artifacts").is_some() {
        if ["revision", "tree", "lockfile_sha256"]
            .iter()
            .any(|field| observed[field].is_null())
        {
            unavailable(
                checks,
                component,
                "source_clean",
                "Unknown source identity cannot establish release-source cleanliness.",
            );
        } else {
            comparison(
                checks,
                component,
                "source_clean",
                json!(true),
                observed["clean"].clone(),
            );
        }
    }
    content(
        checks,
        component,
        "development_content_sha256",
        observed,
        &selected["public_content_sha256"],
    );
}

pub(super) fn build(
    checks: &mut Vec<CompatibilityCheck>,
    component: &str,
    observed: &Value,
    selected: &Value,
    local: bool,
) {
    let mut identity = observed.clone();
    if let Some(object) = identity.as_object_mut() {
        object.remove("observed_at");
        object.remove("executable_sha256");
    }
    let parsed =
        serde_json::from_value::<enumeray_contracts::compatibility::BuildIdentity>(identity);
    match &parsed {
        Ok(identity)
            if identity.source.revision.is_none()
                || identity.source.tree.is_none()
                || identity.source.lockfile_sha256.is_none() =>
        {
            unavailable(
                checks,
                component,
                "complete_source_identity",
                "Source revision, tree and lockfile facts are required; absence does not establish a mismatch.",
            );
        }
        Ok(identity) => comparison(
            checks,
            component,
            "valid_build_identity",
            json!(true),
            json!(identity.validate()),
        ),
        Err(_) => unavailable(
            checks,
            component,
            "complete_build_identity",
            "Required typed build identity fields are missing or malformed; no defaults establish a match.",
        ),
    }
    let target = observed["target"].as_str();
    comparison(
        checks,
        component,
        "supported_target",
        json!(true),
        target
            .map(|t| {
                json!(matches!(
                    t,
                    "aarch64-apple-darwin"
                        | "x86_64-apple-darwin"
                        | "aarch64-unknown-linux-gnu"
                        | "x86_64-unknown-linux-gnu"
                ))
            })
            .unwrap_or(Value::Null),
    );
    if let Some(target) = target {
        comparison(
            checks,
            component,
            "selected_target",
            json!(target),
            json!(target),
        );
    }
    comparison(
        checks,
        component,
        "schema_version",
        json!(1),
        observed["schema_version"].clone(),
    );
    comparison(
        checks,
        component,
        "package",
        json!("enumeray-cli"),
        observed["package"].clone(),
    );
    comparison(
        checks,
        component,
        "version",
        selected["version"].clone(),
        observed["version"].clone(),
    );
    comparison(
        checks,
        component,
        "api",
        json!(API),
        observed["api"].clone(),
    );
    comparison(
        checks,
        component,
        "connector_protocol",
        json!(CONNECTOR_PROTOCOL),
        observed["connector_protocol"].clone(),
    );
    comparison(
        checks,
        component,
        "runner_protocol",
        json!(RUNNER_PROTOCOL),
        observed["runner_protocol"].clone(),
    );
    comparison(
        checks,
        component,
        "wrapper_protocols",
        json!(WRAPPER_PROTOCOLS),
        observed["wrapper_protocols"].clone(),
    );
    source(checks, component, &observed["source"], selected);

    if let Some(artifacts) = selected.get("artifacts") {
        let target = observed["target"].as_str().unwrap_or("");
        comparison(
            checks,
            component,
            "qualified_target",
            json!(true),
            json!(artifacts.get(target).is_some()),
        );
        let expected = &artifacts[target]["build_identity"];
        let mut identity = observed.clone();
        if let Some(object) = identity.as_object_mut() {
            object.remove("observed_at");
            object.remove("executable_sha256");
        }
        let complete = parsed.as_ref().is_ok_and(|value| {
            value.source.revision.is_some()
                && value.source.tree.is_some()
                && value.source.lockfile_sha256.is_some()
        });
        if complete {
            comparison(
                checks,
                component,
                "qualified_build_identity",
                expected.clone(),
                identity,
            );
        } else {
            unavailable(
                checks,
                component,
                "qualified_build_identity",
                "Required compiled source facts are unavailable; equality with a release build was not established.",
            );
        }
        if !local {
            comparison(
                checks,
                component,
                "archive_binary_sha256",
                artifacts[target]["binary_sha256"].clone(),
                observed["executable_sha256"].clone(),
            );
        }
    }
}

pub(super) fn freshness(
    checks: &mut Vec<CompatibilityCheck>,
    component: &str,
    timestamp: &Value,
    snapshot_only: bool,
) {
    let parsed = timestamp
        .as_str()
        .and_then(|s| OffsetDateTime::parse(s, &Rfc3339).ok());
    let Some(parsed) = parsed else {
        unavailable(
            checks,
            component,
            "observation_time",
            "A valid observation timestamp is required.",
        );
        return;
    };
    let age = OffsetDateTime::now_utc() - parsed;
    if snapshot_only {
        comparison(
            checks,
            component,
            "historical_snapshot_time_valid",
            json!(true),
            json!(age.whole_seconds() >= -30),
        );
    } else {
        comparison(
            checks,
            component,
            "observation_recent",
            json!(true),
            json!((-30..=60).contains(&age.whole_seconds())),
        );
        if !(-30..=60).contains(&age.whole_seconds())
            && let Some(last) = checks.last_mut()
        {
            last.status = enumeray_contracts::installation::CompatibilityStatus::MissingEvidence;
            last.reason="A saved observation is stale or future-dated; obtain a fresh observation without launching scientific work.".into();
        }
    }
}

pub(super) fn platform(checks: &mut Vec<CompatibilityCheck>, local: &Value) {
    let target = local["target"].as_str().unwrap_or("");
    let supported = matches!(
        target,
        "aarch64-apple-darwin"
            | "x86_64-apple-darwin"
            | "aarch64-unknown-linux-gnu"
            | "x86_64-unknown-linux-gnu"
    );
    comparison(
        checks,
        "local_host",
        "supported_target",
        json!(true),
        json!(supported),
    );
    if !supported {
        return;
    }
    let macos = target.ends_with("apple-darwin");
    let result = if macos {
        Command::new("sw_vers").arg("-productVersion").output()
    } else {
        Command::new("getconf").arg("GNU_LIBC_VERSION").output()
    };
    let value = result
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok());
    let version = value.as_deref().map(str::trim).and_then(|s| {
        if macos {
            Some(s)
        } else {
            s.strip_prefix("glibc ")
        }
    });
    let version = version.and_then(|v| {
        v.split('.')
            .take(2)
            .map(str::parse::<u32>)
            .collect::<Result<Vec<_>, _>>()
            .ok()
    });
    if let Some(version) = version.filter(|v| v.len() >= 2) {
        comparison(
            checks,
            "local_host",
            if macos {
                "macos_at_least_15"
            } else {
                "glibc_at_least_2_28"
            },
            json!(true),
            json!((version[0], version[1]) >= if macos { (15, 0) } else { (2, 28) }),
        );
    } else {
        unavailable(
            checks,
            "local_host",
            "platform_minimum",
            "Cannot establish the installed host's supported OS/libc baseline.",
        );
    }
}

fn read_profile(args: &Args) -> Result<Option<config::Profile>, Error> {
    let path = args
        .config
        .clone()
        .map(Ok)
        .unwrap_or_else(config::default_path)?;
    Ok(config::load(&path)?.profiles.remove(&args.profile))
}

pub(super) fn service(
    args: &Args,
    options: &Options,
    selected: &Value,
    checks: &mut Vec<CompatibilityCheck>,
) -> Result<(), Error> {
    let profile = if args.api.is_none() || options.connector.is_some() {
        match read_profile(args) {
            Ok(profile) => profile,
            Err(_) if args.api.is_none() => {
                unavailable(
                    checks,
                    "service",
                    "observation",
                    "The selected profile file is unreadable or invalid; no default service was contacted.",
                );
                if options.connector.is_some() {
                    unavailable(
                        checks,
                        "connector",
                        "observation",
                        "The selected service profile is unavailable for authorized connector diagnostics.",
                    );
                }
                return Ok(());
            }
            Err(_) => None,
        }
    } else {
        None
    };
    let origin = config::api_origin(args, profile.as_ref());
    let client = match EnumerayClient::new(origin, None, Duration::from_secs(args.timeout)) {
        Ok(client) => client,
        Err(_) => {
            unavailable(
                checks,
                "service",
                "observation",
                "API origin is invalid or unsupported; no request or credential renewal was performed.",
            );
            return Ok(());
        }
    };
    comparison(
        checks,
        "service",
        "selected_origin",
        json!(client.base_url()),
        json!(client.base_url()),
    );
    match client.get::<Value>("/api/v1/openapi.json") {
        Ok(openapi) => {
            let identity = &openapi["x-enumeray-build"];
            comparison(
                checks,
                "service",
                "schema_version",
                json!(1),
                identity["schema_version"].clone(),
            );
            comparison(
                checks,
                "service",
                "version",
                selected["version"].clone(),
                identity["version"].clone(),
            );
            comparison(
                checks,
                "service",
                "repository",
                json!("https://github.com/raysofspace/enumeray"),
                identity["service_source"]["repository"].clone(),
            );
            comparison(
                checks,
                "service",
                "service_revision",
                selected["service_revision"].clone(),
                identity["service_source"]["revision"].clone(),
            );
            comparison(
                checks,
                "service",
                "public_gitlink",
                selected["public_revision"].clone(),
                identity["public_revision"].clone(),
            );
            comparison(
                checks,
                "service",
                "baseline",
                selected["baseline"].clone(),
                identity["baseline"].clone(),
            );
            source(
                checks,
                "service_public_source",
                &identity["public_source"],
                selected,
            );
            if identity["service_source"]["clean"] != false {
                comparison(
                    checks,
                    "service",
                    "source_clean",
                    json!(true),
                    identity["service_source"]["clean"].clone(),
                );
            }
            content(
                checks,
                "service",
                "development_content_sha256",
                &identity["service_source"],
                &selected["service_content_sha256"],
            );
        }
        Err(_) => unavailable(
            checks,
            "service",
            "observation",
            "API identity is unavailable; no credentials were sent to the public identity response.",
        ),
    }
    if let Some(connector) = &options.connector {
        let profile_matches = profile.as_ref().is_some_and(|p| {
            EnumerayClient::new(&p.api_url, None, Duration::from_secs(args.timeout))
                .is_ok_and(|p| p.base_url() == client.base_url())
        });
        let token = std::env::var("ENUMERAY_TOKEN")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                profile
                    .as_ref()
                    .filter(|_| profile_matches)
                    .and_then(|p| p.access_token.clone())
            });
        let authorized = EnumerayClient::new(origin, token, Duration::from_secs(args.timeout))?;
        let path = enumeray_client::resource_path(&["connectors", connector, "diagnostics"])?;
        match authorized.get::<Value>(&path) {
            Ok(diagnostics) => {
                comparison(
                    checks,
                    "connector",
                    "connector_id",
                    json!(connector),
                    diagnostics["connector_id"].clone(),
                );
                let report = &diagnostics["build_observation"];
                if report.is_null() {
                    unavailable(
                        checks,
                        "connector",
                        "reported_build",
                        "Connector has not reported its executing build identity.",
                    );
                } else {
                    for field in ["recent", "credential_current"] {
                        if report[field] == false {
                            unavailable(
                                checks,
                                "connector",
                                field,
                                "Connector build report is stale or uses a revoked credential.",
                            );
                        } else {
                            comparison(
                                checks,
                                "connector",
                                field,
                                json!(true),
                                report[field].clone(),
                            );
                        }
                    }
                    freshness(checks, "connector", &report["received_at"], false);
                    // The authenticated heartbeat identifies the reporting process,
                    // not the file currently at its old executable pathname.
                    build(
                        checks,
                        "connector_reported_build",
                        &report["identity"],
                        selected,
                        true,
                    );
                }
            }
            Err(_) => unavailable(
                checks,
                "connector",
                "observation",
                "Authorized connector observation is unavailable; static credentials were not renewed.",
            ),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signed_expected_build_keeps_unknown_source_facts_unavailable() {
        let original =
            serde_json::to_value(enumeray_contracts::compatibility::build_identity()).unwrap();
        let target = original["target"].as_str().unwrap();
        let selected = json!({"version":original["version"],"public_revision":original["source"]["revision"],"source":original["source"],"artifacts":{target:{"build_identity":original}}});
        let mut unknown = original.clone();
        unknown["source"]["revision"] = Value::Null;
        unknown["source"]["tree"] = Value::Null;
        unknown["source"]["clean"] = json!(false);
        let mut checks = Vec::new();
        build(&mut checks, "runner", &unknown, &selected, true);
        assert!(
            checks.iter().any(|c| c.status
                == enumeray_contracts::installation::CompatibilityStatus::MissingEvidence)
        );
        assert!(
            checks.iter().all(
                |c| c.status != enumeray_contracts::installation::CompatibilityStatus::Mismatch
            )
        );
    }
}
