// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::DnsAppState;
use crate::errors::{DeliveryStep, DnsError};
use crate::services::domain_delivery::*;
use axum::{
    extract::{Path, Query, State},
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
use utoipa::{IntoParams, ToSchema};
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
    /// A binding apply or delete. Recorded whether it succeeded or failed;
    /// a failure also records how far it got.
    Binding {
        outcome: DeliveryAuditOutcome,
        /// The preview being applied; absent for deletes.
        #[serde(
            skip_serializing_if = "Option::is_none",
            serialize_with = "serialize_optional_uuid"
        )]
        preview_id: Option<Uuid>,
        /// Absent when an apply failed before it reserved a binding.
        binding_id: Option<i32>,
        hostname: Option<String>,
        environment_id: Option<i32>,
        profile_id: Option<i32>,
        provider_kind: Option<DeliveryProviderKind>,
        dns_provider_id: Option<i32>,
        zone: Option<String>,
        /// Bunny bindings that succeeded only: what the apply or delete did
        /// with the hostname on the Pull Zone.
        #[serde(skip_serializing_if = "Option::is_none")]
        bunny_hostname: Option<BunnyHostnameAudit>,
        /// Present only when the operation failed.
        #[serde(skip_serializing_if = "Option::is_none")]
        failure: Option<DeliveryAuditFailure>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum DeliveryAuditOutcome {
    Succeeded,
    Failed,
}

/// What a successful Bunny binding apply or delete did with the hostname on
/// the Pull Zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum BunnyHostnameAudit {
    /// Apply: Temps added the hostname to the Pull Zone (in this or an
    /// earlier attempt), so removing the binding detaches it.
    Owned,
    /// Apply: the hostname was already on the Pull Zone; Temps reused it, and
    /// removing the binding leaves it attached.
    Preexisting,
    /// Delete: cleanup detached the hostname Temps had added.
    Detached,
    /// Delete: cleanup left the hostname, which was on the Pull Zone before
    /// Temps set up delivery, attached together with its edge certificate.
    KeptPreexisting,
}

impl BunnyHostnameAudit {
    /// The record for a successful apply (`applied`) or delete of `binding`;
    /// `None` unless it is a Bunny binding.
    fn for_binding(applied: bool, binding: &DomainDeliveryBindingResponse) -> Option<Self> {
        if binding.provider_kind != DeliveryProviderKind::Bunny {
            return None;
        }
        Some(match (applied, binding.bunny_hostname_owned) {
            (true, true) => Self::Owned,
            (true, false) => Self::Preexisting,
            (false, true) => Self::Detached,
            (false, false) => Self::KeptPreexisting,
        })
    }
}

/// How far a failed binding apply or delete got.
#[derive(Debug, Clone, Serialize)]
struct DeliveryAuditFailure {
    /// The step that failed; absent when the operation stopped before it
    /// changed anything.
    failed_step: Option<DeliveryStep>,
    /// Steps that had completed, and whose changes were kept, before the
    /// failure.
    completed_steps: Vec<DeliveryStep>,
    /// The error returned to the caller. Provider errors never carry
    /// credentials or upstream response bodies.
    error: String,
}

fn serialize_uuid<S: serde::Serializer>(value: &Uuid, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&value.to_string())
}

