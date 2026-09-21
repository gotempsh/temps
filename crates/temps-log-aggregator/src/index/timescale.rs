// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! TimescaleDB (Postgres) implementation of the per-line index (ADR-047).
//!
//! This is the index an operator gets *without* running ClickHouse. The
//! table (`log_lines_index`, created by
//! `temps-migrations::m20260921_000001_log_lines_index`) mirrors the
//! ClickHouse one column for column, and every method here mirrors
//! [`super::clickhouse::ClickHouseLineIndex`]'s semantics — same scope
//! predicate, same attribute value semantics, same facet labels, same
//! keyset — so switching backends changes the cost of a query, never its
//! answer.
//!
//! # Why not reuse `index::analytics`'s SQL builder
//!
//! [`super::analytics`] generates ClickHouse SQL (`toString()`,
//! `fromUnixTimestamp64Milli()`, `attrs.\`key\``, `?` placeholders). The
//! builder below generates Postgres SQL with `$n` parameters against a
//! `jsonb` column. Only the *types* (`AttrPredicate`, `GroupKey`,
//! `Metric`, …) and [`super::analytics::validate_key`] are shared, which is
//! exactly the part that must not drift between backends.
//!
//! # Where the two backends genuinely differ
//!
//! * **Timestamps.** The ClickHouse index rounds to milliseconds to save
//!   3 B/line; Postgres `timestamptz` is 8 bytes either way, so rows keep
//!   full microsecond precision and the keyset comparison uses the exact
//!   timestamp.
//! * **Attribute keys.** ClickHouse has a materialized `log_attr_keys`
//!   rollup; here [`TimescaleLineIndex::attribute_keys`] unrolls `attrs`
//!   with `jsonb_object_keys` over the queried window. That scales with
//!   *lines in the window*, not with distinct keys.
//! * **Deletes.** `forget_chunks` is a plain `DELETE ... WHERE chunk_seq =
//!   ANY($1)`. `chunk_seq` is a compression `segmentby` column, so
//!   TimescaleDB (>= 2.14) drops whole compressed batches instead of
//!   decompressing them.
//! * **Deduplication.** ClickHouse collapses re-inserted lines on merge
//!   (`ReplacingMergeTree`); here the same guarantee is a unique index on
//!   `(chunk_seq, line_index, ts)` plus `ON CONFLICT DO NOTHING`, and a
//!   chunk's batches are one transaction so a partial failure leaves
//!   nothing behind. Both are load-bearing: the seal pipeline re-indexes a
//!   chunk whenever `mark_indexed` fails after a successful insert, a
//!   batch fails mid-chunk, the backend flaps, or the compactor
//!   re-indexes, and a duplicated line inflates every facet, histogram and
//!   aggregate permanently.
//!
//! # Bounds this backend imposes that ClickHouse does not
//!
//! Analytics here run on the **shared control-plane connection pool**, so
//! an unbounded query is not just slow, it starves every other request on
//! the instance. Two guards, both TimescaleDB-only:
//!
//! * Every read runs in a transaction under `SET LOCAL statement_timeout`
//!   of [`READ_TIMEOUT_MS`], matching the chunk scan's time budget. A query
//!   that cannot finish in that window returns an error the operator can
//!   act on rather than pinning a connection.
//! * The queried window is clamped to at most [`MAX_WINDOW_DAYS`] days
//!   ([`clamped_start`]): a request with a 1970 `start_time` reads the last
//!   90 days, not the whole hypertable. Configure ClickHouse for wider
//!   windows — it is the backend built for unbounded historical scans.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, QueryResult, Statement, TransactionTrait,
    Value,
};
use tracing::{info, warn};

use super::analytics::{
    validate_key, AggregateRow, AttrOp, AttrPredicate, GroupKey, HistogramBucket, LinePointer,
    LogAnalytics, Metric,
};
use super::{IndexOutcome, LineIndexBackend, LineIndexSink};
use crate::chunk::{level_to_u8, ChunkLabels};
use crate::error::LogAggregatorError;
use crate::parser::well_known;
use crate::store::{FacetField, FacetValue, LogAccessScope, LogQuery, LogSelection, LogSourceKind};
use crate::types::{LogLevel, LogLine, LogStream};

/// Number of operator-promotable facet slots (matches the schema and the
/// ClickHouse backend).
pub const FACET_SLOTS: usize = 20;

/// Key → slot mapping for promoted facets; `slots[n]` is the attribute key
/// written into `facet_attr_{n+1}`, `None` = free slot.
pub type FacetSlots = [Option<String>; FACET_SLOTS];

/// Rows per `INSERT`. 28 columns × 1000 rows = 28 000 bound parameters,
/// comfortably under Postgres' 65 535-parameter ceiling while still
/// amortising the round trip over a whole chunk in one or two statements.
const INSERT_BATCH_ROWS: usize = 1_000;

/// Ceiling on how long *any* analytics read may run before Postgres cancels
/// it. These queries share the control-plane pool with every other request
/// on the instance, so an unbounded one is a availability problem, not just
/// a slow response. 10 s matches the chunk scan's own time budget.
pub const READ_TIMEOUT_MS: u32 = 10_000;

/// Widest window this backend will scan, regardless of what the caller
/// asked for. A request with a `start_time` of 1970 reads the last 90 days
/// instead of the entire hypertable. ClickHouse has no such bound — point
/// operators who need wider historical windows at it.
pub const MAX_WINDOW_DAYS: i64 = 90;

/// Column list of `log_lines_index`, in the order [`row_placeholders`] binds
/// them. Kept as one constant so the INSERT and the binder cannot drift.
const INSERT_COLUMNS: &str = "project_id, external_service_id, env, service, deploy_id, \
                              container_id, node_id, ts, level, stream, chunk_seq, line_index, \
                              trace_id, span_id, request_id, status_code, http_method, \
                              http_route, duration_ms, attrs, \
                              facet_attr_1, facet_attr_2, facet_attr_3, facet_attr_4, \
                              facet_attr_5, facet_attr_6, facet_attr_7, facet_attr_8, \
                              facet_attr_9, facet_attr_10, facet_attr_11, facet_attr_12, \
                              facet_attr_13, facet_attr_14, facet_attr_15, facet_attr_16, \
                              facet_attr_17, facet_attr_18, facet_attr_19, facet_attr_20";

/// Number of columns in [`INSERT_COLUMNS`].
const INSERT_COLUMN_COUNT: usize = 20 + FACET_SLOTS;

/// The TimescaleDB-backed line index.
pub struct TimescaleLineIndex {
    db: Arc<DatabaseConnection>,
    slots: arc_swap::ArcSwap<FacetSlots>,
    /// Last retention window applied to the hypertable policy; `0` = never.
    /// Mirrors the ClickHouse sink's `ttl_days` cache so the retention tick
    /// is a no-op once the setting has stopped changing.
    retention_days: AtomicU32,
}

impl TimescaleLineIndex {
    /// Wrap an existing control-plane connection. The table is created by
    /// the normal migration run, so there is nothing to probe here — if the
    /// migrations ran, the index is usable.
    pub fn new(db: Arc<DatabaseConnection>) -> Arc<Self> {
        Arc::new(Self {
            db,
            slots: arc_swap::ArcSwap::from_pointee(Default::default()),
            retention_days: AtomicU32::new(0),
        })
    }

    /// Replace the promoted-facet mapping (called by the facet service when
    /// a key is promoted or removed).
    pub fn set_facet_slots(&self, slots: FacetSlots) {
        self.slots.store(Arc::new(slots));
    }

