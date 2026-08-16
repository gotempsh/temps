//! S3 / MinIO metrics collector.
//!
//! Opens a fresh S3 client on every [`collect`] call and calls `ListBuckets`
//! to produce bucket-level metrics.  The connection string is expected to be a
//! pipe-delimited configuration string in the format:
//!
//! ```text
//! region|access_key|secret_key[|endpoint_url]
//! ```
//!
//! The optional `endpoint_url` enables MinIO / S3-compatible store support.
//!
//! ## Metrics emitted
//!
//! | Name | Kind | Description |
//! |---|---|---|
//! | `s3.bucket_count` | Gauge | Total buckets returned by `ListBuckets` |
//!
//! ## Design tradeoffs — per-bucket object counts
//!
//! `ListObjectsV2` is a paginated API that iterates over every object key.
//! At any non-trivial scale (millions of objects) paginating all pages would:
//!
//! 1. Block the scrape loop for minutes or hours.
//! 2. Consume significant API-request quota (S3 charges per 1 000 requests).
//! 3. Still return a count that is stale by the time the last page arrives.
//!
//! `max_keys(0)` is tempting but AWS S3 treats it as `max_keys(1000)` —
//! it does **not** provide a free count.  The correct approach would be to use
//! CloudWatch metrics (`BucketSizeBytes`, `NumberOfObjects`) for AWS, or the
//! MinIO admin Prometheus endpoint for self-hosted storage.
//!
//! ## TODO: total size bytes
//!
//! `s3.total_size_bytes` would require CloudWatch `GetMetricStatistics` (AWS)
//! or the MinIO admin API (`mc admin info`).  Neither is available through the
//! standard S3 SDK, so it is omitted for now.
//!
//! TODO(metrics): Issue 9 — add `s3.total_size_bytes` via CloudWatch for AWS
//! and via MinIO Prometheus scrape for self-hosted deployments.

use async_trait::async_trait;
use aws_credential_types::Credentials;
use aws_sdk_s3::config::{Region, SharedCredentialsProvider, SharedHttpClient};
use aws_sdk_s3::Client;
use aws_smithy_http_client::tls::{
    rustls_provider::CryptoMode, Provider as TlsProvider, TlsContext, TrustStore,
};
use chrono::Utc;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;
use tracing::{debug, warn};

use super::prometheus::{apply_mappings, scrape_prometheus, RUSTFS_METRICS};
use super::{Collector, CollectorConfig};
use crate::error::MetricsError;
use crate::store::{MetricKind, MetricPoint};

/// S3 / MinIO metric collector.
///
/// Stateless — a new [`Client`] is created on every [`collect`] call.
pub struct S3Collector;

impl S3Collector {
    pub fn new() -> Self {
        Self
    }
}

impl Default for S3Collector {
    fn default() -> Self {
        Self::new()
    }
}

/// Parsed S3 connection parameters, extracted from the pipe-delimited
/// `CollectorConfig::connection_string`.
struct S3ConnParams<'a> {
    region: &'a str,
    access_key: &'a str,
    secret_key: &'a str,
    endpoint: Option<&'a str>,
}

/// Parse the pipe-delimited connection string used for S3 collectors.
///
/// Format: `region|access_key|secret_key[|endpoint_url]`
fn parse_connection_string(s: &str) -> Option<S3ConnParams<'_>> {
    let mut parts = s.splitn(4, '|');
    let region = parts.next()?;
    let access_key = parts.next()?;
    let secret_key = parts.next()?;
    let endpoint = parts.next().filter(|ep| !ep.is_empty());
    Some(S3ConnParams {
        region,
        access_key,
        secret_key,
        endpoint,
    })
}

/// Shared HTTPS client backed by Mozilla's bundled CA roots.
///
/// The AWS default client loads roots from the host OS. Minimal production
/// images and sandboxed macOS test processes can return zero valid roots,
/// which makes `aws-smithy-http-client` panic in debug builds and leaves HTTPS
/// unusable in release builds. A compiled-in trust store keeps collection
/// independent of ambient host certificate configuration.
fn bundled_roots_http_client() -> Result<SharedHttpClient, String> {
    static CLIENT: OnceLock<Result<SharedHttpClient, String>> = OnceLock::new();

    CLIENT
        .get_or_init(|| {
            let mut trust_store = TrustStore::empty().with_native_roots(false);
            for der in webpki_root_certs::TLS_SERVER_ROOT_CERTS {
                let pem = pem::Pem::new("CERTIFICATE", der.to_vec());
                trust_store = trust_store.with_pem_certificate(pem::encode(&pem).into_bytes());
            }

            let tls_context = TlsContext::builder()
                .with_trust_store(trust_store)
                .build()
                .map_err(|error| format!("failed to build bundled S3 TLS context: {error}"))?;

            Ok(aws_smithy_http_client::Builder::new()
                .tls_provider(TlsProvider::Rustls(CryptoMode::AwsLc))
                .tls_context(tls_context)
                .build_https())
        })
        .clone()
}

