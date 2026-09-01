// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! # temps-mcp-server
//!
//! Axum-native MCP (Model Context Protocol) server for Temps, per ADR-039.
//!
//! ## Architecture
//!
//! MCP routes live at the ROOT of the HTTP listener — not under `/api` — so
//! the CLI wizard's unauthenticated probe (`GET /mcp/tools`) works without
//! requiring an API key.  The plugin therefore registers its state in the
//! service registry and leaves `configure_routes`/`configure_public_routes`
//! returning `None`.  `console.rs` calls [`build_mcp_routers`] after plugin
//! initialisation and merges the result into `public_app` at root level.
//!
//! ## Propose-then-confirm
//!
//! Write tools never execute immediately.  They create a [`proposal::Proposal`]
//! token and return it to the LLM, which shows the token to the human.  The
//! human approves via `confirm_action { token }`, which is the only path that
//! executes the mutation.  Tokens expire after 5 minutes and are single-use.
//!
//! ## Feature flag
//!
//! The entire server is gated behind `AppSettings.mcp_server.enabled`
//! (default: `false`).  `GET /mcp/tools` returns `404` when the flag is off,
//! signalling the wizard that MCP is not yet configured on this instance.

pub mod access;
pub mod audit;
pub mod error;
pub mod handlers;
pub mod plugin;
pub mod proposal;
pub mod protocol;
pub mod registry;
pub mod tools;

use std::sync::Arc;

use axum::{
    routing::{delete, get, post},
    Router,
};
use temps_config::ConfigService;
use temps_core::project_access::ProjectAccessChecker;
use temps_deployments::DeploymentService;
use temps_projects::ProjectService;

pub use plugin::McpServerPlugin;

use proposal::ProposalStore;

/// Shared state injected into every MCP handler.
pub struct McpHandlerState {
    pub project_service: Arc<ProjectService>,
    pub deployment_service: Arc<DeploymentService>,
    pub config_service: Arc<ConfigService>,
    /// In-memory store for propose-then-confirm tokens.
    pub proposals: Arc<ProposalStore>,
    /// Audit sink for MCP-executed write actions (e.g. `confirm_action` ->
    /// `trigger_deployment`). CLAUDE.md requires audit logging on every write.
    pub audit_service: Arc<dyn temps_core::AuditLogger>,
    /// Optional project-scoped access checker.  `None` on instances without
    /// RBAC/team-based access configured — the absence means "allow all",
    /// matching the OSS default.  Retrieved via `get_service` (not
    /// `require_service`) because this dependency is legitimately optional.
    pub project_access_checker: Option<Arc<dyn ProjectAccessChecker>>,
}

/// The two sub-routers returned by [`build_mcp_routers`].
///
/// Both are fully state-bound (`Router<()>`) so they can be merged into any
/// other Axum router.
pub struct McpRouters {
    /// `GET /mcp/tools` — no auth required; safe to merge into any surface.
    pub public: Router,
    /// `POST /mcp`, `GET /mcp`, `DELETE /mcp` — require Bearer auth.
    ///
    /// The caller MUST apply the plugin middleware stack (which includes
    /// `auth_middleware`) to this router before merging it into the app.
    pub authenticated: Router,
}

/// Build the root-level MCP routers from the registered handler state.
///
/// Call this from `console.rs` after `plugin_manager.initialize_plugins()`.
/// The `authenticated` router still needs the middleware stack applied:
///
/// ```ignore
/// let mcp = temps_mcp_server::build_mcp_routers(state);
/// let auth_mcp = plugin_manager.apply_middleware_to_router(
///     mcp.authenticated,
///     plugin_manager.get_middleware(),
/// );
/// public_app = Router::new()
///     .merge(mcp.public)
///     .merge(auth_mcp)
///     .merge(public_app);
/// ```
pub fn build_mcp_routers(state: Arc<McpHandlerState>) -> McpRouters {
    // Public probe — no auth, no permission check.
    let public = Router::new()
        .route("/mcp/tools", get(handlers::tools_probe_handler))
        .with_state(state.clone());

    // Authenticated MCP endpoints — auth middleware is applied by the caller.
    let authenticated = Router::new()
        .route("/mcp", post(handlers::mcp_post_handler))
        .route("/mcp", get(handlers::mcp_get_handler))
        .route("/mcp", delete(handlers::mcp_delete_handler))
        .with_state(state);

    McpRouters {
        public,
        authenticated,
    }
}
