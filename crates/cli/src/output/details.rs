use super::{field, fields, next, present_fields, scientific, table};
use crate::args::{
    Access, Args, Artifact, Auth, Command, Connector, ConnectorContext, ConnectorMember, Model,
    Run, Service,
};
use serde_json::Value;

pub(super) fn render(output: &mut String, args: &Args, value: &Value) {
    match &args.command {
        Command::Model(Model::Release(_)) => {
            output.push_str("Release\n");
            fields(
                output,
                value,
                &[
                    ("Directory", "/directory"),
                    ("Operation", "/operation_id"),
                    ("Model", "/model_id"),
                    ("Candidate", "/publication_id"),
                    ("Model generation", "/model_generation"),
                    ("Delegation", "/id"),
                    ("Recipient", "/user_id"),
                    ("Grant holder", "/created_by"),
                    ("Captured grant", "/grant_id"),
                    ("Revoked", "/revoked_at"),
                    ("Revocation reason", "/revocation_reason"),
                    ("Dry run", "/dry_run"),
                    ("Stage", "/stage"),
                    ("Validation status", "/candidate_status"),
                    ("Validation message", "/validation_message"),
                    ("Blockers", "/blockers"),
                    ("Current publication", "/current_publication_id"),
                    ("Reviewed current", "/expected_current_publication_id"),
                    ("Guard changed", "/guard_changed"),
                    ("Candidate is current", "/candidate_is_current"),
                    ("Registration uncertain", "/registration_uncertain"),
                    ("Activation uncertain", "/activation_uncertain"),
                    ("Execution location", "/execution_location"),
                    ("Data-access disclosure", "/disclosure"),
                ],
            );
            scientific(
                output,
                "Current and proposed publication",
                value.get("comparison"),
            );
            scientific(output, "Release authority", value.get("release_authority"));
            scientific(output, "Handoff request", value.get("request"));
            scientific(
                output,
                "Candidate trial availability",
                value.get("trial_availability"),
            );
            scientific(
                output,
                "Candidate trial evidence",
                value.get("trial_evidence"),
            );
            if !value["trial_init_command"].is_null() {
                field(
                    output,
                    "Start candidate trial",
                    value.get("trial_init_command"),
                );
                output.push_str("Replace INPUTS.json with your scientific input JSON and NEW_TRIAL_DIR with a new private directory; alternatively use --example NAME. Review the analysis installation and disclosure before explicitly submitting. Release status/review includes its trial evidence automatically.\n");
            }
            if let Some(source) = value.get("rollback_source").filter(|v| !v.is_null()) {
                scientific(
                    output,
                    "Rollback source availability",
                    source.get("rollback_availability"),
                );
                output.push_str("Rollback creates a fresh candidate with current authorization and installation validation. Retired configuration is not proof that release files still exist. Permanently revoked installations cannot be reused.\n");
            }
            output.push_str("Installation readiness, trial execution and scientific validation are separate. Trial success is advisory.\n");
            fields(
                output,
                value,
                &[
                    ("Next action", "/next_action"),
                    ("Next command", "/next_command"),
                ],
            );
        }
        Command::Analysis(crate::args::Analysis::Wait { .. }) => {
            output.push_str(&super::observation(value));
            scientific(
                output,
                "Scientific outputs",
                value.pointer("/result/outputs"),
            );
            candidate_analysis_guidance(output, args, value);
        }
        Command::Analysis(_) => {
            output.push_str("Analysis\n");
            present_fields(
                output,
                value,
                &[
                    ("Directory", "/directory"),
                    ("API", "/api_url"),
                    ("Directory account", "/user_id"),
                    ("Original submitter", "/created_by_user_id"),
                    ("Model", "/model_id"),
                    ("Publication", "/publication_id"),
                    ("Purpose", "/purpose"),
                    ("Captured grant", "/grant_id"),
                    ("Named delegation", "/publication_delegation_id"),
                    ("Context", "/context_id"),
                    (
                        "Installation readiness at initialization",
                        "/installation_status",
                    ),
                    ("Stage", "/stage"),
                    ("Recovery origin", "/origin"),
                    ("Execution location", "/execution_location"),
                    ("Disclosure", "/disclosure"),
                    ("Run", "/receipt/run_id"),
                    ("Run", "/run_id"),
                    ("Bundle", "/bundle"),
                    ("Collection complete", "/complete"),
                    ("Scientific state", "/scientific_state"),
                ],
            );
            if value["input_files"]
                .as_array()
                .is_some_and(|files| !files.is_empty())
            {
                scientific(output, "Local input bindings", value.get("input_files"));
            }
            if value["receipt"].is_object() {
                output.push_str("Original acceptance receipt\n");
                present_fields(
                    output,
                    &value["receipt"],
                    &[
                        ("Accepted state (original receipt)", "/state"),
                        ("Accepted at", "/created_at"),
                    ],
                );
                output.push_str("This receipt does not establish current execution state. Inspect this directory with analysis status.\n");
            }
            if value["run"].is_object() {
                output.push_str(&super::observation(&value["run"]));
            }
            if value["cancellation"].is_object() {
                output.push_str("Local cancellation intent\n");
                present_fields(
                    output,
                    &value["cancellation"],
                    &[
                        ("Attempt", "/attempt"),
                        ("Last request error", "/error_code"),
                    ],
                );
                if value["cancellation"]["receipt"].is_object() {
                    output.push_str("Saved API cancellation acknowledgement\n");
                    present_fields(
                        output,
                        &value["cancellation"]["receipt"],
                        &[
                            ("Cancellation status", "/status"),
                            ("Run state at acknowledgement", "/state"),
                            ("Cancellation requested by", "/requested_by_user_id"),
                            ("Cancellation reason", "/reason"),
                            ("Cancellation scope", "/scope"),
                            ("Cancellation requested", "/requested_at"),
                            ("Cancellation confirmed", "/cancelled_at"),
                        ],
                    );
                } else {
                    output.push_str("No saved API acknowledgement; local intent does not establish remote cancellation.\n");
                }
                output.push_str("Current run inspection is authoritative for cancellation confirmation and the final outcome.\n");
            }
            if value["outputs"].is_object() {
                scientific(output, "Declared file outputs", value.get("outputs"));
            }
            scientific(output, "Collection", value.get("collection"));
            scientific(output, "Retained evidence", value.get("evidence"));
            if value["origin"].is_string() {
                scientific(
                    output,
                    "Recorded installation identity",
                    value.get("installation_identity"),
                );
            }
            present_fields(output, value, &[("Next action", "/next_action")]);
            if value["purpose"] == "candidate_trial" {
                scientific(
                    output,
                    "Exact candidate installation",
                    value.get("installation"),
                );
                if value["stage"] == "editable" {
                    output.push_str("Review this exact candidate installation, execution location and complete disclosure before explicitly authorizing execution with analysis submit DIR.\n");
                }
            }
            candidate_analysis_guidance(output, args, value);
        }
        Command::Model(Model::Get { id } | Model::Create { id, .. }) => {
            model(output, args, value, id)
        }
        Command::Model(Model::Publication { .. }) => publication(output, args, value),
        Command::Model(Model::Init { .. }) => {
            output.push_str("Author project created\n");
            present_fields(
                output,
                value,
                &[
                    ("Project", "/project"),
                    ("Model", "/model_id"),
                    ("Files", "/files"),
                    ("Publication source", "/publication"),
                ],
            );
            output.push_str("Next: edit the scientific contract and run the project's local tests before installation and publication.\n");
        }
        Command::Model(Model::Validate { .. }) => {
            output.push_str("Local publication validation\n");
            receipt(output, value);
            output.push_str("Local validation does not establish API authorization, connector readiness or scientific correctness.\n");
        }
        Command::Connector(Connector::Context(
            ConnectorContext::Create { .. } | ConnectorContext::Update { .. },
        )) => {
            output.push_str("Execution context\n");
            fields(
                output,
                value,
                &[
                    ("Context", "/id"),
                    ("Generation", "/generation"),
                    ("Location", "/metadata/location"),
                    ("Disclosure", "/metadata/disclosure"),
                ],
            );
            present_fields(
                output,
                value,
                &[
                    ("Connector", "/connector_id"),
                    ("Driver", "/definition/driver/kind"),
                    ("Connection", "/definition/connection/kind"),
                ],
            );
        }
        Command::Connector(Connector::Context(
            ConnectorContext::Get { .. } | ConnectorContext::WaitRetirement { .. },
        )) => {
            context(output, value);
        }
        Command::Connector(Connector::Context(ConnectorContext::ReplacementPlan {
            connector,
            context: id,
            replacement,
            section,
            page,
        })) => {
            output.push_str("Context replacement plan\n");
            context(output, &value["context"]);
            scientific(output, "Proposed replacement", value.get("replacement"));
            scientific(
                output,
                "Required actions and responsible roles",
                value.get("actions"),
            );
            scientific(
                output,
                "Dependency page",
                value.pointer("/dependencies/items"),
            );
            if let Some(cursor) = value
                .pointer("/dependencies/next_cursor")
                .and_then(Value::as_str)
            {
                let mut words = vec![
                    "connector".into(),
                    "context".into(),
                    "replacement-plan".into(),
                    connector.clone(),
                    id.clone(),
                    "--section".into(),
                    enumeray_contracts::management::ContextDependencySection::from(*section)
                        .as_str()
                        .into(),
                ];
                if let Some(replacement) = replacement {
                    words.extend(["--replacement".into(), replacement.clone()]);
                }
                if let Some(limit) = page.limit {
                    words.extend(["--limit".into(), limit.to_string()]);
                }
                words.extend(["--cursor".into(), cursor.to_owned()]);
                output.push_str(&format!("Next page: {}\n", super::invocation(args, &words)));
            }
        }
        Command::Connector(Connector::Context(
            ConnectorContext::Cutover { dry_run, .. }
            | ConnectorContext::Reopen { dry_run, .. }
            | ConnectorContext::ReleaseConfigurations { dry_run, .. }
            | ConnectorContext::Retire { dry_run, .. },
        )) => {
            if *dry_run {
                output.push_str("Context change preview (no change applied)\n");
                fields(
                    output,
                    value,
                    &[("Operation", "/operation"), ("Effects", "/effects")],
                );
                context(output, &value["observed"]);
                scientific(output, "Exact guarded request", value.get("request"));
                scientific(
                    output,
                    "Required actions and responsible roles",
                    value.pointer("/plan/actions"),
                );
                output.push_str("Counts describe the current observation; the service rechecks blockers when the change commits.\n");
            } else {
                output.push_str("Context transition receipt\n");
                fields(
                    output,
                    value,
                    &[
                        ("Operation", "/operation"),
                        ("Cancelled queued runs", "/cancelled_queued_runs"),
                        (
                            "Requested publication retirements",
                            "/requested_publications",
                        ),
                    ],
                );
                context(output, &value["context"]);
            }
        }
        Command::Model(Model::Members { .. })
        | Command::Connector(Connector::Member(ConnectorMember::List { .. })) => {
            output.push_str("Resource administration\n");
            fields(
                output,
                value,
                &[
                    ("Owner", "/owner_id"),
                    ("Generation", "/generation"),
                    ("Co-administrators", "/co_admins"),
                ],
            );
            output.push_str("Use this observed generation for a guarded membership change; re-read after each change.\n");
            if matches!(&args.command, Command::Connector(Connector::Member(_))) {
                output.push_str("This is the ownership generation; execution configuration uses a separate generation.\n");
            }
        }
        Command::Connector(
            Connector::Get { .. }
            | Connector::Configure { .. }
            | Connector::Diagnostics { .. }
            | Connector::WaitMaintenance { .. }
            | Connector::Update { .. },
        ) => connector(output, value),
        Command::Connector(
            Connector::Maintenance { dry_run, .. }
            | Connector::Resume { dry_run, .. }
            | Connector::WithdrawAuthority { dry_run, .. }
            | Connector::RestoreAuthority { dry_run, .. },
        ) => {
            if *dry_run {
                output.push_str("Connector change preview (no change applied)\n");
                fields(
                    output,
                    value,
                    &[
                        ("Connector", "/connector_id"),
                        ("Observed generation", "/observed/generation"),
                        ("Observed maintenance", "/observed/draining"),
                        ("Proposed maintenance", "/proposed/draining"),
                        (
                            "Observed authority withdrawn",
                            "/observed/authority_withdrawn",
                        ),
                        (
                            "Proposed authority withdrawn",
                            "/proposed/authority_withdrawn",
                        ),
                        ("Effects", "/effects"),
                    ],
                );
                work(output, &value["observed"]);
            } else {
                connector(output, value);
            }
        }
        Command::Access(Access::Policy { .. }) => {
            output.push_str("Model access policy\n");
            fields(
                output,
                value,
                &[
                    ("Mode", "/mode"),
                    ("Visibility", "/visibility"),
                    ("Generation", "/generation"),
                ],
            );
        }
        Command::Access(Access::SetPolicy { model, dry_run, .. }) => {
            field(output, "Model", Some(&Value::String(model.clone())));
            if *dry_run {
                output.push_str("Access policy preview (no change applied)\n");
                fields(
                    output,
                    value,
                    &[
                        ("Observed generation", "/observed/generation"),
                        ("Observed mode", "/observed/mode"),
                        ("Proposed mode", "/proposed/mode"),
                        ("Observed visibility", "/observed/visibility"),
                        ("Proposed visibility", "/proposed/visibility"),
                    ],
                );
            } else {
                output.push_str("Access policy receipt\n");
                fields(
                    output,
                    value,
                    &[
                        ("Mode", "/mode"),
                        ("Visibility", "/visibility"),
                        ("Generation", "/generation"),
                    ],
                );
            }
        }
        Command::Auth(Auth::Profiles) => {
            output.push_str("Configured profiles\n");
            if let Some(items) = value["profiles"].as_array() {
                table(
                    output,
                    items,
                    &[
                        ("PROFILE", "/name"),
                        ("API", "/api_url"),
                        ("AUTHENTICATED", "/authenticated"),
                        ("EXPIRES (UNIX SECONDS)", "/expires_at"),
                    ],
                );
            }
        }
        Command::Auth(Auth::Whoami) => identity(output, value),
        Command::Auth(Auth::Login { .. } | Auth::Refresh) => {
            receipt(output, value);
            identity(output, &value["identity"]);
        }
        Command::Service(Service::Status) => {
            output.push_str("Service status\n");
            fields(
                output,
                value,
                &[
                    ("Queued runs", "/queued_runs"),
                    ("Running runs", "/running_runs"),
                    ("Blocked runs", "/blocked_runs"),
                    ("Oldest queued at", "/oldest_queued_at"),
                    ("Oldest queue age (seconds)", "/oldest_queue_age_seconds"),
                    ("Connectors behind configuration", "/configuration_lag"),
                    ("Offline connectors", "/offline_connectors"),
                    ("Expired enrollments", "/expired_enrollments"),
                    ("Failed publications", "/failed_publications"),
                    ("Unconfirmed executions", "/unconfirmed_executions"),
                    (
                        "Unconfirmed result transfers",
                        "/unconfirmed_result_transfers",
                    ),
                    ("Pending revocations", "/pending_revocations"),
                    ("Draining revocations", "/draining_revocations"),
                ],
            );
        }
        Command::Service(Service::Limits | Service::SetLimits { .. }) => {
            output.push_str("Service queue limits\n");
            fields(
                output,
                value,
                &[
                    ("Maximum queued runs", "/max_queued_runs"),
                    ("Maximum queued runs per user", "/max_queued_runs_per_user"),
                    ("Generation", "/generation"),
                ],
            );
        }
        Command::Run(Run::Get { id } | Run::Wait { id, .. }) => run(output, args, value, id),
        Command::Run(Run::Cancel { id }) => {
            output.push_str("Cancellation receipt\n");
            fields(
                output,
                value,
                &[
                    ("Run", "/run_id"),
                    ("Status", "/status"),
                    ("Scope", "/scope"),
                    ("Requested by", "/requested_by_user_id"),
                    ("Reason", "/reason"),
                    ("Requested at", "/requested_at"),
                    ("Confirmed at", "/cancelled_at"),
                ],
            );
            output.push_str(
                "Only confirmed backend evidence establishes cancellation. Inspect the same run:\n",
            );
            next(output, args, &["run", "get", id]);
        }
        Command::Run(Run::Provenance { output: None, .. }) => provenance(output, value),
        Command::Run(Run::LogArchive { .. }) => {
            output.push_str("Run log archive\n");
            present_fields(
                output,
                value,
                &[
                    ("Status", "/status"),
                    ("State", "/state"),
                    ("Artifact", "/artifact_id"),
                    ("Expires", "/expires_at"),
                    ("Diagnostic", "/diagnostic"),
                ],
            );
            output.push_str("Only a ready, available archive can be downloaded; use --json for its full manifest.\n");
        }
        Command::Artifact(Artifact::Get { .. }) => {
            output.push_str("Artifact\n");
            fields(
                output,
                value,
                &[
                    ("Artifact", "/artifact_id"),
                    ("Name", "/description/logical_name"),
                    ("Media type", "/description/media_type"),
                    ("Availability", "/availability"),
                    ("Verified bytes", "/content/size_bytes"),
                    ("SHA-256", "/content/checksum/value"),
                ],
            );
            present_fields(
                output,
                value,
                &[
                    ("Producing run", "/provenance/run_id"),
                    ("Created", "/created_at"),
                ],
            );
        }
        _ => {
            output.push_str("Operation receipt\n");
            receipt(output, value);
            if matches!(
                &args.command,
                Command::Connector(
                    Connector::Member(_) | Connector::Ownership(_) | Connector::Archive { .. }
                )
            ) {
                output.push_str("Generation is the connector ownership generation; execution configuration uses a separate generation.\n");
            }
            match &args.command {
                Command::Connector(
                    Connector::Create { id, .. } | Connector::Enrollment { id, .. },
                ) => {
                    output.push_str("Keep the saved enrollment document private and retain the host's durable identity and execution journal.\n");
                    next(output, args, &["connector", "get", id]);
                }
                Command::Access(Access::Invite { .. })
                | Command::Auth(Auth::TokenCreate { .. }) => output.push_str(
                    "The secret is in the saved private file; keep it out of arguments and logs.\n",
                ),
                _ => (),
            }
        }
    }
}

