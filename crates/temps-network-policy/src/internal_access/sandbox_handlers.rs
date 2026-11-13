// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP surface for internal access grants with a sandbox caller (ADR 105).
//!
//! The grant belongs to the project being called: listing needs
//! `projects:read` on it, creating and deleting `projects:write`, never a
//! deployment token. Creating also needs the sandbox to be the caller's
//! own, so nobody can name or probe a sandbox they cannot see. The sandbox
//! page lists what its owner's sandbox may call.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use temps_authz::{
    deny_deployment_token, project_permission_guard, project_scope_guard, Permission, RequireAuth,
};
use temps_core::problemdetails::{Problem, ProblemDetails};
use temps_core::{AuditOperation, RequestMetadata, UtcDateTime};
use tracing::{error, info};
use utoipa::{IntoParams, ToSchema};

use super::handlers::{AuditContext, InternalAccessAppState, ListInternalAccessQuery};
use super::sandbox_grants::{
    NewSandboxGrant, SandboxAddressPage, SandboxAddressView, SandboxGrantPage, SandboxGrantView,
};

// ------------------------------------------------------------------ DTOs

/// One sandbox allowed to call this project (or one environment of it).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct InternalAccessSandboxGrantResponse {
    pub id: i32,
    pub target_project_id: i32,
    pub target_project_name: String,
    pub target_project_slug: String,
    /// `null`: every environment of the target project.
    pub target_environment_id: Option<i32>,
    pub target_environment_name: Option<String>,
    /// The sandbox's public id.
    #[schema(example = "sbx_a1b2c3d4e5f6")]
    pub sandbox_id: String,
    /// The name the user gave the sandbox.
    pub sandbox_name: String,
    /// `running` or `stopped`; a destroyed sandbox's grants are removed.
    pub sandbox_status: String,
    pub description: Option<String>,
    pub created_by: Option<i32>,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: UtcDateTime,
}

