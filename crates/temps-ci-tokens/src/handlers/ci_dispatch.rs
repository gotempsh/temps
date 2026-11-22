// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Admin settings and build-scoped runner endpoints. Runner auth is deliberately
//! separate from session/API-key auth: a one-build credential cannot use either.
use crate::services::ci_dispatch::{
    CiDispatchService, CiError, CiExecutorSettings, CiExecutorStatus,
};
use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use std::sync::Arc;
use temps_auth::{permission_guard, RequireAuth};
use temps_core::{
    problemdetails::{self, Problem},
    AuditContext,
};
use temps_deployer::runner_protocol::{RunnerClaim, RunnerFailure, RunnerResult, RunnerWork};
use uuid::Uuid;
impl From<CiError> for Problem {
    fn from(error: CiError) -> Self {
        let status = match &error {
            CiError::Configuration(_) => StatusCode::BAD_REQUEST,
            CiError::Build { .. } | CiError::Verification { .. } => StatusCode::CONFLICT,
            CiError::NotReady { .. } => StatusCode::TOO_EARLY,
            CiError::Identity { .. } => StatusCode::FORBIDDEN,
            CiError::Database(_) | CiError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        problemdetails::new(status)
            .with_title("CI build dispatch")
            .with_detail(error.to_string())
    }
}
/// State of the authenticated CI builder settings routes.
pub struct CiAdminState {
    pub db: Arc<temps_database::DbConnection>,
    pub encryption_service: Arc<temps_core::EncryptionService>,
    pub data_dir: std::path::PathBuf,
    pub audit_service: Arc<dyn temps_core::AuditLogger>,
}
fn service(state: &CiAdminState) -> Result<CiDispatchService, CiError> {
    CiDispatchService::new(
        state.db.clone(),
        state.encryption_service.clone(),
        state.data_dir.clone(),
    )
}
fn bearer(headers: &HeaderMap) -> Result<&str, Problem> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(|| {
            problemdetails::new(StatusCode::UNAUTHORIZED)
                .with_title("One-build runner token required")
        })
}
pub struct CiRunnerState {
    pub dispatch: Arc<CiDispatchService>,
    pub log_service: Arc<temps_deployment_logs::LogService>,
    pub audit_service: Arc<dyn temps_core::AuditLogger>,
}
async fn audit(
    audit_service: &dyn temps_core::AuditLogger,
    user_id: i32,
    action: &str,
    build_id: Option<Uuid>,
) {
    let event = super::audit::CiBuildAudit {
        context: AuditContext {
            user_id,
            ip_address: None,
            user_agent: "temps-api".into(),
        },
        action: action.into(),
        build_id,
    };
    if let Err(error) = audit_service.create_audit_log(&event).await {
        tracing::error!(%error, action, "CI write succeeded but audit failed");
    }
}
#[utoipa::path(get, path = "/settings/ci-builder", tag = "CI builder", operation_id = "get_ci_builder",
    responses((status = 200, body = CiExecutorStatus), (status = 401, description = "Unauthorized"), (status = 403, description = "Forbidden"), (status = 500, description = "Storage failed")), security(("bearer_auth" = [])))]
async fn get_settings(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<CiAdminState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(service(&state)?.status().await?),
    ))
}
#[utoipa::path(put, path = "/settings/ci-builder", tag = "CI builder", operation_id = "set_ci_builder", request_body = CiExecutorSettings,
    responses((status = 200, body = CiExecutorStatus), (status = 400, description = "Invalid settings"), (status = 401, description = "Unauthorized"), (status = 403, description = "Forbidden"), (status = 500, description = "Storage failed")), security(("bearer_auth" = [])))]
async fn save_settings(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<CiAdminState>>,
    Json(config): Json<CiExecutorSettings>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    let service = service(&state)?;
    service.save(config).await?;
    audit(
        state.audit_service.as_ref(),
        auth.user_id(),
        "settings_saved",
        None,
    )
    .await;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(service.status().await?),
    ))
}
#[utoipa::path(post, path = "/settings/ci-builder/provision", tag = "CI builder", operation_id = "provision_ci_builder",
    responses((status = 204, description = "Builder repository and pinned shim provisioned"), (status = 400, description = "Configuration/provider error"), (status = 401, description = "Unauthorized"), (status = 403, description = "Forbidden"), (status = 500, description = "Storage failed")), security(("bearer_auth" = [])))]
