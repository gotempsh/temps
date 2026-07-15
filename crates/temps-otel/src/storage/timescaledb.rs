//! TimescaleDB storage backend for OTel data.
//!
//! Stores metrics, traces, and logs in TimescaleDB hypertables with
//! automatic downsampling via continuous aggregates, compression, and
//! retention policies.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, FromQueryResult, Statement};
use std::sync::Arc;
use tracing::{debug, error, warn};

use super::{BaselinePoint, DeployEvent, MinuteAggregate, OtelStorage, StorageResult};
use crate::error::OtelError;
use crate::types::*;

// ── Interval / aggregation helpers ─────────────────────────────────────────
//
// These are ported verbatim from the ClickHouse backend so both paths share
// the same SQL-injection-safe interval-to-canonical-string logic.

/// Translate a human interval string into a canonical Postgres/TimescaleDB
/// interval token that is safe to interpolate into SQL (it is never user
/// bytes — only our own controlled keyword strings survive).
///
/// Returns `"1 hour"` for any unrecognised input.
fn translate_bucket_interval_pg(interval: &str) -> String {
    const DEFAULT: &str = "1 hour";
    let trimmed = interval.trim();

    let mut parts = trimmed.split_whitespace();
    let (count_str, unit_raw): (String, String) = match (parts.next(), parts.next()) {
        (Some(count), Some(unit)) => {
            if parts.next().is_some() {
                return DEFAULT.to_string();
            }
            (count.to_string(), unit.to_string())
        }
        (Some(single), None) => {
            let split = single
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(single.len());
            if split == 0 || split == single.len() {
                return DEFAULT.to_string();
            }
            (single[..split].to_string(), single[split..].to_string())
        }
        _ => return DEFAULT.to_string(),
    };

    let Ok(count) = count_str.parse::<u32>() else {
        return DEFAULT.to_string();
    };
    if count == 0 || count > 100_000 {
        return DEFAULT.to_string();
    }

    let unit = match unit_raw.to_ascii_lowercase().as_str() {
        "second" | "seconds" | "sec" | "secs" | "s" => "second",
        "minute" | "minutes" | "min" | "mins" | "m" => "minute",
        "hour" | "hours" | "hr" | "hrs" | "h" => "hour",
        "day" | "days" | "d" => "day",
        "week" | "weeks" | "w" => "week",
        _ => return DEFAULT.to_string(),
    };
    format!("{count} {unit}")
}

/// Derive the bucket width in whole seconds from a canonical interval string
/// produced by [`translate_bucket_interval_pg`].
fn interval_seconds_pg(interval: &str) -> i64 {
    const HOUR: i64 = 3600;
    let mut parts = interval.split_whitespace();
    let Some(count) = parts.next().and_then(|c| c.parse::<i64>().ok()) else {
        return HOUR;
    };
    let unit_secs = match parts.next() {
        Some("second") => 1,
        Some("minute") => 60,
        Some("hour") => HOUR,
        Some("day") => 86_400,
        Some("week") => 604_800,
        _ => return HOUR,
    };
    (count * unit_secs).max(1)
}

/// Validate that a metric label key contains only safe characters.
/// Mirrors the allowlist in `temps-metrics::validate_metric_name`. A bad key is
/// a client error (`Validation` → HTTP 400), not a server error.
fn validate_label_key(key: &str) -> Result<(), OtelError> {
    if key.is_empty() {
        return Err(OtelError::Validation {
            message: "label key is empty (only [a-zA-Z0-9_.:-] allowed)".to_string(),
        });
    }
    for ch in key.chars() {
        if !matches!(ch, 'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '.' | '-' | ':') {
            return Err(OtelError::Validation {
                message: format!(
                    "label key '{key}' is outside the allowed character set [a-zA-Z0-9_.:-]"
                ),
            });
        }
    }
    Ok(())
}

/// TimescaleDB-backed OTel storage.
pub struct TimescaleDbStorage {
    db: Arc<DatabaseConnection>,
    s3_client: Option<Arc<S3LogArchiver>>,
    /// Kept on the struct for API/config compatibility with callers and the
    /// retention task spawned by `temps-otel/plugin.rs`. The actual
    /// retention is enforced by the native TimescaleDB
    /// `add_retention_policy(...)` registered in
    /// `m20260225_000001_create_otel_tables`, so this value isn't read
    /// inside the storage layer anymore — see `apply_retention()` for the
    /// rationale.
    #[allow(dead_code)]
    retention_days: u32,
    /// Per-project storage quota. `None` disables quota enforcement: ingest
    /// never runs the per-project usage estimate (see `get_storage_quota`).
    quota_bytes_per_project: Option<u64>,
}

/// S3 log archiver configuration.
pub struct S3LogArchiver {
    client: aws_sdk_s3::Client,
    bucket: String,
    prefix: String,
}

impl S3LogArchiver {
    pub async fn new(
        region: &str,
        endpoint: Option<&str>,
        access_key: &str,
        secret_key: &str,
        bucket: String,
        prefix: String,
    ) -> Result<Self, OtelError> {
        let credentials =
            aws_sdk_s3::config::Credentials::new(access_key, secret_key, None, None, "temps-otel");
        let creds_provider = aws_sdk_s3::config::SharedCredentialsProvider::new(credentials);
        let region_provider = aws_config::meta::region::RegionProviderChain::first_try(
            aws_sdk_s3::config::Region::new(region.to_string()),
        );

        let mut config_builder = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(region_provider)
            .credentials_provider(creds_provider);
        if let Some(ep) = endpoint {
            config_builder = config_builder.endpoint_url(ep);
        }
        let config = config_builder.load().await;
        let mut s3_config_builder = aws_sdk_s3::config::Builder::from(&config);
        if endpoint.is_some() {
            s3_config_builder = s3_config_builder.force_path_style(true);
        }
        let client = aws_sdk_s3::Client::from_conf(s3_config_builder.build());

        Ok(Self {
            client,
            bucket,
            prefix,
        })
    }

    async fn upload_ndjson(&self, project_id: i32, records: &[LogRecord]) -> Result<(), OtelError> {
        if records.is_empty() {
            return Ok(());
        }

        let now = Utc::now();
        let key = format!(
            "{}/project_{}/{}/{}.ndjson.gz",
            self.prefix,
            project_id,
            now.format("%Y/%m/%d/%H"),
            uuid::Uuid::new_v4()
        );

        let mut ndjson = String::new();
        for record in records {
            let line = serde_json::to_string(record).map_err(|e| OtelError::S3 {
                project_id,
                reason: format!("Failed to serialize log record: {}", e),
            })?;
            ndjson.push_str(&line);
            ndjson.push('\n');
        }

        // Gzip compress the NDJSON
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(ndjson.as_bytes())
            .map_err(|e| OtelError::S3 {
                project_id,
                reason: format!("Failed to compress log data: {}", e),
            })?;
        let compressed = encoder.finish().map_err(|e| OtelError::S3 {
            project_id,
            reason: format!("Failed to finish compression: {}", e),
        })?;

        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(&key)
            .body(compressed.into())
            .content_type("application/x-ndjson")
            .content_encoding("gzip")
            .send()
            .await
            .map_err(|e| OtelError::S3 {
                project_id,
                reason: format!("S3 upload failed for key {}: {}", key, e),
            })?;

        debug!(
            project_id,
            key,
            records = records.len(),
            "Archived log records to S3"
        );
        Ok(())
    }
}

impl TimescaleDbStorage {
    pub fn new(db: Arc<DatabaseConnection>, s3_client: Option<Arc<S3LogArchiver>>) -> Self {
        Self {
            db,
            s3_client,
            retention_days: 7,
            quota_bytes_per_project: None,
        }
    }

    /// Create a new storage backend with custom retention and quota settings.
    pub fn with_config(
        db: Arc<DatabaseConnection>,
        s3_client: Option<Arc<S3LogArchiver>>,
        retention_days: u32,
        quota_bytes_per_project: Option<u64>,
    ) -> Self {
        Self {
            db,
            s3_client,
            retention_days,
            quota_bytes_per_project,
        }
    }

    /// Execute a batch insert using raw SQL with parameter binding.
    ///
    /// Writes all 31 columns (17 legacy + 14 full-fidelity added by
    /// m20260629_000001_otel_metrics_full_fidelity).  The 14 new columns are
    /// all nullable so the INSERT is safe on instances that have not yet run
    /// the migration — Postgres will error with "column does not exist" but
    /// the migration is always applied before ingest in production.
    async fn batch_insert_metrics(&self, points: &[MetricPoint]) -> StorageResult<u64> {
        if points.is_empty() {
            return Ok(0);
        }

        // 31 columns total (17 legacy + 14 new full-fidelity columns).
        const COLS_PER_ROW: u32 = 31;

        let mut sql = String::from(
            "INSERT INTO otel_metrics (
                project_id, deployment_id, service_name, service_version,
                deployment_environment, metric_name, metric_type, unit,
                timestamp, value, histogram_count, histogram_sum,
                histogram_min, histogram_max, histogram_bounds,
                histogram_bucket_counts, attributes,
                start_time, temporality, is_monotonic, flags, description,
                exp_scale, exp_zero_count, exp_zero_threshold,
                exp_positive_offset, exp_positive_counts,
                exp_negative_offset, exp_negative_counts,
                summary_quantiles, exemplars
            ) VALUES ",
        );

        let mut values: Vec<sea_orm::Value> = Vec::new();
        let mut param_idx = 1u32;

        for (i, p) in points.iter().enumerate() {
            if i > 0 {
                sql.push_str(", ");
            }
            // Generate $1, $2, … $31 placeholders for this row.
            let placeholders: Vec<String> = (param_idx..param_idx + COLS_PER_ROW)
                .map(|n| format!("${n}"))
                .collect();
            sql.push('(');
            sql.push_str(&placeholders.join(", "));
            sql.push(')');
            param_idx += COLS_PER_ROW;

            // ── Serialise JSONB fields (never .unwrap() — fall back to null) ──

            let attrs_json = serde_json::to_value(&p.attributes).unwrap_or_default();

            let bounds_json = p
                .histogram_bounds
                .as_ref()
                .and_then(|b| serde_json::to_value(b).ok());
            let bucket_counts_json = p
                .histogram_bucket_counts
                .as_ref()
                .and_then(|c| serde_json::to_value(c).ok());

            // Exponential histogram bucket-count arrays (Vec<u64> → JSON).
            let exp_positive_counts_json = p
                .exp_positive_counts
                .as_ref()
                .and_then(|v| serde_json::to_value(v).ok());
            let exp_negative_counts_json = p
                .exp_negative_counts
                .as_ref()
                .and_then(|v| serde_json::to_value(v).ok());

            // Summary quantiles: Vec<(f64, f64)> → [[q, v], …].
            let summary_quantiles_json = p.summary_quantiles.as_ref().and_then(|pairs| {
                let arr: Vec<serde_json::Value> = pairs
                    .iter()
                    .map(|(q, v)| serde_json::json!([q, v]))
                    .collect();
                serde_json::to_value(arr).ok()
            });

            // Exemplars: serialise as array of objects.
            let exemplars_json = if p.exemplars.is_empty() {
                None
            } else {
                serde_json::to_value(&p.exemplars).ok()
            };

            // flags: stored as i32 (Postgres INTEGER).
            let flags_i32 = p.flags as i32;
            // exp_zero_count: u64 → i64 (Postgres BIGINT).
            let exp_zero_count_i64 = p.exp_zero_count.map(|v| v as i64);

            values.extend_from_slice(&[
                // Legacy 17 columns
                p.project_id.into(),
                p.deployment_id.into(),
                p.resource.service_name.clone().into(),
                p.resource.service_version.clone().into(),
                p.resource.deployment_environment.clone().into(),
                p.metric_name.clone().into(),
                p.metric_type.to_string().into(),
                p.unit.clone().into(),
                p.timestamp.into(),
                p.value.into(),
                p.histogram_count.map(|c| c as i64).into(),
                p.histogram_sum.into(),
                p.histogram_min.into(),
                p.histogram_max.into(),
                bounds_json.into(),
                bucket_counts_json.into(),
                attrs_json.into(),
                // New 14 full-fidelity columns
                p.start_time.into(),
                p.temporality.map(|t| t.to_string()).into(),
                p.is_monotonic.into(),
                flags_i32.into(),
                p.description.clone().into(),
                p.exp_scale.into(),
                exp_zero_count_i64.into(),
                p.exp_zero_threshold.into(),
                p.exp_positive_offset.into(),
                exp_positive_counts_json.into(),
                p.exp_negative_offset.into(),
                exp_negative_counts_json.into(),
                summary_quantiles_json.into(),
                exemplars_json.into(),
            ]);
        }

        let result = self
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                values,
            ))
            .await?;

        Ok(result.rows_affected())
    }

    async fn batch_insert_spans(&self, spans: &[SpanRecord]) -> StorageResult<u64> {
        if spans.is_empty() {
            return Ok(0);
        }

        let mut sql = String::from(
            "INSERT INTO otel_spans (
                project_id, deployment_id, service_name, service_version,
                deployment_environment, trace_id, span_id, parent_span_id,
                name, kind, start_time, end_time, duration_ms,
                status_code, status_message, attributes, events
            ) VALUES ",
        );

        let mut values: Vec<sea_orm::Value> = Vec::new();
        let mut param_idx = 1u32;

        for (i, s) in spans.iter().enumerate() {
            if i > 0 {
                sql.push_str(", ");
            }
            sql.push_str(&format!(
                "(${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${})",
                param_idx, param_idx + 1, param_idx + 2, param_idx + 3,
                param_idx + 4, param_idx + 5, param_idx + 6, param_idx + 7,
                param_idx + 8, param_idx + 9, param_idx + 10, param_idx + 11,
                param_idx + 12, param_idx + 13, param_idx + 14, param_idx + 15,
                param_idx + 16
            ));
            param_idx += 17;

            let attrs_json = serde_json::to_value(&s.attributes).unwrap_or_default();
            let events_json = serde_json::to_value(&s.events).unwrap_or_default();

            values.extend_from_slice(&[
                s.project_id.into(),
                s.deployment_id.into(),
                s.resource.service_name.clone().into(),
                s.resource.service_version.clone().into(),
                s.resource.deployment_environment.clone().into(),
                s.trace_id.clone().into(),
                s.span_id.clone().into(),
                s.parent_span_id.clone().into(),
                s.name.clone().into(),
                s.kind.to_string().into(),
                s.start_time.into(),
                s.end_time.into(),
                s.duration_ms.into(),
                s.status_code.to_string().into(),
                s.status_message.clone().into(),
                attrs_json.into(),
                events_json.into(),
            ]);
        }

        let result = self
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                values,
            ))
            .await?;

        Ok(result.rows_affected())
    }

    /// Maintain the `otel_trace_summaries` pre-aggregated table from a batch of
    /// spans.
    ///
    /// The traces *list* view reads from this table instead of running
    /// `GROUP BY trace_id` over the spans hypertable on every request (which
    /// sorts on a computed aggregate and can't use an index — the scaling wall
    /// at millions of traces). Here we fold the batch into one delta per
    /// distinct `(project_id, trace_id)` in Rust, then issue a single
    /// multi-row `INSERT … ON CONFLICT DO UPDATE` that accumulates the
    /// running aggregates.
    ///
    /// **Correct under late-arriving spans.** Spans dribble in across time with
    /// no "trace complete" signal, so the upsert is purely accumulative:
    /// `span_count`/`error_count` add, `start_time` takes the LEAST, and
    /// `duration_ms` takes the GREATEST. The root span (parent_span_id IS NULL)
    /// owns `root_span_name`/`service_name`/`kind`/`deployment_environment`/
    /// `deployment_id`; once a root has been recorded (`has_root = true`) a
    /// later non-root span never overwrites those fields. If the root arrives
    /// after some children, it claims them on its batch.
    ///
    /// This runs on the ingest hot path but adds only one statement per batch
    /// (rows = distinct traces in the batch, not spans), against a small, hot,
    /// index-cached table. Callers treat a failure here as non-fatal: the spans
    /// are already durably stored, so a summary hiccup must not fail ingest.
    async fn upsert_trace_summaries(&self, spans: &[SpanRecord]) -> StorageResult<u64> {
        if spans.is_empty() {
            return Ok(0);
        }

        // Fold the batch into one delta per distinct trace (pure, unit-tested).
        let deltas = fold_trace_deltas(spans);

        // Build one multi-row INSERT … ON CONFLICT DO UPDATE.
        //
        // The root fields are coalesced on conflict so they're only set when
        // this batch carries a root (EXCLUDED.has_root). Once a row has a root,
        // a rootless later batch (has_root = false) keeps the stored values.
        let mut sql = String::from(
            "INSERT INTO otel_trace_summaries (
                project_id, trace_id, root_span_name, service_name, kind,
                deployment_environment, deployment_id, start_time, duration_ms,
                span_count, error_count, has_root, last_seen
            ) VALUES ",
        );

        let mut values: Vec<sea_orm::Value> = Vec::new();
        let mut param_idx = 1u32;
        for (i, d) in deltas.iter().enumerate() {
            if i > 0 {
                sql.push_str(", ");
            }
            // 12 bound params per row; last_seen uses now() in SQL.
            sql.push_str(&format!(
                "(${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, now())",
                param_idx,
                param_idx + 1,
                param_idx + 2,
                param_idx + 3,
                param_idx + 4,
                param_idx + 5,
                param_idx + 6,
                param_idx + 7,
                param_idx + 8,
                param_idx + 9,
                param_idx + 10,
                param_idx + 11,
            ));
            param_idx += 12;

            values.extend_from_slice(&[
                d.project_id.into(),
                d.trace_id.clone().into(),
                // Empty-string defaults for root fields when this batch has no
                // root yet; they won't overwrite a stored root because the
                // ON CONFLICT clause guards on EXCLUDED.has_root.
                d.root_span_name.clone().unwrap_or_default().into(),
                d.root_service_name.clone().unwrap_or_default().into(),
                d.root_kind
                    .clone()
                    .unwrap_or_else(|| "INTERNAL".to_string())
                    .into(),
                d.root_env.clone().into(),
                d.root_deployment_id.into(),
                d.start_time.into(),
                d.max_duration_ms.into(),
                d.span_count.into(),
                d.error_count.into(),
                d.has_root.into(),
            ]);
        }

        sql.push_str(
            " ON CONFLICT (project_id, trace_id) DO UPDATE SET
                span_count  = otel_trace_summaries.span_count + EXCLUDED.span_count,
                error_count = otel_trace_summaries.error_count + EXCLUDED.error_count,
                start_time  = LEAST(otel_trace_summaries.start_time, EXCLUDED.start_time),
                duration_ms = GREATEST(otel_trace_summaries.duration_ms, EXCLUDED.duration_ms),
                last_seen   = now(),
                -- Root identity: adopt this batch's root only if we don't have
                -- one yet AND this batch brought one. Otherwise keep stored.
                has_root       = otel_trace_summaries.has_root OR EXCLUDED.has_root,
                root_span_name = CASE WHEN NOT otel_trace_summaries.has_root AND EXCLUDED.has_root
                                      THEN EXCLUDED.root_span_name ELSE otel_trace_summaries.root_span_name END,
                service_name   = CASE WHEN NOT otel_trace_summaries.has_root AND EXCLUDED.has_root
                                      THEN EXCLUDED.service_name ELSE otel_trace_summaries.service_name END,
                kind           = CASE WHEN NOT otel_trace_summaries.has_root AND EXCLUDED.has_root
                                      THEN EXCLUDED.kind ELSE otel_trace_summaries.kind END,
                deployment_environment = CASE WHEN NOT otel_trace_summaries.has_root AND EXCLUDED.has_root
                                      THEN EXCLUDED.deployment_environment ELSE otel_trace_summaries.deployment_environment END,
                deployment_id  = CASE WHEN NOT otel_trace_summaries.has_root AND EXCLUDED.has_root
                                      THEN EXCLUDED.deployment_id ELSE otel_trace_summaries.deployment_id END",
        );

        let result = self
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                values,
            ))
            .await?;

        Ok(result.rows_affected())
    }

    async fn batch_insert_logs(&self, records: &[LogRecord]) -> StorageResult<u64> {
        if records.is_empty() {
            return Ok(0);
        }

        let mut sql = String::from(
            "INSERT INTO otel_log_events (
                project_id, deployment_id, service_name, service_version,
                deployment_environment, timestamp, observed_timestamp,
                severity, severity_text, body, trace_id, span_id, attributes
            ) VALUES ",
        );

        let mut values: Vec<sea_orm::Value> = Vec::new();
        let mut param_idx = 1u32;

        for (i, r) in records.iter().enumerate() {
            if i > 0 {
                sql.push_str(", ");
            }
            sql.push_str(&format!(
                "(${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${})",
                param_idx,
                param_idx + 1,
                param_idx + 2,
                param_idx + 3,
                param_idx + 4,
                param_idx + 5,
                param_idx + 6,
                param_idx + 7,
                param_idx + 8,
                param_idx + 9,
                param_idx + 10,
                param_idx + 11,
                param_idx + 12
            ));
            param_idx += 13;

            let attrs_json = serde_json::to_value(&r.attributes).unwrap_or_default();

            values.extend_from_slice(&[
                r.project_id.into(),
                r.deployment_id.into(),
                r.resource.service_name.clone().into(),
                r.resource.service_version.clone().into(),
                r.resource.deployment_environment.clone().into(),
                r.timestamp.into(),
                r.observed_timestamp.into(),
                r.severity.to_string().into(),
                r.severity_text.clone().into(),
                r.body.clone().into(),
                r.trace_id.clone().into(),
                r.span_id.clone().into(),
                attrs_json.into(),
            ]);
        }

        let result = self
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                values,
            ))
            .await?;

        Ok(result.rows_affected())
    }
}

