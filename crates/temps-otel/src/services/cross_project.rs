// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Cross-project trace discovery service (ADR-027 Phases 0 and 2).
//!
//! `CrossProjectTraceService` owns two responsibilities:
//!
//! 1. **Hint recording** (Phase 0): after a successful span ingest, the ingest
//!    path fires a `TraceHintMsg` on a bounded mpsc channel.  A background
//!    consumer calls `record_hint` to insert `(trace_id, project_id)` rows via
//!    `OtelStorage::record_trace_refs` — the Postgres
//!    `cross_project_trace_refs` control table on the TimescaleDB backend, or
//!    the compressed ClickHouse table of the same name when ClickHouse is
//!    enabled (the raw tuples reached 125 GB of uncompressed Postgres heap +
//!    b-tree in three weeks; ClickHouse stores them at a small fraction of
//!    that).
//!
//! 2. **Discovery and unified waterfall** (Phases 1 & 2): `find_sibling_projects`
//!    powers the Phase 1 "also in" banner; `get_unified_trace` fans out to each
//!    contributing project's `OtelStorage::get_trace` call, annotates spans, and
//!    assembles the merged waterfall.

use std::collections::HashSet;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use futures::future::join_all;
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::warn;
use utoipa::ToSchema;

use crate::error::OtelError;
use crate::storage::OtelStorage;
use crate::types::{SpanRecord, SpanStatusCode};

// ── Error type ──────────────────────────────────────────────────────────────

/// Domain error for cross-project trace operations.
#[derive(Error, Debug)]
pub enum CrossProjectTraceError {
    /// trace_id failed the 32-character lowercase hex format check.
    #[error("Invalid trace_id '{trace_id}': expected exactly 32 lowercase hex characters")]
    InvalidTraceId { trace_id: String },

    /// Database error while querying sibling projects for a trace.
    #[error("Database error querying sibling projects for trace {trace_id}: {source}")]
    QuerySiblings {
        trace_id: String,
        #[source]
        source: sea_orm::DbErr,
    },

    /// Database error while querying all projects for a trace.
    #[error("Database error querying trace projects for trace {trace_id}: {source}")]
    QueryProjects {
        trace_id: String,
        #[source]
        source: sea_orm::DbErr,
    },

    /// Catch-all database error for `?`-operator conversions.
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    /// Catch-all storage error for `?`-operator conversions.
    #[error("Storage error: {0}")]
    Storage(#[from] OtelError),
}

// ── Validation ──────────────────────────────────────────────────────────────

/// Returns `true` iff `s` is exactly 32 lowercase hexadecimal characters
/// (the W3C trace-context `trace_id` format as stored verbatim from OTLP).
pub fn is_valid_trace_id(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

// ── Channel message type ────────────────────────────────────────────────────

/// Payload sent on the bounded mpsc channel from the ingest path to the
/// background hint-writer task.  Capacity is 1,000 messages; when full,
/// the sending side drops the message (see `do_ingest_traces`).
#[derive(Debug)]
pub struct TraceHintMsg {
    /// Distinct trace_ids decoded from a single OTLP batch — typically 1–3.
    pub trace_ids: HashSet<String>,
    /// Project that ingested these spans.
    pub project_id: i32,
}

// ── Output types ────────────────────────────────────────────────────────────

/// A sibling project that shares the same `trace_id` and has opted in to
/// cross-project trace sharing (`cross_project_trace_sharing = TRUE`).
///
/// Returned by `CrossProjectTraceService::find_sibling_projects` and exposed
/// by the Phase 1 `GET /otel/traces/cross-project/{trace_id}` endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SiblingRef {
    pub project_id: i32,
    pub project_name: String,
    /// URL slug used to link into the sibling project's single-project trace view.
    pub project_slug: String,
    #[schema(value_type = String, format = DateTime)]
    pub first_seen: DateTime<Utc>,
}

/// All projects that contributed spans to a trace, including their sharing flag.
///
/// Returned by `CrossProjectTraceService::find_trace_projects`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TraceProjectRef {
    pub project_id: i32,
    pub project_name: String,
    /// URL slug used to link into the project's single-project trace view.
    pub project_slug: String,
    #[schema(value_type = String, format = DateTime)]
    pub first_seen: DateTime<Utc>,
    /// Whether this project has `cross_project_trace_sharing = true`.
    pub sharing: bool,
}

