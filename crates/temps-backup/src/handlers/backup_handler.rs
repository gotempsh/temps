use crate::handlers::audit::{
    AuditContext, BackupRunAudit, BackupScheduleStatusChangedAudit, ExternalServiceBackupRunAudit,
    S3SourceCreatedAudit, S3SourceDeletedAudit, S3SourceUpdatedAudit,
};
use crate::handlers::types::BackupAppState;
use crate::services::BackupError;
use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, patch, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use temps_auth::permission_guard;
use temps_auth::RequireAuth;
use temps_core::problemdetails;
use temps_core::problemdetails::{Problem, ProblemDetails};
use temps_core::RequestMetadata;
use tracing::error;
use utoipa::{OpenApi, ToSchema};

impl From<BackupError> for Problem {
    fn from(error: BackupError) -> Self {
        match error {
            BackupError::NotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Resource Not Found")
                .with_detail(error.to_string()),

            BackupError::Validation(ref msg) => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Validation Error")
                .with_detail(msg.clone()),

            BackupError::Schedule(ref msg) => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Schedule Error")
                .with_detail(msg.clone()),

            BackupError::Database(_)
            | BackupError::S3(_)
            | BackupError::Configuration(_)
            | BackupError::ExternalService(_)
            | BackupError::Internal { .. }
            | BackupError::NotificationError(_)
            | BackupError::Unsupported(_)
            | BackupError::Io(_)
            | BackupError::Serialization(_) => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_s3_sources,
        create_s3_source,
        get_s3_source,
        update_s3_source,
        delete_s3_source,
        set_default_s3_source,
        test_s3_source_connection,
        test_s3_connection_preview,
        list_backup_schedules,
        create_backup_schedule,
        get_backup_schedule,
        delete_backup_schedule,
        list_backups_for_schedule,
        run_backup_for_source,
        list_source_backups,
        get_backup,
        disable_backup_schedule,
        enable_backup_schedule,
        run_external_service_backup
    ),
    components(
        schemas(
            CreateS3SourceRequest,
            UpdateS3SourceRequest,
            CreateBackupScheduleRequest,
            RunBackupRequest,
            RunExternalServiceBackupRequest,
            S3SourceResponse,
            S3ConnectionTestResponse,
            BackupScheduleResponse,
            BackupResponse,
            ExternalServiceBackupResponse,
            SourceBackupIndexResponse,
            SourceBackupEntry,
        )
    ),
    info(
        title = "Backups API",
        description = "API endpoints for managing backup operations and schedules. \
        Handles S3 source configuration, backup scheduling, execution, and monitoring.",
        version = "1.0.0"
    ),
    tags(
        (name = "Backups", description = "Backup management endpoints")
    )
)]
pub struct BackupApiDoc;

#[derive(Deserialize, ToSchema, Clone)]
pub struct CreateS3SourceRequest {
    pub name: String,
    pub bucket_name: String,
    pub bucket_path: String,
    pub access_key_id: String,
    pub secret_key: String,
    pub region: String,
    /// Optional endpoint URL for S3-compatible services like MinIO
    #[schema(example = "http://minio.example.com:9000")]
    pub endpoint: Option<String>,
    /// Whether to use path-style addressing (default: true)
    #[schema(example = true)]
    pub force_path_style: Option<bool>,
    /// When true, make this the default source (will swap out any existing default).
    /// The very first S3 source is always created as default regardless of this flag.
    #[schema(example = false)]
    pub is_default: Option<bool>,
}

