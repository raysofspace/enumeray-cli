use enumeray_client::{
    EnumerayClient, Error, PreparedRun, RunSubmission, resource_path, validate_url,
};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
    time::Duration,
};

fn serve(responses: Vec<String>) -> (String, thread::JoinHandle<Vec<String>>) {
    serve_delayed(responses, Duration::ZERO)
}
fn serve_delayed(
    responses: Vec<String>,
    delay: Duration,
) -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    let worker = thread::spawn(move || {
        responses
            .into_iter()
            .map(|response| {
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
                let header = String::from_utf8(bytes.clone()).unwrap();
                let length = header
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|value| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                let mut body = vec![0; length];
                stream.read_exact(&mut body).unwrap();
                bytes.extend(body);
                thread::sleep(delay);
                let _ = stream.write_all(response.as_bytes());
                String::from_utf8(bytes).unwrap()
            })
            .collect()
    });
    (origin, worker)
}
fn response(status: &str, headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{headers}\r\n{body}",
        body.len()
    )
}

fn context_status() -> Value {
    let work = json!({"retained_queued_runs":0,"unfinished_assignments":0,"pending_delivery_assignments":0,
        "pending_uploads":0,"pending_log_archives":0,"unconfirmed_assignments":0,
        "assignment_phases":{"preparing":0,"waiting_for_backend":0,"executing":0,"collecting_results":0,"uploading_results":0}});
    json!({"id":"source","connector_id":"worker","generation":7,"state":"draining",
        "replacement_context_id":"next","accepted_work":"drain","work":work,
        "live_current_publications":0,"retained_publications":0,"pending_configuration_retirements":0,
        "configuration_removal_generation":9,"connector_generation":9,"observed_generation":9,"retirement_ready":true,
        "connector_work":work,"local_cleanup_state":"requires_local_inspection"})
}

#[test]
fn typed_grant_methods_preserve_identity_and_never_refresh_stale_guards() {
    use enumeray_contracts::management::GrantUpdate;
    let grant = uuid::Uuid::new_v4();
    let record = json!({"id":grant,"context_id":"cpu","user_id":uuid::Uuid::new_v4(),"max_queued_runs":10,"max_outstanding_assignments":2,"generation":7,"revoked_at":null});
    let (origin, worker) = serve(vec![
        response("200 OK", "", &record.to_string()),
        response("200 OK", "", &record.to_string()),
        response(
            "409 Conflict",
            "",
            r#"{"code":"management_conflict","message":"Inspect first","request_id":"grant-test"}"#,
        ),
    ]);
    let client =
        EnumerayClient::new(&origin, Some("secret".into()), Duration::from_secs(5)).unwrap();
    assert_eq!(client.compute_grant("worker", grant).unwrap().generation, 7);
    let request = GrantUpdate {
        expected_generation: 6,
        max_queued_runs: 10,
        max_outstanding_assignments: 2,
    };
    assert_eq!(
        client
            .update_compute_grant("worker", grant, &request)
            .unwrap()
            .id,
        grant
    );
    assert!(matches!(
        client.update_compute_grant("worker", grant, &request),
        Err(Error::Api { status: 409, .. })
    ));
    let requests = worker.join().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].starts_with(&format!(
        "GET /api/v1/connectors/worker/grants/{grant} HTTP/1.1\r\n"
    )));
    for wire in &requests[1..] {
        assert!(wire.starts_with(&format!(
            "PUT /api/v1/connectors/worker/grants/{grant} HTTP/1.1\r\n"
        )));
        let body: Value = serde_json::from_str(wire.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(
            body,
            json!({"expected_generation":6,"max_queued_runs":10,"max_outstanding_assignments":2})
        );
    }
}

