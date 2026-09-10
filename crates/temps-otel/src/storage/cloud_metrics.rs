// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The Cloud half of the metric read path (ADR-043 §3 Phase C1).
//!
//! Modeled directly on [`crate::storage::cloud_spans::CloudTelemetrySpanSource`]
//! — same `clickhouse::Client` reuse pattern, same read-only/no-silent-fallback
//! rules, same wall-clock budget. See that module's docs for the reasoning that
//! is not repeated here.
//!
//! # The schema this reads (ADR-043 §5c)
//!
//! ```text
//! telemetry_metrics (
//!     project_ref     String,
//!     name            LowCardinality(String),
//!     ts              DateTime64(3),
//!     value           Float64,
//!     label_keys      Array(String),
//!     label_values    Array(String)
//! )
//! ```
//!
//! Unlike `otel_metrics`/`ChMetricRow` locally, Cloud's table carries only what
//! Phase C1 sends: a scalar value, no histogram/exponential-histogram/summary
//! fields, no `service_name`/`deployment_environment` columns of their own —
//! those travel as `(key, value)` pairs in `label_keys`/`label_values` **only
//! when the operator's metric label allowlist includes them**, which defaults
//! to empty (ADR-043 §3 Phase C1, same default-deny as spans). A caller who
//! filters or groups by `service_name`/`environment` therefore gets an honest
//! empty result rather than a wrong one when those keys are not allowlisted —
//! the read path cannot serve label data the write path was never told to
//! send.
//!
//! # What is intentionally not served from Cloud yet
//!
//! `query_metrics` refuses (rather than silently mis-answering) any request
//! for histogram summaries, quantiles, or `group_by` — the simple scalar
//! schema above has no columns to answer them from. This mirrors
//! `CloudTelemetrySpanSource::query_span_stats`'s precedent: an explicit,
//! actionable refusal beats a confidently wrong empty answer.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use temps_cloud_client::CloudLink;

use crate::error::{OtelError, StorageErrorKind};
use crate::storage::clickhouse::translate_bucket_interval;
use crate::storage::cloud_routed::CloudMetricSource;
use crate::storage::{BaselinePoint, MinuteAggregate, StorageResult};
use crate::types::{MetricAggregation, MetricBucket, MetricQuery};

/// Cloud-side table holding mirrored metric points. Must match what Temps
/// Cloud actually names it — see the module docs.
pub const CLOUD_METRICS_TABLE: &str = "telemetry_metrics";

/// Hard ceiling on rows returned/aggregated by one Cloud metric query. Same
/// reasoning as [`crate::storage::cloud_spans`]'s `MAX_ROWS`: Cloud bounds its
/// own query cost server-side, but nothing bounds what this side buffers out
/// of a successful response.
const MAX_ROWS: u64 = 5_000;

/// The Cloud-bound write projection of one metric point (ADR-043 §5c).
///
/// **Deliberately not [`crate::storage::clickhouse::ChMetricRow`].** ADR-043
/// §5c's `telemetry_metrics` table has six columns; `ChMetricRow` has 31 —
/// histogram buckets, exponential-histogram fields, exemplars, a dedup
/// `_version`, none of which the Cloud table holds. Sending `ChMetricRow`'s
/// column list to a six-column table would fail the names-and-types
/// validation ADR-043 §5d makes mandatory (or, absent that validation, insert
/// garbage). This struct *is* the allowlisted projection — the thing ADR-043
/// §2b point 4 says happens upstream of the outbox — built once, here, so
/// there is exactly one field list for what leaves the instance.
///
/// # Labels are empty until a metric label allowlist column exists
///
/// ADR-043 §3 Phase C1 specifies "the default allowlist for metrics is empty,
/// same default-deny as spans" — but unlike spans
/// (`projects.cloud_telemetry_attribute_allowlist`), no per-project metric
/// label allowlist column has shipped yet. Until one does,
/// [`project_cloud_metric_row`] always sends empty `label_keys`/`label_values`
/// — full default-deny rather than a partial, unenforceable allowlist. This
/// is the conservative direction (ships less, never more) and matches how
/// [`crate::services::otel_service::cloud_span`] behaves at `Metered`
/// fidelity before any attribute allowlist applies. Adding the column and
/// wiring it here is a follow-up, not a Phase C1 blocker for the *schema* —
/// but it does mean Cloud-primary metrics currently carry no labels.
#[derive(Debug, Clone, PartialEq, clickhouse::Row, serde::Serialize, serde::Deserialize)]
pub struct CloudMetricRow {
    pub project_ref: String,
    pub name: String,
    /// Milliseconds since the Unix epoch — the same encoding
    /// [`crate::services::otel_service::cloud_span`] uses for `ts_millis`, so
    /// the outbox payload never needs a `DateTime` (de)serializer.
    pub ts_millis: i64,
    pub value: f64,
    pub label_keys: Vec<String>,
    pub label_values: Vec<String>,
}

