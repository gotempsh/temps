// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Release health (ADR 111, D3): crash-free sessions and users, and the
//! adoption of each release, from the sessions Sentry SDKs send.
//!
//! Only hourly counters are stored ([`recorder`]); distinct users are an
//! estimate ([`hll`]). The console and CLI read [`ReleaseHealthService::list`]
//! and [`ReleaseHealthService::series`]. A publisher writes the crash-free
//! rates of active releases as project metrics every
//! [`PUBLISH_INTERVAL`], so the existing metric alert rules can alert on a
//! crash-free drop, per release, without a second alerting system.

pub mod hll;
pub mod recorder;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, DurationRound, TimeDelta, Utc};
use futures::TryStreamExt;
use relay_event_schema::protocol::{SessionAggregates, SessionUpdate};
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement, StreamTrait, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use temps_core::{ProjectMetricKind, ProjectMetricSample, ProjectMetricsSink};
use tokio::sync::mpsc;
use tracing::{debug, error, warn};
use utoipa::ToSchema;

use self::hll::{merge_bytes, Hll};
use self::recorder::{
    delta_from_update, deltas_from_aggregates, RecordOutcome, SessionDelta, SessionFlusher,
    SessionRecorder, QUEUE_CAPACITY,
};
use super::types::ErrorTrackingError;
use crate::services::group_workflow::next_release::MAX_RELEASE_CHARS;

/// Longest window the release list and series read.
pub const MAX_HEALTH_DAYS: u32 = 14;
/// Default window of the release list and series.
pub const DEFAULT_HEALTH_DAYS: u32 = 7;
/// Adoption is the share of the sessions started in this trailing window.
pub const ADOPTION_HOURS: i64 = 24;
/// Rows older than this are deleted.
pub const RETENTION_DAYS: i64 = 90;
/// Rows deleted per statement by the retention pass.
const PRUNE_BATCH: i64 = 5_000;
/// How often crash-free rates are published as metrics.
pub const PUBLISH_INTERVAL: Duration = Duration::from_secs(300);
/// Window of the published crash-free rates.
pub const PUBLISH_WINDOW_HOURS: i64 = 24;
/// A release must have had a session in this window to be published, so a
/// release nobody runs any more stops reporting instead of repeating a
/// stale rate.
pub const PUBLISH_ACTIVE_HOURS: i64 = 1;
/// Releases (per environment) published per tick, the busiest first.
pub const MAX_PUBLISHED_RELEASES: i64 = 1_000;
/// Sessions (or users) a window needs before a crash-free rate is
/// published: one crash in three sessions is not a 67% crash-free release.
pub const MIN_SESSIONS_FOR_RATE: i64 = 20;
/// Distinct SDK environments listed for the filter.
const MAX_ENVIRONMENTS_LISTED: i64 = 50;

/// Metric names the publisher writes; metric alert rules select them.
pub const CRASH_FREE_SESSIONS_METRIC: &str = "release.crash_free_sessions";
pub const CRASH_FREE_USERS_METRIC: &str = "release.crash_free_users";
pub const SESSIONS_METRIC: &str = "release.sessions";

/// Whether the project has sent any session, and what to do if not.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct ReleaseHealthTracking {
    pub configured: bool,
    pub reason: Option<String>,
    /// Project-relative console path that shows how to set it up.
    pub setup_path: Option<String>,
}

