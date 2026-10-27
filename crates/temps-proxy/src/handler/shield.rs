// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP surface for the console's Shield tab (ADR 066, D11 and I10).
//!
//! Per environment, under `/projects/{project_id}/environments/{environment_id}/shield/`:
//! the summary, recent events, an address lookup and bypass tokens. Per
//! project, `/projects/{project_id}/shield/entries`, and for the platform,
//! `/settings/shield` and `/settings/shield/entries`. Handlers authorise,
//! call the service and audit; validation lives in the service.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Extension, Json,
};
use serde::Serialize;
use temps_auth::{permission_guard, project_permission_guard, project_scope_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem, ProblemDetails};
use temps_core::{AuditLogger, AuditOperation, RequestMetadata, ShieldSettings};
use tracing::error;

use crate::service::shield_console_service::{
    CreateShieldEntryRequest, CreateShieldTokenRequest, CreatedShieldTokenResponse, EntryOwner,
    ShieldChallengeCounts, ShieldConsoleError, ShieldConsoleService, ShieldEntryResponse,
    ShieldEntryScope, ShieldEntryVerdict, ShieldEventResponse, ShieldEventsQuery,
    ShieldEventsResponse, ShieldLookupQuery, ShieldLookupResponse, ShieldModeSource,
    ShieldProposalKind, ShieldProposalResponse, ShieldProposalStatus, ShieldProposalsQuery,
    ShieldSolveTimes, ShieldSourceCount, ShieldSummaryQuery, ShieldSummaryResponse,
    ShieldTokenResponse, ShieldTokenScope, ShieldVerdictCounts, ShieldWindow, ShieldWouldHave,
    UpdateShieldEntryRequest,
};

