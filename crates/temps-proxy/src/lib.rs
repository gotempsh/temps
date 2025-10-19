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
pub mod traits;
pub mod proxy;
pub mod services;
pub mod server;
pub mod handler;
pub mod service;
pub mod plugin;
pub mod crawler_detector;
pub mod tls_cert_loader;
pub use temps_routes::{RouteInfo, CachedPeerTable, RouteTableListener};
pub use handler::*;
pub use crawler_detector::CrawlerDetector;

#[cfg(test)]
pub mod test_utils;
#[cfg(test)]
pub mod tests;
#[cfg(test)]
pub mod integration_test;
#[cfg(test)]
pub mod proxy_test;


// Re-export main types and functions
pub use config::*;
pub use traits::*;
pub use proxy::*;
pub use services::*;
pub use server::*;
pub use plugin::*;
