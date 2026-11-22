// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Error groups and the project's repository (ADR 111, D5): whether the
//! repository can be reached, issues opened from a group or linked to it,
//! the commits that last touched the failing code, and ownership rules.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get, post},
    Extension, Json, Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use temps_authz::{permission_guard, project_access_guard, project_scope_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use temps_core::RequestMetadata;
use utoipa::{OpenApi, ToSchema};

use super::audit::{
    AuditContext, AuditedOwnershipRule, ErrorIssueLinkAudit, ErrorOwnershipRulesAudit,
};
use super::types::AppState;
use crate::services::source_control::ownership::{OwnershipRule, OwnershipRuleInput};
use crate::services::source_control::{IssueLink, SourceControlStatus, SuspectCommits};
use crate::services::types::ErrorTrackingError;

#[derive(OpenApi)]
#[openapi(
    paths(
        get_error_source_control,
        list_error_group_issues,
        create_error_group_issue,
        link_error_group_issue,
        refresh_error_group_issues,
        unlink_error_group_issue,
        get_error_group_suspect_commits,
        list_error_ownership_rules,
        replace_error_ownership_rules,
    ),
    components(schemas(
        SourceControlStatusView,
        IssueLinkView,
        IssueLinkList,
        CreateIssueRequest,
        LinkIssueRequest,
        SuspectCommitView,
        SuspectCommitsView,
        OwnershipRuleView,
        OwnershipRuleList,
        OwnershipRuleRequest,
        ReplaceOwnershipRulesRequest,
    )),
    tags((name = "error-tracking", description = "Error tracking"))
)]
pub struct SourceControlApiDoc;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/projects/{project_id}/error-tracking/source-control",
            get(get_error_source_control),
        )
        .route(
            "/projects/{project_id}/error-groups/{group_id}/issues",
            get(list_error_group_issues).post(create_error_group_issue),
        )
        .route(
            "/projects/{project_id}/error-groups/{group_id}/issues/link",
            post(link_error_group_issue),
        )
        .route(
            "/projects/{project_id}/error-groups/{group_id}/issues/refresh",
            post(refresh_error_group_issues),
        )
        .route(
            "/projects/{project_id}/error-groups/{group_id}/issues/{link_id}",
            delete(unlink_error_group_issue),
        )
        .route(
            "/projects/{project_id}/error-groups/{group_id}/suspect-commits",
            get(get_error_group_suspect_commits),
        )
        .route(
            "/projects/{project_id}/error-ownership-rules",
            get(list_error_ownership_rules).put(replace_error_ownership_rules),
        )
}

/// Whether the project's repository can be reached for issues and commits.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SourceControlStatusView {
    /// Issues can be opened and commits read.
    pub configured: bool,
    /// `github`, `gitlab`, or the provider that is not supported.
    pub provider: Option<String>,
    /// `owner/name`.
    pub repository: Option<String>,
    pub web_url: Option<String>,
    /// What is missing, when not configured.
    pub reason: Option<String>,
    /// The console page that fixes it.
    pub setup_path: Option<String>,
}

impl From<SourceControlStatus> for SourceControlStatusView {
    fn from(s: SourceControlStatus) -> Self {
        Self {
            configured: s.configured,
            provider: s.provider,
            repository: s.repository,
            web_url: s.web_url,
            reason: s.reason,
            setup_path: s.setup_path,
        }
    }
}

/// An issue linked to an error group.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct IssueLinkView {
    pub id: i64,
    pub group_id: i32,
    /// `github` or `gitlab` for an issue of the project's repository, whose
    /// state Temps reads; `link` for any other URL.
    pub provider: String,
    pub repository: Option<String>,
    pub number: Option<i64>,
    pub url: String,
    pub title: String,
    /// `open` or `closed` when last read; absent for plain links.
    pub state: Option<String>,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub state_checked_at: Option<DateTime<Utc>>,
    pub created_by: Option<i32>,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: DateTime<Utc>,
}

impl From<IssueLink> for IssueLinkView {
    fn from(l: IssueLink) -> Self {
        Self {
            id: l.id,
            group_id: l.group_id,
            provider: l.provider,
            repository: l.repository,
            number: l.number,
            url: l.url,
            title: l.title,
            state: l.state,
            state_checked_at: l.state_checked_at,
            created_by: l.created_by,
            created_at: l.created_at,
        }
    }
}

/// The issues of a group.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct IssueLinkList {
    pub data: Vec<IssueLinkView>,
    /// After a refresh: how many issues could not be read (they keep their
    /// last state).
    pub failed_refreshes: usize,
}

/// Open an issue for a group in the project's repository.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct CreateIssueRequest {
    /// The issue's title; the group's title when absent.
    #[serde(default)]
    pub title: Option<String>,
}

