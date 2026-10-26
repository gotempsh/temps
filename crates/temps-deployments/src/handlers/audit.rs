// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use anyhow::Result;
use serde::Serialize;
use temps_core::{AuditContext, AuditOperation};

// ── Deployment lifecycle audits ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct DeploymentRollbackAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub deployment_id: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeploymentPausedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub deployment_id: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeploymentResumedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub deployment_id: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeploymentCancelledAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub deployment_id: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeploymentTeardownAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub deployment_id: i32,
}

/// A deployment received the host's Docker socket (ADR 045).
///
/// Recorded per deployment that actually got the mount, naming the project and
/// the host that granted it. The grant itself is not API-writable, so this is
/// the only durable record that a given container was root-equivalent on a
/// given machine — which is exactly what an operator reconstructing an
/// incident needs.
#[derive(Debug, Clone, Serialize)]
pub struct DeploymentDockerSocketMountedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub deployment_id: i32,
    /// Host that mounted it: a worker node's name, or `control-plane`.
    pub node: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EnvironmentTeardownAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub environment_id: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeploymentPromotedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub source_deployment_id: i32,
    pub target_environment_id: i32,
}

/// A user sent a redacted failure-trace report to the Temps team, or opened
/// a pre-filled GitHub issue, for a failed deployment. Never carries the
/// report content itself -- just that a report was made, and for what.
#[derive(Debug, Clone, Serialize)]
pub struct DeploymentFailureReportedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub deployment_id: i32,
    pub job_id: String,
}

// ── Container action audits ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ContainerActionAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub environment_id: i32,
    pub container_id: String,
    pub action: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContainerEnvironmentVariableRevealedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub environment_id: i32,
    pub container_id: String,
    pub variable_name: String,
}

// ── External image audits ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ExternalImagePushedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub image_ref: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeploymentOperationAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub deployment_id: String,
    pub operation: String,
}

// ── Remote deployment audits ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct DeployFromImageAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub environment_id: i32,
    pub image_ref: String,
    pub deployment_id: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeployFromStaticAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub environment_id: i32,
    pub deployment_id: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeployFromUploadedSourceAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub environment_id: i32,
    pub deployment_id: i32,
    pub source_bundle_id: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeployFromImageUploadAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub environment_id: i32,
    pub deployment_id: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct StaticBundleUploadedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub bundle_id: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExternalImageRegisteredAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub image_id: i32,
    pub image_ref: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExternalImageDeletedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub image_id: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct StaticBundleDeletedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub bundle_id: i32,
}

// ── Deployment token audits ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct DeploymentTokenRotatedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub token_id: i32,
    pub token_name: String,
}

// ── Node audits ─────────────────────────────────────────────────────────────