fn receipt(output: &mut String, value: &Value) {
    // Never serialize entire credential responses or installation environments.
    present_fields(
        output,
        value,
        &[
            ("ID", "/id"),
            ("Model", "/model_id"),
            ("Publication", "/publication_id"),
            ("Current publication", "/current_publication_id"),
            ("Connector", "/connector_id"),
            ("Connector", "/connector"),
            ("Context", "/context_id"),
            ("Grant", "/grant_id"),
            ("User", "/user_id"),
            ("Transfer", "/transfer_id"),
            ("Invitation", "/invitation_id"),
            ("Generation", "/generation"),
            ("Saved to", "/saved_to"),
            ("Output", "/output"),
            ("Path", "/path"),
            ("Bytes", "/size_bytes"),
            ("Artifact", "/artifact_id"),
            ("State", "/state"),
            ("Status", "/status"),
            ("Profile", "/profile"),
            ("API", "/api_url"),
            ("Authenticated", "/authenticated"),
            ("Refreshed", "/refreshed"),
            ("Logged out", "/logged_out"),
            ("Server tokens revoked", "/server_tokens_revoked"),
            ("Expires", "/expires_at"),
            ("Revoked", "/revoked"),
            ("Revoked at", "/revoked_at"),
            ("Archived", "/archived"),
            ("Accepted", "/accepted"),
            ("Allowed", "/allowed"),
            ("Co-administrator", "/co_admin"),
            ("Published", "/published"),
            ("Retired", "/retired"),
            ("Changed", "/changed"),
            ("Valid", "/valid"),
            ("Validation", "/validation"),
            ("Authorization validation", "/authorization_validation"),
            ("Context validation", "/context_validation"),
            ("Installation validation", "/installation_validation"),
            ("Queue limit", "/max_queued_runs"),
            ("Assignment limit", "/max_outstanding_assignments"),
        ],
    );
}

