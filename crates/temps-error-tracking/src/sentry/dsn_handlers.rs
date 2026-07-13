use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::post,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_auth::{permission_guard, project_access_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use tracing::error;
use utoipa::{OpenApi, ToSchema};

use crate::sentry::{DSNService, ProjectDSN, SentryIngesterError};

#[derive(OpenApi)]
#[openapi(
    paths(
        create_dsn,
        get_or_create_dsn,
        list_dsns,
        regenerate_dsn,
        revoke_dsn,
    ),
    components(schemas(
        CreateDSNRequest,
        GetOrCreateDSNRequest,
        ProjectDSNResponse,
        RegenerateDSNRequest,
    )),
    tags(
        (name = "dsn", description = "DSN management endpoints")
    )
)]
pub struct DSNApiDoc;

#[derive(Clone)]
pub struct DSNAppState {
    pub dsn_service: Arc<DSNService>,
    pub audit_service: Arc<dyn temps_core::AuditLogger>,
    pub config_service: Arc<temps_config::ConfigService>,
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

pub fn configure_dsn_routes() -> Router<Arc<DSNAppState>> {
    Router::new()
        .route(
            "/projects/{project_id}/dsns",
            post(create_dsn).get(list_dsns),
        )
        .route(
            "/projects/{project_id}/dsns/get-or-create",
            post(get_or_create_dsn),
        )
        .route(
            "/projects/{project_id}/dsns/{dsn_id}/regenerate",
            post(regenerate_dsn),
        )
        .route(
            "/projects/{project_id}/dsns/{dsn_id}/revoke",
            post(revoke_dsn),
        )
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreateDSNRequest {
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub name: Option<String>,
    pub base_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct GetOrCreateDSNRequest {
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub base_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RegenerateDSNRequest {
    pub base_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ProjectDSNResponse {
    pub id: i32,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub name: String,
    pub public_key: String,
    pub dsn: String,
    pub created_at: String,
    pub is_active: bool,
    pub event_count: i64,
}

impl From<ProjectDSN> for ProjectDSNResponse {
    fn from(dsn: ProjectDSN) -> Self {
        Self {
            id: dsn.id,
            project_id: dsn.project_id,
            environment_id: dsn.environment_id,
            deployment_id: dsn.deployment_id,
            name: dsn.name,
            public_key: dsn.public_key,
            dsn: dsn.dsn,
            created_at: dsn.created_at.to_string(),
            is_active: dsn.is_active,
            event_count: dsn.event_count,
        }
    }
}

impl From<SentryIngesterError> for Problem {
    fn from(error: SentryIngesterError) -> Self {
        match error {
            SentryIngesterError::ProjectNotFound => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Project Not Found")
                .with_detail("The requested project does not exist"),
            SentryIngesterError::InvalidDSN => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("DSN Not Found")
                .with_detail("The requested DSN does not exist"),
            SentryIngesterError::Validation(msg) => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Validation Error")
                .with_detail(msg),
            SentryIngesterError::Database(e) => {
                error!("DSN database error: {}", e);
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Database Error")
                    .with_detail("An internal error occurred")
            }
        }
    }
}

/// Create a new DSN for a project
#[utoipa::path(
    post,
    path = "/projects/{project_id}/dsns",
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    request_body = CreateDSNRequest,
    responses(
        (status = 201, description = "DSN created", body = ProjectDSNResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Project not found"),
    ),
    security(("bearer_auth" = []))
)]
async fn create_dsn(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DSNAppState>>,
    Path(project_id): Path<i32>,
    Json(request): Json<CreateDSNRequest>,
) -> Result<(StatusCode, Json<ProjectDSNResponse>), Problem> {
    permission_guard!(auth, ErrorTrackingCreate);
    project_access_guard!(auth, project_id, state.project_access_checker);

    // Get base URL from config service if not provided (defaults to http://localho.st)
    let base_url = match request.base_url {
        Some(url) => url,
        None => state
            .config_service
            .get_external_url_or_default()
            .await
            .map_err(|e| SentryIngesterError::Validation(format!("Config error: {}", e)))?,
    };

    let dsn = state
        .dsn_service
        .create_project_dsn(
            project_id,
            request.environment_id,
            request.deployment_id,
            request.name,
            &base_url,
        )
        .await?;

    Ok((StatusCode::CREATED, Json(dsn.into())))
}

/// Get or create DSN for a project/environment/deployment combination
#[utoipa::path(
    post,
    path = "/projects/{project_id}/dsns/get-or-create",
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    request_body = GetOrCreateDSNRequest,
    responses(
        (status = 200, description = "DSN retrieved or created", body = ProjectDSNResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Project not found"),
    ),
    security(("bearer_auth" = []))
)]
async fn get_or_create_dsn(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DSNAppState>>,
    Path(project_id): Path<i32>,
    Json(request): Json<GetOrCreateDSNRequest>,
) -> Result<Json<ProjectDSNResponse>, Problem> {
    permission_guard!(auth, ErrorTrackingCreate);
    project_access_guard!(auth, project_id, state.project_access_checker);

    // Get base URL from config service if not provided (defaults to http://localho.st)
    let base_url = match request.base_url {
        Some(url) => url,
        None => state
            .config_service
            .get_external_url_or_default()
            .await
            .map_err(|e| SentryIngesterError::Validation(format!("Config error: {}", e)))?,
    };

    let dsn = state
        .dsn_service
        .get_or_create_project_dsn(
            project_id,
            request.environment_id,
            request.deployment_id,
            &base_url,
        )
        .await?;

    Ok(Json(dsn.into()))
}

/// List all DSNs for a project
#[utoipa::path(
    get,
    path = "/projects/{project_id}/dsns",
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    responses(
        (status = 200, description = "List of DSNs", body = Vec<ProjectDSNResponse>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
async fn list_dsns(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DSNAppState>>,
    Path(project_id): Path<i32>,
) -> Result<Json<Vec<ProjectDSNResponse>>, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_access_guard!(auth, project_id, state.project_access_checker);

    // Get base URL from config service (with default fallback to http://localho.st)
    let base_url = state
        .config_service
        .get_external_url_or_default()
        .await
        .map_err(|e| SentryIngesterError::Validation(format!("Config error: {}", e)))?;

    let dsns = state
        .dsn_service
        .list_project_dsns(project_id, &base_url)
        .await?;

    Ok(Json(dsns.into_iter().map(|d| d.into()).collect()))
}

/// Regenerate DSN keys (rotate keys)
#[utoipa::path(
    post,
    path = "/projects/{project_id}/dsns/{dsn_id}/regenerate",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("dsn_id" = i32, Path, description = "DSN ID")
    ),
    request_body = RegenerateDSNRequest,
    responses(
        (status = 200, description = "DSN keys regenerated", body = ProjectDSNResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "DSN not found"),
    ),
    security(("bearer_auth" = []))
)]
async fn regenerate_dsn(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DSNAppState>>,
    Path((project_id, dsn_id)): Path<(i32, i32)>,
    Json(request): Json<RegenerateDSNRequest>,
) -> Result<Json<ProjectDSNResponse>, Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_access_guard!(auth, project_id, state.project_access_checker);

