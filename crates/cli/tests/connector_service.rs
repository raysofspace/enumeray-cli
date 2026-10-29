//! Continuous-service behavior against a controllable HTTP peer and real models.
#![cfg(unix)]
#[path = "connector_support/mod.rs"]
mod support;
use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use support::Scratch;
use tokio::{
    net::TcpListener,
    process::{Child, Command},
    task::JoinHandle,
};
use uuid::Uuid;
const TOKEN: &str = "enr_c1_service-test-secret";
const CONNECTOR: &str = "local-process-connector";

struct Job {
    cleanup_authority: bool,
    completed_at: String,
    assignment: Value,
    result: Option<Value>,
    delivery_completion: Option<Value>,
    resolution: Option<Value>,
}
#[derive(Default)]
struct Peer {
    installation_root: Option<PathBuf>,
    assignment_descriptor: Option<Value>,
    configuration_unavailable: bool,
    hold_configuration_ack: bool,
    configuration_acks: usize,
    validations: Vec<Value>,
    cleanup_enabled: bool,
    completed_retention: Option<u64>,
    remaining: usize,
    allowed_outstanding: usize,
    maximum_observed: usize,
    local: bool,
    jobs: BTreeMap<Uuid, Job>,
    claims: BTreeMap<String, Uuid>,
    claim_calls: Vec<String>,
    heartbeat: usize,
    sessions: usize,
    recoveries: usize,
    logs: BTreeMap<String, Value>,
    events: BTreeMap<String, Value>,
    unavailable: bool,
    unavailable_heartbeats: usize,
    cancel_requested: bool,
    cancellation_controls: usize,
    lose_claim: bool,
    lose_report: bool,
    lose_delivery: bool,
    hold_delivery: bool,
    delivery_calls: usize,
    maintenance: bool,
    authority_withdrawn: bool,
    lose_event: bool,
    slurm: bool,
    lose_log: bool,
    active_session: Option<Value>,
    journal_id: Value,
    origin: String,
    bulk_input: bool,
    bulk_output: bool,
    large_output: bool,
    hold_input: bool,
    hold_output: bool,
    input_started: usize,
    output_started: usize,
    output_verified: bool,
    cancel_assignment: Option<Uuid>,
    local_workers: Option<usize>,
    retention: Option<u64>,
    pending_cap: Option<usize>,
    transfer_workers: Option<usize>,
    expire_reservation: bool,
    reservation_keys: Vec<String>,
    lose_verification: bool,
    hold_archive: bool,
    archive_started: usize,
    archives: BTreeMap<Uuid, Value>,
    unavailable_archive_intent: bool,
}
struct Api {
    origin: String,
    state: Arc<Mutex<Peer>>,
    task: JoinHandle<()>,
}

#[tokio::test]
async fn slow_configuration_acknowledgment_keeps_running_cancellation_and_observation_responsive() {
    let scratch = Scratch::new();
    install_gated(&scratch);
    let api = Api::start(1, true).await;
    let mut child = start(&scratch, &api, 1);
    api.until(|peer| {
        peer.events
            .values()
            .any(|event| event["phase"] == "executing")
    })
    .await;
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    let previous_acks = {
        let mut peer = api.state.lock().unwrap();
        peer.hold_configuration_ack = true;
        peer.configuration_acks
    };
    let child = start(&scratch, &api, 1);
    api.until(|peer| peer.configuration_acks > previous_acks)
        .await;
    // The acknowledgment is still blocked, after the journal owner has already
    // committed the cache. A pending HTTP request must not delay provider
    // observation or cancellation polling. Completion and publication involve
    // several real runner processes and get their ordinary convergence budget.
    assert!(
        scratch
            .journal()
            .join("compiled-installations.json")
            .exists()
    );
    api.state.lock().unwrap().cancel_requested = true;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if api.state.lock().unwrap().cancellation_controls > 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "configuration acknowledgment blocked cancellation: {}",
            fs::read_to_string(scratch.0.join("service.log")).unwrap()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    api.until(|peer| peer.jobs.values().all(|job| job.result.is_some()))
        .await;
    assert!(api.state.lock().unwrap().hold_configuration_ack);
    {
        let peer = api.state.lock().unwrap();
        assert_eq!(
            peer.jobs.values().next().unwrap().result.as_ref().unwrap()["receipt"]["outcome"],
            "cancelled"
        );
        assert!(
            peer.events
                .values()
                .any(|event| event["diagnostic"]["code"] == "user_cancelled")
        );
    }
    stop(child).await;
    api.state.lock().unwrap().hold_configuration_ack = false;
}

#[tokio::test]
async fn shutdown_during_configuration_acknowledgment_is_prompt() {
    let scratch = Scratch::new();
    install(&scratch, 0.0);
    let api = Api::start(1, true).await;
    api.state.lock().unwrap().hold_configuration_ack = true;
    let child = start(&scratch, &api, 1);
    api.until(|peer| peer.configuration_acks > 0).await;
    // Stop while the request is pending, independently of the time needed to
    // cancel and publish a real model in the recovery scenario above.
    let stopped = Instant::now();
    assert_eq!(stop(child).await["drained"], true);
    assert!(stopped.elapsed() < Duration::from_secs(3));
    let mut peer = api.state.lock().unwrap();
    assert!(peer.hold_configuration_ack);
    assert!(peer.jobs.is_empty());
    peer.hold_configuration_ack = false;
}

#[tokio::test]
async fn fresh_configuration_cannot_claim_work_before_its_acknowledgment() {
    let scratch = Scratch::new();
    install(&scratch, 0.0);
    let api = Api::start(1, true).await;
    api.state.lock().unwrap().hold_configuration_ack = true;
    let child = start(&scratch, &api, 1);
    api.until(|peer| peer.configuration_acks > 0).await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(api.state.lock().unwrap().jobs.is_empty());
    api.state.lock().unwrap().hold_configuration_ack = false;
    archived(&scratch, 1).await;
    stop(child).await;
}

#[tokio::test]
async fn cleanup_reports_each_persistently_blocked_run_once_in_service_logs() {
    let scratch = Scratch::new();
    install(&scratch, 0.0);
    let api = Api::start(1, false).await;
    // Provider deletion authority is deliberately absent from its receipt.
    let child = start(&scratch, &api, 1);
    archived(&scratch, 1).await;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if fs::read_to_string(scratch.0.join("service.log"))
            .unwrap()
            .lines()
            .any(|line| {
                serde_json::from_str::<Value>(line).is_ok_and(|value| {
                    value["status"] == "cleanup_blocked"
                        && value["message"]
                            .as_str()
                            .is_some_and(|message| message.contains("assignment_id"))
                })
            })
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "blocked cleanup was not reported"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(2200)).await;
    stop(child).await;
    let blocked: Vec<_> = fs::read_to_string(scratch.0.join("service.log"))
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|value| value["status"] == "cleanup_blocked")
        .collect();
    assert_eq!(blocked.len(), 1);
    let report: Value = serde_json::from_str(blocked[0]["message"].as_str().unwrap()).unwrap();
    assert_eq!(report["state"], "blocked");
    assert_eq!(
        report["assignment_id"],
        api.state
            .lock()
            .unwrap()
            .jobs
            .keys()
            .next()
            .unwrap()
            .to_string()
    );
    assert!(report["diagnostic"].as_str().unwrap().contains("cleanup"));
}

