// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Performance issues from stored traces (ADR 111, D2).
//!
//! A background job, never ingest: every tick it takes the projects checked
//! longest ago (at most [`PROJECTS_PER_TICK`]), reads the slowest root traces
//! each one stored since its last check (at most [`TRACES_PER_PROJECT`],
//! [`TRACES_PER_TICK`] in all), runs the detectors over them, and files each
//! finding through the normal error ingest path as a group of type
//! `performance`. That path deduplicates by occurrence, so a trace read twice
//! counts once, and it gives the issue the error workflow and alert rules.
//!
//! The window never reaches back more than [`MAX_LOOKBACK`]: a project that
//! stores more traces than the budget is sampled (its slowest traces per
//! window), not queued.

pub mod detectors;

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};
use temps_otel::storage::OtelStorage;
use temps_otel::types::{SortOrder, TraceQuery, TraceSortField};

use super::error_tracking_service::ErrorTrackingService;
use super::types::{CreateErrorEventData, ErrorTrackingError};
use detectors::Finding;

/// Projects checked per tick.
pub const PROJECTS_PER_TICK: u64 = 20;
/// Traces read per project per tick.
pub const TRACES_PER_PROJECT: u64 = 20;
/// Traces read per tick, across projects.
pub const TRACES_PER_TICK: usize = 200;
/// Spans read from one trace; a larger trace is checked on its first ones.
pub const MAX_SPANS_PER_TRACE: usize = 2_000;
/// How far back a window may start.
pub const MAX_LOOKBACK: Duration = Duration::minutes(15);
/// Traces younger than this may still be receiving spans.
pub const SETTLE: Duration = Duration::seconds(60);
/// Time between ticks.
pub const TICK: std::time::Duration = std::time::Duration::from_secs(300);

/// What the scanner last did for a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanState {
    pub scanned_until: DateTime<Utc>,
    pub last_run_at: DateTime<Utc>,
    pub traces_scanned: i64,
    pub issues_found: i64,
    pub last_error: Option<String>,
}

/// One tick's totals.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TickReport {
    pub projects: usize,
    pub traces: usize,
    pub findings: usize,
    pub stored: usize,
}

pub struct PerformanceScanner {
    db: Arc<DatabaseConnection>,
    storage: Arc<dyn OtelStorage>,
    tracking: Arc<ErrorTrackingService>,
}

/// The ingest input for one finding.
pub fn event_for(
    project_id: i32,
    finding: &Finding,
    deployment_id: Option<i32>,
    release: Option<String>,
    environment: Option<String>,
    at: DateTime<Utc>,
) -> CreateErrorEventData {
    CreateErrorEventData {
        source: Some("performance".to_string()),
        issue_type: Some("performance".to_string()),
        dedup_key: Some(finding.dedup_key()),
        grouping_key: Some(finding.grouping_key()),
        exception_type: Some(finding.kind.title().to_string()),
        exception_value: Some(finding.summary.clone()),
        transaction_name: Some(finding.operation.clone()),
        server_name: Some(finding.service.clone()),
        release_version: release,
        environment,
        deployment_id,
        extra_context: Some(serde_json::json!({
            "performance": {
                "detector": finding.kind.key(),
                "trace_id": finding.trace_id,
                "span_ids": finding.span_ids,
                "occurrences": finding.occurrences,
                "duration_ms": finding.duration_ms,
                "pattern": finding.pattern,
                "operation": finding.operation,
                "service": finding.service,
                "trace_started_at": at,
            }
        })),
        project_id,
        ..Default::default()
    }
}

impl PerformanceScanner {
    pub fn new(
        db: Arc<DatabaseConnection>,
        storage: Arc<dyn OtelStorage>,
        tracking: Arc<ErrorTrackingService>,
    ) -> Self {
        Self {
            db,
            storage,
            tracking,
        }
    }

