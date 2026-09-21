// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Read side of the line index (ADR-047 §5): attribute facets, histograms,
//! aggregations and pointer search over `log_lines_index`.
//!
//! Every query is generated from a [`LogQuery`] (the same resolved,
//! authorised filter the chunk store uses) plus attribute predicates. The
//! allow-list scope is applied inside every statement — there is no query
//! shape that reaches ClickHouse without it. Identifiers that come from the
//! user (attribute keys) are validated against the parser's key grammar
//! before they are quoted into SQL; every value is a bound parameter.
//!
//! What is deliberately *not* here: message text. The index holds no
//! message bytes, so a `text` filter cannot be answered by analytics; the
//! handler rejects that combination with an explanation rather than
//! silently ignoring the filter.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use clickhouse::Row;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::error::LogAggregatorError;
use crate::parser::{is_canonical_key, MAX_ATTR_KEY_BYTES};
use crate::store::{FacetField, FacetValue, LogAccessScope, LogQuery, LogSelection, LogSourceKind};
use crate::types::LogLevel;

/// Predicate on one attribute (canonical or dynamic).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AttrPredicate {
    /// Attribute key: a canonical key (`status_code`, `http_route`, …) or
    /// any extracted key (`worker`, `http.status`).
    pub key: String,
    #[serde(default)]
    pub op: AttrOp,
    /// Omitted for `exists`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AttrOp {
    #[default]
    Eq,
    Neq,
    Exists,
    /// Case-sensitive prefix match on the string form of the value.
    Prefix,
    /// Numeric comparison (value parsed as f64; non-numeric rows never match).
    Gt,
    Lt,
}

impl AttrPredicate {
    /// Evaluate the predicate against a line's extracted `fields` — the same
    /// object the index row was built from, so this agrees with the SQL in
    /// [`build_where`] line for line. Used by the chunk scan (sealed chunks
    /// the index nominated, and unsealed head lines the index has not seen).
    ///
    /// Value semantics mirror the index: a value is compared on its string
    /// form (`3` and `"3"` are the same attribute value); `!=` is true when
    /// the key is absent; `>`/`<` need both sides numeric.
    pub fn matches(&self, fields: Option<&serde_json::Value>) -> bool {
        let value = fields
            .and_then(|f| f.as_object())
            .and_then(|m| m.get(self.key.as_str()))
            .filter(|v| !v.is_null());
        let as_string = |v: &serde_json::Value| -> String {
            match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            }
        };
        match self.op {
            AttrOp::Exists => value.is_some(),
            AttrOp::Eq => value.is_some_and(|v| Some(as_string(v)) == self.value),
            AttrOp::Neq => !value.is_some_and(|v| Some(as_string(v)) == self.value),
            AttrOp::Prefix => value.is_some_and(|v| {
                self.value
                    .as_deref()
                    .is_some_and(|p| as_string(v).starts_with(p))
            }),
            AttrOp::Gt | AttrOp::Lt => {
                let Some(want) = self.value.as_deref().and_then(|v| v.parse::<f64>().ok()) else {
                    return false;
                };
                let Some(have) = value.and_then(|v| match v {
                    serde_json::Value::Number(n) => n.as_f64(),
                    serde_json::Value::String(s) => s.parse().ok(),
                    _ => None,
                }) else {
                    return false;
                };
                if self.op == AttrOp::Gt {
                    have > want
                } else {
                    have < want
                }
            }
        }
    }
}

/// What to group or facet by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case", tag = "kind", content = "name")]
pub enum GroupKey {
    /// A stream label (`service`, `env`, `level`, …).
    Label(FacetField),
    /// An attribute key.
    Attr(String),
}

/// Aggregate metric for [`LogAnalytics::aggregate`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case", tag = "fn", content = "attr")]
pub enum Metric {
    Count,
    CountDistinct(String),
    Avg(String),
    P50(String),
    P95(String),
    P99(String),
    Max(String),
    Sum(String),
}

/// One time bucket of a histogram, optionally split by a group value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct HistogramBucket {
    #[schema(value_type = String)]
    pub ts: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub count: i64,
}

/// One row of an aggregation: the group values (in `group_by` order) and
/// the metric.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AggregateRow {
    pub keys: Vec<String>,
    pub value: f64,
    /// Lines that contributed (always populated; equals `value` for
    /// `Count`).
    pub lines: i64,
}

/// A line located by the index: enough to fetch it from its chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinePointer {
    pub chunk_seq: i64,
    pub line_index: u32,
    /// Millisecond-precision timestamp from the index (ordering only; the
    /// chunk holds the exact one).
    pub ts: DateTime<Utc>,
}

