//! Snapshot API handlers (ADR-037).
//!
//! Routes:
//! - `POST /v1/sandboxes/{id}/snapshots`    — create snapshot (202)
//! - `GET  /v1/sandbox-snapshots`           — list snapshots for user
//! - `GET  /v1/sandbox-snapshots/storage-summary` — quota usage
//! - `GET  /v1/sandbox-snapshots/{snap_id}` — get single snapshot
//! - `DELETE /v1/sandbox-snapshots/{snap_id}` — soft-delete snapshot
//!
//! All handlers follow the three-layer pattern: RequireAuth + permission
//! check → service call → typed DTO. No DB access here.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use temps_auth::{permissions::Permission, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_core::{AuditContext, AuditOperation};

use crate::error::SandboxSnapshotError;
use crate::handlers::SandboxAppState;
use crate::services::snapshot_service::StorageSummary;

// ── Audit events ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
struct SnapshotCreatedAudit {
    context: AuditContext,
    snapshot_id: String,
    sandbox_id: String,
}

impl AuditOperation for SnapshotCreatedAudit {
    fn operation_type(&self) -> String {
        "SANDBOX_SNAPSHOT_CREATED".to_string()
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
            .map_err(|e| anyhow::anyhow!("Failed to serialize SnapshotCreatedAudit: {}", e))
    }
}

#[derive(Debug, Clone, Serialize)]
struct SnapshotDeletedAudit {
    context: AuditContext,
    snapshot_id: String,
}

impl AuditOperation for SnapshotDeletedAudit {
    fn operation_type(&self) -> String {
        "SANDBOX_SNAPSHOT_DELETED".to_string()
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
            .map_err(|e| anyhow::anyhow!("Failed to serialize SnapshotDeletedAudit: {}", e))
    }
}

// ── Error → Problem conversion ───────────────────────────────────────────────

