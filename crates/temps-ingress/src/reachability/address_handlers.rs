// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `GET /settings/preview-domain/status` (ADR 068 D2) and a tunnel's
//! environment hosts (ADR 068 D3).

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    response::IntoResponse,
    routing::get,
    Extension, Json, Router,
};
use temps_auth::{permission_guard, require_sensitive_action, RequireAuth};
use temps_core::environment_address::{
    AddressKind, PreviewDomainStatus, PrimaryAddress, ReachabilityFix, ReachabilityFixKind,
    ReachabilityWarning,
};
use temps_core::problemdetails::Problem;
use temps_core::{
    AuditContext, AuditLogger, AuditOperation, RequestMetadata, SensitiveAction,
    SensitiveActionAuthorizer,
};
use tracing::error;
use utoipa::OpenApi;

use super::environment_address::{EnvironmentAddressService, PreviewDomainStatusResponse};
use super::environment_hosts::{
    EnvironmentHostRefusal, EnvironmentHostService, EnvironmentHostView, EnvironmentHostsResponse,
    SetEnvironmentHostsRequest,
};

pub struct AddressAppState {
    pub service: Arc<EnvironmentAddressService>,
    pub environment_hosts: Arc<EnvironmentHostService>,
    pub audit_service: Arc<dyn AuditLogger>,
    pub sensitive_action_authorizer: Arc<dyn SensitiveActionAuthorizer>,
}

#[derive(OpenApi)]
#[openapi(
    paths(
        get_preview_domain_status,
        get_connector_environment_hosts,
        set_connector_environment_hosts
    ),
    components(schemas(
        EnvironmentHostsResponse,
        EnvironmentHostView,
        EnvironmentHostRefusal,
        SetEnvironmentHostsRequest,
        PreviewDomainStatusResponse,
        PreviewDomainStatus,
        PrimaryAddress,
        AddressKind,
        ReachabilityWarning,
        ReachabilityFix,
        ReachabilityFixKind,
    )),
    tags((name = "Reachability", description = "Making a project reachable (ADR 064)"))
)]
pub struct AddressApiDoc;

pub fn configure_routes() -> Router<Arc<AddressAppState>> {
    Router::new()
        .route(
            "/settings/preview-domain/status",
            get(get_preview_domain_status),
        )
        .route(
            "/ingress/connectors/{id}/environment-hosts",
            get(get_connector_environment_hosts).put(set_connector_environment_hosts),
        )
}

/// Whether the preview domain can be reached.
///
/// `local_only` (`localhost`, `localho.st`), `unresolved` (a name under it
/// does not resolve), `no_way_in` (it resolves, but to nothing this install
/// serves publicly) or `usable`. Recomputed when the setting, the tunnels or
/// the nodes' public ingress change, and every 30 minutes.
#[utoipa::path(
    tag = "Reachability",
    get,
    path = "/settings/preview-domain/status",
    operation_id = "get_preview_domain_status",
    responses(
        (status = 200, body = PreviewDomainStatusResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn get_preview_domain_status(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AddressAppState>>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    Ok(Json(state.service.preview_domain_status().await?))
}

/// What a tunnel publishes for environments, or would once turned on.
///
/// Every environment's host under the tunnel's domain, its publication
/// state, why one got a short-hash name, and the environments it refuses
/// (private, signed-in only, hosted elsewhere).
#[utoipa::path(
    tag = "Reachability",
    get,
    path = "/ingress/connectors/{id}/environment-hosts",
    operation_id = "get_connector_environment_hosts",
    params(("id" = i32, Path, description = "Tunnel id")),
    responses(
        (status = 200, body = EnvironmentHostsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "No such tunnel"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn get_connector_environment_hosts(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AddressAppState>>,
    Path(id): Path<i32>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    Ok(Json(state.environment_hosts.list(id).await?))
}

/// Turn a tunnel's environment hosts on or off.
///
/// On publishes every listed environment as `<label>.<domain>`; off
/// withdraws every host it generated. Both need `confirm`: the
/// `confirm_token` of the list from the `GET` that was shown. Turning it on
/// is a sensitive action (it widens every environment to public). At most
/// one tunnel has it on.
#[utoipa::path(
    tag = "Reachability",
    put,
    path = "/ingress/connectors/{id}/environment-hosts",
    operation_id = "set_connector_environment_hosts",
    params(("id" = i32, Path, description = "Tunnel id")),
    request_body = SetEnvironmentHostsRequest,
    responses(
        (status = 200, body = EnvironmentHostsResponse),
        (status = 400, description = "Not confirmed"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "No such tunnel"),
        (status = 409, description = "Another tunnel publishes environment hosts, or the list changed since it was shown"),
        (status = 428, description = "Turning it on needs a recent MFA verification"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
async fn set_connector_environment_hosts(
    RequireAuth(auth): RequireAuth,
    State(state): State<Arc<AddressAppState>>,
    Extension(metadata): Extension<RequestMetadata>,
    Path(id): Path<i32>,
    Json(request): Json<SetEnvironmentHostsRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    // Turning it off withdraws what it listed before: record those.
    let before = state.environment_hosts.list(id).await?;
    if publishing_needs_step_up(before.enabled, request.enabled) {
        require_sensitive_action(
            state.sensitive_action_authorizer.as_ref(),
            &auth,
            SensitiveAction::PublishEnvironmentHosts {
                connector_id: Some(id),
            },
        )
        .await?;
    }
    let view = state.environment_hosts.set(id, &request).await?;
    let listed = if view.enabled { &view } else { &before };
    let audit = EnvironmentHostsAudit {
        context: AuditContext {
            user_id: auth.user_id(),
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        },
        connector_id: id,
        connector_name: view.connector_name.clone(),
        enabled: view.enabled,
        hostnames: listed.hosts.iter().map(|h| h.hostname.clone()).collect(),
    };
    if let Err(error) = state.audit_service.create_audit_log(&audit).await {
        error!(%error, connector_id = id, "environment hosts changed but audit record failed");
    }
    Ok(Json(view))
}

/// Only turning it on widens access, so only that needs a recent MFA
/// verification; turning it off, or a repeat of the current state, does not.
fn publishing_needs_step_up(enabled_before: bool, enable: bool) -> bool {
    enable && !enabled_before
}

#[derive(Debug, serde::Serialize)]
struct EnvironmentHostsAudit {
    context: AuditContext,
    connector_id: i32,
    connector_name: String,
    enabled: bool,
    /// What it publishes now, or what it listed when turned off.
    hostnames: Vec<String>,
}

impl AuditOperation for EnvironmentHostsAudit {
    fn operation_type(&self) -> String {
        if self.enabled {
            "INGRESS_ENVIRONMENT_HOSTS_ENABLED".to_string()
        } else {
            "INGRESS_ENVIRONMENT_HOSTS_DISABLED".to_string()
        }
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
    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_turning_environment_hosts_on_needs_step_up() {
        assert!(publishing_needs_step_up(false, true), "off -> on");
        assert!(!publishing_needs_step_up(true, true), "on -> on");
        assert!(!publishing_needs_step_up(true, false), "on -> off");
        assert!(!publishing_needs_step_up(false, false), "off -> off");
    }
}