#[test]
fn typed_context_methods_preserve_explicit_policy_guard_and_opaque_pagination() {
    use enumeray_contracts::management::{
        ContextAcceptedWorkPolicy, ContextCutover, ContextDependencySection, ContextLifecycleState,
        GenerationChange,
    };
    let status = context_status();
    let plan = json!({"context":status,"replacement":{"id":"next","connector_id":"worker-next","state":"active","generation":1,"driver":"process","connection":"direct","metadata":{"location":"Lab","disclosure":"Lab data access."}},"dependencies":{"items":[],"next_cursor":"cursor"},"actions":[]});
    let receipt = json!({"operation":"cutover","context":status,"cancelled_queued_runs":3,"requested_publications":0});
    let (origin, worker) = serve(vec![
        response("200 OK", "", &status.to_string()),
        response("200 OK", "", &plan.to_string()),
        response("200 OK", "", &receipt.to_string()),
        response("200 OK", "", &receipt.to_string()),
        response("200 OK", "", &receipt.to_string()),
        response("200 OK", "", &receipt.to_string()),
    ]);
    let client =
        EnumerayClient::new(&origin, Some("secret".into()), Duration::from_secs(5)).unwrap();
    assert_eq!(
        client.context_status("worker", "source").unwrap().state,
        ContextLifecycleState::Draining
    );
    let plan = client
        .context_replacement_plan(
            "worker",
            "source",
            Some("next/other?"),
            ContextDependencySection::Runs,
            Some(12),
            Some("a+b/c?="),
        )
        .unwrap();
    assert_eq!(plan.dependencies.next_cursor.as_deref(), Some("cursor"));
    let result = client
        .cutover_context(
            "worker",
            "source",
            &ContextCutover {
                expected_generation: 6,
                replacement_context_id: Some("next".into()),
                accepted_work: ContextAcceptedWorkPolicy::CancelQueued,
            },
        )
        .unwrap();
    assert_eq!(result.cancelled_queued_runs, 3);
    let guard = GenerationChange {
        expected_generation: 7,
    };
    client.reopen_context("worker", "source", &guard).unwrap();
    client
        .release_context_configurations("worker", "source", &guard)
        .unwrap();
    client.retire_context("worker", "source", &guard).unwrap();
    let requests = worker.join().unwrap();
    assert!(requests[0].starts_with("GET /api/v1/connectors/worker/contexts/source HTTP/1.1\r\n"));
    assert!(requests[1].starts_with("GET /api/v1/connectors/worker/contexts/source/replacement-plan?section=runs&limit=12&replacement_context_id=next%2Fother%3F&after=a%2Bb%2Fc%3F%3D HTTP/1.1\r\n"));
    for (index, endpoint, body) in [
        (
            2,
            "cutover",
            json!({"expected_generation":6,"replacement_context_id":"next","accepted_work":"cancel_queued"}),
        ),
        (3, "reopen", json!({"expected_generation":7})),
        (
            4,
            "configuration-retirements",
            json!({"expected_generation":7}),
        ),
        (5, "retirement", json!({"expected_generation":7})),
    ] {
        assert!(requests[index].starts_with(&format!(
            "PUT /api/v1/connectors/worker/contexts/source/{endpoint} HTTP/1.1\r\n"
        )));
        assert_eq!(
            serde_json::from_str::<Value>(requests[index].split_once("\r\n\r\n").unwrap().1)
                .unwrap(),
            body
        );
    }
}

