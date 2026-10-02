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
use temps_core::{
    problemdetails::{Problem, ProblemDetails},
    AuditContext, AuditOperation, RequestMetadata,
};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Clone, Deserialize, ToSchema)]
pub struct CreateDeliveryProfileRequest {
    pub name: String,
    pub provider_kind: DeliveryProviderKind,
    pub bunny_pull_zone_id: Option<i64>,
    pub bunny_api_key: Option<String>,
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
/// Structured, secret-free context recorded with each delivery audit entry.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum DeliveryAuditDetails {
    Profile {
        profile_id: i32,
        name: String,
        provider_kind: DeliveryProviderKind,
        /// Bunny Pull Zone ID; the API key is never recorded.
        bunny_pull_zone_id: Option<i64>,
    },
    ProjectSettings {
        default_profile_id: Option<i32>,
        default_profile_name: Option<String>,
        default_provider_kind: Option<DeliveryProviderKind>,
        environment_overrides: Vec<EnvironmentDeliveryOverride>,
    },
    Preview {
        #[serde(serialize_with = "serialize_uuid")]
        preview_id: Uuid,
        hostname: String,
        environment_id: i32,
        dns_provider_id: i32,
        zone: String,
        profile_id: i32,
        profile_source: String,
        provider_kind: DeliveryProviderKind,
        record_name: String,
        record_type: String,
        requires_adoption: bool,
    },
    Binding {
        binding_id: i32,
        hostname: Option<String>,
        environment_id: Option<i32>,
        profile_id: Option<i32>,
        provider_kind: Option<DeliveryProviderKind>,
        dns_provider_id: Option<i32>,
        zone: Option<String>,
    },
}

fn serialize_uuid<S: serde::Serializer>(value: &Uuid, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&value.to_string())
}

