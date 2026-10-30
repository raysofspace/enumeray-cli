use super::*;
use clap::Parser;
use serde_json::json;
use std::process::Command as Process;

fn args(words: &[&str]) -> Args {
    Args::parse_from(std::iter::once("enumeray").chain(words.iter().copied()))
}

#[test]
fn recovered_analysis_output_names_missing_evidence_without_exposing_private_requests() {
    let value = json!({"origin":"imported_prepared","stage":"submission_uncertain","evidence":{"publication":"unavailable","source_fingerprints":"unavailable"},"prepared":{"idempotency_key":"never-print-this"},"next_action":"Explicitly submit this original operation."});
    let rendered = render(
        &args(&[
            "analysis",
            "import",
            "--prepared",
            "request.json",
            "--directory",
            "recovered",
        ]),
        &value,
    );
    assert!(rendered.contains("Recovery origin: imported_prepared"));
    assert!(rendered.contains("unavailable"));
    assert!(rendered.contains("Explicitly submit"));
    assert!(!rendered.contains("never-print-this"));
}

#[test]
fn observations_name_responsible_roles_without_inventing_blockage_or_outcome() {
    for (reason, role) in [
        ("model_access_revoked", "model administrator"),
        ("publication_revoked", "model administrator"),
        ("installation_revoked", "model administrator"),
        ("no_approved_execution_path", "model administrator"),
        ("compute_access_revoked", "connector owner"),
        ("compute_unavailable", "connector owner"),
        ("connector_maintenance", "connector owner"),
        ("future_reason", "unknown"),
    ] {
        let value =
            json!({"state":"queued","blocked_reason":reason,"observation_freshness":"unknown"});
        let output = observation(&value);
        assert!(
            output.contains(&format!("Next responsible role: {role}")),
            "{output}"
        );
        assert!(!output.contains("100%"));
    }
    let unknown = observation(
        &json!({"state":"queued","blocked_reason":null,"observation_freshness":"unknown"}),
    );
    assert!(unknown.contains("Waiting cause is unknown"));
    let stale = observation(
        &json!({"state":"running","phase":"executing","observation_freshness":"stale"}),
    );
    assert!(stale.contains("stale observation does not establish failure"));
    assert!(stale.contains("Next responsible role: connector owner"));
    for state in ["succeeded", "failed", "cancelled"] {
        let output = observation(&json!({"state":state,"observation_freshness":"stale"}));
        assert!(output.contains("run submitter / model administrator"));
    }
}

#[test]
fn catalogue_is_a_table_with_full_ids_unicode_and_no_invented_access() {
    let value = json!({"items":[{"model_id":"a-model-with-a-long-identity","name":"Ääni λ","description":"safe\n\u{1b}[2J\u{202e}text","token":"never-print-this"}],"next_cursor":null});
    let output = render(&args(&["model", "list"]), &value);
    assert!(output.contains("MODEL"));
    assert!(output.contains("a-model-with-a-long-identity"));
    assert!(output.contains("Ääni λ"));
    assert!(output.contains("\\u{a}\\u{1b}[2J\\u{202e}text"));
    assert!(output.contains("Returned 1 item(s)"));
    assert!(output.contains("model get MODEL_ID"));
    assert!(!output.contains("never-print-this"));
    assert!(!output.contains("CAN RUN"));
    assert!(!output.contains('\u{1b}'));
    let empty = render(
        &args(&["model", "list"]),
        &json!({"items":[],"next_cursor":null}),
    );
    assert!(empty.contains("No items returned"));
    assert!(empty.contains("Returned 0 item(s)"));
    assert!(!empty.contains("Next page:"));
}

