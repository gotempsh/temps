//! Cross-project trace discovery service (ADR-027 Phases 0 and 2).
//!
//! `CrossProjectTraceService` owns two responsibilities:
//!
//! 1. **Hint recording** (Phase 0): after a successful span ingest, the ingest
//!    path fires a `TraceHintMsg` on a bounded mpsc channel.  A background
//!    consumer calls `record_hint` to insert `(trace_id, project_id)` rows into
//!    the `cross_project_trace_refs` control table.
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

    /// Database error while recording cross-project hints for a project.
    #[error("Database error recording cross-project hints for project {project_id}: {source}")]
    RecordHint {
        project_id: i32,
        #[source]
        source: sea_orm::DbErr,
    },

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

impl CrossProjectTraceService {
    pub fn new(db: Arc<DatabaseConnection>, storage: Arc<dyn OtelStorage>) -> Self {
        Self { db, storage }
    }

    // ── Write ───────────────────────────────────────────────────────────────

    /// Record that `project_id` holds spans for each trace_id in the set.
    ///
    /// Issues a single multi-row `INSERT … ON CONFLICT DO NOTHING` so
    /// subsequent batches for the same `(trace_id, project_id)` pair are
    /// cheaply discarded at the primary-key level.  Empty sets are no-ops.
    pub async fn record_hint(
        &self,
        trace_ids: HashSet<String>,
        project_id: i32,
    ) -> Result<(), CrossProjectTraceError> {
        if trace_ids.is_empty() {
            return Ok(());
        }

        // Build a parameterized multi-row INSERT to avoid N individual round-trips.
        // ($1, $2), ($3, $4), …  where odd params are trace_id and even are project_id.
        let ids_vec: Vec<String> = trace_ids.into_iter().collect();

        let mut sql =
            String::from("INSERT INTO cross_project_trace_refs (trace_id, project_id) VALUES ");
        let mut param_idx = 1u32;
        for (i, _) in ids_vec.iter().enumerate() {
            if i > 0 {
                sql.push_str(", ");
            }
            sql.push_str(&format!("(${param_idx}, ${})", param_idx + 1));
            param_idx += 2;
        }
        sql.push_str(" ON CONFLICT DO NOTHING");

        let mut values: Vec<sea_orm::Value> = Vec::with_capacity(ids_vec.len() * 2);
        for tid in &ids_vec {
            values.push(tid.clone().into());
            values.push(project_id.into());
        }

        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                values,
            ))
            .await
            .map_err(|e| CrossProjectTraceError::RecordHint {
                project_id,
                source: e,
            })?;

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
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                r#"SELECT r.project_id, p.name AS project_name, p.slug AS project_slug, r.first_seen
                   FROM cross_project_trace_refs r
                   JOIN projects p ON p.id = r.project_id
                   WHERE r.trace_id = $1
                     AND p.cross_project_trace_sharing = TRUE
                     AND ($2::integer IS NULL OR r.project_id != $2)
                   ORDER BY r.first_seen ASC"#,
                [
                    trace_id.into(),
                    match exclude_project_id {
                        Some(id) => sea_orm::Value::Int(Some(id)),
                        None => sea_orm::Value::Int(None),
                    },
                ],
            ))
            .await
            .map_err(|e| CrossProjectTraceError::QuerySiblings {
                trace_id: trace_id.to_string(),
                source: e,
            })?;

        let siblings = rows
            .iter()
            .filter_map(|row| {
                Some(SiblingRef {
                    project_id: row.try_get("", "project_id").ok()?,
                    project_name: row.try_get("", "project_name").ok()?,
                    project_slug: row.try_get("", "project_slug").ok()?,
                    first_seen: row.try_get("", "first_seen").ok()?,
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
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                r#"SELECT r.project_id, p.name AS project_name, p.slug AS project_slug, r.first_seen,
                          p.cross_project_trace_sharing AS sharing
                   FROM cross_project_trace_refs r
                   JOIN projects p ON p.id = r.project_id
                   WHERE r.trace_id = $1
                   ORDER BY r.first_seen ASC"#,
                [trace_id.into()],
            ))
            .await
            .map_err(|e| CrossProjectTraceError::QueryProjects {
                trace_id: trace_id.to_string(),
                source: e,
            })?;

        let refs = rows
            .iter()
            .filter_map(|row| {
                Some(TraceProjectRef {
                    project_id: row.try_get("", "project_id").ok()?,
                    project_name: row.try_get("", "project_name").ok()?,
                    project_slug: row.try_get("", "project_slug").ok()?,
                    first_seen: row.try_get("", "first_seen").ok()?,
                    sharing: row.try_get("", "sharing").ok()?,
                })
            })
            .collect();

        Ok(refs)
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

/// Delete `cross_project_trace_refs` rows older than 90 days.
///
/// Called by the daily background prune task in `plugin.rs`.  Returns the
/// number of deleted rows (for logging).  Errors are logged by the caller and
/// do not affect the prune schedule.
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
