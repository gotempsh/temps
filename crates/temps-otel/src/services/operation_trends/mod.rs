// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-operation trends and regressions after a deployment (ADR 111, D2).
//!
//! A background job rolls the span-stats report up into one row per
//! project, service, operation and hour (`operation_rollups`). Span names are
//! user controlled, so each hour keeps at most [`MAX_TRACKED_OPERATIONS`]
//! operations per project, the ones that took the most total time; how many
//! did not fit is recorded in `operation_rollup_state` and reported, never
//! dropped silently.
//!
//! A trend reads those rows for one operation. A regression check compares
//! each operation's hourly p95 after a deployment went live with the robust
//! band ([`crate::detectors::robust_band`], the anomaly detectors' model) of
//! the week before it.

pub mod breakdown;
pub mod page_backend;

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Duration, DurationRound, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement, TransactionTrait, Value,
};

use crate::detectors::{robust_band, MIN_BASELINE_SAMPLES};
use crate::error::OtelError;
use crate::storage::OtelStorage;
use crate::types::{
    SortOrder, SpanStats, SpanStatsQuery, SpanStatsSortField, TraceQuery, SPAN_STATS_MAX_LIMIT,
};
use breakdown::{Breakdown, BreakdownBuilder};

/// Operations kept per project and hour, by total time spent.
pub const MAX_TRACKED_OPERATIONS: u64 = 200;
/// Projects rolled up per tick, the ones rolled up longest ago first.
pub const PROJECTS_PER_TICK: u64 = 50;
/// Hours rolled up per project per tick, so a long outage catches up
/// gradually instead of in one burst.
pub const HOURS_PER_PROJECT_PER_TICK: i64 = 6;
/// How far back the first rollup of a project, or one after a long outage,
/// starts.
pub const BACKFILL_HOURS: i64 = 24;
/// Rollups older than this are deleted.
pub const RETENTION_DAYS: i64 = 30;
/// Rows deleted per retention pass.
pub const PRUNE_BATCH: i64 = 5_000;
/// An hour is rolled up once it ended this long ago, so late spans land.
pub const SETTLE: Duration = Duration::minutes(5);
/// Time between ticks.
pub const TICK: std::time::Duration = std::time::Duration::from_secs(600);
/// The widest trend window, in days.
pub const MAX_TREND_DAYS: i64 = 30;
/// The baseline before a deployment, in days.
pub const BASELINE_DAYS: i64 = 7;
/// The window after a deployment that is compared, in hours.
pub const AFTER_HOURS: i64 = 24;
/// Complete hours after a deployment needed before it is judged.
pub const MIN_AFTER_HOURS: usize = 2;
/// How many band scales the p95 must move to count as a change.
pub const DEVIATIONS: f64 = 3.0;
/// The smallest relative p95 change reported, so a very flat baseline does
/// not report a 3 ms wobble as a regression.
pub const MIN_RELATIVE_CHANGE: f64 = 0.2;
/// The band scale is never taken below this share of its center.
pub const MIN_SCALE_SHARE: f64 = 0.05;
/// Changed operations returned per deployment.
pub const MAX_CHANGES_RETURNED: usize = 50;
/// Traces read for one breakdown.
pub const BREAKDOWN_TRACES: usize = 20;
/// Spans read from one of them; a larger trace is broken down on its first
/// ones.
pub const BREAKDOWN_SPANS_PER_TRACE: usize = 2_000;
/// Calls of the operation listed to pick the traces from.
pub const BREAKDOWN_CANDIDATES: u64 = 500;
/// The widest breakdown window, in days.
pub const MAX_BREAKDOWN_DAYS: i64 = 7;
/// The longest attribute key a breakdown accepts.
pub const MAX_ATTRIBUTE_KEY_CHARS: usize = 200;

/// Which calls of an operation a breakdown samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakdownSample {
    /// The most recent calls.
    Recent,
    /// The most recent calls at or over the window's p95.
    Slow,
}

impl BreakdownSample {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "recent" => Some(Self::Recent),
            "slow" => Some(Self::Slow),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Recent => "recent",
            Self::Slow => "slow",
        }
    }
}

/// A breakdown and how it was sampled.
#[derive(Debug, Clone, PartialEq)]
pub struct OperationBreakdown {
    pub sample: BreakdownSample,
    /// The p95 a `slow` sample started from.
    pub slow_threshold_ms: Option<f64>,
    pub breakdown: Breakdown,
}