    // Get base URL from config service if not provided (defaults to http://localho.st)
    let base_url = match request.base_url {
        Some(url) => url,
        None => state
            .config_service
            .get_external_url_or_default()
            .await
            .map_err(|e| SentryIngesterError::Validation(format!("Config error: {}", e)))?,
    };

    let dsn = state
        .dsn_service
        .regenerate_project_dsn(dsn_id, project_id, &base_url)
        .await?;

    Ok(Json(dsn.into()))
}

/// Revoke (deactivate) a DSN
#[utoipa::path(
    post,
    path = "/projects/{project_id}/dsns/{dsn_id}/revoke",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("dsn_id" = i32, Path, description = "DSN ID")
    ),
    responses(
        (status = 204, description = "DSN revoked"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "DSN not found"),
    ),
    security(("bearer_auth" = []))
)]
async fn revoke_dsn(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<DSNAppState>>,
    Path((project_id, dsn_id)): Path<(i32, i32)>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_access_guard!(auth, project_id, state.project_access_checker);

    state.dsn_service.revoke_dsn(dsn_id, project_id).await?;

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::Utc;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_auth::context::AuthContext;
    use temps_auth::permissions::Role;
    use temps_entities::users;

    // Regression tests for the unauthenticated-access finding: every DSN
    // management handler (create/get-or-create/list/regenerate/revoke) had
    // no `RequireAuth` extractor at all, so any caller who knew (or
    // guessed) an integer `project_id` could enumerate, rotate, or revoke
    // that project's Sentry-compatible DSN with zero credentials.

    struct NoopAuditLogger;

    #[async_trait::async_trait]
    impl temps_core::AuditLogger for NoopAuditLogger {
        async fn create_audit_log(
            &self,
            _operation: &dyn temps_core::audit::AuditOperation,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn test_state() -> Arc<DSNAppState> {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let server_config = Arc::new(
            temps_config::ServerConfig::new(
                "127.0.0.1:3000".to_string(),
                "postgres://test:test@localhost/test".to_string(),
                None,
                None,
            )
            .expect("failed to build test ServerConfig"),
        );
        Arc::new(DSNAppState {
            dsn_service: Arc::new(DSNService::new(db.clone())),
            audit_service: Arc::new(NoopAuditLogger),
            config_service: Arc::new(temps_config::ConfigService::new(server_config, db)),
            project_access_checker: None,
        })
    }

    fn test_user(id: i32) -> users::Model {
        let now = Utc::now();
        users::Model {
            id,
            name: "Test User".to_string(),
            email: format!("user{id}@example.com"),
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

    fn user_auth(role: Role) -> RequireAuth {
        RequireAuth(AuthContext::new_session(test_user(1), role))
    }

    #[tokio::test]
    async fn list_dsns_rejects_reader_without_error_tracking_permission() {
        // `Role::ApiReader` holds no ErrorTracking* permissions, so this must
        // fail the `permission_guard!` check before ever touching the DB.
        let err = list_dsns(user_auth(Role::ApiReader), State(test_state()), Path(1))
            .await
            .expect_err("an ApiReader must not be able to list DSNs");
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn revoke_dsn_rejects_reader_without_error_tracking_permission() {
        let err = revoke_dsn(
            user_auth(Role::ApiReader),
            State(test_state()),
            Path((1, 1)),
        )
        .await
        .expect_err("an ApiReader must not be able to revoke a DSN");
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn regenerate_dsn_rejects_reader_without_error_tracking_permission() {
        let err = regenerate_dsn(
            user_auth(Role::ApiReader),
            State(test_state()),
            Path((1, 1)),
            Json(RegenerateDSNRequest { base_url: None }),
        )
        .await
        .expect_err("an ApiReader must not be able to regenerate a DSN");
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn create_dsn_rejects_reader_without_error_tracking_permission() {
        let err = create_dsn(
            user_auth(Role::ApiReader),
            State(test_state()),
            Path(1),
            Json(CreateDSNRequest {
                environment_id: None,
                deployment_id: None,
                name: None,
                base_url: None,
            }),
        )
        .await
        .expect_err("an ApiReader must not be able to create a DSN");
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
    }
}
