//! ClickHouse-backed implementation of [`ProxyLogStorage`].
//!
//! Active only when all four `TEMPS_CLICKHOUSE_*` env vars are populated
//! (`ServerConfig::is_clickhouse_enabled()`). When disabled, the
//! [`TimescaleDbProxyLogStore`](super::TimescaleDbProxyLogStore) path is used
//! unchanged.
//!
//! # Design (locked)
//!
//! One raw `proxy_logs` table with a native 30-day TTL and **query-time**
//! aggregation — no rollup materialized views. Time bucketing is done with
//! `toStartOfInterval()` at read time; the gapfill/spine semantics of the
//! TimescaleDB queries are reproduced with `WITH FILL`. See
//! `migrations/clickhouse/0001_proxy_logs.sql` for the full schema rationale
//! and benchmark.
//!
//! # Result parity
//!
//! Every method returns the exact same DTO shape as
//! [`TimescaleDbProxyLogStore`]. The list / lookups return `proxy_logs::Model`
//! (so the handler's `ProxyLogResponse::from(model)` path is untouched); the
//! aggregations return the same `TimeBucketStats`, `ProjectHealthSummary`,
//! `AiAgentBreakdownRow`, `AiPageBreakdownRow`, `AiAgentTimelineRow`,
//! `AiStatusBreakdownRow` structs. The Rust-side roll-up + empty-marker logic
//! for the AI timeline lives in this file and matches the Postgres path
//! byte-for-byte.
//!
//! # Security
//!
//! - The configured database name is validated with `[A-Za-z0-9_]` before any
//!   DDL (in [`super::clickhouse_migrations`]).
//! - EVERY filter value (path, host, method, status, bot_name, ai_provider,
//!   client_ip, user_agent, browser, os, device_type, time/size ranges, …) is
//!   passed via a bound `?` param using the deferred-bind [`Bv`] enum — never
//!   string-interpolated.
//! - The four substring filters (host, path, user_agent, upstream_host) and the
//!   bot_name substring filter use `ILIKE ?` with `escape_like_pattern` so
//!   `%`/`_`/`\` in user input cannot widen the match.
//! - The only interpolated tokens are: the ORDER BY column (from a fixed
//!   allowlist `match`, mirroring the TimescaleDB sort), the ASC/DESC direction,
//!   the server-derived `LIMIT` (validated numeric), the bucket width in seconds
//!   (derived from an `is_valid_interval`-validated string), and the
//!   server-derived status-class CASE — none of which carry user-controlled
//!   strings. The known-agent IN list is server-derived from
//!   `ai_agent_detector::known_agents()` and is bound as an array regardless.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use serde::{Deserialize, Serialize};
use temps_core::UtcDateTime;
use temps_entities::proxy_logs;
use tracing::warn;

use super::ProxyLogStorage;
use crate::handler::proxy_logs::ProxyLogsQuery;
use crate::service::proxy_log_service::{
    AiAgentBreakdownRow, AiAgentTimelineRow, AiPageBreakdownRow, AiStatusBreakdownRow,
    AiTimelineGroupBy, CreateProxyLogRequest, ProjectHealthSummary, ProxyLogService,
    ProxyLogServiceError, StatsFilters, TimeBucketStats,
};

/// Maximum rows per ClickHouse HTTP insert request. Bounds peak buffer memory
/// in the `clickhouse` client on large batches. Proxy log batches flush at 200
/// rows so this is rarely hit, but a backlog drain could produce more.
const MAX_INSERT_BATCH: usize = 5_000;

// ── Client configuration ────────────────────────────────────────────────────

/// Connection configuration for the ClickHouse proxy-log backend.
///
/// Built from `ServerConfig` fields populated by the `TEMPS_CLICKHOUSE_*`
/// environment variables. All four fields are required; the plugin/server
/// calls `ServerConfig::is_clickhouse_enabled()` to guard construction.
#[derive(Clone)]
pub struct ClickHouseProxyLogConfig {
    pub url: String,
    pub database: String,
    pub user: String,
    pub password: String,
}

// Manual Debug that masks the password so it can never leak into logs.
impl std::fmt::Debug for ClickHouseProxyLogConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClickHouseProxyLogConfig")
            .field("url", &self.url)
            .field("database", &self.database)
            .field("user", &self.user)
            .field("password", &"***")
            .finish()
    }
}

impl ClickHouseProxyLogConfig {
    pub fn new(
        url: impl Into<String>,
        database: impl Into<String>,
        user: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            url: url.into(),
            database: database.into(),
            user: user.into(),
            password: password.into(),
        }
    }
}

// ── LIKE pattern escaping ────────────────────────────────────────────────────