/// Analytics over the line index. Implemented by the ClickHouse sink; the
/// no-index sink returns [`LogAggregatorError::LineIndex`] with the reason.
#[async_trait::async_trait]
pub trait LogAnalytics: Send + Sync {
    /// Distinct values with counts for each requested key, most frequent
    /// first, at most `limit` per key.
    async fn facets(
        &self,
        query: &LogQuery,
        attrs: &[AttrPredicate],
        keys: &[GroupKey],
        limit: u32,
    ) -> Result<BTreeMap<String, Vec<FacetValue>>, LogAggregatorError>;

    /// Attribute keys present in the window (from `log_attr_keys`), with
    /// line counts, most common first.
    async fn attribute_keys(
        &self,
        query: &LogQuery,
        limit: u32,
    ) -> Result<Vec<FacetValue>, LogAggregatorError>;

    /// Line counts per `bucket_secs`, optionally split by `group_by`
    /// (at most `max_groups` series; the rest are folded into `"other"`).
    async fn histogram(
        &self,
        query: &LogQuery,
        attrs: &[AttrPredicate],
        bucket_secs: u32,
        group_by: Option<&GroupKey>,
        max_groups: u32,
    ) -> Result<Vec<HistogramBucket>, LogAggregatorError>;

    /// `GROUP BY group_by` with `metric`, top `limit` rows by the metric.
    async fn aggregate(
        &self,
        query: &LogQuery,
        attrs: &[AttrPredicate],
        group_by: &[GroupKey],
        metric: &Metric,
        limit: u32,
    ) -> Result<Vec<AggregateRow>, LogAggregatorError>;

    /// Newest-first pointers to lines matching the filter and predicates,
    /// strictly older than `query.before` when set, at most `query.limit`.
    /// Manifest `seq`s of chunks holding at least one line matching `query`
    /// and `attrs`, newest first, capped at `limit`. Lets a text needle
    /// combined with attribute predicates run the bloom-pruned chunk scan
    /// over only the chunks that can matter.
    async fn matching_chunks(
        &self,
        query: &LogQuery,
        attrs: &[AttrPredicate],
        limit: u32,
    ) -> Result<Vec<i64>, LogAggregatorError>;

    async fn search_pointers(
        &self,
        query: &LogQuery,
        attrs: &[AttrPredicate],
    ) -> Result<Vec<LinePointer>, LogAggregatorError>;
}

// ── SQL generation ───────────────────────────────────────────────────────

/// How a target stores the residual-attributes bag.
pub(crate) enum AttrStorage {
    /// A `JSON` column with typed dynamic subcolumns — a filter on one
    /// attribute reads one column (the instance's own ClickHouse).
    Json,
    /// A `String` column holding the JSON text, read with the `JSON*`
    /// functions. Temps Cloud's telemetry proxies forward a fixed parameter
    /// allow-list, and the per-request
    /// `input_format_binary_read_json_as_string` /
    /// `output_format_binary_write_json_as_string` settings the `JSON` type
    /// needs are not on it, so the Cloud table stores text.
    JsonString,
}

/// How rows are tied to a project or external service.
pub(crate) enum Scoping {
    /// Raw `project_id` / `external_service_id`, `0` meaning "none".
    Raw,
    /// Pseudonymous `project_ref` / `external_service_ref` — Cloud never
    /// learns a local id (ADR-043). `''` is the external-service "none".
    Pseudonymous(Arc<temps_cloud_client::CloudLink>),
}

/// Everything about a target that changes the SQL: where the rows live, how
/// the attributes are stored, how a project is named, and whether forgotten
/// chunks are masked by tombstones instead of deleted.
pub(crate) struct Dialect {
    pub lines_table: &'static str,
    pub attrs: AttrStorage,
    pub scoping: Scoping,
    /// Table of chunk seqs the instance has forgotten. `Some` only where
    /// `DELETE` is not available (Cloud): every read then excludes them, so
    /// compaction and purge never double-count.
    pub tombstones: Option<&'static str>,
}

impl Dialect {
    /// The instance's own ClickHouse. Must render exactly the SQL this file
    /// has always rendered.
    pub fn local() -> Self {
        Self {
            lines_table: super::clickhouse::LOCAL_LINES_TABLE,
            attrs: AttrStorage::Json,
            scoping: Scoping::Raw,
            tombstones: None,
        }
    }

    /// Temps Cloud's tenant ClickHouse behind the telemetry proxies.
    pub fn cloud(link: Arc<temps_cloud_client::CloudLink>) -> Self {
        Self {
            lines_table: super::clickhouse::CLOUD_LINES_TABLE,
            attrs: AttrStorage::JsonString,
            scoping: Scoping::Pseudonymous(link),
            tombstones: Some(super::clickhouse::CLOUD_FORGOTTEN_CHUNKS_TABLE),
        }
    }

