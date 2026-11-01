//! Saved releases across real process/HTTP boundaries, including uncertain replies.
mod automation_support;
use enumeray_contracts::{
    execution::{InstallationIdentity, InstallationSpec},
    models::ScientificDefinition,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    process::{Command, Output},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

const USER: &str = "22222222-2222-4222-8222-222222222222";
const GRANT: &str = "33333333-3333-4333-8333-333333333333";

#[test]
fn automation_release_transcripts_cover_prepared_ready_reviewed_and_blocked() {
    use enumeray_contracts::automation::ReleaseSummary;
    let root = tempfile::tempdir().unwrap();
    let server = Server::new();
    server.init(root.path());
    let candidate = server.candidate(root.path());
    let operation = serde_json::from_slice::<Value>(
        &fs::read(root.path().join("release/release.json")).unwrap(),
    )
    .unwrap()["operation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    for (name, args) in [
        (
            "release-prepared.json",
            vec!["model", "release", "status", "release"],
        ),
        (
            "release-ready.json",
            vec!["model", "release", "resume", "release"],
        ),
        (
            "release-reviewed.json",
            vec!["model", "release", "review", "release"],
        ),
    ] {
        let output = server
            .command(root.path(), true)
            .args(&args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let transcript = automation_support::specimen(
            root.path(),
            &server.origin,
            name,
            Some("release-summary"),
            &args,
            &output,
            &[
                (&candidate, "pub-example-candidate"),
                (&operation, "11111111-1111-4111-8111-111111111111"),
            ],
        );
        serde_json::from_value::<ReleaseSummary>(transcript["stdout"].clone())
            .unwrap()
            .validate()
            .unwrap();
    }
    {
        let mut fixture = server.fixture.lock().unwrap();
        fixture.publications.get_mut(&candidate).unwrap()["status"] = json!("failed");
        fixture.publications.get_mut(&candidate).unwrap()["validation_message"] =
            json!("Installed wrapper failed validation.");
    }
    let args = ["model", "release", "status", "release"];
    let output = server
        .command(root.path(), true)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success());
    let transcript = automation_support::specimen(
        root.path(),
        &server.origin,
        "release-blocked.json",
        Some("release-summary"),
        &args,
        &output,
        &[
            (&candidate, "pub-example-candidate"),
            (&operation, "11111111-1111-4111-8111-111111111111"),
        ],
    );
    let summary: ReleaseSummary = serde_json::from_value(transcript["stdout"].clone()).unwrap();
    summary.validate().unwrap();
    assert!(!summary.blockers.is_empty());
    assert!(server.fixture.lock().unwrap().activations.is_empty());
}

#[test]
fn release_source_modes_discover_personal_grants_only_when_registering_fresh_candidates() {
    for mode in ["--file", "--rollback-from", "--candidate"] {
        let root = tempfile::tempdir().unwrap();
        let server = Server::new();
        fs::write(root.path().join("candidate.json"), source().to_string()).unwrap();
        let prior = "retained-candidate";
        if mode != "--file" {
            let mut publication = publication(&manifest(prior)["spec"]);
            if mode == "--candidate" {
                publication["release_authority"] = json!({"available":true,"unavailable_reason":null,"can_delegate":false,"grant_holder_user_id":USER,"publication_delegation_id":"77777777-7777-4777-8777-777777777777"});
            }
            server
                .fixture
                .lock()
                .unwrap()
                .publications
                .insert(prior.into(), publication);
        }
        server.call(
            root.path(),
            &[
                "model",
                "release",
                "init",
                "example",
                mode,
                if mode == "--file" {
                    "candidate.json"
                } else {
                    prior
                },
                "--directory",
                "release",
            ],
            0,
        );
        assert!(
            server
                .fixture
                .lock()
                .unwrap()
                .requests
                .iter()
                .all(|r| !r.contains("compute-grants")),
            "initialization must not discover grants: {mode}"
        );
        server.call(root.path(), &["model", "release", "resume", "release"], 0);
        let fixture = server.fixture.lock().unwrap();
        assert_eq!(
            fixture
                .requests
                .iter()
                .any(|r| r.contains("compute-grants")),
            mode != "--candidate",
            "{mode}"
        );
        assert_eq!(fixture.registrations.is_empty(), mode == "--candidate");
    }
}

fn source() -> Value {
    let mut value: Value =
        serde_json::from_str(include_str!("../examples/scalar-publication.json")).unwrap();
    value["model"]["id"] = json!("example");
    value["execution"]["context_id"] = json!("context");
    value
}

fn manifest(id: &str) -> Value {
    let parsed =
        enumeray_contracts::models::parse_publication(&serde_json::to_vec(&source()).unwrap())
            .unwrap();
    json!({"manifest_version":1,"kind":"model-publication","spec":parsed.candidate(id).unwrap()})
}

fn publication(spec: &Value) -> Value {
    let definition: ScientificDefinition =
        serde_json::from_value(spec["definition"].clone()).unwrap();
    let descriptor: InstallationSpec = serde_json::from_value(spec["descriptor"].clone()).unwrap();
    let mut descriptor = serde_json::to_value(descriptor).unwrap();
    descriptor["provider_binding_id"] = json!("installation-key");
    descriptor["installation"] =
        serde_json::to_value(InstallationIdentity::derive(&definition)).unwrap();
    json!({"id":spec["id"],"model_id":"example","definition":definition,"context_id":"context","grant_id":GRANT,"descriptor":descriptor,
        "status":"ready","validation_message":null,"revoked_at":null,"installation_revoked_at":null,"execution_location":"Research compute",
        "disclosure":"Research administrators process scientific data.","generation":1,"admission_open":false,"admission_updated_at":null,"created_at":"2026-10-17T00:00:00Z","published_at":null,"validation_evidence":null,"configuration_retention":{"state":"retained","requested_at":null,"retired_at":null,"retirement_blocked":false,"blockers":{"queued_runs":0,"running_runs":0,"pending_delivery_assignments":0,"pending_log_archives":0,"pending_assignment_uploads":0}},
        "rollback_availability":{"available":true,"unavailable_reason":null,"requires_revalidation":true},
        "trial_availability":{"available":false,"unavailable_reason":"insufficient_scope"},
        "trial_evidence":{"counts":{"queued":0,"running":0,"succeeded":0,"failed":0,"cancelled":0},"latest_run":null,"latest_succeeded_run":null}})
}

fn decimal_spellings(value: &mut Value, exponential: bool) {
    for (field, scientific, decimal) in [
        ("minimum", "1e-9", "0.000000001"),
        (
            "maximum",
            "9007199254740993123456789e-9",
            "9007199254740993.123456789",
        ),
    ] {
        value["input_schema"]["properties"]["value"][field] =
            serde_json::from_str(if exponential { scientific } else { decimal }).unwrap();
    }
    value["examples"][0]["inputs"]["value"] =
        serde_json::from_str(if exponential { "3e-6" } else { "0.000003" }).unwrap();
    value["examples"][0]["outputs"]["doubled"] =
        serde_json::from_str(if exponential { "6e-6" } else { "0.000006" }).unwrap();
}

struct Fixture {
    api_origin: String,
    token_exchanges: Vec<String>,
    bearers: Vec<(String, String)>,
    delegations: BTreeMap<String, Value>,
    drop_delegation: bool,
    current: Option<String>,
    user: String,
    admin: bool,
    grant: Option<String>,
    publications: BTreeMap<String, Value>,
    registrations: Vec<Value>,
    activations: Vec<Value>,
    requests: Vec<String>,
    drop_registration: bool,
    reject_registration: bool,
    slow_registration: bool,
    drop_activation: bool,
    reject_activation: bool,
    validating: bool,
    paginated: bool,
    slow_read: bool,
    race_activation: bool,
    normalize_numbers: bool,
}
impl Fixture {
    fn handle(&mut self, request: &str, bytes: &[u8]) -> Option<(u16, Value)> {
        let line = request.lines().next().unwrap().to_owned();
        self.requests.push(line.clone());
        let bearer = request
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("authorization")
                    .then(|| value.trim().to_owned())
            })
            .unwrap_or_default();
        self.bearers.push((line.clone(), bearer));
        let path = line.split_whitespace().nth(1).unwrap();
        if line.starts_with("GET /api/v1/auth/config ") {
            return Some((
                200,
                json!({"issuer":"https://identity.example/realms/release", "client_id":"enumeray-cli", "audience":"enumeray-api",
                "authorization_endpoint":"https://identity.example/authorize", "token_endpoint":format!("{}token", self.api_origin), "scopes":["openid","offline_access"]}),
            ));
        }
        if line.starts_with("POST /token ") {
            assert!(
                self.token_exchanges.is_empty(),
                "release renewed the same session more than once"
            );
            let form = String::from_utf8(bytes.to_vec()).unwrap();
            assert!(form.contains("refresh_token=release-refresh-before"));
            self.token_exchanges.push(form);
            return Some((
                200,
                json!({"access_token":"release-access-after", "refresh_token":"release-refresh-after", "token_type":"Bearer", "expires_in":300}),
            ));
        }
        if line.starts_with("GET /api/v1/me ") {
            return Some((200, json!({"user_id":self.user,"label":"Author"})));
        }
        if path == "/api/v1/models/example" {
            let current = self.current.as_ref().map(|id| {
                self.publications
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| publication(&manifest(id)["spec"]))
            });
            let current=current.map(|p| json!({"id":p["id"],"publication_id":p["id"],"definition":p["definition"],"disclosure":p["disclosure"],"execution_location":p["execution_location"],"revoked_at":p["revoked_at"],"installation_revoked_at":p["installation_revoked_at"],"context_id":p["context_id"],"status":p["status"],"available":true,"submission_availability":{"admission_open":true,"has_approved_execution_path":true,"unavailable_reason":null,"execution_destination":{"context_id":p["context_id"],"execution_location":p["execution_location"],"disclosure":p["disclosure"]}}}));
            return Some((
                200,
                json!({"generation":1,"model_id":"example","name":"Example","description":"","owner_id":USER,"access_mode":"invite","visibility":"listed","can_run":true,"can_manage":self.admin,"current_publication_id":self.current,"current_publication":current}),
            ));
        }
        if path.starts_with("/api/v1/compute-grants") {
            if self.paginated && !path.contains("cursor=") {
                return Some((200, json!({"items":[],"next_cursor":"next-page"})));
            }
            let items = self
                .grant
                .as_ref()
                .map(|id| vec![json!({"id":id,"user_id":USER,"context_id":"context","connector_id":"worker","connector_label":"Lab","generation":1,"max_queued_runs":100,"max_outstanding_assignments":4,"context":{"id":"context","driver":"process","connection":"direct","installation_roots":["/opt/models"],"metadata":{"location":"Research compute","disclosure":"Research administrators process scientific data."}}})])
                .unwrap_or_default();
            return Some((200, json!({"items":items,"next_cursor":null})));
        }
        if path == "/api/v1/models/example/publications" && line.starts_with("POST ") {
            let spec: Value = serde_json::from_slice(bytes).unwrap();
            if let Some(old) = self.registrations.first() {
                assert_eq!(old, &spec, "registration retry changed identity or body");
            }
            self.registrations.push(spec.clone());
            if std::mem::take(&mut self.reject_registration) {
                return Some((
                    503,
                    json!({"code":"management_unavailable","message":"Unknown commit","request_id":"fixture"}),
                ));
            }
            let id = spec["id"].as_str().unwrap().to_owned();
            self.publications.entry(id.clone()).or_insert_with(|| {
                let mut p = publication(&spec);
                p["status"] = json!(if self.validating {
                    "validating"
                } else {
                    "ready"
                });
                if let Some(grant) = &self.grant {
                    p["grant_id"] = json!(grant);
                }
                if self.normalize_numbers {
                    decimal_spellings(&mut p["definition"], false);
                }
                p
            });
            if std::mem::take(&mut self.drop_registration) {
                return None;
            }
            return Some((
                200,
                json!({"id":id,"status":self.publications[&id]["status"]}),
            ));
        }
        let prefix = "/api/v1/models/example/publications/";
        if let Some(rest) = path.strip_prefix(prefix) {
            if let Some((candidate, id)) = rest.split_once("/delegations/") {
                let body: Value = serde_json::from_slice(bytes).unwrap();
                if line.starts_with("PUT ") {
                    self.delegations.entry(id.into()).or_insert_with(||json!({"id":id,"publication_id":candidate,"user_id":body["user_id"],"expected_generation":body["expected_generation"],"grant_id":GRANT,"created_by":USER,"created_at":"2026-10-16T11:00:00Z","revoked_at":null,"revocation_reason":null}));
                    if std::mem::take(&mut self.drop_delegation) {
                        return None;
                    }
                } else if line.starts_with("DELETE ") {
                    self.delegations.get_mut(id).unwrap()["revoked_at"] =
                        json!("2026-10-16T12:00:00Z");
                    self.delegations.get_mut(id).unwrap()["revocation_reason"] =
                        json!("explicit_revocation");
                }
                return Some((200, self.delegations[id].clone()));
            }

            if let Some(id) = rest.strip_suffix("/activation") {
                assert!(line.starts_with("POST "));
                let body: Value = serde_json::from_slice(bytes).unwrap();
                self.activations.push(body.clone());
                if std::mem::take(&mut self.reject_activation) {
                    return Some((
                        503,
                        json!({"code":"management_unavailable","message":"Uncertain database reply","request_id":"fixture"}),
                    ));
                }
                if std::mem::take(&mut self.race_activation) {
                    self.current = Some("competing".into());
                }
                if self.current.as_deref() != Some(id)
                    && json!(self.current) != body["expected_current_publication_id"]
                {
                    return Some((
                        409,
                        json!({"code":"management_conflict","message":"Current changed","request_id":"fixture"}),
                    ));
                }
                self.current = Some(id.into());
                self.publications.get_mut(id).unwrap()["status"] = json!("published");
                if std::mem::take(&mut self.drop_activation) {
                    return None;
                }
                return Some((200, json!({"id":id,"status":"published"})));
            }
            return Some(
                self.publications
                    .get(rest)
                    .map(|p| (200, p.clone()))
                    .or_else(|| {
                        (self.current.as_deref() == Some(rest))
                            .then(|| (200, publication(&manifest(rest)["spec"])))
                    })
                    .unwrap_or((
                        404,
                        json!({"code":"not_found","message":"Unavailable","request_id":"fixture"}),
                    )),
            );
        }
        panic!("Unexpected request {line}");
    }
}

