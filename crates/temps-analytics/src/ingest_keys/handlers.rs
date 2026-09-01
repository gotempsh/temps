// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Admin CRUD for analytics ingest keys (ADR-040 §5).
//!
//! Mounted on the admin router, so every path below is served under `/api`.
//! Reuses `Permission::AnalyticsRead` / `Permission::AnalyticsWrite` rather
//! than introducing a new permission variant, mirroring how DSN CRUD reuses
//! `ErrorTracking*`. Because `AnalyticsWrite` otherwise only gates data
//! mutation while these endpoints mint and destroy a long-lived credential,
//! **every write here is audited** — that is the agreed mitigation, not an
//! optional extra.
//!
//! There is intentionally no `DELETE`: revocation is soft so the record of
//! which key ingested what survives.
//!
//! # Deployment tokens are denied outright
//!
//! Every handler here pairs its `permission_guard!` with
//! `deny_deployment_token!`. `Permission::AnalyticsWrite` maps to the
//! `VisitorsEnrich` deployment-token permission
//! (`crates/temps-auth/src/context.rs`), so without that second guard a `dt_`
//! token — a secret that lives in a deployed app's runtime environment, and is
//! therefore reachable from app code, a compromised dependency, SSRF or a
//! leaked log — could mint, rotate and revoke its own project's ingest keys.
//! Worse, a deployment token has no user behind it: `auth.user_id()` is `0`,
//! no such row exists in `users`, so the audit insert would fail its foreign
//! key and be swallowed by the "audit failures don't fail the request"
//! handling below. That would leave this capability with *no* audit trail at
//! all — destroying the very mitigation that justifies gating credential
//! minting behind `AnalyticsWrite` in the first place.
//!
//! There is also intentionally no capability/`configured: false` endpoint.
//! This feature depends on no operator configuration — you just mint a key —
//! so an empty `GET` list is a sufficient and honest empty state.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{patch, post},
    Extension, Json, Router,
};
use temps_auth::{
    deny_deployment_token, permission_guard, project_access_guard, project_scope_guard, RequireAuth,
};
use temps_core::problemdetails::{self, Problem};
use temps_core::RequestMetadata;
use temps_core::{AuditContext, AuditLogger};
use tracing::{error, warn};
use utoipa::OpenApi;

use super::audit::{
    AnalyticsIngestKeyCreatedAudit, AnalyticsIngestKeyRevokedAudit, AnalyticsIngestKeyRotatedAudit,
    AnalyticsIngestKeyUpdatedAudit,
};
use super::rate_limiter::AnalyticsIngestRateLimiter;
use super::service::AnalyticsIngestKeyService;
use super::types::{
    AnalyticsIngestKey, AnalyticsIngestKeyError, CreateAnalyticsIngestKeyRequest,
    UpdateAnalyticsIngestKeyRequest,
};

/// State for the ingest-key admin routes.
///
/// Each dependency is held directly as its own `Arc` clone rather than reached
/// through an accessor on another service.
#[derive(Clone)]
pub struct AnalyticsIngestKeysAppState {
    pub ingest_key_service: Arc<AnalyticsIngestKeyService>,
    /// The *same* limiter instance the public ingest handlers use, so revoking
    /// a key here actually releases the bucket ingest was filling.
    pub rate_limiter: Arc<AnalyticsIngestRateLimiter>,
    pub audit_service: Arc<dyn AuditLogger>,
    /// ADR-028 team-based project access checker (human sessions only).
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

#[derive(OpenApi)]
#[openapi(
    paths(
        create_analytics_ingest_key,
        list_analytics_ingest_keys,
        update_analytics_ingest_key,
        rotate_analytics_ingest_key,
        revoke_analytics_ingest_key,
    ),
    components(schemas(
        AnalyticsIngestKey,
        CreateAnalyticsIngestKeyRequest,
        UpdateAnalyticsIngestKeyRequest,
    )),
    tags(
        (name = "Analytics Ingest Keys", description = "Manage non-secret analytics ingest keys for apps Temps does not deploy")
    )
)]
pub struct AnalyticsIngestKeyApiDoc;

