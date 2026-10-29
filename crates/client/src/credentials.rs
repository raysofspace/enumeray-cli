//! Optional credential acquisition without coupling the client to profile storage.
use crate::Error;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

/// A credential snapshot. Secrets are deliberately excluded from Debug output.
pub struct CredentialLease {
    pub(crate) bearer: Option<String>,
    renewal_deadline: Option<Instant>,
}

impl CredentialLease {
    pub fn new(bearer: Option<String>, renewal_deadline: Option<Instant>) -> Self {
        Self {
            bearer,
            renewal_deadline,
        }
    }

    pub fn renewal_deadline(&self) -> Option<Instant> {
        self.renewal_deadline
    }
}

/// Acquire credentials for this exact API before a request is dispatched.
/// Implementations own renewal/persistence and must respect the supplied deadline.
/// Acquisition does not retry or dispatch the caller's API operation.
pub trait CredentialSource: Send + Sync {
    fn acquire(&self, api_origin: &str, deadline: Instant) -> Result<CredentialLease, Error>;

    fn acquire_cancellable(
        &self,
        api_origin: &str,
        deadline: Instant,
        cancellation: &CancellationToken,
    ) -> Result<CredentialLease, Error> {
        if cancellation.is_cancelled() {
            return Err(Error::Invalid(
                "Credential acquisition cancelled before renewal.".into(),
            ));
        }
        if Instant::now() >= deadline {
            return Err(Error::Invalid(
                "Credential acquisition deadline elapsed before renewal.".into(),
            ));
        }
        self.acquire(api_origin, deadline)
    }
}

/// Cooperative cancellation checked before acquisition and request dispatch.
#[derive(Clone, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Clone)]
pub(crate) enum Credentials {
    Static(Option<String>),
    Source(Arc<dyn CredentialSource>),
}

impl Credentials {
    pub(crate) fn acquire(
        &self,
        origin: &str,
        deadline: Instant,
        cancellation: &CancellationToken,
    ) -> Result<CredentialLease, Error> {
        match self {
            Self::Static(token) => Ok(CredentialLease::new(token.clone(), None)),
            Self::Source(source) => source.acquire_cancellable(origin, deadline, cancellation),
        }
    }

    pub(crate) async fn acquire_async(
        &self,
        origin: &str,
        deadline: Instant,
        cancellation: &CancellationToken,
    ) -> Result<CredentialLease, Error> {
        let Self::Source(source) = self else {
            return self.acquire(origin, deadline, cancellation);
        };
        let source = Arc::clone(source);
        let origin = origin.to_owned();
        let cancellation = cancellation.clone();
        // Only acquisition runs in the worker. Cancellation can leave a started
        // rotation finishing its durable commit, but cannot dispatch an API call.
        let worker = tokio::task::spawn_blocking(move || {
            source.acquire_cancellable(&origin, deadline, &cancellation)
        });
        tokio::time::timeout_at(deadline.into(), worker)
            .await
            .map_err(|_| {
                Error::Invalid("Credential acquisition reached the request deadline.".into())
            })?
            .map_err(|_| Error::Invalid("Credential acquisition worker stopped.".into()))?
    }
}

pub(crate) struct RequestBudget {
    pub(crate) deadline: Instant,
    cancellation: CancellationToken,
}

impl RequestBudget {
    pub(crate) fn new(
        timeout: Duration,
        cancellation: &CancellationToken,
        absolute: Option<Instant>,
    ) -> Result<Self, Error> {
        let request_deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| Error::Invalid("HTTP deadline is too large.".into()))?;
        let deadline = absolute.map_or(request_deadline, |limit| limit.min(request_deadline));
        let budget = Self {
            deadline,
            cancellation: cancellation.clone(),
        };
        budget.remaining()?;
        Ok(budget)
    }

    pub(crate) fn remaining(&self) -> Result<Duration, Error> {
        if self.cancellation.is_cancelled() {
            return Err(Error::Invalid(
                "Request cancelled before dispatch; retain the original operation.".into(),
            ));
        }
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::Invalid(
                "Request deadline elapsed before dispatch; retain the original operation.".into(),
            ));
        }
        Ok(remaining)
    }
}
