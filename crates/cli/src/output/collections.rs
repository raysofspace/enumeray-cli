use super::{invocation, present_fields, safe_text, scalar, table};
use crate::args::{
    Access, Analysis, Args, Auth, Command, Connector, ConnectorContext, ConnectorGrant, Model,
    PageArgs, Run, Service,
};
use serde_json::Value;
use std::fmt::Write;

type Columns = &'static [(&'static str, &'static str)];
const MODELS: Columns = &[
    ("MODEL", "/model_id"),
    ("NAME", "/name"),
    ("DESCRIPTION", "/description"),
];
const RUNS: Columns = &[
    ("RUN", "/run_id"),
    ("MODEL", "/model_id"),
    ("PUBLICATION", "/publication_id"),
    ("PURPOSE", "/purpose"),
    ("STATE", "/state"),
    ("CREATED", "/created_at"),
];
const AUDIT: Columns = &[
    ("EVENT", "/id"),
    ("TIME", "/occurred_at"),
    ("ACTOR", "/actor_id"),
    ("ACTION", "/action"),
    ("REQUEST", "/request_id"),
];
const GRANTS: Columns = &[
    ("GRANT", "/id"),
    ("CONTEXT", "/context_id"),
    ("USER", "/user_id"),
    ("GENERATION", "/generation"),
    ("QUEUE LIMIT", "/max_queued_runs"),
    ("ASSIGNMENT LIMIT", "/max_outstanding_assignments"),
    ("REVOKED AT", "/revoked_at"),
];

