// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use serde::Serialize;
use temps_core::{AuditContext, AuditOperation};

#[derive(Debug, Clone, Serialize)]
pub struct DeploymentTokenRotatedAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub token_id: i32,
    pub token_name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CiBuildAudit {
    pub context: AuditContext,
    pub action: String,
    pub build_id: Option<uuid::Uuid>,
}

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

            fn serialize(&self) -> temps_core::AuditResult<String> {
                temps_core::serialize_audit_operation(self)
            }
        }
    };
}

impl_audit_operation!(DeploymentTokenRotatedAudit, "DEPLOYMENT_TOKEN_ROTATED");
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
    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
    }
}
