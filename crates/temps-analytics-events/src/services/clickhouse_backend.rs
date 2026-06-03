//! ClickHouse implementation of [`crate::services::traits::AnalyticsEvents`].
//!
//! Always compiled. Operators activate it by setting `TEMPS_CLICKHOUSE_*`
//! env vars; the plugin layer (in `plugin.rs`) then constructs this backend
//! instead of the Timescale-backed `AnalyticsEventsService` for the read
//! path. No rebuild with a feature flag is required.
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
    EventTypeBreakdown, ProjectDashboardAnalytics, PropertyBreakdownItem,
    PropertyBreakdownResponse, PropertyColumn, PropertyTimelineItem, PropertyTimelineResponse,
    SessionEvent, UniqueCountsResponse,
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

/// Map a [`PropertyColumn`] to a ClickHouse expression that yields the
/// grouping value. Geo columns (country/region/city) are denormalized onto
/// the events table by the fan-out worker — see
/// `ch_fanout::row_to_ch` — so we never need a JOIN at query time.
///
/// Returns the SQL expression alongside the default sentinel string used
/// when the column is empty. Matches the Postgres impl's `'Direct'` for
/// referrer_hostname and `'Unknown'` for everything else, so dashboard
/// labels stay identical across backends.
fn property_column_expr(col: PropertyColumn) -> (&'static str, &'static str) {
    use PropertyColumn::*;
    match col {
        Channel => ("channel", "Unknown"),
        DeviceType => ("device_type", "Unknown"),
        Browser => ("browser", "Unknown"),
        BrowserVersion => ("browser_version", "Unknown"),
        OperatingSystem => ("operating_system", "Unknown"),
        OperatingSystemVersion => ("operating_system_version", "Unknown"),
        UtmSource => ("utm_source", "Unknown"),
        UtmMedium => ("utm_medium", "Unknown"),
        UtmCampaign => ("utm_campaign", "Unknown"),
        UtmTerm => ("utm_term", "Unknown"),
        UtmContent => ("utm_content", "Unknown"),
        ReferrerHostname => ("referrer_hostname", "Direct"),
        Language => ("language", "Unknown"),
        EventType => ("event_type", "Unknown"),
        EventName => ("event_name", "Unknown"),
        PagePath => ("page_path", "Unknown"),
        Pathname => ("pathname", "Unknown"),
        Country => ("country", "Unknown"),
        Region => ("region", "Unknown"),
        City => ("city", "Unknown"),
    }
}

/// Map an `aggregation_level` string ("events" / "sessions" / "visitors")
/// to the matching CH count expression. Mirrors `count_expr` for
/// [`AggregationLevel`] but takes a string because property breakdowns
/// receive the level as a free-form `&str` from the wire format.
fn count_expr_str(level: &str) -> &'static str {
    match level {
        "sessions" => "uniq(session_id)",
        "visitors" => "uniq(visitor_id)",
        _ => "count()",
    }
}

// ---------------------------------------------------------------------------
// SQL builders
//
// These three queries all compute a `percentage` (and, for breakdowns, a
// `total_count`) from a ClickHouse scalar subquery `(SELECT t FROM total)`.
// ClickHouse types a scalar subquery as `Nullable(T)` — it could match zero
// rows — and that nullability propagates through the surrounding `if(...)`
// arithmetic. The `clickhouse` crate decodes a `Nullable` column as a
// null-flag byte followed by the value, but a plain `f64`/`u64` struct field
// reads only the value bytes, so the RowBinary stream desyncs and every read
// fails with "not enough data, probably a row type mismatches a database
// schema". Wrapping each such expression in `assumeNotNull(...)` makes the
// returned column non-nullable so it lines up with the row structs
// (`CountAndPercentRow`, `BreakdownRow`) that hold plain `f64`/`u64`. The
// values are logically never NULL (sum over an empty set is 0, and the `if`
// guard short-circuits on a zero total), so the assumption is always sound.
//
// Extracted into pure functions so a Docker-free unit test can assert the
// guard is present — the integration test that would otherwise catch this
// is gated on a ClickHouse container and silently skips when Docker is
// unavailable.
// ---------------------------------------------------------------------------