struct Server {
    origin: String,
    fixture: Arc<Mutex<Fixture>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        let fixture = Arc::new(Mutex::new(Fixture {
            api_origin: origin.clone(),
            token_exchanges: vec![],
            bearers: vec![],
            delegations: BTreeMap::new(),
            drop_delegation: false,
            current: None,
            user: USER.into(),
            admin: true,
            grant: Some(GRANT.into()),
            publications: BTreeMap::new(),
            registrations: vec![],
            activations: vec![],
            requests: vec![],
            drop_registration: false,
            reject_registration: false,
            slow_registration: false,
            drop_activation: false,
            reject_activation: false,
            validating: false,
            paginated: false,
            slow_read: false,
            race_activation: false,
            normalize_numbers: false,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let (state, stopping) = (fixture.clone(), stop.clone());
        let worker = thread::spawn(move || {
            while !stopping.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(v) => v,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut headers = Vec::new();
                let mut byte = [0];
                while !headers.ends_with(b"\r\n\r\n") {
                    if stream.read_exact(&mut byte).is_err() {
                        break;
                    }
                    headers.push(byte[0]);
                }
                let headers = String::from_utf8(headers).unwrap();
                if headers.is_empty() {
                    continue;
                }
                let length = headers
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|n| n.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                let mut bytes = vec![0; length];
                stream.read_exact(&mut bytes).unwrap();
                let (response, slow) = {
                    let mut f = state.lock().unwrap();
                    let response = f.handle(&headers, &bytes);
                    (
                        response,
                        (f.slow_read && headers.starts_with("GET /api/v1/models/"))
                            || (f.slow_registration
                                && headers
                                    .starts_with("POST /api/v1/models/example/publications ")),
                    )
                };
                if slow {
                    thread::sleep(Duration::from_secs(2));
                }
                if let Some((status, value)) = response {
                    let body = value.to_string();
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {status} fixture\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                }
            }
        });
        Self {
            origin,
            fixture,
            stop,
            worker: Some(worker),
        }
    }
    fn command(&self, root: &Path, json_output: bool) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_enumeray"));
        cmd.current_dir(root)
            .args(["--api", &self.origin, "--config", "profiles.json"])
            .env("ENUMERAY_TOKEN", "fixture-secret");
        if json_output {
            cmd.arg("--json");
        }
        cmd
    }
    fn call(&self, root: &Path, args: &[&str], code: i32) -> Value {
        let output = self.command(root, true).args(args).output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(code),
            "{args:?}\nstdout {}\nstderr {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(if output.stdout.is_empty() {
            &output.stderr
        } else {
            &output.stdout
        })
        .unwrap()
    }
    fn init(&self, root: &Path) {
        fs::write(root.join("candidate.json"), source().to_string()).unwrap();
        self.call(
            root,
            &[
                "model",
                "release",
                "init",
                "example",
                "--file",
                "candidate.json",
                "--directory",
                "release",
            ],
            0,
        );
    }
    fn candidate(&self, root: &Path) -> String {
        let release: Value =
            serde_json::from_slice(&fs::read(root.join("release/release.json")).unwrap()).unwrap();
        release["candidate"]["id"].as_str().unwrap().into()
    }
    fn ready(&self, root: &Path) {
        self.init(root);
        self.call(root, &["model", "release", "resume", "release"], 0);
    }
    fn state(&self, root: &Path) -> Value {
        serde_json::from_slice(&fs::read(root.join("release/state.json")).unwrap()).unwrap()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let result = self.worker.take().unwrap().join();
        if !thread::panicking() {
            result.unwrap();
        }
    }
}

