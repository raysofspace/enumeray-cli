use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

fn cli(config: &Path) -> Command {
    let mut command = human_cli(config);
    command.arg("--json");
    command
}

fn human_cli(config: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_enumeray"));
    command
        .arg("--config")
        .arg(config)
        .env_remove("ENUMERAY_TOKEN");
    command
}

fn server(body: Value) -> (String, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let worker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut byte = [0];
        while !bytes.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            bytes.push(byte[0]);
        }
        let headers = String::from_utf8(bytes.clone()).unwrap();
        let length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .and_then(|value| value.trim().parse::<usize>().ok())
            })
            .unwrap_or(0);
        let mut request_body = vec![0; length];
        stream.read_exact(&mut request_body).unwrap();
        bytes.extend(request_body);
        let body = body.to_string();
        write!(stream,"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",body.len()).unwrap();
        String::from_utf8(bytes).unwrap()
    });
    (url, worker)
}

struct PolicyServer {
    origin: String,
    stop: mpsc::Sender<()>,
    worker: thread::JoinHandle<Vec<String>>,
}

impl PolicyServer {
    fn new(responses: Vec<(u16, Value)>) -> Self {
        Self::with_delay(responses, Duration::ZERO)
    }

    fn with_delay(responses: Vec<(u16, Value)>, response_delay: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let (stop, stopping) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut responses = VecDeque::from(responses);
            let mut requests = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if stopping.try_recv().is_ok() || Instant::now() >= deadline {
                    break;
                }
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("accept policy request: {error}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut byte = [0];
                while !bytes.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    bytes.push(byte[0]);
                }
                let headers = String::from_utf8(bytes.clone()).unwrap();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                let mut body = vec![0; length];
                stream.read_exact(&mut body).unwrap();
                bytes.extend(body);
                requests.push(String::from_utf8(bytes).unwrap());
                let (status, body) = responses.pop_front().unwrap_or_else(|| {
                    (500, json!({"code":"unexpected_request","message":"No further request was expected.","request_id":"unexpected"}))
                });
                let body = body.to_string();
                thread::sleep(response_delay);
                // Deadline tests intentionally disconnect before a delayed response is written.
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} Test\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
            }
            assert!(
                responses.is_empty(),
                "not all scripted policy requests were sent"
            );
            requests
        });
        Self {
            origin,
            stop,
            worker,
        }
    }

    fn finish(self) -> Vec<String> {
        self.stop.send(()).unwrap();
        self.worker.join().unwrap()
    }
}

#[test]
fn connector_lifecycle_commands_preview_and_preserve_the_other_control() {
    for (command, field, value, endpoint) in [
        ("maintenance", "draining", true, "state"),
        ("resume", "draining", false, "state"),
        (
            "withdraw-authority",
            "authority_withdrawn",
            true,
            "authority",
        ),
        (
            "restore-authority",
            "authority_withdrawn",
            false,
            "authority",
        ),
    ] {
        for other in [false, true] {
            let mut observed = connector_status(!value, !value);
            let other_field = if field == "draining" {
                "authority_withdrawn"
            } else {
                "draining"
            };
            observed[other_field] = json!(other);
            observed["maintenance_complete"] = json!(observed["draining"] == true);
            let mut proposed = json!({"draining":observed["draining"],"authority_withdrawn":observed["authority_withdrawn"]});
            proposed[field] = json!(value);
            let receipt = json!({
                "connector_id":"worker","generation":8,"changed":true,
                "draining":proposed["draining"],"authority_withdrawn":proposed["authority_withdrawn"],
                "work":observed["work"],"maintenance_complete":proposed["draining"] == true,
            });
            for dry_run in [true, false] {
                let directory = tempfile::tempdir().unwrap();
                let mut replies = vec![(200, observed.clone())];
                if !dry_run {
                    replies.push((200, receipt.clone()));
                }
                let server = PolicyServer::new(replies);
                let mut process = cli(&directory.path().join("missing.json"));
                process.args([
                    "--api",
                    &server.origin,
                    "connector",
                    command,
                    "worker",
                    "--generation",
                    "7",
                ]);
                if dry_run {
                    process.arg("--dry-run");
                }
                let output = process.output().unwrap();
                let requests = server.finish();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert!(output.stderr.is_empty());
                let actual: Value = serde_json::from_slice(&output.stdout).unwrap();
                if dry_run {
                    assert_eq!(actual["observed"], observed);
                    assert_eq!(actual["proposed"], proposed);
                    assert!(!actual["effects"].as_str().unwrap().is_empty());
                    if command == "restore-authority" {
                        assert!(actual["effects"].as_str().unwrap().contains(
                            "Cancellation already requested or confirmed remains in force."
                        ));
                    }
                    assert_eq!(requests.len(), 1);
                } else {
                    assert_eq!(actual, receipt);
                    assert_eq!(requests.len(), 2);
                    assert!(requests[1].starts_with(&format!(
                        "PUT /api/v1/connectors/worker/{endpoint} HTTP/1.1\r\n"
                    )));
                    let key = if endpoint == "state" {
                        "draining"
                    } else {
                        "withdrawn"
                    };
                    let mut body = json!({"expected_generation":7});
                    body[key] = json!(value);
                    assert_eq!(
                        serde_json::from_str::<Value>(
                            requests[1].split_once("\r\n\r\n").unwrap().1
                        )
                        .unwrap(),
                        body
                    );
                }
                assert!(
                    requests[0]
                        .starts_with("GET /api/v1/connectors/worker/diagnostics HTTP/1.1\r\n")
                );
            }
        }
    }
}

fn connector_status(draining: bool, authority_withdrawn: bool) -> Value {
    json!({
        "connector_id":"worker","owner_id":"11111111-1111-4111-8111-111111111111","ownership_generation":1,"archived_at":null,"generation":7,"observed_generation":6,"configuration_pending":true,
        "draining":draining,"authority_withdrawn":authority_withdrawn,
        "work":{"retained_queued_runs":12,"unfinished_assignments":0,"pending_delivery_assignments":0,
            "pending_uploads":0,"pending_log_archives":0,"unconfirmed_assignments":0,
            "assignment_phases":{"preparing":0,"waiting_for_backend":0,"executing":0,"collecting_results":0,"uploading_results":0}},
        "maintenance_complete":draining,"failed_publications":0,"configuration_capacity":{"desired_bytes":500,"maximum_desired_bytes":2097152,"maximum_retained_publications":1024,"maximum_connector_cache_bytes":4194304,"counts":{"historical_publications":0,"retained_publications":0,"retirement_requested":0,"retired_publications":0}},
    })
}

fn context_lifecycle_status(state: &str) -> Value {
    let mut work = connector_status(false, false)["work"].clone();
    work["retained_queued_runs"] = json!(0);
    let mut sibling_work = work.clone();
    sibling_work["retained_queued_runs"] = json!(12);
    json!({"id":"source","connector_id":"worker","generation":7,
        "state":state,"replacement_context_id":null,"accepted_work":null,"work":work,
        "live_current_publications":0,"retained_publications":0,"pending_configuration_retirements":0,
        "configuration_removal_generation":9,"connector_generation":9,"observed_generation":9,
        "retirement_ready":state == "draining","connector_work":sibling_work,
        "local_cleanup_state":"requires_local_inspection"})
}

fn context_replacement_plan(context: &Value) -> Value {
    json!({"context":context,"replacement":{"id":"next","connector_id":"new-worker","state":"active","generation":1,"driver":"process","connection":"direct","metadata":{"location":"Lab","disclosure":"Lab data access."}},
        "dependencies":{"items":[{"id":"old-publication","model_id":"example","model_owner_id":"22222222-2222-4222-8222-222222222222","grant_id":"33333333-3333-4333-8333-333333333333","grant_holder_user_id":"22222222-2222-4222-8222-222222222222","status":"published","current":false,"model_archived":false,"revoked_at":null,"installation_revoked_at":null,"configuration_retention":{"state":"retained","requested_at":null,"retired_at":null,"retirement_blocked":false,"blockers":{"queued_runs":0,"running_runs":0,"pending_delivery_assignments":0,"pending_log_archives":0,"pending_assignment_uploads":0}}}],"next_cursor":"a+b/c?="},
        "actions":[{"role":"model_author","action":"register_validate_review_activate"}]})
}

#[test]
fn context_lifecycle_previews_and_mutations_keep_original_requests() {
    for (command, endpoint, policy) in [
        ("context-cutover", "cutover", Some("drain")),
        ("context-cutover", "cutover", Some("cancel-queued")),
        ("context-reopen", "reopen", None),
        (
            "context-release-configurations",
            "configuration-retirements",
            None,
        ),
        ("context-retire", "retirement", None),
    ] {
        for dry_run in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let observed = context_lifecycle_status(if policy.is_some() {
                "active"
            } else {
                "draining"
            });
            let mut changed = observed.clone();
            changed["generation"] = json!(8);
            let receipt = json!({"operation":endpoint,"context":changed,"cancelled_queued_runs":0,"requested_publications":0});
            let mut responses = vec![(200, observed.clone())];
            if dry_run && policy.is_some() {
                responses.push((200, context_replacement_plan(&observed)));
            } else if !dry_run {
                responses.push((200, receipt.clone()));
            }
            let server = PolicyServer::new(responses);
            let mut process = cli(&directory.path().join("missing.json"));
            process.args([
                "--api",
                &server.origin,
                "connector",
                "context",
                command.strip_prefix("context-").unwrap(),
                "worker",
                "source",
                "--generation",
                "7",
            ]);
            if let Some(policy) = policy {
                process.args(["--accepted-work", policy, "--replacement", "next"]);
            }
            if dry_run {
                process.arg("--dry-run");
            }
            let output = process.output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(output.stderr.is_empty());
            let result: Value = serde_json::from_slice(&output.stdout).unwrap();
            let expected_request = if let Some(policy) = policy {
                json!({"expected_generation":7,"replacement_context_id":"next","accepted_work":policy.replace('-', "_")})
            } else {
                json!({"expected_generation":7})
            };
            let requests = server.finish();
            assert!(
                requests[0]
                    .starts_with("GET /api/v1/connectors/worker/contexts/source HTTP/1.1\r\n")
            );
            if dry_run {
                assert_eq!(result["request"], expected_request);
                assert_eq!(result["observed"], observed);
                assert_eq!(result["dry_run"], true);
                assert!(requests.iter().all(|request| request.starts_with("GET ")));
            } else {
                assert_eq!(result, receipt);
                assert!(requests[1].starts_with(&format!(
                    "PUT /api/v1/connectors/worker/contexts/source/{endpoint} HTTP/1.1\r\n"
                )));
                assert_eq!(
                    serde_json::from_str::<Value>(requests[1].split_once("\r\n\r\n").unwrap().1)
                        .unwrap(),
                    expected_request
                );
            }
        }
    }
}