/// What the rollup job last did for a project.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackingState {
    /// Hours that start before this have been rolled up.
    pub rolled_until: DateTime<Utc>,
    pub last_run_at: DateTime<Utc>,
    /// Operations kept in the last hour rolled up.
    pub tracked_operations: i32,
    /// Operations of that hour that did not fit under the cap.
    pub overflow_operations: i32,
    pub last_error: Option<String>,
}

/// One hour of one operation.
#[derive(Debug, Clone, PartialEq)]
pub struct TrendPoint {
    pub bucket_start: DateTime<Utc>,
    pub count: i64,
    pub error_count: i64,
    pub avg_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
}

/// A deployment that went live, as a marker on a trend.
#[derive(Debug, Clone, PartialEq)]
pub struct DeploymentMarker {
    pub deployment_id: i32,
    pub slug: String,
    pub environment_id: i32,
    pub live_at: DateTime<Utc>,
}

/// An operation's hourly series and the deployments inside it.
#[derive(Debug, Clone, PartialEq)]
pub struct OperationTrend {
    pub points: Vec<TrendPoint>,
    pub deployments: Vec<DeploymentMarker>,
    pub tracking: Option<TrackingState>,
}

/// How an operation's p95 after a deployment compares with its baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Regressed,
    Improved,
    Unchanged,
    /// Fewer than [`MIN_BASELINE_SAMPLES`] hours before the deployment.
    InsufficientBaseline,
    /// Fewer than [`MIN_AFTER_HOURS`] hours after it.
    InsufficientAfter,
}

impl ChangeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ChangeKind::Regressed => "regressed",
            ChangeKind::Improved => "improved",
            ChangeKind::Unchanged => "unchanged",
            ChangeKind::InsufficientBaseline => "insufficient_baseline",
            ChangeKind::InsufficientAfter => "insufficient_after",
        }
    }
}

/// The comparison of one operation.
#[derive(Debug, Clone, PartialEq)]
pub struct Comparison {
    pub kind: ChangeKind,
    /// The median hourly p95 before.
    pub baseline_p95_ms: Option<f64>,
    /// The median hourly p95 after.
    pub after_p95_ms: Option<f64>,
    /// `after / baseline - 1`; 0.4 is 40% slower.
    pub change_ratio: Option<f64>,
    pub baseline_hours: usize,
    pub after_hours: usize,
}

/// Compare the hourly p95s after a deployment with those before it.
pub fn compare(baseline: &[f64], after: &[f64]) -> Comparison {
    let mut comparison = Comparison {
        kind: ChangeKind::Unchanged,
        baseline_p95_ms: None,
        after_p95_ms: None,
        change_ratio: None,
        baseline_hours: baseline.len(),
        after_hours: after.len(),
    };
    let band = robust_band(baseline);
    let after_center = robust_band(after).map(|(center, _)| center);
    comparison.baseline_p95_ms = band.map(|(center, _)| center);
    comparison.after_p95_ms = after_center;
    if baseline.len() < MIN_BASELINE_SAMPLES {
        comparison.kind = ChangeKind::InsufficientBaseline;
        return comparison;
    }
    if after.len() < MIN_AFTER_HOURS {
        comparison.kind = ChangeKind::InsufficientAfter;
        return comparison;
    }
    let (Some((center, scale)), Some(after_p95)) = (band, after_center) else {
        comparison.kind = ChangeKind::InsufficientBaseline;
        return comparison;
    };
    if center <= 0.0 {
        comparison.kind = ChangeKind::InsufficientBaseline;
        return comparison;
    }
    let scale = scale.max(center * MIN_SCALE_SHARE);
    let ratio = after_p95 / center - 1.0;
    comparison.change_ratio = Some(ratio);
    comparison.kind = if after_p95 > center + DEVIATIONS * scale && ratio >= MIN_RELATIVE_CHANGE {
        ChangeKind::Regressed
    } else if after_p95 < center - DEVIATIONS * scale && ratio <= -MIN_RELATIVE_CHANGE {
        ChangeKind::Improved
    } else {
        ChangeKind::Unchanged
    };
    comparison
}