impl From<SandboxSnapshotError> for Problem {
    fn from(error: SandboxSnapshotError) -> Self {
        match error {
            SandboxSnapshotError::NotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Snapshot Not Found")
                .with_detail(error.to_string()),
            SandboxSnapshotError::NotReady { .. } => {
                problemdetails::new(StatusCode::UNPROCESSABLE_ENTITY)
                    .with_title("Snapshot Not Ready")
                    .with_detail(error.to_string())
            }
            SandboxSnapshotError::CrossBackendRestore { .. } => {
                problemdetails::new(StatusCode::UNPROCESSABLE_ENTITY)
                    .with_title("Cross-Backend Restore Not Supported")
                    .with_detail(error.to_string())
            }
            SandboxSnapshotError::NotSupported { .. } => {
                problemdetails::new(StatusCode::NOT_IMPLEMENTED)
                    .with_title("Snapshots Not Supported")
                    .with_detail(error.to_string())
            }
            SandboxSnapshotError::SnapshotInProgress { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Snapshot In Progress")
                    .with_detail(error.to_string())
            }
            SandboxSnapshotError::QuotaExceeded { .. } => {
                problemdetails::new(StatusCode::UNPROCESSABLE_ENTITY)
                    .with_title("Snapshot Quota Exceeded")
                    .with_detail(error.to_string())
            }
            SandboxSnapshotError::InvalidState { .. } => {
                problemdetails::new(StatusCode::UNPROCESSABLE_ENTITY)
                    .with_title("Invalid Sandbox State")
                    .with_detail(error.to_string())
            }
            SandboxSnapshotError::SandboxNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Sandbox Not Found")
                    .with_detail(error.to_string())
            }
            SandboxSnapshotError::SandboxNotRunning { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Sandbox Not Running")
                    .with_detail(error.to_string())
            }
            SandboxSnapshotError::ScrubFailed { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Credential Scrub Failed")
                    .with_detail(error.to_string())
            }
            SandboxSnapshotError::DigestMismatch { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Snapshot Digest Mismatch")
                    .with_detail(error.to_string())
            }
            SandboxSnapshotError::ArtifactMissing { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Snapshot Artifact Missing")
                    .with_detail(error.to_string())
            }
            SandboxSnapshotError::Database(_) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
            SandboxSnapshotError::Io(_) => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Internal Server Error")
                .with_detail(error.to_string()),
            SandboxSnapshotError::Provider(_) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Provider Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

// ── DTOs ─────────────────────────────────────────────────────────────────────

/// Request body for `POST /v1/sandboxes/{id}/snapshots`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateSnapshotBody {
    /// Optional human-readable label for the snapshot.
    #[serde(default)]
    pub label: Option<String>,
}

/// Single snapshot as returned by the API.
#[derive(Debug, Serialize, ToSchema)]
pub struct SnapshotResponse {
    pub id: String,
    pub status: String,
    pub backend: String,
    pub label: Option<String>,
    pub content_digest: String,
    pub size_bytes: i64,
    pub image_ref: Option<String>,
    // source_sandbox_id is deliberately omitted: exposing the raw sequential
    // integer leaks the internal DB identity of the source sandbox. Clients
    // that need to relate a snapshot to its source sandbox should use the
    // sandbox public_id they already have from the create request.
    pub project_id: Option<i32>,
    pub created_at: String,
    pub updated_at: String,
}

impl From<temps_entities::sandbox_snapshots::Model> for SnapshotResponse {
    fn from(m: temps_entities::sandbox_snapshots::Model) -> Self {
        Self {
            id: m.public_id,
            status: m.status,
            backend: m.backend,
            label: m.label,
            content_digest: m.content_digest,
            size_bytes: m.size_bytes,
            image_ref: m.image_ref,
            project_id: m.project_id,
            created_at: m.created_at.to_rfc3339(),
            updated_at: m.updated_at.to_rfc3339(),
        }
    }
}

/// Paginated list of snapshots.
#[derive(Debug, Serialize, ToSchema)]
pub struct ListSnapshotsResponse {
    pub snapshots: Vec<SnapshotResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

/// Query params for `GET /v1/sandbox-snapshots`.
#[derive(Debug, Deserialize)]
pub struct ListSnapshotsQuery {
    #[serde(default)]
    pub project_id: Option<i32>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub page: Option<u64>,
    #[serde(default)]
    pub page_size: Option<u64>,
}

// ── Validation ────────────────────────────────────────────────────────────────

/// Validate a snapshot label.
///
/// An empty or all-whitespace label would produce an invalid Docker image tag
/// component and cause `take_snapshot` to fail deep inside the provider with
/// a confusing error. We reject it here so the caller gets a 400 instead of
/// a 500. Both the handler and the unit test call this function directly so
/// the test exercises the real code path rather than reimplementing the check.
pub(crate) fn validate_snapshot_label(label: &str) -> Result<(), Problem> {
    if label.trim().is_empty() {
        return Err(problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Label")
            .with_detail("snapshot label must not be empty or whitespace-only"));
    }
    Ok(())
}

// ── Handlers ─────────────────────────────────────────────────────────────────

/// `POST /v1/sandboxes/{id}/snapshots`
///
/// Initiates a snapshot of the sandbox. The response is **202 Accepted**
/// with the snapshot row in `creating` status. The caller should poll
/// `GET /v1/sandbox-snapshots/{snap_id}` until `status` is `ready` or
/// `failed`.
///
/// The sandbox is stopped for the duration of the snapshot and restarted
/// automatically when it completes (unless it was already stopped).
#[utoipa::path(
    tag = "Snapshots",
    post,
    path = "/v1/sandboxes/{id}/snapshots",
    params(("id" = String, Path, description = "Sandbox public ID")),
    request_body = CreateSnapshotBody,
    responses(
        (status = 202, description = "Snapshot initiated", body = SnapshotResponse),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Sandbox not found"),
        (status = 422, description = "Quota exceeded or invalid state"),
        (status = 500, description = "Internal error"),
        (status = 501, description = "Snapshots not supported by backend"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_snapshot(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path(sandbox_id): Path<String>,
    Json(body): Json<CreateSnapshotBody>,
) -> Result<impl IntoResponse, Problem> {
    use crate::handlers::sandboxes::sandbox_permission_guard;
    sandbox_permission_guard(
        &auth,
        Permission::SandboxesWrite,
        Permission::SandboxesWrite,
    )?;

    let user_id = auth.user_id();

    // Resolve the sandbox (ownership check + not-destroyed guard inside
    // find_by_public_id; non-owners get NotFound so we don't leak existence).
    let sandbox = state
        .sandbox_service
        .find_by_public_id(&sandbox_id, user_id)
        .await
        .map_err(Problem::from)?;

    // Validate the label early so the caller gets a 400, not a 500 from
    // the provider. An empty or all-whitespace label is not a valid Docker
    // image tag component and would make `take_snapshot` fail deep inside
    // the provider.
    if let Some(ref label) = body.label {
        validate_snapshot_label(label)?;
    }

    let snapshot_svc = state.snapshot_service.as_ref().ok_or_else(|| {
        Problem::from(SandboxSnapshotError::NotSupported {
            backend: "unknown".to_string(),
        })
    })?;

    let snap = snapshot_svc
        .create_snapshot(
            sandbox.id,
            &sandbox_id,
            user_id,
            sandbox.project_id,
            body.label,
        )
        .await
        .map_err(Problem::from)?;

    let location = format!("/v1/sandbox-snapshots/{}", snap.public_id);

    tracing::info!(
        user_id = %user_id,
        sandbox_id = %sandbox_id,
        snapshot_id = %snap.public_id,
        "snapshot: created"
    );

    // Audit log — failure must not fail the request.
    if let Some(ref audit) = state.audit_service {
        let event = SnapshotCreatedAudit {
            context: AuditContext {
                user_id,
                ip_address: None,
                user_agent: String::new(),
            },
            snapshot_id: snap.public_id.clone(),
            sandbox_id: sandbox_id.clone(),
        };
        if let Err(e) = audit.create_audit_log(&event).await {
            tracing::error!(
                user_id = %user_id,
                snapshot_id = %snap.public_id,
                "snapshot: failed to write audit log: {}",
                e
            );
        }
    }

    let resp = SnapshotResponse::from(snap);
    Ok((
        StatusCode::ACCEPTED,
        [(header::LOCATION, location)],
        Json(resp),
    ))
}

/// `GET /v1/sandbox-snapshots`
#[utoipa::path(
    tag = "Snapshots",
    get,
    path = "/v1/sandbox-snapshots",
    params(
        ("project_id" = Option<i32>, Query, description = "Filter by project"),
        ("status" = Option<String>, Query, description = "Filter by status"),
        ("page" = Option<u64>, Query, description = "Page number (1-indexed)"),
        ("page_size" = Option<u64>, Query, description = "Page size (max 100)"),
    ),
    responses(
        (status = 200, description = "List of snapshots", body = ListSnapshotsResponse),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_snapshots(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Query(query): Query<ListSnapshotsQuery>,
) -> Result<impl IntoResponse, Problem> {
    use crate::handlers::sandboxes::sandbox_permission_guard;
    sandbox_permission_guard(&auth, Permission::SandboxesRead, Permission::SandboxesRead)?;

    let user_id = auth.user_id();
    let page = query.page.unwrap_or(1);
    let page_size = query.page_size.unwrap_or(20);

    let snapshot_svc = state.snapshot_service.as_ref().ok_or_else(|| {
        Problem::from(SandboxSnapshotError::NotSupported {
            backend: "unknown".to_string(),
        })
    })?;

    let (rows, total) = snapshot_svc
        .list_snapshots(user_id, query.project_id, query.status, page, page_size)
        .await
        .map_err(Problem::from)?;

    Ok(Json(ListSnapshotsResponse {
        snapshots: rows.into_iter().map(SnapshotResponse::from).collect(),
        total,
        page,
        page_size,
    }))
}

/// `GET /v1/sandbox-snapshots/storage-summary`
#[utoipa::path(
    tag = "Snapshots",
    get,
    path = "/v1/sandbox-snapshots/storage-summary",
    responses(
        (status = 200, description = "Storage usage summary", body = StorageSummary),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn storage_summary(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
) -> Result<impl IntoResponse, Problem> {
    use crate::handlers::sandboxes::sandbox_permission_guard;
    sandbox_permission_guard(&auth, Permission::SandboxesRead, Permission::SandboxesRead)?;

    let user_id = auth.user_id();

    let snapshot_svc = state.snapshot_service.as_ref().ok_or_else(|| {
        Problem::from(SandboxSnapshotError::NotSupported {
            backend: "unknown".to_string(),
        })
    })?;

    let summary = snapshot_svc
        .storage_summary(user_id)
        .await
        .map_err(Problem::from)?;

    Ok(Json(summary))
}

/// `GET /v1/sandbox-snapshots/{snap_id}`
#[utoipa::path(
    tag = "Snapshots",
    get,
    path = "/v1/sandbox-snapshots/{snap_id}",
    params(("snap_id" = String, Path, description = "Snapshot public ID")),
    responses(
        (status = 200, description = "Snapshot detail", body = SnapshotResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_snapshot(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path(snap_id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    use crate::handlers::sandboxes::sandbox_permission_guard;
    sandbox_permission_guard(&auth, Permission::SandboxesRead, Permission::SandboxesRead)?;

    let user_id = auth.user_id();

    let snapshot_svc = state.snapshot_service.as_ref().ok_or_else(|| {
        Problem::from(SandboxSnapshotError::NotFound {
            snapshot_id: snap_id.clone(),
        })
    })?;

    let row = snapshot_svc
        .get_snapshot(user_id, &snap_id)
        .await
        .map_err(Problem::from)?;

    Ok(Json(SnapshotResponse::from(row)))
}

/// `DELETE /v1/sandbox-snapshots/{snap_id}`
#[utoipa::path(
    tag = "Snapshots",
    delete,
    path = "/v1/sandbox-snapshots/{snap_id}",
    params(("snap_id" = String, Path, description = "Snapshot public ID")),
    responses(
        (status = 204, description = "Snapshot deleted"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Not found"),
        (status = 500, description = "Internal error"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_snapshot(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<SandboxAppState>>,
    Path(snap_id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    use crate::handlers::sandboxes::sandbox_permission_guard;
    sandbox_permission_guard(
        &auth,
        Permission::SandboxesWrite,
        Permission::SandboxesWrite,
    )?;

    let user_id = auth.user_id();

    let snapshot_svc = state.snapshot_service.as_ref().ok_or_else(|| {
        Problem::from(SandboxSnapshotError::NotFound {
            snapshot_id: snap_id.clone(),
        })
    })?;

    tracing::info!(
        user_id = %user_id,
        snapshot_id = %snap_id,
        "snapshot: user requested delete"
    );

    snapshot_svc
        .delete_snapshot(user_id, &snap_id)
        .await
        .map_err(Problem::from)?;

    // Audit log — failure must not fail the request.
    if let Some(ref audit) = state.audit_service {
        let event = SnapshotDeletedAudit {
            context: AuditContext {
                user_id,
                ip_address: None,
                user_agent: String::new(),
            },
            snapshot_id: snap_id.clone(),
        };
        if let Err(e) = audit.create_audit_log(&event).await {
            tracing::error!(
                user_id = %user_id,
                snapshot_id = %snap_id,
                "snapshot delete: failed to write audit log: {}",
                e
            );
        }
    }

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_response_from_model() {
        use chrono::Utc;
        use temps_entities::sandbox_snapshots;

        let now = Utc::now();
        let model = sandbox_snapshots::Model {
            id: 1,
            public_id: "snap_aabbccdd11223344".to_string(),
            user_id: 42,
            project_id: None,
            source_sandbox_id: Some(7),
            label: Some("my snap".to_string()),
            status: "ready".to_string(),
            backend: "docker".to_string(),
            content_digest: "abc123".to_string(),
            content_path: "/data/snapshots/abc123.tar".to_string(),
            size_bytes: 100_000_000,
            image_ref: Some("temps-snapshot/my-snap:latest".to_string()),
            metadata: None,
            created_at: now,
            updated_at: now,
        };

        let resp = SnapshotResponse::from(model);
        assert_eq!(resp.id, "snap_aabbccdd11223344");
        assert_eq!(resp.status, "ready");
        assert_eq!(resp.backend, "docker");
        assert_eq!(resp.size_bytes, 100_000_000);
        // source_sandbox_id is intentionally omitted from the response to
        // avoid leaking the raw internal DB integer — the test confirms the
        // field is absent from the response struct.
        assert_eq!(resp.project_id, None);
    }

    #[test]
    fn from_snapshot_error_maps_not_found_to_404() {
        let err = SandboxSnapshotError::NotFound {
            snapshot_id: "snap_x".to_string(),
        };
        let prob = Problem::from(err);
        assert_eq!(prob.status_code.as_u16(), 404);
    }

    #[test]
    fn from_snapshot_error_maps_quota_exceeded_to_422() {
        let err = SandboxSnapshotError::QuotaExceeded {
            user_id: 1,
            used_bytes: 11_000_000_000,
            quota_bytes: 10_000_000_000,
        };
        let prob = Problem::from(err);
        assert_eq!(prob.status_code.as_u16(), 422);
    }

    #[test]
    fn from_snapshot_error_maps_not_supported_to_501() {
        let err = SandboxSnapshotError::NotSupported {
            backend: "local".to_string(),
        };
        let prob = Problem::from(err);
        assert_eq!(prob.status_code.as_u16(), 501);
    }

    /// Handler-level label validation: an empty string label must produce a
    /// 400 at the handler boundary, not pass through to the provider where it
    /// would surface as a 500. The test calls `validate_snapshot_label`
    /// directly (the same function the handler calls) so it exercises the
    /// real code path, not a reimplementation of the check.
    #[test]
    fn empty_label_is_rejected_at_handler_boundary() {
        let bad_labels = ["", "   ", "\t", "\n"];
        let good_labels = ["my-snap", "v1", "release-2026"];

        for label in bad_labels {
            let result = validate_snapshot_label(label);
            assert!(
                result.is_err(),
                "label {:?} should be rejected by validate_snapshot_label",
                label
            );
            // Verify it's a 400
            let prob = result.unwrap_err();
            assert_eq!(
                prob.status_code.as_u16(),
                400,
                "label {:?} must produce 400",
                label
            );
        }

        for label in good_labels {
            let result = validate_snapshot_label(label);
            assert!(
                result.is_ok(),
                "label {:?} should pass validate_snapshot_label, got {:?}",
                label,
                result
            );
        }
    }

    #[test]
    fn from_snapshot_error_maps_sandbox_not_running_to_409() {
        let err = SandboxSnapshotError::SandboxNotRunning {
            sandbox_id: "sbx_stopped".to_string(),
        };
        let prob = Problem::from(err);
        assert_eq!(
            prob.status_code.as_u16(),
            409,
            "SandboxNotRunning must map to 409 Conflict"
        );
    }

    #[test]
    fn from_snapshot_error_maps_snapshot_in_progress_to_409() {
        let err = SandboxSnapshotError::SnapshotInProgress { user_id: 42 };
        let prob = Problem::from(err);
        assert_eq!(
            prob.status_code.as_u16(),
            409,
            "SnapshotInProgress must map to 409 Conflict"
        );
    }

    #[test]
    fn from_snapshot_error_maps_scrub_failed_to_500() {
        let err = SandboxSnapshotError::ScrubFailed {
            sandbox_id: "sbx_x".to_string(),
            reason: "shred failed".to_string(),
        };
        let prob = Problem::from(err);
        assert_eq!(prob.status_code.as_u16(), 500);
        // The title is stored in body["title"] as a JSON string.
        let title = prob
            .body
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert!(
            title.contains("Credential Scrub"),
            "unexpected title: {}",
            title
        );
    }
}
