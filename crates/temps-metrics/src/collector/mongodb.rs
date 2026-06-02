//! MongoDB metric collector.
//!
//! Connects to a MongoDB instance and runs `db.adminCommand({serverStatus: 1})`
//! with a configurable timeout (default 5 s). The following metrics are
//! extracted from the response document:
//!
//! | Metric name                            | Kind    | Source path |
//! |----------------------------------------|---------|-------------|
//! | `mongo.connections_current`            | Gauge   | `connections.current` |
//! | `mongo.connections_available`          | Gauge   | `connections.available` |
//! | `mongo.connections_total_created`      | Counter | `connections.totalCreated` |
//! | `mongo.op_insert_total`                | Counter | `opcounters.insert` |
//! | `mongo.op_query_total`                 | Counter | `opcounters.query` |
//! | `mongo.op_update_total`                | Counter | `opcounters.update` |
//! | `mongo.op_delete_total`                | Counter | `opcounters.delete` |
//! | `mongo.op_getmore_total`               | Counter | `opcounters.getmore` |
//! | `mongo.op_command_total`               | Counter | `opcounters.command` |
//! | `mongo.network_bytes_in_total`         | Counter | `network.bytesIn` |
//! | `mongo.network_bytes_out_total`        | Counter | `network.bytesOut` |
//! | `mongo.network_requests_total`         | Counter | `network.numRequests` |
//! | `mongo.active_reads`                   | Gauge   | `globalLock.activeClients.readers` |
//! | `mongo.active_writes`                  | Gauge   | `globalLock.activeClients.writers` |
//! | `mongo.queued_reads`                   | Gauge   | `globalLock.currentQueue.readers` |
//! | `mongo.queued_writes`                  | Gauge   | `globalLock.currentQueue.writers` |
//! | `mongo.wiredtiger_cache_bytes_used`    | Gauge   | `wiredTiger.cache.bytes currently in the cache` |
//! | `mongo.wiredtiger_cache_bytes_max`     | Gauge   | `wiredTiger.cache.maximum bytes configured` |
//! | `mongo.wiredtiger_cache_ratio`         | Gauge   | `bytes currently in the cache` / `maximum bytes configured` |
//! | `mongo.wiredtiger_cache_dirty_ratio`   | Gauge   | `tracked dirty bytes in the cache` / `maximum bytes configured` |
//! | `mongo.wiredtiger_evicted_pages_total` | Counter | `wiredTiger.cache.unmodified pages evicted` |
//! | `mongo.document_inserted_total`        | Counter | `metrics.document.inserted` |
//! | `mongo.document_returned_total`        | Counter | `metrics.document.returned` |
//! | `mongo.document_updated_total`         | Counter | `metrics.document.updated` |
//! | `mongo.document_deleted_total`         | Counter | `metrics.document.deleted` |
//! | `mongo.cursor_open_total`              | Gauge   | `metrics.cursor.open.total` |
//! | `mongo.cursor_timed_out_total`         | Counter | `metrics.cursor.timedOut` |
//! | `mongo.replication_buffer_ratio`       | Gauge   | `repl.buffer.sizeBytes` / `repl.buffer.maxSizeBytes` |
//!
//! All errors are logged as warnings and result in an empty metric batch so
//! the scraper loop is never blocked by a slow or unreachable MongoDB instance.

use async_trait::async_trait;
use bson::doc;
use chrono::Utc;
use mongodb::{options::ClientOptions, Client};
use std::collections::HashMap;
use tracing::{debug, warn};

use super::{Collector, CollectorConfig};
use crate::error::MetricsError;
use crate::store::{MetricKind, MetricPoint};

/// MongoDB metric collector.
///
/// A new [`Client`] is created on every [`collect`] call. The MongoDB driver
/// maintains its own internal connection pool, but because we create and
/// immediately drop the client after each scrape the pool never warms up —
/// the overhead is one TCP handshake + `serverStatus` round-trip per scrape
/// interval, which is acceptable.
pub struct MongoCollector;

impl MongoCollector {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MongoCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Collector for MongoCollector {
    fn engine(&self) -> &'static str {
        "mongodb"
    }

