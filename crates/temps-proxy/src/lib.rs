//! Temps Proxy - Pingora-based reverse proxy with advanced features
//!
//! This crate provides a high-performance reverse proxy built on Pingora
//! with features like:
//! - Dynamic certificate management
//! - Visitor and session tracking
//! - Load balancing
//! - Static file serving
//! - Request/response filtering

pub mod config;
pub mod crawler_detector;
pub mod handler;
pub mod plugin;
pub mod proxy;
pub mod server;
pub mod service;
pub mod services;
pub mod tls_cert_loader;
pub mod traits;
pub use crawler_detector::CrawlerDetector;
pub use handler::*;
pub use temps_routes::{CachedPeerTable, RouteInfo, RouteTableListener};

#[cfg(test)]
pub mod integration_test;
#[cfg(test)]
pub mod proxy_test;
#[cfg(test)]
pub mod test_utils;
#[cfg(test)]
pub mod tests;
#[cfg(test)]
pub mod e2e_static_test;

// Re-export main types and functions
pub use config::*;
pub use plugin::*;
pub use proxy::*;
pub use server::*;
pub use services::*;
pub use traits::*;
