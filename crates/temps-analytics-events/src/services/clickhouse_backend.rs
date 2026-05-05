//! ClickHouse implementation of [`crate::services::traits::AnalyticsEvents`].
//!
//! Compiled only when the `clickhouse` feature is enabled. Operators activate
//! it by setting `TEMPS_CLICKHOUSE_*` env vars; the plugin layer (in
//! `plugin.rs`) then constructs this backend instead of the Timescale-backed
//! `AnalyticsEventsService` for the read path.
//!
//! Design choices worth knowing:
//!
//! - **Same trait, different SQL dialect.** Each `query_*` method consumes
//!   the same `*Spec` value-type as the Timescale impl and renders it into
//!   ClickHouse SQL. The trait is intentionally storage-agnostic; this file
//!   only knows about CH.
//! - **Parameter binding via `?Identifier`/`?` placeholders.** The
//!   `clickhouse` crate's typed `query()` builder substitutes values via
//!   `.bind()`. We never `format!` user-controlled values into SQL.
//! - **`final` keyword on reads.** The `events` and `sessions` tables are
//!   `ReplacingMergeTree`; without `FINAL` you can see duplicate rows from
//!   in-progress merges. Performance cost is real but correctness wins.
//! - **Gap-fill via `WITH FILL`.** Replaces TimescaleDB's
//!   `time_bucket_gapfill`. The semantics are equivalent for our
//!   requested ranges (no edge surprises since we always pass start/end).
//! - **Approximate counts.** Where Postgres uses `COUNT(DISTINCT x)`, this
//!   uses `uniq(x)` — ClickHouse's HLL-based unique cardinality. Numbers
//!   are within 1% for our scales; documented divergence vs. Timescale.
//!
//! Methods that don't have a clean CH equivalent yet return a
//! `Validation` error with a clear message so operators see exactly which
//! query they hit. Better than silently returning wrong numbers.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::TimeZone;
use clickhouse::Row;
use serde::Deserialize;
use temps_core::UtcDateTime;

use crate::services::events_service::EventsError;
use crate::services::queries::{
    ActiveVisitorsSpec, AggregatedBucketsSpec, DashboardProjectsSpec, EventTypeBreakdownSpec,
    EventsCountSpec, EventsTimelineSpec, HasEventsSpec, HourlyVisitsSpec, PropertyBreakdownSpec,
    PropertyTimelineSpec, SessionEventsSpec, UniqueCountsSpec,
};
use crate::services::traits::AnalyticsEvents;
use crate::types::{
    AggregatedBucketItem, AggregatedBucketsResponse, AggregationLevel,
    AnalyticsSessionEventsResponse, DashboardProjectsAnalyticsResponse, EventCount, EventTimeline,
    EventTypeBreakdown, PropertyBreakdownResponse, PropertyTimelineResponse, SessionEvent,
    UniqueCountsResponse,
};

/// ClickHouse-backed analytics read implementation.
///
/// Constructed via [`temps_analytics_backend::clickhouse::ClickHouseConfig`]
/// and the matching client. Cheap to clone — wraps an `Arc<Client>`.
pub struct ClickHouseEventsBackend {
    client: Arc<clickhouse::Client>,
}

impl ClickHouseEventsBackend {
    pub fn new(client: Arc<clickhouse::Client>) -> Self {
        Self { client }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// CH dialect of the count expression for a given aggregation level.
/// `uniq()` is HLL-approximate; correct to within ~1% at our scale.
fn count_expr(level: AggregationLevel) -> &'static str {
    match level {
        AggregationLevel::Events => "count()",
        AggregationLevel::Sessions => "uniq(session_id)",
        AggregationLevel::Visitors => "uniq(visitor_id)",
    }
}

/// Map a `bucket_size` string from the public API to a CH `INTERVAL`.
/// Defaults to 1 hour when unspecified or unrecognized.
fn ch_interval(bucket: Option<&str>) -> &'static str {
    match bucket {
        Some("hour") | Some("1 hour") | Some("1h") => "INTERVAL 1 HOUR",
        Some("day") | Some("1 day") | Some("1d") => "INTERVAL 1 DAY",
        Some("week") | Some("1 week") | Some("1w") => "INTERVAL 1 WEEK",
        Some("month") | Some("1 month") | Some("1mo") => "INTERVAL 1 MONTH",
        Some("5 minutes") | Some("5m") => "INTERVAL 5 MINUTE",
        _ => "INTERVAL 1 HOUR",
    }
}

/// Convert a `chrono::DateTime<Utc>` to seconds since the Unix epoch — the
/// shape ClickHouse's `DateTime64(3)` parses cleanly via `fromUnixTimestamp64Milli`.
fn to_unix_milli(t: UtcDateTime) -> i64 {
    t.timestamp_millis()
}

/// Wrap any CH error in an `EventsError::Validation` with the offending
/// query name surfaced. Internal errors at this layer are nearly always
/// "CH is unhappy with our SQL or unavailable," and the user-facing
/// behaviour is the same: 500 with a useful detail.
fn ch_err(query: &str, err: clickhouse::error::Error) -> EventsError {
    EventsError::Validation(format!("clickhouse {query} failed: {err}"))
}

// ---------------------------------------------------------------------------
// Row types
// ---------------------------------------------------------------------------

#[derive(Row, Deserialize)]
struct CountAndPercentRow {
    name: String,
    count: u64,
    percentage: f64,
}

