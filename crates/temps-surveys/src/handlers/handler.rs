// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Survey management endpoints for the console, CLI and API keys.

use std::sync::Arc;

use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use temps_authz::{
    deny_deployment_token, project_permission_guard, project_scope_guard, RequireAuth,
};
use temps_core::problemdetails::{self, Problem};
use temps_core::RequestMetadata;
use tracing::error;
use utoipa::OpenApi;

use super::audit::{AuditContext, SurveyCreatedAudit, SurveyDeletedAudit, SurveyUpdatedAudit};
use super::public::{
    PublicSurvey, PublicSurveyDisplay, PublicSurveysRequest, PublicSurveysResponse,
};
use super::types::*;
use crate::error::SurveyError;
use crate::model::{
    QuestionKind, SurveyFlagCondition, SurveyQuestion, SurveyStatus, SurveyTargeting,
};
use crate::services::results::{AnswerCount, AnswerValue, NpsSummary, QuestionResults};
use crate::services::{CreateSurvey, ResultsWindow, UpdateSurvey};

#[derive(OpenApi)]
#[openapi(
    paths(
        list_surveys,
        create_survey,
        get_survey,
        update_survey,
        delete_survey,
        get_survey_results,
        list_survey_responses,
        super::public::public_surveys,
    ),
    components(schemas(
        CreateSurveyRequest,
        UpdateSurveyRequest,
        SurveyResponse,
        SurveyListResponse,
        SurveyResultsResponse,
        SurveyAnswerSet,
        SurveyAnswerListResponse,
        DeleteSurveyResponse,
        SurveyStatus,
        QuestionKind,
        SurveyQuestion,
        SurveyTargeting,
        SurveyFlagCondition,
        QuestionResults,
        AnswerCount,
        AnswerValue,
        NpsSummary,
        PublicSurveysRequest,
        PublicSurveysResponse,
        PublicSurvey,
        PublicSurveyDisplay,
    )),
    tags((name = "Surveys", description = "In-app surveys shown by the browser SDK; answers are analytics events"))
)]
pub struct SurveysApiDoc;

pub fn configure_routes() -> Router<Arc<SurveysAppState>> {
    Router::new()
        .route(
            "/projects/{project_id}/surveys",
            get(list_surveys).post(create_survey),
        )
        .route(
            "/projects/{project_id}/surveys/{survey_id}",
            get(get_survey).patch(update_survey).delete(delete_survey),
        )
        .route(
            "/projects/{project_id}/surveys/{survey_id}/results",
            get(get_survey_results),
        )
        .route(
            "/projects/{project_id}/surveys/{survey_id}/responses",
            get(list_survey_responses),
        )
}