    async fn collect(&self, config: &CollectorConfig) -> Result<Vec<MetricPoint>, MetricsError> {
        let source_id = config.source_id;
        let timeout = config.timeout;

        debug!(
            source_id,
            engine = "mongodb",
            "starting mongodb metric collection"
        );

        let collect_result = tokio::time::timeout(timeout, run_server_status(config)).await;

        match collect_result {
            Err(_elapsed) => {
                warn!(
                    source_id,
                    engine = "mongodb",
                    timeout_secs = timeout.as_secs(),
                    "mongodb serverStatus timed out; returning empty batch"
                );
                Ok(vec![])
            }
            Ok(Err(e)) => {
                warn!(
                    source_id,
                    engine = "mongodb",
                    error = %e,
                    "mongodb metric collection failed; returning empty batch"
                );
                Ok(vec![])
            }
            Ok(Ok(points)) => {
                debug!(
                    source_id,
                    engine = "mongodb",
                    point_count = points.len(),
                    "mongodb metric collection complete"
                );
                Ok(points)
            }
        }
    }
}

/// Connect to MongoDB, run `serverStatus`, and extract all metrics.
async fn run_server_status(
    config: &CollectorConfig,
) -> Result<Vec<MetricPoint>, Box<dyn std::error::Error + Send + Sync>> {
    // Parse connection string and apply the collection timeout as the server
    // selection timeout so we fail fast on unreachable hosts.
    let mut client_opts = ClientOptions::parse(&config.connection_string).await?;

    // Force direct connection to avoid replica-set topology discovery hanging
    // when internal member addresses are not reachable from the monitoring host.
    client_opts.direct_connection = Some(true);

    // Apply the collection timeout as the server-selection deadline so the
    // driver fails fast instead of waiting indefinitely for a primary.
    client_opts.server_selection_timeout = Some(config.timeout);

    let client = Client::with_options(client_opts)?;

    // Run db.adminCommand({serverStatus: 1}) on the `admin` database.
    let admin_db = client.database("admin");
    let status_doc = admin_db.run_command(doc! { "serverStatus": 1 }).await?;

    Ok(extract_metrics(&status_doc, config))
}

