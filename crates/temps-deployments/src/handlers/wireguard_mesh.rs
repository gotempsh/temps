// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Admin view of the managed WireGuard mesh.
//!
//! `GET /nodes/wireguard` answers "can nodes that only share the internet
//! with this control plane join it, and are the ones that did connected?" —
//! including, when the mesh is off, what is missing and how to turn it on,
//! so the Worker Nodes page can onboard instead of hiding the option.
//! `POST /nodes/wireguard` turns the mesh on; the running `temps serve`
//! notices the setting change and brings its end up without a restart.
//!
//! The logic lives in [`WireguardMeshService`]; these handlers check
//! permissions, record the audit trail and map errors to problems.

use std::sync::Arc;

use axum::{http::StatusCode, response::IntoResponse, Extension, Json};
use serde::Deserialize;
use temps_auth::{permission_guard, require_sensitive_action, AuthContext, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_core::{
    AuditContext, AuditLogger, RequestMetadata, SensitiveAction, SensitiveActionAuthorizer,
};
use temps_network::mesh::MeshError;
use temps_network::mesh_links::Hub;
use tracing::error;
use utoipa::ToSchema;

use crate::handlers::audit::{WireguardMeshEnabledAudit, WireguardMeshHubChangedAudit};
use crate::services::wireguard_mesh::{EnableMesh, WireguardMeshError, WireguardMeshService};

// The response types are the service's view of the mesh; re-exported so the
// API schema and existing paths keep resolving here.
pub use crate::services::wireguard_mesh::{
    WireguardMeshCheck, WireguardMeshCheckStatus, WireguardMeshControlPlaneEntry, WireguardMeshHub,
    WireguardMeshHubTarget, WireguardMeshLink, WireguardMeshLinkState, WireguardMeshNodeConnection,
    WireguardMeshNodeStatus, WireguardMeshState, WireguardMeshStatusResponse, ENABLE_MESH_COMMAND,
};

/// What the mesh admin handlers need. Provided to them as a request
/// extension by the deployments plugin.
#[derive(Clone)]
pub struct WireguardMeshAdminState {
    pub mesh_service: Arc<WireguardMeshService>,
    pub audit_service: Arc<dyn AuditLogger>,
    pub sensitive_action_authorizer: Arc<dyn SensitiveActionAuthorizer>,
}

/// Body of `PUT /nodes/wireguard/hub`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct SetWireguardMeshHubRequest {
    pub hub: WireguardMeshHubTarget,
}

/// Body of `POST /nodes/wireguard`. Both fields keep their current value
/// when omitted; neither can change once a node is on the mesh.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct EnableWireguardMeshRequest {
    /// Mesh address pool (private IPv4, clear of the compute pool).
    pub cidr: Option<String>,
    /// UDP port the mesh listens on.
    pub listen_port: Option<u16>,
    /// TCP port nodes reach the control plane's API on over the mesh.
    /// Defaults to the mesh port number.
    pub node_api_port: Option<u16>,
}

fn mesh_problem(error: WireguardMeshError) -> Problem {
    let (action, error) = match error {
        WireguardMeshError::Nodes(error) => return Problem::from(error),
        WireguardMeshError::UnavailableHere { reason } => {
            return problemdetails::new(StatusCode::CONFLICT)
                .with_title("WireGuard Mesh Unavailable Here")
                .with_detail(reason)
        }
        WireguardMeshError::Mesh { action, source } => (action, source),
    };
    match error {
        MeshError::InvalidCidr { .. }
        | MeshError::OverlapsComputePool { .. }
        | MeshError::InvalidPort(_)
        | MeshError::PortClashesWithVxlan(_) => problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid WireGuard Mesh Settings")
            .with_detail(error.to_string()),
        MeshError::InUse { .. } => problemdetails::new(StatusCode::CONFLICT)
            .with_title("WireGuard Mesh Settings In Use")
            .with_detail(error.to_string()),
        MeshError::Disabled => problemdetails::new(StatusCode::CONFLICT)
            .with_title("WireGuard Mesh Off")
            .with_detail("Turn the WireGuard mesh on before choosing a hub."),
        MeshError::NotOnMesh(_) => problemdetails::new(StatusCode::CONFLICT)
            .with_title("Not On The Mesh")
            .with_detail(error.to_string()),
        MeshError::NodeNotFound(_) => problemdetails::new(StatusCode::NOT_FOUND)
            .with_title("Node Not Found")
            .with_detail(error.to_string()),
        // Stored state that cannot be read, and errors of node registration
        // and pairing, which these operations never return.
        MeshError::Corrupt { .. }
        | MeshError::Database(_)
        | MeshError::Exhausted { .. }
        | MeshError::InvalidEndpoint { .. }
        | MeshError::InvalidPublicKey
        | MeshError::PublicKeyInUse
        | MeshError::PairingClosed
        | MeshError::TooManyPairings { .. } => {
            error!("WireGuard mesh: could not {action}: {error}");
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("WireGuard Mesh Error")
                .with_detail("could not read the WireGuard mesh state; see the server logs")
        }
    }
}