/// Project one metric point into the row Cloud accepts, scoping it by
/// `project_ref` — the same HMAC pseudonym spans use, computed by the same
/// function.
///
/// Returns `None` when the point cannot be projected at all:
/// - it has no scalar `value` — `MetricType::Histogram`/`ExponentialHistogram`/
///   `Summary` points carry their data in fields Cloud's scalar-only schema
///   (ADR-043 §5c) has no column for, and sending a fabricated `0.0` would be
///   a wrong answer, not a partial one (see the module docs' "what is
///   intentionally not served" note); or
/// - it cannot be scoped — telemetry switched off between the policy check
///   and here, or the link lost its credential — the same "not projectable,
///   caller stores locally instead" contract
///   [`crate::services::otel_service::cloud_span`] has for spans.
pub fn project_cloud_metric_row(
    link: &CloudLink,
    point: &crate::types::MetricPoint,
) -> Option<CloudMetricRow> {
    let value = point.value?;
    let project_ref = link
        .pseudonymize_telemetry_id("project", &point.project_id.to_string())
        .ok()?;
    Some(CloudMetricRow {
        project_ref,
        name: point.metric_name.clone(),
        ts_millis: point.timestamp.timestamp_millis(),
        value,
        // Default-deny: see the struct docs for why this is empty rather than
        // an allowlist-filtered subset of `point.attributes`.
        label_keys: Vec::new(),
        label_values: Vec::new(),
    })
}

/// Reads metrics back from Temps Cloud.
pub struct CloudTelemetryMetricSource {
    link: Arc<CloudLink>,
}

impl CloudTelemetryMetricSource {
    pub fn new(link: Arc<CloudLink>) -> Self {
        Self { link }
    }

    fn project_ref(&self, project_id: i32) -> StorageResult<String> {
        self.link
            .pseudonymize_telemetry_id("project", &project_id.to_string())
            .map_err(|error| OtelError::Storage {
                message: format!(
                    "Could not derive the Temps Cloud scoping key for project {project_id}: \
                     {error}. Cloud-held metrics for this project cannot be read until the link \
                     is healthy again."
                ),
                kind: StorageErrorKind::Precondition,
            })
    }

    fn client(&self) -> StorageResult<clickhouse::Client> {
        self.link
            .clickhouse_query_client()
            .map_err(|error| OtelError::Storage {
                message: format!("Temps Cloud telemetry read is unavailable: {error}"),
                kind: StorageErrorKind::Precondition,
            })
    }