/// Link an existing issue by its URL.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LinkIssueRequest {
    /// An http(s) URL: an issue of the project's GitHub or GitLab repository
    /// (its state is then read), or any other tracker's issue.
    pub url: String,
    /// What to call it; the issue's own title, or the URL, when absent.
    #[serde(default)]
    pub title: Option<String>,
}

/// A commit that touched the failing file.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SuspectCommitView {
    pub sha: String,
    pub summary: String,
    pub author_name: Option<String>,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub authored_at: Option<DateTime<Utc>>,
    pub url: String,
}

/// The newest commits that touched the file of the latest event's
/// innermost application frame.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SuspectCommitsView {
    pub path: Option<String>,
    pub function: Option<String>,
    pub lineno: Option<i64>,
    /// The deployed commit the history was read at; the default branch
    /// when absent.
    pub at: Option<String>,
    pub commits: Vec<SuspectCommitView>,
    /// Why there are no commits, when there are none.
    pub reason: Option<String>,
}

impl From<SuspectCommits> for SuspectCommitsView {
    fn from(s: SuspectCommits) -> Self {
        Self {
            path: s.path,
            function: s.function,
            lineno: s.lineno,
            at: s.at,
            commits: s
                .commits
                .into_iter()
                .map(|c| SuspectCommitView {
                    sha: c.sha,
                    summary: c.summary,
                    author_name: c.author_name,
                    authored_at: c.authored_at,
                    url: c.url,
                })
                .collect(),
            reason: s.reason,
        }
    }
}

/// An ownership rule.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OwnershipRuleView {
    pub id: i32,
    /// Rules are tried in this order.
    pub position: i32,
    pub pattern: String,
    pub owner_user_id: i32,
    pub owner_name: Option<String>,
    #[schema(value_type = String, format = DateTime)]
    pub created_at: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: DateTime<Utc>,
}

impl From<OwnershipRule> for OwnershipRuleView {
    fn from(r: OwnershipRule) -> Self {
        Self {
            id: r.id,
            position: r.position,
            pattern: r.pattern,
            owner_user_id: r.owner_user_id,
            owner_name: r.owner_name,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

/// The project's ownership rules, in order.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OwnershipRuleList {
    pub data: Vec<OwnershipRuleView>,
}

/// One rule as written.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OwnershipRuleRequest {
    /// A repository path: `src/billing/` (everything under it), `*.py` (a
    /// file name anywhere), `src/**/cart.ts`.
    pub pattern: String,
    /// The member a matching new group is assigned to.
    pub owner_user_id: i32,
}

/// Replace every ownership rule of the project, in order.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ReplaceOwnershipRulesRequest {
    /// At most 50; the first match wins.
    pub rules: Vec<OwnershipRuleRequest>,
}

fn audit_context(auth: &temps_authz::AuthContext, metadata: RequestMetadata) -> AuditContext {
    AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address),
        user_agent: metadata.user_agent,
    }
}

async fn audit_link(
    state: &AppState,
    context: AuditContext,
    project_id: i32,
    link: &IssueLink,
    action: &'static str,
) {
    let audit = ErrorIssueLinkAudit {
        context,
        project_id,
        group_id: link.group_id,
        link_id: link.id,
        provider: link.provider.clone(),
        repository: link.repository.clone(),
        number: link.number,
        action,
    };
    if let Err(error) = state.audit_service.create_audit_log(&audit).await {
        tracing::error!(
            project_id,
            group_id = link.group_id,
            link_id = link.id,
            "Failed to create issue link audit log: {error}"
        );
    }
}

/// Whether issues can be opened in the project's repository
#[utoipa::path(
    get,
    path = "/projects/{project_id}/error-tracking/source-control",
    responses(
        (status = 200, description = "The repository, or what is missing", body = SourceControlStatusView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "No access to the project"),
        (status = 500, description = "Internal server error")
    ),
    params(("project_id" = i32, Path, description = "Project ID")),
    tag = "error-tracking",
    security(("bearer_auth" = []))
)]
pub async fn get_error_source_control(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path(project_id): Path<i32>,
) -> Result<Json<SourceControlStatusView>, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let status = state
        .error_tracking_service
        .source_control
        .status(project_id)
        .await?;
    Ok(Json(status.into()))
}

/// List the issues linked to an error group
#[utoipa::path(
    get,
    path = "/projects/{project_id}/error-groups/{group_id}/issues",
    responses(
        (status = 200, description = "Linked issues, oldest first, with their last read state", body = IssueLinkList),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "No access to the project"),
        (status = 404, description = "No such group in the project"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("group_id" = i32, Path, description = "Error group ID")
    ),
    tag = "error-tracking",
    security(("bearer_auth" = []))
)]
pub async fn list_error_group_issues(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, group_id)): Path<(i32, i32)>,
) -> Result<Json<IssueLinkList>, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let data = state
        .error_tracking_service
        .source_control
        .list_links(project_id, group_id)
        .await?;
    Ok(Json(IssueLinkList {
        data: data.into_iter().map(Into::into).collect(),
        failed_refreshes: 0,
    }))
}

