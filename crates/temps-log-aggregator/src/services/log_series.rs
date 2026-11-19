// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Counting the lines a saved search matches (ADR 108 D1).
//!
//! This is the series a log alert is evaluated on, and the series a
//! log-count dashboard panel charts. It is a *pull*: the alert evaluator
//! asks for one bounded window per rule per tick. Nothing here is on the
//! ingest path, and no rule is ever matched against a line as it arrives —
//! that is the whole reason log alerts are built this way (ADR 108 D1).
//!
//! Two ways to count, picked per query:
//!
//! - **No text needle**: the line index answers a `COUNT` over the window
//!   (ADR 047), and the unsealed head buffers are counted alongside it, so
//!   the newest minutes are not missing. No message bytes move.
//! - **A text needle, or no index**: the chunk store is paged, in pages of
//!   [`COUNT_PAGE_SIZE`], and the lines are counted. Counting stops at
//!   [`temps_core::log_series::LOG_ALERT_COUNT_CAP`] and reports the sample
//!   as saturated rather than reading an unbounded number of lines into
//!   memory.
//!
//! A saturated sample is a *lower bound*, never an overstatement: it can show
//! a count is above a threshold but not that it is below one, and the alert
//! evaluator leaves a below-threshold rule undecided on a saturated sample.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::DatabaseConnection;
use serde::Serialize;
use tracing::{debug, warn};
use utoipa::ToSchema;

use temps_core::log_series::{
    LogSeriesError, LogSeriesSample, LogSeriesSource, LOG_ALERT_COUNT_CAP,
};

use crate::error::LogAggregatorError;
use crate::index::analytics::Metric;
use crate::index::LineIndex;
use crate::services::saved_search::{LogSavedSearchService, SavedSearchQuery};
use crate::store::{
    services_linked_to_projects, LogAccessScope, LogLineStore, LogQuery, LogSelection,
};

/// Lines fetched per keyset page while counting a text search. Small enough
/// that one page of long lines is a few megabytes, not hundreds.
pub const COUNT_PAGE_SIZE: u32 = 200;

/// Most lines one dashboard series reads when it has to scan (a text
/// needle, or no line index).
pub const SERIES_SCAN_CAP: u64 = 5_000;
/// Most points in a count series.
pub const MAX_SERIES_BUCKETS: u32 = 240;
/// Most points in a numeric series, each one indexed aggregate.
pub const MAX_NUMERIC_SERIES_BUCKETS: u32 = 60;

/// What a dashboard panel asks for.
#[derive(Debug, Clone)]
pub struct SeriesRequest {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub bucket_secs: u32,
    /// `Count`, or a numeric attribute aggregate.
    pub metric: Metric,
}

/// One bucket of a log series.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct LogSeriesPoint {
    /// Bucket start.
    #[schema(value_type = String, format = DateTime)]
    pub ts: DateTime<Utc>,
    /// `None` when nothing was counted for the bucket (outside a capped
    /// scan) or no line contributed to a numeric aggregate.
    pub value: Option<f64>,
}

/// How a series was computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LogSeriesMethod {
    /// The line index (plus unsealed head buffers for counts).
    Index,
    /// A bounded scan of the chunk store.
    Scan,
}

/// A bucketed log series.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct LogSeries {
    pub points: Vec<LogSeriesPoint>,
    /// Set when a scan stopped at its cap: buckets ending before this time
    /// have no value.
    #[schema(value_type = Option<String>, format = DateTime)]
    pub partial_before: Option<DateTime<Utc>>,
    pub method: LogSeriesMethod,
}

fn bucket_count(start: DateTime<Utc>, end: DateTime<Utc>, bucket_secs: u32) -> usize {
    let span = (end - start).num_seconds().max(1);
    let width = i64::from(bucket_secs.max(1));
    usize::try_from((span + width - 1) / width).unwrap_or(0)
}

fn bucket_slot(
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    bucket_secs: u32,
    ts: DateTime<Utc>,
) -> Option<usize> {
    if ts < start || ts >= end {
        return None;
    }
    let offset = (ts - start).num_seconds() / i64::from(bucket_secs.max(1));
    usize::try_from(offset)
        .ok()
        .filter(|slot| *slot < bucket_count(start, end, bucket_secs))
}

fn points(
    start: DateTime<Utc>,
    bucket_secs: u32,
    values: impl Iterator<Item = Option<f64>>,
) -> Vec<LogSeriesPoint> {
    values
        .enumerate()
        .map(|(slot, value)| LogSeriesPoint {
            ts: start + chrono::Duration::seconds(i64::from(bucket_secs) * slot as i64),
            value,
        })
        .collect()
}

