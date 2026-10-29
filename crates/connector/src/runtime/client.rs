//! A bounded HTTP client for the local connector, with no database access.

use std::{collections::HashSet, fs::File, io::Read, path::Path, time::Duration};

use enumeray_contracts::{
    assignments::inspection::{AssignmentDetail, AssignmentList, AssignmentSummary},
    assignments::sessions::SessionReceipt,
    runs::RunState,
};
use reqwest::{
    Method, StatusCode,
    blocking::Client as HttpClient,
    header::{AUTHORIZATION, HeaderValue},
};
use serde_json::{Value, json};
use url::{Host, Url};
use uuid::Uuid;

use super::{
    Result, failure,
    journal::{self, Journal},
};

const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;

fn heartbeat_body() -> Value {
    json!({
        "connector_version": concat!("enumeray/", env!("CARGO_PKG_VERSION")),
        "capabilities": ["json_inputs", "json_outputs", "artifact_inputs", "artifact_outputs", "large_artifacts", "collection_inputs", "collection_outputs"],
        "wrapper_protocol_versions": [1]
    })
}

#[derive(Clone)]
pub struct Client {
    http: HttpClient,
    base_url: String,
    authorization: HeaderValue,
    pub log_readers: super::logs::Readers,
    pub log_policy: enumeray_contracts::logs::LogPolicy,
    pub service: bool,
    pub allow_development_execution: bool,
    pub unpublished_retention_seconds: Option<u64>,
    pub shutdown: std::sync::Arc<std::sync::Mutex<Option<std::time::Instant>>>,
}

