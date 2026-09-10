// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::DnsAppState;
use crate::services::domain_delivery::*;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_auth::{permission_check, project_permission_guard, Permission, RequireAuth};
use temps_core::{problemdetails::Problem, AuditContext, AuditOperation, RequestMetadata};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateDeliveryProfileRequest {
    pub name: String,
    pub provider_kind: DeliveryProviderKind,
}
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateProjectDeliverySettingsRequest {
    pub default_profile_id: Option<i32>,
    #[serde(default)]
    pub environment_overrides: Vec<EnvironmentDeliveryOverride>,
}
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ApplyDomainDeliveryBindingRequest {
    #[schema(value_type=String)]
    pub preview_id: Uuid,
    #[serde(default)]
    pub adopt_records: Vec<AdoptDeliveryRecord>,
}
#[derive(Debug, Clone, Serialize)]
struct DeliveryAudit {
    context: AuditContext,
    action: String,
    project_id: Option<i32>,
    resource_id: String,
}
impl AuditOperation for DeliveryAudit {
    fn operation_type(&self) -> String {
        self.action.clone()
    }
    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }
    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }
    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }
    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize delivery audit: {e}"))
    }
}
async fn audit(
    state: &DnsAppState,
    auth: &temps_auth::AuthContext,
    metadata: &RequestMetadata,
    action: &str,
    project_id: Option<i32>,
    resource_id: String,
) {
    let op = DeliveryAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        action: action.into(),
        project_id,
        resource_id,
    };
    if let Err(error) = state.audit_service.create_audit_log(&op).await {
        tracing::error!(%error,"Failed to record delivery audit")
    }
}

