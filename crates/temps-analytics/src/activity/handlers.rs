// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{service::ActivityService, types::*};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_auth::{
    deny_deployment_token, permission_guard, project_access_guard, project_scope_guard, RequireAuth,
};
use temps_core::{
    problemdetails::{self, Problem},
    AuditLogger, AuditOperation, RequestMetadata,
};
use utoipa::OpenApi;

pub struct ActivityState {
    pub service: Arc<ActivityService>,
    pub audit: Arc<dyn AuditLogger>,
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
}

#[derive(OpenApi)]
#[openapi(paths(get_activity_status, save_activity_settings, run_activity_report, preview_activity_report, suggest_activity_goals), components(schemas(
    ActivityGoalsRequest, ActivityGoals, ActivityGoal, ActivityPreviewRequest, ActivityPreview, ActivitySettings, ActivityCategory, ActivityStatus, ActivityRunSummary, ActivityReport, VisitorActivityAssessment, ActivityEvidence, ActivityProperty
)), tags((name = "Visitor Activity", description = "AI interpretation of recent visitor activity")))]
pub struct ActivityApiDoc;

pub fn routes() -> Router<Arc<ActivityState>> {
    Router::new()
        .route(
            "/projects/{project_id}/analytics/activity/goals",
            post(suggest_activity_goals),
        )
        .route(
            "/projects/{project_id}/analytics/activity/preview",
            post(preview_activity_report),
        )
        .route(
            "/projects/{project_id}/analytics/activity",
            get(get_activity_status).put(save_activity_settings),
        )
        .route(
            "/projects/{project_id}/analytics/activity/run",
            post(run_activity_report),
        )
}

#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
struct ActivityStatusQuery {
    environment_id: Option<i32>,
}

impl From<ActivityError> for Problem {
    fn from(error: ActivityError) -> Self {
        let status = match &error {
            ActivityError::NotFound { .. } => StatusCode::NOT_FOUND,
            ActivityError::Validation { .. } => StatusCode::BAD_REQUEST,
            ActivityError::Busy { .. } => StatusCode::CONFLICT,
            ActivityError::Unavailable { .. } => StatusCode::SERVICE_UNAVAILABLE,
            ActivityError::Database { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            ActivityError::Analysis { .. } => StatusCode::BAD_GATEWAY,
        };
        let detail = match &error {
            ActivityError::Database { project_id, .. }
            | ActivityError::Analysis { project_id, .. } => {
                tracing::error!(error = %error, "Activity report request failed");
                format!("Activity analysis failed for project {project_id}. Check server logs and AI provider settings.")
            }
            ActivityError::NotFound { .. }
            | ActivityError::Validation { .. }
            | ActivityError::Busy { .. }
            | ActivityError::Unavailable { .. } => error.to_string(),
        };
        problemdetails::new(status)
            .with_title("Visitor activity analysis")
            .with_detail(detail)
    }
}

#[utoipa::path(get, path = "/projects/{project_id}/analytics/activity", tag = "Visitor Activity",
    params(("project_id" = i32, Path), ActivityStatusQuery), responses((status = 200, body = ActivityStatus)), security(("bearer_auth" = [])))]
async fn get_activity_status(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ActivityState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<ActivityStatusQuery>,
) -> Result<Json<ActivityStatus>, Problem> {
    permission_guard!(auth, AnalyticsRead);
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    Ok(Json(
        state
            .service
            .status(project_id, query.environment_id)
            .await?,
    ))
}

#[utoipa::path(put, path = "/projects/{project_id}/analytics/activity", tag = "Visitor Activity",
    params(("project_id" = i32, Path)), request_body = ActivitySettings,
    responses((status = 204, description = "Settings saved")), security(("bearer_auth" = [])))]