#[test]
fn model_details_distinguish_access_availability_and_disclosure() {
    let mut value = json!({"model_id":"example","name":"Example","generation":17,"can_run":true,"can_manage":false,"current_publication_id":"release-one","current_publication":{"available":true,"execution_location":"Helsinki","disclosure":"One line\nSecond line","definition":{"input_schema":{"type":"object"},"output_schema":{"type":"object"},"examples":[{"inputs":{"value":2},"outputs":{"doubled":4}}]},"descriptor":{"environment":{"PASSWORD":"never-print-this"}}}});
    let output = render(&args(&["model", "get", "example"]), &value);
    for expected in [
        "Generation: 17",
        "Can run: yes",
        "Available for submission: yes",
        "Input schema:",
        "Scientific examples:",
        "Second line",
        "analysis init example --inputs INPUTS.json --directory NEW_ANALYSIS_DIR",
    ] {
        assert!(output.contains(expected), "{expected}: {output}");
    }
    assert!(!output.contains("never-print-this"));
    value["current_publication"]["available"] = json!(false);
    value["current_publication"]["submission_availability"]["unavailable_reason"] =
        json!("connector_maintenance");
    let output = render(&args(&["model", "get", "example"]), &value);
    assert!(output.contains("connector_maintenance"));
    assert!(!output.contains("analysis init"));
    value["can_run"] = json!(false);
    value["access_mode"] = json!("request");
    value["current_publication"] = Value::Null;
    let output = render(&args(&["model", "get", "example"]), &value);
    assert!(output.contains("unknown / not disclosed"));
    assert!(output.contains("access request example"));
    assert!(!output.contains("Input schema:"));
}

#[test]
fn editable_analysis_displays_the_entire_disclosure_before_explicit_submission() {
    let disclosure = format!(
        "{}\nThe last required disclosure line.",
        "complete disclosure ".repeat(200)
    );
    let output = render(
        &args(&[
            "analysis",
            "init",
            "example",
            "--inputs",
            "inputs.json",
            "--directory",
            "private dir/analysis's",
        ]),
        &json!({"directory":"private dir/analysis's","model_id":"example","publication_id":"release-one","context_id":"cpu","execution_location":"Lab","disclosure":disclosure,"stage":"editable","next_action":"Review the destination/disclosure, then explicitly authorize execution with analysis submit DIR."}),
    );
    assert!(output.contains(&disclosure.replace('\n', "\n  ")));
    assert!(output.contains("Directory: private dir/analysis's"));
    assert!(output.contains("Stage: editable"));
    assert!(output.contains("explicitly authorize execution with analysis submit DIR"));
    assert!(!output.contains("Original acceptance receipt"));
}

#[test]
fn candidate_analysis_shows_reviewed_identity_and_release_guidance() {
    let disclosure = format!(
        "{}\nLast disclosure line.",
        "scientific access ".repeat(200)
    );
    let value = json!({"directory":"trial-r1","model_id":"example","publication_id":"candidate-one","purpose":"candidate_trial","grant_id":"captured-grant","publication_delegation_id":"named-handoff","context_id":"lab","installation_status":"ready","installation":{"release_path":"/opt/models/candidate-one"},"execution_location":"Research lab","disclosure":disclosure,"stage":"editable"});
    let output = render(&args(&["analysis", "status", "trial-r1"]), &value);
    for text in [
        "Purpose: candidate_trial",
        "Captured grant: captured-grant",
        "Named delegation: named-handoff",
        "Exact candidate installation:",
        "/opt/models/candidate-one",
        "Last disclosure line.",
        "before explicitly authorizing execution",
        "model publication example candidate-one",
        "model release status RELEASE_DIR",
        "model release review RELEASE_DIR",
        "model release init example --candidate candidate-one --directory NEW_RELEASE_DIR",
        "Trial success is advisory",
    ] {
        assert!(output.contains(text), "{text}: {output}");
    }
    let logs = render(
        &args(&["analysis", "logs", "trial-r1"]),
        &json!({"model_id":"example","publication_id":"candidate-one","purpose":"candidate_trial","grant_id":"captured-grant","publication_delegation_id":"named-handoff","run_id":"trial-one","items":[],"next_cursor":null}),
    );
    assert!(logs.contains("Purpose: candidate_trial"));
    assert!(logs.contains("Captured grant: captured-grant"));
    assert!(logs.contains("Named delegation: named-handoff"));
}

