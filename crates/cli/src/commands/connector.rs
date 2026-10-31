//! Connector command workflows.
use super::{typed_get, typed_mutate, typed_paged_get};
use enumeray_contracts::management::*;

use super::require_new_output;
use crate::{args::Connector, config};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use enumeray_client::{EnumerayClient, Error};
use enumeray_contracts::management::{
    ConnectorAuthorityChange, ConnectorCreate, ConnectorLifecycleStatus, ConnectorStateChange,
    ConnectorUpdate,
};
use reqwest::Method;
use serde_json::{Value, json};
use std::{
    path::Path,
    time::{Duration, Instant},
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

pub(crate) fn connector(
    client: &EnumerayClient,
    command: &Connector,
    json_output: bool,
) -> Result<Value, Error> {
    match command {
        Connector::Host(_) => unreachable!("host dispatch precedes user profile access"),
        Connector::List { page } => {
            typed_paged_get::<Collection<ConnectorSummary>>(client, &["connectors"], page)
        }
        Connector::Create {
            id,
            label,
            max_outstanding_assignments,
            output,
        } => {
            let _output_lock = require_new_output(output)?;
            let response = typed_mutate::<ConnectorRegistration, _>(
                client,
                Method::POST,
                &["connectors"],
                &ConnectorCreate {
                    id: id.clone(),
                    label: label.clone(),
                    max_outstanding_assignments: *max_outstanding_assignments,
                },
            );
            save_enrollment(client, id, output, response, true)
        }
        Connector::Enrollment { id, output } => {
            let _output_lock = require_new_output(output)?;
            let response = typed_mutate::<ConnectorEnrollment, _>(
                client,
                Method::POST,
                &["connectors", id, "enrollments"],
                &json!({}),
            );
            save_enrollment(client, id, output, response, false)
        }
        Connector::Get { id } => typed_get::<ConnectorSummary>(client, &["connectors", id]),
        Connector::Configure {
            id,
            label,
            max_outstanding_assignments,
            generation,
        } => client.configure_connector(
            id,
            &ConnectorUpdate {
                label: label.clone(),
                max_outstanding_assignments: *max_outstanding_assignments,
                expected_generation: *generation,
            },
        ),
        Connector::Maintenance {
            id,
            generation,
            dry_run,
        } => lifecycle(
            client,
            id,
            *generation,
            *dry_run,
            json_output,
            Change::Maintenance(true),
        ),
        Connector::Resume {
            id,
            generation,
            dry_run,
        } => lifecycle(
            client,
            id,
            *generation,
            *dry_run,
            json_output,
            Change::Maintenance(false),
        ),
        Connector::WithdrawAuthority {
            id,
            generation,
            dry_run,
        } => lifecycle(
            client,
            id,
            *generation,
            *dry_run,
            json_output,
            Change::Authority(true),
        ),
        Connector::RestoreAuthority {
            id,
            generation,
            dry_run,
        } => lifecycle(
            client,
            id,
            *generation,
            *dry_run,
            json_output,
            Change::Authority(false),
        ),
        Connector::WaitMaintenance {
            id,
            generation,
            interval,
            deadline,
        } => wait_maintenance(client, id, *generation, *interval, *deadline, json_output),
        Connector::Update { id, file } => {
            let request: ConnectorStateChange = config::read_json(file)?;
            lifecycle(
                client,
                id,
                request.expected_generation,
                false,
                json_output,
                Change::Maintenance(request.draining),
            )
        }
        Connector::Diagnostics { id, context } => Ok(serde_json::to_value(match context {
            Some(context) => client
                .management()
                .connector_context_diagnostics(id, context)?,
            None => client.management().connector_diagnostics(id)?,
        })?),
        _ => super::connector_resource::resource(client, command, json_output),
    }
}

// Shared secret-bearing contracts reject unknown fields before a private file is saved.
pub(crate) fn valid_enrollment(document: &ConnectorEnrollment, connector: &str) -> bool {
    !document.id.is_nil()
        && document.connector_id == connector
        && document.enrollment_token.len() == 43
        && URL_SAFE_NO_PAD
            .decode(&document.enrollment_token)
            .is_ok_and(|bytes| bytes.len() == 32)
        && valid_expiry(&document.expires_at)
}

fn valid_expiry(value: &str) -> bool {
    // PostgreSQL renders timestamptz with a space and may abbreviate an integral
    // UTC offset to +00. Normalize only those differences for timestamp parsing.
    let mut value = value.replacen(' ', "T", 1);
    if value.len() >= 22 && matches!(value.as_bytes()[value.len() - 3], b'+' | b'-') {
        value.push_str(":00");
    }
    OffsetDateTime::parse(&value, &Rfc3339).is_ok()
}

fn save_enrollment(
    client: &EnumerayClient,
    id: &str,
    output: &Path,
    response: Result<Value, Error>,
    creation: bool,
) -> Result<Value, Error> {
    let value = match response {
        Ok(value) => value,
        // Preserve definitive API failures, including authorization and conflicts.
        Err(
            error @ Error::Api {
                status: 400..=499, ..
            },
        ) => return Err(error),
        Err(_) => {
            return Err(enrollment_recovery(
                client,
                id,
                "Enrollment outcome unknown after the request failed. No enrollment file was saved.",
                false,
            ));
        }
    };
    match serde_json::from_value::<ConnectorRegistration>(value.clone()) {
        Ok(ConnectorRegistration::Enrollment(document)) if valid_enrollment(&document, id) => {}
        Ok(ConnectorRegistration::AlreadyRegistered(existing))
            if creation && existing.already_registered && existing.connector_id == id =>
        {
            return Err(enrollment_recovery(
                client,
                id,
                "The connector was already registered; creation replay returned no enrollment secret. No enrollment file was saved.",
                true,
            ));
        }
        _ => {
            return Err(enrollment_recovery(
                client,
                id,
                "Invalid enrollment response; no enrollment file was saved. The request may have committed.",
                false,
            ));
        }
    }
    if config::write_private(output, &value, false).is_err() {
        return Err(enrollment_recovery(
            client,
            id,
            "Enrollment saving was not confirmed. The output may already exist; preserve and inspect it privately before requesting a replacement. The request may have committed.",
            true,
        ));
    }
    Ok(json!({"saved_to":output,"connector_id":id}))
}

fn enrollment_recovery(
    client: &EnumerayClient,
    id: &str,
    reason: &str,
    registration_confirmed: bool,
) -> Error {
    let observed = typed_get::<ConnectorSummary>(client, &["connectors", id]);
    let registered = observed
        .as_ref()
        .is_ok_and(|value| value["connector_id"] == id);
    let inspection = if registered {
        "Inspection confirmed that the connector is registered."
    } else if registration_confirmed {
        "The response confirmed registration, but inspection did not confirm the connector's current state."
    } else {
        "Inspection did not confirm registration; the creation/enrollment outcome remains unknown. Retry creation only with the same connector ID and settings."
    };
    // Resource names are shell-quoted because these commands are recovery advice.
    let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
    let id = quote(id);
    let replacement = if registered || registration_confirmed {
        format!(
            " If no usable enrollment file was retained, explicitly request a replacement with `enumeray connector enrollment {id} --output NEW-ENROLLMENT.json` using a new output path. Replacement invalidates prior unused enrollments."
        )
    } else {
        String::new()
    };
    Error::Invalid(format!(
        "{reason} {inspection} Inspect with `enumeray connector get {id}`.{replacement} Retry an uncertain host enrollment with its same durable identity directory. After successful enrollment, credential replacement needs a new private credential directory; retain the original execution journal."
    ))
}

#[derive(Clone, Copy)]
enum Change {
    Maintenance(bool),
    Authority(bool),
}

impl Change {
    fn description(self) -> &'static str {
        match self {
            Self::Maintenance(true) => {
                "Stop new acceptance and claims; retain queued work for resume. Assigned prelaunch work, pending allocations, execution and delivery continue subject to independent authority."
            }
            Self::Maintenance(false) => {
                "Permit acceptance and claims when readiness and independent authority allow them. Withdrawn connector authority and permanent revocations remain in force."
            }
            Self::Authority(true) => {
                "Block new acceptance, claims and prelaunch execution; request cancellation of pending allocations. Already running execution and delivery continue; maintenance stays unchanged."
            }
            Self::Authority(false) => {
                "Restore only this connector's reversible execution authority. Maintenance, credentials, grants, user blocks and permanent revocations remain in force. Cancellation already requested or confirmed remains in force."
            }
        }
    }
}

