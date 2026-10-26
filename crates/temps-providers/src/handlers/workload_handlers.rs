// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A database's workload mode (ADR-055 Phase 4).
//!
//! ```text
//! GET  /external-services/{id}/workload-mode
//! PUT  /external-services/{id}/workload-mode
//! GET  /external-services/{id}/workload-mode/pending-changes
//! POST /external-services/{id}/workload-mode/recreate
//! ```
//!
//! Every route is restricted to an instance administrator: the mode decides
//! how a database's container is built on its host, and the pending changes
//! show that host's container configuration. A mode change and a recreate
//! are audit-logged. Nothing running changes when the mode changes; in
//! `enforce` a container gains its spec's host config only when it is
//! recreated, and a recreate is only performed with the fingerprint of the
//! pending changes the administrator was shown.

use std::sync::Arc;

use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, AuthContext, RequireAuth};
use temps_core::error_builder::ErrorBuilder;
use temps_core::problemdetails::{self, Problem};
use temps_core::{AuditContext, RequestMetadata};
use temps_entities::external_service_workload_mode::ServiceWorkloadMode;
use tracing::{error, info};
use utoipa::{OpenApi, ToSchema};

use super::audit::{
    ExternalServiceWorkloadModeChangedAudit, ExternalServiceWorkloadRecreatedAudit,
};
use super::handlers::worker_node_required;
use crate::handlers::types::AppState;
use crate::services::{RecreateOutcome, WorkloadModeStatus};
use crate::workload::{PendingChanges, ServiceWorkloadError, WorkloadChange, WorkloadSupport};

impl From<ServiceWorkloadError> for Problem {
    fn from(error: ServiceWorkloadError) -> Self {
        let (status, title) = match &error {
            ServiceWorkloadError::NotFound { .. } => (StatusCode::NOT_FOUND, "Service Not Found"),
            ServiceWorkloadError::Unsupported { .. } => {
                (StatusCode::BAD_REQUEST, "Workload Mode Not Supported")
            }
            ServiceWorkloadError::Admission { .. } => {
                (StatusCode::UNPROCESSABLE_ENTITY, "Workload Spec Refused")
            }
            ServiceWorkloadError::ContainerMissing { .. } => {
                (StatusCode::CONFLICT, "Database Container Missing")
            }
            ServiceWorkloadError::StaleConfirmation { .. } => {
                (StatusCode::CONFLICT, "Pending Changes Changed")
            }
            ServiceWorkloadError::NotEnforced { .. } => {
                (StatusCode::CONFLICT, "Database Not In Enforce Mode")
            }
            ServiceWorkloadError::AgentUnsupported { .. } => {
                (StatusCode::CONFLICT, "Agent Upgrade Required")
            }
            ServiceWorkloadError::Agent { .. } => (StatusCode::BAD_GATEWAY, "Agent Request Failed"),
            ServiceWorkloadError::Service { source, .. } => {
                if let Some(problem) = worker_node_required(source) {
                    return problem;
                }
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Workload Operation Failed",
                )
            }
            ServiceWorkloadError::Process { .. } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Database Container Unreadable",
            ),
            ServiceWorkloadError::Docker { .. } => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Docker Operation Failed")
            }
            ServiceWorkloadError::Unhealthy { .. } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Database Did Not Become Healthy",
            ),
            ServiceWorkloadError::Recreate { .. } => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Recreate Failed")
            }
            ServiceWorkloadError::Database(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
            }
        };
        problemdetails::new(status)
            .with_title(title)
            .with_detail(error.to_string())
    }
}

/// A database's workload mode.
// Named apart from the environment's `WorkloadModeResponse`: both are in
// the merged OpenAPI document, where the later one replaced this schema.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[schema(as = ServiceWorkloadModeResponse)]
pub struct WorkloadModeResponse {
    pub service_id: i32,
    pub service_name: String,
    pub service_type: String,
    /// `legacy`: the engine (or the worker's agent) builds its own
    /// container request. `shadow`: the same, and a background pass logs
    /// where the running container differs from its workload spec.
    /// `enforce`: the container is created from its admitted spec through
    /// the Docker driver, the next time it is created or recreated.
    pub mode: ServiceWorkloadMode,
    /// `None`: the database runs on the control plane.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<i32>,
    /// Whether the database can leave `legacy`, and why not.
    pub support: WorkloadSupport,
}