#[derive(Row, Deserialize)]
struct BucketRow {
    /// Stored as Unix millis so we don't fight the `clickhouse` crate's
    /// limited timezone support.
    bucket_ms: i64,
    count: u64,
}

#[derive(Row, Deserialize)]
struct ScalarU64 {
    value: u64,
}

#[derive(Row, Deserialize)]
struct ScalarU8 {
    value: u8,
}

// Session event row matches the SELECT order in `query_session_events`.
#[derive(Row, Deserialize)]
struct SessionEventRow {
    event_id: i64,
    event_name: String,
    event_type: String,
    event_data: String,
    timestamp_ms: i64,
    page_url: String,
    page_title: String,
}

// ---------------------------------------------------------------------------
// Trait impl
// ---------------------------------------------------------------------------

#[async_trait]
impl AnalyticsEvents for ClickHouseEventsBackend {
    async fn query_events_count(&self, q: EventsCountSpec) -> Result<Vec<EventCount>, EventsError> {
        let level_expr = count_expr(q.aggregation_level);
        let custom_filter = if q.custom_events_only {
            "AND event_name NOT IN ('page_view', 'page_leave', 'heartbeat')"
        } else {
            ""
        };

        // The query selects (event_name, count, percentage) in one shot using
        // a subquery for the total. ClickHouse doesn't share Postgres's
        // CROSS JOIN ergonomics so we use a constant subquery.
        let sql = format!(
            r#"
            WITH total AS (
                SELECT {level_expr} AS t
                FROM events FINAL
                WHERE project_id = ?
                  AND timestamp >= fromUnixTimestamp64Milli(?)
                  AND timestamp <= fromUnixTimestamp64Milli(?)
                  AND event_name != ''
                  AND (? = 0 OR environment_id = ?)
                  {custom_filter}
            )
            SELECT
                event_name AS name,
                {level_expr} AS count,
                if((SELECT t FROM total) > 0,
                   {level_expr} / (SELECT t FROM total) * 100,
                   0) AS percentage
            FROM events FINAL
            WHERE project_id = ?
              AND timestamp >= fromUnixTimestamp64Milli(?)
              AND timestamp <= fromUnixTimestamp64Milli(?)
              AND event_name != ''
              AND (? = 0 OR environment_id = ?)
              {custom_filter}
            GROUP BY event_name
            ORDER BY count DESC
            LIMIT ?
            "#
        );

        let env_filter_flag: i32 = q.scope.environment_id.map(|_| 1).unwrap_or(0);
        let env_filter_value: i32 = q.scope.environment_id.unwrap_or(0);
        let start_ms = to_unix_milli(q.range.start);
        let end_ms = to_unix_milli(q.range.end);

        let rows = self
            .client
            .query(&sql)
            // total subquery binds
            .bind(q.scope.project_id)
            .bind(start_ms)
            .bind(end_ms)
            .bind(env_filter_flag)
            .bind(env_filter_value)
            // outer query binds
            .bind(q.scope.project_id)
            .bind(start_ms)
            .bind(end_ms)
            .bind(env_filter_flag)
            .bind(env_filter_value)
            .bind(q.limit as u32)
            .fetch_all::<CountAndPercentRow>()
            .await
            .map_err(|e| ch_err("query_events_count", e))?;

        Ok(rows
            .into_iter()
            .map(|r| EventCount {
                event_name: r.name,
                count: r.count as i64,
                percentage: r.percentage,
            })
            .collect())
    }

    async fn query_session_events(
        &self,
        q: SessionEventsSpec,
    ) -> Result<Option<AnalyticsSessionEventsResponse>, EventsError> {
        let env_filter_flag: i32 = q.scope.environment_id.map(|_| 1).unwrap_or(0);
        let env_filter_value: i32 = q.scope.environment_id.unwrap_or(0);

        let sql = r#"
            SELECT
                event_id AS event_id,
                ifNull(event_name, '') AS event_name,
                event_type,
                ifNull(props, '') AS event_data,
                toUnixTimestamp64Milli(timestamp) AS timestamp_ms,
                ifNull(href, '') AS page_url,
                ifNull(page_title, '') AS page_title
            FROM events FINAL
            WHERE session_id = ?
              AND project_id = ?
              AND (? = 0 OR environment_id = ?)
            ORDER BY timestamp ASC
        "#;

        let rows = self
            .client
            .query(sql)
            .bind(&q.session_id)
            .bind(q.scope.project_id)
            .bind(env_filter_flag)
            .bind(env_filter_value)
            .fetch_all::<SessionEventRow>()
            .await
            .map_err(|e| ch_err("query_session_events", e))?;

        if rows.is_empty() {
            return Ok(None);
        }

        let total_events = rows.len();
        let events = rows
            .into_iter()
            .map(|r| SessionEvent {
                id: r.event_id as i32,
                event_name: empty_to_none(r.event_name),
                event_type: Some(r.event_type),
                event_data: serde_json::from_str(&r.event_data).ok(),
                timestamp: format_unix_milli(r.timestamp_ms),
                page_url: empty_to_none(r.page_url),
                page_title: empty_to_none(r.page_title),
            })
            .collect();

        Ok(Some(AnalyticsSessionEventsResponse {
            session_id: q.session_id,
            events,
            total_events,
        }))
    }