/// Open an issue for an error group in the project's repository
#[utoipa::path(
    post,
    path = "/projects/{project_id}/error-groups/{group_id}/issues",
    request_body = CreateIssueRequest,
    responses(
        (status = 201, description = "Issue opened and linked", body = IssueLinkView),
        (status = 400, description = "Title too long"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Error tracking write permission required"),
        (status = 404, description = "No such group in the project"),
        (status = 409, description = "No repository connected, a provider that is not GitHub or GitLab, an issue already open for the group, or one being opened"),
        (status = 502, description = "The provider refused or failed the request"),
        (status = 503, description = "This server has no git provider integration"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("group_id" = i32, Path, description = "Error group ID")
    ),
    tag = "error-tracking",
    security(("bearer_auth" = []))
)]
pub async fn create_error_group_issue(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, group_id)): Path<(i32, i32)>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CreateIssueRequest>,
) -> Result<(StatusCode, Json<IssueLinkView>), Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let link = state
        .error_tracking_service
        .source_control
        .create_issue(
            project_id,
            group_id,
            request.title.as_deref(),
            auth.user_id_opt(),
            Some(metadata.base_url.as_str()),
        )
        .await?;
    audit_link(
        &state,
        audit_context(&auth, metadata),
        project_id,
        &link,
        "opened",
    )
    .await;
    Ok((StatusCode::CREATED, Json(link.into())))
}

/// Link an existing issue to an error group by its URL
#[utoipa::path(
    post,
    path = "/projects/{project_id}/error-groups/{group_id}/issues/link",
    request_body = LinkIssueRequest,
    responses(
        (status = 201, description = "Issue linked", body = IssueLinkView),
        (status = 400, description = "Not an http(s) URL, or a title too long"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Error tracking write permission required"),
        (status = 404, description = "No such group in the project"),
        (status = 409, description = "Already linked, or the group has the maximum of 10 issues"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("group_id" = i32, Path, description = "Error group ID")
    ),
    tag = "error-tracking",
    security(("bearer_auth" = []))
)]
pub async fn link_error_group_issue(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, group_id)): Path<(i32, i32)>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<LinkIssueRequest>,
) -> Result<(StatusCode, Json<IssueLinkView>), Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let link = state
        .error_tracking_service
        .source_control
        .link_issue(
            project_id,
            group_id,
            &request.url,
            request.title.as_deref(),
            auth.user_id_opt(),
        )
        .await?;
    audit_link(
        &state,
        audit_context(&auth, metadata),
        project_id,
        &link,
        "linked",
    )
    .await;
    Ok((StatusCode::CREATED, Json(link.into())))
}

/// Read again whether the group's issues are open or closed
///
/// Reads each linked GitHub or GitLab issue once (at most 10) and stores
/// what it says. It changes nothing in the repository or in the group, so
/// it is not audited.
#[utoipa::path(
    post,
    path = "/projects/{project_id}/error-groups/{group_id}/issues/refresh",
    responses(
        (status = 200, description = "Linked issues with their state as read now", body = IssueLinkList),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "No access to the project"),
        (status = 404, description = "No such group in the project"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("group_id" = i32, Path, description = "Error group ID")
    ),
    tag = "error-tracking",
    security(("bearer_auth" = []))
)]
pub async fn refresh_error_group_issues(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, group_id)): Path<(i32, i32)>,
) -> Result<Json<IssueLinkList>, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let (data, failed) = state
        .error_tracking_service
        .source_control
        .refresh_links(project_id, group_id)
        .await?;
    Ok(Json(IssueLinkList {
        data: data.into_iter().map(Into::into).collect(),
        failed_refreshes: failed,
    }))
}