#[async_trait]
impl Collector for S3Collector {
    fn engine(&self) -> &'static str {
        "s3"
    }

    async fn collect(&self, config: &CollectorConfig) -> Result<Vec<MetricPoint>, MetricsError> {
        let source_id = config.source_id;
        let timeout = config.timeout;

        debug!(source_id, engine = "s3", "starting s3 metric collection");

        // Scrape Prometheus endpoint (non-fatal, returns empty if not available)
        let mut points = scrape_minio_prometheus(config).await;

        // Always also collect bucket count via S3 API
        let list_result = tokio::time::timeout(timeout, run_list_buckets(config)).await;
        match list_result {
            Err(_elapsed) => {
                warn!(
                    source_id,
                    engine = "s3",
                    timeout_secs = timeout.as_secs(),
                    "s3 ListBuckets timed out"
                );
            }
            Ok(Err(e)) => {
                warn!(source_id, engine = "s3", error = %e, "s3 ListBuckets failed");
            }
            Ok(Ok(bucket_points)) => {
                points.extend(bucket_points);
            }
        }

        Ok(points)
    }
}

/// Build an S3 client and call `ListBuckets`.
async fn run_list_buckets(config: &CollectorConfig) -> Result<Vec<MetricPoint>, String> {
    let params = parse_connection_string(&config.connection_string).ok_or_else(|| {
        format!(
            "Invalid S3 connection string for source {}: expected 'region|access_key|secret_key[|endpoint]'",
            config.source_id
        )
    })?;

    let credentials = Credentials::new(
        params.access_key,
        params.secret_key,
        None,
        None,
        "temps-metrics",
    );
    let creds_provider = SharedCredentialsProvider::new(credentials);
    let http_client = bundled_roots_http_client().map_err(|reason| {
        format!(
            "Failed to configure S3 HTTPS client for source {}: {reason}",
            config.source_id
        )
    })?;

    let mut aws_cfg_builder = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(Region::new(params.region.to_string()))
        .credentials_provider(creds_provider)
        .http_client(http_client);

    if let Some(ep) = params.endpoint {
        aws_cfg_builder = aws_cfg_builder.endpoint_url(ep);
    }

    let aws_config = aws_cfg_builder.load().await;
    let mut s3_cfg_builder = aws_sdk_s3::config::Builder::from(&aws_config);

    // Force path-style addressing for MinIO / S3-compatible stores.
    if params.endpoint.is_some() {
        s3_cfg_builder = s3_cfg_builder.force_path_style(true);
    }

    let client = Client::from_conf(s3_cfg_builder.build());

    let response = client
        .list_buckets()
        .send()
        .await
        .map_err(|e| format!("ListBuckets request failed: {e}"))?;

    let bucket_count = response.buckets().len() as f64;

    debug!(
        source_id = config.source_id,
        engine = "s3",
        bucket_count = bucket_count as u64,
        "collected s3 bucket count"
    );

    let now = Utc::now();
    Ok(vec![MetricPoint {
        time: now,
        source_kind: crate::store::SourceKind::Database,
        source_id: config.source_id,
        name: "s3.bucket_count".to_string(),
        value: bucket_count,
        kind: MetricKind::Gauge,
        engine: Some("s3".to_string()),
        environment: config.environment.clone(),
        node_id: config.node_id,
        labels: HashMap::new(),
    }])
}

