// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ClickHouse implementation of [`LineIndexSink`] (ADR-047 §2, §4, §7),
//! against either the instance's own ClickHouse or Temps Cloud's
//! (ADR-043).
//!
//! One `RowBinary` batch insert per sealed chunk into the target's lines
//! table. Locally the `attrs` JSON column is sent as a serialised string
//! (`input_format_binary_read_json_as_string=1`), which is how the Rust
//! client speaks to the `JSON` type. Inserts are `async_insert` with
//! `wait_for_async_insert=1`: many small chunks from idle containers
//! coalesce server-side, and the ack still means "durable".
//!
//! Which half of that applies depends on the [`LineIndexTarget`]:
//!
//! * [`LineIndexTarget::Local`] owns the schema — it migrates, it
//!   `MODIFY TTL`s, it `DELETE`s.
//! * [`LineIndexTarget::Cloud`] owns nothing. Cloud created the tables and
//!   Cloud ages them out; this side only reads (through the read proxy,
//!   always inside `within_query_budget`) and inserts (through the insert
//!   proxy). Neither proxy forwards per-request settings and neither
//!   accepts a `DELETE`, which is what shapes every Cloud-only difference
//!   below: text `attrs`, pseudonymous scoping columns, and tombstones
//!   instead of deletes. See [`CLOUD_LINES_TABLE`] for the exact contract.

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use clickhouse::Row;
use serde::Serialize;
use temps_clickhouse::{ClickHouseConfig, Migration, ServerVersion};
use temps_cloud_client::CloudLink;
use temps_entities::cloud_telemetry_fidelity::CloudTelemetryFidelity;
use tracing::{debug, info, warn};

use super::analytics::{Dialect, Scoping};
use super::{IndexOutcome, LineIndexBackend, LineIndexSink};
use crate::chunk::{level_to_u8, ChunkLabels};
use crate::error::LogAggregatorError;
use crate::parser::well_known;
use crate::types::{LogLine, LogStream};

/// Minimum server version: the `JSON` column type is production-ready from
/// 25.3 (ADR-047 §7).
pub const MIN_CLICKHOUSE_MAJOR: u32 = 25;
pub const MIN_CLICKHOUSE_MINOR: u32 = 3;

/// Number of operator-promotable facet slots (matches the schema).
pub const FACET_SLOTS: usize = 20;

const TRACKING_TABLE: &str = "_temps_ch_log_index_migrations";

const MIGRATIONS: &[Migration] = &[Migration {
    name: "0001_log_lines_index",
    sql: include_str!("../../migrations/clickhouse/0001_log_lines_index.sql"),
}];

/// The instance's own line-index table. Its DDL is in this repository
/// (`migrations/clickhouse/0001_log_lines_index.sql`) and this side applies
/// it.
pub(crate) const LOCAL_LINES_TABLE: &str = "log_lines_index";

/// The instance's own per-project/day attribute-key rollup, maintained by a
/// materialized view on [`LOCAL_LINES_TABLE`]. Cloud has no equivalent.
pub(crate) const LOCAL_ATTR_KEYS_TABLE: &str = "log_attr_keys";

/// Cloud-side tombstones for chunks this instance has forgotten.
///
/// Must match what Temps Cloud actually names it — see
/// [`CLOUD_LINES_TABLE`].
pub(crate) const CLOUD_FORGOTTEN_CHUNKS_TABLE: &str = "telemetry_log_forgotten_chunks";

/// Cloud-side table holding this tenant's log lines, and the DDL contract for
/// both Cloud tables — written down here because this is the only place that
/// owns it.
///
/// The Cloud schema is not in this repository, exactly as with
/// `temps-otel`'s `cloud_spans.rs`: what *is* here is the only statement of
/// what the instance sends and expects back. If the two diverge every query
/// and every insert fails upstream and nothing on this side can explain why.
///
/// ```sql
/// CREATE TABLE telemetry_log_lines
/// (
///     -- Which linked instance wrote the row. One Cloud tenant can hold
///     -- several instances and `chunk_seq` is only unique within one
///     -- manifest, so every read filters on this and every tombstone lookup
///     -- is correlated on it. `HMAC(instance_token, "instance\0log-line-index")`.
///     instance_ref         String,
///
///     -- Scoping. Cloud never learns a local id: both are
///     -- HMAC(instance_token, domain || '\0' || id), derived on this side by
///     -- `CloudLink::pseudonymize_telemetry_id`. `external_service_ref = ''`
///     -- is "this line belongs to a project, not an external service" — the
///     -- same role `external_service_id = 0` plays locally.
///     project_ref          String,
///     external_service_ref String DEFAULT '',
///
///     env                  LowCardinality(String),
///     service              LowCardinality(String),
///
///     -- Labels, not scoping keys: they identify nothing outside this
///     -- instance (a deploy id is meaningless without the instance's
///     -- database), they are only ever displayed and filtered on, and
///     -- pseudonymizing them would cost a facet the operator reads. Sent as
///     -- they are, exactly as locally.
///     deploy_id            Int32 DEFAULT 0,
///     container_id         LowCardinality(String),
///     node_id              Int32 DEFAULT 0,
///
///     ts                   DateTime64(3, 'UTC'),
///     level                Enum8('trace' = 0, 'debug' = 1, 'info' = 2, 'warn' = 3, 'error' = 4),
///     stream               Enum8('stdout' = 0, 'stderr' = 1),
///
///     chunk_seq            UInt64,
///     line_index           UInt32,
///
///     trace_id             String DEFAULT '',
///     span_id              String DEFAULT '',
///     request_id           String DEFAULT '',
///     status_code          UInt16 DEFAULT 0,
///     http_method          LowCardinality(String) DEFAULT '',
///     http_route           String DEFAULT '',
///     duration_ms          Float32 DEFAULT 0,
///
///     -- The residual-attributes JSON as *text*, not a `JSON` column. The
///     -- `JSON` type needs `input_format_binary_read_json_as_string` /
///     -- `output_format_binary_write_json_as_string` per request, and the
///     -- telemetry proxies forward a fixed parameter allow-list that has no
///     -- room for them. Read with `JSONExtract*` / `JSONHas` / `JSONType`.
///     attrs                String DEFAULT '{}',
///
///     facet_attr_1 Nullable(String), …, facet_attr_20 Nullable(String)
/// )
/// ENGINE = ReplacingMergeTree
/// PARTITION BY toDate(ts)
/// ORDER BY (instance_ref, project_ref, service, chunk_seq, line_index)
/// TTL toDateTime(ts) + INTERVAL <tenant retention> DAY
/// SETTINGS ttl_only_drop_parts = 1;
///
/// -- Chunks the instance has forgotten (compacted away, purged, or missing).
/// -- The insert proxy accepts inserts and the read proxy rejects `DELETE`, so
/// -- a tombstone is the only way to stop counting a line; every read here
/// -- excludes `chunk_seq IN (SELECT chunk_seq FROM … WHERE instance_ref = ?)`.
/// CREATE TABLE telemetry_log_forgotten_chunks
/// (
///     instance_ref String,
///     chunk_seq    UInt64,
///     forgotten_at DateTime64(3)
/// )
/// ENGINE = ReplacingMergeTree
/// ORDER BY (instance_ref, chunk_seq)
/// TTL toDateTime(forgotten_at) + INTERVAL <tenant retention> DAY;
/// ```
///
/// Retention is Cloud's alone on both tables: this side never issues
/// `ALTER … MODIFY TTL` (the proxies would reject it), so the tombstones must
/// age out on the same clock as the lines they mask — otherwise they
/// accumulate forever for lines that no longer exist.
///
/// How *much* of a line reaches these columns is not decided here: see
/// [`cloud_index_row`] for the per-project consent projection (ADR-040 §1).
pub(crate) const CLOUD_LINES_TABLE: &str = "telemetry_log_lines";

/// Where a [`ClickHouseLineIndex`] stores and reads its rows.
pub enum LineIndexTarget {
    /// The instance's own ClickHouse (`TEMPS_CLICKHOUSE_*`). Owns the
    /// schema: runs migrations, `MODIFY TTL`, `DELETE`.
    Local(ClickHouseConfig),
    /// Temps Cloud's tenant ClickHouse behind the telemetry proxies. Cloud
    /// owns the schema and the TTL; this side only reads and inserts.
    Cloud {
        link: Arc<CloudLink>,
        /// Read on the write path for each project's
        /// `cloud_telemetry_fidelity` and
        /// `cloud_telemetry_attribute_allowlist` — the consent that decides
        /// how much of a line may leave the instance (ADR-040 §1). Nothing
        /// leaves at more than `Metered` without a row here saying so.
        db: Arc<sea_orm::DatabaseConnection>,
    },
}

/// Why the ClickHouse line index could not be enabled — surfaced verbatim
/// through the capabilities endpoint so the operator knows what to fix.
#[derive(Debug, Clone)]
pub enum IndexUnavailable {
    /// No store at all: neither `TEMPS_CLICKHOUSE_*` nor a Cloud link.
    NotConfigured,
    /// Server reachable but too old.
    VersionTooOld { found: ServerVersion },
    /// Could not reach the server or run migrations.
    Unreachable { error: String },
    /// A Cloud target was chosen but the link cannot serve one.
    CloudUnavailable { error: String },
}

