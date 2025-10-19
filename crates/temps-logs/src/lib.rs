//! Temps Logs - Logging services for pipeline operations
//!
//! This crate provides two main logging services:
//!
//! ## File-based Logging (`file_logs`)
//! - Creating structured log files with date-based organization
//! - Appending to logs asynchronously
//! - Tailing logs in real-time
//! - Reading log content
//!
//! ## Docker Container Logging (`docker_logs`)
//! - Retrieving container logs efficiently
//! - Following container logs in real-time
//! - Checking container status
//! - Saving container logs to files

pub mod file_logs;
pub mod docker_logs;
pub mod plugin;

// Re-export the main types for convenience
pub use file_logs::LogService;
pub use docker_logs::{DockerLogService, DockerLogError};
pub use plugin::LogsPlugin;