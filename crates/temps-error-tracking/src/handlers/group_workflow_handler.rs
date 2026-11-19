// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Issue workflow routes (ADR 111, D1): triage that acts on many error
//! groups at once, instead of one request per group.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    routing::post,
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_authz::{permission_guard, project_access_guard, project_scope_guard, RequireAuth};
use temps_core::problemdetails::Problem;
use temps_core::{DateTime, RequestMetadata};
use tracing::error;
use utoipa::{OpenApi, ToSchema};

use super::activity_handler::record_activity;
use super::audit::{
    AuditContext, ErrorGroupSplitAudit, ErrorGroupUnmergedAudit, ErrorGroupsBulkUpdatedAudit,
    ErrorGroupsMergedAudit,
};
use super::types::AppState;
use crate::services::group_workflow::{
    ActivityDetails, ActivityKind, BulkGroupAction, BulkGroupFilter, BulkGroupOutcome,
    BulkGroupSelector, IgnoreConditions, BULK_GROUP_LIMIT,
};

#[derive(OpenApi)]
#[openapi(
    paths(
        bulk_update_error_groups,
        merge_error_groups,
        unmerge_error_group,
        split_error_group,
        list_error_group_fingerprints,
    ),
    components(schemas(
        BulkErrorGroupAction,
        BulkErrorGroupFilter,
        BulkErrorGroupRequest,
        BulkErrorGroupResponse,
        IgnoreConditionsRequest,
        MergeErrorGroupsRequest,
        MergeErrorGroupsResponse,
        UnmergeErrorGroupResponse,
        SplitErrorGroupRequest,
        SplitErrorGroupResponse,
        ErrorGroupFingerprintResponse,
        ErrorGroupFingerprintsResponse,
    )),
    tags((
        name = "error-tracking",
        description = "Error tracking issue workflow endpoints"
    ))
)]
pub struct GroupWorkflowApiDoc;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/projects/{project_id}/error-groups/bulk",
            post(bulk_update_error_groups),
        )
        .route(
            "/projects/{project_id}/error-groups/{group_id}/merge",
            post(merge_error_groups),
        )
        .route(
            "/projects/{project_id}/error-groups/{group_id}/unmerge",
            post(unmerge_error_group),
        )
        .route(
            "/projects/{project_id}/error-groups/{group_id}/split",
            post(split_error_group),
        )
        .route(
            "/projects/{project_id}/error-groups/{group_id}/fingerprints",
            axum::routing::get(list_error_group_fingerprints),
        )
}

/// Status transition a bulk request applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BulkErrorGroupAction {
    /// Mark the selected groups resolved.
    Resolve,
    /// Reopen the selected groups.
    Unresolve,
    /// Ignore the selected groups.
    Ignore,
}

impl From<BulkErrorGroupAction> for BulkGroupAction {
    fn from(action: BulkErrorGroupAction) -> Self {
        match action {
            BulkErrorGroupAction::Resolve => Self::Resolve,
            BulkErrorGroupAction::Unresolve => Self::Unresolve,
            BulkErrorGroupAction::Ignore => Self::Ignore,
        }
    }
}

/// Filters selecting "every group matching", with the same meaning they have
/// on the list endpoint. `{}` selects every group of the project that is not
/// already in the target status.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct BulkErrorGroupFilter {
    /// Only groups currently in this status.
    pub status: Option<String>,
    /// Only groups first seen in this environment.
    pub environment_id: Option<i32>,
    /// With `end_date`: only groups with at least one event in the window.
    pub start_date: Option<DateTime>,
    /// With `start_date`: only groups with at least one event in the window.
    pub end_date: Option<DateTime>,
    /// Case-insensitive substring of the title, error type or message.
    /// Wildcards are matched literally.
    pub search: Option<String>,
}