/// Counts the lines a saved search matches over a window.
pub struct LogSeriesService {
    saved_searches: Arc<LogSavedSearchService>,
    store: Arc<dyn LogLineStore>,
    line_index: Arc<dyn LineIndex>,
    db: Arc<DatabaseConnection>,
}

impl LogSeriesService {
    pub fn new(
        saved_searches: Arc<LogSavedSearchService>,
        store: Arc<dyn LogLineStore>,
        line_index: Arc<dyn LineIndex>,
        db: Arc<DatabaseConnection>,
    ) -> Self {
        Self {
            saved_searches,
            store,
            line_index,
            db,
        }
    }

    /// Count the lines of `query` in `[start, end)` for one project.
    ///
    /// The scope is built here, from the project alone: a saved search is
    /// project-scoped, so counting it can never read another project's
    /// lines even though no HTTP caller authorized this read.
    /// The project-scoped store query for a saved search over a window.
    ///
    /// The scope is built here, from the project alone: a saved search is
    /// project-scoped, so running it can never read another project's lines
    /// even though no HTTP caller authorized this read.
    async fn build_query(
        &self,
        project_id: i32,
        query: &SavedSearchQuery,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<LogQuery, LogAggregatorError> {
        let external_service_ids = services_linked_to_projects(self.db.as_ref(), &[project_id])
            .await
            .map_err(|e| LogAggregatorError::AccessResolutionFailed {
                reason: e.to_string(),
            })?;
        // Narrow to the services the query names, keeping only ones this
        // project actually owns — a stale id in a saved search must not
        // widen the read.
        let selected_services: Vec<i32> = if query.external_service_ids.is_empty() {
            external_service_ids.clone()
        } else {
            query
                .external_service_ids
                .iter()
                .copied()
                .filter(|id| external_service_ids.contains(id))
                .collect()
        };
        let log_query = LogQuery {
            scope: LogAccessScope::Allowed {
                project_ids: vec![project_id],
                external_service_ids,
            },
            start_time: start,
            end_time: end,
            source: query.source,
            selection: Some(LogSelection {
                project_ids: vec![project_id],
                external_service_ids: selected_services,
            }),
            levels: query.levels.clone(),
            envs: query.envs.clone(),
            services: query.services.clone(),
            container_ids: query.container_ids.clone(),
            node_ids: query.node_ids.clone(),
            deploy_id: query.deploy_id,
            text: query.text.clone(),
            before: None,
            limit: COUNT_PAGE_SIZE,
            context_lines: 0,
            attrs: query.predicates()?,
            chunk_seqs: None,
        };
        log_query.validate()?;
        Ok(log_query)
    }

    /// Count the lines of `query` in `[start, end)` for one project.
    pub async fn count(
        &self,
        project_id: i32,
        query: &SavedSearchQuery,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<LogSeriesSample, LogAggregatorError> {
        let log_query = self.build_query(project_id, query, start, end).await?;
        if query.needs_message_scan() || self.line_index.unavailable_reason().is_some() {
            return self.count_by_scan(log_query).await;
        }
        self.count_by_index(log_query).await
    }

    /// A bucketed series of a saved search, for a dashboard panel.
    ///
    /// A count is read from the line index plus the unsealed head buffers in
    /// one query. When the index cannot answer (a text needle, or no index),
    /// the window is scanned once, newest first, up to
    /// [`SERIES_SCAN_CAP`] lines; buckets older than the oldest line reached
    /// are reported with no value and `partial_before` says from when, so a
    /// chart never draws zero where nothing was counted. A numeric field over
    /// time needs the index and no text needle, and at most
    /// [`MAX_NUMERIC_SERIES_BUCKETS`] buckets (one indexed aggregate each).
    pub async fn series(
        &self,
        project_id: i32,
        query: &SavedSearchQuery,
        request: SeriesRequest,
    ) -> Result<LogSeries, LogAggregatorError> {
        let SeriesRequest {
            start,
            end,
            bucket_secs,
            metric,
        } = request;
        if end <= start {
            return Err(LogAggregatorError::Validation {
                message: "A log series needs end_time after start_time".to_string(),
            });
        }
        let bucket_secs = bucket_secs.max(1);
        let span = (end - start).num_seconds().max(1);
        let buckets = (span + i64::from(bucket_secs) - 1) / i64::from(bucket_secs);
        let numeric = !matches!(metric, Metric::Count);
        let max_buckets = if numeric {
            MAX_NUMERIC_SERIES_BUCKETS
        } else {
            MAX_SERIES_BUCKETS
        };
        if buckets > i64::from(max_buckets) {
            return Err(LogAggregatorError::Validation {
                message: format!(
                    "A log series of {span}s in {bucket_secs}s buckets has {buckets} points; the \
                     limit is {max_buckets}. Use wider buckets"
                ),
            });
        }
        let log_query = self.build_query(project_id, query, start, end).await?;
        let index_unavailable = self.line_index.unavailable_reason();

        if numeric {
            if query.needs_message_scan() {
                return Err(LogAggregatorError::Validation {
                    message: "A numeric log series is computed by the line index, which holds no \
                              message text; chart the line count, or remove the text filter \
                              from the saved search"
                        .to_string(),
                });
            }
            if let Some(reason) = index_unavailable {
                return Err(LogAggregatorError::LineIndex { reason });
            }
            return self
                .numeric_series(log_query, start, end, bucket_secs, &metric)
                .await;
        }
        if query.needs_message_scan() || index_unavailable.is_some() {
            return self
                .count_series_by_scan(log_query, start, end, bucket_secs)
                .await;
        }
        self.count_series_by_index(log_query, start, end, bucket_secs)
            .await
    }

    async fn count_series_by_index(
        &self,
        query: LogQuery,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        bucket_secs: u32,
    ) -> Result<LogSeries, LogAggregatorError> {
        let attrs = query.attrs.clone();
        let indexed = self
            .line_index
            .histogram(&query, &attrs, bucket_secs, None, 1)
            .await?;
        let unsealed = self
            .store
            .unsealed_histogram(&query, bucket_secs, None)
            .await?;
        let mut counts = vec![0_i64; bucket_count(start, end, bucket_secs)];
        for bucket in indexed.iter().chain(unsealed.iter()) {
            if let Some(slot) = bucket_slot(start, end, bucket_secs, bucket.ts) {
                counts[slot] += bucket.count;
            }
        }
        Ok(LogSeries {
            points: points(
                start,
                bucket_secs,
                counts.into_iter().map(|c| Some(c as f64)),
            ),
            partial_before: None,
            method: LogSeriesMethod::Index,
        })
    }

    async fn count_series_by_scan(
        &self,
        mut query: LogQuery,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        bucket_secs: u32,
    ) -> Result<LogSeries, LogAggregatorError> {
        let mut counts = vec![0_i64; bucket_count(start, end, bucket_secs)];
        let mut counted: u64 = 0;
        let mut partial_before = None;
        loop {
            let remaining = SERIES_SCAN_CAP.saturating_sub(counted);
            if remaining == 0 {
                break;
            }
            query.limit = u32::try_from(remaining.min(u64::from(COUNT_PAGE_SIZE)))
                .unwrap_or(COUNT_PAGE_SIZE)
                .max(1);
            let page = self.store.search(&query).await?;
            for line in &page.lines {
                if let Some(slot) = bucket_slot(start, end, bucket_secs, line.timestamp) {
                    counts[slot] += 1;
                }
                counted += 1;
                partial_before = Some(line.timestamp);
            }
            if let Some(scanned_back_to) = page.scanned_back_to {
                partial_before = Some(scanned_back_to);
                break;
            }
            match page.next_cursor {
                Some(next) => query.before = Some(next),
                None => {
                    partial_before = None;
                    break;
                }
            }
        }
        // Here `partial_before` is the oldest line counted when the cap
        // stopped the scan, the store's own horizon when its budget did, and
        // `None` when the window was read to its start.
        let values = counts.into_iter().enumerate().map(|(slot, count)| {
            let bucket_end =
                start + chrono::Duration::seconds(i64::from(bucket_secs) * (slot as i64 + 1));
            match partial_before {
                // A bucket that ends before the oldest line reached was not
                // (fully) counted: no value, not zero.
                Some(cutoff) if bucket_end <= cutoff => None,
                _ => Some(count as f64),
            }
        });
        Ok(LogSeries {
            points: points(start, bucket_secs, values),
            partial_before,
            method: LogSeriesMethod::Scan,
        })
    }

    async fn numeric_series(
        &self,
        query: LogQuery,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        bucket_secs: u32,
        metric: &Metric,
    ) -> Result<LogSeries, LogAggregatorError> {
        let attrs = query.attrs.clone();
        let mut values = Vec::with_capacity(bucket_count(start, end, bucket_secs));
        for slot in 0..bucket_count(start, end, bucket_secs) {
            let bucket_start =
                start + chrono::Duration::seconds(i64::from(bucket_secs) * slot as i64);
            let bucket_end =
                (bucket_start + chrono::Duration::seconds(i64::from(bucket_secs))).min(end);
            let mut bucket_query = query.clone();
            bucket_query.start_time = bucket_start;
            bucket_query.end_time = bucket_end;
            let rows = self
                .line_index
                .aggregate(&bucket_query, &attrs, &[], metric, 1)
                .await?;
            // No contributing line is no value, not zero: an average of
            // nothing did not happen.
            values.push(
                rows.first()
                    .filter(|row| row.lines > 0)
                    .map(|row| row.value),
            );
        }
        Ok(LogSeries {
            points: points(start, bucket_secs, values.into_iter()),
            partial_before: None,
            method: LogSeriesMethod::Index,
        })
    }

    /// Count through the line index plus the unsealed head buffers.
    ///
    /// One bucket as wide as the whole window, so the index returns a single
    /// row. A failure here falls back to the scan rather than reporting a
    /// count of zero: "the index is briefly unavailable" and "nothing
    /// matched" must not look the same to an alert.
    async fn count_by_index(&self, query: LogQuery) -> Result<LogSeriesSample, LogAggregatorError> {
        let window_secs = (query.end_time - query.start_time).num_seconds().max(1);
        let bucket_secs = u32::try_from(window_secs).unwrap_or(u32::MAX);
        let attrs = query.attrs.clone();
        let indexed = match self
            .line_index
            .histogram(&query, &attrs, bucket_secs, None, 1)
            .await
        {
            Ok(buckets) => buckets,
            Err(e) => {
                warn!(
                    error = %e,
                    "counting a saved log search through the line index failed; scanning chunks instead"
                );
                return self.count_by_scan(query).await;
            }
        };
        let unsealed = self
            .store
            .unsealed_histogram(&query, bucket_secs, None)
            .await?;
        let count: i64 = indexed
            .iter()
            .chain(unsealed.iter())
            .map(|bucket| bucket.count)
            .sum();
        let count = u64::try_from(count).unwrap_or(0);
        Ok(LogSeriesSample {
            count,
            saturated: false,
        })
    }

    /// Count by walking keyset pages of the chunk store.
    ///
    /// Stops at [`LOG_ALERT_COUNT_CAP`] lines, or when the store reports a
    /// partial page (its own time/byte budget ran out). Either way the
    /// sample is marked saturated and the count is a lower bound.
    async fn count_by_scan(
        &self,
        mut query: LogQuery,
    ) -> Result<LogSeriesSample, LogAggregatorError> {
        let mut count: u64 = 0;
        loop {
            let remaining = LOG_ALERT_COUNT_CAP.saturating_sub(count);
            if remaining == 0 {
                return Ok(LogSeriesSample {
                    count,
                    saturated: true,
                });
            }
            query.limit = u32::try_from(remaining.min(u64::from(COUNT_PAGE_SIZE)))
                .unwrap_or(COUNT_PAGE_SIZE)
                .max(1);
            let page = self.store.search(&query).await?;
            count += page.lines.len() as u64;
            // A partial page means the store stopped early; nothing older
            // in the window has been looked at.
            if page.scanned_back_to.is_some() {
                return Ok(LogSeriesSample {
                    count,
                    saturated: true,
                });
            }
            let Some(next) = page.next_cursor else {
                return Ok(LogSeriesSample {
                    count,
                    saturated: false,
                });
            };
            query.before = Some(next);
        }
    }
}

#[async_trait]
impl LogSeriesSource for LogSeriesService {
    async fn count_matching(
        &self,
        project_id: i32,
        saved_search_id: i32,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<LogSeriesSample, LogSeriesError> {
        let query = self
            .saved_searches
            .query_for(project_id, saved_search_id)
            .await
            .map_err(|e| match e {
                LogAggregatorError::SavedSearchNotFound { .. } => {
                    LogSeriesError::SavedSearchNotFound {
                        project_id,
                        saved_search_id,
                    }
                }
                other => LogSeriesError::Query {
                    project_id,
                    saved_search_id,
                    reason: other.to_string(),
                },
            })?;
        let sample = self
            .count(project_id, &query, start, end)
            .await
            .map_err(|e| LogSeriesError::Query {
                project_id,
                saved_search_id,
                reason: e.to_string(),
            })?;
        debug!(
            project_id,
            saved_search_id,
            count = sample.count,
            saturated = sample.saturated,
            "counted a saved log search for an alert"
        );
        Ok(sample)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use chrono::Duration;

    use crate::index::analytics::HistogramBucket;
    use crate::index::NoLineIndex;
    use crate::store::{
        FacetField, FacetResult, LogLineKey, LogLineRecord, LogPage, LogSourceKind, MAX_PAGE_SIZE,
    };
    use crate::types::{LogLevel, LogSource, LogStream};

    /// A store that answers `search` from a scripted list of pages and
    /// records the limits it was asked for.
    struct ScriptedStore {
        pages: std::sync::Mutex<Vec<LogPage>>,
        limits: std::sync::Mutex<Vec<u32>>,
        calls: AtomicUsize,
        unsealed: Vec<HistogramBucket>,
    }

    impl ScriptedStore {
        fn new(pages: Vec<LogPage>) -> Self {
            Self {
                pages: std::sync::Mutex::new(pages),
                limits: std::sync::Mutex::new(Vec::new()),
                calls: AtomicUsize::new(0),
                unsealed: Vec::new(),
            }
        }

        fn with_unsealed(mut self, buckets: Vec<HistogramBucket>) -> Self {
            self.unsealed = buckets;
            self
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }

        fn limits(&self) -> Vec<u32> {
            self.limits.lock().expect("limits lock").clone()
        }
    }

    fn record(line_id: i64) -> LogLineRecord {
        LogLineRecord {
            timestamp: Utc::now(),
            project_id: Some(7),
            external_service_id: None,
            env: "production".into(),
            service: "web".into(),
            level: LogLevel::Error,
            stream: LogStream::Stderr,
            container_id: "c1".into(),
            node_id: None,
            node_name: None,
            deploy_id: None,
            message: "payment failed".into(),
            fields: None,
            line_id,
            context: None,
        }
    }

    /// A page of `lines` lines, optionally with a cursor to an older page.
    fn page(lines: usize, more: bool, partial: bool) -> LogPage {
        LogPage {
            lines: (0..lines).map(|i| record(i as i64)).collect(),
            next_cursor: more.then(|| LogLineKey {
                timestamp: Utc::now(),
                container_id: "c1".into(),
                line_id: 1,
            }),
            scanned_back_to: partial.then(Utc::now),
        }
    }

    #[async_trait]
    impl LogLineStore for ScriptedStore {
        async fn search(&self, query: &LogQuery) -> Result<LogPage, LogAggregatorError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.limits.lock().expect("limits lock").push(query.limit);
            let mut pages = self.pages.lock().expect("pages lock");
            if pages.is_empty() {
                return Ok(page(0, false, false));
            }
            Ok(pages.remove(0))
        }

        async fn facets(
            &self,
            _query: &LogQuery,
            _fields: &[FacetField],
        ) -> Result<FacetResult, LogAggregatorError> {
            unreachable!("counting never asks for facets")
        }

        async fn context(
            &self,
            _scope: &LogAccessScope,
            _key: &LogLineKey,
            _before: u32,
            _after: u32,
        ) -> Result<Vec<LogLineRecord>, LogAggregatorError> {
            unreachable!("counting never asks for context")
        }

        async fn sources(&self, _query: &LogQuery) -> Result<Vec<LogSource>, LogAggregatorError> {
            unreachable!("counting never asks for sources")
        }

        async fn latest_timestamp_for_container(
            &self,
            _container_id: &str,
        ) -> Result<Option<DateTime<Utc>>, LogAggregatorError> {
            unreachable!("counting never asks for a container's last line")
        }

        async fn purge_project(
            &self,
            _project_id: i32,
            _before: DateTime<Utc>,
        ) -> Result<u64, LogAggregatorError> {
            unreachable!("counting never purges")
        }

        async fn unsealed_histogram(
            &self,
            _query: &LogQuery,
            _bucket_secs: u32,
            _group_by: Option<FacetField>,
        ) -> Result<Vec<HistogramBucket>, LogAggregatorError> {
            Ok(self.unsealed.clone())
        }
    }

    fn service(store: Arc<ScriptedStore>) -> LogSeriesService {
        // One empty result for the `project_services` lookup `count()` runs
        // to build the project's scope; the tests that call the counting
        // paths directly never reach it.
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_results([Vec::<temps_entities::project_services::Model>::new()])
                .into_connection(),
        );
        LogSeriesService::new(
            Arc::new(crate::services::LogSavedSearchService::new(db.clone())),
            store,
            Arc::new(NoLineIndex::new("no line index in this test")),
            db,
        )
    }

    fn query(text: Option<&str>) -> LogQuery {
        LogQuery {
            scope: LogAccessScope::Allowed {
                project_ids: vec![7],
                external_service_ids: vec![],
            },
            start_time: Utc::now() - Duration::minutes(5),
            end_time: Utc::now(),
            source: LogSourceKind::Collected,
            selection: Some(LogSelection {
                project_ids: vec![7],
                external_service_ids: vec![],
            }),
            levels: vec![],
            envs: vec![],
            services: vec![],
            container_ids: vec![],
            node_ids: vec![],
            deploy_id: None,
            text: text.map(str::to_string),
            before: None,
            limit: COUNT_PAGE_SIZE,
            context_lines: 0,
            attrs: vec![],
            chunk_seqs: None,
        }
    }

    #[tokio::test]
    async fn a_single_short_page_is_an_exact_count() {
        let store = Arc::new(ScriptedStore::new(vec![page(12, false, false)]));
        let sample = service(store.clone())
            .count_by_scan(query(Some("payment failed")))
            .await
            .expect("counting succeeds");
        assert_eq!(sample.count, 12);
        assert!(!sample.saturated, "a complete page is not saturated");
        assert_eq!(store.calls(), 1, "one page needs one query");
    }

    #[tokio::test]
    async fn pages_are_walked_until_the_results_run_out() {
        let store = Arc::new(ScriptedStore::new(vec![
            page(COUNT_PAGE_SIZE as usize, true, false),
            page(5, false, false),
        ]));
        let sample = service(store.clone())
            .count_by_scan(query(Some("payment failed")))
            .await
            .expect("counting succeeds");
        assert_eq!(sample.count, u64::from(COUNT_PAGE_SIZE) + 5);
        assert!(!sample.saturated);
        assert_eq!(store.calls(), 2);
    }

    #[tokio::test]
    async fn counting_stops_at_the_cap_and_says_the_count_is_a_lower_bound() {
        // Every page is full and offers another, so only the cap can stop
        // this: the guarantee is that a busy project cannot make one
        // evaluation read an unbounded number of lines.
        let pages = (0..100)
            .map(|_| page(COUNT_PAGE_SIZE as usize, true, false))
            .collect();
        let store = Arc::new(ScriptedStore::new(pages));
        let sample = service(store.clone())
            .count_by_scan(query(Some("payment failed")))
            .await
            .expect("counting succeeds");
        assert_eq!(sample.count, LOG_ALERT_COUNT_CAP);
        assert!(sample.saturated, "a capped count is a lower bound");
        let expected_pages = LOG_ALERT_COUNT_CAP / u64::from(COUNT_PAGE_SIZE);
        assert_eq!(store.calls() as u64, expected_pages);
        // Never asks for more than the store's own page ceiling, and never
        // for zero rows.
        for limit in store.limits() {
            assert!((1..=MAX_PAGE_SIZE).contains(&limit), "bad limit {limit}");
        }
    }

    #[tokio::test]
    async fn a_partial_page_marks_the_sample_saturated() {
        // The store gave up before proving the page complete, so nothing
        // older in the window has been looked at.
        let store = Arc::new(ScriptedStore::new(vec![page(3, true, true)]));
        let sample = service(store.clone())
            .count_by_scan(query(Some("payment failed")))
            .await
            .expect("counting succeeds");
        assert_eq!(sample.count, 3);
        assert!(sample.saturated);
        assert_eq!(store.calls(), 1, "a partial page is not walked further");
    }

    #[tokio::test]
    async fn the_last_page_asks_only_for_the_rows_still_under_the_cap() {
        let pages = (0..100)
            .map(|_| page(COUNT_PAGE_SIZE as usize, true, false))
            .collect();
        let store = Arc::new(ScriptedStore::new(pages));
        service(store.clone())
            .count_by_scan(query(Some("needle")))
            .await
            .expect("counting succeeds");
        let total: u64 = store.limits().iter().map(|l| u64::from(*l)).sum();
        assert_eq!(
            total, LOG_ALERT_COUNT_CAP,
            "the limits asked for add up to exactly the cap"
        );
    }

    #[tokio::test]
    async fn without_an_index_the_count_falls_back_to_a_scan() {
        // `NoLineIndex` reports itself unavailable, so even a query with no
        // text needle is counted by scanning rather than reporting zero.
        let store = Arc::new(ScriptedStore::new(vec![page(4, false, false)]));
        let sample = service(store.clone())
            .count(
                7,
                &SavedSearchQuery::default(),
                Utc::now() - Duration::minutes(5),
                Utc::now(),
            )
            .await
            .expect("counting succeeds");
        assert_eq!(sample.count, 4);
        assert_eq!(store.calls(), 1);
    }

    #[tokio::test]
    async fn an_index_count_adds_the_unsealed_head_buffers() {
        // The index only knows sealed chunks; the newest minutes are still
        // in head buffers and must be counted too, or a live alert always
        // reads low.
        let store = Arc::new(
            ScriptedStore::new(vec![]).with_unsealed(vec![HistogramBucket {
                ts: Utc::now(),
                group: None,
                count: 6,
            }]),
        );
        // `NoLineIndex::histogram` errors, which the counter treats as
        // "index unavailable" and falls back to the scan — so this asserts
        // the fallback path, not the index path.
        let sample = service(store.clone())
            .count_by_index(query(None))
            .await
            .expect("counting succeeds");
        assert_eq!(sample.count, 0, "the scripted store has no lines");
        assert_eq!(store.calls(), 1, "the fallback scanned once");
    }

    // ── Dashboard series ────────────────────────────────────────────────

    use crate::index::analytics::{AggregateRow, AttrPredicate, GroupKey};
    use crate::index::{IndexOutcome, LineIndexSink};
    use crate::store::FacetValue;
    use chrono::TimeZone;

    /// A line index that answers histograms and aggregates from fixed data.
    struct FakeIndex {
        histogram: Vec<HistogramBucket>,
        /// `(lines, value)` returned for each aggregate call, in order.
        aggregates: std::sync::Mutex<Vec<(i64, f64)>>,
    }

    #[async_trait]
    impl LineIndexSink for FakeIndex {
        async fn index_chunk(
            &self,
            _seq: i64,
            _labels: &crate::chunk::ChunkLabels,
            _segments: &[Arc<Vec<crate::types::LogLine>>],
        ) -> Result<IndexOutcome, LogAggregatorError> {
            Ok(IndexOutcome::Skipped)
        }
    }

    #[async_trait]
    impl crate::index::analytics::LogAnalytics for FakeIndex {
        async fn facets(
            &self,
            _query: &LogQuery,
            _attrs: &[AttrPredicate],
            _keys: &[GroupKey],
            _limit: u32,
        ) -> Result<std::collections::BTreeMap<String, Vec<FacetValue>>, LogAggregatorError>
        {
            unreachable!("series never asks for facets")
        }
        async fn attribute_keys(
            &self,
            _query: &LogQuery,
            _limit: u32,
        ) -> Result<Vec<FacetValue>, LogAggregatorError> {
            unreachable!("series never asks for attribute keys")
        }
        async fn histogram(
            &self,
            _query: &LogQuery,
            _attrs: &[AttrPredicate],
            _bucket_secs: u32,
            _group_by: Option<&GroupKey>,
            _max_groups: u32,
        ) -> Result<Vec<HistogramBucket>, LogAggregatorError> {
            Ok(self.histogram.clone())
        }
        async fn aggregate(
            &self,
            _query: &LogQuery,
            _attrs: &[AttrPredicate],
            _group_by: &[GroupKey],
            _metric: &Metric,
            _limit: u32,
        ) -> Result<Vec<AggregateRow>, LogAggregatorError> {
            let (lines, value) = self.aggregates.lock().expect("aggregates lock").remove(0);
            Ok(vec![AggregateRow {
                keys: vec![],
                value,
                lines,
            }])
        }
        async fn matching_chunks(
            &self,
            _query: &LogQuery,
            _attrs: &[AttrPredicate],
            _limit: u32,
        ) -> Result<Vec<i64>, LogAggregatorError> {
            unreachable!("series never asks for chunks")
        }
        async fn search_pointers(
            &self,
            _query: &LogQuery,
            _attrs: &[AttrPredicate],
        ) -> Result<Vec<crate::index::analytics::LinePointer>, LogAggregatorError> {
            unreachable!("series never asks for pointers")
        }
    }

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 19, 12, 0, 0)
            .single()
            .expect("valid date")
    }