    /// Run a Cloud read under the shared wall-clock budget. Deliberately does
    /// **not** fall back to local storage on failure — see
    /// `CloudTelemetrySpanSource::run`'s docs for why.
    async fn run<T, F>(&self, what: &str, query: F) -> StorageResult<T>
    where
        F: std::future::Future<Output = Result<T, clickhouse::error::Error>>,
    {
        match temps_cloud_client::query::within_query_budget(query).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(OtelError::Storage {
                message: format!("Temps Cloud rejected the {what} query: {error}"),
                kind: StorageErrorKind::ClickHouseOther,
            }),
            Err(error) => Err(OtelError::Storage {
                message: format!("Temps Cloud did not answer the {what} query in time: {error}"),
                kind: StorageErrorKind::ClickHouseTimeout,
            }),
        }
    }

    /// A metric-name/label filter that does not require histogram, quantile
    /// or grouping support Cloud's schema cannot answer.
    fn refuse_unsupported(&self, query: &MetricQuery) -> StorageResult<()> {
        if !query.group_by.is_empty() {
            return Err(OtelError::Validation {
                message: format!(
                    "Grouping metric queries by label (requested: {:?}) is not yet available for \
                     Cloud-primary projects. Temps Cloud's metric table stores one scalar value \
                     per point without a per-group series key. Set this project's analytics \
                     write mode back to `local` to use grouped metric charts for it.",
                    query.group_by
                ),
            });
        }
        if matches!(query.aggregation, MetricAggregation::Quantile(_)) {
            return Err(OtelError::Validation {
                message: "Quantile aggregation is not yet available for Cloud-primary metrics — \
                          Temps Cloud's metric table stores scalar points, not the histogram \
                          buckets a quantile needs. Set this project's analytics write mode back \
                          to `local` to use quantile charts for it."
                    .to_string(),
            });
        }
        Ok(())
    }

    /// `WHERE` clause + bind order shared by every query in this module.
    ///
    /// `project_ref` is deliberately not represented in `binds` — it is always
    /// the first bound value and [`bind_all`] binds it separately, so every
    /// entry in `binds` corresponds 1:1 with a `?` placeholder *after* the
    /// leading `project_ref = ?` clause.
    fn filters(&self, query: &MetricQuery) -> (String, Vec<Bound>) {
        let mut clauses = vec!["project_ref = ?".to_string()];
        let mut binds: Vec<Bound> = Vec::new();

        if let Some(name) = &query.metric_name {
            clauses.push("name = ?".into());
            binds.push(Bound::Str(name.clone()));
        }
        if let Some(start) = query.start_time {
            clauses.push("ts >= ?".into());
            binds.push(Bound::MillisI64(start.timestamp_millis()));
        }
        if let Some(end) = query.end_time {
            clauses.push("ts <= ?".into());
            binds.push(Bound::MillisI64(end.timestamp_millis()));
        }
        for (key, value) in &query.label_filters {
            clauses.push(
                "has(label_keys, ?) AND label_values[indexOf(label_keys, ?)] = ?".to_string(),
            );
            binds.push(Bound::Str(key.clone()));
            binds.push(Bound::Str(key.clone()));
            binds.push(Bound::Str(value.clone()));
        }
        if let Some(service_name) = &query.service_name {
            clauses.push(
                "has(label_keys, 'service.name') AND \
                 label_values[indexOf(label_keys, 'service.name')] = ?"
                    .to_string(),
            );
            binds.push(Bound::Str(service_name.clone()));
        }
        if let Some(environment) = &query.environment {
            clauses.push(
                "has(label_keys, 'deployment.environment') AND \
                 label_values[indexOf(label_keys, 'deployment.environment')] = ?"
                    .to_string(),
            );
            binds.push(Bound::Str(environment.clone()));
        }
        (clauses.join(" AND "), binds)
    }
}

/// A bound value, so `filters` can build one ordered list regardless of type.
enum Bound {
    Str(String),
    MillisI64(i64),
}

fn bind_all(
    mut cursor: clickhouse::query::Query,
    project_ref: &str,
    binds: &[Bound],
) -> clickhouse::query::Query {
    cursor = cursor.bind(project_ref);
    for bound in binds {
        cursor = match bound {
            Bound::Str(value) => cursor.bind(value),
            Bound::MillisI64(value) => cursor.bind(value),
        };
    }
    cursor
}

#[derive(Debug, clickhouse::Row, serde::Deserialize)]
struct CloudMetricBucketRow {
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    bucket: DateTime<Utc>,
    avg_value: f64,
    min_value: f64,
    max_value: f64,
    count: i64,
}