impl From<BulkErrorGroupFilter> for BulkGroupFilter {
    fn from(filter: BulkErrorGroupFilter) -> Self {
        Self {
            status: filter.status,
            environment_id: filter.environment_id,
            start_date: filter.start_date.map(Into::into),
            end_date: filter.end_date.map(Into::into),
            search: filter.search,
        }
    }
}

/// Request body for `POST .../error-groups/bulk`. Provide exactly one of
/// `group_ids` or `filter`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BulkErrorGroupRequest {
    pub action: BulkErrorGroupAction,
    /// Explicit groups to update; every one must be in this project.
    pub group_ids: Option<Vec<i32>>,
    /// Update every group matching these filters, up to 1000 per request.
    pub filter: Option<BulkErrorGroupFilter>,
    /// With `action: ignore`: when the groups come back. Omitted, the next
    /// event reopens them.
    pub ignore: Option<IgnoreConditionsRequest>,
    /// With `status: resolved` (or `action: resolve`): resolved in the next
    /// release. Events from the project's newest release at this moment, or
    /// an older one, or with no release, keep it resolved; an event from a
    /// newer release reopens it. Fails when the project has no release yet.
    #[serde(default)]
    pub in_next_release: bool,
}

/// When an ignored error group comes back. With no field set the next event
/// reopens it; with fields set it stays ignored until any one is met.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct IgnoreConditionsRequest {
    /// Stay ignored until this time (at most 366 days ahead).
    pub until: Option<DateTime>,
    /// Stay ignored for this many more occurrences (1 to 1,000,000).
    pub event_count: Option<i32>,
    /// Stay ignored until this many new distinct users are affected
    /// (1 to 10,000).
    pub user_count: Option<i32>,
    /// Stay ignored whatever happens. Excludes the other fields.
    #[serde(default)]
    pub forever: bool,
}

impl From<IgnoreConditionsRequest> for IgnoreConditions {
    fn from(request: IgnoreConditionsRequest) -> Self {
        Self {
            until: request.until.map(Into::into),
            event_count: request.event_count,
            user_count: request.user_count,
            forever: request.forever,
        }
    }
}

/// Result of a bulk group update.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BulkErrorGroupResponse {
    pub action: BulkErrorGroupAction,
    /// Number of groups whose status changed.
    pub updated: u64,
    /// Ids of the groups whose status changed.
    pub updated_ids: Vec<i32>,
    /// Selected groups already in the target status.
    pub skipped: u64,
    /// Matching groups left for a follow-up request because this one hit the
    /// per-request limit. Always 0 when `group_ids` was used.
    pub remaining: u64,
}

impl BulkErrorGroupResponse {
    fn new(action: BulkErrorGroupAction, outcome: BulkGroupOutcome) -> Self {
        Self {
            action,
            updated: outcome.updated_ids.len() as u64,
            updated_ids: outcome.updated_ids,
            skipped: outcome.skipped,
            remaining: outcome.remaining,
        }
    }
}

/// Exactly one selector, so a request can never mean "these ids, or else
/// everything matching".
fn bulk_selector(request: &BulkErrorGroupRequest) -> Result<BulkGroupSelector, Problem> {
    match (&request.group_ids, &request.filter) {
        (Some(ids), None) => Ok(BulkGroupSelector::Ids(ids.clone())),
        (None, Some(filter)) => Ok(BulkGroupSelector::Filter(filter.clone().into())),
        (Some(_), Some(_)) => Err(temps_core::error_builder::bad_request()
            .detail("Provide either group_ids or filter, not both")
            .build()),
        (None, None) => Err(temps_core::error_builder::bad_request()
            .detail("Provide group_ids or filter to select the groups to update")
            .build()),
    }
}

