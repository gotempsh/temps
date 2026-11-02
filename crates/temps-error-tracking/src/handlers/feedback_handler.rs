// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
use super::types::AppState;
use crate::services::feedback_service::{FeedbackPage, FeedbackView};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::get,
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_authz::{permission_guard, project_access_guard, project_scope_guard, RequireAuth};
use temps_core::{problemdetails::Problem, AuditContext, AuditOperation, RequestMetadata};
use utoipa::{IntoParams, OpenApi, ToSchema};

#[derive(OpenApi)]
#[openapi(paths(list_project_feedback,get_project_feedback,delete_project_feedback,update_project_feedback),components(schemas(FeedbackPage,FeedbackView,FeedbackQuery,FeedbackStatusRequest)),tags((name="feedback",description="Native project feedback inbox")))]
pub struct FeedbackApiDoc;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/projects/{project_id}/feedback",
            get(list_project_feedback),
        )
        .route(
            "/projects/{project_id}/feedback/{feedback_id}",
            get(get_project_feedback)
                .patch(update_project_feedback)
                .delete(delete_project_feedback),
        )
}

#[derive(Deserialize, IntoParams, ToSchema)]
pub struct FeedbackQuery {
    pub status: Option<String>,
    pub page: Option<u64>,
    pub page_size: Option<u64>,
}
#[derive(Deserialize, ToSchema)]
pub struct FeedbackStatusRequest {
    pub status: String,
}

#[utoipa::path(get,path="/projects/{project_id}/feedback",params(("project_id"=i32,Path),FeedbackQuery),responses((status=200,body=FeedbackPage),(status=400,description="Invalid filter"),(status=401,description="Authentication required"),(status=403,description="Project access required")),tag="feedback")]
pub async fn list_project_feedback(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path(project_id): Path<i32>,
    Query(query): Query<FeedbackQuery>,
) -> Result<Json<FeedbackPage>, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    Ok(Json(
        state
            .error_tracking_service
            .feedback
            .list(
                project_id,
                query.status,
                query.page.unwrap_or(1),
                query.page_size.unwrap_or(20),
            )
            .await?,
    ))
}

#[derive(Serialize)]
struct FeedbackTriagedAudit {
    context: AuditContext,
    project_id: i32,
    feedback_id: i64,
    status: String,
}
impl AuditOperation for FeedbackTriagedAudit {
    fn operation_type(&self) -> String {
        "FEEDBACK_TRIAGED".into()
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

#[utoipa::path(patch,path="/projects/{project_id}/feedback/{feedback_id}",params(("project_id"=i32,Path),("feedback_id"=i64,Path)),request_body=FeedbackStatusRequest,responses((status=204,description="Feedback status updated"),(status=400,description="Invalid status"),(status=404,description="Feedback not found")),tag="feedback")]
pub async fn update_project_feedback(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, feedback_id)): Path<(i32, i64)>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<FeedbackStatusRequest>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    state
        .error_tracking_service
        .feedback
        .set_status(project_id, feedback_id, &request.status)
        .await?;
    let audit = FeedbackTriagedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address),
            user_agent: metadata.user_agent,
        },
        project_id,
        feedback_id,
        status: request.status,
    };
    if state.audit_service.create_audit_log(&audit).await.is_err() {
        tracing::error!(
            project_id,
            feedback_id,
            error_class = "audit_storage",
            "Feedback triage audit failed"
        );
    }
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get,path="/projects/{project_id}/feedback/{feedback_id}",params(("project_id"=i32,Path),("feedback_id"=i64,Path)),responses((status=200,body=FeedbackView),(status=404,description="Feedback not found")),tag="feedback")]
pub async fn get_project_feedback(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, feedback_id)): Path<(i32, i64)>,
) -> Result<Json<FeedbackView>, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    Ok(Json(
        state
            .error_tracking_service
            .feedback
            .get(project_id, feedback_id)
            .await?,
    ))
}

#[utoipa::path(delete,path="/projects/{project_id}/feedback/{feedback_id}",params(("project_id"=i32,Path),("feedback_id"=i64,Path)),responses((status=204,description="Feedback permanently deleted"),(status=404,description="Feedback not found")),tag="feedback")]
pub async fn delete_project_feedback(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, feedback_id)): Path<(i32, i64)>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    state
        .error_tracking_service
        .feedback
        .delete(project_id, feedback_id)
        .await?;
    let audit = FeedbackTriagedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address),
            user_agent: metadata.user_agent,
        },
        project_id,
        feedback_id,
        status: "deleted".into(),
    };
    if state.audit_service.create_audit_log(&audit).await.is_err() {
        tracing::error!(
            project_id,
            feedback_id,
            error_class = "audit_storage",
            "Feedback delete audit failed"
        );
    }
    Ok(StatusCode::NO_CONTENT)
}