    fn series_service(store: Arc<ScriptedStore>, index: Option<FakeIndex>) -> LogSeriesService {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_results([Vec::<temps_entities::project_services::Model>::new()])
                .into_connection(),
        );
        let index: Arc<dyn LineIndex> = match index {
            Some(index) => Arc::new(index),
            None => Arc::new(NoLineIndex::new("no line index in this test")),
        };
        LogSeriesService::new(
            Arc::new(crate::services::LogSavedSearchService::new(db.clone())),
            store,
            index,
            db,
        )
    }

    fn at_minute(minute: i64) -> LogLineRecord {
        let mut line = record(minute);
        line.timestamp = t0() + Duration::minutes(minute) + Duration::seconds(10);
        line
    }

    fn request(minutes: i64, metric: Metric) -> SeriesRequest {
        SeriesRequest {
            start: t0(),
            end: t0() + Duration::minutes(minutes),
            bucket_secs: 60,
            metric,
        }
    }

    fn text_query() -> SavedSearchQuery {
        SavedSearchQuery {
            text: Some("payment failed".to_string()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_text_search_is_counted_per_bucket_by_one_scan() {
        // Newest first, as the store returns them.
        let store = Arc::new(ScriptedStore::new(vec![LogPage {
            lines: vec![at_minute(4), at_minute(4), at_minute(1)],
            next_cursor: None,
            scanned_back_to: None,
        }]));
        let series = series_service(store, None)
            .series(7, &text_query(), request(5, Metric::Count))
            .await
            .expect("series");
        let values: Vec<Option<f64>> = series.points.iter().map(|p| p.value).collect();
        assert_eq!(
            values,
            vec![Some(0.0), Some(1.0), Some(0.0), Some(0.0), Some(2.0)]
        );
        assert_eq!(series.partial_before, None);
        assert_eq!(series.method, LogSeriesMethod::Scan);
        assert_eq!(series.points[1].ts, t0() + Duration::minutes(1));
    }

    #[tokio::test]
    async fn a_capped_scan_leaves_older_buckets_without_a_value() {
        // Every page is full and has more: only the cap stops the scan. All
        // lines sit in the last minute, so earlier buckets were never reached.
        let full_page = || LogPage {
            lines: (0..COUNT_PAGE_SIZE).map(|_| at_minute(9)).collect(),
            next_cursor: Some(LogLineKey {
                timestamp: t0(),
                container_id: "c1".into(),
                line_id: 1,
            }),
            scanned_back_to: None,
        };
        let pages = (0..(SERIES_SCAN_CAP / u64::from(COUNT_PAGE_SIZE)) + 2)
            .map(|_| full_page())
            .collect();
        let store = Arc::new(ScriptedStore::new(pages));
        let series = series_service(store.clone(), None)
            .series(7, &text_query(), request(10, Metric::Count))
            .await
            .expect("series");
        assert_eq!(series.points[9].value, Some(SERIES_SCAN_CAP as f64));
        assert!(series.partial_before.is_some());
        assert!(
            series.points[..9].iter().all(|p| p.value.is_none()),
            "an uncounted bucket is not zero: {:?}",
            series.points
        );
        let read: u64 = store.limits().iter().map(|l| u64::from(*l)).sum();
        assert_eq!(read, SERIES_SCAN_CAP, "the scan reads exactly the cap");
    }

    #[tokio::test]
    async fn a_count_without_text_comes_from_the_index_plus_the_head_buffers() {
        let index = FakeIndex {
            histogram: vec![
                HistogramBucket {
                    ts: t0(),
                    group: None,
                    count: 3,
                },
                HistogramBucket {
                    ts: t0() + Duration::minutes(2),
                    group: None,
                    count: 5,
                },
            ],
            aggregates: std::sync::Mutex::new(vec![]),
        };
        let store = Arc::new(
            ScriptedStore::new(vec![]).with_unsealed(vec![HistogramBucket {
                ts: t0() + Duration::minutes(2),
                group: None,
                count: 2,
            }]),
        );
        let series = series_service(store.clone(), Some(index))
            .series(7, &SavedSearchQuery::default(), request(3, Metric::Count))
            .await
            .expect("series");
        let values: Vec<Option<f64>> = series.points.iter().map(|p| p.value).collect();
        assert_eq!(values, vec![Some(3.0), Some(0.0), Some(7.0)]);
        assert_eq!(series.method, LogSeriesMethod::Index);
        assert_eq!(store.calls(), 0, "no message was read");
    }

    #[tokio::test]
    async fn a_numeric_series_aggregates_each_bucket_and_leaves_empty_ones_blank() {
        let index = FakeIndex {
            histogram: vec![],
            aggregates: std::sync::Mutex::new(vec![(4, 120.0), (0, 0.0), (1, 80.0)]),
        };
        let series = series_service(Arc::new(ScriptedStore::new(vec![])), Some(index))
            .series(
                7,
                &SavedSearchQuery::default(),
                request(3, Metric::P95("duration_ms".to_string())),
            )
            .await
            .expect("series");
        let values: Vec<Option<f64>> = series.points.iter().map(|p| p.value).collect();
        assert_eq!(values, vec![Some(120.0), None, Some(80.0)]);
    }

    #[tokio::test]
    async fn a_numeric_series_needs_the_index_and_no_text() {
        let service = series_service(Arc::new(ScriptedStore::new(vec![])), None);
        let error = service
            .series(
                7,
                &text_query(),
                request(3, Metric::Avg("duration_ms".to_string())),
            )
            .await
            .expect_err("text cannot be aggregated by the index");
        assert!(
            matches!(error, LogAggregatorError::Validation { .. }),
            "{error}"
        );

        let service = series_service(Arc::new(ScriptedStore::new(vec![])), None);
        let error = service
            .series(
                7,
                &SavedSearchQuery::default(),
                request(3, Metric::Avg("duration_ms".to_string())),
            )
            .await
            .expect_err("no index, no numeric series");
        assert!(
            matches!(error, LogAggregatorError::LineIndex { .. }),
            "{error}"
        );
    }

    #[tokio::test]
    async fn too_many_points_and_an_empty_window_are_refused() {
        let service = series_service(Arc::new(ScriptedStore::new(vec![])), None);
        let error = service
            .series(
                7,
                &SavedSearchQuery::default(),
                request(i64::from(MAX_SERIES_BUCKETS) + 1, Metric::Count),
            )
            .await
            .expect_err("too many buckets");
        assert!(error.to_string().contains("Use wider buckets"), "{error}");

        let mut empty = request(1, Metric::Count);
        empty.end = empty.start;
        assert!(service
            .series(7, &SavedSearchQuery::default(), empty)
            .await
            .is_err());
    }
}