async fn provision(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<CiAdminState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    service(&state)?.provision().await?;
    audit(
        state.audit_service.as_ref(),
        auth.user_id(),
        "shim_provisioned",
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(post, path = "/internal/ci-builds/{build_id}/claim", tag = "CI runner", operation_id = "claim_ci_build", request_body = RunnerClaim,
    params(("build_id" = String, Path, description = "Opaque build UUID")),
    responses((status = 200, body = RunnerWork), (status = 403, description = "Runner identity rejected"), (status = 409, description = "Build already claimed or terminal"), (status = 425, description = "Provider run has not been recorded; retry claim"), (status = 500, description = "Storage failed")))]
async fn claim(
    State(state): State<Arc<CiRunnerState>>,
    Path(id): Path<Uuid>,
    Json(request): Json<RunnerClaim>,
) -> Result<impl IntoResponse, Problem> {
    let result = match state.dispatch.claim(id, &request.identity_token).await {
        Ok(result) => result,
        Err(error) => {
            if matches!(error, CiError::Identity { .. }) {
                audit(
                    state.audit_service.as_ref(),
                    0,
                    "runner_identity_rejected",
                    Some(id),
                )
                .await;
            }
            return Err(error.into());
        }
    };
    audit(state.audit_service.as_ref(), 0, "runner_claimed", Some(id)).await;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(result)))
}
#[utoipa::path(get, path = "/internal/ci-builds/{build_id}/context", tag = "CI runner", operation_id = "get_ci_build_context",
    params(("build_id" = String, Path, description = "Opaque build UUID")),
    responses((status = 200, description = "Streamed agent-protocol context tar", content_type = "application/x-tar"), (status = 401, description = "One-build token required"), (status = 403, description = "Token rejected"), (status = 409, description = "Build inactive"), (status = 500, description = "Storage failed")), security(("bearer_auth" = [])))]
async fn context(
    State(state): State<Arc<CiRunnerState>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, Problem> {
    let service = &state.dispatch;
    service.authorize(id, bearer(&headers)?).await?;
    let file = tokio::fs::File::open(service.context_path(id))
        .await
        .map_err(CiError::from)?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/x-tar"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Body::from_stream(tokio_util::io::ReaderStream::new(file)),
    ))
}
#[utoipa::path(get, path = "/internal/ci-builds/{build_id}/lease", tag = "CI runner", operation_id = "check_ci_build_lease",
    params(("build_id" = String, Path, description = "Opaque build UUID")),
    responses((status = 204, description = "Lease active"), (status = 401, description = "Token required"), (status = 403, description = "Token rejected"), (status = 409, description = "Build cancelled or expired")), security(("bearer_auth" = [])))]
