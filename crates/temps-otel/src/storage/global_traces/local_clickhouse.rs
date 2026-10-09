// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Page-first global trace reads against the local ClickHouse `spans` table.
//!
//! # Why this exists
//!
//! The global view used to dedup (`ORDER BY _version DESC LIMIT 1 BY`) and
//! aggregate EVERY span in the requested window, wide `attributes` / `events`
//! columns included, and only then sort and cut a page of at most 100 traces —
//! twice, once for `count()` and once for the page. On a busy instance that
//! needs gigabytes (`LIMIT BY` keeps a non-spillable hash entry per distinct
//! span; the sort needs the whole window), so it died with
//! `MEMORY_LIMIT_EXCEEDED` on small hosts however the server profile was tuned.
//! `0001_spans.sql` already says where the dedup belongs: "after narrowing to a
//! page, not across a window". This module applies that rule, mirroring
//! `ClickHouseOtelStorage::query_trace_summaries` / `count_traces`.
//!
//! # Shape
//!
//! One statement per page, two stages, plus a concurrent total:
//!
//! * **Stage 1 (select the page)** reads narrow columns only and
//!   `GROUP BY project_id, trace_id` with duplicate-insensitive aggregates
//!   (`min(start_time)`, `max(duration_ms)`), orders by the requested sort with
//!   `(project_id, trace_id)` tie-breakers, and cuts `LIMIT .. OFFSET ..`.
//!   Its predicates compare `start_time` directly (no wrapping function), so
//!   ClickHouse can prune partitions and serve it from `proj_recent`
//!   (`0007_spans_recent_projection.sql`) whenever no non-projected column is
//!   filtered or sorted on. No `LIMIT BY`, no `attributes` / `events`.
//! * **Stage 2 (hydrate the page)** reads the wide columns for the selected
//!   keys only, through the `(project_id, trace_id, span_id)` primary key, and
//!   applies `ORDER BY _version DESC LIMIT 1 BY project_id, trace_id, span_id`
//!   to that handful of rows. The aggregation is the one the single-stage query
//!   used, so the values it returns are unchanged.
//! * **Total** counts the groups of `GROUP BY project_id, trace_id` over the
//!   same narrow, projection-eligible read — duplicate rows cannot inflate it.
//!   It is deliberately not `uniqExact(project_id, trace_id)`: that keeps one
//!   hash per distinct trace in a set that cannot spill, so its memory grows
//!   with the window, while a grouped count spills like stage 1 does and is
//!   also faster (1.4 s vs 1.7 s on 6M traces).
//!
//! # Memory
//!
//! Stage 2 is page-sized. Stage 1 and the total group by trace over the whole
//! window, and that state is spilled to disk past [`SPILL_BYTES`] (aggregation
//! streams in primary-key order where it can), so peak memory follows the spill
//! threshold rather than the number of traces: about 140 MiB for 6M traces and
//! no more for more. Every statement also carries a hard [`MAX_MEMORY_BYTES`]
//! cap and a [`QUERY_BUDGET`] wall-clock budget. Hitting either one fails the request
//! with a typed [`OtelError::Storage`] naming the stage and the limit — never a
//! partial page.
//!
//! # Measured
//!
//! Synthetic data, ClickHouse 26.2, `max_threads = 2`, 24 h window over 3
//! projects holding 10M spans / 2M traces of a 30M-span / 6M-trace table
//! (wide `attributes` / `events`), newest-first page of 20:
//!
//! | | single-stage (before) | page-first (after) |
//! |---|---|---|
//! | page | 18.8 s, 1.83 GiB, 19.1M rows | 0.7 s, ~140 MiB, 10.2M rows |
//! | total | 5.1 s, 1.55 GiB, 19.1M rows | 0.5 s, ~150 MiB, 10.0M rows |
//! | sort by duration | — | 1.0 s, 46 MiB (in-order grouping) |
//! | 3-day window (30M spans, 6M traces) | `MEMORY_LIMIT_EXCEEDED` at 2.79 GiB | page 1.7 s / 136 MiB, total 1.4 s / 280 MiB |
//!
//! The pages are byte-identical for both sort orders. Stage 1 and the total
//! stay proportional to the window (they must look at every span in it), but
//! their memory is capped by spilling rather than by the window.
//!
//! # Known residuals
//!
//! Stage 1 ranks traces from the physical rows, not the deduped ones. If two
//! physical copies of one span disagree about `duration_ms` / `status_code`
//! (a Postgres backfill next to a live row, see `query_trace_summaries`), page
//! membership at the very edge of a page can follow the superseded copy. The
//! rows returned are always the deduped ones, and the page is ordered by the
//! values it displays. Filtering on a faceted attribute key reads its
//! `facet_attr_N` slot column through the bloom-filter skip index (0008), the
//! same as the project trace list: 21 ms / 327k rows against 1.4 s / 30M rows
//! for the JSON path on the 30M-span table above. An UNFACETED key still has to
//! parse the `attributes` blob of every span in the window in stage 1 and the
//! total; that cost grows with the window (bounded by the spill, memory and
//! time caps), and marking the key as a facet is the fix. A facet still
//! backfilling answers from the rows populated so far, as it does for the
//! project trace list.