    pub fn is_cloud(&self) -> bool {
        matches!(self.scoping, Scoping::Pseudonymous(_))
    }

    /// The name Cloud knows a project by, or the id itself on a raw target.
    pub fn project_ref(&self, id: i32) -> Result<String, LogAggregatorError> {
        match &self.scoping {
            Scoping::Raw => Ok(id.to_string()),
            Scoping::Pseudonymous(link) => pseudonym(link, "project", &id.to_string()),
        }
    }

    /// Same for an external service; `0` ("none") stays the empty sentinel
    /// rather than becoming the pseudonym of the id `0`.
    pub fn external_service_ref(&self, id: i32) -> Result<String, LogAggregatorError> {
        match &self.scoping {
            Scoping::Raw => Ok(id.to_string()),
            Scoping::Pseudonymous(_) if id == 0 => Ok(String::new()),
            Scoping::Pseudonymous(link) => pseudonym(link, "external_service", &id.to_string()),
        }
    }
}

/// The scoping keys are derived with the same function that produced them on
/// the way out; a link that cannot derive one is reported rather than papered
/// over with an unscoped query.
fn pseudonym(
    link: &temps_cloud_client::CloudLink,
    domain: &'static str,
    value: &str,
) -> Result<String, LogAggregatorError> {
    link.pseudonymize_telemetry_id(domain, value)
        .map_err(|error| LogAggregatorError::LineIndex {
            reason: format!(
                "could not derive the Temps Cloud scoping key for {domain} {value}: {error}"
            ),
        })
}

/// A bound parameter for the `clickhouse` client (`?` placeholders).
#[derive(Debug, Clone)]
pub(crate) enum Param {
    Str(String),
    I32(i32),
    I64(i64),
    F64(f64),
    I32List(Vec<i32>),
    StrList(Vec<String>),
}

/// Accumulates a `WHERE` clause and its parameters in order.
#[derive(Debug, Default)]
pub(crate) struct Sql {
    pub conditions: Vec<String>,
    pub params: Vec<Param>,
}

impl Sql {
    fn bind(&mut self, p: Param) -> &'static str {
        self.params.push(p);
        "?"
    }

    pub fn where_clause(&self) -> String {
        if self.conditions.is_empty() {
            "1".to_string()
        } else {
            self.conditions.join(" AND ")
        }
    }
}

/// Reject any key that is not a valid attribute identifier before it is
/// interpolated into SQL as a quoted identifier.
pub(crate) fn validate_key(key: &str) -> Result<(), LogAggregatorError> {
    let ok = !key.is_empty()
        && key.len() <= MAX_ATTR_KEY_BYTES
        && key
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.');
    if ok {
        Ok(())
    } else {
        Err(LogAggregatorError::Validation {
            message: format!("invalid attribute key {key:?}"),
        })
    }
}

/// SQL expression (as a `String` value) for a group key.
///
/// On a pseudonymous target `project` and `external_service` group by the
/// ref, not the id — Cloud holds no ids. The caller maps the refs back for
/// the response.
pub(crate) fn key_expr(key: &GroupKey, d: &Dialect) -> Result<String, LogAggregatorError> {
    Ok(match key {
        GroupKey::Label(f) => match f {
            FacetField::Env => "env".into(),
            FacetField::Service => "service".into(),
            FacetField::Level => "toString(level)".into(),
            FacetField::Stream => "toString(stream)".into(),
            FacetField::Project => match d.scoping {
                Scoping::Raw => "toString(project_id)".into(),
                Scoping::Pseudonymous(_) => "project_ref".into(),
            },
            FacetField::ExternalService => match d.scoping {
                Scoping::Raw => "toString(external_service_id)".into(),
                Scoping::Pseudonymous(_) => "external_service_ref".into(),
            },
            FacetField::Node => "toString(node_id)".into(),
            FacetField::Deploy => "toString(deploy_id)".into(),
            FacetField::Container => "container_id".into(),
        },
        GroupKey::Attr(k) => attr_string_expr(k, d)?,
    })
}

/// Display name of a group key in responses.
pub(crate) fn key_name(key: &GroupKey) -> String {
    match key {
        GroupKey::Label(f) => f.as_str().to_string(),
        GroupKey::Attr(k) => k.clone(),
    }
}

