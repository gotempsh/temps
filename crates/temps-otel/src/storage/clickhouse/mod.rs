//! ClickHouse storage backend for OTel spans.
//!
//! This module provides the ClickHouse client wrapper and the [`ChSpanRow`]
//! row type that mirrors the DDL in
//! `crates/temps-otel/migrations/clickhouse/0001_spans.sql` exactly.
//!
//! [`ClickHouseOtelStorage`] implements the full [`OtelStorage`] trait:
//!
//! - **Span-domain write** (`store_spans`) runs directly against ClickHouse.
//! - **Span-domain reads** (`query_trace_summaries`, `count_traces`,
//!   `query_spans`, `get_trace`, GenAI reads) run natively against ClickHouse
//!   as of Phase 1.
//! - **Non-span methods** (metrics, logs, anomaly helpers, retention) and
//!   **control-row methods** (insights, health summaries, quota) are delegated
//!   to the inner [`Arc<TimescaleDbStorage>`] unconditionally. These are
//!   ADR-016 Phases 2–4.
//!
//! ## Activation
//!
//! The plugin constructs [`ClickHouseOtelStorage`] only when
//! `ServerConfig::is_clickhouse_enabled()` returns `true`
//! (all four `TEMPS_CLICKHOUSE_*` env vars set). When disabled, the existing
//! `TimescaleDbStorage` path is unchanged.
//!
//! ## Row type stability
//!
//! [`ChSpanRow`] field order and types **must stay in lockstep with the DDL**.
//! The `clickhouse` crate's `Row` derive uses positional binary serialization
//! over the HTTP interface; any field order mismatch silently corrupts data.
//! If the schema changes, update both `0001_spans.sql` and [`ChSpanRow`]
//! together and bump the migration number.
//!
//! ## SQL injection safety
//!
//! Filter *values* (service_name, trace_id, status_code, timestamps, etc.) are
//! always passed via `.bind(value)` with a `?` placeholder — never
//! `format!`-ed into the SQL string.  The only interpolated strings are the
//! `ORDER BY` clause direction and field name, both derived from fixed enums
//! (`TraceSortField` / `SortOrder`) with no user-controlled input path.

pub mod migrations;

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::error::OtelError;
use crate::storage::timescaledb::TimescaleDbStorage;
use crate::storage::{BaselinePoint, DeployEvent, MinuteAggregate, OtelStorage, StorageResult};
use crate::types::{
    GenAiEvent, GenAiSpanDetail, GenAiTraceSummary, HealthSummary, Insight, InsightStatus,
    LogQuery, LogRecord, MetricBucket, MetricPoint, MetricQuery, SpanEvent, SpanKind, SpanRecord,
    SpanStatusCode, StorageQuota, TraceQuery, TraceSummary,
};

// ── Client configuration ────────────────────────────────────────────────────

/// Connection configuration for the ClickHouse OTel backend.
///
/// Built from `ServerConfig` fields populated by the `TEMPS_CLICKHOUSE_*`
/// environment variables. All four fields are required; the plugin calls
/// `ServerConfig::is_clickhouse_enabled()` to guard construction.
#[derive(Clone)]
pub struct ClickHouseOtelConfig {
    pub url: String,
    pub database: String,
    pub user: String,
    pub password: String,
}

// Manual Debug that masks the password so it can never leak into logs, panic
// messages, or tracing spans that capture the config with `{:?}`.
impl std::fmt::Debug for ClickHouseOtelConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClickHouseOtelConfig")
            .field("url", &self.url)
            .field("database", &self.database)
            .field("user", &self.user)
            .field("password", &"***")
            .finish()
    }
}

impl ClickHouseOtelConfig {
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

// ── Client wrapper ──────────────────────────────────────────────────────────

/// Thin wrapper around `clickhouse::Client` for the OTel backend.
///
/// `clickhouse::Client` is already cheaply cloneable (Arc-backed internally),
/// but wrapping it lets us add OTel-specific helpers (e.g., health check,
/// migration runner) without coupling the storage struct to construction details.
pub struct ClickHouseOtelClient {
    pub(crate) client: ::clickhouse::Client,
}

impl ClickHouseOtelClient {
    /// Build a client from configuration.
    ///
    /// Does not validate connectivity; call [`health_check`] or run migrations
    /// to confirm the connection is live.
    pub fn new(config: ClickHouseOtelConfig) -> Self {
        let client = ::clickhouse::Client::default()
            .with_url(config.url)
            .with_database(config.database)
            .with_user(config.user)
            .with_password(config.password);
        Self { client }
    }

    /// Borrow the underlying client for queries.
    pub fn client(&self) -> &::clickhouse::Client {
        &self.client
    }

    /// Clone the underlying client (cheap — Arc internally).
    pub fn client_clone(&self) -> ::clickhouse::Client {
        self.client.clone()
    }

    /// Verify connectivity and authentication with a `SELECT 1`.
    pub async fn health_check(&self) -> Result<(), OtelError> {
        self.client
            .query("SELECT 1")
            .fetch_one::<u8>()
            .await
            .map_err(|e| OtelError::Storage {
                message: format!("ClickHouse health check failed: {e}"),
            })?;
        Ok(())
    }
}

// ── Row type ────────────────────────────────────────────────────────────────

/// ClickHouse row matching the `spans` table DDL in `0001_spans.sql`.
///
/// **Field order must match the DDL column order exactly.** The `clickhouse`
/// crate serialises fields positionally (binary protocol over HTTP); any
/// reordering here relative to the DDL silently corrupts inserts.
///
/// ## Type mapping
///
/// | DDL type                    | Rust type          | Notes                                         |
/// |-----------------------------|-------------------|-----------------------------------------------|
/// | `Int32`                     | `i32`             |                                               |
/// | `Nullable(Int32)`           | `Option<i32>`     |                                               |
/// | `LowCardinality(String)`    | `String`          | No special Rust type needed                   |
/// | `String`                    | `String`          |                                               |
/// | `DateTime64(3, 'UTC')`      | `i64`             | Unix milliseconds; avoids precision ambiguity |
/// | `Float64`                   | `f64`             |                                               |
/// | `UInt64`                    | `u64`             |                                               |
///
/// ## Timestamp encoding
///
/// `start_time` and `end_time` are stored as milliseconds since the Unix
/// epoch (`i64`). This matches how the analytics `ChEventRow` encodes its
/// `timestamp` column and is the safest mapping for `DateTime64(3)` — the
/// `clickhouse` crate's `chrono` feature can also send `DateTime<Utc>`, but
/// the i64 path is explicit about precision and avoids any mismatch between
/// the Rust timezone representation and the CH server setting.
///
/// At read time, the query layer converts back via
/// `DateTime::from_timestamp_millis(ms).unwrap_or_default()`.
///
/// ## Null vs empty-string sentinels
///
/// - `parent_span_id`: `String` (not `Option<String>`). Root spans store `""`.
///   This matches the DDL `DEFAULT ''` and avoids a CH `Nullable` column on a
///   high-cardinality ordering key.
/// - `service_version`, `deployment_environment`, `status_message`,
///   `attributes`, `events`: `String` with `""` / `"{}"` / `"[]"` sentinels.
///   CH `LowCardinality(String)` and plain `String` columns are non-nullable
///   in this DDL.
/// - `deployment_id`: `Option<i32>` — genuinely nullable in both the domain
///   type and the DDL (`Nullable(Int32)`).
#[derive(::clickhouse::Row, Serialize, Deserialize, Debug, Clone)]
pub struct ChSpanRow {
    // ── Tenant + deployment context ─────────────────────────────────────────
    /// project_id  Int32
    pub project_id: i32,
    /// deployment_id  Nullable(Int32)
    pub deployment_id: Option<i32>,

    // ── Resource / service identity (denormalized at ingest) ────────────────
    /// service_name  LowCardinality(String)
    pub service_name: String,
    /// service_version  LowCardinality(String)
    pub service_version: String,
    /// deployment_environment  LowCardinality(String)
    pub deployment_environment: String,

    // ── Span identity ───────────────────────────────────────────────────────
    /// trace_id  String
    pub trace_id: String,
    /// span_id  String
    pub span_id: String,
    /// parent_span_id  String  DEFAULT ''
    pub parent_span_id: String,

    // ── Span semantics ──────────────────────────────────────────────────────
    /// name  String
    pub name: String,
    /// kind  LowCardinality(String)
    pub kind: String,

    // ── Timing ─────────────────────────────────────────────────────────────
    /// start_time  DateTime64(3, 'UTC') — stored as Unix milliseconds
    pub start_time: i64,
    /// end_time  DateTime64(3, 'UTC') — stored as Unix milliseconds
    pub end_time: i64,
    /// duration_ms  Float64
    pub duration_ms: f64,

