// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HTTP handlers for `/api/admin/gate-settings`.
//!
//! - `GET` returns the active config + its source (env|db|default) so the UI
//!   knows whether to render read-only.
//! - `PATCH` validates, runs a lockout pre-flight against the caller's
//!   address/host, persists, and atomic-swaps the live config.
//!
//! Both routes require `SettingsWrite` (PATCH) or `SettingsRead` (GET) like
//! the rest of the admin surface.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    extract::{ConnectInfo, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use utoipa::ToSchema;

use super::admin_gate::{trusted_forwarded_client_ip, AdminGateSource, InvalidForwardedFor};
use super::admin_gate_service::{
    AdminGateCallerIps, AdminGateService, AdminGateServiceError, AdminGateSettings,
};

/// State carried by these handlers. Held behind an `Arc` so axum can clone
/// it cheaply for every request.
pub struct AdminGateAppState {
    pub service: AdminGateService,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AdminGateResponse {
    /// IPs / CIDRs allowed to reach the admin listener. Empty = any source.
    pub allowed_ips: Vec<String>,
    /// `Host` header values allowed. Empty = any host.
    pub allowed_hosts: Vec<String>,
    /// When true, the gate trusts `X-Forwarded-For` from loopback peers.
    pub trust_forwarded_for: bool,
    /// Where the active config came from.
    pub source: AdminGateSource,
    /// True when the config is writable through this API. False when env
    /// vars are dictating the active config.
    pub editable: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateAdminGateRequest {
    pub allowed_ips: Vec<String>,
    pub allowed_hosts: Vec<String>,
    pub trust_forwarded_for: bool,
}

fn update_caller_ips(
    headers: &axum::http::HeaderMap,
    peer: SocketAddr,
    trust_forwarded_for: bool,
) -> Result<AdminGateCallerIps, AdminGateServiceError> {
    let forwarded_ip =
        trusted_forwarded_client_ip(headers, peer.ip()).map_err(|InvalidForwardedFor| {
            AdminGateServiceError::InvalidCallerForwardedFor { peer_ip: peer.ip() }
        })?;
    let edge_client_ip = forwarded_ip.unwrap_or_else(|| peer.ip());
    let console_client_ip = if trust_forwarded_for {
        edge_client_ip
    } else {
        peer.ip()
    };
    Ok(AdminGateCallerIps {
        edge_client_ip,
        console_client_ip,
    })
}

impl From<AdminGateServiceError> for Problem {
    fn from(err: AdminGateServiceError) -> Self {
        use AdminGateServiceError::*;
        match err {
            Invalid(_) => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Admin Gate Config")
                .with_detail(err.to_string()),
            EnvOverridden => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Admin Gate Read-Only")
                .with_detail(err.to_string()),
            WouldLockOut { .. } | InvalidCallerForwardedFor { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Lockout Prevented")
                    .with_detail(err.to_string())
            }
            Database(_) | Serde(_) => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Internal Server Error")
                .with_detail(err.to_string()),
        }
    }
}

#[utoipa::path(
    tag = "AdminGate",
    get,
    path = "/admin/gate-settings",
    responses(
        (status = 200, description = "Current admin gate config", body = AdminGateResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions")
    ),
    security(("bearer_auth" = []))
)]
async fn get_admin_gate(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AdminGateAppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    let cfg = state.service.snapshot();
    let response = AdminGateResponse {
        allowed_ips: cfg.allowed_nets.iter().map(|n| n.to_string()).collect(),
        allowed_hosts: cfg.allowed_hosts.as_ref().clone(),
        trust_forwarded_for: cfg.trust_forwarded_for,
        source: cfg.source,
        editable: cfg.is_editable() && !state.service.env_overridden(),
    };
    Ok(Json(response))
}

#[utoipa::path(
    tag = "AdminGate",
    patch,
    path = "/admin/gate-settings",
    request_body = UpdateAdminGateRequest,
    responses(
        (status = 200, description = "Updated admin gate config", body = AdminGateResponse),
        (status = 400, description = "Invalid IP/CIDR/host"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 409, description = "Env-overridden or would lock out caller")
    ),
    security(("bearer_auth" = []))
)]
async fn patch_admin_gate(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AdminGateAppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(request): Json<UpdateAdminGateRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);

    let caller_host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let caller_ips = update_caller_ips(&headers, peer, request.trust_forwarded_for)?;

    let new_settings = AdminGateSettings {
        allowed_ips: request.allowed_ips,
        allowed_hosts: request.allowed_hosts,
        trust_forwarded_for: request.trust_forwarded_for,
    };

    let cfg = state
        .service
        .update(new_settings, caller_ips, caller_host.as_deref())
        .await?;

    Ok(Json(AdminGateResponse {
        allowed_ips: cfg.allowed_nets.iter().map(|n| n.to_string()).collect(),
        allowed_hosts: cfg.allowed_hosts.as_ref().clone(),
        trust_forwarded_for: cfg.trust_forwarded_for,
        source: cfg.source,
        editable: cfg.is_editable() && !state.service.env_overridden(),
    }))
}

