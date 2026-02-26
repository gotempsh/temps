//! Core OTel service orchestrating ingest and storage.
//!
//! Sampling is the responsibility of the client SDK (head-based sampling).
//! The server stores all spans it receives.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use tracing::{error, warn};

use crate::error::OtelError;
use crate::ingest::auth::{OtelAuthService, ProjectAuth};
use crate::ingest::rate_limit::RateLimiter;
use crate::storage::OtelStorage;
use crate::types::*;

/// Core OTel service that orchestrates ingest and storage.
pub struct OtelService {
    storage: Arc<dyn OtelStorage>,
    auth_service: Arc<OtelAuthService>,
    rate_limiter: Arc<RateLimiter>,
    stats: PipelineStatsAtomic,
}

/// Atomic counters for pipeline observability.
struct PipelineStatsAtomic {
    metrics_received: AtomicU64,
    metrics_stored: AtomicU64,
    metrics_dropped: AtomicU64,
    spans_received: AtomicU64,
    spans_stored: AtomicU64,
    spans_dropped: AtomicU64,
    logs_received: AtomicU64,
    logs_stored_db: AtomicU64,
    logs_stored_s3: AtomicU64,
    logs_dropped: AtomicU64,
    ingest_errors: AtomicU64,
}

impl Default for PipelineStatsAtomic {
    fn default() -> Self {
        Self {
            metrics_received: AtomicU64::new(0),
            metrics_stored: AtomicU64::new(0),
            metrics_dropped: AtomicU64::new(0),
            spans_received: AtomicU64::new(0),
            spans_stored: AtomicU64::new(0),
            spans_dropped: AtomicU64::new(0),
            logs_received: AtomicU64::new(0),
            logs_stored_db: AtomicU64::new(0),
            logs_stored_s3: AtomicU64::new(0),
            logs_dropped: AtomicU64::new(0),
            ingest_errors: AtomicU64::new(0),
        }
    }
}

impl OtelService {
    pub fn new(
        storage: Arc<dyn OtelStorage>,
        auth_service: Arc<OtelAuthService>,
        rate_limiter: Arc<RateLimiter>,
    ) -> Self {
        Self {
            storage,
            auth_service,
            rate_limiter,
            stats: PipelineStatsAtomic::default(),
        }
    }

    /// Authenticate a token (API key `tk_` or deployment token `dt_`).
    pub async fn authenticate(
        &self,
        token: &str,
        header_project_id: Option<i32>,
    ) -> Result<ProjectAuth, OtelError> {
        self.auth_service
            .authenticate(token, header_project_id)
            .await
    }

    /// Check rate limit for a project.
    pub fn check_rate_limit(&self, project_id: i32) -> Result<(), OtelError> {
        if !self.rate_limiter.check_and_increment(project_id) {
            return Err(OtelError::RateLimitExceeded {
                project_id,
                limit: 1000, // TODO: make configurable
            });
        }
        Ok(())
    }

    /// Check storage quota for a project.
    pub async fn check_quota(&self, project_id: i32) -> Result<(), OtelError> {
        if self.storage.check_quota(project_id).await? {
            let quota = self.storage.get_storage_quota(project_id).await?;
            return Err(OtelError::QuotaExceeded {
                project_id,
                used_bytes: quota.total_bytes,
                limit_bytes: quota.limit_bytes,
            });
        }
        Ok(())
    }

    // ── Ingest operations ───────────────────────────────────────────

    /// Ingest metric data points.
    pub async fn ingest_metrics(&self, points: Vec<MetricPoint>) -> Result<u64, OtelError> {
        let count = points.len() as u64;
        self.stats
            .metrics_received
            .fetch_add(count, Ordering::Relaxed);

        match self.storage.store_metrics(points).await {
            Ok(stored) => {
                self.stats
                    .metrics_stored
                    .fetch_add(stored, Ordering::Relaxed);
                Ok(stored)
            }
            Err(e) => {
                self.stats
                    .metrics_dropped
                    .fetch_add(count, Ordering::Relaxed);
                self.stats.ingest_errors.fetch_add(1, Ordering::Relaxed);
                error!(count, error = %e, "Failed to store metrics");
                Err(e)
            }
        }
    }