#[test]
fn connector_lifecycle_methods_keep_maintenance_and_authority_requests_separate() {
    use enumeray_contracts::management::{ConnectorAuthorityChange, ConnectorStateChange};
    let work = json!({"retained_queued_runs":4,"unfinished_assignments":0,"pending_delivery_assignments":1,
        "pending_uploads":0,"pending_log_archives":0,"unconfirmed_assignments":0,
        "assignment_phases":{"preparing":0,"waiting_for_backend":0,"executing":0,"collecting_results":0,"uploading_results":0}});
    let status = json!({"connector_id":"worker","owner_id":"11111111-1111-4111-8111-111111111111","ownership_generation":1,"archived_at":null,"generation":7,"observed_generation":6,"configuration_pending":true,
        "draining":true,"authority_withdrawn":false,"work":work,"maintenance_complete":false,
        "failed_publications":0,"configuration_capacity":{"desired_bytes":500,"maximum_desired_bytes":2097152,"maximum_retained_publications":1024,"maximum_connector_cache_bytes":4194304,"counts":{"historical_publications":0,"retained_publications":0,"retirement_requested":0,"retired_publications":0}}});
    let maintenance = json!({"connector_id":"worker","owner_id":"11111111-1111-4111-8111-111111111111","ownership_generation":1,"archived_at":null,"generation":8,"draining":false,"authority_withdrawn":false,
        "changed":true,"work":work,"maintenance_complete":false});
    let authority = json!({"connector_id":"worker","owner_id":"11111111-1111-4111-8111-111111111111","ownership_generation":1,"archived_at":null,"generation":9,"draining":false,"authority_withdrawn":true,
        "changed":true,"work":work,"maintenance_complete":false});
    let (origin, worker) = serve(vec![
        response("200 OK", "", &status.to_string()),
        response("200 OK", "", &maintenance.to_string()),
        response("200 OK", "", &authority.to_string()),
    ]);
    let client =
        EnumerayClient::new(&origin, Some("secret".into()), Duration::from_secs(5)).unwrap();
    let observed = client.connector_diagnostics("worker").unwrap();
    assert!(observed.draining);
    assert_eq!(observed.work.pending_delivery_assignments, 1);
    let resumed = client
        .set_connector_maintenance(
            "worker",
            &ConnectorStateChange {
                draining: false,
                expected_generation: 7,
            },
        )
        .unwrap();
    assert!(!resumed.draining);
    assert!(!resumed.authority_withdrawn);
    let withdrawn = client
        .set_connector_authority(
            "worker",
            &ConnectorAuthorityChange {
                withdrawn: true,
                expected_generation: 8,
            },
        )
        .unwrap();
    assert!(withdrawn.authority_withdrawn);
    assert!(!withdrawn.draining);
    let requests = worker.join().unwrap();
    assert!(requests[0].starts_with("GET /api/v1/connectors/worker/diagnostics HTTP/1.1\r\n"));
    for (request, endpoint, body) in [
        (
            &requests[1],
            "state",
            json!({"draining":false,"expected_generation":7}),
        ),
        (
            &requests[2],
            "authority",
            json!({"withdrawn":true,"expected_generation":8}),
        ),
    ] {
        assert!(request.starts_with(&format!(
            "PUT /api/v1/connectors/worker/{endpoint} HTTP/1.1\r\n"
        )));
        assert_eq!(
            serde_json::from_str::<Value>(request.split_once("\r\n\r\n").unwrap().1).unwrap(),
            body
        );
    }
}

#[test]
fn url_and_identifier_boundaries_cannot_leak_credentials() {
    for value in [
        "http://example.com",
        "https://token@example.com",
        "https://example.com/#fragment",
        "file:///tmp/foo",
    ] {
        assert!(validate_url(value).is_err());
    }
    assert!(validate_url("http://[::1]:3000").is_ok());
    assert_eq!(
        resource_path(&["models", "a/b?admin=true"]).unwrap(),
        "/api/v1/models/a%2Fb%3Fadmin=true"
    );
    assert!(resource_path(&["models", ".."]).is_err());
    let client = EnumerayClient::new(
        "https://example.com",
        Some("secret".into()),
        Duration::from_secs(1),
    )
    .unwrap();
    for path in [
        "//attacker.example/api/v1/me",
        "/api/v1/../../../elsewhere",
        "https://attacker.example/api/v1/me",
    ] {
        assert!(matches!(client.get::<Value>(path), Err(Error::Invalid(_))));
    }
}

#[test]
fn client_timeouts_reject_zero_and_unrepresentable_deadlines() {
    for timeout in [Duration::ZERO, Duration::from_secs(u64::MAX)] {
        assert!(matches!(
            EnumerayClient::new("https://example.org", None, timeout),
            Err(Error::Invalid(_))
        ));
    }
    let client = EnumerayClient::new("https://example.org", None, Duration::from_secs(1)).unwrap();
    assert!(client.with_timeout_limit(Duration::ZERO).is_err());
    assert_eq!(
        client
            .with_timeout_limit(Duration::from_millis(10))
            .unwrap()
            .base_url(),
        client.base_url()
    );
}

#[test]
fn structured_errors_preserve_status_request_id_and_retry_hint() {
    let body=json!({"code":"publication_changed","message":"Rediscover the publication.","request_id":"request-7","details":{"expected":"old"}}).to_string();
    let (origin, worker) = serve(vec![response("409 Conflict", "Retry-After: 3\r\n", &body)]);
    let client =
        EnumerayClient::new(&origin, Some("secret".into()), Duration::from_secs(5)).unwrap();
    match client.get::<Value>("/api/v1/models") {
        Err(Error::Api {
            status,
            failure,
            retry_after,
        }) => {
            assert_eq!(status, 409);
            assert_eq!(failure.code, "publication_changed");
            assert_eq!(failure.request_id, "request-7");
            assert_eq!(retry_after.as_deref(), Some("3"));
            assert_eq!(failure.details.unwrap()["expected"], "old");
        }
        other => panic!("unexpected response: {other:?}"),
    }
    assert!(worker.join().unwrap()[0].contains("authorization: Bearer secret"));
}