use super::{
    can_use_lifetime_summaries, ch_query, invalid, Bind, Facets, GlobalTraceQuery, GlobalTraceRow,
    GlobalTraceStream, MAX_LIFETIME_CANDIDATES,
};
use crate::storage::clickhouse::read_limits::{classify, Failure};
use crate::{
    error::{OtelError, StorageErrorKind},
    storage::StorageResult,
    types::*,
};
use std::time::Duration;

/// Hard per-query memory ceiling for every statement on this path. Sized for a
/// 4 GiB host that also runs the rest of the stack; the page and the total run
/// concurrently, so a request stays under twice this.
pub const MAX_MEMORY_BYTES: u64 = 512 << 20;
/// Aggregation / sort state beyond this spills to disk instead of growing.
/// Measured on 6M traces: 128 MiB gives the lowest peak (~136 MiB) at the same
/// speed as 256 MiB (~306 MiB); smaller values spill more for no gain.
pub(super) const SPILL_BYTES: u64 = 128 << 20;
/// Server-side wall-clock budget per statement.
pub(super) const QUERY_BUDGET: Duration = Duration::from_secs(30);
/// Client-side backstop, a little longer than the server budget so the server's
/// own `TIMEOUT_EXCEEDED` (which names the limit) wins the race.
const CLIENT_BACKSTOP: Duration = Duration::from_secs(35);
/// Most rows one source may be asked to hydrate. Local-only reads need at most
/// one page (100); only a merge that must read this source from its start (a
/// mixed local/Cloud read at a deep offset) asks for more. Each hydrated trace
/// costs one primary-key seek, so the ask is bounded and a deeper one is
/// refused with an error rather than silently truncated.
pub(super) const MAX_PAGE_ROWS: u64 = 10_000;

/// SQL text and the values for its `?` placeholders, kept in lockstep. Binds
/// are positional, so every fragment is appended in the order it appears in the
/// final statement — this is the only place that order is maintained.
#[derive(Default)]
struct Frag {
    sql: String,
    binds: Vec<Bind>,
}

impl Frag {
    fn text(&mut self, sql: &str) -> &mut Self {
        self.sql.push_str(sql);
        self
    }
    fn bind(&mut self, value: Bind) -> &mut Self {
        self.sql.push('?');
        self.binds.push(value);
        self
    }
    fn frag(&mut self, other: Frag) -> &mut Self {
        self.sql.push_str(&other.sql);
        self.binds.extend(other.binds);
        self
    }
}

/// How whole-trace values are derived for a page.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Values {
    /// Only spans inside the scope windows that match the filters.
    Window,
    /// Every span of a trace that has a span in the window (unfiltered reads).
    Lifetime,
}

/// One statement with its positional values.
pub(super) struct Stmt {
    pub(super) sql: String,
    pub(super) binds: Vec<Bind>,
    /// Group in primary-key order (constant memory) instead of hashing. Only
    /// sound to request when `proj_recent` cannot serve the statement: the
    /// setting makes ClickHouse prefer the base table's sort order over the
    /// projection, which would read the whole partition for a one-hour window.
    in_order: bool,
}

/// The statements that answer one request.
pub(super) struct Plan {
    /// Present when the total is not already known from the candidate count.
    pub total: Option<Stmt>,
    pub page: Stmt,
}

/// Rows this source must produce, from the merge contract: the page plus
/// everything before it that the merge may still need from this source.
fn requested_rows(q: &GlobalTraceQuery) -> u64 {
    q.filter
        .offset
        .unwrap_or(0)
        .saturating_add(q.filter.limit.unwrap_or(20).clamp(1, 100))
        .saturating_sub(q.source_offset)
}

fn sort_key(q: &GlobalTraceQuery) -> (&'static str, &'static str) {
    // (stage 1 aggregate, stage 2 output column)
    if q.filter.sort_by == TraceSortField::Duration {
        ("max(duration_ms)", "duration")
    } else {
        ("min(start_time)", "start_ms")
    }
}

/// Whether every filter reads only columns `proj_recent` stores
/// (`project_id`, `start_time`, `trace_id`, `span_id`, `parent_span_id`).
fn filters_fit_projection(q: &GlobalTraceQuery) -> bool {
    let f = &q.filter;
    f.service_name.is_none()
        && f.name_pattern.is_none()
        && f.deployment_id.is_none()
        && f.attributes.as_ref().is_none_or(|a| a.is_empty())
        && f.min_duration_ms.is_none()
        && f.status.is_none()
}

/// `(a) OR (b) ..`: one window per scope. `start_time` is compared as a bare
/// column — wrapping it in `toUnixTimestamp64Milli` hides it from partition
/// pruning and from `proj_recent`.
fn scope(q: &GlobalTraceQuery) -> Frag {
    let mut f = Frag::default();
    if q.scopes.is_empty() {
        f.text("FALSE");
    }
    for (i, s) in q.scopes.iter().enumerate() {
        if i > 0 {
            f.text(" OR ");
        }
        f.text("(project_id = ")
            .bind(Bind::Int(s.project_id as i64))
            .text(" AND start_time >= fromUnixTimestamp64Milli(")
            .bind(Bind::Int(s.from.timestamp_millis()))
            .text(") AND start_time <= fromUnixTimestamp64Milli(")
            .bind(Bind::Int(s.to.timestamp_millis()))
            .text("))");
    }
    f
}

