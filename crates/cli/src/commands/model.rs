//! Model command workflows.
use super::{typed_get, typed_mutate, typed_paged_get};
use enumeray_contracts::management::*;

use super::{accept_transfer, member, paged_get};
use crate::{args::Model, manifest::publication_source};
use enumeray_client::{EnumerayClient, Error};
use enumeray_contracts::{
    management::{
        GenerationChange, ModelCreate, PublicationActivation, PublicationRetirement, TransferCreate,
    },
    models::ModelMetadata,
};
use reqwest::Method;
use serde_json::Value;

pub(crate) fn model(client: &EnumerayClient, command: &Model) -> Result<Value, Error> {
    match command {
        Model::List { page } => paged_get(client, &["models"], page),
        Model::Get { id } => typed_get::<ManagedModel>(client, &["models", id]),
        Model::Create {
            id,
            name,
            description,
            file,
        } => {
            let metadata = if let Some(file) = file {
                let (_, source) = publication_source(file)?;
                if source.model.id != *id {
                    return Err(Error::Invalid(
                        "Publication source model ID differs from the selected model.".into(),
                    ));
                }
                source.model
            } else {
                ModelMetadata {
                    id: id.clone(),
                    name: name
                        .clone()
                        .ok_or_else(|| Error::Invalid("Provide --name or --file.".into()))?,
                    description: description.clone(),
                }
            };
            typed_mutate::<ManagedModel, _>(
                client,
                Method::POST,
                &["models"],
                &ModelCreate { model: metadata },
            )
        }
        Model::Validate { .. } | Model::Init { .. } | Model::Release(_) => unreachable!(),
        Model::Publications { id, page } => typed_paged_get::<Collection<PublicationSummary>>(
            client,
            &["models", id, "publications"],
            page,
        ),
        Model::Publication { id, publication } => {
            typed_get::<PublicationDetail>(client, &["models", id, "publications", publication])
        }
        Model::RevokePublication { id, publication } => Ok(serde_json::to_value(
            client.management().revoke_publication(id, publication)?,
        )?),
        Model::RetirePublication {
            id,
            publication,
            generation,
        } => typed_mutate::<PublicationRetirementReceipt, _>(
            client,
            Method::PUT,
            &["models", id, "publications", publication, "retirement"],
            &PublicationRetirement {
                expected_generation: *generation,
            },
        ),
        Model::Publish {
            id,
            publication,
            expected_current,
        } => typed_mutate::<PublicationActivationReceipt, _>(
            client,
            Method::POST,
            &["models", id, "publications", publication, "activation"],
            &PublicationActivation {
                publication_delegation_id: None,
                expected_current_publication_id: expected_current.clone(),
            },
        ),
        Model::Runs { id, page } => {
            typed_paged_get::<Collection<ManagedRun>>(client, &["models", id, "runs"], page)
        }
        Model::Audit { id, page } => {
            typed_paged_get::<Collection<AuditEvent>>(client, &["audit", "model", id], page)
        }
        Model::Members { id } => typed_get::<ResourceMembers>(client, &["models", id, "members"]),
        Model::MemberAdd {
            id,
            user,
            generation,
        } => member(client, "models", id, *user, *generation, true),
        Model::MemberRemove {
            id,
            user,
            generation,
        } => member(client, "models", id, *user, *generation, false),
        Model::Transfer { id, to, generation } => typed_mutate::<TransferProposalReceipt, _>(
            client,
            Method::POST,
            &["models", id, "ownership-transfers"],
            &TransferCreate {
                to_user_id: *to,
                expected_generation: *generation,
            },
        ),
        Model::AcceptTransfer { id, transfer_id } => {
            accept_transfer(client, "model", id, transfer_id)
        }
        Model::Archive { id, generation } => typed_mutate::<ArchiveReceipt, _>(
            client,
            Method::POST,
            &["models", id, "archive"],
            &GenerationChange {
                expected_generation: *generation,
            },
        ),
    }
}
