//! Standalone CLI observation over real sockets, including interruption and recovery.
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    process::{Command, Output, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const USER: &str = "22222222-2222-4222-8222-222222222222";
const ASSIGNMENT: &str = "33333333-3333-4333-8333-333333333333";
const ANALYSIS_RUN: &str = "44444444-4444-4444-8444-444444444444";

struct Reply {
    status: u16,
    headers: String,
    body: String,
    hold: Duration,
    delay: Duration,
}
impl Reply {
    fn json(value: Value) -> Self {
        Self {
            status: 200,
            headers: "Content-Type: application/json\r\n".into(),
            body: value.to_string(),
            hold: Duration::ZERO,
            delay: Duration::ZERO,
        }
    }
    fn sse(body: String, hold: u64) -> Self {
        Self {
            status: 200,
            headers: "Content-Type: text/event-stream\r\n".into(),
            body,
            hold: Duration::from_secs(hold),
            delay: Duration::ZERO,
        }
    }
}

struct Server {
    origin: String,
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<String>>>,
    first_request: Arc<Mutex<Option<Instant>>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Server {
    fn new(handler: impl Fn(&str) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let first_request = Arc::new(Mutex::new(None));
        let (stopping, recorded, first, handler) = (
            stop.clone(),
            requests.clone(),
            first_request.clone(),
            Arc::new(handler),
        );
        let worker = thread::spawn(move || {
            let mut connections = Vec::new();
            while !stopping.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let (stop, requests, first, handler) = (
                            stopping.clone(),
                            recorded.clone(),
                            first.clone(),
                            handler.clone(),
                        );
                        connections.push(thread::spawn(move || {
                            // Accepted sockets inherit O_NONBLOCK on BSD/macOS.
                            stream.set_nonblocking(false).unwrap();
                            stream
                                .set_read_timeout(Some(Duration::from_secs(5)))
                                .unwrap();
                            let mut bytes = Vec::new();
                            let mut byte = [0];
                            while !bytes.ends_with(b"\r\n\r\n") {
                                if stream.read_exact(&mut byte).is_err() {
                                    return;
                                }
                                bytes.push(byte[0]);
                            }
                            let request = String::from_utf8(bytes).unwrap();
                            let length = request
                                .lines()
                                .find_map(|line| {
                                    line.to_ascii_lowercase()
                                        .strip_prefix("content-length:")
                                        .and_then(|value| value.trim().parse::<usize>().ok())
                                })
                                .unwrap_or(0);
                            let mut body = vec![0; length];
                            if stream.read_exact(&mut body).is_err() {
                                return;
                            }
                            first.lock().unwrap().get_or_insert_with(Instant::now);
                            requests.lock().unwrap().push(request.clone());
                            let reply = handler(&request);
                            let pause = |duration: Duration| {
                                let until = Instant::now() + duration;
                                while !stop.load(Ordering::SeqCst) && Instant::now() < until {
                                    thread::sleep(Duration::from_millis(5));
                                }
                            };
                            pause(reply.delay);
                            let _ = write!(
                                stream,
                                "HTTP/1.1 {} Test\r\nConnection: close\r\n{}\r\n{}",
                                reply.status, reply.headers, reply.body
                            );
                            let _ = stream.flush();
                            pause(reply.hold);
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(error) => panic!("accept: {error}"),
                }
            }
            for connection in connections {
                connection.join().unwrap();
            }
        });
        Self {
            origin,
            stop,
            requests,
            first_request,
            worker: Some(worker),
        }
    }

    fn assert_read_only(&self) {
        let requests = self.requests.lock().unwrap();
        assert!(!requests.is_empty());
        assert!(
            requests
                .iter()
                .all(|request| request.starts_with("GET /api/v1/")),
            "{requests:?}"
        );
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}

fn cli(path: &Path, origin: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_enumeray"));
    command
        .args(["--api", origin, "--config"])
        .arg(path.join("profiles.json"))
        .arg("--json")
        .env("ENUMERAY_TOKEN", "private-test-token")
        .current_dir(path);
    command
}

fn private_json(path: &Path, value: &Value) {
    std::fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn browser_profile(path: &Path, origin: &str, renew_after: u64) {
    private_json(
        &path.join("profiles.json"),
        &json!({
            "credential_epochs":{"default":"11111111-1111-4111-8111-111111111111"},
            "profiles":{"default":{
                "api_url":origin,"access_token":"before-renewal","refresh_token":"refresh-before",
                "expires_at":renew_after + 1,
                "oauth":{"user_id":USER,"issuer":"https://identity.example/realms/test",
                    "client_id":"enumeray-cli","audience":"enumeray-api",
                    "token_endpoint":format!("{origin}token"),"renew_after":renew_after,
                    "renewal":{"status":"ready"}}
            }}
        }),
    );
}

fn oauth_config(origin: &str) -> Value {
    json!({"issuer":"https://identity.example/realms/test","client_id":"enumeray-cli",
        "audience":"enumeray-api","authorization_endpoint":"https://identity.example/authorize",
        "token_endpoint":format!("{origin}token"),"scopes":["openid","offline_access"]})
}

#[test]
fn browser_log_renewal_reconnects_the_original_stream_from_its_processed_cursor() {
    let directory = tempfile::tempdir().unwrap();
    let origin = Arc::new(Mutex::new(String::new()));
    let streams = Arc::new(AtomicUsize::new(0));
    let exchanges = Arc::new(AtomicUsize::new(0));
    let (api, connected, refreshed) = (origin.clone(), streams.clone(), exchanges.clone());
    let server = Server::new(move |request| {
        if request.starts_with("POST /token ") {
            refreshed.fetch_add(1, Ordering::SeqCst);
            Reply::json(
                json!({"access_token":"after-renewal","refresh_token":"refresh-after","token_type":"Bearer","expires_in":300}),
            )
        } else if request.contains(" /api/v1/auth/config ") {
            Reply::json(oauth_config(&api.lock().unwrap()))
        } else if request.contains(" /api/v1/me ") {
            Reply::json(json!({"user_id":USER,"label":"Observer"}))
        } else if request.contains("/logs/stream ") {
            let number = connected.fetch_add(1, Ordering::SeqCst);
            if number == 0 {
                Reply::sse(frame(1), 30)
            } else {
                Reply::sse(frame(2), 30)
            }
        } else if request.contains("/logs?") {
            Reply::json(if request.contains("cursor=c2") {
                page(&[], "c2")
            } else {
                page(&[2], "c2")
            })
        } else {
            Reply::json(run(if connected.load(Ordering::SeqCst) >= 2 {
                "succeeded"
            } else {
                "running"
            }))
        }
    });
    *origin.lock().unwrap() = server.origin.clone();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    browser_profile(directory.path(), &server.origin, now + 6);
    let output = cli(directory.path(), &server.origin)
        .env_remove("ENUMERAY_TOKEN")
        .args([
            "run",
            "logs",
            "run-one",
            "--follow",
            "--interval",
            "1",
            "--deadline",
            "15",
            "--state",
        ])
        .arg(directory.path().join("logs.json"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(positions(&output), vec![1, 2]);
    assert_eq!(exchanges.load(Ordering::SeqCst), 1);
    assert_eq!(streams.load(Ordering::SeqCst), 2);
    let recorded = server.requests.lock().unwrap();
    let handshakes: Vec<_> = recorded
        .iter()
        .filter(|r| r.contains("/logs/stream "))
        .collect();
    assert!(handshakes[0].contains("Bearer before-renewal"));
    assert!(handshakes[1].contains("Bearer after-renewal"));
    assert!(
        handshakes[1]
            .to_ascii_lowercase()
            .contains("last-event-id: c1")
    );
    drop(recorded);
    let saved: Value =
        serde_json::from_slice(&std::fs::read(directory.path().join("logs.json")).unwrap())
            .unwrap();
    assert_eq!(saved["cursor"], "c2");
    assert!(
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|r| r.starts_with("GET ") || r.starts_with("POST /token "))
    );
}

#[cfg(unix)]
#[test]
fn cancellation_during_browser_renewal_exits_promptly_without_business_dispatch_or_reuse() {
    for interrupt in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let origin = Arc::new(Mutex::new(String::new()));
        let started = Arc::new(AtomicBool::new(false));
        let (api, exchanging) = (origin.clone(), started.clone());
        let server = Server::new(move |request| {
            if request.contains(" /api/v1/auth/config ") {
                Reply::json(oauth_config(&api.lock().unwrap()))
            } else if request.starts_with("POST /token ") {
                exchanging.store(true, Ordering::SeqCst);
                let mut reply = Reply::json(
                    json!({"access_token":"after-renewal","refresh_token":"refresh-after","token_type":"Bearer","expires_in":300}),
                );
                reply.delay = Duration::from_secs(10);
                reply
            } else {
                panic!("Business request dispatched during cancelled renewal: {request}");
            }
        });
        *origin.lock().unwrap() = server.origin.clone();
        browser_profile(directory.path(), &server.origin, 1);
        let mut child = cli(directory.path(), &server.origin)
            .env_remove("ENUMERAY_TOKEN")
            .args([
                "run",
                "wait",
                "run-one",
                "--check-outcome",
                "--deadline",
                if interrupt { "15" } else { "5" },
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let until = Instant::now() + Duration::from_secs(8);
        while !started.load(Ordering::SeqCst) {
            if Instant::now() >= until {
                child.kill().unwrap();
                let output = child.wait_with_output().unwrap();
                panic!(
                    "Renewal did not start: {}; requests: {:?}",
                    String::from_utf8_lossy(&output.stderr),
                    server.requests.lock().unwrap()
                );
            }
            thread::sleep(Duration::from_millis(10));
        }
        if interrupt {
            assert!(
                Command::new("/bin/kill")
                    .args(["-INT", &child.id().to_string()])
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let before = Instant::now();
        let output = child.wait_with_output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(if interrupt { 130 } else { 5 }),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            before.elapsed()
                < if interrupt {
                    Duration::from_millis(1500)
                } else {
                    Duration::from_secs(6)
                }
        );
        let output = cli(directory.path(), &server.origin)
            .env_remove("ENUMERAY_TOKEN")
            .args(["auth", "whoami"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("session_renewal_uncertain"));
        assert_eq!(
            server
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.starts_with("POST /token "))
                .count(),
            1
        );
    }
}

/// A complete accepted scalar operation; observation must not need init or submission HTTP.
fn accepted_analysis(path: &Path, origin: &str) {
    let root = path.join("analysis");
    if root.exists() {
        return;
    }
    for relative in ["", "state", "state/uploads", "results", "results/files"] {
        let directory = root.join(relative);
        std::fs::create_dir(&directory).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    let definition = enumeray_contracts::models::parse_publication(include_bytes!(
        "../examples/scalar-publication.json"
    ))
    .unwrap()
    .definition;
    let publication = json!({"publication_id":"publication-one","definition":definition,"execution_location":"Local test","disclosure":"Test execution"});
    let inputs = json!({"value":2});
    let key = "55555555-5555-4555-8555-555555555555";
    let prepared = json!({"schema_version":1,"api_url":origin,"user_id":USER,"idempotency_key":key,"request":{"model_id":"scalar-double","expected_publication_id":"publication-one","inputs":inputs}});
    let receipt = json!({"purpose":"standard","run_id":ANALYSIS_RUN,"model_id":"scalar-double","publication_id":"publication-one","state":"queued","created_at":"2026-10-19T00:00:00Z"});
    let saved = json!({"schema_version":1,"api_url":origin,"user_id":USER,"model_id":"scalar-double","publication_id":"publication-one","publication":publication,"bindings":{},"run_key":key,"frozen_inputs":inputs,"input_artifacts":{},"uploads":{},"prepared":prepared,"submission_started":true,"receipt":receipt});
    for (relative, value) in [
        ("analysis.json", saved),
        ("inputs.json", inputs.clone()),
        ("state/publication.json", publication),
        ("state/frozen-inputs.json", inputs),
        ("state/prepared-run.json", prepared),
        ("state/receipt.json", receipt),
    ] {
        private_json(&root.join(relative), &value);
    }
}

fn follow_cli(path: &Path, origin: &str, folder: bool) -> Command {
    let mut command = cli(path, origin);
    if folder {
        accepted_analysis(path, origin);
        command.args(["analysis", "logs", "analysis", "--follow"]);
    } else {
        command.args([
            "run",
            "logs",
            "run-one",
            "--follow",
            "--state",
            "watch.json",
        ]);
    }
    command
}

fn checkpoint(path: &Path, folder: bool) -> std::path::PathBuf {
    path.join(if folder {
        "analysis/state/logs.json"
    } else {
        "watch.json"
    })
}

fn run_for(state: &str, folder: bool) -> Value {
    let mut value = run(state);
    if folder {
        value["run_id"] = json!(ANALYSIS_RUN);
        value["model_id"] = json!("scalar-double");
        value["publication_id"] = json!("publication-one");
        value["created_by_user_id"] = json!(USER);
        value["requested_inputs"] = json!({"value":2});
        value["effective_inputs"] = json!({"value":2});
        value["execution_control"] = json!({"state":if matches!(state,"succeeded"|"failed"|"cancelled") {"completed"} else {"authorized"},"authorization_reason":null});
    }
    value
}

fn run(state: &str) -> Value {
    json!({"run_id":"run-one","state":state,"phase":if state == "running" { Some("executing") } else { None },"observation_freshness":"fresh","last_observed_at":"now"})
}
fn entry(position: i64) -> Value {
    json!({"position":position,"assignment_id":ASSIGNMENT,"source":"stdout","epoch":1,"sequence":position,
    "offset":position - 1,"byte_length":1,"text":format!("line-{position}\u{1b}[2J"),"gap":null,"source_reset":false,"received_at":"now"})
}
fn frame(position: i64) -> String {
    format!("event: log\nid: c{position}\ndata: {}\n\n", entry(position))
}
fn boundary() -> Value {
    json!({"earliest_position":5,"latest_position":6,"recovery_cursor":"c4","payload_max_age_seconds":604800,"payload_max_entries":1024})
}
fn page(positions: &[i64], cursor: &str) -> Value {
    json!({"items":positions.iter().map(|n| entry(*n)).collect::<Vec<_>>(),"next_cursor":null,
    "resume_cursor":cursor,"retention":{"earliest_position":1,"latest_position":positions.last().copied().unwrap_or(0),"recovery_cursor":"c0","payload_max_age_seconds":604800,"payload_max_entries":1024}})
}
fn records(output: &Output) -> Vec<Value> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn positions(output: &Output) -> Vec<i64> {
    records(output)
        .iter()
        .filter(|v| v["event"] == "log")
        .map(|v| v["data"]["position"].as_i64().unwrap())
        .collect()
}

#[test]
fn terminal_wait_preserves_json_and_default_exits_and_checks_outcomes_explicitly() {
    let directory = tempfile::tempdir().unwrap();
    for (state, checked) in [("succeeded", 0), ("failed", 3), ("cancelled", 4)] {
        let value = run(state);
        let response = value.clone();
        let server = Server::new(move |_| Reply::json(response.clone()));
        for check in [false, true] {
            let mut command = cli(directory.path(), &server.origin);
            command.args(["run", "wait", "run-one"]);
            if check {
                command.arg("--check-outcome");
            }
            let output = command.output().unwrap();
            assert_eq!(
                output.status.code(),
                Some(if check { checked } else { 0 }),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                serde_json::from_slice::<Value>(&output.stdout).unwrap(),
                value
            );
        }
        server.assert_read_only();
    }
}

#[test]
fn phase_only_and_freshness_changes_are_reported_without_timestamp_noise() {
    let directory = tempfile::tempdir().unwrap();
    let count = AtomicUsize::new(0);
    let server = Server::new(move |_| {
        let n = count.fetch_add(1, Ordering::SeqCst);
        let mut value = run(if n == 4 { "succeeded" } else { "running" });
        value["last_observed_at"] = json!(format!("time-{n}"));
        value["phase"] = json!(if n < 2 { "preparing" } else { "executing" });
        if n == 3 {
            value["observation_freshness"] = json!("stale");
        }
        Reply::json(value)
    });
    let output = cli(directory.path(), &server.origin)
        .args(["run", "wait", "run-one", "--interval", "1"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.matches("Run run-one:").count(), 4, "{stderr}");
    assert!(
        stderr.contains("Phase: preparing")
            && stderr.contains("Phase: executing")
            && stderr.contains("Observation freshness: stale")
    );
    server.assert_read_only();
}

#[test]
fn deadlines_bound_idle_requests_and_streams_without_mutation() {
    for folder in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let server = Server::new(move |request| {
            if request.contains(" /api/v1/me ") {
                return Reply::json(json!({"user_id":USER,"label":"User"}));
            }
            if request.contains("/logs/stream") {
                return Reply::sse(": keepalive\n\n".into(), 10);
            }
            let mut reply = Reply::json(run_for("running", folder));
            reply.delay = Duration::from_secs(3);
            reply
        });
        for (kind, check, expected) in [("wait", false, 1), ("wait", true, 5), ("logs", false, 5)] {
            *server.first_request.lock().unwrap() = None;
            let mut command = cli(directory.path(), &server.origin);
            if folder {
                accepted_analysis(directory.path(), &server.origin);
                command.args(["analysis", kind, "analysis", "--deadline", "1"]);
            } else {
                command.args(["run", kind, "run-one", "--deadline", "1"]);
            }
            if kind == "logs" {
                command.arg("--follow");
            }
            if check {
                command.arg("--check-outcome");
            }
            let output = command.output().unwrap();
            assert_eq!(
                output.status.code(),
                Some(expected),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let elapsed = server.first_request.lock().unwrap().unwrap().elapsed();
            assert!(
                elapsed < Duration::from_secs(2),
                "folder={folder}, kind={kind}: {elapsed:?}"
            );
            assert!(output.stdout.is_empty());
            assert!(String::from_utf8_lossy(&output.stderr).contains(if folder {
                "analysis status analysis"
            } else {
                "same run ID"
            }));
        }
        server.assert_read_only();
    }
}

#[test]
fn folder_observation_deadline_includes_initial_account_discovery() {
    for (kind, expected) in [("wait", 1), ("logs", 5)] {
        let directory = tempfile::tempdir().unwrap();
        let server = Server::new(|request| {
            assert!(request.contains(" /api/v1/me "), "{request}");
            let mut reply = Reply::json(json!({"user_id":USER,"label":"User"}));
            reply.delay = Duration::from_secs(10);
            reply
        });
        accepted_analysis(directory.path(), &server.origin);
        let journal = std::fs::read(directory.path().join("analysis/analysis.json")).unwrap();
        let mut command = cli(directory.path(), &server.origin);
        command.args(["analysis", kind, "analysis", "--deadline", "1"]);
        if kind == "logs" {
            command.arg("--follow");
        }
        let output = command.output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(expected),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let elapsed = server.first_request.lock().unwrap().unwrap().elapsed();
        assert!(elapsed < Duration::from_secs(2), "{kind}: {elapsed:?}");
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("analysis status analysis"));
        assert_eq!(
            std::fs::read(directory.path().join("analysis/analysis.json")).unwrap(),
            journal
        );
        assert!(!checkpoint(directory.path(), true).exists());
        assert!(
            !directory
                .path()
                .join("analysis/state/last-run.json")
                .exists()
        );
        server.assert_read_only();
    }
}

#[test]
fn folder_wait_and_follow_reject_conflicting_inspection_identity_before_progress_or_output() {
    for (field, changed) in [
        ("run_id", json!(USER)),
        ("model_id", json!("another-model")),
        ("publication_id", json!("another-publication")),
        ("created_by_user_id", json!(ANALYSIS_RUN)),
        ("requested_inputs", json!({"value":3})),
        ("effective_inputs", json!({"value":3})),
    ] {
        for kind in ["wait", "logs"] {
            let directory = tempfile::tempdir().unwrap();
            let changed = changed.clone();
            let server = Server::new(move |request| {
                if request.contains(" /api/v1/me ") {
                    Reply::json(json!({"user_id":USER,"label":"User"}))
                } else if request.contains("/logs/stream") {
                    Reply::sse(": keepalive\n\n".into(), 10)
                } else {
                    let mut value = run_for("running", true);
                    value[field] = changed.clone();
                    Reply::json(value)
                }
            });
            accepted_analysis(directory.path(), &server.origin);
            let journal = std::fs::read(directory.path().join("analysis/analysis.json")).unwrap();
            let mut command = cli(directory.path(), &server.origin);
            command.args(["analysis", kind, "analysis", "--deadline", "2"]);
            if kind == "logs" {
                command.arg("--follow");
            }
            let output = command.output().unwrap();
            assert_eq!(
                output.status.code(),
                Some(1),
                "{field}/{kind}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(output.stdout.is_empty());
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains(
                    "Run inspection conflicts with the saved analysis identity or inputs"
                ),
                "{field}/{kind}: {stderr}"
            );
            assert!(!stderr.contains("Run 44444444"), "{stderr}");
            assert_eq!(
                std::fs::read(directory.path().join("analysis/analysis.json")).unwrap(),
                journal
            );
            assert!(
                !directory
                    .path()
                    .join("analysis/state/last-run.json")
                    .exists()
            );
            server.assert_read_only();
        }
    }
}

#[test]
fn follow_finishes_for_every_terminal_outcome_after_retained_log_drain() {
    for folder in [false, true] {
        for state in ["succeeded", "failed", "cancelled"] {
            let directory = tempfile::tempdir().unwrap();
            let server = Server::new(move |request| {
                if request.contains(" /api/v1/me ") {
                    Reply::json(json!({"user_id":USER,"label":"User"}))
                } else if request.contains("/logs/stream") {
                    Reply::sse(": keepalive\n\n".into(), 10)
                } else if request.contains("/logs?") {
                    Reply::json(page(&[1, 2], "c2"))
                } else {
                    Reply::json(run_for(state, folder))
                }
            });
            let output = follow_cli(directory.path(), &server.origin, folder)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(positions(&output), vec![1, 2]);
            let records = records(&output);
            assert_eq!(records.last().unwrap()["event"], "run");
            assert_eq!(records.last().unwrap()["data"], run_for(state, folder));
            server.assert_read_only();
        }
    }
}

#[test]
fn named_sources_follow_and_resume_the_existing_checkpoint() {
    for folder in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let server = Server::new(move |request| {
            if request.contains(" /api/v1/me ") {
                Reply::json(json!({"user_id": USER, "label":"User"}))
            } else if request.contains("/logs/stream") {
                Reply::sse(": keepalive\n\n".into(), 10)
            } else if request.contains("/logs?") {
                let mut value = if request.contains("cursor=c2") {
                    page(&[], "c2")
                } else {
                    page(&[1, 2], "c2")
                };
                value["retention"]["latest_position"] = json!(2);
                for entry in value["items"].as_array_mut().unwrap() {
                    entry["source"] = json!("model.solver");
                }
                Reply::json(value)
            } else {
                Reply::json(run_for("succeeded", folder))
            }
        });
        let execute = || {
            follow_cli(directory.path(), &server.origin, folder)
                .output()
                .unwrap()
        };
        let first = execute();
        assert!(
            first.status.success(),
            "{}",
            String::from_utf8_lossy(&first.stderr)
        );
        assert_eq!(positions(&first), vec![1, 2]);
        assert!(
            records(&first)
                .iter()
                .filter(|v| v["event"] == "log")
                .all(|v| v["data"]["source"] == "model.solver")
        );
        let resumed = execute();
        assert!(
            resumed.status.success(),
            "{}",
            String::from_utf8_lossy(&resumed.stderr)
        );
        assert!(positions(&resumed).is_empty());
        server.assert_read_only();
    }
}

#[test]
fn disconnected_stream_resumes_processed_cursor_deduplicates_and_drains_final_page() {
    let directory = tempfile::tempdir().unwrap();
    let streams = Arc::new(AtomicUsize::new(0));
    let observed = streams.clone();
    let server = Server::new(move |request| {
        if request.contains("/logs/stream") {
            let n = observed.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Reply::sse(
                    format!("{}event: log\nid: unfinished\ndata: {{", frame(1)),
                    0,
                )
            } else {
                assert!(request.to_ascii_lowercase().contains("last-event-id: c1"));
                Reply::sse(format!("{}{}", frame(1), frame(2)), 10)
            }
        } else if request.contains("/logs?") {
            // Terminal polling can win before the reconnected stream's second entry
            // is consumed. Drain from the last processed cursor in either case.
            assert!(request.contains("cursor=c1") || request.contains("cursor=c2"));
            Reply::json(page(&[2, 3], "c3"))
        } else {
            Reply::json(run(if observed.load(Ordering::SeqCst) > 1 {
                "succeeded"
            } else {
                "running"
            }))
        }
    });
    let output = cli(directory.path(), &server.origin)
        .args([
            "run",
            "logs",
            "run-one",
            "--follow",
            "--interval",
            "1",
            "--deadline",
            "8",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(positions(&output), vec![1, 2, 3]);
    assert!(streams.load(Ordering::SeqCst) >= 2);
    server.assert_read_only();
}

#[test]
fn token_expiry_checkpoint_can_resume_after_explicit_renewal() {
    for folder in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let stage = Arc::new(AtomicBool::new(false));
        let renewed = stage.clone();
        let server = Server::new(move |request| {
            if request.contains(" /api/v1/me ") {
                Reply::json(json!({"user_id":USER,"label":"User"}))
            } else if request.contains("/logs/stream") {
                if !renewed.load(Ordering::SeqCst) {
                    Reply::sse(
                        format!(
                            "{}event: auth_revoked\ndata: {{\"code\":\"auth_revoked\"}}\n\n",
                            frame(1)
                        ),
                        0,
                    )
                } else {
                    assert!(request.to_ascii_lowercase().contains("last-event-id: c1"));
                    Reply::sse(": keepalive\n\n".into(), 10)
                }
            } else if request.contains("/logs?") {
                Reply::json(page(&[1, 2], "c2"))
            } else {
                Reply::json(run_for(
                    if renewed.load(Ordering::SeqCst) {
                        "succeeded"
                    } else {
                        "running"
                    },
                    folder,
                ))
            }
        });
        let output = follow_cli(directory.path(), &server.origin, folder)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(positions(&output), vec![1]);
        assert!(String::from_utf8_lossy(&output.stderr).contains("auth refresh/login"));
        let state: Value =
            serde_json::from_slice(&std::fs::read(checkpoint(directory.path(), folder)).unwrap())
                .unwrap();
        assert_eq!(state["cursor"], "c1");
        assert!(!state.to_string().contains("private-test-token"));
        stage.store(true, Ordering::SeqCst);
        let output = follow_cli(directory.path(), &server.origin, folder)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(positions(&output), vec![2]);
        server.assert_read_only();
    }
}

#[test]
fn retention_gaps_require_explicit_acknowledgement_for_http_and_open_stream() {
    for folder in [false, true] {
        for http in [true, false] {
            for allow in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let streams = AtomicUsize::new(0);
                let inspections = AtomicUsize::new(0);
                let server = Server::new(move |request| {
                    if request.contains(" /api/v1/me ") {
                        Reply::json(json!({"user_id":USER,"label":"User"}))
                    } else if request.contains("/logs/stream") {
                        let n = streams.fetch_add(1, Ordering::SeqCst);
                        if n == 0 {
                            if http {
                                let mut reply = Reply::json(
                                    json!({"code":"history_cursor_expired","message":"expired"}),
                                );
                                reply.status = 410;
                                reply.headers.push_str("X-History-Recovery-Cursor: c4\r\nX-History-Earliest-Position: 5\r\nX-History-Latest-Position: 6\r\n");
                                reply
                            } else {
                                Reply::sse(
                                    format!(
                                        "event: retention_gap\nid: c4\ndata: {}\n\n{}",
                                        boundary(),
                                        frame(5)
                                    ),
                                    10,
                                )
                            }
                        } else {
                            assert!(request.to_ascii_lowercase().contains("last-event-id: c4"));
                            Reply::sse(frame(5), 10)
                        }
                    } else if request.contains("/logs?") {
                        Reply::json(page(&[5, 6], "c6"))
                    } else {
                        let n = streams.load(Ordering::SeqCst);
                        Reply::json(run_for(
                            if inspections.fetch_add(1, Ordering::SeqCst) > 0
                                && allow
                                && (n > 1 || !http && n > 0)
                            {
                                "succeeded"
                            } else {
                                "running"
                            },
                            folder,
                        ))
                    }
                });
                let mut command = follow_cli(directory.path(), &server.origin, folder);
                private_json(
                    &checkpoint(directory.path(), folder),
                    &json!({"schema_version":1,"api_url":server.origin,"user_id":USER,"run_id":if folder {ANALYSIS_RUN} else {"run-one"},"cursor":"old","last_position":0}),
                );
                command.args(["--interval", "1", "--deadline", "8"]);
                if allow {
                    command.arg("--allow-gaps");
                }
                let output = command.output().unwrap();
                assert_eq!(
                    output.status.success(),
                    allow,
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let records = records(&output);
                assert_eq!(
                    records
                        .iter()
                        .filter(|v| v["event"] == "retention_gap")
                        .count(),
                    1
                );
                assert_eq!(positions(&output), if allow { vec![5, 6] } else { vec![] });
                server.assert_read_only();
            }
        }
    }
}

#[test]
fn follow_options_and_unsafe_combinations_are_rejected_before_requests() {
    let directory = tempfile::tempdir().unwrap();
    for options in [
        vec!["--state", "watch.json"],
        vec!["--interval", "1"],
        vec!["--deadline", "1"],
        vec!["--allow-gaps"],
        vec!["--follow", "--limit", "1"],
        vec!["--follow", "--state", "watch.json", "--cursor", "c1"],
        vec!["--follow", "--deadline", "0"],
    ] {
        let output = cli(directory.path(), "http://127.0.0.1:1/")
            .args(["run", "logs", "run-one"])
            .args(options)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
    }
}

#[test]
fn retry_after_shutdown_and_source_unavailability_reconnect_without_losing_control_events() {
    let directory = tempfile::tempdir().unwrap();
    let starts = Arc::new(Mutex::new(Vec::<Instant>::new()));
    let observed = starts.clone();
    let server = Server::new(move |request| {
        if request.contains("/logs/stream") {
            let mut starts = observed.lock().unwrap();
            starts.push(Instant::now());
            match starts.len() {
                1 => {
                    let mut reply =
                        Reply::json(json!({"code":"log_stream_limit","message":"retry"}));
                    reply.status = 429;
                    reply.headers.push_str("Retry-After: 1\r\n");
                    reply
                }
                2 => Reply::sse(
                    "event: shutdown\ndata: {\"code\":\"shutdown\"}\n\n".into(),
                    0,
                ),
                3 => Reply::sse(
                    format!(
                        "{}event: source_unavailable\ndata: {{\"code\":\"source_unavailable\"}}\n\n",
                        frame(1)
                    ),
                    0,
                ),
                _ => Reply::sse(": keepalive\n\n".into(), 10),
            }
        } else if request.contains("/logs?") {
            Reply::json(page(&[1], "c1"))
        } else {
            Reply::json(run(if observed.lock().unwrap().len() >= 4 {
                "succeeded"
            } else {
                "running"
            }))
        }
    });
    let output = cli(directory.path(), &server.origin)
        .args([
            "run",
            "logs",
            "run-one",
            "--follow",
            "--interval",
            "1",
            "--deadline",
            "10",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(positions(&output), vec![1]);
    let events = records(&output);
    for name in ["shutdown", "source_unavailable"] {
        assert!(events.iter().any(|event| event["event"] == name));
    }
    let starts = starts.lock().unwrap();
    assert!(starts[1].duration_since(starts[0]) >= Duration::from_millis(950));
    server.assert_read_only();
}

#[test]
fn authentication_failures_and_redirects_stop_without_following_or_overwriting_state() {
    let directory = tempfile::tempdir().unwrap();
    for status in [401, 403, 302] {
        let server = Server::new(move |request| {
            if request.contains("/logs/stream") {
                let mut reply = Reply::json(
                    json!({"code":"read_denied","message":"denied","request_id":"read-request"}),
                );
                reply.status = status;
                reply
                    .headers
                    .push_str("Location: http://127.0.0.1:1/steal\r\n");
                reply
            } else {
                Reply::json(run("running"))
            }
        });
        let output = cli(directory.path(), &server.origin)
            .args(["run", "logs", "run-one", "--follow", "--deadline", "2"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8(output.stderr).unwrap();
        let error: Value = serde_json::from_str(stderr.lines().last().unwrap()).unwrap();
        assert_eq!(error["status"], status);
        assert_eq!(error["error"]["request_id"], "read-request");
        assert_eq!(
            server
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|request| request.contains("/logs/stream"))
                .count(),
            1
        );
        assert!(!stderr.contains("private-test-token"));
        server.assert_read_only();
    }
}

#[test]
fn final_drain_paginates_and_reports_implicit_retention_gaps() {
    let directory = tempfile::tempdir().unwrap();
    for gap in [false, true] {
        let server = Server::new(move |request| {
            if request.contains("/logs/stream") {
                Reply::sse(": keepalive\n\n".into(), 10)
            } else if request.contains("/logs?") {
                let mut result = if request.contains("cursor=") {
                    page(&[2], "c2")
                } else {
                    page(&[1], "c1")
                };
                if !request.contains("cursor=") {
                    result["next_cursor"] = json!("c1");
                }
                if gap {
                    result["retention"] = boundary();
                }
                Reply::json(result)
            } else {
                Reply::json(run("succeeded"))
            }
        });
        let output = cli(directory.path(), &server.origin)
            .args(["run", "logs", "run-one", "--follow"])
            .output()
            .unwrap();
        assert_eq!(
            output.status.success(),
            !gap,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if gap {
            assert!(positions(&output).is_empty());
            assert_eq!(records(&output)[0]["event"], "retention_gap");
        } else {
            assert_eq!(positions(&output), vec![1, 2]);
            assert_eq!(records(&output)[1]["id"], "c2");
        }
        server.assert_read_only();
    }
}

#[test]
fn enormous_intervals_are_capped_and_unrepresentable_deadlines_fail_without_panic() {
    let directory = tempfile::tempdir().unwrap();
    let server = Server::new(|_| Reply::json(run("running")));
    let output = cli(directory.path(), &server.origin)
        .args([
            "run",
            "wait",
            "run-one",
            "--check-outcome",
            "--interval",
            "18446744073709551615",
            "--deadline",
            "1",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(5));
    let output = cli(directory.path(), &server.origin)
        .args([
            "run",
            "wait",
            "run-one",
            "--deadline",
            "18446744073709551615",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("too large"));
}

#[cfg(unix)]
#[test]
fn folder_ctrl_c_interrupts_initial_account_discovery_without_creating_checkpoints() {
    for kind in ["wait", "logs"] {
        let directory = tempfile::tempdir().unwrap();
        let server = Server::new(|request| {
            assert!(request.contains(" /api/v1/me "), "{request}");
            let mut reply = Reply::json(json!({"user_id":USER,"label":"User"}));
            reply.delay = Duration::from_secs(10);
            reply
        });
        accepted_analysis(directory.path(), &server.origin);
        let journal = std::fs::read(directory.path().join("analysis/analysis.json")).unwrap();
        let mut command = cli(directory.path(), &server.origin);
        command.args(["analysis", kind, "analysis"]);
        if kind == "logs" {
            command.arg("--follow");
        }
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        while server.requests.lock().unwrap().is_empty() {
            if Instant::now() >= until {
                child.kill().unwrap();
                let output = child.wait_with_output().unwrap();
                panic!(
                    "{kind} account discovery did not start: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            Command::new("/bin/kill")
                .args(["-INT", &child.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        let before = Instant::now();
        let output = child.wait_with_output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(130),
            "{kind}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(before.elapsed() < Duration::from_secs(1));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("analysis status analysis"));
        assert_eq!(
            std::fs::read(directory.path().join("analysis/analysis.json")).unwrap(),
            journal
        );
        assert!(!checkpoint(directory.path(), true).exists());
        assert!(
            !directory
                .path()
                .join("analysis/state/last-run.json")
                .exists()
        );
        server.assert_read_only();
    }
}

#[cfg(unix)]
#[test]
fn interrupt_exits_130_and_preserves_checkpoint_while_idle() {
    for folder in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let server = Server::new(move |request| {
            if request.contains(" /api/v1/me ") {
                Reply::json(json!({"user_id":USER,"label":"User"}))
            } else if request.contains("/logs/stream") {
                Reply::sse(frame(1), 10)
            } else {
                Reply::json(run_for("running", folder))
            }
        });
        for kind in ["wait", "logs"] {
            let mut command = if kind == "logs" {
                follow_cli(directory.path(), &server.origin, folder)
            } else {
                let mut command = cli(directory.path(), &server.origin);
                if folder {
                    accepted_analysis(directory.path(), &server.origin);
                    command.args(["analysis", "wait", "analysis"]);
                } else {
                    command.args(["run", "wait", "run-one"]);
                }
                command
            };
            let mut child = command
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let until = Instant::now() + Duration::from_secs(10);
            loop {
                let ready = if kind == "logs" {
                    std::fs::read(checkpoint(directory.path(), folder))
                        .ok()
                        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                        .is_some_and(|v| v["cursor"] == "c1")
                } else {
                    server
                        .requests
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|request| request.contains("/runs/"))
                };
                if ready {
                    break;
                }
                if Instant::now() >= until {
                    child.kill().unwrap();
                    let output = child.wait_with_output().unwrap();
                    panic!(
                        "observer did not start (folder={folder}, kind={kind}): {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                thread::sleep(Duration::from_millis(10));
            }
            assert!(
                Command::new("/bin/kill")
                    .args(["-INT", &child.id().to_string()])
                    .status()
                    .unwrap()
                    .success()
            );
            let before = Instant::now();
            let output = child.wait_with_output().unwrap();
            assert_eq!(
                output.status.code(),
                Some(130),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(before.elapsed() < Duration::from_secs(1));
        }
        server.assert_read_only();
    }
}
