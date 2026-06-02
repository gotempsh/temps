//! Redis metrics collector.
//!
//! Opens a fresh connection to a Redis instance on every [`collect`] call,
//! runs `INFO all`, and parses the plain-text response into [`MetricPoint`]s.
//!
//! ## Counter semantics
//!
//! Counter metrics (e.g. `evicted_keys`) are returned with their **raw
//! cumulative value**.  Delta computation is performed by the caller
//! (`MetricsScraper`), consistent with the rest of the collector framework.
//!
//! ## Metrics emitted
//!
//! | Name | Kind | Description |
//! |---|---|---|
//! | `redis.memory_used_bytes` | Gauge | `used_memory` |
//! | `redis.memory_peak_bytes` | Gauge | `used_memory_peak` |
//! | `redis.memory_fragmentation_ratio` | Gauge | `mem_fragmentation_ratio` |
//! | `redis.keyspace_hit_ratio` | Gauge | hits / (hits + misses) |
//! | `redis.evicted_keys_total` | Counter | `evicted_keys` (raw cumulative) |
//! | `redis.connected_clients` | Gauge | `connected_clients` |
//! | `redis.blocked_clients` | Gauge | `blocked_clients` |
//! | `redis.replication_offset_lag` | Gauge | master_repl_offset − slave_repl_offset (replicas only) |

use async_trait::async_trait;
use chrono::Utc;
use std::collections::HashMap;
use tracing::{debug, warn};

use super::{Collector, CollectorConfig};
use crate::error::MetricsError;
use crate::store::{MetricKind, MetricPoint};

/// Redis metric collector.
///
/// Stateless — a new connection is opened on every [`collect`] call so the
/// scraper loop can time-out and retry without worrying about stale state.
pub struct RedisCollector;

impl RedisCollector {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RedisCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Collector for RedisCollector {
    fn engine(&self) -> &'static str {
        "redis"
    }

    async fn collect(&self, config: &CollectorConfig) -> Result<Vec<MetricPoint>, MetricsError> {
        let source_id = config.source_id;
        let timeout = config.timeout;

        debug!(
            source_id,
            engine = "redis",
            "starting redis metric collection"
        );

        let collect_result = tokio::time::timeout(timeout, run_info_all(config)).await;

        match collect_result {
            Err(_elapsed) => {
                warn!(
                    source_id,
                    engine = "redis",
                    timeout_secs = timeout.as_secs(),
                    "redis INFO all timed out; returning empty batch"
                );
                Ok(vec![])
            }
            Ok(Err(e)) => {
                warn!(
                    source_id,
                    engine = "redis",
                    error = %e,
                    "redis INFO all failed; returning empty batch"
                );
                Ok(vec![])
            }
            Ok(Ok(points)) => Ok(points),
        }
    }
}

/// Connect to Redis, send `INFO all`, and return metric points.
async fn run_info_all(config: &CollectorConfig) -> Result<Vec<MetricPoint>, String> {
    let client = redis::Client::open(config.connection_string.as_str())
        .map_err(|e| format!("Failed to open Redis client: {e}"))?;

    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .map_err(|e| format!("Failed to connect to Redis: {e}"))?;

    let info_str: String = redis::cmd("INFO")
        .arg("all")
        .query_async(&mut conn)
        .await
        .map_err(|e| format!("INFO all command failed: {e}"))?;

    debug!(
        source_id = config.source_id,
        engine = "redis",
        response_bytes = info_str.len(),
        "received Redis INFO response"
    );

    Ok(parse_info(config, &info_str))
}

