// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Retention service for cleaning up expired log data
//!
//! - Nightly job: deletes S3/filesystem chunks where ended_at < NOW() - retention_interval
//! - Never deletes metadata until storage object confirmed deleted
//! - Manual purge API for GDPR compliance

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use tracing::{debug, error, info, warn};

use crate::error::LogAggregatorError;
use crate::services::LogMetadataService;
use crate::storage::LogStorage;
use crate::types::RetentionConfig;

/// Result of a retention cleanup run
#[derive(Debug, Clone)]
pub struct RetentionResult {
    /// Number of chunks deleted from storage
    pub chunks_deleted: u64,
    /// Number of chunks that failed to delete
    pub chunks_failed: u64,
    /// Total bytes reclaimed from storage
    pub bytes_reclaimed: u64,
}

/// Service for managing log data retention.
pub struct RetentionService {
    storage: Arc<dyn LogStorage>,
    metadata_service: Arc<LogMetadataService>,
}

impl RetentionService {
    pub fn new(storage: Arc<dyn LogStorage>, metadata_service: Arc<LogMetadataService>) -> Self {
        Self {
            storage,
            metadata_service,
        }
    }

    /// Run retention cleanup for a specific project.
    ///
    /// Deletes chunks older than the configured retention period.
    /// Storage object is deleted first; metadata row is deleted only after confirmed.
    pub async fn cleanup_project(
        &self,
        project_id: i32,
        config: &RetentionConfig,
    ) -> Result<RetentionResult, LogAggregatorError> {
        let cutoff = Utc::now() - Duration::days(config.chunk_retention_days as i64);
        let expired_chunks = self
            .metadata_service
            .find_expired_chunks(project_id, cutoff)
            .await?;

        if expired_chunks.is_empty() {
            return Ok(RetentionResult {
                chunks_deleted: 0,
                chunks_failed: 0,
                bytes_reclaimed: 0,
            });
        }

        info!(
            project_id = %project_id,
            chunk_count = expired_chunks.len(),
            cutoff = %cutoff,
            "Starting retention cleanup"
        );

        let mut deleted = 0u64;
        let mut failed = 0u64;
        let mut bytes = 0u64;

        for chunk in &expired_chunks {
            // Step 1: Delete from storage
            match self.storage.delete_chunk(&chunk.storage_key).await {
                Ok(()) => {
                    // Step 2: Only delete metadata after confirmed storage deletion
                    match self.metadata_service.delete_chunk_meta(chunk.id).await {
                        Ok(()) => {
                            deleted += 1;
                            bytes += chunk.compressed_size_bytes as u64;
                            debug!(
                                chunk_id = %chunk.id,
                                storage_key = chunk.storage_key,
                                "Deleted expired chunk"
                            );
                        }
                        Err(e) => {
                            // Storage deleted but metadata remains — will be retried next run
                            warn!(
                                chunk_id = %chunk.id,
                                error = %e,
                                "Deleted chunk from storage but failed to delete metadata"
                            );
                            failed += 1;
                        }
                    }
                }
                Err(e) => {
                    error!(
                        chunk_id = %chunk.id,
                        storage_key = chunk.storage_key,
                        error = %e,
                        "Failed to delete chunk from storage"
                    );
                    failed += 1;
                }
            }
        }

        info!(
            project_id = %project_id,
            deleted = deleted,
            failed = failed,
            bytes_reclaimed = bytes,
            "Retention cleanup completed"
        );

        Ok(RetentionResult {
            chunks_deleted: deleted,
            chunks_failed: failed,
            bytes_reclaimed: bytes,
        })
    }

    /// Manual purge: delete all log data for a project before a given timestamp.
    ///
    /// Used for GDPR compliance or accidental sensitive data logging.
    /// Deletes both S3 chunks and log_events rows within the time range.
    pub async fn manual_purge(
        &self,
        project_id: i32,
        before: DateTime<Utc>,
    ) -> Result<RetentionResult, LogAggregatorError> {
        info!(
            project_id = %project_id,
            before = %before,
            "Starting manual purge"
        );

        let chunks = self
            .metadata_service
            .find_expired_chunks(project_id, before)
            .await?;

        let mut deleted = 0u64;
        let mut failed = 0u64;
        let mut bytes = 0u64;

        for chunk in &chunks {
            match self.storage.delete_chunk(&chunk.storage_key).await {
                Ok(()) => match self.metadata_service.delete_chunk_meta(chunk.id).await {
                    Ok(()) => {
                        deleted += 1;
                        bytes += chunk.compressed_size_bytes as u64;
                    }
                    Err(e) => {
                        warn!(
                            chunk_id = %chunk.id,
                            error = %e,
                            "Failed to delete chunk metadata during purge"
                        );
                        failed += 1;
                    }
                },
                Err(e) => {
                    error!(
                        chunk_id = %chunk.id,
                        error = %e,
                        "Failed to delete chunk from storage during purge"
                    );
                    failed += 1;
                }
            }
        }

        info!(
            project_id = %project_id,
            deleted = deleted,
            failed = failed,
            bytes_reclaimed = bytes,
            "Manual purge completed"
        );

        Ok(RetentionResult {
            chunks_deleted: deleted,
            chunks_failed: failed,
            bytes_reclaimed: bytes,
        })
    }
}
