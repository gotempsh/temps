//! LocalTemps API Server
//!
//! Provides SDK-compatible API endpoints for KV and Blob operations.

pub mod auth;
pub mod autoinit;
pub mod blob;
pub mod kv;
pub mod server;

pub use server::create_api_server;