pub fn configure_ingest_key_routes() -> Router<Arc<AnalyticsIngestKeysAppState>> {
    Router::new()
        .route(
            "/projects/{project_id}/analytics/ingest-keys",
            post(create_analytics_ingest_key).get(list_analytics_ingest_keys),
        )
        .route(
            "/projects/{project_id}/analytics/ingest-keys/{key_id}",
            patch(update_analytics_ingest_key),
        )
        .route(
            "/projects/{project_id}/analytics/ingest-keys/{key_id}/rotate",
            post(rotate_analytics_ingest_key),
        )
        .route(
            "/projects/{project_id}/analytics/ingest-keys/{key_id}/revoke",
            post(revoke_analytics_ingest_key),
        )
}

impl From<AnalyticsIngestKeyError> for Problem {
    fn from(error: AnalyticsIngestKeyError) -> Self {
        match error {
            AnalyticsIngestKeyError::KeyNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Analytics Ingest Key Not Found")
                    .with_detail(error.to_string())
            }
            AnalyticsIngestKeyError::ProjectNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Project Not Found")
                    .with_detail(error.to_string())
            }
            AnalyticsIngestKeyError::EnvironmentNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Environment Not Found")
                    .with_detail(error.to_string())
            }
            // Deliberately answered as a plain "not found in this project":
            // telling the caller which *other* project owns the environment
            // would leak cross-tenant structure. The full detail, including
            // the owning project, stays in the server log.
            AnalyticsIngestKeyError::EnvironmentProjectMismatch {
                environment_id,
                environment_project_id,
                project_id,
            } => {
                warn!(
                    environment_id,
                    environment_project_id,
                    project_id,
                    "refused to scope an analytics ingest key to an environment in another project"
                );
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Environment Not Found")
                    .with_detail(format!(
                        "Environment {environment_id} does not belong to project {project_id}"
                    ))
            }
            AnalyticsIngestKeyError::Validation { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Validation Error")
                    .with_detail(error.to_string())
            }
            AnalyticsIngestKeyError::MalformedAllowedOrigins { .. } => {
                error!("{error}");
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
            AnalyticsIngestKeyError::Database(ref e) => {
                error!("Analytics ingest key database error: {e}");
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Database Error")
                    .with_detail("An internal error occurred")
            }
        }
    }
}

fn audit_context(
    auth: &temps_auth::context::AuthContext,
    metadata: &RequestMetadata,
) -> AuditContext {
    AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    }
}

/// Mint a new analytics ingest key.
///
/// The returned `public_key` is **not a secret**: it is designed to be embedded
/// in client-side JavaScript and sent as `X-Temps-Analytics-Key` or
/// `?temps_key=`. It is returned in full here and on every subsequent read, so
/// an operator can copy it at any time; there is no "reveal" step because there
/// is nothing to conceal.
#[utoipa::path(
    tag = "Analytics Ingest Keys",
    post,
    path = "/projects/{project_id}/analytics/ingest-keys",
    operation_id = "create_analytics_ingest_key",
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    request_body = CreateAnalyticsIngestKeyRequest,
    responses(
        (status = 201, description = "Ingest key created", body = AnalyticsIngestKey),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Project or environment not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn create_analytics_ingest_key(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AnalyticsIngestKeysAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(request): Json<CreateAnalyticsIngestKeyRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsWrite);
    // A project-bound deployment token has no legitimate reason to mint, list,
    // or manage ingest credentials for its own project — require a real user or
    // API-key session. See the module docs for why `permission_guard!` alone
    // does not cover this.
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let key = state
        .ingest_key_service
        .create(
            project_id,
            request.environment_id,
            request.name,
            request.allowed_origins,
            request.rate_limit_per_minute,
            auth.user_id_opt(),
        )
        .await?;

    let audit = AnalyticsIngestKeyCreatedAudit {
        context: audit_context(&auth, &metadata),
        project_id,
        environment_id: key.environment_id,
        key_id: key.id,
        name: key.name.clone(),
        public_key: key.public_key.clone(),
        allowed_origins: key.allowed_origins.clone(),
        rate_limit_per_minute: key.rate_limit_per_minute,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create analytics ingest key audit log: {e}");
    }

    Ok((StatusCode::CREATED, Json(key)))
}

