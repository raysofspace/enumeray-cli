//! Restricted service command workflows.
use super::{typed_get, typed_mutate, typed_paged_get};
use enumeray_contracts::management::*;

use crate::args::Service;
use enumeray_client::{EnumerayClient, Error};
use reqwest::Method;
use serde_json::{Value, json};

pub(crate) fn service(client: &EnumerayClient, command: &Service) -> Result<Value, Error> {
    match command {
        Service::Status => typed_get::<ServiceStatus>(client, &["service", "status"]),
        Service::Limits => typed_get::<ServiceLimits>(client, &["service", "limits"]),
        Service::SetLimits {
            max_queued_runs,
            max_queued_runs_per_user,
            generation,
        } => typed_mutate::<ServiceLimits, _>(
            client,
            Method::PUT,
            &["service", "limits"],
            &json!({
                "max_queued_runs":max_queued_runs,"max_queued_runs_per_user":max_queued_runs_per_user,"expected_generation":generation
            }),
        ),
        Service::Suspend { user, reason } => typed_mutate::<UserSuspensionReceipt, _>(
            client,
            Method::PUT,
            &["service", "users", &user.to_string(), "suspension"],
            &json!({"suspended":true,"reason":reason}),
        ),
        Service::Resume { user, reason } => typed_mutate::<UserSuspensionReceipt, _>(
            client,
            Method::PUT,
            &["service", "users", &user.to_string(), "suspension"],
            &json!({"suspended":false,"reason":reason}),
        ),
        Service::Audit { page } => {
            typed_paged_get::<Collection<ServiceAuditEvent>>(client, &["service", "audit"], page)
        }
    }
}
