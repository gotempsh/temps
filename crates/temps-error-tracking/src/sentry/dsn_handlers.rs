// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::post,
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_auth::{permission_guard, project_access_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_core::RequestMetadata;
use tracing::error;
use utoipa::{OpenApi, ToSchema};

use crate::handlers::audit::{AuditContext, DsnCreatedAudit, DsnRegeneratedAudit, DsnRevokedAudit};
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
    Extension(metadata): Extension<RequestMetadata>,
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

    record_dsn_created(&state, audit_context(&auth, &metadata), &dsn).await;

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
    Extension(metadata): Extension<RequestMetadata>,
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

    let (dsn, created) = state
        .dsn_service
        .get_or_create_project_dsn_reporting_creation(
            project_id,
            request.environment_id,
            request.deployment_id,
            &base_url,
        )
        .await?;

    // Returning an existing DSN is a read; only minting one is audited.
    if created {
        record_dsn_created(&state, audit_context(&auth, &metadata), &dsn).await;
    }

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
    Extension(metadata): Extension<RequestMetadata>,
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

    let audit = DsnRegeneratedAudit {
        context: audit_context(&auth, &metadata),
        project_id,
        dsn_id: dsn.id,
        environment_id: dsn.environment_id,
        deployment_id: dsn.deployment_id,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(
            project_id,
            dsn_id, "Failed to create DSN regeneration audit log: {}", e
        );
    }

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
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_access_guard!(auth, project_id, state.project_access_checker);

    state.dsn_service.revoke_dsn(dsn_id, project_id).await?;

    let audit = DsnRevokedAudit {
        context: audit_context(&auth, &metadata),
        project_id,
        dsn_id,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(
            project_id,
            dsn_id, "Failed to create DSN revocation audit log: {}", e
        );
    }

    Ok(StatusCode::NO_CONTENT)
}

fn audit_context(auth: &temps_auth::AuthContext, metadata: &RequestMetadata) -> AuditContext {
    AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    }
}

