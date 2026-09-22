// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `log_chunks` manifest queries (ADR-046 §2, §3 step 1).
//!
//! [`ManifestRepo`] is the planner's SQL layer: it turns a [`LogQuery`] (or a
//! narrower scope, for `around`/`by_seq`) into candidate chunk manifests, and
//! it is the only place that reads or writes `log_chunks` rows for the v2
//! object-storage chunk store. Every predicate is built with bound
//! parameters (`$n`) — caller values are never interpolated into SQL text —
//! following the pattern in the retired `TimescaleLogLineStore`.
//!
//! # Authorization and selection, fail-closed
//!
//! [`LogAccessScope::Allowed`] with both lists empty, and
//! [`LogSelection`] with both lists empty, each compile to a literal `FALSE`
//! predicate rather than an `= ANY('{}')` array comparison. Both forms are
//! equivalent in Postgres, but the literal makes the fail-closed intent
//! visible in the generated SQL and in the tests below, instead of relying on
//! an incidental property of empty-array comparison.
//!
//! # Live vs. tombstoned
//!
//! Every read that answers a query (`candidates`, `around`, `by_seq`,
//! `facets`, `sources`) filters `deleted_at IS NULL`. Tombstoned rows exist
//! only for the GC grace period (ADR-046 §8a.3) and must never be visible to
//! a reader; `mark_deleted`/`tombstoned_before`/`hard_delete`/`expired` are
//! the only methods that see them.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, QueryResult, Statement, Value,
};
use uuid::Uuid;

use super::{
    FacetField, FacetResult, FacetValue, LogAccessScope, LogQuery, LogSelection, LogSourceKind,
    MAX_FACET_VALUES,
};
use crate::chunk::{level_mask_for, ChunkLabels};
use crate::error::LogAggregatorError;
use crate::types::{ChunkMeta, LogSource};

/// Columns selected for a full manifest row, in the order [`row_to_manifest`]
/// reads them.
const MANIFEST_COLUMNS: &str = "id, seq, project_id, external_service_id, env, service, \
                                 container_id, deploy_id, node_id, node_name, started_at, \
                                 ended_at, storage_key, line_count, compressed_size_bytes, \
                                 format_version, level_mask, footer_offset, footer_len, bloom_len, \
                                 level_counts";

/// One manifest row: everything the planner needs to decide whether a chunk
/// is a candidate and, if so, where to range-GET its footer.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub id: Uuid,
    pub seq: i64,
    pub labels: ChunkLabels,
    pub storage_key: String,
    pub format_version: u16,
    pub level_mask: u16,
    pub footer_offset: Option<u64>,
    pub footer_len: Option<u32>,
    pub bloom_len: u32,
    pub line_count: u32,
    pub compressed_size_bytes: u32,
}

/// Keyset position for [`ManifestRepo::candidates`]: strictly after this in
/// `(ended_at DESC, id DESC)` order.
#[derive(Clone, Debug)]
pub struct ManifestCursor {
    pub ended_at: DateTime<Utc>,
    pub id: Uuid,
}

/// The `log_chunks` manifest repository.
pub struct ManifestRepo {
    db: Arc<DatabaseConnection>,
}

// ── Parameter binding ───────────────────────────────────────────────────

/// Accumulates positional parameters so no caller value is ever interpolated
/// into SQL text. Mirrors the pattern used by the retired
/// `TimescaleLogLineStore`.
struct Binder {
    values: Vec<Value>,
}

impl Binder {
    fn new() -> Self {
        Self { values: Vec::new() }
    }

    fn bind(&mut self, value: impl Into<Value>) -> String {
        self.values.push(value.into());
        format!("${}", self.values.len())
    }

    fn into_values(self) -> Vec<Value> {
        self.values
    }
}

// ── Predicate builders (pure, unit-tested) ─────────────────────────────

/// `WHERE` fragment for the authorization allow-list. `All` adds nothing;
/// `Allowed` with both lists empty is `FALSE`; otherwise an OR of the two
/// membership checks.
fn scope_condition(scope: &LogAccessScope, binder: &mut Binder) -> Option<String> {
    match scope {
        LogAccessScope::All => None,
        LogAccessScope::Allowed {
            project_ids,
            external_service_ids,
        } => Some(
            if project_ids.is_empty() && external_service_ids.is_empty() {
                "FALSE".to_string()
            } else {
                let projects = binder.bind(project_ids.clone());
                let services = binder.bind(external_service_ids.clone());
                format!(
                    "((external_service_id IS NULL AND project_id = ANY({projects}::int[])) \
                  OR (external_service_id = ANY({services}::int[])))"
                )
            },
        ),
    }
}

/// `WHERE` fragment for an explicit resource selection *within* the scope.
/// `Some` with both lists empty is `FALSE` — the caller asked for resources
/// that resolve to nothing, which is a well-formed empty answer.
fn selection_condition(selection: &Option<LogSelection>, binder: &mut Binder) -> Option<String> {
    let selection = selection.as_ref()?;
    Some(
        if selection.project_ids.is_empty() && selection.external_service_ids.is_empty() {
            "FALSE".to_string()
        } else {
            let projects = binder.bind(selection.project_ids.clone());
            let services = binder.bind(selection.external_service_ids.clone());
            format!(
                "((external_service_id IS NULL AND project_id = ANY({projects}::int[])) \
              OR (external_service_id = ANY({services}::int[])))"
            )
        },
    )
}

/// `WHERE` fragment narrowing to application (deployment) or managed-service
/// chunks. `Collected` (everything the caller can see) adds nothing.
fn source_condition(source: LogSourceKind) -> Option<&'static str> {
    match source {
        LogSourceKind::Application => Some("external_service_id IS NULL"),
        LogSourceKind::Service => Some("external_service_id IS NOT NULL"),
        LogSourceKind::Collected => None,
    }
}

/// `level_mask & wanted <> 0` — a chunk is a candidate if it may contain any
/// wanted level. `level_mask_for(&[])` is "all levels", so an unfiltered
/// query never excludes a chunk on this predicate alone.
fn level_condition(levels: &[crate::types::LogLevel], binder: &mut Binder) -> String {
    let wanted = level_mask_for(levels) as i16;
    let v = binder.bind(wanted);
    format!("(level_mask & {v}) <> 0")
}

/// Shared `WHERE` conditions for a [`LogQuery`]: liveness, time overlap,
/// authorization, source kind, selection and label filters. Level is
/// included only when `with_level` is set — `facets`/`sources` answer across
/// all levels, `candidates` does not.
fn build_conditions(query: &LogQuery, binder: &mut Binder, with_level: bool) -> Vec<String> {
    let mut conditions = vec!["deleted_at IS NULL".to_string()];

    let start = binder.bind(query.start_time);
    let end = binder.bind(query.end_time);
    conditions.push(format!("started_at <= {end} AND ended_at >= {start}"));

    if let Some(scope) = scope_condition(&query.scope, binder) {
        conditions.push(scope);
    }
    if let Some(source) = source_condition(query.source) {
        conditions.push(source.to_string());
    }
    if let Some(selection) = selection_condition(&query.selection, binder) {
        conditions.push(selection);
    }
    if !query.envs.is_empty() {
        let v = binder.bind(query.envs.clone());
        conditions.push(format!("env = ANY({v}::text[])"));
    }
    if !query.services.is_empty() {
        let v = binder.bind(query.services.clone());
        conditions.push(format!("service = ANY({v}::text[])"));
    }
    if !query.container_ids.is_empty() {
        let v = binder.bind(query.container_ids.clone());
        conditions.push(format!("container_id = ANY({v}::text[])"));
    }
    if !query.node_ids.is_empty() {
        let v = binder.bind(query.node_ids.clone());
        conditions.push(format!("node_id = ANY({v}::int[])"));
    }
    if let Some(deploy_id) = query.deploy_id {
        let v = binder.bind(deploy_id);
        conditions.push(format!("deploy_id = {v}"));
    }
    if let Some(seqs) = &query.chunk_seqs {
        let v = binder.bind(seqs.clone());
        conditions.push(format!("seq = ANY({v}::bigint[])"));
    }
    if with_level {
        conditions.push(level_condition(&query.levels, binder));
    }

    conditions
}

