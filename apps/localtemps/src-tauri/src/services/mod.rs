//! Minimal service implementations for LocalTemps
//!
//! These are standalone implementations that don't depend on any temps-* crates.

pub mod analytics;
pub mod blob;
pub mod kv;
pub mod redis;
pub mod rustfs;

pub use analytics::AnalyticsService;
pub use blob::BlobService;
pub use kv::KvService;
pub use redis::RedisService;
pub use rustfs::RustfsService;
