// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-line index of sealed chunks (ADR-047).
//!
//! The object store holds the message bytes exactly once (ADR-046). When
//! ClickHouse is configured, every sealed chunk is additionally *indexed*:
//! one row per line with its labels, timestamp, level and extracted
//! attributes plus a pointer back into the chunk (`chunk_seq`,
//! `line_index`, i.e. the [`crate::store::LogLineKey`] encoding). That index
//! is what attribute facets, histograms and `GROUP BY` analytics read.
//!
//! The writer talks to the index only through [`LineIndexSink`], invoked
//! right after the manifest commit of a seal (ADR-047 §4). Failure policy:
//! an index write may fail or be skipped, sealing never blocks on it — the
//! manifest row's `indexed_at` stays `NULL` and the reindexer drains those
//! chunks later, so the index is always rebuildable from the chunks.

pub mod analytics;
pub mod clickhouse;

use std::sync::Arc;

use async_trait::async_trait;

use crate::chunk::ChunkLabels;
use crate::error::LogAggregatorError;
use crate::types::LogLine;

/// What a sink did with a sealed chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexOutcome {
    /// Rows are durably accepted by the index; the manifest may be marked
    /// `indexed_at = now()`.
    Indexed,
    /// No index is configured (or it is disabled by the version gate). The
    /// manifest stays unmarked so a later-configured index can backfill.
    Skipped,
}

/// Everything the plugin registers for the index: the writer-side sink and
/// the read-side analytics, one object.
pub trait LineIndex: LineIndexSink + analytics::LogAnalytics {}
impl<T: LineIndexSink + analytics::LogAnalytics> LineIndex for T {}

/// Writer-side dependency on the line index — narrow so tests can fake it.
#[async_trait]
pub trait LineIndexSink: Send + Sync {
    /// Index every line of a chunk that was just committed under manifest
    /// sequence `seq`. `segments` are the sealed lines in chunk order; the
    /// chunk-wide `line_index` of a line is its position across all
    /// segments concatenated, matching what the encoder wrote.
    async fn index_chunk(
        &self,
        seq: i64,
        labels: &ChunkLabels,
        segments: &[Arc<Vec<LogLine>>],
    ) -> Result<IndexOutcome, LogAggregatorError>;

    /// Drop the rows of chunks that no longer exist logically — compacted
    /// away, purged, or found missing by reconcile — so aggregations never
    /// count a line twice or count a line the reader can no longer fetch.
    /// Best effort: a failure is logged by the caller and the rows age out
    /// by TTL at worst.
    async fn forget_chunks(&self, _seqs: &[i64]) -> Result<(), LogAggregatorError> {
        Ok(())
    }

    /// Keep the index's own expiry equal to the chunk retention window
    /// (ADR-047 §6). Called on every retention tick with the current
    /// setting; implementations must be idempotent.
    async fn set_retention_days(&self, _days: u32) -> Result<(), LogAggregatorError> {
        Ok(())
    }

    /// Human-readable reason the index is unavailable, for the capabilities
    /// endpoint (`None` when indexing is active).
    fn unavailable_reason(&self) -> Option<String> {
        None
    }
}

#[async_trait]
impl analytics::LogAnalytics for NoLineIndex {
    async fn facets(
        &self,
        _query: &crate::store::LogQuery,
        _attrs: &[analytics::AttrPredicate],
        _keys: &[analytics::GroupKey],
        _limit: u32,
    ) -> Result<std::collections::BTreeMap<String, Vec<crate::store::FacetValue>>, LogAggregatorError>
    {
        Err(self.unavailable())
    }

    async fn attribute_keys(
        &self,
        _query: &crate::store::LogQuery,
        _limit: u32,
    ) -> Result<Vec<crate::store::FacetValue>, LogAggregatorError> {
        Err(self.unavailable())
    }

    async fn histogram(
        &self,
        _query: &crate::store::LogQuery,
        _attrs: &[analytics::AttrPredicate],
        _bucket_secs: u32,
        _group_by: Option<&analytics::GroupKey>,
        _max_groups: u32,
    ) -> Result<Vec<analytics::HistogramBucket>, LogAggregatorError> {
        Err(self.unavailable())
    }

    async fn aggregate(
        &self,
        _query: &crate::store::LogQuery,
        _attrs: &[analytics::AttrPredicate],
        _group_by: &[analytics::GroupKey],
        _metric: &analytics::Metric,
        _limit: u32,
    ) -> Result<Vec<analytics::AggregateRow>, LogAggregatorError> {
        Err(self.unavailable())
    }

    async fn matching_chunks(
        &self,
        _query: &crate::store::LogQuery,
        _attrs: &[analytics::AttrPredicate],
        _limit: u32,
    ) -> Result<Vec<i64>, LogAggregatorError> {
        Err(self.unavailable())
    }

    async fn search_pointers(
        &self,
        _query: &crate::store::LogQuery,
        _attrs: &[analytics::AttrPredicate],
    ) -> Result<Vec<analytics::LinePointer>, LogAggregatorError> {
        Err(self.unavailable())
    }
}

/// The sink used when ClickHouse is not configured: never fails, never
/// indexes.
#[derive(Debug, Default, Clone)]
pub struct NoLineIndex {
    reason: String,
}

impl NoLineIndex {
    /// `reason` is what the UI shows next to "attribute analytics
    /// unavailable" — say exactly what is missing.
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }

    fn unavailable(&self) -> LogAggregatorError {
        LogAggregatorError::LineIndex {
            reason: self.reason.clone(),
        }
    }
}

#[async_trait]
impl LineIndexSink for NoLineIndex {
    async fn index_chunk(
        &self,
        _seq: i64,
        _labels: &ChunkLabels,
        _segments: &[Arc<Vec<LogLine>>],
    ) -> Result<IndexOutcome, LogAggregatorError> {
        Ok(IndexOutcome::Skipped)
    }

    fn unavailable_reason(&self) -> Option<String> {
        Some(self.reason.clone())
    }
}