/// Resolve, reopen or ignore many error groups of a project at once — either
/// the explicit `group_ids` or every group matching `filter`.
#[utoipa::path(
    post,
    path = "/projects/{project_id}/error-groups/bulk",
    tag = "error-tracking",
    operation_id = "bulkUpdateErrorGroups",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body = BulkErrorGroupRequest,
    responses(
        (status = 200, description = "Groups updated", body = BulkErrorGroupResponse),
        (status = 400, description = "Invalid selection or filter"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "A group in group_ids is not in this project"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn bulk_update_error_groups(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path(project_id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<BulkErrorGroupRequest>,
) -> Result<Json<BulkErrorGroupResponse>, Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let selector = bulk_selector(&request)?;
    let outcome = state
        .error_tracking_service
        .bulk_update_error_group_status(
            project_id,
            selector,
            request.action.into(),
            request.ignore.clone().map(Into::into),
            request.in_next_release,
        )
        .await?;
    let response = BulkErrorGroupResponse::new(request.action, outcome);
    record_activity(
        &state,
        project_id,
        &response.updated_ids,
        ActivityKind::StatusChanged,
        auth.user_id_opt(),
        ActivityDetails {
            status: Some(
                BulkGroupAction::from(request.action)
                    .target_status()
                    .to_string(),
            ),
            in_next_release: request.in_next_release,
            bulk: true,
            ..Default::default()
        },
    )
    .await;

    // Audited even when nothing changed: the attempt and its selection are
    // part of the record.
    let audit = ErrorGroupsBulkUpdatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        project_id,
        action: request.action,
        status: BulkGroupAction::from(request.action)
            .target_status()
            .to_string(),
        requested_group_ids: request.group_ids,
        filter: request.filter,
        ignore: request.ignore,
        in_next_release: request.in_next_release,
        updated_group_ids: response.updated_ids.clone(),
        skipped: response.skipped,
        remaining: response.remaining,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(
            project_id,
            "Failed to create audit log for bulk error-group update ({:?}): {}", request.action, e
        );
    }

    Ok(Json(response))
}

/// The documented per-request cap, re-exported so clients and tests agree on
/// one number.
pub const BULK_ERROR_GROUP_LIMIT: usize = BULK_GROUP_LIMIT;

// ===== Merge, unmerge and split =====

/// Request body for `POST .../error-groups/{group_id}/merge`. The path group
/// is the one that survives.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MergeErrorGroupsRequest {
    /// Groups to merge into the path group; at most 50 per request.
    pub source_group_ids: Vec<i32>,
}

/// Result of a merge.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MergeErrorGroupsResponse {
    /// The group that survived; future events of every merged fingerprint
    /// land here.
    pub target_group_id: i32,
    pub merged_group_ids: Vec<i32>,
    /// Fingerprints now routing to the surviving group.
    pub moved_fingerprints: u64,
    /// Events the surviving group accounts for after the merge.
    pub total_count: i64,
}

/// Result of taking a group back out of a merge.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UnmergeErrorGroupResponse {
    pub group_id: i32,
    /// The group it had been merged into.
    pub target_group_id: i32,
    pub moved_fingerprints: u64,
}

/// Request body for `POST .../error-groups/{group_id}/split`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SplitErrorGroupRequest {
    /// Fingerprints to move out; at most 200 per request, and the group must
    /// keep at least one.
    pub fingerprints: Vec<String>,
    /// Title for the new group. Defaults to the original title with
    /// "(split)" appended.
    pub title: Option<String>,
}

/// Result of a split.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SplitErrorGroupResponse {
    pub group_id: i32,
    /// The new group; future events of the moved fingerprints land here.
    pub new_group_id: i32,
    pub moved_fingerprints: u64,
}

/// One fingerprint routing to a group.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ErrorGroupFingerprintResponse {
    pub fingerprint_hash: String,
    /// Set when a merge moved this fingerprint here; unmerging that group
    /// moves it back, and a split cannot take it.
    pub merged_from_group_id: Option<i32>,
}

/// The fingerprints of one group.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ErrorGroupFingerprintsResponse {
    pub group_id: i32,
    pub fingerprints: Vec<ErrorGroupFingerprintResponse>,
    /// Groups merged into this one.
    pub merged_group_ids: Vec<i32>,
}

