// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
    extract::{Extension, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, require_sensitive_action, Permission, RequireAuth};
use temps_core::problemdetails::{Problem, ProblemDetails};
use temps_core::{RequestMetadata, SensitiveAction};
use temps_entities::postgres_major_upgrades;
use temps_providers::postgres_upgrade_service::StartMajorUpgradeRequest;
use tracing::error;
use utoipa::{OpenApi, ToSchema};

use crate::handlers::audit::{AuditContext, PgUpgradeAuditAction, PgUpgradeMutationAudit};
use crate::handlers::authz::require_service_access;
use crate::handlers::types::BackupAppState;

async fn audit_pg_upgrade_mutation(
    state: &BackupAppState,
    auth: &temps_auth::AuthContext,
    metadata: &RequestMetadata,
    row: &postgres_major_upgrades::Model,
    action: PgUpgradeAuditAction,
) {
    // The authorization lookup already establishes ownership for non-admin
    // callers. Resolve the full project set separately for the durable audit
    // record because admins bypass that lookup and services may be linked to
    // multiple projects. Audit enrichment/logging failures must not turn a
    // successful control-plane mutation into a failed HTTP response.
    let project_ids = match state
        .backup_service
        .project_scopes_for_services(&[row.service_id])
        .await
    {
        Ok(scopes) => scopes
            .into_iter()
            .flat_map(|scope| scope.project_ids)
            .collect(),
        Err(err) => {
            error!(
                service_id = row.service_id,
                upgrade_id = row.id,
                error = %err,
                "failed to resolve project identity for PostgreSQL upgrade audit"
            );
            Vec::new()
        }
    };

    let audit = PgUpgradeMutationAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        action,
        project_ids,
        service_id: row.service_id,
        upgrade_id: row.id,
        from_version: row.from_version.clone(),
        to_version: row.to_version.clone(),
        status: row.status.clone(),
        phase: row.phase.clone(),
        attempt: row.attempt,
    };

    if let Err(err) = state.audit_service.create_audit_log(&audit).await {
        error!(
            service_id = row.service_id,
            upgrade_id = row.id,
            action = ?action,
            error = %err,
            "failed to create PostgreSQL upgrade audit log"
        );
    }
}

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