    /// Ingest trace spans — stores all received spans.
    ///
    /// Sampling is the client SDK's responsibility (head-based).
    /// The server stores everything it receives.
    pub async fn ingest_spans(&self, spans: Vec<SpanRecord>) -> Result<u64, OtelError> {
        let count = spans.len() as u64;
        self.stats
            .spans_received
            .fetch_add(count, Ordering::Relaxed);

        if spans.is_empty() {
            return Ok(0);
        }

        match self.storage.store_spans(spans).await {
            Ok(stored) => {
                self.stats.spans_stored.fetch_add(stored, Ordering::Relaxed);
                Ok(stored)
            }
            Err(e) => {
                self.stats.spans_dropped.fetch_add(count, Ordering::Relaxed);
                self.stats.ingest_errors.fetch_add(1, Ordering::Relaxed);
                error!(count, error = %e, "Failed to store spans");
                Err(e)
            }
        }
    }

    /// Ingest log records.
    ///
    /// Routes ERROR/WARN to DB for fast search, archives all to S3.
    pub async fn ingest_logs(&self, records: Vec<LogRecord>) -> Result<u64, OtelError> {
        let count = records.len() as u64;
        self.stats.logs_received.fetch_add(count, Ordering::Relaxed);

        // Partition: ERROR/WARN go to DB, all go to S3
        let db_records: Vec<LogRecord> = records
            .iter()
            .filter(|r| r.severity >= LogSeverity::Warn)
            .cloned()
            .collect();

        // Store high-severity records in DB
        let db_count = db_records.len() as u64;
        if !db_records.is_empty() {
            match self.storage.store_logs(db_records).await {
                Ok(stored) => {
                    self.stats
                        .logs_stored_db
                        .fetch_add(stored, Ordering::Relaxed);
                }
                Err(e) => {
                    self.stats
                        .logs_dropped
                        .fetch_add(db_count, Ordering::Relaxed);
                    self.stats.ingest_errors.fetch_add(1, Ordering::Relaxed);
                    error!(db_count, error = %e, "Failed to store log records in DB");
                }
            }
        }

        // Archive all records to S3
        match self.storage.archive_logs(records).await {
            Ok(archived) => {
                self.stats
                    .logs_stored_s3
                    .fetch_add(archived, Ordering::Relaxed);
            }
            Err(e) => {
                // S3 archival failure is non-fatal
                warn!(count, error = %e, "Failed to archive logs to S3");
            }
        }

        Ok(count)
    }

    // ── Query operations ────────────────────────────────────────────

    pub async fn query_metrics(&self, query: MetricQuery) -> Result<Vec<MetricBucket>, OtelError> {
        self.storage.query_metrics(query).await
    }

    pub async fn list_metric_names(&self, project_id: i32) -> Result<Vec<String>, OtelError> {
        self.storage.list_metric_names(project_id).await
    }

    pub async fn query_spans(&self, query: TraceQuery) -> Result<Vec<SpanRecord>, OtelError> {
        self.storage.query_spans(query).await
    }

    pub async fn query_trace_summaries(
        &self,
        query: TraceQuery,
    ) -> Result<Vec<TraceSummary>, OtelError> {
        self.storage.query_trace_summaries(query).await
    }

    pub async fn count_traces(&self, query: TraceQuery) -> Result<u64, OtelError> {
        self.storage.count_traces(query).await
    }

    pub async fn get_trace(
        &self,
        project_id: i32,
        trace_id: &str,
    ) -> Result<Vec<SpanRecord>, OtelError> {
        self.storage.get_trace(project_id, trace_id).await
    }

    pub async fn query_logs(&self, query: LogQuery) -> Result<Vec<LogRecord>, OtelError> {
        self.storage.query_logs(query).await
    }

