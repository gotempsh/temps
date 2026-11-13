// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! "Test archive target" for a PostgreSQL service's continuous WAL archiving.
//!
//! ```text
//! POST /external-services/{id}/wal-archive/test   run WAL-G's write check (audited)
//! ```
//!
//! WAL-G writes a small probe object to one fixed key in the service's own
//! WAL-G prefix, with exactly the settings `walg.env` holds, then tries to
//! remove it (best effort: a credential that may write but not delete still
//! passes, and the next check overwrites the same key). A target that
//! refuses is a `200` with `ok: false` and WAL-G's error, so the console can
//! show it next to the button.

use std::sync::Arc;

use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_core::{AuditContext, AuditOperation, RequestMetadata};
use tracing::error;
use utoipa::{OpenApi, ToSchema};

use crate::handlers::types::AppState;
use crate::services::{ArchiveTargetTestError, ArchiveTargetTestOutcome};

/// What WAL-G said about a service's archive target.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ArchiveTargetTestResponse {
    /// Whether WAL-G could write a probe object to the archive target, which
    /// is what `archive_command` needs. The object is never read back, and
    /// its removal afterwards is best effort: `ok` says nothing about read
    /// or delete permission.
    pub ok: bool,
    /// The S3 source WAL is archived to.
    pub s3_source_id: i32,
    pub s3_source_name: String,
    /// `AWS_ENDPOINT` exactly as WAL-G receives it.
    #[schema(example = "http://10.0.0.5:9000")]
    pub endpoint: String,
    /// `http`, `https`, or `aws` for AWS S3 itself.
    #[schema(example = "http")]
    pub scheme: String,
    /// Whether WAL-G uses path-style addressing.
    pub force_path_style: bool,
    /// Why path style has that value.
    #[schema(example = "forced: the endpoint is an IP address")]
    pub path_style_reason: String,
    /// A confirmation, or WAL-G's error with credentials redacted.
    pub message: String,
    /// Set when the running container's `walg.env` differs from these
    /// settings: PostgreSQL archives with that file, so archiving can be
    /// failing even when this check passes. Says how to fix it.
    pub live_settings_drift: Option<String>,
    #[schema(value_type = String, format = DateTime)]
    pub checked_at: DateTime<Utc>,
}

impl From<ArchiveTargetTestOutcome> for ArchiveTargetTestResponse {
    fn from(outcome: ArchiveTargetTestOutcome) -> Self {
        Self {
            ok: outcome.ok,
            s3_source_id: outcome.s3_source_id,
            s3_source_name: outcome.s3_source_name,
            endpoint: outcome.endpoint,
            scheme: outcome.scheme,
            force_path_style: outcome.force_path_style,
            path_style_reason: outcome.path_style_reason,
            message: outcome.message,
            live_settings_drift: outcome.live_settings_drift,
            checked_at: outcome.checked_at,
        }
    }
}

impl From<ArchiveTargetTestError> for Problem {
    fn from(error: ArchiveTargetTestError) -> Self {
        match error {
            ArchiveTargetTestError::ServiceNotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Service Not Found")
                    .with_detail(error.to_string())
            }
            ArchiveTargetTestError::UnsupportedEngine { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Not a PostgreSQL Service")
                    .with_detail(error.to_string())
            }
            ArchiveTargetTestError::Cluster { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("No Continuous Archiving On Clusters")
                .with_detail(error.to_string()),
            ArchiveTargetTestError::LocalWorkloadsDisabled { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Local Workloads Disabled")
                    .with_detail(error.to_string())
            }
            ArchiveTargetTestError::NotConfigured { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("WAL Archiving Not Configured")
                    // The backups section of the service page, where a
                    // backup turns archiving on.
                    .with_value("setup_path", "#backups")
                    .with_detail(error.to_string())
            }
            ArchiveTargetTestError::SourceNotFound { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Archive Target Missing")
                    .with_detail(error.to_string())
            }
            ArchiveTargetTestError::WorkerNode { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Archive Target Check Not Available On Worker Nodes")
                .with_detail(error.to_string()),
            ArchiveTargetTestError::Decryption { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Archive Target Credentials Unreadable")
                    .with_detail(error.to_string())
            }
            ArchiveTargetTestError::Database { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Database Error")
                    .with_detail(error.to_string())
            }
            ArchiveTargetTestError::Engine { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Archive Target Check Could Not Run")
                    .with_detail(error.to_string())
            }
        }
    }
}

/// An operator ran the archive target check, which writes a probe object to
/// the service's WAL-G prefix and tries to remove it. Records the outcome and the
/// settings checked; never a credential.
#[derive(Debug, Clone, Serialize)]
pub struct ExternalServiceArchiveTargetTestedAudit {
    pub context: AuditContext,
    pub service_id: i32,
    pub s3_source_id: i32,
    pub ok: bool,
    pub endpoint: String,
    pub scheme: String,
    pub force_path_style: bool,
}

impl AuditOperation for ExternalServiceArchiveTargetTestedAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_SERVICE_ARCHIVE_TARGET_TESTED".to_string()
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

    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
    }
}