/// Merge other groups into this one: their fingerprints route here, so their
/// future events land here, and their counts are added to this group's.
///
/// Stored events keep the group they were ingested into, so nothing is
/// rewritten and unmerging restores both groups exactly.
#[utoipa::path(
    post,
    path = "/projects/{project_id}/error-groups/{group_id}/merge",
    tag = "error-tracking",
    operation_id = "mergeErrorGroups",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("group_id" = i32, Path, description = "Surviving error group ID")
    ),
    request_body = MergeErrorGroupsRequest,
    responses(
        (status = 200, description = "Groups merged", body = MergeErrorGroupsResponse),
        (status = 400, description = "Invalid selection"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "A named group is not in this project"),
        (status = 409, description = "A named group is already merged, or has groups merged into it"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn merge_error_groups(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, group_id)): Path<(i32, i32)>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<MergeErrorGroupsRequest>,
) -> Result<Json<MergeErrorGroupsResponse>, Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let outcome = state
        .error_tracking_service
        .workflow
        .merge_groups(project_id, group_id, request.source_group_ids.clone())
        .await?;
    record_activity(
        &state,
        project_id,
        &[outcome.target_group_id],
        ActivityKind::Merged,
        auth.user_id_opt(),
        ActivityDetails {
            group_ids: outcome.merged_group_ids.clone(),
            fingerprints: Some(outcome.moved_fingerprints),
            ..Default::default()
        },
    )
    .await;

    let audit = ErrorGroupsMergedAudit {
        context: audit_context(&auth, &metadata),
        project_id,
        target_group_id: outcome.target_group_id,
        merged_group_ids: outcome.merged_group_ids.clone(),
        moved_fingerprints: outcome.moved_fingerprints,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(
            project_id,
            group_id, "Failed to create audit log for error-group merge: {}", e
        );
    }

    Ok(Json(MergeErrorGroupsResponse {
        target_group_id: outcome.target_group_id,
        merged_group_ids: outcome.merged_group_ids,
        moved_fingerprints: outcome.moved_fingerprints,
        total_count: outcome.total_count,
    }))
}

/// Take this group back out of the merge it is part of: its fingerprints
/// route to it again and the count it contributed is subtracted from the
/// group it was merged into.
#[utoipa::path(
    post,
    path = "/projects/{project_id}/error-groups/{group_id}/unmerge",
    tag = "error-tracking",
    operation_id = "unmergeErrorGroup",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("group_id" = i32, Path, description = "Merged-away error group ID")
    ),
    responses(
        (status = 200, description = "Group taken out of the merge", body = UnmergeErrorGroupResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "The group is not in this project"),
        (status = 409, description = "The group is not merged into another group"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn unmerge_error_group(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, group_id)): Path<(i32, i32)>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<Json<UnmergeErrorGroupResponse>, Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let outcome = state
        .error_tracking_service
        .workflow
        .unmerge_group(project_id, group_id)
        .await?;
    for (on, other) in [
        (outcome.group_id, outcome.target_group_id),
        (outcome.target_group_id, outcome.group_id),
    ] {
        record_activity(
            &state,
            project_id,
            &[on],
            ActivityKind::Unmerged,
            auth.user_id_opt(),
            ActivityDetails {
                group_ids: vec![other],
                fingerprints: Some(outcome.moved_fingerprints),
                ..Default::default()
            },
        )
        .await;
    }

    let audit = ErrorGroupUnmergedAudit {
        context: audit_context(&auth, &metadata),
        project_id,
        group_id: outcome.group_id,
        target_group_id: outcome.target_group_id,
        moved_fingerprints: outcome.moved_fingerprints,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(
            project_id,
            group_id, "Failed to create audit log for error-group unmerge: {}", e
        );
    }

    Ok(Json(UnmergeErrorGroupResponse {
        group_id: outcome.group_id,
        target_group_id: outcome.target_group_id,
        moved_fingerprints: outcome.moved_fingerprints,
    }))
}

