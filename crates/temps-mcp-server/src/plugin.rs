// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use temps_config::ConfigService;
use temps_core::plugin::{PluginError, ServiceRegistrationContext, TempsPlugin};

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
            let config_service = context.require_service::<ConfigService>();
            let audit_service = context.require_service::<dyn temps_core::AuditLogger>();
            let state = Arc::new(McpHandlerState {
                virtual_cli: std::sync::OnceLock::new(),
                config_service,
                proposals: Arc::new(ProposalStore::new()),
                audit_service,
            });

            // Register as Arc<McpHandlerState> so console.rs can retrieve it
            // via `service_context.get_service::<McpHandlerState>()`.
            context.register_service(state);

            tracing::debug!("MCP server plugin services registered");
            Ok(())
        })
    }

    fn initialize_plugin_services<'a>(
        &'a self,
        context: &'a temps_core::plugin::PluginContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            let state = context.require_service::<McpHandlerState>();
            let _ = state
                .virtual_cli
                .set(Arc::new(crate::virtual_cli::VirtualCli {
                    api_tools: context.require_service::<temps_ai_api_tools::ApiToolsHandle>(),
                    write_api_tools: context
                        .require_service::<temps_ai_api_tools::WriteApiToolsHandle>(),
                    proposals: state.proposals.clone(),
                    audit_service: state.audit_service.clone(),
                }));
            Ok(())
        })
    }

    // configure_routes / configure_public_routes intentionally return None:
    // all MCP routes live at root level (not under /api) and are assembled
    // in console.rs via build_mcp_routers().
}
