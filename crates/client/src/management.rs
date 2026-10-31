//! Typed management workflows. General request methods remain available for extensions.
use crate::{EnumerayClient, Error, resource_path};
use enumeray_contracts::management::*;
use reqwest::Method;

pub struct ManagementClient<'a> {
    client: &'a EnumerayClient,
}
impl EnumerayClient {
    pub fn management(&self) -> ManagementClient<'_> {
        ManagementClient { client: self }
    }
}
impl ManagementClient<'_> {
    pub fn context_preflight(
        &self,
        connector: &str,
        context: &str,
    ) -> Result<enumeray_contracts::setup::ContextPreflightStatus, Error> {
        self.client.request::<_, serde_json::Value>(
            Method::GET,
            &resource_path(&["connectors", connector, "contexts", context, "preflight"])?,
            None,
            None,
        )
    }
    pub fn request_preflight(
        &self,
        connector: &str,
        context: &str,
        request_id: uuid::Uuid,
        request: &enumeray_contracts::setup::PreflightRequest,
    ) -> Result<enumeray_contracts::setup::ContextPreflightStatus, Error> {
        self.client.request(
            Method::PUT,
            &resource_path(&[
                "connectors",
                connector,
                "contexts",
                context,
                "preflight",
                &request_id.to_string(),
            ])?,
            Some(request),
            None,
        )
    }
    pub fn revoke_publication(
        &self,
        model_id: &str,
        publication_id: &str,
    ) -> Result<PublicationRevocationReceipt, Error> {
        self.client.request::<_, serde_json::Value>(
            Method::DELETE,
            &resource_path(&["models", model_id, "publications", publication_id])?,
            None,
            None,
        )
    }

    pub fn managed_model(&self, model_id: &str) -> Result<ManagedModel, Error> {
        let path = resource_path(&["models", model_id])?;
        self.client
            .request::<ManagedModel, serde_json::Value>(Method::GET, &path, None, None)
    }
    pub fn create_model(&self, request: &ModelCreate) -> Result<ManagedModel, Error> {
        let path = resource_path(&["models"])?;
        self.client
            .request::<ManagedModel, ModelCreate>(Method::POST, &path, Some(request), None)
    }
    pub fn publications(
        &self,
        model_id: &str,
        query: &[(&str, &str)],
    ) -> Result<Collection<PublicationSummary>, Error> {
        let path = resource_path(&["models", model_id, "publications"])?;
        let mut url = reqwest::Url::parse(&format!("https://enumeray.invalid{path}"))
            .expect("encoded resource path");
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query.iter().copied());
        }
        let path = format!(
            "{}{}",
            url.path(),
            url.query().map(|q| format!("?{q}")).unwrap_or_default()
        );
        self.client
            .request::<Collection<PublicationSummary>, serde_json::Value>(
                Method::GET,
                &path,
                None,
                None,
            )
    }
    pub fn register_publication(
        &self,
        model_id: &str,
        request: &PublicationCreate,
    ) -> Result<PublicationRegistration, Error> {
        let path = resource_path(&["models", model_id, "publications"])?;
        self.client
            .request::<PublicationRegistration, PublicationCreate>(
                Method::POST,
                &path,
                Some(request),
                None,
            )
    }
    pub fn publication(
        &self,
        model_id: &str,
        publication_id: &str,
    ) -> Result<PublicationDetail, Error> {
        let path = resource_path(&["models", model_id, "publications", publication_id])?;
        self.client
            .request::<PublicationDetail, serde_json::Value>(Method::GET, &path, None, None)
    }
    pub fn activate_publication(
        &self,
        model_id: &str,
        publication_id: &str,
        request: &PublicationActivation,
    ) -> Result<PublicationActivationReceipt, Error> {
        let path = resource_path(&[
            "models",
            model_id,
            "publications",
            publication_id,
            "activation",
        ])?;
        self.client
            .request::<PublicationActivationReceipt, PublicationActivation>(
                Method::POST,
                &path,
                Some(request),
                None,
            )
    }
    pub fn retire_publication(
        &self,
        model_id: &str,
        publication_id: &str,
        request: &PublicationRetirement,
    ) -> Result<PublicationRetirementReceipt, Error> {
        let path = resource_path(&[
            "models",
            model_id,
            "publications",
            publication_id,
            "retirement",
        ])?;
        self.client
            .request::<PublicationRetirementReceipt, PublicationRetirement>(
                Method::PUT,
                &path,
                Some(request),
                None,
            )
    }
    pub fn model_runs(
        &self,
        model_id: &str,
        query: &[(&str, &str)],
    ) -> Result<Collection<ManagedRun>, Error> {
        let path = resource_path(&["models", model_id, "runs"])?;
        let mut url = reqwest::Url::parse(&format!("https://enumeray.invalid{path}"))
            .expect("encoded resource path");
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query.iter().copied());
        }
        let path = format!(
            "{}{}",
            url.path(),
            url.query().map(|q| format!("?{q}")).unwrap_or_default()
        );
        self.client
            .request::<Collection<ManagedRun>, serde_json::Value>(Method::GET, &path, None, None)
    }
    pub fn access_policy(&self, model_id: &str) -> Result<AccessPolicy, Error> {
        let path = resource_path(&["models", model_id, "access-policy"])?;
        self.client
            .request::<AccessPolicy, serde_json::Value>(Method::GET, &path, None, None)
    }
    pub fn set_access_policy(
        &self,
        model_id: &str,
        request: &AccessPolicyUpdate,
    ) -> Result<AccessPolicy, Error> {
        let path = resource_path(&["models", model_id, "access-policy"])?;
        self.client.request::<AccessPolicy, AccessPolicyUpdate>(
            Method::PUT,
            &path,
            Some(request),
            None,
        )
    }
    pub fn change_access(
        &self,
        model_id: &str,
        user_id: &str,
        request: &AccessChange,
    ) -> Result<AccessReceipt, Error> {
        let path = resource_path(&["models", model_id, "access", user_id])?;
        self.client
            .request::<AccessReceipt, AccessChange>(Method::PUT, &path, Some(request), None)
    }
    pub fn invitations(
        &self,
        model_id: &str,
        query: &[(&str, &str)],
    ) -> Result<Collection<InvitationSummary>, Error> {
        let path = resource_path(&["models", model_id, "invitations"])?;
        let mut url = reqwest::Url::parse(&format!("https://enumeray.invalid{path}"))
            .expect("encoded resource path");
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query.iter().copied());
        }
        let path = format!(
            "{}{}",
            url.path(),
            url.query().map(|q| format!("?{q}")).unwrap_or_default()
        );
        self.client
            .request::<Collection<InvitationSummary>, serde_json::Value>(
                Method::GET,
                &path,
                None,
                None,
            )
    }
    pub fn invite(
        &self,
        model_id: &str,
        request: &InvitationCreate,
    ) -> Result<InvitationReceipt, Error> {
        let path = resource_path(&["models", model_id, "invitations"])?;
        self.client.request::<InvitationReceipt, InvitationCreate>(
            Method::POST,
            &path,
            Some(request),
            None,
        )
    }
    pub fn accept_invitation(
        &self,
        model_id: &str,
        request: &InvitationAcceptance,
    ) -> Result<AcceptanceReceipt, Error> {
        let path = resource_path(&["models", model_id, "invitations", "accept"])?;
        self.client
            .request::<AcceptanceReceipt, InvitationAcceptance>(
                Method::POST,
                &path,
                Some(request),
                None,
            )
    }
    pub fn access_requests(
        &self,
        model_id: &str,
        query: &[(&str, &str)],
    ) -> Result<Collection<AccessRequestSummary>, Error> {
        let path = resource_path(&["models", model_id, "access-requests"])?;
        let mut url = reqwest::Url::parse(&format!("https://enumeray.invalid{path}"))
            .expect("encoded resource path");
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query.iter().copied());
        }
        let path = format!(
            "{}{}",
            url.path(),
            url.query().map(|q| format!("?{q}")).unwrap_or_default()
        );
        self.client
            .request::<Collection<AccessRequestSummary>, serde_json::Value>(
                Method::GET,
                &path,
                None,
                None,
            )
    }
    pub fn request_access(
        &self,
        model_id: &str,
        request: &AccessRequestCreate,
    ) -> Result<AccessRequestReceipt, Error> {
        let path = resource_path(&["models", model_id, "access-requests"])?;
        self.client
            .request::<AccessRequestReceipt, AccessRequestCreate>(
                Method::POST,
                &path,
                Some(request),
                None,
            )
    }
    pub fn decide_access(
        &self,
        model_id: &str,
        request_id: &str,
        request: &AccessRequestDecision,
    ) -> Result<AccessDecisionReceipt, Error> {
        let path = resource_path(&["models", model_id, "access-requests", request_id])?;
        self.client
            .request::<AccessDecisionReceipt, AccessRequestDecision>(
                Method::PUT,
                &path,
                Some(request),
                None,
            )
    }
    pub fn publication_delegations(
        &self,
        model_id: &str,
        publication_id: &str,
        query: &[(&str, &str)],
    ) -> Result<Collection<PublicationDelegation>, Error> {
        let path = resource_path(&[
            "models",
            model_id,
            "publications",
            publication_id,
            "delegations",
        ])?;
        let mut url = reqwest::Url::parse(&format!("https://enumeray.invalid{path}"))
            .expect("encoded resource path");
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query.iter().copied());
        }
        let path = format!(
            "{}{}",
            url.path(),
            url.query().map(|q| format!("?{q}")).unwrap_or_default()
        );
        self.client
            .request::<Collection<PublicationDelegation>, serde_json::Value>(
                Method::GET,
                &path,
                None,
                None,
            )
    }
    pub fn publication_delegation(
        &self,
        model_id: &str,
        publication_id: &str,
        delegation_id: &str,
    ) -> Result<PublicationDelegation, Error> {
        let path = resource_path(&[
            "models",
            model_id,
            "publications",
            publication_id,
            "delegations",
            delegation_id,
        ])?;
        self.client
            .request::<PublicationDelegation, serde_json::Value>(Method::GET, &path, None, None)
    }
    pub fn delegate_publication(
        &self,
        model_id: &str,
        publication_id: &str,
        delegation_id: &str,
        request: &PublicationDelegationCreate,
    ) -> Result<PublicationDelegation, Error> {
        let path = resource_path(&[
            "models",
            model_id,
            "publications",
            publication_id,
            "delegations",
            delegation_id,
        ])?;
        self.client
            .request::<PublicationDelegation, PublicationDelegationCreate>(
                Method::PUT,
                &path,
                Some(request),
                None,
            )
    }
    pub fn revoke_publication_delegation(
        &self,
        model_id: &str,
        publication_id: &str,
        delegation_id: &str,
        request: &GenerationChange,
    ) -> Result<PublicationDelegation, Error> {
        let path = resource_path(&[
            "models",
            model_id,
            "publications",
            publication_id,
            "delegations",
            delegation_id,
        ])?;
        self.client
            .request::<PublicationDelegation, GenerationChange>(
                Method::DELETE,
                &path,
                Some(request),
                None,
            )
    }

    pub fn available_compute_grants(
        &self,
        query: &[(&str, &str)],
    ) -> Result<Collection<AvailableGrant>, Error> {
        let path = resource_path(&["compute-grants"])?;
        let mut url = reqwest::Url::parse(&format!("https://enumeray.invalid{path}"))
            .expect("encoded resource path");
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query.iter().copied());
        }
        let path = format!(
            "{}{}",
            url.path(),
            url.query().map(|q| format!("?{q}")).unwrap_or_default()
        );
        self.client
            .request::<Collection<AvailableGrant>, serde_json::Value>(
                Method::GET,
                &path,
                None,
                None,
            )
    }
    pub fn accept_ownership_transfer(
        &self,
        transfer_id: &str,
        request: &TransferAcceptance,
    ) -> Result<TransferAcceptanceReceipt, Error> {
        let path = resource_path(&["ownership-transfers", transfer_id, "accept"])?;
        self.client
            .request::<TransferAcceptanceReceipt, TransferAcceptance>(
                Method::POST,
                &path,
                Some(request),
                None,
            )
    }

    pub fn contexts(
        &self,
        connector_id: &str,
        query: &[(&str, &str)],
    ) -> Result<Collection<ContextSummary>, Error> {
        let path = resource_path(&["connectors", connector_id, "contexts"])?;
        let mut url = reqwest::Url::parse(&format!("https://enumeray.invalid{path}"))
            .expect("encoded resource path");
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query.iter().copied());
        }
        let path = format!(
            "{}{}",
            url.path(),
            url.query().map(|q| format!("?{q}")).unwrap_or_default()
        );
        self.client
            .request::<Collection<ContextSummary>, serde_json::Value>(
                Method::GET,
                &path,
                None,
                None,
            )
    }
    pub fn create_context(
        &self,
        connector_id: &str,
        request: &ContextCreate,
    ) -> Result<ContextRegistration, Error> {
        let path = resource_path(&["connectors", connector_id, "contexts"])?;
        self.client.request::<ContextRegistration, ContextCreate>(
            Method::POST,
            &path,
            Some(request),
            None,
        )
    }
    pub fn update_context_metadata(
        &self,
        connector_id: &str,
        context_id: &str,
        request: &ContextMetadataUpdate,
    ) -> Result<ContextMetadataReceipt, Error> {
        let path = resource_path(&[
            "connectors",
            connector_id,
            "contexts",
            context_id,
            "metadata",
        ])?;
        self.client
            .request::<ContextMetadataReceipt, ContextMetadataUpdate>(
                Method::PUT,
                &path,
                Some(request),
                None,
            )
    }
    pub fn context(
        &self,
        connector_id: &str,
        context_id: &str,
    ) -> Result<ContextLifecycleStatus, Error> {
        let path = resource_path(&["connectors", connector_id, "contexts", context_id])?;
        self.client
            .request::<ContextLifecycleStatus, serde_json::Value>(Method::GET, &path, None, None)
    }
    pub fn context_plan(
        &self,
        connector_id: &str,
        context_id: &str,
        query: &[(&str, &str)],
    ) -> Result<ContextReplacementPlan, Error> {
        let path = resource_path(&[
            "connectors",
            connector_id,
            "contexts",
            context_id,
            "replacement-plan",
        ])?;
        let mut url = reqwest::Url::parse(&format!("https://enumeray.invalid{path}"))
            .expect("encoded resource path");
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query.iter().copied());
        }
        let path = format!(
            "{}{}",
            url.path(),
            url.query().map(|q| format!("?{q}")).unwrap_or_default()
        );
        self.client
            .request::<ContextReplacementPlan, serde_json::Value>(Method::GET, &path, None, None)
    }
    pub fn context_cutover(
        &self,
        connector_id: &str,
        context_id: &str,
        request: &ContextCutover,
    ) -> Result<ContextLifecycleReceipt, Error> {
        let path = resource_path(&[
            "connectors",
            connector_id,
            "contexts",
            context_id,
            "cutover",
        ])?;
        self.client
            .request::<ContextLifecycleReceipt, ContextCutover>(
                Method::PUT,
                &path,
                Some(request),
                None,
            )
    }
    pub fn context_reopen(
        &self,
        connector_id: &str,
        context_id: &str,
        request: &GenerationChange,
    ) -> Result<ContextLifecycleReceipt, Error> {
        let path = resource_path(&["connectors", connector_id, "contexts", context_id, "reopen"])?;
        self.client
            .request::<ContextLifecycleReceipt, GenerationChange>(
                Method::PUT,
                &path,
                Some(request),
                None,
            )
    }
    pub fn context_release_configurations(
        &self,
        connector_id: &str,
        context_id: &str,
        request: &GenerationChange,
    ) -> Result<ContextLifecycleReceipt, Error> {
        let path = resource_path(&[
            "connectors",
            connector_id,
            "contexts",
            context_id,
            "configuration-retirements",
        ])?;
        self.client
            .request::<ContextLifecycleReceipt, GenerationChange>(
                Method::PUT,
                &path,
                Some(request),
                None,
            )
    }
    pub fn context_retirement(
        &self,
        connector_id: &str,
        context_id: &str,
        request: &GenerationChange,
    ) -> Result<ContextLifecycleReceipt, Error> {
        let path = resource_path(&[
            "connectors",
            connector_id,
            "contexts",
            context_id,
            "retirement",
        ])?;
        self.client
            .request::<ContextLifecycleReceipt, GenerationChange>(
                Method::PUT,
                &path,
                Some(request),
                None,
            )
    }
    pub fn compute_grants(
        &self,
        connector_id: &str,
        query: &[(&str, &str)],
    ) -> Result<Collection<ComputeGrant>, Error> {
        let path = resource_path(&["connectors", connector_id, "grants"])?;
        let mut url = reqwest::Url::parse(&format!("https://enumeray.invalid{path}"))
            .expect("encoded resource path");
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query.iter().copied());
        }
        let path = format!(
            "{}{}",
            url.path(),
            url.query().map(|q| format!("?{q}")).unwrap_or_default()
        );
        self.client
            .request::<Collection<ComputeGrant>, serde_json::Value>(Method::GET, &path, None, None)
    }
    pub fn compute_grant(&self, connector_id: &str, grant_id: &str) -> Result<ComputeGrant, Error> {
        let path = resource_path(&["connectors", connector_id, "grants", grant_id])?;
        self.client
            .request::<ComputeGrant, serde_json::Value>(Method::GET, &path, None, None)
    }
    pub fn grant_compute(
        &self,
        connector_id: &str,
        request: &GrantCreate,
    ) -> Result<ComputeGrant, Error> {
        let path = resource_path(&["connectors", connector_id, "grants"])?;
        self.client
            .request::<ComputeGrant, GrantCreate>(Method::POST, &path, Some(request), None)
    }
    pub fn change_compute_grant(
        &self,
        connector_id: &str,
        grant_id: &str,
        request: &GrantUpdate,
    ) -> Result<ComputeGrant, Error> {
        let path = resource_path(&["connectors", connector_id, "grants", grant_id])?;
        self.client
            .request::<ComputeGrant, GrantUpdate>(Method::PUT, &path, Some(request), None)
    }
    pub fn revoke_compute_grant(
        &self,
        connector_id: &str,
        grant_id: &str,
    ) -> Result<RevokedGrant, Error> {
        let path = resource_path(&["connectors", connector_id, "grants", grant_id])?;
        self.client
            .request::<RevokedGrant, serde_json::Value>(Method::DELETE, &path, None, None)
    }
    pub fn connector_runs(
        &self,
        connector_id: &str,
        query: &[(&str, &str)],
    ) -> Result<Collection<OperationalRun>, Error> {
        let path = resource_path(&["connectors", connector_id, "runs"])?;
        let mut url = reqwest::Url::parse(&format!("https://enumeray.invalid{path}"))
            .expect("encoded resource path");
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query.iter().copied());
        }
        let path = format!(
            "{}{}",
            url.path(),
            url.query().map(|q| format!("?{q}")).unwrap_or_default()
        );
        self.client
            .request::<Collection<OperationalRun>, serde_json::Value>(
                Method::GET,
                &path,
                None,
                None,
            )
    }
    pub fn connectors(
        &self,
        query: &[(&str, &str)],
    ) -> Result<Collection<ConnectorSummary>, Error> {
        let path = resource_path(&["connectors"])?;
        let mut url = reqwest::Url::parse(&format!("https://enumeray.invalid{path}"))
            .expect("encoded resource path");
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query.iter().copied());
        }
        let path = format!(
            "{}{}",
            url.path(),
            url.query().map(|q| format!("?{q}")).unwrap_or_default()
        );
        self.client
            .request::<Collection<ConnectorSummary>, serde_json::Value>(
                Method::GET,
                &path,
                None,
                None,
            )
    }
    pub fn register_connector(
        &self,
        request: &ConnectorCreate,
    ) -> Result<ConnectorRegistration, Error> {
        let path = resource_path(&["connectors"])?;
        self.client
            .request::<ConnectorRegistration, ConnectorCreate>(
                Method::POST,
                &path,
                Some(request),
                None,
            )
    }
    pub fn connector(&self, connector_id: &str) -> Result<ConnectorSummary, Error> {
        let path = resource_path(&["connectors", connector_id])?;
        self.client
            .request::<ConnectorSummary, serde_json::Value>(Method::GET, &path, None, None)
    }
    pub fn update_connector(
        &self,
        connector_id: &str,
        request: &ConnectorUpdate,
    ) -> Result<ConnectorUpdateReceipt, Error> {
        let path = resource_path(&["connectors", connector_id])?;
        self.client
            .request::<ConnectorUpdateReceipt, ConnectorUpdate>(
                Method::PUT,
                &path,
                Some(request),
                None,
            )
    }
    pub fn connector_diagnostics(&self, connector_id: &str) -> Result<ConnectorDiagnostics, Error> {
        let path = resource_path(&["connectors", connector_id, "diagnostics"])?;
        self.client
            .request::<ConnectorDiagnostics, serde_json::Value>(Method::GET, &path, None, None)
    }
    pub fn new_connector_enrollment(
        &self,
        connector_id: &str,
    ) -> Result<ConnectorEnrollment, Error> {
        let path = resource_path(&["connectors", connector_id, "enrollments"])?;
        self.client
            .request::<ConnectorEnrollment, serde_json::Value>(Method::POST, &path, None, None)
    }
    pub fn connector_state(
        &self,
        connector_id: &str,
        request: &ConnectorStateChange,
    ) -> Result<ConnectorLifecycleReceipt, Error> {
        let path = resource_path(&["connectors", connector_id, "state"])?;
        self.client
            .request::<ConnectorLifecycleReceipt, ConnectorStateChange>(
                Method::PUT,
                &path,
                Some(request),
                None,
            )
    }
    pub fn connector_authority(
        &self,
        connector_id: &str,
        request: &ConnectorAuthorityChange,
    ) -> Result<ConnectorLifecycleReceipt, Error> {
        let path = resource_path(&["connectors", connector_id, "authority"])?;
        self.client
            .request::<ConnectorLifecycleReceipt, ConnectorAuthorityChange>(
                Method::PUT,
                &path,
                Some(request),
                None,
            )
    }
    pub fn revoke_connector(
        &self,
        connector_id: &str,
    ) -> Result<ConnectorRevocationReceipt, Error> {
        let path = resource_path(&["connectors", connector_id, "credentials"])?;
        self.client
            .request::<ConnectorRevocationReceipt, serde_json::Value>(
                Method::DELETE,
                &path,
                None,
                None,
            )
    }
    pub fn model_members(&self, model_id: &str) -> Result<ResourceMembers, Error> {
        let path = resource_path(&["models", model_id, "members"])?;
        self.client
            .request::<ResourceMembers, serde_json::Value>(Method::GET, &path, None, None)
    }
    pub fn model_add_member(
        &self,
        model_id: &str,
        request: &MembershipChange,
    ) -> Result<MembershipReceipt, Error> {
        let path = resource_path(&["models", model_id, "members"])?;
        self.client.request::<MembershipReceipt, MembershipChange>(
            Method::POST,
            &path,
            Some(request),
            None,
        )
    }
    pub fn model_remove_member(
        &self,
        model_id: &str,
        user_id: &str,
        request: &GenerationChange,
    ) -> Result<MembershipReceipt, Error> {
        let path = resource_path(&["models", model_id, "members", user_id])?;
        self.client.request::<MembershipReceipt, GenerationChange>(
            Method::DELETE,
            &path,
            Some(request),
            None,
        )
    }
    pub fn model_archive(
        &self,
        model_id: &str,
        request: &GenerationChange,
    ) -> Result<ArchiveReceipt, Error> {
        let path = resource_path(&["models", model_id, "archive"])?;
        self.client.request::<ArchiveReceipt, GenerationChange>(
            Method::POST,
            &path,
            Some(request),
            None,
        )
    }
    pub fn model_propose_transfer(
        &self,
        model_id: &str,
        request: &TransferCreate,
    ) -> Result<TransferProposalReceipt, Error> {
        let path = resource_path(&["models", model_id, "ownership-transfers"])?;
        self.client
            .request::<TransferProposalReceipt, TransferCreate>(
                Method::POST,
                &path,
                Some(request),
                None,
            )
    }
    pub fn connector_members(&self, connector_id: &str) -> Result<ResourceMembers, Error> {
        let path = resource_path(&["connectors", connector_id, "members"])?;
        self.client
            .request::<ResourceMembers, serde_json::Value>(Method::GET, &path, None, None)
    }
    pub fn connector_add_member(
        &self,
        connector_id: &str,
        request: &MembershipChange,
    ) -> Result<MembershipReceipt, Error> {
        let path = resource_path(&["connectors", connector_id, "members"])?;
        self.client.request::<MembershipReceipt, MembershipChange>(
            Method::POST,
            &path,
            Some(request),
            None,
        )
    }
    pub fn connector_remove_member(
        &self,
        connector_id: &str,
        user_id: &str,
        request: &GenerationChange,
    ) -> Result<MembershipReceipt, Error> {
        let path = resource_path(&["connectors", connector_id, "members", user_id])?;
        self.client.request::<MembershipReceipt, GenerationChange>(
            Method::DELETE,
            &path,
            Some(request),
            None,
        )
    }
    pub fn connector_archive(
        &self,
        connector_id: &str,
        request: &GenerationChange,
    ) -> Result<ArchiveReceipt, Error> {
        let path = resource_path(&["connectors", connector_id, "archive"])?;
        self.client.request::<ArchiveReceipt, GenerationChange>(
            Method::POST,
            &path,
            Some(request),
            None,
        )
    }
    pub fn connector_propose_transfer(
        &self,
        connector_id: &str,
        request: &TransferCreate,
    ) -> Result<TransferProposalReceipt, Error> {
        let path = resource_path(&["connectors", connector_id, "ownership-transfers"])?;
        self.client
            .request::<TransferProposalReceipt, TransferCreate>(
                Method::POST,
                &path,
                Some(request),
                None,
            )
    }
    pub fn resource_audit(
        &self,
        kind: &str,
        resource: &str,
        query: &[(&str, &str)],
    ) -> Result<Collection<AuditEvent>, Error> {
        let path = resource_path(&["audit", kind, resource])?;
        let mut url = reqwest::Url::parse(&format!("https://enumeray.invalid{path}"))
            .expect("encoded resource path");
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query.iter().copied());
        }
        let path = format!(
            "{}{}",
            url.path(),
            url.query().map(|q| format!("?{q}")).unwrap_or_default()
        );
        self.client
            .request::<Collection<AuditEvent>, serde_json::Value>(Method::GET, &path, None, None)
    }
    pub fn service_status(&self) -> Result<ServiceStatus, Error> {
        let path = resource_path(&["service", "status"])?;
        self.client
            .request::<ServiceStatus, serde_json::Value>(Method::GET, &path, None, None)
    }
    pub fn service_limits(&self) -> Result<ServiceLimits, Error> {
        let path = resource_path(&["service", "limits"])?;
        self.client
            .request::<ServiceLimits, serde_json::Value>(Method::GET, &path, None, None)
    }
    pub fn set_service_limits(
        &self,
        request: &ServiceLimitsUpdate,
    ) -> Result<ServiceLimits, Error> {
        let path = resource_path(&["service", "limits"])?;
        self.client.request::<ServiceLimits, ServiceLimitsUpdate>(
            Method::PUT,
            &path,
            Some(request),
            None,
        )
    }
    pub fn service_audit(
        &self,
        query: &[(&str, &str)],
    ) -> Result<Collection<ServiceAuditEvent>, Error> {
        let path = resource_path(&["service", "audit"])?;
        let mut url = reqwest::Url::parse(&format!("https://enumeray.invalid{path}"))
            .expect("encoded resource path");
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query.iter().copied());
        }
        let path = format!(
            "{}{}",
            url.path(),
            url.query().map(|q| format!("?{q}")).unwrap_or_default()
        );
        self.client
            .request::<Collection<ServiceAuditEvent>, serde_json::Value>(
                Method::GET,
                &path,
                None,
                None,
            )
    }
    pub fn set_user_suspension(
        &self,
        user_id: &str,
        request: &UserSuspensionUpdate,
    ) -> Result<UserSuspensionReceipt, Error> {
        let path = resource_path(&["service", "users", user_id, "suspension"])?;
        self.client
            .request::<UserSuspensionReceipt, UserSuspensionUpdate>(
                Method::PUT,
                &path,
                Some(request),
                None,
            )
    }
}