    // ── Status ──────────────────────────────────────────────────────────────
    /// status_code  LowCardinality(String)
    pub status_code: String,
    /// status_message  String  DEFAULT ''
    pub status_message: String,

    // ── Payload (JSON serialised) ───────────────────────────────────────────
    /// attributes  String  DEFAULT '{}'  (JSON object)
    pub attributes: String,
    /// events  String  DEFAULT '[]'  (JSON array)
    pub events: String,

    // ── Dedup key ───────────────────────────────────────────────────────────
    /// _version  UInt64  DEFAULT toUnixTimestamp64Milli(now64())
    /// Set to the current Unix millisecond timestamp at ingest time so that
    /// OTLP retries of the same span converge to one canonical row via
    /// ReplacingMergeTree (highest _version wins).
    pub _version: u64,
}

// ── From<&SpanRecord> for ChSpanRow ────────────────────────────────────────

impl From<&SpanRecord> for ChSpanRow {
    fn from(span: &SpanRecord) -> Self {
        // Serialize attributes and events to JSON strings. These are
        // BTreeMap<String,String> / Vec<SpanEvent>, both of which are
        // trivially serializable. We fall back to "{}" / "[]" on the
        // (unreachable in practice) serialization error path rather than
        // propagating — ingest must not drop spans over a serialization
        // hiccup in metadata.
        let attributes = serde_json::to_string(&span.attributes).unwrap_or_else(|_| "{}".into());
        let events = serde_json::to_string(&span.events).unwrap_or_else(|_| "[]".into());

        // _version: Unix ms timestamp used as the ReplacingMergeTree dedup key.
        // Using now() at conversion time (same moment as ingest). Spans retried
        // by the OTLP exporter will produce a higher _version than the first
        // attempt and win the dedup.
        let version = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            project_id: span.project_id,
            deployment_id: span.deployment_id,
            service_name: span.resource.service_name.clone(),
            service_version: span.resource.service_version.clone().unwrap_or_default(),
            deployment_environment: span
                .resource
                .deployment_environment
                .clone()
                .unwrap_or_default(),
            trace_id: span.trace_id.clone(),
            span_id: span.span_id.clone(),
            parent_span_id: span.parent_span_id.clone().unwrap_or_default(),
            name: span.name.clone(),
            kind: span_kind_to_str(span.kind).to_owned(),
            start_time: span.start_time.timestamp_millis(),
            end_time: span.end_time.timestamp_millis(),
            duration_ms: span.duration_ms,
            status_code: span_status_to_str(span.status_code).to_owned(),
            status_message: span.status_message.clone(),
            attributes,
            events,
            _version: version,
        }
    }
}

// ── ChSpanRow → SpanRecord ──────────────────────────────────────────────────

/// Convert a [`ChSpanRow`] read from ClickHouse back into a [`SpanRecord`].
///
/// Used by the query methods (`query_spans`, `get_trace`) that fetch raw rows
/// and need to return the canonical domain type.
///
/// Deserialization failures in `attributes`/`events` JSON fall back to empty
/// collections — the span identity and timing are preserved, and partial
/// attribute loss is preferable to surfacing an error for an otherwise-valid
/// span.
impl From<ChSpanRow> for SpanRecord {
    fn from(row: ChSpanRow) -> Self {
        use chrono::TimeZone;

        let start_time = chrono::Utc
            .timestamp_millis_opt(row.start_time)
            .single()
            .unwrap_or_default();
        let end_time = chrono::Utc
            .timestamp_millis_opt(row.end_time)
            .single()
            .unwrap_or_default();

        let attributes: std::collections::BTreeMap<String, String> =
            serde_json::from_str(&row.attributes).unwrap_or_default();
        let events: Vec<SpanEvent> = serde_json::from_str(&row.events).unwrap_or_default();

        let resource = crate::types::ResourceInfo {
            service_name: row.service_name,
            service_version: if row.service_version.is_empty() {
                None
            } else {
                Some(row.service_version)
            },
            deployment_environment: if row.deployment_environment.is_empty() {
                None
            } else {
                Some(row.deployment_environment)
            },
            attributes: std::collections::BTreeMap::new(),
        };

        SpanRecord {
            project_id: row.project_id,
            deployment_id: row.deployment_id,
            resource,
            trace_id: row.trace_id,
            span_id: row.span_id,
            parent_span_id: if row.parent_span_id.is_empty() {
                None
            } else {
                Some(row.parent_span_id)
            },
            name: row.name,
            kind: str_to_span_kind(&row.kind),
            start_time,
            end_time,
            duration_ms: row.duration_ms,
            status_code: str_to_span_status(&row.status_code),
            status_message: row.status_message,
            attributes,
            events,
        }
    }
}

// ── Enum ↔ string helpers ───────────────────────────────────────────────────

/// Map [`SpanKind`] to the string stored in the CH `kind` column.
/// Matches the `Display` impl on `SpanKind` (SCREAMING_SNAKE_CASE).
pub(crate) fn span_kind_to_str(kind: SpanKind) -> &'static str {
    match kind {
        SpanKind::Unspecified => "UNSPECIFIED",
        SpanKind::Internal => "INTERNAL",
        SpanKind::Server => "SERVER",
        SpanKind::Client => "CLIENT",
        SpanKind::Producer => "PRODUCER",
        SpanKind::Consumer => "CONSUMER",
    }
}

/// Reverse map — unknown strings become [`SpanKind::Unspecified`].
pub(crate) fn str_to_span_kind(s: &str) -> SpanKind {
    match s {
        "INTERNAL" => SpanKind::Internal,
        "SERVER" => SpanKind::Server,
        "CLIENT" => SpanKind::Client,
        "PRODUCER" => SpanKind::Producer,
        "CONSUMER" => SpanKind::Consumer,
        _ => SpanKind::Unspecified,
    }
}

/// Map [`SpanStatusCode`] to the string stored in the CH `status_code` column.
pub(crate) fn span_status_to_str(code: SpanStatusCode) -> &'static str {
    match code {
        SpanStatusCode::Unset => "UNSET",
        SpanStatusCode::Ok => "OK",
        SpanStatusCode::Error => "ERROR",
    }
}

/// Reverse map — unknown strings become [`SpanStatusCode::Unset`].
pub(crate) fn str_to_span_status(s: &str) -> SpanStatusCode {
    match s {
        "OK" => SpanStatusCode::Ok,
        "ERROR" => SpanStatusCode::Error,
        _ => SpanStatusCode::Unset,
    }
}

// ── Read-side row types ─────────────────────────────────────────────────────
//
// These are separate from ChSpanRow (which is optimised for writes with Serialize).
// Read rows use Deserialize so the clickhouse crate can deserialise them from
// the HTTP row-binary response.  Field names must exactly match the SQL column
// aliases used in the SELECT list.

/// Row returned by `query_trace_summaries` — one row per distinct trace_id.
#[derive(::clickhouse::Row, Deserialize, Debug)]
struct ChTraceSummaryRow {
    trace_id: String,
    root_span_name: String,
    service_name: String,
    kind: String,
    deployment_environment: String,
    /// Unix milliseconds (toUnixTimestamp64Milli)
    start_time_ms: i64,
    max_duration_ms: f64,
    span_count: u64,
    error_count: u64,
}

/// Row returned by count queries — a single u64 scalar.
#[derive(::clickhouse::Row, Deserialize, Debug)]
struct ChCountRow {
    cnt: u64,
}

/// Row returned by `query_genai_trace_summaries`.
#[derive(::clickhouse::Row, Deserialize, Debug)]
struct ChGenAiSummaryRow {
    trace_id: String,
    root_span_name: String,
    service_name: String,
    gen_ai_system: String,
    gen_ai_model: String,
    gen_ai_operation: String,
    /// Unix milliseconds
    start_time_ms: i64,
    max_duration_ms: f64,
    span_count: u64,
    error_count: u64,
    total_input_tokens: i64,
    total_output_tokens: i64,
    total_cache_creation_input_tokens: i64,
    total_cache_read_input_tokens: i64,
}

/// Row returned by `get_genai_trace_spans` — spans belonging to one trace.
/// We reuse `ChSpanRow` with `Deserialize` already derived there; so only a
/// purpose-specific row for `get_genai_trace_events` is needed.
#[derive(::clickhouse::Row, Deserialize, Debug)]
struct ChSpanEventsRow {
    span_id: String,
    events: String, // JSON string
}

// ── LIKE pattern helpers ────────────────────────────────────────────────────