impl From<temps_entities::postgres_major_upgrades::Model> for PgUpgradeResponse {
    fn from(m: temps_entities::postgres_major_upgrades::Model) -> Self {
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
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 409, description = "An upgrade is already running for this service"),
        (status = 412, description = "No default S3 source configured"),
        (status = 500, description = "Internal error")
    ),
    security(("bearer_auth" = []))
)]
async fn start_pg_upgrade(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<BackupAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(service_id): Path<i32>,
    Json(req): Json<StartPgUpgradeRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);
    require_service_access(
        &state,
        &auth,
        service_id,
        Permission::ExternalServicesWrite,
        "external service",
        "PostgreSQL upgrade",
    )
    .await?;

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

    audit_pg_upgrade_mutation(
        &state,
        &auth,
        &metadata,
        &inserted,
        PgUpgradeAuditAction::Started,
    )
    .await;

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
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
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
    require_service_access(
        &state,
        &auth,
        service_id,
        Permission::ExternalServicesRead,
        "external service",
        "PostgreSQL upgrade",
    )
    .await?;

    let rows = state
        .pg_upgrade_service
        .list_upgrades_for_service(service_id)
        .await?;

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
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
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
    require_service_access(
        &state,
        &auth,
        service_id,
        Permission::ExternalServicesRead,
        "external service",
        "PostgreSQL upgrade",
    )
    .await?;

    let row = state
        .pg_upgrade_service
        .get_upgrade_for_service(service_id, id)
        .await?;
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
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Not found"),
        (status = 500, description = "Internal error")
    ),
    security(("bearer_auth" = []))
)]
async fn retry_pg_upgrade(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<BackupAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((service_id, id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);
    require_service_access(
        &state,
        &auth,
        service_id,
        Permission::ExternalServicesWrite,
        "external service",
        "PostgreSQL upgrade",
    )
    .await?;

    // Ownership check before mutating — keeps the service method oblivious
    // to URL structure and prevents retrying another service's upgrade.
    let _ = state
        .pg_upgrade_service
        .get_upgrade_for_service(service_id, id)
        .await?;

    let updated = state.pg_upgrade_service.retry_major_upgrade(id).await?;
    audit_pg_upgrade_mutation(
        &state,
        &auth,
        &metadata,
        &updated,
        PgUpgradeAuditAction::Retried,
    )
    .await;
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
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Not found"),
        (status = 409, description = "Upgrade already terminal"),
        (status = 500, description = "Internal error")
    ),
    security(("bearer_auth" = []))
)]
async fn cancel_pg_upgrade(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<BackupAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((service_id, id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);
    require_service_access(
        &state,
        &auth,
        service_id,
        Permission::ExternalServicesWrite,
        "external service",
        "PostgreSQL upgrade",
    )
    .await?;

    // Ownership check before mutating.
    let _ = state
        .pg_upgrade_service
        .get_upgrade_for_service(service_id, id)
        .await?;

    let updated = state.pg_upgrade_service.cancel_major_upgrade(id).await?;
    audit_pg_upgrade_mutation(
        &state,
        &auth,
        &metadata,
        &updated,
        PgUpgradeAuditAction::CancellationRequested,
    )
    .await;
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
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "Not found"),
        (status = 409, description = "Upgrade is not in a rollbackable state (not completed, volume swept, or retention expired)"),
        (status = 500, description = "Internal error")
    ),
    security(("bearer_auth" = []))
)]
async fn rollback_pg_upgrade(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<BackupAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path((service_id, id)): Path<(i32, i32)>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);
    require_service_access(
        &state,
        &auth,
        service_id,
        Permission::ExternalServicesWrite,
        "external service",
        "PostgreSQL upgrade",
    )
    .await?;
    require_sensitive_action(
        state.sensitive_action_authorizer.as_ref(),
        &auth,
        SensitiveAction::RollbackPgUpgrade {
            service_id,
            upgrade_id: id,
        },
    )
    .await?;

    let _ = state
        .pg_upgrade_service
        .get_upgrade_for_service(service_id, id)
        .await?;

    let updated = state.pg_upgrade_service.rollback_major_upgrade(id).await?;
    audit_pg_upgrade_mutation(
        &state,
        &auth,
        &metadata,
        &updated,
        PgUpgradeAuditAction::RolledBack,
    )
    .await;
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
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
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
    require_service_access(
        &state,
        &auth,
        service_id,
        Permission::ExternalServicesRead,
        "external service",
        "PostgreSQL upgrade",
    )
    .await?;

    let row = state
        .pg_upgrade_service
        .get_upgrade_for_service(service_id, id)
        .await?;

    let log_id = row.log_id.clone();
    let content = state.pg_upgrade_service.read_log(id, &log_id).await?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use axum::http::HeaderMap;
    use bollard::Docker;
    use sea_orm::{DatabaseBackend, DatabaseConnection, MockDatabase};
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;
    use temps_auth::Role;
    use temps_backup_core::BackupExecutorBuilder;
    use temps_core::notifications::{
        EmailMessage, NotificationData, NotificationError, NotificationService,
    };
    use temps_core::{
        AuditLogger, AuditOperation, ProjectAccessChecker, SensitiveActionAuthorizationError,
        SensitiveActionAuthorizer, SensitiveActionDecision, SensitiveActionPrincipal,
    };
    use temps_entities::{postgres_major_upgrades, project_services, users};
    use temps_logs::LogService;
    use temps_providers::externalsvc::postgres_upgrade::{
        PostgresConnection, PostgresContainerLifecycle, PreUpgradeBackupProvider,
    };
    use temps_providers::postgres_upgrade_service::PostgresUpgradeService;

    use crate::services::{BackupService, RestoreService};

    struct NoopNotifications;

    #[async_trait]
    impl NotificationService for NoopNotifications {
        async fn send_email(&self, _message: EmailMessage) -> Result<(), NotificationError> {
            Ok(())
        }

        async fn send_notification(
            &self,
            _notification: NotificationData,
        ) -> Result<(), NotificationError> {
            Ok(())
        }

        async fn is_configured(&self) -> Result<bool, NotificationError> {
            Ok(false)
        }
    }

    struct NoopJobQueue;

    #[async_trait]
    impl temps_core::JobQueue for NoopJobQueue {
        async fn send(&self, _job: temps_core::Job) -> Result<(), temps_core::QueueError> {
            Ok(())
        }

        fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
            unimplemented!("NoopJobQueue does not support subscribing in tests")
        }
    }

    #[derive(Default)]
    struct RecordingAuditLogger {
        operations: Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl AuditLogger for RecordingAuditLogger {
        async fn create_audit_log(&self, operation: &dyn AuditOperation) -> anyhow::Result<()> {
            self.operations
                .lock()
                .expect("recording audit mutex should not be poisoned")
                .push((operation.operation_type(), operation.serialize()?));
            Ok(())
        }
    }

    struct FailingAuditLogger {
        called: Arc<AtomicBool>,
    }

    #[async_trait]
    impl AuditLogger for FailingAuditLogger {
        async fn create_audit_log(&self, _operation: &dyn AuditOperation) -> anyhow::Result<()> {
            self.called.store(true, Ordering::SeqCst);
            Err(anyhow::anyhow!("audit store unavailable"))
        }
    }

    struct DenyProjectAccess;

    #[async_trait]
    impl ProjectAccessChecker for DenyProjectAccess {
        async fn user_can_access_project(
            &self,
            _user_id: i32,
            _project_id: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            Ok(false)
        }

        async fn effective_project_permissions(
            &self,
            _user_id: i32,
            _project_id: i32,
        ) -> Result<Option<Vec<String>>, Box<dyn std::error::Error + Send + Sync>> {
            Ok(Some(Vec::new()))
        }

        async fn effective_project_permissions_batch(
            &self,
            _user_id: i32,
            project_ids: &[i32],
        ) -> Result<BTreeMap<i32, Option<Vec<String>>>, Box<dyn std::error::Error + Send + Sync>>
        {
            Ok(project_ids
                .iter()
                .copied()
                .map(|project_id| (project_id, Some(Vec::new())))
                .collect())
        }
    }

    struct AllowSensitiveActions;

    #[async_trait]
    impl SensitiveActionAuthorizer for AllowSensitiveActions {
        async fn authorize(
            &self,
            _action: &SensitiveAction,
            _principal: &SensitiveActionPrincipal,
        ) -> Result<SensitiveActionDecision, SensitiveActionAuthorizationError> {
            Ok(SensitiveActionDecision::Allow)
        }
    }

    struct StubBackupProvider;

    #[async_trait]
    impl PreUpgradeBackupProvider for StubBackupProvider {
        async fn default_s3_source_id(&self, _service_id: i32) -> Result<Option<i32>, String> {
            Ok(Some(1))
        }

        async fn create_pre_upgrade_backup(
            &self,
            _service_id: i32,
            _s3_source_id: i32,
            _created_by: i32,
        ) -> Result<i32, String> {
            Ok(1)
        }
    }

    struct StubLifecycle;

    #[async_trait]
    impl PostgresContainerLifecycle for StubLifecycle {
        async fn container_name(&self, service_id: i32) -> Result<String, String> {
            Ok(format!("postgres-{service_id}"))
        }

        async fn connection_params(&self, _service_id: i32) -> Result<PostgresConnection, String> {
            Ok(PostgresConnection {
                username: "postgres".to_string(),
                password: "secret".to_string(),
                database: "postgres".to_string(),
                port: "5432".to_string(),
            })
        }

        async fn docker_image(&self, _service_id: i32) -> Result<String, String> {
            Ok("postgres:16-bookworm".to_string())
        }

        async fn stop_and_remove(&self, _service_id: i32) -> Result<(), String> {
            Ok(())
        }

        async fn create_and_start(&self, _service_id: i32, _image: &str) -> Result<(), String> {
            Ok(())
        }

        async fn set_docker_image(&self, _service_id: i32, _image: &str) -> Result<(), String> {
            Ok(())
        }
    }

    fn test_user_auth(role: Role) -> temps_auth::AuthContext {
        let now = chrono::Utc::now();
        temps_auth::AuthContext::new_session(
            users::Model {
                id: 42,
                name: "PG upgrade tester".to_string(),
                email: "pg-upgrade@example.com".to_string(),
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

    fn metadata() -> RequestMetadata {
        RequestMetadata {
            ip_address: "203.0.113.10".to_string(),
            user_agent: "pg-upgrade-handler-test".to_string(),
            headers: HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "http://localhost".to_string(),
            scheme: "http".to_string(),
            host: "localhost".to_string(),
            is_secure: false,
        }
    }

    fn project_link(service_id: i32, project_id: i32) -> project_services::Model {
        let now = chrono::Utc::now();
        project_services::Model {
            id: 1,
            project_id,
            service_id,
            created_at: now,
            updated_at: now,
        }
    }

    fn upgrade(id: i32, service_id: i32) -> postgres_major_upgrades::Model {
        let now = chrono::Utc::now();
        postgres_major_upgrades::Model {
            id,
            service_id,
            from_version: "16".to_string(),
            to_version: "17".to_string(),
            from_image: "postgres:16-bookworm".to_string(),
            to_image: "postgres:17-bookworm".to_string(),
            status: "failed".to_string(),
            phase: "dump".to_string(),
            pre_upgrade_backup_id: Some(1),
            log_id: format!("upgrade-{id}"),
            rollback_volume_name: None,
            rollback_volume_expires_at: None,
            error_message: Some("test failure".to_string()),
            attempt: 1,
            started_at: Some(now),
            finished_at: Some(now),
            created_by: 42,
            created_at: now,
        }
    }

    fn server_config() -> temps_config::ServerConfig {
        temps_config::ServerConfig {
            address: "127.0.0.1:3000".to_string(),
            database_url: "postgres://localhost/test".to_string(),
            tls_address: None,
            console_address: "127.0.0.1:3001".to_string(),
            console_admin_address: None,
            admin_allowed_ips: Vec::new(),
            admin_allowed_hosts: Vec::new(),
            admin_trust_forwarded_for: false,
            data_dir: std::env::temp_dir().join("temps-pg-upgrade-handler-tests"),
            auth_secret: "test-auth-secret".to_string(),
            encryption_key: "test-encryption-key".to_string(),
            api_base_url: "/api".to_string(),
            postgres_max_connections: Some(1),
            postgres_min_connections: Some(0),
            postgres_connect_timeout_secs: Some(1),
            postgres_acquire_timeout_secs: Some(1),
            postgres_idle_timeout_secs: Some(1),
            postgres_max_lifetime_secs: Some(1),
            clickhouse_url: None,
            clickhouse_database: None,
            clickhouse_user: None,
            clickhouse_password: None,
            docker_extra_networks: Vec::new(),
        }
    }

    fn test_state(
        db: Arc<DatabaseConnection>,
        audit_service: Arc<dyn AuditLogger>,
        project_access_checker: Option<Arc<dyn ProjectAccessChecker>>,
    ) -> Arc<BackupAppState> {
        let encryption = Arc::new(temps_core::EncryptionService::new_from_password(
            "pg-upgrade-handler-tests",
        ));
        let docker = Arc::new(
            Docker::connect_with_local_defaults()
                .expect("Docker client configuration should be available"),
        );
        let external_service_manager = Arc::new(temps_providers::ExternalServiceManager::new(
            db.clone(),
            encryption.clone(),
            docker.clone(),
            Arc::new(temps_providers::DnsRegistry::new(db.clone())),
        ));
        let alarm_service = Arc::new(temps_monitoring::alarm_service::AlarmService::new(
            db.clone(),
            Arc::new(NoopNotifications),
            Arc::new(NoopJobQueue),
        ));
        let backup_service = Arc::new(BackupService::new(
            db.clone(),
            external_service_manager.clone(),
            alarm_service,
            Arc::new(temps_config::ConfigService::new(
                Arc::new(server_config()),
                db.clone(),
            )),
            encryption.clone(),
        ));
        let restore_service = Arc::new(RestoreService::new(
            db.clone(),
            external_service_manager,
            encryption,
        ));
        let pg_upgrade_service = Arc::new(PostgresUpgradeService::new(
            db.clone(),
            docker,
            Arc::new(StubBackupProvider),
            Arc::new(StubLifecycle),
            Arc::new(LogService::new(
                std::env::temp_dir().join("temps-pg-upgrade-tests"),
            )),
        ));

        Arc::new(BackupAppState {
            backup_service,
            restore_service,
            audit_service,
            pg_upgrade_service,
            db: db.clone(),
            backup_executor: Arc::new(BackupExecutorBuilder::new(db).build()),
            telemetry: Arc::new(temps_core::NoopTelemetryReporter),
            project_access_checker,
            sensitive_action_authorizer: Arc::new(AllowSensitiveActions),
        })
    }

    fn cross_project_state(service_id: i32) -> Arc<BackupAppState> {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![project_link(service_id, 7)]])
                .into_connection(),
        );
        test_state(
            db,
            Arc::new(RecordingAuditLogger::default()),
            Some(Arc::new(DenyProjectAccess)),
        )
    }

    fn assert_forbidden<T>(result: Result<T, Problem>) {
        let problem = match result {
            Ok(_) => panic!("cross-project request should be denied"),
            Err(problem) => problem,
        };
        assert_eq!(problem.into_response().status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn all_pg_upgrade_handlers_reject_cross_project_access() {
        let service_id = 17;
        let upgrade_id = 31;

        assert_forbidden(
            start_pg_upgrade(
                RequireAuth(test_user_auth(Role::User)),
                State(cross_project_state(service_id)),
                Extension(metadata()),
                Path(service_id),
                Json(StartPgUpgradeRequest {
                    from_version: "16".to_string(),
                    to_version: "17".to_string(),
                    from_image: "postgres:16-bookworm".to_string(),
                    to_image: "postgres:17-bookworm".to_string(),
                }),
            )
            .await,
        );
        assert_forbidden(
            list_pg_upgrades(
                RequireAuth(test_user_auth(Role::User)),
                State(cross_project_state(service_id)),
                Path(service_id),
            )
            .await,
        );
        assert_forbidden(
            get_pg_upgrade(
                RequireAuth(test_user_auth(Role::User)),
                State(cross_project_state(service_id)),
                Path((service_id, upgrade_id)),
            )
            .await,
        );
        assert_forbidden(
            retry_pg_upgrade(
                RequireAuth(test_user_auth(Role::User)),
                State(cross_project_state(service_id)),
                Extension(metadata()),
                Path((service_id, upgrade_id)),
            )
            .await,
        );
        assert_forbidden(
            cancel_pg_upgrade(
                RequireAuth(test_user_auth(Role::User)),
                State(cross_project_state(service_id)),
                Extension(metadata()),
                Path((service_id, upgrade_id)),
            )
            .await,
        );
        assert_forbidden(
            rollback_pg_upgrade(
                RequireAuth(test_user_auth(Role::User)),
                State(cross_project_state(service_id)),
                Extension(metadata()),
                Path((service_id, upgrade_id)),
            )
            .await,
        );
        assert_forbidden(
            get_pg_upgrade_logs(
                RequireAuth(test_user_auth(Role::User)),
                State(cross_project_state(service_id)),
                Path((service_id, upgrade_id)),
            )
            .await,
        );
    }

    #[tokio::test]
    async fn upgrade_id_from_another_service_is_not_found_before_retry_mutation() {
        let requested_service_id = 17;
        let upgrade_id = 31;
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![upgrade(upgrade_id, 99)]])
                .into_connection(),
        );
        let state = test_state(db, Arc::new(RecordingAuditLogger::default()), None);

        let result = retry_pg_upgrade(
            RequireAuth(test_user_auth(Role::Admin)),
            State(state),
            Extension(metadata()),
            Path((requested_service_id, upgrade_id)),
        )
        .await;

        let problem = match result {
            Ok(_) => panic!("upgrade owned by another service must not be retried"),
            Err(problem) => problem,
        };
        assert_eq!(problem.into_response().status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn audit_enrichment_records_project_and_request_identity() {
        let row = upgrade(31, 17);
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![project_link(17, 7)]])
                .into_connection(),
        );
        let recorder = Arc::new(RecordingAuditLogger::default());
        let state = test_state(db, recorder.clone(), None);

        audit_pg_upgrade_mutation(
            &state,
            &test_user_auth(Role::Admin),
            &metadata(),
            &row,
            PgUpgradeAuditAction::Retried,
        )
        .await;

        let operations = recorder
            .operations
            .lock()
            .expect("recording audit mutex should not be poisoned");
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].0, "POSTGRES_MAJOR_UPGRADE_RETRIED");
        let payload: serde_json::Value =
            serde_json::from_str(&operations[0].1).expect("audit payload should be JSON");
        assert_eq!(payload["context"]["user_id"], 42);
        assert_eq!(payload["context"]["ip_address"], "203.0.113.10");
        assert_eq!(payload["context"]["user_agent"], "pg-upgrade-handler-test");
        assert_eq!(payload["project_ids"], serde_json::json!([7]));
        assert_eq!(payload["service_id"], 17);
        assert_eq!(payload["upgrade_id"], 31);
        assert_eq!(payload["action"], "retried");
    }

    #[tokio::test]
    async fn audit_storage_failure_does_not_fail_completed_mutation_path() {
        let row = upgrade(31, 17);
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![project_link(17, 7)]])
                .into_connection(),
        );
        let called = Arc::new(AtomicBool::new(false));
        let state = test_state(
            db,
            Arc::new(FailingAuditLogger {
                called: called.clone(),
            }),
            None,
        );

        audit_pg_upgrade_mutation(
            &state,
            &test_user_auth(Role::Admin),
            &metadata(),
            &row,
            PgUpgradeAuditAction::CancellationRequested,
        )
        .await;

        assert!(called.load(Ordering::SeqCst));
    }
}