impl DeliveryAuditDetails {
    fn profile(profile: &DeliveryProfileResponse) -> Self {
        Self::Profile {
            profile_id: profile.id,
            name: profile.name.clone(),
            provider_kind: profile.provider_kind,
            bunny_pull_zone_id: profile.bunny_pull_zone_id,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct DeliveryAudit {
    context: AuditContext,
    action: String,
    project_id: Option<i32>,
    resource_id: String,
    details: DeliveryAuditDetails,
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
    details: DeliveryAuditDetails,
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
        details,
    };
    if let Err(error) = state.audit_service.create_audit_log(&op).await {
        tracing::error!(%error, action, ?project_id, "Failed to record delivery audit")
    }
}

#[utoipa::path(
    get,
    path = "/delivery-capabilities",
    tag = "Traffic Delivery",
    responses(
        (status = 200, description = "Supported delivery providers and their setup state", body = Vec<DeliveryCapabilityResponse>),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_delivery_capabilities(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::DnsProvidersRead);
    Ok(Json(state.domain_delivery_service.capabilities().await?))
}

/// List delivery profiles.
///
/// Callers with DNS provider read access see every field. Project readers
/// without it (who need profiles for the project delivery switch) get a
/// reduced view: `id`, `name`, `provider_kind` and timestamps, with
/// `bunny_pull_zone_id` and `bunny_hostname` set to `null`.
#[utoipa::path(
    get,
    path = "/delivery-profiles",
    tag = "Traffic Delivery",
    responses(
        (status = 200, description = "Delivery profiles; provider details are null without DNS provider read access", body = Vec<DeliveryProfileResponse>),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_delivery_profiles(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
) -> Result<impl IntoResponse, Problem> {
    let full_view = auth.has_permission(&Permission::DnsProvidersRead);
    if !full_view {
        permission_check!(auth, Permission::ProjectsRead);
    }
    let profiles = state.domain_delivery_service.list_profiles().await?;
    let profiles: Vec<DeliveryProfileResponse> = if full_view {
        profiles
    } else {
        profiles
            .into_iter()
            .map(DeliveryProfileResponse::without_provider_details)
            .collect()
    };
    Ok(Json(profiles))
}

#[utoipa::path(
    post,
    path = "/delivery-profiles",
    tag = "Traffic Delivery",
    request_body = CreateDeliveryProfileRequest,
    responses(
        (status = 201, description = "Delivery profile created", body = DeliveryProfileResponse),
        (status = 400, description = "Invalid profile or Bunny Pull Zone configuration", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 409, description = "A profile with this name already exists", body = ProblemDetails),
        (status = 429, description = "Bunny API rate limited the request", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
        (status = 502, description = "Bunny API unreachable or returned an error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
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
        .create_profile_with_bunny(
            req.name,
            req.provider_kind,
            req.bunny_pull_zone_id,
            req.bunny_api_key,
        )
        .await?;
    audit(
        &state,
        &auth,
        &metadata,
        "DELIVERY_PROFILE_CREATED",
        None,
        profile.id.to_string(),
        DeliveryAuditDetails::profile(&profile),
    )
    .await;
    Ok((StatusCode::CREATED, Json(profile)))
}

#[utoipa::path(
    delete,
    path = "/delivery-profiles/{profile_id}",
    tag = "Traffic Delivery",
    params(("profile_id" = i32, Path, description = "Delivery profile ID")),
    responses(
        (status = 204, description = "Delivery profile deleted"),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Delivery profile not found", body = ProblemDetails),
        (status = 409, description = "Profile is still referenced or required by the new-project default", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_delivery_profile(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(profile_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_check!(auth, Permission::DnsProvidersWrite);
    permission_check!(auth, Permission::DnsAutomationWrite);
    let deleted = state
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
        DeliveryAuditDetails::profile(&deleted),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// Project delivery settings. Without DNS provider read access the effective
/// profile is returned in its reduced view (see `GET /delivery-profiles`).
#[utoipa::path(
    get,
    path = "/projects/{project_id}/delivery-settings",
    tag = "Traffic Delivery",
    params(("project_id" = i32, Path, description = "Project ID")),
    responses(
        (status = 200, description = "Project delivery settings", body = ProjectDeliverySettingsResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_project_delivery_settings(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Path(project_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(auth, ProjectsRead, project_id, state.project_access_checker);
    let settings = state.domain_delivery_service.settings(project_id).await?;
    Ok(Json(
        if auth.has_permission(&Permission::DnsProvidersRead) {
            settings
        } else {
            settings.without_provider_details()
        },
    ))
}

#[utoipa::path(
    put,
    path = "/projects/{project_id}/delivery-settings",
    tag = "Traffic Delivery",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body = UpdateProjectDeliverySettingsRequest,
    responses(
        (status = 200, description = "Updated project delivery settings", body = ProjectDeliverySettingsResponse),
        (status = 400, description = "Environment belongs to another project", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project, environment, or delivery profile not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
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
    let requested_overrides = req.environment_overrides.clone();
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
        DeliveryAuditDetails::ProjectSettings {
            default_profile_id: response.default_profile_id,
            default_profile_name: response
                .effective_default_profile
                .as_ref()
                .map(|profile| profile.name.clone()),
            default_provider_kind: response
                .effective_default_profile
                .as_ref()
                .map(|profile| profile.provider_kind),
            environment_overrides: requested_overrides,
        },
    )
    .await;
    Ok(Json(
        if auth.has_permission(&Permission::DnsProvidersRead) {
            response
        } else {
            response.without_provider_details()
        },
    ))
}

#[utoipa::path(
    get,
    path = "/projects/{project_id}/domain-delivery-bindings",
    tag = "Traffic Delivery",
    params(("project_id" = i32, Path, description = "Project ID")),
    responses(
        (status = 200, description = "Domain delivery bindings for the project", body = Vec<DomainDeliveryBindingResponse>),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
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

#[utoipa::path(
    post,
    path = "/projects/{project_id}/domain-delivery-bindings/preview",
    tag = "Traffic Delivery",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body = PreviewDomainDeliveryBindingRequest,
    responses(
        (status = 200, description = "Delivery plan; apply it with the returned preview_id", body = DomainDeliveryPreviewResponse),
        (status = 400, description = "Invalid hostname, zone, origin, or profile configuration", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project, environment, profile, DNS provider, or managed zone not found", body = ProblemDetails),
        (status = 409, description = "Hostname or DNS record is owned by something else", body = ProblemDetails),
        (status = 429, description = "Upstream provider rate limited the request", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
        (status = 502, description = "DNS or CDN provider unreachable or returned an error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn preview_domain_delivery_binding(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
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
    let environment_id = req.environment_id;
    let dns_provider_id = req.dns_provider_id;
    let hostname = normalize_dns_name(&req.hostname);
    let zone = normalize_dns_name(&req.zone);
    let preview = state
        .domain_delivery_service
        .preview(project_id, auth.user_id(), req)
        .await?;
    // Preview persists a plan row and calls the CDN API, so it is audited
    // like any other write.
    audit(
        &state,
        &auth,
        &metadata,
        "DOMAIN_DELIVERY_PREVIEW_CREATED",
        Some(project_id),
        preview.preview_id.to_string(),
        DeliveryAuditDetails::Preview {
            preview_id: preview.preview_id,
            hostname,
            environment_id,
            dns_provider_id,
            zone,
            profile_id: preview.profile_id,
            profile_source: preview.profile_source.clone(),
            provider_kind: preview.provider_kind,
            record_name: preview.record.name.clone(),
            record_type: preview.record.record_type.to_string(),
            requires_adoption: preview.record.requires_adoption,
        },
    )
    .await;
    Ok(Json(preview))
}

/// Audit-only normalization matching what the service stores.
fn normalize_dns_name(value: &str) -> String {
    value.trim().trim_end_matches('.').to_ascii_lowercase()
}

#[utoipa::path(
    post,
    path = "/projects/{project_id}/domain-delivery-bindings/apply",
    tag = "Traffic Delivery",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body = ApplyDomainDeliveryBindingRequest,
    responses(
        (status = 200, description = "Delivery binding applied", body = DomainDeliveryBindingResponse),
        (status = 400, description = "Preview expired, stale, or adoption request invalid", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions or preview created by another user", body = ProblemDetails),
        (status = 404, description = "Preview, project, environment, profile, DNS provider, or managed zone not found", body = ProblemDetails),
        (status = 409, description = "Routing or DNS records changed since preview, or another operation holds the hostname", body = ProblemDetails),
        (status = 429, description = "Upstream provider rate limited the request", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
        (status = 502, description = "DNS or CDN provider unreachable or returned an error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
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
        DeliveryAuditDetails::Binding {
            binding_id: response.id,
            hostname: Some(response.hostname.clone()),
            environment_id: Some(response.environment_id),
            profile_id: Some(response.delivery_profile_id),
            provider_kind: Some(response.provider_kind),
            dns_provider_id: Some(response.dns_provider_id),
            zone: Some(response.zone.clone()),
        },
    )
    .await;
    Ok(Json(response))
}

#[utoipa::path(
    delete,
    path = "/projects/{project_id}/domain-delivery-bindings/{binding_id}",
    tag = "Traffic Delivery",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("binding_id" = i32, Path, description = "Domain delivery binding ID")
    ),
    responses(
        (status = 204, description = "DNS record, CDN hostname and binding removed"),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Binding not found in this project", body = ProblemDetails),
        (status = 409, description = "Record is owned by another scope, or another operation holds the hostname", body = ProblemDetails),
        (status = 429, description = "Upstream provider rate limited the request", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
        (status = 502, description = "DNS or CDN provider unreachable or returned an error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
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
        DeliveryAuditDetails::Binding {
            binding_id,
            hostname: None,
            environment_id: None,
            profile_id: None,
            provider_kind: None,
            dns_provider_id: None,
            zone: None,
        },
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}
