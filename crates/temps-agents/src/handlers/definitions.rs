use axum::{
    extract::{DefaultBodyLimit, Multipart, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::ToSchema;

use temps_auth::{permission_guard, project_access_guard, project_scope_guard, RequireAuth};
use temps_core::audit::{AuditContext, AuditOperation};
use temps_core::problemdetails::{self, Problem};
use temps_core::RequestMetadata;

use crate::handlers::AppState;
use crate::services::definition_service::{
    CreateMcpDefinitionRequest, CreateSkillDefinitionRequest, UpdateMcpDefinitionRequest,
    UpdateSkillDefinitionRequest,
};

// ── Response DTOs ───────────────────────────────────────────────────────────

#[derive(Debug, Serialize, ToSchema)]
pub struct SkillDefinitionResponse {
    pub id: i32,
    pub project_id: Option<i32>,
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub content: String,
    pub has_archive: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// Concrete list wrapper for skill definitions (utoipa requires non-generic types).
#[derive(Debug, Serialize, ToSchema)]
pub struct ListSkillsResponse {
    pub items: Vec<SkillDefinitionResponse>,
    pub total: usize,
}

/// Concrete list wrapper for MCP server definitions (utoipa requires non-generic types).
#[derive(Debug, Serialize, ToSchema)]
pub struct ListMcpsResponse {
    pub items: Vec<McpDefinitionResponse>,
    pub total: usize,
}

impl From<temps_entities::project_skill_definitions::Model> for SkillDefinitionResponse {
    fn from(m: temps_entities::project_skill_definitions::Model) -> Self {
        let has_archive = m.archive.is_some();
        Self {
            id: m.id,
            project_id: m.project_id,
            slug: m.slug,
            name: m.name,
            description: m.description,
            content: m.content,
            has_archive,
            created_at: m.created_at.to_rfc3339(),
            updated_at: m.updated_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct McpDefinitionResponse {
    pub id: i32,
    pub project_id: Option<i32>,
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    #[schema(value_type = Object)]
    pub config: serde_json::Value,
    pub created_at: String,
    pub updated_at: String,
}

impl From<temps_entities::project_mcp_definitions::Model> for McpDefinitionResponse {
    fn from(m: temps_entities::project_mcp_definitions::Model) -> Self {
        Self {
            id: m.id,
            project_id: m.project_id,
            slug: m.slug,
            name: m.name,
            description: m.description,
            config: m.config,
            created_at: m.created_at.to_rfc3339(),
            updated_at: m.updated_at.to_rfc3339(),
        }
    }
}

// ── Request DTOs (re-export for OpenAPI) ────────────────────────────────────

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateSkillRequest {
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub content: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateSkillRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub content: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateMcpRequest {
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    #[schema(value_type = Object)]
    pub config: serde_json::Value,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateMcpRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    #[schema(value_type = Object)]
    pub config: Option<serde_json::Value>,
}

// ── Routes ──────────────────────────────────────────────────────────────────

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        // Project-scoped skills
        .route(
            "/projects/{project_id}/skills",
            get(list_skills).post(create_skill),
        )
        .route(
            "/projects/{project_id}/skills/upload",
            post(upload_skill).layer(DefaultBodyLimit::max(50 * 1024 * 1024)),
        )
        .route(
            "/projects/{project_id}/skills/{slug}",
            get(get_skill).put(update_skill).delete(delete_skill),
        )
        .route(
            "/projects/{project_id}/skills/{slug}/archive",
            get(download_skill_archive),
        )
        // Project-scoped MCP servers
        .route(
            "/projects/{project_id}/mcp-servers",
            get(list_mcps).post(create_mcp),
        )
        .route(
            "/projects/{project_id}/mcp-servers/{slug}",
            get(get_mcp).put(update_mcp).delete(delete_mcp),
        )
        // Global skills (platform-wide)
        .route(
            "/settings/skills",
            get(list_global_skills).post(create_global_skill),
        )
        .route(
            "/settings/skills/upload",
            post(upload_global_skill).layer(DefaultBodyLimit::max(50 * 1024 * 1024)),
        )
        .route(
            "/settings/skills/{slug}",
            get(get_global_skill)
                .put(update_global_skill)
                .delete(delete_global_skill),
        )
        .route(
            "/settings/skills/{slug}/archive",
            get(download_global_skill_archive),
        )
        // Global MCP servers (platform-wide)
        .route(
            "/settings/mcp-servers",
            get(list_global_mcps).post(create_global_mcp),
        )
        .route(
            "/settings/mcp-servers/{slug}",
            get(get_global_mcp)
                .put(update_global_mcp)
                .delete(delete_global_mcp),
        )
}

// ── Audit ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
struct DefinitionAudit {
    context: AuditContext,
    operation: &'static str,
    resource_kind: &'static str,
    scope: &'static str,
    project_id: Option<i32>,
    slug: String,
    name: Option<String>,
}

impl AuditOperation for DefinitionAudit {
    fn operation_type(&self) -> String {
        format!("{}_{}", self.resource_kind, self.operation)
    }
    fn user_id(&self) -> i32 {
        self.context.user_id
    }
    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }
    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }
    fn serialize(&self) -> temps_core::anyhow::Result<String> {
        serde_json::to_string(self)
            .map_err(|e| temps_core::anyhow::anyhow!("Failed to serialize audit: {}", e))
    }
}

fn audit_ctx(auth: &temps_auth::AuthContext, metadata: &RequestMetadata) -> AuditContext {
    AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    }
}

async fn log_audit(app_state: &AppState, audit: DefinitionAudit) {
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        tracing::error!(
            "Failed to write audit log ({}_{} {} {}): {}",
            audit.resource_kind,
            audit.operation,
            audit.scope,
            audit.slug,
            e
        );
    }
}

// ── Project-scoped Skill handlers ──────────────────────────────────────────

#[utoipa::path(
    tag = "Agents",
    get,
    path = "/projects/{project_id}/skills",
    params(("project_id" = i32, Path, description = "Project ID")),
    responses(
        (status = 200, body = ListSkillsResponse),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_skills(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    let items = app_state
        .definition_service
        .list_skills(project_id)
        .await
        .map_err(Problem::from)?;

    let total = items.len();
    Ok(Json(ListSkillsResponse {
        items: items
            .into_iter()
            .map(SkillDefinitionResponse::from)
            .collect(),
        total,
    }))
}

#[utoipa::path(
    tag = "Agents",
    get,
    path = "/projects/{project_id}/skills/{slug}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("slug" = String, Path, description = "Skill slug"),
    ),
    responses(
        (status = 200, body = SkillDefinitionResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Skill not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_skill(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path((project_id, slug)): Path<(i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    let skill = app_state
        .definition_service
        .get_skill(project_id, &slug)
        .await
        .map_err(Problem::from)?;

    Ok(Json(SkillDefinitionResponse::from(skill)))
}

#[utoipa::path(
    tag = "Agents",
    post,
    path = "/projects/{project_id}/skills",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body = CreateSkillRequest,
    responses(
        (status = 201, body = SkillDefinitionResponse),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_skill(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(request): Json<CreateSkillRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    let skill = app_state
        .definition_service
        .create_skill(
            project_id,
            CreateSkillDefinitionRequest {
                slug: request.slug,
                name: request.name,
                description: request.description,
                content: request.content,
                archive: None,
            },
        )
        .await
        .map_err(Problem::from)?;

    log_audit(
        &app_state,
        DefinitionAudit {
            context: audit_ctx(&auth, &metadata),
            operation: "CREATED",
            resource_kind: "SKILL",
            scope: "project",
            project_id: Some(project_id),
            slug: skill.slug.clone(),
            name: Some(skill.name.clone()),
        },
    )
    .await;

    Ok((
        StatusCode::CREATED,
        Json(SkillDefinitionResponse::from(skill)),
    ))
}

#[utoipa::path(
    tag = "Agents",
    put,
    path = "/projects/{project_id}/skills/{slug}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("slug" = String, Path, description = "Skill slug"),
    ),
    request_body = UpdateSkillRequest,
    responses(
        (status = 200, body = SkillDefinitionResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Skill not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_skill(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, slug)): Path<(i32, String)>,
    Json(request): Json<UpdateSkillRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    let skill = app_state
        .definition_service
        .update_skill(
            project_id,
            &slug,
            UpdateSkillDefinitionRequest {
                name: request.name,
                description: request.description,
                content: request.content,
                archive: None,
            },
        )
        .await
        .map_err(Problem::from)?;

    log_audit(
        &app_state,
        DefinitionAudit {
            context: audit_ctx(&auth, &metadata),
            operation: "UPDATED",
            resource_kind: "SKILL",
            scope: "project",
            project_id: Some(project_id),
            slug: skill.slug.clone(),
            name: Some(skill.name.clone()),
        },
    )
    .await;

    Ok(Json(SkillDefinitionResponse::from(skill)))
}

#[utoipa::path(
    tag = "Agents",
    delete,
    path = "/projects/{project_id}/skills/{slug}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("slug" = String, Path, description = "Skill slug"),
    ),
    responses(
        (status = 204, description = "Skill deleted"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Skill not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_skill(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, slug)): Path<(i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    app_state
        .definition_service
        .delete_skill(project_id, &slug)
        .await
        .map_err(Problem::from)?;

    log_audit(
        &app_state,
        DefinitionAudit {
            context: audit_ctx(&auth, &metadata),
            operation: "DELETED",
            resource_kind: "SKILL",
            scope: "project",
            project_id: Some(project_id),
            slug: slug.clone(),
            name: None,
        },
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

// ── Project-scoped MCP handlers ────────────────────────────────────────────

#[utoipa::path(
    tag = "Agents",
    get,
    path = "/projects/{project_id}/mcp-servers",
    params(("project_id" = i32, Path, description = "Project ID")),
    responses(
        (status = 200, body = ListMcpsResponse),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_mcps(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(project_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    let items = app_state
        .definition_service
        .list_mcps(project_id)
        .await
        .map_err(Problem::from)?;

    let total = items.len();
    Ok(Json(ListMcpsResponse {
        items: items.into_iter().map(McpDefinitionResponse::from).collect(),
        total,
    }))
}

#[utoipa::path(
    tag = "Agents",
    get,
    path = "/projects/{project_id}/mcp-servers/{slug}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("slug" = String, Path, description = "MCP server slug"),
    ),
    responses(
        (status = 200, body = McpDefinitionResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "MCP server not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_mcp(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path((project_id, slug)): Path<(i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    let mcp = app_state
        .definition_service
        .get_mcp(project_id, &slug)
        .await
        .map_err(Problem::from)?;

    Ok(Json(McpDefinitionResponse::from(mcp)))
}

#[utoipa::path(
    tag = "Agents",
    post,
    path = "/projects/{project_id}/mcp-servers",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body = CreateMcpRequest,
    responses(
        (status = 201, body = McpDefinitionResponse),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_mcp(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(request): Json<CreateMcpRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    let mcp = app_state
        .definition_service
        .create_mcp(
            project_id,
            CreateMcpDefinitionRequest {
                slug: request.slug,
                name: request.name,
                description: request.description,
                config: request.config,
            },
        )
        .await
        .map_err(Problem::from)?;

    log_audit(
        &app_state,
        DefinitionAudit {
            context: audit_ctx(&auth, &metadata),
            operation: "CREATED",
            resource_kind: "MCP",
            scope: "project",
            project_id: Some(project_id),
            slug: mcp.slug.clone(),
            name: Some(mcp.name.clone()),
        },
    )
    .await;

    Ok((StatusCode::CREATED, Json(McpDefinitionResponse::from(mcp))))
}

#[utoipa::path(
    tag = "Agents",
    put,
    path = "/projects/{project_id}/mcp-servers/{slug}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("slug" = String, Path, description = "MCP server slug"),
    ),
    request_body = UpdateMcpRequest,
    responses(
        (status = 200, body = McpDefinitionResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "MCP server not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_mcp(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, slug)): Path<(i32, String)>,
    Json(request): Json<UpdateMcpRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    let mcp = app_state
        .definition_service
        .update_mcp(
            project_id,
            &slug,
            UpdateMcpDefinitionRequest {
                name: request.name,
                description: request.description,
                config: request.config,
            },
        )
        .await
        .map_err(Problem::from)?;

    log_audit(
        &app_state,
        DefinitionAudit {
            context: audit_ctx(&auth, &metadata),
            operation: "UPDATED",
            resource_kind: "MCP",
            scope: "project",
            project_id: Some(project_id),
            slug: mcp.slug.clone(),
            name: Some(mcp.name.clone()),
        },
    )
    .await;

    Ok(Json(McpDefinitionResponse::from(mcp)))
}

#[utoipa::path(
    tag = "Agents",
    delete,
    path = "/projects/{project_id}/mcp-servers/{slug}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("slug" = String, Path, description = "MCP server slug"),
    ),
    responses(
        (status = 204, description = "MCP server deleted"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "MCP server not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_mcp(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, slug)): Path<(i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    app_state
        .definition_service
        .delete_mcp(project_id, &slug)
        .await
        .map_err(Problem::from)?;

    log_audit(
        &app_state,
        DefinitionAudit {
            context: audit_ctx(&auth, &metadata),
            operation: "DELETED",
            resource_kind: "MCP",
            scope: "project",
            project_id: Some(project_id),
            slug: slug.clone(),
            name: None,
        },
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

// ── Global Skill handlers ──────────────────────────────────────────────────

#[utoipa::path(
    tag = "Agents",
    get,
    path = "/settings/skills",
    responses(
        (status = 200, body = ListSkillsResponse),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_global_skills(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);

    let items = app_state
        .definition_service
        .list_global_skills()
        .await
        .map_err(Problem::from)?;

    let total = items.len();
    Ok(Json(ListSkillsResponse {
        items: items
            .into_iter()
            .map(SkillDefinitionResponse::from)
            .collect(),
        total,
    }))
}

#[utoipa::path(
    tag = "Agents",
    get,
    path = "/settings/skills/{slug}",
    params(("slug" = String, Path, description = "Skill slug")),
    responses(
        (status = 200, body = SkillDefinitionResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Skill not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_global_skill(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(slug): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);

    let skill = app_state
        .definition_service
        .get_global_skill(&slug)
        .await
        .map_err(Problem::from)?;

    Ok(Json(SkillDefinitionResponse::from(skill)))
}

#[utoipa::path(
    tag = "Agents",
    post,
    path = "/settings/skills",
    request_body = CreateSkillRequest,
    responses(
        (status = 201, body = SkillDefinitionResponse),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_global_skill(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CreateSkillRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);

    let skill = app_state
        .definition_service
        .create_global_skill(CreateSkillDefinitionRequest {
            slug: request.slug,
            name: request.name,
            description: request.description,
            content: request.content,
            archive: None,
        })
        .await
        .map_err(Problem::from)?;

    log_audit(
        &app_state,
        DefinitionAudit {
            context: audit_ctx(&auth, &metadata),
            operation: "CREATED",
            resource_kind: "SKILL",
            scope: "global",
            project_id: None,
            slug: skill.slug.clone(),
            name: Some(skill.name.clone()),
        },
    )
    .await;

    Ok((
        StatusCode::CREATED,
        Json(SkillDefinitionResponse::from(skill)),
    ))
}

#[utoipa::path(
    tag = "Agents",
    put,
    path = "/settings/skills/{slug}",
    params(("slug" = String, Path, description = "Skill slug")),
    request_body = UpdateSkillRequest,
    responses(
        (status = 200, body = SkillDefinitionResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Skill not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_global_skill(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(slug): Path<String>,
    Json(request): Json<UpdateSkillRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);

    let skill = app_state
        .definition_service
        .update_global_skill(
            &slug,
            UpdateSkillDefinitionRequest {
                name: request.name,
                description: request.description,
                content: request.content,
                archive: None,
            },
        )
        .await
        .map_err(Problem::from)?;

    log_audit(
        &app_state,
        DefinitionAudit {
            context: audit_ctx(&auth, &metadata),
            operation: "UPDATED",
            resource_kind: "SKILL",
            scope: "global",
            project_id: None,
            slug: skill.slug.clone(),
            name: Some(skill.name.clone()),
        },
    )
    .await;

    Ok(Json(SkillDefinitionResponse::from(skill)))
}

#[utoipa::path(
    tag = "Agents",
    delete,
    path = "/settings/skills/{slug}",
    params(("slug" = String, Path, description = "Skill slug")),
    responses(
        (status = 204, description = "Skill deleted"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Skill not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_global_skill(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(slug): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);

    app_state
        .definition_service
        .delete_global_skill(&slug)
        .await
        .map_err(Problem::from)?;

    log_audit(
        &app_state,
        DefinitionAudit {
            context: audit_ctx(&auth, &metadata),
            operation: "DELETED",
            resource_kind: "SKILL",
            scope: "global",
            project_id: None,
            slug: slug.clone(),
            name: None,
        },
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

// ── Global MCP handlers ────────────────────────────────────────────────────

#[utoipa::path(
    tag = "Agents",
    get,
    path = "/settings/mcp-servers",
    responses(
        (status = 200, body = ListMcpsResponse),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_global_mcps(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);

    let items = app_state
        .definition_service
        .list_global_mcps()
        .await
        .map_err(Problem::from)?;

    let total = items.len();
    Ok(Json(ListMcpsResponse {
        items: items.into_iter().map(McpDefinitionResponse::from).collect(),
        total,
    }))
}

#[utoipa::path(
    tag = "Agents",
    get,
    path = "/settings/mcp-servers/{slug}",
    params(("slug" = String, Path, description = "MCP server slug")),
    responses(
        (status = 200, body = McpDefinitionResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "MCP server not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_global_mcp(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(slug): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);

    let mcp = app_state
        .definition_service
        .get_global_mcp(&slug)
        .await
        .map_err(Problem::from)?;

    Ok(Json(McpDefinitionResponse::from(mcp)))
}

#[utoipa::path(
    tag = "Agents",
    post,
    path = "/settings/mcp-servers",
    request_body = CreateMcpRequest,
    responses(
        (status = 201, body = McpDefinitionResponse),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_global_mcp(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CreateMcpRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);

    let mcp = app_state
        .definition_service
        .create_global_mcp(CreateMcpDefinitionRequest {
            slug: request.slug,
            name: request.name,
            description: request.description,
            config: request.config,
        })
        .await
        .map_err(Problem::from)?;

    log_audit(
        &app_state,
        DefinitionAudit {
            context: audit_ctx(&auth, &metadata),
            operation: "CREATED",
            resource_kind: "MCP",
            scope: "global",
            project_id: None,
            slug: mcp.slug.clone(),
            name: Some(mcp.name.clone()),
        },
    )
    .await;

    Ok((StatusCode::CREATED, Json(McpDefinitionResponse::from(mcp))))
}

#[utoipa::path(
    tag = "Agents",
    put,
    path = "/settings/mcp-servers/{slug}",
    params(("slug" = String, Path, description = "MCP server slug")),
    request_body = UpdateMcpRequest,
    responses(
        (status = 200, body = McpDefinitionResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "MCP server not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_global_mcp(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(slug): Path<String>,
    Json(request): Json<UpdateMcpRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);

    let mcp = app_state
        .definition_service
        .update_global_mcp(
            &slug,
            UpdateMcpDefinitionRequest {
                name: request.name,
                description: request.description,
                config: request.config,
            },
        )
        .await
        .map_err(Problem::from)?;

    log_audit(
        &app_state,
        DefinitionAudit {
            context: audit_ctx(&auth, &metadata),
            operation: "UPDATED",
            resource_kind: "MCP",
            scope: "global",
            project_id: None,
            slug: mcp.slug.clone(),
            name: Some(mcp.name.clone()),
        },
    )
    .await;

    Ok(Json(McpDefinitionResponse::from(mcp)))
}

#[utoipa::path(
    tag = "Agents",
    delete,
    path = "/settings/mcp-servers/{slug}",
    params(("slug" = String, Path, description = "MCP server slug")),
    responses(
        (status = 204, description = "MCP server deleted"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "MCP server not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_global_mcp(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(slug): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);

    app_state
        .definition_service
        .delete_global_mcp(&slug)
        .await
        .map_err(Problem::from)?;

    log_audit(
        &app_state,
        DefinitionAudit {
            context: audit_ctx(&auth, &metadata),
            operation: "DELETED",
            resource_kind: "MCP",
            scope: "global",
            project_id: None,
            slug: slug.clone(),
            name: None,
        },
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

// ── Skill archive upload/download ─────────────────────────────────────────

/// Parse multipart form for skill upload.
/// Expected fields: slug, name, content (SKILL.md text), archive (tar.gz binary),
/// and optionally description.
async fn parse_skill_multipart(
    mut multipart: Multipart,
) -> Result<CreateSkillDefinitionRequest, Problem> {
    let mut slug = None;
    let mut name = None;
    let mut description = None;
    let mut content = None;
    let mut archive = None;

    while let Some(field) = multipart.next_field().await.map_err(|e| {
        problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Multipart Error")
            .with_detail(format!("Failed to read multipart field: {}", e))
    })? {
        let field_name = field.name().unwrap_or("").to_string();
        match field_name.as_str() {
            "slug" => {
                slug = Some(field.text().await.map_err(|e| {
                    problemdetails::new(StatusCode::BAD_REQUEST)
                        .with_title("Multipart Error")
                        .with_detail(format!("Failed to read slug: {}", e))
                })?);
            }
            "name" => {
                name = Some(field.text().await.map_err(|e| {
                    problemdetails::new(StatusCode::BAD_REQUEST)
                        .with_title("Multipart Error")
                        .with_detail(format!("Failed to read name: {}", e))
                })?);
            }
            "description" => {
                description = Some(field.text().await.map_err(|e| {
                    problemdetails::new(StatusCode::BAD_REQUEST)
                        .with_title("Multipart Error")
                        .with_detail(format!("Failed to read description: {}", e))
                })?);
            }
            "content" => {
                content = Some(field.text().await.map_err(|e| {
                    problemdetails::new(StatusCode::BAD_REQUEST)
                        .with_title("Multipart Error")
                        .with_detail(format!("Failed to read content: {}", e))
                })?);
            }
            "archive" => {
                let bytes = field.bytes().await.map_err(|e| {
                    problemdetails::new(StatusCode::BAD_REQUEST)
                        .with_title("Multipart Error")
                        .with_detail(format!("Failed to read archive: {}", e))
                })?;
                if !bytes.is_empty() {
                    archive = Some(bytes.to_vec());
                }
            }
            _ => {} // ignore unknown fields
        }
    }

    let slug = slug.ok_or_else(|| {
        problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Missing Field")
            .with_detail("'slug' is required")
    })?;
    let name = name.ok_or_else(|| {
        problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Missing Field")
            .with_detail("'name' is required")
    })?;
    let content = content.unwrap_or_default();

    Ok(CreateSkillDefinitionRequest {
        slug,
        name,
        description,
        content,
        archive,
    })
}

/// Upload a skill with an archive (tar.gz) — project-scoped.
#[utoipa::path(
    tag = "Agents",
    post,
    path = "/projects/{project_id}/skills/upload",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body(content_type = "multipart/form-data", content = String),
    responses(
        (status = 201, body = SkillDefinitionResponse),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn upload_skill(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    multipart: Multipart,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    let request = parse_skill_multipart(multipart).await?;
    let skill = app_state
        .definition_service
        .create_skill(project_id, request)
        .await
        .map_err(Problem::from)?;

    log_audit(
        &app_state,
        DefinitionAudit {
            context: audit_ctx(&auth, &metadata),
            operation: "UPLOADED",
            resource_kind: "SKILL",
            scope: "project",
            project_id: Some(project_id),
            slug: skill.slug.clone(),
            name: Some(skill.name.clone()),
        },
    )
    .await;

    Ok((
        StatusCode::CREATED,
        Json(SkillDefinitionResponse::from(skill)),
    ))
}

/// Upload a skill with an archive (tar.gz) — global.
#[utoipa::path(
    tag = "Agents",
    post,
    path = "/settings/skills/upload",
    request_body(content_type = "multipart/form-data", content = String),
    responses(
        (status = 201, body = SkillDefinitionResponse),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn upload_global_skill(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    multipart: Multipart,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);

    let request = parse_skill_multipart(multipart).await?;
    let skill = app_state
        .definition_service
        .create_global_skill(request)
        .await
        .map_err(Problem::from)?;

    log_audit(
        &app_state,
        DefinitionAudit {
            context: audit_ctx(&auth, &metadata),
            operation: "UPLOADED",
            resource_kind: "SKILL",
            scope: "global",
            project_id: None,
            slug: skill.slug.clone(),
            name: Some(skill.name.clone()),
        },
    )
    .await;

    Ok((
        StatusCode::CREATED,
        Json(SkillDefinitionResponse::from(skill)),
    ))
}

/// Download a skill's archive (tar.gz) — project-scoped.
#[utoipa::path(
    tag = "Agents",
    get,
    path = "/projects/{project_id}/skills/{slug}/archive",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("slug" = String, Path, description = "Skill slug"),
    ),
    responses(
        (status = 200, description = "Skill archive tar.gz", content_type = "application/gzip"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Skill not found or has no archive"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn download_skill_archive(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path((project_id, slug)): Path<(i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, app_state.project_access_checker);

    let skill = app_state
        .definition_service
        .get_skill(project_id, &slug)
        .await
        .map_err(Problem::from)?;

    let archive = skill.archive.ok_or_else(|| {
        problemdetails::new(StatusCode::NOT_FOUND)
            .with_title("No Archive")
            .with_detail(format!("Skill '{}' does not have an archive", slug))
    })?;

    Ok((
        StatusCode::OK,
        [
            (
                axum::http::header::CONTENT_TYPE,
                "application/gzip".to_string(),
            ),
            (
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}.tar.gz\"", slug),
            ),
        ],
        archive,
    ))
}

/// Download a skill's archive (tar.gz) — global.
#[utoipa::path(
    tag = "Agents",
    get,
    path = "/settings/skills/{slug}/archive",
    params(("slug" = String, Path, description = "Skill slug")),
    responses(
        (status = 200, description = "Skill archive tar.gz", content_type = "application/gzip"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Skill not found or has no archive"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn download_global_skill_archive(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(slug): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);

    let skill = app_state
        .definition_service
        .get_global_skill(&slug)
        .await
        .map_err(Problem::from)?;

    let archive = skill.archive.ok_or_else(|| {
        problemdetails::new(StatusCode::NOT_FOUND)
            .with_title("No Archive")
            .with_detail(format!("Skill '{}' does not have an archive", slug))
    })?;

    Ok((
        StatusCode::OK,
        [
            (
                axum::http::header::CONTENT_TYPE,
                "application/gzip".to_string(),
            ),
            (
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}.tar.gz\"", slug),
            ),
        ],
        archive,
    ))
}