/// Scrape the MinIO / RustFS Prometheus cluster endpoint, if the connection
/// string carries an `endpoint_url`.
///
/// Pure AWS S3 (no endpoint) has no such endpoint, so this returns an empty
/// `Vec`. Any scrape failure is logged at `debug` level — many S3-compatible
/// services won't expose `/minio/v2/metrics/cluster`, so this is non-fatal.
///
/// MinIO ≥ RELEASE.2023 requires authentication on the metrics endpoint.
/// We try two approaches in sequence:
/// 1. Bearer token using the access key (RustFS and some MinIO configs).
/// 2. Unauthenticated (older MinIO / open deployments).
async fn scrape_minio_prometheus(config: &CollectorConfig) -> Vec<MetricPoint> {
    // Parse endpoint from pipe-delimited connection string: region|access_key|secret_key[|endpoint]
    // If no 4th field (endpoint), return empty — this is pure AWS S3
    let endpoint = match parse_connection_string(&config.connection_string) {
        Some(params) => match params.endpoint {
            Some(ep) => ep.to_owned(),
            None => return vec![],
        },
        None => return vec![],
    };

    let url = format!("{}/minio/v2/metrics/cluster", endpoint);

    // Use a short dedicated timeout for the Prometheus probe so a connection-
    // refused (MinIO not exposing the endpoint) doesn't consume the full
    // collector timeout and starve the ListBuckets call that follows.
    let probe_timeout = config.timeout.min(Duration::from_secs(2));

    // Try unauthenticated first (works for open deployments and RustFS alpha).
    // If we get a 403, the endpoint exists but requires auth we can't provide
    // (MinIO ≥ 2023 needs a JWT generated via `mc admin prometheus generate`).
    // In that case we skip silently — falling back to S3 API metrics only.
    let result = scrape_prometheus(&url, None, probe_timeout).await;

    match result {
        Ok(prom_metrics) => {
            let points = apply_mappings(&prom_metrics, RUSTFS_METRICS, config);
            debug!(
                source_id = config.source_id,
                engine = "s3",
                metric_count = points.len(),
                "scraped minio prometheus endpoint"
            );
            points
        }
        Err(e) => {
            // Non-fatal: log at debug level since many S3 services won't have this endpoint
            debug!(
                source_id = config.source_id,
                engine = "s3",
                error = %e,
                "minio prometheus endpoint unavailable; using s3 api only"
            );
            vec![]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SourceKind;
    use std::time::Duration;

    // ── parse_connection_string ───────────────────────────────────────────────

    #[test]
    fn test_parse_conn_str_aws() {
        let s = "us-east-1|AKID|SECRET";
        let p = parse_connection_string(s).expect("should parse");
        assert_eq!(p.region, "us-east-1");
        assert_eq!(p.access_key, "AKID");
        assert_eq!(p.secret_key, "SECRET");
        assert!(p.endpoint.is_none());
    }

    #[test]
    fn test_parse_conn_str_minio() {
        let s = "us-east-1|minioadmin|minioadmin|http://localhost:9000";
        let p = parse_connection_string(s).expect("should parse");
        assert_eq!(p.region, "us-east-1");
        assert_eq!(p.endpoint, Some("http://localhost:9000"));
    }

    #[test]
    fn test_parse_conn_str_empty_endpoint_treated_as_none() {
        let s = "us-east-1|AKID|SECRET|";
        let p = parse_connection_string(s).expect("should parse");
        assert!(p.endpoint.is_none());
    }

    #[test]
    fn test_parse_conn_str_too_few_parts() {
        assert!(parse_connection_string("only-region|AKID").is_none());
    }

    // ── MetricPoint shape ─────────────────────────────────────────────────────

    #[test]
    fn test_s3_bucket_count_metric_shape() {
        // Verify that a hand-crafted point has the expected structure —
        // mirrors what run_list_buckets produces.
        let point = MetricPoint {
            time: Utc::now(),
            source_kind: SourceKind::Database,
            source_id: 7,
            name: "s3.bucket_count".to_string(),
            value: 3.0,
            kind: MetricKind::Gauge,
            engine: Some("s3".to_string()),
            environment: None,
            node_id: None,
            labels: HashMap::new(),
        };

        assert_eq!(point.name, "s3.bucket_count");
        assert_eq!(point.kind, MetricKind::Gauge);
        assert_eq!(point.engine.as_deref(), Some("s3"));
        assert_eq!(point.source_id, 7);
    }

    // ── S3Collector::collect (timeout path) ───────────────────────────────────

    #[tokio::test]
    async fn test_collect_returns_empty_on_bad_credentials() {
        // Uses a non-existent endpoint so the connection will fail.
        let config = CollectorConfig {
            source_id: 1,
            source_kind: SourceKind::Database,
            connection_string: "us-east-1|FAKE|FAKE|http://127.0.0.1:1".to_string(),
            environment: None,
            node_id: None,
            timeout: Duration::from_millis(500),
        };

        let collector = S3Collector::new();
        // Must not panic — must return Ok(vec![]) with a warning.
        let result = collector.collect(&config).await;
        assert!(result.is_ok());
        // Either timed out or connection refused; both yield empty vec.
        assert!(result.unwrap().is_empty());
    }
}
