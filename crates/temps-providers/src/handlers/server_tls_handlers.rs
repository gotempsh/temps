// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Server-side TLS of a managed database (ADR 093).
//!
//! ```text
//! GET /external-services/{id}/tls                 effective status
//! PUT /external-services/{id}/tls                 change the mode (audited)
//! GET /external-services/{id}/tls/ca-certificate  the managed-services CA (public)
//! ```
//!
//! No response ever contains a private key: the status carries fingerprints
//! and validity, and the CA download is the public certificate only.

use std::sync::Arc;

use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::error_builder::ErrorBuilder;
use temps_core::problemdetails::Problem;
use temps_core::{AuditContext, RequestMetadata};
use temps_providers_core::externalsvc::server_tls::ServerTlsMode;
use tracing::{error, info};
use utoipa::{OpenApi, ToSchema};

use super::audit::ExternalServiceTlsModeChangedAudit;
use crate::handlers::types::AppState;
use crate::managed_tls::{
    AuthorityCertificate, AuthorityInfo, ContainerTlsState, IssuedCertificateInfo, ServerTlsError,
    ServiceTlsStatus,
};

/// The certificate currently issued for a service. Public facts only.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ServiceTlsCertificateResponse {
    /// SHA-256 (lowercase hex) of the certificate's DER encoding.
    pub fingerprint_sha256: String,
    /// Names the certificate is valid for.
    pub subject_alt_names: Vec<String>,
    #[schema(value_type = String, format = DateTime)]
    pub not_before: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub not_after: DateTime<Utc>,
}

impl From<IssuedCertificateInfo> for ServiceTlsCertificateResponse {
    fn from(info: IssuedCertificateInfo) -> Self {
        Self {
            fingerprint_sha256: info.fingerprint_sha256,
            subject_alt_names: info.subject_alt_names,
            not_before: info.not_before,
            not_after: info.not_after,
        }
    }
}

/// The managed-services CA. Public facts only.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManagedCaInfoResponse {
    /// SHA-256 (lowercase hex) of the CA certificate's DER encoding: compare
    /// it with the downloaded file before trusting it.
    pub fingerprint_sha256: String,
    #[schema(value_type = String, format = DateTime)]
    pub not_before: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub not_after: DateTime<Utc>,
}

impl From<AuthorityInfo> for ManagedCaInfoResponse {
    fn from(info: AuthorityInfo) -> Self {
        Self {
            fingerprint_sha256: info.fingerprint_sha256,
            not_before: info.not_before,
            not_after: info.not_after,
        }
    }
}

/// What one data node of a cluster serves.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ServiceTlsMemberResponse {
    pub container_name: String,
    /// The member's lifecycle status (`running`, `stopped`, ...).
    pub status: String,
    /// Whether it serves the managed certificate. `null` when it could not
    /// be read (see `detail`).
    pub active: Option<bool>,
    /// Whether it still serves pg_auto_failover's self-signed certificate,
    /// which clients cannot verify.
    pub self_signed: bool,
    /// Whether it lags the setting; the next start applies it.
    pub pending: bool,
    /// Why its state is unknown, when it is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// A service's effective server TLS status.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ServiceTlsStatusResponse {
    pub service_id: i32,
    /// `off`, `available` (TLS offered, plaintext still accepted) or
    /// `required` (plaintext rejected; not yet supported).
    pub mode: ServerTlsMode,
    /// Whether this service can use managed TLS at all.
    pub supported: bool,
    /// Why not, when `supported` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unsupported_reason: Option<String>,
    /// Whether the server accepts unencrypted connections in this mode.
    pub accepts_plaintext: bool,
    /// Whether the managed certificate is served: by the container, or by
    /// every data node of a cluster. `null` when it cannot be inspected (not
    /// created yet, no Docker daemon here, or cluster members disagree).
    pub active: Option<bool>,
    /// What runs lags the setting; the next start (or a restart) applies it.
    pub pending_restart: bool,
    /// Whether changing the mode restarts the database. False for a
    /// cluster, whose data nodes switch certificates in place.
    pub change_restarts_database: bool,
    /// A cluster's data nodes; empty for a standalone service.
    pub members: Vec<ServiceTlsMemberResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certificate: Option<ServiceTlsCertificateResponse>,
    /// Absent until TLS is first enabled on any service.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certificate_authority: Option<ManagedCaInfoResponse>,
    /// Where to download the CA certificate, once it exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ca_certificate_path: Option<String>,
}