/// Split named fingerprints out of this group into a new one, so their future
/// events are tracked separately. Stored events stay with this group.
#[utoipa::path(
    post,
    path = "/projects/{project_id}/error-groups/{group_id}/split",
    tag = "error-tracking",
    operation_id = "splitErrorGroup",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("group_id" = i32, Path, description = "Error group ID")
    ),
    request_body = SplitErrorGroupRequest,
    responses(
        (status = 200, description = "Fingerprints split out", body = SplitErrorGroupResponse),
        (status = 400, description = "Invalid selection"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "The group or a fingerprint is not in this project"),
        (status = 409, description = "The group is merged into another group"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn split_error_group(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, group_id)): Path<(i32, i32)>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<SplitErrorGroupRequest>,
) -> Result<Json<SplitErrorGroupResponse>, Problem> {
    permission_guard!(auth, ErrorTrackingWrite);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let outcome = state
        .error_tracking_service
        .workflow
        .split_group(
            project_id,
            group_id,
            request.fingerprints.clone(),
            request.title.clone(),
        )
        .await?;
    for (on, other) in [
        (outcome.group_id, outcome.new_group_id),
        (outcome.new_group_id, outcome.group_id),
    ] {
        record_activity(
            &state,
            project_id,
            &[on],
            ActivityKind::Split,
            auth.user_id_opt(),
            ActivityDetails {
                group_ids: vec![other],
                fingerprints: Some(outcome.moved_fingerprints),
                ..Default::default()
            },
        )
        .await;
    }

    let audit = ErrorGroupSplitAudit {
        context: audit_context(&auth, &metadata),
        project_id,
        group_id: outcome.group_id,
        new_group_id: outcome.new_group_id,
        moved_fingerprints: outcome.moved_fingerprints,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(
            project_id,
            group_id, "Failed to create audit log for error-group split: {}", e
        );
    }

    Ok(Json(SplitErrorGroupResponse {
        group_id: outcome.group_id,
        new_group_id: outcome.new_group_id,
        moved_fingerprints: outcome.moved_fingerprints,
    }))
}