/// ` AND ..` for every span-level filter. Same predicates, same semantics as
/// the Postgres/Cloud builder.
fn span_filters(q: &GlobalTraceQuery, facets: &Facets) -> StorageResult<Frag> {
    let f = &q.filter;
    let mut out = Frag::default();
    for (column, value) in [("trace_id", &f.trace_id), ("service_name", &f.service_name)] {
        if let Some(v) = value {
            out.text(&format!(" AND {column} = "))
                .bind(Bind::Text(v.clone()));
        }
    }
    if let Some(v) = &f.name_pattern {
        out.text(" AND name ILIKE ")
            .bind(Bind::Text(format!("%{v}%")));
    }
    if let Some(v) = f.deployment_id {
        out.text(" AND deployment_id = ").bind(Bind::Int(v as i64));
    }
    if f.environment_id.is_some() {
        return Err(invalid(
            "Use the project trace endpoint for environment ID filters",
        ));
    }
    for (k, v) in f.attributes.iter().flatten() {
        if let Some(&slot) = facets.get(k.as_str()) {
            // Faceted key: the slot column carries a bloom-filter skip index, so
            // ClickHouse skips granules that cannot hold the value instead of
            // parsing the JSON of every span in the window.
            out.text(&format!(
                " AND {} = ",
                crate::services::facet_service::facet_column_name(slot)
            ))
            .bind(Bind::Text(v.clone()));
        } else {
            out.text(" AND JSONExtractString(attributes, ")
                .bind(Bind::Text(k.clone()))
                .text(") = ")
                .bind(Bind::Text(v.clone()));
        }
    }
    Ok(out)
}

/// `WHERE (scope) AND filters`.
fn where_clause(q: &GlobalTraceQuery, facets: &Facets) -> StorageResult<Frag> {
    let mut f = Frag::default();
    f.text(" WHERE (")
        .frag(scope(q))
        .text(")")
        .frag(span_filters(q, facets)?);
    Ok(f)
}

/// Whole-trace conditions (`min_duration_ms`, `status`), expressed on the
/// per-trace aggregate exactly as the single-stage query's outer filter was.
fn trace_having(q: &GlobalTraceQuery) -> Frag {
    let mut f = Frag::default();
    let mut first = true;
    let mut clause = |f: &mut Frag| {
        f.text(if first { " HAVING " } else { " AND " });
        first = false;
    };
    if let Some(v) = q.filter.min_duration_ms {
        clause(&mut f);
        f.text("max(duration_ms) >= ").bind(Bind::Float(v));
    }
    if let Some(v) = q.filter.status {
        clause(&mut f);
        f.text(
            "(CASE WHEN countIf(upper(status_code) = 'ERROR') > 0 THEN 'ERROR' \
             ELSE argMax(upper(status_code), tuple(parent_span_id = '', duration_ms, span_id)) END) = ",
        )
        .bind(Bind::Text(v.to_string()));
    }
    f
}

/// Row-level conditions for raw span reads (`summaries = false`).
fn span_row_filters(q: &GlobalTraceQuery) -> Frag {
    let mut f = Frag::default();
    if let Some(v) = q.filter.min_duration_ms {
        f.text(" AND duration_ms >= ").bind(Bind::Float(v));
    }
    if let Some(v) = q.filter.status {
        f.text(" AND upper(status_code) = ")
            .bind(Bind::Text(v.to_string()));
    }
    f
}

fn pick(field: &str) -> String {
    format!("argMax(raw.{field}, tuple(raw.parent_span_id = '', raw.duration, raw.span_id))")
}

/// Stage 2 for trace summaries: dedup the page's spans, then fold them into one
/// row per trace. `keys` selects the page's `(project_id, trace_id)` pairs.
fn summaries_hydration(
    q: &GlobalTraceQuery,
    facets: &Facets,
    values: Values,
    keys: Frag,
) -> StorageResult<Frag> {
    let mut f = Frag::default();
    f.text(
        "WITH raw AS (SELECT project_id AS project_id, trace_id, span_id, \
         COALESCE(parent_span_id, '') AS parent_span_id, name, service_name, \
         COALESCE(deployment_environment, '') AS environment, kind AS kind, \
         upper(status_code) AS status, toUnixTimestamp64Milli(start_time) AS start_ms, \
         duration_ms AS duration FROM spans WHERE ",
    );
    if values == Values::Window {
        f.text("(")
            .frag(scope(q))
            .text(")")
            .frag(span_filters(q, facets)?)
            .text(" AND ");
    }
    let status = match values {
        Values::Window => pick("status"),
        Values::Lifetime => "'OK'".to_string(),
    };
    f.text("(project_id, trace_id) IN (")
        .frag(keys)
        .text(&format!(
            ") ORDER BY _version DESC LIMIT 1 BY project_id, trace_id, span_id), \
             grouped AS (SELECT project_id, trace_id, '' AS span_id, '' AS parent_span_id, \
             {} AS name, {} AS service_name, {} AS environment, {} AS kind, \
             CASE WHEN countIf(raw.status = 'ERROR') > 0 THEN 'ERROR' ELSE {status} END AS status, \
             MIN(raw.start_ms) AS start_ms, MAX(raw.duration) AS duration, \
             toInt64(count()) AS span_count, toInt64(countIf(raw.status = 'ERROR')) AS error_count, \
             '{{}}' AS attributes, '[]' AS events, '' AS status_message \
             FROM raw GROUP BY project_id, trace_id) \
             SELECT * FROM grouped ORDER BY {} {}, project_id, trace_id",
            pick("name"),
            pick("service_name"),
            pick("environment"),
            pick("kind"),
            sort_key(q).1,
            q.filter.sort_order.as_sql(),
        ));
    Ok(f)
}