/// The deployment a release was first shipped by, when Temps deployed it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct ReleaseDeployment {
    pub deployment_id: i32,
    pub environment_id: i32,
    pub environment_name: Option<String>,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub ready_at: Option<DateTime<Utc>>,
    pub commit_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct ReleaseHealth {
    pub release: String,
    pub sessions: i64,
    /// Sessions with at least one error, crashes included (an estimate for
    /// single sessions, exact for aggregates).
    pub errored_sessions: i64,
    pub crashed_sessions: i64,
    pub abnormal_sessions: i64,
    /// Distinct users, estimated (about 6.5% standard error).
    pub users: i64,
    pub crashed_users: i64,
    /// `1 - crashed / sessions`; absent without sessions.
    pub crash_free_sessions: Option<f64>,
    /// `1 - crashed users / users`; absent when no session named a user.
    pub crash_free_users: Option<f64>,
    /// This release's share of the sessions started in the last 24 hours,
    /// in the same environment filter.
    pub adoption: Option<f64>,
    pub sessions_last_24h: i64,
    #[schema(value_type = String, format = DateTime)]
    pub first_session_hour: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub last_session_hour: DateTime<Utc>,
    /// When Temps first saw or deployed this release (the order "resolve in
    /// the next release" uses).
    #[schema(value_type = Option<String>, format = DateTime)]
    pub released_at: Option<DateTime<Utc>>,
    pub deployment: Option<ReleaseDeployment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct ReleaseHealthPage {
    pub items: Vec<ReleaseHealth>,
    pub total: i64,
    pub page: u64,
    pub per_page: u64,
    /// SDK environments with sessions in the window, for the filter.
    pub environments: Vec<String>,
    #[schema(value_type = String, format = DateTime)]
    pub start_time: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub end_time: DateTime<Utc>,
    pub tracking: ReleaseHealthTracking,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct ReleaseHealthPoint {
    #[schema(value_type = String, format = DateTime)]
    pub bucket_start: DateTime<Utc>,
    pub sessions: i64,
    pub errored_sessions: i64,
    pub crashed_sessions: i64,
    pub abnormal_sessions: i64,
    pub crash_free_sessions: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
pub struct ReleaseHealthSeries {
    pub release: String,
    pub environment: Option<String>,
    /// One point per hour with sessions, oldest first.
    pub points: Vec<ReleaseHealthPoint>,
    #[schema(value_type = String, format = DateTime)]
    pub start_time: DateTime<Utc>,
    #[schema(value_type = String, format = DateTime)]
    pub end_time: DateTime<Utc>,
}

/// What recording one envelope's sessions did, and the releases it named
/// (to register them once, like the releases of events).
#[derive(Debug, Default)]
pub struct EnvelopeSessions {
    pub outcome: RecordOutcome,
    pub releases: Vec<String>,
}

pub(crate) fn crash_free(total: i64, crashed: i64) -> Option<f64> {
    if total <= 0 {
        return None;
    }
    Some((1.0 - crashed.clamp(0, total) as f64 / total as f64).clamp(0.0, 1.0))
}

fn share(part: i64, whole: i64) -> Option<f64> {
    if whole <= 0 {
        return None;
    }
    Some((part as f64 / whole as f64).clamp(0.0, 1.0))
}

fn validate_window(days: u32) -> Result<(), ErrorTrackingError> {
    if !(1..=MAX_HEALTH_DAYS).contains(&days) {
        return Err(ErrorTrackingError::Validation(format!(
            "Release health reads 1 to {MAX_HEALTH_DAYS} days, got {days}"
        )));
    }
    Ok(())
}

fn validate_environment(environment: &Option<String>) -> Result<(), ErrorTrackingError> {
    if let Some(environment) = environment {
        if environment.chars().count() > recorder::MAX_ENVIRONMENT_CHARS
            || environment.contains('\0')
        {
            return Err(ErrorTrackingError::Validation(format!(
                "Release health environment must be at most {} characters without NUL",
                recorder::MAX_ENVIRONMENT_CHARS
            )));
        }
    }
    Ok(())
}

/// The start of the window: whole hours, so a bucket is in or out.
fn window_start(now: DateTime<Utc>, days: u32) -> DateTime<Utc> {
    let start = now - TimeDelta::days(i64::from(days));
    start.duration_trunc(TimeDelta::hours(1)).unwrap_or(start)
}

pub struct ReleaseHealthService {
    db: Arc<DatabaseConnection>,
    recorder: SessionRecorder,
    /// Taken once by [`ReleaseHealthService::start`]; startup only, never
    /// touched on the request path.
    receiver: std::sync::Mutex<Option<mpsc::Receiver<SessionDelta>>>,
}

impl ReleaseHealthService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        let (recorder, receiver) = SessionRecorder::new(QUEUE_CAPACITY);
        Self {
            db,
            recorder,
            receiver: std::sync::Mutex::new(Some(receiver)),
        }
    }

    /// Start the flusher, and the publisher when a metrics sink exists.
    /// Calling it again does nothing.
    pub fn start(self: &Arc<Self>, sink: Option<Arc<dyn ProjectMetricsSink>>) {
        let receiver = match self.receiver.lock() {
            Ok(mut slot) => slot.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        let Some(receiver) = receiver else {
            return;
        };
        let flusher = SessionFlusher::new(self.db.clone(), receiver, self.recorder.stats_arc());
        tokio::spawn(flusher.run());
        let service = self.clone();
        tokio::spawn(async move { service.run_publisher(sink).await });
    }

    /// Queue the sessions of one envelope. Never waits; see [`recorder`].
    pub fn record_envelope(
        &self,
        project_id: i32,
        updates: &[SessionUpdate],
        aggregates: &[SessionAggregates],
    ) -> EnvelopeSessions {
        let now = Utc::now();
        let mut releases = HashSet::new();
        let deltas: Vec<_> = updates
            .iter()
            .map(|update| delta_from_update(project_id, update, now))
            .chain(
                aggregates
                    .iter()
                    .flat_map(|item| deltas_from_aggregates(project_id, item, now)),
            )
            .inspect(|delta| {
                if let Ok(delta) = delta {
                    releases.insert(delta.key.release.clone());
                }
            })
            .collect();
        EnvelopeSessions {
            outcome: self.recorder.record(deltas),
            releases: releases.into_iter().collect(),
        }
    }

    /// Counters of the session queue and flusher (drops included).
    pub fn recorder_stats(&self) -> &recorder::RecorderStats {
        self.recorder.stats()
    }

    async fn tracking<C: ConnectionTrait>(
        &self,
        db: &C,
        project_id: i32,
    ) -> Result<ReleaseHealthTracking, ErrorTrackingError> {
        let row = db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT EXISTS (SELECT 1 FROM release_health_hourly WHERE project_id = $1) AS any",
                [project_id.into()],
            ))
            .await?;
        let configured = match row {
            Some(row) => row.try_get::<bool>("", "any")?,
            None => false,
        };
        Ok(if configured {
            ReleaseHealthTracking {
                configured: true,
                reason: None,
                setup_path: None,
            }
        } else {
            ReleaseHealthTracking {
                configured: false,
                reason: Some(
                    "No sessions received yet. Release health counts the sessions a Sentry \
                     SDK sends when session tracking is on (the default in the browser and \
                     mobile SDKs; server SDKs need it enabled), with a release set."
                        .into(),
                ),
                setup_path: Some("errors/setup".into()),
            }
        })
    }

    /// Releases with sessions in the last `days` days, the most recently
    /// active first, with their crash-free rates, adoption and deployment.
    pub async fn list(
        &self,
        project_id: i32,
        environment: Option<String>,
        days: u32,
        page: u64,
        per_page: u64,
    ) -> Result<ReleaseHealthPage, ErrorTrackingError> {
        validate_window(days)?;
        validate_environment(&environment)?;
        if page == 0 || !(1..=100).contains(&per_page) || page > 10_000 {
            return Err(ErrorTrackingError::Validation(format!(
                "Release health pages start at 1 (at most 10000) with 1 to 100 per page, \
                 got page {page} with {per_page} per page"
            )));
        }
        let end_time = Utc::now();
        let start_time = window_start(end_time, days);
        let adoption_start = window_start(end_time, 1).max(start_time);
        let offset = (page - 1) * per_page;
        let tx = self.db.begin().await?;
        tx.execute_unprepared("SET LOCAL statement_timeout='5s'; SET LOCAL work_mem='4MB'")
            .await?;
        let tracking = self.tracking(&tx, project_id).await?;
        let rows = tx
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "WITH r AS ( \
                     SELECT release, SUM(started)::bigint AS started, \
                            SUM(crashed)::bigint AS crashed, SUM(abnormal)::bigint AS abnormal, \
                            SUM(errored_exact)::bigint AS errored_exact, \
                            COALESCE(SUM(started) FILTER (WHERE bucket_start >= $4), 0)::bigint \
                                AS started_recent, \
                            MIN(bucket_start) AS first_bucket, MAX(bucket_start) AS last_bucket \
                     FROM release_health_hourly \
                     WHERE project_id = $1 AND bucket_start >= $2 \
                       AND ($3::text IS NULL OR environment = $3) \
                     GROUP BY release) \
                 SELECT r.*, COUNT(*) OVER () AS total, \
                        SUM(r.started_recent) OVER ()::bigint AS all_started_recent, \
                        er.released_at, d.id AS deployment_id, \
                        d.environment_id AS deployment_environment_id, \
                        e.name AS deployment_environment, d.ready_at, d.commit_message \
                 FROM r \
                 LEFT JOIN error_releases er ON er.project_id = $1 AND er.version = r.release \
                 LEFT JOIN deployments d ON d.id = er.deployment_id AND d.project_id = $1 \
                 LEFT JOIN environments e ON e.id = d.environment_id \
                 ORDER BY r.last_bucket DESC, r.release \
                 LIMIT $5 OFFSET $6",
                [
                    project_id.into(),
                    start_time.into(),
                    environment.clone().into(),
                    adoption_start.into(),
                    (per_page as i64).into(),
                    (offset as i64).into(),
                ],
            ))
            .await?;

        struct Counters {
            release: String,
            started: i64,
            crashed: i64,
            abnormal: i64,
            errored_exact: i64,
            started_recent: i64,
            first_bucket: DateTime<Utc>,
            last_bucket: DateTime<Utc>,
            released_at: Option<DateTime<Utc>>,
            deployment: Option<ReleaseDeployment>,
        }
        let mut total = 0i64;
        let mut all_recent = 0i64;
        let mut counters = Vec::with_capacity(rows.len());
        for row in &rows {
            total = row.try_get("", "total")?;
            all_recent = row
                .try_get::<Option<i64>>("", "all_started_recent")?
                .unwrap_or(0);
            let deployment = match row.try_get::<Option<i32>>("", "deployment_id")? {
                Some(deployment_id) => Some(ReleaseDeployment {
                    deployment_id,
                    environment_id: row.try_get("", "deployment_environment_id")?,
                    environment_name: row.try_get("", "deployment_environment")?,
                    ready_at: row.try_get("", "ready_at")?,
                    commit_message: row.try_get("", "commit_message")?,
                }),
                None => None,
            };
            counters.push(Counters {
                release: row.try_get("", "release")?,
                started: row.try_get("", "started")?,
                crashed: row.try_get("", "crashed")?,
                abnormal: row.try_get("", "abnormal")?,
                errored_exact: row.try_get("", "errored_exact")?,
                started_recent: row.try_get("", "started_recent")?,
                first_bucket: row.try_get("", "first_bucket")?,
                last_bucket: row.try_get("", "last_bucket")?,
                released_at: row.try_get("", "released_at")?,
                deployment,
            });
        }
        if counters.is_empty() && page > 1 {
            let row = tx
                .query_one(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "SELECT COUNT(DISTINCT release)::bigint AS total FROM release_health_hourly \
                     WHERE project_id = $1 AND bucket_start >= $2 \
                       AND ($3::text IS NULL OR environment = $3)",
                    [
                        project_id.into(),
                        start_time.into(),
                        environment.clone().into(),
                    ],
                ))
                .await?;
            if let Some(row) = row {
                total = row.try_get("", "total")?;
            }
        }

        // Sketches of the page's releases, merged as they stream so memory
        // stays at three sketches per release whatever the window.
        let mut sketches: HashMap<String, [Option<Hll>; 3]> = HashMap::new();
        if !counters.is_empty() {
            let names = serde_json::to_string(
                &counters
                    .iter()
                    .map(|c| c.release.as_str())
                    .collect::<Vec<_>>(),
            )
            .map_err(|e| {
                ErrorTrackingError::Validation(format!(
                    "Could not encode the releases of project {project_id}: {e}"
                ))
            })?;
            let mut stream = tx
                .stream(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "SELECT release, errored_sessions, users, crashed_users \
                     FROM release_health_hourly \
                     WHERE project_id = $1 AND bucket_start >= $2 \
                       AND ($3::text IS NULL OR environment = $3) \
                       AND release IN (SELECT jsonb_array_elements_text($4::jsonb))",
                    [
                        project_id.into(),
                        start_time.into(),
                        environment.clone().into(),
                        names.into(),
                    ],
                ))
                .await?;
            while let Some(row) = stream.try_next().await? {
                let release: String = row.try_get("", "release")?;
                let entry = sketches.entry(release).or_default();
                for (slot, column) in
                    entry
                        .iter_mut()
                        .zip(["errored_sessions", "users", "crashed_users"])
                {
                    let bytes: Option<Vec<u8>> = row.try_get("", column)?;
                    merge_bytes(slot, bytes.as_deref());
                }
            }
        }

        let environments = tx
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT DISTINCT environment FROM release_health_hourly \
                 WHERE project_id = $1 AND bucket_start >= $2 \
                 ORDER BY environment LIMIT $3",
                [
                    project_id.into(),
                    start_time.into(),
                    MAX_ENVIRONMENTS_LISTED.into(),
                ],
            ))
            .await?
            .iter()
            .map(|row| row.try_get::<String>("", "environment"))
            .collect::<Result<Vec<_>, _>>()?;
        tx.commit().await?;

        let items = counters
            .into_iter()
            .map(|c| {
                let [errored, users, crashed_users] =
                    sketches.remove(&c.release).unwrap_or_default();
                let estimate = |s: &Option<Hll>, ceiling: i64| {
                    s.as_ref().map_or(0, |s| s.count(Some(ceiling)))
                };
                let users_count = users.as_ref().map_or(0, |s| s.count(None));
                let crashed_users_count = estimate(&crashed_users, users_count);
                let errored_sessions =
                    (estimate(&errored, c.started) + c.errored_exact).min(c.started.max(0));
                ReleaseHealth {
                    crash_free_sessions: crash_free(c.started, c.crashed),
                    crash_free_users: crash_free(users_count, crashed_users_count),
                    adoption: share(c.started_recent, all_recent),
                    release: c.release,
                    sessions: c.started,
                    errored_sessions,
                    crashed_sessions: c.crashed,
                    abnormal_sessions: c.abnormal,
                    users: users_count,
                    crashed_users: crashed_users_count,
                    sessions_last_24h: c.started_recent,
                    first_session_hour: c.first_bucket,
                    last_session_hour: c.last_bucket,
                    released_at: c.released_at,
                    deployment: c.deployment,
                }
            })
            .collect();
        Ok(ReleaseHealthPage {
            items,
            total,
            page,
            per_page,
            environments,
            start_time,
            end_time,
            tracking,
        })
    }

    /// The hourly sessions and crash-free rate of one release.
    pub async fn series(
        &self,
        project_id: i32,
        release: String,
        environment: Option<String>,
        days: u32,
    ) -> Result<ReleaseHealthSeries, ErrorTrackingError> {
        validate_window(days)?;
        validate_environment(&environment)?;
        let release = release.trim().to_owned();
        if release.is_empty() || release.chars().count() > MAX_RELEASE_CHARS {
            return Err(ErrorTrackingError::Validation(format!(
                "A release health series needs a release of 1 to {MAX_RELEASE_CHARS} characters"
            )));
        }
        let end_time = Utc::now();
        let start_time = window_start(end_time, days);
        let tx = self.db.begin().await?;
        tx.execute_unprepared("SET LOCAL statement_timeout='5s'")
            .await?;
        let mut points: Vec<ReleaseHealthPoint> = Vec::new();
        let mut errored: Option<Hll> = None;
        {
            // Rows of one hour arrive together (one per environment); the
            // errored sessions of the hour are their merged sketch.
            let mut stream = tx
                .stream(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "SELECT bucket_start, started, crashed, abnormal, errored_exact, \
                            errored_sessions \
                     FROM release_health_hourly \
                     WHERE project_id = $1 AND release = $2 AND bucket_start >= $3 \
                       AND ($4::text IS NULL OR environment = $4) \
                     ORDER BY bucket_start",
                    [
                        project_id.into(),
                        release.clone().into(),
                        start_time.into(),
                        environment.clone().into(),
                    ],
                ))
                .await?;
            let finish = |point: &mut ReleaseHealthPoint, errored: &mut Option<Hll>| {
                let estimate = errored.take().map_or(0, |s| s.count(Some(point.sessions)));
                point.errored_sessions = (point.errored_sessions + estimate).min(point.sessions);
                point.crash_free_sessions = crash_free(point.sessions, point.crashed_sessions);
            };
            while let Some(row) = stream.try_next().await? {
                let bucket_start: DateTime<Utc> = row.try_get("", "bucket_start")?;
                if points.last().is_none_or(|p| p.bucket_start != bucket_start) {
                    if let Some(last) = points.last_mut() {
                        finish(last, &mut errored);
                    }
                    points.push(ReleaseHealthPoint {
                        bucket_start,
                        sessions: 0,
                        errored_sessions: 0,
                        crashed_sessions: 0,
                        abnormal_sessions: 0,
                        crash_free_sessions: None,
                    });
                }
                if let Some(point) = points.last_mut() {
                    point.sessions += row.try_get::<i64>("", "started")?;
                    point.crashed_sessions += row.try_get::<i64>("", "crashed")?;
                    point.abnormal_sessions += row.try_get::<i64>("", "abnormal")?;
                    point.errored_sessions += row.try_get::<i64>("", "errored_exact")?;
                }
                let bytes: Option<Vec<u8>> = row.try_get("", "errored_sessions")?;
                merge_bytes(&mut errored, bytes.as_deref());
            }
            if let Some(last) = points.last_mut() {
                finish(last, &mut errored);
            }
        }
        tx.commit().await?;
        Ok(ReleaseHealthSeries {
            release,
            environment,
            points,
            start_time,
            end_time,
        })
    }

    async fn run_publisher(self: Arc<Self>, sink: Option<Arc<dyn ProjectMetricsSink>>) {
        let mut tick = tokio::time::interval(PUBLISH_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if let Some(sink) = sink.as_ref() {
                match self.publish(Utc::now()).await {
                    Ok(samples) if samples.is_empty() => {}
                    Ok(samples) => {
                        let count = samples.len();
                        if let Err(err) = sink.record(samples).await {
                            warn!(error = %err, "Could not store release health metrics");
                        } else {
                            debug!(samples = count, "Release health metrics published");
                        }
                    }
                    Err(err) => error!(error = %err, "Could not compute release health metrics"),
                }
            }
            if let Err(err) = self.prune(Utc::now()).await {
                error!(error = %err, "Could not delete expired release health counters");
            }
        }
    }

    /// The crash-free rates of releases with a session in the last hour,
    /// over the trailing day, as project metric samples labelled by
    /// `release` and (when the SDK sent one) `environment`.
    pub async fn publish(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<ProjectMetricSample>, ErrorTrackingError> {
        let window = now - TimeDelta::hours(PUBLISH_WINDOW_HOURS);
        let active = now - TimeDelta::hours(PUBLISH_ACTIVE_HOURS);
        let tx = self.db.begin().await?;
        tx.execute_unprepared("SET LOCAL statement_timeout='30s'")
            .await?;
        type Key = (i32, String, String);
        let mut totals: HashMap<Key, (i64, i64, Option<Hll>, Option<Hll>)> = HashMap::new();
        {
            let mut stream = tx
                .stream(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "WITH top AS ( \
                         SELECT project_id, environment, release FROM release_health_hourly \
                         WHERE bucket_start >= $1 \
                         GROUP BY project_id, environment, release \
                         HAVING MAX(updated_at) >= $2 \
                         ORDER BY SUM(started) DESC LIMIT $3) \
                     SELECT h.project_id, h.environment, h.release, h.started, h.crashed, \
                            h.users, h.crashed_users \
                     FROM release_health_hourly h \
                     JOIN top USING (project_id, environment, release) \
                     WHERE h.bucket_start >= $1",
                    [window.into(), active.into(), MAX_PUBLISHED_RELEASES.into()],
                ))
                .await?;
            while let Some(row) = stream.try_next().await? {
                let key: Key = (
                    row.try_get("", "project_id")?,
                    row.try_get("", "environment")?,
                    row.try_get("", "release")?,
                );
                let entry = totals.entry(key).or_default();
                entry.0 += row.try_get::<i64>("", "started")?;
                entry.1 += row.try_get::<i64>("", "crashed")?;
                let users: Option<Vec<u8>> = row.try_get("", "users")?;
                merge_bytes(&mut entry.2, users.as_deref());
                let crashed_users: Option<Vec<u8>> = row.try_get("", "crashed_users")?;
                merge_bytes(&mut entry.3, crashed_users.as_deref());
            }
        }
        tx.commit().await?;
        let mut samples = Vec::new();
        for ((project_id, environment, release), (started, crashed, users, crashed_users)) in totals
        {
            let mut labels = vec![("release", release)];
            if !environment.is_empty() {
                labels.push(("environment", environment));
            }
            let sample = |name: &'static str, value: f64| ProjectMetricSample {
                project_id,
                name,
                kind: ProjectMetricKind::Gauge,
                unit: "1",
                value,
                labels: labels.clone(),
                timestamp: now,
            };
            samples.push(sample(SESSIONS_METRIC, started as f64));
            if started >= MIN_SESSIONS_FOR_RATE {
                if let Some(rate) = crash_free(started, crashed) {
                    samples.push(sample(CRASH_FREE_SESSIONS_METRIC, rate));
                }
            }
            let users = users.as_ref().map_or(0, |s| s.count(None));
            if users >= MIN_SESSIONS_FOR_RATE {
                let crashed_users = crashed_users.as_ref().map_or(0, |s| s.count(Some(users)));
                if let Some(rate) = crash_free(users, crashed_users) {
                    samples.push(sample(CRASH_FREE_USERS_METRIC, rate));
                }
            }
        }
        Ok(samples)
    }

    /// Delete counters older than [`RETENTION_DAYS`], a batch at a time.
    pub async fn prune(&self, now: DateTime<Utc>) -> Result<u64, ErrorTrackingError> {
        let cutoff = now - TimeDelta::days(RETENTION_DAYS);
        let mut deleted = 0;
        loop {
            let result = self
                .db
                .execute(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "DELETE FROM release_health_hourly WHERE ctid IN ( \
                         SELECT ctid FROM release_health_hourly WHERE bucket_start < $1 LIMIT $2)",
                    [cutoff.into(), PRUNE_BATCH.into()],
                ))
                .await?;
            deleted += result.rows_affected();
            if result.rows_affected() < PRUNE_BATCH as u64 {
                return Ok(deleted);
            }
        }
    }
}
