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
    quota_bytes_per_project: u64,
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
            quota_bytes_per_project: 10 * 1024 * 1024 * 1024,
        }
    }

    /// Create a new storage backend with custom retention and quota settings.
    pub fn with_config(
        db: Arc<DatabaseConnection>,
        s3_client: Option<Arc<S3LogArchiver>>,
        retention_days: u32,
        quota_bytes_per_project: u64,
    ) -> Self {
        Self {
            db,
            s3_client,
            retention_days,
            quota_bytes_per_project,
        }
    }

    /// Execute a batch insert using raw SQL with parameter binding.
    async fn batch_insert_metrics(&self, points: &[MetricPoint]) -> StorageResult<u64> {
        if points.is_empty() {
            return Ok(0);
        }

        // Build batch INSERT with VALUES list
        let mut sql = String::from(
            "INSERT INTO otel_metrics (
                project_id, deployment_id, service_name, service_version,
                deployment_environment, metric_name, metric_type, unit,
                timestamp, value, histogram_count, histogram_sum,
                histogram_min, histogram_max, histogram_bounds,
                histogram_bucket_counts, attributes
            ) VALUES ",
        );

        let mut values: Vec<sea_orm::Value> = Vec::new();
        let mut param_idx = 1u32;

        for (i, p) in points.iter().enumerate() {
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

            let attrs_json = serde_json::to_value(&p.attributes).unwrap_or_default();
            let bounds_json = p
                .histogram_bounds
                .as_ref()
                .map(|b| serde_json::to_value(b).unwrap_or_default());
            let bucket_counts_json = p
                .histogram_bucket_counts
                .as_ref()
                .map(|c| serde_json::to_value(c).unwrap_or_default());

            values.extend_from_slice(&[
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

#[derive(Debug, FromQueryResult)]
struct MetricBucketRow {
    bucket: DateTime<Utc>,
    avg_value: f64,
    min_value: f64,
    max_value: f64,
    count: i64,
}

#[derive(Debug, FromQueryResult)]
struct MetricNameRow {
    metric_name: String,
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

    async fn query_metrics(&self, query: MetricQuery) -> StorageResult<Vec<MetricBucket>> {
        let interval = query
            .bucket_interval
            .as_deref()
            .unwrap_or("1 hour")
            .to_string();
        let limit = query.limit.unwrap_or(1000).min(10000);

        let mut where_clauses = vec!["project_id = $1".to_string()];
        let mut values: Vec<sea_orm::Value> = vec![query.project_id.into()];
        let mut param_idx = 2u32;

        if let Some(ref name) = query.metric_name {
            where_clauses.push(format!("metric_name = ${}", param_idx));
            values.push(name.clone().into());
            param_idx += 1;
        }
        if let Some(ref svc) = query.service_name {
            where_clauses.push(format!("service_name = ${}", param_idx));
            values.push(svc.clone().into());
            param_idx += 1;
        }
        if let Some(ref env) = query.environment {
            where_clauses.push(format!("deployment_environment = ${}", param_idx));
            values.push(env.clone().into());
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

        // Pass interval as parameterized value to prevent SQL injection
        let interval_param_idx = param_idx;
        values.push(interval.into());
        param_idx += 1;

        let sql = format!(
            r#"
            SELECT bucket::timestamptz as bucket, avg_value, min_value, max_value, count
            FROM (
                SELECT
                    time_bucket(${interval_param_idx}::interval, timestamp) as bucket,
                    AVG(value) as avg_value,
                    MIN(value) as min_value,
                    MAX(value) as max_value,
                    COUNT(*) as count
                FROM otel_metrics
                WHERE {where_sql}
                GROUP BY bucket
            ) sub
            ORDER BY bucket ASC
            LIMIT ${param_idx}
            "#
        );
        values.push((limit as i64).into());

        let rows = MetricBucketRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            values,
        ))
        .all(self.db.as_ref())
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| MetricBucket {
                bucket: r.bucket,
                avg_value: r.avg_value,
                min_value: r.min_value,
                max_value: r.max_value,
                count: r.count,
            })
            .collect())
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
        let sql = r#"
            SELECT project_id, deployment_id, service_name, service_version,
                   deployment_environment, trace_id, span_id, parent_span_id,
                   name, kind, start_time, end_time, duration_ms,
                   status_code, status_message, attributes, events
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

        let limit_bytes: u64 = self.quota_bytes_per_project;
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

/// Escape LIKE/ILIKE metacharacters in a user-supplied substring pattern.
///
/// PostgreSQL ILIKE uses backslash as the default escape character. We
/// escape:
///
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
}
