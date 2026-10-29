//! Infrastructure command workflows.
use super::{typed_get, typed_mutate, typed_paged_get};
use enumeray_contracts::management::*;

use super::{accept_transfer, member};
use crate::{args::Infra, manifest::manifest};
use enumeray_client::{EnumerayClient, Error};
use enumeray_contracts::management::{
    ContextAcceptedWorkPolicy, ContextCreate, ContextCutover, ContextDependencySection,
    ContextLifecycleState, ContextLifecycleStatus, ContextMetadata, ContextMetadataUpdate,
    GenerationChange, GrantCreate, GrantUpdate, InfrastructureCreate, TransferCreate,
};
use reqwest::Method;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

pub(crate) fn infrastructure(
    client: &EnumerayClient,
    command: &Infra,
    json_output: bool,
) -> Result<Value, Error> {
    match command {
        Infra::List { page } => {
            typed_paged_get::<Collection<InfrastructureSummary>>(client, &["infrastructures"], page)
        }
        Infra::Available { page } => {
            typed_paged_get::<Collection<AvailableGrant>>(client, &["compute-grants"], page)
        }
        Infra::Get { id } => typed_get::<InfrastructureRecord>(client, &["infrastructures", id]),
        Infra::Create {
            id,
            label,
            description,
        } => typed_mutate::<InfrastructureRecord, _>(
            client,
            Method::POST,
            &["infrastructures"],
            &InfrastructureCreate {
                id: id.clone(),
                label: label.clone(),
                description: description.clone(),
            },
        ),
        Infra::Contexts { id, page } => typed_paged_get::<Collection<ContextSummary>>(
            client,
            &["infrastructures", id, "contexts"],
            page,
        ),
        Infra::ContextCreate { id, file } => {
            let spec: ContextCreate = manifest(file, "execution-context")?;
            spec.metadata
                .validate()
                .map_err(|message| Error::Invalid(message.into()))?;
            spec.definition
                .validate_execution()
                .map_err(|message| Error::Invalid(message.into()))?;
            typed_mutate::<ContextRegistration, _>(
                client,
                Method::POST,
                &["infrastructures", id, "contexts"],
                &spec,
            )
        }
        Infra::ContextUpdate {
            infra,
            context,
            location,
            disclosure,
            generation,
        } => {
            let metadata = ContextMetadata {
                location: location.clone(),
                disclosure: disclosure.clone(),
            };
            metadata
                .validate()
                .map_err(|message| Error::Invalid(message.into()))?;
            typed_mutate::<ContextMetadataReceipt, _>(
                client,
                Method::PUT,
                &["infrastructures", infra, "contexts", context, "metadata"],
                &ContextMetadataUpdate {
                    metadata,
                    expected_generation: *generation,
                },
            )
        }
        Infra::ContextGet { infra, context } => Ok(serde_json::to_value(inspect_context(
            client, infra, context,
        )?)?),
        Infra::ContextReplacementPlan {
            infra,
            context,
            replacement,
            section,
            page,
        } => Ok(serde_json::to_value(client.context_replacement_plan(
            infra,
            context,
            replacement.as_deref(),
            (*section).into(),
            page.limit,
            page.cursor.as_deref(),
        )?)?),
        Infra::ContextCutover {
            infra,
            context,
            replacement,
            accepted_work,
            generation,
            dry_run,
        } => context_change(
            client,
            infra,
            context,
            *generation,
            *dry_run,
            json_output,
            ContextChange::Cutover {
                replacement: replacement.clone(),
                accepted_work: (*accepted_work).into(),
            },
        ),
        Infra::ContextReopen {
            infra,
            context,
            generation,
            dry_run,
        } => context_change(
            client,
            infra,
            context,
            *generation,
            *dry_run,
            json_output,
            ContextChange::Reopen,
        ),
        Infra::ContextReleaseConfigurations {
            infra,
            context,
            generation,
            dry_run,
        } => context_change(
            client,
            infra,
            context,
            *generation,
            *dry_run,
            json_output,
            ContextChange::ReleaseConfigurations,
        ),
        Infra::ContextRetire {
            infra,
            context,
            generation,
            dry_run,
        } => context_change(
            client,
            infra,
            context,
            *generation,
            *dry_run,
            json_output,
            ContextChange::Retire,
        ),
        Infra::ContextWaitRetirement {
            infra,
            context,
            generation,
            interval,
            deadline,
        } => wait_retirement(
            client,
            infra,
            context,
            *generation,
            *interval,
            *deadline,
            json_output,
        ),
        Infra::Grants { id, page } => typed_paged_get::<Collection<ComputeGrant>>(
            client,
            &["infrastructures", id, "grants"],
            page,
        ),
        Infra::Grant {
            id,
            context,
            user,
            max_queued_runs,
            max_outstanding_assignments,
        } => typed_mutate::<ComputeGrant, _>(
            client,
            Method::POST,
            &["infrastructures", id, "grants"],
            &GrantCreate {
                context_id: context.clone(),
                user_id: *user,
                max_queued_runs: *max_queued_runs,
                max_outstanding_assignments: *max_outstanding_assignments,
            },
        ),
        Infra::GrantGet { infra, grant_id } => Ok(serde_json::to_value(
            client.compute_grant(infra, *grant_id)?,
        )?),
        Infra::GrantUpdate {
            infra,
            grant_id,
            max_queued_runs,
            max_outstanding_assignments,
            generation,
        } => {
            if *generation < 1
                || !(1..=10000).contains(max_queued_runs)
                || !(1..=1024).contains(max_outstanding_assignments)
            {
                return Err(Error::Invalid("Grant limits require a positive generation, 1–10000 queued runs and 1–1024 outstanding assignments.".into()));
            }
            Ok(serde_json::to_value(client.update_compute_grant(
                infra,
                *grant_id,
                &GrantUpdate {
                    expected_generation: *generation,
                    max_queued_runs: *max_queued_runs,
                    max_outstanding_assignments: *max_outstanding_assignments,
                },
            )?)?)
        }
        Infra::RevokeGrant { id, grant_id } => typed_mutate::<RevokedGrant, _>(
            client,
            Method::DELETE,
            &["infrastructures", id, "grants", grant_id],
            &Value::Null,
        ),
        Infra::Runs { id, page } => typed_paged_get::<Collection<OperationalRun>>(
            client,
            &["infrastructures", id, "runs"],
            page,
        ),
        Infra::Audit { id, page } => typed_paged_get::<Collection<AuditEvent>>(
            client,
            &["audit", "infrastructure", id],
            page,
        ),
        Infra::Members { id } => {
            typed_get::<ResourceMembers>(client, &["infrastructures", id, "members"])
        }
        Infra::MemberAdd {
            id,
            user,
            generation,
        } => member(client, "infrastructures", id, *user, *generation, true),
        Infra::MemberRemove {
            id,
            user,
            generation,
        } => member(client, "infrastructures", id, *user, *generation, false),
        Infra::Transfer { id, to, generation } => typed_mutate::<TransferProposalReceipt, _>(
            client,
            Method::POST,
            &["infrastructures", id, "ownership-transfers"],
            &TransferCreate {
                to_user_id: *to,
                expected_generation: *generation,
            },
        ),
        Infra::AcceptTransfer { id, transfer_id } => {
            accept_transfer(client, "infrastructure", id, transfer_id)
        }
        Infra::Archive { id, generation } => typed_mutate::<ArchiveReceipt, _>(
            client,
            Method::POST,
            &["infrastructures", id, "archive"],
            &GenerationChange {
                expected_generation: *generation,
            },
        ),
    }
}