#[test]
fn release_trial_command_remains_separate_from_explicit_activation() {
    let value = json!({"trial_init_command":"enumeray analysis init example --candidate candidate-one --inputs INPUTS.json --directory NEW_TRIAL_DIR","next_command":"enumeray model release activate release-r1","trial_evidence":{"counts":{"failed":1,"cancelled":1}}});
    let output = render(&args(&["model", "release", "review", "release-r1"]), &value);
    assert!(output.contains(
        "Start candidate trial: enumeray analysis init example --candidate candidate-one"
    ));
    assert!(output.contains("Replace INPUTS.json"));
    assert!(output.contains("NEW_TRIAL_DIR"));
    assert!(output.contains("Release status/review includes its trial evidence automatically"));
    assert!(output.contains("Next command: enumeray model release activate release-r1"));
    assert!(output.contains("Trial success is advisory"));
}

#[test]
fn accepted_receipt_and_terminal_observations_remain_distinct() {
    let receipt = render(
        &args(&["analysis", "submit", "saved-analysis"]),
        &json!({"directory":"saved-analysis","model_id":"example","publication_id":"one","stage":"accepted","receipt":{"run_id":"11111111-1111-4111-8111-111111111111","state":"queued"},"next_action":"Observe with analysis status/wait DIR; collect terminal evidence with analysis collect DIR."}),
    );
    assert!(receipt.contains("Accepted state (original receipt): queued"));
    assert!(receipt.contains("This receipt does not establish current execution state."));
    assert!(receipt.contains("analysis status/wait DIR"));
    assert!(!receipt.contains("run submit"));
    for state in ["succeeded", "failed", "cancelled"] {
        let output = render(
            &args(&["run", "get", "run-one"]),
            &json!({"run_id":"run-one","state":state,"observation_freshness":"stale","cancellation":{"status":"confirmed","cancelled_at":"2026-10-15T00:00:00Z"},"result":{"receipt":{"outcome":state},"outputs":{"doubled":4},"files":{"report":{"artifact_id":"full-artifact-id"}},"failure":{"code":"example_failure","message":"retained failure"}}}),
        );
        for expected in [
            format!("State: {state}"),
            "Observation freshness: stale".into(),
            "Cancellation status: confirmed".into(),
            "2026-10-15T00:00:00Z".into(),
            "\"doubled\": 4".into(),
            "full-artifact-id".into(),
            "retained failure".into(),
        ] {
            assert!(output.contains(&expected), "{expected}: {output}");
        }
    }
}

#[test]
fn previews_generations_profiles_and_secret_receipts_are_readable() {
    let output = render(
        &args(&[
            "access",
            "set-policy",
            "example",
            "--mode",
            "request",
            "--generation",
            "7",
            "--dry-run",
        ]),
        &json!({"observed":{"generation":7,"mode":"open","visibility":"unlisted"},"proposed":{"mode":"request","visibility":"unlisted"},"token":"never-print-this"}),
    );
    for expected in [
        "no change applied",
        "Observed generation: 7",
        "Proposed mode: request",
        "Proposed visibility: unlisted",
    ] {
        assert!(output.contains(expected));
    }
    let output = render(
        &args(&[
            "connector",
            "maintenance",
            "worker",
            "--generation",
            "7",
            "--dry-run",
        ]),
        &json!({"connector_id":"worker","observed":{"generation":7,"draining":false,"authority_withdrawn":true,"work":{"unfinished_assignments":2}},"proposed":{"draining":true,"authority_withdrawn":true},"effects":"retain queued work"}),
    );
    assert!(output.contains("Proposed authority withdrawn: yes"));
    assert!(output.contains("Unfinished assignments: 2"));
    let output = render(
        &args(&["auth", "profiles"]),
        &json!({"profiles":[{"name":"science","api_url":"https://example.org","authenticated":true,"access_token":"never-print-this"}]}),
    );
    assert!(output.contains("science"));
    assert!(!output.contains("never-print-this"));
    let output = render(
        &args(&[
            "auth",
            "token-create",
            "bot",
            "--scopes",
            "me:read",
            "--output",
            "token.json",
        ]),
        &json!({"saved_to":"token.json","id":"full-token-id","token":"never-print-this"}),
    );
    assert!(output.contains("Saved to: token.json"));
    assert!(output.contains("full-token-id"));
    assert!(!output.contains("never-print-this"));
}