// ── Query result structs ────────────────────────────────────────────

/// Row returned by the scalar metric aggregation query.
#[derive(Debug, FromQueryResult)]
struct MetricBucketRow {
    bucket: DateTime<Utc>,
    avg_value: f64,
    min_value: f64,
    max_value: f64,
    count: i64,
    agg_value: f64,
    // Serialised group-by label values as a JSON array (["v1","v2",…]).
    series_values_json: Option<serde_json::Value>,
}

/// Row returned by the histogram aggregation sub-query.
#[derive(Debug)]
struct HistogramBucketRow {
    bucket: DateTime<Utc>,
    series_values_json: Option<serde_json::Value>,
    hcount: i64,
    hsum: f64,
    hmin: Option<f64>,
    hmax: Option<f64>,
    hbounds: Option<serde_json::Value>,
    hbuckets: Option<serde_json::Value>,
}

#[derive(Debug, FromQueryResult)]
struct MetricNameRow {
    metric_name: String,
}

#[derive(Debug, FromQueryResult)]
struct LabelKeyRow {
    label_key: String,
}

#[derive(Debug, FromQueryResult)]
struct LabelValueRow {
    label_value: String,
}

#[derive(Debug, FromQueryResult)]
struct BaselineRow {
    hour_of_day: i32,
    day_of_week: i32,
    avg_value: f64,
    stddev_value: f64,
    sample_count: i64,
}

#[derive(Debug, FromQueryResult)]
struct MinuteAggregateRow {
    bucket: DateTime<Utc>,
    avg_value: f64,
    count: i64,
}

#[derive(Debug, FromQueryResult)]
struct DeployRow {
    id: i32,
    project_id: i32,
    environment_id: Option<i32>,
    created_at: DateTime<Utc>,
}

#[derive(Debug, FromQueryResult)]
#[allow(dead_code)]
struct QuotaRow {
    total_bytes: i64,
}

#[derive(Debug, FromQueryResult)]
struct P95Row {
    p95: f64,
}

// ── OtelStorage implementation ──────────────────────────────────────

#[async_trait]
impl OtelStorage for TimescaleDbStorage {
    async fn store_metrics(&self, points: Vec<MetricPoint>) -> StorageResult<u64> {
        self.batch_insert_metrics(&points).await
    }

    async fn store_spans(&self, spans: Vec<SpanRecord>) -> StorageResult<u64> {
        let stored = self.batch_insert_spans(&spans).await?;

        // Maintain the pre-aggregated trace-summary table that backs the list
        // view. Fail-soft: the spans are already durably written, so a summary
        // upsert error must not fail ingest — the worst case is a trace that's
        // momentarily missing or stale in the list until its next span arrives
        // (or until a backfill/reconcile runs). Log and continue.
        if let Err(e) = self.upsert_trace_summaries(&spans).await {
            warn!(
                error = %e,
                span_count = spans.len(),
                "failed to upsert otel_trace_summaries; spans stored, summary will lag"
            );
        }

        Ok(stored)
    }

    async fn store_logs(&self, records: Vec<LogRecord>) -> StorageResult<u64> {
        self.batch_insert_logs(&records).await
    }

    async fn archive_logs(&self, records: Vec<LogRecord>) -> StorageResult<u64> {
        let count = records.len() as u64;
        match &self.s3_client {
            Some(archiver) => {
                // Group by project for separate S3 paths
                let mut by_project: std::collections::HashMap<i32, Vec<LogRecord>> =
                    std::collections::HashMap::new();
                for record in records {
                    by_project
                        .entry(record.project_id)
                        .or_default()
                        .push(record);
                }
                for (project_id, project_records) in by_project {
                    if let Err(e) = archiver.upload_ndjson(project_id, &project_records).await {
                        error!(
                            project_id,
                            error = %e,
                            records = project_records.len(),
                            "Failed to archive logs to S3, data will be lost"
                        );
                    }
                }
                Ok(count)
            }
            None => {
                warn!(
                    "S3 archiver not configured, skipping log archival for {} records",
                    count
                );
                Ok(0)
            }
        }
    }