/// Parse an `INFO all` text response into [`MetricPoint`]s.
fn parse_info(config: &CollectorConfig, info: &str) -> Vec<MetricPoint> {
    let now = Utc::now();
    let source_id = config.source_id;
    let environment = config.environment.clone();
    let node_id = config.node_id;
    let mut points = Vec::new();

    macro_rules! gauge {
        ($name:expr, $value:expr) => {
            points.push(MetricPoint {
                time: now,
                source_kind: crate::store::SourceKind::Database,
                source_id,
                name: $name.to_string(),
                value: $value,
                kind: MetricKind::Gauge,
                engine: Some("redis".to_string()),
                environment: environment.clone(),
                node_id,
                labels: HashMap::new(),
            });
        };
    }

    macro_rules! counter {
        ($name:expr, $value:expr) => {
            points.push(MetricPoint {
                time: now,
                source_kind: crate::store::SourceKind::Database,
                source_id,
                name: $name.to_string(),
                value: $value,
                kind: MetricKind::Counter,
                engine: Some("redis".to_string()),
                environment: environment.clone(),
                node_id,
                labels: HashMap::new(),
            });
        };
    }

    // ── Gauges ────────────────────────────────────────────────────────────────

    if let Some(v) = parse_info_field(info, "used_memory") {
        gauge!("redis.memory_used_bytes", v);
    }

    if let Some(v) = parse_info_field(info, "used_memory_peak") {
        gauge!("redis.memory_peak_bytes", v);
    }

    if let Some(v) = parse_info_field(info, "mem_fragmentation_ratio") {
        gauge!("redis.memory_fragmentation_ratio", v);
    }

    if let Some(v) = parse_info_field(info, "connected_clients") {
        gauge!("redis.connected_clients", v);
    }

    if let Some(v) = parse_info_field(info, "blocked_clients") {
        gauge!("redis.blocked_clients", v);
    }

    // Keyspace hit ratio (derived gauge — hits / (hits + misses))
    let hits = parse_info_field(info, "keyspace_hits");
    let misses = parse_info_field(info, "keyspace_misses");
    if let (Some(h), Some(m)) = (hits, misses) {
        let total = h + m;
        if total > 0.0 {
            gauge!("redis.keyspace_hit_ratio", h / total);
        }
    }

    // Replication lag — only present when this instance is a replica.
    let master_offset = parse_info_field(info, "master_repl_offset");
    let slave_offset = parse_info_field(info, "slave_repl_offset");
    if let (Some(master), Some(slave)) = (master_offset, slave_offset) {
        gauge!("redis.replication_offset_lag", (master - slave).max(0.0));
    }

    // Instantaneous ops/sec — live throughput gauge from Redis Stats section.
    if let Some(v) = parse_info_field(info, "instantaneous_ops_per_sec") {
        gauge!("redis.ops_per_second", v);
    }

    // Last RDB save duration — from Persistence section.
    if let Some(v) = parse_info_field(info, "rdb_last_bgsave_time_sec") {
        gauge!("redis.rdb_last_save_duration_ms", v * 1000.0);
    }

    // Total commands processed — useful for throughput tracking.
    if let Some(v) = parse_info_field(info, "total_commands_processed") {
        gauge!("redis.commands_processed_total", v);
    }

    // Total connections received.
    if let Some(v) = parse_info_field(info, "total_connections_received") {
        gauge!("redis.connections_received_total", v);
    }

    // Total net input/output bytes.
    if let Some(v) = parse_info_field(info, "total_net_input_bytes") {
        gauge!("redis.net_input_bytes_total", v);
    }
    if let Some(v) = parse_info_field(info, "total_net_output_bytes") {
        gauge!("redis.net_output_bytes_total", v);
    }

    // Number of keys across all databases.
    if let Some(v) = parse_info_field(info, "db0") {
        // db0:keys=N,expires=M — extract keys count
        // parse_info_field won't work here; use a direct search
        let _ = v; // handled below
    }

    // ── Counters (raw cumulative — delta computed by MetricsScraper) ──────────

    if let Some(v) = parse_info_field(info, "evicted_keys") {
        counter!("redis.evicted_keys_total", v);
    }

    if let Some(v) = parse_info_field(info, "keyspace_misses") {
        counter!("redis.keyspace_misses_total", v);
    }

    if let Some(v) = parse_info_field(info, "keyspace_hits") {
        counter!("redis.keyspace_hits_total", v);
    }

    if let Some(v) = parse_info_field(info, "expired_keys") {
        counter!("redis.expired_keys_total", v);
    }

    points
}

