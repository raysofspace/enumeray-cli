//! The test connector uses HTTP and a private local journal, never the database.

#![cfg(unix)]

#[path = "connector_support/mod.rs"]
mod support;
use support::{Scratch, command, run};

use std::{
    error::Error,
    fs::{self, File},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Output, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{Path, RawQuery, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
};
use serde_json::{Value, json};
use tokio::{
    io::AsyncWriteExt, net::TcpListener, process::Command, task::JoinHandle, time::timeout,
};
use uuid::Uuid;

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;
const TOKEN: &str = "enr_c1_test-token-must-never-appear-in-output";

impl Scratch {
    fn with_token(token: &str) -> Self {
        let scratch = Self::new();
        scratch.token(token);
        support::install_scalar(&scratch);
        scratch
    }

    fn claim(&self) -> Command {
        let mut command = command();
        command
            .arg("claim-once")
            .arg("--journal")
            .arg(self.journal())
            .arg("--token-file")
            .arg(self.0.join("token"));
        command
    }

    fn reconcile(&self, confirm_stopped: bool) -> Command {
        let mut command = command();
        command
            .arg("reconcile")
            .arg("--journal")
            .arg(self.journal())
            .arg("--token-file")
            .arg(self.0.join("token"));
        if confirm_stopped {
            command.arg("--confirm-prior-process-stopped");
        }
        command
    }

    fn execute(&self, confirm_stopped: bool) -> Command {
        let mut command = command();
        command
            .args(["execute-once", "--journal"])
            .arg(self.journal())
            .arg("--token-file")
            .arg(self.0.join("token"));
        if confirm_stopped {
            command.arg("--confirm-prior-process-stopped");
        }
        command
    }

    fn write(&self, data: &Value) {
        fs::write(
            self.journal().join("journal.json"),
            serde_json::to_vec(data).unwrap(),
        )
        .unwrap();
    }

    async fn initialize(&self, origin: &str) -> Value {
        let output = run(command()
            .arg("init")
            .arg("--journal")
            .arg(self.journal())
            .arg("--api-base-url")
            .arg(origin)
            .args(["--connector-id", "test-connector"]))
        .await;
        success(output)
    }
}

fn success(output: Output) -> Value {
    let value = support::successful_json(&output);
    assert!(!String::from_utf8_lossy(&output.stdout).contains(TOKEN));
    value
}

fn failure(output: Output, message: &str) {
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(message), "{stderr}");
    assert!(!stderr.contains(TOKEN));
    assert!(output.stdout.is_empty());
}

#[derive(Clone, Copy)]
enum Behavior {
    Prepared,
    Executable,
    Empty,
    RetryClaim,
    InterruptedClaim,
    CloseUnavailable,
    WrongOwner,
    SessionConflict,
}

#[derive(Clone, Copy, Default)]
enum ListBehavior {
    #[default]
    Normal,
    RepeatedIdentity,
    RepeatedCursor,
    MissingItems,
    MissingCursor,
    InvalidSummary,
}

#[derive(Clone, Copy, Default)]
enum DetailBehavior {
    #[default]
    Normal,
    MissingState,
    InvalidPhase,
    ConflictingReceipt,
}

struct Observation {
    configuration_generation: u64,
    configuration_race: bool,
    lost_configuration_acknowledgements: usize,
    configuration_acknowledgements: Vec<(u64, Value)>,
    behavior: Behavior,
    journal_path: PathBuf,
    calls: Vec<&'static str>,
    session_ids: Vec<Value>,
    claim_keys: Vec<String>,
    assignment: Option<Value>,
    recovery_requests: Vec<Value>,
    lost_recovery_responses: usize,
    released_sessions: Vec<String>,
    omit_outstanding: bool,
    inventory: Option<Vec<Value>>,
    list_behavior: ListBehavior,
    detail_behavior: DetailBehavior,
    events: Vec<(Uuid, Value, Value)>,
    logs: Vec<Value>,
    result: Option<Value>,
    delivery_completion: Option<Value>,
    lost_result_responses: usize,
    lost_event_responses: usize,
    backend_evidence: Option<Value>,
    resolution: Option<Value>,
    observation_freshness: &'static str,
    revoked: bool,
}

struct Api {
    origin: String,
    observations: Arc<Mutex<Observation>>,
    task: JoinHandle<()>,
}

impl Api {
    async fn new(scratch: &Scratch, behavior: Behavior) -> Self {
        let observations = Arc::new(Mutex::new(Observation {
            configuration_generation: 1,
            configuration_race: false,
            lost_configuration_acknowledgements: 0,
            configuration_acknowledgements: Vec::new(),
            behavior,
            journal_path: scratch.journal().join("journal.json"),
            calls: Vec::new(),
            session_ids: Vec::new(),
            claim_keys: Vec::new(),
            assignment: None,
            recovery_requests: Vec::new(),
            lost_recovery_responses: 0,
            released_sessions: Vec::new(),
            omit_outstanding: false,
            inventory: None,
            list_behavior: ListBehavior::Normal,
            detail_behavior: DetailBehavior::Normal,
            events: Vec::new(),
            logs: Vec::new(),
            result: None,
            delivery_completion: None,
            lost_result_responses: 0,
            lost_event_responses: 0,
            backend_evidence: None,
            resolution: None,
            observation_freshness: "unknown",
            revoked: false,
        }));
        let router = Router::new()
            .route("/connector/v1/configuration", get(configuration))
            .route(
                "/connector/v1/configuration/{generation}/ack",
                put(configuration_ack),
            )
            .route("/connector/v1/sessions", post(register))
            .route("/connector/v1/sessions/recover", post(recover))
            .route("/connector/v1/sessions/{session_id}", delete(close))
            .route("/connector/v1/heartbeat", put(heartbeat))
            .route("/connector/v1/assignments", post(claim).get(list))
            .route("/connector/v1/assignments/{assignment_id}", get(detail))
            .route(
                "/connector/v1/assignments/{assignment_id}/control",
                get(control),
            )
            .route(
                "/connector/v1/assignments/{assignment_id}/events/{event_id}",
                put(event),
            )
            .route(
                "/connector/v1/assignments/{assignment_id}/logs/{source}/{epoch}/{sequence}",
                put(log_batch),
            )
            .route(
                "/connector/v1/assignments/{assignment_id}/result",
                put(result),
            )
            .route(
                "/connector/v1/assignments/{assignment_id}/delivery",
                put(delivery),
            )
            .with_state(observations.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            origin,
            observations,
            task,
        }
    }
}

async fn configuration(
    State(state): State<Arc<Mutex<Observation>>>,
    headers: HeaderMap,
) -> Json<Value> {
    check_authorization(&headers);
    let mut state = state.lock().unwrap();
    let mut desired =
        support::desired_configuration(state.journal_path.parent().unwrap().parent().unwrap());
    desired["generation"] = json!(state.configuration_generation);
    if state.configuration_race {
        state.configuration_generation += 1;
        state.configuration_race = false;
    }
    Json(desired)
}
async fn configuration_ack(
    State(state): State<Arc<Mutex<Observation>>>,
    Path(generation): Path<u64>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    check_authorization(&headers);
    assert!(body["validations"].is_array());
    let mut state = state.lock().unwrap();
    assert!(
        state
            .journal_path
            .parent()
            .unwrap()
            .join("compiled-installations.json")
            .exists()
    );
    state
        .configuration_acknowledgements
        .push((generation, body));
    if generation != state.configuration_generation {
        return StatusCode::CONFLICT.into_response();
    }
    if state.lost_configuration_acknowledgements > 0 {
        state.lost_configuration_acknowledgements -= 1;
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    Json(json!({"generation":generation,"acknowledged":true})).into_response()
}

#[tokio::test]
async fn stale_configuration_acknowledgement_never_claims_and_retries_current_generation()
-> TestResult {
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::Empty).await;
    scratch.initialize(&api.origin).await;
    api.observations.lock().unwrap().configuration_race = true;
    failure(
        run(&mut scratch.claim()).await,
        "configuration acknowledgment returned HTTP 409",
    );
    assert!(api.observations.lock().unwrap().claim_keys.is_empty());
    success(run(&mut scratch.reconcile(true)).await);
    assert_eq!(success(run(&mut scratch.claim()).await)["status"], "empty");
    let state = api.observations.lock().unwrap();
    assert_eq!(
        state
            .configuration_acknowledgements
            .iter()
            .map(|(generation, _)| *generation)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(state.claim_keys.len(), 1);
    Ok(())
}

#[tokio::test]
async fn lost_configuration_acknowledgement_replays_exact_report_after_persistence() -> TestResult {
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::Empty).await;
    scratch.initialize(&api.origin).await;
    api.observations
        .lock()
        .unwrap()
        .lost_configuration_acknowledgements = 1;
    assert_eq!(success(run(&mut scratch.claim()).await)["status"], "empty");
    let state = api.observations.lock().unwrap();
    assert_eq!(state.configuration_acknowledgements.len(), 2);
    assert_eq!(
        state.configuration_acknowledgements[0],
        state.configuration_acknowledgements[1]
    );
    assert_eq!(
        state.configuration_acknowledgements[0].1["validations"][0]["ready"],
        true
    );
    assert_eq!(state.claim_keys.len(), 1);
    Ok(())
}

