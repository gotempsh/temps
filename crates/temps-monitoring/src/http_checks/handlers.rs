// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_auth::{permission_guard, project_access_guard, project_scope_guard, RequireAuth};
use temps_core::{
    problemdetails::{self, Problem},
    AuditContext, AuditOperation, RequestMetadata,
};
use temps_credential_checks::{provider_presets, ProviderPreset};
use utoipa::{IntoParams, OpenApi};

pub struct HttpChecksState {
    pub service: Arc<HttpChecksService>,
    pub audit: Arc<dyn temps_core::AuditLogger>,
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}
impl From<HttpChecksError> for Problem {
    fn from(error: HttpChecksError) -> Self {
        let status = match &error {
            HttpChecksError::NotFound { .. } => StatusCode::NOT_FOUND,
            HttpChecksError::Invalid { .. } => StatusCode::BAD_REQUEST,
            HttpChecksError::Busy { .. } => StatusCode::CONFLICT,
            HttpChecksError::Database { .. }
            | HttpChecksError::Encryption { .. }
            | HttpChecksError::Stored { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        };
        problemdetails::new(status)
            .with_title("HTTP check failed")
            .with_detail(error.to_string())
    }
}
#[derive(Deserialize, IntoParams)]
pub struct ListQuery {
    pub page: Option<u64>,
    pub page_size: Option<u64>,
}
#[derive(Serialize)]
struct CheckAudit {
    context: AuditContext,
    project_id: i32,
    check_id: i32,
    operation: &'static str,
}
impl AuditOperation for CheckAudit {
    fn operation_type(&self) -> String {
        self.operation.into()
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
        Ok(serde_json::to_string(self)?)
    }
}
async fn audit(
    state: &HttpChecksState,
    user_id: i32,
    metadata: RequestMetadata,
    project_id: i32,
    check_id: i32,
    operation: &'static str,
) {
    let event = CheckAudit {
        context: AuditContext {
            user_id,
            ip_address: Some(metadata.ip_address),
            user_agent: metadata.user_agent,
        },
        project_id,
        check_id,
        operation,
    };
    if let Err(error) = state.audit.create_audit_log(&event).await {
        tracing::error!(project_id,check_id,error=%error,"Could not record HTTP check audit event");
    }
}

#[utoipa::path(get, path="/projects/{project_id}/http-checks", tag="HTTP Checks", operation_id="listHttpChecks",
    params(("project_id"=i32,Path,description="Project ID"),ListQuery),
    responses((status=200,description="Success",body=HttpCheckList),(status=400,description="Invalid configuration"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Not found"),(status=409,description="Check busy"),(status=500,description="Internal error")),security(("bearer_auth"=[])))]
pub async fn list(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<HttpChecksState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<ListQuery>,
) -> Result<Json<HttpCheckList>, Problem> {
    permission_guard!(auth, EnvironmentsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    Ok(Json(
        state
            .service
            .list(
                project_id,
                query.page.unwrap_or(1),
                query.page_size.unwrap_or(20),
            )
            .await?,
    ))
}

#[utoipa::path(get, path="/projects/{project_id}/http-checks/presets", tag="HTTP Checks", operation_id="listHttpCheckPresets",
    params(("project_id"=i32,Path,description="Project ID")),
    responses((status=200,description="Success",body=Vec<ProviderPreset>),(status=400,description="Invalid configuration"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Not found"),(status=409,description="Check busy"),(status=500,description="Internal error")),security(("bearer_auth"=[])))]
pub async fn presets(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<HttpChecksState>>,
    Path(project_id): Path<i32>,
) -> Result<Json<Vec<ProviderPreset>>, Problem> {
    permission_guard!(auth, EnvironmentsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    Ok(Json(provider_presets()))
}

#[utoipa::path(post, path="/projects/{project_id}/env-vars/{env_var_id}/detect", tag="HTTP Checks", operation_id="detectEnvCredential",
    params(("project_id"=i32,Path,description="Project ID"),("env_var_id"=i32,Path,description="Environment variable ID")),
    responses((status=200,description="Success",body=DetectionView),(status=400,description="Invalid configuration"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Not found"),(status=409,description="Check busy"),(status=500,description="Internal error")),security(("bearer_auth"=[])))]
pub async fn detect(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<HttpChecksState>>,
    Path((project_id, env_var_id)): Path<(i32, i32)>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<Json<DetectionView>, Problem> {
    permission_guard!(auth, EnvironmentsWrite);
    permission_guard!(auth, SecretsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let result = state.service.detect(project_id, env_var_id).await?;
    audit(
        &state,
        auth.user_id(),
        metadata,
        project_id,
        env_var_id,
        "HTTP_CREDENTIAL_DETECTED",
    )
    .await;
    Ok(Json(result))
}

#[utoipa::path(post, path="/projects/{project_id}/http-checks", tag="HTTP Checks", operation_id="createHttpCheck",
    params(("project_id"=i32,Path,description="Project ID")),
    request_body=SaveHttpCheck,
    responses((status=200,description="Success",body=HttpCheckView),(status=400,description="Invalid configuration"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Not found"),(status=409,description="Check busy"),(status=500,description="Internal error")),security(("bearer_auth"=[])))]
pub async fn create(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<HttpChecksState>>,
    Path(project_id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(input): Json<SaveHttpCheck>,
) -> Result<Json<HttpCheckView>, Problem> {
    permission_guard!(auth, EnvironmentsWrite);
    permission_guard!(auth, SecretsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let result = state.service.save(project_id, None, input).await?;
    audit(
        &state,
        auth.user_id(),
        metadata,
        project_id,
        result.id,
        "HTTP_CHECK_CREATED",
    )
    .await;
    Ok(Json(result))
}

#[utoipa::path(put, path="/projects/{project_id}/http-checks/{check_id}", tag="HTTP Checks", operation_id="updateHttpCheck",
    params(("project_id"=i32,Path,description="Project ID"),("check_id"=i32,Path,description="Check ID")),
    request_body=SaveHttpCheck,
    responses((status=200,description="Success",body=HttpCheckView),(status=400,description="Invalid configuration"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Not found"),(status=409,description="Check busy"),(status=500,description="Internal error")),security(("bearer_auth"=[])))]
pub async fn update(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<HttpChecksState>>,
    Path((project_id, check_id)): Path<(i32, i32)>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(input): Json<SaveHttpCheck>,
) -> Result<Json<HttpCheckView>, Problem> {
    permission_guard!(auth, EnvironmentsWrite);
    permission_guard!(auth, SecretsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let result = state
        .service
        .save(project_id, Some(check_id), input)
        .await?;
    audit(
        &state,
        auth.user_id(),
        metadata,
        project_id,
        result.id,
        "HTTP_CHECK_UPDATED",
    )
    .await;
    Ok(Json(result))
}

#[utoipa::path(post, path="/projects/{project_id}/http-checks/{check_id}/run", tag="HTTP Checks", operation_id="runHttpCheck",
    params(("project_id"=i32,Path,description="Project ID"),("check_id"=i32,Path,description="Check ID")),
    responses((status=200,description="Success",body=HttpCheckView),(status=400,description="Invalid configuration"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Not found"),(status=409,description="Check busy"),(status=500,description="Internal error")),security(("bearer_auth"=[])))]
pub async fn run(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<HttpChecksState>>,
    Path((project_id, check_id)): Path<(i32, i32)>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<Json<HttpCheckView>, Problem> {
    permission_guard!(auth, EnvironmentsWrite);
    permission_guard!(auth, SecretsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    audit(
        &state,
        auth.user_id(),
        metadata,
        project_id,
        check_id,
        "HTTP_CHECK_REQUESTED",
    )
    .await;
    Ok(Json(state.service.run_now(project_id, check_id).await?))
}

#[utoipa::path(delete, path="/projects/{project_id}/http-checks/{check_id}", tag="HTTP Checks", operation_id="deleteHttpCheck",
    params(("project_id"=i32,Path,description="Project ID"),("check_id"=i32,Path,description="Check ID")),
    responses((status=200,description="Success"),(status=400,description="Invalid configuration"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Not found"),(status=409,description="Check busy"),(status=500,description="Internal error")),security(("bearer_auth"=[])))]
pub async fn delete(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<HttpChecksState>>,
    Path((project_id, check_id)): Path<(i32, i32)>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, EnvironmentsWrite);
    permission_guard!(auth, SecretsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    state.service.delete(project_id, check_id).await?;
    audit(
        &state,
        auth.user_id(),
        metadata,
        project_id,
        check_id,
        "HTTP_CHECK_DELETED",
    )
    .await;
    Ok(StatusCode::OK)
}

#[utoipa::path(get,path="/projects/{project_id}/http-checks/capabilities",tag="HTTP Checks",operation_id="getHttpChecksCapabilities",params(("project_id"=i32,Path)),responses((status=200,description="Capabilities",body=HttpChecksCapabilities),(status=401,description="Unauthorized"),(status=403,description="Forbidden")),security(("bearer_auth"=[])))]
pub async fn capabilities(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<HttpChecksState>>,
    Path(project_id): Path<i32>,
) -> Result<Json<HttpChecksCapabilities>, Problem> {
    permission_guard!(auth, EnvironmentsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    Ok(Json(state.service.capabilities().await))
}
#[utoipa::path(patch,path="/projects/{project_id}/http-checks/{check_id}",tag="HTTP Checks",operation_id="setHttpCheckEnabled",params(("project_id"=i32,Path),("check_id"=i32,Path)),request_body=SetHttpCheckEnabled,responses((status=200,description="Updated check",body=HttpCheckView),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Not found"),(status=500,description="Internal error")),security(("bearer_auth"=[])))]
pub async fn set_enabled(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<HttpChecksState>>,
    Path((project_id, check_id)): Path<(i32, i32)>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(body): Json<SetHttpCheckEnabled>,
) -> Result<Json<HttpCheckView>, Problem> {
    authorize_check_toggle(&auth, body.enabled)?;
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let result = state
        .service
        .set_enabled(project_id, check_id, body.enabled)
        .await?;
    audit(
        &state,
        auth.user_id(),
        metadata,
        project_id,
        check_id,
        "HTTP_CHECK_ENABLED_CHANGED",
    )
    .await;
    Ok(Json(result))
}

#[utoipa::path(get,path="/projects/{project_id}/env-vars/{env_var_id}/history",tag="HTTP Checks",operation_id="listVariableHistory",params(("project_id"=i32,Path),("env_var_id"=i32,Path),ListQuery),responses((status=200,description="Variable activity and verification history",body=VariableHistoryList),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Not found")),security(("bearer_auth"=[])))]
pub async fn history(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<HttpChecksState>>,
    Path((project_id, env_var_id)): Path<(i32, i32)>,
    Query(query): Query<ListQuery>,
) -> Result<Json<VariableHistoryList>, Problem> {
    permission_guard!(auth, EnvironmentsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    Ok(Json(
        state
            .service
            .variable_history(
                project_id,
                env_var_id,
                query.page.unwrap_or(1),
                query.page_size.unwrap_or(15),
            )
            .await?,
    ))
}
pub fn routes() -> Router<Arc<HttpChecksState>> {
    Router::new()
        .route(
            "/projects/{project_id}/env-vars/{env_var_id}/history",
            get(history),
        )
        .route("/projects/{project_id}/http-checks", get(list).post(create))
        .route("/projects/{project_id}/http-checks/presets", get(presets))
        .route(
            "/projects/{project_id}/http-checks/capabilities",
            get(capabilities),
        )
        .route(
            "/projects/{project_id}/http-checks/{check_id}",
            axum::routing::put(update).delete(delete).patch(set_enabled),
        )
        .route(
            "/projects/{project_id}/http-checks/{check_id}/run",
            post(run),
        )
        .route(
            "/projects/{project_id}/env-vars/{env_var_id}/detect",
            post(detect),
        )
}
#[derive(OpenApi)]
#[openapi(paths(list,presets,detect,create,update,run,delete,capabilities,set_enabled,history),components(schemas(VariableHistoryList,VariableHistoryEntry,HttpChecksCapabilities,SetHttpCheckEnabled,HttpCheckView,HttpCheckList,SaveHttpCheck,DetectionView,temps_credential_checks::Candidate,temps_credential_checks::ProviderPreset,temps_credential_checks::HttpCheckSpec,temps_credential_checks::VerificationResult)),tags((name="HTTP Checks",description="Provider-independent HTTP credential checks")))]
pub struct HttpChecksApiDoc;

fn authorize_check_toggle(auth: &temps_auth::AuthContext, enabled: bool) -> Result<(), Problem> {
    permission_guard!(auth, EnvironmentsWrite);
    if enabled {
        permission_guard!(auth, SecretsRead);
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;
    #[test]
    fn resuming_requires_secret_permission_but_pausing_does_not() {
        let user = temps_entities::users::Model {
            id: 1,
            name: "Test".into(),
            email: "test@example.com".into(),
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
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let auth = temps_auth::AuthContext::new_session(user.clone(), temps_auth::Role::User);
        assert!(authorize_check_toggle(&auth, false).is_ok());
        assert_eq!(
            authorize_check_toggle(&auth, true)
                .unwrap_err()
                .into_response()
                .status(),
            StatusCode::FORBIDDEN
        );
        let admin = temps_auth::AuthContext::new_session(user, temps_auth::Role::Admin);
        assert!(authorize_check_toggle(&admin, true).is_ok());
    }
}
