//! External plugin manifest types.
//!
//! External plugins are standalone binaries that Temps discovers, spawns, and
//! communicates with over Unix domain sockets. This module contains the shared
//! manifest types used by both the plugin SDK (`temps-plugin-sdk`) and the
//! plugin management crate (`temps-external-plugins`).
//!
//! The actual lifecycle management, proxying, and API handlers live in the
//! `temps-external-plugins` crate.

pub mod manifest;

pub use manifest::{
    HandshakeMessage, NavEntry, NavSection, PluginManifest, PluginManifestBuilder, PluginReady,
    UiManifest, UiRoute,
};