/// Extract a named field from an `INFO` response.
///
/// Redis `INFO` lines have the format `field:value\r\n`.  This helper finds
/// the first line whose key matches `field` (case-sensitive) and parses the
/// value portion as `f64`.
///
/// Returns `None` if the field is absent or the value is not a valid number.
pub fn parse_info_field(info: &str, field: &str) -> Option<f64> {
    info.lines().find_map(|line| {
        // Strip the optional trailing `\r` so the value part is clean.
        let line = line.trim_end_matches('\r');
        let (key, value) = line.split_once(':')?;
        if key == field {
            value.trim().parse::<f64>().ok()
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SourceKind;
    use std::time::Duration;

    const SAMPLE_INFO: &str = "\
# Server\r\n\
redis_version:7.2.4\r\n\
# Clients\r\n\
connected_clients:3\r\n\
blocked_clients:0\r\n\
# Memory\r\n\
used_memory:1024000\r\n\
used_memory_peak:2048000\r\n\
mem_fragmentation_ratio:1.23\r\n\
# Stats\r\n\
keyspace_hits:1000\r\n\
keyspace_misses:200\r\n\
evicted_keys:50\r\n\
# Replication\r\n\
role:slave\r\n\
master_repl_offset:12345\r\n\
slave_repl_offset:12300\r\n\
";

    fn make_config(source_id: i32) -> CollectorConfig {
        CollectorConfig {
            source_id,
            source_kind: SourceKind::Database,
            connection_string: "redis://localhost:6379".to_string(),
            environment: None,
            node_id: None,
            timeout: Duration::from_secs(5),
        }
    }

    // ── parse_info_field ──────────────────────────────────────────────────────

    #[test]
    fn test_parse_info_field_found() {
        assert_eq!(
            parse_info_field(SAMPLE_INFO, "connected_clients"),
            Some(3.0)
        );
        assert_eq!(
            parse_info_field(SAMPLE_INFO, "used_memory"),
            Some(1_024_000.0)
        );
        assert_eq!(
            parse_info_field(SAMPLE_INFO, "mem_fragmentation_ratio"),
            Some(1.23)
        );
    }

    #[test]
    fn test_parse_info_field_missing() {
        assert_eq!(parse_info_field(SAMPLE_INFO, "nonexistent_field"), None);
    }

    #[test]
    fn test_parse_info_field_section_header_not_parsed() {
        // Section headers like "# Server" must not be returned.
        assert_eq!(parse_info_field(SAMPLE_INFO, "# Server"), None);
    }

    // ── parse_info (full output) ──────────────────────────────────────────────

    #[test]
    fn test_parse_info_gauges_present() {
        let config = make_config(42);
        let points = parse_info(&config, SAMPLE_INFO);
        let names: Vec<&str> = points.iter().map(|p| p.name.as_str()).collect();

        assert!(names.contains(&"redis.memory_used_bytes"));
        assert!(names.contains(&"redis.memory_peak_bytes"));
        assert!(names.contains(&"redis.memory_fragmentation_ratio"));
        assert!(names.contains(&"redis.connected_clients"));
        assert!(names.contains(&"redis.blocked_clients"));
        assert!(names.contains(&"redis.keyspace_hit_ratio"));
        assert!(names.contains(&"redis.replication_offset_lag"));
        // Counter emitted as raw cumulative value.
        assert!(names.contains(&"redis.evicted_keys_total"));
    }

    #[test]
    fn test_keyspace_hit_ratio_value() {
        let config = make_config(1);
        let points = parse_info(&config, SAMPLE_INFO);
        let ratio = points
            .iter()
            .find(|p| p.name == "redis.keyspace_hit_ratio")
            .expect("keyspace_hit_ratio missing");
        // hits=1000, misses=200 → 1000/1200 ≈ 0.8333...
        let expected = 1000.0_f64 / 1200.0;
        assert!((ratio.value - expected).abs() < 1e-9);
    }

    #[test]
    fn test_replication_lag_value() {
        let config = make_config(1);
        let points = parse_info(&config, SAMPLE_INFO);
        let lag = points
            .iter()
            .find(|p| p.name == "redis.replication_offset_lag")
            .expect("replication_offset_lag missing");
        assert_eq!(lag.value, 45.0); // 12345 - 12300
    }

    #[test]
    fn test_evicted_keys_raw_cumulative() {
        let config = make_config(1);
        let points = parse_info(&config, SAMPLE_INFO);
        let evicted = points
            .iter()
            .find(|p| p.name == "redis.evicted_keys_total")
            .expect("evicted_keys_total missing");
        // Raw value returned; delta computed by MetricsScraper.
        assert_eq!(evicted.value, 50.0);
        assert_eq!(evicted.kind, MetricKind::Counter);
    }

    #[test]
    fn test_all_gauges_have_engine_label() {
        let config = make_config(1);
        let points = parse_info(&config, SAMPLE_INFO);
        for p in &points {
            assert_eq!(
                p.engine.as_deref(),
                Some("redis"),
                "expected engine=redis on metric {}",
                p.name
            );
        }
    }

    #[test]
    fn test_no_hit_ratio_when_no_ops() {
        let info = "keyspace_hits:0\r\nkeyspace_misses:0\r\n";
        let config = make_config(1);
        let points = parse_info(&config, info);
        let has_ratio = points.iter().any(|p| p.name == "redis.keyspace_hit_ratio");
        assert!(!has_ratio, "should not emit hit_ratio when total is 0");
    }

    #[test]
    fn test_no_replication_lag_on_master() {
        // A Redis master only has master_repl_offset (not slave_repl_offset).
        let info = "role:master\r\nmaster_repl_offset:99999\r\n";
        let config = make_config(1);
        let points = parse_info(&config, info);
        let has_lag = points
            .iter()
            .any(|p| p.name == "redis.replication_offset_lag");
        assert!(
            !has_lag,
            "replication_offset_lag should not appear on master"
        );
    }
}