impl std::fmt::Display for IndexUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Reached only when every store is ruled out, including the
            // TimescaleDB fallback — so it names the stores, not one of them.
            Self::NotConfigured => write!(
                f,
                "no line index store is available: neither a local ClickHouse \
                 (TEMPS_CLICKHOUSE_URL, _DATABASE, _USER and _PASSWORD) nor a Temps Cloud link \
                 with telemetry enabled is configured"
            ),
            Self::VersionTooOld { found } => write!(
                f,
                "ClickHouse {found} found; log analytics needs >= \
                 {MIN_CLICKHOUSE_MAJOR}.{MIN_CLICKHOUSE_MINOR} for the JSON column type"
            ),
            Self::Unreachable { error } => {
                write!(f, "ClickHouse is configured but unavailable: {error}")
            }
            Self::CloudUnavailable { error } => write!(
                f,
                "Temps Cloud cannot host the log line index: {error} — link this instance and \
                 enable telemetry export in Settings → Temps Cloud"
            ),
        }
    }
}

// ── Per-project egress consent (ADR-040 §1) ─────────────────────────────

/// How long a resolved per-project consent stays valid.
///
/// Short enough that raising or lowering fidelity in the UI takes effect
/// without a restart, long enough that sealing a chunk is not a Postgres
/// round trip per line. Matched to the TTL `CloudPolicyCache` uses for spans
/// in spirit, not by import: `temps-log-aggregator` does not depend on
/// `temps-otel`, and a shared cache across the two would tie the log seal
/// path to the span ingest path's lifetime.
const CONSENT_TTL: std::time::Duration = std::time::Duration::from_secs(60);

/// The resolved answer to "how much of this project's log lines may leave
/// this instance".
#[derive(Debug, Clone, PartialEq, Eq)]
struct CloudLinePolicy {
    fidelity: CloudTelemetryFidelity,
    /// Exact-match keys permitted at `Queryable`. Empty — the default even
    /// there — ships no attributes at all.
    allowlist: Arc<BTreeSet<String>>,
}

impl CloudLinePolicy {
    /// The default and every failure's answer.
    fn metered() -> Self {
        Self {
            fidelity: CloudTelemetryFidelity::Metered,
            allowlist: Arc::new(BTreeSet::new()),
        }
    }

    fn is_queryable(&self) -> bool {
        self.fidelity.is_queryable()
    }

    /// Exact match only. No prefix or glob semantics — one `http.*` entry
    /// would widen egress to whatever a parser decides to extract next
    /// release.
    fn allows(&self, key: &str) -> bool {
        self.allowlist.contains(key)
    }
}

/// Only the two consent columns, so the seal path never pulls the whole
/// (wide) project row to read a flag.
#[derive(Debug, sea_orm::FromQueryResult)]
struct ProjectConsentRow {
    cloud_telemetry_fidelity: CloudTelemetryFidelity,
    cloud_telemetry_attribute_allowlist: Vec<String>,
}

/// TTL cache of per-project [`CloudLinePolicy`].
///
/// Every failure — a database error, a missing project row, a line with no
/// project at all — resolves to [`CloudLinePolicy::metered`], the same
/// direction `CloudPolicyCache` fails in for spans. Failing towards *less*
/// egress costs a project that opted in up to one TTL of reduced fidelity,
/// which is recoverable; failing the other way ships real service names and
/// attributes because a `SELECT` timed out, which is not.
struct CloudConsent {
    db: Arc<sea_orm::DatabaseConnection>,
    ttl: std::time::Duration,
    entries: std::sync::Mutex<HashMap<i32, (CloudLinePolicy, std::time::Instant)>>,
}

impl CloudConsent {
    fn new(db: Arc<sea_orm::DatabaseConnection>) -> Self {
        Self {
            db,
            ttl: CONSENT_TTL,
            entries: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// The policy for `project_id`. Never fails: it refuses to let a lookup
    /// problem widen what leaves the instance.
    async fn policy(&self, project_id: i32) -> CloudLinePolicy {
        // A line with no owning project (an external service's container)
        // has no row to consent with, so it never gets past Metered.
        if project_id <= 0 {
            return CloudLinePolicy::metered();
        }
        {
            let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((policy, at)) = entries.get(&project_id) {
                if at.elapsed() < self.ttl {
                    return policy.clone();
                }
            }
        }
        let policy = match self.fetch(project_id).await {
            Ok(Some(policy)) => policy,
            Ok(None) => {
                warn!(
                    project_id,
                    "no project row for a log chunk; mirroring it to Temps Cloud at `metered` \
                     fidelity"
                );
                CloudLinePolicy::metered()
            }
            Err(error) => {
                warn!(
                    project_id,
                    %error,
                    "could not read the Temps Cloud telemetry fidelity; mirroring this \
                     project's log lines at `metered` fidelity until the lookup succeeds"
                );
                // Deliberately not cached: a transient database error must
                // not pin a project to Metered for the whole TTL.
                return CloudLinePolicy::metered();
            }
        };
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(project_id, (policy.clone(), std::time::Instant::now()));
        policy
    }

    async fn fetch(&self, project_id: i32) -> Result<Option<CloudLinePolicy>, sea_orm::DbErr> {
        use sea_orm::{EntityTrait, QuerySelect};
        let row = temps_entities::projects::Entity::find_by_id(project_id)
            .select_only()
            .column(temps_entities::projects::Column::CloudTelemetryFidelity)
            .column(temps_entities::projects::Column::CloudTelemetryAttributeAllowlist)
            .into_model::<ProjectConsentRow>()
            .one(self.db.as_ref())
            .await?;
        Ok(row.map(|row| CloudLinePolicy {
            fidelity: row.cloud_telemetry_fidelity,
            allowlist: Arc::new(
                row.cloud_telemetry_attribute_allowlist
                    .into_iter()
                    .collect(),
            ),
        }))
    }
}

/// One `log_lines_index` row in insertion order (must match the DDL).
#[derive(Debug, Row, Serialize)]
struct IndexRow<'a> {
    project_id: i32,
    external_service_id: i32,
    env: &'a str,
    service: &'a str,
    deploy_id: i32,
    container_id: &'a str,
    node_id: i32,
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    ts: DateTime<Utc>,
    level: i8,
    stream: i8,
    chunk_seq: u64,
    line_index: u32,
    trace_id: &'a str,
    span_id: &'a str,
    request_id: &'a str,
    status_code: u16,
    http_method: &'a str,
    http_route: &'a str,
    duration_ms: f32,
    attrs: String,
    facet_attr_1: Option<&'a str>,
    facet_attr_2: Option<&'a str>,
    facet_attr_3: Option<&'a str>,
    facet_attr_4: Option<&'a str>,
    facet_attr_5: Option<&'a str>,
    facet_attr_6: Option<&'a str>,
    facet_attr_7: Option<&'a str>,
    facet_attr_8: Option<&'a str>,
    facet_attr_9: Option<&'a str>,
    facet_attr_10: Option<&'a str>,
    facet_attr_11: Option<&'a str>,
    facet_attr_12: Option<&'a str>,
    facet_attr_13: Option<&'a str>,
    facet_attr_14: Option<&'a str>,
    facet_attr_15: Option<&'a str>,
    facet_attr_16: Option<&'a str>,
    facet_attr_17: Option<&'a str>,
    facet_attr_18: Option<&'a str>,
    facet_attr_19: Option<&'a str>,
    facet_attr_20: Option<&'a str>,
}

/// The same row as Cloud stores it: the scoping columns are pseudonyms
/// (`external_service_ref = ''` is "none"), and how many of the remaining
/// columns are populated is the owning project's consent decision — see
/// [`cloud_index_row`]. Names and types are validated server-side on insert,
/// so a drift against the DDL in [`CLOUD_LINES_TABLE`] fails loudly rather
/// than writing garbage.
#[derive(Debug, Row, Serialize)]
struct CloudIndexRow<'a> {
    instance_ref: &'a str,
    project_ref: &'a str,
    external_service_ref: &'a str,
    env: &'a str,
    service: &'a str,
    deploy_id: i32,
    container_id: &'a str,
    node_id: i32,
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    ts: DateTime<Utc>,
    level: i8,
    stream: i8,
    chunk_seq: u64,
    line_index: u32,
    trace_id: &'a str,
    span_id: &'a str,
    request_id: &'a str,
    status_code: u16,
    http_method: &'a str,
    http_route: &'a str,
    duration_ms: f32,
    /// The residual-attributes JSON as text (the Cloud column is `String`).
    attrs: String,
    facet_attr_1: Option<&'a str>,
    facet_attr_2: Option<&'a str>,
    facet_attr_3: Option<&'a str>,
    facet_attr_4: Option<&'a str>,
    facet_attr_5: Option<&'a str>,
    facet_attr_6: Option<&'a str>,
    facet_attr_7: Option<&'a str>,
    facet_attr_8: Option<&'a str>,
    facet_attr_9: Option<&'a str>,
    facet_attr_10: Option<&'a str>,
    facet_attr_11: Option<&'a str>,
    facet_attr_12: Option<&'a str>,
    facet_attr_13: Option<&'a str>,
    facet_attr_14: Option<&'a str>,
    facet_attr_15: Option<&'a str>,
    facet_attr_16: Option<&'a str>,
    facet_attr_17: Option<&'a str>,
    facet_attr_18: Option<&'a str>,
    facet_attr_19: Option<&'a str>,
    facet_attr_20: Option<&'a str>,
}