/// A node reported a container platform different from the one on record.
///
/// Worth auditing rather than only logging: the field decides where images are
/// placed, it is supplied by the node itself, and a change means either an
/// operator repointed a daemon or something is impersonating the node. `from`
/// is `None` the first time a node reports one.
#[derive(Debug, Clone, Serialize)]
pub struct NodeArchitectureChangedAudit {
    pub context: AuditContext,
    pub node_id: i32,
    pub node_name: String,
    pub from: Option<String>,
    pub to: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct NodePublicIngressChangedAudit {
    pub context: AuditContext,
    pub node_id: i32,
    pub enabled: bool,
}

/// ADR 058 D7: an operator checked whether a worker is reachable from the
/// internet (an outbound dial to its public address).
#[derive(Debug, Clone, Serialize)]
pub struct NodePublicIngressCheckedAudit {
    pub context: AuditContext,
    pub node_id: i32,
    pub target: Option<String>,
    pub ok: bool,
}

/// ADR 058 D7: where a worker listens for public traffic and what DNS should
/// point at changed.
#[derive(Debug, Clone, Serialize)]
pub struct NodePublicIngressSettingsChangedAudit {
    pub context: AuditContext,
    pub node_id: i32,
    pub enabled: bool,
    pub listen_address: Option<String>,
    pub http_port: u16,
    pub https_port: u16,
    pub public_address: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NodeBuildLimitsChangedAudit {
    pub context: AuditContext,
    pub node_id: i32,
    pub limits: temps_deployer::build_protocol::BuildLimits,
}

/// An operator removed a node, or asked to and was refused after Temps had
/// already cleaned up containers on it. Container removal on the host is a
/// write even when the node row stays.
#[derive(Debug, Clone, Serialize)]
pub struct NodeRemovalAudit {
    pub context: AuditContext,
    pub node_id: i32,
    pub node_name: String,
    /// Whether the operator chose to remove the node even with containers
    /// Temps could not confirm are gone.
    pub force: bool,
    /// `removed`, `refused_unconfirmed_containers`, `refused_still_serving`,
    /// `cleanup_interrupted` (containers may already have been removed; the
    /// counts say how many), `cleanup_failed` or `failed`.
    pub outcome: String,
    /// Leftover containers removed from the host, or confirmed already gone.
    pub containers_confirmed_gone: usize,
    /// Containers that may still exist on the host. With `force` they are
    /// recorded as orphaned.
    pub containers_unconfirmed: usize,
    /// Placements whose `target_nodes` named the node and were rewritten
    /// without it, each with the target nodes left.
    pub placements_updated: Vec<String>,
    /// Environments still serving from the node when it was removed as gone
    /// (`force` on an unreachable node, ADR 057); they need a redeploy.
    /// Listed up to 20, then a line counting the serving containers.
    pub lost_environments: Vec<String>,
    /// Whether the removal was allowed to let go of serving containers:
    /// `force` on a node with no heartbeat for 90 seconds.
    pub node_unreachable: bool,
}

/// An operator drained a node: redeploys were queued elsewhere and the
/// node's other containers removed and retired.
#[derive(Debug, Clone, Serialize)]
pub struct NodeDrainAudit {
    pub context: AuditContext,
    pub node_id: i32,
    pub node_name: String,
    /// `draining`, or `incomplete` when some redeploys could not be queued.
    pub outcome: String,
    pub redeployed_environments: usize,
    pub retired_containers: usize,
    pub failed_redeploys: usize,
}

/// An operator turned on the cluster's WireGuard mesh from the API.
#[derive(Debug, Clone, Serialize)]
pub struct WireguardMeshEnabledAudit {
    pub context: AuditContext,
    pub cidr: String,
    pub listen_port: u16,
}

/// An operator started pairing a node the control plane will dial (ADR 048
/// D2b): whoever runs the returned command at that address joins the mesh.
/// Also recorded when a worker that joins outbound only starts its own
/// pairing with a join token (ADR 055); it has no endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct NodePairingCreatedAudit {
    pub context: AuditContext,
    pub pairing_id: i32,
    pub name: String,
    pub node_endpoint: Option<String>,
}

/// A worker proved it is node `node_id` (its WireGuard key, name and current
/// agent credential) and started joining the same control plane again, which
/// updates that node in place when it registers (ADR 060).
#[derive(Debug, Clone, Serialize)]
pub struct NodeRejoinStartedAudit {
    pub context: AuditContext,
    pub node_id: i32,
    pub name: String,
    pub mesh_address: String,
}

/// An operator cancelled a pending node pairing.
#[derive(Debug, Clone, Serialize)]
pub struct NodePairingCancelledAudit {
    pub context: AuditContext,
    pub pairing_id: i32,
    pub name: String,
}

/// An operator made a mesh member the hub, or removed it (ADR 048 D4).
#[derive(Debug, Clone, Serialize)]
pub struct WireguardMeshHubChangedAudit {
    pub context: AuditContext,
    /// `none`, `control-plane` or `node <id>`.
    pub hub: String,
}

/// The cluster compute pool changed (ADR 056); never-applied allocations
/// moved to the new pool.
#[derive(Debug, Clone, Serialize)]
pub struct ClusterComputePoolChangedAudit {
    pub context: AuditContext,
    /// `<pool>/<per-node prefix>` before and after.
    pub from: String,
    pub to: String,
    /// `<node>: <old subnet> -> <new subnet>` per moved allocation.
    pub reassigned: Vec<String>,
}

/// An operator changed the cluster's overlay network name (ADR 059).
#[derive(Debug, Clone, Serialize)]
pub struct ClusterOverlayNetworkNameChangedAudit {
    pub context: AuditContext,
    pub from: String,
    pub to: String,
}

/// An operator confirmed the cluster compute pool for the first time
/// (ADR 056), whether or not they changed it.
#[derive(Debug, Clone, Serialize)]
pub struct ClusterComputePoolConfirmedAudit {
    pub context: AuditContext,
    /// `<pool>/<per-node prefix>` as confirmed.
    pub pool: String,
}

/// An operator started adding a server over SSH (ADR 048 D2c). The
/// credentials are not recorded; the host key they confirmed is.
#[derive(Debug, Clone, Serialize)]
pub struct NodeSshEnrollmentStartedAudit {
    pub context: AuditContext,
    pub enrollment_id: i32,
    pub pairing_id: i32,
    pub name: String,
    pub ssh_address: String,
    pub ssh_user: String,
    pub auth_method: String,
    pub host_key_fingerprint: String,
}

/// A server added over SSH joined and its agent runs. Recorded when the
/// background enrollment ends, with the context (user, IP, user agent) of
/// the operator who started it.
#[derive(Debug, Clone, Serialize)]
pub struct NodeSshEnrollmentSucceededAudit {
    pub context: AuditContext,
    pub enrollment_id: i32,
    pub pairing_id: i32,
    pub name: String,
    pub ssh_address: String,
    /// The node the server registered as.
    pub node_id: Option<i32>,
    /// `service` or `detached`.
    pub agent_mode: String,
}

/// Adding a server over SSH failed. Recorded when the background enrollment
/// ends, with the context of the operator who started it.
#[derive(Debug, Clone, Serialize)]
pub struct NodeSshEnrollmentFailedAudit {
    pub context: AuditContext,
    pub enrollment_id: i32,
    pub pairing_id: i32,
    pub name: String,
    pub ssh_address: String,
    /// The step it was on when it failed.
    pub step: String,
    /// Why it failed, as the enrollment shows it (secrets already masked).
    pub error: String,
}

/// An operator read a server's SSH host key (`POST /nodes/ssh/host-key`).
/// Not a write, but it opens a connection from the control plane to an
/// address the caller chooses, so every attempt is recorded, whatever its
/// outcome. Never carries credentials: reading a host key takes none.
#[derive(Debug, Clone, Serialize)]
pub struct NodeSshHostKeyProbedAudit {
    pub context: AuditContext,
    /// The host as the operator typed it.
    pub host: String,
    pub port: u16,
    /// `ip:port` it resolved to, when it did.
    pub address: Option<String>,
    /// `host_key_read`, `refused_target` (invalid or not allowed),
    /// `unreachable` (no SSH server answered) or `error`.
    pub outcome: String,
    /// The fingerprint read, when one was.
    pub fingerprint: Option<String>,
}

// ── Traefik discovery audits ────────────────────────────────────────────────

/// An operator suppressed or restored a single Traefik-discovered route.
///
/// Worth auditing rather than only logging: flipping this flag changes which
/// hostname the proxy serves for a container nobody deployed through Temps,
/// and it is the one write operation on the discovery surface.
#[derive(Debug, Clone, Serialize)]
pub struct TraefikDiscoveredRouteToggledAudit {
    pub context: AuditContext,
    pub host: String,
    pub container_name: String,
    pub network: String,
    /// The value after the change.
    pub enabled: bool,
}

/// Operator authorized Temps to issue an ACME certificate for a discovered
/// Traefik route (Path A of ADR-041).
#[derive(Debug, Clone, Serialize)]
pub struct TraefikDiscoveredRouteCertRequestedAudit {
    pub context: AuditContext,
    /// The hostname being authorized.
    pub host: String,
    /// Container that was serving the host at authorization time.
    pub container_id: String,
    pub container_name: String,
    /// "http-01" or "dns-01".
    pub renewal_method: String,
    /// DNS zone supplied for DNS-01 challenges, absent for HTTP-01.
    pub dns01_zone: Option<String>,
}

/// Operator imported an existing certificate from a Traefik `acme.json` file
/// for a discovered route (Path B of ADR-041).
#[derive(Debug, Clone, Serialize)]
pub struct TraefikDiscoveredRouteCertImportedAudit {
    pub context: AuditContext,
    /// Hosts successfully imported in this call.
    pub imported_hosts: Vec<String>,
    /// Hosts that were present in the acme.json but failed validation.
    pub failed_hosts: Vec<String>,
    /// Total number of entries parsed from the document.
    pub entries_parsed: usize,
}

/// Operator removed TLS authorization from a discovered Traefik route.
#[derive(Debug, Clone, Serialize)]
pub struct TraefikDiscoveredRouteCertDeauthorizedAudit {
    pub context: AuditContext,
    pub host: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeHealthPolicyUpdatedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub environment_id: i32,
    pub enabled: bool,
    pub paused: bool,
    pub required_healthy: Option<i32>,
}

// ── AuditOperation implementations ──────────────────────────────────────────

macro_rules! impl_audit_operation {
    ($type:ty, $op:expr) => {
        impl AuditOperation for $type {
            fn operation_type(&self) -> String {
                $op.to_string()
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

            fn serialize(&self) -> Result<String> {
                serde_json::to_string(self)
                    .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
            }
        }
    };
}

impl_audit_operation!(
    RuntimeHealthPolicyUpdatedAudit,
    "RUNTIME_HEALTH_POLICY_UPDATED"
);
impl_audit_operation!(DeploymentRollbackAudit, "DEPLOYMENT_ROLLBACK");
impl_audit_operation!(DeploymentPausedAudit, "DEPLOYMENT_PAUSED");
impl_audit_operation!(DeploymentResumedAudit, "DEPLOYMENT_RESUMED");
impl_audit_operation!(DeploymentCancelledAudit, "DEPLOYMENT_CANCELLED");
impl_audit_operation!(DeploymentTeardownAudit, "DEPLOYMENT_TEARDOWN");
impl_audit_operation!(DeploymentPromotedAudit, "DEPLOYMENT_PROMOTED");
impl_audit_operation!(
    DeploymentFailureReportedAudit,
    "DEPLOYMENT_FAILURE_REPORTED"
);
impl_audit_operation!(
    DeploymentDockerSocketMountedAudit,
    "DEPLOYMENT_DOCKER_SOCKET_MOUNTED"
);
impl_audit_operation!(EnvironmentTeardownAudit, "ENVIRONMENT_TEARDOWN");
impl_audit_operation!(ContainerActionAudit, "CONTAINER_ACTION");
impl_audit_operation!(
    ContainerEnvironmentVariableRevealedAudit,
    "CONTAINER_ENVIRONMENT_VARIABLE_REVEALED"
);
impl_audit_operation!(ExternalImagePushedAudit, "EXTERNAL_IMAGE_PUSHED");
impl_audit_operation!(DeploymentOperationAudit, "DEPLOYMENT_OPERATION_EXECUTED");
impl_audit_operation!(DeployFromImageAudit, "DEPLOY_FROM_IMAGE");
impl_audit_operation!(DeployFromStaticAudit, "DEPLOY_FROM_STATIC");
impl_audit_operation!(DeployFromUploadedSourceAudit, "DEPLOY_FROM_UPLOADED_SOURCE");
impl_audit_operation!(DeployFromImageUploadAudit, "DEPLOY_FROM_IMAGE_UPLOAD");
impl_audit_operation!(StaticBundleUploadedAudit, "STATIC_BUNDLE_UPLOADED");
impl_audit_operation!(ExternalImageRegisteredAudit, "EXTERNAL_IMAGE_REGISTERED");
impl_audit_operation!(ExternalImageDeletedAudit, "EXTERNAL_IMAGE_DELETED");
impl_audit_operation!(StaticBundleDeletedAudit, "STATIC_BUNDLE_DELETED");
impl_audit_operation!(DeploymentTokenRotatedAudit, "DEPLOYMENT_TOKEN_ROTATED");
impl_audit_operation!(NodeArchitectureChangedAudit, "NODE_ARCHITECTURE_CHANGED");
impl_audit_operation!(NodePublicIngressChangedAudit, "NODE_PUBLIC_INGRESS_CHANGED");
impl_audit_operation!(
    NodePublicIngressSettingsChangedAudit,
    "NODE_PUBLIC_INGRESS_SETTINGS_CHANGED"
);
impl_audit_operation!(NodePublicIngressCheckedAudit, "NODE_PUBLIC_INGRESS_CHECKED");
impl_audit_operation!(NodeRemovalAudit, "NODE_REMOVAL");
impl_audit_operation!(NodeDrainAudit, "NODE_DRAIN");
impl_audit_operation!(WireguardMeshEnabledAudit, "WIREGUARD_MESH_ENABLED");
impl_audit_operation!(WireguardMeshHubChangedAudit, "WIREGUARD_MESH_HUB_CHANGED");
impl_audit_operation!(
    ClusterComputePoolChangedAudit,
    "CLUSTER_COMPUTE_POOL_CHANGED"
);
impl_audit_operation!(
    ClusterComputePoolConfirmedAudit,
    "CLUSTER_COMPUTE_POOL_CONFIRMED"
);
impl_audit_operation!(
    ClusterOverlayNetworkNameChangedAudit,
    "CLUSTER_OVERLAY_NETWORK_NAME_CHANGED"
);
impl_audit_operation!(NodePairingCreatedAudit, "NODE_PAIRING_CREATED");
impl_audit_operation!(NodePairingCancelledAudit, "NODE_PAIRING_CANCELLED");
impl_audit_operation!(NodeRejoinStartedAudit, "NODE_REJOIN_STARTED");
impl_audit_operation!(NodeSshEnrollmentStartedAudit, "NODE_SSH_ENROLLMENT_STARTED");
impl_audit_operation!(
    NodeSshEnrollmentSucceededAudit,
    "NODE_SSH_ENROLLMENT_SUCCEEDED"
);
impl_audit_operation!(NodeSshEnrollmentFailedAudit, "NODE_SSH_ENROLLMENT_FAILED");
impl_audit_operation!(NodeSshHostKeyProbedAudit, "NODE_SSH_HOST_KEY_PROBED");
impl_audit_operation!(
    TraefikDiscoveredRouteToggledAudit,
    "TRAEFIK_DISCOVERED_ROUTE_TOGGLED"
);
impl_audit_operation!(
    TraefikDiscoveredRouteCertRequestedAudit,
    "TRAEFIK_DISCOVERED_ROUTE_CERT_REQUESTED"
);
impl_audit_operation!(
    TraefikDiscoveredRouteCertImportedAudit,
    "TRAEFIK_DISCOVERED_ROUTE_CERT_IMPORTED"
);
impl_audit_operation!(
    TraefikDiscoveredRouteCertDeauthorizedAudit,
    "TRAEFIK_DISCOVERED_ROUTE_CERT_DEAUTHORIZED"
);

/// A node replaced the WireGuard mesh key it had registered.
///
/// Recorded rather than only logged: the key is what every mesh member
/// trusts as this node, it is supplied by the node itself, and a change
/// means either the agent re-keyed (a reinstall, a lost key file) or
/// something holding the node's token took its place on the mesh. Public
/// keys are not secret, so both are kept in full to compare with
/// `wg show` on the node. Not recorded on a node's first registration or
/// when only the endpoint moves.
#[derive(Debug, Clone, Serialize)]
pub struct NodeMeshKeyChangedAudit {
    pub context: AuditContext,
    pub node_id: i32,
    pub node_name: String,
    pub old_public_key: String,
    pub new_public_key: String,
    pub old_endpoint: Option<String>,
    /// `None` for a node nobody dials (ADR 055).
    pub new_endpoint: Option<String>,
}

impl_audit_operation!(NodeMeshKeyChangedAudit, "NODE_MESH_KEY_CHANGED");

impl_audit_operation!(NodeBuildLimitsChangedAudit, "NODE_BUILD_LIMITS_CHANGED");

#[derive(Debug, Clone, Serialize)]
pub struct CiBuildAudit {
    pub context: AuditContext,
    pub action: String,
    pub build_id: Option<uuid::Uuid>,
}
impl AuditOperation for CiBuildAudit {
    fn operation_type(&self) -> String {
        "CI_BUILD_DISPATCH".into()
    }
    fn user_id(&self) -> Option<i32> {
        (self.context.user_id > 0).then_some(self.context.user_id)
    }
    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }
    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }
    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|error| anyhow::anyhow!("Cannot serialize CI dispatch audit: {error}"))
    }
}