    fn stmt(sql: String, values: Vec<Value>) -> Statement {
        Statement::from_sql_and_values(DatabaseBackend::Postgres, sql, values)
    }

    /// Run one analytics statement under a statement timeout.
    ///
    /// Every read in this file goes through here, not just the expensive
    /// ones: `SET LOCAL` only survives inside a transaction, so the
    /// transaction is the timeout. Without it a handful of concurrent
    /// wide-window requests can hold every connection in the shared
    /// control-plane pool until they finish, which takes the whole instance
    /// down, not just Global Logs.
    async fn query_all(
        &self,
        sql: String,
        values: Vec<Value>,
    ) -> Result<Vec<QueryResult>, LogAggregatorError> {
        let txn = self.db.begin().await.map_err(db_err)?;
        txn.execute(Self::stmt(
            format!("SET LOCAL statement_timeout = {READ_TIMEOUT_MS}"),
            vec![],
        ))
        .await
        .map_err(db_err)?;
        let rows = txn
            .query_all(Self::stmt(sql, values))
            .await
            .map_err(db_err)?;
        txn.commit().await.map_err(db_err)?;
        Ok(rows)
    }

    /// Is the `timescaledb` extension installed? The migration degrades to a
    /// plain table without it, so retention has to degrade the same way.
    async fn timescale_enabled(&self) -> Result<bool, LogAggregatorError> {
        let row = self
            .db
            .query_one(Self::stmt(
                "SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'timescaledb') AS ok"
                    .into(),
                vec![],
            ))
            .await
            .map_err(db_err)?;
        Ok(match row {
            Some(row) => row.try_get::<bool>("", "ok")?,
            None => false,
        })
    }

    /// Insert `lines` (chunk order, `line_index` starting at `first_index`)
    /// as rows for chunk `seq`, in batches of [`INSERT_BATCH_ROWS`].
    ///
    /// Takes the connection rather than using `self.db` so a whole chunk's
    /// batches can share one transaction (see [`Self::index_chunk`]).
    ///
    /// `ON CONFLICT DO NOTHING` on `(chunk_seq, line_index, ts)` makes a
    /// re-index a no-op instead of a silent double-count — this is the
    /// TimescaleDB equivalent of ClickHouse's `ReplacingMergeTree`, and the
    /// reindexer's idempotency guarantee rests on it.
    pub async fn insert_lines<C: ConnectionTrait>(
        &self,
        conn: &C,
        seq: i64,
        labels: &ChunkLabels,
        first_index: u32,
        lines: &[LogLine],
    ) -> Result<(), LogAggregatorError> {
        let slots = self.slots.load();
        for (batch_no, batch) in lines.chunks(INSERT_BATCH_ROWS).enumerate() {
            let base_index = first_index + (batch_no * INSERT_BATCH_ROWS) as u32;
            let mut values: Vec<Value> = Vec::with_capacity(batch.len() * INSERT_COLUMN_COUNT);
            let mut tuples: Vec<String> = Vec::with_capacity(batch.len());
            for (offset, line) in batch.iter().enumerate() {
                let line_index = base_index + offset as u32;
                push_row(&mut values, seq, labels, line_index, line, &slots);
                tuples.push(row_placeholders(values.len()));
            }
            let sql = format!(
                "INSERT INTO log_lines_index ({INSERT_COLUMNS}) VALUES {} \
                 ON CONFLICT (chunk_seq, line_index, ts) DO NOTHING",
                tuples.join(", ")
            );
            conn.execute(Self::stmt(sql, values)).await?;
        }
        Ok(())
    }
}

/// Bind one row's 40 values in [`INSERT_COLUMNS`] order.
fn push_row(
    values: &mut Vec<Value>,
    seq: i64,
    labels: &ChunkLabels,
    line_index: u32,
    line: &LogLine,
    slots: &FacetSlots,
) {
    let fields = line.fields.as_ref();
    let wk = fields.map(well_known).unwrap_or_default();
    let attrs = fields.map(residual_attrs).unwrap_or_else(default_attrs);
    let slot_values = slot_values(fields, slots);

    values.push(labels.project_id.into());
    values.push(labels.external_service_id.unwrap_or(0).into());
    values.push(labels.env.clone().into());
    values.push(labels.service.clone().into());
    values.push(labels.deploy_id.unwrap_or(0).into());
    values.push(labels.container_id.clone().into());
    values.push(labels.node_id.unwrap_or(0).into());
    values.push(line.ts.into());
    values.push(i16::from(level_to_u8(line.level)).into());
    values.push(
        match line.stream {
            LogStream::Stdout => 0i16,
            LogStream::Stderr => 1i16,
        }
        .into(),
    );
    values.push(seq.into());
    values.push((line_index as i32).into());
    values.push(wk.trace_id.unwrap_or("").to_string().into());
    values.push(wk.span_id.unwrap_or("").to_string().into());
    values.push(wk.request_id.unwrap_or("").to_string().into());
    // INT2 column: a status above 32 767 is not an HTTP status, and
    // saturating beats failing the whole chunk's insert over one bad line.
    values.push((wk.status_code.unwrap_or(0).min(i16::MAX as u16) as i16).into());
    values.push(wk.http_method.unwrap_or("").to_string().into());
    values.push(wk.http_route.unwrap_or("").to_string().into());
    values.push(wk.duration_ms.unwrap_or(0.0).into());
    values.push(attrs.into());
    for slot in slot_values {
        values.push(Value::String(slot.map(|s| Box::new(s.to_string()))));
    }
}

/// `($n, $n+1, …)` for one row, given the parameter count *after* the row's
/// values were pushed.
fn row_placeholders(end: usize) -> String {
    let start = end - INSERT_COLUMN_COUNT;
    let mut out = String::from("(");
    for i in 0..INSERT_COLUMN_COUNT {
        if i > 0 {
            out.push_str(", ");
        }
        out.push('$');
        out.push_str(&(start + i + 1).to_string());
    }
    out.push(')');
    out
}

fn default_attrs() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