#[tokio::test]
async fn restart_collects_running_publication_when_configuration_is_unavailable() {
    let scratch = Scratch::new();
    install(&scratch, 2.0);
    let api = Api::start(2, true).await;
    let mut child = start(&scratch, &api, 1);
    api.until(|peer| {
        peer.events
            .values()
            .any(|event| event["phase"] == "executing")
    })
    .await;
    let cache = fs::read(scratch.journal().join("compiled-installations.json")).unwrap();
    let first = scratch.read()["prepared_assignments"][0]["assignment"].clone();
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    api.state.lock().unwrap().configuration_unavailable = true;
    let child = start(&scratch, &api, 3);
    archived(&scratch, 1).await;
    stop(child).await;
    assert_eq!(
        fs::read(scratch.journal().join("compiled-installations.json")).unwrap(),
        cache
    );
    let peer = api.state.lock().unwrap();
    assert_eq!(peer.jobs.len(), 1);
    assert_eq!(peer.jobs.values().next().unwrap().assignment, first);
    assert_eq!(
        peer.jobs.values().next().unwrap().result.as_ref().unwrap()["receipt"]["outcome"],
        "succeeded"
    );
    assert_eq!(
        fs::read_to_string(scratch.0.join("launches"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[tokio::test]
async fn delivery_acknowledgement_outage_keeps_other_scientific_work_progressing() {
    let scratch = Scratch::new();
    let api = Api::start(2, false).await;
    api.state.lock().unwrap().hold_delivery = true;
    let child = start(&scratch, &api, 1);
    api.until(|peer| peer.jobs.len() == 2 && peer.jobs.values().all(|job| job.result.is_some()))
        .await;
    assert!(
        api.state
            .lock()
            .unwrap()
            .jobs
            .values()
            .all(|job| job.delivery_completion.is_none())
    );
    assert_eq!(stop(child).await["drained"], false);
    api.state.lock().unwrap().hold_delivery = false;
    let child = start(&scratch, &api, 3);
    archived(&scratch, 2).await;
    assert_eq!(stop(child).await["drained"], true);
}

#[tokio::test]
async fn shutdown_fences_an_uncertain_claim_without_inventing_launch_authority() {
    let scratch = Scratch::new();
    install(&scratch, 0.1);
    let api = Api::start(1, true).await;
    api.state.lock().unwrap().lose_claim = true;
    let child = start(&scratch, &api, 2);
    api.until(|peer| peer.jobs.len() == 1).await;
    let pending = scratch.read();
    assert!(pending["pending_claim_key"].is_string());
    assert!(
        pending["prepared_assignments"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(stop(child).await["drained"], false);
    let peer = api.state.lock().unwrap();
    assert_eq!(peer.claim_calls.len(), 1);
    assert_eq!(peer.jobs.len(), 1);
    assert!(peer.recoveries > 0);
    assert!(peer.jobs.values().all(|job| job.result.is_none()));
    let retained = scratch.read();
    assert!(retained["pending_claim_key"].is_null());
    assert_eq!(
        retained["prepared_assignments"].as_array().unwrap().len(),
        1
    );
    assert!(retained["prepared_assignments"][0]["launch_intent_id"].is_null());
    assert_eq!(
        retained["prepared_assignments"][0]["submission_state"],
        "unknown"
    );
    assert!(!scratch.0.join("launches").exists());
}

#[tokio::test]
async fn delivery_receipt_recovers_after_lost_reply_and_restart_without_reexecution() {
    let scratch = Scratch::new();
    install(&scratch, 0.1);
    let api = Api::start(1, true).await;
    api.state.lock().unwrap().lose_delivery = true;
    let mut child = start(&scratch, &api, 1);
    api.until(|peer| {
        peer.jobs
            .values()
            .any(|job| job.delivery_completion.is_some())
    })
    .await;
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    let retained = scratch.read()["prepared_assignments"][0].clone();
    // Delivery recovery uses original proofs even when a new configuration
    // cannot be acknowledged. It must not acquire fresh execution authority.
    api.state.lock().unwrap().configuration_unavailable = true;
    let child = start(&scratch, &api, 3);
    archived(&scratch, 1).await;
    assert_eq!(stop(child).await["drained"], true);
    assert_eq!(api.state.lock().unwrap().delivery_calls, 1);
    let assignment_id = retained["assignment"]["assignment_id"].as_str().unwrap();
    let archive: Value = serde_json::from_slice(
        &fs::read(
            scratch
                .journal()
                .join("archive")
                .join(format!("{assignment_id}.json")),
        )
        .unwrap(),
    )
    .unwrap();
    for field in [
        "assignment",
        "launch_intent_id",
        "execution",
        "terminal_result",
    ] {
        assert_eq!(archive["entry"][field], retained[field], "{field}");
    }
    assert_eq!(
        archive["entry"]["delivery_completion"],
        serde_json::to_value(
            api.state
                .lock()
                .unwrap()
                .jobs
                .values()
                .next()
                .unwrap()
                .delivery_completion
                .as_ref()
                .unwrap()
        )
        .unwrap()
    );
    assert_eq!(
        fs::read_to_string(scratch.0.join("launches")).unwrap(),
        "launch\n"
    );
}

#[tokio::test]
async fn undisclosed_archive_never_settles_scientific_completion_and_recovers_after_restart() {
    let scratch = Scratch::new();
    install(&scratch, 0.1);
    fs::write(
        scratch.0.join("log-policy.json"),
        r#"{"archive_enabled":true}"#,
    )
    .unwrap();
    let api = Api::start(1, true).await;
    api.state.lock().unwrap().unavailable_archive_intent = true;
    let mut child = start(&scratch, &api, 1);
    api.until(|peer| peer.jobs.values().any(|job| job.result.is_some()))
        .await;
    {
        let peer = api.state.lock().unwrap();
        assert!(peer.archives.is_empty());
        assert_eq!(peer.delivery_calls, 0);
        assert!(
            peer.jobs
                .values()
                .all(|job| job.delivery_completion.is_none())
        );
    }
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    api.state.lock().unwrap().unavailable_archive_intent = false;
    let child = start(&scratch, &api, 3);
    archived(&scratch, 1).await;
    assert_eq!(stop(child).await["drained"], true);
    assert_eq!(
        fs::read_to_string(scratch.0.join("launches")).unwrap(),
        "launch\n"
    );
}

#[tokio::test]
async fn one_off_archive_publication_fences_and_clears_an_interrupted_claim_without_new_work() {
    let scratch = Scratch::new();
    install(&scratch, 0.1);
    let api = Api::start(1, true).await;
    let child = start(&scratch, &api, 3);
    archived(&scratch, 1).await;
    assert_eq!(stop(child).await["drained"], true);
    let mut saved = scratch.read();
    saved["active_session_id"] = json!(Uuid::new_v4());
    saved["pending_claim_key"] = json!(Uuid::new_v4());
    fs::write(
        scratch.journal().join("journal.json"),
        serde_json::to_vec(&saved).unwrap(),
    )
    .unwrap();
    let before = api.state.lock().unwrap().claim_calls.len();
    let output = support::run(
        support::command()
            .arg("publish-log-archives")
            .arg("--journal")
            .arg(scratch.journal())
            .arg("--token-file")
            .arg(scratch.0.join("token"))
            .arg("--confirm-prior-process-stopped"),
    )
    .await;
    assert_eq!(
        support::successful_json(&output)["status"],
        "published_log_archives"
    );
    assert!(scratch.read()["pending_claim_key"].is_null());
    assert!(scratch.read()["active_session_id"].is_null());
    assert_eq!(api.state.lock().unwrap().claim_calls.len(), before);
    assert_eq!(
        fs::read_to_string(scratch.0.join("launches")).unwrap(),
        "launch\n"
    );
}

#[tokio::test]
async fn server_terminal_receipt_cannot_settle_delivery_with_missing_local_execution() {
    let scratch = Scratch::new();
    install(&scratch, 0.1);
    let api = Api::start(1, true).await;
    api.state.lock().unwrap().hold_delivery = true;
    let mut child = start(&scratch, &api, 1);
    api.until(|peer| peer.delivery_calls > 0).await;
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    let mut saved = scratch.read();
    saved["prepared_assignments"][0]["execution"] = Value::Null;
    saved["prepared_assignments"][0]["transfers"] = Value::Null;
    fs::write(
        scratch.journal().join("journal.json"),
        serde_json::to_vec(&saved).unwrap(),
    )
    .unwrap();
    api.state.lock().unwrap().hold_delivery = false;
    let before = api.state.lock().unwrap().delivery_calls;
    let child = start(&scratch, &api, 1);
    api.until(|peer| peer.recoveries > 0 && peer.heartbeat > 1)
        .await;
    assert_eq!(stop(child).await["drained"], false);
    {
        let peer = api.state.lock().unwrap();
        assert_eq!(peer.delivery_calls, before);
        assert!(
            peer.jobs
                .values()
                .all(|job| job.delivery_completion.is_none())
        );
    }
    assert!(
        fs::read_to_string(scratch.0.join("service.log"))
            .unwrap()
            .contains("missing_local_execution_evidence")
    );
    let inspected = support::successful_json(
        &support::run(
            support::command()
                .arg("inspect")
                .arg("--journal")
                .arg(scratch.journal()),
        )
        .await,
    );
    assert_eq!(
        inspected["prepared_assignments"][0]["delivery_blocker"],
        "missing_local_execution_evidence"
    );
    assert!(
        inspected["prepared_assignments"][0]["delivery_next_action"]
            .as_str()
            .unwrap()
            .contains("audited operator evidence")
    );
}

#[tokio::test]
async fn maintenance_finishes_assigned_prelaunch_work_and_retains_queued_work_for_resume() {
    let scratch = Scratch::new();
    install(&scratch, 0.1);
    let api = Api::start(2, true).await;
    {
        let mut peer = api.state.lock().unwrap();
        peer.bulk_input = true;
        peer.hold_input = true;
    }
    let child = start(&scratch, &api, 3);
    api.until(|peer| peer.input_started > 0).await;
    {
        let mut peer = api.state.lock().unwrap();
        peer.maintenance = true;
        peer.hold_input = false;
    }
    archived(&scratch, 1).await;
    assert_eq!(api.state.lock().unwrap().remaining, 1);
    api.state.lock().unwrap().maintenance = false;
    archived(&scratch, 2).await;
    assert_eq!(stop(child).await["drained"], true);
}

#[tokio::test]
async fn maintenance_with_unavailable_configuration_finishes_prelaunch_work_from_its_pinned_installation()
 {
    let scratch = Scratch::new();
    install(&scratch, 0.1);
    let api = Api::start(1, true).await;
    {
        let mut peer = api.state.lock().unwrap();
        peer.bulk_input = true;
        peer.hold_input = true;
    }
    let child = start(&scratch, &api, 1);
    api.until(|peer| peer.input_started > 0).await;
    assert_eq!(stop(child).await["drained"], false);
    let retained = scratch.read();
    assert!(retained["prepared_assignments"][0]["transfers"]["installation"].is_object());
    assert_eq!(
        retained["prepared_assignments"][0]["submission_state"],
        "definitely_unsubmitted"
    );
    {
        let mut peer = api.state.lock().unwrap();
        peer.maintenance = true;
        peer.configuration_unavailable = true;
        peer.hold_input = false;
    }
    let child = start(&scratch, &api, 3);
    archived(&scratch, 1).await;
    assert_eq!(stop(child).await["drained"], true);
    assert!(
        fs::read_to_string(scratch.0.join("service.log"))
            .unwrap()
            .contains("configuration_unavailable")
    );
    assert_eq!(
        fs::read_to_string(scratch.0.join("launches")).unwrap(),
        "launch\n"
    );
}

#[tokio::test]
async fn withdrawal_blocks_assigned_prelaunch_work_until_authority_is_restored() {
    let scratch = Scratch::new();
    install(&scratch, 0.1);
    let api = Api::start(1, true).await;
    {
        let mut peer = api.state.lock().unwrap();
        peer.bulk_input = true;
        peer.hold_input = true;
    }
    let child = start(&scratch, &api, 3);
    api.until(|peer| peer.input_started > 0).await;
    {
        let mut peer = api.state.lock().unwrap();
        peer.authority_withdrawn = true;
        peer.hold_input = false;
    }
    api.until(|_| {
        fs::read_to_string(scratch.0.join("service.log"))
            .unwrap_or_default()
            .contains("authorization revoked before launch")
    })
    .await;
    assert!(!scratch.0.join("launches").exists());
    api.state.lock().unwrap().authority_withdrawn = false;
    archived(&scratch, 1).await;
    assert_eq!(stop(child).await["drained"], true);
    assert_eq!(
        fs::read_to_string(scratch.0.join("launches")).unwrap(),
        "launch\n"
    );
}

#[tokio::test]
async fn slow_archives_yield_required_work_and_resume_after_restart() {
    let scratch = Scratch::new();
    install(&scratch, 0.2);
    fs::write(
        scratch.0.join("log-policy.json"),
        r#"{"archive_enabled":true}"#,
    )
    .unwrap();
    let api = Api::start(1, true).await;
    {
        let mut peer = api.state.lock().unwrap();
        peer.transfer_workers = Some(1);
        peer.hold_archive = true;
    }
    let mut child = start(&scratch, &api, 1);
    api.until(|peer| peer.archive_started > 0).await;
    assert!(
        api.state
            .lock()
            .unwrap()
            .jobs
            .values()
            .all(|job| job.result.is_some())
    );
    let first = scratch.read();
    assert!(first["prepared_assignments"][0]["terminal_result"].is_object());
    assert_eq!(
        first["prepared_assignments"][0]["log_archive"]["finished"],
        false
    );
    api.state.lock().unwrap().remaining = 1;
    // The sole transfer slot is held by an optional upload. Required runner
    // preparation must preempt it so this next scientific job can finish.
    api.until(|peer| peer.jobs.len() == 2 && peer.jobs.values().all(|job| job.result.is_some()))
        .await;
    let heartbeat = api.state.lock().unwrap().heartbeat;
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    api.state.lock().unwrap().hold_archive = false;
    let child = start(&scratch, &api, 3);
    archived(&scratch, 2).await;
    stop(child).await;
    let peer = api.state.lock().unwrap();
    assert!(peer.heartbeat >= heartbeat);
    assert_eq!(peer.jobs.len(), 2);
    assert!(
        peer.archives
            .values()
            .all(|archive| archive["state"] == "ready")
    );
    assert_eq!(
        fs::read_to_string(scratch.0.join("launches"))
            .unwrap()
            .lines()
            .count(),
        2
    );
}
impl Drop for Api {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Api {
    async fn start(remaining: usize, local: bool) -> Self {
        let state = Arc::new(Mutex::new(Peer {
            remaining,
            allowed_outstanding: 1,
            local,
            ..Default::default()
        }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        state.lock().unwrap().origin = origin.clone();
        let router = Router::new().fallback(handle).with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            origin,
            state,
            task,
        }
    }
    async fn until(&self, check: impl Fn(&Peer) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(50);
        loop {
            if check(&self.state.lock().unwrap()) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "service did not reach expected state: {}",
                self.state
                    .lock()
                    .unwrap()
                    .installation_root
                    .as_ref()
                    .and_then(|root| fs::read_to_string(root.join("service.log")).ok())
                    .unwrap_or_default()
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}
fn receipt(job: &Job, report: &Value) -> Value {
    json!({"assignment_id":job.assignment["assignment_id"],"run_id":job.assignment["run_id"],"attempt_id":job.assignment["attempt_id"],"outcome":report["outcome"],"completed_at":job.completed_at,"provider_cleanup_acknowledged":job.cleanup_authority})
}
async fn handle(State(state): State<Arc<Mutex<Peer>>>, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    let cursor = request.uri().query().and_then(|query| {
        url::form_urlencoded::parse(query.as_bytes())
            .find(|(key, _)| key == "cursor")
            .map(|(_, value)| value.into_owned())
    });
    let method = request.method().to_string();
    if path == "/connector/v1/configuration" {
        let peer = state.lock().unwrap();
        if peer.configuration_unavailable {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        return Json(support::desired_configuration(
            peer.installation_root.as_ref().unwrap(),
        ))
        .into_response();
    }
    if path == "/connector/v1/configuration/1/retention-dispositions" {
        let body: Value =
            serde_json::from_slice(&to_bytes(request.into_body(), 256 * 1024).await.unwrap())
                .unwrap();
        return Json(json!({"generation":1,"configuration_changed":false,"dispositions":body["bindings"].as_array().unwrap().iter().map(|binding| json!({"context_id":binding["context_id"],"provider_binding_id":binding["provider_binding_id"],"state":"unmanaged"})).collect::<Vec<_>>()})).into_response();
    }
    if path == "/connector/v1/configuration/1/ack" {
        let body: Value =
            serde_json::from_slice(&to_bytes(request.into_body(), 256 * 1024).await.unwrap())
                .unwrap();
        assert!(body["validations"].is_array());
        {
            let mut peer = state.lock().unwrap();
            peer.configuration_acks += 1;
            peer.validations
                .extend(body["validations"].as_array().unwrap().iter().cloned());
        }
        while state.lock().unwrap().hold_configuration_ack {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        return Json(json!({"generation":1,"acknowledged":true})).into_response();
    }
    if path == "/blob/archive" {
        let bytes = to_bytes(request.into_body(), 50 * 1024 * 1024)
            .await
            .unwrap();
        assert!(bytes.starts_with(&[31, 139]));
        state.lock().unwrap().archive_started += 1;
        while state.lock().unwrap().hold_archive {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        return (StatusCode::OK, [("x-amz-version-id", "fixture-archive-v1")]).into_response();
    }
    if path == "/blob/input" || path == "/blob/output" {
        let input = path.ends_with("input");
        let range = request
            .headers()
            .get("range")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        {
            let mut peer = state.lock().unwrap();
            if input {
                peer.input_started += 1;
            } else {
                peer.output_started += 1;
            }
        }
        if !input {
            assert_eq!(
                to_bytes(request.into_body(), 1024).await.unwrap().as_ref(),
                b"abc"
            );
        }
        loop {
            let held = {
                let peer = state.lock().unwrap();
                if input {
                    peer.hold_input
                } else {
                    peer.hold_output
                }
            };
            if !held {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        if input {
            assert_eq!(range.as_deref(), Some("bytes=0-2"));
            return (
                StatusCode::PARTIAL_CONTENT,
                [
                    ("x-amz-version-id", "fixture-v1"),
                    ("content-range", "bytes 0-2/3"),
                ],
                "abc",
            )
                .into_response();
        }
        return ([("x-amz-version-id", "fixture-v1")], "").into_response();
    }
    let headers = request.headers().clone();
    assert_eq!(headers["authorization"], format!("Bearer {TOKEN}"));
    let bytes = to_bytes(request.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    let mut peer = state.lock().unwrap();
    if peer.unavailable {
        if path == "/connector/v1/heartbeat" {
            peer.unavailable_heartbeats += 1;
        }
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if path == "/connector/v1/heartbeat" {
        peer.heartbeat += 1;
        return Json(json!({"connector_id":CONNECTOR,"received_at":"2026-09-29T12:00:00Z","heartbeat_interval_seconds":1,"stale_after_seconds":60})).into_response();
    }
    if path == "/connector/v1/sessions" || path == "/connector/v1/sessions/recover" {
        if path.ends_with("recover") {
            peer.recoveries += 1;
        } else {
            peer.sessions += 1;
        }
        peer.active_session = Some(body["session_id"].clone());
        peer.journal_id = body["journal_id"].clone();
        return Json(json!({"connector_id":CONNECTOR,"journal_id":body["journal_id"],"session_id":body["session_id"]})).into_response();
    }
    if method == "DELETE" {
        peer.active_session = None;
        return StatusCode::NO_CONTENT.into_response();
    }
    if path == "/connector/v1/assignments" {
        if method == "GET" {
            let mut items: Vec<_> = peer.jobs.iter().filter(|(id, job)| job.delivery_completion.is_none()
                && cursor.as_ref().is_none_or(|after| id.to_string() > *after)).map(|(_, job)| json!({
                "assignment_id":job.assignment["assignment_id"],"run_id":job.assignment["run_id"],"attempt_id":job.assignment["attempt_id"],
                "journal_id":job.assignment["journal_id"],"state":job.result.as_ref().map_or(json!("running"), |result|result["receipt"]["outcome"].clone()),
                "phase":if job.result.is_some(){Value::Null}else{json!("preparing")},"created_at":job.assignment["created_at"]
            })).collect();
            let next = (items.len() > 1).then(|| items[0]["assignment_id"].clone());
            items.truncate(1);
            return Json(json!({"items":items,"next_cursor":next})).into_response();
        }
        let key = headers["idempotency-key"].to_str().unwrap().to_owned();
        peer.claim_calls.push(key.clone());
        let id = if let Some(id) = peer.claims.get(&key) {
            *id
        } else {
            if peer.maintenance
                || peer.authority_withdrawn
                || peer.remaining == 0
                || peer.jobs.values().filter(|j| j.result.is_none()).count()
                    >= peer.allowed_outstanding
            {
                return StatusCode::NO_CONTENT.into_response();
            }
            peer.remaining -= 1;
            let local = peer.local;
            let mut deployment: Value = serde_json::from_str(if local {
                include_str!("../../../examples/local-scalar-requirements.json")
            } else {
                include_str!("../../../examples/test-requirements.json")
            })
            .unwrap();
            if peer.slurm {
                deployment["driver"] = json!("slurm");
            }
            let binding = peer.assignment_descriptor.clone().unwrap_or_else(|| {
                support::desired_configuration(peer.installation_root.as_ref().unwrap())
                    ["publications"][0]["descriptor"]
                    .clone()
            });
            let model: Value = serde_json::to_value(
                enumeray_contracts::models::parse_publication(if local {
                    include_bytes!("../../../examples/local-scalar-model.json")
                } else {
                    include_bytes!("../../../examples/scalar-double.json")
                })
                .unwrap(),
            )
            .unwrap();
            let id = Uuid::new_v4();
            let mut assignment = json!({"assignment_id":id,"run_id":Uuid::new_v4(),"attempt_id":Uuid::new_v4(),
                "connector_id":CONNECTOR,"journal_id":peer.journal_id,"model_id":"scalar-double","publication_id":json!("scalar-double-publication"),
                "contract_version":1,"publication_id":"test-publication","context_id":"test-context","descriptor":binding,
                "requirements":deployment,"definition":model["definition"],"inputs":{"value":2},"created_at":"2026-09-29T12:00:00Z"});
            if peer.bulk_input || peer.bulk_output {
                if peer.jobs.is_empty() {
                    if peer.bulk_input {
                        let schema: Value = serde_json::to_value(
                            enumeray_contracts::models::parse_publication(include_bytes!(
                                "../../../examples/artifact-input.json"
                            ))
                            .unwrap(),
                        )
                        .unwrap();
                        assignment["definition"]["input_schema"]["properties"]["dataset"] =
                            schema["definition"]["input_schema"]["properties"]["dataset"].clone();
                        assignment["definition"]["input_schema"]["required"] =
                            json!(["value", "dataset"]);
                        assignment["inputs"]["dataset"] =
                            json!({"artifact_id":Uuid::from_u128(123)});
                    }
                    if peer.bulk_output {
                        let model: Value = serde_json::to_value(
                            enumeray_contracts::models::parse_publication(include_bytes!(
                                "../../../examples/artifact-output.json"
                            ))
                            .unwrap(),
                        )
                        .unwrap();
                        assignment["definition"]["output_schema"] =
                            model["definition"]["output_schema"].clone();
                        if peer.large_output {
                            assignment["definition"]["output_schema"]["properties"]["details"] =
                                json!({"type":"string","maxLength":61440});
                            assignment["definition"]["output_schema"]["required"] =
                                json!(["result", "details"]);
                        }
                    }
                } else {
                    assignment["inputs"]["value"] = json!(3);
                }
            }
            assignment["requirements"] = serde_json::to_value(
                enumeray_contracts::execution::ExecutionRequirements::derive(
                    if peer.slurm {
                        enumeray_contracts::execution::ExecutionDriver::Slurm
                    } else {
                        enumeray_contracts::execution::ExecutionDriver::Process
                    },
                    &serde_json::from_value(assignment["definition"].clone()).unwrap(),
                ),
            )
            .unwrap();
            let cleanup_authority = peer.cleanup_enabled;
            peer.jobs.insert(
                id,
                Job {
                    cleanup_authority,
                    completed_at: if cleanup_authority {
                        time::OffsetDateTime::now_utc()
                            .format(&time::format_description::well_known::Rfc3339)
                            .unwrap()
                    } else {
                        "2026-09-29T12:00:00Z".into()
                    },
                    assignment,
                    result: None,
                    delivery_completion: None,
                    resolution: None,
                },
            );
            peer.claims.insert(key, id);
            let active = peer
                .jobs
                .values()
                .filter(|job| job.result.is_none())
                .count();
            assert!(
                active <= peer.allowed_outstanding,
                "service exceeded configured concurrency"
            );
            peer.maximum_observed = peer.maximum_observed.max(active);
            id
        };
        if peer.lose_claim {
            peer.lose_claim = false;
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        return (StatusCode::CREATED, Json(peer.jobs[&id].assignment.clone())).into_response();
    }
    let parts: Vec<_> = path.split('/').collect();
    if path.ends_with("/control") {
        let id: Uuid = parts[4].parse().unwrap();
        let cancelled = peer.cancel_requested || peer.cancel_assignment == Some(id);
        if cancelled {
            peer.cancellation_controls += 1;
        }
        return Json(json!({"allow_launch":!cancelled && !peer.authority_withdrawn,"cancel_pending":peer.authority_withdrawn,"cancel_running":cancelled,"reason":null})).into_response();
    }
    let id: Uuid = parts[4].parse().unwrap();
    if parts.get(5) == Some(&"delivery") {
        peer.delivery_calls += 1;
        if peer.hold_delivery {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        let archive = peer.archives.get(&id);
        assert!(peer.jobs[&id].result.is_some());
        assert_eq!(body["archive_expected"], json!(archive.is_some()));
        assert!(archive.is_none_or(|archive| matches!(
            archive["state"].as_str(),
            Some("ready" | "unavailable" | "expired")
        )));
        let job = peer.jobs.get_mut(&id).unwrap();
        let receipt = job.delivery_completion.get_or_insert_with(|| json!({"assignment_id":id,"run_id":job.assignment["run_id"],"attempt_id":job.assignment["attempt_id"],"source":"connector","completed_at":"2026-10-15T12:00:00Z"})).clone();
        if peer.lose_delivery {
            peer.lose_delivery = false;
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        return Json(receipt).into_response();
    }
    if parts[5..].first() == Some(&"log-archive") {
        if parts.len() == 6 {
            if peer.unavailable_archive_intent {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            let status = peer.archives.entry(id).or_insert_with(|| json!({"state":"capturing","artifact_id":null,"manifest":null,"expires_at":null,"diagnostic":null}));
            if body["finalized_at"].is_string() && status["state"] == "capturing" {
                status["state"] = json!("pending");
            }
            return Json(status.clone()).into_response();
        }
        if path.ends_with("/completion") {
            assert_eq!(body["version_id"], "fixture-archive-v1");
            peer.archives.get_mut(&id).unwrap()["state"] = json!("ready");
            return Json(json!({})).into_response();
        }
        assert!(
            peer.jobs[&id].result.is_some(),
            "archive upload preceded scientific completion"
        );
        let key = headers["idempotency-key"].to_str().unwrap();
        let origin = peer.origin.clone();
        let status = peer.archives.get_mut(&id).unwrap();
        status["artifact_id"] = json!(key);
        status["manifest"] = body["scientific_metadata"]["log_archive_manifest"].clone();
        return (StatusCode::CREATED,Json(json!({"reservation":{"upload_id":key,"artifact_id":key},"state":"pending",
            "transfer":{"status":"available","request":{"url":format!("{origin}/blob/archive"),"method":"PUT","headers":[]}}}))).into_response();
    }
    if parts[5..].first() == Some(&"artifacts") {
        if path.ends_with("/content") {
            return (
                StatusCode::SEE_OTHER,
                [("location", format!("{}/blob/input", peer.origin))],
            )
                .into_response();
        }
        return Json(json!({"content":{"size_bytes":3,"version_id":"fixture-v1","checksum":{"value":"ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0="}}})).into_response();
    }
    if parts[5..].first() == Some(&"uploads") {
        if path.ends_with("/completion") {
            assert_eq!(body["version_id"], "fixture-v1");
            peer.output_verified = true;
            if peer.lose_verification {
                peer.lose_verification = false;
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            return Json(json!({})).into_response();
        }
        assert!(
            peer.events
                .iter()
                .any(|(path, event)| path.contains(&id.to_string())
                    && event["phase"] == "collecting_results")
        );
        let key = headers["idempotency-key"].to_str().unwrap();
        peer.reservation_keys.push(key.to_owned());
        if peer.expire_reservation {
            peer.expire_reservation = false;
            return (StatusCode::CREATED,Json(json!({"reservation":{"upload_id":key,"artifact_id":key},"state":"expired","transfer":{"status":"expired"}}))).into_response();
        }
        return (StatusCode::CREATED,Json(json!({"reservation":{"upload_id":key,"artifact_id":key},"state":if peer.output_verified {"completed"}else{"pending"},"transfer":{"status":"available","request":{"url":format!("{}/blob/output",peer.origin),"method":"PUT","headers":[]}}}))).into_response();
    }
    if parts.len() == 5 {
        let job = &peer.jobs[&id];
        return Json(json!({"assignment":job.assignment,"state":job.result.as_ref().map_or(json!("running"), |r| r["receipt"]["outcome"].clone()),
            "phase":if job.result.is_some(){Value::Null}else{json!("preparing")},"result":job.result,"delivery_completion":job.delivery_completion,
            "last_event_sequence":null,"backend_handle":null,"last_observed_at":null,"observation_freshness":"unknown","resolution":job.resolution})).into_response();
    }
    if parts[5] == "logs" {
        if let Some(old) = peer.logs.insert(path.clone(), body.clone()) {
            assert_eq!(old, body);
        }
        if peer.lose_log {
            peer.lose_log = false;
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        return Json(json!({"assignment_id":id,"source":parts[6],"epoch":1,"sequence":parts[8].parse::<u64>().unwrap(),"position":1,"received_at":"2026-09-29T12:00:00Z"})).into_response();
    }
    if parts[5] == "events" {
        let event: enumeray_contracts::assignments::observations::EventRequest =
            serde_json::from_value(body.clone()).unwrap();
        event.normalize().unwrap();
        if let Some(old) = peer.events.insert(path.clone(), body.clone()) {
            assert_eq!(old, body);
        }
        if peer.lose_event {
            peer.lose_event = false;
            peer.unavailable = true;
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        return Json(json!({"assignment_id":id,"event_id":parts[6],"sequence":body["sequence"],"phase":body["phase"],"disposition":"applied"})).into_response();
    }
    let job = peer.jobs.get_mut(&id).unwrap();
    let receipt = receipt(job, &body);
    let result = json!({"receipt":receipt,"outputs":body["outputs"],"files":body["files"],"optional_output_failures":body["optional_output_failures"],"failure":body["failure"]});
    if let Some(old) = &job.result {
        assert_eq!(old, &result);
    }
    job.result = Some(result);
    if peer.lose_report {
        peer.lose_report = false;
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    Json(receipt).into_response()
}
fn start(scratch: &Scratch, api: &Api, grace: u64) -> Child {
    start_with_policy(scratch, api, grace, false)
}

fn start_with_policy(scratch: &Scratch, api: &Api, grace: u64, allow_development: bool) -> Child {
    scratch.token(TOKEN);
    if !scratch.0.join("installation-fixture.json").exists() {
        support::install_scalar(scratch);
    }
    api.state.lock().unwrap().installation_root = Some(scratch.0.clone());
    let stderr = fs::File::create(scratch.0.join("service.log")).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_enumeray"));
    command
        .arg("--config")
        .arg(scratch.0.join("installation-fixture.json"))
        .env(
            "ENUMERAY_TOKEN",
            "user-profile-token-must-not-reach-connector",
        )
        .arg("connector");
    command
        .args([
            "run",
            "--api-base-url",
            &api.origin,
            "--connector-id",
            CONNECTOR,
            "--journal",
        ])
        .arg(scratch.journal())
        .arg("--token-file")
        .arg(scratch.0.join("token"))
        .args([
            "--cleanup-interval-seconds",
            "1",
            "--poll-interval-seconds",
            "1",
            "--recovery-mode",
            "exclusive-local",
            "--shutdown-timeout-seconds",
            &grace.to_string(),
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(stderr)
        .kill_on_drop(true);
    if scratch.0.join("log-policy.json").exists() {
        command
            .arg("--log-policy-file")
            .arg(scratch.0.join("log-policy.json"));
    }
    command.arg("--max-workers").arg({
        let peer = api.state.lock().unwrap();
        peer.local_workers
            .unwrap_or(peer.allowed_outstanding)
            .to_string()
    });
    {
        let peer = api.state.lock().unwrap();
        if let Some(seconds) = peer.completed_retention {
            command
                .arg("--completed-retention-seconds")
                .arg(seconds.to_string());
        }
        if let Some(seconds) = peer.retention {
            command
                .arg("--unpublished-retention-seconds")
                .arg(seconds.to_string());
        }
        if let Some(workers) = peer.transfer_workers {
            command
                .arg("--max-transfer-workers")
                .arg(workers.to_string());
        }
        if let Some(cap) = peer.pending_cap {
            command
                .arg("--max-pending-assignments")
                .arg(cap.to_string());
        }
    }
    if allow_development {
        command.arg("--allow-development-execution");
    }
    command.spawn().unwrap()
}
async fn stop(child: Child) -> Value {
    request_stop(&child).await;
    stopped(child, Duration::from_secs(8)).await
}
async fn request_stop(child: &Child) {
    assert!(
        Command::new("/bin/kill")
            .args(["-TERM", &child.id().unwrap().to_string()])
            .status()
            .await
            .unwrap()
            .success()
    );
}
async fn stopped(child: Child, timeout: Duration) -> Value {
    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    support::successful_json(&output)
}
async fn service_status(scratch: &Scratch, status: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let log = fs::read_to_string(scratch.0.join("service.log")).unwrap_or_default();
        if log.lines().any(|line| {
            serde_json::from_str::<Value>(line).is_ok_and(|entry| entry["status"] == status)
        }) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "service did not reach {status}: {log}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
fn install(scratch: &Scratch, delay: f64) {
    install_wait(scratch, &format!("time.sleep({delay})"));
}
fn install_gated(scratch: &Scratch) -> PathBuf {
    let gate = scratch.0.join("model-gate");
    fs::write(&gate, "wait").unwrap();
    install_wait(
        scratch,
        &format!("while Path({gate:?}).exists(): time.sleep(0.025)"),
    );
    gate
}
fn install_wait(scratch: &Scratch, wait: &str) {
    let reference = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/scalar_model")
        .canonicalize()
        .unwrap();
    let wrapper = scratch.0.join("slow.py");
    fs::write(&wrapper, format!("import json,time,runpy,sys\nfrom pathlib import Path\nr=json.loads(Path(sys.argv[sys.argv.index('--request')+1]).read_text())\nwith open({:?},'a') as f: f.write('launch\\n')\nPath({:?}+'-'+r['attempt_id']).write_text('started')\nprint('Service model started.',flush=True)\n{wait}\nsys.path.insert(0,{:?})\nrunpy.run_path({:?},run_name='__main__')\n", scratch.0.join("launches"), scratch.0.join("wrapper-started").to_str().unwrap(), reference, reference.join("wrapper.py"))).unwrap();
    let mut binding: Value = serde_json::from_str(include_str!(
        "../../../examples/local-scalar-installation.json"
    ))
    .unwrap();
    binding["argv"] = json!(["/usr/bin/python3", wrapper, "execute"]);
    binding["environment"] = json!({});
    support::write_installation_fixture(scratch, binding.clone());
}
async fn archived(scratch: &Scratch, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(15 + 4 * count as u64);
    loop {
        if fs::read_dir(scratch.journal().join("archive")).is_ok_and(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
                .count()
                == count
        }) && scratch.read()["prepared_assignments"]
            .as_array()
            .unwrap()
            .is_empty()
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{}",
            fs::read_to_string(scratch.0.join("service.log")).unwrap_or_default()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn sequential_service_reuses_bounded_journal_across_lost_receipts_and_archive_interruption() {
    let scratch = Scratch::new();
    let api = Api::start(4, false).await;
    {
        let mut peer = api.state.lock().unwrap();
        peer.lose_claim = true;
        peer.lose_report = true;
        // Cross the configured pending bound; the journal unit test exercises
        // the full 64-entry limit without launching dozens of real processes.
        peer.pending_cap = Some(2);
    }
    let child = start(&scratch, &api, 3);
    archived(&scratch, 4).await;
    api.until(|p| p.heartbeat >= 2).await;
    assert_eq!(stop(child).await["drained"], true);
    {
        let peer = api.state.lock().unwrap();
        assert_eq!(peer.jobs.len(), 4);
        assert_eq!(peer.claim_calls[0], peer.claim_calls[1]);
        assert_eq!(peer.sessions, 1);
    }
    // Recreate the crash window after an archive was synced but before removal.
    let path = fs::read_dir(scratch.journal().join("archive"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let archive: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(archive["version"], 1);
    let mut data = scratch.read();
    data["prepared_assignments"] = json!([archive["entry"]]);
    fs::write(
        scratch.journal().join("journal.json"),
        serde_json::to_vec(&data).unwrap(),
    )
    .unwrap();
    let child = start(&scratch, &api, 3);
    archived(&scratch, 4).await;
    stop(child).await;
    assert!(
        !fs::read_to_string(scratch.0.join("service.log"))
            .unwrap()
            .contains(TOKEN)
    );
}

#[tokio::test]
async fn completed_payload_cleanup_resumes_archives_without_changing_receipts_or_launching_again() {
    let scratch = Scratch::new();
    install(&scratch, 0.0);
    let api = Api::start(4, false).await;
    {
        let mut peer = api.state.lock().unwrap();
        peer.cleanup_enabled = true;
        peer.completed_retention = Some(3600);
        peer.pending_cap = Some(2);
        peer.lose_claim = true;
        peer.lose_report = true;
    }
    let child = start(&scratch, &api, 3);
    archived(&scratch, 4).await;
    stop(child).await;
    let archives: Vec<_> = fs::read_dir(scratch.journal().join("archive"))
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let bytes = fs::read(&path).unwrap();
            let archive: Value = serde_json::from_slice(&bytes).unwrap();
            let intent = archive["entry"]["launch_intent_id"]
                .as_str()
                .unwrap()
                .to_owned();
            (path, bytes, intent)
        })
        .collect();
    let checkpoint = scratch.journal().join("cleanup/scan.json");
    let prior_checkpoint = fs::read(&checkpoint).ok();
    let output = Command::new(env!("CARGO_BIN_EXE_enumeray"))
        .arg("connector")
        .args(["cleanup", "--journal"])
        .arg(scratch.journal())
        .args(["--dry-run", "--limit", "8"])
        .output()
        .await
        .unwrap();
    let dry = support::successful_json(&output);
    assert!(
        dry["runs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|run| run["state"] == "eligible")
    );
    assert_eq!(fs::read(&checkpoint).ok(), prior_checkpoint);
    for (_, _, intent) in &archives {
        let connection: Value = serde_json::from_slice(
            &fs::read(
                scratch
                    .journal()
                    .join("compute-jobs")
                    .join(intent)
                    .join("connection.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(
            PathBuf::from(connection["work_root"].as_str().unwrap())
                .join(intent)
                .join("workspace")
                .exists()
        );
    }
    api.state.lock().unwrap().completed_retention = Some(0);
    let child = start(&scratch, &api, 3);
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        let complete = archives.iter().all(|(path, _, _)| {
            let id = path.file_stem().unwrap();
            fs::read(
                scratch
                    .journal()
                    .join("cleanup")
                    .join(format!("{}.json", id.to_string_lossy())),
            )
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .is_some_and(|s| s["compute_complete"] == true && s["connector_complete"] == true)
        });
        if complete {
            break;
        }
        assert!(
            Instant::now() < until,
            "cleanup failed: {}",
            fs::read_to_string(scratch.0.join("service.log")).unwrap()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    stop(child).await;
    for (path, bytes, intent) in &archives {
        assert_eq!(&fs::read(path).unwrap(), bytes);
        let local = scratch.journal().join("local-jobs").join(intent);
        assert!(
            scratch
                .journal()
                .join("compute-jobs")
                .join(intent)
                .join("job.json")
                .exists()
        );
        assert!(local.join("log-policy.json").exists());
        assert!(!local.join("workspace").exists());
        assert!(
            !scratch
                .journal()
                .join("input-staging")
                .join(intent)
                .exists()
        );
        let connection: Value = serde_json::from_slice(
            &fs::read(
                scratch
                    .journal()
                    .join("compute-jobs")
                    .join(intent)
                    .join("connection.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let remote = PathBuf::from(connection["work_root"].as_str().unwrap()).join(intent);
        assert!(!remote.join("workspace").exists());
        assert!(remote.join("completion.json").exists());
        let mut replay = Command::new(env!("CARGO_BIN_EXE_enumeray"))
            .arg("connector")
            .args(["compute-runner", "start", "--directory"])
            .arg(remote)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use tokio::io::AsyncWriteExt;
        replay
            .stdin
            .take()
            .unwrap()
            .write_all(br#"{"version":1,"action":"start","payload":null}"#)
            .await
            .unwrap();
        let _ = replay.wait_with_output().await.unwrap();
    }
    assert_eq!(
        fs::read_to_string(scratch.0.join("launches"))
            .unwrap()
            .lines()
            .count(),
        4
    );
    assert_eq!(api.state.lock().unwrap().jobs.len(), 4);
}

#[tokio::test]
async fn long_local_execution_streams_logs_and_heartbeats_across_api_outage() {
    let scratch = Scratch::new();
    // Keep the model running through observed outage/recovery instead of
    // waiting for a fixed scientific execution duration.
    let gate = install_gated(&scratch);
    let api = Api::start(1, true).await;
    api.state.lock().unwrap().lose_log = true;
    let child = start(&scratch, &api, 3);
    api.until(|p| !p.logs.is_empty() && !p.events.is_empty())
        .await;
    let beats = api.state.lock().unwrap().heartbeat;
    api.state.lock().unwrap().unavailable = true;
    api.until(|p| p.unavailable_heartbeats > 0).await;
    api.state.lock().unwrap().unavailable = false;
    api.until(|p| p.heartbeat >= beats + 2).await;
    assert!(
        api.state
            .lock()
            .unwrap()
            .jobs
            .values()
            .all(|j| j.result.is_none())
    );
    fs::remove_file(gate).unwrap();
    api.until(|p| p.jobs.values().any(|j| j.result.is_some()))
        .await;
    archived(&scratch, 1).await;
    stop(child).await;
    assert_eq!(
        fs::read_to_string(scratch.0.join("launches")).unwrap(),
        "launch\n"
    );
    assert_eq!(
        api.state
            .lock()
            .unwrap()
            .jobs
            .values()
            .next()
            .unwrap()
            .result
            .as_ref()
            .unwrap()["outputs"]["doubled"],
        4
    );
}

#[tokio::test]
async fn shutdown_deadline_and_restart_resume_same_supervisor_and_exclude_second_owner() {
    let scratch = Scratch::new();
    install(&scratch, 4.0);
    let api = Api::start(1, true).await;
    let child = start(&scratch, &api, 1);
    api.until(|p| !p.logs.is_empty()).await;
    let second = start(&scratch, &api, 1).wait_with_output().await.unwrap();
    assert!(!second.status.success());
    assert_eq!(stop(child).await["drained"], false);
    assert!(scratch.read()["active_session_id"].is_string());
    let child = start(&scratch, &api, 3);
    archived(&scratch, 1).await;
    stop(child).await;
    assert_eq!(
        fs::read_to_string(scratch.0.join("launches")).unwrap(),
        "launch\n"
    );
    assert_eq!(api.state.lock().unwrap().recoveries, 1);
}

#[tokio::test]
async fn shutdown_drains_active_model_and_idle_stop_is_prompt() {
    let scratch = Scratch::new();
    let gate = install_gated(&scratch);
    let api = Api::start(1, true).await;
    let child = start(&scratch, &api, 15);
    api.until(|p| !p.logs.is_empty()).await;
    request_stop(&child).await;
    service_status(&scratch, "draining").await;
    assert!(
        api.state
            .lock()
            .unwrap()
            .jobs
            .values()
            .all(|job| job.result.is_none())
    );
    fs::remove_file(gate).unwrap();
    assert_eq!(
        stopped(child, Duration::from_secs(20)).await["drained"],
        true
    );
    assert!(scratch.read()["active_session_id"].is_null());
    assert_eq!(
        fs::read_to_string(scratch.0.join("launches")).unwrap(),
        "launch\n"
    );
    let child = start(&scratch, &api, 3);
    service_status(&scratch, "idle").await;
    let now = Instant::now();
    assert_eq!(stop(child).await["drained"], true);
    assert!(now.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn uncertain_launch_waits_for_operator_resolution_and_delivery_abandonment_then_claims_only_new_work()
 {
    let scratch = Scratch::new();
    scratch.token(TOKEN);
    if !scratch.0.join("installation-fixture.json").exists() {
        support::install_scalar(&scratch);
    }
    let api = Api::start(2, false).await;
    api.state.lock().unwrap().installation_root = Some(scratch.0.clone());
    support::successful_json(
        &support::run(
            support::command()
                .args([
                    "init",
                    "--api-base-url",
                    &api.origin,
                    "--connector-id",
                    CONNECTOR,
                    "--journal",
                ])
                .arg(scratch.journal()),
        )
        .await,
    );
    support::successful_json(
        &support::run(
            support::command()
                .arg("claim-once")
                .arg("--journal")
                .arg(scratch.journal())
                .arg("--token-file")
                .arg(scratch.0.join("token")),
        )
        .await,
    );
    let mut data = scratch.read();
    data["prepared_assignments"][0]["submission_state"] = json!("unknown");
    let old_intent = data["prepared_assignments"][0]["launch_intent_id"].clone();
    fs::write(
        scratch.journal().join("journal.json"),
        serde_json::to_vec(&data).unwrap(),
    )
    .unwrap();
    let child = start(&scratch, &api, 3);
    api.until(|p| p.heartbeat >= 3).await;
    assert_eq!(api.state.lock().unwrap().jobs.len(), 1);
    assert!(!scratch.journal().join("compute-jobs").exists());
    {
        let mut peer = api.state.lock().unwrap();
        let job = peer.jobs.values_mut().next().unwrap();
        let mut receipt = receipt(job, &json!({"outcome":"failed"}));
        // Operator resolution retains its own five-field receipt and cannot
        // acknowledge provider payload cleanup.
        receipt
            .as_object_mut()
            .unwrap()
            .remove("provider_cleanup_acknowledged");
        job.resolution = Some(
            json!({"assignment_id":job.assignment["assignment_id"],"attempt_id":job.assignment["attempt_id"],"execution_status":"never_started","resolved_at":receipt["completed_at"]}),
        );
        job.result = Some(
            json!({"receipt":receipt,"outputs":{},"files":{},"optional_output_failures":{},"failure":{"code":"operator_resolved","message":"An operator confirmed execution termination and resolved this assignment."}}),
        );
        job.delivery_completion = Some(json!({"assignment_id":job.assignment["assignment_id"],
            "run_id":job.assignment["run_id"],"attempt_id":job.assignment["attempt_id"],
            "source":"operator_abandonment","completed_at":"2026-10-15T12:00:00Z"}));
    }
    api.until(|p| p.jobs.len() == 2 && p.jobs.values().all(|j| j.result.is_some()))
        .await;
    archived(&scratch, 2).await;
    stop(child).await;
    assert!(
        !scratch
            .journal()
            .join("compute-jobs")
            .join(old_intent.as_str().unwrap())
            .exists()
    );
}

#[tokio::test]
async fn manual_recovery_session_requires_confirmation_and_identity_mismatch_is_rejected() {
    let scratch = Scratch::new();
    scratch.token(TOKEN);
    if !scratch.0.join("installation-fixture.json").exists() {
        support::install_scalar(&scratch);
    }
    let api = Api::start(0, false).await;
    support::successful_json(
        &support::run(
            support::command()
                .args([
                    "init",
                    "--api-base-url",
                    &api.origin,
                    "--connector-id",
                    CONNECTOR,
                    "--journal",
                ])
                .arg(scratch.journal()),
        )
        .await,
    );
    let mut data = scratch.read();
    data["active_session_id"] = json!(Uuid::new_v4());
    fs::write(
        scratch.journal().join("journal.json"),
        serde_json::to_vec(&data).unwrap(),
    )
    .unwrap();
    let failed = start(&scratch, &api, 1).wait_with_output().await.unwrap();
    assert!(!failed.status.success());
    assert_eq!(scratch.read(), data);
    assert!(
        fs::read_to_string(scratch.0.join("service.log"))
            .unwrap()
            .contains("--confirm-prior-process-stopped")
    );
    data["active_session_id"] = Value::Null;
    data["connector_id"] = json!("another-connector");
    fs::write(
        scratch.journal().join("journal.json"),
        serde_json::to_vec(&data).unwrap(),
    )
    .unwrap();
    assert!(
        !start(&scratch, &api, 1)
            .wait_with_output()
            .await
            .unwrap()
            .status
            .success()
    );
    assert_eq!(api.state.lock().unwrap().sessions, 0);
}

#[tokio::test]
async fn bounded_assignments_execute_concurrently_with_one_journal_owner() {
    let scratch = Scratch::new();
    let gate = install_gated(&scratch);
    let api = Api::start(3, true).await;
    api.state.lock().unwrap().allowed_outstanding = 2;
    let child = start(&scratch, &api, 5);
    api.until(|peer| {
        peer.jobs.len() == 2
            && peer.jobs.values().all(|job| {
                scratch
                    .0
                    .join(format!(
                        "wrapper-started-{}",
                        job.assignment["attempt_id"].as_str().unwrap()
                    ))
                    .exists()
            })
    })
    .await;
    assert!(
        api.state
            .lock()
            .unwrap()
            .jobs
            .values()
            .all(|job| job.result.is_none())
    );
    fs::remove_file(gate).unwrap();
    api.until(|p| p.jobs.len() == 3 && p.jobs.values().all(|job| job.result.is_some()))
        .await;
    archived(&scratch, 3).await;
    assert_eq!(api.state.lock().unwrap().maximum_observed, 2);
    assert_eq!(
        fs::read_to_string(scratch.0.join("launches"))
            .unwrap()
            .lines()
            .count(),
        3
    );
    assert_eq!(stop(child).await["drained"], true);
}

#[tokio::test]
async fn connector_cancels_running_work_after_restart_and_lost_result_reply() {
    let scratch = Scratch::new();
    install(&scratch, 30.0);
    let api = Api::start(1, true).await;
    let mut child = start(&scratch, &api, 1);
    api.until(|p| !p.logs.is_empty()).await;
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    {
        let mut peer = api.state.lock().unwrap();
        peer.cancel_requested = true;
        peer.lose_report = true;
    }
    let child = start(&scratch, &api, 3);
    archived(&scratch, 1).await;
    stop(child).await;
    assert_eq!(
        fs::read_to_string(scratch.0.join("launches")).unwrap(),
        "launch\n"
    );
    let peer = api.state.lock().unwrap();
    let result = peer.jobs.values().next().unwrap().result.as_ref().unwrap();
    assert_eq!(result["receipt"]["outcome"], "cancelled");
    assert_eq!(result["failure"]["code"], "user_cancelled");
    assert!(!peer.logs.is_empty());
    assert_eq!(peer.recoveries, 1);
}

#[tokio::test]
async fn user_cancellation_before_launch_publishes_no_execution_progress() {
    let scratch = Scratch::new();
    install(&scratch, 30.0);
    let api = Api::start(1, true).await;
    {
        let mut peer = api.state.lock().unwrap();
        peer.cancel_requested = true;
        peer.lose_report = true;
    }
    let child = start(&scratch, &api, 3);
    archived(&scratch, 1).await;
    stop(child).await;
    assert!(!scratch.0.join("launches").exists());
    let peer = api.state.lock().unwrap();
    assert_eq!(
        peer.jobs.values().next().unwrap().result.as_ref().unwrap()["receipt"]["outcome"],
        "cancelled"
    );
    assert_eq!(peer.events.len(), 1);
    assert!(
        peer.events.values().all(
            |event| event["phase"].is_null() && event["diagnostic"]["code"] == "user_cancelled"
        )
    );
}

#[tokio::test]
async fn scheduler_failure_history_survives_lost_acknowledgements_restart_and_frees_capacity() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;
    for observed_execution in [false, true] {
        let scratch = Scratch::new();
        install(&scratch, 30.0);
        let scheduler = scratch.0.join("scheduler");
        fs::create_dir(&scheduler).unwrap();
        for name in ["sbatch", "squeue", "sacct", "scancel"] {
            let path = scheduler.join(name);
            fs::write(
                &path,
                include_str!("../../connector/tests/fixtures/cancellation-scheduler.py"),
            )
            .unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            #[cfg(target_os = "macos")]
            assert!(
                std::process::Command::new(&path)
                    .arg("--fixture-ready")
                    .status()
                    .unwrap()
                    .success()
            );
        }
        fs::write(scheduler.join("state"), "LAG").unwrap();
        fs::write(scheduler.join("mode"), "keep-pending").unwrap();
        fs::write(scheduler.join("reply"), "123;cluster-a").unwrap();
        let script = scratch.0.join("model.sbatch");
        fs::write(&script, "#!/bin/sh\nexit 23\n").unwrap();
        let config = scratch.0.join("installation-fixture.json");
        let mut binding: Value = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
        let installation = &mut binding["installations"][0];
        installation["driver"] =
            json!({"kind":"slurm", "worker_path":env!("CARGO_BIN_EXE_enumeray")});
        installation["slurm_script"] = json!(script);
        installation["installation"]["file_digests"] = json!({script.to_str().unwrap():STANDARD.encode(Sha256::digest(fs::read(&script).unwrap()))});
        let path = std::env::join_paths(std::iter::once(scheduler.clone()).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .unwrap();
        installation["connection"]["launcher"] = json!([
            "/usr/bin/env",
            format!("PATH={}", path.to_string_lossy()),
            env!("CARGO_BIN_EXE_enumeray")
        ]);
        fs::write(config, serde_json::to_vec(&binding).unwrap()).unwrap();
        let api = Api::start(2, true).await;
        api.state.lock().unwrap().slurm = true;
        let mut child = start(&scratch, &api, 1);
        let deadline = Instant::now() + Duration::from_secs(20);
        while !scheduler.join("intent").exists() {
            assert!(
                Instant::now() < deadline,
                "{}",
                fs::read_to_string(scratch.0.join("service.log")).unwrap()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        if observed_execution {
            let intent = fs::read_to_string(scheduler.join("intent")).unwrap();
            let remote = scratch.0.join("compute").join(intent);
            for name in ["started", "worker.lock"] {
                let path = remote.join(name);
                fs::write(&path, "true").unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
            }
            let workspace = remote.join("workspace");
            fs::create_dir(&workspace).unwrap();
            fs::set_permissions(&workspace, fs::Permissions::from_mode(0o700)).unwrap();
            fs::write(workspace.join("stdout.log"), "Retained before failure.\n").unwrap();
            fs::set_permissions(
                workspace.join("stdout.log"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            fs::write(scheduler.join("state"), "RUNNING").unwrap();
            api.until(|p| p.events.values().any(|e| e["phase"] == "executing"))
                .await;
        }
        api.state.lock().unwrap().lose_event = true;
        fs::write(scheduler.join("state"), "FAILED").unwrap();
        fs::write(scheduler.join("exit-code"), "23:0").unwrap();
        api.until(|p| p.unavailable).await;
        child.kill().await.unwrap();
        child.wait().await.unwrap();
        let retained = scratch.read()["prepared_assignments"][0]["execution"].clone();
        assert_eq!(
            retained["events"].as_array().unwrap().len(),
            1 + usize::from(observed_execution)
        );
        assert_eq!(
            retained["scheduler_evidence"]["allocation"]["exit_status"],
            23
        );
        {
            let mut peer = api.state.lock().unwrap();
            peer.unavailable = false;
            peer.lose_report = true;
        }
        let child = start(&scratch, &api, 3);
        archived(&scratch, 2).await;
        stop(child).await;
        let archive: Vec<Value> = fs::read_dir(scratch.journal().join("archive"))
            .unwrap()
            .map(|entry| serde_json::from_slice(&fs::read(entry.unwrap().path()).unwrap()).unwrap())
            .collect();
        assert_eq!(archive.len(), 2);
        let resumed = &archive
            .iter()
            .find(|document| {
                document["entry"]["execution"]["events"][0]["id"] == retained["events"][0]["id"]
            })
            .unwrap()["entry"];
        assert_eq!(
            resumed["execution"]["scheduler_evidence"],
            retained["scheduler_evidence"]
        );
        assert_eq!(resumed["execution"]["report"], retained["report"]);
        for (before, after) in retained["events"]
            .as_array()
            .unwrap()
            .iter()
            .zip(resumed["execution"]["events"].as_array().unwrap())
        {
            assert_eq!(before["id"], after["id"]);
            assert_eq!(before["request"], after["request"]);
        }
        assert!(!scratch.0.join("launches").exists());
        let calls = fs::read_to_string(scheduler.join("calls.jsonl")).unwrap();
        assert_eq!(
            calls
                .lines()
                .filter(|line| serde_json::from_str::<Value>(line).unwrap()["command"] == "sbatch")
                .count(),
            2
        );
        let peer = api.state.lock().unwrap();
        assert_eq!(peer.maximum_observed, 1);
        assert_eq!(peer.jobs.len(), 2);
        assert!(
            peer.jobs
                .values()
                .all(|job| job.result.as_ref().unwrap()["failure"]["code"] == "slurm_failed")
        );
        assert!(
            peer.events
                .values()
                .all(|event| event["phase"].is_null() || event["phase"] == "executing")
        );
        assert_eq!(
            peer.events
                .values()
                .filter(|event| event["phase"].is_null())
                .count(),
            2
        );
    }
}

#[tokio::test]
async fn proven_launch_failure_replays_after_lost_event_and_result_ack_and_releases_capacity() {
    for configuration in ["connector", "runner", "slurm"] {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new();
        install(&scratch, 0.0);
        let descriptor =
            support::desired_configuration(&scratch.0)["publications"][0]["descriptor"].clone();
        let config = scratch.0.join("installation-fixture.json");
        let mut binding: Value = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
        let scheduler = scratch.0.join("scheduler");
        if configuration == "connector" {
            binding["installations"] = json!([]);
        } else if configuration == "runner" {
            binding["installations"][0]["installation"]["argv"][0] =
                json!(scratch.0.join("unavailable-executable"));
        } else {
            fs::create_dir(&scheduler).unwrap();
            for name in ["sbatch", "squeue", "sacct", "scancel"] {
                let path = scheduler.join(name);
                fs::write(
                    &path,
                    include_str!("../../connector/tests/fixtures/cancellation-scheduler.py"),
                )
                .unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
            }
            fs::write(scheduler.join("mode"), "reject-submit").unwrap();
            fs::write(scheduler.join("state"), "LAG").unwrap();
            let script = scratch.0.join("model.sbatch");
            fs::write(&script, "#!/bin/sh\nexit 1\n").unwrap();
            let installation = &mut binding["installations"][0];
            installation["driver"] = json!({"kind":"slurm","worker_path":"/unavailable/worker"});
            installation["slurm_script"] = json!(script);
            installation["installation"]["file_digests"] = json!({script.to_str().unwrap():STANDARD.encode(Sha256::digest(fs::read(&script).unwrap()))});
            let path = std::env::join_paths(std::iter::once(scheduler.clone()).chain(
                std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
            ))
            .unwrap();
            installation["connection"]["launcher"] = json!([
                "/usr/bin/env",
                format!("PATH={}", path.to_string_lossy()),
                env!("CARGO_BIN_EXE_enumeray")
            ]);
        }
        fs::write(config, serde_json::to_vec(&binding).unwrap()).unwrap();
        let api = Api::start(2, true).await;
        {
            let mut peer = api.state.lock().unwrap();
            peer.slurm = configuration == "slurm";
            // The accepted publication remains valid when the connector loses
            // its local installation; that loss must produce startup evidence.
            if configuration == "connector" {
                peer.assignment_descriptor = Some(descriptor);
            }
            peer.lose_event = true;
        }
        let mut child = start(&scratch, &api, 1);
        api.until(|peer| peer.unavailable).await;
        child.kill().await.unwrap();
        child.wait().await.unwrap();
        let retained = scratch.read()["prepared_assignments"][0]["execution"].clone();
        assert_eq!(retained["events"].as_array().unwrap().len(), 1);
        assert!(retained["events"][0]["request"]["phase"].is_null());
        assert!(retained["events"][0]["request"]["backend_handle"].is_null());
        assert!(retained["startup_evidence"].is_object());
        assert!(retained["receipt"].is_null());
        {
            let mut peer = api.state.lock().unwrap();
            peer.unavailable = false;
            peer.lose_report = true;
        }
        let child = start(&scratch, &api, 3);
        archived(&scratch, 2).await;
        stop(child).await;
        let archives: Vec<Value> = fs::read_dir(scratch.journal().join("archive"))
            .unwrap()
            .map(|entry| serde_json::from_slice(&fs::read(entry.unwrap().path()).unwrap()).unwrap())
            .collect();
        let replayed = &archives
            .iter()
            .find(|entry| {
                entry["entry"]["execution"]["events"][0]["id"] == retained["events"][0]["id"]
            })
            .unwrap()["entry"];
        assert_eq!(
            replayed["execution"]["startup_evidence"],
            retained["startup_evidence"]
        );
        assert_eq!(replayed["execution"]["report"], retained["report"]);
        assert_eq!(
            replayed["execution"]["events"][0]["request"],
            retained["events"][0]["request"]
        );
        assert!(replayed["execution"]["receipt"].is_object());
        assert!(!scratch.0.join("launches").exists());
        let peer = api.state.lock().unwrap();
        assert_eq!(peer.maximum_observed, 1);
        assert_eq!(peer.jobs.len(), 2);
        assert_eq!(
            peer.logs.len(),
            if configuration == "slurm" { 6 } else { 4 }
        );
        assert!(peer.logs.values().all(|batch| batch["text"] == ""
            && batch["byte_length"] == 0
            && batch["gap"]["reason"] == "source_unavailable"
            && batch["gap"]["bytes"].is_null()));
        assert!(peer.events.values().all(|event| event["phase"].is_null()));
        assert!(
            peer.jobs
                .values()
                .all(|job| job.result.as_ref().unwrap()["receipt"]["outcome"] == "failed")
        );
        if configuration == "slurm" {
            assert_eq!(
                fs::read_to_string(scheduler.join("calls.jsonl"))
                    .unwrap()
                    .lines()
                    .filter(
                        |line| serde_json::from_str::<Value>(line).unwrap()["command"] == "sbatch"
                    )
                    .count(),
                2
            );
        }
    }
}

fn install_bulk(scratch: &Scratch) {
    install(scratch, 0.0);
    let wrapper = scratch.0.join("bulk.py");
    let code = format!(
        r#"import json,time
from pathlib import Path
r=json.loads(Path('request.json').read_text())
with open({launches:?},'a') as f:f.write(str(r['launch_intent_id'])+'\n')
Path({started:?}+'-'+r['attempt_id']).write_text('started')
value=r['inputs']['value']
if value==3:time.sleep(10)
for key in ['inputs','model_id','publication_id','input_files']:r.pop(key,None)
if value==2:
 Path('answer.txt').write_text('abc')
 r.update(outcome='succeeded',outputs={{}},failure=None,files={{'result':{{'path':'answer.txt','media_type':'text/plain'}}}})
else:r.update(outcome='succeeded',outputs={{'doubled':6}},failure=None,files={{}})
Path('result.json').write_text(json.dumps(r))
"#,
        launches = scratch.0.join("launches"),
        started = scratch.0.join("wrapper-started").to_str().unwrap(),
    );
    fs::write(&wrapper, code).unwrap();
    let path = scratch.0.join("installation-fixture.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    config["installations"][0]["installation"]["argv"] = json!(["/usr/bin/python3", wrapper]);
    fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
}
async fn cancelled_second(api: &Api) -> Uuid {
    api.until(|p| {
        p.jobs
            .values()
            .find(|j| j.assignment["inputs"]["value"] == 3)
            .is_some_and(|second| {
                p.events.values().any(|e| {
                    e["phase"] == "executing" && e["attempt_id"] == second.assignment["attempt_id"]
                }) && p
                    .installation_root
                    .as_ref()
                    .unwrap()
                    .join(format!(
                        "wrapper-started-{}",
                        second.assignment["attempt_id"].as_str().unwrap()
                    ))
                    .exists()
            })
    })
    .await;
    let id = *api
        .state
        .lock()
        .unwrap()
        .jobs
        .iter()
        .find(|(_, j)| j.assignment["inputs"]["value"] == 3)
        .unwrap()
        .0;
    api.state.lock().unwrap().cancel_assignment = Some(id);
    api.until(|p| {
        p.jobs[&id]
            .result
            .as_ref()
            .is_some_and(|r| r["receipt"]["outcome"] == "cancelled")
    })
    .await;
    id
}
#[tokio::test]
async fn slow_input_staging_keeps_second_assignment_cancellation_and_shutdown_responsive() {
    let scratch = Scratch::new();
    install(&scratch, 10.0);
    let api = Api::start(2, true).await;
    {
        let mut peer = api.state.lock().unwrap();
        peer.allowed_outstanding = 2;
        peer.bulk_input = true;
        peer.hold_input = true;
    }
    let child = start(&scratch, &api, 1);
    api.until(|p| p.input_started > 0).await;
    cancelled_second(&api).await;
    assert!(api.state.lock().unwrap().hold_input);
    let before = Instant::now();
    assert_eq!(stop(child).await["drained"], false);
    assert!(before.elapsed() < Duration::from_secs(4));
    let journal = scratch.read();
    assert!(
        journal["prepared_assignments"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["transfers"]["task"].is_object())
    );
    // Explicit cancellation after recovery stops prelaunch work without a model launch.
    api.state.lock().unwrap().cancel_requested = true;
    let child = start(&scratch, &api, 3);
    archived(&scratch, 2).await;
    assert_eq!(stop(child).await["drained"], true);
    assert_eq!(
        fs::read_to_string(scratch.0.join("launches"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}
#[tokio::test]
async fn unpublished_outputs_free_compute_capacity_and_resume_after_restart_without_relaunch() {
    let scratch = Scratch::new();
    install_bulk(&scratch);
    let api = Api::start(2, true).await;
    {
        let mut peer = api.state.lock().unwrap();
        peer.allowed_outstanding = 2;
        peer.local_workers = Some(1);
        peer.bulk_output = true;
        peer.hold_output = true;
    }
    let child = start(&scratch, &api, 1);
    api.until(|p| p.output_started > 0).await;
    cancelled_second(&api).await;
    assert_eq!(
        api.state
            .lock()
            .unwrap()
            .jobs
            .values()
            .filter(|j| j.result.is_some())
            .count(),
        1
    );
    assert_eq!(stop(child).await["drained"], false);
    let saved = scratch.read();
    let output = saved["prepared_assignments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["execution"]["files"]["result"].is_object())
        .unwrap();
    assert_eq!(output["transfers"]["allocation_released"], true);
    assert!(output["transfers"]["expires_at"].is_null());
    api.state.lock().unwrap().hold_output = false;
    let child = start(&scratch, &api, 3);
    archived(&scratch, 2).await;
    assert_eq!(stop(child).await["drained"], true);
    assert_eq!(
        fs::read_to_string(scratch.0.join("launches"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    assert!(api.state.lock().unwrap().output_verified);
}
#[tokio::test]
async fn explicit_retention_expiry_reports_delivery_failure_and_preserves_scientific_evidence() {
    let scratch = Scratch::new();
    install_bulk(&scratch);
    let api = Api::start(1, true).await;
    {
        let mut peer = api.state.lock().unwrap();
        peer.bulk_output = true;
        peer.hold_output = true;
        peer.retention = Some(2);
    }
    let child = start(&scratch, &api, 1);
    archived(&scratch, 1).await;
    let archive: Value = serde_json::from_slice(
        &fs::read(
            fs::read_dir(scratch.journal().join("archive"))
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        archive["entry"]["execution"]["report"]["outcome"], "succeeded",
        "{archive}"
    );
    assert_eq!(
        archive["entry"]["execution"]["delivery_report"]["failure"]["code"],
        "output_retention_expired"
    );
    let intent = archive["entry"]["launch_intent_id"].as_str().unwrap();
    assert_eq!(
        fs::read(
            scratch
                .0
                .join("compute")
                .join(intent)
                .join("workspace/answer.txt")
        )
        .unwrap(),
        b"abc"
    );
    assert_eq!(stop(child).await["drained"], true);
}

fn gate_runner(scratch: &Scratch, action: &str) -> PathBuf {
    gate_runner_at(scratch, action, false)
}
fn gate_runner_at(scratch: &Scratch, action: &str, after_action: bool) -> PathBuf {
    let gate = scratch.0.join("transfer-gate");
    fs::write(&gate, b"hold").unwrap();
    let launcher = scratch.0.join("gated-runner.py");
    fs::write(
        &launcher,
        format!(
            r#"import os,subprocess,sys,time
from pathlib import Path
if sys.argv[3]=={action:?}:
 if {after_action}:completed=subprocess.run(sys.argv[1:],check=True,stdout=subprocess.PIPE)
 Path({started:?}).write_text(str(os.getpid()))
 while Path({gate:?}).exists():time.sleep(.025)
 if {after_action}:
  sys.stdout.buffer.write(completed.stdout)
  sys.exit(0)
os.execv(sys.argv[1],sys.argv[1:])
"#,
            after_action = if after_action { "True" } else { "False" },
            started = scratch.0.join("transfer-started")
        ),
    )
    .unwrap();
    let path = scratch.0.join("installation-fixture.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    config["installations"][0]["connection"]["launcher"] =
        json!(["/usr/bin/python3", launcher, env!("CARGO_BIN_EXE_enumeray")]);
    fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
    gate
}
async fn transfer_started(scratch: &Scratch) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !scratch.0.join("transfer-started").exists() {
        assert!(
            Instant::now() < deadline,
            "{}",
            fs::read_to_string(scratch.0.join("service.log")).unwrap_or_default()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
#[tokio::test]
async fn slow_runner_input_upload_does_not_block_second_assignment() {
    let scratch = Scratch::new();
    install(&scratch, 10.0);
    let gate = gate_runner(&scratch, "write-input");
    let api = Api::start(2, true).await;
    {
        let mut peer = api.state.lock().unwrap();
        peer.allowed_outstanding = 2;
        peer.bulk_input = true;
    }
    let child = start(&scratch, &api, 1);
    transfer_started(&scratch).await;
    cancelled_second(&api).await;
    assert!(gate.exists());
    assert_eq!(stop(child).await["drained"], false);
    api.state.lock().unwrap().cancel_requested = true;
    let child = start(&scratch, &api, 3);
    archived(&scratch, 2).await;
    stop(child).await;
    assert_eq!(
        fs::read_to_string(scratch.0.join("launches"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}
#[tokio::test]
async fn interrupted_output_collection_resumes_same_run_while_compute_slots_remain_available() {
    let scratch = Scratch::new();
    install_bulk(&scratch);
    let gate = gate_runner(&scratch, "read-output");
    let api = Api::start(2, true).await;
    {
        let mut peer = api.state.lock().unwrap();
        peer.allowed_outstanding = 2;
        peer.local_workers = Some(1);
        peer.bulk_output = true;
    }
    let child = start(&scratch, &api, 1);
    transfer_started(&scratch).await;
    cancelled_second(&api).await;
    assert_eq!(stop(child).await["drained"], false);
    fs::remove_file(gate).unwrap();
    let child = start(&scratch, &api, 3);
    archived(&scratch, 2).await;
    stop(child).await;
    assert_eq!(
        fs::read_to_string(scratch.0.join("launches"))
            .unwrap()
            .lines()
            .count(),
        2
    );
}
#[tokio::test]
async fn approved_shared_workspace_publishes_without_a_connector_output_copy() {
    let scratch = Scratch::new();
    install_bulk(&scratch);
    let path = scratch.0.join("installation-fixture.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let root = config["installations"][0]["work_root"].clone();
    fs::create_dir_all(root.as_str().unwrap()).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root.as_str().unwrap(), fs::Permissions::from_mode(0o700)).unwrap();
    }
    let connector_root = PathBuf::from(root.as_str().unwrap())
        .canonicalize()
        .unwrap();
    config["installations"][0]["shared_workspace"] =
        json!({"runner_root":root,"connector_root":connector_root});
    fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
    let api = Api::start(1, true).await;
    api.state.lock().unwrap().bulk_output = true;
    let child = start(&scratch, &api, 3);
    archived(&scratch, 1).await;
    stop(child).await;
    let entries = fs::read_dir(scratch.journal().join("compute-jobs"))
        .unwrap()
        .collect::<std::io::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert!(!entries[0].path().join("outputs").exists());
    assert!(api.state.lock().unwrap().output_verified);
}

#[tokio::test]
async fn abrupt_parent_loss_stops_bulk_descendants_before_recovery() {
    let scratch = Scratch::new();
    install(&scratch, 0.0);
    let _gate = gate_runner(&scratch, "write-input");
    let api = Api::start(1, true).await;
    api.state.lock().unwrap().bulk_input = true;
    let mut child = start(&scratch, &api, 1);
    transfer_started(&scratch).await;
    let pid = fs::read_to_string(scratch.0.join("transfer-started")).unwrap();
    child.kill().await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(4);
    while Command::new("/bin/kill")
        .args(["-0", &pid])
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .unwrap()
        .success()
    {
        // Container PID 1 may leave a terminated adopted child unreaped. A
        // zombie still answers kill -0, but cannot execute or retain file locks.
        #[cfg(target_os = "linux")]
        if fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|record| {
            record
                .rsplit_once(") ")
                .is_some_and(|(_, fields)| fields.starts_with('Z'))
        }) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "bulk descendant survived parent death"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    api.state.lock().unwrap().cancel_requested = true;
    let child = start(&scratch, &api, 3);
    archived(&scratch, 1).await;
    stop(child).await;
    assert!(!scratch.0.join("launches").exists());
}

#[tokio::test]
async fn pending_cap_applies_backpressure_until_required_publication_finishes() {
    let scratch = Scratch::new();
    install_bulk(&scratch);
    let api = Api::start(2, true).await;
    {
        let mut peer = api.state.lock().unwrap();
        peer.allowed_outstanding = 2;
        peer.local_workers = Some(1);
        peer.pending_cap = Some(1);
        peer.bulk_output = true;
        peer.hold_output = true;
    }
    let child = start(&scratch, &api, 3);
    api.until(|p| p.output_started > 0).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(api.state.lock().unwrap().jobs.len(), 1);
    api.state.lock().unwrap().hold_output = false;
    cancelled_second(&api).await;
    archived(&scratch, 2).await;
    assert_eq!(stop(child).await["drained"], true);
}
#[tokio::test]
async fn full_transfer_pool_still_claims_and_cancels_other_prelaunch_work() {
    let scratch = Scratch::new();
    install(&scratch, 0.0);
    let api = Api::start(2, true).await;
    {
        let mut peer = api.state.lock().unwrap();
        peer.allowed_outstanding = 2;
        peer.transfer_workers = Some(1);
        peer.bulk_input = true;
        peer.hold_input = true;
    }
    let child = start(&scratch, &api, 3);
    api.until(|p| p.input_started > 0 && p.jobs.len() == 2)
        .await;
    let second = *api
        .state
        .lock()
        .unwrap()
        .jobs
        .iter()
        .find(|(_, j)| j.assignment["inputs"]["value"] == 3)
        .unwrap()
        .0;
    api.state.lock().unwrap().cancel_assignment = Some(second);
    api.until(|p| {
        p.jobs[&second]
            .result
            .as_ref()
            .is_some_and(|r| r["receipt"]["outcome"] == "cancelled")
    })
    .await;
    assert_eq!(api.state.lock().unwrap().input_started, 1);
    assert!(!scratch.0.join("launches").exists());
    api.state.lock().unwrap().cancel_requested = true;
    archived(&scratch, 2).await;
    service_status(&scratch, "idle").await;
    assert_eq!(stop(child).await["drained"], true);
}

#[tokio::test]
async fn expired_reservations_and_lost_verification_replies_reuse_the_finalized_run() {
    let scratch = Scratch::new();
    install_bulk(&scratch);
    let api = Api::start(1, true).await;
    {
        let mut peer = api.state.lock().unwrap();
        peer.bulk_output = true;
        peer.expire_reservation = true;
        peer.lose_verification = true;
    }
    let child = start(&scratch, &api, 3);
    archived(&scratch, 1).await;
    stop(child).await;
    let peer = api.state.lock().unwrap();
    assert!(peer.output_verified);
    assert_eq!(peer.output_started, 1);
    assert!(peer.reservation_keys.len() >= 2);
    assert_ne!(peer.reservation_keys[0], peer.reservation_keys[1]);
    assert_eq!(
        fs::read_to_string(scratch.0.join("launches"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert!(
        fs::read_to_string(scratch.0.join("service.log"))
            .unwrap()
            .contains("output_reservation_expired")
    );
}
#[tokio::test]
async fn large_scientific_reports_publish_through_bounded_transfer_completion_frames() {
    let scratch = Scratch::new();
    install_bulk(&scratch);
    let wrapper = scratch.0.join("bulk.py");
    let code = fs::read_to_string(&wrapper).unwrap().replace(
        "outputs={},failure=None,files=",
        "outputs={'details':'x'*61440},failure=None,files=",
    );
    fs::write(wrapper, code).unwrap();
    let api = Api::start(1, true).await;
    {
        let mut peer = api.state.lock().unwrap();
        peer.bulk_output = true;
        peer.large_output = true;
    }
    let child = start(&scratch, &api, 3);
    archived(&scratch, 1).await;
    assert_eq!(stop(child).await["drained"], true);
    let peer = api.state.lock().unwrap();
    let result = peer.jobs.values().next().unwrap().result.as_ref().unwrap();
    assert_eq!(result["receipt"]["outcome"], "succeeded");
    assert_eq!(result["outputs"]["details"].as_str().unwrap().len(), 61440);
    assert!(peer.output_verified);
    assert_eq!(peer.output_started, 1);
    assert_eq!(
        fs::read_to_string(scratch.0.join("launches"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[tokio::test]
async fn stalled_bulk_work_is_diagnosed_and_retried_without_blocking_cancellation() {
    let scratch = Scratch::new();
    install(&scratch, 0.0);
    // Hold a completed preparation reply: the supervisor's stall budget wins
    // over the worker's overall deadline, and cancellation can inspect the runner.
    let _gate = gate_runner_at(&scratch, "prepare", true);
    let api = Api::start(1, true).await;
    let child = start(&scratch, &api, 3);
    transfer_started(&scratch).await;
    let deadline = Instant::now() + Duration::from_secs(75);
    loop {
        let saved = if scratch.journal().join("journal.json").exists() {
            Some(scratch.read())
        } else {
            None
        };
        if saved.as_ref().is_some_and(|d| {
            d["prepared_assignments"][0]["transfers"]["retries"]
                .as_u64()
                .is_some_and(|r| r > 0)
        }) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "missing stalled-transfer diagnostic"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        fs::read_to_string(scratch.0.join("service.log"))
            .unwrap()
            .contains("transfer_stalled")
    );
    api.state.lock().unwrap().cancel_requested = true;
    archived(&scratch, 1).await;
    assert_eq!(stop(child).await["drained"], true);
    assert!(!scratch.0.join("launches").exists());
}

fn use_real_identity(scratch: &Scratch, development: bool) {
    let path = scratch.0.join("installation-fixture.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    config["installations"][0]["execution_mode"] = json!(if development {
        "development"
    } else {
        "managed"
    });
    config["installations"][0]["real_identity"] = json!(true);
    fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
}

#[tokio::test]
async fn same_user_requires_both_optins_and_managed_never_falls_back() {
    for (development, allowed) in [(false, false), (false, true), (true, false)] {
        let scratch = Scratch::new();
        install(&scratch, 0.0);
        use_real_identity(&scratch, development);
        let api = Api::start(0, true).await;
        let child = start_with_policy(&scratch, &api, 1, allowed);
        api.until(|peer| {
            peer.validations
                .iter()
                .any(|validation| validation["ready"] == false)
        })
        .await;
        let message = api.state.lock().unwrap().validations.last().unwrap()["message"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(
            message.contains(if development {
                "--allow-development-execution"
            } else {
                "separate execution identity"
            }),
            "{message}"
        );
        assert!(!scratch.0.join("launches").exists());
        assert_eq!(stop(child).await["drained"], true);
    }
}

fn regular_user() -> bool {
    let regular = std::process::Command::new("/usr/bin/id")
        .arg("-u")
        .output()
        .unwrap()
        .stdout
        != b"0\n";
    if !regular {
        eprintln!(
            "same-user success requires a non-root test user; root rejection is covered by execution_policy"
        );
    }
    regular
}

#[tokio::test]
async fn development_restart_without_consent_observes_delivers_and_cancels_one_launch() {
    if !regular_user() {
        return;
    }
    for cancel in [false, true] {
        let scratch = Scratch::new();
        let gate = install_gated(&scratch);
        use_real_identity(&scratch, true);
        let api = Api::start(1, true).await;
        let mut child = start_with_policy(&scratch, &api, 1, true);
        api.until(|peer| {
            peer.events
                .values()
                .any(|event| event["phase"] == "executing")
        })
        .await;
        let intent = scratch.read()["prepared_assignments"][0]["launch_intent_id"].clone();
        let remote = scratch.0.join("compute").join(intent.as_str().unwrap());
        let snapshot = fs::read(remote.join("remote.json")).unwrap();
        assert!(
            fs::read_to_string(scratch.0.join("service.log"))
                .unwrap()
                .contains("connector credentials and files")
        );
        child.kill().await.unwrap();
        child.wait().await.unwrap();
        let child = start_with_policy(&scratch, &api, 3, false);
        service_status(&scratch, "starting").await;
        if cancel {
            api.state.lock().unwrap().cancel_requested = true;
        } else {
            fs::remove_file(gate).unwrap();
        }
        archived(&scratch, 1).await;
        assert_eq!(stop(child).await["drained"], true);
        assert_eq!(fs::read(remote.join("remote.json")).unwrap(), snapshot);
        assert!(
            !scratch.read()["last_allow_development_execution"]
                .as_bool()
                .unwrap_or(false)
        );
        assert_eq!(
            fs::read_to_string(scratch.0.join("launches"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        let peer = api.state.lock().unwrap();
        assert_eq!(
            peer.jobs.values().next().unwrap().result.as_ref().unwrap()["receipt"]["outcome"],
            if cancel { "cancelled" } else { "succeeded" }
        );
    }
}

#[tokio::test]
async fn prepared_development_work_waits_for_fresh_consent_and_preserves_its_intent() {
    if !regular_user() {
        return;
    }
    let scratch = Scratch::new();
    install(&scratch, 0.0);
    let gate = gate_runner(&scratch, "prepare");
    use_real_identity(&scratch, true);
    let api = Api::start(1, true).await;
    let child = start_with_policy(&scratch, &api, 1, true);
    transfer_started(&scratch).await;
    let intent = scratch.read()["prepared_assignments"][0]["launch_intent_id"].clone();
    assert_eq!(stop(child).await["drained"], false);
    fs::remove_file(gate).unwrap();
    let child = start_with_policy(&scratch, &api, 1, false);
    service_status(&scratch, "blocked").await;
    let pending = scratch.read()["prepared_assignments"][0].clone();
    assert_eq!(pending["launch_intent_id"], intent);
    assert!(pending["terminal_result"].is_null());
    assert!(!scratch.0.join("launches").exists());
    assert!(
        !scratch
            .journal()
            .join("compute-jobs")
            .join(intent.as_str().unwrap())
            .join("start-requested.json")
            .exists()
    );
    let inspection = support::successful_json(
        &support::run(
            support::command()
                .arg("inspect")
                .arg("--journal")
                .arg(scratch.journal()),
        )
        .await,
    );
    assert_eq!(inspection["last_allow_development_execution"], false);
    assert_eq!(
        inspection["configuration_retention"]["execution_contexts"][0]["execution_mode"],
        "development"
    );
    assert_eq!(stop(child).await["drained"], false);
    let child = start_with_policy(&scratch, &api, 3, true);
    archived(&scratch, 1).await;
    assert_eq!(stop(child).await["drained"], true);
    assert_eq!(
        fs::read_to_string(scratch.0.join("launches"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    let archive: Value = serde_json::from_slice(
        &fs::read(
            fs::read_dir(scratch.journal().join("archive"))
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(archive["entry"]["launch_intent_id"], intent);
}

#[tokio::test]
async fn development_lost_start_reply_recovers_without_consent_or_duplicate_launch() {
    if !regular_user() {
        return;
    }
    let scratch = Scratch::new();
    let model_gate = install_gated(&scratch);
    let reply_gate = gate_runner_at(&scratch, "start", true);
    use_real_identity(&scratch, true);
    let api = Api::start(1, true).await;
    let mut child = start_with_policy(&scratch, &api, 1, true);
    transfer_started(&scratch).await;
    let intent = scratch.read()["prepared_assignments"][0]["launch_intent_id"].clone();
    assert!(
        scratch
            .journal()
            .join("compute-jobs")
            .join(intent.as_str().unwrap())
            .join("start-requested.json")
            .exists()
    );
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    fs::remove_file(reply_gate).unwrap();
    let child = start_with_policy(&scratch, &api, 3, false);
    fs::remove_file(model_gate).unwrap();
    archived(&scratch, 1).await;
    assert_eq!(stop(child).await["drained"], true);
    assert_eq!(
        fs::read_to_string(scratch.0.join("launches"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}