/// The fingerprints routing to this group, and the groups merged into it.
#[utoipa::path(
    get,
    path = "/projects/{project_id}/error-groups/{group_id}/fingerprints",
    tag = "error-tracking",
    operation_id = "listErrorGroupFingerprints",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("group_id" = i32, Path, description = "Error group ID")
    ),
    responses(
        (status = 200, description = "Fingerprints of the group", body = ErrorGroupFingerprintsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "The group is not in this project"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_error_group_fingerprints(
    State(state): State<Arc<AppState>>,
    RequireAuth(auth): RequireAuth,
    Path((project_id, group_id)): Path<(i32, i32)>,
) -> Result<Json<ErrorGroupFingerprintsResponse>, Problem> {
    permission_guard!(auth, ErrorTrackingRead);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, state.project_access_checker);

    let fingerprints = state
        .error_tracking_service
        .workflow
        .list_group_fingerprints(project_id, group_id)
        .await?;
    let merged_group_ids = state
        .error_tracking_service
        .workflow
        .merged_group_ids(project_id, group_id)
        .await?;

    Ok(Json(ErrorGroupFingerprintsResponse {
        group_id,
        fingerprints: fingerprints
            .into_iter()
            .map(|fingerprint| ErrorGroupFingerprintResponse {
                fingerprint_hash: fingerprint.fingerprint_hash,
                merged_from_group_id: fingerprint.merged_from_group_id,
            })
            .collect(),
        merged_group_ids,
    }))
}

/// Audit context from the caller's session and request metadata.
fn audit_context(auth: &temps_authz::AuthContext, metadata: &RequestMetadata) -> AuditContext {
    AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::services::error_alert_service::ErrorAlertService;
    use crate::services::error_tracking_service::ErrorTrackingService;
    use async_trait::async_trait;
    use axum::http::HeaderMap;
    use sea_orm::{ActiveModelTrait, EntityTrait, Set};
    use std::sync::Mutex;
    use temps_authz::{AuthContext, Role};
    use temps_entities::{error_groups, projects, users};
    use temps_test_utils::{is_container_runtime_unavailable, TestDatabase};

    #[derive(Default)]
    pub(crate) struct RecordingAuditLogger {
        pub(crate) entries: Mutex<Vec<(String, serde_json::Value)>>,
    }

    #[async_trait]
    impl temps_core::AuditLogger for RecordingAuditLogger {
        async fn create_audit_log(
            &self,
            operation: &dyn temps_core::AuditOperation,
        ) -> temps_core::AuditResult<()> {
            let payload: serde_json::Value = serde_json::from_str(&operation.serialize()?)
                .expect("audit operation serializes to valid JSON");
            self.entries
                .lock()
                .map_err(|_| {
                    temps_core::AuditError::write(
                        operation.operation_type(),
                        "recording audit logger lock poisoned",
                    )
                })?
                .push((operation.operation_type(), payload));
            Ok(())
        }
    }

    pub(crate) fn test_user(id: i32, role: Role) -> AuthContext {
        let now = chrono::Utc::now();
        AuthContext::new_session(
            users::Model {
                id,
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
            },
            role,
        )
    }

    pub(crate) fn metadata() -> RequestMetadata {
        RequestMetadata {
            ip_address: "203.0.113.9".to_string(),
            user_agent: "bulk-test".to_string(),
            headers: HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "https://temps.test".to_string(),
            scheme: "https".to_string(),
            host: "temps.test".to_string(),
            is_secure: true,
        }
    }

    pub(crate) async fn seed(db: &sea_orm::DatabaseConnection) -> (i32, i32, i32) {
        let now = chrono::Utc::now();
        let project = projects::ActiveModel {
            name: Set("Bulk Project".to_string()),
            repo_name: Set("repo".to_string()),
            repo_owner: Set("owner".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            slug: Set(format!("bulk-project-{}", uuid::Uuid::new_v4())),
            preset: Set(temps_entities::preset::Preset::NextJs),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert project");
        let mut ids = Vec::new();
        for title in ["TypeError: a", "TypeError: b"] {
            ids.push(
                error_groups::ActiveModel {
                    title: Set(title.to_string()),
                    error_type: Set("TypeError".to_string()),
                    message_template: Set(None),
                    embedding: Set(None),
                    first_seen: Set(now),
                    last_seen: Set(now),
                    total_count: Set(1),
                    status: Set("unresolved".to_string()),
                    assigned_to: Set(None),
                    project_id: Set(project.id),
                    environment_id: Set(None),
                    deployment_id: Set(None),
                    visitor_id: Set(None),
                    created_at: Set(now),
                    updated_at: Set(now),
                    ..Default::default()
                }
                .insert(db)
                .await
                .expect("insert error group")
                .id,
            );
        }
        (project.id, ids[0], ids[1])
    }

    /// A user row, so activity entries can name their actor.
    pub(crate) async fn create_user(db: &sea_orm::DatabaseConnection) -> i32 {
        use sea_orm::ConnectionTrait;
        db.query_one(sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "INSERT INTO users (name, email, email_verified, must_change_password, mfa_enabled, \
             created_at, updated_at) VALUES ('Ana', $1, TRUE, FALSE, FALSE, NOW(), NOW()) \
             RETURNING id",
            [format!("ana-{}@example.test", uuid::Uuid::new_v4()).into()],
        ))
        .await
        .expect("insert user")
        .expect("user row")
        .try_get("", "id")
        .expect("user id")
    }

    pub(crate) fn app_state(
        db: &Arc<sea_orm::DatabaseConnection>,
        audit: Arc<RecordingAuditLogger>,
    ) -> Arc<AppState> {
        Arc::new(AppState {
            error_tracking_service: Arc::new(ErrorTrackingService::new(db.clone())),
            alert_service: Arc::new(ErrorAlertService::new(db.clone())),
            audit_service: audit,
            project_access_checker: None,
        })
    }

    #[tokio::test]
    async fn bulk_update_resolves_the_named_groups_and_audits_the_request() {
        let test_db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
                eprintln!("Skipping bulk error-group handler test: {error}");
                return;
            }
            Err(error) => panic!("bulk handler test database setup failed: {error}"),
        };
        let db = test_db.connection_arc();
        let (project_id, first, second) = seed(db.as_ref()).await;
        let recorder = Arc::new(RecordingAuditLogger::default());
        let state = app_state(&db, recorder.clone());
        let actor = create_user(db.as_ref()).await;

        let response = bulk_update_error_groups(
            State(state.clone()),
            RequireAuth(test_user(actor, Role::Admin)),
            Path(project_id),
            Extension(metadata()),
            Json(BulkErrorGroupRequest {
                ignore: None,
                in_next_release: false,
                action: BulkErrorGroupAction::Resolve,
                group_ids: Some(vec![first, second]),
                filter: None,
            }),
        )
        .await
        .expect("bulk update succeeds")
        .0;

        assert_eq!(response.updated, 2);
        assert_eq!(response.skipped, 0);
        for id in [first, second] {
            let group = error_groups::Entity::find_by_id(id)
                .one(db.as_ref())
                .await
                .expect("load group")
                .expect("group exists");
            assert_eq!(group.status, "resolved");
        }

        for id in [first, second] {
            let activity = state
                .error_tracking_service
                .workflow
                .list_activity(project_id, id, 1, 20)
                .await
                .expect("activity");
            assert_eq!(activity.total, 1, "group {id} logs the bulk change");
            let entry = &activity.entries[0];
            assert_eq!(entry.kind, ActivityKind::StatusChanged);
            assert_eq!(entry.details.status.as_deref(), Some("resolved"));
            assert!(entry.details.bulk);
        }

        let entries = recorder.entries.lock().expect("lock");
        assert_eq!(entries.len(), 1);
        let (operation, payload) = &entries[0];
        assert_eq!(operation, "ERROR_GROUPS_BULK_UPDATED");
        assert_eq!(payload["project_id"], project_id);
        assert_eq!(payload["action"], "resolve");
        assert_eq!(payload["status"], "resolved");
        assert_eq!(
            payload["updated_group_ids"].as_array().map(Vec::len),
            Some(2)
        );
    }

    #[tokio::test]
    async fn a_reader_cannot_bulk_update_error_groups() {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        let state = app_state(&db, Arc::new(RecordingAuditLogger::default()));

        let problem = bulk_update_error_groups(
            State(state),
            RequireAuth(test_user(9, Role::Reader)),
            Path(1),
            Extension(metadata()),
            Json(BulkErrorGroupRequest {
                ignore: None,
                in_next_release: false,
                action: BulkErrorGroupAction::Resolve,
                group_ids: Some(vec![1]),
                filter: None,
            }),
        )
        .await
        .expect_err("a viewer must be refused");
        assert_eq!(
            axum::response::IntoResponse::into_response(problem).status(),
            axum::http::StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn a_request_must_select_groups_exactly_one_way() {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        let state = app_state(&db, Arc::new(RecordingAuditLogger::default()));

        for (ids, filter) in [
            (None, None),
            (Some(vec![1]), Some(BulkErrorGroupFilter::default())),
        ] {
            let problem = bulk_update_error_groups(
                State(state.clone()),
                RequireAuth(test_user(3, Role::Admin)),
                Path(1),
                Extension(metadata()),
                Json(BulkErrorGroupRequest {
                    ignore: None,
                    in_next_release: false,
                    action: BulkErrorGroupAction::Ignore,
                    group_ids: ids,
                    filter,
                }),
            )
            .await
            .expect_err("an ambiguous selection must be refused");
            assert_eq!(
                axum::response::IntoResponse::into_response(problem).status(),
                axum::http::StatusCode::BAD_REQUEST
            );
        }
    }
}
