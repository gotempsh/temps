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

/// A confirmed virtual CLI operation; never records executable parameters.
#[derive(Debug, Clone, Serialize)]
pub struct McpCliWriteAudit {
    pub context: AuditContext,
    pub operation_id: String,
    pub method: String,
    pub path: String,
    pub succeeded: bool,
}
impl AuditOperation for McpCliWriteAudit {
    fn operation_type(&self) -> String {
        "MCP_CLI_WRITE".into()
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