#[test]
fn logs_and_events_keep_gap_reset_resume_and_cancellation_evidence() {
    let value = json!({"items":[{"source":"stdout","position":9,"epoch":2,"offset":1024,"byte_length":4,"text":"λ\n\u{1b}[31mtext","source_reset":true,"gap":{"reason":"capture_limit","bytes":1024}}],"next_cursor":"page-two","resume_cursor":"resume-nine","retention":{"earliest_position":5,"latest_position":9}});
    let output = render(&args(&["run", "logs", "run-one", "--limit", "1"]), &value);
    for expected in [
        "Source reset",
        "Log gap",
        "Omitted bytes: 1024",
        "Next page:",
        "--cursor page-two",
        "Resume later (after this page):",
        "--cursor resume-nine",
        "Earliest retained position: 5",
        "λ",
    ] {
        assert!(output.contains(expected), "{expected}: {output}");
    }
    assert!(!output.contains('\u{1b}'));
    let output = render(
        &args(&["run", "events", "run-one"]),
        &json!({"items":[],"next_cursor":null,"resume_cursor":"later","cancellation_events":[{"kind":"requested","scope":"all","occurred_at":"now"}]}),
    );
    assert!(output.contains("may repeat independently"));
    assert!(output.contains("requested"));
}

#[test]
fn analysis_log_options_keep_follow_checkpoints_private_and_paging_independent() {
    for options in [
        vec!["--follow", "--limit", "1"],
        vec!["--follow", "--cursor", "c1"],
        vec!["--follow", "--state", "other.json"],
        vec!["--interval", "1"],
        vec!["--deadline", "1"],
        vec!["--allow-gaps"],
    ] {
        assert!(
            Args::try_parse_from(
                ["enumeray", "analysis", "logs", "analysis"]
                    .into_iter()
                    .chain(options)
            )
            .is_err()
        );
    }
    let value = json!({"items":[{"source":"model.solver","position":9,"text":"safe\u{1b}[2J"}],"next_cursor":"page-two","resume_cursor":"resume-nine"});
    let output = render(
        &args(&["analysis", "logs", "private analysis", "--limit", "1"]),
        &value,
    );
    for expected in [
        "Analysis logs",
        "model.solver",
        "analysis logs 'private analysis' --limit 1 --cursor page-two",
        "--cursor resume-nine",
    ] {
        assert!(output.contains(expected), "{expected}: {output}");
    }
    assert!(!output.contains('\u{1b}'));
    let failure = enumeray_client::Error::Api {
        status: 410,
        failure: Box::new(enumeray_client::ApiFailure {
            code: "history_cursor_expired".into(),
            message: "expired".into(),
            request_id: "request".into(),
            details: None,
        }),
        retry_after: None,
    };
    let hint = error_hint(
        &args(&[
            "analysis",
            "logs",
            "private analysis",
            "--limit",
            "1",
            "--cursor",
            "old",
        ]),
        &failure,
    )
    .unwrap();
    assert!(hint.contains("analysis logs 'private analysis' --limit 1"));
    assert!(!hint.lines().last().unwrap().contains("--cursor"));
}