#[async_trait]
impl CloudMetricSource for CloudTelemetryMetricSource {
    async fn query_metrics(&self, query: MetricQuery) -> StorageResult<Vec<MetricBucket>> {
        self.refuse_unsupported(&query)?;
        let project_ref = self.project_ref(query.project_id)?;
        let client = self.client()?;
        let interval =
            translate_bucket_interval(query.bucket_interval.as_deref().unwrap_or("1 hour"));
        let (filter_sql, binds) = self.filters(&query);
        let sql = format!(
            "SELECT toStartOfInterval(ts, {interval}) AS bucket, \
                    avg(value) AS avg_value, min(value) AS min_value, max(value) AS max_value, \
                    count() AS count \
             FROM {CLOUD_METRICS_TABLE} WHERE {filter_sql} \
             GROUP BY bucket ORDER BY bucket ASC LIMIT {}",
            query.limit.unwrap_or(1000).clamp(1, MAX_ROWS),
        );
        let cursor = bind_all(client.query(&sql), &project_ref, &binds);
        let rows = self
            .run("metric bucket", cursor.fetch_all::<CloudMetricBucketRow>())
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| {
                MetricBucket::scalar(
                    row.bucket,
                    row.avg_value,
                    row.min_value,
                    row.max_value,
                    row.count,
                )
            })
            .collect())
    }

    async fn list_metric_names(&self, project_id: i32) -> StorageResult<Vec<String>> {
        let project_ref = self.project_ref(project_id)?;
        let client = self.client()?;
        let sql = format!(
            "SELECT DISTINCT name FROM {CLOUD_METRICS_TABLE} WHERE project_ref = ? \
             ORDER BY name LIMIT {MAX_ROWS}"
        );
        self.run(
            "metric names",
            client.query(&sql).bind(project_ref).fetch_all::<String>(),
        )
        .await
    }

    async fn list_metric_label_keys(
        &self,
        project_id: i32,
        metric_name: &str,
        start_time: DateTime<Utc>,
        end_time: DateTime<Utc>,
    ) -> StorageResult<Vec<String>> {
        let project_ref = self.project_ref(project_id)?;
        let client = self.client()?;
        let sql = format!(
            "SELECT DISTINCT arrayJoin(label_keys) AS key FROM {CLOUD_METRICS_TABLE} \
             WHERE project_ref = ? AND name = ? AND ts >= ? AND ts <= ? \
             ORDER BY key LIMIT {MAX_ROWS}"
        );
        self.run(
            "metric label keys",
            client
                .query(&sql)
                .bind(project_ref)
                .bind(metric_name)
                .bind(start_time.timestamp_millis())
                .bind(end_time.timestamp_millis())
                .fetch_all::<String>(),
        )
        .await
    }

    async fn list_metric_label_values(
        &self,
        project_id: i32,
        metric_name: &str,
        label_key: &str,
        start_time: DateTime<Utc>,
        end_time: DateTime<Utc>,
    ) -> StorageResult<Vec<String>> {
        let project_ref = self.project_ref(project_id)?;
        let client = self.client()?;
        let sql = format!(
            "SELECT DISTINCT label_values[indexOf(label_keys, ?)] AS value \
             FROM {CLOUD_METRICS_TABLE} \
             WHERE project_ref = ? AND name = ? AND ts >= ? AND ts <= ? \
               AND has(label_keys, ?) \
             ORDER BY value LIMIT {MAX_ROWS}"
        );
        self.run(
            "metric label values",
            client
                .query(&sql)
                .bind(label_key)
                .bind(project_ref)
                .bind(metric_name)
                .bind(start_time.timestamp_millis())
                .bind(end_time.timestamp_millis())
                .bind(label_key)
                .fetch_all::<String>(),
        )
        .await
    }

    async fn get_metric_baseline(
        &self,
        project_id: i32,
        service_name: &str,
        metric_name: &str,
        environment: Option<&str>,
        lookback_days: i32,
    ) -> StorageResult<Vec<BaselinePoint>> {
        let project_ref = self.project_ref(project_id)?;
        let client = self.client()?;
        let mut sql = format!(
            "SELECT toHour(ts) AS hour_of_day, toDayOfWeek(ts) AS day_of_week, \
                    avg(value) AS avg_value, stddevPop(value) AS stddev_value, count() AS sample_count \
             FROM {CLOUD_METRICS_TABLE} \
             WHERE project_ref = ? AND name = ? AND ts >= now() - INTERVAL ? DAY \
               AND has(label_keys, 'service.name') \
               AND label_values[indexOf(label_keys, 'service.name')] = ?"
        );
        if environment.is_some() {
            sql.push_str(
                " AND has(label_keys, 'deployment.environment') \
                  AND label_values[indexOf(label_keys, 'deployment.environment')] = ?",
            );
        }
        sql.push_str(" GROUP BY hour_of_day, day_of_week");

        #[derive(Debug, clickhouse::Row, serde::Deserialize)]
        struct Row {
            hour_of_day: u8,
            day_of_week: u8,
            avg_value: f64,
            stddev_value: f64,
            sample_count: i64,
        }

        let mut cursor = client
            .query(&sql)
            .bind(project_ref)
            .bind(metric_name)
            .bind(lookback_days)
            .bind(service_name);
        if let Some(environment) = environment {
            cursor = cursor.bind(environment);
        }
        let rows = self
            .run("metric baseline", cursor.fetch_all::<Row>())
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| BaselinePoint {
                hour_of_day: i32::from(row.hour_of_day),
                day_of_week: i32::from(row.day_of_week),
                avg_value: row.avg_value,
                stddev_value: row.stddev_value,
                sample_count: row.sample_count,
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
        let project_ref = self.project_ref(project_id)?;
        let client = self.client()?;
        let mut sql = format!(
            "SELECT toStartOfMinute(ts) AS bucket, avg(value) AS avg_value, count() AS count \
             FROM {CLOUD_METRICS_TABLE} \
             WHERE project_ref = ? AND name = ? AND ts >= now() - INTERVAL ? MINUTE \
               AND has(label_keys, 'service.name') \
               AND label_values[indexOf(label_keys, 'service.name')] = ?"
        );
        if environment.is_some() {
            sql.push_str(
                " AND has(label_keys, 'deployment.environment') \
                  AND label_values[indexOf(label_keys, 'deployment.environment')] = ?",
            );
        }
        sql.push_str(" GROUP BY bucket ORDER BY bucket ASC");

        #[derive(Debug, clickhouse::Row, serde::Deserialize)]
        struct Row {
            #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
            bucket: DateTime<Utc>,
            avg_value: f64,
            count: i64,
        }

        let mut cursor = client
            .query(&sql)
            .bind(project_ref)
            .bind(metric_name)
            .bind(minutes)
            .bind(service_name);
        if let Some(environment) = environment {
            cursor = cursor.bind(environment);
        }
        let rows = self
            .run("recent minute aggregates", cursor.fetch_all::<Row>())
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| MinuteAggregate {
                bucket: row.bucket,
                avg_value: row.avg_value,
                count: row.count,
            })
            .collect())
    }
}