#[test]
fn release_recovers_normalized_numbers_and_preserves_review_and_frozen_source() {
    for lost_reply in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let server = Server::new();
        let mut declaration = source();
        decimal_spellings(&mut declaration, true);
        fs::write(root.path().join("candidate.json"), declaration.to_string()).unwrap();
        server.call(
            root.path(),
            &[
                "model",
                "release",
                "init",
                "example",
                "--file",
                "candidate.json",
                "--directory",
                "release",
            ],
            0,
        );
        let frozen = ["release.json", "publication.json"].map(|name| {
            let path = root.path().join("release").join(name);
            (path.clone(), fs::read(path).unwrap())
        });
        {
            let mut fixture = server.fixture.lock().unwrap();
            fixture.normalize_numbers = true;
            fixture.drop_registration = lost_reply;
        }
        if lost_reply {
            server.call(root.path(), &["model", "release", "resume", "release"], 1);
        }
        server.call(root.path(), &["model", "release", "resume", "release"], 0);
        let candidate = server.candidate(root.path());
        {
            let mut fixture = server.fixture.lock().unwrap();
            decimal_spellings(
                &mut fixture.publications.get_mut(&candidate).unwrap()["definition"],
                true,
            );
        }
        server.call(root.path(), &["model", "release", "review", "release"], 0);
        server.call(root.path(), &["model", "release", "status", "release"], 0);
        {
            let mut fixture = server.fixture.lock().unwrap();
            decimal_spellings(
                &mut fixture.publications.get_mut(&candidate).unwrap()["definition"],
                false,
            );
        }
        server.call(root.path(), &["model", "release", "activate", "release"], 0);
        server.call(root.path(), &["model", "release", "status", "release"], 0);
        for (path, original) in frozen {
            assert_eq!(fs::read(path).unwrap(), original);
        }
        let fixture = server.fixture.lock().unwrap();
        assert_eq!(fixture.registrations.len(), 1);
        assert_eq!(fixture.activations.len(), 1);
    }
}