async fn control(State(state): State<Arc<Mutex<Observation>>>, headers: HeaderMap) -> Json<Value> {
    check_authorization(&headers);
    let revoked = state.lock().unwrap().revoked;
    Json(json!({"allow_launch":!revoked,"cancel_pending":revoked,
        "cancel_running":false,"reason":revoked.then_some("execution_authorization_revoked")}))
}

impl Drop for Api {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn check_authorization(headers: &HeaderMap) {
    assert_eq!(headers["authorization"], format!("Bearer {TOKEN}"));
}

async fn register(
    State(state): State<Arc<Mutex<Observation>>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    check_authorization(&headers);
    let mut state = state.lock().unwrap();
    state.calls.push("session");
    if state
        .released_sessions
        .contains(&body["session_id"].as_str().unwrap().to_owned())
    {
        return (
            StatusCode::CONFLICT,
            Json(json!({"code": "session_closed"})),
        )
            .into_response();
    }
    if matches!(state.behavior, Behavior::SessionConflict) {
        return (StatusCode::CONFLICT, Json(json!({"secret_echo": TOKEN}))).into_response();
    }
    let journal: Value = serde_json::from_slice(&fs::read(&state.journal_path).unwrap()).unwrap();
    // Ownership is durable before the server receives the registration request.
    assert_eq!(body["session_id"], journal["active_session_id"]);
    assert_eq!(body["journal_id"], journal["journal_id"]);
    assert_eq!(body.as_object().unwrap().len(), 2);
    state.session_ids.push(body["session_id"].clone());
    Json(json!({"connector_id": "test-connector", "journal_id": body["journal_id"], "session_id": body["session_id"]})).into_response()
}

async fn recover(
    State(state): State<Arc<Mutex<Observation>>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    check_authorization(&headers);
    let mut state = state.lock().unwrap();
    state.calls.push("recover");
    let journal: Value = serde_json::from_slice(&fs::read(&state.journal_path).unwrap()).unwrap();
    assert_eq!(body["prior_process_stopped"], true);
    assert_eq!(body["journal_id"], journal["journal_id"]);
    assert_eq!(body["previous_session_id"], journal["active_session_id"]);
    assert_eq!(
        body["session_id"],
        journal["pending_recovery"]["session_id"]
    );
    state.recovery_requests.push(body.clone());
    if state.lost_recovery_responses > 0 {
        state.lost_recovery_responses -= 1;
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    Json(json!({"connector_id": "test-connector", "journal_id": body["journal_id"], "session_id": body["session_id"]})).into_response()
}

async fn list(
    State(state): State<Arc<Mutex<Observation>>>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Json<Value> {
    check_authorization(&headers);
    let mut state = state.lock().unwrap();
    state.calls.push("list");
    let query = query.unwrap();
    let parameters: Vec<_> = url::form_urlencoded::parse(query.as_bytes()).collect();
    assert!(
        parameters
            .iter()
            .any(|(key, value)| key == "limit" && value == "1")
    );
    let page: usize = parameters
        .iter()
        .find(|(key, _)| key == "cursor")
        .map(|(_, value)| value.strip_prefix("page-").unwrap().parse().unwrap())
        .unwrap_or(0);
    let inventory = state
        .inventory
        .clone()
        .unwrap_or_else(|| state.assignment.iter().cloned().collect());
    let index = if matches!(state.list_behavior, ListBehavior::RepeatedIdentity) {
        0
    } else {
        page
    };
    let items: Vec<Value> = inventory
        .get(index)
        .filter(|_| !state.omit_outstanding && state.result.is_none())
        .map(|assignment| {
            json!({
                "assignment_id": assignment["assignment_id"],
                "run_id": assignment["run_id"], "attempt_id": assignment["attempt_id"],
                "journal_id": assignment["journal_id"], "state": "running", "phase": "preparing",
                "created_at": assignment["created_at"]
            })
        })
        .into_iter()
        .collect();
    let cursor = if matches!(state.list_behavior, ListBehavior::RepeatedCursor) {
        Some("page-1".to_owned())
    } else if !state.omit_outstanding && page + 1 < inventory.len() {
        Some(format!("page-{}", page + 1))
    } else {
        None
    };
    Json(match state.list_behavior {
        ListBehavior::MissingItems => json!({"next_cursor": null}),
        ListBehavior::MissingCursor => json!({"items": items}),
        ListBehavior::InvalidSummary => {
            json!({"items": [{"assignment_id": inventory[0]["assignment_id"]}], "next_cursor": null})
        }
        _ => json!({"items": items, "next_cursor": cursor}),
    })
}

async fn detail(
    State(state): State<Arc<Mutex<Observation>>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Response {
    check_authorization(&headers);
    let mut state = state.lock().unwrap();
    state.calls.push("detail");
    let assignment = state
        .inventory
        .as_ref()
        .and_then(|inventory| {
            inventory
                .iter()
                .find(|assignment| assignment["assignment_id"] == json!(id))
        })
        .or_else(|| {
            state
                .assignment
                .as_ref()
                .filter(|assignment| assignment["assignment_id"] == json!(id))
        });
    match assignment {
        Some(assignment) => {
            let mut detail = json!({
                "assignment": assignment, "state": "running", "phase": "preparing",
                "last_event_sequence": null, "backend_handle": null,
                "last_observed_at": null, "observation_freshness": state.observation_freshness,
                "resolution": state.resolution,"delivery_completion":state.delivery_completion
            });
            detail["backend_handle"] = json!(state.backend_evidence);
            if let Some(result) = &state.result {
                detail["state"] = result["receipt"]["outcome"].clone();
                detail["phase"] = Value::Null;
                detail["result"] = result.clone();
            }
            match state.detail_behavior {
                DetailBehavior::Normal => (),
                DetailBehavior::MissingState => {
                    detail.as_object_mut().unwrap().remove("state");
                }
                DetailBehavior::InvalidPhase => detail["phase"] = json!("unexpected_phase"),
                DetailBehavior::ConflictingReceipt => {
                    detail["assignment"]["inputs"] = json!({"changed_input": 42})
                }
            }
            Json(detail).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn delivery(
    State(state): State<Arc<Mutex<Observation>>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    check_authorization(&headers);
    let mut state = state.lock().unwrap();
    assert!(state.result.is_some());
    assert_eq!(body["archive_expected"], false);
    let assignment = state.assignment.as_ref().unwrap().clone();
    let receipt = state.delivery_completion.get_or_insert_with(|| json!({"assignment_id":id,"run_id":assignment["run_id"],"attempt_id":assignment["attempt_id"],"source":"connector","completed_at":"2026-10-15T12:00:00Z"})).clone();
    Json(receipt)
}

async fn heartbeat(
    State(state): State<Arc<Mutex<Observation>>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> StatusCode {
    check_authorization(&headers);
    assert_eq!(
        body,
        json!({"connector_version": format!("enumeray/{}", env!("CARGO_PKG_VERSION")), "capabilities": ["json_inputs", "json_outputs", "artifact_inputs", "artifact_outputs", "large_artifacts", "collection_inputs", "collection_outputs"], "wrapper_protocol_versions": [1]})
    );
    state.lock().unwrap().calls.push("heartbeat");
    StatusCode::OK
}

async fn claim(
    State(state): State<Arc<Mutex<Observation>>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    check_authorization(&headers);
    assert_eq!(body, json!({}));
    let mut state = state.lock().unwrap();
    state.calls.push("claim");
    let journal: Value = serde_json::from_slice(&fs::read(&state.journal_path).unwrap()).unwrap();
    // The idempotency key and session are durable before any delegation happens.
    assert_eq!(
        headers["x-connector-session"].to_str().unwrap(),
        journal["active_session_id"].as_str().unwrap()
    );
    assert_eq!(
        headers["idempotency-key"].to_str().unwrap(),
        journal["pending_claim_key"].as_str().unwrap()
    );
    state
        .claim_keys
        .push(headers["idempotency-key"].to_str().unwrap().to_owned());
    if matches!(state.behavior, Behavior::Empty) {
        return StatusCode::NO_CONTENT.into_response();
    }
    if state.assignment.is_none() {
        let desired =
            support::desired_configuration(state.journal_path.parent().unwrap().parent().unwrap());
        let binding = desired["publications"][0]["descriptor"].clone();
        let deployment: Value =
            serde_json::from_str(include_str!("../../../examples/test-requirements.json")).unwrap();
        let model: Value = serde_json::to_value(
            enumeray_contracts::models::parse_publication(include_bytes!(
                "../../../examples/scalar-double.json"
            ))
            .unwrap(),
        )
        .unwrap();
        state.assignment = Some(json!({
            "assignment_id": Uuid::new_v4(), "run_id": Uuid::new_v4(), "attempt_id": Uuid::new_v4(),
            "connector_id": if matches!(state.behavior, Behavior::WrongOwner) { "unrelated-connector" } else { "test-connector" },
            "journal_id": journal["journal_id"], "model_id": "scalar-double", "publication_id": "r1",
            "contract_version": 1,  "publication_id":"test-publication","context_id":"test-context",
            "descriptor": binding, "requirements": deployment, "definition": model["definition"],
            "inputs": {"private_input": "must-stay-in-journal"}, "created_at": "2026-09-21T12:00:00Z"
        }));
        if matches!(state.behavior, Behavior::Executable) {
            state.assignment.as_mut().unwrap()["inputs"] = json!({"value": 7});
        }
    }
    if matches!(state.behavior, Behavior::RetryClaim) && state.claim_keys.len() == 1 {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"secret_echo": TOKEN})),
        )
            .into_response();
    }
    if matches!(state.behavior, Behavior::InterruptedClaim) {
        let body = axum::body::Body::from_stream(futures_util::stream::pending::<
            Result<axum::body::Bytes, std::convert::Infallible>,
        >());
        return (StatusCode::CREATED, body).into_response();
    }
    (StatusCode::CREATED, Json(state.assignment.clone().unwrap())).into_response()
}

async fn event(
    State(state): State<Arc<Mutex<Observation>>>,
    Path((assignment_id, event_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    check_authorization(&headers);
    let mut state = state.lock().unwrap();
    state.calls.push("event");
    let journal: Value = serde_json::from_slice(&fs::read(&state.journal_path).unwrap()).unwrap();
    let persisted = journal["prepared_assignments"][0]["execution"]["events"]
        .as_array()
        .unwrap();
    assert!(
        persisted
            .iter()
            .any(|event| event["id"] == json!(event_id) && event["request"] == body)
    );
    assert_eq!(
        headers["x-connector-session"].to_str().unwrap(),
        journal["active_session_id"].as_str().unwrap()
    );
    assert_eq!(
        journal["prepared_assignments"][0]["submission_state"],
        "submitted"
    );
    let receipt = if let Some((_, original, receipt)) =
        state.events.iter().find(|(id, _, _)| *id == event_id)
    {
        assert_eq!(*original, body);
        receipt.clone()
    } else {
        let receipt = json!({"assignment_id": assignment_id, "event_id": event_id, "sequence": body["sequence"],
            "disposition":"applied", "phase":body["phase"], "received_at":"2026-09-21T12:00:00Z", "observation_confirmed":true});
        state.events.push((event_id, body, receipt.clone()));
        receipt
    };
    if state.lost_event_responses > 0 {
        state.lost_event_responses -= 1;
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    Json(receipt).into_response()
}

async fn log_batch(
    State(state): State<Arc<Mutex<Observation>>>,
    Path((assignment_id, source, epoch, sequence)): Path<(Uuid, String, i64, i64)>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    check_authorization(&headers);
    let mut state = state.lock().unwrap();
    let journal: Value = serde_json::from_slice(&fs::read(&state.journal_path).unwrap()).unwrap();
    let pending = &journal["prepared_assignments"][0]["logs"]["streams"][&source]["pending"];
    assert_eq!(pending["request"], body);
    assert_eq!(pending["epoch"], epoch);
    assert_eq!(pending["sequence"], sequence);
    let receipt = json!({"assignment_id":assignment_id,"source":source,"epoch":epoch,"sequence":sequence,
        "position":state.logs.len()+1,"received_at":"2026-09-21T12:00:00Z"});
    state.logs.push(json!({"source":source,"request":body}));
    Json(receipt)
}

async fn result(
    State(state): State<Arc<Mutex<Observation>>>,
    Path(assignment_id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    check_authorization(&headers);
    let mut state = state.lock().unwrap();
    state.calls.push("result");
    let journal: Value = serde_json::from_slice(&fs::read(&state.journal_path).unwrap()).unwrap();
    assert_eq!(
        body,
        journal["prepared_assignments"][0]["execution"]["report"]
    );
    assert_eq!(body["execution_terminated"], true);
    assert_eq!(
        state.events.len(),
        if journal["prepared_assignments"][0]["execution"]["startup_evidence"].is_object() {
            1
        } else {
            2
        }
    );
    let assignment = state.assignment.as_ref().unwrap();
    let receipt = json!({"assignment_id":assignment_id, "run_id":assignment["run_id"], "attempt_id":body["attempt_id"],
        "outcome":body["outcome"], "completed_at":"2026-09-21T12:00:00Z"});
    let result = json!({"receipt":receipt, "outputs":body["outputs"], "files":body["files"],
        "optional_output_failures":body["optional_output_failures"], "failure":body["failure"]});
    if let Some(prior) = &state.result {
        assert_eq!(*prior, result);
    }
    state.result = Some(result);
    if state.lost_result_responses > 0 {
        state.lost_result_responses -= 1;
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    Json(receipt).into_response()
}

async fn close(State(state): State<Arc<Mutex<Observation>>>, headers: HeaderMap) -> StatusCode {
    check_authorization(&headers);
    let mut state = state.lock().unwrap();
    state.calls.push("close");
    let journal: Value = serde_json::from_slice(&fs::read(&state.journal_path).unwrap()).unwrap();
    assert!(journal["pending_claim_key"].is_null());
    assert!(journal["active_session_id"].is_string());
    if let Some(assignment) = &state.assignment {
        // There is a persisted intent before the connector releases process ownership.
        assert_eq!(
            journal["prepared_assignments"][0]["assignment"],
            *assignment
        );
        if journal["prepared_assignments"][0]["submission_state"] == "definitely_unsubmitted" {
            assert!(journal["prepared_assignments"][0]["launch_intent_id"].is_string());
        }
    }
    state
        .released_sessions
        .push(journal["active_session_id"].as_str().unwrap().to_owned());
    if matches!(state.behavior, Behavior::CloseUnavailable) {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::NO_CONTENT
    }
}

#[tokio::test]
async fn initialization_is_private_validates_origins_and_never_overwrites_a_journal() -> TestResult
{
    let scratch = Scratch::with_token(TOKEN);
    let initialized = scratch.initialize("http://127.0.0.1:8080").await;
    let before = scratch.read();
    assert_eq!(initialized["journal_id"], before["journal_id"]);
    assert_eq!(
        fs::metadata(scratch.journal())?.permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(scratch.journal().join("journal.json"))?
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let output = run(command()
        .arg("init")
        .arg("--journal")
        .arg(scratch.journal())
        .args([
            "--api-base-url",
            "http://127.0.0.1:8080",
            "--connector-id",
            "another-connector",
        ]))
    .await;
    failure(output, "journal already exists");
    assert_eq!(scratch.read(), before);
    for origin in [
        "http://192.0.2.1",
        "http://127.0.0.1/path",
        "http://secret@127.0.0.1",
        "http://127.0.0.1?secret",
    ] {
        let output = run(command()
            .arg("init")
            .arg("--journal")
            .arg(scratch.0.join("invalid"))
            .args(["--api-base-url", origin, "--connector-id", "test-connector"]))
        .await;
        failure(output, "HTTP loopback origin");
    }
    Ok(())
}

#[tokio::test]
async fn claimed_assignment_and_launch_intent_are_durable_without_executing_science() -> TestResult
{
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::Prepared).await;
    scratch.initialize(&api.origin).await;
    let receipt = success(run(&mut scratch.claim()).await);
    assert_eq!(receipt["status"], "prepared");
    assert!(!receipt.to_string().contains("must-stay-in-journal"));
    let journal = scratch.read();
    assert_eq!(journal["prepared_assignments"].as_array().unwrap().len(), 1);
    assert_eq!(
        journal["prepared_assignments"][0]["launch_intent_id"],
        receipt["launch_intent_id"]
    );
    assert_eq!(
        journal["prepared_assignments"][0]["assignment"]["inputs"]["private_input"],
        "must-stay-in-journal"
    );
    assert!(journal["active_session_id"].is_null());
    assert!(journal["pending_claim_key"].is_null());
    assert_eq!(
        api.observations.lock().unwrap().calls,
        ["session", "list", "heartbeat", "claim", "close"]
    );
    let inspected = success(
        run(command()
            .arg("inspect")
            .arg("--journal")
            .arg(scratch.journal()))
        .await,
    );
    assert!(!inspected.to_string().contains("must-stay-in-journal"));
    assert_eq!(
        inspected["prepared_assignments"][0]["assignment_id"],
        receipt["assignment_id"]
    );
    Ok(())
}

#[tokio::test]
async fn uncertain_claim_response_reuses_the_persisted_key_and_one_launch_intent() -> TestResult {
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::RetryClaim).await;
    scratch.initialize(&api.origin).await;
    success(run(&mut scratch.claim()).await);
    let state = api.observations.lock().unwrap();
    assert_eq!(state.claim_keys.len(), 2);
    assert_eq!(state.claim_keys[0], state.claim_keys[1]);
    assert_eq!(
        scratch.read()["prepared_assignments"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn empty_polls_close_sessions_and_the_next_invocation_uses_fresh_identities() -> TestResult {
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::Empty).await;
    scratch.initialize(&api.origin).await;
    for _ in 0..2 {
        assert_eq!(success(run(&mut scratch.claim()).await)["status"], "empty");
        assert!(scratch.read()["active_session_id"].is_null());
        assert!(scratch.read()["pending_claim_key"].is_null());
    }
    let state = api.observations.lock().unwrap();
    assert_ne!(state.claim_keys[0], state.claim_keys[1]);
    assert_ne!(state.session_ids[0], state.session_ids[1]);
    Ok(())
}

#[tokio::test]
async fn a_second_local_process_cannot_open_the_same_journal() -> TestResult {
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::Prepared).await;
    scratch.initialize(&api.origin).await;
    let lock = File::open(scratch.journal().join("owner.lock"))?;
    lock.try_lock()?;
    failure(
        run(&mut scratch.claim()).await,
        "already owned by another process",
    );
    assert!(api.observations.lock().unwrap().calls.is_empty());
    Ok(())
}

#[tokio::test]
async fn uncertain_session_closure_preserves_intent_and_blocks_a_new_process() -> TestResult {
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::CloseUnavailable).await;
    scratch.initialize(&api.origin).await;
    failure(
        run(&mut scratch.claim()).await,
        "session closure returned HTTP 503",
    );
    let before = scratch.read();
    assert!(before["active_session_id"].is_string());
    assert_eq!(before["prepared_assignments"].as_array().unwrap().len(), 1);
    let call_count = api.observations.lock().unwrap().calls.len();
    failure(
        run(&mut scratch.claim()).await,
        "unresolved process ownership",
    );
    assert_eq!(scratch.read(), before);
    assert_eq!(api.observations.lock().unwrap().calls.len(), call_count);
    Ok(())
}

#[tokio::test]
async fn a_crashed_process_cannot_reclaim_an_assignment_with_an_uncertain_response() -> TestResult {
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::InterruptedClaim).await;
    scratch.initialize(&api.origin).await;
    let mut process = scratch.claim().spawn()?;
    timeout(Duration::from_secs(5), async {
        loop {
            if api.observations.lock().unwrap().assignment.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    process.kill().await?;
    let before = scratch.read();
    assert!(before["active_session_id"].is_string());
    assert!(before["pending_claim_key"].is_string());
    assert!(
        before["prepared_assignments"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let calls = api.observations.lock().unwrap().calls.len();
    failure(
        run(&mut scratch.claim()).await,
        "unresolved process ownership",
    );
    assert_eq!(scratch.read(), before);
    assert_eq!(api.observations.lock().unwrap().calls.len(), calls);
    failure(
        run(&mut scratch.reconcile(false)).await,
        "--confirm-prior-process-stopped",
    );
    assert_eq!(scratch.read(), before);
    let recovered = success(run(&mut scratch.reconcile(true)).await);
    assert_eq!(recovered["status"], "reconciliation_required");
    let after = scratch.read();
    assert!(after["active_session_id"].is_null());
    assert!(after["pending_claim_key"].is_null());
    assert!(after["prepared_assignments"][0]["launch_intent_id"].is_null());
    assert_eq!(
        after["prepared_assignments"][0]["submission_state"],
        "unknown"
    );
    {
        let state = api.observations.lock().unwrap();
        assert_eq!(
            after["prepared_assignments"][0]["assignment"],
            *state.assignment.as_ref().unwrap()
        );
        assert_eq!(
            state.claim_keys.len(),
            1,
            "recovery must not replay an empty-capable claim"
        );
    }
    failure(
        run(&mut scratch.claim()).await,
        "unknown submission outcomes",
    );
    Ok(())
}

#[tokio::test]
async fn clean_restart_retrieves_pinned_work_and_keeps_the_original_launch_identity() -> TestResult
{
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::Prepared).await;
    scratch.initialize(&api.origin).await;
    success(run(&mut scratch.claim()).await);
    let before = scratch.read();
    for _ in 0..2 {
        let output = success(run(&mut scratch.reconcile(false)).await);
        assert_eq!(output["status"], "reconciled");
        assert!(!output.to_string().contains("must-stay-in-journal"));
        assert_eq!(
            scratch.read()["prepared_assignments"],
            before["prepared_assignments"]
        );
    }
    let state = api.observations.lock().unwrap();
    assert_eq!(state.claim_keys.len(), 1);
    assert_eq!(
        state.calls.iter().filter(|call| **call == "detail").count(),
        2
    );
    assert!(state.recovery_requests.is_empty());
    Ok(())
}

fn additional_assignment(original: &Value) -> Value {
    let mut assignment = original.clone();
    assignment["assignment_id"] = json!(Uuid::new_v4());
    assignment["run_id"] = json!(Uuid::new_v4());
    assignment["attempt_id"] = json!(Uuid::new_v4());
    assignment
}

#[tokio::test]
async fn reconciliation_follows_every_page_without_inventing_launch_identities() -> TestResult {
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::Prepared).await;
    scratch.initialize(&api.origin).await;
    success(run(&mut scratch.claim()).await);
    let original = scratch.read()["prepared_assignments"][0].clone();
    let inventory = vec![
        original["assignment"].clone(),
        additional_assignment(&original["assignment"]),
        additional_assignment(&original["assignment"]),
    ];
    api.observations.lock().unwrap().inventory = Some(inventory.clone());

    let output = success(run(&mut scratch.reconcile(false)).await);
    assert_eq!(output["status"], "reconciliation_required");
    let journal = scratch.read();
    let entries = journal["prepared_assignments"].as_array().unwrap();
    assert_eq!(entries.len(), inventory.len());
    assert_eq!(entries[0], original);
    for (entry, assignment) in entries.iter().zip(&inventory).skip(1) {
        assert_eq!(entry["assignment"], *assignment);
        assert_eq!(entry["submission_state"], "unknown");
        assert!(entry["launch_intent_id"].is_null());
    }
    assert!(journal["active_session_id"].is_null());
    assert!(journal["pending_claim_key"].is_null());
    let state = api.observations.lock().unwrap();
    assert_eq!(state.claim_keys.len(), 1);
    assert_eq!(
        state.calls.iter().filter(|call| **call == "list").count(),
        4
    );
    assert_eq!(
        state.calls.iter().filter(|call| **call == "detail").count(),
        3
    );
    Ok(())
}

#[tokio::test]
async fn invalid_recovery_responses_preserve_ownership_and_pinned_history() -> TestResult {
    let cases = [
        (
            ListBehavior::RepeatedIdentity,
            DetailBehavior::Normal,
            "assignment inventory repeats",
        ),
        (
            ListBehavior::RepeatedCursor,
            DetailBehavior::Normal,
            "invalid or excessive assignment pagination",
        ),
        (
            ListBehavior::MissingItems,
            DetailBehavior::Normal,
            "invalid assignment inventory",
        ),
        (
            ListBehavior::MissingCursor,
            DetailBehavior::Normal,
            "invalid or excessive assignment pagination",
        ),
        (
            ListBehavior::InvalidSummary,
            DetailBehavior::Normal,
            "invalid assignment inventory",
        ),
        (
            ListBehavior::Normal,
            DetailBehavior::MissingState,
            "invalid assignment detail",
        ),
        (
            ListBehavior::Normal,
            DetailBehavior::InvalidPhase,
            "invalid assignment detail",
        ),
        (
            ListBehavior::Normal,
            DetailBehavior::ConflictingReceipt,
            "assignment conflicts with the durable local record",
        ),
    ];
    for (list_behavior, detail_behavior, expected_error) in cases {
        let scratch = Scratch::with_token(TOKEN);
        let api = Api::new(&scratch, Behavior::Prepared).await;
        scratch.initialize(&api.origin).await;
        success(run(&mut scratch.claim()).await);
        let original = scratch.read()["prepared_assignments"][0].clone();
        {
            let mut state = api.observations.lock().unwrap();
            state.inventory = Some(vec![
                original["assignment"].clone(),
                additional_assignment(&original["assignment"]),
            ]);
            state.list_behavior = list_behavior;
            state.detail_behavior = detail_behavior;
        }
        failure(run(&mut scratch.reconcile(false)).await, expected_error);
        let journal = scratch.read();
        assert!(journal["active_session_id"].is_string());
        assert_eq!(journal["prepared_assignments"][0], original);
        let state = api.observations.lock().unwrap();
        assert_eq!(state.claim_keys.len(), 1, "recovery must never claim work");
        assert_eq!(
            state.calls.iter().filter(|call| **call == "close").count(),
            1,
            "incomplete recovery must retain the unresolved process session"
        );
    }
    Ok(())
}

#[tokio::test]
async fn recovery_beyond_the_local_assignment_bound_retains_a_readable_journal() -> TestResult {
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::Prepared).await;
    scratch.initialize(&api.origin).await;
    success(run(&mut scratch.claim()).await);
    let original = scratch.read()["prepared_assignments"][0].clone();
    let mut inventory = vec![original["assignment"].clone()];
    inventory.extend((0..64).map(|_| additional_assignment(&original["assignment"])));
    api.observations.lock().unwrap().inventory = Some(inventory);

    failure(run(&mut scratch.reconcile(false)).await, "64-assignment");
    let journal = scratch.read();
    assert_eq!(
        journal["prepared_assignments"].as_array().unwrap().len(),
        64
    );
    assert_eq!(journal["prepared_assignments"][0], original);
    assert!(journal["active_session_id"].is_string());
    let inspected = success(
        run(command()
            .arg("inspect")
            .arg("--journal")
            .arg(scratch.journal()))
        .await,
    );
    assert_eq!(
        inspected["prepared_assignments"].as_array().unwrap().len(),
        64
    );
    let state = api.observations.lock().unwrap();
    assert_eq!(state.claim_keys.len(), 1);
    assert_eq!(
        state.calls.iter().filter(|call| **call == "close").count(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn lost_release_acknowledgement_recovers_closed_predecessor_without_new_attempt() -> TestResult
{
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::CloseUnavailable).await;
    scratch.initialize(&api.origin).await;
    failure(
        run(&mut scratch.claim()).await,
        "session closure returned HTTP 503",
    );
    let before = scratch.read();
    api.observations.lock().unwrap().behavior = Behavior::Prepared;
    let output = success(run(&mut scratch.reconcile(true)).await);
    assert_eq!(output["status"], "reconciled");
    assert_eq!(
        scratch.read()["prepared_assignments"],
        before["prepared_assignments"]
    );
    assert!(scratch.read()["active_session_id"].is_null());
    assert_eq!(api.observations.lock().unwrap().claim_keys.len(), 1);
    Ok(())
}

#[tokio::test]
async fn lost_recovery_acknowledgements_retry_the_same_persisted_transition() -> TestResult {
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::CloseUnavailable).await;
    scratch.initialize(&api.origin).await;
    failure(
        run(&mut scratch.claim()).await,
        "session closure returned HTTP 503",
    );
    {
        let mut state = api.observations.lock().unwrap();
        state.behavior = Behavior::Prepared;
        state.lost_recovery_responses = 3;
    }
    failure(
        run(&mut scratch.reconcile(true)).await,
        "session recovery returned HTTP 503",
    );
    let pending = scratch.read()["pending_recovery"].clone();
    assert!(pending["session_id"].is_string());
    let output = success(run(&mut scratch.reconcile(true)).await);
    assert_eq!(output["status"], "reconciled");
    assert!(scratch.read()["pending_recovery"].is_null());
    let state = api.observations.lock().unwrap();
    assert_eq!(state.recovery_requests.len(), 4);
    for request in &state.recovery_requests {
        assert_eq!(request, &state.recovery_requests[0]);
        assert_eq!(request["session_id"], pending["session_id"]);
    }
    assert_eq!(state.claim_keys.len(), 1);
    Ok(())
}

#[tokio::test]
async fn missing_local_assignment_history_blocks_a_normal_claim_before_any_new_delegation()
-> TestResult {
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::Prepared).await;
    scratch.initialize(&api.origin).await;
    success(run(&mut scratch.claim()).await);
    let mut damaged = scratch.read();
    damaged["prepared_assignments"] = json!([]);
    scratch.write(&damaged);
    failure(
        run(&mut scratch.claim()).await,
        "unknown submission outcomes",
    );
    let recovered = scratch.read();
    assert_eq!(
        recovered["prepared_assignments"][0]["submission_state"],
        "unknown"
    );
    assert!(recovered["prepared_assignments"][0]["launch_intent_id"].is_null());
    assert_eq!(api.observations.lock().unwrap().claim_keys.len(), 1);
    success(run(&mut scratch.reconcile(true)).await);
    success(run(&mut scratch.reconcile(false)).await);
    assert_eq!(
        scratch.read()["prepared_assignments"],
        recovered["prepared_assignments"]
    );
    Ok(())
}

#[tokio::test]
async fn reconciliation_keeps_unknown_submission_and_checks_local_entries_missing_from_list()
-> TestResult {
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::Prepared).await;
    scratch.initialize(&api.origin).await;
    success(run(&mut scratch.claim()).await);
    let mut journal = scratch.read();
    journal["prepared_assignments"][0]["submission_state"] = json!("unknown");
    scratch.write(&journal);
    api.observations.lock().unwrap().omit_outstanding = true;
    let output = success(run(&mut scratch.reconcile(false)).await);
    assert_eq!(output["status"], "reconciliation_required");
    assert_eq!(
        scratch.read()["prepared_assignments"],
        journal["prepared_assignments"]
    );
    api.observations.lock().unwrap().assignment = None;
    failure(
        run(&mut scratch.reconcile(false)).await,
        "assignment retrieval returned HTTP 404",
    );
    assert!(scratch.read()["active_session_id"].is_string());
    Ok(())
}

#[tokio::test]
async fn reconciliation_refuses_a_second_local_process_even_with_stop_assertion() -> TestResult {
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::Prepared).await;
    scratch.initialize(&api.origin).await;
    let lock = File::open(scratch.journal().join("owner.lock"))?;
    lock.try_lock()?;
    failure(
        run(&mut scratch.reconcile(true)).await,
        "already owned by another process",
    );
    assert!(api.observations.lock().unwrap().calls.is_empty());
    Ok(())
}

#[tokio::test]
async fn an_assignment_for_another_owner_is_not_prepared() -> TestResult {
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::WrongOwner).await;
    scratch.initialize(&api.origin).await;
    failure(
        run(&mut scratch.claim()).await,
        "does not belong to this connector journal",
    );
    assert!(scratch.read()["pending_claim_key"].is_string());
    assert!(
        scratch.read()["prepared_assignments"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(!api.observations.lock().unwrap().calls.contains(&"close"));
    Ok(())
}

#[tokio::test]
async fn server_ownership_conflicts_are_reported_without_response_body_or_token() -> TestResult {
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::SessionConflict).await;
    scratch.initialize(&api.origin).await;
    failure(
        run(&mut scratch.claim()).await,
        "session registration returned HTTP 409",
    );
    assert_eq!(api.observations.lock().unwrap().calls, ["session"]);
    assert!(
        scratch.read()["prepared_assignments"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn an_exposed_token_file_is_rejected_before_registering_ownership() -> TestResult {
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::Prepared).await;
    scratch.initialize(&api.origin).await;
    fs::set_permissions(scratch.0.join("token"), fs::Permissions::from_mode(0o644))?;
    failure(run(&mut scratch.claim()).await, "must be private");
    assert!(api.observations.lock().unwrap().calls.is_empty());
    assert!(scratch.read()["active_session_id"].is_null());
    Ok(())
}

#[tokio::test]
async fn credentials_can_be_supplied_on_stdin_without_process_arguments() -> TestResult {
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::Empty).await;
    scratch.initialize(&api.origin).await;
    let mut child = command()
        .arg("claim-once")
        .arg("--journal")
        .arg(scratch.journal())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(format!("{TOKEN}\n").as_bytes()).await?;
    stdin.shutdown().await?;
    drop(stdin);
    let receipt = success(timeout(Duration::from_secs(5), child.wait_with_output()).await??);
    assert_eq!(receipt["status"], "empty");
    Ok(())
}

#[tokio::test]
async fn a_journal_with_duplicate_execution_identities_fails_closed() -> TestResult {
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::Prepared).await;
    scratch.initialize(&api.origin).await;
    success(run(&mut scratch.claim()).await);
    let mut journal = scratch.read();
    let prepared = journal["prepared_assignments"][0].clone();
    journal["prepared_assignments"]
        .as_array_mut()
        .unwrap()
        .push(prepared);
    fs::write(
        scratch.journal().join("journal.json"),
        serde_json::to_vec(&journal)?,
    )?;
    let calls = api.observations.lock().unwrap().calls.len();
    failure(
        run(&mut scratch.claim()).await,
        "duplicate execution identities",
    );
    assert_eq!(api.observations.lock().unwrap().calls.len(), calls);
    assert_eq!(scratch.read(), journal);
    Ok(())
}

#[tokio::test]
async fn revoked_manual_assignment_never_crosses_the_submission_boundary() -> TestResult {
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::Executable).await;
    scratch.initialize(&api.origin).await;
    let claimed = success(run(&mut scratch.claim()).await);
    api.observations.lock().unwrap().revoked = true;
    failure(
        run(&mut scratch.execute(false)).await,
        "authorization revoked before launch",
    );
    let retained = scratch.read();
    assert_eq!(
        retained["prepared_assignments"][0]["launch_intent_id"],
        claimed["launch_intent_id"]
    );
    assert_eq!(
        retained["prepared_assignments"][0]["submission_state"],
        "definitely_unsubmitted"
    );
    assert!(!scratch.journal().join("compute-jobs").exists());
    let peer = api.observations.lock().unwrap();
    assert!(peer.events.is_empty() && peer.result.is_none());
    Ok(())
}

#[tokio::test]
async fn lost_backend_acknowledgement_recovers_revoked_work_without_another_launch() -> TestResult {
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::Executable).await;
    scratch.initialize(&api.origin).await;
    let claimed = success(run(&mut scratch.claim()).await);
    failure(
        run(scratch.execute(false).arg("--simulate-lost-submission-ack")).await,
        "lost backend submission acknowledgement",
    );
    let uncertain = scratch.read();
    assert_eq!(
        uncertain["prepared_assignments"][0]["submission_state"],
        "unknown"
    );
    assert!(uncertain["prepared_assignments"][0]["execution"].is_null());
    let backend_path = scratch.journal().join("compute-jobs").join(format!(
        "{}/job.json",
        claimed["launch_intent_id"].as_str().unwrap()
    ));
    let original_launch = fs::read(&backend_path)?;
    api.observations.lock().unwrap().revoked = true;
    failure(
        run(&mut scratch.execute(false)).await,
        "--confirm-prior-process-stopped",
    );
    let completed = success(run(&mut scratch.execute(true)).await);
    assert_eq!(completed["status"], "completed");
    assert_eq!(completed["launch_intent_id"], claimed["launch_intent_id"]);
    assert_eq!(completed["result_receipt"]["outcome"], "succeeded");
    assert_eq!(
        scratch.read()["prepared_assignments"][0]["terminal_result"]["outputs"],
        json!({"doubled":14})
    );
    assert_eq!(
        success(run(&mut scratch.execute(false)).await)["status"],
        "idle"
    );
    assert_eq!(fs::read(&backend_path)?, original_launch);
    assert_eq!(
        fs::read_dir(scratch.journal().join("compute-jobs"))?.count(),
        1
    );
    let state = api.observations.lock().unwrap();
    assert_eq!(state.claim_keys.len(), 1);
    assert_eq!(state.events.len(), 2);
    assert_eq!(
        state.calls.iter().filter(|call| **call == "result").count(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn missing_backend_history_never_authorizes_an_unknown_launch() -> TestResult {
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::Executable).await;
    scratch.initialize(&api.origin).await;
    let claimed = success(run(&mut scratch.claim()).await);
    failure(
        run(scratch.execute(false).arg("--simulate-lost-submission-ack")).await,
        "lost backend submission acknowledgement",
    );
    fs::remove_file(scratch.journal().join("compute-jobs").join(format!(
        "{}/job.json",
        claimed["launch_intent_id"].as_str().unwrap()
    )))?;
    for _ in 0..2 {
        failure(
            run(&mut scratch.execute(true)).await,
            "unknown submission remains blocked",
        );
        assert_eq!(
            scratch.read()["prepared_assignments"][0]["submission_state"],
            "unknown"
        );
        assert_eq!(
            fs::read_dir(scratch.journal().join("compute-jobs"))?.count(),
            1
        );
    }
    let state = api.observations.lock().unwrap();
    assert_eq!(state.claim_keys.len(), 1);
    assert!(state.events.is_empty());
    assert!(state.result.is_none());
    Ok(())
}

#[tokio::test]
async fn lost_event_acknowledgement_replays_durable_identity_without_reexecution() -> TestResult {
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::Executable).await;
    scratch.initialize(&api.origin).await;
    success(run(&mut scratch.claim()).await);
    api.observations.lock().unwrap().lost_event_responses = 3;
    failure(
        run(&mut scratch.execute(false)).await,
        "execution event returned HTTP 503",
    );
    let pending = scratch.read()["prepared_assignments"][0]["execution"]["events"][0].clone();
    assert!(pending["receipt"].is_null());
    assert_eq!(
        success(run(&mut scratch.execute(true)).await)["status"],
        "completed"
    );
    let resumed = scratch.read()["prepared_assignments"][0]["execution"]["events"][0].clone();
    assert_eq!(pending["id"], resumed["id"]);
    assert_eq!(pending["request"], resumed["request"]);
    assert_eq!(
        fs::read_dir(scratch.journal().join("compute-jobs"))?.count(),
        1
    );
    assert_eq!(api.observations.lock().unwrap().events.len(), 2);
    Ok(())
}

#[tokio::test]
async fn lost_terminal_acknowledgement_recovers_from_detail_after_outstanding_disappears()
-> TestResult {
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::Executable).await;
    scratch.initialize(&api.origin).await;
    success(run(&mut scratch.claim()).await);
    api.observations.lock().unwrap().lost_result_responses = 3;
    failure(
        run(&mut scratch.execute(false)).await,
        "terminal report returned HTTP 503",
    );
    let pending = scratch.read()["prepared_assignments"][0].clone();
    assert!(pending["execution"]["receipt"].is_null());
    assert!(pending["terminal_result"].is_null());
    assert_eq!(
        success(run(&mut scratch.execute(true)).await)["status"],
        "idle"
    );
    let recovered = scratch.read()["prepared_assignments"][0].clone();
    assert_eq!(
        recovered["execution"]["report"],
        pending["execution"]["report"]
    );
    assert_eq!(
        recovered["terminal_result"],
        api.observations.lock().unwrap().result.clone().unwrap()
    );
    assert_eq!(
        recovered["execution"]["receipt"],
        recovered["terminal_result"]["receipt"]
    );
    assert_eq!(
        fs::read_dir(scratch.journal().join("compute-jobs"))?.count(),
        1
    );
    assert_eq!(api.observations.lock().unwrap().claim_keys.len(), 1);
    Ok(())
}

#[tokio::test]
async fn observed_backend_evidence_blocks_a_restored_unsubmitted_journal() -> TestResult {
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::Executable).await;
    scratch.initialize(&api.origin).await;
    success(run(&mut scratch.claim()).await);
    api.observations.lock().unwrap().backend_evidence =
        Some(json!({"namespace":"other-backend", "job_id":"existing-job"}));
    failure(
        run(&mut scratch.execute(false)).await,
        "unknown submission remains blocked",
    );
    assert_eq!(
        scratch.read()["prepared_assignments"][0]["submission_state"],
        "unknown"
    );
    assert!(!scratch.journal().join("compute-jobs").exists());
    assert!(api.observations.lock().unwrap().events.is_empty());
    Ok(())
}

#[tokio::test]
async fn unsupported_inputs_are_rejected_before_the_backend_submission_boundary() -> TestResult {
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::Prepared).await;
    scratch.initialize(&api.origin).await;
    success(run(&mut scratch.claim()).await);
    let done = success(run(&mut scratch.execute(false)).await);
    assert_eq!(done["status"], "completed");
    assert_eq!(
        scratch.read()["prepared_assignments"][0]["submission_state"],
        "submitted"
    );
    assert!(!scratch.journal().join("compute-jobs").exists());
    let retained = scratch.read()["prepared_assignments"][0].clone();
    assert_eq!(
        success(run(&mut scratch.execute(false)).await)["status"],
        "idle"
    );
    assert_eq!(scratch.read()["prepared_assignments"][0], retained);
    assert_eq!(
        retained["execution"]["startup_evidence"]["stage"],
        "launch_preparation"
    );
    assert_eq!(
        retained["execution"]["report"]["failure"]["code"],
        "launch_preparation_failed"
    );
    assert_eq!(
        retained["execution"]["report"]["failure"]["message"],
        "inputs do not match pinned model schema"
    );
    let state = api.observations.lock().unwrap();
    assert_eq!(state.events.len(), 1);
    assert!(state.events[0].1["phase"].is_null());
    assert_eq!(state.logs.len(), 2);
    assert!(state.logs.iter().all(|log| {
        log["request"]["byte_length"] == 0
            && log["request"]["gap"]["reason"] == "source_unavailable"
            && log["request"]["gap"]["bytes"].is_null()
    }));
    assert_eq!(
        state.result.as_ref().unwrap()["receipt"]["outcome"],
        "failed"
    );
    Ok(())
}

#[tokio::test]
async fn one_off_claim_reclaims_full_delivered_capacity_without_losing_evidence() -> TestResult {
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::Empty).await;
    scratch.initialize(&api.origin).await;
    let mut journal = scratch.read();
    let fixture: Value = serde_json::from_str(include_str!(
        "../../connector/tests/fixtures/journal-minimal-evidence.json"
    ))?;
    let mut assignment = fixture["prepared_assignments"][0]["assignment"].clone();
    assignment["journal_id"] = journal["journal_id"].clone();
    let mut entries = Vec::new();
    for _ in 0..64 {
        for identity in ["assignment_id", "run_id", "attempt_id"] {
            assignment[identity] = json!(Uuid::new_v4());
        }
        entries.push(json!({
            "assignment":assignment,
            "launch_intent_id":null,
            "submission_state":"unknown",
            "delivery_completion":{"assignment_id":assignment["assignment_id"],"run_id":assignment["run_id"],"attempt_id":assignment["attempt_id"],"source":"connector","completed_at":"2026-09-21T12:00:00Z"},
            "terminal_result":{"receipt":{"assignment_id":assignment["assignment_id"],"run_id":assignment["run_id"],"attempt_id":assignment["attempt_id"],"outcome":"succeeded","completed_at":"2026-09-21T12:00:00Z"}}
        }));
    }
    journal["prepared_assignments"] = json!(entries);
    scratch.write(&journal);
    assert_eq!(success(run(&mut scratch.claim()).await)["status"], "empty");
    assert_eq!(scratch.read()["prepared_assignments"], json!([]));
    assert_eq!(fs::read_dir(scratch.journal().join("archive"))?.count(), 64);
    assert_eq!(
        fs::read_dir(scratch.journal().join("cleanup/pending"))?.count(),
        64
    );
    for entry in entries {
        let path = scratch.journal().join("archive").join(format!(
            "{}.json",
            entry["assignment"]["assignment_id"].as_str().unwrap()
        ));
        let archived: Value = serde_json::from_slice(&fs::read(path)?)?;
        assert_eq!(archived["entry"], entry);
    }
    Ok(())
}

#[tokio::test]
async fn a_restored_server_cannot_hide_a_retained_terminal_receipt() -> TestResult {
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::Executable).await;
    scratch.initialize(&api.origin).await;
    success(run(&mut scratch.claim()).await);
    success(run(&mut scratch.execute(false)).await);
    let completed = scratch.read()["prepared_assignments"][0].clone();
    api.observations.lock().unwrap().result = None;
    failure(
        run(&mut scratch.execute(false)).await,
        "server progress conflicts with the durable local terminal receipt",
    );
    assert_eq!(scratch.read()["prepared_assignments"][0], completed);
    assert_eq!(
        fs::read_dir(scratch.journal().join("compute-jobs"))?.count(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn equivalent_decimal_result_spelling_recovers_but_changed_values_conflict() -> TestResult {
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::Executable).await;
    scratch.initialize(&api.origin).await;
    success(run(&mut scratch.claim()).await);
    success(run(&mut scratch.execute(false)).await);
    api.observations.lock().unwrap().result.as_mut().unwrap()["outputs"]["doubled"] =
        serde_json::from_str("14e0")?;
    assert_eq!(
        success(run(&mut scratch.execute(false)).await)["status"],
        "idle"
    );
    api.observations.lock().unwrap().result.as_mut().unwrap()["outputs"]["doubled"] =
        serde_json::from_str("14.0000000000000000000000001")?;
    failure(
        run(&mut scratch.execute(false)).await,
        "terminal result conflicts with the durable local report",
    );
    assert_eq!(
        fs::read_dir(scratch.journal().join("compute-jobs"))?.count(),
        1
    );
    Ok(())
}

fn resolve_assignment(state: &mut Observation, execution_status: &str) {
    let assignment = state.assignment.as_ref().unwrap();
    let resolved_at = "2026-09-21T12:00:00Z";
    state.resolution = Some(json!({"assignment_id":assignment["assignment_id"],
        "attempt_id":assignment["attempt_id"], "execution_status":execution_status, "resolved_at":resolved_at}));
    state.result = Some(
        json!({"receipt":{"assignment_id":assignment["assignment_id"],
        "run_id":assignment["run_id"], "attempt_id":assignment["attempt_id"], "outcome":"failed", "completed_at":resolved_at},
        "outputs":{}, "files":{}, "optional_output_failures":{},
        "failure":{"code":"operator_resolved", "message":"An operator confirmed execution termination and resolved this assignment."}}),
    );
}

#[tokio::test]
async fn missing_backend_after_lost_ack_stays_blocked_until_audited_resolution_without_relaunch()
-> TestResult {
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::Executable).await;
    scratch.initialize(&api.origin).await;
    let claimed = success(run(&mut scratch.claim()).await);
    failure(
        run(scratch.execute(false).arg("--simulate-lost-submission-ack")).await,
        "lost backend submission acknowledgement",
    );
    let uncertain = scratch.read()["prepared_assignments"][0].clone();
    let path = scratch.journal().join("compute-jobs").join(format!(
        "{}/job.json",
        claimed["launch_intent_id"].as_str().unwrap()
    ));
    let launch = fs::read(&path)?;
    fs::remove_file(&path)?;
    for freshness in ["unknown", "stale", "fresh"] {
        api.observations.lock().unwrap().observation_freshness = freshness;
        failure(
            run(&mut scratch.execute(true)).await,
            "unknown submission remains blocked",
        );
        assert_eq!(scratch.read()["prepared_assignments"][0], uncertain);
        assert!(!path.exists());
    }
    resolve_assignment(&mut api.observations.lock().unwrap(), "terminated");
    let reconciled = success(run(&mut scratch.reconcile(true)).await);
    assert_eq!(reconciled["status"], "reconciled");
    let resolved = scratch.read()["prepared_assignments"][0].clone();
    for field in [
        "assignment",
        "launch_intent_id",
        "submission_state",
        "execution",
    ] {
        assert_eq!(resolved[field], uncertain[field]);
    }
    assert_eq!(resolved["submission_state"], "unknown");
    assert_eq!(
        resolved["operator_resolution"]["execution_status"],
        "terminated"
    );
    assert_eq!(resolved["terminal_result"]["receipt"]["outcome"], "failed");
    assert_eq!(
        success(run(&mut scratch.execute(false)).await)["status"],
        "idle"
    );
    assert!(!path.exists());
    assert!(!launch.is_empty());
    api.observations.lock().unwrap().behavior = Behavior::Empty;
    assert_eq!(success(run(&mut scratch.claim()).await)["status"], "empty");
    let state = api.observations.lock().unwrap();
    assert_eq!(state.claim_keys.len(), 2);
    assert!(state.events.is_empty());
    assert!(!state.calls.contains(&"result"));
    Ok(())
}

#[tokio::test]
async fn operator_resolution_preserves_pending_events_report_and_backend_without_acknowledging_them()
-> TestResult {
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::Executable).await;
    scratch.initialize(&api.origin).await;
    let claimed = success(run(&mut scratch.claim()).await);
    api.observations.lock().unwrap().lost_event_responses = 3;
    failure(
        run(&mut scratch.execute(false)).await,
        "execution event returned HTTP 503",
    );
    let pending = scratch.read()["prepared_assignments"][0].clone();
    let path = scratch.journal().join("compute-jobs").join(format!(
        "{}/job.json",
        claimed["launch_intent_id"].as_str().unwrap()
    ));
    let launch = fs::read(&path)?;
    resolve_assignment(&mut api.observations.lock().unwrap(), "terminated");
    assert_eq!(
        success(run(&mut scratch.execute(true)).await)["status"],
        "idle"
    );
    let resolved = scratch.read()["prepared_assignments"][0].clone();
    assert_eq!(resolved["execution"], pending["execution"]);
    assert_eq!(resolved["execution"]["report"]["outcome"], "succeeded");
    assert!(resolved["execution"]["receipt"].is_null());
    assert!(resolved["execution"]["events"][0]["receipt"].is_null());
    assert_eq!(resolved["terminal_result"]["receipt"]["outcome"], "failed");
    assert_eq!(fs::read(&path)?, launch);
    assert_eq!(
        success(run(&mut scratch.reconcile(false)).await)["status"],
        "reconciled"
    );
    assert_eq!(scratch.read()["prepared_assignments"][0], resolved);
    let state = api.observations.lock().unwrap();
    assert_eq!(state.events.len(), 1);
    assert!(!state.calls.contains(&"result"));
    Ok(())
}

#[tokio::test]
async fn resolution_of_a_retrieved_assignment_never_invents_missing_launch_history() -> TestResult {
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::Executable).await;
    scratch.initialize(&api.origin).await;
    success(run(&mut scratch.claim()).await);
    let mut lost = scratch.read();
    lost["prepared_assignments"] = json!([]);
    scratch.write(&lost);
    assert_eq!(
        success(run(&mut scratch.reconcile(false)).await)["status"],
        "reconciliation_required"
    );
    let unknown = scratch.read()["prepared_assignments"][0].clone();
    assert!(unknown["launch_intent_id"].is_null());
    resolve_assignment(&mut api.observations.lock().unwrap(), "never_started");
    assert_eq!(
        success(run(&mut scratch.execute(false)).await)["status"],
        "idle"
    );
    let resolved = scratch.read()["prepared_assignments"][0].clone();
    assert!(resolved["launch_intent_id"].is_null());
    assert_eq!(resolved["submission_state"], "unknown");
    assert!(!scratch.journal().join("compute-jobs").exists());
    Ok(())
}