#[test]
fn context_mutation_replays_original_generation_and_never_retries_conflicts() {
    for replay in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let mut observed = context_lifecycle_status("draining");
        observed["generation"] = json!(8);
        let response = if replay {
            (
                200,
                json!({"operation":"cutover","context":observed,"cancelled_queued_runs":4,"requested_publications":0}),
            )
        } else {
            (
                409,
                json!({"code":"management_conflict","message":"Different request","request_id":"cutover"}),
            )
        };
        let server = PolicyServer::new(vec![(200, observed), response.clone()]);
        let output = cli(&directory.path().join("missing.json"))
            .args([
                "--api",
                &server.origin,
                "connector",
                "context",
                "cutover",
                "worker",
                "source",
                "--generation",
                "7",
                "--accepted-work",
                "cancel-queued",
            ])
            .output()
            .unwrap();
        assert_eq!(output.status.success(), replay);
        let requests = server.finish();
        assert_eq!(requests.len(), 2);
        let request: Value =
            serde_json::from_str(requests[1].split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(request["expected_generation"], 7);
        if replay {
            assert_eq!(
                serde_json::from_slice::<Value>(&output.stdout).unwrap(),
                response.1
            );
        }
    }
}

#[test]
fn context_replacement_plan_encodes_filters_and_preserves_them_in_human_continuation() {
    for human in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let plan = context_replacement_plan(&context_lifecycle_status("active"));
        let server = PolicyServer::new(vec![(200, plan.clone())]);
        let mut process = if human {
            human_cli(&directory.path().join("missing.json"))
        } else {
            cli(&directory.path().join("missing.json"))
        };
        let output = process
            .args([
                "--api",
                &server.origin,
                "connector",
                "context",
                "replacement-plan",
                "worker",
                "source",
                "--replacement",
                "next/other?",
                "--section",
                "runs",
                "--limit",
                "12",
                "--cursor",
                "a+b/c?=",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if human {
            let text = String::from_utf8(output.stdout).unwrap();
            for snippet in [
                "Context retirement ready: no",
                "Whole connector retained work",
                "model_author",
                "Next page:",
                "--section runs",
                "--replacement",
                "--limit 12",
                "--cursor",
            ] {
                assert!(text.contains(snippet), "missing {snippet}: {text}");
            }
        } else {
            assert_eq!(
                serde_json::from_slice::<Value>(&output.stdout).unwrap(),
                plan
            );
        }
        let requests = server.finish();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("GET /api/v1/connectors/worker/contexts/source/replacement-plan?section=runs&limit=12&replacement_context_id=next%2Fother%3F&after=a%2Bb%2Fc%3F%3D HTTP/1.1\r\n"));
    }
}

#[test]
fn context_dry_run_rejects_stale_or_inconsistent_inspection_without_mutation() {
    let observed = context_lifecycle_status("draining");
    let mut stale = observed.clone();
    stale["generation"] = json!(8);
    let mut negative = observed.clone();
    negative["work"]["pending_uploads"] = json!(-1);
    let mut unfinished = observed.clone();
    unfinished["work"]["unfinished_assignments"] = json!(1);
    let mut missing_ack = observed.clone();
    missing_ack["observed_generation"] = json!(8);
    let mut missing_removal = observed.clone();
    missing_removal["configuration_removal_generation"] = Value::Null;
    let mut other = observed.clone();
    other["id"] = json!("other");
    for response in [
        stale,
        negative,
        unfinished,
        missing_ack,
        missing_removal,
        other,
        Value::Null,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let server = PolicyServer::new(vec![(200, response)]);
        let output = cli(&directory.path().join("missing.json"))
            .args([
                "--api",
                &server.origin,
                "connector",
                "context",
                "retire",
                "worker",
                "source",
                "--generation",
                "7",
                "--dry-run",
            ])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert_eq!(server.finish().len(), 1);
    }
}

#[test]
fn context_retirement_waits_for_each_context_obligation_but_excludes_sibling_work() {
    for blocker in [
        "retained_queued_runs",
        "unfinished_assignments",
        "pending_delivery_assignments",
        "pending_uploads",
        "pending_log_archives",
        "configuration_ack",
        "configuration_retirement",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let complete = context_lifecycle_status("draining");
        let mut pending = complete.clone();
        pending["retirement_ready"] = json!(false);
        match blocker {
            "configuration_ack" => pending["observed_generation"] = json!(8),
            "configuration_retirement" => {
                pending["retained_publications"] = json!(1);
                pending["pending_configuration_retirements"] = json!(1);
            }
            _ => pending["work"][blocker] = json!(1),
        }
        let server = PolicyServer::new(vec![(200, pending), (200, complete.clone())]);
        let output = cli(&directory.path().join("missing.json"))
            .args([
                "--api",
                &server.origin,
                "connector",
                "context",
                "wait-retirement",
                "worker",
                "source",
                "--generation",
                "7",
                "--interval",
                "1",
                "--deadline",
                "3",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{blocker}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap(),
            complete
        );
        let requests = server.finish();
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| request.starts_with("GET ")));
    }
}

#[test]
fn context_retirement_wait_stops_on_generation_change_reopening_and_deadline() {
    for mode in ["changed", "reopened", "timeout"] {
        let directory = tempfile::tempdir().unwrap();
        let mut observed = context_lifecycle_status("draining");
        match mode {
            "changed" => observed["generation"] = json!(8),
            "reopened" => {
                observed["state"] = json!("active");
                observed["retirement_ready"] = json!(false);
            }
            _ => {
                observed["retirement_ready"] = json!(false);
                observed["work"]["pending_uploads"] = json!(1);
            }
        }
        let server = PolicyServer::new(vec![(200, observed)]);
        let output = cli(&directory.path().join("missing.json"))
            .args([
                "--api",
                &server.origin,
                "connector",
                "context",
                "wait-retirement",
                "worker",
                "source",
                "--generation",
                "7",
                "--interval",
                "1",
                "--deadline",
                "1",
            ])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert_eq!(server.finish().len(), 1);
    }
}

#[test]
fn connector_lifecycle_human_summary_explains_effects_and_affected_work() {
    let directory = tempfile::tempdir().unwrap();
    let observed = connector_status(false, false);
    let receipt = json!({"connector_id":"worker","owner_id":"11111111-1111-4111-8111-111111111111","ownership_generation":1,"archived_at":null,"generation":8,"draining":true,"authority_withdrawn":false,"changed":true,"work":observed["work"],"maintenance_complete":true});
    let server = PolicyServer::new(vec![(200, observed), (200, receipt.clone())]);
    let output = human_cli(&directory.path().join("missing.json"))
        .args([
            "--api",
            &server.origin,
            "connector",
            "maintenance",
            "worker",
            "--generation",
            "7",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Connector: worker"));
    assert!(stdout.contains("Generation: 8"));
    assert!(stdout.contains("Maintenance: yes"));
    assert!(stdout.contains("Authority withdrawn: no"));
    let summary = String::from_utf8(output.stderr).unwrap();
    assert!(summary.contains("maintenance false -> true; authority withdrawn false -> false"));
    assert!(summary.contains("Stop new acceptance and claims; retain queued work for resume"));
    assert!(summary.contains("retained queued 12; unfinished assignments 0"));
    assert_eq!(server.finish().len(), 2);
}

#[test]
fn connector_lifecycle_stale_unreadable_or_unauthorized_inspection_never_mutates() {
    let original = connector_status(false, false);
    let mut stale = original.clone();
    stale["generation"] = json!(8);
    let mut missing = original.clone();
    missing.as_object_mut().unwrap().remove("work");
    let mut negative = original.clone();
    negative["work"]["unfinished_assignments"] = json!(-1);
    let mut unconfirmed_without_assignment = original.clone();
    unconfirmed_without_assignment["work"]["unconfirmed_assignments"] = json!(1);
    let mut inconsistent = original.clone();
    inconsistent["maintenance_complete"] = json!(true);
    let mut other = original.clone();
    other["connector_id"] = json!("other");
    for (status, observed) in [
        (200, stale),
        (200, missing),
        (200, negative),
        (200, unconfirmed_without_assignment),
        (200, inconsistent),
        (200, other),
        (
            403,
            json!({"code":"permission_denied","message":"Denied","request_id":"inspection"}),
        ),
    ] {
        for dry_run in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let server = PolicyServer::new(vec![(status, observed.clone())]);
            let mut process = cli(&directory.path().join("missing.json"));
            process.args([
                "--api",
                &server.origin,
                "connector",
                "maintenance",
                "worker",
                "--generation",
                "7",
            ]);
            if dry_run {
                process.arg("--dry-run");
            }
            let output = process.output().unwrap();
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            assert_eq!(server.finish().len(), 1);
        }
    }
}

#[test]
fn connector_lifecycle_conflicts_and_uncertain_receipts_are_not_retried() {
    for reply in [
        (
            409,
            json!({"code":"management_conflict","message":"Changed","request_id":"state-put"}),
        ),
        (200, Value::Null),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let server = PolicyServer::new(vec![(200, connector_status(false, false)), reply]);
        let output = cli(&directory.path().join("missing.json"))
            .args([
                "--api",
                &server.origin,
                "connector",
                "withdraw-authority",
                "worker",
                "--generation",
                "7",
            ])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert_eq!(server.finish().len(), 2);
    }
}

#[test]
fn maintenance_wait_excludes_retained_queue_and_blocks_for_each_delivery_obligation() {
    for blocker in [
        "unfinished_assignments",
        "pending_delivery_assignments",
        "pending_uploads",
        "pending_log_archives",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let completed = connector_status(true, false);
        let mut pending = completed.clone();
        pending["work"][blocker] = json!(1);
        pending["maintenance_complete"] = json!(false);
        if blocker == "unfinished_assignments" {
            pending["work"]["assignment_phases"]["waiting_for_backend"] = json!(1);
            pending["work"]["unconfirmed_assignments"] = json!(1);
        }
        let server = PolicyServer::new(vec![(200, pending), (200, completed.clone())]);
        let output = cli(&directory.path().join("missing.json"))
            .args([
                "--api",
                &server.origin,
                "connector",
                "wait-maintenance",
                "worker",
                "--generation",
                "7",
                "--interval",
                "1",
                "--deadline",
                "3",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap(),
            completed
        );
        let requests = server.finish();
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| request.starts_with("GET ")));
    }
}