/// The three pseudonyms every Cloud row carries, derived once per chunk.
struct CloudScope<'a> {
    instance_ref: &'a str,
    project_ref: &'a str,
    external_service_ref: &'a str,
}

/// Build one Cloud row at the owning project's consented fidelity
/// (ADR-040 §1) — the projection that decides what actually leaves this
/// instance.
///
/// This is the log-line counterpart of `cloud_span`, and deliberately the
/// same shape of decision:
///
/// * **Metered** (the default for every project, and the answer to every
///   failed or impossible consent lookup) ships only what billing and
///   liveness need: the three pseudonyms, the timestamp, the level and
///   stream, and the chunk pointer. Every label and every attribute column
///   is written at its absent value — `''`, `0`, `'{}'`, `NULL`. A log line
///   is application-controlled text: service names, container ids, routes
///   and extracted attributes routinely carry customer identifiers, so none
///   of it leaves without the project saying so.
/// * **Queryable** is opt-in per project and ships what makes a line
///   findable: the real labels (`env`, `service`, `container_id`,
///   `deploy_id`, `node_id`) and the real correlation ids (`trace_id`,
///   `span_id`, `request_id` — in the clear, as spans do at this tier, so
///   the line joins to the user's own traces). Everything that carries
///   application *content* — `http_route`, `http_method`, `status_code`,
///   `duration_ms`, the residual `attrs` and the promoted facet slots — stays
///   default-deny against `cloud_telemetry_attribute_allowlist`: a key ships
///   only if it is listed, and the default empty allowlist ships none of it.
fn cloud_index_row<'a>(
    seq: i64,
    labels: &'a ChunkLabels,
    line_index: u32,
    line: &'a LogLine,
    slots: &'a FacetSlots,
    scope: &CloudScope<'a>,
    policy: &CloudLinePolicy,
) -> CloudIndexRow<'a> {
    // The always-shipped skeleton, with every optional column at its absent
    // value. The `Queryable` branch below fills columns in; nothing is ever
    // populated by omission.
    let mut row = CloudIndexRow {
        instance_ref: scope.instance_ref,
        project_ref: scope.project_ref,
        external_service_ref: scope.external_service_ref,
        env: "",
        service: "",
        deploy_id: 0,
        container_id: "",
        node_id: 0,
        ts: line.ts,
        level: level_to_u8(line.level) as i8,
        stream: match line.stream {
            LogStream::Stdout => 0,
            LogStream::Stderr => 1,
        },
        chunk_seq: seq as u64,
        line_index,
        trace_id: "",
        span_id: "",
        request_id: "",
        status_code: 0,
        http_method: "",
        http_route: "",
        duration_ms: 0.0,
        attrs: "{}".into(),
        facet_attr_1: None,
        facet_attr_2: None,
        facet_attr_3: None,
        facet_attr_4: None,
        facet_attr_5: None,
        facet_attr_6: None,
        facet_attr_7: None,
        facet_attr_8: None,
        facet_attr_9: None,
        facet_attr_10: None,
        facet_attr_11: None,
        facet_attr_12: None,
        facet_attr_13: None,
        facet_attr_14: None,
        facet_attr_15: None,
        facet_attr_16: None,
        facet_attr_17: None,
        facet_attr_18: None,
        facet_attr_19: None,
        facet_attr_20: None,
    };
    if !policy.is_queryable() {
        return row;
    }

    let fields = line.fields.as_ref();
    let wk = fields.map(well_known).unwrap_or_default();
    row.env = &labels.env;
    row.service = &labels.service;
    row.deploy_id = labels.deploy_id.unwrap_or(0);
    row.container_id = &labels.container_id;
    row.node_id = labels.node_id.unwrap_or(0);
    // Trace and span ids ship as spans ship them at this tier (real, so the
    // line joins to its trace in Cloud). They come from the application's
    // own log line, not from this instance, but the parser only accepts them
    // under the trace/span keys and a project at `queryable` has already
    // consented to its trace ids leaving. `request_id` is different: it is
    // routinely copied from a caller-supplied header and can be anything, so
    // it is content and needs listing like every other key.
    row.trace_id = wk.trace_id.unwrap_or("");
    row.span_id = wk.span_id.unwrap_or("");
    if policy.allows("request_id") {
        row.request_id = wk.request_id.unwrap_or("");
    }
    // Everything below is content, and content is allowlisted by key.
    if policy.allows("http_method") {
        row.http_method = wk.http_method.unwrap_or("");
    }
    if policy.allows("http_route") {
        row.http_route = wk.http_route.unwrap_or("");
    }
    if policy.allows("status_code") {
        row.status_code = wk.status_code.unwrap_or(0);
    }
    if policy.allows("duration_ms") {
        row.duration_ms = wk.duration_ms.unwrap_or(0.0);
    }
    row.attrs = fields
        .map(|f| allowed_residual_attrs(f, policy))
        .unwrap_or_else(|| "{}".into());
    let values = allowed_slot_values(fields, slots, policy);
    row.facet_attr_1 = values[0];
    row.facet_attr_2 = values[1];
    row.facet_attr_3 = values[2];
    row.facet_attr_4 = values[3];
    row.facet_attr_5 = values[4];
    row.facet_attr_6 = values[5];
    row.facet_attr_7 = values[6];
    row.facet_attr_8 = values[7];
    row.facet_attr_9 = values[8];
    row.facet_attr_10 = values[9];
    row.facet_attr_11 = values[10];
    row.facet_attr_12 = values[11];
    row.facet_attr_13 = values[12];
    row.facet_attr_14 = values[13];
    row.facet_attr_15 = values[14];
    row.facet_attr_16 = values[15];
    row.facet_attr_17 = values[16];
    row.facet_attr_18 = values[17];
    row.facet_attr_19 = values[18];
    row.facet_attr_20 = values[19];
    row
}

/// [`residual_attrs`] with the allowlist applied: a key ships only if the
/// project listed it, so an empty allowlist yields `{}`.
fn allowed_residual_attrs(fields: &serde_json::Value, policy: &CloudLinePolicy) -> String {
    let Some(obj) = fields.as_object() else {
        return "{}".into();
    };
    let allowed: serde_json::Map<String, serde_json::Value> = obj
        .iter()
        .filter(|(k, _)| !crate::parser::is_canonical_key(k) && policy.allows(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if allowed.is_empty() {
        return "{}".into();
    }
    serde_json::to_string(&allowed).unwrap_or_else(|_| "{}".into())
}

/// [`slot_values`] with the allowlist applied. A promoted facet is still an
/// attribute value — promotion is a local indexing decision and says nothing
/// about egress.
fn allowed_slot_values<'a>(
    fields: Option<&'a serde_json::Value>,
    slots: &'a FacetSlots,
    policy: &CloudLinePolicy,
) -> [Option<&'a str>; FACET_SLOTS] {
    let mut out = slot_values(fields, slots);
    for (i, key) in slots.iter().enumerate() {
        if !key.as_deref().is_some_and(|k| policy.allows(k)) {
            out[i] = None;
        }
    }
    out
}

/// One tombstone: the chunk this instance no longer has, and when it said so.
#[derive(Debug, Row, Serialize)]
struct ForgottenChunkRow<'a> {
    instance_ref: &'a str,
    chunk_seq: u64,
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    forgotten_at: DateTime<Utc>,
}

/// Key → slot mapping for promoted facets (ADR-047 §2). `slots[n]` is the
/// attribute key written into `facet_attr_{n+1}`; `None` = free slot.
pub type FacetSlots = [Option<String>; FACET_SLOTS];

/// The live ClickHouse-backed sink, local or Cloud.
pub struct ClickHouseLineIndex {
    /// `Some` for a local target, which holds one long-lived client for both
    /// reads and writes. A Cloud target builds its clients per operation —
    /// see [`ClickHouseLineIndex::read_client`].
    client: Option<clickhouse::Client>,
    /// `Some` only where the version was probed (local).
    version: Option<ServerVersion>,
    dialect: Dialect,
    /// `Some` only for a Cloud target: nothing leaves the instance without
    /// the owning project's consent, so the write path always has this.
    consent: Option<CloudConsent>,
    slots: arc_swap::ArcSwap<FacetSlots>,
    /// Last TTL (days) applied with `ALTER TABLE … MODIFY TTL`; `0` = never.
    ttl_days: AtomicU32,
    /// Whether the "retention is Cloud's" note has been logged already;
    /// retention ticks are frequent and the note does not change.
    retention_note_logged: AtomicBool,
}

