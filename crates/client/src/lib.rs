//! Standalone Enumeray client: no database, service, or object-storage SDK dependencies.
pub mod auth;
pub mod bulk;
pub mod collections;
mod credentials;
pub mod management;
pub mod observation;
pub mod types;

pub use credentials::{CancellationToken, CredentialLease, CredentialSource};
use credentials::{Credentials, RequestBudget};
pub use enumeray_contracts::runs::RunPurpose;
use reqwest::{
    Method,
    blocking::{Client, Response},
    redirect::Policy,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    fmt,
    io::Read,
    sync::Arc,
    time::{Duration, Instant},
};
pub use types::*;
use url::Url;

const MAX_JSON_RESPONSE: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionErrorCode {
    LoginRequired,
    Changed,
    RenewalUncertain,
    ProviderUnavailable,
    ProfileBusy,
    AccountMismatch,
    InvalidResponse,
    Persistence,
}
impl SessionErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LoginRequired => "session_login_required",
            Self::Changed => "session_changed",
            Self::RenewalUncertain => "session_renewal_uncertain",
            Self::ProviderUnavailable => "session_provider_unavailable",
            Self::ProfileBusy => "session_profile_busy",
            Self::AccountMismatch => "session_account_mismatch",
            Self::InvalidResponse => "session_invalid_response",
            Self::Persistence => "session_persistence",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthFailureKind {
    InvalidGrant,
    Configuration,
    TemporarilyUnavailable,
    InvalidResponse,
}
impl OAuthFailureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidGrant => "oauth_invalid_grant",
            Self::Configuration => "oauth_configuration",
            Self::TemporarilyUnavailable => "oauth_temporarily_unavailable",
            Self::InvalidResponse => "oauth_invalid_response",
        }
    }
}

#[derive(Debug)]
pub enum Error {
    Invalid(String),
    Transport(reqwest::Error),
    Io(std::io::Error),
    Json(serde_json::Error),
    Session {
        code: SessionErrorCode,
        message: String,
    },
    OAuth {
        status: u16,
        kind: OAuthFailureKind,
        retry_after: Option<String>,
    },
    Api {
        status: u16,
        failure: Box<ApiFailure>,
        retry_after: Option<String>,
    },
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => f.write_str(message),
            Self::Transport(error) => write!(f, "HTTP transport failed: {error}"),
            Self::Io(error) => write!(f, "I/O failed: {error}"),
            Self::Json(_) => f.write_str("The JSON document does not match the expected contract."),
            Self::Session { message, .. } => f.write_str(message),
            Self::OAuth { status, kind, .. } => write!(
                f,
                "{} (HTTP {status}): identity-provider token exchange failed.",
                kind.as_str()
            ),
            Self::Api {
                status, failure, ..
            } => write!(
                f,
                "{} (HTTP {status}): {} [request {}]",
                failure.code, failure.message, failure.request_id
            ),
        }
    }
}
impl std::error::Error for Error {}
impl From<reqwest::Error> for Error {
    fn from(error: reqwest::Error) -> Self {
        Self::Transport(error.without_url())
    }
}
impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// Validate transport before attaching credentials. Cleartext is allowed only on loopback.
pub fn validate_url(value: &str) -> Result<Url, Error> {
    let url = Url::parse(value).map_err(|_| Error::Invalid("Invalid URL.".into()))?;
    let loopback = match url.host() {
        Some(url::Host::Domain("localhost")) => true,
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    };
    if (url.scheme() != "https" && !(url.scheme() == "http" && loopback))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::Invalid(
            "URLs require HTTPS (or loopback HTTP), no embedded credentials, and no fragment."
                .into(),
        ));
    }
    Ok(url)
}

/// Encodes identifiers as individual path segments; identifiers cannot introduce routes or queries.
pub fn resource_path(segments: &[&str]) -> Result<String, Error> {
    let mut url = Url::parse("https://enumeray.invalid/api/v1/").expect("static URL");
    {
        let mut path = url.path_segments_mut().expect("base URL");
        path.pop_if_empty();
        for segment in segments {
            if segment.is_empty() || *segment == "." || *segment == ".." {
                return Err(Error::Invalid(
                    "A resource identifier is empty or invalid.".into(),
                ));
            }
            path.push(segment);
        }
    }
    Ok(url.path().to_owned())
}