#[test]
fn maintenance_wait_stops_on_resume_or_generation_changes_and_preserves_state_on_timeout() {
    for resumed in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let mut observed = connector_status(!resumed, false);
        if !resumed {
            observed["generation"] = json!(8);
        }
        let server = PolicyServer::new(vec![(200, observed)]);
        let output = cli(&directory.path().join("missing.json"))
            .args([
                "--api",
                &server.origin,
                "connector",
                "wait-maintenance",
                "worker",
                "--generation",
                "7",
            ])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert_eq!(server.finish().len(), 1);
    }
    let directory = tempfile::tempdir().unwrap();
    let mut pending = connector_status(true, false);
    pending["work"]["pending_delivery_assignments"] = json!(1);
    pending["maintenance_complete"] = json!(false);
    let server = PolicyServer::new(vec![(200, pending)]);
    let output = cli(&directory.path().join("missing.json"))
        .args([
            "--api",
            &server.origin,
            "connector",
            "wait-maintenance",
            "worker",
            "--generation",
            "7",
            "--interval",
            "1",
            "--deadline",
            "1",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("maintenance remains enabled")
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].starts_with("GET "));
}

#[test]
fn maintenance_wait_rejects_unconfirmed_work_without_an_unfinished_assignment() {
    for maintenance_complete in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let mut inconsistent = connector_status(true, false);
        inconsistent["work"]["unconfirmed_assignments"] = json!(1);
        inconsistent["maintenance_complete"] = json!(maintenance_complete);
        let server = PolicyServer::new(vec![(200, inconsistent)]);
        let output = cli(&directory.path().join("missing.json"))
            .args([
                "--api",
                &server.origin,
                "connector",
                "wait-maintenance",
                "worker",
                "--generation",
                "7",
                "--interval",
                "1",
                "--deadline",
                "2",
            ])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let failure: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(failure["error"]["code"], "client_error");
        assert!(
            failure["error"]["message"]
                .as_str()
                .unwrap()
                .contains("inconsistent maintenance evidence")
        );
        assert_eq!(server.finish().len(), 1);
    }
}

#[test]
fn maintenance_wait_does_not_accept_a_completed_response_after_its_deadline() {
    let directory = tempfile::tempdir().unwrap();
    let server = PolicyServer::with_delay(
        vec![(200, connector_status(true, false))],
        Duration::from_millis(2500),
    );
    let started = Instant::now();
    let output = cli(&directory.path().join("missing.json"))
        .args([
            "--api",
            &server.origin,
            "--timeout",
            "5",
            "connector",
            "wait-maintenance",
            "worker",
            "--generation",
            "7",
            "--deadline",
            "1",
        ])
        .output()
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "HTTP request exceeded the local observation deadline"
    );
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let failure: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(failure["error"]["code"], "client_error");
    assert!(
        failure["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Maintenance wait deadline elapsed")
    );
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].starts_with("GET "));
}

#[test]
fn policy_dry_run_shows_observed_and_resolved_changes_without_mutation() {
    for (before_mode, before_visibility, mode, visibility) in [
        ("invite", "unlisted", "open", None),
        ("open", "listed", "invite", None),
        ("invite", "unlisted", "request", Some("listed")),
        ("request", "listed", "invite", Some("unlisted")),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let observed = json!({"mode":before_mode,"visibility":before_visibility,"generation":7});
        let server = PolicyServer::new(vec![(200, observed.clone())]);
        let mut command = cli(&directory.path().join("missing.json"));
        command.args([
            "--api",
            &server.origin,
            "--timeout",
            "5",
            "access",
            "set-policy",
            "example",
            "--mode",
            mode,
            "--generation",
            "7",
            "--dry-run",
        ]);
        if let Some(visibility) = visibility {
            command.args(["--visibility", visibility]);
        }
        let output = command.output().unwrap();
        let requests = server.finish();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap(),
            json!({
                "model_id":"example",
                "observed":observed,
                "proposed":{"mode":mode,"visibility":visibility.unwrap_or(before_visibility)},
            })
        );
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("GET /api/v1/models/example/access-policy HTTP/1.1\r\n"));
    }
}

#[test]
fn policy_application_keeps_optional_visibility_and_the_supplied_generation() {
    for (before_visibility, mode, visibility) in [
        ("unlisted", "open", None),
        ("listed", "invite", None),
        ("unlisted", "request", Some("listed")),
        ("listed", "invite", Some("unlisted")),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let result = json!({"mode":mode,"visibility":visibility.unwrap_or(before_visibility),"generation":8});
        let server = PolicyServer::new(vec![
            (
                200,
                json!({"mode":"request","visibility":before_visibility,"generation":7}),
            ),
            (200, result.clone()),
        ]);
        let mut command = cli(&directory.path().join("missing.json"));
        command.args([
            "--api",
            &server.origin,
            "--timeout",
            "5",
            "access",
            "set-policy",
            "example",
            "--mode",
            mode,
            "--generation",
            "7",
        ]);
        if let Some(visibility) = visibility {
            command.args(["--visibility", visibility]);
        }
        let output = command.output().unwrap();
        let requests = server.finish();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap(),
            result
        );
        assert_eq!(requests.len(), 2);
        assert!(requests[0].starts_with("GET /api/v1/models/example/access-policy HTTP/1.1\r\n"));
        assert!(requests[1].starts_with("PUT /api/v1/models/example/access-policy HTTP/1.1\r\n"));
        assert_eq!(
            serde_json::from_str::<Value>(requests[1].split_once("\r\n\r\n").unwrap().1).unwrap(),
            json!({"mode":mode,"visibility":visibility,"expected_generation":7})
        );
    }
}

#[test]
fn policy_application_displays_the_preview_and_human_receipt() {
    let directory = tempfile::tempdir().unwrap();
    let result = json!({"mode":"open","visibility":"unlisted","generation":8});
    let server = PolicyServer::new(vec![
        (
            200,
            json!({"mode":"invite","visibility":"unlisted","generation":7}),
        ),
        (200, result.clone()),
    ]);
    let output = human_cli(&directory.path().join("missing.json"))
        .args([
            "--api",
            &server.origin,
            "--timeout",
            "5",
            "access",
            "set-policy",
            "example",
            "--mode",
            "open",
            "--generation",
            "7",
        ])
        .output()
        .unwrap();
    let requests = server.finish();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Model: example"));
    assert!(stdout.contains("Mode: open"));
    assert!(stdout.contains("Visibility: unlisted"));
    assert!(stdout.contains("Generation: 8"));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "Model example, generation 7: mode invite -> open; visibility unlisted -> unlisted.\n"
    );
    assert_eq!(requests.len(), 2);
}

#[test]
fn policy_dry_run_uses_normal_stdout_rendering_without_a_stderr_summary() {
    let directory = tempfile::tempdir().unwrap();
    let server = PolicyServer::new(vec![(
        200,
        json!({"mode":"open","visibility":"listed","generation":7}),
    )]);
    let output = human_cli(&directory.path().join("missing.json"))
        .args([
            "--api",
            &server.origin,
            "--timeout",
            "5",
            "access",
            "set-policy",
            "example",
            "--mode",
            "request",
            "--generation",
            "7",
            "--dry-run",
        ])
        .output()
        .unwrap();
    let requests = server.finish();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("no change applied"));
    assert!(stdout.contains("Observed generation: 7"));
    assert!(stdout.contains("Observed mode: open"));
    assert!(stdout.contains("Proposed mode: request"));
    assert!(stdout.contains("Proposed visibility: listed"));
    assert_eq!(requests.len(), 1);
}

#[test]
fn stale_policy_generation_stops_both_preview_and_application_without_a_put() {
    for dry_run in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let server = PolicyServer::new(vec![(
            200,
            json!({"mode":"invite","visibility":"unlisted","generation":8}),
        )]);
        let mut command = cli(&directory.path().join("missing.json"));
        command.args([
            "--api",
            &server.origin,
            "--timeout",
            "5",
            "access",
            "set-policy",
            "example",
            "--mode",
            "open",
            "--generation",
            "7",
        ]);
        if dry_run {
            command.arg("--dry-run");
        }
        let output = command.output().unwrap();
        let requests = server.finish();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let failure: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(failure["error"]["code"], "client_error");
        assert!(
            failure["error"]["message"]
                .as_str()
                .unwrap()
                .contains("expected generation 7, observed 8")
        );
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("GET /api/v1/models/example/access-policy "));
    }
}

#[test]
fn unreadable_or_unauthorized_policy_inspection_never_sends_a_put() {
    for (status, observed) in [
        (
            200,
            json!({"mode":"unknown","visibility":"unlisted","generation":7}),
        ),
        (
            200,
            json!({"mode":"invite","visibility":"unknown","generation":7}),
        ),
        (200, json!({"mode":"invite","generation":7})),
        (200, json!({"mode":"invite","visibility":"unlisted"})),
        (
            200,
            json!({"mode":"invite","visibility":"unlisted","generation":"7"}),
        ),
        (
            200,
            json!({"mode":"invite","visibility":null,"generation":7}),
        ),
        (
            403,
            json!({"code":"permission_denied","message":"Policy inspection is not authorized.","request_id":"policy-get"}),
        ),
    ] {
        for dry_run in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let server = PolicyServer::new(vec![(status, observed.clone())]);
            let mut command = cli(&directory.path().join("missing.json"));
            command.args([
                "--api",
                &server.origin,
                "--timeout",
                "5",
                "access",
                "set-policy",
                "example",
                "--mode",
                "open",
                "--visibility",
                "listed",
                "--generation",
                "7",
            ]);
            if dry_run {
                command.arg("--dry-run");
            }
            let output = command.output().unwrap();
            let requests = server.finish();
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            let failure: Value = serde_json::from_slice(&output.stderr).unwrap();
            if status == 403 {
                assert_eq!(failure["status"], 403);
                assert_eq!(failure["error"], observed);
            } else {
                assert_eq!(failure["error"]["code"], "client_error");
            }
            assert_eq!(requests.len(), 1);
            assert!(requests[0].starts_with("GET /api/v1/models/example/access-policy "));
        }
    }
}

