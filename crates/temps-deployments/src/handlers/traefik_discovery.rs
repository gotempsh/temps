// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Traefik label discovery handlers.
//!
//! Operator-facing HTTP surface over `temps_deployer::traefik_discovery`,
//! which adopts containers Temps did **not** deploy into the route table by
//! reading their `traefik.*` labels.
//!
//! These are host-level operations, not project-scoped ones: the containers in
//! question belong to no project by definition. They are therefore gated on
//! `SettingsRead`/`SettingsWrite` — the same admin-level permissions the node
//! and cluster-DNS admin endpoints use — and never on project membership.
//!
//! Routes are only ever *read* or *suppressed* here. Creating or deleting one
//! through the API would be undone by the next reconciliation pass, so the
//! durable operator control is the `enabled` flag.

use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::http::StatusCode;
use axum::{
    extract::{Path, Query, State},
    routing::{get, patch, post},
    Extension, Json, Router,
};
use serde::Deserialize;
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem, ProblemDetails};
use temps_core::{AuditContext, RequestMetadata};
use tracing::error;
use utoipa::{OpenApi, ToSchema};

use super::audit::{
    TraefikDiscoveredRouteCertDeauthorizedAudit, TraefikDiscoveredRouteCertImportedAudit,
    TraefikDiscoveredRouteCertRequestedAudit, TraefikDiscoveredRouteToggledAudit,
};
use crate::services::traefik_discovery_service::{
    ImportTraefikAcmeJsonRequest, ImportTraefikAcmeJsonResponse, ImportedHostVerdict,
    RequestDiscoveredRouteCertRequest, TraefikDiscoveredRouteListResponse,
    TraefikDiscoveredRouteResponse, TraefikDiscoveryAdminError, TraefikDiscoveryAdminService,
    TraefikDiscoveryConflictResponse, TraefikDiscoverySetupResponse,
    TraefikDiscoveryStatusResponse, TraefikReconciliationResponse, TraefikRouteTlsBlock,
    UpdateTraefikRouteEnabledRequest,
};

/// App state for the Traefik discovery handlers.
pub struct TraefikDiscoveryAppState {
    pub traefik_discovery_service: Arc<TraefikDiscoveryAdminService>,
    /// Audit logger for the one write operation (suppress/restore a route).
    pub audit_service: Arc<dyn temps_core::AuditLogger>,
}

