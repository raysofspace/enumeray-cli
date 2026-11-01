//! Independent durable cancellation intent. Only an explicit command sends a PUT.
use super::{
    accepted_id, get,
    state::{AnalysisState, Directory, invalid},
    summary, validate_run,
};
use crate::config;
use enumeray_client::{EnumerayClient, Error, resource_path};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

const PATH: &str = "state/cancellation.json";

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Attempt {
    Pending,
    Acknowledged,
    Uncertain,
    Rejected,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema_version: u32,
    api_url: String,
    user_id: Uuid,
    run_id: String,
    attempt: Attempt,
    receipt: Option<Value>,
    error_code: Option<String>,
}

fn validate_receipt(run_id: &str, receipt: &Value) -> Result<(), Error> {
    let state = receipt["state"].as_str();
    let status = receipt["status"].as_str();
    if receipt["run_id"] != run_id
        || receipt["requested_by_user_id"]
            .as_str()
            .and_then(|s| s.parse::<Uuid>().ok())
            .is_none_or(|id| id.is_nil())
        || receipt["requested_at"].as_str().is_none_or(str::is_empty)
        || !matches!(
            receipt["scope"].as_str(),
            Some("pending_and_running" | "queued_only")
        )
        || !matches!(
            (state, status),
            (Some("queued" | "running"), Some("requested"))
                | (Some("cancelled"), Some("confirmed"))
                | (Some("succeeded" | "failed"), Some("completed"))
        )
        || (status == Some("confirmed")
            && receipt["cancelled_at"].as_str().is_none_or(str::is_empty))
    {
        return Err(invalid(
            "Cancellation receipt conflicts with this analysis or has an invalid state; retain the directory and repeat analysis cancel DIR to reconcile.",
        ));
    }
    Ok(())
}

fn read(directory: &Directory, saved: &AnalysisState) -> Result<Option<Journal>, Error> {
    if !directory.exists(PATH)? {
        return Ok(None);
    }
    let journal: Journal = directory.read(PATH)?;
    if journal.schema_version != 1
        || journal.api_url != saved.api_url()
        || journal.user_id != saved.user_id()
        || journal.run_id != accepted_id(saved)?
        || (matches!(journal.attempt, Attempt::Acknowledged) && journal.receipt.is_none())
        || (matches!(journal.attempt, Attempt::Uncertain | Attempt::Rejected)
            != journal.error_code.is_some())
        || journal.error_code.as_deref().is_some_and(|code| {
            code.is_empty() || code.len() > 128 || code.chars().any(char::is_control)
        })
    {
        return Err(invalid(
            "Cancellation journal belongs to another API, account or run, or is inconsistent; preserve it before retrying.",
        ));
    }
    if let Some(receipt) = &journal.receipt {
        validate_receipt(&journal.run_id, receipt)?;
    }
    Ok(Some(journal))
}

/// Called within the directory snapshot, without acquiring cancellation ownership.
pub(super) fn status(directory: &Directory, saved: &AnalysisState) -> Result<Value, Error> {
    Ok(serde_json::to_value(read(directory, saved)?)?)
}

pub(super) fn cancel(
    client: &EnumerayClient,
    directory: &Directory,
    saved: &AnalysisState,
) -> Result<Value, Error> {
    let run_id = accepted_id(saved)?;
    // Serialize only cancellation; status, following and transfer writers remain independent.
    let _control = config::lock_private(
        &directory.root.join(PATH),
        "Analysis cancellation is already in progress; inspect analysis status DIR or retry cancellation after it exits.",
    )?;
    let mut journal = {
        let _snapshot = directory.state_lock()?;
        read(directory, saved)?
    }
    .unwrap_or_else(|| Journal {
        schema_version: 1,
        api_url: saved.api_url().to_owned(),
        user_id: saved.user_id(),
        run_id: run_id.clone(),
        attempt: Attempt::Pending,
        receipt: None,
        error_code: None,
    });
    journal.attempt = Attempt::Pending;
    journal.error_code = None;
    directory.write(PATH, &journal)?;
    let result = client
        .request::<Value, Value>(
            Method::PUT,
            &resource_path(&["runs", &run_id, "cancellation"])?,
            None,
            None,
        )
        .and_then(|receipt| {
            validate_receipt(&run_id, &receipt)?;
            Ok(receipt)
        });
    match result {
        Ok(receipt) => {
            journal.attempt = Attempt::Acknowledged;
            journal.receipt = Some(receipt);
            directory.write(PATH, &journal)?;
            // Cancellation authority is independent of read scope: a successful PUT stays
            // successful even when inspection is unavailable. Never infer termination locally.
            let run = get(client, &["runs", &run_id]).ok();
            if let Some(run) = &run {
                validate_run(saved, run)?;
            }
            let mut value = summary(directory, saved, run)?;
            value["run_id"] = json!(run_id);
            value["cancellation"] = serde_json::to_value(journal)?;
            value["next_action"] = json!(
                "Observe authoritative progress with analysis status DIR or analysis wait DIR; collect terminal evidence with analysis collect DIR. A request does not establish termination."
            );
            super::validated_summary(value)
        }
        Err(error) => {
            let rejected = matches!(
                &error,
                Error::Api {
                    status: 400 | 401 | 403 | 404 | 409 | 422,
                    ..
                }
            );
            journal.attempt = if rejected {
                Attempt::Rejected
            } else {
                Attempt::Uncertain
            };
            let code: String = match &error {
                Error::Api { failure, .. } => failure
                    .code
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
                    .take(128)
                    .collect(),
                _ => "unconfirmed_reply".into(),
            };
            journal.error_code = Some(if code.is_empty() {
                "unconfirmed_reply".into()
            } else {
                code
            });
            directory.write(PATH, &journal)?;
            if matches!(
                &error,
                Error::Api {
                    status: 401 | 403,
                    ..
                }
            ) {
                eprintln!(
                    "Cancellation authority is unavailable. Renew the same account with auth refresh/login and check runs:cancel scope and submitter authority, then repeat analysis cancel DIR."
                );
            } else {
                eprintln!(
                    "Inspect analysis status DIR. If cancellation is still intended, repeat analysis cancel DIR for the same run; a missing reply does not establish termination."
                );
            }
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_receipts_require_consistent_authoritative_states() {
        let run = Uuid::new_v4().to_string();
        let base = json!({"run_id":run,"requested_by_user_id":Uuid::new_v4(),
            "requested_at":"2026-10-19T12:00:00Z","scope":"pending_and_running",
            "state":"running","status":"requested","cancelled_at":null});
        for (state, status, cancelled_at) in [
            ("running", "requested", Value::Null),
            ("cancelled", "confirmed", json!("2026-10-19T12:00:01Z")),
            ("succeeded", "completed", Value::Null),
            ("failed", "completed", Value::Null),
        ] {
            let mut receipt = base.clone();
            receipt["state"] = json!(state);
            receipt["status"] = json!(status);
            receipt["cancelled_at"] = cancelled_at;
            validate_receipt(&run, &receipt).unwrap();
        }
        for (field, value) in [
            ("run_id", json!(Uuid::new_v4())),
            ("requested_by_user_id", json!(Uuid::nil())),
            ("state", json!("succeeded")),
            ("status", json!("confirmed")),
            ("scope", json!("unknown")),
        ] {
            let mut receipt = base.clone();
            receipt[field] = value;
            assert!(validate_receipt(&run, &receipt).is_err());
        }
    }
}
