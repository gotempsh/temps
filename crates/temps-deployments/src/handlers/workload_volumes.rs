// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Admin API for an environment's volumes (ADR-055 §12, Phase 9): create,
//! list, attach (mount in every replica), detach, delete, and whether the
//! next deploy mounts them.
//!
//! Instance-admin like the rest of the workload-plane API: reads need
//! `SettingsRead`, writes `SettingsWrite`, and every write is audited with
//! the environment, the volume and what changed.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, put},
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::{
    problemdetails::{self, Problem, ProblemDetails},
    AuditContext, AuditLogger, RequestMetadata,
};
use temps_entities::environment_workload_mode::EnvironmentWorkloadMode;
use temps_workload_plane::VolumeRecord;
use tracing::error;
use utoipa::{IntoParams, OpenApi, ToSchema};

use crate::handlers::audit::{
    EnvironmentVolumeAttachmentChangedAudit, EnvironmentVolumeCreatedAudit,
    EnvironmentVolumeDeletedAudit,
};
use crate::services::workload_volumes::{
    CreateEnvironmentVolume, WorkloadVolumeError, WorkloadVolumeService,
};

pub struct WorkloadVolumesAppState {
    pub service: Arc<WorkloadVolumeService>,
    pub audit_service: Arc<dyn AuditLogger>,
}

impl From<WorkloadVolumeError> for Problem {
    fn from(error: WorkloadVolumeError) -> Self {
        let (status, title) = match &error {
            WorkloadVolumeError::EnvironmentNotFound { .. } => {
                (StatusCode::NOT_FOUND, "Environment Not Found")
            }
            WorkloadVolumeError::NotFound { .. } => (StatusCode::NOT_FOUND, "Volume Not Found"),
            WorkloadVolumeError::NameTaken { .. } => (StatusCode::CONFLICT, "Volume Name Taken"),
            WorkloadVolumeError::MountPathTaken { .. } => {
                (StatusCode::CONFLICT, "Mount Path Taken")
            }
            WorkloadVolumeError::Validation { .. } => (StatusCode::BAD_REQUEST, "Invalid Volume"),
            WorkloadVolumeError::InUse { .. } => (StatusCode::CONFLICT, "Volume In Use"),
            WorkloadVolumeError::Storage { .. } => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Volume Storage Failed")
            }
            WorkloadVolumeError::Database { .. } => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Volume Storage Failed")
            }
        };
        problemdetails::new(status)
            .with_title(title)
            .with_detail(error.to_string())
    }
}

// ── DTOs ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct VolumeAttachmentDto {
    /// Absolute path inside every replica.
    pub mount_path: String,
    pub read_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EnvironmentVolumeResponse {
    pub id: i64,
    pub environment_id: i32,
    pub name: String,
    /// `named` (a Docker named volume).
    pub backing: String,
    /// The node holding the data: `null` until first created, `0` for the
    /// control plane. A replica that mounts it is pinned there.
    pub node_id: Option<i32>,
    pub size_mb: Option<u64>,
    pub retain_on_delete: bool,
    /// Where replicas mount it; `null` when detached.
    pub attachment: Option<VolumeAttachmentDto>,
    /// The pin the console shows, e.g. "pinned to node 0 by volume `data`".
    pub pin_reason: Option<String>,
    /// ISO 8601, UTC.
    pub created_at: String,
    pub updated_at: String,
}