enum ContextChange {
    Cutover {
        replacement: Option<String>,
        accepted_work: ContextAcceptedWorkPolicy,
    },
    Reopen,
    ReleaseConfigurations,
    Retire,
}

impl ContextChange {
    fn operation(&self) -> &'static str {
        match self {
            Self::Cutover { .. } => "cutover",
            Self::Reopen => "reopen",
            Self::ReleaseConfigurations => "configuration_retirements",
            Self::Retire => "retirement",
        }
    }

    fn effects(&self) -> &'static str {
        match self {
            Self::Cutover {
                accepted_work: ContextAcceptedWorkPolicy::Drain,
                ..
            } => {
                "Close new grants, publications, activations, submissions and trials; continue dispatching already accepted queued runs under existing authority and capacity checks. Assigned work and delivery continue at their original context."
            }
            Self::Cutover {
                accepted_work: ContextAcceptedWorkPolicy::CancelQueued,
                ..
            } => {
                "Close new grants, publications, activations, submissions and trials; cancel only runs still queued when cutover commits. Assigned work and delivery continue at their original context."
            }
            Self::Reopen => {
                "Restore future eligibility. Cancelled runs, configuration retirement requests, permanent revocations and author activation choices remain unchanged."
            }
            Self::ReleaseConfigurations => {
                "Request retirement of obsolete source publication configuration, including abandoned candidates. Connector maintenance removes unused references; protected release files remain subject to local cleanup checks."
            }
            Self::Retire => {
                "Permanently archive this context after all publication, work, delivery and connector acknowledgement blockers clear. Retain historical evidence and revocation fences."
            }
        }
    }
}