#[derive(Deserialize, ToSchema, Clone)]
pub struct UpdateS3SourceRequest {
    /// Optional new name for the source
    pub name: Option<String>,
    /// Optional new bucket name
    pub bucket_name: Option<String>,
    /// Optional new bucket path
    pub bucket_path: Option<String>,
    /// Optional new access key ID
    #[schema(example = "AKIAXXXXXXXXXXXXXXXX")]
    pub access_key_id: Option<String>,
    /// Optional new secret key
    pub secret_key: Option<String>,
    /// Optional new region
    pub region: Option<String>,
    /// Optional new endpoint URL for S3-compatible services
    #[schema(example = "http://minio.example.com:9000")]
    pub endpoint: Option<String>,
    /// Optional new path-style addressing setting
    #[schema(example = true)]
    pub force_path_style: Option<bool>,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateBackupScheduleRequest {
    pub name: String,
    pub backup_type: String,
    pub retention_period: i32,
    /// Optional S3 source. If omitted, the current default S3 source is used.
    pub s3_source_id: Option<i32>,
    pub schedule_expression: String,
    pub enabled: bool,
    pub description: Option<String>,
    pub tags: Vec<String>,
}

#[derive(Deserialize, ToSchema, Clone)]
pub struct RunBackupRequest {
    /// Type of backup to perform
    #[schema(example = "full")]
    pub backup_type: String,
}

#[derive(Deserialize, ToSchema, Clone)]
pub struct RunExternalServiceBackupRequest {
    /// ID of the S3 source to store the backup. If omitted, the current default S3 source is used.
    #[schema(example = 1)]
    pub s3_source_id: Option<i32>,
    /// Type of backup to perform (e.g., "full", "incremental")
    #[schema(example = "full")]
    pub backup_type: Option<String>,
}

/// Response type for external service backup
#[derive(Debug, Serialize, ToSchema)]
pub struct ExternalServiceBackupResponse {
    pub id: i32,
    pub service_id: i32,
    pub backup_id: i32,
    pub backup_type: String,
    pub state: String,
    #[schema(example = "2025-01-15T14:30:00.123Z")]
    pub started_at: String,
    #[schema(example = "2025-01-15T14:35:00.456Z")]
    pub finished_at: Option<String>,
    pub size_bytes: Option<i64>,
    pub s3_location: String,
    pub error_message: Option<String>,
    pub metadata: serde_json::Value,
    pub checksum: Option<String>,
    pub compression_type: String,
    pub created_by: i32,
    #[schema(example = "2025-02-15T14:30:00.123Z")]
    pub expires_at: Option<String>,
}

impl From<temps_entities::external_service_backups::Model> for ExternalServiceBackupResponse {
    fn from(backup: temps_entities::external_service_backups::Model) -> Self {
        Self {
            id: backup.id,
            service_id: backup.service_id,
            backup_id: backup.backup_id,
            backup_type: backup.backup_type,
            state: backup.state,
            started_at: backup.started_at.to_rfc3339(),
            finished_at: backup.finished_at.map(|dt| dt.to_rfc3339()),
            size_bytes: backup.size_bytes,
            s3_location: backup.s3_location,
            error_message: backup.error_message,
            metadata: backup.metadata.clone(),
            checksum: backup.checksum,
            compression_type: backup.compression_type,
            created_by: backup.created_by,
            expires_at: backup.expires_at.map(|dt| dt.to_rfc3339()),
        }
    }
}

/// Response type for S3 source
#[derive(Serialize, ToSchema)]
pub struct S3SourceResponse {
    pub id: i32,
    pub name: String,
    pub bucket_name: String,
    pub bucket_path: String,
    #[schema(example = "AKIAXXXXXXXXXXXXXXXX")]
    pub access_key_id: String,
    #[schema(write_only)]
    pub secret_key: String,
    pub region: String,
    #[schema(example = "http://minio.example.com:9000")]
    pub endpoint: Option<String>,
    pub force_path_style: Option<bool>,
    pub is_default: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Response body for an S3 connection test.
#[derive(Serialize, ToSchema)]
pub struct S3ConnectionTestResponse {
    /// Whether the connection and credentials worked.
    pub ok: bool,
    /// Human-readable message (success confirmation or error detail).
    pub message: String,
}

/// Response type for backup schedule
#[derive(Serialize, ToSchema)]
pub struct BackupScheduleResponse {
    pub id: i32,
    pub name: String,
    pub backup_type: String,
    pub retention_period: i32,
    pub s3_source_id: i32,
    #[schema(example = "0 0 * * *")]
    pub schedule_expression: String,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub next_run: Option<i64>,
    pub last_run: Option<i64>,
}

/// Response type for backup
#[derive(Debug, Serialize, ToSchema)]
pub struct BackupResponse {
    pub id: i32,
    pub name: String,
    pub backup_id: String,
    pub schedule_id: Option<i32>,
    pub backup_type: String,
    pub state: String,
    pub started_at: i64,
    pub completed_at: Option<i64>,
    /// Final size of the backup once completed. Null while running.
    pub size_bytes: Option<i64>,
    /// Best-effort partial size while a backup is still running, computed
    /// by listing the S3 prefix. Null when the backup is finished
    /// (`size_bytes` is authoritative in that case).
    pub live_size_bytes: Option<i64>,
    pub file_count: Option<i32>,
    pub s3_source_id: i32,
    pub s3_location: String,
    pub error_message: Option<String>,
    pub metadata: serde_json::Value,
    pub checksum: Option<String>,
    pub compression_type: String,
    pub created_by: i32,
    pub expires_at: Option<i64>,
    pub tags: Vec<String>,
    /// Last time the worker reported progress. Older than ~5 minutes while
    /// `state == "running"` indicates a stalled backup the UI should flag.
    pub last_heartbeat_at: Option<i64>,
    /// True when `state == "running"` but the heartbeat is stale, suggesting
    /// the worker process died mid-backup.
    pub stalled: bool,
}

/// Response type for source backup index
#[derive(Serialize, ToSchema)]
pub struct SourceBackupIndexResponse {
    /// List of backups in the source
    pub backups: Vec<SourceBackupEntry>,
    /// When the index was last updated
    #[schema(example = "2024-01-15T14:30:00.123Z")]
    pub last_updated: String,
}

/// Entry in the source backup index. Covers both DB-tracked backups
/// (have a row in `backups`) and S3-scan discoveries (raw S3 objects with
/// no DB row — used for disaster-recovery from another Temps instance).
#[derive(Serialize, ToSchema)]
pub struct SourceBackupEntry {
    /// DB row id. Zero for S3-scan entries that have no DB row.
    #[schema(example = 1)]
    pub id: i32,
    /// UUID identifier from the DB row. Empty for S3-scan entries.
    #[schema(example = "550e8400-e29b-41d4-a716-446655440000")]
    pub backup_id: String,
    /// Human-friendly display name ("postgres backup (svc-name)" for DB
    /// rows, or a synthesized label derived from the S3 path for scans).
    #[schema(example = "postgres backup (postgres-n4ea)")]
    pub name: String,
    /// Backup variant as recorded by the backup pipeline (e.g. "full").
    #[schema(example = "full")]
    pub backup_type: String,
    /// When the backup was created. For S3-scan entries this is the
    /// object's LastModified time.
    #[schema(example = "2024-01-15T14:30:00.123Z")]
    pub created_at: String,
    /// Size of the backup in bytes, if known.
    #[schema(example = 1024000)]
    pub size_bytes: Option<i64>,
    /// Raw S3 URL / key where the backup sits. For Postgres WAL-G backups
    /// this starts with `s3://`; for pg_dump-style backups it's the
    /// relative object key.
    #[schema(example = "s3://bucket/external_services/postgres/svc-name/walg")]
    pub location: String,
    /// Sidecar metadata.json location, if any. Empty when none.
    #[schema(example = "")]
    pub metadata_location: String,
    /// Engine that produced the backup ("postgres", "redis", "mongodb",
    /// "s3", "rustfs"). Used by the UI to mark engine-compat with the
    /// target service.
    #[schema(example = "postgres")]
    pub engine: Option<String>,
    /// Name of the service that produced the backup. For S3-scan entries
    /// this is parsed from the S3 path.
    #[schema(example = "postgres-n4ea")]
    pub origin_service_name: Option<String>,
    /// Storage format: "walg" for continuous-archive (PITR-capable),
    /// "pg_dump" for point-in-time dumps, "" for non-postgres.
    #[schema(example = "walg")]
    pub format: Option<String>,
    /// Provenance: "db" for rows in this Temps, "s3_scan" for objects
    /// discovered by the S3 bucket walk (e.g., backups made by another
    /// Temps instance).
    #[schema(example = "db")]
    pub source: String,
    /// Observed state ("completed", "running", "failed") — DB only.
    /// Empty string for S3-scan entries.
    #[schema(example = "completed")]
    pub state: String,
}

impl From<temps_entities::s3_sources::Model> for S3SourceResponse {
    fn from(source: temps_entities::s3_sources::Model) -> Self {
        Self {
            id: source.id,
            name: source.name,
            bucket_name: source.bucket_name,
            bucket_path: source.bucket_path,
            // Credentials are encrypted at rest — mask them in API responses
            access_key_id: "***".to_string(),
            secret_key: "***".to_string(),
            region: source.region,
            endpoint: source.endpoint,
            force_path_style: source.force_path_style,
            is_default: source.is_default,
            created_at: source.created_at.timestamp_millis(),
            updated_at: source.updated_at.timestamp_millis(),
        }
    }
}

impl From<temps_entities::backup_schedules::Model> for BackupScheduleResponse {
    fn from(schedule: temps_entities::backup_schedules::Model) -> Self {
        Self {
            id: schedule.id,
            name: schedule.name,
            backup_type: schedule.backup_type,
            retention_period: schedule.retention_period,
            s3_source_id: schedule.s3_source_id,
            schedule_expression: schedule.schedule_expression,
            enabled: schedule.enabled,
            created_at: schedule.created_at.timestamp_millis(),
            updated_at: schedule.updated_at.timestamp_millis(),
            description: schedule.description,
            tags: serde_json::from_str(&schedule.tags).unwrap_or_default(),
            next_run: schedule.next_run.map(|dt| dt.timestamp_millis()),
            last_run: schedule.last_run.map(|dt| dt.timestamp_millis()),
        }
    }
}

/// Heartbeat older than this while state=running marks the row as stalled.
pub const BACKUP_STALL_THRESHOLD_SECS: i64 = 300;

impl From<temps_entities::backups::Model> for BackupResponse {
    fn from(backup: temps_entities::backups::Model) -> Self {
        let stalled = backup.state == "running"
            && backup
                .last_heartbeat_at
                .map(|hb| (chrono::Utc::now() - hb).num_seconds() > BACKUP_STALL_THRESHOLD_SECS)
                .unwrap_or_else(|| {
                    // No heartbeat yet AND started long enough ago.
                    (chrono::Utc::now() - backup.started_at).num_seconds()
                        > BACKUP_STALL_THRESHOLD_SECS
                });

        Self {
            id: backup.id,
            name: backup.name,
            backup_id: backup.backup_id,
            schedule_id: backup.schedule_id,
            backup_type: backup.backup_type,
            state: backup.state,
            started_at: backup.started_at.timestamp_millis(),
            completed_at: backup.finished_at.map(|dt| dt.timestamp_millis()),
            size_bytes: backup.size_bytes,
            live_size_bytes: None,
            file_count: backup.file_count,
            s3_source_id: backup.s3_source_id,
            s3_location: backup.s3_location,
            error_message: backup.error_message,
            metadata: serde_json::from_str(&backup.metadata).unwrap_or_default(),
            checksum: backup.checksum,
            compression_type: backup.compression_type,
            created_by: backup.created_by,
            expires_at: backup.expires_at.map(|dt| dt.timestamp_millis()),
            tags: serde_json::from_str(&backup.tags).unwrap_or_default(),
            last_heartbeat_at: backup.last_heartbeat_at.map(|dt| dt.timestamp_millis()),
            stalled,
        }
    }
}

#[derive(Deserialize)]
struct S3BackupIndex {
    backups: Vec<S3BackupEntry>,
    last_updated: String,
}

#[derive(Deserialize)]
struct S3BackupEntry {
    #[serde(default)]
    id: i32,
    #[serde(default)]
    backup_id: String,
    name: String,
    #[serde(rename = "type")]
    backup_type: String,
    created_at: String,
    #[serde(default)]
    size_bytes: Option<i64>,
    #[serde(default)]
    location: String,
    #[serde(default)]
    metadata_location: String,
    // Newly surfaced fields — older cached JSON may not include them, so
    // `#[serde(default)]` keeps backwards compat with any legacy index.json.
    #[serde(default)]
    engine: Option<String>,
    #[serde(default)]
    origin_service_name: Option<String>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default = "default_backup_source")]
    source: String,
    #[serde(default)]
    state: String,
}

