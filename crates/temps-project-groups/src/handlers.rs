// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP surface for project groups.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_auth::permissions::Role;
use temps_auth::{permission_guard, project_permission_guard, RequireAuth};
use temps_core::error_builder::ErrorBuilder;
use temps_core::problemdetails::Problem;
use temps_core::{
    AuditContext, AuditLogger, AuditOperation, ProjectAccessChecker, RequestMetadata,
};
use temps_entities::{project_group_env_vars, project_group_routes, project_groups, projects};
use utoipa::{OpenApi, ToSchema};

use crate::env_vars::{CreateGroupEnvVarRequest, GroupEnvVarService, UpdateGroupEnvVarRequest};
use crate::error::ProjectGroupError;
use crate::repository::{
    GroupRepository, GroupRepositoryInput, GroupRepositoryService, MemberSyncResult,
    RepositorySource,
};
use crate::routes::{
    CreateGroupRouteRequest, GroupRouteEnvironment, GroupRouteResponse, GroupRouteService,
};
use crate::service::{
    CreateProjectGroupRequest, ProjectGroupService, ProjectGroupWithMembers,
    UpdateProjectGroupRequest,
};

#[derive(Clone)]
pub struct ProjectGroupsAppState {
    pub service: Arc<ProjectGroupService>,
    pub env_vars: Arc<GroupEnvVarService>,
    pub repository: Arc<GroupRepositoryService>,
    pub routes: Arc<GroupRouteService>,
    pub audit: Arc<dyn AuditLogger>,
    /// Present when a plugin (teams) restricts project access. `None` means
    /// every caller with the instance-wide permission sees every project.
    pub project_access_checker: Option<Arc<dyn ProjectAccessChecker>>,
}

pub fn router(state: Arc<ProjectGroupsAppState>) -> Router {
    Router::new()
        .route(
            "/project-groups",
            get(list_project_groups).post(create_project_group),
        )
        .route(
            "/project-groups/by-slug/{slug}",
            get(get_project_group_by_slug),
        )
        .route(
            "/project-groups/{group_id}",
            get(get_project_group)
                .patch(update_project_group)
                .delete(delete_project_group),
        )
        .route(
            "/project-groups/{group_id}/projects/{project_id}",
            put(add_project_to_group).delete(remove_project_from_group),
        )
        .route(
            "/project-groups/{group_id}/repository",
            get(get_group_repository)
                .put(set_group_repository)
                .delete(clear_group_repository),
        )
        .route(
            "/project-groups/{group_id}/projects/{project_id}/repository-inheritance",
            put(set_repository_inheritance),
        )
        .route(
            "/project-groups/{group_id}/env-vars",
            get(list_group_env_vars).post(create_group_env_var),
        )
        .route(
            "/project-groups/{group_id}/env-vars/{var_id}",
            axum::routing::patch(update_group_env_var).delete(delete_group_env_var),
        )
        .route(
            "/project-groups/{group_id}/routes",
            get(list_group_routes).post(create_group_route),
        )
        .route(
            "/project-groups/{group_id}/routes/{route_id}",
            axum::routing::delete(delete_group_route),
        )
        .route(
            "/projects/{project_id}/group",
            get(get_project_group_membership),
        )
        .route(
            "/projects/{project_id}/inherited-env-vars",
            get(get_inherited_env_vars),
        )
        .with_state(state)
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

/// A project as it appears inside a group.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProjectGroupMemberResponse {
    pub id: i32,
    pub name: String,
    pub slug: String,
    pub preset: String,
    pub project_type: String,
    pub source_type: String,
    pub repo_owner: String,
    pub repo_name: String,
    /// Branch the project deploys from.
    #[schema(example = "main")]
    pub main_branch: String,
    /// Directory of the repository the project builds from.
    #[schema(example = "apps/web")]
    pub directory: String,
    /// Whether the project follows the group's repository and branch.
    pub inherits_repository: bool,
    /// For a project that follows the group's repository: whether it points
    /// at it right now. False means a change could not be applied to it (or
    /// its git settings were edited directly); set the repository again.
    pub follows_repository: Option<bool>,
    #[schema(example = "2026-09-28T12:15:47.609192Z")]
    pub last_deployment: Option<chrono::DateTime<chrono::Utc>>,
    /// `external` for a service whose app runs outside Temps.
    pub hosting: temps_entities::types::Hosting,
}

impl ProjectGroupMemberResponse {
    fn build(group: &project_groups::Model, p: projects::Model, inherits: bool) -> Self {
        let follows_repository = inherits.then(|| crate::repository::follows(group, &p));
        Self {
            main_branch: p.main_branch.clone(),
            directory: p.directory.clone(),
            inherits_repository: inherits,
            follows_repository,
            ..Self::from(p)
        }
    }
}

