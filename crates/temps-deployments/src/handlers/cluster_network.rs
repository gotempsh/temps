// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `GET`/`PUT /settings/cluster-network` (ADR 056): see and change the
//! cluster compute pool from the console. The logic lives in
//! [`ClusterNetworkService`]; these handlers check permissions, require a
//! step-up for the change, record the audit trail and map errors.

use std::sync::Arc;

use axum::{http::StatusCode, response::IntoResponse, Extension, Json};
use serde::Deserialize;
use temps_auth::{permission_guard, require_sensitive_action, RequireAuth};
use temps_core::problemdetails::{self, Problem};
use temps_core::{
    AuditContext, AuditLogger, RequestMetadata, SensitiveAction, SensitiveActionAuthorizer,
};
use temps_network::allocator::AllocatorError;
use tracing::error;
use utoipa::ToSchema;

use crate::handlers::audit::{
    ClusterComputePoolChangedAudit, ClusterComputePoolConfirmedAudit,
    ClusterOverlayNetworkNameChangedAudit,
};
use crate::services::cluster_network::{reassignments, ClusterNetworkError, ClusterNetworkService};
pub use crate::services::cluster_network::{
    ClusterNetworkChangeResponse, ClusterNetworkControlPlane, ClusterNetworkNode,
    ClusterNetworkReassignment, ClusterNetworkStatus,
};

/// What the cluster network handlers need, as a request extension.
#[derive(Clone)]
pub struct ClusterNetworkAdminState {
    pub service: Arc<ClusterNetworkService>,
    pub audit_service: Arc<dyn AuditLogger>,
    pub sensitive_action_authorizer: Arc<dyn SensitiveActionAuthorizer>,
}

/// Body of `PUT /settings/cluster-network`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateClusterNetworkRequest {
    /// New compute pool, an RFC 1918 IPv4 network such as `10.202.0.0/16`.
    pub compute_pool_cidr: String,
    /// Prefix of each node's subnet; keeps the current one when omitted.
    pub subnet_prefix_len: Option<u8>,
    /// New cluster overlay network name (ADR 059); keeps the current one
    /// when omitted. At most 9 characters: a lowercase letter, then lowercase
    /// letters, digits, `-` or `_`. Hosts build the renamed network beside
    /// the old one, which Temps never removes; containers move onto it as
    /// they are redeployed.
    #[serde(default)]
    pub overlay_network_name: Option<String>,
}

