//! External plugin system for loading standalone binary plugins.
//!
//! External plugins are standalone binaries that Temps discovers, spawns, and
//! communicates with over Unix domain sockets. This crate handles the Temps side:
//! - Discovery: scanning the plugins directory for binaries
//! - Lifecycle: spawning, handshaking, health-checking, and shutting down
//! - Proxying: forwarding HTTP requests to plugin processes
//! - Channel: bidirectional WebSocket for plugin queries and event delivery
//! - Event delivery: forwarding platform events to subscribing plugins
//! - API: listing plugin manifests via REST endpoint

pub mod channel;
pub mod event_listener;
pub mod handler;
pub mod host_api;
pub mod manager;
pub mod plugin;
pub mod proxy;
pub mod service;

pub use channel::PluginChannel;
pub use event_listener::PluginEventListener;
pub use plugin::ExternalPluginsPlugin;
pub use service::ExternalPluginsService;