/// Stage 1 for trace summaries: the page's `(project_id, trace_id)` pairs.
/// `source` is the `WHERE` clause (scope + filters, or the lifetime candidate
/// set) the per-trace groups are built from.
fn summary_keys(q: &GlobalTraceQuery, source: Frag, having: Frag, limit: u64) -> Frag {
    let mut f = Frag::default();
    f.text("SELECT project_id, trace_id FROM spans")
        .frag(source)
        .text(" GROUP BY project_id, trace_id")
        .frag(having)
        .text(&format!(
            " ORDER BY {} {}, project_id, trace_id LIMIT {limit} OFFSET {}",
            sort_key(q).0,
            q.filter.sort_order.as_sql(),
            q.source_offset,
        ));
    f
}

/// Stage 1 for raw spans: the page's `(project_id, trace_id, span_id)` keys.
/// Grouping by the whole primary key dedups physical copies without `LIMIT BY`
/// and streams in key order.
fn span_keys(q: &GlobalTraceQuery, facets: &Facets, limit: u64) -> StorageResult<Frag> {
    let mut f = Frag::default();
    f.text("SELECT project_id, trace_id, span_id FROM spans")
        .frag(where_clause(q, facets)?)
        .frag(span_row_filters(q))
        .text(&format!(
            " GROUP BY project_id, trace_id, span_id ORDER BY {} {}, project_id, trace_id, span_id \
             LIMIT {limit} OFFSET {}",
            sort_key(q).0,
            q.filter.sort_order.as_sql(),
            q.source_offset,
        ));
    Ok(f)
}

/// Stage 2 for raw spans: the wide columns of the page's spans, deduplicated.
fn span_hydration(q: &GlobalTraceQuery, facets: &Facets, keys: Frag) -> StorageResult<Frag> {
    let mut f = Frag::default();
    f.text(
        "WITH raw AS (SELECT project_id AS project_id, trace_id, span_id, \
         COALESCE(parent_span_id, '') AS parent_span_id, name, service_name, \
         COALESCE(deployment_environment, '') AS environment, kind AS kind, \
         upper(status_code) AS status, toUnixTimestamp64Milli(start_time) AS start_ms, \
         duration_ms AS duration, attributes AS attributes, events AS events, \
         status_message AS status_message FROM spans",
    )
    .frag(where_clause(q, facets)?)
    .text(" AND (project_id, trace_id, span_id) IN (")
    .frag(keys)
    .text(&format!(
        ") ORDER BY _version DESC LIMIT 1 BY project_id, trace_id, span_id) \
         SELECT project_id, trace_id, span_id, parent_span_id, name, service_name, environment, \
         kind, status, start_ms, duration, toInt64(1) AS span_count, \
         toInt64(status = 'ERROR') AS error_count, attributes, events, status_message \
         FROM raw ORDER BY {} {}, project_id, trace_id, span_id",
        sort_key(q).1,
        q.filter.sort_order.as_sql(),
    ));
    Ok(f)
}

/// Distinct traces (or spans) matching the filters, counted as the groups of a
/// spillable `GROUP BY` (see the module docs for why not `uniqExact`). Narrow
/// and projection-eligible; duplicate physical rows cannot inflate it.
fn total(q: &GlobalTraceQuery, facets: &Facets) -> StorageResult<Frag> {
    let mut f = Frag::default();
    if !q.summaries {
        // Distinct spans: stream the primary-key groups rather than hashing
        // three strings per span in a set that grows with the window.
        f.text("SELECT count() FROM (SELECT project_id FROM spans")
            .frag(where_clause(q, facets)?)
            .frag(span_row_filters(q))
            .text(" GROUP BY project_id, trace_id, span_id)");
        return Ok(f);
    }
    f.text("SELECT count() FROM (SELECT project_id FROM spans")
        .frag(where_clause(q, facets)?)
        .text(" GROUP BY project_id, trace_id")
        .frag(trace_having(q))
        .text(")");
    Ok(f)
}

