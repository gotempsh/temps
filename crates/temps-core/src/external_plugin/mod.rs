//! External plugin system for loading standalone binary plugins.
//!
//! External plugins are standalone binaries that Temps discovers, spawns, and
//! communicates with over Unix domain sockets. This module handles the Temps side:
//! - Discovery: scanning the plugins directory for binaries
//! - Lifecycle: spawning, handshaking, health-checking, and shutting down
//! - Proxying: forwarding HTTP requests to plugin processes
//! - UI: extracting and serving embedded UI assets
//!
//! Plugin binaries use the `temps-plugin-sdk` crate to implement the other side
//! of this protocol.

pub mod manager;
pub mod manifest;
pub mod proxy;

pub use manager::{ExternalPluginConfig, ExternalPluginManager};
pub use manifest::{
    HandshakeMessage, NavEntry, NavSection, PluginManifest, PluginManifestBuilder, PluginReady,
    UiManifest, UiRoute,
};
pub use proxy::PluginProxy;
