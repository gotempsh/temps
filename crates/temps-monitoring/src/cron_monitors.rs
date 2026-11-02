// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Native external cron monitoring. Wire adapters submit check-ins; this service
//! owns schedules, run state and deadlines independently of any SDK protocol.
pub mod handlers;
mod management;
pub use management::*;

use chrono::{DateTime, Datelike, Duration, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, DatabaseTransaction, DbErr,
    FromQueryResult, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use std::{str::FromStr, sync::Arc};

#[derive(Debug, thiserror::Error)]
pub enum CronMonitorError {
    #[error("Invalid cron monitor: {0}")]
    Invalid(String),
    #[error("Cron monitor limit reached")]
    Limit,
    #[error("Cron monitor {monitor_id} was not found in project {project_id}")]
    NotFound { project_id: i32, monitor_id: i32 },
    #[error("Cron monitor already exists in this project and environment")]
    Conflict,
    #[error(
        "Cron monitoring scope was not found: project {project_id}, environment {environment_id:?}"
    )]
    ScopeNotFound {
        project_id: i32,
        environment_id: Option<i32>,
    },
    #[error("Stored cron monitor {monitor_id} has invalid configuration")]
    StoredConfiguration { monitor_id: i32 },
    #[error(transparent)]
    Alarm(CronOperationError<crate::AlarmError>),
    #[error(transparent)]
    Database(CronOperationError<DbErr>),
}
#[derive(Debug, thiserror::Error)]
#[error("Cron monitoring operation '{operation}' failed for project {project_id:?}, monitor {monitor_id:?}: {source}")]
pub struct CronOperationError<E: std::error::Error + 'static> {
    pub operation: &'static str,
    pub project_id: Option<i32>,
    pub monitor_id: Option<i32>,
    #[source]
    pub source: E,
}
impl From<DbErr> for CronMonitorError {
    fn from(source: DbErr) -> Self {
        Self::Database(CronOperationError {
            operation: "execute storage operation",
            project_id: None,
            monitor_id: None,
            source,
        })
    }
}
impl From<crate::AlarmError> for CronMonitorError {
    fn from(source: crate::AlarmError) -> Self {
        Self::Alarm(CronOperationError {
            operation: "synchronize alarm",
            project_id: None,
            monitor_id: None,
            source,
        })
    }
}
impl CronMonitorError {
    fn with_context(
        mut self,
        operation: &'static str,
        project: Option<i32>,
        monitor: Option<i32>,
    ) -> Self {
        match &mut self {
            Self::Database(error) => {
                error.operation = operation;
                error.project_id = error.project_id.or(project);
                error.monitor_id = error.monitor_id.or(monitor);
            }
            Self::Alarm(error) => {
                error.operation = operation;
                error.project_id = error.project_id.or(project);
                error.monitor_id = error.monitor_id.or(monitor);
            }
            _ => {}
        }
        self
    }