impl EnvironmentVolumeResponse {
    fn new(environment_id: i32, record: VolumeRecord) -> Self {
        let pin_reason = match (record.node_id, record.attachment.is_some()) {
            (Some(node_id), true) => Some(
                temps_workload_volumes::VolumePin {
                    node_id,
                    volumes: vec![record.name.clone()],
                }
                .to_string(),
            ),
            _ => None,
        };
        Self {
            id: record.id,
            environment_id,
            name: record.name,
            backing: record.backing.kind().as_str().to_string(),
            node_id: record.node_id,
            size_mb: record.size_mb,
            retain_on_delete: record.retain_on_delete,
            attachment: record.attachment.map(|a| VolumeAttachmentDto {
                mount_path: a.mount_path,
                read_only: a.read_only,
            }),
            pin_reason,
            created_at: record
                .created_at
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            updated_at: record
                .updated_at
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateEnvironmentVolumeRequest {
    /// 2-64 characters of `[a-z0-9_.-]`, starting with a letter or digit;
    /// unique per environment.
    pub name: String,
    #[serde(default)]
    pub size_mb: Option<u64>,
    /// Keep the data when the volume is deleted.
    #[serde(default)]
    pub retain_on_delete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AttachEnvironmentVolumeRequest {
    pub mount_path: String,
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct VolumeListQuery {
    /// 1-based page (default 1).
    pub page: Option<u64>,
    /// Default 20, at most 100.
    pub page_size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EnvironmentVolumeListResponse {
    /// Newest first.
    pub volumes: Vec<EnvironmentVolumeResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EnvironmentVolumeStatusResponse {
    pub environment_id: i32,
    pub workload_mode: EnvironmentWorkloadMode,
    pub replicas: i32,
    /// Attached volumes.
    pub attached: usize,
    /// Whether the next deploy mounts them.
    pub active: bool,
    /// Why they would not be mounted (the next deploy fails with it).
    pub reason: Option<String>,
    /// Console path that fixes it.
    pub setup_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DeletedEnvironmentVolumeResponse {
    pub volume: EnvironmentVolumeResponse,
    /// The Docker volume (and its data) was removed.
    pub backing_removed: bool,
    /// Why the data was kept, when it was.
    pub backing_kept_reason: Option<String>,
}

fn audit_context(auth: &temps_auth::AuthContext, metadata: &RequestMetadata) -> AuditContext {
    AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    }
}

async fn audit(state: &WorkloadVolumesAppState, operation: &dyn temps_core::AuditOperation) {
    if let Err(e) = state.audit_service.create_audit_log(operation).await {
        error!(
            operation = %operation.operation_type(),
            error = %e,
            "volume write succeeded but the audit record failed"
        );
    }
}

// ── Handlers ────────────────────────────────────────────────────────────────

#[utoipa::path(
    tag = "Workload Volumes", get,
    path = "/internal/environments/{environment_id}/volumes",
    operation_id = "admin_list_environment_volumes",
    params(("environment_id" = i32, Path, description = "Environment ID"), VolumeListQuery),
    responses(
        (status = 200, body = EnvironmentVolumeListResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Environment not found", body = ProblemDetails),
        (status = 500, description = "Storage failed", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn admin_list_environment_volumes(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WorkloadVolumesAppState>>,
    Path(environment_id): Path<i32>,
    Query(query): Query<VolumeListQuery>,
) -> Result<Json<EnvironmentVolumeListResponse>, Problem> {
    permission_guard!(auth, SettingsRead);
    let page = query.page.unwrap_or(1).max(1);
    let page_size = query
        .page_size
        .unwrap_or(temps_workload_plane::volumes::DEFAULT_PAGE_SIZE)
        .clamp(1, temps_workload_plane::volumes::MAX_PAGE_SIZE);
    let (records, total) = state
        .service
        .list(environment_id, Some(page), Some(page_size))
        .await?;
    Ok(Json(EnvironmentVolumeListResponse {
        volumes: records
            .into_iter()
            .map(|r| EnvironmentVolumeResponse::new(environment_id, r))
            .collect(),
        total,
        page,
        page_size,
    }))
}

#[utoipa::path(
    tag = "Workload Volumes", post,
    path = "/internal/environments/{environment_id}/volumes",
    operation_id = "admin_create_environment_volume",
    params(("environment_id" = i32, Path, description = "Environment ID")),
    request_body = CreateEnvironmentVolumeRequest,
    responses(
        (status = 201, body = EnvironmentVolumeResponse),
        (status = 400, description = "Invalid volume", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Instance admin required", body = ProblemDetails),
        (status = 404, description = "Environment not found", body = ProblemDetails),
        (status = 409, description = "Name taken", body = ProblemDetails),
        (status = 500, description = "Storage failed", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn admin_create_environment_volume(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WorkloadVolumesAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(environment_id): Path<i32>,
    Json(request): Json<CreateEnvironmentVolumeRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    let record = state
        .service
        .create(
            environment_id,
            CreateEnvironmentVolume {
                name: request.name,
                size_mb: request.size_mb,
                retain_on_delete: request.retain_on_delete,
            },
        )
        .await?;
    audit(
        &state,
        &EnvironmentVolumeCreatedAudit {
            context: audit_context(&auth, &metadata),
            environment_id,
            volume_id: record.id,
            name: record.name.clone(),
            size_mb: record.size_mb,
            retain_on_delete: record.retain_on_delete,
        },
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(EnvironmentVolumeResponse::new(environment_id, record)),
    ))
}

#[utoipa::path(
    tag = "Workload Volumes", put,
    path = "/internal/environments/{environment_id}/volumes/{volume_id}/attachment",
    operation_id = "admin_attach_environment_volume",
    params(
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("volume_id" = i64, Path, description = "Volume ID")
    ),
    request_body = AttachEnvironmentVolumeRequest,
    responses(
        (status = 200, body = EnvironmentVolumeResponse),
        (status = 400, description = "Invalid mount path", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Instance admin required", body = ProblemDetails),
        (status = 404, description = "Environment or volume not found", body = ProblemDetails),
        (status = 409, description = "Mount path taken", body = ProblemDetails),
        (status = 500, description = "Storage failed", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn admin_attach_environment_volume(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WorkloadVolumesAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((environment_id, volume_id)): Path<(i32, i64)>,
    Json(request): Json<AttachEnvironmentVolumeRequest>,
) -> Result<Json<EnvironmentVolumeResponse>, Problem> {
    permission_guard!(auth, SettingsWrite);
    let record = state
        .service
        .attach(
            environment_id,
            volume_id,
            &request.mount_path,
            request.read_only,
        )
        .await?;
    audit(
        &state,
        &EnvironmentVolumeAttachmentChangedAudit {
            context: audit_context(&auth, &metadata),
            environment_id,
            volume_id,
            name: record.name.clone(),
            mount_path: Some(request.mount_path),
            read_only: request.read_only,
        },
    )
    .await;
    Ok(Json(EnvironmentVolumeResponse::new(environment_id, record)))
}

#[utoipa::path(
    tag = "Workload Volumes", delete,
    path = "/internal/environments/{environment_id}/volumes/{volume_id}/attachment",
    operation_id = "admin_detach_environment_volume",
    params(
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("volume_id" = i64, Path, description = "Volume ID")
    ),
    responses(
        (status = 200, body = EnvironmentVolumeResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Instance admin required", body = ProblemDetails),
        (status = 404, description = "Volume not found", body = ProblemDetails),
        (status = 500, description = "Storage failed", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn admin_detach_environment_volume(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WorkloadVolumesAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((environment_id, volume_id)): Path<(i32, i64)>,
) -> Result<Json<EnvironmentVolumeResponse>, Problem> {
    permission_guard!(auth, SettingsWrite);
    let record = state.service.detach(environment_id, volume_id).await?;
    audit(
        &state,
        &EnvironmentVolumeAttachmentChangedAudit {
            context: audit_context(&auth, &metadata),
            environment_id,
            volume_id,
            name: record.name.clone(),
            mount_path: None,
            read_only: false,
        },
    )
    .await;
    Ok(Json(EnvironmentVolumeResponse::new(environment_id, record)))
}

#[utoipa::path(
    tag = "Workload Volumes", delete,
    path = "/internal/environments/{environment_id}/volumes/{volume_id}",
    operation_id = "admin_delete_environment_volume",
    params(
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("volume_id" = i64, Path, description = "Volume ID")
    ),
    responses(
        (status = 200, body = DeletedEnvironmentVolumeResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Instance admin required", body = ProblemDetails),
        (status = 404, description = "Volume not found", body = ProblemDetails),
        (status = 409, description = "Volume still mounted", body = ProblemDetails),
        (status = 500, description = "Storage failed", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn admin_delete_environment_volume(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WorkloadVolumesAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((environment_id, volume_id)): Path<(i32, i64)>,
) -> Result<Json<DeletedEnvironmentVolumeResponse>, Problem> {
    permission_guard!(auth, SettingsWrite);
    let deleted = state.service.delete(environment_id, volume_id).await?;
    audit(
        &state,
        &EnvironmentVolumeDeletedAudit {
            context: audit_context(&auth, &metadata),
            environment_id,
            volume_id,
            name: deleted.volume.name.clone(),
            backing_removed: deleted.backing_removed,
            backing_kept_reason: deleted.backing_kept_reason.clone(),
        },
    )
    .await;
    Ok(Json(DeletedEnvironmentVolumeResponse {
        volume: EnvironmentVolumeResponse::new(environment_id, deleted.volume),
        backing_removed: deleted.backing_removed,
        backing_kept_reason: deleted.backing_kept_reason,
    }))
}

#[utoipa::path(
    tag = "Workload Volumes", get,
    path = "/internal/environments/{environment_id}/volumes/status",
    operation_id = "admin_environment_volume_status",
    params(("environment_id" = i32, Path, description = "Environment ID")),
    responses(
        (status = 200, body = EnvironmentVolumeStatusResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Environment not found", body = ProblemDetails),
        (status = 500, description = "Storage failed", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn admin_environment_volume_status(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<WorkloadVolumesAppState>>,
    Path(environment_id): Path<i32>,
) -> Result<Json<EnvironmentVolumeStatusResponse>, Problem> {
    permission_guard!(auth, SettingsRead);
    let status = state.service.status(environment_id).await?;
    Ok(Json(EnvironmentVolumeStatusResponse {
        environment_id: status.environment_id,
        workload_mode: status.workload_mode,
        replicas: status.replicas,
        attached: status.attached,
        active: status.active,
        reason: status.reason,
        setup_path: status.setup_path,
    }))
}

pub fn configure_routes() -> Router<Arc<WorkloadVolumesAppState>> {
    Router::new()
        .route(
            "/internal/environments/{environment_id}/volumes",
            get(admin_list_environment_volumes).post(admin_create_environment_volume),
        )
        .route(
            "/internal/environments/{environment_id}/volumes/status",
            get(admin_environment_volume_status),
        )
        .route(
            "/internal/environments/{environment_id}/volumes/{volume_id}",
            axum::routing::delete(admin_delete_environment_volume),
        )
        .route(
            "/internal/environments/{environment_id}/volumes/{volume_id}/attachment",
            put(admin_attach_environment_volume).delete(admin_detach_environment_volume),
        )
}

#[derive(OpenApi)]
#[openapi(
    paths(
        admin_list_environment_volumes,
        admin_create_environment_volume,
        admin_attach_environment_volume,
        admin_detach_environment_volume,
        admin_delete_environment_volume,
        admin_environment_volume_status,
    ),
    components(schemas(
        VolumeAttachmentDto,
        EnvironmentVolumeResponse,
        CreateEnvironmentVolumeRequest,
        AttachEnvironmentVolumeRequest,
        EnvironmentVolumeListResponse,
        EnvironmentVolumeStatusResponse,
        DeletedEnvironmentVolumeResponse,
        EnvironmentWorkloadMode,
    )),
    tags((name = "Workload Volumes", description = "ADR-055 opt-in volumes for single-container deployments"))
)]
pub struct WorkloadVolumesApiDoc;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::wireguard_mesh::admin_test_support::*;
    use sea_orm::DatabaseConnection;

    fn app(db: DatabaseConnection, audit: Arc<RecordingAuditLogger>) -> axum::Router {
        let service = Arc::new(WorkloadVolumeService::new(Arc::new(db), None));
        configure_routes().with_state(Arc::new(WorkloadVolumesAppState {
            service,
            audit_service: audit,
        }))
    }

    #[tokio::test]
    async fn every_route_requires_authentication() {
        for (method, uri, body) in [
            ("GET", "/internal/environments/3/volumes", None),
            (
                "POST",
                "/internal/environments/3/volumes",
                Some(serde_json::json!({"name": "data"})),
            ),
            ("GET", "/internal/environments/3/volumes/status", None),
            (
                "PUT",
                "/internal/environments/3/volumes/9/attachment",
                Some(serde_json::json!({"mount_path": "/data"})),
            ),
            (
                "DELETE",
                "/internal/environments/3/volumes/9/attachment",
                None,
            ),
            ("DELETE", "/internal/environments/3/volumes/9", None),
        ] {
            let audit = Arc::new(RecordingAuditLogger::default());
            let (status, _, _) = send(
                app(mock_db().into_connection(), audit),
                method,
                uri,
                body,
                None,
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
        }
    }

    #[tokio::test]
    async fn writes_are_admin_only_and_a_refused_write_is_not_audited() {
        for (method, uri, body) in [
            (
                "POST",
                "/internal/environments/3/volumes",
                Some(serde_json::json!({"name": "data"})),
            ),
            (
                "PUT",
                "/internal/environments/3/volumes/9/attachment",
                Some(serde_json::json!({"mount_path": "/data"})),
            ),
            (
                "DELETE",
                "/internal/environments/3/volumes/9/attachment",
                None,
            ),
            ("DELETE", "/internal/environments/3/volumes/9", None),
        ] {
            let audit = Arc::new(RecordingAuditLogger::default());
            let (status, _, _) = send(
                app(mock_db().into_connection(), audit.clone()),
                method,
                uri,
                body,
                Some(settings_reader()),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
            assert!(audit.records().is_empty(), "{method} {uri}");
        }
        let audit = Arc::new(RecordingAuditLogger::default());
        let (status, _, _) = send(
            app(mock_db().into_connection(), audit),
            "GET",
            "/internal/environments/3/volumes",
            None,
            Some(outsider()),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "listing needs SettingsRead");
    }

    #[tokio::test]
    async fn creating_a_volume_for_an_unknown_environment_is_a_404_naming_it() {
        let db = mock_db()
            .append_query_results(vec![Vec::<(
                temps_entities::environments::Model,
                Option<temps_entities::projects::Model>,
            )>::new()])
            .into_connection();
        let audit = Arc::new(RecordingAuditLogger::default());
        let (status, _, body) = send(
            app(db, audit.clone()),
            "POST",
            "/internal/environments/99/volumes",
            Some(serde_json::json!({"name": "data"})),
            Some(admin()),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert!(body["detail"].as_str().unwrap_or_default().contains("99"));
        assert!(audit.records().is_empty());
    }

    #[test]
    fn every_error_maps_to_its_status() {
        let cases: Vec<(WorkloadVolumeError, StatusCode)> = vec![
            (
                WorkloadVolumeError::EnvironmentNotFound { environment_id: 3 },
                StatusCode::NOT_FOUND,
            ),
            (
                WorkloadVolumeError::NotFound {
                    volume_id: 9,
                    environment_id: 3,
                },
                StatusCode::NOT_FOUND,
            ),
            (
                WorkloadVolumeError::NameTaken {
                    environment_id: 3,
                    name: "data".into(),
                },
                StatusCode::CONFLICT,
            ),
            (
                WorkloadVolumeError::MountPathTaken {
                    environment_id: 3,
                    mount_path: "/data".into(),
                    other: "logs".into(),
                },
                StatusCode::CONFLICT,
            ),
            (
                WorkloadVolumeError::Validation {
                    environment_id: 3,
                    message: "bad".into(),
                },
                StatusCode::BAD_REQUEST,
            ),
            (
                WorkloadVolumeError::InUse {
                    volume_id: 9,
                    environment_id: 3,
                    name: "data".into(),
                    reason: "volume is in use".into(),
                },
                StatusCode::CONFLICT,
            ),
            (
                WorkloadVolumeError::Storage {
                    environment_id: 3,
                    reason: "boom".into(),
                },
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];
        for (error, expected) in cases {
            let text = error.to_string();
            let problem = Problem::from(error);
            assert_eq!(problem.status_code, expected, "{text}");
        }
    }

    #[test]
    fn a_response_carries_the_pin_reason_and_iso_dates() {
        let now = chrono::Utc::now();
        let record = VolumeRecord {
            id: 9,
            owner: temps_workload_plane::Owner::new("environment", 3),
            name: "data".into(),
            backing: temps_workload_volumes::Backing::Named {
                name: "temps-vol-environment-3-data".into(),
            },
            node_id: Some(0),
            size_mb: Some(1024),
            retain_on_delete: true,
            attachment: Some(temps_workload_plane::VolumeAttachment {
                mount_path: "/var/lib/app".into(),
                read_only: false,
            }),
            created_at: now,
            updated_at: now,
        };
        let response = EnvironmentVolumeResponse::new(3, record);
        assert_eq!(
            response.pin_reason.as_deref(),
            Some("pinned to node 0 by volume `data`")
        );
        assert!(
            response.created_at.ends_with('Z'),
            "{}",
            response.created_at
        );
        assert_eq!(response.backing, "named");
    }
}