/// A lightweight project descriptor included in `UnifiedTrace`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProjectRef {
    pub project_id: i32,
    pub project_name: String,
    /// URL slug used to link a span back into its owning project's trace view.
    pub project_slug: String,
}

/// A single span annotated with the project that originally stored it.
/// Used in `UnifiedTrace` to let the UI colour-code spans by project.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AnnotatedSpan {
    /// The project that stored this span (same as `span.project_id`).
    pub project_id: i32,
    /// Human-readable project name for waterfall colour-coding and legend.
    pub project_name: String,
    /// Original span data verbatim from storage.
    pub span: SpanRecord,
}

/// Merged cross-project trace result (Phase 2 unified waterfall).
///
/// Spans are sorted by `start_time ASC`.  At most 20 projects and 10,000
/// spans total are included; `truncated` / `truncated_projects` signal when
/// the caps were hit.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UnifiedTrace {
    pub trace_id: String,
    /// Projects that contributed spans to this result set.
    pub projects: Vec<ProjectRef>,
    /// Annotated, merged span list sorted by `start_time ASC`.
    pub spans: Vec<AnnotatedSpan>,
    #[schema(value_type = String, format = DateTime)]
    pub start_time: Option<DateTime<Utc>>,
    #[schema(value_type = String, format = DateTime)]
    pub end_time: Option<DateTime<Utc>>,
    /// Trace wall-clock duration in milliseconds (`end_time – start_time`).
    pub total_duration_ms: f64,
    pub span_count: usize,
    pub error_count: usize,
    /// `true` when at least one project has `cross_project_trace_sharing = false`
    /// and its spans were therefore excluded from the result set.
    pub has_redacted_spans: bool,
    /// `true` when the 20-project or 10,000-span cap was hit.
    pub truncated: bool,
    /// project_ids excluded due to truncation (most-recent first_seen dropped first).
    pub truncated_projects: Vec<i32>,
}

// ── Service ─────────────────────────────────────────────────────────────────

/// Maximum number of projects fanned out to in `get_unified_trace`.
///
/// Projects are kept oldest-first (by `first_seen ASC`).  Projects beyond
/// this cap are dropped most-recent-first and listed in `truncated_projects`.
const MAX_FAN_OUT_PROJECTS: usize = 20;

/// Maximum total spans returned by `get_unified_trace`.
///
/// Per-project slot is `MAX_TOTAL_SPANS / kept_project_count`.  Spans beyond
/// the slot cap are truncated within each project result set.
const MAX_TOTAL_SPANS: usize = 10_000;

/// Service for cross-project trace discovery and unified waterfall assembly.
pub struct CrossProjectTraceService {
    db: Arc<DatabaseConnection>,
    storage: Arc<dyn OtelStorage>,
}

/// Project metadata joined onto trace refs from the Postgres `projects` table.
struct ProjectMeta {
    name: String,
    slug: String,
    sharing: bool,
}

impl CrossProjectTraceService {
    pub fn new(db: Arc<DatabaseConnection>, storage: Arc<dyn OtelStorage>) -> Self {
        Self { db, storage }
    }

    // ── Write ───────────────────────────────────────────────────────────────

    /// Record that `project_id` holds spans for each trace_id in the set.
    ///
    /// Delegates to `OtelStorage::record_trace_refs` so the ref lands on the
    /// active backend (Postgres control table, or the ClickHouse table when
    /// ClickHouse is enabled). Both backends give the `(trace_id, project_id)`
    /// pair first-write-wins semantics, so re-recording is cheap and never
    /// moves `first_seen`.  Empty sets are no-ops.
    pub async fn record_hint(
        &self,
        trace_ids: HashSet<String>,
        project_id: i32,
    ) -> Result<(), CrossProjectTraceError> {
        if trace_ids.is_empty() {
            return Ok(());
        }
        let ids_vec: Vec<String> = trace_ids.into_iter().collect();
        self.storage.record_trace_refs(&ids_vec, project_id).await?;
        Ok(())
    }