#[test]
fn content_redirect_never_forwards_api_credentials() {
    let (storage, storage_worker) = serve(vec![response("200 OK", "", "content")]);
    let (origin, api_worker) = serve(vec![response(
        "303 See Other",
        &format!("Location: {storage}object?secret=capability\r\n"),
        "",
    )]);
    let client =
        EnumerayClient::new(&origin, Some("api-secret".into()), Duration::from_secs(5)).unwrap();
    let mut output = Vec::new();
    assert_eq!(
        client
            .download("/api/v1/artifacts/id/content", &mut output)
            .unwrap(),
        7
    );
    assert_eq!(&output, b"content");
    assert!(api_worker.join().unwrap()[0].contains("authorization: Bearer api-secret"));
    assert!(
        !storage_worker.join().unwrap()[0]
            .to_ascii_lowercase()
            .contains("authorization:")
    );
}

#[test]
fn regular_api_requests_do_not_follow_redirects() {
    let (origin, worker) = serve(vec![response(
        "302 Found",
        "Location: https://attacker.example/\r\n",
        "",
    )]);
    let client =
        EnumerayClient::new(&origin, Some("secret".into()), Duration::from_secs(5)).unwrap();
    assert!(matches!(client.me(), Err(Error::Api { status: 302, .. })));
    worker.join().unwrap();
}

#[test]
fn provenance_can_be_downloaded_directly_from_the_authenticated_api() {
    let (origin, worker) = serve(vec![response("200 OK", "", "{\"version\":1}")]);
    let client =
        EnumerayClient::new(&origin, Some("secret".into()), Duration::from_secs(5)).unwrap();
    let mut output = Vec::new();
    client
        .download("/api/v1/runs/id/provenance/content", &mut output)
        .unwrap();
    assert_eq!(output, b"{\"version\":1}");
    assert!(worker.join().unwrap()[0].contains("authorization: Bearer secret"));
}

#[test]
fn prepared_operation_replays_exact_publication_and_key() {
    let user = uuid::Uuid::new_v4();
    let run = uuid::Uuid::new_v4();
    let identity = json!({"user_id":user,"label":"caller","permissions":[]}).to_string();
    let receipt=json!({"run_id":run,"model_id":"model","publication_id":"original","state":"queued","created_at":"2026-10-01T00:00:00Z"}).to_string();
    let (origin, worker) = serve(vec![
        response("200 OK", "", &identity),
        response("201 Created", "", &receipt),
        response("200 OK", "", &identity),
        response("201 Created", "", &receipt),
    ]);
    let client =
        EnumerayClient::new(&origin, Some("secret".into()), Duration::from_secs(5)).unwrap();
    let prepared = PreparedRun {
        schema_version: 1,
        api_url: origin,
        user_id: user,
        idempotency_key: "durable-operation".into(),
        request: RunSubmission {
            publication_delegation_id: None,
            purpose: Default::default(),
            model_id: "model".into(),
            expected_publication_id: "original".into(),
            inputs: json!({"x":1}),
        },
    };
    let first = client.submit(&prepared).unwrap();
    assert_eq!(first.run_id, run);
    assert_eq!(first.publication_id, "original");
    assert_eq!(client.submit(&prepared).unwrap().run_id, run);
    let requests = worker.join().unwrap();
    assert_eq!(requests[1], requests[3]);
    assert!(requests[1].contains("idempotency-key: durable-operation"));
    assert!(requests[1].contains("\"expected_publication_id\":\"original\""));
    assert!(!requests[1].contains("execution_policy"));
    assert!(
        !requests
            .iter()
            .any(|request| request.starts_with("GET /api/v1/models"))
    );
}