/// Escape LIKE/ILIKE metacharacters in a user-supplied substring pattern.
///
/// ClickHouse LIKE uses backslash as the default escape character. We escape
/// `\` → `\\` first, then `%` → `\%` and `_` → `\_`. The caller wraps the
/// result with `%{escaped}%` for a case-insensitive substring search via ILIKE.
/// (Copied from `temps-otel`'s helper — verified against live CH 26.2.)
pub(crate) fn escape_like_pattern(pattern: &str) -> String {
    pattern
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Convert a validated `"N unit"` interval string (already passed through
/// [`ProxyLogService::is_valid_interval`]) into a whole number of seconds for
/// `toStartOfInterval(..., INTERVAL N SECOND)` and `WITH FILL ... STEP`.
///
/// Returns `None` if the string is malformed (the caller has already validated
/// it, so this is belt-and-braces). Months and years are approximated (30 days
/// / 365 days) — proxy-log charts only ever request second/minute/hour/day
/// buckets in practice, so the approximation never reaches the UI.
fn interval_to_seconds(interval: &str) -> Option<i64> {
    let parts: Vec<&str> = interval.split_whitespace().collect();
    if parts.len() != 2 {
        return None;
    }
    let n: i64 = parts[0].parse().ok()?;
    let unit_secs: i64 = match parts[1] {
        "microsecond" | "microseconds" | "millisecond" | "milliseconds" | "second" | "seconds" => 1,
        "minute" | "minutes" => 60,
        "hour" | "hours" => 3_600,
        "day" | "days" => 86_400,
        "week" | "weeks" => 604_800,
        "month" | "months" => 2_592_000,
        "year" | "years" => 31_536_000,
        _ => return None,
    };
    // Sub-second units collapse to a 1-second minimum bucket (CH cannot bucket
    // sub-second via INTERVAL … SECOND, and proxy charts never go below 1s).
    //
    // Use saturating arithmetic and clamp to a sane upper bound: a caller-supplied
    // count like "4294967295 years" must not overflow i64 here (or downstream when
    // multiplied by 1000 for step_ms). A 10-year bucket is already absurd for a
    // request chart, so MAX_BUCKET_SECS caps it without affecting any real query.
    const MAX_BUCKET_SECS: i64 = 10 * 31_536_000; // 10 years
    Some(n.saturating_mul(unit_secs).clamp(1, MAX_BUCKET_SECS))
}

// ── Deferred bind value ──────────────────────────────────────────────────────

/// A bind value queued while a dynamic WHERE clause is assembled, applied to
/// the query builder in order after the SQL string is finalized. Mirrors the
/// `Bv` pattern in `temps-otel`'s `query_spans`.
enum Bv {
    I16(i16),
    I32(i32),
    I64(i64),
    Bool(bool),
    Str(String),
    /// Bound as a ClickHouse `Array(String)` (e.g. the known-agent list).
    StrVec(Vec<String>),
}

/// Apply all queued bind values to a ClickHouse query builder in order.
fn apply_binds(mut q: ::clickhouse::query::Query, binds: Vec<Bv>) -> ::clickhouse::query::Query {
    for b in binds {
        q = match b {
            Bv::I16(v) => q.bind(v),
            Bv::I32(v) => q.bind(v),
            Bv::I64(v) => q.bind(v),
            Bv::Bool(v) => q.bind(v),
            Bv::Str(v) => q.bind(v),
            Bv::StrVec(v) => q.bind(v),
        };
    }
    q
}

// ── Write-side row type ──────────────────────────────────────────────────────

/// ClickHouse row matching the `proxy_logs` table DDL in `0001_proxy_logs.sql`.
///
/// **Field order MUST match the DDL column order exactly.** The `clickhouse`
/// crate serialises fields positionally (binary protocol over HTTP); any
/// reordering relative to the DDL silently corrupts inserts.
///
/// ## Type mapping
///
/// | DDL type                  | Rust type      | Notes                              |
/// |---------------------------|----------------|------------------------------------|
/// | `DateTime64(3, 'UTC')`    | `i64`          | Unix milliseconds                  |
/// | `LowCardinality(String)`  | `String`       | method/source/routing/browser/etc. |
/// | `String DEFAULT ''`       | `String`       | `Option<String>` → `""` sentinel   |
/// | `Int16`                   | `i16`          | status_code                        |
/// | `Nullable(Int32)`         | `Option<i32>`  | ids, response_time                 |
/// | `Nullable(Int64)`         | `Option<i64>`  | sizes                              |
/// | `Nullable(UInt8)`         | `Option<u8>`   | is_bot tri-state                   |
/// | `UInt8`                   | `u8`           | is_system_request (0/1)            |
/// | `Date`                    | `u16`          | created_date as days-since-epoch   |
/// | `UInt64`                  | `u64`          | _version                           |
#[derive(::clickhouse::Row, Serialize, Deserialize, Debug, Clone)]
pub struct ChProxyLogRow {
    /// timestamp DateTime64(3,'UTC') — Unix milliseconds.
    pub timestamp: i64,
    /// method LowCardinality(String)
    pub method: String,
    /// path String
    pub path: String,
    /// query_string String DEFAULT ''
    pub query_string: String,
    /// host String
    pub host: String,
    /// status_code Int16
    pub status_code: i16,
    /// response_time_ms Nullable(Int32)
    pub response_time_ms: Option<i32>,
    /// request_source LowCardinality(String)
    pub request_source: String,
    /// is_system_request UInt8
    pub is_system_request: u8,
    /// routing_status LowCardinality(String)
    pub routing_status: String,
    /// project_id Nullable(Int32)
    pub project_id: Option<i32>,
    /// environment_id Nullable(Int32)
    pub environment_id: Option<i32>,
    /// deployment_id Nullable(Int32)
    pub deployment_id: Option<i32>,
    /// session_id Nullable(Int32)
    pub session_id: Option<i32>,
    /// visitor_id Nullable(Int32)
    pub visitor_id: Option<i32>,
    /// container_id String DEFAULT ''
    pub container_id: String,
    /// upstream_host String DEFAULT ''
    pub upstream_host: String,
    /// error_message String DEFAULT ''
    pub error_message: String,
    /// client_ip String DEFAULT ''
    pub client_ip: String,
    /// user_agent String DEFAULT ''
    pub user_agent: String,
    /// referrer String DEFAULT ''
    pub referrer: String,
    /// request_id String
    pub request_id: String,
    /// ip_geolocation_id Nullable(Int32)
    pub ip_geolocation_id: Option<i32>,
    /// browser LowCardinality(String) DEFAULT ''
    pub browser: String,
    /// browser_version String DEFAULT ''
    pub browser_version: String,
    /// operating_system LowCardinality(String) DEFAULT ''
    pub operating_system: String,
    /// device_type LowCardinality(String) DEFAULT ''
    pub device_type: String,
    /// is_bot Nullable(UInt8) — tri-state preserved.
    pub is_bot: Option<u8>,
    /// bot_name LowCardinality(String) DEFAULT ''
    pub bot_name: String,
    /// request_size_bytes Nullable(Int64)
    pub request_size_bytes: Option<i64>,
    /// response_size_bytes Nullable(Int64)
    pub response_size_bytes: Option<i64>,
    /// cache_status LowCardinality(String) DEFAULT ''
    pub cache_status: String,
    /// request_headers String DEFAULT '{}'
    pub request_headers: String,
    /// response_headers String DEFAULT '{}'
    pub response_headers: String,
    /// created_date Date — days since the Unix epoch.
    pub created_date: u16,
    /// trace_id String DEFAULT ''
    pub trace_id: String,
    /// error_group_id Nullable(Int32)
    pub error_group_id: Option<i32>,
    /// _version UInt64 — Unix-ms dedup key for ReplacingMergeTree.
    pub _version: u64,
    /// retention_days UInt16 DEFAULT 30
    ///
    /// Added by migration 0003_retention_days.sql — must remain the last
    /// field so its position matches the DDL column order (positional binary
    /// serialization). The TTL expression in 0004_retention_ttl.sql reads
    /// this column: `toDateTime(timestamp) + toIntervalDay(retention_days)`.
    pub retention_days: u16,
}

impl From<&CreateProxyLogRequest> for ChProxyLogRow {
    /// Build a row from an already-enriched entry. `timestamp`/`created_date`/
    /// `_version` are stamped at conversion time (the ingest moment), matching
    /// the TimescaleDB path which uses `Utc::now()` per batch.
    fn from(entry: &CreateProxyLogRequest) -> Self {
        let now = Utc::now();
        let now_ms = now.timestamp_millis();
        // Days since the Unix epoch for the CH `Date` column.
        let created_date_days = (now.timestamp() / 86_400) as u16;

        // JSON header columns: serialize the Value, fall back to the canonical
        // empty-object sentinel on the (unreachable in practice) error path.
        let request_headers = entry
            .request_headers
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "{}".into()))
            .unwrap_or_else(|| "{}".into());
        let response_headers = entry
            .response_headers
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "{}".into()))
            .unwrap_or_else(|| "{}".into());

        Self {
            timestamp: now_ms,
            method: entry.method.clone(),
            path: entry.path.clone(),
            query_string: entry.query_string.clone().unwrap_or_default(),
            host: entry.host.clone(),
            status_code: entry.status_code,
            response_time_ms: entry.response_time_ms,
            request_source: entry.request_source.clone(),
            is_system_request: u8::from(entry.is_system_request),
            routing_status: entry.routing_status.clone(),
            project_id: entry.project_id,
            environment_id: entry.environment_id,
            deployment_id: entry.deployment_id,
            session_id: entry.session_id,
            visitor_id: entry.visitor_id,
            container_id: entry.container_id.clone().unwrap_or_default(),
            upstream_host: entry.upstream_host.clone().unwrap_or_default(),
            error_message: entry.error_message.clone().unwrap_or_default(),
            client_ip: entry.client_ip.clone().unwrap_or_default(),
            user_agent: entry.user_agent.clone().unwrap_or_default(),
            referrer: entry.referrer.clone().unwrap_or_default(),
            request_id: entry.request_id.clone(),
            ip_geolocation_id: entry.ip_geolocation_id,
            browser: entry.browser.clone().unwrap_or_default(),
            browser_version: entry.browser_version.clone().unwrap_or_default(),
            operating_system: entry.operating_system.clone().unwrap_or_default(),
            device_type: entry.device_type.clone().unwrap_or_default(),
            is_bot: entry.is_bot.map(u8::from),
            bot_name: entry.bot_name.clone().unwrap_or_default(),
            request_size_bytes: entry.request_size_bytes,
            response_size_bytes: entry.response_size_bytes,
            cache_status: entry.cache_status.clone().unwrap_or_default(),
            request_headers,
            response_headers,
            created_date: created_date_days,
            trace_id: entry.trace_id.clone().unwrap_or_default(),
            error_group_id: entry.error_group_id,
            _version: now_ms as u64,
            // Callers that hold a RetentionResolver should override this field
            // after construction. The fixed default matches the DDL DEFAULT.
            retention_days: temps_core::RetentionTable::ProxyLogs.default_days(),
        }
    }
}

// ── Read-side row types ──────────────────────────────────────────────────────

/// Full proxy-log row read from ClickHouse for the list and lookup paths.
///
/// Column aliases in the SELECT list must match these field names. Timestamp
/// columns are aliased to `*_ms` (Unix milliseconds) so they decode as `i64`.
#[derive(::clickhouse::Row, Deserialize, Debug)]
struct ChProxyLogReadRow {
    timestamp_ms: i64,
    method: String,
    path: String,
    query_string: String,
    host: String,
    status_code: i16,
    response_time_ms: Option<i32>,
    request_source: String,
    is_system_request: u8,
    routing_status: String,
    project_id: Option<i32>,
    environment_id: Option<i32>,
    deployment_id: Option<i32>,
    session_id: Option<i32>,
    visitor_id: Option<i32>,
    container_id: String,
    upstream_host: String,
    error_message: String,
    client_ip: String,
    user_agent: String,
    referrer: String,
    request_id: String,
    ip_geolocation_id: Option<i32>,
    browser: String,
    browser_version: String,
    operating_system: String,
    device_type: String,
    is_bot: Option<u8>,
    bot_name: String,
    request_size_bytes: Option<i64>,
    response_size_bytes: Option<i64>,
    cache_status: String,
}

