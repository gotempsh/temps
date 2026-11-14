// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{audit::*, audit_context, json_body, record_audit, total_pages, types::*};
use crate::{
    service::{page_bounds, StartRun, WorkflowGrantAction},
    store::Starter,
};
use axum::{
    body::Body,
    extract::{Extension, Path, Query, State},
    http::{HeaderMap, StatusCode},
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

#[derive(Deserialize, ToSchema)]
pub struct CreateWorkflowGrantRequest {
    pub source_environment_id: i32,
    pub action: WorkflowGrantAction,
    pub resource: String,
}
#[derive(Serialize, ToSchema)]
pub struct WorkflowGrantResponse {
    pub id: Uuid,
    pub source_environment_id: i32,
    pub target_environment_id: i32,
    pub action: String,
    pub resource: String,
    pub created_at: DateTime<Utc>,
}
impl From<temps_entities::workflow_grants::Model> for WorkflowGrantResponse {
    fn from(v: temps_entities::workflow_grants::Model) -> Self {
        Self {
            id: v.id,
            source_environment_id: v.source_environment_id,
            target_environment_id: v.target_environment_id,
            action: v.action,
            resource: v.resource,
            created_at: v.created_at,
        }
    }
}
#[derive(Serialize, ToSchema)]
pub struct WorkflowGrantListResponse {
    pub grants: Vec<WorkflowGrantResponse>,
    pub page: u64,
    pub page_size: u64,
    pub total: u64,
    pub total_pages: u64,
}

#[utoipa::path(get,path="/projects/{project_id}/environments/{environment_id}/workflows/grants",tag="Workflows",params(("project_id"=i32,Path),("environment_id"=i32,Path),PageQuery),responses((status=200,body=WorkflowGrantListResponse,description="Incoming grants"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Environment not found"),(status=500,description="Database failure")),security(("bearer_auth"=[])))]
pub async fn list_workflow_grants(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WorkflowsAppState>>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
    Query(query): Query<PageQuery>,
) -> Result<Json<WorkflowGrantListResponse>, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        WorkflowsRead,
        project_id,
        &state.project_access_checker
    );
    let (page, page_size) = page_bounds(query.page, query.page_size);
    let (grants, total) = state
        .service
        .workflow_grants(project_id, environment_id, page, page_size)
        .await?;
    Ok(Json(WorkflowGrantListResponse {
        grants: grants.into_iter().map(Into::into).collect(),
        page,
        page_size,
        total,
        total_pages: total_pages(total, page_size),
    }))
}
#[utoipa::path(post,path="/projects/{project_id}/environments/{environment_id}/workflows/grants",tag="Workflows",params(("project_id"=i32,Path),("environment_id"=i32,Path)),request_body=CreateWorkflowGrantRequest,responses((status=200,body=WorkflowGrantResponse,description="Grant created or already exists"),(status=400,description="Invalid grant"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Environment not found"),(status=500,description="Database failure")),security(("bearer_auth"=[])))]
pub async fn create_workflow_grant(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WorkflowsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
    Json(request): Json<CreateWorkflowGrantRequest>,
) -> Result<Json<WorkflowGrantResponse>, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        WorkflowsWrite,
        project_id,
        &state.project_access_checker
    );
    let grant = state
        .service
        .grant_workflow_access(
            project_id,
            environment_id,
            request.source_environment_id,
            request.action,
            &request.resource,
        )
        .await?;
    record_audit(
        &state,
        &WorkflowGrantChangedAudit {
            context: audit_context(auth.user_id_opt(), None, Some(&metadata)),
            project_id,
            environment_id,
            grant_id: grant.id,
            revoked: false,
        },
    )
    .await;
    Ok(Json(grant.into()))
}
#[utoipa::path(delete,path="/projects/{project_id}/environments/{environment_id}/workflows/grants/{grant_id}",tag="Workflows",params(("project_id"=i32,Path),("environment_id"=i32,Path),("grant_id"=Uuid,Path)),responses((status=204,description="Grant revoked; in-flight authorized operations may finish"),(status=401,description="Unauthorized"),(status=403,description="Forbidden"),(status=404,description="Environment not found"),(status=500,description="Database failure")),security(("bearer_auth"=[])))]
pub async fn revoke_workflow_grant(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WorkflowsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, environment_id, grant_id)): Path<(i32, i32, Uuid)>,
) -> Result<StatusCode, Problem> {
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_permission_guard!(
        auth,
        WorkflowsWrite,
        project_id,
        &state.project_access_checker
    );
    if state
        .service
        .revoke_workflow_grant(project_id, environment_id, grant_id)
        .await?
    {
        record_audit(
            &state,
            &WorkflowGrantChangedAudit {
                context: audit_context(auth.user_id_opt(), None, Some(&metadata)),
                project_id,
                environment_id,
                grant_id,
                revoked: true,
            },
        )
        .await;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post,path="/workflows/v1/projects/{project_id}/environments/{environment_id}/runs",tag="Workflow App API",params(("project_id"=i32,Path),("environment_id"=i32,Path)),request_body=StartWorkflowRunRequest,responses((status=201,body=AppStartWorkflowRunResponse,description="Started through grant"),(status=200,body=AppStartWorkflowRunResponse,description="Idempotent replay"),(status=400,description="Invalid input"),(status=401,description="Invalid credential"),(status=403,description="Grant required"),(status=413,description="Payload too large"),(status=429,description="Quota exceeded")),security(("bearer_auth"=[])))]
