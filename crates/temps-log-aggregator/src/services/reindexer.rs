// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Reindexer (ADR-047 §6): brings the ClickHouse line index up to date
//! from the chunks themselves.
//!
//! The index is derived data. Anything with `log_chunks.indexed_at IS NULL`
//! — chunks sealed while ClickHouse was down, chunks sealed before it was
//! configured, chunks written by the compactor — is read from object
//! storage, decoded, and handed to the same [`LineIndexSink`] the writer
//! uses. Newest first, so the explorer's "index current to" watermark
//! moves immediately and old history fills in behind it.
//!
//! Idempotent by construction: `log_lines_index` is a `ReplacingMergeTree`
//! keyed on `(chunk_seq, line_index)`, so re-inserting a chunk that was in
//! fact already indexed (e.g. `mark_indexed` lost a race) collapses on
//! merge.

use std::sync::Arc;

use tracing::{debug, info, warn};

use crate::error::LogAggregatorError;
use crate::index::{IndexOutcome, LineIndexSink};
use crate::services::compactor::{decoded_to_log_line, read_all_lines};
use crate::storage::LogStorage;
use crate::store::manifest::ManifestRepo;

/// Chunks examined per [`ReindexService::run_once`] call.
pub const DEFAULT_REINDEX_BATCH: u32 = 32;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReindexReport {
    /// Chunks whose rows the index accepted and that are now marked.
    pub indexed: u64,
    /// Lines inserted.
    pub lines: u64,
    /// Chunks that could not be read or inserted (retried next run).
    pub failed: u64,
    /// True when the batch was full, i.e. more work is likely waiting.
    pub more: bool,
}

pub struct ReindexService {
    manifests: Arc<ManifestRepo>,
    storage: Arc<dyn LogStorage>,
    line_index: Arc<dyn LineIndexSink>,
}

impl ReindexService {
    pub fn new(
        manifests: Arc<ManifestRepo>,
        storage: Arc<dyn LogStorage>,
        line_index: Arc<dyn LineIndexSink>,
    ) -> Self {
        Self {
            manifests,
            storage,
            line_index,
        }
    }

    /// Index up to `batch` unindexed chunks, newest first. Returns early
    /// (with `more = false`) when the sink reports itself unavailable, so
    /// the scheduler does not spin against an unconfigured index.
    pub async fn run_once(&self, batch: u32) -> ReindexReport {
        let mut report = ReindexReport::default();
        if self.line_index.unavailable_reason().is_some() {
            return report;
        }

        let page = match self.manifests.unindexed_before_seq(i64::MAX, batch).await {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "reindex: could not list unindexed chunks");
                report.failed += 1;
                return report;
            }
        };
        report.more = page.len() as u32 >= batch;

        for m in page {
            match self.index_one(&m).await {
                Ok(lines) => {
                    report.indexed += 1;
                    report.lines += lines;
                }
                Err(e) => {
                    warn!(seq = m.seq, storage_key = %m.storage_key, error = %e, "reindex: chunk failed");
                    report.failed += 1;
                }
            }
        }

        if report.indexed > 0 {
            info!(
                indexed = report.indexed,
                lines = report.lines,
                failed = report.failed,
                more = report.more,
                "reindex pass"
            );
        } else {
            debug!(failed = report.failed, "reindex pass: nothing to do");
        }
        report
    }

    async fn index_one(
        &self,
        m: &crate::store::manifest::Manifest,
    ) -> Result<u64, LogAggregatorError> {
        let decoded = read_all_lines(self.storage.as_ref(), m).await?;
        let count = decoded.len() as u64;
        let lines: Vec<_> = decoded
            .into_iter()
            .map(|l| decoded_to_log_line(&m.labels, l))
            .collect();
        let segments = [Arc::new(lines)];
        match self
            .line_index
            .index_chunk(m.seq, &m.labels, &segments)
            .await?
        {
            IndexOutcome::Indexed => {
                self.manifests.mark_indexed(m.seq).await?;
                Ok(count)
            }
            IndexOutcome::Skipped => Ok(0),
        }
    }
}
