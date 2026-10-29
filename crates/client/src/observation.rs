//! Cancellable read-only observation and bounded SSE decoding. No background producer.
use crate::credentials::{Credentials, RequestBudget};
use crate::{ApiFailure, CancellationToken, Error, Identity, api_url, resource_path};
use enumeray_contracts::logs::{LogGap, LogSource};
use reqwest::{Response, redirect::Policy};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use url::Url;
use uuid::Uuid;

const MAX_EVENT_BYTES: usize = 64 * 1024;

#[derive(Clone, Deserialize, Serialize)]
pub struct LogEntry {
    pub position: i64,
    pub assignment_id: Uuid,
    pub source: LogSource,
    pub epoch: i64,
    pub sequence: i64,
    pub offset: i64,
    pub byte_length: i64,
    pub text: String,
    pub gap: Option<LogGap>,
    pub source_reset: bool,
    pub received_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RetentionBoundary {
    pub earliest_position: Option<i64>,
    pub latest_position: i64,
    pub recovery_cursor: String,
    pub payload_max_age_seconds: Option<i64>,
    pub payload_max_entries: Option<i64>,
}

#[derive(Deserialize)]
pub struct LogHistory {
    pub items: Vec<LogEntry>,
    pub next_cursor: Option<String>,
    pub resume_cursor: String,
    pub retention: RetentionBoundary,
}

pub enum LogStreamData {
    Log(LogEntry),
    RetentionGap(RetentionBoundary),
    AuthRevoked,
    SourceUnavailable,
    Shutdown,
}

pub struct LogStreamEvent {
    pub id: Option<String>,
    pub data: LogStreamData,
}

impl LogStreamEvent {
    pub fn name(&self) -> &'static str {
        match self.data {
            LogStreamData::Log(_) => "log",
            LogStreamData::RetentionGap(_) => "retention_gap",
            LogStreamData::AuthRevoked => "auth_revoked",
            LogStreamData::SourceUnavailable => "source_unavailable",
            LogStreamData::Shutdown => "shutdown",
        }
    }

    pub fn data(&self) -> Value {
        match &self.data {
            LogStreamData::Log(entry) => serde_json::to_value(entry).expect("log entry"),
            LogStreamData::RetentionGap(boundary) => {
                serde_json::to_value(boundary).expect("boundary")
            }
            _ => json!({"code": self.name()}),
        }
    }
}

#[derive(Debug)]
pub enum ObservationError {
    Request(Error),
    RetentionGap(RetentionBoundary),
}

impl From<Error> for ObservationError {
    fn from(error: Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::Error> for ObservationError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error.into())
    }
}

impl std::fmt::Display for ObservationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request(error) => error.fmt(formatter),
            Self::RetentionGap(_) => formatter.write_str(
                "Log history precedes retained payloads; explicit gap acknowledgement is required.",
            ),
        }
    }
}

impl std::error::Error for ObservationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Request(error) => Some(error),
            _ => None,
        }
    }
}

/// An asynchronous companion to the blocking client. Credentials stay on the API origin.
#[derive(Clone)]
pub struct ObservationClient {
    base: Url,
    credentials: Credentials,
    http: reqwest::Client,
    timeout: Duration,
    cancellation: CancellationToken,
    deadline: Option<Instant>,
}

impl ObservationClient {
    pub(super) fn new(
        base: Url,
        credentials: Credentials,
        timeout: Duration,
        cancellation: CancellationToken,
        deadline: Option<Instant>,
    ) -> Result<Self, Error> {
        let http = reqwest::Client::builder()
            .connect_timeout(timeout.min(Duration::from_secs(15)))
            .read_timeout(timeout)
            .redirect(Policy::none())
            .user_agent(concat!("enumeray-client/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            base,
            credentials,
            http,
            timeout,
            cancellation,
            deadline,
        })
    }

