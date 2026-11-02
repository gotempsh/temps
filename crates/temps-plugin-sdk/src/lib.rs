// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! # Temps Plugin SDK
//!
//! Build external Temps plugins as standalone binaries.
//!
//! An external plugin is a separate binary that Temps discovers in its `plugins/` directory,
//! spawns as a child process, and communicates with over a Unix domain socket.
//! The plugin provides API routes (axum Router) and optionally embeds UI assets.
//!
//! ## Architecture
//!
//! ```text
//! Temps (main process)
//!   │
//!   ├── Scans ~/.temps/plugins/ for binaries
//!   ├── Spawns each binary and sends its assertion secret over a one-shot pipe
//!   ├── Reads JSON manifest from stdout handshake
//!   ├── Opens WebSocket to plugin's /_temps/channel (bidirectional data channel)
//!   ├── Proxies /api/x/{plugin_name}/* → Unix socket
//!   ├── Extracts + serves UI assets at /x/{plugin_name}/*
//!   └── Registers nav entries in the shell
//! ```
//!
//! ## Usage
//!
//! ```rust,no_run
//! use temps_plugin_sdk::prelude::*;
//!
//! #[derive(Default)]
//! struct MyPlugin;
//!
//! async fn list_items() -> &'static str {
//!     "[]"
//! }
//!
//! impl ExternalPlugin for MyPlugin {
//!     fn manifest(&self) -> PluginManifest {
//!         PluginManifest::builder("my-plugin", "1.0.0")
//!             .display_name("My Plugin")
//!             .description("Does something useful")
//!             .nav(NavEntry {
//!                 label: "My Plugin".into(),
//!                 icon: "puzzle".into(),
//!                 section: NavSection::Platform,
//!                 path: "/my-plugin".into(),
//!                 order: 50,
//!             })
//!             .build()
//!     }
//!
//!     fn router(&self, ctx: PluginContext) -> axum::Router {
//!         axum::Router::new()
//!             .route("/items", axum::routing::get(list_items))
//!             .with_state(ctx)
//!     }
//! }
//!
//! temps_plugin_sdk::main!(MyPlugin);
//! ```

pub mod api;
pub mod auth;
pub mod client;
pub mod context;
pub mod error;
pub mod manifest;
pub mod protocol;
pub mod runtime;

pub use api::PlatformApi;
pub use auth::{AuthenticatedCaller, OptionalCaller, PluginPermission, PluginRole};
pub use client::TempsClient;
pub use context::PluginContext;
pub use error::PluginSdkError;
pub use manifest::{
    NavEntry, NavSection, PluginCapability, PluginEvent, PluginManifest, PluginManifestBuilder,
    UiManifest, PLUGIN_EVENTS_PATH,
};

/// Re-export commonly used types
pub mod prelude {
    pub use crate::api::PlatformApi;
    pub use crate::auth::{AuthenticatedCaller, OptionalCaller, PluginPermission, PluginRole};
    pub use crate::client::TempsClient;
    pub use crate::context::PluginContext;
    pub use crate::error::PluginSdkError;
    pub use crate::manifest::{
        NavEntry, NavSection, PluginCapability, PluginEvent, PluginManifest, UiManifest,
    };
    pub use crate::ExternalPlugin;

    // Re-export common dependencies for convenience
    pub use axum;

    /// The path plugins have always named the protocol types by. The SDK no
    /// longer depends on `temps-core`, so this is the only part of it left.
    pub mod temps_core {
        pub use temps_plugin_protocol as external_plugin;
    }
}

