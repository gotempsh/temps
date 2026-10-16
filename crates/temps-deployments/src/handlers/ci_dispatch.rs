// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Admin settings and build-scoped runner endpoints. Runner auth is deliberately
//! separate from session/API-key auth: a one-build credential cannot use either.
use super::types::AppState;
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
fn service(state: &AppState) -> Result<CiDispatchService, CiError> {
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
async fn audit(state: &AppState, user_id: i32, action: &str, build_id: Option<Uuid>) {
    let event = super::audit::CiBuildAudit {
        context: AuditContext {
            user_id,
            ip_address: None,
            user_agent: "temps-api".into(),
        },
        action: action.into(),
        build_id,
    };
    if let Err(error) = state.audit_service.create_audit_log(&event).await {
        tracing::error!(%error, action, "CI write succeeded but audit failed");
    }
}
#[utoipa::path(get, path = "/settings/ci-builder", tag = "CI builder", operation_id = "get_ci_builder",
    responses((status = 200, body = CiExecutorStatus), (status = 401, description = "Unauthorized"), (status = 403, description = "Forbidden"), (status = 500, description = "Storage failed")), security(("bearer_auth" = [])))]
async fn get_settings(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
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
    State(state): State<Arc<AppState>>,
    Json(config): Json<CiExecutorSettings>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    let service = service(&state)?;
    service.save(config).await?;
    audit(&state, auth.user_id(), "settings_saved", None).await;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(service.status().await?),
    ))
}
#[utoipa::path(post, path = "/settings/ci-builder/provision", tag = "CI builder", operation_id = "provision_ci_builder",
    responses((status = 204, description = "Builder repository and pinned shim provisioned"), (status = 400, description = "Configuration/provider error"), (status = 401, description = "Unauthorized"), (status = 403, description = "Forbidden"), (status = 500, description = "Storage failed")), security(("bearer_auth" = [])))]
async fn provision(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    service(&state)?.provision().await?;
    audit(&state, auth.user_id(), "shim_provisioned", None).await;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(post, path = "/internal/ci-builds/{build_id}/claim", tag = "CI runner", operation_id = "claim_ci_build", request_body = RunnerClaim,
    params(("build_id" = String, Path, description = "Opaque build UUID")),
    responses((status = 200, body = RunnerWork), (status = 403, description = "Runner identity rejected"), (status = 409, description = "Build already claimed or terminal"), (status = 425, description = "Provider run has not been recorded; retry claim"), (status = 500, description = "Storage failed")))]
async fn claim(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(request): Json<RunnerClaim>,
) -> Result<impl IntoResponse, Problem> {
    let result = match service(&state)?.claim(id, &request.identity_token).await {
        Ok(result) => result,
        Err(error) => {
            if matches!(error, CiError::Identity { .. }) {
                audit(&state, 0, "runner_identity_rejected", Some(id)).await;
            }
            return Err(error.into());
        }
    };
    audit(&state, 0, "runner_claimed", Some(id)).await;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(result)))
}
#[utoipa::path(get, path = "/internal/ci-builds/{build_id}/context", tag = "CI runner", operation_id = "get_ci_build_context",
    params(("build_id" = String, Path, description = "Opaque build UUID")),
    responses((status = 200, description = "Streamed agent-protocol context tar", content_type = "application/x-tar"), (status = 401, description = "One-build token required"), (status = 403, description = "Token rejected"), (status = 409, description = "Build inactive"), (status = 500, description = "Storage failed")), security(("bearer_auth" = [])))]
async fn context(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, Problem> {
    let service = service(&state)?;
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
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, Problem> {
    service(&state)?.authorize(id, bearer(&headers)?).await?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(post, path = "/internal/ci-builds/{build_id}/result", tag = "CI runner", operation_id = "complete_ci_build", request_body = RunnerResult,
    params(("build_id" = String, Path, description = "Opaque build UUID")),
    responses((status = 204, description = "Verified result accepted and token revoked"), (status = 401, description = "Token required"), (status = 403, description = "Token rejected"), (status = 409, description = "Verification rejected or build inactive"), (status = 500, description = "Storage failed")), security(("bearer_auth" = [])))]
async fn complete(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(result): Json<RunnerResult>,
) -> Result<impl IntoResponse, Problem> {
    if let Err(error) = service(&state)?
        .complete(id, bearer(&headers)?, result)
        .await
    {
        if matches!(error, CiError::Verification { .. }) {
            audit(&state, 0, "runner_result_rejected", Some(id)).await;
        }
        return Err(error.into());
    }
    audit(&state, 0, "runner_result_verified", Some(id)).await;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(post, path = "/internal/ci-builds/{build_id}/failure", tag = "CI runner", operation_id = "fail_ci_build", request_body = RunnerFailure,
    params(("build_id" = String, Path, description = "Opaque build UUID")),
    responses((status = 204, description = "Build failed and token revoked"), (status = 401, description = "Token required"), (status = 403, description = "Token rejected"), (status = 409, description = "Build inactive")), security(("bearer_auth" = [])))]
async fn fail(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(request): Json<RunnerFailure>,
) -> Result<impl IntoResponse, Problem> {
    let service = service(&state)?;
    service.authorize(id, bearer(&headers)?).await?;
    service.fail(id, &request.message).await?;
    audit(&state, 0, "runner_failed", Some(id)).await;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(post, path = "/internal/ci-builds/{build_id}/logs", tag = "CI runner", operation_id = "append_ci_build_logs", request_body = temps_deployer::runner_protocol::RunnerLogs,
    params(("build_id" = String, Path, description = "Opaque build UUID")),
    responses((status = 204, description = "Log batch appended"), (status = 401, description = "Token required"), (status = 403, description = "Token rejected"), (status = 409, description = "Build inactive or batch exceeds limit")), security(("bearer_auth" = [])))]
async fn logs(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(logs): Json<temps_deployer::runner_protocol::RunnerLogs>,
) -> Result<impl IntoResponse, Problem> {
    service(&state)?
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
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/settings/ci-builder", get(get_settings).put(save_settings))
        .route("/settings/ci-builder/provision", post(provision))
        .route("/internal/ci-builds/{build_id}/claim", post(claim))
        .route("/internal/ci-builds/{build_id}/context", get(context))
        .route("/internal/ci-builds/{build_id}/lease", get(lease))
        .route("/internal/ci-builds/{build_id}/result", post(complete))
        .route("/internal/ci-builds/{build_id}/failure", post(fail))
        .route("/internal/ci-builds/{build_id}/logs", post(logs))
        .layer(axum::extract::DefaultBodyLimit::max(256 * 1024))
}
