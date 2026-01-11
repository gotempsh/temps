//! temps-blob: Blob storage service for Temps platform
//!
//! Provides S3-compatible blob storage with project isolation.
//! Uses RustFS (S3-compatible storage) for high-performance object storage.

pub mod error;
pub mod handlers;
pub mod plugin;
pub mod services;

pub use error::BlobError;
pub use plugin::BlobPlugin;
pub use services::BlobService;
