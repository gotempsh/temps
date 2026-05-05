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