    fn request(&self, path: &str, bearer: Option<&str>) -> Result<reqwest::RequestBuilder, Error> {
        let mut request = self.http.get(api_url(&self.base, path)?);
        if let Some(token) = bearer {
            request = request.bearer_auth(token);
        }
        Ok(request)
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ObservationError> {
        let budget = RequestBudget::new(self.timeout, &self.cancellation, self.deadline)?;
        let mut request = self.request(path, None)?;
        let lease = self
            .credentials
            .acquire_async(self.base.as_str(), budget.deadline, &self.cancellation)
            .await?;
        if let Some(token) = &lease.bearer {
            request = request.bearer_auth(token);
        }
        let response = request.timeout(budget.remaining()?).send().await?;
        let response = checked_response(response).await?;
        let bytes = bounded_body(response).await?;
        serde_json::from_slice(&bytes).map_err(|error| Error::from(error).into())
    }

    pub async fn me(&self) -> Result<Identity, ObservationError> {
        self.get("/api/v1/me").await
    }

    pub async fn run(&self, id: &str) -> Result<Value, ObservationError> {
        self.get(&resource_path(&["runs", id])?).await
    }

    pub async fn logs(
        &self,
        id: &str,
        cursor: Option<&str>,
    ) -> Result<LogHistory, ObservationError> {
        let mut url = api_url(&self.base, &resource_path(&["runs", id, "logs"])?)?;
        url.query_pairs_mut().append_pair("limit", "100");
        if let Some(cursor) = cursor {
            valid_cursor(cursor)?;
            url.query_pairs_mut().append_pair("cursor", cursor);
        }
        self.get(&format!(
            "{}?{}",
            url.path(),
            url.query().unwrap_or_default()
        ))
        .await
    }

    pub async fn log_stream(
        &self,
        id: &str,
        cursor: Option<&str>,
    ) -> Result<LogStream, ObservationError> {
        let budget = RequestBudget::new(self.timeout, &self.cancellation, self.deadline)?;
        let mut request = self
            .request(&resource_path(&["runs", id, "logs", "stream"])?, None)?
            .header(reqwest::header::ACCEPT, "text/event-stream");
        if let Some(cursor) = cursor {
            valid_cursor(cursor)?;
            request = request.header("Last-Event-ID", cursor);
        }
        let lease = self
            .credentials
            .acquire_async(self.base.as_str(), budget.deadline, &self.cancellation)
            .await?;
        if let Some(token) = &lease.bearer {
            request = request.bearer_auth(token);
        }
        budget.remaining()?;
        // Bound acquisition and the handshake, without assigning an overall
        // request timeout that would terminate an otherwise healthy SSE body.
        let response = tokio::time::timeout_at(budget.deadline.into(), request.send())
            .await
            .map_err(|_| {
                Error::Invalid("Log stream handshake reached the request deadline.".into())
            })??;
        let response = checked_response(response).await?;
        if response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_none_or(|value| {
                !value
                    .split(';')
                    .next()
                    .is_some_and(|v| v.trim().eq_ignore_ascii_case("text/event-stream"))
            })
        {
            return Err(
                Error::Invalid("Log stream did not return text/event-stream.".into()).into(),
            );
        }
        Ok(LogStream {
            response,
            decoder: Decoder::default(),
            pending: Vec::new(),
            offset: 0,
            renewal_deadline: lease.renewal_deadline(),
        })
    }
}

async fn bounded_body(mut response: Response) -> Result<Vec<u8>, Error> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len().saturating_add(chunk.len()) > super::MAX_JSON_RESPONSE as usize {
            return Err(Error::Invalid("API JSON response exceeded 8 MiB.".into()));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn checked_response(response: Response) -> Result<Response, ObservationError> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status().as_u16();
    let header = |name| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    if status == 410
        && let (Some(cursor), Some(latest)) = (
            header("x-history-recovery-cursor"),
            header("x-history-latest-position"),
        )
    {
        valid_cursor(&cursor)?;
        let latest_position = latest
            .parse::<i64>()
            .ok()
            .filter(|n| *n >= 0)
            .ok_or_else(|| Error::Invalid("Invalid log retention boundary.".into()))?;
        let earliest_position = header("x-history-earliest-position")
            .map(|s| s.parse::<i64>())
            .transpose()
            .map_err(|_| Error::Invalid("Invalid log retention boundary.".into()))?;
        return Err(ObservationError::RetentionGap(RetentionBoundary {
            recovery_cursor: cursor,
            latest_position,
            earliest_position,
            payload_max_age_seconds: None,
            payload_max_entries: None,
        }));
    }
    let retry_after = header("retry-after");
    let request_id = header("x-request-id").unwrap_or_default();
    let failure =
        serde_json::from_slice(&bounded_body(response).await?).unwrap_or_else(|_| ApiFailure {
            code: "invalid_api_response".into(),
            message: "The service returned an unexpected response.".into(),
            request_id,
            details: None,
        });
    Err(Error::Api {
        status,
        failure: Box::new(failure),
        retry_after,
    }
    .into())
}

fn valid_cursor(cursor: &str) -> Result<(), Error> {
    if cursor.is_empty() || cursor.len() > 1024 || cursor.chars().any(char::is_control) {
        return Err(Error::Invalid("Invalid log resume cursor.".into()));
    }
    Ok(())
}

/// `next` is cancellation-safe: decoded bytes and partial frames belong to this reader.
pub struct LogStream {
    response: Response,
    decoder: Decoder,
    pending: Vec<u8>,
    offset: usize,
    renewal_deadline: Option<Instant>,
}

impl LogStream {
    /// Deadline belonging to the credential used for this stream's handshake.
    pub fn renewal_deadline(&self) -> Option<Instant> {
        self.renewal_deadline
    }