    /// Query bucketed metric aggregates — full-fidelity port of the ClickHouse
    /// implementation.
    ///
    /// Two separate SQL queries are issued:
    ///
    /// 1. **Scalar query** — aggregates the `value` column into avg/min/max/count
    ///    plus the requested [`MetricAggregation`], grouped by time bucket and
    ///    optional label keys.  `value IS NOT NULL` filters out histogram rows.
    ///
    /// 2. **Histogram query** — issued only when the metric carries histogram
    ///    data.  Uses TimescaleDB's `last(col, timestamp)` for cumulative series
    ///    (snapshot semantics) and `sum` for delta/unspecified.  Element-wise
    ///    summation of `histogram_bucket_counts` across series is done via a CTE
    ///    that unnests the JSONB array, sums by ordinal position, and re-aggregates
    ///    into a JSONB array.
    ///
    /// Results are matched by `(bucket, series_values_json)` and merged into the
    /// returned [`MetricBucket`] slice.
    async fn query_metrics(&self, query: MetricQuery) -> StorageResult<Vec<MetricBucket>> {
        // ── Validate label keys (group_by + label_filters) ──────────
        for key in query
            .group_by
            .iter()
            .chain(query.label_filters.iter().map(|(k, _)| k))
        {
            validate_label_key(key)?;
        }

        let interval =
            translate_bucket_interval_pg(query.bucket_interval.as_deref().unwrap_or("1 hour"));
        let secs = interval_seconds_pg(&interval);
        let limit = query.limit.unwrap_or(1000).min(10_000);

        // ── Build shared WHERE clause ────────────────────────────────
        //
        // We build two WHERE clauses from the same filters (scalar + histogram
        // queries share project/name/svc/env/time filters, but differ in the
        // scalar query adding `value IS NOT NULL` and the histogram query adding
        // `histogram_bucket_counts IS NOT NULL`).

        let (base_where_sql, base_values, base_param_next) = build_metrics_where(&query);

        // ── Scalar aggregation expression ────────────────────────────
        let agg_expr = match query.aggregation {
            MetricAggregation::Avg => "avg(value)".to_string(),
            MetricAggregation::Sum => "sum(value)".to_string(),
            MetricAggregation::Min => "min(value)".to_string(),
            MetricAggregation::Max => "max(value)".to_string(),
            MetricAggregation::Count => "count(*)::float8".to_string(),
            MetricAggregation::RatePerSec => {
                // Temporality-aware: delta → sum/window, cumulative → (max-min)/window.
                format!(
                    "CASE WHEN bool_or(temporality = 'delta') \
                         THEN sum(value) \
                         ELSE max(value) - min(value) \
                     END / {secs}.0"
                )
            }
            MetricAggregation::Quantile(q) => {
                let qc = q.clamp(0.0, 1.0);
                format!("percentile_cont({qc}) WITHIN GROUP (ORDER BY value)")
            }
        };

        // ── Param layout (positional $N must match the values vector) ──
        //
        // base_values occupy $1..=$(base_param_next-1) (from build_metrics_where,
        // used inside the inner WHERE). Then: interval, then one $N per group_by
        // key (for the `attributes->>$N` projection), then LIMIT.
        let interval_param = base_param_next;
        let first_group_key_param = base_param_next + 1;
        let limit_param = first_group_key_param + query.group_by.len() as u32;

        let group_by_cols: Vec<String> = (0..query.group_by.len())
            .map(|i| format!("gb_{i}"))
            .collect();
        let group_by_extra = if group_by_cols.is_empty() {
            String::new()
        } else {
            format!(", {}", group_by_cols.join(", "))
        };
        // Series key as a JSON array of the grouped label values (outer level,
        // where gb_N are GROUP BY columns).
        let series_json_expr = if group_by_cols.is_empty() {
            "NULL::jsonb AS series_values_json".to_string()
        } else {
            format!(
                "jsonb_build_array({}) AS series_values_json",
                group_by_cols.join(", ")
            )
        };

        // ── Scalar query ─────────────────────────────────────────────
        //
        // Compute `time_bucket()` + project the raw columns in an INNER subquery
        // (no aggregation), then aggregate in the OUTER query — per the codebase
        // rule "never cast time_bucket() in the same level as GROUP BY". The
        // inner projects `value`/`temporality` (consumed by the aggregate exprs)
        // plus one `gb_N` column per group_by key.
        //
        // NOTE: we deliberately do NOT route this read to the `otel_metrics_1min`/
        // `1hr` continuous aggregates. Those rollups are refreshed on a lag and
        // (in this setup) do not return un-materialized recent data, so reading
        // them would silently DROP the most recent buckets from charts. The raw
        // hypertable + the `(project_id, metric_name, service_name, timestamp)`
        // index + chunk pruning keep this correct AND fast.
        let mut inner_parts = vec![
            format!("time_bucket(${interval_param}::interval, timestamp) AS bucket"),
            "value".to_string(),
            "temporality".to_string(),
        ];
        for (i, _) in query.group_by.iter().enumerate() {
            inner_parts.push(format!(
                "attributes->>${} AS gb_{i}",
                first_group_key_param + i as u32
            ));
        }

        let scalar_where = format!("{base_where_sql} AND value IS NOT NULL");
        let inner_select = format!(
            "SELECT {} FROM otel_metrics WHERE {scalar_where}",
            inner_parts.join(", ")
        );

        let scalar_sql = format!(
            "SELECT bucket, avg(value) AS avg_value, min(value) AS min_value, \
                    max(value) AS max_value, count(*) AS count, {agg_expr} AS agg_value, \
                    {series_json_expr} \
             FROM ({inner_select}) _inner \
             GROUP BY bucket{group_by_extra} \
             ORDER BY bucket ASC \
             LIMIT ${limit_param}"
        );

        // Bind in $ order: base_values, interval, group keys, LIMIT.
        let mut scalar_values: Vec<sea_orm::Value> = base_values.clone();
        scalar_values.push(interval.clone().into());
        for k in &query.group_by {
            scalar_values.push(k.clone().into());
        }
        scalar_values.push((limit as i64).into());

        let scalar_results = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &scalar_sql,
                scalar_values,
            ))
            .await?;

        // Parse scalar rows.
        let scalar_rows: Vec<MetricBucketRow> = scalar_results
            .iter()
            .filter_map(|row| {
                Some(MetricBucketRow {
                    bucket: row.try_get("", "bucket").ok()?,
                    avg_value: row.try_get("", "avg_value").ok()?,
                    min_value: row.try_get("", "min_value").ok()?,
                    max_value: row.try_get("", "max_value").ok()?,
                    count: row.try_get("", "count").ok()?,
                    agg_value: row.try_get("", "agg_value").ok()?,
                    series_values_json: row.try_get("", "series_values_json").ok().flatten(),
                })
            })
            .collect();

        // NOTE: do NOT early-return when scalar_rows is empty — histogram-only
        // metrics (data points with no scalar `value`) produce no scalar rows but
        // DO have histogram buckets, so the histogram query below must still run.

        // ── Histogram query ──────────────────────────────────────────
        //
        // Mirrors the CH inner/outer structure:
        // - Inner: collapse each (bucket, series) by temporality
        // - Outer: sum across series, element-wise sum bucket counts
        //
        // Element-wise JSONB array summation is done via a CTE that unnests
        // both arrays together, sums values by ordinal, and re-aggregates.

        let hist_where = format!("{base_where_sql} AND histogram_bucket_counts IS NOT NULL");

        // Group-by label projections for the histogram CTE: one `attributes->>$N`
        // per key, carried through as gb_0, gb_1, …. Same param positions as the
        // scalar query (base_values, then interval, then group keys, then LIMIT).
        let hist_gb_select_csv = if query.group_by.is_empty() {
            String::new()
        } else {
            let parts: Vec<String> = query
                .group_by
                .iter()
                .enumerate()
                .map(|(i, _)| {
                    format!(
                        "attributes->>${} AS gb_{i}",
                        first_group_key_param + i as u32
                    )
                })
                .collect();
            format!("{}, ", parts.join(", "))
        };
        // `, gb_0, gb_1` suffix reused in every GROUP BY / USING / SELECT.
        let hist_gb_csv = if group_by_cols.is_empty() {
            String::new()
        } else {
            format!(", {}", group_by_cols.join(", "))
        };
        // Qualify with `s.` — the final join keeps both `s.gb_N` and `c.gb_N`
        // in scope (an `ON` join doesn't merge the columns the way `USING`
        // does), so a bare `gb_N` here would be ambiguous.
        let hist_series_json_expr = if group_by_cols.is_empty() {
            "NULL::jsonb".to_string()
        } else {
            let qualified: Vec<String> = group_by_cols.iter().map(|c| format!("s.{c}")).collect();
            format!("jsonb_build_array({})", qualified.join(", "))
        };
        // NULL-safe join between the `scalars` and `counts_arr` CTEs. A group
        // label that is absent on some series is NULL on BOTH sides; a plain
        // `USING`/`=` equi-join would drop those rows (`NULL = NULL` is not true),
        // so compare the group columns with `IS NOT DISTINCT FROM`. (`bucket` is
        // never NULL, so it stays an equi-join.)
        let hist_join = if group_by_cols.is_empty() {
            "USING (bucket)".to_string()
        } else {
            let mut conds = vec!["s.bucket = c.bucket".to_string()];
            for col in &group_by_cols {
                conds.push(format!("s.{col} IS NOT DISTINCT FROM c.{col}"));
            }
            format!("ON {}", conds.join(" AND "))
        };

        // Temporality-aware histogram reconstruction (vanilla Postgres, no
        // TimescaleDB-specific aggregates other than time_bucket):
        //  1. `contributing` — per data point: its bucket + the recency rank
        //     (latest first) within (bucket, attribute-set).
        //  2. `picked` — keep ALL delta/unspecified rows but only the LATEST
        //     snapshot of each cumulative series (a cumulative bucket-count is a
        //     running total; only the newest snapshot should contribute).
        //  3. `counts` — element-wise sum the bucket-count arrays across picked
        //     rows, per bucket index, via WITH ORDINALITY so `idx` is the position
        //     WITHIN each array (a global row_number would break alignment).
        //  4. `scalars` — sum count/sum, min/max, and pick one bounds array.
        // Joined back per (bucket, group keys).
        let hist_sql = format!(
            "WITH contributing AS ( \
                 SELECT \
                     time_bucket(${interval_param}::interval, timestamp) AS bucket, \
                     {hist_gb_select_csv}\
                     histogram_bucket_counts, histogram_count, histogram_sum, \
                     histogram_min, histogram_max, histogram_bounds, timestamp, \
                     (COALESCE(temporality, '') = 'cumulative') AS is_cum, \
                     row_number() OVER ( \
                         PARTITION BY time_bucket(${interval_param}::interval, timestamp), md5(attributes::text) \
                         ORDER BY timestamp DESC \
                     ) AS rn \
                 FROM otel_metrics WHERE {hist_where} \
             ), \
             picked AS (SELECT * FROM contributing WHERE NOT is_cum OR rn = 1), \
             counts AS ( \
                 SELECT bucket{hist_gb_csv}, idx, sum(v::numeric)::bigint AS cnt \
                 FROM picked, LATERAL jsonb_array_elements_text(histogram_bucket_counts) \
                     WITH ORDINALITY AS e(v, idx) \
                 GROUP BY bucket{hist_gb_csv}, idx \
             ), \
             counts_arr AS ( \
                 SELECT bucket{hist_gb_csv}, jsonb_agg(cnt ORDER BY idx) AS hbuckets \
                 FROM counts GROUP BY bucket{hist_gb_csv} \
             ), \
             scalars AS ( \
                 SELECT bucket{hist_gb_csv}, \
                     sum(histogram_count)::bigint AS hcount, sum(histogram_sum) AS hsum, \
                     min(histogram_min) AS hmin, max(histogram_max) AS hmax, \
                     (array_agg(histogram_bounds ORDER BY timestamp DESC))[1] AS hbounds \
                 FROM picked GROUP BY bucket{hist_gb_csv} \
             ) \
             SELECT s.bucket AS bucket, {hist_series_json_expr} AS series_values_json, \
                    s.hcount AS hcount, s.hsum AS hsum, s.hmin AS hmin, s.hmax AS hmax, \
                    s.hbounds AS hbounds, c.hbuckets AS hbuckets \
             FROM scalars s JOIN counts_arr c {hist_join} \
             WHERE s.hbounds IS NOT NULL \
             ORDER BY s.bucket ASC \
             LIMIT ${limit_param}"
        );

        // Same bind order as the scalar query: base_values, interval, group keys, LIMIT.
        let mut hist_values: Vec<sea_orm::Value> = base_values.clone();
        hist_values.push(interval.clone().into());
        for k in &query.group_by {
            hist_values.push(k.clone().into());
        }
        hist_values.push((limit as i64).into());

        let hist_results = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &hist_sql,
                hist_values,
            ))
            .await
            .unwrap_or_else(|e| {
                // Histogram reconstruction is best-effort: on any failure (e.g. a
                // pre-migration schema missing the new columns) degrade to
                // scalar-only results rather than failing the whole query.
                debug!(
                    error = %e,
                    "query_metrics: histogram sub-query failed, returning scalar-only"
                );
                Vec::new()
            });

        // Parse histogram rows.
        let hist_rows: Vec<HistogramBucketRow> = hist_results
            .iter()
            .filter_map(|row| {
                Some(HistogramBucketRow {
                    bucket: row.try_get("", "bucket").ok()?,
                    series_values_json: row.try_get("", "series_values_json").ok().flatten(),
                    hcount: row.try_get("", "hcount").ok()?,
                    hsum: row.try_get("", "hsum").ok()?,
                    hmin: row.try_get("", "hmin").ok().flatten(),
                    hmax: row.try_get("", "hmax").ok().flatten(),
                    hbounds: row.try_get("", "hbounds").ok().flatten(),
                    hbuckets: row.try_get("", "hbuckets").ok().flatten(),
                })
            })
            .collect();

        // Map keyed by (bucket_ms, series_json_string) → (bucket, series_json,
        // summary). The bucket time + series_json are retained so histogram-only
        // metrics (whose data points carry no scalar `value`, so the scalar query
        // yields no rows for them) can still produce result buckets driven by the
        // histogram query alone.
        type HistEntry = (
            chrono::DateTime<chrono::Utc>,
            Option<serde_json::Value>,
            HistogramSummary,
        );
        let mut hist_map: std::collections::HashMap<(i64, String), HistEntry> =
            std::collections::HashMap::new();
        for h in hist_rows {
            let bounds = match parse_jsonb_f64_array(h.hbounds.as_ref()) {
                Some(b) if !b.is_empty() => b,
                _ => continue,
            };
            let bucket_counts = match parse_jsonb_u64_array(h.hbuckets.as_ref()) {
                Some(c) => c,
                None => continue,
            };
            let series_key = series_json_key(&h.series_values_json);
            hist_map.insert(
                (h.bucket.timestamp_millis(), series_key),
                (
                    h.bucket,
                    h.series_values_json.clone(),
                    HistogramSummary {
                        count: h.hcount as u64,
                        sum: h.hsum,
                        min: h.hmin,
                        max: h.hmax,
                        bounds,
                        bucket_counts,
                    },
                ),
            );
        }

        // ── Assemble MetricBucket results ────────────────────────────
        let group_keys = query.group_by.clone();
        let agg = query.aggregation;

        // Reconstruct the (key, value) series pairs from the stored JSON array.
        let derive_series_key =
            |series_json: &Option<serde_json::Value>| -> Option<Vec<(String, String)>> {
                if group_keys.is_empty() {
                    return None;
                }
                let values = series_json
                    .as_ref()
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .map(|v| {
                                v.as_str()
                                    .map(|s| s.to_string())
                                    .unwrap_or_else(|| v.to_string())
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                Some(group_keys.iter().cloned().zip(values).collect())
            };

        let mut buckets: Vec<MetricBucket> = Vec::new();
        let mut covered: std::collections::HashSet<(i64, String)> =
            std::collections::HashSet::new();

        for r in scalar_rows {
            let key = (
                r.bucket.timestamp_millis(),
                series_json_key(&r.series_values_json),
            );
            covered.insert(key.clone());
            let histogram_summary = hist_map.get(&key).map(|(_, _, s)| s.clone());
            let agg_value = r.agg_value;
            let quantiles = match agg.quantile() {
                Some(qq) => vec![(qq, agg_value)],
                None => Vec::new(),
            };
            buckets.push(MetricBucket {
                bucket: r.bucket,
                avg_value: r.avg_value,
                min_value: r.min_value,
                max_value: r.max_value,
                count: r.count,
                value: agg_value,
                quantiles,
                histogram_summary,
                series_key: derive_series_key(&r.series_values_json),
            });
        }

        // Histogram-only metrics: a histogram data point carries no scalar
        // `value`, so the scalar query produced no row for it. Emit those buckets
        // from the histogram summary, deriving the scalar fields from it.
        for (key, (bucket, series_json, summary)) in &hist_map {
            if covered.contains(key) {
                continue;
            }
            let count = summary.count as i64;
            let avg_value = if summary.count > 0 {
                summary.sum / summary.count as f64
            } else {
                0.0
            };
            let min_value = summary.min.unwrap_or(0.0);
            let max_value = summary.max.unwrap_or(0.0);
            let value = match agg {
                MetricAggregation::Sum => summary.sum,
                MetricAggregation::Count => summary.count as f64,
                MetricAggregation::Min => min_value,
                MetricAggregation::Max => max_value,
                // p50/p95/p99 over a histogram: interpolate from the bucket
                // counts rather than returning the mean. Falls back to the mean
                // only if the histogram is empty/malformed.
                MetricAggregation::Quantile(qq) => {
                    histogram_quantile(qq, &summary.bounds, &summary.bucket_counts)
                        .unwrap_or(avg_value)
                }
                MetricAggregation::Avg | MetricAggregation::RatePerSec => avg_value,
            };
            let quantiles = match agg {
                MetricAggregation::Quantile(qq) => vec![(qq, value)],
                _ => Vec::new(),
            };
            buckets.push(MetricBucket {
                bucket: *bucket,
                avg_value,
                min_value,
                max_value,
                count,
                value,
                quantiles,
                histogram_summary: Some(summary.clone()),
                series_key: derive_series_key(series_json),
            });
        }

        // Stable order by bucket time (scalar rows already arrived ordered; the
        // histogram-only additions are merged in).
        buckets.sort_by_key(|b| b.bucket);

        Ok(buckets)
    }

    async fn list_metric_names(&self, project_id: i32) -> StorageResult<Vec<String>> {
        let sql = "SELECT DISTINCT metric_name FROM otel_metrics WHERE project_id = $1 ORDER BY metric_name";
        let rows = MetricNameRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            vec![project_id.into()],
        ))
        .all(self.db.as_ref())
        .await?;
        Ok(rows.into_iter().map(|r| r.metric_name).collect())
    }

    async fn list_metric_label_keys(
        &self,
        project_id: i32,
        metric_name: &str,
        start_time: DateTime<Utc>,
        end_time: DateTime<Utc>,
    ) -> StorageResult<Vec<String>> {
        // Sample the most-recent matching rows (subquery LIMIT) so the
        // distinct-keys scan is bounded no matter how much history the metric
        // has — TimescaleDB chunk exclusion + a per-chunk index make
        // `ORDER BY timestamp DESC LIMIT N` over (project_id, metric_name) cheap.
        // LATERAL jsonb_object_keys unnests each sampled row's attribute keys.
        let sql = "SELECT DISTINCT key AS label_key FROM ( \
                     SELECT attributes FROM otel_metrics \
                     WHERE project_id = $1 AND metric_name = $2 \
                       AND timestamp >= $3 AND timestamp <= $4 \
                       AND attributes IS NOT NULL AND attributes <> '{}'::jsonb \
                     ORDER BY timestamp DESC LIMIT 2000 \
                   ) sub, LATERAL jsonb_object_keys(sub.attributes) AS key \
                   ORDER BY label_key";
        let rows = LabelKeyRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            vec![
                project_id.into(),
                metric_name.into(),
                start_time.into(),
                end_time.into(),
            ],
        ))
        .all(self.db.as_ref())
        .await?;
        Ok(rows.into_iter().map(|r| r.label_key).collect())
    }

    async fn list_metric_label_values(
        &self,
        project_id: i32,
        metric_name: &str,
        label_key: &str,
        start_time: DateTime<Utc>,
        end_time: DateTime<Utc>,
    ) -> StorageResult<Vec<String>> {
        // `attributes ->> $3` extracts the value for the chosen key (NULL when
        // absent). Same bounded recent-sample strategy, then dedup and cap the
        // value list so a high-cardinality label can't return unbounded rows.
        let sql = "SELECT DISTINCT label_value FROM ( \
                     SELECT attributes ->> $3 AS label_value, timestamp \
                     FROM otel_metrics \
                     WHERE project_id = $1 AND metric_name = $2 \
                       AND timestamp >= $4 AND timestamp <= $5 \
                       AND attributes ->> $3 IS NOT NULL \
                     ORDER BY timestamp DESC LIMIT 5000 \
                   ) sub \
                   ORDER BY label_value LIMIT 500";
        let rows = LabelValueRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            vec![
                project_id.into(),
                metric_name.into(),
                label_key.into(),
                start_time.into(),
                end_time.into(),
            ],
        ))
        .all(self.db.as_ref())
        .await?;
        Ok(rows.into_iter().map(|r| r.label_value).collect())
    }

    async fn query_spans(&self, query: TraceQuery) -> StorageResult<Vec<SpanRecord>> {
        let limit = query.limit.unwrap_or(100).min(1000);
        let offset = query.offset.unwrap_or(0);

        let mut where_clauses = vec!["project_id = $1".to_string()];
        let mut values: Vec<sea_orm::Value> = vec![query.project_id.into()];
        let mut param_idx = 2u32;

        if let Some(ref tid) = query.trace_id {
            where_clauses.push(format!("trace_id = ${}", param_idx));
            values.push(tid.clone().into());
            param_idx += 1;
        }
        if let Some(ref svc) = query.service_name {
            where_clauses.push(format!("service_name = ${}", param_idx));
            values.push(svc.clone().into());
            param_idx += 1;
        }
        if let Some(status) = query.status {
            where_clauses.push(format!("status_code = ${}", param_idx));
            values.push(status.to_string().into());
            param_idx += 1;
        }
        if let Some(min_dur) = query.min_duration_ms {
            where_clauses.push(format!("duration_ms >= ${}", param_idx));
            values.push(min_dur.into());
            param_idx += 1;
        }
        if let Some(start) = query.start_time {
            where_clauses.push(format!("start_time >= ${}", param_idx));
            values.push(start.into());
            param_idx += 1;
        }
        if let Some(end) = query.end_time {
            where_clauses.push(format!("start_time <= ${}", param_idx));
            values.push(end.into());
            param_idx += 1;
        }
        if let Some(deployment_id) = query.deployment_id {
            where_clauses.push(format!("deployment_id = ${}", param_idx));
            values.push(deployment_id.into());
            param_idx += 1;
        }
        if let Some(environment_id) = query.environment_id {
            where_clauses.push(format!(
                "deployment_id IN (SELECT id FROM deployments WHERE environment_id = ${})",
                param_idx
            ));
            values.push(environment_id.into());
            param_idx += 1;
        }
        if let Some(ref attrs) = query.attributes {
            for (key, value) in attrs {
                where_clauses.push(format!(
                    "attributes->>${}::text = ${}",
                    param_idx,
                    param_idx + 1
                ));
                values.push(key.clone().into());
                values.push(value.clone().into());
                param_idx += 2;
            }
        }
        if let Some(ref pattern) = query.name_pattern {
            where_clauses.push(format!("name ILIKE ${}", param_idx));
            values.push(format!("%{}%", escape_like_pattern(pattern)).into());
            param_idx += 1;
        }

        let where_sql = where_clauses.join(" AND ");

        let sql = format!(
            r#"
            SELECT project_id, deployment_id, service_name, service_version,
                   deployment_environment, trace_id, span_id, parent_span_id,
                   name, kind, start_time, end_time, duration_ms,
                   status_code, status_message, attributes, events
            FROM otel_spans
            WHERE {where_sql}
            ORDER BY start_time DESC
            LIMIT ${param_idx} OFFSET ${next_param}
            "#,
            next_param = param_idx + 1
        );
        values.push((limit as i64).into());
        values.push((offset as i64).into());

        let results = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                values,
            ))
            .await?;

        let spans = results
            .iter()
            .filter_map(|row| parse_span_row(row).ok())
            .collect();

        Ok(spans)
    }

    /// List traces for the UI. Reads the pre-aggregated `otel_trace_summaries`
    /// table (one indexed row per trace) so duration/start sorts are index
    /// scans instead of a sort-after-aggregate over the spans hypertable —
    /// the scaling wall at millions of traces.
    ///
    /// Falls back to the span-aggregation path only for queries carrying a
    /// span-level filter the summary table can't satisfy (`attributes` or
    /// `name_pattern`); see `query_trace_summaries_from_spans`.
    async fn query_trace_summaries(&self, query: TraceQuery) -> StorageResult<Vec<TraceSummary>> {
        if needs_span_level_filter(&query) {
            return self.query_trace_summaries_from_spans(query).await;
        }
        self.query_trace_summaries_from_table(query).await
    }

    /// Count traces matching the query, for pagination. Mirrors the dispatch in
    /// `query_trace_summaries` so the count matches the listed rows exactly.
    async fn count_traces(&self, query: TraceQuery) -> StorageResult<u64> {
        if needs_span_level_filter(&query) {
            return self.count_traces_from_spans(query).await;
        }
        self.count_traces_from_table(query).await
    }
    async fn get_trace(&self, project_id: i32, trace_id: &str) -> StorageResult<Vec<SpanRecord>> {
        // Two-phase lookup. Phase 1: fetch the trace's time window from
        // otel_trace_summaries (PK lookup, O(1)). Phase 2: scan otel_spans
        // bounded to that window so hypertable chunk exclusion prunes the
        // scan to 1-2 chunks. Without the bound, this query touches every
        // chunk in the retention window — and on compressed chunks (which
        // have no B-tree indexes, and no trace_id segmentby since
        // m20260714_000001) that means decompressing the project's entire
        // history for one trace.
        //
        // The window is padded generously: spans of a trace can start before
        // the summary's recorded start (late/out-of-order batches update it,
        // but a reader can race the update) and child spans can outlive the
        // root. A day of slack is negligible for chunk exclusion (chunks are
        // 1 day) and safe for any realistic trace duration.
        let window = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT start_time, duration_ms FROM otel_trace_summaries
                 WHERE project_id = $1 AND trace_id = $2",
                vec![project_id.into(), trace_id.to_string().into()],
            ))
            .await?
            .and_then(|row| {
                let start: chrono::DateTime<chrono::Utc> = row.try_get("", "start_time").ok()?;
                let duration_ms: f64 = row.try_get("", "duration_ms").unwrap_or(0.0);
                let end = start + chrono::Duration::milliseconds(duration_ms.ceil() as i64);
                Some((
                    start - chrono::Duration::days(1),
                    end + chrono::Duration::days(1),
                ))
            });

        // No summary row means the trace is invisible in every list view (the
        // list reads from otel_trace_summaries), so it can only be requested
        // via a direct link. That happens for (a) an ingest race — the summary
        // upsert in store_spans runs just after the span insert, (b) a
        // transient, fail-soft summary upsert failure, or (c) legacy spans
        // predating the summaries table. (a) and (b) are recent by nature, so
        // fall back to a scan bounded to the last 7 days — the window in which
        // chunks are still uncompressed and carry the trace_id B-tree index.
        // Never scan unbounded: on compressed chunks a trace_id lookup has no
        // index and no segmentby to seek on, so an unbounded fallback would
        // decompress the project's entire history for any unknown trace_id
        // (stale links, cross-project probes, or abuse).
        let (from, to) = window.unwrap_or_else(|| {
            let now = chrono::Utc::now();
            (now - chrono::Duration::days(7), now)
        });

        let sql = r#"
            SELECT project_id, deployment_id, service_name, service_version,
                   deployment_environment, trace_id, span_id, parent_span_id,
                   name, kind, start_time, end_time, duration_ms,
                   status_code, status_message, attributes, events
            FROM otel_spans
            WHERE project_id = $1 AND trace_id = $2
              AND start_time >= $3 AND start_time <= $4
            ORDER BY start_time ASC
        "#;
        let values: Vec<sea_orm::Value> = vec![
            project_id.into(),
            trace_id.to_string().into(),
            from.into(),
            to.into(),
        ];

        let results = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                values,
            ))
            .await?;

        let spans = results
            .iter()
            .filter_map(|row| parse_span_row(row).ok())
            .collect();

        Ok(spans)
    }

    async fn query_genai_trace_summaries(
        &self,
        query: TraceQuery,
    ) -> StorageResult<Vec<GenAiTraceSummary>> {
        let limit = query.limit.unwrap_or(50).min(100);
        let offset = query.offset.unwrap_or(0);

        // Base filter: must have gen_ai.system or gen_ai.provider.name (deprecated → current)
        let mut where_clauses = vec![
            "s.project_id = $1".to_string(),
            "(s.attributes ? 'gen_ai.system' OR s.attributes ? 'gen_ai.provider.name')".to_string(),
        ];
        let mut values: Vec<sea_orm::Value> = vec![query.project_id.into()];
        let mut param_idx = 2u32;

        if let Some(ref svc) = query.service_name {
            where_clauses.push(format!("s.service_name = ${}", param_idx));
            values.push(svc.clone().into());
            param_idx += 1;
        }
        if let Some(start) = query.start_time {
            where_clauses.push(format!("s.start_time >= ${}", param_idx));
            values.push(start.into());
            param_idx += 1;
        }
        if let Some(end) = query.end_time {
            where_clauses.push(format!("s.start_time <= ${}", param_idx));
            values.push(end.into());
            param_idx += 1;
        }
        if let Some(ref attrs) = query.attributes {
            for (key, value) in attrs {
                // Handle deprecated attribute names with COALESCE
                match key.as_str() {
                    "gen_ai.system" => {
                        where_clauses.push(format!(
                            "COALESCE(s.attributes->>'gen_ai.provider.name', s.attributes->>'gen_ai.system') = ${}",
                            param_idx
                        ));
                        values.push(value.clone().into());
                        param_idx += 1;
                    }
                    "gen_ai.usage.input_tokens" => {
                        where_clauses.push(format!(
                            "COALESCE(s.attributes->>'gen_ai.usage.input_tokens', s.attributes->>'gen_ai.usage.prompt_tokens') = ${}",
                            param_idx
                        ));
                        values.push(value.clone().into());
                        param_idx += 1;
                    }
                    _ => {
                        where_clauses.push(format!(
                            "s.attributes->>${}::text = ${}",
                            param_idx,
                            param_idx + 1
                        ));
                        values.push(key.clone().into());
                        values.push(value.clone().into());
                        param_idx += 2;
                    }
                }
            }
        }

        let where_sql = where_clauses.join(" AND ");

        let sql = format!(
            r#"
            SELECT
                s.trace_id,
                (array_agg(s.name ORDER BY
                    CASE WHEN s.parent_span_id IS NULL THEN 0 ELSE 1 END,
                    s.duration_ms DESC
                ))[1] AS root_span_name,
                (array_agg(s.service_name ORDER BY
                    CASE WHEN s.parent_span_id IS NULL THEN 0 ELSE 1 END,
                    s.duration_ms DESC
                ))[1] AS service_name,
                (array_agg(
                    COALESCE(s.attributes->>'gen_ai.provider.name', s.attributes->>'gen_ai.system')
                    ORDER BY s.start_time ASC)
                    FILTER (WHERE COALESCE(s.attributes->>'gen_ai.provider.name', s.attributes->>'gen_ai.system') IS NOT NULL)
                )[1] AS gen_ai_system,
                (array_agg(s.attributes->>'gen_ai.request.model' ORDER BY s.start_time ASC)
                    FILTER (WHERE s.attributes->>'gen_ai.request.model' IS NOT NULL)
                )[1] AS gen_ai_model,
                (array_agg(s.attributes->>'gen_ai.operation.name' ORDER BY s.start_time ASC)
                    FILTER (WHERE s.attributes->>'gen_ai.operation.name' IS NOT NULL)
                )[1] AS gen_ai_operation,
                MIN(s.start_time) AS start_time,
                MAX(s.duration_ms) AS duration_ms,
                COUNT(*)::bigint AS span_count,
                COUNT(*) FILTER (WHERE s.status_code = 'ERROR')::bigint AS error_count,
                (SUM(COALESCE(
                    (s.attributes->>'gen_ai.usage.input_tokens')::bigint,
                    (s.attributes->>'gen_ai.usage.prompt_tokens')::bigint
                )) FILTER (WHERE COALESCE(
                    s.attributes->>'gen_ai.usage.input_tokens',
                    s.attributes->>'gen_ai.usage.prompt_tokens'
                ) IS NOT NULL))::bigint AS total_input_tokens,
                (SUM(COALESCE(
                    (s.attributes->>'gen_ai.usage.output_tokens')::bigint,
                    (s.attributes->>'gen_ai.usage.completion_tokens')::bigint
                )) FILTER (WHERE COALESCE(
                    s.attributes->>'gen_ai.usage.output_tokens',
                    s.attributes->>'gen_ai.usage.completion_tokens'
                ) IS NOT NULL))::bigint AS total_output_tokens,
                (SUM((s.attributes->>'gen_ai.usage.cache_creation.input_tokens')::bigint)
                    FILTER (WHERE s.attributes->>'gen_ai.usage.cache_creation.input_tokens' IS NOT NULL))::bigint
                    AS total_cache_creation_input_tokens,
                (SUM((s.attributes->>'gen_ai.usage.cache_read.input_tokens')::bigint)
                    FILTER (WHERE s.attributes->>'gen_ai.usage.cache_read.input_tokens' IS NOT NULL))::bigint
                    AS total_cache_read_input_tokens
            FROM otel_spans s
            WHERE {where_sql}
            GROUP BY s.trace_id
            ORDER BY MIN(s.start_time) DESC
            LIMIT ${param_idx} OFFSET ${next_param}
            "#,
            next_param = param_idx + 1
        );
        values.push((limit as i64).into());
        values.push((offset as i64).into());

        let results = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                values,
            ))
            .await?;

        let summaries = results
            .iter()
            .filter_map(|row| {
                Some(GenAiTraceSummary {
                    trace_id: row.try_get("", "trace_id").ok()?,
                    root_span_name: row.try_get("", "root_span_name").ok()?,
                    service_name: row.try_get("", "service_name").ok()?,
                    gen_ai_system: row.try_get("", "gen_ai_system").ok().flatten(),
                    gen_ai_model: row.try_get("", "gen_ai_model").ok().flatten(),
                    gen_ai_operation: row.try_get("", "gen_ai_operation").ok().flatten(),
                    start_time: row.try_get("", "start_time").ok()?,
                    duration_ms: row.try_get("", "duration_ms").ok()?,
                    span_count: row.try_get("", "span_count").ok()?,
                    error_count: row.try_get("", "error_count").ok()?,
                    total_input_tokens: row.try_get("", "total_input_tokens").ok().flatten(),
                    total_output_tokens: row.try_get("", "total_output_tokens").ok().flatten(),
                    total_cache_creation_input_tokens: row
                        .try_get("", "total_cache_creation_input_tokens")
                        .ok()
                        .flatten(),
                    total_cache_read_input_tokens: row
                        .try_get("", "total_cache_read_input_tokens")
                        .ok()
                        .flatten(),
                })
            })
            .collect();

        Ok(summaries)
    }

    async fn get_genai_trace_spans(
        &self,
        project_id: i32,
        trace_id: &str,
    ) -> StorageResult<Vec<GenAiSpanDetail>> {
        // Fetch ALL spans in the trace — not just those with gen_ai.system/provider.name.
        // This ensures child spans (HTTP, DB, tool execution) that are part of the
        // GenAI trace tree are included, giving a complete trace view.
        // The trace is already known to be a GenAI trace (discovered by query_genai_trace_summaries).
        let sql = r#"
            SELECT span_id, parent_span_id, name, kind, start_time, duration_ms,
                   status_code, attributes
            FROM otel_spans
            WHERE project_id = $1 AND trace_id = $2
            ORDER BY start_time ASC
        "#;

        let results = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                vec![project_id.into(), trace_id.to_string().into()],
            ))
            .await?;

        let spans = results
            .iter()
            .filter_map(|row| {
                let attributes: serde_json::Value = row.try_get("", "attributes").ok()?;
                let attrs: std::collections::BTreeMap<String, String> = attributes
                    .as_object()
                    .map(|obj| {
                        obj.iter()
                            .map(|(k, v)| {
                                (k.clone(), v.as_str().unwrap_or(&v.to_string()).to_string())
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                let kind_str: String = row.try_get("", "kind").ok()?;
                let kind = parse_span_kind(&kind_str);

                let status_str: String = row.try_get("", "status_code").ok()?;
                let status_code = match status_str.as_str() {
                    "OK" => SpanStatusCode::Ok,
                    "ERROR" => SpanStatusCode::Error,
                    _ => SpanStatusCode::Unset,
                };

                Some(GenAiSpanDetail::from_span_attrs(
                    row.try_get("", "span_id").ok()?,
                    row.try_get("", "parent_span_id").ok().flatten(),
                    row.try_get("", "name").ok()?,
                    kind,
                    row.try_get("", "start_time").ok()?,
                    row.try_get("", "duration_ms").ok()?,
                    status_code,
                    attrs,
                ))
            })
            .collect();

        Ok(spans)
    }

    async fn get_genai_trace_events(
        &self,
        project_id: i32,
        trace_id: &str,
    ) -> StorageResult<Vec<GenAiEvent>> {
        // Query span events from spans in this trace that have gen_ai-related events.
        // Events are stored as JSONB arrays in the otel_spans table.
        let sql = r#"
            SELECT span_id, events
            FROM otel_spans
            WHERE project_id = $1 AND trace_id = $2
              AND jsonb_array_length(COALESCE(events, '[]'::jsonb)) > 0
            ORDER BY start_time ASC
        "#;

        let results = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                vec![project_id.into(), trace_id.to_string().into()],
            ))
            .await?;

        let mut events = Vec::new();
        for row in &results {
            let span_id: String = match row.try_get("", "span_id") {
                Ok(v) => v,
                Err(_) => continue,
            };
            let events_json: serde_json::Value = match row.try_get("", "events") {
                Ok(v) => v,
                Err(_) => continue,
            };

            if let Some(event_array) = events_json.as_array() {
                for event in event_array {
                    let event_name = event
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();

                    // Only include gen_ai-related events
                    if !event_name.starts_with("gen_ai.") {
                        continue;
                    }

                    let raw_ts = event.get("timestamp").and_then(|v| v.as_str());
                    let timestamp_ns = raw_ts
                        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                        .map(|dt| dt.with_timezone(&chrono::Utc))
                        .unwrap_or_else(|| {
                            if raw_ts.is_some() {
                                warn!(
                                    span_id = %span_id,
                                    raw_timestamp = raw_ts,
                                    "get_genai_trace_events: unparsable span event timestamp; \
                                     substituting Unix epoch"
                                );
                            }
                            chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0)
                                .unwrap_or_default()
                        });

                    let attrs: std::collections::BTreeMap<String, String> = event
                        .get("attributes")
                        .and_then(|v| v.as_object())
                        .map(|obj| {
                            obj.iter()
                                .map(|(k, v)| {
                                    (k.clone(), v.as_str().unwrap_or(&v.to_string()).to_string())
                                })
                                .collect()
                        })
                        .unwrap_or_default();

                    events.push(GenAiEvent {
                        span_id: span_id.clone(),
                        trace_id: trace_id.to_string(),
                        event_name: event_name.to_string(),
                        timestamp: timestamp_ns,
                        attributes: attrs,
                    });
                }
            }
        }

        Ok(events)
    }

    async fn count_genai_traces(&self, query: TraceQuery) -> StorageResult<u64> {
        let mut where_clauses = vec![
            "project_id = $1".to_string(),
            "(attributes ? 'gen_ai.system' OR attributes ? 'gen_ai.provider.name')".to_string(),
        ];
        let mut values: Vec<sea_orm::Value> = vec![query.project_id.into()];
        let mut param_idx = 2u32;

        if let Some(ref svc) = query.service_name {
            where_clauses.push(format!("service_name = ${}", param_idx));
            values.push(svc.clone().into());
            param_idx += 1;
        }
        if let Some(start) = query.start_time {
            where_clauses.push(format!("start_time >= ${}", param_idx));
            values.push(start.into());
            param_idx += 1;
        }
        if let Some(end) = query.end_time {
            where_clauses.push(format!("start_time <= ${}", param_idx));
            values.push(end.into());
            param_idx += 1;
        }
        if let Some(ref attrs) = query.attributes {
            for (key, value) in attrs {
                match key.as_str() {
                    "gen_ai.system" => {
                        where_clauses.push(format!(
                            "COALESCE(attributes->>'gen_ai.provider.name', attributes->>'gen_ai.system') = ${}",
                            param_idx
                        ));
                        values.push(value.clone().into());
                        param_idx += 1;
                    }
                    _ => {
                        where_clauses.push(format!(
                            "attributes->>${}::text = ${}",
                            param_idx,
                            param_idx + 1
                        ));
                        values.push(key.clone().into());
                        values.push(value.clone().into());
                        param_idx += 2;
                    }
                }
            }
        }
        let _ = param_idx;

        let where_sql = where_clauses.join(" AND ");
        let sql = format!(
            "SELECT COUNT(DISTINCT trace_id)::bigint AS cnt FROM otel_spans WHERE {where_sql}"
        );

        let result = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                values,
            ))
            .await?;

        if let Some(row) = result {
            let cnt: i64 = row.try_get("", "cnt").unwrap_or(0);
            Ok(cnt as u64)
        } else {
            Ok(0)
        }
    }

    async fn query_logs(&self, query: LogQuery) -> StorageResult<Vec<LogRecord>> {
        let limit = query.limit.unwrap_or(100).min(1000);
        let offset = query.offset.unwrap_or(0);

        let mut where_clauses = vec!["project_id = $1".to_string()];
        let mut values: Vec<sea_orm::Value> = vec![query.project_id.into()];
        let mut param_idx = 2u32;

        if let Some(sev) = query.severity {
            where_clauses.push(format!("severity = ${}", param_idx));
            values.push(sev.to_string().into());
            param_idx += 1;
        }
        if let Some(ref svc) = query.service_name {
            where_clauses.push(format!("service_name = ${}", param_idx));
            values.push(svc.clone().into());
            param_idx += 1;
        }
        if let Some(ref search) = query.search {
            where_clauses.push(format!("body ILIKE ${}", param_idx));
            values.push(format!("%{}%", search).into());
            param_idx += 1;
        }
        if let Some(ref tid) = query.trace_id {
            where_clauses.push(format!("trace_id = ${}", param_idx));
            values.push(tid.clone().into());
            param_idx += 1;
        }
        if let Some(start) = query.start_time {
            where_clauses.push(format!("timestamp >= ${}", param_idx));
            values.push(start.into());
            param_idx += 1;
        }
        if let Some(end) = query.end_time {
            where_clauses.push(format!("timestamp <= ${}", param_idx));
            values.push(end.into());
            param_idx += 1;
        }

        let where_sql = where_clauses.join(" AND ");

        let sql = format!(
            r#"
            SELECT project_id, deployment_id, service_name, service_version,
                   deployment_environment, timestamp, observed_timestamp,
                   severity, severity_text, body, trace_id, span_id, attributes
            FROM otel_log_events
            WHERE {where_sql}
            ORDER BY timestamp DESC
            LIMIT ${param_idx} OFFSET ${next_param}
            "#,
            next_param = param_idx + 1
        );
        values.push((limit as i64).into());
        values.push((offset as i64).into());

        let results = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                values,
            ))
            .await?;

        let records = results
            .iter()
            .filter_map(|row| parse_log_row(row).ok())
            .collect();

        Ok(records)
    }

    async fn upsert_insight(&self, insight: &Insight) -> StorageResult<i64> {
        let anomaly_ids_json = serde_json::to_value(&insight.anomaly_ids).unwrap_or_default();

        let sql = r#"
            INSERT INTO otel_insights (
                project_id, environment, service_name, severity, status,
                title, description, metric_name, correlated_deploy_id,
                anomaly_ids, started_at, resolved_at, created_at, updated_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
            ON CONFLICT (project_id, service_name, metric_name, status)
            WHERE status = 'active'
            DO UPDATE SET
                severity = EXCLUDED.severity,
                description = EXCLUDED.description,
                anomaly_ids = EXCLUDED.anomaly_ids,
                correlated_deploy_id = EXCLUDED.correlated_deploy_id,
                updated_at = EXCLUDED.updated_at
            RETURNING id
        "#;

        let result = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                vec![
                    insight.project_id.into(),
                    insight.environment.clone().into(),
                    insight.service_name.clone().into(),
                    insight.severity.to_string().into(),
                    (match insight.status {
                        InsightStatus::Active => "active",
                        InsightStatus::Resolved => "resolved",
                    })
                    .into(),
                    insight.title.clone().into(),
                    insight.description.clone().into(),
                    insight.metric_name.clone().into(),
                    insight.correlated_deploy_id.into(),
                    anomaly_ids_json.into(),
                    insight.started_at.into(),
                    insight.resolved_at.into(),
                    insight.created_at.into(),
                    insight.updated_at.into(),
                ],
            ))
            .await?;

        match result {
            Some(row) => {
                let id: i64 = row.try_get("", "id").map_err(|e| OtelError::Storage {
                    message: format!("Failed to get insight id: {}", e),
                })?;
                Ok(id)
            }
            None => Err(OtelError::Storage {
                message: "Insight upsert returned no rows".into(),
            }),
        }
    }

    async fn list_insights(
        &self,
        project_id: i32,
        status: Option<InsightStatus>,
        limit: u64,
        offset: u64,
    ) -> StorageResult<Vec<Insight>> {
        let mut where_clauses = vec!["project_id = $1".to_string()];
        let mut values: Vec<sea_orm::Value> = vec![project_id.into()];
        let mut param_idx = 2u32;

        if let Some(s) = status {
            where_clauses.push(format!("status = ${}", param_idx));
            values.push(
                match s {
                    InsightStatus::Active => "active",
                    InsightStatus::Resolved => "resolved",
                }
                .into(),
            );
            param_idx += 1;
        }

        let where_sql = where_clauses.join(" AND ");

        let sql = format!(
            r#"
            SELECT id, project_id, environment, service_name, severity, status,
                   title, description, metric_name, correlated_deploy_id,
                   anomaly_ids, started_at, resolved_at, created_at, updated_at
            FROM otel_insights
            WHERE {where_sql}
            ORDER BY
                CASE severity
                    WHEN 'critical' THEN 0
                    WHEN 'high' THEN 1
                    WHEN 'medium' THEN 2
                    WHEN 'low' THEN 3
                    ELSE 4
                END,
                created_at DESC
            LIMIT ${param_idx} OFFSET ${next}
            "#,
            next = param_idx + 1
        );
        values.push((limit as i64).into());
        values.push((offset as i64).into());

        let results = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                values,
            ))
            .await?;

        let insights = results
            .iter()
            .filter_map(|row| parse_insight_row(row).ok())
            .collect();

        Ok(insights)
    }

    async fn resolve_insight(&self, insight_id: i64) -> StorageResult<()> {
        let sql = r#"
            UPDATE otel_insights
            SET status = 'resolved', resolved_at = NOW(), updated_at = NOW()
            WHERE id = $1 AND status = 'active'
        "#;

        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                vec![insight_id.into()],
            ))
            .await?;

        Ok(())
    }

    async fn store_health_summary(&self, summary: &HealthSummary) -> StorageResult<()> {
        let sql = r#"
            INSERT INTO otel_health_summaries (
                project_id, environment_id, service_name, status,
                uptime_pct, error_rate, p95_latency_ms,
                cpu_usage_pct, memory_usage_pct,
                last_deploy_id, last_deploy_at, computed_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
            ON CONFLICT (project_id, environment_id, service_name)
            DO UPDATE SET
                status = EXCLUDED.status,
                uptime_pct = EXCLUDED.uptime_pct,
                error_rate = EXCLUDED.error_rate,
                p95_latency_ms = EXCLUDED.p95_latency_ms,
                cpu_usage_pct = EXCLUDED.cpu_usage_pct,
                memory_usage_pct = EXCLUDED.memory_usage_pct,
                last_deploy_id = EXCLUDED.last_deploy_id,
                last_deploy_at = EXCLUDED.last_deploy_at,
                computed_at = EXCLUDED.computed_at
        "#;

        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                vec![
                    summary.project_id.into(),
                    summary.environment_id.into(),
                    summary.service_name.clone().into(),
                    summary.status.to_string().into(),
                    summary.uptime_pct.into(),
                    summary.error_rate.into(),
                    summary.p95_latency_ms.into(),
                    summary.cpu_usage_pct.into(),
                    summary.memory_usage_pct.into(),
                    summary.last_deploy_id.into(),
                    summary.last_deploy_at.into(),
                    summary.computed_at.into(),
                ],
            ))
            .await?;

        Ok(())
    }

    async fn get_health_summaries(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> StorageResult<Vec<HealthSummary>> {
        let (sql, values): (&str, Vec<sea_orm::Value>) = if let Some(env_id) = environment_id {
            (
                r#"
                SELECT project_id, environment_id, service_name, status,
                       uptime_pct, error_rate, p95_latency_ms,
                       cpu_usage_pct, memory_usage_pct,
                       last_deploy_id, last_deploy_at, computed_at
                FROM otel_health_summaries
                WHERE project_id = $1 AND environment_id = $2
                ORDER BY service_name
                "#,
                vec![project_id.into(), env_id.into()],
            )
        } else {
            (
                r#"
                SELECT project_id, environment_id, service_name, status,
                       uptime_pct, error_rate, p95_latency_ms,
                       cpu_usage_pct, memory_usage_pct,
                       last_deploy_id, last_deploy_at, computed_at
                FROM otel_health_summaries
                WHERE project_id = $1
                ORDER BY service_name
                "#,
                vec![project_id.into()],
            )
        };

        let results = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                values,
            ))
            .await?;

        let summaries = results
            .iter()
            .filter_map(|row| parse_health_summary_row(row).ok())
            .collect();

        Ok(summaries)
    }

    async fn get_storage_quota(&self, project_id: i32) -> StorageResult<StorageQuota> {
        // Quota is opt-in. With no limit configured, skip the usage estimate
        // entirely and report zeros: this method sits on the ingest hot path
        // (`OtelService::check_quota` calls it on every quota-cache miss), and
        // its three per-project COUNT(*) hypertable scans are far too
        // expensive to pay for a limit that is never enforced.
        let Some(limit_bytes) = self.quota_bytes_per_project else {
            return Ok(StorageQuota {
                project_id,
                metrics_bytes: 0,
                traces_bytes: 0,
                logs_bytes: 0,
                total_bytes: 0,
                limit_bytes: 0,
                usage_pct: 0.0,
            });
        };

        // Per-project storage is estimated as
        //   table_size * (rows for project / total rows in table).
        // The per-project numerator must be exact, but the whole-table
        // denominator only needs to be approximate -- the whole formula is
        // already a proportional estimate. On these hypertables an unfiltered
        // `COUNT(*)` scans every chunk, and this runs three times per call on
        // the ingest hot path (`check_quota`), so the denominator uses
        // TimescaleDB's `approximate_row_count` (planner stats, microseconds).
        // `GREATEST(.., 1)` still guards against a zero/negative estimate on a
        // freshly-created, never-analyzed table.
        let sql = r#"
            SELECT
                COALESCE((SELECT pg_total_relation_size('otel_metrics') *
                    (SELECT COUNT(*) FROM otel_metrics WHERE project_id = $1)::float /
                    GREATEST(approximate_row_count('otel_metrics'::regclass), 1)::float
                ), 0)::bigint +
                COALESCE((SELECT pg_total_relation_size('otel_spans') *
                    (SELECT COUNT(*) FROM otel_spans WHERE project_id = $1)::float /
                    GREATEST(approximate_row_count('otel_spans'::regclass), 1)::float
                ), 0)::bigint +
                COALESCE((SELECT pg_total_relation_size('otel_log_events') *
                    (SELECT COUNT(*) FROM otel_log_events WHERE project_id = $1)::float /
                    GREATEST(approximate_row_count('otel_log_events'::regclass), 1)::float
                ), 0)::bigint as total_bytes
        "#;

        let result = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                vec![project_id.into()],
            ))
            .await?;

        let total_bytes = match result {
            Some(row) => row.try_get::<i64>("", "total_bytes").unwrap_or(0) as u64,
            None => 0,
        };

        let usage_pct = if limit_bytes > 0 {
            (total_bytes as f64 / limit_bytes as f64) * 100.0
        } else {
            0.0
        };

        Ok(StorageQuota {
            project_id,
            metrics_bytes: 0, // Approximate breakdown not available cheaply
            traces_bytes: 0,
            logs_bytes: 0,
            total_bytes,
            limit_bytes,
            usage_pct,
        })
    }

    async fn check_quota(&self, project_id: i32) -> StorageResult<bool> {
        // With no quota configured, get_storage_quota short-circuits to zeros
        // without touching the database, so this is always "not exceeded".
        let quota = self.get_storage_quota(project_id).await?;
        Ok(quota.usage_pct >= 100.0)
    }

    async fn get_metric_baseline(
        &self,
        project_id: i32,
        service_name: &str,
        metric_name: &str,
        environment: Option<&str>,
        lookback_days: i32,
    ) -> StorageResult<Vec<BaselinePoint>> {
        let mut where_clauses = vec![
            "project_id = $1".to_string(),
            "service_name = $2".to_string(),
            "metric_name = $3".to_string(),
            format!("timestamp >= NOW() - INTERVAL '{} days'", lookback_days),
        ];
        let mut values: Vec<sea_orm::Value> = vec![
            project_id.into(),
            service_name.to_string().into(),
            metric_name.to_string().into(),
        ];
        let mut param_idx = 4u32;

        if let Some(env) = environment {
            where_clauses.push(format!("deployment_environment = ${}", param_idx));
            values.push(env.to_string().into());
            param_idx += 1;
        }
        let _ = param_idx;

        let where_sql = where_clauses.join(" AND ");

        let sql = format!(
            r#"
            SELECT
                EXTRACT(HOUR FROM timestamp)::int as hour_of_day,
                EXTRACT(DOW FROM timestamp)::int as day_of_week,
                AVG(value) as avg_value,
                COALESCE(STDDEV(value), 0) as stddev_value,
                COUNT(*) as sample_count
            FROM otel_metrics
            WHERE {where_sql}
            GROUP BY hour_of_day, day_of_week
            ORDER BY day_of_week, hour_of_day
            "#
        );

        let rows = BaselineRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            values,
        ))
        .all(self.db.as_ref())
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| BaselinePoint {
                hour_of_day: r.hour_of_day,
                day_of_week: r.day_of_week,
                avg_value: r.avg_value,
                stddev_value: r.stddev_value,
                sample_count: r.sample_count,
            })
            .collect())
    }

    async fn get_recent_minute_aggregates(
        &self,
        project_id: i32,
        service_name: &str,
        metric_name: &str,
        environment: Option<&str>,
        minutes: i32,
    ) -> StorageResult<Vec<MinuteAggregate>> {
        let mut where_clauses = vec![
            "project_id = $1".to_string(),
            "service_name = $2".to_string(),
            "metric_name = $3".to_string(),
            format!("timestamp >= NOW() - INTERVAL '{} minutes'", minutes),
        ];
        let mut values: Vec<sea_orm::Value> = vec![
            project_id.into(),
            service_name.to_string().into(),
            metric_name.to_string().into(),
        ];
        let mut param_idx = 4u32;

        if let Some(env) = environment {
            where_clauses.push(format!("deployment_environment = ${}", param_idx));
            values.push(env.to_string().into());
            param_idx += 1;
        }
        let _ = param_idx;

        let where_sql = where_clauses.join(" AND ");

        let sql = format!(
            r#"
            SELECT
                time_bucket('1 minute', timestamp) as bucket,
                AVG(value) as avg_value,
                COUNT(*) as count
            FROM otel_metrics
            WHERE {where_sql}
            GROUP BY bucket
            ORDER BY bucket ASC
            "#
        );

        let rows = MinuteAggregateRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            values,
        ))
        .all(self.db.as_ref())
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| MinuteAggregate {
                bucket: r.bucket,
                avg_value: r.avg_value,
                count: r.count,
            })
            .collect())
    }

    async fn get_recent_deploys(
        &self,
        project_id: i32,
        minutes: i32,
    ) -> StorageResult<Vec<DeployEvent>> {
        let sql = format!(
            r#"
            SELECT id, project_id, environment_id, created_at
            FROM deployments
            WHERE project_id = $1
              AND status = 'succeeded'
              AND created_at >= NOW() - INTERVAL '{} minutes'
            ORDER BY created_at DESC
            "#,
            minutes
        );

        let rows = DeployRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            vec![project_id.into()],
        ))
        .all(self.db.as_ref())
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| DeployEvent {
                deployment_id: r.id,
                project_id: r.project_id,
                environment_id: r.environment_id,
                deployed_at: r.created_at,
                service_name: None,
            })
            .collect())
    }

    async fn apply_retention(&self, _project_id: i32) -> StorageResult<u64> {
        // No-op. The OTel hypertables (`otel_metrics`, `otel_spans`,
        // `otel_log_events`) all have a native TimescaleDB
        // `add_retention_policy(..., INTERVAL '90 days')` registered in
        // `m20260225_000001_create_otel_tables`. Timescale runs that policy
        // in the background using `drop_chunks`, which is atomic and
        // chunk-aware.
        //
        // The original implementation issued
        // `DELETE FROM otel_metrics WHERE timestamp < ...` from this
        // method on every retention tick. That DELETE races with the
        // native policy: the planner snapshots a chunk list, the policy
        // worker drops one of those chunks, the executor reaches it, and
        // PostgreSQL throws `chunk not found` (observed in prod logs as
        // `_hyper_15_3617_chunk` already gone). The error then surfaces
        // as `Failed to run migrations: chunk not found` because the
        // retention task and migration loader run on overlapping startup
        // connections.
        //
        // We keep the trait method so callers/tests don't break; for the
        // hypertables it does nothing — Timescale's policy is the single
        // source of truth for their retention.
        //
        // EXCEPTION: `otel_trace_summaries` is a plain table, NOT a
        // hypertable, so no native retention policy covers it. A summary row
        // can't outlive the spans it derives from, which `otel_spans` expires
        // at 90 days, so we sweep summaries on the same window here. This is a
        // plain indexed DELETE on `start_time` (idx_otel_trace_summaries_start)
        // and does not race any Timescale `drop_chunks` worker, since the
        // summary table has no chunks.
        let deleted = self
            .db
            .execute(Statement::from_string(
                DatabaseBackend::Postgres,
                "DELETE FROM otel_trace_summaries WHERE start_time < now() - INTERVAL '90 days'"
                    .to_string(),
            ))
            .await
            .map(|r| r.rows_affected())
            .unwrap_or_else(|e| {
                // Non-fatal: a failed summary sweep just leaves stale rows that
                // the next tick retries. Never propagate — the retention task
                // logs and continues.
                warn!(error = %e, "otel_trace_summaries retention sweep failed");
                0
            });

        Ok(deleted)
    }

    async fn get_p95_latency(
        &self,
        project_id: i32,
        service_name: &str,
        window_minutes: i32,
    ) -> StorageResult<f64> {
        let sql = format!(
            r#"
            SELECT COALESCE(
                percentile_cont(0.95) WITHIN GROUP (ORDER BY duration_ms),
                0.0
            ) as p95
            FROM otel_spans
            WHERE project_id = $1
              AND service_name = $2
              AND start_time >= NOW() - INTERVAL '{} minutes'
            "#,
            window_minutes
        );

        let row = P95Row::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            vec![project_id.into(), service_name.to_string().into()],
        ))
        .one(self.db.as_ref())
        .await?;

        Ok(row.map(|r| r.p95).unwrap_or(0.0))
    }
}