pub(super) fn render(output: &mut String, args: &Args, value: &Value) -> bool {
    // The command, not a guessed response shape, selects collection rendering.
    let (title, columns, page, mut words): (&str, Columns, &PageArgs, Vec<String>) =
        match &args.command {
            Command::Model(Model::List { page }) => {
                ("Models", MODELS, page, vec!["model".into(), "list".into()])
            }
            Command::Model(Model::Publications { id, page }) => (
                "Publications",
                &[
                    ("PUBLICATION", "/id"),
                    ("STATUS", "/status"),
                    ("CONTEXT", "/context_id"),
                    ("GENERATION", "/generation"),
                    ("PUBLISHED", "/published_at"),
                    ("REVOKED AT", "/revoked_at"),
                    ("INSTALLATION REVOKED AT", "/installation_revoked_at"),
                ],
                page,
                vec!["model".into(), "publications".into(), id.clone()],
            ),
            Command::Model(Model::Runs { id, page }) => (
                "Model run history",
                RUNS,
                page,
                vec!["model".into(), "runs".into(), id.clone()],
            ),
            Command::Model(Model::Audit { id, page }) => (
                "Model audit",
                AUDIT,
                page,
                vec!["model".into(), "audit".into(), id.clone()],
            ),
            Command::Connector(Connector::Grant(ConnectorGrant::Available { page })) => (
                "Available compute grants",
                &[
                    ("GRANT", "/id"),
                    ("CONNECTOR", "/connector_id"),
                    ("CONTEXT", "/context_id"),
                    ("LOCATION", "/context/metadata/location"),
                    ("GENERATION", "/generation"),
                    ("QUEUE LIMIT", "/max_queued_runs"),
                    ("ASSIGNMENT LIMIT", "/max_outstanding_assignments"),
                ],
                page,
                vec!["connector".into(), "grant".into(), "available".into()],
            ),
            Command::Connector(Connector::Context(ConnectorContext::List { id, page })) => (
                "Execution contexts",
                &[
                    ("CONTEXT", "/id"),
                    ("CONNECTOR", "/connector_id"),
                    ("STATE", "/state"),
                    ("DRIVER", "/definition/driver/kind"),
                    ("CONNECTION", "/definition/connection/kind"),
                    ("LOCATION", "/metadata/location"),
                    ("GENERATION", "/generation"),
                ],
                page,
                vec![
                    "connector".into(),
                    "context".into(),
                    "list".into(),
                    id.clone(),
                ],
            ),
            Command::Connector(Connector::Grant(ConnectorGrant::List { id, page })) => (
                "Compute grants",
                GRANTS,
                page,
                vec![
                    "connector".into(),
                    "grant".into(),
                    "list".into(),
                    id.clone(),
                ],
            ),
            Command::Connector(Connector::Runs { id, page }) => (
                "Connector run history",
                &[
                    ("RUN", "/run_id"),
                    ("MODEL", "/model_id"),
                    ("CONTEXT", "/context_id"),
                    ("STATE", "/state"),
                    ("PHASE", "/phase"),
                    ("BLOCKAGE", "/blocked_reason"),
                ],
                page,
                vec!["connector".into(), "runs".into(), id.clone()],
            ),
            Command::Connector(Connector::Audit { id, page }) => (
                "Connector audit",
                AUDIT,
                page,
                vec!["connector".into(), "audit".into(), id.clone()],
            ),
            Command::Connector(Connector::List { page }) => (
                "Connectors",
                &[
                    ("CONNECTOR", "/connector_id"),
                    ("LABEL", "/label"),
                    ("OWNER", "/owner_id"),
                    ("OWNERSHIP GENERATION", "/ownership_generation"),
                    ("GENERATION", "/generation"),
                    ("OBSERVED", "/observed_generation"),
                    ("MAINTENANCE", "/draining"),
                    ("AUTHORITY WITHDRAWN", "/authority_withdrawn"),
                ],
                page,
                vec!["connector".into(), "list".into()],
            ),
            Command::Access(Access::Requests { model, page }) => (
                "Access requests",
                &[
                    ("REQUEST", "/id"),
                    ("USER", "/user_id"),
                    ("STATUS", "/status"),
                    ("MESSAGE", "/message"),
                    ("CREATED", "/created_at"),
                ],
                page,
                vec!["access".into(), "requests".into(), model.clone()],
            ),
            Command::Access(Access::Invitations { model, page }) => (
                "Invitations",
                &[
                    ("INVITATION", "/id"),
                    ("RECIPIENT USER", "/recipient_id"),
                    ("RECIPIENT EMAIL", "/recipient_email"),
                    ("STATUS", "/status"),
                    ("EXPIRES", "/expires_at"),
                ],
                page,
                vec!["access".into(), "invitations".into(), model.clone()],
            ),
            Command::Auth(Auth::TokenList { page }) => (
                "Personal tokens",
                &[
                    ("TOKEN ID", "/id"),
                    ("NAME", "/name"),
                    ("SCOPES", "/scopes"),
                    ("EXPIRES", "/expires_at"),
                    ("REVOKED AT", "/revoked_at"),
                ],
                page,
                vec!["auth".into(), "token-list".into()],
            ),
            Command::Service(Service::Audit { page }) => (
                "Service audit",
                &[
                    ("EVENT", "/id"),
                    ("TIME", "/occurred_at"),
                    ("RESOURCE KIND", "/resource_kind"),
                    ("RESOURCE", "/resource_id"),
                    ("ACTION", "/action"),
                    ("ACTOR", "/actor_id"),
                ],
                page,
                vec!["service".into(), "audit".into()],
            ),
            Command::Run(Run::List { page, filters }) => {
                let mut words = vec!["run".into(), "list".into()];
                for (key, value) in filters.query() {
                    words.extend([
                        format!(
                            "--{}",
                            if key == "model_id" {
                                "model".into()
                            } else {
                                key.replace('_', "-")
                            }
                        ),
                        value,
                    ]);
                }
                ("Your run history", RUNS, page, words)
            }
            Command::Run(Run::Artifacts { id, page }) => (
                "Run artifacts",
                &[
                    ("ARTIFACT", "/artifact_id"),
                    ("NAME", "/logical_name"),
                    ("MEDIA TYPE", "/media_type"),
                    ("AVAILABILITY", "/availability"),
                    ("BYTES", "/size_bytes"),
                ],
                page,
                vec!["run".into(), "artifacts".into(), id.clone()],
            ),
            Command::Run(Run::Logs { id, page, .. }) => (
                "Run logs",
                &[],
                page,
                vec!["run".into(), "logs".into(), id.clone()],
            ),
            Command::Analysis(Analysis::Logs {
                directory, page, ..
            }) => (
                "Analysis logs",
                &[],
                page,
                vec![
                    "analysis".into(),
                    "logs".into(),
                    directory.to_string_lossy().into_owned(),
                ],
            ),
            Command::Run(Run::Events { id, page }) => (
                "Run events",
                &[],
                page,
                vec!["run".into(), "events".into(), id.clone()],
            ),
            _ => return false,
        };
    writeln!(output, "{title}").expect("string write");
    if matches!(args.command, Command::Analysis(Analysis::Logs { .. })) {
        present_fields(
            output,
            value,
            &[
                ("Model", "/model_id"),
                ("Publication", "/publication_id"),
                ("Purpose", "/purpose"),
                ("Captured grant", "/grant_id"),
                ("Named delegation", "/publication_delegation_id"),
                ("Run", "/run_id"),
            ],
        );
    }
    if let Some(items) = value["items"].as_array() {
        match &args.command {
            Command::Run(Run::Logs { .. }) | Command::Analysis(Analysis::Logs { .. }) => {
                logs(output, items)
            }
            Command::Run(Run::Events { .. }) => events(output, items, value),
            _ => table(output, items, columns),
        }
        writeln!(output, "\nReturned {} item(s) on this page.", items.len()).expect("string write");
    } else {
        writeln!(output, "Collection items unavailable; inspect with --json.")
            .expect("string write");
    }
    if let Some(limit) = page.limit {
        words.extend(["--limit".into(), limit.to_string()]);
    }
    if let Some(cursor) = value["next_cursor"].as_str() {
        let mut continuation = words.clone();
        continuation.extend(["--cursor".into(), cursor.to_owned()]);
        writeln!(output, "Next page: {}", invocation(args, &continuation)).expect("string write");
    } else if value.get("next_cursor").is_some_and(Value::is_null) {
        writeln!(output, "End of collection at this observation.").expect("string write");
    }
    if let Some(cursor) = value["resume_cursor"].as_str() {
        words.extend(["--cursor".into(), cursor.to_owned()]);
        writeln!(
            output,
            "Resume later (after this page): {}",
            invocation(args, &words)
        )
        .expect("string write");
        present_fields(
            output,
            value,
            &[
                ("Earliest retained position", "/retention/earliest_position"),
                ("Latest position", "/retention/latest_position"),
                (
                    "Payload maximum age (seconds)",
                    "/retention/payload_max_age_seconds",
                ),
                ("Payload maximum entries", "/retention/payload_max_entries"),
            ],
        );
    }
    match &args.command {
        Command::Model(Model::List { .. }) => output.push_str("Inspect access, availability, contract and disclosure with model get MODEL_ID.\n"),
        Command::Model(Model::Publications { id, .. }) => super::next(output, args, &["model", "publication", id, "PUBLICATION_ID"]),
        Command::Connector(Connector::Grant(ConnectorGrant::List { .. })) => output.push_str("A revoked timestamp marks withdrawn authority; a missing timestamp alone does not establish execution readiness.\n"),
        Command::Run(Run::List { .. }) | Command::Model(Model::Runs { .. }) => output.push_str("Inspect current state and results with run get RUN_ID.\n"),
        Command::Connector(Connector::Runs { .. }) => output.push_str("Connector authority exposes operational metadata; inspect connector diagnostics for the selected context.\n"),
        Command::Run(Run::Artifacts { .. }) => output.push_str("Download by full artifact ID: artifact download ARTIFACT_ID --output NEW_PATH.\n"),
        _ => (),
    }
    true
}