/// Keyset boundary: strictly after `after` in `(ended_at DESC, id DESC)`
/// order, i.e. `(ended_at, id) < (after.ended_at, after.id)`.
fn cursor_condition(after: &ManifestCursor, binder: &mut Binder) -> String {
    let ended_at = binder.bind(after.ended_at);
    let id = binder.bind(after.id);
    format!("(ended_at, id) < ({ended_at}, {id})")
}

// ── Row decoding ────────────────────────────────────────────────────────

fn row_to_manifest(row: &QueryResult) -> Result<Manifest, LogAggregatorError> {
    let project_id: i32 = row.try_get("", "project_id")?;
    let external_service_id: Option<i32> = row.try_get("", "external_service_id")?;
    let started_at: DateTime<Utc> = row.try_get("", "started_at")?;
    let ended_at: DateTime<Utc> = row.try_get("", "ended_at")?;
    let line_count: i32 = row.try_get("", "line_count")?;
    let level_mask: i16 = row.try_get("", "level_mask")?;
    let footer_offset: Option<i64> = row.try_get("", "footer_offset")?;
    let footer_len: Option<i32> = row.try_get("", "footer_len")?;
    let bloom_len: i32 = row.try_get("", "bloom_len")?;
    let format_version: i16 = row.try_get("", "format_version")?;
    let level_counts: Vec<i32> = row.try_get("", "level_counts").unwrap_or_default();
    let mut counts = [0u32; crate::chunk::LEVEL_COUNT];
    for (i, c) in level_counts
        .iter()
        .take(crate::chunk::LEVEL_COUNT)
        .enumerate()
    {
        counts[i] = (*c).max(0) as u32;
    }
    let compressed_size_bytes: i32 = row.try_get("", "compressed_size_bytes")?;

    Ok(Manifest {
        id: row.try_get("", "id")?,
        seq: row.try_get("", "seq")?,
        labels: ChunkLabels {
            project_id,
            external_service_id,
            env: row.try_get("", "env")?,
            service: row.try_get("", "service")?,
            container_id: row.try_get("", "container_id")?,
            deploy_id: row.try_get("", "deploy_id")?,
            node_id: row.try_get("", "node_id")?,
            node_name: row.try_get("", "node_name")?,
            started_at,
            ended_at,
            line_count: line_count.max(0) as u32,
            level_mask: level_mask.max(0) as u16,
            level_counts: counts,
        },
        storage_key: row.try_get("", "storage_key")?,
        format_version: format_version.max(0) as u16,
        level_mask: level_mask.max(0) as u16,
        footer_offset: footer_offset.map(|v| v.max(0) as u64),
        footer_len: footer_len.map(|v| v.max(0) as u32),
        bloom_len: bloom_len.max(0) as u32,
        line_count: line_count.max(0) as u32,
        compressed_size_bytes: compressed_size_bytes.max(0) as u32,
    })
}

/// Newest sealed event time for one container, across chunk rows (live or
/// tombstoned) and the durable collector position. `$1` = container_id.
pub(crate) const LATEST_SEALED_SQL: &str = "SELECT GREATEST( \
        (SELECT MAX(ended_at) FROM log_chunks WHERE container_id = $1), \
        (SELECT last_ts FROM log_collector_positions WHERE container_id = $1) \
     ) AS latest";

// ── The repository ─────────────────────────────────────────────────────