/// Build the statements for one request. `lifetime` selects whole-trace values
/// for an unfiltered read whose candidate count fits the cap.
pub(super) fn plan(q: &GlobalTraceQuery, lifetime: bool, facets: &Facets) -> StorageResult<Plan> {
    let requested = requested_rows(q);
    let limit = if lifetime {
        // At most one row per candidate trace exists, so this only bounds the
        // statement text.
        requested.min(MAX_LIFETIME_CANDIDATES)
    } else if requested > MAX_PAGE_ROWS {
        return Err(invalid(&format!(
            "A merged global trace page needs {requested} rows from the local ClickHouse source \
             (offset {} + page size - {} already skipped), above the {MAX_PAGE_ROWS}-row limit; \
             narrow the time window or filters instead of paging this deep",
            q.filter.offset.unwrap_or(0),
            q.source_offset,
        )));
    } else {
        requested
    };
    let fits = filters_fit_projection(q);
    let (page, page_in_order) = if !q.summaries {
        (
            span_hydration(q, facets, span_keys(q, facets, limit)?)?,
            !(fits && q.filter.sort_by == TraceSortField::StartTime),
        )
    } else if lifetime {
        let mut candidates = Frag::default();
        candidates
            .text(" WHERE (project_id, trace_id) IN (SELECT project_id, trace_id FROM spans")
            .frag(where_clause(q, facets)?)
            .text(" GROUP BY project_id, trace_id)");
        let keys = summary_keys(q, candidates, Frag::default(), limit);
        // At most `MAX_LIFETIME_CANDIDATES` groups: hashing them is cheap, and
        // the membership subquery wants the projection.
        (
            summaries_hydration(q, facets, Values::Lifetime, keys)?,
            false,
        )
    } else {
        let keys = summary_keys(q, where_clause(q, facets)?, trace_having(q), limit);
        (
            summaries_hydration(q, facets, Values::Window, keys)?,
            !(fits && q.filter.sort_by == TraceSortField::StartTime),
        )
    };
    let total = if lifetime {
        None
    } else {
        let t = total(q, facets)?;
        Some(Stmt {
            sql: t.sql,
            binds: t.binds,
            in_order: !fits,
        })
    };
    Ok(Plan {
        total,
        page: Stmt {
            sql: page.sql,
            binds: page.binds,
            in_order: page_in_order,
        },
    })
}

fn describe(q: &GlobalTraceQuery) -> String {
    let from = q.scopes.iter().map(|s| s.from).min();
    let to = q.scopes.iter().map(|s| s.to).max();
    format!(
        "{} project scope(s), window {}..{}, offset {}",
        q.scopes.len(),
        from.map(|v| v.to_rfc3339()).unwrap_or_default(),
        to.map(|v| v.to_rfc3339()).unwrap_or_default(),
        q.filter.offset.unwrap_or(0),
    )
}

fn failure(stage: &str, q: &GlobalTraceQuery, error: ::clickhouse::error::Error) -> OtelError {
    let context = describe(q);
    match classify(&error) {
        Failure::MemoryLimit => OtelError::Storage {
            message: format!(
                "Local ClickHouse global trace {stage} query exceeded its {} MiB memory budget \
                 ({context}); narrow the time window or filters: {error}",
                MAX_MEMORY_BYTES >> 20
            ),
            kind: StorageErrorKind::ClickHouseOther,
        },
        Failure::Timeout => OtelError::Storage {
            message: format!(
                "Local ClickHouse global trace {stage} query exceeded its {:?} budget \
                 ({context}): {error}",
                QUERY_BUDGET
            ),
            kind: StorageErrorKind::ClickHouseTimeout,
        },
        Failure::Other => OtelError::Storage {
            message: format!(
                "Local ClickHouse global trace {stage} query failed ({context}): {error}"
            ),
            kind: crate::storage::clickhouse::ch_err_kind(&error),
        },
    }
}

fn backstop(stage: &str, q: &GlobalTraceQuery) -> OtelError {
    OtelError::Storage {
        message: format!(
            "Local ClickHouse global trace {stage} query got no answer within {CLIENT_BACKSTOP:?} ({})",
            describe(q)
        ),
        kind: StorageErrorKind::ClickHouseTimeout,
    }
}

/// Attach the memory / spill / wall-clock bounds to a statement.
fn bounded(query: ::clickhouse::query::Query, in_order: bool) -> ::clickhouse::query::Query {
    query
        .with_setting("max_memory_usage", MAX_MEMORY_BYTES.to_string())
        .with_setting(
            "max_bytes_before_external_group_by",
            SPILL_BYTES.to_string(),
        )
        .with_setting("max_bytes_before_external_sort", SPILL_BYTES.to_string())
        .with_setting("max_execution_time", QUERY_BUDGET.as_secs().to_string())
        // Group by the primary-key prefix in order where the planner can:
        // memory stops depending on the number of groups.
        .with_setting(
            "optimize_aggregation_in_order",
            if in_order { "1" } else { "0" },
        )
}

async fn fetch_total(
    client: &::clickhouse::Client,
    q: &GlobalTraceQuery,
    stage: &str,
    stmt: &Stmt,
) -> StorageResult<u64> {
    tokio::time::timeout(
        CLIENT_BACKSTOP,
        bounded(ch_query(client, &stmt.sql, &stmt.binds), stmt.in_order).fetch_one::<u64>(),
    )
    .await
    .map_err(|_| backstop(stage, q))?
    .map_err(|e| failure(stage, q, e))
}

async fn fetch_page(
    client: &::clickhouse::Client,
    q: &GlobalTraceQuery,
    stmt: &Stmt,
) -> StorageResult<Vec<GlobalTraceRow>> {
    tokio::time::timeout(
        CLIENT_BACKSTOP,
        bounded(ch_query(client, &stmt.sql, &stmt.binds), stmt.in_order)
            .fetch_all::<GlobalTraceRow>(),
    )
    .await
    .map_err(|_| backstop("page", q))?
    .map_err(|e| failure("page", q, e))
}