impl ClickHouseLineIndex {
    /// Connect to `target`. Returns the reason instead of a sink when
    /// anything rules the index out — the caller wires [`super::NoLineIndex`]
    /// with that reason.
    ///
    /// Locally that means gating on the server version and running
    /// migrations. For Cloud it means only that the instance is linked:
    /// persisted feature switches are applied *after* plugin startup
    /// (ADR-042), so `telemetry_enabled()` is routinely still false here on
    /// an instance that enables it a second later. Refusing on it would
    /// disable the index for the whole process lifetime over a race. The
    /// switch is enforced per operation instead, where it is current.
    pub async fn connect(target: LineIndexTarget) -> Result<Arc<Self>, IndexUnavailable> {
        let (client, version, dialect, consent) = match target {
            LineIndexTarget::Local(config) => {
                let client = config
                    .client()
                    .with_setting("input_format_binary_read_json_as_string", "1")
                    .with_setting("output_format_binary_write_json_as_string", "1")
                    .with_setting("async_insert", "1")
                    .with_setting("wait_for_async_insert", "1");

                let version = temps_clickhouse::server_version(&client)
                    .await
                    .map_err(|e| IndexUnavailable::Unreachable {
                        error: e.to_string(),
                    })?;
                if !version.at_least(MIN_CLICKHOUSE_MAJOR, MIN_CLICKHOUSE_MINOR) {
                    return Err(IndexUnavailable::VersionTooOld { found: version });
                }

                let report = temps_clickhouse::apply_migrations(
                    &client,
                    &config.database,
                    TRACKING_TABLE,
                    MIGRATIONS,
                )
                .await
                .map_err(|e| IndexUnavailable::Unreachable {
                    error: e.to_string(),
                })?;
                info!(
                    version = %version,
                    applied = ?report.applied,
                    skipped = report.skipped,
                    "log line index ready (ClickHouse)"
                );
                (Some(client), Some(version), Dialect::local(), None)
            }
            LineIndexTarget::Cloud { link, db } => {
                if !link.is_linked() {
                    return Err(IndexUnavailable::CloudUnavailable {
                        error: "this instance is not linked to Temps Cloud".into(),
                    });
                }
                // Nothing to probe: Cloud created the tables, runs a
                // supported server, and answering a version query at startup
                // would only add a round trip that can fail for reasons the
                // operator cannot act on.
                info!(
                    table = CLOUD_LINES_TABLE,
                    "log line index ready (Temps Cloud)"
                );
                (
                    None,
                    None,
                    Dialect::cloud(link),
                    Some(CloudConsent::new(db)),
                )
            }
        };

        Ok(Arc::new(Self {
            client,
            version,
            dialect,
            consent,
            slots: arc_swap::ArcSwap::from_pointee(Default::default()),
            ttl_days: AtomicU32::new(0),
            retention_note_logged: AtomicBool::new(false),
        }))
    }

    /// The probed server version — `None` for a Cloud target, which does not
    /// probe one.
    pub fn version(&self) -> Option<ServerVersion> {
        self.version
    }

    /// The instance's own client — `None` for a Cloud target, whose clients
    /// are built per operation from the live link.
    pub fn client(&self) -> Option<&clickhouse::Client> {
        self.client.as_ref()
    }

    fn link(&self) -> Option<&Arc<CloudLink>> {
        match &self.dialect.scoping {
            Scoping::Raw => None,
            Scoping::Pseudonymous(link) => Some(link),
        }
    }

    /// A client for reads. Built fresh on Cloud so the operator's current
    /// telemetry switch decides, not the one that happened to be loaded at
    /// startup.
    fn read_client(&self) -> Result<clickhouse::Client, LogAggregatorError> {
        match (&self.client, self.link()) {
            (Some(client), _) => Ok(client.clone()),
            (None, Some(link)) => link.clickhouse_query_client().map_err(cloud_err),
            (None, None) => Err(LogAggregatorError::LineIndex {
                reason: "the log line index has no client".into(),
            }),
        }
    }

    /// A client for inserts. The Cloud read proxy injects `readonly=1`, so
    /// writes go to the separate insert surface.
    fn write_client(&self) -> Result<clickhouse::Client, LogAggregatorError> {
        match (&self.client, self.link()) {
            (Some(client), _) => Ok(client.clone()),
            (None, Some(link)) => link.clickhouse_insert_client().map_err(cloud_err),
            (None, None) => Err(LogAggregatorError::LineIndex {
                reason: "the log line index has no client".into(),
            }),
        }
    }

    /// Run one read, bounded by Cloud's wall-clock budget where that applies.
    ///
    /// `clickhouse` 0.15 carries no timeout of its own, so a stalled proxy
    /// would otherwise hang the console request that asked for the facet.
    async fn fetch<T, F>(&self, what: &str, query: F) -> Result<T, LogAggregatorError>
    where
        F: Future<Output = Result<T, clickhouse::error::Error>>,
    {
        if !self.dialect.is_cloud() {
            return query.await.map_err(ch_err);
        }
        match temps_cloud_client::query::within_query_budget(query).await {
            Ok(outcome) => outcome.map_err(ch_err),
            Err(error) => Err(LogAggregatorError::LineIndex {
                reason: format!("Temps Cloud did not answer the {what} query in time: {error}"),
            }),
        }
    }

    /// Replace the promoted-facet mapping (called by the facet service when
    /// a key is promoted or removed).
    pub fn set_facet_slots(&self, slots: FacetSlots) {
        self.slots.store(Arc::new(slots));
    }

    /// Insert `lines` (chunk order, `line_index` starting at `first_index`)
    /// as rows for chunk `seq`.
    pub async fn insert_lines(
        &self,
        seq: i64,
        labels: &ChunkLabels,
        first_index: u32,
        lines: &[LogLine],
    ) -> Result<(), LogAggregatorError> {
        let slots = self.slots.load();
        let client = self.write_client()?;
        let table = self.dialect.lines_table;
        if !self.dialect.is_cloud() {
            let mut insert: clickhouse::insert::Insert<IndexRow<'_>> =
                client.insert(table).await.map_err(ch_err)?;
            for (offset, line) in lines.iter().enumerate() {
                let row = index_row(seq, labels, first_index + offset as u32, line, &slots);
                insert.write(&row).await.map_err(ch_err)?;
            }
            return insert.end().await.map_err(ch_err);
        }
        // Cloud scopes by pseudonym, and the whole chunk shares all three:
        // the labels are constant for it, so each HMAC is computed once.
        let instance_ref = self.dialect.instance_ref()?;
        let project_ref = self.dialect.project_ref(labels.project_id)?;
        let service_ref = self
            .dialect
            .external_service_ref(labels.external_service_id.unwrap_or(0))?;
        let scope = CloudScope {
            instance_ref: &instance_ref,
            project_ref: &project_ref,
            external_service_ref: &service_ref,
        };
        // One consent read per chunk, not per line: every line in a chunk
        // belongs to the same project by construction. Lines from an
        // external service have no project row and stay Metered.
        let policy = match (&self.consent, labels.external_service_id) {
            (Some(consent), None) => consent.policy(labels.project_id).await,
            _ => CloudLinePolicy::metered(),
        };
        let mut insert: clickhouse::insert::Insert<CloudIndexRow<'_>> =
            client.insert(table).await.map_err(ch_err)?;
        for (offset, line) in lines.iter().enumerate() {
            let row = cloud_index_row(
                seq,
                labels,
                first_index + offset as u32,
                line,
                &slots,
                &scope,
                &policy,
            );
            insert.write(&row).await.map_err(ch_err)?;
        }
        insert.end().await.map_err(ch_err)
    }
}

/// One row, in the local column order.
fn index_row<'a>(
    seq: i64,
    labels: &'a ChunkLabels,
    line_index: u32,
    line: &'a LogLine,
    slots: &'a FacetSlots,
) -> IndexRow<'a> {
    let fields = line.fields.as_ref();
    let wk = fields.map(well_known).unwrap_or_default();
    let attrs = fields.map(residual_attrs).unwrap_or_else(|| "{}".into());
    let slot_values = slot_values(fields, slots);
    IndexRow {
        project_id: labels.project_id,
        external_service_id: labels.external_service_id.unwrap_or(0),
        env: &labels.env,
        service: &labels.service,
        deploy_id: labels.deploy_id.unwrap_or(0),
        container_id: &labels.container_id,
        node_id: labels.node_id.unwrap_or(0),
        ts: line.ts,
        level: level_to_u8(line.level) as i8,
        stream: match line.stream {
            LogStream::Stdout => 0,
            LogStream::Stderr => 1,
        },
        chunk_seq: seq as u64,
        line_index,
        trace_id: wk.trace_id.unwrap_or(""),
        span_id: wk.span_id.unwrap_or(""),
        request_id: wk.request_id.unwrap_or(""),
        status_code: wk.status_code.unwrap_or(0),
        http_method: wk.http_method.unwrap_or(""),
        http_route: wk.http_route.unwrap_or(""),
        duration_ms: wk.duration_ms.unwrap_or(0.0),
        attrs,
        facet_attr_1: slot_values[0],
        facet_attr_2: slot_values[1],
        facet_attr_3: slot_values[2],
        facet_attr_4: slot_values[3],
        facet_attr_5: slot_values[4],
        facet_attr_6: slot_values[5],
        facet_attr_7: slot_values[6],
        facet_attr_8: slot_values[7],
        facet_attr_9: slot_values[8],
        facet_attr_10: slot_values[9],
        facet_attr_11: slot_values[10],
        facet_attr_12: slot_values[11],
        facet_attr_13: slot_values[12],
        facet_attr_14: slot_values[13],
        facet_attr_15: slot_values[14],
        facet_attr_16: slot_values[15],
        facet_attr_17: slot_values[16],
        facet_attr_18: slot_values[17],
        facet_attr_19: slot_values[18],
        facet_attr_20: slot_values[19],
    }
}