#[test]
fn prepared_operations_reject_retired_fields_and_wrong_api_or_user() {
    let user = uuid::Uuid::new_v4();
    let request = json!({"model_id":"model","expected_publication_id":"original","inputs":{"x":1}});
    let mut retired = request.clone();
    retired["execution_policy_id"] = json!("removed");
    assert!(serde_json::from_value::<RunSubmission>(retired).is_err());

    let identity = json!({"user_id":uuid::Uuid::new_v4(),"label":"other"}).to_string();
    let (origin, worker) = serve(vec![response("200 OK", "", &identity)]);
    let client = EnumerayClient::new(&origin, None, Duration::from_secs(5)).unwrap();
    let mut prepared = PreparedRun {
        schema_version: 1,
        api_url: "https://another.invalid/".into(),
        user_id: user,
        idempotency_key: "durable-operation".into(),
        request: serde_json::from_value(request).unwrap(),
    };
    assert!(matches!(client.submit(&prepared), Err(Error::Invalid(_))));
    prepared.api_url = origin;
    prepared.schema_version = 2;
    assert!(matches!(client.submit(&prepared), Err(Error::Invalid(_))));
    prepared.schema_version = 1;
    assert!(matches!(client.submit(&prepared), Err(Error::Invalid(_))));
    assert_eq!(worker.join().unwrap().len(), 1);
}

#[test]
fn management_client_decodes_shared_records_and_preserves_pagination_and_guards() {
    use enumeray_contracts::management::{AccessMode, AccessPolicyUpdate};
    let owner = uuid::Uuid::new_v4();
    let record = json!({"connector_id":"worker","owner_id":owner,"ownership_generation":1,"label":"Lab","generation":1,"observed_generation":0,"draining":false,"authority_withdrawn":false,"max_outstanding_assignments":4,"archived_at":null});
    let page = json!({"items":[],"next_cursor":"a+b/c?="});
    let policy = json!({"mode":"request","visibility":"listed","generation":8});
    let (origin, worker) = serve(vec![
        response("200 OK", "", &record.to_string()),
        response("200 OK", "", &page.to_string()),
        response("200 OK", "", &policy.to_string()),
    ]);
    let client =
        EnumerayClient::new(&origin, Some("secret".into()), Duration::from_secs(5)).unwrap();
    let api = client.management();
    assert_eq!(api.connector("worker").unwrap().owner_id, owner);
    assert_eq!(
        api.publications("example", &[("cursor", "a+b/c?="), ("limit", "10")])
            .unwrap()
            .next_cursor
            .as_deref(),
        Some("a+b/c?=")
    );
    assert_eq!(
        api.set_access_policy(
            "example",
            &AccessPolicyUpdate {
                mode: AccessMode::Request,
                visibility: None,
                expected_generation: 7
            }
        )
        .unwrap()
        .generation,
        8
    );
    let requests = worker.join().unwrap();
    assert!(requests[1].starts_with(
        "GET /api/v1/models/example/publications?cursor=a%2Bb%2Fc%3F%3D&limit=10 HTTP/1.1"
    ));
    let body: Value = serde_json::from_str(requests[2].split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["expected_generation"], 7);
}

#[test]
fn part_transfers_use_their_bulk_budget_without_forwarding_api_credentials() {
    use enumeray_client::ScopedTransfer;
    let path = std::env::temp_dir().join(format!("enumeray-bulk-budget-{}", uuid::Uuid::new_v4()));
    std::fs::write(&path, b"abc").unwrap();
    let (origin, worker) = serve_delayed(
        vec![response("200 OK", "", ""), response("200 OK", "", "")],
        Duration::from_millis(250),
    );
    let client = EnumerayClient::new(
        &origin,
        Some("private-api-token".into()),
        Duration::from_millis(50),
    )
    .unwrap();
    let transfer = ScopedTransfer {
        method: "PUT".into(),
        url: origin,
        headers: vec![],
    };
    client
        .transfer_part(
            &transfer,
            std::fs::File::open(&path).unwrap(),
            0,
            3,
            Duration::from_secs(3),
        )
        .unwrap();
    assert!(
        client
            .transfer_part(
                &transfer,
                std::fs::File::open(&path).unwrap(),
                0,
                3,
                Duration::from_millis(100)
            )
            .is_err()
    );
    let requests = worker.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|wire| !wire.to_ascii_lowercase().contains("authorization:")
                && !wire.contains("private-api-token")
                && wire.ends_with("abc"))
    );
    std::fs::remove_file(path).unwrap();
}

struct RenewableCredentials {
    origin: String,
    calls: std::sync::atomic::AtomicUsize,
    delay: Duration,
    finished: std::sync::atomic::AtomicBool,
    renewal_deadline: Option<std::time::Instant>,
    cancel_on_acquire: Option<enumeray_client::CancellationToken>,
}

impl RenewableCredentials {
    fn new(origin: &str) -> Self {
        Self {
            origin: origin.into(),
            calls: 0.into(),
            delay: Duration::ZERO,
            finished: false.into(),
            renewal_deadline: None,
            cancel_on_acquire: None,
        }
    }
}