#[test]
fn release_rejects_changed_exact_numbers_and_numeric_strings() {
    for changed in [
        "9007199254740993.123456788",
        "\"9007199254740993.123456789\"",
    ] {
        let root = tempfile::tempdir().unwrap();
        let server = Server::new();
        let mut declaration = source();
        decimal_spellings(&mut declaration, true);
        fs::write(root.path().join("candidate.json"), declaration.to_string()).unwrap();
        server.call(
            root.path(),
            &[
                "model",
                "release",
                "init",
                "example",
                "--file",
                "candidate.json",
                "--directory",
                "release",
            ],
            0,
        );
        server.call(root.path(), &["model", "release", "resume", "release"], 0);
        {
            let mut fixture = server.fixture.lock().unwrap();
            let publication = fixture
                .publications
                .get_mut(&server.candidate(root.path()))
                .unwrap();
            publication["definition"]["input_schema"]["properties"]["value"]["maximum"] =
                serde_json::from_str(changed).unwrap();
        }
        let failed = server.call(root.path(), &["model", "release", "status", "release"], 1);
        assert!(
            failed["error"]["message"]
                .as_str()
                .unwrap()
                .contains("Registered publication conflicts")
        );
        assert!(server.fixture.lock().unwrap().activations.is_empty());
    }
}

#[test]
fn first_release_freezes_inputs_requires_review_and_never_publishes_on_resume() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new();
    server.init(root.path());
    assert!(server.fixture.lock().unwrap().registrations.is_empty());
    fs::write(root.path().join("candidate.json"), "changed after init").unwrap();
    let ready = server.call(root.path(), &["model", "release", "resume", "release"], 0);
    assert_eq!(ready["stage"], "ready");
    assert!(ready["next_command"].as_str().unwrap().contains("review"));
    server.call(root.path(), &["model", "release", "activate", "release"], 1);
    server.call(root.path(), &["model", "release", "review", "release"], 0);
    let review = server.state(root.path())["review"].clone();
    server.call(root.path(), &["model", "release", "resume", "release"], 0);
    server.call(root.path(), &["model", "release", "status", "release"], 0);
    assert_eq!(server.state(root.path())["review"], review);
    assert!(server.fixture.lock().unwrap().activations.is_empty());
    let activated = server.call(root.path(), &["model", "release", "activate", "release"], 0);
    assert_eq!(activated["candidate_is_current"], true);
    assert_eq!(activated["activation_receipt"]["status"], "published");
    server.call(root.path(), &["model", "release", "activate", "release"], 0);
    let fixture = server.fixture.lock().unwrap();
    assert_eq!(fixture.registrations.len(), 1);
    assert_eq!(
        fixture.activations,
        vec![json!({"expected_current_publication_id":null})]
    );
    drop(fixture);
    let human = server
        .command(root.path(), false)
        .args(["model", "release", "status", "release"])
        .output()
        .unwrap();
    let text = String::from_utf8(human.stdout).unwrap();
    assert!(text.contains("Research administrators"));
    assert!(text.contains("Trial success is advisory"));
    // Connector revalidation may fail after activation. Retain the immutable
    // activation fact without treating the later readiness failure as corruption.
    {
        let mut fixture = server.fixture.lock().unwrap();
        let candidate = fixture
            .publications
            .get_mut(&server.candidate(root.path()))
            .unwrap();
        candidate["status"] = json!("failed");
        candidate["published_at"] = json!("2026-10-16T12:00:00Z");
    }
    let status = server.call(root.path(), &["model", "release", "status", "release"], 0);
    assert_eq!(status["stage"], "activated");
    assert_eq!(status["candidate_status"], "failed");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for file in [
            "release.json",
            "publication.json",
            "state.json",
            "release.lock",
        ] {
            assert_eq!(
                fs::metadata(root.path().join("release").join(file))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o077,
                0
            );
        }
    }
}

