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

// ── Container action audits ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ContainerActionAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub environment_id: i32,
    pub container_id: String,
    pub action: String,
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

// ── AuditOperation implementations ──────────────────────────────────────────

macro_rules! impl_audit_operation {
    ($type:ty, $op:expr) => {
        impl AuditOperation for $type {
            fn operation_type(&self) -> String {
                $op.to_string()
            }

            fn user_id(&self) -> i32 {
                self.context.user_id
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
impl_audit_operation!(EnvironmentTeardownAudit, "ENVIRONMENT_TEARDOWN");
impl_audit_operation!(ContainerActionAudit, "CONTAINER_ACTION");
impl_audit_operation!(ExternalImagePushedAudit, "EXTERNAL_IMAGE_PUSHED");
impl_audit_operation!(DeploymentOperationAudit, "DEPLOYMENT_OPERATION_EXECUTED");
impl_audit_operation!(DeployFromImageAudit, "DEPLOY_FROM_IMAGE");
impl_audit_operation!(DeployFromStaticAudit, "DEPLOY_FROM_STATIC");
impl_audit_operation!(DeployFromImageUploadAudit, "DEPLOY_FROM_IMAGE_UPLOAD");
impl_audit_operation!(StaticBundleUploadedAudit, "STATIC_BUNDLE_UPLOADED");
impl_audit_operation!(ExternalImageRegisteredAudit, "EXTERNAL_IMAGE_REGISTERED");
impl_audit_operation!(ExternalImageDeletedAudit, "EXTERNAL_IMAGE_DELETED");
impl_audit_operation!(StaticBundleDeletedAudit, "STATIC_BUNDLE_DELETED");
impl_audit_operation!(DeploymentTokenRotatedAudit, "DEPLOYMENT_TOKEN_ROTATED");
