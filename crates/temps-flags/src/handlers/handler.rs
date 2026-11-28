// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP handlers for feature flags.

use std::sync::Arc;

use axum::{
    extract::{DefaultBodyLimit, Extension, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
    routing::{delete, get, patch, post, put},
    Json, Router,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use temps_authz::{
    deny_deployment_token, permission_guard, project_access_guard, project_permission_guard,
    project_scope_guard, RequireAuth,
};
use temps_core::problemdetails::{self, Problem};
use temps_core::RequestMetadata;
use tracing::error;
use utoipa::{IntoParams, OpenApi};

use super::audit::{
    AuditContext, FeatureFlagArchivedAudit, FeatureFlagCreatedAudit,
    FeatureFlagEnvironmentValueSetAudit, FeatureFlagReshuffledAudit, FeatureFlagRestoredAudit,
    FeatureFlagRulesSetAudit, FeatureFlagUpdatedAudit,
};
use super::public::{
    ofrep_server_bulk, ofrep_server_single, OfrepBulkResponse, OfrepEvaluationFailure,
    OfrepEvaluationRequest, OfrepEvaluationSuccess, OfrepMetadata, PublicFlagEvaluation,
    PublicFlagsRequest, PublicFlagsResponse, EVALUATION_BODY_LIMIT,
};
use super::types::*;
use crate::error::FlagError;
use crate::eval::{
    AttributeValue, ConditionOperator, ConditionValue, EvalContext, EvalReason, Evaluation,
    FlagRule, FlagSnapshot, FlagValueType, FlagVariant, RuleCondition, RuleServe, VariantWeight,
};
use crate::services::{
    normalize_pagination, CreateFlag, SetEnvironmentValue, SetRules, UpdateFlag, UpdatedFlag,
};

#[derive(OpenApi)]
#[openapi(
    paths(
        list_flags,
        create_flag,
        get_flag,
        update_flag,
        archive_flag,
        restore_flag,
        set_flag_environment,
        set_flag_rules,
        reshuffle_flag,
        evaluate_flag,
        get_flag_snapshot,
        record_flag_exposure,
        super::public::evaluate_public_flags,
        super::public::ofrep_public_bulk,
        super::public::ofrep_public_single,
        super::public::ofrep_server_bulk,
        super::public::ofrep_server_single,
    ),
    components(schemas(
        CreateFlagRequest,
        UpdateFlagRequest,
        SetFlagEnvironmentRequest,
        FlagResponse,
        FlagEnvironmentResponse,
        FlagListResponse,
        FlagSnapshotResponse,
        ArchiveFlagResponse,
        RecordExposureRequest,
        RecordExposureResponse,
        FlagSnapshot,
        FlagValueType,
        SetFlagRulesRequest,
        EvaluateFlagRequest,
        Evaluation,
        EvalReason,
        FlagVariant,
        FlagRule,
        RuleCondition,
        ConditionOperator,
        ConditionValue,
        AttributeValue,
        RuleServe,
        VariantWeight,
        PublicFlagsRequest,
        PublicFlagsResponse,
        PublicFlagEvaluation,
        OfrepEvaluationRequest,
        OfrepEvaluationSuccess,
        OfrepEvaluationFailure,
        OfrepBulkResponse,
        OfrepMetadata,
    )),
    tags((
        name = "Feature Flags",
        description = "Runtime configuration that changes without a redeploy"
    ))
)]
pub struct FlagsApiDoc;

pub fn configure_routes() -> Router<Arc<FlagsAppState>> {
    Router::new()
        .route("/projects/{project_id}/flags", get(list_flags))
        .route("/projects/{project_id}/flags", post(create_flag))
        .route("/projects/{project_id}/flags/{key}", get(get_flag))
        .route("/projects/{project_id}/flags/{key}", patch(update_flag))
        .route("/projects/{project_id}/flags/{key}", delete(archive_flag))
        .route(
            "/projects/{project_id}/flags/{key}/restore",
            post(restore_flag),
        )
        .route(
            "/projects/{project_id}/flags/{key}/environments/{environment_id}",
            put(set_flag_environment),
        )
        .route(
            "/projects/{project_id}/flags/{key}/environments/{environment_id}/rules",
            put(set_flag_rules),
        )
        .route(
            "/projects/{project_id}/flags/{key}/reshuffle",
            post(reshuffle_flag),
        )
        .route(
            "/projects/{project_id}/flags/{key}/evaluate",
            post(evaluate_flag),
        )
        // OpenFeature Remote Evaluation Protocol for server-side SDKs; scope
        // comes from the deployment token.
        // Bounded like the public OFREP routes; axum's default would accept
        // 2 MB before `parse_ofrep` ever saw it.
        .route(
            "/ofrep/v1/evaluate/flags",
            post(ofrep_server_bulk).layer(DefaultBodyLimit::max(EVALUATION_BODY_LIMIT)),
        )
        .route(
            "/ofrep/v1/evaluate/flags/{key}",
            post(ofrep_server_single).layer(DefaultBodyLimit::max(EVALUATION_BODY_LIMIT)),
        )
        // Delivery endpoint for running services. Scope comes from the
        // deployment token, not the URL.
        .route("/flags/snapshot", get(get_flag_snapshot))
        .route("/flags/exposure", post(record_flag_exposure))
}

// =============================================================================
// Error mapping
// =============================================================================

