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
    /// `removed`, `refused_unconfirmed_containers` or `failed`.
    pub outcome: String,
    /// Leftover containers removed from the host, or confirmed already gone.
    pub containers_confirmed_gone: usize,
    /// Containers that may still exist on the host. With `force` they are
    /// recorded as orphaned.
    pub containers_unconfirmed: usize,
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
#[derive(Debug, Clone, Serialize)]
pub struct NodePairingCreatedAudit {
    pub context: AuditContext,
    pub pairing_id: i32,
    pub name: String,
    pub node_endpoint: String,
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
impl_audit_operation!(NodeRemovalAudit, "NODE_REMOVAL");
impl_audit_operation!(NodeDrainAudit, "NODE_DRAIN");
impl_audit_operation!(WireguardMeshEnabledAudit, "WIREGUARD_MESH_ENABLED");
impl_audit_operation!(WireguardMeshHubChangedAudit, "WIREGUARD_MESH_HUB_CHANGED");
impl_audit_operation!(NodePairingCreatedAudit, "NODE_PAIRING_CREATED");
impl_audit_operation!(NodePairingCancelledAudit, "NODE_PAIRING_CANCELLED");
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
    pub new_endpoint: String,
}

impl_audit_operation!(NodeMeshKeyChangedAudit, "NODE_MESH_KEY_CHANGED");

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
