// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ClickHouse implementation of [`LineIndexSink`] (ADR-047 §2, §4, §7).
//!
//! One `RowBinary` batch insert per sealed chunk into `log_lines_index`.
//! The `attrs` JSON column is sent as a serialised string
//! (`input_format_binary_read_json_as_string=1`), which is how the Rust
//! client speaks to the `JSON` type. Inserts are `async_insert` with
//! `wait_for_async_insert=1`: many small chunks from idle containers
//! coalesce server-side, and the ack still means "durable".

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use clickhouse::Row;
use serde::Serialize;
use temps_clickhouse::{ClickHouseConfig, Migration, ServerVersion};
use tracing::{info, warn};

use super::{IndexOutcome, LineIndexSink};
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

/// Why the ClickHouse line index could not be enabled — surfaced verbatim
/// through the capabilities endpoint so the operator knows what to fix.
#[derive(Debug, Clone)]
pub enum IndexUnavailable {
    /// `TEMPS_CLICKHOUSE_*` not (fully) set.
    NotConfigured,
    /// Server reachable but too old.
    VersionTooOld { found: ServerVersion },
    /// Could not reach the server or run migrations.
    Unreachable { error: String },
}

impl std::fmt::Display for IndexUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConfigured => write!(
                f,
                "ClickHouse is not configured (set TEMPS_CLICKHOUSE_URL, _DATABASE, _USER and \
                 _PASSWORD) — attribute facets and log analytics need it"
            ),
            Self::VersionTooOld { found } => write!(
                f,
                "ClickHouse {found} found; log analytics needs >= \
                 {MIN_CLICKHOUSE_MAJOR}.{MIN_CLICKHOUSE_MINOR} for the JSON column type"
            ),
            Self::Unreachable { error } => {
                write!(f, "ClickHouse is configured but unavailable: {error}")
            }
        }
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

/// Key → slot mapping for promoted facets (ADR-047 §2). `slots[n]` is the
/// attribute key written into `facet_attr_{n+1}`; `None` = free slot.
pub type FacetSlots = [Option<String>; FACET_SLOTS];

/// The live ClickHouse-backed sink.
pub struct ClickHouseLineIndex {
    client: clickhouse::Client,
    version: ServerVersion,
    slots: arc_swap::ArcSwap<FacetSlots>,
    /// Last TTL (days) applied with `ALTER TABLE … MODIFY TTL`; `0` = never.
    ttl_days: std::sync::atomic::AtomicU32,
}

impl ClickHouseLineIndex {
    /// Connect, gate on the server version, run migrations. Returns the
    /// reason instead of a sink when any step rules the index out — the
    /// caller wires [`super::NoLineIndex`] with that reason.
    pub async fn connect(config: &ClickHouseConfig) -> Result<Arc<Self>, IndexUnavailable> {
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

        Ok(Arc::new(Self {
            client,
            version,
            slots: arc_swap::ArcSwap::from_pointee(Default::default()),
            ttl_days: std::sync::atomic::AtomicU32::new(0),
        }))
    }

    pub fn version(&self) -> ServerVersion {
        self.version
    }