/// The column/subcolumn holding an attribute, as a raw (typed) expression.
///
/// On a JSON-text target the "raw" form is the value's JSON text
/// (`"hit"`, `3`, `{…}`), which is only ever consumed by the helpers below —
/// see [`attr_equality_expr`] for why equality cannot use it there.
fn attr_raw_expr(key: &str, d: &Dialect) -> Result<String, LogAggregatorError> {
    validate_key(key)?;
    if is_canonical_key(key) {
        return Ok(key.to_string());
    }
    Ok(match d.attrs {
        AttrStorage::Json => format!("attrs.`{key}`"),
        AttrStorage::JsonString => format!("JSONExtractRaw(attrs, '{key}')"),
    })
}

/// Attribute as a `String` (dynamic subcolumns are typed `Dynamic`; labels
/// are `''`/`0` when absent and are shown as such).
fn attr_string_expr(key: &str, d: &Dialect) -> Result<String, LogAggregatorError> {
    let raw = attr_raw_expr(key, d)?;
    if is_canonical_key(key) {
        return Ok(format!("toString({raw})"));
    }
    Ok(match d.attrs {
        AttrStorage::Json => format!("toString(ifNull({raw}, ''))"),
        // `JSONExtractString` is the only form that unescapes a string value,
        // but it returns `''` for a number — and the index compares values on
        // their string form (`3` and `"3"` are the same value), so a number
        // has to come back as its digits. Hence the branch on `JSONType`
        // rather than either function alone. A missing key is `''`, matching
        // the `ifNull` above.
        AttrStorage::JsonString => format!(
            "if(JSONType(attrs, '{key}') = 'String', JSONExtractString(attrs, '{key}'), \
             trim(BOTH '\"' FROM {raw}))"
        ),
    })
}

/// Attribute for an equality test.
///
/// On the `JSON` column this is the raw (possibly `Dynamic`) subcolumn: an
/// absent key is NULL, which is simply not equal, and skipping the
/// `toString(ifNull(…))` wrapper reads 2.4× faster at 50M lines. On the
/// JSON-text column the raw form still carries its JSON quotes, so equality
/// has to compare the decoded string.
fn attr_equality_expr(key: &str, d: &Dialect) -> Result<String, LogAggregatorError> {
    match d.attrs {
        AttrStorage::Json => attr_raw_expr(key, d),
        AttrStorage::JsonString => attr_string_expr(key, d),
    }
}

/// Attribute as `Float64` for numeric metrics/comparisons (`NULL` when not
/// numeric).
fn attr_number_expr(key: &str, d: &Dialect) -> Result<String, LogAggregatorError> {
    let raw = attr_raw_expr(key, d)?;
    if is_canonical_key(key) {
        return Ok(format!("toFloat64({raw})"));
    }
    Ok(match d.attrs {
        AttrStorage::Json => format!("toFloat64OrNull(toString(ifNull({raw}, '')))"),
        // Both `7` and `"7"` are numeric here, as they are locally: the raw
        // JSON text of a number parses directly, a quoted number is unescaped
        // first, and anything else is NULL.
        AttrStorage::JsonString => format!(
            "toFloat64OrNull(if(JSONType(attrs, '{key}') = 'String', \
             JSONExtractString(attrs, '{key}'), {raw}))"
        ),
    })
}

/// Existence test for an attribute.
fn attr_exists_expr(key: &str, d: &Dialect) -> Result<String, LogAggregatorError> {
    let raw = attr_raw_expr(key, d)?;
    if is_canonical_key(key) {
        return Ok(match key {
            "status_code" | "duration_ms" => format!("{raw} != 0"),
            _ => format!("{raw} != ''"),
        });
    }
    Ok(match d.attrs {
        AttrStorage::Json => format!("{raw} IS NOT NULL"),
        // `JSONHas` is true for a key whose value is `null`; locally a null
        // is absent, so the type check keeps the two sides agreeing.
        AttrStorage::JsonString => {
            format!("(JSONHas(attrs, '{key}') AND JSONType(attrs, '{key}') != 'Null')")
        }
    })
}

fn level_name(l: LogLevel) -> &'static str {
    match l {
        LogLevel::Trace => "trace",
        LogLevel::Debug => "debug",
        LogLevel::Info => "info",
        LogLevel::Warn => "warn",
        LogLevel::Error => "error",
    }
}

fn ts_param(t: DateTime<Utc>) -> Param {
    Param::I64(t.timestamp_millis())
}