#[test]
fn concurrent_policy_edits_keep_the_generation_guard_and_do_not_retry() {
    let directory = tempfile::tempdir().unwrap();
    let conflict = json!({"code":"management_conflict","message":"The resource changed. Inspect the current policy.","request_id":"policy-put"});
    let server = PolicyServer::new(vec![
        (
            200,
            json!({"mode":"invite","visibility":"unlisted","generation":7}),
        ),
        (409, conflict.clone()),
    ]);
    let output = cli(&directory.path().join("missing.json"))
        .args([
            "--api",
            &server.origin,
            "--timeout",
            "5",
            "access",
            "set-policy",
            "example",
            "--mode",
            "open",
            "--generation",
            "7",
        ])
        .output()
        .unwrap();
    let requests = server.finish();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let failure: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(failure["status"], 409);
    assert_eq!(failure["error"], conflict);
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("GET /api/v1/models/example/access-policy "));
    assert!(requests[1].starts_with("PUT /api/v1/models/example/access-policy "));
    assert_eq!(
        serde_json::from_str::<Value>(requests[1].split_once("\r\n\r\n").unwrap().1).unwrap(),
        json!({"mode":"open","visibility":null,"expected_generation":7})
    );
}

#[test]
fn help_exposes_each_self_service_role_without_configuration() {
    let directory = tempfile::tempdir().unwrap();
    for family in [
        "service",
        "auth",
        "connector",
        "model",
        "access",
        "run",
        "artifact",
        "collection",
        "analysis",
    ] {
        let output = cli(&directory.path().join("missing.json"))
            .args([family, "--help"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn run_help_retains_investigation_without_execution_submission_commands() {
    let directory = tempfile::tempdir().unwrap();
    let output = human_cli(&directory.path().join("missing.json"))
        .args(["run", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let help = String::from_utf8(output.stdout).unwrap();
    for command in [
        "collections",
        "list",
        "get",
        "wait",
        "log-archive",
        "logs",
        "events",
        "provenance",
        "artifacts",
        "cancel",
    ] {
        assert!(
            help.lines().any(|line| {
                line.strip_prefix("  ")
                    .and_then(|line| line.split_whitespace().next())
                    == Some(command)
            }),
            "Missing retained command {command}: {help}"
        );
    }
    for removed in ["prepare", "submit"] {
        assert!(
            !help.lines().any(|line| {
                line.strip_prefix("  ")
                    .and_then(|line| line.split_whitespace().next())
                    == Some(removed)
            }),
            "Removed command {removed} remains in help: {help}"
        );
    }
}

#[test]
fn removed_run_commands_fail_during_parsing_without_files_or_http() {
    let directory = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    let profiles = directory.path().join("profiles.json");
    let inputs = directory.path().join("inputs.json");
    let prepared = directory.path().join("original-prepared.json");
    std::fs::write(
        &profiles,
        json!({"profiles":{"default":{"api_url":origin,"access_token":"saved-test-token"}}})
            .to_string(),
    )
    .unwrap();
    std::fs::write(&inputs, br#"{"value":2}"#).unwrap();
    std::fs::write(
        &prepared,
        json!({"schema_version":1,"api_url":origin,"user_id":uuid::Uuid::new_v4(),
            "idempotency_key":uuid::Uuid::new_v4().to_string(),
            "request":{"model_id":"model","expected_publication_id":"original","inputs":{"value":2}}})
        .to_string(),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [&profiles, &prepared] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
    let snapshot = || {
        let mut files = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (entry.file_name(), std::fs::read(entry.path()).unwrap())
            })
            .collect::<Vec<_>>();
        files.sort();
        files
    };
    let original = snapshot();
    for json in [false, true] {
        for args in [
            vec![
                "run",
                "prepare",
                "model",
                "--inputs",
                "inputs.json",
                "--output",
                "new-output/prepared.json",
            ],
            vec![
                "run",
                "prepare",
                "model",
                "--candidate",
                "candidate",
                "--inputs",
                "inputs.json",
                "--output",
                "new-output/trial.json",
            ],
            vec!["run", "submit", "--prepared", "original-prepared.json"],
            vec!["run", "prepare", "--help"],
            vec!["run", "submit", "--help"],
        ] {
            let mut command = human_cli(&profiles);
            command
                .current_dir(directory.path())
                .args(["--timeout", "1"]);
            if json {
                command.arg("--json");
            }
            let output = command.args(&args).output().unwrap();
            let error = String::from_utf8_lossy(&output.stderr);
            assert_eq!(output.status.code(), Some(2), "{args:?}: {error}");
            assert!(output.stdout.is_empty(), "{args:?}");
            assert!(
                error.contains(&format!("unrecognized subcommand '{}'", args[1])),
                "{args:?}: {error}"
            );
            assert!(!error.contains("saved-test-token"));
            assert_eq!(snapshot(), original, "{args:?}");
            assert!(matches!(
                listener.accept(),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
            ));
        }
    }
}

#[test]
fn rotated_refresh_is_saved_even_when_the_following_identity_request_fails() {
    for fails in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("profiles.json");
        let (token_origin, token_worker) = server(json!({
            "access_token":"replacement-access", "refresh_token":"replacement-refresh",
            "token_type":"Bearer", "expires_in":300
        }));
        let api = PolicyServer::new(vec![
            (
                200,
                json!({"issuer":"https://identity.example/realms/enumeray",
                "client_id":"enumeray-cli", "audience":"enumeray-api",
                "authorization_endpoint":"https://identity.example/authorize",
                "token_endpoint":format!("{token_origin}token"),
                "scopes":["openid","offline_access"]}),
            ),
            if fails {
                (
                    503,
                    json!({"code":"authentication_unavailable", "message":"Retry later", "request_id":"refresh"}),
                )
            } else {
                (
                    200,
                    json!({"user_id":uuid::Uuid::new_v4(),"label":"Recipient"}),
                )
            },
        ]);
        std::fs::write(
            &path,
            json!({"profiles":{"default":{
                "api_url":api.origin,"access_token":"previous-access",
                "refresh_token":"previous-refresh","expires_at":1
            }}})
            .to_string(),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let output = cli(&path).args(["auth", "refresh"]).output().unwrap();
        assert_eq!(output.status.success(), !fails);
        let request = token_worker.join().unwrap();
        assert!(request.starts_with("POST /token "));
        assert!(request.contains("refresh_token=previous-refresh"));
        assert!(!request.contains("client_secret"));
        let requests = api.finish();
        assert!(requests[0].starts_with("GET /api/v1/auth/config "));
        assert!(requests[1].contains("Bearer replacement-access"));
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let saved = &saved["profiles"]["default"];
        assert_eq!(saved["access_token"], "replacement-access");
        assert_eq!(saved["refresh_token"], "replacement-refresh");
        assert!(saved["expires_at"].as_u64().unwrap() > 1);
        for secret in [
            "previous-refresh",
            "replacement-refresh",
            "replacement-access",
        ] {
            assert!(!String::from_utf8_lossy(&output.stdout).contains(secret));
            assert!(!String::from_utf8_lossy(&output.stderr).contains(secret));
        }
    }
}

const SESSION_USER: &str = "33333333-3333-4333-8333-333333333333";

fn session_auth(token_origin: &str) -> Value {
    json!({"issuer":"https://identity.example/realms/enumeray", "client_id":"enumeray-cli", "audience":"enumeray-api", "authorization_endpoint":"https://identity.example/authorize", "token_endpoint":format!("{token_origin}token"), "scopes":["openid","offline_access"]})
}

fn save_session(path: &Path, api: &str, token_origin: &str, expires_at: u64, renew_after: u64) {
    let auth = session_auth(token_origin);
    std::fs::write(path, json!({
        "credential_epochs":{"default":"11111111-1111-4111-8111-111111111111"},
        "profiles":{"default":{
            "api_url":api,"access_token":"previous-access","refresh_token":"previous-refresh","expires_at":expires_at,
            "oauth":{"user_id":SESSION_USER,"issuer":auth["issuer"],"client_id":auth["client_id"],"audience":auth["audience"],"token_endpoint":auth["token_endpoint"],"renew_after":renew_after,"renewal":{"status":"ready"}}
        }}
    }).to_string()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn saved_session(path: &Path) -> Value {
    serde_json::from_slice::<Value>(&std::fs::read(path).unwrap()).unwrap()["profiles"]["default"]
        .clone()
}

#[test]
fn automatic_renewal_sends_one_mutation_and_concurrent_commands_share_rotation() {
    for concurrent in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("profiles.json");
        let (token_origin, tokens) = server(
            json!({"access_token":"replacement-access","refresh_token":"replacement-refresh","token_type":"Bearer","expires_in":300}),
        );
        let mut replies = vec![
            (200, session_auth(&token_origin)),
            (200, json!({"user_id":SESSION_USER,"label":"Researcher"})),
            (200, json!({"items":[]})),
        ];
        if concurrent {
            replies.push((200, json!({"items":[]})));
        }
        let api = PolicyServer::with_delay(replies, Duration::from_millis(40));
        save_session(&path, &api.origin, &token_origin, 1, 1);
        let first = cli(&path)
            .args(["api", "POST", "/api/v1/example"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let second = concurrent.then(|| {
            cli(&path)
                .args(["model", "list"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        });
        let result = first.wait_with_output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        if let Some(second) = second {
            let result = second.wait_with_output().unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
        let token_request = tokens.join().unwrap();
        assert!(token_request.contains("refresh_token=previous-refresh"));
        let requests = api.finish();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("POST /api/v1/example "))
                .count(),
            1
        );
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("GET /api/v1/auth/config "))
                .count(),
            1
        );
        assert!(
            requests
                .iter()
                .filter(|request| !request.starts_with("GET /api/v1/auth/config "))
                .all(|request| request.contains("Bearer replacement-access"))
        );
        let saved = saved_session(&path);
        assert_eq!(saved["refresh_token"], "replacement-refresh");
        assert_eq!(saved["oauth"]["renewal"]["status"], "ready");
    }
}

#[test]
fn pending_verification_reuses_persisted_rotation_after_identity_outage() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("profiles.json");
    let (token_origin, tokens) = server(
        json!({"access_token":"replacement-access","refresh_token":"replacement-refresh","token_type":"Bearer","expires_in":300}),
    );
    let api = PolicyServer::new(vec![
        (200, session_auth(&token_origin)),
        (
            503,
            json!({"code":"authentication_unavailable","message":"Retry later","request_id":"pending"}),
        ),
        (200, json!({"user_id":SESSION_USER,"label":"Researcher"})),
        (200, json!({"items":[]})),
    ]);
    save_session(&path, &api.origin, &token_origin, 1, 1);
    let first = cli(&path).args(["model", "list"]).output().unwrap();
    assert!(!first.status.success());
    let pending = saved_session(&path);
    assert_eq!(pending["refresh_token"], "replacement-refresh");
    assert_eq!(pending["oauth"]["renewal"]["status"], "verify_pending");
    let second = cli(&path).args(["model", "list"]).output().unwrap();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    tokens.join().unwrap();
    let requests = api.finish();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.starts_with("GET /api/v1/auth/config "))
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.starts_with("GET /api/v1/models ")
                || request.starts_with("GET /api/v1/models?"))
            .count(),
        1
    );
    assert_eq!(saved_session(&path)["oauth"]["renewal"]["status"], "ready");
}

#[test]
fn expired_pending_verification_renews_the_durable_replacement_not_the_consumed_token() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("profiles.json");
    let provider = PolicyServer::new(vec![
        (
            200,
            json!({"access_token":"first-access","refresh_token":"first-refresh","token_type":"Bearer","expires_in":300}),
        ),
        (
            200,
            json!({"access_token":"second-access","refresh_token":"second-refresh","token_type":"Bearer","expires_in":300}),
        ),
    ]);
    let api = PolicyServer::new(vec![
        (200, session_auth(&provider.origin)),
        (
            503,
            json!({"code":"authentication_unavailable","message":"Retry later","request_id":"pending"}),
        ),
        (200, session_auth(&provider.origin)),
        (200, json!({"user_id":SESSION_USER,"label":"Researcher"})),
        (200, json!({"items":[]})),
    ]);
    save_session(&path, &api.origin, &provider.origin, 1, 1);
    assert!(
        !cli(&path)
            .args(["model", "list"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let mut contents: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    contents["profiles"]["default"]["expires_at"] = json!(1);
    contents["profiles"]["default"]["oauth"]["renew_after"] = json!(1);
    std::fs::write(&path, contents.to_string()).unwrap();
    let result = cli(&path).args(["model", "list"]).output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let requests = provider.finish();
    assert!(requests[0].contains("refresh_token=previous-refresh"));
    assert!(requests[1].contains("refresh_token=first-refresh"));
    assert!(!requests[1].contains("previous-refresh"));
    let api_requests = api.finish();
    assert!(
        api_requests
            .last()
            .unwrap()
            .contains("Bearer second-access")
    );
    assert_eq!(saved_session(&path)["refresh_token"], "second-refresh");
}

#[test]
fn temporary_provider_failure_preserves_session_for_a_later_command_without_retrying() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("profiles.json");
    let provider = PolicyServer::new(vec![
        (
            503,
            json!({"error":"temporarily_unavailable","error_description":"private failure detail"}),
        ),
        (
            200,
            json!({"access_token":"replacement-access","refresh_token":"replacement-refresh","token_type":"Bearer","expires_in":300}),
        ),
    ]);
    let api = PolicyServer::new(vec![
        (200, session_auth(&provider.origin)),
        (200, session_auth(&provider.origin)),
        (200, json!({"user_id":SESSION_USER,"label":"Researcher"})),
        (200, json!({"items":[]})),
    ]);
    save_session(&path, &api.origin, &provider.origin, 1, 1);
    let first = cli(&path).args(["model", "list"]).output().unwrap();
    assert!(!first.status.success());
    let error = String::from_utf8_lossy(&first.stderr);
    assert!(error.contains("session_provider_unavailable"));
    assert!(!error.contains("private failure detail"));
    assert_eq!(saved_session(&path)["oauth"]["renewal"]["status"], "ready");
    assert_eq!(saved_session(&path)["refresh_token"], "previous-refresh");
    let second = cli(&path).args(["model", "list"]).output().unwrap();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(provider.finish().len(), 2);
    assert_eq!(api.finish().len(), 4);
}

#[test]
fn pinned_provider_changes_and_permission_denials_do_not_exchange_refresh_credentials() {
    for denied in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("profiles.json");
        let mut changed = session_auth("http://127.0.0.1:1/");
        changed["issuer"] = json!("https://another-provider.example/realm");
        let api = PolicyServer::new(vec![if denied {
            (
                403,
                json!({"code":"insufficient_scope","message":"Scope unavailable","request_id":"scope"}),
            )
        } else {
            (200, changed)
        }]);
        save_session(
            &path,
            &api.origin,
            "http://127.0.0.1:1/",
            if denied { u64::MAX } else { 1 },
            if denied { u64::MAX } else { 1 },
        );
        let result = cli(&path).args(["model", "list"]).output().unwrap();
        assert!(!result.status.success());
        let error = String::from_utf8_lossy(&result.stderr);
        assert!(error.contains(if denied {
            "insufficient_scope"
        } else {
            "session_changed"
        }));
        assert_eq!(api.finish().len(), 1);
        assert_eq!(saved_session(&path)["refresh_token"], "previous-refresh");
    }
}

#[test]
fn refresh_rejection_uncertain_exchange_and_account_mismatch_never_send_the_operation() {
    for kind in ["rejected", "uncertain", "account"] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("profiles.json");
        let provider = PolicyServer::new(vec![match kind {
            "rejected" => (
                400,
                json!({"error":"invalid_grant","error_description":"private-provider-detail"}),
            ),
            "uncertain" => (200, json!({"access_token":"private-partial-response"})),
            _ => (
                200,
                json!({"access_token":"replacement-access","refresh_token":"replacement-refresh","token_type":"Bearer","expires_in":300}),
            ),
        }]);
        let mut replies = vec![(200, session_auth(&provider.origin))];
        if kind == "account" {
            replies.push((
                200,
                json!({"user_id":"44444444-4444-4444-8444-444444444444","label":"Another account"}),
            ));
        }
        let api = PolicyServer::new(replies);
        save_session(&path, &api.origin, &provider.origin, 1, 1);
        for _ in 0..2 {
            let result = cli(&path)
                .args(["api", "POST", "/api/v1/example"])
                .output()
                .unwrap();
            assert!(!result.status.success());
            let stderr = String::from_utf8_lossy(&result.stderr);
            assert!(!stderr.contains("private-provider-detail"));
            assert!(!stderr.contains("private-partial-response"));
            assert!(!stderr.contains("replacement-refresh"));
        }
        assert_eq!(provider.finish().len(), 1);
        let requests = api.finish();
        assert!(
            requests
                .iter()
                .all(|request| request.starts_with("GET /api/v1/auth/config ")
                    || request.starts_with("GET /api/v1/me "))
        );
        assert_eq!(
            saved_session(&path)["oauth"]["renewal"]["status"],
            if kind == "uncertain" {
                "exchanging"
            } else {
                "reauth_required"
            }
        );
    }
}

struct PausedRefresh {
    origin: String,
    arrival: mpsc::Receiver<()>,
    release: mpsc::Sender<()>,
    worker: thread::JoinHandle<()>,
}

fn paused_refresh() -> PausedRefresh {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    let (arrived, arrival) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let worker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            headers.push(byte[0]);
        }
        let headers = String::from_utf8(headers).unwrap();
        let length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .and_then(|value| value.trim().parse::<usize>().ok())
            })
            .unwrap();
        let mut body = vec![0; length];
        stream.read_exact(&mut body).unwrap();
        assert!(
            String::from_utf8(body)
                .unwrap()
                .contains("refresh_token=previous-refresh")
        );
        arrived.send(()).unwrap();
        released.recv_timeout(Duration::from_secs(15)).unwrap();
        let body = json!({"access_token":"lost-access","refresh_token":"lost-refresh","token_type":"Bearer","expires_in":300}).to_string();
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
    });
    PausedRefresh {
        origin,
        arrival,
        release,
        worker,
    }
}