#[utoipa::path(
    tag = "External Services",
    post,
    path = "/external-services/{id}/wal-archive/test",
    params(("id" = i32, Path, description = "External service ID")),
    responses(
        (status = 200, description = "The check ran; `ok` says whether WAL-G could write to the archive target", body = ArchiveTargetTestResponse),
        (status = 400, description = "The service is not PostgreSQL", body = temps_core::problemdetails::ProblemDetails),
        (status = 401, description = "Unauthorized", body = temps_core::problemdetails::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::problemdetails::ProblemDetails),
        (status = 404, description = "Service not found", body = temps_core::problemdetails::ProblemDetails),
        (status = 409, description = "Archiving is not set up, its S3 source is gone, the service runs on a worker node, it is a cluster (no continuous archiving), or local workloads are disabled", body = temps_core::problemdetails::ProblemDetails),
        (status = 500, description = "The check could not run", body = temps_core::problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn test_wal_archive_target(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
) -> Result<impl IntoResponse, Problem> {
    // The check writes (and tries to delete) an object in the service's
    // archive.
    permission_guard!(auth, ExternalServicesWrite);
    super::metrics_handlers::assert_service_permission_for_caller(
        id,
        &auth,
        &app_state,
        temps_auth::Permission::ExternalServicesWrite,
    )
    .await?;
    let outcome = app_state
        .external_service_manager
        .test_wal_archive_target(id)
        .await?;
    Ok(audited_response(&app_state, &auth, &metadata, id, outcome).await)
}

/// Audit a completed check, passed or refused, and build its response.
async fn audited_response(
    app_state: &AppState,
    auth: &temps_auth::AuthContext,
    metadata: &RequestMetadata,
    id: i32,
    outcome: ArchiveTargetTestOutcome,
) -> Json<ArchiveTargetTestResponse> {
    let audit = ExternalServiceArchiveTargetTestedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        service_id: id,
        s3_source_id: outcome.s3_source_id,
        ok: outcome.ok,
        endpoint: outcome.endpoint.clone(),
        scheme: outcome.scheme.clone(),
        force_path_style: outcome.force_path_style,
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!(
            service_id = id,
            "Failed to create audit log for a WAL archive target check: {}", e
        );
    }
    Json(ArchiveTargetTestResponse::from(outcome))
}

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new().route(
        "/external-services/{id}/wal-archive/test",
        post(test_wal_archive_target),
    )
}

