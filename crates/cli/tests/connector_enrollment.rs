//! Enrollment files are published only after a validated secret-bearing response.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const SECRET_BYTES: [u8; 32] = [67; 32];

fn document() -> Value {
    json!({
        "id":"f32ad050-c405-4e56-91ee-c19c57b45ceb",
        "connector_id":"worker",
        "enrollment_token":URL_SAFE_NO_PAD.encode(SECRET_BYTES),
        "expires_at":"2026-10-15 12:00:00.123456+00",
    })
}

enum Reply {
    Json(u16, Value),
    Drop,
    InvalidJson,
    WriteFile(PathBuf, Vec<u8>, Value),
}

struct Server {
    origin: String,
    stop: mpsc::Sender<()>,
    worker: thread::JoinHandle<Vec<String>>,
}

impl Server {
    fn new(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        let (stop, stopping) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut replies = VecDeque::from(replies);
            let mut requests = Vec::new();
            let until = Instant::now() + Duration::from_secs(15);
            while stopping.try_recv().is_err() && Instant::now() < until {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("accept enrollment request: {error}"),
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
                            .map(|length| length.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                let mut body = vec![0; length];
                stream.read_exact(&mut body).unwrap();
                bytes.extend(body);
                requests.push(String::from_utf8(bytes).unwrap());
                let (status, body) = match replies.pop_front().expect("unexpected request") {
                    Reply::Json(status, body) => (status, body.to_string()),
                    Reply::Drop => continue,
                    Reply::InvalidJson => (200, "{secret response was interrupted".into()),
                    Reply::WriteFile(path, bytes, body) => {
                        // Preflight now creates/locks the output parent before issuance.
                        // Model an external replacement after that boundary, retaining its lock inode.
                        if path.is_dir() {
                            fs::rename(&path, path.with_extension("displaced")).unwrap();
                        }
                        fs::write(path, bytes).unwrap();
                        (200, body.to_string())
                    }
                };
                write!(stream, "HTTP/1.1 {status} Test\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
            }
            assert!(
                replies.is_empty(),
                "not all expected enrollment requests were sent"
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

fn call(root: &Path, server: &Server, creation: bool, json: bool, destination: &Path) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_enumeray"));
    command
        .env_remove("ENUMERAY_TOKEN")
        .arg("--config")
        .arg(root.join("profiles.json"))
        .args(["--api", &server.origin]);
    if json {
        command.arg("--json");
    }
    command.args([
        "connector",
        if creation { "create" } else { "enrollment" },
        "worker",
    ]);
    if creation {
        command.args(["--label", "Worker"]);
    }
    command.arg("--output").arg(destination).output().unwrap()
}

fn assert_failure(output: &Output, json: bool) -> String {
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr.clone()).unwrap();
    assert!(!stderr.contains(&URL_SAFE_NO_PAD.encode(SECRET_BYTES)));
    assert!(!stderr.contains("secret response"));
    if json {
        let error: Value = serde_json::from_str(&stderr).unwrap();
        error["error"]["message"].as_str().unwrap().to_owned()
    } else {
        stderr
    }
}

#[test]
fn creation_and_replacement_publish_private_documents_with_compatible_summaries() {
    for creation in [true, false] {
        for json in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let destination = root.path().join("enrollment.json");
            let server = Server::new(vec![Reply::Json(200, document())]);
            let output = call(root.path(), &server, creation, json, &destination);
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(output.stderr.is_empty());
            if json {
                let summary: Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(
                    summary,
                    json!({"saved_to":destination,"connector_id":"worker"})
                );
            } else {
                let stdout = String::from_utf8_lossy(&output.stdout);
                assert!(stdout.contains(&format!("Saved to: {}", destination.display())));
                assert!(stdout.contains("Connector: worker"));
            }
            assert!(
                !String::from_utf8_lossy(&output.stdout)
                    .contains(&URL_SAFE_NO_PAD.encode(SECRET_BYTES))
            );
            assert_eq!(
                serde_json::from_slice::<Value>(&fs::read(&destination).unwrap()).unwrap(),
                document()
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(&destination).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
            let requests = server.finish();
            assert_eq!(requests.len(), 1);
            let route = if creation {
                "/api/v1/connectors"
            } else {
                "/api/v1/connectors/worker/enrollments"
            };
            assert!(requests[0].starts_with(&format!("POST {route} HTTP/1.1")));
        }
    }
}

#[test]
fn replay_and_invalid_documents_are_never_saved_and_inspect_only_once() {
    let mut invalid = vec![
        json!({"connector_id":"worker","already_registered":true}),
        Value::Null,
    ];
    for field in ["id", "connector_id", "enrollment_token", "expires_at"] {
        let mut missing = document();
        missing.as_object_mut().unwrap().remove(field);
        invalid.push(missing);
        let mut wrong_type = document();
        wrong_type[field] = json!(42);
        invalid.push(wrong_type);
    }
    for (field, value) in [
        ("id", json!("00000000-0000-0000-0000-000000000000")),
        ("connector_id", json!("another-worker")),
        ("enrollment_token", json!("")),
        ("enrollment_token", json!(" ".repeat(43))),
        ("enrollment_token", json!("*".repeat(43))),
        ("enrollment_token", json!("A".repeat(42))),
        ("expires_at", json!("  ")),
        ("expires_at", json!("not-a-timestamp")),
        ("expires_at", json!("2026-10-15\nsecret response")),
        ("already_registered", json!(true)),
    ] {
        let mut value_document = document();
        value_document[field] = value;
        invalid.push(value_document);
    }
    invalid.push(json!({"connector_id":"another-worker","already_registered":true}));
    invalid.push(json!({"connector_id":"worker","already_registered":false}));
    for creation in [true, false] {
        for json in [true, false] {
            for value in &invalid {
                let root = tempfile::tempdir().unwrap();
                let destination = root.path().join("enrollment.json");
                let server = Server::new(vec![
                    Reply::Json(200, value.clone()),
                    Reply::Json(200, registered_connector()),
                ]);
                let output = call(root.path(), &server, creation, json, &destination);
                let message = assert_failure(&output, json);
                assert!(message.contains("confirmed that the connector is registered"));
                assert!(message.contains(
                    "enumeray connector enrollment 'worker' --output NEW-ENROLLMENT.json"
                ));
                assert!(!destination.exists());
                let requests = server.finish();
                assert_eq!(requests.len(), 2);
                assert!(requests[1].starts_with("GET /api/v1/connectors/worker HTTP/1.1"));
            }
        }
    }
}

#[test]
fn interrupted_or_failed_requests_inspect_without_repeating_mutations() {
    for creation in [true, false] {
        for json in [true, false] {
            for failure in [
                Reply::Drop,
                Reply::InvalidJson,
                Reply::Json(
                    503,
                    json!({"code":"unavailable","message":URL_SAFE_NO_PAD.encode(SECRET_BYTES),"request_id":"test"}),
                ),
            ] {
                let root = tempfile::tempdir().unwrap();
                let destination = root.path().join("enrollment.json");
                let server = Server::new(vec![failure, Reply::Json(200, registered_connector())]);
                let output = call(root.path(), &server, creation, json, &destination);
                let message = assert_failure(&output, json);
                assert!(message.contains("confirmed that the connector is registered"));
                assert!(message.contains("same durable identity directory"));
                assert!(message.contains("original execution journal"));
                assert!(!destination.exists());
                assert_eq!(server.finish().len(), 2);
            }
        }
    }
}

#[test]
fn failed_or_mismatched_inspection_leaves_outcome_unknown() {
    for json in [true, false] {
        for inspection in [
            Reply::Drop,
            Reply::Json(200, json!({"connector_id":"other"})),
            Reply::Json(
                404,
                json!({"code":"not_found","message":"Not found","request_id":"test"}),
            ),
            Reply::Json(
                403,
                json!({"code":"forbidden","message":"Forbidden","request_id":"test"}),
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let destination = root.path().join("enrollment.json");
            let server = Server::new(vec![Reply::Drop, inspection]);
            let output = call(root.path(), &server, true, json, &destination);
            let message = assert_failure(&output, json);
            assert!(message.contains("outcome remains unknown"));
            assert!(message.contains("same connector ID and settings"));
            assert!(message.contains("enumeray connector get"));
            assert!(!message.contains("enumeray connector enrollment"));
            assert!(!destination.exists());
            assert_eq!(server.finish().len(), 2);
        }
    }
}

#[test]
fn replay_registration_evidence_is_retained_when_inspection_fails() {
    for json in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("enrollment.json");
        let server = Server::new(vec![
            Reply::Json(
                200,
                json!({"connector_id":"worker","already_registered":true}),
            ),
            Reply::Drop,
        ]);
        let output = call(root.path(), &server, true, json, &destination);
        let message = assert_failure(&output, json);
        assert!(message.contains("response confirmed registration"));
        assert!(message.contains("inspection did not confirm the connector's current state"));
        assert!(message.contains("enumeray connector enrollment"));
        assert!(!destination.exists());
        assert_eq!(server.finish().len(), 2);
    }
}

#[test]
fn definitive_api_failures_keep_the_original_error_without_inspection() {
    for status in [400, 401, 403, 404, 409, 429] {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("enrollment.json");
        let failure = json!({"code":"original_code","message":"Original safe message","request_id":"original-request"});
        let server = Server::new(vec![Reply::Json(status, failure.clone())]);
        let output = call(root.path(), &server, true, true, &destination);
        assert_failure(&output, true);
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["status"], status);
        assert_eq!(error["error"], failure);
        assert!(!destination.exists());
        assert_eq!(server.finish().len(), 1);
    }
}

#[test]
fn existing_files_directories_and_symlinks_are_preserved_without_a_request() {
    for creation in [true, false] {
        for json in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let saved = root.path().join("saved.json");
            let original = serde_json::to_vec(&document()).unwrap();
            fs::write(&saved, &original).unwrap();
            let invalid = root.path().join("invalid.json");
            fs::write(&invalid, b"previous invalid enrollment").unwrap();
            let mut paths = vec![saved.clone(), invalid.clone(), root.path().to_path_buf()];
            #[cfg(unix)]
            {
                use std::os::unix::fs::symlink;
                let link = root.path().join("link.json");
                symlink(&saved, &link).unwrap();
                let dangling = root.path().join("dangling.json");
                symlink(root.path().join("absent.json"), &dangling).unwrap();
                paths.extend([link, dangling]);
            }
            for destination in paths {
                let server = Server::new(vec![]);
                let output = call(root.path(), &server, creation, json, &destination);
                assert!(assert_failure(&output, json).contains("already exists"));
                assert!(server.finish().is_empty());
                assert!(fs::symlink_metadata(&destination).is_ok());
            }
            assert_eq!(fs::read(saved).unwrap(), original);
            assert_eq!(fs::read(invalid).unwrap(), b"previous invalid enrollment");
        }
    }
}

#[test]
fn save_failures_and_destination_races_never_clobber_or_claim_success() {
    for creation in [true, false] {
        for json in [true, false] {
            for blocked_parent in [true, false] {
                let root = tempfile::tempdir().unwrap();
                let raced_path = root.path().join("raced");
                let destination = if blocked_parent {
                    raced_path.join("enrollment.json")
                } else {
                    raced_path.clone()
                };
                let server = Server::new(vec![
                    Reply::WriteFile(
                        raced_path.clone(),
                        b"retain this evidence".to_vec(),
                        document(),
                    ),
                    Reply::Json(200, registered_connector()),
                ]);
                let output = call(root.path(), &server, creation, json, &destination);
                let message = assert_failure(&output, json);
                assert!(message.contains("saving was not confirmed"));
                assert!(
                    message.contains("output may already exist; preserve and inspect it privately")
                );
                assert_eq!(fs::read(raced_path).unwrap(), b"retain this evidence");
                assert_eq!(server.finish().len(), 2);
            }
        }
    }
}

fn registered_connector() -> Value {
    json!({"connector_id":"worker","owner_id":"11111111-1111-4111-8111-111111111111","ownership_generation":1,"archived_at":null,"label":"Worker","generation":1,"observed_generation":1,"draining":false,"authority_withdrawn":false,"max_outstanding_assignments":4,"secret":URL_SAFE_NO_PAD.encode(SECRET_BYTES)})
}
