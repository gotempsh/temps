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
use temps_authz::{
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
    ActivityGoalsRequest, ActivityGoals, ActivityGoal, ActivityPreviewRequest, ActivityPreview, ActivitySettings, ActivityCategory, ActivityStatus, ActivityRunSummary, ActivityReport, VisitorActivityAssessment, ActivityEvidence, ActivityProperty,
    ActivityInProgress, ActivityCooldowns, ActivityOperation, ActivityErrorBody, ActivityErrorCode, ActivityProviderFailure
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
        let detail = error.user_message();
        let mut body = ActivityErrorBody {
            title: "Visitor activity analysis".into(),
            detail,
            code: ActivityErrorCode::Internal,
            operation: None,
            started_at: None,
            retry_after_secs: None,
            provider_failure: None,
            model: None,
            provider_status: None,
            setup_url: None,
        };
        let status = match &error {
            ActivityError::NotFound { .. } => {
                body.code = ActivityErrorCode::NotFound;
                StatusCode::NOT_FOUND
            }
            ActivityError::Validation { .. } => {
                body.code = ActivityErrorCode::Validation;
                StatusCode::BAD_REQUEST
            }
            ActivityError::AnalysisRunning {
                operation,
                started_at,
                retry_after_secs,
                ..
            } => {
                body.code = ActivityErrorCode::AnalysisRunning;
                body.title = "Analysis already running".into();
                body.operation = Some(*operation);
                body.started_at = Some(*started_at);
                body.retry_after_secs = Some(*retry_after_secs);
                StatusCode::CONFLICT
            }
            ActivityError::CoolingDown {
                operation,
                retry_after_secs,
                ..
            } => {
                body.code = ActivityErrorCode::CoolingDown;
                body.title = "Cooling down".into();
                body.operation = Some(*operation);
                body.retry_after_secs = Some(*retry_after_secs);
                StatusCode::TOO_MANY_REQUESTS
            }
            ActivityError::ServerBusy {
                operation,
                retry_after_secs,
                ..
            } => {
                body.code = ActivityErrorCode::ServerBusy;
                body.title = "Analysis capacity busy".into();
                body.operation = Some(*operation);
                body.retry_after_secs = Some(*retry_after_secs);
                StatusCode::SERVICE_UNAVAILABLE
            }
            ActivityError::Superseded { .. } => {
                body.code = ActivityErrorCode::Superseded;
                StatusCode::CONFLICT
            }
            ActivityError::Unavailable { .. } => {
                body.code = ActivityErrorCode::AiUnavailable;
                body.title = "AI provider not configured".into();
                body.setup_url = Some(AI_GATEWAY_SETTINGS_PATH.into());
                StatusCode::SERVICE_UNAVAILABLE
            }
            ActivityError::Provider {
                model,
                kind,
                status,
                retry_after_secs,
                ..
            } => {
                tracing::warn!(error = %error, "AI provider failed a visitor activity request");
                body.code = ActivityErrorCode::ProviderError;
                body.title = "AI provider error".into();
                body.provider_failure = Some((*kind).into());
                body.model = Some(model.clone());
                body.provider_status = *status;
                body.setup_url = Some(AI_GATEWAY_SETTINGS_PATH.into());
                match kind {
                    temps_ai::ProviderFailureKind::RateLimited
                    | temps_ai::ProviderFailureKind::BudgetExceeded => {
                        body.retry_after_secs = *retry_after_secs;
                        StatusCode::TOO_MANY_REQUESTS
                    }
                    temps_ai::ProviderFailureKind::Timeout => StatusCode::GATEWAY_TIMEOUT,
                    temps_ai::ProviderFailureKind::Authentication
                    | temps_ai::ProviderFailureKind::ModelNotFound
                    | temps_ai::ProviderFailureKind::ModelNotAllowed
                    | temps_ai::ProviderFailureKind::MalformedResponse
                    | temps_ai::ProviderFailureKind::Unreachable
                    | temps_ai::ProviderFailureKind::Misconfigured
                    | temps_ai::ProviderFailureKind::Rejected
                    | temps_ai::ProviderFailureKind::ProviderError => StatusCode::BAD_GATEWAY,
                }
            }
            ActivityError::SiteUnreadable { .. } => {
                body.code = ActivityErrorCode::SiteUnreadable;
                body.title = "Website could not be read".into();
                StatusCode::UNPROCESSABLE_ENTITY
            }
            ActivityError::InvalidModelOutput { model, .. } => {
                tracing::warn!(error = %error, "AI model returned unusable visitor activity output");
                body.code = ActivityErrorCode::InvalidModelOutput;
                body.title = "Unusable AI response".into();
                body.model = Some(model.clone());
                body.setup_url = Some(AI_GATEWAY_SETTINGS_PATH.into());
                StatusCode::BAD_GATEWAY
            }
            ActivityError::Database { .. } => {
                tracing::error!(error = %error, "Activity report request failed");
                StatusCode::INTERNAL_SERVER_ERROR
            }
            ActivityError::Analysis { .. } => {
                tracing::error!(error = %error, "Activity report request failed");
                StatusCode::BAD_GATEWAY
            }
        };
        activity_problem(status, body)
    }
}

