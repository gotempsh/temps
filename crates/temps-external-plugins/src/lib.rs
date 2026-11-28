// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! External plugin system.
//!
//! A plugin runs as an ordinary Temps service: someone deploys it like any
//! app and an administrator adds that service as a plugin. This crate is the
//! Temps side:
//! - Attaching: reading the plugin's manifest from its running container and
//!   following its deployments, restarts and idle stops
//! - Proxying: forwarding HTTP requests to the plugin
//! - Channel: bidirectional WebSocket for plugin queries and event delivery
//! - Event delivery: forwarding platform events to subscribing plugins
//! - API: listing plugins and managing what each may do
//!
//! Temps starts no plugin executable and no plugin container.

pub mod actor;
pub mod catalog;
pub mod channel;
pub mod endpoint;
pub mod event_listener;
pub mod grants;
pub mod handler;
pub mod host_api;
pub mod manager;
pub mod naming;
pub mod plugin;
pub mod proxy;
pub mod repository;
pub mod retired;
pub mod scope;
pub mod service;
pub mod service_runtime;
pub mod source_catalog;
pub mod tier;
pub mod trust;
pub mod updates;

pub use channel::PluginChannel;
pub use event_listener::PluginEventListener;
pub use plugin::ExternalPluginsPlugin;
pub use service::ExternalPluginsService;

pub mod ai_tools;

mod watches;
