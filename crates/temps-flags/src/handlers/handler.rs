//! HTTP handlers for feature flags.

use std::sync::Arc;

use axum::{
    extract::{Extension, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
    routing::{delete, get, patch, post, put},
    Json, Router,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use temps_auth::{permission_guard, project_access_guard, project_scope_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_core::RequestMetadata;
use tracing::error;
use utoipa::{IntoParams, OpenApi};

use super::audit::{
    AuditContext, FeatureFlagArchivedAudit, FeatureFlagCreatedAudit,
    FeatureFlagEnvironmentValueSetAudit, FeatureFlagUpdatedAudit,
};
use super::types::*;
use crate::error::FlagError;
use crate::eval::{FlagSnapshot, FlagValueType};
use crate::services::{normalize_pagination, CreateFlag, SetEnvironmentValue, UpdateFlag};

#[derive(OpenApi)]
#[openapi(
    paths(
        list_flags,
        create_flag,
        get_flag,
        update_flag,
        archive_flag,
        set_flag_environment,
        get_flag_snapshot,
    ),
    components(schemas(
        CreateFlagRequest,
        UpdateFlagRequest,
        SetFlagEnvironmentRequest,
        FlagResponse,
        FlagEnvironmentResponse,
        FlagListResponse,
        FlagSnapshotResponse,
        ArchiveFlagResponse,
        FlagSnapshot,
        FlagValueType,
    )),
    tags((
        name = "Feature Flags",
        description = "Runtime configuration that changes without a redeploy"
    ))
)]
pub struct FlagsApiDoc;

pub fn configure_routes() -> Router<Arc<FlagsAppState>> {
    Router::new()
        .route("/projects/{project_id}/flags", get(list_flags))
        .route("/projects/{project_id}/flags", post(create_flag))
        .route("/projects/{project_id}/flags/{key}", get(get_flag))
        .route("/projects/{project_id}/flags/{key}", patch(update_flag))
        .route("/projects/{project_id}/flags/{key}", delete(archive_flag))
        .route(
            "/projects/{project_id}/flags/{key}/environments/{environment_id}",
            put(set_flag_environment),
        )
        // Delivery endpoint for running services. Scope comes from the
        // deployment token, not the URL.
        .route("/flags/snapshot", get(get_flag_snapshot))
}

// =============================================================================
// Error mapping
// =============================================================================

impl From<FlagError> for Problem {
    fn from(error: FlagError) -> Self {
        match error {
            FlagError::NotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Feature Flag Not Found")
                .with_detail(error.to_string()),

            FlagError::EnvironmentNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Environment Not Found")
                .with_detail(error.to_string()),

            FlagError::DuplicateKey { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Feature Flag Already Exists")
                .with_detail(error.to_string()),

            // Cross-tenant attachment is a client error, not a server one, but
            // it is worth distinguishing from a plain validation failure.
            FlagError::EnvironmentNotInProject { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Environment Not In Project")
                    .with_detail(error.to_string())
            }

            FlagError::InvalidKey { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Flag Key")
                .with_detail(error.to_string()),

            FlagError::ValueTypeMismatch { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Value Type Mismatch")
                .with_detail(error.to_string()),

            FlagError::Validation { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Validation Error")
                .with_detail(error.to_string()),

            FlagError::TokenNotEnvironmentScoped { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Environment Required")
                    .with_detail(error.to_string())
            }

            // A stored value_type that no longer parses is data corruption, not
            // bad input.
            FlagError::InvalidValueType { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Invalid Stored Value Type")
                    .with_detail(error.to_string())
            }

            FlagError::Database(_) => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Internal Server Error")
                .with_detail(error.to_string()),

            FlagError::Serialization { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

// =============================================================================
// Query parameters
// =============================================================================

#[derive(Debug, Deserialize, IntoParams)]
pub struct ListFlagsQuery {
    /// Include archived flags. Defaults to false.
    #[serde(default)]
    pub include_archived: bool,
    /// 1-indexed page number. Defaults to 1.
    pub page: Option<u64>,
    /// Items per page. Defaults to 20, capped at 100.
    pub page_size: Option<u64>,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct SnapshotQuery {
    /// Required only when the calling token is project-wide rather than scoped
    /// to a single environment.
    pub environment_id: Option<i32>,
}

// =============================================================================
// Helpers
// =============================================================================

fn audit_context(auth: &temps_auth::AuthContext, metadata: &RequestMetadata) -> AuditContext {
    AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    }
}

/// Weak-ish entity tag over the serialized snapshot.
///
/// The snapshot is sorted by key in the service, so equal flag state always
/// produces an equal tag and the SDK's poll collapses to a 304.
fn snapshot_etag(flags: &[FlagSnapshot]) -> Result<String, FlagError> {
    let body = serde_json::to_vec(flags).map_err(|e| FlagError::Serialization {
        key: "<snapshot>".to_string(),
        reason: e.to_string(),
    })?;

    let mut hasher = Sha256::new();
    hasher.update(&body);
    let digest = hasher.finalize();

    Ok(format!("\"{}\"", hex::encode(&digest[..16])))
}

// =============================================================================
// Management handlers
// =============================================================================

#[utoipa::path(
    tag = "Feature Flags",
    get,
    path = "/projects/{project_id}/flags",
    params(("project_id" = i32, Path, description = "Project ID"), ListFlagsQuery),
    responses(
        (status = 200, description = "Flags listed", body = FlagListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_flags(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<FlagsAppState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<ListFlagsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, FlagsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, &state.project_access_checker);

    let (entries, total) = state
        .flag_service
        .list(
            project_id,
            query.include_archived,
            query.page,
            query.page_size,
        )
        .await?;

    let flags: Vec<FlagResponse> = entries
        .into_iter()
        .map(|entry| FlagResponse::new(entry.flag, entry.environments))
        .collect();

    // Same clamp the query used, so `total_pages` can never describe a page
    // size the server did not actually serve.
    let (page, page_size) = normalize_pagination(query.page, query.page_size);
    let total_pages = total.div_ceil(page_size);

    Ok(Json(FlagListResponse {
        flags,
        total,
        page,
        page_size,
        total_pages,
    }))
}

#[utoipa::path(
    tag = "Feature Flags",
    post,
    path = "/projects/{project_id}/flags",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body = CreateFlagRequest,
    responses(
        (status = 201, description = "Flag created", body = FlagResponse),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 409, description = "Flag key already exists"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_flag(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<FlagsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(request): Json<CreateFlagRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, FlagsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, &state.project_access_checker);

    let flag = state
        .flag_service
        .create(
            project_id,
            CreateFlag {
                key: request.key,
                value_type: request.value_type,
                default_value: request.default_value,
                description: request.description,
                client_visible: request.client_visible,
            },
        )
        .await?;

    let audit = FeatureFlagCreatedAudit {
        context: audit_context(&auth, &metadata),
        project_id,
        flag_id: flag.id,
        key: flag.key.clone(),
        value_type: flag.value_type.clone(),
        default_value: flag.default_value.clone(),
        client_visible: flag.client_visible,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(
            "Failed to create audit log for feature flag creation: {}",
            e
        );
    }

    Ok((
        StatusCode::CREATED,
        Json(FlagResponse::new(flag, Vec::new())),
    ))
}

#[utoipa::path(
    tag = "Feature Flags",
    get,
    path = "/projects/{project_id}/flags/{key}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("key" = String, Path, description = "Flag key")
    ),
    responses(
        (status = 200, description = "Flag retrieved", body = FlagResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Flag not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_flag(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<FlagsAppState>>,
    Path((project_id, key)): Path<(i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, FlagsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, &state.project_access_checker);

    let entry = state.flag_service.get(project_id, &key).await?;

    Ok(Json(FlagResponse::new(entry.flag, entry.environments)))
}

#[utoipa::path(
    tag = "Feature Flags",
    patch,
    path = "/projects/{project_id}/flags/{key}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("key" = String, Path, description = "Flag key")
    ),
    request_body = UpdateFlagRequest,
    responses(
        (status = 200, description = "Flag updated", body = FlagResponse),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Flag not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_flag(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<FlagsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, key)): Path<(i32, String)>,
    Json(request): Json<UpdateFlagRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, FlagsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, &state.project_access_checker);

    let before = state.flag_service.get(project_id, &key).await?.flag;

    let flag = state
        .flag_service
        .update(
            project_id,
            &key,
            UpdateFlag {
                default_value: request.default_value,
                description: request.description,
                client_visible: request.client_visible,
            },
        )
        .await?;

    let audit = FeatureFlagUpdatedAudit {
        context: audit_context(&auth, &metadata),
        project_id,
        flag_id: flag.id,
        key: flag.key.clone(),
        old_default_value: before.default_value,
        new_default_value: flag.default_value.clone(),
        old_client_visible: before.client_visible,
        new_client_visible: flag.client_visible,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log for feature flag update: {}", e);
    }

    let environments = state.flag_service.get(project_id, &key).await?.environments;

    Ok(Json(FlagResponse::new(flag, environments)))
}

#[utoipa::path(
    tag = "Feature Flags",
    delete,
    path = "/projects/{project_id}/flags/{key}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("key" = String, Path, description = "Flag key")
    ),
    responses(
        (status = 200, description = "Flag archived", body = ArchiveFlagResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Flag not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn archive_flag(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<FlagsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, key)): Path<(i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, FlagsDelete);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, &state.project_access_checker);

    let flag = state.flag_service.archive(project_id, &key).await?;

    let audit = FeatureFlagArchivedAudit {
        context: audit_context(&auth, &metadata),
        project_id,
        flag_id: flag.id,
        key: flag.key.clone(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log for feature flag archive: {}", e);
    }

    Ok(Json(ArchiveFlagResponse {
        key: flag.key,
        archived_at: flag.archived_at.map(|t| t.to_rfc3339()),
    }))
}

/// Set a flag's value in one environment, and/or flip its kill switch.
#[utoipa::path(
    tag = "Feature Flags",
    put,
    path = "/projects/{project_id}/flags/{key}/environments/{environment_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("key" = String, Path, description = "Flag key"),
        ("environment_id" = i32, Path, description = "Environment ID")
    ),
    request_body = SetFlagEnvironmentRequest,
    responses(
        (status = 200, description = "Environment value set", body = FlagEnvironmentResponse),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Flag or environment not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn set_flag_environment(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<FlagsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, key, environment_id)): Path<(i32, String, i32)>,
    Json(request): Json<SetFlagEnvironmentRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, FlagsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, &state.project_access_checker);

    // Captured before the write so the audit entry records what the flag was
    // actually changed *from* — the only record that a production behaviour
    // change happened, since a flag flip leaves no deployment behind.
    let before = state
        .flag_service
        .get(project_id, &key)
        .await?
        .environments
        .into_iter()
        .find(|row| row.environment_id == environment_id);

    let model = state
        .flag_service
        .set_environment_value(
            project_id,
            &key,
            environment_id,
            SetEnvironmentValue {
                value: request.value,
                enabled: request.enabled,
            },
        )
        .await?;

    let audit = FeatureFlagEnvironmentValueSetAudit {
        context: audit_context(&auth, &metadata),
        project_id,
        flag_id: model.flag_id,
        key: key.clone(),
        environment_id,
        old_enabled: before.as_ref().map(|row| row.enabled),
        new_enabled: model.enabled,
        old_value: before.and_then(|row| row.value),
        new_value: model.value.clone(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(
            "Failed to create audit log for feature flag environment value: {}",
            e
        );
    }

    Ok(Json(FlagEnvironmentResponse::from(model)))
}

// =============================================================================
// Delivery handler
// =============================================================================

/// Every flag for the caller's environment, collapsed to what the evaluator
/// needs.
///
/// Scope comes from the deployment token, never from the URL: a container's
/// baked-in `TEMPS_API_TOKEN` identifies exactly one project (and usually one
/// environment), so a compromised app cannot read another tenant's flags by
/// changing a path parameter.
///
/// Supports `If-None-Match`, so the SDK's background poll is a 304 in the
/// common case.
#[utoipa::path(
    tag = "Feature Flags",
    get,
    path = "/flags/snapshot",
    params(SnapshotQuery),
    responses(
        (status = 200, description = "Snapshot for the environment", body = FlagSnapshotResponse),
        (status = 304, description = "Snapshot unchanged"),
        (status = 400, description = "Environment could not be determined"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_flag_snapshot(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<FlagsAppState>>,
    headers: HeaderMap,
    Query(query): Query<SnapshotQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, FlagsRead);

    let token_info = auth.deployment_token_info();

    // A deployment token pins the project. Anything else (session, API key)
    // must name the project it wants and pass the access guard.
    let project_id = match auth.project_id() {
        Some(project_id) => project_id,
        None => {
            return Err(problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Deployment Token Required")
                .with_detail(
                    "The flag snapshot endpoint is scoped by a deployment token. \
                     Use TEMPS_API_TOKEN, or read flags via /projects/{project_id}/flags.",
                ))
        }
    };

    // Prefer the token's own environment. A project-wide token has to say
    // which environment it means, and the service verifies that environment
    // actually belongs to this project before returning anything.
    let environment_id = token_info
        .and_then(|info| info.environment_id)
        .or(query.environment_id)
        .ok_or_else(|| Problem::from(FlagError::TokenNotEnvironmentScoped { project_id }))?;

    let flags = state
        .flag_service
        // Server-side callers see every flag, including server-only ones. The
        // client_visible filter belongs to the unauthenticated same-origin
        // endpoint, which does not exist yet.
        .snapshot(project_id, environment_id, false)
        .await
        // Collapse "no such environment" and "someone else's environment" into
        // one response. Distinguishing them lets a token holder probe
        // sequential ids to learn which environments exist platform-wide, and a
        // calling app has no reason to tell the two apart.
        .map_err(|error| match error {
            FlagError::EnvironmentNotFound { .. } | FlagError::EnvironmentNotInProject { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Environment Not Available")
                    .with_detail(
                        "The requested environment does not exist in this token's project.",
                    )
            }
            other => Problem::from(other),
        })?;

    let etag = snapshot_etag(&flags)?;

    if let Some(requested) = headers.get(header::IF_NONE_MATCH) {
        if requested.as_bytes() == etag.as_bytes() {
            return Ok(StatusCode::NOT_MODIFIED.into_response());
        }
    }

    let mut response_headers = HeaderMap::new();
    match HeaderValue::from_str(&etag) {
        Ok(value) => {
            response_headers.insert(header::ETAG, value);
        }
        Err(e) => {
            // A non-ASCII ETag is impossible (hex), but degrade rather than
            // fail the caller's flag fetch.
            error!("Failed to build ETag header for flag snapshot: {}", e);
        }
    }

    Ok((
        StatusCode::OK,
        response_headers,
        Json(FlagSnapshotResponse {
            environment_id,
            flags,
        }),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::FlagService;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_auth::context::AuthContext;
    use temps_auth::permissions::{Permission, Role};
    use temps_entities::deployment_tokens::DeploymentTokenPermission;
    use temps_entities::users;

    // =====================================================================
    // Handler guards
    // =====================================================================

    /// Minimal `AuditLogger` so handler tests don't need the audit crate's
    /// database and geo-IP dependencies.
    struct NoopAuditLogger;

    #[async_trait::async_trait]
    impl temps_core::AuditLogger for NoopAuditLogger {
        async fn create_audit_log(
            &self,
            _operation: &dyn temps_core::AuditOperation,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn test_user() -> users::Model {
        let now = chrono::Utc::now();
        users::Model {
            id: 1,
            name: "Test User".to_string(),
            email: "test@example.com".to_string(),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
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

    fn test_metadata() -> RequestMetadata {
        RequestMetadata {
            ip_address: "127.0.0.1".to_string(),
            user_agent: "test".to_string(),
            headers: HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "http://localhost".to_string(),
            scheme: "http".to_string(),
            host: "localhost".to_string(),
            is_secure: false,
        }
    }

    fn app_state() -> Arc<FlagsAppState> {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        Arc::new(FlagsAppState {
            flag_service: Arc::new(FlagService::new(db)),
            audit_service: Arc::new(NoopAuditLogger),
            project_access_checker: None,
        })
    }

    fn list_query() -> ListFlagsQuery {
        ListFlagsQuery {
            include_archived: false,
            page: None,
            page_size: None,
        }
    }

    /// The Problem status if the handler rejected the request, or `None` if it
    /// passed the guards and went on to the service.
    async fn list_rejection(auth: AuthContext) -> Option<StatusCode> {
        list_flags(
            RequireAuth(auth),
            State(app_state()),
            Path(1),
            Query(list_query()),
        )
        .await
        .err()
        .map(|p| p.status_code)
    }

    async fn archive_rejection(auth: AuthContext) -> Option<StatusCode> {
        archive_flag(
            RequireAuth(auth),
            State(app_state()),
            Extension(test_metadata()),
            Path((1, "checkout.v2".to_string())),
        )
        .await
        .err()
        .map(|p| p.status_code)
    }

    #[tokio::test]
    async fn reader_cannot_archive_a_flag() {
        assert_eq!(
            archive_rejection(AuthContext::new_session(test_user(), Role::Reader)).await,
            Some(StatusCode::FORBIDDEN),
            "flags:delete must not be implied by a read-only role"
        );
    }

    #[tokio::test]
    async fn reader_can_reach_the_list_handler() {
        assert_ne!(
            list_rejection(AuthContext::new_session(test_user(), Role::Reader)).await,
            Some(StatusCode::FORBIDDEN),
            "Reader holds flags:read and must pass the guard"
        );
    }

    #[tokio::test]
    async fn api_key_scoped_to_read_cannot_write() {
        let read_only = || {
            AuthContext::new_api_key(
                test_user(),
                None,
                Some(vec![Permission::FlagsRead]),
                "flags-read-key".to_string(),
                1,
            )
        };

        assert_ne!(
            list_rejection(read_only()).await,
            Some(StatusCode::FORBIDDEN),
            "a flags:read key must be able to list"
        );
        assert_eq!(
            archive_rejection(read_only()).await,
            Some(StatusCode::FORBIDDEN),
            "a flags:read key must NOT be able to archive"
        );
    }

    /// Regression: a deployment token scoped to project 1 must not be able to
    /// read project 2's flags by putting a different id in the path.
    ///
    /// `project_access_guard!` deliberately skips deployment tokens (they carry
    /// no user identity for a team-membership check), so `project_scope_guard!`
    /// is the *only* thing confining them. Without it this returned 200 and the
    /// other tenant's flag values.
    #[tokio::test]
    async fn deployment_token_cannot_read_another_projects_flags() {
        let foreign_project = AuthContext::new_deployment_token(
            1,
            Some(1),
            None,
            1,
            "app".to_string(),
            vec![DeploymentTokenPermission::FlagsRead],
        );

        let rejection = list_flags(
            RequireAuth(foreign_project),
            State(app_state()),
            Path(2),
            Query(list_query()),
        )
        .await
        .err()
        .map(|p| p.status_code);

        assert_eq!(
            rejection,
            Some(StatusCode::FORBIDDEN),
            "a project-1 token must not list project 2's flags"
        );
    }

    /// Same confinement for the single-flag read.
    #[tokio::test]
    async fn deployment_token_cannot_get_another_projects_flag() {
        let rejection = get_flag(
            RequireAuth(AuthContext::new_deployment_token(
                1,
                Some(1),
                None,
                1,
                "app".to_string(),
                vec![DeploymentTokenPermission::FlagsRead],
            )),
            State(app_state()),
            Path((2, "victim.secret".to_string())),
        )
        .await
        .err()
        .map(|p| p.status_code);

        assert_eq!(rejection, Some(StatusCode::FORBIDDEN));
    }

    /// A full-access deployment token is still a *project-scoped* credential:
    /// the wildcard grants flag reads, not other tenants' data.
    #[tokio::test]
    async fn full_access_deployment_token_is_still_project_confined() {
        let rejection = list_flags(
            RequireAuth(AuthContext::new_deployment_token(
                1,
                Some(1),
                None,
                1,
                "app".to_string(),
                vec![DeploymentTokenPermission::FullAccess],
            )),
            State(app_state()),
            Path(2),
            Query(list_query()),
        )
        .await
        .err()
        .map(|p| p.status_code);

        assert_eq!(rejection, Some(StatusCode::FORBIDDEN));
    }

    /// The token's own project must still work, or the guard is too strict.
    #[tokio::test]
    async fn deployment_token_can_read_its_own_project() {
        let rejection = list_flags(
            RequireAuth(AuthContext::new_deployment_token(
                1,
                Some(1),
                None,
                1,
                "app".to_string(),
                vec![DeploymentTokenPermission::FlagsRead],
            )),
            State(app_state()),
            Path(1),
            Query(list_query()),
        )
        .await
        .err()
        .map(|p| p.status_code);

        assert_ne!(rejection, Some(StatusCode::FORBIDDEN));
    }

    /// A container's baked-in `TEMPS_API_TOKEN` must never be able to change a
    /// production flag, only read one.
    #[tokio::test]
    async fn deployment_token_is_read_only() {
        let token = || {
            AuthContext::new_deployment_token(
                1,
                Some(1),
                None,
                1,
                "app".to_string(),
                vec![DeploymentTokenPermission::FlagsRead],
            )
        };

        assert_eq!(
            archive_rejection(token()).await,
            Some(StatusCode::FORBIDDEN),
            "a deployment token must not archive flags"
        );
    }

    /// The snapshot endpoint takes its scope from the token, never the URL.
    /// A session principal has no project, so it must be turned away rather
    /// than defaulting to something.
    #[tokio::test]
    async fn snapshot_requires_a_deployment_token() {
        let result = get_flag_snapshot(
            RequireAuth(AuthContext::new_session(test_user(), Role::Admin)),
            State(app_state()),
            HeaderMap::new(),
            Query(SnapshotQuery {
                environment_id: Some(1),
            }),
        )
        .await;

        assert_eq!(
            result.err().map(|p| p.status_code),
            Some(StatusCode::BAD_REQUEST)
        );
    }

    /// A project-wide token names no environment, so one must be supplied —
    /// and the service still checks it belongs to the token's project.
    #[tokio::test]
    async fn snapshot_requires_an_environment_for_a_project_wide_token() {
        let result = get_flag_snapshot(
            RequireAuth(AuthContext::new_deployment_token(
                1,
                None,
                None,
                1,
                "app".to_string(),
                vec![DeploymentTokenPermission::FlagsRead],
            )),
            State(app_state()),
            HeaderMap::new(),
            Query(SnapshotQuery {
                environment_id: None,
            }),
        )
        .await;

        assert_eq!(
            result.err().map(|p| p.status_code),
            Some(StatusCode::BAD_REQUEST)
        );
    }

    // =====================================================================
    // ETag
    // =====================================================================

    fn snapshot(key: &str, value: serde_json::Value) -> FlagSnapshot {
        FlagSnapshot {
            key: key.to_string(),
            value_type: FlagValueType::Bool,
            default_value: serde_json::json!(false),
            enabled: true,
            environment_value: Some(value),
        }
    }

    #[test]
    fn etag_is_stable_for_identical_snapshots() {
        let a = vec![snapshot("checkout.v2", serde_json::json!(true))];
        let b = vec![snapshot("checkout.v2", serde_json::json!(true))];

        assert_eq!(snapshot_etag(&a).unwrap(), snapshot_etag(&b).unwrap());
    }

    #[test]
    fn etag_changes_when_a_value_changes() {
        let a = vec![snapshot("checkout.v2", serde_json::json!(true))];
        let b = vec![snapshot("checkout.v2", serde_json::json!(false))];

        assert_ne!(snapshot_etag(&a).unwrap(), snapshot_etag(&b).unwrap());
    }

    #[test]
    fn etag_changes_when_a_flag_is_added() {
        let a = vec![snapshot("checkout.v2", serde_json::json!(true))];
        let b = vec![
            snapshot("checkout.v2", serde_json::json!(true)),
            snapshot("new.search", serde_json::json!(true)),
        ];

        assert_ne!(snapshot_etag(&a).unwrap(), snapshot_etag(&b).unwrap());
    }

    #[test]
    fn etag_is_quoted_and_hex() {
        let etag = snapshot_etag(&[snapshot("a", serde_json::json!(true))]).unwrap();

        assert!(etag.starts_with('"') && etag.ends_with('"'), "{etag}");
        let inner = etag.trim_matches('"');
        assert_eq!(inner.len(), 32);
        assert!(inner.chars().all(|c| c.is_ascii_hexdigit()), "{etag}");
    }
}
