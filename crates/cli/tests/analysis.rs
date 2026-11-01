//! Binary-level analysis recovery over real HTTP sockets and real local files.
mod automation_support;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    process::{Command, Output},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};
const USER: &str = "22222222-2222-4222-8222-222222222222";
const RUN: &str = "33333333-3333-4333-8333-333333333333";
const INPUT: &str = "44444444-4444-4444-8444-444444444444";
const OUTPUT: &str = "55555555-5555-4555-8555-555555555555";
const UPLOAD: &str = "66666666-6666-4666-8666-666666666666";
const GRANT: &str = "77777777-7777-4777-8777-777777777777";
const DELEGATION: &str = "88888888-8888-4888-8888-888888888888";
const BYTES: &[u8] = b"index,value\n0,2\n1,3\n";
const ANSWER: &[u8] = b"index,value\n0,4\n1,6\n";
const SLOT: &str = "../hostile%output";

#[test]
fn automation_transcripts_cover_review_replay_observation_and_collection() {
    use enumeray_contracts::automation::{
        AnalysisCollectionSummary, AnalysisSummary, CliRuntimeError,
    };
    let server = Server::new(false);
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("inputs.json"), r#"{"value":2}"#).unwrap();
    let model = server.call(root.path(), &["model", "get", "model"], true);
    let init_args = [
        "analysis",
        "init",
        "model",
        "--directory",
        "analysis",
        "--inputs",
        "inputs.json",
    ];
    let initialized = server.call(root.path(), &init_args, true);
    let initialized = automation_support::specimen(
        root.path(),
        &server.origin,
        "analysis-initialized.json",
        Some("analysis-summary"),
        &init_args,
        &initialized,
        &[],
    );
    let reviewed = server.call(root.path(), &["analysis", "status", "analysis"], true);
    let reviewed = automation_support::specimen(
        root.path(),
        &server.origin,
        "analysis-reviewed.json",
        Some("analysis-summary"),
        &["analysis", "status", "analysis"],
        &reviewed,
        &[],
    );
    let mut files = json!({"schema_version":1,"files": {
        "model.json":serde_json::from_slice::<Value>(&model.stdout).unwrap(),
        "initialized.json":initialized["stdout"], "reviewed-analysis.json":reviewed["stdout"],
        "analysis/inputs.json":read(root.path(), "analysis/inputs.json")
    }});
    automation_support::normalize(
        &mut files,
        &[
            (
                root.path().canonicalize().unwrap().to_str().unwrap(),
                "/private/enumeray-example",
            ),
            (root.path().to_str().unwrap(), "/private/enumeray-example"),
            (&server.origin, "https://enumeray.example.org/"),
        ],
    );
    automation_support::save("four-file-review.json", &files);
    let parsed: AnalysisSummary = serde_json::from_value(initialized["stdout"].clone()).unwrap();
    parsed.validate().unwrap();
    server.fixture.lock().unwrap().drop_accept = true;
    server.call(root.path(), &["analysis", "submit", "analysis"], false);
    let uncertain = server.call(root.path(), &["analysis", "status", "analysis"], true);
    automation_support::specimen(
        root.path(),
        &server.origin,
        "analysis-submission-uncertain.json",
        Some("analysis-summary"),
        &["analysis", "status", "analysis"],
        &uncertain,
        &[],
    );
    let accepted = server.call(root.path(), &["analysis", "submit", "analysis"], true);
    let accepted = automation_support::specimen(
        root.path(),
        &server.origin,
        "analysis-submitted.json",
        Some("analysis-summary"),
        &["analysis", "submit", "analysis"],
        &accepted,
        &[],
    );
    for state in ["queued", "running", "succeeded"] {
        {
            let mut fixture = server.fixture.lock().unwrap();
            fixture.outcome = state.into();
            fixture.run_override = if state == "running" {
                Some(("phase".into(), json!("executing")))
            } else {
                None
            };
        }
        let output = server.call(root.path(), &["analysis", "status", "analysis"], true);
        let transcript = automation_support::specimen(
            root.path(),
            &server.origin,
            &format!("analysis-status-{state}.json"),
            Some("analysis-summary"),
            &["analysis", "status", "analysis"],
            &output,
            &[],
        );
        let parsed: AnalysisSummary = serde_json::from_value(transcript["stdout"].clone()).unwrap();
        parsed.validate().unwrap();
        assert_eq!(
            transcript["stdout"]["receipt"],
            accepted["stdout"]["receipt"]
        );
        assert_eq!(transcript["stdout"]["run"]["state"], state);
    }
    let collected = server.call(root.path(), &["analysis", "collect", "analysis"], true);
    let transcript = automation_support::specimen(
        root.path(),
        &server.origin,
        "analysis-collected-empty.json",
        Some("analysis-collection-summary"),
        &["analysis", "collect", "analysis"],
        &collected,
        &[],
    );
    serde_json::from_value::<AnalysisCollectionSummary>(transcript["stdout"].clone())
        .unwrap()
        .validate()
        .unwrap();

    let server = Server::new(false);
    let root = tempfile::tempdir().unwrap();
    server.init(root.path(), false);
    server.call(root.path(), &["analysis", "submit", "analysis"], true);
    server.fixture.lock().unwrap().outcome = "failed".into();
    let output = server.call(root.path(), &["analysis", "status", "analysis"], true);
    automation_support::specimen(
        root.path(),
        &server.origin,
        "analysis-status-failed.json",
        Some("analysis-summary"),
        &["analysis", "status", "analysis"],
        &output,
        &[],
    );
    let failed = server.call(root.path(), &["analysis", "collect", "analysis"], true);
    let transcript = automation_support::specimen(
        root.path(),
        &server.origin,
        "analysis-collected-failed.json",
        Some("analysis-collection-summary"),
        &["analysis", "collect", "analysis"],
        &failed,
        &[],
    );
    serde_json::from_value::<AnalysisCollectionSummary>(transcript["stdout"].clone())
        .unwrap()
        .validate()
        .unwrap();

    let server = Server::new(true);
    let root = tempfile::tempdir().unwrap();
    server.init(root.path(), true);
    server.call(root.path(), &["analysis", "submit", "analysis"], true);
    for (name, args) in [
        (
            "analysis-collected-partial.json",
            vec!["analysis", "collect", "analysis", "--evidence-only"],
        ),
        (
            "analysis-collected-files.json",
            vec!["analysis", "collect", "analysis"],
        ),
    ] {
        let output = server.call(root.path(), &args, true);
        let transcript = automation_support::specimen(
            root.path(),
            &server.origin,
            name,
            Some("analysis-collection-summary"),
            &args,
            &output,
            &[],
        );
        serde_json::from_value::<AnalysisCollectionSummary>(transcript["stdout"].clone())
            .unwrap()
            .validate()
            .unwrap();
    }
    let root = tempfile::tempdir().unwrap();
    let output = server.call(root.path(), &["analysis", "collect", "missing"], false);
    let transcript = automation_support::specimen(
        root.path(),
        &server.origin,
        "analysis-runtime-error.json",
        Some("runtime-error"),
        &["analysis", "collect", "missing"],
        &output,
        &[],
    );
    serde_json::from_str::<CliRuntimeError>(transcript["stderr"].as_str().unwrap()).unwrap();
}