fn inspect_context(
    client: &EnumerayClient,
    infra: &str,
    context: &str,
) -> Result<ContextLifecycleStatus, Error> {
    let observed = client.context_status(infra, context)?;
    if observed.id != context || observed.infrastructure_id != infra {
        return Err(Error::Invalid(
            "Context inspection returned a different resource.".into(),
        ));
    }
    for work in [&observed.work, &observed.connector_work] {
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
        .any(|n| *n < 0)
            || work.unconfirmed_assignments > work.unfinished_assignments
        {
            return Err(Error::Invalid(
                "Context inspection contained inconsistent work evidence.".into(),
            ));
        }
    }
    if observed.generation < 1
        || observed.connector_generation < 1
        || observed.live_current_publications < 0
        || observed.retained_publications < 0
        || observed.pending_configuration_retirements < 0
        || (observed.retirement_ready
            && (observed.state != ContextLifecycleState::Draining
                || observed.live_current_publications != 0
                || observed.retained_publications != 0
                || observed.pending_configuration_retirements != 0
                || observed.work.retained_queued_runs != 0
                || !observed.work.assigned_work_finished()
                || observed
                    .configuration_removal_generation
                    .is_none_or(|generation| {
                        generation < 1 || observed.observed_generation < generation
                    })))
    {
        return Err(Error::Invalid(
            "Context inspection contained inconsistent retirement evidence.".into(),
        ));
    }
    Ok(observed)
}

fn check_generation(observed: &ContextLifecycleStatus, generation: i64) -> Result<(), Error> {
    if generation < 1 {
        return Err(Error::Invalid(
            "Context generation must be positive.".into(),
        ));
    }
    if observed.generation != generation {
        return Err(Error::Invalid(format!(
            "Context changed: expected generation {generation}, observed {}. Inspect the context before choosing a new change.",
            observed.generation
        )));
    }
    Ok(())
}

fn work_description(observed: &ContextLifecycleStatus) -> String {
    let work = &observed.work;
    format!(
        "queued {}; unfinished assignments {} (unconfirmed {}); pending delivery {}; uploads {}; log archives {}; retained publication configurations {}; pending configuration retirements {}",
        work.retained_queued_runs,
        work.unfinished_assignments,
        work.unconfirmed_assignments,
        work.pending_delivery_assignments,
        work.pending_uploads,
        work.pending_log_archives,
        observed.retained_publications,
        observed.pending_configuration_retirements
    )
}

