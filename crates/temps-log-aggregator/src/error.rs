// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Typed error handling for the log aggregator crate

use thiserror::Error;
use uuid::Uuid;

#[derive(Error, Debug)]
pub enum LogAggregatorError {
    // ── Storage errors ──────────────────────────────────────────────────
    #[error("Failed to write chunk {chunk_id} for service '{service}' in project {project_id}: {reason}")]
    ChunkWriteFailed {
        chunk_id: Uuid,
        project_id: i32,
        service: String,
        reason: String,
    },

    #[error("Failed to read chunk {chunk_id} at key '{storage_key}': {reason}")]
    ChunkReadFailed {
        chunk_id: Uuid,
        storage_key: String,
        reason: String,
    },

    #[error("Chunk {chunk_id} not found at key '{storage_key}'")]
    ChunkNotFound { chunk_id: Uuid, storage_key: String },

    #[error("Failed to delete chunk {chunk_id} at key '{storage_key}': {reason}")]
    ChunkDeleteFailed {
        chunk_id: Uuid,
        storage_key: String,
        reason: String,
    },

    #[error("Failed to list chunks for project {project_id}, service '{service}': {reason}")]
    ChunkListFailed {
        project_id: i32,
        service: String,
        reason: String,
    },

    // ── Compression errors ──────────────────────────────────────────────
    #[error("Zstd compression failed for chunk {chunk_id}: {reason}")]
    CompressionFailed { chunk_id: Uuid, reason: String },

    #[error("Zstd decompression failed for chunk {chunk_id}: {reason}")]
    DecompressionFailed { chunk_id: Uuid, reason: String },

    // ── Database errors ─────────────────────────────────────────────────
    #[error("Database error in log aggregator: {0}")]
    Database(#[from] sea_orm::DbErr),

    // ── Docker errors ───────────────────────────────────────────────────
    #[error("Docker streaming error for container '{container_id}': {reason}")]
    DockerStreamFailed {
        container_id: String,
        reason: String,
    },

    #[error("Container '{container_id}' not found")]
    ContainerNotFound { container_id: String },

    // ── Search errors ───────────────────────────────────────────────────
    #[error("Search requires project_id and time range")]
    SearchMissingRequiredParams,

    #[error("Search time range exceeds maximum of {max_hours} hours for {search_type} search")]
    SearchTimeRangeExceeded { max_hours: u32, search_type: String },

    #[error("Invalid search cursor: {cursor}")]
    InvalidCursor { cursor: String },

    // ── Validation errors ───────────────────────────────────────────────
    #[error("Validation error: {message}")]
    Validation { message: String },

    // ── Configuration errors ────────────────────────────────────────────
    #[error("Storage configuration error: {message}")]
    StorageConfiguration { message: String },

    // ── I/O errors ──────────────────────────────────────────────────────
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    // ── Serialization errors ────────────────────────────────────────────
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    // ── S3 errors ───────────────────────────────────────────────────────
    #[error("S3 operation failed for bucket '{bucket}', key '{key}': {reason}")]
    S3 {
        bucket: String,
        key: String,
        reason: String,
    },
}

impl From<bollard::errors::Error> for LogAggregatorError {
    fn from(error: bollard::errors::Error) -> Self {
        LogAggregatorError::DockerStreamFailed {
            container_id: "unknown".to_string(),
            reason: error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display_includes_context() {
        let err = LogAggregatorError::ChunkWriteFailed {
            chunk_id: Uuid::nil(),
            project_id: 0,
            service: "web".to_string(),
            reason: "disk full".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("web"), "should include service name");
        assert!(msg.contains("disk full"), "should include reason");
    }

    #[test]
    fn test_search_missing_params_error() {
        let err = LogAggregatorError::SearchMissingRequiredParams;
        assert!(err.to_string().contains("project_id"));
    }

    #[test]
    fn test_s3_error_display() {
        let err = LogAggregatorError::S3 {
            bucket: "my-bucket".to_string(),
            key: "logs/test.ndjson.zst".to_string(),
            reason: "access denied".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("my-bucket"));
        assert!(msg.contains("logs/test.ndjson.zst"));
        assert!(msg.contains("access denied"));
    }
}