/// Map a `""` sentinel column back to `Option::None` for the `Model` fields the
/// `ProxyLogResponse` exposes as `Option<String>`.
fn opt_str(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

impl ChProxyLogReadRow {
    /// Reconstruct a Sea-ORM `proxy_logs::Model` so the handler's existing
    /// `ProxyLogResponse::from(model)` mapping is reused unchanged.
    ///
    /// The columns NOT part of `ProxyLogResponse` (`id`, `created_date`,
    /// `request_headers`, `response_headers`, `trace_id`, `error_group_id`) are
    /// filled with backend-neutral placeholders — they are never read by the
    /// response mapping. `id` has no CH equivalent (no serial); see the
    /// module-level note. We surface `id = 0`; the response carries it but the
    /// UI keys rows by `request_id`.
    fn into_model(self) -> proxy_logs::Model {
        let timestamp = Utc
            .timestamp_millis_opt(self.timestamp_ms)
            .single()
            .unwrap_or_default();
        proxy_logs::Model {
            id: 0,
            timestamp,
            method: self.method,
            path: self.path,
            query_string: opt_str(self.query_string),
            host: self.host,
            status_code: self.status_code,
            response_time_ms: self.response_time_ms,
            request_source: self.request_source,
            is_system_request: self.is_system_request != 0,
            routing_status: self.routing_status,
            project_id: self.project_id,
            environment_id: self.environment_id,
            deployment_id: self.deployment_id,
            container_id: opt_str(self.container_id),
            upstream_host: opt_str(self.upstream_host),
            error_message: opt_str(self.error_message),
            client_ip: opt_str(self.client_ip),
            user_agent: opt_str(self.user_agent),
            referrer: opt_str(self.referrer),
            request_id: self.request_id,
            ip_geolocation_id: self.ip_geolocation_id,
            browser: opt_str(self.browser),
            browser_version: opt_str(self.browser_version),
            operating_system: opt_str(self.operating_system),
            device_type: opt_str(self.device_type),
            is_bot: self.is_bot.map(|b| b != 0),
            bot_name: opt_str(self.bot_name),
            request_size_bytes: self.request_size_bytes,
            response_size_bytes: self.response_size_bytes,
            cache_status: opt_str(self.cache_status),
            request_headers: None,
            response_headers: None,
            created_date: timestamp.date_naive(),
            session_id: self.session_id,
            visitor_id: self.visitor_id,
            trace_id: None,
            error_group_id: None,
        }
    }
}

/// Single-scalar count row.
#[derive(::clickhouse::Row, Deserialize, Debug)]
struct ChCountRow {
    cnt: u64,
}

/// One bucketed row for `get_time_bucket_stats`.
#[derive(::clickhouse::Row, Deserialize, Debug)]
struct ChTimeBucketRow {
    bucket_ms: i64,
    request_count: u64,
    avg_response_time_ms: f64,
    error_count: u64,
    total_request_bytes: i64,
    total_response_bytes: i64,
}

/// One per-project rollup row for `get_projects_health_summary`.
#[derive(::clickhouse::Row, Deserialize, Debug)]
struct ChProjectHealthRow {
    project_id: i32,
    total_requests: u64,
    total_errors: u64,
    avg_response_time_ms: f64,
}

/// One per-agent rollup row for `get_ai_agent_breakdown`.
#[derive(::clickhouse::Row, Deserialize, Debug)]
struct ChAiAgentRow {
    bot_name: String,
    request_count: u64,
    unique_ips: u64,
    last_seen_ms: i64,
}

/// One per-path rollup row for `get_ai_page_breakdown`.
#[derive(::clickhouse::Row, Deserialize, Debug)]
struct ChAiPageRow {
    path: String,
    request_count: u64,
    agent_count: u64,
    last_seen_ms: i64,
}

/// One (bucket, agent, count) row for `get_ai_agent_timeline`.
#[derive(::clickhouse::Row, Deserialize, Debug)]
struct ChAiTimelineRow {
    bucket_ms: i64,
    agent: String,
    request_count: u64,
}

/// One status-class rollup row for `get_ai_status_breakdown`.
#[derive(::clickhouse::Row, Deserialize, Debug)]
struct ChAiStatusRow {
    status_class: String,
    request_count: u64,
}

/// Convert a Unix-ms value to an RFC3339 string (matching the Postgres path's
/// `to_rfc3339()`).
fn ms_to_rfc3339(ms: i64) -> String {
    Utc.timestamp_millis_opt(ms)
        .single()
        .unwrap_or_default()
        .to_rfc3339()
}

// ── Store ────────────────────────────────────────────────────────────────────

/// ClickHouse-backed [`ProxyLogStorage`].
///
/// The `client` is cheap to clone (Arc-backed internally). Construction does no
/// I/O; run migrations separately via
/// [`super::clickhouse_migrations::apply_migrations`].
pub struct ClickHouseProxyLogStore {
    client: ::clickhouse::Client,
    /// Resolves the per-project `retention_days` value stamped onto each
    /// ingested proxy log row. The default [`temps_core::FixedRetentionResolver`]
    /// always returns 30; a plugin can register an alternative implementation.
    resolver: Arc<dyn temps_core::RetentionResolver>,
}

impl ClickHouseProxyLogStore {
    /// Build a store from connection configuration. Does not validate
    /// connectivity.
    ///
    /// `resolver` is called once per row in `write_batch` to populate
    /// `retention_days`. Pass `Arc::new(FixedRetentionResolver)` unless a
    /// plugin has registered a project-aware implementation.
    pub fn new(
        config: ClickHouseProxyLogConfig,
        resolver: Arc<dyn temps_core::RetentionResolver>,
    ) -> Self {
        let client = ::clickhouse::Client::default()
            .with_url(&config.url)
            .with_database(&config.database)
            .with_user(&config.user)
            .with_password(&config.password);
        Self { client, resolver }
    }

    /// Borrow the underlying client (for migrations / health checks).
    pub fn client(&self) -> &::clickhouse::Client {
        &self.client
    }

    /// Verify connectivity and authentication with `SELECT 1`.
    pub async fn health_check(&self) -> Result<(), ProxyLogServiceError> {
        self.client
            .query("SELECT 1")
            .fetch_one::<u8>()
            .await
            .map_err(|e| ProxyLogServiceError::ClickHouse {
                operation: "health_check".to_string(),
                reason: e.to_string(),
            })?;
        Ok(())
    }

    /// Server-derived list of known AI-agent canonical names. Identical to the
    /// list the TimescaleDB path binds into `bot_name = ANY(...)`.
    fn known_agent_names() -> Vec<String> {
        crate::ai_agent_detector::known_agents()
            .iter()
            .map(|(_, m)| m.agent.to_owned())
            .collect()
    }

    /// Build the dynamic WHERE fragment + bind list shared by the four AI-* stats
    /// queries: `timestamp >= ? AND timestamp < ? AND is_bot = 1 AND bot_name !=
    /// '' [AND project_id = ?] [AND environment_id = ?] [AND path = ?] AND
    /// bot_name IN ?`.
    ///
    /// `is_bot = 1` excludes NULL/0 (matching the Postgres `is_bot = true`), and
    /// `bot_name != ''` mirrors `bot_name IS NOT NULL` (the CH column stores ''
    /// for unset). The known-agent list is bound as an `Array(String)`.
    fn ai_prefilter(
        start_time: UtcDateTime,
        end_time: UtcDateTime,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        path: Option<&str>,
        known: Vec<String>,
    ) -> (String, Vec<Bv>) {
        let mut clauses: Vec<String> = vec![
            "timestamp >= fromUnixTimestamp64Milli(?)".to_string(),
            "timestamp < fromUnixTimestamp64Milli(?)".to_string(),
            "is_bot = 1".to_string(),
            "bot_name != ''".to_string(),
        ];
        let mut binds: Vec<Bv> = vec![
            Bv::I64(start_time.timestamp_millis()),
            Bv::I64(end_time.timestamp_millis()),
        ];

        if let Some(pid) = project_id {
            clauses.push("project_id = ?".to_string());
            binds.push(Bv::I32(pid));
        }
        if let Some(eid) = environment_id {
            clauses.push("environment_id = ?".to_string());
            binds.push(Bv::I32(eid));
        }
        if let Some(p) = path {
            clauses.push("path = ?".to_string());
            binds.push(Bv::Str(p.to_owned()));
        }

        clauses.push("bot_name IN ?".to_string());
        binds.push(Bv::StrVec(known));

        (clauses.join(" AND "), binds)
    }
}

/// Map a sort key (the validated `sort_by` allowlist) + order to a CH ORDER BY
/// clause column name. Mirrors the TimescaleDB `match` exactly so result
/// ordering is identical. Returns the column name; the caller appends the
/// direction. NONE of these are user strings — the `match` is the allowlist.
fn sort_column(sort_by: Option<&str>) -> &'static str {
    match sort_by {
        Some("timestamp") | None => "timestamp",
        Some("response_time") | Some("response_time_ms") => "response_time_ms",
        Some("status_code") => "status_code",
        Some("method") => "method",
        Some("host") => "host",
        Some("path") => "path",
        Some("request_size") | Some("request_size_bytes") => "request_size_bytes",
        Some("response_size") | Some("response_size_bytes") => "response_size_bytes",
        Some("client_ip") => "client_ip",
        Some("routing_status") => "routing_status",
        Some("project_id") => "project_id",
        Some("environment_id") => "environment_id",
        Some("deployment_id") => "deployment_id",
        Some("request_source") => "request_source",
        Some("browser") => "browser",
        Some("operating_system") => "operating_system",
        Some("device_type") => "device_type",
        Some("is_bot") => "is_bot",
        Some("is_system_request") => "is_system_request",
        _ => "timestamp",
    }
}

