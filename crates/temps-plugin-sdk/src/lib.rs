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
//!   ├── Spawns each binary with --socket-path and --database-url
//!   ├── Reads JSON manifest from stdout handshake
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
//! struct MyPlugin;
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

pub mod context;
pub mod error;
pub mod manifest;
pub mod protocol;
pub mod runtime;

pub use context::PluginContext;
pub use error::PluginSdkError;
pub use manifest::{NavEntry, NavSection, PluginManifest, PluginManifestBuilder, UiManifest};

/// Re-export commonly used types
pub mod prelude {
    pub use crate::context::PluginContext;
    pub use crate::error::PluginSdkError;
    pub use crate::manifest::{NavEntry, NavSection, PluginManifest, UiManifest};
    pub use crate::ExternalPlugin;

    // Re-export axum for convenience
    pub use axum;
    pub use sea_orm;
    pub use temps_core;
    pub use temps_entities;
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
    /// The `PluginContext` provides access to the database connection,
    /// auth validation, and other shared services.
    fn router(&self, ctx: PluginContext) -> axum::Router;

    /// Returns embedded UI assets as a tar.gz bundle, if any.
    ///
    /// Use `include_bytes!` in your `build.rs` to embed the UI bundle:
    /// ```rust,no_run
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
    /// Use this for one-time setup like running migrations.
    fn on_start(&self, _ctx: &PluginContext) -> Result<(), PluginSdkError> {
        Ok(())
    }

    /// Called when the plugin is shutting down.
    /// Use this for cleanup (closing connections, flushing buffers).
    fn on_shutdown(&self) {}
}

/// Macro that generates the `main()` function for a plugin binary.
///
/// This handles:
/// - Parsing CLI arguments (--socket-path, --database-url, --auth-secret)
/// - Setting up tracing/logging
/// - Connecting to the database
/// - Writing the manifest to stdout (handshake)
/// - Starting the axum server on the Unix socket
/// - Graceful shutdown on SIGTERM
///
/// # Usage
///
/// ```rust,no_run
/// use temps_plugin_sdk::prelude::*;
///
/// struct MyPlugin;
/// impl ExternalPlugin for MyPlugin {
///     fn manifest(&self) -> PluginManifest {
///         PluginManifest::builder("my-plugin", "1.0.0").build()
///     }
///     fn router(&self, ctx: PluginContext) -> axum::Router {
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
