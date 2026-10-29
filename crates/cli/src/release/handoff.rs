//! Checkpoint handoff mutation identities before sending any remote write.
use super::*;
use state::DelegationAttempt;

pub(super) fn execute(
    client: &EnumerayClient,
    command: &Command,
    directory: &Directory,
    release: &Release,
    progress: &mut Progress,
    snapshot: &Snapshot,
) -> Result<Completion, Failure> {
    let (user, id, generation, dry_run) = match command {
        Command::Delegate {
            user,
            generation,
            dry_run,
            ..
        } => (Some(*user), None, *generation, *dry_run),
        Command::RevokeDelegation {
            delegation_id,
            generation,
            dry_run,
            ..
        } => (None, Some(*delegation_id), *generation, *dry_run),
        _ => unreachable!(),
    };
    if generation < 1 {
        return Err(invalid("A positive observed model generation is required.").into());
    }
    let publication = snapshot
        .publication
        .as_ref()
        .ok_or_else(|| invalid("Register the candidate before delegating it."))?;
    if user.is_some() && publication["release_authority"]["can_delegate"] != true {
        return Err(
            invalid("Only the active captured-grant holder may create a named handoff.").into(),
        );
    }
    let request = if let Some(user) = user {
        json!({"user_id":user,"expected_generation":generation})
    } else {
        json!({"expected_generation":generation})
    };
    if dry_run {
        return Ok(Completion {
            value: Some(
                json!({"dry_run":true,"model_id":release.model_id,"publication_id":release.candidate.id,"model_generation":snapshot.model["generation"],"release_authority":publication["release_authority"],"delegation_id":id,"request":request}),
            ),
            exit: 0,
        });
    }
    let index = progress.delegation_attempts.iter().position(|a| {
        a.user_id == user && a.expected_generation == generation && id.is_none_or(|id| a.id == id)
    });
    let index = if let Some(index) = index {
        index
    } else {
        if progress.delegation_attempts.len() >= 128 {
            return Err(invalid("Handoff history is full; adopt this candidate into a new private release directory.").into());
        }
        progress.delegation_attempts.push(DelegationAttempt {
            id: id.unwrap_or_else(Uuid::new_v4),
            user_id: user,
            expected_generation: generation,
            response: None,
        });
        progress.delegation_attempts.len() - 1
    };
    directory.write("state.json", progress, true)?;
    let attempt = &progress.delegation_attempts[index];
    // Reconcile with the server even after a successful reply: retained revocation
    // must remain visible and never be presented as an active replayed handoff.
    let result = super::management_json(
        client.request::<enumeray_contracts::management::PublicationDelegation, _>(
            if user.is_some() {
                reqwest::Method::PUT
            } else {
                reqwest::Method::DELETE
            },
            &resource_path(&[
                "models",
                &release.model_id,
                "publications",
                &release.candidate.id,
                "delegations",
                &attempt.id.to_string(),
            ])?,
            Some(&request),
            None,
        )?,
    )?;
    if result["id"] != attempt.id.to_string()
        || result["publication_id"] != release.candidate.id
        || user.is_some_and(|u| result["user_id"] != u.to_string())
        || result["grant_id"] != publication["grant_id"]
        || result["created_by"] != publication["release_authority"]["grant_holder_user_id"]
        || result["user_id"]
            .as_str()
            .and_then(|id| id.parse::<Uuid>().ok())
            .is_none_or(|id| id.is_nil())
        || result["created_by"]
            .as_str()
            .and_then(|id| id.parse::<Uuid>().ok())
            .is_none_or(|id| id.is_nil())
        || result["created_at"].as_str().is_none_or(str::is_empty)
        || result["expected_generation"].as_i64().is_none_or(|n| n < 1)
        || (user.is_some() && result["expected_generation"] != generation)
        || !matches!(
            result.get("revoked_at"),
            Some(Value::Null | Value::String(_))
        )
        || !matches!(
            result.get("revocation_reason"),
            Some(Value::Null | Value::String(_))
        )
        || result["revoked_at"].is_null() != result["revocation_reason"].is_null()
        || result["revoked_at"].as_str().is_some_and(str::is_empty)
        || result["revocation_reason"]
            .as_str()
            .is_some_and(str::is_empty)
        || (user.is_none() && result["revoked_at"].is_null())
    {
        return Err(invalid(
            "Handoff reply conflicts with the saved request; retry this operation to reconcile.",
        )
        .into());
    }
    progress.delegation_attempts[index].response = Some(result.clone());
    directory.write("state.json", progress, true)?;
    Ok(Completion {
        value: Some(result),
        exit: 0,
    })
}