fn context_change(
    client: &EnumerayClient,
    infra: &str,
    context: &str,
    generation: i64,
    dry_run: bool,
    json_output: bool,
    change: ContextChange,
) -> Result<Value, Error> {
    if generation < 1 {
        return Err(Error::Invalid(
            "Context generation must be positive.".into(),
        ));
    }
    let observed = inspect_context(client, infra, context)?;
    let request = match &change {
        ContextChange::Cutover {
            replacement,
            accepted_work,
        } => serde_json::to_value(ContextCutover {
            expected_generation: generation,
            replacement_context_id: replacement.clone(),
            accepted_work: *accepted_work,
        })?,
        _ => serde_json::to_value(GenerationChange {
            expected_generation: generation,
        })?,
    };
    if dry_run {
        check_generation(&observed, generation)?;
        let plan = if let ContextChange::Cutover { replacement, .. } = &change {
            let plan = client.context_replacement_plan(
                infra,
                context,
                replacement.as_deref(),
                ContextDependencySection::Publications,
                None,
                None,
            )?;
            check_generation(&plan.context, generation)?;
            Some(plan)
        } else {
            None
        };
        return Ok(
            json!({"dry_run":true,"operation":change.operation(),"observed":observed,
            "request":request,"effects":change.effects(),"plan":plan}),
        );
    }
    if !json_output {
        eprintln!(
            "Context {context}: {} at requested generation {generation} (observed {}).\n{}\n{}.",
            change.operation(),
            observed.generation,
            change.effects(),
            work_description(&observed)
        );
    }
    // Send the original guard once. The service can return an exact retained receipt
    // after a lost reply; only the service decides whether a stale request is a replay.
    let guard = GenerationChange {
        expected_generation: generation,
    };
    let receipt = match change {
        ContextChange::Cutover {
            replacement,
            accepted_work,
        } => client.cutover_context(
            infra,
            context,
            &ContextCutover {
                expected_generation: generation,
                replacement_context_id: replacement,
                accepted_work,
            },
        )?,
        ContextChange::Reopen => client.reopen_context(infra, context, &guard)?,
        ContextChange::ReleaseConfigurations => {
            client.release_context_configurations(infra, context, &guard)?
        }
        ContextChange::Retire => client.retire_context(infra, context, &guard)?,
    };
    Ok(serde_json::to_value(receipt)?)
}

fn wait_retirement(
    client: &EnumerayClient,
    infra: &str,
    context: &str,
    generation: i64,
    interval: u64,
    deadline: u64,
    json_output: bool,
) -> Result<Value, Error> {
    if generation < 1 || interval == 0 || deadline == 0 {
        return Err(Error::Invalid(
            "Context generation, wait interval and deadline must be positive.".into(),
        ));
    }
    let until = Instant::now()
        .checked_add(Duration::from_secs(deadline))
        .ok_or_else(|| Error::Invalid("Wait deadline is too large.".into()))?;
    let elapsed = || {
        Error::Invalid("Context retirement wait deadline elapsed; inspect context work, configuration acknowledgements and local journal evidence. Context state is unchanged.".into())
    };
    let mut last = String::new();
    loop {
        let remaining = until.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(elapsed());
        }
        let bounded = client.with_timeout_limit(remaining)?;
        let observed = inspect_context(&bounded, infra, context).map_err(|error| {
            if Instant::now() >= until {
                elapsed()
            } else {
                error
            }
        })?;
        check_generation(&observed, generation)?;
        if observed.state != ContextLifecycleState::Draining {
            return Err(Error::Invalid(
                "Context must be draining before waiting for retirement readiness.".into(),
            ));
        }
        if Instant::now() >= until {
            return Err(elapsed());
        }
        let description = work_description(&observed);
        if !json_output && description != last {
            eprintln!("Context {context}: {description}.");
            last = description;
        }
        if observed.retirement_ready {
            return Ok(serde_json::to_value(observed)?);
        }
        std::thread::sleep(
            Duration::from_secs(interval).min(until.saturating_duration_since(Instant::now())),
        );
    }
}