#[cfg(test)]
mod node_audit_tests {
    use super::*;

    fn context() -> AuditContext {
        AuditContext {
            user_id: 3,
            ip_address: None,
            user_agent: "temps-api".to_string(),
        }
    }

    /// Node removal records who removed which node, whether they forced it,
    /// and what happened to the containers on the host.
    #[test]
    fn node_removal_audit_records_force_outcome_and_containers() {
        let audit = NodeRemovalAudit {
            context: context(),
            node_id: 7,
            node_name: "worker-a".to_string(),
            force: true,
            outcome: "removed".to_string(),
            containers_confirmed_gone: 4,
            containers_unconfirmed: 2,
            placements_updated: vec![
                "environment 'production' (id 4) of project 'shop' now targets [0, 1]".to_string(),
            ],
            lost_environments: vec!["environment 'prod' of project 'api'".to_string()],
            node_unreachable: true,
        };
        assert_eq!(audit.operation_type(), "NODE_REMOVAL");
        assert_eq!(AuditOperation::user_id(&audit), Some(3));
        let json: serde_json::Value =
            serde_json::from_str(&AuditOperation::serialize(&audit).unwrap()).unwrap();
        assert_eq!(json["node_id"], 7);
        assert_eq!(json["force"], true);
        assert_eq!(json["outcome"], "removed");
        assert_eq!(json["containers_confirmed_gone"], 4);
        assert_eq!(json["containers_unconfirmed"], 2);
        assert_eq!(
            json["placements_updated"][0],
            "environment 'production' (id 4) of project 'shop' now targets [0, 1]"
        );
        assert_eq!(
            json["lost_environments"][0],
            "environment 'prod' of project 'api'"
        );
        assert_eq!(json["node_unreachable"], true);
    }