impl From<FlagError> for Problem {
    fn from(error: FlagError) -> Self {
        match error {
            FlagError::NotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Feature Flag Not Found")
                .with_detail(error.to_string()),

            FlagError::ProjectNotFound { project_id } => {
                temps_core::api_problems::project_not_found(project_id)
            }

            FlagError::EnvironmentNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Environment Not Found")
                .with_detail(error.to_string()),

            FlagError::DuplicateKey { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Feature Flag Already Exists")
                .with_detail(error.to_string()),

            FlagError::StaleWrite { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Feature Flag Changed")
                .with_detail(error.to_string()),

            // Cross-tenant attachment is a client error, not a server one, but
            // it is worth distinguishing from a plain validation failure.
            FlagError::EnvironmentNotInProject { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Environment Not In Project")
                    .with_detail(error.to_string())
            }

            FlagError::InvalidKey { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Flag Key")
                .with_detail(error.to_string()),

            FlagError::ValueTypeMismatch { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Value Type Mismatch")
                .with_detail(error.to_string()),

            FlagError::Validation { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Validation Error")
                .with_detail(error.to_string()),

            FlagError::TokenNotEnvironmentScoped { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Environment Required")
                    .with_detail(error.to_string())
            }

            // A stored value_type that no longer parses is data corruption, not
            // bad input.
            FlagError::InvalidValueType { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Invalid Stored Value Type")
                    .with_detail(error.to_string())
            }

            FlagError::Database(_) => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Internal Server Error")
                .with_detail(error.to_string()),

            FlagError::Serialization { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

// =============================================================================
// Query parameters
// =============================================================================

#[derive(Debug, Deserialize, IntoParams)]
pub struct ListFlagsQuery {
    /// Include archived flags. Defaults to false.
    #[serde(default)]
    pub include_archived: bool,
    /// 1-indexed page number. Defaults to 1.
    pub page: Option<u64>,
    /// Items per page. Defaults to 20, capped at 100.
    pub page_size: Option<u64>,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct SnapshotQuery {
    /// Required only when the calling token is project-wide rather than scoped
    /// to a single environment.
    pub environment_id: Option<i32>,
}

// =============================================================================
// Helpers
// =============================================================================

/// Parse an `expected_updated_at` revision sent back by a client.
fn parse_revision(raw: Option<String>) -> Result<Option<chrono::DateTime<chrono::Utc>>, Problem> {
    raw.map(|text| {
        chrono::DateTime::parse_from_rfc3339(&text)
            .map(|parsed| parsed.with_timezone(&chrono::Utc))
            .map_err(|e| {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid Revision")
                    .with_detail(format!(
                        "expected_updated_at '{text}' is not an RFC 3339 timestamp: {e}"
                    ))
            })
    })
    .transpose()
}

fn audit_context(auth: &temps_authz::AuthContext, metadata: &RequestMetadata) -> AuditContext {
    AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    }
}

/// Weak-ish entity tag over the serialized snapshot.
///
/// The snapshot is sorted by key in the service, so equal flag state always
/// produces an equal tag and the SDK's poll collapses to a 304.
fn snapshot_etag(flags: &[FlagSnapshot]) -> Result<String, FlagError> {
    let body = serde_json::to_vec(flags).map_err(|e| FlagError::Serialization {
        key: "<snapshot>".to_string(),
        reason: e.to_string(),
    })?;

    let mut hasher = Sha256::new();
    hasher.update(&body);
    let digest = hasher.finalize();

    Ok(format!("\"{}\"", hex::encode(&digest[..16])))
}

/// Does an `If-None-Match` header match our tag?
///
/// Handles the two things exact-byte comparison gets wrong: a weak validator
/// (`W/"abc"`), which any conformant intermediary is allowed to introduce, and
/// a comma-separated list of candidates. Both would otherwise silently defeat
/// the 304 and make every poll transfer the whole snapshot.
fn if_none_match_matches(requested: &HeaderValue, etag: &str) -> bool {
    let Ok(raw) = requested.to_str() else {
        return false;
    };

    if raw.trim() == "*" {
        return true;
    }

    raw.split(',')
        .map(|candidate| candidate.trim())
        .map(|candidate| candidate.strip_prefix("W/").unwrap_or(candidate))
        .any(|candidate| candidate == etag)
}

// =============================================================================
// Management handlers
// =============================================================================

#[utoipa::path(
    tag = "Feature Flags",
    get,
    path = "/projects/{project_id}/flags",
    params(("project_id" = i32, Path, description = "Project ID"), ListFlagsQuery),
    responses(
        (status = 200, description = "Flags listed", body = FlagListResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_flags(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<FlagsAppState>>,
    Path(project_id): Path<i32>,
    Query(query): Query<ListFlagsQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, FlagsRead);
    // A deployment token is pinned to one environment, but this endpoint
    // returns every environment's override. `/flags/snapshot` is the machine
    // surface and honours that pinning; without this guard a preview container
    // could read production flag values for its project.
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, &state.project_access_checker);

    let (entries, total) = state
        .flag_service
        .list(
            project_id,
            query.include_archived,
            query.page,
            query.page_size,
        )
        .await?;

    let flags: Vec<FlagResponse> = entries
        .into_iter()
        .map(|entry| FlagResponse::new(entry.flag, entry.environments))
        .collect();

    // Same clamp the query used, so `total_pages` can never describe a page
    // size the server did not actually serve.
    let (page, page_size) = normalize_pagination(query.page, query.page_size);
    let total_pages = total.div_ceil(page_size);

    Ok(Json(FlagListResponse {
        flags,
        total,
        page,
        page_size,
        total_pages,
    }))
}

#[utoipa::path(
    tag = "Feature Flags",
    post,
    path = "/projects/{project_id}/flags",
    params(("project_id" = i32, Path, description = "Project ID")),
    request_body = CreateFlagRequest,
    responses(
        (status = 201, description = "Flag created", body = FlagResponse),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 409, description = "Flag key already exists"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_flag(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<FlagsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(project_id): Path<i32>,
    Json(request): Json<CreateFlagRequest>,
) -> Result<impl IntoResponse, Problem> {
    project_scope_guard!(auth, project_id);
    project_permission_guard!(auth, FlagsWrite, project_id, &state.project_access_checker);

    let flag = state
        .flag_service
        .create(
            project_id,
            CreateFlag {
                key: request.key,
                value_type: request.value_type,
                default_value: request.default_value,
                description: request.description,
                client_visible: request.client_visible,
                variants: request.variants,
                exposure_sample_rate: request.exposure_sample_rate,
            },
        )
        .await?;

    let audit = FeatureFlagCreatedAudit {
        context: audit_context(&auth, &metadata),
        project_id,
        flag_id: flag.id,
        key: flag.key.clone(),
        value_type: flag.value_type.clone(),
        default_value: flag.default_value.clone(),
        client_visible: flag.client_visible,
        variants: flag.variants.clone(),
        exposure_sample_rate: flag.exposure_sample_rate,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(
            "Failed to create audit log for feature flag creation: {}",
            e
        );
    }

    Ok((
        StatusCode::CREATED,
        Json(FlagResponse::new(flag, Vec::new())),
    ))
}

#[utoipa::path(
    tag = "Feature Flags",
    get,
    path = "/projects/{project_id}/flags/{key}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("key" = String, Path, description = "Flag key")
    ),
    responses(
        (status = 200, description = "Flag retrieved", body = FlagResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Flag not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_flag(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<FlagsAppState>>,
    Path((project_id, key)): Path<(i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, FlagsRead);
    // Same reasoning as `list_flags`: the response carries every environment's
    // override, which is more than a token's own environment.
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, &state.project_access_checker);

    let entry = state.flag_service.get(project_id, &key).await?;

    Ok(Json(FlagResponse::new(entry.flag, entry.environments)))
}

#[utoipa::path(
    tag = "Feature Flags",
    patch,
    path = "/projects/{project_id}/flags/{key}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("key" = String, Path, description = "Flag key")
    ),
    request_body = UpdateFlagRequest,
    responses(
        (status = 200, description = "Flag updated", body = FlagResponse),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Flag not found"),
        (status = 409, description = "Flag changed since it was read"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_flag(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<FlagsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, key)): Path<(i32, String)>,
    Json(request): Json<UpdateFlagRequest>,
) -> Result<impl IntoResponse, Problem> {
    project_scope_guard!(auth, project_id);
    project_permission_guard!(auth, FlagsWrite, project_id, &state.project_access_checker);

    // The service returns the row it locked as the before-image, so the audit
    // records the transition this write made even when another writer
    // committed after this request was sent.
    let expected_updated_at = parse_revision(request.expected_updated_at)?;
    let UpdatedFlag {
        before: before_flag,
        flag,
        environments,
    } = state
        .flag_service
        .update(
            project_id,
            &key,
            UpdateFlag {
                default_value: request.default_value,
                description: request.description,
                client_visible: request.client_visible,
                variants: request.variants,
                exposure_sample_rate: request.exposure_sample_rate,
                expected_updated_at,
            },
        )
        .await?;

    let audit = FeatureFlagUpdatedAudit {
        context: audit_context(&auth, &metadata),
        project_id,
        flag_id: flag.id,
        key: flag.key.clone(),
        old_default_value: before_flag.default_value,
        new_default_value: flag.default_value.clone(),
        old_client_visible: before_flag.client_visible,
        new_client_visible: flag.client_visible,
        old_variants: before_flag.variants,
        new_variants: flag.variants.clone(),
        old_exposure_sample_rate: before_flag.exposure_sample_rate,
        new_exposure_sample_rate: flag.exposure_sample_rate,
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log for feature flag update: {}", e);
    }

    Ok(Json(FlagResponse::new(flag, environments)))
}

#[utoipa::path(
    tag = "Feature Flags",
    delete,
    path = "/projects/{project_id}/flags/{key}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("key" = String, Path, description = "Flag key")
    ),
    responses(
        (status = 200, description = "Flag archived", body = ArchiveFlagResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Flag not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn archive_flag(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<FlagsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, key)): Path<(i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    project_scope_guard!(auth, project_id);
    project_permission_guard!(auth, FlagsDelete, project_id, &state.project_access_checker);

    let flag = state.flag_service.archive(project_id, &key).await?;

    let audit = FeatureFlagArchivedAudit {
        context: audit_context(&auth, &metadata),
        project_id,
        flag_id: flag.id,
        key: flag.key.clone(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log for feature flag archive: {}", e);
    }

    Ok(Json(ArchiveFlagResponse {
        key: flag.key,
        archived_at: flag.archived_at.map(super::types::iso),
    }))
}

/// Bring an archived flag back.
///
/// Archiving is otherwise one-way: the key stays reserved so the flag cannot
/// even be re-created under the same name, which makes an accidental archive
/// unrecoverable through the API.
#[utoipa::path(
    tag = "Feature Flags",
    post,
    path = "/projects/{project_id}/flags/{key}/restore",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("key" = String, Path, description = "Flag key")
    ),
    responses(
        (status = 200, description = "Flag restored", body = FlagResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Flag not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn restore_flag(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<FlagsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, key)): Path<(i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    project_scope_guard!(auth, project_id);
    project_permission_guard!(auth, FlagsWrite, project_id, &state.project_access_checker);

    let flag = state.flag_service.restore(project_id, &key).await?;

    let audit = FeatureFlagRestoredAudit {
        context: audit_context(&auth, &metadata),
        project_id,
        flag_id: flag.id,
        key: flag.key.clone(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log for feature flag restore: {}", e);
    }

    Ok(Json(FlagResponse::new(flag, Vec::new())))
}

/// Set a flag's value in one environment, and/or flip its kill switch.
#[utoipa::path(
    tag = "Feature Flags",
    put,
    path = "/projects/{project_id}/flags/{key}/environments/{environment_id}",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("key" = String, Path, description = "Flag key"),
        ("environment_id" = i32, Path, description = "Environment ID")
    ),
    request_body = SetFlagEnvironmentRequest,
    responses(
        (status = 200, description = "Environment value set", body = FlagEnvironmentResponse),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Flag or environment not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn set_flag_environment(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<FlagsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, key, environment_id)): Path<(i32, String, i32)>,
    Json(request): Json<SetFlagEnvironmentRequest>,
) -> Result<impl IntoResponse, Problem> {
    project_scope_guard!(auth, project_id);
    project_permission_guard!(auth, FlagsWrite, project_id, &state.project_access_checker);

    // Captured before the write so the audit entry records what the flag was
    // actually changed *from* — the only record that a production behaviour
    // change happened, since a flag flip leaves no deployment behind.
    let before = state
        .flag_service
        .get(project_id, &key)
        .await?
        .environments
        .into_iter()
        .find(|row| row.environment_id == environment_id);

    let model = state
        .flag_service
        .set_environment_value(
            project_id,
            &key,
            environment_id,
            SetEnvironmentValue {
                value: request.value,
                enabled: request.enabled,
            },
        )
        .await?;

    let audit = FeatureFlagEnvironmentValueSetAudit {
        context: audit_context(&auth, &metadata),
        project_id,
        flag_id: model.flag_id,
        key: key.clone(),
        environment_id,
        old_enabled: before.as_ref().map(|row| row.enabled),
        new_enabled: model.enabled,
        old_value: before.and_then(|row| row.value),
        new_value: model.value.clone(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(
            "Failed to create audit log for feature flag environment value: {}",
            e
        );
    }

    Ok(Json(FlagEnvironmentResponse::from(model)))
}

/// Replace the ordered targeting rules of a flag in one environment.
///
/// Rules apply after the kill switch and before the environment value; the
/// first rule whose conditions hold (and whose rollout includes the subject)
/// decides the value. Send the environment row's `updated_at` as
/// `expected_updated_at` so a concurrent edit is refused with 409 instead of
/// being overwritten.
#[utoipa::path(
    tag = "Feature Flags",
    put,
    path = "/projects/{project_id}/flags/{key}/environments/{environment_id}/rules",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("key" = String, Path, description = "Flag key"),
        ("environment_id" = i32, Path, description = "Environment ID")
    ),
    request_body = SetFlagRulesRequest,
    responses(
        (status = 200, description = "Rules replaced", body = FlagEnvironmentResponse),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Flag or environment not found"),
        (status = 409, description = "Rules changed since they were read"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn set_flag_rules(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<FlagsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, key, environment_id)): Path<(i32, String, i32)>,
    Json(request): Json<SetFlagRulesRequest>,
) -> Result<impl IntoResponse, Problem> {
    project_scope_guard!(auth, project_id);
    project_permission_guard!(auth, FlagsWrite, project_id, &state.project_access_checker);

    let expected_updated_at = parse_revision(request.expected_updated_at)?;
    let (before, after) = state
        .flag_service
        .set_rules(
            project_id,
            &key,
            environment_id,
            SetRules {
                rules: request.rules,
                expected_updated_at,
            },
        )
        .await?;

    let audit = FeatureFlagRulesSetAudit {
        context: audit_context(&auth, &metadata),
        project_id,
        flag_id: after.flag_id,
        key: key.clone(),
        environment_id,
        old_rules: before.map(|row| row.rules),
        new_rules: after.rules.clone(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(
            "Failed to create audit log for feature flag '{}' rules in environment {}: {}",
            key, environment_id, e
        );
    }

    Ok(Json(FlagEnvironmentResponse::from(after)))
}

/// Regenerate the flag's bucketing salt.
///
/// Reassigns every subject of every percentage rollout and split of this flag,
/// in every environment. Use it to draw a fresh sample, never on a running
/// experiment.
#[utoipa::path(
    tag = "Feature Flags",
    post,
    path = "/projects/{project_id}/flags/{key}/reshuffle",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("key" = String, Path, description = "Flag key")
    ),
    responses(
        (status = 200, description = "Salt regenerated", body = FlagResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Flag not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn reshuffle_flag(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<FlagsAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((project_id, key)): Path<(i32, String)>,
) -> Result<impl IntoResponse, Problem> {
    project_scope_guard!(auth, project_id);
    project_permission_guard!(auth, FlagsWrite, project_id, &state.project_access_checker);

    // Read the environments first: a reshuffle does not change them, and a
    // read failing after the new salt is committed would report a reshuffle
    // that did happen as failed, inviting a retry that reshuffles again.
    let environments = state.flag_service.get(project_id, &key).await?.environments;
    let flag = state.flag_service.reshuffle(project_id, &key).await?;

    let audit = FeatureFlagReshuffledAudit {
        context: audit_context(&auth, &metadata),
        project_id,
        flag_id: flag.id,
        key: flag.key.clone(),
    };
    if let Err(e) = state.audit_service.create_audit_log(&audit).await {
        error!(
            "Failed to create audit log for feature flag reshuffle: {}",
            e
        );
    }

    Ok(Json(FlagResponse::new(flag, environments)))
}

/// Evaluate one flag as an app in the given environment would, for a context
/// you supply. Read-only; the console's "test this flag" panel.
#[utoipa::path(
    tag = "Feature Flags",
    post,
    path = "/projects/{project_id}/flags/{key}/evaluate",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("key" = String, Path, description = "Flag key")
    ),
    request_body = EvaluateFlagRequest,
    responses(
        (status = 200, description = "Evaluation", body = Evaluation),
        (status = 400, description = "Environment not in project, or the context exceeds the evaluation limits"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Flag or environment not found"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn evaluate_flag(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<FlagsAppState>>,
    Path((project_id, key)): Path<(i32, String)>,
    Json(request): Json<EvaluateFlagRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, FlagsRead);
    // A deployment token is pinned to one environment; this endpoint names
    // any environment of the project.
    deny_deployment_token!(auth);
    project_scope_guard!(auth, project_id);
    project_access_guard!(auth, project_id, &state.project_access_checker);

    let context = EvalContext {
        key: request.key,
        attributes: request.attributes,
    };
    // The same bounds as the app-facing endpoints: an attribute longer than
    // the SDKs accept would otherwise test differently here than in an app.
    if let Err(reason) = super::public::check_context(&context) {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Flag Context")
            .with_detail(format!(
                "Cannot evaluate flag '{key}' in project {project_id}: {reason}"
            )));
    }
    let evaluation = state
        .flag_service
        .evaluate_for_console(project_id, &key, request.environment_id, &context)
        .await?;

    Ok(Json(evaluation))
}

/// Record which flags a running app actually evaluated.
///
/// This is what makes `last_evaluated_at` mean something. The snapshot
/// endpoint hands the SDK every flag in the environment and evaluation then
/// happens locally, so the control plane cannot otherwise tell a flag that is
/// referenced by live code from one nothing has called in a year. Stamping on
/// snapshot fetch would mark every flag as freshly used and defeat the point.
///
/// Scope comes from the deployment token, never the body. The endpoint writes
/// only `last_evaluated_at` — never a flag's value — so "a deployment token
/// cannot change what a flag serves" still holds despite this being a write.
#[utoipa::path(
    tag = "Feature Flags",
    post,
    path = "/flags/exposure",
    request_body = RecordExposureRequest,
    responses(
        (status = 200, description = "Exposure recorded", body = RecordExposureResponse),
        (status = 400, description = "Deployment token required"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn record_flag_exposure(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<FlagsAppState>>,
    Json(request): Json<RecordExposureRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, FlagsRead);

    let project_id = auth.project_id().ok_or_else(|| {
        problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Deployment Token Required")
            .with_detail(
                "Flag exposure is reported by deployed applications using TEMPS_API_TOKEN.",
            )
    })?;

    let recorded = state
        .flag_service
        .record_exposure(project_id, &request.keys)
        .await?;

    Ok(Json(RecordExposureResponse { recorded }))
}

// =============================================================================
// Delivery handler
// =============================================================================

/// Every flag for the caller's environment, collapsed to what the evaluator
/// needs.
///
/// Scope comes from the deployment token, never from the URL: a container's
/// baked-in `TEMPS_API_TOKEN` identifies exactly one project (and usually one
/// environment), so a compromised app cannot read another tenant's flags by
/// changing a path parameter.
///
/// Supports `If-None-Match`, so the SDK's background poll is a 304 in the
/// common case.
#[utoipa::path(
    tag = "Feature Flags",
    get,
    path = "/flags/snapshot",
    params(SnapshotQuery),
    responses(
        (status = 200, description = "Snapshot for the environment", body = FlagSnapshotResponse),
        (status = 304, description = "Snapshot unchanged"),
        (status = 400, description = "Environment could not be determined"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_flag_snapshot(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<FlagsAppState>>,
    headers: HeaderMap,
    Query(query): Query<SnapshotQuery>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, FlagsRead);

    let token_info = auth.deployment_token_info();

    // A deployment token pins the project. Anything else (session, API key)
    // has no project to pin, so it is turned away — the management endpoints
    // are the route for those principals.
    let project_id = match auth.project_id() {
        Some(project_id) => project_id,
        None => {
            return Err(problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Deployment Token Required")
                .with_detail(
                    "The flag snapshot endpoint is scoped by a deployment token. \
                     Use TEMPS_API_TOKEN, or read flags via /projects/{project_id}/flags.",
                ))
        }
    };

    // Prefer the token's own environment. A project-wide token has to say
    // which environment it means, and the service verifies that environment
    // actually belongs to this project before returning anything.
    let environment_id = token_info
        .and_then(|info| info.environment_id)
        .or(query.environment_id)
        .ok_or_else(|| Problem::from(FlagError::TokenNotEnvironmentScoped { project_id }))?;

    let flags = state
        .flag_service
        // Server-side callers see every flag, including server-only ones, with
        // rules and salt so the SDK can evaluate locally. The unauthenticated
        // same-origin endpoint serves evaluated client_visible flags only.
        .snapshot(project_id, environment_id, false)
        .await
        // Collapse "no such environment" and "someone else's environment" into
        // one response. Distinguishing them lets a token holder probe
        // sequential ids to learn which environments exist platform-wide, and a
        // calling app has no reason to tell the two apart.
        .map_err(|error| match error {
            FlagError::EnvironmentNotFound { .. } | FlagError::EnvironmentNotInProject { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Environment Not Available")
                    .with_detail(
                        "The requested environment does not exist in this token's project.",
                    )
            }
            other => Problem::from(other),
        })?;

    let etag = snapshot_etag(&flags)?;

    if let Some(requested) = headers.get(header::IF_NONE_MATCH) {
        if if_none_match_matches(requested, &etag) {
            return Ok(StatusCode::NOT_MODIFIED.into_response());
        }
    }

    let mut response_headers = HeaderMap::new();
    match HeaderValue::from_str(&etag) {
        Ok(value) => {
            response_headers.insert(header::ETAG, value);
        }
        Err(e) => {
            // A non-ASCII ETag is impossible (hex), but degrade rather than
            // fail the caller's flag fetch.
            error!("Failed to build ETag header for flag snapshot: {}", e);
        }
    }

    Ok((
        StatusCode::OK,
        response_headers,
        Json(FlagSnapshotResponse {
            environment_id,
            flags,
        }),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::FlagService;
    use axum::body::Bytes;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_authz::context::AuthContext;
    use temps_authz::permissions::{Permission, Role};
    use temps_entities::deployment_tokens::DeploymentTokenPermission;
    use temps_entities::users;

    // =====================================================================
    // Handler guards
    // =====================================================================

    /// Minimal `AuditLogger` so handler tests don't need the audit crate's
    /// database and geo-IP dependencies.
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

    fn app_state() -> Arc<FlagsAppState> {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        Arc::new(FlagsAppState {
            flag_service: Arc::new(FlagService::new(db)),
            audit_service: Arc::new(NoopAuditLogger),
            project_access_checker: None,
        })
    }

    fn list_query() -> ListFlagsQuery {
        ListFlagsQuery {
            include_archived: false,
            page: None,
            page_size: None,
        }
    }

    /// The Problem status if the handler rejected the request, or `None` if it
    /// passed the guards and went on to the service.
    async fn list_rejection(auth: AuthContext) -> Option<StatusCode> {
        list_flags(
            RequireAuth(auth),
            State(app_state()),
            Path(1),
            Query(list_query()),
        )
        .await
        .err()
        .map(|p| p.status_code)
    }

    async fn archive_rejection(auth: AuthContext) -> Option<StatusCode> {
        archive_flag(
            RequireAuth(auth),
            State(app_state()),
            Extension(test_metadata()),
            Path((1, "checkout.v2".to_string())),
        )
        .await
        .err()
        .map(|p| p.status_code)
    }

    #[tokio::test]
    async fn reader_cannot_archive_a_flag() {
        assert_eq!(
            archive_rejection(AuthContext::new_session(test_user(), Role::Reader)).await,
            Some(StatusCode::FORBIDDEN),
            "flags:delete must not be implied by a read-only role"
        );
    }

    #[tokio::test]
    async fn reader_can_reach_the_list_handler() {
        assert_ne!(
            list_rejection(AuthContext::new_session(test_user(), Role::Reader)).await,
            Some(StatusCode::FORBIDDEN),
            "Reader holds flags:read and must pass the guard"
        );
    }

    #[tokio::test]
    async fn api_key_scoped_to_read_cannot_write() {
        let read_only = || {
            AuthContext::new_api_key(
                test_user(),
                None,
                Some(vec![Permission::FlagsRead]),
                "flags-read-key".to_string(),
                1,
            )
        };

        assert_ne!(
            list_rejection(read_only()).await,
            Some(StatusCode::FORBIDDEN),
            "a flags:read key must be able to list"
        );
        assert_eq!(
            archive_rejection(read_only()).await,
            Some(StatusCode::FORBIDDEN),
            "a flags:read key must NOT be able to archive"
        );
    }

    /// Regression: a deployment token scoped to project 1 must not be able to
    /// read project 2's flags by putting a different id in the path.
    ///
    /// `project_access_guard!` deliberately skips deployment tokens (they carry
    /// no user identity for a team-membership check), so `project_scope_guard!`
    /// is the *only* thing confining them. Without it this returned 200 and the
    /// other tenant's flag values.
    #[tokio::test]
    async fn deployment_token_cannot_read_another_projects_flags() {
        let foreign_project = AuthContext::new_deployment_token(
            1,
            Some(1),
            None,
            1,
            "app".to_string(),
            vec![DeploymentTokenPermission::FlagsRead],
        );

        let rejection = list_flags(
            RequireAuth(foreign_project),
            State(app_state()),
            Path(2),
            Query(list_query()),
        )
        .await
        .err()
        .map(|p| p.status_code);

        assert_eq!(
            rejection,
            Some(StatusCode::FORBIDDEN),
            "a project-1 token must not list project 2's flags"
        );
    }

    /// Same confinement for the single-flag read.
    #[tokio::test]
    async fn deployment_token_cannot_get_another_projects_flag() {
        let rejection = get_flag(
            RequireAuth(AuthContext::new_deployment_token(
                1,
                Some(1),
                None,
                1,
                "app".to_string(),
                vec![DeploymentTokenPermission::FlagsRead],
            )),
            State(app_state()),
            Path((2, "victim.secret".to_string())),
        )
        .await
        .err()
        .map(|p| p.status_code);

        assert_eq!(rejection, Some(StatusCode::FORBIDDEN));
    }

    /// A full-access deployment token is still a *project-scoped* credential:
    /// the wildcard grants flag reads, not other tenants' data.
    #[tokio::test]
    async fn full_access_deployment_token_is_still_project_confined() {
        let rejection = list_flags(
            RequireAuth(AuthContext::new_deployment_token(
                1,
                Some(1),
                None,
                1,
                "app".to_string(),
                vec![DeploymentTokenPermission::FullAccess],
            )),
            State(app_state()),
            Path(2),
            Query(list_query()),
        )
        .await
        .err()
        .map(|p| p.status_code);

        assert_eq!(rejection, Some(StatusCode::FORBIDDEN));
    }

    /// A deployment token must not reach the management reads AT ALL — not even
    /// for its own project.
    ///
    /// This endpoint returns every environment's override, but a token is
    /// pinned to one environment. Allowing it would let a preview container
    /// read production flag values. `/flags/snapshot` is the machine surface
    /// and honours the pinning; this one is for humans and API keys.
    #[tokio::test]
    async fn deployment_token_cannot_read_management_endpoints_even_for_its_own_project() {
        let token = || {
            AuthContext::new_deployment_token(
                1,
                Some(1),
                None,
                1,
                "app".to_string(),
                vec![DeploymentTokenPermission::FlagsRead],
            )
        };

        assert_eq!(
            list_rejection(token()).await,
            Some(StatusCode::FORBIDDEN),
            "a token must not list flags: the response spans every environment"
        );

        let get_rejection = get_flag(
            RequireAuth(token()),
            State(app_state()),
            Path((1, "checkout.v2".to_string())),
        )
        .await
        .err()
        .map(|p| p.status_code);
        assert_eq!(
            get_rejection,
            Some(StatusCode::FORBIDDEN),
            "a token must not read a single flag either"
        );
    }

    /// ...but a human session must still reach it, or the guard is too strict.
    #[tokio::test]
    async fn session_can_still_reach_the_management_reads() {
        assert_ne!(
            list_rejection(AuthContext::new_session(test_user(), Role::Reader)).await,
            Some(StatusCode::FORBIDDEN),
            "Reader holds flags:read and is not a deployment token"
        );
    }

    /// The snapshot endpoint is the token's supported path and must keep
    /// working — this guard must not have closed the legitimate route.
    #[tokio::test]
    async fn deployment_token_can_still_reach_the_snapshot() {
        let result = get_flag_snapshot(
            RequireAuth(AuthContext::new_deployment_token(
                1,
                Some(1),
                None,
                1,
                "app".to_string(),
                vec![DeploymentTokenPermission::FlagsRead],
            )),
            State(app_state()),
            HeaderMap::new(),
            Query(SnapshotQuery {
                environment_id: None,
            }),
        )
        .await;

        assert_ne!(
            result.err().map(|p| p.status_code),
            Some(StatusCode::FORBIDDEN),
            "the snapshot is the documented machine surface"
        );
    }

    /// A container's baked-in `TEMPS_API_TOKEN` must never be able to change a
    /// production flag, only read one.
    #[tokio::test]
    async fn deployment_token_is_read_only() {
        let token = || {
            AuthContext::new_deployment_token(
                1,
                Some(1),
                None,
                1,
                "app".to_string(),
                vec![DeploymentTokenPermission::FlagsRead],
            )
        };

        assert_eq!(
            archive_rejection(token()).await,
            Some(StatusCode::FORBIDDEN),
            "a deployment token must not archive flags"
        );
    }

    /// The snapshot endpoint takes its scope from the token, never the URL.
    /// A session principal has no project, so it must be turned away rather
    /// than defaulting to something.
    #[tokio::test]
    async fn snapshot_requires_a_deployment_token() {
        let result = get_flag_snapshot(
            RequireAuth(AuthContext::new_session(test_user(), Role::Admin)),
            State(app_state()),
            HeaderMap::new(),
            Query(SnapshotQuery {
                environment_id: Some(1),
            }),
        )
        .await;

        assert_eq!(
            result.err().map(|p| p.status_code),
            Some(StatusCode::BAD_REQUEST)
        );
    }

    /// A project-wide token names no environment, so one must be supplied —
    /// and the service still checks it belongs to the token's project.
    #[tokio::test]
    async fn snapshot_requires_an_environment_for_a_project_wide_token() {
        let result = get_flag_snapshot(
            RequireAuth(AuthContext::new_deployment_token(
                1,
                None,
                None,
                1,
                "app".to_string(),
                vec![DeploymentTokenPermission::FlagsRead],
            )),
            State(app_state()),
            HeaderMap::new(),
            Query(SnapshotQuery {
                environment_id: None,
            }),
        )
        .await;

        assert_eq!(
            result.err().map(|p| p.status_code),
            Some(StatusCode::BAD_REQUEST)
        );
    }

    // -------------------------------------------------------------------
    // Write handlers
    //
    // These were the gap: removing a guard from `set_flag_environment` — the
    // endpoint that flips a flag in production — used to break nothing in CI.
    // -------------------------------------------------------------------

    async fn create_rejection(auth: AuthContext, project_id: i32) -> Option<StatusCode> {
        create_flag(
            RequireAuth(auth),
            State(app_state()),
            Extension(test_metadata()),
            Path(project_id),
            Json(CreateFlagRequest {
                key: "checkout.v2".to_string(),
                value_type: FlagValueType::Bool,
                default_value: serde_json::json!(false),
                description: None,
                client_visible: false,
                variants: Vec::new(),
                exposure_sample_rate: 0.0,
            }),
        )
        .await
        .err()
        .map(|p| p.status_code)
    }

    async fn update_rejection(auth: AuthContext, project_id: i32) -> Option<StatusCode> {
        update_flag(
            RequireAuth(auth),
            State(app_state()),
            Extension(test_metadata()),
            Path((project_id, "checkout.v2".to_string())),
            Json(UpdateFlagRequest {
                default_value: None,
                description: None,
                client_visible: Some(true),
                variants: None,
                exposure_sample_rate: None,
                expected_updated_at: None,
            }),
        )
        .await
        .err()
        .map(|p| p.status_code)
    }

    async fn set_environment_rejection(auth: AuthContext, project_id: i32) -> Option<StatusCode> {
        set_flag_environment(
            RequireAuth(auth),
            State(app_state()),
            Extension(test_metadata()),
            Path((project_id, "checkout.v2".to_string(), 1)),
            Json(SetFlagEnvironmentRequest {
                value: Some(Some(serde_json::json!(true))),
                enabled: None,
            }),
        )
        .await
        .err()
        .map(|p| p.status_code)
    }

    #[tokio::test]
    async fn write_handlers_reject_a_read_only_role() {
        let reader = || AuthContext::new_session(test_user(), Role::Reader);

        assert_eq!(
            create_rejection(reader(), 1).await,
            Some(StatusCode::FORBIDDEN)
        );
        assert_eq!(
            update_rejection(reader(), 1).await,
            Some(StatusCode::FORBIDDEN)
        );
        assert_eq!(
            set_environment_rejection(reader(), 1).await,
            Some(StatusCode::FORBIDDEN),
            "Reader must not be able to flip a flag in an environment"
        );
    }

    #[tokio::test]
    async fn write_handlers_reject_a_read_only_api_key() {
        let read_only = || {
            AuthContext::new_api_key(
                test_user(),
                None,
                Some(vec![Permission::FlagsRead]),
                "flags-read-key".to_string(),
                1,
            )
        };

        assert_eq!(
            create_rejection(read_only(), 1).await,
            Some(StatusCode::FORBIDDEN)
        );
        assert_eq!(
            update_rejection(read_only(), 1).await,
            Some(StatusCode::FORBIDDEN)
        );
        assert_eq!(
            set_environment_rejection(read_only(), 1).await,
            Some(StatusCode::FORBIDDEN)
        );
    }

    /// Deployment tokens cannot reach the write handlers at all.
    ///
    /// Note *why*: `permission_guard!(FlagsWrite)` rejects them before
    /// `project_scope_guard!` is ever consulted, because the token bridge only
    /// ever grants `FlagsRead`. So the scope guard on the write handlers is
    /// unreachable defence-in-depth, not the thing doing the work here — a
    /// sentinel run confirms removing it does not fail this test. It stays
    /// because "every project-scoped handler carries both guards" is a rule
    /// worth keeping mechanical rather than case-by-case.
    #[tokio::test]
    async fn write_handlers_reject_deployment_tokens_outright() {
        let token = || {
            AuthContext::new_deployment_token(
                1,
                Some(1),
                None,
                1,
                "app".to_string(),
                vec![DeploymentTokenPermission::FullAccess],
            )
        };

        for (name, status) in [
            ("create", create_rejection(token(), 2).await),
            ("update", update_rejection(token(), 2).await),
            (
                "set_environment",
                set_environment_rejection(token(), 2).await,
            ),
        ] {
            assert_eq!(
                status,
                Some(StatusCode::FORBIDDEN),
                "{name} must reject a deployment token"
            );
        }
    }

    /// A writer must still get through, or the guards are too strict.
    #[tokio::test]
    async fn write_handlers_admit_a_writer() {
        let admin = || AuthContext::new_session(test_user(), Role::Admin);

        assert_ne!(
            create_rejection(admin(), 1).await,
            Some(StatusCode::FORBIDDEN)
        );
        assert_ne!(
            update_rejection(admin(), 1).await,
            Some(StatusCode::FORBIDDEN)
        );
        assert_ne!(
            set_environment_rejection(admin(), 1).await,
            Some(StatusCode::FORBIDDEN)
        );
    }

    /// Exposure is reported by deployed apps, so a session principal (which
    /// has no project) must be turned away rather than defaulting to one.
    #[tokio::test]
    async fn exposure_requires_a_deployment_token() {
        let result = record_flag_exposure(
            RequireAuth(AuthContext::new_session(test_user(), Role::Admin)),
            State(app_state()),
            Json(RecordExposureRequest {
                keys: vec!["checkout.v2".to_string()],
            }),
        )
        .await;

        assert_eq!(
            result.err().map(|p| p.status_code),
            Some(StatusCode::BAD_REQUEST)
        );
    }

    /// A read-only deployment token may report exposure: it writes a timestamp,
    /// never a flag value, so this does not break "tokens cannot change what a
    /// flag serves".
    #[tokio::test]
    async fn exposure_is_allowed_for_a_read_only_deployment_token() {
        let result = record_flag_exposure(
            RequireAuth(AuthContext::new_deployment_token(
                1,
                Some(1),
                None,
                1,
                "app".to_string(),
                vec![DeploymentTokenPermission::FlagsRead],
            )),
            State(app_state()),
            Json(RecordExposureRequest { keys: vec![] }),
        )
        .await;

        assert_ne!(
            result.err().map(|p| p.status_code),
            Some(StatusCode::FORBIDDEN),
            "flags:read must be enough to report exposure"
        );
    }

    // =====================================================================
    // Targeting handlers (ADR 110 D3)
    // =====================================================================

    async fn rules_rejection(auth: AuthContext, project_id: i32) -> Option<StatusCode> {
        set_flag_rules(
            RequireAuth(auth),
            State(app_state()),
            Extension(test_metadata()),
            Path((project_id, "checkout.v2".to_string(), 1)),
            Json(SetFlagRulesRequest {
                rules: Vec::new(),
                expected_updated_at: None,
            }),
        )
        .await
        .err()
        .map(|p| p.status_code)
    }

    async fn reshuffle_rejection(auth: AuthContext, project_id: i32) -> Option<StatusCode> {
        reshuffle_flag(
            RequireAuth(auth),
            State(app_state()),
            Extension(test_metadata()),
            Path((project_id, "checkout.v2".to_string())),
        )
        .await
        .err()
        .map(|p| p.status_code)
    }

    async fn evaluate_rejection(auth: AuthContext, project_id: i32) -> Option<StatusCode> {
        evaluate_flag(
            RequireAuth(auth),
            State(app_state()),
            Path((project_id, "checkout.v2".to_string())),
            Json(EvaluateFlagRequest {
                environment_id: 1,
                key: None,
                attributes: std::collections::HashMap::new(),
            }),
        )
        .await
        .err()
        .map(|p| p.status_code)
    }

    /// Editing targeting and reshuffling a rollout change what a flag serves
    /// in production, so they need `flags:write` exactly like a value change.
    #[tokio::test]
    async fn targeting_writes_reject_readers_read_only_keys_and_deployment_tokens() {
        let reader = || AuthContext::new_session(test_user(), Role::Reader);
        let read_only_key = || {
            AuthContext::new_api_key(
                test_user(),
                None,
                Some(vec![Permission::FlagsRead]),
                "flags-read-key".to_string(),
                1,
            )
        };
        let token = || {
            AuthContext::new_deployment_token(
                1,
                Some(1),
                None,
                1,
                "app".to_string(),
                vec![DeploymentTokenPermission::FullAccess],
            )
        };

        for (who, rules, reshuffle) in [
            (
                "reader",
                rules_rejection(reader(), 1).await,
                reshuffle_rejection(reader(), 1).await,
            ),
            (
                "read-only key",
                rules_rejection(read_only_key(), 1).await,
                reshuffle_rejection(read_only_key(), 1).await,
            ),
            (
                "deployment token",
                rules_rejection(token(), 1).await,
                reshuffle_rejection(token(), 1).await,
            ),
        ] {
            assert_eq!(
                rules,
                Some(StatusCode::FORBIDDEN),
                "{who} must not set rules"
            );
            assert_eq!(
                reshuffle,
                Some(StatusCode::FORBIDDEN),
                "{who} must not reshuffle"
            );
        }
    }

    #[tokio::test]
    async fn targeting_writes_admit_a_writer() {
        let admin = || AuthContext::new_session(test_user(), Role::Admin);
        assert_ne!(
            rules_rejection(admin(), 1).await,
            Some(StatusCode::FORBIDDEN)
        );
        assert_ne!(
            reshuffle_rejection(admin(), 1).await,
            Some(StatusCode::FORBIDDEN)
        );
    }

    /// The console's test panel is a read, and like every project-scoped
    /// read it confines a deployment token to its own project.
    #[tokio::test]
    async fn evaluate_is_a_project_scoped_read() {
        assert_ne!(
            evaluate_rejection(AuthContext::new_session(test_user(), Role::Reader), 1).await,
            Some(StatusCode::FORBIDDEN),
            "a reader may test a flag"
        );
        let token = AuthContext::new_deployment_token(
            1,
            Some(1),
            None,
            1,
            "app".to_string(),
            vec![DeploymentTokenPermission::FlagsRead],
        );
        assert_eq!(
            evaluate_rejection(token, 2).await,
            Some(StatusCode::FORBIDDEN),
            "a project-1 token must not evaluate project 2's flags"
        );
    }

    async fn ofrep_server_status(auth: AuthContext) -> StatusCode {
        match ofrep_server_bulk(
            RequireAuth(auth),
            State(app_state()),
            HeaderMap::new(),
            Bytes::new(),
        )
        .await
        {
            Ok(response) => response.status(),
            Err(problem) => problem.status_code,
        }
    }

    /// Server-side OFREP is scoped by the deployment token alone: a session
    /// or a project-wide token has no environment to evaluate in.
    #[tokio::test]
    async fn server_ofrep_requires_an_environment_scoped_deployment_token() {
        assert_eq!(
            ofrep_server_status(AuthContext::new_session(test_user(), Role::Admin)).await,
            StatusCode::BAD_REQUEST
        );
        let project_wide = AuthContext::new_deployment_token(
            1,
            None,
            None,
            1,
            "app".to_string(),
            vec![DeploymentTokenPermission::FlagsRead],
        );
        assert_eq!(
            ofrep_server_status(project_wide).await,
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn server_ofrep_admits_a_read_only_environment_token() {
        let token = AuthContext::new_deployment_token(
            1,
            Some(1),
            None,
            1,
            "app".to_string(),
            vec![DeploymentTokenPermission::FlagsRead],
        );
        let status = ofrep_server_status(token).await;
        assert_ne!(status, StatusCode::FORBIDDEN);
        assert_ne!(status, StatusCode::BAD_REQUEST);
    }

    // =====================================================================
    // ETag
    // =====================================================================

    fn snapshot(key: &str, value: serde_json::Value) -> FlagSnapshot {
        FlagSnapshot {
            key: key.to_string(),
            value_type: FlagValueType::Bool,
            default_value: serde_json::json!(false),
            enabled: true,
            environment_value: Some(value),
            salt: "a1b2c3".to_string(),
            variants: Vec::new(),
            rules: Vec::new(),
        }
    }

    #[test]
    fn etag_is_stable_for_identical_snapshots() {
        let a = vec![snapshot("checkout.v2", serde_json::json!(true))];
        let b = vec![snapshot("checkout.v2", serde_json::json!(true))];

        assert_eq!(snapshot_etag(&a).unwrap(), snapshot_etag(&b).unwrap());
    }

    #[test]
    fn etag_changes_when_a_value_changes() {
        let a = vec![snapshot("checkout.v2", serde_json::json!(true))];
        let b = vec![snapshot("checkout.v2", serde_json::json!(false))];

        assert_ne!(snapshot_etag(&a).unwrap(), snapshot_etag(&b).unwrap());
    }

    #[test]
    fn etag_changes_when_a_flag_is_added() {
        let a = vec![snapshot("checkout.v2", serde_json::json!(true))];
        let b = vec![
            snapshot("checkout.v2", serde_json::json!(true)),
            snapshot("new.search", serde_json::json!(true)),
        ];

        assert_ne!(snapshot_etag(&a).unwrap(), snapshot_etag(&b).unwrap());
    }

    #[test]
    fn if_none_match_handles_weak_validators_and_lists() {
        let etag = "\"abc123\"";

        assert!(if_none_match_matches(
            &HeaderValue::from_static("\"abc123\""),
            etag
        ));
        assert!(
            if_none_match_matches(&HeaderValue::from_static("W/\"abc123\""), etag),
            "a weak validator must still match — intermediaries may add W/"
        );
        assert!(
            if_none_match_matches(&HeaderValue::from_static("\"other\", W/\"abc123\""), etag),
            "a comma-separated candidate list must be searched"
        );
        assert!(if_none_match_matches(&HeaderValue::from_static("*"), etag));
        assert!(!if_none_match_matches(
            &HeaderValue::from_static("\"nope\""),
            etag
        ));
    }

    #[test]
    fn etag_is_quoted_and_hex() {
        let etag = snapshot_etag(&[snapshot("a", serde_json::json!(true))]).unwrap();

        assert!(etag.starts_with('"') && etag.ends_with('"'), "{etag}");
        let inner = etag.trim_matches('"');
        assert_eq!(inner.len(), 32);
        assert!(inner.chars().all(|c| c.is_ascii_hexdigit()), "{etag}");
    }
}
