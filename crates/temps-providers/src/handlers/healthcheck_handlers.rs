// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The Docker healthcheck of managed service containers.
//!
//! ```text
//! GET  /external-services/healthcheck-defaults
//! PUT  /external-services/healthcheck-defaults
//! GET  /external-services/{id}/healthcheck
//! PUT  /external-services/{id}/healthcheck
//! POST /external-services/{id}/healthcheck/apply
//! ```
//!
//! The instance default is a platform setting (`SettingsRead` /
//! `SettingsWrite`); a service's overrides follow the service's own
//! permissions and project access. Saving either changes nothing running:
//! Docker cannot change a container's healthcheck, so it applies when the
//! container is next created or recreated. `apply` recreates the container
//! now, keeping its volumes and configuration. Every write is audit-logged.

use std::sync::Arc;

use axum::{
    extract::{Extension, Path, State},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use temps_auth::{permission_guard, AuthContext, RequireAuth};
use temps_core::error_builder::{bad_request, conflict, not_found};
use temps_core::problemdetails::Problem;
use temps_core::{AuditContext, RequestMetadata};
use temps_providers_core::externalsvc::recreate_guard::ManagedEngine;
use temps_providers_core::healthcheck::{
    HealthcheckOverrides, HealthcheckSource, HealthcheckSources, HealthcheckTimings,
    ResolvedHealthcheck,
};
use tracing::{error, warn};
use utoipa::{OpenApi, ToSchema};

use super::audit::{
    ExternalServiceHealthcheckAppliedAudit, ExternalServiceHealthcheckUpdatedAudit,
    ManagedServiceHealthcheckDefaultsUpdatedAudit,
};
use super::handlers::external_service_problem;
use crate::handlers::types::AppState;
use crate::services::{
    EngineHealthcheckDefault, ExternalServiceError, HealthcheckCapability, HealthcheckDefaults,
    HealthcheckDefaultsService, ServiceHealthcheck,
};

/// The handlers' dependencies: the providers' state (manager, audit, access
/// checks) and the instance-default store.
pub struct HealthcheckState {
    pub app: Arc<AppState>,
    pub defaults: Arc<HealthcheckDefaultsService>,
}

/// After `apply`: the container was recreated with these timings.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ApplyServiceHealthcheckResponse {
    pub service_id: i32,
    /// As this host's Docker got them: below API 1.44 the interval is the
    /// start interval.
    pub applied: HealthcheckTimings,
    /// The service's healthcheck as it is now. `null` when it could not be
    /// read back; the recreate still succeeded, so read it again rather than
    /// applying again.
    pub healthcheck: Option<ServiceHealthcheck>,
}

fn audit_context(auth: &AuthContext, metadata: &RequestMetadata) -> AuditContext {
    AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    }
}

/// Healthcheck errors: an override out of bounds is a 400, a service whose
/// container has none (or cannot be recreated from here) a 409. Everything
/// else takes the shared classification (404, 409 data-safety refusal, 500).
fn healthcheck_problem(error: ExternalServiceError, operation: &str) -> Problem {
    healthcheck_specific_problem(&error).unwrap_or_else(|| {
        external_service_problem(&error, format!("Failed to {operation}: {error}"))
    })
}

/// The variants these routes answer differently from the shared
/// classification. Every variant is listed, so a new one is a decision here.
fn healthcheck_specific_problem(error: &ExternalServiceError) -> Option<Problem> {
    use ExternalServiceError as E;
    let (problem, title) = match error {
        E::InvalidHealthcheck { .. } => (bad_request(), "Invalid Healthcheck"),
        E::HealthcheckUnsupported { .. } => {
            (conflict(), "Healthcheck Not Available For This Service")
        }
        E::HealthcheckDefaultConflictsWithService { .. } => (
            conflict(),
            "Healthcheck Default Conflicts With A Service Override",
        ),
        E::ServiceNotFound { .. } => (not_found(), "Service Not Found"),
        E::Engine { .. }
        | E::ServiceNameConflict { .. }
        | E::ServiceNameRestoreConflict { .. }
        | E::ServiceContainerRestoreConflict { .. }
        | E::AmbiguousServiceName { .. }
        | E::ServiceResourceConflict { .. }
        | E::ServiceContainerConflict { .. }
        | E::ServiceNotFoundByName { .. }
        | E::ServiceNotFoundBySlug { .. }
        | E::ClusterMemberNotFound { .. }
        | E::NodeNotFound { .. }
        | E::InitializationFailed { .. }
        | E::UpgradeRejected { .. }
        | E::EncryptionFailed { .. }
        | E::DecryptionFailed { .. }
        | E::InvalidServiceType { .. }
        | E::ServiceNotLinkedToProject { .. }
        | E::NoSandboxAccessEngine { .. }
        | E::SandboxAccessUnsupported { .. }
        | E::SandboxNamespaceConflict { .. }
        | E::ServiceClaimDenied { .. }
        | E::InvalidDatabaseProvisioning { .. }
        | E::ProjectNotFound { .. }
        | E::EnvironmentNotFound { .. }
        | E::DatabaseError { .. }
        | E::ArchiveSourceDesynced { .. }
        | E::ParameterValidationFailed { .. }
        | E::DataSafetyRefusal { .. }
        | E::StartFailed { .. }
        | E::UpgradeInProgress { .. }
        | E::StopFailed { .. }
        | E::ClusterMemberProvisioning { .. }
        | E::ClusterNotSettled { .. }
        | E::ClusterBusy { .. }
        | E::LockTimeout { .. }
        | E::DeletionFailed { .. }
        | E::ServiceHasLinkedProjects { .. }
        | E::EnvironmentVariableNotFound { .. }
        | E::ParameterNotFound { .. }
        | E::ParameterNotSensitive { .. }
        | E::EncryptedVariableAccessDenied { .. }
        | E::DockerError { .. }
        | E::DuplicateServiceType { .. }
        | E::InternalError { .. }
        | E::NodeBusy { .. }
        | E::AgentUnanswered { .. }
        | E::DockerUnavailable(_)
        | E::LocalWorkloadsDisabled { .. }
        | E::ControlPlaneAddressRequired { .. }
        | E::ControlPlaneMemberUnreachable { .. }
        | E::ClusterPortsUnavailable { .. }
        | E::ClusterMemberLimitExceeded { .. } => return None,
    };
    Some(problem.title(title).detail(error.to_string()).build())
}