impl From<SurveyError> for Problem {
    fn from(error: SurveyError) -> Self {
        match error {
            SurveyError::NotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Survey Not Found")
                .with_detail(error.to_string()),
            SurveyError::Validation { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Survey")
                .with_detail(error.to_string()),
            SurveyError::InvalidQuery { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Results Query")
                .with_detail(error.to_string()),
            SurveyError::StaleWrite { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Survey Changed")
                .with_detail(error.to_string()),
            SurveyError::Corrupt { .. } => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Unreadable Survey")
                .with_detail(error.to_string()),
            SurveyError::Serialization { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Survey Serialization Error")
                    .with_detail(error.to_string())
            }
            SurveyError::Database(_) => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Internal Server Error")
                .with_detail(error.to_string()),
        }
    }
}

fn audit_context(auth: &temps_authz::AuthContext, metadata: &RequestMetadata) -> AuditContext {
    AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    }
}

fn parse_time(
    field: &str,
    raw: Option<&str>,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, Problem> {
    raw.map(|text| {
        chrono::DateTime::parse_from_rfc3339(text)
            .map(|parsed| parsed.with_timezone(&chrono::Utc))
            .map_err(|e| {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid Timestamp")
                    .with_detail(format!(
                        "{field} '{text}' is not an RFC 3339 timestamp: {e}"
                    ))
            })
    })
    .transpose()
}

fn window(query: &ResultsQuery) -> Result<ResultsWindow, Problem> {
    Ok(ResultsWindow {
        start: parse_time("start_date", query.start_date.as_deref())?,
        end: parse_time("end_date", query.end_date.as_deref())?,
        environment_id: query.environment_id,
    })
}

fn targeting_json(targeting: &SurveyTargeting) -> serde_json::Value {
    serde_json::to_value(targeting).unwrap_or(serde_json::Value::Null)
}

#[utoipa::path(
    tag = "Surveys",
    get,
    path = "/projects/{project_id}/surveys",
    params(("project_id" = i32, Path, description = "Project ID"), ListSurveysQuery),
    responses(
        (status = 200, description = "Surveys, newest first", body = SurveyListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions (requires surveys:read)"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_surveys(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SurveysAppState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<ListSurveysQuery>,
) -> Result<impl IntoResponse, Problem> {
    project_scope_guard!(auth, project_id);
    project_permission_guard!(auth, SurveysRead, project_id, &state.project_access_checker);

    let (surveys, total) = state
        .survey_service
        .list(project_id, query.status, query.page, query.page_size)
        .await?;
    let (page, page_size) = crate::services::normalize_pagination(query.page, query.page_size);
    Ok(Json(SurveyListResponse {
        surveys: surveys.into_iter().map(SurveyResponse::from).collect(),
        total,
        page,
        page_size,
        total_pages: total_pages(total, page_size),
    }))
}

#[utoipa::path(
    tag = "Surveys",
    post,
    path = "/projects/{project_id}/surveys",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body = CreateSurveyRequest,
    responses(
        (status = 201, description = "Survey created", body = SurveyResponse),
        (status = 400, description = "Invalid survey"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions (requires surveys:write)"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_survey(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SurveysAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(request): Json<CreateSurveyRequest>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        SurveysWrite,
        project_id,
        &state.project_access_checker
    );

    let survey = state
        .survey_service
        .create(
            project_id,
            CreateSurvey {
                name: request.name,
                description: request.description,
                status: request.status,
                questions: request.questions,
                targeting: request.targeting,
            },
        )
        .await?;

    let audit = SurveyCreatedAudit {
        context: audit_context(&auth, &metadata),
        project_id,
        survey_id: survey.id,
        name: survey.name.clone(),
        status: survey.status.as_str().to_string(),
        question_count: survey.questions.len(),
        targeting: targeting_json(&survey.targeting),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(
            "Failed to create audit log for survey {} creation in project {}: {}",
            survey.id, project_id, e
        );
    }

    Ok((StatusCode::CREATED, Json(SurveyResponse::from(survey))))
}

#[utoipa::path(
    tag = "Surveys",
    get,
    path = "/projects/{project_id}/surveys/{survey_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("survey_id" = i32, Path, description = "Survey ID")
    ),
    responses(
        (status = 200, description = "The survey", body = SurveyResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions (requires surveys:read)"),
        (status = 404, description = "Survey not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_survey(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SurveysAppState>>,
    Path((project_id, survey_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    project_scope_guard!(auth, project_id);
    project_permission_guard!(auth, SurveysRead, project_id, &state.project_access_checker);

    let survey = state.survey_service.get(project_id, survey_id).await?;
    Ok(Json(SurveyResponse::from(survey)))
}

#[utoipa::path(
    tag = "Surveys",
    patch,
    path = "/projects/{project_id}/surveys/{survey_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("survey_id" = i32, Path, description = "Survey ID")
    ),
    request_body = UpdateSurveyRequest,
    responses(
        (status = 200, description = "Survey updated", body = SurveyResponse),
        (status = 400, description = "Invalid survey"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions (requires surveys:write)"),
        (status = 404, description = "Survey not found"),
        (status = 409, description = "The survey changed since expected_updated_at"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_survey(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SurveysAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, survey_id)): Path<(i32, i32)>,
    Json(request): Json<UpdateSurveyRequest>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        SurveysWrite,
        project_id,
        &state.project_access_checker
    );

    let expected_updated_at = parse_time(
        "expected_updated_at",
        request.expected_updated_at.as_deref(),
    )?;
    let (before, after) = state
        .survey_service
        .update(
            project_id,
            survey_id,
            UpdateSurvey {
                name: request.name,
                description: request.description,
                status: request.status,
                questions: request.questions,
                targeting: request.targeting,
                expected_updated_at,
            },
        )
        .await?;

    let audit = SurveyUpdatedAudit {
        context: audit_context(&auth, &metadata),
        project_id,
        survey_id,
        name: after.name.clone(),
        old_status: before.status.as_str().to_string(),
        new_status: after.status.as_str().to_string(),
        old_question_count: before.questions.len(),
        new_question_count: after.questions.len(),
        old_targeting: targeting_json(&before.targeting),
        new_targeting: targeting_json(&after.targeting),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(
            "Failed to create audit log for survey {} update in project {}: {}",
            survey_id, project_id, e
        );
    }

    Ok(Json(SurveyResponse::from(after)))
}

#[utoipa::path(
    tag = "Surveys",
    delete,
    path = "/projects/{project_id}/surveys/{survey_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("survey_id" = i32, Path, description = "Survey ID")
    ),
    responses(
        (status = 200, description = "Survey deleted; its answers stay in analytics", body = DeleteSurveyResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions (requires surveys:write)"),
        (status = 404, description = "Survey not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_survey(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SurveysAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, survey_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        SurveysWrite,
        project_id,
        &state.project_access_checker
    );

    let survey = state.survey_service.delete(project_id, survey_id).await?;

    let audit = SurveyDeletedAudit {
        context: audit_context(&auth, &metadata),
        project_id,
        survey_id,
        name: survey.name.clone(),
        status: survey.status.as_str().to_string(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(
            "Failed to create audit log for survey {} deletion in project {}: {}",
            survey_id, project_id, e
        );
    }

    Ok(Json(DeleteSurveyResponse {
        id: survey_id,
        deleted: true,
    }))
}

#[utoipa::path(
    tag = "Surveys",
    get,
    path = "/projects/{project_id}/surveys/{survey_id}/results",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("survey_id" = i32, Path, description = "Survey ID"),
        ResultsQuery
    ),
    responses(
        (status = 200, description = "Response rate and per-question results", body = SurveyResultsResponse),
        (status = 400, description = "Invalid time window"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions (requires surveys:read)"),
        (status = 404, description = "Survey not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_survey_results(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SurveysAppState>>,
    Path((project_id, survey_id)): Path<(i32, i32)>,
    Query(query): Query<ResultsQuery>,
) -> Result<impl IntoResponse, Problem> {
    project_scope_guard!(auth, project_id);
    project_permission_guard!(auth, SurveysRead, project_id, &state.project_access_checker);

    let (_, results) = state
        .survey_service
        .results(project_id, survey_id, window(&query)?)
        .await?;
    Ok(Json(SurveyResultsResponse::new(survey_id, results)))
}

#[utoipa::path(
    tag = "Surveys",
    get,
    path = "/projects/{project_id}/surveys/{survey_id}/responses",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("survey_id" = i32, Path, description = "Survey ID"),
        ResultsQuery
    ),
    responses(
        (status = 200, description = "Submitted responses, newest first", body = SurveyAnswerListResponse),
        (status = 400, description = "Invalid time window"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions (requires surveys:read)"),
        (status = 404, description = "Survey not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_survey_responses(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SurveysAppState>>,
    Path((project_id, survey_id)): Path<(i32, i32)>,
    Query(query): Query<ResultsQuery>,
) -> Result<impl IntoResponse, Problem> {
    project_scope_guard!(auth, project_id);
    project_permission_guard!(auth, SurveysRead, project_id, &state.project_access_checker);

    let (responses, total) = state
        .survey_service
        .responses(
            project_id,
            survey_id,
            window(&query)?,
            query.page,
            query.page_size,
        )
        .await?;
    let (page, page_size) = crate::services::normalize_pagination(query.page, query.page_size);
    Ok(Json(SurveyAnswerListResponse {
        responses: responses.into_iter().map(SurveyAnswerSet::from).collect(),
        total,
        page,
        page_size,
        total_pages: total_pages(total, page_size),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::SurveyService;
    use axum::http::HeaderMap;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_authz::context::AuthContext;
    use temps_authz::permissions::{Permission, Role};
    use temps_entities::deployment_tokens::DeploymentTokenPermission;
    use temps_entities::users;

    struct NoopAuditLogger;

    #[async_trait::async_trait]
    impl temps_core::AuditLogger for NoopAuditLogger {
        async fn create_audit_log(
            &self,
            _operation: &dyn temps_core::AuditOperation,
        ) -> temps_core::AuditResult<()> {
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

    fn app_state() -> Arc<SurveysAppState> {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        Arc::new(SurveysAppState {
            survey_service: Arc::new(SurveyService::new(db)),
            audit_service: Arc::new(NoopAuditLogger),
            project_access_checker: None,
        })
    }

    fn create_body() -> CreateSurveyRequest {
        CreateSurveyRequest {
            name: "NPS".into(),
            description: None,
            status: SurveyStatus::Draft,
            questions: Vec::new(),
            targeting: SurveyTargeting::default(),
        }
    }

    async fn create_rejection(auth: AuthContext, project_id: i32) -> Option<StatusCode> {
        create_survey(
            RequireAuth(auth),
            State(app_state()),
            Extension(test_metadata()),
            Path(project_id),
            Json(create_body()),
        )
        .await
        .err()
        .map(|p| p.status_code)
    }

    async fn delete_rejection(auth: AuthContext) -> Option<StatusCode> {
        delete_survey(
            RequireAuth(auth),
            State(app_state()),
            Extension(test_metadata()),
            Path((1, 1)),
        )
        .await
        .err()
        .map(|p| p.status_code)
    }

    async fn results_rejection(auth: AuthContext, project_id: i32) -> Option<StatusCode> {
        get_survey_results(
            RequireAuth(auth),
            State(app_state()),
            Path((project_id, 1)),
            Query(ResultsQuery {
                start_date: None,
                end_date: None,
                environment_id: None,
                page: None,
                page_size: None,
            }),
        )
        .await
        .err()
        .map(|p| p.status_code)
    }

    fn deployment_token(project_id: i32) -> AuthContext {
        AuthContext::new_deployment_token(
            project_id,
            Some(1),
            None,
            1,
            "app".to_string(),
            vec![DeploymentTokenPermission::FullAccess],
        )
    }

    #[tokio::test]
    async fn a_reader_can_read_results_but_cannot_write() {
        let reader = || AuthContext::new_session(test_user(), Role::Reader);
        assert_eq!(
            create_rejection(reader(), 1).await,
            Some(StatusCode::FORBIDDEN)
        );
        assert_eq!(
            delete_rejection(reader()).await,
            Some(StatusCode::FORBIDDEN)
        );
        assert_ne!(
            results_rejection(reader(), 1).await,
            Some(StatusCode::FORBIDDEN),
            "Reader holds surveys:read"
        );
    }

    #[tokio::test]
    async fn a_user_may_create_surveys() {
        // Passes the guards and reaches the (empty) mock database, which is a
        // 500, not a 403.
        assert_ne!(
            create_rejection(AuthContext::new_session(test_user(), Role::User), 1).await,
            Some(StatusCode::FORBIDDEN)
        );
    }

    #[tokio::test]
    async fn a_read_only_key_cannot_write() {
        let key = || {
            AuthContext::new_api_key(
                test_user(),
                None,
                Some(vec![Permission::SurveysRead]),
                "surveys-read-key".to_string(),
                1,
            )
        };
        assert_eq!(
            create_rejection(key(), 1).await,
            Some(StatusCode::FORBIDDEN)
        );
        assert_ne!(
            results_rejection(key(), 1).await,
            Some(StatusCode::FORBIDDEN)
        );
    }

    #[tokio::test]
    async fn deployment_tokens_cannot_write_or_read_surveys() {
        assert_eq!(
            create_rejection(deployment_token(1), 1).await,
            Some(StatusCode::FORBIDDEN)
        );
        // No deployment-token permission maps to surveys:read.
        assert_eq!(
            results_rejection(deployment_token(1), 1).await,
            Some(StatusCode::FORBIDDEN)
        );
        // And never across projects.
        assert_eq!(
            results_rejection(deployment_token(2), 1).await,
            Some(StatusCode::FORBIDDEN)
        );
    }

    #[test]
    fn every_error_maps_to_a_status() {
        let cases = [
            (
                SurveyError::NotFound {
                    project_id: 1,
                    survey_id: 2,
                },
                StatusCode::NOT_FOUND,
            ),
            (
                SurveyError::Validation {
                    name: "x".into(),
                    message: "bad".into(),
                },
                StatusCode::BAD_REQUEST,
            ),
            (
                SurveyError::InvalidQuery {
                    survey_id: 2,
                    message: "bad".into(),
                },
                StatusCode::BAD_REQUEST,
            ),
            (
                SurveyError::StaleWrite {
                    project_id: 1,
                    survey_id: 2,
                },
                StatusCode::CONFLICT,
            ),
        ];
        for (error, status) in cases {
            let detail = error.to_string();
            let problem = Problem::from(error);
            assert_eq!(problem.status_code, status, "{detail}");
        }
    }

    #[test]
    fn a_bad_timestamp_is_a_400_naming_the_field() {
        let error = window(&ResultsQuery {
            start_date: Some("yesterday".into()),
            end_date: None,
            environment_id: None,
            page: None,
            page_size: None,
        })
        .unwrap_err();
        assert_eq!(error.status_code, StatusCode::BAD_REQUEST);
    }
}