/// `fields` without the canonical keys that already live in fixed columns,
/// so nothing is stored twice and the facet sidebar lists each attribute
/// once. A private copy of the ClickHouse sink's helper (it returns a
/// serialised string there; `jsonb` wants the value).
fn residual_attrs(fields: &serde_json::Value) -> serde_json::Value {
    let Some(obj) = fields.as_object() else {
        return default_attrs();
    };
    serde_json::Value::Object(
        obj.iter()
            .filter(|(k, _)| !crate::parser::is_canonical_key(k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    )
}

/// Values for the promoted slots, read from `fields` by key. Only string
/// values are promoted, matching the ClickHouse sink.
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

// ── SQL generation (pure, unit-tested) ──────────────────────────────────

/// Accumulates positional parameters so no caller value is ever
/// interpolated into SQL text.
#[derive(Debug, Default)]
pub(crate) struct Binder {
    values: Vec<Value>,
}

impl Binder {
    fn bind(&mut self, value: impl Into<Value>) -> String {
        self.values.push(value.into());
        format!("${}", self.values.len())
    }

    fn into_values(self) -> Vec<Value> {
        self.values
    }
}

/// A `WHERE` clause and the parameters it binds, in order.
#[derive(Debug, Default)]
pub(crate) struct Where {
    conditions: Vec<String>,
    binder: Binder,
}

impl Where {
    fn clause(&self) -> String {
        if self.conditions.is_empty() {
            "TRUE".to_string()
        } else {
            self.conditions.join(" AND ")
        }
    }

    fn values(self) -> Vec<Value> {
        self.binder.into_values()
    }
}

fn level_to_i16(level: LogLevel) -> i16 {
    i16::from(level_to_u8(level))
}

/// `CASE` mapping `level` back to the name ClickHouse's `toString(level)`
/// returns for the same Enum8, so both backends label the facet identically.
const LEVEL_NAME_EXPR: &str = "(CASE level WHEN 0 THEN 'trace' WHEN 1 THEN 'debug' \
                               WHEN 2 THEN 'info' WHEN 3 THEN 'warn' WHEN 4 THEN 'error' \
                               ELSE 'unknown' END)";

const STREAM_NAME_EXPR: &str = "(CASE stream WHEN 1 THEN 'stderr' ELSE 'stdout' END)";

/// Numeric-literal guard for a `jsonb` string value before casting it to
/// `float8`. Mirrors ClickHouse's `toFloat64OrNull`: a non-numeric value is
/// `NULL`, never an error.
const NUMERIC_RE: &str = r"^[+-]?([0-9]+(\.[0-9]*)?|\.[0-9]+)([eE][+-]?[0-9]+)?$";

/// SQL expression (as `text`) for a group/facet key.
pub(crate) fn key_expr(key: &GroupKey) -> Result<String, LogAggregatorError> {
    Ok(match key {
        GroupKey::Label(f) => match f {
            FacetField::Env => "env".into(),
            FacetField::Service => "service".into(),
            FacetField::Level => LEVEL_NAME_EXPR.into(),
            FacetField::Stream => STREAM_NAME_EXPR.into(),
            FacetField::Project => "project_id::text".into(),
            FacetField::ExternalService => "external_service_id::text".into(),
            FacetField::Node => "node_id::text".into(),
            FacetField::Deploy => "deploy_id::text".into(),
            FacetField::Container => "container_id".into(),
        },
        GroupKey::Attr(k) => attr_string_expr(k)?,
    })
}

/// Display name of a group key in responses (identical to the ClickHouse
/// backend's).
pub(crate) fn key_name(key: &GroupKey) -> String {
    match key {
        GroupKey::Label(f) => f.as_str().to_string(),
        GroupKey::Attr(k) => k.clone(),
    }
}

/// The column or `jsonb` path holding an attribute. Canonical keys live in
/// fixed columns; everything else is a `jsonb` lookup. The key is validated
/// before it is quoted, and it is quoted as a *literal* (single quotes,
/// doubled internally) because `attrs->>'k'` takes a value, not an
/// identifier.
fn attr_json_path(key: &str) -> Result<String, LogAggregatorError> {
    validate_key(key)?;
    // `validate_key` already rejects quotes; the escape is belt-and-braces
    // so a future relaxation of the grammar cannot become an injection.
    Ok(format!("'{}'", key.replace('\'', "''")))
}

/// Attribute as `text`. Absent dynamic keys read as `''`, matching
/// ClickHouse's `toString(ifNull(…, ''))`.
fn attr_string_expr(key: &str) -> Result<String, LogAggregatorError> {
    validate_key(key)?;
    Ok(if crate::parser::is_canonical_key(key) {
        format!("{key}::text")
    } else {
        format!("coalesce(attrs->>{}, '')", attr_json_path(key)?)
    })
}

/// Attribute as `float8`, `NULL` when the value is not numeric.
fn attr_number_expr(key: &str) -> Result<String, LogAggregatorError> {
    validate_key(key)?;
    if crate::parser::is_canonical_key(key) {
        return Ok(match key {
            // Already numeric columns (int2 / real): cast directly.
            "status_code" | "duration_ms" => format!("{key}::float8"),
            // The other canonical columns are `text`. A bare `::float8` on
            // them errors the *whole query* the moment one row holds a
            // non-numeric value — which is every row, for `trace_id`. So
            // `p95:trace_id` must yield NULL per row (and therefore 0), the
            // same as ClickHouse's `toFloat64OrNull`, not a 500.
            _ => format!("(CASE WHEN {key} ~ '{NUMERIC_RE}' THEN {key}::float8 ELSE NULL END)"),
        });
    }
    let path = attr_json_path(key)?;
    // A JSON number casts directly; a JSON string only when it looks like a
    // number (the regex is what stops `'abc'::float8` from erroring out the
    // whole query, which is the difference between this and a bare cast).
    Ok(format!(
        "(CASE WHEN jsonb_typeof(attrs->{path}) = 'number' THEN (attrs->>{path})::float8 \
          WHEN attrs->>{path} ~ '{NUMERIC_RE}' THEN (attrs->>{path})::float8 ELSE NULL END)"
    ))
}

/// Existence test. A JSON `null` counts as absent, matching both the
/// ClickHouse subcolumn (`IS NOT NULL`) and [`AttrPredicate::matches`].
fn attr_exists_expr(key: &str) -> Result<String, LogAggregatorError> {
    validate_key(key)?;
    if crate::parser::is_canonical_key(key) {
        return Ok(match key {
            "status_code" | "duration_ms" => format!("{key} <> 0"),
            _ => format!("{key} <> ''"),
        });
    }
    let path = attr_json_path(key)?;
    // `jsonb_exists(attrs, k)` rather than the `?` operator: `?` inside a
    // statement that also carries `$n` parameters is needlessly confusing to
    // read and to any future driver that rewrites placeholders.
    Ok(format!(
        "(jsonb_exists(attrs, {path}) AND jsonb_typeof(attrs->{path}) <> 'null')"
    ))
}

/// Scope / selection fragment. Mirrors `manifest::scope_condition`, with the
/// index's `external_service_id = 0` sentinel in place of the manifest's
/// `NULL`. Both lists empty is `FALSE` — fail closed, visibly.
fn resource_condition(binder: &mut Binder, project_ids: &[i32], service_ids: &[i32]) -> String {
    if project_ids.is_empty() && service_ids.is_empty() {
        return "FALSE".to_string();
    }
    let p = binder.bind(project_ids.to_vec());
    let s = binder.bind(service_ids.to_vec());
    format!(
        "((external_service_id = 0 AND project_id = ANY({p}::int4[])) \
          OR (external_service_id <> 0 AND external_service_id = ANY({s}::int4[])))"
    )
}

/// `query.start_time`, never more than [`MAX_WINDOW_DAYS`] before
/// `query.end_time`.
///
/// A TimescaleDB-only bound. Analytics here run on the shared control-plane
/// pool, so "since 1970" is a request to scan the entire hypertable while
/// everything else on the instance waits for a connection. Narrowing beats
/// erroring: the caller still gets an answer, for the most recent 90 days.
/// Operators who need wider historical windows should configure ClickHouse,
/// which is built for exactly that.
pub(crate) fn clamped_start(query: &LogQuery) -> DateTime<Utc> {
    let floor = query.end_time - chrono::Duration::days(MAX_WINDOW_DAYS);
    query.start_time.max(floor)
}

/// `WHERE` for a [`LogQuery`] plus attribute predicates. Never omits the
/// scope.
pub(crate) fn build_where(
    query: &LogQuery,
    attrs: &[AttrPredicate],
) -> Result<Where, LogAggregatorError> {
    let mut w = Where::default();

    let start = w.binder.bind(clamped_start(query));
    w.conditions.push(format!("ts >= {start}"));
    let end = w.binder.bind(query.end_time);
    w.conditions.push(format!("ts <= {end}"));

    match &query.scope {
        LogAccessScope::All => {}
        LogAccessScope::Allowed {
            project_ids,
            external_service_ids,
        } => {
            let c = resource_condition(&mut w.binder, project_ids, external_service_ids);
            w.conditions.push(c);
        }
    }
    match query.source {
        LogSourceKind::Application => w.conditions.push("external_service_id = 0".into()),
        LogSourceKind::Service => w.conditions.push("external_service_id <> 0".into()),
        LogSourceKind::Collected => {}
    }
    if let Some(LogSelection {
        project_ids,
        external_service_ids,
    }) = &query.selection
    {
        let c = resource_condition(&mut w.binder, project_ids, external_service_ids);
        w.conditions.push(c);
    }
    if !query.levels.is_empty() {
        let levels: Vec<i16> = query.levels.iter().map(|l| level_to_i16(*l)).collect();
        let v = w.binder.bind(levels);
        w.conditions.push(format!("level = ANY({v}::int2[])"));
    }
    if !query.envs.is_empty() {
        let v = w.binder.bind(query.envs.clone());
        w.conditions.push(format!("env = ANY({v}::text[])"));
    }
    if !query.services.is_empty() {
        let v = w.binder.bind(query.services.clone());
        w.conditions.push(format!("service = ANY({v}::text[])"));
    }
    if !query.container_ids.is_empty() {
        let v = w.binder.bind(query.container_ids.clone());
        w.conditions
            .push(format!("container_id = ANY({v}::text[])"));
    }
    if !query.node_ids.is_empty() {
        let v = w.binder.bind(query.node_ids.clone());
        w.conditions.push(format!("node_id = ANY({v}::int4[])"));
    }
    if let Some(d) = query.deploy_id {
        let v = w.binder.bind(d);
        w.conditions.push(format!("deploy_id = {v}"));
    }

    for p in attrs {
        let cond = match p.op {
            AttrOp::Exists => attr_exists_expr(&p.key)?,
            AttrOp::Eq | AttrOp::Neq | AttrOp::Prefix => {
                let value = p
                    .value
                    .clone()
                    .ok_or_else(|| LogAggregatorError::Validation {
                        message: format!("attribute predicate on {:?} needs a value", p.key),
                    })?;
                // `=` compares the raw extraction: an absent key is NULL,
                // which is simply not equal. `!=` uses the coalesced form so
                // lines without the key count as "not that value" — exactly
                // the ClickHouse split, and what `AttrPredicate::matches`
                // does in memory.
                let raw = if crate::parser::is_canonical_key(&p.key) {
                    attr_string_expr(&p.key)?
                } else {
                    format!("attrs->>{}", attr_json_path(&p.key)?)
                };
                let v = w.binder.bind(value);
                match p.op {
                    AttrOp::Eq => format!("{raw} = {v}"),
                    AttrOp::Neq => format!("{} <> {v}", attr_string_expr(&p.key)?),
                    _ => format!("starts_with({}, {v})", attr_string_expr(&p.key)?),
                }
            }
            AttrOp::Gt | AttrOp::Lt => {
                let value: f64 =
                    p.value
                        .as_deref()
                        .and_then(|v| v.parse().ok())
                        .ok_or_else(|| LogAggregatorError::Validation {
                            message: format!(
                                "attribute predicate on {:?} needs a numeric value",
                                p.key
                            ),
                        })?;
                let expr = attr_number_expr(&p.key)?;
                let v = w.binder.bind(value);
                if p.op == AttrOp::Gt {
                    format!("{expr} > {v}")
                } else {
                    format!("{expr} < {v}")
                }
            }
        };
        w.conditions.push(cond);
    }

    Ok(w)
}

/// Aggregate expression for a metric, as `float8`. `coalesce(…, 0)` on every
/// one so a group with no numeric value reports `0` rather than `NULL`
/// leaking into the response.
pub(crate) fn metric_expr(metric: &Metric) -> Result<String, LogAggregatorError> {
    Ok(match metric {
        Metric::Count => "count(*)::float8".into(),
        Metric::CountDistinct(k) => format!("count(DISTINCT {})::float8", attr_string_expr(k)?),
        Metric::Avg(k) => format!("coalesce(avg({}), 0)::float8", attr_number_expr(k)?),
        Metric::P50(k) => percentile(0.5, k)?,
        Metric::P95(k) => percentile(0.95, k)?,
        Metric::P99(k) => percentile(0.99, k)?,
        Metric::Max(k) => format!("coalesce(max({}), 0)::float8", attr_number_expr(k)?),
        Metric::Sum(k) => format!("coalesce(sum({}), 0)::float8", attr_number_expr(k)?),
    })
}

fn percentile(q: f64, key: &str) -> Result<String, LogAggregatorError> {
    Ok(format!(
        "coalesce(percentile_cont({q}) WITHIN GROUP (ORDER BY {}), 0)::float8",
        attr_number_expr(key)?
    ))
}

fn db_err(e: sea_orm::DbErr) -> LogAggregatorError {
    LogAggregatorError::Database(e)
}

// ── Writer side ─────────────────────────────────────────────────────────

#[async_trait]
impl LineIndexSink for TimescaleLineIndex {
    async fn index_chunk(
        &self,
        seq: i64,
        labels: &ChunkLabels,
        segments: &[Arc<Vec<LogLine>>],
    ) -> Result<IndexOutcome, LogAggregatorError> {
        // One transaction for the whole chunk: a failure partway through a
        // multi-batch chunk must leave *nothing* behind, or the retry has to
        // reason about which lines already landed. Combined with
        // `ON CONFLICT DO NOTHING` this makes `index_chunk` idempotent under
        // any interleaving of failure and retry.
        let txn = self.db.begin().await.map_err(db_err)?;
        let mut first_index = 0u32;
        for segment in segments {
            if let Err(e) = self
                .insert_lines(&txn, seq, labels, first_index, segment)
                .await
            {
                warn!(seq, error = %e, "line index insert failed");
                // Dropping `txn` rolls back; be explicit about it.
                let _ = txn.rollback().await;
                return Err(e);
            }
            first_index += segment.len() as u32;
        }
        txn.commit().await.map_err(db_err)?;
        Ok(IndexOutcome::Indexed)
    }

    async fn forget_chunks(&self, seqs: &[i64]) -> Result<(), LogAggregatorError> {
        if seqs.is_empty() {
            return Ok(());
        }
        // `chunk_seq` is a compression segmentby column, so on TimescaleDB
        // >= 2.14 this drops whole compressed batches rather than
        // decompressing the chunk to delete rows out of it.
        self.db
            .execute(TimescaleLineIndex::stmt(
                "DELETE FROM log_lines_index WHERE chunk_seq = ANY($1::int8[])".into(),
                vec![seqs.to_vec().into()],
            ))
            .await
            .map_err(db_err)?;
        Ok(())
    }

    async fn set_retention_days(&self, days: u32) -> Result<(), LogAggregatorError> {
        let days = days.clamp(1, 3650);
        if self.retention_days.load(Ordering::Relaxed) == days {
            return Ok(());
        }
        // `days` is a clamped integer we produced, never caller text, so it
        // is safe inside the INTERVAL literal — Postgres has no parameter
        // form for an interval unit anyway.
        //
        // Replacing rather than altering the policy is what makes this
        // idempotent: `add_retention_policy` on a table that already has one
        // errors even with `if_not_exists`, because the *interval* differs.
        // The `drop_chunks` applies the new window immediately instead of
        // waiting for the policy's first scheduled run.
        //
        // Each statement is issued separately rather than wrapped in one
        // `DO` block: a `DO` block is a transaction, and `drop_chunks`
        // wants to be its own.
        let statements: Vec<String> = if self.timescale_enabled().await? {
            vec![
                "SELECT remove_retention_policy('log_lines_index', if_exists => TRUE)".into(),
                format!(
                    "SELECT add_retention_policy('log_lines_index', INTERVAL '{days} days', \
                     if_not_exists => TRUE)"
                ),
                format!(
                    "SELECT drop_chunks('log_lines_index', older_than => INTERVAL '{days} days')"
                ),
            ]
        } else {
            // Plain PostgreSQL: no chunks to drop, so the window is applied
            // by deleting the rows outright.
            vec![format!(
                "DELETE FROM log_lines_index WHERE ts < now() - INTERVAL '{days} days'"
            )]
        };
        for sql in statements {
            self.db
                .execute(TimescaleLineIndex::stmt(sql, vec![]))
                .await
                .map_err(db_err)?;
        }
        self.retention_days.store(days, Ordering::Relaxed);
        info!(
            days,
            "line index retention aligned with container log retention (TimescaleDB)"
        );
        Ok(())
    }

    fn unavailable_reason(&self) -> Option<String> {
        None
    }

    fn backend(&self) -> Option<LineIndexBackend> {
        Some(LineIndexBackend::TimescaleDb)
    }
}

// ── Read side (ADR-047 §5) ──────────────────────────────────────────────

#[async_trait]
impl LogAnalytics for TimescaleLineIndex {
    async fn facets(
        &self,
        query: &LogQuery,
        attrs: &[AttrPredicate],
        keys: &[GroupKey],
        limit: u32,
    ) -> Result<BTreeMap<String, Vec<FacetValue>>, LogAggregatorError> {
        let mut out = BTreeMap::new();
        for key in keys {
            let expr = key_expr(key)?;
            let w = build_where(query, attrs)?;
            let sql = format!(
                "SELECT {expr} AS value, count(*)::int8 AS count FROM log_lines_index \
                 WHERE {} GROUP BY 1 ORDER BY 2 DESC, 1 ASC LIMIT {}",
                w.clause(),
                limit.clamp(1, 1000)
            );
            let rows = self.query_all(sql, w.values()).await?;
            let mut values = Vec::with_capacity(rows.len());
            for row in rows {
                values.push(FacetValue {
                    value: row
                        .try_get::<Option<String>>("", "value")?
                        .unwrap_or_default(),
                    count: row.try_get::<i64>("", "count")?,
                });
            }
            out.insert(key_name(key), values);
        }
        Ok(out)
    }

    async fn attribute_keys(
        &self,
        query: &LogQuery,
        limit: u32,
    ) -> Result<Vec<FacetValue>, LogAggregatorError> {
        // No materialized key rollup here (the ClickHouse backend has
        // `log_attr_keys`); the keys are unrolled from `attrs` over the
        // queried window, so this costs one pass over the window's lines.
        // The clamped window plus `query_all`'s statement timeout are what
        // keep that honest.
        let w = build_where(query, &[])?;
        let sql = format!(
            "SELECT k.key AS key, count(*)::int8 AS lines \
             FROM log_lines_index t, LATERAL jsonb_object_keys(t.attrs) AS k(key) \
             WHERE {} GROUP BY 1 ORDER BY 2 DESC, 1 ASC LIMIT {}",
            w.clause(),
            limit.clamp(1, 1000)
        );
        let rows = self.query_all(sql, w.values()).await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(FacetValue {
                value: row.try_get::<String>("", "key")?,
                count: row.try_get::<i64>("", "lines")?,
            });
        }
        Ok(out)
    }

    async fn histogram(
        &self,
        query: &LogQuery,
        attrs: &[AttrPredicate],
        bucket_secs: u32,
        group_by: Option<&GroupKey>,
        max_groups: u32,
    ) -> Result<Vec<HistogramBucket>, LogAggregatorError> {
        let w = build_where(query, attrs)?;
        let bucket = bucket_secs.clamp(1, 86_400 * 7);
        let group_expr = match group_by {
            Some(k) => key_expr(k)?,
            None => "''".to_string(),
        };
        // A CTE over the filtered rows means the predicate (and its
        // parameters) appears once, unlike the ClickHouse query which has to
        // repeat the WHERE and bind everything twice. Top-N groups by total
        // count keep the series bounded; the rest fold into "other".
        let sql = format!(
            "WITH scoped AS ( \
                 SELECT ts, {group_expr} AS g FROM log_lines_index WHERE {w} \
             ), top AS ( \
                 SELECT g FROM scoped GROUP BY g ORDER BY count(*) DESC, g ASC LIMIT {max_groups} \
             ) \
             SELECT to_timestamp((floor(extract(epoch FROM ts) / {bucket}) * {bucket})::float8) \
                    AS bucket, \
                    (CASE WHEN g IN (SELECT g FROM top) THEN g ELSE 'other' END) AS grp, \
                    count(*)::int8 AS count \
             FROM scoped GROUP BY 1, 2 ORDER BY 1 ASC, 2 ASC",
            w = w.clause(),
            max_groups = max_groups.clamp(1, 50),
        );
        let rows = self.query_all(sql, w.values()).await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(HistogramBucket {
                ts: row.try_get::<DateTime<Utc>>("", "bucket")?,
                group: if group_by.is_some() {
                    Some(
                        row.try_get::<Option<String>>("", "grp")?
                            .unwrap_or_default(),
                    )
                } else {
                    None
                },
                count: row.try_get::<i64>("", "count")?,
            });
        }
        Ok(out)
    }

    async fn aggregate(
        &self,
        query: &LogQuery,
        attrs: &[AttrPredicate],
        group_by: &[GroupKey],
        metric: &Metric,
        limit: u32,
    ) -> Result<Vec<AggregateRow>, LogAggregatorError> {
        let w = build_where(query, attrs)?;
        let key_exprs: Vec<String> = group_by.iter().map(key_expr).collect::<Result<_, _>>()?;
        // Every element is coalesced: a `text[]` with a NULL element has no
        // faithful `Vec<String>` decoding, and the response shape (a list of
        // group values) is the same as ClickHouse's only if absent reads as
        // `''` — which is exactly what `key_expr` already promises.
        let keys_array = if key_exprs.is_empty() {
            "ARRAY[]::text[]".to_string()
        } else {
            format!(
                "ARRAY[{}]::text[]",
                key_exprs
                    .iter()
                    .map(|e| format!("coalesce({e}, '')"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let group_clause = if key_exprs.is_empty() {
            String::new()
        } else {
            format!("GROUP BY {}", key_exprs.join(", "))
        };
        let sql = format!(
            "SELECT {keys_array} AS keys, {metric} AS value, count(*)::int8 AS lines \
             FROM log_lines_index WHERE {w} {group_clause} \
             ORDER BY value DESC LIMIT {limit}",
            metric = metric_expr(metric)?,
            w = w.clause(),
            limit = limit.clamp(1, 1000),
        );
        let rows = self.query_all(sql, w.values()).await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let value: f64 = row.try_get("", "value")?;
            out.push(AggregateRow {
                keys: row.try_get::<Vec<String>>("", "keys")?,
                value: if value.is_finite() { value } else { 0.0 },
                lines: row.try_get::<i64>("", "lines")?,
            });
        }
        Ok(out)
    }

    async fn matching_chunks(
        &self,
        query: &LogQuery,
        attrs: &[AttrPredicate],
        limit: u32,
    ) -> Result<Vec<i64>, LogAggregatorError> {
        let w = build_where(query, attrs)?;
        let sql = format!(
            "SELECT DISTINCT chunk_seq FROM log_lines_index WHERE {} \
             ORDER BY chunk_seq DESC LIMIT {}",
            w.clause(),
            limit.max(1)
        );
        let rows = self.query_all(sql, w.values()).await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(row.try_get::<i64>("", "chunk_seq")?);
        }
        Ok(out)
    }

    async fn search_pointers(
        &self,
        query: &LogQuery,
        attrs: &[AttrPredicate],
    ) -> Result<Vec<LinePointer>, LogAggregatorError> {
        let mut w = build_where(query, attrs)?;
        if let Some(before) = &query.before {
            // Keyset: strictly older than the cursor line. Unlike the
            // ClickHouse index (millisecond `ts`), the exact timestamp is
            // stored here, so the comparison uses it directly and the
            // pointer tie-break only ever fires on a genuine tie.
            let (seq, idx) = before
                .chunk_position()
                .map(|(s, i)| (s, i as i32))
                .unwrap_or((i64::MAX, i32::MAX));
            let t = w.binder.bind(before.timestamp);
            let s1 = w.binder.bind(seq);
            let i1 = w.binder.bind(idx);
            w.conditions.push(format!(
                "(ts < {t} OR (ts = {t} AND (chunk_seq < {s1} \
                  OR (chunk_seq = {s1} AND line_index < {i1}))))"
            ));
        }
        let sql = format!(
            "SELECT chunk_seq, line_index, ts FROM log_lines_index WHERE {} \
             ORDER BY ts DESC, chunk_seq DESC, line_index DESC LIMIT {}",
            w.clause(),
            query.limit.clamp(1, 1000)
        );
        let rows = self.query_all(sql, w.values()).await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(LinePointer {
                chunk_seq: row.try_get::<i64>("", "chunk_seq")?,
                line_index: row.try_get::<i32>("", "line_index")? as u32,
                ts: row.try_get::<DateTime<Utc>>("", "ts")?,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(scope: LogAccessScope) -> LogQuery {
        let mut q = LogQuery::for_scope(scope);
        q.start_time = "2026-09-20T00:00:00Z".parse().unwrap();
        q.end_time = "2026-09-20T01:00:00Z".parse().unwrap();
        q
    }

    #[test]
    fn scope_is_always_present_and_empty_allow_list_is_false() {
        let w = build_where(
            &q(LogAccessScope::Allowed {
                project_ids: vec![],
                external_service_ids: vec![],
            }),
            &[],
        )
        .unwrap();
        assert!(w.clause().contains(" AND FALSE"), "{}", w.clause());

        let w = build_where(
            &q(LogAccessScope::Allowed {
                project_ids: vec![1, 2],
                external_service_ids: vec![9],
            }),
            &[],
        )
        .unwrap();
        let clause = w.clause();
        assert!(clause.contains("project_id = ANY($3::int4[])"), "{clause}");
        assert!(
            clause.contains("external_service_id = ANY($4::int4[])"),
            "{clause}"
        );
        assert_eq!(w.values().len(), 4);
    }

    #[test]
    fn window_is_clamped_to_the_backend_maximum() {
        let mut query = q(LogAccessScope::All);
        // Untouched when the caller asks for something sane.
        assert_eq!(clamped_start(&query), query.start_time);

        query.start_time = "1970-01-01T00:00:00Z".parse().unwrap();
        assert_eq!(
            clamped_start(&query),
            query.end_time - chrono::Duration::days(MAX_WINDOW_DAYS),
            "a 1970 start must read the last {MAX_WINDOW_DAYS} days, not the whole hypertable"
        );
        // The clamp is what the SQL binds, not the raw start_time.
        let mut w = build_where(&query, &[]).unwrap();
        assert_eq!(
            w.binder.values.remove(0),
            Value::from(query.end_time - chrono::Duration::days(MAX_WINDOW_DAYS))
        );
    }

    #[test]
    fn numeric_metrics_never_cast_text_columns_unguarded() {
        // `p95:trace_id` on a text column must yield NULL per row, not error
        // the whole query.
        for key in [
            "trace_id",
            "span_id",
            "request_id",
            "http_method",
            "http_route",
        ] {
            let e = attr_number_expr(key).unwrap();
            assert!(e.starts_with("(CASE WHEN"), "{key}: {e}");
            assert!(e.contains(NUMERIC_RE), "{key}: {e}");
        }
        // The genuinely numeric columns stay a direct cast.
        assert_eq!(
            attr_number_expr("status_code").unwrap(),
            "status_code::float8"
        );
        assert_eq!(
            attr_number_expr("duration_ms").unwrap(),
            "duration_ms::float8"
        );
    }

    #[test]
    fn attribute_keys_are_validated_before_quoting() {
        let bad = AttrPredicate {
            key: "x'; DROP TABLE log_lines_index; --".into(),
            op: AttrOp::Eq,
            value: Some("1".into()),
        };
        assert!(build_where(&q(LogAccessScope::All), &[bad]).is_err());
        assert!(key_expr(&GroupKey::Attr("1abc".into())).is_err());
        assert!(attr_number_expr("bad key").is_err());
        assert!(attr_exists_expr(&"k".repeat(65)).is_err());
    }

    #[test]
    fn canonical_keys_use_fixed_columns_and_dynamic_keys_use_jsonb() {
        assert_eq!(
            attr_string_expr("status_code").unwrap(),
            "status_code::text"
        );
        assert_eq!(
            attr_string_expr("worker").unwrap(),
            "coalesce(attrs->>'worker', '')"
        );
        assert_eq!(attr_exists_expr("duration_ms").unwrap(), "duration_ms <> 0");
        assert_eq!(attr_exists_expr("trace_id").unwrap(), "trace_id <> ''");
        assert_eq!(
            attr_exists_expr("worker").unwrap(),
            "(jsonb_exists(attrs, 'worker') AND jsonb_typeof(attrs->'worker') <> 'null')"
        );
        assert_eq!(
            attr_number_expr("duration_ms").unwrap(),
            "duration_ms::float8"
        );
        assert!(attr_number_expr("worker").unwrap().contains("jsonb_typeof"));
    }

    #[test]
    fn level_and_stream_render_as_names_matching_clickhouse() {
        let expr = key_expr(&GroupKey::Label(FacetField::Level)).unwrap();
        for name in ["trace", "debug", "info", "warn", "error"] {
            assert!(expr.contains(name), "{expr}");
        }
        assert_eq!(level_to_i16(LogLevel::Trace), 0);
        assert_eq!(level_to_i16(LogLevel::Error), 4);

        let mut query = q(LogAccessScope::All);
        query.levels = vec![LogLevel::Warn, LogLevel::Error];
        let w = build_where(&query, &[]).unwrap();
        assert!(w.clause().contains("level = ANY($3::int2[])"));
        assert_eq!(
            w.values().pop().unwrap(),
            Value::from(vec![3i16, 4i16]),
            "level names map to the level_to_u8 encoding"
        );

        let expr = key_expr(&GroupKey::Label(FacetField::Stream)).unwrap();
        assert!(expr.contains("stdout") && expr.contains("stderr"));
    }

    #[test]
    fn predicates_bind_values_never_inline_them() {
        let w = build_where(
            &q(LogAccessScope::All),
            &[
                AttrPredicate {
                    key: "status_code".into(),
                    op: AttrOp::Gt,
                    value: Some("499".into()),
                },
                AttrPredicate {
                    key: "cache".into(),
                    op: AttrOp::Prefix,
                    value: Some("hi'".into()),
                },
                AttrPredicate {
                    key: "worker".into(),
                    op: AttrOp::Eq,
                    value: Some("3".into()),
                },
                AttrPredicate {
                    key: "worker".into(),
                    op: AttrOp::Neq,
                    value: Some("4".into()),
                },
                AttrPredicate {
                    key: "cache".into(),
                    op: AttrOp::Exists,
                    value: None,
                },
            ],
        )
        .unwrap();
        let c = w.clause();
        assert!(!c.contains("499") && !c.contains("hi'"), "{c}");
        assert!(c.contains("status_code::float8 > $3"), "{c}");
        assert!(
            c.contains("starts_with(coalesce(attrs->>'cache', ''), $4)"),
            "{c}"
        );
        // `=` on the raw extraction (absent key is NULL, not equal);
        // `<>` on the coalesced form (absent key *is* "not that value").
        assert!(c.contains("attrs->>'worker' = $5"), "{c}");
        assert!(c.contains("coalesce(attrs->>'worker', '') <> $6"), "{c}");
        assert!(c.contains("jsonb_exists(attrs, 'cache')"), "{c}");
    }

    #[test]
    fn every_metric_renders() {
        assert_eq!(metric_expr(&Metric::Count).unwrap(), "count(*)::float8");
        assert!(metric_expr(&Metric::P95("duration_ms".into()))
            .unwrap()
            .contains("percentile_cont(0.95) WITHIN GROUP (ORDER BY duration_ms::float8)"));
        for m in [
            Metric::CountDistinct("worker".into()),
            Metric::Avg("worker".into()),
            Metric::P50("worker".into()),
            Metric::P99("worker".into()),
            Metric::Max("worker".into()),
            Metric::Sum("worker".into()),
        ] {
            let e = metric_expr(&m).unwrap();
            assert!(e.ends_with("::float8"), "{e}");
        }
        assert!(metric_expr(&Metric::Avg("bad key".into())).is_err());
    }

    #[test]
    fn residual_attrs_drop_canonical_keys() {
        let fields = serde_json::json!({"request_id": "r1", "status_code": 500, "worker": "3"});
        assert_eq!(residual_attrs(&fields), serde_json::json!({"worker": "3"}));
        let only_canonical = serde_json::json!({"trace_id": "t"});
        assert_eq!(residual_attrs(&only_canonical), serde_json::json!({}));
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
    fn row_placeholders_are_contiguous_and_one_based() {
        assert!(row_placeholders(INSERT_COLUMN_COUNT).starts_with("($1, $2, $3,"));
        let second = row_placeholders(INSERT_COLUMN_COUNT * 2);
        assert!(second.starts_with(&format!("(${}", INSERT_COLUMN_COUNT + 1)));
        assert!(second.ends_with(&format!("${})", INSERT_COLUMN_COUNT * 2)));
        assert_eq!(
            INSERT_COLUMNS.split(',').count(),
            INSERT_COLUMN_COUNT,
            "the column list and the placeholder count must agree"
        );
    }
}

#[cfg(test)]
mod live_tests {
    //! Round trip against a real TimescaleDB, via the same testcontainers
    //! helper the manifest tests use. Skips (rather than fails) when Docker
    //! is unavailable, matching `store::manifest`'s convention.

    use super::*;
    use crate::chunk::ChunkLabels;
    use crate::store::{FacetField, LogLineKey};
    use crate::types::{LogLevel, LogLine, LogStream};

    fn labels(project_id: i32) -> ChunkLabels {
        let now = Utc::now();
        ChunkLabels {
            project_id,
            external_service_id: None,
            env: "prod".into(),
            service: "api".into(),
            container_id: "c-timescale-index".into(),
            deploy_id: Some(7),
            node_id: None,
            node_name: None,
            started_at: now,
            ended_at: now,
            line_count: 0,
            level_mask: 0,
            level_counts: [0; 5],
        }
    }

    fn line(ts: DateTime<Utc>, level: LogLevel, fields: Option<serde_json::Value>) -> LogLine {
        LogLine {
            ts,
            stream: LogStream::Stdout,
            level,
            msg: "hello".into(),
            fields,
            container_id: "c-timescale-index".into(),
            service: "api".into(),
            env: "prod".into(),
            project_id: 0,
            external_service_id: None,
            deploy_id: Some(7),
            node_id: None,
            node_name: None,
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn index_query_forget_round_trip() {
        let db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(_) => {
                println!("Docker/DB not available, skipping test");
                return;
            }
        };
        let idx = TimescaleLineIndex::new(db.connection_arc());
        let project_id = 991_047;
        let labels = labels(project_id);
        let base: DateTime<Utc> = "2026-09-20T12:00:00Z".parse().unwrap();

        // Two chunks, five lines each, distinct timestamps so the keyset is
        // unambiguous. Chunk 1 carries attributes, chunk 2 does not.
        let seq_a = 900_001i64;
        let seq_b = 900_002i64;
        let mut lines_a = Vec::new();
        for i in 0..5i64 {
            lines_a.push(line(
                base + chrono::Duration::seconds(i),
                if i % 2 == 0 {
                    LogLevel::Info
                } else {
                    LogLevel::Error
                },
                Some(serde_json::json!({
                    "worker": (i % 2).to_string(),
                    "duration_ms": 10.0 * (i as f64 + 1.0),
                    "request_id": format!("r{i}"),
                })),
            ));
        }
        let lines_b: Vec<LogLine> = (0..5i64)
            .map(|i| {
                line(
                    base + chrono::Duration::seconds(10 + i),
                    LogLevel::Warn,
                    None,
                )
            })
            .collect();

        let started = std::time::Instant::now();
        idx.index_chunk(seq_a, &labels, &[Arc::new(lines_a.clone())])
            .await
            .expect("index chunk a");
        idx.index_chunk(seq_b, &labels, &[Arc::new(lines_b.clone())])
            .await
            .expect("index chunk b");
        let rows = (lines_a.len() + lines_b.len()) as f64;
        println!(
            "indexed {rows} rows in {:?} ({:.0} rows/s)",
            started.elapsed(),
            rows / started.elapsed().as_secs_f64()
        );

        let mut q = LogQuery::for_scope(LogAccessScope::Allowed {
            project_ids: vec![project_id],
            external_service_ids: vec![],
        });
        q.start_time = base - chrono::Duration::hours(1);
        q.end_time = base + chrono::Duration::hours(1);
        q.limit = 4;

        // ── re-indexing is a no-op, not a double-count ─────────────────
        // The seal pipeline re-indexes a chunk on any retry; without the
        // unique key + ON CONFLICT this silently doubles every count below
        // and stays wrong forever.
        let count = |q: LogQuery| {
            let idx = idx.clone();
            async move {
                idx.aggregate(&q, &[], &[], &Metric::Count, 1)
                    .await
                    .unwrap()[0]
                    .lines
            }
        };
        assert_eq!(count(q.clone()).await, 10);
        idx.index_chunk(seq_a, &labels, &[Arc::new(lines_a.clone())])
            .await
            .expect("re-index chunk a");
        idx.index_chunk(seq_b, &labels, &[Arc::new(lines_b.clone())])
            .await
            .expect("re-index chunk b");
        assert_eq!(
            count(q.clone()).await,
            10,
            "re-indexing the same chunks must not duplicate rows"
        );

        // ── facets ─────────────────────────────────────────────────────
        let facets = idx
            .facets(
                &q,
                &[],
                &[
                    GroupKey::Label(FacetField::Level),
                    GroupKey::Label(FacetField::Service),
                    GroupKey::Attr("worker".into()),
                ],
                10,
            )
            .await
            .expect("facets");
        println!("facets: {facets:#?}");
        let levels = &facets["level"];
        assert_eq!(levels.iter().map(|v| v.count).sum::<i64>(), 10);
        assert!(levels.iter().any(|v| v.value == "warn" && v.count == 5));
        assert!(levels.iter().any(|v| v.value == "error" && v.count == 2));
        assert_eq!(facets["service"][0].value, "api");
        // Lines without the key report '' — the ClickHouse semantics.
        let workers = &facets["worker"];
        assert!(workers.iter().any(|v| v.value.is_empty() && v.count == 5));
        assert!(workers.iter().any(|v| v.value == "0" && v.count == 3));

        // ── attribute_keys ─────────────────────────────────────────────
        let keys = idx.attribute_keys(&q, 20).await.expect("attribute_keys");
        println!("attribute_keys: {keys:?}");
        assert!(keys.iter().any(|k| k.value == "worker" && k.count == 5));
        assert!(
            !keys.iter().any(|k| k.value == "request_id"),
            "canonical keys live in fixed columns, not in attrs"
        );

        // ── histogram ──────────────────────────────────────────────────
        let hist = idx
            .histogram(&q, &[], 10, Some(&GroupKey::Label(FacetField::Level)), 5)
            .await
            .expect("histogram");
        println!("histogram: {hist:?}");
        assert_eq!(hist.iter().map(|b| b.count).sum::<i64>(), 10);
        assert!(hist.iter().all(|b| b.group.is_some()));

        // ── aggregate ──────────────────────────────────────────────────
        let counts = idx
            .aggregate(
                &q,
                &[],
                &[GroupKey::Label(FacetField::Level)],
                &Metric::Count,
                10,
            )
            .await
            .expect("aggregate count");
        println!("count by level: {counts:?}");
        assert_eq!(counts.iter().map(|r| r.lines).sum::<i64>(), 10);

        let p95 = idx
            .aggregate(
                &q,
                &[],
                &[GroupKey::Label(FacetField::Service)],
                &Metric::P95("duration_ms".into()),
                5,
            )
            .await
            .expect("aggregate p95");
        println!("p95(duration_ms) by service: {p95:?}");
        assert_eq!(p95.len(), 1);
        assert!(p95[0].value.is_finite() && p95[0].value > 0.0);

        // A p95 over a *dynamic* key that is only sometimes numeric must
        // still return a finite number, never an error.
        let p95_dyn = idx
            .aggregate(
                &q,
                &[],
                &[GroupKey::Label(FacetField::Service)],
                &Metric::P95("worker".into()),
                5,
            )
            .await
            .expect("aggregate p95 over a dynamic key");
        assert!(p95_dyn.iter().all(|r| r.value.is_finite()));

        // ── matching_chunks ────────────────────────────────────────────
        let chunks = idx.matching_chunks(&q, &[], 10).await.expect("chunks");
        assert_eq!(chunks, vec![seq_b, seq_a]);
        let only_a = idx
            .matching_chunks(
                &q,
                &[AttrPredicate {
                    key: "worker".into(),
                    op: AttrOp::Exists,
                    value: None,
                }],
                10,
            )
            .await
            .expect("chunks with predicate");
        assert_eq!(only_a, vec![seq_a]);

        // ── search_pointers + keyset paging ────────────────────────────
        let page1 = idx.search_pointers(&q, &[]).await.expect("page 1");
        assert_eq!(page1.len(), 4);
        let mut q2 = q.clone();
        q2.before = Some(LogLineKey {
            timestamp: page1[3].ts,
            container_id: labels.container_id.clone(),
            line_id: LogLineKey::sealed_line_id(page1[3].chunk_seq, page1[3].line_index),
        });
        let page2 = idx.search_pointers(&q2, &[]).await.expect("page 2");
        println!("page1: {page1:?}\npage2: {page2:?}");
        assert_eq!(page2.len(), 4);
        let ids = |p: &[LinePointer]| -> Vec<(i64, u32)> {
            p.iter().map(|x| (x.chunk_seq, x.line_index)).collect()
        };
        let (a, b) = (ids(&page1), ids(&page2));
        assert!(
            a.iter().all(|k| !b.contains(k)),
            "page 2 must not repeat page 1"
        );
        assert_eq!(
            a.len() + b.len(),
            8,
            "two full pages, nothing skipped or repeated"
        );

        // Scope never leaks: an empty allow-list yields nothing.
        let mut denied = q.clone();
        denied.scope = LogAccessScope::Allowed {
            project_ids: vec![],
            external_service_ids: vec![],
        };
        assert!(idx
            .search_pointers(&denied, &[])
            .await
            .expect("denied")
            .is_empty());

        // ── retention is idempotent ────────────────────────────────────
        idx.set_retention_days(30).await.expect("retention");
        idx.set_retention_days(30).await.expect("retention again");
        idx.set_retention_days(14).await.expect("retention changed");

        // ── forget one chunk ───────────────────────────────────────────
        idx.forget_chunks(&[seq_a]).await.expect("forget a");
        let chunks = idx.matching_chunks(&q, &[], 10).await.expect("chunks");
        assert_eq!(chunks, vec![seq_b]);
        let remaining = idx.search_pointers(&q, &[]).await.expect("after forget");
        assert!(remaining.iter().all(|p| p.chunk_seq == seq_b));

        // ── forget the other chunk after forcing compression ───────────
        // Two things are being proved here. First, that compression still
        // succeeds *with the unique index on (chunk_seq, line_index, ts)* —
        // Timescale only allows a unique index on a compressed hypertable
        // when its columns are covered by segmentby + orderby, and this is
        // the assertion that catches it if that ever stops holding. Second,
        // that the DELETE reaches compressed rows: `chunk_seq` is a
        // segmentby column, so whole compressed batches are dropped rather
        // than decompressed.
        let compressed = db
            .connection()
            .query_all(TimescaleLineIndex::stmt(
                "SELECT compress_chunk(c)::text AS chunk FROM show_chunks('log_lines_index') c"
                    .into(),
                vec![],
            ))
            .await
            .expect("compress chunks with the unique index present");
        assert!(
            !compressed.is_empty(),
            "expected at least one chunk to compress"
        );
        idx.forget_chunks(&[seq_b])
            .await
            .expect("delete on a compressed chunk");
        assert!(idx
            .matching_chunks(&q, &[], 10)
            .await
            .expect("chunks")
            .is_empty());
        assert!(idx
            .facets(&q, &[], &[GroupKey::Label(FacetField::Level)], 10)
            .await
            .expect("facets after forget")["level"]
            .is_empty());

        // ── a chunk larger than one INSERT batch ───────────────────────
        // Exercises the multi-statement path (line_index must keep
        // counting across batches, not restart at 0) and gives a rough
        // insert rate.
        let seq_c = 900_003i64;
        let big: Vec<LogLine> = (0..(INSERT_BATCH_ROWS as i64 + 500))
            .map(|i| {
                line(
                    base + chrono::Duration::milliseconds(i),
                    LogLevel::Info,
                    Some(serde_json::json!({ "worker": (i % 8).to_string() })),
                )
            })
            .collect();
        let rows = big.len();
        let started = std::time::Instant::now();
        idx.index_chunk(seq_c, &labels, &[Arc::new(big)])
            .await
            .expect("index a multi-batch chunk");
        let elapsed = started.elapsed();
        println!(
            "indexed {rows} rows in {elapsed:?} ({:.0} rows/s)",
            rows as f64 / elapsed.as_secs_f64()
        );
        let indexed = idx
            .aggregate(&q, &[], &[], &Metric::Count, 1)
            .await
            .expect("count");
        assert_eq!(indexed[0].lines, rows as i64);
        let mut q_all = q.clone();
        q_all.limit = 1;
        let newest = idx.search_pointers(&q_all, &[]).await.expect("newest");
        assert_eq!(
            newest[0].line_index,
            rows as u32 - 1,
            "line_index must keep counting across INSERT batches"
        );
    }
}