/// `fields` serialised without the canonical keys that already live in
/// fixed columns, so nothing is stored twice and the facet sidebar lists
/// each attribute once.
fn residual_attrs(fields: &serde_json::Value) -> String {
    let Some(obj) = fields.as_object() else {
        return "{}".into();
    };
    let residual: serde_json::Map<String, serde_json::Value> = obj
        .iter()
        .filter(|(k, _)| !crate::parser::is_canonical_key(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if residual.is_empty() {
        return "{}".into();
    }
    serde_json::to_string(&residual).unwrap_or_else(|_| "{}".into())
}

/// Values for the promoted slots, read from `fields` by key.
fn slot_values<'a>(
    fields: Option<&'a serde_json::Value>,
    slots: &'a FacetSlots,
) -> [Option<&'a str>; FACET_SLOTS] {
    let mut out = [None; FACET_SLOTS];
    let Some(obj) = fields.and_then(|f| f.as_object()) else {
        return out;
    };
    for (i, key) in slots.iter().enumerate() {
        if let Some(key) = key {
            out[i] = obj.get(key).and_then(|v| v.as_str());
        }
    }
    out
}

fn ch_err(e: clickhouse::error::Error) -> LogAggregatorError {
    LogAggregatorError::LineIndex {
        reason: e.to_string(),
    }
}

/// A link that will not serve a client says why (not enrolled, blocked,
/// telemetry switched off) — each needs a different fix, so the reason is
/// carried through to the operator rather than flattened.
fn cloud_err(e: temps_cloud_client::CloudError) -> LogAggregatorError {
    LogAggregatorError::LineIndex {
        reason: format!("Temps Cloud cannot serve the log line index: {e}"),
    }
}

#[async_trait]
impl LineIndexSink for ClickHouseLineIndex {
    async fn index_chunk(
        &self,
        seq: i64,
        labels: &ChunkLabels,
        segments: &[Arc<Vec<LogLine>>],
    ) -> Result<IndexOutcome, LogAggregatorError> {
        let mut first_index = 0u32;
        for segment in segments {
            if let Err(e) = self.insert_lines(seq, labels, first_index, segment).await {
                warn!(seq, error = %e, "line index insert failed");
                return Err(e);
            }
            first_index += segment.len() as u32;
        }
        Ok(IndexOutcome::Indexed)
    }

    async fn set_retention_days(&self, days: u32) -> Result<(), LogAggregatorError> {
        if self.dialect.is_cloud() {
            // Cloud owns the TTL on both its tables and the proxies reject
            // `ALTER`. Said once, at debug: retention ticks are frequent and
            // there is nothing here for the operator to act on.
            if !self.retention_note_logged.swap(true, Ordering::Relaxed) {
                debug!(
                    days,
                    "log line index retention is managed by Temps Cloud; not altering TTL"
                );
            }
            return Ok(());
        }
        let days = days.clamp(1, 3650);
        if self.ttl_days.load(Ordering::Relaxed) == days {
            return Ok(());
        }
        let client = self.read_client()?;
        // `MODIFY TTL` is metadata-only; expired parts are dropped by the
        // next TTL merge (`ttl_only_drop_parts` makes that a part drop, not
        // a rewrite).
        client
            .query(&format!(
                "ALTER TABLE {LOCAL_LINES_TABLE} MODIFY TTL toDateTime(ts) + INTERVAL {days} DAY"
            ))
            .execute()
            .await
            .map_err(ch_err)?;
        client
            .query(&format!(
                "ALTER TABLE {LOCAL_ATTR_KEYS_TABLE} MODIFY TTL day + INTERVAL {days} DAY"
            ))
            .execute()
            .await
            .map_err(ch_err)?;
        self.ttl_days.store(days, Ordering::Relaxed);
        info!(days, "line index TTL aligned with container log retention");
        Ok(())
    }

    async fn forget_chunks(&self, seqs: &[i64]) -> Result<(), LogAggregatorError> {
        if seqs.is_empty() {
            return Ok(());
        }
        if let Some(tombstones) = self.dialect.tombstones {
            // No `DELETE` through the Cloud proxies, so the rows are masked
            // instead: every read excludes these seqs, and Cloud's TTL ages
            // the lines and their tombstones out together.
            let client = self.write_client()?;
            let now = Utc::now();
            // Stamped with this instance's key: a tombstone must not mask a
            // sibling instance's chunk of the same sequence number.
            let instance_ref = self.dialect.instance_ref()?;
            let mut insert: clickhouse::insert::Insert<ForgottenChunkRow<'_>> =
                client.insert(tombstones).await.map_err(ch_err)?;
            for seq in seqs {
                insert
                    .write(&ForgottenChunkRow {
                        instance_ref: &instance_ref,
                        chunk_seq: *seq as u64,
                        forgotten_at: now,
                    })
                    .await
                    .map_err(ch_err)?;
            }
            return insert.end().await.map_err(ch_err);
        }
        // Lightweight DELETE (mask + background rewrite), not a full
        // mutation; cheap enough for compaction/purge cadence.
        let list = seqs
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .join(",");
        self.read_client()?
            .query(&format!(
                "DELETE FROM {LOCAL_LINES_TABLE} WHERE chunk_seq IN ({list})"
            ))
            .execute()
            .await
            .map_err(ch_err)
    }

    fn unavailable_reason(&self) -> Option<String> {
        // The operator's switch is the one thing that can turn a working
        // Cloud index off mid-run, and the capabilities endpoint has to say
        // so rather than showing an index that silently indexes nothing.
        let link = self.link()?;
        (!link.telemetry_enabled())
            .then(|| "Temps Cloud telemetry export is switched off (Settings → Temps Cloud)".into())
    }

    fn backend(&self) -> Option<LineIndexBackend> {
        Some(if self.dialect.is_cloud() {
            LineIndexBackend::TempsCloud
        } else {
            LineIndexBackend::ClickHouse
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn residual_attrs_drop_canonical_keys() {
        let fields = serde_json::json!({
            "request_id": "r1", "status_code": 500, "worker": "3"
        });
        assert_eq!(residual_attrs(&fields), r#"{"worker":"3"}"#);
        let only_canonical = serde_json::json!({"trace_id": "t"});
        assert_eq!(residual_attrs(&only_canonical), "{}");
    }

    #[test]
    fn slot_values_follow_the_mapping() {
        let fields = serde_json::json!({"user": "u1", "region": "eu", "n": 3});
        let mut slots: FacetSlots = Default::default();
        slots[0] = Some("region".into());
        slots[4] = Some("user".into());
        slots[5] = Some("n".into()); // not a string → None
        let v = slot_values(Some(&fields), &slots);
        assert_eq!(v[0], Some("eu"));
        assert_eq!(v[4], Some("u1"));
        assert_eq!(v[5], None);
        assert_eq!(v[1], None);
    }

    #[test]
    fn unavailable_reasons_name_the_fix() {
        let s = IndexUnavailable::NotConfigured.to_string();
        assert!(s.contains("TEMPS_CLICKHOUSE_URL"), "{s}");
        assert!(s.contains("Temps Cloud"), "both stores, not one: {s}");
        let s = IndexUnavailable::VersionTooOld {
            found: ServerVersion {
                major: 24,
                minor: 8,
                patch: 1,
            },
        }
        .to_string();
        assert!(s.contains("24.8") && s.contains("25.3"));
        let s = IndexUnavailable::CloudUnavailable {
            error: "this instance is not linked to Temps Cloud".into(),
        }
        .to_string();
        assert!(
            s.contains("not linked") && s.contains("Settings → Temps Cloud"),
            "{s}"
        );
    }

    /// An unlinked instance cannot host the index in Cloud, and says so with
    /// the one thing the operator can do about it.
    #[tokio::test]
    async fn an_unlinked_instance_refuses_a_cloud_target() {
        let dir = tempfile::tempdir().expect("temp dir");
        let link = Arc::new(temps_cloud_client::CloudLink::load(
            dir.path().to_path_buf(),
            "0.1.0-test",
        ));

        match ClickHouseLineIndex::connect(LineIndexTarget::Cloud {
            link,
            db: test_db(),
        })
        .await
        {
            Err(IndexUnavailable::CloudUnavailable { error }) => {
                assert!(error.contains("not linked"), "{error}")
            }
            Err(other) => panic!("expected a Cloud refusal, got {other}"),
            Ok(_) => panic!("an unlinked instance must not get a Cloud index"),
        }
    }

    /// A linked instance gets the index even while telemetry export is off:
    /// the persisted switches are applied after plugin startup, so refusing
    /// here would disable the index for the whole process over a race. The
    /// switch shows up as a live `unavailable_reason` instead, and clears
    /// itself the moment the operator turns it on. No network either way.
    #[tokio::test]
    async fn a_linked_instance_reports_the_telemetry_switch_rather_than_refusing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let link = super::super::analytics::test_cloud_link(dir.path());
        link.set_feature_switches(temps_cloud_client::CloudFeatureSwitches::default())
            .expect("apply feature switches");

        let index = ClickHouseLineIndex::connect(LineIndexTarget::Cloud {
            link: link.clone(),
            db: test_db(),
        })
        .await
        .expect("a linked instance can host the index in Cloud");

        assert_eq!(index.backend(), Some(LineIndexBackend::TempsCloud));
        assert!(index.version().is_none(), "Cloud probes no server version");
        assert!(
            index.client().is_none(),
            "Cloud builds its clients per call"
        );
        let reason = index
            .unavailable_reason()
            .expect("the switch must be surfaced, not silently indexing nothing");
        assert!(reason.contains("switched off") && reason.contains("Settings → Temps Cloud"));

        link.set_feature_switches(temps_cloud_client::CloudFeatureSwitches {
            telemetry: true,
            backups: false,
            notifications: false,
        })
        .expect("apply feature switches");
        assert_eq!(index.unavailable_reason(), None);
    }

    /// Retention on Cloud is Cloud's: the proxies reject `ALTER`, and a tick
    /// that tried would fail every time instead of doing nothing.
    #[tokio::test]
    async fn cloud_retention_is_a_no_op() {
        let dir = tempfile::tempdir().expect("temp dir");
        let index = ClickHouseLineIndex::connect(LineIndexTarget::Cloud {
            link: super::super::analytics::test_cloud_link(dir.path()),
            db: test_db(),
        })
        .await
        .expect("connect");
        index.set_retention_days(30).await.expect("no-op");
        index.set_retention_days(7).await.expect("still a no-op");
        // Nothing to forget is nothing to write, with or without a client.
        index.forget_chunks(&[]).await.expect("no-op");
    }

    // ── The Cloud egress projection (ADR-040 §1) ─────────────────────────

    /// A `DatabaseConnection` that answers nothing. Every consent test that
    /// needs an answer asks for one explicitly.
    fn test_db() -> Arc<sea_orm::DatabaseConnection> {
        Arc::new(sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection())
    }

    fn test_labels() -> ChunkLabels {
        let now = Utc::now();
        ChunkLabels {
            project_id: 7,
            external_service_id: None,
            env: "production".into(),
            service: "api".into(),
            container_id: "c0ffee".into(),
            deploy_id: Some(42),
            node_id: Some(3),
            node_name: None,
            started_at: now,
            ended_at: now,
            line_count: 1,
            level_mask: 0,
            level_counts: [0; 5],
        }
    }

    fn test_line() -> LogLine {
        LogLine {
            ts: Utc::now(),
            stream: LogStream::Stderr,
            level: crate::types::LogLevel::Error,
            msg: "boom".into(),
            fields: Some(serde_json::json!({
                "worker": "3",
                "customer_email": "person@example.test",
                "request_id": "r-1",
                "trace_id": "t-1",
                "status_code": 503,
                "duration_ms": 12.5,
                "http_route": "/orders/{id}",
                "http_method": "POST",
            })),
            container_id: "unused".into(),
            service: "unused".into(),
            env: "unused".into(),
            project_id: 7,
            external_service_id: None,
            deploy_id: None,
            node_id: None,
            node_name: None,
        }
    }

    fn test_slots() -> FacetSlots {
        let mut slots: FacetSlots = Default::default();
        slots[0] = Some("worker".into());
        slots[1] = Some("customer_email".into());
        slots
    }

    fn projected(policy: &CloudLinePolicy) -> (CloudIndexRow<'static>, &'static ChunkLabels) {
        // Leaked so the row's borrows outlive the helper; a test binary is
        // the one place where that is simply free.
        let labels: &'static ChunkLabels = Box::leak(Box::new(test_labels()));
        let line: &'static LogLine = Box::leak(Box::new(test_line()));
        let slots: &'static FacetSlots = Box::leak(Box::new(test_slots()));
        let scope = CloudScope {
            instance_ref: "i",
            project_ref: "p",
            external_service_ref: "",
        };
        (
            cloud_index_row(9, labels, 4, line, slots, &scope, policy),
            labels,
        )
    }

    #[test]
    fn a_metered_project_ships_no_labels_and_no_attributes() {
        // The default for every project. Only billing and liveness leave:
        // the pseudonyms, the clock, the severity and the chunk pointer.
        let (row, _) = projected(&CloudLinePolicy::metered());

        assert_eq!(row.instance_ref, "i");
        assert_eq!(row.project_ref, "p");
        assert_eq!(row.chunk_seq, 9);
        assert_eq!(row.line_index, 4);
        assert_eq!(row.stream, 1);

        assert_eq!(row.env, "");
        assert_eq!(row.service, "");
        assert_eq!(row.container_id, "");
        assert_eq!(row.deploy_id, 0);
        assert_eq!(row.node_id, 0);
        assert_eq!(row.trace_id, "");
        assert_eq!(row.span_id, "");
        assert_eq!(row.request_id, "");
        assert_eq!(row.status_code, 0);
        assert_eq!(row.http_method, "");
        assert_eq!(row.http_route, "");
        assert_eq!(row.duration_ms, 0.0);
        assert_eq!(row.attrs, "{}");
        assert_eq!(row.facet_attr_1, None);
        assert_eq!(row.facet_attr_2, None);
    }

    #[test]
    fn a_queryable_project_ships_only_the_allowlisted_content() {
        let policy = CloudLinePolicy {
            fidelity: CloudTelemetryFidelity::Queryable,
            allowlist: Arc::new(BTreeSet::from(["worker".to_string()])),
        };
        let (row, labels) = projected(&policy);

        // Labels and trace ids become real at this tier; `request_id` is
        // caller-controlled content and was not listed.
        assert_eq!(row.env, labels.env);
        assert_eq!(row.service, labels.service);
        assert_eq!(row.container_id, labels.container_id);
        assert_eq!(row.deploy_id, 42);
        assert_eq!(row.node_id, 3);
        assert_eq!(row.trace_id, "t-1");
        assert_eq!(row.request_id, "", "request_id is content; not listed");

        // Content is default-deny: only `worker` was listed.
        assert_eq!(row.attrs, r#"{"worker":"3"}"#);
        assert!(!row.attrs.contains("person@example.test"));
        assert_eq!(row.facet_attr_1, Some("3"));
        assert_eq!(
            row.facet_attr_2, None,
            "a promoted slot is still an attribute value"
        );
        assert_eq!(row.http_route, "", "not listed, so it does not leave");
        assert_eq!(row.http_method, "");
        assert_eq!(row.status_code, 0);
        assert_eq!(row.duration_ms, 0.0);
    }

    #[test]
    fn an_empty_allowlist_ships_no_content_even_at_queryable() {
        let (row, labels) = projected(&CloudLinePolicy {
            fidelity: CloudTelemetryFidelity::Queryable,
            allowlist: Arc::new(BTreeSet::new()),
        });
        assert_eq!(row.service, labels.service, "the line is still findable");
        assert_eq!(row.attrs, "{}");
        assert_eq!(row.facet_attr_1, None);
        assert_eq!(row.status_code, 0);
    }

    #[test]
    fn the_canonical_columns_a_queryable_project_listed_do_ship() {
        let (row, _) = projected(&CloudLinePolicy {
            fidelity: CloudTelemetryFidelity::Queryable,
            allowlist: Arc::new(BTreeSet::from([
                "http_route".to_string(),
                "status_code".to_string(),
                "duration_ms".to_string(),
                "http_method".to_string(),
                "request_id".to_string(),
            ])),
        });
        assert_eq!(row.http_route, "/orders/{id}");
        assert_eq!(row.http_method, "POST");
        assert_eq!(row.status_code, 503);
        assert_eq!(row.duration_ms, 12.5);
        assert_eq!(row.request_id, "r-1");
        // Canonical keys never double as residual attributes.
        assert_eq!(row.attrs, "{}");
    }

    #[tokio::test]
    async fn a_consent_lookup_failure_falls_back_to_metered() {
        // Fail closed: a database blip must not widen what leaves the
        // instance, and must not be cached as if it were an answer.
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_errors(vec![sea_orm::DbErr::Custom("connection reset".into())])
            .into_connection();
        let consent = CloudConsent::new(Arc::new(db));

        assert_eq!(consent.policy(7).await, CloudLinePolicy::metered());
        assert!(
            consent
                .entries
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty(),
            "an error must not pin the project to Metered for a whole TTL"
        );
    }

    #[tokio::test]
    async fn a_line_with_no_project_is_metered_without_a_lookup() {
        // External-service containers have no project row to consent with.
        let consent = CloudConsent::new(test_db());
        assert_eq!(consent.policy(0).await, CloudLinePolicy::metered());
        assert_eq!(consent.policy(-1).await, CloudLinePolicy::metered());
    }
}

// ── Analytics (ADR-047 §5) ──────────────────────────────────────────────

use super::analytics::{
    build_where, key_expr, key_name, metric_expr, AggregateRow, AggregateRowRaw, AttrPredicate,
    FacetRow, GroupKey, HistogramBucket, HistogramRow, KeyRow, LinePointer, LogAnalytics, Metric,
    Param, PointerRow, Sql,
};
use crate::store::{FacetField, FacetValue, LogQuery};
use std::collections::BTreeMap;

/// Bind `params` onto a query in order.
fn bind_all(mut q: clickhouse::query::Query, params: &[Param]) -> clickhouse::query::Query {
    for p in params {
        q = match p {
            Param::Str(s) => q.bind(s.as_str()),
            Param::I32(v) => q.bind(*v),
            Param::I64(v) => q.bind(*v),
            Param::F64(v) => q.bind(*v),
            Param::I32List(v) => q.bind(v.as_slice()),
            Param::StrList(v) => q.bind(v.as_slice()),
        };
    }
    q
}

/// Maps the pseudonyms a Cloud `GROUP BY` returns back to the ids the caller
/// asked about. Empty on a local target, and empty for an unrestricted
/// (`LogAccessScope::All`, no selection) Cloud query — see [`RefMap::id`].
#[derive(Default)]
struct RefMap {
    project: BTreeMap<String, String>,
    external_service: BTreeMap<String, String>,
}

impl RefMap {
    /// The id behind a group value, or the value unchanged.
    ///
    /// A ref is only reversible from a list of candidate ids, so a query that
    /// named none — an operator-wide `All` scope with no selection — gets the
    /// raw pseudonym back. That is a stable, opaque per-project key: it groups
    /// correctly and is never mistaken for an id, which an invented number
    /// would be.
    fn id(&self, key: &GroupKey, value: String) -> String {
        let table = match key {
            GroupKey::Label(FacetField::Project) => &self.project,
            GroupKey::Label(FacetField::ExternalService) => &self.external_service,
            _ => return value,
        };
        table.get(&value).cloned().unwrap_or(value)
    }
}

impl ClickHouseLineIndex {
    fn query_with(
        &self,
        sql: &str,
        params: &[Param],
    ) -> Result<clickhouse::query::Query, LogAggregatorError> {
        Ok(bind_all(self.read_client()?.query(sql), params))
    }

    /// Build the ref → id map for this query from the ids it was allowed to
    /// see (scope) and asked about (selection).
    fn ref_map(&self, query: &LogQuery) -> Result<RefMap, LogAggregatorError> {
        let mut map = RefMap::default();
        if !self.dialect.is_cloud() {
            return Ok(map);
        }
        let mut projects: Vec<i32> = Vec::new();
        let mut services: Vec<i32> = Vec::new();
        if let crate::store::LogAccessScope::Allowed {
            project_ids,
            external_service_ids,
        } = &query.scope
        {
            projects.extend(project_ids);
            services.extend(external_service_ids);
        }
        if let Some(sel) = &query.selection {
            projects.extend(&sel.project_ids);
            services.extend(&sel.external_service_ids);
        }
        for id in projects {
            map.project
                .insert(self.dialect.project_ref(id)?, id.to_string());
        }
        for id in services {
            map.external_service
                .insert(self.dialect.external_service_ref(id)?, id.to_string());
        }
        Ok(map)
    }

    /// Attribute keys on Cloud, where there is no `log_attr_keys` rollup.
    ///
    /// The keys are read out of the lines themselves. That is genuinely
    /// expensive — `JSONExtractKeys` parses every row's `attrs` text in the
    /// scoped window, where the local path reads a pre-aggregated row per
    /// project/day — so it runs over exactly the window the caller asked for
    /// and nothing wider, and Cloud's own server-side row and memory caps
    /// bound the rest. A materialized rollup on the Cloud side would remove
    /// this, and would be the first thing to add if the sidebar feels slow.
    async fn cloud_attribute_keys(
        &self,
        query: &LogQuery,
        limit: u32,
    ) -> Result<Vec<FacetValue>, LogAggregatorError> {
        let base = build_where(query, &[], &self.dialect)?;
        let sql = format!(
            "SELECT key, toUInt64(count()) AS lines FROM {table} \
             ARRAY JOIN JSONExtractKeys(attrs) AS key \
             WHERE {} GROUP BY key ORDER BY lines DESC LIMIT {}",
            base.where_clause(),
            limit.clamp(1, 1000),
            table = self.dialect.lines_table,
        );
        let rows: Vec<KeyRow> = self
            .fetch(
                "attribute key",
                self.query_with(&sql, &base.params)?.fetch_all(),
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| FacetValue {
                value: r.key,
                count: r.lines as i64,
            })
            .collect())
    }
}

#[async_trait]
impl LogAnalytics for ClickHouseLineIndex {
    async fn facets(
        &self,
        query: &LogQuery,
        attrs: &[AttrPredicate],
        keys: &[GroupKey],
        limit: u32,
    ) -> Result<BTreeMap<String, Vec<FacetValue>>, LogAggregatorError> {
        let base = build_where(query, attrs, &self.dialect)?;
        let refs = self.ref_map(query)?;
        let mut out = BTreeMap::new();
        for key in keys {
            let expr = key_expr(key, &self.dialect)?;
            let sql = format!(
                "SELECT {expr} AS value, toInt64(count()) AS count FROM {table} \
                 WHERE {} GROUP BY value ORDER BY count DESC, value ASC LIMIT {}",
                base.where_clause(),
                limit.clamp(1, 1000),
                table = self.dialect.lines_table,
            );
            let rows: Vec<FacetRow> = self
                .fetch("facet", self.query_with(&sql, &base.params)?.fetch_all())
                .await?;
            out.insert(
                key_name(key),
                rows.into_iter()
                    .map(|r| FacetValue {
                        value: refs.id(key, r.value),
                        count: r.count,
                    })
                    .collect(),
            );
        }
        Ok(out)
    }

    async fn attribute_keys(
        &self,
        query: &LogQuery,
        limit: u32,
    ) -> Result<Vec<FacetValue>, LogAggregatorError> {
        if self.dialect.is_cloud() {
            return self.cloud_attribute_keys(query, limit).await;
        }
        // `log_attr_keys` is per project/day; apply the scope on project_id
        // and the window on `day`. External-service lines carry
        // project_id 0 and are visible to callers allowed any service.
        let mut sql = Sql::default();
        let start = query.start_time.date_naive().to_string();
        let end = query.end_time.date_naive().to_string();
        sql.params.push(Param::Str(start));
        sql.conditions.push("day >= toDate(?)".into());
        sql.params.push(Param::Str(end));
        sql.conditions.push("day <= toDate(?)".into());
        if let crate::store::LogAccessScope::Allowed {
            project_ids,
            external_service_ids,
        } = &query.scope
        {
            let mut allowed = project_ids.clone();
            if !external_service_ids.is_empty() {
                allowed.push(0);
            }
            if allowed.is_empty() {
                return Ok(Vec::new());
            }
            sql.params.push(Param::I32List(allowed));
            sql.conditions.push("project_id IN ?".into());
        }
        if let Some(sel) = &query.selection {
            let mut allowed = sel.project_ids.clone();
            if !sel.external_service_ids.is_empty() {
                allowed.push(0);
            }
            if allowed.is_empty() {
                return Ok(Vec::new());
            }
            sql.params.push(Param::I32List(allowed));
            sql.conditions.push("project_id IN ?".into());
        }
        let stmt = format!(
            "SELECT key, countMerge(lines) AS lines FROM {LOCAL_ATTR_KEYS_TABLE} WHERE {} \
             GROUP BY key ORDER BY lines DESC LIMIT {}",
            sql.where_clause(),
            limit.clamp(1, 1000)
        );
        let rows: Vec<KeyRow> = self
            .fetch(
                "attribute key",
                self.query_with(&stmt, &sql.params)?.fetch_all(),
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| FacetValue {
                value: r.key,
                count: r.lines as i64,
            })
            .collect())
    }

    async fn histogram(
        &self,
        query: &LogQuery,
        attrs: &[AttrPredicate],
        bucket_secs: u32,
        group_by: Option<&GroupKey>,
        max_groups: u32,
    ) -> Result<Vec<HistogramBucket>, LogAggregatorError> {
        let base = build_where(query, attrs, &self.dialect)?;
        let refs = self.ref_map(query)?;
        let bucket = bucket_secs.clamp(1, 86_400 * 7);
        let group_expr = match group_by {
            Some(k) => key_expr(k, &self.dialect)?,
            None => "''".to_string(),
        };
        // Top-N groups by total count keep the series bounded; the rest
        // fold into "other".
        let sql = format!(
            "WITH top AS ( \
                 SELECT {group_expr} AS g FROM {table} WHERE {w} \
                 GROUP BY g ORDER BY count() DESC LIMIT {max_groups} \
             ) \
             SELECT toInt64(intDiv(toUnixTimestamp(ts), {bucket}) * {bucket}) AS bucket, \
                    if({group_expr} IN (SELECT g FROM top), {group_expr}, 'other') AS group, \
                    toInt64(count()) AS count \
             FROM {table} WHERE {w} \
             GROUP BY bucket, group ORDER BY bucket ASC, group ASC",
            w = base.where_clause(),
            max_groups = max_groups.clamp(1, 50),
            table = self.dialect.lines_table,
        );
        // The WHERE appears twice; bind the params twice in order.
        let mut params = base.params.clone();
        params.extend(base.params.iter().cloned());
        let rows: Vec<HistogramRow> = self
            .fetch("histogram", self.query_with(&sql, &params)?.fetch_all())
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| HistogramBucket {
                ts: DateTime::<Utc>::from_timestamp(r.bucket, 0).unwrap_or(query.start_time),
                group: group_by.map(|k| refs.id(k, r.group)),
                count: r.count,
            })
            .collect())
    }

    async fn aggregate(
        &self,
        query: &LogQuery,
        attrs: &[AttrPredicate],
        group_by: &[GroupKey],
        metric: &Metric,
        limit: u32,
    ) -> Result<Vec<AggregateRow>, LogAggregatorError> {
        let base = build_where(query, attrs, &self.dialect)?;
        let refs = self.ref_map(query)?;
        let key_exprs: Vec<String> = group_by
            .iter()
            .map(|k| key_expr(k, &self.dialect))
            .collect::<Result<_, _>>()?;
        let keys_array = if key_exprs.is_empty() {
            "[]::Array(String)".to_string()
        } else {
            format!("[{}]", key_exprs.join(", "))
        };
        let group_clause = if key_exprs.is_empty() {
            String::new()
        } else {
            format!("GROUP BY {}", key_exprs.join(", "))
        };
        let sql = format!(
            "SELECT {keys_array} AS keys, {metric} AS value, toInt64(count()) AS lines \
             FROM {table} WHERE {w} {group_clause} \
             ORDER BY value DESC LIMIT {limit}",
            metric = metric_expr(metric, &self.dialect)?,
            w = base.where_clause(),
            limit = limit.clamp(1, 1000),
            table = self.dialect.lines_table,
        );
        let rows: Vec<AggregateRowRaw> = self
            .fetch(
                "aggregate",
                self.query_with(&sql, &base.params)?.fetch_all(),
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| AggregateRow {
                // The group values come back in `group_by` order, so a
                // project/external-service key maps back positionally.
                keys: r
                    .keys
                    .into_iter()
                    .enumerate()
                    .map(|(i, value)| match group_by.get(i) {
                        Some(key) => refs.id(key, value),
                        None => value,
                    })
                    .collect(),
                value: if r.value.is_finite() { r.value } else { 0.0 },
                lines: r.lines,
            })
            .collect())
    }

    async fn matching_chunks(
        &self,
        query: &LogQuery,
        attrs: &[AttrPredicate],
        limit: u32,
    ) -> Result<Vec<i64>, LogAggregatorError> {
        let base = build_where(query, attrs, &self.dialect)?;
        let sql = format!(
            "SELECT chunk_seq FROM {table} WHERE {} \
             GROUP BY chunk_seq ORDER BY chunk_seq DESC LIMIT {}",
            base.where_clause(),
            limit.max(1),
            table = self.dialect.lines_table,
        );
        let rows: Vec<u64> = self
            .fetch(
                "matching chunk",
                self.query_with(&sql, &base.params)?.fetch_all(),
            )
            .await?;
        Ok(rows.into_iter().map(|s| s as i64).collect())
    }

    async fn search_pointers(
        &self,
        query: &LogQuery,
        attrs: &[AttrPredicate],
    ) -> Result<Vec<LinePointer>, LogAggregatorError> {
        let mut base = build_where(query, attrs, &self.dialect)?;
        if let Some(before) = &query.before {
            // Keyset: strictly older than the cursor line. The cursor's ts
            // is exact (nanoseconds) while the index is ms; compare on ms
            // and break ties on the pointer so nothing is skipped or
            // repeated across pages.
            let ms = before.timestamp.timestamp_millis();
            let (seq, idx) = before
                .chunk_position()
                .map(|(s, i)| (s, i as i64))
                .unwrap_or((i64::MAX, i64::MAX));
            base.params.push(Param::I64(ms));
            base.params.push(Param::I64(ms));
            base.params.push(Param::I64(seq));
            base.params.push(Param::I64(seq));
            base.params.push(Param::I64(idx));
            base.conditions.push(
                "(ts < fromUnixTimestamp64Milli(?) OR (ts = fromUnixTimestamp64Milli(?) \
                 AND (toInt64(chunk_seq) < ? OR (toInt64(chunk_seq) = ? AND toInt64(line_index) < ?))))"
                    .into(),
            );
        }
        let sql = format!(
            "SELECT chunk_seq, line_index, toInt64(toUnixTimestamp64Milli(ts)) AS ts_ms \
             FROM {table} WHERE {} \
             ORDER BY ts DESC, chunk_seq DESC, line_index DESC LIMIT {}",
            base.where_clause(),
            query.limit.clamp(1, 1000),
            table = self.dialect.lines_table,
        );
        let rows: Vec<PointerRow> = self
            .fetch("pointer", self.query_with(&sql, &base.params)?.fetch_all())
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| LinePointer {
                chunk_seq: r.chunk_seq as i64,
                line_index: r.line_index,
                ts: DateTime::<Utc>::from_timestamp_millis(r.ts_ms).unwrap_or(query.start_time),
            })
            .collect())
    }
}