/// One operation compared across a deployment.
#[derive(Debug, Clone, PartialEq)]
pub struct OperationChange {
    pub service_name: String,
    pub span_name: String,
    pub comparison: Comparison,
}

/// Every tracked operation of a project compared across one deployment.
#[derive(Debug, Clone, PartialEq)]
pub struct DeploymentRegressions {
    pub deployment: DeploymentMarker,
    pub baseline_start: DateTime<Utc>,
    /// The first complete hour after the deployment went live.
    pub after_start: DateTime<Utc>,
    pub after_end: DateTime<Utc>,
    /// Regressed operations first (largest change first), then improved ones.
    pub changes: Vec<OperationChange>,
    pub regressed: usize,
    pub improved: usize,
    pub unchanged: usize,
    pub insufficient_baseline: usize,
    pub insufficient_after: usize,
    pub tracking: Option<TrackingState>,
}

/// One tick's totals.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RollupReport {
    pub projects: usize,
    pub hours: usize,
    pub rows: usize,
    pub overflow: usize,
    pub pruned: u64,
}

pub struct OperationTrendService {
    db: Arc<DatabaseConnection>,
    storage: Arc<dyn OtelStorage>,
}

fn hour_floor(at: DateTime<Utc>) -> DateTime<Utc> {
    at.duration_trunc(Duration::hours(1)).unwrap_or(at)
}

fn hour_ceil(at: DateTime<Utc>) -> DateTime<Utc> {
    let floor = hour_floor(at);
    if floor == at {
        at
    } else {
        floor + Duration::hours(1)
    }
}

impl OperationTrendService {
    pub fn new(db: Arc<DatabaseConnection>, storage: Arc<dyn OtelStorage>) -> Self {
        Self { db, storage }
    }

    /// Run forever, one tick every [`TICK`].
    pub async fn run(self: Arc<Self>) {
        loop {
            tokio::time::sleep(TICK).await;
            match self.tick(Utc::now()).await {
                Ok(report) => tracing::debug!(
                    projects = report.projects,
                    hours = report.hours,
                    rows = report.rows,
                    overflow = report.overflow,
                    pruned = report.pruned,
                    "Operation rollup finished"
                ),
                Err(error) => tracing::warn!("Operation rollup could not run: {}", error),
            }
        }
    }