/// Unlink an issue from an error group; the issue itself is left as it is
#[utoipa::path(
    delete,
    path = "/projects/{project_id}/error-groups/{group_id}/issues/{link_id}",
    responses(
        (status = 204, description = "Unlinked"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Error tracking write permission required"),
        (status = 404, description = "No such link on the group"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("group_id" = i32, Path, description = "Error group ID"),
        ("link_id" = i64, Path, description = "Issue link ID")
    ),
    tag = "error-tracking",
    security(("bearer_auth" = []))
)]
pub async fn unlink_error_group_issue(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, group_id, link_id)): Path<(i32, i32, i64)>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<StatusCode, Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let link = state
        .error_tracking_service
        .source_control
        .unlink_issue(project_id, group_id, link_id, auth.user_id_opt())
        .await?;
    audit_link(
        &state,
        audit_context(&auth, metadata),
        project_id,
        &link,
        "unlinked",
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// The commits that last touched the code an error group fails in
#[utoipa::path(
    get,
    path = "/projects/{project_id}/error-groups/{group_id}/suspect-commits",
    responses(
        (status = 200, description = "Up to 5 commits that touched the file of the latest event's innermost application frame, or why there are none", body = SuspectCommitsView),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "No access to the project"),
        (status = 404, description = "No such group in the project"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("group_id" = i32, Path, description = "Error group ID")
    ),
    tag = "error-tracking",
    security(("bearer_auth" = []))
)]
pub async fn get_error_group_suspect_commits(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, group_id)): Path<(i32, i32)>,
) -> Result<Json<SuspectCommitsView>, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let commits = state
        .error_tracking_service
        .source_control
        .suspect_commits(project_id, group_id)
        .await?;
    Ok(Json(commits.into()))
}

/// List the project's ownership rules
#[utoipa::path(
    get,
    path = "/projects/{project_id}/error-ownership-rules",
    responses(
        (status = 200, description = "Rules in the order they are tried", body = OwnershipRuleList),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "No access to the project"),
        (status = 500, description = "Internal server error")
    ),
    params(("project_id" = i32, Path, description = "Project ID")),
    tag = "error-tracking",
    security(("bearer_auth" = []))
)]
pub async fn list_error_ownership_rules(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path(project_id): Path<i32>,
) -> Result<Json<OwnershipRuleList>, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let rules = state
        .error_tracking_service
        .source_control
        .ownership
        .list(project_id)
        .await?;
    Ok(Json(OwnershipRuleList {
        data: rules.into_iter().map(Into::into).collect(),
    }))
}

/// Replace the project's ownership rules
///
/// A new group is assigned to the owner of the first rule matching one of
/// its frames, innermost first. Existing groups are not reassigned.
#[utoipa::path(
    put,
    path = "/projects/{project_id}/error-ownership-rules",
    request_body = ReplaceOwnershipRulesRequest,
    responses(
        (status = 200, description = "The rules now in force", body = OwnershipRuleList),
        (status = 400, description = "A pattern that is not a repository path, more than 50 rules, or an owner who is not a user or cannot open the project"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Error tracking write permission required"),
        (status = 500, description = "Internal server error")
    ),
    params(("project_id" = i32, Path, description = "Project ID")),
    tag = "error-tracking",
    security(("bearer_auth" = []))
)]
pub async fn replace_error_ownership_rules(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path(project_id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<ReplaceOwnershipRulesRequest>,
) -> Result<Json<OwnershipRuleList>, Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);
    let rules: Vec<OwnershipRuleInput> = request
        .rules
        .iter()
        .map(|r| OwnershipRuleInput {
            pattern: r.pattern.clone(),
            owner_user_id: r.owner_user_id,
        })
        .collect();
    // An owner must be able to open the project the groups belong to.
    if let Some(checker) = state.project_access_checker.as_ref() {
        let owners: Vec<i32> = rules.iter().map(|r| r.owner_user_id).collect();
        let access = checker
            .users_can_access_project(&owners, project_id)
            .await
            .map_err(|error| {
                tracing::error!(project_id, %error, "Could not check ownership rule owners' project access");
                Problem::from(ErrorTrackingError::Validation(format!(
                    "could not check whether the owners can open project {project_id}: {error}"
                )))
            })?;
        if let Some(owner) = owners.iter().find(|id| access.get(id) != Some(&true)) {
            return Err(ErrorTrackingError::Validation(format!(
                "user {owner} cannot open project {project_id}, so they cannot own its errors"
            ))
            .into());
        }
    }
    let (now, previous) = state
        .error_tracking_service
        .source_control
        .ownership
        .replace(project_id, &rules)
        .await?;
    let audited = |rules: &[OwnershipRule]| {
        rules
            .iter()
            .map(|r| AuditedOwnershipRule {
                pattern: r.pattern.clone(),
                owner_user_id: r.owner_user_id,
            })
            .collect()
    };
    let audit = ErrorOwnershipRulesAudit {
        context: audit_context(&auth, metadata),
        project_id,
        previous: audited(&previous),
        rules: audited(&now),
    };
    if let Err(error) = state.audit_service.create_audit_log(&audit).await {
        tracing::error!(
            project_id,
            "Failed to create ownership rules audit log: {error}"
        );
    }
    Ok(Json(OwnershipRuleList {
        data: now.into_iter().map(Into::into).collect(),
    }))
}

#[cfg(test)]
mod tests;