fn default_backup_source() -> String {
    "db".to_string()
}

/// List all backups in an S3 source
#[utoipa::path(
    tag = "Backups",
    get,
    path = "/backups/s3-sources/{id}/backups",
    responses(
        (status = 200, description = "List of all backups in the source", body = SourceBackupIndexResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 404, description = "S3 source not found"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn list_source_backups(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsRead);

    let index = app_state
        .backup_service
        .list_source_backups(id)
        .await
        .map_err(Problem::from)?;

    let s3_index: S3BackupIndex = serde_json::from_value(index).map_err(|e| {
        error!("Failed to parse backup index: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let response = SourceBackupIndexResponse {
        backups: s3_index
            .backups
            .into_iter()
            .map(|entry| SourceBackupEntry {
                id: entry.id,
                backup_id: entry.backup_id,
                name: entry.name,
                backup_type: entry.backup_type,
                created_at: entry.created_at,
                size_bytes: entry.size_bytes,
                location: entry.location,
                metadata_location: entry.metadata_location,
                engine: entry.engine,
                origin_service_name: entry.origin_service_name,
                format: entry.format,
                source: entry.source,
                state: entry.state,
            })
            .collect(),
        last_updated: s3_index.last_updated,
    };
    Ok(Json(response))
}

pub fn configure_routes() -> Router<Arc<BackupAppState>> {
    Router::new()
        .route(
            "/backups/s3-sources",
            get(list_s3_sources).post(create_s3_source),
        )
        .route(
            "/backups/s3-sources/{id}",
            get(get_s3_source)
                .patch(update_s3_source)
                .delete(delete_s3_source),
        )
        .route(
            "/backups/s3-sources/{id}/set-default",
            post(set_default_s3_source),
        )
        .route(
            "/backups/s3-sources/{id}/test",
            post(test_s3_source_connection),
        )
        .route("/backups/s3-sources/test", post(test_s3_connection_preview))
        .route("/backups/s3-sources/{id}/run", post(run_backup_for_source))
        .route(
            "/backups/schedules",
            get(list_backup_schedules).post(create_backup_schedule),
        )
        .route(
            "/backups/schedules/{id}",
            get(get_backup_schedule).delete(delete_backup_schedule),
        )
        .route(
            "/backups/schedules/{id}/backups",
            get(list_backups_for_schedule),
        )
        .route("/backups/s3-sources/{id}/backups", get(list_source_backups))
        .route("/backups/{id}", get(get_backup))
        .route(
            "/backups/schedules/{id}/disable",
            patch(disable_backup_schedule),
        )
        .route(
            "/backups/schedules/{id}/enable",
            patch(enable_backup_schedule),
        )
        .route(
            "/backups/external-services/{id}/run",
            post(run_external_service_backup),
        )
}

/// List all S3 sources
#[utoipa::path(
    tag = "Backups",
    get,
    path = "/backups/s3-sources",
    responses(
        (status = 200, description = "List of S3 sources", body = Vec<S3SourceResponse>),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn list_s3_sources(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsRead);
    let sources = app_state
        .backup_service
        .list_s3_sources()
        .await
        .map_err(Problem::from)?;

    let responses: Vec<S3SourceResponse> = sources.into_iter().map(Into::into).collect();
    Ok(Json(responses))
}

/// Create a new S3 source
#[utoipa::path(
    tag = "Backups",
    post,
    path = "/backups/s3-sources",
    request_body = CreateS3SourceRequest,
    responses(
        (status = 201, description = "S3 source created", body = S3SourceResponse),
        (status = 400, description = "Invalid request", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn create_s3_source(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<CreateS3SourceRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsCreate);
    let source = app_state
        .backup_service
        .create_s3_source(request.clone())
        .await
        .map_err(Problem::from)?;

    // Create audit log
    let audit = S3SourceCreatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        source_id: source.id,
        name: source.name.clone(),
        bucket_name: source.bucket_name.clone(),
    };

    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok((StatusCode::CREATED, Json(S3SourceResponse::from(source))))
}

/// Get an S3 source by ID
#[utoipa::path(
    tag = "Backups",
    get,
    path = "/backups/s3-sources/{id}",
    responses(
        (status = 200, description = "S3 source details", body = S3SourceResponse),
        (status = 404, description = "S3 source not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn get_s3_source(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsRead);
    let source = app_state.backup_service.get_s3_source(id).await?;

    Ok(Json(S3SourceResponse::from(source)))
}

/// Update an S3 source
#[utoipa::path(
    tag = "Backups",
    patch,
    path = "/backups/s3-sources/{id}",
    request_body = UpdateS3SourceRequest,
    responses(
        (status = 200, description = "S3 source updated", body = S3SourceResponse),
        (status = 404, description = "S3 source not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn update_s3_source(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<UpdateS3SourceRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsWrite);
    let source = app_state
        .backup_service
        .update_s3_source(id, request.clone())
        .await
        .map_err(Problem::from)?;

    // Create audit log with changed fields
    let mut updated_fields = HashMap::new();
    if request.name.is_some() {
        updated_fields.insert("name".to_string(), "updated".to_string());
    }
    if request.bucket_name.is_some() {
        updated_fields.insert("bucket_name".to_string(), "updated".to_string());
    }
    if request.bucket_path.is_some() {
        updated_fields.insert("bucket_path".to_string(), "updated".to_string());
    }
    if request.access_key_id.is_some() {
        updated_fields.insert("access_key_id".to_string(), "updated".to_string());
    }
    if request.secret_key.is_some() {
        updated_fields.insert("secret_key".to_string(), "updated".to_string());
    }
    if request.region.is_some() {
        updated_fields.insert("region".to_string(), "updated".to_string());
    }

    let audit = S3SourceUpdatedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        source_id: source.id,
        name: source.name.clone(),
        bucket_name: source.bucket_name.clone(),
        updated_fields,
    };

    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(Json(S3SourceResponse::from(source)))
}

/// Delete an S3 source
#[utoipa::path(
    tag = "Backups",
    delete,
    path = "/backups/s3-sources/{id}",
    responses(
        (status = 204, description = "S3 source deleted"),
        (status = 404, description = "S3 source not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn delete_s3_source(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsDelete);
    // Get source details before deletion for audit log
    let source = app_state.backup_service.get_s3_source(id).await?;

    app_state.backup_service.delete_s3_source(id).await?;

    let audit = S3SourceDeletedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        source_id: source.id,
        name: source.name,
        bucket_name: source.bucket_name,
    };

    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(StatusCode::NO_CONTENT)
}

/// Mark an S3 source as the default. All new backups/schedules/services that do not
/// explicitly reference a source will use the default. Returns the updated source.
#[utoipa::path(
    tag = "Backups",
    post,
    path = "/backups/s3-sources/{id}/set-default",
    responses(
        (status = 200, description = "S3 source marked as default", body = S3SourceResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 404, description = "S3 source not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn set_default_s3_source(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsWrite);

    let source = app_state
        .backup_service
        .set_default_s3_source(id)
        .await
        .map_err(Problem::from)?;

    Ok(Json(S3SourceResponse::from(source)))
}

/// Test connectivity to an existing S3 source using its stored credentials.
#[utoipa::path(
    tag = "Backups",
    post,
    path = "/backups/s3-sources/{id}/test",
    responses(
        (status = 200, description = "Connection test result", body = S3ConnectionTestResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 404, description = "S3 source not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn test_s3_source_connection(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsRead);

    match app_state.backup_service.test_s3_source_connection(id).await {
        Ok(()) => Ok(Json(S3ConnectionTestResponse {
            ok: true,
            message: "Connection successful".to_string(),
        })),
        Err(BackupError::NotFound { .. }) => Err(Problem::from(BackupError::NotFound {
            resource: "S3Source".to_string(),
            detail: format!("S3 source {} not found", id),
        })),
        Err(e) => Ok(Json(S3ConnectionTestResponse {
            ok: false,
            message: e.to_string(),
        })),
    }
}

/// Test S3 connectivity against a prospective source configuration (before creating it).
/// The credentials are NOT persisted. Useful for validating the form in the UI.
#[utoipa::path(
    tag = "Backups",
    post,
    path = "/backups/s3-sources/test",
    request_body = CreateS3SourceRequest,
    responses(
        (status = 200, description = "Connection test result", body = S3ConnectionTestResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 400, description = "Invalid request", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn test_s3_connection_preview(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Json(request): Json<CreateS3SourceRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsCreate);

    match app_state
        .backup_service
        .test_s3_connection_from_request(&request)
        .await
    {
        Ok(()) => Ok(Json(S3ConnectionTestResponse {
            ok: true,
            message: "Credentials valid and bucket reachable".to_string(),
        })),
        Err(BackupError::Validation(msg)) => Err(Problem::from(BackupError::Validation(msg))),
        Err(e) => Ok(Json(S3ConnectionTestResponse {
            ok: false,
            message: e.to_string(),
        })),
    }
}

/// List all backup schedules
#[utoipa::path(
    tag = "Backups",
    get,
    path = "/backups/schedules",
    responses(
        (status = 200, description = "List of backup schedules", body = Vec<BackupScheduleResponse>),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn list_backup_schedules(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsRead);
    let schedules = app_state.backup_service.list_backup_schedules().await?;

    let responses: Vec<BackupScheduleResponse> = schedules.into_iter().map(Into::into).collect();
    Ok(Json(responses))
}

/// Create a new backup schedule
#[utoipa::path(
    tag = "Backups",
    post,
    path = "/backups/schedules",
    request_body = CreateBackupScheduleRequest,
    responses(
        (status = 201, description = "Backup schedule created", body = BackupScheduleResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn create_backup_schedule(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Json(request): Json<CreateBackupScheduleRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsCreate);

    match app_state
        .backup_service
        .create_backup_schedule(request)
        .await
    {
        Ok(schedule) => Ok((
            StatusCode::CREATED,
            Json(BackupScheduleResponse::from(schedule)),
        )),
        Err(e) => {
            error!("Failed to create backup schedule: {}", e);
            Err(Problem::from(e))
        }
    }
}

/// Get a backup schedule by ID
#[utoipa::path(
    tag = "Backups",
    get,
    path = "/backups/schedules/{id}",
    responses(
        (status = 200, description = "Backup schedule details", body = BackupScheduleResponse),
        (status = 404, description = "Backup schedule not found"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn get_backup_schedule(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsRead);
    let schedule = app_state.backup_service.get_backup_schedule(id).await?;
    Ok(Json(BackupScheduleResponse::from(schedule)))
}

/// Delete a backup schedule
#[utoipa::path(
    tag = "Backups",
    delete,
    path = "/backups/schedules/{id}",
    responses(
        (status = 204, description = "Backup schedule deleted"),
        (status = 404, description = "Backup schedule not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn delete_backup_schedule(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, temps_core::problemdetails::Problem> {
    permission_guard!(auth, BackupsDelete);
    let result = app_state.backup_service.delete_backup_schedule(id).await?;
    if result {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(temps_core::error_builder::not_found()
            .title("Backup schedule not found")
            .build())
    }
}

/// List backups for a schedule
#[utoipa::path(
    tag = "Backups",
    get,
    path = "/backups/schedules/{id}/backups",
    responses(
        (status = 200, description = "List of backups for the schedule", body = Vec<BackupResponse>),
        (status = 404, description = "Backup schedule not found"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn list_backups_for_schedule(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsRead);
    let backups = app_state
        .backup_service
        .list_backups_for_schedule(id)
        .await?;
    let responses: Vec<BackupResponse> = backups.into_iter().map(Into::into).collect();
    Ok(Json(responses))
}

/// Run a backup immediately for an S3 source
#[utoipa::path(
    tag = "Backups",
    post,
    path = "/backups/s3-sources/{id}/run",
    request_body = RunBackupRequest,
    responses(
        (status = 200, description = "Backup started successfully", body = BackupResponse),
        (status = 400, description = "Invalid request", body = ProblemDetails),
        (status = 404, description = "S3 source not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn run_backup_for_source(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<RunBackupRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsCreate);

    let backup = app_state
        .backup_service
        .run_backup_for_source(id, &request.backup_type, auth.user_id())
        .await
        .map_err(|e| {
            error!("Failed to run backup for S3 source {}: {}", id, e);
            Problem::from(e)
        })?;

    let audit = BackupRunAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        source_id: backup.id,
        source_name: backup.name.clone(),
        backup_id: backup.backup_id.clone(),
        backup_type: request.backup_type,
    };

    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(Json(BackupResponse::from(backup)))
}

/// Get a backup by ID
#[utoipa::path(
    tag = "Backups",
    get,
    path = "/backups/{id}",
    responses(
        (status = 200, description = "Backup details", body = BackupResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 404, description = "Backup not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn get_backup(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsRead);

    let Some(backup) = app_state.backup_service.get_backup(&id).await? else {
        return Err(temps_core::error_builder::not_found()
            .title("Backup Not Found")
            .detail(format!("Backup with ID {} not found", id))
            .build());
    };

    // Compute partial size while the backup is still running. Best-effort
    // and capped to one S3 list call per request — `compute_live_size`
    // returns None for finished or unresolvable backups.
    let live_size = app_state.backup_service.compute_live_size(&backup).await;
    let mut response = BackupResponse::from(backup);
    response.live_size_bytes = live_size;
    Ok(Json(response))
}

/// Disable a backup schedule
#[utoipa::path(
    tag = "Backups",
    patch,
    path = "/backups/schedules/{id}/disable",
    responses(
        (status = 200, description = "Backup schedule disabled", body = BackupScheduleResponse),
        (status = 404, description = "Backup schedule not found"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn disable_backup_schedule(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsWrite);
    let schedule = app_state.backup_service.disable_backup_schedule(id).await?;

    let audit = BackupScheduleStatusChangedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        schedule_id: schedule.id,
        name: schedule.name.clone(),
        new_status: "disabled".to_string(),
    };

    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(Json(BackupScheduleResponse::from(schedule)))
}

/// Enable a backup schedule
#[utoipa::path(
    tag = "Backups",
    patch,
    path = "/backups/schedules/{id}/enable",
    responses(
        (status = 200, description = "Backup schedule enabled", body = BackupScheduleResponse),
        (status = 404, description = "Backup schedule not found"),
        (status = 500, description = "Internal server error")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn enable_backup_schedule(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsWrite);
    let schedule = app_state.backup_service.enable_backup_schedule(id).await?;
    let audit = BackupScheduleStatusChangedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        schedule_id: schedule.id,
        name: schedule.name.clone(),
        new_status: "enabled".to_string(),
    };

    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(Json(BackupScheduleResponse::from(schedule)))
}

/// Run a backup for an external service manually
#[utoipa::path(
    tag = "Backups",
    post,
    path = "/backups/external-services/{id}/run",
    request_body = RunExternalServiceBackupRequest,
    responses(
        (status = 200, description = "Backup started successfully", body = ExternalServiceBackupResponse),
        (status = 400, description = "Invalid request", body = ProblemDetails),
        (status = 404, description = "External service or S3 source not found", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(
        ("bearer_auth" = [])
    )
)]
async fn run_external_service_backup(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<BackupAppState>>,
    Path(id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<RunExternalServiceBackupRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, BackupsCreate);

    // Get the external service
    let service = app_state
        .backup_service
        .get_external_service(id)
        .await
        .map_err(|e| {
            error!("Failed to get external service {} for backup: {}", id, e);
            Problem::from(e)
        })?;

    let backup_type = request.backup_type.as_deref().unwrap_or("full");

    let s3_source_id = app_state
        .backup_service
        .resolve_s3_source_id(request.s3_source_id)
        .await
        .map_err(Problem::from)?;

    // Run the backup
    let backup = app_state
        .backup_service
        .backup_external_service(&service, s3_source_id, backup_type, auth.user_id())
        .await
        .map_err(|e| {
            error!(
                "Failed to backup external service {} ({}): {}",
                service.name, service.service_type, e
            );
            Problem::from(e)
        })?;

    // Create audit log
    let audit = ExternalServiceBackupRunAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        service_id: service.id,
        service_name: service.name.clone(),
        service_type: service.service_type.clone(),
        backup_id: backup.id,
        backup_type: backup_type.to_string(),
    };

    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log: {}", e);
    }

    Ok(Json(ExternalServiceBackupResponse::from(backup)))
}