    pub async fn next(&mut self) -> Result<Option<LogStreamEvent>, Error> {
        loop {
            while let Some(byte) = self.pending.get(self.offset).copied() {
                self.offset += 1;
                if let Some(event) = self.decoder.push(byte)? {
                    return Ok(Some(event));
                }
            }
            let Some(chunk) = self.response.chunk().await? else {
                return Ok(None);
            };
            self.pending = chunk.to_vec();
            self.offset = 0;
        }
    }
}

#[derive(Default)]
struct Decoder {
    line: Vec<u8>,
    event: String,
    id: Option<String>,
    data: String,
    size: usize,
    after_cr: bool,
}

impl Decoder {
    fn push(&mut self, byte: u8) -> Result<Option<LogStreamEvent>, Error> {
        if self.after_cr && byte == b'\n' {
            self.after_cr = false;
            return Ok(None);
        }
        self.after_cr = byte == b'\r';
        self.size += 1;
        if self.size > MAX_EVENT_BYTES {
            return Err(Error::Invalid("Log SSE event exceeded 64 KiB.".into()));
        }
        if byte != b'\n' && byte != b'\r' {
            self.line.push(byte);
            return Ok(None);
        }
        let bytes = std::mem::take(&mut self.line);
        let line = std::str::from_utf8(&bytes)
            .map_err(|_| Error::Invalid("Invalid UTF-8 in log SSE event.".into()))?;
        if line.is_empty() {
            self.size = 0;
            if self.data.is_empty() {
                self.event.clear();
                self.id = None;
                return Ok(None);
            }
            let event = self.decode()?;
            self.event.clear();
            self.id = None;
            self.data.clear();
            return Ok(Some(event));
        }
        if line.starts_with(':') {
            return Ok(None);
        }
        let (name, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match name {
            "event" => self.event = value.into(),
            "id" => {
                valid_cursor(value)?;
                self.id = Some(value.into());
            }
            "data" => {
                self.data.push_str(value);
                self.data.push('\n');
            }
            _ => (),
        }
        Ok(None)
    }

    fn decode(&self) -> Result<LogStreamEvent, Error> {
        let data = match self.event.as_str() {
            "log" => {
                let entry: LogEntry = serde_json::from_str(&self.data)?;
                if entry.position < 1 || self.id.is_none() {
                    return Err(Error::Invalid(
                        "Log event omitted a valid position or cursor.".into(),
                    ));
                }
                LogStreamData::Log(entry)
            }
            "retention_gap" => {
                let boundary: RetentionBoundary = serde_json::from_str(&self.data)?;
                valid_cursor(&boundary.recovery_cursor)?;
                if self.id.as_deref() != Some(&boundary.recovery_cursor)
                    || boundary.latest_position < 0
                {
                    return Err(Error::Invalid(
                        "Log retention event has a conflicting cursor.".into(),
                    ));
                }
                LogStreamData::RetentionGap(boundary)
            }
            "auth_revoked" | "source_unavailable" | "shutdown" => {
                let value: Value = serde_json::from_str(&self.data)?;
                if value["code"] != self.event || self.id.is_some() {
                    return Err(Error::Invalid("Invalid log control event.".into()));
                }
                match self.event.as_str() {
                    "auth_revoked" => LogStreamData::AuthRevoked,
                    "shutdown" => LogStreamData::Shutdown,
                    _ => LogStreamData::SourceUnavailable,
                }
            }
            _ => return Err(Error::Invalid("Unknown log SSE event type.".into())),
        };
        Ok(LogStreamEvent {
            id: self.id.clone(),
            data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log() -> Value {
        json!({"position":1,"assignment_id":Uuid::nil(),"source":"stdout","epoch":1,
            "sequence":1,"offset":0,"byte_length":4,"text":"λ\nend","gap":null,
            "source_reset":false,"received_at":"2026-10-15T00:00:00Z"})
    }

    fn decode(bytes: &[u8]) -> Result<Vec<LogStreamEvent>, Error> {
        let mut decoder = Decoder::default();
        let mut events = Vec::new();
        for byte in bytes {
            if let Some(event) = decoder.push(*byte)? {
                events.push(event);
            }
        }
        Ok(events)
    }

    #[test]
    fn byte_fragmentation_utf8_keepalives_and_all_line_endings() {
        for newline in ["\n", "\r\n", "\r"] {
            let text = format!(
                ": keepalive{newline}{newline}event: log{newline}id: opaque{newline}data: {}{newline}{newline}",
                log()
            );
            let events = decode(text.as_bytes()).unwrap();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].name(), "log");
            assert_eq!(events[0].id.as_deref(), Some("opaque"));
            assert_eq!(events[0].data(), log());
        }
    }

    #[test]
    fn multiline_data_and_unterminated_frames_do_not_advance() {
        let mut data = log().to_string();
        data = data.replacen('{', "{\ndata: ", 1);
        let text = format!("event: log\nid: a\ndata: {data}\n\n");
        let events = decode(text.as_bytes()).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data(), log());
        assert!(decode(text.trim_end().as_bytes()).unwrap().is_empty());
    }

    #[test]
    fn typed_gap_and_control_events_preserve_evidence() {
        let boundary = json!({"earliest_position":5,"latest_position":8,"recovery_cursor":"r",
            "payload_max_age_seconds":604800,"payload_max_entries":1024});
        let events =
            decode(format!("event: retention_gap\nid: r\ndata: {boundary}\n\n").as_bytes())
                .unwrap();
        assert_eq!(events[0].data(), boundary);
        for name in ["auth_revoked", "source_unavailable", "shutdown"] {
            let events =
                decode(format!("event: {name}\ndata: {{\"code\":\"{name}\"}}\n\n").as_bytes())
                    .unwrap();
            assert_eq!(events[0].name(), name);
        }
    }

    #[test]
    fn malformed_events_and_invalid_resume_ids_are_rejected() {
        for text in [
            "event: log\nid: a\ndata: {}\n\n".into(),
            format!("event: log\ndata: {}\n\n", log()),
            "event: shutdown\ndata: {\"code\":\"auth_revoked\"}\n\n".into(),
            "event: unknown\ndata: {}\n\n".into(),
            "event: log\nid: \ndata: {}\n\n".into(),
            format!("event: log\nid: {}\ndata: {{}}\n\n", "a".repeat(1025)),
            "event: retention_gap\nid: a\ndata: {\"recovery_cursor\":\"b\",\"latest_position\":8}\n\n".into(),
        ] { assert!(decode(text.as_bytes()).is_err(), "{text}"); }
        assert!(decode(b"data: \xff\n\n").is_err());
    }

    #[test]
    fn event_bound_applies_to_unfinished_and_accumulated_lines_but_not_whole_stream() {
        assert!(decode(&vec![b'a'; MAX_EVENT_BYTES + 1]).is_err());
        assert!(decode("data: a\n".repeat(MAX_EVENT_BYTES / 8 + 1).as_bytes()).is_err());
        assert!(
            decode(": keepalive\n\n".repeat(MAX_EVENT_BYTES).as_bytes())
                .unwrap()
                .is_empty()
        );
    }
}
