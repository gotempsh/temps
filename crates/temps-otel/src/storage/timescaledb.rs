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
        self.batch_insert_spans(&spans).await
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
        let interval = query.bucket_interval.as_deref().unwrap_or("1 hour");
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

        let sql = format!(
            r#"
            SELECT bucket::timestamptz as bucket, avg_value, min_value, max_value, count
            FROM (
                SELECT
                    time_bucket('{interval}'::interval, timestamp) as bucket,
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

    async fn query_trace_summaries(&self, query: TraceQuery) -> StorageResult<Vec<TraceSummary>> {
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

                let kind = match kind_str.as_str() {
                    "Server" => crate::types::SpanKind::Server,
                    "Client" => crate::types::SpanKind::Client,
                    "Producer" => crate::types::SpanKind::Producer,
                    "Consumer" => crate::types::SpanKind::Consumer,
                    "Internal" => crate::types::SpanKind::Internal,
                    _ => crate::types::SpanKind::Internal,
                };

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

    async fn count_traces(&self, query: TraceQuery) -> StorageResult<u64> {
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
            let _ = param_idx;
        }

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
        let sql = r#"
            SELECT
                COALESCE((SELECT pg_total_relation_size('otel_metrics') *
                    (SELECT COUNT(*) FROM otel_metrics WHERE project_id = $1)::float /
                    GREATEST((SELECT COUNT(*) FROM otel_metrics), 1)::float
                ), 0)::bigint +
                COALESCE((SELECT pg_total_relation_size('otel_spans') *
                    (SELECT COUNT(*) FROM otel_spans WHERE project_id = $1)::float /
                    GREATEST((SELECT COUNT(*) FROM otel_spans), 1)::float
                ), 0)::bigint +
                COALESCE((SELECT pg_total_relation_size('otel_log_events') *
                    (SELECT COUNT(*) FROM otel_log_events WHERE project_id = $1)::float /
                    GREATEST((SELECT COUNT(*) FROM otel_log_events), 1)::float
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

    async fn apply_retention(&self, project_id: i32) -> StorageResult<u64> {
        let days = self.retention_days;

        let sql1 = format!(
            "DELETE FROM otel_metrics WHERE project_id = $1 AND timestamp < NOW() - INTERVAL '{days} days'"
        );
        let r1 = self
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql1,
                vec![project_id.into()],
            ))
            .await?;

        let sql2 = format!(
            "DELETE FROM otel_spans WHERE project_id = $1 AND start_time < NOW() - INTERVAL '{days} days'"
        );
        let r2 = self
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql2,
                vec![project_id.into()],
            ))
            .await?;

        let sql3 = format!(
            "DELETE FROM otel_log_events WHERE project_id = $1 AND timestamp < NOW() - INTERVAL '{days} days'"
        );
        let r3 = self
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql3,
                vec![project_id.into()],
            ))
            .await?;

        Ok(r1.rows_affected() + r2.rows_affected() + r3.rows_affected())
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