pub(super) fn logs(output: &mut String, items: &[Value]) {
    if items.is_empty() {
        output.push_str("No log entries returned.\n");
    }
    for item in items {
        writeln!(
            output,
            "[{}; position {}; epoch {}; offset {}; {} bytes]",
            scalar(item.get("source")),
            scalar(item.get("position")),
            scalar(item.get("epoch")),
            scalar(item.get("offset")),
            scalar(item.get("byte_length"))
        )
        .expect("string write");
        if item["source_reset"] == true {
            output.push_str("Source reset: earlier text may be missing.\n");
        }
        if let Some(gap) = item.get("gap").filter(|value| !value.is_null()) {
            // Gap counts/reasons are evidence; select only the documented fields.
            output.push_str("Log gap: omitted data.\n");
            present_fields(
                output,
                gap,
                &[
                    ("Reason", "/reason"),
                    ("Omitted bytes", "/bytes"),
                    ("From offset", "/from_offset"),
                    ("To offset", "/to_offset"),
                ],
            );
        }
        if let Some(text) = item["text"].as_str() {
            for line in safe_text(text, true).lines() {
                writeln!(output, "  {line}").expect("string write");
            }
        }
    }
}

fn events(output: &mut String, items: &[Value], value: &Value) {
    if items.is_empty() {
        output.push_str("No execution events returned.\n");
    }
    for event in items {
        present_fields(
            output,
            event,
            &[
                ("Position", "/position"),
                ("Observed", "/request/observed_at"),
                ("Phase", "/request/phase"),
                ("Diagnostic level", "/request/diagnostic/level"),
                ("Diagnostic code", "/request/diagnostic/code"),
                ("Diagnostic", "/request/diagnostic/message"),
                ("Backend namespace", "/request/backend_handle/namespace"),
                ("Backend job", "/request/backend_handle/job_id"),
            ],
        );
        output.push('\n');
    }
    if let Some(events) = value["cancellation_events"]
        .as_array()
        .filter(|events| !events.is_empty())
    {
        output.push_str("Cancellation facts (may repeat independently on each page):\n");
        table(
            output,
            events,
            &[
                ("KIND", "/kind"),
                ("SCOPE", "/scope"),
                ("TIME", "/occurred_at"),
            ],
        );
    }
}