#[test]
fn analysis_follow_resume_round_trips_folder_options_without_exposing_checkpoint_cursor() {
    let args = args(&[
        "--profile",
        "science team",
        "--config",
        "private profiles.json",
        "--api",
        "https://example.org/",
        "--timeout",
        "19",
        "--json",
        "analysis",
        "logs",
        "private analysis",
        "--follow",
        "--interval",
        "3",
        "--deadline",
        "17",
        "--allow-gaps",
    ]);
    let command = follow_resume(&args, Some("private-cursor"));
    assert!(!command.contains("private-cursor"));
    assert!(!command.contains("--state"));
    let printed = Process::new("/bin/sh")
        .args(["-c", &format!("set -- {command}; printf '%s\\000' \"$@\"")])
        .output()
        .unwrap();
    assert!(printed.status.success());
    let words: Vec<_> = printed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|word| !word.is_empty())
        .map(|word| std::str::from_utf8(word).unwrap())
        .collect();
    let resumed = Args::try_parse_from(words).unwrap();
    assert_eq!(resumed.profile, "science team");
    assert_eq!(
        resumed.config.as_deref(),
        Some(std::path::Path::new("private profiles.json"))
    );
    assert_eq!(resumed.api.as_deref(), Some("https://example.org/"));
    assert!(resumed.json);
    assert_eq!(resumed.timeout, 19);
    let Command::Analysis(crate::args::Analysis::Logs {
        directory,
        page,
        follow,
        interval,
        deadline,
        allow_gaps,
    }) = resumed.command
    else {
        panic!("wrong command")
    };
    assert_eq!(directory, std::path::Path::new("private analysis"));
    assert!(page.cursor.is_none() && page.limit.is_none() && follow && allow_gaps);
    assert_eq!((interval, deadline), (Some(3), Some(17)));
}

#[test]
fn analysis_cancellation_distinguishes_local_intent_saved_acknowledgement_and_current_outcome() {
    let args = args(&["analysis", "cancel", "analysis"]);
    let mut value = json!({"directory":"analysis","run_id":"run-one","cancellation":{"attempt":"uncertain","receipt":null,"error_code":"transport_failure","token":"never-print-this"}});
    let output = render(&args, &value);
    assert!(output.contains("Attempt: uncertain"));
    assert!(output.contains("local intent does not establish remote cancellation"));
    assert!(!output.contains("never-print-this"));
    value["cancellation"]["attempt"] = json!("acknowledged");
    value["cancellation"]["receipt"] = json!({"status":"requested","state":"running"});
    value["run"] = json!({"state":"succeeded","cancellation":{"status":"completed"}});
    let output = render(&args, &value);
    for expected in [
        "State: succeeded",
        "Cancellation status: completed",
        "Saved API cancellation acknowledgement",
        "Cancellation status: requested",
        "Run state at acknowledgement: running",
        "Current run inspection is authoritative",
    ] {
        assert!(output.contains(expected), "{expected}: {output}");
    }
    assert!(!output.contains("No saved API acknowledgement"));
}