/// Get the instance-wide default healthcheck of managed service containers
///
/// Null values use each engine's built-in timing. `engines` lists what a
/// service of each engine inherits.
#[utoipa::path(
    tag = "External Services",
    get,
    path = "/external-services/healthcheck-defaults",
    responses(
        (status = 200, description = "The instance default", body = HealthcheckDefaults),
        (status = 401, description = "Unauthorized", body = temps_core::problemdetails::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::problemdetails::ProblemDetails),
        (status = 500, description = "Internal server error", body = temps_core::problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_healthcheck_defaults(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<HealthcheckState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    let defaults = state
        .defaults
        .get()
        .await
        .map_err(|e| healthcheck_problem(e, "read the healthcheck defaults"))?;
    Ok(Json(defaults))
}

/// Replace the instance-wide default healthcheck of managed service containers
///
/// Applies to containers created or recreated afterwards; nothing running
/// changes. Send null for a value to use each engine's built-in timing.
#[utoipa::path(
    tag = "External Services",
    put,
    path = "/external-services/healthcheck-defaults",
    request_body = HealthcheckOverrides,
    responses(
        (status = 200, description = "Saved", body = HealthcheckDefaults),
        (status = 400, description = "A value is out of range, the timeout is not shorter than the interval, or no check would land inside an engine's readiness wait", body = temps_core::problemdetails::ProblemDetails),
        (status = 409, description = "The default would make a service's existing overrides invalid", body = temps_core::problemdetails::ProblemDetails),
        (status = 401, description = "Unauthorized", body = temps_core::problemdetails::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::problemdetails::ProblemDetails),
        (status = 500, description = "Internal server error", body = temps_core::problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_healthcheck_defaults(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<HealthcheckState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<HealthcheckOverrides>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    let update = state
        .defaults
        .update(request)
        .await
        .map_err(|e| healthcheck_problem(e, "save the healthcheck defaults"))?;

    let audit = ManagedServiceHealthcheckDefaultsUpdatedAudit {
        context: audit_context(&auth, &metadata),
        previous: update.previous,
        current: update.current,
    };
    if let Err(e) = state.app.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log for the managed service healthcheck defaults: {e}");
    }
    Ok(Json(HealthcheckDefaults::of(update.current)))
}

/// Get a managed service's healthcheck
///
/// Its own overrides, what a new or recreated container gets (with where
/// each value comes from), and what the running container has.
#[utoipa::path(
    tag = "External Services",
    get,
    path = "/external-services/{id}/healthcheck",
    params(("id" = i32, Path, description = "External service ID")),
    responses(
        (status = 200, description = "The service's healthcheck", body = ServiceHealthcheck),
        (status = 401, description = "Unauthorized", body = temps_core::problemdetails::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::problemdetails::ProblemDetails),
        (status = 404, description = "Service not found", body = temps_core::problemdetails::ProblemDetails),
        (status = 500, description = "Internal server error", body = temps_core::problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_service_healthcheck(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<HealthcheckState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);
    super::metrics_handlers::assert_service_owned_by_caller(id, &auth, &state.app).await?;
    let healthcheck = state
        .app
        .external_service_manager
        .get_service_healthcheck(id)
        .await
        .map_err(|e| healthcheck_problem(e, "read the healthcheck"))?;
    Ok(Json(healthcheck))
}

/// Replace a managed service's healthcheck overrides
///
/// Send null for a value to inherit the instance default. Applies when the
/// container is next recreated; nothing running changes.
#[utoipa::path(
    tag = "External Services",
    put,
    path = "/external-services/{id}/healthcheck",
    params(("id" = i32, Path, description = "External service ID")),
    request_body = HealthcheckOverrides,
    responses(
        (status = 200, description = "Saved; applies when the container is recreated", body = ServiceHealthcheck),
        (status = 400, description = "A value is out of range, the timeout is not shorter than the interval, or no check would land inside the engine's readiness wait", body = temps_core::problemdetails::ProblemDetails),
        (status = 401, description = "Unauthorized", body = temps_core::problemdetails::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::problemdetails::ProblemDetails),
        (status = 404, description = "Service not found", body = temps_core::problemdetails::ProblemDetails),
        (status = 409, description = "The service's container has no healthcheck Temps builds", body = temps_core::problemdetails::ProblemDetails),
        (status = 500, description = "Internal server error", body = temps_core::problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_service_healthcheck(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<HealthcheckState>>,
    Path(id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<HealthcheckOverrides>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);
    super::metrics_handlers::assert_service_permission_for_caller(
        id,
        &auth,
        &state.app,
        temps_auth::Permission::ExternalServicesWrite,
    )
    .await?;
    let manager = &state.app.external_service_manager;
    let update = manager
        .set_service_healthcheck(id, request)
        .await
        .map_err(|e| healthcheck_problem(e, "save the healthcheck"))?;

    let audit = ExternalServiceHealthcheckUpdatedAudit {
        context: audit_context(&auth, &metadata),
        service_id: update.service_id,
        service_name: update.service_name.clone(),
        previous: update.previous,
        current: update.current,
    };
    if let Err(e) = state.app.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log for the healthcheck of service {id}: {e}");
    }
    let healthcheck = manager
        .get_service_healthcheck(id)
        .await
        .map_err(|e| healthcheck_problem(e, "read the healthcheck"))?;
    Ok(Json(healthcheck))
}

/// Recreate a managed service's container to apply its healthcheck now
///
/// The service restarts briefly. Its volumes and configuration are kept: the
/// original container is kept until the replacement holds the same data,
/// and put back otherwise. Only for a running standalone service on a
/// process that runs local containers: recreating starts the container, so
/// a stopped service is left alone. Not available for clusters, worker
/// databases or imported containers.
#[utoipa::path(
    tag = "External Services",
    post,
    path = "/external-services/{id}/healthcheck/apply",
    params(("id" = i32, Path, description = "External service ID")),
    responses(
        (status = 200, description = "The container was recreated", body = ApplyServiceHealthcheckResponse),
        (status = 401, description = "Unauthorized", body = temps_core::problemdetails::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::problemdetails::ProblemDetails),
        (status = 404, description = "Service not found", body = temps_core::problemdetails::ProblemDetails),
        (status = 409, description = "Temps will not recreate this container from here (not running, local workloads disabled, a cluster, worker or imported container), or the recreate was refused to protect its data", body = temps_core::problemdetails::ProblemDetails),
        (status = 500, description = "The recreate failed; the original container was kept", body = temps_core::problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn apply_service_healthcheck(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<HealthcheckState>>,
    Path(id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);
    super::metrics_handlers::assert_service_permission_for_caller(
        id,
        &auth,
        &state.app,
        temps_auth::Permission::ExternalServicesWrite,
    )
    .await?;
    let manager = &state.app.external_service_manager;
    let applied = manager
        .apply_service_healthcheck(id)
        .await
        .map_err(|e| healthcheck_problem(e, "recreate the container"))?;

    let audit = ExternalServiceHealthcheckAppliedAudit {
        context: audit_context(&auth, &metadata),
        service_id: applied.service_id,
        service_name: applied.service_name.clone(),
        applied: applied.timings,
    };
    if let Err(e) = state.app.audit_service.create_audit_log(&audit).await {
        error!("Failed to create audit log for applying the healthcheck of service {id}: {e}");
    }
    // The container is already replaced: a failed read is not a failed
    // apply, and a 500 here would invite a second restart.
    let healthcheck = match manager.get_service_healthcheck(id).await {
        Ok(healthcheck) => Some(healthcheck),
        Err(error) => {
            warn!(
                service_id = id,
                %error,
                "Recreated the container with its healthcheck, but could not read the healthcheck back"
            );
            None
        }
    };
    Ok(Json(ApplyServiceHealthcheckResponse {
        service_id: id,
        applied: applied.timings,
        healthcheck,
    }))
}

pub fn configure_routes() -> Router<Arc<HealthcheckState>> {
    Router::new()
        .route(
            "/external-services/healthcheck-defaults",
            get(get_healthcheck_defaults).put(update_healthcheck_defaults),
        )
        .route(
            "/external-services/{id}/healthcheck",
            get(get_service_healthcheck).put(update_service_healthcheck),
        )
        .route(
            "/external-services/{id}/healthcheck/apply",
            post(apply_service_healthcheck),
        )
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_healthcheck_defaults,
        update_healthcheck_defaults,
        get_service_healthcheck,
        update_service_healthcheck,
        apply_service_healthcheck,
    ),
    components(schemas(
        HealthcheckOverrides,
        HealthcheckTimings,
        HealthcheckSource,
        HealthcheckSources,
        ResolvedHealthcheck,
        ManagedEngine,
        HealthcheckDefaults,
        EngineHealthcheckDefault,
        HealthcheckCapability,
        ServiceHealthcheck,
        ApplyServiceHealthcheckResponse,
    ))
)]
pub struct HealthcheckApiDoc;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::StatusCode;
    use sea_orm::{
        ActiveModelTrait, DatabaseBackend, DatabaseConnection, EntityTrait, MockDatabase, Set,
    };
    use std::sync::Mutex;
    use temps_entities::external_services;
    use tower::ServiceExt;

    use crate::services::HEALTHCHECK_REGISTRY_TEST_LOCK as REGISTRY_LOCK;

    #[derive(Clone, Default)]
    struct RecordingAuditLogger {
        operations: Arc<Mutex<Vec<(String, String)>>>,
    }

    #[async_trait::async_trait]
    impl temps_core::AuditLogger for RecordingAuditLogger {
        async fn create_audit_log(
            &self,
            operation: &dyn temps_core::AuditOperation,
        ) -> temps_core::AuditResult<()> {
            let body = operation.serialize()?;
            self.operations
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push((operation.operation_type(), body));
            Ok(())
        }
    }

    impl RecordingAuditLogger {
        fn recorded(&self) -> Vec<(String, String)> {
            self.operations
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone()
        }
    }

    fn auth(role: temps_auth::Role) -> AuthContext {
        let now = chrono::Utc::now();
        let user = temps_entities::users::Model {
            id: 42,
            name: "Healthcheck Operator".to_string(),
            email: "operator@example.test".to_string(),
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
        };
        AuthContext::new_session(user, role)
    }

    fn metadata() -> RequestMetadata {
        RequestMetadata {
            ip_address: "198.51.100.7".to_string(),
            user_agent: "healthcheck-test".to_string(),
            headers: Default::default(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "https://temps.example.test".to_string(),
            scheme: "https".to_string(),
            host: "temps.example.test".to_string(),
            is_secure: true,
        }
    }

    const ENCRYPTION_PASSWORD: &str = "healthcheck-handler-test";

    /// A service row as the manager stores it: its parameters encrypted.
    async fn insert_service(
        db: &DatabaseConnection,
        name: &str,
        service_type: &str,
        topology: &str,
    ) -> external_services::Model {
        let encryption = temps_core::EncryptionService::new_from_password(ENCRYPTION_PASSWORD);
        let now = chrono::Utc::now();
        external_services::ActiveModel {
            name: Set(name.to_string()),
            service_type: Set(service_type.to_string()),
            status: Set("running".to_string()),
            config: Set(Some(
                encryption.encrypt_string("{}").expect("encrypt config"),
            )),
            created_at: Set(now),
            updated_at: Set(now),
            topology: Set(topology.to_string()),
            consecutive_health_failures: Set(0),
            metrics_enabled: Set(true),
            default_backup_provisioned: Set(false),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert external service")
    }

    fn state(db: Arc<DatabaseConnection>, audit: RecordingAuditLogger) -> Arc<HealthcheckState> {
        state_with(db, audit, true)
    }

    /// `local_workloads`: whether this process runs managed containers.
    fn state_with(
        db: Arc<DatabaseConnection>,
        audit: RecordingAuditLogger,
        local_workloads: bool,
    ) -> Arc<HealthcheckState> {
        let docker = Arc::new(temps_core::DockerHandle::disabled(
            "control-plane",
            "healthcheck handler test",
        ));
        let manager = Arc::new(crate::services::ExternalServiceManager::new_with_handle(
            db.clone(),
            Arc::new(temps_core::EncryptionService::new_from_password(
                ENCRYPTION_PASSWORD,
            )),
            docker,
            local_workloads,
            Arc::new(temps_dns::DnsRegistry::new(db.clone())),
        ));
        let server_config = Arc::new(
            temps_config::ServerConfig::new(
                "127.0.0.1:3000".to_string(),
                "postgresql://test".to_string(),
                None,
                Some("127.0.0.1:8000".to_string()),
            )
            .expect("ServerConfig::new"),
        );
        let config = Arc::new(temps_config::ConfigService::new(server_config, db.clone()));
        let app = Arc::new(AppState {
            external_service_manager: manager.clone(),
            data_import_service: crate::handlers::types::test_support::data_import_service(
                manager.clone(),
                db.clone(),
            ),
            sensitive_action_authorizer:
                crate::handlers::types::test_support::allow_sensitive_actions(),
            audit_service: Arc::new(audit),
            query_service: Arc::new(crate::QueryService::new(manager)),
            health_monitor: None,
            metrics_store: None,
            worker_metrics_status: None,
            db: db.clone(),
            api_key_service: Arc::new(temps_auth::ApiKeyService::new(db)),
            config_service: None,
            telemetry: Arc::new(temps_core::NoopTelemetryReporter),
            project_access_checker: None,
            application_network_reconciler: None,
            notification_service: Arc::new(crate::handlers::types::StaticNotificationService {
                destinations: Some(0),
            }),
        });
        let defaults = Arc::new(HealthcheckDefaultsService::new(
            config,
            app.external_service_manager.clone(),
        ));
        Arc::new(HealthcheckState { app, defaults })
    }

    fn mock_state(audit: RecordingAuditLogger) -> Arc<HealthcheckState> {
        // No query results: any database access fails the request with a
        // 500, so a 4xx shows the refusal came first.
        state(
            Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
            audit,
        )
    }

    async fn body_json(
        result: Result<impl IntoResponse, Problem>,
    ) -> (StatusCode, serde_json::Value) {
        let response = match result {
            Ok(response) => response.into_response(),
            Err(problem) => problem.into_response(),
        };
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or_default())
    }

    fn interval(secs: u32) -> HealthcheckOverrides {
        HealthcheckOverrides {
            interval_secs: Some(secs),
            ..Default::default()
        }
    }

    async fn test_database(name: &str) -> Option<temps_test_utils::TestDatabase> {
        match temps_test_utils::TestDatabase::with_migrations().await {
            Ok(db) => Some(db),
            Err(error)
                if temps_test_utils::is_container_runtime_unavailable(&error.to_string()) =>
            {
                eprintln!("Skipping {name}: Docker unavailable: {error}");
                None
            }
            Err(error) => panic!("{name} database setup failed: {error}"),
        }
    }

    #[tokio::test]
    async fn every_healthcheck_route_requires_authentication() {
        for (method, uri, body) in [
            ("GET", "/external-services/healthcheck-defaults", None),
            (
                "PUT",
                "/external-services/healthcheck-defaults",
                Some(r#"{"interval_secs":60}"#),
            ),
            ("GET", "/external-services/7/healthcheck", None),
            (
                "PUT",
                "/external-services/7/healthcheck",
                Some(r#"{"interval_secs":60}"#),
            ),
            ("POST", "/external-services/7/healthcheck/apply", None),
        ] {
            let audit = RecordingAuditLogger::default();
            let app = configure_routes().with_state(mock_state(audit.clone()));
            let request = axum::http::Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .extension(metadata())
                .body(body.map(Body::from).unwrap_or_else(Body::empty))
                .unwrap();
            let response = app.oneshot(request).await.unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {uri}"
            );
            assert!(audit.recorded().is_empty());
        }
    }

    #[tokio::test]
    async fn roles_without_the_permission_are_refused_before_anything_is_read() {
        for role in [temps_auth::Role::Reader, temps_auth::Role::ApiReader] {
            let audit = RecordingAuditLogger::default();
            let app = mock_state(audit.clone());
            let (status, body) = body_json(
                update_healthcheck_defaults(
                    RequireAuth(auth(role.clone())),
                    State(app.clone()),
                    Extension(metadata()),
                    Json(interval(60)),
                )
                .await,
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{role:?} saving the default");
            assert!(body["detail"].is_string(), "{body}");
            assert_eq!(
                body_json(
                    update_service_healthcheck(
                        RequireAuth(auth(role.clone())),
                        State(app.clone()),
                        Path(7),
                        Extension(metadata()),
                        Json(interval(60)),
                    )
                    .await
                )
                .await
                .0,
                StatusCode::FORBIDDEN,
                "{role:?} saving a service override"
            );
            assert_eq!(
                body_json(
                    apply_service_healthcheck(
                        RequireAuth(auth(role.clone())),
                        State(app.clone()),
                        Path(7),
                        Extension(metadata()),
                    )
                    .await
                )
                .await
                .0,
                StatusCode::FORBIDDEN,
                "{role:?} applying"
            );
            assert!(audit.recorded().is_empty());
        }
        // The instance default is a platform setting: a member who may
        // manage services still cannot change it, nor read it.
        let app = mock_state(RecordingAuditLogger::default());
        assert_eq!(
            body_json(
                get_healthcheck_defaults(RequireAuth(auth(temps_auth::Role::User)), State(app))
                    .await
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn an_out_of_range_default_is_a_400_naming_the_field() {
        let audit = RecordingAuditLogger::default();
        let (status, body) = body_json(
            update_healthcheck_defaults(
                RequireAuth(auth(temps_auth::Role::Admin)),
                State(mock_state(audit.clone())),
                Extension(metadata()),
                Json(interval(2)),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        let detail = body["detail"].as_str().unwrap_or_default();
        assert!(detail.contains("interval_secs"), "{detail}");
        assert!(audit.recorded().is_empty());
    }

    #[tokio::test]
    async fn an_out_of_range_service_override_is_a_400_before_anything_is_written() {
        let audit = RecordingAuditLogger::default();
        // The access check reads the service and its project links; the
        // bounds are checked before anything else is read or written.
        let mut service = crate::externalsvc::test_utils::create_mock_external_service(
            "cache".to_string(),
            "redis",
            "8",
        );
        service.id = 7;
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![service]])
            .append_query_results([Vec::<temps_entities::project_services::Model>::new()]);
        let (status, body) = body_json(
            update_service_healthcheck(
                RequireAuth(auth(temps_auth::Role::Admin)),
                State(state(Arc::new(db.into_connection()), audit.clone())),
                Path(7),
                Extension(metadata()),
                Json(HealthcheckOverrides {
                    retries: Some(11),
                    ..Default::default()
                }),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        let detail = body["detail"].as_str().unwrap_or_default();
        assert!(
            detail.contains("retries") && detail.contains("service 7"),
            "{detail}"
        );
        assert!(audit.recorded().is_empty());
    }

    #[tokio::test]
    async fn saving_the_instance_default_is_audit_logged_with_both_values() {
        let _registry = REGISTRY_LOCK.lock().await;
        let Some(database) = test_database("healthcheck defaults handler test").await else {
            return;
        };
        let audit = RecordingAuditLogger::default();
        let app = state(database.connection_arc(), audit.clone());
        let saved = HealthcheckOverrides {
            interval_secs: Some(60),
            retries: Some(4),
            ..Default::default()
        };
        let result = update_healthcheck_defaults(
            RequireAuth(auth(temps_auth::Role::Admin)),
            State(app.clone()),
            Extension(metadata()),
            Json(saved),
        )
        .await;
        // The process-wide default would reach every later container
        // creation in this test binary; put it back before asserting.
        temps_providers_core::healthcheck::registry::set_instance_default(
            HealthcheckOverrides::default(),
        );
        let (status, body) = body_json(result).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["overrides"]["interval_secs"], 60);
        assert_eq!(body["overrides"]["retries"], 4);
        assert!(body["overrides"]["timeout_secs"].is_null());

        let operations = audit.recorded();
        assert_eq!(operations.len(), 1, "{operations:?}");
        let (kind, body) = &operations[0];
        assert_eq!(kind, "MANAGED_SERVICE_HEALTHCHECK_DEFAULTS_UPDATED");
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        assert!(body["previous"]["interval_secs"].is_null(), "{body}");
        assert_eq!(body["current"]["interval_secs"], 60);
        assert_eq!(body["context"]["user_id"], 42);

        let (status, body) = body_json(
            get_healthcheck_defaults(RequireAuth(auth(temps_auth::Role::Admin)), State(app)).await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["overrides"]["interval_secs"], 60, "{body}");
    }

    #[tokio::test]
    async fn a_reload_picks_up_what_another_process_saved() {
        use temps_providers_core::healthcheck::registry;
        let _registry = REGISTRY_LOCK.lock().await;
        let Some(database) = test_database("healthcheck registry reload test").await else {
            return;
        };
        let db = database.connection_arc();
        let row = insert_service(db.as_ref(), "cache-elsewhere", "redis", "standalone").await;
        let app = state(db.clone(), RecordingAuditLogger::default());

        // Another process saves both, publishing only to its own registry.
        let mut active: external_services::ActiveModel = row.clone().into();
        active.healthcheck_interval_secs = Set(Some(90));
        active.update(db.as_ref()).await.expect("save the override");
        let other_process = temps_config::ConfigService::new(
            Arc::new(
                temps_config::ServerConfig::new(
                    "127.0.0.1:3000".to_string(),
                    "postgresql://test".to_string(),
                    None,
                    Some("127.0.0.1:8000".to_string()),
                )
                .expect("ServerConfig::new"),
            ),
            db,
        );
        other_process
            .update_managed_service_healthcheck_settings(|section| {
                section.retries = Some(4);
            })
            .await
            .expect("save the instance default");

        let result = app.defaults.reload_registry().await;
        let instance = registry::instance_default();
        let service = registry::for_service(row.id);
        // The registry is process-wide; put it back before asserting.
        registry::set_instance_default(HealthcheckOverrides::default());
        registry::clear_service(row.id);
        assert_eq!(result.expect("reload"), 1);
        assert_eq!(
            instance,
            HealthcheckOverrides {
                retries: Some(4),
                ..Default::default()
            }
        );
        assert_eq!(service, interval(90));
    }

    #[tokio::test]
    async fn a_change_notification_reloads_only_the_service_it_names() {
        use temps_providers_core::healthcheck::registry;
        let _registry = REGISTRY_LOCK.lock().await;
        let Some(database) = test_database("healthcheck notification test").await else {
            return;
        };
        let db = database.connection_arc();
        let with_interval = |row: &external_services::Model, interval: i32| {
            let mut active: external_services::ActiveModel = row.clone().into();
            active.healthcheck_interval_secs = Set(Some(interval));
            active
        };
        let changed = insert_service(db.as_ref(), "cache-changed", "redis", "standalone").await;
        with_interval(&changed, 90)
            .update(db.as_ref())
            .await
            .expect("save the changed override");
        let quiet = insert_service(db.as_ref(), "cache-quiet", "redis", "standalone").await;
        with_interval(&quiet, 120)
            .update(db.as_ref())
            .await
            .expect("save the other override");
        // Cleared and deleted by another process; this one still has them.
        let cleared = insert_service(db.as_ref(), "cache-cleared", "redis", "standalone").await;
        let deleted = cleared.id + 1_000;
        for (service_id, container) in [(cleared.id, None), (deleted, Some("redis-gone"))] {
            registry::set_service(registry::ServiceHealthcheckEntry {
                service_id,
                container_name: container.map(str::to_string),
                overrides: interval(60),
            });
        }
        let app = state(db, RecordingAuditLogger::default());

        for payload in [
            changed.id.to_string(),
            cleared.id.to_string(),
            deleted.to_string(),
            "not-a-service".to_string(),
        ] {
            app.defaults
                .handle_registry_notification(&payload)
                .await
                .unwrap_or_else(|error| panic!("{payload}: {error}"));
        }
        let published = [changed.id, quiet.id, cleared.id, deleted].map(registry::for_service);
        let gone_by_name = registry::for_container("redis-gone");
        // The registry is process-wide; put it back before asserting.
        for id in [changed.id, quiet.id, cleared.id, deleted] {
            registry::clear_service(id);
        }
        assert_eq!(published[0], interval(90));
        assert!(
            published[1].is_empty(),
            "not named, not read: {:?}",
            published[1]
        );
        assert!(published[2].is_empty(), "cleared: {:?}", published[2]);
        assert!(published[3].is_empty(), "deleted: {:?}", published[3]);
        assert!(gone_by_name.is_empty(), "deleted: {gone_by_name:?}");
    }

    #[tokio::test]
    async fn a_saved_override_is_announced_to_other_processes() {
        use sqlx::postgres::PgListener;
        let _registry = REGISTRY_LOCK.lock().await;
        let Some(database) = test_database("healthcheck announcement test").await else {
            return;
        };
        let db = database.connection_arc();
        let row = insert_service(db.as_ref(), "cache-announced", "redis", "standalone").await;
        let mut listener = PgListener::connect(&database.database_url)
            .await
            .expect("connect a listener");
        listener
            .listen(crate::services::HEALTHCHECK_CHANNEL)
            .await
            .expect("listen");
        let app = state(db, RecordingAuditLogger::default());

        let result = update_service_healthcheck(
            RequireAuth(auth(temps_auth::Role::Admin)),
            State(app),
            Path(row.id),
            Extension(metadata()),
            Json(interval(120)),
        )
        .await;
        temps_providers_core::healthcheck::registry::clear_service(row.id);
        let (status, body) = body_json(result).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        // Other tests share the channel; wait for this service's id.
        let announced = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let notification = listener.recv().await.expect("receive");
                if notification.payload() == row.id.to_string() {
                    return;
                }
            }
        })
        .await;
        assert!(announced.is_ok(), "no notification for service {}", row.id);
    }

    #[tokio::test]
    async fn a_service_override_is_saved_audit_logged_and_shown_as_its_source() {
        let _registry = REGISTRY_LOCK.lock().await;
        let Some(database) = test_database("service healthcheck handler test").await else {
            return;
        };
        let db = database.connection_arc();
        let row = insert_service(db.as_ref(), "cache-healthcheck", "redis", "standalone").await;
        let audit = RecordingAuditLogger::default();
        let app = state(db, audit.clone());

        let result = update_service_healthcheck(
            RequireAuth(auth(temps_auth::Role::Admin)),
            State(app.clone()),
            Path(row.id),
            Extension(metadata()),
            Json(interval(120)),
        )
        .await;
        // The registry is process-wide; forget the row before asserting.
        temps_providers_core::healthcheck::registry::clear_service(row.id);
        let (status, body) = body_json(result).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["overrides"]["interval_secs"], 120);
        assert_eq!(body["effective"]["timings"]["interval_secs"], 120);
        assert_eq!(body["effective"]["sources"]["interval_secs"], "service");
        assert_eq!(body["effective"]["sources"]["timeout_secs"], "builtin");
        assert_eq!(body["capability"]["configurable"], true);
        assert_eq!(body["capability"]["can_apply_now"], true);

        let operations = audit.recorded();
        assert_eq!(operations.len(), 1, "{operations:?}");
        let (kind, audit_body) = &operations[0];
        assert_eq!(kind, "EXTERNAL_SERVICE_HEALTHCHECK_UPDATED");
        let audit_body: serde_json::Value = serde_json::from_str(audit_body).unwrap();
        assert_eq!(audit_body["service_id"], row.id);
        assert_eq!(audit_body["service_name"], "cache-healthcheck");
        assert!(audit_body["previous"]["interval_secs"].is_null());
        assert_eq!(audit_body["current"]["interval_secs"], 120);

        // The stored column reads back.
        let (status, body) = body_json(
            get_service_healthcheck(
                RequireAuth(auth(temps_auth::Role::Admin)),
                State(app.clone()),
                Path(row.id),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["overrides"]["interval_secs"], 120, "{body}");

        // The timeout must stay shorter than the interval, including the
        // inherited one (Redis' built-in timeout is 5s).
        for refused in [
            HealthcheckOverrides {
                interval_secs: Some(10),
                timeout_secs: Some(10),
                ..Default::default()
            },
            interval(5),
        ] {
            let (status, body) = body_json(
                update_service_healthcheck(
                    RequireAuth(auth(temps_auth::Role::Admin)),
                    State(app.clone()),
                    Path(row.id),
                    Extension(metadata()),
                    Json(refused),
                )
                .await,
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{refused:?}: {body}");
            let detail = body["detail"].as_str().unwrap_or_default();
            assert!(detail.contains("timeout"), "{detail}");
        }

        // An unknown service is a 404 that names the id.
        let (status, body) = body_json(
            get_service_healthcheck(
                RequireAuth(auth(temps_auth::Role::Admin)),
                State(app),
                Path(row.id + 1_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        // Only the one accepted save was audited.
        assert_eq!(audit.recorded().len(), 1);
    }

    #[tokio::test]
    async fn a_cluster_cannot_be_overridden_or_recreated_from_here() {
        let Some(database) = test_database("cluster healthcheck handler test").await else {
            return;
        };
        let db = database.connection_arc();
        let row = insert_service(db.as_ref(), "ha-healthcheck", "postgres", "cluster").await;
        let audit = RecordingAuditLogger::default();
        let app = state(db, audit.clone());

        let (status, body) = body_json(
            update_service_healthcheck(
                RequireAuth(auth(temps_auth::Role::Admin)),
                State(app.clone()),
                Path(row.id),
                Extension(metadata()),
                Json(interval(60)),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        let (status, body) = body_json(
            apply_service_healthcheck(
                RequireAuth(auth(temps_auth::Role::Admin)),
                State(app),
                Path(row.id),
                Extension(metadata()),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        let detail = body["detail"].as_str().unwrap_or_default();
        assert!(detail.contains(&row.id.to_string()), "{detail}");
        assert!(audit.recorded().is_empty());
    }

    async fn set_status(db: &DatabaseConnection, row: &external_services::Model, status: &str) {
        let mut active: external_services::ActiveModel = row.clone().into();
        active.status = Set(status.to_string());
        active.update(db).await.expect("update status");
    }

    #[tokio::test]
    async fn a_default_whose_first_check_comes_after_the_readiness_wait_is_a_400() {
        let audit = RecordingAuditLogger::default();
        // A start period of 0 and a 120s interval: Docker's first check
        // would come after RustFS's 60s and every other engine's wait.
        let (status, body) = body_json(
            update_healthcheck_defaults(
                RequireAuth(auth(temps_auth::Role::Admin)),
                State(mock_state(audit.clone())),
                Extension(metadata()),
                Json(HealthcheckOverrides {
                    start_period_secs: Some(0),
                    interval_secs: Some(120),
                    ..Default::default()
                }),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        let detail = body["detail"].as_str().unwrap_or_default();
        // Names the value to change and the wait it misses.
        assert!(
            detail.contains("first check comes 120s") && detail.contains("interval_secs"),
            "{detail}"
        );
        assert!(audit.recorded().is_empty());
    }

    #[tokio::test]
    async fn a_stopped_service_is_not_started_by_apply_now() {
        let Some(database) = test_database("stopped healthcheck apply test").await else {
            return;
        };
        let db = database.connection_arc();
        let row = insert_service(db.as_ref(), "stopped-cache", "redis", "standalone").await;
        set_status(db.as_ref(), &row, "stopped").await;
        let audit = RecordingAuditLogger::default();
        let app = state(db.clone(), audit.clone());

        let (status, body) = body_json(
            get_service_healthcheck(
                RequireAuth(auth(temps_auth::Role::Admin)),
                State(app.clone()),
                Path(row.id),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["capability"]["configurable"], true);
        assert_eq!(body["capability"]["can_apply_now"], false);
        let reason = body["capability"]["apply_now_reason"]
            .as_str()
            .unwrap_or_default();
        assert!(
            reason.contains("stopped") && reason.contains("would start it"),
            "{reason}"
        );

        let (status, body) = body_json(
            apply_service_healthcheck(
                RequireAuth(auth(temps_auth::Role::Admin)),
                State(app),
                Path(row.id),
                Extension(metadata()),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(
            body["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("stopped"),
            "{body}"
        );
        // Nothing was recreated or started: the row is still stopped.
        let stored = external_services::Entity::find_by_id(row.id)
            .one(db.as_ref())
            .await
            .expect("read service")
            .expect("service exists");
        assert_eq!(stored.status, "stopped");
        assert!(audit.recorded().is_empty());
    }

    #[tokio::test]
    async fn without_local_workloads_apply_reports_the_policy_not_docker() {
        let Some(database) = test_database("local workloads healthcheck apply test").await else {
            return;
        };
        let db = database.connection_arc();
        let row = insert_service(db.as_ref(), "remote-console-cache", "redis", "standalone").await;
        let audit = RecordingAuditLogger::default();
        let app = state_with(db, audit.clone(), false);

        let (status, body) = body_json(
            get_service_healthcheck(
                RequireAuth(auth(temps_auth::Role::Admin)),
                State(app.clone()),
                Path(row.id),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["capability"]["can_apply_now"], false);
        assert!(
            body["capability"]["apply_now_reason"]
                .as_str()
                .unwrap_or_default()
                .contains("local workloads are disabled"),
            "{body}"
        );

        let (status, body) = body_json(
            apply_service_healthcheck(
                RequireAuth(auth(temps_auth::Role::Admin)),
                State(app),
                Path(row.id),
                Extension(metadata()),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(
            body["error_code"],
            temps_core::WORKER_NODE_REQUIRED_ERROR_CODE,
            "{body}"
        );
        assert!(audit.recorded().is_empty());
    }

    #[tokio::test]
    async fn a_default_that_would_invalidate_a_service_override_is_refused() {
        let _registry = REGISTRY_LOCK.lock().await;
        let Some(database) = test_database("healthcheck default conflict test").await else {
            return;
        };
        let db = database.connection_arc();
        let row = insert_service(db.as_ref(), "slow-check-cache", "redis", "standalone").await;
        let audit = RecordingAuditLogger::default();
        let app = state(db, audit.clone());

        // A 20s timeout is fine against the built-in 30s interval...
        let saved = update_service_healthcheck(
            RequireAuth(auth(temps_auth::Role::Admin)),
            State(app.clone()),
            Path(row.id),
            Extension(metadata()),
            Json(HealthcheckOverrides {
                timeout_secs: Some(20),
                ..Default::default()
            }),
        )
        .await;
        let (status, body) = body_json(saved).await;
        assert_eq!(status, StatusCode::OK, "{body}");

        // ...but not against a 15s instance default, which every engine's
        // built-in values would accept.
        let refused = update_healthcheck_defaults(
            RequireAuth(auth(temps_auth::Role::Admin)),
            State(app.clone()),
            Extension(metadata()),
            Json(HealthcheckOverrides {
                interval_secs: Some(15),
                ..Default::default()
            }),
        )
        .await;
        temps_providers_core::healthcheck::registry::clear_service(row.id);
        let (status, body) = body_json(refused).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        let detail = body["detail"].as_str().unwrap_or_default();
        assert!(
            detail.contains(&format!("service {}", row.id))
                && detail.contains("slow-check-cache")
                && detail.contains("timeout_secs"),
            "{detail}"
        );
        // Only the service save was audited; the default was not saved.
        let operations = audit.recorded();
        assert_eq!(operations.len(), 1, "{operations:?}");
        assert_eq!(operations[0].0, "EXTERNAL_SERVICE_HEALTHCHECK_UPDATED");
    }
}
