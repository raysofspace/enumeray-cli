//! Exact author-trial selection shared by folder and retained prepared operations.
use enumeray_client::{EnumerayClient, Error, resource_path};
use enumeray_contracts::{
    execution::InstallationDescriptor,
    management::{Availability, PublicationReleaseAuthority},
    models::{ScientificDefinition, validation},
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Deserialize)]
struct Publication {
    id: String,
    model_id: String,
    context_id: String,
    grant_id: Uuid,
    status: String,
    descriptor: InstallationDescriptor,
    definition: ScientificDefinition,
    execution_location: String,
    disclosure: String,
    trial_availability: Availability,
    release_authority: PublicationReleaseAuthority,
}

pub(crate) struct Candidate {
    pub publication: Value,
    pub publication_delegation_id: Option<Uuid>,
}

pub(crate) fn inspect(client: &EnumerayClient, model: &str, id: &str) -> Result<Candidate, Error> {
    let publication = client.get(&resource_path(&["models", model, "publications", id])?)?;
    validate(publication, model, id)
}

/// Validate the original snapshot as well as fresh discovery. Mutable availability
/// is checked only on fresh selection/preflight, never during exact POST recovery.
pub(crate) fn validate(mut publication: Value, model: &str, id: &str) -> Result<Candidate, Error> {
    // Report an authoritative blocker even when an unavailable response omits
    // installation details that cannot currently be inspected.
    if publication["trial_availability"]["available"] != true {
        let reason = publication["trial_availability"]["unavailable_reason"]
            .as_str()
            .unwrap_or("trial_authority_or_readiness_unavailable");
        return Err(Error::Invalid(format!(
            "Candidate trial unavailable: {reason}. Inspect model publication and retain this candidate ID."
        )));
    }
    let selected: Publication = serde_json::from_value(publication.clone())?;
    if selected.id != id
        || selected.model_id != model
        || publication
            .get("publication_id")
            .is_some_and(|publication_id| publication_id != id)
        || !validation::valid_model_id(&selected.context_id)
        || selected.grant_id.is_nil()
        || !matches!(selected.status.as_str(), "ready" | "published")
        || selected.execution_location.trim().is_empty()
        || selected.disclosure.trim().is_empty()
        || selected.descriptor.provider_binding_id.is_empty()
        || selected.descriptor.installation
            != enumeray_contracts::execution::InstallationIdentity::derive(&selected.definition)
        || selected.release_authority.grant_holder_user_id.is_nil()
        || selected
            .release_authority
            .publication_delegation_id
            .is_some_and(|delegation| delegation.is_nil())
        || !selected.trial_availability.available
        || !selected.release_authority.available
    {
        return Err(Error::Invalid(
            "Candidate inspection returned inconsistent publication or trial authority evidence."
                .into(),
        ));
    }
    validation::validate_definition(&selected.definition)
        .map_err(|error| Error::Invalid(error.to_string()))?;
    publication["publication_id"] = json!(id);
    Ok(Candidate {
        publication,
        publication_delegation_id: selected.release_authority.publication_delegation_id,
    })
}

pub(crate) fn validate_actor(publication: &Value, user_id: Uuid) -> Result<(), Error> {
    if publication["release_authority"]["publication_delegation_id"].is_null()
        && publication["release_authority"]["grant_holder_user_id"] != user_id.to_string()
    {
        return Err(Error::Invalid(
            "Candidate trial requires the captured grant holder or an exact named delegation."
                .into(),
        ));
    }
    Ok(())
}

/// Readiness, trial evidence and publication generation can advance without
/// changing the exact scientific installation or the reviewed destination.
pub(crate) fn identity(publication: &Value) -> Value {
    json!({"id":publication["id"],"model_id":publication["model_id"],
        "definition":publication["definition"],"context_id":publication["context_id"],
        "grant_id":publication["grant_id"],"descriptor":publication["descriptor"],
        "execution_location":publication["execution_location"],"disclosure":publication["disclosure"]})
}