fn identity(output: &mut String, value: &Value) {
    output.push_str("Authenticated identity\n");
    fields(
        output,
        value,
        &[
            ("User", "/user_id"),
            ("Label", "/label"),
            ("Permissions", "/permissions"),
            ("Credential scopes", "/credential_scopes"),
            ("Service administrator", "/is_platform_admin"),
        ],
    );
}

fn model(output: &mut String, args: &Args, value: &Value, id: &str) {
    output.push_str("Model\n");
    fields(
        output,
        value,
        &[
            ("Model", "/model_id"),
            ("Name", "/name"),
            ("Description", "/description"),
            ("Owner", "/owner_id"),
            ("Generation", "/generation"),
            ("Access mode", "/access_mode"),
            ("Visibility", "/visibility"),
            ("Can run", "/can_run"),
            ("Can manage", "/can_manage"),
            ("Current publication", "/current_publication_id"),
        ],
    );
    let publication = &value["current_publication"];
    if publication.is_object() {
        fields(
            output,
            publication,
            &[
                ("Installation status", "/status"),
                ("Available for submission", "/available"),
                (
                    "Availability reason",
                    "/submission_availability/unavailable_reason",
                ),
                ("Context", "/context_id"),
                ("Execution location", "/execution_location"),
                ("Data-access disclosure", "/disclosure"),
            ],
        );
        definition(output, &publication["definition"]);
        output.push_str(
            "Availability is advisory; submission rechecks access and the selected publication.\n",
        );
    } else {
        output.push_str("Publication details and availability: unknown / not disclosed.\n");
    }
    if value["can_run"] == true && publication["available"] == true {
        output.push_str("Initialize one new directory for the intended execution, then review its inputs, destination and disclosure before explicitly submitting:\n");
        next(
            output,
            args,
            &[
                "analysis",
                "init",
                id,
                "--inputs",
                "INPUTS.json",
                "--directory",
                "NEW_ANALYSIS_DIR",
            ],
        );
    } else if value["can_run"] == false {
        match value["access_mode"].as_str() {
            Some("request") => {
                output.push_str(
                    "Request approval if appropriate; an explicit block may still prevent it.\n",
                );
                next(output, args, &["access", "request", id]);
            }
            Some("invite") => {
                output.push_str("Next: ask the model owner for a recipient-bound invitation.\n")
            }
            _ => output.push_str(
                "Next: ask the model owner to explain the execution access restriction.\n",
            ),
        }
    } else {
        output.push_str("Next: ask the model administrator to review publication availability before submitting.\n");
    }
}