pub async fn app_start_granted_workflow(
    State(state): State<Arc<WorkflowsAppState>>,
    metadata: Option<Extension<RequestMetadata>>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
    headers: HeaderMap,
    body: Body,
) -> Result<(StatusCode, Json<AppStartWorkflowRunResponse>), Problem> {
    let credential = super::app::credential(&state, &headers).await?;
    let request: StartWorkflowRunRequest = json_body(body).await?;
    state
        .service
        .authorize_workflow_target(
            &credential,
            project_id,
            environment_id,
            WorkflowGrantAction::Start,
            &request.workflow,
        )
        .await?;
    let (run, created) = state
        .service
        .start(
            project_id,
            environment_id,
            StartRun {
                workflow: request.workflow,
                input: request.input,
                idempotency_key: request.idempotency_key,
            },
            Starter::Credential(credential.id),
        )
        .await?;
    if created {
        record_audit(
            &state,
            &WorkflowRunStartedAudit {
                context: audit_context(
                    None,
                    Some(credential.id),
                    metadata.as_ref().map(|Extension(m)| m),
                ),
                project_id,
                environment_id,
                run_id: run.id,
                workflow: run.workflow_name.clone(),
            },
        )
        .await;
    }
    Ok((
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(AppStartWorkflowRunResponse {
            run: super::app::app_run(run, None, None),
            created,
        }),
    ))
}
#[utoipa::path(post,path="/workflows/v1/projects/{project_id}/environments/{environment_id}/events",tag="Workflow App API",params(("project_id"=i32,Path),("environment_id"=i32,Path)),request_body=SendWorkflowEventRequest,responses((status=200,body=SendWorkflowEventResponse,description="Event delivered through grant"),(status=400,description="Invalid event"),(status=401,description="Invalid credential"),(status=403,description="Grant required"),(status=413,description="Payload too large")),security(("bearer_auth"=[])))]
pub async fn app_send_granted_event(
    State(state): State<Arc<WorkflowsAppState>>,
    metadata: Option<Extension<RequestMetadata>>,
    Path((project_id, environment_id)): Path<(i32, i32)>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<SendWorkflowEventResponse>, Problem> {
    let credential = super::app::credential(&state, &headers).await?;
    let request: SendWorkflowEventRequest = json_body(body).await?;
    state
        .service
        .authorize_workflow_target(
            &credential,
            project_id,
            environment_id,
            WorkflowGrantAction::Event,
            &request.name,
        )
        .await?;
    let (event_id, delivery) = state
        .service
        .send_event(
            project_id,
            environment_id,
            &request.name,
            request.key.as_deref(),
            request.payload.as_ref(),
            "credential",
        )
        .await?;
    record_audit(
        &state,
        &WorkflowEventSentAudit {
            context: audit_context(
                None,
                Some(credential.id),
                metadata.as_ref().map(|Extension(m)| m),
            ),
            project_id,
            environment_id,
            event_id,
            event_name: request.name,
            delivered: delivery.delivered,
        },
    )
    .await;
    Ok(Json(SendWorkflowEventResponse {
        event_id,
        delivered: delivery.delivered,
    }))
}