/// Extract metric points from a `serverStatus` BSON document.
fn extract_metrics(doc: &bson::Document, config: &CollectorConfig) -> Vec<MetricPoint> {
    let mut points: Vec<MetricPoint> = Vec::with_capacity(16);
    let now = Utc::now();

    // Build shared labels.
    let mut base_labels: HashMap<String, String> = HashMap::new();
    base_labels.insert("engine".into(), "mongodb".into());
    if let Some(env) = &config.environment {
        base_labels.insert("environment".into(), env.clone());
    }

    let source_kind = config.source_kind.clone();
    let source_id = config.source_id;

    // Helper to build a point with shared fields.
    let make_point = |name: &str, value: f64, kind: MetricKind| -> MetricPoint {
        MetricPoint {
            time: now,
            source_kind: source_kind.clone(),
            source_id,
            name: name.to_owned(),
            value,
            kind,
            engine: Some("mongodb".into()),
            environment: config.environment.clone(),
            node_id: config.node_id,
            labels: base_labels.clone(),
        }
    };

    // -------------------------------------------------------------------------
    // uptime & page faults
    // -------------------------------------------------------------------------
    if let Some(v) = bson_to_f64(doc, "uptimeMillis") {
        points.push(make_point(
            "mongo.uptime_seconds",
            v / 1000.0,
            MetricKind::Gauge,
        ));
    }
    if let Ok(ef) = doc.get_document("extra_info") {
        if let Some(v) = bson_to_f64(ef, "page_faults") {
            points.push(make_point("mongo.page_faults_total", v, MetricKind::Gauge));
        }
    }

    // -------------------------------------------------------------------------
    // asserts
    // -------------------------------------------------------------------------
    if let Ok(asserts) = doc.get_document("asserts") {
        for field in &["regular", "warning", "msg", "user", "rollovers"] {
            if let Some(v) = bson_to_f64(asserts, field) {
                points.push(make_point(
                    &format!("mongo.asserts_{}_total", field),
                    v,
                    MetricKind::Gauge,
                ));
            }
        }
    }

    // -------------------------------------------------------------------------
    // connections
    // -------------------------------------------------------------------------
    if let Ok(conn) = doc.get_document("connections") {
        if let Some(current) = bson_to_f64(conn, "current") {
            points.push(make_point(
                "mongo.connections_current",
                current,
                MetricKind::Gauge,
            ));
        }
        if let Some(available) = bson_to_f64(conn, "available") {
            points.push(make_point(
                "mongo.connections_available",
                available,
                MetricKind::Gauge,
            ));
        }
        if let Some(created) = bson_to_f64(conn, "totalCreated") {
            points.push(make_point(
                "mongo.connections_total_created",
                created,
                MetricKind::Gauge,
            ));
        }
    }

    // -------------------------------------------------------------------------
    // opcounters (cumulative counters since mongod start)
    // -------------------------------------------------------------------------
    if let Ok(ops) = doc.get_document("opcounters") {
        let counter_metrics = [
            ("insert", "mongo.op_insert_total"),
            ("query", "mongo.op_query_total"),
            ("update", "mongo.op_update_total"),
            ("delete", "mongo.op_delete_total"),
            ("getmore", "mongo.op_getmore_total"),
            ("command", "mongo.op_command_total"),
        ];
        for (field, metric_name) in &counter_metrics {
            if let Some(v) = bson_to_f64(ops, field) {
                points.push(make_point(metric_name, v, MetricKind::Gauge));
            }
        }
    }

    // -------------------------------------------------------------------------
    // network throughput
    // -------------------------------------------------------------------------
    if let Ok(net) = doc.get_document("network") {
        let net_metrics = [
            ("bytesIn", "mongo.network_bytes_in_total"),
            ("bytesOut", "mongo.network_bytes_out_total"),
            ("numRequests", "mongo.network_requests_total"),
        ];
        for (field, metric_name) in &net_metrics {
            if let Some(v) = bson_to_f64(net, field) {
                points.push(make_point(metric_name, v, MetricKind::Gauge));
            }
        }
    }

    // -------------------------------------------------------------------------
    // global lock — active clients and queued operations
    // -------------------------------------------------------------------------
    if let Ok(gl) = doc.get_document("globalLock") {
        if let Ok(ac) = gl.get_document("activeClients") {
            if let Some(v) = bson_to_f64(ac, "readers") {
                points.push(make_point("mongo.active_reads", v, MetricKind::Gauge));
            }
            if let Some(v) = bson_to_f64(ac, "writers") {
                points.push(make_point("mongo.active_writes", v, MetricKind::Gauge));
            }
        }
        if let Ok(cq) = gl.get_document("currentQueue") {
            if let Some(v) = bson_to_f64(cq, "readers") {
                points.push(make_point("mongo.queued_reads", v, MetricKind::Gauge));
            }
            if let Some(v) = bson_to_f64(cq, "writers") {
                points.push(make_point("mongo.queued_writes", v, MetricKind::Gauge));
            }
        }
    }

    // -------------------------------------------------------------------------
    // wiredTiger cache — usage ratios and eviction pressure
    // -------------------------------------------------------------------------
    if let Ok(wt) = doc.get_document("wiredTiger") {
        if let Ok(cache) = wt.get_document("cache") {
            let in_cache = bson_to_f64(cache, "bytes currently in the cache");
            let dirty = bson_to_f64(cache, "tracked dirty bytes in the cache");
            let max_cache = bson_to_f64(cache, "maximum bytes configured");
            let evicted = bson_to_f64(cache, "unmodified pages evicted");

            if let Some(used) = in_cache {
                points.push(make_point(
                    "mongo.wiredtiger_cache_bytes_used",
                    used,
                    MetricKind::Gauge,
                ));
            }
            if let Some(max) = max_cache {
                points.push(make_point(
                    "mongo.wiredtiger_cache_bytes_max",
                    max,
                    MetricKind::Gauge,
                ));
            }
            if let (Some(used), Some(max)) = (in_cache, max_cache) {
                let ratio = if max > 0.0 { used / max } else { 0.0 };
                points.push(make_point(
                    "mongo.wiredtiger_cache_ratio",
                    ratio,
                    MetricKind::Gauge,
                ));
            }
            if let (Some(d), Some(max)) = (dirty, max_cache) {
                let dirty_ratio = if max > 0.0 { d / max } else { 0.0 };
                points.push(make_point(
                    "mongo.wiredtiger_cache_dirty_ratio",
                    dirty_ratio,
                    MetricKind::Gauge,
                ));
            }
            if let Some(ev) = evicted {
                points.push(make_point(
                    "mongo.wiredtiger_evicted_pages_total",
                    ev,
                    MetricKind::Gauge,
                ));
            }
        }
    }

    // -------------------------------------------------------------------------
    // document metrics (higher fidelity than opcounters for actual row traffic)
    // -------------------------------------------------------------------------
    if let Ok(m) = doc.get_document("metrics") {
        if let Ok(d) = m.get_document("document") {
            let doc_metrics = [
                ("inserted", "mongo.document_inserted_total"),
                ("returned", "mongo.document_returned_total"),
                ("updated", "mongo.document_updated_total"),
                ("deleted", "mongo.document_deleted_total"),
            ];
            for (field, metric_name) in &doc_metrics {
                if let Some(v) = bson_to_f64(d, field) {
                    points.push(make_point(metric_name, v, MetricKind::Gauge));
                }
            }
        }
        if let Ok(cur) = m.get_document("cursor") {
            if let Ok(open) = cur.get_document("open") {
                if let Some(v) = bson_to_f64(open, "total") {
                    points.push(make_point("mongo.cursor_open_total", v, MetricKind::Gauge));
                }
            }
            if let Some(v) = bson_to_f64(cur, "timedOut") {
                points.push(make_point(
                    "mongo.cursor_timed_out_total",
                    v,
                    MetricKind::Gauge,
                ));
            }
        }
    }

    // -------------------------------------------------------------------------
    // replication buffer utilisation ratio (only present on replica-set members)
    // -------------------------------------------------------------------------
    if let Ok(repl) = doc.get_document("repl") {
        if let Ok(buf) = repl.get_document("buffer") {
            let size_bytes = bson_to_f64(buf, "sizeBytes");
            let max_size_bytes = bson_to_f64(buf, "maxSizeBytes");

            if let (Some(sz), Some(max)) = (size_bytes, max_size_bytes) {
                let ratio = if max > 0.0 { sz / max } else { 0.0 };
                points.push(make_point(
                    "mongo.replication_buffer_ratio",
                    ratio,
                    MetricKind::Gauge,
                ));
            }
        }
    }

    points
}