fn audit_context(auth: &AuthContext, metadata: &RequestMetadata) -> AuditContext {
    AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    }
}

/// Make a member the mesh hub, or remove the hub (ADR 048 D4). The hub
/// relays traffic between members that cannot reach each other, so it sees
/// that traffic: pick one of your own machines.
#[utoipa::path(
    tag = "Nodes",
    put,
    path = "/nodes/wireguard/hub",
    operation_id = "WireguardMeshHubSet",
    request_body = SetWireguardMeshHubRequest,
    responses(
        (status = 200, description = "Hub set; current state", body = WireguardMeshStatusResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "No such node"),
        (status = 409, description = "The mesh is off, or the member is not on it"),
        (status = 428, description = "Re-authentication required"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn set_wireguard_mesh_hub(
    RequireAuth(auth): RequireAuth,
    Extension(state): Extension<WireguardMeshAdminState>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<SetWireguardMeshHubRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    require_sensitive_action(
        state.sensitive_action_authorizer.as_ref(),
        &auth,
        SensitiveAction::SetWireguardMeshHub,
    )
    .await?;
    let hub = request.hub.hub();
    state
        .mesh_service
        .set_hub(hub)
        .await
        .map_err(mesh_problem)?;

    let audit = WireguardMeshHubChangedAudit {
        context: audit_context(&auth, &metadata),
        hub: match hub {
            None => "none".to_string(),
            Some(Hub::ControlPlane) => "control-plane".to_string(),
            Some(Hub::Node(node_id)) => format!("node {node_id}"),
        },
    };
    if let Err(error) = state.audit_service.create_audit_log(&audit).await {
        error!(%error, "WireGuard mesh hub changed but audit record failed");
    }
    Ok(Json(
        state.mesh_service.status().await.map_err(mesh_problem)?,
    ))
}

/// Mesh state, per-node connection and join onboarding for the Worker Nodes
/// page.
#[utoipa::path(
    tag = "Nodes",
    get,
    path = "/nodes/wireguard",
    operation_id = "WireguardMeshStatusGet",
    responses(
        (status = 200, description = "WireGuard mesh state", body = WireguardMeshStatusResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn wireguard_mesh_status(
    RequireAuth(auth): RequireAuth,
    Extension(state): Extension<WireguardMeshAdminState>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    Ok(Json(
        state.mesh_service.status().await.map_err(mesh_problem)?,
    ))
}

/// Turn on the cluster's WireGuard mesh (idempotent). The running control
/// plane brings its end up within a minute and nodes follow; it cannot be
/// turned off again from the API.
#[utoipa::path(
    tag = "Nodes",
    post,
    path = "/nodes/wireguard",
    operation_id = "WireguardMeshEnable",
    request_body = EnableWireguardMeshRequest,
    responses(
        (status = 200, description = "Mesh enabled; current state", body = WireguardMeshStatusResponse),
        (status = 400, description = "Invalid pool or port"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 409, description = "This server cannot bring up the mesh, or the pool/port is in use"),
        (status = 428, description = "Re-authentication required"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn enable_wireguard_mesh(
    RequireAuth(auth): RequireAuth,
    Extension(state): Extension<WireguardMeshAdminState>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<EnableWireguardMeshRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    require_sensitive_action(
        state.sensitive_action_authorizer.as_ref(),
        &auth,
        SensitiveAction::EnableWireguardMesh,
    )
    .await?;

    let settings = state
        .mesh_service
        .enable(EnableMesh {
            cidr: request.cidr,
            listen_port: request.listen_port,
            node_api_port: request.node_api_port,
        })
        .await
        .map_err(mesh_problem)?;

    let audit = WireguardMeshEnabledAudit {
        context: audit_context(&auth, &metadata),
        cidr: settings.cidr.to_string(),
        listen_port: settings.port,
    };
    if let Err(error) = state.audit_service.create_audit_log(&audit).await {
        error!(%error, "WireGuard mesh enabled but audit record failed");
    }

    Ok(Json(
        state.mesh_service.status().await.map_err(mesh_problem)?,
    ))
}

/// Harness shared by the mesh and pairing handler tests: the handlers read
/// their state, the caller and the request metadata from request
/// extensions, so a test router needs no `AppState`.
#[cfg(test)]
pub(crate) mod admin_test_support {
    use std::sync::{Arc, Mutex};

    use axum::body::Body;
    use axum::http::{HeaderMap, Request, StatusCode};
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_core::{
        SensitiveAction, SensitiveActionAuthorizationError, SensitiveActionAuthorizer,
        SensitiveActionDecision, SensitiveActionPrincipal,
    };
    use tower::ServiceExt;

    pub const CLIENT_IP: &str = "203.0.113.99";
    pub const CLIENT_AGENT: &str = "temps-tests/1.0";

    fn user() -> temps_entities::users::Model {
        temps_entities::users::Model {
            id: 1,
            name: "Admin".to_string(),
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
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    /// An admin in a browser session, so sensitive actions reach the
    /// authorizer.
    pub fn admin() -> temps_auth::AuthContext {
        temps_auth::AuthContext::new_persisted_session(user(), temps_auth::Role::Admin, 42)
    }

    /// An API key that may read settings but not change them.
    pub fn settings_reader() -> temps_auth::AuthContext {
        temps_auth::AuthContext::new_api_key(
            user(),
            None,
            Some(vec![temps_auth::Permission::SettingsRead]),
            "settings-reader".to_string(),
            7,
        )
    }

    /// An API key with no settings access at all.
    pub fn outsider() -> temps_auth::AuthContext {
        temps_auth::AuthContext::new_api_key(
            user(),
            None,
            Some(vec![temps_auth::Permission::ProjectsRead]),
            "projects-only".to_string(),
            8,
        )
    }

    pub struct AllowSensitiveActions;

    #[async_trait::async_trait]
    impl SensitiveActionAuthorizer for AllowSensitiveActions {
        async fn authorize(
            &self,
            _action: &SensitiveAction,
            _principal: &SensitiveActionPrincipal,
        ) -> Result<SensitiveActionDecision, SensitiveActionAuthorizationError> {
            Ok(SensitiveActionDecision::Allow)
        }
    }

    /// Every sensitive action needs a fresh MFA verification.
    pub struct RequireStepUp;

    #[async_trait::async_trait]
    impl SensitiveActionAuthorizer for RequireStepUp {
        async fn authorize(
            &self,
            _action: &SensitiveAction,
            _principal: &SensitiveActionPrincipal,
        ) -> Result<SensitiveActionDecision, SensitiveActionAuthorizationError> {
            Ok(SensitiveActionDecision::RequireVerification {
                mfa_setup_required: false,
            })
        }
    }

    /// One recorded audit entry.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Recorded {
        pub operation: String,
        pub ip_address: Option<String>,
        pub user_agent: String,
        pub body: String,
    }

    #[derive(Default)]
    pub struct RecordingAuditLogger {
        pub records: Mutex<Vec<Recorded>>,
    }

    impl RecordingAuditLogger {
        pub fn records(&self) -> Vec<Recorded> {
            self.records.lock().map(|r| r.clone()).unwrap_or_default()
        }
    }

    #[async_trait::async_trait]
    impl temps_core::AuditLogger for RecordingAuditLogger {
        async fn create_audit_log(
            &self,
            operation: &dyn temps_core::audit::AuditOperation,
        ) -> anyhow::Result<()> {
            let record = Recorded {
                operation: operation.operation_type(),
                ip_address: operation.ip_address(),
                user_agent: operation.user_agent().to_string(),
                body: operation.serialize()?,
            };
            self.records
                .lock()
                .map_err(|_| anyhow::anyhow!("audit recorder poisoned"))?
                .push(record);
            Ok(())
        }
    }

    pub fn metadata() -> temps_core::RequestMetadata {
        temps_core::RequestMetadata {
            ip_address: CLIENT_IP.to_string(),
            user_agent: CLIENT_AGENT.to_string(),
            headers: HeaderMap::new(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "https://cp.example.com".to_string(),
            scheme: "https".to_string(),
            host: "cp.example.com".to_string(),
            is_secure: true,
        }
    }

    pub fn encryption_service() -> Arc<temps_core::EncryptionService> {
        Arc::new(
            temps_core::EncryptionService::new("01234567890123456789012345678901")
                .expect("test encryption key"),
        )
    }

    /// A config service whose settings are `settings`.
    pub fn config_service(settings: temps_core::AppSettings) -> Arc<temps_config::ConfigService> {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![temps_entities::settings::Model {
                id: 1,
                data: settings.to_json(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            }]])
            .into_connection();
        let server_config = Arc::new(temps_config::ServerConfig {
            address: "127.0.0.1:3000".to_string(),
            database_url: "postgres://test".to_string(),
            tls_address: None,
            console_address: "127.0.0.1:0".to_string(),
            console_admin_address: None,
            admin_allowed_ips: Vec::new(),
            admin_allowed_hosts: Vec::new(),
            admin_trust_forwarded_for: false,
            data_dir: std::path::PathBuf::from("/tmp/temps-test"),
            auth_secret: "test-secret".to_string(),
            encryption_key: "test-key".to_string(),
            api_base_url: "/api".to_string(),
            postgres_max_connections: None,
            postgres_min_connections: None,
            postgres_connect_timeout_secs: None,
            postgres_acquire_timeout_secs: None,
            postgres_idle_timeout_secs: None,
            postgres_max_lifetime_secs: None,
            clickhouse_url: None,
            clickhouse_database: None,
            clickhouse_user: None,
            clickhouse_password: None,
            docker_extra_networks: Vec::new(),
        });
        Arc::new(temps_config::ConfigService::new(
            server_config,
            Arc::new(db),
        ))
    }

    /// The singleton `network_config` row: mesh on or off, and whether the
    /// control plane published its end.
    pub fn network_config(
        wireguard_enabled: bool,
        published: bool,
    ) -> temps_entities::network_config::Model {
        temps_entities::network_config::Model {
            id: 1,
            compute_pool_cidr: "172.20.0.0/16".to_string(),
            subnet_prefix_len: 24,
            transport: "vxlan".to_string(),
            vxlan_vni: 42,
            vxlan_port: 4789,
            underlay_mtu: 1500,
            control_plane_compute_cidr: None,
            control_plane_underlay_address: None,
            control_plane_overlay_ready: false,
            control_plane_setup_generation: 0,
            wireguard_enabled,
            wireguard_cidr: "10.201.0.0/16".to_string(),
            wireguard_port: 51820,
            control_plane_wg_public_key: published
                .then(|| "Y29udHJvbC1wbGFuZS1rZXktMzItYnl0ZXMtbG9uZyE=".to_string()),
            control_plane_wg_endpoint: published.then(|| "198.51.100.1:51820".to_string()),
            node_api_port: None,
            mesh_hub_node_id: None,
            mesh_hub_control_plane: false,
            updated_at: chrono::Utc::now(),
        }
    }

    pub fn mock_db() -> MockDatabase {
        MockDatabase::new(DatabaseBackend::Postgres)
    }

    /// A worker node whose agent authenticates with `token`.
    pub fn node(id: i32, name: &str, token: &str) -> temps_entities::nodes::Model {
        use sha2::Digest;
        temps_entities::nodes::Model {
            id,
            name: name.to_string(),
            token_hash: hex::encode(sha2::Sha256::digest(token.as_bytes())),
            token_encrypted: None,
            address: format!("https://10.100.0.{id}:3100"),
            private_address: format!("10.100.0.{id}"),
            public_endpoint: None,
            wg_public_key: None,
            role: "worker".to_string(),
            status: "active".to_string(),
            labels: serde_json::json!({}),
            capacity: serde_json::json!({}),
            last_heartbeat: None,
            edge_public_key: None,
            compute_cidr: None,
            architecture: None,
            underlay_address: None,
            mesh_wg_public_key: None,
            mesh_wg_endpoint: None,
            mesh_wg_address: None,
            dns_resolver_running: None,
            dns_resolver_tasks_alive: None,
            dns_resolver_last_sync_at: None,
            dns_resolver_consecutive_failures: 0,
            dns_resolver_last_error: None,
            dns_resolver_record_count: None,
            failover_at: None,
            public_ingress_enabled: false,
            public_ingress_running: None,
            public_ingress_last_error: None,
            public_ingress_certificate_count: None,
            public_ingress_route_count: None,
            public_ingress_unsupported_route_count: None,
            public_ingress_unsupported_reasons: serde_json::json!([]),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    /// Send `method uri` with `body`, as `auth` (anonymous when `None`).
    pub async fn send(
        app: axum::Router,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
        auth: Option<temps_auth::AuthContext>,
    ) -> (StatusCode, HeaderMap, serde_json::Value) {
        let mut request = Request::builder().method(method).uri(uri);
        if body.is_some() {
            request = request.header("content-type", "application/json");
        }
        let mut request = request
            .body(match body {
                Some(body) => Body::from(body.to_string()),
                None => Body::empty(),
            })
            .expect("request");
        request.extensions_mut().insert(metadata());
        if let Some(auth) = auth {
            request.extensions_mut().insert(auth);
        }
        let response = app.oneshot(request).await.expect("response");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let json = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|_| {
                serde_json::Value::String(String::from_utf8_lossy(&bytes).into_owned())
            })
        };
        (status, headers, json)
    }
}

#[cfg(test)]
mod tests {
    use super::admin_test_support::*;
    use super::*;
    use axum::routing::{get, put};
    use axum::Router;
    use sea_orm::DatabaseConnection;

    fn problem_status(error: MeshError) -> StatusCode {
        mesh_problem(WireguardMeshError::Mesh {
            action: "set the mesh hub",
            source: error,
        })
        .status_code
    }

    #[test]
    fn set_hub_errors_map_to_conflict_or_not_found() {
        assert_eq!(problem_status(MeshError::Disabled), StatusCode::CONFLICT);
        assert_eq!(
            problem_status(MeshError::NotOnMesh("worker-1".into())),
            StatusCode::CONFLICT
        );
        assert_eq!(
            problem_status(MeshError::NodeNotFound(7)),
            StatusCode::NOT_FOUND
        );
    }

    #[test]
    fn settings_errors_are_the_callers_and_storage_errors_are_ours() {
        assert_eq!(
            problem_status(MeshError::InvalidPort(0)),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            problem_status(MeshError::PortClashesWithVxlan(4789)),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            problem_status(MeshError::InUse {
                setting: "pool",
                current: "10.201.0.0/16".into(),
                assigned: 2,
            }),
            StatusCode::CONFLICT
        );
        assert_eq!(
            problem_status(MeshError::from(sea_orm::DbErr::Custom(
                "connection reset".into()
            ))),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        let unavailable = mesh_problem(WireguardMeshError::UnavailableHere {
            reason: "needs Linux".into(),
        });
        assert_eq!(unavailable.status_code, StatusCode::CONFLICT);
    }

    fn app(
        db: DatabaseConnection,
        authorizer: Arc<dyn SensitiveActionAuthorizer>,
        audit: Arc<RecordingAuditLogger>,
    ) -> Router {
        let db = Arc::new(db);
        let settings = temps_core::AppSettings {
            external_url: Some("https://cp.example.com/".to_string()),
            ..Default::default()
        };
        let state = WireguardMeshAdminState {
            mesh_service: Arc::new(WireguardMeshService::new(
                db.clone(),
                Arc::new(crate::services::NodeService::new(db)),
                config_service(settings),
            )),
            audit_service: audit,
            sensitive_action_authorizer: authorizer,
        };
        Router::new()
            .route(
                "/nodes/wireguard",
                get(wireguard_mesh_status).post(enable_wireguard_mesh),
            )
            .route("/nodes/wireguard/hub", put(set_wireguard_mesh_hub))
            .layer(Extension(state))
    }

    fn allow() -> Arc<dyn SensitiveActionAuthorizer> {
        Arc::new(AllowSensitiveActions)
    }

    fn step_up() -> Arc<dyn SensitiveActionAuthorizer> {
        Arc::new(RequireStepUp)
    }

    fn quiet_app() -> Router {
        app(
            mock_db().into_connection(),
            allow(),
            Arc::new(RecordingAuditLogger::default()),
        )
    }

    // ── authentication and permissions ──────────────────────────────────

    #[tokio::test]
    async fn every_mesh_endpoint_requires_authentication() {
        for (method, uri, body) in [
            ("GET", "/nodes/wireguard", None),
            ("POST", "/nodes/wireguard", Some(serde_json::json!({}))),
            (
                "PUT",
                "/nodes/wireguard/hub",
                Some(serde_json::json!({"hub": {"kind": "none"}})),
            ),
        ] {
            let (status, _, _) = send(quiet_app(), method, uri, body, None).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
        }
    }

    #[tokio::test]
    async fn reading_the_mesh_needs_settings_read() {
        let (status, _, _) = send(
            quiet_app(),
            "GET",
            "/nodes/wireguard",
            None,
            Some(outsider()),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn changing_the_mesh_needs_settings_write() {
        for (method, uri, body) in [
            ("POST", "/nodes/wireguard", serde_json::json!({})),
            (
                "PUT",
                "/nodes/wireguard/hub",
                serde_json::json!({"hub": {"kind": "control_plane"}}),
            ),
        ] {
            let (status, _, _) = send(
                quiet_app(),
                method,
                uri,
                Some(body),
                Some(settings_reader()),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
        }
    }

    #[tokio::test]
    async fn changing_the_mesh_asks_for_step_up() {
        let audit = Arc::new(RecordingAuditLogger::default());
        for (method, uri, body) in [
            ("POST", "/nodes/wireguard", serde_json::json!({})),
            (
                "PUT",
                "/nodes/wireguard/hub",
                serde_json::json!({"hub": {"kind": "none"}}),
            ),
        ] {
            let app = app(mock_db().into_connection(), step_up(), audit.clone());
            let (status, _, body) = send(app, method, uri, Some(body), Some(admin())).await;
            assert_eq!(status, StatusCode::PRECONDITION_REQUIRED, "{method} {uri}");
            assert_eq!(body["error_code"], "STEP_UP_REQUIRED");
        }
        assert!(
            audit.records().is_empty(),
            "nothing changed, nothing audited"
        );
    }

    // ── success paths ───────────────────────────────────────────────────

    fn worker() -> temps_entities::nodes::Model {
        node(1, "worker-1", "worker-1-token")
    }

    #[tokio::test]
    async fn status_onboards_while_the_mesh_is_off() {
        let db = mock_db()
            // load_settings, published_control_plane, configured_port
            .append_query_results(vec![vec![network_config(false, false)]])
            .append_query_results(vec![vec![network_config(false, false)]])
            .append_query_results(vec![vec![network_config(false, false)]])
            // the nodes
            .append_query_results(vec![vec![worker()]])
            .into_connection();
        let app = app(db, allow(), Arc::new(RecordingAuditLogger::default()));
        let (status, _, body) = send(
            app,
            "GET",
            "/nodes/wireguard",
            None,
            Some(settings_reader()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["state"], "disabled");
        assert_eq!(body["listen_port"], 51820);
        assert_eq!(body["join_url"], "https://cp.example.com");
        assert_eq!(body["enable_command"], ENABLE_MESH_COMMAND);
        assert_eq!(body["can_enable"], cfg!(target_os = "linux"));
        assert!(body["reason"]
            .as_str()
            .unwrap()
            .contains("Enable the WireGuard mesh"));
        assert_eq!(body["nodes"][0]["name"], "worker-1");
        assert_eq!(body["nodes"][0]["connection"], "mesh_off");
        assert_eq!(body["nodes"][0]["checks"], serde_json::json!([]));
        assert_eq!(body["links"], serde_json::json!([]));
        assert!(body["hub"].is_null());
    }

    #[tokio::test]
    async fn enabling_an_enabled_mesh_records_who_did_it_and_from_where() {
        let db = mock_db()
            // enable: load_settings, then the locked read and the update
            .append_query_results(vec![vec![network_config(true, false)]])
            .append_query_results(vec![vec![network_config(true, false)]])
            .append_query_results(vec![vec![network_config(true, false)]])
            // status: load_settings, published_control_plane, the nodes
            .append_query_results(vec![vec![network_config(true, false)]])
            .append_query_results(vec![vec![network_config(true, false)]])
            .append_query_results(vec![Vec::<temps_entities::nodes::Model>::new()])
            .into_connection();
        let audit = Arc::new(RecordingAuditLogger::default());
        let app = app(db, allow(), audit.clone());
        let (status, _, body) = send(
            app,
            "POST",
            "/nodes/wireguard",
            Some(serde_json::json!({})),
            Some(admin()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["state"], "starting");
        assert_eq!(body["cidr"], "10.201.0.0/16");

        let records = audit.records();
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0].operation, "WIREGUARD_MESH_ENABLED");
        assert_eq!(records[0].ip_address.as_deref(), Some(CLIENT_IP));
        assert_eq!(records[0].user_agent, CLIENT_AGENT);
    }

    #[tokio::test]
    async fn removing_the_hub_records_who_did_it_and_from_where() {
        let db = mock_db()
            // set_hub: the config row, then the update
            .append_query_results(vec![vec![network_config(true, false)]])
            .append_query_results(vec![vec![network_config(true, false)]])
            // status: load_settings, published_control_plane (not yet, so
            // no links are read), the nodes
            .append_query_results(vec![vec![network_config(true, false)]])
            .append_query_results(vec![vec![network_config(true, false)]])
            .append_query_results(vec![Vec::<temps_entities::nodes::Model>::new()])
            .into_connection();
        let audit = Arc::new(RecordingAuditLogger::default());
        let app = app(db, allow(), audit.clone());
        let (status, _, body) = send(
            app,
            "PUT",
            "/nodes/wireguard/hub",
            Some(serde_json::json!({"hub": {"kind": "none"}})),
            Some(admin()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");

        let records = audit.records();
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0].operation, "WIREGUARD_MESH_HUB_CHANGED");
        assert!(records[0].body.contains("\"hub\":\"none\""), "{records:?}");
        assert_eq!(records[0].ip_address.as_deref(), Some(CLIENT_IP));
        assert_eq!(records[0].user_agent, CLIENT_AGENT);
    }

    #[tokio::test]
    async fn choosing_a_hub_while_the_mesh_is_off_is_a_conflict() {
        let db = mock_db()
            .append_query_results(vec![vec![network_config(false, false)]])
            .into_connection();
        let audit = Arc::new(RecordingAuditLogger::default());
        let app = app(db, allow(), audit.clone());
        let (status, _, body) = send(
            app,
            "PUT",
            "/nodes/wireguard/hub",
            Some(serde_json::json!({"hub": {"kind": "control_plane"}})),
            Some(admin()),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["title"], "WireGuard Mesh Off");
        assert!(audit.records().is_empty());
    }
}
