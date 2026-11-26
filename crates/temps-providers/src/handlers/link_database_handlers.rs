// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Change which database a project's link to a service gives its apps.
//!
//! ```text
//! PUT /external-services/{id}/projects/{project_id}/database   (audited)
//! ```
//!
//! After a restore from another server the restored data can sit in a
//! database whose name differs from the one the link computes; pointing the
//! link at it (`custom` mode) makes the apps read it from their next
//! deployment.

use std::sync::Arc;

use axum::{
    extract::{Extension, Path, State},
    response::IntoResponse,
    routing::put,
    Json, Router,
};
use serde::Deserialize;
use temps_auth::RequireAuth;
use temps_auth::{deny_deployment_token, project_permission_guard, project_scope_guard};
use temps_core::error_builder::{bad_request, not_found};
use temps_core::problemdetails::Problem;
use temps_core::{AuditContext, RequestMetadata};
use tracing::{error, info};
use utoipa::{OpenApi, ToSchema};

use super::audit::ExternalServiceProjectDatabaseChangedAudit;
use super::handlers::external_service_problem;
use crate::handlers::types::{AppState, ProjectServiceInfo};
use crate::services::{DatabaseProvisioningConfig, DatabaseProvisioningMode, ExternalServiceError};

/// Which database the link gives the project's apps.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateLinkDatabaseRequest {
    /// `project_environment` (one database per environment), `project` (one
    /// for the whole project) or `custom` (the name below).
    pub database_provisioning_mode: DatabaseProvisioningMode,
    /// The database every environment of the project reads, in `custom` mode.
    #[serde(default)]
    #[schema(example = "shop_prod")]
    pub custom_database_name: Option<String>,
}

/// Change the database a project's apps read from a service
#[utoipa::path(
    put,
    path = "/external-services/{id}/projects/{project_id}/database",
    tag = "External Services",
    request_body = UpdateLinkDatabaseRequest,
    responses(
        (status = 200, description = "The link gives the project this database from its next deployment", body = ProjectServiceInfo),
        (status = 400, description = "Invalid database provisioning configuration"),
        (status = 401, description = "Authentication required"),
        (status = 403, description = "Insufficient permission to change this link"),
        (status = 404, description = "Service, project or link not found"),
        (status = 500, description = "Internal server error")
    ),
    params(
        ("id" = i32, Path, description = "External service ID"),
        ("project_id" = i32, Path, description = "Project ID")
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_link_database(
    State(app_state): State<Arc<AppState>>,
    Path((id, project_id)): Path<(i32, i32)>,
    RequireAuth(auth): RequireAuth,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<UpdateLinkDatabaseRequest>,
) -> Result<impl IntoResponse, Problem> {
    // A deployment token's apps must not repoint their own data.
    deny_deployment_token!(auth);
    project_permission_guard!(
        auth,
        ExternalServicesWrite,
        project_id,
        app_state.project_access_checker
    );
    project_scope_guard!(auth, project_id);

    let provisioning = DatabaseProvisioningConfig {
        mode: request.database_provisioning_mode,
        custom_database_name: request.custom_database_name,
    };
    let info = app_state
        .external_service_manager
        .set_link_database(id, project_id, provisioning)
        .await
        .map_err(|error| match error {
            ExternalServiceError::ServiceNotFound { .. }
            | ExternalServiceError::ProjectNotFound { .. }
            | ExternalServiceError::ServiceNotLinkedToProject { .. } => {
                not_found().detail(error.to_string()).build()
            }
            ExternalServiceError::InvalidDatabaseProvisioning { .. } => {
                bad_request().detail(error.to_string()).build()
            }
            other => external_service_problem(
                &other,
                format!(
                    "Failed to change the database project {project_id} reads from service {id}: {other}"
                ),
            ),
        })?;

    info!(
        service_id = id,
        project_id,
        mode = ?info.database_provisioning_mode,
        custom_database_name = ?info.custom_database_name,
        "Link now gives the project another database"
    );
    let audit = ExternalServiceProjectDatabaseChangedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent,
        },
        service_id: id,
        service_name: info.service.name.clone(),
        project_id,
        database_provisioning_mode: info.database_provisioning_mode,
        custom_database_name: info.custom_database_name.clone(),
    };
    if let Err(error) = app_state.audit_service.create_audit_log(&audit).await {
        error!(service_id = id, project_id, error = %error, "failed to audit the link database change");
    }
    Ok(Json(info))
}

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new().route(
        "/external-services/{id}/projects/{project_id}/database",
        put(update_link_database),
    )
}