/// List a project's analytics ingest keys, newest first.
///
/// Includes revoked keys: revocation is soft, and an operator investigating
/// "which key sent this?" needs to see the rows that no longer work. Every
/// `public_key` is returned unmasked — see [`create_analytics_ingest_key`].
#[utoipa::path(
    tag = "Analytics Ingest Keys",
    get,
    path = "/projects/{project_id}/analytics/ingest-keys",
    operation_id = "list_analytics_ingest_keys",
    params(
        ("project_id" = i32, Path, description = "Project ID")
    ),
    responses(
        (status = 200, description = "Ingest keys, active and revoked, newest first", body = Vec<AnalyticsIngestKey>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn list_analytics_ingest_keys(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AnalyticsIngestKeysAppState>>,
    Path(project_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsRead);
    // A project-bound deployment token has no legitimate reason to mint, list,
    // or manage ingest credentials for its own project — require a real user or
    // API-key session. See the module docs for why `permission_guard!` alone
    // does not cover this.
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let keys = state.ingest_key_service.list(project_id).await?;

    Ok((StatusCode::OK, Json(keys)))
}

/// Update an ingest key's label, origin allowlist, or rate limit.
///
/// `allowed_origins` and `rate_limit_per_minute` are three-state: omit to leave
/// unchanged, send `null` to clear, send a value to replace. Clearing
/// `allowed_origins` allows any origin; clearing `rate_limit_per_minute`
/// removes the limit.
#[utoipa::path(
    tag = "Analytics Ingest Keys",
    patch,
    path = "/projects/{project_id}/analytics/ingest-keys/{key_id}",
    operation_id = "update_analytics_ingest_key",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("key_id" = i32, Path, description = "Analytics ingest key ID")
    ),
    request_body = UpdateAnalyticsIngestKeyRequest,
    responses(
        (status = 200, description = "Ingest key updated", body = AnalyticsIngestKey),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Ingest key not found in this project"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn update_analytics_ingest_key(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AnalyticsIngestKeysAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, key_id)): Path<(i32, i32)>,
    Json(request): Json<UpdateAnalyticsIngestKeyRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsWrite);
    // A project-bound deployment token has no legitimate reason to mint, list,
    // or manage ingest credentials for its own project — require a real user or
    // API-key session. See the module docs for why `permission_guard!` alone
    // does not cover this.
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let key = state
        .ingest_key_service
        .update(
            project_id,
            key_id,
            // The column is NOT NULL, so the request DTO offers no "clear"
            // state; an omitted name leaves the label untouched.
            request.name.map(Some),
            request.allowed_origins,
            request.rate_limit_per_minute,
        )
        .await?;

    let audit = AnalyticsIngestKeyUpdatedAudit {
        context: audit_context(&auth, &metadata),
        project_id,
        key_id: key.id,
        name: key.name.clone(),
        allowed_origins: key.allowed_origins.clone(),
        rate_limit_per_minute: key.rate_limit_per_minute,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create analytics ingest key audit log: {e}");
    }

    Ok((StatusCode::OK, Json(key)))
}

