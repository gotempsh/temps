//! Core OTel service orchestrating ingest and storage.
//!
//! Sampling is the responsibility of the client SDK (head-based sampling).
//! The server stores all spans it receives.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;
use tracing::{error, warn};

use crate::error::OtelError;
use crate::ingest::auth::{IngestAuth, OtelAuthService, ProjectAuth};
use crate::ingest::rate_limit::RateLimiter;
use crate::storage::OtelStorage;
use crate::types::*;

/// Core OTel service that orchestrates ingest and storage.
pub struct OtelService {
    storage: Arc<dyn OtelStorage>,
    auth_service: Arc<OtelAuthService>,
    rate_limiter: Arc<RateLimiter>,
    /// Rate limiter for service-scoped (`si_`) OTLP ingest, keyed by
    /// `service_id`. Kept separate from the project limiter so the two token
    /// classes don't share counters. Sized generously (see [`SERVICE_INGEST_*`]
    /// constants): `si_` traffic is machine-to-machine and periodic (a RustFS
    /// container pushes every ~10–30s), so the ceiling exists only to cap a
    /// runaway/compromised exporter, not to shape normal traffic.
    service_rate_limiter: Arc<RateLimiter>,
    stats: PipelineStatsAtomic,
}

/// Max `si_` ingest requests per service per window. ~10 req/s — far above a
/// healthy exporter's cadence, low enough to stop a tight-loop flood from
/// filling TimescaleDB chunks / saturating the write channel.
const SERVICE_INGEST_MAX_REQUESTS: u32 = 600;
/// Sliding window for the service ingest limiter.
const SERVICE_INGEST_WINDOW: Duration = Duration::from_secs(60);

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
            service_rate_limiter: Arc::new(RateLimiter::new(
                SERVICE_INGEST_MAX_REQUESTS,
                SERVICE_INGEST_WINDOW,
            )),
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

    /// Authenticate any ingest token — project (`tk_`/`dt_`) or service (`si_`).
    ///
    /// Routes `si_` tokens to the service auth path; all others fall through
    /// to the existing project auth path.
    pub async fn authenticate_any(
        &self,
        token: &str,
        header_project_id: Option<i32>,
    ) -> Result<IngestAuth, OtelError> {
        self.auth_service
            .authenticate_any(token, header_project_id)
            .await
    }

    /// Check rate limit for a project.
    pub fn check_rate_limit(&self, project_id: i32) -> Result<(), OtelError> {
        if !self.rate_limiter.check_and_increment(project_id) {
            // Report the limiter's actual configured limit (set via
            // `TEMPS_OTEL_RATE_LIMIT`) so the error matches reality.
            return Err(OtelError::RateLimitExceeded {
                project_id,
                limit: self.rate_limiter.max_requests(),
            });
        }
        Ok(())
    }

    /// Check the rate limit for a service-scoped (`si_`) ingest token, keyed by
    /// `service_id`. Used by the OTLP service-metrics path, which is otherwise
    /// unthrottled. Generous ceiling — see [`SERVICE_INGEST_MAX_REQUESTS`].
    pub fn check_service_rate_limit(&self, service_id: i32) -> Result<(), OtelError> {
        if !self.service_rate_limiter.check_and_increment(service_id) {
            return Err(OtelError::ServiceRateLimitExceeded {
                service_id,
                limit: self.service_rate_limiter.max_requests(),
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
        // Validate every label key used for filtering or grouping against the
        // same allowlist the ingest path enforces on metric/attribute names.
        // Keys flow into the store's SQL (as bound map indices); rejecting bad
        // keys here is a defence-in-depth trust boundary on top of the store's
        // own check. Values are always bound, never validated for content.
        for key in query
            .group_by
            .iter()
            .chain(query.label_filters.iter().map(|(k, _)| k))
        {
            if temps_metrics::validate_metric_name(key).is_err() {
                return Err(OtelError::Validation {
                    message: format!(
                        "metric query label key '{key}' contains characters outside the allowed set [a-zA-Z0-9_.:-]"
                    ),
                });
            }
        }
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

    pub async fn query_genai_trace_summaries(
        &self,
        query: TraceQuery,
    ) -> Result<Vec<GenAiTraceSummary>, OtelError> {
        self.storage.query_genai_trace_summaries(query).await
    }

    pub async fn get_genai_trace_spans(
        &self,
        project_id: i32,
        trace_id: &str,
    ) -> Result<Vec<GenAiSpanDetail>, OtelError> {
        self.storage
            .get_genai_trace_spans(project_id, trace_id)
            .await
    }

    pub async fn count_genai_traces(&self, query: TraceQuery) -> Result<u64, OtelError> {
        self.storage.count_genai_traces(query).await
    }

    pub async fn get_genai_trace_events(
        &self,
        project_id: i32,
        trace_id: &str,
    ) -> Result<Vec<GenAiEvent>, OtelError> {
        self.storage
            .get_genai_trace_events(project_id, trace_id)
            .await
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

        let mut point = MetricPoint::skeleton(
            1,
            None,
            ResourceInfo::default(),
            "http.duration".into(),
            MetricType::Gauge,
            "ms".into(),
            chrono::Utc::now(),
            Default::default(),
        );
        point.value = Some(42.5);

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
        // The error must report the limiter's actual configured limit (2),
        // not a hardcoded value.
        assert!(matches!(
            result,
            Err(OtelError::RateLimitExceeded { limit: 2, .. })
        ));
    }

    // ── service (`si_`) ingest rate limit (SECURITY metrics-security-2) ────────

    #[test]
    fn test_check_service_rate_limit_allows_within_limit() {
        let (svc, _) = make_service(MockOtelStorage::new());
        // Well within the generous SERVICE_INGEST_MAX_REQUESTS ceiling.
        for _ in 0..50 {
            assert!(svc.check_service_rate_limit(7).is_ok());
        }
    }

    #[test]
    fn test_check_service_rate_limit_rejects_over_limit() {
        let (svc, _) = make_service(MockOtelStorage::new());
        // Exhaust the per-service window, then expect a service-scoped 429.
        for _ in 0..SERVICE_INGEST_MAX_REQUESTS {
            assert!(svc.check_service_rate_limit(7).is_ok());
        }
        let result = svc.check_service_rate_limit(7);
        assert!(
            matches!(
                result,
                Err(OtelError::ServiceRateLimitExceeded {
                    service_id: 7,
                    limit
                }) if limit == SERVICE_INGEST_MAX_REQUESTS
            ),
            "expected ServiceRateLimitExceeded for service 7, got {result:?}"
        );
    }

    #[test]
    fn test_service_rate_limit_is_isolated_per_service() {
        let (svc, _) = make_service(MockOtelStorage::new());
        // Exhaust service 7's window.
        for _ in 0..SERVICE_INGEST_MAX_REQUESTS {
            assert!(svc.check_service_rate_limit(7).is_ok());
        }
        assert!(svc.check_service_rate_limit(7).is_err());
        // A different service must be unaffected.
        assert!(svc.check_service_rate_limit(8).is_ok());
    }

    #[test]
    fn test_service_rate_limit_does_not_consume_project_limit() {
        // The service limiter and the project limiter are separate instances:
        // hammering one must not reject the other.
        let db = Arc::new(sea_orm::DatabaseConnection::Disconnected);
        let auth = Arc::new(crate::ingest::auth::OtelAuthService::new(db));
        let project_limiter = Arc::new(RateLimiter::new(2, Duration::from_secs(60)));
        let storage = Arc::new(MockOtelStorage::new()) as Arc<dyn OtelStorage>;
        let svc = OtelService::new(storage, auth, project_limiter);

        // Spend many service-ingest tokens for service 1...
        for _ in 0..10 {
            assert!(svc.check_service_rate_limit(1).is_ok());
        }
        // ...the project limiter (limit 2) for project 1 is untouched.
        assert!(svc.check_rate_limit(1).is_ok());
        assert!(svc.check_rate_limit(1).is_ok());
        assert!(svc.check_rate_limit(1).is_err());
    }

    #[tokio::test]
    async fn test_query_genai_traces_filters_by_gen_ai_system() {
        let mock = MockOtelStorage::new();
        let (svc, storage) = make_service(mock);

        let now = chrono::Utc::now();
        let mut genai_attrs = std::collections::BTreeMap::new();
        genai_attrs.insert("gen_ai.system".to_string(), "openai".to_string());
        genai_attrs.insert("gen_ai.request.model".to_string(), "gpt-4".to_string());
        genai_attrs.insert("gen_ai.usage.input_tokens".to_string(), "100".to_string());

        let genai_span = SpanRecord {
            project_id: 1,
            deployment_id: None,
            resource: ResourceInfo::default(),
            trace_id: "genai-trace-1".into(),
            span_id: "span-1".into(),
            parent_span_id: None,
            name: "chat".into(),
            kind: SpanKind::Client,
            start_time: now,
            end_time: now,
            duration_ms: 500.0,
            status_code: SpanStatusCode::Ok,
            status_message: String::new(),
            attributes: genai_attrs,
            events: vec![],
        };

        let normal_span = SpanRecord {
            trace_id: "normal-trace".into(),
            span_id: "span-2".into(),
            name: "GET /api".into(),
            attributes: std::collections::BTreeMap::new(),
            ..genai_span.clone()
        };

        storage
            .spans
            .lock()
            .unwrap()
            .extend(vec![genai_span, normal_span]);

        let summaries = svc
            .query_genai_trace_summaries(TraceQuery {
                project_id: 1,
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].trace_id, "genai-trace-1");
        assert_eq!(summaries[0].gen_ai_system.as_deref(), Some("openai"));
        assert_eq!(summaries[0].gen_ai_model.as_deref(), Some("gpt-4"));
    }

    #[tokio::test]
    async fn test_get_genai_trace_spans() {
        let mock = MockOtelStorage::new();
        let (svc, storage) = make_service(mock);

        let now = chrono::Utc::now();
        let mut attrs = std::collections::BTreeMap::new();
        attrs.insert("gen_ai.system".to_string(), "anthropic".to_string());
        attrs.insert(
            "gen_ai.request.model".to_string(),
            "claude-sonnet-4-20250514".to_string(),
        );
        attrs.insert("gen_ai.operation.name".to_string(), "chat".to_string());
        attrs.insert("gen_ai.usage.input_tokens".to_string(), "50".to_string());
        attrs.insert("gen_ai.usage.output_tokens".to_string(), "200".to_string());

        let span = SpanRecord {
            project_id: 1,
            deployment_id: None,
            resource: ResourceInfo::default(),
            trace_id: "trace-abc".into(),
            span_id: "span-1".into(),
            parent_span_id: None,
            name: "chat".into(),
            kind: SpanKind::Client,
            start_time: now,
            end_time: now,
            duration_ms: 1200.0,
            status_code: SpanStatusCode::Ok,
            status_message: String::new(),
            attributes: attrs,
            events: vec![],
        };

        storage.spans.lock().unwrap().push(span);

        let details = svc.get_genai_trace_spans(1, "trace-abc").await.unwrap();

        assert_eq!(details.len(), 1);
        assert_eq!(details[0].gen_ai_system.as_deref(), Some("anthropic"));
        assert_eq!(
            details[0].gen_ai_model.as_deref(),
            Some("claude-sonnet-4-20250514")
        );
        assert_eq!(details[0].gen_ai_operation.as_deref(), Some("chat"));
        assert_eq!(details[0].input_tokens, Some(50));
        assert_eq!(details[0].output_tokens, Some(200));
    }

    #[tokio::test]
    async fn test_genai_handles_deprecated_provider_name() {
        let mock = MockOtelStorage::new();
        let (svc, storage) = make_service(mock);

        let now = chrono::Utc::now();

        // Use the new gen_ai.provider.name attribute (not deprecated gen_ai.system)
        let mut new_attrs = std::collections::BTreeMap::new();
        new_attrs.insert("gen_ai.provider.name".to_string(), "anthropic".to_string());
        new_attrs.insert(
            "gen_ai.request.model".to_string(),
            "claude-sonnet-4-20250514".to_string(),
        );
        new_attrs.insert("gen_ai.usage.input_tokens".to_string(), "100".to_string());

        // Use deprecated gen_ai.usage.prompt_tokens and gen_ai.system
        let mut old_attrs = std::collections::BTreeMap::new();
        old_attrs.insert("gen_ai.system".to_string(), "openai".to_string());
        old_attrs.insert("gen_ai.request.model".to_string(), "gpt-4".to_string());
        old_attrs.insert("gen_ai.usage.prompt_tokens".to_string(), "50".to_string());
        old_attrs.insert(
            "gen_ai.usage.completion_tokens".to_string(),
            "150".to_string(),
        );

        for (tid, attrs) in [("new-trace", new_attrs), ("old-trace", old_attrs)] {
            storage.spans.lock().unwrap().push(SpanRecord {
                project_id: 1,
                deployment_id: None,
                resource: ResourceInfo::default(),
                trace_id: tid.into(),
                span_id: format!("{}-s1", tid),
                parent_span_id: None,
                name: "chat".into(),
                kind: SpanKind::Client,
                start_time: now,
                end_time: now,
                duration_ms: 100.0,
                status_code: SpanStatusCode::Ok,
                status_message: String::new(),
                attributes: attrs,
                events: vec![],
            });
        }

        // Both should be found
        let count = svc
            .count_genai_traces(TraceQuery {
                project_id: 1,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(count, 2);

        // Get details for the span with deprecated attributes
        let details = svc.get_genai_trace_spans(1, "old-trace").await.unwrap();
        assert_eq!(details.len(), 1);
        assert_eq!(details[0].gen_ai_system.as_deref(), Some("openai"));
        assert_eq!(details[0].input_tokens, Some(50));
        assert_eq!(details[0].output_tokens, Some(150));

        // Get details for the span with new attributes
        let details = svc.get_genai_trace_spans(1, "new-trace").await.unwrap();
        assert_eq!(details.len(), 1);
        assert_eq!(details[0].gen_ai_system.as_deref(), Some("anthropic"));
        assert_eq!(details[0].input_tokens, Some(100));
    }

    #[tokio::test]
    async fn test_count_genai_traces() {
        let mock = MockOtelStorage::new();
        let (svc, storage) = make_service(mock);

        let now = chrono::Utc::now();
        let mut genai_attrs = std::collections::BTreeMap::new();
        genai_attrs.insert("gen_ai.system".to_string(), "openai".to_string());

        // Two GenAI traces, one normal trace
        for (tid, has_genai) in [("t1", true), ("t2", true), ("t3", false)] {
            let attrs = if has_genai {
                genai_attrs.clone()
            } else {
                std::collections::BTreeMap::new()
            };
            storage.spans.lock().unwrap().push(SpanRecord {
                project_id: 1,
                deployment_id: None,
                resource: ResourceInfo::default(),
                trace_id: tid.into(),
                span_id: format!("{}-s1", tid),
                parent_span_id: None,
                name: "op".into(),
                kind: SpanKind::Client,
                start_time: now,
                end_time: now,
                duration_ms: 100.0,
                status_code: SpanStatusCode::Ok,
                status_message: String::new(),
                attributes: attrs,
                events: vec![],
            });
        }

        let count = svc
            .count_genai_traces(TraceQuery {
                project_id: 1,
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(count, 2);
    }
}