impl enumeray_client::CredentialSource for RenewableCredentials {
    fn acquire(
        &self,
        origin: &str,
        deadline: std::time::Instant,
    ) -> Result<enumeray_client::CredentialLease, Error> {
        use std::sync::atomic::Ordering;
        assert_eq!(origin, self.origin);
        assert!(deadline > std::time::Instant::now());
        let generation = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        thread::sleep(self.delay);
        if let Some(cancel) = &self.cancel_on_acquire {
            cancel.cancel();
        }
        self.finished.store(true, Ordering::SeqCst);
        Ok(enumeray_client::CredentialLease::new(
            Some(format!("lease-{generation}")),
            self.renewal_deadline,
        ))
    }
}

#[test]
fn renewable_credentials_are_shared_by_clones_and_observers_without_replaying_mutations() {
    use std::sync::{Arc, atomic::Ordering};
    let identity = json!({"user_id":uuid::Uuid::nil(),"label":"same account"}).to_string();
    let (origin, worker) = serve(vec![
        response("200 OK", "", "{}"),
        response("200 OK", "", &identity),
        response(
            "401 Unauthorized",
            "",
            r#"{"code":"unauthenticated","message":"denied"}"#,
        ),
    ]);
    let source = Arc::new(RenewableCredentials::new(&origin));
    let client =
        EnumerayClient::with_credentials(&origin, source.clone(), Duration::from_secs(5)).unwrap();
    client.clone().get::<Value>("/api/v1/models").unwrap();
    let observer = client.observation_client().unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(observer.me())
        .unwrap();
    assert!(matches!(
        client.request::<Value, _>(
            reqwest::Method::POST,
            "/api/v1/runs",
            Some(&json!({"original":"body"})),
            Some("original-key")
        ),
        Err(Error::Api { status: 401, .. })
    ));
    assert_eq!(source.calls.load(Ordering::SeqCst), 3);
    let requests = worker.join().unwrap();
    for (index, request) in requests.iter().enumerate() {
        assert!(request.contains(&format!("authorization: Bearer lease-{}", index + 1)));
    }
    assert!(requests[2].contains("idempotency-key: original-key"));
    assert!(requests[2].ends_with(r#"{"original":"body"}"#));
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.starts_with("POST"))
            .count(),
        1
    );
}

#[test]
fn renewal_and_cancellation_consume_the_same_absolute_request_budget() {
    use std::sync::{Arc, atomic::Ordering};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    let mut slow = RenewableCredentials::new(&origin);
    slow.delay = Duration::from_millis(80);
    let slow = Arc::new(slow);
    let client = EnumerayClient::with_credentials(&origin, slow.clone(), Duration::from_secs(5))
        .unwrap()
        .with_deadline(std::time::Instant::now() + Duration::from_millis(20));
    assert!(
        matches!(client.get::<Value>("/api/v1/models"), Err(Error::Invalid(message)) if message.contains("deadline"))
    );
    assert_eq!(slow.calls.load(Ordering::SeqCst), 1);
    let cancellation = enumeray_client::CancellationToken::new();
    let mut source = RenewableCredentials::new(&origin);
    source.cancel_on_acquire = Some(cancellation.clone());
    let source = Arc::new(source);
    let client = EnumerayClient::with_credentials(&origin, source.clone(), Duration::from_secs(5))
        .unwrap()
        .with_cancellation(cancellation);
    assert!(
        matches!(client.post::<Value, _>("/api/v1/tokens", &json!({})), Err(Error::Invalid(message)) if message.contains("cancelled"))
    );
    assert_eq!(source.calls.load(Ordering::SeqCst), 1);
    assert!(client.get::<Value>("/api/v1/models").is_err());
    assert_eq!(
        source.calls.load(Ordering::SeqCst),
        1,
        "cancelled clients must not acquire again"
    );
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );

    let source = Arc::new(RenewableCredentials::new(&origin));
    let client =
        EnumerayClient::with_credentials(&origin, source.clone(), Duration::from_secs(5)).unwrap();
    assert!(
        client
            .get::<Value>("https://other.invalid/api/v1/models")
            .is_err()
    );
    assert!(
        client
            .request::<Value, Value>(
                reqwest::Method::POST,
                "/api/v1/runs",
                None,
                Some("invalid/key")
            )
            .is_err()
    );
    assert!(
        client
            .download_location("https://other.invalid/api/v1/artifacts/a/content")
            .is_err()
    );
    assert_eq!(
        source.calls.load(Ordering::SeqCst),
        0,
        "invalid requests must not begin renewal"
    );
}