#[derive(Clone)]
pub struct EnumerayClient {
    base: Url,
    credentials: Credentials,
    http: Client,
    timeout: Duration,
    cancellation: CancellationToken,
    deadline: Option<Instant>,
}

impl EnumerayClient {
    pub fn new(base: &str, token: Option<String>, timeout: Duration) -> Result<Self, Error> {
        validate_timeout(timeout)?;
        let mut base = validate_url(base)?;
        if base.query().is_some() || base.path() != "/" {
            return Err(Error::Invalid(
                "The API URL must be an origin, without a path or query.".into(),
            ));
        }
        base.set_path("/");
        let http = Client::builder()
            .timeout(timeout)
            .connect_timeout(Duration::from_secs(15))
            .redirect(Policy::none())
            .user_agent(concat!("enumeray-client/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            base,
            credentials: Credentials::Static(token),
            http,
            timeout,
            cancellation: CancellationToken::new(),
            deadline: None,
        })
    }
    pub fn with_credentials(
        base: &str,
        source: Arc<dyn CredentialSource>,
        timeout: Duration,
    ) -> Result<Self, Error> {
        let mut client = Self::new(base, None, timeout)?;
        client.credentials = Credentials::Source(source);
        Ok(client)
    }

    pub fn with_cancellation(&self, cancellation: CancellationToken) -> Self {
        let mut client = self.clone();
        client.cancellation = cancellation;
        client
    }

    /// Bound all subsequent acquisition and request work by one workflow deadline.
    pub fn with_deadline(&self, deadline: Instant) -> Self {
        let mut client = self.clone();
        client.deadline = Some(self.deadline.map_or(deadline, |old| old.min(deadline)));
        client
    }
    pub fn base_url(&self) -> &str {
        self.base.as_str()
    }

    /// Keep this API/account and connection pool while bounding an individual workflow.
    pub fn with_timeout_limit(&self, timeout: Duration) -> Result<Self, Error> {
        validate_timeout(timeout)?;
        let mut client = self.clone();
        client.timeout = self.timeout.min(timeout);
        Ok(client)
    }

    fn url(&self, path: &str) -> Result<Url, Error> {
        api_url(&self.base, path)
    }

    /// Create a cancellable asynchronous reader; existing blocking methods are unchanged.
    pub fn observation_client(&self) -> Result<observation::ObservationClient, Error> {
        observation::ObservationClient::new(
            self.base.clone(),
            self.credentials.clone(),
            self.timeout,
            self.cancellation.clone(),
            self.deadline,
        )
    }