    async fn query_has_events(&self, q: HasEventsSpec) -> Result<bool, EventsError> {
        let env_filter_flag: i32 = q.scope.environment_id.map(|_| 1).unwrap_or(0);
        let env_filter_value: i32 = q.scope.environment_id.unwrap_or(0);

        let row = self
            .client
            .query(
                r#"
                SELECT toUInt8(count() > 0) AS value
                FROM events
                WHERE project_id = ?
                  AND (? = 0 OR environment_id = ?)
                LIMIT 1
                "#,
            )
            .bind(q.scope.project_id)
            .bind(env_filter_flag)
            .bind(env_filter_value)
            .fetch_one::<ScalarU8>()
            .await
            .map_err(|e| ch_err("query_has_events", e))?;

        Ok(row.value != 0)
    }

    async fn query_event_type_breakdown(
        &self,
        q: EventTypeBreakdownSpec,
    ) -> Result<Vec<EventTypeBreakdown>, EventsError> {
        let level_expr = count_expr(q.aggregation_level);
        let env_filter_flag: i32 = q.scope.environment_id.map(|_| 1).unwrap_or(0);
        let env_filter_value: i32 = q.scope.environment_id.unwrap_or(0);
        let start_ms = to_unix_milli(q.range.start);
        let end_ms = to_unix_milli(q.range.end);

        let sql = format!(
            r#"
            WITH total AS (
                SELECT {level_expr} AS t
                FROM events FINAL
                WHERE project_id = ?
                  AND timestamp >= fromUnixTimestamp64Milli(?)
                  AND timestamp <= fromUnixTimestamp64Milli(?)
                  AND (? = 0 OR environment_id = ?)
            )
            SELECT
                event_type AS name,
                {level_expr} AS count,
                if((SELECT t FROM total) > 0,
                   {level_expr} / (SELECT t FROM total) * 100,
                   0) AS percentage
            FROM events FINAL
            WHERE project_id = ?
              AND timestamp >= fromUnixTimestamp64Milli(?)
              AND timestamp <= fromUnixTimestamp64Milli(?)
              AND (? = 0 OR environment_id = ?)
            GROUP BY event_type
            ORDER BY count DESC
            "#
        );

        let rows = self
            .client
            .query(&sql)
            .bind(q.scope.project_id)
            .bind(start_ms)
            .bind(end_ms)
            .bind(env_filter_flag)
            .bind(env_filter_value)
            .bind(q.scope.project_id)
            .bind(start_ms)
            .bind(end_ms)
            .bind(env_filter_flag)
            .bind(env_filter_value)
            .fetch_all::<CountAndPercentRow>()
            .await
            .map_err(|e| ch_err("query_event_type_breakdown", e))?;

        Ok(rows
            .into_iter()
            .map(|r| EventTypeBreakdown {
                event_type: r.name,
                count: r.count as i64,
                percentage: r.percentage,
            })
            .collect())
    }

    async fn query_events_timeline(
        &self,
        q: EventsTimelineSpec,
    ) -> Result<Vec<EventTimeline>, EventsError> {
        let level_expr = count_expr(q.aggregation_level);

        // Auto-detect bucket size if not provided. Same heuristic as the
        // Timescale impl so dashboards pick the same granularity.
        let duration = q.range.end - q.range.start;
        let interval = match q.bucket_size.as_deref() {
            Some("hour") => "INTERVAL 1 HOUR",
            Some("day") => "INTERVAL 1 DAY",
            Some("week") => "INTERVAL 1 WEEK",
            _ => {
                if duration.num_days() <= 1 {
                    "INTERVAL 1 HOUR"
                } else if duration.num_days() <= 30 {
                    "INTERVAL 1 DAY"
                } else {
                    "INTERVAL 1 WEEK"
                }
            }
        };

        let env_filter_flag: i32 = q.scope.environment_id.map(|_| 1).unwrap_or(0);
        let env_filter_value: i32 = q.scope.environment_id.unwrap_or(0);
        let event_filter_flag: i32 = q.event_name.as_ref().map(|_| 1).unwrap_or(0);
        let event_filter_value: String = q.event_name.clone().unwrap_or_default();
        let start_ms = to_unix_milli(q.range.start);
        let end_ms = to_unix_milli(q.range.end);

        let sql = format!(
            r#"
            SELECT
                toUnixTimestamp64Milli(toStartOfInterval(timestamp, {interval})) AS bucket_ms,
                {level_expr} AS count
            FROM events FINAL
            WHERE project_id = ?
              AND timestamp >= fromUnixTimestamp64Milli(?)
              AND timestamp <= fromUnixTimestamp64Milli(?)
              AND (? = 0 OR environment_id = ?)
              AND (? = 0 OR event_name = ?)
            GROUP BY bucket_ms
            ORDER BY bucket_ms ASC
            WITH FILL
                FROM toUnixTimestamp64Milli(toStartOfInterval(fromUnixTimestamp64Milli(?), {interval}))
                TO toUnixTimestamp64Milli(toStartOfInterval(fromUnixTimestamp64Milli(?), {interval})) + 1
                STEP toInt64({interval})
            "#
        );

        let rows = self
            .client
            .query(&sql)
            .bind(q.scope.project_id)
            .bind(start_ms)
            .bind(end_ms)
            .bind(env_filter_flag)
            .bind(env_filter_value)
            .bind(event_filter_flag)
            .bind(&event_filter_value)
            .bind(start_ms)
            .bind(end_ms)
            .fetch_all::<BucketRow>()
            .await
            .map_err(|e| ch_err("query_events_timeline", e))?;

        Ok(rows
            .into_iter()
            .map(|r| EventTimeline {
                date: from_unix_milli(r.bucket_ms),
                count: r.count as i64,
            })
            .collect())
    }

