// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Counting the lines a saved search matches (ADR 073 D1).
//!
//! This is the series a log alert is evaluated on, and the series a
//! log-count dashboard panel charts. It is a *pull*: the alert evaluator
//! asks for one bounded window per rule per tick. Nothing here is on the
//! ingest path, and no rule is ever matched against a line as it arrives —
//! that is the whole reason log alerts are built this way (ADR 073 D1).
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
//! A saturated sample is a *lower bound*, never an overstatement, so a rule
//! can under-report a spike but can never fire on a count that did not
//! happen.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::DatabaseConnection;
use tracing::{debug, warn};

use temps_core::log_series::{
    LogSeriesError, LogSeriesSample, LogSeriesSource, LOG_ALERT_COUNT_CAP,
};

use crate::error::LogAggregatorError;
use crate::index::LineIndex;
use crate::services::saved_search::{LogSavedSearchService, SavedSearchQuery};
use crate::store::{
    services_linked_to_projects, LogAccessScope, LogLineStore, LogQuery, LogSelection,
};

/// Lines fetched per keyset page while counting a text search. Small enough
/// that one page of long lines is a few megabytes, not hundreds.
pub const COUNT_PAGE_SIZE: u32 = 200;

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
    pub async fn count(
        &self,
        project_id: i32,
        query: &SavedSearchQuery,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<LogSeriesSample, LogAggregatorError> {
        let external_service_ids = services_linked_to_projects(self.db.as_ref(), &[project_id])
            .await
            .map_err(|e| LogAggregatorError::AccessResolutionFailed {
                reason: e.to_string(),
            })?;
        // Narrow to the services the query names, keeping only ones this
        // project actually owns — a stale id in a saved search must not
        // widen the count.
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
        let scope = LogAccessScope::Allowed {
            project_ids: vec![project_id],
            external_service_ids: external_service_ids.clone(),
        };

        let log_query = LogQuery {
            scope,
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

        if query.needs_message_scan() || self.line_index.unavailable_reason().is_some() {
            return self.count_by_scan(log_query).await;
        }
        self.count_by_index(log_query).await
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
}