    pub fn request<T: DeserializeOwned, B: Serialize + ?Sized>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
        key: Option<&str>,
    ) -> Result<T, Error> {
        let response = self.send(method, path, body, key)?;
        let bytes = bounded_body(response)?;
        Ok(serde_json::from_slice(if bytes.is_empty() {
            b"null"
        } else {
            &bytes
        })?)
    }
    pub fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, Error> {
        self.request::<T, Value>(Method::GET, path, None, None)
    }
    pub fn post<T: DeserializeOwned, B: Serialize + ?Sized>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, Error> {
        self.request(Method::POST, path, Some(body), None)
    }
    /// Inspect current limits and generation, including a retained revoked grant.
    pub fn compute_grant(
        &self,
        infrastructure: &str,
        grant: uuid::Uuid,
    ) -> Result<enumeray_contracts::management::ComputeGrant, Error> {
        self.get(&resource_path(&[
            "infrastructures",
            infrastructure,
            "grants",
            &grant.to_string(),
        ])?)
    }

    /// Replace limits at the supplied generation. Never refresh or retry the guard.
    pub fn update_compute_grant(
        &self,
        infrastructure: &str,
        grant: uuid::Uuid,
        request: &enumeray_contracts::management::GrantUpdate,
    ) -> Result<enumeray_contracts::management::ComputeGrant, Error> {
        self.request(
            Method::PUT,
            &resource_path(&[
                "infrastructures",
                infrastructure,
                "grants",
                &grant.to_string(),
            ])?,
            Some(request),
            None,
        )
    }

    pub fn configure_connector(
        &self,
        infrastructure: &str,
        connector: &str,
        request: &enumeray_contracts::management::ConnectorUpdate,
    ) -> Result<Value, Error> {
        for id in [infrastructure, connector] {
            if !enumeray_contracts::models::validation::valid_model_id(id) {
                return Err(Error::Invalid(
                    "Invalid connector resource identifier.".into(),
                ));
            }
        }
        Ok(serde_json::to_value(self.management().update_connector(
            infrastructure,
            connector,
            request,
        )?)?)
    }
    /// Inspect maintenance, authority and retained work without claiming assignments.
    pub fn connector_diagnostics(
        &self,
        infrastructure: &str,
        connector: &str,
    ) -> Result<enumeray_contracts::management::ConnectorLifecycleStatus, Error> {
        self.get(&resource_path(&[
            "infrastructures",
            infrastructure,
            "connectors",
            connector,
            "diagnostics",
        ])?)
    }

    /// Change only maintenance state at the supplied generation, without retrying.
    pub fn set_connector_maintenance(
        &self,
        infrastructure: &str,
        connector: &str,
        request: &enumeray_contracts::management::ConnectorStateChange,
    ) -> Result<enumeray_contracts::management::ConnectorLifecycleReceipt, Error> {
        self.request(
            Method::PUT,
            &resource_path(&[
                "infrastructures",
                infrastructure,
                "connectors",
                connector,
                "state",
            ])?,
            Some(request),
            None,
        )
    }

    /// Change only reversible execution authority at the supplied generation.
    pub fn set_connector_authority(
        &self,
        infrastructure: &str,
        connector: &str,
        request: &enumeray_contracts::management::ConnectorAuthorityChange,
    ) -> Result<enumeray_contracts::management::ConnectorLifecycleReceipt, Error> {
        self.request(
            Method::PUT,
            &resource_path(&[
                "infrastructures",
                infrastructure,
                "connectors",
                connector,
                "authority",
            ])?,
            Some(request),
            None,
        )
    }

    pub fn me(&self) -> Result<Identity, Error> {
        self.get("/api/v1/me")
    }

    /// Inspect context-scoped retirement evidence separately from connector shutdown.
    pub fn context_status(
        &self,
        infrastructure: &str,
        context: &str,
    ) -> Result<enumeray_contracts::management::ContextLifecycleStatus, Error> {
        self.get(&resource_path(&[
            "infrastructures",
            infrastructure,
            "contexts",
            context,
        ])?)
    }

    /// Read one bounded dependency page without changing publications or accepted work.
    pub fn context_replacement_plan(
        &self,
        infrastructure: &str,
        context: &str,
        replacement_context_id: Option<&str>,
        section: enumeray_contracts::management::ContextDependencySection,
        limit: Option<u16>,
        after: Option<&str>,
    ) -> Result<enumeray_contracts::management::ContextReplacementPlan, Error> {
        let path = resource_path(&[
            "infrastructures",
            infrastructure,
            "contexts",
            context,
            "replacement-plan",
        ])?;
        let mut url = self.url(&path)?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("section", section.as_str());
            if let Some(limit) = limit {
                query.append_pair("limit", &limit.to_string());
            }
            if let Some(replacement) = replacement_context_id {
                query.append_pair("replacement_context_id", replacement);
            }
            if let Some(after) = after {
                query.append_pair("after", after);
            }
        }
        self.get(&format!(
            "{}?{}",
            url.path(),
            url.query().unwrap_or_default()
        ))
    }

    /// Commit one guarded cutover. Never retry with a refreshed generation.
    pub fn cutover_context(
        &self,
        infrastructure: &str,
        context: &str,
        request: &enumeray_contracts::management::ContextCutover,
    ) -> Result<enumeray_contracts::management::ContextLifecycleReceipt, Error> {
        self.request(
            Method::PUT,
            &resource_path(&[
                "infrastructures",
                infrastructure,
                "contexts",
                context,
                "cutover",
            ])?,
            Some(request),
            None,
        )
    }

    pub fn reopen_context(
        &self,
        infrastructure: &str,
        context: &str,
        request: &enumeray_contracts::management::GenerationChange,
    ) -> Result<enumeray_contracts::management::ContextLifecycleReceipt, Error> {
        self.request(
            Method::PUT,
            &resource_path(&[
                "infrastructures",
                infrastructure,
                "contexts",
                context,
                "reopen",
            ])?,
            Some(request),
            None,
        )
    }

    /// Explicitly release source publication configuration; this does not revoke it.
    pub fn release_context_configurations(
        &self,
        infrastructure: &str,
        context: &str,
        request: &enumeray_contracts::management::GenerationChange,
    ) -> Result<enumeray_contracts::management::ContextLifecycleReceipt, Error> {
        self.request(
            Method::PUT,
            &resource_path(&[
                "infrastructures",
                infrastructure,
                "contexts",
                context,
                "configuration-retirements",
            ])?,
            Some(request),
            None,
        )
    }

    pub fn retire_context(
        &self,
        infrastructure: &str,
        context: &str,
        request: &enumeray_contracts::management::GenerationChange,
    ) -> Result<enumeray_contracts::management::ContextLifecycleReceipt, Error> {
        self.request(
            Method::PUT,
            &resource_path(&[
                "infrastructures",
                infrastructure,
                "contexts",
                context,
                "retirement",
            ])?,
            Some(request),
            None,
        )
    }
    pub fn auth_config(&self) -> Result<AuthConfig, Error> {
        self.get("/api/v1/auth/config")
    }
    pub fn log_archive(
        &self,
        run: uuid::Uuid,
    ) -> Result<enumeray_contracts::logs::LogArchiveStatus, Error> {
        self.get(&format!("/api/v1/runs/{run}/log-archive"))
    }
    pub fn submit(&self, prepared: &PreparedRun) -> Result<SubmissionReceipt, Error> {
        if prepared.schema_version != 1 || prepared.api_url != self.base_url() {
            return Err(Error::Invalid(
                "Prepared request belongs to another API or unsupported format.".into(),
            ));
        }
        if self.me()?.user_id != prepared.user_id {
            return Err(Error::Invalid(
                "Prepared request belongs to another user.".into(),
            ));
        }
        self.request(
            Method::POST,
            "/api/v1/runs",
            Some(&prepared.request),
            Some(&prepared.idempotency_key),
        )
    }
    pub fn prepare_run(&self, request: RunSubmission) -> Result<PreparedRun, Error> {
        Ok(PreparedRun {
            schema_version: 1,
            api_url: self.base_url().to_owned(),
            user_id: self.me()?.user_id,
            idempotency_key: uuid::Uuid::new_v4().to_string(),
            request,
        })
    }

    fn send<B: Serialize + ?Sized>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
        key: Option<&str>,
    ) -> Result<Response, Error> {
        let budget = RequestBudget::new(self.timeout, &self.cancellation, self.deadline)?;
        let mut request = self.http.request(method, self.url(path)?);
        if let Some(key) = key {
            if key.is_empty()
                || key.len() > 128
                || !key
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            {
                return Err(Error::Invalid("Invalid idempotency key.".into()));
            }
            request = request.header("Idempotency-Key", key);
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        let lease =
            self.credentials
                .acquire(self.base_url(), budget.deadline, &self.cancellation)?;
        if let Some(token) = &lease.bearer {
            request = request.bearer_auth(token);
        }
        request = request.timeout(budget.remaining()?);
        let response = request.send()?;
        if response.status().is_success() {
            return Ok(response);
        }
        Err(api_error(response))
    }

    /// Obtain a short-lived storage capability, then use a separate unauthenticated request.
    /// Redirects are disabled on both clients: API credentials never reach storage.
    pub fn download(&self, path: &str, output: &mut impl std::io::Write) -> Result<u64, Error> {
        let url = self.url(path)?;
        let budget = RequestBudget::new(self.timeout, &self.cancellation, self.deadline)?;
        let lease =
            self.credentials
                .acquire(self.base_url(), budget.deadline, &self.cancellation)?;
        let mut request = self.http.get(url);
        if let Some(token) = &lease.bearer {
            request = request.bearer_auth(token);
        }
        let mut response = request.timeout(budget.remaining()?).send()?;
        // Provenance exports are generated directly by the API; artifact bytes
        // are delivered through the separate short-lived capability below.
        if response.status() == reqwest::StatusCode::OK {
            return Ok(std::io::copy(&mut response, output)?);
        }
        if response.status() != reqwest::StatusCode::SEE_OTHER {
            return Err(api_error(response));
        }
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| {
                Error::Invalid("Content response omitted its transfer location.".into())
            })?;
        let url = validate_url(location)?;
        bulk::download(url.as_str(), output, |_| {})
    }

    /// Obtain fresh authority for the original content endpoint. Never attach
    /// this client's bearer to the returned storage capability.
    pub fn download_location(&self, path: &str) -> Result<String, Error> {
        let url = self.url(path)?;
        let budget = RequestBudget::new(self.timeout, &self.cancellation, self.deadline)?;
        let lease =
            self.credentials
                .acquire(self.base_url(), budget.deadline, &self.cancellation)?;
        let mut request = self.http.get(url);
        if let Some(token) = &lease.bearer {
            request = request.bearer_auth(token);
        }
        let response = request.timeout(budget.remaining()?).send()?;
        if response.status() != reqwest::StatusCode::SEE_OTHER {
            return Err(api_error(response));
        }
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| {
                Error::Invalid("Content response omitted its transfer location.".into())
            })?;
        Ok(validate_url(location)?.to_string())
    }

    /// Stream one bounded range without forwarding API credentials to storage.
    pub fn transfer_part(
        &self,
        transfer: &ScopedTransfer,
        file: std::fs::File,
        offset: u64,
        size: u64,
        overall: Duration,
    ) -> Result<(), Error> {
        if transfer.method != "PUT"
            || size == 0
            || size > enumeray_contracts::models::MAX_SINGLE_UPLOAD_BYTES
        {
            return Err(Error::Invalid("Invalid upload part transfer.".into()));
        }
        bulk::upload(
            file,
            offset,
            size,
            &transfer.url,
            &transfer.headers,
            overall,
            |_| {},
        )?;
        Ok(())
    }

    /// PUT exact bytes to an assignment-scoped storage URL without the API bearer.
    pub fn transfer_upload(
        &self,
        transfer: &ScopedTransfer,
        file: std::fs::File,
    ) -> Result<String, Error> {
        self.transfer_upload_with_budget(transfer, file, Duration::from_secs(172800))
    }

    /// Share the caller's remaining attempt budget with a single PUT.
    pub fn transfer_upload_with_budget(
        &self,
        transfer: &ScopedTransfer,
        file: std::fs::File,
        overall: Duration,
    ) -> Result<String, Error> {
        if transfer.method != "PUT" {
            return Err(Error::Invalid("Unsupported upload method.".into()));
        }
        let size = file.metadata()?.len();
        bulk::upload(
            file,
            0,
            size,
            &transfer.url,
            &transfer.headers,
            overall,
            |_| {},
        )?
        .ok_or_else(|| {
            Error::Invalid(
                "Storage omitted the immutable object version; upload cannot be completed.".into(),
            )
        })
    }
}