#[test]
fn next_command_round_trips_profile_paths_filters_and_opaque_cursor() {
    let config = "private dir/config's.json";
    let cursor = "opaque +/&?='$(false);`false`";
    let args = args(&[
        "--profile",
        "science team",
        "--config",
        config,
        "--api",
        "https://example.org/",
        "--timeout",
        "19",
        "run",
        "list",
        "--model",
        "example",
        "--status",
        "queued",
        "--created-after",
        "2026-10-15T12:00:00+03:00",
        "--limit",
        "1",
    ]);
    let output = render(&args, &json!({"items":[],"next_cursor":cursor}));
    let command = output
        .lines()
        .find_map(|line| line.strip_prefix("Next page: "))
        .unwrap();
    let printed = Process::new("/bin/sh")
        .args(["-c", &format!("set -- {command}; printf '%s\\000' \"$@\"")])
        .output()
        .unwrap();
    assert!(printed.status.success());
    let words: Vec<_> = printed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|word| !word.is_empty())
        .map(|word| std::str::from_utf8(word).unwrap())
        .collect();
    assert_eq!(
        &words[..9],
        &[
            "enumeray",
            "--profile",
            "science team",
            "--config",
            config,
            "--api",
            "https://example.org/",
            "--timeout",
            "19"
        ]
    );
    assert_eq!(
        &words[9..],
        &[
            "run",
            "list",
            "--model",
            "example",
            "--status",
            "queued",
            "--created-after",
            "2026-10-15T12:00:00+03:00",
            "--limit",
            "1",
            "--cursor",
            cursor
        ]
    );
    let reparsed = Args::try_parse_from(words).unwrap();
    if let Command::Run(crate::args::Run::List { page, .. }) = reparsed.command {
        assert_eq!(page.cursor.as_deref(), Some(cursor));
    } else {
        panic!("wrong command");
    }
}

#[test]
fn shell_words_escape_controls_without_changing_the_argument() {
    let word = "a'\n\u{1b}\u{202e}$(false)\\";
    let quoted = shell_word(word);
    assert!(!quoted.chars().any(char::is_control));
    assert!(!quoted.contains('\u{202e}'));
    let printed = Process::new("/bin/bash")
        .args(["-f", "-c", &format!("printf '%s' {quoted}")])
        .output()
        .unwrap();
    assert!(printed.status.success());
    assert_eq!(printed.stdout, word.as_bytes());
}

#[test]
fn raw_api_keeps_pretty_json_and_missing_fields_never_panic() {
    let value = json!({"future_field":{"anything":[1,2]}});
    assert_eq!(
        render(&args(&["api", "GET", "/api/v1/models"]), &value),
        serde_json::to_string_pretty(&value).unwrap()
    );
    for command in [
        vec!["service", "status"],
        vec!["connector", "get", "worker"],
        vec!["connector", "member", "list", "worker"],
        vec!["artifact", "get", "artifact"],
        vec!["model", "publication", "example", "one"],
        vec!["run", "provenance", "run-one"],
    ] {
        let output = render(&args(&command), &json!({"token":"never-print-this"}));
        assert!(output.contains("unknown / not disclosed"));
        assert!(!output.contains("never-print-this"));
    }
}

#[test]
fn tables_shorten_prose_without_shortening_identifiers_or_detail_text() {
    let id = "a".repeat(64);
    let prose = "Long scientific description λ. ".repeat(100);
    let output = render(
        &args(&["model", "list"]),
        &json!({"items":[{"model_id":id,"name":"Example","description":prose}],"next_cursor":null}),
    );
    assert!(output.contains(&id));
    assert!(!output.contains(&prose));
    assert!(output.contains("use --json for complete rows"));
    let detail = render(
        &args(&["model", "get", &id]),
        &json!({"model_id":id,"description":prose}),
    );
    assert!(detail.contains(&prose));
}

#[test]
fn artifact_details_use_nested_description_provenance_and_verified_content() {
    let output = render(
        &args(&["artifact", "get", "full-artifact-id"]),
        &json!({
            "artifact_id":"full-artifact-id",
            "description":{"logical_name":"processed_dataset","media_type":"text/csv","scientific_metadata":{}},
            "provenance":{"kind":"run_output","run_id":"full-producer-id"},
            "availability":"available",
            "content":{"size_bytes":42,"checksum":{"value":"full-sha256-digest"}}
        }),
    );
    for expected in [
        "Artifact: full-artifact-id",
        "Name: processed_dataset",
        "Media type: text/csv",
        "Producing run: full-producer-id",
        "Verified bytes: 42",
        "SHA-256: full-sha256-digest",
    ] {
        assert!(output.contains(expected), "{expected}: {output}");
    }
}