    #[test]
    fn node_drain_audit_records_what_moved() {
        let audit = NodeDrainAudit {
            context: context(),
            node_id: 7,
            node_name: "worker-a".to_string(),
            outcome: "incomplete".to_string(),
            redeployed_environments: 2,
            retired_containers: 5,
            failed_redeploys: 1,
        };
        assert_eq!(audit.operation_type(), "NODE_DRAIN");
        let json: serde_json::Value =
            serde_json::from_str(&AuditOperation::serialize(&audit).unwrap()).unwrap();
        assert_eq!(json["outcome"], "incomplete");
        assert_eq!(json["redeployed_environments"], 2);
        assert_eq!(json["retired_containers"], 5);
        assert_eq!(json["failed_redeploys"], 1);
    }
}

/// An admin changed an environment's `workload_mode` (ADR-055 Phase 5).
/// Moving back to `legacy` is the rollback, so both values are recorded.
#[derive(Debug, Clone, Serialize)]
pub struct EnvironmentWorkloadModeChangedAudit {
    pub context: AuditContext,
    pub environment_id: i32,
    pub previous_mode: String,
    pub workload_mode: String,
}

impl_audit_operation!(
    EnvironmentWorkloadModeChangedAudit,
    "ENVIRONMENT_WORKLOAD_MODE_CHANGED"
);