    /// One tick at `now`: roll up the complete hours of the projects waiting
    /// longest, then prune old rows.
    pub async fn tick(&self, now: DateTime<Utc>) -> Result<RollupReport, OtelError> {
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT p.id, s.rolled_until FROM projects p \
                 LEFT JOIN operation_rollup_state s ON s.project_id = p.id \
                 WHERE NOT p.is_deleted \
                 ORDER BY s.last_run_at ASC NULLS FIRST, p.id LIMIT $1",
                [(PROJECTS_PER_TICK as i64).into()],
            ))
            .await?;
        let last_complete = hour_floor(now - SETTLE);
        let earliest = last_complete - Duration::hours(BACKFILL_HOURS);
        let mut report = RollupReport::default();
        for row in rows {
            let project_id: i32 = row.try_get("", "id")?;
            let rolled_until: Option<DateTime<Utc>> = row.try_get("", "rolled_until")?;
            let mut hour = rolled_until.map_or(earliest, |until| until.max(earliest));
            report.projects += 1;
            let mut hours_done = 0;
            let mut last: Option<(usize, usize)> = None;
            let mut error = None;
            while hour < last_complete && hours_done < HOURS_PER_PROJECT_PER_TICK {
                match self.roll_up_hour(project_id, hour).await {
                    Ok((tracked, overflow)) => {
                        report.hours += 1;
                        report.rows += tracked;
                        report.overflow += overflow;
                        last = Some((tracked, overflow));
                        hour += Duration::hours(1);
                        hours_done += 1;
                    }
                    Err(failure) => {
                        tracing::warn!(
                            project_id,
                            hour = %hour,
                            "Operation rollup of project failed: {}",
                            failure
                        );
                        // Skip the hour so one bad hour cannot stall the
                        // project; its trend has a gap there.
                        error = Some(format!("rolling up the hour of {hour} failed: {failure}"));
                        hour += Duration::hours(1);
                        break;
                    }
                }
            }
            self.save_state(project_id, hour, last, error).await?;
        }
        report.pruned = self.prune(now).await?;
        Ok(report)
    }

    /// Roll one hour of one project up; returns the operations kept and the
    /// ones over the cap.
    pub async fn roll_up_hour(
        &self,
        project_id: i32,
        hour: DateTime<Utc>,
    ) -> Result<(usize, usize), OtelError> {
        let base = SpanStatsQuery {
            project_ids: vec![project_id],
            start_time: hour,
            // The span-stats window is inclusive at both ends.
            end_time: hour + Duration::hours(1) - Duration::microseconds(1),
            service_name: None,
            span_name: None,
            name_pattern: None,
            kind: None,
            status: None,
            environment_id: None,
            deployment_id: None,
            attributes: None,
            min_duration_ms: None,
            min_count: 1,
            sort_by: SpanStatsSortField::TotalDurationMs,
            sort_order: SortOrder::Desc,
            limit: Some(SPAN_STATS_MAX_LIMIT),
            offset: None,
            apdex_threshold_ms: None,
        };
        let mut stats: Vec<SpanStats> = Vec::new();
        while (stats.len() as u64) < MAX_TRACKED_OPERATIONS {
            let page = self
                .storage
                .query_span_stats(SpanStatsQuery {
                    offset: Some(stats.len() as u64),
                    limit: Some(
                        SPAN_STATS_MAX_LIMIT.min(MAX_TRACKED_OPERATIONS - stats.len() as u64),
                    ),
                    ..base.clone()
                })
                .await?;
            let full = page.len() as u64 == SPAN_STATS_MAX_LIMIT;
            stats.extend(page);
            if !full {
                break;
            }
        }
        let overflow = if stats.len() as u64 >= MAX_TRACKED_OPERATIONS {
            let total = self.storage.count_span_stats(base).await?;
            total.saturating_sub(stats.len() as u64) as usize
        } else {
            0
        };
        if !stats.is_empty() {
            self.write_rows(project_id, hour, &stats).await?;
        }
        Ok((stats.len(), overflow))
    }

    async fn write_rows(
        &self,
        project_id: i32,
        hour: DateTime<Utc>,
        stats: &[SpanStats],
    ) -> Result<(), OtelError> {
        const COLUMNS: usize = 10;
        let mut placeholders = Vec::with_capacity(stats.len());
        let mut values: Vec<Value> = Vec::with_capacity(stats.len() * COLUMNS);
        for (index, row) in stats.iter().enumerate() {
            let base = index * COLUMNS;
            placeholders.push(format!(
                "(${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${})",
                base + 1,
                base + 2,
                base + 3,
                base + 4,
                base + 5,
                base + 6,
                base + 7,
                base + 8,
                base + 9,
                base + 10
            ));
            values.extend([
                project_id.into(),
                row.service_name.clone().into(),
                row.span_name.clone().into(),
                hour.into(),
                row.count.into(),
                row.error_count.into(),
                row.avg_duration_ms.into(),
                row.p50_duration_ms.into(),
                row.p95_duration_ms.into(),
                row.p99_duration_ms.into(),
            ]);
        }
        let sql = format!(
            "INSERT INTO operation_rollups (project_id, service_name, span_name, bucket_start, \
                 span_count, error_count, avg_ms, p50_ms, p95_ms, p99_ms) \
             VALUES {} \
             ON CONFLICT (project_id, service_name, span_name, bucket_start) DO UPDATE SET \
                 span_count = EXCLUDED.span_count, error_count = EXCLUDED.error_count, \
                 avg_ms = EXCLUDED.avg_ms, p50_ms = EXCLUDED.p50_ms, \
                 p95_ms = EXCLUDED.p95_ms, p99_ms = EXCLUDED.p99_ms",
            placeholders.join(", ")
        );
        let txn = self.db.begin().await?;
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            values,
        ))
        .await?;
        txn.commit().await?;
        Ok(())
    }

    async fn save_state(
        &self,
        project_id: i32,
        rolled_until: DateTime<Utc>,
        last: Option<(usize, usize)>,
        error: Option<String>,
    ) -> Result<(), OtelError> {
        // A tick that rolled nothing keeps the last hour's counts.
        let (tracked, overflow) = match last {
            Some((tracked, overflow)) => (Some(tracked as i32), Some(overflow as i32)),
            None => (None, None),
        };
        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO operation_rollup_state \
                     (project_id, rolled_until, last_run_at, tracked_operations, overflow_operations, last_error) \
                 VALUES ($1, $2, NOW(), COALESCE($3, 0), COALESCE($4, 0), $5) \
                 ON CONFLICT (project_id) DO UPDATE SET rolled_until = EXCLUDED.rolled_until, \
                     last_run_at = NOW(), \
                     tracked_operations = COALESCE($3, operation_rollup_state.tracked_operations), \
                     overflow_operations = COALESCE($4, operation_rollup_state.overflow_operations), \
                     last_error = EXCLUDED.last_error",
                [
                    project_id.into(),
                    rolled_until.into(),
                    tracked.into(),
                    overflow.into(),
                    error.into(),
                ],
            ))
            .await?;
        Ok(())
    }

    async fn prune(&self, now: DateTime<Utc>) -> Result<u64, OtelError> {
        let result = self
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "DELETE FROM operation_rollups WHERE ctid IN ( \
                     SELECT ctid FROM operation_rollups WHERE bucket_start < $1 LIMIT $2)",
                [
                    (now - Duration::days(RETENTION_DAYS)).into(),
                    PRUNE_BATCH.into(),
                ],
            ))
            .await?;
        Ok(result.rows_affected())
    }

    /// What the rollup job last did for a project; `None` before its first
    /// run.
    pub async fn tracking_state(
        &self,
        project_id: i32,
    ) -> Result<Option<TrackingState>, OtelError> {
        let row = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT rolled_until, last_run_at, tracked_operations, overflow_operations, last_error \
                 FROM operation_rollup_state WHERE project_id = $1",
                [project_id.into()],
            ))
            .await?;
        row.map(|row| {
            Ok(TrackingState {
                rolled_until: row.try_get("", "rolled_until")?,
                last_run_at: row.try_get("", "last_run_at")?,
                tracked_operations: row.try_get("", "tracked_operations")?,
                overflow_operations: row.try_get("", "overflow_operations")?,
                last_error: row.try_get("", "last_error")?,
            })
        })
        .transpose()
    }

    /// The hourly series of one operation in `[start, end)`, with the
    /// deployments that went live inside it.
    pub async fn trend(
        &self,
        project_id: i32,
        service_name: &str,
        span_name: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<OperationTrend, OtelError> {
        if service_name.trim().is_empty() || span_name.trim().is_empty() {
            return Err(OtelError::Validation {
                message: format!(
                    "An operation trend of project {project_id} needs both a service name and an operation name"
                ),
            });
        }
        if start >= end {
            return Err(OtelError::Validation {
                message: format!("The trend window must start before it ends ({start} to {end})"),
            });
        }
        if end - start > Duration::days(MAX_TREND_DAYS) {
            return Err(OtelError::Validation {
                message: format!(
                    "The trend window is {} days; at most {MAX_TREND_DAYS} are kept",
                    (end - start).num_days()
                ),
            });
        }
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT bucket_start, span_count, error_count, avg_ms, p50_ms, p95_ms, p99_ms \
                 FROM operation_rollups \
                 WHERE project_id = $1 AND service_name = $2 AND span_name = $3 \
                   AND bucket_start >= $4 AND bucket_start < $5 \
                 ORDER BY bucket_start",
                [
                    project_id.into(),
                    service_name.into(),
                    span_name.into(),
                    start.into(),
                    end.into(),
                ],
            ))
            .await?;
        let points = rows
            .iter()
            .map(|row| {
                Ok(TrendPoint {
                    bucket_start: row.try_get("", "bucket_start")?,
                    count: row.try_get("", "span_count")?,
                    error_count: row.try_get("", "error_count")?,
                    avg_ms: row.try_get("", "avg_ms")?,
                    p50_ms: row.try_get("", "p50_ms")?,
                    p95_ms: row.try_get("", "p95_ms")?,
                    p99_ms: row.try_get("", "p99_ms")?,
                })
            })
            .collect::<Result<Vec<_>, OtelError>>()?;
        Ok(OperationTrend {
            points,
            deployments: self.deployments_between(project_id, start, end).await?,
            tracking: self.tracking_state(project_id).await?,
        })
    }

    /// Deployments of a project that went live in `[start, end)`, at most 100,
    /// oldest first.
    pub async fn deployments_between(
        &self,
        project_id: i32,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<DeploymentMarker>, OtelError> {
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT id, slug, environment_id, ready_at AS live_at \
                 FROM deployments \
                 WHERE project_id = $1 AND ready_at IS NOT NULL \
                   AND ready_at >= $2 AND ready_at < $3 \
                 ORDER BY ready_at LIMIT 100",
                [project_id.into(), start.into(), end.into()],
            ))
            .await?;
        rows.iter().map(marker_from_row).collect()
    }

    /// Every tracked operation of a project compared across one deployment.
    pub async fn regressions(
        &self,
        project_id: i32,
        deployment_id: i32,
        now: DateTime<Utc>,
    ) -> Result<DeploymentRegressions, OtelError> {
        let row = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT id, slug, environment_id, ready_at AS live_at \
                 FROM deployments WHERE id = $1 AND project_id = $2",
                [deployment_id.into(), project_id.into()],
            ))
            .await?
            .ok_or(OtelError::DeploymentNotFound {
                project_id,
                deployment_id,
            })?;
        let live_at: Option<DateTime<Utc>> = row.try_get("", "live_at")?;
        let Some(live_at) = live_at else {
            return Err(OtelError::Validation {
                message: format!(
                    "Deployment {deployment_id} of project {project_id} never went live, so there is nothing after it to compare"
                ),
            });
        };
        let deployment = DeploymentMarker {
            deployment_id,
            slug: row.try_get("", "slug")?,
            environment_id: row.try_get("", "environment_id")?,
            live_at,
        };
        let baseline_start = hour_floor(live_at) - Duration::days(BASELINE_DAYS);
        // The hour the deployment went live in is mixed, so it belongs to
        // neither side.
        let baseline_end = hour_floor(live_at);
        let after_start = hour_ceil(live_at);
        let after_end = (after_start + Duration::hours(AFTER_HOURS)).min(hour_floor(now));
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT service_name, span_name, bucket_start, p95_ms FROM operation_rollups \
                 WHERE project_id = $1 AND bucket_start >= $2 AND bucket_start < $3 \
                   AND (bucket_start < $4 OR bucket_start >= $5)",
                [
                    project_id.into(),
                    baseline_start.into(),
                    after_end.max(after_start).into(),
                    baseline_end.into(),
                    after_start.into(),
                ],
            ))
            .await?;
        let mut series: BTreeMap<(String, String), (Vec<f64>, Vec<f64>)> = BTreeMap::new();
        for row in &rows {
            let service_name: String = row.try_get("", "service_name")?;
            let span_name: String = row.try_get("", "span_name")?;
            let bucket: DateTime<Utc> = row.try_get("", "bucket_start")?;
            let p95: f64 = row.try_get("", "p95_ms")?;
            let entry = series.entry((service_name, span_name)).or_default();
            if bucket < baseline_end {
                entry.0.push(p95);
            } else {
                entry.1.push(p95);
            }
        }
        let mut result = DeploymentRegressions {
            deployment,
            baseline_start,
            after_start,
            after_end: after_end.max(after_start),
            changes: Vec::new(),
            regressed: 0,
            improved: 0,
            unchanged: 0,
            insufficient_baseline: 0,
            insufficient_after: 0,
            tracking: self.tracking_state(project_id).await?,
        };
        for ((service_name, span_name), (baseline, after)) in series {
            let comparison = compare(&baseline, &after);
            match comparison.kind {
                ChangeKind::Regressed => result.regressed += 1,
                ChangeKind::Improved => result.improved += 1,
                ChangeKind::Unchanged => result.unchanged += 1,
                ChangeKind::InsufficientBaseline => result.insufficient_baseline += 1,
                ChangeKind::InsufficientAfter => result.insufficient_after += 1,
            }
            if matches!(
                comparison.kind,
                ChangeKind::Regressed | ChangeKind::Improved
            ) {
                result.changes.push(OperationChange {
                    service_name,
                    span_name,
                    comparison,
                });
            }
        }
        result.changes.sort_by(|a, b| {
            let rank = |c: &OperationChange| (c.comparison.kind != ChangeKind::Regressed) as u8;
            rank(a).cmp(&rank(b)).then_with(|| {
                let magnitude =
                    |c: &OperationChange| c.comparison.change_ratio.unwrap_or(0.0).abs();
                magnitude(b)
                    .partial_cmp(&magnitude(a))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        });
        result.changes.truncate(MAX_CHANGES_RETURNED);
        Ok(result)
    }
}

