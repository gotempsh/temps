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