fn definition(output: &mut String, definition: &Value) {
    present_fields(
        output,
        definition,
        &[
            ("Model identity (author claim)", "/model_identity"),
            ("Wrapper identity (author claim)", "/wrapper_identity"),
            ("Runtime identity (author claim)", "/runtime_identity"),
            ("Dataset identities (author claims)", "/dataset_identities"),
        ],
    );
    scientific(output, "Input schema", definition.get("input_schema"));
    scientific(output, "Output schema", definition.get("output_schema"));
    scientific(output, "Scientific examples", definition.get("examples"));
}

fn candidate_analysis_guidance(output: &mut String, args: &Args, value: &Value) {
    if value["purpose"] != "candidate_trial" {
        return;
    }
    let (Some(model), Some(publication)) =
        (value["model_id"].as_str(), value["publication_id"].as_str())
    else {
        return;
    };
    output.push_str("Inspect this exact candidate and its retained trial evidence:\n");
    next(output, args, &["model", "publication", model, publication]);
    output.push_str("After collecting evidence and completing your scientific checks, use model release status RELEASE_DIR and model release review RELEASE_DIR with your existing release directory. Trial success is advisory; activation remains explicit. If this account has no release directory, adopt the exact candidate into a new directory (replace NEW_RELEASE_DIR):\n");
    next(
        output,
        args,
        &[
            "model",
            "release",
            "init",
            model,
            "--candidate",
            publication,
            "--directory",
            "NEW_RELEASE_DIR",
        ],
    );
}