// ── Outbox drain: entity_type = 'metric' (ADR-043 §2b/§4/§5a) ──────────────

/// Everything that can go wrong draining a batch of Cloud-primary metric
/// points out of the outbox and into Cloud.
#[derive(Debug, thiserror::Error)]
pub enum MetricDrainError {
    #[error(
        "Could not decode a Temps Cloud telemetry outbox row (id {outbox_id}) as a metric \
         point; dead-lettering it: {reason}"
    )]
    Undecodable { outbox_id: i64, reason: String },

    #[error("Could not build a Temps Cloud insert client: {source}")]
    Client {
        #[source]
        source: temps_cloud_client::CloudError,
    },

    #[error("Temps Cloud rejected an insert into {target_table} ({row_count} row(s)): {source}")]
    Insert {
        target_table: String,
        row_count: usize,
        #[source]
        source: clickhouse::error::Error,
    },
}

/// What one drain attempt against Cloud's insert surface did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricDrainOutcome {
    pub delivered_ids: Vec<i64>,
    pub dead_lettered_ids: Vec<i64>,
}

/// Claim up to `batch_size` pending metric rows, group them by
/// `target_table`, and insert each group into Cloud via
/// [`temps_cloud_client::CloudLink::clickhouse_insert_client`].
///
/// This is the concrete realisation of ADR-043 §2b's "one background task
/// drains the shared table in batches, groups a claimed batch by
/// `(entity_type, target_table)`, and issues one insert per group" for the
/// `metric` entity type specifically. The generic claim/settle mechanics live
/// in [`temps_cloud_client::TelemetryOutbox`] (shared with every future entity
/// type); only the concrete row type (`CloudMetricRow`) and its
/// deserialization are specific to this module, because
/// `Client::insert::<T>()` requires a concrete Rust type and
/// `temps-cloud-client` cannot depend on `temps-otel` to provide one.
///
/// A row that fails to decode as JSON is dead-lettered immediately rather
/// than retried — an undecodable row can never succeed. A row whose Cloud
/// insert fails is left `pending` (via `record_attempt_failure`, called by the
/// caller once this returns) so the shared retry/backoff curve applies.
pub async fn drain_metric_outbox_batch(
    link: &temps_cloud_client::CloudLink,
    outbox: &temps_cloud_client::TelemetryOutbox,
    batch_size: u32,
) -> Result<MetricDrainOutcome, MetricDrainError> {
    let claimed = outbox
        .claim(batch_size)
        .await
        .map_err(|source| MetricDrainError::Client {
            source: temps_cloud_client::CloudError::Rejected {
                detail: source.to_string(),
            },
        })?;

    let mut by_table: std::collections::BTreeMap<String, Vec<(i64, CloudMetricRow)>> =
        std::collections::BTreeMap::new();
    let mut dead_lettered_ids = Vec::new();

    for row in claimed {
        match serde_json::from_slice::<CloudMetricRow>(&row.row_bytes) {
            Ok(metric_row) => by_table
                .entry(row.target_table.clone())
                .or_default()
                .push((row.id, metric_row)),
            Err(error) => {
                tracing::error!(
                    outbox_id = row.id,
                    %error,
                    "Temps Cloud metric outbox row could not be decoded; dead-lettering it \
                     rather than blocking the queue behind it"
                );
                dead_lettered_ids.push(row.id);
            }
        }
    }
    if !dead_lettered_ids.is_empty() {
        let _ = outbox
            .dead_letter(
                &dead_lettered_ids,
                "outbox row could not be decoded as a metric point",
            )
            .await;
    }

    let mut delivered_ids = Vec::new();
    for (target_table, rows) in by_table {
        let client = link
            .clickhouse_insert_client()
            .map_err(|source| MetricDrainError::Client { source })?;
        let ids: Vec<i64> = rows.iter().map(|(id, _)| *id).collect();
        match insert_batch(&client, &target_table, &rows).await {
            Ok(()) => delivered_ids.extend(ids),
            Err(source) => {
                // Left `pending` (not dead-lettered): a rejected insert may be
                // transient (Cloud restart, network blip), so this batch stays
                // eligible for `claim()` again once `attempts` allows it,
                // exactly like `SpanOutbox`'s worker leaves a failed shipment
                // for the shared retry/backoff curve rather than discarding it.
                let reason = source.to_string();
                let _ = outbox.record_attempt_failure(&ids, &reason).await;
                return Err(MetricDrainError::Insert {
                    target_table,
                    row_count: rows.len(),
                    source,
                });
            }
        }
    }

    if !delivered_ids.is_empty() {
        outbox
            .mark_delivered(&delivered_ids)
            .await
            .map_err(|source| MetricDrainError::Client {
                source: temps_cloud_client::CloudError::Rejected {
                    detail: source.to_string(),
                },
            })?;
    }

    Ok(MetricDrainOutcome {
        delivered_ids,
        dead_lettered_ids,
    })
}