    /// Check-in IDs make uncertain commits safe to retry. Deterministic schema,
    /// constraint and conversion failures remain server errors without retry hints.
    pub fn is_retryable(&self) -> bool {
        use sea_orm::{sqlx::Error as SqlxError, RuntimeErr};
        let Self::Database(error) = self else {
            return false;
        };
        match &error.source {
            DbErr::ConnectionAcquire(_) | DbErr::Conn(_) => true,
            DbErr::Query(RuntimeErr::SqlxError(error))
            | DbErr::Exec(RuntimeErr::SqlxError(error)) => match error {
                SqlxError::Database(error) => error.code().is_some_and(|code| {
                    code.starts_with("08")
                        || [
                            "40001", "40P01", "55P03", "57014", "53300", "53400", "57P01", "57P02",
                            "57P03", "42P01",
                        ]
                        .contains(&code.as_ref())
                }),
                SqlxError::Io(_)
                | SqlxError::Tls(_)
                | SqlxError::PoolTimedOut
                | SqlxError::PoolClosed => true,
                _ => false,
            },
            _ => false,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum MonitorSchedule {
    Crontab { value: String },
    Interval { value: u32, unit: String },
}
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MonitorConfig {
    pub schedule: MonitorSchedule,
    #[serde(default = "utc")]
    pub timezone: String,
    #[serde(default = "margin")]
    pub checkin_margin: u32,
    #[serde(default = "runtime")]
    pub max_runtime: u32,
    #[serde(default = "one")]
    pub failure_issue_threshold: u32,
    #[serde(default = "one")]
    pub recovery_threshold: u32,
}
fn utc() -> String {
    "UTC".into()
}
fn margin() -> u32 {
    1
}
fn runtime() -> u32 {
    30
}
fn one() -> u32 {
    1
}
impl MonitorConfig {
    pub fn validate(&self, now: DateTime<Utc>) -> Result<(), CronMonitorError> {
        if !(1..=40320).contains(&self.checkin_margin)
            || !(1..=40320).contains(&self.max_runtime)
            || !(1..=100).contains(&self.failure_issue_threshold)
            || !(1..=100).contains(&self.recovery_threshold)
        {
            return Err(CronMonitorError::Invalid(
                "minute limits must be 1–40320 and thresholds 1–100".into(),
            ));
        }
        self.next_after(now)?;
        Ok(())
    }
    fn next_after(&self, now: DateTime<Utc>) -> Result<DateTime<Utc>, CronMonitorError> {
        let invalid = || CronMonitorError::Invalid("schedule or timezone is unsupported".into());
        let tz = chrono_tz::Tz::from_str(&self.timezone).map_err(|_| invalid())?;
        match &self.schedule {
            MonitorSchedule::Crontab { value } => {
                if value.len() > 128 || value.split_whitespace().count() != 5 {
                    return Err(invalid());
                }
                let local = now.with_timezone(&tz);
                // Restrict search to eight years instead of letting impossible
                // public-DSN schedules scan millennia. Includes leap-day cycles.
                let year = local.year();
                let schedule = croner::Cron::from_str(&format!("0 {value} {year}-{}", year + 8))
                    .map_err(|_| invalid())?;
                schedule
                    .find_next_occurrence(&local, false)
                    .map(|v| v.with_timezone(&Utc))
                    .map_err(|_| invalid())
            }
            MonitorSchedule::Interval { value, unit } => {
                let minutes = match unit.as_str() {
                    "minute" => 1,
                    "hour" => 60,
                    "day" => 1440,
                    "week" => 10080,
                    _ => return Err(invalid()),
                };
                if *value == 0 || i64::from(*value) * minutes > 525600 {
                    return Err(invalid());
                }
                Ok(now + Duration::minutes(i64::from(*value) * minutes))
            }
        }
    }
}
#[derive(Clone, Debug)]
pub struct MonitorCheckIn {
    pub id: String,
    pub slug: String,
    pub status: String,
    pub duration: Option<f64>,
    pub config: Option<MonitorConfig>,
    pub release: Option<String>,
    pub environment: Option<String>,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
}
impl MonitorCheckIn {
    pub fn validate(&self, now: DateTime<Utc>) -> Result<(), CronMonitorError> {
        if self.id.is_empty()
            || self.id.len() > 64
            || !self
                .id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
            || self.slug.is_empty()
            || self.slug.len() > 128
            || !self
                .slug
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
            || !["in_progress", "ok", "error"].contains(&self.status.as_str())
            || self
                .duration
                .is_some_and(|v| !v.is_finite() || !(0.0..=31536000.0).contains(&v))
            || [&self.release, &self.environment].iter().any(|v| {
                v.as_ref()
                    .is_some_and(|s| s.len() > 256 || s.contains('\0'))
            })
        {
            return Err(CronMonitorError::Invalid(
                "check-in fields exceed supported limits".into(),
            ));
        }
        for (id, len) in [(&self.trace_id, 32), (&self.span_id, 16)] {
            if id.as_ref().is_some_and(|s| {
                s.len() != len
                    || !s.bytes().all(|c| c.is_ascii_hexdigit())
                    || s.bytes().all(|c| c == b'0')
            }) {
                return Err(CronMonitorError::Invalid("invalid trace context".into()));
            }
        }
        if let Some(config) = &self.config {
            config.validate(now)?;
        }
        Ok(())
    }
}
#[derive(Debug, Serialize, FromQueryResult)]
pub struct CronMonitor {
    pub id: i32,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub slug: String,
    pub config: serde_json::Value,
    pub status: String,
    pub consecutive_failures: i32,
    pub consecutive_successes: i32,
    pub next_expected_at: DateTime<Utc>,
    pub next_check_at: DateTime<Utc>,
    pub last_check_in_at: Option<DateTime<Utc>>,
    pub enabled: bool,
    pub managed_config: bool,
    pub alarm_id: Option<i32>,
}
#[derive(Debug, Serialize, FromQueryResult, utoipa::ToSchema)]
pub struct CronMonitorRun {
    pub id: i64,
    pub monitor_id: i32,
    pub check_in_id: String,
    pub status: String,
    #[schema(value_type = String, format = DateTime)]
    pub started_at: DateTime<Utc>,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub finished_at: Option<DateTime<Utc>>,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub deadline_at: Option<DateTime<Utc>>,
    pub duration: Option<f64>,
    pub release: Option<String>,
    pub environment: Option<String>,
    pub deployment_id: Option<i32>,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
}
fn sql(text: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DatabaseBackend::Postgres, text, values)
}

#[derive(Debug, Default)]
pub struct CheckInIngestResult {
    pub accepted: u64,
    pub pending_configuration: u64,
}

pub struct CronMonitorService {
    db: Arc<DatabaseConnection>,
}
impl CronMonitorService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }
    /// At most 100 check-ins in one transaction, 100 monitors per project and
    /// 10000 retained runs per monitor. Replays of a terminal ID are no-ops.
    pub async fn ingest(
        &self,
        project: i32,
        environment: Option<i32>,
        deployment: Option<i32>,
        items: Vec<MonitorCheckIn>,
    ) -> Result<u64, CronMonitorError> {
        Ok(self
            .ingest_report(project, environment, deployment, items)
            .await?
            .accepted)
    }
    pub async fn ingest_report(
        &self,
        project: i32,
        environment: Option<i32>,
        deployment: Option<i32>,
        items: Vec<MonitorCheckIn>,
    ) -> Result<CheckInIngestResult, CronMonitorError> {
        self.ingest_report_at(project, environment, deployment, items, Utc::now())
            .await
    }
    pub async fn ingest_at(
        &self,
        project: i32,
        environment: Option<i32>,
        deployment: Option<i32>,
        items: Vec<MonitorCheckIn>,
        now: DateTime<Utc>,
    ) -> Result<u64, CronMonitorError> {
        Ok(self
            .ingest_report_at(project, environment, deployment, items, now)
            .await?
            .accepted)
    }
    async fn ingest_report_at(
        &self,
        project: i32,
        environment: Option<i32>,
        deployment: Option<i32>,
        items: Vec<MonitorCheckIn>,
        now: DateTime<Utc>,
    ) -> Result<CheckInIngestResult, CronMonitorError> {
        async {
        if items.len() > 100 {
            return Err(CronMonitorError::Limit);
        }
        for item in &items {
            item.validate(now)?;
        }
        let tx = self.db.begin().await?;
        // Serialize project admission and monitor creation across server instances.
        tx.execute(sql(
            "SELECT pg_advisory_xact_lock(1936027246, $1)",
            vec![project.into()],
        ))
        .await?;
        // The service itself enforces ownership, including callers outside DSN ingress.
        let owned=tx.query_one(sql("SELECT id FROM projects WHERE id=$1 AND is_deleted=FALSE AND deleted_at IS NULL AND ($2::int IS NULL OR EXISTS(SELECT 1 FROM environments WHERE id=$2 AND project_id=$1 AND deleted_at IS NULL)) AND ($3::int IS NULL OR EXISTS(SELECT 1 FROM deployments WHERE id=$3 AND project_id=$1 AND ($2::int IS NULL OR environment_id=$2)))",vec![project.into(),environment.into(),deployment.into()])).await?;
        if owned.is_none() {
            return Err(CronMonitorError::Invalid(
                "invalid project/environment/deployment ownership".into(),
            ));
        }
        let mut pending = std::collections::BTreeMap::<i32, u64>::new();
        for item in &items {
            let mut monitor=CronMonitor::find_by_statement(sql("SELECT * FROM cron_monitors WHERE project_id=$1 AND environment_id IS NOT DISTINCT FROM $2 AND slug=$3 FOR UPDATE", vec![project.into(),environment.into(),item.slug.clone().into()])).one(&tx).await?;
            if monitor.is_none() {
                let count = tx
                    .query_one(sql(
                        "SELECT count(*) AS n FROM cron_monitors WHERE project_id=$1",
                        vec![project.into()],
                    ))
                    .await?
                    .ok_or_else(|| DbErr::Custom("monitor count missing".into()))?
                    .try_get::<i64>("", "n")?;
                if count >= 100 {
                    return Err(CronMonitorError::Limit);
                }
                // Completions can arrive before the SDK's start/configuration.
                // Preserve them without inventing a schedule or runtime limit.
                let parked = chrono::DateTime::parse_from_rfc3339("9999-12-31T00:00:00Z")
                    .map_err(|_| DbErr::Custom("invalid parked deadline".into()))?
                    .with_timezone(&Utc);
                let (next, due) = match item.config.as_ref() {
                    Some(config) => {
                        let next = config.next_after(now)?;
                        (
                            next,
                            next + Duration::minutes(i64::from(config.checkin_margin)),
                        )
                    }
                    None => (parked, parked),
                };
                monitor=CronMonitor::find_by_statement(sql("INSERT INTO cron_monitors(project_id,environment_id,slug,config,next_expected_at,next_check_at,last_check_in_at,status) VALUES($1,$2,$3,$4,$5,$6,$7,$8) RETURNING *",vec![project.into(),environment.into(),item.slug.clone().into(),serde_json::to_value(item.config.as_ref()).map_err(|_|CronMonitorError::Invalid("configuration".into()))?.into(),next.into(),due.into(),now.into(),(if item.config.is_some(){"unknown"}else{"unconfigured"}).into()])).one(&tx).await?;
            }
            let mut monitor =
                monitor.ok_or_else(|| DbErr::Custom("monitor missing after insert".into()))?;
            let mut config: Option<MonitorConfig> = if monitor.config.is_null() {
                None
            } else {
                Some(serde_json::from_value(monitor.config.clone()).map_err(|_| {
                    CronMonitorError::StoredConfiguration {
                        monitor_id: monitor.id,
                    }
                })?)
            };
            let was_unconfigured = config.is_none();
            // Configure before checking terminal immutability: a late start must
            // configure the monitor even when its completion is already stored.
            if was_unconfigured && !monitor.managed_config {
                if let Some(updated) = &item.config {
                    Self::configure_monitor(&tx, &mut monitor, updated, now).await?;
                    config = Some(updated.clone());
                }
            }
            if let Some(config) = &config {
                Self::initialize_deadlines(&tx, monitor.id, config).await?;
                if monitor.enabled {
                    Self::evaluate_deadlines(&tx, &monitor, config, now).await?;
                }
                Self::reschedule(&tx, monitor.id, config).await?;
                pending.remove(&monitor.id);
            } else {
                *pending.entry(monitor.id).or_default() += 1;
            }
            let run = CronMonitorRun::find_by_statement(sql(
                "SELECT * FROM cron_monitor_runs WHERE monitor_id=$1 AND check_in_id=$2",
                vec![monitor.id.into(), item.id.clone().into()],
            ))
            .one(&tx)
            .await?;
            if let Some(run) = &run {
                // A monitor may have more overdue runs than the worker page.
                // Always enforce this check-in's own deadline before accepting
                // a completion, even when it falls beyond that bounded page.
                let deadline = run.deadline_at.or_else(|| {
                    config.as_ref().map(|config| {
                        run.started_at + Duration::minutes(i64::from(config.max_runtime))
                    })
                });
                if monitor.enabled
                    && run.status == "in_progress"
                    && deadline.is_some_and(|at| at <= now)
                {
                    tx.execute(sql("UPDATE cron_monitor_runs SET status='timeout',finished_at=$2,deadline_at=NULL WHERE id=$1",vec![run.id.into(),now.into()])).await?;
                    let configured =
                        config
                            .as_ref()
                            .ok_or(CronMonitorError::StoredConfiguration {
                                monitor_id: monitor.id,
                            })?;
                    Self::record_result(&tx, monitor.id, false, configured, 1).await?;
                    Self::reschedule(&tx, monitor.id, configured).await?;
                    continue;
                }
                // Terminals are immutable, including conflicting SDK retries and
                // late starts after a completion or a server-detected timeout.
                if run.status != "in_progress" || item.status == "in_progress" {
                    continue;
                }
            }
            if !was_unconfigured && !monitor.managed_config {
                if let Some(updated) = &item.config {
                    Self::configure_monitor(&tx, &mut monitor, updated, now).await?;
                    config = Some(updated.clone());
                }
            }
            if run.is_none() {
                Self::reserve_run_slot(&tx, monitor.id).await?;
                tx.execute(sql("INSERT INTO cron_monitor_runs(monitor_id,check_in_id,status,started_at,finished_at,deadline_at,duration,release,environment,deployment_id,trace_id,span_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",vec![monitor.id.into(),item.id.clone().into(),item.status.clone().into(),now.into(),(if item.status=="in_progress" {None} else {Some(now)}).into(),(if item.status=="in_progress" {config.as_ref().map(|config|now+Duration::minutes(i64::from(config.max_runtime)))} else {None}).into(),item.duration.into(),item.release.clone().into(),item.environment.clone().into(),deployment.into(),item.trace_id.clone().into(),item.span_id.clone().into()])).await?;
            } else {
                tx.execute(sql("UPDATE cron_monitor_runs SET status=$3,finished_at=$4,deadline_at=NULL,duration=$5,release=COALESCE($6,release),environment=COALESCE($7,environment),trace_id=COALESCE($8,trace_id),span_id=COALESCE($9,span_id) WHERE monitor_id=$1 AND check_in_id=$2",vec![monitor.id.into(),item.id.clone().into(),item.status.clone().into(),now.into(),item.duration.into(),item.release.clone().into(),item.environment.clone().into(),item.trace_id.clone().into(),item.span_id.clone().into()])).await?;
            }
            if item.status != "in_progress" {
                if let Some(config) = &config {
                    Self::record_result(&tx, monitor.id, item.status == "ok", config, 1).await?;
                } else {
                    tx.execute(sql("UPDATE cron_monitors SET consecutive_failures=CASE WHEN $2 THEN 0 ELSE LEAST(consecutive_failures+1,1000000) END,consecutive_successes=CASE WHEN $2 THEN LEAST(consecutive_successes+1,1000000) ELSE 0 END,status='unconfigured' WHERE id=$1 AND enabled",vec![monitor.id.into(),(item.status=="ok").into()])).await?;
                }
            }
            if let Some(config) = &config {
                // Paired completions do not shift the next expected start.
                tx.execute(sql("UPDATE cron_monitors SET last_check_in_at=$2,next_expected_at=CASE WHEN $4 THEN $3 ELSE next_expected_at END WHERE id=$1",vec![monitor.id.into(),now.into(),config.next_after(now)?.into(),(run.is_none()||(!monitor.managed_config && item.config.is_some())).into()])).await?;
                Self::reschedule(&tx, monitor.id, config).await?;
            } else {
                tx.execute(sql(
                    "UPDATE cron_monitors SET last_check_in_at=$2 WHERE id=$1",
                    vec![monitor.id.into(), now.into()],
                ))
                .await?;
            }
        }
        tx.commit().await?;
        Ok(CheckInIngestResult {
            accepted: items.len() as u64,
            pending_configuration: pending.values().sum(),
        })

        }.await.map_err(|error: CronMonitorError| error.with_context("ingest_report_at", Some(project), None))
    }
    async fn configure_monitor(
        tx: &DatabaseTransaction,
        monitor: &mut CronMonitor,
        config: &MonitorConfig,
        now: DateTime<Utc>,
    ) -> Result<(), CronMonitorError> {
        monitor.config = serde_json::to_value(config)
            .map_err(|_| CronMonitorError::Invalid("configuration".into()))?;
        monitor.next_expected_at = config.next_after(now)?;
        tx.execute(sql("UPDATE cron_monitors SET config=$2,next_expected_at=$3,alarm_pending=alarm_pending OR status='unconfigured',status=CASE WHEN status='unconfigured' THEN CASE WHEN consecutive_failures >= $4 THEN 'failing' WHEN consecutive_successes >= $5 THEN 'ok' ELSE 'unknown' END ELSE status END WHERE id=$1",vec![monitor.id.into(),monitor.config.clone().into(),monitor.next_expected_at.into(),(config.failure_issue_threshold as i32).into(),(config.recovery_threshold as i32).into()])).await?;
        Ok(())
    }
    async fn initialize_deadlines(
        tx: &DatabaseTransaction,
        monitor: i32,
        config: &MonitorConfig,
    ) -> Result<(), DbErr> {
        tx.execute(sql("UPDATE cron_monitor_runs SET deadline_at=started_at+$2::int * INTERVAL '1 minute' WHERE id IN (SELECT id FROM cron_monitor_runs WHERE monitor_id=$1 AND status='in_progress' AND deadline_at IS NULL ORDER BY id LIMIT 100)",vec![monitor.into(),(config.max_runtime as i32).into()])).await?;
        Ok(())
    }
    async fn reserve_run_slot(
        tx: &DatabaseTransaction,
        monitor: i32,
    ) -> Result<(), CronMonitorError> {
        let count = tx
            .query_one(sql(
                "SELECT count(*) AS n FROM cron_monitor_runs WHERE monitor_id=$1",
                vec![monitor.into()],
            ))
            .await?
            .ok_or_else(|| DbErr::Custom("run count missing".into()))?
            .try_get::<i64>("", "n")?;
        if count >= 10000 {
            let removed=tx.execute(sql("DELETE FROM cron_monitor_runs WHERE id IN (SELECT id FROM cron_monitor_runs WHERE monitor_id=$1 AND finished_at IS NOT NULL ORDER BY id LIMIT 100)",vec![monitor.into()])).await?.rows_affected();
            if removed == 0 {
                return Err(CronMonitorError::Limit);
            }
        }
        Ok(())
    }
    async fn record_result(
        tx: &DatabaseTransaction,
        id: i32,
        ok: bool,
        config: &MonitorConfig,
        count: i32,
    ) -> Result<(), DbErr> {
        tx.execute(sql("UPDATE cron_monitors SET alarm_pending=true, consecutive_failures=CASE WHEN $2 THEN 0 ELSE LEAST(consecutive_failures+$5,1000000) END, consecutive_successes=CASE WHEN $2 THEN LEAST(consecutive_successes+1,1000000) ELSE 0 END,status=CASE WHEN $2 AND consecutive_successes+1 >= $4 THEN 'ok' WHEN NOT $2 AND consecutive_failures+$5 >= $3 THEN 'failing' ELSE status END WHERE id=$1 AND enabled",vec![id.into(),ok.into(),(config.failure_issue_threshold as i32).into(),(config.recovery_threshold as i32).into(),count.into()])).await?;
        Ok(())
    }
    async fn reschedule(
        tx: &DatabaseTransaction,
        id: i32,
        config: &MonitorConfig,
    ) -> Result<(), DbErr> {
        tx.execute(sql("UPDATE cron_monitors SET next_check_at=LEAST(next_expected_at + $2::int * INTERVAL '1 minute',(SELECT min(COALESCE(deadline_at,started_at)) FROM cron_monitor_runs WHERE monitor_id=$1 AND status='in_progress')) WHERE id=$1",vec![id.into(),(config.checkin_margin as i32).into()])).await?;
        Ok(())
    }
    async fn evaluate_deadlines(
        tx: &DatabaseTransaction,
        monitor: &CronMonitor,
        config: &MonitorConfig,
        now: DateTime<Utc>,
    ) -> Result<(), CronMonitorError> {
        let timed_out=tx.execute(sql("UPDATE cron_monitor_runs SET status='timeout',finished_at=$2,deadline_at=NULL WHERE id IN (SELECT id FROM cron_monitor_runs WHERE monitor_id=$1 AND status='in_progress' AND deadline_at <= $2 ORDER BY deadline_at LIMIT 100)",vec![monitor.id.into(),now.into()])).await?.rows_affected();
        if timed_out > 0 {
            Self::record_result(tx, monitor.id, false, config, timed_out.min(1000000) as i32)
                .await?;
        }
        if monitor.next_expected_at + Duration::minutes(i64::from(config.checkin_margin)) <= now {
            Self::reserve_run_slot(tx, monitor.id).await?;
            let missed_id = format!("missed:{}", monitor.next_expected_at.timestamp());
            tx.execute(sql("INSERT INTO cron_monitor_runs(monitor_id,check_in_id,status,started_at,finished_at) VALUES($1,$2,'missed',$3,$4) ON CONFLICT DO NOTHING",vec![monitor.id.into(),missed_id.into(),monitor.next_expected_at.into(),now.into()])).await?;
            Self::record_result(tx, monitor.id, false, config, 1).await?;
            tx.execute(sql(
                "UPDATE cron_monitors SET next_expected_at=$2 WHERE id=$1",
                vec![monitor.id.into(), config.next_after(now)?.into()],
            ))
            .await?;
        }
        Ok(())
    }
    /// Indexed, bounded work. Multiple instances claim disjoint due rows.
    /// Downtime coalesces missed occurrences into one failure per monitor;
    /// it never creates an unbounded historical backlog after a restart.
    pub async fn detect_due(&self, now: DateTime<Utc>) -> Result<usize, CronMonitorError> {
        async {
        let tx = self.db.begin().await?;
        let due=CronMonitor::find_by_statement(sql("SELECT * FROM cron_monitors WHERE enabled AND next_check_at <= $1 ORDER BY next_check_at LIMIT 100 FOR UPDATE SKIP LOCKED",vec![now.into()])).all(&tx).await?;
        for monitor in &due {
            let live=tx.query_one(sql("SELECT id FROM projects WHERE id=$1 AND is_deleted=FALSE AND deleted_at IS NULL AND ($2::int IS NULL OR EXISTS(SELECT 1 FROM environments WHERE id=$2 AND project_id=$1 AND deleted_at IS NULL))",vec![monitor.project_id.into(),monitor.environment_id.into()])).await?;
            if live.is_none() {
                // Park deleted scopes out of the due index. A future valid
                // check-in after restoration reschedules the monitor normally.
                tx.execute(sql("UPDATE cron_monitors SET next_check_at='9999-12-31 00:00:00+00',alarm_pending=false WHERE id=$1",vec![monitor.id.into()])).await?;
                continue;
            }
            let config: MonitorConfig =
                serde_json::from_value(monitor.config.clone()).map_err(|_| {
                    CronMonitorError::StoredConfiguration {
                        monitor_id: monitor.id,
                    }
                })?;
            Self::initialize_deadlines(&tx, monitor.id, &config).await.map_err(|error| CronMonitorError::from(error).with_context("evaluate cron deadlines", Some(monitor.project_id), Some(monitor.id)))?;
            Self::evaluate_deadlines(&tx, monitor, &config, now).await.map_err(|error| error.with_context("evaluate cron deadlines", Some(monitor.project_id), Some(monitor.id)))?;
            Self::reschedule(&tx, monitor.id, &config).await.map_err(|error| CronMonitorError::from(error).with_context("evaluate cron deadlines", Some(monitor.project_id), Some(monitor.id)))?;
        }
        tx.commit().await?;
        // Retention holds no monitor locks, so pruning another monitor's rows
        // cannot deadlock a multi-monitor check-in batch. At most 1000 per tick.
        self.db.execute(sql("DELETE FROM cron_monitor_runs WHERE id IN (SELECT id FROM cron_monitor_runs WHERE finished_at < $1 ORDER BY finished_at LIMIT 1000)",vec![(now-Duration::days(30)).into()])).await?;
        Ok(due.len())

        }.await.map_err(|error: CronMonitorError| error.with_context("detect_due", None, None))
    }
    pub async fn list(
        &self,
        project: i32,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<CronMonitor>, CronMonitorError> {
        async {
            Ok(CronMonitor::find_by_statement(sql(
                "SELECT * FROM cron_monitors WHERE project_id=$1 ORDER BY id LIMIT $2 OFFSET $3",
                vec![
                    project.into(),
                    (limit.clamp(1, 100) as i64).into(),
                    (offset.min(i64::MAX as u64) as i64).into(),
                ],
            ))
            .all(self.db.as_ref())
            .await?)
        }
        .await
        .map_err(|error: CronMonitorError| error.with_context("list", Some(project), None))
    }
    pub async fn runs(
        &self,
        project: i32,
        monitor: i32,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<CronMonitorRun>, CronMonitorError> {
        async {
        Ok(CronMonitorRun::find_by_statement(sql("SELECT r.* FROM cron_monitor_runs r JOIN cron_monitors m ON m.id=r.monitor_id WHERE m.project_id=$1 AND m.id=$2 ORDER BY r.id DESC LIMIT $3 OFFSET $4",vec![project.into(),monitor.into(),(limit.clamp(1,100) as i64).into(),(offset.min(i64::MAX as u64) as i64).into()])).all(self.db.as_ref()).await?)

        }.await.map_err(|error: CronMonitorError| error.with_context("runs", Some(project), Some(monitor)))
    }
    /// Durable pending flags retry alarm publication after process/storage failures.
    /// Native alarms expose failures in the existing console and notification routes.
    pub async fn sync_alarms(&self, alarms: &crate::AlarmService) -> Result<(), CronMonitorError> {
        async {
        let tx = self.db.begin().await?;
        let pending=tx.query_all(sql("SELECT m.*,a.id AS active_alarm_id FROM cron_monitors m LEFT JOIN alarms a ON a.id=m.alarm_id AND a.project_id=m.project_id AND a.alarm_type='cron_job_failed' AND a.status IN ('firing','acknowledged') AND a.metadata->>'cron_monitor_id'=m.id::text WHERE m.alarm_pending ORDER BY m.id LIMIT 100 FOR UPDATE OF m SKIP LOCKED",vec![])).await?;
        for row in pending {
            let monitor = CronMonitor::from_query_result(&row, "")?;
            let project_id = monitor.project_id;
            let monitor_id = monitor.id;
            async {
            // The bounded batch joins active alarms. A manually resolved
            // historical link cannot suppress the next failing run.
            let mut alarm_id: Option<i32> = row.try_get("", "active_alarm_id")?;
            if monitor.enabled && monitor.status == "failing" {
                if alarm_id.is_none() {
                    // Recover the link if the process stopped after AlarmService
                    // committed but before this transaction committed its pointer.
                    alarm_id=tx.query_one(sql("SELECT id FROM alarms WHERE project_id=$1 AND alarm_type='cron_job_failed' AND status IN ('firing','acknowledged') AND metadata->>'cron_monitor_id'=$2 ORDER BY id DESC LIMIT 1",vec![monitor.project_id.into(),monitor.id.to_string().into()])).await?.map(|r|r.try_get("","id")).transpose()?;
                }
                if alarm_id.is_none() {
                    let recent = self.runs(monitor.project_id, monitor.id, 1, 0).await?;
                    alarm_id=alarms.fire_alarm(crate::FireAlarmRequest {
                        project_id:Some(monitor.project_id),environment_id:monitor.environment_id,
                        deployment_id:None,container_id:None,service_id:None,
                        alarm_type:crate::AlarmType::CronJobFailed,severity:crate::AlarmSeverity::Warning,
                        title:format!("Cron monitor {} is failing", monitor.slug),
                        message:format!("Latest run: {}. Consecutive failures: {}; consecutive successes: {}. Recovery requires {} successful runs.",recent.first().map(|run|run.status.as_str()).unwrap_or("unknown"),monitor.consecutive_failures,monitor.consecutive_successes,monitor.config["recovery_threshold"]),
                        metadata:Some(serde_json::json!({"cron_monitor_id":monitor.id,"monitor_slug":monitor.slug,"config":monitor.config,"latest_run":recent.first(),"consecutive_failures":monitor.consecutive_failures})),
                    }).await.map_err(|source| CronMonitorError::from(source).with_context("raise cron failure alarm", Some(monitor.project_id), Some(monitor.id)))?;
                }
            } else if monitor.status == "ok" || monitor.status == "unknown" || !monitor.enabled {
                if let Some(id) = alarm_id {
                    alarms.resolve_alarm(id, Some(monitor.project_id)).await.map_err(|source| CronMonitorError::from(source).with_context("resolve cron failure alarm", Some(monitor.project_id), Some(monitor.id)))?;
                }
                alarm_id = None;
            }
            let retry = monitor.enabled && monitor.status == "failing" && alarm_id.is_none();
            tx.execute(sql(
                "UPDATE cron_monitors SET alarm_id=$2,alarm_pending=$3 WHERE id=$1",
                vec![monitor.id.into(), alarm_id.into(), retry.into()],
            ))
            .await?;
                Ok::<(), CronMonitorError>(())
            }.await.map_err(|error| error.with_context("synchronize monitor alarm", Some(project_id), Some(monitor_id)))?;

        }
        tx.commit().await?;
        Ok(())

        }.await.map_err(|error: CronMonitorError| error.with_context("sync_alarms", None, None))
    }
    pub fn start(self: &Arc<Self>, alarms: Arc<crate::AlarmService>) {
        let service = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(15));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                if let Err(error) = service.detect_due(Utc::now()).await {
                    tracing::error!(error = %error, "Cron monitor deadline detection failed");
                }
                if let Err(error) = service.sync_alarms(&alarms).await {
                    tracing::error!(error = %error, "Cron monitor alarm synchronization failed");
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, Set};
    pub(super) struct Notifications;
    #[async_trait::async_trait]
    impl temps_core::notifications::NotificationService for Notifications {
        async fn send_notification(
            &self,
            _: temps_core::notifications::NotificationData,
        ) -> Result<(), temps_core::notifications::NotificationError> {
            Ok(())
        }
        async fn send_email(
            &self,
            _: temps_core::notifications::EmailMessage,
        ) -> Result<(), temps_core::notifications::NotificationError> {
            Ok(())
        }
        async fn is_configured(
            &self,
        ) -> Result<bool, temps_core::notifications::NotificationError> {
            Ok(false)
        }
    }
    pub(super) struct Queue;
    #[async_trait::async_trait]
    impl temps_core::JobQueue for Queue {
        async fn send(&self, _: temps_core::Job) -> Result<(), temps_core::jobs::QueueError> {
            Ok(())
        }
        fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
            panic!("unused in cron monitor test")
        }
    }
    #[test]
    fn storage_errors_preserve_operation_scope_source_and_retry_policy() {
        use std::error::Error;
        let error = CronMonitorError::from(DbErr::Conn(sea_orm::RuntimeErr::Internal(
            "synthetic unavailable".into(),
        )))
        .with_context("persist check-in", Some(17), Some(23));
        let message = error.to_string();
        for expected in ["persist check-in", "17", "23", "synthetic unavailable"] {
            assert!(message.contains(expected), "{message}");
        }
        assert!(error.is_retryable());
        assert!(error.source().is_some());
        let permanent = CronMonitorError::from(DbErr::Type("synthetic invalid type".into()))
            .with_context("read monitor", Some(17), Some(23));
        assert!(!permanent.is_retryable());
        let alarm = CronMonitorError::from(crate::AlarmError::Database {
            operation: "insert alarm".into(),
            reason: "synthetic alarm storage failure".into(),
        })
        .with_context("raise cron failure alarm", Some(17), Some(23));
        for expected in [
            "raise cron failure alarm",
            "17",
            "23",
            "insert alarm",
            "synthetic alarm storage failure",
        ] {
            assert!(alarm.to_string().contains(expected));
        }
        assert!(alarm.source().is_some());
    }

    #[test]
    fn schedules_validate_timezone_units_and_minute_precision() {
        let now = DateTime::parse_from_rfc3339("2026-03-29T00:59:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let config:MonitorConfig=serde_json::from_value(serde_json::json!({"schedule":{"type":"crontab","value":"0 3 * * *"},"timezone":"Europe/Madrid"})).unwrap();
        assert_eq!(
            config.next_after(now).unwrap().to_rfc3339(),
            "2026-03-29T01:00:00+00:00"
        );
        let monday = DateTime::parse_from_rfc3339("2026-10-19T00:01:00Z")
            .unwrap()
            .with_timezone(&Utc);
        for (expression, expected) in [
            ("0 0 * * 0", "2026-10-25T00:00:00+00:00"),
            ("0 0 * * 7", "2026-10-25T00:00:00+00:00"),
            ("0 0 * * 1-5", "2026-10-20T00:00:00+00:00"),
            ("0 0 1 * 1", "2026-10-26T00:00:00+00:00"),
        ] {
            let config: MonitorConfig = serde_json::from_value(
                serde_json::json!({"schedule":{"type":"crontab","value":expression}}),
            )
            .unwrap();
            assert_eq!(
                config.next_after(monday).unwrap().to_rfc3339(),
                expected,
                "Unix crontab {expression}"
            );
        }
        for value in [
            serde_json::json!({"schedule":{"type":"interval","value":0,"unit":"minute"}}),
            serde_json::json!({"schedule":{"type":"interval","value":1,"unit":"second"}}),
            serde_json::json!({"schedule":{"type":"crontab","value":"* * * * * *"}}),
            serde_json::json!({"schedule":{"type":"crontab","value":"0 0 31 2 *"}}),
            serde_json::json!({"schedule":{"type":"crontab","value":"* * * * *"},"timezone":"invalid"}),
        ] {
            assert!(serde_json::from_value::<MonitorConfig>(value)
                .unwrap()
                .validate(now)
                .is_err());
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn persisted_check_ins_deadlines_retries_thresholds_and_ownership() {
        let db = match temps_test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(e) if temps_test_utils::is_container_runtime_unavailable(&e.to_string()) => {
                eprintln!("Cron DB integration unavailable: {e}");
                return;
            }
            Err(e) => panic!("Cron DB setup failed: {e}"),
        };
        let project = temps_entities::projects::ActiveModel {
            name: Set("Synthetic monitor".into()),
            slug: Set(uuid::Uuid::new_v4().to_string()),
            repo_name: Set("synthetic".into()),
            repo_owner: Set("synthetic".into()),
            directory: Set("/synthetic".into()),
            main_branch: Set("main".into()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.connection())
        .await
        .unwrap();
        let service = Arc::new(CronMonitorService::new(db.connection_arc()));
        let now = DateTime::parse_from_rfc3339("2026-10-19T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let config:MonitorConfig=serde_json::from_value(serde_json::json!({"schedule":{"type":"interval","value":5,"unit":"minute"},"max_runtime":1,"failure_issue_threshold":2,"recovery_threshold":2})).unwrap();
        let item = |id: &str, status: &str| MonitorCheckIn {
            id: id.into(),
            slug: "synthetic-job".into(),
            status: status.into(),
            duration: None,
            config: Some(config.clone()),
            release: Some("synthetic-release".into()),
            environment: Some("synthetic-env".into()),
            trace_id: Some("a".repeat(32)),
            span_id: Some("b".repeat(16)),
        };
        let first = item(&"1".repeat(32), "in_progress");
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let service = service.clone();
            let first = first.clone();
            tasks.spawn(async move {
                service
                    .ingest_at(project.id, None, None, vec![first], now)
                    .await
                    .unwrap()
            });
        }
        while let Some(task) = tasks.join_next().await {
            assert_eq!(task.unwrap(), 1);
        }
        let monitors = service.list(project.id, 100, 0).await.unwrap();
        assert_eq!(monitors.len(), 1);
        let id = monitors[0].id;
        assert_eq!(service.runs(project.id, id, 100, 0).await.unwrap().len(), 1);
        assert_eq!(
            service
                .detect_due(now + Duration::seconds(59))
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            service
                .detect_due(now + Duration::minutes(1))
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            service.runs(project.id, id, 100, 0).await.unwrap()[0].status,
            "timeout"
        );
        assert_eq!(
            service.list(project.id, 100, 0).await.unwrap()[0].status,
            "unknown"
        );
        // Late completion and retried starts cannot undo a timeout.
        service
            .ingest_at(
                project.id,
                None,
                None,
                vec![item(&"1".repeat(32), "ok"), first.clone()],
                now + Duration::minutes(2),
            )
            .await
            .unwrap();
        assert_eq!(
            service.runs(project.id, id, 100, 0).await.unwrap()[0].status,
            "timeout"
        );
        let mut detectors = tokio::task::JoinSet::new();
        for _ in 0..4 {
            let service = service.clone();
            detectors.spawn(async move {
                service
                    .detect_due(now + Duration::minutes(6))
                    .await
                    .unwrap()
            });
        }
        let mut claimed = 0;
        while let Some(result) = detectors.join_next().await {
            claimed += result.unwrap();
        }
        assert_eq!(claimed, 1);
        let monitor = service.list(project.id, 100, 0).await.unwrap().remove(0);
        assert_eq!(monitor.status, "failing");
        assert_eq!(monitor.consecutive_failures, 2);
        let alarms = crate::AlarmService::new(
            db.connection_arc(),
            Arc::new(Notifications),
            Arc::new(Queue),
        );
        service.sync_alarms(&alarms).await.unwrap();
        service.sync_alarms(&alarms).await.unwrap();
        let (native_alarms, total) = alarms
            .list_alarms(Some(project.id), crate::AlarmFilters::default(), 1, 20)
            .await
            .unwrap();
        assert_eq!(total, 1);
        assert_eq!(
            native_alarms[0].metadata.as_ref().unwrap()["latest_run"]["status"],
            "missed"
        );
        assert_eq!(
            native_alarms[0].metadata.as_ref().unwrap()["monitor_slug"],
            "synthetic-job"
        );
        let active = db
            .connection()
            .query_one(sql(
                "SELECT count(*) AS n FROM alarms WHERE project_id=$1 AND status='firing'",
                vec![project.id.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            active.try_get::<i64>("", "n").unwrap(),
            1,
            "alarm publication is idempotent"
        );
        // Manual resolution is historical, not a permanent mute. A new failed
        // run after cooldown must create a new alarm; repeated syncs reuse it.
        let old_alarm = native_alarms[0].id;
        alarms
            .resolve_alarm(old_alarm, Some(project.id))
            .await
            .unwrap();
        db.connection()
            .execute(sql(
                "UPDATE alarms SET fired_at=NOW()-INTERVAL '6 minutes' WHERE id=$1",
                vec![old_alarm.into()],
            ))
            .await
            .unwrap();
        let failure = item(&"c".repeat(32), "error");
        service
            .ingest_at(
                project.id,
                None,
                None,
                vec![failure],
                now + Duration::minutes(6),
            )
            .await
            .unwrap();
        service.sync_alarms(&alarms).await.unwrap();
        let replacement = service.list(project.id, 100, 0).await.unwrap()[0]
            .alarm_id
            .unwrap();
        assert_ne!(replacement, old_alarm);
        service.sync_alarms(&alarms).await.unwrap();
        assert_eq!(
            service.list(project.id, 100, 0).await.unwrap()[0].alarm_id,
            Some(replacement)
        );
        let runs = service.runs(project.id, id, 100, 0).await.unwrap();
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].status, "error");
        assert!(runs.iter().any(|run| run.status == "missed"));
        let mut other = item(&"a".repeat(32), "error");
        other.slug = "synthetic-other-job".into();
        other.config.as_mut().unwrap().failure_issue_threshold = 1;
        other.config.as_mut().unwrap().recovery_threshold = 1;
        service
            .ingest_at(
                project.id,
                None,
                None,
                vec![other.clone()],
                now + Duration::minutes(6),
            )
            .await
            .unwrap();
        service.sync_alarms(&alarms).await.unwrap();
        let active = db
            .connection()
            .query_one(sql(
                "SELECT count(*) AS n FROM alarms WHERE project_id=$1 AND status='firing'",
                vec![project.id.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            active.try_get::<i64>("", "n").unwrap(),
            2,
            "each monitor has a separate native alarm cooldown"
        );
        other.id = "b".repeat(32);
        other.status = "ok".into();
        service
            .ingest_at(
                project.id,
                None,
                None,
                vec![other],
                now + Duration::minutes(6),
            )
            .await
            .unwrap();
        service.sync_alarms(&alarms).await.unwrap();
        // Native history cannot be queried through another project.
        assert!(service
            .runs(project.id + 1, id, 100, 0)
            .await
            .unwrap()
            .is_empty());
        assert!(service
            .ingest_at(project.id, Some(i32::MAX), None, vec![first], now)
            .await
            .is_err());
        for (key, expected) in [("2", "failing"), ("3", "ok")] {
            let success = item(&key.repeat(32), "ok");
            service
                .ingest_at(
                    project.id,
                    None,
                    None,
                    vec![success.clone(), success],
                    now + Duration::minutes(7),
                )
                .await
                .unwrap();
            assert_eq!(
                service.list(project.id, 100, 0).await.unwrap()[0].status,
                expected
            );
        }
        service.sync_alarms(&alarms).await.unwrap();
        let active = db
            .connection()
            .query_one(sql(
                "SELECT count(*) AS n FROM alarms WHERE project_id=$1 AND status='firing'",
                vec![project.id.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            active.try_get::<i64>("", "n").unwrap(),
            0,
            "recovery resolves the native alarm"
        );
        let mut unknown = item(&"4".repeat(32), "ok");
        unknown.slug = "synthetic-not-configured".into();
        unknown.config = None;
        let receipt = service
            .ingest_report_at(project.id, None, None, vec![unknown.clone()], now)
            .await
            .unwrap();
        assert_eq!(receipt.pending_configuration, 1);
        let unconfigured = service
            .list(project.id, 100, 0)
            .await
            .unwrap()
            .into_iter()
            .find(|m| m.slug == unknown.slug)
            .unwrap();
        assert_eq!(unconfigured.status, "unconfigured");
        assert!(unconfigured.config.is_null());
        unknown.status = "in_progress".into();
        unknown.config = Some(config.clone());
        let receipt = service
            .ingest_report_at(
                project.id,
                None,
                None,
                vec![unknown],
                now + Duration::seconds(1),
            )
            .await
            .unwrap();
        assert_eq!(receipt.pending_configuration, 0);
        let runs = service
            .runs(project.id, unconfigured.id, 100, 0)
            .await
            .unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "ok");
        let before = service.runs(project.id, id, 100, 0).await.unwrap().len();
        // Admission fails on the second monitor; the earlier run/counter change
        // in this batch must roll back along with it.
        db.connection().execute(sql("INSERT INTO cron_monitors(project_id,slug,config,status,next_expected_at,next_check_at,last_check_in_at) SELECT $1,'synthetic-cap-'||n,'null'::jsonb,'unconfigured','9999-12-31','9999-12-31',$2 FROM generate_series(1,97) n",vec![project.id.into(),now.into()])).await.unwrap();
        let mut over_limit = item(&"5".repeat(32), "ok");
        over_limit.slug = "synthetic-over-limit".into();
        assert!(matches!(
            service
                .ingest_at(
                    project.id,
                    None,
                    None,
                    vec![item(&"5".repeat(32), "ok"), over_limit],
                    now
                )
                .await,
            Err(CronMonitorError::Limit)
        ));
        assert_eq!(
            service.runs(project.id, id, 100, 0).await.unwrap().len(),
            before,
            "batch rollback must remove earlier check-ins"
        );
        db.connection()
            .execute(sql(
                "DELETE FROM cron_monitors WHERE project_id=$1 AND slug LIKE 'synthetic-cap-%'",
                vec![project.id.into()],
            ))
            .await
            .unwrap();
        // Deadline enforcement also runs during ingestion between worker ticks.
        service
            .ingest_at(
                project.id,
                None,
                None,
                vec![item(&"7".repeat(32), "in_progress")],
                now + Duration::minutes(8),
            )
            .await
            .unwrap();
        db.connection().execute(sql("INSERT INTO cron_monitor_runs(monitor_id,check_in_id,status,started_at,deadline_at) SELECT $1,lpad(to_hex(n),32,'0'),'in_progress',$2,$3 FROM generate_series(1,100) n",vec![id.into(),(now+Duration::minutes(8)).into(),(now+Duration::seconds(510)).into()])).await.unwrap();
        service
            .ingest_at(
                project.id,
                None,
                None,
                vec![item(&"7".repeat(32), "ok")],
                now + Duration::minutes(9),
            )
            .await
            .unwrap();
        assert_eq!(
            service
                .runs(project.id, id, 100, 100)
                .await
                .unwrap()
                .iter()
                .find(|run| run.check_in_id == "7".repeat(32))
                .unwrap()
                .status,
            "timeout"
        );

        let mut backlog = item(&"a".repeat(32), "ok");
        backlog.slug = "synthetic-backlog".into();
        backlog.config = None;
        service
            .ingest_at(project.id, None, None, vec![backlog.clone()], now)
            .await
            .unwrap();
        let backlog_id = service
            .list(project.id, 100, 0)
            .await
            .unwrap()
            .into_iter()
            .find(|m| m.slug == backlog.slug)
            .unwrap()
            .id;
        db.connection().execute(sql("INSERT INTO cron_monitor_runs(monitor_id,check_in_id,status,started_at) SELECT $1,lpad(to_hex(n),32,'0'),'in_progress',$2 FROM generate_series(1,101) n",vec![backlog_id.into(),now.into()])).await.unwrap();
        backlog.id = "b".repeat(32);
        backlog.status = "in_progress".into();
        service
            .ingest_at(project.id, None, None, vec![backlog.clone()], now)
            .await
            .unwrap();
        backlog.status = "ok".into();
        backlog.config = Some(config.clone());
        service
            .ingest_at(
                project.id,
                None,
                None,
                vec![backlog],
                now + Duration::minutes(2),
            )
            .await
            .unwrap();
        assert_eq!(
            service.runs(project.id, backlog_id, 1, 0).await.unwrap()[0].status,
            "timeout",
            "a completion beyond the initialization page still enforces its runtime"
        );
        // A terminal arriving before its start also stays terminal.
        service
            .ingest_at(
                project.id,
                None,
                None,
                vec![
                    item(&"6".repeat(32), "error"),
                    item(&"6".repeat(32), "in_progress"),
                ],
                now,
            )
            .await
            .unwrap();
        assert_eq!(
            service.runs(project.id, id, 100, 0).await.unwrap()[0].status,
            "error"
        );
    }
}
