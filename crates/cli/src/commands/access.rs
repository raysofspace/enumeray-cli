//! Access command workflows.
use super::{typed_get, typed_mutate, typed_paged_get};
use enumeray_contracts::management::*;

use super::{require_new_output, save_secret_output, secret_request_error, stdin_secret};
use crate::args::Access;
use enumeray_client::{EnumerayClient, Error};
use enumeray_contracts::management::{
    AccessPolicyUpdate, AccessRequestCreate, AccessRequestDecision, InvitationAcceptance,
    InvitationCreate,
};
use reqwest::Method;
use serde_json::{Value, json};

pub(crate) fn access(
    client: &EnumerayClient,
    command: &Access,
    json_output: bool,
) -> Result<Value, Error> {
    match command {
        Access::Policy { model } => {
            typed_get::<AccessPolicy>(client, &["models", model, "access-policy"])
        }
        Access::SetPolicy {
            model,
            mode,
            visibility,
            generation,
            dry_run,
        } => {
            let observed = client.management().access_policy(model)?;
            if observed.generation != *generation {
                return Err(Error::Invalid(format!(
                    "Model access policy changed: expected generation {generation}, observed {}. Inspect the current policy before choosing a new change.",
                    observed.generation
                )));
            }
            let request: AccessPolicyUpdate = serde_json::from_value(
                json!({"mode":mode,"visibility":visibility,"expected_generation":generation}),
            )?;
            let proposed_visibility = request.visibility.unwrap_or(observed.visibility);
            if *dry_run {
                return Ok(json!({
                    "model_id": model,
                    "observed": observed,
                    "proposed": {"mode": request.mode, "visibility": proposed_visibility},
                }));
            }
            if !json_output {
                eprintln!(
                    "Model {model}, generation {}: mode {} -> {}; visibility {} -> {}.",
                    observed.generation,
                    observed.mode.as_str(),
                    request.mode.as_str(),
                    observed.visibility.as_str(),
                    proposed_visibility.as_str(),
                );
            }
            typed_mutate::<AccessPolicy, _>(
                client,
                Method::PUT,
                &["models", model, "access-policy"],
                &request,
            )
        }
        Access::Invitations { model, page } => typed_paged_get::<Collection<InvitationSummary>>(
            client,
            &["models", model, "invitations"],
            page,
        ),
        Access::Invite {
            model,
            user,
            email,
            output,
        } => {
            let _output_lock = require_new_output(output)?;
            let recovery = "Inspect access invitations for this model before explicitly issuing another invitation; an earlier invitation may still be active.";
            let value = typed_mutate::<InvitationReceipt, _>(
                client,
                Method::POST,
                &["models", model, "invitations"],
                &InvitationCreate {
                    recipient_id: *user,
                    recipient_email: email.clone(),
                },
            )
            .map_err(|error| secret_request_error(error, "Invitation creation", recovery))?;
            save_secret_output(output, &value, "Invitation creation", recovery)?;
            Ok(
                json!({"saved_to":output,"invitation_id":value.get("id"),"expires_at":value.get("expires_at")}),
            )
        }
        Access::Accept { model } => typed_mutate::<AcceptanceReceipt, _>(
            client,
            Method::POST,
            &["models", model, "invitations", "accept"],
            &InvitationAcceptance {
                token: stdin_secret()?,
            },
        ),
        Access::Request { model, message } => typed_mutate::<AccessRequestReceipt, _>(
            client,
            Method::POST,
            &["models", model, "access-requests"],
            &AccessRequestCreate {
                message: message.clone(),
            },
        ),
        Access::Requests { model, page } => typed_paged_get::<Collection<AccessRequestSummary>>(
            client,
            &["models", model, "access-requests"],
            page,
        ),
        Access::Decide {
            model,
            request,
            decision,
        } => typed_mutate::<AccessDecisionReceipt, _>(
            client,
            Method::PUT,
            &["models", model, "access-requests", request],
            &AccessRequestDecision {
                decision: decision.clone(),
            },
        ),
        Access::Block {
            model,
            user,
            generation,
        } => typed_mutate::<AccessReceipt, _>(
            client,
            Method::PUT,
            &["models", model, "access", &user.to_string()],
            &json!({"allowed":false,"expected_generation":generation}),
        ),
        Access::Unblock {
            model,
            user,
            generation,
        } => typed_mutate::<AccessReceipt, _>(
            client,
            Method::PUT,
            &["models", model, "access", &user.to_string()],
            &json!({"allowed":true,"expected_generation":generation}),
        ),
    }
}