impl From<ServiceTlsStatus> for ServiceTlsStatusResponse {
    fn from(status: ServiceTlsStatus) -> Self {
        let pending_restart = status.pending_restart();
        let active = status.container.serves_managed_certificate();
        let issued = status
            .certificate
            .as_ref()
            .map(|c| c.fingerprint_sha256.clone());
        let members = status
            .members
            .iter()
            .map(|member| ServiceTlsMemberResponse {
                container_name: member.container_name.clone(),
                status: member.status.clone(),
                active: member.tls.serves_managed_certificate(),
                self_signed: matches!(member.tls, ContainerTlsState::Unmanaged { .. }),
                pending: member.tls.lags(status.mode, issued.as_deref()),
                detail: member.detail.clone(),
            })
            .collect();
        let ca_certificate_path = status.authority.as_ref().map(|_| {
            format!(
                "/external-services/{}/tls/ca-certificate",
                status.service_id
            )
        });
        Self {
            service_id: status.service_id,
            mode: status.mode,
            supported: status.unsupported_reason.is_none(),
            unsupported_reason: status.unsupported_reason,
            accepts_plaintext: status.mode.accepts_plaintext(),
            active,
            pending_restart,
            change_restarts_database: !status.cluster,
            members,
            certificate: status.certificate.map(Into::into),
            certificate_authority: status.authority.map(Into::into),
            ca_certificate_path,
        }
    }
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateServiceTlsRequest {
    /// The new mode. `required` is rejected until it is supported.
    pub mode: ServerTlsMode,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UpdateServiceTlsResponse {
    pub previous_mode: ServerTlsMode,
    pub tls: ServiceTlsStatusResponse,
}

/// The managed-services CA certificate, to add to a client's trust store.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManagedCaCertificateResponse {
    /// PEM-encoded CA certificate. Public material; the key never leaves
    /// the control plane.
    pub certificate_pem: String,
    /// Suggested file name for the download.
    pub file_name: String,
    pub fingerprint_sha256: String,
    #[schema(value_type = String, format = DateTime)]
    pub not_before: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub not_after: DateTime<Utc>,
}

impl From<AuthorityCertificate> for ManagedCaCertificateResponse {
    fn from(ca: AuthorityCertificate) -> Self {
        Self {
            certificate_pem: ca.certificate_pem,
            file_name: "temps-managed-services-ca.pem".to_string(),
            fingerprint_sha256: ca.info.fingerprint_sha256,
            not_before: ca.info.not_before,
            not_after: ca.info.not_after,
        }
    }
}

impl From<ServerTlsError> for Problem {
    fn from(error: ServerTlsError) -> Self {
        let detail = error.to_string();
        match error {
            ServerTlsError::NotFound { service_id } => ErrorBuilder::new(StatusCode::NOT_FOUND)
                .title("External Service Not Found")
                .detail(detail)
                .value("service_id", service_id)
                .build(),
            ServerTlsError::Unsupported { service_id, .. } => {
                ErrorBuilder::new(StatusCode::BAD_REQUEST)
                    .title("Server TLS Not Supported For This Service")
                    .detail(detail)
                    .value("service_id", service_id)
                    .build()
            }
            ServerTlsError::ModeNotYetSupported { service_id, mode } => {
                ErrorBuilder::new(StatusCode::BAD_REQUEST)
                    .title("Server TLS Mode Not Yet Supported")
                    .detail(detail)
                    .value("service_id", service_id)
                    .value("mode", mode.as_str())
                    .build()
            }
            ServerTlsError::NoAuthority { service_id } => ErrorBuilder::new(StatusCode::NOT_FOUND)
                .title("No Managed Services CA Yet")
                .detail(detail)
                .value("service_id", service_id)
                .build(),
            ServerTlsError::Busy {
                service_id, status, ..
            } => ErrorBuilder::new(StatusCode::CONFLICT)
                .title("Service Is Starting")
                .detail(detail)
                .value("service_id", service_id)
                .value("status", status)
                .build(),
            ServerTlsError::Apply {
                service_id, source, ..
            } => {
                let problem =
                    super::handlers::upgrade_error_problem(&source).unwrap_or_else(|| {
                        super::handlers::external_service_problem(&source, detail.clone())
                    });
                problem.with_value("service_id", service_id)
            }
            ServerTlsError::NotApplied {
                service_id, mode, ..
            } => ErrorBuilder::new(StatusCode::CONFLICT)
                .title("Server TLS Not Applied")
                .detail(detail)
                .value("service_id", service_id)
                .value("mode", mode.as_str())
                .build(),
            ServerTlsError::ClusterApply {
                service_id, mode, ..
            } => ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Server TLS Not Applied To The Cluster")
                .detail(detail)
                .value("service_id", service_id)
                .value("mode", mode.as_str())
                .build(),
            ServerTlsError::ChangeAborted {
                service_id, mode, ..
            } => ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Server TLS Change Did Not Finish")
                .detail(detail)
                .value("service_id", service_id)
                .value("mode", mode.as_str())
                .build(),
            ServerTlsError::Service(source) => super::handlers::upgrade_error_problem(&source)
                .unwrap_or_else(|| super::handlers::external_service_problem(&source, detail)),
            ServerTlsError::Storage(_) => ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Server TLS Storage Failed")
                .detail(detail)
                .build(),
        }
    }
}

#[utoipa::path(
    tag = "External Services",
    get,
    path = "/external-services/{id}/tls",
    params(("id" = i32, Path, description = "External service ID")),
    responses(
        (status = 200, description = "Effective server TLS status", body = ServiceTlsStatusResponse),
        (status = 401, description = "Unauthorized", body = temps_core::problemdetails::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::problemdetails::ProblemDetails),
        (status = 404, description = "Service not found", body = temps_core::problemdetails::ProblemDetails),
        (status = 500, description = "Internal server error", body = temps_core::problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_service_tls(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);
    super::metrics_handlers::assert_service_owned_by_caller(id, &auth, &app_state).await?;
    let status = app_state
        .external_service_manager
        .get_service_tls_status(id)
        .await?;
    Ok(Json(ServiceTlsStatusResponse::from(status)))
}

#[utoipa::path(
    tag = "External Services",
    put,
    path = "/external-services/{id}/tls",
    params(("id" = i32, Path, description = "External service ID")),
    request_body = UpdateServiceTlsRequest,
    responses(
        (status = 200, description = "Mode changed and applied (a running database is restarted)", body = UpdateServiceTlsResponse),
        (status = 400, description = "The service cannot use this mode", body = temps_core::problemdetails::ProblemDetails),
        (status = 401, description = "Unauthorized", body = temps_core::problemdetails::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::problemdetails::ProblemDetails),
        (status = 404, description = "Service not found", body = temps_core::problemdetails::ProblemDetails),
        (status = 409, description = "A major upgrade is in progress, the service is still being created or starting, or the recreate guard kept its existing container so the mode was not applied", body = temps_core::problemdetails::ProblemDetails),
        (status = 500, description = "Applying the mode failed; the previous mode was kept", body = temps_core::problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_service_tls(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<UpdateServiceTlsRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesWrite);
    // Membership of a linked project is not enough to change how its
    // database accepts connections: the caller's role in that project must
    // grant the write as well.
    super::metrics_handlers::assert_service_permission_for_caller(
        id,
        &auth,
        &app_state,
        temps_auth::Permission::ExternalServicesWrite,
    )
    .await?;
    let (previous, status) = app_state
        .external_service_manager
        .set_service_tls_mode(id, request.mode)
        .await?;

    if previous != status.mode {
        let service = app_state.external_service_manager.get_service(id).await;
        let (name, service_type) = match service {
            Ok(service) => (service.name, service.service_type),
            Err(_) => (String::new(), String::new()),
        };
        let audit = ExternalServiceTlsModeChangedAudit {
            context: AuditContext {
                user_id: auth.user_id(),
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            service_id: id,
            name,
            service_type,
            previous_mode: previous,
            new_mode: status.mode,
        };
        if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
            error!(
                service_id = id,
                "Failed to create audit log for a server TLS mode change: {}", e
            );
        }
        info!(
            service_id = id,
            previous_mode = %previous,
            mode = %status.mode,
            "Server TLS mode of external service changed"
        );
    }

    Ok(Json(UpdateServiceTlsResponse {
        previous_mode: previous,
        tls: status.into(),
    }))
}

#[utoipa::path(
    tag = "External Services",
    get,
    path = "/external-services/{id}/tls/ca-certificate",
    params(("id" = i32, Path, description = "External service ID")),
    responses(
        (status = 200, description = "The managed-services CA certificate (public)", body = ManagedCaCertificateResponse),
        (status = 401, description = "Unauthorized", body = temps_core::problemdetails::ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = temps_core::problemdetails::ProblemDetails),
        (status = 404, description = "Service not found, or no CA exists yet", body = temps_core::problemdetails::ProblemDetails),
        (status = 500, description = "Internal server error", body = temps_core::problemdetails::ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_managed_ca_certificate(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<AppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, ExternalServicesRead);
    super::metrics_handlers::assert_service_owned_by_caller(id, &auth, &app_state).await?;
    let ca = app_state
        .external_service_manager
        .get_managed_ca_certificate(id)
        .await?;
    Ok(Json(ManagedCaCertificateResponse::from(ca)))
}

pub fn configure_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/external-services/{id}/tls",
            get(get_service_tls).put(update_service_tls),
        )
        .route(
            "/external-services/{id}/tls/ca-certificate",
            get(get_managed_ca_certificate),
        )
}

#[derive(OpenApi)]
#[openapi(
    paths(get_service_tls, update_service_tls, get_managed_ca_certificate),
    components(schemas(
        ServiceTlsStatusResponse,
        ServiceTlsCertificateResponse,
        ServiceTlsMemberResponse,
        ManagedCaInfoResponse,
        UpdateServiceTlsRequest,
        UpdateServiceTlsResponse,
        ManagedCaCertificateResponse,
        ServerTlsMode,
    ))
)]
pub struct ServerTlsApiDoc;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::sync::Mutex;
    use temps_auth::AuthContext;
    use temps_entities::{
        external_service_tls, external_services, managed_service_certificate_authorities,
        postgres_major_upgrades, project_services,
    };
    use tower::ServiceExt;

    const PASSWORD: &str = "server-tls-handler-test";

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
        let now = chrono::Utc::now();
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
            user_agent: "server-tls-test".to_string(),
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

    fn service(service_type: &str, status: &str) -> external_services::Model {
        let mut model = crate::externalsvc::test_utils::create_mock_external_service(
            "orders".to_string(),
            service_type,
            "18",
        );
        model.id = 7;
        model.status = status.to_string();
        model.config = Some(
            encryption()
                .encrypt_string(r#"{"database":"app","username":"app","password":"pw"}"#)
                .unwrap(),
        );
        model
    }

    fn tls_row(mode: &str) -> external_service_tls::Model {
        external_service_tls::Model {
            service_id: 7,
            mode: mode.into(),
            authority_id: None,
            certificate_pem: None,
            private_key_encrypted: None,
            fingerprint_sha256: None,
            subject_alt_names: None,
            not_before: None,
            not_after: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn ca_row() -> managed_service_certificate_authorities::Model {
        let ca = temps_core::managed_service_pki::generate_managed_services_ca(Utc::now()).unwrap();
        managed_service_certificate_authorities::Model {
            id: 1,
            purpose: "managed-services".into(),
            certificate_pem: ca.cert_pem,
            private_key_encrypted: encryption().encrypt_string(&ca.key_pem).unwrap(),
            fingerprint_sha256: ca.fingerprint_sha256,
            not_before: ca.not_before,
            not_after: ca.not_after,
            created_at: Utc::now(),
        }
    }

    /// The service lookup and the (empty) project links of the ownership
    /// check every route starts with.
    fn owned(db: MockDatabase, service: &external_services::Model) -> MockDatabase {
        db.append_query_results([vec![service.clone()]])
            .append_query_results([Vec::<project_services::Model>::new()])
    }

    fn state(db: MockDatabase, audit: RecordingAuditLogger) -> Arc<AppState> {
        state_with_project_roles(db, audit, None)
    }

    fn state_with_project_roles(
        db: MockDatabase,
        audit: RecordingAuditLogger,
        project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
    ) -> Arc<AppState> {
        let db = Arc::new(db.into_connection());
        let docker = Arc::new(temps_core::DockerHandle::disabled(
            "control-plane",
            "server TLS handler test",
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
            project_access_checker,
            application_network_reconciler: None,
            notification_service: Arc::new(crate::handlers::types::StaticNotificationService {
                destinations: Some(0),
            }),
        })
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
    async fn every_tls_route_requires_authentication() {
        for (method, uri, body) in [
            ("GET", "/external-services/7/tls", None),
            (
                "PUT",
                "/external-services/7/tls",
                Some(r#"{"mode":"available"}"#),
            ),
            ("GET", "/external-services/7/tls/ca-certificate", None),
        ] {
            let app = configure_routes().with_state(state(
                MockDatabase::new(DatabaseBackend::Postgres),
                RecordingAuditLogger::default(),
            ));
            let mut request = axum::http::Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json");
            request = request.extension(metadata());
            let request = request
                .body(body.map(Body::from).unwrap_or_else(Body::empty))
                .unwrap();
            let response = app.oneshot(request).await.unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {uri}"
            );
        }
    }

    #[tokio::test]
    async fn read_only_roles_cannot_change_the_mode() {
        for role in [temps_auth::Role::Reader, temps_auth::Role::ApiReader] {
            let audit = RecordingAuditLogger::default();
            // No query results: any database access would be a 500, so a
            // 403 shows the refusal comes first.
            let app = state(MockDatabase::new(DatabaseBackend::Postgres), audit.clone());
            let (status, body) = body_json(
                update_service_tls(
                    RequireAuth(auth(role.clone())),
                    State(app),
                    Path(7),
                    Extension(metadata()),
                    Json(UpdateServiceTlsRequest {
                        mode: ServerTlsMode::Available,
                    }),
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
    async fn a_read_only_role_in_the_linked_project_cannot_change_the_mode() {
        let audit = RecordingAuditLogger::default();
        let svc = service("postgres", "running");
        let link = project_services::Model {
            id: 1,
            project_id: 99,
            service_id: 7,
            database_provisioning_mode: "shared".into(),
            custom_database_name: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        // Only the ownership lookups: reading or writing the TLS row would
        // run out of mock results and fail with a 500, not a 403.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![svc.clone()]])
            .append_query_results([vec![link]]);
        let app = state_with_project_roles(db, audit.clone(), Some(Arc::new(ReadOnlyProjectRole)));
        // An instance User holds ExternalServicesWrite and passes the
        // instance-level guard; the project role must still refuse.
        let (status, body) = body_json(
            update_service_tls(
                RequireAuth(auth(temps_auth::Role::User)),
                State(app),
                Path(7),
                Extension(metadata()),
                Json(UpdateServiceTlsRequest {
                    mode: ServerTlsMode::Off,
                }),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert!(body["detail"].is_string(), "Problem Details body: {body}");
        assert!(audit.operations.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn status_of_a_service_without_a_tls_record_is_off_with_no_secrets() {
        let svc = service("postgres", "running");
        let db = owned(MockDatabase::new(DatabaseBackend::Postgres), &svc)
            .append_query_results([vec![svc.clone()]])
            .append_query_results([vec![svc.clone()]])
            .append_query_results([Vec::<external_service_tls::Model>::new()])
            .append_query_results([vec![ca_row()]]);
        let app = state(db, RecordingAuditLogger::default());
        let (status, body) = body_json(
            get_service_tls(
                RequireAuth(auth(temps_auth::Role::Admin)),
                State(app),
                Path(7),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["mode"], "off");
        assert_eq!(body["supported"], true);
        assert_eq!(body["accepts_plaintext"], true);
        assert_eq!(body["active"], serde_json::Value::Null);
        assert_eq!(
            body["ca_certificate_path"],
            "/external-services/7/tls/ca-certificate"
        );
        let text = body.to_string();
        assert!(
            !text.contains("PRIVATE KEY") && !text.contains("encrypted"),
            "{text}"
        );
        // ISO 8601 with a Z suffix.
        assert!(body["certificate_authority"]["not_after"]
            .as_str()
            .unwrap()
            .ends_with('Z'));
    }

    #[tokio::test]
    async fn enabling_tls_on_a_stopped_service_records_and_audits_both_modes() {
        let audit = RecordingAuditLogger::default();
        let svc = service("postgres", "stopped");
        let db = owned(MockDatabase::new(DatabaseBackend::Postgres), &svc)
            // set_service_tls_mode: service, parameters, active upgrades
            .append_query_results([vec![svc.clone()]])
            .append_query_results([vec![svc.clone()]])
            .append_query_results([Vec::<postgres_major_upgrades::Model>::new()])
            // current status: no record, no CA
            .append_query_results([Vec::<external_service_tls::Model>::new()])
            .append_query_results([Vec::<managed_service_certificate_authorities::Model>::new()])
            // set_mode: no row, then the inserted row
            .append_query_results([Vec::<external_service_tls::Model>::new()])
            .append_query_results([vec![tls_row("available")]])
            // resulting status
            .append_query_results([vec![tls_row("available")]])
            .append_query_results([Vec::<managed_service_certificate_authorities::Model>::new()])
            // the audit's service lookup
            .append_query_results([vec![svc.clone()]]);
        let app = state(db, audit.clone());
        let (status, body) = body_json(
            update_service_tls(
                RequireAuth(auth(temps_auth::Role::Admin)),
                State(app),
                Path(7),
                Extension(metadata()),
                Json(UpdateServiceTlsRequest {
                    mode: ServerTlsMode::Available,
                }),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["previous_mode"], "off");
        assert_eq!(body["tls"]["mode"], "available");
        assert_eq!(body["tls"]["accepts_plaintext"], true);

        let operations = audit.operations.lock().unwrap().clone();
        assert_eq!(operations.len(), 1, "{operations:?}");
        let (kind, body) = &operations[0];
        assert_eq!(kind, "EXTERNAL_SERVICE_TLS_MODE_CHANGED");
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(body["service_id"], 7);
        assert_eq!(body["previous_mode"], "off");
        assert_eq!(body["new_mode"], "available");
        assert_eq!(body["name"], "orders");
        assert_eq!(body["context"]["user_id"], 42);
    }

    fn member_row(
        ordinal: i32,
        role: &str,
        status: &str,
    ) -> temps_entities::service_members::Model {
        temps_entities::service_members::Model {
            id: ordinal + 1,
            service_id: 7,
            node_id: None,
            role: role.into(),
            container_id: None,
            container_name: format!("postgres-orders-{ordinal}"),
            hostname: None,
            port: Some(5432),
            compute_ip: None,
            status: status.into(),
            ordinal,
            config: None,
            provisioning_step: None,
            provisioning_error: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    /// A stopped cluster records the mode without touching any member; the
    /// status lists its data nodes (not the monitor) and says the change
    /// applies in place.
    #[tokio::test]
    async fn enabling_tls_on_a_stopped_cluster_records_it_for_the_next_start() {
        let audit = RecordingAuditLogger::default();
        let mut svc = service("postgres", "stopped");
        svc.topology = "cluster".into();
        let members = vec![
            member_row(0, "monitor", "stopped"),
            member_row(1, "replica", "stopped"),
            member_row(2, "replica", "stopped"),
        ];
        let db = owned(MockDatabase::new(DatabaseBackend::Postgres), &svc)
            // set_service_tls_mode: service, parameters, active upgrades
            .append_query_results([vec![svc.clone()]])
            .append_query_results([vec![svc.clone()]])
            .append_query_results([Vec::<postgres_major_upgrades::Model>::new()])
            // current status: no record, no CA, the members
            .append_query_results([Vec::<external_service_tls::Model>::new()])
            .append_query_results([Vec::<managed_service_certificate_authorities::Model>::new()])
            .append_query_results([members.clone()])
            // set_mode: no row, then the inserted row
            .append_query_results([Vec::<external_service_tls::Model>::new()])
            .append_query_results([vec![tls_row("available")]])
            // resulting status
            .append_query_results([vec![tls_row("available")]])
            .append_query_results([Vec::<managed_service_certificate_authorities::Model>::new()])
            .append_query_results([members])
            // the audit's service lookup
            .append_query_results([vec![svc.clone()]]);
        let app = state(db, audit.clone());
        let (status, body) = body_json(
            update_service_tls(
                RequireAuth(auth(temps_auth::Role::Admin)),
                State(app),
                Path(7),
                Extension(metadata()),
                Json(UpdateServiceTlsRequest {
                    mode: ServerTlsMode::Available,
                }),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["previous_mode"], "off");
        let tls = &body["tls"];
        assert_eq!(tls["mode"], "available");
        assert_eq!(tls["supported"], true);
        assert_eq!(tls["change_restarts_database"], false);
        assert_eq!(tls["active"], serde_json::Value::Null);
        let names: Vec<&str> = tls["members"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["container_name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["postgres-orders-1", "postgres-orders-2"]);
        assert_eq!(tls["members"][0]["active"], serde_json::Value::Null);
        assert_eq!(tls["members"][0]["detail"], "the member is stopped");
        assert_eq!(tls["members"][0]["pending"], false);
        let operations = audit.operations.lock().unwrap().clone();
        assert_eq!(operations.len(), 1, "{operations:?}");
        assert_eq!(operations[0].0, "EXTERNAL_SERVICE_TLS_MODE_CHANGED");
    }

    #[tokio::test]
    async fn required_is_rejected_as_not_yet_supported() {
        let audit = RecordingAuditLogger::default();
        let svc = service("postgres", "running");
        let db = owned(MockDatabase::new(DatabaseBackend::Postgres), &svc)
            .append_query_results([vec![svc.clone()]]);
        let app = state(db, audit.clone());
        let (status, body) = body_json(
            update_service_tls(
                RequireAuth(auth(temps_auth::Role::Admin)),
                State(app),
                Path(7),
                Extension(metadata()),
                Json(UpdateServiceTlsRequest {
                    mode: ServerTlsMode::Required,
                }),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["mode"], "required");
        assert_eq!(body["service_id"], 7);
        assert!(body["detail"].as_str().unwrap().contains("available"));
        assert!(audit.operations.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_engine_without_managed_tls_is_refused_with_the_reason() {
        let svc = service("redis", "running");
        let db = owned(MockDatabase::new(DatabaseBackend::Postgres), &svc)
            .append_query_results([vec![svc.clone()]])
            .append_query_results([vec![svc.clone()]]);
        let app = state(db, RecordingAuditLogger::default());
        let (status, body) = body_json(
            update_service_tls(
                RequireAuth(auth(temps_auth::Role::Admin)),
                State(app),
                Path(7),
                Extension(metadata()),
                Json(UpdateServiceTlsRequest {
                    mode: ServerTlsMode::Available,
                }),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body["detail"].as_str().unwrap().contains("redis"), "{body}");
    }

    #[tokio::test]
    async fn a_missing_service_is_a_404() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<external_services::Model>::new()]);
        let app = state(db, RecordingAuditLogger::default());
        let (status, _) = body_json(
            get_service_tls(
                RequireAuth(auth(temps_auth::Role::Admin)),
                State(app),
                Path(7),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_ca_download_is_the_public_certificate_only() {
        let svc = service("postgres", "running");
        let ca = ca_row();
        let db = owned(MockDatabase::new(DatabaseBackend::Postgres), &svc)
            .append_query_results([vec![svc.clone()]])
            .append_query_results([vec![ca.clone()]]);
        let app = state(db, RecordingAuditLogger::default());
        let (status, body) = body_json(
            get_managed_ca_certificate(
                RequireAuth(auth(temps_auth::Role::Reader)),
                State(app),
                Path(7),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["certificate_pem"], ca.certificate_pem);
        assert_eq!(body["fingerprint_sha256"], ca.fingerprint_sha256);
        assert_eq!(body["file_name"], "temps-managed-services-ca.pem");
        let text = body.to_string();
        assert!(!text.contains("PRIVATE KEY"), "{text}");
        assert!(!text.contains(&ca.private_key_encrypted), "{text}");
    }

    #[tokio::test]
    async fn the_ca_download_is_a_404_before_any_ca_exists() {
        let svc = service("postgres", "running");
        let db = owned(MockDatabase::new(DatabaseBackend::Postgres), &svc)
            .append_query_results([vec![svc.clone()]])
            .append_query_results([Vec::<managed_service_certificate_authorities::Model>::new()]);
        let app = state(db, RecordingAuditLogger::default());
        let (status, body) = body_json(
            get_managed_ca_certificate(
                RequireAuth(auth(temps_auth::Role::Admin)),
                State(app),
                Path(7),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["detail"].as_str().unwrap().contains("first time TLS"));
    }

    #[test]
    fn every_error_maps_to_its_status() {
        let cases = [
            (
                ServerTlsError::NotFound { service_id: 3 },
                StatusCode::NOT_FOUND,
            ),
            (
                ServerTlsError::Unsupported {
                    service_id: 3,
                    name: "x".into(),
                    reason: "y".into(),
                },
                StatusCode::BAD_REQUEST,
            ),
            (
                ServerTlsError::ModeNotYetSupported {
                    service_id: 3,
                    mode: ServerTlsMode::Required,
                },
                StatusCode::BAD_REQUEST,
            ),
            (
                ServerTlsError::NoAuthority { service_id: 3 },
                StatusCode::NOT_FOUND,
            ),
            (
                ServerTlsError::Busy {
                    service_id: 3,
                    name: "orders".into(),
                    status: "starting".into(),
                },
                StatusCode::CONFLICT,
            ),
            (
                ServerTlsError::Service(crate::services::ExternalServiceError::UpgradeInProgress {
                    id: 3,
                    upgrade_id: 1,
                    phase: "dump".into(),
                }),
                StatusCode::CONFLICT,
            ),
            (
                ServerTlsError::NotApplied {
                    service_id: 3,
                    name: "orders".into(),
                    mode: ServerTlsMode::Available,
                    previous: ServerTlsMode::Off,
                    rollback: "the previous mode is recorded again".into(),
                },
                StatusCode::CONFLICT,
            ),
            (
                ServerTlsError::ClusterApply {
                    service_id: 3,
                    mode: ServerTlsMode::Available,
                    previous: ServerTlsMode::Off,
                    failures: "postgres-orders-1: exit 5".into(),
                },
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                ServerTlsError::ChangeAborted {
                    service_id: 3,
                    mode: ServerTlsMode::Available,
                    reason: "task 12 panicked".into(),
                },
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                ServerTlsError::Storage(crate::managed_tls::ManagedTlsError::AuthorityMissing {
                    purpose: "managed-services".into(),
                }),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];
        for (error, status) in cases {
            let detail = error.to_string();
            assert_eq!(
                Problem::from(error).into_response().status(),
                status,
                "{detail}"
            );
        }
    }
}