pub(crate) fn problem(error: ClusterNetworkError) -> Problem {
    let invalid = |detail: String| {
        problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Compute Pool")
            .with_detail(detail)
    };
    match error {
        ClusterNetworkError::InvalidCidr { .. } => invalid(error.to_string()),
        ClusterNetworkError::InvalidOverlayNetworkName(detail) => {
            problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Invalid Overlay Network Name")
                .with_detail(detail)
        }
        ClusterNetworkError::Allocator(error) => match error {
            AllocatorError::InvalidConfig { .. } | AllocatorError::PoolExhausted { .. } => {
                invalid(error.to_string())
            }
            AllocatorError::PoolOverlapsMesh { .. }
            | AllocatorError::PoolOverlapsUnderlay { .. }
            | AllocatorError::PoolOverlapsOccupied { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Compute Pool Collides")
                    .with_detail(error.to_string())
            }
            AllocatorError::PoolInUse { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Compute Pool In Use")
                .with_detail(error.to_string()),
            AllocatorError::ControlPlaneSetupInFlight { .. } => {
                problemdetails::new(StatusCode::CONFLICT)
                    .with_title("Control Plane Overlay Setup In Progress")
                    .with_detail(error.to_string())
            }
            other => {
                error!(error = %other, "cluster network: could not change the compute pool");
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Cluster Network Error")
                    .with_detail(format!(
                        "could not change the compute pool: {other}. Nothing was changed; retry, \
                         and check the server logs if it persists"
                    ))
            }
        },
        ClusterNetworkError::Database(source) => {
            error!(error = %source, "cluster network: database error");
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Cluster Network Error")
                .with_detail(
                    "could not read the cluster network from the database; retry, and check the \
                     server logs if it persists",
                )
        }
    }
}

/// The compute pool, whether it can change and what uses it, and each
/// member's cross-node overlay state.
#[utoipa::path(
    tag = "Nodes",
    get,
    path = "/settings/cluster-network",
    operation_id = "ClusterNetworkGet",
    responses(
        (status = 200, description = "Cluster network", body = ClusterNetworkStatus),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_cluster_network(
    RequireAuth(auth): RequireAuth,
    Extension(state): Extension<ClusterNetworkAdminState>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsRead);
    Ok(Json(state.service.status().await.map_err(problem)?))
}

/// Change the cluster compute pool. Allowed while no allocation is in use;
/// allocations handed out but never applied move to the new pool.
#[utoipa::path(
    tag = "Nodes",
    put,
    path = "/settings/cluster-network",
    operation_id = "ClusterNetworkUpdate",
    request_body = UpdateClusterNetworkRequest,
    responses(
        (status = 200, description = "Pool changed", body = ClusterNetworkChangeResponse),
        (status = 400, description = "Invalid pool or overlay network name, or the pool collides with the mesh pool, a node's address or a node's occupied range"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 409, description = "The pool is in use"),
        (status = 428, description = "Re-authentication required"),
        (status = 500, description = "Internal server error")
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_cluster_network(
    RequireAuth(auth): RequireAuth,
    Extension(state): Extension<ClusterNetworkAdminState>,
    Extension(metadata): Extension<RequestMetadata>,
    Json(request): Json<UpdateClusterNetworkRequest>,
) -> Result<impl IntoResponse, Problem> {
    permission_guard!(auth, SettingsWrite);
    require_sensitive_action(
        state.sensitive_action_authorizer.as_ref(),
        &auth,
        SensitiveAction::ChangeClusterComputePool,
    )
    .await?;
    let before = state.service.status().await.map_err(problem)?;
    let prefix_len = request
        .subnet_prefix_len
        .unwrap_or(before.subnet_prefix_len);
    // Checked before anything changes, so an invalid name leaves the pool
    // untouched too.
    if let Some(name) = request.overlay_network_name.as_deref() {
        temps_network::overlay_name::validate_overlay_network_name(name.trim()).map_err(|e| {
            problem(ClusterNetworkError::InvalidOverlayNetworkName(
                e.to_string(),
            ))
        })?;
    }
    let (change, first_confirmation) = state
        .service
        .change_pool(&request.compute_pool_cidr, prefix_len)
        .await
        .map_err(problem)?;
    let reassigned = reassignments(&change);
    let context = AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    };
    if let Some(name) = request.overlay_network_name.as_deref() {
        let previous = state
            .service
            .change_overlay_network_name(name)
            .await
            .map_err(problem)?;
        if previous != name.trim() {
            let audit = ClusterOverlayNetworkNameChangedAudit {
                context: context.clone(),
                from: previous,
                to: name.trim().to_owned(),
            };
            if let Err(error) = state.audit_service.create_audit_log(&audit).await {
                error!(%error, "cluster overlay network name changed but audit record failed");
            }
        }
    }
    let pool = format!(
        "{}/{}",
        change.config.compute_pool_cidr, change.config.subnet_prefix_len
    );
    if first_confirmation {
        let audit = ClusterComputePoolConfirmedAudit {
            context: context.clone(),
            pool: pool.clone(),
        };
        if let Err(error) = state.audit_service.create_audit_log(&audit).await {
            error!(%error, "cluster compute pool confirmed but audit record failed");
        }
    }
    if before.compute_pool_cidr != change.config.compute_pool_cidr.to_string()
        || before.subnet_prefix_len != change.config.subnet_prefix_len
    {
        let audit = ClusterComputePoolChangedAudit {
            context,
            from: format!("{}/{}", before.compute_pool_cidr, before.subnet_prefix_len),
            to: pool,
            reassigned: reassigned
                .iter()
                .map(|moved| format!("{}: {} -> {}", moved.node_name, moved.from, moved.to))
                .collect(),
        };
        if let Err(error) = state.audit_service.create_audit_log(&audit).await {
            error!(%error, "cluster compute pool changed but audit record failed");
        }
    }
    Ok(Json(ClusterNetworkChangeResponse {
        reassigned,
        status: state.service.status().await.map_err(problem)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_entities::{network_config, nodes};

    #[test]
    fn collisions_are_bad_requests_and_in_use_is_a_conflict() {
        let pool: ipnet::Ipv4Net = "10.201.0.0/16".parse().unwrap();
        let overlaps = problem(ClusterNetworkError::Allocator(
            AllocatorError::PoolOverlapsMesh { pool, mesh: pool },
        ));
        assert_eq!(overlaps.status_code, StatusCode::BAD_REQUEST);
        let in_use = problem(ClusterNetworkError::Allocator(AllocatorError::PoolInUse {
            current_pool: pool,
            requested_pool: pool,
            users: vec!["node 'a' (10.202.0.0/24, overlay up)".into()],
        }));
        assert_eq!(in_use.status_code, StatusCode::CONFLICT);
        let invalid = problem(ClusterNetworkError::InvalidCidr {
            raw: "nope".into(),
            reason: "invalid IP address syntax".into(),
        });
        assert_eq!(invalid.status_code, StatusCode::BAD_REQUEST);
    }

    fn config() -> network_config::Model {
        network_config::Model {
            id: 1,
            compute_pool_cidr: "172.20.0.0/16".into(),
            subnet_prefix_len: 24,
            transport: "vxlan".into(),
            vxlan_vni: 42,
            vxlan_port: 4789,
            underlay_mtu: 1500,
            control_plane_compute_cidr: None,
            control_plane_underlay_address: None,
            control_plane_overlay_ready: false,
            control_plane_setup_generation: 0,
            wireguard_enabled: true,
            wireguard_cidr: "10.201.0.0/16".into(),
            wireguard_port: 51820,
            control_plane_wg_public_key: None,
            control_plane_wg_endpoint: None,
            node_api_port: None,
            mesh_hub_node_id: None,
            mesh_hub_control_plane: false,
            compute_pool_confirmed_at: None,
            docker_network_name: "temps0".into(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn node(name: &str, status: Option<&str>, ready_cidr: Option<&str>) -> nodes::Model {
        let now = chrono::Utc::now();
        nodes::Model {
            id: 7,
            name: name.into(),
            token_hash: "h".into(),
            token_encrypted: None,
            address: "https://10.0.0.7:3100".into(),
            private_address: "10.0.0.7".into(),
            public_endpoint: None,
            wg_public_key: None,
            role: "worker".into(),
            status: "active".into(),
            labels: serde_json::json!({}),
            capacity: serde_json::json!({}),
            last_heartbeat: None,
            edge_public_key: None,
            compute_cidr: Some("172.20.0.0/24".into()),
            architecture: None,
            underlay_address: Some("10.0.0.7".into()),
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
            build_limits: None,
            public_ingress_enabled: false,
            public_ingress_running: None,
            public_ingress_last_error: None,
            public_ingress_certificate_count: None,
            public_ingress_route_count: None,
            public_ingress_unsupported_route_count: None,
            public_ingress_unsupported_reasons: serde_json::json!([]),
            occupied_ranges: Some(serde_json::json!([
                {"cidr": "172.20.0.0/16", "source": "route", "name": "br-f6051ccc7abf"}
            ])),
            overlay_status: status.map(str::to_owned),
            overlay_last_error: status
                .filter(|status| *status == "unavailable")
                .map(|_| "overlay subnet 172.20.0.0/24 ... Fix: ...".to_owned()),
            overlay_ready_cidr: ready_cidr.map(str::to_owned),
            overlay_remediation: None,
            overlay_network_name: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn a_node_that_never_applied_its_allocation_does_not_block_a_change() {
        let status = crate::services::cluster_network::status_of(
            &config(),
            &[node("edge-1", Some("unavailable"), None)],
            &Default::default(),
            Some("overlay subnet ... Fix: change the cluster compute pool".into()),
        );
        assert!(status.can_change, "{:?}", status.change_blocked_by);
        let edge = &status.nodes[0];
        assert!(!edge.in_use);
        assert!(edge.overlay_last_error.as_deref().unwrap().contains("Fix"));
        assert_eq!(edge.occupied_ranges[0].name, "br-f6051ccc7abf");
        // The control plane is not up, so its setup failure is shown.
        assert!(status.control_plane.setup_failure.is_some());
        assert_eq!(
            status.default_compute_pool_cidr,
            network_config::DEFAULT_COMPUTE_POOL_CIDR
        );
    }

    #[test]
    fn a_node_running_the_overlay_or_silent_blocks_a_change() {
        let ready = crate::services::cluster_network::status_of(
            &config(),
            &[node("edge-1", Some("ready"), Some("172.20.0.0/24"))],
            &Default::default(),
            None,
        );
        assert!(!ready.can_change);
        assert!(ready.change_blocked_by[0].contains("overlay up"));
        let silent = crate::services::cluster_network::status_of(
            &config(),
            &[node("old", None, None)],
            &Default::default(),
            None,
        );
        assert!(!silent.can_change);
        assert!(silent.change_blocked_by[0].contains("has not reported"));
    }

    #[test]
    fn a_node_whose_services_hold_addresses_blocks_a_change() {
        let edge = node("edge-1", Some("unavailable"), None);
        let status = crate::services::cluster_network::status_of(
            &config(),
            std::slice::from_ref(&edge),
            &[edge.id].into(),
            None,
        );
        assert!(!status.can_change);
        assert!(status.nodes[0].in_use);
        assert!(status.change_blocked_by[0].contains("runs services that hold addresses"));
    }
}