async fn lease(
    State(state): State<Arc<CiRunnerState>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, Problem> {
    state.dispatch.authorize(id, bearer(&headers)?).await?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(post, path = "/internal/ci-builds/{build_id}/result", tag = "CI runner", operation_id = "complete_ci_build", request_body = RunnerResult,
    params(("build_id" = String, Path, description = "Opaque build UUID")),
    responses((status = 204, description = "Verified result accepted and token revoked"), (status = 401, description = "Token required"), (status = 403, description = "Token rejected"), (status = 409, description = "Verification rejected or build inactive"), (status = 500, description = "Storage failed")), security(("bearer_auth" = [])))]
async fn complete(
    State(state): State<Arc<CiRunnerState>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(result): Json<RunnerResult>,
) -> Result<impl IntoResponse, Problem> {
    if let Err(error) = state.dispatch.complete(id, bearer(&headers)?, result).await {
        if matches!(error, CiError::Verification { .. }) {
            audit(
                state.audit_service.as_ref(),
                0,
                "runner_result_rejected",
                Some(id),
            )
            .await;
        }
        return Err(error.into());
    }
    audit(
        state.audit_service.as_ref(),
        0,
        "runner_result_verified",
        Some(id),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(post, path = "/internal/ci-builds/{build_id}/failure", tag = "CI runner", operation_id = "fail_ci_build", request_body = RunnerFailure,
    params(("build_id" = String, Path, description = "Opaque build UUID")),
    responses((status = 204, description = "Build failed and token revoked"), (status = 401, description = "Token required"), (status = 403, description = "Token rejected"), (status = 409, description = "Build inactive")), security(("bearer_auth" = [])))]
async fn fail(
    State(state): State<Arc<CiRunnerState>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(request): Json<RunnerFailure>,
) -> Result<impl IntoResponse, Problem> {
    let service = &state.dispatch;
    service.authorize(id, bearer(&headers)?).await?;
    service.fail(id, &request.message).await?;
    audit(state.audit_service.as_ref(), 0, "runner_failed", Some(id)).await;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(post, path = "/internal/ci-builds/{build_id}/logs", tag = "CI runner", operation_id = "append_ci_build_logs", request_body = temps_deployer::runner_protocol::RunnerLogs,
    params(("build_id" = String, Path, description = "Opaque build UUID")),
    responses((status = 204, description = "Log batch appended"), (status = 401, description = "Token required"), (status = 403, description = "Token rejected"), (status = 409, description = "Build inactive or batch exceeds limit")), security(("bearer_auth" = [])))]
async fn logs(
    State(state): State<Arc<CiRunnerState>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(logs): Json<temps_deployer::runner_protocol::RunnerLogs>,
) -> Result<impl IntoResponse, Problem> {
    state
        .dispatch
        .append_logs(id, bearer(&headers)?, logs, state.log_service.as_ref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(utoipa::OpenApi)]
#[openapi(
    paths(
        get_settings,
        save_settings,
        provision,
        claim,
        context,
        lease,
        complete,
        fail,
        logs
    ),
    components(schemas(
        CiExecutorSettings,
        CiExecutorStatus,
        crate::services::ci_dispatch::TrustedIdentityIssuer,
        RunnerClaim,
        RunnerWork,
        RunnerResult,
        RunnerFailure,
        temps_deployer::build_protocol::BuildSpec
    ))
)]
pub struct CiDispatchApiDoc;
pub fn admin_routes() -> Router<Arc<CiAdminState>> {
    Router::new()
        .route("/settings/ci-builder", get(get_settings).put(save_settings))
        .route("/settings/ci-builder/provision", post(provision))
}
pub fn runner_routes() -> Router<Arc<CiRunnerState>> {
    Router::new()
        .route("/internal/ci-builds/{build_id}/claim", post(claim))
        .route("/internal/ci-builds/{build_id}/context", get(context))
        .route("/internal/ci-builds/{build_id}/lease", get(lease))
        .route("/internal/ci-builds/{build_id}/result", post(complete))
        .route("/internal/ci-builds/{build_id}/failure", post(fail))
        .route("/internal/ci-builds/{build_id}/logs", post(logs))
        .layer(axum::extract::DefaultBodyLimit::max(256 * 1024))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use tower::ServiceExt;

    struct NoAudit;

    #[async_trait::async_trait]
    impl temps_core::AuditLogger for NoAudit {
        async fn create_audit_log(
            &self,
            _operation: &dyn temps_core::audit::AuditOperation,
        ) -> temps_core::AuditResult<()> {
            Ok(())
        }
    }

    fn user() -> temps_entities::users::Model {
        temps_entities::users::Model {
            id: 1,
            name: "Reader".to_string(),
            email: "reader@example.com".to_string(),
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
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn api_key(permissions: Vec<temps_auth::Permission>) -> temps_auth::AuthContext {
        temps_auth::AuthContext::new_api_key(user(), None, Some(permissions), "key".to_string(), 7)
    }

    async fn status_of(
        method: &str,
        uri: &str,
        auth: Option<temps_auth::AuthContext>,
    ) -> StatusCode {
        let state = Arc::new(CiAdminState {
            db: Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
            encryption_service: Arc::new(
                temps_core::EncryptionService::new("01234567890123456789012345678901")
                    .expect("test encryption key"),
            ),
            data_dir: std::env::temp_dir(),
            audit_service: Arc::new(NoAudit),
        });
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .expect("request");
        if let Some(auth) = auth {
            request.extensions_mut().insert(auth);
        }
        admin_routes()
            .with_state(state)
            .oneshot(request)
            .await
            .expect("response")
            .status()
    }

    #[tokio::test]
    async fn the_ci_builder_settings_refuse_anonymous_callers() {
        for (method, uri) in [
            ("GET", "/settings/ci-builder"),
            ("PUT", "/settings/ci-builder"),
            ("POST", "/settings/ci-builder/provision"),
        ] {
            assert_eq!(
                status_of(method, uri, None).await,
                StatusCode::UNAUTHORIZED,
                "{method} {uri}"
            );
        }
    }

    #[tokio::test]
    async fn reading_the_ci_builder_settings_needs_settings_read() {
        let outsider = api_key(vec![temps_auth::Permission::ProjectsRead]);
        assert_eq!(
            status_of("GET", "/settings/ci-builder", Some(outsider)).await,
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn provisioning_the_ci_builder_needs_settings_write() {
        let reader = api_key(vec![temps_auth::Permission::SettingsRead]);
        assert_eq!(
            status_of("POST", "/settings/ci-builder/provision", Some(reader)).await,
            StatusCode::FORBIDDEN
        );
    }
}