/// How long the worker sleeps after an empty claim, matching
/// [`temps_cloud_client::outbox_worker::IDLE_POLL_INTERVAL`] so the two
/// drain loops feel like one system to an operator watching logs.
pub use temps_cloud_client::outbox_worker::{BASE_BACKOFF, IDLE_POLL_INTERVAL, MAX_BACKOFF};

/// Runs [`drain_metric_outbox_batch`] until `cancel_rx` is signalled.
///
/// Same shape as `temps_cloud_client::outbox_worker::run` (drain until idle,
/// then poll; back off on failure up to [`MAX_BACKOFF`]) but specific to
/// metrics because the concrete insert type (`CloudMetricRow`) has to live in
/// this crate — see the module docs on why `temps-cloud-client` cannot own
/// this loop itself.
pub async fn run_metric_outbox_worker(
    link: Arc<temps_cloud_client::CloudLink>,
    outbox: Arc<temps_cloud_client::TelemetryOutbox>,
    mut cancel_rx: tokio::sync::watch::Receiver<bool>,
) {
    let mut backoff = BASE_BACKOFF;
    loop {
        if *cancel_rx.borrow() {
            return;
        }

        match drain_metric_outbox_batch(&link, &outbox, temps_cloud_client::OUTBOX_BATCH_SIZE).await
        {
            Ok(outcome)
                if outcome.delivered_ids.is_empty() && outcome.dead_lettered_ids.is_empty() =>
            {
                backoff = BASE_BACKOFF;
                tokio::select! {
                    _ = tokio::time::sleep(IDLE_POLL_INTERVAL) => {}
                    _ = cancel_rx.changed() => {}
                }
            }
            Ok(_) => {
                // Work was done; loop again immediately to drain until idle,
                // same rationale as the span worker's "drains until idle
                // instead" design (see `outbox_worker` module docs).
                backoff = BASE_BACKOFF;
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    backoff_secs = backoff.as_secs(),
                    "Temps Cloud metric outbox drain failed; backing off"
                );
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = cancel_rx.changed() => {}
                }
                backoff = (backoff.max(BASE_BACKOFF) * 2).min(MAX_BACKOFF);
            }
        }
    }
}