/// The trait that external plugin binaries implement.
///
/// This is the single entry point for defining an external plugin.
/// The plugin provides a manifest (metadata, nav entries), an axum Router (API routes),
/// and optionally embedded UI assets.
pub trait ExternalPlugin: Send + Sync + 'static {
    /// Returns the plugin's metadata manifest.
    ///
    /// This is sent to Temps during the handshake and used for:
    /// - Plugin identification and versioning
    /// - Navigation entries in the UI shell
    /// - API route prefix configuration
    /// - UI asset serving configuration
    fn manifest(&self) -> PluginManifest;

    /// Returns the axum Router for this plugin's API endpoints.
    ///
    /// Routes are relative — Temps mounts them under `/api/x/{plugin_name}/`.
    /// The `PluginContext` provides access to the platform client,
    /// auth validation, and other shared services.
    fn router(&self, ctx: PluginContext) -> axum::Router;

    /// Returns embedded UI assets as a tar.gz bundle, if any.
    ///
    /// Use `include_bytes!` in your `build.rs` to embed the UI bundle (not
    /// compiled here: `OUT_DIR` comes from the plugin's own build script):
    /// ```rust,ignore
    /// fn ui_assets(&self) -> Option<&'static [u8]> {
    ///     Some(include_bytes!(concat!(env!("OUT_DIR"), "/ui.tar.gz")))
    /// }
    /// ```
    ///
    /// The bundle should contain a `plugin-manifest.json` at its root
    /// describing the entry points and routes.
    fn ui_assets(&self) -> Option<&'static [u8]> {
        None
    }

    /// Returns an OpenAPI schema for this plugin's endpoints, if any.
    fn openapi_schema(&self) -> Option<utoipa::openapi::OpenApi> {
        None
    }

    /// Called once after the plugin is initialized but before it starts serving.
    /// Use this for one-time setup.
    fn on_start(&self, _ctx: &PluginContext) -> Result<(), PluginSdkError> {
        Ok(())
    }

    /// Called when the plugin is shutting down.
    /// Use this for cleanup (closing connections, flushing buffers).
    fn on_shutdown(&self) {}

    /// Called when Temps delivers a platform event the plugin subscribed to.
    ///
    /// Events are declared in the manifest via `.event("deployment.succeeded")`.
    /// Events are delivered over the platform channel (WebSocket).
    ///
    /// The default implementation does nothing. Override this to react to
    /// platform events (e.g., run an audit after a deployment succeeds).
    fn on_event(&self, _ctx: &PluginContext, _event: temps_plugin_protocol::PluginEvent) {
        // Default: no-op
    }
}

/// Macro that generates the `main()` function for a plugin binary.
///
/// This handles:
/// - Parsing host-supplied runtime arguments
/// - Setting up tracing/logging
/// - Writing the manifest to stdout (handshake)
/// - Starting the axum server on the Unix socket
/// - Accepting the platform channel (WebSocket) for data access
/// - Graceful shutdown on SIGTERM
///
/// # Usage
///
/// ```rust,no_run
/// use temps_plugin_sdk::prelude::*;
///
/// #[derive(Default)]
/// struct MyPlugin;
/// impl ExternalPlugin for MyPlugin {
///     fn manifest(&self) -> PluginManifest {
///         PluginManifest::builder("my-plugin", "1.0.0").build()
///     }
///     fn router(&self, _ctx: PluginContext) -> axum::Router {
///         axum::Router::new()
///     }
/// }
///
/// temps_plugin_sdk::main!(MyPlugin);
/// ```
#[macro_export]
macro_rules! main {
    ($plugin_type:ty) => {
        fn main() {
            $crate::runtime::run_plugin(<$plugin_type>::default());
        }
    };
    ($plugin_expr:expr) => {
        fn main() {
            $crate::runtime::run_plugin($plugin_expr);
        }
    };
}

#[cfg(test)]
mod tests {
    use super::prelude::*;

    /// Plugins name these types through the prelude's `temps_core` path; the
    /// example plugins do too. Both spellings must stay the one protocol type.
    #[test]
    fn prelude_keeps_the_protocol_path_plugins_already_use() {
        fn same_type(event: temps_core::external_plugin::PluginEvent) -> PluginEvent {
            event
        }
        let manifest: temps_core::external_plugin::manifest::PluginManifest =
            PluginManifest::builder("demo", "0.1.0").build();
        assert_eq!(manifest.name, "demo");
        let event = same_type(
            serde_json::from_str(
                r#"{"id":"evt-1","event_type":"project.created","timestamp":"2026-01-02T03:04:05Z","project_id":null,"data":{}}"#,
            )
            .unwrap(),
        );
        assert_eq!(event.id, "evt-1");
    }
}