/// Extract a numeric value from a BSON document field, regardless of whether
/// the underlying BSON type is `Int32`, `Int64`, or `Double`.
///
/// Returns `None` if the field is absent or cannot be converted to `f64`.
fn bson_to_f64(doc: &bson::Document, key: &str) -> Option<f64> {
    match doc.get(key) {
        Some(bson::Bson::Int32(v)) => Some(*v as f64),
        Some(bson::Bson::Int64(v)) => Some(*v as f64),
        Some(bson::Bson::Double(v)) => Some(*v),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SourceKind;
    use bson::doc;
    use std::time::Duration;

    fn make_config() -> CollectorConfig {
        CollectorConfig {
            source_id: 10,
            source_kind: SourceKind::Database,
            connection_string: "mongodb://127.0.0.1:27017".to_owned(),
            environment: Some("test".to_owned()),
            node_id: None,
            timeout: Duration::from_secs(5),
        }
    }

    #[test]
    fn mongo_collector_engine_name() {
        let col = MongoCollector::new();
        assert_eq!(col.engine(), "mongodb");
    }

    #[test]
    fn bson_to_f64_conversions() {
        let d = doc! {
            "int32": 42_i32,
            "int64": 1_000_000_i64,
            "double": 1.618_f64,
            "string": "not_a_number",
        };
        assert_eq!(bson_to_f64(&d, "int32"), Some(42.0));
        assert_eq!(bson_to_f64(&d, "int64"), Some(1_000_000.0));
        assert!((bson_to_f64(&d, "double").unwrap() - 1.618).abs() < 1e-9);
        assert_eq!(bson_to_f64(&d, "string"), None);
        assert_eq!(bson_to_f64(&d, "missing"), None);
    }

    #[test]
    fn extract_metrics_connections_and_opcounters() {
        let status = doc! {
            "connections": {
                "current": 5_i32,
                "available": 195_i32,
            },
            "opcounters": {
                "insert": 100_i64,
                "query": 200_i64,
                "update": 50_i64,
                "delete": 10_i64,
                "getmore": 0_i64,
                "command": 300_i64,
            },
        };
        let config = make_config();
        let points = extract_metrics(&status, &config);

        let find =
            |name: &str| -> Option<f64> { points.iter().find(|p| p.name == name).map(|p| p.value) };

        assert_eq!(find("mongo.connections_current"), Some(5.0));
        assert_eq!(find("mongo.connections_available"), Some(195.0));
        assert_eq!(find("mongo.op_insert_total"), Some(100.0));
        assert_eq!(find("mongo.op_query_total"), Some(200.0));
        assert_eq!(find("mongo.op_update_total"), Some(50.0));
        assert_eq!(find("mongo.op_delete_total"), Some(10.0));
        // No wiredTiger or repl sections → those metrics absent.
        assert!(find("mongo.wiredtiger_cache_ratio").is_none());
        assert!(find("mongo.replication_buffer_ratio").is_none());
    }

    #[test]
    fn extract_metrics_wiredtiger_cache_ratio() {
        let status = doc! {
            "connections": { "current": 1_i32, "available": 99_i32 },
            "opcounters": { "insert": 0_i64, "query": 0_i64, "update": 0_i64, "delete": 0_i64 },
            "wiredTiger": {
                "cache": {
                    "bytes currently in the cache": 512_i64,
                    "maximum bytes configured": 1024_i64,
                }
            },
        };
        let config = make_config();
        let points = extract_metrics(&status, &config);
        let ratio = points
            .iter()
            .find(|p| p.name == "mongo.wiredtiger_cache_ratio")
            .map(|p| p.value);
        assert_eq!(ratio, Some(0.5));
    }

    #[test]
    fn extract_metrics_wiredtiger_zero_max_does_not_panic() {
        let status = doc! {
            "connections": { "current": 0_i32, "available": 0_i32 },
            "opcounters": { "insert": 0_i64, "query": 0_i64, "update": 0_i64, "delete": 0_i64 },
            "wiredTiger": {
                "cache": {
                    "bytes currently in the cache": 0_i64,
                    "maximum bytes configured": 0_i64,
                }
            },
        };
        let config = make_config();
        let points = extract_metrics(&status, &config);
        let ratio = points
            .iter()
            .find(|p| p.name == "mongo.wiredtiger_cache_ratio")
            .map(|p| p.value);
        // Zero max → ratio is 0.0 (not NaN / divide-by-zero panic).
        assert_eq!(ratio, Some(0.0));
    }

    #[test]
    fn extract_metrics_replication_buffer_ratio() {
        let status = doc! {
            "connections": { "current": 2_i32, "available": 98_i32 },
            "opcounters": { "insert": 0_i64, "query": 0_i64, "update": 0_i64, "delete": 0_i64 },
            "repl": {
                "buffer": {
                    "sizeBytes": 256_i64,
                    "maxSizeBytes": 1024_i64,
                }
            },
        };
        let config = make_config();
        let points = extract_metrics(&status, &config);
        let ratio = points
            .iter()
            .find(|p| p.name == "mongo.replication_buffer_ratio")
            .map(|p| p.value);
        assert_eq!(ratio, Some(0.25));
    }

    #[test]
    fn extract_metrics_labels_contain_engine_and_environment() {
        let status = doc! {
            "connections": { "current": 1_i32, "available": 1_i32 },
            "opcounters": { "insert": 0_i64, "query": 0_i64, "update": 0_i64, "delete": 0_i64 },
        };
        let config = make_config();
        let points = extract_metrics(&status, &config);
        for p in &points {
            assert_eq!(p.labels.get("engine").map(|s| s.as_str()), Some("mongodb"));
            assert_eq!(
                p.labels.get("environment").map(|s| s.as_str()),
                Some("test")
            );
            assert_eq!(p.engine.as_deref(), Some("mongodb"));
        }
    }

    #[tokio::test]
    async fn mongo_collector_returns_empty_on_bad_connection() {
        let col = MongoCollector::new();
        let config = CollectorConfig {
            source_id: 99,
            source_kind: SourceKind::Database,
            // Port 19998 should be unreachable on any CI box.
            connection_string: "mongodb://127.0.0.1:19998".to_owned(),
            environment: None,
            node_id: None,
            timeout: Duration::from_secs(2),
        };

        let result = col.collect(&config).await;
        // Must not propagate error.
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }
}