fn serialize_optional_uuid<S: serde::Serializer>(
    value: &Option<Uuid>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match value {
        Some(value) => serialize_uuid(value, serializer),
        None => serializer.serialize_none(),
    }
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

    /// A binding apply (`preview_id` set) or delete that succeeded.
    fn binding_succeeded(
        preview_id: Option<Uuid>,
        binding: &DomainDeliveryBindingResponse,
    ) -> Self {
        Self::Binding {
            outcome: DeliveryAuditOutcome::Succeeded,
            preview_id,
            binding_id: Some(binding.id),
            hostname: Some(binding.hostname.clone()),
            environment_id: Some(binding.environment_id),
            profile_id: Some(binding.delivery_profile_id),
            provider_kind: Some(binding.provider_kind),
            dns_provider_id: Some(binding.dns_provider_id),
            zone: Some(binding.zone.clone()),
            bunny_hostname: BunnyHostnameAudit::for_binding(preview_id.is_some(), binding),
            failure: None,
        }
    }

    /// A binding apply (`preview_id` set) or delete that failed. A
    /// [`DnsError::DeliveryIncomplete`] supplies what had already changed;
    /// any other error stopped the operation before it changed anything.
    fn binding_failed(preview_id: Option<Uuid>, binding_id: Option<i32>, error: &DnsError) -> Self {
        let failure_error = error.to_string();
        if let DnsError::DeliveryIncomplete(incomplete) = error {
            return Self::Binding {
                outcome: DeliveryAuditOutcome::Failed,
                preview_id: incomplete.preview_id.or(preview_id),
                binding_id: incomplete.binding_id.or(binding_id),
                hostname: Some(incomplete.hostname.clone()),
                environment_id: Some(incomplete.environment_id),
                profile_id: None,
                provider_kind: None,
                dns_provider_id: None,
                zone: None,
                bunny_hostname: None,
                failure: Some(DeliveryAuditFailure {
                    failed_step: Some(incomplete.failed_step),
                    completed_steps: incomplete.completed_steps.clone(),
                    error: failure_error,
                }),
            };
        }
        Self::Binding {
            outcome: DeliveryAuditOutcome::Failed,
            preview_id,
            binding_id,
            hostname: None,
            environment_id: None,
            profile_id: None,
            provider_kind: None,
            dns_provider_id: None,
            zone: None,
            bunny_hostname: None,
            failure: Some(DeliveryAuditFailure {
                failed_step: None,
                completed_steps: Vec::new(),
                error: failure_error,
            }),
        }
    }
}

/// The binding a failed delivery operation had reserved or was cleaning up,
/// when the error says.
fn failed_binding_id(error: &DnsError) -> Option<i32> {
    if let DnsError::DeliveryIncomplete(incomplete) = error {
        incomplete.binding_id
    } else {
        None
    }
}

/// Audit entry `(action, resource_id, details)` for a binding apply.
fn apply_audit_entry(
    preview_id: Uuid,
    result: &Result<DomainDeliveryBindingResponse, DnsError>,
) -> (&'static str, String, DeliveryAuditDetails) {
    match result {
        Ok(binding) => (
            "DOMAIN_DELIVERY_BINDING_APPLIED",
            binding.id.to_string(),
            DeliveryAuditDetails::binding_succeeded(Some(preview_id), binding),
        ),
        Err(error) => (
            "DOMAIN_DELIVERY_BINDING_APPLY_FAILED",
            failed_binding_id(error).map_or_else(
                || preview_id.to_string(),
                |binding_id| binding_id.to_string(),
            ),
            DeliveryAuditDetails::binding_failed(Some(preview_id), None, error),
        ),
    }
}