#[derive(OpenApi)]
#[openapi(
    paths(test_wal_archive_target),
    components(schemas(ArchiveTargetTestResponse))
)]
pub struct ArchiveTargetApiDoc;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::sync::Mutex;
    use temps_auth::AuthContext;
    use temps_entities::{external_services, project_services};
    use tower::ServiceExt;

    #[derive(Clone, Default)]
    struct RecordingAuditLogger {
        operations: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl temps_core::AuditLogger for RecordingAuditLogger {
        async fn create_audit_log(
            &self,
            operation: &dyn temps_core::AuditOperation,
        ) -> temps_core::AuditResult<()> {
            self.operations
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(operation.operation_type());
            Ok(())
        }
    }

    fn auth(role: temps_auth::Role) -> AuthContext {
        let now = Utc::now();
        let user = temps_entities::users::Model {
            id: 42,
            name: "Database Operator".to_string(),
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
            user_agent: "archive-target-test".to_string(),
            headers: Default::default(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "https://temps.example.test".to_string(),
            scheme: "https".to_string(),
            host: "temps.example.test".to_string(),
            is_secure: true,
        }
    }

    fn service(service_type: &str) -> external_services::Model {
        let mut model = crate::externalsvc::test_utils::create_mock_external_service(
            "orders".to_string(),
            service_type,
            "18",
        );
        model.id = 7;
        model.node_id = None;
        model.continuous_archive_s3_source_id = None;
        model
    }

    fn state(db: MockDatabase, audit: RecordingAuditLogger) -> Arc<AppState> {
        state_with_project_roles(db, audit, None)
    }

    fn state_with_project_roles(
        db: MockDatabase,
        audit: RecordingAuditLogger,
        project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
    ) -> Arc<AppState> {
        state_with_policy(db, audit, project_access_checker, true)
    }

    fn state_with_policy(
        db: MockDatabase,
        audit: RecordingAuditLogger,
        project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
        local_workloads_enabled: bool,
    ) -> Arc<AppState> {
        let db = Arc::new(db.into_connection());
        let encryption = Arc::new(temps_core::EncryptionService::new_from_password(
            "archive-target-handler-test",
        ));
        let docker = Arc::new(temps_core::DockerHandle::disabled(
            "control-plane",
            "archive target handler test",
        ));
        let manager = Arc::new(crate::services::ExternalServiceManager::new_with_handle(
            db.clone(),
            encryption,
            docker,
            local_workloads_enabled,
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
            project_access_checker,
            application_network_reconciler: None,
            notification_service: Arc::new(crate::handlers::types::StaticNotificationService {
                destinations: Some(0),
            }),
        })
    }

    /// The ownership check, then the service lookup of the check itself.
    fn owned_then_loaded(service: &external_services::Model) -> MockDatabase {
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![service.clone()]])
            .append_query_results([Vec::<project_services::Model>::new()])
            .append_query_results([vec![service.clone()]])
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

    #[tokio::test]
    async fn the_route_requires_authentication() {
        let app = configure_routes().with_state(state(
            MockDatabase::new(DatabaseBackend::Postgres),
            RecordingAuditLogger::default(),
        ));
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/external-services/7/wal-archive/test")
            .extension(metadata())
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn read_only_roles_cannot_run_the_check() {
        for role in [temps_auth::Role::Reader, temps_auth::Role::ApiReader] {
            let audit = RecordingAuditLogger::default();
            // No query results: any database access would be a 500, so a
            // 403 shows the refusal comes first.
            let app = state(MockDatabase::new(DatabaseBackend::Postgres), audit.clone());
            let (status, body) = body_json(
                test_wal_archive_target(
                    RequireAuth(auth(role.clone())),
                    State(app),
                    Path(7),
                    Extension(metadata()),
                )
                .await,
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{role:?}");
            assert!(body["detail"].is_string(), "Problem Details body: {body}");
            assert!(audit.operations.lock().unwrap().is_empty());
        }
    }

    /// Scoped project roles: a member of every project whose role there
    /// grants only reading external services.
    struct ReadOnlyProjectRole;

    #[async_trait::async_trait]
    impl temps_core::ProjectAccessChecker for ReadOnlyProjectRole {
        async fn user_can_access_project(
            &self,
            _user_id: i32,
            _project_id: i32,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            Ok(true)
        }

        async fn effective_project_permissions(
            &self,
            _user_id: i32,
            _project_id: i32,
        ) -> Result<Option<Vec<String>>, Box<dyn std::error::Error + Send + Sync>> {
            Ok(Some(vec![
                temps_auth::Permission::ExternalServicesRead.to_string()
            ]))
        }
    }

    #[tokio::test]
    async fn a_read_only_role_in_the_linked_project_cannot_run_the_check() {
        let audit = RecordingAuditLogger::default();
        let svc = service("postgres");
        let link = project_services::Model {
            id: 1,
            project_id: 99,
            service_id: 7,
            database_provisioning_mode: "shared".into(),
            custom_database_name: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        // Only the ownership lookups: loading the service for the check
        // would run out of mock results and fail with a 500, not a 403.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![svc]])
            .append_query_results([vec![link]]);
        let app = state_with_project_roles(db, audit.clone(), Some(Arc::new(ReadOnlyProjectRole)));
        // An instance User holds ExternalServicesWrite and passes the
        // instance-level guard; the project role must still refuse.
        let (status, body) = body_json(
            test_wal_archive_target(
                RequireAuth(auth(temps_auth::Role::User)),
                State(app),
                Path(7),
                Extension(metadata()),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert!(body["detail"].is_string(), "Problem Details body: {body}");
        assert!(audit.operations.lock().unwrap().is_empty());
    }

    fn checked_outcome(ok: bool) -> ArchiveTargetTestOutcome {
        ArchiveTargetTestOutcome {
            ok,
            s3_source_id: 3,
            s3_source_name: "archive".to_string(),
            endpoint: "http://10.0.0.5:9000".to_string(),
            scheme: "http".to_string(),
            force_path_style: true,
            path_style_reason: "Forced: the endpoint is an IP address".to_string(),
            message: if ok {
                "WAL-G wrote a probe object to the archive target".to_string()
            } else {
                "server gave HTTP response to HTTPS client".to_string()
            },
            live_settings_drift: None,
            checked_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn a_completed_check_is_audited_and_returned_passed_or_refused() {
        for ok in [true, false] {
            let audit = RecordingAuditLogger::default();
            let app = state(MockDatabase::new(DatabaseBackend::Postgres), audit.clone());
            let response = audited_response(
                &app,
                &auth(temps_auth::Role::Admin),
                &metadata(),
                7,
                checked_outcome(ok),
            )
            .await
            .into_response();
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["ok"], ok);
            assert_eq!(body["endpoint"], "http://10.0.0.5:9000");
            assert_eq!(body["scheme"], "http");
            assert_eq!(body["force_path_style"], true);
            assert_eq!(body["s3_source_id"], 3);
            assert!(body["live_settings_drift"].is_null());
            assert!(body["checked_at"].as_str().unwrap().ends_with('Z'));
            assert_eq!(
                *audit.operations.lock().unwrap(),
                vec!["EXTERNAL_SERVICE_ARCHIVE_TARGET_TESTED".to_string()]
            );
        }
    }

    #[tokio::test]
    async fn without_docker_the_check_is_a_500_and_nothing_is_audited() {
        let audit = RecordingAuditLogger::default();
        let mut svc = service("postgres");
        svc.continuous_archive_s3_source_id = Some(3);
        let encryption =
            temps_core::EncryptionService::new_from_password("archive-target-handler-test");
        let now = Utc::now();
        let source = temps_entities::s3_sources::Model {
            id: 3,
            name: "archive".to_string(),
            bucket_name: "archive".to_string(),
            region: "us-east-1".to_string(),
            endpoint: Some("http://10.0.0.5:9000".to_string()),
            bucket_path: "pg".to_string(),
            access_key_id: encryption.encrypt_string("test-access").unwrap(),
            secret_key: encryption.encrypt_string("test-secret").unwrap(),
            session_token: None,
            credentials_expire_at: None,
            force_path_style: None,
            is_default: false,
            managed_by_cloud: false,
            lifecycle_reconcile_failed_at: None,
            lifecycle_reconcile_generation: 0,
            created_at: now,
            updated_at: now,
            backing_service_id: None,
            worker_credentials: temps_entities::s3_sources::WORKER_CREDENTIALS_SCOPED.to_string(),
        };
        let db = owned_then_loaded(&svc).append_query_results([vec![source]]);
        let app = state(db, audit.clone());
        let (status, body) = body_json(
            test_wal_archive_target(
                RequireAuth(auth(temps_auth::Role::Admin)),
                State(app),
                Path(7),
                Extension(metadata()),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        assert!(
            body["detail"].as_str().unwrap().contains("service 7"),
            "{body}"
        );
        assert!(audit.operations.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_service_without_archiving_says_how_to_set_it_up() {
        let audit = RecordingAuditLogger::default();
        let app = state(owned_then_loaded(&service("postgres")), audit.clone());
        let (status, body) = body_json(
            test_wal_archive_target(
                RequireAuth(auth(temps_auth::Role::Admin)),
                State(app),
                Path(7),
                Extension(metadata()),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["title"], "WAL Archiving Not Configured");
        assert!(body["detail"].as_str().unwrap().contains("run a backup"));
        assert_eq!(body["setup_path"], "#backups");
        // Nothing was checked, so nothing is audited.
        assert!(audit.operations.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_cluster_is_refused_before_any_container_is_touched() {
        let audit = RecordingAuditLogger::default();
        let mut cluster = service("postgres");
        cluster.topology = "cluster".to_string();
        // Even with a pinned source, the check never reaches the source,
        // Docker or a container.
        cluster.continuous_archive_s3_source_id = Some(3);
        let app = state(owned_then_loaded(&cluster), audit.clone());
        let (status, body) = body_json(
            test_wal_archive_target(
                RequireAuth(auth(temps_auth::Role::Admin)),
                State(app),
                Path(7),
                Extension(metadata()),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["title"], "No Continuous Archiving On Clusters");
        assert!(body["detail"]
            .as_str()
            .unwrap()
            .contains("service 7 is a cluster"));
        assert!(audit.operations.lock().unwrap().is_empty());
    }

    /// With local workloads disabled (and no Docker either), the operator
    /// sees the policy, not a capability error.
    #[tokio::test]
    async fn a_profile_without_local_workloads_is_refused_as_policy() {
        let audit = RecordingAuditLogger::default();
        let mut svc = service("postgres");
        svc.continuous_archive_s3_source_id = Some(3);
        let app = state_with_policy(owned_then_loaded(&svc), audit.clone(), None, false);
        let (status, body) = body_json(
            test_wal_archive_target(
                RequireAuth(auth(temps_auth::Role::Admin)),
                State(app),
                Path(7),
                Extension(metadata()),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["title"], "Local Workloads Disabled");
        assert!(body["detail"]
            .as_str()
            .unwrap()
            .contains("local workloads disabled"));
        assert!(audit.operations.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_non_postgres_service_is_a_bad_request() {
        let app = state(
            owned_then_loaded(&service("redis")),
            RecordingAuditLogger::default(),
        );
        let (status, body) = body_json(
            test_wal_archive_target(
                RequireAuth(auth(temps_auth::Role::Admin)),
                State(app),
                Path(7),
                Extension(metadata()),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body["detail"].as_str().unwrap().contains("'redis'"));
    }

    #[test]
    fn every_error_maps_to_a_problem_with_a_detail() {
        let cases = [
            (
                ArchiveTargetTestError::ServiceNotFound { service_id: 7 },
                StatusCode::NOT_FOUND,
            ),
            (
                ArchiveTargetTestError::WorkerNode {
                    service_id: 7,
                    node_id: 2,
                },
                StatusCode::CONFLICT,
            ),
            (
                ArchiveTargetTestError::Cluster { service_id: 7 },
                StatusCode::CONFLICT,
            ),
            (
                ArchiveTargetTestError::LocalWorkloadsDisabled { service_id: 7 },
                StatusCode::CONFLICT,
            ),
            (
                ArchiveTargetTestError::SourceNotFound {
                    service_id: 7,
                    s3_source_id: 3,
                },
                StatusCode::CONFLICT,
            ),
            (
                ArchiveTargetTestError::Engine {
                    service_id: 7,
                    reason: "Docker unavailable".to_string(),
                },
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];
        for (error, expected) in cases {
            let detail = error.to_string();
            let response = Problem::from(error).into_response();
            assert_eq!(response.status(), expected, "{detail}");
        }
    }

    #[test]
    fn the_audit_entry_carries_no_credential() {
        let audit = ExternalServiceArchiveTargetTestedAudit {
            context: AuditContext {
                user_id: 42,
                ip_address: None,
                user_agent: "test".to_string(),
            },
            service_id: 7,
            s3_source_id: 3,
            ok: false,
            endpoint: "http://10.0.0.5:9000".to_string(),
            scheme: "http".to_string(),
            force_path_style: true,
        };
        let body = AuditOperation::serialize(&audit).unwrap();
        assert!(body.contains("http://10.0.0.5:9000"));
        assert!(!body.contains("secret"));
        assert_eq!(
            audit.operation_type(),
            "EXTERNAL_SERVICE_ARCHIVE_TARGET_TESTED"
        );
    }
}