// Inherent helpers backing the trait dispatch above. Kept out of the trait
// impl so they can be private and unit-tested without going through the
// OtelStorage trait.
impl TimescaleDbStorage {
    /// Fallback list query that aggregates spans at read time
    /// (`GROUP BY trace_id`). Used only when the query carries a span-level
    /// filter the summary table can't satisfy — `attributes` (per-span JSONB,
    /// e.g. GenAI `gen_ai.system`) or `name_pattern` matched against *any*
    /// span name. These are narrow drill-downs, so the unindexed
    /// sort-after-aggregate cost is acceptable. The common list view (no such
    /// filter) goes through the fast summary-table path instead.
    async fn query_trace_summaries_from_spans(
        &self,
        query: TraceQuery,
    ) -> StorageResult<Vec<TraceSummary>> {
        let limit = query.limit.unwrap_or(50).min(100);
        let offset = query.offset.unwrap_or(0);

        let mut where_clauses = vec!["s.project_id = $1".to_string()];
        let mut values: Vec<sea_orm::Value> = vec![query.project_id.into()];
        let mut param_idx = 2u32;

        if let Some(ref tid) = query.trace_id {
            where_clauses.push(format!("s.trace_id = ${}", param_idx));
            values.push(tid.clone().into());
            param_idx += 1;
        }
        if let Some(ref svc) = query.service_name {
            where_clauses.push(format!("s.service_name = ${}", param_idx));
            values.push(svc.clone().into());
            param_idx += 1;
        }
        if let Some(min_dur) = query.min_duration_ms {
            where_clauses.push(format!("s.duration_ms >= ${}", param_idx));
            values.push(min_dur.into());
            param_idx += 1;
        }
        if let Some(start) = query.start_time {
            where_clauses.push(format!("s.start_time >= ${}", param_idx));
            values.push(start.into());
            param_idx += 1;
        }
        if let Some(end) = query.end_time {
            where_clauses.push(format!("s.start_time <= ${}", param_idx));
            values.push(end.into());
            param_idx += 1;
        }
        if let Some(deployment_id) = query.deployment_id {
            where_clauses.push(format!("s.deployment_id = ${}", param_idx));
            values.push(deployment_id.into());
            param_idx += 1;
        }
        if let Some(environment_id) = query.environment_id {
            where_clauses.push(format!("e.id = ${}", param_idx));
            values.push(environment_id.into());
            param_idx += 1;
        }
        if let Some(ref attrs) = query.attributes {
            for (key, value) in attrs {
                where_clauses.push(format!(
                    "s.attributes->>${}::text = ${}",
                    param_idx,
                    param_idx + 1
                ));
                values.push(key.clone().into());
                values.push(value.clone().into());
                param_idx += 2;
            }
        }
        if let Some(ref pattern) = query.name_pattern {
            where_clauses.push(format!("s.name ILIKE ${}", param_idx));
            values.push(format!("%{}%", escape_like_pattern(pattern)).into());
            param_idx += 1;
        }

        // status filter: if ERROR, find traces that have at least one error span
        // if OK, find traces with no error spans
        let status_having = match query.status {
            Some(crate::types::SpanStatusCode::Error) => {
                "HAVING COUNT(*) FILTER (WHERE s.status_code = 'ERROR') > 0"
            }
            Some(crate::types::SpanStatusCode::Ok) => {
                "HAVING COUNT(*) FILTER (WHERE s.status_code = 'ERROR') = 0"
            }
            _ => "",
        };

        let where_sql = where_clauses.join(" AND ");

        // Build the ORDER BY from the requested sort. Both options sort on an
        // aggregate (this is a GROUP BY trace_id query), so neither can use an
        // index — but the time-window WHERE keeps the grouped set small. We add
        // a stable tie-breaker so pagination is deterministic across pages.
        //
        // NOTE: sort_by/sort_order come from a fixed enum, not user strings, so
        // interpolating them into SQL is injection-safe.
        let order_dir = query.sort_order.as_sql();
        let order_sql = match query.sort_by {
            crate::types::TraceSortField::Duration => {
                format!(
                    "ORDER BY MAX(s.duration_ms) {order_dir}, MIN(s.start_time) DESC, s.trace_id"
                )
            }
            crate::types::TraceSortField::StartTime => {
                format!("ORDER BY MIN(s.start_time) {order_dir}, s.trace_id")
            }
        };

        // Aggregate per trace_id: pick root span (NULL parent) or longest span,
        // count total spans and error spans, compute trace duration.
        // LEFT JOIN deployments + environments to resolve the environment name
        // from the deployment record, falling back to the OTel resource attribute.
        let sql = format!(
            r#"
            SELECT
                s.trace_id,
                (array_agg(s.name ORDER BY
                    CASE WHEN s.parent_span_id IS NULL THEN 0 ELSE 1 END,
                    s.duration_ms DESC
                ))[1] AS root_span_name,
                (array_agg(s.service_name ORDER BY
                    CASE WHEN s.parent_span_id IS NULL THEN 0 ELSE 1 END,
                    s.duration_ms DESC
                ))[1] AS service_name,
                (array_agg(s.kind ORDER BY
                    CASE WHEN s.parent_span_id IS NULL THEN 0 ELSE 1 END,
                    s.duration_ms DESC
                ))[1] AS kind,
                (array_agg(COALESCE(e.name, s.deployment_environment) ORDER BY
                    CASE WHEN s.parent_span_id IS NULL THEN 0 ELSE 1 END,
                    s.duration_ms DESC
                ))[1] AS deployment_environment,
                MIN(s.start_time) AS start_time,
                MAX(s.duration_ms) AS duration_ms,
                COUNT(*)::bigint AS span_count,
                COUNT(*) FILTER (WHERE s.status_code = 'ERROR')::bigint AS error_count
            FROM otel_spans s
            LEFT JOIN deployments d ON d.id = s.deployment_id
            LEFT JOIN environments e ON e.id = d.environment_id
            WHERE {where_sql}
            GROUP BY s.trace_id
            {status_having}
            {order_sql}
            LIMIT ${param_idx} OFFSET ${next_param}
            "#,
            next_param = param_idx + 1
        );
        values.push((limit as i64).into());
        values.push((offset as i64).into());

        let results = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                values,
            ))
            .await?;

        let summaries = results
            .iter()
            .filter_map(|row| {
                let trace_id: String = row.try_get("", "trace_id").ok()?;
                let root_span_name: String = row.try_get("", "root_span_name").ok()?;
                let service_name: String = row.try_get("", "service_name").ok()?;
                let kind_str: String = row.try_get("", "kind").ok()?;
                let deployment_environment: Option<String> =
                    row.try_get("", "deployment_environment").ok().flatten();
                let start_time: DateTime<Utc> = row.try_get("", "start_time").ok()?;
                let duration_ms: f64 = row.try_get("", "duration_ms").ok()?;
                let span_count: i64 = row.try_get("", "span_count").ok()?;
                let error_count: i64 = row.try_get("", "error_count").ok()?;

                let kind = parse_span_kind(&kind_str);

                let status_code = if error_count > 0 {
                    crate::types::SpanStatusCode::Error
                } else {
                    crate::types::SpanStatusCode::Ok
                };

                Some(TraceSummary {
                    trace_id,
                    root_span_name,
                    service_name,
                    deployment_environment,
                    kind,
                    status_code,
                    start_time,
                    duration_ms,
                    span_count,
                    error_count,
                })
            })
            .collect();

        Ok(summaries)
    }

    /// Fallback count that matches `query_trace_summaries_from_spans` exactly.
    /// Used only on the span-aggregation fallback path (attribute/name filters).
    async fn count_traces_from_spans(&self, query: TraceQuery) -> StorageResult<u64> {
        // Mirrors query_trace_summaries_from_spans filters exactly — including
        // `status` (via HAVING) and `min_duration_ms` — so the pagination count
        // matches the actual result set returned by that method.
        let mut where_clauses = vec!["s.project_id = $1".to_string()];
        let mut values: Vec<sea_orm::Value> = vec![query.project_id.into()];
        let mut param_idx = 2u32;

        if let Some(ref tid) = query.trace_id {
            where_clauses.push(format!("s.trace_id = ${}", param_idx));
            values.push(tid.clone().into());
            param_idx += 1;
        }
        if let Some(ref svc) = query.service_name {
            where_clauses.push(format!("s.service_name = ${}", param_idx));
            values.push(svc.clone().into());
            param_idx += 1;
        }
        if let Some(min_dur) = query.min_duration_ms {
            where_clauses.push(format!("s.duration_ms >= ${}", param_idx));
            values.push(min_dur.into());
            param_idx += 1;
        }
        if let Some(start) = query.start_time {
            where_clauses.push(format!("s.start_time >= ${}", param_idx));
            values.push(start.into());
            param_idx += 1;
        }
        if let Some(end) = query.end_time {
            where_clauses.push(format!("s.start_time <= ${}", param_idx));
            values.push(end.into());
            param_idx += 1;
        }
        if let Some(deployment_id) = query.deployment_id {
            where_clauses.push(format!("s.deployment_id = ${}", param_idx));
            values.push(deployment_id.into());
            param_idx += 1;
        }
        if let Some(environment_id) = query.environment_id {
            where_clauses.push(format!(
                "s.deployment_id IN (SELECT id FROM deployments WHERE environment_id = ${})",
                param_idx
            ));
            values.push(environment_id.into());
            param_idx += 1;
        }
        if let Some(ref attrs) = query.attributes {
            for (key, value) in attrs {
                where_clauses.push(format!(
                    "s.attributes->>${}::text = ${}",
                    param_idx,
                    param_idx + 1
                ));
                values.push(key.clone().into());
                values.push(value.clone().into());
                param_idx += 2;
            }
        }
        if let Some(ref pattern) = query.name_pattern {
            where_clauses.push(format!("s.name ILIKE ${}", param_idx));
            values.push(format!("%{}%", escape_like_pattern(pattern)).into());
            param_idx += 1;
        }

        // status filter: mirrors query_trace_summaries HAVING clause.
        // ERROR = traces with at least one ERROR span; OK = traces with none.
        let status_having = match query.status {
            Some(crate::types::SpanStatusCode::Error) => {
                "HAVING COUNT(*) FILTER (WHERE s.status_code = 'ERROR') > 0"
            }
            Some(crate::types::SpanStatusCode::Ok) => {
                "HAVING COUNT(*) FILTER (WHERE s.status_code = 'ERROR') = 0"
            }
            _ => "",
        };

        let where_sql = where_clauses.join(" AND ");

        // Wrap in a subquery so we can count the traces that survive the HAVING.
        let sql = format!(
            "SELECT COUNT(*) AS cnt FROM (\
                SELECT s.trace_id \
                FROM otel_spans s \
                WHERE {where_sql} \
                GROUP BY s.trace_id \
                {status_having}\
            ) sub",
        );
        let _ = param_idx;

        let result = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                values,
            ))
            .await?;

        if let Some(row) = result {
            let cnt: i64 = row.try_get("", "cnt").unwrap_or(0);
            Ok(cnt as u64)
        } else {
            Ok(0)
        }
    }

    /// Fast list query reading the pre-aggregated `otel_trace_summaries` table.
    /// Every filter maps to an indexed column on the summary row and both sorts
    /// are index scans, so this is O(log n) in the number of traces instead of
    /// the GROUP-BY-then-sort the spans path pays.
    ///
    /// The `deployment_environment` display value still resolves the canonical
    /// environment name via a LEFT JOIN to `deployments`/`environments`
    /// (falling back to the stored resource attribute), identical to the old
    /// query, so the UI shows the same label.
    async fn query_trace_summaries_from_table(
        &self,
        query: TraceQuery,
    ) -> StorageResult<Vec<TraceSummary>> {
        let limit = query.limit.unwrap_or(50).min(100);
        let offset = query.offset.unwrap_or(0);

        let (where_sql, mut values, param_idx) = Self::build_summary_filters(&query);

        // sort_by/sort_order come from fixed enums, never user strings, so
        // interpolating them is injection-safe. Both columns are indexed
        // (idx_..._project_duration, idx_..._project_start), and the tie-breaker
        // keeps pagination deterministic across pages.
        let order_dir = query.sort_order.as_sql();
        let order_sql = match query.sort_by {
            crate::types::TraceSortField::Duration => {
                format!("ORDER BY ts.duration_ms {order_dir}, ts.start_time DESC, ts.trace_id")
            }
            crate::types::TraceSortField::StartTime => {
                format!("ORDER BY ts.start_time {order_dir}, ts.trace_id")
            }
        };

        let sql = format!(
            r#"
            SELECT
                ts.trace_id,
                ts.root_span_name,
                ts.service_name,
                ts.kind,
                COALESCE(e.name, ts.deployment_environment) AS deployment_environment,
                ts.start_time,
                ts.duration_ms,
                ts.span_count,
                ts.error_count
            FROM otel_trace_summaries ts
            LEFT JOIN deployments d ON d.id = ts.deployment_id
            LEFT JOIN environments e ON e.id = d.environment_id
            WHERE {where_sql}
            {order_sql}
            LIMIT ${limit_param} OFFSET ${offset_param}
            "#,
            limit_param = param_idx,
            offset_param = param_idx + 1,
        );
        values.push((limit as i64).into());
        values.push((offset as i64).into());

        let results = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                values,
            ))
            .await?;

        let summaries = results
            .iter()
            .filter_map(|row| {
                let trace_id: String = row.try_get("", "trace_id").ok()?;
                let root_span_name: String = row.try_get("", "root_span_name").ok()?;
                let service_name: String = row.try_get("", "service_name").ok()?;
                let kind_str: String = row.try_get("", "kind").ok()?;
                let deployment_environment: Option<String> =
                    row.try_get("", "deployment_environment").ok().flatten();
                let start_time: DateTime<Utc> = row.try_get("", "start_time").ok()?;
                let duration_ms: f64 = row.try_get("", "duration_ms").ok()?;
                let span_count: i64 = row.try_get("", "span_count").ok()?;
                let error_count: i64 = row.try_get("", "error_count").ok()?;

                let kind = parse_span_kind(&kind_str);
                let status_code = if error_count > 0 {
                    crate::types::SpanStatusCode::Error
                } else {
                    crate::types::SpanStatusCode::Ok
                };

                Some(TraceSummary {
                    trace_id,
                    root_span_name,
                    service_name,
                    deployment_environment,
                    kind,
                    status_code,
                    start_time,
                    duration_ms,
                    span_count,
                    error_count,
                })
            })
            .collect();

        Ok(summaries)
    }

    /// Fast count over `otel_trace_summaries`, matching
    /// `query_trace_summaries_from_table` filters exactly.
    async fn count_traces_from_table(&self, query: TraceQuery) -> StorageResult<u64> {
        let (where_sql, values, _param_idx) = Self::build_summary_filters(&query);

        // No JOIN needed for the count: every filter is on ts columns, and the
        // env JOIN only affects the displayed name, not row membership. (The
        // environment_id filter is expressed as a deployment_id subquery in
        // build_summary_filters, so it doesn't need the JOIN either.)
        let sql = format!("SELECT COUNT(*) AS cnt FROM otel_trace_summaries ts WHERE {where_sql}");

        let result = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                values,
            ))
            .await?;

        if let Some(row) = result {
            let cnt: i64 = row.try_get("", "cnt").unwrap_or(0);
            Ok(cnt as u64)
        } else {
            Ok(0)
        }
    }

    /// Build the shared WHERE clause + bound params for the summary-table
    /// list/count queries. Returns `(where_sql, values, next_param_idx)`.
    ///
    /// All filters map to indexed `ts` columns:
    /// - time window → `ts.start_time` (the trace's earliest span)
    /// - `status` → `ts.error_count > 0` / `= 0` (partial index for errors)
    /// - `min_duration_ms` → `ts.duration_ms` (the trace's longest span)
    /// - `environment_id` → `ts.deployment_id IN (SELECT … environment_id = ?)`
    /// - `name_pattern` → `ts.root_span_name ILIKE ?` (root-name match; the
    ///   any-span variant takes the span-aggregation fallback instead)
    fn build_summary_filters(query: &TraceQuery) -> (String, Vec<sea_orm::Value>, u32) {
        let mut where_clauses = vec!["ts.project_id = $1".to_string()];
        let mut values: Vec<sea_orm::Value> = vec![query.project_id.into()];
        let mut param_idx = 2u32;

        if let Some(ref tid) = query.trace_id {
            where_clauses.push(format!("ts.trace_id = ${param_idx}"));
            values.push(tid.clone().into());
            param_idx += 1;
        }
        if let Some(ref svc) = query.service_name {
            where_clauses.push(format!("ts.service_name = ${param_idx}"));
            values.push(svc.clone().into());
            param_idx += 1;
        }
        if let Some(min_dur) = query.min_duration_ms {
            where_clauses.push(format!("ts.duration_ms >= ${param_idx}"));
            values.push(min_dur.into());
            param_idx += 1;
        }
        if let Some(start) = query.start_time {
            where_clauses.push(format!("ts.start_time >= ${param_idx}"));
            values.push(start.into());
            param_idx += 1;
        }
        if let Some(end) = query.end_time {
            where_clauses.push(format!("ts.start_time <= ${param_idx}"));
            values.push(end.into());
            param_idx += 1;
        }
        if let Some(deployment_id) = query.deployment_id {
            where_clauses.push(format!("ts.deployment_id = ${param_idx}"));
            values.push(deployment_id.into());
            param_idx += 1;
        }
        if let Some(environment_id) = query.environment_id {
            where_clauses.push(format!(
                "ts.deployment_id IN (SELECT id FROM deployments WHERE environment_id = ${param_idx})"
            ));
            values.push(environment_id.into());
            param_idx += 1;
        }
        match query.status {
            Some(crate::types::SpanStatusCode::Error) => {
                where_clauses.push("ts.error_count > 0".to_string());
            }
            Some(crate::types::SpanStatusCode::Ok) => {
                where_clauses.push("ts.error_count = 0".to_string());
            }
            _ => {}
        }
        if let Some(ref pattern) = query.name_pattern {
            where_clauses.push(format!("ts.root_span_name ILIKE ${param_idx}"));
            values.push(format!("%{}%", escape_like_pattern(pattern)).into());
            param_idx += 1;
        }

        (where_clauses.join(" AND "), values, param_idx)
    }
}