fn publication(output: &mut String, args: &Args, value: &Value) {
    output.push_str("Publication\n");
    fields(
        output,
        value,
        &[
            ("Publication", "/id"),
            ("Status", "/status"),
            ("Generation", "/generation"),
            ("Context", "/context_id"),
            ("Compute grant", "/grant_id"),
            ("Validation message", "/validation_message"),
            ("Admission open", "/admission_open"),
            ("Execution location", "/execution_location"),
            ("Data-access disclosure", "/disclosure"),
            ("Published at", "/published_at"),
            ("Revoked at", "/revoked_at"),
            ("Installation revoked at", "/installation_revoked_at"),
        ],
    );
    present_fields(
        output,
        value,
        &[
            (
                "Trial available to this credential",
                "/trial_availability/available",
            ),
            ("Trial blocker", "/trial_availability/unavailable_reason"),
            ("Activation available", "/activation_availability/available"),
            (
                "Activation blocker",
                "/activation_availability/unavailable_reason",
            ),
            ("Grant holder", "/release_authority/grant_holder_user_id"),
            (
                "Named delegation",
                "/release_authority/publication_delegation_id",
            ),
            ("May delegate", "/release_authority/can_delegate"),
            ("Latest trial", "/trial_evidence/latest_run/run_id"),
            ("Latest trial outcome", "/trial_evidence/latest_run/state"),
            (
                "Latest successful trial",
                "/trial_evidence/latest_succeeded_run/run_id",
            ),
        ],
    );
    if let Some(counts) = value.pointer("/trial_evidence/counts") {
        scientific(output, "Trial counts by execution state", Some(counts));
        output.push_str("Installation readiness and trial execution success do not certify scientific correctness. Author scientific validation is required before promotion.\n");
    }
    if let Some(id) = value
        .pointer("/trial_evidence/latest_run/run_id")
        .and_then(Value::as_str)
    {
        for command in ["get", "artifacts", "logs", "provenance"] {
            next(output, args, &["run", command, id]);
        }
    }
    present_fields(
        output,
        value,
        &[
            ("Validation authority", "/validation_evidence/authority"),
            ("Validation observed at", "/validation_evidence/observed_at"),
        ],
    );
    present_fields(
        output,
        value,
        &[
            ("Configuration retention", "/configuration_retention/state"),
            (
                "Retirement blocked",
                "/configuration_retention/retirement_blocked",
            ),
            (
                "Retirement queued runs",
                "/configuration_retention/blockers/queued_runs",
            ),
            (
                "Retirement running runs",
                "/configuration_retention/blockers/running_runs",
            ),
            (
                "Retirement pending deliveries",
                "/configuration_retention/blockers/pending_delivery_assignments",
            ),
            (
                "Retirement pending uploads",
                "/configuration_retention/blockers/pending_assignment_uploads",
            ),
            (
                "Retirement pending archives",
                "/configuration_retention/blockers/pending_log_archives",
            ),
            ("Rollback available", "/rollback_availability/available"),
            (
                "Rollback unavailable reason",
                "/rollback_availability/unavailable_reason",
            ),
            (
                "Rollback requires revalidation",
                "/rollback_availability/requires_revalidation",
            ),
        ],
    );
    definition(output, &value["definition"]);
    output.push_str("Readiness establishes installation evidence; scientific validation remains the author's responsibility. Full retained evidence is available with --json.\n");
}

