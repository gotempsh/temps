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

use super::admin_gate::AdminGateSource;
use super::admin_gate_service::{AdminGateService, AdminGateServiceError, AdminGateSettings};

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
            WouldLockOut { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Lockout Prevented")
                .with_detail(err.to_string()),
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

    let new_settings = AdminGateSettings {
        allowed_ips: request.allowed_ips,
        allowed_hosts: request.allowed_hosts,
        trust_forwarded_for: request.trust_forwarded_for,
    };

    let cfg = state
        .service
        .update(new_settings, peer.ip(), caller_host.as_deref())
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