impl From<SandboxGrantView> for InternalAccessSandboxGrantResponse {
    fn from(grant: SandboxGrantView) -> Self {
        Self {
            id: grant.id,
            target_project_id: grant.target_project_id,
            target_project_name: grant.target_project_name,
            target_project_slug: grant.target_project_slug,
            target_environment_id: grant.target_environment_id,
            target_environment_name: grant.target_environment_name,
            sandbox_id: grant.sandbox_id,
            sandbox_name: grant.sandbox_name,
            sandbox_status: grant.sandbox_status,
            description: grant.description,
            created_by: grant.created_by,
            created_at: grant.created_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct InternalAccessSandboxGrantListResponse {
    pub items: Vec<InternalAccessSandboxGrantResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

impl From<SandboxGrantPage> for InternalAccessSandboxGrantListResponse {
    fn from(page: SandboxGrantPage) -> Self {
        Self {
            items: page.items.into_iter().map(Into::into).collect(),
            total: page.total,
            page: page.page,
            page_size: page.page_size,
        }
    }
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateInternalAccessSandboxGrantRequest {
    /// The environment of this project that may be called; omit for every
    /// environment, present and future.
    pub target_environment_id: Option<i32>,
    /// Public id of one of your sandboxes.
    #[schema(example = "sbx_a1b2c3d4e5f6")]
    pub sandbox_id: String,
    /// Why the grant exists, up to 200 characters.
    #[schema(example = "The coding agent calls the orders API")]
    pub description: Option<String>,
}

/// One internal address the sandbox may call, with the command that calls it.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SandboxInternalAddressResponse {
    pub grant_id: i32,
    pub project_id: i32,
    pub project_name: String,
    pub project_slug: String,
    pub environment_id: i32,
    pub environment_name: String,
    #[schema(example = "production.orders.temps.local")]
    pub hostname: String,
    #[schema(example = "http://production.orders.temps.local/")]
    pub url: String,
    /// Run inside the sandbox: its egress proxy sends it to this host's
    /// internal proxy.
    #[schema(example = "curl -sS http://production.orders.temps.local/")]
    pub curl_command: String,
    /// The same environment by its subdomain, `<subdomain>.temps.local`
    /// (ADR-105 D7). Absent when the subdomain is not one DNS label, or a
    /// managed service or another owner holds that name.
    #[schema(example = "orders-production.temps.local")]
    pub alias_hostname: Option<String>,
    #[schema(example = "curl -sS http://orders-production.temps.local/")]
    pub alias_curl_command: Option<String>,
}

impl From<SandboxAddressView> for SandboxInternalAddressResponse {
    fn from(address: SandboxAddressView) -> Self {
        let hostname = address.hostname();
        let url = format!("http://{hostname}/");
        let alias_hostname = address.alias_hostname();
        Self {
            alias_curl_command: alias_hostname
                .as_ref()
                .map(|alias| format!("curl -sS http://{alias}/")),
            alias_hostname,
            grant_id: address.grant_id,
            project_id: address.project_id,
            project_name: address.project_name,
            project_slug: address.project_slug,
            environment_id: address.environment_id,
            environment_name: address.environment_name,
            curl_command: format!("curl -sS {url}"),
            hostname,
            url,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SandboxInternalAddressListResponse {
    pub items: Vec<SandboxInternalAddressResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

impl From<SandboxAddressPage> for SandboxInternalAddressListResponse {
    fn from(page: SandboxAddressPage) -> Self {
        Self {
            items: page.items.into_iter().map(Into::into).collect(),
            total: page.total,
            page: page.page,
            page_size: page.page_size,
        }
    }
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct SandboxInternalAddressQuery {
    /// 1-based page, default 1.
    pub page: Option<u64>,
    /// Default 20, at most 100.
    pub page_size: Option<u64>,
}

// ----------------------------------------------------------------- audit

pub(super) const SANDBOX_GRANT_CREATED: &str = "INTERNAL_ACCESS_SANDBOX_GRANT_CREATED";
pub(super) const SANDBOX_GRANT_DELETED: &str = "INTERNAL_ACCESS_SANDBOX_GRANT_DELETED";

#[derive(Debug, Clone, Serialize)]
struct SandboxGrantAudit {
    #[serde(skip)]
    operation: &'static str,
    context: AuditContext,
    grant_id: i32,
    target_project_id: i32,
    target_project_slug: String,
    target_environment_id: Option<i32>,
    sandbox_id: String,
    sandbox_name: String,
    description: Option<String>,
}

impl SandboxGrantAudit {
    fn new(
        operation: &'static str,
        user_id: Option<i32>,
        metadata: &RequestMetadata,
        grant: &SandboxGrantView,
    ) -> Self {
        Self {
            operation,
            context: AuditContext {
                user_id,
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            grant_id: grant.id,
            target_project_id: grant.target_project_id,
            target_project_slug: grant.target_project_slug.clone(),
            target_environment_id: grant.target_environment_id,
            sandbox_id: grant.sandbox_id.clone(),
            sandbox_name: grant.sandbox_name.clone(),
            description: grant.description.clone(),
        }
    }
}

impl AuditOperation for SandboxGrantAudit {
    fn operation_type(&self) -> String {
        self.operation.to_string()
    }

    fn user_id(&self) -> Option<i32> {
        self.context.user_id
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> temps_core::AuditResult<String> {
        serde_json::to_string(self)
            .map_err(|source| temps_core::AuditError::serialize(self.operation, source))
    }
}

async fn write_audit(state: &InternalAccessAppState, audit: &SandboxGrantAudit) {
    // The write already succeeded; a failed audit entry is reported, not fatal.
    if let Err(e) = state.audit.create_audit_log(audit).await {
        error!(
            operation = audit.operation,
            grant_id = audit.grant_id,
            target_project_id = audit.target_project_id,
            sandbox_id = %audit.sandbox_id,
            error = %e,
            "failed to write the internal access sandbox grant audit entry"
        );
    }
}

/// A grant names a sandbox its creator owns; a request with no user (none
/// reaches here past `deny_deployment_token!`) owns none.
fn requesting_user(auth: &temps_authz::AuthContext) -> Result<i32, Problem> {
    auth.user_id_opt().ok_or_else(|| {
        temps_core::error_builder::ErrorBuilder::new(StatusCode::FORBIDDEN)
            .type_("https://temps.sh/probs/insufficient-permissions")
            .title("Insufficient Permissions")
            .detail("Only a user can name one of their sandboxes in an internal access grant")
            .build()
    })
}

// -------------------------------------------------------------- handlers

/// List the sandboxes allowed to call this project's internal addresses
///
/// Newest first. With `environment_id`, only the grants that cover that
/// environment (its own and the project-wide ones).
#[utoipa::path(
    tag = "Internal Access",
    get,
    path = "/projects/{project_id}/internal-access/sandboxes",
    params(
        ("project_id" = i32, Path, description = "The project being called"),
        ListInternalAccessQuery
    ),
    responses(
        (status = 200, description = "Sandbox grants on the project", body = InternalAccessSandboxGrantListResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project or environment not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_internal_access_sandbox_grants(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<InternalAccessAppState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<ListInternalAccessQuery>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(auth, ProjectsRead, project_id, state.project_access_checker);
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);

    let page = state
        .service
        .list_sandbox_grants(
            project_id,
            query.environment_id,
            query.page,
            query.page_size,
        )
        .await?;
    Ok(Json(InternalAccessSandboxGrantListResponse::from(page)))
}

/// Allow one of your sandboxes to call this project's internal addresses
///
/// The sandbox may then call this project's `<environment>.<project>.temps.local`
/// addresses (or one environment's) with plain `http://`, through its egress
/// proxy, in every egress mode. The internal proxy applies it with its next
/// route snapshot, within seconds. Destroying the sandbox removes the grant.
#[utoipa::path(
    tag = "Internal Access",
    post,
    path = "/projects/{project_id}/internal-access/sandboxes",
    params(("project_id" = i32, Path, description = "The project being called")),
    request_body = CreateInternalAccessSandboxGrantRequest,
    responses(
        (status = 201, description = "Grant created", body = InternalAccessSandboxGrantResponse),
        (status = 400, description = "Invalid grant", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions on the project", body = ProblemDetails),
        (status = 404, description = "Project, environment or sandbox not found", body = ProblemDetails),
        (status = 409, description = "Already granted, or too many grants", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_internal_access_sandbox_grant(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<InternalAccessAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    ai_call: Option<Extension<temps_core::ai_tool_call::AiToolCall>>,
    confirmation: Option<Extension<temps_core::ai_tool_call::ConfirmedAiWrite>>,
    Path(project_id): Path<i32>,
    Json(request): Json<CreateInternalAccessSandboxGrantRequest>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        state.project_access_checker
    );
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    // An agent, even one inside this very sandbox, may propose the grant;
    // only the owner's confirmation in the console makes it (ADR-105 D8).
    temps_core::ai_tool_call::require_human_confirmation(
        ai_call.as_ref().map(|m| &m.0),
        confirmation.as_ref().map(|m| &m.0),
        "create_internal_access_sandbox_grant",
    )?;
    let user_id = requesting_user(&auth)?;

    let grant = state
        .service
        .create_sandbox_grant(
            project_id,
            NewSandboxGrant {
                target_environment_id: request.target_environment_id,
                sandbox_id: request.sandbox_id,
                description: request.description,
            },
            user_id,
        )
        .await?;
    info!(
        grant_id = grant.id,
        target_project_id = grant.target_project_id,
        target_environment_id = ?grant.target_environment_id,
        sandbox_id = %grant.sandbox_id,
        "internal access granted to a sandbox"
    );
    write_audit(
        &state,
        &SandboxGrantAudit::new(SANDBOX_GRANT_CREATED, Some(user_id), &metadata, &grant),
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(InternalAccessSandboxGrantResponse::from(grant)),
    ))
}

/// Stop a sandbox from calling this project's internal addresses
///
/// The internal proxy refuses the sandbox again with its next route
/// snapshot, within seconds.
#[utoipa::path(
    tag = "Internal Access",
    delete,
    path = "/projects/{project_id}/internal-access/sandboxes/{grant_id}",
    params(
        ("project_id" = i32, Path, description = "The project being called"),
        ("grant_id" = i32, Path, description = "Sandbox grant ID")
    ),
    responses(
        (status = 204, description = "Grant removed"),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Grant not found on this project", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_internal_access_sandbox_grant(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<InternalAccessAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, grant_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        state.project_access_checker
    );
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);

    let grant = state
        .service
        .delete_sandbox_grant(project_id, grant_id)
        .await?;
    info!(
        grant_id,
        target_project_id = project_id,
        sandbox_id = %grant.sandbox_id,
        "internal access revoked from a sandbox"
    );
    write_audit(
        &state,
        &SandboxGrantAudit::new(SANDBOX_GRANT_DELETED, auth.user_id_opt(), &metadata, &grant),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// List the internal addresses one of your sandboxes may call
///
/// One entry per environment its grants cover, with the `curl` command to
/// run inside the sandbox.
#[utoipa::path(
    tag = "Internal Access",
    get,
    path = "/sandboxes/{sandbox_id}/internal-addresses",
    params(
        ("sandbox_id" = String, Path, description = "Public id of one of your sandboxes"),
        SandboxInternalAddressQuery
    ),
    responses(
        (status = 200, description = "Addresses the sandbox may call", body = SandboxInternalAddressListResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Sandbox not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_sandbox_internal_addresses(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<InternalAccessAppState>>,
    Path(sandbox_id): Path<String>,
    Query(query): Query<SandboxInternalAddressQuery>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    if !(auth.has_permission(&Permission::SandboxesRead)
        || auth.has_permission(&Permission::ProjectsRead))
    {
        return Err(
            temps_core::error_builder::ErrorBuilder::new(StatusCode::FORBIDDEN)
                .type_("https://temps.sh/probs/insufficient-permissions")
                .title("Insufficient Permissions")
                .detail("This operation requires the sandboxes:read permission")
                .value("required_permission", Permission::SandboxesRead.to_string())
                .build(),
        );
    }
    let user_id = requesting_user(&auth)?;
    let page = state
        .service
        .sandbox_addresses(&sandbox_id, user_id, query.page, query.page_size)
        .await?;
    Ok(Json(SandboxInternalAddressListResponse::from(page)))
}

#[cfg(test)]
mod tests;