/// Build the `query_events_count` SQL. `level_expr` is a trusted count
/// expression from [`count_expr`]; `custom_filter` is a fixed SQL fragment
/// (empty or the page-view exclusion) — neither is user-controlled.
fn events_count_sql(level_expr: &str, custom_filter: &str) -> String {
    format!(
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
                assumeNotNull(if((SELECT t FROM total) > 0,
                   {level_expr} / (SELECT t FROM total) * 100,
                   0)) AS percentage
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
    )
}

/// Build the `query_event_type_breakdown` SQL. `level_expr` is a trusted
/// count expression from [`count_expr`].
fn event_type_breakdown_sql(level_expr: &str) -> String {
    format!(
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
                assumeNotNull(if((SELECT t FROM total) > 0,
                   {level_expr} / (SELECT t FROM total) * 100,
                   0)) AS percentage
            FROM events FINAL
            WHERE project_id = ?
              AND timestamp >= fromUnixTimestamp64Milli(?)
              AND timestamp <= fromUnixTimestamp64Milli(?)
              AND (? = 0 OR environment_id = ?)
            GROUP BY event_type
            ORDER BY count DESC
            "#
    )
}

/// Build the `query_property_breakdown` SQL. `col`/`sentinel` come from the
/// allowlisted [`property_column_expr`] and `count_sql` from [`count_expr_str`]
/// — all three are fixed strings, never raw user input.
fn property_breakdown_sql(col: &str, sentinel: &str, count_sql: &str) -> String {
    format!(
        r#"
            WITH value_counts AS (
                SELECT
                    if({col} = '', '{sentinel}', {col}) AS value,
                    {count_sql} AS count
                FROM events FINAL
                WHERE project_id = ?
                  AND timestamp >= fromUnixTimestamp64Milli(?)
                  AND timestamp <= fromUnixTimestamp64Milli(?)
                  AND (? = 0 OR environment_id = ?)
                  AND (? = 0 OR deployment_id = ?)
                  AND (? = 0 OR event_name = ?)
                  AND (? = 0 OR country = ?)
                  AND (? = 0 OR region = ?)
                  AND (? = 0 OR browser = ?)
                  AND (? = 0 OR operating_system = ?)
                  AND (? = 0 OR channel = ?)
                  AND (? = 0
                       OR (? = 1 AND referrer_hostname = ?)
                       OR (? = 2 AND referrer_hostname = ''))
                GROUP BY value
            ),
            total AS (SELECT sum(count) AS t FROM value_counts)
            SELECT
                value,
                count,
                assumeNotNull(if((SELECT t FROM total) > 0,
                   count / (SELECT t FROM total) * 100,
                   0)) AS percentage,
                assumeNotNull((SELECT t FROM total)) AS total_count
            FROM value_counts
            ORDER BY count DESC
            LIMIT ?
            "#
    )
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

        let sql = events_count_sql(level_expr, custom_filter);

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

        let sql = event_type_breakdown_sql(level_expr);

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

        // CH 26+ requires toUnixTimestamp64Milli's argument to be DateTime64.
        // toStartOfInterval returns DateTime, so wrap with toDateTime64(_, 3, 'UTC').
        let sql = format!(
            r#"
            SELECT
                toUnixTimestamp64Milli(toDateTime64(toStartOfInterval(timestamp, {interval}), 3, 'UTC')) AS bucket_ms,
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
                FROM toUnixTimestamp64Milli(toDateTime64(toStartOfInterval(fromUnixTimestamp64Milli(?), {interval}), 3, 'UTC'))
                TO toUnixTimestamp64Milli(toDateTime64(toStartOfInterval(fromUnixTimestamp64Milli(?), {interval}), 3, 'UTC')) + 1
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
        q: PropertyBreakdownSpec,
    ) -> Result<PropertyBreakdownResponse, EventsError> {
        let (col, sentinel) = property_column_expr(q.group_by_column.clone());
        let count_sql = count_expr_str(&q.aggregation_level);
        let group_by_str = q.group_by_column.as_str().to_string();

        let env_filter_flag: i32 = q.scope.environment_id.map(|_| 1).unwrap_or(0);
        let env_filter_value: i32 = q.scope.environment_id.unwrap_or(0);
        let dep_filter_flag: i32 = q.scope.deployment_id.map(|_| 1).unwrap_or(0);
        let dep_filter_value: i32 = q.scope.deployment_id.unwrap_or(0);
        let event_filter_flag: i32 = q.event_name.as_ref().map(|_| 1).unwrap_or(0);
        let event_filter_value = q.event_name.clone().unwrap_or_default();
        let start_ms = to_unix_milli(q.range.start);
        let end_ms = to_unix_milli(q.range.end);

        // Drill-down filters mirror the Postgres impl. Every filter is
        // optional; the (flag = 0 OR column = value) idiom keeps the SQL
        // shape constant regardless of which filters are populated.
        let f = q.filters.clone().unwrap_or_default();
        let f_country_flag = i32::from(f.country.is_some());
        let f_country_value = f.country.clone().unwrap_or_default();
        let f_region_flag = i32::from(f.region.is_some());
        let f_region_value = f.region.clone().unwrap_or_default();
        let f_browser_flag = i32::from(f.browser.is_some());
        let f_browser_value = f.browser.clone().unwrap_or_default();
        let f_os_flag = i32::from(f.operating_system.is_some());
        let f_os_value = f.operating_system.clone().unwrap_or_default();
        let f_channel_flag = i32::from(f.channel.is_some());
        let f_channel_value = f.channel.clone().unwrap_or_default();
        // 'Direct' is the sentinel for "no referrer" — match it as empty
        // string since CH stores empty referrer_hostname as ''.
        let (f_referrer_flag, f_referrer_value) = match f.referrer.as_deref() {
            None => (0, String::new()),
            Some("Direct") => (2, String::new()),
            Some(other) => (1, other.to_string()),
        };

        // NOTE on parity gap (documented divergence from PG):
        // - The PG impl applies a self-referral filter against
        //   `project_custom_domains` when grouping by referrer_hostname.
        //   We don't have that table replicated to CH. Apps that drill
        //   into referrer breakdowns will see their own domain in the
        //   list. Acceptable v1; flagged in the runbook.
        let sql = property_breakdown_sql(col, sentinel, count_sql);

        #[derive(Row, Deserialize)]
        struct BreakdownRow {
            value: String,
            count: u64,
            percentage: f64,
            total_count: u64,
        }

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
            .bind(event_filter_flag)
            .bind(&event_filter_value)
            .bind(f_country_flag)
            .bind(&f_country_value)
            .bind(f_region_flag)
            .bind(&f_region_value)
            .bind(f_browser_flag)
            .bind(&f_browser_value)
            .bind(f_os_flag)
            .bind(&f_os_value)
            .bind(f_channel_flag)
            .bind(&f_channel_value)
            // Referrer filter has three states (none / equality / direct-only)
            // so it gets three params; matched by the (? = 0 OR …) chain.
            .bind(f_referrer_flag)
            .bind(f_referrer_flag)
            .bind(&f_referrer_value)
            .bind(f_referrer_flag)
            .bind(q.limit as u32)
            .fetch_all::<BreakdownRow>()
            .await
            .map_err(|e| ch_err("query_property_breakdown", e))?;

        let total = rows.first().map(|r| r.total_count as i64).unwrap_or(0);
        Ok(PropertyBreakdownResponse {
            property: group_by_str,
            items: rows
                .into_iter()
                .map(|r| PropertyBreakdownItem {
                    value: r.value,
                    count: r.count as i64,
                    percentage: r.percentage,
                })
                .collect(),
            total,
        })
    }

    async fn query_property_timeline(
        &self,
        q: PropertyTimelineSpec,
    ) -> Result<PropertyTimelineResponse, EventsError> {
        let (col, sentinel) = property_column_expr(q.group_by_column.clone());
        let count_sql = count_expr_str(&q.aggregation_level);
        let group_by_str = q.group_by_column.as_str().to_string();

        // Bucket auto-detection mirrors the Postgres impl so dashboards
        // pick the same granularity at the same range widths.
        let duration_days = (q.range.end - q.range.start).num_days();
        let bucket_label = q.bucket_size.clone().unwrap_or_else(|| {
            if duration_days <= 1 {
                "1 hour".to_string()
            } else if duration_days <= 7 {
                "1 day".to_string()
            } else if duration_days <= 60 {
                "1 week".to_string()
            } else {
                "1 month".to_string()
            }
        });
        let interval = ch_interval(Some(bucket_label.as_str()));

        let env_filter_flag: i32 = q.scope.environment_id.map(|_| 1).unwrap_or(0);
        let env_filter_value: i32 = q.scope.environment_id.unwrap_or(0);
        let dep_filter_flag: i32 = q.scope.deployment_id.map(|_| 1).unwrap_or(0);
        let dep_filter_value: i32 = q.scope.deployment_id.unwrap_or(0);
        let event_filter_flag: i32 = q.event_name.as_ref().map(|_| 1).unwrap_or(0);
        let event_filter_value = q.event_name.clone().unwrap_or_default();
        let start_ms = to_unix_milli(q.range.start);
        let end_ms = to_unix_milli(q.range.end);

        let sql = format!(
            r#"
            SELECT
                toUnixTimestamp64Milli(toDateTime64(toStartOfInterval(timestamp, {interval}), 3, 'UTC')) AS bucket_ms,
                if({col} = '', '{sentinel}', {col}) AS value,
                {count_sql} AS count
            FROM events FINAL
            WHERE project_id = ?
              AND timestamp >= fromUnixTimestamp64Milli(?)
              AND timestamp <= fromUnixTimestamp64Milli(?)
              AND (? = 0 OR environment_id = ?)
              AND (? = 0 OR deployment_id = ?)
              AND (? = 0 OR event_name = ?)
            GROUP BY bucket_ms, value
            ORDER BY bucket_ms ASC, count DESC
            "#
        );

        #[derive(Row, Deserialize)]
        struct TimelineRow {
            bucket_ms: i64,
            value: String,
            count: u64,
        }

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
            .bind(event_filter_flag)
            .bind(&event_filter_value)
            .fetch_all::<TimelineRow>()
            .await
            .map_err(|e| ch_err("query_property_timeline", e))?;

        Ok(PropertyTimelineResponse {
            property: group_by_str,
            bucket_size: bucket_label,
            items: rows
                .into_iter()
                .map(|r| PropertyTimelineItem {
                    // Match Timescale impl: ISO-8601 with timezone (`to_rfc3339`).
                    timestamp: from_unix_milli(r.bucket_ms).to_rfc3339(),
                    value: r.value,
                    count: r.count as i64,
                })
                .collect(),
        })
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
                toUnixTimestamp64Milli(toDateTime64(toStartOfHour(timestamp), 3, 'UTC')) AS bucket_ms,
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
                FROM toUnixTimestamp64Milli(toDateTime64(toStartOfHour(fromUnixTimestamp64Milli(?)), 3, 'UTC'))
                TO toUnixTimestamp64Milli(toDateTime64(toStartOfHour(fromUnixTimestamp64Milli(?)), 3, 'UTC')) + 1
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
        use std::collections::HashMap;

        // Empty input short-circuits to empty output without hitting CH.
        if q.project_ids.is_empty() {
            return Ok(DashboardProjectsAnalyticsResponse {
                projects: HashMap::new(),
            });
        }

        // Three CH queries in parallel would cut latency, but tokio::join!
        // here would also fan out three separate connections. Sequential is
        // fine for the dashboard view — each is sub-second on a healthy CH.
        // If this becomes a bottleneck, batch them into a single query
        // with FILTER clauses in CH ≥ 23.

        let start_ms = to_unix_milli(q.range.start);
        let end_ms = to_unix_milli(q.range.end);
        // Previous period: same duration, shifted back. Mirrors Timescale impl.
        let duration = q.range.end - q.range.start;
        let prev_start_ms = to_unix_milli(q.range.start - duration);
        let prev_end_ms = start_ms;

        // ---- Query 1: current-period unique visitors per project ----
        // Sub-select FROM (SELECT … FROM events FINAL) gives FINAL once
        // and lets the outer aggregate group cleanly.
        #[derive(Row, Deserialize)]
        struct ProjectCountRow {
            project_id: i32,
            visitors: u64,
        }

        let current_sql = r#"
            SELECT
                project_id,
                uniq(visitor_id) AS visitors
            FROM events FINAL
            WHERE has(?, project_id)
              AND timestamp >= fromUnixTimestamp64Milli(?)
              AND timestamp <= fromUnixTimestamp64Milli(?)
            GROUP BY project_id
        "#;

        let project_ids = q.project_ids.clone();
        let current: Vec<ProjectCountRow> = self
            .client
            .query(current_sql)
            .bind(project_ids.clone())
            .bind(start_ms)
            .bind(end_ms)
            .fetch_all::<ProjectCountRow>()
            .await
            .map_err(|e| ch_err("query_dashboard_projects[current]", e))?;
        let current_map: HashMap<i32, i64> = current
            .into_iter()
            .map(|r| (r.project_id, r.visitors as i64))
            .collect();

        // ---- Query 2: previous-period unique visitors per project ----
        let previous: Vec<ProjectCountRow> = self
            .client
            .query(current_sql)
            .bind(project_ids.clone())
            .bind(prev_start_ms)
            .bind(prev_end_ms)
            .fetch_all::<ProjectCountRow>()
            .await
            .map_err(|e| ch_err("query_dashboard_projects[previous]", e))?;
        let previous_map: HashMap<i32, i64> = previous
            .into_iter()
            .map(|r| (r.project_id, r.visitors as i64))
            .collect();

        // ---- Query 3: hourly sparkline per project (page_view only) ----
        // ClickHouse `WITH FILL` only fills along ORDER BY, so we have to
        // pivot per project_id at the application layer. Each project gets
        // a sparse row set; we densify in Rust below.
        #[derive(Row, Deserialize)]
        struct HourlyRow {
            project_id: i32,
            bucket_ms: i64,
            visitors: u64,
        }

        let hourly_sql = r#"
            SELECT
                project_id,
                toUnixTimestamp64Milli(toDateTime64(toStartOfHour(timestamp), 3, 'UTC')) AS bucket_ms,
                uniq(visitor_id) AS visitors
            FROM events FINAL
            WHERE has(?, project_id)
              AND timestamp >= fromUnixTimestamp64Milli(?)
              AND timestamp <= fromUnixTimestamp64Milli(?)
              AND event_type = 'page_view'
            GROUP BY project_id, bucket_ms
            ORDER BY project_id, bucket_ms
        "#;

        let hourly_rows: Vec<HourlyRow> = self
            .client
            .query(hourly_sql)
            .bind(project_ids.clone())
            .bind(start_ms)
            .bind(end_ms)
            .fetch_all::<HourlyRow>()
            .await
            .map_err(|e| ch_err("query_dashboard_projects[hourly]", e))?;

        let mut hourly_map: HashMap<i32, HashMap<i64, i64>> = HashMap::new();
        for r in hourly_rows {
            hourly_map
                .entry(r.project_id)
                .or_default()
                .insert(r.bucket_ms, r.visitors as i64);
        }

        // Densify: produce one EventTimeline per hour in [start, end] for
        // every project, including zero-visitor hours. Mirrors the
        // Timescale impl's generate_series gap-fill.
        let bucket_start = align_to_hour_ms(start_ms);
        let bucket_end = align_to_hour_ms(end_ms);
        let mut buckets = Vec::new();
        let mut t = bucket_start;
        while t <= bucket_end {
            buckets.push(t);
            t += 3_600_000; // one hour in ms
        }

        let mut projects = HashMap::new();
        for &pid in &q.project_ids {
            let counts = hourly_map.remove(&pid).unwrap_or_default();
            let hourly_visits: Vec<EventTimeline> = buckets
                .iter()
                .map(|&ms| EventTimeline {
                    date: from_unix_milli(ms),
                    count: counts.get(&ms).copied().unwrap_or(0),
                })
                .collect();

            let current = current_map.get(&pid).copied().unwrap_or(0);
            let previous = previous_map.get(&pid).copied().unwrap_or(0);
            // Identical trend semantics to the Timescale impl: None when
            // both periods are zero, 100% when previous is zero but
            // current is positive.
            let trend_percentage = if previous > 0 {
                Some(((current - previous) as f64 / previous as f64) * 100.0)
            } else if current > 0 {
                Some(100.0)
            } else {
                None
            };

            projects.insert(
                pid.to_string(),
                ProjectDashboardAnalytics {
                    project_id: pid,
                    unique_visitors: current,
                    previous_unique_visitors: previous,
                    trend_percentage,
                    hourly_visits,
                },
            );
        }

        Ok(DashboardProjectsAnalyticsResponse { projects })
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
                toUnixTimestamp64Milli(toDateTime64(toStartOfInterval(timestamp, {interval}), 3, 'UTC')) AS bucket_ms,
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
                FROM toUnixTimestamp64Milli(toDateTime64(toStartOfInterval(fromUnixTimestamp64Milli(?), {interval}), 3, 'UTC'))
                TO toUnixTimestamp64Milli(toDateTime64(toStartOfInterval(fromUnixTimestamp64Milli(?), {interval}), 3, 'UTC')) + 1
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

/// Floor a Unix-millis timestamp to the start of its UTC hour. Used by the
/// dashboard sparkline densifier to align bucket boundaries with CH's
/// `toStartOfHour(timestamp)` so the in-memory grid lines up with the
/// values CH returns.
fn align_to_hour_ms(ms: i64) -> i64 {
    const HOUR_MS: i64 = 3_600_000;
    (ms / HOUR_MS) * HOUR_MS
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
            country: String,
            region: String,
            city: String,
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
                country: "US".into(),
                region: "CA".into(),
                city: "San Francisco".into(),
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

        // ---- query_property_breakdown ----
        // Group by channel; all 5 project-7 events have channel="direct".
        // events level = 5 raw events, all under one bucket.
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
            .await
            .expect("query_property_breakdown channel");
        assert_eq!(pb.property, "channel");
        assert_eq!(pb.total, 5);
        assert!(
            pb.items.iter().any(|i| i.value == "direct" && i.count == 5),
            "expected channel=direct count=5, got {:?}",
            pb.items
        );

        // Group by country (denormalized geo column populated by seed).
        let pb_country = backend
            .query_property_breakdown(PropertyBreakdownSpec::new(
                full_range(),
                project_scope(7),
                None,
                PropertyColumn::Country,
                "events",
                Some(20),
                None,
            ))
            .await
            .expect("query_property_breakdown country");
        assert!(
            pb_country.items.iter().any(|i| i.value == "US"),
            "expected country=US in {:?}",
            pb_country.items
        );

        // ---- query_property_timeline ----
        let pt = backend
            .query_property_timeline(crate::services::queries::PropertyTimelineSpec {
                range: full_range(),
                scope: project_scope(7).with_deployment(None),
                event_name: None,
                group_by_column: PropertyColumn::Channel,
                aggregation_level: "events".to_string(),
                bucket_size: Some("1 hour".to_string()),
            })
            .await
            .expect("query_property_timeline");
        assert_eq!(pt.property, "channel");
        let pt_total: i64 = pt.items.iter().map(|i| i.count).sum();
        assert_eq!(pt_total, 5, "got {:?}", pt.items);

        // ---- query_dashboard_projects: empty short-circuit ----
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

        // ---- query_dashboard_projects: real input ----
        let dash = backend
            .query_dashboard_projects(crate::services::queries::DashboardProjectsSpec {
                project_ids: vec![7, 8],
                range: full_range(),
            })
            .await
            .expect("query_dashboard_projects");
        let p7 = dash.projects.get("7").expect("project 7 in response");
        // 2 distinct visitors on project 7 (100, 101).
        assert_eq!(p7.unique_visitors, 2);
        // Hourly sparkline has one EventTimeline per hour in range,
        // including zero-visitor hours (densified to mirror PG impl).
        assert!(
            !p7.hourly_visits.is_empty(),
            "sparkline must include densified buckets"
        );
        let p8 = dash.projects.get("8").expect("project 8 in response");
        assert_eq!(p8.unique_visitors, 1);

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

    // ── SQL-shape guards (no Docker required) ──────────────────────────────
    //
    // The integration test above is the real end-to-end check, but it skips
    // silently when Docker is unavailable — which is exactly how the
    // Nullable-scalar-subquery bug shipped. These guards run in plain
    // `cargo test`: they assert the SQL each row struct deserializes wraps
    // its nullable scalar-subquery columns in `assumeNotNull(...)`, so a
    // `Nullable(T)` column can never be read into a plain `f64`/`u64` field
    // (which fails with "not enough data, probably a row type mismatch").

    /// Helper: every `(SELECT t FROM total)` reference that feeds an output
    /// column must sit inside an `assumeNotNull(...)`. We assert by checking
    /// the SQL has no bare `) AS percentage` / `) AS total_count` whose
    /// expression isn't `assumeNotNull`-wrapped. Cheap, robust proxy: the
    /// only output columns built from the scalar subquery are wrapped.
    fn assert_no_bare_nullable_output(sql: &str) {
        // The percentage column is always derived from the scalar subquery.
        assert!(
            sql.contains("assumeNotNull(if((SELECT t FROM total) > 0,"),
            "percentage column must wrap the Nullable if(...) in assumeNotNull; SQL:\n{sql}"
        );
        assert!(
            !sql.contains("0) AS percentage"),
            "found an unwrapped `0) AS percentage` — the Nullable if(...) leaks; SQL:\n{sql}"
        );
    }

    #[test]
    fn events_count_sql_wraps_nullable_percentage() {
        // Both aggregation levels and both custom-filter states.
        for level in ["count()", "uniq(session_id)", "uniq(visitor_id)"] {
            for filter in ["", "AND event_name NOT IN ('page_view')"] {
                let sql = events_count_sql(level, filter);
                assert_no_bare_nullable_output(&sql);
                // The count column itself is non-nullable (count()/uniq()) and
                // must NOT be wrapped — sanity-check we didn't over-wrap.
                assert!(sql.contains(&format!("{level} AS count")));
            }
        }
    }

    #[test]
    fn event_type_breakdown_sql_wraps_nullable_percentage() {
        for level in ["count()", "uniq(session_id)", "uniq(visitor_id)"] {
            let sql = event_type_breakdown_sql(level);
            assert_no_bare_nullable_output(&sql);
        }
    }

    #[test]
    fn property_breakdown_sql_wraps_nullable_percentage_and_total() {
        let sql = property_breakdown_sql("channel", "Unknown", "count()");
        assert_no_bare_nullable_output(&sql);
        // total_count is the raw scalar subquery — it MUST be wrapped, since a
        // bare `(SELECT t FROM total)` is `Nullable(UInt64)` and the
        // `total_count: u64` field can't decode it.
        assert!(
            sql.contains("assumeNotNull((SELECT t FROM total)) AS total_count"),
            "total_count must wrap the scalar subquery in assumeNotNull; SQL:\n{sql}"
        );
        assert!(
            !sql.contains("total)) AS total_count")
                || sql.contains("assumeNotNull((SELECT t FROM total)) AS total_count"),
            "total_count column must be assumeNotNull-wrapped; SQL:\n{sql}"
        );
    }

    /// Exhaustive guard across the allowlisted grouping columns: every
    /// property-breakdown variant produces the same wrapped output shape, so
    /// drilling into any dimension (browser, country, referrer, …) is safe.
    #[test]
    fn property_breakdown_sql_wrapped_for_every_column() {
        use crate::types::PropertyColumn::*;
        let columns = [
            Channel,
            DeviceType,
            Browser,
            OperatingSystem,
            ReferrerHostname,
            Country,
            Region,
            City,
            UtmSource,
            EventName,
            PagePath,
        ];
        for col in columns {
            let (expr, sentinel) = property_column_expr(col.clone());
            for level in ["events", "sessions", "visitors"] {
                let count_sql = count_expr_str(level);
                let sql = property_breakdown_sql(expr, sentinel, count_sql);
                assert_no_bare_nullable_output(&sql);
                assert!(
                    sql.contains("assumeNotNull((SELECT t FROM total)) AS total_count"),
                    "column {col:?} / level {level}: total_count not wrapped"
                );
            }
        }
    }
}