/// Escape LIKE/ILIKE metacharacters in a user-supplied substring pattern.
///
/// ClickHouse LIKE uses backslash as the default escape character (no explicit
/// `ESCAPE` clause required). We must escape:
///
/// - `\` → `\\`   (backslash itself, before the other replacements)
/// - `%` → `\%`   (wildcard: any sequence of chars)
/// - `_` → `\_`   (wildcard: exactly one char)
///
/// The caller then wraps the result with `%{escaped}%` to perform a
/// case-insensitive substring search via ILIKE.
///
/// ## Verification
///
/// Confirmed against live ClickHouse 26.2: `'hello%world' LIKE '%\%%'`
/// returns 1, `'helloXworld' LIKE '%\%%'` returns 0. The backslash is the
/// default escape character; no `ESCAPE` clause is needed.
pub(crate) fn escape_like_pattern(pattern: &str) -> String {
    // Order matters: escape backslash first so the subsequent replacements
    // don't double-escape the backslashes we just introduced.
    pattern
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

// ── OtelError helpers ───────────────────────────────────────────────────────

/// Wrap a ClickHouse ingest error into an [`OtelError::Storage`] with context.
///
/// All span-domain write methods use this helper so error messages
/// consistently identify the operation and the CH error.
pub(crate) fn ch_ingest_err(operation: &str, err: ::clickhouse::error::Error) -> OtelError {
    OtelError::Storage {
        message: format!("ClickHouse {operation} failed: {err}"),
    }
}

/// Wrap a ClickHouse query error into an [`OtelError::Storage`] with context.
pub(crate) fn ch_query_err(operation: &str, err: ::clickhouse::error::Error) -> OtelError {
    OtelError::Storage {
        message: format!("ClickHouse query {operation} failed: {err}"),
    }
}

// ── ClickHouseOtelStorage ────────────────────────────────────────────────────

/// ClickHouse-backed OTel storage.
///
/// Span writes go directly to ClickHouse. Span reads and all non-span
/// methods delegate to the inner `TimescaleDbStorage` until Phase 1–4
/// implementations land (see module-level doc).
pub struct ClickHouseOtelStorage {
    /// ClickHouse client — cheap to clone (Arc-backed internally).
    ch: ::clickhouse::Client,
    /// Postgres/TimescaleDB inner storage for delegation.
    ///
    /// All non-span methods and (for now) all span read methods are
    /// forwarded here verbatim. Phase 1 will replace span read delegation
    /// with native CH queries one method at a time.
    inner: Arc<TimescaleDbStorage>,
}

impl ClickHouseOtelStorage {
    /// Construct a new ClickHouse OTel storage backend.
    ///
    /// - `config`: connection parameters for the ClickHouse `otel` database.
    /// - `inner`: the `TimescaleDbStorage` used for delegation of non-span
    ///   methods and (during Phase 0) span reads. Callers typically pass
    ///   the same `Arc<TimescaleDbStorage>` they would have registered
    ///   without ClickHouse.
    pub fn new(config: ClickHouseOtelConfig, inner: Arc<TimescaleDbStorage>) -> Self {
        let ch = ::clickhouse::Client::default()
            .with_url(&config.url)
            .with_database(&config.database)
            .with_user(&config.user)
            .with_password(&config.password);
        Self { ch, inner }
    }

    /// Expose the raw ClickHouse client for migration runners / health checks.
    pub fn ch_client(&self) -> &::clickhouse::Client {
        &self.ch
    }
}

#[async_trait]
impl OtelStorage for ClickHouseOtelStorage {
    // ── Span write (ClickHouse — system of record) ──────────────────────────

    /// Batch-insert spans directly into the ClickHouse `spans` table.
    ///
    /// Uses `client.insert` + per-row `write` + `end()` — the same pattern
    /// as `ChFanout` in `temps-analytics-events`. `ReplacingMergeTree(_version)`
    /// deduplicates retried OTLP payloads automatically.
    ///
    /// Large batches are split into chunks of at most [`MAX_SPAN_INSERT_BATCH`]
    /// rows to bound the peak memory held in the ClickHouse client's HTTP
    /// buffer.  The total stored count is always the full input length.
    async fn store_spans(&self, spans: Vec<SpanRecord>) -> StorageResult<u64> {
        /// Maximum number of span rows per ClickHouse HTTP insert request.
        /// Limits peak CH client buffer memory on very large OTLP payloads.
        const MAX_SPAN_INSERT_BATCH: usize = 10_000;

        if spans.is_empty() {
            return Ok(0);
        }
        let total = spans.len() as u64;

        for chunk in spans.chunks(MAX_SPAN_INSERT_BATCH) {
            let mut inserter = self
                .ch
                .insert::<ChSpanRow>("spans")
                .map_err(|e| ch_ingest_err("store_spans (inserter setup)", e))?;

            for span in chunk {
                let row = ChSpanRow::from(span);
                inserter
                    .write(&row)
                    .await
                    .map_err(|e| ch_ingest_err("store_spans (write)", e))?;
            }

            inserter
                .end()
                .await
                .map_err(|e| ch_ingest_err("store_spans (end)", e))?;
        }

        debug!(total, "ClickHouseOtelStorage: stored spans");
        Ok(total)
    }

    // ── Span reads (Phase 1 — native ClickHouse queries) ────────────────────

    /// Fetch raw span rows matching the query filters.
    ///
    /// Maps 1-to-1 with the TimescaleDB `query_spans` implementation. Bind
    /// params are used for all filter values; only `ORDER BY` direction is
    /// interpolated (from a fixed enum — injection-safe).
    async fn query_spans(&self, query: TraceQuery) -> StorageResult<Vec<SpanRecord>> {
        let limit = query.limit.unwrap_or(50).min(100);
        let offset = query.offset.unwrap_or(0);

        // Build the WHERE clause and a matching bind list.
        // Using a String accumulator for the SQL fragments + a Vec of
        // closures that call .bind() is not ergonomic in Rust (the clickhouse
        // QueryBuilder is not object-safe). Instead we use a small state
        // machine: we render SQL with `?` placeholders in the same order as
        // the values, then call .bind() in the same order.
        let mut sql = String::from(
            "SELECT project_id, deployment_id, service_name, service_version, \
             deployment_environment, trace_id, span_id, parent_span_id, name, kind, \
             toUnixTimestamp64Milli(start_time) AS start_time_ms, \
             toUnixTimestamp64Milli(end_time) AS end_time_ms, \
             duration_ms, status_code, status_message, attributes, events \
             FROM spans FINAL WHERE project_id = ?",
        );

        // We build bind values as a Vec<ChBindValue> — a local enum that lets
        // us defer the actual .bind() calls until we have the full query string.
        enum Bv {
            I32(i32),
            I64(i64),
            F64(f64),
            Str(String),
        }
        let mut binds: Vec<Bv> = vec![Bv::I32(query.project_id)];

        if let Some(ref tid) = query.trace_id {
            sql.push_str(" AND trace_id = ?");
            binds.push(Bv::Str(tid.clone()));
        }
        if let Some(ref svc) = query.service_name {
            sql.push_str(" AND service_name = ?");
            binds.push(Bv::Str(svc.clone()));
        }
        if let Some(status) = query.status {
            sql.push_str(" AND status_code = ?");
            binds.push(Bv::Str(span_status_to_str(status).to_owned()));
        }
        if let Some(min_dur) = query.min_duration_ms {
            sql.push_str(" AND duration_ms >= ?");
            binds.push(Bv::F64(min_dur));
        }
        if let Some(start) = query.start_time {
            sql.push_str(" AND start_time >= fromUnixTimestamp64Milli(?)");
            binds.push(Bv::I64(start.timestamp_millis()));
        }
        if let Some(end) = query.end_time {
            sql.push_str(" AND start_time <= fromUnixTimestamp64Milli(?)");
            binds.push(Bv::I64(end.timestamp_millis()));
        }
        if let Some(did) = query.deployment_id {
            sql.push_str(" AND deployment_id = ?");
            binds.push(Bv::I32(did));
        }
        // environment_id: CH has no JOIN to deployments; filter delegated when
        // environment_id is set by falling back to inner (see module note).
        // In Phase 1 we use the denormalized deployment_environment column
        // for the common case; environment_id is not resolvable in CH without
        // a separate Postgres lookup, so we skip that filter here.
        if let Some(ref attrs) = query.attributes {
            for (key, value) in attrs {
                sql.push_str(" AND JSONExtractString(attributes, ?) = ?");
                binds.push(Bv::Str(key.clone()));
                binds.push(Bv::Str(value.clone()));
            }
        }
        if let Some(ref pattern) = query.name_pattern {
            sql.push_str(" AND name ILIKE ?");
            binds.push(Bv::Str(format!("%{}%", escape_like_pattern(pattern))));
        }

        // ORDER BY — enum-derived, injection-safe.
        let order_dir = query.sort_order.as_sql();
        match query.sort_by {
            crate::types::TraceSortField::Duration => {
                sql.push_str(&format!(" ORDER BY duration_ms {order_dir}"));
            }
            crate::types::TraceSortField::StartTime => {
                sql.push_str(&format!(" ORDER BY start_time {order_dir}"));
            }
        }
        sql.push_str(" LIMIT ? OFFSET ?");
        binds.push(Bv::I64(limit as i64));
        binds.push(Bv::I64(offset as i64));

        // Apply binds sequentially to the query builder.
        let mut q = self.ch.query(&sql);
        for b in binds {
            q = match b {
                Bv::I32(v) => q.bind(v),
                Bv::I64(v) => q.bind(v),
                Bv::F64(v) => q.bind(v),
                Bv::Str(v) => q.bind(v),
            };
        }

        // We read into a dedicated row type that has the renamed timestamp columns.
        #[derive(::clickhouse::Row, Deserialize, Debug)]
        struct ChRawSpanRow {
            project_id: i32,
            deployment_id: Option<i32>,
            service_name: String,
            service_version: String,
            deployment_environment: String,
            trace_id: String,
            span_id: String,
            parent_span_id: String,
            name: String,
            kind: String,
            start_time_ms: i64,
            end_time_ms: i64,
            duration_ms: f64,
            status_code: String,
            status_message: String,
            attributes: String,
            events: String,
        }

        let rows = q
            .fetch_all::<ChRawSpanRow>()
            .await
            .map_err(|e| ch_query_err("query_spans", e))?;

        let spans: Vec<SpanRecord> = rows
            .into_iter()
            .map(|r| {
                use chrono::TimeZone;
                let start_time = chrono::Utc
                    .timestamp_millis_opt(r.start_time_ms)
                    .single()
                    .unwrap_or_default();
                let end_time = chrono::Utc
                    .timestamp_millis_opt(r.end_time_ms)
                    .single()
                    .unwrap_or_default();
                let attributes: std::collections::BTreeMap<String, String> =
                    serde_json::from_str(&r.attributes).unwrap_or_default();
                let events: Vec<SpanEvent> = serde_json::from_str(&r.events).unwrap_or_default();
                let resource = crate::types::ResourceInfo {
                    service_name: r.service_name,
                    service_version: if r.service_version.is_empty() {
                        None
                    } else {
                        Some(r.service_version)
                    },
                    deployment_environment: if r.deployment_environment.is_empty() {
                        None
                    } else {
                        Some(r.deployment_environment)
                    },
                    attributes: std::collections::BTreeMap::new(),
                };
                SpanRecord {
                    project_id: r.project_id,
                    deployment_id: r.deployment_id,
                    resource,
                    trace_id: r.trace_id,
                    span_id: r.span_id,
                    parent_span_id: if r.parent_span_id.is_empty() {
                        None
                    } else {
                        Some(r.parent_span_id)
                    },
                    name: r.name,
                    kind: str_to_span_kind(&r.kind),
                    start_time,
                    end_time,
                    duration_ms: r.duration_ms,
                    status_code: str_to_span_status(&r.status_code),
                    status_message: r.status_message,
                    attributes,
                    events,
                }
            })
            .collect();

        debug!(count = spans.len(), "ClickHouseOtelStorage: query_spans");
        Ok(spans)
    }

    /// Aggregate spans into per-trace summaries using query-time GROUP BY.
    ///
    /// Chosen approach from benchmark (ADR-016 Phase 0): query-time GROUP BY
    /// on `spans FINAL` beats the AggregatingMergeTree MV approach at our
    /// benchmark scale (23ms vs 31ms best, 400k traces).  Re-evaluate if the
    /// table grows past 10M distinct traces.
    ///
    /// `deployment_environment` is a denormalized LowCardinality column at
    /// ingest time; there is no CH→Postgres JOIN for environment names. The
    /// `environment_id` filter in `TraceQuery` is therefore ignored here
    /// (CH has no access to the `environments` Postgres table). This mirrors
    /// the trade-off documented in ADR-016 §Consequences → Relational JOINs.
    async fn query_trace_summaries(&self, query: TraceQuery) -> StorageResult<Vec<TraceSummary>> {
        let limit = query.limit.unwrap_or(50).min(100);
        let offset = query.offset.unwrap_or(0);

        // ── Build WHERE clause and ordered bind list ────────────────────────
        enum Bv {
            I32(i32),
            I64(i64),
            F64(f64),
            Str(String),
        }

        let mut where_parts: Vec<String> = vec!["project_id = ?".to_owned()];
        let mut binds: Vec<Bv> = vec![Bv::I32(query.project_id)];

        if let Some(ref tid) = query.trace_id {
            where_parts.push("trace_id = ?".to_owned());
            binds.push(Bv::Str(tid.clone()));
        }
        if let Some(ref svc) = query.service_name {
            where_parts.push("service_name = ?".to_owned());
            binds.push(Bv::Str(svc.clone()));
        }
        if let Some(min_dur) = query.min_duration_ms {
            where_parts.push("duration_ms >= ?".to_owned());
            binds.push(Bv::F64(min_dur));
        }
        if let Some(start) = query.start_time {
            where_parts.push("start_time >= fromUnixTimestamp64Milli(?)".to_owned());
            binds.push(Bv::I64(start.timestamp_millis()));
        }
        if let Some(end) = query.end_time {
            where_parts.push("start_time <= fromUnixTimestamp64Milli(?)".to_owned());
            binds.push(Bv::I64(end.timestamp_millis()));
        }
        if let Some(did) = query.deployment_id {
            where_parts.push("deployment_id = ?".to_owned());
            binds.push(Bv::I32(did));
        }
        // environment_id: skipped — no Postgres JOIN in CH (see doc comment).
        if let Some(ref attrs) = query.attributes {
            for (key, value) in attrs {
                where_parts.push("JSONExtractString(attributes, ?) = ?".to_owned());
                binds.push(Bv::Str(key.clone()));
                binds.push(Bv::Str(value.clone()));
            }
        }
        if let Some(ref pattern) = query.name_pattern {
            where_parts.push("name ILIKE ?".to_owned());
            binds.push(Bv::Str(format!("%{}%", escape_like_pattern(pattern))));
        }

        let where_sql = where_parts.join(" AND ");

        // ── HAVING clause for status filter ────────────────────────────────
        // Mirrors TimescaleDB: ERROR = has at least one ERROR span;
        // Ok = has zero ERROR spans. `HAVING` can reference aggregate exprs.
        let having_sql = match query.status {
            Some(SpanStatusCode::Error) => " HAVING countIf(status_code = 'ERROR') > 0",
            Some(SpanStatusCode::Ok) => " HAVING countIf(status_code = 'ERROR') = 0",
            _ => "",
        };

        // ── ORDER BY — enum-derived, injection-safe ─────────────────────────
        let order_dir = query.sort_order.as_sql();
        let order_sql = match query.sort_by {
            crate::types::TraceSortField::Duration => {
                format!("ORDER BY max_duration_ms {order_dir}, min(start_time) DESC, trace_id")
            }
            crate::types::TraceSortField::StartTime => {
                format!("ORDER BY min(start_time) {order_dir}, trace_id")
            }
        };

        // ── Full query ──────────────────────────────────────────────────────
        // argMax(name, …) picks the root-span name: root spans have
        // parent_span_id = '' (our empty-string sentinel), so we boost their
        // priority with a large addend so argMax always selects them when
        // present; otherwise falls back to the longest span (max duration).
        let sql = format!(
            r#"SELECT
                trace_id,
                argMax(name,
                    CASE WHEN parent_span_id = '' THEN duration_ms + 1e15
                         ELSE duration_ms END) AS root_span_name,
                argMax(service_name,
                    CASE WHEN parent_span_id = '' THEN duration_ms + 1e15
                         ELSE duration_ms END) AS service_name,
                argMax(kind,
                    CASE WHEN parent_span_id = '' THEN duration_ms + 1e15
                         ELSE duration_ms END) AS kind,
                argMax(deployment_environment,
                    CASE WHEN parent_span_id = '' THEN duration_ms + 1e15
                         ELSE duration_ms END) AS deployment_environment,
                toUnixTimestamp64Milli(min(start_time)) AS start_time_ms,
                max(duration_ms) AS max_duration_ms,
                count() AS span_count,
                countIf(status_code = 'ERROR') AS error_count
            FROM spans FINAL
            WHERE {where_sql}
            GROUP BY trace_id
            {having_sql}
            {order_sql}
            LIMIT ? OFFSET ?"#
        );
        binds.push(Bv::I64(limit as i64));
        binds.push(Bv::I64(offset as i64));

        let mut q = self.ch.query(&sql);
        for b in binds {
            q = match b {
                Bv::I32(v) => q.bind(v),
                Bv::I64(v) => q.bind(v),
                Bv::F64(v) => q.bind(v),
                Bv::Str(v) => q.bind(v),
            };
        }

        let rows = q
            .fetch_all::<ChTraceSummaryRow>()
            .await
            .map_err(|e| ch_query_err("query_trace_summaries", e))?;

        let summaries = rows
            .into_iter()
            .map(|r| {
                use chrono::TimeZone;
                let start_time = chrono::Utc
                    .timestamp_millis_opt(r.start_time_ms)
                    .single()
                    .unwrap_or_default();
                let kind = str_to_span_kind(&r.kind);
                let status_code = if r.error_count > 0 {
                    SpanStatusCode::Error
                } else {
                    SpanStatusCode::Ok
                };
                let deployment_environment = if r.deployment_environment.is_empty() {
                    None
                } else {
                    Some(r.deployment_environment)
                };
                TraceSummary {
                    trace_id: r.trace_id,
                    root_span_name: r.root_span_name,
                    service_name: r.service_name,
                    deployment_environment,
                    kind,
                    status_code,
                    start_time,
                    duration_ms: r.max_duration_ms,
                    span_count: r.span_count as i64,
                    error_count: r.error_count as i64,
                }
            })
            .collect();

        Ok(summaries)
    }

    /// Count distinct traces matching the query filters (without pagination).
    ///
    /// Mirrors `query_trace_summaries` filters exactly — including `status`
    /// (via a HAVING on countIf) and `min_duration_ms` — so the pagination
    /// count matches the actual result set returned by that method.
    async fn count_traces(&self, query: TraceQuery) -> StorageResult<u64> {
        enum Bv {
            I32(i32),
            I64(i64),
            F64(f64),
            Str(String),
        }

        let mut where_parts: Vec<String> = vec!["project_id = ?".to_owned()];
        let mut binds: Vec<Bv> = vec![Bv::I32(query.project_id)];

        if let Some(ref tid) = query.trace_id {
            where_parts.push("trace_id = ?".to_owned());
            binds.push(Bv::Str(tid.clone()));
        }
        if let Some(ref svc) = query.service_name {
            where_parts.push("service_name = ?".to_owned());
            binds.push(Bv::Str(svc.clone()));
        }
        if let Some(min_dur) = query.min_duration_ms {
            where_parts.push("duration_ms >= ?".to_owned());
            binds.push(Bv::F64(min_dur));
        }
        if let Some(start) = query.start_time {
            where_parts.push("start_time >= fromUnixTimestamp64Milli(?)".to_owned());
            binds.push(Bv::I64(start.timestamp_millis()));
        }
        if let Some(end) = query.end_time {
            where_parts.push("start_time <= fromUnixTimestamp64Milli(?)".to_owned());
            binds.push(Bv::I64(end.timestamp_millis()));
        }
        if let Some(did) = query.deployment_id {
            where_parts.push("deployment_id = ?".to_owned());
            binds.push(Bv::I32(did));
        }
        if let Some(ref attrs) = query.attributes {
            for (key, value) in attrs {
                where_parts.push("JSONExtractString(attributes, ?) = ?".to_owned());
                binds.push(Bv::Str(key.clone()));
                binds.push(Bv::Str(value.clone()));
            }
        }
        if let Some(ref pattern) = query.name_pattern {
            where_parts.push("name ILIKE ?".to_owned());
            binds.push(Bv::Str(format!("%{}%", escape_like_pattern(pattern))));
        }

        let where_sql = where_parts.join(" AND ");

        // status filter mirrors query_trace_summaries: ERROR = at least one
        // ERROR span in the trace, OK = no ERROR spans. Implemented as HAVING
        // on the per-trace GROUP BY, wrapped in a subquery so the outer query
        // can COUNT the matching trace rows.
        let having_sql = match query.status {
            Some(SpanStatusCode::Error) => " HAVING countIf(status_code = 'ERROR') > 0",
            Some(SpanStatusCode::Ok) => " HAVING countIf(status_code = 'ERROR') = 0",
            _ => "",
        };

        // Use a subquery so we can apply HAVING on per-trace aggregates and
        // then count the filtered set. `uniqExact` on the outer query is not
        // needed because the inner GROUP BY already yields one row per trace.
        let sql = format!(
            "SELECT count() AS cnt FROM (\
                SELECT trace_id \
                FROM spans FINAL \
                WHERE {where_sql} \
                GROUP BY trace_id\
                {having_sql}\
            )"
        );

        let mut q = self.ch.query(&sql);
        for b in binds {
            q = match b {
                Bv::I32(v) => q.bind(v),
                Bv::I64(v) => q.bind(v),
                Bv::F64(v) => q.bind(v),
                Bv::Str(v) => q.bind(v),
            };
        }

        let row = q
            .fetch_one::<ChCountRow>()
            .await
            .map_err(|e| ch_query_err("count_traces", e))?;

        Ok(row.cnt)
    }

    /// Fetch all spans of a single trace, ordered by start_time ASC.
    async fn get_trace(&self, project_id: i32, trace_id: &str) -> StorageResult<Vec<SpanRecord>> {
        // Simple point-lookup — the ORDER BY (project_id, trace_id, span_id)
        // primary index makes this a sequential read of one contiguous block.
        #[derive(::clickhouse::Row, Deserialize, Debug)]
        struct ChRawSpanRow {
            project_id: i32,
            deployment_id: Option<i32>,
            service_name: String,
            service_version: String,
            deployment_environment: String,
            trace_id: String,
            span_id: String,
            parent_span_id: String,
            name: String,
            kind: String,
            start_time_ms: i64,
            end_time_ms: i64,
            duration_ms: f64,
            status_code: String,
            status_message: String,
            attributes: String,
            events: String,
        }

        let sql = "SELECT project_id, deployment_id, service_name, service_version, \
                   deployment_environment, trace_id, span_id, parent_span_id, name, kind, \
                   toUnixTimestamp64Milli(start_time) AS start_time_ms, \
                   toUnixTimestamp64Milli(end_time) AS end_time_ms, \
                   duration_ms, status_code, status_message, attributes, events \
                   FROM spans FINAL \
                   WHERE project_id = ? AND trace_id = ? \
                   ORDER BY start_time ASC";

        let rows = self
            .ch
            .query(sql)
            .bind(project_id)
            .bind(trace_id)
            .fetch_all::<ChRawSpanRow>()
            .await
            .map_err(|e| ch_query_err("get_trace", e))?;

        let spans: Vec<SpanRecord> = rows
            .into_iter()
            .map(|r| {
                use chrono::TimeZone;
                let start_time = chrono::Utc
                    .timestamp_millis_opt(r.start_time_ms)
                    .single()
                    .unwrap_or_default();
                let end_time = chrono::Utc
                    .timestamp_millis_opt(r.end_time_ms)
                    .single()
                    .unwrap_or_default();
                let attributes: std::collections::BTreeMap<String, String> =
                    serde_json::from_str(&r.attributes).unwrap_or_default();
                let events: Vec<SpanEvent> = serde_json::from_str(&r.events).unwrap_or_default();
                let resource = crate::types::ResourceInfo {
                    service_name: r.service_name,
                    service_version: if r.service_version.is_empty() {
                        None
                    } else {
                        Some(r.service_version)
                    },
                    deployment_environment: if r.deployment_environment.is_empty() {
                        None
                    } else {
                        Some(r.deployment_environment)
                    },
                    attributes: std::collections::BTreeMap::new(),
                };
                SpanRecord {
                    project_id: r.project_id,
                    deployment_id: r.deployment_id,
                    resource,
                    trace_id: r.trace_id,
                    span_id: r.span_id,
                    parent_span_id: if r.parent_span_id.is_empty() {
                        None
                    } else {
                        Some(r.parent_span_id)
                    },
                    name: r.name,
                    kind: str_to_span_kind(&r.kind),
                    start_time,
                    end_time,
                    duration_ms: r.duration_ms,
                    status_code: str_to_span_status(&r.status_code),
                    status_message: r.status_message,
                    attributes,
                    events,
                }
            })
            .collect();

        debug!(
            project_id,
            trace_id,
            count = spans.len(),
            "ClickHouseOtelStorage: get_trace"
        );
        Ok(spans)
    }

    /// List GenAI trace summaries from ClickHouse.
    ///
    /// A GenAI trace is one that has at least one span with the
    /// `gen_ai.system` or `gen_ai.provider.name` attribute (same definition
    /// as the TimescaleDB implementation).  We use `JSONHas` to detect presence
    /// and `JSONExtractString` to extract values from the JSON-as-String
    /// `attributes` column.
    ///
    /// `deployment_environment`, `gen_ai.system`, `gen_ai.request.model`, and
    /// `gen_ai.operation.name` are extracted from the attributes JSON. Token
    /// counts are extracted and summed per trace.
    async fn query_genai_trace_summaries(
        &self,
        query: TraceQuery,
    ) -> StorageResult<Vec<GenAiTraceSummary>> {
        let limit = query.limit.unwrap_or(50).min(100);
        let offset = query.offset.unwrap_or(0);

        enum Bv {
            I32(i32),
            I64(i64),
            Str(String),
        }

        // Base filter: must be a GenAI span.
        let mut where_parts: Vec<String> = vec![
            "project_id = ?".to_owned(),
            "(JSONHas(attributes, 'gen_ai.system') = 1 OR JSONHas(attributes, 'gen_ai.provider.name') = 1)".to_owned(),
        ];
        let mut binds: Vec<Bv> = vec![Bv::I32(query.project_id)];

        if let Some(ref svc) = query.service_name {
            where_parts.push("service_name = ?".to_owned());
            binds.push(Bv::Str(svc.clone()));
        }
        if let Some(start) = query.start_time {
            where_parts.push("start_time >= fromUnixTimestamp64Milli(?)".to_owned());
            binds.push(Bv::I64(start.timestamp_millis()));
        }
        if let Some(end) = query.end_time {
            where_parts.push("start_time <= fromUnixTimestamp64Milli(?)".to_owned());
            binds.push(Bv::I64(end.timestamp_millis()));
        }
        if let Some(ref attrs) = query.attributes {
            for (key, value) in attrs {
                // Mirror the TimescaleDB impl: gen_ai.system queries also
                // check the deprecated gen_ai.provider.name.
                match key.as_str() {
                    "gen_ai.system" => {
                        where_parts.push(
                            "coalesce(nullIf(JSONExtractString(attributes, 'gen_ai.provider.name'), ''), \
                             JSONExtractString(attributes, 'gen_ai.system')) = ?".to_owned(),
                        );
                        binds.push(Bv::Str(value.clone()));
                    }
                    _ => {
                        where_parts.push("JSONExtractString(attributes, ?) = ?".to_owned());
                        binds.push(Bv::Str(key.clone()));
                        binds.push(Bv::Str(value.clone()));
                    }
                }
            }
        }

        let where_sql = where_parts.join(" AND ");

        // Per-trace aggregation: pick root span name from the span with the
        // highest priority (root span = parent_span_id = '' gets the boost).
        // Token fields are SUM across the trace; use 0 as the sentinel for
        // missing values (ifNull), then coerce back to nullable below.
        let sql = format!(
            r#"SELECT
                trace_id,
                argMax(name,
                    CASE WHEN parent_span_id = '' THEN duration_ms + 1e15
                         ELSE duration_ms END) AS root_span_name,
                argMax(service_name,
                    CASE WHEN parent_span_id = '' THEN duration_ms + 1e15
                         ELSE duration_ms END) AS service_name,
                argMaxIf(
                    coalesce(nullIf(JSONExtractString(attributes, 'gen_ai.provider.name'), ''),
                             JSONExtractString(attributes, 'gen_ai.system')),
                    start_time,
                    JSONExtractString(attributes, 'gen_ai.system') != ''
                    OR JSONExtractString(attributes, 'gen_ai.provider.name') != ''
                ) AS gen_ai_system,
                argMaxIf(
                    JSONExtractString(attributes, 'gen_ai.request.model'),
                    start_time,
                    JSONExtractString(attributes, 'gen_ai.request.model') != ''
                ) AS gen_ai_model,
                argMaxIf(
                    JSONExtractString(attributes, 'gen_ai.operation.name'),
                    start_time,
                    JSONExtractString(attributes, 'gen_ai.operation.name') != ''
                ) AS gen_ai_operation,
                toUnixTimestamp64Milli(min(start_time)) AS start_time_ms,
                max(duration_ms) AS max_duration_ms,
                count() AS span_count,
                countIf(status_code = 'ERROR') AS error_count,
                sumIf(
                    toInt64OrZero(coalesce(
                        nullIf(JSONExtractString(attributes, 'gen_ai.usage.input_tokens'), ''),
                        JSONExtractString(attributes, 'gen_ai.usage.prompt_tokens')
                    )),
                    JSONExtractString(attributes, 'gen_ai.usage.input_tokens') != ''
                    OR JSONExtractString(attributes, 'gen_ai.usage.prompt_tokens') != ''
                ) AS total_input_tokens,
                sumIf(
                    toInt64OrZero(coalesce(
                        nullIf(JSONExtractString(attributes, 'gen_ai.usage.output_tokens'), ''),
                        JSONExtractString(attributes, 'gen_ai.usage.completion_tokens')
                    )),
                    JSONExtractString(attributes, 'gen_ai.usage.output_tokens') != ''
                    OR JSONExtractString(attributes, 'gen_ai.usage.completion_tokens') != ''
                ) AS total_output_tokens,
                sumIf(
                    toInt64OrZero(JSONExtractString(attributes, 'gen_ai.usage.cache_creation.input_tokens')),
                    JSONExtractString(attributes, 'gen_ai.usage.cache_creation.input_tokens') != ''
                ) AS total_cache_creation_input_tokens,
                sumIf(
                    toInt64OrZero(JSONExtractString(attributes, 'gen_ai.usage.cache_read.input_tokens')),
                    JSONExtractString(attributes, 'gen_ai.usage.cache_read.input_tokens') != ''
                ) AS total_cache_read_input_tokens
            FROM spans FINAL
            WHERE {where_sql}
            GROUP BY trace_id
            ORDER BY min(start_time) DESC
            LIMIT ? OFFSET ?"#
        );
        binds.push(Bv::I64(limit as i64));
        binds.push(Bv::I64(offset as i64));

        let mut q = self.ch.query(&sql);
        for b in binds {
            q = match b {
                Bv::I32(v) => q.bind(v),
                Bv::I64(v) => q.bind(v),
                Bv::Str(v) => q.bind(v),
            };
        }

        let rows = q
            .fetch_all::<ChGenAiSummaryRow>()
            .await
            .map_err(|e| ch_query_err("query_genai_trace_summaries", e))?;

        let summaries = rows
            .into_iter()
            .map(|r| {
                use chrono::TimeZone;
                let start_time = chrono::Utc
                    .timestamp_millis_opt(r.start_time_ms)
                    .single()
                    .unwrap_or_default();
                // Empty-string sentinels → None to match TimescaleDB shape.
                let gen_ai_system = if r.gen_ai_system.is_empty() {
                    None
                } else {
                    Some(r.gen_ai_system)
                };
                let gen_ai_model = if r.gen_ai_model.is_empty() {
                    None
                } else {
                    Some(r.gen_ai_model)
                };
                let gen_ai_operation = if r.gen_ai_operation.is_empty() {
                    None
                } else {
                    Some(r.gen_ai_operation)
                };
                // Token totals: 0 means "no spans contributed" → None.
                let opt_i64 = |v: i64| if v == 0 { None } else { Some(v) };

                GenAiTraceSummary {
                    trace_id: r.trace_id,
                    root_span_name: r.root_span_name,
                    service_name: r.service_name,
                    gen_ai_system,
                    gen_ai_model,
                    gen_ai_operation,
                    start_time,
                    duration_ms: r.max_duration_ms,
                    span_count: r.span_count as i64,
                    error_count: r.error_count as i64,
                    total_input_tokens: opt_i64(r.total_input_tokens),
                    total_output_tokens: opt_i64(r.total_output_tokens),
                    total_cache_creation_input_tokens: opt_i64(r.total_cache_creation_input_tokens),
                    total_cache_read_input_tokens: opt_i64(r.total_cache_read_input_tokens),
                }
            })
            .collect();

        Ok(summaries)
    }

    /// Fetch all spans of one trace for the GenAI detail view.
    ///
    /// Identical to `get_trace` but used by the GenAI handler; the trace was
    /// already validated as a GenAI trace by `query_genai_trace_summaries`.
    async fn get_genai_trace_spans(
        &self,
        project_id: i32,
        trace_id: &str,
    ) -> StorageResult<Vec<GenAiSpanDetail>> {
        #[derive(::clickhouse::Row, Deserialize, Debug)]
        struct ChGenAiSpanRow {
            span_id: String,
            parent_span_id: String,
            name: String,
            kind: String,
            start_time_ms: i64,
            duration_ms: f64,
            status_code: String,
            attributes: String,
        }

        let sql = "SELECT span_id, parent_span_id, name, kind, \
                   toUnixTimestamp64Milli(start_time) AS start_time_ms, \
                   duration_ms, status_code, attributes \
                   FROM spans FINAL \
                   WHERE project_id = ? AND trace_id = ? \
                   ORDER BY start_time ASC";

        let rows = self
            .ch
            .query(sql)
            .bind(project_id)
            .bind(trace_id)
            .fetch_all::<ChGenAiSpanRow>()
            .await
            .map_err(|e| ch_query_err("get_genai_trace_spans", e))?;

        let spans = rows
            .into_iter()
            .map(|r| {
                use chrono::TimeZone;
                let start_time = chrono::Utc
                    .timestamp_millis_opt(r.start_time_ms)
                    .single()
                    .unwrap_or_default();
                let attrs: std::collections::BTreeMap<String, String> =
                    serde_json::from_str(&r.attributes).unwrap_or_default();
                let kind = str_to_span_kind(&r.kind);
                let status_code = str_to_span_status(&r.status_code);
                let parent_span_id = if r.parent_span_id.is_empty() {
                    None
                } else {
                    Some(r.parent_span_id)
                };

                GenAiSpanDetail::from_span_attrs(
                    r.span_id,
                    parent_span_id,
                    r.name,
                    kind,
                    start_time,
                    r.duration_ms,
                    status_code,
                    attrs,
                )
            })
            .collect();

        Ok(spans)
    }

    /// Count distinct GenAI traces matching the query filters.
    async fn count_genai_traces(&self, query: TraceQuery) -> StorageResult<u64> {
        enum Bv {
            I32(i32),
            I64(i64),
            Str(String),
        }

        let mut where_parts: Vec<String> = vec![
            "project_id = ?".to_owned(),
            "(JSONHas(attributes, 'gen_ai.system') = 1 OR JSONHas(attributes, 'gen_ai.provider.name') = 1)".to_owned(),
        ];
        let mut binds: Vec<Bv> = vec![Bv::I32(query.project_id)];

        if let Some(ref svc) = query.service_name {
            where_parts.push("service_name = ?".to_owned());
            binds.push(Bv::Str(svc.clone()));
        }
        if let Some(start) = query.start_time {
            where_parts.push("start_time >= fromUnixTimestamp64Milli(?)".to_owned());
            binds.push(Bv::I64(start.timestamp_millis()));
        }
        if let Some(end) = query.end_time {
            where_parts.push("start_time <= fromUnixTimestamp64Milli(?)".to_owned());
            binds.push(Bv::I64(end.timestamp_millis()));
        }
        if let Some(ref attrs) = query.attributes {
            for (key, value) in attrs {
                match key.as_str() {
                    "gen_ai.system" => {
                        where_parts.push(
                            "coalesce(nullIf(JSONExtractString(attributes, 'gen_ai.provider.name'), ''), \
                             JSONExtractString(attributes, 'gen_ai.system')) = ?".to_owned(),
                        );
                        binds.push(Bv::Str(value.clone()));
                    }
                    _ => {
                        where_parts.push("JSONExtractString(attributes, ?) = ?".to_owned());
                        binds.push(Bv::Str(key.clone()));
                        binds.push(Bv::Str(value.clone()));
                    }
                }
            }
        }

        let where_sql = where_parts.join(" AND ");
        let sql = format!("SELECT uniqExact(trace_id) AS cnt FROM spans FINAL WHERE {where_sql}");

        let mut q = self.ch.query(&sql);
        for b in binds {
            q = match b {
                Bv::I32(v) => q.bind(v),
                Bv::I64(v) => q.bind(v),
                Bv::Str(v) => q.bind(v),
            };
        }

        let row = q
            .fetch_one::<ChCountRow>()
            .await
            .map_err(|e| ch_query_err("count_genai_traces", e))?;

        Ok(row.cnt)
    }

    /// Extract GenAI-related span events from one trace.
    ///
    /// Events are stored as a JSON array in the `events` String column.  We
    /// fetch the raw JSON per-span and parse it in Rust, mirroring exactly
    /// what the TimescaleDB implementation does with its JSONB column.
    async fn get_genai_trace_events(
        &self,
        project_id: i32,
        trace_id: &str,
    ) -> StorageResult<Vec<GenAiEvent>> {
        // Fetch spans that have at least one event (non-empty JSON array).
        // JSONLength returns 0 for '[]', so the filter keeps only spans with events.
        let sql = "SELECT span_id, events \
                   FROM spans FINAL \
                   WHERE project_id = ? AND trace_id = ? \
                   AND JSONLength(events) > 0 \
                   ORDER BY start_time ASC";

        let rows = self
            .ch
            .query(sql)
            .bind(project_id)
            .bind(trace_id)
            .fetch_all::<ChSpanEventsRow>()
            .await
            .map_err(|e| ch_query_err("get_genai_trace_events", e))?;

        let mut events: Vec<GenAiEvent> = Vec::new();
        for row in rows {
            let event_array: Vec<serde_json::Value> =
                serde_json::from_str(&row.events).unwrap_or_default();
            for event in event_array {
                let event_name = event
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                // Only include gen_ai.* events, matching TimescaleDB impl.
                if !event_name.starts_with("gen_ai.") {
                    continue;
                }
                let raw_ts = event.get("timestamp").and_then(|v| v.as_str());
                let timestamp = raw_ts
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .unwrap_or_else(|| {
                        if raw_ts.is_some() {
                            tracing::warn!(
                                span_id = %row.span_id,
                                raw_timestamp = raw_ts,
                                "get_genai_trace_events: unparsable span event timestamp; \
                                 substituting Unix epoch"
                            );
                        }
                        chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap_or_default()
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
                    span_id: row.span_id.clone(),
                    trace_id: trace_id.to_string(),
                    event_name: event_name.to_string(),
                    timestamp,
                    attributes: attrs,
                });
            }
        }

        Ok(events)
    }

    // ── Non-span methods — delegate to TimescaleDB unconditionally ───────────
    // (ADR-016 Phases 2–4 will replace these with CH implementations)

    async fn store_metrics(&self, points: Vec<MetricPoint>) -> StorageResult<u64> {
        self.inner.store_metrics(points).await
    }

    async fn store_logs(&self, records: Vec<LogRecord>) -> StorageResult<u64> {
        self.inner.store_logs(records).await
    }

    async fn archive_logs(&self, records: Vec<LogRecord>) -> StorageResult<u64> {
        self.inner.archive_logs(records).await
    }

    async fn query_metrics(&self, query: MetricQuery) -> StorageResult<Vec<MetricBucket>> {
        self.inner.query_metrics(query).await
    }

    async fn list_metric_names(&self, project_id: i32) -> StorageResult<Vec<String>> {
        self.inner.list_metric_names(project_id).await
    }

    async fn query_logs(&self, query: LogQuery) -> StorageResult<Vec<LogRecord>> {
        self.inner.query_logs(query).await
    }

    // ── Control-row methods — always Postgres (insights, health, quota) ──────

    async fn upsert_insight(&self, insight: &Insight) -> StorageResult<i64> {
        self.inner.upsert_insight(insight).await
    }

    async fn list_insights(
        &self,
        project_id: i32,
        status: Option<InsightStatus>,
        limit: u64,
        offset: u64,
    ) -> StorageResult<Vec<Insight>> {
        self.inner
            .list_insights(project_id, status, limit, offset)
            .await
    }

    async fn resolve_insight(&self, insight_id: i64) -> StorageResult<()> {
        self.inner.resolve_insight(insight_id).await
    }

    async fn store_health_summary(&self, summary: &HealthSummary) -> StorageResult<()> {
        self.inner.store_health_summary(summary).await
    }

    async fn get_health_summaries(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
    ) -> StorageResult<Vec<HealthSummary>> {
        self.inner
            .get_health_summaries(project_id, environment_id)
            .await
    }

    async fn get_storage_quota(&self, project_id: i32) -> StorageResult<StorageQuota> {
        self.inner.get_storage_quota(project_id).await
    }

    async fn check_quota(&self, project_id: i32) -> StorageResult<bool> {
        self.inner.check_quota(project_id).await
    }

    // ── Anomaly-detection helpers — delegate to TimescaleDB ──────────────────

    async fn get_metric_baseline(
        &self,
        project_id: i32,
        service_name: &str,
        metric_name: &str,
        environment: Option<&str>,
        lookback_days: i32,
    ) -> StorageResult<Vec<BaselinePoint>> {
        self.inner
            .get_metric_baseline(
                project_id,
                service_name,
                metric_name,
                environment,
                lookback_days,
            )
            .await
    }

    async fn get_recent_minute_aggregates(
        &self,
        project_id: i32,
        service_name: &str,
        metric_name: &str,
        environment: Option<&str>,
        minutes: i32,
    ) -> StorageResult<Vec<MinuteAggregate>> {
        self.inner
            .get_recent_minute_aggregates(
                project_id,
                service_name,
                metric_name,
                environment,
                minutes,
            )
            .await
    }

    async fn get_recent_deploys(
        &self,
        project_id: i32,
        minutes: i32,
    ) -> StorageResult<Vec<DeployEvent>> {
        self.inner.get_recent_deploys(project_id, minutes).await
    }

    async fn apply_retention(&self, project_id: i32) -> StorageResult<u64> {
        self.inner.apply_retention(project_id).await
    }

    async fn get_p95_latency(
        &self,
        project_id: i32,
        service_name: &str,
        window_minutes: i32,
    ) -> StorageResult<f64> {
        self.inner
            .get_p95_latency(project_id, service_name, window_minutes)
            .await
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ResourceInfo, SpanKind, SpanRecord, SpanStatusCode};
    use chrono::Utc;
    use std::collections::BTreeMap;

    fn make_span() -> SpanRecord {
        SpanRecord {
            project_id: 42,
            deployment_id: Some(7),
            resource: ResourceInfo {
                service_name: "my-service".into(),
                service_version: Some("1.2.3".into()),
                deployment_environment: Some("production".into()),
                attributes: BTreeMap::new(),
            },
            trace_id: "abc123".into(),
            span_id: "span001".into(),
            parent_span_id: Some("parent001".into()),
            name: "GET /api/v1/health".into(),
            kind: SpanKind::Server,
            start_time: Utc::now(),
            end_time: Utc::now(),
            duration_ms: 42.5,
            status_code: SpanStatusCode::Ok,
            status_message: "".into(),
            attributes: {
                let mut m = BTreeMap::new();
                m.insert("http.method".into(), "GET".into());
                m
            },
            events: vec![],
        }
    }

    #[test]
    fn span_record_to_ch_row_field_mapping() {
        let span = make_span();
        let row = ChSpanRow::from(&span);

        assert_eq!(row.project_id, 42);
        assert_eq!(row.deployment_id, Some(7));
        assert_eq!(row.service_name, "my-service");
        assert_eq!(row.service_version, "1.2.3");
        assert_eq!(row.deployment_environment, "production");
        assert_eq!(row.trace_id, "abc123");
        assert_eq!(row.span_id, "span001");
        assert_eq!(row.parent_span_id, "parent001");
        assert_eq!(row.name, "GET /api/v1/health");
        assert_eq!(row.kind, "SERVER");
        assert_eq!(row.duration_ms, 42.5);
        assert_eq!(row.status_code, "OK");
        assert_eq!(row.status_message, "");
        // attributes JSON must contain the key
        assert!(row.attributes.contains("http.method"));
        // events is an empty JSON array
        assert_eq!(row.events, "[]");
        // _version is a positive Unix millisecond timestamp
        assert!(row._version > 0);
    }

    #[test]
    fn root_span_gets_empty_parent_span_id() {
        let mut span = make_span();
        span.parent_span_id = None;
        let row = ChSpanRow::from(&span);
        assert_eq!(row.parent_span_id, "");
    }

    #[test]
    fn missing_service_version_and_env_become_empty_strings() {
        let mut span = make_span();
        span.resource.service_version = None;
        span.resource.deployment_environment = None;
        let row = ChSpanRow::from(&span);
        assert_eq!(row.service_version, "");
        assert_eq!(row.deployment_environment, "");
    }

    #[test]
    fn ch_row_roundtrips_to_span_record() {
        let original = make_span();
        let row = ChSpanRow::from(&original);
        let recovered = SpanRecord::from(row);

        assert_eq!(recovered.project_id, original.project_id);
        assert_eq!(recovered.deployment_id, original.deployment_id);
        assert_eq!(recovered.trace_id, original.trace_id);
        assert_eq!(recovered.span_id, original.span_id);
        assert_eq!(recovered.parent_span_id, original.parent_span_id);
        assert_eq!(recovered.name, original.name);
        assert_eq!(recovered.kind, original.kind);
        assert_eq!(recovered.duration_ms, original.duration_ms);
        assert_eq!(recovered.status_code, original.status_code);
        assert_eq!(
            recovered.resource.service_name,
            original.resource.service_name
        );
        assert_eq!(
            recovered.resource.service_version,
            original.resource.service_version
        );
        assert_eq!(
            recovered.resource.deployment_environment,
            original.resource.deployment_environment
        );
        // Timestamps round-trip to millisecond precision
        assert_eq!(
            recovered.start_time.timestamp_millis(),
            original.start_time.timestamp_millis()
        );
        assert_eq!(
            recovered.end_time.timestamp_millis(),
            original.end_time.timestamp_millis()
        );
        // Attributes survive the JSON round-trip
        assert_eq!(recovered.attributes, original.attributes);
    }

    #[test]
    fn root_span_ch_row_recovers_none_parent() {
        let mut span = make_span();
        span.parent_span_id = None;
        let row = ChSpanRow::from(&span);
        let recovered = SpanRecord::from(row);
        assert_eq!(recovered.parent_span_id, None);
    }

    #[test]
    fn span_kind_roundtrip() {
        for kind in [
            SpanKind::Unspecified,
            SpanKind::Internal,
            SpanKind::Server,
            SpanKind::Client,
            SpanKind::Producer,
            SpanKind::Consumer,
        ] {
            let s = span_kind_to_str(kind);
            assert_eq!(str_to_span_kind(s), kind);
        }
    }

    #[test]
    fn span_status_roundtrip() {
        for code in [
            SpanStatusCode::Unset,
            SpanStatusCode::Ok,
            SpanStatusCode::Error,
        ] {
            let s = span_status_to_str(code);
            assert_eq!(str_to_span_status(s), code);
        }
    }

    #[test]
    fn unknown_kind_string_becomes_unspecified() {
        assert_eq!(str_to_span_kind("BOGUS"), SpanKind::Unspecified);
    }

    #[test]
    fn unknown_status_string_becomes_unset() {
        assert_eq!(str_to_span_status("BOGUS"), SpanStatusCode::Unset);
    }

    // ── escape_like_pattern tests ─────────────────────────────────────────

    #[test]
    fn escape_plain_pattern_unchanged() {
        assert_eq!(escape_like_pattern("hello"), "hello");
        assert_eq!(escape_like_pattern("GET /api/v1"), "GET /api/v1");
    }

    #[test]
    fn escape_percent_metachar() {
        // A literal '%' in the user pattern must become '\%' so it does not
        // act as a wildcard in the LIKE expression.
        assert_eq!(escape_like_pattern("%"), "\\%");
        assert_eq!(escape_like_pattern("50%"), "50\\%");
    }

    #[test]
    fn escape_underscore_metachar() {
        assert_eq!(escape_like_pattern("_id"), "\\_id");
        assert_eq!(escape_like_pattern("user_name"), "user\\_name");
    }

    #[test]
    fn escape_backslash_first() {
        // A literal backslash must become '\\' and must be processed before
        // the other replacements so that introduced backslashes are not
        // double-escaped.
        assert_eq!(escape_like_pattern("\\"), "\\\\");
        // A pattern with both a backslash and a %:
        //   input:  `\%`  (backslash then percent)
        //   want:   `\\\%` (escaped backslash, then escaped percent)
        assert_eq!(escape_like_pattern("\\%"), "\\\\\\%");
    }

    #[test]
    fn wrapped_escaped_pattern_is_correct() {
        // Full round-trip: user types "50%" → ILIKE pattern "%50\%%"
        let pattern = "50%";
        let wrapped = format!("%{}%", escape_like_pattern(pattern));
        assert_eq!(wrapped, "%50\\%%");
    }

    // ── store_spans chunking tests ────────────────────────────────────────

    #[test]
    fn spans_chunks_split_correctly() {
        // Verify that a Vec of N spans is chunked into ceil(N/10_000) pieces.
        // 30_001 spans → 3 full chunks of 10_000 + 1 tail chunk of 1.
        let n = 30_001usize;
        let spans: Vec<SpanRecord> = (0..n).map(|_| make_span()).collect();
        let chunks: Vec<_> = spans.chunks(10_000).collect();
        // ceil(30_001 / 10_000) = 4 chunks
        assert_eq!(chunks.len(), 4);
        assert_eq!(chunks[0].len(), 10_000);
        assert_eq!(chunks[1].len(), 10_000);
        assert_eq!(chunks[2].len(), 10_000);
        assert_eq!(chunks[3].len(), 1);
        // Total preserved
        let total: usize = chunks.iter().map(|c| c.len()).sum();
        assert_eq!(total, n);
    }

    #[test]
    fn spans_chunks_below_batch_size_is_single_chunk() {
        let spans: Vec<SpanRecord> = (0..42).map(|_| make_span()).collect();
        let chunks: Vec<_> = spans.chunks(10_000).collect();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 42);
    }

    #[test]
    fn spans_chunks_exact_batch_size_is_single_chunk() {
        let spans: Vec<SpanRecord> = (0..10_000).map(|_| make_span()).collect();
        let chunks: Vec<_> = spans.chunks(10_000).collect();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 10_000);
    }

    #[test]
    fn ch_ingest_err_includes_operation_name() {
        let err = ::clickhouse::error::Error::BadResponse("network error".into());
        let otel_err = ch_ingest_err("store_spans", err);
        assert!(otel_err.to_string().contains("store_spans"));
        assert!(otel_err.to_string().contains("network error"));
    }

    #[test]
    fn ch_query_err_includes_operation_name() {
        let err = ::clickhouse::error::Error::BadResponse("timeout".into());
        let otel_err = ch_query_err("query_trace_summaries", err);
        assert!(otel_err.to_string().contains("query_trace_summaries"));
    }
}
