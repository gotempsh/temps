//! Structured log aggregation for temps.sh
//!
//! This crate provides:
//! - Docker container log collection via daemon streaming
//! - Pluggable storage backends (filesystem and S3-compatible)
//! - Structured log parsing and level detection
//! - NDJSON chunk storage with zstd compression
//! - TimescaleDB metadata indexing for ERROR/WARN log events
//! - Search API with time range enforcement and cursor-based pagination
//! - Live tail via SSE for real-time log streaming
//! - Retention management with per-project policies

pub mod error;
pub mod handlers;
pub mod parser;
pub mod plugin;
pub mod services;
pub mod storage;
pub mod types;

// Re-export primary types
pub use error::LogAggregatorError;
pub use plugin::LogAggregatorPlugin;
pub use services::{
    ChunkWriterService, CollectorService, FlushResult, LogMetadataService, LogSearchService,
    RemoteContainerInfo, RemoteContainerLogSource, RemoteLogCollectorService, RemoteLogSourceError,
    RemoteLogStream, RetentionService, TailService,
};
pub use storage::{FilesystemStorage, LogStorage, S3Storage};
pub use types::*;