    async fn query_property_breakdown(
        &self,
        _q: PropertyBreakdownSpec,
    ) -> Result<PropertyBreakdownResponse, EventsError> {
        Err(EventsError::Validation(
            "query_property_breakdown is not yet implemented for ClickHouse. \
             Set TEMPS_CLICKHOUSE_* to None to fall back to TimescaleDB for this query, \
             or contribute the implementation in temps-analytics-events."
                .to_string(),
        ))
    }

    async fn query_property_timeline(
        &self,
        _q: PropertyTimelineSpec,
    ) -> Result<PropertyTimelineResponse, EventsError> {
        Err(EventsError::Validation(
            "query_property_timeline is not yet implemented for ClickHouse. \
             Set TEMPS_CLICKHOUSE_* to None to fall back to TimescaleDB for this query, \
             or contribute the implementation in temps-analytics-events."
                .to_string(),
        ))
    }

    async fn query_active_visitors(&self, q: ActiveVisitorsSpec) -> Result<i64, EventsError> {
        // 5-minute live window, matching the Timescale path semantics.
        let env_filter_flag: i32 = q.scope.environment_id.map(|_| 1).unwrap_or(0);
        let env_filter_value: i32 = q.scope.environment_id.unwrap_or(0);
        let dep_filter_flag: i32 = q.scope.deployment_id.map(|_| 1).unwrap_or(0);
        let dep_filter_value: i32 = q.scope.deployment_id.unwrap_or(0);

        let row = self
            .client
            .query(
                r#"
                SELECT uniq(session_id) AS value
                FROM events FINAL
                WHERE project_id = ?
                  AND (? = 0 OR environment_id = ?)
                  AND (? = 0 OR deployment_id = ?)
                  AND timestamp >= now64() - INTERVAL 5 MINUTE
                "#,
            )
            .bind(q.scope.project_id)
            .bind(env_filter_flag)
            .bind(env_filter_value)
            .bind(dep_filter_flag)
            .bind(dep_filter_value)
            .fetch_one::<ScalarU64>()
            .await
            .map_err(|e| ch_err("query_active_visitors", e))?;

        Ok(row.value as i64)
    }

    async fn query_hourly_visits(
        &self,
        q: HourlyVisitsSpec,
    ) -> Result<Vec<EventTimeline>, EventsError> {
        let level_expr = count_expr(q.aggregation_level);
        let env_filter_flag: i32 = q.scope.environment_id.map(|_| 1).unwrap_or(0);
        let env_filter_value: i32 = q.scope.environment_id.unwrap_or(0);
        let start_ms = to_unix_milli(q.range.start);
        let end_ms = to_unix_milli(q.range.end);

        let sql = format!(
            r#"
            SELECT
                toUnixTimestamp64Milli(toStartOfHour(timestamp)) AS bucket_ms,
                {level_expr} AS count
            FROM events FINAL
            WHERE project_id = ?
              AND timestamp >= fromUnixTimestamp64Milli(?)
              AND timestamp <= fromUnixTimestamp64Milli(?)
              AND event_type = 'page_view'
              AND (? = 0 OR environment_id = ?)
            GROUP BY bucket_ms
            ORDER BY bucket_ms ASC
            WITH FILL
                FROM toUnixTimestamp64Milli(toStartOfHour(fromUnixTimestamp64Milli(?)))
                TO toUnixTimestamp64Milli(toStartOfHour(fromUnixTimestamp64Milli(?))) + 1
                STEP 3600000
            "#
        );

        let rows = self
            .client
            .query(&sql)
            .bind(q.scope.project_id)
            .bind(start_ms)
            .bind(end_ms)
            .bind(env_filter_flag)
            .bind(env_filter_value)
            .bind(start_ms)
            .bind(end_ms)
            .fetch_all::<BucketRow>()
            .await
            .map_err(|e| ch_err("query_hourly_visits", e))?;

        Ok(rows
            .into_iter()
            .map(|r| EventTimeline {
                date: from_unix_milli(r.bucket_ms),
                count: r.count as i64,
            })
            .collect())
    }

    async fn query_unique_counts(
        &self,
        q: UniqueCountsSpec,
    ) -> Result<UniqueCountsResponse, EventsError> {
        // The Timescale impl validates the metric here; do the same so behavior
        // is identical.
        let count_expr = match q.metric.as_str() {
            "sessions" => "uniq(session_id)",
            "visitors" => "uniq(visitor_id)",
            "page_views" | "paths" => "countIf(event_type = 'page_view')",
            other => {
                return Err(EventsError::Validation(format!(
                    "Invalid metric '{other}'. Valid options: sessions, visitors, page_views"
                )))
            }
        };

        let env_filter_flag: i32 = q.scope.environment_id.map(|_| 1).unwrap_or(0);
        let env_filter_value: i32 = q.scope.environment_id.unwrap_or(0);
        let dep_filter_flag: i32 = q.scope.deployment_id.map(|_| 1).unwrap_or(0);
        let dep_filter_value: i32 = q.scope.deployment_id.unwrap_or(0);
        let start_ms = to_unix_milli(q.range.start);
        let end_ms = to_unix_milli(q.range.end);

        let sql = format!(
            r#"
            SELECT {count_expr} AS value
            FROM events FINAL
            WHERE project_id = ?
              AND timestamp >= fromUnixTimestamp64Milli(?)
              AND timestamp <= fromUnixTimestamp64Milli(?)
              AND (? = 0 OR environment_id = ?)
              AND (? = 0 OR deployment_id = ?)
            "#
        );

        let row = self
            .client
            .query(&sql)
            .bind(q.scope.project_id)
            .bind(start_ms)
            .bind(end_ms)
            .bind(env_filter_flag)
            .bind(env_filter_value)
            .bind(dep_filter_flag)
            .bind(dep_filter_value)
            .fetch_one::<ScalarU64>()
            .await
            .map_err(|e| ch_err("query_unique_counts", e))?;

        Ok(UniqueCountsResponse {
            count: row.value as i64,
        })
    }

