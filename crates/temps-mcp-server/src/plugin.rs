// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_config::ConfigService;
use temps_core::plugin::{PluginError, ServiceRegistrationContext, TempsPlugin};
use temps_core::project_access::ProjectAccessChecker;
use temps_deployments::DeploymentService;
use temps_projects::ProjectService;

use crate::proposal::ProposalStore;
use crate::McpHandlerState;

/// MCP server plugin (ADR-039).
///
/// Registers [`McpHandlerState`] as a service so `console.rs` can retrieve it
/// and attach the root-level MCP routes (`/mcp`, `/mcp/tools`) BEFORE the
/// `/api` prefix is applied.  The plugin itself exposes no `/api` routes.
pub struct McpServerPlugin;

impl McpServerPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for McpServerPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TempsPlugin for McpServerPlugin {
    fn name(&self) -> &'static str {
        "mcp-server"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let project_service = context.require_service::<ProjectService>();
            let deployment_service = context.require_service::<DeploymentService>();
            let config_service = context.require_service::<ConfigService>();
            let audit_service = context.require_service::<dyn temps_core::AuditLogger>();
            // Optional — not all instances have team-based project access configured.
            // `get_service` (not `require_service`) so startup succeeds on plain OSS.
            let project_access_checker = context.get_service::<dyn ProjectAccessChecker>();

            let state = Arc::new(McpHandlerState {
                project_service,
                deployment_service,
                config_service,
                proposals: Arc::new(ProposalStore::new()),
                audit_service,
                project_access_checker,
            });

            // Register as Arc<McpHandlerState> so console.rs can retrieve it
            // via `service_context.get_service::<McpHandlerState>()`.
            context.register_service(state);

            tracing::debug!("MCP server plugin services registered");
            Ok(())
        })
    }

    // configure_routes / configure_public_routes intentionally return None:
    // all MCP routes live at root level (not under /api) and are assembled
    // in console.rs via build_mcp_routers().
}