#[derive(OpenApi)]
#[openapi(
    paths(update_link_database),
    components(schemas(
        UpdateLinkDatabaseRequest,
        ProjectServiceInfo,
        DatabaseProvisioningMode
    ))
)]
pub struct LinkDatabaseApiDoc;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::StatusCode;
    use chrono::Utc;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
    use std::sync::Mutex;
    use temps_auth::AuthContext;
    use temps_entities::{external_services, project_services, projects};
    use tower::ServiceExt;

    const PASSWORD: &str = "link-database-handler-test";

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
            user_agent: "link-database-test".to_string(),
            headers: Default::default(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "https://temps.example.test".to_string(),
            scheme: "https".to_string(),
            host: "temps.example.test".to_string(),
            is_secure: true,
        }
    }

    fn encryption() -> Arc<temps_core::EncryptionService> {
        Arc::new(temps_core::EncryptionService::new_from_password(PASSWORD))
    }

    fn service() -> external_services::Model {
        let mut model = crate::externalsvc::test_utils::create_mock_external_service(
            "orders".to_string(),
            "postgres",
            "18",
        );
        model.id = 7;
        model
    }

    fn link(mode: &str, custom: Option<&str>) -> project_services::Model {
        project_services::Model {
            id: 3,
            project_id: 10,
            service_id: 7,
            database_provisioning_mode: mode.to_string(),
            custom_database_name: custom.map(str::to_string),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn project() -> projects::Model {
        let mut project = crate::services::environment_preview_project(10);
        project.slug = "shop".to_string();
        project
    }

    fn state(db: MockDatabase, audit: RecordingAuditLogger) -> Arc<AppState> {
        let db = Arc::new(db.into_connection());
        let docker = Arc::new(temps_core::DockerHandle::disabled(
            "control-plane",
            "link database handler test",
        ));
        let manager = Arc::new(crate::services::ExternalServiceManager::new_with_handle(
            db.clone(),
            encryption(),
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

    async fn call(
        app: Arc<AppState>,
        role: temps_auth::Role,
        body: UpdateLinkDatabaseRequest,
    ) -> (StatusCode, serde_json::Value) {
        let response = match update_link_database(
            State(app),
            Path((7, 10)),
            RequireAuth(auth(role)),
            Extension(metadata()),
            Json(body),
        )
        .await
        {
            Ok(response) => response.into_response(),
            Err(problem) => problem.into_response(),
        };
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("body");
        (status, serde_json::from_slice(&bytes).unwrap_or_default())
    }

    fn custom(name: &str) -> UpdateLinkDatabaseRequest {
        UpdateLinkDatabaseRequest {
            database_provisioning_mode: DatabaseProvisioningMode::Custom,
            custom_database_name: Some(name.to_string()),
        }
    }

    #[tokio::test]
    async fn the_route_requires_authentication() {
        let app = configure_routes().with_state(state(
            MockDatabase::new(DatabaseBackend::Postgres),
            RecordingAuditLogger::default(),
        ));
        let request = axum::http::Request::builder()
            .method("PUT")
            .uri("/external-services/7/projects/10/database")
            .header("content-type", "application/json")
            .extension(metadata())
            .body(Body::from(
                r#"{"database_provisioning_mode":"custom","custom_database_name":"shop_prod"}"#,
            ))
            .expect("request");
        let response = app.oneshot(request).await.expect("response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn read_only_roles_cannot_repoint_a_link() {
        for role in [temps_auth::Role::Reader, temps_auth::Role::ApiReader] {
            let audit = RecordingAuditLogger::default();
            // No query results: any database access would be a 500, so a 403
            // shows the refusal comes first.
            let app = state(MockDatabase::new(DatabaseBackend::Postgres), audit.clone());
            let (status, body) = call(app, role.clone(), custom("shop_prod")).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{role:?}: {body}");
            assert!(audit.operations.lock().expect("audit").is_empty());
        }
    }

    #[tokio::test]
    async fn an_invalid_custom_name_is_a_bad_request_naming_it() {
        let audit = RecordingAuditLogger::default();
        let app = state(
            MockDatabase::new(DatabaseBackend::Postgres).append_query_results([vec![service()]]),
            audit.clone(),
        );
        let (status, body) = call(app, temps_auth::Role::User, custom("Shop-Prod")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        let detail = body["detail"].as_str().unwrap_or_default();
        assert!(detail.contains("Shop-Prod"), "{detail}");
        assert!(audit.operations.lock().expect("audit").is_empty());
    }

    #[tokio::test]
    async fn a_project_that_is_not_linked_is_not_found() {
        let audit = RecordingAuditLogger::default();
        let app = state(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![service()]])
                .append_query_results([
                    Vec::<(project_services::Model, Option<projects::Model>)>::new(),
                ]),
            audit.clone(),
        );
        let (status, body) = call(app, temps_auth::Role::User, custom("shop_prod")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        let detail = body["detail"].as_str().unwrap_or_default();
        assert!(
            detail.contains("Service 7") && detail.contains("project 10"),
            "{detail}"
        );
        assert!(audit.operations.lock().expect("audit").is_empty());
    }

    #[tokio::test]
    async fn pointing_a_link_at_a_named_database_saves_and_audits_it() {
        let audit = RecordingAuditLogger::default();
        let app = state(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![service()]])
                .append_query_results([vec![(link("project_environment", None), Some(project()))]])
                .append_query_results([vec![link("custom", Some("shop_prod"))]])
                .append_exec_results([MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }]),
            audit.clone(),
        );
        let (status, body) = call(app, temps_auth::Role::User, custom("shop_prod")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["database_provisioning_mode"], "custom");
        assert_eq!(body["custom_database_name"], "shop_prod");
        assert_eq!(body["project"]["slug"], "shop");

        let operations = audit.operations.lock().expect("audit");
        assert_eq!(operations.len(), 1, "{operations:?}");
        assert_eq!(operations[0].0, "EXTERNAL_SERVICE_PROJECT_DATABASE_CHANGED");
        assert!(operations[0].1.contains("shop_prod"), "{}", operations[0].1);
    }
}