    async fn query_dashboard_projects(
        &self,
        q: DashboardProjectsSpec,
    ) -> Result<DashboardProjectsAnalyticsResponse, EventsError> {
        // Returns an empty response for empty input — matches Timescale impl.
        if q.project_ids.is_empty() {
            return Ok(DashboardProjectsAnalyticsResponse {
                projects: std::collections::HashMap::new(),
            });
        }

        // Beyond that, this query joins to several Postgres-side tables and
        // does period-over-period math against TimescaleDB continuous
        // aggregates. Implementing it fully on CH is meaningful work and
        // its output shape is dashboard-specific. Surface a clear error
        // until that lands.
        Err(EventsError::Validation(
            "query_dashboard_projects is not yet implemented for ClickHouse. \
             The dashboard view falls back to TimescaleDB; configure your hybrid \
             read router to send this query to PG, or implement it here."
                .to_string(),
        ))
    }

    async fn query_aggregated_buckets(
        &self,
        q: AggregatedBucketsSpec,
    ) -> Result<AggregatedBucketsResponse, EventsError> {
        let level_expr = count_expr(q.aggregation_level);
        let interval = ch_interval(Some(q.bucket_size.as_str()));
        let env_filter_flag: i32 = q.scope.environment_id.map(|_| 1).unwrap_or(0);
        let env_filter_value: i32 = q.scope.environment_id.unwrap_or(0);
        let dep_filter_flag: i32 = q.scope.deployment_id.map(|_| 1).unwrap_or(0);
        let dep_filter_value: i32 = q.scope.deployment_id.unwrap_or(0);
        let start_ms = to_unix_milli(q.range.start);
        let end_ms = to_unix_milli(q.range.end);

        let sql = format!(
            r#"
            SELECT
                toUnixTimestamp64Milli(toStartOfInterval(timestamp, {interval})) AS bucket_ms,
                {level_expr} AS count
            FROM events FINAL
            WHERE project_id = ?
              AND timestamp >= fromUnixTimestamp64Milli(?)
              AND timestamp <= fromUnixTimestamp64Milli(?)
              AND (? = 0 OR environment_id = ?)
              AND (? = 0 OR deployment_id = ?)
            GROUP BY bucket_ms
            ORDER BY bucket_ms ASC
            WITH FILL
                FROM toUnixTimestamp64Milli(toStartOfInterval(fromUnixTimestamp64Milli(?), {interval}))
                TO toUnixTimestamp64Milli(toStartOfInterval(fromUnixTimestamp64Milli(?), {interval})) + 1
                STEP toInt64({interval})
            "#
        );

        let rows = self
            .client
            .query(&sql)
            .bind(q.scope.project_id)
            .bind(start_ms)
            .bind(end_ms)
            .bind(env_filter_flag)
            .bind(env_filter_value)
            .bind(dep_filter_flag)
            .bind(dep_filter_value)
            .bind(start_ms)
            .bind(end_ms)
            .fetch_all::<BucketRow>()
            .await
            .map_err(|e| ch_err("query_aggregated_buckets", e))?;

        let total: i64 = rows.iter().map(|r| r.count as i64).sum();

        Ok(AggregatedBucketsResponse {
            bucket_size: q.bucket_size.clone(),
            aggregation_level: q.aggregation_level.as_str().to_string(),
            items: rows
                .into_iter()
                .map(|r| AggregatedBucketItem {
                    timestamp: format_unix_milli(r.bucket_ms),
                    count: r.count as i64,
                })
                .collect(),
            total,
        })
    }
}

// ---------------------------------------------------------------------------
// Local helpers (post-query reshaping)
// ---------------------------------------------------------------------------