impl From<TraefikDiscoveryAdminError> for Problem {
    fn from(error: TraefikDiscoveryAdminError) -> Self {
        match error {
            TraefikDiscoveryAdminError::NotFound { .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Discovered Route Not Found")
                    .with_detail(error.to_string())
            }
            TraefikDiscoveryAdminError::Validation { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Validation Error")
                    .with_detail(error.to_string())
            }
            TraefikDiscoveryAdminError::HostOwned { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Host Already Owned")
                    .with_detail(error.to_string())
            }
            TraefikDiscoveryAdminError::VerificationMethodConflict { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Verification Method Conflict")
                    .with_detail(error.to_string())
            }
            TraefikDiscoveryAdminError::CertificateValidation { .. } => {
                problemdetails::new(StatusCode::UNPROCESSABLE_ENTITY)
                    .with_title("Certificate Validation Failed")
                    .with_detail(error.to_string())
            }
            TraefikDiscoveryAdminError::Upstream { .. } => {
                problemdetails::new(StatusCode::BAD_GATEWAY)
                    .with_title("TLS Provisioner Error")
                    .with_detail(error.to_string())
            }
            TraefikDiscoveryAdminError::Database { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ListDiscoveredRoutesQuery {
    #[schema(example = 1)]
    pub page: Option<u64>,
    #[schema(example = 20)]
    pub page_size: Option<u64>,
}

/// 1 MiB body cap on the Path B import endpoint — matches the cap stated in
/// ADR-041 §4 and the pattern used in `temps-error-tracking`.
const IMPORT_BODY_LIMIT: usize = 1024 * 1024;

#[derive(OpenApi)]
#[openapi(
    paths(
        get_traefik_discovery_status,
        list_traefik_discovered_routes,
        set_traefik_discovered_route_enabled,
        request_discovered_route_cert,
        deauthorize_discovered_route_cert,
        import_traefik_acme_json,
    ),
    components(schemas(
        TraefikDiscoveryStatusResponse,
        TraefikDiscoverySetupResponse,
        TraefikReconciliationResponse,
        TraefikDiscoveryConflictResponse,
        TraefikDiscoveredRouteResponse,
        TraefikDiscoveredRouteListResponse,
        UpdateTraefikRouteEnabledRequest,
        ListDiscoveredRoutesQuery,
        TraefikRouteTlsBlock,
        RequestDiscoveredRouteCertRequest,
        ImportTraefikAcmeJsonRequest,
        ImportTraefikAcmeJsonResponse,
        ImportedHostVerdict,
    )),
    info(
        title = "Traefik Discovery API",
        description = "Operator API for live Traefik-label route discovery: whether it is \
        enabled on this instance, which containers it adopted, why a labelled container is \
        not being routed, a per-route kill switch, and TLS certificate management for \
        discovered routes (ADR-041).",
        version = "1.0.0"
    )
)]
pub struct TraefikDiscoveryApiDoc;

/// Configure Traefik discovery routes.
pub fn configure_routes() -> Router<Arc<TraefikDiscoveryAppState>> {
    Router::new()
        .route(
            "/traefik-discovery/status",
            get(get_traefik_discovery_status),
        )
        .route(
            "/traefik-discovery/routes",
            get(list_traefik_discovered_routes),
        )
        .route(
            "/traefik-discovery/routes/{host}/enabled",
            patch(set_traefik_discovered_route_enabled),
        )
        .route(
            "/traefik-discovery/routes/{host}/certificate",
            // POST body is a small JSON object (challenge_type + one bool).
            // 8 KiB is generous; it prevents unbounded request bodies that
            // cannot affect the application (ADR-041 §4).
            post(request_discovered_route_cert)
                .layer(DefaultBodyLimit::max(8 * 1024))
                .delete(deauthorize_discovered_route_cert),
        )
        // 1 MiB body limit applied as a route-level layer (ADR-041 §4).
        // Never add a RequestDecompressionLayer to this router — the 1 MiB cap's
        // security properties depend on the absence of decompression here.
        .route(
            "/traefik-discovery/tls/import",
            post(import_traefik_acme_json).layer(DefaultBodyLimit::max(IMPORT_BODY_LIMIT)),
        )
}

/// Whether Traefik label discovery is active on this instance, and how to turn
/// it on when it is not.
#[utoipa::path(
    tag = "Traefik Discovery",
    get,
    path = "/traefik-discovery/status",
    responses(
        (status = 200, description = "Discovery status. `configured: false` means it is not turned on here — the `setup` block says exactly how to turn it on", body = TraefikDiscoveryStatusResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn get_traefik_discovery_status(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<TraefikDiscoveryAppState>>,
) -> Result<Json<TraefikDiscoveryStatusResponse>, Problem> {
    permission_guard!(auth, SettingsRead);

    let status = app_state
        .traefik_discovery_service
        .status()
        .await
        .map_err(|e| {
            error!("Failed to read Traefik discovery status: {}", e);
            Problem::from(e)
        })?;

    Ok(Json(status))
}

/// List every container adopted from Traefik labels, plus the labelled
/// containers that were found and rejected.
#[utoipa::path(
    tag = "Traefik Discovery",
    get,
    path = "/traefik-discovery/routes",
    params(
        ("page" = Option<u64>, Query, description = "Page number (default: 1)"),
        ("page_size" = Option<u64>, Query, description = "Page size (default: 20, max: 100)")
    ),
    responses(
        (status = 200, description = "Discovered routes and unresolved host conflicts", body = TraefikDiscoveredRouteListResponse),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn list_traefik_discovered_routes(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<TraefikDiscoveryAppState>>,
    Query(query): Query<ListDiscoveredRoutesQuery>,
) -> Result<Json<TraefikDiscoveredRouteListResponse>, Problem> {
    permission_guard!(auth, SettingsRead);

    let routes = app_state
        .traefik_discovery_service
        .list_routes(query.page, query.page_size)
        .await
        .map_err(|e| {
            error!("Failed to list Traefik-discovered routes: {}", e);
            Problem::from(e)
        })?;

    Ok(Json(routes))
}

/// Suppress or restore a single discovered route.
///
/// This is a plain column update: the `traefik_discovered_routes` row-level
/// trigger fires `notify_route_table_change()` on an `enabled` change, so the
/// existing `route_table_changes` LISTEN/NOTIFY path reloads this node's route
/// table *and* every other control plane node's. No manual reload here.
#[utoipa::path(
    tag = "Traefik Discovery",
    patch,
    path = "/traefik-discovery/routes/{host}/enabled",
    params(
        ("host" = String, Path, description = "Hostname of the discovered route")
    ),
    request_body = UpdateTraefikRouteEnabledRequest,
    responses(
        (status = 200, description = "Updated discovered route", body = TraefikDiscoveredRouteResponse),
        (status = 400, description = "Validation error", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "No discovered route for that host", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn set_traefik_discovered_route_enabled(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<TraefikDiscoveryAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(host): Path<String>,
    Json(request): Json<UpdateTraefikRouteEnabledRequest>,
) -> Result<Json<TraefikDiscoveredRouteResponse>, Problem> {
    permission_guard!(auth, SettingsWrite);

    let route = app_state
        .traefik_discovery_service
        .set_route_enabled(&host, request.enabled)
        .await
        .map_err(|e| {
            error!(
                "Failed to set enabled={} on Traefik-discovered route '{}': {}",
                request.enabled, host, e
            );
            Problem::from(e)
        })?;

    let audit = TraefikDiscoveredRouteToggledAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        host: route.host.clone(),
        container_name: route.target_container_name.clone(),
        network: route.network.clone(),
        enabled: route.enabled,
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!(
            "Failed to create audit log for Traefik-discovered route '{}' toggle: {}",
            route.host, e
        );
    }

    Ok(Json(route))
}

// ── ADR-041 TLS handlers ─────────────────────────────────────────────────────

/// Request Temps to issue an ACME certificate for a discovered route (Path A).
///
/// The operator explicitly authorizes issuance; `cert_eligible` stays `false`
/// so the container's own labels can never trigger this. Both `SettingsWrite`
/// and `DomainsCreate` are required: `SettingsWrite` is Admin/PlatformAdmin
/// only, so any caller reaching this endpoint is already an administrator.
///
/// The authorization is recorded against the container identity currently
/// serving the host (§2a). Container drift after authorization fires a Critical
/// alarm but does not auto-clear `cert_authorized` — auto-clearing would not
/// remove the certificate and would be a DoS primitive (ADR-041 §2a).
#[utoipa::path(
    tag = "Traefik Discovery",
    post,
    path = "/traefik-discovery/routes/{host}/certificate",
    params(
        ("host" = String, Path, description = "Hostname of the discovered route")
    ),
    request_body = RequestDiscoveredRouteCertRequest,
    responses(
        (status = 201, description = "TLS authorization created and ACME challenge initiated"),
        (status = 400, description = "Validation error (e.g. unsupported challenge_type)", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "No discovered route for that host", body = ProblemDetails),
        (status = 409, description = "Host owned by another resource, or verification_method conflict", body = ProblemDetails),
        (status = 422, description = "Certificate validation failed", body = ProblemDetails),
        (status = 502, description = "TLS provisioner error (ACME upstream failure)", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn request_discovered_route_cert(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<TraefikDiscoveryAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(host): Path<String>,
    Json(request): Json<RequestDiscoveredRouteCertRequest>,
) -> Result<impl axum::response::IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    permission_guard!(auth, DomainsCreate);

    let user_id = auth.user_id();
    let cert_row = app_state
        .traefik_discovery_service
        .authorize_acme_cert(&host, &request, user_id)
        .await
        .map_err(|e| {
            error!(
                "Failed to authorize ACME cert for discovered route '{}': {}",
                host, e
            );
            Problem::from(e)
        })?;

    let audit = TraefikDiscoveredRouteCertRequestedAudit {
        context: AuditContext {
            user_id,
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        host: cert_row.host.clone(),
        container_id: cert_row.authorized_container_id.clone(),
        container_name: cert_row.authorized_container_name.clone(),
        renewal_method: cert_row.renewal_method.clone(),
        dns01_zone: None,
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!(
            "Failed to create audit log for ACME cert request on '{}': {}",
            cert_row.host, e
        );
    }

    Ok(StatusCode::CREATED)
}

/// Remove TLS authorization for a discovered route.
///
/// Clears `cert_authorized` so Temps stops attempting renewal. Does **not**
/// delete the `domains` row or the certificate — deleting live key material as
/// a side effect of deauthorization is the kind of surprise this codebase
/// avoids. Use `DELETE /domains/{host}` to remove the certificate itself.
#[utoipa::path(
    tag = "Traefik Discovery",
    delete,
    path = "/traefik-discovery/routes/{host}/certificate",
    params(
        ("host" = String, Path, description = "Hostname of the discovered route")
    ),
    responses(
        (status = 204, description = "TLS authorization cleared"),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 404, description = "No authorization record for that host", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn deauthorize_discovered_route_cert(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<TraefikDiscoveryAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(host): Path<String>,
) -> Result<impl axum::response::IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    permission_guard!(auth, DomainsCreate);

    // Normalize here so the audit record matches the actual host key used by
    // the service. The service trims and lowercases before querying, so the
    // raw path segment would otherwise diverge from the stored host on
    // mixed-case input (e.g. "App.Example.COM" vs "app.example.com").
    let host = host.trim().to_ascii_lowercase();

    app_state
        .traefik_discovery_service
        .deauthorize_cert(&host)
        .await
        .map_err(|e| {
            error!(
                "Failed to deauthorize cert for discovered route '{}': {}",
                host, e
            );
            Problem::from(e)
        })?;

    let audit = TraefikDiscoveredRouteCertDeauthorizedAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        host: host.clone(),
    };
    if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
        error!(
            "Failed to create audit log for cert deauthorization on '{}': {}",
            host, e
        );
    }

    Ok(StatusCode::NO_CONTENT)
}

/// Import certificates from a Traefik `acme.json` document (Path B).
///
/// Upload the raw contents of Traefik's `acme.json` and a list of hosts to
/// import. Each host is independently validated (8-step chain from ADR-041 §5)
/// and a per-host verdict is returned. `dry_run: true` runs all validation
/// without writing anything, so the operator can preview before committing.
///
/// The request body is capped at 1 MiB. Do **not** add a decompression layer
/// to this route — the 1 MiB cap's security properties depend on the absence
/// of decompression here (ADR-041 §4).
#[utoipa::path(
    tag = "Traefik Discovery",
    post,
    path = "/traefik-discovery/tls/import",
    request_body = ImportTraefikAcmeJsonRequest,
    responses(
        (status = 200, description = "Import results (per-host verdicts)", body = ImportTraefikAcmeJsonResponse),
        (status = 400, description = "Validation error (malformed JSON, unsupported renewal_method)", body = ProblemDetails),
        (status = 401, description = "Unauthorized", body = ProblemDetails),
        (status = 403, description = "Insufficient permissions", body = ProblemDetails),
        (status = 413, description = "Request body exceeds the 1 MiB limit", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails)
    ),
    security(("bearer_auth" = []))
)]
async fn import_traefik_acme_json(
    RequireAuth(auth): RequireAuth,
    State(app_state): State<Arc<TraefikDiscoveryAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<ImportTraefikAcmeJsonRequest>,
) -> Result<Json<ImportTraefikAcmeJsonResponse>, Problem> {
    permission_guard!(auth, SettingsWrite);
    permission_guard!(auth, DomainsCreate);

    let user_id = auth.user_id();
    let response = app_state
        .traefik_discovery_service
        .import_acme_json(&request, user_id)
        .await
        .map_err(|e| {
            error!("Failed to import acme.json: {}", e);
            Problem::from(e)
        })?;

    let imported_hosts: Vec<String> = response
        .verdicts
        .iter()
        .filter(|v| v.success)
        .map(|v| v.host.clone())
        .collect();
    let failed_hosts: Vec<String> = response
        .verdicts
        .iter()
        .filter(|v| !v.success)
        .map(|v| v.host.clone())
        .collect();

    if !request.dry_run {
        let audit = TraefikDiscoveredRouteCertImportedAudit {
            context: AuditContext {
                user_id,
                ip_address: Some(metadata.ip_address.clone()),
                user_agent: metadata.user_agent.clone(),
            },
            imported_hosts,
            failed_hosts,
            entries_parsed: response.total_requested,
        };
        if let Err(e) = app_state.audit_service.create_audit_log(&audit).await {
            error!("Failed to create audit log for acme.json import: {}", e);
        }
    }

    Ok(Json(response))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use sea_orm::{DatabaseBackend, DbErr, MockDatabase, MockExecResult};
    use temps_auth::context::AuthContext;
    use temps_auth::permissions::Role;
    use temps_core::AuditLogger;
    use temps_deployer::traefik_discovery::{TraefikDiscoveryConfig, TraefikDiscoveryHandle};
    use temps_entities::traefik_discovered_routes as discovered;
    use temps_entities::users;

    /// Audit logger that records nothing — the handlers must not depend on a
    /// real audit backend to succeed (audit failures degrade, never fail).
    struct NoopAuditLogger;

    #[async_trait::async_trait]
    impl AuditLogger for NoopAuditLogger {
        async fn create_audit_log(
            &self,
            _operation: &dyn temps_core::AuditOperation,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn test_user() -> users::Model {
        let now = Utc::now();
        users::Model {
            id: 1,
            name: "Test Admin".to_string(),
            email: "admin@example.com".to_string(),
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
        }
    }

    fn user_auth(role: Role) -> RequireAuth {
        RequireAuth(AuthContext::new_session(test_user(), role))
    }

    fn request_metadata() -> Extension<RequestMetadata> {
        Extension(RequestMetadata {
            ip_address: "127.0.0.1".to_string(),
            user_agent: "test-agent".to_string(),
            headers: axum::http::HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "http://localhost:3000".to_string(),
            scheme: "http".to_string(),
            host: "localhost".to_string(),
            is_secure: false,
        })
    }

    fn route_model(host: &str, enabled: bool) -> discovered::Model {
        let now = Utc::now();
        discovered::Model {
            id: 7,
            host: host.to_string(),
            router_name: "app".to_string(),
            target_container_id: "abc123".to_string(),
            target_container_name: "whoami".to_string(),
            target_port: 80,
            target_host_port: None,
            network: "temps".to_string(),
            tls: false,
            enabled,
            last_seen_at: now,
            created_at: now,
            updated_at: now,
        }
    }

    /// Sea-ORM's `.count()` / paginator `num_items()` execute
    /// `SELECT COUNT(*) AS num_items ...` and read the result as a `BigInt`.
    fn count_row(n: i64) -> std::collections::BTreeMap<String, sea_orm::Value> {
        let mut row = std::collections::BTreeMap::new();
        row.insert("num_items".to_string(), sea_orm::Value::BigInt(Some(n)));
        row
    }

    fn state_with(db: sea_orm::DatabaseConnection) -> Arc<TraefikDiscoveryAppState> {
        state_with_handle(
            db,
            TraefikDiscoveryHandle::not_running(
                TraefikDiscoveryConfig::resolve(None, None, "temps"),
                "TEMPS_TRAEFIK_DISCOVERY_ENABLED is not set to 'true'",
            ),
        )
    }

    /// A `DiscoveredHostTlsProvisioner` that always succeeds — used by handler
    /// tests that exercise paths unrelated to TLS provisioning.
    struct NoopProvisioner;

    #[async_trait::async_trait]
    impl crate::services::traefik_discovery_service::DiscoveredHostTlsProvisioner for NoopProvisioner {
        async fn request_acme_cert(
            &self,
            _host: &str,
            _challenge_type: &str,
        ) -> Result<(), crate::services::traefik_discovery_service::TlsProvisionerError> {
            Ok(())
        }

        async fn save_imported_cert(
            &self,
            _host: &str,
            _certificate_pem: &str,
            _key_pem: &str,
            _renewal_method: &str,
            _not_after: chrono::DateTime<chrono::Utc>,
        ) -> Result<i32, crate::services::traefik_discovery_service::TlsProvisionerError> {
            Ok(1)
        }

        async fn dns_zone_is_auto_managed(
            &self,
            _host: &str,
        ) -> Result<bool, crate::services::traefik_discovery_service::TlsProvisionerError> {
            Ok(true)
        }
    }

    fn noop_provisioner(
    ) -> std::sync::Arc<dyn crate::services::traefik_discovery_service::DiscoveredHostTlsProvisioner>
    {
        std::sync::Arc::new(NoopProvisioner)
    }

    fn state_with_handle(
        db: sea_orm::DatabaseConnection,
        handle: TraefikDiscoveryHandle,
    ) -> Arc<TraefikDiscoveryAppState> {
        Arc::new(TraefikDiscoveryAppState {
            traefik_discovery_service: Arc::new(TraefikDiscoveryAdminService::new(
                Arc::new(db),
                Arc::new(handle),
                noop_provisioner(),
            )),
            audit_service: Arc::new(NoopAuditLogger),
        })
    }

    #[tokio::test]
    async fn status_rejects_a_caller_without_settings_read() {
        let state = state_with(MockDatabase::new(DatabaseBackend::Postgres).into_connection());

        let err = get_traefik_discovery_status(user_auth(Role::User), State(state))
            .await
            .expect_err("a plain User must not read host-level discovery status");

        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn status_allows_an_admin_and_reports_the_setup_block() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![count_row(1)], vec![count_row(1)]])
            .into_connection();

        let response = get_traefik_discovery_status(user_auth(Role::Admin), State(state_with(db)))
            .await
            .expect("an admin must be able to read discovery status");
        let Json(status): Json<TraefikDiscoveryStatusResponse> = response;

        assert!(!status.configured);
        assert_eq!(
            status.setup.enable_env_var,
            "TEMPS_TRAEFIK_DISCOVERY_ENABLED"
        );
        assert!(status.reason.is_some(), "a disabled feature must say why");
    }

    #[tokio::test]
    async fn list_routes_rejects_a_caller_without_settings_read() {
        let state = state_with(MockDatabase::new(DatabaseBackend::Postgres).into_connection());

        let err = list_traefik_discovered_routes(
            user_auth(Role::User),
            State(state),
            Query(ListDiscoveredRoutesQuery {
                page: None,
                page_size: None,
            }),
        )
        .await
        .expect_err("a plain User must not list discovered routes");

        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn list_routes_returns_the_rows_for_an_admin() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![count_row(1)]])
            .append_query_results([vec![route_model("app.example.com", true)]])
            // Cert rows lookup: no cert authorized for this host.
            .append_query_results([Vec::<temps_entities::traefik_route_certificates::Model>::new()])
            .into_connection();

        let response = list_traefik_discovered_routes(
            user_auth(Role::Admin),
            State(state_with(db)),
            Query(ListDiscoveredRoutesQuery {
                page: None,
                page_size: None,
            }),
        )
        .await
        .expect("an admin must be able to list discovered routes");
        let Json(list): Json<TraefikDiscoveredRouteListResponse> = response;

        assert_eq!(list.total, 1);
        assert_eq!(list.routes.len(), 1);
        assert_eq!(list.routes[0].host, "app.example.com");
    }

    #[tokio::test]
    async fn set_enabled_rejects_a_caller_without_settings_write() {
        let state = state_with(MockDatabase::new(DatabaseBackend::Postgres).into_connection());

        let err = set_traefik_discovered_route_enabled(
            user_auth(Role::User),
            State(state),
            request_metadata(),
            Path("app.example.com".to_string()),
            Json(UpdateTraefikRouteEnabledRequest { enabled: false }),
        )
        .await
        .expect_err("a plain User must not suppress a discovered route");

        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn set_enabled_toggles_the_route_for_an_admin() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![route_model("app.example.com", true)]])
            .append_query_results([vec![route_model("app.example.com", false)]])
            .append_exec_results([MockExecResult {
                last_insert_id: 7,
                rows_affected: 1,
            }])
            .into_connection();

        let response = set_traefik_discovered_route_enabled(
            user_auth(Role::Admin),
            State(state_with(db)),
            request_metadata(),
            Path("app.example.com".to_string()),
            Json(UpdateTraefikRouteEnabledRequest { enabled: false }),
        )
        .await
        .expect("an admin must be able to suppress a discovered route");
        let Json(route): Json<TraefikDiscoveredRouteResponse> = response;

        assert!(!route.enabled);
        assert!(!route.active);
    }