impl ManifestRepo {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    async fn query_all(
        &self,
        sql: String,
        values: Vec<Value>,
    ) -> Result<Vec<QueryResult>, LogAggregatorError> {
        self.db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                values,
            ))
            .await
            .map_err(LogAggregatorError::Database)
    }

    async fn query_manifests(
        &self,
        sql: String,
        values: Vec<Value>,
    ) -> Result<Vec<Manifest>, LogAggregatorError> {
        self.query_all(sql, values)
            .await?
            .iter()
            .map(row_to_manifest)
            .collect()
    }

    /// Planner step 1 (ADR-046 §3): live manifests overlapping the query's
    /// window, matching scope/selection/label/level filters, strictly after
    /// `after` when given, newest first.
    pub async fn candidates(
        &self,
        query: &LogQuery,
        after: Option<&ManifestCursor>,
        limit: u32,
    ) -> Result<Vec<Manifest>, LogAggregatorError> {
        let mut binder = Binder::new();
        let mut conditions = build_conditions(query, &mut binder, true);
        if let Some(after) = after {
            conditions.push(cursor_condition(after, &mut binder));
        }
        let limit_param = binder.bind(i64::from(limit));

        let sql = format!(
            "SELECT {MANIFEST_COLUMNS} FROM log_chunks WHERE {} \
             ORDER BY ended_at DESC, id DESC LIMIT {limit_param}",
            conditions.join(" AND ")
        );
        self.query_manifests(sql, binder.into_values()).await
    }

    /// Chunks of one container bracketing `at`, for reading raw context
    /// across a chunk edge: up to `n` ending at or before `at` (newest
    /// first) and up to `n` starting at or after `at` (oldest first).
    pub async fn around(
        &self,
        scope: &LogAccessScope,
        container_id: &str,
        at: DateTime<Utc>,
        n: u32,
    ) -> Result<(Vec<Manifest>, Vec<Manifest>), LogAggregatorError> {
        let mut before_binder = Binder::new();
        let mut before_conditions = vec!["deleted_at IS NULL".to_string()];
        if let Some(scope) = scope_condition(scope, &mut before_binder) {
            before_conditions.push(scope);
        }
        let container_param = before_binder.bind(container_id.to_string());
        before_conditions.push(format!("container_id = {container_param}"));
        let at_param = before_binder.bind(at);
        before_conditions.push(format!("ended_at <= {at_param}"));
        let limit_param = before_binder.bind(i64::from(n));
        let before_sql = format!(
            "SELECT {MANIFEST_COLUMNS} FROM log_chunks WHERE {} \
             ORDER BY ended_at DESC LIMIT {limit_param}",
            before_conditions.join(" AND ")
        );
        let before = self
            .query_manifests(before_sql, before_binder.into_values())
            .await?;

        let mut after_binder = Binder::new();
        let mut after_conditions = vec!["deleted_at IS NULL".to_string()];
        if let Some(scope) = scope_condition(scope, &mut after_binder) {
            after_conditions.push(scope);
        }
        let container_param = after_binder.bind(container_id.to_string());
        after_conditions.push(format!("container_id = {container_param}"));
        let at_param = after_binder.bind(at);
        after_conditions.push(format!("started_at >= {at_param}"));
        let limit_param = after_binder.bind(i64::from(n));
        let after_sql = format!(
            "SELECT {MANIFEST_COLUMNS} FROM log_chunks WHERE {} \
             ORDER BY started_at ASC LIMIT {limit_param}",
            after_conditions.join(" AND ")
        );
        let after = self
            .query_manifests(after_sql, after_binder.into_values())
            .await?;

        Ok((before, after))
    }

    /// The live chunk of `container_id` whose time span covers `at`, within
    /// scope — how a line is found again once the chunk its `line_id`
    /// named has been compacted away (the newest such chunk wins when
    /// spans overlap at a seal boundary).
    pub async fn containing(
        &self,
        scope: &LogAccessScope,
        container_id: &str,
        at: DateTime<Utc>,
    ) -> Result<Option<Manifest>, LogAggregatorError> {
        let mut binder = Binder::new();
        let mut conditions = vec!["deleted_at IS NULL".to_string()];
        if let Some(scope) = scope_condition(scope, &mut binder) {
            conditions.push(scope);
        }
        let container_param = binder.bind(container_id.to_string());
        conditions.push(format!("container_id = {container_param}"));
        let at_param = binder.bind(at);
        conditions.push(format!(
            "started_at <= {at_param} AND ended_at >= {at_param}"
        ));
        let sql = format!(
            "SELECT {MANIFEST_COLUMNS} FROM log_chunks WHERE {} \
             ORDER BY seq DESC LIMIT 1",
            conditions.join(" AND ")
        );
        Ok(self
            .query_manifests(sql, binder.into_values())
            .await?
            .into_iter()
            .next())
    }

    /// A single live manifest by its stable sequence number, within scope.
    pub async fn by_seq(
        &self,
        scope: &LogAccessScope,
        seq: i64,
    ) -> Result<Option<Manifest>, LogAggregatorError> {
        let mut binder = Binder::new();
        let mut conditions = vec!["deleted_at IS NULL".to_string()];
        if let Some(scope) = scope_condition(scope, &mut binder) {
            conditions.push(scope);
        }
        let seq_param = binder.bind(seq);
        conditions.push(format!("seq = {seq_param}"));

        let sql = format!(
            "SELECT {MANIFEST_COLUMNS} FROM log_chunks WHERE {} LIMIT 1",
            conditions.join(" AND ")
        );
        Ok(self
            .query_manifests(sql, binder.into_values())
            .await?
            .into_iter()
            .next())
    }

    /// Distinct values + `SUM(line_count)` per requested field, within the
    /// query's scope and window (level filter excluded — facets answer
    /// across all levels so the picker can offer values the current filter
    /// hides). [`FacetField::Level`] and [`FacetField::Stream`] are answered
    /// from `level_mask` rather than `GROUP BY`, since neither is a column:
    /// the level counts are an over-estimate (a chunk's `line_count` is
    /// attributed to every level bit it carries, not just the lines at that
    /// level), and the stream split returns the same total line count under
    /// both `stdout` and `stderr` — manifests do not record per-stream
    /// counts. Both are documented estimates, not exact counts.
    pub async fn facets(
        &self,
        query: &LogQuery,
        fields: &[FacetField],
    ) -> Result<FacetResult, LogAggregatorError> {
        let mut result = FacetResult::default();

        for field in fields {
            match field {
                FacetField::Level => {
                    let mut values = Vec::new();
                    for level in [
                        crate::types::LogLevel::Trace,
                        crate::types::LogLevel::Debug,
                        crate::types::LogLevel::Info,
                        crate::types::LogLevel::Warn,
                        crate::types::LogLevel::Error,
                    ] {
                        let bit = crate::chunk::level_bit(level) as i16;
                        // v2 rows carry exact per-level counts
                        // (`level_counts[idx]`, 1-based in SQL); v1 rows
                        // have an empty array and fall back to the mask
                        // estimate (whole chunk attributed to each bit).
                        let idx = i32::from(crate::chunk::level_to_u8(level)) + 1;
                        let mut binder = Binder::new();
                        let mut conditions = build_conditions(query, &mut binder, false);
                        let bit_param = binder.bind(bit);
                        let idx_param = binder.bind(idx);
                        conditions.push(format!("(level_mask & {bit_param}) <> 0"));
                        let sql = format!(
                            "SELECT COALESCE(SUM(CASE WHEN cardinality(level_counts) >= {idx_param} \
                                    THEN level_counts[{idx_param}] ELSE line_count END), 0)::bigint AS count \
                             FROM log_chunks WHERE {}",
                            conditions.join(" AND ")
                        );
                        let rows = self.query_all(sql, binder.into_values()).await?;
                        let count: i64 = rows
                            .first()
                            .map(|r| r.try_get::<i64>("", "count"))
                            .transpose()?
                            .unwrap_or(0);
                        if count > 0 {
                            values.push(FacetValue {
                                value: level.to_string(),
                                count,
                            });
                        }
                    }
                    values.sort_by_key(|v| std::cmp::Reverse(v.count));
                    result.fields.insert(field.as_str().to_string(), values);
                }
                FacetField::Stream => {
                    let mut binder = Binder::new();
                    let conditions = build_conditions(query, &mut binder, false);
                    let sql = format!(
                        "SELECT COALESCE(SUM(line_count), 0)::bigint AS count \
                         FROM log_chunks WHERE {}",
                        conditions.join(" AND ")
                    );
                    let rows = self.query_all(sql, binder.into_values()).await?;
                    let count: i64 = rows
                        .first()
                        .map(|r| r.try_get::<i64>("", "count"))
                        .transpose()?
                        .unwrap_or(0);
                    result.fields.insert(
                        field.as_str().to_string(),
                        vec![
                            FacetValue {
                                value: "stdout".to_string(),
                                count,
                            },
                            FacetValue {
                                value: "stderr".to_string(),
                                count,
                            },
                        ],
                    );
                }
                other => {
                    let mut binder = Binder::new();
                    let mut conditions = build_conditions(query, &mut binder, false);
                    let column = other.column();
                    conditions.push(format!("{column} IS NOT NULL"));
                    let limit = binder.bind(i64::from(MAX_FACET_VALUES) + 1);
                    let sql = format!(
                        "SELECT {column}::text AS value, COALESCE(SUM(line_count), 0)::bigint AS count \
                         FROM log_chunks WHERE {} GROUP BY {column} \
                         ORDER BY count DESC, value ASC LIMIT {limit}",
                        conditions.join(" AND ")
                    );
                    let rows = self.query_all(sql, binder.into_values()).await?;
                    let mut values: Vec<FacetValue> = rows
                        .iter()
                        .map(|row| {
                            Ok(FacetValue {
                                value: row.try_get::<String>("", "value")?,
                                count: row.try_get::<i64>("", "count")?,
                            })
                        })
                        .collect::<Result<_, LogAggregatorError>>()?;
                    if values.len() > MAX_FACET_VALUES as usize {
                        values.truncate(MAX_FACET_VALUES as usize);
                        result.partial = true;
                    }
                    result.fields.insert(other.as_str().to_string(), values);
                }
            }
        }

        Ok(result)
    }

    /// Distinct `(container_id, service, node_id, node_name)` in the query's
    /// scope and window, independent of level. Powers the source picker's
    /// filter dropdowns.
    pub async fn sources(&self, query: &LogQuery) -> Result<Vec<LogSource>, LogAggregatorError> {
        let mut binder = Binder::new();
        let conditions = build_conditions(query, &mut binder, false);
        let limit = binder.bind(i64::from(MAX_FACET_VALUES));

        let sql = format!(
            "SELECT DISTINCT ON (container_id) container_id, service, node_id, node_name \
             FROM log_chunks WHERE {} ORDER BY container_id, ended_at DESC LIMIT {limit}",
            conditions.join(" AND ")
        );
        let rows = self.query_all(sql, binder.into_values()).await?;
        rows.iter()
            .map(|row| {
                Ok(LogSource {
                    container_id: row.try_get("", "container_id")?,
                    service: row.try_get("", "service")?,
                    node_id: row.try_get("", "node_id")?,
                    node_name: row.try_get("", "node_name")?,
                })
            })
            .collect()
    }

    /// Record that every line of `container_id` up to `ended_at` is sealed.
    /// `log_collector_positions` outlives the chunk rows themselves (GC never
    /// touches it), so a restart after retention or a purge removed a
    /// container's every chunk still resumes after the last sealed line
    /// instead of replaying — and resurrecting — the purged history.
    async fn advance_position(
        &self,
        container_id: &str,
        ended_at: DateTime<Utc>,
    ) -> Result<(), LogAggregatorError> {
        self.query_all(
            "INSERT INTO log_collector_positions (container_id, last_ts, updated_at) \
             VALUES ($1, $2, now()) \
             ON CONFLICT (container_id) DO UPDATE \
             SET last_ts = GREATEST(log_collector_positions.last_ts, EXCLUDED.last_ts), \
                 updated_at = now()"
                .to_string(),
            vec![container_id.into(), ended_at.into()],
        )
        .await?;
        Ok(())
    }

    /// Event time of the newest line ever sealed for a container, if any:
    /// the newest stored chunk (tombstoned rows included) or, once GC has
    /// removed even those, the collector position recorded at seal time.
    /// Resume must never replay history a retention sweep or purge already
    /// accounted for.
    pub async fn latest_ended_at(
        &self,
        container_id: &str,
    ) -> Result<Option<DateTime<Utc>>, LogAggregatorError> {
        let rows = self
            .query_all(LATEST_SEALED_SQL.to_string(), vec![container_id.into()])
            .await?;
        match rows.first() {
            Some(row) => Ok(row.try_get::<Option<DateTime<Utc>>>("", "latest")?),
            None => Ok(None),
        }
    }

    /// Insert a v2 manifest row. Storage-key uniqueness applies only to v2
    /// chunks because legacy v1 metadata may contain multiple rows referring
    /// to the same object. A crash-and-replay of a v2 key is absorbed and
    /// returns the existing v2 row's `seq` (ADR-046 §8a.2).
    pub async fn insert(&self, meta: &ChunkMeta) -> Result<i64, LogAggregatorError> {
        let sql = "INSERT INTO log_chunks \
                    (id, project_id, external_service_id, env, service, container_id, \
                     deploy_id, node_id, node_name, started_at, ended_at, storage_key, \
                     line_count, compressed_size_bytes, has_errors, line_offsets, \
                     format_version, level_mask, footer_offset, footer_len, bloom_len, \
                     level_counts) \
                    VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22) \
                    ON CONFLICT (storage_key) WHERE format_version >= 2 DO NOTHING \
                    RETURNING seq"
            .to_string();
        let values: Vec<Value> = vec![
            meta.id.into(),
            meta.project_id.into(),
            meta.external_service_id.into(),
            meta.env.clone().into(),
            meta.service.clone().into(),
            meta.container_id.clone().into(),
            meta.deploy_id.into(),
            meta.node_id.into(),
            meta.node_name.clone().into(),
            meta.started_at.into(),
            meta.ended_at.into(),
            meta.storage_key.clone().into(),
            meta.line_count.into(),
            meta.compressed_size_bytes.into(),
            meta.has_errors.into(),
            meta.line_offsets.clone().into(),
            (meta.format_version as i16).into(),
            (meta.level_mask as i16).into(),
            meta.footer_offset.map(|v| v as i64).into(),
            meta.footer_len.map(|v| v as i32).into(),
            (meta.bloom_len as i32).into(),
            meta.level_counts
                .iter()
                .map(|c| *c as i32)
                .collect::<Vec<i32>>()
                .into(),
        ];

        let rows = self.query_all(sql, values).await?;
        self.advance_position(&meta.container_id, meta.ended_at)
            .await?;
        if let Some(row) = rows.first() {
            return Ok(row.try_get::<i64>("", "seq")?);
        }

        // Conflict: another writer already inserted this v2 storage_key.
        // Fetch that v2 row rather than selecting legacy metadata that happens
        // to refer to the same object.
        let rows = self
            .query_all(
                "SELECT seq FROM log_chunks \
                 WHERE storage_key = $1 AND format_version >= 2"
                    .to_string(),
                vec![meta.storage_key.clone().into()],
            )
            .await?;
        rows.first()
            .map(|row| row.try_get::<i64>("", "seq"))
            .transpose()?
            .ok_or_else(|| LogAggregatorError::ManifestConflictUnresolved {
                storage_key: meta.storage_key.clone(),
            })
    }

    /// Tombstone: set `deleted_at = now()` for `ids`, returning how many rows
    /// were newly tombstoned.
    pub async fn mark_deleted(&self, ids: &[Uuid]) -> Result<u64, LogAggregatorError> {
        if ids.is_empty() {
            return Ok(0);
        }
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE log_chunks SET deleted_at = now() \
                 WHERE id = ANY($1::uuid[]) AND deleted_at IS NULL",
                [Value::from(ids.to_vec())],
            ))
            .await
            .map(|r| r.rows_affected())
            .map_err(LogAggregatorError::Database)
    }

    /// Like [`Self::mark_deleted`] but returns the `seq` of every row newly
    /// tombstoned, so the caller can forget them in the line index.
    pub async fn mark_deleted_returning_seqs(
        &self,
        ids: &[Uuid],
    ) -> Result<Vec<i64>, LogAggregatorError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = self
            .query_all(
                "UPDATE log_chunks SET deleted_at = now() \
                 WHERE id = ANY($1::uuid[]) AND deleted_at IS NULL RETURNING seq"
                    .to_string(),
                vec![Value::from(ids.to_vec())],
            )
            .await?;
        rows.iter()
            .map(|r| {
                r.try_get::<i64>("", "seq")
                    .map_err(LogAggregatorError::Database)
            })
            .collect()
    }

    /// Tombstoned rows past `cutoff` (the GC grace period), oldest first.
    /// A shared object remains protected while any manifest with the same
    /// storage key is live or still within its grace period.
    pub async fn tombstoned_before(
        &self,
        cutoff: DateTime<Utc>,
        limit: u32,
    ) -> Result<Vec<(Uuid, String)>, LogAggregatorError> {
        let rows = self
            .query_all(
                "SELECT candidate.id, candidate.storage_key FROM log_chunks AS candidate \
                 WHERE candidate.deleted_at IS NOT NULL AND candidate.deleted_at < $1 \
                   AND NOT EXISTS ( \
                       SELECT 1 FROM log_chunks AS sibling \
                       WHERE sibling.storage_key = candidate.storage_key \
                         AND (sibling.deleted_at IS NULL OR sibling.deleted_at >= $1) \
                   ) \
                 ORDER BY candidate.deleted_at ASC LIMIT $2"
                    .to_string(),
                vec![cutoff.into(), i64::from(limit).into()],
            )
            .await?;
        rows.iter()
            .map(|row| {
                Ok((
                    row.try_get::<Uuid>("", "id")?,
                    row.try_get::<String>("", "storage_key")?,
                ))
            })
            .collect()
    }

    /// Permanently remove tombstoned rows. Refuses to delete a row that is
    /// not tombstoned — hard delete is only ever the second half of the GC
    /// sweep, never a shortcut around it.
    pub async fn hard_delete(&self, ids: &[Uuid]) -> Result<u64, LogAggregatorError> {
        if ids.is_empty() {
            return Ok(0);
        }
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "DELETE FROM log_chunks WHERE id = ANY($1::uuid[]) AND deleted_at IS NOT NULL",
                [Value::from(ids.to_vec())],
            ))
            .await
            .map(|r| r.rows_affected())
            .map_err(LogAggregatorError::Database)
    }

    /// Live chunks past their retention cutoff, oldest first.
    pub async fn expired(
        &self,
        cutoff: DateTime<Utc>,
        limit: u32,
    ) -> Result<Vec<Manifest>, LogAggregatorError> {
        let sql = format!(
            "SELECT {MANIFEST_COLUMNS} FROM log_chunks \
             WHERE deleted_at IS NULL AND ended_at < $1 ORDER BY ended_at ASC LIMIT $2"
        );
        self.query_manifests(sql, vec![cutoff.into(), i64::from(limit).into()])
            .await
    }

    /// Live chunks of one container ending within `[start, end)`, oldest
    /// first — the compactor's input for one stream-day.
    pub async fn for_container_window(
        &self,
        container_id: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<Manifest>, LogAggregatorError> {
        let sql = format!(
            "SELECT {MANIFEST_COLUMNS} FROM log_chunks \
             WHERE deleted_at IS NULL AND container_id = $1 \
               AND ended_at >= $2 AND ended_at < $3 \
             ORDER BY ended_at ASC"
        );
        self.query_manifests(sql, vec![container_id.into(), start.into(), end.into()])
            .await
    }

    /// Container ids with at least `min_chunks` live chunks ending within
    /// `[start, end)` — compaction candidates.
    pub async fn fragmented_containers(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        min_chunks: u32,
        limit: u32,
    ) -> Result<Vec<String>, LogAggregatorError> {
        let rows = self
            .query_all(
                "SELECT container_id FROM log_chunks \
                 WHERE deleted_at IS NULL AND ended_at >= $1 AND ended_at < $2 \
                 GROUP BY container_id HAVING COUNT(*) >= $3 LIMIT $4"
                    .to_string(),
                vec![
                    start.into(),
                    end.into(),
                    i64::from(min_chunks).into(),
                    i64::from(limit).into(),
                ],
            )
            .await?;
        rows.iter()
            .map(|row| Ok(row.try_get::<String>("", "container_id")?))
            .collect()
    }

    /// Tombstone every live chunk of `project_id` ending before `before`;
    /// returns the summed `line_count` of the chunks tombstoned, for the
    /// operator-initiated purge endpoint's response.
    /// Tombstone every live chunk of `project_id` that ended before
    /// `before`. Returns `(lines purged, seqs of the tombstoned chunks)`;
    /// the seqs let the caller forget those rows in the line index.
    pub async fn purge_project(
        &self,
        project_id: i32,
        before: DateTime<Utc>,
    ) -> Result<(u64, Vec<i64>), LogAggregatorError> {
        let rows = self
            .query_all(
                "WITH purged AS ( \
                     UPDATE log_chunks SET deleted_at = now() \
                     WHERE deleted_at IS NULL AND project_id = $1 AND ended_at < $2 \
                     RETURNING seq, line_count \
                 ) SELECT COALESCE(SUM(line_count), 0)::bigint AS total, \
                          COALESCE(array_agg(seq), '{}')::bigint[] AS seqs FROM purged"
                    .to_string(),
                vec![project_id.into(), before.into()],
            )
            .await?;
        let total: i64 = rows
            .first()
            .map(|r| r.try_get::<i64>("", "total"))
            .transpose()?
            .unwrap_or(0);
        let seqs: Vec<i64> = rows
            .first()
            .map(|r| r.try_get::<Vec<i64>>("", "seqs"))
            .transpose()?
            .unwrap_or_default();
        Ok((total.max(0) as u64, seqs))
    }

    /// Which of `keys` already have a manifest row (live or tombstoned).
    /// Used by the reconcile sweep to find orphaned objects.
    pub async fn known_storage_keys(
        &self,
        keys: &[String],
    ) -> Result<std::collections::HashSet<String>, LogAggregatorError> {
        if keys.is_empty() {
            return Ok(Default::default());
        }
        let rows = self
            .query_all(
                "SELECT storage_key FROM log_chunks WHERE storage_key = ANY($1)".to_string(),
                vec![keys.to_vec().into()],
            )
            .await?;
        rows.iter()
            .map(|r| r.try_get::<String>("", "storage_key").map_err(Into::into))
            .collect()
    }

    /// Live manifests, oldest first, paged by `seq` — for the reconcile
    /// sweep's "does the object still exist" pass.
    pub async fn live_after_seq(
        &self,
        after_seq: i64,
        limit: u32,
    ) -> Result<Vec<Manifest>, LogAggregatorError> {
        let sql = format!(
            "SELECT {MANIFEST_COLUMNS} FROM log_chunks \
             WHERE deleted_at IS NULL AND seq > $1 ORDER BY seq ASC LIMIT $2"
        );
        let rows = self
            .query_all(sql, vec![after_seq.into(), (limit as i64).into()])
            .await?;
        rows.iter().map(row_to_manifest).collect()
    }

    /// ADR-047 §4: record that chunk `seq` is fully present in the line
    /// index.
    pub async fn mark_indexed(&self, seq: i64) -> Result<(), LogAggregatorError> {
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE log_chunks SET indexed_at = now() WHERE seq = $1 AND indexed_at IS NULL",
                [seq.into()],
            ))
            .await
            .map(|_| ())
            .map_err(LogAggregatorError::Database)
    }

    /// Live manifests not yet in the line index, newest first (ADR-047 §6:
    /// the reindexer works from the present backwards so the explorer's
    /// "index current to" watermark moves immediately), paged by `seq`.
    pub async fn unindexed_before_seq(
        &self,
        before_seq: i64,
        limit: u32,
    ) -> Result<Vec<Manifest>, LogAggregatorError> {
        let sql = format!(
            "SELECT {MANIFEST_COLUMNS} FROM log_chunks \
             WHERE deleted_at IS NULL AND indexed_at IS NULL AND seq < $1 \
             ORDER BY seq DESC LIMIT $2"
        );
        let rows = self
            .query_all(sql, vec![before_seq.into(), (limit as i64).into()])
            .await?;
        rows.iter().map(row_to_manifest).collect()
    }

    /// Live chunk count and how many of them are indexed — the index
    /// coverage figure for the capabilities endpoint.
    pub async fn index_coverage(&self) -> Result<(u64, u64), LogAggregatorError> {
        let rows = self
            .query_all(
                "SELECT COUNT(*)::bigint AS live, \
                        COUNT(indexed_at)::bigint AS indexed \
                 FROM log_chunks WHERE deleted_at IS NULL"
                    .to_string(),
                vec![],
            )
            .await?;
        match rows.first() {
            Some(row) => {
                let live: i64 = row.try_get("", "live")?;
                let indexed: i64 = row.try_get("", "indexed")?;
                Ok((live.max(0) as u64, indexed.max(0) as u64))
            }
            None => Ok((0, 0)),
        }
    }

    /// Record which store the line index lives in (ADR-047 §8). Returns
    /// `true` when the backend changed since the last start, in which case
    /// every live chunk has been marked un-indexed so the reindexer rebuilds
    /// the index in the new store — rows left in the old store are never
    /// queried again and age out by its own retention.
    pub async fn activate_index_backend(&self, backend: &str) -> Result<bool, LogAggregatorError> {
        let rows = self
            .query_all(
                "INSERT INTO log_line_index_state (id, backend, changed_at) \
                 VALUES (1, $1, now()) \
                 ON CONFLICT (id) DO UPDATE \
                     SET backend = EXCLUDED.backend, changed_at = now() \
                     WHERE log_line_index_state.backend <> EXCLUDED.backend \
                 RETURNING backend"
                    .to_string(),
                vec![backend.into()],
            )
            .await?;
        // `RETURNING` yields a row only when the insert or the conditional
        // update actually wrote — i.e. first start or a real change.
        let changed = !rows.is_empty();
        if changed {
            self.db
                .execute(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "UPDATE log_chunks SET indexed_at = NULL \
                     WHERE deleted_at IS NULL AND indexed_at IS NOT NULL",
                    vec![],
                ))
                .await
                .map_err(LogAggregatorError::Database)?;
        }
        Ok(changed)
    }

    /// Durably record that `seqs` must be forgotten from the line index
    /// (ADR-047 §8a): retired manifests (compacted, purged, or tombstoned by
    /// retention) whose rows a caller has not yet confirmed removed from the
    /// index. Idempotent — a `seq` already queued keeps its original
    /// `requested_at` and attempt count.
    ///
    /// This is the durability half of the forget path: a caller enqueues
    /// *before* attempting the immediate forget, so a crash between the two
    /// still leaves [`crate::services::forget_sweeper::ForgetSweeper`]
    /// something to retry. A successful immediate forget resolves the entry
    /// right away ([`Self::resolve_forgets`]); the sweeper only ever sees
    /// the ones that failed or that never got to run.
    pub async fn enqueue_forget(&self, seqs: &[i64]) -> Result<(), LogAggregatorError> {
        if seqs.is_empty() {
            return Ok(());
        }
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO log_line_forget_backlog (chunk_seq) \
                 SELECT * FROM unnest($1::bigint[]) \
                 ON CONFLICT (chunk_seq) DO NOTHING",
                vec![Value::from(seqs.to_vec())],
            ))
            .await
            .map_err(LogAggregatorError::Database)?;
        Ok(())
    }

    /// Oldest-first page of chunks still waiting to be forgotten from the
    /// line index.
    pub async fn pending_forgets(&self, limit: u32) -> Result<Vec<i64>, LogAggregatorError> {
        let rows = self
            .query_all(
                "SELECT chunk_seq FROM log_line_forget_backlog \
                 ORDER BY requested_at ASC LIMIT $1"
                    .to_string(),
                vec![Value::from(limit as i64)],
            )
            .await?;
        rows.iter()
            .map(|r| {
                r.try_get::<i64>("", "chunk_seq")
                    .map_err(LogAggregatorError::Database)
            })
            .collect()
    }

    /// Confirmed forgotten: drop the backlog entries.
    pub async fn resolve_forgets(&self, seqs: &[i64]) -> Result<(), LogAggregatorError> {
        if seqs.is_empty() {
            return Ok(());
        }
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "DELETE FROM log_line_forget_backlog WHERE chunk_seq = ANY($1::bigint[])",
                vec![Value::from(seqs.to_vec())],
            ))
            .await
            .map_err(LogAggregatorError::Database)?;
        Ok(())
    }

    /// The attempt failed again: keep the entry, bump the counter, and keep
    /// the error for diagnostics. Never drops a row on its own — only
    /// [`Self::resolve_forgets`] does, and only once the index itself
    /// confirms the rows are gone.
    ///
    /// Upsert, not a plain UPDATE: the immediate forget can fail on the same
    /// call where [`Self::enqueue_forget`] itself also failed (e.g. a
    /// transient Postgres blip), in which case no backlog row exists yet to
    /// UPDATE and the retry would be silently lost — every call site enqueues
    /// best-effort before attempting, but this is the one place that must
    /// leave a durable row regardless of whether that enqueue landed.
    pub async fn record_forget_failure(
        &self,
        seqs: &[i64],
        error: &str,
    ) -> Result<(), LogAggregatorError> {
        if seqs.is_empty() {
            return Ok(());
        }
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO log_line_forget_backlog \
                     (chunk_seq, attempts, last_error, last_attempted_at) \
                 SELECT s, 1, $2, now() FROM unnest($1::bigint[]) AS s \
                 ON CONFLICT (chunk_seq) DO UPDATE \
                 SET attempts = log_line_forget_backlog.attempts + 1, \
                     last_error = EXCLUDED.last_error, \
                     last_attempted_at = EXCLUDED.last_attempted_at",
                vec![Value::from(seqs.to_vec()), Value::from(error)],
            ))
            .await
            .map_err(LogAggregatorError::Database)?;
        Ok(())
    }

    /// Count of chunks still waiting on a confirmed line-index forget — the
    /// capabilities endpoint's "forget backlog" figure, so a stuck Cloud
    /// outage is visible rather than a silent, slowly-growing discrepancy.
    pub async fn forget_backlog_size(&self) -> Result<u64, LogAggregatorError> {
        let rows = self
            .query_all(
                "SELECT COUNT(*)::bigint AS n FROM log_line_forget_backlog".to_string(),
                vec![],
            )
            .await?;
        match rows.first() {
            Some(row) => {
                let n: i64 = row.try_get("", "n")?;
                Ok(n.max(0) as u64)
            }
            None => Ok(0),
        }
    }

    /// Total live bytes and chunk count — the "log storage used" figure.
    pub async fn usage(&self) -> Result<(u64, u64), LogAggregatorError> {
        let rows = self
            .query_all(
                "SELECT COALESCE(SUM(compressed_size_bytes), 0)::bigint AS bytes, \
                        COUNT(*)::bigint AS chunks \
                 FROM log_chunks WHERE deleted_at IS NULL"
                    .to_string(),
                vec![],
            )
            .await?;
        match rows.first() {
            Some(row) => {
                let bytes: i64 = row.try_get("", "bytes")?;
                let chunks: i64 = row.try_get("", "chunks")?;
                Ok((bytes.max(0) as u64, chunks.max(0) as u64))
            }
            None => Ok((0, 0)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{LogSourceKind, MAX_PAGE_SIZE};
    use crate::types::LogLevel;

    fn query(scope: LogAccessScope) -> LogQuery {
        LogQuery {
            scope,
            start_time: "2026-01-01T00:00:00Z".parse().unwrap(),
            end_time: "2026-01-02T00:00:00Z".parse().unwrap(),
            source: LogSourceKind::Collected,
            selection: None,
            levels: vec![],
            envs: vec![],
            services: vec![],
            container_ids: vec![],
            node_ids: vec![],
            deploy_id: None,
            text: None,
            before: None,
            limit: MAX_PAGE_SIZE,
            context_lines: 0,
            attrs: Vec::new(),
            chunk_seqs: None,
        }
    }

    #[test]
    fn all_scope_adds_no_authorization_predicate() {
        let mut binder = Binder::new();
        assert!(scope_condition(&LogAccessScope::All, &mut binder).is_none());
    }

    #[test]
    fn allowed_scope_with_values_emits_an_or_predicate() {
        let mut binder = Binder::new();
        let condition = scope_condition(
            &LogAccessScope::Allowed {
                project_ids: vec![1, 2],
                external_service_ids: vec![7],
            },
            &mut binder,
        )
        .unwrap();
        assert!(condition.contains("project_id = ANY"));
        assert!(condition.contains("external_service_id"));
        assert_eq!(binder.into_values().len(), 2);
    }

    #[test]
    fn allowed_scope_with_both_lists_empty_is_literal_false() {
        let mut binder = Binder::new();
        let condition = scope_condition(
            &LogAccessScope::Allowed {
                project_ids: vec![],
                external_service_ids: vec![],
            },
            &mut binder,
        )
        .unwrap();
        assert_eq!(condition, "FALSE");
        assert!(
            binder.into_values().is_empty(),
            "no parameters bound for a literal FALSE"
        );
    }

    #[test]
    fn selection_none_adds_no_predicate() {
        let mut binder = Binder::new();
        assert!(selection_condition(&None, &mut binder).is_none());
    }

    #[test]
    fn selection_with_both_lists_empty_is_literal_false() {
        let mut binder = Binder::new();
        let condition = selection_condition(
            &Some(LogSelection {
                project_ids: vec![],
                external_service_ids: vec![],
            }),
            &mut binder,
        )
        .unwrap();
        assert_eq!(condition, "FALSE");
    }

    #[test]
    fn selection_with_values_narrows_within_scope() {
        let mut binder = Binder::new();
        let condition = selection_condition(
            &Some(LogSelection {
                project_ids: vec![1],
                external_service_ids: vec![],
            }),
            &mut binder,
        )
        .unwrap();
        assert!(condition.contains("project_id = ANY"));
    }

    #[test]
    fn source_kind_narrows_to_the_right_family() {
        assert_eq!(
            source_condition(LogSourceKind::Application),
            Some("external_service_id IS NULL")
        );
        assert_eq!(
            source_condition(LogSourceKind::Service),
            Some("external_service_id IS NOT NULL")
        );
        assert_eq!(source_condition(LogSourceKind::Collected), None);
    }

    #[test]
    fn level_condition_masks_on_wanted_bits() {
        let mut binder = Binder::new();
        let condition = level_condition(&[LogLevel::Error, LogLevel::Warn], &mut binder);
        assert_eq!(condition, "(level_mask & $1) <> 0");
        let values = binder.into_values();
        assert_eq!(values.len(), 1);
        // ERROR | WARN = 0b11000 = 24.
        assert_eq!(
            format!("{:?}", values[0]),
            format!("{:?}", Value::from(24_i16))
        );
    }

    #[test]
    fn level_condition_empty_wants_all_levels() {
        let mut binder = Binder::new();
        let condition = level_condition(&[], &mut binder);
        assert_eq!(condition, "(level_mask & $1) <> 0");
        let values = binder.into_values();
        assert_eq!(
            format!("{:?}", values[0]),
            format!("{:?}", Value::from(31_i16))
        );
    }

    #[test]
    fn cursor_condition_orders_strictly_after_in_desc_order() {
        let mut binder = Binder::new();
        let after = ManifestCursor {
            ended_at: "2026-01-01T12:00:00Z".parse().unwrap(),
            id: Uuid::nil(),
        };
        let condition = cursor_condition(&after, &mut binder);
        assert_eq!(condition, "(ended_at, id) < ($1, $2)");
        assert_eq!(binder.into_values().len(), 2);
    }

    #[test]
    fn build_conditions_always_filters_live_rows() {
        let mut binder = Binder::new();
        let conditions = build_conditions(&query(LogAccessScope::All), &mut binder, true);
        assert!(conditions.contains(&"deleted_at IS NULL".to_string()));
    }

    #[test]
    fn build_conditions_without_level_omits_the_level_predicate() {
        let mut binder = Binder::new();
        let conditions = build_conditions(&query(LogAccessScope::All), &mut binder, false);
        assert!(!conditions.iter().any(|c| c.contains("level_mask &")));
    }

    #[test]
    fn build_conditions_with_level_includes_the_level_predicate() {
        let mut binder = Binder::new();
        let conditions = build_conditions(&query(LogAccessScope::All), &mut binder, true);
        assert!(conditions.iter().any(|c| c.contains("level_mask &")));
    }

    #[test]
    fn no_filter_value_is_interpolated_into_sql() {
        let mut q = query(LogAccessScope::Allowed {
            project_ids: vec![1],
            external_service_ids: vec![],
        });
        q.selection = Some(LogSelection {
            project_ids: vec![1],
            external_service_ids: vec![],
        });
        q.envs = vec!["'; DROP TABLE log_chunks; --".to_string()];
        q.services = vec!["web".to_string()];
        q.container_ids = vec!["abc".to_string()];

        let mut binder = Binder::new();
        let conditions = build_conditions(&q, &mut binder, true);
        let sql = conditions.join(" AND ");
        assert!(!sql.contains("DROP TABLE"), "values must be bound: {sql}");
        assert!(!sql.contains("web"), "values must be bound: {sql}");
    }

    // ── Integration: insert → candidates → mark_deleted → tombstoned_before
    // → hard_delete round trip. Needs a live Postgres (via testcontainers);
    // run with `cargo test -p temps-log-aggregator --lib
    // store::manifest::tests::insert_candidates_delete_round_trip -- --ignored`
    // when Docker is available. Skips gracefully (rather than failing) when
    // it is not, matching `services::metadata`'s existing test convention.

    fn test_chunk_meta(project_id: i32, container_id: &str) -> ChunkMeta {
        let now = Utc::now();
        ChunkMeta {
            id: Uuid::new_v4(),
            project_id,
            external_service_id: None,
            env: "prod".to_string(),
            service: "api".to_string(),
            container_id: container_id.to_string(),
            deploy_id: None,
            node_id: None,
            node_name: None,
            started_at: now - chrono::Duration::minutes(5),
            ended_at: now,
            storage_key: format!("test/{}/{}", project_id, Uuid::new_v4()),
            line_count: 10,
            compressed_size_bytes: 512,
            has_errors: false,
            line_offsets: vec![0],
            format_version: 2,
            level_mask: crate::chunk::level_bit(LogLevel::Info),
            level_counts: vec![],
            footer_offset: Some(1024),
            footer_len: Some(2048),
            bloom_len: 512,
        }
    }

    async fn legacy_regression_database() -> Option<temps_database::test_utils::TestDatabase> {
        if std::env::var_os("TEMPS_TEST_DATABASE_URL").is_none() {
            let available = match bollard::Docker::connect_with_local_defaults() {
                Ok(docker) => docker.ping().await.is_ok(),
                Err(_) => false,
            };
            if !available {
                eprintln!("Skipping legacy manifest regression: Docker unavailable");
                return None;
            }
        }
        Some(
            temps_database::test_utils::TestDatabase::with_migrations()
                .await
                .expect("apply migrations for legacy manifest regression"),
        )
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn legacy_duplicates_preserve_v2_replay_and_shared_object_gc() {
        let Some(db) = legacy_regression_database().await else {
            return;
        };
        let repo = ManifestRepo::new(db.connection_arc());
        let metadata = crate::services::LogMetadataService::new(db.connection_arc());
        let mut legacy = test_chunk_meta(990_003, "legacy-shared-object");
        legacy.format_version = 1;
        legacy.footer_offset = None;
        legacy.footer_len = None;
        metadata.insert_chunk_meta(&legacy).await.unwrap();
        let first_id = legacy.id;
        legacy.id = Uuid::new_v4();
        legacy.started_at += chrono::Duration::seconds(30);
        legacy.ended_at += chrono::Duration::seconds(30);
        legacy.compressed_size_bytes += 20;
        metadata.insert_chunk_meta(&legacy).await.unwrap();

        // Even if a legacy row has this key, replay must resolve the v2 seq.
        let mut current = test_chunk_meta(990_003, "legacy-shared-object");
        current.storage_key = legacy.storage_key.clone();
        let seq = repo.insert(&current).await.unwrap();
        let current_id = current.id;
        current.id = Uuid::new_v4();
        assert_eq!(repo.insert(&current).await.unwrap(), seq);
        let rows = repo
            .query_all(
                "SELECT seq FROM log_chunks WHERE storage_key = $1".to_string(),
                vec![legacy.storage_key.clone().into()],
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 3, "replay must not duplicate the v2 manifest");

        let cutoff = Utc::now() - chrono::Duration::hours(1);
        let old = cutoff - chrono::Duration::seconds(1);
        repo.query_all(
            "UPDATE log_chunks SET deleted_at = $1 WHERE id = ANY($2::uuid[]) RETURNING id"
                .to_string(),
            vec![old.into(), vec![first_id, current_id].into()],
        )
        .await
        .unwrap();
        assert!(
            repo.tombstoned_before(cutoff, 100)
                .await
                .unwrap()
                .is_empty(),
            "a live legacy sibling must protect the shared object"
        );

        // Exactly at the cutoff is still protected, including across formats.
        repo.query_all(
            "UPDATE log_chunks SET deleted_at = $1 WHERE id = $2 RETURNING id".to_string(),
            vec![cutoff.into(), legacy.id.into()],
        )
        .await
        .unwrap();
        assert!(
            repo.tombstoned_before(cutoff, 100)
                .await
                .unwrap()
                .is_empty(),
            "a sibling still in its grace period must protect the object"
        );

        repo.query_all(
            "UPDATE log_chunks SET deleted_at = $1 WHERE id = $2 RETURNING id".to_string(),
            vec![old.into(), legacy.id.into()],
        )
        .await
        .unwrap();
        let eligible = repo.tombstoned_before(cutoff, 100).await.unwrap();
        assert_eq!(eligible.len(), 3);
        assert!(eligible.iter().all(|(_, key)| key == &legacy.storage_key));
        assert_eq!(
            repo.insert(&current).await.unwrap(),
            seq,
            "replay must not resurrect a tombstoned v2 manifest"
        );
        assert_eq!(
            repo.hard_delete(&[first_id, legacy.id, current_id])
                .await
                .unwrap(),
            3
        );
        assert!(repo
            .tombstoned_before(cutoff, 100)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn v2_replay_supports_previously_installed_full_unique_index() {
        let Some(db) = legacy_regression_database().await else {
            return;
        };
        let repo = ManifestRepo::new(db.connection_arc());
        db.execute_sql("DROP INDEX idx_log_chunks_storage_key")
            .await
            .unwrap();
        db.execute_sql("CREATE UNIQUE INDEX idx_log_chunks_storage_key ON log_chunks(storage_key)")
            .await
            .unwrap();
        let mut meta = test_chunk_meta(990_004, "already-upgraded-instance");
        let seq = repo.insert(&meta).await.unwrap();
        meta.id = Uuid::new_v4();
        assert_eq!(
            repo.insert(&meta).await.unwrap(),
            seq,
            "the new conflict predicate must also infer the old full unique index"
        );
    }

    /// The collector's resume position must survive the chunk rows: after
    /// a purge tombstones everything and GC hard-deletes the rows, a restart
    /// would otherwise replay the container's whole Docker log — bringing
    /// purged lines back.
    #[tokio::test]
    #[serial_test::serial]
    async fn resume_position_outlives_hard_deleted_chunk_rows() {
        let db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(_) => {
                println!("Docker/DB not available, skipping test");
                return;
            }
        };
        let repo = ManifestRepo::new(db.connection_arc());
        let container = "container-position-outlives";
        assert_eq!(repo.latest_ended_at(container).await.unwrap(), None);

        let mut meta = test_chunk_meta(990_002, container);
        repo.insert(&meta).await.unwrap();
        let first_end = meta.ended_at;
        assert_eq!(
            repo.latest_ended_at(container).await.unwrap(),
            Some(first_end)
        );

        // A later chunk moves the mark forward; an older (replayed) one never
        // moves it back.
        meta.id = Uuid::new_v4();
        meta.storage_key = format!("{}-later", meta.storage_key);
        meta.ended_at = first_end + chrono::Duration::minutes(5);
        repo.insert(&meta).await.unwrap();
        let later_end = meta.ended_at;
        meta.id = Uuid::new_v4();
        meta.storage_key = format!("{}-older", meta.storage_key);
        meta.ended_at = first_end - chrono::Duration::minutes(5);
        repo.insert(&meta).await.unwrap();
        assert_eq!(
            repo.latest_ended_at(container).await.unwrap(),
            Some(later_end)
        );

        // Purge + GC: every row for the container is gone.
        repo.query_all(
            "DELETE FROM log_chunks WHERE container_id = $1".to_string(),
            vec![container.into()],
        )
        .await
        .unwrap();
        assert_eq!(
            repo.latest_ended_at(container).await.unwrap(),
            Some(later_end),
            "position must come from log_collector_positions once the rows are gone"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn insert_candidates_delete_round_trip() {
        let db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(_) => {
                println!("Docker/DB not available, skipping test");
                return;
            }
        };
        let repo = ManifestRepo::new(db.connection_arc());
        let project_id = 990_001;
        let meta = test_chunk_meta(project_id, "container-round-trip");

        let seq = repo.insert(&meta).await.unwrap();
        assert!(seq >= 0);

        // Re-inserting the same storage_key is a no-op absorbed by
        // ON CONFLICT, and returns the same seq (crash-replay idempotency).
        let seq_again = repo.insert(&meta).await.unwrap();
        assert_eq!(seq, seq_again);

        let mut q = query(LogAccessScope::Allowed {
            project_ids: vec![project_id],
            external_service_ids: vec![],
        });
        q.start_time = meta.started_at - chrono::Duration::hours(1);
        q.end_time = meta.ended_at + chrono::Duration::hours(1);

        let candidates = repo.candidates(&q, None, 10).await.unwrap();
        assert_eq!(
            candidates.len(),
            1,
            "the inserted chunk must be a candidate"
        );
        assert_eq!(candidates[0].seq, seq);

        let deleted = repo.mark_deleted(&[meta.id]).await.unwrap();
        assert_eq!(deleted, 1);

        let candidates_after_delete = repo.candidates(&q, None, 10).await.unwrap();
        assert!(
            candidates_after_delete.is_empty(),
            "a tombstoned chunk must never be a candidate"
        );

        let tombstoned = repo
            .tombstoned_before(Utc::now() + chrono::Duration::hours(1), 10)
            .await
            .unwrap();
        assert!(tombstoned.iter().any(|(id, _)| *id == meta.id));

        let hard_deleted = repo.hard_delete(&[meta.id]).await.unwrap();
        assert_eq!(hard_deleted, 1);

        let tombstoned_after = repo
            .tombstoned_before(Utc::now() + chrono::Duration::hours(1), 10)
            .await
            .unwrap();
        assert!(!tombstoned_after.iter().any(|(id, _)| *id == meta.id));
    }

    /// The forget backlog round trip a `ForgetSweeper` relies on: enqueue is
    /// idempotent, `pending_forgets` returns oldest-first, a failure keeps
    /// the row and records why, and only `resolve_forgets` ever removes one.
    #[tokio::test]
    #[serial_test::serial]
    async fn forget_backlog_enqueue_fail_resolve_round_trip() {
        let db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(_) => {
                println!("Docker/DB not available, skipping test");
                return;
            }
        };
        let repo = ManifestRepo::new(db.connection_arc());

        // Unique, out-of-range sequence numbers so this test never collides
        // with another test's or a real chunk's seq in the shared schema.
        let (a, b) = (-900_001_i64, -900_002_i64);

        repo.enqueue_forget(&[a]).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        repo.enqueue_forget(&[b]).await.unwrap();
        // Re-enqueuing `a` must not disturb its position or reset anything.
        repo.enqueue_forget(&[a]).await.unwrap();
        assert_eq!(repo.forget_backlog_size().await.unwrap(), 2);

        let pending = repo.pending_forgets(10).await.unwrap();
        assert_eq!(
            pending
                .iter()
                .filter(|s| **s == a || **s == b)
                .collect::<Vec<_>>(),
            vec![&a, &b],
            "oldest-first"
        );

        repo.record_forget_failure(&[a], "simulated ClickHouse timeout")
            .await
            .unwrap();
        assert_eq!(
            repo.forget_backlog_size().await.unwrap(),
            2,
            "a failure keeps the row, it does not drop it"
        );

        repo.resolve_forgets(&[a]).await.unwrap();
        let remaining = repo.pending_forgets(10).await.unwrap();
        assert!(!remaining.contains(&a), "resolved entries are gone");
        assert!(remaining.contains(&b), "unresolved entries survive");

        repo.resolve_forgets(&[b]).await.unwrap();
        let remaining = repo.pending_forgets(10).await.unwrap();
        assert!(!remaining.contains(&a) && !remaining.contains(&b));
    }

    /// A call site enqueues best-effort before attempting the immediate
    /// forget — if that enqueue itself fails (Postgres blip) there is no
    /// backlog row yet when the immediate forget also fails. This is the one
    /// path that must not lose the retry: `record_forget_failure` has to
    /// create its own row rather than assume one already exists.
    #[tokio::test]
    #[serial_test::serial]
    async fn record_forget_failure_creates_the_row_when_enqueue_never_ran() {
        let db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(_) => {
                println!("Docker/DB not available, skipping test");
                return;
            }
        };
        let repo = ManifestRepo::new(db.connection_arc());
        let seq = -900_003_i64;

        assert!(!repo.pending_forgets(1000).await.unwrap().contains(&seq));

        repo.record_forget_failure(&[seq], "simulated: enqueue and forget both failed")
            .await
            .unwrap();

        assert!(
            repo.pending_forgets(1000).await.unwrap().contains(&seq),
            "the failure must be durably queued even though nothing enqueued it first"
        );

        repo.resolve_forgets(&[seq]).await.unwrap();
    }
}