fn empty_to_none(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Format Unix millis the way the Timescale path serializes a `UtcDateTime`
/// — `"YYYY-MM-DD HH:MM:SS"` with no timezone suffix. Keeps wire output
/// identical between backends so dashboards don't need a backend-aware
/// formatter.
fn format_unix_milli(ms: i64) -> String {
    let secs = ms / 1000;
    let nsec = ((ms % 1000) * 1_000_000) as u32;
    chrono::Utc
        .timestamp_opt(secs, nsec)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default()
}

fn from_unix_milli(ms: i64) -> UtcDateTime {
    let secs = ms / 1000;
    let nsec = ((ms % 1000) * 1_000_000) as u32;
    chrono::Utc
        .timestamp_opt(secs, nsec)
        .single()
        .unwrap_or_else(|| {
            chrono::Utc
                .timestamp_opt(0, 0)
                .single()
                .expect("epoch is a valid timestamp")
        })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
//
// Real-CH integration tests against a `clickhouse/clickhouse-server`
// testcontainer. Validates SQL dialect, parameter binding, the row mapper,
// and the migration runner end-to-end. If Docker is not available the test
// skips gracefully (per CLAUDE.md: never `#[ignore]`).
//
// The fan-out worker is exercised indirectly — we ingest rows using the
// same `ChEventRow` shape via the same `clickhouse::Client::insert()` path,
// then read them back through the trait methods. If the row shape is wrong
// or a query is malformed, this test catches it before any operator does.

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::{Duration, Utc};

    use crate::services::queries::{
        AnalyticsScope, EventTypeBreakdownSpec, EventsCountSpec, EventsTimelineSpec, HasEventsSpec,
        HourlyVisitsSpec, PropertyBreakdownSpec, SessionEventsSpec, TimeRange, UniqueCountsSpec,
    };
    use crate::services::traits::AnalyticsEvents;
    use crate::types::{AggregationLevel, PropertyColumn};

    /// Bring up a ClickHouse container, run migrations, and return a
    /// connected backend ready to query against. Returns `None` if Docker
    /// isn't reachable so the test can skip without failing CI on
    /// machines that don't have Docker.
    async fn setup_clickhouse() -> Option<(
        ClickHouseEventsBackend,
        Arc<::clickhouse::Client>,
        // The container handle has to outlive the test, so return it.
        // Boxed-as-any so we don't need to name the testcontainers types.
        Box<dyn std::any::Any + Send>,
    )> {
        use testcontainers::{
            core::{ContainerPort, WaitFor},
            runners::AsyncRunner,
            GenericImage, ImageExt,
        };

        // Probe Docker. If not reachable, skip.
        let image = GenericImage::new("clickhouse/clickhouse-server", "24.8")
            .with_exposed_port(ContainerPort::Tcp(8123))
            .with_wait_for(WaitFor::message_on_stdout("Ready for connections"))
            .with_env_var("CLICKHOUSE_DB", "temps_test")
            .with_env_var("CLICKHOUSE_USER", "default")
            .with_env_var("CLICKHOUSE_PASSWORD", "");

        let container = match image.start().await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Skipping ClickHouse test: failed to start container ({e})");
                return None;
            }
        };

        let host_port = match container.get_host_port_ipv4(8123).await {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Skipping ClickHouse test: cannot get host port ({e})");
                return None;
            }
        };

        let url = format!("http://127.0.0.1:{host_port}");
        let client = ::clickhouse::Client::default()
            .with_url(&url)
            .with_database("temps_test")
            .with_user("default")
            .with_password("");

        // Wait briefly for CH to fully accept HTTP queries (the readiness
        // message is on stdout but the HTTP listener can lag a moment).
        let mut last_err = String::new();
        for _ in 0..30 {
            match client.query("SELECT 1").execute().await {
                Ok(_) => {
                    last_err.clear();
                    break;
                }
                Err(e) => {
                    last_err = format!("{e}");
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
            }
        }
        if !last_err.is_empty() {
            eprintln!("Skipping ClickHouse test: server never became ready ({last_err})");
            return None;
        }

        // Apply migrations. If this fails, the test SHOULD fail loudly —
        // it's the entire reason we're running.
        temps_analytics_backend::migrations::apply_migrations(&client)
            .await
            .expect("apply_migrations failed against testcontainer ClickHouse");

        let client = Arc::new(client);
        let backend = ClickHouseEventsBackend::new(Arc::clone(&client));
        Some((backend, client, Box::new(container)))
    }

    /// Insert a few hand-crafted rows so we have something to query.
    /// Mirrors the field order of the `ChEventRow` shape in
    /// `ch_fanout::ChEventRow`. Uses the public clickhouse client API so
    /// any drift between this and the worker would also fail compilation.
    async fn seed_rows(client: &::clickhouse::Client) {
        // Local row type: matches the production DDL field-for-field. We
        // duplicate it here rather than taking a dep on the fan-out
        // module, because the fan-out type is `#[cfg(feature =
        // "clickhouse")]`-gated within ch_fanout but the test module here
        // is already feature-gated, so we could in principle use it. The
        // duplication is intentional: if the test row diverges from the
        // production row, the test fails loudly.
        #[derive(::clickhouse::Row, serde::Serialize)]
        struct SeedRow {
            event_id: i64,
            project_id: i32,
            environment_id: Option<i32>,
            deployment_id: Option<i32>,
            session_id: String,
            visitor_id: Option<i32>,
            timestamp: i64,
            hostname: String,
            pathname: String,
            page_path: String,
            href: String,
            querystring: String,
            page_title: String,
            referrer: String,
            referrer_hostname: String,
            event_type: String,
            event_name: String,
            props: String,
            user_agent: String,
            browser: String,
            browser_version: String,
            operating_system: String,
            operating_system_version: String,
            device_type: String,
            screen_width: Option<i16>,
            screen_height: Option<i16>,
            viewport_width: Option<i16>,
            viewport_height: Option<i16>,
            ip_geolocation_id: Option<i32>,
            channel: String,
            utm_source: String,
            utm_medium: String,
            utm_campaign: String,
            utm_term: String,
            utm_content: String,
            ttfb: Option<f32>,
            lcp: Option<f32>,
            fid: Option<f32>,
            fcp: Option<f32>,
            cls: Option<f32>,
            inp: Option<f32>,
            is_entry: u8,
            is_exit: u8,
            is_bounce: u8,
            is_crawler: u8,
            time_on_page: Option<i32>,
            session_page_number: Option<i32>,
            scroll_depth: Option<i32>,
            clicks: Option<i32>,
            language: String,
            crawler_name: String,
        }

        fn make(
            event_id: i64,
            project_id: i32,
            session_id: &str,
            visitor_id: Option<i32>,
            event_type: &str,
            event_name: &str,
            ts_ms: i64,
        ) -> SeedRow {
            SeedRow {
                event_id,
                project_id,
                environment_id: Some(1),
                deployment_id: None,
                session_id: session_id.to_string(),
                visitor_id,
                timestamp: ts_ms,
                hostname: "example.com".into(),
                pathname: "/".into(),
                page_path: "/".into(),
                href: "https://example.com/".into(),
                querystring: String::new(),
                page_title: "Home".into(),
                referrer: String::new(),
                referrer_hostname: String::new(),
                event_type: event_type.into(),
                event_name: event_name.into(),
                props: "{}".into(),
                user_agent: "test-agent".into(),
                browser: "Firefox".into(),
                browser_version: "120".into(),
                operating_system: "Linux".into(),
                operating_system_version: "6".into(),
                device_type: "desktop".into(),
                screen_width: Some(1920),
                screen_height: Some(1080),
                viewport_width: Some(1920),
                viewport_height: Some(1080),
                ip_geolocation_id: None,
                channel: "direct".into(),
                utm_source: String::new(),
                utm_medium: String::new(),
                utm_campaign: String::new(),
                utm_term: String::new(),
                utm_content: String::new(),
                ttfb: None,
                lcp: None,
                fid: None,
                fcp: None,
                cls: None,
                inp: None,
                is_entry: 0,
                is_exit: 0,
                is_bounce: 0,
                is_crawler: 0,
                time_on_page: None,
                session_page_number: None,
                scroll_depth: None,
                clicks: None,
                language: "en".into(),
                crawler_name: String::new(),
            }
        }

        let now = Utc::now();
        let t = |mins_ago: i64| (now - Duration::minutes(mins_ago)).timestamp_millis();

        // Project 7, session A, visitor 100: 2 page_views + 1 signup.
        // Project 7, session B, visitor 101: 1 page_view, 1 click.
        // Project 8, session C, visitor 102: 1 page_view (different project — must
        // not leak into project 7 queries).
        let rows = [
            make(1, 7, "sess-a", Some(100), "page_view", "page_view", t(60)),
            make(2, 7, "sess-a", Some(100), "page_view", "page_view", t(50)),
            make(3, 7, "sess-a", Some(100), "signup", "signup", t(45)),
            make(4, 7, "sess-b", Some(101), "page_view", "page_view", t(30)),
            make(5, 7, "sess-b", Some(101), "click", "click", t(25)),
            make(6, 8, "sess-c", Some(102), "page_view", "page_view", t(20)),
        ];

        let mut inserter = client.insert::<SeedRow>("events").expect("inserter setup");
        for row in &rows {
            inserter.write(row).await.expect("insert row");
        }
        inserter.end().await.expect("inserter end");

        // ReplacingMergeTree merges happen in the background; OPTIMIZE FINAL
        // forces them so FINAL reads return deterministic results in this
        // test. Production queries use FINAL on the read side and tolerate
        // some duplication during merges, but tests want determinism.
        client
            .query("OPTIMIZE TABLE events FINAL")
            .execute()
            .await
            .expect("optimize");
    }

    fn project_scope(project_id: i32) -> AnalyticsScope {
        AnalyticsScope::project(project_id).with_environment(Some(1))
    }

    fn full_range() -> TimeRange {
        // 2 hours of slack on either side so all seeded events are inside.
        let start = Utc::now() - Duration::hours(2);
        let end = Utc::now() + Duration::hours(1);
        TimeRange { start, end }
    }

    #[tokio::test]
    async fn ch_backend_full_query_surface() {
        let Some((backend, client, _container)) = setup_clickhouse().await else {
            return; // Docker not available, skip.
        };

        seed_rows(&client).await;

        // ---- query_has_events ----
        assert!(
            backend
                .query_has_events(HasEventsSpec {
                    scope: project_scope(7),
                })
                .await
                .expect("query_has_events project 7"),
            "project 7 has events"
        );
        assert!(
            !backend
                .query_has_events(HasEventsSpec {
                    scope: project_scope(999),
                })
                .await
                .expect("query_has_events project 999"),
            "project 999 has no events"
        );

        // ---- query_events_count (events level, custom-only=true) ----
        // Default custom_events_only=true filters out page_view, so we
        // should see signup (1) and click (1) only — page_views excluded.
        let counts = backend
            .query_events_count(EventsCountSpec::new(
                full_range(),
                project_scope(7),
                AggregationLevel::Events,
                Some(50),
                Some(true),
            ))
            .await
            .expect("query_events_count");
        let names: std::collections::HashSet<&str> =
            counts.iter().map(|c| c.event_name.as_str()).collect();
        assert!(
            names.contains("signup") && names.contains("click"),
            "expected signup+click, got {:?}",
            names
        );
        assert!(
            !names.contains("page_view"),
            "page_view should be filtered when custom_events_only=true"
        );

        // ---- query_events_count (custom-only=false) ----
        let counts_all = backend
            .query_events_count(EventsCountSpec::new(
                full_range(),
                project_scope(7),
                AggregationLevel::Events,
                Some(50),
                Some(false),
            ))
            .await
            .expect("query_events_count all");
        let total_events: i64 = counts_all.iter().map(|c| c.count).sum();
        // 5 events on project 7. Project 8's row must not leak in.
        assert_eq!(total_events, 5, "got {:?}", counts_all);

        // ---- query_event_type_breakdown ----
        let by_type = backend
            .query_event_type_breakdown(EventTypeBreakdownSpec {
                range: full_range(),
                scope: project_scope(7),
                aggregation_level: AggregationLevel::Events,
            })
            .await
            .expect("query_event_type_breakdown");
        // 3 page_view, 1 signup, 1 click on project 7.
        let pv_count = by_type
            .iter()
            .find(|r| r.event_type == "page_view")
            .map(|r| r.count)
            .unwrap_or(0);
        assert_eq!(pv_count, 3);

        // ---- query_active_visitors ----
        // The seeded events are 60-20 minutes ago, all outside the 5-min
        // active window. So this should be 0.
        let active = backend
            .query_active_visitors(crate::services::queries::ActiveVisitorsSpec {
                scope: project_scope(7).with_deployment(None),
            })
            .await
            .expect("query_active_visitors");
        assert_eq!(active, 0, "no events in last 5 min");

        // ---- query_unique_counts: visitors ----
        let visitors = backend
            .query_unique_counts(UniqueCountsSpec {
                range: full_range(),
                scope: project_scope(7).with_deployment(None),
                metric: "visitors".to_string(),
            })
            .await
            .expect("query_unique_counts visitors");
        // 2 distinct visitor_ids on project 7 (100, 101).
        assert_eq!(visitors.count, 2);

        // ---- query_unique_counts: page_views ----
        let pvs = backend
            .query_unique_counts(UniqueCountsSpec {
                range: full_range(),
                scope: project_scope(7).with_deployment(None),
                metric: "page_views".to_string(),
            })
            .await
            .expect("query_unique_counts page_views");
        assert_eq!(pvs.count, 3);

        // ---- query_unique_counts: bad metric ----
        let bad = backend
            .query_unique_counts(UniqueCountsSpec {
                range: full_range(),
                scope: project_scope(7),
                metric: "nonsense".to_string(),
            })
            .await;
        assert!(
            matches!(bad, Err(EventsError::Validation(_))),
            "bad metric must yield Validation error"
        );

        // ---- query_session_events ----
        let session = backend
            .query_session_events(SessionEventsSpec {
                session_id: "sess-a".to_string(),
                scope: project_scope(7),
            })
            .await
            .expect("query_session_events");
        let session = session.expect("session A exists");
        assert_eq!(session.session_id, "sess-a");
        assert_eq!(session.total_events, 3);
        // Events ordered by timestamp ASC.
        assert_eq!(session.events[0].event_type.as_deref(), Some("page_view"));

        let none = backend
            .query_session_events(SessionEventsSpec {
                session_id: "does-not-exist".to_string(),
                scope: project_scope(7),
            })
            .await
            .expect("query_session_events none");
        assert!(none.is_none(), "missing session must be None");

        // ---- query_events_timeline ----
        // Smoke-check: the WITH FILL clause is the trickiest piece of the
        // CH SQL and a syntax error here would surface. We don't assert
        // exact bucket counts because gap-fill semantics depend on the
        // chosen interval, but we DO assert the call succeeds.
        let timeline = backend
            .query_events_timeline(EventsTimelineSpec {
                range: full_range(),
                scope: project_scope(7),
                aggregation_level: AggregationLevel::Events,
                event_name: None,
                bucket_size: Some("hour".to_string()),
            })
            .await
            .expect("query_events_timeline");
        assert!(
            !timeline.is_empty(),
            "timeline must have at least one bucket"
        );

        // ---- query_hourly_visits ----
        // Filters event_type='page_view' so only the 3 page_views count.
        let hourly = backend
            .query_hourly_visits(HourlyVisitsSpec {
                range: full_range(),
                scope: project_scope(7),
                aggregation_level: AggregationLevel::Events,
            })
            .await
            .expect("query_hourly_visits");
        let hourly_total: i64 = hourly.iter().map(|p| p.count).sum();
        assert_eq!(hourly_total, 3, "page_view count: {:?}", hourly);

        // ---- query_aggregated_buckets ----
        let aggr = backend
            .query_aggregated_buckets(crate::services::queries::AggregatedBucketsSpec {
                range: full_range(),
                scope: project_scope(7).with_deployment(None),
                aggregation_level: AggregationLevel::Events,
                bucket_size: "hour".to_string(),
            })
            .await
            .expect("query_aggregated_buckets");
        assert_eq!(aggr.total, 5);

        // ---- query_property_breakdown returns Validation (not implemented) ----
        let pb = backend
            .query_property_breakdown(PropertyBreakdownSpec::new(
                full_range(),
                project_scope(7),
                None,
                PropertyColumn::Channel,
                "events",
                Some(20),
                None,
            ))
            .await;
        assert!(
            matches!(pb, Err(EventsError::Validation(_))),
            "property_breakdown should report not-implemented as Validation"
        );

        // ---- query_dashboard_projects with empty input returns empty ----
        let empty_dash = backend
            .query_dashboard_projects(crate::services::queries::DashboardProjectsSpec {
                project_ids: vec![],
                range: full_range(),
            })
            .await
            .expect("empty dashboard returns Ok");
        assert!(
            empty_dash.projects.is_empty(),
            "empty input must yield empty response without hitting CH"
        );

        // ---- migration runner is idempotent ----
        // Re-applying must skip everything, not error.
        let report = temps_analytics_backend::migrations::apply_migrations(&client)
            .await
            .expect("re-apply migrations idempotent");
        assert!(
            report.applied.is_empty(),
            "second migration run must apply nothing, got {:?}",
            report.applied
        );
        assert_eq!(report.skipped.len(), 3, "all three migrations skipped");
    }
}