#[derive(utoipa::OpenApi)]
#[openapi(
    paths(get_admin_gate, patch_admin_gate),
    components(schemas(AdminGateResponse, UpdateAdminGateRequest, AdminGateSource))
)]
pub struct AdminGateApiDoc;

/// Build the router for admin gate settings. Mount this on the admin surface
/// (never the public one) so the env override behavior makes sense.
pub fn configure_routes(state: Arc<AdminGateAppState>) -> Router {
    Router::new()
        .route(
            "/admin/gate-settings",
            get(get_admin_gate).patch(patch_admin_gate),
        )
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::net::IpAddr;
    use temps_auth::{AuthContext, Role};
    use temps_core::admin_gate::{AdminGateConfig, AdminGateSource};
    use temps_entities::{settings, users};

    fn settings_row(data: serde_json::Value) -> settings::Model {
        settings::Model {
            id: 1,
            data,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn admin_auth() -> AuthContext {
        let now = chrono::Utc::now();
        let user = users::Model {
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
            created_at: now,
            updated_at: now,
        };
        AuthContext::new_session(user, Role::Admin)
    }

    fn request_headers(forwarded_for: &str) -> axum::http::HeaderMap {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(header::HOST, "admin.example.com".parse().unwrap());
        headers.insert("x-forwarded-for", forwarded_for.parse().unwrap());
        headers
    }

    #[test]
    fn update_preflight_uses_original_ip_when_enabling_forwarded_for() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-forwarded-for", "100.100.1.2".parse().unwrap());
        let peer = "127.0.0.1:43210".parse().unwrap();

        let caller_ips = update_caller_ips(&headers, peer, true).unwrap();
        let candidate = AdminGateConfig::from_parts(
            &["100.64.0.0/10".to_string()],
            &[],
            true,
            AdminGateSource::Db,
        )
        .unwrap();

        assert_eq!(
            caller_ips,
            AdminGateCallerIps::direct("100.100.1.2".parse::<IpAddr>().unwrap())
        );
        assert!(candidate.would_allow(caller_ips.edge_client_ip, None));
        assert!(candidate.would_allow(caller_ips.console_client_ip, None));
    }

    #[test]
    fn update_preflight_ignores_forwarded_ip_when_candidate_disables_trust() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-forwarded-for", "100.100.1.2".parse().unwrap());
        let peer = "127.0.0.1:43210".parse().unwrap();

        let caller_ips = update_caller_ips(&headers, peer, false).unwrap();
        assert_eq!(
            caller_ips.edge_client_ip,
            "100.100.1.2".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            caller_ips.console_client_ip,
            "127.0.0.1".parse::<IpAddr>().unwrap()
        );
    }

    #[tokio::test]
    async fn patch_allows_forwarded_caller_inside_candidate_cidr() {
        let bootstrap_row = settings_row(serde_json::json!({}));
        let returned_row = settings_row(serde_json::json!({
            "admin_gate": {
                "allowed_ips": ["100.64.0.0/10"],
                "allowed_hosts": ["admin.example.com"],
                "trust_forwarded_for": true
            }
        }));
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![bootstrap_row.clone()]])
            .append_query_results(vec![vec![bootstrap_row]])
            .append_query_results(vec![vec![returned_row]])
            .into_connection();
        let (service, _handle) = AdminGateService::new(Arc::new(db), &[], &[], false)
            .await
            .unwrap();
        let state = Arc::new(AdminGateAppState { service });

        let response = patch_admin_gate(
            RequireAuth(admin_auth()),
            State(Arc::clone(&state)),
            ConnectInfo("127.0.0.1:43210".parse().unwrap()),
            request_headers("100.100.1.2"),
            Json(UpdateAdminGateRequest {
                allowed_ips: vec!["100.64.0.0/10".to_string()],
                allowed_hosts: vec!["admin.example.com".to_string()],
                trust_forwarded_for: true,
            }),
        )
        .await
        .unwrap()
        .into_response();

        assert_eq!(response.status(), StatusCode::OK);
        let config = state.service.snapshot();
        assert!(config.would_allow(
            "100.100.1.2".parse::<IpAddr>().unwrap(),
            Some("admin.example.com")
        ));
    }

    #[tokio::test]
    async fn patch_rejects_loopback_only_candidate_that_denies_edge_caller() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<settings::Model>::new()])
            .into_connection();
        let (service, _handle) = AdminGateService::new(Arc::new(db), &[], &[], false)
            .await
            .unwrap();
        let state = Arc::new(AdminGateAppState { service });

        let result = patch_admin_gate(
            RequireAuth(admin_auth()),
            State(Arc::clone(&state)),
            ConnectInfo("127.0.0.1:43210".parse().unwrap()),
            request_headers("100.100.1.2"),
            Json(UpdateAdminGateRequest {
                allowed_ips: vec!["127.0.0.1/32".to_string()],
                allowed_hosts: vec!["admin.example.com".to_string()],
                trust_forwarded_for: false,
            }),
        )
        .await;
        let problem = match result {
            Ok(_) => panic!("loopback-only candidate unexpectedly passed lockout preflight"),
            Err(problem) => problem,
        };

        assert_eq!(problem.status_code, StatusCode::CONFLICT);
        assert!(state.service.snapshot().is_noop());
    }
}