async fn insert_batch(
    client: &clickhouse::Client,
    target_table: &str,
    rows: &[(i64, CloudMetricRow)],
) -> Result<(), clickhouse::error::Error> {
    let mut insert = client.insert::<CloudMetricRow>(target_table).await?;
    for (_, row) in rows {
        insert.write(row).await?;
    }
    insert.end().await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `refuse_unsupported` needs no live link — construct an unenrolled one
    /// so the pure validation logic is testable without a Cloud tenant.
    fn source() -> CloudTelemetryMetricSource {
        let dir = tempfile::tempdir().expect("temp dir");
        let link = CloudLink::load(dir.path().to_path_buf(), "0.1.0-test");
        CloudTelemetryMetricSource::new(Arc::new(link))
    }

    #[test]
    fn a_quantile_request_is_refused_rather_than_answered_wrongly() {
        let query = MetricQuery {
            project_id: 7,
            aggregation: MetricAggregation::Quantile(0.95),
            ..Default::default()
        };
        let error = source()
            .refuse_unsupported(&query)
            .expect_err("Cloud's scalar schema cannot answer a quantile request");
        assert!(error.to_string().contains("Quantile"), "{error}");
        assert!(error.to_string().contains("local"), "{error}");
    }

    #[test]
    fn a_grouped_query_is_refused_and_names_the_requested_keys() {
        let query = MetricQuery {
            project_id: 7,
            group_by: vec!["service.name".to_string()],
            ..Default::default()
        };
        let error = source()
            .refuse_unsupported(&query)
            .expect_err("Cloud's scalar schema has no per-group series key");
        assert!(error.to_string().contains("service.name"), "{error}");
    }

    #[test]
    fn an_ungrouped_scalar_query_is_accepted() {
        let query = MetricQuery {
            project_id: 7,
            aggregation: MetricAggregation::Avg,
            ..Default::default()
        };
        assert!(source().refuse_unsupported(&query).is_ok());
    }

    #[test]
    fn every_bind_in_filters_has_exactly_one_placeholder() {
        let query = MetricQuery {
            project_id: 7,
            metric_name: Some("http.server.duration".into()),
            start_time: Some(Utc::now() - chrono::Duration::hours(1)),
            end_time: Some(Utc::now()),
            service_name: Some("api".into()),
            environment: Some("production".into()),
            label_filters: vec![("route".to_string(), "/health".to_string())],
            ..Default::default()
        };
        let (clause, binds) = source().filters(&query);
        // The leading `project_ref = ?` placeholder is bound separately by
        // `bind_all` and deliberately excluded from `binds` (see `filters`'s
        // docs); every placeholder after it must have exactly one entry.
        let placeholder_count = clause.matches('?').count();
        assert_eq!(
            placeholder_count,
            binds.len() + 1,
            "a mismatch here silently filters on the wrong column: {clause}"
        );
    }

    #[test]
    fn an_unfiltered_query_still_scopes_to_the_project() {
        let (clause, binds) = source().filters(&MetricQuery {
            project_id: 7,
            ..Default::default()
        });
        assert_eq!(clause, "project_ref = ?");
        assert!(binds.is_empty());
    }

    // ── drain_metric_outbox_batch ───────────────────────────────────────

    fn metric_outbox_claim_row(
        id: i64,
        project_id: i32,
        target_table: &str,
        row_bytes: Vec<u8>,
    ) -> std::collections::BTreeMap<&'static str, sea_orm::Value> {
        // Same shape as `TelemetryOutbox`'s own `claimed_generic_row_mock` —
        // duplicated here rather than exported cross-crate for tests-only
        // use, matching how other `MockDatabase` row builders in this
        // codebase stay local to the file whose decode path they exercise.
        let bytes_len = row_bytes.len() as i32;
        let mut row: std::collections::BTreeMap<&str, sea_orm::Value> =
            std::collections::BTreeMap::new();
        row.insert("id", id.into());
        row.insert("project_id", project_id.into());
        row.insert("target_table", Some(target_table.to_string()).into());
        row.insert("payload_row", Some(row_bytes).into());
        row.insert("payload_bytes", bytes_len.into());
        row.insert("enqueued_at", chrono::Utc::now().into());
        row
    }

    fn sample_metric_row() -> CloudMetricRow {
        CloudMetricRow {
            project_ref: "proj_ref_abc".to_string(),
            name: "http.server.duration".to_string(),
            ts_millis: 1_700_000_000_000,
            value: 12.5,
            label_keys: Vec::new(),
            label_values: Vec::new(),
        }
    }

    #[tokio::test]
    async fn a_row_that_fails_json_decoding_is_dead_lettered_without_an_insert_attempt() {
        // Garbage bytes can never succeed no matter how many times they are
        // retried, so this must not consume an insert attempt or block the
        // rows behind it — it goes straight to `dead_letter` instead of
        // `record_attempt_failure`.
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_results(vec![vec![metric_outbox_claim_row(
                    1,
                    9,
                    "otel_metrics",
                    b"not valid json".to_vec(),
                )]])
                // The `dead_letter` call issues two UPDATE statements
                // (`settle`'s state + settled_at, matching `SpanOutbox`'s
                // shape); MockDatabase only needs exec results for these.
                .append_exec_results(vec![
                    sea_orm::MockExecResult {
                        last_insert_id: 0,
                        rows_affected: 1,
                    },
                    sea_orm::MockExecResult {
                        last_insert_id: 0,
                        rows_affected: 1,
                    },
                ])
                .into_connection(),
        );
        let outbox = temps_cloud_client::TelemetryOutbox::new(
            db,
            temps_entities::cloud_telemetry_outbox::CloudTelemetryOutboxEntityType::Metric,
            1_000_000,
        );
        let dir = tempfile::tempdir().expect("temp dir");
        let link = temps_cloud_client::CloudLink::load(dir.path().to_path_buf(), "0.1.0-test");

        let outcome = drain_metric_outbox_batch(&link, &outbox, 10)
            .await
            .expect("an all-undecodable batch is not itself an error");

        assert_eq!(outcome.dead_lettered_ids, vec![1]);
        assert!(outcome.delivered_ids.is_empty());
    }

    #[tokio::test]
    async fn a_decodable_row_with_no_linked_cloud_account_surfaces_a_client_error() {
        // A row that decodes fine but has nowhere to ship (this instance was
        // never enrolled) must surface as an error the worker's backoff curve
        // can act on — never silently dropped and never dead-lettered, since
        // enrollment can be completed later and the row should still be
        // deliverable then.
        let row_bytes = serde_json::to_vec(&sample_metric_row()).expect("serialize test row");
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
                .append_query_results(vec![vec![metric_outbox_claim_row(
                    2,
                    9,
                    "otel_metrics",
                    row_bytes,
                )]])
                .into_connection(),
        );
        let outbox = temps_cloud_client::TelemetryOutbox::new(
            db,
            temps_entities::cloud_telemetry_outbox::CloudTelemetryOutboxEntityType::Metric,
            1_000_000,
        );
        let dir = tempfile::tempdir().expect("temp dir");
        let link = temps_cloud_client::CloudLink::load(dir.path().to_path_buf(), "0.1.0-test");

        let error = drain_metric_outbox_batch(&link, &outbox, 10)
            .await
            .expect_err("an unenrolled instance cannot build an insert client");

        assert!(
            matches!(error, MetricDrainError::Client { .. }),
            "expected a Client error, got {error:?}"
        );
    }
}