    /// For a batch of `trace_id`s, resolve the "canonical" root span name +
    /// service from whichever **sharing** project owns the trace's root span.
    ///
    /// Used to name trace-list rows whose local project holds only child spans
    /// (which would otherwise render as "(unnamed)"). Best-effort: trace_ids
    /// with no discoverable root — or whose root lives in an opted-out project —
    /// are simply absent from the returned map. Returns
    /// `trace_id -> (root_span_name, service_name)`.
    pub async fn resolve_root_names(
        &self,
        trace_ids: &[String],
    ) -> Result<std::collections::HashMap<String, (String, String)>, CrossProjectTraceError> {
        use std::collections::HashMap;
        if trace_ids.is_empty() {
            return Ok(HashMap::new());
        }

        // Pick one root-owning summary per trace, from a sharing project only.
        //   WHERE s.trace_id IN ($1, $2, …)
        let mut sql = String::from(
            "SELECT DISTINCT ON (s.trace_id) s.trace_id, s.root_span_name, s.service_name \
             FROM otel_trace_summaries s \
             JOIN projects p ON p.id = s.project_id \
             WHERE s.has_root = TRUE AND s.root_span_name <> '' \
               AND p.cross_project_trace_sharing = TRUE \
               AND s.trace_id IN (",
        );
        for i in 0..trace_ids.len() {
            if i > 0 {
                sql.push_str(", ");
            }
            sql.push_str(&format!("${}", i + 1));
        }
        sql.push_str(") ORDER BY s.trace_id, s.has_root DESC");

        let values: Vec<sea_orm::Value> = trace_ids.iter().map(|t| t.clone().into()).collect();

        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                values,
            ))
            .await
            .map_err(|e| CrossProjectTraceError::QueryProjects {
                trace_id: format!("batch of {}", trace_ids.len()),
                source: e,
            })?;

        let mut map = HashMap::with_capacity(rows.len());
        for row in &rows {
            let tid: String = match row.try_get("", "trace_id") {
                Ok(v) => v,
                Err(_) => continue,
            };
            let name: String = row.try_get("", "root_span_name").unwrap_or_default();
            let svc: String = row.try_get("", "service_name").unwrap_or_default();
            map.insert(tid, (name, svc));
        }
        Ok(map)
    }

    // ── Phase 1: sibling discovery ──────────────────────────────────────────

    /// Returns projects that share `trace_id`, excluding `exclude_project_id`
    /// and any project with `cross_project_trace_sharing = false`.
    ///
    /// Results are ordered by `first_seen ASC` (earliest observer first).
    /// Returns an empty `Vec` — never 404 — when no siblings exist.
    pub async fn find_sibling_projects(
        &self,
        trace_id: &str,
        exclude_project_id: Option<i32>,
    ) -> Result<Vec<SiblingRef>, CrossProjectTraceError> {
        // Refs come from the active storage backend; project metadata always
        // lives in Postgres, so it is joined in a second, tiny query.
        let mut refs = self.storage.get_trace_ref_projects(trace_id).await?;
        refs.sort_by_key(|r| r.first_seen);

        let ids: Vec<i32> = refs.iter().map(|r| r.project_id).collect();
        let meta = self.load_project_meta(&ids).await.map_err(|e| {
            CrossProjectTraceError::QuerySiblings {
                trace_id: trace_id.to_string(),
                source: e,
            }
        })?;

        // Refs whose project no longer exists are dropped (JOIN semantics).
        let siblings = refs
            .into_iter()
            .filter(|r| exclude_project_id != Some(r.project_id))
            .filter_map(|r| {
                let m = meta.get(&r.project_id)?;
                if !m.sharing {
                    return None;
                }
                Some(SiblingRef {
                    project_id: r.project_id,
                    project_name: m.name.clone(),
                    project_slug: m.slug.clone(),
                    first_seen: r.first_seen,
                })
            })
            .collect();

        Ok(siblings)
    }

    // ── Phase 2: unified waterfall helpers ──────────────────────────────────

    /// Returns ALL projects that hold spans for `trace_id`, including their
    /// `cross_project_trace_sharing` flag (used by `get_unified_trace` to
    /// partition shared vs. opted-out projects).
    ///
    /// Results are ordered by `first_seen ASC`.
    pub async fn find_trace_projects(
        &self,
        trace_id: &str,
    ) -> Result<Vec<TraceProjectRef>, CrossProjectTraceError> {
        let mut refs = self.storage.get_trace_ref_projects(trace_id).await?;
        refs.sort_by_key(|r| r.first_seen);

        let ids: Vec<i32> = refs.iter().map(|r| r.project_id).collect();
        let meta = self.load_project_meta(&ids).await.map_err(|e| {
            CrossProjectTraceError::QueryProjects {
                trace_id: trace_id.to_string(),
                source: e,
            }
        })?;

        // Refs whose project no longer exists are dropped (JOIN semantics).
        let refs = refs
            .into_iter()
            .filter_map(|r| {
                let m = meta.get(&r.project_id)?;
                Some(TraceProjectRef {
                    project_id: r.project_id,
                    project_name: m.name.clone(),
                    project_slug: m.slug.clone(),
                    first_seen: r.first_seen,
                    sharing: m.sharing,
                })
            })
            .collect();

        Ok(refs)
    }

    /// Fetch name / slug / sharing flag for a set of project ids from the
    /// Postgres `projects` table. Returns a map keyed by project id; ids with
    /// no matching project are simply absent.
    async fn load_project_meta(
        &self,
        project_ids: &[i32],
    ) -> Result<std::collections::HashMap<i32, ProjectMeta>, sea_orm::DbErr> {
        use std::collections::HashMap;
        if project_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let mut sql = String::from(
            "SELECT id, name, slug, cross_project_trace_sharing FROM projects WHERE id IN (",
        );
        for i in 0..project_ids.len() {
            if i > 0 {
                sql.push_str(", ");
            }
            sql.push_str(&format!("${}", i + 1));
        }
        sql.push(')');

        let values: Vec<sea_orm::Value> = project_ids.iter().map(|id| (*id).into()).collect();
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                values,
            ))
            .await?;

        let mut map = HashMap::with_capacity(rows.len());
        for row in &rows {
            let id: i32 = match row.try_get("", "id") {
                Ok(v) => v,
                Err(_) => continue,
            };
            map.insert(
                id,
                ProjectMeta {
                    name: row.try_get("", "name").unwrap_or_default(),
                    slug: row.try_get("", "slug").unwrap_or_default(),
                    sharing: row
                        .try_get("", "cross_project_trace_sharing")
                        .unwrap_or(false),
                },
            );
        }
        Ok(map)
    }

    // ── Phase 2: unified waterfall assembly ─────────────────────────────────

    /// Assembles a merged, cross-project span waterfall for `trace_id`.
    ///
    /// **Algorithm:**
    /// 1. Call `find_trace_projects` to discover all projects.
    /// 2. Partition into shared (sharing=true) vs. opted-out (sharing=false).
    ///    Set `has_redacted_spans` if any project opted out.
    /// 3. Cap to `MAX_FAN_OUT_PROJECTS` (20).  Projects are ordered by
    ///    `first_seen ASC` (oldest first); excess projects are dropped from the
    ///    tail (most recent) and recorded in `truncated_projects`.
    /// 4. Fan out `OtelStorage::get_trace` calls via `join_all`.  Per-project
    ///    failures are warned and produce an empty span list (non-fatal).
    /// 5. Apply per-project slot cap (`MAX_TOTAL_SPANS / kept_project_count`).
    /// 6. Apply global `MAX_TOTAL_SPANS` cap.
    /// 7. Sort merged spans by `start_time ASC`.
    /// 8. Compute `start_time`, `end_time`, `total_duration_ms`, `span_count`,
    ///    `error_count`.
    pub async fn get_unified_trace(
        &self,
        trace_id: &str,
    ) -> Result<UnifiedTrace, CrossProjectTraceError> {
        // 1. Discover all projects for this trace.
        let all_projects = self.find_trace_projects(trace_id).await?;

        // 2. Partition into shared vs. opted-out.
        let has_redacted_spans = all_projects.iter().any(|p| !p.sharing);

        // Shared projects are already ordered by first_seen ASC from the query.
        let mut shared: Vec<TraceProjectRef> =
            all_projects.into_iter().filter(|p| p.sharing).collect();

        // 3. Apply 20-project cap — drop most-recent projects (tail of the
        //    first_seen ASC list) and record their IDs in truncated_projects.
        let mut truncated_projects: Vec<i32> = Vec::new();
        let mut truncated = has_redacted_spans;

        if shared.len() > MAX_FAN_OUT_PROJECTS {
            let dropped_iter = shared.drain(MAX_FAN_OUT_PROJECTS..);
            truncated_projects.extend(dropped_iter.map(|p| p.project_id));
            truncated = true;
        }

        // 4. Per-project span slot.
        let slot = if shared.is_empty() {
            MAX_TOTAL_SPANS
        } else {
            MAX_TOTAL_SPANS / shared.len()
        };

        // 5. Fan-out: issue `get_trace` calls in parallel.
        //    Failures are warned and produce an empty span list so a single
        //    unavailable project does not fail the whole request.
        let storage = self.storage.clone();
        let trace_id_str = trace_id.to_string();

        let fetch_futs = shared.into_iter().map(|proj| {
            let tid = trace_id_str.clone();
            let st = storage.clone();
            async move {
                let result = st.get_trace(proj.project_id, &tid).await;
                match result {
                    Ok(spans) => (proj.project_id, proj.project_name, proj.project_slug, spans),
                    Err(e) => {
                        warn!(
                            trace_id = %tid,
                            project_id = proj.project_id,
                            error = %e,
                            "Failed to fetch spans during cross-project trace fan-out; \
                             project omitted from unified result"
                        );
                        (
                            proj.project_id,
                            proj.project_name,
                            proj.project_slug,
                            Vec::new(),
                        )
                    }
                }
            }
        });
        let results = join_all(fetch_futs).await;

        // 6. Annotate spans, apply per-project slot cap, collect projects.
        let mut all_spans: Vec<AnnotatedSpan> = Vec::new();
        let mut projects: Vec<ProjectRef> = Vec::new();

        for (project_id, project_name, project_slug, mut spans) in results {
            if spans.len() > slot {
                spans.truncate(slot);
                truncated = true;
            }
            if !spans.is_empty() {
                projects.push(ProjectRef {
                    project_id,
                    project_name: project_name.clone(),
                    project_slug,
                });
            }
            for span in spans {
                all_spans.push(AnnotatedSpan {
                    project_id,
                    project_name: project_name.clone(),
                    span,
                });
            }
        }

        // 7. Global span cap.
        if all_spans.len() > MAX_TOTAL_SPANS {
            all_spans.truncate(MAX_TOTAL_SPANS);
            truncated = true;
        }

        // 8. Sort by start_time ASC for waterfall rendering.
        all_spans.sort_by_key(|a| a.span.start_time);

        // 9. Compute trace-level stats.
        let start_time = all_spans.iter().map(|a| a.span.start_time).min();
        let end_time = all_spans.iter().map(|a| a.span.end_time).max();
        let total_duration_ms = match (start_time, end_time) {
            (Some(s), Some(e)) if e >= s => (e - s).num_milliseconds().max(0) as f64,
            _ => 0.0,
        };
        let span_count = all_spans.len();
        let error_count = all_spans
            .iter()
            .filter(|a| matches!(a.span.status_code, SpanStatusCode::Error))
            .count();

        Ok(UnifiedTrace {
            trace_id: trace_id.to_string(),
            projects,
            spans: all_spans,
            start_time,
            end_time,
            total_duration_ms,
            span_count,
            error_count,
            has_redacted_spans,
            truncated,
            truncated_projects,
        })
    }
}

