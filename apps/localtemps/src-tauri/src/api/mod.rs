//! LocalTemps API Server
//!
//! Provides SDK-compatible API endpoints for KV and Blob operations,
//! as well as analytics event capture for @temps-sdk/react-analytics.

pub mod analytics;
pub mod auth;
pub mod autoinit;
pub mod blob;
pub mod kv;
pub mod server;

pub use server::create_api_server;