#[test]
fn killed_refresh_owner_leaves_an_uncertain_marker_and_never_reuses_the_old_token() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("profiles.json");
    let provider = paused_refresh();
    let api = PolicyServer::new(vec![(200, session_auth(&provider.origin))]);
    save_session(&path, &api.origin, &provider.origin, 1, 1);
    let mut child = cli(&path)
        .args(["model", "list"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Err(error) = provider.arrival.recv_timeout(Duration::from_secs(15)) {
        let _ = child.kill();
        let output = child.wait_with_output().unwrap();
        panic!(
            "Refresh did not start: {error}; child {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(
        saved_session(&path)["oauth"]["renewal"]["status"],
        "exchanging"
    );
    child.kill().unwrap();
    child.wait().unwrap();
    provider.release.send(()).unwrap();
    provider.worker.join().unwrap();
    let retry = cli(&path).args(["model", "list"]).output().unwrap();
    assert!(!retry.status.success());
    assert!(String::from_utf8_lossy(&retry.stderr).contains("session_renewal_uncertain"));
    assert_eq!(saved_session(&path)["refresh_token"], "previous-refresh");
    assert_eq!(api.finish().len(), 1);
}

#[cfg(unix)]
#[test]
fn rotation_persistence_failure_preserves_uncertainty_and_never_follows_a_symlink() {
    use std::os::unix::fs::symlink;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("profiles.json");
    let marker = directory.path().join("exchanging.json");
    let unrelated = directory.path().join("unrelated.json");
    std::fs::write(&unrelated, b"preserve unrelated data").unwrap();
    let provider = paused_refresh();
    let api = PolicyServer::new(vec![(200, session_auth(&provider.origin))]);
    save_session(&path, &api.origin, &provider.origin, 1, 1);
    let child = cli(&path)
        .args(["model", "list"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    provider
        .arrival
        .recv_timeout(Duration::from_secs(15))
        .unwrap();
    assert_eq!(
        saved_session(&path)["oauth"]["renewal"]["status"],
        "exchanging"
    );
    std::fs::rename(&path, &marker).unwrap();
    symlink(&unrelated, &path).unwrap();
    provider.release.send(()).unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("session_persistence"), "{stderr}");
    assert!(!stderr.contains("lost-refresh"));
    provider.worker.join().unwrap();
    assert_eq!(
        std::fs::read(&unrelated).unwrap(),
        b"preserve unrelated data"
    );
    std::fs::remove_file(&path).unwrap();
    std::fs::rename(&marker, &path).unwrap();
    let retry = cli(&path).args(["model", "list"]).output().unwrap();
    assert!(!retry.status.success());
    assert!(String::from_utf8_lossy(&retry.stderr).contains("session_renewal_uncertain"));
    assert_eq!(saved_session(&path)["refresh_token"], "previous-refresh");
    assert_eq!(api.finish().len(), 1);
}

#[test]
fn expired_legacy_profile_requires_one_explicit_refresh_and_environment_tokens_stay_static() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("profiles.json");
    let api = PolicyServer::new(vec![(200, json!({"items":[]}))]);
    save_session(&path, &api.origin, "http://127.0.0.1:1/", 1, 1);
    let mut contents: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    contents["profiles"]["default"]
        .as_object_mut()
        .unwrap()
        .remove("oauth");
    std::fs::write(&path, contents.to_string()).unwrap();
    let result = cli(&path).args(["model", "list"]).output().unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("auth refresh once"));
    let result = cli(&path)
        .env("ENUMERAY_TOKEN", "environment-token")
        .args(["model", "list"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let requests = api.finish();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].contains("Bearer environment-token"));
    assert_eq!(saved_session(&path)["refresh_token"], "previous-refresh");
}

#[test]
fn stored_credentials_are_private_redacted_and_bound_to_the_original_origin() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("profiles.json");
    let user = uuid::Uuid::new_v4();
    let (origin, worker) = server(json!({"user_id":user,"label":"Researcher","permissions":[]}));
    let mut login = cli(&path)
        .args(["--api", &origin, "auth", "login", "--token-stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    login
        .stdin
        .take()
        .unwrap()
        .write_all(b"secret-test-token\n")
        .unwrap();
    let output = login.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("secret-test-token"));
    assert!(
        worker
            .join()
            .unwrap()
            .contains("authorization: Bearer secret-test-token")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let output = cli(&path).args(["auth", "profiles"]).output().unwrap();
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("secret-test-token"));
    let (other, other_worker) = server(json!({"items":[]}));
    let output = cli(&path)
        .args(["--api", &other, "model", "list"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !other_worker
            .join()
            .unwrap()
            .to_ascii_lowercase()
            .contains("authorization:")
    );
    let output = cli(&path).args(["auth", "logout"]).output().unwrap();
    assert!(output.status.success());
    assert!(
        !std::fs::read_to_string(&path)
            .unwrap()
            .contains("secret-test-token")
    );
}

#[test]
fn unsupported_manifests_fail_locally_before_credentials_or_network() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("manifest.json");
    std::fs::write(
        &path,
        br#"{"manifest_version":900,"kind":"model-publication","spec":{}}"#,
    )
    .unwrap();
    let output = cli(&directory.path().join("absent-profile.json"))
        .args(["model", "validate", "--file"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "client_error");
    assert!(
        !error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Configure an API")
    );
}

#[test]
fn cancellation_sends_an_empty_body_to_the_named_operation() {
    let directory = tempfile::tempdir().unwrap();
    let (origin, worker) = server(json!({"run_id":"example","state":"cancelled"}));
    let output = cli(&directory.path().join("missing.json"))
        .args(["--api", &origin, "run", "cancel", "example"])
        .env("ENUMERAY_TOKEN", "scoped-user-token")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let request = worker.join().unwrap().to_ascii_lowercase();
    assert!(request.starts_with("put /api/v1/runs/example/cancellation http/1.1\r\n"));
    assert!(!request.contains("content-type:"));
    assert!(!request.contains("content-length: 2"));
}

#[test]
fn publication_revocation_sends_an_empty_body_to_the_exact_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let (origin, worker) = server(
        json!({"scope":"publication","affected_publication_ids":["original"],"publication_id":"original","revoked_at":"2026-10-12T00:00:00Z"}),
    );
    let output = cli(&directory.path().join("missing.json"))
        .args([
            "--api",
            &origin,
            "model",
            "revoke-publication",
            "model",
            "original",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let request = worker.join().unwrap().to_ascii_lowercase();
    assert!(request.starts_with("delete /api/v1/models/model/publications/original http/1.1\r\n"));
    assert!(!request.contains("content-type:"));
    assert!(!request.contains("content-length: 2"));
}

#[test]
fn publication_retirement_sends_the_saved_generation_to_the_exact_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let (origin, worker) = server(
        json!({"publication_id":"original","model_id":"model","requested_at":"2026-10-17T00:00:00Z","generation":8,"state":"retirement_requested"}),
    );
    let output = cli(&directory.path().join("missing.json"))
        .args([
            "--api",
            &origin,
            "model",
            "retire-publication",
            "model",
            "original",
            "--generation",
            "7",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let request = worker.join().unwrap();
    assert!(
        request
            .starts_with("PUT /api/v1/models/model/publications/original/retirement HTTP/1.1\r\n")
    );
    assert_eq!(
        request.split("\r\n\r\n").nth(1).unwrap(),
        r#"{"expected_generation":7}"#
    );
}

#[test]
fn context_update_sends_only_metadata_and_the_observed_generation() {
    let directory = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    let worker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut headers = Vec::new();
        let mut byte = [0];
        while !headers.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            headers.push(byte[0]);
        }
        let headers = String::from_utf8(headers).unwrap();
        let length: usize = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length: ")
                    .map(|value| value.parse().unwrap())
            })
            .unwrap();
        let mut body = vec![0; length];
        stream.read_exact(&mut body).unwrap();
        let reply=json!({"id":"cpu","metadata":{"location":"Research lab","disclosure":"Research-owned execution"},"generation":2}).to_string();
        write!(stream,"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{reply}",reply.len()).unwrap();
        (headers, serde_json::from_slice::<Value>(&body).unwrap())
    });
    let output = cli(&directory.path().join("missing.json"))
        .args([
            "--api",
            &origin,
            "connector",
            "context",
            "update",
            "worker",
            "cpu",
            "--location",
            "Research lab",
            "--disclosure",
            "Research-owned execution",
            "--generation",
            "1",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (headers, body) = worker.join().unwrap();
    assert!(
        headers.starts_with("PUT /api/v1/connectors/worker/contexts/cpu/metadata HTTP/1.1\r\n")
    );
    assert_eq!(
        body,
        json!({"metadata":{"location":"Research lab","disclosure":"Research-owned execution"},"expected_generation":1})
    );
}

#[test]
fn grant_commands_send_explicit_limits_and_display_generation() {
    let directory = tempfile::tempdir().unwrap();
    let grant = "33333333-3333-4333-8333-333333333333";
    let record = json!({"id":grant,"context_id":"cpu","user_id":"11111111-1111-4111-8111-111111111111","max_queued_runs":10,"max_outstanding_assignments":2,"generation":8,"revoked_at":null});
    for command in ["grant-get", "grant-update"] {
        let (origin, worker) = server(record.clone());
        let mut invocation = human_cli(&directory.path().join("missing.json"));
        invocation.args([
            "--api",
            &origin,
            "connector",
            "grant",
            command.strip_prefix("grant-").unwrap(),
            "worker",
            grant,
        ]);
        if command == "grant-update" {
            invocation.args([
                "--max-queued-runs",
                "10",
                "--max-outstanding-assignments",
                "2",
                "--generation",
                "7",
            ]);
        }
        let output = invocation.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let human = String::from_utf8(output.stdout).unwrap();
        for text in [
            grant,
            "Generation: 8",
            "Queue limit: 10",
            "Assignment limit: 2",
        ] {
            assert!(human.contains(text), "{human}");
        }
        let wire = worker.join().unwrap();
        let method = if command == "grant-get" { "GET" } else { "PUT" };
        assert!(wire.starts_with(&format!(
            "{method} /api/v1/connectors/worker/grants/{grant} HTTP/1.1\r\n"
        )));
        if command == "grant-update" {
            assert_eq!(
                serde_json::from_str::<Value>(wire.split("\r\n\r\n").nth(1).unwrap()).unwrap(),
                json!({"expected_generation":7,"max_queued_runs":10,"max_outstanding_assignments":2})
            );
        }
    }
    let (origin, worker) = server(record.clone());
    let output = cli(&directory.path().join("missing.json"))
        .args([
            "--api",
            &origin,
            "connector",
            "grant",
            "get",
            "worker",
            grant,
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        record
    );
    worker.join().unwrap();
    for flags in [
        vec!["--max-queued-runs", "10", "--generation", "7"],
        vec![
            "--max-queued-runs",
            "0",
            "--max-outstanding-assignments",
            "2",
            "--generation",
            "7",
        ],
    ] {
        let output = cli(&directory.path().join("missing.json"))
            .args([
                "--api",
                "http://127.0.0.1:1",
                "connector",
                "grant",
                "update",
                "worker",
                grant,
            ])
            .args(flags)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("Connection refused"));
    }
}

#[test]
fn grant_update_conflict_is_not_retried_or_refreshed() {
    let directory = tempfile::tempdir().unwrap();
    let server = PolicyServer::new(vec![(
        409,
        json!({"code":"management_conflict","message":"Inspect grant first","request_id":"grant"}),
    )]);
    let output = cli(&directory.path().join("missing.json"))
        .args([
            "--api",
            &server.origin,
            "connector",
            "grant",
            "update",
            "worker",
            "33333333-3333-4333-8333-333333333333",
            "--max-queued-runs",
            "10",
            "--max-outstanding-assignments",
            "2",
            "--generation",
            "7",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("management_conflict"));
    assert_eq!(server.finish().len(), 1);
}

fn paginated_commands() -> Vec<(Vec<&'static str>, &'static str)> {
    vec![
        (vec!["model", "list"], "/api/v1/models"),
        (
            vec!["model", "publications", "example"],
            "/api/v1/models/example/publications",
        ),
        (
            vec!["model", "runs", "example"],
            "/api/v1/models/example/runs",
        ),
        (
            vec!["model", "audit", "example"],
            "/api/v1/audit/model/example",
        ),
        (vec!["connector", "list"], "/api/v1/connectors"),
        (
            vec!["connector", "grant", "available"],
            "/api/v1/compute-grants",
        ),
        (
            vec!["connector", "context", "list", "worker"],
            "/api/v1/connectors/worker/contexts",
        ),
        (
            vec!["connector", "grant", "list", "worker"],
            "/api/v1/connectors/worker/grants",
        ),
        (
            vec!["connector", "runs", "worker"],
            "/api/v1/connectors/worker/runs",
        ),
        (
            vec!["connector", "audit", "worker"],
            "/api/v1/audit/connector/worker",
        ),
        (
            vec!["access", "requests", "example"],
            "/api/v1/models/example/access-requests",
        ),
        (
            vec!["access", "invitations", "example"],
            "/api/v1/models/example/invitations",
        ),
        (vec!["auth", "token-list"], "/api/v1/tokens"),
        (vec!["service", "audit"], "/api/v1/service/audit"),
        (vec!["run", "list"], "/api/v1/runs"),
        (vec!["run", "logs", "run-one"], "/api/v1/runs/run-one/logs"),
        (
            vec!["run", "events", "run-one"],
            "/api/v1/runs/run-one/events",
        ),
        (
            vec!["run", "artifacts", "run-one"],
            "/api/v1/runs/run-one/artifacts",
        ),
    ]
}

fn request_url(request: &str) -> reqwest::Url {
    reqwest::Url::parse(&format!(
        "http://fixture{}",
        request
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
    ))
    .unwrap()
}

#[test]
fn every_native_collection_forwards_one_encoded_page_and_preserves_json() {
    let cursor = "opaque +/&?='$(false);`false`";
    for (command, path) in paginated_commands() {
        let value = collection_page(path, "next-opaque");
        let directory = tempfile::tempdir().unwrap();
        let server = PolicyServer::new(vec![(200, value.clone())]);
        let output = cli(&directory.path().join("missing.json"))
            .args(["--api", &server.origin])
            .args(&command)
            .args(["--limit", "1", "--cursor", cursor])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{command:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap(),
            value
        );
        assert!(output.stderr.is_empty());
        let requests = server.finish();
        assert_eq!(requests.len(), 1, "{command:?}");
        let url = request_url(&requests[0]);
        assert_eq!(url.path(), path);
        assert_eq!(
            url.query_pairs().collect::<Vec<_>>(),
            vec![
                ("limit".into(), "1".into()),
                ("cursor".into(), cursor.into())
            ]
        );
    }
}

#[test]
fn every_native_collection_leaves_unspecified_defaults_to_the_api() {
    for (command, path) in paginated_commands() {
        let directory = tempfile::tempdir().unwrap();
        let server = PolicyServer::new(vec![(200, json!({"items":[],"next_cursor":null}))]);
        let output = cli(&directory.path().join("missing.json"))
            .args(["--api", &server.origin])
            .args(&command)
            .output()
            .unwrap();
        assert!(output.status.success());
        let requests = server.finish();
        assert_eq!(requests.len(), 1);
        let url = request_url(&requests[0]);
        assert_eq!(url.path(), path);
        assert_eq!(url.query(), None);
    }
}

#[test]
fn human_continuation_commands_follow_every_collection_without_hidden_requests() {
    for (command, path) in paginated_commands() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("private profiles's.json");
        let cursor = "opaque +/&?='$(false);`false`";
        let server = PolicyServer::new(vec![
            (200, collection_page(path, cursor)),
            (200, json!({"items":[],"next_cursor":null})),
        ]);
        let mut process = human_cli(&config);
        process
            .args([
                "--api",
                &server.origin,
                "--profile",
                "science team",
                "--timeout",
                "11",
            ])
            .args(&command)
            .args(["--limit", "1"]);
        if command == ["run", "list"] {
            process.args([
                "--model",
                "example",
                "--status",
                "queued",
                "--created-after",
                "2026-10-15T12:00:00+03:00",
                "--created-before",
                "2026-10-16T12:00:00Z",
            ]);
        }
        let output = process.output().unwrap();
        assert!(
            output.status.success(),
            "{command:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains("Returned 1 item(s)"));
        let continuation = stdout
            .lines()
            .find_map(|line| line.strip_prefix("Next page: enumeray "))
            .unwrap();
        let output = Command::new("/bin/sh")
            .args(["-c", &format!("\"$ENUMERAY_TEST_BIN\" {continuation}")])
            .env("ENUMERAY_TEST_BIN", env!("CARGO_BIN_EXE_enumeray"))
            .env_remove("ENUMERAY_TOKEN")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{command:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains("Returned 0 item(s)"));
        assert!(!stdout.contains("Next page:"));
        let requests = server.finish();
        assert_eq!(requests.len(), 2);
        let first = request_url(&requests[0]);
        let second = request_url(&requests[1]);
        assert_eq!(first.path(), path);
        assert_eq!(second.path(), path);
        let second_query: Vec<_> = second
            .query_pairs()
            .filter(|(key, _)| key != "cursor")
            .collect();
        assert_eq!(first.query_pairs().collect::<Vec<_>>(), second_query);
        assert_eq!(
            second
                .query_pairs()
                .find(|(key, _)| key == "cursor")
                .unwrap()
                .1,
            cursor
        );
    }
}

#[test]
fn collections_preserve_authorization_failures_on_stderr_without_output() {
    for (command, _) in paginated_commands() {
        for json_output in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let failure = json!({"code":"not_found","message":"The requested resource was not found.","request_id":"request-one"});
            let server = PolicyServer::new(vec![(404, failure.clone())]);
            let mut process = human_cli(&directory.path().join("missing.json"));
            if json_output {
                process.arg("--json");
            }
            let output = process
                .args(["--api", &server.origin])
                .args(&command)
                .output()
                .unwrap();
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            if json_output {
                let actual: Value = serde_json::from_slice(&output.stderr).unwrap();
                assert_eq!(actual["status"], 404);
                assert_eq!(actual["error"], failure);
            } else {
                assert!(String::from_utf8_lossy(&output.stderr).contains("not_found (HTTP 404)"));
            }
            assert_eq!(server.finish().len(), 1);
        }
    }
}

#[test]
fn invalid_page_limits_and_unsupported_filters_fail_before_network() {
    let directory = tempfile::tempdir().unwrap();
    for (command, _) in paginated_commands() {
        for limit in ["0", "101", "-1", "abc"] {
            let output = human_cli(&directory.path().join("missing.json"))
                .args(&command)
                .args(["--limit", limit])
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(2), "{command:?}, {limit}");
            assert!(output.stdout.is_empty());
        }
    }
    let output = human_cli(&directory.path().join("missing.json"))
        .args(["model", "runs", "example", "--status", "queued"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let output = human_cli(&directory.path().join("missing.json"))
        .args(["run", "list", "--status", "done"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn expired_history_cursor_requires_explicit_recovery_without_another_request() {
    for kind in ["logs", "events"] {
        for json_output in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let failure = json!({"code":"history_cursor_expired","message":"The cursor precedes retained logs. Resume with X-History-Recovery-Cursor and display the retention gap.","request_id":"history-request"});
            let server = PolicyServer::new(vec![(410, failure.clone())]);
            let mut process = human_cli(&directory.path().join("missing.json"));
            if json_output {
                process.arg("--json");
            }
            let output = process
                .args([
                    "--api",
                    &server.origin,
                    "run",
                    kind,
                    "run-one",
                    "--cursor",
                    "expired-cursor",
                    "--limit",
                    "1",
                ])
                .output()
                .unwrap();
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            if json_output {
                assert_eq!(
                    serde_json::from_slice::<Value>(&output.stderr).unwrap(),
                    json!({"status":410,"error":failure,"retry_after":null})
                );
            } else {
                let stderr = String::from_utf8(output.stderr).unwrap();
                assert!(stderr.contains("explicitly acknowledge missing earlier entries"));
                let recovery = stderr.lines().last().unwrap();
                assert!(recovery.contains(&format!("run {kind} run-one --limit 1")));
                assert!(!recovery.contains("--cursor"));
            }
            assert_eq!(server.finish().len(), 1);
        }
    }
}

#[test]
fn publication_review_shows_trial_evidence_and_inspection_commands() {
    let directory = tempfile::tempdir().unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let mut publication =
        collection_page("/api/v1/models/model/publications", "cursor")["items"][0].clone();
    publication["id"] = json!("candidate");
    publication["model_id"] = json!("model");
    publication["status"] = json!("ready");
    publication["admission_updated_at"] = Value::Null;
    publication["validation_evidence"] = Value::Null;
    publication["trial_availability"] = json!({"available":true,"unavailable_reason":null});
    publication["trial_evidence"] = json!({"counts":{"queued":0,"running":0,"succeeded":1,"failed":0,"cancelled":0},"latest_run":{"run_id":id,"state":"succeeded"},"latest_succeeded_run":{"run_id":id}});
    let (origin, worker) = server(publication);
    let output = human_cli(&directory.path().join("missing.json"))
        .args([
            "--api",
            &origin,
            "model",
            "publication",
            "model",
            "candidate",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let human = String::from_utf8(output.stdout).unwrap();
    for text in [
        "Latest successful trial",
        "scientific correctness",
        "run get",
        "run logs",
        "run artifacts",
        "run provenance",
        &id,
    ] {
        assert!(human.contains(text), "{human}");
    }
    worker.join().unwrap();
}

// Collection transport tests use complete management records, matching the real API.
fn collection_page(path: &str, cursor: &str) -> Value {
    let user = "22222222-2222-4222-8222-222222222222";
    let id = "33333333-3333-4333-8333-333333333333";
    let at = "2026-10-17T00:00:00+00:00";
    let metadata = json!({"location":"Lab","disclosure":"Lab data access."});
    let item = if path.ends_with("/publications") {
        let definition = serde_json::to_value(
            enumeray_contracts::models::parse_publication(include_bytes!(
                "../examples/scalar-publication.json"
            ))
            .unwrap()
            .definition,
        )
        .unwrap();
        json!({"id":"full-identifier","definition":definition,"descriptor":{},"admission_open":true,"context_id":"cpu","grant_id":id,"generation":1,"status":"published","validation_message":null,"disclosure":"Lab data access.","execution_location":"Lab","revoked_at":null,"installation_revoked_at":null,"created_at":at,"published_at":at,"configuration_retention":{"state":"retained","requested_at":null,"retired_at":null,"retirement_blocked":false,"blockers":{"queued_runs":0,"running_runs":0,"pending_delivery_assignments":0,"pending_log_archives":0,"pending_assignment_uploads":0}},"rollback_availability":{"available":true,"unavailable_reason":null,"requires_revalidation":true}})
    } else if path == "/api/v1/models/example/runs" {
        json!({"run_id":id,"model_id":"example","publication_id":"full-identifier","purpose":"standard","created_by_user_id":user,"state":"succeeded","created_at":at})
    } else if path.starts_with("/api/v1/audit/") || path == "/api/v1/service/audit" {
        let mut event = json!({"id":id,"actor_kind":"user","actor_id":user,"action":"created","details":{},"occurred_at":at,"request_id":"request"});
        if path == "/api/v1/service/audit" {
            event["resource_kind"] = json!("model");
            event["resource_id"] = json!("example");
        }
        event
    } else if path == "/api/v1/connectors" {
        json!({"connector_id":"full-identifier","label":"Lab","owner_id":user,"ownership_generation":1,"archived_at":null,"generation":1,"observed_generation":1,"draining":false,"authority_withdrawn":false,"max_outstanding_assignments":4})
    } else if path == "/api/v1/compute-grants" {
        json!({"id":id,"context_id":"cpu","connector_id":"worker","connector_label":"Lab","user_id":user,"generation":1,"max_queued_runs":100,"max_outstanding_assignments":4,"context":{"id":"cpu","driver":"process","connection":"direct","installation_roots":["/opt/models"],"metadata":metadata}})
    } else if path.ends_with("/contexts") {
        json!({"id":"full-identifier","connector_id":"worker","definition":{},"metadata":metadata,"state":"active","generation":1})
    } else if path.ends_with("/grants") {
        json!({"id":id,"context_id":"cpu","user_id":user,"generation":1,"max_queued_runs":100,"max_outstanding_assignments":4,"revoked_at":null})
    } else if path == "/api/v1/connectors/worker/runs" {
        json!({"run_id":id,"model_id":"example","publication_id":"full-identifier","context_id":"cpu","execution_location":"Lab","connector_id":"worker","state":"succeeded","phase":null,"created_at":at,"blocked_reason":null})
    } else if path.ends_with("/connectors") {
        json!({"connector_id":"full-identifier","label":"Worker","generation":1,"observed_generation":1,"draining":false,"authority_withdrawn":false,"max_outstanding_assignments":4})
    } else if path.ends_with("/access-requests") {
        json!({"id":id,"user_id":user,"status":"pending","message":"","created_at":at,"resolved_at":null})
    } else if path.ends_with("/invitations") {
        json!({"id":id,"model_id":"example","recipient_id":user,"recipient_email":null,"expires_at":at,"accepted_by":null,"accepted_at":null,"created_at":at,"status":"pending"})
    } else {
        json!({"id":"full-identifier","model_id":"example"})
    };
    json!({"items":[item],"next_cursor":cursor})
}

#[test]
fn model_create_imports_only_catalogue_metadata_from_an_offline_draft() {
    for defaults in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("publication.json");
        let mut source: Value =
            serde_json::from_str(include_str!("../examples/scalar-publication.json")).unwrap();
        source["execution"]["context_id"] = Value::Null;
        source["execution"]["release_path"] = Value::Null;
        if defaults {
            source["model"].as_object_mut().unwrap().remove("name");
            source["model"]
                .as_object_mut()
                .unwrap()
                .remove("description");
        }
        std::fs::write(&path, source.to_string()).unwrap();
        let name = if defaults {
            "scalar-double"
        } else {
            "Scalar double"
        };
        let description = if defaults {
            ""
        } else {
            "Doubles a finite scalar value."
        };
        let response = json!({"model_id":"scalar-double","name":name,"description":description,"owner_id":"22222222-2222-4222-8222-222222222222","generation":1,"access_mode":"invite","visibility":"listed","current_publication_id":null,"can_run":true,"can_manage":true,"current_publication":null});
        let (origin, worker) = server(response);
        let output = cli(&directory.path().join("missing-profile.json"))
            .args([
                "--api",
                &origin,
                "model",
                "create",
                "scalar-double",
                "--file",
            ])
            .arg(&path)
            .env("ENUMERAY_TOKEN", "scoped-author")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let request = worker.join().unwrap();
        assert!(request.starts_with("POST /api/v1/models HTTP/1.1\r\n"));
        let body: Value = serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(
            body,
            json!({"model":{"id":"scalar-double","name":name,"description":description}})
        );
    }
}

#[test]
fn model_create_rejects_mismatched_source_and_conflicting_metadata_without_requests() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("publication.json");
    std::fs::write(&path, include_str!("../examples/scalar-publication.json")).unwrap();
    let service = PolicyServer::new(vec![]);
    for extra in [
        vec![],
        vec!["--name", "Conflicting name"],
        vec!["--description", "Conflicting description"],
    ] {
        let output = cli(&directory.path().join("missing-profile.json"))
            .args([
                "--api",
                &service.origin,
                "model",
                "create",
                "another-model",
                "--file",
            ])
            .arg(&path)
            .args(extra)
            .env("ENUMERAY_TOKEN", "scoped-author")
            .output()
            .unwrap();
        assert!(!output.status.success());
    }
    assert!(service.finish().is_empty());
}

#[test]
fn connector_resources_are_nested_and_removed_infrastructure_paths_fail_before_network() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("missing.json");
    for group in ["context", "grant", "member", "ownership"] {
        let output = cli(&config)
            .args(["connector", group, "--help"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    for words in [
        vec!["infra", "list"],
        vec!["connector", "context-get", "worker", "cpu"],
        vec!["connector", "get", "old-scope", "worker"],
    ] {
        let output = cli(&config).args(words).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }
    let path = directory.path().join("context.json");
    std::fs::write(
        &path,
        include_bytes!("../examples/process-direct-context.json"),
    )
    .unwrap();
    let output = cli(&config)
        .args([
            "--api",
            "http://127.0.0.1:1",
            "connector",
            "context",
            "create",
            "different-connector",
            "--file",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("connector_id differs from the selected connector")
    );
}

#[test]
fn connector_ownership_mutations_send_the_exact_resource_and_ownership_generation() {
    let directory = tempfile::tempdir().unwrap();
    let user = "11111111-1111-4111-8111-111111111111";
    let transfer = "33333333-3333-4333-8333-333333333333";
    for (words, method, suffix, request, response) in [
        (
            vec![
                "member",
                "add",
                "worker",
                "--user",
                user,
                "--generation",
                "9",
            ],
            "POST",
            "members",
            json!({"user_id":user,"expected_generation":9}),
            json!({"user_id":user,"co_admin":true,"generation":10}),
        ),
        (
            vec![
                "member",
                "remove",
                "worker",
                "--user",
                user,
                "--generation",
                "9",
            ],
            "DELETE",
            "members/11111111-1111-4111-8111-111111111111",
            json!({"expected_generation":9}),
            json!({"user_id":user,"co_admin":false,"generation":10}),
        ),
        (
            vec![
                "ownership",
                "transfer",
                "worker",
                "--to",
                user,
                "--generation",
                "9",
            ],
            "POST",
            "ownership-transfers",
            json!({"to_user_id":user,"expected_generation":9}),
            json!({"id":transfer,"to_user_id":user}),
        ),
        (
            vec!["archive", "worker", "--generation", "9"],
            "POST",
            "archive",
            json!({"expected_generation":9}),
            json!({"id":"worker","archived":true,"generation":10}),
        ),
    ] {
        let (origin, worker) = server(response.clone());
        let output = cli(&directory.path().join("missing.json"))
            .args(["--api", &origin, "connector"])
            .args(words)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap(),
            response
        );
        let wire = worker.join().unwrap();
        assert!(wire.starts_with(&format!(
            "{method} /api/v1/connectors/worker/{suffix} HTTP/1.1\r\n"
        )));
        assert_eq!(
            serde_json::from_str::<Value>(wire.split_once("\r\n\r\n").unwrap().1).unwrap(),
            request
        );
    }
}

#[test]
fn remote_preflight_reconciles_lost_replies_with_its_saved_request_and_refuses_account_changes() {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let directory = tempfile::tempdir().unwrap();
    let operation = directory.path().join("check");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&operation)
        .unwrap();
    let id = uuid::Uuid::new_v4();
    let user = uuid::Uuid::new_v4();
    let hash = "a".repeat(64);
    let observed = json!({"connector_id":"worker","context_id":"cpu","definition_hash":hash,"ownership_generation":1,"attempt":null,"report":null,"validity":"unavailable","reason":"not_checked","ready":false});
    let report = json!({"manifest_version":1,"request_id":id,"connector_id":"worker","context_id":"cpu","definition_hash":hash,"execution_mode":"managed","observed_at":"2026-10-31T12:00:00Z","source":"connector","execution_uid":"501","checks":[{"check":"configuration","outcome":"passed","required":true,"code":"passed","responsible_role":"connector_owner","correction":""}],"limitations":[]});
    let completed = json!({"connector_id":"worker","context_id":"cpu","definition_hash":hash,"ownership_generation":1,"attempt":{"request_id":id,"requested_at":"2026-10-31T12:00:00Z","expires_at":"2026-10-31T12:15:00Z","state":"completed"},"report":report,"validity":"current","reason":"current","ready":true});
    let api = PolicyServer::new(vec![
        (200, json!({"user_id":user,"label":"Owner"})),
        (200, observed),
        (
            503,
            json!({"code":"management_unavailable","message":"Retry the same request.","request_id":"lost"}),
        ),
        (200, json!({"user_id":user,"label":"Owner"})),
        (200, completed),
        (
            200,
            json!({"user_id":uuid::Uuid::new_v4(),"label":"Other account"}),
        ),
    ]);
    let saved = json!({"manifest_version":1,"request_id":id,"api_url":api.origin,"user_id":user,"connector_id":"worker","context_id":"cpu","request":{"expected_previous_request_id":null,"expected_ownership_generation":1,"definition_hash":hash}});
    let path = operation.join("operation.json");
    std::fs::write(&path, serde_json::to_vec(&saved).unwrap()).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let before = std::fs::read(&path).unwrap();
    let invoke = || {
        cli(&directory.path().join("missing.json"))
            .args([
                "--api",
                &api.origin,
                "connector",
                "context",
                "preflight",
                "worker",
                "cpu",
                "--directory",
            ])
            .arg(&operation)
            .output()
            .unwrap()
    };
    assert!(!invoke().status.success());
    assert_eq!(before, std::fs::read(&path).unwrap());
    let reconciled = invoke();
    assert!(
        reconciled.status.success(),
        "{}",
        String::from_utf8_lossy(&reconciled.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&reconciled.stdout).unwrap()["ready"],
        true
    );
    assert!(!invoke().status.success());
    assert_eq!(before, std::fs::read(&path).unwrap());
    let requests = api.finish();
    assert_eq!(requests.len(), 6);
    assert_eq!(requests.iter().filter(|r| r.starts_with("PUT ")).count(), 1);
    assert!(requests[2].contains(&format!("/preflight/{id} HTTP/1.1")));
}