// ── Prune helper ─────────────────────────────────────────────────────────────

/// Delete Postgres `cross_project_trace_refs` rows older than 90 days.
///
/// Called by the daily background prune task in `plugin.rs`.  Returns the
/// number of deleted rows (for logging).  Errors are logged by the caller and
/// do not affect the prune schedule.
///
/// This targets the POSTGRES control table only. When the ClickHouse backend
/// is active, new refs land in ClickHouse (expired natively by per-row TTL)
/// and this task's remaining job is draining the legacy Postgres rows written
/// before the cutover — after one retention window it deletes nothing.
pub async fn prune_stale_hints(db: &DatabaseConnection) -> Result<u64, sea_orm::DbErr> {
    let result = db
        .execute(Statement::from_string(
            DatabaseBackend::Postgres,
            "DELETE FROM cross_project_trace_refs \
             WHERE first_seen < NOW() - INTERVAL '90 days'"
                .to_string(),
        ))
        .await?;
    Ok(result.rows_affected())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_valid_trace_id ────────────────────────────────────────────────────

    #[test]
    fn test_valid_trace_id_accepted() {
        // Standard 32-char lowercase hex (real W3C trace-context format).
        assert!(is_valid_trace_id("4bf92f3577b34da6a3ce929d0e0e4736"));
        assert!(is_valid_trace_id("0".repeat(32).as_str()));
        assert!(is_valid_trace_id("abcdef1234567890abcdef1234567890"));
    }

    #[test]
    fn test_invalid_trace_id_wrong_length() {
        assert!(!is_valid_trace_id("4bf92f3577b34da6a3ce929d0e0e473")); // 31 chars
        assert!(!is_valid_trace_id("4bf92f3577b34da6a3ce929d0e0e47361")); // 33 chars
        assert!(!is_valid_trace_id(""));
    }

    #[test]
    fn test_invalid_trace_id_uppercase_rejected() {
        // OTLP encodes trace_ids as lowercase hex; uppercase is not a valid hint.
        assert!(!is_valid_trace_id("4BF92F3577B34DA6A3CE929D0E0E4736"));
        assert!(!is_valid_trace_id("4Bf92f3577b34da6a3ce929d0e0e4736"));
    }

    #[test]
    fn test_invalid_trace_id_non_hex() {
        assert!(!is_valid_trace_id("4bf92f3577b34da6a3ce929d0e0e473z")); // 'z'
        assert!(!is_valid_trace_id("4bf92f3577b34da6-3ce929d0e0e4736")); // '-'
        assert!(!is_valid_trace_id("4bf92f3577b34da6 3ce929d0e0e4736")); // space
    }

    // ── UnifiedTrace stats computation ────────────────────────────────────────

    use crate::types::{ResourceInfo, SpanKind, SpanStatusCode};
    use chrono::Duration;
    use std::collections::BTreeMap;

    fn make_span(
        project_id: i32,
        trace_id: &str,
        status_code: SpanStatusCode,
        start_offset_ms: i64,
        duration_ms: f64,
    ) -> SpanRecord {
        let start_time = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap()
            + Duration::milliseconds(start_offset_ms);
        let end_time = start_time + Duration::milliseconds(duration_ms as i64);
        SpanRecord {
            project_id,
            deployment_id: None,
            resource: ResourceInfo {
                service_name: "test-service".to_string(),
                service_version: None,
                deployment_environment: None,
                attributes: BTreeMap::new(),
            },
            trace_id: trace_id.to_string(),
            span_id: format!("span_{project_id}_{start_offset_ms}"),
            parent_span_id: None,
            name: "test-span".to_string(),
            kind: SpanKind::Server,
            start_time,
            end_time,
            duration_ms,
            status_code,
            status_message: String::new(),
            attributes: BTreeMap::new(),
            events: Vec::new(),
        }
    }

    #[test]
    fn test_unified_trace_span_count_and_error_count() {
        // Build a small annotated span list directly to verify stat computation.
        let spans = [
            AnnotatedSpan {
                project_id: 1,
                project_name: "svc-a".to_string(),
                span: make_span(1, "aa", SpanStatusCode::Ok, 0, 100.0),
            },
            AnnotatedSpan {
                project_id: 2,
                project_name: "svc-b".to_string(),
                span: make_span(2, "aa", SpanStatusCode::Error, 50, 200.0),
            },
            AnnotatedSpan {
                project_id: 1,
                project_name: "svc-a".to_string(),
                span: make_span(1, "aa", SpanStatusCode::Error, 10, 80.0),
            },
        ];

        let span_count = spans.len();
        let error_count = spans
            .iter()
            .filter(|a| matches!(a.span.status_code, SpanStatusCode::Error))
            .count();

        assert_eq!(span_count, 3);
        assert_eq!(error_count, 2);
    }

    #[test]
    fn test_trace_hint_msg_construction() {
        let mut ids = HashSet::new();
        ids.insert("4bf92f3577b34da6a3ce929d0e0e4736".to_string());
        ids.insert("abcdef1234567890abcdef1234567890".to_string());
        let msg = TraceHintMsg {
            trace_ids: ids.clone(),
            project_id: 42,
        };
        assert_eq!(msg.project_id, 42);
        assert_eq!(msg.trace_ids.len(), 2);
    }

    // ── Service paths over MockOtelStorage + MockDatabase ────────────────────

    fn meta_row(
        id: i32,
        name: &str,
        slug: &str,
        sharing: bool,
    ) -> std::collections::BTreeMap<String, sea_orm::Value> {
        [
            ("id".to_string(), sea_orm::Value::from(id)),
            ("name".to_string(), sea_orm::Value::from(name)),
            ("slug".to_string(), sea_orm::Value::from(slug)),
            (
                "cross_project_trace_sharing".to_string(),
                sea_orm::Value::from(sharing),
            ),
        ]
        .into_iter()
        .collect()
    }

    #[tokio::test]
    async fn record_hint_dedupes_pairs_first_write_wins() {
        use crate::test_support::MockOtelStorage;
        use sea_orm::MockDatabase;

        let storage = Arc::new(MockOtelStorage::new());
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let svc = CrossProjectTraceService::new(db, storage.clone());

        let tid = "4bf92f3577b34da6a3ce929d0e0e4736".to_string();
        let mut ids = HashSet::new();
        ids.insert(tid.clone());

        svc.record_hint(ids.clone(), 7).await.unwrap();
        let first_seen = storage.trace_refs.lock().unwrap()[&(tid.clone(), 7)];
        svc.record_hint(ids, 7).await.unwrap();

        let refs = storage.trace_refs.lock().unwrap();
        assert_eq!(refs.len(), 1, "re-recording must not duplicate the pair");
        assert_eq!(refs[&(tid, 7)], first_seen, "first_seen must not move");
    }

    #[tokio::test]
    async fn discovery_filters_sharing_exclusion_and_deleted_projects() {
        use crate::test_support::MockOtelStorage;
        use sea_orm::MockDatabase;

        let storage = Arc::new(MockOtelStorage::new());
        let tid = "4bf92f3577b34da6a3ce929d0e0e4736".to_string();
        for project_id in [1, 2, 3, 4] {
            storage
                .record_trace_refs(std::slice::from_ref(&tid), project_id)
                .await
                .unwrap();
        }

        // Project meta: 1 shares, 2 opted out, 3 shares (used as `exclude`),
        // project 4 is absent (deleted) — one result set per service call.
        let meta = vec![
            meta_row(1, "alpha", "alpha", true),
            meta_row(2, "beta", "beta", false),
            meta_row(3, "gamma", "gamma", true),
        ];
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![meta.clone(), meta])
                .into_connection(),
        );
        let svc = CrossProjectTraceService::new(db, storage);

        // Siblings: opted-out (2), excluded (3), and deleted (4) all drop out.
        let siblings = svc.find_sibling_projects(&tid, Some(3)).await.unwrap();
        assert_eq!(siblings.len(), 1);
        assert_eq!(siblings[0].project_id, 1);
        assert_eq!(siblings[0].project_slug, "alpha");

        // find_trace_projects keeps opted-out projects (with their flag) but
        // still drops deleted ones.
        let mut all = svc.find_trace_projects(&tid).await.unwrap();
        all.sort_by_key(|p| p.project_id);
        assert_eq!(
            all.iter().map(|p| p.project_id).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(!all[1].sharing);
    }

    #[test]
    fn test_is_valid_trace_id_boundary() {
        // Exactly 32 all-zeros is valid.
        let s = "0".repeat(32);
        assert!(is_valid_trace_id(&s));
        // 31 chars is not valid.
        let short = "0".repeat(31);
        assert!(!is_valid_trace_id(&short));
    }
}