#[tokio::test]
async fn malformed_resolution_and_conflicting_terminal_history_fail_closed() -> TestResult {
    for corruption in [
        "attempt_id",
        "assignment_id",
        "status",
        "extra",
        "timestamp",
        "result",
        "running",
        "acknowledged",
    ] {
        let scratch = Scratch::with_token(TOKEN);
        let api = Api::new(&scratch, Behavior::Executable).await;
        scratch.initialize(&api.origin).await;
        success(run(&mut scratch.claim()).await);
        if corruption == "acknowledged" {
            success(run(&mut scratch.execute(false)).await);
        }
        let before = scratch.read()["prepared_assignments"][0].clone();
        {
            let mut state = api.observations.lock().unwrap();
            resolve_assignment(&mut state, "terminated");
            match corruption {
                "attempt_id" | "assignment_id" => {
                    state.resolution.as_mut().unwrap()[corruption] = json!(Uuid::new_v4())
                }
                "status" => {
                    state.resolution.as_mut().unwrap()["execution_status"] = json!("unknown")
                }
                "extra" => state.resolution.as_mut().unwrap()["actor"] = json!("must-not-leak"),
                "timestamp" => {
                    state.resolution.as_mut().unwrap()["resolved_at"] =
                        json!("2026-09-21T12:00:01Z")
                }
                "result" => state.result.as_mut().unwrap()["outputs"] = json!({"doubled":14}),
                "running" => state.result = None,
                _ => (),
            }
        }
        let outcome = run(&mut scratch.execute(false)).await;
        assert!(!outcome.status.success(), "{corruption}");
        assert!(outcome.stdout.is_empty());
        assert_eq!(
            scratch.read()["prepared_assignments"][0],
            before,
            "{corruption}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn unknown_journal_version_is_rejected_without_erasing_execution_evidence() -> TestResult {
    let scratch = Scratch::with_token(TOKEN);
    let api = Api::new(&scratch, Behavior::Prepared).await;
    scratch.initialize(&api.origin).await;
    success(run(&mut scratch.claim()).await);
    let mut legacy = scratch.read();
    legacy["version"] = json!(2);
    scratch.write(&legacy);
    failure(
        run(&mut scratch.reconcile(true)).await,
        "unsupported journal version",
    );
    assert_eq!(scratch.read(), legacy);
    Ok(())
}

#[tokio::test]
async fn never_started_resolution_cannot_override_a_retained_backend_launch() -> TestResult {
    for pending_events in [false, true] {
        let scratch = Scratch::with_token(TOKEN);
        let api = Api::new(&scratch, Behavior::Executable).await;
        scratch.initialize(&api.origin).await;
        let claimed = success(run(&mut scratch.claim()).await);
        if pending_events {
            api.observations.lock().unwrap().lost_event_responses = 3;
            failure(
                run(&mut scratch.execute(false)).await,
                "execution event returned HTTP 503",
            );
        } else {
            failure(
                run(scratch.execute(false).arg("--simulate-lost-submission-ack")).await,
                "lost backend submission acknowledgement",
            );
        }
        let before = scratch.read()["prepared_assignments"][0].clone();
        let path = scratch.journal().join("compute-jobs").join(format!(
            "{}/job.json",
            claimed["launch_intent_id"].as_str().unwrap()
        ));
        let backend = fs::read(&path)?;
        resolve_assignment(&mut api.observations.lock().unwrap(), "never_started");
        failure(
            run(&mut scratch.execute(true)).await,
            "never-started operator resolution conflicts",
        );
        assert_eq!(scratch.read()["prepared_assignments"][0], before);
        assert_eq!(fs::read(path)?, backend);
    }
    Ok(())
}