impl From<projects::Model> for ProjectGroupMemberResponse {
    fn from(p: projects::Model) -> Self {
        Self {
            main_branch: p.main_branch,
            directory: p.directory,
            inherits_repository: false,
            follows_repository: None,
            id: p.id,
            name: p.name,
            slug: p.slug,
            preset: p.preset.to_string(),
            project_type: p.project_type.to_string(),
            source_type: p.source_type.to_string(),
            repo_owner: p.repo_owner,
            repo_name: p.repo_name,
            last_deployment: p.last_deployment,
            hosting: p.hosting,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProjectGroupResponse {
    pub id: i32,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    /// The repository members can follow (monorepos); null when none is set.
    pub repository: Option<ProjectGroupRepositoryRef>,
    /// Member projects the caller can access, in display order.
    pub projects: Vec<ProjectGroupMemberResponse>,
    #[schema(example = "2026-09-28T12:15:47.609192Z")]
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[schema(example = "2026-09-28T12:15:47.609192Z")]
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl ProjectGroupResponse {
    fn build(value: ProjectGroupWithMembers, hidden: &HashSet<i32>) -> Self {
        let ProjectGroupWithMembers {
            group,
            members,
            inherits_repository,
        } = value;
        let projects = members
            .into_iter()
            .filter(|p| !hidden.contains(&p.id))
            .map(|p| {
                let inherits = inherits_repository.contains(&p.id);
                ProjectGroupMemberResponse::build(&group, p, inherits)
            })
            .collect();
        Self {
            id: group.id,
            repository: ProjectGroupRepositoryRef::from_group(&group),
            name: group.name,
            slug: group.slug,
            description: group.description,
            projects,
            created_at: group.created_at,
            updated_at: group.updated_at,
        }
    }
}

/// A group's repository as listed with the group. The full record (with the
/// synced repository id used to scan it) is at
/// `GET /project-groups/{group_id}/repository`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProjectGroupRepositoryRef {
    pub git_provider_connection_id: Option<i32>,
    /// `github` or `gitlab` for a public repository read without a
    /// connection.
    #[schema(example = "github")]
    pub public_provider: Option<String>,
    #[schema(example = "acme")]
    pub repo_owner: String,
    #[schema(example = "shop")]
    pub repo_name: String,
    #[schema(example = "main")]
    pub main_branch: String,
}

impl ProjectGroupRepositoryRef {
    fn from_group(group: &project_groups::Model) -> Option<Self> {
        match (&group.repo_owner, &group.repo_name, &group.main_branch) {
            (Some(owner), Some(name), Some(branch)) => Some(Self {
                git_provider_connection_id: group.git_provider_connection_id,
                public_provider: group.public_provider.clone(),
                repo_owner: owner.clone(),
                repo_name: name.clone(),
                main_branch: branch.clone(),
            }),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProjectGroupListResponse {
    pub groups: Vec<ProjectGroupResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

/// The group a project belongs to; `group` is null for an ungrouped project.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProjectGroupMembershipResponse {
    pub group: Option<ProjectGroupResponse>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ListProjectGroupsQuery {
    pub page: Option<u64>,
    pub page_size: Option<u64>,
}

// ---------------------------------------------------------------------------
// Audit
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProjectGroupAudit {
    #[serde(skip)]
    operation: &'static str,
    #[serde(flatten)]
    context: AuditContext,
    group_id: i32,
    group_slug: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) project_id: Option<i32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) project_ids: Vec<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_group_id: Option<i32>,
    /// Variable name only; values are never audited.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) variable_key: Option<String>,
    /// `owner/name@branch` when the group's repository was set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) repository: Option<String>,
    /// `source /prefix → target` for path route changes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) path_route: Option<String>,
    /// The template run (instantiation or import) this change belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) instantiation_id: Option<i32>,
    /// Slug of the template a run creates from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) template: Option<String>,
}

impl AuditOperation for ProjectGroupAudit {
    fn operation_type(&self) -> String {
        self.operation.to_string()
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
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("failed to serialize {} audit event: {e}", self.operation))
    }
}

impl ProjectGroupAudit {
    pub(crate) fn new(
        operation: &'static str,
        auth: &temps_auth::context::AuthContext,
        metadata: &RequestMetadata,
        group_id: i32,
        group_slug: &str,
    ) -> Self {
        Self {
            operation,
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            group_id,
            group_slug: group_slug.to_string(),
            project_id: None,
            project_ids: Vec::new(),
            previous_group_id: None,
            variable_key: None,
            repository: None,
            path_route: None,
            instantiation_id: None,
            template: None,
        }
    }

    pub(crate) async fn record(self, audit: &Arc<dyn AuditLogger>) {
        if let Err(e) = audit.create_audit_log(&self).await {
            tracing::error!(
                error = %e,
                operation = self.operation,
                group_id = self.group_id,
                "project groups: failed to write audit log"
            );
        }
    }

    /// Build an audit event from the instantiation engine's `RequestOrigin`
    /// (available without an Axum `RequestMetadata` extension).
    pub(crate) fn from_engine(
        operation: &'static str,
        user_id: i32,
        origin: &crate::instantiation::RequestOrigin,
        group_id: i32,
        group_slug: &str,
    ) -> Self {
        Self {
            operation,
            context: AuditContext {
                user_id,
                ip_address: origin.ip_address.clone(),
                user_agent: origin.user_agent.clone(),
            },
            group_id,
            group_slug: group_slug.to_string(),
            project_id: None,
            project_ids: Vec::new(),
            previous_group_id: None,
            variable_key: None,
            repository: None,
            path_route: None,
            instantiation_id: None,
            template: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Authorization helpers
// ---------------------------------------------------------------------------

/// Groups are an instance-level organizing surface; a deployment token is
/// confined to one project and has no business reading or reshaping them.
pub(crate) fn reject_deployment_token(
    auth: &temps_auth::context::AuthContext,
) -> Result<(), Problem> {
    if auth.is_deployment_token() {
        return Err(ErrorBuilder::new(StatusCode::FORBIDDEN)
            .type_("https://temps.sh/probs/deployment-token-not-allowed")
            .title("Deployment Tokens Cannot Manage Project Groups")
            .detail("Project groups span several projects; use a user session or API key")
            .build());
    }
    Ok(())
}

/// Projects the caller must not see, mirroring the project list: instance
/// administrators and OSS instances without an access checker hide nothing.
pub(crate) async fn hidden_projects(
    state: &ProjectGroupsAppState,
    auth: &temps_auth::context::AuthContext,
) -> Result<HashSet<i32>, Problem> {
    if auth.is_admin() || auth.has_role(&Role::PlatformAdmin) {
        return Ok(HashSet::new());
    }
    let Some(checker) = state.project_access_checker.as_ref() else {
        return Ok(HashSet::new());
    };
    let Some(user_id) = auth.user_id_opt() else {
        tracing::error!("project groups: authenticated caller has no user id");
        return Err(ErrorBuilder::new(StatusCode::FORBIDDEN)
            .type_("https://temps.sh/probs/project-access-denied")
            .title("Project Access Denied")
            .detail("Could not resolve caller identity")
            .build());
    };
    match checker.hidden_project_ids(user_id).await {
        Ok(hidden) => Ok(hidden.unwrap_or_default().into_iter().collect()),
        Err(e) => {
            tracing::error!(error = %e, user_id, "project groups: hidden project lookup failed");
            Err(ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .type_("https://temps.sh/probs/project-access-check-failed")
                .title("Project Access Check Failed")
                .detail("Could not verify which projects you can access; please try again")
                .build())
        }
    }
}

/// Reshaping a group (rename, delete) affects how everyone sees its
/// projects, so the caller must be able to reach every one of them.
pub(crate) fn require_access_to_all_members(
    group: &ProjectGroupWithMembers,
    hidden: &HashSet<i32>,
) -> Result<(), Problem> {
    let blocked = group
        .members
        .iter()
        .filter(|p| hidden.contains(&p.id))
        .count();
    if blocked > 0 {
        return Err(ErrorBuilder::new(StatusCode::FORBIDDEN)
            .type_("https://temps.sh/probs/project-access-denied")
            .title("Project Access Denied")
            .detail(format!(
                "Changing project group '{}' requires access to all of its projects; \
                 you cannot access {blocked} of them",
                group.group.slug
            ))
            .build());
    }
    Ok(())
}

/// The projects that follow the group's repository have their git settings
/// rewritten (or detached) by repository changes, so the caller needs write
/// access to each of them. Visibility alone is not enough where access is
/// granted per project.
///
/// Returns the ids that were checked. The service compares them with the
/// followers it finds under lock, so a project that starts following after
/// this check cannot be rewritten or detached without having been checked.
async fn require_write_on_followers(
    state: &ProjectGroupsAppState,
    auth: &temps_auth::context::AuthContext,
    group: &ProjectGroupWithMembers,
) -> Result<HashSet<i32>, Problem> {
    let followers: Vec<i32> = group
        .members
        .iter()
        .map(|p| p.id)
        .filter(|id| group.inherits_repository.contains(id))
        .collect();
    require_on_projects(
        auth,
        &state.project_access_checker,
        &followers,
        MemberPermission::ProjectsWrite,
    )
    .await?;
    Ok(followers.into_iter().collect())
}

/// Per-project permissions a group operation can require of its members.
#[derive(Debug, Clone, Copy)]
enum MemberPermission {
    EnvironmentsRead,
    EnvironmentsWrite,
    ProjectsWrite,
}

/// `permission` on every project in `project_ids`: the instance-wide
/// permission, narrowed per project by the access checker when one is
/// installed (teams, per-project roles).
async fn require_on_projects(
    auth: &temps_auth::context::AuthContext,
    project_access_checker: &Option<Arc<dyn ProjectAccessChecker>>,
    project_ids: &[i32],
    permission: MemberPermission,
) -> Result<(), Problem> {
    for &project_id in project_ids {
        match permission {
            MemberPermission::EnvironmentsRead => {
                project_permission_guard!(
                    auth,
                    EnvironmentsRead,
                    project_id,
                    *project_access_checker
                );
            }
            MemberPermission::EnvironmentsWrite => {
                project_permission_guard!(
                    auth,
                    EnvironmentsWrite,
                    project_id,
                    *project_access_checker
                );
            }
            MemberPermission::ProjectsWrite => {
                project_permission_guard!(auth, ProjectsWrite, project_id, *project_access_checker);
            }
        }
    }
    Ok(())
}

/// A project leaving its group (removed, moved to another group, or listed
/// when creating a new one) takes the old group's routes involving it with
/// it: the database deletes them. That rewrites traffic of the route's other
/// endpoint, so it takes what deleting the route directly takes
/// ([`delete_group_route`]): the other endpoint must be visible to the caller
/// (a route naming a service the caller cannot open is reported as missing,
/// not described) and the caller needs project-scoped `ProjectsWrite` on both
/// endpoints.
///
/// Returns the ids of the authorized routes, which the `*_checked` service
/// mutators compare against what they find under lock, so a route created
/// after this check cannot be dropped unauthorized.
pub(crate) async fn authorize_route_drops(
    auth: &temps_auth::context::AuthContext,
    project_access_checker: &Option<Arc<dyn ProjectAccessChecker>>,
    hidden: &HashSet<i32>,
    routes: &[project_group_routes::Model],
) -> Result<HashSet<i32>, Problem> {
    let mut endpoints: Vec<i32> = Vec::with_capacity(routes.len() * 2);
    for route in routes {
        if hidden.contains(&route.source_project_id) || hidden.contains(&route.target_project_id) {
            return Err(ProjectGroupError::RouteNotFound {
                group_id: route.group_id,
                route_id: route.id,
            }
            .into());
        }
        for id in [route.source_project_id, route.target_project_id] {
            if !endpoints.contains(&id) {
                endpoints.push(id);
            }
        }
    }
    require_on_projects(
        auth,
        project_access_checker,
        &endpoints,
        MemberPermission::ProjectsWrite,
    )
    .await?;
    Ok(routes.iter().map(|r| r.id).collect())
}

/// One `PROJECT_GROUP_ROUTE_REMOVED` audit event per route a membership
/// change dropped, recorded against the group the route belonged to.
async fn audit_dropped_routes(
    state: &ProjectGroupsAppState,
    auth: &temps_auth::context::AuthContext,
    metadata: &RequestMetadata,
    routes: &[project_group_routes::Model],
    reason: &str,
) {
    if routes.is_empty() {
        return;
    }
    let mut group_ids: Vec<i32> = routes.iter().map(|r| r.group_id).collect();
    group_ids.sort_unstable();
    group_ids.dedup();
    let slugs: HashMap<i32, String> = match state.service.group_slugs(&group_ids).await {
        Ok(slugs) => slugs,
        Err(e) => {
            tracing::warn!(error = %e, "project groups: could not resolve group slugs for route audit");
            HashMap::new()
        }
    };
    audit_dropped_routes_in(state, auth, metadata, routes, reason, &slugs).await;
}

/// [`audit_dropped_routes`] for callers that already know the slugs of the
/// groups the routes belonged to, as when the group itself was just deleted
/// and can no longer be looked up.
async fn audit_dropped_routes_in(
    state: &ProjectGroupsAppState,
    auth: &temps_auth::context::AuthContext,
    metadata: &RequestMetadata,
    routes: &[project_group_routes::Model],
    reason: &str,
    slugs: &HashMap<i32, String>,
) {
    for route in routes {
        let slug = slugs.get(&route.group_id).map_or("", String::as_str);
        let mut audit = ProjectGroupAudit::new(
            "PROJECT_GROUP_ROUTE_REMOVED",
            auth,
            metadata,
            route.group_id,
            slug,
        );
        audit.project_ids = vec![route.source_project_id, route.target_project_id];
        audit.path_route = Some(format!(
            "project {} {} → project {} ({reason})",
            route.source_project_id, route.path_prefix, route.target_project_id
        ));
        audit.record(&state.audit).await;
    }
}

/// A group whose every member is hidden from the caller is reported exactly
/// like a missing one, so its name and id existence do not leak.
pub(crate) fn visible_or_not_found(
    group: ProjectGroupWithMembers,
    hidden: &HashSet<i32>,
    not_found: ProjectGroupError,
) -> Result<ProjectGroupWithMembers, Problem> {
    if ProjectGroupService::is_visible(&group, hidden) {
        Ok(group)
    } else {
        Err(not_found.into())
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

#[utoipa::path(
    tag = "Project Groups",
    get,
    path = "/project-groups",
    params(
        ("page" = Option<u64>, Query, description = "1-indexed page"),
        ("page_size" = Option<u64>, Query, description = "default 20, max 100"),
    ),
    responses(
        (status = 200, description = "Project groups, ordered by name", body = ProjectGroupListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_project_groups(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ProjectGroupsAppState>>,
    Query(q): Query<ListProjectGroupsQuery>,
) -> Result<impl IntoResponse, Problem> {
    reject_deployment_token(&auth)?;
    permission_guard!(auth, ProjectsRead);
    let hidden = hidden_projects(&state, &auth).await?;
    let (page, page_size) = ProjectGroupService::page_params(q.page, q.page_size);
    let (groups, total) = state
        .service
        .list_groups(q.page, q.page_size, &hidden)
        .await?;
    Ok(Json(ProjectGroupListResponse {
        groups: groups
            .into_iter()
            .map(|g| ProjectGroupResponse::build(g, &hidden))
            .collect(),
        total,
        page,
        page_size,
    }))
}

#[utoipa::path(
    tag = "Project Groups",
    post,
    path = "/project-groups",
    request_body = CreateProjectGroupRequest,
    responses(
        (status = 201, description = "Project group created", body = ProjectGroupResponse),
        (status = 400, description = "Validation error"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "A listed project does not exist, or a route dropped by moving a listed project involves a service the caller cannot see"),
        (status = 409, description = "Slug already taken, or a listed project gained a route or changed group while the request was being authorized; retry"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_project_group(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ProjectGroupsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(req): Json<CreateProjectGroupRequest>,
) -> Result<impl IntoResponse, Problem> {
    reject_deployment_token(&auth)?;
    permission_guard!(auth, ProjectsCreate);
    for project_id in &req.project_ids {
        project_permission_guard!(
            auth,
            ProjectsWrite,
            *project_id,
            state.project_access_checker
        );
    }

    // A listed project that is already in another group is moved out of it,
    // and that group's routes involving it are dropped. Authorize those the
    // way deleting them would be.
    let hidden = hidden_projects(&state, &auth).await?;
    let dropping = state
        .service
        .routes_dropped_by_leaving_group(&req.project_ids, None)
        .await?;
    let authorized =
        authorize_route_drops(&auth, &state.project_access_checker, &hidden, &dropping).await?;

    let (created, dropped) = state
        .service
        .create_group_checked(req, Some(&authorized))
        .await?;
    let mut audit = ProjectGroupAudit::new(
        "PROJECT_GROUP_CREATED",
        &auth,
        &metadata,
        created.group.id,
        &created.group.slug,
    );
    audit.project_ids = created.members.iter().map(|p| p.id).collect();
    audit.record(&state.audit).await;
    audit_dropped_routes(
        &state,
        &auth,
        &metadata,
        &dropped,
        "project moved into a new group",
    )
    .await;

    Ok((
        StatusCode::CREATED,
        Json(ProjectGroupResponse::build(created, &hidden)),
    ))
}

#[utoipa::path(
    tag = "Project Groups",
    get,
    path = "/project-groups/{group_id}",
    params(("group_id" = i32, Path, description = "Project group ID")),
    responses(
        (status = 200, description = "Project group", body = ProjectGroupResponse),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Project group not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_project_group(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ProjectGroupsAppState>>,
    Path(group_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    reject_deployment_token(&auth)?;
    permission_guard!(auth, ProjectsRead);
    let hidden = hidden_projects(&state, &auth).await?;
    let group = visible_or_not_found(
        state.service.get_group(group_id).await?,
        &hidden,
        ProjectGroupError::NotFound { group_id },
    )?;
    Ok(Json(ProjectGroupResponse::build(group, &hidden)))
}

#[utoipa::path(
    tag = "Project Groups",
    get,
    path = "/project-groups/by-slug/{slug}",
    params(("slug" = String, Path, description = "Project group slug")),
    responses(
        (status = 200, description = "Project group", body = ProjectGroupResponse),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Project group not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_project_group_by_slug(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ProjectGroupsAppState>>,
    Path(slug): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    reject_deployment_token(&auth)?;
    permission_guard!(auth, ProjectsRead);
    let hidden = hidden_projects(&state, &auth).await?;
    let group = visible_or_not_found(
        state.service.get_group_by_slug(&slug).await?,
        &hidden,
        ProjectGroupError::SlugNotFound { slug: slug.clone() },
    )?;
    Ok(Json(ProjectGroupResponse::build(group, &hidden)))
}

#[utoipa::path(
    tag = "Project Groups",
    patch,
    path = "/project-groups/{group_id}",
    params(("group_id" = i32, Path, description = "Project group ID")),
    request_body = UpdateProjectGroupRequest,
    responses(
        (status = 200, description = "Project group updated", body = ProjectGroupResponse),
        (status = 400, description = "Validation error"),
        (status = 403, description = "Insufficient permissions or no access to a member project"),
        (status = 404, description = "Project group not found"),
        (status = 409, description = "Slug already taken, or a project joined the group while the request was being authorized; retry"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_project_group(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ProjectGroupsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(group_id): Path<i32>,
    Json(req): Json<UpdateProjectGroupRequest>,
) -> Result<impl IntoResponse, Problem> {
    reject_deployment_token(&auth)?;
    permission_guard!(auth, ProjectsWrite);
    let hidden = hidden_projects(&state, &auth).await?;
    let current = visible_or_not_found(
        state.service.get_group(group_id).await?,
        &hidden,
        ProjectGroupError::NotFound { group_id },
    )?;
    require_access_to_all_members(&current, &hidden)?;
    // Written under a lock that re-reads the members: one that joined after
    // the check above (and may be hidden from the caller) fails with 409.
    let authorized_members: HashSet<i32> = current.members.iter().map(|p| p.id).collect();

    let updated = state
        .service
        .update_group_checked(group_id, req, &authorized_members)
        .await?;
    ProjectGroupAudit::new(
        "PROJECT_GROUP_UPDATED",
        &auth,
        &metadata,
        updated.group.id,
        &updated.group.slug,
    )
    .record(&state.audit)
    .await;

    Ok(Json(ProjectGroupResponse::build(updated, &hidden)))
}

#[utoipa::path(
    tag = "Project Groups",
    delete,
    path = "/project-groups/{group_id}",
    params(("group_id" = i32, Path, description = "Project group ID")),
    responses(
        (status = 204, description = "Project group deleted; its projects are now ungrouped and otherwise unchanged"),
        (status = 403, description = "Insufficient permissions, no access to a member project, or no ProjectsWrite on a service whose route in the group is deleted with it"),
        (status = 404, description = "Project group not found, or a route that would be deleted involves a service the caller cannot see"),
        (status = 409, description = "A route appeared in the group, or a project joined it, while the request was being authorized; retry"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_project_group(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ProjectGroupsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(group_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    reject_deployment_token(&auth)?;
    permission_guard!(auth, ProjectsWrite);
    // Deleting a group also removes its shared secrets from every member's
    // environment, so the caller must hold EnvironmentsWrite on every member —
    // the same gate as editing a group variable.
    let group = authorize_group_env(&state, &auth, group_id, true).await?;
    let authorized_members: HashSet<i32> = group.members.iter().map(|p| p.id).collect();

    // The database deletes the group's routes with it. That rewrites traffic
    // between its services, so it takes what deleting each route directly
    // takes: both endpoints visible and project-scoped ProjectsWrite on both.
    // The deletion re-reads the routes under lock and refuses (409) any that
    // appeared after this check, and so does a member that joined after the
    // access check above.
    let hidden = hidden_projects(&state, &auth).await?;
    let dropping = state.service.routes_of_group(group_id).await?;
    let authorized =
        authorize_route_drops(&auth, &state.project_access_checker, &hidden, &dropping).await?;

    let deletion = state
        .service
        .delete_group_checked(group_id, &authorized, &authorized_members)
        .await?;
    let group = deletion.group;

    // The group is gone, so its slug cannot be looked up for the route audit.
    let slugs = HashMap::from([(group.id, group.slug.clone())]);
    audit_dropped_routes_in(
        &state,
        &auth,
        &metadata,
        &deletion.dropped_routes,
        "project group deleted",
        &slugs,
    )
    .await;

    let mut audit = ProjectGroupAudit::new(
        "PROJECT_GROUP_DELETED",
        &auth,
        &metadata,
        group.id,
        &group.slug,
    );
    audit.project_ids = deletion.released_project_ids;
    audit.record(&state.audit).await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    tag = "Project Groups",
    put,
    path = "/project-groups/{group_id}/projects/{project_id}",
    params(
        ("group_id" = i32, Path, description = "Project group ID"),
        ("project_id" = i32, Path, description = "Project ID"),
    ),
    responses(
        (status = 200, description = "Project placed in the group (moved out of any other group)", body = ProjectGroupResponse),
        (status = 403, description = "Insufficient permissions on the project, or on the group's projects: joining gives it their shared variables, so it requires managing them; or no ProjectsWrite on a service whose route to or from the project is dropped by leaving its previous group"),
        (status = 404, description = "Project group or project not found, or a route that would be dropped involves a service the caller cannot see"),
        (status = 409, description = "A route involving the project appeared, the project changed group, or a project joined the destination group, while the request was being authorized; retry"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn add_project_to_group(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ProjectGroupsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((group_id, project_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    reject_deployment_token(&auth)?;
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        state.project_access_checker
    );

    // A project that joins the group receives its shared variables (secrets
    // included) on its next deployment. Joining therefore takes what managing
    // those variables takes, on the joining project as much as on every
    // member; otherwise owning one project would be enough to deploy code that
    // reads the group's secrets.
    project_permission_guard!(
        auth,
        EnvironmentsWrite,
        project_id,
        state.project_access_checker
    );
    let group = authorize_group_env(&state, &auth, group_id, true).await?;
    let authorized_members: HashSet<i32> = group.members.iter().map(|p| p.id).collect();
    let hidden = hidden_projects(&state, &auth).await?;

    // Moving out of another group drops that group's routes involving the
    // project; deleting those takes ProjectsWrite on both of their ends.
    let dropping = state
        .service
        .routes_dropped_by_leaving_group(&[project_id], Some(group_id))
        .await?;
    let authorized =
        authorize_route_drops(&auth, &state.project_access_checker, &hidden, &dropping).await?;

    let placement = state
        .service
        .add_project_checked(group_id, project_id, &authorized, &authorized_members)
        .await?;
    let previous = placement.previous_group_id;
    let group = state.service.get_group(group_id).await?;
    audit_dropped_routes(
        &state,
        &auth,
        &metadata,
        &placement.dropped_routes,
        "project moved to another group",
    )
    .await;
    if previous != Some(group_id) {
        let mut audit = ProjectGroupAudit::new(
            "PROJECT_GROUP_PROJECT_ADDED",
            &auth,
            &metadata,
            group.group.id,
            &group.group.slug,
        );
        audit.project_id = Some(project_id);
        audit.previous_group_id = previous;
        audit.record(&state.audit).await;
    }

    Ok(Json(ProjectGroupResponse::build(group, &hidden)))
}

#[utoipa::path(
    tag = "Project Groups",
    delete,
    path = "/project-groups/{group_id}/projects/{project_id}",
    params(
        ("group_id" = i32, Path, description = "Project group ID"),
        ("project_id" = i32, Path, description = "Project ID"),
    ),
    responses(
        (status = 204, description = "Project removed from the group; it is now ungrouped"),
        (status = 403, description = "Insufficient permissions on the project, or no ProjectsWrite on a service whose route to or from it is dropped with it"),
        (status = 404, description = "Project group not found, the project is not in it, or a route that would be dropped involves a service the caller cannot see"),
        (status = 409, description = "A route involving the project appeared while the request was being authorized; retry"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn remove_project_from_group(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ProjectGroupsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((group_id, project_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    reject_deployment_token(&auth)?;
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        state.project_access_checker
    );

    let hidden = hidden_projects(&state, &auth).await?;
    visible_or_not_found(
        state.service.get_group(group_id).await?,
        &hidden,
        ProjectGroupError::NotFound { group_id },
    )?;

    // Routes of this group involving the project go with it (the database
    // deletes them). Authorize them as deleting each would be, then remove
    // under a lock that re-checks the set.
    let dropping = state
        .service
        .routes_involving_project(group_id, project_id)
        .await?;
    let authorized =
        authorize_route_drops(&auth, &state.project_access_checker, &hidden, &dropping).await?;

    let dropped = state
        .service
        .remove_project_checked(group_id, project_id, &authorized)
        .await?;
    let group = state.service.get_group(group_id).await?;

    // Audit each dropped route before the member audit so the log is complete.
    audit_dropped_routes(
        &state,
        &auth,
        &metadata,
        &dropped,
        "project removed from the group",
    )
    .await;

    let mut audit = ProjectGroupAudit::new(
        "PROJECT_GROUP_PROJECT_REMOVED",
        &auth,
        &metadata,
        group.group.id,
        &group.group.slug,
    );
    audit.project_id = Some(project_id);
    audit.record(&state.audit).await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    tag = "Project Groups",
    get,
    path = "/projects/{project_id}/group",
    params(("project_id" = i32, Path, description = "Project ID")),
    responses(
        (status = 200, description = "The project's group, or null when ungrouped", body = ProjectGroupMembershipResponse),
        (status = 403, description = "Insufficient permissions on the project"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_project_group_membership(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ProjectGroupsAppState>>,
    Path(project_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    reject_deployment_token(&auth)?;
    project_permission_guard!(auth, ProjectsRead, project_id, state.project_access_checker);
    let group = state.service.group_for_project(project_id).await?;
    let hidden = match group {
        Some(_) => hidden_projects(&state, &auth).await?,
        None => HashSet::new(),
    };
    Ok(Json(ProjectGroupMembershipResponse {
        group: group.map(|g| ProjectGroupResponse::build(g, &hidden)),
    }))
}

// ---------------------------------------------------------------------------
// Shared (inherited) environment variables
// ---------------------------------------------------------------------------

/// A group variable as returned by the API. `value` is `"***"` in lists,
/// plaintext only in the create/update response of a non-secret variable,
/// and always `null` for secrets.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GroupEnvVarResponse {
    pub id: i32,
    pub key: String,
    pub value: Option<String>,
    pub is_secret: bool,
    /// Environment slug this applies to; `null` means every environment.
    pub environment_slug: Option<String>,
    pub include_in_preview: bool,
    #[schema(example = "2026-09-28T12:15:47.609192Z")]
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[schema(example = "2026-09-28T12:15:47.609192Z")]
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl GroupEnvVarResponse {
    fn masked(m: project_group_env_vars::Model) -> Self {
        Self::with_value(m, None)
    }

    fn with_value(m: project_group_env_vars::Model, plaintext: Option<String>) -> Self {
        let value = if m.is_secret {
            None
        } else {
            Some(plaintext.unwrap_or_else(|| "***".to_string()))
        };
        Self {
            id: m.id,
            key: m.key,
            value,
            is_secret: m.is_secret,
            environment_slug: m.environment_slug,
            include_in_preview: m.include_in_preview,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct InheritedGroupRef {
    pub id: i32,
    pub name: String,
    pub slug: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct InheritedEnvVarResponse {
    pub key: String,
    pub environment_slug: Option<String>,
    pub is_secret: bool,
    pub include_in_preview: bool,
    /// The project also defines this key itself (in some environment), and
    /// its own value wins wherever both apply.
    pub defined_on_project: bool,
}

/// What a project inherits from its group. `group` is null when ungrouped.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct InheritedEnvVarsResponse {
    pub group: Option<InheritedGroupRef>,
    pub variables: Vec<InheritedEnvVarResponse>,
}

/// Group variables land in every member's deployments, so touching them
/// needs the caller to reach every member with the matching per-project
/// environment permission.
pub(crate) async fn authorize_group_env(
    state: &ProjectGroupsAppState,
    auth: &temps_auth::context::AuthContext,
    group_id: i32,
    write: bool,
) -> Result<ProjectGroupWithMembers, Problem> {
    reject_deployment_token(auth)?;
    let hidden = hidden_projects(state, auth).await?;
    let group = visible_or_not_found(
        state.service.get_group(group_id).await?,
        &hidden,
        ProjectGroupError::NotFound { group_id },
    )?;
    require_access_to_all_members(&group, &hidden)?;
    let permission = if write {
        MemberPermission::EnvironmentsWrite
    } else {
        MemberPermission::EnvironmentsRead
    };
    let members: Vec<i32> = group.members.iter().map(|p| p.id).collect();
    require_on_projects(auth, &state.project_access_checker, &members, permission).await?;
    Ok(group)
}

#[utoipa::path(
    tag = "Project Groups",
    get,
    path = "/project-groups/{group_id}/env-vars",
    params(("group_id" = i32, Path, description = "Project group ID")),
    responses(
        (status = 200, description = "Variables every project in the group inherits (values masked)", body = Vec<GroupEnvVarResponse>),
        (status = 403, description = "Insufficient permissions on a member project"),
        (status = 404, description = "Project group not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_group_env_vars(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ProjectGroupsAppState>>,
    Path(group_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EnvironmentsRead);
    authorize_group_env(&state, &auth, group_id, false).await?;
    let vars = state.env_vars.list(group_id).await?;
    Ok(Json(
        vars.into_iter()
            .map(GroupEnvVarResponse::masked)
            .collect::<Vec<_>>(),
    ))
}

#[utoipa::path(
    tag = "Project Groups",
    post,
    path = "/project-groups/{group_id}/env-vars",
    params(("group_id" = i32, Path, description = "Project group ID")),
    request_body = CreateGroupEnvVarRequest,
    responses(
        (status = 201, description = "Variable created; applies to each project's next deployment", body = GroupEnvVarResponse),
        (status = 400, description = "Validation error"),
        (status = 403, description = "Insufficient permissions on a member project"),
        (status = 404, description = "Project group not found"),
        (status = 409, description = "Variable already exists for that scope, or a project joined the group while the request was being authorized; retry"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_group_env_var(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ProjectGroupsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(group_id): Path<i32>,
    Json(req): Json<CreateGroupEnvVarRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EnvironmentsWrite);
    let group = authorize_group_env(&state, &auth, group_id, true).await?;
    let authorized_members: HashSet<i32> = group.members.iter().map(|p| p.id).collect();
    let plaintext = req.value.clone();
    let created = state
        .env_vars
        .create_checked(group_id, &authorized_members, req)
        .await?;

    let mut audit = ProjectGroupAudit::new(
        "PROJECT_GROUP_ENV_VAR_CREATED",
        &auth,
        &metadata,
        group_id,
        &group.group.slug,
    );
    audit.variable_key = Some(created.key.clone());
    audit.record(&state.audit).await;

    Ok((
        StatusCode::CREATED,
        Json(GroupEnvVarResponse::with_value(created, Some(plaintext))),
    ))
}

#[utoipa::path(
    tag = "Project Groups",
    patch,
    path = "/project-groups/{group_id}/env-vars/{var_id}",
    params(
        ("group_id" = i32, Path, description = "Project group ID"),
        ("var_id" = i32, Path, description = "Variable ID"),
    ),
    request_body = UpdateGroupEnvVarRequest,
    responses(
        (status = 200, description = "Variable updated; applies to each project's next deployment", body = GroupEnvVarResponse),
        (status = 400, description = "Validation error"),
        (status = 403, description = "Insufficient permissions on a member project"),
        (status = 404, description = "Project group or variable not found"),
        (status = 409, description = "A project joined the group while the request was being authorized, so it was not checked; nothing was changed. Retry the request"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_group_env_var(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ProjectGroupsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((group_id, var_id)): Path<(i32, i32)>,
    Json(req): Json<UpdateGroupEnvVarRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EnvironmentsWrite);
    let group = authorize_group_env(&state, &auth, group_id, true).await?;
    let authorized_members: HashSet<i32> = group.members.iter().map(|p| p.id).collect();
    let plaintext = req.value.clone();
    let updated = state
        .env_vars
        .update_checked(group_id, var_id, &authorized_members, req)
        .await?;

    let mut audit = ProjectGroupAudit::new(
        "PROJECT_GROUP_ENV_VAR_UPDATED",
        &auth,
        &metadata,
        group_id,
        &group.group.slug,
    );
    audit.variable_key = Some(updated.key.clone());
    audit.record(&state.audit).await;

    Ok(Json(GroupEnvVarResponse::with_value(updated, plaintext)))
}

#[utoipa::path(
    tag = "Project Groups",
    delete,
    path = "/project-groups/{group_id}/env-vars/{var_id}",
    params(
        ("group_id" = i32, Path, description = "Project group ID"),
        ("var_id" = i32, Path, description = "Variable ID"),
    ),
    responses(
        (status = 204, description = "Variable deleted; projects stop receiving it on their next deployment"),
        (status = 403, description = "Insufficient permissions on a member project"),
        (status = 404, description = "Project group or variable not found"),
        (status = 409, description = "A project joined the group while the request was being authorized, so it was not checked; nothing was changed. Retry the request"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_group_env_var(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ProjectGroupsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((group_id, var_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, EnvironmentsWrite);
    let group = authorize_group_env(&state, &auth, group_id, true).await?;
    let authorized_members: HashSet<i32> = group.members.iter().map(|p| p.id).collect();
    let deleted = state
        .env_vars
        .delete_checked(group_id, var_id, &authorized_members)
        .await?;

    let mut audit = ProjectGroupAudit::new(
        "PROJECT_GROUP_ENV_VAR_DELETED",
        &auth,
        &metadata,
        group_id,
        &group.group.slug,
    );
    audit.variable_key = Some(deleted.key);
    audit.record(&state.audit).await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    tag = "Project Groups",
    get,
    path = "/projects/{project_id}/inherited-env-vars",
    params(("project_id" = i32, Path, description = "Project ID")),
    responses(
        (status = 200, description = "Variables this project inherits from its group (names only)", body = InheritedEnvVarsResponse),
        (status = 403, description = "Insufficient permissions on the project"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_inherited_env_vars(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ProjectGroupsAppState>>,
    Path(project_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    reject_deployment_token(&auth)?;
    project_permission_guard!(
        auth,
        EnvironmentsRead,
        project_id,
        state.project_access_checker
    );
    let Some(group) = state.service.group_for_project(project_id).await? else {
        return Ok(Json(InheritedEnvVarsResponse {
            group: None,
            variables: Vec::new(),
        }));
    };
    let own_keys = state.env_vars.project_variable_keys(project_id).await?;
    let variables = state
        .env_vars
        .list(group.group.id)
        .await?
        .into_iter()
        .map(|v| InheritedEnvVarResponse {
            defined_on_project: own_keys.contains(&v.key),
            key: v.key,
            environment_slug: v.environment_slug,
            is_secret: v.is_secret,
            include_in_preview: v.include_in_preview,
        })
        .collect();
    Ok(Json(InheritedEnvVarsResponse {
        group: Some(InheritedGroupRef {
            id: group.group.id,
            name: group.group.name,
            slug: group.group.slug,
        }),
        variables,
    }))
}

// ---------------------------------------------------------------------------
// Repository (monorepos)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GroupRepositoryResponse {
    /// Null when the group has no repository.
    pub repository: Option<GroupRepository>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetGroupRepositoryRequest {
    /// The connection to read the repository through. Set exactly one of
    /// this and `public_provider`.
    #[serde(default)]
    pub git_provider_connection_id: Option<i32>,
    /// `github` or `gitlab`, to use a public repository without a
    /// connection. Pushes to it cannot trigger deploys.
    #[serde(default)]
    #[schema(example = "github")]
    pub public_provider: Option<String>,
    #[schema(example = "acme")]
    pub repo_owner: String,
    #[schema(example = "shop")]
    pub repo_name: String,
    #[schema(example = "main")]
    pub main_branch: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SetGroupRepositoryResponse {
    pub group: ProjectGroupResponse,
    /// One entry per project that follows the repository. An entry with an
    /// `error` still points at its previous repository or branch.
    pub services: Vec<MemberSyncResult>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetRepositoryInheritanceRequest {
    /// True: the project follows the group's repository and branch (and is
    /// pointed at them now, keeping its own directory). False: it keeps its
    /// current git settings and stops following.
    pub inherit: bool,
}

/// The git permissions the project's own git settings endpoint requires.
fn require_git_permissions(auth: &temps_auth::context::AuthContext) -> Result<(), Problem> {
    permission_guard!(auth, GitConnectionsRead);
    permission_guard!(auth, GitRepositoriesRead);
    Ok(())
}

#[utoipa::path(
    tag = "Project Groups",
    get,
    path = "/project-groups/{group_id}/repository",
    params(("group_id" = i32, Path, description = "Project group ID")),
    responses(
        (status = 200, description = "The group's repository, if any", body = GroupRepositoryResponse),
        (status = 404, description = "Project group not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_group_repository(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ProjectGroupsAppState>>,
    Path(group_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    reject_deployment_token(&auth)?;
    permission_guard!(auth, ProjectsRead);
    let hidden = hidden_projects(&state, &auth).await?;
    let group = visible_or_not_found(
        state.service.get_group(group_id).await?,
        &hidden,
        ProjectGroupError::NotFound { group_id },
    )?;
    let repository = state.repository.repository_of(&group.group).await?;
    Ok(Json(GroupRepositoryResponse { repository }))
}

#[utoipa::path(
    tag = "Project Groups",
    put,
    path = "/project-groups/{group_id}/repository",
    params(("group_id" = i32, Path, description = "Project group ID")),
    request_body = SetGroupRepositoryRequest,
    responses(
        (status = 200, description = "Repository set and written through to every project that follows it; see `services` for per-project outcomes", body = SetGroupRepositoryResponse),
        (status = 400, description = "Connection, repository or branch not usable"),
        (status = 403, description = "Insufficient permissions, or no access to every project in the group"),
        (status = 404, description = "Project group not found"),
        (status = 409, description = "A project started following the group's repository while the request was being authorized, so it was not checked, or another request is changing the group's repository right now; nothing was changed. Retry the request"),
        (status = 429, description = "Too many repository changes are being written through at once on this server; nothing was changed. Retry shortly"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn set_group_repository(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ProjectGroupsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(group_id): Path<i32>,
    Json(req): Json<SetGroupRepositoryRequest>,
) -> Result<impl IntoResponse, Problem> {
    reject_deployment_token(&auth)?;
    permission_guard!(auth, ProjectsWrite);
    require_git_permissions(&auth)?;
    let hidden = hidden_projects(&state, &auth).await?;
    let group = visible_or_not_found(
        state.service.get_group(group_id).await?,
        &hidden,
        ProjectGroupError::NotFound { group_id },
    )?;
    // Changing the repository rewrites the git settings of every project
    // that follows it, so the caller must be able to reach all of them and
    // change each follower, not merely see it.
    require_access_to_all_members(&group, &hidden)?;
    let authorized_followers = require_write_on_followers(&state, &auth, &group).await?;

    let source = match (req.git_provider_connection_id, req.public_provider) {
        (Some(connection_id), None) => RepositorySource::Connection(connection_id),
        (None, Some(provider)) => RepositorySource::Public(provider),
        _ => {
            return Err(ProjectGroupError::Validation {
                message: "Set either git_provider_connection_id or public_provider".to_string(),
            }
            .into())
        }
    };
    let (_, services) = state
        .repository
        .set_repository(
            group_id,
            GroupRepositoryInput {
                source,
                repo_owner: req.repo_owner,
                repo_name: req.repo_name,
                main_branch: req.main_branch,
            },
            &authorized_followers,
            temps_core::docker_socket_grant::DeployCaller::from_instance_admin(
                auth.is_instance_admin(),
            ),
        )
        .await?;
    let group = state.service.get_group(group_id).await?;

    let mut audit = ProjectGroupAudit::new(
        "PROJECT_GROUP_REPOSITORY_SET",
        &auth,
        &metadata,
        group.group.id,
        &group.group.slug,
    );
    audit.project_ids = services
        .iter()
        .filter(|s| s.error.is_none())
        .map(|s| s.project_id)
        .collect();
    audit.repository = group
        .group
        .repo_owner
        .as_ref()
        .zip(group.group.repo_name.as_ref())
        .zip(group.group.main_branch.as_ref())
        .map(
            |((owner, name), branch)| match &group.group.public_provider {
                Some(provider) => format!("{owner}/{name}@{branch} (public, {provider})"),
                None => format!("{owner}/{name}@{branch}"),
            },
        );
    audit.record(&state.audit).await;

    Ok(Json(SetGroupRepositoryResponse {
        group: ProjectGroupResponse::build(group, &hidden),
        services,
    }))
}

#[utoipa::path(
    tag = "Project Groups",
    delete,
    path = "/project-groups/{group_id}/repository",
    params(("group_id" = i32, Path, description = "Project group ID")),
    responses(
        (status = 200, description = "Repository removed; projects keep their current git settings and stop following", body = ProjectGroupResponse),
        (status = 403, description = "Insufficient permissions, or no access to every project in the group"),
        (status = 404, description = "Project group not found"),
        (status = 409, description = "A project started following the group's repository while the request was being authorized, so it was not checked, or another request is changing the group's repository right now; nothing was changed. Retry the request"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn clear_group_repository(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ProjectGroupsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(group_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    reject_deployment_token(&auth)?;
    permission_guard!(auth, ProjectsWrite);
    let hidden = hidden_projects(&state, &auth).await?;
    let group = visible_or_not_found(
        state.service.get_group(group_id).await?,
        &hidden,
        ProjectGroupError::NotFound { group_id },
    )?;
    require_access_to_all_members(&group, &hidden)?;
    let authorized_followers = require_write_on_followers(&state, &auth, &group).await?;

    state
        .repository
        .clear_repository(group_id, &authorized_followers)
        .await?;
    let group = state.service.get_group(group_id).await?;
    ProjectGroupAudit::new(
        "PROJECT_GROUP_REPOSITORY_CLEARED",
        &auth,
        &metadata,
        group.group.id,
        &group.group.slug,
    )
    .record(&state.audit)
    .await;
    Ok(Json(ProjectGroupResponse::build(group, &hidden)))
}

#[utoipa::path(
    tag = "Project Groups",
    put,
    path = "/project-groups/{group_id}/projects/{project_id}/repository-inheritance",
    params(
        ("group_id" = i32, Path, description = "Project group ID"),
        ("project_id" = i32, Path, description = "Project ID"),
    ),
    request_body = SetRepositoryInheritanceRequest,
    responses(
        (status = 200, description = "Inheritance updated", body = ProjectGroupResponse),
        (status = 400, description = "The project cannot follow the repository (not deployed from Git, branch missing, …)"),
        (status = 403, description = "Insufficient permissions on the project"),
        (status = 404, description = "Project group not found, or the project is not in it"),
        (status = 409, description = "The group has no repository yet, or its repository changed while the project was being pointed at it; retry the request"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn set_repository_inheritance(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ProjectGroupsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((group_id, project_id)): Path<(i32, i32)>,
    Json(req): Json<SetRepositoryInheritanceRequest>,
) -> Result<impl IntoResponse, Problem> {
    reject_deployment_token(&auth)?;
    // Opting one project in or out only rewrites that project's git
    // settings, so write access to it is what is required; unlike changing
    // the group's repository, the other members are untouched.
    project_permission_guard!(
        auth,
        ProjectsWrite,
        project_id,
        state.project_access_checker
    );
    require_git_permissions(&auth)?;
    let hidden = hidden_projects(&state, &auth).await?;
    visible_or_not_found(
        state.service.get_group(group_id).await?,
        &hidden,
        ProjectGroupError::NotFound { group_id },
    )?;

    state
        .repository
        .set_inheritance(
            group_id,
            project_id,
            req.inherit,
            temps_core::docker_socket_grant::DeployCaller::from_instance_admin(
                auth.is_instance_admin(),
            ),
        )
        .await?;
    let group = state.service.get_group(group_id).await?;
    let mut audit = ProjectGroupAudit::new(
        if req.inherit {
            "PROJECT_GROUP_PROJECT_FOLLOWS_REPOSITORY"
        } else {
            "PROJECT_GROUP_PROJECT_UNFOLLOWS_REPOSITORY"
        },
        &auth,
        &metadata,
        group.group.id,
        &group.group.slug,
    );
    audit.project_id = Some(project_id);
    audit.record(&state.audit).await;
    Ok(Json(ProjectGroupResponse::build(group, &hidden)))
}

// ---------------------------------------------------------------------------
// Path routes
// ---------------------------------------------------------------------------

/// The group, if the caller can see it. Routes naming a service the caller
/// cannot open are left out of listings.
async fn authorize_group_routes(
    state: &ProjectGroupsAppState,
    auth: &temps_auth::context::AuthContext,
    group_id: i32,
) -> Result<(ProjectGroupWithMembers, HashSet<i32>), Problem> {
    reject_deployment_token(auth)?;
    let hidden = hidden_projects(state, auth).await?;
    let group = visible_or_not_found(
        state.service.get_group(group_id).await?,
        &hidden,
        ProjectGroupError::NotFound { group_id },
    )?;
    Ok((group, hidden))
}

fn route_label(route: &GroupRouteResponse) -> String {
    format!(
        "{} {} → {}",
        route.source_project_name, route.path_prefix, route.target_project_name
    )
}

#[utoipa::path(
    tag = "Project Groups",
    get,
    path = "/project-groups/{group_id}/routes",
    params(("group_id" = i32, Path, description = "Project group ID")),
    responses(
        (status = 200, description = "Path routes between the project's services, with how each applies per environment", body = Vec<GroupRouteResponse>),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Project group not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_group_routes(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ProjectGroupsAppState>>,
    Path(group_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsRead);
    let (group, hidden) = authorize_group_routes(&state, &auth, group_id).await?;
    let routes = state.routes.list(&group).await?;
    Ok(Json(
        routes
            .into_iter()
            .filter(|r| {
                !hidden.contains(&r.source_project_id) && !hidden.contains(&r.target_project_id)
            })
            .collect::<Vec<_>>(),
    ))
}

#[utoipa::path(
    tag = "Project Groups",
    post,
    path = "/project-groups/{group_id}/routes",
    params(("group_id" = i32, Path, description = "Project group ID")),
    request_body = CreateGroupRouteRequest,
    responses(
        (status = 201, description = "Route created; the proxy applies it within seconds, no redeploy needed", body = GroupRouteResponse),
        (status = 400, description = "Invalid prefix, or source and target are the same service"),
        (status = 403, description = "Insufficient permissions on the source or target service"),
        (status = 404, description = "Project group not found, or a service is not in it"),
        (status = 409, description = "The source service already routes this prefix"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_group_route(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ProjectGroupsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(group_id): Path<i32>,
    Json(req): Json<CreateGroupRouteRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);
    let (group, _) = authorize_group_routes(&state, &auth, group_id).await?;
    // The source's traffic changes and the target starts receiving it, so
    // both services must be the caller's to change.
    project_permission_guard!(
        auth,
        ProjectsWrite,
        req.source_project_id,
        state.project_access_checker
    );
    project_permission_guard!(
        auth,
        ProjectsWrite,
        req.target_project_id,
        state.project_access_checker
    );
    let created = state.routes.create(&group, req).await?;

    let mut audit = ProjectGroupAudit::new(
        "PROJECT_GROUP_ROUTE_ADDED",
        &auth,
        &metadata,
        group_id,
        &group.group.slug,
    );
    audit.project_ids = vec![created.source_project_id, created.target_project_id];
    audit.path_route = Some(route_label(&created));
    audit.record(&state.audit).await;

    Ok((StatusCode::CREATED, Json(created)))
}

#[utoipa::path(
    tag = "Project Groups",
    delete,
    path = "/project-groups/{group_id}/routes/{route_id}",
    params(
        ("group_id" = i32, Path, description = "Project group ID"),
        ("route_id" = i32, Path, description = "Route ID"),
    ),
    responses(
        (status = 204, description = "Route removed; the source service serves the prefix itself again"),
        (status = 403, description = "Insufficient permissions on the source or target service"),
        (status = 404, description = "Project group or route not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_group_route(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<ProjectGroupsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((group_id, route_id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ProjectsWrite);
    let (group, hidden) = authorize_group_routes(&state, &auth, group_id).await?;
    let route = state.routes.find(group_id, route_id).await?;
    if hidden.contains(&route.source_project_id) || hidden.contains(&route.target_project_id) {
        return Err(ProjectGroupError::RouteNotFound { group_id, route_id }.into());
    }
    project_permission_guard!(
        auth,
        ProjectsWrite,
        route.source_project_id,
        state.project_access_checker
    );
    project_permission_guard!(
        auth,
        ProjectsWrite,
        route.target_project_id,
        state.project_access_checker
    );
    state.routes.delete(&route).await?;

    let name = |id: i32| {
        group
            .members
            .iter()
            .find(|p| p.id == id)
            .map_or_else(|| format!("project {id}"), |p| p.name.clone())
    };
    let mut audit = ProjectGroupAudit::new(
        "PROJECT_GROUP_ROUTE_REMOVED",
        &auth,
        &metadata,
        group_id,
        &group.group.slug,
    );
    audit.project_ids = vec![route.source_project_id, route.target_project_id];
    audit.path_route = Some(format!(
        "{} {} → {}",
        name(route.source_project_id),
        route.path_prefix,
        name(route.target_project_id)
    ));
    audit.record(&state.audit).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_project_groups,
        create_project_group,
        get_project_group,
        get_project_group_by_slug,
        update_project_group,
        delete_project_group,
        add_project_to_group,
        remove_project_from_group,
        get_group_repository,
        set_group_repository,
        clear_group_repository,
        set_repository_inheritance,
        get_project_group_membership,
        list_group_env_vars,
        create_group_env_var,
        update_group_env_var,
        delete_group_env_var,
        get_inherited_env_vars,
        list_group_routes,
        create_group_route,
        delete_group_route,
    ),
    components(schemas(
        ProjectGroupResponse,
        ProjectGroupMemberResponse,
        ProjectGroupListResponse,
        ProjectGroupMembershipResponse,
        ProjectGroupRepositoryRef,
        GroupRepository,
        GroupRepositoryResponse,
        SetGroupRepositoryRequest,
        SetGroupRepositoryResponse,
        MemberSyncResult,
        SetRepositoryInheritanceRequest,
        CreateProjectGroupRequest,
        UpdateProjectGroupRequest,
        GroupEnvVarResponse,
        CreateGroupEnvVarRequest,
        UpdateGroupEnvVarRequest,
        InheritedGroupRef,
        InheritedEnvVarResponse,
        InheritedEnvVarsResponse,
        CreateGroupRouteRequest,
        GroupRouteResponse,
        GroupRouteEnvironment,
    )),
    tags((name = "Project Groups", description = "Group several projects into one unit"))
)]
pub struct ProjectGroupsApiDoc;

#[cfg(test)]
mod tests {
    use super::*;
    use temps_auth::context::AuthContext;

    fn user_auth(role: Role) -> AuthContext {
        AuthContext::new_session(
            temps_entities::users::Model {
                id: 7,
                name: "Test User".to_string(),
                email: "user@example.com".to_string(),
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
            },
            role,
        )
    }

    /// Grants access to a fixed set of projects, like a team-scoped checker.
    struct Allow(Vec<i32>);

    #[async_trait::async_trait]
    impl ProjectAccessChecker for Allow {
        async fn user_can_access_project(
            &self,
            _user_id: i32,
            project_id: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            Ok(self.0.contains(&project_id))
        }
    }

    fn checker(allowed: &[i32]) -> Option<Arc<dyn ProjectAccessChecker>> {
        Some(Arc::new(Allow(allowed.to_vec())))
    }

    #[tokio::test]
    async fn every_listed_project_must_be_reachable() {
        let auth = user_auth(Role::User);
        let allowed = checker(&[1, 2]);
        for permission in [
            MemberPermission::EnvironmentsRead,
            MemberPermission::EnvironmentsWrite,
            MemberPermission::ProjectsWrite,
        ] {
            assert!(require_on_projects(&auth, &allowed, &[1, 2], permission)
                .await
                .is_ok());
            let denied = require_on_projects(&auth, &allowed, &[1, 3], permission)
                .await
                .expect_err("project 3 is outside the caller's access");
            assert_eq!(denied.status_code, StatusCode::FORBIDDEN);
        }
    }

    /// Seeing a project is not being allowed to change it: a reader with
    /// access to every member still cannot rewrite their git settings.
    #[tokio::test]
    async fn the_instance_wide_permission_is_still_required() {
        let reader = user_auth(Role::Reader);
        let denied = require_on_projects(
            &reader,
            &checker(&[1, 2]),
            &[1, 2],
            MemberPermission::ProjectsWrite,
        )
        .await
        .expect_err("readers cannot write");
        assert_eq!(denied.status_code, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn without_a_checker_the_instance_permission_decides() {
        let auth = user_auth(Role::User);
        assert!(
            require_on_projects(&auth, &None, &[1, 2, 3], MemberPermission::ProjectsWrite)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn an_empty_set_requires_nothing() {
        let reader = user_auth(Role::Reader);
        assert!(
            require_on_projects(&reader, &checker(&[]), &[], MemberPermission::ProjectsWrite)
                .await
                .is_ok()
        );
    }

    /// A checker with per-project roles: each listed project maps to the
    /// permission strings the caller holds there, and unlisted projects are
    /// outside the caller's access. `hidden` is what `hidden_project_ids`
    /// reports.
    struct PerProject {
        perms: std::collections::HashMap<i32, Vec<String>>,
    }

    #[async_trait::async_trait]
    impl ProjectAccessChecker for PerProject {
        async fn user_can_access_project(
            &self,
            _user_id: i32,
            project_id: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            Ok(self.perms.contains_key(&project_id))
        }

        async fn effective_project_permissions(
            &self,
            _user_id: i32,
            project_id: i32,
        ) -> Result<Option<Vec<String>>, Box<dyn std::error::Error + Send + Sync>> {
            Ok(Some(
                self.perms.get(&project_id).cloned().unwrap_or_default(),
            ))
        }
    }

    fn per_project(roles: &[(i32, &[&str])]) -> Option<Arc<dyn ProjectAccessChecker>> {
        Some(Arc::new(PerProject {
            perms: roles
                .iter()
                .map(|(id, perms)| (*id, perms.iter().map(|p| p.to_string()).collect()))
                .collect(),
        }))
    }

    fn route(id: i32, group_id: i32, source: i32, target: i32) -> project_group_routes::Model {
        let now = chrono::Utc::now();
        project_group_routes::Model {
            id,
            group_id,
            source_project_id: source,
            target_project_id: target,
            path_prefix: "/api".to_string(),
            strip_prefix: false,
            created_at: now,
            updated_at: now,
        }
    }

    const WRITE: &str = "projects:write";
    const READ: &str = "projects:read";

    #[tokio::test]
    async fn dropping_a_route_requires_write_on_the_other_endpoint() {
        // Caller administers A (10) but is only a viewer of B (20): removing
        // A from the group would silently delete the A -> B route.
        let auth = user_auth(Role::User);
        let checker = per_project(&[(10, &[WRITE]), (20, &[READ])]);
        let routes = [route(1, 5, 10, 20)];
        let denied = authorize_route_drops(&auth, &checker, &HashSet::new(), &routes)
            .await
            .expect_err("a viewer of the route's other end cannot drop the route");
        assert_eq!(denied.status_code, StatusCode::FORBIDDEN);

        // The same holds with the roles of the two ends swapped.
        let checker = per_project(&[(10, &[READ]), (20, &[WRITE])]);
        let denied = authorize_route_drops(&auth, &checker, &HashSet::new(), &routes)
            .await
            .expect_err("the departing project's own end is checked too");
        assert_eq!(denied.status_code, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn dropping_a_route_is_allowed_with_write_on_both_endpoints() {
        let auth = user_auth(Role::User);
        let checker = per_project(&[(10, &[WRITE]), (20, &[WRITE]), (30, &[WRITE])]);
        let routes = [route(1, 5, 10, 20), route(2, 5, 30, 10)];
        let authorized = authorize_route_drops(&auth, &checker, &HashSet::new(), &routes)
            .await
            .expect("write on every endpoint authorizes the drop");
        assert_eq!(authorized, [1, 2].into_iter().collect::<HashSet<i32>>());
    }

    #[tokio::test]
    async fn dropping_a_route_to_a_hidden_project_is_reported_as_missing() {
        // B (20) is hidden: the caller must not learn that a route to it
        // exists, exactly like delete-route's 404.
        let auth = user_auth(Role::User);
        let checker = per_project(&[(10, &[WRITE]), (20, &[WRITE])]);
        let hidden: HashSet<i32> = [20].into_iter().collect();
        let routes = [route(7, 5, 10, 20)];
        let denied = authorize_route_drops(&auth, &checker, &hidden, &routes)
            .await
            .expect_err("a route involving a hidden project cannot be dropped");
        assert_eq!(denied.status_code, StatusCode::NOT_FOUND);
        // The hidden project may sit on either end.
        let routes = [route(8, 5, 20, 10)];
        let denied = authorize_route_drops(&auth, &checker, &hidden, &routes)
            .await
            .expect_err("a route involving a hidden project cannot be dropped");
        assert_eq!(denied.status_code, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn no_routes_to_drop_requires_nothing_further() {
        let auth = user_auth(Role::User);
        let checker = per_project(&[]);
        let authorized = authorize_route_drops(&auth, &checker, &HashSet::new(), &[])
            .await
            .expect("nothing is dropped, so nothing needs authorizing");
        assert!(authorized.is_empty());
    }

    #[tokio::test]
    async fn instance_admins_and_unchecked_instances_may_drop_routes() {
        let routes = [route(1, 5, 10, 20)];
        // OSS instance without an access checker: the instance-wide role decides.
        let user = user_auth(Role::User);
        assert!(
            authorize_route_drops(&user, &None, &HashSet::new(), &routes)
                .await
                .is_ok()
        );
        // Instance admins are not narrowed by project roles.
        let admin = user_auth(Role::Admin);
        let checker = per_project(&[]);
        assert!(
            authorize_route_drops(&admin, &checker, &HashSet::new(), &routes)
                .await
                .is_ok()
        );
        // But a read-only role never is, with or without a checker.
        let reader = user_auth(Role::Reader);
        let denied = authorize_route_drops(&reader, &None, &HashSet::new(), &routes)
            .await
            .expect_err("readers cannot write");
        assert_eq!(denied.status_code, StatusCode::FORBIDDEN);
    }

    // -----------------------------------------------------------------
    // Deleting a group, end to end. These boot a TimescaleDB container and
    // skip (not fail) when no container runtime is available.
    // -----------------------------------------------------------------

    /// Keeps every audit event the handler records as `(operation, json)`.
    #[derive(Default)]
    struct RecordedAudit(std::sync::Mutex<Vec<(String, String)>>);

    #[async_trait::async_trait]
    impl AuditLogger for RecordedAudit {
        async fn create_audit_log(&self, operation: &dyn AuditOperation) -> anyhow::Result<()> {
            let json = operation.serialize()?;
            if let Ok(mut events) = self.0.lock() {
                events.push((operation.operation_type(), json));
            }
            Ok(())
        }
    }

    impl RecordedAudit {
        fn operations(&self) -> Vec<(String, String)> {
            self.0
                .lock()
                .map(|events| events.clone())
                .unwrap_or_default()
        }
    }

    fn request_metadata() -> RequestMetadata {
        RequestMetadata {
            ip_address: "127.0.0.1".to_string(),
            user_agent: "test".to_string(),
            headers: axum::http::HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "http://localhost".to_string(),
            scheme: "http".to_string(),
            host: "localhost".to_string(),
            is_secure: false,
        }
    }

    struct DeleteFixture {
        _db: temps_database::test_utils::TestDatabase,
        state: Arc<ProjectGroupsAppState>,
        audit: Arc<RecordedAudit>,
        conn: Arc<sea_orm::DatabaseConnection>,
        group_id: i32,
        route_id: i32,
    }

    /// A group of two projects `base` and `base + 1` with one route between
    /// them, behind `checker`.
    async fn delete_fixture(
        base: i32,
        checker: Option<Arc<dyn ProjectAccessChecker>>,
    ) -> Option<DeleteFixture> {
        use crate::repository::tests::{insert_project, repository_service};
        use sea_orm::ActiveModelTrait;
        use sea_orm::ActiveValue::Set;

        let db = temps_database::test_utils::TestDatabase::with_migrations()
            .await
            .ok()?;
        let conn = db.connection_arc();
        insert_project(conn.as_ref(), base, "front").await;
        insert_project(conn.as_ref(), base + 1, "api").await;
        let service = Arc::new(ProjectGroupService::new(conn.clone()));
        let group = service
            .create_group(crate::service::CreateProjectGroupRequest {
                name: "Delete Me".to_string(),
                slug: Some("delete-me".to_string()),
                description: None,
                project_ids: vec![base, base + 1],
            })
            .await
            .ok()?;
        let now = chrono::Utc::now();
        let route = project_group_routes::ActiveModel {
            group_id: Set(group.group.id),
            source_project_id: Set(base),
            target_project_id: Set(base + 1),
            path_prefix: Set("/api".to_string()),
            strip_prefix: Set(false),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(conn.as_ref())
        .await
        .ok()?;
        let encryption = Arc::new(temps_core::EncryptionService::new(&"0".repeat(64)).ok()?);
        let audit = Arc::new(RecordedAudit::default());
        let state = Arc::new(ProjectGroupsAppState {
            service,
            env_vars: Arc::new(GroupEnvVarService::new(conn.clone(), encryption)),
            repository: Arc::new(repository_service(conn.clone()).await?),
            routes: Arc::new(crate::routes::GroupRouteService::new(conn.clone())),
            audit: audit.clone(),
            project_access_checker: checker,
        });
        Some(DeleteFixture {
            _db: db,
            state,
            audit,
            conn,
            group_id: group.group.id,
            route_id: route.id,
        })
    }

    async fn call_delete(
        fixture: &DeleteFixture,
        auth: temps_auth::context::AuthContext,
    ) -> Result<StatusCode, Problem> {
        delete_project_group(
            RequireAuth(auth),
            State(fixture.state.clone()),
            Extension(request_metadata()),
            Path(fixture.group_id),
        )
        .await
        .map(|response| response.into_response().status())
    }

    const ENV_WRITE: &str = "environments:write";

    #[tokio::test]
    async fn deleting_a_group_requires_write_on_both_ends_of_each_route() {
        // The caller manages the env vars of both members and administers the
        // route's source, but is only a viewer of its target: deleting the
        // group would silently delete the route, which deleting the route
        // directly would refuse.
        let base = 9811;
        let checker = per_project(&[(base, &[WRITE, ENV_WRITE]), (base + 1, &[READ, ENV_WRITE])]);
        let Some(fixture) = delete_fixture(base, checker).await else {
            eprintln!("Docker/TimescaleDB unavailable, skipping group deletion handler test");
            return;
        };

        let denied = call_delete(&fixture, user_auth(Role::User))
            .await
            .expect_err("a viewer of the route's target cannot delete the group");
        assert_eq!(denied.status_code, StatusCode::FORBIDDEN);

        // Nothing was deleted and nothing was audited.
        assert!(fixture
            .state
            .service
            .get_group(fixture.group_id)
            .await
            .is_ok());
        assert_eq!(
            fixture
                .state
                .service
                .routes_of_group(fixture.group_id)
                .await
                .map(|routes| routes.len())
                .ok(),
            Some(1)
        );
        assert!(fixture.audit.operations().is_empty());
    }

    #[tokio::test]
    async fn deleting_a_group_audits_the_routes_it_drops() {
        let base = 9821;
        let checker = per_project(&[(base, &[WRITE, ENV_WRITE]), (base + 1, &[WRITE, ENV_WRITE])]);
        let Some(fixture) = delete_fixture(base, checker).await else {
            eprintln!("Docker/TimescaleDB unavailable, skipping group deletion handler test");
            return;
        };

        let status = call_delete(&fixture, user_auth(Role::User))
            .await
            .expect("write on every route endpoint authorizes the deletion");
        assert_eq!(status, StatusCode::NO_CONTENT);

        assert!(matches!(
            fixture.state.service.get_group(fixture.group_id).await,
            Err(ProjectGroupError::NotFound { .. })
        ));
        use sea_orm::EntityTrait;
        let remaining = project_group_routes::Entity::find_by_id(fixture.route_id)
            .one(fixture.conn.as_ref())
            .await
            .expect("route lookup");
        assert!(remaining.is_none(), "the route goes with its group");

        let events = fixture.audit.operations();
        let operations: Vec<&str> = events.iter().map(|(op, _)| op.as_str()).collect();
        assert_eq!(
            operations,
            vec!["PROJECT_GROUP_ROUTE_REMOVED", "PROJECT_GROUP_DELETED"]
        );
        // The group no longer exists to be looked up, so the route event must
        // carry the slug it was deleted under.
        assert!(
            events[0].1.contains("\"group_slug\":\"delete-me\""),
            "route audit lost the group slug: {}",
            events[0].1
        );
        assert!(events[1].1.contains(&format!("{}", base)));
    }

    /// [`PerProject`] that also reports a set of projects as hidden from the
    /// caller, as a team-scoped checker does.
    struct HidingChecker {
        inner: PerProject,
        hidden: Vec<i32>,
    }

    #[async_trait::async_trait]
    impl ProjectAccessChecker for HidingChecker {
        async fn user_can_access_project(
            &self,
            user_id: i32,
            project_id: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            if self.hidden.contains(&project_id) {
                return Ok(false);
            }
            self.inner
                .user_can_access_project(user_id, project_id)
                .await
        }

        async fn effective_project_permissions(
            &self,
            user_id: i32,
            project_id: i32,
        ) -> Result<Option<Vec<String>>, Box<dyn std::error::Error + Send + Sync>> {
            self.inner
                .effective_project_permissions(user_id, project_id)
                .await
        }

        async fn hidden_project_ids(
            &self,
            _user_id: i32,
        ) -> Result<Option<Vec<i32>>, Box<dyn std::error::Error + Send + Sync>> {
            Ok(Some(self.hidden.clone()))
        }
    }

    fn hiding(roles: &[(i32, &[&str])], hidden: &[i32]) -> Option<Arc<dyn ProjectAccessChecker>> {
        Some(Arc::new(HidingChecker {
            inner: PerProject {
                perms: roles
                    .iter()
                    .map(|(id, perms)| (*id, perms.iter().map(|p| p.to_string()).collect()))
                    .collect(),
            },
            hidden: hidden.to_vec(),
        }))
    }

    async fn assert_group_untouched(fixture: &DeleteFixture) {
        assert!(fixture
            .state
            .service
            .get_group(fixture.group_id)
            .await
            .is_ok());
        assert_eq!(
            fixture
                .state
                .service
                .routes_of_group(fixture.group_id)
                .await
                .map(|routes| routes.len())
                .ok(),
            Some(1)
        );
        assert!(fixture.audit.operations().is_empty());
    }

    #[tokio::test]
    async fn deleting_a_group_whose_members_are_all_hidden_is_not_found() {
        // Both route endpoints are hidden from the caller: the group and its
        // route must be reported exactly like a missing group.
        let base = 9831;
        let perms: &[(i32, &[&str])] =
            &[(base, &[WRITE, ENV_WRITE]), (base + 1, &[WRITE, ENV_WRITE])];
        let Some(fixture) = delete_fixture(base, hiding(perms, &[base, base + 1])).await else {
            eprintln!("Docker/TimescaleDB unavailable, skipping group deletion handler test");
            return;
        };

        let denied = call_delete(&fixture, user_auth(Role::User))
            .await
            .expect_err("a fully hidden group cannot be deleted");
        assert_eq!(denied.status_code, StatusCode::NOT_FOUND);
        assert_group_untouched(&fixture).await;
    }

    #[tokio::test]
    async fn deleting_a_group_with_a_hidden_member_is_forbidden() {
        // One endpoint of the route is hidden: deleting the group would
        // silently delete a route the caller cannot even see.
        let base = 9841;
        let perms: &[(i32, &[&str])] =
            &[(base, &[WRITE, ENV_WRITE]), (base + 1, &[WRITE, ENV_WRITE])];
        let Some(fixture) = delete_fixture(base, hiding(perms, &[base + 1])).await else {
            eprintln!("Docker/TimescaleDB unavailable, skipping group deletion handler test");
            return;
        };

        let denied = call_delete(&fixture, user_auth(Role::User))
            .await
            .expect_err("a partially hidden group cannot be deleted");
        assert_eq!(denied.status_code, StatusCode::FORBIDDEN);
        assert_group_untouched(&fixture).await;
    }

    #[tokio::test]
    async fn deleting_a_missing_group_is_not_found_and_audits_nothing() {
        let base = 9851;
        let perms: &[(i32, &[&str])] =
            &[(base, &[WRITE, ENV_WRITE]), (base + 1, &[WRITE, ENV_WRITE])];
        let Some(mut fixture) = delete_fixture(base, per_project(perms)).await else {
            eprintln!("Docker/TimescaleDB unavailable, skipping group deletion handler test");
            return;
        };
        let real_group = fixture.group_id;
        fixture.group_id = 999_999;

        let denied = call_delete(&fixture, user_auth(Role::User))
            .await
            .expect_err("a missing group cannot be deleted");
        assert_eq!(denied.status_code, StatusCode::NOT_FOUND);
        assert!(fixture.audit.operations().is_empty());
        fixture.group_id = real_group;
        assert_group_untouched(&fixture).await;
    }

    fn minimal_group(id: i32, member_ids: &[i32]) -> ProjectGroupWithMembers {
        let now = chrono::Utc::now();
        let group = temps_entities::project_groups::Model {
            id,
            name: "Test Group".to_string(),
            slug: "test-group".to_string(),
            description: None,
            git_provider_connection_id: None,
            repo_owner: None,
            repo_name: None,
            main_branch: None,
            public_provider: None,
            created_at: now,
            updated_at: now,
        };
        ProjectGroupWithMembers {
            group,
            members: member_ids
                .iter()
                .map(|&mid| {
                    let now = chrono::Utc::now();
                    temps_entities::projects::Model {
                        id: mid,
                        name: format!("project-{mid}"),
                        repo_name: "repo".to_string(),
                        repo_owner: "owner".to_string(),
                        directory: String::new(),
                        pull_only_root_directory: false,
                        main_branch: "main".to_string(),
                        preset: temps_entities::preset::Preset::Dockerfile,
                        preset_config: None,
                        deployment_config: None,
                        created_at: now,
                        updated_at: now,
                        slug: format!("project-{mid}"),
                        is_deleted: false,
                        deleted_at: None,
                        last_deployment: None,
                        is_public_repo: false,
                        git_url: None,
                        git_provider_connection_id: None,
                        attack_mode: false,
                        ai_alert_summaries_enabled: None,
                        ai_debug_chat_enabled: None,
                        ai_write_actions_enabled: false,
                        error_source_context_enabled: false,
                        vulnerability_scanning_enabled: false,
                        error_source_root: None,
                        enable_preview_environments: false,
                        preview_envs_on_demand: false,
                        preview_envs_idle_timeout_seconds: 300,
                        preview_envs_wake_timeout_seconds: 30,
                        source_type: Default::default(),
                        project_type: temps_entities::types::ProjectType::Server,
                        allow_alternate_sources: None,
                        hosting: Default::default(),
                        template_slug: None,
                        service_template: None,
                        gitlab_webhook_id: None,
                        gitlab_webhook_signing_token: None,
                        gitea_webhook_signing_token: None,
                        github_webhook_id: None,
                        github_webhook_signing_token: None,
                        bitbucket_webhook_token: None,
                        bitbucket_webhook_hook_id: None,
                        generic_webhook_token: None,
                        cross_project_trace_sharing: true,
                        serves_own_temps_api: false,
                        ai_api_traffic_summary_enabled: None,
                        image_retention_hours: None,
                        cloud_telemetry_fidelity: Default::default(),
                        cloud_telemetry_attribute_allowlist: Vec::new(),
                        cloud_telemetry_write_mode: Default::default(),
                        cloud_analytics_write_mode: Default::default(),
                        instantiation_token: None,
                    }
                })
                .collect(),
            inherits_repository: HashSet::new(),
        }
    }

    #[test]
    fn fully_hidden_group_returns_not_found() {
        // A group where all members are hidden -> not found (not 403, to avoid
        // leaking group existence).
        let group = minimal_group(1, &[10, 20]);
        let mut hidden = HashSet::new();
        hidden.insert(10);
        hidden.insert(20);
        let err = visible_or_not_found(group, &hidden, ProjectGroupError::NotFound { group_id: 1 })
            .expect_err("a fully-hidden group must be treated as not found");
        assert_eq!(
            err.status_code,
            StatusCode::NOT_FOUND,
            "fully-hidden group must produce 404"
        );
    }

    #[test]
    fn partially_visible_group_passes_visibility_check() {
        let group = minimal_group(1, &[10, 20]);
        let mut hidden = HashSet::new();
        hidden.insert(20); // project 10 is visible, so the group is visible
        let result =
            visible_or_not_found(group, &hidden, ProjectGroupError::NotFound { group_id: 1 });
        assert!(
            result.is_ok(),
            "a group with at least one visible member must be accessible"
        );
    }

    #[test]
    fn empty_group_is_always_visible() {
        let group = minimal_group(1, &[]);
        let hidden = HashSet::new();
        assert!(
            visible_or_not_found(group, &hidden, ProjectGroupError::NotFound { group_id: 1 })
                .is_ok(),
            "a group with no members is always visible"
        );
    }

    fn deployment_token_auth(project_id: i32) -> temps_auth::context::AuthContext {
        temps_auth::context::AuthContext::new_deployment_token(
            project_id,
            None,
            None,
            1,
            "test-token".to_string(),
            vec![],
        )
    }

    #[test]
    fn deployment_token_is_rejected_by_reject_deployment_token() {
        let auth = deployment_token_auth(42);
        let err = reject_deployment_token(&auth)
            .expect_err("deployment tokens must be rejected for group management");
        assert_eq!(
            err.status_code,
            StatusCode::FORBIDDEN,
            "deployment token rejection must produce 403 Forbidden"
        );
    }

    #[test]
    fn user_session_passes_reject_deployment_token() {
        let auth = user_auth(Role::User);
        assert!(
            reject_deployment_token(&auth).is_ok(),
            "a normal user session must not be rejected"
        );
    }

    #[test]
    fn admin_session_passes_reject_deployment_token() {
        let auth = user_auth(Role::Admin);
        assert!(
            reject_deployment_token(&auth).is_ok(),
            "an admin session must not be rejected"
        );
    }

    #[tokio::test]
    async fn fully_hidden_group_returns_no_members() {
        // A group whose every member project is hidden should surface no
        // project IDs to the caller, so that hidden membership is not inferred.
        let auth = user_auth(Role::User);
        // All projects are hidden (not in the allowed set).
        let allowed = checker(&[]);
        let hidden = hidden_projects_from_set(&[10, 20, 30], &auth, &allowed).await;
        assert!(
            [10, 20, 30].iter().all(|id| hidden.contains(id)),
            "all projects should be hidden from a caller with no access"
        );
    }

    /// Helper used only in tests: given a list of project IDs and an access checker,
    /// compute which ones are hidden from the caller.
    async fn hidden_projects_from_set(
        project_ids: &[i32],
        auth: &temps_auth::context::AuthContext,
        checker: &Option<Arc<dyn ProjectAccessChecker>>,
    ) -> HashSet<i32> {
        if auth.is_admin() {
            return HashSet::new();
        }
        let Some(ch) = checker.as_ref() else {
            return HashSet::new();
        };
        let user_id = match auth.user_id_opt() {
            Some(id) => id,
            None => return project_ids.iter().copied().collect(),
        };
        let mut hidden = HashSet::new();
        for &id in project_ids {
            let accessible = ch
                .user_can_access_project(user_id, id)
                .await
                .unwrap_or(false);
            if !accessible {
                hidden.insert(id);
            }
        }
        hidden
    }
}