fn validate_timeout(timeout: Duration) -> Result<(), Error> {
    if timeout.is_zero() || std::time::Instant::now().checked_add(timeout).is_none() {
        return Err(Error::Invalid(
            "HTTP timeout must be positive and representable.".into(),
        ));
    }
    Ok(())
}

fn bounded_body(response: Response) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    response
        .take(MAX_JSON_RESPONSE + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_JSON_RESPONSE {
        return Err(Error::Invalid("API JSON response exceeded 8 MiB.".into()));
    }
    Ok(bytes)
}
fn api_error(response: Response) -> Error {
    let status = response.status().as_u16();
    let request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let failure = bounded_body(response)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_else(|| ApiFailure {
            code: "invalid_api_response".into(),
            message: "The service returned an unexpected response.".into(),
            request_id,
            details: None,
        });
    Error::Api {
        status,
        failure: Box::new(failure),
        retry_after,
    }
}

fn api_url(base: &Url, path: &str) -> Result<Url, Error> {
    if !path.starts_with("/api/v1/") || path.contains('#') || path.contains('\\') {
        return Err(Error::Invalid("API paths must start with /api/v1/.".into()));
    }
    let url = base
        .join(path)
        .map_err(|_| Error::Invalid("Invalid API path.".into()))?;
    if url.origin() != base.origin() || !url.path().starts_with("/api/v1/") {
        return Err(Error::Invalid(
            "API path must stay within this service.".into(),
        ));
    }
    Ok(url)
}