#[test]
fn asynchronous_acquisition_inherits_absolute_deadlines_without_dispatching_late_requests() {
    use std::sync::Arc;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    let mut source = RenewableCredentials::new(&origin);
    source.delay = Duration::from_millis(150);
    let client =
        EnumerayClient::with_credentials(&origin, Arc::new(source), Duration::from_secs(5))
            .unwrap()
            .with_deadline(std::time::Instant::now() + Duration::from_millis(50));
    let observer = client.observation_client().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let error = runtime
        .block_on(observer.get::<Value>("/api/v1/models"))
        .unwrap_err();
    assert!(error.to_string().contains("deadline"));
    runtime.shutdown_timeout(Duration::from_secs(1));
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn asynchronous_acquisition_keeps_observation_timers_responsive() {
    use std::sync::{Arc, atomic::Ordering};
    let (origin, worker) = serve(vec![response("200 OK", "", "{}")]);
    let mut source = RenewableCredentials::new(&origin);
    source.delay = Duration::from_millis(150);
    let source = Arc::new(source);
    let client =
        EnumerayClient::with_credentials(&origin, source.clone(), Duration::from_secs(5)).unwrap();
    let observer = client.observation_client().unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (result, _) = tokio::join!(observer.get::<Value>("/api/v1/models"), async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                assert!(
                    !source.finished.load(Ordering::SeqCst),
                    "credential work blocked the runtime timer"
                );
            });
            result.unwrap();
        });
    worker.join().unwrap();
}

#[test]
fn renewable_sse_captures_its_handshake_lease_and_preserves_revocation_events() {
    use std::sync::Arc;
    let body = "event: auth_revoked\ndata: {\"code\":\"auth_revoked\"}\n\n";
    let (origin, worker) = serve(vec![format!(
        "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )]);
    let mut source = RenewableCredentials::new(&origin);
    let renewal = std::time::Instant::now() + Duration::from_secs(10);
    source.renewal_deadline = Some(renewal);
    let client =
        EnumerayClient::with_credentials(&origin, Arc::new(source), Duration::from_secs(5))
            .unwrap();
    let observer = client.observation_client().unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut stream = observer
                .log_stream("run", Some("processed-cursor"))
                .await
                .unwrap();
            assert_eq!(stream.renewal_deadline(), Some(renewal));
            assert!(matches!(
                stream.next().await.unwrap().unwrap().data,
                enumeray_client::observation::LogStreamData::AuthRevoked
            ));
        });
    let requests = worker.join().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].contains("last-event-id: processed-cursor"));
    assert!(requests[0].contains("authorization: Bearer lease-1"));
}

#[test]
fn renewable_downloads_acquire_each_api_capability_without_leaking_bearers() {
    use std::sync::Arc;
    let (storage, storage_worker) = serve(vec![response("200 OK", "", "content")]);
    let (origin, worker) = serve(vec![
        response(
            "303 See Other",
            &format!("Location: {storage}object\r\n"),
            "",
        ),
        response("200 OK", "", "provenance"),
        response(
            "303 See Other",
            &format!("Location: {storage}object\r\n"),
            "",
        ),
    ]);
    let client = EnumerayClient::with_credentials(
        &origin,
        Arc::new(RenewableCredentials::new(&origin)),
        Duration::from_secs(5),
    )
    .unwrap();
    assert_eq!(
        client
            .download_location("/api/v1/artifacts/a/content")
            .unwrap(),
        format!("{storage}object")
    );
    let mut output = Vec::new();
    client
        .download("/api/v1/runs/r/provenance/content", &mut output)
        .unwrap();
    assert_eq!(output, b"provenance");
    output.clear();
    client
        .download("/api/v1/artifacts/a/content", &mut output)
        .unwrap();
    assert_eq!(output, b"content");
    for (index, request) in worker.join().unwrap().iter().enumerate() {
        assert!(request.contains(&format!("authorization: Bearer lease-{}", index + 1)));
    }
    assert!(
        !storage_worker.join().unwrap()[0]
            .to_ascii_lowercase()
            .contains("authorization:")
    );
}
