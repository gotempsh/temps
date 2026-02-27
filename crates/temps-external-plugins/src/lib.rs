//! External plugin system for loading standalone binary plugins.
//!
//! External plugins are standalone binaries that Temps discovers, spawns, and
//! communicates with over Unix domain sockets. This crate handles the Temps side:
//! - Discovery: scanning the plugins directory for binaries
//! - Lifecycle: spawning, handshaking, health-checking, and shutting down
//! - Proxying: forwarding HTTP requests to plugin processes
//! - API: listing plugin manifests via REST endpoint

pub mod handler;
pub mod manager;
pub mod plugin;
pub mod proxy;
pub mod service;

pub use plugin::ExternalPluginsPlugin;
pub use service::ExternalPluginsService;
