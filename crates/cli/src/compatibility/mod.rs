//! Read-only identity comparison. No profiles are renewed and no execution is created.
mod observations;
mod selection;

use crate::{
    app::Completion,
    args::{Args, Compatibility},
    config,
};
use enumeray_client::Error;
use enumeray_contracts::{
    automation::SchemaVersion,
    compatibility::build_identity,
    installation::{CompatibilityCheck, CompatibilityReceipt, CompatibilityStatus as Status},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

pub(super) fn now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .expect("UTC timestamp")
}

pub(super) fn digest(path: &Path) -> Result<String, Error> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut block = [0u8; 65536];
    loop {
        let length = file.read(&mut block)?;
        if length == 0 {
            break;
        }
        hash.update(&block[..length]);
    }
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

pub(super) fn full_hash(value: &Value, length: usize) -> bool {
    value.as_str().is_some_and(|v| {
        v.len() == length
            && v.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

pub(super) fn comparison(
    checks: &mut Vec<CompatibilityCheck>,
    component: &str,
    field: &str,
    expected: Value,
    observed: Value,
) {
    let status = if expected.is_null() || observed.is_null() {
        Status::MissingEvidence
    } else if expected == observed {
        Status::Match
    } else {
        Status::Mismatch
    };
    checks.push(CompatibilityCheck {
        component: component.into(),
        field: field.into(),
        status,
        expected,
        observed,
        reason: match status {
            Status::Match => "selected identity agrees",
            Status::Mismatch => "selected and observed identities differ",
            Status::MissingEvidence => "required identity evidence is unavailable",
        }
        .into(),
    });
}

pub(super) fn unavailable(
    checks: &mut Vec<CompatibilityCheck>,
    component: &str,
    field: &str,
    reason: &str,
) {
    checks.push(CompatibilityCheck {
        component: component.into(),
        field: field.into(),
        status: Status::MissingEvidence,
        expected: Value::Null,
        observed: Value::Null,
        reason: reason.into(),
    });
}

pub(super) fn inspect() -> Result<Value, Error> {
    let mut value = serde_json::to_value(build_identity())?;
    value["observed_at"] = json!(now());
    // This observes the installed file, separately from embedded provenance. It
    // must never be used to identify an already-running connector after replacement.
    value["executable_sha256"] = json!(digest(&std::env::current_exe()?)?);
    Ok(value)
}

pub(crate) fn execute(args: &Args, command: &Compatibility) -> Result<Completion, Error> {
    let Compatibility::Check(options) = command else {
        return Ok(Completion {
            value: Some(inspect()?),
            exit: 0,
        });
    };
    if options.receipt.try_exists()? {
        return Err(Error::Invalid(
            "Compatibility receipt already exists; choose a new output file.".into(),
        ));
    }
    let (selected, trust, mut checks) = selection::load(options)?;
    let local = inspect()?;
    observations::build(&mut checks, "local_executable", &local, &selected, true);
    observations::platform(&mut checks, &local);
    if let Some(artifacts) = selected.get("artifacts") {
        let artifact = &artifacts[local["target"].as_str().unwrap_or("")];
        comparison(
            &mut checks,
            "local_executable",
            "archive_binary_sha256",
            artifact["binary_sha256"].clone(),
            local["executable_sha256"].clone(),
        );
    }
    let mut limits = vec![
        "This compares only the explicitly selected software and evidence; it does not qualify scientific runtimes, model results or sites.".into(),
        "Connector contact reports do not establish journal ownership, predecessor shutdown or backend execution state.".into(),
        "Matching development identities and protocol version 1 do not establish release qualification or alpha upgrade support.".into(),
        "Connector and runner reports identify embedded source/builds, not executable-byte attestation or journal ownership. Docs/examples inputs compare identity metadata, not all bundle bytes.".into(),
    ];
    if options.connector.is_none() {
        limits.push("No live connector was selected for comparison.".into());
    }
    if options.context.is_empty() {
        limits.push("No runner/context observation was selected for comparison.".into());
    }
    if options.peer.is_empty() {
        limits.push("No saved peer executable observation was selected for comparison.".into());
    }
    if options.local_only {
        limits.push("Local/saved-snapshot scope: live API, connector ownership and remote runtime availability were not checked.".into());
    } else {
        observations::service(args, options, &selected, &mut checks)?;
    }
    for (index, path) in options.peer.iter().enumerate() {
        let component = format!("peer_{index}");
        match config::read_json::<Value>(path) {
            Ok(observed) => {
                observations::freshness(
                    &mut checks,
                    &component,
                    &observed["observed_at"],
                    options.local_only,
                );
                observations::build(&mut checks, &component, &observed, &selected, false);
            }
            Err(_) => unavailable(
                &mut checks,
                &component,
                "observation",
                "Saved peer observation is missing or invalid; retain the original evidence.",
            ),
        }
    }
    for (index, path) in options.context.iter().enumerate() {
        let component = format!("context_{index}");
        match config::read_json::<Value>(path) {
            Ok(observed) => {
                comparison(
                    &mut checks,
                    &component,
                    "schema_version",
                    json!(1),
                    observed["schema_version"].clone(),
                );
                comparison(
                    &mut checks,
                    &component,
                    "context_identified",
                    json!(true),
                    observed["context_id"]
                        .as_str()
                        .map(|id| json!(!id.is_empty()))
                        .unwrap_or(Value::Null),
                );
                if let Some(id) = observed["context_id"].as_str().filter(|id| !id.is_empty()) {
                    comparison(
                        &mut checks,
                        &component,
                        "selected_context_id",
                        json!(id),
                        json!(id),
                    );
                }
                comparison(
                    &mut checks,
                    &component,
                    "observation_time",
                    observed["observed_at"].clone(),
                    observed["observed_at"].clone(),
                );
                observations::freshness(
                    &mut checks,
                    &component,
                    &observed["observed_at"],
                    options.local_only,
                );
                observations::build(
                    &mut checks,
                    &component,
                    &observed["build_identity"],
                    &selected,
                    true,
                );
            }
            Err(_) => unavailable(
                &mut checks,
                &component,
                "observation",
                "Runner/context observation is unavailable; no probe or job was launched.",
            ),
        }
    }
    if let Some(path) = &options.docs {
        match config::read_json::<Value>(path) {
            Ok(docs) => {
                comparison(
                    &mut checks,
                    "docs",
                    "schema_version",
                    json!(1),
                    docs["schema_version"].clone(),
                );
                comparison(
                    &mut checks,
                    "docs",
                    "service_revision",
                    selected["service_revision"].clone(),
                    docs["root"]["revision"].clone(),
                );
                comparison(
                    &mut checks,
                    "docs",
                    "service_repository",
                    json!("https://github.com/raysofspace/enumeray"),
                    docs["root"]["repository"].clone(),
                );
                if docs["root"]["clean"] != false {
                    comparison(
                        &mut checks,
                        "docs",
                        "root_clean",
                        json!(true),
                        docs["root"]["clean"].clone(),
                    );
                }
                observations::content(
                    &mut checks,
                    "docs",
                    "service_content_sha256",
                    &docs["root"],
                    &selected["service_content_sha256"],
                );
                observations::source(&mut checks, "docs", &docs["public"], &selected);
                if !docs["examples"]["source"].is_null() {
                    observations::source(
                        &mut checks,
                        "docs_examples",
                        &docs["examples"]["source"],
                        &selected,
                    );
                }
            }
            Err(_) => unavailable(
                &mut checks,
                "docs",
                "build_identity",
                "Selected documentation build identity is unavailable.",
            ),
        }
    } else {
        limits.push("No documentation build was selected for comparison.".into());
    }
    if let Some(path) = &options.examples {
        match config::read_json::<Value>(path) {
            Ok(examples) => {
                observations::source(&mut checks, "examples", &examples["source"], &selected)
            }
            Err(_) => unavailable(
                &mut checks,
                "examples",
                "source",
                "Selected examples manifest is unavailable.",
            ),
        }
    } else {
        limits.push("No example bundle was selected for comparison.".into());
    }
    let status = if checks.iter().any(|c| c.status == Status::Mismatch) {
        Status::Mismatch
    } else if checks.iter().any(|c| c.status == Status::MissingEvidence) {
        Status::MissingEvidence
    } else {
        Status::Match
    };
    let receipt = CompatibilityReceipt {
        schema_version: SchemaVersion,
        status,
        trust,
        observed_at: now(),
        selected,
        checks,
        limits,
    };
    config::write_private(&options.receipt, &receipt, false)?;
    Ok(Completion {
        value: Some(serde_json::to_value(receipt)?),
        exit: u8::from(status != Status::Match),
    })
}