impl ClickHouseProxyLogStore {
    /// Translate the full `ProxyLogsQuery` (plus the separately-passed date
    /// bounds) into a WHERE fragment + ordered bind list. Every value is a bound
    /// `?`; the four substring filters use escaped ILIKE. Returns the joined
    /// clause (without the `WHERE` keyword) and a flag noting whether the query
    /// definitely matches no rows (the unknown-`ai_provider` case, mirroring the
    /// Postgres `Id.eq(-1)`).
    fn build_list_where(
        start_date: Option<UtcDateTime>,
        end_date: Option<UtcDateTime>,
        filters: &ProxyLogsQuery,
    ) -> (Vec<String>, Vec<Bv>, bool) {
        let mut clauses: Vec<String> = Vec::new();
        let mut binds: Vec<Bv> = Vec::new();
        let mut impossible = false;

        // IDs (equality)
        if let Some(pid) = filters.project_id {
            clauses.push("project_id = ?".into());
            binds.push(Bv::I32(pid));
        }
        if let Some(eid) = filters.environment_id {
            clauses.push("environment_id = ?".into());
            binds.push(Bv::I32(eid));
        }
        if let Some(did) = filters.deployment_id {
            clauses.push("deployment_id = ?".into());
            binds.push(Bv::I32(did));
        }
        if let Some(sid) = filters.session_id {
            clauses.push("session_id = ?".into());
            binds.push(Bv::I32(sid));
        }
        if let Some(vid) = filters.visitor_id {
            clauses.push("visitor_id = ?".into());
            binds.push(Bv::I32(vid));
        }

        // Date range
        if let Some(start) = start_date {
            clauses.push("timestamp >= fromUnixTimestamp64Milli(?)".into());
            binds.push(Bv::I64(start.timestamp_millis()));
        }
        if let Some(end) = end_date {
            clauses.push("timestamp <= fromUnixTimestamp64Milli(?)".into());
            binds.push(Bv::I64(end.timestamp_millis()));
        }

        // Request
        if let Some(ref method) = filters.method {
            clauses.push("method = ?".into());
            binds.push(Bv::Str(method.clone()));
        }
        if let Some(ref host) = filters.host {
            clauses.push("host ILIKE ?".into());
            binds.push(Bv::Str(format!("%{}%", escape_like_pattern(host))));
        }
        if let Some(ref path) = filters.path {
            clauses.push("path ILIKE ?".into());
            binds.push(Bv::Str(format!("%{}%", escape_like_pattern(path))));
        }
        if let Some(ref ip) = filters.client_ip {
            clauses.push("client_ip = ?".into());
            binds.push(Bv::Str(ip.clone()));
        }

        // Response
        if let Some(code) = filters.status_code {
            clauses.push("status_code = ?".into());
            binds.push(Bv::I16(code));
        }
        if let Some(min_time) = filters.response_time_min {
            clauses.push("response_time_ms >= ?".into());
            binds.push(Bv::I32(min_time));
        }
        if let Some(max_time) = filters.response_time_max {
            clauses.push("response_time_ms <= ?".into());
            binds.push(Bv::I32(max_time));
        }

        // Routing
        if let Some(ref status) = filters.routing_status {
            clauses.push("routing_status = ?".into());
            binds.push(Bv::Str(status.clone()));
        }
        if let Some(ref source) = filters.request_source {
            clauses.push("request_source = ?".into());
            binds.push(Bv::Str(source.clone()));
        }
        if let Some(is_system) = filters.is_system_request {
            clauses.push("is_system_request = ?".into());
            binds.push(Bv::Bool(is_system));
        }

        // User agent
        if let Some(ref ua) = filters.user_agent {
            clauses.push("user_agent ILIKE ?".into());
            binds.push(Bv::Str(format!("%{}%", escape_like_pattern(ua))));
        }
        if let Some(ref browser) = filters.browser {
            clauses.push("browser = ?".into());
            binds.push(Bv::Str(browser.clone()));
        }
        if let Some(ref os) = filters.operating_system {
            clauses.push("operating_system = ?".into());
            binds.push(Bv::Str(os.clone()));
        }
        if let Some(ref device) = filters.device_type {
            clauses.push("device_type = ?".into());
            binds.push(Bv::Str(device.clone()));
        }

        // Bot
        if let Some(is_bot) = filters.is_bot {
            // Postgres `is_bot = true/false` never matches NULL — `is_bot = ?`
            // in CH compares the Nullable(UInt8) and skips NULLs identically.
            clauses.push("is_bot = ?".into());
            binds.push(Bv::Bool(is_bot));
        }
        if let Some(ref bot_name) = filters.bot_name {
            clauses.push("bot_name ILIKE ?".into());
            binds.push(Bv::Str(format!("%{}%", escape_like_pattern(bot_name))));
        }

        // AI agent filters (canonical names persisted at ingest; equality / IN).
        if let Some(ref agent) = filters.ai_agent {
            clauses.push("is_bot = 1".into());
            clauses.push("bot_name = ?".into());
            binds.push(Bv::Str(agent.clone()));
        }
        if let Some(ref provider) = filters.ai_provider {
            let agents_for_provider: Vec<String> = crate::ai_agent_detector::known_agents()
                .iter()
                .filter(|(_, m)| m.provider.eq_ignore_ascii_case(provider))
                .map(|(_, m)| m.agent.to_string())
                .collect();
            if agents_for_provider.is_empty() {
                // Unknown provider — match nothing (Postgres uses Id.eq(-1)).
                impossible = true;
            } else {
                clauses.push("is_bot = 1".into());
                clauses.push("bot_name IN ?".into());
                binds.push(Bv::StrVec(agents_for_provider));
            }
        }
        match filters.is_ai_agent {
            Some(true) => {
                clauses.push("is_bot = 1".into());
                clauses.push("bot_name IN ?".into());
                binds.push(Bv::StrVec(ClickHouseProxyLogStore::known_agent_names()));
            }
            Some(false) => {
                clauses.push("bot_name NOT IN ?".into());
                binds.push(Bv::StrVec(ClickHouseProxyLogStore::known_agent_names()));
            }
            None => {}
        }

        // Size
        if let Some(v) = filters.request_size_min {
            clauses.push("request_size_bytes >= ?".into());
            binds.push(Bv::I64(v));
        }
        if let Some(v) = filters.request_size_max {
            clauses.push("request_size_bytes <= ?".into());
            binds.push(Bv::I64(v));
        }
        if let Some(v) = filters.response_size_min {
            clauses.push("response_size_bytes >= ?".into());
            binds.push(Bv::I64(v));
        }
        if let Some(v) = filters.response_size_max {
            clauses.push("response_size_bytes <= ?".into());
            binds.push(Bv::I64(v));
        }

        // Cache
        if let Some(ref cache_status) = filters.cache_status {
            clauses.push("cache_status = ?".into());
            binds.push(Bv::Str(cache_status.clone()));
        }

        // Container
        if let Some(ref container_id) = filters.container_id {
            clauses.push("container_id = ?".into());
            binds.push(Bv::Str(container_id.clone()));
        }
        if let Some(ref upstream_host) = filters.upstream_host {
            clauses.push("upstream_host ILIKE ?".into());
            binds.push(Bv::Str(format!("%{}%", escape_like_pattern(upstream_host))));
        }

        // Error presence. The CH column stores '' for "no error" (the Postgres
        // column is NULL); `error_message != ''` ≡ `IS NOT NULL`, and `= ''` ≡
        // `IS NULL`, preserving the has_error semantics.
        if let Some(has_error) = filters.has_error {
            if has_error {
                clauses.push("error_message != ''".into());
            } else {
                clauses.push("error_message = ''".into());
            }
        }

        (clauses, binds, impossible)
    }
}

#[async_trait]
impl ProxyLogStorage for ClickHouseProxyLogStore {
    async fn write_batch(
        &self,
        entries: Vec<CreateProxyLogRequest>,
    ) -> Result<(), ProxyLogServiceError> {
        if entries.is_empty() {
            return Ok(());
        }

        for chunk in entries.chunks(MAX_INSERT_BATCH) {
            let mut inserter = self
                .client
                .insert::<ChProxyLogRow>("proxy_logs")
                .await
                .map_err(|e| ProxyLogServiceError::ClickHouse {
                    operation: "write_batch (inserter setup)".to_string(),
                    reason: e.to_string(),
                })?;

            for entry in chunk {
                let mut row = ChProxyLogRow::from(entry);
                // Unrouted requests have no project context; use the table
                // default directly so the resolver is not called with a
                // fabricated project ID.
                row.retention_days = match entry.project_id {
                    Some(pid) => self
                        .resolver
                        .resolve(pid, temps_core::RetentionTable::ProxyLogs),
                    None => temps_core::RetentionTable::ProxyLogs.default_days(),
                };
                inserter
                    .write(&row)
                    .await
                    .map_err(|e| ProxyLogServiceError::ClickHouse {
                        operation: "write_batch (write)".to_string(),
                        reason: e.to_string(),
                    })?;
            }

            inserter
                .end()
                .await
                .map_err(|e| ProxyLogServiceError::ClickHouse {
                    operation: "write_batch (end)".to_string(),
                    reason: e.to_string(),
                })?;
        }

        Ok(())
    }

    async fn list_with_filters(
        &self,
        start_date: Option<UtcDateTime>,
        end_date: Option<UtcDateTime>,
        filters: ProxyLogsQuery,
        page: u64,
        page_size: u64,
    ) -> Result<(Vec<proxy_logs::Model>, u64), ProxyLogServiceError> {
        let (clauses, _, impossible) = Self::build_list_where(start_date, end_date, &filters);

        // Unknown ai_provider → guaranteed empty, no round-trip.
        if impossible {
            return Ok((vec![], 0));
        }

        let where_clause = if clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", clauses.join(" AND "))
        };

        // ── COUNT(*) total (same WHERE) ──────────────────────────────────────
        // FINAL: proxy_logs is a ReplacingMergeTree, and an insert retry or a
        // re-run of the TimescaleDB→ClickHouse backfill can leave duplicate rows
        // that ClickHouse only collapses on a later merge — and two copies in
        // different parts may never merge together without OPTIMIZE. Without
        // FINAL this count() over-reports the pagination total. FINAL forces
        // merge-on-read so the total matches the deduplicated result set.
        let count_sql = format!("SELECT count() AS cnt FROM proxy_logs FINAL {where_clause}");
        let (_, count_binds, _) = Self::build_list_where(start_date, end_date, &filters);
        let count_q = apply_binds(self.client.query(&count_sql), count_binds);
        let total = count_q
            .fetch_one::<ChCountRow>()
            .await
            .map_err(|e| ProxyLogServiceError::ClickHouse {
                operation: "list_with_filters (count)".to_string(),
                reason: e.to_string(),
            })?
            .cnt;

        // ── Page fetch ───────────────────────────────────────────────────────
        // ORDER BY column is allowlist-derived; direction is a fixed enum. Both
        // are safe to interpolate (no user string reaches the SQL).
        let order_col = sort_column(filters.sort_by.as_deref());
        let order_dir = match filters.sort_order.as_deref() {
            Some("asc") => "ASC",
            _ => "DESC",
        };
        let offset = (page.saturating_sub(1)) * page_size;

        let select_sql = format!(
            "SELECT \
                toUnixTimestamp64Milli(timestamp) AS timestamp_ms, method, path, query_string, \
                host, status_code, response_time_ms, request_source, is_system_request, \
                routing_status, project_id, environment_id, deployment_id, session_id, visitor_id, \
                container_id, upstream_host, error_message, client_ip, user_agent, referrer, \
                request_id, ip_geolocation_id, browser, browser_version, operating_system, \
                device_type, is_bot, bot_name, request_size_bytes, response_size_bytes, cache_status \
             FROM proxy_logs FINAL \
             {where_clause} \
             ORDER BY {order_col} {order_dir} \
             LIMIT ? OFFSET ?"
        );

        let (_, mut binds, _) = Self::build_list_where(start_date, end_date, &filters);
        binds.push(Bv::I64(page_size as i64));
        binds.push(Bv::I64(offset as i64));

        let q = apply_binds(self.client.query(&select_sql), binds);
        let rows = q.fetch_all::<ChProxyLogReadRow>().await.map_err(|e| {
            ProxyLogServiceError::ClickHouse {
                operation: "list_with_filters (page)".to_string(),
                reason: e.to_string(),
            }
        })?;