/// Scope / selection fragment: mirrors `manifest::scope_condition` exactly
/// (`external_service_id = 0` is the index's "none"; on a pseudonymous
/// target the ids are mapped to refs and `''` is the "none").
fn resource_condition(
    sql: &mut Sql,
    project_ids: &[i32],
    service_ids: &[i32],
    d: &Dialect,
) -> Result<String, LogAggregatorError> {
    if project_ids.is_empty() && service_ids.is_empty() {
        return Ok("0".to_string());
    }
    Ok(match d.scoping {
        Scoping::Raw => {
            let p = sql.bind(Param::I32List(project_ids.to_vec()));
            let s = sql.bind(Param::I32List(service_ids.to_vec()));
            format!("((external_service_id = 0 AND project_id IN {p}) OR (external_service_id != 0 AND external_service_id IN {s}))")
        }
        Scoping::Pseudonymous(_) => {
            let projects = project_ids
                .iter()
                .map(|id| d.project_ref(*id))
                .collect::<Result<Vec<_>, _>>()?;
            let services = service_ids
                .iter()
                .map(|id| d.external_service_ref(*id))
                .collect::<Result<Vec<_>, _>>()?;
            let p = sql.bind(Param::StrList(projects));
            let s = sql.bind(Param::StrList(services));
            format!("((external_service_ref = '' AND project_ref IN {p}) OR (external_service_ref != '' AND external_service_ref IN {s}))")
        }
    })
}