    #[tokio::test]
    async fn set_enabled_unknown_host_maps_to_404() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<discovered::Model>::new()])
            .into_connection();

        let err = set_traefik_discovered_route_enabled(
            user_auth(Role::Admin),
            State(state_with(db)),
            request_metadata(),
            Path("missing.example.com".to_string()),
            Json(UpdateTraefikRouteEnabledRequest { enabled: false }),
        )
        .await
        .expect_err("an unknown host must 404");

        assert_eq!(err.status_code, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn set_enabled_blank_host_maps_to_400() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();

        let err = set_traefik_discovered_route_enabled(
            user_auth(Role::Admin),
            State(state_with(db)),
            request_metadata(),
            Path("   ".to_string()),
            Json(UpdateTraefikRouteEnabledRequest { enabled: true }),
        )
        .await
        .expect_err("a blank host must be rejected");

        assert_eq!(err.status_code, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn database_errors_map_to_500_and_keep_their_context() {
        let problem: Problem = TraefikDiscoveryAdminError::Database {
            operation: "listing discovered routes".to_string(),
            source: DbErr::Custom("boom".to_string()),
        }
        .into();
        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn openapi_exposes_every_traefik_discovery_path() {
        let spec = TraefikDiscoveryApiDoc::openapi();
        let paths = &spec.paths.paths;

        for expected in [
            "/traefik-discovery/status",
            "/traefik-discovery/routes",
            "/traefik-discovery/routes/{host}/enabled",
            "/traefik-discovery/routes/{host}/certificate",
            "/traefik-discovery/tls/import",
        ] {
            assert!(
                paths.contains_key(expected),
                "{expected} must be in the OpenAPI schema; got {:?}",
                paths.keys().collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn cert_endpoints_declare_host_path_parameter() {
        let spec = TraefikDiscoveryApiDoc::openapi();
        let item = spec
            .paths
            .paths
            .get("/traefik-discovery/routes/{host}/certificate")
            .expect("certificate path must exist");

        for op in [item.post.as_ref(), item.delete.as_ref()] {
            let op = op.expect("POST and DELETE must both exist");
            let params = op
                .parameters
                .as_ref()
                .expect("cert operations must declare path parameters");
            assert!(
                params.iter().any(|p| p.name == "host"),
                "`host` path parameter must be declared on cert operation"
            );
        }
    }

    #[tokio::test]
    async fn request_cert_rejects_caller_without_settings_write() {
        let state = state_with(MockDatabase::new(DatabaseBackend::Postgres).into_connection());

        let result = request_discovered_route_cert(
            user_auth(Role::User),
            State(state),
            request_metadata(),
            Path("app.example.com".to_string()),
            Json(RequestDiscoveredRouteCertRequest {
                challenge_type: "http-01".to_string(),
                acknowledge_manual_dns_renewal: false,
            }),
        )
        .await;

        match result {
            Err(prob) => assert_eq!(prob.status_code, StatusCode::FORBIDDEN),
            Ok(_) => panic!("a plain User must not authorize TLS"),
        }
    }

    #[tokio::test]
    async fn deauthorize_cert_rejects_caller_without_settings_write() {
        let state = state_with(MockDatabase::new(DatabaseBackend::Postgres).into_connection());

        let result = deauthorize_discovered_route_cert(
            user_auth(Role::User),
            State(state),
            request_metadata(),
            Path("app.example.com".to_string()),
        )
        .await;

        match result {
            Err(prob) => assert_eq!(prob.status_code, StatusCode::FORBIDDEN),
            Ok(_) => panic!("a plain User must not deauthorize TLS"),
        }
    }

    #[tokio::test]
    async fn import_acme_json_rejects_caller_without_settings_write() {
        let state = state_with(MockDatabase::new(DatabaseBackend::Postgres).into_connection());

        let err = import_traefik_acme_json(
            user_auth(Role::User),
            State(state),
            request_metadata(),
            Json(ImportTraefikAcmeJsonRequest {
                acme_json: "{}".to_string(),
                hosts: vec![],
                renewal_method: "http-01".to_string(),
                acknowledge_manual_dns_renewal: false,
                dry_run: true,
            }),
        )
        .await
        .expect_err("a plain User must not import acme.json");

        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn import_acme_json_dry_run_empty_hosts_returns_empty_verdicts() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let state = state_with(db);

        let response = import_traefik_acme_json(
            user_auth(Role::Admin),
            State(state),
            request_metadata(),
            Json(ImportTraefikAcmeJsonRequest {
                acme_json: r#"{"r1": {"Certificates": []}}"#.to_string(),
                hosts: vec![],
                renewal_method: "http-01".to_string(),
                acknowledge_manual_dns_renewal: false,
                dry_run: true,
            }),
        )
        .await
        .expect("empty import must succeed");

        let Json(result) = response;
        assert!(result.dry_run);
        assert_eq!(result.total_requested, 0);
        assert_eq!(result.succeeded, 0);
        assert_eq!(result.failed, 0);
        assert!(result.verdicts.is_empty());
    }

    #[tokio::test]
    async fn import_acme_json_rejects_invalid_renewal_method() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let state = state_with(db);

        let err = import_traefik_acme_json(
            user_auth(Role::Admin),
            State(state),
            request_metadata(),
            Json(ImportTraefikAcmeJsonRequest {
                acme_json: "{}".to_string(),
                hosts: vec!["app.example.com".to_string()],
                renewal_method: "manual".to_string(), // invalid
                acknowledge_manual_dns_renewal: false,
                dry_run: true,
            }),
        )
        .await
        .expect_err("invalid renewal_method must be rejected");

        assert_eq!(err.status_code, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn import_acme_json_returns_per_host_failure_for_unknown_host() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Route lookup for "app.example.com" returns empty.
            .append_query_results([Vec::<discovered::Model>::new()])
            .into_connection();
        let state = state_with(db);

        let response = import_traefik_acme_json(
            user_auth(Role::Admin),
            State(state),
            request_metadata(),
            Json(ImportTraefikAcmeJsonRequest {
                acme_json: r#"{"r1": {"Certificates": []}}"#.to_string(),
                hosts: vec!["app.example.com".to_string()],
                renewal_method: "http-01".to_string(),
                acknowledge_manual_dns_renewal: false,
                dry_run: true,
            }),
        )
        .await
        .expect("import must not fail for a missing host — it returns a per-host verdict");

        let Json(result) = response;
        assert_eq!(result.failed, 1);
        assert_eq!(result.succeeded, 0);
        assert!(!result.verdicts[0].success);
    }

    #[test]
    fn openapi_declares_the_host_path_parameter() {
        // A missing `params(...)` entry for a path parameter generates an SDK
        // function whose argument type is `never`, silently breaking the CLI.
        let spec = TraefikDiscoveryApiDoc::openapi();
        let item = spec
            .paths
            .paths
            .get("/traefik-discovery/routes/{host}/enabled")
            .expect("the toggle path must exist");
        let operation = item
            .patch
            .as_ref()
            .expect("the toggle path must expose a PATCH operation");
        let params = operation
            .parameters
            .as_ref()
            .expect("the toggle operation must declare its path parameters");
        assert!(
            params.iter().any(|p| p.name == "host"),
            "the `host` path parameter must be declared, got {:?}",
            params.iter().map(|p| &p.name).collect::<Vec<_>>()
        );
    }
}