impl From<WorkloadModeStatus> for WorkloadModeResponse {
    fn from(status: WorkloadModeStatus) -> Self {
        Self {
            service_id: status.service_id,
            service_name: status.service_name,
            service_type: status.service_type,
            mode: status.mode,
            node_id: status.node_id,
            support: status.support,
        }
    }
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateWorkloadModeRequest {
    pub mode: ServiceWorkloadMode,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WorkloadModeChangeResponse {
    pub previous_mode: ServiceWorkloadMode,
    pub workload: WorkloadModeResponse,
    /// What a recreate from the spec would change, when the new mode is not
    /// `legacy` and the container could be read. Nothing has changed yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_changes: Option<PendingChanges>,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct RecreateFromWorkloadSpecRequest {
    /// The `fingerprint` of the pending changes being confirmed. Leave out
    /// to only list them: nothing is recreated without it.
    #[serde(default)]
    pub confirmed_fingerprint: Option<String>,
}

/// What a recreate request did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RecreateStatus {
    /// Nothing changed: confirm `pending_changes` by sending back its
    /// fingerprint.
    ConfirmationRequired,
    /// The container was replaced from its spec.
    Recreated,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RecreateFromWorkloadSpecResponse {
    pub status: RecreateStatus,
    /// The changes to confirm, or the changes that were applied.
    pub pending_changes: PendingChanges,
    /// After a recreate: what still differs (empty when it worked).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining_changes: Option<PendingChanges>,
}

fn change_fields(changes: &[WorkloadChange]) -> Vec<String> {
    changes.iter().map(|c| c.field.clone()).collect()
}

/// Only an instance administrator decides how a database's container is
/// built on its host. Deployment tokens carry no user and are never one.
fn require_instance_admin(
    auth: &AuthContext,
    service_id: i32,
    operation: &str,
) -> Result<(), Problem> {
    if !auth.is_deployment_token() && auth.is_instance_admin() {
        return Ok(());
    }
    Err(ErrorBuilder::new(StatusCode::FORBIDDEN)
        .type_("https://temps.sh/probs/insufficient-permissions")
        .title("Instance Administrator Required")
        .detail(format!(
            "Only an instance administrator may {operation} of external service {service_id}: \
             the workload mode decides how the database's container is built on its host"
        ))
        .value("required_role", temps_auth::Role::Admin.to_string())
        .value("user_role", auth.effective_role.to_string())
        .build())
}

fn audit_context(auth: &AuthContext, metadata: &RequestMetadata) -> AuditContext {
    AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    }
}

#[utoipa::path(
    tag = "External Services",
    get,
    path = "/external-services/{id}/workload-mode",
    params(("id" = i32, Path, description = "External service ID")),
    responses(
        (status = 200, description = "The database's workload mode", body = WorkloadModeResponse),
        (status = 401, description = "Unauthorized", body = temps_core::problemdetails::ProblemDetails),
        (status = 403, description = "Instance administrator required", body = temps_core::problemdetails::ProblemDetails),
        (status = 404, description = "Service not found", body = temps_core::problemdetails::ProblemDetails),
        (status = 500, description = "Internal server error", body = temps_core::problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_workload_mode(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);
    require_instance_admin(&auth, id, "read the workload mode")?;
    let status = app_state
        .external_service_manager
        .workload_mode_status(id)
        .await?;
    Ok(Json(WorkloadModeResponse::from(status)))
}

#[utoipa::path(
    tag = "External Services",
    put,
    path = "/external-services/{id}/workload-mode",
    params(("id" = i32, Path, description = "External service ID")),
    request_body = UpdateWorkloadModeRequest,
    responses(
        (status = 200, description = "Mode changed; nothing running changed", body = WorkloadModeChangeResponse),
        (status = 400, description = "The database cannot use this mode", body = temps_core::problemdetails::ProblemDetails),
        (status = 401, description = "Unauthorized", body = temps_core::problemdetails::ProblemDetails),
        (status = 403, description = "Instance administrator required", body = temps_core::problemdetails::ProblemDetails),
        (status = 404, description = "Service not found", body = temps_core::problemdetails::ProblemDetails),
        (status = 500, description = "Internal server error", body = temps_core::problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_workload_mode(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<UpdateWorkloadModeRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);
    require_instance_admin(&auth, id, "change the workload mode")?;
    let change = app_state
        .external_service_manager
        .set_workload_mode(id, request.mode)
        .await?;

    let audit = ExternalServiceWorkloadModeChangedAudit {
        context: audit_context(&auth, &metadata),
        service_id: id,
        name: change.status.service_name.clone(),
        service_type: change.status.service_type.clone(),
        previous_mode: change.previous.to_string(),
        new_mode: change.status.mode.to_string(),
        pending_change_fields: change
            .pending
            .as_ref()
            .map(|p| change_fields(&p.changes))
            .unwrap_or_default(),
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!(
            service_id = id,
            "Failed to create audit log for a workload mode change: {}", e
        );
    }
    info!(
        service_id = id,
        previous_mode = %change.previous,
        workload_mode = %change.status.mode,
        "Workload mode of external service changed by an administrator"
    );

    Ok(Json(WorkloadModeChangeResponse {
        previous_mode: change.previous,
        workload: change.status.into(),
        pending_changes: change.pending,
    }))
}

#[utoipa::path(
    tag = "External Services",
    get,
    path = "/external-services/{id}/workload-mode/pending-changes",
    params(("id" = i32, Path, description = "External service ID")),
    responses(
        (status = 200, description = "What a recreate from the workload spec would change", body = PendingChanges),
        (status = 401, description = "Unauthorized", body = temps_core::problemdetails::ProblemDetails),
        (status = 403, description = "Instance administrator required", body = temps_core::problemdetails::ProblemDetails),
        (status = 404, description = "Service not found", body = temps_core::problemdetails::ProblemDetails),
        (status = 409, description = "The node's agent must be upgraded", body = temps_core::problemdetails::ProblemDetails),
        (status = 500, description = "Internal server error", body = temps_core::problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_workload_pending_changes(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);
    require_instance_admin(&auth, id, "list the pending workload changes")?;
    let pending = app_state
        .external_service_manager
        .pending_workload_changes(id)
        .await?;
    Ok(Json(pending))
}

#[utoipa::path(
    tag = "External Services",
    post,
    path = "/external-services/{id}/workload-mode/recreate",
    params(("id" = i32, Path, description = "External service ID")),
    request_body = RecreateFromWorkloadSpecRequest,
    responses(
        (status = 200, description = "The changes to confirm, or the recreate's result", body = RecreateFromWorkloadSpecResponse),
        (status = 401, description = "Unauthorized", body = temps_core::problemdetails::ProblemDetails),
        (status = 403, description = "Instance administrator required", body = temps_core::problemdetails::ProblemDetails),
        (status = 404, description = "Service not found", body = temps_core::problemdetails::ProblemDetails),
        (status = 409, description = "Not in enforce mode, or the pending changes changed since they were shown", body = temps_core::problemdetails::ProblemDetails),
        (status = 422, description = "The workload spec was refused at admission", body = temps_core::problemdetails::ProblemDetails),
        (status = 500, description = "The recreate failed; the original container was kept", body = temps_core::problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn recreate_from_workload_spec(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<RecreateFromWorkloadSpecRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);
    require_instance_admin(&auth, id, "recreate the container from its workload spec")?;
    let outcome = app_state
        .external_service_manager
        .recreate_from_workload_spec(id, request.confirmed_fingerprint.as_deref())
        .await?;
    let response = match outcome {
        RecreateOutcome::NeedsConfirmation(pending) => RecreateFromWorkloadSpecResponse {
            status: RecreateStatus::ConfirmationRequired,
            pending_changes: pending,
            remaining_changes: None,
        },
        RecreateOutcome::Recreated { applied, remaining } => {
            let audit = ExternalServiceWorkloadRecreatedAudit {
                context: audit_context(&auth, &metadata),
                service_id: id,
                name: applied.service_name.clone(),
                node_id: applied.node_id,
                confirmed_fingerprint: applied.fingerprint.clone(),
                applied_change_fields: change_fields(&applied.changes),
                remaining_change_fields: change_fields(&remaining.changes),
            };
            if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
                error!(
                    service_id = id,
                    "Failed to create audit log for a recreate from the workload spec: {}", e
                );
            }
            RecreateFromWorkloadSpecResponse {
                status: RecreateStatus::Recreated,
                pending_changes: applied,
                remaining_changes: Some(remaining),
            }
        }
    };
    Ok(Json(response))
}

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/external-services/{id}/workload-mode",
            get(get_workload_mode).put(update_workload_mode),
        )
        .route(
            "/external-services/{id}/workload-mode/pending-changes",
            get(get_workload_pending_changes),
        )
        .route(
            "/external-services/{id}/workload-mode/recreate",
            post(recreate_from_workload_spec),
        )
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_workload_mode,
        update_workload_mode,
        get_workload_pending_changes,
        recreate_from_workload_spec,
    ),
    components(schemas(
        WorkloadModeResponse,
        UpdateWorkloadModeRequest,
        WorkloadModeChangeResponse,
        RecreateFromWorkloadSpecRequest,
        RecreateFromWorkloadSpecResponse,
        RecreateStatus,
        ServiceWorkloadMode,
        WorkloadSupport,
        WorkloadChange,
        PendingChanges,
    ))
)]
pub struct WorkloadModeApiDoc;

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::sync::Mutex;
    use temps_entities::external_services;

    #[derive(Clone, Default)]
    struct RecordingAuditLogger {
        operations: Arc<Mutex<Vec<(String, String)>>>,
    }

    #[async_trait::async_trait]
    impl temps_core::AuditLogger for RecordingAuditLogger {
        async fn create_audit_log(
            &self,
            operation: &dyn temps_core::AuditOperation,
        ) -> Result<(), temps_core::anyhow::Error> {
            let body = operation.serialize()?;
            self.operations
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push((operation.operation_type(), body));
            Ok(())
        }
    }

    fn auth(role: temps_auth::Role) -> AuthContext {
        let now = chrono::Utc::now();
        let user = temps_entities::users::Model {
            id: 42,
            name: "Workload Operator".to_string(),
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
            user_agent: "workload-mode-test".to_string(),
            headers: Default::default(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "https://temps.example.test".to_string(),
            scheme: "https".to_string(),
            host: "temps.example.test".to_string(),
            is_secure: true,
        }
    }

    fn service(mode: ServiceWorkloadMode) -> external_services::Model {
        let mut model = crate::externalsvc::test_utils::create_mock_external_service(
            "cache".to_string(),
            "redis",
            "8",
        );
        model.id = 7;
        model.workload_mode = mode;
        model
    }

    fn state(db: MockDatabase, audit: RecordingAuditLogger) -> Arc<AppState> {
        let db = Arc::new(db.into_connection());
        let docker = Arc::new(temps_core::DockerHandle::disabled(
            "control-plane",
            "workload mode handler test",
        ));
        let manager = Arc::new(crate::services::ExternalServiceManager::new_with_handle(
            db.clone(),
            Arc::new(temps_core::EncryptionService::new_from_password(
                "workload-mode-handler-test",
            )),
            docker,
            true,
            Arc::new(temps_dns::DnsRegistry::new(db.clone())),
        ));
        Arc::new(AppState {
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
        })
    }

    fn status_of(result: Result<impl IntoResponse, Problem>) -> StatusCode {
        match result {
            Ok(response) => response.into_response().status(),
            Err(problem) => problem.into_response().status(),
        }
    }

    #[tokio::test]
    async fn non_admins_cannot_read_or_change_the_workload_mode() {
        // No query results: any database access would fail the request
        // with a 500, so a 403 shows the refusal comes first.
        for role in [
            temps_auth::Role::User,
            temps_auth::Role::Reader,
            temps_auth::Role::ApiReader,
        ] {
            let audit = RecordingAuditLogger::default();
            let app = state(MockDatabase::new(DatabaseBackend::Postgres), audit.clone());
            let changed = update_workload_mode(
                RequireAuth(auth(role.clone())),
                State(app.clone()),
                Path(7),
                Extension(metadata()),
                Json(UpdateWorkloadModeRequest {
                    mode: ServiceWorkloadMode::Enforce,
                }),
            )
            .await;
            let changed = status_of(changed);
            assert!(
                changed == StatusCode::FORBIDDEN,
                "{role:?} changing the mode got {changed}"
            );
            assert_eq!(
                status_of(
                    get_workload_mode(RequireAuth(auth(role.clone())), State(app.clone()), Path(7))
                        .await
                ),
                StatusCode::FORBIDDEN,
                "{role:?} reading the mode"
            );
            assert_eq!(
                status_of(
                    get_workload_pending_changes(
                        RequireAuth(auth(role.clone())),
                        State(app.clone()),
                        Path(7)
                    )
                    .await
                ),
                StatusCode::FORBIDDEN,
                "{role:?} listing pending changes"
            );
            assert_eq!(
                status_of(
                    recreate_from_workload_spec(
                        RequireAuth(auth(role.clone())),
                        State(app),
                        Path(7),
                        Extension(metadata()),
                        Json(RecreateFromWorkloadSpecRequest {
                            confirmed_fingerprint: Some("x".into()),
                        }),
                    )
                    .await
                ),
                StatusCode::FORBIDDEN,
                "{role:?} recreating"
            );
            assert!(audit.operations.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn an_admin_mode_change_is_audit_logged_with_both_modes() {
        let audit = RecordingAuditLogger::default();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![service(ServiceWorkloadMode::Shadow)]])
            .append_query_results([vec![service(ServiceWorkloadMode::Legacy)]]);
        let app = state(db, audit.clone());
        let response = update_workload_mode(
            RequireAuth(auth(temps_auth::Role::Admin)),
            State(app),
            Path(7),
            Extension(metadata()),
            Json(UpdateWorkloadModeRequest {
                mode: ServiceWorkloadMode::Legacy,
            }),
        )
        .await
        .map_err(|p| p.into_response().status())
        .expect("an admin may change the mode");
        assert_eq!(response.into_response().status(), StatusCode::OK);

        let operations = audit.operations.lock().unwrap().clone();
        assert_eq!(operations.len(), 1, "{operations:?}");
        let (kind, body) = &operations[0];
        assert_eq!(kind, "EXTERNAL_SERVICE_WORKLOAD_MODE_CHANGED");
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(body["service_id"], 7);
        assert_eq!(body["previous_mode"], "shadow");
        assert_eq!(body["new_mode"], "legacy");
        assert_eq!(body["context"]["user_id"], 42);
        assert_eq!(body["context"]["ip_address"], "198.51.100.7");
    }

    #[tokio::test]
    async fn an_unsupported_database_cannot_leave_legacy() {
        let audit = RecordingAuditLogger::default();
        let mut kv = service(ServiceWorkloadMode::Legacy);
        kv.service_type = "kv".to_string();
        let db = MockDatabase::new(DatabaseBackend::Postgres).append_query_results([vec![kv]]);
        let app = state(db, audit.clone());
        let result = update_workload_mode(
            RequireAuth(auth(temps_auth::Role::Admin)),
            State(app),
            Path(7),
            Extension(metadata()),
            Json(UpdateWorkloadModeRequest {
                mode: ServiceWorkloadMode::Enforce,
            }),
        )
        .await;
        assert_eq!(status_of(result), StatusCode::BAD_REQUEST);
        assert!(audit.operations.lock().unwrap().is_empty());
    }

    #[test]
    fn workload_errors_map_to_their_status() {
        let cases = [
            (
                ServiceWorkloadError::NotFound { service_id: 3 },
                StatusCode::NOT_FOUND,
            ),
            (
                ServiceWorkloadError::StaleConfirmation {
                    service_id: 3,
                    confirmed: "a".into(),
                    current: "b".into(),
                },
                StatusCode::CONFLICT,
            ),
            (
                ServiceWorkloadError::AgentUnsupported {
                    node: "worker-1".into(),
                    operation: "create a database container from its workload spec",
                },
                StatusCode::CONFLICT,
            ),
            (
                ServiceWorkloadError::Admission {
                    service_id: 3,
                    container: "redis-cache".into(),
                    rule: "validation".into(),
                    message: "no volume".into(),
                },
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
        ];
        for (error, status) in cases {
            let detail = error.to_string();
            let response = Problem::from(error).into_response();
            assert_eq!(response.status(), status, "{detail}");
        }
    }
}