#[test]
fn readiness_polling_renews_oauth_without_changing_saved_release_or_replaying_mutations() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new();
    server.init(root.path());
    let candidate = server.candidate(root.path());
    let frozen = ["release.json", "publication.json"].map(|name| {
        let path = root.path().join("release").join(name);
        (path.clone(), fs::read(path).unwrap())
    });
    server.fixture.lock().unwrap().validating = true;
    let epoch = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    };
    let expires_at = epoch() + 6;
    let path = root.path().join("profiles.json");
    fs::write(&path, json!({
        "credential_epochs":{"default":"11111111-1111-4111-8111-111111111111"},
        "profiles":{"default":{
            "api_url":server.origin, "access_token":"release-access-before", "refresh_token":"release-refresh-before", "expires_at":expires_at,
            "oauth":{"user_id":USER, "issuer":"https://identity.example/realms/release", "client_id":"enumeray-cli", "audience":"enumeray-api",
                "token_endpoint":format!("{}token", server.origin), "renew_after":expires_at - 3, "renewal":{"status":"ready"}}
        }}
    }).to_string()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let mut child = server
        .command(root.path(), true)
        .env_remove("ENUMERAY_TOKEN")
        .args([
            "model",
            "release",
            "resume",
            "release",
            "--interval",
            "1",
            "--deadline",
            "15",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let until = std::time::Instant::now() + Duration::from_secs(12);
    loop {
        {
            let mut fixture = server.fixture.lock().unwrap();
            if fixture.token_exchanges.len() == 1 && epoch() >= expires_at {
                fixture
                    .publications
                    .get_mut(&candidate)
                    .expect("original candidate was registered")["status"] = json!("ready");
                break;
            }
        }
        if std::time::Instant::now() >= until {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "release did not renew during readiness polling: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let ready: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(ready["stage"], "ready");
    let progress = server.state(root.path());
    assert_eq!(progress["registered_identity"]["id"], candidate);
    assert!(
        progress["activation_attempts"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    {
        let fixture = server.fixture.lock().unwrap();
        assert_eq!(fixture.registrations.len(), 1);
        assert_eq!(fixture.registrations[0]["id"], candidate);
        assert!(
            fixture.activations.is_empty(),
            "renewal must not activate a release"
        );
        assert!(fixture.bearers.iter().any(|(request, bearer)| {
            request.starts_with("POST /api/v1/models/example/publications ")
                && bearer == "Bearer release-access-before"
        }));
        assert!(fixture.bearers.iter().any(|(request, bearer)| {
            request.starts_with("GET /api/v1/models/example/publications/")
                && bearer == "Bearer release-access-after"
        }));
        let rotated = fixture
            .bearers
            .iter()
            .position(|(request, _)| request.starts_with("POST /token "))
            .unwrap();
        assert!(
            fixture.bearers[rotated + 1..]
                .iter()
                .filter(|(request, _)| request.starts_with("GET /api/v1/")
                    && !request.starts_with("GET /api/v1/auth/config "))
                .all(|(_, bearer)| bearer == "Bearer release-access-after"),
            "readiness polling kept using an earlier access token after rotation"
        );
    }
    let oauth_call = |args: &[&str]| {
        let output = server
            .command(root.path(), true)
            .env_remove("ENUMERAY_TOKEN")
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    oauth_call(&["model", "release", "review", "release"]);
    let review = server.state(root.path())["review"].clone();
    oauth_call(&["model", "release", "activate", "release"]);
    oauth_call(&["model", "release", "activate", "release"]);
    let progress = server.state(root.path());
    assert_eq!(progress["review"], review);
    assert_eq!(progress["activation_attempts"].as_array().unwrap().len(), 1);
    let fixture = server.fixture.lock().unwrap();
    assert_eq!(fixture.current.as_deref(), Some(candidate.as_str()));
    assert_eq!(fixture.registrations.len(), 1);
    assert_eq!(fixture.token_exchanges.len(), 1);
    assert_eq!(
        fixture.activations,
        vec![json!({"expected_current_publication_id":null})]
    );
    for (path, bytes) in frozen {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    let saved: Value =
        serde_json::from_slice(&fs::read(root.path().join("profiles.json")).unwrap()).unwrap();
    assert_eq!(
        saved["profiles"]["default"]["refresh_token"],
        "release-refresh-after"
    );
    assert_eq!(saved["profiles"]["default"]["oauth"]["user_id"], USER);
}

#[test]
fn lost_registration_and_activation_replies_are_reconciled_without_new_candidates_or_silent_activation()
 {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new();
    server.init(root.path());
    server.fixture.lock().unwrap().drop_registration = true;
    server.call(root.path(), &["model", "release", "resume", "release"], 1);
    assert_eq!(server.state(root.path())["registration_started"], true);
    server.call(root.path(), &["model", "release", "resume", "release"], 0);
    assert_eq!(server.fixture.lock().unwrap().registrations.len(), 1);
    server.call(root.path(), &["model", "release", "review", "release"], 0);
    server.fixture.lock().unwrap().drop_activation = true;
    server.call(root.path(), &["model", "release", "activate", "release"], 1);
    let reconciled = server.call(root.path(), &["model", "release", "resume", "release"], 0);
    assert_eq!(reconciled["stage"], "activated");
    assert_eq!(reconciled["activation_receipt"], Value::Null);
    server.call(root.path(), &["model", "release", "activate", "release"], 0);
    assert_eq!(server.fixture.lock().unwrap().activations.len(), 1);
}

#[test]
fn uncertain_uncommitted_activation_preserves_the_request_until_explicit_retry() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new();
    server.ready(root.path());
    server.call(root.path(), &["model", "release", "review", "release"], 0);
    server.fixture.lock().unwrap().reject_activation = true;
    server.call(root.path(), &["model", "release", "activate", "release"], 1);
    let request = server.state(root.path())["activation_attempts"][0]["request"].clone();
    let summary = server.call(root.path(), &["model", "release", "resume", "release"], 0);
    assert_eq!(summary["activation_uncertain"], true);
    assert_eq!(server.fixture.lock().unwrap().activations.len(), 1);
    server.call(root.path(), &["model", "release", "activate", "release"], 0);
    assert_eq!(
        server.state(root.path())["activation_attempts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        server.fixture.lock().unwrap().activations,
        vec![request.clone(), request]
    );
}

#[test]
fn replacement_conflict_requires_explicit_re_review_and_server_guard_catches_races() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new();
    server.fixture.lock().unwrap().current = Some("original".into());
    server.ready(root.path());
    server.call(root.path(), &["model", "release", "review", "release"], 0);
    server.fixture.lock().unwrap().current = Some("replacement".into());
    let status = server.call(root.path(), &["model", "release", "status", "release"], 0);
    assert_eq!(status["guard_changed"], true);
    server.call(root.path(), &["model", "release", "activate", "release"], 1);
    assert!(server.fixture.lock().unwrap().activations.is_empty());
    server.call(root.path(), &["model", "release", "review", "release"], 0);
    server.fixture.lock().unwrap().race_activation = true;
    let conflict = server.call(root.path(), &["model", "release", "activate", "release"], 1);
    assert_eq!(conflict["status"], 409);
    server.call(root.path(), &["model", "release", "resume", "release"], 0);
    assert_eq!(server.fixture.lock().unwrap().activations.len(), 1);
    server.call(root.path(), &["model", "release", "review", "release"], 0);
    server.call(root.path(), &["model", "release", "activate", "release"], 0);
    assert_eq!(
        server.fixture.lock().unwrap().activations.last().unwrap()["expected_current_publication_id"],
        "competing"
    );
}

#[test]
fn readiness_deadline_retains_registration_and_recovery_reuses_the_candidate() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new();
    server.init(root.path());
    server.fixture.lock().unwrap().validating = true;
    server.call(
        root.path(),
        &["model", "release", "resume", "release", "--deadline", "1"],
        5,
    );
    let candidate = server.state(root.path())["registered_identity"]["id"].clone();
    assert_eq!(candidate, server.candidate(root.path()));
    server
        .fixture
        .lock()
        .unwrap()
        .publications
        .get_mut(&server.candidate(root.path()))
        .unwrap()["status"] = json!("ready");
    server.call(root.path(), &["model", "release", "resume", "release"], 0);
    assert_eq!(server.fixture.lock().unwrap().registrations.len(), 1);
}

#[test]
fn polling_deadline_cancels_slow_reads() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new();
    server.init(root.path());
    server.fixture.lock().unwrap().slow_read = true;
    let start = std::time::Instant::now();
    server.call(
        root.path(),
        &["model", "release", "resume", "release", "--deadline", "1"],
        5,
    );
    assert!(start.elapsed() < Duration::from_millis(1800));
    assert!(server.fixture.lock().unwrap().registrations.is_empty());
}

#[test]
fn authority_changes_block_promotion_without_substituting_a_reissued_grant() {
    for reason in [
        "admin",
        "grant",
        "publication",
        "installation",
        "retirement",
        "failed",
        "compute",
    ] {
        let root = tempfile::tempdir().unwrap();
        let server = Server::new();
        server.ready(root.path());
        server.call(root.path(), &["model", "release", "review", "release"], 0);
        {
            let mut f = server.fixture.lock().unwrap();
            match reason {
                "admin" => f.admin = false,
                "grant" => f.grant = Some("44444444-4444-4444-8444-444444444444".into()),
                "publication" => {
                    f.publications
                        .get_mut(&server.candidate(root.path()))
                        .unwrap()["revoked_at"] = json!("now")
                }
                "installation" => {
                    f.publications
                        .get_mut(&server.candidate(root.path()))
                        .unwrap()["installation_revoked_at"] = json!("now")
                }
                "retirement" => {
                    f.publications
                        .get_mut(&server.candidate(root.path()))
                        .unwrap()["configuration_retention"]["state"] =
                        json!("retirement_requested")
                }
                "failed" => {
                    f.publications
                        .get_mut(&server.candidate(root.path()))
                        .unwrap()["status"] = json!("failed")
                }
                "compute" => {
                    f.publications
                        .get_mut(&server.candidate(root.path()))
                        .unwrap()["rollback_availability"]["unavailable_reason"] =
                        json!("compute_authorization_unavailable")
                }
                _ => unreachable!(),
            }
        }
        let status = server.call(root.path(), &["model", "release", "status", "release"], 0);
        assert_eq!(status["stage"], "blocked", "{reason}");
        server.call(root.path(), &["model", "release", "activate", "release"], 1);
        assert!(server.fixture.lock().unwrap().activations.is_empty());
    }
}

#[test]
fn trial_outcomes_and_missing_trial_scope_are_advisory_and_grants_are_paginated() {
    for state in [None, Some("failed"), Some("cancelled"), Some("succeeded")] {
        let root = tempfile::tempdir().unwrap();
        let server = Server::new();
        server.fixture.lock().unwrap().paginated = true;
        server.ready(root.path());
        if let Some(state) = state {
            let candidate = server.candidate(root.path());
            server
                .fixture
                .lock()
                .unwrap()
                .publications
                .get_mut(&candidate)
                .unwrap()["trial_evidence"]["latest_run"] = json!({"run_id":"44444444-4444-4444-8444-444444444444","publication_id":candidate,"purpose":"candidate_trial","state":state,"created_at":"2026-10-17T00:00:00Z"});
        }
        let reviewed = server.call(root.path(), &["model", "release", "review", "release"], 0);
        assert_eq!(
            reviewed["trial_availability"]["unavailable_reason"],
            "insufficient_scope"
        );
        assert!(reviewed["trial_init_command"].is_null());
        if let Some(state) = state {
            assert_eq!(reviewed["trial_evidence"]["latest_run"]["state"], state);
        }
        server.call(root.path(), &["model", "release", "activate", "release"], 0);
    }
}

#[test]
fn release_trial_guidance_pins_candidate_and_does_not_refresh_activation_review() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new();
    server.ready(root.path());
    let candidate = server.candidate(root.path());
    server
        .fixture
        .lock()
        .unwrap()
        .publications
        .get_mut(&candidate)
        .unwrap()["trial_availability"] = json!({"available":true,"unavailable_reason":null});
    let status = server.call(root.path(), &["model", "release", "status", "release"], 0);
    let command = status["trial_init_command"].as_str().unwrap();
    assert!(
        command.contains(&format!("analysis init example --candidate {candidate}")),
        "{command}"
    );
    assert!(
        command.contains("--inputs INPUTS.json") && command.contains("--directory NEW_TRIAL_DIR"),
        "{command}"
    );
    assert!(
        status["next_command"]
            .as_str()
            .unwrap()
            .contains("model release review")
    );
    let reviewed = server.call(root.path(), &["model", "release", "review", "release"], 0);
    assert!(
        reviewed["next_command"]
            .as_str()
            .unwrap()
            .contains("model release activate")
    );
    server
        .fixture
        .lock()
        .unwrap()
        .publications
        .get_mut(&candidate)
        .unwrap()["trial_evidence"] = json!({"counts":{"queued":0,"running":0,"failed":1,"cancelled":1,"succeeded":1},"latest_run":{"run_id":"44444444-4444-4444-8444-444444444444","publication_id":candidate,"purpose":"candidate_trial","state":"failed","created_at":"2026-10-17T00:02:00Z"},"latest_succeeded_run":{"run_id":"55555555-5555-4555-8555-555555555555","publication_id":candidate,"purpose":"candidate_trial","state":"succeeded","created_at":"2026-10-17T00:00:00Z"}});
    let later = server.call(root.path(), &["model", "release", "status", "release"], 0);
    assert_eq!(later["guard_changed"], false);
    assert_eq!(later["stage"], "reviewed");
    assert_eq!(later["trial_evidence"]["counts"]["cancelled"], 1);
    assert!(server.fixture.lock().unwrap().activations.is_empty());
    server.call(root.path(), &["model", "release", "activate", "release"], 0);
}

#[test]
fn rollback_uses_retired_settings_but_fresh_identity_grant_and_validation() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new();
    let mut old = publication(&manifest("old")["spec"]);
    old["grant_id"] = json!("44444444-4444-4444-8444-444444444444");
    old["status"] = json!("published");
    old["configuration_retention"]["state"] = json!("retired");
    old["rollback_availability"] = json!({"available":false,"unavailable_reason":"configuration_retired","requires_revalidation":true});
    server
        .fixture
        .lock()
        .unwrap()
        .publications
        .insert("old".into(), old.clone());
    let initialized = server.call(
        root.path(),
        &[
            "model",
            "release",
            "init",
            "example",
            "--rollback-from",
            "old",
            "--directory",
            "release",
        ],
        0,
    );
    assert_ne!(initialized["publication_id"], "old");
    let saved: Value =
        serde_json::from_slice(&fs::read(root.path().join("release/publication.json")).unwrap())
            .unwrap();
    assert_eq!(saved["spec"]["definition"], old["definition"]);
    assert!(saved["spec"].get("grant_id").is_none());
    assert!(
        saved["spec"]["descriptor"]
            .get("provider_binding_id")
            .is_none()
    );
    assert!(saved["spec"]["descriptor"].get("installation").is_none());
    server.call(root.path(), &["model", "release", "resume", "release"], 0);
    assert_eq!(
        server.state(root.path())["registered_identity"]["grant_id"],
        GRANT
    );
    assert_eq!(server.fixture.lock().unwrap().publications["old"], old);
}

#[test]
fn permanently_revoked_rollback_and_wrong_model_project_are_refused_before_local_creation() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new();
    let mut old = publication(&manifest("old")["spec"]);
    old["rollback_availability"]["unavailable_reason"] = json!("installation_revoked");
    server
        .fixture
        .lock()
        .unwrap()
        .publications
        .insert("old".into(), old);
    server.call(
        root.path(),
        &[
            "model",
            "release",
            "init",
            "example",
            "--rollback-from",
            "old",
            "--directory",
            "release",
        ],
        1,
    );
    assert!(!root.path().join("release").exists());
    let output = server
        .command(root.path(), true)
        .args(["model", "init", "other", "--directory", "project"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let mut publication: Value =
        serde_json::from_slice(&fs::read(root.path().join("project/publication.json")).unwrap())
            .unwrap();
    publication["execution"]["context_id"] = json!("context");
    publication["execution"]["release_path"] = json!("/opt/enumeray/example/r1");
    fs::write(
        root.path().join("project/publication.json"),
        publication.to_string(),
    )
    .unwrap();
    server.call(
        root.path(),
        &[
            "model",
            "release",
            "init",
            "example",
            "--file",
            "project/publication.json",
            "--directory",
            "release",
        ],
        1,
    );
    assert!(!root.path().join("release").exists());
}

#[test]
fn project_snapshots_survive_source_removal() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new();
    let output = server
        .command(root.path(), true)
        .args(["model", "init", "example", "--directory", "project"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let mut publication: Value =
        serde_json::from_slice(&fs::read(root.path().join("project/publication.json")).unwrap())
            .unwrap();
    publication["execution"]["context_id"] = json!("context");
    publication["execution"]["release_path"] = json!("/opt/enumeray/example/r1");
    fs::write(
        root.path().join("project/publication.json"),
        publication.to_string(),
    )
    .unwrap();
    server.call(
        root.path(),
        &[
            "model",
            "release",
            "init",
            "example",
            "--file",
            "project/publication.json",
            "--directory",
            "release",
        ],
        0,
    );
    fs::remove_dir_all(root.path().join("project")).unwrap();
    server.call(root.path(), &["model", "release", "resume", "release"], 0);
}

#[test]
fn edited_evidence_wrong_accounts_and_conflicting_destinations_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new();
    server.init(root.path());
    let saved = fs::read(root.path().join("release/publication.json")).unwrap();
    server.call(
        root.path(),
        &[
            "model",
            "release",
            "init",
            "example",
            "--file",
            "candidate.json",
            "--directory",
            "release",
        ],
        1,
    );
    fs::write(
        root.path().join("release/publication.json"),
        manifest("changed").to_string(),
    )
    .unwrap();
    server.call(root.path(), &["model", "release", "resume", "release"], 1);
    fs::write(root.path().join("release/publication.json"), saved).unwrap();
    server.fixture.lock().unwrap().user = "44444444-4444-4444-8444-444444444444".into();
    server.call(root.path(), &["model", "release", "resume", "release"], 1);
    server.fixture.lock().unwrap().user = USER.into();
    let other = Server::new();
    other.call(root.path(), &["model", "release", "resume", "release"], 1);
    assert!(server.fixture.lock().unwrap().registrations.is_empty());
}

#[test]
fn concurrent_writers_and_symlink_evidence_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new();
    server.init(root.path());
    let lock = fs::OpenOptions::new()
        .write(true)
        .open(root.path().join("release/release.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    server.call(root.path(), &["model", "release", "status", "release"], 1);
    drop(lock);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            root.path().join("candidate.json"),
            root.path().join("release/link.json"),
        )
        .unwrap();
        fs::remove_file(root.path().join("release/publication.json")).unwrap();
        fs::rename(
            root.path().join("release/link.json"),
            root.path().join("release/publication.json"),
        )
        .unwrap();
        server.call(root.path(), &["model", "release", "resume", "release"], 1);
    }
}

#[cfg(unix)]
#[test]
fn ctrl_c_stops_polling_with_130_and_keeps_the_registered_candidate() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new();
    server.init(root.path());
    server.fixture.lock().unwrap().validating = true;
    let child = server
        .command(root.path(), true)
        .args(["model", "release", "resume", "release"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    for _ in 0..200 {
        if server.state(root.path())["registered_identity"].is_object() {
            break;
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
    let output: Output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(130));
    assert_eq!(
        server.state(root.path())["registered_identity"]["id"],
        server.candidate(root.path())
    );
    assert!(server.fixture.lock().unwrap().activations.is_empty());
}

#[test]
fn uncertain_uncommitted_registration_replays_the_original_manifest() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new();
    server.init(root.path());
    server.fixture.lock().unwrap().reject_registration = true;
    server.call(root.path(), &["model", "release", "resume", "release"], 1);
    assert!(server.state(root.path())["registered_identity"].is_null());
    fs::remove_file(root.path().join("candidate.json")).unwrap();
    server.call(root.path(), &["model", "release", "resume", "release"], 0);
    let fixture = server.fixture.lock().unwrap();
    assert_eq!(fixture.registrations.len(), 2);
    assert_eq!(fixture.registrations[0], fixture.registrations[1]);
    assert!(fixture.activations.is_empty());
}

#[test]
fn malformed_or_changed_local_and_remote_evidence_stops_release() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new();
    server.ready(root.path());
    let original =
        server.fixture.lock().unwrap().publications[&server.candidate(root.path())].clone();
    for (field, value) in [
        ("disclosure", json!("changed")),
        ("grant_id", json!("changed")),
        ("definition", json!("changed")),
        ("configuration_retention", json!("changed")),
        ("release_authority", Value::Null),
        ("activation_availability", Value::Null),
    ] {
        let mut changed = original.clone();
        changed[field] = value;
        server
            .fixture
            .lock()
            .unwrap()
            .publications
            .insert(server.candidate(root.path()), changed);
        server.call(root.path(), &["model", "release", "review", "release"], 1);
    }
    server
        .fixture
        .lock()
        .unwrap()
        .publications
        .insert(server.candidate(root.path()), original);
    let path = root.path().join("release/state.json");
    let state = fs::read(&path).unwrap();
    let mut forged: Value = serde_json::from_slice(&state).unwrap();
    forged["observed_current"] = json!(true);
    fs::write(&path, forged.to_string()).unwrap();
    server.call(root.path(), &["model", "release", "activate", "release"], 1);
    fs::write(&path, vec![b' '; 8 * 1024 * 1024 + 1]).unwrap();
    server.call(root.path(), &["model", "release", "status", "release"], 1);
    fs::write(&path, state).unwrap();
    let release_path = root.path().join("release/release.json");
    let mut release: Value = serde_json::from_slice(&fs::read(&release_path).unwrap()).unwrap();
    release["source"]["kind"] = json!("altered");
    fs::write(&release_path, release.to_string()).unwrap();
    server.call(root.path(), &["model", "release", "status", "release"], 1);
    assert!(server.fixture.lock().unwrap().activations.is_empty());
}

#[cfg(unix)]
#[test]
fn ctrl_c_during_registration_keeps_the_uncertain_request_for_reconciliation() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new();
    server.init(root.path());
    server.fixture.lock().unwrap().slow_registration = true;
    let child = server
        .command(root.path(), true)
        .args(["model", "release", "resume", "release"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    for _ in 0..200 {
        if !server.fixture.lock().unwrap().registrations.is_empty() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(server.fixture.lock().unwrap().registrations.len(), 1);
    assert!(
        Command::new("/bin/kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(130));
    assert_eq!(server.state(root.path())["registration_started"], true);
    assert!(server.state(root.path())["registration_response"].is_null());
    server.call(root.path(), &["model", "release", "resume", "release"], 0);
    assert_eq!(server.fixture.lock().unwrap().registrations.len(), 1);
    assert!(server.fixture.lock().unwrap().activations.is_empty());
}

#[test]
fn handoff_retry_reuses_saved_identity_and_retains_revocation() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new();
    server.ready(root.path());
    {
        let mut f = server.fixture.lock().unwrap();
        f.publications
            .get_mut(&server.candidate(root.path()))
            .unwrap()["release_authority"] = json!({"available":true,"unavailable_reason":null,"can_delegate":true,"grant_holder_user_id":USER,"publication_delegation_id":null});
        f.drop_delegation = true;
    }
    let recipient = "55555555-5555-4555-8555-555555555555";
    let args = [
        "model",
        "release",
        "delegate",
        "release",
        "--user",
        recipient,
        "--generation",
        "1",
    ];
    let before = fs::read(root.path().join("release/state.json")).unwrap();
    server.call(root.path(), &[args.as_slice(), &["--dry-run"]].concat(), 0);
    assert_eq!(
        fs::read(root.path().join("release/state.json")).unwrap(),
        before
    );
    server.call(root.path(), &args, 1);
    let state = server.state(root.path());
    let id = state["delegation_attempts"][0]["id"].as_str().unwrap();
    let receipt = server.call(root.path(), &args, 0);
    assert_eq!(receipt["id"], id);
    assert_eq!(server.fixture.lock().unwrap().delegations.len(), 1);
    server.call(
        root.path(),
        &[
            "model",
            "release",
            "revoke-delegation",
            "release",
            id,
            "--generation",
            "2",
        ],
        0,
    );
    assert!(!server.call(root.path(), &args, 0)["revoked_at"].is_null());
}

#[test]
fn reviewer_adopts_without_grant_inventory_and_pins_authority_in_explicit_review() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new();
    let id = "77777777-7777-4777-8777-777777777777";
    {
        let mut f = server.fixture.lock().unwrap();
        f.grant = None;
        let mut p = publication(&manifest("candidate")["spec"]);
        p["release_authority"] = json!({"available":true,"unavailable_reason":null,"can_delegate":false,"publication_delegation_id":id,"grant_holder_user_id":USER});
        f.publications.insert("candidate".into(), p);
    }
    server.call(
        root.path(),
        &[
            "model",
            "release",
            "init",
            "example",
            "--candidate",
            "candidate",
            "--directory",
            "release",
        ],
        0,
    );
    server.call(root.path(), &["model", "release", "resume", "release"], 0);
    server.call(root.path(), &["model", "release", "activate", "release"], 1);
    server.call(root.path(), &["model", "release", "review", "release"], 0);
    {
        let mut f = server.fixture.lock().unwrap();
        f.publications
            .get_mut(&server.candidate(root.path()))
            .unwrap()["release_authority"]["publication_delegation_id"] =
            json!("88888888-8888-4888-8888-888888888888");
    }
    server.call(root.path(), &["model", "release", "activate", "release"], 1);
    server.call(root.path(), &["model", "release", "review", "release"], 0);
    server.call(root.path(), &["model", "release", "activate", "release"], 0);
    let f = server.fixture.lock().unwrap();
    assert!(f.registrations.is_empty());
    assert!(f.requests.iter().all(|r| !r.contains("compute-grants")));
    assert_eq!(
        f.activations[0]["publication_delegation_id"],
        "88888888-8888-4888-8888-888888888888"
    );
}

#[test]
fn explicit_initialization_allocates_distinct_candidates_without_editing_source() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new();
    server.init(root.path());
    let first = server.candidate(root.path());
    let original = fs::read(root.path().join("candidate.json")).unwrap();
    let second = server.call(
        root.path(),
        &[
            "model",
            "release",
            "init",
            "example",
            "--file",
            "candidate.json",
            "--directory",
            "second",
        ],
        0,
    );
    assert_ne!(second["publication_id"], first);
    assert_eq!(
        fs::read(root.path().join("candidate.json")).unwrap(),
        original
    );
    assert!(
        serde_json::from_slice::<Value>(&original)
            .unwrap()
            .get("id")
            .is_none()
    );
    assert!(server.fixture.lock().unwrap().registrations.is_empty());
    assert!(server.fixture.lock().unwrap().activations.is_empty());
}

#[derive(serde::Serialize, serde::Deserialize)]
struct HistoricalRelease {
    schema_version: u32,
    operation_id: uuid::Uuid,
    api_url: String,
    user_id: uuid::Uuid,
    model_id: String,
    candidate: enumeray_contracts::management::PublicationCreate,
    source: Value,
    initial_current: Value,
}

#[test]
fn complete_historical_operations_retain_split_source_evidence_without_author_parsing() {
    use sha2::Digest;
    for kind in ["project", "manifest"] {
        let root = tempfile::tempdir().unwrap();
        let server = Server::new();
        server.init(root.path());
        let id = server.candidate(root.path());
        let file = root.path().join("release/release.json");
        let mut saved: HistoricalRelease =
            serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        let machine =
            json!({"manifest_version":1,"kind":"model-publication","spec":saved.candidate});
        saved.source = if kind == "project" {
            let parsed = enumeray_contracts::models::parse_publication(
                &serde_json::to_vec(&source()).unwrap(),
            )
            .unwrap();
            json!({"kind":"project","scientific":{"model":parsed.model,"definition":parsed.definition},"binding":{"binding_version":1,"driver":"process","context_id":"context","release_path":saved.candidate.descriptor.release_path,"entrypoint":saved.candidate.descriptor.entrypoint,"environment":{}}})
        } else {
            json!({"kind":"manifest","manifest":machine})
        };
        let bytes = serde_json::to_vec(&saved).unwrap();
        fs::write(&file, &bytes).unwrap();
        let state_path = root.path().join("release/state.json");
        let mut state: Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
        state["release_sha256"] = json!(
            sha2::Sha256::digest(&bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        fs::write(&state_path, state.to_string()).unwrap();
        fs::remove_file(root.path().join("candidate.json")).unwrap();
        server.call(root.path(), &["model", "release", "resume", "release"], 0);
        assert_eq!(server.fixture.lock().unwrap().registrations[0]["id"], id);
        assert_eq!(fs::read(&file).unwrap(), bytes);
    }
}

#[test]
fn interrupted_initialization_never_registers_and_new_operation_is_explicit() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new();
    server.init(root.path());
    let abandoned = server.candidate(root.path());
    fs::remove_file(root.path().join("release/state.json")).unwrap();
    let failed = server.call(root.path(), &["model", "release", "resume", "release"], 1);
    let message = failed["error"]["message"].as_str().unwrap();
    assert!(message.contains("preserve this directory"));
    assert!(message.contains("source.publication"));
    assert!(message.contains("uncertain registration"));
    assert!(server.fixture.lock().unwrap().registrations.is_empty());
    // Saved protocol evidence is not accepted as fresh editable author input.
    server.call(
        root.path(),
        &[
            "model",
            "release",
            "init",
            "example",
            "--file",
            "release/publication.json",
            "--directory",
            "invalid",
        ],
        1,
    );
    assert!(!root.path().join("invalid").exists());
    let saved: Value =
        serde_json::from_slice(&fs::read(root.path().join("release/release.json")).unwrap())
            .unwrap();
    fs::write(
        root.path().join("recovered-author.json"),
        saved["source"]["publication"].to_string(),
    )
    .unwrap();
    fs::remove_file(root.path().join("candidate.json")).unwrap();
    let fresh = server.call(
        root.path(),
        &[
            "model",
            "release",
            "init",
            "example",
            "--file",
            "recovered-author.json",
            "--directory",
            "fresh",
        ],
        0,
    );
    assert_ne!(fresh["publication_id"], abandoned);
    assert!(server.fixture.lock().unwrap().registrations.is_empty());
}