#[utoipa::path(get,path="/delivery-capabilities",tag="Traffic Delivery",responses((status=200,body=Vec<DeliveryCapabilityResponse>)),security(("bearer_auth"=[])))]
pub async fn get_delivery_capabilities(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::DnsProvidersRead);
    Ok(Json(state.domain_delivery_service.capabilities().await?))
}
#[utoipa::path(get,path="/delivery-profiles",tag="Traffic Delivery",responses((status=200,body=Vec<DeliveryProfileResponse>)),security(("bearer_auth"=[])))]
pub async fn list_delivery_profiles(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::DnsProvidersRead);
    Ok(Json(state.domain_delivery_service.list_profiles().await?))
}
#[utoipa::path(post,path="/delivery-profiles",tag="Traffic Delivery",request_body=CreateDeliveryProfileRequest,responses((status=201,body=DeliveryProfileResponse)),security(("bearer_auth"=[])))]
pub async fn create_delivery_profile(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(req): Json<CreateDeliveryProfileRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::DnsProvidersWrite);
    permission_check!(auth, Permission::DnsAutomationWrite);
    let profile = state
        .domain_delivery_service
        .create_profile(req.name, req.provider_kind)
        .await?;
    audit(
        &state,
        &auth,
        &metadata,
        "DELIVERY_PROFILE_CREATED",
        None,
        profile.id.to_string(),
    )
    .await;
    Ok((StatusCode::CREATED, Json(profile)))
}
#[utoipa::path(delete,path="/delivery-profiles/{profile_id}",tag="Traffic Delivery",params(("profile_id"=i32,Path)),responses((status=204)),security(("bearer_auth"=[])))]
pub async fn delete_delivery_profile(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(profile_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::DnsProvidersWrite);
    permission_check!(auth, Permission::DnsAutomationWrite);
    state
        .domain_delivery_service
        .delete_profile(profile_id)
        .await?;
    audit(
        &state,
        &auth,
        &metadata,
        "DELIVERY_PROFILE_DELETED",
        None,
        profile_id.to_string(),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(get,path="/projects/{project_id}/delivery-settings",tag="Traffic Delivery",params(("project_id"=i32,Path)),responses((status=200,body=ProjectDeliverySettingsResponse)),security(("bearer_auth"=[])))]
pub async fn get_project_delivery_settings(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Path(project_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(auth, ProjectsRead, project_id, state.project_access_checker);
    Ok(Json(
        state.domain_delivery_service.settings(project_id).await?,
    ))
}
#[utoipa::path(put,path="/projects/{project_id}/delivery-settings",tag="Traffic Delivery",params(("project_id"=i32,Path)),request_body=UpdateProjectDeliverySettingsRequest,responses((status=200,body=ProjectDeliverySettingsResponse)),security(("bearer_auth"=[])))]
pub async fn update_project_delivery_settings(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(req): Json<UpdateProjectDeliverySettingsRequest>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        state.project_access_checker
    );
    let response = state
        .domain_delivery_service
        .update_settings(
            project_id,
            req.default_profile_id,
            req.environment_overrides,
        )
        .await?;
    audit(
        &state,
        &auth,
        &metadata,
        "PROJECT_DELIVERY_SETTINGS_UPDATED",
        Some(project_id),
        project_id.to_string(),
    )
    .await;
    Ok(Json(response))
}
#[utoipa::path(get,path="/projects/{project_id}/domain-delivery-bindings",tag="Traffic Delivery",params(("project_id"=i32,Path)),responses((status=200,body=Vec<DomainDeliveryBindingResponse>)),security(("bearer_auth"=[])))]
pub async fn list_domain_delivery_bindings(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Path(project_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(auth, ProjectsRead, project_id, state.project_access_checker);
    Ok(Json(
        state
            .domain_delivery_service
            .list_bindings(project_id)
            .await?,
    ))
}
#[utoipa::path(post,path="/projects/{project_id}/domain-delivery-bindings/preview",tag="Traffic Delivery",params(("project_id"=i32,Path)),request_body=PreviewDomainDeliveryBindingRequest,responses((status=200,body=DomainDeliveryPreviewResponse)),security(("bearer_auth"=[])))]
pub async fn preview_domain_delivery_binding(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Path(project_id): Path<i32>,
    Json(req): Json<PreviewDomainDeliveryBindingRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::DnsProvidersWrite);
    permission_check!(auth, Permission::DnsAutomationWrite);
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        state.project_access_checker
    );
    Ok(Json(
        state
            .domain_delivery_service
            .preview(project_id, auth.user_id(), req)
            .await?,
    ))
}
#[utoipa::path(post,path="/projects/{project_id}/domain-delivery-bindings/apply",tag="Traffic Delivery",params(("project_id"=i32,Path)),request_body=ApplyDomainDeliveryBindingRequest,responses((status=200,body=DomainDeliveryBindingResponse)),security(("bearer_auth"=[])))]
pub async fn apply_domain_delivery_binding(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(req): Json<ApplyDomainDeliveryBindingRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::DnsProvidersWrite);
    permission_check!(auth, Permission::DnsAutomationWrite);
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        state.project_access_checker
    );
    let response = state
        .domain_delivery_service
        .apply(
            project_id,
            auth.user_id(),
            req.preview_id,
            req.adopt_records,
        )
        .await?;
    audit(
        &state,
        &auth,
        &metadata,
        "DOMAIN_DELIVERY_BINDING_APPLIED",
        Some(project_id),
        response.id.to_string(),
    )
    .await;
    Ok(Json(response))
}

#[utoipa::path(delete,path="/projects/{project_id}/domain-delivery-bindings/{binding_id}",tag="Traffic Delivery",params(("project_id"=i32,Path),("binding_id"=i32,Path)),responses((status=204)),security(("bearer_auth"=[])))]
pub async fn delete_domain_delivery_binding(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, binding_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::DnsProvidersWrite);
    permission_check!(auth, Permission::DnsAutomationWrite);
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        state.project_access_checker
    );
    state
        .domain_delivery_service
        .delete_binding(project_id, binding_id)
        .await?;
    audit(
        &state,
        &auth,
        &metadata,
        "DOMAIN_DELIVERY_BINDING_DELETED",
        Some(project_id),
        binding_id.to_string(),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}