// ── LIKE pattern helpers ─────────────────────────────────────────────

// ── Metric query helpers ────────────────────────────────────────────────────

/// Build the shared WHERE clause + bound values for the metric queries.
///
/// Returns `(where_sql, values, next_param_idx)` where `next_param_idx` is
/// the first `$N` index NOT yet consumed.  The caller then appends its own
/// parameters (group_by keys, interval, limit) starting at that index.
///
/// Parameter binding order:
///   $1  = project_id
///   $2… = metric_name (optional), service_name (optional), metric_type
///          (optional), deployment_environment (optional), label_filter values
///          (optional), start_time (optional), end_time (optional)
fn build_metrics_where(query: &MetricQuery) -> (String, Vec<sea_orm::Value>, u32) {
    let mut where_clauses = vec!["project_id = $1".to_string()];
    let mut values: Vec<sea_orm::Value> = vec![query.project_id.into()];
    let mut param_idx = 2u32;

    if let Some(ref name) = query.metric_name {
        where_clauses.push(format!("metric_name = ${param_idx}"));
        values.push(name.clone().into());
        param_idx += 1;
    }
    if let Some(ref svc) = query.service_name {
        where_clauses.push(format!("service_name = ${param_idx}"));
        values.push(svc.clone().into());
        param_idx += 1;
    }
    if let Some(mt) = query.metric_type {
        where_clauses.push(format!("metric_type = ${param_idx}"));
        values.push(mt.to_string().into());
        param_idx += 1;
    }
    if let Some(ref env) = query.environment {
        where_clauses.push(format!("deployment_environment = ${param_idx}"));
        values.push(env.clone().into());
        param_idx += 1;
    }
    // label_filters: use JSONB containment @> so the GIN index applies.
    // Build one JSONB object from all (key, value) pairs.
    if !query.label_filters.is_empty() {
        // Construct the JSONB literal via jsonb_build_object($k1,$v1,$k2,$v2,…).
        let mut kv_params: Vec<String> = Vec::with_capacity(query.label_filters.len() * 2);
        for (k, v) in &query.label_filters {
            kv_params.push(format!("${param_idx}"));
            values.push(k.clone().into());
            param_idx += 1;
            kv_params.push(format!("${param_idx}"));
            values.push(v.clone().into());
            param_idx += 1;
        }
        where_clauses.push(format!(
            "attributes @> jsonb_build_object({})",
            kv_params.join(", ")
        ));
    }
    if let Some(start) = query.start_time {
        where_clauses.push(format!("timestamp >= ${param_idx}"));
        values.push(start.into());
        param_idx += 1;
    }
    if let Some(end) = query.end_time {
        where_clauses.push(format!("timestamp <= ${param_idx}"));
        values.push(end.into());
        param_idx += 1;
    }

    (where_clauses.join(" AND "), values, param_idx)
}