fn inspect_generation(
    client: &EnumerayClient,
    id: &str,
    generation: i64,
) -> Result<ConnectorLifecycleStatus, Error> {
    if generation < 1 {
        return Err(Error::Invalid(
            "Connector generation must be positive.".into(),
        ));
    }
    let observed = client.connector_diagnostics(id)?;
    if observed.connector_id != id {
        return Err(Error::Invalid(
            "Connector diagnostics returned a different connector.".into(),
        ));
    }
    if observed.generation != generation {
        return Err(Error::Invalid(format!(
            "Connector changed: expected generation {generation}, observed {}. Inspect the connector before choosing a new change.",
            observed.generation
        )));
    }
    let work = &observed.work;
    let phases = &work.assignment_phases;
    if [
        work.retained_queued_runs,
        work.unfinished_assignments,
        work.pending_delivery_assignments,
        work.pending_uploads,
        work.pending_log_archives,
        work.unconfirmed_assignments,
        phases.preparing,
        phases.waiting_for_backend,
        phases.executing,
        phases.collecting_results,
        phases.uploading_results,
    ]
    .iter()
    .any(|count| *count < 0)
        || work.unconfirmed_assignments > work.unfinished_assignments
        || observed.maintenance_complete != (observed.draining && work.assigned_work_finished())
    {
        return Err(Error::Invalid(
            "Connector diagnostics contained inconsistent maintenance evidence.".into(),
        ));
    }
    Ok(observed)
}

