// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use serde::Serialize;
use temps_core::AuditOperation;

/// Audit context for MCP-originated actions. Mirrors the per-crate
/// `AuditContext` pattern (e.g. `temps_projects::handlers::audit::AuditContext`)
/// rather than reusing it directly, since that module is crate-private.
#[derive(Debug, Clone, Serialize)]
pub struct AuditContext {
    pub user_id: i32,
    pub ip_address: Option<String>,
    pub user_agent: String,
}

/// Recorded when an MCP client's `confirm_action` call executes a proposed
/// `trigger_deployment` (ADR-039 propose-then-confirm write flow).
#[derive(Debug, Clone, Serialize)]
pub struct McpDeploymentTriggeredAudit {
    pub context: AuditContext,
    pub project_id: i32,
    pub environment_id: i32,
    pub branch: Option<String>,
}

impl AuditOperation for McpDeploymentTriggeredAudit {
    fn operation_type(&self) -> String {
        "MCP_DEPLOYMENT_TRIGGERED".to_string()
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

    fn serialize(&self) -> temps_core::anyhow::Result<String> {
        serde_json::to_string(self)
            .map_err(|e| temps_core::anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}