pub struct ShieldConsoleState {
    pub service: Arc<ShieldConsoleService>,
    pub config_service: Arc<temps_config::ConfigService>,
    pub audit_service: Arc<dyn AuditLogger>,
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

impl From<ShieldConsoleError> for Problem {
    fn from(error: ShieldConsoleError) -> Self {
        match error {
            ShieldConsoleError::ProjectNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Project Not Found")
                    .with_detail(error.to_string())
            }
            ShieldConsoleError::EnvironmentNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Environment Not Found")
                    .with_detail(error.to_string())
            }
            ShieldConsoleError::TokenNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Bypass Token Not Found")
                .with_detail(error.to_string()),
            ShieldConsoleError::EntryNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Shield Entry Not Found")
                .with_detail(error.to_string()),
            ShieldConsoleError::ProposalNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Shield Proposal Not Found")
                    .with_detail(error.to_string())
            }
            ShieldConsoleError::Validation { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Shield Request")
                .with_detail(error.to_string()),
            ShieldConsoleError::LimitReached { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Shield Limit Reached")
                .with_detail(error.to_string()),
            ShieldConsoleError::Snapshot { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Shield Snapshot Unreadable")
                    .with_detail(error.to_string())
            }
            ShieldConsoleError::Database(_) | ShieldConsoleError::Stats(_) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

fn settings_problem(operation: &str, error: temps_config::ConfigServiceError) -> Problem {
    error!("Failed to {operation} the Shield settings: {error}");
    problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
        .with_title("Settings Error")
        .with_detail(format!(
            "Failed to {operation} the Shield settings: {error}"
        ))
}

// ---- audit -------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct ShieldAuditContext {
    user_id: Option<i32>,
    ip_address: Option<String>,
    user_agent: String,
}

#[derive(Debug, Clone, Serialize)]
struct ShieldTokenAudit {
    context: ShieldAuditContext,
    project_id: i32,
    environment_id: i32,
    token_id: i64,
    name: String,
    scope: ShieldTokenScope,
}

#[derive(Debug, Clone, Serialize)]
struct ShieldEntryAudit {
    context: ShieldAuditContext,
    entry: ShieldEntryResponse,
}

#[derive(Debug, Clone, Serialize)]
struct ShieldProposalAudit {
    context: ShieldAuditContext,
    proposal: ShieldProposalResponse,
}

#[derive(Debug, Clone, Serialize)]
struct ShieldSettingsAudit {
    context: ShieldAuditContext,
    previous: Option<ShieldSettings>,
    settings: ShieldSettings,
}

/// One audit operation type per struct and name.
struct Named<T> {
    name: &'static str,
    body: T,
    context: ShieldAuditContext,
}

impl<T: Serialize + Send + Sync> AuditOperation for Named<T> {
    fn operation_type(&self) -> String {
        self.name.to_string()
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

    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(&self.body).map_err(|e| {
            anyhow::anyhow!("Failed to serialize audit operation {}: {}", self.name, e)
        })
    }
}

fn audit_context(user_id: Option<i32>, metadata: &RequestMetadata) -> ShieldAuditContext {
    ShieldAuditContext {
        user_id,
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    }
}

async fn write_audit<T: Serialize + Send + Sync>(
    state: &ShieldConsoleState,
    name: &'static str,
    context: ShieldAuditContext,
    body: T,
) {
    let operation = Named {
        name,
        body,
        context,
    };
    // The write already succeeded; a failed audit entry is reported, not fatal.
    if let Err(e) = state.audit_service.create_audit_log(&operation).await {
        error!("Failed to create audit log for {}: {}", name, e);
    }
}

// ---- environment -------------------------------------------------------

/// Shield summary for an environment
///
/// The mode the environment runs in and where it is set, requests by verdict
/// and by source, what `selective` would have blocked or challenged, and
/// challenge solve times, over the last hour, day or week.
#[utoipa::path(
    tag = "Shield",
    get,
    path = "/projects/{project_id}/environments/{environment_id}/shield/summary",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ShieldSummaryQuery
    ),
    responses(
        (status = 200, description = "Shield summary", body = ShieldSummaryResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project or environment not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_shield_summary(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ShieldConsoleState>>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
    Query(query): Query<ShieldSummaryQuery>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(auth, ProjectsRead, project_id, state.project_access_checker);
    project_scope_guard!(auth, project_id);
    Ok(Json(
        state
            .service
            .summary(project_id, environment_id, query.window.unwrap_or_default())
            .await?,
    ))
}

/// Recent Shield events for an environment
///
/// Newest first. Pass `next_before` back as `before` for older ones.
#[utoipa::path(
    tag = "Shield",
    get,
    path = "/projects/{project_id}/environments/{environment_id}/shield/events",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ShieldEventsQuery
    ),
    responses(
        (status = 200, description = "Recent events", body = ShieldEventsResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project or environment not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_shield_events(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ShieldConsoleState>>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
    Query(query): Query<ShieldEventsQuery>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(auth, ProjectsRead, project_id, state.project_access_checker);
    project_scope_guard!(auth, project_id);
    Ok(Json(
        state
            .service
            .events(project_id, environment_id, query)
            .await?,
    ))
}

/// Look up what Shield says about a client
///
/// Evaluates an address, a JA4 fingerprint or both against the newest stored
/// snapshot for this project, and says why ("spamhaus_drop (185.93.89.0/24)").
#[utoipa::path(
    tag = "Shield",
    get,
    path = "/projects/{project_id}/environments/{environment_id}/shield/lookup",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ShieldLookupQuery
    ),
    responses(
        (status = 200, description = "Verdict and reasons", body = ShieldLookupResponse),
        (status = 400, description = "Invalid address or fingerprint", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project or environment not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn lookup_shield_client(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ShieldConsoleState>>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
    Query(query): Query<ShieldLookupQuery>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(auth, ProjectsRead, project_id, state.project_access_checker);
    project_scope_guard!(auth, project_id);
    Ok(Json(
        state
            .service
            .lookup(project_id, environment_id, query)
            .await?,
    ))
}

/// List bypass tokens for an environment
///
/// The active tokens valid here: the environment's own and the project-wide
/// ones. Secrets are never returned.
#[utoipa::path(
    tag = "Shield",
    get,
    path = "/projects/{project_id}/environments/{environment_id}/shield/tokens",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID")
    ),
    responses(
        (status = 200, description = "Active bypass tokens", body = Vec<ShieldTokenResponse>),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project or environment not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_shield_tokens(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ShieldConsoleState>>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(auth, ProjectsRead, project_id, state.project_access_checker);
    project_scope_guard!(auth, project_id);
    Ok(Json(
        state
            .service
            .list_tokens(project_id, environment_id)
            .await?,
    ))
}

/// Create a bypass token
///
/// Monitors, CI and API clients that send it in `X-Temps-Shield-Token` are
/// never challenged or blocked. The token is returned this once; only its
/// hash is stored.
#[utoipa::path(
    tag = "Shield",
    post,
    path = "/projects/{project_id}/environments/{environment_id}/shield/tokens",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID")
    ),
    request_body = CreateShieldTokenRequest,
    responses(
        (status = 201, description = "Token created; `secret` is shown only now", body = CreatedShieldTokenResponse),
        (status = 400, description = "Invalid name", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project or environment not found", body = ProblemDetails),
        (status = 409, description = "The project has the most tokens it may hold", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_shield_token(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ShieldConsoleState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
    Json(request): Json<CreateShieldTokenRequest>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        state.project_access_checker
    );
    project_scope_guard!(auth, project_id);
    let user_id = auth.user_id_opt();
    let created = state
        .service
        .create_token(project_id, environment_id, request, user_id)
        .await?;
    // The secret stays out of the audit entry.
    write_audit(
        &state,
        "SHIELD_BYPASS_TOKEN_CREATED",
        audit_context(user_id, &metadata),
        ShieldTokenAudit {
            context: audit_context(user_id, &metadata),
            project_id,
            environment_id,
            token_id: created.token.id,
            name: created.token.name.clone(),
            scope: created.token.scope,
        },
    )
    .await;
    Ok((
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(created),
    ))
}

/// Revoke a bypass token
///
/// It stops working on every ingress once they reload their routes, which
/// this triggers.
#[utoipa::path(
    tag = "Shield",
    delete,
    path = "/projects/{project_id}/environments/{environment_id}/shield/tokens/{token_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("token_id" = i64, Path, description = "Bypass token ID")
    ),
    responses(
        (status = 200, description = "Token revoked", body = ShieldTokenResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project, environment or token not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn revoke_shield_token(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ShieldConsoleState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, environment_id, token_id)): Path<(i32, i32, i64)>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        state.project_access_checker
    );
    project_scope_guard!(auth, project_id);
    let token = state
        .service
        .revoke_token(project_id, environment_id, token_id)
        .await?;
    let user_id = auth.user_id_opt();
    write_audit(
        &state,
        "SHIELD_BYPASS_TOKEN_REVOKED",
        audit_context(user_id, &metadata),
        ShieldTokenAudit {
            context: audit_context(user_id, &metadata),
            project_id,
            environment_id,
            token_id,
            name: token.name.clone(),
            scope: token.scope,
        },
    )
    .await;
    Ok(Json(token))
}

// ---- operator entries ----------------------------------------------------

async fn audit_entry(
    state: &ShieldConsoleState,
    name: &'static str,
    user_id: Option<i32>,
    metadata: &RequestMetadata,
    entry: &ShieldEntryResponse,
) {
    write_audit(
        state,
        name,
        audit_context(user_id, metadata),
        ShieldEntryAudit {
            context: audit_context(user_id, metadata),
            entry: entry.clone(),
        },
    )
    .await;
}

/// List a project's Shield entries
///
/// Address ranges and fingerprints this project allows, logs, challenges or
/// blocks, whatever the lists say.
#[utoipa::path(
    tag = "Shield",
    get,
    path = "/projects/{project_id}/shield/entries",
    params(("project_id" = i32, Path, description = "Project ID")),
    responses(
        (status = 200, description = "The project's entries, newest first", body = Vec<ShieldEntryResponse>),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_project_shield_entries(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ShieldConsoleState>>,
    Path(project_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(auth, ProjectsRead, project_id, state.project_access_checker);
    project_scope_guard!(auth, project_id);
    Ok(Json(
        state
            .service
            .list_entries(EntryOwner::Project(project_id))
            .await?,
    ))
}

/// Add a Shield entry to a project
///
/// One address, CIDR range or JA4 fingerprint, with the verdict it gets. It
/// applies from the next snapshot, within minutes.
#[utoipa::path(
    tag = "Shield",
    post,
    path = "/projects/{project_id}/shield/entries",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body = CreateShieldEntryRequest,
    responses(
        (status = 201, description = "Entry added", body = ShieldEntryResponse),
        (status = 400, description = "Invalid network, fingerprint, note or expiry", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Project not found", body = ProblemDetails),
        (status = 409, description = "The project has the most entries it may hold", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_project_shield_entry(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ShieldConsoleState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(request): Json<CreateShieldEntryRequest>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        state.project_access_checker
    );
    project_scope_guard!(auth, project_id);
    let user_id = auth.user_id_opt();
    let entry = state
        .service
        .create_entry(EntryOwner::Project(project_id), request, user_id)
        .await?;
    audit_entry(&state, "SHIELD_ENTRY_CREATED", user_id, &metadata, &entry).await;
    Ok((StatusCode::CREATED, Json(entry)))
}

/// Change a project's Shield entry
#[utoipa::path(
    tag = "Shield",
    put,
    path = "/projects/{project_id}/shield/entries/{entry_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("entry_id" = i64, Path, description = "Entry ID")
    ),
    request_body = UpdateShieldEntryRequest,
    responses(
        (status = 200, description = "Entry changed", body = ShieldEntryResponse),
        (status = 400, description = "Invalid note or expiry", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Entry not found in this project", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_project_shield_entry(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ShieldConsoleState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, entry_id)): Path<(i32, i64)>,
    Json(request): Json<UpdateShieldEntryRequest>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        state.project_access_checker
    );
    project_scope_guard!(auth, project_id);
    let entry = state
        .service
        .update_entry(EntryOwner::Project(project_id), entry_id, request)
        .await?;
    audit_entry(
        &state,
        "SHIELD_ENTRY_UPDATED",
        auth.user_id_opt(),
        &metadata,
        &entry,
    )
    .await;
    Ok(Json(entry))
}

/// Remove a project's Shield entry
#[utoipa::path(
    tag = "Shield",
    delete,
    path = "/projects/{project_id}/shield/entries/{entry_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("entry_id" = i64, Path, description = "Entry ID")
    ),
    responses(
        (status = 204, description = "Entry removed"),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Entry not found in this project", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_project_shield_entry(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ShieldConsoleState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, entry_id)): Path<(i32, i64)>,
) -> Result<impl IntoResponse, Problem> {
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        state.project_access_checker
    );
    project_scope_guard!(auth, project_id);
    let entry = state
        .service
        .delete_entry(EntryOwner::Project(project_id), entry_id)
        .await?;
    audit_entry(
        &state,
        "SHIELD_ENTRY_DELETED",
        auth.user_id_opt(),
        &metadata,
        &entry,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

// ---- platform ----------------------------------------------------------

/// Get the platform Shield settings
#[utoipa::path(
    tag = "Shield",
    get,
    path = "/settings/shield",
    responses(
        (status = 200, description = "Platform Shield settings", body = ShieldSettings),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_shield_settings(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ShieldConsoleState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    let settings = state
        .config_service
        .get_settings()
        .await
        .map_err(|e| settings_problem("read", e))?;
    Ok(Json(settings.shield))
}

/// Change the platform Shield settings
///
/// Instance admins only; audit-logged as `SHIELD_SETTINGS_UPDATED`.
#[utoipa::path(
    tag = "Shield",
    put,
    path = "/settings/shield",
    request_body = ShieldSettings,
    responses(
        (status = 200, description = "Settings saved", body = ShieldSettings),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_shield_settings(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ShieldConsoleState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<ShieldSettings>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    // Best effort: the previous value only enriches the audit record.
    let previous = match state.config_service.get_settings().await {
        Ok(settings) => Some(settings.shield),
        Err(e) => {
            error!(error = %e, "Could not read the previous Shield settings for the audit record");
            None
        }
    };
    let saved = state
        .config_service
        .update_shield_settings(request)
        .await
        .map_err(|e| settings_problem("save", e))?;
    let user_id = auth.user_id_opt();
    write_audit(
        &state,
        "SHIELD_SETTINGS_UPDATED",
        audit_context(user_id, &metadata),
        ShieldSettingsAudit {
            context: audit_context(user_id, &metadata),
            previous,
            settings: saved.clone(),
        },
    )
    .await;
    Ok(Json(saved))
}

/// List platform Shield entries
///
/// Entries that apply to every project.
#[utoipa::path(
    tag = "Shield",
    get,
    path = "/settings/shield/entries",
    responses(
        (status = 200, description = "Platform entries, newest first", body = Vec<ShieldEntryResponse>),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_platform_shield_entries(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ShieldConsoleState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    Ok(Json(
        state.service.list_entries(EntryOwner::Platform).await?,
    ))
}

/// Add a platform Shield entry
#[utoipa::path(
    tag = "Shield",
    post,
    path = "/settings/shield/entries",
    request_body = CreateShieldEntryRequest,
    responses(
        (status = 201, description = "Entry added", body = ShieldEntryResponse),
        (status = 400, description = "Invalid network, fingerprint, note or expiry", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 409, description = "The platform has the most entries it may hold", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_platform_shield_entry(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ShieldConsoleState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CreateShieldEntryRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    let user_id = auth.user_id_opt();
    let entry = state
        .service
        .create_entry(EntryOwner::Platform, request, user_id)
        .await?;
    audit_entry(&state, "SHIELD_ENTRY_CREATED", user_id, &metadata, &entry).await;
    Ok((StatusCode::CREATED, Json(entry)))
}

/// Change a platform Shield entry
#[utoipa::path(
    tag = "Shield",
    put,
    path = "/settings/shield/entries/{entry_id}",
    params(("entry_id" = i64, Path, description = "Entry ID")),
    request_body = UpdateShieldEntryRequest,
    responses(
        (status = 200, description = "Entry changed", body = ShieldEntryResponse),
        (status = 400, description = "Invalid note or expiry", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Entry not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_platform_shield_entry(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ShieldConsoleState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(entry_id): Path<i64>,
    Json(request): Json<UpdateShieldEntryRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    let entry = state
        .service
        .update_entry(EntryOwner::Platform, entry_id, request)
        .await?;
    audit_entry(
        &state,
        "SHIELD_ENTRY_UPDATED",
        auth.user_id_opt(),
        &metadata,
        &entry,
    )
    .await;
    Ok(Json(entry))
}

/// Remove a platform Shield entry
#[utoipa::path(
    tag = "Shield",
    delete,
    path = "/settings/shield/entries/{entry_id}",
    params(("entry_id" = i64, Path, description = "Entry ID")),
    responses(
        (status = 204, description = "Entry removed"),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Entry not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_platform_shield_entry(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ShieldConsoleState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(entry_id): Path<i64>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    let entry = state
        .service
        .delete_entry(EntryOwner::Platform, entry_id)
        .await?;
    audit_entry(
        &state,
        "SHIELD_ENTRY_DELETED",
        auth.user_id_opt(),
        &metadata,
        &entry,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// List fleet learning's proposals
///
/// Addresses and fingerprints learning proposes to challenge or block. None
/// applies until an instance admin approves it.
#[utoipa::path(
    tag = "Shield",
    get,
    path = "/settings/shield/proposals",
    params(ShieldProposalsQuery),
    responses(
        (status = 200, description = "Proposals that have not expired, newest evidence first", body = Vec<ShieldProposalResponse>),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_shield_proposals(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ShieldConsoleState>>,
    Query(query): Query<ShieldProposalsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    Ok(Json(state.service.list_proposals(query.status).await?))
}

/// Approve a proposal
///
/// The address or fingerprint is challenged or blocked on every ingress
/// until the proposal expires. Also changes an earlier rejection.
#[utoipa::path(
    tag = "Shield",
    post,
    path = "/settings/shield/proposals/{proposal_id}/approve",
    params(("proposal_id" = i64, Path, description = "Proposal ID")),
    responses(
        (status = 200, description = "Proposal approved", body = ShieldProposalResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Proposal not found or expired", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn approve_shield_proposal(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ShieldConsoleState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(proposal_id): Path<i64>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    decide_proposal(
        &state,
        auth.user_id_opt(),
        &metadata,
        proposal_id,
        ShieldProposalStatus::Approved,
    )
    .await
}

/// Reject a proposal
///
/// Learning keeps the rejection until the proposal expires, so it is not
/// proposed again meanwhile. Also withdraws an earlier approval.
#[utoipa::path(
    tag = "Shield",
    post,
    path = "/settings/shield/proposals/{proposal_id}/reject",
    params(("proposal_id" = i64, Path, description = "Proposal ID")),
    responses(
        (status = 200, description = "Proposal rejected", body = ShieldProposalResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Proposal not found or expired", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn reject_shield_proposal(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ShieldConsoleState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(proposal_id): Path<i64>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    decide_proposal(
        &state,
        auth.user_id_opt(),
        &metadata,
        proposal_id,
        ShieldProposalStatus::Rejected,
    )
    .await
}

async fn decide_proposal(
    state: &ShieldConsoleState,
    user_id: Option<i32>,
    metadata: &RequestMetadata,
    proposal_id: i64,
    status: ShieldProposalStatus,
) -> Result<Json<ShieldProposalResponse>, Problem> {
    let proposal = state
        .service
        .decide_proposal(proposal_id, status, user_id)
        .await?;
    let name = match status {
        ShieldProposalStatus::Approved => "SHIELD_PROPOSAL_APPROVED",
        _ => "SHIELD_PROPOSAL_REJECTED",
    };
    write_audit(
        state,
        name,
        audit_context(user_id, metadata),
        ShieldProposalAudit {
            context: audit_context(user_id, metadata),
            proposal: proposal.clone(),
        },
    )
    .await;
    Ok(Json(proposal))
}

pub fn create_routes() -> axum::Router<Arc<ShieldConsoleState>> {
    use axum::routing::{delete, get, post, put};

    const ENV: &str = "/projects/{project_id}/environments/{environment_id}/shield";
    axum::Router::new()
        .route(&format!("{ENV}/summary"), get(get_shield_summary))
        .route(&format!("{ENV}/events"), get(list_shield_events))
        .route(&format!("{ENV}/lookup"), get(lookup_shield_client))
        .route(
            &format!("{ENV}/tokens"),
            get(list_shield_tokens).post(create_shield_token),
        )
        .route(
            &format!("{ENV}/tokens/{{token_id}}"),
            delete(revoke_shield_token),
        )
        .route(
            "/projects/{project_id}/shield/entries",
            get(list_project_shield_entries).post(create_project_shield_entry),
        )
        .route(
            "/projects/{project_id}/shield/entries/{entry_id}",
            put(update_project_shield_entry).delete(delete_project_shield_entry),
        )
        .route(
            "/settings/shield",
            get(get_shield_settings).put(update_shield_settings),
        )
        .route(
            "/settings/shield/entries",
            get(list_platform_shield_entries).post(create_platform_shield_entry),
        )
        .route(
            "/settings/shield/entries/{entry_id}",
            put(update_platform_shield_entry).delete(delete_platform_shield_entry),
        )
        .route("/settings/shield/proposals", get(list_shield_proposals))
        .route(
            "/settings/shield/proposals/{proposal_id}/approve",
            post(approve_shield_proposal),
        )
        .route(
            "/settings/shield/proposals/{proposal_id}/reject",
            post(reject_shield_proposal),
        )
}

pub fn openapi() -> utoipa::openapi::OpenApi {
    use utoipa::OpenApi;

    #[derive(OpenApi)]
    #[openapi(
        paths(
            get_shield_summary,
            list_shield_events,
            lookup_shield_client,
            list_shield_tokens,
            create_shield_token,
            revoke_shield_token,
            list_project_shield_entries,
            create_project_shield_entry,
            update_project_shield_entry,
            delete_project_shield_entry,
            get_shield_settings,
            update_shield_settings,
            list_platform_shield_entries,
            create_platform_shield_entry,
            update_platform_shield_entry,
            delete_platform_shield_entry,
            list_shield_proposals,
            approve_shield_proposal,
            reject_shield_proposal,
        ),
        components(schemas(
            ShieldSummaryResponse,
            ShieldWindow,
            ShieldModeSource,
            ShieldVerdictCounts,
            ShieldSourceCount,
            ShieldWouldHave,
            ShieldChallengeCounts,
            ShieldSolveTimes,
            ShieldEventsResponse,
            ShieldEventResponse,
            ShieldLookupResponse,
            ShieldTokenResponse,
            ShieldTokenScope,
            CreateShieldTokenRequest,
            CreatedShieldTokenResponse,
            ShieldEntryResponse,
            ShieldEntryScope,
            ShieldEntryVerdict,
            CreateShieldEntryRequest,
            UpdateShieldEntryRequest,
            ShieldProposalResponse,
            ShieldProposalKind,
            ShieldProposalStatus,
            ShieldSettings,
            temps_shield::ShieldMode,
        )),
        tags(
            (name = "Shield", description = "Shield modes, events, lookups, bypass tokens and operator entries (ADR 066)")
        )
    )]
    struct ShieldApiDoc;

    ShieldApiDoc::openapi()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_auth::{AuthContext, Role};
    use tower::ServiceExt;

    #[derive(Default)]
    struct RecordingAudit {
        entries: std::sync::Mutex<Vec<(String, String)>>,
    }

    impl RecordingAudit {
        fn recorded(&self) -> Vec<(String, String)> {
            self.entries
                .lock()
                .map(|entries| entries.clone())
                .unwrap_or_default()
        }
    }

    #[async_trait::async_trait]
    impl AuditLogger for RecordingAudit {
        async fn create_audit_log(&self, operation: &dyn AuditOperation) -> anyhow::Result<()> {
            let payload = operation.serialize()?;
            if let Ok(mut entries) = self.entries.lock() {
                entries.push((operation.operation_type(), payload));
            }
            Ok(())
        }
    }

    struct DenyEveryProject;

    #[async_trait::async_trait]
    impl temps_core::ProjectAccessChecker for DenyEveryProject {
        async fn user_can_access_project(
            &self,
            _user_id: i32,
            _project_id: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            Ok(false)
        }
    }

    fn user() -> temps_entities::users::Model {
        let now = chrono::Utc::now();
        temps_entities::users::Model {
            id: 1,
            name: "Test User".to_string(),
            email: "user1@example.test".to_string(),
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
        }
    }

    fn metadata() -> RequestMetadata {
        RequestMetadata {
            ip_address: "203.0.113.9".to_string(),
            user_agent: "route-test".to_string(),
            headers: axum::http::HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "http://console.test".to_string(),
            scheme: "http".to_string(),
            host: "console.test".to_string(),
            is_secure: false,
        }
    }

    fn app(
        db: Arc<sea_orm::DatabaseConnection>,
        auth: Option<AuthContext>,
        checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
        audit: Arc<RecordingAudit>,
    ) -> axum::Router {
        let config = temps_config::ServerConfig::new(
            "127.0.0.1:3000".to_string(),
            "postgresql://test".to_string(),
            None,
            Some("127.0.0.1:8000".to_string()),
        )
        .expect("ServerConfig::new");
        let state = Arc::new(ShieldConsoleState {
            service: Arc::new(ShieldConsoleService::new(
                db.clone(),
                Arc::new(crate::storage::shield_stats::PostgresShieldStats::new(
                    db.clone(),
                )),
            )),
            config_service: Arc::new(temps_config::ConfigService::new(Arc::new(config), db)),
            audit_service: audit,
            project_access_checker: checker,
        });
        create_routes()
            .layer(axum::middleware::from_fn(
                move |mut request: axum::extract::Request, next: axum::middleware::Next| {
                    let auth = auth.clone();
                    async move {
                        request.extensions_mut().insert(metadata());
                        if let Some(auth) = auth {
                            request.extensions_mut().insert(auth);
                        }
                        next.run(request).await
                    }
                },
            ))
            .with_state(state)
    }

    fn mock() -> Arc<sea_orm::DatabaseConnection> {
        Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection())
    }

    fn request(method: &str, uri: &str) -> Request<Body> {
        let method = Method::from_bytes(method.as_bytes()).expect("known method");
        let body = match method {
            Method::POST if uri.ends_with("/tokens") => serde_json::json!({ "name": "CI" }),
            Method::POST => serde_json::json!({ "network": "203.0.113.0/24", "verdict": "block" }),
            Method::PUT if uri == "/settings/shield" => {
                serde_json::json!({ "share_observations": false })
            }
            Method::PUT => serde_json::json!({ "verdict": "log" }),
            _ => serde_json::Value::Null,
        };
        Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(if body.is_null() {
                Body::empty()
            } else {
                Body::from(body.to_string())
            })
            .expect("request builds")
    }

    async fn send(
        app: axum::Router,
        request: Request<Body>,
    ) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
        let response = app.oneshot(request).await.expect("router answers");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body reads");
        let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, headers, body)
    }

    const ENV: &str = "/projects/5/environments/9/shield";
    const PROJECT_ROUTES: [(&str, &str); 10] = [
        ("GET", "/projects/5/environments/9/shield/summary?window=7d"),
        ("GET", "/projects/5/environments/9/shield/events?limit=5"),
        (
            "GET",
            "/projects/5/environments/9/shield/lookup?ip=203.0.113.9",
        ),
        ("GET", "/projects/5/environments/9/shield/tokens"),
        ("POST", "/projects/5/environments/9/shield/tokens"),
        ("DELETE", "/projects/5/environments/9/shield/tokens/3"),
        ("GET", "/projects/5/shield/entries"),
        ("POST", "/projects/5/shield/entries"),
        ("PUT", "/projects/5/shield/entries/3"),
        ("DELETE", "/projects/5/shield/entries/3"),
    ];
    const PLATFORM_ROUTES: [(&str, &str); 9] = [
        ("GET", "/settings/shield"),
        ("PUT", "/settings/shield"),
        ("GET", "/settings/shield/entries"),
        ("POST", "/settings/shield/entries"),
        ("PUT", "/settings/shield/entries/3"),
        ("DELETE", "/settings/shield/entries/3"),
        ("GET", "/settings/shield/proposals?status=pending"),
        ("POST", "/settings/shield/proposals/3/approve"),
        ("POST", "/settings/shield/proposals/3/reject"),
    ];

    fn assert_problem(
        outcome: &(StatusCode, axum::http::HeaderMap, serde_json::Value),
        status: StatusCode,
        title: &str,
    ) {
        assert_eq!(outcome.0, status, "body: {}", outcome.2);
        assert_eq!(outcome.2["title"], title, "body: {}", outcome.2);
    }

    #[tokio::test]
    async fn every_route_requires_authentication() {
        for (method, uri) in PROJECT_ROUTES.iter().chain(PLATFORM_ROUTES.iter()) {
            // Nothing is queued: the database must not be reached.
            let audit = Arc::new(RecordingAudit::default());
            let outcome = send(app(mock(), None, None, audit.clone()), request(method, uri)).await;
            assert_problem(
                &outcome,
                StatusCode::UNAUTHORIZED,
                "Authentication Required",
            );
            assert!(audit.recorded().is_empty(), "{method} {uri}");
        }
    }

    #[tokio::test]
    async fn project_writes_need_projects_write_and_platform_writes_settings_write() {
        for (method, uri) in PROJECT_ROUTES {
            if method == "GET" {
                continue;
            }
            let outcome = send(
                app(
                    mock(),
                    Some(AuthContext::new_session(user(), Role::Reader)),
                    None,
                    Arc::default(),
                ),
                request(method, uri),
            )
            .await;
            assert_problem(&outcome, StatusCode::FORBIDDEN, "Insufficient Permissions");
        }
        // A project member is not an instance admin.
        for (method, uri) in PLATFORM_ROUTES {
            if method == "GET" {
                continue;
            }
            let outcome = send(
                app(
                    mock(),
                    Some(AuthContext::new_session(user(), Role::User)),
                    None,
                    Arc::default(),
                ),
                request(method, uri),
            )
            .await;
            assert_problem(&outcome, StatusCode::FORBIDDEN, "Insufficient Permissions");
        }
    }

    /// Proposals carry visitors' addresses and deciding one blocks traffic
    /// everywhere, so neither a project member nor a reader reaches them.
    #[tokio::test]
    async fn only_instance_admins_read_or_decide_proposals() {
        for role in [Role::User, Role::Reader] {
            for uri in [
                "/settings/shield/proposals",
                "/settings/shield/proposals?status=approved",
            ] {
                let audit = Arc::new(RecordingAudit::default());
                // Nothing is queued: the database must not be reached.
                let outcome = send(
                    app(
                        mock(),
                        Some(AuthContext::new_session(user(), role.clone())),
                        None,
                        audit.clone(),
                    ),
                    request("GET", uri),
                )
                .await;
                assert_problem(&outcome, StatusCode::FORBIDDEN, "Insufficient Permissions");
                assert!(audit.recorded().is_empty(), "{uri}");
            }
        }
    }

    #[tokio::test]
    async fn a_user_outside_the_project_team_is_refused_everywhere() {
        let checker: Arc<dyn temps_core::ProjectAccessChecker> = Arc::new(DenyEveryProject);
        for (method, uri) in PROJECT_ROUTES {
            let audit = Arc::new(RecordingAudit::default());
            let outcome = send(
                app(
                    mock(),
                    Some(AuthContext::new_session(user(), Role::User)),
                    Some(checker.clone()),
                    audit.clone(),
                ),
                request(method, uri),
            )
            .await;
            assert_problem(&outcome, StatusCode::FORBIDDEN, "Project Access Denied");
            assert!(audit.recorded().is_empty(), "{method} {uri}");
        }
    }

    #[test]
    fn errors_map_to_distinct_statuses() {
        let status = |error| Problem::from(error).status_code;
        assert_eq!(
            status(ShieldConsoleError::EnvironmentNotFound {
                project_id: 5,
                environment_id: 9
            }),
            StatusCode::NOT_FOUND
        );
        let missing = Problem::from(ShieldConsoleError::ProposalNotFound { proposal_id: 7 });
        assert_eq!(missing.status_code, StatusCode::NOT_FOUND);
        assert_eq!(
            missing.body.get("title").and_then(|title| title.as_str()),
            Some("Shield Proposal Not Found")
        );
        assert_eq!(
            status(ShieldConsoleError::Validation {
                message: "bad".into()
            }),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status(ShieldConsoleError::LimitReached {
                scope: "Project 5".into(),
                what: "Shield entries",
                limit: 500
            }),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status(ShieldConsoleError::Database(sea_orm::DbErr::Custom(
                "boom".into()
            ))),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    /// An admin approves and rejects over HTTP: each decision is stored with
    /// who made it and audited with the proposal; a missing one is a 404
    /// that leaves no audit record.
    #[tokio::test]
    async fn an_admin_decides_a_proposal_and_each_decision_is_audited() {
        use temps_database::test_utils::{is_container_runtime_unavailable, TestDatabase};
        let test_db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
                eprintln!("Docker not available, skipping Shield proposal route test");
                return;
            }
            Err(error) => panic!("test database: {error}"),
        };
        let db = test_db.connection_arc();
        sea_orm::ConnectionTrait::execute_unprepared(
            db.as_ref(),
            "INSERT INTO users (id, name, email, created_at, updated_at) VALUES \
             (1, 'Test User', 'user1@example.test', now(), now()); \
             INSERT INTO shield_observed (id, kind, subject, verdict, reason, first_seen, \
             last_seen, hits, expires_at) VALUES \
             (3, 'ip', '203.0.113.7', 'block', 'exploit CVE-2021-41773', now(), now(), 1, \
              now() + interval '7 days')",
        )
        .await
        .expect("seed user and proposal");
        let audit = Arc::new(RecordingAudit::default());
        let admin = AuthContext::new_session(user(), Role::Admin);
        let call = |method: &'static str, uri: &'static str| {
            send(
                app(db.clone(), Some(admin.clone()), None, audit.clone()),
                request(method, uri),
            )
        };

        let pending = call("GET", "/settings/shield/proposals?status=pending").await;
        assert_eq!(pending.0, StatusCode::OK, "body: {}", pending.2);
        assert_eq!(pending.2[0]["subject"], "203.0.113.7");
        assert_eq!(pending.2[0]["status"], "pending");

        let approved = call("POST", "/settings/shield/proposals/3/approve").await;
        assert_eq!(approved.0, StatusCode::OK, "body: {}", approved.2);
        assert_eq!(approved.2["status"], "approved");
        assert_eq!(approved.2["decided_by"], 1);
        assert!(approved.2["decided_at"].is_string());

        let rejected = call("POST", "/settings/shield/proposals/3/reject").await;
        assert_eq!(rejected.0, StatusCode::OK, "body: {}", rejected.2);
        assert_eq!(rejected.2["status"], "rejected");
        let listed = call("GET", "/settings/shield/proposals?status=rejected").await;
        assert_eq!(
            listed.2.as_array().map(Vec::len),
            Some(1),
            "body: {}",
            listed.2
        );

        let missing = call("POST", "/settings/shield/proposals/99/approve").await;
        assert_problem(&missing, StatusCode::NOT_FOUND, "Shield Proposal Not Found");

        let recorded = audit.recorded();
        let names: Vec<&str> = recorded.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            ["SHIELD_PROPOSAL_APPROVED", "SHIELD_PROPOSAL_REJECTED"]
        );
        assert!(
            recorded[0].1.contains(r#""subject":"203.0.113.7""#),
            "{}",
            recorded[0].1
        );
        assert!(
            recorded[0].1.contains(r#""status":"approved""#),
            "{}",
            recorded[0].1
        );
    }

    /// The minted token is returned once, uncached, and never reaches the
    /// audit log; the platform settings save is audited with both values.
    #[tokio::test]
    async fn a_new_token_is_shown_once_and_kept_out_of_the_audit_log() {
        use temps_database::test_utils::{is_container_runtime_unavailable, TestDatabase};
        let test_db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
                eprintln!("Docker not available, skipping Shield token route test");
                return;
            }
            Err(error) => panic!("test database: {error}"),
        };
        let db = test_db.connection_arc();
        sea_orm::ConnectionTrait::execute_unprepared(
            db.as_ref(),
            "INSERT INTO users (id, name, email, created_at, updated_at) VALUES \
             (1, 'Test User', 'user1@example.test', now(), now()); \
             INSERT INTO projects (id, name, repo_name, repo_owner, directory, main_branch, \
             preset, created_at, updated_at, slug) VALUES \
             (5, 'shield-routes', 'repo', 'owner', '.', 'main', 'nodejs', now(), now(), 'shield-routes'); \
             INSERT INTO environments (id, project_id, name, slug, subdomain, host, upstreams, \
             created_at, updated_at) VALUES \
             (9, 5, 'production', 'production', 'shield-routes-prod', '', '[]', now(), now())",
        )
        .await
        .expect("seed project and environment");
        let audit = Arc::new(RecordingAudit::default());
        let admin = AuthContext::new_session(user(), Role::Admin);
        let outcome = send(
            app(db.clone(), Some(admin.clone()), None, audit.clone()),
            request("POST", &format!("{ENV}/tokens")),
        )
        .await;
        assert_eq!(outcome.0, StatusCode::CREATED, "body: {}", outcome.2);
        assert_eq!(outcome.1[header::CACHE_CONTROL], "no-store");
        let secret = outcome.2["secret"]
            .as_str()
            .expect("secret returned")
            .to_string();
        assert!(secret.starts_with("tst_"), "{secret}");
        assert_eq!(outcome.2["scope"], "environment");
        assert_eq!(outcome.2["name"], "CI");

        let listed = send(
            app(db.clone(), Some(admin.clone()), None, audit.clone()),
            request("GET", &format!("{ENV}/tokens")),
        )
        .await;
        assert_eq!(listed.0, StatusCode::OK, "body: {}", listed.2);
        assert!(!listed.2.to_string().contains(&secret));
        assert_eq!(listed.2[0]["id"], outcome.2["id"]);

        let settings = send(
            app(db.clone(), Some(admin), None, audit.clone()),
            request("PUT", "/settings/shield"),
        )
        .await;
        assert_eq!(settings.0, StatusCode::OK, "body: {}", settings.2);
        assert_eq!(settings.2["share_observations"], false);

        let recorded = audit.recorded();
        assert_eq!(recorded[0].0, "SHIELD_BYPASS_TOKEN_CREATED");
        assert!(recorded
            .iter()
            .all(|(_, payload)| !payload.contains(&secret)));
        assert_eq!(recorded[1].0, "SHIELD_SETTINGS_UPDATED");
        assert!(
            recorded[1]
                .1
                .contains(r#""previous":{"share_observations":true}"#),
            "{}",
            recorded[1].1
        );
    }
}