fn work_description(observed: &ConnectorLifecycleStatus) -> String {
    let work = &observed.work;
    format!(
        "retained queued {}; unfinished assignments {} (unconfirmed {}); pending delivery {}; uploads {}; log archives {}",
        work.retained_queued_runs,
        work.unfinished_assignments,
        work.unconfirmed_assignments,
        work.pending_delivery_assignments,
        work.pending_uploads,
        work.pending_log_archives
    )
}

fn lifecycle(
    client: &EnumerayClient,
    id: &str,
    generation: i64,
    dry_run: bool,
    json_output: bool,
    change: Change,
) -> Result<Value, Error> {
    let observed = inspect_generation(client, id, generation)?;
    let (draining, authority_withdrawn) = match change {
        Change::Maintenance(draining) => (draining, observed.authority_withdrawn),
        Change::Authority(withdrawn) => (observed.draining, withdrawn),
    };
    if dry_run {
        return Ok(json!({
            "connector_id":id,
            "observed":observed,
            "proposed":{"draining":draining,"authority_withdrawn":authority_withdrawn},
            "effects":change.description(),
        }));
    }
    if !json_output {
        eprintln!(
            "Connector {id}, generation {generation}: maintenance {} -> {draining}; authority withdrawn {} -> {authority_withdrawn}.\n{}\n{}.",
            observed.draining,
            observed.authority_withdrawn,
            change.description(),
            work_description(&observed)
        );
    }
    let receipt = match change {
        Change::Maintenance(draining) => client.set_connector_maintenance(
            id,
            &ConnectorStateChange {
                draining,
                expected_generation: generation,
            },
        )?,
        Change::Authority(withdrawn) => client.set_connector_authority(
            id,
            &ConnectorAuthorityChange {
                withdrawn,
                expected_generation: generation,
            },
        )?,
    };
    Ok(serde_json::to_value(receipt)?)
}

fn wait_maintenance(
    client: &EnumerayClient,
    id: &str,
    generation: i64,
    interval: u64,
    deadline: u64,
    json_output: bool,
) -> Result<Value, Error> {
    if interval == 0 || deadline == 0 {
        return Err(Error::Invalid(
            "Wait interval and deadline must be positive.".into(),
        ));
    }
    let until = Instant::now()
        .checked_add(Duration::from_secs(deadline))
        .ok_or_else(|| Error::Invalid("Wait deadline is too large.".into()))?;
    let deadline_elapsed = || {
        Error::Invalid("Maintenance wait deadline elapsed; maintenance remains enabled. Inspect the same connector and retained delivery evidence.".into())
    };
    let mut last = String::new();
    loop {
        if Instant::now() >= until {
            return Err(deadline_elapsed());
        }
        let remaining = until.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(deadline_elapsed());
        }
        let bounded = client.with_timeout_limit(remaining)?;
        let observed = inspect_generation(&bounded, id, generation).map_err(|error| {
            if Instant::now() >= until {
                deadline_elapsed()
            } else {
                error
            }
        })?;
        if !observed.draining {
            return Err(Error::Invalid(
                "Connector maintenance is not enabled. Inspect its state before waiting.".into(),
            ));
        }
        if Instant::now() >= until {
            return Err(deadline_elapsed());
        }
        let description = work_description(&observed);
        if !json_output && description != last {
            eprintln!("Connector {id}: {description}.");
            last = description;
        }
        if observed.maintenance_complete {
            return Ok(serde_json::to_value(observed)?);
        }
        std::thread::sleep(
            Duration::from_secs(interval).min(until.saturating_duration_since(Instant::now())),
        );
    }
}