fn work(output: &mut String, value: &Value) {
    present_fields(
        output,
        value,
        &[
            ("Retained queued runs", "/work/retained_queued_runs"),
            ("Unfinished assignments", "/work/unfinished_assignments"),
            (
                "Pending delivery assignments",
                "/work/pending_delivery_assignments",
            ),
            ("Pending uploads", "/work/pending_uploads"),
            ("Pending log archives", "/work/pending_log_archives"),
            ("Unconfirmed assignments", "/work/unconfirmed_assignments"),
            ("Preparing", "/work/assignment_phases/preparing"),
            (
                "Waiting for backend",
                "/work/assignment_phases/waiting_for_backend",
            ),
            ("Executing", "/work/assignment_phases/executing"),
            (
                "Collecting results",
                "/work/assignment_phases/collecting_results",
            ),
            (
                "Uploading results",
                "/work/assignment_phases/uploading_results",
            ),
        ],
    );
}

fn context(output: &mut String, value: &Value) {
    output.push_str("Execution context lifecycle\n");
    fields(
        output,
        value,
        &[
            ("Context", "/id"),
            ("Connector", "/connector_id"),
            ("Generation", "/generation"),
            ("State", "/state"),
            ("Replacement context", "/replacement_context_id"),
            ("Accepted work policy", "/accepted_work"),
            ("Live current publications", "/live_current_publications"),
            (
                "Retained publication configurations",
                "/retained_publications",
            ),
            (
                "Pending configuration retirements",
                "/pending_configuration_retirements",
            ),
            (
                "Configuration removal generation",
                "/configuration_removal_generation",
            ),
            ("Connector generation", "/connector_generation"),
            ("Connector observed generation", "/observed_generation"),
            ("Context retirement ready", "/retirement_ready"),
            ("Local cleanup", "/local_cleanup_state"),
        ],
    );
    work(output, value);
    output.push_str("Whole connector retained work\n");
    work(output, &serde_json::json!({"work":value["connector_work"]}));
    output.push_str("Context retirement readiness does not establish whole-connector shutdown readiness or local cleanup safety. Inspect local evidence with enumeray connector inspect --journal PATH and enumeray connector cleanup --journal PATH --dry-run.\n");
}