#[cfg(test)]
mod live_tests {
    //! Against a real ClickHouse: `TEMPS_TEST_CLICKHOUSE_URL=http://localhost:18123
    //! TEMPS_TEST_CLICKHOUSE_USER=temps TEMPS_TEST_CLICKHOUSE_PASSWORD=temps
    //! cargo test -p temps-log-aggregator live_tests -- --ignored --nocapture`
    use super::*;
    use crate::index::analytics::{AttrOp, AttrPredicate, GroupKey, LogAnalytics, Metric};
    use crate::store::{FacetField, LogAccessScope, LogQuery};

    fn config() -> Option<ClickHouseConfig> {
        let url = std::env::var("TEMPS_TEST_CLICKHOUSE_URL").ok()?;
        Some(ClickHouseConfig::new(
            url,
            std::env::var("TEMPS_TEST_CLICKHOUSE_DATABASE").unwrap_or_else(|_| "temps".into()),
            std::env::var("TEMPS_TEST_CLICKHOUSE_USER").unwrap_or_else(|_| "temps".into()),
            std::env::var("TEMPS_TEST_CLICKHOUSE_PASSWORD").unwrap_or_else(|_| "temps".into()),
        ))
    }

    #[tokio::test]
    #[ignore = "needs a live ClickHouse with indexed data"]
    async fn analytics_queries_execute_against_live_index() {
        let Some(cfg) = config() else {
            eprintln!("TEMPS_TEST_CLICKHOUSE_URL unset; skipping");
            return;
        };
        let idx = ClickHouseLineIndex::connect(LineIndexTarget::Local(cfg))
            .await
            .expect("connect");
        let mut q = LogQuery::for_scope(LogAccessScope::All);
        q.start_time = Utc::now() - chrono::Duration::days(2);
        q.end_time = Utc::now();
        q.limit = 5;

        let facets = idx
            .facets(
                &q,
                &[],
                &[
                    GroupKey::Label(FacetField::Service),
                    GroupKey::Label(FacetField::Level),
                    GroupKey::Attr("cache".into()),
                    GroupKey::Attr("request_id".into()),
                ],
                5,
            )
            .await
            .expect("facets");
        eprintln!("facets: {facets:#?}");
        assert!(facets.contains_key("service"));

        let keys = idx.attribute_keys(&q, 20).await.expect("keys");
        eprintln!("keys: {keys:?}");

        let hist = idx
            .histogram(&q, &[], 3600, Some(&GroupKey::Label(FacetField::Level)), 5)
            .await
            .expect("histogram");
        eprintln!("histogram buckets: {}", hist.len());

        let agg = idx
            .aggregate(
                &q,
                &[AttrPredicate {
                    key: "cache".into(),
                    op: AttrOp::Eq,
                    value: Some("miss".into()),
                }],
                &[
                    GroupKey::Label(FacetField::Service),
                    GroupKey::Attr("worker".into()),
                ],
                &Metric::Count,
                5,
            )
            .await
            .expect("aggregate");
        eprintln!("aggregate: {agg:?}");

        let p95 = idx
            .aggregate(
                &q,
                &[],
                &[GroupKey::Label(FacetField::Service)],
                &Metric::P95("worker".into()),
                3,
            )
            .await
            .expect("p95 over a numeric-looking dynamic attribute");
        eprintln!("p95(worker) by service: {p95:?}");
        assert!(p95.iter().all(|r| r.value.is_finite()));

        let ptrs = idx
            .search_pointers(
                &q,
                &[AttrPredicate {
                    key: "request_id".into(),
                    op: AttrOp::Prefix,
                    value: Some("7-3133".into()),
                }],
            )
            .await
            .expect("pointers");
        eprintln!("pointers: {ptrs:?}");

        // Scoped query never leaks: an empty allow-list yields nothing.
        let mut denied = q.clone();
        denied.scope = LogAccessScope::Allowed {
            project_ids: vec![],
            external_service_ids: vec![],
        };
        let none = idx.search_pointers(&denied, &[]).await.expect("denied");
        assert!(none.is_empty());
    }
}