        let models = rows
            .into_iter()
            .map(ChProxyLogReadRow::into_model)
            .collect();
        Ok((models, total))
    }

    async fn get_by_id(
        &self,
        id: i32,
        _timestamp: Option<UtcDateTime>,
    ) -> Result<Option<proxy_logs::Model>, ProxyLogServiceError> {
        // ClickHouse proxy_logs has no serial `id` column (see the schema note).
        // The list rows surface id=0 and the UI keys off request_id, so an
        // id-based lookup cannot resolve a CH row. Return None (404) rather than
        // fabricate a row; callers should use get_by_request_id under CH.
        warn!(
            id,
            "get_by_id is not supported under the ClickHouse proxy-log backend \
             (no serial id); returning None — use get_by_request_id"
        );
        Ok(None)
    }

    async fn get_by_request_id(
        &self,
        request_id: &str,
        timestamp: Option<UtcDateTime>,
    ) -> Result<Option<proxy_logs::Model>, ProxyLogServiceError> {
        // request_id is the 3rd ORDER BY element, so a request_id-only lookup
        // has no sort-key prefix and scans every partition. When the caller
        // knows the row's event time (the list endpoint returns it per row), a
        // ±1-day timestamp bound prunes the scan to the monthly partition(s)
        // that can contain the row.
        let time_clause = if timestamp.is_some() {
            " AND timestamp >= fromUnixTimestamp64Milli(?) \
              AND timestamp <= fromUnixTimestamp64Milli(?)"
        } else {
            ""
        };
        let sql = format!(
            "SELECT \
                toUnixTimestamp64Milli(timestamp) AS timestamp_ms, method, path, query_string, \
                host, status_code, response_time_ms, request_source, is_system_request, \
                routing_status, project_id, environment_id, deployment_id, session_id, visitor_id, \
                container_id, upstream_host, error_message, client_ip, user_agent, referrer, \
                request_id, ip_geolocation_id, browser, browser_version, operating_system, \
                device_type, is_bot, bot_name, request_size_bytes, response_size_bytes, cache_status \
             FROM proxy_logs \
             WHERE request_id = ?{time_clause} \
             ORDER BY timestamp DESC \
             LIMIT 1"
        );

        let mut q = self.client.query(&sql).bind(request_id);
        if let Some(ts) = timestamp {
            // Checked arithmetic saturating at the representable range: a bare
            // `ts + Duration::days(1)` panics on overflow, and `ts` comes from a
            // user-supplied query parameter.
            let day = chrono::Duration::days(1);
            let lo = ts
                .checked_sub_signed(day)
                .unwrap_or(chrono::DateTime::<Utc>::MIN_UTC);
            let hi = ts
                .checked_add_signed(day)
                .unwrap_or(chrono::DateTime::<Utc>::MAX_UTC);
            q = q.bind(lo.timestamp_millis()).bind(hi.timestamp_millis());
        }
        let row = q.fetch_optional::<ChProxyLogReadRow>().await.map_err(|e| {
            ProxyLogServiceError::ClickHouse {
                operation: "get_by_request_id".to_string(),
                reason: e.to_string(),
            }
        })?;

        Ok(row.map(ChProxyLogReadRow::into_model))
    }

    async fn get_today_count(
        &self,
        filters: Option<StatsFilters>,
    ) -> Result<i64, ProxyLogServiceError> {
        let today_start = Utc::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .map(|d| chrono::DateTime::<Utc>::from_naive_utc_and_offset(d, Utc))
            .unwrap_or_else(Utc::now);

        let mut clauses: Vec<String> = vec!["timestamp >= fromUnixTimestamp64Milli(?)".to_string()];
        let mut binds: Vec<Bv> = vec![Bv::I64(today_start.timestamp_millis())];

        if let Some(ref f) = filters {
            Self::append_stats_filters(f, &mut clauses, &mut binds);
        }

        // FINAL: dedup ReplacingMergeTree before count() (see the list-count
        // note above) so retried inserts / backfill re-runs don't inflate this.
        let sql = format!(
            "SELECT count() AS cnt FROM proxy_logs FINAL WHERE {}",
            clauses.join(" AND ")
        );
        let q = apply_binds(self.client.query(&sql), binds);
        let cnt = q
            .fetch_one::<ChCountRow>()
            .await
            .map_err(|e| ProxyLogServiceError::ClickHouse {
                operation: "get_today_count".to_string(),
                reason: e.to_string(),
            })?
            .cnt;
        Ok(cnt as i64)
    }

    async fn get_time_bucket_stats(
        &self,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
        bucket_interval: String,
        filters: Option<StatsFilters>,
    ) -> Result<Vec<TimeBucketStats>, ProxyLogServiceError> {
        if !ProxyLogService::is_valid_interval(&bucket_interval) {
            return Err(ProxyLogServiceError::InvalidFilter(format!(
                "Invalid bucket interval: {}",
                bucket_interval
            )));
        }
        let step_secs = interval_to_seconds(&bucket_interval).ok_or_else(|| {
            ProxyLogServiceError::InvalidFilter(format!(
                "Invalid bucket interval: {}",
                bucket_interval
            ))
        })?;

        // WHERE: timestamp >= start AND timestamp < end [AND stats filters].
        let mut clauses: Vec<String> = vec![
            "timestamp >= fromUnixTimestamp64Milli(?)".to_string(),
            "timestamp < fromUnixTimestamp64Milli(?)".to_string(),
        ];
        let mut binds: Vec<Bv> = vec![
            Bv::I64(start_time.timestamp_millis()),
            Bv::I64(end_time.timestamp_millis()),
        ];
        if let Some(ref f) = filters {
            Self::append_stats_filters(f, &mut clauses, &mut binds);
        }

        // toStartOfInterval(timestamp, INTERVAL N SECOND) buckets at the second
        // grid; WITH FILL reproduces `time_bucket_gapfill` so empty buckets are
        // present (the frontend relies on a continuous x-axis). step_secs is a
        // server-derived integer (validated interval → seconds), safe to
        // interpolate; the FILL bounds are bound params. The bucket expression
        // lives in the SELECT so GROUP BY / ORDER BY / WITH FILL reference the
        // `bucket_ms` alias.
        let sql = format!(
            "SELECT \
                toUnixTimestamp(toStartOfInterval(timestamp, INTERVAL {step} SECOND)) * 1000 AS bucket_ms, \
                count() AS request_count, \
                ifNull(avg(response_time_ms), 0) AS avg_response_time_ms, \
                countIf(status_code >= 400) AS error_count, \
                sum(ifNull(request_size_bytes, 0)) AS total_request_bytes, \
                sum(ifNull(response_size_bytes, 0)) AS total_response_bytes \
             FROM proxy_logs FINAL \
             WHERE {where_clause} \
             GROUP BY bucket_ms \
             ORDER BY bucket_ms ASC \
             WITH FILL \
                FROM toUnixTimestamp(toStartOfInterval(fromUnixTimestamp64Milli(?), INTERVAL {step} SECOND)) * 1000 \
                TO toUnixTimestamp(toStartOfInterval(fromUnixTimestamp64Milli(?), INTERVAL {step} SECOND)) * 1000 \
                STEP {step_ms}",
            where_clause = clauses.join(" AND "),
            step = step_secs,
            step_ms = step_secs * 1000,
        );

        // FILL bounds (start, end) are bound AFTER the WHERE binds.
        binds.push(Bv::I64(start_time.timestamp_millis()));
        binds.push(Bv::I64(end_time.timestamp_millis()));

        let q = apply_binds(self.client.query(&sql), binds);
        let rows = q.fetch_all::<ChTimeBucketRow>().await.map_err(|e| {
            ProxyLogServiceError::ClickHouse {
                operation: "get_time_bucket_stats".to_string(),
                reason: e.to_string(),
            }
        })?;

        let stats = rows
            .into_iter()
            .map(|r| TimeBucketStats {
                bucket: ms_to_rfc3339(r.bucket_ms),
                request_count: r.request_count as i64,
                // SQL already coerces avg over an empty/all-NULL bucket to 0 via
                // ifNull (matching Postgres COALESCE(avg, 0)). The is_nan guard
                // is a defensive belt-and-braces for any future SQL change.
                avg_response_time_ms: if r.avg_response_time_ms.is_nan() {
                    0.0
                } else {
                    r.avg_response_time_ms
                },
                error_count: r.error_count as i64,
                total_request_bytes: r.total_request_bytes,
                total_response_bytes: r.total_response_bytes,
            })
            .collect();

        Ok(stats)
    }

    async fn get_projects_health_summary(
        &self,
        project_ids: &[i32],
        start_time: UtcDateTime,
        end_time: UtcDateTime,
        is_bot: Option<bool>,
    ) -> Result<Vec<ProjectHealthSummary>, ProxyLogServiceError> {
        if project_ids.is_empty() {
            return Ok(vec![]);
        }

        // project_id is Nullable(Int32); CH `IN (array of Int32)` skips NULL
        // rows, matching the Postgres `project_id IN (...)`. is_bot is an
        // optional trailing `is_bot = ?`. We bind the i32 array directly (not
        // via the String-based `Bv::StrVec`) so types match the column.
        let mut clauses: Vec<String> = vec![
            "timestamp >= fromUnixTimestamp64Milli(?)".to_string(),
            "timestamp < fromUnixTimestamp64Milli(?)".to_string(),
            "project_id IN ?".to_string(),
        ];
        let project_id_array: Vec<i32> = project_ids.to_vec();

        if is_bot.is_some() {
            clauses.push("is_bot = ?".to_string());
        }

        let sql = format!(
            "SELECT \
                assumeNotNull(project_id) AS project_id, \
                count() AS total_requests, \
                countIf(status_code >= 500) AS total_errors, \
                ifNull(avg(response_time_ms), 0) AS avg_response_time_ms \
             FROM proxy_logs FINAL \
             WHERE {} \
             GROUP BY project_id",
            clauses.join(" AND ")
        );

        // Bind in clause order: start, end, project_id array, [is_bot].
        let mut q = self
            .client
            .query(&sql)
            .bind(start_time.timestamp_millis())
            .bind(end_time.timestamp_millis())
            .bind(project_id_array);
        if let Some(flag) = is_bot {
            q = q.bind(flag);
        }

        let rows = q.fetch_all::<ChProjectHealthRow>().await.map_err(|e| {
            ProxyLogServiceError::ClickHouse {
                operation: "get_projects_health_summary".to_string(),
                reason: e.to_string(),
            }
        })?;

        // Build a map, derive status in Rust (identical thresholds to Postgres).
        let mut summaries: std::collections::HashMap<i32, ProjectHealthSummary> =
            std::collections::HashMap::new();
        for row in rows {
            let total_requests = row.total_requests as i64;
            let total_errors = row.total_errors as i64;
            // SQL coerces NULL avg → 0 via ifNull; guard is defensive only.
            let avg = if row.avg_response_time_ms.is_nan() {
                0.0
            } else {
                row.avg_response_time_ms
            };
            let error_rate = if total_requests > 0 {
                (total_errors as f64 / total_requests as f64) * 100.0
            } else {
                0.0
            };
            let status = if total_requests == 0 {
                "unknown".to_string()
            } else if error_rate > 50.0 {
                "down".to_string()
            } else if error_rate > 10.0 {
                "degraded".to_string()
            } else {
                "healthy".to_string()
            };
            summaries.insert(
                row.project_id,
                ProjectHealthSummary {
                    project_id: row.project_id,
                    total_requests,
                    total_errors,
                    avg_response_time_ms: (avg * 10.0).round() / 10.0,
                    error_rate: (error_rate * 10.0).round() / 10.0,
                    status,
                },
            );
        }

        // Preserve input order; missing projects → "unknown".
        let result = project_ids
            .iter()
            .map(|&id| {
                summaries.remove(&id).unwrap_or(ProjectHealthSummary {
                    project_id: id,
                    total_requests: 0,
                    total_errors: 0,
                    avg_response_time_ms: 0.0,
                    error_rate: 0.0,
                    status: "unknown".to_string(),
                })
            })
            .collect();

        Ok(result)
    }

    async fn get_ai_agent_breakdown(
        &self,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        path: Option<String>,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
        limit: u64,
    ) -> Result<Vec<AiAgentBreakdownRow>, ProxyLogServiceError> {
        let known = Self::known_agent_names();
        if known.is_empty() {
            return Ok(vec![]);
        }

        let (where_clause, binds) = Self::ai_prefilter(
            start_time,
            end_time,
            project_id,
            environment_id,
            path.as_deref(),
            known,
        );

        // limit is server-derived (handler-capped) — clamp again to ≤100 and
        // interpolate as a validated integer.
        let lim = std::cmp::min(limit, 100);
        let sql = format!(
            "SELECT \
                bot_name, \
                count() AS request_count, \
                uniqExact(client_ip) AS unique_ips, \
                toUnixTimestamp64Milli(max(timestamp)) AS last_seen_ms \
             FROM proxy_logs FINAL \
             WHERE {where_clause} \
             GROUP BY bot_name \
             ORDER BY request_count DESC \
             LIMIT {lim}"
        );

        let q = apply_binds(self.client.query(&sql), binds);
        let rows =
            q.fetch_all::<ChAiAgentRow>()
                .await
                .map_err(|e| ProxyLogServiceError::ClickHouse {
                    operation: "get_ai_agent_breakdown".to_string(),
                    reason: e.to_string(),
                })?;

        // Map agent → (provider, purpose); drop rows for unknown agents (same as
        // the Postgres filter_map).
        let agent_index: std::collections::HashMap<
            &'static str,
            &'static crate::ai_agent_detector::AiAgentMatch,
        > = crate::ai_agent_detector::known_agents()
            .iter()
            .map(|(_, m)| (m.agent, m))
            .collect();

        let result = rows
            .into_iter()
            .filter_map(|row| {
                let meta = agent_index.get(row.bot_name.as_str())?;
                Some(AiAgentBreakdownRow {
                    provider: meta.provider.to_string(),
                    agent: meta.agent.to_string(),
                    purpose: meta.purpose.as_str().to_string(),
                    request_count: row.request_count as i64,
                    unique_ips: row.unique_ips as i64,
                    last_seen: if row.last_seen_ms == 0 {
                        None
                    } else {
                        Some(ms_to_rfc3339(row.last_seen_ms))
                    },
                })
            })
            .collect();

        Ok(result)
    }

    async fn get_ai_page_breakdown(
        &self,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        path: Option<String>,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
        limit: u64,
    ) -> Result<Vec<AiPageBreakdownRow>, ProxyLogServiceError> {
        let known = Self::known_agent_names();
        if known.is_empty() {
            return Ok(vec![]);
        }

        let (where_clause, binds) = Self::ai_prefilter(
            start_time,
            end_time,
            project_id,
            environment_id,
            path.as_deref(),
            known,
        );

        let lim = std::cmp::min(limit, 100);
        let sql = format!(
            "SELECT \
                path, \
                count() AS request_count, \
                uniqExact(bot_name) AS agent_count, \
                toUnixTimestamp64Milli(max(timestamp)) AS last_seen_ms \
             FROM proxy_logs FINAL \
             WHERE {where_clause} \
             GROUP BY path \
             ORDER BY request_count DESC \
             LIMIT {lim}"
        );

        let q = apply_binds(self.client.query(&sql), binds);
        let rows =
            q.fetch_all::<ChAiPageRow>()
                .await
                .map_err(|e| ProxyLogServiceError::ClickHouse {
                    operation: "get_ai_page_breakdown".to_string(),
                    reason: e.to_string(),
                })?;

        let result = rows
            .into_iter()
            .map(|row| AiPageBreakdownRow {
                path: row.path,
                request_count: row.request_count as i64,
                agent_count: row.agent_count as i64,
                last_seen: if row.last_seen_ms == 0 {
                    None
                } else {
                    Some(ms_to_rfc3339(row.last_seen_ms))
                },
            })
            .collect();

        Ok(result)
    }

    async fn get_ai_agent_timeline(
        &self,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
        bucket_interval: String,
        group_by: AiTimelineGroupBy,
    ) -> Result<Vec<AiAgentTimelineRow>, ProxyLogServiceError> {
        if !ProxyLogService::is_valid_interval(&bucket_interval) {
            return Err(ProxyLogServiceError::InvalidFilter(format!(
                "Invalid bucket interval: {}",
                bucket_interval
            )));
        }
        let step_secs = interval_to_seconds(&bucket_interval).ok_or_else(|| {
            ProxyLogServiceError::InvalidFilter(format!(
                "Invalid bucket interval: {}",
                bucket_interval
            ))
        })?;

        let known = Self::known_agent_names();
        if known.is_empty() {
            return Ok(vec![]);
        }

        // The AI pre-filter (no path) — same as the breakdown.
        let (where_clause, mut binds) = Self::ai_prefilter(
            start_time,
            end_time,
            project_id,
            environment_id,
            None,
            known,
        );

        // Group by bucket + bot_name, WITH FILL on the bucket axis so EVERY
        // bucket in the window appears (reproducing the generate_series spine
        // the Postgres path builds — the frontend relies on a continuous
        // x-axis). The empty FILL buckets come back with agent='' and count 0;
        // the Rust roll-up below treats agent='' as an x-axis-only marker,
        // exactly like the Postgres NULL-agent spine rows.
        let sql = format!(
            "SELECT \
                toUnixTimestamp(toStartOfInterval(timestamp, INTERVAL {step} SECOND)) * 1000 AS bucket_ms, \
                bot_name AS agent, \
                count() AS request_count \
             FROM proxy_logs FINAL \
             WHERE {where_clause} \
             GROUP BY bucket_ms, bot_name \
             ORDER BY bucket_ms ASC \
             WITH FILL \
                FROM toUnixTimestamp(toStartOfInterval(fromUnixTimestamp64Milli(?), INTERVAL {step} SECOND)) * 1000 \
                TO toUnixTimestamp(toStartOfInterval(fromUnixTimestamp64Milli(?), INTERVAL {step} SECOND)) * 1000 \
                STEP {step_ms}",
            step = step_secs,
            step_ms = step_secs * 1000,
        );

        // FILL bounds appended after the pre-filter binds.
        binds.push(Bv::I64(start_time.timestamp_millis()));
        binds.push(Bv::I64(end_time.timestamp_millis()));

        let q = apply_binds(self.client.query(&sql), binds);
        let rows = q.fetch_all::<ChAiTimelineRow>().await.map_err(|e| {
            ProxyLogServiceError::ClickHouse {
                operation: "get_ai_agent_timeline".to_string(),
                reason: e.to_string(),
            }
        })?;

        // ── Rust-side roll-up + empty markers (identical to the Postgres path) ──
        let agent_index: std::collections::HashMap<
            &'static str,
            &'static crate::ai_agent_detector::AiAgentMatch,
        > = crate::ai_agent_detector::known_agents()
            .iter()
            .map(|(_, m)| (m.agent, m))
            .collect();

        let mut acc: std::collections::HashMap<(String, String), i64> =
            std::collections::HashMap::new();
        let mut all_buckets: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

        for row in rows {
            let bucket_iso = ms_to_rfc3339(row.bucket_ms);
            all_buckets.insert(bucket_iso.clone());

            // FILL spine buckets carry agent='' — x-axis only, no series count.
            if row.agent.is_empty() {
                continue;
            }
            let count = row.request_count as i64;

            let key = match group_by {
                AiTimelineGroupBy::Agent => row.agent.clone(),
                AiTimelineGroupBy::Provider => agent_index
                    .get(row.agent.as_str())
                    .map(|m| m.provider.to_string())
                    .unwrap_or_else(|| row.agent.clone()),
            };

            *acc.entry((bucket_iso, key)).or_insert(0) += count;
        }

        let mut result: Vec<AiAgentTimelineRow> = acc
            .into_iter()
            .map(|((bucket, key), count)| AiAgentTimelineRow {
                bucket,
                key,
                request_count: count,
            })
            .collect();

        let buckets_with_data: std::collections::HashSet<&str> =
            result.iter().map(|r| r.bucket.as_str()).collect();
        let empty_markers: Vec<AiAgentTimelineRow> = all_buckets
            .iter()
            .filter(|b| !buckets_with_data.contains(b.as_str()))
            .map(|b| AiAgentTimelineRow {
                bucket: b.clone(),
                key: String::new(),
                request_count: 0,
            })
            .collect();
        result.extend(empty_markers);

        result.sort_by(|a, b| a.bucket.cmp(&b.bucket).then_with(|| a.key.cmp(&b.key)));

        Ok(result)
    }

    async fn get_ai_status_breakdown(
        &self,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        start_time: UtcDateTime,
        end_time: UtcDateTime,
    ) -> Result<Vec<AiStatusBreakdownRow>, ProxyLogServiceError> {
        let known = Self::known_agent_names();
        if known.is_empty() {
            return Ok(vec![]);
        }

        let (where_clause, binds) = Self::ai_prefilter(
            start_time,
            end_time,
            project_id,
            environment_id,
            None,
            known,
        );

        // multiIf reproduces the Postgres CASE status-class buckets exactly. The
        // class labels are constants (server-derived), safe to interpolate.
        let sql = format!(
            "SELECT \
                multiIf( \
                    status_code >= 200 AND status_code < 300, '2xx', \
                    status_code >= 300 AND status_code < 400, '3xx', \
                    status_code >= 400 AND status_code < 500, '4xx', \
                    status_code >= 500 AND status_code < 600, '5xx', \
                    'other') AS status_class, \
                count() AS request_count \
             FROM proxy_logs FINAL \
             WHERE {where_clause} \
             GROUP BY status_class \
             ORDER BY request_count DESC"
        );

        let q = apply_binds(self.client.query(&sql), binds);
        let rows =
            q.fetch_all::<ChAiStatusRow>()
                .await
                .map_err(|e| ProxyLogServiceError::ClickHouse {
                    operation: "get_ai_status_breakdown".to_string(),
                    reason: e.to_string(),
                })?;

        let result = rows
            .into_iter()
            .map(|row| AiStatusBreakdownRow {
                status_class: row.status_class,
                request_count: row.request_count as i64,
            })
            .collect();

        Ok(result)
    }
}