/// An admin changed the isolation an environment's plane-run deployments
/// request (ADR-055 Phase 10). `process` is the rollback.
#[derive(Debug, Clone, Serialize)]
pub struct EnvironmentWorkloadIsolationChangedAudit {
    pub context: AuditContext,
    pub environment_id: i32,
    pub previous_isolation: String,
    pub workload_isolation: String,
}

impl_audit_operation!(
    EnvironmentWorkloadIsolationChangedAudit,
    "ENVIRONMENT_WORKLOAD_ISOLATION_CHANGED"
);

/// An admin changed a node's resource policy or system reserve (node 0 is
/// the control plane).
#[derive(Debug, Clone, Serialize)]
pub struct NodeResourcesChangedAudit {
    pub context: AuditContext,
    pub node_id: i32,
    pub previous_resource_policy: String,
    pub resource_policy: String,
    pub previous_system_reserved: temps_entities::node_resource_settings::SystemReserved,
    pub system_reserved: temps_entities::node_resource_settings::SystemReserved,
}

impl_audit_operation!(NodeResourcesChangedAudit, "NODE_RESOURCES_CHANGED");

/// An admin created a volume for an environment (ADR-055 Phase 9).
#[derive(Debug, Clone, Serialize)]
pub struct EnvironmentVolumeCreatedAudit {
    pub context: AuditContext,
    pub environment_id: i32,
    pub volume_id: i64,
    pub name: String,
    pub size_mb: Option<u64>,
    pub retain_on_delete: bool,
}

impl_audit_operation!(EnvironmentVolumeCreatedAudit, "ENVIRONMENT_VOLUME_CREATED");

/// An admin attached (`mount_path` set) or detached (`None`) an
/// environment's volume.
#[derive(Debug, Clone, Serialize)]
pub struct EnvironmentVolumeAttachmentChangedAudit {
    pub context: AuditContext,
    pub environment_id: i32,
    pub volume_id: i64,
    pub name: String,
    pub mount_path: Option<String>,
    pub read_only: bool,
}

impl_audit_operation!(
    EnvironmentVolumeAttachmentChangedAudit,
    "ENVIRONMENT_VOLUME_ATTACHMENT_CHANGED"
);

/// An admin deleted an environment's volume, and whether its data went
/// with it.
#[derive(Debug, Clone, Serialize)]
pub struct EnvironmentVolumeDeletedAudit {
    pub context: AuditContext,
    pub environment_id: i32,
    pub volume_id: i64,
    pub name: String,
    pub backing_removed: bool,
    pub backing_kept_reason: Option<String>,
}

impl_audit_operation!(EnvironmentVolumeDeletedAudit, "ENVIRONMENT_VOLUME_DELETED");
