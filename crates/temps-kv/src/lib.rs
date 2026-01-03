//! temps-kv: Key-Value storage service for Temps platform
//!
//! Provides a Redis-backed KV store with project isolation.
//! Uses Bollard to manage Redis containers on-demand.

pub mod error;
pub mod handlers;
pub mod plugin;
pub mod services;

pub use error::KvError;
pub use plugin::KvPlugin;
pub use services::KvService;