impl ClickHouseProxyLogStore {
    /// Append the `StatsFilters` predicates (used by `today` + `time-buckets`)
    /// as bound `?` clauses. Mirrors the Postgres `build_filter_sql` /
    /// `add_filter_values` pair exactly (same fields, same order, host = EQ).
    fn append_stats_filters(f: &StatsFilters, clauses: &mut Vec<String>, binds: &mut Vec<Bv>) {
        if let Some(ref method) = f.method {
            clauses.push("method = ?".into());
            binds.push(Bv::Str(method.clone()));
        }
        if let Some(ref ip) = f.client_ip {
            clauses.push("client_ip = ?".into());
            binds.push(Bv::Str(ip.clone()));
        }
        if let Some(pid) = f.project_id {
            clauses.push("project_id = ?".into());
            binds.push(Bv::I32(pid));
        }
        if let Some(eid) = f.environment_id {
            clauses.push("environment_id = ?".into());
            binds.push(Bv::I32(eid));
        }
        if let Some(did) = f.deployment_id {
            clauses.push("deployment_id = ?".into());
            binds.push(Bv::I32(did));
        }
        if let Some(ref host) = f.host {
            // NOTE: stats host filter is EQUALITY (the list filter is substring).
            clauses.push("host = ?".into());
            binds.push(Bv::Str(host.clone()));
        }
        if let Some(code) = f.status_code {
            clauses.push("status_code = ?".into());
            binds.push(Bv::I16(code));
        }
        if let Some(ref class) = f.status_code_class {
            if let Some((min, max)) = status_class_range(class) {
                clauses.push("status_code >= ?".into());
                binds.push(Bv::I16(min));
                clauses.push("status_code < ?".into());
                binds.push(Bv::I16(max));
            }
        }
        if let Some(ref routing_status) = f.routing_status {
            clauses.push("routing_status = ?".into());
            binds.push(Bv::Str(routing_status.clone()));
        }
        if let Some(ref request_source) = f.request_source {
            clauses.push("request_source = ?".into());
            binds.push(Bv::Str(request_source.clone()));
        }
        if let Some(is_bot) = f.is_bot {
            clauses.push("is_bot = ?".into());
            binds.push(Bv::Bool(is_bot));
        }
        if let Some(ref device_type) = f.device_type {
            clauses.push("device_type = ?".into());
            binds.push(Bv::Str(device_type.clone()));
        }
        if let Some(has_project) = f.has_project {
            // Fully-in-SQL predicate, no bound value (matches Postgres).
            clauses.push(if has_project {
                "project_id IS NOT NULL".into()
            } else {
                "project_id IS NULL".into()
            });
        }
    }
}