fn connector(output: &mut String, value: &Value) {
    output.push_str("Connector\n");
    fields(
        output,
        value,
        &[
            ("Connector", "/connector_id"),
            ("Owner", "/owner_id"),
            ("Ownership generation", "/ownership_generation"),
            ("Archived at", "/archived_at"),
            ("Generation", "/generation"),
        ],
    );
    present_fields(
        output,
        value,
        &[
            ("Label", "/label"),
            ("Observed generation", "/observed_generation"),
            ("Configuration pending", "/configuration_pending"),
            ("Maintenance", "/draining"),
            ("Authority withdrawn", "/authority_withdrawn"),
            ("Maintenance complete", "/maintenance_complete"),
            ("Assignment limit", "/max_outstanding_assignments"),
            ("Failed publications", "/failed_publications"),
            ("Changed", "/changed"),
        ],
    );
    work(output, value);
    present_fields(
        output,
        value,
        &[
            (
                "Desired configuration bytes",
                "/configuration_capacity/desired_bytes",
            ),
            (
                "Maximum configuration bytes",
                "/configuration_capacity/maximum_desired_bytes",
            ),
            (
                "Retained publications",
                "/configuration_capacity/counts/retained_publications",
            ),
            (
                "Maximum retained publications",
                "/configuration_capacity/maximum_retained_publications",
            ),
            (
                "Historical publications",
                "/configuration_capacity/counts/historical_publications",
            ),
            (
                "Retirement requested",
                "/configuration_capacity/counts/retirement_requested",
            ),
            (
                "Retired publications",
                "/configuration_capacity/counts/retired_publications",
            ),
        ],
    );
}