impl Client {
    pub fn origin(&self) -> &str {
        &self.base_url
    }
    pub fn configured(base_url: &str, token_file: Option<&Path>, allow_http: bool) -> Result<Self> {
        let base_url = validate_origin(base_url, allow_http)?;
        let mut token = Vec::new();
        let input: Box<dyn Read> = if let Some(path) = token_file {
            journal::check_private_path(path, false)?;
            Box::new(File::open(path).map_err(|_| failure("cannot open token file"))?)
        } else {
            Box::new(std::io::stdin())
        };
        input
            .take(257)
            .read_to_end(&mut token)
            .map_err(|_| failure("cannot read connector token"))?;
        if token.len() > 256 {
            return Err(failure("connector token input exceeds 256 bytes"));
        }
        let token = std::str::from_utf8(&token)
            .map_err(|_| failure("invalid connector token"))?
            .trim_end_matches(['\r', '\n']);
        if token.is_empty()
            || token.len() > 256
            || !token.bytes().all(|byte| (b'!'..=b'~').contains(&byte))
        {
            return Err(failure(
                "read one connector token, without spaces, from stdin or a private token file",
            ));
        }
        let mut authorization = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| failure("invalid connector token"))?;
        authorization.set_sensitive(true);
        let http = HttpClient::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| failure("cannot initialize HTTP client"))?;
        Ok(Self {
            http,
            base_url,
            authorization,
            log_readers: Default::default(),
            log_policy: Default::default(),
            service: false,
            allow_development_execution: false,
            unpublished_retention_seconds: None,
            shutdown: Default::default(),
        })
    }

    pub fn transfer_location(&self, path: &str) -> Result<String> {
        let response = self
            .http
            .get(format!("{}{path}", self.base_url))
            .header(AUTHORIZATION, self.authorization.clone())
            .send()
            .map_err(|_| failure("artifact capability unavailable"))?;
        if response.status() != StatusCode::SEE_OTHER {
            return Err(http_error("artifact download", response.status()));
        }
        response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
            .ok_or_else(|| failure("artifact capability missing"))
    }

    pub fn assignment_control(
        &self,
        id: Uuid,
    ) -> Result<enumeray_contracts::execution::AssignmentControl> {
        let (status, bytes) = self.request(
            Method::GET,
            &format!("/connector/v1/assignments/{id}/control"),
            None,
            None,
            None,
        )?;
        if status != StatusCode::OK {
            return Err(http_error("assignment control", status));
        }
        serde_json::from_slice(&bytes).map_err(|_| failure("invalid assignment control"))
    }

    pub fn require_launch(&self, id: Uuid) -> Result<()> {
        let control = self.assignment_control(id)?;
        if !control.allow_launch {
            return Err(failure("assignment authorization revoked before launch"));
        }
        Ok(())
    }

    pub fn claim_once(
        &self,
        journal: &mut Journal,
        credentials_file: Option<&Path>,
    ) -> Result<Value> {
        if journal.ownership().state == journal::OwnershipState::NoSession
            && journal.ownership().pending_claim_key.is_none()
        {
            // One-off execution leaves its receipt available for inspection.
            // Archive safely delivered entries before reserving capacity for
            // the next claim, just as the continuous service does.
            journal.archive_completed()?;
        }
        let session_id = journal.begin_session()?;
        self.register_session(journal, session_id, false)?;
        self.reconcile_inventory(journal)?;
        if journal.has_unknown_submissions() {
            return Err(failure(
                "journal contains unknown submission outcomes; preserve it for evidence-based resolution",
            ));
        }
        crate::control::reconcile(self, &journal.directory, credentials_file)?;
        let (status, _) = self.request(
            Method::PUT,
            "/connector/v1/heartbeat",
            Some(session_id),
            None,
            Some(&heartbeat_body()),
        )?;
        if status != StatusCode::OK {
            return Err(http_error("heartbeat", status));
        }
        let outcome = self.claim(journal, session_id)?;
        self.release_session(journal, session_id)?;
        Ok(outcome)
    }

    pub fn claim(&self, journal: &mut Journal, session_id: Uuid) -> Result<Value> {
        // A live owner retries the exact pending request, never a fresh claim.
        let retrying = journal.data.pending_claim_key.is_some();
        let claim_key = journal.begin_claim(session_id)?;
        let response = self.request(
            Method::POST,
            "/connector/v1/assignments",
            Some(session_id),
            Some(claim_key),
            Some(&json!({})),
        );
        let (status, response) = match response {
            Ok(response) => response,
            Err(error) => {
                // The service makes one request attempt. These guards run
                // before sending it, so its newly reserved key delegated no
                // ownership and must not strand shutdown draining. An older
                // key may have reached the API and still needs reconciliation.
                if self.service
                    && !retrying
                    && matches!(
                        error.kind,
                        super::ErrorKind::Waiting | super::ErrorKind::Stopping
                    )
                {
                    journal.complete_empty_claim(session_id, claim_key)?;
                }
                return Err(error);
            }
        };
        match status {
            StatusCode::CREATED => {
                journal.prepare_claim_assignment(session_id, claim_key, parse_json(&response)?)
            }
            StatusCode::NO_CONTENT => {
                journal.complete_empty_claim(session_id, claim_key)?;
                Ok(json!({"status":"empty"}))
            }
            _ => Err(http_error("assignment claim", status)),
        }
    }

    pub fn heartbeat(&self) -> Result<Duration> {
        let (status, bytes) = self.request(
            Method::PUT,
            "/connector/v1/heartbeat",
            None,
            None,
            Some(&heartbeat_body()),
        )?;
        if status != StatusCode::OK {
            return Err(http_error("heartbeat", status));
        }
        let receipt = parse_json(&bytes)?;
        let interval = receipt["heartbeat_interval_seconds"]
            .as_u64()
            .filter(|value| (1..=60).contains(value))
            .ok_or_else(|| failure("invalid heartbeat interval"))?;
        Ok(Duration::from_secs(interval))
    }

    pub fn register_session(
        &self,
        journal: &Journal,
        session_id: Uuid,
        allow_closed: bool,
    ) -> Result<()> {
        let (status, response) = self.request(
            Method::POST,
            "/connector/v1/sessions",
            None,
            None,
            Some(&json!({
                "journal_id": journal.data.journal_id, "session_id": session_id
            })),
        )?;
        if allow_closed && status == StatusCode::CONFLICT {
            let error = parse_json(&response)?;
            if error["code"] == "session_closed" {
                return Ok(());
            }
        }
        if status != StatusCode::OK {
            return Err(http_error("session registration", status));
        }
        let registered: SessionReceipt = parse_response(&response)?;
        if registered.connector_id != journal.data.connector_id
            || registered.journal_id != journal.data.journal_id
            || registered.session_id != session_id
        {
            return Err(failure(
                "session response does not match this connector journal; preserve ownership for reconciliation",
            ));
        }
        Ok(())
    }

    pub fn reconcile(&self, journal: &mut Journal, prior_stopped: bool) -> Result<Value> {
        let session_id = self.acquire_recovery_session(journal, prior_stopped)?;
        self.reconcile_inventory(journal)?;
        // No claim retry: an empty/never-committed key could delegate new work.
        // The old session is fenced, so its delayed request cannot claim now.
        journal.inventory_reconciled(session_id)?;
        self.release_session(journal, session_id)?;
        let mut summary = journal.summary();
        summary["status"] = json!(if journal.has_unknown_submissions() {
            "reconciliation_required"
        } else {
            "reconciled"
        });
        Ok(summary)
    }

    pub fn execute_once(
        &self,
        journal: &mut Journal,
        prior_stopped: bool,
        lose_ack: bool,
        credentials_file: Option<&Path>,
    ) -> Result<Value> {
        let session_id = self.acquire_recovery_session(journal, prior_stopped)?;
        self.reconcile_inventory(journal)?;
        journal.inventory_reconciled(session_id)?;
        let fresh_assignment = journal
            .data
            .prepared_assignments
            .iter()
            .find(|entry| entry.terminal_result.is_none())
            .filter(|entry| {
                entry.submission_state() == journal::SubmissionState::DefinitelyUnsubmitted
            })
            .map(|entry| entry.assignment.decoded().assignment_id);
        let needs_configuration = match fresh_assignment {
            Some(id) => !self.assignment_control(id)?.cancel_running,
            None => false,
        };
        let configuration = if needs_configuration {
            Some(crate::control::reconcile(
                self,
                &journal.directory,
                credentials_file,
            )?)
        } else {
            None
        };
        let outcome = super::execution::execute(
            self,
            journal,
            session_id,
            lose_ack,
            configuration.as_deref(),
        )?;
        super::execution::complete_deliveries(self, journal, session_id)?;
        self.release_session(journal, session_id)?;
        Ok(outcome)
    }

    pub fn acquire_recovery_session(
        &self,
        journal: &mut Journal,
        prior_stopped: bool,
    ) -> Result<Uuid> {
        let session_id = if let Some(previous_session_id) = journal.data.active_session_id {
            if !prior_stopped {
                return Err(failure(
                    "recovery requires --confirm-prior-process-stopped after verifying the prior process has stopped on every host",
                ));
            }
            if journal.data.pending_recovery.is_none() {
                // The old registration may never have reached the API, or its
                // release may have committed without a response. Neither proves
                // anything about scientific execution.
                self.register_session(journal, previous_session_id, true)?;
            }
            let recovery = journal.prepare_recovery(previous_session_id)?;
            let (status, response) = self.request(
                Method::POST,
                "/connector/v1/sessions/recover",
                None,
                None,
                Some(&json!({
                    "journal_id": journal.data.journal_id,
                    "previous_session_id": recovery.previous_session_id,
                    "session_id": recovery.session_id,
                    "prior_process_stopped": true
                })),
            )?;
            if status != StatusCode::OK {
                return Err(http_error("session recovery", status));
            }
            let receipt: SessionReceipt = parse_response(&response)?;
            if receipt.connector_id != journal.data.connector_id
                || receipt.journal_id != journal.data.journal_id
                || receipt.session_id != recovery.session_id
            {
                return Err(failure(
                    "recovery response does not match the persisted session identity",
                ));
            }
            journal.complete_recovery(recovery.previous_session_id, recovery.session_id)?;
            recovery.session_id
        } else {
            let session_id = journal.begin_reconciliation_session()?;
            self.register_session(journal, session_id, false)?;
            session_id
        };
        Ok(session_id)
    }

    pub fn reconcile_inventory(&self, journal: &mut Journal) -> Result<()> {
        let mut cursor: Option<String> = None;
        let mut cursors = HashSet::new();
        let mut seen = HashSet::new();
        // Fetch one bounded specification at a time. The local journal supports
        // at most 64 records; an oversized/repeating inventory fails closed.
        for page_number in 0..=64 {
            let mut query = url::form_urlencoded::Serializer::new(String::new());
            query.append_pair("limit", "1");
            if let Some(cursor) = &cursor {
                query.append_pair("cursor", cursor);
            }
            let (status, bytes) = self.request(
                Method::GET,
                &format!("/connector/v1/assignments?{}", query.finish()),
                None,
                None,
                None,
            )?;
            if status != StatusCode::OK {
                return Err(http_error("assignment listing", status));
            }
            let page = parse_json(&bytes)?;
            let decoded: AssignmentList = serde_json::from_value(page.clone())
                .map_err(|_| failure("invalid assignment inventory"))?;
            let items = &decoded.items;
            if items.len() > 1 {
                return Err(failure("invalid assignment inventory"));
            }
            for item in items {
                let id = item.assignment_id;
                if id.is_nil()
                    || item.journal_id != journal.data.journal_id
                    || (item.state == RunState::Running) != item.phase.is_some()
                {
                    return Err(failure("invalid assignment inventory identity or progress"));
                }
                if !seen.insert(id) {
                    return Err(failure("assignment inventory repeats an identity"));
                }
                self.reconcile_detail(journal, id, Some(item))?;
            }
            match page.get("next_cursor") {
                Some(Value::Null) => break,
                Some(Value::String(next))
                    if !next.is_empty()
                        && next.len() <= 2048
                        && !items.is_empty()
                        && page_number < 64
                        && cursors.insert(next.clone()) =>
                {
                    cursor = Some(next.clone())
                }
                _ => return Err(failure("invalid or excessive assignment pagination")),
            }
        }
        let local_ids: Vec<Uuid> = journal
            .data
            .prepared_assignments
            .iter()
            .map(|entry| entry.assignment.decoded().assignment_id)
            .collect();
        for id in local_ids {
            if !seen.contains(&id) {
                self.reconcile_detail(journal, id, None)?;
            }
        }
        Ok(())
    }

    fn reconcile_detail(
        &self,
        journal: &mut Journal,
        id: Uuid,
        expected: Option<&AssignmentSummary>,
    ) -> Result<()> {
        let (status, bytes) = self.request(
            Method::GET,
            &format!("/connector/v1/assignments/{id}"),
            None,
            None,
            None,
        )?;
        if status != StatusCode::OK {
            return Err(http_error("assignment retrieval", status));
        }
        let detail = parse_json(&bytes)?;
        let decoded: AssignmentDetail = serde_json::from_value(detail.clone())
            .map_err(|_| failure("invalid assignment detail"))?;
        if (decoded.state == RunState::Running) != decoded.phase.is_some() {
            return Err(failure("invalid assignment detail progress"));
        }
        if decoded.state == RunState::Running
            && detail
                .get("resolution")
                .is_some_and(|resolution| !resolution.is_null())
        {
            return Err(failure(
                "running assignment contains conflicting operator resolution evidence",
            ));
        }
        if expected.is_some_and(|summary| {
            summary.run_id != decoded.assignment.run_id
                || summary.attempt_id != decoded.assignment.attempt_id
                || summary.journal_id != decoded.assignment.journal_id
        }) {
            return Err(failure(
                "assignment detail conflicts with its inventory identity",
            ));
        }
        if decoded.assignment.assignment_id != id {
            return Err(failure(
                "assignment detail does not match the requested identity",
            ));
        }
        if decoded.state == RunState::Running
            && journal.data.prepared_assignments.iter().any(|entry| {
                entry.assignment.decoded().assignment_id == id && entry.terminal_result.is_some()
            })
        {
            return Err(failure(
                "server progress conflicts with the durable local terminal receipt; preserve both histories for reconciliation",
            ));
        }
        if decoded.state == RunState::Running && decoded.delivery_completion.is_some() {
            return Err(failure(
                "running assignment contains delivery completion evidence",
            ));
        }
        journal.reconcile_assignment(detail["assignment"].clone())?;
        if decoded.state != RunState::Running {
            super::execution::reconcile_terminal(journal, id, &detail)?;
            if let Some(receipt) = decoded.delivery_completion {
                super::execution::reconcile_delivery(journal, id, receipt)?;
            }
        } else if decoded.backend_handle.is_some() {
            journal.observe_backend_evidence(id)?;
        }
        Ok(())
    }

    pub fn release_session(&self, journal: &mut Journal, session_id: Uuid) -> Result<()> {
        journal.require_session(session_id)?;
        if journal.ownership().pending_claim_key.is_some() {
            return Err(failure("cannot release a session with an unresolved claim"));
        }
        let (status, _) = self.request(
            Method::DELETE,
            &format!("/connector/v1/sessions/{session_id}"),
            None,
            None,
            None,
        )?;
        if status != StatusCode::NO_CONTENT {
            return Err(http_error("session closure", status));
        }
        journal.complete_session(session_id)
    }

    pub fn request(
        &self,
        method: Method,
        path: &str,
        session_id: Option<Uuid>,
        claim_key: Option<Uuid>,
        body: Option<&Value>,
    ) -> Result<(StatusCode, Vec<u8>)> {
        let attempts = if self.service { 1 } else { 3 };
        for attempt in 0..attempts {
            let deadline = *self.shutdown.lock().expect("shutdown lock");
            let remaining =
                deadline.map(|d| d.saturating_duration_since(std::time::Instant::now()));
            if self.service
                && deadline.is_some()
                && method == Method::POST
                && path == "/connector/v1/assignments"
            {
                return Err(
                    failure("new claims stopped for shutdown").with_kind(super::ErrorKind::Waiting)
                );
            }
            if remaining.is_some_and(|d| d.is_zero()) {
                return Err(failure("shutdown drain deadline reached")
                    .with_kind(super::ErrorKind::Stopping));
            }
            let mut request = self
                .http
                .request(method.clone(), format!("{}{path}", self.base_url))
                .header(AUTHORIZATION, self.authorization.clone());
            if let Some(session_id) = session_id {
                request = request.header("X-Connector-Session", session_id.to_string());
            }
            if let Some(claim_key) = claim_key {
                request = request.header("Idempotency-Key", claim_key.to_string());
            }
            if let Some(body) = body {
                request = request.json(body);
            }
            if let Some(remaining) = remaining {
                request = request.timeout(remaining.min(Duration::from_secs(5)));
            }
            let response = request.send();
            if let Ok(mut response) = response {
                let status = response.status();
                let mut bytes = Vec::new();
                match response
                    .by_ref()
                    .take(MAX_RESPONSE_BYTES + 1)
                    .read_to_end(&mut bytes)
                {
                    Ok(_) if bytes.len() as u64 > MAX_RESPONSE_BYTES => {
                        return Err(failure(
                            "HTTP response exceeds 4 MiB; preserve journal for reconciliation",
                        ));
                    }
                    Ok(_)
                        if status != StatusCode::SERVICE_UNAVAILABLE || attempt + 1 == attempts =>
                    {
                        return Ok((status, bytes));
                    }
                    _ => (),
                }
            }
            if attempt + 1 < attempts {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        Err(
            failure("HTTP outcome is uncertain; preserve the journal and retry the same request")
                .with_kind(super::ErrorKind::Retry),
        )
    }
}

pub fn validate_api_url(value: &str) -> Result<String> {
    validate_origin(value, false)
}

pub fn validate_origin(value: &str, allow_http: bool) -> Result<String> {
    let invalid = || {
        failure(
            "API base URL must be an HTTPS origin or an HTTP loopback origin; non-loopback HTTP requires --allow-insecure-http",
        )
    };
    let url = Url::parse(value).map_err(|_| invalid())?;
    let loopback = match url.host() {
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        Some(Host::Domain("localhost")) => true,
        _ => false,
    };
    if url.host().is_none()
        || !(url.scheme() == "https" || (url.scheme() == "http" && (loopback || allow_http)))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid());
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

pub fn parse_json(bytes: &[u8]) -> Result<Value> {
    serde_json::from_slice(bytes).map_err(|_| {
        failure("HTTP response is not valid JSON; preserve journal for reconciliation")
    })
}

pub fn http_error(operation: &str, status: StatusCode) -> super::ConnectorError {
    let kind = if status.is_server_error() || matches!(status.as_u16(), 408 | 429) {
        super::ErrorKind::Retry
    } else {
        super::ErrorKind::Fatal
    };
    failure(format!("{operation} returned HTTP {}; preserve the journal and reconcile its process session before restarting", status.as_u16())).with_kind(kind)
}

fn parse_response<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    serde_json::from_slice(bytes).map_err(|_| failure("invalid connector response"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn shutdown_before_claim_send_releases_only_a_newly_reserved_request_key() {
        use std::os::unix::fs::PermissionsExt;

        for retrying in [false, true] {
            let path =
                std::env::temp_dir().join(format!("enumeray-stopped-claim-{}", Uuid::new_v4()));
            let mut journal =
                Journal::initialize(&path, "http://127.0.0.1:9".into(), "test-connector".into())
                    .unwrap();
            let token = path.join("token");
            std::fs::write(&token, "test-connector-token").unwrap();
            std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
            let mut client =
                Client::configured(&journal.data.api_base_url, Some(&token), false).unwrap();
            client.service = true;
            *client.shutdown.lock().unwrap() = Some(std::time::Instant::now());
            let session = journal.begin_session().unwrap();
            let pending = retrying.then(|| journal.begin_claim(session).unwrap());
            let error = client.claim(&mut journal, session).unwrap_err();
            assert_eq!(error.kind, super::super::ErrorKind::Waiting);
            assert_eq!(journal.data.pending_claim_key, pending);
            assert_eq!(journal.data.active_session_id, Some(session));
            drop(journal);
            let journal = Journal::open(&path).unwrap();
            assert_eq!(journal.data.pending_claim_key, pending);
            assert_eq!(journal.data.active_session_id, Some(session));
            drop(journal);
            std::fs::remove_dir_all(path).unwrap();
        }
    }

    #[test]
    fn origins_require_explicit_private_network_http_and_reject_credential_urls() {
        for origin in [
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
            "https://api.example.org",
        ] {
            assert!(validate_origin(origin, false).is_ok());
        }
        assert!(validate_origin("http://api:8080", false).is_err());
        assert_eq!(
            validate_origin("http://api:8080/", true).unwrap(),
            "http://api:8080"
        );
        for origin in [
            "https://secret@api",
            "https://api/path",
            "https://api?token=secret",
            "https://api/#fragment",
            "file:///tmp/api",
        ] {
            assert!(validate_origin(origin, true).is_err());
        }
        assert_eq!(
            http_error("test", StatusCode::UNAUTHORIZED).kind,
            super::super::ErrorKind::Fatal
        );
        assert_eq!(
            http_error("test", StatusCode::TOO_MANY_REQUESTS).kind,
            super::super::ErrorKind::Retry
        );
        assert_eq!(
            http_error("test", StatusCode::BAD_GATEWAY).kind,
            super::super::ErrorKind::Retry
        );
    }
}