/// Canonical string key for the series-values JSON (used as a HashMap key).
/// `None` / `Null` → empty string (ungrouped series).
fn series_json_key(val: &Option<serde_json::Value>) -> String {
    match val {
        Some(v) if !v.is_null() => v.to_string(),
        _ => String::new(),
    }
}

/// Decode a nullable JSONB column containing `[1.0, 2.0, …]` into `Vec<f64>`.
fn parse_jsonb_f64_array(val: Option<&serde_json::Value>) -> Option<Vec<f64>> {
    let arr = val?.as_array()?;
    let result: Option<Vec<f64>> = arr
        .iter()
        .map(|v| {
            v.as_f64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .collect();
    result
}

/// Decode a nullable JSONB column containing `[1, 2, …]` into `Vec<u64>`.
fn parse_jsonb_u64_array(val: Option<&serde_json::Value>) -> Option<Vec<u64>> {
    let arr = val?.as_array()?;
    let result: Option<Vec<u64>> = arr
        .iter()
        .map(|v| {
            v.as_u64()
                .or_else(|| v.as_i64().map(|i| i as u64))
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .collect();
    result
}

/// Estimate the `q`-th quantile (`0.0..=1.0`) from an explicit-bucket histogram
/// via linear interpolation within the bucket containing the q-th observation —
/// the Prometheus `histogram_quantile` model.
///
/// `bounds` are the inclusive upper bounds of the first N buckets (OTLP
/// `explicit_bounds`); `bucket_counts` has N+1 entries (one per bucket plus the
/// final `+Inf` overflow bucket), though a missing overflow bucket is tolerated.
/// Returns `None` for an empty/malformed histogram so the caller can fall back
/// to the arithmetic mean.
fn histogram_quantile(q: f64, bounds: &[f64], bucket_counts: &[u64]) -> Option<f64> {
    if bounds.is_empty() || bucket_counts.is_empty() {
        return None;
    }
    let total: u64 = bucket_counts.iter().sum();
    if total == 0 {
        return None;
    }
    let q = q.clamp(0.0, 1.0);
    let rank = q * total as f64;

    let mut cum_before = 0.0_f64;
    for (i, &count) in bucket_counts.iter().enumerate() {
        let cum_after = cum_before + count as f64;
        if cum_after < rank {
            cum_before = cum_after;
            continue;
        }
        // The q-th observation falls in bucket `i` = (lower, upper].
        let upper = match bounds.get(i) {
            Some(&u) => u,
            // Overflow (+Inf) bucket: no finite upper bound to interpolate to —
            // report the largest finite bound.
            None => return bounds.last().copied(),
        };
        // First bucket with a non-positive upper bound: no sensible lower bound
        // to interpolate from (Prometheus convention) — report the upper bound.
        if i == 0 && upper <= 0.0 {
            return Some(upper);
        }
        let lower = if i == 0 { 0.0 } else { *bounds.get(i - 1)? };
        if count == 0 {
            return Some(upper);
        }
        let frac = (rank - cum_before) / count as f64;
        return Some(lower + (upper - lower) * frac);
    }
    // rank == total (e.g. q == 1.0) → top of the distribution.
    bounds.last().copied()
}

// ── Like-pattern helpers ────────────────────────────────────────────────────

/// Escape LIKE/ILIKE metacharacters in a user-supplied substring pattern.
///
/// PostgreSQL ILIKE uses backslash as the default escape character. We escape:
/// - `\` → `\\`   (backslash itself, first)
/// - `%` → `\%`   (wildcard: any sequence of chars)
/// - `_` → `\_`   (wildcard: exactly one char)
///
/// The caller wraps the result with `%{escaped}%` for a substring match.
fn escape_like_pattern(pattern: &str) -> String {
    pattern
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Whether a trace query needs a span-level filter the pre-aggregated
/// `otel_trace_summaries` table can't satisfy, forcing the slower
/// `GROUP BY trace_id` fallback over the spans hypertable.
///
/// Two filters are span-level:
/// - `attributes`: per-span JSONB (e.g. GenAI `gen_ai.system`). The summary
///   row has no attributes, so this must scan spans.
/// - `name_pattern`: the spans path matches *any* span name in the trace,
///   while the summary only knows the root span name. To preserve the exact
///   any-span semantics, route name-pattern queries through the fallback too.
///
/// Everything else (project, time window, service, deployment, environment,
/// status, min-duration, trace_id) maps to indexed summary columns and takes
/// the fast path.
fn needs_span_level_filter(query: &TraceQuery) -> bool {
    let has_attributes = query.attributes.as_ref().is_some_and(|a| !a.is_empty());
    let has_name_pattern = query.name_pattern.as_ref().is_some_and(|p| !p.is_empty());
    has_attributes || has_name_pattern
}

/// Parse the canonical span-kind text stored in `otel_spans.kind` /
/// `otel_trace_summaries.kind` (uppercase, e.g. `"SERVER"`, written by
/// `SpanKind::to_string`) back into a [`SpanKind`]. Unknown values fall back to
/// `Internal`.
///
/// NOTE: the previous trace-summary read path matched capitalized variants
/// (`"Server"`, `"Client"`, …) which never matched the uppercase stored
/// values, so every trace's kind silently collapsed to `Internal`. This parser
/// matches the actually-stored uppercase form and fixes that.
fn parse_span_kind(kind_str: &str) -> crate::types::SpanKind {
    match kind_str {
        "SERVER" => crate::types::SpanKind::Server,
        "CLIENT" => crate::types::SpanKind::Client,
        "PRODUCER" => crate::types::SpanKind::Producer,
        "CONSUMER" => crate::types::SpanKind::Consumer,
        "UNSPECIFIED" => crate::types::SpanKind::Unspecified,
        _ => crate::types::SpanKind::Internal,
    }
}

/// One trace's worth of aggregates folded from a span batch, ready to upsert
/// into `otel_trace_summaries`. The root fields are `Some` only when this batch
/// carried the trace's root span (parent_span_id IS NULL); otherwise the upsert
/// leaves the stored row's root identity untouched.
#[derive(Debug, Clone, PartialEq)]
struct TraceDelta {
    project_id: i32,
    trace_id: String,
    /// Earliest span start seen in this batch for the trace.
    start_time: DateTime<Utc>,
    /// Longest span duration seen in this batch for the trace.
    max_duration_ms: f64,
    span_count: i64,
    error_count: i64,
    root_span_name: Option<String>,
    root_service_name: Option<String>,
    root_kind: Option<String>,
    root_env: Option<String>,
    root_deployment_id: Option<i32>,
    has_root: bool,
}

/// Fold a span batch into one [`TraceDelta`] per distinct `(project_id,
/// trace_id)`. Pure (no I/O) so the aggregation semantics can be unit-tested
/// without a database.
///
/// Semantics, matching the `ON CONFLICT DO UPDATE` in `upsert_trace_summaries`:
/// - `span_count` / `error_count` count this batch's spans (accumulated into
///   the stored totals on upsert).
/// - `start_time` is the MIN and `max_duration_ms` the MAX across the batch
///   (combined with the stored row via LEAST/GREATEST on upsert).
/// - The first root span encountered (parent_span_id IS NULL) sets the root
///   identity fields; later spans never overwrite them within the batch. A
///   batch with no root leaves all root fields `None` / `has_root = false`.
///
/// Output order is deterministic (sorted by `(project_id, trace_id)`) so the
/// generated multi-row INSERT and tests are stable.
fn fold_trace_deltas(spans: &[SpanRecord]) -> Vec<TraceDelta> {
    use std::collections::HashMap;

    let mut by_trace: HashMap<(i32, String), TraceDelta> = HashMap::new();

    for s in spans {
        let is_error = matches!(s.status_code, SpanStatusCode::Error);
        let is_root = s.parent_span_id.is_none();
        let key = (s.project_id, s.trace_id.clone());

        let entry = by_trace.entry(key).or_insert_with(|| TraceDelta {
            project_id: s.project_id,
            trace_id: s.trace_id.clone(),
            start_time: s.start_time,
            max_duration_ms: 0.0,
            span_count: 0,
            error_count: 0,
            root_span_name: None,
            root_service_name: None,
            root_kind: None,
            root_env: None,
            root_deployment_id: None,
            has_root: false,
        });

        entry.span_count += 1;
        if is_error {
            entry.error_count += 1;
        }
        if s.start_time < entry.start_time {
            entry.start_time = s.start_time;
        }
        if s.duration_ms > entry.max_duration_ms {
            entry.max_duration_ms = s.duration_ms;
        }
        if is_root && !entry.has_root {
            // First root span in this batch wins the trace's identity.
            entry.has_root = true;
            entry.root_span_name = Some(s.name.clone());
            entry.root_service_name = Some(s.resource.service_name.clone());
            entry.root_kind = Some(s.kind.to_string());
            entry.root_env = s.resource.deployment_environment.clone();
            entry.root_deployment_id = s.deployment_id;
        }
    }

    let mut deltas: Vec<TraceDelta> = by_trace.into_values().collect();
    deltas.sort_by(|a, b| {
        a.project_id
            .cmp(&b.project_id)
            .then_with(|| a.trace_id.cmp(&b.trace_id))
    });
    deltas
}

// ── Row parsers ─────────────────────────────────────────────────────

fn parse_span_row(row: &sea_orm::QueryResult) -> Result<SpanRecord, OtelError> {
    let attrs_json: serde_json::Value = row.try_get("", "attributes").unwrap_or_default();
    let events_json: serde_json::Value = row.try_get("", "events").unwrap_or_default();

    let attributes: std::collections::BTreeMap<String, String> =
        serde_json::from_value(attrs_json).unwrap_or_default();
    let events: Vec<SpanEvent> = serde_json::from_value(events_json).unwrap_or_default();

    let status_str: String = row.try_get("", "status_code").unwrap_or_default();
    let status_code = match status_str.as_str() {
        "OK" => SpanStatusCode::Ok,
        "ERROR" => SpanStatusCode::Error,
        _ => SpanStatusCode::Unset,
    };

    let kind_str: String = row.try_get("", "kind").unwrap_or_default();
    let kind = match kind_str.as_str() {
        "INTERNAL" => SpanKind::Internal,
        "SERVER" => SpanKind::Server,
        "CLIENT" => SpanKind::Client,
        "PRODUCER" => SpanKind::Producer,
        "CONSUMER" => SpanKind::Consumer,
        _ => SpanKind::Unspecified,
    };

    Ok(SpanRecord {
        project_id: row.try_get("", "project_id").unwrap_or(0),
        deployment_id: row.try_get("", "deployment_id").ok(),
        resource: ResourceInfo {
            service_name: row.try_get("", "service_name").unwrap_or_default(),
            service_version: row.try_get("", "service_version").ok(),
            deployment_environment: row.try_get("", "deployment_environment").ok(),
            attributes: std::collections::BTreeMap::new(),
        },
        trace_id: row.try_get("", "trace_id").unwrap_or_default(),
        span_id: row.try_get("", "span_id").unwrap_or_default(),
        parent_span_id: row.try_get("", "parent_span_id").ok(),
        name: row.try_get("", "name").unwrap_or_default(),
        kind,
        start_time: row.try_get("", "start_time").unwrap_or_default(),
        end_time: row.try_get("", "end_time").unwrap_or_default(),
        duration_ms: row.try_get("", "duration_ms").unwrap_or(0.0),
        status_code,
        status_message: row.try_get("", "status_message").unwrap_or_default(),
        attributes,
        events,
    })
}

fn parse_log_row(row: &sea_orm::QueryResult) -> Result<LogRecord, OtelError> {
    let attrs_json: serde_json::Value = row.try_get("", "attributes").unwrap_or_default();
    let attributes: std::collections::BTreeMap<String, String> =
        serde_json::from_value(attrs_json).unwrap_or_default();

    let severity_str: String = row.try_get("", "severity").unwrap_or_default();
    let severity = match severity_str.as_str() {
        "TRACE" => LogSeverity::Trace,
        "DEBUG" => LogSeverity::Debug,
        "INFO" => LogSeverity::Info,
        "WARN" => LogSeverity::Warn,
        "ERROR" => LogSeverity::Error,
        "FATAL" => LogSeverity::Fatal,
        _ => LogSeverity::Info,
    };

    Ok(LogRecord {
        project_id: row.try_get("", "project_id").unwrap_or(0),
        deployment_id: row.try_get("", "deployment_id").ok(),
        resource: ResourceInfo {
            service_name: row.try_get("", "service_name").unwrap_or_default(),
            service_version: row.try_get("", "service_version").ok(),
            deployment_environment: row.try_get("", "deployment_environment").ok(),
            attributes: std::collections::BTreeMap::new(),
        },
        timestamp: row.try_get("", "timestamp").unwrap_or_default(),
        observed_timestamp: row.try_get("", "observed_timestamp").unwrap_or_default(),
        severity,
        severity_text: row.try_get("", "severity_text").unwrap_or_default(),
        body: row.try_get("", "body").unwrap_or_default(),
        trace_id: row.try_get("", "trace_id").ok(),
        span_id: row.try_get("", "span_id").ok(),
        attributes,
    })
}

fn parse_insight_row(row: &sea_orm::QueryResult) -> Result<Insight, OtelError> {
    let anomaly_ids_json: serde_json::Value = row.try_get("", "anomaly_ids").unwrap_or_default();
    let anomaly_ids: Vec<i64> = serde_json::from_value(anomaly_ids_json).unwrap_or_default();

    let severity_str: String = row.try_get("", "severity").unwrap_or_default();
    let severity = match severity_str.as_str() {
        "critical" => InsightSeverity::Critical,
        "high" => InsightSeverity::High,
        "medium" => InsightSeverity::Medium,
        _ => InsightSeverity::Low,
    };

    let status_str: String = row.try_get("", "status").unwrap_or_default();
    let status = match status_str.as_str() {
        "resolved" => InsightStatus::Resolved,
        _ => InsightStatus::Active,
    };

    Ok(Insight {
        id: row.try_get("", "id").unwrap_or(0),
        project_id: row.try_get("", "project_id").unwrap_or(0),
        environment: row.try_get("", "environment").ok(),
        service_name: row.try_get("", "service_name").unwrap_or_default(),
        severity,
        status,
        title: row.try_get("", "title").unwrap_or_default(),
        description: row.try_get("", "description").unwrap_or_default(),
        metric_name: row.try_get("", "metric_name").ok(),
        correlated_deploy_id: row.try_get("", "correlated_deploy_id").ok(),
        anomaly_ids,
        started_at: row.try_get("", "started_at").unwrap_or_default(),
        resolved_at: row.try_get("", "resolved_at").ok(),
        created_at: row.try_get("", "created_at").unwrap_or_default(),
        updated_at: row.try_get("", "updated_at").unwrap_or_default(),
    })
}

fn parse_health_summary_row(row: &sea_orm::QueryResult) -> Result<HealthSummary, OtelError> {
    let status_str: String = row.try_get("", "status").unwrap_or_default();
    let status = match status_str.as_str() {
        "healthy" => HealthStatus::Healthy,
        "degraded" => HealthStatus::Degraded,
        "down" => HealthStatus::Down,
        _ => HealthStatus::Unknown,
    };

    Ok(HealthSummary {
        project_id: row.try_get("", "project_id").unwrap_or(0),
        environment_id: row.try_get("", "environment_id").ok(),
        service_name: row.try_get("", "service_name").unwrap_or_default(),
        status,
        uptime_pct: row.try_get("", "uptime_pct").unwrap_or(0.0),
        error_rate: row.try_get("", "error_rate").unwrap_or(0.0),
        p95_latency_ms: row.try_get("", "p95_latency_ms").unwrap_or(0.0),
        cpu_usage_pct: row.try_get("", "cpu_usage_pct").unwrap_or(0.0),
        memory_usage_pct: row.try_get("", "memory_usage_pct").unwrap_or(0.0),
        last_deploy_id: row.try_get("", "last_deploy_id").ok(),
        last_deploy_at: row.try_get("", "last_deploy_at").ok(),
        computed_at: row.try_get("", "computed_at").unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        ResourceInfo, SpanKind, SpanRecord, SpanStatusCode, TraceQuery, TraceSortField,
    };
    use chrono::TimeZone;
    use std::collections::BTreeMap;

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + secs, 0).single().unwrap()
    }

    /// Minimal span builder for fold tests.
    fn span(
        project_id: i32,
        trace_id: &str,
        span_id: &str,
        parent: Option<&str>,
        start_secs: i64,
        duration_ms: f64,
        status: SpanStatusCode,
    ) -> SpanRecord {
        SpanRecord {
            project_id,
            deployment_id: Some(7),
            resource: ResourceInfo {
                service_name: "api".to_string(),
                service_version: None,
                deployment_environment: Some("production".to_string()),
                attributes: BTreeMap::new(),
            },
            trace_id: trace_id.to_string(),
            span_id: span_id.to_string(),
            parent_span_id: parent.map(|p| p.to_string()),
            name: format!("op-{span_id}"),
            kind: SpanKind::Server,
            start_time: ts(start_secs),
            end_time: ts(start_secs) + chrono::Duration::milliseconds(duration_ms as i64),
            duration_ms,
            status_code: status,
            status_message: String::new(),
            attributes: BTreeMap::new(),
            events: vec![],
        }
    }

    fn base_query(project_id: i32) -> TraceQuery {
        TraceQuery {
            project_id,
            trace_id: None,
            service_name: None,
            status: None,
            min_duration_ms: None,
            start_time: None,
            end_time: None,
            environment_id: None,
            deployment_id: None,
            attributes: None,
            name_pattern: None,
            sort_by: TraceSortField::StartTime,
            sort_order: Default::default(),
            limit: None,
            offset: None,
        }
    }

    // ── fold_trace_deltas ────────────────────────────────────────────────

    #[test]
    fn fold_empty_batch_yields_no_deltas() {
        assert!(fold_trace_deltas(&[]).is_empty());
    }

    #[test]
    fn fold_groups_by_trace_and_counts_spans_and_errors() {
        // Two traces in one batch. Trace A: 3 spans, 1 error. Trace B: 1 span.
        let spans = vec![
            span(1, "A", "a1", None, 10, 100.0, SpanStatusCode::Ok),
            span(1, "A", "a2", Some("a1"), 11, 250.0, SpanStatusCode::Error),
            span(1, "A", "a3", Some("a1"), 12, 50.0, SpanStatusCode::Ok),
            span(1, "B", "b1", None, 20, 30.0, SpanStatusCode::Ok),
        ];
        let deltas = fold_trace_deltas(&spans);
        assert_eq!(deltas.len(), 2);

        // Deterministic order: A before B.
        let a = &deltas[0];
        assert_eq!(a.trace_id, "A");
        assert_eq!(a.span_count, 3);
        assert_eq!(a.error_count, 1);
        // start_time is the MIN (the root at +10s), duration the MAX (250ms).
        assert_eq!(a.start_time, ts(10));
        assert_eq!(a.max_duration_ms, 250.0);

        let b = &deltas[1];
        assert_eq!(b.trace_id, "B");
        assert_eq!(b.span_count, 1);
        assert_eq!(b.error_count, 0);
    }

    #[test]
    fn fold_root_span_wins_identity_even_when_not_first() {
        // The root (no parent) arrives AFTER a child in the batch; it must still
        // claim the trace's root identity, and a longer child must not.
        let mut root = span(1, "T", "root", None, 5, 80.0, SpanStatusCode::Ok);
        root.name = "GET /checkout".to_string();
        root.resource.service_name = "gateway".to_string();
        root.kind = SpanKind::Server;

        let child = span(1, "T", "child", Some("root"), 6, 900.0, SpanStatusCode::Ok);

        // child first, root second
        let deltas = fold_trace_deltas(&[child, root]);
        assert_eq!(deltas.len(), 1);
        let d = &deltas[0];
        assert!(d.has_root);
        assert_eq!(d.root_span_name.as_deref(), Some("GET /checkout"));
        assert_eq!(d.root_service_name.as_deref(), Some("gateway"));
        assert_eq!(d.root_kind.as_deref(), Some("SERVER")); // canonical uppercase
                                                            // duration is still the MAX across the trace (the 900ms child).
        assert_eq!(d.max_duration_ms, 900.0);
    }

    #[test]
    fn fold_batch_without_root_leaves_root_fields_unset() {
        // A batch of only child spans (late, before the root arrives). The
        // delta must carry has_root=false so the upsert won't clobber a stored
        // root identity with empty strings.
        let spans = vec![
            span(1, "T", "c1", Some("root"), 6, 100.0, SpanStatusCode::Ok),
            span(1, "T", "c2", Some("root"), 7, 120.0, SpanStatusCode::Error),
        ];
        let deltas = fold_trace_deltas(&spans);
        assert_eq!(deltas.len(), 1);
        let d = &deltas[0];
        assert!(!d.has_root);
        assert!(d.root_span_name.is_none());
        assert!(d.root_kind.is_none());
        // counts still accumulate for late spans
        assert_eq!(d.span_count, 2);
        assert_eq!(d.error_count, 1);
    }

    #[test]
    fn fold_separates_same_trace_id_across_projects() {
        // Same trace_id string under two different projects must NOT merge.
        let spans = vec![
            span(1, "shared", "s1", None, 10, 10.0, SpanStatusCode::Ok),
            span(2, "shared", "s2", None, 10, 10.0, SpanStatusCode::Ok),
        ];
        let deltas = fold_trace_deltas(&spans);
        assert_eq!(deltas.len(), 2);
        assert_eq!(deltas[0].project_id, 1);
        assert_eq!(deltas[1].project_id, 2);
    }

    // ── needs_span_level_filter (fast vs. fallback dispatch) ──────────────

    #[test]
    fn plain_list_query_uses_fast_path() {
        let mut q = base_query(1);
        q.service_name = Some("api".to_string());
        q.status = Some(SpanStatusCode::Error);
        q.min_duration_ms = Some(100.0);
        q.start_time = Some(ts(0));
        assert!(
            !needs_span_level_filter(&q),
            "service/status/duration/time filters all live on the summary table"
        );
    }

    #[test]
    fn attribute_filter_forces_span_fallback() {
        let mut q = base_query(1);
        let mut attrs = BTreeMap::new();
        attrs.insert("gen_ai.system".to_string(), "openai".to_string());
        q.attributes = Some(attrs);
        assert!(needs_span_level_filter(&q));
    }

    #[test]
    fn empty_attribute_map_stays_on_fast_path() {
        let mut q = base_query(1);
        q.attributes = Some(BTreeMap::new());
        assert!(
            !needs_span_level_filter(&q),
            "an empty attribute map is not a real filter"
        );
    }

    #[test]
    fn name_pattern_forces_span_fallback() {
        let mut q = base_query(1);
        q.name_pattern = Some("checkout".to_string());
        assert!(needs_span_level_filter(&q));

        // empty pattern is not a real filter
        q.name_pattern = Some(String::new());
        assert!(!needs_span_level_filter(&q));
    }

    // ── parse_span_kind (regression: uppercase stored values) ────────────

    #[test]
    fn parse_span_kind_matches_canonical_uppercase() {
        assert!(matches!(parse_span_kind("SERVER"), SpanKind::Server));
        assert!(matches!(parse_span_kind("CLIENT"), SpanKind::Client));
        assert!(matches!(parse_span_kind("PRODUCER"), SpanKind::Producer));
        assert!(matches!(parse_span_kind("CONSUMER"), SpanKind::Consumer));
        assert!(matches!(parse_span_kind("INTERNAL"), SpanKind::Internal));
        assert!(matches!(
            parse_span_kind("UNSPECIFIED"),
            SpanKind::Unspecified
        ));
    }

    #[test]
    fn parse_span_kind_round_trips_with_display() {
        // The fold stores `SpanKind::to_string()`; parse_span_kind must invert
        // it for every variant, or the list view shows the wrong kind.
        for k in [
            SpanKind::Server,
            SpanKind::Client,
            SpanKind::Producer,
            SpanKind::Consumer,
            SpanKind::Internal,
            SpanKind::Unspecified,
        ] {
            let stored = k.to_string();
            let parsed = parse_span_kind(&stored);
            assert_eq!(
                parsed.to_string(),
                stored,
                "round-trip failed for {k:?} (stored as {stored})"
            );
        }
    }

    #[test]
    fn parse_span_kind_unknown_falls_back_to_internal() {
        assert!(matches!(parse_span_kind("bogus"), SpanKind::Internal));
        // The OLD buggy capitalized form must now resolve to Internal (it's not
        // a stored value), proving we no longer accidentally match it.
        assert!(matches!(parse_span_kind("Server"), SpanKind::Internal));
    }

    // ── translate_bucket_interval_pg ────────────────────────────────────────

    #[test]
    fn translate_interval_known_units() {
        assert_eq!(translate_bucket_interval_pg("1 hour"), "1 hour");
        assert_eq!(translate_bucket_interval_pg("5 minutes"), "5 minute");
        assert_eq!(translate_bucket_interval_pg("30 seconds"), "30 second");
        assert_eq!(translate_bucket_interval_pg("1 day"), "1 day");
        assert_eq!(translate_bucket_interval_pg("2 weeks"), "2 week");
    }

    #[test]
    fn translate_interval_compact_form() {
        assert_eq!(translate_bucket_interval_pg("300s"), "300 second");
        assert_eq!(translate_bucket_interval_pg("5m"), "5 minute");
        assert_eq!(translate_bucket_interval_pg("1h"), "1 hour");
        assert_eq!(translate_bucket_interval_pg("2d"), "2 day");
    }

    #[test]
    fn translate_interval_unknown_falls_back_to_1hour() {
        assert_eq!(translate_bucket_interval_pg(""), "1 hour");
        assert_eq!(translate_bucket_interval_pg("bogus"), "1 hour");
        assert_eq!(translate_bucket_interval_pg("1 year"), "1 hour");
        // Extra tokens — injection attempt guard.
        assert_eq!(translate_bucket_interval_pg("1 hour; DROP TABLE"), "1 hour");
        // Count=0 is rejected.
        assert_eq!(translate_bucket_interval_pg("0 hours"), "1 hour");
    }

    // ── interval_seconds_pg ─────────────────────────────────────────────────

    #[test]
    fn interval_seconds_known_values() {
        assert_eq!(interval_seconds_pg("1 second"), 1);
        assert_eq!(interval_seconds_pg("1 minute"), 60);
        assert_eq!(interval_seconds_pg("1 hour"), 3600);
        assert_eq!(interval_seconds_pg("1 day"), 86_400);
        assert_eq!(interval_seconds_pg("1 week"), 604_800);
        assert_eq!(interval_seconds_pg("5 minute"), 300);
    }

    #[test]
    fn interval_seconds_unknown_falls_back_to_hour() {
        assert_eq!(interval_seconds_pg("bogus"), 3600);
        assert_eq!(interval_seconds_pg(""), 3600);
    }

    // ── validate_label_key ──────────────────────────────────────────────────

    #[test]
    fn validate_label_key_accepts_valid_keys() {
        assert!(validate_label_key("http.status_code").is_ok());
        assert!(validate_label_key("gen_ai.system").is_ok());
        assert!(validate_label_key("env").is_ok());
        assert!(validate_label_key("k8s:node").is_ok());
        assert!(validate_label_key("A-Za-z0-9_.:-").is_ok());
    }

    #[test]
    fn validate_label_key_rejects_bad_keys() {
        assert!(validate_label_key("").is_err());
        // Space is not allowed.
        assert!(validate_label_key("my key").is_err());
        // SQL injection attempt.
        assert!(validate_label_key("'; DROP TABLE otel_metrics; --").is_err());
        // Unicode not in the allowlist.
        assert!(validate_label_key("http.stätus").is_err());
    }

    // ── parse_jsonb arrays ──────────────────────────────────────────────────

    #[test]
    fn parse_jsonb_f64_array_basic() {
        let v = serde_json::json!([1.0, 2.5, 10.0]);
        let result = parse_jsonb_f64_array(Some(&v)).unwrap();
        assert_eq!(result, vec![1.0, 2.5, 10.0]);
    }

    #[test]
    fn parse_jsonb_f64_array_none_input() {
        assert!(parse_jsonb_f64_array(None).is_none());
    }

    #[test]
    fn parse_jsonb_u64_array_basic() {
        let v = serde_json::json!([0, 3, 7, 100]);
        let result = parse_jsonb_u64_array(Some(&v)).unwrap();
        assert_eq!(result, vec![0u64, 3, 7, 100]);
    }

    #[test]
    fn parse_jsonb_u64_array_none_input() {
        assert!(parse_jsonb_u64_array(None).is_none());
    }

    // ── histogram_quantile ──────────────────────────────────────────────────

    #[test]
    fn histogram_quantile_empty_or_zero_is_none() {
        assert!(histogram_quantile(0.95, &[], &[]).is_none());
        assert!(histogram_quantile(0.95, &[1.0, 2.0], &[]).is_none());
        // All-zero counts → no observations → None (caller falls back to mean).
        assert!(histogram_quantile(0.95, &[1.0, 2.0], &[0, 0, 0]).is_none());
    }

    #[test]
    fn histogram_quantile_interpolates_within_bucket() {
        // bounds (0,1],(1,2],(2,5],(5,+Inf); counts 10/10/10/0 → 30 obs.
        let bounds = [1.0, 2.0, 5.0];
        let counts = [10u64, 10, 10, 0];
        // p50 → rank 15 → falls in bucket (1,2], 5 into its 10 → 1 + 1*0.5 = 1.5
        let p50 = histogram_quantile(0.5, &bounds, &counts).unwrap();
        assert!((p50 - 1.5).abs() < 1e-9, "p50 = {p50}");
        // p90 → rank 27 → bucket (2,5], 7 into its 10 → 2 + 3*0.7 = 4.1
        let p90 = histogram_quantile(0.9, &bounds, &counts).unwrap();
        assert!((p90 - 4.1).abs() < 1e-9, "p90 = {p90}");
    }

    #[test]
    fn histogram_quantile_overflow_bucket_clamps_to_last_bound() {
        // Most mass in the +Inf overflow bucket: p99 can't interpolate past the
        // last finite bound, so it clamps there.
        let bounds = [1.0, 2.0];
        let counts = [1u64, 1, 100]; // (.,1],(1,2],(2,+Inf]
        let p99 = histogram_quantile(0.99, &bounds, &counts).unwrap();
        assert!((p99 - 2.0).abs() < 1e-9, "p99 = {p99}");
    }

    #[test]
    fn histogram_quantile_q1_is_top_bound() {
        let bounds = [1.0, 2.0, 5.0];
        let counts = [10u64, 10, 10]; // no overflow bucket stored
        let p100 = histogram_quantile(1.0, &bounds, &counts).unwrap();
        assert!((p100 - 5.0).abs() < 1e-9, "p100 = {p100}");
    }

    // ── series_json_key ─────────────────────────────────────────────────────

    #[test]
    fn series_json_key_none_yields_empty() {
        assert_eq!(series_json_key(&None), "");
    }

    #[test]
    fn series_json_key_null_yields_empty() {
        assert_eq!(series_json_key(&Some(serde_json::Value::Null)), "");
    }

    #[test]
    fn series_json_key_array_yields_json_string() {
        let v = serde_json::json!(["production", "api"]);
        let key = series_json_key(&Some(v));
        assert_eq!(key, r#"["production","api"]"#);
    }

    // ── build_metrics_where ─────────────────────────────────────────────────

    #[test]
    fn build_metrics_where_base_case() {
        let query = crate::types::MetricQuery {
            project_id: 42,
            ..Default::default()
        };
        let (sql, values, next) = build_metrics_where(&query);
        assert!(sql.contains("project_id = $1"));
        assert_eq!(values.len(), 1);
        assert_eq!(next, 2);
    }

    #[test]
    fn build_metrics_where_label_filters_use_containment() {
        let query = crate::types::MetricQuery {
            project_id: 1,
            label_filters: vec![("env".to_string(), "prod".to_string())],
            ..Default::default()
        };
        let (sql, _values, _next) = build_metrics_where(&query);
        // Must use JSONB containment, not key-value string comparison.
        assert!(sql.contains("attributes @>"), "expected @> in: {sql}");
        assert!(
            sql.contains("jsonb_build_object"),
            "expected jsonb_build_object in: {sql}"
        );
    }

    #[test]
    fn build_metrics_where_label_filters_param_count() {
        // 2 label filters → 4 extra params (k1, v1, k2, v2).
        let query = crate::types::MetricQuery {
            project_id: 1,
            label_filters: vec![
                ("env".to_string(), "prod".to_string()),
                ("svc".to_string(), "api".to_string()),
            ],
            ..Default::default()
        };
        let (_sql, values, next) = build_metrics_where(&query);
        // project_id(1) + k1(1) + v1(1) + k2(1) + v2(1) = 5 values
        assert_eq!(values.len(), 5);
        assert_eq!(next, 6);
    }

    /// When otel_trace_summaries has a row for the trace, the span scan must
    /// be bounded by the trace's time window so hypertable chunk exclusion
    /// applies (compressed chunks have no trace_id B-tree index).
    #[tokio::test]
    async fn get_trace_bounds_span_scan_when_summary_exists() {
        use sea_orm::{DatabaseBackend, MockDatabase};
        use std::collections::BTreeMap;

        let start = chrono::Utc.with_ymd_and_hms(2026, 7, 10, 10, 0, 0).unwrap();
        let summary_row: BTreeMap<&str, sea_orm::Value> = BTreeMap::from([
            ("start_time", start.into()),
            ("duration_ms", 1500.0_f64.into()),
        ]);

        let conn = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![summary_row]])
                .append_query_results([Vec::<BTreeMap<&str, sea_orm::Value>>::new()])
                .into_connection(),
        );
        let storage = TimescaleDbStorage::new(conn.clone(), None);

        let spans = storage.get_trace(7, "abc123").await.unwrap();
        assert!(spans.is_empty());

        drop(storage);
        let log = Arc::try_unwrap(conn)
            .unwrap_or_else(|_| panic!("connection still shared"))
            .into_transaction_log();
        assert_eq!(log.len(), 2, "summary lookup + bounded span query");
        let span_query = format!("{:?}", log[1]);
        assert!(
            span_query.contains("start_time >= $3"),
            "span query must be time-bounded, got: {span_query}"
        );
        assert!(span_query.contains("start_time <= $4"));
        // Window derived from the summary: start - 1 day .. start + duration + 1 day.
        assert!(
            span_query.contains("2026-07-09"),
            "lower bound must come from the summary window, got: {span_query}"
        );
        assert!(span_query.contains("2026-07-11"));
    }

    /// Without a summary row (ingest race, fail-soft summary upsert failure,
    /// or legacy spans predating summaries), get_trace must still bound the
    /// scan — to the recent hot window — and never scan the full retention
    /// range: on compressed chunks a trace_id lookup has no index to seek on,
    /// so an unbounded fallback would decompress the project's entire history
    /// for any unknown trace_id.
    #[tokio::test]
    async fn get_trace_falls_back_to_recent_window_without_summary() {
        use sea_orm::{DatabaseBackend, MockDatabase};
        use std::collections::BTreeMap;

        let conn = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([Vec::<BTreeMap<&str, sea_orm::Value>>::new()])
                .append_query_results([Vec::<BTreeMap<&str, sea_orm::Value>>::new()])
                .into_connection(),
        );
        let storage = TimescaleDbStorage::new(conn.clone(), None);

        let spans = storage.get_trace(7, "abc123").await.unwrap();
        assert!(spans.is_empty());

        drop(storage);
        let log = Arc::try_unwrap(conn)
            .unwrap_or_else(|_| panic!("connection still shared"))
            .into_transaction_log();
        assert_eq!(log.len(), 2, "summary lookup + fallback span query");
        let span_query = format!("{:?}", log[1]);
        assert!(
            span_query.contains("start_time >= $3") && span_query.contains("start_time <= $4"),
            "fallback query must be bounded to the recent window, got: {span_query}"
        );
    }
}