    pub fn client(&self) -> &clickhouse::Client {
        &self.client
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
        let mut insert: clickhouse::insert::Insert<IndexRow<'_>> = self
            .client
            .insert("log_lines_index")
            .await
            .map_err(ch_err)?;
        for (offset, line) in lines.iter().enumerate() {
            let line_index = first_index + offset as u32;
            let fields = line.fields.as_ref();
            let wk = fields.map(well_known).unwrap_or_default();
            let attrs = fields.map(residual_attrs).unwrap_or_else(|| "{}".into());
            let slot_values = slot_values(fields, &slots);
            let row = IndexRow {
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
            };
            insert.write(&row).await.map_err(ch_err)?;
        }
        insert.end().await.map_err(ch_err)
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
        use std::sync::atomic::Ordering;
        let days = days.clamp(1, 3650);
        if self.ttl_days.load(Ordering::Relaxed) == days {
            return Ok(());
        }
        // `MODIFY TTL` is metadata-only; expired parts are dropped by the
        // next TTL merge (`ttl_only_drop_parts` makes that a part drop, not
        // a rewrite).
        self.client
            .query(&format!(
                "ALTER TABLE log_lines_index MODIFY TTL toDateTime(ts) + INTERVAL {days} DAY"
            ))
            .execute()
            .await
            .map_err(ch_err)?;
        self.client
            .query(&format!(
                "ALTER TABLE log_attr_keys MODIFY TTL day + INTERVAL {days} DAY"
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
        // Lightweight DELETE (mask + background rewrite), not a full
        // mutation; cheap enough for compaction/purge cadence.
        let list = seqs
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .join(",");
        self.client
            .query(&format!(
                "DELETE FROM log_lines_index WHERE chunk_seq IN ({list})"
            ))
            .execute()
            .await
            .map_err(ch_err)
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
        assert!(s.contains("TEMPS_CLICKHOUSE_URL"));
        let s = IndexUnavailable::VersionTooOld {
            found: ServerVersion {
                major: 24,
                minor: 8,
                patch: 1,
            },
        }
        .to_string();
        assert!(s.contains("24.8") && s.contains("25.3"));
    }
}

// ── Analytics (ADR-047 §5) ──────────────────────────────────────────────

use super::analytics::{
    build_where, key_expr, key_name, metric_expr, AggregateRow, AggregateRowRaw, AttrPredicate,
    FacetRow, GroupKey, HistogramBucket, HistogramRow, KeyRow, LinePointer, LogAnalytics, Metric,
    Param, PointerRow, Sql,
};
use crate::store::{FacetValue, LogQuery};
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

impl ClickHouseLineIndex {
    fn query_with(&self, sql: &str, params: &[Param]) -> clickhouse::query::Query {
        bind_all(self.client.query(sql), params)
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
        let base = build_where(query, attrs)?;
        let mut out = BTreeMap::new();
        for key in keys {
            let expr = key_expr(key)?;
            let sql = format!(
                "SELECT {expr} AS value, toInt64(count()) AS count FROM log_lines_index \
                 WHERE {} GROUP BY value ORDER BY count DESC, value ASC LIMIT {}",
                base.where_clause(),
                limit.clamp(1, 1000)
            );
            let rows: Vec<FacetRow> = self
                .query_with(&sql, &base.params)
                .fetch_all()
                .await
                .map_err(ch_err)?;
            out.insert(
                key_name(key),
                rows.into_iter()
                    .map(|r| FacetValue {
                        value: r.value,
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
            "SELECT key, countMerge(lines) AS lines FROM log_attr_keys WHERE {} \
             GROUP BY key ORDER BY lines DESC LIMIT {}",
            sql.where_clause(),
            limit.clamp(1, 1000)
        );
        let rows: Vec<KeyRow> = self
            .query_with(&stmt, &sql.params)
            .fetch_all()
            .await
            .map_err(ch_err)?;
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
        let base = build_where(query, attrs)?;
        let bucket = bucket_secs.clamp(1, 86_400 * 7);
        let group_expr = match group_by {
            Some(k) => key_expr(k)?,
            None => "''".to_string(),
        };
        // Top-N groups by total count keep the series bounded; the rest
        // fold into "other".
        let sql = format!(
            "WITH top AS ( \
                 SELECT {group_expr} AS g FROM log_lines_index WHERE {w} \
                 GROUP BY g ORDER BY count() DESC LIMIT {max_groups} \
             ) \
             SELECT toInt64(intDiv(toUnixTimestamp(ts), {bucket}) * {bucket}) AS bucket, \
                    if({group_expr} IN (SELECT g FROM top), {group_expr}, 'other') AS group, \
                    toInt64(count()) AS count \
             FROM log_lines_index WHERE {w} \
             GROUP BY bucket, group ORDER BY bucket ASC, group ASC",
            w = base.where_clause(),
            max_groups = max_groups.clamp(1, 50),
        );
        // The WHERE appears twice; bind the params twice in order.
        let mut params = base.params.clone();
        params.extend(base.params.iter().cloned());
        let rows: Vec<HistogramRow> = self
            .query_with(&sql, &params)
            .fetch_all()
            .await
            .map_err(ch_err)?;
        Ok(rows
            .into_iter()
            .map(|r| HistogramBucket {
                ts: DateTime::<Utc>::from_timestamp(r.bucket, 0).unwrap_or(query.start_time),
                group: if group_by.is_some() {
                    Some(r.group)
                } else {
                    None
                },
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
        let base = build_where(query, attrs)?;
        let key_exprs: Vec<String> = group_by.iter().map(key_expr).collect::<Result<_, _>>()?;
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
             FROM log_lines_index WHERE {w} {group_clause} \
             ORDER BY value DESC LIMIT {limit}",
            metric = metric_expr(metric)?,
            w = base.where_clause(),
            limit = limit.clamp(1, 1000),
        );
        let rows: Vec<AggregateRowRaw> = self
            .query_with(&sql, &base.params)
            .fetch_all()
            .await
            .map_err(ch_err)?;
        Ok(rows
            .into_iter()
            .map(|r| AggregateRow {
                keys: r.keys,
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
        let base = build_where(query, attrs)?;
        let sql = format!(
            "SELECT chunk_seq FROM log_lines_index WHERE {} \
             GROUP BY chunk_seq ORDER BY chunk_seq DESC LIMIT {}",
            base.where_clause(),
            limit.max(1)
        );
        let rows: Vec<u64> = self
            .query_with(&sql, &base.params)
            .fetch_all()
            .await
            .map_err(ch_err)?;
        Ok(rows.into_iter().map(|s| s as i64).collect())
    }

    async fn search_pointers(
        &self,
        query: &LogQuery,
        attrs: &[AttrPredicate],
    ) -> Result<Vec<LinePointer>, LogAggregatorError> {
        let mut base = build_where(query, attrs)?;
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
             FROM log_lines_index WHERE {} \
             ORDER BY ts DESC, chunk_seq DESC, line_index DESC LIMIT {}",
            base.where_clause(),
            query.limit.clamp(1, 1000)
        );
        let rows: Vec<PointerRow> = self
            .query_with(&sql, &base.params)
            .fetch_all()
            .await
            .map_err(ch_err)?;
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
        let idx = ClickHouseLineIndex::connect(&cfg).await.expect("connect");
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