/// Audit entry `(action, resource_id, details)` for a binding delete.
fn delete_audit_entry(
    binding_id: i32,
    result: &Result<DomainDeliveryBindingResponse, DnsError>,
) -> (&'static str, String, DeliveryAuditDetails) {
    match result {
        Ok(binding) => (
            "DOMAIN_DELIVERY_BINDING_DELETED",
            binding_id.to_string(),
            DeliveryAuditDetails::binding_succeeded(None, binding),
        ),
        Err(error) => (
            "DOMAIN_DELIVERY_BINDING_DELETE_FAILED",
            binding_id.to_string(),
            DeliveryAuditDetails::binding_failed(None, Some(binding_id), error),
        ),
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

/// Whether the caller gets every delivery-profile field (`true`) or the
/// reduced view (`false`). Callers with neither DNS provider read nor
/// project read access are rejected with 403.
fn delivery_profile_full_view(auth: &temps_auth::AuthContext) -> Result<bool, Problem> {
    if auth.has_permission(&Permission::DnsProvidersRead) {
        return Ok(true);
    }
    permission_check!(auth, Permission::ProjectsRead);
    Ok(false)
}

/// Filter for `GET /delivery-profiles`, read alongside
/// [`temps_core::PaginationParams`] from the same query string.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DeliveryProfileSearchParams {
    /// Keep only profiles whose name contains this text, ignoring case.
    /// Surrounding whitespace is ignored and a blank value matches every
    /// profile; `%`, `_` and `\` match themselves. At most 100 characters.
    #[param(example = "edge", max_length = 100)]
    pub search: Option<String>,
}

/// List delivery profiles, one page at a time.
///
/// Newest first by default. `sort_by` accepts `created_at` (default) or
/// `name`; `sort_order` accepts `asc` or `desc` (default), case-insensitive.
/// Profile ID breaks ties in the same direction. `page_size` defaults to 20
/// and is clamped to 1..=100. `search` keeps only profiles whose name
/// contains it, ignoring case, and combines with paging and sorting: `total`
/// counts the matching profiles.
///
/// Callers with DNS provider read access see every field. Project readers
/// without it (who need profiles for the project delivery switch) get a
/// reduced view: `id`, `name`, `provider_kind` and timestamps, with
/// `bunny_pull_zone_id` and `bunny_hostname` set to `null`.
#[utoipa::path(
    get,
    path = "/delivery-profiles",
    tag = "Traffic Delivery",
    params(temps_core::PaginationParams, DeliveryProfileSearchParams),
    responses(
        (status = 200, description = "One page of delivery profiles; provider details are null without DNS provider read access", body = DeliveryProfilePage),
        (status = 400, description = "Unknown sort_by or sort_order value, or a search term longer than 100 characters", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_delivery_profiles(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Query(pagination): Query<temps_core::PaginationParams>,
    Query(filter): Query<DeliveryProfileSearchParams>,
) -> Result<impl IntoResponse, Problem> {
    let full_view = delivery_profile_full_view(&auth)?;
    let page = state
        .domain_delivery_service
        .list_profiles(pagination, filter.search.as_deref())
        .await?;
    Ok(Json(if full_view {
        page
    } else {
        page.without_provider_details()
    }))
}

/// Get one delivery profile.
///
/// Same visibility as `GET /delivery-profiles`: callers with DNS provider
/// read access see every field, project readers get the reduced view.
#[utoipa::path(
    get,
    path = "/delivery-profiles/{profile_id}",
    tag = "Traffic Delivery",
    params(("profile_id" = i32, Path, description = "Delivery profile ID")),
    responses(
        (status = 200, description = "Delivery profile; provider details are null without DNS provider read access", body = DeliveryProfileResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Delivery profile not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_delivery_profile(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DnsAppState>>,
    Path(profile_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    let full_view = delivery_profile_full_view(&auth)?;
    let profile = state
        .domain_delivery_service
        .get_profile(profile_id)
        .await?;
    Ok(Json(if full_view {
        profile
    } else {
        profile.without_provider_details()
    }))
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

/// List a project's domain delivery bindings, one page at a time.
///
/// Newest first by default. `sort_by` accepts `created_at` (default),
/// `hostname` or `updated_at`; `sort_order` accepts `asc` or `desc`
/// (default), case-insensitive. Binding ID breaks ties in the same direction.
/// `page_size` defaults to 20 and is clamped to 1..=100.
#[utoipa::path(
    get,
    path = "/projects/{project_id}/domain-delivery-bindings",
    tag = "Traffic Delivery",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        temps_core::PaginationParams
    ),
    responses(
        (status = 200, description = "One page of the project's domain delivery bindings", body = DomainDeliveryBindingPage),
        (status = 400, description = "Unknown sort_by or sort_order value", body = ProblemDetails),
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
    Query(pagination): Query<temps_core::PaginationParams>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(auth, ProjectsRead, project_id, state.project_access_checker);
    Ok(Json(
        state
            .domain_delivery_service
            .list_bindings(project_id, pagination)
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
        (status = 409, description = "Routing, DNS records, or the DNS provider or managed zone changed since preview, the project or environment is being deleted, or another operation holds the hostname", body = ProblemDetails),
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
    let preview_id = req.preview_id;
    let result = state
        .domain_delivery_service
        .apply(project_id, auth.user_id(), preview_id, req.adopt_records)
        .await;
    // Audited whether it succeeded or failed: a failed apply can already
    // have changed routing, DNS, or CDN state.
    let (action, resource_id, details) = apply_audit_entry(preview_id, &result);
    audit(
        &state,
        &auth,
        &metadata,
        action,
        Some(project_id),
        resource_id,
        details,
    )
    .await;
    Ok(Json(result?))
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
        (status = 204, description = "DNS record and binding removed; a Bunny hostname is detached only when Temps added it to the Pull Zone (see bunny_hostname_owned)"),
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
    let result = state
        .domain_delivery_service
        .delete_binding(project_id, binding_id)
        .await;
    // Audited whether it succeeded or failed: a failed cleanup can already
    // have removed the DNS record or the CDN hostname.
    let (action, resource_id, details) = delete_audit_entry(binding_id, &result);
    audit(
        &state,
        &auth,
        &metadata,
        action,
        Some(project_id),
        resource_id,
        details,
    )
    .await;
    result?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::{DeliveryIncomplete, DeliveryOperation};
    use crate::services::{DnsProviderService, DnsRecordService, ManagedDnsRecordService};
    use async_trait::async_trait;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::sync::Mutex;
    use temps_core::{AuditLogger, Job, JobQueue, JobReceiver, QueueError};

    /// An apply that wrote DNS, then failed requesting the certificate.
    fn incomplete_apply(preview_id: Uuid, source: DnsError) -> DnsError {
        DnsError::DeliveryIncomplete(Box::new(DeliveryIncomplete {
            operation: DeliveryOperation::Apply,
            project_id: 7,
            environment_id: 10,
            hostname: "app.example.com".into(),
            preview_id: Some(preview_id),
            binding_id: Some(5),
            completed_steps: vec![
                DeliveryStep::BindingReserved,
                DeliveryStep::DnsRecordWritten,
            ],
            failed_step: DeliveryStep::CertificateRequested,
            source,
        }))
    }

    fn binding() -> DomainDeliveryBindingResponse {
        DomainDeliveryBindingResponse {
            id: 5,
            hostname: "app.example.com".into(),
            project_id: 7,
            environment_id: 10,
            custom_domain_id: 4,
            delivery_profile_id: 2,
            delivery_profile_name: "Edge".into(),
            profile_source: "project".into(),
            provider_kind: DeliveryProviderKind::Bunny,
            dns_provider_id: 3,
            zone: "example.com".into(),
            origin_target: "edge.example.net".into(),
            record_type: crate::providers::DnsRecordType::CNAME,
            proxied: false,
            bunny_hostname_owned: true,
            status: "dns_configured".into(),
            last_error: None,
            applied_at: Some(chrono::Utc::now()),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn delivery_incomplete_keeps_the_status_and_title_of_the_error_that_stopped_it() {
        let sources: [fn() -> DnsError; 6] = [
            || {
                DnsError::ApiError(
                    "Bunny API request to /pullzone/loadFreeCertificate failed (HTTP 500)".into(),
                )
            },
            || DnsError::RateLimited("Bunny API rate limited the request (HTTP 429)".into()),
            || DnsError::Validation("Bunny API resource was not found (HTTP 404)".into()),
            || DnsError::RecordLocked {
                zone: "example.com".into(),
                name: "app".into(),
            },
            || DnsError::ConnectionFailed("Provider readback did not match".into()),
            || DnsError::Database(sea_orm::DbErr::Custom("connection reset".into())),
        ];
        for source in sources {
            let plain = Problem::from(source());
            let wrapped = Problem::from(incomplete_apply(Uuid::nil(), source()));
            assert_eq!(wrapped.status_code, plain.status_code);
            assert_eq!(wrapped.body.get("title"), plain.body.get("title"));
            let detail = wrapped
                .body
                .get("detail")
                .and_then(|value| value.as_str())
                .expect("detail");
            assert!(
                detail.contains("failed at step 'certificate_requested'"),
                "{detail}"
            );
            assert!(
                detail.contains("[binding_reserved, dns_record_written]"),
                "{detail}"
            );
            let source_detail = source().to_string();
            assert!(detail.contains(&source_detail), "{detail}");
        }
    }

    #[test]
    fn delivery_zone_unavailable_is_a_conflict_naming_provider_and_zone() {
        let problem = Problem::from(DnsError::DeliveryZoneUnavailable {
            hostname: "app.example.com".into(),
            zone: "example.com".into(),
            provider_id: 3,
            reason: "managed domain 9 is no longer auto-managed".into(),
        });
        assert_eq!(problem.status_code, StatusCode::CONFLICT);
        let detail = problem
            .body
            .get("detail")
            .and_then(|value| value.as_str())
            .expect("detail");
        assert!(
            detail.contains("zone 'example.com' on DNS provider 3"),
            "{detail}"
        );
        assert!(detail.contains("no longer auto-managed"), "{detail}");
    }

    #[test]
    fn partial_apply_failure_audits_completed_and_failed_steps() {
        let preview_id = Uuid::new_v4();
        let result = Err(incomplete_apply(
            preview_id,
            DnsError::ApiError(
                "Bunny API request to /pullzone/loadFreeCertificate failed (HTTP 500)".into(),
            ),
        ));
        let (action, resource_id, details) = apply_audit_entry(preview_id, &result);
        assert_eq!(action, "DOMAIN_DELIVERY_BINDING_APPLY_FAILED");
        assert_eq!(resource_id, "5");
        let json = serde_json::to_value(&details).expect("serializable audit details");
        assert_eq!(json["kind"], "binding");
        assert_eq!(json["outcome"], "failed");
        assert_eq!(json["preview_id"], preview_id.to_string());
        assert_eq!(json["binding_id"], 5);
        assert_eq!(json["hostname"], "app.example.com");
        assert_eq!(json["environment_id"], 10);
        assert_eq!(json["failure"]["failed_step"], "certificate_requested");
        assert_eq!(
            json["failure"]["completed_steps"],
            serde_json::json!(["binding_reserved", "dns_record_written"])
        );
        let error = json["failure"]["error"].as_str().expect("error text");
        assert!(error.contains("HTTP 500"), "{error}");
    }

    #[test]
    fn apply_failure_before_any_change_audits_no_completed_steps() {
        let preview_id = Uuid::new_v4();
        let result = Err(DnsError::DomainNotFound(format!(
            "delivery preview {preview_id}"
        )));
        let (action, resource_id, details) = apply_audit_entry(preview_id, &result);
        assert_eq!(action, "DOMAIN_DELIVERY_BINDING_APPLY_FAILED");
        assert_eq!(resource_id, preview_id.to_string());
        let json = serde_json::to_value(&details).expect("serializable audit details");
        assert_eq!(json["outcome"], "failed");
        assert_eq!(json["preview_id"], preview_id.to_string());
        assert!(json["binding_id"].is_null());
        assert!(json["failure"]["failed_step"].is_null());
        assert_eq!(json["failure"]["completed_steps"], serde_json::json!([]));
        assert!(json["failure"]["error"]
            .as_str()
            .expect("error text")
            .contains(&preview_id.to_string()));
    }

    #[test]
    fn successful_apply_and_delete_audit_the_binding_without_a_failure() {
        let preview_id = Uuid::new_v4();
        let (action, resource_id, details) = apply_audit_entry(preview_id, &Ok(binding()));
        assert_eq!(action, "DOMAIN_DELIVERY_BINDING_APPLIED");
        assert_eq!(resource_id, "5");
        let json = serde_json::to_value(&details).expect("serializable audit details");
        assert_eq!(json["outcome"], "succeeded");
        assert_eq!(json["preview_id"], preview_id.to_string());
        assert_eq!(json["zone"], "example.com");
        assert_eq!(json["dns_provider_id"], 3);
        assert_eq!(json["provider_kind"], "bunny");
        assert!(json.get("failure").is_none(), "{json}");

        let (action, resource_id, details) = delete_audit_entry(5, &Ok(binding()));
        assert_eq!(action, "DOMAIN_DELIVERY_BINDING_DELETED");
        assert_eq!(resource_id, "5");
        let json = serde_json::to_value(&details).expect("serializable audit details");
        assert_eq!(json["outcome"], "succeeded");
        assert_eq!(json["hostname"], "app.example.com");
        assert!(json.get("preview_id").is_none(), "{json}");
        assert!(json.get("failure").is_none(), "{json}");
    }

    /// The audit says whether a Bunny hostname belongs to Temps (apply) and
    /// whether cleanup detached it or left the preexisting one (delete).
    #[test]
    fn bunny_binding_audits_record_whether_the_hostname_was_detached_or_kept() {
        let preview_id = Uuid::new_v4();
        let owned = binding();
        let preexisting = DomainDeliveryBindingResponse {
            bunny_hostname_owned: false,
            ..binding()
        };
        for (result, expected_apply, expected_delete) in [
            (&owned, "owned", "detached"),
            (&preexisting, "preexisting", "kept_preexisting"),
        ] {
            let (_, _, details) = apply_audit_entry(preview_id, &Ok(result.clone()));
            let json = serde_json::to_value(&details).expect("serializable audit details");
            assert_eq!(json["bunny_hostname"], expected_apply, "{json}");
            let (_, _, details) = delete_audit_entry(5, &Ok(result.clone()));
            let json = serde_json::to_value(&details).expect("serializable audit details");
            assert_eq!(json["bunny_hostname"], expected_delete, "{json}");
        }

        // Only Bunny cleanup touches a Pull Zone, and a failure carries its
        // completed steps instead.
        let direct = DomainDeliveryBindingResponse {
            provider_kind: DeliveryProviderKind::Direct,
            bunny_hostname_owned: false,
            ..binding()
        };
        let (_, _, details) = delete_audit_entry(5, &Ok(direct));
        let json = serde_json::to_value(&details).expect("serializable audit details");
        assert!(json.get("bunny_hostname").is_none(), "{json}");
        let (_, _, details) =
            delete_audit_entry(5, &Err(DnsError::DomainNotFound("project 7".into())));
        let json = serde_json::to_value(&details).expect("serializable audit details");
        assert!(json.get("bunny_hostname").is_none(), "{json}");
    }

    #[test]
    fn partial_cleanup_failure_audits_what_was_already_removed() {
        let result = Err(DnsError::DeliveryIncomplete(Box::new(DeliveryIncomplete {
            operation: DeliveryOperation::Cleanup,
            project_id: 7,
            environment_id: 10,
            hostname: "app.example.com".into(),
            preview_id: None,
            binding_id: Some(5),
            completed_steps: vec![DeliveryStep::DnsRecordRemoved],
            failed_step: DeliveryStep::BunnyHostnameRemoved,
            source: DnsError::ApiError(
                "Bunny API request to /pullzone/42/removeHostname failed (HTTP 500)".into(),
            ),
        })));
        let (action, resource_id, details) = delete_audit_entry(5, &result);
        assert_eq!(action, "DOMAIN_DELIVERY_BINDING_DELETE_FAILED");
        assert_eq!(resource_id, "5");
        let json = serde_json::to_value(&details).expect("serializable audit details");
        assert_eq!(json["outcome"], "failed");
        assert_eq!(json["binding_id"], 5);
        assert_eq!(json["hostname"], "app.example.com");
        assert_eq!(json["environment_id"], 10);
        assert!(json.get("preview_id").is_none(), "{json}");
        assert_eq!(json["failure"]["failed_step"], "bunny_hostname_removed");
        assert_eq!(
            json["failure"]["completed_steps"],
            serde_json::json!(["dns_record_removed"])
        );
        let error = json["failure"]["error"].as_str().expect("error text");
        assert!(
            error.contains("delete it again to finish cleanup"),
            "{error}"
        );
    }

    struct NoopQueue;

    #[async_trait]
    impl JobQueue for NoopQueue {
        async fn send(&self, _job: Job) -> Result<(), QueueError> {
            Ok(())
        }

        fn subscribe(&self) -> Box<dyn JobReceiver> {
            unreachable!("delivery handler tests never subscribe to jobs")
        }
    }

    #[derive(Default)]
    struct RecordingAudit {
        entries: Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl AuditLogger for RecordingAudit {
        async fn create_audit_log(&self, operation: &dyn AuditOperation) -> anyhow::Result<()> {
            let details = operation.serialize()?;
            self.entries
                .lock()
                .map_err(|_| anyhow::anyhow!("audit recorder lock poisoned"))?
                .push((operation.operation_type(), details));
            Ok(())
        }
    }

    fn state_with(db: MockDatabase, audit: Arc<RecordingAudit>) -> Arc<DnsAppState> {
        let db = Arc::new(db.into_connection());
        let encryption = Arc::new(temps_core::EncryptionService::new_from_password("test"));
        let provider_service = Arc::new(DnsProviderService::new(db.clone(), encryption.clone()));
        let managed_record_service = Arc::new(ManagedDnsRecordService::new(
            db.clone(),
            provider_service.clone(),
            encryption.clone(),
        ));
        Arc::new(DnsAppState {
            domain_delivery_service: Arc::new(DomainDeliveryService::new(
                db,
                managed_record_service.clone(),
                encryption,
            )),
            managed_record_service,
            project_access_checker: None,
            record_service: Arc::new(DnsRecordService::new(provider_service.clone())),
            provider_service,
            queue: Arc::new(NoopQueue),
            audit_service: audit,
        })
    }

    fn delivery_writer() -> temps_auth::AuthContext {
        let now = chrono::Utc::now();
        let user = temps_entities::users::Model {
            id: 42,
            name: "Delivery operator".into(),
            email: "delivery@example.com".into(),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            must_change_password: false,
            deleted_at: None,
            mfa_secret: None,
            mfa_enabled: false,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: now,
            updated_at: now,
        };
        temps_auth::AuthContext::new_api_key(
            user,
            None,
            Some(vec![
                Permission::DnsProvidersWrite,
                Permission::DnsAutomationWrite,
                Permission::ProjectsWrite,
            ]),
            "delivery-handler-test".into(),
            1,
        )
    }

    fn metadata() -> RequestMetadata {
        RequestMetadata {
            ip_address: "192.0.2.10".into(),
            user_agent: "delivery-handler-test".into(),
            headers: Default::default(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "http://localhost".into(),
            scheme: "http".into(),
            host: "localhost".into(),
            is_secure: false,
        }
    }

    #[tokio::test]
    async fn failed_apply_is_audited_once_with_its_outcome() {
        let preview_id = Uuid::new_v4();
        let audit = Arc::new(RecordingAudit::default());
        let state = state_with(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([
                    Vec::<temps_entities::domain_delivery_previews::Model>::new(),
                ]),
            audit.clone(),
        );
        let Err(problem) = apply_domain_delivery_binding(
            RequireAuth(delivery_writer()),
            State(state),
            Extension(metadata()),
            Path(7),
            Json(ApplyDomainDeliveryBindingRequest {
                preview_id,
                adopt_records: vec![],
            }),
        )
        .await
        else {
            panic!("applying a missing preview must fail");
        };
        assert_eq!(problem.status_code, StatusCode::NOT_FOUND);
        let entries = audit.entries.lock().expect("audit entries");
        assert_eq!(entries.len(), 1, "exactly one audit entry per apply");
        let (action, details) = &entries[0];
        assert_eq!(action, "DOMAIN_DELIVERY_BINDING_APPLY_FAILED");
        let details: serde_json::Value = serde_json::from_str(details).expect("audit json");
        assert_eq!(details["resource_id"], preview_id.to_string());
        assert_eq!(details["project_id"], 7);
        assert_eq!(details["details"]["outcome"], "failed");
        assert_eq!(details["details"]["preview_id"], preview_id.to_string());
    }

    #[tokio::test]
    async fn failed_binding_delete_is_audited_once_with_its_outcome() {
        let audit = Arc::new(RecordingAudit::default());
        let state = state_with(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<temps_entities::projects::Model>::new()]),
            audit.clone(),
        );
        let Err(problem) = delete_domain_delivery_binding(
            RequireAuth(delivery_writer()),
            State(state),
            Extension(metadata()),
            Path((7, 5)),
        )
        .await
        else {
            panic!("deleting a binding of a missing project must fail");
        };
        assert_eq!(problem.status_code, StatusCode::NOT_FOUND);
        let entries = audit.entries.lock().expect("audit entries");
        assert_eq!(entries.len(), 1, "exactly one audit entry per delete");
        let (action, details) = &entries[0];
        assert_eq!(action, "DOMAIN_DELIVERY_BINDING_DELETE_FAILED");
        let details: serde_json::Value = serde_json::from_str(details).expect("audit json");
        assert_eq!(details["resource_id"], "5");
        assert_eq!(details["details"]["outcome"], "failed");
        assert_eq!(details["details"]["binding_id"], 5);
        assert_eq!(
            details["details"]["failure"]["completed_steps"],
            serde_json::json!([])
        );
    }
}