    /// Run forever, one tick every [`TICK`].
    pub async fn run(self) {
        loop {
            tokio::time::sleep(TICK).await;
            match self.tick(Utc::now()).await {
                Ok(report) if report.findings > 0 => tracing::info!(
                    projects = report.projects,
                    traces = report.traces,
                    findings = report.findings,
                    stored = report.stored,
                    "Performance issue scan finished"
                ),
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!("Performance issue scan could not pick projects: {}", error)
                }
            }
        }
    }

    /// One tick at `now`.
    pub async fn tick(&self, now: DateTime<Utc>) -> Result<TickReport, ErrorTrackingError> {
        let projects = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT p.id, s.scanned_until FROM projects p \
                 LEFT JOIN performance_scan_state s ON s.project_id = p.id \
                 WHERE NOT p.is_deleted \
                 ORDER BY s.last_run_at ASC NULLS FIRST, p.id LIMIT $1",
                [(PROJECTS_PER_TICK as i64).into()],
            ))
            .await?;
        let mut report = TickReport::default();
        let window_end = now - SETTLE;
        for row in projects {
            if report.traces >= TRACES_PER_TICK {
                break;
            }
            let project_id: i32 = row.try_get("", "id")?;
            let scanned_until: Option<DateTime<Utc>> = row.try_get("", "scanned_until")?;
            let window_start =
                scanned_until.map_or(now - MAX_LOOKBACK, |until| until.max(now - MAX_LOOKBACK));
            report.projects += 1;
            let budget = (TRACES_PER_TICK - report.traces).min(TRACES_PER_PROJECT as usize);
            let outcome = if window_end > window_start {
                self.scan_project(project_id, window_start, window_end, budget)
                    .await
            } else {
                Ok((0, 0, 0))
            };
            let (traces, findings, stored, error) = match outcome {
                Ok((traces, findings, stored)) => (traces, findings, stored, None),
                Err(error) => {
                    tracing::warn!(
                        project_id,
                        "Performance issue scan of project failed: {}",
                        error
                    );
                    (0, 0, 0, Some(error))
                }
            };
            report.traces += traces;
            report.findings += findings;
            report.stored += stored;
            // A failed window is not retried: the next tick moves on, so one
            // bad trace cannot stall a project.
            self.save_state(
                project_id,
                window_end.max(window_start),
                traces,
                stored,
                error,
            )
            .await?;
        }
        Ok(report)
    }

    async fn scan_project(
        &self,
        project_id: i32,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        budget: usize,
    ) -> Result<(usize, usize, usize), String> {
        let summaries = self
            .storage
            .query_trace_summaries(TraceQuery {
                project_id,
                start_time: Some(start),
                end_time: Some(end),
                root_only: true,
                sort_by: TraceSortField::Duration,
                sort_order: SortOrder::Desc,
                limit: Some(budget as u64),
                ..Default::default()
            })
            .await
            .map_err(|error| format!("listing traces failed: {error}"))?;
        let (mut traces, mut findings, mut stored) = (0, 0, 0);
        for summary in summaries.into_iter().take(budget) {
            let mut spans = self
                .storage
                .get_trace(project_id, &summary.trace_id)
                .await
                .map_err(|error| format!("reading trace {} failed: {error}", summary.trace_id))?;
            spans.truncate(MAX_SPANS_PER_TRACE);
            traces += 1;
            let deployment_id = spans.iter().find_map(|span| span.deployment_id);
            let release = spans
                .iter()
                .find_map(|span| span.resource.service_version.clone());
            let environment = spans
                .iter()
                .find_map(|span| span.resource.deployment_environment.clone());
            for finding in detectors::detect(&spans) {
                findings += 1;
                let event = event_for(
                    project_id,
                    &finding,
                    deployment_id,
                    release.clone(),
                    environment.clone(),
                    summary.start_time,
                );
                match self.tracking.process_error_event_with_receipt(event).await {
                    Ok(receipt) if receipt.stored => stored += 1,
                    Ok(_) => {}
                    Err(error) => {
                        return Err(format!(
                            "filing a {} issue for trace {} failed: {error}",
                            finding.kind.key(),
                            summary.trace_id
                        ))
                    }
                }
            }
        }
        Ok((traces, findings, stored))
    }

    async fn save_state(
        &self,
        project_id: i32,
        scanned_until: DateTime<Utc>,
        traces: usize,
        stored: usize,
        error: Option<String>,
    ) -> Result<(), ErrorTrackingError> {
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO performance_scan_state \
                     (project_id, scanned_until, last_run_at, traces_scanned, issues_found, last_error) \
                 VALUES ($1, $2, NOW(), $3, $4, $5) \
                 ON CONFLICT (project_id) DO UPDATE SET scanned_until = EXCLUDED.scanned_until, \
                     last_run_at = NOW(), \
                     traces_scanned = performance_scan_state.traces_scanned + EXCLUDED.traces_scanned, \
                     issues_found = performance_scan_state.issues_found + EXCLUDED.issues_found, \
                     last_error = EXCLUDED.last_error",
                [
                    project_id.into(),
                    scanned_until.into(),
                    (traces as i64).into(),
                    (stored as i64).into(),
                    error.into(),
                ],
            ))
            .await?;
        Ok(())
    }
}

/// What the scanner last did for a project; `None` before its first check.
pub async fn scan_state<C: ConnectionTrait>(
    db: &C,
    project_id: i32,
) -> Result<Option<ScanState>, ErrorTrackingError> {
    let row = db
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT scanned_until, last_run_at, traces_scanned, issues_found, last_error \
             FROM performance_scan_state WHERE project_id = $1",
            [project_id.into()],
        ))
        .await?;
    row.map(|row| {
        Ok(ScanState {
            scanned_until: row.try_get("", "scanned_until")?,
            last_run_at: row.try_get("", "last_run_at")?,
            traces_scanned: row.try_get("", "traces_scanned")?,
            issues_found: row.try_get("", "issues_found")?,
            last_error: row.try_get("", "last_error")?,
        })
    })
    .transpose()
}

#[cfg(test)]
mod tests;