fn run(output: &mut String, args: &Args, value: &Value, id: &str) {
    output.push_str("Run observation\n");
    fields(
        output,
        value,
        &[
            ("Run", "/run_id"),
            ("Model", "/model_id"),
            ("Publication", "/publication_id"),
            ("Context", "/context_id"),
            ("Execution location", "/execution_location"),
            ("Data-access disclosure", "/disclosure"),
            ("Created", "/created_at"),
        ],
    );
    output.push_str(&super::observation(value));
    if value["result"].is_object() {
        present_fields(
            output,
            &value["result"],
            &[("Result outcome", "/receipt/outcome")],
        );
        scientific(
            output,
            "Scientific outputs",
            value.pointer("/result/outputs"),
        );
        scientific(output, "Output files", value.pointer("/result/files"));
        scientific(
            output,
            "Optional output failures",
            value.pointer("/result/optional_output_failures"),
        );
        scientific(output, "Failure", value.pointer("/result/failure"));
    } else {
        output.push_str("Result: not recorded.\n");
    }
    match value["state"].as_str() {
        Some("succeeded" | "failed" | "cancelled") => next(
            output,
            args,
            &["run", "provenance", id, "--output", "provenance.json"],
        ),
        _ => next(output, args, &["run", "get", id]),
    }
}

fn provenance(output: &mut String, value: &Value) {
    output.push_str("Retained run provenance\n");
    fields(
        output,
        value,
        &[
            ("Format version", "/format_version"),
            ("Run", "/run/run_id"),
            ("Model", "/run/model_id"),
            ("State", "/run/state"),
            ("Publication", "/publication/publication_id"),
            ("Context", "/publication/context_id"),
            ("Compute grant", "/publication/compute_grant_id"),
            ("Generation", "/publication/generation"),
            ("Execution location", "/publication/execution_location"),
            ("Data-access disclosure", "/publication/disclosure"),
            ("Validation authority", "/publication/validation/authority"),
            ("Validation observed", "/publication/validation/observed_at"),
        ],
    );
    scientific(
        output,
        "Scientific outputs",
        value.pointer("/result/outputs"),
    );
    scientific(output, "Output files", value.pointer("/result/files"));
    scientific(
        output,
        "Optional output failures",
        value.pointer("/result/optional_output_failures"),
    );
    scientific(output, "Failure", value.pointer("/result/failure"));
    output.push_str("Use --json or run provenance RUN_ID --output NEW_PATH to retain the complete immutable evidence.\n");
}