async fn save_activity_settings(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ActivityState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(settings): Json<ActivitySettings>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, AnalyticsWrite);
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    state.service.save(project_id, settings).await?;
    record_audit(
        &state,
        project_id,
        auth.user_id_opt(),
        metadata,
        "VISITOR_ACTIVITY_SETTINGS_UPDATED",
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/projects/{project_id}/analytics/activity/run", tag = "Visitor Activity",
    params(("project_id" = i32, Path)), responses((status = 200, body = ActivityReport)), security(("bearer_auth" = [])))]
async fn run_activity_report(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ActivityState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
) -> Result<Json<ActivityReport>, Problem> {
    permission_guard!(auth, AnalyticsWrite);
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    record_audit(
        &state,
        project_id,
        auth.user_id_opt(),
        metadata,
        "VISITOR_ACTIVITY_RUN_REQUESTED",
    )
    .await;
    Ok(Json(state.service.run(project_id, false).await?))
}

#[utoipa::path(post, path = "/projects/{project_id}/analytics/activity/preview", tag = "Visitor Activity",
    params(("project_id" = i32, Path)), request_body = ActivityPreviewRequest,
    responses((status = 200, body = ActivityPreview), (status = 400, description = "Invalid goal or sharing disabled"),
        (status = 403, description = "Access denied"), (status = 404, description = "Project not found"),
        (status = 409, description = "Analysis busy or preview cooling down"),
        (status = 502, description = "AI preview failed"), (status = 503, description = "AI provider not configured")),
    security(("bearer_auth" = [])))]
async fn preview_activity_report(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ActivityState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(request): Json<ActivityPreviewRequest>,
) -> Result<Json<ActivityPreview>, Problem> {
    permission_guard!(auth, AnalyticsWrite);
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    record_audit(
        &state,
        project_id,
        auth.user_id_opt(),
        metadata,
        "VISITOR_ACTIVITY_PREVIEW_REQUESTED",
    )
    .await;
    Ok(Json(state.service.preview(project_id, request).await?))
}

#[utoipa::path(post, path = "/projects/{project_id}/analytics/activity/goals", tag = "Visitor Activity",
    params(("project_id" = i32, Path)), request_body = ActivityGoalsRequest,
    responses((status = 200, body = ActivityGoals), (status = 400, description = "Invalid public URL or consent missing"),
        (status = 403, description = "Access denied"), (status = 409, description = "Busy"),
        (status = 502, description = "Site scan or AI suggestion failed"), (status = 503, description = "AI provider missing")), security(("bearer_auth" = [])))]
async fn suggest_activity_goals(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ActivityState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(request): Json<ActivityGoalsRequest>,
) -> Result<Json<ActivityGoals>, Problem> {
    permission_guard!(auth, AnalyticsWrite);
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    record_audit(
        &state,
        project_id,
        auth.user_id_opt(),
        metadata,
        "VISITOR_ACTIVITY_GOALS_REQUESTED",
    )
    .await;
    Ok(Json(
        state.service.suggest_goals(project_id, request).await?,
    ))
}