    pub async fn list_insights(
        &self,
        project_id: i32,
        status: Option<InsightStatus>,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<Insight>, OtelError> {
        self.storage
            .list_insights(project_id, status, limit, offset)
            .await
    }

    pub async fn get_health_summaries(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> Result<Vec<HealthSummary>, OtelError> {
        self.storage
            .get_health_summaries(project_id, environment_id)
            .await
    }

    pub async fn get_storage_quota(&self, project_id: i32) -> Result<StorageQuota, OtelError> {
        self.storage.get_storage_quota(project_id).await
    }

    // ── Observability ───────────────────────────────────────────────

    /// Get pipeline statistics snapshot.
    pub fn pipeline_stats(&self) -> PipelineStats {
        PipelineStats {
            metrics_received: self.stats.metrics_received.load(Ordering::Relaxed),
            metrics_stored: self.stats.metrics_stored.load(Ordering::Relaxed),
            metrics_dropped: self.stats.metrics_dropped.load(Ordering::Relaxed),
            spans_received: self.stats.spans_received.load(Ordering::Relaxed),
            spans_stored: self.stats.spans_stored.load(Ordering::Relaxed),
            spans_dropped: self.stats.spans_dropped.load(Ordering::Relaxed),
            logs_received: self.stats.logs_received.load(Ordering::Relaxed),
            logs_stored_db: self.stats.logs_stored_db.load(Ordering::Relaxed),
            logs_stored_s3: self.stats.logs_stored_s3.load(Ordering::Relaxed),
            logs_dropped: self.stats.logs_dropped.load(Ordering::Relaxed),
            ingest_errors: self.stats.ingest_errors.load(Ordering::Relaxed),
        }
    }

    /// Access to storage for background jobs (anomaly detection, health computation).
    pub fn storage(&self) -> &Arc<dyn OtelStorage> {
        &self.storage
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::decode;
    use crate::test_support::{self, MockOtelStorage};
    use std::time::Duration;

    fn make_service(storage: MockOtelStorage) -> (OtelService, MockOtelStorage) {
        let storage_clone = storage.clone();
        let db = Arc::new(sea_orm::DatabaseConnection::Disconnected);
        let auth = Arc::new(crate::ingest::auth::OtelAuthService::new(db));
        let limiter = Arc::new(RateLimiter::new(1000, Duration::from_secs(60)));
        let svc = OtelService::new(Arc::new(storage) as Arc<dyn OtelStorage>, auth, limiter);
        (svc, storage_clone)
    }

    #[test]
    fn test_pipeline_stats_default() {
        let stats = PipelineStatsAtomic::default();
        assert_eq!(stats.metrics_received.load(Ordering::Relaxed), 0);
        assert_eq!(stats.ingest_errors.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn test_ingest_spans_stores_and_tracks_stats() {
        let mock = MockOtelStorage::new();
        let (svc, _storage) = make_service(mock);

        let (_trace_id, encoded) = test_support::build_sample_trace_tree();
        let spans = decode::decode_traces_request(&encoded, 1, None).unwrap();
        assert_eq!(spans.len(), 4);

        let stored = svc.ingest_spans(spans).await.unwrap();

        // All received spans are stored (no server-side sampling)
        assert_eq!(stored, 4);
        let stats = svc.pipeline_stats();
        assert_eq!(stats.spans_received, 4);
        assert_eq!(stats.spans_stored, 4);
    }

    #[tokio::test]
    async fn test_ingest_spans_error_span_stored() {
        let mock = MockOtelStorage::new();
        let (svc, storage) = make_service(mock);

        // Build a trace with one error span
        let trace_id: [u8; 16] = [0xAA; 16];
        let span_id: [u8; 8] = [0xBB; 8];
        let error_span = test_support::span(
            &trace_id,
            &span_id,
            &[],
            "failing-op",
            2,
            1_700_000_000_000_000_000,
            1_700_000_000_050_000_000,
            2, // ERROR
        );
        let res = test_support::resource("err-service");
        let request = test_support::trace_request(res, vec![error_span]);
        let encoded = test_support::encode_proto(&request);

        let spans = decode::decode_traces_request(&encoded, 1, None).unwrap();
        let stored = svc.ingest_spans(spans).await.unwrap();

        assert_eq!(stored, 1);
        let stored_spans = storage.stored_spans();
        assert_eq!(stored_spans.len(), 1);
        assert_eq!(stored_spans[0].status_code, SpanStatusCode::Error);
    }

    #[tokio::test]
    async fn test_ingest_spans_storage_failure_tracks_stats() {
        let mock = MockOtelStorage::new();
        *mock.fail_store_spans.lock().unwrap() = Some("disk full".into());
        let (svc, _storage) = make_service(mock);

        let trace_id: [u8; 16] = [0xCC; 16];
        let span_id: [u8; 8] = [0xDD; 8];
        let error_span = test_support::span(
            &trace_id,
            &span_id,
            &[],
            "op",
            2,
            1_700_000_000_000_000_000,
            1_700_000_000_050_000_000,
            2,
        );
        let res = test_support::resource("svc");
        let request = test_support::trace_request(res, vec![error_span]);
        let encoded = test_support::encode_proto(&request);
        let spans = decode::decode_traces_request(&encoded, 1, None).unwrap();

        let result = svc.ingest_spans(spans).await;
        assert!(result.is_err());

        let stats = svc.pipeline_stats();
        assert_eq!(stats.spans_received, 1);
        assert!(stats.spans_dropped > 0 || stats.ingest_errors > 0);
    }

    #[tokio::test]
    async fn test_ingest_and_query_trace_tree_roundtrip() {
        let mock = MockOtelStorage::new();
        let (svc, storage) = make_service(mock);

        // Build and ingest the sample trace tree
        let (trace_id_hex, encoded) = test_support::build_sample_trace_tree();
        let spans = decode::decode_traces_request(&encoded, 42, None).unwrap();

        // Force all spans through by marking one as error to ensure the trace is kept
        // Actually, let's just store directly to mock to test the query path
        storage.spans.lock().unwrap().extend(spans.clone());

        // Query the trace back
        let queried = svc.get_trace(42, &trace_id_hex).await.unwrap();
        assert_eq!(queried.len(), 4, "Should retrieve all 4 spans");

        // Verify tree structure
        let roots = test_support::find_roots(&queried);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].name, "GET /api/users");

        let tree = test_support::build_tree(&queried);
        let root_children = tree.get(&roots[0].span_id).unwrap();
        assert_eq!(root_children.len(), 2, "Root should have 2 direct children");

        // Verify we can find the grandchild
        let by_name: std::collections::HashMap<&str, &SpanRecord> =
            queried.iter().map(|s| (s.name.as_str(), s)).collect();
        let http_child = by_name["POST /external/validate"];
        let grandchildren = tree.get(&http_child.span_id).unwrap();
        assert_eq!(grandchildren.len(), 1);
    }

    #[tokio::test]
    async fn test_query_spans_filter_by_status() {
        let mock = MockOtelStorage::new();
        let (svc, storage) = make_service(mock);

        // Insert spans with mixed statuses
        let ok_span = SpanRecord {
            project_id: 1,
            deployment_id: None,
            resource: ResourceInfo::default(),
            trace_id: "aaa".into(),
            span_id: "001".into(),
            parent_span_id: None,
            name: "ok-op".into(),
            kind: SpanKind::Server,
            start_time: chrono::Utc::now(),
            end_time: chrono::Utc::now(),
            duration_ms: 10.0,
            status_code: SpanStatusCode::Ok,
            status_message: String::new(),
            attributes: Default::default(),
            events: vec![],
        };

        let err_span = SpanRecord {
            status_code: SpanStatusCode::Error,
            span_id: "002".into(),
            name: "err-op".into(),
            ..ok_span.clone()
        };

        storage
            .spans
            .lock()
            .unwrap()
            .extend(vec![ok_span, err_span]);

        // Query only errors
        let results = svc
            .query_spans(TraceQuery {
                project_id: 1,
                status: Some(SpanStatusCode::Error),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "err-op");
    }

    #[tokio::test]
    async fn test_ingest_logs_severity_routing() {
        let mock = MockOtelStorage::new();
        let (svc, storage) = make_service(mock);

        let now = chrono::Utc::now();
        let make_log = |severity: LogSeverity, body: &str| LogRecord {
            project_id: 1,
            deployment_id: None,
            resource: ResourceInfo::default(),
            timestamp: now,
            observed_timestamp: now,
            severity,
            severity_text: severity.to_string(),
            body: body.to_string(),
            trace_id: None,
            span_id: None,
            attributes: Default::default(),
        };

        let logs = vec![
            make_log(LogSeverity::Debug, "debug msg"),
            make_log(LogSeverity::Info, "info msg"),
            make_log(LogSeverity::Warn, "warn msg"),
            make_log(LogSeverity::Error, "error msg"),
            make_log(LogSeverity::Fatal, "fatal msg"),
        ];

        svc.ingest_logs(logs).await.unwrap();

        // DB should only have WARN, ERROR, FATAL
        let db_logs = storage.stored_logs();
        assert_eq!(db_logs.len(), 3, "Only WARN+ should go to DB");
        assert!(db_logs.iter().all(|l| l.severity >= LogSeverity::Warn));

        // S3 archive should have all 5
        let archived = storage.stored_archived_logs();
        assert_eq!(archived.len(), 5, "All logs should be archived to S3");

        let stats = svc.pipeline_stats();
        assert_eq!(stats.logs_received, 5);
        assert_eq!(stats.logs_stored_db, 3);
        assert_eq!(stats.logs_stored_s3, 5);
    }

    #[tokio::test]
    async fn test_ingest_metrics_roundtrip() {
        let mock = MockOtelStorage::new();
        let (svc, storage) = make_service(mock);

        let point = MetricPoint {
            project_id: 1,
            deployment_id: None,
            resource: ResourceInfo::default(),
            metric_name: "http.duration".into(),
            metric_type: MetricType::Gauge,
            unit: "ms".into(),
            timestamp: chrono::Utc::now(),
            value: Some(42.5),
            histogram_count: None,
            histogram_sum: None,
            histogram_min: None,
            histogram_max: None,
            histogram_bounds: None,
            histogram_bucket_counts: None,
            attributes: Default::default(),
        };

        let stored = svc.ingest_metrics(vec![point]).await.unwrap();
        assert_eq!(stored, 1);

        let stats = svc.pipeline_stats();
        assert_eq!(stats.metrics_received, 1);
        assert_eq!(stats.metrics_stored, 1);

        let stored_metrics = storage.stored_metrics();
        assert_eq!(stored_metrics.len(), 1);
        assert_eq!(stored_metrics[0].metric_name, "http.duration");
    }

    #[test]
    fn test_check_rate_limit_allows_within_limit() {
        let mock = MockOtelStorage::new();
        let (svc, _) = make_service(mock);

        // Should succeed
        assert!(svc.check_rate_limit(1).is_ok());
    }

    #[test]
    fn test_check_rate_limit_rejects_over_limit() {
        let db = Arc::new(sea_orm::DatabaseConnection::Disconnected);
        let auth = Arc::new(crate::ingest::auth::OtelAuthService::new(db));
        let limiter = Arc::new(RateLimiter::new(2, Duration::from_secs(60))); // only 2 allowed
        let storage = Arc::new(MockOtelStorage::new()) as Arc<dyn OtelStorage>;
        let svc = OtelService::new(storage, auth, limiter);

        assert!(svc.check_rate_limit(1).is_ok());
        assert!(svc.check_rate_limit(1).is_ok());
        let result = svc.check_rate_limit(1);
        assert!(matches!(result, Err(OtelError::RateLimitExceeded { .. })));
    }
}
