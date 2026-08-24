//! HTTP handlers for PostgreSQL major-version upgrades.
//!
//! Upgrades are a child resource of an external service, so they live under
//! `/external-services/{service_id}/upgrades/...`. Every handler that takes
//! an upgrade id also validates the upgrade actually belongs to the service
//! in the path — a stray id from another service surfaces as 404 rather than
//! silently leaking cross-service state.
//!
//! Routes:
//!   POST   /external-services/{service_id}/upgrades              start a new upgrade
//!   GET    /external-services/{service_id}/upgrades              list upgrades for a service
//!   GET    /external-services/{service_id}/upgrades/{id}         get a single upgrade
//!   POST   /external-services/{service_id}/upgrades/{id}/retry   retry a failed or cancelled upgrade
//!   POST   /external-services/{service_id}/upgrades/{id}/cancel  request cancellation of a running upgrade
//!   POST   /external-services/{service_id}/upgrades/{id}/rollback  roll back a completed upgrade to its pre-upgrade PGDATA volume
//!   GET    /external-services/{service_id}/upgrades/{id}/logs    get accumulated log content

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, require_sensitive_action, RequireAuth};
use temps_core::problemdetails::Problem;
use temps_core::SensitiveAction;
use temps_entities::postgres_major_upgrades;
use temps_providers::externalsvc::postgres_upgrade::PostgresUpgradeError;
use temps_providers::postgres_upgrade_service::StartMajorUpgradeRequest;
use utoipa::{OpenApi, ToSchema};

use crate::handlers::types::BackupAppState;

// ---- DTOs --------------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct StartPgUpgradeRequest {
    #[schema(example = "16")]
    pub from_version: String,
    #[schema(example = "17")]
    pub to_version: String,
    #[schema(example = "postgres:16-bookworm")]
    pub from_image: String,
    #[schema(example = "postgres:17-bookworm")]
    pub to_image: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PgUpgradeResponse {
    pub id: i32,
    pub service_id: i32,
    pub from_version: String,
    pub to_version: String,
    pub from_image: String,
    pub to_image: String,
    pub status: String,
    pub phase: String,
    pub log_id: String,
    pub pre_upgrade_backup_id: Option<i32>,
    pub rollback_volume_name: Option<String>,
    pub error_message: Option<String>,
    pub attempt: i32,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub created_at: String,
}

impl From<postgres_major_upgrades::Model> for PgUpgradeResponse {
    fn from(m: postgres_major_upgrades::Model) -> Self {
        Self {
            id: m.id,
            service_id: m.service_id,
            from_version: m.from_version,
            to_version: m.to_version,
            from_image: m.from_image,
            to_image: m.to_image,
            status: m.status,
            phase: m.phase,
            log_id: m.log_id,
            pre_upgrade_backup_id: m.pre_upgrade_backup_id,
            rollback_volume_name: m.rollback_volume_name,
            error_message: m.error_message,
            attempt: m.attempt,
            started_at: m.started_at.map(|d| d.to_rfc3339()),
            finished_at: m.finished_at.map(|d| d.to_rfc3339()),
            created_at: m.created_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PgUpgradeLogResponse {
    pub log_id: String,
    pub content: String,
}

// ---- Helpers -----------------------------------------------------------

/// Load an upgrade row and verify it actually belongs to the service named
/// in the URL. A mismatch is treated as 404 so cross-service ids don't leak
/// existence information.
async fn load_upgrade_for_service(
    state: &BackupAppState,
    service_id: i32,
    upgrade_id: i32,
) -> Result<postgres_major_upgrades::Model, PostgresUpgradeError> {
    let row = postgres_major_upgrades::Entity::find_by_id(upgrade_id)
        .one(state.db.as_ref())
        .await
        .map_err(PostgresUpgradeError::Database)?
        .ok_or(PostgresUpgradeError::NotFound { upgrade_id })?;

    if row.service_id != service_id {
        return Err(PostgresUpgradeError::NotFound { upgrade_id });
    }

    Ok(row)
}

// ---- Handlers ----------------------------------------------------------

/// Start a new PostgreSQL major-version upgrade for a service.
#[utoipa::path(
    post,
    path = "/external-services/{service_id}/upgrades",
    tag = "Postgres Upgrades",
    params(("service_id" = i32, Path, description = "External service id")),
    request_body = StartPgUpgradeRequest,
    responses(
        (status = 201, description = "Upgrade started", body = PgUpgradeResponse),
        (status = 400, description = "Invalid request"),
        (status = 409, description = "An upgrade is already running for this service"),
        (status = 412, description = "No default S3 source configured"),
        (status = 500, description = "Internal error")
    ),
    security(("bearer_auth" = []))
)]
async fn start_pg_upgrade(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<BackupAppState>>,
    Path(service_id): Path<i32>,
    Json(req): Json<StartPgUpgradeRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);