#[derive(Serialize)]
struct ActivityAudit {
    project_id: i32,
    user_id: Option<i32>,
    ip_address: String,
    user_agent: String,
    operation: String,
}
impl AuditOperation for ActivityAudit {
    fn operation_type(&self) -> String {
        self.operation.clone()
    }
    fn user_id(&self) -> Option<i32> {
        self.user_id
    }
    fn ip_address(&self) -> Option<String> {
        Some(self.ip_address.clone())
    }
    fn user_agent(&self) -> &str {
        &self.user_agent
    }
    fn serialize(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string(self)?)
    }
}
async fn record_audit(
    state: &ActivityState,
    project_id: i32,
    user_id: Option<i32>,
    metadata: RequestMetadata,
    operation: &str,
) {
    let audit = ActivityAudit {
        project_id,
        user_id,
        ip_address: metadata.ip_address,
        user_agent: metadata.user_agent,
        operation: operation.into(),
    };
    if let Err(error) = state.audit.create_audit_log(&audit).await {
        tracing::error!(project_id, error = %error, "Failed to audit visitor activity action");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_ai::{AiError, AiRequest, AiResponse, AiService, ChatTurnRequest, TokenStream};
    use temps_auth::context::AuthContext;
    use temps_entities::deployment_tokens::DeploymentTokenPermission;

    struct NoAi;
    #[async_trait::async_trait]
    impl AiService for NoAi {
        async fn is_available(&self) -> bool {
            false
        }
        async fn complete(&self, _: AiRequest) -> Result<AiResponse, AiError> {
            panic!("unauthorized handler reached AI")
        }
        async fn chat_stream(&self, _: ChatTurnRequest) -> Result<TokenStream, AiError> {
            Err(AiError::NotAvailable)
        }
    }
    struct NoAudit;
    #[async_trait::async_trait]
    impl AuditLogger for NoAudit {
        async fn create_audit_log(&self, _: &dyn AuditOperation) -> anyhow::Result<()> {
            panic!("unauthorized handler reached audit")
        }
    }
    fn state() -> State<Arc<ActivityState>> {
        State(Arc::new(ActivityState {
            service: Arc::new(ActivityService::new(
                Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
                Arc::new(NoAi),
            )),
            audit: Arc::new(NoAudit),
            project_access_checker: None,
        }))
    }
    fn auth() -> RequireAuth {
        RequireAuth(AuthContext::new_deployment_token(
            1,
            None,
            None,
            1,
            "test".into(),
            vec![DeploymentTokenPermission::FullAccess],
        ))
    }
    fn metadata() -> Extension<RequestMetadata> {
        Extension(RequestMetadata {
            ip_address: "127.0.0.1".into(),
            user_agent: "test".into(),
            headers: axum::http::HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "http://example.test".into(),
            scheme: "http".into(),
            host: "example.test".into(),
            is_secure: false,
        })
    }
    #[tokio::test]
    async fn deployment_tokens_cannot_read_configure_or_spend_on_ai() {
        let read = get_activity_status(
            auth(),
            state(),
            Path(1),
            Query(ActivityStatusQuery::default()),
        )
        .await
        .unwrap_err();
        let write = save_activity_settings(
            auth(),
            state(),
            metadata(),
            Path(1),
            Json(ActivitySettings::default()),
        )
        .await
        .unwrap_err();
        let run = run_activity_report(auth(), state(), metadata(), Path(1))
            .await
            .unwrap_err();
        let preview = preview_activity_report(
            auth(),
            state(),
            metadata(),
            Path(1),
            Json(ActivityPreviewRequest {
                goal: "Understand readers".into(),
                share_activity_with_ai: true,
                property_keys: vec![],
                environment_id: None,
                source_url: None,
                source_domain: None,
                min_sessions: 2,
                min_page_paths: 2,
            }),
        )
        .await
        .unwrap_err();
        let goals = suggest_activity_goals(
            auth(),
            state(),
            metadata(),
            Path(1),
            Json(ActivityGoalsRequest {
                url: "https://example.com".into(),
                share_with_ai: true,
                environment_id: None,
            }),
        )
        .await
        .unwrap_err();
        for problem in [read, write, run, preview, goals] {
            assert_eq!(problem.status_code, StatusCode::FORBIDDEN);
            assert!(serde_json::to_string(&problem.body)
                .unwrap()
                .contains("deployment-token-not-allowed"));
        }
    }
    #[test]
    fn errors_map_to_status_codes_without_provider_payloads() {
        let problem: Problem = ActivityError::Analysis {
            project_id: 1,
            reason: "private-provider-response".into(),
        }
        .into();
        assert_eq!(problem.status_code, StatusCode::BAD_GATEWAY);
        assert!(!serde_json::to_string(&problem.body)
            .unwrap()
            .contains("private-provider-response"));
        let problem: Problem = ActivityError::Busy { project_id: 1 }.into();
        assert_eq!(problem.status_code, StatusCode::CONFLICT);
        let problem: Problem = ActivityError::Validation {
            project_id: 1,
            reason: "Missing context".into(),
        }
        .into();
        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
    }
}