impl OperationTrendService {
    /// Where the time of one operation goes, from at most
    /// [`BREAKDOWN_TRACES`] traces in `[start, end]`.
    #[allow(
        clippy::too_many_arguments,
        reason = "each argument is one filter of the request"
    )]
    pub async fn breakdown(
        &self,
        project_id: i32,
        service_name: &str,
        span_name: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        sample: BreakdownSample,
        attribute: Option<&str>,
    ) -> Result<OperationBreakdown, OtelError> {
        if service_name.trim().is_empty() || span_name.trim().is_empty() {
            return Err(OtelError::Validation {
                message: format!(
                    "A breakdown of project {project_id} needs both a service name and an operation name"
                ),
            });
        }
        if start >= end {
            return Err(OtelError::Validation {
                message: format!(
                    "The breakdown window must start before it ends ({start} to {end})"
                ),
            });
        }
        if end - start > Duration::days(MAX_BREAKDOWN_DAYS) {
            return Err(OtelError::Validation {
                message: format!(
                    "The breakdown window is {} days; at most {MAX_BREAKDOWN_DAYS} are read",
                    (end - start).num_days()
                ),
            });
        }
        if let Some(key) = attribute {
            if key.trim().is_empty() || key.chars().count() > MAX_ATTRIBUTE_KEY_CHARS {
                return Err(OtelError::Validation {
                    message: format!(
                        "The attribute to split by must be 1 to {MAX_ATTRIBUTE_KEY_CHARS} characters"
                    ),
                });
            }
        }
        let mut result = OperationBreakdown {
            sample,
            slow_threshold_ms: None,
            breakdown: Breakdown::default(),
        };
        if sample == BreakdownSample::Slow {
            let stats = self
                .storage
                .query_span_stats(SpanStatsQuery {
                    project_ids: vec![project_id],
                    start_time: start,
                    end_time: end,
                    service_name: Some(service_name.to_string()),
                    span_name: Some(span_name.to_string()),
                    name_pattern: None,
                    kind: None,
                    status: None,
                    environment_id: None,
                    deployment_id: None,
                    attributes: None,
                    min_duration_ms: None,
                    min_count: 1,
                    sort_by: SpanStatsSortField::TotalDurationMs,
                    sort_order: SortOrder::Desc,
                    limit: Some(1),
                    offset: None,
                    apdex_threshold_ms: None,
                })
                .await?;
            let Some(row) = stats.into_iter().next() else {
                result.breakdown =
                    BreakdownBuilder::new(service_name, span_name, attribute).finish();
                return Ok(result);
            };
            result.slow_threshold_ms = Some(row.p95_duration_ms);
        }
        let candidates = self
            .storage
            .query_spans(TraceQuery {
                project_id,
                service_name: Some(service_name.to_string()),
                name_pattern: Some(span_name.to_string()),
                min_duration_ms: result.slow_threshold_ms,
                start_time: Some(start),
                end_time: Some(end),
                limit: Some(BREAKDOWN_CANDIDATES),
                ..Default::default()
            })
            .await?;
        // The name filter is a substring match; keep exact calls only.
        let mut trace_ids: Vec<String> = Vec::new();
        for span in candidates
            .iter()
            .filter(|span| span.name == span_name && span.resource.service_name == service_name)
        {
            if !trace_ids.contains(&span.trace_id) {
                trace_ids.push(span.trace_id.clone());
                if trace_ids.len() == BREAKDOWN_TRACES {
                    break;
                }
            }
        }
        let mut builder = BreakdownBuilder::new(service_name, span_name, attribute);
        for trace_id in &trace_ids {
            let spans = self
                .storage
                .get_trace_spans(project_id, trace_id, BREAKDOWN_SPANS_PER_TRACE)
                .await?;
            builder.add_trace(&spans);
        }
        result.breakdown = builder.finish();
        Ok(result)
    }
}

fn marker_from_row(row: &sea_orm::QueryResult) -> Result<DeploymentMarker, OtelError> {
    Ok(DeploymentMarker {
        deployment_id: row.try_get("", "id")?,
        slug: row.try_get("", "slug")?,
        environment_id: row.try_get("", "environment_id")?,
        live_at: row.try_get("", "live_at")?,
    })
}

#[cfg(test)]
mod tests;