/// `WHERE` for a [`LogQuery`] plus attribute predicates. Never omits the
/// scope.
pub(crate) fn build_where(
    query: &LogQuery,
    attrs: &[AttrPredicate],
    d: &Dialect,
) -> Result<Sql, LogAggregatorError> {
    let mut sql = Sql::default();

    let start = sql.bind(ts_param(query.start_time));
    sql.conditions
        .push(format!("ts >= fromUnixTimestamp64Milli({start})"));
    let end = sql.bind(ts_param(query.end_time));
    sql.conditions
        .push(format!("ts <= fromUnixTimestamp64Milli({end})"));

    match &query.scope {
        LogAccessScope::All => {}
        LogAccessScope::Allowed {
            project_ids,
            external_service_ids,
        } => {
            let c = resource_condition(&mut sql, project_ids, external_service_ids, d)?;
            sql.conditions.push(c);
        }
    }
    let (no_service, some_service) = match d.scoping {
        Scoping::Raw => ("external_service_id = 0", "external_service_id != 0"),
        Scoping::Pseudonymous(_) => ("external_service_ref = ''", "external_service_ref != ''"),
    };
    match query.source {
        LogSourceKind::Application => sql.conditions.push(no_service.into()),
        LogSourceKind::Service => sql.conditions.push(some_service.into()),
        LogSourceKind::Collected => {}
    }
    if let Some(LogSelection {
        project_ids,
        external_service_ids,
    }) = &query.selection
    {
        let c = resource_condition(&mut sql, project_ids, external_service_ids, d)?;
        sql.conditions.push(c);
    }
    // Chunks the instance has forgotten (compacted away, purged, or missing)
    // are masked here where they cannot be deleted, so no aggregation counts
    // a line the reader can no longer fetch.
    if let Some(tombstones) = d.tombstones {
        sql.conditions.push(format!(
            "chunk_seq NOT IN (SELECT chunk_seq FROM {tombstones})"
        ));
    }
    if !query.levels.is_empty() {
        let v = sql.bind(Param::StrList(
            query
                .levels
                .iter()
                .map(|l| level_name(*l).to_string())
                .collect(),
        ));
        sql.conditions.push(format!("toString(level) IN {v}"));
    }
    if !query.envs.is_empty() {
        let v = sql.bind(Param::StrList(query.envs.clone()));
        sql.conditions.push(format!("env IN {v}"));
    }
    if !query.services.is_empty() {
        let v = sql.bind(Param::StrList(query.services.clone()));
        sql.conditions.push(format!("service IN {v}"));
    }
    if !query.container_ids.is_empty() {
        let v = sql.bind(Param::StrList(query.container_ids.clone()));
        sql.conditions.push(format!("container_id IN {v}"));
    }
    if !query.node_ids.is_empty() {
        let v = sql.bind(Param::I32List(query.node_ids.clone()));
        sql.conditions.push(format!("node_id IN {v}"));
    }
    if let Some(d) = query.deploy_id {
        let v = sql.bind(Param::I32(d));
        sql.conditions.push(format!("deploy_id = {v}"));
    }

    for p in attrs {
        let cond = match p.op {
            AttrOp::Exists => attr_exists_expr(&p.key, d)?,
            AttrOp::Eq | AttrOp::Neq | AttrOp::Prefix => {
                let value = p
                    .value
                    .clone()
                    .ok_or_else(|| LogAggregatorError::Validation {
                        message: format!("attribute predicate on {:?} needs a value", p.key),
                    })?;
                let v = sql.bind(Param::Str(value));
                match p.op {
                    // `=` compares the cheapest form the storage allows (see
                    // `attr_equality_expr`); `!=` always compares the string
                    // form so lines without the key count as "not that value".
                    AttrOp::Eq => format!("{} = {v}", attr_equality_expr(&p.key, d)?),
                    AttrOp::Neq => format!("{} != {v}", attr_string_expr(&p.key, d)?),
                    _ => format!("startsWith({}, {v})", attr_string_expr(&p.key, d)?),
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
                let expr = attr_number_expr(&p.key, d)?;
                let v = sql.bind(Param::F64(value));
                if p.op == AttrOp::Gt {
                    format!("{expr} > {v}")
                } else {
                    format!("{expr} < {v}")
                }
            }
        };
        sql.conditions.push(cond);
    }

    Ok(sql)
}

/// Metric expression and the attribute it needs (if any).
pub(crate) fn metric_expr(metric: &Metric, d: &Dialect) -> Result<String, LogAggregatorError> {
    // Numeric attributes are `Nullable` (non-numeric rows → NULL) and the
    // aggregates over them are therefore `Nullable(Float64)`; a group with
    // no numeric value reports 0 rather than failing to deserialise.
    Ok(match metric {
        Metric::Count => "toFloat64(count())".into(),
        Metric::CountDistinct(k) => format!("toFloat64(uniq({}))", attr_string_expr(k, d)?),
        Metric::Avg(k) => format!("ifNull(avg({}), 0)", attr_number_expr(k, d)?),
        Metric::P50(k) => format!("ifNull(quantile(0.5)({}), 0)", attr_number_expr(k, d)?),
        Metric::P95(k) => format!("ifNull(quantile(0.95)({}), 0)", attr_number_expr(k, d)?),
        Metric::P99(k) => format!("ifNull(quantile(0.99)({}), 0)", attr_number_expr(k, d)?),
        Metric::Max(k) => format!("ifNull(max({}), 0)", attr_number_expr(k, d)?),
        Metric::Sum(k) => format!("ifNull(sum({}), 0)", attr_number_expr(k, d)?),
    })
}

// ── row types ────────────────────────────────────────────────────────────

#[derive(Debug, Row, Deserialize)]
pub(crate) struct FacetRow {
    pub value: String,
    pub count: i64,
}

#[derive(Debug, Row, Deserialize)]
pub(crate) struct KeyRow {
    pub key: String,
    pub lines: u64,
}

#[derive(Debug, Row, Deserialize)]
pub(crate) struct HistogramRow {
    pub bucket: i64,
    pub group: String,
    pub count: i64,
}

#[derive(Debug, Row, Deserialize)]
pub(crate) struct AggregateRowRaw {
    pub keys: Vec<String>,
    pub value: f64,
    pub lines: i64,
}

#[derive(Debug, Row, Deserialize)]
pub(crate) struct PointerRow {
    pub chunk_seq: u64,
    pub line_index: u32,
    pub ts_ms: i64,
}

/// An enrolled, telemetry-enabled [`CloudLink`](temps_cloud_client::CloudLink)
/// for dialect tests, shared with `clickhouse.rs`.
///
/// No network and no enrollment round trip: the state file is the only thing
/// the link reads to consider itself linked, and the scoping keys are derived
/// from the token in it. The caller keeps the `TempDir` alive.
#[cfg(test)]
pub(crate) fn test_cloud_link(dir: &std::path::Path) -> Arc<temps_cloud_client::CloudLink> {
    let mut state = temps_cloud_client::state::EnrollmentState::new("https://cloud.example.test");
    state.token = Some("inst_test_token".into());
    state
        .save(&dir.join("cloud-link").join("state.json"))
        .expect("persist link state");
    let link = Arc::new(temps_cloud_client::CloudLink::load(
        dir.to_path_buf(),
        "0.1.0-test",
    ));
    link.set_feature_switches(temps_cloud_client::CloudFeatureSwitches {
        telemetry: true,
        backups: false,
        notifications: false,
    })
    .expect("apply feature switches");
    link
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dialect every pre-Cloud expectation in this file is written
    /// against; its SQL must not move.
    fn local() -> Dialect {
        Dialect::local()
    }

    fn q(scope: LogAccessScope) -> LogQuery {
        let mut q = LogQuery::for_scope(scope);
        q.start_time = "2026-09-20T00:00:00Z".parse().unwrap();
        q.end_time = "2026-09-20T01:00:00Z".parse().unwrap();
        q
    }

    #[test]
    fn scope_is_always_present_and_empty_allow_list_is_false() {
        let sql = build_where(
            &q(LogAccessScope::Allowed {
                project_ids: vec![],
                external_service_ids: vec![],
            }),
            &[],
            &local(),
        )
        .unwrap();
        assert!(sql.where_clause().contains(" AND 0"));
        let sql = build_where(
            &q(LogAccessScope::Allowed {
                project_ids: vec![1, 2],
                external_service_ids: vec![9],
            }),
            &[],
            &local(),
        )
        .unwrap();
        assert!(sql.where_clause().contains("project_id IN ?"));
        assert!(sql.where_clause().contains("external_service_id IN ?"));
        assert_eq!(sql.params.len(), 4);
    }

    #[test]
    fn attribute_keys_are_validated_before_quoting() {
        let mut query = q(LogAccessScope::All);
        query.limit = 10;
        let bad = AttrPredicate {
            key: "x`; DROP TABLE".into(),
            op: AttrOp::Eq,
            value: Some("1".into()),
        };
        assert!(build_where(&query, &[bad], &local()).is_err());
        assert!(validate_key("http.status").is_ok());
        assert!(validate_key("1abc").is_err());
        assert!(validate_key(&"k".repeat(65)).is_err());
    }

    #[test]
    fn canonical_keys_use_fixed_columns_and_dynamic_keys_use_json() {
        assert_eq!(
            attr_string_expr("status_code", &local()).unwrap(),
            "toString(status_code)"
        );
        assert_eq!(
            attr_string_expr("worker", &local()).unwrap(),
            "toString(ifNull(attrs.`worker`, ''))"
        );
        assert_eq!(
            attr_exists_expr("duration_ms", &local()).unwrap(),
            "duration_ms != 0"
        );
        assert_eq!(
            attr_exists_expr("worker", &local()).unwrap(),
            "attrs.`worker` IS NOT NULL"
        );
    }

    #[test]
    fn predicates_bind_values_never_inline_them() {
        let query = q(LogAccessScope::All);
        let sql = build_where(
            &query,
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
            ],
            &local(),
        )
        .unwrap();
        let w = sql.where_clause();
        assert!(!w.contains("499") && !w.contains("hi'"), "{w}");
        assert!(w.contains("toFloat64(status_code) > ?"));
        assert!(w.contains("startsWith(toString(ifNull(attrs.`cache`, '')), ?)"));
    }

    #[test]
    fn predicate_matches_mirror_the_sql_semantics() {
        let fields = serde_json::json!({"worker": "3", "n": 7, "cache": "hit", "nested": null});
        let p = |key: &str, op: AttrOp, value: Option<&str>| AttrPredicate {
            key: key.into(),
            op,
            value: value.map(String::from),
        };
        assert!(p("worker", AttrOp::Eq, Some("3")).matches(Some(&fields)));
        assert!(
            p("n", AttrOp::Eq, Some("7")).matches(Some(&fields)),
            "numbers compare on their string form"
        );
        assert!(!p("worker", AttrOp::Eq, Some("4")).matches(Some(&fields)));
        assert!(
            p("missing", AttrOp::Neq, Some("x")).matches(Some(&fields)),
            "absent key is 'not that value'"
        );
        assert!(!p("worker", AttrOp::Neq, Some("3")).matches(Some(&fields)));
        assert!(p("cache", AttrOp::Exists, None).matches(Some(&fields)));
        assert!(
            !p("nested", AttrOp::Exists, None).matches(Some(&fields)),
            "null is absent"
        );
        assert!(!p("missing", AttrOp::Exists, None).matches(Some(&fields)));
        assert!(p("cache", AttrOp::Prefix, Some("hi")).matches(Some(&fields)));
        assert!(
            !p("cache", AttrOp::Prefix, Some("HI")).matches(Some(&fields)),
            "prefix is case-sensitive"
        );
        assert!(p("n", AttrOp::Gt, Some("6.5")).matches(Some(&fields)));
        assert!(
            p("worker", AttrOp::Lt, Some("4")).matches(Some(&fields)),
            "numeric strings compare numerically"
        );
        assert!(
            !p("cache", AttrOp::Gt, Some("1")).matches(Some(&fields)),
            "non-numeric never matches"
        );
        assert!(!p("worker", AttrOp::Gt, Some("abc")).matches(Some(&fields)));
        assert!(!p("worker", AttrOp::Eq, Some("3")).matches(None));
        assert!(p("worker", AttrOp::Neq, Some("3")).matches(None));
    }

    #[test]
    fn metric_expressions() {
        assert_eq!(
            metric_expr(&Metric::Count, &local()).unwrap(),
            "toFloat64(count())"
        );
        assert_eq!(
            metric_expr(&Metric::P95("duration_ms".into()), &local()).unwrap(),
            "ifNull(quantile(0.95)(toFloat64(duration_ms)), 0)"
        );
        assert!(metric_expr(&Metric::Avg("bad key".into()), &local()).is_err());
    }

    // ── Cloud dialect ────────────────────────────────────────────────────

    fn cloud(dir: &tempfile::TempDir) -> Dialect {
        Dialect::cloud(test_cloud_link(dir.path()))
    }

    #[test]
    fn cloud_attributes_are_read_with_the_json_string_functions() {
        let dir = tempfile::tempdir().expect("temp dir");
        let d = cloud(&dir);
        // Canonical keys are real columns on both sides — unchanged.
        assert_eq!(
            attr_string_expr("status_code", &d).unwrap(),
            "toString(status_code)"
        );
        // A dynamic key is JSON text: strings are unescaped, numbers keep
        // their digits (the index compares values on their string form).
        assert_eq!(
            attr_string_expr("worker", &d).unwrap(),
            "if(JSONType(attrs, 'worker') = 'String', JSONExtractString(attrs, 'worker'), \
             trim(BOTH '\"' FROM JSONExtractRaw(attrs, 'worker')))"
        );
        assert_eq!(
            attr_number_expr("worker", &d).unwrap(),
            "toFloat64OrNull(if(JSONType(attrs, 'worker') = 'String', \
             JSONExtractString(attrs, 'worker'), JSONExtractRaw(attrs, 'worker')))"
        );
        assert_eq!(
            attr_exists_expr("worker", &d).unwrap(),
            "(JSONHas(attrs, 'worker') AND JSONType(attrs, 'worker') != 'Null')"
        );
        // Equality cannot compare the raw form there: it still carries the
        // JSON quotes.
        assert_eq!(
            attr_equality_expr("worker", &d).unwrap(),
            attr_string_expr("worker", &d).unwrap()
        );
        assert!(attr_string_expr("x'; DROP TABLE", &d).is_err());
    }

    #[test]
    fn cloud_scoping_binds_refs_and_groups_by_them() {
        let dir = tempfile::tempdir().expect("temp dir");
        let d = cloud(&dir);
        let sql = build_where(
            &q(LogAccessScope::Allowed {
                project_ids: vec![1, 2],
                external_service_ids: vec![9],
            }),
            &[],
            &d,
        )
        .unwrap();
        let w = sql.where_clause();
        assert!(
            w.contains("external_service_ref = '' AND project_ref IN ?"),
            "{w}"
        );
        assert!(
            w.contains("external_service_ref != '' AND external_service_ref IN ?"),
            "{w}"
        );
        assert!(
            !w.contains("project_id"),
            "Cloud never sees a local id: {w}"
        );
        // The refs are opaque and bound, never the ids themselves.
        let refs: Vec<&Vec<String>> = sql
            .params
            .iter()
            .filter_map(|p| match p {
                Param::StrList(v) => Some(v),
                _ => None,
            })
            .collect();
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].len(), 2);
        assert!(refs[0].iter().all(|r| r != "1" && r != "2"));
        assert_eq!(
            key_expr(&GroupKey::Label(FacetField::Project), &d).unwrap(),
            "project_ref"
        );
        assert_eq!(
            key_expr(&GroupKey::Label(FacetField::ExternalService), &d).unwrap(),
            "external_service_ref"
        );
    }

    #[test]
    fn every_cloud_read_excludes_forgotten_chunks_and_no_local_read_does() {
        let dir = tempfile::tempdir().expect("temp dir");
        let tombstones = "chunk_seq NOT IN (SELECT chunk_seq FROM telemetry_log_forgotten_chunks)";
        // Cloud cannot DELETE through the proxies, so the mask is the only
        // thing keeping a compacted chunk out of every aggregation.
        for scope in [
            LogAccessScope::All,
            LogAccessScope::Allowed {
                project_ids: vec![7],
                external_service_ids: vec![],
            },
        ] {
            let sql = build_where(&q(scope), &[], &cloud(&dir)).unwrap();
            assert!(sql.where_clause().contains(tombstones));
        }
        let sql = build_where(&q(LogAccessScope::All), &[], &local()).unwrap();
        assert!(!sql
            .where_clause()
            .contains("telemetry_log_forgotten_chunks"));
    }

    #[test]
    fn the_external_service_none_sentinel_differs_per_dialect() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut query = q(LogAccessScope::All);
        query.source = LogSourceKind::Application;
        assert!(build_where(&query, &[], &local())
            .unwrap()
            .where_clause()
            .contains("external_service_id = 0"));
        assert!(build_where(&query, &[], &cloud(&dir))
            .unwrap()
            .where_clause()
            .contains("external_service_ref = ''"));
        // `0` is "no external service", not the pseudonym of the id 0.
        assert_eq!(cloud(&dir).external_service_ref(0).unwrap(), "");
        assert!(!cloud(&dir).external_service_ref(9).unwrap().is_empty());
    }
}