fn stream(total: u64, rows: Vec<GlobalTraceRow>) -> GlobalTraceStream {
    GlobalTraceStream {
        total,
        rows: Box::pin(futures::stream::iter(rows.into_iter().map(Ok))),
    }
}

/// Read one ordered, page-bounded cursor from the local `spans` table.
pub(super) async fn read(
    client: &::clickhouse::Client,
    q: &GlobalTraceQuery,
    facets: &Facets,
) -> StorageResult<GlobalTraceStream> {
    if can_use_lifetime_summaries(q) {
        // Unfiltered, so the candidate count IS the window total.
        let count = total(q, facets)?;
        let count = Stmt {
            sql: count.sql,
            binds: count.binds,
            in_order: false,
        };
        let candidates = fetch_total(client, q, "candidate count", &count).await?;
        let lifetime = candidates <= MAX_LIFETIME_CANDIDATES;
        let plan = plan(q, lifetime, facets)?;
        let rows = fetch_page(client, q, &plan.page).await?;
        return Ok(stream(candidates, rows));
    }
    let plan = plan(q, false, facets)?;
    let Some(total_sql) = plan.total.as_ref() else {
        return Err(invalid("Local global trace plan has no total statement"));
    };
    let (total, rows) = tokio::try_join!(
        fetch_total(client, q, "total", total_sql),
        fetch_page(client, q, &plan.page)
    )?;
    Ok(stream(total, rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;

    /// Plans without any facets, the shape most tests are about.
    fn plan(q: &GlobalTraceQuery, lifetime: bool) -> StorageResult<Plan> {
        super::plan(q, lifetime, &Facets::new())
    }

    fn query(scopes: usize) -> GlobalTraceQuery {
        let from = DateTime::from_timestamp_millis(1_000).unwrap();
        let to = DateTime::from_timestamp_millis(2_000).unwrap();
        GlobalTraceQuery {
            filter: TraceQuery {
                limit: Some(20),
                offset: Some(40),
                ..Default::default()
            },
            scopes: (1..=scopes as i32)
                .map(|project_id| super::super::TraceReadScope {
                    project_id,
                    from,
                    to,
                    cloud: false,
                    window_clamped_at: None,
                })
                .collect(),
            summaries: true,
            use_preaggregated_summaries: true,
            lifetime_candidate_total: None,
            source_offset: 40,
        }
    }

    /// Substitute binds so a test can assert on the text a placeholder landed in.
    fn render(sql: &str, binds: &[Bind]) -> String {
        let mut out = String::new();
        let mut binds = binds.iter();
        for c in sql.chars() {
            if c != '?' {
                out.push(c);
                continue;
            }
            match binds.next() {
                Some(Bind::Int(v)) => out.push_str(&v.to_string()),
                Some(Bind::Float(v)) => out.push_str(&v.to_string()),
                Some(Bind::Text(v)) => out.push_str(&format!("'{v}'")),
                None => out.push('?'),
            }
        }
        assert!(binds.next().is_none(), "more binds than placeholders");
        out
    }

    fn placeholders(sql: &str) -> usize {
        sql.matches('?').count()
    }

    #[test]
    fn stage_one_is_narrow_and_dedups_nothing_across_the_window() {
        let plan = plan(&query(3), false).unwrap();
        let (sql, binds) = (&plan.page.sql, &plan.page.binds);
        assert_eq!(placeholders(sql), binds.len());

        // The page is selected by an inner statement that never touches the
        // wide columns or the dedup machinery.
        let (hydrate, keys) = sql
            .split_once("IN (SELECT project_id, trace_id FROM spans")
            .unwrap();
        let keys = keys.split(") ORDER BY _version").next().unwrap();
        assert!(!keys.contains("LIMIT 1 BY"), "stage 1 must not LIMIT BY");
        assert!(!keys.contains("attributes") && !keys.contains("events"));
        assert!(keys.contains("GROUP BY project_id, trace_id ORDER BY min(start_time) DESC"));
        assert!(keys.contains("project_id, trace_id LIMIT 20 OFFSET 40"));
        assert!(!keys.contains("_version"));

        // The dedup runs once, over the keys of the page only.
        assert_eq!(sql.matches("LIMIT 1 BY").count(), 1);
        assert!(sql.contains(") ORDER BY _version DESC LIMIT 1 BY project_id, trace_id, span_id)"));
        assert!(!hydrate.contains("attributes") && hydrate.contains("FROM spans WHERE ("));
        assert!(!sql.contains("FINAL"));
        assert!(!sql.contains("toUnixTimestamp64Milli(start_time) >="));
    }

    #[test]
    fn scope_compares_the_bare_start_time_column_so_proj_recent_applies() {
        let plan = plan(&query(2), false).unwrap();
        let total = plan.total.unwrap();
        assert!(total
            .sql
            .starts_with("SELECT count() FROM (SELECT project_id FROM spans WHERE ("));
        assert!(total
            .sql
            .contains("start_time >= fromUnixTimestamp64Milli(?)"));
        assert!(!total.sql.contains("uniqExact"));
        assert!(total.sql.ends_with("GROUP BY project_id, trace_id)"));
        assert_eq!(total.binds.len(), 6);
        assert!(!total.sql.contains("attributes"));
        let rendered = render(&total.sql, &total.binds);
        assert!(rendered.contains(
            "(project_id = 1 AND start_time >= fromUnixTimestamp64Milli(1000) AND start_time <= fromUnixTimestamp64Milli(2000)) OR (project_id = 2"
        ));
    }

    #[test]
    fn binds_follow_placeholder_order_when_the_filters_repeat_in_both_stages() {
        let mut q = query(2);
        q.filter.service_name = Some("api".into());
        q.filter.min_duration_ms = Some(25.0);
        q.filter.status = Some(SpanStatusCode::Error);
        let plan = plan(&q, false).unwrap();
        let (sql, binds) = (&plan.page.sql, &plan.page.binds);
        assert_eq!(placeholders(sql), binds.len());
        // scope(2) + service twice (hydration, keys); scope twice; min duration + status once.
        let rendered = render(sql, binds);
        assert_eq!(rendered.matches("service_name = 'api'").count(), 2);
        assert_eq!(rendered.matches("max(duration_ms) >= 25").count(), 1);
        assert_eq!(rendered.matches("END) = 'ERROR'").count(), 1);
        let hydrate_scope = rendered.find("project_id = 1 AND").unwrap();
        let keys_scope = rendered.rfind("project_id = 1 AND").unwrap();
        assert!(hydrate_scope < keys_scope);
        // `service_name = ?` follows the scope in each stage.
        assert!(rendered[hydrate_scope..]
            .find("service_name = 'api'")
            .is_some());

        let total = plan.total.unwrap();
        assert_eq!(placeholders(&total.sql), total.binds.len());
        assert!(total
            .sql
            .starts_with("SELECT count() FROM (SELECT project_id FROM spans"));
        assert!(total
            .sql
            .contains("GROUP BY project_id, trace_id HAVING max(duration_ms) >= ?"));
    }

    #[test]
    fn duration_sort_orders_stage_one_by_the_aggregate_and_stage_two_by_its_column() {
        let mut q = query(1);
        q.filter.sort_by = TraceSortField::Duration;
        q.filter.sort_order = SortOrder::Asc;
        let sql = plan(&q, false).unwrap().page.sql;
        assert!(
            sql.contains("ORDER BY max(duration_ms) ASC, project_id, trace_id LIMIT 20 OFFSET 40")
        );
        assert!(sql.ends_with("SELECT * FROM grouped ORDER BY duration ASC, project_id, trace_id"));
    }

    #[test]
    fn lifetime_pages_rank_candidates_by_whole_trace_values_without_a_window_dedup() {
        let q = query(2);
        let plan = plan(&q, true).unwrap();
        assert!(plan.total.is_none(), "the candidate count is the total");
        let (sql, binds) = (&plan.page.sql, &plan.page.binds);
        assert_eq!(placeholders(sql), binds.len());
        assert_eq!(
            binds.len(),
            6,
            "the window is bound once, for membership only"
        );
        assert!(sql.contains("IN (SELECT project_id, trace_id FROM spans WHERE (project_id, trace_id) IN (SELECT project_id, trace_id FROM spans WHERE ("));
        assert_eq!(sql.matches("LIMIT 1 BY").count(), 1);
        assert!(sql.contains("ELSE 'OK' END AS status"));
        assert!(sql.contains("MIN(raw.start_ms) AS start_ms"));
        assert!(!sql.contains("attributes AS"));
    }

    #[test]
    fn lifetime_page_size_is_capped_but_a_window_page_that_deep_is_refused() {
        let mut q = query(1);
        q.filter.offset = Some(1_000_000);
        q.source_offset = 0;
        assert!(plan(&q, true)
            .unwrap()
            .page
            .sql
            .contains("LIMIT 5000 OFFSET 0"));

        let error = plan(&q, false).err().unwrap().to_string();
        assert!(error.contains("1000020 rows"), "{error}");
        assert!(error.contains("10000-row limit"), "{error}");

        q.source_offset = 1_000_000;
        assert!(plan(&q, false)
            .unwrap()
            .page
            .sql
            .contains("LIMIT 20 OFFSET 1000000"));
    }

    #[test]
    fn raw_span_pages_select_keys_before_reading_attributes_and_events() {
        let mut q = query(2);
        q.summaries = false;
        q.filter.min_duration_ms = Some(10.0);
        q.filter.status = Some(SpanStatusCode::Ok);
        let plan = plan(&q, false).unwrap();
        let (sql, binds) = (&plan.page.sql, &plan.page.binds);
        assert_eq!(placeholders(sql), binds.len());
        let (hydrate, keys) = sql
            .split_once("IN (SELECT project_id, trace_id, span_id FROM spans")
            .unwrap();
        let keys = keys.split(") ORDER BY _version").next().unwrap();
        assert!(hydrate.contains("attributes AS attributes"));
        assert!(!keys.contains("attributes") && !keys.contains("LIMIT 1 BY"));
        assert!(
            keys.contains("GROUP BY project_id, trace_id, span_id ORDER BY min(start_time) DESC")
        );
        assert!(keys.contains("AND duration_ms >= ? AND upper(status_code) = ?"));
        // The row-level conditions apply while choosing the page, not after
        // hydration — a hydrated row must never be dropped from a full page.
        assert!(!hydrate.contains("duration_ms >= ?"));
        let total = plan.total.unwrap();
        assert!(total
            .sql
            .starts_with("SELECT count() FROM (SELECT project_id FROM spans"));
        assert!(total
            .sql
            .ends_with("GROUP BY project_id, trace_id, span_id)"));
        assert_eq!(placeholders(&total.sql), total.binds.len());
    }

    #[test]
    fn in_order_grouping_is_requested_only_where_proj_recent_cannot_serve_the_statement() {
        // Default view: start-time sort, no wide filters. The projection serves
        // both statements, and in-order grouping would displace it.
        let default_view = plan(&query(2), false).unwrap();
        assert!(!default_view.page.in_order && !default_view.total.as_ref().unwrap().in_order);

        // Duration is not stored in the projection: stream the groups instead.
        let mut by_duration = query(2);
        by_duration.filter.sort_by = TraceSortField::Duration;
        let sorted = plan(&by_duration, false).unwrap();
        assert!(sorted.page.in_order);
        assert!(
            !sorted.total.unwrap().in_order,
            "the total never reads duration"
        );

        // A filter on a wide column forces the base table for both.
        let mut filtered = query(2);
        filtered.filter.service_name = Some("api".into());
        let wide = plan(&filtered, false).unwrap();
        assert!(wide.page.in_order && wide.total.unwrap().in_order);

        // A trace_id filter is a projected column.
        let mut by_trace = query(2);
        by_trace.filter.trace_id = Some("abc".into());
        let narrow = plan(&by_trace, false).unwrap();
        assert!(!narrow.page.in_order && !narrow.total.unwrap().in_order);

        // At most 5000 candidate groups: hashing is fine, and the membership
        // subquery wants the projection.
        assert!(!plan(&query(2), true).unwrap().page.in_order);
    }

    #[test]
    fn environment_id_filters_are_rejected_like_every_other_global_source() {
        let mut q = query(1);
        q.filter.environment_id = Some(3);
        assert!(plan(&q, false).is_err());
    }

    #[test]
    fn attribute_filters_stay_bound_not_interpolated() {
        let mut q = query(1);
        q.filter
            .attributes
            .get_or_insert_default()
            .insert("http.method".into(), "GET' OR 1=1 --".into());
        let page = plan(&q, false).unwrap().page;
        let (sql, binds) = (page.sql, page.binds);
        assert!(!sql.contains("OR 1=1"));
        assert_eq!(placeholders(&sql), binds.len());
        assert!(sql.contains("JSONExtractString(attributes, ?) = ?"));
    }

    #[test]
    fn faceted_attribute_keys_filter_on_their_indexed_slot_column() {
        let mut q = query(1);
        let attributes = q.filter.attributes.get_or_insert_default();
        attributes.insert("tier".into(), "free".into());
        attributes.insert("region".into(), "eu".into());
        let facets = Facets::from([("tier".to_string(), 3u8)]);
        let page = super::plan(&q, false, &facets).unwrap().page;
        // Faceted: the slot column, no JSON parsing. Unfaceted: JSON fallback.
        assert!(page.sql.contains(" AND facet_attr_3 = ?"));
        assert!(page
            .sql
            .contains(" AND JSONExtractString(attributes, ?) = ?"));
        assert_eq!(placeholders(&page.sql), page.binds.len());
        let rendered = render(&page.sql, &page.binds);
        assert!(rendered.contains("facet_attr_3 = 'free'"));
        assert!(rendered.contains("JSONExtractString(attributes, 'region') = 'eu'"));
        // Both stages repeat the predicate.
        assert_eq!(page.sql.matches("facet_attr_3 = ?").count(), 2);
        assert!(!page.sql.contains("JSONExtractString(attributes, 'tier')"));
    }

    #[test]
    fn the_production_memory_error_is_recognised_and_reported_with_its_limit() {
        let production = ::clickhouse::error::Error::BadResponse(
            "Code: 241. DB::Exception: Query memory limit exceeded: would use 2.80 GiB \
             (attempt to allocate chunk of 4.05 MiB), maximum: 2.79 GiB: (while reading column \
             span_id): (MEMORY_LIMIT_EXCEEDED) (version 26.6)"
                .into(),
        );
        assert_eq!(classify(&production), Failure::MemoryLimit);
        let q = query(3);
        let error = failure("page", &q, production);
        let message = error.to_string();
        assert!(
            message.contains("page query exceeded its 512 MiB memory budget"),
            "{message}"
        );
        assert!(message.contains("3 project scope(s)"), "{message}");
        assert!(message.contains("window 1970-01-01T00:00:01"), "{message}");
    }

    #[test]
    fn a_server_side_timeout_is_a_typed_timeout() {
        let timeout = ::clickhouse::error::Error::BadResponse(
            "Code: 159. DB::Exception: Timeout exceeded: elapsed 30.1 seconds (TIMEOUT_EXCEEDED)"
                .into(),
        );
        assert_eq!(classify(&timeout), Failure::Timeout);
        match failure("total", &query(1), timeout) {
            OtelError::Storage { kind, message } => {
                assert_eq!(kind, StorageErrorKind::ClickHouseTimeout);
                assert!(message.contains("total query exceeded"), "{message}");
            }
            other => panic!("expected a storage error, got {other:?}"),
        }
        assert_eq!(
            classify(&::clickhouse::error::Error::BadResponse(
                "Code: 60. UNKNOWN_TABLE".into()
            )),
            Failure::Other
        );
    }
}
