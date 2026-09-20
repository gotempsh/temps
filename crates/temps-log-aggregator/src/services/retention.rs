// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Retention service for cleaning up expired log data
//!
//! - Nightly job: deletes S3/filesystem chunks where ended_at < NOW() - retention_interval
//! - Never deletes anything directly: expired chunks are tombstoned and the
//!   compactor's GC removes object + row after a grace period
//! - Manual purge API for GDPR compliance

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use tracing::{debug, error, info};

use uuid::Uuid;

use crate::error::LogAggregatorError;
use crate::index::{LineIndexSink, NoLineIndex};
use crate::services::LogMetadataService;
use crate::store::manifest::ManifestRepo;
use crate::types::{ChunkMeta, RetentionConfig};

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
    manifests: Arc<ManifestRepo>,
    metadata_service: Arc<LogMetadataService>,
    /// ADR-047: rows of expired chunks are forgotten in the line index, and
    /// the index TTL is kept equal to the retention window.
    line_index: Arc<dyn LineIndexSink>,
}

impl RetentionService {
    pub fn new(manifests: Arc<ManifestRepo>, metadata_service: Arc<LogMetadataService>) -> Self {
        Self {
            manifests,
            metadata_service,
            line_index: Arc::new(NoLineIndex::default()),
        }
    }

    pub fn with_line_index(mut self, line_index: Arc<dyn LineIndexSink>) -> Self {
        self.line_index = line_index;
        self
    }

    /// Align the line index's TTL with `config` (no-op without an index;
    /// idempotent — the sink skips the ALTER when nothing changed).
    pub async fn sync_index_retention(&self, config: &RetentionConfig) {
        if let Err(e) = self
            .line_index
            .set_retention_days(config.chunk_retention_days)
            .await
        {
            error!(error = %e, days = config.chunk_retention_days, "could not sync line index TTL");
        }
    }

    /// Tombstone manifests; objects are removed by the compactor's GC once
    /// the tombstone is older than [`crate::services::compactor::GC_GRACE`],
    /// so an in-flight reader is never pulled out from under (ADR-046 §8a.3).
    async fn tombstone(&self, chunks: &[ChunkMeta]) -> (u64, u64, u64) {
        if chunks.is_empty() {
            return (0, 0, 0);
        }
        let ids: Vec<Uuid> = chunks.iter().map(|c| c.id).collect();
        let bytes: u64 = chunks.iter().map(|c| c.compressed_size_bytes as u64).sum();
        match self.manifests.mark_deleted_returning_seqs(&ids).await {
            Ok(seqs) => {
                let n = seqs.len() as u64;
                debug!(tombstoned = n, "Tombstoned expired log chunks");
                if let Err(e) = self.line_index.forget_chunks(&seqs).await {
                    error!(error = %e, chunks = seqs.len(), "line index forget after retention failed; rows age out by TTL");
                }
                (n, ids.len() as u64 - n.min(ids.len() as u64), bytes)
            }
            Err(e) => {
                error!(error = %e, count = ids.len(), "Failed to tombstone expired log chunks");
                (0, ids.len() as u64, 0)
            }
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

        let (deleted, failed, bytes) = self.tombstone(&expired_chunks).await;

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

        let (deleted, failed, bytes) = self.tombstone(&chunks).await;

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