#[derive(Default)]
struct RequestGate {
    state: Mutex<(bool, bool)>,
    changed: Condvar,
}
impl RequestGate {
    fn arrive(&self) {
        let mut state = self.state.lock().unwrap();
        state.0 = true;
        self.changed.notify_all();
        let (state, timeout) = self
            .changed
            .wait_timeout_while(state, Duration::from_secs(10), |state| !state.1)
            .unwrap();
        assert!(
            !timeout.timed_out() || state.1,
            "request was never released"
        );
    }
    fn wait(&self) {
        let (state, timeout) = self
            .changed
            .wait_timeout_while(
                self.state.lock().unwrap(),
                Duration::from_secs(5),
                |state| !state.0,
            )
            .unwrap();
        let arrived = state.0;
        drop(state);
        assert!(!timeout.timed_out() || arrived, "request never arrived");
    }
    fn release(&self) {
        self.state.lock().unwrap().1 = true;
        self.changed.notify_all();
    }
}
struct RequestBarrier(Arc<RequestGate>);
impl std::ops::Deref for RequestBarrier {
    type Target = RequestGate;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl Drop for RequestBarrier {
    fn drop(&mut self) {
        // Assertion failures must release the server connection before its join.
        self.release();
    }
}

fn checksum(bytes: &[u8]) -> Value {
    json!({"algorithm":"sha256","checksum_type":"full_object","value":STANDARD.encode(Sha256::digest(bytes))})
}
fn definition(files: bool) -> Value {
    let publication = enumeray_contracts::models::parse_publication(include_bytes!(
        "../examples/scalar-publication.json"
    ))
    .unwrap();
    let mut definition = serde_json::to_value(publication.definition).unwrap();
    assert!(definition.is_object());
    if files {
        let file = json!({"type":"object","properties":{"artifact_id":{"type":"string"}},"required":["artifact_id"],"additionalProperties":false,"x-enumeray-file":{"max_bytes":1048576,"media_types":["text/csv"]}});
        definition["input_schema"] = json!({"type":"object","properties":{"dataset":file},"required":["dataset"],"additionalProperties":false});
        definition["output_schema"] = json!({"type":"object","properties":{SLOT:file,"preview":file,"count":{"type":"integer"}},"required":[SLOT,"count"],"additionalProperties":false});
        definition["examples"] =
            json!([{"name":"example","inputs":{"dataset":{"artifact_id":INPUT}}}]);
    } else {
        definition["examples"] = json!([{"name":"example","inputs":{"value":2}}]);
    }
    definition
}

fn candidate(files: bool) -> Value {
    let parsed = enumeray_contracts::models::parse_publication(include_bytes!(
        "../examples/scalar-publication.json"
    ))
    .unwrap();
    let mut descriptor =
        serde_json::to_value(parsed.candidate("release-one").unwrap().descriptor).unwrap();
    descriptor["provider_binding_id"] = json!("installation-one");
    descriptor["installation"] =
        serde_json::to_value(enumeray_contracts::execution::InstallationIdentity::derive(
            &serde_json::from_value(definition(files)).unwrap(),
        ))
        .unwrap();
    json!({"id":"release-one","model_id":"model","context_id":"context-one","grant_id":GRANT,
        "definition":definition(files),"descriptor":descriptor,"status":"ready","generation":1,
        "execution_location":"Research compute","disclosure":"Model and compute administrators can read submitted data.",
        "trial_availability":{"available":true,"unavailable_reason":null},
        "release_authority":{"available":true,"unavailable_reason":null,"grant_holder_user_id":USER,"can_delegate":true,"publication_delegation_id":null}})
}

struct Fixture {
    files: bool,
    candidate: Value,
    run_override: Option<(String, Value)>,
    provenance_override: Option<(String, Value)>,
    receipt_override: Option<(String, Value)>,
    reject_accept: bool,
    current: String,
    user: String,
    outcome: String,
    accepted: Option<(String, Value)>,
    requests: Vec<String>,
    drop_accept: bool,
    drop_reservation: bool,
    drop_completion: bool,
    fail_download: bool,
    expired_authorization: bool,
    drop_cancellation: bool,
    deny_cancellation: bool,
    cancellation: Option<Value>,
    logs: Vec<Value>,
    stall_next: Option<(String, Arc<RequestGate>)>,
    upload_complete: bool,
    upload_request: Option<(String, Value)>,
    upload_bytes: Vec<u8>,
    origin: String,
}
impl Fixture {
    fn completion(&self) -> Value {
        json!({"upload_id":UPLOAD,"artifact_id":INPUT,"version_id":"version-one","size_bytes":BYTES.len(),"checksum":checksum(BYTES),"completed_at":"2026-10-16T12:00:00Z"})
    }
    fn run(&self) -> Value {
        let inputs = self.accepted.as_ref().unwrap().1["inputs"].clone();
        let result = if self.outcome == "succeeded" {
            if self.files {
                json!({"outputs":{SLOT:{"artifact_id":OUTPUT},"count":2},"files":{SLOT:{"artifact_id":OUTPUT}},"optional_output_failures":{"preview":{"code":"preview_failed","message":"Optional preview unavailable."}},"failure":null})
            } else {
                json!({"outputs":{"doubled":inputs["value"].as_i64().unwrap() * 2},"files":{},"optional_output_failures":{},"failure":null})
            }
        } else {
            Value::Null
        };
        let request = &self.accepted.as_ref().unwrap().1;
        let mut run = json!({"run_id":RUN,"model_id":"model","publication_id":"release-one","purpose":request.get("purpose").cloned().unwrap_or(json!("standard")),"publication_delegation_id":request["publication_delegation_id"],"created_by_user_id":USER,"contract_version":1,"storage_destination_id":null,"created_at":"2026-10-16T12:00:00Z","cancelled_at":null,"phase":null,"state":self.outcome,"requested_inputs":inputs,"effective_inputs":inputs,"result":result});
        run["execution_control"] = json!({"state":if matches!(self.outcome.as_str(),"succeeded"|"failed"|"cancelled") {"completed"} else {"authorized"},"authorization_reason":null});
        if let Some((field, value)) = &self.run_override {
            run[field] = value.clone();
        }
        run
    }
    fn artifact(&self, id: &str) -> Value {
        let bytes = if id == INPUT { BYTES } else { ANSWER };
        json!({"artifact_id":id,"availability":"ready","description":{"media_type":"text/csv","logical_name":if id == INPUT {"dataset"} else {SLOT}},"content":{"version_id":"version-one","size_bytes":bytes.len(),"checksum":checksum(bytes)}})
    }
    fn provenance(&self) -> Value {
        let run = self.run();
        let mut provenance = json!({"format_version":1,"publication":{"publication_id":"release-one","context_id":"context-one","compute_grant_id":GRANT,"generation":1,"execution_location":"Research compute","disclosure":"Model and compute administrators can read submitted data.","installation":candidate(self.files)["descriptor"]["installation"],"validation":null},"run":run,"definition":definition(self.files),"result":run["result"],"input_artifacts":if self.files {json!([{"name":"dataset","artifact_id":INPUT,"logical_name":"dataset","media_type":"text/csv","version_id":"version-one","size_bytes":BYTES.len(),"checksum":checksum(BYTES)}])} else {json!([])},"output_artifacts":if self.files && self.outcome == "succeeded" {
            json!([{"name":SLOT,"artifact_id":OUTPUT,"logical_name":SLOT,"media_type":"text/csv","version_id":"version-one","size_bytes":ANSWER.len(),"checksum":checksum(ANSWER)}])
        } else {json!([])}});
        if let Some((pointer, value)) = &self.provenance_override {
            *provenance.pointer_mut(pointer).unwrap() = value.clone();
        }
        provenance
    }
    fn handle(&mut self, request: &str, body: &[u8]) -> Option<(u16, String, Vec<u8>)> {
        self.requests.push(request.lines().next().unwrap().into());
        let line = request.lines().next().unwrap();
        if line.starts_with("GET /api/v1/me ") && self.expired_authorization {
            return Some((401,"Content-Type: application/json\r\n".into(),json!({"code":"invalid_credential","message":"Credential expired.","request_id":"expired-collection"}).to_string().into_bytes()));
        }
        let key = request
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("idempotency-key:")
                    .map(|key| key.trim().to_owned())
            })
            .unwrap_or_default();
        let response = if line.starts_with("GET /api/v1/me ") {
            json!({"user_id":self.user,"label":"Researcher"})
        } else if line.starts_with("GET /api/v1/models/model ") {
            json!({"model_id":"model","name":"Example model","description":"A reproducible scalar or file fixture.","owner_id":USER,"generation":1,"access_mode":"open","visibility":"listed","can_run":true,"can_manage":false,"current_publication_id":self.current,"current_publication":{"id":self.current,"publication_id":self.current,"context_id":"context-one","execution_location":"Research compute","disclosure":"Model and compute administrators can read submitted data.","definition":definition(self.files),"revoked_at":null,"installation_revoked_at":null,"status":"published","available":true,"submission_availability":{"admission_open":true,"has_approved_execution_path":true,"unavailable_reason":null,"execution_destination":{"context_id":"context-one","execution_location":"Research compute","disclosure":"Model and compute administrators can read submitted data."}}}})
        } else if line.starts_with("GET /api/v1/models/model/publications/release-one ") {
            self.candidate.clone()
        } else if line.starts_with("POST /api/v1/runs ") {
            let body: Value = serde_json::from_slice(body).unwrap();
            if std::mem::take(&mut self.reject_accept) {
                return None;
            }
            if let Some((retained_key, retained_body)) = &self.accepted {
                assert_eq!(retained_key, &key);
                assert_eq!(retained_body, &body);
            } else {
                if body["purpose"] == "candidate_trial" {
                    assert_eq!(body["expected_publication_id"], "release-one");
                } else {
                    if body["expected_publication_id"] != self.current {
                        return Some((409, "Content-Type: application/json\r\n".into(), json!({"code":"publication_changed","message":"The original publication is no longer current.","request_id":"changed-publication"}).to_string().into_bytes()));
                    }
                }
                self.accepted = Some((key, body));
            }
            if std::mem::take(&mut self.drop_accept) {
                return None;
            }
            let request = &self.accepted.as_ref().unwrap().1;
            let mut receipt = json!({"run_id":RUN,"model_id":"model","publication_id":"release-one","purpose":request.get("purpose").cloned().unwrap_or(json!("standard")),"publication_delegation_id":request["publication_delegation_id"],"state":"queued","created_at":"2026-10-16T12:00:00Z"});
            if let Some((field, value)) = &self.receipt_override {
                receipt[field] = value.clone();
            }
            receipt
        } else if line.starts_with(&format!("GET /api/v1/runs/{RUN}/provenance ")) {
            self.provenance()
        } else if line.starts_with(&format!("PUT /api/v1/runs/{RUN}/cancellation ")) {
            if self.deny_cancellation {
                return Some((403, "Content-Type: application/json\r\n".into(), json!({"code":"insufficient_scope","message":"Cancellation is not allowed.","request_id":"denied-cancellation"}).to_string().into_bytes()));
            }
            if self.cancellation.is_none() && !matches!(self.outcome.as_str(), "queued" | "running")
            {
                return Some((409, "Content-Type: application/json\r\n".into(), json!({"code":"run_not_cancellable","message":"The run is already terminal.","request_id":"terminal-cancellation"}).to_string().into_bytes()));
            }
            let (status, cancelled_at) = if self.outcome == "queued" || self.outcome == "cancelled"
            {
                self.outcome = "cancelled".into();
                ("confirmed", json!("2026-10-19T12:00:00Z"))
            } else if self.outcome == "running" {
                ("requested", Value::Null)
            } else {
                ("completed", Value::Null)
            };
            let receipt = json!({"run_id":RUN,"requested_by_user_id":USER,"state":self.outcome,"status":status,"scope":"pending_and_running","requested_at":"2026-10-19T12:00:00Z","reason":"user_requested","cancelled_at":cancelled_at});
            self.cancellation = Some(receipt.clone());
            if std::mem::take(&mut self.drop_cancellation) {
                return None;
            }
            receipt
        } else if line.starts_with(&format!("GET /api/v1/runs/{RUN}/logs/stream")) {
            let body = self
                .logs
                .iter()
                .map(|entry| format!("event: log\nid: c{}\ndata: {entry}\n\n", entry["position"]))
                .collect::<String>();
            return Some((
                200,
                "Content-Type: text/event-stream\r\n".into(),
                body.into_bytes(),
            ));
        } else if line.starts_with(&format!("GET /api/v1/runs/{RUN}/logs")) {
            let last = self
                .logs
                .last()
                .map_or(0, |entry| entry["position"].as_i64().unwrap());
            let items = if line.contains(&format!("cursor=c{last}")) {
                vec![]
            } else {
                self.logs.clone()
            };
            json!({"items":items,"next_cursor":null,"resume_cursor":format!("c{last}"),"retention":{"earliest_position":1,"latest_position":last,"recovery_cursor":"c0","payload_max_age_seconds":604800,"payload_max_entries":1024}})
        } else if line.starts_with(&format!("GET /api/v1/runs/{RUN} ")) {
            let mut run = self.run();
            run["cancellation"] = self
                .cancellation
                .clone()
                .map_or(Value::Null, |mut receipt| {
                    receipt["state"] = json!(self.outcome);
                    receipt["status"] = json!(match self.outcome.as_str() {
                        "cancelled" => "confirmed",
                        "succeeded" | "failed" => "completed",
                        _ => "requested",
                    });
                    if self.outcome == "cancelled" {
                        receipt["cancelled_at"] = json!("2026-10-19T12:00:00Z");
                    }
                    receipt
                });
            run
        } else if line.starts_with("POST /api/v1/uploads ") {
            let body: Value = serde_json::from_slice(body).unwrap();
            if let Some((retained_key, retained_body)) = &self.upload_request {
                assert_eq!(retained_key, &key);
                assert_eq!(retained_body, &body);
            } else {
                self.upload_request = Some((key, body));
            }
            if std::mem::take(&mut self.drop_reservation) {
                return None;
            }
            json!({"state":"pending","reservation":{"upload_id":UPLOAD,"artifact_id":INPUT},"transfer":{"status":"unavailable"}})
        } else if line.starts_with(&format!("GET /api/v1/uploads/{UPLOAD} ")) {
            json!({"state":if self.upload_complete {"completed"} else {"pending"},"reservation":{"upload_id":UPLOAD,"artifact_id":INPUT},"transfer":if self.upload_complete {json!({"status":"completed"})} else {json!({"status":"available","request":{"method":"PUT","url":format!("{}storage/input",self.origin),"headers":[]}})},"completion":if self.upload_complete {self.completion()} else {Value::Null}})
        } else if line.starts_with("PUT /storage/input ") {
            assert!(!request.to_ascii_lowercase().contains("authorization:"));
            self.upload_bytes = body.into();
            return Some((200, "x-amz-version-id: version-one\r\n".into(), vec![]));
        } else if line.starts_with(&format!("PUT /api/v1/uploads/{UPLOAD}/completion ")) {
            assert_eq!(
                serde_json::from_slice::<Value>(body).unwrap()["version_id"],
                "version-one"
            );
            self.upload_complete = true;
            if std::mem::take(&mut self.drop_completion) {
                return None;
            }
            self.completion()
        } else if line.starts_with(&format!("GET /api/v1/artifacts/{OUTPUT}/content ")) {
            return Some((
                303,
                format!("Location: {}storage/output\r\n", self.origin),
                vec![],
            ));
        } else if line.starts_with("GET /storage/output ") {
            assert!(!request.to_ascii_lowercase().contains("authorization:"));
            let range = request
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("range: bytes=")
                        .map(str::to_owned)
                })
                .unwrap();
            let (start, end) = range.split_once('-').unwrap();
            let start = start.parse::<usize>().unwrap();
            let end = end.parse::<usize>().unwrap();
            let headers = format!(
                "x-amz-version-id: version-one\r\nContent-Range: bytes {start}-{end}/{}\r\n",
                ANSWER.len()
            );
            if std::mem::take(&mut self.fail_download) {
                return Some((
                    206,
                    format!("{headers}X-Fixture-Truncated: 1\r\n"),
                    ANSWER[..5].to_vec(),
                ));
            }
            return Some((206, headers, ANSWER[start..=end].into()));
        } else if line.starts_with(&format!("GET /api/v1/artifacts/{INPUT} ")) {
            self.artifact(INPUT)
        } else if line.starts_with(&format!("GET /api/v1/artifacts/{OUTPUT} ")) {
            self.artifact(OUTPUT)
        } else {
            panic!("Unexpected request: {line}");
        };
        Some((
            200,
            "Content-Type: application/json\r\n".into(),
            response.to_string().into_bytes(),
        ))
    }
}
struct Server {
    origin: String,
    fixture: Arc<Mutex<Fixture>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new(files: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        let fixture = Arc::new(Mutex::new(Fixture {
            files,
            candidate: candidate(files),
            run_override: None,
            provenance_override: None,
            receipt_override: None,
            reject_accept: false,
            current: "release-one".into(),
            user: USER.into(),
            outcome: "succeeded".into(),
            accepted: None,
            requests: vec![],
            drop_accept: false,
            drop_reservation: false,
            drop_completion: false,
            fail_download: false,
            expired_authorization: false,
            drop_cancellation: false,
            deny_cancellation: false,
            cancellation: None,
            logs: vec![],
            stall_next: None,
            upload_complete: false,
            upload_request: None,
            upload_bytes: vec![],
            origin: origin.clone(),
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let (state, stopping) = (fixture.clone(), stop.clone());
        let worker = thread::spawn(move || {
            let mut connections = Vec::new();
            while !stopping.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("{error}"),
                };
                let state = state.clone();
                let connection = thread::spawn(move || {
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut bytes = vec![];
                    let mut byte = [0];
                    while !bytes.ends_with(b"\r\n\r\n") {
                        if stream.read_exact(&mut byte).is_err() {
                            break;
                        }
                        bytes.push(byte[0]);
                    }
                    let headers = String::from_utf8(bytes).unwrap();
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|s| s.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    let mut body = vec![0; length];
                    stream.read_exact(&mut body).unwrap();
                    let gate = {
                        let mut fixture = state.lock().unwrap();
                        if fixture
                            .stall_next
                            .as_ref()
                            .is_some_and(|(prefix, _)| headers.starts_with(prefix))
                        {
                            fixture.stall_next.take().map(|(_, gate)| gate)
                        } else {
                            None
                        }
                    };
                    if let Some(gate) = gate {
                        gate.arrive();
                    }
                    let reply = state.lock().unwrap().handle(&headers, &body);
                    if let Some((status, headers, bytes)) = reply {
                        let _ = write!(
                            stream,
                            "HTTP/1.1 {status} Test\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n",
                            if headers.contains("X-Fixture-Truncated") {
                                ANSWER.len()
                            } else {
                                bytes.len()
                            }
                        );
                        let _ = stream.write_all(&bytes);
                    }
                });
                connections.push(connection);
            }
            for connection in connections {
                connection.join().unwrap();
            }
        });
        Self {
            origin,
            fixture,
            stop,
            worker: Some(worker),
        }
    }
    fn call(&self, root: &Path, args: &[&str], success: bool) -> Output {
        let output = self.command(root).args(args).output().unwrap();
        assert_eq!(
            output.status.success(),
            success,
            "args {args:?}\nstdout {}\nstderr {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
    fn command(&self, root: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_enumeray"));
        command
            .current_dir(root)
            .args(["--api", &self.origin, "--config", "profiles.json", "--json"])
            .env("ENUMERAY_TOKEN", "test-token");
        command
    }
    fn start(&self, root: &Path, args: &[&str]) -> thread::JoinHandle<Output> {
        let mut command = self.command(root);
        command.args(args);
        thread::spawn(move || command.output().unwrap())
    }
    fn stall(&self, prefix: &str) -> RequestBarrier {
        let gate = Arc::new(RequestGate::default());
        self.fixture.lock().unwrap().stall_next = Some((prefix.into(), gate.clone()));
        RequestBarrier(gate)
    }
    fn init(&self, root: &Path, files: bool) {
        if files {
            fs::write(root.join("dataset.csv"), BYTES).unwrap();
        }
        let mut args = vec![
            "analysis",
            "init",
            "model",
            "--directory",
            "analysis",
            "--example",
            "example",
        ];
        if files {
            args.extend(["--file", "dataset=dataset.csv"]);
        }
        self.call(root, &args, true);
    }
    fn init_candidate(&self, root: &Path, files: bool) -> Value {
        if files {
            fs::write(root.join("dataset.csv"), BYTES).unwrap();
        }
        let mut args = vec![
            "analysis",
            "init",
            "model",
            "--candidate",
            "release-one",
            "--directory",
            "analysis",
            "--example",
            "example",
        ];
        if files {
            args.extend(["--file", "dataset=dataset.csv"]);
        }
        let output = self.call(root, &args, true);
        serde_json::from_slice(&output.stdout).unwrap()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn read(root: &Path, relative: &str) -> Value {
    serde_json::from_slice(&fs::read(root.join(relative)).unwrap()).unwrap()
}

fn private_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn prepared(origin: &str, files: bool, delegated_trial: bool) -> Value {
    let mut request = json!({"model_id":"model","expected_publication_id":"release-one","inputs":if files {json!({"dataset":{"artifact_id":INPUT}})} else {json!({"value":7})}});
    if delegated_trial {
        request["purpose"] = json!("candidate_trial");
        request["publication_delegation_id"] = json!(DELEGATION);
    }
    json!({"schema_version":1,"api_url":origin,"user_id":USER,"idempotency_key":"retained-original-run-key","request":request})
}

fn seed_accepted(server: &Server, files: bool, delegated_trial: bool) -> Value {
    let prepared = prepared(&server.origin, files, delegated_trial);
    server.fixture.lock().unwrap().accepted = Some((
        prepared["idempotency_key"].as_str().unwrap().into(),
        prepared["request"].clone(),
    ));
    json!({"run_id":RUN,"model_id":"model","publication_id":"release-one","purpose":if delegated_trial {"candidate_trial"} else {"standard"},"publication_delegation_id":if delegated_trial {json!(DELEGATION)} else {Value::Null},"state":"queued","created_at":"2026-10-16T12:00:00Z"})
}

fn copy_private_tree(source: &Path, destination: &Path) {
    fs::create_dir(destination).unwrap();
    fs::set_permissions(destination, fs::metadata(source).unwrap().permissions()).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_private_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
            fs::set_permissions(&target, entry.metadata().unwrap().permissions()).unwrap();
        }
    }
}

#[test]
fn prepared_import_is_offline_private_and_resumes_from_its_original_journal() {
    let root = tempfile::tempdir().unwrap();
    let source = prepared("https://original.example/", false, false);
    private_json(&root.path().join("prepared.json"), &source);
    fs::write(
        root.path().join("broken-config.json"),
        b"not valid configuration",
    )
    .unwrap();
    let before = fs::read(root.path().join("prepared.json")).unwrap();
    let import = || {
        Command::new(env!("CARGO_BIN_EXE_enumeray"))
            .current_dir(root.path())
            .env_remove("ENUMERAY_TOKEN")
            .args([
                "--config",
                "broken-config.json",
                "--json",
                "analysis",
                "import",
                "--prepared",
                "prepared.json",
                "--directory",
                "imported",
            ])
            .output()
            .unwrap()
    };
    let first = import();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let summary: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(summary["origin"], "imported_prepared");
    let saved = read(root.path(), "imported/analysis.json");
    assert_eq!(saved["kind"], "imported_prepared");
    assert_eq!(saved["operation"], source);
    assert!(saved["receipt"].is_null());
    let journal = fs::read(root.path().join("imported/recovery.json")).unwrap();
    fs::remove_file(root.path().join("imported/analysis.json")).unwrap();
    assert!(import().status.success());
    assert_eq!(read(root.path(), "imported/analysis.json"), saved);
    assert_eq!(
        fs::read(root.path().join("imported/recovery.json")).unwrap(),
        journal
    );
    let mut changed = source;
    changed["idempotency_key"] = json!("replacement-key");
    private_json(&root.path().join("prepared.json"), &changed);
    assert!(!import().status.success());
    assert_eq!(read(root.path(), "imported/analysis.json"), saved);
    assert_eq!(
        fs::read(root.path().join("imported/recovery.json")).unwrap(),
        journal
    );
    private_json(&root.path().join("prepared.json"), &saved["operation"]);
    assert_eq!(fs::read(root.path().join("prepared.json")).unwrap(), before);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(root.path().join("imported"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        for file in ["analysis.json", "recovery.json", "state/prepared-run.json"] {
            assert_eq!(
                fs::metadata(root.path().join("imported").join(file))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
}

#[test]
fn imported_lost_acceptance_replays_exact_inputs_key_and_trial_authority_without_uploads() {
    for (files, delegated_trial) in [(false, false), (true, false), (true, true)] {
        let server = Server::new(files);
        let root = tempfile::tempdir().unwrap();
        seed_accepted(&server, files, delegated_trial);
        let source = prepared(&server.origin, files, delegated_trial);
        private_json(&root.path().join("prepared.json"), &source);
        let before = fs::read(root.path().join("prepared.json")).unwrap();
        server.call(
            root.path(),
            &[
                "analysis",
                "import",
                "--prepared",
                "prepared.json",
                "--directory",
                "imported",
            ],
            true,
        );
        assert!(server.fixture.lock().unwrap().requests.is_empty());
        {
            let mut fixture = server.fixture.lock().unwrap();
            fixture.current = "replacement-publication".into();
            fixture.candidate["release_authority"]["available"] = json!(false);
            fixture.candidate["release_authority"]["publication_delegation_id"] = Value::Null;
            fixture.candidate["grant_id"] = json!(DELEGATION);
        }
        server.call(root.path(), &["analysis", "status", "imported"], true);
        server.call(root.path(), &["analysis", "logs", "imported"], false);
        assert!(
            !server
                .fixture
                .lock()
                .unwrap()
                .requests
                .iter()
                .any(|request| request.starts_with("POST "))
        );
        server.fixture.lock().unwrap().drop_accept = true;
        server.call(root.path(), &["analysis", "submit", "imported"], false);
        server.call(root.path(), &["analysis", "submit", "imported"], true);
        server.call(
            root.path(),
            &["analysis", "wait", "imported", "--check-outcome"],
            true,
        );
        server.call(root.path(), &["analysis", "collect", "imported"], true);
        assert_eq!(
            read(root.path(), "imported/state/receipt.json")["run_id"],
            RUN
        );
        assert_eq!(
            read(root.path(), "imported/analysis.json")["operation"],
            source
        );
        assert_eq!(read(root.path(), "imported/bundle.json")["complete"], true);
        assert_eq!(fs::read(root.path().join("prepared.json")).unwrap(), before);
        let fixture = server.fixture.lock().unwrap();
        assert_eq!(
            fixture.accepted.as_ref().unwrap().0,
            "retained-original-run-key"
        );
        assert_eq!(fixture.accepted.as_ref().unwrap().1, source["request"]);
        assert_eq!(
            fixture
                .requests
                .iter()
                .filter(|request| request.starts_with("POST /api/v1/runs "))
                .count(),
            2
        );
        assert!(
            !fixture
                .requests
                .iter()
                .any(|request| request.contains("/models/")
                    || request.contains("/uploads")
                    || request.starts_with("PUT /storage/"))
        );
        if files {
            assert_eq!(
                read(root.path(), "imported/bundle.json")["outputs"][SLOT]["version_id"],
                "version-one"
            );
        }
    }
}

#[test]
fn rejected_imported_acceptance_keeps_the_original_operation_for_explicit_retry() {
    let server = Server::new(false);
    let root = tempfile::tempdir().unwrap();
    let source = prepared(&server.origin, false, false);
    private_json(&root.path().join("prepared.json"), &source);
    server.call(
        root.path(),
        &[
            "analysis",
            "import",
            "--prepared",
            "prepared.json",
            "--directory",
            "imported",
        ],
        true,
    );
    server.fixture.lock().unwrap().current = "replacement-publication".into();
    server.call(root.path(), &["analysis", "submit", "imported"], false);
    assert!(server.fixture.lock().unwrap().accepted.is_none());
    assert_eq!(
        read(root.path(), "imported/analysis.json")["operation"],
        source
    );
    server.call(root.path(), &["analysis", "status", "imported"], true);
    assert!(server.fixture.lock().unwrap().accepted.is_none());
    server.fixture.lock().unwrap().current = "release-one".into();
    server.call(root.path(), &["analysis", "submit", "imported"], true);
    assert_eq!(
        server.fixture.lock().unwrap().accepted.as_ref().unwrap().0,
        "retained-original-run-key"
    );
    assert_eq!(
        read(root.path(), "imported/state/receipt.json")["run_id"],
        RUN
    );
}

#[test]
fn accepted_run_ids_and_receipts_attach_without_current_publication_or_local_sources() {
    for delegated_trial in [false, true] {
        for by_receipt in [false, true] {
            let server = Server::new(true);
            let root = tempfile::tempdir().unwrap();
            let receipt = seed_accepted(&server, true, delegated_trial);
            private_json(&root.path().join("receipt.json"), &receipt);
            let original_receipt = fs::read(root.path().join("receipt.json")).unwrap();
            server.fixture.lock().unwrap().current = "replacement-publication".into();
            server.fixture.lock().unwrap().candidate = Value::Null;
            let mut args = vec!["analysis", "attach"];
            if by_receipt {
                args.extend(["--receipt", "receipt.json"]);
            } else {
                args.push(RUN);
            }
            args.extend(["--directory", "attached"]);
            let output = server.call(root.path(), &args, true);
            assert_eq!(
                serde_json::from_slice::<Value>(&output.stdout).unwrap()["origin"],
                "attached_run"
            );
            let saved = read(root.path(), "attached/analysis.json");
            assert_eq!(saved["kind"], "attached_run");
            assert!(saved.get("operation").is_none());
            assert!(saved.get("run_key").is_none());
            assert!(saved.get("bindings").is_none());
            server.call(root.path(), &["analysis", "submit", "attached"], true);
            server.call(root.path(), &["analysis", "status", "attached"], true);
            server.call(root.path(), &["analysis", "logs", "attached"], true);
            server.call(
                root.path(),
                &["analysis", "wait", "attached", "--check-outcome"],
                true,
            );
            server.call(root.path(), &["analysis", "collect", "attached"], true);
            let bundle = read(root.path(), "attached/bundle.json");
            assert_eq!(bundle["run_id"], RUN);
            assert_eq!(bundle["outputs"][SLOT]["version_id"], "version-one");
            assert_eq!(bundle["complete"], true);
            assert_eq!(
                fs::read(root.path().join("receipt.json")).unwrap(),
                original_receipt
            );
            let fixture = server.fixture.lock().unwrap();
            assert!(
                !fixture
                    .requests
                    .iter()
                    .any(|request| request.starts_with("POST ")
                        || request.contains("/models/")
                        || request.contains("/uploads")
                        || request.starts_with("PUT /storage/"))
            );
        }
    }
}

#[test]
fn copied_accepted_folders_keep_working_without_the_original_absolute_sources() {
    for completed in [false, true] {
        let server = Server::new(true);
        let original = tempfile::tempdir().unwrap();
        server.init(original.path(), true);
        server.call(original.path(), &["analysis", "submit", "analysis"], true);
        server.fixture.lock().unwrap().fail_download = !completed;
        server.call(
            original.path(),
            &["analysis", "collect", "analysis"],
            completed,
        );
        let original_checkpoint = enumeray_client::bulk::checkpoint_directory(
            &original
                .path()
                .join(format!("analysis/results/files/{OUTPUT}")),
        )
        .unwrap();
        let relative_checkpoint = original_checkpoint
            .strip_prefix(fs::canonicalize(original.path()).unwrap())
            .unwrap()
            .to_path_buf();
        let checkpoint = fs::read(original_checkpoint.join("state.json")).unwrap();
        let payload = fs::read(original_checkpoint.join("payload.part")).ok();
        assert_eq!(
            payload.as_ref().map(Vec::len),
            if completed { None } else { Some(5) }
        );
        let recovered = tempfile::tempdir().unwrap();
        copy_private_tree(
            &original.path().join("analysis"),
            &recovered.path().join("analysis"),
        );
        let copied_checkpoint = recovered.path().join(relative_checkpoint);
        assert_eq!(
            fs::read(copied_checkpoint.join("state.json")).unwrap(),
            checkpoint
        );
        assert_eq!(
            fs::read(copied_checkpoint.join("payload.part")).ok(),
            payload
        );
        original.close().unwrap();
        {
            let mut fixture = server.fixture.lock().unwrap();
            fixture.current = "replacement-publication".into();
            fixture.requests.clear();
        }
        for command in ["submit", "status", "logs", "wait", "collect"] {
            server.call(recovered.path(), &["analysis", command, "analysis"], true);
        }
        assert_eq!(
            read(recovered.path(), "analysis/bundle.json")["complete"],
            true
        );
        assert_eq!(
            fs::read(
                recovered
                    .path()
                    .join(format!("analysis/results/files/{OUTPUT}"))
            )
            .unwrap(),
            ANSWER
        );
        // A checkpoint tied to the old absolute destination remains retained evidence.
        assert_eq!(
            fs::read(copied_checkpoint.join("state.json")).unwrap(),
            checkpoint
        );
        assert_eq!(
            fs::read(copied_checkpoint.join("payload.part")).ok(),
            payload
        );
        let fixture = server.fixture.lock().unwrap();
        assert!(
            !fixture
                .requests
                .iter()
                .any(|request| request.starts_with("POST ")
                    || request.contains("/models/")
                    || request.contains("/uploads"))
        );
        if completed {
            assert!(
                !fixture
                    .requests
                    .iter()
                    .any(|request| request.starts_with("GET /storage/output "))
            );
        } else {
            assert!(
                fixture
                    .requests
                    .iter()
                    .any(|request| request.starts_with("GET /storage/output "))
            );
        }
    }
}

#[test]
fn attachment_is_resumable_after_interruption_and_rejects_conflicting_recovery_sources() {
    let server = Server::new(false);
    let root = tempfile::tempdir().unwrap();
    seed_accepted(&server, false, false);
    let gate = server.stall(&format!("GET /api/v1/runs/{RUN}/provenance "));
    let args = ["analysis", "attach", RUN, "--directory", "attached"];
    let mut child = server
        .command(root.path())
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    gate.wait();
    assert!(root.path().join("attached/recovery.json").exists());
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
    gate.release();
    let journal = fs::read(root.path().join("attached/recovery.json")).unwrap();
    server.call(root.path(), &args, true);
    server.call(root.path(), &args, true);
    assert_eq!(
        fs::read(root.path().join("attached/recovery.json")).unwrap(),
        journal
    );
    let saved = fs::read(root.path().join("attached/analysis.json")).unwrap();
    server.call(
        root.path(),
        &["analysis", "attach", INPUT, "--directory", "attached"],
        false,
    );
    assert_eq!(
        fs::read(root.path().join("attached/analysis.json")).unwrap(),
        saved
    );
    assert_eq!(
        fs::read(root.path().join("attached/recovery.json")).unwrap(),
        journal
    );
    assert!(
        !server
            .fixture
            .lock()
            .unwrap()
            .requests
            .iter()
            .any(|request| request.starts_with("POST "))
    );
}

#[test]
fn attached_evidence_rejects_wrong_api_account_receipt_and_acceptance_tampering() {
    let server = Server::new(false);
    let root = tempfile::tempdir().unwrap();
    let mut receipt = seed_accepted(&server, false, false);
    receipt["publication_id"] = json!("different-publication");
    private_json(&root.path().join("receipt.json"), &receipt);
    server.call(
        root.path(),
        &[
            "analysis",
            "attach",
            "--receipt",
            "receipt.json",
            "--directory",
            "conflict",
        ],
        false,
    );
    assert!(!root.path().join("conflict/bundle.json").exists());
    server.call(
        root.path(),
        &["analysis", "attach", RUN, "--directory", "attached"],
        true,
    );
    let saved = read(root.path(), "attached/analysis.json");
    let other = Server::new(false);
    other.call(root.path(), &["analysis", "status", "attached"], false);
    assert_eq!(other.fixture.lock().unwrap().requests.len(), 1);
    server.fixture.lock().unwrap().user = DELEGATION.into();
    server.call(root.path(), &["analysis", "status", "attached"], false);
    server.call(root.path(), &["analysis", "submit", "attached"], false);
    server.fixture.lock().unwrap().user = USER.into();
    let mut changed = saved.clone();
    changed["accepted"]["effective_inputs"] = json!({"value":99});
    private_json(&root.path().join("attached/analysis.json"), &changed);
    server.call(root.path(), &["analysis", "status", "attached"], false);
    private_json(&root.path().join("attached/analysis.json"), &saved);
    server.call(root.path(), &["analysis", "status", "attached"], true);
    assert!(
        !server
            .fixture
            .lock()
            .unwrap()
            .requests
            .iter()
            .any(|request| request.starts_with("POST "))
    );
}

#[test]
fn recovery_validates_selectors_and_local_formats_before_replacing_any_destination() {
    let server = Server::new(false);
    let root = tempfile::tempdir().unwrap();
    private_json(
        &root.path().join("receipt.json"),
        &seed_accepted(&server, false, false),
    );
    for args in [
        vec!["analysis", "attach", "--directory", "missing"],
        vec![
            "analysis",
            "attach",
            RUN,
            "--receipt",
            "receipt.json",
            "--directory",
            "both",
        ],
        vec![
            "analysis",
            "attach",
            "not-a-run-id",
            "--directory",
            "bad-id",
        ],
    ] {
        server.call(root.path(), &args, false);
    }
    let source = prepared(&server.origin, false, false);
    for (index, (pointer, value)) in [
        ("/schema_version", json!(999)),
        ("/user_id", json!("not-an-account")),
        ("/idempotency_key", json!("")),
        ("/api_url", json!("not-an-api-origin")),
        ("/request/purpose", json!("another-purpose")),
        ("/request/expected_publication_id", json!("")),
    ]
    .into_iter()
    .enumerate()
    {
        let mut malformed = source.clone();
        if pointer == "/request/purpose" {
            malformed["request"]["purpose"] = value;
        } else {
            *malformed.pointer_mut(pointer).unwrap() = value;
        }
        private_json(&root.path().join("prepared.json"), &malformed);
        let destination = format!("malformed-{index}");
        server.call(
            root.path(),
            &[
                "analysis",
                "import",
                "--prepared",
                "prepared.json",
                "--directory",
                &destination,
            ],
            false,
        );
        assert!(!root.path().join(destination).exists());
    }
    private_json(&root.path().join("prepared.json"), &source);
    fs::create_dir(root.path().join("existing")).unwrap();
    fs::write(root.path().join("existing/keep"), b"retain me").unwrap();
    server.call(
        root.path(),
        &[
            "analysis",
            "import",
            "--prepared",
            "prepared.json",
            "--directory",
            "existing",
        ],
        false,
    );
    server.call(
        root.path(),
        &["analysis", "attach", RUN, "--directory", "existing"],
        false,
    );
    assert_eq!(
        fs::read(root.path().join("existing/keep")).unwrap(),
        b"retain me"
    );
    assert!(!root.path().join("existing/analysis.json").exists());
    // Offline import retains bounded JSON without guessing the absent contract.
    let mut generic = source;
    generic["request"]["inputs"] = json!([1, 2]);
    private_json(&root.path().join("generic.json"), &generic);
    server.call(
        root.path(),
        &[
            "analysis",
            "import",
            "--prepared",
            "generic.json",
            "--directory",
            "generic",
        ],
        true,
    );
    assert_eq!(
        read(root.path(), "generic/analysis.json")["operation"],
        generic
    );
    assert!(
        !server
            .fixture
            .lock()
            .unwrap()
            .requests
            .iter()
            .any(|request| request.starts_with("POST "))
    );
}

#[test]
fn candidate_examples_and_supplied_inputs_work_without_any_current_publication() {
    for delegated in [false, true] {
        let server = Server::new(false);
        let root = tempfile::tempdir().unwrap();
        {
            let mut fixture = server.fixture.lock().unwrap();
            fixture.current.clear();
            if delegated {
                fixture.candidate["release_authority"]["grant_holder_user_id"] = json!(GRANT);
                fixture.candidate["release_authority"]["publication_delegation_id"] =
                    json!(DELEGATION);
                fixture.candidate["release_authority"]["can_delegate"] = json!(false);
            }
        }
        let initialization = if delegated {
            fs::write(root.path().join("supplied.json"), br#"{"value":9}"#).unwrap();
            let output = server.call(
                root.path(),
                &[
                    "analysis",
                    "init",
                    "model",
                    "--candidate",
                    "release-one",
                    "--directory",
                    "analysis",
                    "--inputs",
                    "supplied.json",
                ],
                true,
            );
            serde_json::from_slice::<Value>(&output.stdout).unwrap()
        } else {
            server.init_candidate(root.path(), false)
        };
        assert_eq!(initialization["purpose"], "candidate_trial");
        assert_eq!(initialization["grant_id"], GRANT);
        assert_eq!(initialization["publication_id"], "release-one");
        let publication = server.fixture.lock().unwrap().candidate.clone();
        for field in ["context_id", "execution_location", "disclosure"] {
            assert_eq!(initialization[field], publication[field]);
        }
        assert_eq!(initialization["installation_status"], "ready");
        assert_eq!(initialization["installation"], publication["descriptor"]);
        assert!(server.fixture.lock().unwrap().accepted.is_none());
        let output = server.call(root.path(), &["analysis", "submit", "analysis"], true);
        let summary: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(summary["receipt"]["purpose"], "candidate_trial");
        assert_eq!(
            summary["publication_delegation_id"],
            if delegated {
                json!(DELEGATION)
            } else {
                Value::Null
            }
        );
        server.call(root.path(), &["analysis", "status", "analysis"], true);
        server.call(
            root.path(),
            &["analysis", "wait", "analysis", "--check-outcome"],
            true,
        );
        server.call(root.path(), &["analysis", "collect", "analysis"], true);
        let bundle = read(root.path(), "analysis/bundle.json");
        assert_eq!(bundle["purpose"], "candidate_trial");
        assert_eq!(bundle["grant_id"], GRANT);
        assert_eq!(
            bundle["publication_delegation_id"],
            if delegated {
                json!(DELEGATION)
            } else {
                Value::Null
            }
        );
        assert_eq!(
            read(root.path(), "analysis/results/outputs.json")["doubled"],
            if delegated { 18 } else { 4 }
        );
        let fixture = server.fixture.lock().unwrap();
        assert!(
            fixture
                .requests
                .iter()
                .all(|line| !line.starts_with("GET /api/v1/models/model "))
        );
        assert_eq!(
            fixture
                .requests
                .iter()
                .filter(|line| line.starts_with("POST /api/v1/runs "))
                .count(),
            1
        );
    }
}

#[test]
fn trial_file_recovery_replays_exact_authority_after_activation_replacement_and_revocation() {
    let server = Server::new(true);
    let root = tempfile::tempdir().unwrap();
    server.fixture.lock().unwrap().candidate["release_authority"]["publication_delegation_id"] =
        json!(DELEGATION);
    server.init_candidate(root.path(), true);
    server.fixture.lock().unwrap().drop_accept = true;
    server.call(root.path(), &["analysis", "submit", "analysis"], false);
    let original = read(root.path(), "analysis/state/prepared-run.json");
    let inspections = {
        let mut fixture = server.fixture.lock().unwrap();
        fixture.current = "replacement".into();
        fixture.candidate["status"] = json!("published");
        fixture.candidate["trial_availability"] =
            json!({"available":false,"unavailable_reason":"captured_grant_or_delegation_required"});
        fixture.candidate["release_authority"]["publication_delegation_id"] = json!(GRANT);
        fixture
            .requests
            .iter()
            .filter(|line| line.contains("/publications/release-one "))
            .count()
    };
    fs::remove_file(root.path().join("dataset.csv")).unwrap();
    server.call(root.path(), &["analysis", "submit", "analysis"], true);
    assert_eq!(
        read(root.path(), "analysis/state/prepared-run.json"),
        original
    );
    // The receipt was published before the journal advanced; recovery must not POST again.
    let mut saved = read(root.path(), "analysis/analysis.json");
    saved["receipt"] = Value::Null;
    fs::write(
        root.path().join("analysis/analysis.json"),
        serde_json::to_vec(&saved).unwrap(),
    )
    .unwrap();
    server.call(root.path(), &["analysis", "status", "analysis"], true);
    server.call(root.path(), &["analysis", "submit", "analysis"], true);
    server.call(root.path(), &["analysis", "collect", "analysis"], true);
    let bundle = read(root.path(), "analysis/bundle.json");
    assert_eq!(bundle["publication_delegation_id"], DELEGATION);
    assert_eq!(bundle["outputs"][SLOT]["version_id"], "version-one");
    let fixture = server.fixture.lock().unwrap();
    assert_eq!(
        fixture
            .requests
            .iter()
            .filter(|line| line.contains("/publications/release-one "))
            .count(),
        inspections
    );
    assert_eq!(
        fixture
            .requests
            .iter()
            .filter(|line| line.starts_with("POST /api/v1/runs "))
            .count(),
        2
    );
    assert_eq!(
        fixture
            .requests
            .iter()
            .filter(|line| line.starts_with("PUT /storage/input "))
            .count(),
        1
    );
}

#[test]
fn exact_trial_accepts_mutable_readiness_observations_without_following_current() {
    let server = Server::new(false);
    let root = tempfile::tempdir().unwrap();
    server.init_candidate(root.path(), false);
    {
        let mut fixture = server.fixture.lock().unwrap();
        fixture.current = "replacement".into();
        fixture.candidate["status"] = json!("published");
        fixture.candidate["generation"] = json!(99);
        fixture.candidate["trial_evidence"] = json!({"counts":{"succeeded":42}});
        fixture.reject_accept = true;
    }
    server.call(root.path(), &["analysis", "submit", "analysis"], false);
    assert!(server.fixture.lock().unwrap().accepted.is_none());
    let original = read(root.path(), "analysis/state/prepared-run.json");
    server.call(root.path(), &["analysis", "submit", "analysis"], true);
    assert_eq!(
        read(root.path(), "analysis/state/prepared-run.json"),
        original
    );
    assert_eq!(
        original["request"]["expected_publication_id"],
        "release-one"
    );
    assert_eq!(original["request"]["purpose"], "candidate_trial");
}

#[test]
fn trial_blockers_and_incomplete_authority_fail_before_directory_or_upload_allocation() {
    for (pointer, value) in [
        (
            "/trial_availability",
            json!({"available":false,"unavailable_reason":"installation_not_ready"}),
        ),
        (
            "/trial_availability",
            json!({"available":false,"unavailable_reason":"insufficient_scope"}),
        ),
        (
            "/trial_availability",
            json!({"available":false,"unavailable_reason":"captured_grant_required"}),
        ),
        (
            "/trial_availability",
            json!({"available":false,"unavailable_reason":"configuration_retiring"}),
        ),
        ("/release_authority", Value::Null),
        ("/grant_id", json!("invalid-grant")),
        ("/model_id", json!("another-model")),
        ("/id", json!("another-publication")),
        ("/disclosure", json!("")),
    ] {
        let server = Server::new(false);
        let root = tempfile::tempdir().unwrap();
        *server
            .fixture
            .lock()
            .unwrap()
            .candidate
            .pointer_mut(pointer)
            .unwrap() = value;
        server.call(
            root.path(),
            &[
                "analysis",
                "init",
                "model",
                "--candidate",
                "release-one",
                "--directory",
                "analysis",
                "--example",
                "example",
            ],
            false,
        );
        assert!(!root.path().join("analysis").exists(), "{pointer}");
        assert!(
            server
                .fixture
                .lock()
                .unwrap()
                .requests
                .iter()
                .all(|line| line.starts_with("GET "))
        );
    }
}

#[test]
fn fresh_trial_refuses_changed_grant_delegation_or_authority_before_uploads() {
    for (pointer, value) in [
        ("/grant_id", json!(DELEGATION)),
        ("/release_authority/publication_delegation_id", json!(GRANT)),
        ("/execution_location", json!("Different compute")),
        (
            "/trial_availability",
            json!({"available":false,"unavailable_reason":"captured_grant_or_delegation_required"}),
        ),
        (
            "/trial_availability",
            json!({"available":false,"unavailable_reason":"connector_draining"}),
        ),
    ] {
        let server = Server::new(true);
        let root = tempfile::tempdir().unwrap();
        server.fixture.lock().unwrap().candidate["release_authority"]["publication_delegation_id"] =
            json!(DELEGATION);
        server.init_candidate(root.path(), true);
        let original = read(root.path(), "analysis/analysis.json");
        *server
            .fixture
            .lock()
            .unwrap()
            .candidate
            .pointer_mut(pointer)
            .unwrap() = value;
        server.call(root.path(), &["analysis", "submit", "analysis"], false);
        assert_eq!(
            read(root.path(), "analysis/analysis.json"),
            original,
            "{pointer}"
        );
        assert!(
            server
                .fixture
                .lock()
                .unwrap()
                .requests
                .iter()
                .all(|line| line.starts_with("GET "))
        );
    }
}

#[test]
fn trial_receipt_run_and_provenance_must_match_saved_purpose_grant_and_delegation() {
    let server = Server::new(false);
    let root = tempfile::tempdir().unwrap();
    server.fixture.lock().unwrap().candidate["release_authority"]["publication_delegation_id"] =
        json!(DELEGATION);
    server.init_candidate(root.path(), false);
    for (field, value) in [
        ("purpose", json!("standard")),
        ("publication_delegation_id", Value::Null),
    ] {
        server.fixture.lock().unwrap().receipt_override = Some((field.into(), value));
        server.call(root.path(), &["analysis", "submit", "analysis"], false);
        assert!(!root.path().join("analysis/state/receipt.json").exists());
    }
    server.fixture.lock().unwrap().receipt_override = None;
    server.call(root.path(), &["analysis", "submit", "analysis"], true);
    for (field, value) in [
        ("purpose", json!("standard")),
        ("publication_delegation_id", Value::Null),
    ] {
        server.fixture.lock().unwrap().run_override = Some((field.into(), value));
        server.call(root.path(), &["analysis", "status", "analysis"], false);
        server.call(root.path(), &["analysis", "wait", "analysis"], false);
        server.call(root.path(), &["analysis", "collect", "analysis"], false);
    }
    server.fixture.lock().unwrap().run_override = None;
    for (pointer, value) in [
        ("/run/purpose", json!("standard")),
        ("/run/publication_delegation_id", Value::Null),
        ("/publication/compute_grant_id", json!(DELEGATION)),
    ] {
        server.fixture.lock().unwrap().provenance_override = Some((pointer.into(), value));
        server.call(root.path(), &["analysis", "collect", "analysis"], false);
        assert!(!root.path().join("analysis/bundle.json").exists());
    }
    server.fixture.lock().unwrap().provenance_override = None;
    server.call(root.path(), &["analysis", "collect", "analysis"], true);
}

#[test]
fn trial_saved_intent_cannot_be_downgraded_or_rebound() {
    let server = Server::new(false);
    let root = tempfile::tempdir().unwrap();
    server.init_candidate(root.path(), false);
    let original = read(root.path(), "analysis/analysis.json");
    for (pointer, value) in [
        ("/purpose", json!("standard")),
        ("/publication_delegation_id", json!(DELEGATION)),
        ("/publication/grant_id", json!(DELEGATION)),
    ] {
        let mut changed = original.clone();
        changed[pointer.trim_start_matches('/')] = value.clone();
        if pointer.starts_with("/publication/") {
            changed = original.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
        }
        fs::write(
            root.path().join("analysis/analysis.json"),
            serde_json::to_vec(&changed).unwrap(),
        )
        .unwrap();
        server.call(root.path(), &["analysis", "submit", "analysis"], false);
    }
    fs::write(
        root.path().join("analysis/analysis.json"),
        serde_json::to_vec(&original).unwrap(),
    )
    .unwrap();
    server.call(root.path(), &["analysis", "submit", "analysis"], true);
    let original = read(root.path(), "analysis/analysis.json");
    let mut changed = original.clone();
    changed["prepared"]["request"]["purpose"] = json!("standard");
    fs::write(
        root.path().join("analysis/analysis.json"),
        serde_json::to_vec(&changed).unwrap(),
    )
    .unwrap();
    fs::write(
        root.path().join("analysis/state/prepared-run.json"),
        serde_json::to_vec(&changed["prepared"]).unwrap(),
    )
    .unwrap();
    server.call(root.path(), &["analysis", "status", "analysis"], false);
}

#[test]
fn ordinary_folders_without_new_trial_fields_still_load() {
    let server = Server::new(false);
    let root = tempfile::tempdir().unwrap();
    server.init(root.path(), false);
    let mut saved = read(root.path(), "analysis/analysis.json");
    saved.as_object_mut().unwrap().remove("purpose");
    saved
        .as_object_mut()
        .unwrap()
        .remove("publication_delegation_id");
    fs::write(
        root.path().join("analysis/analysis.json"),
        serde_json::to_vec(&saved).unwrap(),
    )
    .unwrap();
    server.call(root.path(), &["analysis", "submit", "analysis"], true);
    let collected = server.call(root.path(), &["analysis", "collect", "analysis"], true);
    assert_eq!(
        serde_json::from_slice::<Value>(&collected.stdout).unwrap()["purpose"],
        "standard"
    );
    let historical = fs::read(root.path().join("analysis/bundle.json")).unwrap();
    assert!(
        read(root.path(), "analysis/bundle.json")
            .get("purpose")
            .is_none()
    );
    server.call(root.path(), &["analysis", "collect", "analysis"], true);
    assert_eq!(
        fs::read(root.path().join("analysis/bundle.json")).unwrap(),
        historical
    );
}

#[test]
fn delegated_trial_following_allows_cancellation_and_keeps_failed_outcome_evidence() {
    let server = Server::new(false);
    let root = tempfile::tempdir().unwrap();
    server.fixture.lock().unwrap().candidate["release_authority"]["publication_delegation_id"] =
        json!(DELEGATION);
    server.init_candidate(root.path(), false);
    server.call(root.path(), &["analysis", "submit", "analysis"], true);
    {
        let mut fixture = server.fixture.lock().unwrap();
        fixture.outcome = "running".into();
        fixture.logs = vec![
            json!({"position":1,"assignment_id":UPLOAD,"source":"model.progress","epoch":1,"sequence":1,"offset":0,"byte_length":14,"text":"Trial running\n","gap":null,"source_reset":false,"received_at":"2026-10-19T12:00:00Z"}),
        ];
    }
    let gate = server.stall(&format!("GET /api/v1/runs/{RUN} "));
    let follower = server.start(
        root.path(),
        &[
            "analysis",
            "logs",
            "analysis",
            "--follow",
            "--deadline",
            "5",
            "--interval",
            "1",
        ],
    );
    gate.wait();
    let status = server.call(root.path(), &["analysis", "status", "analysis"], true);
    assert_eq!(
        serde_json::from_slice::<Value>(&status.stdout).unwrap()["purpose"],
        "candidate_trial"
    );
    let logs = server.call(
        root.path(),
        &["analysis", "logs", "analysis", "--limit", "1"],
        true,
    );
    let page: Value = serde_json::from_slice(&logs.stdout).unwrap();
    assert_eq!(page["publication_delegation_id"], DELEGATION);
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    server.call(root.path(), &["analysis", "cancel", "analysis"], true);
    server.fixture.lock().unwrap().outcome = "cancelled".into();
    gate.release();
    let output = follower.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(events.last().unwrap()["data"]["purpose"], "candidate_trial");
    assert_eq!(
        events.last().unwrap()["data"]["publication_delegation_id"],
        DELEGATION
    );
    let wait = server.call(
        root.path(),
        &["analysis", "wait", "analysis", "--check-outcome"],
        false,
    );
    assert_eq!(wait.status.code(), Some(4));
    server.call(
        root.path(),
        &["analysis", "collect", "analysis", "--evidence-only"],
        true,
    );
    let bundle = read(root.path(), "analysis/bundle.json");
    assert_eq!(bundle["scientific_state"], "cancelled");
    assert_eq!(bundle["purpose"], "candidate_trial");
    assert_eq!(bundle["publication_delegation_id"], DELEGATION);
}

#[test]
fn editable_scalar_inputs_freeze_and_recover_lost_acceptance_after_publication_change() {
    let server = Server::new(false);
    let root = tempfile::tempdir().unwrap();
    let initialized = server.call(
        root.path(),
        &[
            "analysis",
            "init",
            "model",
            "--directory",
            "analysis",
            "--example",
            "example",
        ],
        true,
    );
    let initialization: Value = serde_json::from_slice(&initialized.stdout).unwrap();
    let publication = server.fixture.lock().unwrap().candidate.clone();
    assert_eq!(initialization["publication_id"], "release-one");
    assert_eq!(initialization["stage"], "editable");
    for field in ["context_id", "execution_location", "disclosure"] {
        assert_eq!(initialization[field], publication[field]);
    }
    let initial = read(root.path(), "analysis/analysis.json");
    assert_eq!(initial["schema_version"], 1);
    assert_eq!(initial["api_url"], server.origin);
    assert_eq!(initial["user_id"], USER);
    assert!(server.fixture.lock().unwrap().accepted.is_none());
    fs::write(root.path().join("analysis/inputs.json"), r#"{"value":7}"#).unwrap();
    server.fixture.lock().unwrap().drop_accept = true;
    server.call(root.path(), &["analysis", "submit", "analysis"], false);
    let saved = read(root.path(), "analysis/analysis.json");
    assert!(saved["submission_started"].as_bool().unwrap());
    let prepared = read(root.path(), "analysis/state/prepared-run.json");
    assert_eq!(prepared["schema_version"], 1);
    assert_eq!(prepared["api_url"], server.origin);
    assert_eq!(prepared["user_id"], USER);
    assert_eq!(prepared["idempotency_key"], initial["run_key"]);
    assert_eq!(
        prepared["request"],
        json!({"model_id":"model","expected_publication_id":"release-one","inputs":{"value":7}})
    );
    assert!(prepared.get("disclosure").is_none());
    server.fixture.lock().unwrap().current = "release-two".into();
    let accepted = server.call(root.path(), &["analysis", "submit", "analysis"], true);
    let receipt = read(root.path(), "analysis/state/receipt.json");
    assert_eq!(receipt["run_id"], RUN);
    let accepted_summary: Value = serde_json::from_slice(&accepted.stdout).unwrap();
    assert_eq!(accepted_summary["stage"], "accepted");
    assert_eq!(accepted_summary["receipt"], receipt);
    server.call(root.path(), &["analysis", "submit", "analysis"], true);
    assert_eq!(read(root.path(), "analysis/state/receipt.json"), receipt);
    server.call(root.path(), &["analysis", "status", "analysis"], true);
    server.call(
        root.path(),
        &["analysis", "wait", "analysis", "--check-outcome"],
        true,
    );
    server.call(root.path(), &["analysis", "collect", "analysis"], true);
    server.call(root.path(), &["analysis", "collect", "analysis"], true);
    assert_eq!(
        read(root.path(), "analysis/results/outputs.json"),
        json!({"doubled":14})
    );
    assert_eq!(read(root.path(), "analysis/bundle.json")["complete"], true);
    let state = server.fixture.lock().unwrap();
    assert_eq!(
        state
            .requests
            .iter()
            .filter(|request| request.starts_with("POST /api/v1/runs "))
            .count(),
        2
    );
    assert_eq!(
        state.accepted.as_ref().unwrap().1["inputs"],
        json!({"value":7})
    );
}

#[test]
fn file_upload_recovers_reservation_and_completion_and_collection_preserves_conflicts() {
    let server = Server::new(true);
    let root = tempfile::tempdir().unwrap();
    server.init(root.path(), true);
    server.fixture.lock().unwrap().drop_reservation = true;
    server.call(root.path(), &["analysis", "submit", "analysis"], false);
    server.fixture.lock().unwrap().drop_completion = true;
    server.call(root.path(), &["analysis", "submit", "analysis"], false);
    server.fixture.lock().unwrap().drop_accept = true;
    server.call(root.path(), &["analysis", "submit", "analysis"], false);
    assert_eq!(server.fixture.lock().unwrap().upload_bytes, BYTES);
    fs::remove_file(root.path().join("dataset.csv")).unwrap();
    server.fixture.lock().unwrap().current = "release-two".into();
    server.call(root.path(), &["analysis", "submit", "analysis"], true);
    server.fixture.lock().unwrap().fail_download = true;
    server.call(root.path(), &["analysis", "collect", "analysis"], false);
    assert!(!root.path().join("analysis/bundle.json").exists());
    let checkpoint = enumeray_client::bulk::checkpoint_directory(
        &root.path().join(format!("analysis/results/files/{OUTPUT}")),
    )
    .unwrap()
    .join("state.json");
    let retained: Value = serde_json::from_slice(&fs::read(checkpoint).unwrap()).unwrap();
    assert_eq!(retained["offset"], 5);
    server.fixture.lock().unwrap().expired_authorization = true;
    let expired = server.call(root.path(), &["analysis", "collect", "analysis"], false);
    assert!(String::from_utf8_lossy(&expired.stderr).contains("auth refresh/login"));
    server.fixture.lock().unwrap().expired_authorization = false;
    server.call(root.path(), &["analysis", "collect", "analysis"], true);
    let bundle = read(root.path(), "analysis/bundle.json");
    assert_eq!(
        bundle["outputs"][SLOT]["path"],
        format!("results/files/{OUTPUT}")
    );
    assert_eq!(
        fs::read(root.path().join(format!("analysis/results/files/{OUTPUT}"))).unwrap(),
        ANSWER
    );
    assert!(!root.path().join("hostile%output").exists());
    // Adopt a verified file even when the last journal advancement was lost.
    let collection = root.path().join("analysis/state/collection.json");
    let mut plan = read(root.path(), "analysis/state/collection.json");
    plan["outputs"][SLOT]["verified"] = json!(false);
    plan["complete"] = json!(false);
    fs::write(&collection, serde_json::to_vec(&plan).unwrap()).unwrap();
    server.call(root.path(), &["analysis", "collect", "analysis"], true);
    fs::write(
        root.path().join(format!("analysis/results/files/{OUTPUT}")),
        b"unrelated",
    )
    .unwrap();
    server.call(root.path(), &["analysis", "collect", "analysis"], false);
    assert_eq!(
        fs::read(root.path().join(format!("analysis/results/files/{OUTPUT}"))).unwrap(),
        b"unrelated"
    );
    assert_eq!(
        server
            .fixture
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|r| r.starts_with("PUT /storage/input "))
            .count(),
        1
    );
}

#[test]
fn lost_upload_completion_reply_recovers_completed_artifact_without_the_original_source() {
    let server = Server::new(true);
    let root = tempfile::tempdir().unwrap();
    server.init(root.path(), true);
    server.fixture.lock().unwrap().drop_completion = true;
    server.call(root.path(), &["analysis", "submit", "analysis"], false);
    let saved = read(root.path(), "analysis/analysis.json");
    assert!(saved["prepared"].is_null());
    assert!(saved["uploads"]["dataset"].is_null());
    assert!(
        !root
            .path()
            .join("analysis/state/prepared-run.json")
            .exists()
    );
    assert!(server.fixture.lock().unwrap().upload_complete);
    fs::remove_file(root.path().join("dataset.csv")).unwrap();
    server.call(root.path(), &["analysis", "submit", "analysis"], true);
    server.call(root.path(), &["analysis", "collect", "analysis"], true);
    assert_eq!(
        read(root.path(), "analysis/state/receipt.json")["run_id"],
        RUN
    );
    assert_eq!(read(root.path(), "analysis/bundle.json")["complete"], true);
    let fixture = server.fixture.lock().unwrap();
    assert_eq!(
        fixture
            .requests
            .iter()
            .filter(|request| request.starts_with("PUT /storage/input "))
            .count(),
        1
    );
    assert_eq!(
        fixture
            .requests
            .iter()
            .filter(|request| request.starts_with("POST /api/v1/uploads "))
            .count(),
        1
    );
    assert_eq!(
        fixture
            .requests
            .iter()
            .filter(|request| request.starts_with("POST /api/v1/runs "))
            .count(),
        1
    );
    assert_eq!(
        fixture.accepted.as_ref().unwrap().1["inputs"]["dataset"]["artifact_id"],
        INPUT
    );
}

#[test]
fn changed_sources_inputs_publications_and_accounts_cannot_create_replacement_work() {
    let server = Server::new(true);
    let root = tempfile::tempdir().unwrap();
    server.init(root.path(), true);
    fs::write(root.path().join("dataset.csv"), b"changed").unwrap();
    server.call(root.path(), &["analysis", "submit", "analysis"], false);
    fs::write(root.path().join("dataset.csv"), BYTES).unwrap();
    server.fixture.lock().unwrap().current = "release-two".into();
    server.call(root.path(), &["analysis", "submit", "analysis"], false);
    server.fixture.lock().unwrap().current = "release-one".into();
    server.fixture.lock().unwrap().drop_reservation = true;
    server.call(root.path(), &["analysis", "submit", "analysis"], false);
    fs::write(root.path().join("analysis/inputs.json"), r#"{}"#).unwrap();
    server.call(root.path(), &["analysis", "submit", "analysis"], false);
    fs::write(
        root.path().join("analysis/inputs.json"),
        serde_json::to_vec(&json!({"dataset":{"artifact_id":INPUT}})).unwrap(),
    )
    .unwrap();
    server.fixture.lock().unwrap().user = OUTPUT.into();
    server.call(root.path(), &["analysis", "submit", "analysis"], false);
    assert!(server.fixture.lock().unwrap().accepted.is_none());
    assert!(server.fixture.lock().unwrap().upload_bytes.is_empty());
}

#[test]
fn supplied_existing_artifact_requires_no_upload_and_human_output_exposes_review() {
    let server = Server::new(true);
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("inputs.json"),
        serde_json::to_vec(&json!({"dataset":{"artifact_id":INPUT}})).unwrap(),
    )
    .unwrap();
    server.call(
        root.path(),
        &[
            "analysis",
            "init",
            "model",
            "--directory",
            "analysis",
            "--inputs",
            "inputs.json",
        ],
        true,
    );
    server.call(root.path(), &["analysis", "submit", "analysis"], true);
    assert!(
        !server
            .fixture
            .lock()
            .unwrap()
            .requests
            .iter()
            .any(|r| r.starts_with("POST /api/v1/uploads "))
    );
    let output = Command::new(env!("CARGO_BIN_EXE_enumeray"))
        .current_dir(root.path())
        .args([
            "--api",
            &server.origin,
            "--config",
            "profiles.json",
            "analysis",
            "status",
            "analysis",
        ])
        .env("ENUMERAY_TOKEN", "renewed-token")
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.contains("Research compute")
            && text.contains("administrators can read")
            && text.contains(RUN)
    );
}

#[test]
fn terminal_failures_collect_evidence_and_wait_preserves_outcome_exit_codes() {
    for trial in [false, true] {
        for (state, exit) in [("failed", 3), ("cancelled", 4)] {
            let server = Server::new(false);
            let root = tempfile::tempdir().unwrap();
            if trial {
                server.init_candidate(root.path(), false);
            } else {
                server.init(root.path(), false);
            }
            server.call(root.path(), &["analysis", "submit", "analysis"], true);
            server.fixture.lock().unwrap().outcome = state.into();
            let output = server.call(
                root.path(),
                &["analysis", "wait", "analysis", "--check-outcome"],
                false,
            );
            assert_eq!(output.status.code(), Some(exit));
            assert_eq!(
                serde_json::from_slice::<Value>(&output.stdout).unwrap()["state"],
                state
            );
            server.call(root.path(), &["analysis", "collect", "analysis"], true);
            assert_eq!(
                read(root.path(), "analysis/bundle.json")["scientific_state"],
                state
            );
        }
    }
}

#[test]
fn initialization_validates_options_without_overwriting_existing_paths() {
    let server = Server::new(false);
    let root = tempfile::tempdir().unwrap();
    for extra in [
        vec!["--file", "value=missing"],
        vec!["--media-type", "dataset=text/csv"],
        vec!["--file", "dataset=a", "--file", "dataset=b"],
        vec!["--example", "missing"],
    ] {
        let mut args = vec![
            "analysis",
            "init",
            "model",
            "--directory",
            "analysis",
            "--example",
            "example",
        ];
        args.extend(extra);
        server.call(root.path(), &args, false);
        assert!(!root.path().join("analysis").exists());
    }
    server.init(root.path(), false);
    let original = fs::read(root.path().join("analysis/analysis.json")).unwrap();
    server.call(
        root.path(),
        &[
            "analysis",
            "init",
            "model",
            "--directory",
            "analysis",
            "--example",
            "example",
        ],
        false,
    );
    assert_eq!(
        fs::read(root.path().join("analysis/analysis.json")).unwrap(),
        original
    );
}

#[test]
fn writer_lock_receipt_recovery_and_corrupt_state_fail_safely() {
    let server = Server::new(false);
    let root = tempfile::tempdir().unwrap();
    server.init(root.path(), false);
    let lock = fs::OpenOptions::new()
        .write(true)
        .open(root.path().join("analysis/analysis.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    server.call(root.path(), &["analysis", "submit", "analysis"], false);
    drop(lock);
    server.call(root.path(), &["analysis", "submit", "analysis"], true);
    let mut journal = read(root.path(), "analysis/analysis.json");
    journal["receipt"] = Value::Null;
    fs::write(
        root.path().join("analysis/analysis.json"),
        serde_json::to_vec(&journal).unwrap(),
    )
    .unwrap();
    server.call(root.path(), &["analysis", "submit", "analysis"], true);
    assert_eq!(
        server
            .fixture
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|r| r.starts_with("POST /api/v1/runs "))
            .count(),
        1
    );
    journal["schema_version"] = json!(999);
    fs::write(
        root.path().join("analysis/analysis.json"),
        serde_json::to_vec(&journal).unwrap(),
    )
    .unwrap();
    server.call(root.path(), &["analysis", "status", "analysis"], false);
}

#[cfg(unix)]
#[test]
fn private_permissions_and_symlinks_are_enforced() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let server = Server::new(false);
    let root = tempfile::tempdir().unwrap();
    server.init(root.path(), false);
    assert_eq!(
        fs::metadata(root.path().join("analysis"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(root.path().join("analysis/analysis.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    for name in [
        "inputs.json",
        "analysis.lock",
        "state/publication.json",
        "state",
    ] {
        let path = root.path().join("analysis").join(name);
        let retained = path.with_extension("retained");
        fs::rename(&path, &retained).unwrap();
        symlink(&retained, &path).unwrap();
        server.call(root.path(), &["analysis", "submit", "analysis"], false);
        fs::remove_file(&path).unwrap();
        fs::rename(&retained, &path).unwrap();
    }
    fs::set_permissions(
        root.path().join("analysis"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    server.call(root.path(), &["analysis", "status", "analysis"], false);
    assert!(server.fixture.lock().unwrap().accepted.is_none());
}

#[test]
fn another_origin_and_local_deadline_leave_the_same_operation_available() {
    let server = Server::new(false);
    let other = Server::new(false);
    let root = tempfile::tempdir().unwrap();
    server.init(root.path(), false);
    other.call(root.path(), &["analysis", "submit", "analysis"], false);
    assert!(other.fixture.lock().unwrap().accepted.is_none());
    server.call(root.path(), &["analysis", "submit", "analysis"], true);
    server.fixture.lock().unwrap().outcome = "running".into();
    let output = server.call(
        root.path(),
        &[
            "analysis",
            "wait",
            "analysis",
            "--check-outcome",
            "--deadline",
            "1",
            "--interval",
            "1",
        ],
        false,
    );
    assert_eq!(output.status.code(), Some(5));
    assert_eq!(
        read(root.path(), "analysis/state/receipt.json")["run_id"],
        RUN
    );
    server.fixture.lock().unwrap().outcome = "succeeded".into();
    server.call(root.path(), &["analysis", "wait", "analysis"], true);
    assert_eq!(
        server
            .fixture
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|request| request.starts_with("POST /api/v1/runs "))
            .count(),
        1
    );
}

#[test]
fn edited_raw_json_is_validated_before_any_mutation() {
    let server = Server::new(false);
    let root = tempfile::tempdir().unwrap();
    server.init(root.path(), false);
    fs::write(
        root.path().join("analysis/inputs.json"),
        br#"{"value":{"$serde_json::private::Number":"2"}}"#,
    )
    .unwrap();
    server.call(root.path(), &["analysis", "submit", "analysis"], false);
    assert!(server.fixture.lock().unwrap().accepted.is_none());
    assert!(
        !read(root.path(), "analysis/analysis.json")["submission_started"]
            .as_bool()
            .unwrap()
    );
}

#[test]
fn logs_and_cancellation_require_explicit_submission_reconciliation() {
    let server = Server::new(false);
    let root = tempfile::tempdir().unwrap();
    server.init(root.path(), false);
    for uncertain in [false, true] {
        if uncertain {
            server.fixture.lock().unwrap().drop_accept = true;
            server.call(root.path(), &["analysis", "submit", "analysis"], false);
        }
        let journal = fs::read(root.path().join("analysis/analysis.json")).unwrap();
        let before = server.fixture.lock().unwrap().requests.len();
        for command in ["logs", "cancel"] {
            let output = server.call(root.path(), &["analysis", command, "analysis"], false);
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("analysis submit"),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert_eq!(
            fs::read(root.path().join("analysis/analysis.json")).unwrap(),
            journal
        );
        assert!(
            !root
                .path()
                .join("analysis/state/cancellation.json")
                .exists()
        );
        assert!(
            server.fixture.lock().unwrap().requests[before..]
                .iter()
                .all(|request| request.starts_with("GET /api/v1/me "))
        );
    }
    assert_eq!(
        server
            .fixture
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|request| request.starts_with("POST /api/v1/runs "))
            .count(),
        1
    );
}

#[test]
fn receipt_snapshot_supports_observation_without_repair_or_writer_ownership() {
    let server = Server::new(false);
    let root = tempfile::tempdir().unwrap();
    server.init(root.path(), false);
    server.call(root.path(), &["analysis", "submit", "analysis"], true);
    let mut journal = read(root.path(), "analysis/analysis.json");
    journal["receipt"] = Value::Null;
    let journal = serde_json::to_vec(&journal).unwrap();
    fs::write(root.path().join("analysis/analysis.json"), &journal).unwrap();
    let lock = fs::OpenOptions::new()
        .write(true)
        .open(root.path().join("analysis/analysis.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    server.call(root.path(), &["analysis", "status", "analysis"], true);
    server.call(root.path(), &["analysis", "wait", "analysis"], true);
    server.call(
        root.path(),
        &[
            "analysis", "logs", "analysis", "--limit", "2", "--cursor", "c0",
        ],
        true,
    );
    server.call(root.path(), &["analysis", "submit", "analysis"], false);
    assert_eq!(
        fs::read(root.path().join("analysis/analysis.json")).unwrap(),
        journal
    );
    let requests = &server.fixture.lock().unwrap().requests;
    assert!(
        requests
            .iter()
            .any(|request| request.contains(&format!("/runs/{RUN}/logs?"))
                && request.contains("limit=2")
                && request.contains("cursor=c0"))
    );
}

#[test]
fn status_logs_and_cancellation_proceed_while_wait_is_in_flight() {
    let server = Server::new(false);
    let root = tempfile::tempdir().unwrap();
    server.init(root.path(), false);
    server.call(root.path(), &["analysis", "submit", "analysis"], true);
    server.fixture.lock().unwrap().outcome = "running".into();
    let journal = fs::read(root.path().join("analysis/analysis.json")).unwrap();
    let gate = server.stall(&format!("GET /api/v1/runs/{RUN} "));
    let waiter = server.start(
        root.path(),
        &[
            "analysis",
            "wait",
            "analysis",
            "--interval",
            "1",
            "--deadline",
            "5",
            "--check-outcome",
        ],
    );
    gate.wait();
    server.call(root.path(), &["analysis", "status", "analysis"], true);
    server.call(root.path(), &["analysis", "logs", "analysis"], true);
    server.call(root.path(), &["analysis", "cancel", "analysis"], true);
    let cancellation = read(root.path(), "analysis/state/cancellation.json");
    assert_eq!(cancellation["attempt"], "acknowledged");
    assert_eq!(cancellation["receipt"]["status"], "requested");
    server.fixture.lock().unwrap().outcome = "cancelled".into();
    let output = server.call(root.path(), &["analysis", "status", "analysis"], true);
    let status: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["cancellation"]["receipt"]["status"], "requested");
    assert_eq!(status["run"]["cancellation"]["status"], "confirmed");
    gate.release();
    let output = waiter.join().unwrap();
    assert_eq!(
        output.status.code(),
        Some(4),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["state"],
        "cancelled"
    );
    assert_eq!(
        fs::read(root.path().join("analysis/analysis.json")).unwrap(),
        journal
    );
}

#[test]
fn one_folder_follower_keeps_named_logs_and_allows_status_pages_and_cancellation() {
    let server = Server::new(false);
    let root = tempfile::tempdir().unwrap();
    server.init(root.path(), false);
    server.call(root.path(), &["analysis", "submit", "analysis"], true);
    {
        let mut fixture = server.fixture.lock().unwrap();
        fixture.outcome = "running".into();
        fixture.logs = vec![
            json!({"position":1,"assignment_id":UPLOAD,"source":"model.progress","epoch":1,"sequence":1,"offset":0,"byte_length":7,"text":"step 1\n","gap":null,"source_reset":false,"received_at":"2026-10-19T12:00:00Z"}),
        ];
    }
    let gate = server.stall(&format!("GET /api/v1/runs/{RUN}/logs/stream"));
    let follower = server.start(
        root.path(),
        &[
            "analysis",
            "logs",
            "analysis",
            "--follow",
            "--interval",
            "1",
            "--deadline",
            "5",
        ],
    );
    gate.wait();
    let second = server.call(
        root.path(),
        &[
            "analysis",
            "logs",
            "analysis",
            "--follow",
            "--deadline",
            "1",
        ],
        false,
    );
    assert!(String::from_utf8_lossy(&second.stderr).contains("one follower per directory"));
    server.call(root.path(), &["analysis", "status", "analysis"], true);
    let page = server.call(root.path(), &["analysis", "logs", "analysis"], true);
    assert_eq!(
        serde_json::from_slice::<Value>(&page.stdout).unwrap()["items"][0]["source"],
        "model.progress"
    );
    server.call(root.path(), &["analysis", "cancel", "analysis"], true);
    server.fixture.lock().unwrap().outcome = "cancelled".into();
    gate.release();
    let output = follower.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let logs = events
        .iter()
        .filter(|event| event["event"] == "log")
        .collect::<Vec<_>>();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0]["data"]["source"], "model.progress");
    assert_eq!(events.last().unwrap()["data"]["state"], "cancelled");
    let checkpoint = read(root.path(), "analysis/state/logs.json");
    assert_eq!(checkpoint["run_id"], RUN);
    assert_eq!(checkpoint["user_id"], USER);
    assert_eq!(checkpoint["last_position"], 1);
    let resumed = server.call(
        root.path(),
        &[
            "analysis",
            "logs",
            "analysis",
            "--follow",
            "--deadline",
            "5",
        ],
        true,
    );
    assert!(
        String::from_utf8_lossy(&resumed.stdout)
            .lines()
            .all(|line| serde_json::from_str::<Value>(line).unwrap()["event"] != "log")
    );
}

#[test]
fn stalled_collection_excludes_writers_but_allows_observation_and_cancellation() {
    let server = Server::new(true);
    let root = tempfile::tempdir().unwrap();
    server.init(root.path(), true);
    server.call(root.path(), &["analysis", "submit", "analysis"], true);
    // A previous cancellation intent can race with successful completion.
    server.fixture.lock().unwrap().outcome = "running".into();
    server.call(root.path(), &["analysis", "cancel", "analysis"], true);
    server.fixture.lock().unwrap().outcome = "succeeded".into();
    let gate = server.stall("GET /storage/output ");
    let collector = server.start(root.path(), &["analysis", "collect", "analysis"]);
    gate.wait();
    server.call(root.path(), &["analysis", "submit", "analysis"], false);
    server.call(root.path(), &["analysis", "collect", "analysis"], false);
    server.call(root.path(), &["analysis", "status", "analysis"], true);
    server.call(root.path(), &["analysis", "logs", "analysis"], true);
    server.call(root.path(), &["analysis", "cancel", "analysis"], true);
    assert_eq!(
        read(root.path(), "analysis/state/cancellation.json")["receipt"]["status"],
        "completed"
    );
    gate.release();
    let output = collector.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        read(root.path(), "analysis/bundle.json")["scientific_state"],
        "succeeded"
    );
}

#[test]
fn stalled_upload_allows_status_with_advancing_multipart_checkpoint() {
    let server = Server::new(true);
    let root = tempfile::tempdir().unwrap();
    server.init(root.path(), true);
    let gate = server.stall("PUT /storage/input ");
    let submitter = server.start(root.path(), &["analysis", "submit", "analysis"]);
    gate.wait();
    let saved = read(root.path(), "analysis/analysis.json");
    let upload = format!(
        "analysis/state/uploads/{}.json",
        saved["bindings"]["dataset"]["id"].as_str().unwrap()
    );
    let mut checkpoint = read(root.path(), &upload);
    checkpoint["multipart"] =
        json!({"part_size_bytes":8388608,"part_count":1,"confirmed":{"1":"part-checksum"}});
    fs::write(
        root.path().join(&upload),
        serde_json::to_vec(&checkpoint).unwrap(),
    )
    .unwrap();
    server.call(root.path(), &["analysis", "status", "analysis"], true);
    server.call(root.path(), &["analysis", "submit", "analysis"], false);
    server.call(root.path(), &["analysis", "collect", "analysis"], false);
    for command in ["logs", "cancel"] {
        let output = server.call(root.path(), &["analysis", command, "analysis"], false);
        assert!(String::from_utf8_lossy(&output.stderr).contains("analysis submit"));
    }
    assert_eq!(read(root.path(), "analysis/analysis.json"), saved);
    gate.release();
    let output = submitter.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        read(root.path(), "analysis/state/receipt.json")["run_id"],
        RUN
    );
}

#[test]
fn lost_cancellation_reply_preserves_intent_and_retries_only_the_accepted_run() {
    let server = Server::new(false);
    let root = tempfile::tempdir().unwrap();
    server.init(root.path(), false);
    server.call(root.path(), &["analysis", "submit", "analysis"], true);
    {
        let mut fixture = server.fixture.lock().unwrap();
        fixture.outcome = "queued".into();
        fixture.drop_cancellation = true;
    }
    let journal = fs::read(root.path().join("analysis/analysis.json")).unwrap();
    let output = server.call(root.path(), &["analysis", "cancel", "analysis"], false);
    assert!(String::from_utf8_lossy(&output.stderr).contains("analysis cancel"));
    let uncertain = read(root.path(), "analysis/state/cancellation.json");
    assert_eq!(uncertain["attempt"], "uncertain");
    assert_eq!(uncertain["run_id"], RUN);
    assert_eq!(uncertain["receipt"], Value::Null);
    let before = server.fixture.lock().unwrap().requests.len();
    let status = server.call(root.path(), &["analysis", "status", "analysis"], true);
    assert_eq!(
        serde_json::from_slice::<Value>(&status.stdout).unwrap()["cancellation"]["attempt"],
        "uncertain"
    );
    assert!(
        server.fixture.lock().unwrap().requests[before..]
            .iter()
            .all(|request| request.starts_with("GET "))
    );
    server.call(root.path(), &["analysis", "cancel", "analysis"], true);
    server.call(root.path(), &["analysis", "cancel", "analysis"], true);
    let acknowledged = read(root.path(), "analysis/state/cancellation.json");
    assert_eq!(acknowledged["attempt"], "acknowledged");
    assert_eq!(acknowledged["receipt"]["status"], "confirmed");
    assert_eq!(acknowledged["receipt"]["run_id"], RUN);
    assert_eq!(
        fs::read(root.path().join("analysis/analysis.json")).unwrap(),
        journal
    );
    let fixture = server.fixture.lock().unwrap();
    assert_eq!(
        fixture
            .requests
            .iter()
            .filter(|request| request.starts_with("POST /api/v1/runs "))
            .count(),
        1
    );
    assert_eq!(
        fixture
            .requests
            .iter()
            .filter(|request| request.starts_with(&format!("PUT /api/v1/runs/{RUN}/cancellation ")))
            .count(),
        3
    );
}

#[test]
fn in_flight_cancellation_persists_pending_intent_without_blocking_status_or_logs() {
    let server = Server::new(false);
    let root = tempfile::tempdir().unwrap();
    server.init(root.path(), false);
    server.call(root.path(), &["analysis", "submit", "analysis"], true);
    server.fixture.lock().unwrap().outcome = "running".into();
    let gate = server.stall(&format!("PUT /api/v1/runs/{RUN}/cancellation "));
    let cancellation = server.start(root.path(), &["analysis", "cancel", "analysis"]);
    gate.wait();
    let pending = read(root.path(), "analysis/state/cancellation.json");
    assert_eq!(pending["attempt"], "pending");
    assert_eq!(pending["receipt"], Value::Null);
    assert_eq!(pending["run_id"], RUN);
    let output = server.call(root.path(), &["analysis", "status", "analysis"], true);
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["cancellation"]["attempt"],
        "pending"
    );
    server.call(root.path(), &["analysis", "logs", "analysis"], true);
    let second = server.call(root.path(), &["analysis", "cancel", "analysis"], false);
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("cancellation is already in progress")
    );
    gate.release();
    let output = cancellation.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        read(root.path(), "analysis/state/cancellation.json")["attempt"],
        "acknowledged"
    );
}

#[test]
fn cancellation_preserves_denied_and_completion_races_without_claiming_confirmation() {
    for (outcome, denied, code) in [
        ("running", true, "insufficient_scope"),
        ("succeeded", false, "run_not_cancellable"),
    ] {
        let server = Server::new(false);
        let root = tempfile::tempdir().unwrap();
        server.init(root.path(), false);
        server.call(root.path(), &["analysis", "submit", "analysis"], true);
        {
            let mut fixture = server.fixture.lock().unwrap();
            fixture.outcome = outcome.into();
            fixture.deny_cancellation = denied;
        }
        server.call(root.path(), &["analysis", "cancel", "analysis"], false);
        let saved = read(root.path(), "analysis/state/cancellation.json");
        assert_eq!(saved["attempt"], "rejected");
        assert_eq!(saved["error_code"], code);
        assert_eq!(saved["receipt"], Value::Null);
        assert_eq!(server.fixture.lock().unwrap().outcome, outcome);
    }
}

#[cfg(unix)]
#[test]
fn cancellation_sidecars_are_private_identity_bound_and_reject_corruption_and_symlinks() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let server = Server::new(false);
    let other = Server::new(false);
    let root = tempfile::tempdir().unwrap();
    server.init(root.path(), false);
    server.call(root.path(), &["analysis", "submit", "analysis"], true);
    server.fixture.lock().unwrap().outcome = "running".into();
    server.call(root.path(), &["analysis", "cancel", "analysis"], true);
    let path = root.path().join("analysis/state/cancellation.json");
    let original = fs::read(&path).unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    other.call(root.path(), &["analysis", "cancel", "analysis"], false);
    server.fixture.lock().unwrap().user = OUTPUT.into();
    server.call(root.path(), &["analysis", "cancel", "analysis"], false);
    assert_eq!(fs::read(&path).unwrap(), original);
    server.fixture.lock().unwrap().user = USER.into();
    let before = server.fixture.lock().unwrap().requests.len();
    fs::write(&path, b"{").unwrap();
    server.call(root.path(), &["analysis", "cancel", "analysis"], false);
    fs::write(&path, &original).unwrap();
    let retained = path.with_extension("retained");
    fs::rename(&path, &retained).unwrap();
    symlink(&retained, &path).unwrap();
    server.call(root.path(), &["analysis", "cancel", "analysis"], false);
    assert_eq!(fs::read(&retained).unwrap(), original);
    assert!(
        server.fixture.lock().unwrap().requests[before..]
            .iter()
            .all(|request| request.starts_with("GET "))
    );
}