/// Audit the minting of a DSN. Records ids and scope only, never the key.
/// An audit failure is logged and does not fail the request.
async fn record_dsn_created(state: &DSNAppState, context: AuditContext, dsn: &ProjectDSN) {
    let audit = DsnCreatedAudit {
        context,
        project_id: dsn.project_id,
        dsn_id: dsn.id,
        environment_id: dsn.environment_id,
        deployment_id: dsn.deployment_id,
        name: dsn.name.clone(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(
            project_id = dsn.project_id,
            dsn_id = dsn.id,
            "Failed to create DSN creation audit log: {}",
            e
        );
    }
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

    fn user_auth(role: Role) -> RequireAuth {
        RequireAuth(AuthContext::new_session(test_user(1), role))
    }

    fn metadata() -> RequestMetadata {
        RequestMetadata {
            ip_address: "203.0.113.9".to_string(),
            user_agent: "audit-test".to_string(),
            headers: axum::http::HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "https://temps.test".to_string(),
            scheme: "https".to_string(),
            host: "temps.test".to_string(),
            is_secure: true,
        }
    }

    /// Audit logger that records every operation it receives.
    #[derive(Default)]
    struct RecordingAuditLogger {
        entries: std::sync::Mutex<Vec<(String, Option<i32>, serde_json::Value)>>,
    }

    #[async_trait::async_trait]
    impl temps_core::AuditLogger for RecordingAuditLogger {
        async fn create_audit_log(
            &self,
            operation: &dyn temps_core::audit::AuditOperation,
        ) -> anyhow::Result<()> {
            let payload: serde_json::Value = serde_json::from_str(&operation.serialize()?)?;
            self.entries
                .lock()
                .map_err(|_| anyhow::anyhow!("recording audit logger lock poisoned"))?
                .push((operation.operation_type(), operation.user_id(), payload));
            Ok(())
        }
    }

    impl RecordingAuditLogger {
        fn ops(&self) -> Vec<String> {
            self.entries
                .lock()
                .expect("lock")
                .iter()
                .map(|(op, _, _)| op.clone())
                .collect()
        }
    }

    /// Covers the whole DSN credential lifecycle against a real database:
    /// create, get-or-create (audited only when it mints), regenerate, and
    /// revoke each emit exactly one audit event carrying ids and never the
    /// key; a failed operation emits nothing.
    #[tokio::test]
    async fn dsn_lifecycle_emits_audit_events_without_the_key() {
        use sea_orm::{ActiveModelTrait, Set};
        use temps_database::test_utils::{is_container_runtime_unavailable, TestDatabase};

        let test_db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
                eprintln!("Skipping DSN audit test: {error}");
                return;
            }
            Err(error) => panic!("DSN audit test database setup failed: {error}"),
        };
        let db = test_db.connection_arc();
        let now = Utc::now();
        let project = temps_entities::projects::ActiveModel {
            name: Set("DSN Audit Project".to_string()),
            repo_name: Set("repo".to_string()),
            repo_owner: Set("owner".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            slug: Set(format!("dsn-audit-{}", uuid::Uuid::new_v4())),
            preset: Set(temps_entities::preset::Preset::NextJs),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("insert project");
        let project_id = project.id;

        let recorder = Arc::new(RecordingAuditLogger::default());
        let server_config = Arc::new(
            temps_config::ServerConfig::new(
                "127.0.0.1:3000".to_string(),
                "postgres://test:test@localhost/test".to_string(),
                None,
                None,
            )
            .expect("failed to build test ServerConfig"),
        );
        let state = Arc::new(DSNAppState {
            dsn_service: Arc::new(DSNService::new(db.clone())),
            audit_service: recorder.clone(),
            config_service: Arc::new(temps_config::ConfigService::new(server_config, db.clone())),
            project_access_checker: None,
        });
        let base_url = Some("https://temps.test".to_string());

        // create
        let (status, Json(created)) = create_dsn(
            user_auth(Role::Admin),
            State(state.clone()),
            Path(project_id),
            Extension(metadata()),
            Json(CreateDSNRequest {
                environment_id: None,
                deployment_id: None,
                name: Some("CI DSN".to_string()),
                base_url: base_url.clone(),
            }),
        )
        .await
        .expect("create succeeds");
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(recorder.ops(), ["DSN_CREATED"]);

        // get-or-create returns the existing (env=None, deployment=None) DSN:
        // a read, so no new audit event.
        let Json(existing) = get_or_create_dsn(
            user_auth(Role::Admin),
            State(state.clone()),
            Path(project_id),
            Extension(metadata()),
            Json(GetOrCreateDSNRequest {
                environment_id: None,
                deployment_id: None,
                base_url: base_url.clone(),
            }),
        )
        .await
        .expect("get-or-create succeeds");
        assert_eq!(existing.id, created.id);
        assert_eq!(
            recorder.ops(),
            ["DSN_CREATED"],
            "returning an existing DSN is not audited"
        );

        // regenerate
        let Json(rotated) = regenerate_dsn(
            user_auth(Role::Admin),
            State(state.clone()),
            Path((project_id, created.id)),
            Extension(metadata()),
            Json(RegenerateDSNRequest {
                base_url: base_url.clone(),
            }),
        )
        .await
        .expect("regenerate succeeds");
        assert_ne!(rotated.public_key, created.public_key);

        // revoke
        let status = revoke_dsn(
            user_auth(Role::Admin),
            State(state.clone()),
            Path((project_id, created.id)),
            Extension(metadata()),
        )
        .await
        .expect("revoke succeeds");
        assert_eq!(status, StatusCode::NO_CONTENT);

        // get-or-create after revocation mints a fresh DSN: audited.
        let Json(minted) = get_or_create_dsn(
            user_auth(Role::Admin),
            State(state.clone()),
            Path(project_id),
            Extension(metadata()),
            Json(GetOrCreateDSNRequest {
                environment_id: None,
                deployment_id: None,
                base_url,
            }),
        )
        .await
        .expect("get-or-create after revoke succeeds");
        assert_ne!(minted.id, created.id);

        // Revoking a DSN of another project fails and is not audited.
        let missing = revoke_dsn(
            user_auth(Role::Admin),
            State(state.clone()),
            Path((project_id + 10_000, created.id)),
            Extension(metadata()),
        )
        .await;
        assert!(missing.is_err(), "cross-project revoke must fail");

        let entries = recorder.entries.lock().expect("lock");
        let ops: Vec<&str> = entries.iter().map(|(op, _, _)| op.as_str()).collect();
        assert_eq!(
            ops,
            [
                "DSN_CREATED",
                "DSN_REGENERATED",
                "DSN_REVOKED",
                "DSN_CREATED"
            ]
        );
        for (op, user_id, payload) in entries.iter() {
            assert_eq!(*user_id, Some(1), "{op} records the acting user");
            assert_eq!(
                payload["project_id"], project_id,
                "{op} records the project"
            );
            assert!(payload["dsn_id"].is_i64(), "{op} records the DSN id");
            assert_eq!(payload["context"]["ip_address"], "203.0.113.9");
            let raw = payload.to_string();
            for key in [&created.public_key, &rotated.public_key, &minted.public_key] {
                assert!(!raw.contains(key.as_str()), "{op} audit leaked a DSN key");
            }
            assert!(payload.get("public_key").is_none());
            assert!(payload.get("dsn").is_none());
        }
        assert_eq!(entries[0].2["dsn_id"], created.id);
        assert_eq!(entries[0].2["name"], "CI DSN");
        assert_eq!(entries[1].2["dsn_id"], created.id);
        assert_eq!(entries[2].2["dsn_id"], created.id);
        assert_eq!(entries[3].2["dsn_id"], minted.id);
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
            Extension(metadata()),
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
            Extension(metadata()),
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
            Extension(metadata()),
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