/// Replace an ingest key's value, keeping the same row and scope.
///
/// The previous value stops working immediately — the resolution cache entry
/// for it is evicted synchronously rather than left to expire. Any client still
/// sending the old value will start receiving 401s, so roll out the new value
/// before rotating.
#[utoipa::path(
    tag = "Analytics Ingest Keys",
    post,
    path = "/projects/{project_id}/analytics/ingest-keys/{key_id}/rotate",
    operation_id = "rotate_analytics_ingest_key",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("key_id" = i32, Path, description = "Analytics ingest key ID")
    ),
    responses(
        (status = 200, description = "Ingest key rotated", body = AnalyticsIngestKey),
        (status = 400, description = "The key is revoked and cannot be rotated"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Ingest key not found in this project"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn rotate_analytics_ingest_key(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AnalyticsIngestKeysAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, key_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsWrite);
    // A project-bound deployment token has no legitimate reason to mint, list,
    // or manage ingest credentials for its own project — require a real user or
    // API-key session. See the module docs for why `permission_guard!` alone
    // does not cover this.
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    // Read the outgoing value first so the audit record names the key that
    // stopped working, not only the one that replaced it.
    let previous_public_key = state
        .ingest_key_service
        .get(project_id, key_id)
        .await?
        .public_key;

    let key = state
        .ingest_key_service
        .rotate(project_id, key_id, auth.user_id_opt())
        .await?;

    let audit = AnalyticsIngestKeyRotatedAudit {
        context: audit_context(&auth, &metadata),
        project_id,
        key_id: key.id,
        previous_public_key,
        public_key: key.public_key.clone(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create analytics ingest key audit log: {e}");
    }

    Ok((StatusCode::OK, Json(key)))
}

/// Revoke an ingest key.
///
/// Soft by design: the row is kept with `is_active = false` and `revoked_at`
/// set, so the record of which key ingested what survives. The key stops
/// resolving immediately.
#[utoipa::path(
    tag = "Analytics Ingest Keys",
    post,
    path = "/projects/{project_id}/analytics/ingest-keys/{key_id}/revoke",
    operation_id = "revoke_analytics_ingest_key",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("key_id" = i32, Path, description = "Analytics ingest key ID")
    ),
    responses(
        (status = 204, description = "Ingest key revoked"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Ingest key not found in this project"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn revoke_analytics_ingest_key(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AnalyticsIngestKeysAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, key_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, AnalyticsWrite);
    // A project-bound deployment token has no legitimate reason to mint, list,
    // or manage ingest credentials for its own project — require a real user or
    // API-key session. See the module docs for why `permission_guard!` alone
    // does not cover this.
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let key = state.ingest_key_service.revoke(project_id, key_id).await?;

    // Release the key's rate-limit window. The service already evicts the
    // resolution cache, but the limiter keeps its own per-key bucket in a
    // `HashMap` that nothing else ever prunes — without this, every revoked key
    // leaks one entry for the lifetime of the process.
    //
    // Deliberately *not* done on rotate: rotation keeps the same `key_id`, so
    // it must keep the same window. Clearing it there would turn "rotate" into
    // a free rate-limit reset.
    state.rate_limiter.forget(key.id).await;

    let audit = AnalyticsIngestKeyRevokedAudit {
        context: audit_context(&auth, &metadata),
        project_id,
        key_id: key.id,
        public_key: key.public_key.clone(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create analytics ingest key audit log: {e}");
    }

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::ingest_keys::test_fixtures::{key_model, project_model, user_model};
    use axum::body::to_bytes;
    use axum::http::HeaderMap;
    use chrono::Utc;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::sync::Mutex;
    use temps_auth::context::AuthContext;
    use temps_auth::permissions::{Permission, Role};
    use temps_entities::analytics_ingest_keys;
    use temps_entities::deployment_tokens::DeploymentTokenPermission;

    #[derive(Default)]
    struct RecordingAuditLogger {
        operations: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl AuditLogger for RecordingAuditLogger {
        async fn create_audit_log(
            &self,
            operation: &dyn temps_core::audit::AuditOperation,
        ) -> anyhow::Result<()> {
            let mut operations = match self.operations.lock() {
                Ok(operations) => operations,
                Err(poisoned) => poisoned.into_inner(),
            };
            operations.push(operation.operation_type());
            Ok(())
        }
    }

    impl RecordingAuditLogger {
        fn recorded(&self) -> Vec<String> {
            match self.operations.lock() {
                Ok(operations) => operations.clone(),
                Err(poisoned) => poisoned.into_inner().clone(),
            }
        }
    }

    fn state_with(
        db: MockDatabase,
    ) -> (Arc<AnalyticsIngestKeysAppState>, Arc<RecordingAuditLogger>) {
        let audit = Arc::new(RecordingAuditLogger::default());
        let state = Arc::new(AnalyticsIngestKeysAppState {
            ingest_key_service: Arc::new(AnalyticsIngestKeyService::new(Arc::new(
                db.into_connection(),
            ))),
            rate_limiter: Arc::new(AnalyticsIngestRateLimiter::new()),
            audit_service: audit.clone(),
            project_access_checker: None,
        });
        (state, audit)
    }

    fn empty_state() -> Arc<AnalyticsIngestKeysAppState> {
        state_with(MockDatabase::new(DatabaseBackend::Postgres)).0
    }

    fn user_auth(role: Role) -> RequireAuth {
        RequireAuth(AuthContext::new_session(user_model(1), role))
    }

    /// A deployment token bound to project 1 with `FullAccess` — the strongest
    /// `dt_` credential there is. `FullAccess` grants `VisitorsEnrich`, which
    /// `Permission::AnalyticsWrite` maps onto, so `permission_guard!` alone
    /// would let this through; only `deny_deployment_token!` stops it. The
    /// project id matches the path in every test below so `project_scope_guard!`
    /// cannot be what produces the 403 either.
    fn deployment_token_auth() -> RequireAuth {
        RequireAuth(AuthContext::new_deployment_token(
            1,
            None,
            None,
            1,
            "test-deployment-token".to_string(),
            vec![DeploymentTokenPermission::FullAccess],
        ))
    }

    /// Assert a 403 that specifically names the deployment-token denial, not
    /// merely "some 403" — a scope or permission failure would be a different
    /// bug wearing the same status code.
    fn assert_deployment_token_denied(problem: Problem) {
        assert_eq!(problem.status_code, StatusCode::FORBIDDEN);
        let rendered = serde_json::to_string(&problem.body).expect("problem body should serialize");
        assert!(
            rendered.contains("deployment-token-not-allowed"),
            "expected a deployment-token denial, got: {rendered}"
        );
    }

    fn metadata() -> Extension<RequestMetadata> {
        Extension(RequestMetadata {
            ip_address: "203.0.113.7".to_string(),
            user_agent: "test-agent".to_string(),
            headers: HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "https://temps.example.com".to_string(),
            scheme: "https".to_string(),
            host: "temps.example.com".to_string(),
            is_secure: true,
        })
    }

    async fn body_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should be readable");
        serde_json::from_slice(&bytes).expect("response body should be JSON")
    }

    // ── Permission enforcement ───────────────────────────────────────────

    #[tokio::test]
    async fn create_rejects_a_reader_without_analytics_write() {
        let err = create_analytics_ingest_key(
            user_auth(Role::ApiReader),
            State(empty_state()),
            metadata(),
            Path(1),
            Json(CreateAnalyticsIngestKeyRequest::default()),
        )
        .await
        .err()
        .expect("an ApiReader must not be able to mint an ingest key");
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn update_rejects_a_reader_without_analytics_write() {
        let err = update_analytics_ingest_key(
            user_auth(Role::ApiReader),
            State(empty_state()),
            metadata(),
            Path((1, 2)),
            Json(UpdateAnalyticsIngestKeyRequest::default()),
        )
        .await
        .err()
        .expect("an ApiReader must not be able to update an ingest key");
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn rotate_rejects_a_reader_without_analytics_write() {
        let err = rotate_analytics_ingest_key(
            user_auth(Role::ApiReader),
            State(empty_state()),
            metadata(),
            Path((1, 2)),
        )
        .await
        .err()
        .expect("an ApiReader must not be able to rotate an ingest key");
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn revoke_rejects_a_reader_without_analytics_write() {
        let err = revoke_analytics_ingest_key(
            user_auth(Role::ApiReader),
            State(empty_state()),
            metadata(),
            Path((1, 2)),
        )
        .await
        .err()
        .expect("an ApiReader must not be able to revoke an ingest key");
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn list_rejects_a_role_without_analytics_read() {
        // `Role::MetricsIngest` holds no permissions at all, so the guard must
        // reject it before any database access (the state has no query results
        // appended, so a DB hit would surface as a different failure).
        assert!(!Role::MetricsIngest.has_permission(&Permission::AnalyticsRead));

        let err = list_analytics_ingest_keys(
            user_auth(Role::MetricsIngest),
            State(empty_state()),
            Path(1),
        )
        .await
        .err()
        .expect("a role without AnalyticsRead must not list ingest keys");
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
    }

    // ── Deployment tokens are denied outright ────────────────────────────
    //
    // A `dt_` token lives in a deployed app's runtime environment, so it is
    // reachable from app code, a compromised dependency, SSRF or a leaked log.
    // It must not be able to mint, read, rotate or revoke the ingest
    // credentials of the very project it is bound to — and because it has no
    // user behind it (`auth.user_id()` is 0), letting it through would also
    // FK-fail the audit insert and leave the action entirely untraced.

    #[tokio::test]
    async fn create_rejects_a_deployment_token() {
        let (state, audit) = state_with(MockDatabase::new(DatabaseBackend::Postgres));

        let err = create_analytics_ingest_key(
            deployment_token_auth(),
            State(state),
            metadata(),
            Path(1),
            Json(CreateAnalyticsIngestKeyRequest::default()),
        )
        .await
        .err()
        .expect("a deployment token must not be able to mint an ingest key");
        assert_deployment_token_denied(err);
        assert!(
            audit.recorded().is_empty(),
            "nothing happened, so nothing may be audited"
        );
    }

    #[tokio::test]
    async fn list_rejects_a_deployment_token() {
        let err =
            list_analytics_ingest_keys(deployment_token_auth(), State(empty_state()), Path(1))
                .await
                .err()
                .expect("a deployment token must not be able to list ingest keys");
        assert_deployment_token_denied(err);
    }

    #[tokio::test]
    async fn update_rejects_a_deployment_token() {
        let err = update_analytics_ingest_key(
            deployment_token_auth(),
            State(empty_state()),
            metadata(),
            Path((1, 2)),
            Json(UpdateAnalyticsIngestKeyRequest::default()),
        )
        .await
        .err()
        .expect("a deployment token must not be able to update an ingest key");
        assert_deployment_token_denied(err);
    }

    #[tokio::test]
    async fn rotate_rejects_a_deployment_token() {
        let err = rotate_analytics_ingest_key(
            deployment_token_auth(),
            State(empty_state()),
            metadata(),
            Path((1, 2)),
        )
        .await
        .err()
        .expect("a deployment token must not be able to rotate an ingest key");
        assert_deployment_token_denied(err);
    }

    #[tokio::test]
    async fn revoke_rejects_a_deployment_token() {
        let err = revoke_analytics_ingest_key(
            deployment_token_auth(),
            State(empty_state()),
            metadata(),
            Path((1, 2)),
        )
        .await
        .err()
        .expect("a deployment token must not be able to revoke an ingest key");
        assert_deployment_token_denied(err);
    }

    /// The mapping that makes the guard necessary: `AnalyticsWrite` is
    /// satisfied by a deployment token, so `permission_guard!` cannot be the
    /// thing keeping it out.
    #[test]
    fn analytics_write_is_reachable_by_a_deployment_token() {
        let RequireAuth(auth) = deployment_token_auth();
        assert!(
            auth.has_permission(&Permission::AnalyticsWrite),
            "if this ever becomes false the deny guard is still correct, but the \
             comments explaining why it exists need updating"
        );
        assert!(auth.has_permission(&Permission::AnalyticsRead));
    }

    // ── Project scoping ──────────────────────────────────────────────────

    #[tokio::test]
    async fn update_of_another_projects_key_is_not_found() {
        // The service filters on project_id, so a guessed key id in a project
        // the caller can reach never resolves to another project's row.
        let (state, audit) = state_with(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<analytics_ingest_keys::Model>::new()]),
        );

        let err = update_analytics_ingest_key(
            user_auth(Role::Admin),
            State(state),
            metadata(),
            Path((42, 7)),
            Json(UpdateAnalyticsIngestKeyRequest {
                name: Some("hijacked".into()),
                ..Default::default()
            }),
        )
        .await
        .err()
        .expect("a key from another project must not be updatable");
        assert_eq!(err.status_code, StatusCode::NOT_FOUND);
        assert!(
            audit.recorded().is_empty(),
            "a failed update must not emit an audit record"
        );
    }

    #[tokio::test]
    async fn revoke_of_another_projects_key_is_not_found() {
        let (state, _audit) = state_with(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<analytics_ingest_keys::Model>::new()]),
        );

        let err = revoke_analytics_ingest_key(
            user_auth(Role::Admin),
            State(state),
            metadata(),
            Path((42, 7)),
        )
        .await
        .err()
        .expect("a key from another project must not be revocable");
        assert_eq!(err.status_code, StatusCode::NOT_FOUND);
    }

    // ── Success paths ────────────────────────────────────────────────────

    #[tokio::test]
    async fn create_returns_201_with_the_key_in_the_clear_and_audits() {
        let (state, audit) = state_with(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![project_model(1)]])
                .append_query_results([vec![key_model(10, 1, None)]]),
        );

        let response = create_analytics_ingest_key(
            user_auth(Role::Admin),
            State(state),
            metadata(),
            Path(1),
            Json(CreateAnalyticsIngestKeyRequest::default()),
        )
        .await
        .expect("create should succeed")
        .into_response();

        assert_eq!(response.status(), StatusCode::CREATED);
        let body = body_json(response).await;
        assert_eq!(body["id"], 10);
        assert_eq!(body["project_id"], 1);
        assert_eq!(body["is_active"], true);

        let public_key = body["public_key"]
            .as_str()
            .expect("public_key must be a string");
        assert!(public_key.starts_with("pa_"), "{public_key}");
        assert_eq!(public_key.len(), 67, "{public_key}");
        assert!(
            !public_key.contains('*'),
            "the ingest key is public by construction and must never be masked"
        );

        assert_eq!(audit.recorded(), vec!["ANALYTICS_INGEST_KEY_CREATED"]);
    }

    #[tokio::test]
    async fn list_returns_200_with_every_key_unmasked() {
        let mut revoked = key_model(11, 1, None);
        revoked.is_active = false;
        revoked.public_key = format!("pa_{}", "1".repeat(64));

        let (state, _audit) = state_with(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![key_model(10, 1, None), revoked]]),
        );

        let response = list_analytics_ingest_keys(user_auth(Role::Admin), State(state), Path(1))
            .await
            .expect("list should succeed")
            .into_response();

        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        let keys = body.as_array().expect("body should be an array");
        assert_eq!(keys.len(), 2);
        for key in keys {
            let public_key = key["public_key"].as_str().expect("public_key");
            assert!(public_key.starts_with("pa_"));
            assert!(!public_key.contains('*'));
        }
        assert_eq!(keys[1]["is_active"], false, "revoked keys are still listed");
    }

    #[tokio::test]
    async fn update_returns_200_and_audits() {
        let mut updated = key_model(10, 1, None);
        updated.name = "Marketing site".to_string();

        let (state, audit) = state_with(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![key_model(10, 1, None)]])
                .append_query_results([vec![updated]]),
        );

        let response = update_analytics_ingest_key(
            user_auth(Role::Admin),
            State(state),
            metadata(),
            Path((1, 10)),
            Json(UpdateAnalyticsIngestKeyRequest {
                name: Some("Marketing site".into()),
                ..Default::default()
            }),
        )
        .await
        .expect("update should succeed")
        .into_response();

        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["name"], "Marketing site");
        assert_eq!(audit.recorded(), vec!["ANALYTICS_INGEST_KEY_UPDATED"]);
    }

    #[tokio::test]
    async fn rotate_returns_200_with_a_new_value_and_audits() {
        let original = key_model(10, 1, None);
        let mut rotated = original.clone();
        rotated.public_key = format!("pa_{}", "e".repeat(64));

        let (state, audit) = state_with(
            MockDatabase::new(DatabaseBackend::Postgres)
                // handler reads the outgoing value for the audit record
                .append_query_results([vec![original.clone()]])
                // service: find_scoped
                .append_query_results([vec![original.clone()]])
                // service: update
                .append_query_results([vec![rotated]]),
        );

        let response = rotate_analytics_ingest_key(
            user_auth(Role::Admin),
            State(state),
            metadata(),
            Path((1, 10)),
        )
        .await
        .expect("rotate should succeed")
        .into_response();

        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["id"], 10);
        assert_ne!(body["public_key"], serde_json::json!(original.public_key));
        assert_eq!(audit.recorded(), vec!["ANALYTICS_INGEST_KEY_ROTATED"]);
    }

    #[tokio::test]
    async fn revoke_returns_204_and_audits() {
        let mut revoked = key_model(10, 1, None);
        revoked.is_active = false;
        revoked.revoked_at = Some(Utc::now());

        let (state, audit) = state_with(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![key_model(10, 1, None)]])
                .append_query_results([vec![revoked]]),
        );

        let response = revoke_analytics_ingest_key(
            user_auth(Role::Admin),
            State(state),
            metadata(),
            Path((1, 10)),
        )
        .await
        .expect("revoke should succeed")
        .into_response();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(audit.recorded(), vec!["ANALYTICS_INGEST_KEY_REVOKED"]);
    }

    #[tokio::test]
    async fn revoke_releases_the_keys_rate_limit_bucket() {
        let mut revoked = key_model(10, 1, None);
        revoked.is_active = false;
        revoked.revoked_at = Some(Utc::now());

        let (state, _audit) = state_with(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![key_model(10, 1, None)]])
                .append_query_results([vec![revoked]]),
        );

        // Fill key 10's window so a bucket definitely exists in the limiter.
        assert!(state.rate_limiter.check(10, Some(1)).await);
        assert!(
            !state.rate_limiter.check(10, Some(1)).await,
            "the bucket must be occupied before the revoke, or this proves nothing"
        );

        revoke_analytics_ingest_key(
            user_auth(Role::Admin),
            State(state.clone()),
            metadata(),
            Path((1, 10)),
        )
        .await
        .expect("revoke should succeed");

        // The bucket is gone: a fresh window is available again. Nothing else
        // in the process ever prunes this map, so without the explicit
        // `forget` every revoked key would leak an entry forever.
        assert!(
            state.rate_limiter.check(10, Some(1)).await,
            "revoking a key must release its rate-limit bucket"
        );
    }

    // ── Error mapping ────────────────────────────────────────────────────

    #[test]
    fn cross_project_environment_error_does_not_leak_the_owning_project() {
        let problem: Problem = AnalyticsIngestKeyError::EnvironmentProjectMismatch {
            environment_id: 5,
            environment_project_id: 999,
            project_id: 1,
        }
        .into();

        assert_eq!(problem.status_code, StatusCode::NOT_FOUND);
        let rendered = serde_json::to_string(&problem.body).expect("problem body should serialize");
        assert!(
            !rendered.contains("999"),
            "the owning project id must not reach the client: {rendered}"
        );
    }

    #[test]
    fn error_status_mapping_is_exhaustive_and_correct() {
        let cases: Vec<(AnalyticsIngestKeyError, StatusCode)> = vec![
            (
                AnalyticsIngestKeyError::KeyNotFound {
                    key_id: 1,
                    project_id: 2,
                },
                StatusCode::NOT_FOUND,
            ),
            (
                AnalyticsIngestKeyError::ProjectNotFound { project_id: 2 },
                StatusCode::NOT_FOUND,
            ),
            (
                AnalyticsIngestKeyError::EnvironmentNotFound {
                    environment_id: 3,
                    project_id: 2,
                },
                StatusCode::NOT_FOUND,
            ),
            (
                AnalyticsIngestKeyError::Validation {
                    field: "name".into(),
                    message: "bad".into(),
                },
                StatusCode::BAD_REQUEST,
            ),
            (
                AnalyticsIngestKeyError::MalformedAllowedOrigins {
                    key_id: 4,
                    reason: "not an array".into(),
                },
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                AnalyticsIngestKeyError::Database(sea_orm::DbErr::Custom("boom".into())),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];

        for (error, expected) in cases {
            let problem: Problem = error.into();
            assert_eq!(problem.status_code, expected);
        }
    }

    // ── OpenAPI contract ─────────────────────────────────────────────────

    #[test]
    fn openapi_declares_path_params_and_unique_operation_ids() {
        let doc: serde_json::Value = serde_json::to_value(AnalyticsIngestKeyApiDoc::openapi())
            .expect("the generated OpenAPI document should serialize");
        let paths = doc["paths"].as_object().expect("paths object");
        let mut operation_ids = Vec::new();

        for (path, item) in paths {
            let item = item.as_object().expect("path item object");
            for (method, operation) in item {
                if !["get", "post", "patch", "put", "delete"].contains(&method.as_str()) {
                    continue;
                }
                let id = operation["operationId"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{path} {method} is missing an operationId"))
                    .to_string();
                assert!(
                    id.contains("analytics_ingest_key"),
                    "operationId '{id}' is too generic and will collide in the merged doc"
                );
                operation_ids.push(id);

                // Every `{...}` segment must be declared, or the generated TS
                // client types the path as `never` and the CLI won't compile.
                let declared: Vec<String> = operation["parameters"]
                    .as_array()
                    .map(|params| {
                        params
                            .iter()
                            .filter_map(|p| p["name"].as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                for segment in path.split('/') {
                    if let Some(name) = segment.strip_prefix('{').and_then(|s| s.strip_suffix('}'))
                    {
                        assert!(
                            declared.iter().any(|d| d == name),
                            "{path} {method} does not declare path param '{name}' \
                             (declared: {declared:?})"
                        );
                    }
                }
            }
        }

        assert_eq!(operation_ids.len(), 5, "expected five operations");
        let unique: std::collections::HashSet<_> = operation_ids.iter().collect();
        assert_eq!(unique.len(), 5, "operationIds must be unique");
    }

    /// The `public_key` field must never be described as a secret, and must
    /// never be masked. ADR-040 deliberately overrides the codebase's default
    /// "mask credentials in responses" rule for this one type: the value ships
    /// in a public JS bundle, and hiding it would only stop operators copying
    /// it.
    #[test]
    fn openapi_never_describes_the_key_as_confidential() {
        let doc: serde_json::Value = serde_json::to_value(AnalyticsIngestKeyApiDoc::openapi())
            .expect("the generated OpenAPI document should serialize");
        let rendered = doc.to_string().to_lowercase();

        for forbidden in [
            "keep it secret",
            "confidential",
            "do not share",
            "sensitive value",
        ] {
            assert!(
                !rendered.contains(forbidden),
                "the ingest key is public by construction; '{forbidden}' must not appear"
            );
        }
    }
}
