//! temps-blob: Blob storage service for Temps platform
//!
//! Provides S3-compatible blob storage with project isolation.
//! Uses Bollard to manage MinIO containers on-demand.

pub mod error;
pub mod handlers;
pub mod plugin;
pub mod services;

pub use error::BlobError;
pub use plugin::BlobPlugin;
pub use services::BlobService;