    let inserted = state
        .pg_upgrade_service
        .start_major_upgrade(StartMajorUpgradeRequest {
            service_id,
            from_version: req.from_version,
            to_version: req.to_version,
            from_image: req.from_image,
            to_image: req.to_image,
            created_by: auth.user_id(),
        })
        .await?;

    Ok((StatusCode::CREATED, Json(PgUpgradeResponse::from(inserted))))
}

/// List recent upgrades for a single service (newest first, page size 50).
#[utoipa::path(
    get,
    path = "/external-services/{service_id}/upgrades",
    tag = "Postgres Upgrades",
    params(("service_id" = i32, Path, description = "External service id")),
    responses(
        (status = 200, description = "Recent upgrades", body = Vec<PgUpgradeResponse>),
        (status = 500, description = "Internal error")
    ),
    security(("bearer_auth" = []))
)]
async fn list_pg_upgrades(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<BackupAppState>>,
    Path(service_id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);

    let rows = postgres_major_upgrades::Entity::find()
        .filter(postgres_major_upgrades::Column::ServiceId.eq(service_id))
        .order_by_desc(postgres_major_upgrades::Column::CreatedAt)
        .paginate(state.db.as_ref(), 50)
        .fetch_page(0)
        .await
        .map_err(PostgresUpgradeError::Database)?;

    let resp: Vec<PgUpgradeResponse> = rows.into_iter().map(PgUpgradeResponse::from).collect();
    Ok((StatusCode::OK, Json(resp)))
}