/// Convert a status-class label to a `(min, max)` half-open range. Mirrors the
/// private `ProxyLogService::status_class_range`.
fn status_class_range(class: &str) -> Option<(i16, i16)> {
    match class {
        "1xx" => Some((100, 200)),
        "2xx" => Some((200, 300)),
        "3xx" => Some((300, 400)),
        "4xx" => Some((400, 500)),
        "5xx" => Some((500, 600)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(request_id: &str) -> CreateProxyLogRequest {
        CreateProxyLogRequest {
            method: "GET".to_string(),
            path: "/hello".to_string(),
            query_string: Some("a=1".to_string()),
            host: "example.com".to_string(),
            status_code: 200,
            response_time_ms: Some(12),
            request_source: "proxy".to_string(),
            is_system_request: false,
            routing_status: "routed".to_string(),
            project_id: Some(7),
            environment_id: Some(3),
            deployment_id: None,
            session_id: None,
            visitor_id: None,
            container_id: None,
            upstream_host: Some("up.internal".to_string()),
            error_message: None,
            client_ip: Some("127.0.0.1".to_string()),
            user_agent: Some("Mozilla".to_string()),
            referrer: None,
            request_id: request_id.to_string(),
            ip_geolocation_id: None,
            browser: Some("Firefox".to_string()),
            browser_version: Some("1.0".to_string()),
            operating_system: Some("Linux".to_string()),
            device_type: Some("desktop".to_string()),
            is_bot: Some(false),
            bot_name: None,
            request_size_bytes: Some(100),
            response_size_bytes: Some(2048),
            cache_status: None,
            request_headers: Some(serde_json::json!({"x": "y"})),
            response_headers: None,
            trace_id: Some("abc123".to_string()),
            error_group_id: None,
            visitor_uuid: None,
            session_uuid: None,
        }
    }

    #[test]
    fn config_debug_masks_password() {
        let cfg =
            ClickHouseProxyLogConfig::new("http://localhost:8123", "otel", "temps", "super-secret");
        let dbg = format!("{cfg:?}");
        assert!(dbg.contains("***"));
        assert!(!dbg.contains("super-secret"));
        assert!(dbg.contains("otel"));
    }

    #[test]
    fn escape_like_pattern_escapes_metacharacters() {
        assert_eq!(escape_like_pattern("a%b"), "a\\%b");
        assert_eq!(escape_like_pattern("a_b"), "a\\_b");
        assert_eq!(escape_like_pattern("a\\b"), "a\\\\b");
        // backslash must be escaped first so we don't double-escape.
        assert_eq!(escape_like_pattern("%_\\"), "\\%\\_\\\\");
        // plain text is unchanged.
        assert_eq!(escape_like_pattern("/api/users"), "/api/users");
    }

    #[test]
    fn interval_to_seconds_maps_common_units() {
        assert_eq!(interval_to_seconds("1 hour"), Some(3_600));
        assert_eq!(interval_to_seconds("5 minutes"), Some(300));
        assert_eq!(interval_to_seconds("1 day"), Some(86_400));
        assert_eq!(interval_to_seconds("30 seconds"), Some(30));
        assert_eq!(interval_to_seconds("2 weeks"), Some(1_209_600));
        // sub-second collapses to a 1s floor.
        assert_eq!(interval_to_seconds("500 milliseconds"), Some(500));
        assert_eq!(interval_to_seconds("1 millisecond"), Some(1));
        // malformed → None.
        assert_eq!(interval_to_seconds("hour"), None);
        assert_eq!(interval_to_seconds("1 fortnight"), None);
        assert_eq!(interval_to_seconds(""), None);
        // absurd counts must saturate to the 10-year cap, not overflow i64
        // (guards step_secs * 1000 downstream).
        const MAX: i64 = 10 * 31_536_000;
        assert_eq!(interval_to_seconds("4294967295 years"), Some(MAX));
        assert_eq!(interval_to_seconds("9223372036854775807 years"), Some(MAX));
    }

    #[test]
    fn row_conversion_maps_all_fields_and_sentinels() {
        let entry = make_entry("req-1");
        let row = ChProxyLogRow::from(&entry);

        assert_eq!(row.method, "GET");
        assert_eq!(row.path, "/hello");
        assert_eq!(row.query_string, "a=1");
        assert_eq!(row.host, "example.com");
        assert_eq!(row.status_code, 200);
        assert_eq!(row.response_time_ms, Some(12));
        assert_eq!(row.is_system_request, 0);
        assert_eq!(row.project_id, Some(7));
        assert_eq!(row.environment_id, Some(3));
        // None Option<String> → '' sentinel.
        assert_eq!(row.error_message, "");
        assert_eq!(row.referrer, "");
        assert_eq!(row.bot_name, "");
        // present Option<String> kept.
        assert_eq!(row.upstream_host, "up.internal");
        assert_eq!(row.client_ip, "127.0.0.1");
        assert_eq!(row.browser, "Firefox");
        // is_bot Some(false) → Some(0); tri-state preserved.
        assert_eq!(row.is_bot, Some(0));
        assert_eq!(row.request_size_bytes, Some(100));
        // header JSON serialized; None → "{}" sentinel.
        assert_eq!(row.request_headers, "{\"x\":\"y\"}");
        assert_eq!(row.response_headers, "{}");
        assert_eq!(row.trace_id, "abc123");
        assert_eq!(row.error_group_id, None);
        // _version is the ingest ms and matches timestamp.
        assert_eq!(row._version, row.timestamp as u64);
        assert!(row._version > 0);
    }

    #[test]
    fn row_conversion_is_bot_tristate() {
        let mut entry = make_entry("req-2");
        entry.is_bot = None;
        let row = ChProxyLogRow::from(&entry);
        assert_eq!(row.is_bot, None);

        entry.is_bot = Some(true);
        let row = ChProxyLogRow::from(&entry);
        assert_eq!(row.is_bot, Some(1));
    }

    #[test]
    fn read_row_into_model_maps_sentinels_back() {
        let read = ChProxyLogReadRow {
            timestamp_ms: 1_717_200_000_000,
            method: "POST".into(),
            path: "/x".into(),
            query_string: String::new(),
            host: "h".into(),
            status_code: 404,
            response_time_ms: None,
            request_source: "proxy".into(),
            is_system_request: 1,
            routing_status: "routed".into(),
            project_id: Some(5),
            environment_id: None,
            deployment_id: None,
            session_id: None,
            visitor_id: None,
            container_id: String::new(),
            upstream_host: String::new(),
            error_message: "boom".into(),
            client_ip: "1.2.3.4".into(),
            user_agent: String::new(),
            referrer: String::new(),
            request_id: "rid".into(),
            ip_geolocation_id: None,
            browser: String::new(),
            browser_version: String::new(),
            operating_system: String::new(),
            device_type: String::new(),
            is_bot: Some(1),
            bot_name: "GPTBot".into(),
            request_size_bytes: None,
            response_size_bytes: Some(10),
            cache_status: String::new(),
        };
        let model = read.into_model();
        assert_eq!(model.id, 0);
        assert_eq!(model.method, "POST");
        assert_eq!(model.status_code, 404);
        assert!(model.is_system_request);
        assert_eq!(model.project_id, Some(5));
        // '' sentinels become None.
        assert_eq!(model.query_string, None);
        assert_eq!(model.container_id, None);
        assert_eq!(model.user_agent, None);
        // populated strings survive.
        assert_eq!(model.error_message, Some("boom".to_string()));
        assert_eq!(model.client_ip, Some("1.2.3.4".to_string()));
        assert_eq!(model.bot_name, Some("GPTBot".to_string()));
        // is_bot tri-state restored.
        assert_eq!(model.is_bot, Some(true));
        // timestamp round-trips.
        assert_eq!(model.timestamp.timestamp_millis(), 1_717_200_000_000);
    }

    #[test]
    fn sort_column_allowlist_matches_timescaledb() {
        assert_eq!(sort_column(None), "timestamp");
        assert_eq!(sort_column(Some("timestamp")), "timestamp");
        assert_eq!(sort_column(Some("response_time")), "response_time_ms");
        assert_eq!(sort_column(Some("response_time_ms")), "response_time_ms");
        assert_eq!(sort_column(Some("status_code")), "status_code");
        assert_eq!(sort_column(Some("request_size")), "request_size_bytes");
        assert_eq!(sort_column(Some("response_size")), "response_size_bytes");
        assert_eq!(sort_column(Some("client_ip")), "client_ip");
        assert_eq!(sort_column(Some("is_bot")), "is_bot");
        // Any unknown / injection attempt falls back to timestamp.
        assert_eq!(
            sort_column(Some("'; DROP TABLE proxy_logs; --")),
            "timestamp"
        );
        assert_eq!(sort_column(Some("nonexistent")), "timestamp");
    }

    #[test]
    fn status_class_range_matches_postgres() {
        assert_eq!(status_class_range("2xx"), Some((200, 300)));
        assert_eq!(status_class_range("4xx"), Some((400, 500)));
        assert_eq!(status_class_range("5xx"), Some((500, 600)));
        assert_eq!(status_class_range("xyz"), None);
    }

    /// The list WHERE builder must bind every value and never interpolate user
    /// strings. We assert the clause uses `?` placeholders and the escaped LIKE
    /// pattern is in the bind list (not the SQL).
    #[test]
    fn build_list_where_binds_all_values() {
        let filters = ProxyLogsQuery {
            project_id: Some(7),
            environment_id: None,
            deployment_id: None,
            session_id: None,
            visitor_id: None,
            start_date: None,
            end_date: None,
            method: Some("GET".into()),
            host: None,
            path: Some("admin%".into()),
            client_ip: Some("1.1.1.1".into()),
            status_code: Some(200),
            response_time_min: Some(5),
            response_time_max: None,
            routing_status: None,
            request_source: None,
            is_system_request: Some(false),
            user_agent: None,
            browser: None,
            operating_system: None,
            device_type: None,
            is_bot: Some(true),
            bot_name: None,
            ai_provider: None,
            ai_agent: None,
            is_ai_agent: None,
            request_size_min: None,
            request_size_max: None,
            response_size_min: None,
            response_size_max: None,
            cache_status: None,
            container_id: None,
            upstream_host: None,
            has_error: Some(true),
            page: None,
            page_size: None,
            sort_by: None,
            sort_order: None,
        };

        let (clauses, binds, impossible) =
            ClickHouseProxyLogStore::build_list_where(None, None, &filters);
        assert!(!impossible);
        let joined = clauses.join(" AND ");
        // Every value-carrying predicate uses a bound `?`.
        assert!(joined.contains("project_id = ?"));
        assert!(joined.contains("method = ?"));
        assert!(joined.contains("path ILIKE ?"));
        assert!(joined.contains("client_ip = ?"));
        assert!(joined.contains("status_code = ?"));
        assert!(joined.contains("response_time_ms >= ?"));
        assert!(joined.contains("is_system_request = ?"));
        assert!(joined.contains("is_bot = ?"));
        // has_error=true → error_message != '' (no value bound).
        assert!(joined.contains("error_message != ''"));
        // The raw user path value must NOT appear in the SQL — only in the binds,
        // wrapped + escaped.
        assert!(!joined.contains("admin"));
        // The bind list carries the escaped LIKE pattern.
        let has_escaped_path = binds.iter().any(|b| match b {
            Bv::Str(s) => s == "%admin\\%%",
            _ => false,
        });
        assert!(
            has_escaped_path,
            "escaped LIKE pattern must be a bind value"
        );
    }

    /// Unknown ai_provider must mark the query impossible (Postgres Id.eq(-1)).
    #[test]
    fn build_list_where_unknown_provider_is_impossible() {
        let mut filters = empty_query();
        filters.ai_provider = Some("DefinitelyNotARealProvider".into());
        let (_, _, impossible) = ClickHouseProxyLogStore::build_list_where(None, None, &filters);
        assert!(impossible);
    }

    /// is_ai_agent=false → bot_name NOT IN (known) with the known list bound.
    #[test]
    fn build_list_where_is_ai_agent_false_uses_not_in() {
        let mut filters = empty_query();
        filters.is_ai_agent = Some(false);
        let (clauses, binds, impossible) =
            ClickHouseProxyLogStore::build_list_where(None, None, &filters);
        assert!(!impossible);
        assert!(clauses.join(" AND ").contains("bot_name NOT IN ?"));
        assert!(binds.iter().any(|b| matches!(b, Bv::StrVec(_))));
    }

    fn empty_query() -> ProxyLogsQuery {
        ProxyLogsQuery {
            project_id: None,
            environment_id: None,
            deployment_id: None,
            session_id: None,
            visitor_id: None,
            start_date: None,
            end_date: None,
            method: None,
            host: None,
            path: None,
            client_ip: None,
            status_code: None,
            response_time_min: None,
            response_time_max: None,
            routing_status: None,
            request_source: None,
            is_system_request: None,
            user_agent: None,
            browser: None,
            operating_system: None,
            device_type: None,
            is_bot: None,
            bot_name: None,
            ai_provider: None,
            ai_agent: None,
            is_ai_agent: None,
            request_size_min: None,
            request_size_max: None,
            response_size_min: None,
            response_size_max: None,
            cache_status: None,
            container_id: None,
            upstream_host: None,
            has_error: None,
            page: None,
            page_size: None,
            sort_by: None,
            sort_order: None,
        }
    }

    #[tokio::test]
    async fn write_batch_empty_is_noop() {
        let store = ClickHouseProxyLogStore::new(
            ClickHouseProxyLogConfig::new("http://127.0.0.1:1", "otel", "temps", "temps_dev"),
            Arc::new(temps_core::FixedRetentionResolver),
        );
        assert!(store.write_batch(vec![]).await.is_ok());
    }

    #[tokio::test]
    async fn get_by_id_returns_none_under_clickhouse() {
        // No serial id in CH — get_by_id always resolves to None (404) without
        // any I/O, so an unreachable URL still succeeds.
        let store = ClickHouseProxyLogStore::new(
            ClickHouseProxyLogConfig::new("http://127.0.0.1:1", "otel", "temps", "temps_dev"),
            Arc::new(temps_core::FixedRetentionResolver),
        );
        assert!(store
            .get_by_id(42, None)
            .await
            .expect("no-io path")
            .is_none());
    }

    #[tokio::test]
    async fn list_unknown_provider_short_circuits_without_io() {
        let store = ClickHouseProxyLogStore::new(
            ClickHouseProxyLogConfig::new("http://127.0.0.1:1", "otel", "temps", "temps_dev"),
            Arc::new(temps_core::FixedRetentionResolver),
        );
        let mut filters = empty_query();
        filters.ai_provider = Some("NopeNotReal".into());
        let (rows, total) = store
            .list_with_filters(None, None, filters, 1, 20)
            .await
            .expect("impossible query => empty, no round-trip");
        assert!(rows.is_empty());
        assert_eq!(total, 0);
    }
}