/// Build the Problem from the typed body, so the fields documented in
/// `ActivityErrorBody` are exactly the ones sent.
fn activity_problem(status: StatusCode, body: ActivityErrorBody) -> Problem {
    let retry_after = body.retry_after_secs;
    let mut problem = problemdetails::new(status);
    if let Ok(serde_json::Value::Object(fields)) = serde_json::to_value(&body) {
        for (key, value) in fields {
            problem = problem.with_value(&key, value);
        }
    }
    match retry_after {
        Some(seconds) => problem.with_retry_after(seconds),
        None => problem,
    }
}

#[utoipa::path(get, path = "/projects/{project_id}/analytics/activity", tag = "Visitor Activity",
    params(("project_id" = i32, Path), ActivityStatusQuery), responses((status = 200, body = ActivityStatus),
        (status = 401, description = "Authentication required"), (status = 403, description = "Access denied"),
        (status = 404, description = "Project not found", body = ActivityErrorBody)), security(("bearer_auth" = [])))]
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
    responses((status = 204, description = "Settings saved"), (status = 400, description = "Invalid settings", body = ActivityErrorBody),
        (status = 401, description = "Authentication required"), (status = 403, description = "Access denied"),
        (status = 404, description = "Project not found", body = ActivityErrorBody),
        (status = 409, description = "A report run is in progress; `Retry-After` says when its lease ends at the latest", body = ActivityErrorBody)),
    security(("bearer_auth" = [])))]
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
    params(("project_id" = i32, Path)), responses((status = 200, body = ActivityReport),
        (status = 400, description = "Setup incomplete or sharing disabled", body = ActivityErrorBody),
        (status = 401, description = "Authentication required"), (status = 403, description = "Access denied"),
        (status = 404, description = "Project not found", body = ActivityErrorBody),
        (status = 409, description = "Analysis already running, or the run was superseded", body = ActivityErrorBody),
        (status = 429, description = "Cooling down after spending provider tokens, or rate-limited; see `Retry-After`", body = ActivityErrorBody),
        (status = 502, description = "The AI provider failed the request", body = ActivityErrorBody),
        (status = 503, description = "AI provider not configured, or analysis capacity busy", body = ActivityErrorBody),
        (status = 504, description = "The AI provider timed out", body = ActivityErrorBody)),
    security(("bearer_auth" = [])))]
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
    responses((status = 200, body = ActivityPreview), (status = 400, description = "Invalid goal or sharing disabled", body = ActivityErrorBody),
        (status = 401, description = "Authentication required"),
        (status = 403, description = "Access denied"), (status = 404, description = "Project not found", body = ActivityErrorBody),
        (status = 409, description = "An analysis is already running for this project", body = ActivityErrorBody),
        (status = 429, description = "Cooling down after spending provider tokens, or rate-limited; see `Retry-After`", body = ActivityErrorBody),
        (status = 502, description = "The AI provider failed or returned unusable output", body = ActivityErrorBody),
        (status = 503, description = "AI provider not configured, or analysis capacity busy", body = ActivityErrorBody),
        (status = 504, description = "The AI provider timed out", body = ActivityErrorBody)),
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
    responses((status = 200, body = ActivityGoals), (status = 400, description = "Invalid public URL or consent missing", body = ActivityErrorBody),
        (status = 401, description = "Authentication required"),
        (status = 403, description = "Access denied"), (status = 404, description = "Project not found", body = ActivityErrorBody),
        (status = 409, description = "An analysis is already running for this project", body = ActivityErrorBody),
        (status = 422, description = "The public website could not be read", body = ActivityErrorBody),
        (status = 429, description = "Cooling down after spending provider tokens, or rate-limited; see `Retry-After`", body = ActivityErrorBody),
        (status = 502, description = "The AI provider failed or returned unusable output", body = ActivityErrorBody),
        (status = 503, description = "AI provider not configured, or analysis capacity busy", body = ActivityErrorBody),
        (status = 504, description = "The AI provider timed out", body = ActivityErrorBody)), security(("bearer_auth" = [])))]
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
    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
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
    use temps_authz::context::AuthContext;
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
        async fn create_audit_log(&self, _: &dyn AuditOperation) -> temps_core::AuditResult<()> {
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
    async fn response_parts(problem: Problem) -> (StatusCode, Option<String>, serde_json::Value) {
        use axum::response::IntoResponse;
        let response = problem.into_response();
        let status = response.status();
        let retry_after = response
            .headers()
            .get(axum::http::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, retry_after, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn provider_payloads_and_internal_ids_never_reach_the_body() {
        let (status, retry_after, body) = response_parts(
            ActivityError::Analysis {
                project_id: 4242,
                reason: "private-provider-response".into(),
            }
            .into(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(retry_after, None);
        assert_eq!(body["code"], "internal");
        let text = body.to_string();
        assert!(!text.contains("private-provider-response"));
        assert!(!text.contains("4242"));
        let (status, _, body) = response_parts(
            ActivityError::Validation {
                project_id: 4242,
                reason: "Missing context".into(),
            }
            .into(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["detail"], "Missing context");
    }

    #[tokio::test]
    async fn a_running_analysis_is_409_with_retry_after_and_its_start() {
        let started_at = chrono::Utc::now() - chrono::Duration::seconds(30);
        let (status, retry_after, body) = response_parts(
            ActivityError::AnalysisRunning {
                project_id: 4242,
                project_name: "Sample site".into(),
                operation: ActivityOperation::WebsiteAnalysis,
                started_at,
                retry_after_secs: 60,
            }
            .into(),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(retry_after.as_deref(), Some("60"));
        assert_eq!(body["code"], "analysis_running");
        assert_eq!(body["operation"], "website_analysis");
        assert_eq!(body["retry_after_secs"], 60);
        assert_eq!(
            body["started_at"],
            serde_json::to_value(started_at).unwrap()
        );
        let detail = body["detail"].as_str().unwrap();
        assert!(detail.contains("Sample site"), "{detail}");
        assert!(!detail.contains("4242"), "{detail}");
    }

    #[tokio::test]
    async fn a_cooldown_is_429_with_retry_after_and_names_the_project() {
        let (status, retry_after, body) = response_parts(
            ActivityError::CoolingDown {
                project_id: 4242,
                project_name: "Sample site".into(),
                operation: ActivityOperation::Preview,
                retry_after_secs: 17,
            }
            .into(),
        )
        .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(retry_after.as_deref(), Some("17"));
        assert_eq!(body["code"], "cooling_down");
        assert_eq!(body["operation"], "preview");
        assert_eq!(body["retry_after_secs"], 17);
        let detail = body["detail"].as_str().unwrap();
        assert!(
            detail.contains("Sample site") && detail.contains("17s"),
            "{detail}"
        );
        assert!(!detail.contains("4242"), "{detail}");

        let (status, retry_after, body) = response_parts(
            ActivityError::ServerBusy {
                project_id: 4242,
                operation: ActivityOperation::Report,
                retry_after_secs: 15,
            }
            .into(),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(retry_after.as_deref(), Some("15"));
        assert_eq!(body["code"], "server_busy");
    }

    #[tokio::test]
    async fn provider_failures_are_typed_actionable_and_redacted() {
        let failure = |kind, status, retry_after_secs| ActivityError::Provider {
            project_id: 4242,
            model: "mock-flash".into(),
            kind,
            status,
            retry_after_secs,
            reason: "Incorrect API key provided: sk-test-1234; prompt: secret goal".into(),
        };
        let (status, retry_after, body) = response_parts(
            failure(
                temps_ai::ProviderFailureKind::ModelNotFound,
                Some(404),
                None,
            )
            .into(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(retry_after, None);
        assert_eq!(body["code"], "provider_error");
        assert_eq!(body["provider_failure"], "model_not_found");
        assert_eq!(body["model"], "mock-flash");
        assert_eq!(body["provider_status"], 404);
        assert_eq!(body["setup_url"], AI_GATEWAY_SETTINGS_PATH);
        assert_eq!(
            body["detail"],
            "The AI provider rejected the request: model 'mock-flash' not found — check AI Gateway settings."
        );
        let text = body.to_string();
        assert!(
            !text.contains("sk-test") && !text.contains("secret goal"),
            "{text}"
        );

        let (status, _, body) = response_parts(
            failure(
                temps_ai::ProviderFailureKind::Authentication,
                Some(401),
                None,
            )
            .into(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body["provider_failure"], "authentication");
        assert!(!body.to_string().contains("sk-test"));

        let (status, retry_after, body) = response_parts(
            failure(
                temps_ai::ProviderFailureKind::RateLimited,
                Some(429),
                Some(20),
            )
            .into(),
        )
        .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(retry_after.as_deref(), Some("20"));
        assert_eq!(body["provider_failure"], "rate_limited");

        let (status, _, body) =
            response_parts(failure(temps_ai::ProviderFailureKind::Timeout, None, None).into())
                .await;
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(body["provider_failure"], "timeout");

        let (status, _, body) =
            response_parts(ActivityError::Unavailable { project_id: 4242 }.into()).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["code"], "ai_unavailable");
        assert_eq!(body["setup_url"], AI_GATEWAY_SETTINGS_PATH);

        let (status, _, body) = response_parts(
            ActivityError::InvalidModelOutput {
                project_id: 4242,
                model: "mock-flash".into(),
                reason: "unknown field `secret`".into(),
            }
            .into(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body["code"], "invalid_model_output");
        assert!(!body.to_string().contains("secret"));

        let (status, _, body) = response_parts(
            ActivityError::SiteUnreadable {
                project_id: 4242,
                url: "https://example.com/".into(),
                reason: "Site returned HTTP 404 Not Found.".into(),
            }
            .into(),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["code"], "site_unreadable");
        assert!(body["detail"].as_str().unwrap().contains("HTTP 404"));
    }

    fn router() -> Router {
        let State(state) = state();
        routes().with_state(state)
    }

    async fn send(
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
        auth: Option<AuthContext>,
    ) -> StatusCode {
        use tower::ServiceExt;
        let mut request = axum::http::Request::builder().method(method).uri(uri);
        if body.is_some() {
            request = request.header("content-type", "application/json");
        }
        let mut request = request
            .body(match body {
                Some(body) => axum::body::Body::from(body.to_string()),
                None => axum::body::Body::empty(),
            })
            .unwrap();
        let Extension(metadata) = metadata();
        request.extensions_mut().insert(metadata);
        if let Some(auth) = auth {
            request.extensions_mut().insert(auth);
        }
        router().oneshot(request).await.unwrap().status()
    }

    fn requests() -> Vec<(&'static str, &'static str, Option<serde_json::Value>)> {
        vec![
            ("GET", "/projects/1/analytics/activity", None),
            (
                "PUT",
                "/projects/1/analytics/activity",
                Some(serde_json::to_value(ActivitySettings::default()).unwrap()),
            ),
            ("POST", "/projects/1/analytics/activity/run", None),
            (
                "POST",
                "/projects/1/analytics/activity/preview",
                Some(
                    serde_json::json!({"goal": "Understand readers", "share_activity_with_ai": true}),
                ),
            ),
            (
                "POST",
                "/projects/1/analytics/activity/goals",
                Some(serde_json::json!({"url": "https://example.com/", "share_with_ai": true})),
            ),
        ]
    }

    #[tokio::test]
    async fn every_activity_endpoint_requires_authentication() {
        for (method, uri, body) in requests() {
            assert_eq!(
                send(method, uri, body, None).await,
                StatusCode::UNAUTHORIZED,
                "{method} {uri}"
            );
        }
    }

    #[tokio::test]
    async fn analytics_readers_cannot_configure_or_spend_on_ai() {
        let reader = || {
            AuthContext::new_api_key(
                crate::ingest_keys::test_fixtures::user_model(7),
                None,
                Some(vec![temps_authz::Permission::AnalyticsRead]),
                "reader".into(),
                7,
            )
        };
        for (method, uri, body) in requests().into_iter().skip(1) {
            assert_eq!(
                send(method, uri, body, Some(reader())).await,
                StatusCode::FORBIDDEN,
                "{method} {uri}"
            );
        }
    }
}