/// Get a single upgrade by id, scoped to a service.
#[utoipa::path(
    get,
    path = "/external-services/{service_id}/upgrades/{id}",
    tag = "Postgres Upgrades",
    params(
        ("service_id" = i32, Path, description = "External service id"),
        ("id" = i32, Path, description = "Upgrade id"),
    ),
    responses(
        (status = 200, description = "Upgrade", body = PgUpgradeResponse),
        (status = 404, description = "Not found"),
        (status = 500, description = "Internal error")
    ),
    security(("bearer_auth" = []))
)]
async fn get_pg_upgrade(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<BackupAppState>>,
    Path((service_id, id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);

    let row = load_upgrade_for_service(&state, service_id, id).await?;
    Ok((StatusCode::OK, Json(PgUpgradeResponse::from(row))))
}

/// Retry a failed upgrade. The phase is preserved, so the state machine
/// resumes from where it failed.
#[utoipa::path(
    post,
    path = "/external-services/{service_id}/upgrades/{id}/retry",
    tag = "Postgres Upgrades",
    params(
        ("service_id" = i32, Path, description = "External service id"),
        ("id" = i32, Path, description = "Upgrade id"),
    ),
    responses(
        (status = 200, description = "Retry scheduled", body = PgUpgradeResponse),
        (status = 400, description = "Upgrade is not in a retriable state"),
        (status = 404, description = "Not found"),
        (status = 500, description = "Internal error")
    ),
    security(("bearer_auth" = []))
)]
async fn retry_pg_upgrade(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<BackupAppState>>,
    Path((service_id, id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);

    // Ownership check before mutating — keeps the service method oblivious
    // to URL structure and prevents retrying another service's upgrade.
    let _ = load_upgrade_for_service(&state, service_id, id).await?;

    let updated = state.pg_upgrade_service.retry_major_upgrade(id).await?;
    Ok((StatusCode::OK, Json(PgUpgradeResponse::from(updated))))
}

/// Cancel an in-flight upgrade. The orchestrator stops at its next phase
/// boundary; already-terminal upgrades return 409.
#[utoipa::path(
    post,
    path = "/external-services/{service_id}/upgrades/{id}/cancel",
    tag = "Postgres Upgrades",
    params(
        ("service_id" = i32, Path, description = "External service id"),
        ("id" = i32, Path, description = "Upgrade id"),
    ),
    responses(
        (status = 200, description = "Cancellation requested", body = PgUpgradeResponse),
        (status = 404, description = "Not found"),
        (status = 409, description = "Upgrade already terminal"),
        (status = 500, description = "Internal error")
    ),
    security(("bearer_auth" = []))
)]
async fn cancel_pg_upgrade(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<BackupAppState>>,
    Path((service_id, id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);

    // Ownership check before mutating.
    let _ = load_upgrade_for_service(&state, service_id, id).await?;

    let updated = state.pg_upgrade_service.cancel_major_upgrade(id).await?;
    Ok((StatusCode::OK, Json(PgUpgradeResponse::from(updated))))
}

/// Roll a completed upgrade back to its pre-upgrade PGDATA volume and old image.
/// Only valid while the rollback retention window is still open (see
/// `ROLLBACK_RETENTION_DAYS`) and the rollback volume has not been swept.
#[utoipa::path(
    post,
    path = "/external-services/{service_id}/upgrades/{id}/rollback",
    tag = "Postgres Upgrades",
    params(
        ("service_id" = i32, Path, description = "External service id"),
        ("id" = i32, Path, description = "Upgrade id"),
    ),
    responses(
        (status = 200, description = "Rollback complete", body = PgUpgradeResponse),
        (status = 404, description = "Not found"),
        (status = 409, description = "Upgrade is not in a rollbackable state (not completed, volume swept, or retention expired)"),
        (status = 500, description = "Internal error")
    ),
    security(("bearer_auth" = []))
)]
async fn rollback_pg_upgrade(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<BackupAppState>>,
    Path((service_id, id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);
    require_sensitive_action(
        state.sensitive_action_authorizer.as_ref(),
        &auth,
        SensitiveAction::RollbackPgUpgrade {
            service_id,
            upgrade_id: id,
        },
    )
    .await?;

    let _ = load_upgrade_for_service(&state, service_id, id).await?;

    let updated = state.pg_upgrade_service.rollback_major_upgrade(id).await?;
    Ok((StatusCode::OK, Json(PgUpgradeResponse::from(updated))))
}

/// Get the accumulated JSONL log content for an upgrade (for dashboard display).
#[utoipa::path(
    get,
    path = "/external-services/{service_id}/upgrades/{id}/logs",
    tag = "Postgres Upgrades",
    params(
        ("service_id" = i32, Path, description = "External service id"),
        ("id" = i32, Path, description = "Upgrade id"),
    ),
    responses(
        (status = 200, description = "Log content", body = PgUpgradeLogResponse),
        (status = 404, description = "Not found"),
        (status = 500, description = "Internal error")
    ),
    security(("bearer_auth" = []))
)]
async fn get_pg_upgrade_logs(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<BackupAppState>>,
    Path((service_id, id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);

    let row = load_upgrade_for_service(&state, service_id, id).await?;

    let log_id = row.log_id.clone();
    let content = state
        .pg_upgrade_service
        .read_log(&log_id)
        .await
        .unwrap_or_default();

    Ok((
        StatusCode::OK,
        Json(PgUpgradeLogResponse { log_id, content }),
    ))
}

// ---- Router + OpenAPI --------------------------------------------------

pub fn configure_routes() -> Router<Arc<BackupAppState>> {
    Router::new()
        .route(
            "/external-services/{service_id}/upgrades",
            post(start_pg_upgrade).get(list_pg_upgrades),
        )
        .route(
            "/external-services/{service_id}/upgrades/{id}",
            get(get_pg_upgrade),
        )
        .route(
            "/external-services/{service_id}/upgrades/{id}/retry",
            post(retry_pg_upgrade),
        )
        .route(
            "/external-services/{service_id}/upgrades/{id}/cancel",
            post(cancel_pg_upgrade),
        )
        .route(
            "/external-services/{service_id}/upgrades/{id}/rollback",
            post(rollback_pg_upgrade),
        )
        .route(
            "/external-services/{service_id}/upgrades/{id}/logs",
            get(get_pg_upgrade_logs),
        )
}

#[derive(OpenApi)]
#[openapi(
    paths(
        start_pg_upgrade,
        list_pg_upgrades,
        get_pg_upgrade,
        retry_pg_upgrade,
        cancel_pg_upgrade,
        rollback_pg_upgrade,
        get_pg_upgrade_logs
    ),
    components(schemas(StartPgUpgradeRequest, PgUpgradeResponse, PgUpgradeLogResponse))
)]
pub struct PgUpgradeApiDoc;
