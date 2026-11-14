// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{
    audit::WorkflowKeyShreddedAudit,
    audit_context, record_audit, total_pages,
    types::{PageQuery, WorkflowsAppState},
};
use crate::{error::WorkflowError, service::page_bounds};
use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_authz::{
    deny_deployment_token, project_permission_guard, project_scope_guard, RequireAuth,
};
use temps_core::{problemdetails::Problem, RequestMetadata};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Serialize, ToSchema)]
pub struct WorkflowProjectKeyResponse {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub shredded_at: Option<DateTime<Utc>>,
}
#[derive(Serialize, ToSchema)]
pub struct WorkflowProjectKeysResponse {
    pub keys: Vec<WorkflowProjectKeyResponse>,
    pub page: u64,
    pub page_size: u64,
    pub total: u64,
    pub total_pages: u64,
}
#[derive(Deserialize, ToSchema)]
pub struct ShredWorkflowKeyRequest {
    /// Exact key UUID, confirming that its retained payloads become unreadable.
    pub confirm_key_id: Uuid,
}

#[utoipa::path(get, path="/projects/{project_id}/workflow-keys", tag="Workflows",
    params(("project_id"=i32,Path),PageQuery),
    responses((status=200,description="Key metadata only; never wrapped key material",body=WorkflowProjectKeysResponse),
        (status=401,description="Unauthorized"),(status=403,description="Insufficient permissions"),(status=500,description="Database failure")),security(("bearer_auth"=[])))]
pub async fn list_workflow_project_keys(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WorkflowsAppState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<PageQuery>,
) -> Result<Json<WorkflowProjectKeysResponse>, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        WorkflowsRead,
        project_id,
        &state.project_access_checker
    );
    let (page, page_size) = page_bounds(query.page, query.page_size);
    let (keys, total) = state
        .service
        .project_key_history(project_id, page, page_size)
        .await?;
    Ok(Json(WorkflowProjectKeysResponse {
        keys: keys
            .into_iter()
            .map(|key| WorkflowProjectKeyResponse {
                id: key.id,
                created_at: key.created_at,
                shredded_at: key.shredded_at,
            })
            .collect(),
        page,
        page_size,
        total,
        total_pages: total_pages(total, page_size),
    }))
}

#[utoipa::path(post,path="/projects/{project_id}/workflow-keys/{key_id}/shred",tag="Workflows",
    params(("project_id"=i32,Path),("key_id"=Uuid,Path)),request_body=ShredWorkflowKeyRequest,
    responses((status=204,description="Wrapped key erased; new payloads get a new key. Backups may retain the old key."),
    (status=400,description="Confirmation mismatch"),(status=401,description="Unauthorized"),(status=403,description="Insufficient permissions"),
    (status=410,description="Key unavailable"),(status=500,description="Database failure")),security(("bearer_auth"=[])))]
pub async fn shred_workflow_project_key(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WorkflowsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, key_id)): Path<(i32, Uuid)>,
    Json(request): Json<ShredWorkflowKeyRequest>,
) -> Result<StatusCode, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        WorkflowsWrite,
        project_id,
        &state.project_access_checker
    );
    project_permission_guard!(
        auth,
        WorkflowRunsDelete,
        project_id,
        &state.project_access_checker
    );
    if request.confirm_key_id != key_id {
        return Err(WorkflowError::Validation {
            field: "confirm_key_id",
            reason: "must match the key being permanently shredded".into(),
        }
        .into());
    }
    state.service.shred_project_key(project_id, key_id).await?;
    record_audit(
        &state,
        &WorkflowKeyShreddedAudit {
            context: audit_context(auth.user_id_opt(), None, Some(&metadata)),
            project_id,
            key_id,
        },
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}
